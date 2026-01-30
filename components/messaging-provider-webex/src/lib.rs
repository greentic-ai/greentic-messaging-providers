use base64::{Engine as _, engine::general_purpose};
use greentic_types::{ChannelMessageEnvelope, EnvId, MessageMetadata, TenantCtx, TenantId};
use messaging_universal_dto::{
    EncodeInV1, HttpInV1, HttpOutV1, ProviderPayloadV1, RenderPlanInV1, RenderPlanOutV1,
    SendPayloadInV1, SendPayloadResultV1,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-webex",
        world: "messaging-provider-webex",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::http::client;
use bindings::greentic::secrets_store::secrets_store;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.webex.bot";
const CONFIG_SCHEMA_REF: &str = "schemas/messaging/webex/public.config.schema.json";
const DEFAULT_API_BASE: &str = "https://webexapis.com/v1";
const DEFAULT_TOKEN_KEY: &str = "WEBEX_BOT_TOKEN";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    #[serde(default)]
    default_room_id: Option<String>,
    #[serde(default)]
    api_base_url: Option<String>,
}

struct Component;

impl Guest for Component {
    fn describe() -> Vec<u8> {
        let manifest = ProviderManifest {
            provider_type: PROVIDER_TYPE.to_string(),
            capabilities: vec![],
            ops: vec![
                "send".to_string(),
                "reply".to_string(),
                "ingest_http".to_string(),
                "render_plan".to_string(),
                "encode".to_string(),
                "send_payload".to_string(),
            ],
            config_schema_ref: Some(CONFIG_SCHEMA_REF.to_string()),
            state_schema_ref: None,
        };
        json_bytes(&manifest)
    }

    fn validate_config(config_json: Vec<u8>) -> Vec<u8> {
        match parse_config_bytes(&config_json) {
            Ok(cfg) => json_bytes(&json!({
                "ok": true,
                "config": {
                    "default_room_id": cfg.default_room_id,
                    "api_base_url": cfg.api_base_url.unwrap_or_else(|| DEFAULT_API_BASE.to_string()),
                }
            })),
            Err(err) => json_bytes(&json!({"ok": false, "error": err})),
        }
    }

    fn healthcheck() -> Vec<u8> {
        json_bytes(&json!({"status": "ok"}))
    }

    fn invoke(op: String, input_json: Vec<u8>) -> Vec<u8> {
        match op.as_str() {
            "send" => handle_send(&input_json),
            "reply" => handle_reply(&input_json),
            "ingest_http" => ingest_http(&input_json),
            "render_plan" => render_plan(&input_json),
            "encode" => encode_op(&input_json),
            "send_payload" => send_payload(&input_json),
            other => json_bytes(&json!({"ok": false, "error": format!("unsupported op: {other}")})),
        }
    }
}

bindings::exports::greentic::provider_schema_core::schema_core_api::__export_greentic_provider_schema_core_schema_core_api_1_0_0_cabi!(
    Component with_types_in bindings::exports::greentic::provider_schema_core::schema_core_api
);

