use base64::{engine::general_purpose::STANDARD, Engine as _};
use messaging_universal_dto::{
    EncodeInV1, HttpInV1, HttpOutV1, ProviderPayloadV1, RenderPlanInV1, RenderPlanOutV1,
    SendPayloadInV1, SendPayloadResultV1,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-slack",
        world: "messaging-provider-slack",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::http::client;
use bindings::greentic::secrets_store::secrets_store;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.slack.api";
const CONFIG_SCHEMA_REF: &str = "schemas/messaging/slack/public.config.schema.json";
const DEFAULT_API_BASE: &str = "https://slack.com/api";
const DEFAULT_BOT_TOKEN_KEY: &str = "SLACK_BOT_TOKEN";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
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

    let token = match get_secret_string(DEFAULT_BOT_TOKEN_KEY) {
        Ok(tok) => tok,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
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
    for key in ["default_channel", "api_base_url"] {
        if let Some(v) = input.get(key) {
            partial.insert(key.to_string(), v.clone());
        }
    }
    if !partial.is_empty() {
        return parse_config_value(&Value::Object(partial));
    }

    Ok(ProviderConfig {
        default_channel: None,
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
    let body_bytes = match STANDARD.decode(&request.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return http_out_error(400, &format!("invalid body encoding: {err}")),
    };
    let body_val: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let normalized = json!({
        "ok": true,
        "event": body_val,
    });
    let normalized_bytes = serde_json::to_vec(&normalized).unwrap_or_else(|_| b"{}".to_vec());
    let out = HttpOutV1 {
        status: 200,
        headers: Vec::new(),
        body_b64: STANDARD.encode(&normalized_bytes),
        events: Vec::new(),
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
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "slack message".to_string());
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
    let channel = encode_in.message.channel.trim();
    if channel.is_empty() {
        return encode_error("channel required");
    }
    let channel = channel.to_string();
    let text = encode_in
        .message
        .text
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "slack universal payload".to_string());
    let url = format!("{}/chat.postMessage", DEFAULT_API_BASE);
    let body = json!({
        "channel": channel,
        "text": text,
    });
    let body_bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let mut metadata = HashMap::new();
    metadata.insert("url".to_string(), Value::String(url));
    metadata.insert("method".to_string(), Value::String("POST".to_string()));
    metadata.insert("channel".to_string(), Value::String(channel));
    let payload = ProviderPayloadV1 {
        content_type: "application/json".to_string(),
        body_b64: STANDARD.encode(&body_bytes),
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
    let url = metadata_string(&metadata, "url")
        .unwrap_or_else(|| format!("{}/chat.postMessage", DEFAULT_API_BASE));
    let method = metadata_string(&metadata, "method").unwrap_or_else(|| "POST".to_string());
    let body_bytes = match STANDARD.decode(&body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return send_payload_error(&format!("payload decode failed: {err}"), false),
    };
    let token = match get_secret_string(DEFAULT_BOT_TOKEN_KEY) {
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
        body: Some(body_bytes),
    };
    let resp = match client::send(&request, None, None) {
        Ok(value) => value,
        Err(err) => {
            return send_payload_error(&format!("transport error: {}", err.message), true);
        }
    };
    if resp.status < 200 || resp.status >= 300 {
        return send_payload_error(
            &format!("slack returned status {}", resp.status),
            resp.status >= 500,
        );
    }
    send_payload_success()
}

fn metadata_string(metadata: &HashMap<String, Value>, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| value.as_str().map(|s| s.to_string()))
}

fn http_out_error(status: u16, message: &str) -> Vec<u8> {
    let out = HttpOutV1 {
        status,
        headers: Vec::new(),
        body_b64: STANDARD.encode(message.as_bytes()),
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
    fn load_config_defaults_to_empty_values() {
        let input = json!({"text":"hi"});
        let cfg = load_config(&input).unwrap();
        assert!(cfg.default_channel.is_none());
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"default_channel":"x","unknown":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }
}
