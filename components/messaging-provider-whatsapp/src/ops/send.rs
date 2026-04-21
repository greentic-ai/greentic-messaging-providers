use greentic_types::{
    ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};
use provider_common::helpers::json_bytes;
use serde_json::{Value, json};

use crate::bindings::greentic::http::http_client as client;
use crate::config::{get_token, load_config};
use crate::{DEFAULT_API_BASE, DEFAULT_API_VERSION, PROVIDER_TYPE};

pub(crate) fn handle_send(input_json: &[u8]) -> Vec<u8> {
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
    if !cfg.enabled {
        return json_bytes(&json!({"ok": false, "error": "provider disabled by config"}));
    }

    let envelope: ChannelMessageEnvelope = match serde_json::from_slice(input_json) {
        Ok(env) => env,
        Err(err) => match build_send_envelope_from_input(&parsed) {
            Ok(env) => env,
            Err(message) => {
                return json_bytes(
                    &json!({"ok": false, "error": format!("invalid envelope: {message}: {err}")}),
                );
            }
        },
    };

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

    let destination = envelope.to.first().cloned();
    let destination = match destination {
        Some(dest) => dest,
        None => return json_bytes(&json!({"ok": false, "error": "destination required"})),
    };

    let dest_id = destination.id.trim();
    if dest_id.is_empty() {
        return json_bytes(&json!({"ok": false, "error": "destination id required"}));
    }
    let kind = destination.kind.as_deref().unwrap_or("phone");
    if kind != "phone" {
        return json_bytes(&json!({
            "ok": false,
            "error": format!("unsupported destination kind: {kind}"),
        }));
    }

    let token = match get_token(&cfg) {
        Ok(token) => token,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let api_base = cfg
        .api_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let api_version = cfg
        .api_version
        .clone()
        .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());
    let url = format!(
        "{}/{}/{}/messages",
        api_base, api_version, cfg.phone_number_id
    );

    // Check for WhatsApp-specific rich content from AC conversion.
    let wa_buttons: Vec<Value> = parsed
        .get("wa_buttons")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let wa_image = parsed
        .get("wa_image")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let wa_header = parsed
        .get("wa_header")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let wa_video = parsed.get("wa_video").and_then(Value::as_str);
    let wa_video_caption = parsed.get("wa_video_caption").and_then(Value::as_str);
    let wa_audio = parsed.get("wa_audio").and_then(Value::as_str);
    let wa_document = parsed.get("wa_document").and_then(Value::as_str);
    let wa_document_filename = parsed.get("wa_document_filename").and_then(Value::as_str);
    let wa_document_caption = parsed.get("wa_document_caption").and_then(Value::as_str);
    let wa_sticker = parsed.get("wa_sticker").and_then(Value::as_str);
    let wa_location = parsed.get("wa_location");

    // Send media messages before the main text/interactive message.
    // Each media type is sent as a separate API call (WhatsApp Cloud API pattern).
    let mut media_results: Vec<Value> = Vec::new();

    if let Some(video_url) = wa_video {
        let mut video = json!({ "link": video_url });
        if let Some(cap) = wa_video_caption {
            let cap: String = cap.chars().take(1024).collect();
            video
                .as_object_mut()
                .unwrap()
                .insert("caption".into(), json!(cap));
        }
        let r = send_media_message(
            &url,
            &token,
            dest_id,
            &json!({
                "messaging_product": "whatsapp", "to": dest_id,
                "type": "video", "video": video
            }),
        );
        media_results.push(json!({"type": "video", "ok": r.is_ok(), "detail": format!("{r:?}")}));
    }
    if let Some(audio_url) = wa_audio {
        let r = send_media_message(
            &url,
            &token,
            dest_id,
            &json!({
                "messaging_product": "whatsapp", "to": dest_id,
                "type": "audio", "audio": { "link": audio_url }
            }),
        );
        media_results.push(json!({"type": "audio", "ok": r.is_ok(), "detail": format!("{r:?}")}));
    }
    if let Some(doc_url) = wa_document {
        let mut doc = json!({ "link": doc_url });
        if let Some(fname) = wa_document_filename {
            doc.as_object_mut()
                .unwrap()
                .insert("filename".into(), json!(fname));
        }
        if let Some(cap) = wa_document_caption {
            let cap: String = cap.chars().take(1024).collect();
            doc.as_object_mut()
                .unwrap()
                .insert("caption".into(), json!(cap));
        }
        let r = send_media_message(
            &url,
            &token,
            dest_id,
            &json!({
                "messaging_product": "whatsapp", "to": dest_id,
                "type": "document", "document": doc
            }),
        );
        media_results
            .push(json!({"type": "document", "ok": r.is_ok(), "detail": format!("{r:?}")}));
    }
    if let Some(ref image_url) = wa_image {
        let caption: String = text.chars().take(1024).collect();
        let r = send_media_message(
            &url,
            &token,
            dest_id,
            &json!({
                "messaging_product": "whatsapp", "to": dest_id,
                "type": "image", "image": { "link": image_url, "caption": caption }
            }),
        );
        media_results.push(json!({"type": "image", "ok": r.is_ok(), "detail": format!("{r:?}")}));
    }
    if let Some(sticker_url) = wa_sticker {
        let r = send_media_message(
            &url,
            &token,
            dest_id,
            &json!({
                "messaging_product": "whatsapp", "to": dest_id,
                "type": "sticker", "sticker": { "link": sticker_url }
            }),
        );
        media_results.push(json!({"type": "sticker", "ok": r.is_ok(), "detail": format!("{r:?}")}));
    }
    if let Some(loc) = wa_location
        && loc.get("latitude").is_some()
        && loc.get("longitude").is_some()
    {
        let r = send_media_message(
            &url,
            &token,
            dest_id,
            &json!({
                "messaging_product": "whatsapp", "to": dest_id,
                "type": "location", "location": loc
            }),
        );
        media_results
            .push(json!({"type": "location", "ok": r.is_ok(), "detail": format!("{r:?}")}));
    }

    // Build the main message payload.
    let payload = if !wa_buttons.is_empty() {
        // Interactive message with reply buttons (max 3).
        let buttons: Vec<Value> = wa_buttons
            .into_iter()
            .take(3)
            .enumerate()
            .map(|(i, btn)| {
                let title = btn.get("title").and_then(Value::as_str).unwrap_or("Button");
                let truncated: String = title.chars().take(20).collect();
                json!({
                    "type": "reply",
                    "reply": { "id": format!("btn_{i}"), "title": truncated }
                })
            })
            .collect();
        let body_text: String = text.chars().take(1024).collect();
        let mut interactive = json!({
            "type": "button",
            "body": { "text": body_text },
            "action": { "buttons": buttons }
        });
        if let Some(header) = wa_header {
            let h: String = header.chars().take(60).collect();
            interactive
                .as_object_mut()
                .unwrap()
                .insert("header".into(), json!({ "type": "text", "text": h }));
        }
        json!({
            "messaging_product": "whatsapp",
            "to": dest_id,
            "type": "interactive",
            "interactive": interactive
        })
    } else if wa_image.is_some() {
        // Image already sent above — skip text-only if no additional content.
        // But if there are facts/columns beyond the image caption, send text too.
        // We always send text as fallback after image.
        json!({
            "messaging_product": "whatsapp",
            "to": dest_id,
            "type": "text",
            "text": {"body": text},
        })
    } else {
        json!({
            "messaging_product": "whatsapp",
            "to": dest_id,
            "type": "text",
            "text": {"body": text},
        })
    };

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

    let mut result = json!({
        "ok": true,
        "status": "sent",
        "provider_type": PROVIDER_TYPE,
        "public_base_url": cfg.public_base_url,
        "message_id": msg_id,
        "provider_message_id": provider_message_id,
        "response": body_json
    });
    if !media_results.is_empty() {
        result
            .as_object_mut()
            .unwrap()
            .insert("media".into(), json!(media_results));
    }
    json_bytes(&result)
}

