//! `ingest_http` op for the Telegram provider.
//!
//! Normalises a raw Telegram webhook body into a
//! [`ChannelMessageEnvelope`] + HTTP response envelope. Supports both regular
//! `message` events (including reply-to-bot detection that flips the envelope
//! into form-input mode) and `callback_query` events from inline keyboard
//! button clicks. Shared envelope + extractor helpers live in this file so
//! that `send.rs` and `send_payload.rs` can reuse them.

use base64::{Engine, engine::general_purpose::STANDARD};
use greentic_types::messaging::universal_dto::HttpOutV1;
use greentic_types::{
    Actor, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};
use provider_common::http_compat::{http_out_error, http_out_v1_bytes, parse_operator_http_in};
use serde_json::{Value, json};

pub(crate) fn ingest_http(input_json: &[u8]) -> Vec<u8> {
    // Use operator-compat parser which handles both `body_b64` (string)
    // and `body` (raw byte array from operator's IngressRequestV1).
    let request = match parse_operator_http_in(input_json) {
        Ok(req) => req,
        Err(err) => return http_out_error(400, &format!("invalid http input: {err}")),
    };
    let body_bytes = match STANDARD.decode(&request.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return http_out_error(400, &format!("invalid body encoding: {err}")),
    };
    let body_val: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

    // Handle callback_query (inline keyboard button clicks — e.g. AC Action.Submit).
    let has_callback = body_val.get("callback_query").is_some();
    let has_message = body_val.get("message").is_some();
    eprintln!(
        "telegram ingest_http: has_callback={} has_message={} keys={:?}",
        has_callback,
        has_message,
        body_val
            .as_object()
            .map(|o| o.keys().collect::<Vec<_>>())
            .unwrap_or_default()
    );
    if let Some(callback) = body_val.get("callback_query") {
        return ingest_callback_query(&body_val, callback);
    }

    let message = body_val.get("message").cloned().unwrap_or(Value::Null);
    let text = extract_message_text(&message);
    let chat_id = extract_chat_id(&message);
    let from = extract_from_user(&message);
    let msg_locale = extract_language_code(&message);
    let mut envelope = build_telegram_envelope_with_locale(
        text.clone(),
        chat_id.clone(),
        from.clone(),
        msg_locale,
    );

    // Detect reply-to-bot messages (form input responses from ForceReply).
    // When user replies to a bot message that had a form prompt, mark the
    // envelope so the flow engine can treat it as form input.
    if let Some(reply_msg) = message.get("reply_to_message") {
        let is_from_bot = reply_msg
            .get("from")
            .and_then(|f| f.get("is_bot"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_from_bot {
            envelope
                .metadata
                .insert("is_form_reply".to_string(), "true".to_string());
            // Pass the original bot message_id for context.
            if let Some(mid) = reply_msg.get("message_id").and_then(Value::as_i64) {
                envelope
                    .metadata
                    .insert("reply_to_bot_message_id".to_string(), mid.to_string());
            }
        }
    }
    let normalized = json!({
        "ok": true,
        "event": body_val,
        "message": message,
        "chat_id": chat_id,
        "from": from
    });
    let normalized_bytes = serde_json::to_vec(&normalized).unwrap_or_else(|_| b"{}".to_vec());
    let out = HttpOutV1 {
        status: 200,
        headers: Vec::new(),
        body_b64: STANDARD.encode(&normalized_bytes),
        events: vec![envelope],
    };
    http_out_v1_bytes(&out)
}

fn ingest_callback_query(body_val: &Value, callback: &Value) -> Vec<u8> {
    let callback_id = callback
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let cb_message = callback.get("message").unwrap_or(&Value::Null);
    let chat_id = extract_chat_id(cb_message);
    let from = extract_from_user(callback);
    let data_str = callback
        .get("data")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    eprintln!(
        "telegram callback_query: id={} data_str={}",
        callback_id, data_str
    );

    // Try to parse callback data as JSON (AC Action.Submit serializes data as JSON).
    let (route_to_card, card_id, action_text) = parse_callback_data(data_str);

    let cb_locale = extract_language_code(callback);
    let mut envelope =
        build_telegram_envelope_with_locale(action_text, chat_id.clone(), from.clone(), cb_locale);
    if !route_to_card.is_empty() {
        envelope
            .metadata
            .insert("routeToCardId".into(), route_to_card);
    }
    if !card_id.is_empty() {
        envelope.metadata.insert("cardId".into(), card_id);
    }
    envelope
        .metadata
        .insert("callback_query_id".into(), callback_id.clone());
    envelope
        .metadata
        .insert("callback_data".into(), data_str.to_string());

    let normalized = json!({
        "ok": true,
        "event": body_val,
        "callback_query": callback,
        "chat_id": chat_id,
        "from": from,
    });
    let normalized_bytes = serde_json::to_vec(&normalized).unwrap_or_else(|_| b"{}".to_vec());
    let out = HttpOutV1 {
        status: 200,
        headers: Vec::new(),
        body_b64: STANDARD.encode(&normalized_bytes),
        events: vec![envelope],
    };
    http_out_v1_bytes(&out)
}

/// Parse callback data which AC Action.Submit serialises as JSON.
/// Supports both full keys (routeToCardId/cardId) and abbreviated keys (r/c)
/// used by `compact_callback_data` to fit Telegram's 64-byte limit.
fn parse_callback_data(data_str: &str) -> (String, String, String) {
    if let Ok(data_val) = serde_json::from_str::<Value>(data_str) {
        let rtc = data_val
            .get("routeToCardId")
            .or_else(|| data_val.get("r"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let cid = data_val
            .get("cardId")
            .or_else(|| data_val.get("c"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let text = if !rtc.is_empty() {
            format!("[card:{rtc}]")
        } else if !cid.is_empty() {
            format!("[action:{cid}]")
        } else {
            format!("[callback:{data_str}]")
        };
        (rtc, cid, text)
    } else {
        (
            String::new(),
            String::new(),
            format!("[callback:{data_str}]"),
        )
    }
}

fn build_telegram_envelope_with_locale(
    text: String,
    chat_id: Option<String>,
    from: Option<String>,
    locale: Option<String>,
) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    if let Some(chat) = &chat_id {
        metadata.insert("chat_id".to_string(), chat.clone());
    }
    if let Some(sender) = &from {
        metadata.insert("from".to_string(), sender.clone());
    }
    if let Some(lang) = &locale
        && !lang.is_empty()
    {
        metadata.insert("locale".to_string(), lang.clone());
    }
    let channel = "telegram".to_string();
    let sender = from.map(|id| Actor {
        id,
        kind: Some("user".into()),
    });
    let destinations = if let Some(chat) = &chat_id {
        vec![Destination {
            id: chat.clone(),
            kind: Some("chat".into()),
        }]
    } else {
        Vec::new()
    };
    ChannelMessageEnvelope {
        id: format!("telegram-{channel}"),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: channel.clone(),
        session_id: chat_id.clone().unwrap_or_else(|| "telegram".to_string()),
        reply_scope: None,
        from: sender,
        to: destinations,
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
        extensions: Default::default(),
    }
}

pub(crate) fn extract_message_text(value: &Value) -> String {
    value
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(crate) fn extract_chat_id(value: &Value) -> Option<String> {
    value
        .get("chat")
        .and_then(|chat| chat.get("id"))
        .and_then(Value::as_i64)
        .map(|id| id.to_string())
}

pub(crate) fn extract_from_user(value: &Value) -> Option<String> {
    value
        .get("from")
        .and_then(|from| from.get("id"))
        .and_then(Value::as_i64)
        .map(|id| id.to_string())
}

/// Extract the user's preferred language from the Telegram `from.language_code` field.
fn extract_language_code(value: &Value) -> Option<String> {
    value
        .get("from")
        .and_then(|from| from.get("language_code"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub(crate) fn extract_ids(body: &Value) -> (String, String) {
    let message_id = body
        .get("result")
        .and_then(|v| v.get("message_id"))
        .map(|val| match val {
            Value::Number(num) => num.to_string(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "dummy-message-id".into());
    let provider_message_id = format!("tg:{message_id}");
    (message_id, provider_message_id)
}
