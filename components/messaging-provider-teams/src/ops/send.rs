//! Step 3 of the 3-step egress pipeline: outbound HTTP to Bot Connector.
//! Also hosts the legacy `handle_send` / `handle_reply` JSON passthrough paths
//! invoked by `dispatch_json_invoke("run" | "send" | "reply")`.

use base64::{Engine, engine::general_purpose::STANDARD};
use greentic_types::messaging::universal_dto::SendPayloadInV1;
use greentic_types::{ChannelMessageEnvelope, Destination};
use provider_common::helpers::{json_bytes, send_payload_error};
use serde_json::{Value, json};

use crate::PROVIDER_TYPE;
use crate::auth::acquire_bot_token;
use crate::bindings::greentic::http::http_client as client;
use crate::config::{ProviderConfig, default_channel_destination, get_service_url, load_config};

use super::build_team_envelope;

/// Handles the "send" operation - sends a message via Bot Connector API.
///
/// Endpoint: `{serviceUrl}/v3/conversations/{conversationId}/activities`
pub(crate) fn handle_send(input_json: &[u8]) -> Vec<u8> {
    let parsed: Value = match serde_json::from_slice(input_json) {
        Ok(val) => val,
        Err(err) => {
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}") }));
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
        Err(err) => match build_team_envelope_from_input(&parsed, &cfg) {
            Ok(env) => env,
            Err(message) => {
                return json_bytes(
                    &json!({"ok": false, "error": format!("invalid envelope: {message}: {err}") }),
                );
            }
        },
    };

    // Adaptive Card payloads may have no text — allow empty text when _ac_json is present.
    // Envelope attachments alone are also valid (media-only message).
    let has_ac = parsed.get("_ac_json").is_some();
    let has_attachments = !envelope.attachments.is_empty();
    let text = envelope
        .text
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    if text.is_empty() && !has_ac && !has_attachments {
        return json_bytes(
            &json!({"ok": false, "error": "text, adaptive_card, or attachments required"}),
        );
    }

    // Get service URL from metadata or config
    let service_url = parsed
        .get("metadata")
        .and_then(|m| m.get("serviceUrl"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .or_else(|| get_service_url(&parsed, &cfg))
        .or_else(|| cfg.default_service_url.clone());

    let service_url = match service_url {
        Some(url) if !url.is_empty() => url.trim_end_matches('/').to_string(),
        _ => {
            return json_bytes(&json!({
                "ok": false,
                "error": "service_url required (from metadata.serviceUrl, config.default_service_url, or Activity)"
            }));
        }
    };

    // Get conversation ID from metadata or destination
    let conversation_id = parsed
        .get("metadata")
        .and_then(|m| m.get("conversationId"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .or_else(|| {
            envelope
                .to
                .first()
                .map(|d| d.id.clone())
                .or_else(|| default_channel_destination(&cfg).map(|d| d.id))
        });

    let conversation_id = match conversation_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            return json_bytes(&json!({
                "ok": false,
                "error": "conversation_id required (from metadata.conversationId or destination)"
            }));
        }
    };

    // Reply-in-thread: check for reply_to_id in envelope or metadata
    let reply_to_id = parsed
        .get("reply_to_id")
        .and_then(Value::as_str)
        .or_else(|| parsed.get("reply_scope").and_then(Value::as_str))
        .or_else(|| {
            parsed
                .get("metadata")
                .and_then(|m| m.get("reply_to_id"))
                .and_then(Value::as_str)
        })
        .map(|s| s.trim_matches('"'))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Acquire bot token
    let token = match acquire_bot_token(&cfg) {
        Ok(tok) => tok,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    // Build Bot Framework Activity payload.
    //
    // Two attachment sources can contribute to the outbound activity's
    // `attachments` array: the Adaptive Card (if any) and envelope
    // attachments (media URLs). They coexist — Teams renders cards alongside
    // file previews.
    let ac_json_str = parsed
        .get("_ac_json")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok());

    let mut bot_attachments: Vec<Value> = Vec::new();
    if let Some(ac_card) = ac_json_str {
        bot_attachments.push(json!({
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": ac_card
        }));
    }
    for a in &envelope.attachments {
        let mut att = serde_json::Map::new();
        att.insert(
            "contentType".to_string(),
            Value::String(a.mime_type.clone()),
        );
        att.insert("contentUrl".to_string(), Value::String(a.url.clone()));
        if let Some(name) = &a.name {
            att.insert("name".to_string(), Value::String(name.clone()));
        }
        bot_attachments.push(Value::Object(att));
    }

    let body = if bot_attachments.is_empty() {
        json!({"type": "message", "text": text})
    } else {
        // Bot Framework requires `text` alongside attachments — use empty
        // fallback when the sender provided only media/card.
        json!({
            "type": "message",
            "text": text,
            "attachments": bot_attachments
        })
    };

    // Build URL for Bot Connector API
    let url = if let Some(ref activity_id) = reply_to_id {
        format!(
            "{}/v3/conversations/{}/activities/{}",
            service_url, conversation_id, activity_id
        )
    } else {
        format!(
            "{}/v3/conversations/{}/activities",
            service_url, conversation_id
        )
    };

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
            return json_bytes(&json!({
                "ok": false,
                "error": format!("transport error: {}", err.message),
            }));
        }
    };

    if resp.status < 200 || resp.status >= 300 {
        let err_body = resp
            .body
            .as_ref()
            .and_then(|b| serde_json::from_slice::<Value>(b).ok())
            .unwrap_or(Value::Null);
        let err_msg = err_body
            .get("message")
            .or_else(|| err_body.get("error").and_then(|e| e.get("message")))
            .and_then(Value::as_str)
            .unwrap_or("");
        return json_bytes(&json!({
            "ok": false,
            "error": format!("bot connector returned status {}: {}", resp.status, err_msg),
            "response": err_body,
        }));
    }

    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let message_id = body_json
        .get("id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "bot-message".to_string());
    let provider_message_id = format!("teams:{message_id}");

    json_bytes(&json!({
        "ok": true,
        "status": "sent",
        "provider_type": PROVIDER_TYPE,
        "public_base_url": cfg.public_base_url,
        "message_id": message_id,
        "provider_message_id": provider_message_id,
        "response": body_json,
    }))
}

/// Handles the "reply" operation - replies in a thread via Bot Connector API.
///
/// Endpoint: `{serviceUrl}/v3/conversations/{conversationId}/activities/{replyToId}`
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

    let reply_to_id = parsed
        .get("reply_to_id")
        .or_else(|| parsed.get("thread_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if reply_to_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "reply_to_id or thread_id required"}));
    }

    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "text required"}));
    }

    // Get service URL
    let service_url = parsed
        .get("service_url")
        .or_else(|| parsed.get("serviceUrl"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .or_else(|| cfg.default_service_url.clone());

    let service_url = match service_url {
        Some(url) if !url.is_empty() => url.trim_end_matches('/').to_string(),
        _ => {
            return json_bytes(&json!({
                "ok": false,
                "error": "service_url required"
            }));
        }
    };

    // Get conversation ID
    let conversation_id = parsed
        .get("conversation_id")
        .or_else(|| parsed.get("conversationId"))
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    let conversation_id = match conversation_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            return json_bytes(&json!({
                "ok": false,
                "error": "conversation_id required"
            }));
        }
    };

    let token = match acquire_bot_token(&cfg) {
        Ok(tok) => tok,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let url = format!(
        "{}/v3/conversations/{}/activities/{}",
        service_url, conversation_id, reply_to_id
    );

    let body = json!({
        "type": "message",
        "text": text
    });

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
            return json_bytes(&json!({
                "ok": false,
                "error": format!("transport error: {}", err.message),
            }));
        }
    };

    if resp.status < 200 || resp.status >= 300 {
        return json_bytes(&json!({
            "ok": false,
            "error": format!("bot connector returned status {}", resp.status),
        }));
    }

    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let message_id = body_json
        .get("id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "bot-reply".to_string());
    let provider_message_id = format!("teams:{message_id}");

    json_bytes(&json!({
        "ok": true,
        "status": "replied",
        "provider_type": PROVIDER_TYPE,
        "public_base_url": cfg.public_base_url,
        "message_id": message_id,
        "provider_message_id": provider_message_id,
        "response": body_json,
    }))
}

