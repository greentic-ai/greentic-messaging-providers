//! Step 3 of the egress pipeline: persist the encoded payload + emit a bot
//! activity back into the Direct Line conversation so frontend polling can
//! pick it up.
//!
//! For webchat, "send" is a local operation — there is no outbound HTTP call to
//! a third-party API. We write to the host state store and, when a
//! `session_id` is present on the payload, append a bot activity to the
//! stored conversation state.

use base64::{Engine as _, engine::general_purpose};
use greentic_types::messaging::universal_dto::SendPayloadInV1;
use provider_common::helpers::{json_bytes, send_payload_error, send_payload_success};
use serde_json::{Value, json};

use crate::directline::HostStateStore;
use crate::directline::jwt::DirectLineContext;
use crate::directline::state::{StoredActivity, conversation_key};
use crate::directline::store::StateStore as _;

use super::helpers::{
    extract_text, public_base_url_from_value, route_from_value, tenant_channel_from_value,
    value_as_trimmed_string,
};

pub(crate) fn send_payload(input_json: &[u8]) -> Vec<u8> {
    let send_in = match serde_json::from_slice::<SendPayloadInV1>(input_json) {
        Ok(value) => value,
        Err(err) => {
            return send_payload_error(&format!("invalid send_payload input: {err}"), false);
        }
    };
    if !send_in.provider_type.starts_with("messaging.webchat") {
        return send_payload_error("provider type mismatch", false);
    }
    let payload_bytes = match general_purpose::STANDARD.decode(&send_in.payload.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => {
            return send_payload_error(&format!("payload decode failed: {err}"), false);
        }
    };
    let payload: Value = serde_json::from_slice(&payload_bytes).unwrap_or(Value::Null);
    match persist_send_payload(&payload) {
        Ok(_) => send_payload_success(),
        Err(err) => send_payload_error(&err, false),
    }
}

fn persist_send_payload(payload: &Value) -> Result<(), String> {
    let route = route_from_value(payload);
    let tenant_channel_id = tenant_channel_from_value(payload);
    let key = route
        .clone()
        .or(tenant_channel_id.clone())
        .ok_or_else(|| "route or tenant_channel_id required".to_string())?;
    let text = extract_text(payload);
    let adaptive_card_json = payload
        .get("adaptive_card")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let extensions = payload.get("extensions").cloned();
    let envelope_attachments = payload.get("attachments").filter(|v| v.is_array()).cloned();
    if text.is_empty()
        && adaptive_card_json.is_none()
        && extensions.is_none()
        && envelope_attachments.is_none()
    {
        return Err("text, adaptive_card, attachments, or extensions required".into());
    }

    // If session_id is present, try to append the bot response as a Direct Line
    // activity so that GET /activities polling returns it to the frontend.
    if let Some(session_id) = value_as_trimmed_string(payload.get("session_id")) {
        let env = value_as_trimmed_string(payload.get("env"));
        let tenant = value_as_trimmed_string(payload.get("tenant"));
        let team = value_as_trimmed_string(payload.get("team"));
        let _ = append_bot_activity_to_conversation(
            &session_id,
            &text,
            adaptive_card_json.as_deref(),
            extensions.as_ref(),
            envelope_attachments.as_ref(),
            env.as_deref(),
            tenant.as_deref(),
            team.as_deref(),
        );
    }

    let public_base_url = public_base_url_from_value(payload);
    let stored = json!({
        "route": route,
        "tenant_channel_id": tenant_channel_id,
        "public_base_url": public_base_url,
        "mode": value_as_trimmed_string(payload.get("mode")).unwrap_or_else(|| "local_queue".to_string()),
        "base_url": value_as_trimmed_string(payload.get("base_url")),
        "text": text,
    });
    let mut state_store = HostStateStore;
    state_store.write(&key, &json_bytes(&stored))?;
    Ok(())
}