fn handle_send(input_json: &[u8]) -> Vec<u8> {
    let parsed: Value = match serde_json::from_slice(input_json) {
        Ok(val) => val,
        Err(err) => {
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}")}));
        }
    };

    if parsed.get("attachments").is_some() {
        return json_bytes(&json!({"ok": false, "error": "attachments not supported"}));
    }

    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let destination = parsed.get("to").and_then(|v| v.as_object());
    let (room_id, person_id) =
        match destination.and_then(|o| o.get("kind").and_then(|k| k.as_str())) {
            Some("room") => {
                let id = destination
                    .and_then(|o| o.get("id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or(cfg.default_room_id.clone());
                (id, None)
            }
            Some("user") => {
                let id = destination
                    .and_then(|o| o.get("id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                (None, id)
            }
            _ => (cfg.default_room_id.clone(), None),
        };

    let (room_id, person_id) = match (room_id, person_id) {
        (Some(r), p) if !r.is_empty() => (Some(r), p),
        (None, Some(p)) if !p.is_empty() => (None, Some(p)),
        _ => return json_bytes(&json!({"ok": false, "error": "destination required"})),
    };

    let text = parsed
        .get("text")
        .or_else(|| parsed.get("markdown"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "text or markdown required"}));
    }

    let token = match secrets_store::get(DEFAULT_TOKEN_KEY) {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return json_bytes(&json!({"ok": false, "error": "access_token not utf-8"})),
        },
        Ok(None) => {
            return json_bytes(
                &json!({"ok": false, "error": format!("missing secret: {}", DEFAULT_TOKEN_KEY)}),
            );
        }
        Err(e) => {
            return json_bytes(
                &json!({"ok": false, "error": format!("secret store error: {e:?}")}),
            );
        }
    };

    let api_base = cfg
        .api_base_url
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{}/messages", api_base);
    let mut body = json!({ "text": text });
    if let Some(room) = room_id {
        body.as_object_mut()
            .expect("body object")
            .insert("roomId".into(), Value::String(room));
    }
    if let Some(person) = person_id {
        body.as_object_mut()
            .expect("body object")
            .insert("personId".into(), Value::String(person));
    }

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
            return json_bytes(
                &json!({"ok": false, "error": format!("transport error: {}", err.message)}),
            );
        }
    };

    if resp.status < 200 || resp.status >= 300 {
        return json_bytes(
            &json!({"ok": false, "error": format!("webex returned status {}", resp.status)}),
        );
    }

    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let msg_id = body_json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("webex-message")
        .to_string();
    let provider_message_id = format!("webex:{msg_id}");

    json_bytes(&json!({
        "ok": true,
        "status": "sent",
        "provider_type": PROVIDER_TYPE,
        "message_id": msg_id,
        "provider_message_id": provider_message_id,
        "response": body_json
    }))
}

fn handle_reply(_input_json: &[u8]) -> Vec<u8> {
    let parsed: Value = match serde_json::from_slice(_input_json) {
        Ok(val) => val,
        Err(err) => {
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}")}));
        }
    };
    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "text required"}));
    }
    let thread_id = parsed
        .get("reply_to_id")
        .or_else(|| parsed.get("thread_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if thread_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "reply_to_id or thread_id required"}));
    }

    let token = match secrets_store::get(DEFAULT_TOKEN_KEY) {
        Ok(Some(bytes)) => String::from_utf8(bytes).unwrap_or_default(),
        _ => return json_bytes(&json!({"ok": false, "error": "missing access token"})),
    };
    if token.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "access token empty"}));
    }
    let api_base = cfg
        .api_base_url
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{}/messages", api_base);
    let payload = json!({
        "parentId": thread_id,
        "markdown": text,
    });
    let request = client::Request {
        method: "POST".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: Some(serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())),
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
            "error": format!("webex returned status {}", resp.status),
        }));
    }
    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let msg_id = body_json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("webex-reply")
        .to_string();
    let provider_message_id = format!("webex:{msg_id}");

    json_bytes(&json!({
        "ok": true,
        "status": "replied",
        "provider_type": PROVIDER_TYPE,
        "message_id": msg_id,
        "provider_message_id": provider_message_id,
        "response": body_json
    }))
}

fn parse_config_bytes(bytes: &[u8]) -> Result<ProviderConfig, String> {
    serde_json::from_slice::<ProviderConfig>(bytes).map_err(|e| format!("invalid config: {e}"))
}

fn parse_config_value(val: &Value) -> Result<ProviderConfig, String> {
    serde_json::from_value::<ProviderConfig>(val.clone())
        .map_err(|e| format!("invalid config: {e}"))
}

fn load_config(input: &Value) -> Result<ProviderConfig, String> {
    if let Some(cfg) = input.get("config") {
        return parse_config_value(cfg);
    }
    let mut partial = serde_json::Map::new();
    for key in ["default_room_id", "api_base_url"] {
        if let Some(v) = input.get(key) {
            partial.insert(key.to_string(), v.clone());
        }
    }
    if !partial.is_empty() {
        return parse_config_value(&Value::Object(partial));
    }

    Ok(ProviderConfig {
        default_room_id: None,
        api_base_url: None,
    })
}

fn json_bytes<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec())
}

