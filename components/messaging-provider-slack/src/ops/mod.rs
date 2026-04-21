//! Slack provider operations.
//!
//! The egress pipeline and supporting logic is split into focused submodules
//! so each file stays under the project's 500-line cap:
//!
//! - `render`   — `render_plan` (step 1 of the 3-step egress pipeline)
//! - `encode`   — `encode_op`   (step 2)
//! - `send`     — `send_payload` + legacy `handle_send` HTTP calls
//! - `ingest`   — `ingest_http` (inbound Slack events + interactions)
//! - `modal`    — Slack modal (`views.open` + `view_submission`) support
//! - `webhook`  — `setup_webhook` helper that updates the Slack app manifest
//! - `blockkit` — Adaptive Card → Slack Block Kit conversion
//!
//! Shared low-level helpers (envelope construction) live directly in this file
//! because they are used by both ingest and modal submission paths.

mod blockkit;
mod encode;
mod ingest;
mod modal;
mod render;
mod send;
mod webhook;

pub(crate) use encode::encode_op;
pub(crate) use ingest::ingest_http;
pub(crate) use render::render_plan;
pub(crate) use send::{handle_send, send_payload};
pub(crate) use webhook::setup_webhook;

use greentic_types::{
    Actor, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};

/// Build a minimal [`ChannelMessageEnvelope`] for inbound Slack events.
///
/// Shared by `ingest_http` and `modal::handle_view_submission` because both
/// paths produce synthetic envelopes from Slack interaction payloads.
pub(super) fn build_slack_envelope(
    text: String,
    channel: Option<String>,
    sender: Option<String>,
) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    if let Some(channel_id) = &channel {
        metadata.insert("channel".to_string(), channel_id.clone());
    }
    if let Some(sender_id) = &sender {
        metadata.insert("from".to_string(), sender_id.clone());
    }
    let channel_name = channel.clone().unwrap_or_else(|| "slack".to_string());
    let actor = sender.map(|id| Actor {
        id,
        kind: Some("user".into()),
    });
    let destinations = if let Some(ch) = &channel {
        vec![Destination {
            id: ch.clone(),
            kind: None,
        }]
    } else {
        Vec::new()
    };
    ChannelMessageEnvelope {
        id: format!("slack-{channel_name}"),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: channel_name.clone(),
        session_id: channel_name,
        reply_scope: None,
        from: actor,
        to: destinations,
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
        extensions: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use greentic_types::messaging::universal_dto::HttpOutV1;
    use greentic_types::{Attachment, EnvId, TenantCtx, TenantId};
    use serde_json::{Value, json};

    fn envelope_json() -> Value {
        serde_json::to_value(ChannelMessageEnvelope {
            id: "msg-1".to_string(),
            tenant: TenantCtx::new(
                EnvId::try_from("default").expect("env"),
                TenantId::try_from("default").expect("tenant"),
            ),
            channel: "slack".to_string(),
            session_id: "session-1".to_string(),
            reply_scope: None,
            from: None,
            to: vec![Destination {
                id: "C123".to_string(),
                kind: Some("channel".to_string()),
            }],
            correlation_id: None,
            text: Some("hello".to_string()),
            attachments: Vec::new(),
            metadata: MessageMetadata::new(),
            extensions: Default::default(),
        })
        .expect("envelope")
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
    fn handle_send_rejects_invalid_json() {
        let body = parse_json(handle_send(b"{", false));
        assert_eq!(body.get("ok").and_then(Value::as_bool), Some(false));
        assert!(
            body.get("error")
                .and_then(Value::as_str)
                .expect("error")
                .starts_with("invalid json:")
        );
    }

    #[test]
    fn handle_send_accepts_attachments_before_network() {
        // Regression: slack used to hard-reject envelope.attachments. Now it
        // forwards them into the `attachments` array on chat.postMessage.
        // Clear the destination so the flow short-circuits at
        // "destination required" before touching the host `http_client::send`
        // import (unlinkable in native tests). Reaching that error proves
        // the guard accepts the envelope.
        let mut payload = envelope_json();
        {
            let obj = payload.as_object_mut().expect("object");
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

        let body = parse_json(handle_send(&with_config(payload), false));
        let err = body.get("error").and_then(Value::as_str).unwrap_or("");
        assert_ne!(err, "attachments not supported");
        assert_eq!(err, "destination required");
    }

    #[test]
    fn handle_send_requires_destination_without_default_channel() {
        let mut payload = envelope_json();
        payload
            .as_object_mut()
            .expect("object")
            .insert("to".to_string(), Value::Array(Vec::new()));

        let body = parse_json(handle_send(&with_config(payload), false));
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("destination required")
        );
    }

    #[test]
    fn ingest_http_answers_url_verification_challenge() {
        let request = json!({
            "method": "POST",
            "path": "/slack",
            "query": [],
            "headers": [],
            "body_b64": STANDARD.encode(br#"{"type":"url_verification","challenge":"abc123"}"#)
        });
        let out: HttpOutV1 = serde_json::from_slice(&ingest_http(
            &serde_json::to_vec(&request).expect("request bytes"),
        ))
        .expect("http out");

        assert_eq!(out.status, 200);
        assert_eq!(
            String::from_utf8(STANDARD.decode(out.body_b64).expect("body")).expect("utf8"),
            "abc123"
        );
    }
}
