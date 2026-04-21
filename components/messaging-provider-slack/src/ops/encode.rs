//! Step 2 of the egress pipeline: `encode_op`.
//!
//! Converts a [`provider_common`] encode message into a Slack-flavoured
//! [`ProviderPayloadV1`]. If the message carries an Adaptive Card, the card
//! is converted to Slack Block Kit via [`super::blockkit`].

use base64::{Engine as _, engine::general_purpose::STANDARD};
use greentic_types::messaging::universal_dto::ProviderPayloadV1;
use provider_common::helpers::{
    decode_encode_message, encode_error, extract_ac_summary, json_bytes,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::blockkit::ac_to_slack_blocks;
use crate::DEFAULT_API_BASE;

pub(crate) fn encode_op(input_json: &[u8]) -> Vec<u8> {
    let encode_message = match decode_encode_message(input_json) {
        Ok(value) => value,
        Err(err) => return encode_error(&err),
    };
    let channel = encode_message
        .to
        .first()
        .map(|d| d.id.clone())
        .unwrap_or_default();
    if channel.is_empty() {
        return encode_error("destination (to) required");
    }

    // If the message carries an Adaptive Card, convert to Slack Block Kit.
    let ac_raw_str = provider_common::helpers::resolve_adaptive_card(&encode_message)
        .map(|v| serde_json::to_string(&v).unwrap_or_default());
    let ac_result = ac_raw_str.as_deref().and_then(ac_to_slack_blocks);

    let text = if ac_result.is_some() {
        // Blocks present — text is the plain-text fallback for notifications.
        // Capability matrix is centralized in greentic-messaging-renderer.
        let caps = greentic_messaging_renderer::capabilities_for("slack")
            .expect("slack capabilities must be registered");
        ac_raw_str
            .as_deref()
            .and_then(|ac_raw| extract_ac_summary(ac_raw, &caps))
            .or_else(|| encode_message.text.clone().filter(|t| !t.trim().is_empty()))
            .unwrap_or_else(|| "slack universal payload".to_string())
    } else {
        encode_message
            .text
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "slack universal payload".to_string())
    };

    let url = format!("{}/chat.postMessage", DEFAULT_API_BASE);
    let mut body = json!({
        "channel": channel,
        "text": text,
    });
    if let Some(ref result) = ac_result {
        body.as_object_mut()
            .unwrap()
            .insert("blocks".into(), Value::Array(result.blocks.clone()));
        // Store modal input specs in Slack message metadata (not in button value)
        // so they can be retrieved when a modal-trigger button is clicked.
        if !result.modal_inputs.is_empty() {
            body.as_object_mut().unwrap().insert(
                "metadata".into(),
                json!({
                    "event_type": "ac_modal_inputs",
                    "event_payload": {
                        "inputs": result.modal_inputs
                    }
                }),
            );
        }
    }
    let body_bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let mut metadata = BTreeMap::new();
    metadata.insert("url".to_string(), Value::String(url));
    metadata.insert("method".to_string(), Value::String("POST".to_string()));
    metadata.insert("channel".to_string(), Value::String(channel));
    let payload = ProviderPayloadV1 {
        content_type: "application/json".to_string(),
        body_b64: STANDARD.encode(&body_bytes),
        metadata,
    };
    json_bytes(&json!({"ok": true, "payload": payload}))
}

/// Extract a top-level `rich.format` + `rich.blocks` pair from a legacy
/// send payload. Used by `handle_send` to pass through pre-built Slack
/// block kit JSON from flow steps that produce native Slack output.
pub(crate) fn parse_blocks(parsed: &Value) -> (Option<String>, Option<Value>) {
    let format = parsed
        .get("rich")
        .and_then(|v| v.get("format"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let blocks = parsed.get("rich").and_then(|v| v.get("blocks")).cloned();
    (format, blocks)
}

/// Look up a metadata value as a `String`, ignoring non-string entries.
pub(crate) fn metadata_string(metadata: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| value.as_str().map(|s| s.to_string()))
}
