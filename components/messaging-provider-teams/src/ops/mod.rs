//! Teams provider operations.
//!
//! Uses the Bot Connector API (not Microsoft Graph). The 3-step egress
//! pipeline and inbound webhook handling are split into focused submodules so
//! each file stays under the project's 500-line cap:
//!
//! - `render`  — `render_plan` (step 1; Tier A via `capabilities_for("teams")`)
//! - `encode`  — `encode_op` (step 2)
//! - `send`    — `send_payload`, `handle_send`, `handle_reply` (step 3 + legacy)
//! - `ingest`  — `ingest_http` + Bot Framework activity/card-action handling
//!
//! `build_team_envelope` is shared between `send` and `ingest` because both
//! produce synthetic envelopes from inbound Bot Framework payloads.

mod encode;
mod ingest;
mod render;
mod send;

pub(crate) use encode::encode_op;
pub(crate) use ingest::ingest_http;
pub(crate) use render::render_plan;
pub(crate) use send::{handle_reply, handle_send, send_payload};

// Re-export extraction helpers for the in-module test suite. The pattern
// mirrors `messaging-provider-slack::ops` so unit tests can keep using
// `use super::*` without having to know which submodule hosts each helper.
#[cfg(test)]
pub(crate) use ingest::{extract_bot_text, extract_channel_id, extract_sender, extract_team_id};

use greentic_types::{
    Actor, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};

/// Build a minimal [`ChannelMessageEnvelope`] for inbound Teams payloads.
///
/// Shared by `send::build_team_envelope_from_input` (outbound fallback when
/// the caller ships a loose JSON body) and `ingest::ingest_http` /
/// `ingest::handle_card_action` (inbound Bot Framework activities and card
/// actions).
pub(super) fn build_team_envelope(
    text: String,
    user_id: Option<String>,
    team_id: Option<String>,
    channel_id: Option<String>,
) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    if let Some(team) = &team_id {
        metadata.insert("team_id".to_string(), team.clone());
    }
    if let Some(channel) = &channel_id {
        metadata.insert("channel_id".to_string(), channel.clone());
    }
    if let Some(sender) = &user_id {
        metadata.insert("from".to_string(), sender.clone());
    }
    let channel_name = channel_id
        .clone()
        .or_else(|| team_id.clone())
        .unwrap_or_else(|| "teams".to_string());
    let sender = user_id.map(|id| Actor {
        id,
        kind: Some("user".into()),
    });
    ChannelMessageEnvelope {
        id: format!("teams-{channel_name}"),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: channel_name.clone(),
        session_id: channel_name,
        reply_scope: None,
        from: sender,
        to: {
            let mut dests = Vec::new();
            if let (Some(team), Some(channel)) = (&team_id, &channel_id) {
                dests.push(Destination {
                    id: format!("{}:{}", team, channel),
                    kind: Some("channel".to_string()),
                });
            }
            dests
        },
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
    use greentic_types::{EnvId, TenantCtx, TenantId};
    use serde_json::{Value, json};

    fn envelope_json() -> Value {
        serde_json::to_value(ChannelMessageEnvelope {
            id: "msg-1".to_string(),
            tenant: TenantCtx::new(
                EnvId::try_from("default").expect("env"),
                TenantId::try_from("default").expect("tenant"),
            ),
            channel: "teams".to_string(),
            session_id: "session-1".to_string(),
            reply_scope: None,
            from: None,
            to: vec![Destination {
                id: "conv-1".to_string(),
                kind: Some("conversation".to_string()),
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
        obj.insert(
            "ms_bot_app_id".to_string(),
            Value::String("app-id".to_string()),
        );
        serde_json::to_vec(&value).expect("bytes")
    }

    fn parse_json(bytes: Vec<u8>) -> Value {
        serde_json::from_slice(&bytes).expect("json")
    }

    #[test]
    fn handle_send_requires_service_url_before_network() {
        let body = parse_json(handle_send(&with_config(envelope_json())));
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some(
                "service_url required (from metadata.serviceUrl, config.default_service_url, or Activity)"
            )
        );
    }

    #[test]
    fn handle_reply_checks_required_fields() {
        let body = parse_json(handle_reply(&with_config(json!({
            "text": "hello",
            "conversation_id": "conv-1"
        }))));
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("reply_to_id or thread_id required")
        );

        let body = parse_json(handle_reply(&with_config(json!({
            "text": "hello",
            "reply_to_id": "activity-1",
            "conversation_id": "conv-1"
        }))));
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("service_url required")
        );
    }

    #[test]
    fn extraction_helpers_read_bot_framework_shapes() {
        let value = json!({
            "text": "hello",
            "channelData": {
                "team": { "id": "team-1" },
                "channel": { "id": "channel-1" }
            },
            "from": { "id": "user-1" }
        });

        assert_eq!(extract_bot_text(&value), "hello");
        assert_eq!(extract_team_id(&value).as_deref(), Some("team-1"));
        assert_eq!(extract_channel_id(&value).as_deref(), Some("channel-1"));
        assert_eq!(extract_sender(&value).as_deref(), Some("user-1"));
    }
}
