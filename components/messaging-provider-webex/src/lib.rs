use base64::{Engine, engine::general_purpose::STANDARD};
use greentic_types::{
    Actor, Attachment, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx,
    TenantId,
};
use messaging_universal_dto::{
    EncodeInV1, HttpInV1, HttpOutV1, ProviderPayloadV1, RenderPlanInV1, RenderPlanOutV1,
    SendPayloadInV1, SendPayloadResultV1,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-webex",
        world: "messaging-provider-webex",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use bindings::greentic::http::client;
use bindings::greentic::secrets_store::secrets_store;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.webex.bot";
const CONFIG_SCHEMA_REF: &str = "schemas/messaging/webex/public.config.schema.json";
const DEFAULT_API_BASE: &str = "https://webexapis.com/v1";
const DEFAULT_TOKEN_KEY: &str = "WEBEX_BOT_TOKEN";

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    #[serde(default)]
    default_room_id: Option<String>,
    #[serde(default)]
    default_to_person_email: Option<String>,
    #[serde(default)]
    api_base_url: Option<String>,
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
                    "default_room_id": cfg.default_room_id,
                    "default_to_person_email": cfg.default_to_person_email,
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
            "send" => handle_send(&input_json),
            "reply" => handle_reply(&input_json),
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

    let mut cfg = match load_config(&parsed) {
        Ok(cfg) => cfg,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let envelope: ChannelMessageEnvelope = match serde_json::from_slice(input_json) {
        Ok(env) => env,
        Err(err) => match build_send_envelope_from_input(&parsed, &cfg) {
            Ok(env) => env,
            Err(message) => {
                return json_bytes(
                    &json!({"ok": false, "error": format!("invalid envelope: {message}: {err}")}),
                );
            }
        },
    };

    override_config_from_metadata(&mut cfg, &envelope.metadata);

    println!(
        "webex encoded envelope {}",
        serde_json::to_string(&envelope).unwrap_or_default()
    );
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

    let destination = envelope.to.first().cloned().or_else(|| {
        cfg.default_to_person_email
            .clone()
            .map(|email| Destination {
                id: email,
                kind: Some("email".into()),
            })
    });
    println!("webex envelope to={:?}", envelope.to);
    let destination = match destination {
        Some(dest) => dest,
        None => return json_bytes(&json!({"ok": false, "error": "destination required"})),
    };

    let dest_id = destination.id.trim();
    if dest_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "destination id required"}));
    }
    let dest_id = dest_id.to_string();
    let kind = destination.kind.as_deref().unwrap_or("email");

    let api_base = cfg
        .api_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{}/messages", api_base);
    let mut body = json!({ "text": text });
    let body_obj = body.as_object_mut().expect("body object");
    match kind {
        "room" => {
            body_obj.insert("roomId".into(), Value::String(dest_id));
        }
        "person" | "user" => {
            body_obj.insert("toPersonId".into(), Value::String(dest_id));
        }
        "email" | "" => {
            body_obj.insert("toPersonEmail".into(), Value::String(dest_id));
        }
        other => {
            return json_bytes(&json!({
                "ok": false,
                "error": format!("unsupported destination kind: {other}")
            }));
        }
    }

    let token = match secrets_store::get(DEFAULT_TOKEN_KEY) {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return json_bytes(&json!({"ok": false, "error": "access_token not utf-8"})),
        },
        Ok(None) => {
            return json_bytes(
                &json!({"ok": false, "error": format!("missing secret: {}", DEFAULT_TOKEN_KEY)}),
            );
        }
        Err(e) => {
            return json_bytes(
                &json!({"ok": false, "error": format!("secret store error: {e:?}")}),
            );
        }
    };

    println!(
        "webex send url={} body={}",
        url,
        serde_json::to_string(&body).unwrap_or_default()
    );
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
            return json_bytes(
                &json!({"ok": false, "error": format!("transport error: {}", err.message)}),
            );
        }
    };

    if resp.status < 200 || resp.status >= 300 {
        return json_bytes(
            &json!({"ok": false, "error": format!("webex returned status {}", resp.status)}),
        );
    }

    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let msg_id = body_json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("webex-message")
        .to_string();
    let provider_message_id = format!("webex:{msg_id}");

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

    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "text required"}));
    }
    let thread_id = parsed
        .get("reply_to_id")
        .or_else(|| parsed.get("thread_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if thread_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "reply_to_id or thread_id required"}));
    }

    let token = match secrets_store::get(DEFAULT_TOKEN_KEY) {
        Ok(Some(bytes)) => String::from_utf8(bytes).unwrap_or_default(),
        _ => return json_bytes(&json!({"ok": false, "error": "missing access token"})),
    };
    if token.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "access token empty"}));
    }
    let api_base = cfg
        .api_base_url
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{}/messages", api_base);
    let payload = json!({
        "parentId": thread_id,
        "markdown": text,
    });
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
            return json_bytes(&json!({
                "ok": false,
                "error": format!("transport error: {}", err.message),
            }));
        }
    };
    if resp.status < 200 || resp.status >= 300 {
        return json_bytes(&json!({
            "ok": false,
            "error": format!("webex returned status {}", resp.status),
        }));
    }
    let body_bytes = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let msg_id = body_json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("webex-reply")
        .to_string();
    let provider_message_id = format!("webex:{msg_id}");

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
    for key in ["default_room_id", "default_to_person_email", "api_base_url"] {
        if let Some(v) = input.get(key) {
            partial.insert(key.to_string(), v.clone());
        }
    }
    if !partial.is_empty() {
        return parse_config_value(&Value::Object(partial));
    }

    Ok(ProviderConfig {
        default_room_id: None,
        default_to_person_email: None,
        api_base_url: None,
    })
}