/// Append a bot-originated activity to the Direct Line conversation state.
/// Uses provided env/tenant context from envelope metadata, falling back to "default".
/// Best-effort: silently ignores errors (conversation may not exist).
#[allow(clippy::too_many_arguments)]
fn append_bot_activity_to_conversation(
    conversation_id: &str,
    text: &str,
    adaptive_card_json: Option<&str>,
    extensions: Option<&Value>,
    envelope_attachments: Option<&Value>,
    env: Option<&str>,
    tenant: Option<&str>,
    team: Option<&str>,
) -> Result<(), String> {
    let ctx = DirectLineContext {
        env: env.unwrap_or("default").to_string(),
        tenant: tenant.unwrap_or("default").to_string(),
        team: team.map(str::to_string),
    };
    let conv_key = conversation_key(&ctx, conversation_id);
    let mut store = HostStateStore;

    let conv_bytes = match store.read(&conv_key) {
        Ok(Some(bytes)) => bytes,
        _ => return Ok(()),
    };

    let mut conversation: crate::directline::state::ConversationState =
        serde_json::from_slice(&conv_bytes).map_err(|e| e.to_string())?;

    let watermark = conversation.bump_watermark();
    let raw = build_bot_activity_raw(text, adaptive_card_json, extensions, envelope_attachments);

    let activity = StoredActivity {
        id: format!("bot-{watermark}"),
        type_: "message".to_string(),
        text: if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        },
        from: Some("bot".to_string()),
        timestamp: chrono::Utc::now().timestamp_millis(),
        watermark,
        raw,
    };
    conversation.activities.push(activity);

    let updated = serde_json::to_vec(&conversation).map_err(|e| e.to_string())?;
    store.write(&conv_key, &updated)?;
    Ok(())
}