fn ingest_http(input_json: &[u8]) -> Vec<u8> {
    let request = match serde_json::from_slice::<HttpInV1>(input_json) {
        Ok(req) => req,
        Err(err) => return http_out_error(400, &format!("invalid http input: {err}")),
    };
    let body_bytes = match general_purpose::STANDARD.decode(&request.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return http_out_error(400, &format!("invalid body encoding: {err}")),
    };
    let body_val: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let text = body_val
        .get("text")
        .or_else(|| body_val.get("markdown"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let (room_id, person_id) = extract_destination(&body_val);
    let envelope = build_envelope("webex", text, room_id.clone(), person_id.clone());
    let normalized = json!({
        "ok": true,
        "event": body_val,
    });
    let normalized_bytes = serde_json::to_vec(&normalized).unwrap_or_else(|_| b"{}".to_vec());
    let out = HttpOutV1 {
        status: 200,
        headers: Vec::new(),
        body_b64: general_purpose::STANDARD.encode(&normalized_bytes),
        events: vec![envelope],
    };
    json_bytes(&out)
}

fn render_plan(input_json: &[u8]) -> Vec<u8> {
    let plan_in = match serde_json::from_slice::<RenderPlanInV1>(input_json) {
        Ok(value) => value,
        Err(err) => return render_plan_error(&format!("invalid render input: {err}")),
    };
    let summary = plan_in
        .message
        .text
        .clone()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "webex message".to_string());
    let plan_obj = json!({
        "tier": "TierC",
        "summary_text": summary,
        "actions": [],
        "attachments": [],
        "warnings": [],
        "debug": plan_in.metadata,
    });
    let plan_json =
        serde_json::to_string(&plan_obj).unwrap_or_else(|_| "{\"tier\":\"TierC\"}".to_string());
    let plan_out = RenderPlanOutV1 { plan_json };
    json_bytes(&json!({"ok": true, "plan": plan_out}))
}

fn encode_op(input_json: &[u8]) -> Vec<u8> {
    let encode_in = match serde_json::from_slice::<EncodeInV1>(input_json) {
        Ok(value) => value,
        Err(err) => return encode_error(&format!("invalid encode input: {err}")),
    };
    let text = encode_in
        .message
        .text
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "webex universal payload".to_string());
    let room_id = encode_in
        .message
        .metadata
        .get("room_id")
        .map(|s| s.clone())
        .or_else(|| Some(encode_in.message.channel.clone()))
        .filter(|s| !s.trim().is_empty());
    let person_id = encode_in
        .message
        .metadata
        .get("person_id")
        .map(|s| s.clone());
    let mut metadata = HashMap::new();
    metadata.insert(
        "url".to_string(),
        Value::String(format!("{}/messages", DEFAULT_API_BASE)),
    );
    metadata.insert("method".to_string(), Value::String("POST".to_string()));
    if let Some(room) = &room_id {
        metadata.insert("room_id".to_string(), Value::String(room.clone()));
    }
    if let Some(person) = &person_id {
        metadata.insert("person_id".to_string(), Value::String(person.clone()));
    }
    let mut body = json!({ "text": text });
    if let Some(room) = room_id {
        body.as_object_mut()
            .expect("body object")
            .insert("roomId".into(), Value::String(room));
    }
    if let Some(person) = person_id {
        body.as_object_mut()
            .expect("body object")
            .insert("personId".into(), Value::String(person));
    }
    let body_bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let payload = ProviderPayloadV1 {
        content_type: "application/json".to_string(),
        body_b64: general_purpose::STANDARD.encode(&body_bytes),
        metadata,
    };
    json_bytes(&json!({"ok": true, "payload": payload}))
}

