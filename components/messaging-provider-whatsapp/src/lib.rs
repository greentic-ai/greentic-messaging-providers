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
        path: "wit/messaging-provider-whatsapp",
        world: "messaging-provider-whatsapp",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::http::client;
use bindings::greentic::secrets_store::secrets_store;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.whatsapp.cloud";
const CONFIG_SCHEMA_REF: &str = "schemas/messaging/whatsapp/public.config.schema.json";
const DEFAULT_API_BASE: &str = "https://graph.facebook.com";
const DEFAULT_API_VERSION: &str = "v19.0";
const DEFAULT_TOKEN_KEY: &str = "WHATSAPP_TOKEN";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    phone_number_id: String,
    #[serde(default)]
    business_account_id: Option<String>,
    #[serde(default)]
    api_base_url: Option<String>,
    #[serde(default)]
    api_version: Option<String>,
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
                    "phone_number_id": cfg.phone_number_id,
                    "business_account_id": cfg.business_account_id,
                    "api_base_url": cfg.api_base_url.unwrap_or_else(|| DEFAULT_API_BASE.to_string()),
                    "api_version": cfg.api_version.unwrap_or_else(|| DEFAULT_API_VERSION.to_string()),
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

    if let Some(rich) = parsed.get("rich")
        && rich.get("format").and_then(Value::as_str) == Some("whatsapp_template")
    {
        return json_bytes(&json!({"ok": false, "error": "template messages not supported yet"}));
    }

    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let to_kind = parsed
        .get("to")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let to_id = parsed
        .get("to")
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if to_kind != "user" || to_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "to.kind=user with to.id required"}));
    }

    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "text required"}));
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
    let api_version = cfg
        .api_version
        .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());
    let url = format!(
        "{}/{}/{}/messages",
        api_base, api_version, cfg.phone_number_id
    );

    let payload = json!({
        "messaging_product": "whatsapp",
        "to": to_id,
        "type": "text",
        "text": {"body": text},
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
            return json_bytes(
                &json!({"ok": false, "error": format!("transport error: {}", err.message)}),
            );
        }
    };

    if resp.status < 200 || resp.status >= 300 {
        return json_bytes(
            &json!({"ok": false, "error": format!("whatsapp returned status {}", resp.status)}),
        );
    }

    let body = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let msg_id = body_json
        .get("messages")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("wa-message")
        .to_string();
    let provider_message_id = format!("whatsapp:{msg_id}");

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
    let to_kind = parsed
        .get("to")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let to_id = parsed
        .get("to")
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if to_kind != "user" || to_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "to.kind=user with to.id required"}));
    }
    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "text required"}));
    }
    let reply_to = parsed
        .get("reply_to_id")
        .or_else(|| parsed.get("thread_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if reply_to.is_empty() {
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
    let api_version = cfg
        .api_version
        .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());
    let url = format!(
        "{}/{}/{}/messages",
        api_base, api_version, cfg.phone_number_id
    );
    let payload = json!({
        "messaging_product": "whatsapp",
        "to": to_id,
        "type": "text",
        "context": {"message_id": reply_to},
        "text": { "body": text }
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
            "error": format!("whatsapp returned status {}", resp.status),
        }));
    }
    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let msg_id = body_json
        .get("messages")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("wa-reply")
        .to_string();
    let provider_message_id = format!("whatsapp:{msg_id}");

    json_bytes(&json!({
        "ok": true,
        "status": "replied",
        "provider_type": PROVIDER_TYPE,
        "message_id": msg_id,
        "provider_message_id": provider_message_id,
        "response": body_json
    }))
}

