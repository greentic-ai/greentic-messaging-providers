use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Duration, SecondsFormat, TimeZone, Utc};
use greentic_types::messaging::universal_dto::{
    AuthUserRefV1, EncodeInV1, Header, HttpInV1, HttpOutV1, ProviderPayloadV1, RenderPlanInV1,
    RenderPlanOutV1, SendPayloadInV1, SendPayloadResultV1, SubscriptionDeleteInV1,
    SubscriptionDeleteOutV1, SubscriptionEnsureInV1, SubscriptionEnsureOutV1,
    SubscriptionRenewInV1, SubscriptionRenewOutV1,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use urlencoding::decode as url_decode;

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-email",
        world: "messaging-provider-email",
        generate_all
    });
}

mod auth;

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::http::client;
use greentic_types::{
    Actor, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, ProviderManifest,
    TenantCtx, TenantId,
};

const PROVIDER_TYPE: &str = "messaging.email.smtp";
const CONFIG_SCHEMA_REF: &str = "schemas/messaging/email/public.config.schema.json";
const DEFAULT_GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const GRAPH_MAX_EXPIRATION_MINUTES: u32 = 4230;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    username: String,
    from_address: String,
    #[serde(default = "default_tls")]
    tls_mode: String,
    #[serde(default)]
    default_to_address: Option<String>,
    #[serde(default)]
    graph_tenant_id: Option<String>,
    #[serde(default)]
    graph_authority: Option<String>,
    #[serde(default)]
    graph_base_url: Option<String>,
    #[serde(default)]
    graph_token_endpoint: Option<String>,
    #[serde(default)]
    graph_scope: Option<String>,
}

fn default_port() -> u16 {
    587
}

