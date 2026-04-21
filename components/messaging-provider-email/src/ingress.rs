use base64::{Engine, engine::general_purpose::STANDARD};
use greentic_types::messaging::universal_dto::{AuthUserRefV1, Header, HttpInV1, HttpOutV1};
use provider_common::http_compat::{
    http_out_error, http_out_v1_bytes, parse_operator_http_in_with_config,
};
use serde_json::Value;
use urlencoding::decode as url_decode;

use crate::auth;
use crate::config::{ProviderConfig, parse_config_value};
use crate::graph::{graph_base_url, graph_get};
use greentic_types::{
    Actor, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};

pub(crate) fn ingest_http(input_json: &[u8]) -> Vec<u8> {
    // Try native greentic-types format first, fall back to operator format
    let http = match serde_json::from_slice::<HttpInV1>(input_json) {
        Ok(value) => value,
        Err(_) => match parse_operator_http_in_with_config(input_json) {
            Ok(req) => req,
            Err(err) => return http_out_error(400, &format!("invalid http input: {err}")),
        },
    };
    match http.method.to_uppercase().as_str() {
        "GET" => handle_validation(&http),
        "POST" => handle_graph_notifications(&http),
        _ => http_out_error(405, "method not allowed"),
    }
}

pub(crate) fn handle_validation(http: &HttpInV1) -> Vec<u8> {
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
    http_out_v1_bytes(&out)
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
    http_out_v1_bytes(&out)
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

pub(crate) fn binding_to_user(binding: Option<&String>) -> Result<AuthUserRefV1, String> {
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
        metadata.insert("to".to_string(), from_address.to_string());
    }
    metadata.insert("resource".to_string(), resource.to_string());
    let env = default_env();
    let tenant = default_tenant();
    let destinations = if !from_address.is_empty() {
        vec![Destination {
            id: from_address.to_string(),
            kind: Some("email".into()),
        }]
    } else {
        Vec::new()
    };
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
        to: destinations,
        correlation_id: Some(resource.to_string()),
        text: Some(subject),
        attachments: Vec::new(),
        metadata,
        extensions: Default::default(),
    }
}

fn default_env() -> EnvId {
    EnvId::try_from("default").expect("default env id present")
}

fn default_tenant() -> TenantId {
    TenantId::try_from("default").expect("default tenant id present")
}
