//! Legacy `run` / `send` / `reply` operations.
//!
//! These entrypoints are kept for providers that are still wired to the
//! single-shot invocation path (rather than the three-step
//! `render_plan` → `encode` → `send_payload` pipeline). They handle the full
//! request/response lifecycle in-process: config loading, envelope
//! construction, Webex HTTP call and response normalisation.
//!
//! The `send_payload` operation (step 3 of the egress pipeline) lives in
//! [`super::send_payload`].

use greentic_types::{ChannelMessageEnvelope, Destination};
use provider_common::helpers::json_bytes;
use serde_json::{Value, json};

use super::{build_webex_body, format_webex_error, summarize_card_text};
use crate::bindings::greentic::http::http_client as client;
use crate::config::{
    build_send_envelope_from_input, detect_destination_kind, get_token, load_config,
    override_config_from_metadata,
};
use crate::{DEFAULT_API_BASE, PROVIDER_TYPE};

pub(crate) fn handle_send(input_json: &[u8]) -> Vec<u8> {
    let parsed: Value = match serde_json::from_slice(input_json) {
        Ok(val) => val,
        Err(err) => {
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}")}));
        }
    };

    let mut cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };
    if !cfg.enabled {
        return json_bytes(&json!({"ok": false, "error": "provider disabled by config"}));
    }

    let envelope: ChannelMessageEnvelope = match serde_json::from_slice(input_json) {
        Ok(env) => env,
        Err(err) => match build_send_envelope_from_input(&parsed, &cfg) {
            Ok(env) => env,
            Err(message) => {
                return json_bytes(
                    &json!({"ok": false, "error": format!("invalid envelope: {message}: {err}")}),
                );
            }
        },
    };

    override_config_from_metadata(&mut cfg, &envelope.metadata);

    println!(
        "webex encoded envelope {}",
        serde_json::to_string(&envelope).unwrap_or_default()
    );

    let text = envelope
        .text
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let text = match text {
        Some(value) => value,
        None if envelope.attachments.is_empty()
            && provider_common::helpers::resolve_adaptive_card(&envelope).is_none() =>
        {
            return json_bytes(
                &json!({"ok": false, "error": "text, adaptive_card, or attachments required"}),
            );
        }
        None => String::new(),
    };

    let destination = envelope.to.first().cloned().or_else(|| {
        cfg.default_to_person_email
            .clone()
            .map(|email| Destination {
                id: email,
                kind: Some("email".into()),
            })
    });
    println!("webex envelope to={:?}", envelope.to);
    let destination = match destination {
        Some(dest) => dest,
        None => return json_bytes(&json!({"ok": false, "error": "destination required"})),
    };

    let dest_id = destination.id.trim();
    if dest_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "destination id required"}));
    }
    let dest_id = dest_id.to_string();
    let kind = destination
        .kind
        .as_deref()
        .unwrap_or_else(|| detect_destination_kind(&dest_id));

    // Check for AC card in extensions or legacy metadata.
    let card_payload = provider_common::helpers::resolve_adaptive_card(&envelope);
    let markdown_value = card_payload
        .as_ref()
        .and_then(summarize_card_text)
        .unwrap_or_else(|| text.clone());
    // Reply-in-thread: check for reply_to_id or parentId.
    let parent_id = parsed
        .get("reply_to_id")
        .and_then(Value::as_str)
        .or_else(|| parsed.get("parentId").and_then(Value::as_str))
        .or_else(|| envelope.metadata.get("reply_to_id").map(|s| s.as_str()))
        .map(|s| s.trim_matches('"'))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let api_base = cfg
        .api_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{}/messages", api_base);
    let text_for_body = if text.is_empty() { None } else { Some(&text) };
    let mut body_map = build_webex_body(card_payload.as_ref(), text_for_body, &markdown_value);
    if let Some(pid) = &parent_id {
        body_map.insert("parentId".into(), Value::String(pid.clone()));
    }
    // Forward envelope.attachments as Webex `files` URLs (see send_payload.rs
    // for the same fix on the three-step pipeline path).
    if !envelope.attachments.is_empty() {
        let files: Vec<Value> = envelope
            .attachments
            .iter()
            .map(|a| Value::String(a.url.clone()))
            .collect();
        body_map.insert("files".into(), Value::Array(files));
    }
    let mut body = Value::Object(body_map);
    let body_obj = body.as_object_mut().expect("body object");
    match kind {
        "room" => {
            body_obj.insert("roomId".into(), Value::String(dest_id));
        }
        "person" | "user" => {
            body_obj.insert("toPersonId".into(), Value::String(dest_id));
        }
        "email" | "" => {
            body_obj.insert("toPersonEmail".into(), Value::String(dest_id));
        }
        other => {
            return json_bytes(&json!({
                "ok": false,
                "error": format!("unsupported destination kind: {other}")
            }));
        }
    }

    let token = match get_token(&cfg) {
        Ok(token) => token,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    println!(
        "webex send url={} body={}",
        url,
        serde_json::to_string(&body).unwrap_or_default()
    );
    let request = client::Request {
        method: "POST".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: Some(serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec())),
    };

    let resp = match client::send(&request, None, None) {
        Ok(resp) => resp,
        Err(err) => {
            return json_bytes(
                &json!({"ok": false, "error": format!("transport error: {}", err.message)}),
            );
        }
    };

    if resp.status < 200 || resp.status >= 300 {
        let err_body = resp.body.unwrap_or_default();
        let detail = format_webex_error(resp.status, &err_body);
        return json_bytes(&json!({"ok": false, "error": detail}));
    }

    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let msg_id = body_json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("webex-message")
        .to_string();
    let provider_message_id = format!("webex:{msg_id}");

    json_bytes(&json!({
        "ok": true,
        "status": "sent",
        "provider_type": PROVIDER_TYPE,
        "public_base_url": cfg.public_base_url,
        "message_id": msg_id,
        "provider_message_id": provider_message_id,
        "response": body_json
    }))
}