fn default_tls() -> String {
    "starttls".to_string()
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
                    "host": cfg.host,
                    "port": cfg.port,
                    "username": cfg.username,
                    "from_address": cfg.from_address,
                    "default_to_address": cfg.default_to_address,
                    "tls_mode": cfg.tls_mode,
                    "graph_tenant_id": cfg.graph_tenant_id,
                    "graph_authority": cfg.graph_authority,
                    "graph_base_url": cfg.graph_base_url,
                    "graph_token_endpoint": cfg.graph_token_endpoint,
                    "graph_scope": cfg.graph_scope,
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
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}")}));
        }
    };

    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let envelope = match serde_json::from_slice::<ChannelMessageEnvelope>(input_json) {
        Ok(env) => {
            eprintln!("parsed envelope to={:?}", env.to);
            env
        }
        Err(err) => {
            eprintln!("fallback envelope due to parse error: {err}");
            build_channel_envelope(&parsed, &cfg)
        }
    };

    if !envelope.attachments.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "attachments not supported"}));
    }

    let body = envelope
        .text
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let body = match body {
        Some(value) => value,
        None => return json_bytes(&json!({"ok": false, "error": "text required"})),
    };

    let destination = envelope.to.first().cloned().or_else(|| {
        cfg.default_to_address.clone().map(|addr| Destination {
            id: addr,
            kind: Some("email".into()),
        })
    });
    let destination = match destination {
        Some(dest) => dest,
        None => return json_bytes(&json!({"ok": false, "error": "destination required"})),
    };

    let dest_id = destination.id.trim();
    if dest_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "destination id required"}));
    }
    let kind = destination.kind.as_deref().unwrap_or("email");
    if kind != "email" && !kind.is_empty() {
        return json_bytes(&json!({
            "ok": false,
            "error": format!("unsupported destination kind: {kind}"),
        }));
    }

    let subject = envelope
        .metadata
        .get("subject")
        .cloned()
        .unwrap_or_else(|| "email message".to_string());

    let payload = json!({
        "from": cfg.from_address,
        "to": dest_id,
        "subject": subject,
        "body": body,
        "host": cfg.host,
        "port": cfg.port,
        "username": cfg.username,
        "tls_mode": cfg.tls_mode,
    });
    let hash = hex_sha256(&json_bytes(&payload));
    let message_id = pseudo_uuid_from_hex(&hash);
    let provider_message_id = format!("smtp:{hash}");

    json_bytes(&json!({
        "ok": true,
        "status": "sent",
        "provider_type": PROVIDER_TYPE,
        "message_id": message_id,
        "provider_message_id": provider_message_id,
        "payload": payload
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

    let to = match parsed.get("to").and_then(|v| v.as_str()) {
        Some(addr) if !addr.is_empty() => addr.to_string(),
        _ => return json_bytes(&json!({"ok": false, "error": "to required"})),
    };
    let subject = parsed
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let body = parsed
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let thread_ref = parsed
        .get("reply_to_id")
        .or_else(|| parsed.get("thread_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let payload = json!({
        "from": cfg.from_address,
        "to": to,
        "subject": subject,
        "body": body,
        "in_reply_to": thread_ref,
        "host": cfg.host,
        "port": cfg.port,
        "username": cfg.username,
        "tls_mode": cfg.tls_mode,
    });
    let hash = hex_sha256(&json_bytes(&payload));
    let message_id = pseudo_uuid_from_hex(&hash);
    let provider_message_id = format!("smtp-reply:{hash}");

    json_bytes(&json!({
        "ok": true,
        "status": "replied",
        "provider_type": PROVIDER_TYPE,
        "message_id": message_id,
        "provider_message_id": provider_message_id,
        "payload": payload
    }))
}

fn ingest_http(input_json: &[u8]) -> Vec<u8> {
    let http = match serde_json::from_slice::<HttpInV1>(input_json) {
        Ok(value) => value,
        Err(err) => return http_out_error(400, &format!("invalid http input: {err}")),
    };
    match http.method.to_uppercase().as_str() {
        "GET" => handle_validation(&http),
        "POST" => handle_graph_notifications(&http),
        _ => http_out_error(405, "method not allowed"),
    }
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
        .unwrap_or_else(|| "email message".to_string());
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
        .unwrap_or_else(|| "universal email payload".to_string());
    let to = encode_in
        .message
        .metadata
        .get("to")
        .cloned()
        .unwrap_or_else(|| "recipient@example.com".to_string());
    let subject = encode_in
        .message
        .metadata
        .get("subject")
        .cloned()
        .unwrap_or_else(|| "universal subject".to_string());
    let payload_body = json!({
        "to": to.clone(),
        "subject": subject.clone(),
        "body": text,
    });
    let body_bytes = serde_json::to_vec(&payload_body).unwrap_or_else(|_| b"{}".to_vec());
    let mut metadata = BTreeMap::new();
    metadata.insert("to".to_string(), Value::String(to));
    metadata.insert("subject".to_string(), Value::String(subject));
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
    let payload_bytes: Vec<u8> = match STANDARD.decode(&send_in.payload.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => {
            return send_payload_error(&format!("payload decode failed: {err}"), false);
        }
    };
    let payload: Value = serde_json::from_slice(&payload_bytes).unwrap_or(Value::Null);
    let subject = payload
        .get("subject")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let to = payload
        .get("to")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let body = payload
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if to.is_empty() {
        return send_payload_error("missing email target", false);
    }
    if subject.is_empty() {
        return send_payload_error("subject required", false);
    }
    let auth_user = match send_in.auth_user {
        Some(user) => user,
        None => return send_payload_error("auth_user missing", false),
    };
    let mut config_value = serde_json::Map::new();
    for key in [
        "host",
        "port",
        "username",
        "from_address",
        "tls_mode",
        "graph_tenant_id",
        "graph_authority",
        "graph_base_url",
        "graph_token_endpoint",
        "graph_scope",
    ] {
        if let Some(value) = send_in.payload.metadata.get(key) {
            config_value.insert(key.to_string(), value.clone());
        }
    }
    let cfg = if !config_value.is_empty() {
        match parse_config_value(&Value::Object(config_value)) {
            Ok(cfg) => cfg,
            Err(err) => return send_payload_error(&err, false),
        }
    } else {
        return send_payload_error("config metadata required for send_payload", false);
    };
    let token = match auth::acquire_graph_token(&cfg, &auth_user) {
        Ok(value) => value,
        Err(err) => return send_payload_error(&err, true),
    };
    let mail_body = json!({
        "message": {
            "subject": subject,
            "body": { "contentType": "Text", "content": body },
            "toRecipients": [
                { "emailAddress": { "address": to } }
            ]
        },
        "saveToSentItems": false
    });
    let url = format!("{}/me/sendMail", graph_base_url(&cfg));
    if let Err(err) = graph_post(&token, &url, &mail_body) {
        return send_payload_error(&err, true);
    }
    send_payload_success()
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
    let parsed = match serde_json::from_slice::<Value>(input_json) {
        Ok(value) => value,
        Err(err) => {
            return subscription_error(&format!("invalid subscription input: {err}"));
        }
    };
    let dto = match serde_json::from_value::<SubscriptionEnsureInV1>(parsed.clone()) {
        Ok(value) => value,
        Err(err) => {
            return subscription_error(&format!("invalid subscription payload: {err}"));
        }
    };
    if let Err(err) = ensure_provider(&dto.provider) {
        return subscription_error(&err);
    }
    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return subscription_error(&err),
    };
    let token = match auth::acquire_graph_token(&cfg, &dto.user) {
        Ok(value) => value,
        Err(err) => return subscription_error(&err),
    };
    let change_types = if dto.change_types.is_empty() {
        vec!["created".to_string()]
    } else {
        dto.change_types.clone()
    };
    let expiration = target_expiration(dto.expiration_minutes, dto.expiration_target_unix_ms);
    let expiration = clamp_expiration(expiration);
    let iso_expiration = expiration.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut body = json!({
        "changeType": change_types.join(","),
        "notificationUrl": dto.notification_url,
        "resource": dto.resource,
        "expirationDateTime": iso_expiration,
    });
    if let Some(client_state) = &dto.client_state {
        body["clientState"] = Value::String(client_state.clone());
    }
    if let Some(metadata) = &dto.metadata {
        body["metadata"] = metadata.clone();
    }
    let url = format!("{}/subscriptions", graph_base_url(&cfg));
    let resp = match graph_post(&token, &url, &body) {
        Ok(value) => value,
        Err(err) => return subscription_error(&err),
    };
    let subscription_id = resp
        .get("id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_default();
    if subscription_id.is_empty() {
        return subscription_error("subscription response missing id");
    }
    let expiration_ms = resp
        .get("expirationDateTime")
        .and_then(Value::as_str)
        .and_then(parse_datetime)
        .map(|dt| dt.timestamp_millis() as u64)
        .unwrap_or_else(|| expiration.timestamp_millis() as u64);
    let out = SubscriptionEnsureOutV1 {
        v: 1,
        subscription_id,
        expiration_unix_ms: expiration_ms,
        resource: dto.resource,
        change_types,
        client_state: dto.client_state.clone(),
        metadata: dto.metadata.clone(),
        binding_id: dto.binding_id.clone(),
        user: dto.user,
    };
    json_bytes(&json!({"ok": true, "subscription": out}))
}

fn subscription_renew(input_json: &[u8]) -> Vec<u8> {
    let parsed = match serde_json::from_slice::<Value>(input_json) {
        Ok(value) => value,
        Err(err) => {
            return subscription_error(&format!("invalid subscription input: {err}"));
        }
    };
    let dto = match serde_json::from_value::<SubscriptionRenewInV1>(parsed.clone()) {
        Ok(value) => value,
        Err(err) => {
            return subscription_error(&format!("invalid subscription payload: {err}"));
        }
    };
    if let Err(err) = ensure_provider(&dto.provider) {
        return subscription_error(&err);
    }
    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return subscription_error(&err),
    };
    let token = match auth::acquire_graph_token(&cfg, &dto.user) {
        Ok(value) => value,
        Err(err) => return subscription_error(&err),
    };
    let expiration = target_expiration(dto.expiration_minutes, dto.expiration_target_unix_ms);
    let expiration = clamp_expiration(expiration);
    let iso_expiration = expiration.to_rfc3339_opts(SecondsFormat::Secs, true);
    let body = json!({
        "expirationDateTime": iso_expiration,
    });
    let url = format!(
        "{}/subscriptions/{}",
        graph_base_url(&cfg),
        dto.subscription_id
    );
    let resp = match graph_patch(&token, &url, &body) {
        Ok(value) => value,
        Err(err) => return subscription_error(&err),
    };
    let expiration_ms = resp
        .get("expirationDateTime")
        .and_then(Value::as_str)
        .and_then(parse_datetime)
        .map(|dt| dt.timestamp_millis() as u64)
        .unwrap_or_else(|| expiration.timestamp_millis() as u64);
    let out = SubscriptionRenewOutV1 {
        v: 1,
        subscription_id: dto.subscription_id,
        expiration_unix_ms: expiration_ms,
        metadata: dto.metadata.clone(),
        user: dto.user,
    };
    json_bytes(&json!({"ok": true, "subscription": out}))
}

fn subscription_delete(input_json: &[u8]) -> Vec<u8> {
    let parsed = match serde_json::from_slice::<Value>(input_json) {
        Ok(value) => value,
        Err(err) => {
            return subscription_error(&format!("invalid subscription input: {err}"));
        }
    };
    let dto = match serde_json::from_value::<SubscriptionDeleteInV1>(parsed.clone()) {
        Ok(value) => value,
        Err(err) => {
            return subscription_error(&format!("invalid subscription payload: {err}"));
        }
    };
    if let Err(err) = ensure_provider(&dto.provider) {
        return subscription_error(&err);
    }
    let cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return subscription_error(&err),
    };
    let token = match auth::acquire_graph_token(&cfg, &dto.user) {
        Ok(value) => value,
        Err(err) => return subscription_error(&err),
    };
    let url = format!(
        "{}/subscriptions/{}",
        graph_base_url(&cfg),
        dto.subscription_id
    );
    if let Err(err) = graph_delete(&token, &url) {
        return subscription_error(&err);
    }
    let out = SubscriptionDeleteOutV1 {
        v: 1,
        subscription_id: dto.subscription_id,
        user: dto.user,
    };
    json_bytes(&json!({"ok": true, "subscription": out}))
}