fn override_config_from_metadata(cfg: &mut ProviderConfig, metadata: &MessageMetadata) {
    if let Some(api) = metadata.get("config.api_base_url") {
        cfg.api_base_url = Some(api.clone());
    }
    if let Some(email) = metadata.get("config.default_to_person_email") {
        cfg.default_to_person_email = Some(email.clone());
    }
}

fn build_send_envelope_from_input(
    parsed: &Value,
    cfg: &ProviderConfig,
) -> Result<ChannelMessageEnvelope, String> {
    let text = parsed
        .get("text")
        .and_then(|value| value.as_str())
        .map(|s| s.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let text = match text {
        Some(value) => value,
        None => return Err("text required".to_string()),
    };
    let destination =
        parse_send_destination(parsed, cfg).ok_or_else(|| "destination required".to_string())?;

    let env = EnvId::try_from("manual").expect("manual env id");
    let tenant = TenantId::try_from("manual").expect("manual tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("synthetic".to_string(), "true".to_string());
    if let Some(kind) = &destination.kind {
        metadata.insert("destination_kind".to_string(), kind.clone());
    }
    let channel_name = destination.id.clone();

    Ok(ChannelMessageEnvelope {
        id: format!("webex-manual-{channel_name}"),
        tenant: TenantCtx::new(env, tenant),
        channel: channel_name.clone(),
        session_id: channel_name,
        reply_scope: None,
        from: None,
        to: vec![destination],
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
    })
}

fn parse_send_destination(parsed: &Value, cfg: &ProviderConfig) -> Option<Destination> {
    if let Some(dest) = parsed_to_destination(parsed) {
        return Some(dest);
    }
    if let Some(room) = cfg.default_room_id.clone() {
        return Some(Destination {
            id: room,
            kind: Some("room".to_string()),
        });
    }
    if let Some(email) = cfg.default_to_person_email.clone() {
        return Some(Destination {
            id: email,
            kind: Some("email".to_string()),
        });
    }
    None
}

fn parsed_to_destination(parsed: &Value) -> Option<Destination> {
    let to_value = parsed.get("to")?;
    if let Some(id) = to_value.as_str() {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(Destination {
            id: trimmed.to_string(),
            kind: Some("room".to_string()),
        });
    }
    let obj = to_value.as_object()?;
    let id = obj
        .get("id")
        .and_then(|value| value.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let kind = obj
        .get("kind")
        .and_then(|value| value.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    id.map(|id| Destination { id, kind })
}

fn summarize_card_text(card: &Value) -> Option<String> {
    if let Some(text) = card
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return Some(text.to_string());
    }

    if let Some(body_array) = card.get("body").and_then(Value::as_array) {
        let mut segments = Vec::new();
        for block in body_array {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    segments.push(trimmed.to_string());
                }
            }
        }
        if !segments.is_empty() {
            return Some(segments.join(" "));
        }
    }

    None
}

fn build_webex_body(
    card_payload: Option<&Value>,
    text_value: Option<&String>,
    markdown: &str,
) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    if let Some(card) = card_payload {
        let attachment = json!({
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": card,
        });
        map.insert("attachments".into(), Value::Array(vec![attachment]));
    } else if let Some(text_val) = text_value {
        map.insert("text".into(), Value::String(text_val.clone()));
    }
    map.insert("markdown".into(), Value::String(markdown.to_string()));
    map
}

