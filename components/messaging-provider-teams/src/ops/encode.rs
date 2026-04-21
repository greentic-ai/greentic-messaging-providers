//! Step 2 of the 3-step egress pipeline: envelope encoding.
//!
//! Converts a `ChannelMessageEnvelope` into a `ProviderPayloadV1` ready for
//! `send_payload` to POST to the Bot Connector API.

use base64::{Engine, engine::general_purpose::STANDARD};
use greentic_types::messaging::universal_dto::ProviderPayloadV1;
use provider_common::helpers::{decode_encode_message, encode_error, json_bytes};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn encode_op(input_json: &[u8]) -> Vec<u8> {
    let encode_message = match decode_encode_message(input_json) {
        Ok(value) => value,
        Err(err) => return encode_error(&err),
    };

    // Extract AC card from extensions (typed Value) or metadata (legacy string)
    let ac_json = provider_common::helpers::resolve_adaptive_card(&encode_message)
        .map(|v| serde_json::to_string(&v).unwrap_or_default());

    // Serialize the full envelope so send_payload -> handle_send can parse it
    let mut envelope_val =
        serde_json::to_value(&encode_message).unwrap_or(Value::Object(Default::default()));

    if let Some(ac) = &ac_json {
        envelope_val
            .as_object_mut()
            .unwrap()
            .insert("_ac_json".to_string(), Value::String(ac.clone()));
    }

    // Forward reply_to_id from metadata
    if let Some(reply_id) = encode_message.metadata.get("reply_to_id") {
        let clean = reply_id.trim_matches('"');
        if !clean.is_empty() {
            envelope_val
                .as_object_mut()
                .unwrap()
                .insert("reply_to_id".to_string(), Value::String(clean.to_string()));
        }
    }

    // Forward serviceUrl and conversationId from metadata
    if let Some(service_url) = encode_message.metadata.get("serviceUrl") {
        envelope_val
            .as_object_mut()
            .unwrap()
            .insert("metadata".to_string(), json!({
                "serviceUrl": service_url,
                "conversationId": encode_message.metadata.get("conversationId").cloned().unwrap_or_default()
            }));
    }

    let body_bytes = serde_json::to_vec(&envelope_val).unwrap_or_else(|_| b"{}".to_vec());
    let mut metadata = BTreeMap::new();
    metadata.insert("method".to_string(), Value::String("POST".to_string()));

    let payload = ProviderPayloadV1 {
        content_type: "application/json".to_string(),
        body_b64: STANDARD.encode(&body_bytes),
        metadata,
    };
    json_bytes(&json!({"ok": true, "payload": payload}))
}
