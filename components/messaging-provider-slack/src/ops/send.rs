//! Outbound HTTP paths: legacy `handle_send` + step 3 `send_payload`.
//!
//! `handle_send` is the legacy "compose and send" entry-point still used by
//! tests and flows that bypass the three-step pipeline. `send_payload` is the
//! runtime-facing step 3: fetch secret, POST to Slack, parse ok flag.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use greentic_types::messaging::universal_dto::{ProviderPayloadV1, SendPayloadInV1};
use greentic_types::{
    ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};
use provider_common::helpers::{json_bytes, send_payload_error, send_payload_success};
use serde_json::{Value, json};

use super::encode::{metadata_string, parse_blocks};
use crate::bindings::greentic::http::http_client as client;
use crate::config::{
    ProviderConfig, get_secret_string, load_config, parse_destination, resolve_bot_token,
};
use crate::{DEFAULT_API_BASE, DEFAULT_BOT_TOKEN_KEY, PROVIDER_TYPE};

pub(crate) fn handle_send(input_json: &[u8], is_reply: bool) -> Vec<u8> {
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

    let envelope: ChannelMessageEnvelope = match serde_json::from_slice(input_json) {
        Ok(env) => env,
        Err(_) => match build_synthetic_envelope(&parsed, &cfg) {
            Ok(env) => env,
            Err(message) => return json_bytes(&json!({"ok": false, "error": message})),
        },
    };

    let text = envelope
        .text
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let text = match text {
        Some(value) => value,
        None if envelope.attachments.is_empty() => {
            return json_bytes(&json!({"ok": false, "error": "text or attachments required"}));
        }
        None => String::new(),
    };

    let destination = envelope.to.first().cloned().or_else(|| {
        cfg.default_channel.clone().map(|channel| Destination {
            id: channel,
            kind: Some("channel".into()),
        })
    });
    let destination = match destination {
        Some(dest) => dest,
        None => return json_bytes(&json!({"ok": false, "error": "destination required"})),
    };

    let dest_id = destination.id.trim();
    if dest_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "destination id required"}));
    }
    let dest_id = dest_id.to_string();
    let kind = destination.kind.as_deref().unwrap_or("channel");
    if kind != "channel" && kind != "user" && !kind.is_empty() {
        return json_bytes(&json!({
            "ok": false,
            "error": format!("unsupported destination kind: {kind}")
        }));
    }

    let thread_ts = if is_reply {
        parsed
            .get("thread_id")
            .or_else(|| parsed.get("reply_to_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    let (format, blocks) = parse_blocks(&parsed);

    let token = resolve_bot_token(&cfg);
    let api_base = cfg
        .api_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{}/chat.postMessage", api_base);
    let mut payload = json!({
        "channel": dest_id,
        "text": text,
    });
    if let Some(ts) = thread_ts {
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("thread_ts".into(), Value::String(ts));
    }
    if format.as_deref() == Some("slack_blocks")
        && let Some(b) = blocks
    {
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("blocks".into(), b);
    }
    // Forward envelope attachments as Slack legacy attachments array. Images
    // render inline via `image_url`; non-image URLs go into `text` with a
    // title for unfurl.
    if !envelope.attachments.is_empty() {
        let slack_attachments: Vec<Value> = envelope
            .attachments
            .iter()
            .map(|a| {
                let mime = a.mime_type.to_lowercase();
                let title = a.name.clone().unwrap_or_else(|| "attachment".to_string());
                if mime.starts_with("image/") {
                    json!({
                        "fallback": title.clone(),
                        "image_url": a.url,
                        "title": title,
                    })
                } else {
                    json!({
                        "fallback": title.clone(),
                        "title": title,
                        "title_link": a.url,
                        "text": a.url,
                    })
                }
            })
            .collect();
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("attachments".into(), Value::Array(slack_attachments));
    }

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
            return json_bytes(
                &json!({"ok": false, "error": format!("transport error: {}", err.message)}),
            );
        }
    };

    if resp.status < 200 || resp.status >= 300 {
        return json_bytes(
            &json!({"ok": false, "error": format!("slack returned status {}", resp.status)}),
        );
    }

    let body = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    // Slack returns HTTP 200 even on errors — check the JSON body.
    if body_json.get("ok").and_then(Value::as_bool) == Some(false) {
        let err = body_json
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown slack error");
        return json_bytes(
            &json!({"ok": false, "error": format!("slack api error: {err}"), "response": body_json}),
        );
    }

    let ts = body_json
        .get("ts")
        .or_else(|| body_json.get("message").and_then(|m| m.get("ts")))
        .and_then(|v| v.as_str())
        .unwrap_or("pending-ts")
        .to_string();
    let provider_message_id = format!("slack:{ts}");

    let result = json!({
        "ok": true,
        "status": if is_reply {"replied"} else {"sent"},
        "provider_type": PROVIDER_TYPE,
        "public_base_url": cfg.public_base_url,
        "message_id": ts,
        "provider_message_id": provider_message_id,
        "response": body_json
    });
    json_bytes(&result)
}

