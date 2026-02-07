use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, LocalResult, SecondsFormat, TimeZone, Utc};
use greentic_types::messaging::universal_dto::{
    EncodeInV1, HttpInV1, HttpOutV1, ProviderPayloadV1, RenderPlanInV1, RenderPlanOutV1,
    SendPayloadInV1, SendPayloadResultV1, SubscriptionDeleteInV1, SubscriptionDeleteOutV1,
    SubscriptionEnsureInV1, SubscriptionEnsureOutV1, SubscriptionRenewInV1, SubscriptionRenewOutV1,
};
use greentic_types::{
    Actor, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt;
use urlencoding::encode as url_encode;

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-teams",
        world: "messaging-provider-teams",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::http::client;
use bindings::greentic::secrets_store::secrets_store;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.teams.graph";
const CONFIG_SCHEMA_REF: &str = "schemas/messaging/teams/public.config.schema.json";
const DEFAULT_CLIENT_SECRET_KEY: &str = "MS_GRAPH_CLIENT_SECRET";
const DEFAULT_REFRESH_TOKEN_KEY: &str = "MS_GRAPH_REFRESH_TOKEN";
const DEFAULT_TOKEN_SCOPE: &str = "https://graph.microsoft.com/.default";
const DEFAULT_GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const DEFAULT_AUTH_BASE: &str = "https://login.microsoftonline.com";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    tenant_id: String,
    client_id: String,
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
            ops: vec![
                "send".to_string(),
                "reply".to_string(),
                "ingest_http".to_string(),
                "render_plan".to_string(),
                "encode".to_string(),
                "send_payload".to_string(),
                "subscription_ensure".to_string(),
                "subscription_renew".to_string(),
                "subscription_delete".to_string(),
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
                    "tenant_id": cfg.tenant_id,
                    "client_id": cfg.client_id,
                    "team_id": cfg.team_id,
                    "channel_id": cfg.channel_id,
                    "graph_base_url": cfg.graph_base_url.unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string()),
                    "auth_base_url": cfg.auth_base_url.unwrap_or_else(|| DEFAULT_AUTH_BASE.to_string()),
                    "token_scope": cfg.token_scope.unwrap_or_else(|| DEFAULT_TOKEN_SCOPE.to_string()),
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
            "subscription_ensure" => subscription_ensure(&input_json),
            "subscription_renew" => subscription_renew(&input_json),
            "subscription_delete" => subscription_delete(&input_json),
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
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}") }));
        }
    };

    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let envelope: ChannelMessageEnvelope = match serde_json::from_slice(input_json) {
        Ok(env) => env,
        Err(err) => match build_team_envelope_from_input(&parsed, &cfg) {
            Ok(env) => env,
            Err(message) => {
                return json_bytes(
                    &json!({"ok": false, "error": format!("invalid envelope: {message}: {err}") }),
                );
            }
        },
    };

    if !envelope.attachments.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "attachments not supported"}));
    }

    let text = envelope
        .text
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let text = match text {
        Some(value) => value,
        None => return json_bytes(&json!({"ok": false, "error": "text required"})),
    };

    let destination = envelope
        .to
        .first()
        .cloned()
        .or_else(|| default_channel_destination(&cfg));
    let destination = match destination {
        Some(dest) => dest,
        None => return json_bytes(&json!({"ok": false, "error": "destination required"})),
    };

    let dest_id = destination.id.trim();
    if dest_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "destination id required"}));
    }
    let dest_id = dest_id.to_string();
    let kind = destination.kind.as_deref().unwrap_or("channel");
    let graph_base = cfg
        .graph_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());

    let url = match kind {
        "channel" => {
            let (team_id, channel_id) = match dest_id.split_once(':') {
                Some((team, channel)) => {
                    let team = team.trim();
                    let channel = channel.trim();
                    if team.is_empty() || channel.is_empty() {
                        return json_bytes(&json!({
                            "ok": false,
                            "error": "channel destination must include team_id and channel_id",
                        }));
                    }
                    (team.to_string(), channel.to_string())
                }
                None => {
                    return json_bytes(&json!({
                        "ok": false,
                        "error": "channel destination must be team_id:channel_id",
                    }));
                }
            };
            format!("{graph_base}/teams/{team_id}/channels/{channel_id}/messages")
        }
        "chat" => {
            if dest_id.is_empty() {
                return json_bytes(&json!({"ok": false, "error": "destination id required"}));
            }
            format!("{graph_base}/chats/{dest_id}/messages")
        }
        other => {
            return json_bytes(&json!({
                "ok": false,
                "error": format!("unsupported destination kind: {other}"),
            }));
        }
    };

    let token = match acquire_token(&cfg) {
        Ok(tok) => tok,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let body = json!({
        "body": {
            "content": text,
            "contentType": "html"
        }
    });

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
    let text = extract_team_text(&body_val);
    let team_id = extract_team_id(&body_val);
    let channel_id = extract_channel_id(&body_val);
    let user = extract_sender(&body_val);
    let envelope = build_team_envelope(text.clone(), user, team_id.clone(), channel_id.clone());
    let normalized = json!({
        "ok": true,
        "event": body_val,
        "team_id": team_id,
        "channel_id": channel_id,
    });
    let normalized_bytes = serde_json::to_vec(&normalized).unwrap_or_else(|_| b"{}".to_vec());
    let out = HttpOutV1 {
        status: 200,
        headers: Vec::new(),
        body_b64: STANDARD.encode(&normalized_bytes),
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
        .unwrap_or_else(|| "teams message".to_string());
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
        .unwrap_or_else(|| "universal teams payload".to_string());
    let team_id = encode_in.message.metadata.get("team_id").cloned();
    let channel_id = encode_in
        .message
        .metadata
        .get("channel_id")
        .cloned()
        .or_else(|| {
            let channel = encode_in.message.channel.clone();
            if channel.is_empty() {
                None
            } else {
                Some(channel)
            }
        });
    let payload_body = json!({
        "text": text,
        "team_id": team_id.clone(),
        "channel_id": channel_id.clone(),
    });
    let body_bytes = serde_json::to_vec(&payload_body).unwrap_or_else(|_| b"{}".to_vec());
    let mut metadata = BTreeMap::new();
    if let Some(team) = team_id {
        metadata.insert("team_id".to_string(), Value::String(team));
    }
    if let Some(channel) = channel_id {
        metadata.insert("channel_id".to_string(), Value::String(channel));
    }
    metadata.insert("method".to_string(), Value::String("POST".to_string()));
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
    let payload_bytes = match STANDARD.decode(&send_in.payload.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => {
            return send_payload_error(&format!("payload decode failed: {err}"), false);
        }
    };
    let payload: Value = serde_json::from_slice(&payload_bytes).unwrap_or(Value::Null);
    let payload_bytes = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
    let result_bytes = handle_send(&payload_bytes);
    let result_value: Value = serde_json::from_slice(&result_bytes).unwrap_or(Value::Null);
    let ok = result_value
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if ok {
        send_payload_success()
    } else {
        let message = result_value
            .get("error")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "send_payload failed".to_string());
        send_payload_error(&message, false)
    }
}