fn format_webex_error(status: u16, body: &[u8]) -> String {
    let trimmed = String::from_utf8_lossy(body).trim().to_string();
    if trimmed.is_empty() {
        format!("webex returned status {}", status)
    } else {
        format!("webex returned status {} body={}", status, trimmed)
    }
}

fn json_bytes<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec())
}

struct IngestOutcome {
    envelope: ChannelMessageEnvelope,
    status: u16,
    error: Option<String>,
}

struct MessageDetails {
    markdown: Option<String>,
    text: Option<String>,
    room_id: Option<String>,
    person_email: Option<String>,
    person_id: Option<String>,
    attachments: Vec<Attachment>,
}

fn handle_webhook_event(body: &Value, cfg: &ProviderConfig) -> IngestOutcome {
    let resource = body
        .get("resource")
        .and_then(|s| s.as_str())
        .unwrap_or_default();
    let event = body
        .get("event")
        .and_then(|s| s.as_str())
        .unwrap_or_default();
    let data = body.get("data").unwrap_or(&Value::Null);
    let message_id = data
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let webhook_room = data
        .get("roomId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let webhook_person_email = data
        .get("personEmail")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let webhook_person_id = data
        .get("personId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if resource == "messages"
        && event == "created"
        && let Some(message_id) = message_id.clone()
    {
        let api_base = cfg
            .api_base_url
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(DEFAULT_API_BASE)
            .trim_end_matches('/')
            .to_string();
        match get_secret_string(DEFAULT_TOKEN_KEY) {
            Ok(token) => match fetch_message_details(&message_id, &api_base, &token) {
                Ok(details) => {
                    let session_id = details
                        .room_id
                        .clone()
                        .or(webhook_room.clone())
                        .unwrap_or_else(|| message_id.clone());
                    let sender = pick_sender(&details.person_email, &details.person_id)
                        .or_else(|| pick_sender(&webhook_person_email, &webhook_person_id));
                    let text = details
                        .markdown
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .map(ToOwned::to_owned)
                        .or_else(|| details.text.clone())
                        .unwrap_or_default();
                    let attachment_types = if details.attachments.is_empty() {
                        None
                    } else {
                        Some(
                            details
                                .attachments
                                .iter()
                                .map(|a| a.mime_type.clone())
                                .collect::<Vec<_>>()
                                .join(","),
                        )
                    };
                    let metadata = build_webhook_metadata(
                        resource,
                        event,
                        Some(&message_id),
                        details.room_id.as_ref().or(webhook_room.as_ref()),
                        details
                            .person_email
                            .as_ref()
                            .or(webhook_person_email.as_ref()),
                        details.person_id.as_ref().or(webhook_person_id.as_ref()),
                        None,
                        attachment_types.clone(),
                        Some(200),
                    );
                    let envelope = build_webhook_envelope(
                        text,
                        session_id,
                        sender,
                        metadata,
                        details.attachments.clone(),
                        Some(&message_id),
                    );
                    return IngestOutcome {
                        envelope,
                        status: 200,
                        error: None,
                    };
                }
                Err(err) => {
                    println!("webex ingest fetch error for {message_id}: {err}");
                    let session_id = webhook_room.clone().unwrap_or_else(|| message_id.clone());
                    let sender = pick_sender(&webhook_person_email, &webhook_person_id);
                    let metadata = build_webhook_metadata(
                        resource,
                        event,
                        Some(&message_id),
                        webhook_room.as_ref(),
                        webhook_person_email.as_ref(),
                        webhook_person_id.as_ref(),
                        Some(&err),
                        None,
                        Some(502),
                    );
                    let envelope = build_webhook_envelope(
                        "".to_string(),
                        session_id,
                        sender,
                        metadata,
                        Vec::new(),
                        Some(&message_id),
                    );
                    return IngestOutcome {
                        envelope,
                        status: 502,
                        error: Some(err),
                    };
                }
            },
            Err(err) => {
                let session_id = webhook_room.clone().unwrap_or_else(|| message_id.clone());
                let sender = pick_sender(&webhook_person_email, &webhook_person_id);
                let metadata = build_webhook_metadata(
                    resource,
                    event,
                    Some(&message_id),
                    webhook_room.as_ref(),
                    webhook_person_email.as_ref(),
                    webhook_person_id.as_ref(),
                    Some(&err),
                    None,
                    Some(500),
                );
                let envelope = build_webhook_envelope(
                    "".to_string(),
                    session_id,
                    sender,
                    metadata,
                    Vec::new(),
                    Some(&message_id),
                );
                return IngestOutcome {
                    envelope,
                    status: 500,
                    error: Some(err),
                };
            }
        }
    }

    let text = body
        .get("text")
        .or_else(|| body.get("markdown"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let session_id = webhook_room
        .clone()
        .unwrap_or_else(|| message_id.clone().unwrap_or_else(|| "webex".to_string()));
    let sender = pick_sender(&webhook_person_email, &webhook_person_id);
    let metadata = build_webhook_metadata(
        resource,
        event,
        message_id.as_ref(),
        webhook_room.as_ref(),
        webhook_person_email.as_ref(),
        webhook_person_id.as_ref(),
        None,
        None,
        Some(200),
    );
    let envelope = build_webhook_envelope(
        text,
        session_id,
        sender,
        metadata,
        Vec::new(),
        message_id.as_ref(),
    );
    IngestOutcome {
        envelope,
        status: 200,
        error: None,
    }
}

fn fetch_message_details(
    message_id: &str,
    api_base: &str,
    token: &str,
) -> Result<MessageDetails, String> {
    let url = format!("{api_base}/messages/{message_id}");
    println!("webex ingest fetching message {message_id} from {url}");
    let request = client::Request {
        method: "GET".to_string(),
        url: url.clone(),
        headers: vec![("Authorization".into(), format!("Bearer {token}"))],
        body: None,
    };
    let resp = client::send(&request, None, None)
        .map_err(|err| format!("transport error: {}", err.message))?;
    println!("webex ingest fetch {message_id} status={}", resp.status);
    if resp.status < 200 || resp.status >= 300 {
        let body = resp.body.unwrap_or_default();
        return Err(format_webex_error(resp.status, &body));
    }
    let body = resp.body.unwrap_or_default();
    let message_json: Value =
        serde_json::from_slice(&body).map_err(|err| format!("invalid message JSON: {err}"))?;
    let data = message_json
        .get("result")
        .cloned()
        .unwrap_or_else(|| message_json.clone());
    let attachments = convert_webex_attachments(message_id, &data);
    Ok(MessageDetails {
        markdown: data
            .get("markdown")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        text: data
            .get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        room_id: data
            .get("roomId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        person_email: data
            .get("personEmail")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        person_id: data
            .get("personId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        attachments,
    })
}

fn convert_webex_attachments(message_id: &str, data: &Value) -> Vec<Attachment> {
    data.get("attachments")
        .and_then(Value::as_array)
        .map(|array| {
            array
                .iter()
                .enumerate()
                .filter_map(|(idx, attachment)| build_webex_attachment(message_id, idx, attachment))
                .collect()
        })
        .unwrap_or_default()
}

fn build_webex_attachment(message_id: &str, idx: usize, value: &Value) -> Option<Attachment> {
    let mime_type = value
        .get("contentType")
        .and_then(|v| v.as_str())
        .unwrap_or("application/octet-stream")
        .to_string();
    let url = value
        .get("contentUrl")
        .and_then(|v| v.as_str())
        .or_else(|| {
            value
                .get("content")
                .and_then(|content| content.get("url"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("webex:{message_id}:attachment:{idx}"));
    let name = value
        .get("name")
        .or_else(|| value.get("displayName"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let size_bytes = value
        .get("size")
        .and_then(|v| v.as_u64())
        .or_else(|| value.get("sizeBytes").and_then(|v| v.as_u64()));
    Some(Attachment {
        mime_type,
        url,
        name,
        size_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_webhook_metadata(
    resource: &str,
    event: &str,
    message_id: Option<&String>,
    room_id: Option<&String>,
    person_email: Option<&String>,
    person_id: Option<&String>,
    error: Option<&String>,
    attachment_types: Option<String>,
    status: Option<u16>,
) -> MessageMetadata {
    let mut metadata = MessageMetadata::new();
    metadata.insert("webex.resource".to_string(), resource.to_string());
    metadata.insert("webex.event".to_string(), event.to_string());
    if let Some(msg) = message_id {
        metadata.insert("webex.messageId".to_string(), msg.clone());
    }
    if let Some(room) = room_id {
        metadata.insert("webex.roomId".to_string(), room.clone());
    }
    if let Some(email) = person_email {
        metadata.insert("webex.personEmail".to_string(), email.clone());
    }
    if let Some(id) = person_id {
        metadata.insert("webex.personId".to_string(), id.clone());
    }
    if let Some(err) = error {
        metadata.insert("webex.ingestError".to_string(), err.clone());
    }
    if let Some(status) = status {
        metadata.insert("webex.fetchStatus".to_string(), status.to_string());
    }
    metadata.insert(
        "webex.hasAttachments".to_string(),
        attachment_types.is_some().to_string(),
    );
    if let Some(types) = attachment_types {
        metadata.insert("webex.attachmentTypes".to_string(), types);
    }
    metadata
}

fn build_webhook_envelope(
    text: String,
    session_id: String,
    from: Option<Actor>,
    metadata: MessageMetadata,
    attachments: Vec<Attachment>,
    message_id: Option<&String>,
) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    ChannelMessageEnvelope {
        id: message_id
            .map(|id| format!("webex-{id}"))
            .unwrap_or_else(|| format!("webex-ingress-{session_id}")),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: "webex".to_string(),
        session_id: session_id.clone(),
        reply_scope: None,
        from,
        to: Vec::new(),
        correlation_id: None,
        text: Some(text),
        attachments,
        metadata,
    }
}

fn pick_sender(person_email: &Option<String>, person_id: &Option<String>) -> Option<Actor> {
    if let Some(email) = person_email {
        return Some(Actor {
            id: email.clone(),
            kind: Some("person".into()),
        });
    }
    if let Some(id) = person_id {
        return Some(Actor {
            id: id.clone(),
            kind: Some("person".into()),
        });
    }
    None
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
    let cfg = load_config(&json!({})).unwrap_or_default();
    let outcome = handle_webhook_event(&body_val, &cfg);

    let mut normalized = json!({
        "ok": outcome.error.is_none(),
        "event": body_val,
    });
    if let Some(err) = &outcome.error {
        normalized
            .as_object_mut()
            .map(|map| map.insert("error".into(), Value::String(err.clone())));
    }

    let normalized_bytes = serde_json::to_vec(&normalized).unwrap_or_else(|_| b"{}".to_vec());
    let out = HttpOutV1 {
        status: outcome.status,
        headers: Vec::new(),
        body_b64: STANDARD.encode(&normalized_bytes),
        events: vec![outcome.envelope],
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
        .unwrap_or_else(|| "webex message".to_string());
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
    let envelope = encode_in.message;
    let body_bytes = serde_json::to_vec(&envelope).unwrap_or_else(|_| b"{}".to_vec());
    let payload = ProviderPayloadV1 {
        content_type: "application/json".to_string(),
        body_b64: STANDARD.encode(&body_bytes),
        metadata: HashMap::new(),
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
    let api_base = metadata
        .get("api_base_url")
        .and_then(|value| value.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let url = format!("{}/messages", api_base);
    let method = metadata
        .get("method")
        .and_then(|value| value.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "POST".to_string());
    let body_bytes = match STANDARD.decode(&body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return send_payload_error(&format!("payload decode failed: {err}"), false),
    };
    let envelope = match serde_json::from_slice::<ChannelMessageEnvelope>(&body_bytes) {
        Ok(env) => env,
        Err(err) => {
            eprintln!("webex send_payload invalid envelope: {err}");
            return send_payload_error(&format!("invalid envelope: {err}"), false);
        }
    };
    if !envelope.attachments.is_empty() {
        eprintln!(
            "webex send_payload rejected attachments {:?}",
            envelope.attachments
        );
        return send_payload_error("attachments not supported", false);
    }
    let text = envelope
        .text
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let card_payload = envelope
        .metadata
        .get("adaptive_card")
        .and_then(|value| serde_json::from_str::<Value>(value).ok());
    let card_summary = card_payload.as_ref().and_then(summarize_card_text);
    if card_payload.is_none() && text.is_none() {
        eprintln!(
            "webex send_payload missing text envelope metadata={:?}",
            envelope.metadata
        );
        return send_payload_error("text required", false);
    }
    let destination = envelope.to.first().cloned().or_else(|| {
        metadata
            .get("default_to_person_email")
            .and_then(|value| value.as_str())
            .map(|s| Destination {
                id: s.to_string(),
                kind: Some("email".into()),
            })
    });
    let destination = match destination {
        Some(dest) => dest,
        None => {
            return send_payload_error(
                &format!("destination required (envelope to={:?})", envelope.to),
                false,
            );
        }
    };
    let dest_id = destination.id.trim();
    if dest_id.is_empty() {
        return send_payload_error("destination id required", false);
    }
    let summary_text = text.clone().or(card_summary.clone());
    let markdown_value = summary_text.clone().unwrap_or_else(|| " ".to_string());
    let mut body_map = build_webex_body(card_payload.as_ref(), text.as_ref(), &markdown_value);
    let kind = destination.kind.as_deref().unwrap_or("email");
    match kind {
        "room" => {
            body_map.insert("roomId".into(), Value::String(dest_id.to_string()));
        }
        "person" | "user" => {
            body_map.insert("toPersonId".into(), Value::String(dest_id.to_string()));
        }
        "email" | "" => {
            body_map.insert("toPersonEmail".into(), Value::String(dest_id.to_string()));
        }
        other => {
            return send_payload_error(&format!("unsupported destination kind: {other}"), false);
        }
    }
    let body_req = Value::Object(body_map);
    println!(
        "webex send url={}/messages body={}",
        api_base,
        serde_json::to_string(&body_req).unwrap_or_default()
    );
    let token = match get_secret_string(DEFAULT_TOKEN_KEY) {
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
        body: Some(serde_json::to_vec(&body_req).unwrap_or_else(|_| b"{}".to_vec())),
    };
    let resp = match client::send(&request, None, None) {
        Ok(value) => value,
        Err(err) => {
            return send_payload_error(&format!("transport error: {}", err.message), true);
        }
    };
    if resp.status < 200 || resp.status >= 300 {
        let body = resp.body.unwrap_or_default();
        let detail = format_webex_error(resp.status, &body);
        return send_payload_error(&detail, resp.status >= 500);
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
    fn build_webex_body_includes_markdown_and_attachment() {
        let card = json!({
            "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
            "type": "AdaptiveCard",
            "version": "1.3",
            "body": [{"type": "TextBlock", "text": "hi"}]
        });
        let mut body = build_webex_body(Some(&card), None, " ");
        body.insert("toPersonEmail".into(), Value::String("example@test".into()));
        assert_eq!(body.get("markdown"), Some(&Value::String(" ".into())));
        assert_eq!(
            body.get("toPersonEmail"),
            Some(&Value::String("example@test".into()))
        );
        let attachments = body
            .get("attachments")
            .and_then(Value::as_array)
            .expect("attachments present");
        assert_eq!(
            attachments[0]
                .get("contentType")
                .and_then(Value::as_str)
                .unwrap(),
            "application/vnd.microsoft.card.adaptive"
        );
        assert!(attachments[0].get("content").is_some());
    }

    #[test]
    fn format_webex_error_includes_body_text_when_present() {
        let msg = format_webex_error(400, br#"{"message":"bad request"}"#);
        assert!(msg.contains("webex returned status 400"));
        assert!(msg.contains(r#"{"message":"bad request"}"#));
        let empty = format_webex_error(500, b"");
        assert_eq!(empty, "webex returned status 500");
    }

    #[test]
    fn validate_accepts_defaults() {
        let cfg = br#"{"default_room_id":"room"}"#;
        let resp = Component::validate_config(cfg.to_vec());
        let json: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(json.get("ok"), Some(&Value::Bool(true)));
    }

    #[test]
    fn load_config_defaults_to_token_key() {
        let input = json!({});
        let cfg = load_config(&input).unwrap();
        assert!(cfg.default_room_id.is_none());
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"default_room_id":"k","unexpected":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }
}
