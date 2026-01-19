use serde::Deserialize;
use serde_json::{Value, json};

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-whatsapp",
        world: "messaging-provider-whatsapp",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::http::http_client;
use bindings::greentic::secrets_store::secrets_store;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.whatsapp.cloud";
const CONFIG_SCHEMA_REF: &str = "assets/schemas/messaging/whatsapp/config.schema.json";
const DEFAULT_API_BASE: &str = "https://graph.facebook.com";
const DEFAULT_API_VERSION: &str = "v19.0";
const DEFAULT_TOKEN_KEY: &str = "WHATSAPP_TOKEN";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    #[serde(default = "default_token_key")]
    access_token: String,
    phone_number_id: String,
    #[serde(default)]
    business_account_id: Option<String>,
    #[serde(default)]
    api_base_url: Option<String>,
    #[serde(default)]
    api_version: Option<String>,
}

fn default_token_key() -> String {
    DEFAULT_TOKEN_KEY.to_string()
}

struct Component;

impl Guest for Component {
    fn describe() -> Vec<u8> {
        let manifest = ProviderManifest {
            provider_type: PROVIDER_TYPE.to_string(),
            capabilities: vec![],
            ops: vec!["send".to_string(), "reply".to_string()],
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
                    "access_token": cfg.access_token,
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

    let token_key = cfg.access_token;
    let token = match secrets_store::get(&token_key) {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return json_bytes(&json!({"ok": false, "error": "access_token not utf-8"})),
        },
        Ok(None) => {
            return json_bytes(
                &json!({"ok": false, "error": format!("missing secret: {}", token_key)}),
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

    let request = http_client::Request {
        method: "POST".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: Some(serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())),
    };

    let resp = match http_client::send(&request, None, None) {
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

    let token = match secrets_store::get(&cfg.access_token) {
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
    let request = http_client::Request {
        method: "POST".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: Some(serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())),
    };
    let resp = match http_client::send(&request, None, None) {
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
        "access_token",
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
        let cfg = br#"{"access_token":"k"}"#;
        let resp = Component::validate_config(cfg.to_vec());
        let json: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(json.get("ok"), Some(&Value::Bool(false)));
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"access_token":"k","phone_number_id":"p","unexpected":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }

    #[test]
    fn load_config_prefers_nested() {
        let input = json!({
            "config": {"access_token":"k","phone_number_id":"pn","api_version":"v20.0"},
            "api_version": "outer"
        });
        let cfg = load_config(&input).unwrap();
        assert_eq!(cfg.api_version.as_deref(), Some("v20.0"));
        assert_eq!(cfg.phone_number_id, "pn");
    }
}
