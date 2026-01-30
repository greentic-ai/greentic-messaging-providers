use base64::{Engine as _, engine::general_purpose};
use greentic_types::{ChannelMessageEnvelope, EnvId, MessageMetadata, TenantCtx, TenantId};
use messaging_universal_dto::{
    EncodeInV1, HttpInV1, HttpOutV1, ProviderPayloadV1, RenderPlanInV1, RenderPlanOutV1,
    SendPayloadInV1, SendPayloadResultV1,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-webchat",
        world: "messaging-provider-webchat",
        generate_all
    });
}
mod directline;

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::state::state_store;
use directline::{HostSecretStore, HostStateStore, handle_directline_request};
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.webchat";
const CONFIG_SCHEMA_REF: &str = "schemas/messaging/webchat/public.config.schema.json";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    tenant_channel_id: Option<String>,
    #[serde(default)]
    public_base_url: Option<String>,
}

struct Component;

impl Guest for Component {
    fn describe() -> Vec<u8> {
        let manifest = ProviderManifest {
            provider_type: PROVIDER_TYPE.to_string(),
            capabilities: vec![],
            ops: vec![
                "send".to_string(),
                "ingest".to_string(),
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
            Ok(cfg) => {
                if cfg.route.is_none() && cfg.tenant_channel_id.is_none() {
                    return json_bytes(
                        &json!({"ok": false, "error": "route or tenant_channel_id required"}),
                    );
                }
                let has_public_base_url = cfg
                    .public_base_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .is_some();
                if !has_public_base_url {
                    return json_bytes(&json!({"ok": false, "error": "public_base_url required"}));
                }
                json_bytes(&json!({
                    "ok": true,
                    "config": {
                        "route": cfg.route,
                        "tenant_channel_id": cfg.tenant_channel_id,
                        "public_base_url": cfg.public_base_url,
                    }
                }))
            }
            Err(err) => json_bytes(&json!({"ok": false, "error": err})),
        }
    }

    fn healthcheck() -> Vec<u8> {
        json_bytes(&json!({"status": "ok"}))
    }

    fn invoke(op: String, input_json: Vec<u8>) -> Vec<u8> {
        match op.as_str() {
            "send" => handle_send(&input_json),
            "ingest" => handle_ingest(&input_json),
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

    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let route = parsed
        .get("route")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| cfg.route.clone());
    let tenant_channel_id = parsed
        .get("tenant_channel_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| cfg.tenant_channel_id.clone());

    if route.is_none() && tenant_channel_id.is_none() {
        return json_bytes(&json!({"ok": false, "error": "route or tenant_channel_id required"}));
    }

    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "text required"}));
    }

    let payload = json!({
        "route": route,
        "tenant_channel_id": tenant_channel_id,
        "public_base_url": cfg.public_base_url,
        "text": text,
    });
    let payload_bytes = json_bytes(&payload);
    let key = route
        .clone()
        .or(tenant_channel_id.clone())
        .unwrap_or_else(|| "webchat".to_string());

    let write_result = state_store::write(&key, &payload_bytes, None);
    if let Err(err) = write_result {
        return json_bytes(
            &json!({"ok": false, "error": format!("state write error: {}", err.message)}),
        );
    }

    let hash_hex = hex_sha256(&payload_bytes);
    let message_id = pseudo_uuid_from_hex(&hash_hex);
    let provider_message_id = format!("webchat:{hash_hex}");

    json_bytes(&json!({
        "ok": true,
        "status": "sent",
        "provider_type": PROVIDER_TYPE,
        "message_id": message_id,
        "provider_message_id": provider_message_id,
        "payload": payload
    }))
}