fn build_team_envelope(
    text: String,
    user_id: Option<String>,
    team_id: Option<String>,
    channel_id: Option<String>,
) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    if let Some(team) = &team_id {
        metadata.insert("team_id".to_string(), team.clone());
    }
    if let Some(channel) = &channel_id {
        metadata.insert("channel_id".to_string(), channel.clone());
    }
    if let Some(sender) = &user_id {
        metadata.insert("from".to_string(), sender.clone());
    }
    let channel_name = channel_id
        .clone()
        .or_else(|| team_id.clone())
        .unwrap_or_else(|| "teams".to_string());
    let sender = user_id.map(|id| Actor {
        id,
        kind: Some("user".into()),
    });
    ChannelMessageEnvelope {
        id: format!("teams-{channel_name}"),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: channel_name.clone(),
        session_id: channel_name,
        reply_scope: None,
        from: sender,
        to: Vec::new(),
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
    }
}

fn build_team_envelope_from_input(
    parsed: &Value,
    cfg: &ProviderConfig,
) -> Result<ChannelMessageEnvelope, String> {
    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| "text required".to_string())?;

    let destination = channel_destination(parsed, cfg)?;
    let team_id = parsed
        .get("team_id")
        .and_then(Value::as_str)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| cfg.team_id.clone());
    let channel_id = parsed
        .get("channel_id")
        .and_then(Value::as_str)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| cfg.channel_id.clone());
    let mut envelope = build_team_envelope(text, None, team_id.clone(), channel_id.clone());
    envelope.to = vec![destination];
    Ok(envelope)
}