pub(crate) fn send_payload(input_json: &[u8]) -> Vec<u8> {
    let send_in = match serde_json::from_slice::<SendPayloadInV1>(input_json) {
        Ok(value) => value,
        Err(err) => {
            return send_payload_error(&format!("invalid send_payload input: {err}"), false);
        }
    };
    if send_in.provider_type != PROVIDER_TYPE {
        return send_payload_error("provider type mismatch", false);
    }
    let ProviderPayloadV1 {
        content_type,
        body_b64,
        metadata,
    } = send_in.payload;
    let url = metadata_string(&metadata, "url")
        .unwrap_or_else(|| format!("{}/chat.postMessage", DEFAULT_API_BASE));
    let method = metadata_string(&metadata, "method").unwrap_or_else(|| "POST".to_string());
    let body_bytes = match STANDARD.decode(&body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return send_payload_error(&format!("payload decode failed: {err}"), false),
    };
    let token = match get_secret_string(DEFAULT_BOT_TOKEN_KEY) {
        Ok(value) => value,
        Err(err) => return send_payload_error(&err, false),
    };
    let request = client::Request {
        method,
        url,
        headers: vec![
            ("Content-Type".into(), content_type.clone()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: Some(body_bytes),
    };
    let resp = match client::send(&request, None, None) {
        Ok(value) => value,
        Err(err) => {
            return send_payload_error(&format!("transport error: {}", err.message), true);
        }
    };
    if resp.status < 200 || resp.status >= 300 {
        return send_payload_error(
            &format!("slack returned status {}", resp.status),
            resp.status >= 500,
        );
    }
    // Slack returns HTTP 200 even on errors — check the JSON body.
    let body = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if body_json.get("ok").and_then(Value::as_bool) == Some(false) {
        let err = body_json
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown slack error");
        return send_payload_error(&format!("slack api error: {err}"), false);
    }
    send_payload_success()
}

fn build_synthetic_envelope(
    parsed: &Value,
    cfg: &ProviderConfig,
) -> Result<ChannelMessageEnvelope, String> {
    let destination = parse_destination(parsed).or_else(|| {
        cfg.default_channel.clone().map(|channel| Destination {
            id: channel,
            kind: Some("channel".to_string()),
        })
    });
    let destination = destination.ok_or_else(|| "channel required".to_string())?;

    let env = EnvId::try_from("manual").expect("manual env id");
    let tenant = TenantId::try_from("manual").expect("manual tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("channel".to_string(), destination.id.clone());
    if let Some(kind) = &destination.kind {
        metadata.insert("destination_kind".to_string(), kind.clone());
    }

    let text = parsed
        .get("text")
        .and_then(|value| value.as_str())
        .map(|s| s.to_string());

    Ok(ChannelMessageEnvelope {
        id: "synthetic-slack-envelope".to_string(),
        tenant: TenantCtx::new(env, tenant),
        channel: destination.id.clone(),
        session_id: destination.id.clone(),
        reply_scope: None,
        from: None,
        to: vec![destination],
        correlation_id: None,
        text,
        attachments: Vec::new(),
        metadata,
        extensions: Default::default(),
    })
}