fn handle_ingest(input_json: &[u8]) -> Vec<u8> {
    let parsed: Value = match serde_json::from_slice(input_json) {
        Ok(val) => val,
        Err(err) => {
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}")}));
        }
    };
    let text = parsed
        .get("text")
        .or_else(|| parsed.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let user = parsed
        .get("user_id")
        .or_else(|| parsed.get("from"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let envelope = json!({
        "from": user,
        "text": text,
        "raw": parsed,
    });
    json_bytes(&json!({"ok": true, "envelope": envelope}))
}

fn ingest_http(input_json: &[u8]) -> Vec<u8> {
    let request = match serde_json::from_slice::<HttpInV1>(input_json) {
        Ok(req) => req,
        Err(err) => return http_out_error(400, &format!("invalid http input: {err}")),
    };
    if request.path.starts_with("/v3/directline") {
        let mut state_driver = HostStateStore;
        let secrets_driver = HostSecretStore;
        let out = handle_directline_request(&request, &mut state_driver, &secrets_driver);
        return json_bytes(&out);
    }
    let body_bytes = match general_purpose::STANDARD.decode(&request.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return http_out_error(400, &format!("invalid body encoding: {err}")),
    };
    let body_val: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let text = extract_text(&body_val);
    let user = user_from_value(&body_val);
    let route =
        non_empty_string(request.route_hint.as_deref()).or_else(|| route_from_value(&body_val));
    let tenant_channel_id = tenant_channel_from_value(&body_val);
    let envelope = build_webchat_envelope(
        text.clone(),
        user.clone(),
        route.clone(),
        tenant_channel_id.clone(),
    );
    let normalized = json!({
        "ok": true,
        "event": body_val,
        "route": route,
        "tenant_channel_id": tenant_channel_id,
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
        .unwrap_or_else(|| "webchat message".to_string());
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
        .unwrap_or_else(|| "webchat universal payload".to_string());
    let metadata_route = encode_in.message.metadata.get("route").cloned();
    let route = metadata_route
        .clone()
        .or_else(|| Some(encode_in.message.session_id.clone()));
    let route_value = route.clone().unwrap_or_else(|| "webchat".to_string());
    let payload_body = json!({
        "text": text,
        "route": route_value.clone(),
        "session_id": encode_in.message.session_id,
    });
    let body_bytes = serde_json::to_vec(&payload_body).unwrap_or_else(|_| b"{}".to_vec());
    let mut metadata = HashMap::new();
    metadata.insert("route".to_string(), Value::String(route_value.clone()));
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
    if text.is_empty() {
        return Err("text required".into());
    }
    let public_base_url = public_base_url_from_value(payload);
    let stored = json!({
        "route": route,
        "tenant_channel_id": tenant_channel_id,
        "public_base_url": public_base_url,
        "text": text,
    });
    state_store::write(&key, &json_bytes(&stored), None)
        .map_err(|err| format!("state write error: {}", err.message))?;
    Ok(())
}

fn build_webchat_envelope(
    text: String,
    user_id: Option<String>,
    route: Option<String>,
    tenant_channel_id: Option<String>,
) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    if let Some(route) = &route {
        metadata.insert("route".to_string(), route.clone());
    }
    if let Some(channel) = &tenant_channel_id {
        metadata.insert("tenant_channel_id".to_string(), channel.clone());
    }
    let channel = route
        .clone()
        .or_else(|| tenant_channel_id.clone())
        .unwrap_or_else(|| "webchat".to_string());
    ChannelMessageEnvelope {
        id: format!("webchat-{channel}"),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: channel.clone(),
        session_id: channel,
        reply_scope: None,
        user_id,
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
    }
}

fn extract_text(value: &Value) -> String {
    value
        .get("text")
        .or_else(|| value.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn user_from_value(value: &Value) -> Option<String> {
    value
        .get("user_id")
        .or_else(|| value.get("from"))
        .and_then(|v| v.as_str())
        .and_then(|s| non_empty_string(Some(s)))
}

fn route_from_value(value: &Value) -> Option<String> {
    value_as_trimmed_string(value.get("route"))
}

fn tenant_channel_from_value(value: &Value) -> Option<String> {
    value_as_trimmed_string(value.get("tenant_channel_id"))
}

fn public_base_url_from_value(value: &Value) -> Option<String> {
    value_as_trimmed_string(value.get("public_base_url"))
}

fn value_as_trimmed_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
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
    for key in ["route", "tenant_channel_id", "public_base_url"] {
        if let Some(v) = input.get(key) {
            partial.insert(key.to_string(), v.clone());
        }
    }
    if !partial.is_empty() {
        return parse_config_value(&Value::Object(partial));
    }

    Err("config required".into())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{:02x}", byte);
    }
    out
}

fn pseudo_uuid_from_hex(hex: &str) -> String {
    let padded = if hex.len() < 32 {
        format!("{hex:0<32}")
    } else {
        hex[..32].to_string()
    };
    format!(
        "{}-{}-{}-{}-{}",
        &padded[0..8],
        &padded[8..12],
        &padded[12..16],
        &padded[16..20],
        &padded[20..32]
    )
}

fn json_bytes<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_requires_route_or_channel() {
        let cfg = br#"{"public_base_url":"https://example.com"}"#;
        let resp = Component::validate_config(cfg.to_vec());
        let json: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(json.get("ok"), Some(&Value::Bool(false)));
    }

    #[test]
    fn load_config_prefers_nested() {
        let input = json!({
            "config": {"route":"inner","public_base_url":"https://example.com"},
            "route": "outer"
        });
        let cfg = load_config(&input).unwrap();
        assert_eq!(cfg.route.as_deref(), Some("inner"));
        assert_eq!(cfg.public_base_url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"route":"r","public_base_url":"https://example.com","extra":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }
}
