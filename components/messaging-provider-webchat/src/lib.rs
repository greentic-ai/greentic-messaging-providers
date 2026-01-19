use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-webchat",
        world: "messaging-provider-webchat",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::state::state_store;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.webchat";
const CONFIG_SCHEMA_REF: &str = "assets/schemas/messaging/webchat/config.schema.json";
const DEFAULT_MODE: &str = "local_queue";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    tenant_channel_id: Option<String>,
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default)]
    base_url: Option<String>,
}

fn default_mode() -> String {
    DEFAULT_MODE.to_string()
}

struct Component;

impl Guest for Component {
    fn describe() -> Vec<u8> {
        let manifest = ProviderManifest {
            provider_type: PROVIDER_TYPE.to_string(),
            capabilities: vec![],
            ops: vec!["send".to_string(), "ingest".to_string()],
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
                json_bytes(&json!({
                    "ok": true,
                    "config": {
                        "route": cfg.route,
                        "tenant_channel_id": cfg.tenant_channel_id,
                        "mode": cfg.mode,
                        "base_url": cfg.base_url,
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
        "mode": cfg.mode,
        "base_url": cfg.base_url,
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
    for key in ["route", "tenant_channel_id", "mode", "base_url"] {
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
        let cfg = br#"{"mode":"local_queue"}"#;
        let resp = Component::validate_config(cfg.to_vec());
        let json: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(json.get("ok"), Some(&Value::Bool(false)));
    }

    #[test]
    fn load_config_prefers_nested() {
        let input = json!({
            "config": {"route":"inner","mode":"pubsub"},
            "route": "outer"
        });
        let cfg = load_config(&input).unwrap();
        assert_eq!(cfg.route.as_deref(), Some("inner"));
        assert_eq!(cfg.mode, "pubsub");
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"route":"r","mode":"local_queue","extra":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }
}