fn subscription_error(message: &str) -> Vec<u8> {
    json_bytes(&json!({"ok": false, "error": message}))
}

fn ensure_provider(provider: &str) -> Result<(), String> {
    if provider != PROVIDER_TYPE {
        return Err(format!(
            "provider mismatch: expected {PROVIDER_TYPE}, got {provider}"
        ));
    }
    Ok(())
}

fn target_expiration(minutes: Option<u32>, target_unix_ms: Option<u64>) -> DateTime<Utc> {
    if let Some(ms) = target_unix_ms
        && let Some(dt) = parse_datetime_value(ms)
    {
        return dt;
    }
    if let Some(mins) = minutes {
        return Utc::now() + Duration::minutes(mins as i64);
    }
    Utc::now() + Duration::minutes(GRAPH_MAX_EXPIRATION_MINUTES as i64)
}

fn clamp_expiration(expiration: DateTime<Utc>) -> DateTime<Utc> {
    let now = Utc::now();
    let max = now + Duration::minutes(GRAPH_MAX_EXPIRATION_MINUTES as i64);
    if expiration > max {
        max
    } else if expiration < now {
        now
    } else {
        expiration
    }
}

fn parse_datetime(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn parse_datetime_value(unix_ms: u64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(unix_ms as i64).single()
}

fn graph_base_url(cfg: &ProviderConfig) -> String {
    cfg.graph_base_url
        .as_ref()
        .cloned()
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn graph_post(token: &str, url: &str, body: &Value) -> Result<Value, String> {
    graph_request(token, "POST", url, Some(body))
}

fn graph_patch(token: &str, url: &str, body: &Value) -> Result<Value, String> {
    graph_request(token, "PATCH", url, Some(body))
}

fn graph_delete(token: &str, url: &str) -> Result<Value, String> {
    graph_request(token, "DELETE", url, None)
}

fn graph_get(token: &str, url: &str) -> Result<Value, String> {
    graph_request(token, "GET", url, None)
}

fn graph_request(
    token: &str,
    method: &str,
    url: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let mut headers = vec![("Authorization".into(), format!("Bearer {token}"))];
    let (body_vec, _needs_content) = if let Some(value) = body {
        let bytes = serde_json::to_vec(value).map_err(|e| format!("invalid graph body: {e}"))?;
        headers.push(("Content-Type".into(), "application/json".into()));
        (Some(bytes), true)
    } else {
        (None, false)
    };
    let request = client::Request {
        method: method.into(),
        url: url.to_string(),
        headers,
        body: body_vec,
    };
    let resp = client::send(&request, None, None)
        .map_err(|e| format!("graph request error: {}", e.message))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(format!("graph request returned {}", resp.status));
    }
    let body = match resp.body {
        Some(body) if !body.is_empty() => body,
        _ => return Ok(Value::Null),
    };
    serde_json::from_slice(&body).map_err(|e| format!("graph response decode failed: {e}"))
}

fn handle_validation(http: &HttpInV1) -> Vec<u8> {
    let token = http
        .query
        .as_deref()
        .and_then(|query| query_param_value(query, "validationToken"))
        .unwrap_or_default();
    if token.is_empty() {
        return http_out_error(400, "validationToken missing");
    }
    let headers = vec![Header {
        name: "Content-Type".into(),
        value: "text/plain".into(),
    }];
    let out = HttpOutV1 {
        status: 200,
        headers,
        body_b64: STANDARD.encode(token.as_bytes()),
        events: Vec::new(),
    };
    json_bytes(&out)
}

fn handle_graph_notifications(http: &HttpInV1) -> Vec<u8> {
    let config_value = match http.config.as_ref() {
        Some(cfg) => cfg,
        None => return http_out_error(400, "config required for ingest"),
    };
    let cfg = match parse_config_value(config_value) {
        Ok(cfg) => cfg,
        Err(err) => return http_out_error(400, &err),
    };
    let user = match binding_to_user(http.binding_id.as_ref()) {
        Ok(value) => value,
        Err(err) => return http_out_error(400, &err),
    };
    let token = match auth::acquire_graph_token(&cfg, &user) {
        Ok(value) => value,
        Err(err) => return http_out_error(500, &err),
    };
    let notifications = match parse_graph_notifications(&http.body_b64) {
        Ok(value) => value,
        Err(err) => return http_out_error(400, &err),
    };
    let mut events = Vec::new();
    for (resource, message_id) in notifications {
        match fetch_graph_message(&token, &cfg, &message_id) {
            Ok(message) => {
                events.push(channel_message_envelope(
                    &message,
                    &user,
                    &message_id,
                    &resource,
                ));
            }
            Err(err) => return http_out_error(500, &err),
        }
    }
    let out = HttpOutV1 {
        status: 200,
        headers: Vec::new(),
        body_b64: String::new(),
        events,
    };
    json_bytes(&out)
}

fn query_param_value(query: &str, key: &str) -> Option<String> {
    for part in query.split('&') {
        let mut kv = part.splitn(2, '=');
        if let Some(k) = kv.next()
            && k == key
            && let Some(v) = kv.next()
        {
            return url_decode(v).ok().map(|cow| cow.into_owned());
        }
    }
    None
}

fn binding_to_user(binding: Option<&String>) -> Result<AuthUserRefV1, String> {
    let binding = binding.ok_or_else(|| "binding_id required".to_string())?;
    let parts: Vec<&str> = binding.splitn(2, '|').collect();
    let (user_id, token_key) = if parts.len() == 2 {
        (parts[0], parts[1])
    } else {
        (binding.as_str(), binding.as_str())
    };
    Ok(AuthUserRefV1 {
        user_id: user_id.to_string(),
        token_key: token_key.to_string(),
        tenant_id: None,
        email: None,
        display_name: None,
    })
}

fn parse_graph_notifications(body_b64: &str) -> Result<Vec<(String, String)>, String> {
    let bytes = STANDARD
        .decode(body_b64)
        .map_err(|err| format!("invalid notification body: {err}"))?;
    let json: Value = serde_json::from_slice(&bytes)
        .map_err(|err| format!("notification decode failed: {err}"))?;
    let entries = json
        .get("value")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing notification value array".to_string())?;
    let mut parsed = Vec::new();
    for entry in entries {
        let resource = entry
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let message_id = entry
            .get("resourceData")
            .and_then(|rd| rd.get("id"))
            .and_then(Value::as_str)
            .or_else(|| {
                entry
                    .get("resourceData")
                    .and_then(|rd| rd.get("@odata.id"))
                    .and_then(Value::as_str)
            })
            .ok_or_else(|| "notification missing resourceData.id".to_string())?
            .to_string();
        parsed.push((resource, message_id));
    }
    Ok(parsed)
}

fn fetch_graph_message(
    token: &str,
    cfg: &ProviderConfig,
    message_id: &str,
) -> Result<Value, String> {
    let base = graph_base_url(cfg);
    let url = format!(
        "{}/me/messages/{}?$select=subject,bodyPreview,receivedDateTime,from,toRecipients,webLink,internetMessageId",
        base, message_id
    );
    graph_get(token, &url)
}

fn channel_message_envelope(
    message: &Value,
    user: &AuthUserRefV1,
    message_id: &str,
    resource: &str,
) -> ChannelMessageEnvelope {
    let subject = message
        .get("subject")
        .and_then(Value::as_str)
        .unwrap_or("email message")
        .to_string();
    let preview = message
        .get("bodyPreview")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let received = message
        .get("receivedDateTime")
        .and_then(Value::as_str)
        .unwrap_or("");
    let from_address = message
        .get("from")
        .and_then(|from| from.get("emailAddress"))
        .and_then(|ea| ea.get("address"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut metadata = MessageMetadata::new();
    metadata.insert("graph_message_id".to_string(), message_id.to_string());
    metadata.insert("subject".to_string(), subject.clone());
    if !preview.is_empty() {
        metadata.insert("body_preview".to_string(), preview);
    }
    if !received.is_empty() {
        metadata.insert("receivedDateTime".to_string(), received.to_string());
    }
    if !from_address.is_empty() {
        metadata.insert("from".to_string(), from_address.to_string());
    }
    metadata.insert("resource".to_string(), resource.to_string());
    let env = default_env();
    let tenant = default_tenant();
    ChannelMessageEnvelope {
        id: format!("email-{message_id}"),
        tenant: TenantCtx::new(env, tenant),
        channel: "email".to_string(),
        session_id: message_id.to_string(),
        reply_scope: None,
        from: Some(Actor {
            id: user.user_id.clone(),
            kind: Some("user".into()),
        }),
        to: Vec::new(),
        correlation_id: Some(resource.to_string()),
        text: Some(subject),
        attachments: Vec::new(),
        metadata,
    }
}

fn default_env() -> EnvId {
    EnvId::try_from("default").expect("default env id present")
}

fn default_tenant() -> TenantId {
    TenantId::try_from("default").expect("default tenant id present")
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
        "host",
        "port",
        "username",
        "from_address",
        "default_to_address",
        "tls_mode",
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

fn build_channel_envelope(parsed: &Value, cfg: &ProviderConfig) -> ChannelMessageEnvelope {
    let to_addr = parsed
        .get("to")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            cfg.default_to_address
                .clone()
                .unwrap_or_else(|| "recipient@example.com".to_string())
        });
    let subject = parsed
        .get("subject")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "universal subject".to_string());
    let body_text = parsed
        .get("body")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut metadata = MessageMetadata::new();
    metadata.insert("to".to_string(), to_addr.clone());
    metadata.insert("subject".to_string(), subject.clone());
    ChannelMessageEnvelope {
        id: "synthetic-envelope".to_string(),
        tenant: TenantCtx::new(default_env(), default_tenant()),
        channel: PROVIDER_TYPE.to_string(),
        session_id: "synthetic-session".to_string(),
        reply_scope: None,
        from: None,
        to: vec![Destination {
            id: to_addr,
            kind: Some("email".to_string()),
        }],
        correlation_id: None,
        text: body_text,
        attachments: Vec::new(),
        metadata,
    }
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
    fn validate_accepts_defaults() {
        let cfg =
            br#"{"host":"smtp.example.com","username":"u","from_address":"from@example.com"}"#;
        let resp = Component::validate_config(cfg.to_vec());
        let json: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(json.get("ok"), Some(&Value::Bool(true)));
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"host":"smtp","username":"u","from_address":"f","unknown":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }

    #[test]
    fn load_config_prefers_nested() {
        let input = json!({
            "config": {"host":"a","port":25,"username":"u","from_address":"f"},
            "host": "b"
        });
        let cfg = load_config(&input).unwrap();
        assert_eq!(cfg.host, "a");
        assert_eq!(cfg.port, 25);
    }
}