fn send_payload(input_json: &[u8]) -> Vec<u8> {
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
    let url = metadata
        .get("url")
        .and_then(|value| value.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}/messages", DEFAULT_API_BASE));
    let method = metadata
        .get("method")
        .and_then(|value| value.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "POST".to_string());
    let body_bytes = match general_purpose::STANDARD.decode(&body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return send_payload_error(&format!("payload decode failed: {err}"), false),
    };
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let text = payload
        .get("text")
        .or_else(|| payload.get("markdown"))
        .and_then(|v| v.as_str())
        .unwrap_or("webex message")
        .to_string();
    let (room_id, person_id) = extract_destination(&payload);
    let (room_id, person_id) = match (room_id, person_id) {
        (Some(room), _) if !room.trim().is_empty() => (Some(room), None),
        (None, Some(person)) if !person.trim().is_empty() => (None, Some(person)),
        _ => return send_payload_error("destination required", false),
    };
    let mut body_req = json!({ "text": text });
    if let Some(room) = room_id.clone() {
        body_req
            .as_object_mut()
            .expect("body object")
            .insert("roomId".into(), Value::String(room));
    }
    if let Some(person) = person_id.clone() {
        body_req
            .as_object_mut()
            .expect("body object")
            .insert("personId".into(), Value::String(person));
    }
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
        return send_payload_error(
            &format!("webex returned status {}", resp.status),
            resp.status >= 500,
        );
    }
    send_payload_success()
}

fn extract_destination(payload: &Value) -> (Option<String>, Option<String>) {
    let direct_room = payload
        .get("roomId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if direct_room.is_some() {
        return (direct_room, None);
    }
    let direct_person = payload
        .get("personId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if direct_person.is_some() {
        return (None, direct_person);
    }
    let to = payload.get("to").and_then(|v| v.as_object());
    let kind = to
        .and_then(|o| o.get("kind"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let id = to
        .and_then(|o| o.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let config_room = payload
        .get("config")
        .and_then(|c| c.get("default_room_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if let Some(choice) = kind {
        match choice.as_str() {
            "room" => return (id.or(config_room), None),
            "user" => return (None, id),
            _ => {}
        }
    }
    (config_room, None)
}

fn build_envelope(
    channel_prefix: &str,
    text: String,
    room_id: Option<String>,
    person_id: Option<String>,
) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    if let Some(room) = &room_id {
        metadata.insert("room_id".to_string(), room.clone());
    }
    if let Some(person) = &person_id {
        metadata.insert("person_id".to_string(), person.clone());
    }
    let channel = room_id
        .clone()
        .or(person_id.clone())
        .unwrap_or_else(|| channel_prefix.to_string());
    ChannelMessageEnvelope {
        id: format!("{channel_prefix}-{channel}"),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: channel.clone(),
        session_id: channel,
        reply_scope: None,
        user_id: person_id,
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
    }
}

fn http_out_error(status: u16, message: &str) -> Vec<u8> {
    let out = HttpOutV1 {
        status,
        headers: Vec::new(),
        body_b64: general_purpose::STANDARD.encode(message.as_bytes()),
        events: Vec::new(),
    };
    json_bytes(&out)
}

fn render_plan_error(message: &str) -> Vec<u8> {
    json_bytes(&json!({"ok": false, "error": message}))
}

fn encode_error(message: &str) -> Vec<u8> {
    json_bytes(&json!({"ok": false, "error": message}))
}

fn send_payload_error(message: &str, retryable: bool) -> Vec<u8> {
    let result = SendPayloadResultV1 {
        ok: false,
        message: Some(message.to_string()),
        retryable,
    };
    json_bytes(&result)
}

fn send_payload_success() -> Vec<u8> {
    let result = SendPayloadResultV1 {
        ok: true,
        message: None,
        retryable: false,
    };
    json_bytes(&result)
}

fn get_secret_string(key: &str) -> Result<String, String> {
    match secrets_store::get(key) {
        Ok(Some(bytes)) => String::from_utf8(bytes).map_err(|_| "secret not valid utf-8".into()),
        Ok(None) => Err(format!("missing secret: {key}")),
        Err(e) => Err(format!("secret store error: {e:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_defaults() {
        let cfg = br#"{"default_room_id":"room"}"#;
        let resp = Component::validate_config(cfg.to_vec());
        let json: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(json.get("ok"), Some(&Value::Bool(true)));
    }

    #[test]
    fn load_config_defaults_to_token_key() {
        let input = json!({});
        let cfg = load_config(&input).unwrap();
        assert!(cfg.default_room_id.is_none());
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"default_room_id":"k","unexpected":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }
}