fn channel_destination(parsed: &Value, cfg: &ProviderConfig) -> Result<Destination, String> {
    let kind = parsed
        .get("kind")
        .and_then(Value::as_str)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("channel");
    match kind {
        "channel" => {
            let team = parsed
                .get("team_id")
                .and_then(Value::as_str)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| cfg.team_id.clone())
                .ok_or_else(|| "team_id required for channel destination".to_string())?;
            let channel = parsed
                .get("channel_id")
                .and_then(Value::as_str)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| cfg.channel_id.clone())
                .ok_or_else(|| "channel_id required for channel destination".to_string())?;
            Ok(Destination {
                id: format!("{team}:{channel}"),
                kind: Some("channel".into()),
            })
        }
        "chat" => {
            let chat_id = parsed
                .get("chat_id")
                .and_then(Value::as_str)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .ok_or_else(|| "chat_id required for chat destination".to_string())?;
            Ok(Destination {
                id: chat_id,
                kind: Some("chat".into()),
            })
        }
        other => Err(format!(
            "unsupported destination kind for envelope fallback: {other}"
        )),
    }
}

fn extract_team_text(value: &Value) -> String {
    value
        .get("resourceData")
        .and_then(|rd| rd.get("body"))
        .and_then(|body| body.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn extract_team_id(value: &Value) -> Option<String> {
    value
        .get("resourceData")
        .and_then(|rd| rd.get("channelIdentity"))
        .and_then(|ci| ci.get("teamId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn extract_channel_id(value: &Value) -> Option<String> {
    value
        .get("resourceData")
        .and_then(|rd| rd.get("channelIdentity"))
        .and_then(|ci| ci.get("channelId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn extract_sender(value: &Value) -> Option<String> {
    value
        .get("resourceData")
        .and_then(|rd| rd.get("from"))
        .and_then(|from| from.get("user"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
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

fn subscription_ensure(input_json: &[u8]) -> Vec<u8> {
    let parsed: Value = match serde_json::from_slice(input_json) {
        Ok(value) => value,
        Err(err) => {
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}")}));
        }
    };

    let dto = match serde_json::from_slice::<SubscriptionEnsureInV1>(input_json) {
        Ok(value) => value,
        Err(err) => {
            return json_bytes(
                &json!({"ok": false, "error": format!("invalid subscription ensure input: {err}")}),
            );
        }
    };

    if let Err(err) = ensure_provider(&dto.provider) {
        return json_bytes(&json!({"ok": false, "error": err}));
    }

    let mut config_value = parsed.clone();
    if let Some(map) = config_value.as_object_mut() {
        if let Some(tenant) = dto.tenant_hint.clone() {
            map.insert("tenant_id".into(), Value::String(tenant));
        }
        if let Some(team) = dto.team_hint.clone() {
            map.insert("team_id".into(), Value::String(team));
        }
    }

    let cfg = match load_config(&config_value) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let token = match acquire_token(&cfg) {
        Ok(token) => token,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    if dto.change_types.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "change_types required"}));
    }

    let change_type = dto.change_types.join(",");
    let expiration_target_ms = match dto.expiration_target_unix_ms {
        Some(ms) => ms,
        None => {
            return json_bytes(
                &json!({"ok": false, "error": "expiration_target_unix_ms required"}),
            );
        }
    };
    let expiration_iso = match expiration_ms_to_iso(expiration_target_ms) {
        Ok(text) => text,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let client_state = dto.client_state.clone().or_else(|| dto.binding_id.clone());

    let subscription = match create_subscription(
        &cfg,
        &token,
        &dto.notification_url,
        &dto.resource,
        &change_type,
        &expiration_iso,
        client_state.as_deref(),
    ) {
        Ok(sub) => sub,
        Err(err) => {
            if matches!(err, GraphRequestError::Status(409)) {
                let existing = match list_subscriptions(&cfg, &token) {
                    Ok(subs) => subs,
                    Err(err) => return json_bytes(&json!({"ok": false, "error": err.to_string()})),
                };
                if let Some(found) = existing.into_iter().find(|sub| {
                    sub.resource == dto.resource
                        && sub.change_type == change_type
                        && sub
                            .notification_url
                            .as_deref()
                            .map(|url| url == dto.notification_url)
                            .unwrap_or(false)
                }) {
                    if let Err(err) = renew_subscription(&cfg, &token, &found.id, &expiration_iso) {
                        return json_bytes(&json!({"ok": false, "error": err.to_string()}));
                    }
                    let mut updated = found.clone();
                    updated.expiration_datetime = Some(expiration_iso.clone());
                    updated
                } else {
                    return json_bytes(
                        &json!({"ok": false, "error": "subscription conflict: existing subscription not found"}),
                    );
                }
            } else {
                return json_bytes(&json!({"ok": false, "error": err.to_string()}));
            }
        }
    };

    let expiration_unix_ms = match subscription.expiration_datetime.as_deref() {
        Some(datetime) => parse_expiration_ms(datetime).unwrap_or(expiration_target_ms),
        None => expiration_target_ms,
    };

    let out = SubscriptionEnsureOutV1 {
        v: 1,
        subscription_id: subscription.id.clone(),
        expiration_unix_ms,
        resource: subscription.resource.clone(),
        change_types: dto.change_types.clone(),
        client_state,
        metadata: dto.metadata.clone(),
        binding_id: dto.binding_id.clone(),
        user: dto.user.clone(),
    };
    json_bytes(&json!({"ok": true, "subscription": out}))
}

fn subscription_renew(input_json: &[u8]) -> Vec<u8> {
    let parsed: Value = match serde_json::from_slice(input_json) {
        Ok(value) => value,
        Err(err) => {
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}")}));
        }
    };

    let dto = match serde_json::from_slice::<SubscriptionRenewInV1>(input_json) {
        Ok(value) => value,
        Err(err) => {
            return json_bytes(
                &json!({"ok": false, "error": format!("invalid subscription renew input: {err}")}),
            );
        }
    };

    if let Err(err) = ensure_provider(&dto.provider) {
        return json_bytes(&json!({"ok": false, "error": err}));
    }

    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let token = match acquire_token(&cfg) {
        Ok(token) => token,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let expiration_target_ms = match dto.expiration_target_unix_ms {
        Some(ms) => ms,
        None => {
            return json_bytes(
                &json!({"ok": false, "error": "expiration_target_unix_ms required"}),
            );
        }
    };
    let expiration_iso = match expiration_ms_to_iso(expiration_target_ms) {
        Ok(text) => text,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    if let Err(err) = renew_subscription(&cfg, &token, &dto.subscription_id, &expiration_iso) {
        return json_bytes(&json!({"ok": false, "error": err.to_string()}));
    }

    let expiration_unix_ms = parse_expiration_ms(&expiration_iso).unwrap_or(expiration_target_ms);
    let out = SubscriptionRenewOutV1 {
        v: 1,
        subscription_id: dto.subscription_id,
        expiration_unix_ms,
        metadata: dto.metadata,
        user: dto.user.clone(),
    };
    json_bytes(&json!({"ok": true, "subscription": out}))
}

fn subscription_delete(input_json: &[u8]) -> Vec<u8> {
    let parsed: Value = match serde_json::from_slice(input_json) {
        Ok(value) => value,
        Err(err) => {
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}")}));
        }
    };

    let dto = match serde_json::from_slice::<SubscriptionDeleteInV1>(input_json) {
        Ok(value) => value,
        Err(err) => {
            return json_bytes(
                &json!({"ok": false, "error": format!("invalid subscription delete input: {err}")}),
            );
        }
    };

    if let Err(err) = ensure_provider(&dto.provider) {
        return json_bytes(&json!({"ok": false, "error": err}));
    }

    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let token = match acquire_token(&cfg) {
        Ok(token) => token,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    if let Err(err) = delete_subscription(&cfg, &token, &dto.subscription_id) {
        return json_bytes(&json!({"ok": false, "error": err.to_string()}));
    }

    let out = SubscriptionDeleteOutV1 {
        v: 1,
        subscription_id: dto.subscription_id,
        user: dto.user.clone(),
    };
    json_bytes(&json!({"ok": true, "subscription": out}))
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

    if let Ok(refresh_token) = get_secret(DEFAULT_REFRESH_TOKEN_KEY) {
        let mut form = format!(
            "client_id={}&grant_type=refresh_token&refresh_token={}&scope={}",
            url_encode(&cfg.client_id),
            url_encode(&refresh_token),
            url_encode(&scope)
        );
        if let Ok(secret) = get_secret(DEFAULT_CLIENT_SECRET_KEY) {
            form.push_str(&format!("&client_secret={}", url_encode(&secret)));
        }
        return send_token_request(&token_url, &form);
    }

    let client_secret = get_secret(DEFAULT_CLIENT_SECRET_KEY)?;
    let form = format!(
        "client_id={}&client_secret={}&grant_type=client_credentials&scope={}",
        url_encode(&cfg.client_id),
        url_encode(&client_secret),
        url_encode(&scope)
    );
    send_token_request(&token_url, &form)
}

fn send_token_request(url: &str, form: &str) -> Result<String, String> {
    let request = client::Request {
        method: "POST".into(),
        url: url.to_string(),
        headers: vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body: Some(form.as_bytes().to_vec()),
    };

    let resp = client::send(&request, None, None)
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

fn default_channel_destination(cfg: &ProviderConfig) -> Option<Destination> {
    let team = cfg.team_id.as_ref()?;
    let channel = cfg.channel_id.as_ref()?;
    let team = team.trim();
    let channel = channel.trim();
    if team.is_empty() || channel.is_empty() {
        return None;
    }
    Some(Destination {
        id: format!("{team}:{channel}"),
        kind: Some("channel".into()),
    })
}

fn ensure_provider(provider: &str) -> Result<(), String> {
    match provider {
        "teams" | "msgraph" => Ok(()),
        other => Err(format!("unsupported provider: {other}")),
    }
}

fn expiration_ms_to_iso(ms: u64) -> Result<String, String> {
    let secs = (ms / 1000) as i64;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    match Utc.timestamp_opt(secs, nanos) {
        LocalResult::Single(datetime) => Ok(datetime.to_rfc3339_opts(SecondsFormat::Secs, true)),
        _ => Err("invalid expiration timestamp".to_string()),
    }
}

fn parse_expiration_ms(value: &str) -> Result<u64, String> {
    let dt = DateTime::parse_from_rfc3339(value)
        .map_err(|e| format!("invalid expiration datetime: {e}"))?;
    Ok(dt.timestamp_millis() as u64)
}

#[derive(Clone, Debug)]
struct ExistingSubscription {
    id: String,
    resource: String,
    change_type: String,
    expiration_datetime: Option<String>,
    notification_url: Option<String>,
}

#[derive(Debug)]
enum GraphRequestError {
    Status(u16),
    Transport(String),
    Parse(String),
}

impl fmt::Display for GraphRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphRequestError::Status(code) => {
                write!(f, "graph request failed with status {}", code)
            }
            GraphRequestError::Transport(err) => write!(f, "{}", err),
            GraphRequestError::Parse(err) => write!(f, "{}", err),
        }
    }
}

impl std::error::Error for GraphRequestError {}

fn list_subscriptions(
    cfg: &ProviderConfig,
    token: &str,
) -> Result<Vec<ExistingSubscription>, GraphRequestError> {
    let graph_base = cfg
        .graph_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());
    let url = format!("{}/subscriptions", graph_base);
    let request = client::Request {
        method: "GET".into(),
        url,
        headers: vec![("Authorization".into(), format!("Bearer {}", token))],
        body: None,
    };
    let resp = client::send(&request, None, None)
        .map_err(|e| GraphRequestError::Transport(format!("transport error: {}", e.message)))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(GraphRequestError::Status(resp.status));
    }
    let body = resp.body.unwrap_or_default();
    let json: Value = serde_json::from_slice(&body)
        .map_err(|e| GraphRequestError::Parse(format!("invalid subscriptions response: {e}")))?;
    let list = json
        .get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for item in list {
        let id = item.get("id").and_then(Value::as_str);
        let resource = item.get("resource").and_then(Value::as_str);
        let change_type = item.get("changeType").and_then(Value::as_str);
        if let (Some(id), Some(resource), Some(change_type)) = (id, resource, change_type) {
            out.push(ExistingSubscription {
                id: id.to_string(),
                resource: resource.to_string(),
                change_type: change_type.to_string(),
                expiration_datetime: item
                    .get("expirationDateTime")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string()),
                notification_url: item
                    .get("notificationUrl")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string()),
            });
        }
    }
    Ok(out)
}

fn create_subscription(
    cfg: &ProviderConfig,
    token: &str,
    notification_url: &str,
    resource: &str,
    change_type: &str,
    expiration: &str,
    client_state: Option<&str>,
) -> Result<ExistingSubscription, GraphRequestError> {
    let graph_base = cfg
        .graph_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());
    let url = format!("{}/subscriptions", graph_base);
    let mut payload = json!({
        "changeType": change_type,
        "notificationUrl": notification_url,
        "resource": resource,
        "expirationDateTime": expiration,
    });
    if let Some(state) = client_state {
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("clientState".into(), Value::String(state.to_string()));
    }
    let request = client::Request {
        method: "POST".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {}", token)),
        ],
        body: Some(serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())),
    };
    let resp = client::send(&request, None, None)
        .map_err(|e| GraphRequestError::Transport(format!("transport error: {}", e.message)))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(GraphRequestError::Status(resp.status));
    }
    let body = resp.body.unwrap_or_default();
    let json: Value = serde_json::from_slice(&body)
        .map_err(|e| GraphRequestError::Parse(format!("invalid create response: {e}")))?;
    let id = json
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| GraphRequestError::Parse("create response missing id".to_string()))?;
    Ok(ExistingSubscription {
        id: id.to_string(),
        resource: resource.to_string(),
        change_type: change_type.to_string(),
        expiration_datetime: json
            .get("expirationDateTime")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .or_else(|| Some(expiration.to_string())),
        notification_url: Some(notification_url.to_string()),
    })
}

