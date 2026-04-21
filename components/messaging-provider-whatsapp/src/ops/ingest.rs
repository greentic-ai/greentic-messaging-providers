use base64::{Engine as _, engine::general_purpose};
use greentic_types::messaging::universal_dto::{HttpInV1, HttpOutV1};
use greentic_types::{
    Actor, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};
use provider_common::http_compat::{http_out_error, http_out_v1_bytes, parse_operator_http_in};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::config::load_config;

pub(crate) fn ingest_http(input_json: &[u8]) -> Vec<u8> {
    // Try native greentic-types format first, fall back to operator format
    let request = match serde_json::from_slice::<HttpInV1>(input_json) {
        Ok(req) => req,
        Err(_) => match parse_operator_http_in(input_json) {
            Ok(req) => req,
            Err(err) => return http_out_error(400, &format!("invalid http input: {err}")),
        },
    };
    if request.method.eq_ignore_ascii_case("GET") {
        let challenge = parse_query(&request.query)
            .and_then(|params| params.get("hub.challenge").cloned())
            .unwrap_or_default();
        let out = HttpOutV1 {
            status: 200,
            headers: Vec::new(),
            body_b64: general_purpose::STANDARD.encode(challenge.as_bytes()),
            events: Vec::new(),
        };
        return http_out_v1_bytes(&out);
    }
    let body_bytes = match general_purpose::STANDARD.decode(&request.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return http_out_error(400, &format!("invalid body encoding: {err}")),
    };
    let body_val: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    // Extract message from Cloud API nested format: entry[].changes[].value.messages[]
    let cloud_msg = body_val
        .get("entry")
        .and_then(|e| e.as_array())
        .and_then(|arr| arr.first())
        .and_then(|e| e.get("changes"))
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("value"))
        .and_then(|v| v.get("messages"))
        .and_then(|m| m.as_array())
        .and_then(|arr| arr.first());
    // Use Cloud API message if available, otherwise fall back to flat format
    let msg = cloud_msg.unwrap_or(&body_val);
    let text = msg
        .get("text")
        .and_then(|t| t.get("body"))
        .and_then(Value::as_str)
        .or_else(|| msg.get("text").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let from = msg.get("from").and_then(Value::as_str).map(str::to_string);
    // Extract phone_number_id from Cloud API metadata
    let cloud_phone_id = body_val
        .get("entry")
        .and_then(|e| e.as_array())
        .and_then(|arr| arr.first())
        .and_then(|e| e.get("changes"))
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("value"))
        .and_then(|v| v.get("metadata"))
        .and_then(|m| m.get("phone_number_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut envelope = build_whatsapp_envelope(text.clone(), from.clone(), cloud_phone_id);
    // WhatsApp doesn't include locale in webhooks; use provider config default.
    if let Ok(wa_cfg) = load_config(&body_val)
        && let Some(locale) = &wa_cfg.default_locale
        && !locale.is_empty()
    {
        envelope
            .metadata
            .insert("locale".to_string(), locale.clone());
    }
    let normalized = json!({
        "ok": true,
        "event": body_val,
        "text": text,
        "from": from,
    });
    let normalized_bytes = serde_json::to_vec(&normalized).unwrap_or_else(|_| b"{}".to_vec());
    let out = HttpOutV1 {
        status: 200,
        headers: Vec::new(),
        body_b64: general_purpose::STANDARD.encode(&normalized_bytes),
        events: vec![envelope],
    };
    http_out_v1_bytes(&out)
}

fn build_whatsapp_envelope(
    text: String,
    from: Option<String>,
    phone_number_id: Option<String>,
) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    metadata.insert("channel_id".to_string(), "whatsapp".to_string());
    let pnid = phone_number_id.unwrap_or_else(|| "unknown".to_string());
    metadata.insert("phone_number_id".to_string(), pnid);
    let sender = from.map(|id| Actor {
        id,
        kind: Some("user".into()),
    });
    if let Some(actor) = &sender {
        metadata.insert("from".to_string(), actor.id.clone());
    }
    let destinations = if let Some(actor) = &sender {
        vec![Destination {
            id: actor.id.clone(),
            kind: Some("phone".into()),
        }]
    } else {
        Vec::new()
    };
    ChannelMessageEnvelope {
        id: format!("whatsapp-{}", text),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: "whatsapp".to_string(),
        session_id: "whatsapp".to_string(),
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

fn parse_query(query: &Option<String>) -> Option<HashMap<String, String>> {
    let query = query.as_deref()?;
    let mut map = HashMap::new();
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
            map.insert(key.to_string(), value.to_string());
        }
    }
    if map.is_empty() { None } else { Some(map) }
}