pub(crate) fn send_payload(input_json: &[u8]) -> Vec<u8> {
    let send_in = match serde_json::from_slice::<SendPayloadInV1>(input_json) {
        Ok(value) => value,
        Err(err) => {
            return send_payload_error(&format!("invalid send_payload input: {err}"), false);
        }
    };
    // Accept the historical messaging.teams.graph alias for backward compatibility.
    if !send_in.provider_type.starts_with("messaging.teams") {
        return send_payload_error("provider type mismatch", false);
    }
    let payload_bytes = match STANDARD.decode(&send_in.payload.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => {
            return send_payload_error(&format!("payload decode failed: {err}"), false);
        }
    };
    let payload: Value = serde_json::from_slice(&payload_bytes).unwrap_or(Value::Null);
    let payload_bytes = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
    let result_bytes = handle_send(&payload_bytes);
    let result_value: Value = serde_json::from_slice(&result_bytes).unwrap_or(Value::Null);
    let ok = result_value
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if ok {
        let msg_id = result_value
            .get("message_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        json_bytes(&json!({
            "ok": true,
            "message": msg_id,
            "retryable": false
        }))
    } else {
        let message = result_value
            .get("error")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "send_payload failed".to_string());
        send_payload_error(&message, false)
    }
}

fn build_team_envelope_from_input(
    parsed: &Value,
    cfg: &ProviderConfig,
) -> Result<ChannelMessageEnvelope, String> {
    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| "text required".to_string())?;

    let destination = channel_destination(parsed, cfg)?;
    let team_id = parsed
        .get("team_id")
        .and_then(Value::as_str)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| cfg.team_id.clone());
    let channel_id = parsed
        .get("channel_id")
        .and_then(Value::as_str)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| cfg.channel_id.clone());
    let mut envelope = build_team_envelope(text, None, team_id.clone(), channel_id.clone());
    envelope.to = vec![destination];
    Ok(envelope)
}