fn renew_subscription(
    cfg: &ProviderConfig,
    token: &str,
    subscription_id: &str,
    expiration: &str,
) -> Result<(), GraphRequestError> {
    let graph_base = cfg
        .graph_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());
    let url = format!("{}/subscriptions/{}", graph_base, subscription_id);
    let payload = json!({ "expirationDateTime": expiration });
    let request = client::Request {
        method: "PATCH".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {}", token)),
        ],
        body: Some(serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())),
    };
    let resp = client::send(&request, None, None)
        .map_err(|e| GraphRequestError::Transport(format!("transport error: {}", e.message)))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(GraphRequestError::Status(resp.status));
    }
    Ok(())
}

fn delete_subscription(
    cfg: &ProviderConfig,
    token: &str,
    subscription_id: &str,
) -> Result<(), GraphRequestError> {
    let graph_base = cfg
        .graph_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());
    let url = format!("{}/subscriptions/{}", graph_base, subscription_id);
    let request = client::Request {
        method: "DELETE".into(),
        url,
        headers: vec![("Authorization".into(), format!("Bearer {}", token))],
        body: None,
    };
    let resp = client::send(&request, None, None)
        .map_err(|e| GraphRequestError::Transport(format!("transport error: {}", e.message)))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(GraphRequestError::Status(resp.status));
    }
    Ok(())
}

fn json_bytes<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_allows_defaults() {
        let cfg = br#"{"tenant_id":"t","client_id":"c"}"#;
        let resp = Component::validate_config(cfg.to_vec());
        let json: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(json.get("ok"), Some(&Value::Bool(true)));
    }

    #[test]
    fn load_config_prefers_nested() {
        let input = json!({
            "config": {
                "tenant_id": "t",
                "client_id": "c"
            },
            "tenant_id": "outer"
        });
        let cfg = load_config(&input).expect("cfg");
        assert_eq!(cfg.tenant_id, "t");
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"tenant_id":"t","client_id":"c","unexpected":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }
}
