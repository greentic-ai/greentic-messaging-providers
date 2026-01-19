use serde::Deserialize;
use serde_json::{Value, json};
use urlencoding::encode;

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-teams",
        world: "messaging-provider-teams",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::http::http_client;
use bindings::greentic::secrets_store::secrets_store;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.teams.graph";
const CONFIG_SCHEMA_REF: &str = "assets/schemas/messaging/teams/config.schema.json";
const DEFAULT_CLIENT_SECRET_KEY: &str = "MS_GRAPH_CLIENT_SECRET";
const DEFAULT_TOKEN_SCOPE: &str = "https://graph.microsoft.com/.default";
const DEFAULT_GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const DEFAULT_AUTH_BASE: &str = "https://login.microsoftonline.com";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    tenant_id: String,
    client_id: String,
    #[serde(default)]
    client_secret_key: Option<String>,
    #[serde(default)]
    refresh_token_key: Option<String>,
    #[serde(default)]
    team_id: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    graph_base_url: Option<String>,
    #[serde(default)]
    auth_base_url: Option<String>,
    #[serde(default)]
    token_scope: Option<String>,
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
            Ok(cfg) => {
                if cfg.client_secret_key.is_none() && cfg.refresh_token_key.is_none() {
                    return json_bytes(
                        &json!({"ok": false, "error": "client_secret_key or refresh_token_key required"}),
                    );
                }
                json_bytes(&json!({
                    "ok": true,
                    "config": {
                        "tenant_id": cfg.tenant_id,
                        "client_id": cfg.client_id,
                        "client_secret_key": cfg.client_secret_key,
                        "refresh_token_key": cfg.refresh_token_key,
                        "team_id": cfg.team_id,
                        "channel_id": cfg.channel_id,
                        "graph_base_url": cfg.graph_base_url.unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string()),
                        "auth_base_url": cfg.auth_base_url.unwrap_or_else(|| DEFAULT_AUTH_BASE.to_string()),
                        "token_scope": cfg.token_scope.unwrap_or_else(|| DEFAULT_TOKEN_SCOPE.to_string()),
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

    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    if cfg.client_secret_key.is_none() && cfg.refresh_token_key.is_none() {
        return json_bytes(
            &json!({"ok": false, "error": "client_secret_key or refresh_token_key required"}),
        );
    }

    let text = match parsed
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
    {
        Some(t) if !t.is_empty() => t,
        _ => return json_bytes(&json!({"ok": false, "error": "text required"})),
    };

    let team_id = parsed
        .get("team_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| cfg.team_id.clone());
    let channel_id = parsed
        .get("channel_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| cfg.channel_id.clone());

    let (Some(team_id), Some(channel_id)) = (team_id, channel_id) else {
        return json_bytes(&json!({"ok": false, "error": "team_id and channel_id required"}));
    };

    let token = match acquire_token(&cfg) {
        Ok(tok) => tok,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let graph_base = cfg
        .graph_base_url
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());
    let url = format!(
        "{}/teams/{}/channels/{}/messages",
        graph_base, team_id, channel_id
    );
    let body = json!({
        "body": {
            "content": text,
            "contentType": "html"
        }
    });

    let request = http_client::Request {
        method: "POST".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: Some(serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec())),
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
            "error": format!("graph returned status {}", resp.status),
        }));
    }

    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let message_id = body_json
        .get("id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "graph-message".to_string());
    let provider_message_id = format!("teams:{message_id}");

    json_bytes(&json!({
        "ok": true,
        "status": "sent",
        "provider_type": PROVIDER_TYPE,
        "message_id": message_id,
        "provider_message_id": provider_message_id,
        "response": body_json,
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
    let thread_id = parsed
        .get("reply_to_id")
        .or_else(|| parsed.get("thread_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if thread_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "reply_to_id or thread_id required"}));
    }
    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "text required"}));
    }

    let token = match acquire_token(&cfg) {
        Ok(tok) => tok,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let graph_base = cfg
        .graph_base_url
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());
    let team_id = parsed
        .get("team_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| cfg.team_id.clone());
    let channel_id = parsed
        .get("channel_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| cfg.channel_id.clone());
    let (Some(team_id), Some(channel_id)) = (team_id, channel_id) else {
        return json_bytes(&json!({"ok": false, "error": "team_id and channel_id required"}));
    };

    let url = format!(
        "{}/teams/{}/channels/{}/messages/{}/replies",
        graph_base, team_id, channel_id, thread_id
    );
    let body = json!({
        "body": {
            "content": text,
            "contentType": "html"
        }
    });
    let request = http_client::Request {
        method: "POST".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: Some(serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec())),
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
            "error": format!("graph returned status {}", resp.status),
        }));
    }
    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let message_id = body_json
        .get("id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "graph-reply".to_string());
    let provider_message_id = format!("teams:{message_id}");

    json_bytes(&json!({
        "ok": true,
        "status": "replied",
        "provider_type": PROVIDER_TYPE,
        "message_id": message_id,
        "provider_message_id": provider_message_id,
        "response": body_json,
    }))
}

fn acquire_token(cfg: &ProviderConfig) -> Result<String, String> {
    let auth_base = cfg
        .auth_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_AUTH_BASE.to_string());
    let token_url = format!("{}/{}/oauth2/v2.0/token", auth_base, cfg.tenant_id);
    let scope = cfg
        .token_scope
        .clone()
        .unwrap_or_else(|| DEFAULT_TOKEN_SCOPE.to_string());

    if let Some(rt_key) = cfg.refresh_token_key.as_ref() {
        let refresh_token = get_secret(rt_key)?;
        let mut form = format!(
            "client_id={}&grant_type=refresh_token&refresh_token={}&scope={}",
            encode(&cfg.client_id),
            encode(&refresh_token),
            encode(&scope)
        );
        if let Some(secret_key) = cfg.client_secret_key.as_ref()
            && let Ok(secret) = get_secret(secret_key)
        {
            form.push_str(&format!("&client_secret={}", encode(&secret)));
        }
        return send_token_request(&token_url, &form);
    }

    let client_secret_key = cfg
        .client_secret_key
        .as_deref()
        .unwrap_or(DEFAULT_CLIENT_SECRET_KEY);
    let client_secret = get_secret(client_secret_key)?;
    let form = format!(
        "client_id={}&client_secret={}&grant_type=client_credentials&scope={}",
        encode(&cfg.client_id),
        encode(&client_secret),
        encode(&scope)
    );
    send_token_request(&token_url, &form)
}