fn channel_destination(parsed: &Value, cfg: &ProviderConfig) -> Result<Destination, String> {
    let kind = parsed
        .get("kind")
        .and_then(Value::as_str)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("channel");

    match kind {
        "channel" => {
            let team = parsed
                .get("team_id")
                .and_then(Value::as_str)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| cfg.team_id.clone())
                .ok_or_else(|| "team_id required for channel destination".to_string())?;
            let channel = parsed
                .get("channel_id")
                .and_then(Value::as_str)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| cfg.channel_id.clone())
                .ok_or_else(|| "channel_id required for channel destination".to_string())?;
            Ok(Destination {
                id: format!("{team}:{channel}"),
                kind: Some("channel".into()),
            })
        }
        "chat" => {
            let chat_id = parsed
                .get("chat_id")
                .and_then(Value::as_str)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .ok_or_else(|| "chat_id required for chat destination".to_string())?;
            Ok(Destination {
                id: chat_id,
                kind: Some("chat".into()),
            })
        }
        "conversation" => {
            // Bot Framework conversation - use conversation_id directly
            let conversation_id = parsed
                .get("conversation_id")
                .or_else(|| parsed.get("conversationId"))
                .and_then(Value::as_str)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .ok_or_else(|| {
                    "conversation_id required for conversation destination".to_string()
                })?;
            Ok(Destination {
                id: conversation_id,
                kind: Some("conversation".into()),
            })
        }
        other => Err(format!(
            "unsupported destination kind for envelope fallback: {other}"
        )),
    }
}
