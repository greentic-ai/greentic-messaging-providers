use serde::Deserialize;
use serde_json::{Value, json};

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-slack",
        world: "messaging-provider-slack",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::http::http_client;
use bindings::greentic::secrets_store::secrets_store;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.slack.api";
const CONFIG_SCHEMA_REF: &str = "assets/schemas/messaging/slack/config.schema.json";
const DEFAULT_API_BASE: &str = "https://slack.com/api";
const DEFAULT_BOT_TOKEN_KEY: &str = "SLACK_BOT_TOKEN";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    bot_token: String,
    #[serde(default)]
    signing_secret: Option<String>,
    #[serde(default)]
    default_channel: Option<String>,
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
                    "signing_secret": cfg.signing_secret,
                    "default_channel": cfg.default_channel,
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
            "send" => handle_send(&input_json, false),
            "reply" => handle_send(&input_json, true),
            other => json_bytes(&json!({"ok": false, "error": format!("unsupported op: {other}")})),
        }
    }
}

bindings::exports::greentic::provider_schema_core::schema_core_api::__export_greentic_provider_schema_core_schema_core_api_1_0_0_cabi!(
    Component with_types_in bindings::exports::greentic::provider_schema_core::schema_core_api
);

fn handle_send(input_json: &[u8], is_reply: bool) -> Vec<u8> {
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

    let channel = destination_channel(&parsed, &cfg);
    let channel = match channel {
        Some(c) if !c.is_empty() => c,
        _ => return json_bytes(&json!({"ok": false, "error": "channel required"})),
    };

    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let thread_ts = if is_reply {
        parsed
            .get("thread_id")
            .or_else(|| parsed.get("reply_to_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    let (format, blocks) = parse_blocks(&parsed);

    let token_key = cfg.bot_token.as_str();
    let token = match secrets_store::get(token_key) {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return json_bytes(&json!({"ok": false, "error": "bot token not utf-8"})),
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
    let url = format!("{}/chat.postMessage", api_base);
    let mut payload = json!({
        "channel": channel,
        "text": text,
    });
    if let Some(ts) = thread_ts {
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("thread_ts".into(), Value::String(ts));
    }
    if format.as_deref() == Some("slack_blocks")
        && let Some(b) = blocks
    {
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("blocks".into(), b);
    }

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
            &json!({"ok": false, "error": format!("slack returned status {}", resp.status)}),
        );
    }

    let body = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let ts = body_json
        .get("ts")
        .or_else(|| body_json.get("message").and_then(|m| m.get("ts")))
        .and_then(|v| v.as_str())
        .unwrap_or("pending-ts")
        .to_string();
    let provider_message_id = format!("slack:{ts}");

    json_bytes(&json!({
        "ok": true,
        "status": if is_reply {"replied"} else {"sent"},
        "provider_type": PROVIDER_TYPE,
        "message_id": ts,
        "provider_message_id": provider_message_id,
        "response": body_json
    }))
}

fn destination_channel(parsed: &Value, cfg: &ProviderConfig) -> Option<String> {
    if let Some(to) = parsed.get("to").and_then(|v| v.as_object())
        && let Some(id) = to.get("id").and_then(|v| v.as_str())
    {
        return Some(id.to_string());
    }
    cfg.default_channel.clone()
}

fn parse_blocks(parsed: &Value) -> (Option<String>, Option<Value>) {
    let format = parsed
        .get("rich")
        .and_then(|v| v.get("format"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let blocks = parsed.get("rich").and_then(|v| v.get("blocks")).cloned();
    (format, blocks)
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
        "bot_token",
        "signing_secret",
        "default_channel",
        "api_base_url",
    ] {
        if let Some(v) = input.get(key) {
            partial.insert(key.to_string(), v.clone());
        }
    }
    if !partial.is_empty() {
        return parse_config_value(&Value::Object(partial));
    }

    Ok(ProviderConfig {
        bot_token: DEFAULT_BOT_TOKEN_KEY.to_string(),
        signing_secret: None,
        default_channel: None,
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
    fn validate_requires_bot_token() {
        let cfg = br#"{"default_channel":"c"}"#;
        let resp = Component::validate_config(cfg.to_vec());
        let json: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(json.get("ok"), Some(&Value::Bool(false)));
    }

    #[test]
    fn load_config_defaults_to_token_key() {
        let input = json!({"text":"hi"});
        let cfg = load_config(&input).unwrap();
        assert_eq!(cfg.bot_token, DEFAULT_BOT_TOKEN_KEY);
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"bot_token":"x","unknown":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }
}