/// Build the raw DirectLine activity JSON for a bot message, merging any
/// `extensions` fields (attachments, channelData, entities, etc.) and the
/// envelope's top-level `attachments` field back into their DirectLine-native
/// camelCase form.
///
/// Merge order for attachments: legacy AC (metadata) → envelope.attachments
/// (top-level typed field) → extensions.adaptive_card → extensions.attachments.
///
/// Pure function — no state I/O — suitable for unit testing the merge logic.
fn build_bot_activity_raw(
    text: &str,
    adaptive_card_json: Option<&str>,
    extensions: Option<&Value>,
    envelope_attachments: Option<&Value>,
) -> Value {
    let mut raw = json!({
        "type": "message",
        "from": {"id": "bot", "name": "Bot", "role": "bot"},
    });
    if !text.is_empty() {
        raw["text"] = Value::String(text.to_string());
    }

    let mut attachments: Vec<Value> = Vec::new();
    if let Some(ac_json) = adaptive_card_json
        && let Ok(ac_value) = serde_json::from_str::<Value>(ac_json)
    {
        attachments.push(json!({
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": ac_value,
        }));
    }

    if let Some(Value::Array(env_atts)) = envelope_attachments {
        attachments.extend(env_atts.clone());
    }

    if let Some(ext) = extensions
        && let Some(ext_obj) = ext.as_object()
    {
        if adaptive_card_json.is_none()
            && let Some(ac) = ext_obj.get("adaptive_card")
        {
            attachments.push(json!({
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": ac.clone(),
            }));
        }
        if let Some(Value::Array(ext_atts)) = ext_obj.get("attachments") {
            attachments.extend(ext_atts.clone());
        }
        let passthroughs: &[(&str, &str)] = &[
            ("channel_data", "channelData"),
            ("entities", "entities"),
            ("name", "name"),
            ("input_hint", "inputHint"),
            ("speak", "speak"),
            ("suggested_actions", "suggestedActions"),
        ];
        for (src, dst) in passthroughs {
            if let Some(v) = ext_obj.get(*src)
                && !v.is_null()
            {
                raw[*dst] = v.clone();
            }
        }
    }

    if !attachments.is_empty() {
        raw["attachments"] = Value::Array(attachments);
    }
    raw
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_bot_activity_raw_plain_text_only() {
        let raw = build_bot_activity_raw("hello", None, None, None);
        assert_eq!(raw["type"], "message");
        assert_eq!(raw["text"], "hello");
        assert_eq!(raw["from"]["id"], "bot");
        assert!(raw.get("attachments").is_none());
    }

    #[test]
    fn build_bot_activity_raw_adaptive_card_via_legacy_metadata() {
        // Upstream still writes metadata["adaptive_card"] as JSON string.
        let ac_json = r#"{"type":"AdaptiveCard","body":[{"type":"TextBlock","text":"hi"}]}"#;
        let raw = build_bot_activity_raw("hi", Some(ac_json), None, None);

        let attachments = raw["attachments"].as_array().expect("attachments array");
        assert_eq!(attachments.len(), 1);
        assert_eq!(
            attachments[0]["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        assert_eq!(attachments[0]["content"]["type"], "AdaptiveCard");
    }

    #[test]
    fn build_bot_activity_raw_adaptive_card_via_extensions() {
        // New path: RAG component writes extensions["adaptive_card"] as typed Value.
        let extensions = json!({
            "adaptive_card": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "hi"}]},
        });
        let raw = build_bot_activity_raw("hi", None, Some(&extensions), None);

        let attachments = raw["attachments"].as_array().expect("attachments array");
        assert_eq!(attachments.len(), 1);
        assert_eq!(
            attachments[0]["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        assert_eq!(attachments[0]["content"]["type"], "AdaptiveCard");
    }

    #[test]
    fn build_bot_activity_raw_merges_ac_from_metadata_and_native_attachments_from_extensions() {
        // Legacy AC in metadata + provider-native attachments in extensions.
        let ac_json = r#"{"type":"AdaptiveCard","body":[]}"#;
        let extensions = json!({
            "attachments": [
                {"contentType": "application/vnd.microsoft.card.hero", "content": {"title": "Hero"}}
            ]
        });
        let raw = build_bot_activity_raw("", Some(ac_json), Some(&extensions), None);

        let attachments = raw["attachments"].as_array().expect("attachments array");
        assert_eq!(attachments.len(), 2, "AC + hero card expected");
        assert_eq!(
            attachments[0]["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        assert_eq!(
            attachments[1]["contentType"],
            "application/vnd.microsoft.card.hero"
        );
    }

    #[test]
    fn build_bot_activity_raw_materializes_all_directline_fields() {
        // Full scenario: RAG component emits AC + citations (via channelData.rag) +
        // all DirectLine Bot Framework fields, expecting them preserved verbatim.
        let extensions = json!({
            "adaptive_card": {"type": "AdaptiveCard", "body": []},
            "channel_data": {"rag": {"citations": [{"id": "c1"}]}, "feature": "x"},
            "entities": [{"type": "mention", "text": "@bot"}],
            "input_hint": "acceptingInput",
            "speak": "hello there",
            "suggested_actions": {"actions": [{"type": "imBack", "title": "Yes", "value": "yes"}]},
            "name": "event/custom",
        });

        let raw = build_bot_activity_raw("Based on your docs...", None, Some(&extensions), None);

        // Text preserved.
        assert_eq!(raw["text"], "Based on your docs...");
        // AC wrapped as attachment.
        let atts = raw["attachments"].as_array().unwrap();
        assert_eq!(
            atts[0]["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        // Snake_case extensions keys → DirectLine camelCase.
        assert_eq!(
            raw["channelData"],
            json!({"rag": {"citations": [{"id": "c1"}]}, "feature": "x"})
        );
        assert_eq!(raw["entities"][0]["type"], "mention");
        assert_eq!(raw["inputHint"], "acceptingInput");
        assert_eq!(raw["speak"], "hello there");
        assert_eq!(raw["suggestedActions"]["actions"][0]["value"], "yes");
        assert_eq!(raw["name"], "event/custom");
    }

    #[test]
    fn build_bot_activity_raw_rag_citations_round_trip() {
        // Regression test for TASK-082 Bug 3 — RAG component emits citations
        // via extensions["channel_data"]["rag"], they must survive into the
        // DirectLine activity's channelData field exactly as emitted.
        let extensions = json!({
            "adaptive_card": {"type": "AdaptiveCard", "body": []},
            "channel_data": {
                "rag": {
                    "citations": [
                        {"id": "c1", "source": "docs/x.md", "snippet": "..."},
                        {"id": "c2", "source": "docs/y.md", "snippet": "..."}
                    ]
                }
            }
        });

        let raw = build_bot_activity_raw("answer", None, Some(&extensions), None);

        let citations = raw
            .pointer("/channelData/rag/citations")
            .and_then(|v| v.as_array())
            .expect("citations preserved under channelData.rag");
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0]["id"], "c1");
        assert_eq!(citations[0]["source"], "docs/x.md");
        assert_eq!(citations[1]["id"], "c2");
    }

    #[test]
    fn build_bot_activity_raw_skips_null_extension_fields() {
        let extensions = json!({
            "channel_data": null,
            "entities": null,
            "speak": "keep me",
        });
        let raw = build_bot_activity_raw("x", None, Some(&extensions), None);
        assert!(raw.get("channelData").is_none());
        assert!(raw.get("entities").is_none());
        assert_eq!(raw["speak"], "keep me");
    }

    #[test]
    fn build_bot_activity_raw_forwards_envelope_attachments() {
        // Regression test for TASK-082 Bug 3 follow-up — envelope's top-level
        // `attachments: Vec<Attachment>` field must reach the DirectLine activity.
        let envelope_attachments = json!([
            {
                "contentType": "image/png",
                "contentUrl": "https://cdn.example.com/diagram.png",
                "name": "diagram.png",
            }
        ]);
        let raw = build_bot_activity_raw("see image", None, None, Some(&envelope_attachments));

        let attachments = raw["attachments"].as_array().expect("attachments array");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0]["contentType"], "image/png");
        assert_eq!(
            attachments[0]["contentUrl"],
            "https://cdn.example.com/diagram.png"
        );
    }

    #[test]
    fn build_bot_activity_raw_merges_envelope_attachments_with_legacy_ac_and_extensions() {
        // All three sources present — expect legacy AC first, envelope attachments
        // second, extensions.attachments last. Deterministic order matters for
        // clients that rely on card-ordering semantics.
        let ac_json = r#"{"type":"AdaptiveCard","body":[]}"#;
        let envelope_attachments = json!([
            {"contentType": "image/png", "contentUrl": "https://e/1.png"}
        ]);
        let extensions = json!({
            "attachments": [
                {"contentType": "application/vnd.microsoft.card.hero", "content": {"title": "H"}}
            ]
        });

        let raw = build_bot_activity_raw(
            "multi",
            Some(ac_json),
            Some(&extensions),
            Some(&envelope_attachments),
        );

        let attachments = raw["attachments"].as_array().expect("attachments array");
        assert_eq!(attachments.len(), 3);
        assert_eq!(
            attachments[0]["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        assert_eq!(attachments[1]["contentType"], "image/png");
        assert_eq!(
            attachments[2]["contentType"],
            "application/vnd.microsoft.card.hero"
        );
    }

    #[test]
    fn build_bot_activity_raw_ignores_non_array_envelope_attachments() {
        // Defensive: if upstream emits a malformed non-array value, drop silently.
        let envelope_attachments = json!({"this": "is not an array"});
        let raw = build_bot_activity_raw("x", None, None, Some(&envelope_attachments));
        assert!(raw.get("attachments").is_none());
    }

    #[test]
    fn append_context_uses_team_from_payload() {
        assert_eq!(
            value_as_trimmed_string(json!({"team": "default"}).get("team")).as_deref(),
            Some("default")
        );
        assert_eq!(
            value_as_trimmed_string(json!({"team": "  "}).get("team")),
            None
        );
    }
}
