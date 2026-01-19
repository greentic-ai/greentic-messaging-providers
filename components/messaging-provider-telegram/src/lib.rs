use serde::Deserialize;
use serde_json::{Value, json};

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-telegram",
        world: "messaging-provider-telegram",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::http::http_client;
use bindings::greentic::secrets_store::secrets_store;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.telegram.bot";
const CONFIG_SCHEMA_REF: &str = "assets/schemas/messaging/telegram/config.schema.json";
const DEFAULT_API_BASE: &str = "https://api.telegram.org";
const TOKEN_SECRET: &str = "TELEGRAM_BOT_TOKEN";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    bot_token: String,
    #[serde(default)]
    default_chat_id: Option<String>,
    #[serde(default)]
    api_base_url: Option<String>,
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
                    "bot_token": cfg.bot_token,
                    "default_chat_id": cfg.default_chat_id,
                    "api_base_url": cfg.api_base_url.unwrap_or_else(|| DEFAULT_API_BASE.to_string()),
                }
            })),
            Err(err) => json_bytes(&json!({
                "ok": false,
                "error": err,
            })),
        }
    }

    fn healthcheck() -> Vec<u8> {
        json_bytes(&json!({ "status": "ok" }))
    }

    fn invoke(op: String, input_json: Vec<u8>) -> Vec<u8> {
        match op.as_str() {
            "send" => handle_send(&input_json),
            "reply" => handle_reply(&input_json),
            other => json_bytes(&json!({
                "ok": false,
                "error": format!("unsupported op: {other}"),
            })),
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

    let text = match parsed
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
    {
        Some(t) if !t.is_empty() => t,
        _ => return json_bytes(&json!({"ok": false, "error": "text required"})),
    };

    let chat_id = match parsed
        .get("chat_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| cfg.default_chat_id.clone())
    {
        Some(chat) if !chat.is_empty() => chat,
        _ => return json_bytes(&json!({"ok": false, "error": "chat_id required"})),
    };

    let token = match secrets_store::get(&cfg.bot_token) {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return json_bytes(&json!({"ok": false, "error": "bot token not utf-8"})),
        },
        Ok(None) => {
            return json_bytes(&json!({
                "ok": false,
                "error": format!("missing secret: {TOKEN_SECRET}"),
            }));
        }
        Err(e) => {
            return json_bytes(&json!({
                "ok": false,
                "error": format!("secret store error: {e:?}"),
            }));
        }
    };

    let api_base = cfg
        .api_base_url
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{api_base}/bot{token}/sendMessage");
    let payload = json!({ "chat_id": chat_id, "text": text });
    let request = http_client::Request {
        method: "POST".to_string(),
        url,
        headers: vec![("Content-Type".into(), "application/json".into())],
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
            "error": format!("telegram returned status {}", resp.status),
        }));
    }

    let body = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (message_id, provider_message_id) = extract_ids(&body_json);

    json_bytes(&json!({
        "ok": true,
        "status": "sent",
        "provider_type": PROVIDER_TYPE,
        "message_id": message_id,
        "provider_message_id": provider_message_id,
        "response": body_json
    }))
}

fn handle_reply(input_json: &[u8]) -> Vec<u8> {
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

    let text = match parsed
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
    {
        Some(t) if !t.is_empty() => t,
        _ => return json_bytes(&json!({"ok": false, "error": "text required"})),
    };

    let chat_id = match parsed
        .get("chat_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| cfg.default_chat_id.clone())
    {
        Some(chat) if !chat.is_empty() => chat,
        _ => return json_bytes(&json!({"ok": false, "error": "chat_id required"})),
    };

    let reply_to = parsed
        .get("reply_to_id")
        .or_else(|| parsed.get("thread_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if reply_to.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "reply_to_id or thread_id required"}));
    }

    let token = match secrets_store::get(&cfg.bot_token) {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return json_bytes(&json!({"ok": false, "error": "bot token not utf-8"})),
        },
        Ok(None) => {
            return json_bytes(&json!({
                "ok": false,
                "error": format!("missing secret: {TOKEN_SECRET}"),
            }));
        }
        Err(e) => {
            return json_bytes(&json!({
                "ok": false,
                "error": format!("secret store error: {e:?}"),
            }));
        }
    };

    let api_base = cfg
        .api_base_url
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{api_base}/bot{token}/sendMessage");
    let payload = json!({
        "chat_id": chat_id,
        "text": text,
        "reply_to_message_id": reply_to
    });
    let request = http_client::Request {
        method: "POST".to_string(),
        url,
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
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
            "error": format!("telegram returned status {}", resp.status),
        }));
    }

    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let (message_id, provider_message_id) = extract_ids(&body_json);

    json_bytes(&json!({
        "ok": true,
        "status": "replied",
        "provider_type": PROVIDER_TYPE,
        "message_id": message_id,
        "provider_message_id": provider_message_id,
        "response": body_json
    }))
}

fn extract_ids(body: &Value) -> (String, String) {
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

fn parse_config_value(val: &Value) -> Result<ProviderConfig, String> {
    serde_json::from_value::<ProviderConfig>(val.clone())
        .map_err(|e| format!("invalid config: {e}"))
}

fn parse_config_bytes(bytes: &[u8]) -> Result<ProviderConfig, String> {
    serde_json::from_slice::<ProviderConfig>(bytes).map_err(|e| format!("invalid config: {e}"))
}

fn load_config(input: &Value) -> Result<ProviderConfig, String> {
    if let Some(cfg) = input.get("config") {
        return parse_config_value(cfg);
    }
    let mut partial = serde_json::Map::new();
    if let Some(v) = input.get("bot_token") {
        partial.insert("bot_token".into(), v.clone());
    }
    if let Some(v) = input.get("default_chat_id") {
        partial.insert("default_chat_id".into(), v.clone());
    }
    if let Some(v) = input.get("api_base_url") {
        partial.insert("api_base_url".into(), v.clone());
    }
    if !partial.is_empty() {
        return parse_config_value(&Value::Object(partial));
    }

    Ok(ProviderConfig {
        bot_token: TOKEN_SECRET.to_string(),
        default_chat_id: None,
        api_base_url: None,
    })
}

fn json_bytes<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_config_prefers_nested_config() {
        let input = json!({
            "config": {
                "bot_token": "nested-token",
                "default_chat_id": "abc",
            },
            "bot_token": "outer",
        });
        let cfg = load_config(&input).expect("config");
        assert_eq!(cfg.bot_token, "nested-token");
        assert_eq!(cfg.default_chat_id.as_deref(), Some("abc"));
    }

    #[test]
    fn load_config_defaults_to_secret_requirement() {
        let input = json!({"text": "hi"});
        let cfg = load_config(&input).expect("config");
        assert_eq!(cfg.bot_token, TOKEN_SECRET);
        assert!(cfg.default_chat_id.is_none());
    }

    #[test]
    fn parse_config_bytes_rejects_unknown_fields() {
        let cfg = br#"{ "bot_token": "abc", "unknown": "field" }"#;
        let err = parse_config_bytes(cfg).expect_err("should fail");
        assert!(err.contains("unknown field"));
    }

    #[test]
    fn extract_ids_handles_strings() {
        let body = json!({"result": {"message_id": "42"}});
        let (id, provider) = extract_ids(&body);
        assert_eq!(id, "42");
        assert_eq!(provider, "tg:42");
    }
}