fn send_media_message(
    api_url: &str,
    token: &str,
    _dest_id: &str,
    payload: &Value,
) -> Result<Value, String> {
    let req = client::Request {
        method: "POST".into(),
        url: api_url.to_string(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: Some(serde_json::to_vec(payload).unwrap_or_default()),
    };
    match client::send(&req, None, None) {
        Ok(resp) => {
            let body = resp.body.unwrap_or_default();
            let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            if resp.status >= 200 && resp.status < 300 {
                Ok(body_json)
            } else {
                Err(format!("media send status {}: {}", resp.status, body_json))
            }
        }
        Err(err) => Err(format!("media transport error: {}", err.message)),
    }
}

fn build_send_envelope_from_input(parsed: &Value) -> Result<ChannelMessageEnvelope, String> {
    let text = parsed
        .get("text")
        .and_then(Value::as_str)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| "text required".to_string())?;
    let destination =
        parse_send_destination(parsed).ok_or_else(|| "destination required".to_string())?;
    let env = EnvId::try_from("manual").expect("manual env id");
    let tenant = TenantId::try_from("manual").expect("manual tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("synthetic".to_string(), "true".to_string());
    if let Some(kind) = destination.kind.as_ref() {
        metadata.insert("destination_kind".to_string(), kind.clone());
    }
    let channel = destination.id.clone();
    Ok(ChannelMessageEnvelope {
        id: format!("whatsapp-manual-{channel}"),
        tenant: TenantCtx::new(env, tenant),
        channel: channel.clone(),
        session_id: channel,
        reply_scope: None,
        from: None,
        to: vec![destination],
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
        extensions: Default::default(),
    })
}

fn parse_send_destination(parsed: &Value) -> Option<Destination> {
    let to_value = parsed.get("to")?;
    if let Some(id) = to_value.as_str() {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(Destination {
            id: trimmed.to_string(),
            kind: Some("phone".to_string()),
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
        .map(|s| s.trim().to_string());
    let kind = match kind.as_deref() {
        Some("user") => Some("phone".to_string()),
        Some(kind_str) if !kind_str.is_empty() => Some(kind_str.to_string()),
        _ => Some("phone".to_string()),
    };
    id.map(|id| Destination { id, kind })
}
