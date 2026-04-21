//! Step 2 of the egress pipeline: universal message → provider payload.
//!
//! Serializes a `ChannelMessageEnvelope`-derived `EncodeMessage` into a
//! `ProviderPayloadV1` whose body is a JSON blob understood by the webchat
//! send/persist path. Passes through adaptive card, tenant, env, and team metadata.
//!
//! Attachment handling:
//!
//! Upstream flow-components may emit attachments in two conventions:
//!
//! 1. **Greentic generic** (`mime_type`, `url`) — deserialises cleanly into
//!    `greentic_types::messaging::Attachment` and is accessible via
//!    `encode_message.attachments`.
//! 2. **DirectLine / Bot Framework** (`contentType`, `content`) — the shape
//!    native to Adaptive Cards, Hero Cards, etc. Does NOT deserialise into
//!    the typed `Attachment` struct because field names differ and `url` is
//!    required on v0.4.60 of greentic-types; strict decoding would fail the
//!    whole envelope.
//!
//! This encode_op parses the raw input as `serde_json::Value` first, extracts
//! the raw `attachments` / `rag` / `entities` / `channelData` fields, strips
//! them from the input before handing off to strict decoding, and forwards
//! the raw values to the payload body. `send_payload::build_bot_activity_raw`
//! then merges them into the DirectLine activity.
//!
//! Refs: Paul Hale (3Point) TASK-082 Bug 3 analysis note, 2026-04-17.

use base64::{Engine as _, engine::general_purpose};
use greentic_types::messaging::universal_dto::ProviderPayloadV1;
use provider_common::helpers::{decode_encode_message, encode_error, json_bytes};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

use super::helpers::{envelope_attachments_to_directline, normalize_attachments_to_directline};

/// Fields we extract from the raw input before handing off to strict decoding.
/// They're DirectLine-native surfaces that `ChannelMessageEnvelope` either
/// rejects (attachments with inline `content`) or silently drops (`rag`,
/// top-level `channelData`, `entities`).
struct RawPassthrough {
    attachments: Option<Value>,
    rag: Option<Value>,
    entities: Option<Value>,
    channel_data: Option<Value>,
    input_hint: Option<Value>,
    speak: Option<Value>,
    suggested_actions: Option<Value>,
    name: Option<Value>,
}

/// Locate the object that carries the envelope fields. The runner may pass the
/// envelope either under `{"message": {...}}` or as the envelope itself, so we
/// handle both shapes — same fallback contract as `decode_encode_message`.
fn envelope_object(raw: &Value) -> Option<&Map<String, Value>> {
    if let Some(msg) = raw.get("message").and_then(Value::as_object) {
        return Some(msg);
    }
    raw.as_object()
}

fn extract_raw_passthrough(raw: &Value) -> RawPassthrough {
    let get = |key: &str| {
        envelope_object(raw)
            .and_then(|obj| obj.get(key))
            .filter(|v| !v.is_null())
            .cloned()
    };
    RawPassthrough {
        attachments: get("attachments"),
        rag: get("rag"),
        entities: get("entities"),
        // Accept either convention on the way in — emit snake_case downstream.
        channel_data: get("channelData").or_else(|| get("channel_data")),
        input_hint: get("inputHint").or_else(|| get("input_hint")),
        speak: get("speak"),
        suggested_actions: get("suggestedActions").or_else(|| get("suggested_actions")),
        name: get("name"),
    }
}

/// Strip the non-strict fields from the input so the typed decode doesn't
/// choke on DirectLine-shaped attachments (missing `url`, unknown
/// `contentType`/`content`) or on fields the envelope struct doesn't define.
fn sanitize_for_strict_decode(raw: Value) -> Value {
    fn strip_from(obj: &mut Map<String, Value>) {
        for key in [
            "attachments",
            "rag",
            "entities",
            "channelData",
            "channel_data",
            "inputHint",
            "input_hint",
            "speak",
            "suggestedActions",
            "suggested_actions",
            "name",
        ] {
            obj.remove(key);
        }
    }
    let mut out = raw;
    if let Some(obj) = out.as_object_mut() {
        if let Some(msg) = obj.get_mut("message").and_then(|v| v.as_object_mut()) {
            strip_from(msg);
        } else {
            strip_from(obj);
        }
    }
    out
}

