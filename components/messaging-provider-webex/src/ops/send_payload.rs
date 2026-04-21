//! Step 3 of the egress pipeline: `send_payload`.
//!
//! Receives a pre-encoded [`ProviderPayloadV1`] (produced by [`super::encode`])
//! together with routing metadata, then performs the actual Webex
//! `POST /messages` call using the stored bot token.

use base64::{Engine, engine::general_purpose::STANDARD};
use greentic_types::messaging::universal_dto::{ProviderPayloadV1, SendPayloadInV1};
use greentic_types::{ChannelMessageEnvelope, Destination};
use provider_common::helpers::{json_bytes, send_payload_error};
use serde_json::{Value, json};

use super::{build_webex_body, format_webex_error, summarize_card_text};
use crate::bindings::greentic::http::http_client as client;
use crate::config::{detect_destination_kind, get_secret_string};
use crate::{DEFAULT_API_BASE, DEFAULT_TOKEN_KEY, PROVIDER_TYPE};

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
    let api_base = metadata
        .get("api_base_url")
        .and_then(|value| value.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{}/messages", api_base);
    let method = metadata
        .get("method")
        .and_then(|value| value.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "POST".to_string());
    let body_bytes = match STANDARD.decode(&body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return send_payload_error(&format!("payload decode failed: {err}"), false),
    };
    let envelope = match serde_json::from_slice::<ChannelMessageEnvelope>(&body_bytes) {
        Ok(env) => env,
        Err(err) => {
            eprintln!("webex send_payload invalid envelope: {err}");
            return send_payload_error(&format!("invalid envelope: {err}"), false);
        }
    };
    let text = envelope
        .text
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let card_payload = provider_common::helpers::resolve_adaptive_card(&envelope);
    let card_summary = card_payload.as_ref().and_then(summarize_card_text);
    if card_payload.is_none() && text.is_none() && envelope.attachments.is_empty() {
        eprintln!(
            "webex send_payload missing text/card/attachments envelope metadata={:?}",
            envelope.metadata
        );
        return send_payload_error("text, adaptive_card, or attachments required", false);
    }
    let destination = envelope.to.first().cloned().or_else(|| {
        metadata
            .get("default_to_person_email")
            .and_then(|value| value.as_str())
            .map(|s| Destination {
                id: s.to_string(),
                kind: Some("email".into()),
            })
    });
    let destination = match destination {
        Some(dest) => dest,
        None => {
            return send_payload_error(
                &format!("destination required (envelope to={:?})", envelope.to),
                false,
            );
        }
    };
    let dest_id = destination.id.trim();
    if dest_id.is_empty() {
        return send_payload_error("destination id required", false);
    }
    // Reply-in-thread: check for reply_to_id / parentId in envelope metadata.
    let parent_id = envelope
        .metadata
        .get("reply_to_id")
        .or_else(|| envelope.metadata.get("parentId"))
        .map(|s| s.trim_matches('"'))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let summary_text = text.clone().or(card_summary.clone());
    let markdown_value = summary_text.clone().unwrap_or_else(|| " ".to_string());
    let mut body_map = build_webex_body(card_payload.as_ref(), text.as_ref(), &markdown_value);
    if let Some(pid) = &parent_id {
        body_map.insert("parentId".into(), Value::String(pid.clone()));
    }
    // Forward envelope.attachments as Webex `files` URLs. Webex API accepts an
    // array under `files`; each entry is the attachment URL. The AC attachment
    // path stays on `attachments` (Adaptive Card contentType) and is handled by
    // build_webex_body above.
    if !envelope.attachments.is_empty() {
        let files: Vec<Value> = envelope
            .attachments
            .iter()
            .map(|a| Value::String(a.url.clone()))
            .collect();
        body_map.insert("files".into(), Value::Array(files));
    }
    let kind = destination
        .kind
        .as_deref()
        .unwrap_or_else(|| detect_destination_kind(dest_id));
    match kind {
        "room" => {
            body_map.insert("roomId".into(), Value::String(dest_id.to_string()));
        }
        "person" | "user" => {
            body_map.insert("toPersonId".into(), Value::String(dest_id.to_string()));
        }
        "email" | "" => {
            body_map.insert("toPersonEmail".into(), Value::String(dest_id.to_string()));
        }
        other => {
            return send_payload_error(&format!("unsupported destination kind: {other}"), false);
        }
    }
    let body_req = Value::Object(body_map);
    println!(
        "webex send url={}/messages body={}",
        api_base,
        serde_json::to_string(&body_req).unwrap_or_default()
    );
    let token = match get_secret_string(DEFAULT_TOKEN_KEY) {
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
        body: Some(serde_json::to_vec(&body_req).unwrap_or_else(|_| b"{}".to_vec())),
    };
    let resp = match client::send(&request, None, None) {
        Ok(value) => value,
        Err(err) => {
            return send_payload_error(&format!("transport error: {}", err.message), true);
        }
    };
    if resp.status < 200 || resp.status >= 300 {
        let body = resp.body.unwrap_or_default();
        let detail = format_webex_error(resp.status, &body);
        return send_payload_error(&detail, resp.status >= 500);
    }
    // Forward message_id so callers can use it for replies/threading.
    let resp_body = resp.body.unwrap_or_default();
    let resp_json: Value = serde_json::from_slice(&resp_body).unwrap_or(Value::Null);
    let msg_id = resp_json
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    json_bytes(&json!({
        "ok": true,
        "message": msg_id,
        "retryable": false
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use greentic_types::messaging::universal_dto::{ProviderPayloadV1, SendPayloadInV1};
    use greentic_types::{Attachment, EnvId, MessageMetadata, TenantCtx, TenantId};
    use std::collections::BTreeMap;

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

    fn parse_json(bytes: Vec<u8>) -> Value {
        serde_json::from_slice(&bytes).expect("json")
    }

    fn result_message(value: &Value) -> Option<&str> {
        value
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| value.get("message").and_then(Value::as_str))
    }

    #[test]
    fn send_payload_rejects_provider_mismatch() {
        let mismatch = SendPayloadInV1 {
            provider_type: "other".to_string(),
            tenant_id: None,
            auth_user: None,
            payload: ProviderPayloadV1 {
                content_type: "application/json".to_string(),
                body_b64: STANDARD.encode(b"{}"),
                metadata: BTreeMap::new(),
            },
        };
        let body = parse_json(send_payload(
            &serde_json::to_vec(&mismatch).expect("payload bytes"),
        ));
        assert_eq!(result_message(&body), Some("provider type mismatch"));
    }

    #[test]
    fn send_payload_requires_text_or_card_or_attachments() {
        // Envelope with no text, no AC, no attachments → 400-equivalent.
        let mut envelope = envelope();
        envelope.text = None;
        let empty = SendPayloadInV1 {
            provider_type: PROVIDER_TYPE.to_string(),
            tenant_id: None,
            auth_user: None,
            payload: ProviderPayloadV1 {
                content_type: "application/json".to_string(),
                body_b64: STANDARD.encode(serde_json::to_vec(&envelope).expect("envelope bytes")),
                metadata: BTreeMap::new(),
            },
        };
        let body = parse_json(send_payload(
            &serde_json::to_vec(&empty).expect("payload bytes"),
        ));
        assert_eq!(
            result_message(&body),
            Some("text, adaptive_card, or attachments required")
        );
    }

    #[test]
    fn send_payload_accepts_envelope_with_attachments_only() {
        // No text, no card, but attachments present → should pass the content
        // guard and proceed to the destination check. We remove the
        // destination so the flow short-circuits at "destination required"
        // before touching the host `get_secret_string` import (which isn't
        // linkable in native-target tests). Reaching that specific error is
        // proof the attachment guard accepts the envelope — which is the
        // regression we care about.
        let mut envelope = envelope();
        envelope.text = None;
        envelope.to.clear();
        envelope.attachments.push(Attachment {
            mime_type: "image/png".to_string(),
            url: "https://example.com/image.png".to_string(),
            name: Some("diagram.png".to_string()),
            size_bytes: None,
        });
        let with_att = SendPayloadInV1 {
            provider_type: PROVIDER_TYPE.to_string(),
            tenant_id: None,
            auth_user: None,
            payload: ProviderPayloadV1 {
                content_type: "application/json".to_string(),
                body_b64: STANDARD.encode(serde_json::to_vec(&envelope).expect("envelope bytes")),
                metadata: BTreeMap::new(),
            },
        };
        let body = parse_json(send_payload(
            &serde_json::to_vec(&with_att).expect("payload bytes"),
        ));
        let err = result_message(&body).unwrap_or("");
        assert!(
            err.starts_with("destination required"),
            "expected destination error, got: {err:?}"
        );
        // And crucially NOT the old hard-reject nor the content guard:
        assert_ne!(err, "attachments not supported");
        assert_ne!(err, "text, adaptive_card, or attachments required");
    }
}