pub(crate) fn handle_reply(input_json: &[u8]) -> Vec<u8> {
    let parsed: Value = match serde_json::from_slice(input_json) {
        Ok(val) => val,
        Err(err) => {
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}")}));
        }
    };
    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };
    if !cfg.enabled {
        return json_bytes(&json!({"ok": false, "error": "provider disabled by config"}));
    }

    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "text required"}));
    }
    let thread_id = parsed
        .get("reply_to_id")
        .or_else(|| parsed.get("thread_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if thread_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "reply_to_id or thread_id required"}));
    }

    let token = match get_token(&cfg) {
        Ok(token) => token,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };
    let api_base = cfg
        .api_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{}/messages", api_base);
    let payload = json!({
        "parentId": thread_id,
        "markdown": text,
    });
    let request = client::Request {
        method: "POST".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: Some(serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())),
    };

    let resp = match client::send(&request, None, None) {
        Ok(resp) => resp,
        Err(err) => {
            return json_bytes(&json!({
                "ok": false,
                "error": format!("transport error: {}", err.message),
            }));
        }
    };
    if resp.status < 200 || resp.status >= 300 {
        return json_bytes(&json!({
            "ok": false,
            "error": format!("webex returned status {}", resp.status),
        }));
    }
    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let msg_id = body_json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("webex-reply")
        .to_string();
    let provider_message_id = format!("webex:{msg_id}");

    json_bytes(&json!({
        "ok": true,
        "status": "replied",
        "provider_type": PROVIDER_TYPE,
        "public_base_url": cfg.public_base_url,
        "message_id": msg_id,
        "provider_message_id": provider_message_id,
        "response": body_json
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use greentic_types::{Attachment, EnvId, MessageMetadata, TenantCtx, TenantId};

    fn envelope() -> ChannelMessageEnvelope {
        ChannelMessageEnvelope {
            id: "msg-1".to_string(),
            tenant: TenantCtx::new(
                EnvId::try_from("default").expect("env"),
                TenantId::try_from("default").expect("tenant"),
            ),
            channel: "webex".to_string(),
            session_id: "session-1".to_string(),
            reply_scope: None,
            from: None,
            to: vec![Destination {
                id: "person@example.com".to_string(),
                kind: Some("email".to_string()),
            }],
            correlation_id: None,
            text: Some("hello".to_string()),
            attachments: Vec::new(),
            metadata: MessageMetadata::new(),
            extensions: Default::default(),
        }
    }

    fn with_config(mut value: Value) -> Vec<u8> {
        let obj = value.as_object_mut().expect("object");
        obj.insert(
            "public_base_url".to_string(),
            Value::String("https://example.com".to_string()),
        );
        obj.insert("bot_token".to_string(), Value::String("token".to_string()));
        serde_json::to_vec(&value).expect("bytes")
    }

    fn parse_json(bytes: Vec<u8>) -> Value {
        serde_json::from_slice(&bytes).expect("json")
    }

    #[test]
    fn handle_send_accepts_attachments_and_rejects_missing_destination() {
        // Attachments must NOT cause a hard-reject any more — they flow
        // through to the Webex `files` array. Clear the destination so the
        // flow short-circuits at "destination required" before reaching the
        // host `http_client::send` import (unlinkable in native tests).
        // Hitting that specific error proves the attachment guard now
        // accepts the envelope — the regression under test.
        let mut with_attachment = serde_json::to_value(envelope()).expect("value");
        {
            let obj = with_attachment.as_object_mut().expect("object");
            obj.insert(
                "attachments".to_string(),
                serde_json::to_value(vec![Attachment {
                    mime_type: "image/png".to_string(),
                    url: "https://example.com/image.png".to_string(),
                    name: None,
                    size_bytes: None,
                }])
                .expect("attachments"),
            );
            obj.insert("to".to_string(), Value::Array(Vec::new()));
        }
        let body = parse_json(handle_send(&with_config(with_attachment)));
        let err = body.get("error").and_then(Value::as_str).unwrap_or("");
        assert_ne!(err, "attachments not supported");
        assert_eq!(err, "destination required");

        let mut no_destination = serde_json::to_value(envelope()).expect("value");
        no_destination
            .as_object_mut()
            .expect("object")
            .insert("to".to_string(), Value::Array(Vec::new()));
        let body = parse_json(handle_send(&with_config(no_destination)));
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("destination required")
        );
    }

    #[test]
    fn handle_reply_requires_text_and_thread_id() {
        let body = parse_json(handle_reply(&with_config(json!({
            "reply_to_id": "thread-1"
        }))));
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("text required")
        );

        let body = parse_json(handle_reply(&with_config(json!({
            "text": "hello"
        }))));
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("reply_to_id or thread_id required")
        );
    }
}