pub(crate) fn encode_op(input_json: &[u8]) -> Vec<u8> {
    // Parse raw first so we can extract DirectLine-shaped / non-struct fields
    // that strict decoding would reject or silently drop.
    let raw_input: Value = serde_json::from_slice(input_json).unwrap_or(Value::Null);
    let passthrough = extract_raw_passthrough(&raw_input);
    let sanitized = sanitize_for_strict_decode(raw_input);
    let sanitized_bytes = serde_json::to_vec(&sanitized).unwrap_or_else(|_| b"{}".to_vec());

    let encode_message = match decode_encode_message(&sanitized_bytes) {
        Ok(value) => value,
        Err(err) => return encode_error(&err),
    };
    let has_adaptive_card = encode_message.metadata.contains_key("adaptive_card")
        || encode_message.extensions.contains_key("adaptive_card");
    let text = encode_message
        .text
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| {
            if has_adaptive_card {
                // Don't emit fallback text when an adaptive card is present —
                // it renders natively and the text bubble is redundant.
                String::new()
            } else {
                "webchat universal payload".to_string()
            }
        });
    let metadata_route = encode_message.metadata.get("route").cloned();
    let route = metadata_route
        .clone()
        .or_else(|| Some(encode_message.session_id.clone()));
    let route_value = route.clone().unwrap_or_else(|| "webchat".to_string());
    // Pass through adaptive_card from envelope metadata (set by app flow for AC output).
    let adaptive_card = encode_message.metadata.get("adaptive_card").cloned();
    let tenant = encode_message.metadata.get("tenant").cloned();
    let env = encode_message.metadata.get("env").cloned();
    let team = encode_message.metadata.get("team").cloned();
    let mut payload_body = json!({
        "text": text,
        "route": route_value.clone(),
        "session_id": encode_message.session_id,
    });
    if let Some(ac) = &adaptive_card {
        payload_body["adaptive_card"] = Value::String(ac.clone());
    }
    if let Some(ref tenant) = tenant {
        payload_body["tenant"] = Value::String(tenant.clone());
    }
    if let Some(ref env) = env {
        payload_body["env"] = Value::String(env.clone());
    }
    if let Some(ref team) = team {
        payload_body["team"] = Value::String(team.clone());
    }
    // Build the extensions map for downstream `build_bot_activity_raw` to
    // materialise onto the DirectLine activity. Start from the typed envelope
    // extensions (if any), then layer DirectLine-native fields extracted from
    // raw input. The snake_case keys here are the ones `send_payload.rs`
    // already maps back to camelCase on the outbound activity.
    let mut extensions_map: Map<String, Value> = if encode_message.extensions.is_empty() {
        Map::new()
    } else {
        serde_json::to_value(&encode_message.extensions)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    };
    if let Some(rag) = passthrough.rag {
        // RAG context lives under channelData.rag on the DirectLine activity
        // so client-side UI (Paul's Astro three-panel) can read it without
        // parsing text sentinels.
        let mut channel_data_obj = extensions_map
            .get("channel_data")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        channel_data_obj.insert("rag".to_string(), rag);
        extensions_map.insert("channel_data".to_string(), Value::Object(channel_data_obj));
    }
    if let Some(channel_data) = passthrough.channel_data {
        // Merge top-level channelData into channel_data — keep existing keys
        // (including any `rag` merged just above) when they are set.
        let mut cd_obj = extensions_map
            .get("channel_data")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(src) = channel_data.as_object() {
            for (key, value) in src {
                cd_obj.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        if !cd_obj.is_empty() {
            extensions_map.insert("channel_data".to_string(), Value::Object(cd_obj));
        }
    }
    if let Some(entities) = passthrough.entities {
        extensions_map
            .entry("entities".to_string())
            .or_insert(entities);
    }
    if let Some(input_hint) = passthrough.input_hint {
        extensions_map
            .entry("input_hint".to_string())
            .or_insert(input_hint);
    }
    if let Some(speak) = passthrough.speak {
        extensions_map.entry("speak".to_string()).or_insert(speak);
    }
    if let Some(suggested_actions) = passthrough.suggested_actions {
        extensions_map
            .entry("suggested_actions".to_string())
            .or_insert(suggested_actions);
    }
    if let Some(name) = passthrough.name {
        extensions_map.entry("name".to_string()).or_insert(name);
    }
    if !extensions_map.is_empty() {
        payload_body["extensions"] = Value::Object(extensions_map);
    }

    // Forward attachments. Priority order:
    //   1. Raw `attachments` from the input (handles BOTH DirectLine-shape
    //      `{contentType, content}` and greentic-shape `{mime_type, url}`,
    //      plus any future rich-card shapes via pass-through).
    //   2. Typed `encode_message.attachments` (greentic shape), converted to
    //      DirectLine camelCase via `envelope_attachments_to_directline`.
    //
    // Raw wins because strict decoding may have silently dropped attachments
    // whose shape it couldn't recognise.
    let attachments_to_forward: Option<Value> = match passthrough.attachments {
        Some(Value::Array(ref arr)) if !arr.is_empty() => {
            Some(normalize_attachments_to_directline(arr))
        }
        _ if !encode_message.attachments.is_empty() => Some(envelope_attachments_to_directline(
            &encode_message.attachments,
        )),
        _ => None,
    };
    if let Some(arr) = attachments_to_forward {
        payload_body["attachments"] = arr;
    }
    let body_bytes = serde_json::to_vec(&payload_body).unwrap_or_else(|_| b"{}".to_vec());
    let mut metadata = BTreeMap::new();
    metadata.insert("route".to_string(), Value::String(route_value.clone()));
    metadata.insert("method".to_string(), Value::String("POST".to_string()));
    if let Some(tenant) = tenant {
        metadata.insert("tenant".to_string(), Value::String(tenant));
    }
    if let Some(team) = team {
        metadata.insert("team".to_string(), Value::String(team));
    }
    let payload = ProviderPayloadV1 {
        content_type: "application/json".to_string(),
        body_b64: general_purpose::STANDARD.encode(&body_bytes),
        metadata,
    };
    json_bytes(&json!({"ok": true, "payload": payload}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope_with_attachments(attachments: Value) -> Value {
        json!({
            "message": {
                "id": "msg-1",
                "tenant": {
                    "env": "demo",
                    "tenant": "demo",
                    "tenant_id": "demo",
                    "attempt": 0
                },
                "channel": "webchat",
                "session_id": "conv-1",
                "text": "see image",
                "attachments": attachments,
                "metadata": {}
            }
        })
    }

    fn decode_payload_body(response: &[u8]) -> Value {
        let resp: Value = serde_json::from_slice(response).unwrap();
        assert_eq!(resp["ok"], true, "encode_op should succeed: {resp:?}");
        let body_b64 = resp["payload"]["body_b64"].as_str().unwrap();
        let body_bytes = general_purpose::STANDARD.decode(body_b64).unwrap();
        serde_json::from_slice(&body_bytes).unwrap()
    }

    #[test]
    fn encode_op_forwards_envelope_attachments_in_directline_shape() {
        let envelope = envelope_with_attachments(json!([
            {
                "mime_type": "image/png",
                "url": "https://cdn.example.com/diagram.png",
                "name": "diagram.png"
            }
        ]));
        let input = serde_json::to_vec(&envelope).unwrap();
        let body = decode_payload_body(&encode_op(&input));

        let attachments = body["attachments"].as_array().expect("attachments array");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0]["contentType"], "image/png");
        assert_eq!(
            attachments[0]["contentUrl"],
            "https://cdn.example.com/diagram.png"
        );
        assert_eq!(attachments[0]["name"], "diagram.png");
    }

    #[test]
    fn encode_op_omits_attachments_field_when_envelope_has_none() {
        let envelope = envelope_with_attachments(json!([]));
        let input = serde_json::to_vec(&envelope).unwrap();
        let body = decode_payload_body(&encode_op(&input));
        assert!(body.get("attachments").is_none());
    }

    #[test]
    fn encode_op_passes_through_directline_shape_attachments() {
        // Regression test for TASK-082 Bug 3 Paul's case — RAG component emits
        // `{contentType, content}` for an inline Adaptive Card. Serde cannot
        // deserialise that into greentic's typed `Attachment`, so the raw
        // passthrough extracts the attachment before strict decoding and
        // forwards it verbatim.
        let envelope = envelope_with_attachments(json!([
            {
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": {
                    "type": "AdaptiveCard",
                    "version": "1.5",
                    "body": [{"type": "TextBlock", "text": "hi"}]
                }
            }
        ]));
        let input = serde_json::to_vec(&envelope).unwrap();
        let body = decode_payload_body(&encode_op(&input));

        let attachments = body["attachments"].as_array().expect("attachments array");
        assert_eq!(attachments.len(), 1);
        assert_eq!(
            attachments[0]["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        assert_eq!(attachments[0]["content"]["type"], "AdaptiveCard");
    }

    #[test]
    fn encode_op_forwards_top_level_rag_via_channel_data() {
        // Paul's component emits `rag` at envelope top-level. Route it under
        // extensions.channel_data.rag so send_payload's passthrough emits
        // activity.channelData.rag for the DirectLine client.
        let envelope = json!({
            "message": {
                "id": "msg-1",
                "tenant": {"env": "demo", "tenant": "demo", "tenant_id": "demo", "attempt": 0},
                "channel": "webchat",
                "session_id": "conv-1",
                "text": "Answer",
                "rag": {
                    "type": "tool",
                    "tool_id": "epnm",
                    "citations": [{"id": "c1"}]
                },
                "metadata": {}
            }
        });
        let input = serde_json::to_vec(&envelope).unwrap();
        let body = decode_payload_body(&encode_op(&input));

        let channel_data = body
            .pointer("/extensions/channel_data")
            .expect("channel_data present");
        assert_eq!(channel_data["rag"]["tool_id"], "epnm");
        assert_eq!(channel_data["rag"]["citations"][0]["id"], "c1");
    }

    #[test]
    fn encode_op_forwards_top_level_entities_and_name() {
        let envelope = json!({
            "message": {
                "id": "msg-1",
                "tenant": {"env": "demo", "tenant": "demo", "tenant_id": "demo", "attempt": 0},
                "channel": "webchat",
                "session_id": "conv-1",
                "text": "Answer",
                "entities": [{"type": "mention", "text": "@bot"}],
                "name": "event/custom",
                "inputHint": "acceptingInput",
                "metadata": {}
            }
        });
        let input = serde_json::to_vec(&envelope).unwrap();
        let body = decode_payload_body(&encode_op(&input));

        assert_eq!(body["extensions"]["entities"][0]["type"], "mention");
        assert_eq!(body["extensions"]["name"], "event/custom");
        assert_eq!(body["extensions"]["input_hint"], "acceptingInput");
    }

    #[test]
    fn encode_op_does_not_fail_on_malformed_directline_attachment() {
        // If serde would have rejected the envelope because `url` is missing
        // on a DirectLine-shape attachment, the sanitiser strips it first so
        // the rest of the envelope still decodes cleanly and the attachment
        // is forwarded raw.
        let envelope = json!({
            "message": {
                "id": "msg-1",
                "tenant": {"env": "demo", "tenant": "demo", "tenant_id": "demo", "attempt": 0},
                "channel": "webchat",
                "session_id": "conv-1",
                "text": "hi",
                "attachments": [{"contentType": "application/vnd.microsoft.card.hero"}],
                "metadata": {}
            }
        });
        let input = serde_json::to_vec(&envelope).unwrap();
        let body = decode_payload_body(&encode_op(&input));
        assert_eq!(
            body["attachments"][0]["contentType"],
            "application/vnd.microsoft.card.hero"
        );
    }

    #[test]
    fn encode_op_raw_attachments_win_over_typed_envelope() {
        // If both raw (DirectLine shape) and strict-decoded typed attachments
        // exist, the raw path wins — important so DirectLine clients get the
        // intended shape exactly as emitted, without greentic's remap.
        let envelope = envelope_with_attachments(json!([
            {
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": {"type": "AdaptiveCard", "version": "1.5", "body": []}
            }
        ]));
        let input = serde_json::to_vec(&envelope).unwrap();
        let body = decode_payload_body(&encode_op(&input));
        // No `contentUrl` should be present because the raw entry didn't have
        // one — if it did we'd know the greentic remap path ran by accident.
        assert!(body["attachments"][0].get("contentUrl").is_none());
        assert!(body["attachments"][0].get("content").is_some());
    }
}