fn send_token_request(url: &str, form: &str) -> Result<String, String> {
    let request = http_client::Request {
        method: "POST".into(),
        url: url.to_string(),
        headers: vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body: Some(form.as_bytes().to_vec()),
    };

    let resp = http_client::send(&request, None, None)
        .map_err(|e| format!("transport error: {}", e.message))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(format!("token endpoint returned status {}", resp.status));
    }
    let body = resp.body.unwrap_or_default();
    let json: Value =
        serde_json::from_slice(&body).map_err(|e| format!("invalid token response: {e}"))?;
    let token = json
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "token response missing access_token".to_string())?;
    Ok(token.to_string())
}

fn get_secret(key: &str) -> Result<String, String> {
    match secrets_store::get(key) {
        Ok(Some(bytes)) => String::from_utf8(bytes).map_err(|_| format!("secret {key} not utf-8")),
        Ok(None) => Err(format!("missing secret: {key}")),
        Err(e) => Err(format!("secret store error: {e:?}")),
    }
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
    let keys = [
        "tenant_id",
        "client_id",
        "client_secret_key",
        "refresh_token_key",
        "team_id",
        "channel_id",
        "graph_base_url",
        "auth_base_url",
        "token_scope",
    ];
    for key in keys {
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
    fn validate_requires_auth_mode() {
        let cfg = br#"{"tenant_id":"t","client_id":"c"}"#;
        let resp = Component::validate_config(cfg.to_vec());
        let json: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(json.get("ok"), Some(&Value::Bool(false)));
    }

    #[test]
    fn load_config_prefers_nested() {
        let input = json!({
            "config": {
                "tenant_id": "t",
                "client_id": "c",
                "client_secret_key": "inner"
            },
            "tenant_id": "outer"
        });
        let cfg = load_config(&input).expect("cfg");
        assert_eq!(cfg.client_secret_key.as_deref(), Some("inner"));
        assert_eq!(cfg.tenant_id, "t");
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"tenant_id":"t","client_id":"c","unexpected":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }
}
