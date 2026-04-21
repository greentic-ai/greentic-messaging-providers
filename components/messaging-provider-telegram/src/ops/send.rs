//! `send`/`run` op for the Telegram provider.
//!
//! [`handle_send`] takes a host request carrying either a full
//! [`ChannelMessageEnvelope`] or a flat JSON object, assembles the Telegram
//! API calls, and dispatches media pre-sends (video/photo/doc/voice/etc.)
//! before the main text message. Callers that only want to inject a synthetic
//! envelope from flat fields (`text`, `chat_id`) go through
//! [`build_synthetic_envelope`].

use greentic_types::{
    ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};
use provider_common::helpers::json_bytes;
use serde_json::{Value, json};

use crate::bindings::greentic::http::http_client as client;
use crate::config::{ProviderConfig, get_bot_token, load_config};
use crate::{DEFAULT_API_BASE, PROVIDER_TYPE};

use super::ac_helpers::{
    build_inline_keyboard_from_metadata, first_input_placeholder, has_pending_text_inputs,
    truncate_html,
};
use super::http::{tg_send_media, tg_send_message};
use super::ingest::extract_ids;

pub(crate) fn handle_send(input_json: &[u8]) -> Vec<u8> {
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
    if !cfg.enabled {
        return json_bytes(&json!({"ok": false, "error": "provider disabled by config"}));
    }

    let envelope: ChannelMessageEnvelope = match serde_json::from_slice(input_json) {
        Ok(env) => env,
        Err(_) => match build_synthetic_envelope(&parsed, &cfg) {
            Ok(env) => env,
            Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
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

    let destination = envelope
        .to
        .first()
        .cloned()
        .or_else(|| {
            // Fallback: recover destination from metadata.chat_id
            // (preserved through operator round-trip).
            envelope
                .metadata
                .get("chat_id")
                .filter(|id| !id.is_empty())
                .map(|id| Destination {
                    id: id.clone(),
                    kind: Some("chat".into()),
                })
        })
        .or_else(|| {
            cfg.default_chat_id.clone().map(|chat| Destination {
                id: chat,
                kind: Some("chat".into()),
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
    let dest_id = dest_id.to_string();
    let kind = destination.kind.as_deref().unwrap_or("chat");
    if kind != "chat" && !kind.is_empty() {
        return json_bytes(&json!({
            "ok": false,
            "error": format!("unsupported destination kind: {kind}")
        }));
    }

    let token = match get_bot_token(&cfg) {
        Ok(s) => s,
        Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
    };

    let api_base = cfg
        .api_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());

    // Build inline keyboard from AC actions (multiple buttons supported).
    let inline_keyboard = build_inline_keyboard_from_metadata(&envelope.metadata);
    let has_text_inputs = has_pending_text_inputs(&envelope.metadata);
    let reply_markup = if has_text_inputs {
        // Card has text input fields — use ForceReply so user's next message
        // is captured as the input value. Suppress inline keyboard buttons
        // because Telegram can't show both ForceReply and inline buttons;
        // the operator will auto-submit when the user types text.
        Some(json!({
            "force_reply": true,
            "selective": true,
            "input_field_placeholder": first_input_placeholder(&envelope.metadata),
        }))
    } else if !inline_keyboard.is_empty() {
        Some(json!({ "inline_keyboard": inline_keyboard }))
    } else {
        None
    };

    // Read parse_mode from metadata (set by encode_op for AC content).
    let parse_mode = envelope.metadata.get("parse_mode").cloned();

    // ── Pre-send media messages (video, audio, document, voice, animation, sticker, photo, location) ──
    let media_results = send_media_messages(&api_base, &token, &dest_id, &parse_mode, &envelope);

    // Check for AC images — use sendPhoto if available, otherwise sendMessage.
    let images: Vec<String> = envelope
        .metadata
        .get("ac_images")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    // Build sendMessage payload (always needed as fallback).
    let text_payload = build_text_payload(&dest_id, &text, parse_mode.as_deref(), &reply_markup);

    // Choose API method based on content:
    //   1 image  → sendPhoto (photo + caption + buttons)
    //   2+ images → sendMediaGroup (album) + sendMessage (text + buttons)
    //   0 images → sendMessage (text + buttons)
    let resp = match images.len() {
        0 => match tg_send_message(&api_base, &token, &text_payload) {
            Ok(r) => r,
            Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
        },
        1 => {
            let args = SinglePhotoArgs {
                api_base: &api_base,
                token: &token,
                dest_id: &dest_id,
                photo_url: &images[0],
                text: &text,
                parse_mode: parse_mode.as_deref(),
                reply_markup: &reply_markup,
                text_payload: &text_payload,
            };
            match send_single_photo(args) {
                Ok(r) => r,
                Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
            }
        }
        _ => match send_media_group_then_text(
            &api_base,
            &token,
            &dest_id,
            &images,
            parse_mode.as_deref(),
            &text_payload,
        ) {
            Ok(r) => r,
            Err(err) => return json_bytes(&json!({"ok": false, "error": err})),
        },
    };

    if resp.status < 200 || resp.status >= 300 {
        let body = resp.body.unwrap_or_default();
        let body_str = String::from_utf8_lossy(&body);
        return json_bytes(&json!({
            "ok": false,
            "error": format!("telegram returned status {}: {}", resp.status, body_str),
        }));
    }

    let body = resp.body.unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (message_id, provider_message_id) = extract_ids(&body_json);

    let mut result = json!({
        "ok": true,
        "status": "sent",
        "provider_type": PROVIDER_TYPE,
        "public_base_url": cfg.public_base_url,
        "message_id": message_id,
        "provider_message_id": provider_message_id,
        "response": body_json
    });
    if !media_results.is_empty() {
        result["media"] = json!(
            media_results
                .iter()
                .map(|r| match r {
                    Ok(v) => json!({"ok": true, "response": v}),
                    Err(e) => json!({"ok": false, "error": e}),
                })
                .collect::<Vec<_>>()
        );
    }
    json_bytes(&result)
}

/// Fan out all pre-send media messages advertised via `tg_*` metadata fields.
fn send_media_messages(
    api_base: &str,
    token: &str,
    dest_id: &str,
    parse_mode: &Option<String>,
    envelope: &ChannelMessageEnvelope,
) -> Vec<Result<Value, String>> {
    let mut media_results: Vec<Result<Value, String>> = Vec::new();

    for (field, caption_field, method, body_key) in [
        ("tg_video", "tg_video_caption", "sendVideo", "video"),
        ("tg_audio", "tg_audio_caption", "sendAudio", "audio"),
        (
            "tg_document",
            "tg_document_caption",
            "sendDocument",
            "document",
        ),
        ("tg_voice", "tg_voice_caption", "sendVoice", "voice"),
        (
            "tg_animation",
            "tg_animation_caption",
            "sendAnimation",
            "animation",
        ),
    ] {
        if let Some(url) = envelope.metadata.get(field) {
            let mut p = json!({"chat_id": dest_id, body_key: url});
            if let Some(cap) = envelope.metadata.get(caption_field) {
                p["caption"] = json!(truncate_html(cap, 1024));
            }
            if let Some(pm) = parse_mode {
                p["parse_mode"] = json!(pm);
            }
            media_results.push(tg_send_media(api_base, token, method, &p));
        }
    }

    if let Some(url) = envelope.metadata.get("tg_sticker") {
        let p = json!({"chat_id": dest_id, "sticker": url});
        media_results.push(tg_send_media(api_base, token, "sendSticker", &p));
    }

    if let Some(url) = envelope.metadata.get("tg_photo") {
        let mut p = json!({"chat_id": dest_id, "photo": url});
        if let Some(pm) = parse_mode {
            p["parse_mode"] = json!(pm);
        }
        media_results.push(tg_send_media(api_base, token, "sendPhoto", &p));
    }

    if let Some(loc_json) = envelope.metadata.get("tg_location")
        && let Ok(loc) = serde_json::from_str::<Value>(loc_json)
    {
        let lat = loc
            .get("latitude")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok());
        let lon = loc
            .get("longitude")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok());
        if let (Some(lat), Some(lon)) = (lat, lon) {
            let name = loc.get("name").and_then(Value::as_str).unwrap_or("");
            let address = loc.get("address").and_then(Value::as_str).unwrap_or("");
            if !name.is_empty() && !address.is_empty() {
                let p = json!({
                    "chat_id": dest_id,
                    "latitude": lat,
                    "longitude": lon,
                    "title": name,
                    "address": address,
                });
                media_results.push(tg_send_media(api_base, token, "sendVenue", &p));
            } else {
                let p = json!({
                    "chat_id": dest_id,
                    "latitude": lat,
                    "longitude": lon,
                });
                media_results.push(tg_send_media(api_base, token, "sendLocation", &p));
            }
        }
    }

    media_results
}

fn build_text_payload(
    dest_id: &str,
    text: &str,
    parse_mode: Option<&str>,
    reply_markup: &Option<Value>,
) -> Value {
    let mut p = json!({
        "chat_id": dest_id,
        "text": text,
    });
    if let Some(pm) = parse_mode {
        p.as_object_mut()
            .unwrap()
            .insert("parse_mode".into(), json!(pm));
    }
    if let Some(rm) = reply_markup {
        p.as_object_mut()
            .unwrap()
            .insert("reply_markup".into(), rm.clone());
    }
    p
}

/// Arguments for [`send_single_photo`].
struct SinglePhotoArgs<'a> {
    api_base: &'a str,
    token: &'a str,
    dest_id: &'a str,
    photo_url: &'a str,
    text: &'a str,
    parse_mode: Option<&'a str>,
    reply_markup: &'a Option<Value>,
    text_payload: &'a Value,
}

fn send_single_photo(args: SinglePhotoArgs<'_>) -> Result<client::Response, String> {
    // Single image: sendPhoto with caption (max 1024 chars) + buttons.
    let caption = truncate_html(args.text, 1024);
    let mut p = json!({
        "chat_id": args.dest_id,
        "photo": args.photo_url,
        "caption": caption,
    });
    if let Some(pm) = args.parse_mode {
        p.as_object_mut()
            .unwrap()
            .insert("parse_mode".into(), json!(pm));
    }
    if let Some(rm) = args.reply_markup {
        p.as_object_mut()
            .unwrap()
            .insert("reply_markup".into(), rm.clone());
    }
    let req = client::Request {
        method: "POST".to_string(),
        url: format!("{}/bot{}/sendPhoto", args.api_base, args.token),
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: Some(serde_json::to_vec(&p).unwrap_or_else(|_| b"{}".to_vec())),
    };
    match client::send(&req, None, None) {
        Ok(r) if (200..300).contains(&r.status) => Ok(r),
        _ => {
            // sendPhoto failed — fall back to sendMessage.
            tg_send_message(args.api_base, args.token, args.text_payload)
        }
    }
}

fn send_media_group_then_text(
    api_base: &str,
    token: &str,
    dest_id: &str,
    images: &[String],
    parse_mode: Option<&str>,
    text_payload: &Value,
) -> Result<client::Response, String> {
    // Multiple images: sendMediaGroup (album), then sendMessage (text + buttons).
    let media: Vec<Value> = images
        .iter()
        .take(10) // Telegram max 10 media per group
        .enumerate()
        .map(|(i, url)| {
            if i == 0 {
                // First item can have caption
                let mut m = json!({"type": "photo", "media": url});
                // Only short caption on album, full text in follow-up message
                if let Some(pm) = parse_mode {
                    m.as_object_mut()
                        .unwrap()
                        .insert("parse_mode".into(), json!(pm));
                }
                m
            } else {
                json!({"type": "photo", "media": url})
            }
        })
        .collect();
    let album_payload = json!({
        "chat_id": dest_id,
        "media": media,
    });
    let album_req = client::Request {
        method: "POST".to_string(),
        url: format!("{api_base}/bot{token}/sendMediaGroup"),
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: Some(serde_json::to_vec(&album_payload).unwrap_or_else(|_| b"{}".to_vec())),
    };
    // Send album (ignore errors — text message below is the important one).
    let _ = client::send(&album_req, None, None);
    // Follow up with text + inline keyboard.
    tg_send_message(api_base, token, text_payload)
}

/// Build a synthetic envelope from a flat JSON request (used when the caller
/// passes `text` and `chat_id` directly instead of a full envelope).
pub(crate) fn build_synthetic_envelope(
    parsed: &Value,
    cfg: &ProviderConfig,
) -> Result<ChannelMessageEnvelope, String> {
    let text = parsed
        .get("text")
        .and_then(|value| value.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| "text required".to_string())?;

    let chat_id = parsed
        .get("chat_id")
        .and_then(|value| value.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| cfg.default_chat_id.clone())
        .ok_or_else(|| "chat_id required".to_string())?;

    let env = EnvId::try_from("synthetic").expect("manual env id");
    let tenant = TenantId::try_from("synthetic").expect("manual tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("chat_id".to_string(), chat_id.clone());
    metadata.insert("synthetic".to_string(), "true".to_string());

    let destination = Destination {
        id: chat_id.clone(),
        kind: Some("chat".to_string()),
    };

    Ok(ChannelMessageEnvelope {
        id: format!("synthetic-telegram-{chat_id}"),
        tenant: TenantCtx::new(env, tenant),
        channel: chat_id.clone(),
        session_id: chat_id.clone(),
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