fn ingest_http(input_json: &[u8]) -> Vec<u8> {
    let request = match serde_json::from_slice::<HttpInV1>(input_json) {
        Ok(req) => req,
        Err(err) => return http_out_error(400, &format!("invalid http input: {err}")),
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
        return json_bytes(&out);
    }
    let body_bytes = match general_purpose::STANDARD.decode(&request.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return http_out_error(400, &format!("invalid body encoding: {err}")),
    };
    let body_val: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let text = body_val
        .get("text")
        .and_then(|t| t.get("body"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let from = body_val
        .get("from")
        .and_then(Value::as_str)
        .map(str::to_string);
    let envelope = build_whatsapp_envelope(text.clone(), from.clone());
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
        .unwrap_or_else(|| "whatsapp message".to_string());
    let plan_obj = json!({
        "tier": "TierD",
        "summary_text": summary,
        "actions": [],
        "attachments": [],
        "warnings": [],
        "debug": plan_in.metadata,
    });
    let plan_json =
        serde_json::to_string(&plan_obj).unwrap_or_else(|_| "{\"tier\":\"TierD\"}".to_string());
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
        .unwrap_or_else(|| "universal whatsapp payload".to_string());
    let to_id = encode_in
        .message
        .metadata
        .get("from")
        .cloned()
        .unwrap_or_else(|| "whatsapp-user".to_string());
    let phone_number_id = encode_in
        .message
        .metadata
        .get("phone_number_id")
        .cloned()
        .unwrap_or_else(|| "phone-universal".to_string());
    let to = json!({
        "kind": "user",
        "id": to_id,
    });
    let config = json!({
        "phone_number_id": phone_number_id,
    });
    let payload_body = json!({
        "text": text,
        "to": to,
        "config": config,
    });
    let body_bytes = serde_json::to_vec(&payload_body).unwrap_or_else(|_| b"{}".to_vec());
    let mut metadata = HashMap::new();
    metadata.insert("method".to_string(), Value::String("POST".to_string()));
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
    let payload_bytes = match general_purpose::STANDARD.decode(&send_in.payload.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => {
            return send_payload_error(&format!("payload decode failed: {err}"), false);
        }
    };
    let payload: Value = serde_json::from_slice(&payload_bytes).unwrap_or(Value::Null);
    match forward_send_payload(&payload) {
        Ok(_) => send_payload_success(),
        Err(err) => send_payload_error(&err, false),
    }
}

fn forward_send_payload(payload: &Value) -> Result<(), String> {
    let payload_bytes =
        serde_json::to_vec(payload).map_err(|err| format!("serialize failed: {err}"))?;
    let result = handle_send(&payload_bytes);
    let result_value: Value =
        serde_json::from_slice(&result).map_err(|err| format!("parse send result: {err}"))?;
    let ok = result_value
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        let message = result_value
            .get("error")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "send_payload failed".to_string());
        Err(message)
    }
}

fn build_whatsapp_envelope(text: String, from: Option<String>) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    if let Some(sender) = &from {
        metadata.insert("from".to_string(), sender.clone());
    }
    metadata.insert("channel_id".to_string(), "whatsapp".to_string());
    ChannelMessageEnvelope {
        id: format!("whatsapp-{}", text),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: "whatsapp".to_string(),
        session_id: "whatsapp".to_string(),
        reply_scope: None,
        user_id: from,
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
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
    for key in [
        "phone_number_id",
        "business_account_id",
        "api_base_url",
        "api_version",
    ] {
        if let Some(v) = input.get(key) {
            partial.insert(key.to_string(), v.clone());
        }
    }
    if !partial.is_empty() {
        return parse_config_value(&Value::Object(partial));
    }

    Err("config required".into())
}

fn json_bytes<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_requires_phone_number_id() {
        let cfg = br#"{"business_account_id":"k"}"#;
        let resp = Component::validate_config(cfg.to_vec());
        let json: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(json.get("ok"), Some(&Value::Bool(false)));
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"phone_number_id":"p","unexpected":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }

    #[test]
    fn load_config_prefers_nested() {
        let input = json!({
            "config": {"phone_number_id":"pn","api_version":"v20.0"},
            "api_version": "outer"
        });
        let cfg = load_config(&input).unwrap();
        assert_eq!(cfg.api_version.as_deref(), Some("v20.0"));
        assert_eq!(cfg.phone_number_id, "pn");
    }
}
