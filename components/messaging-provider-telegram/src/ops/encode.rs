//! `encode` op for the Telegram provider.
//!
//! Responsibilities:
//! 1. Decode the host `encode` CBOR/JSON request into a
//!    [`ChannelMessageEnvelope`].
//! 2. If an Adaptive Card is attached, delegate to
//!    [`super::ac_to_html::ac_to_telegram`] to produce HTML + actions + image
//!    URLs + pending inputs, stashing them in metadata for `send.rs` to pick
//!    up later.
//! 3. Normalise all `tg_*` metadata fields (strip operator quoting, map URL
//!    suffixes, merge lat/lon into `tg_location`, classify attachments by MIME
//!    type).
//! 4. Re-serialise into a [`ProviderPayloadV1`] the runner can feed into
//!    `send_payload`.

use base64::{Engine, engine::general_purpose::STANDARD};
use greentic_types::messaging::universal_dto::ProviderPayloadV1;
use provider_common::helpers::{decode_encode_message, encode_error, json_bytes};
use serde_json::json;
use std::collections::BTreeMap;

use super::ac_to_html::ac_to_telegram;

pub(crate) fn encode_op(input_json: &[u8]) -> Vec<u8> {
    let mut envelope = match decode_encode_message(input_json) {
        Ok(value) => value,
        Err(err) => return encode_error(&err),
    };

    // If the message carries an Adaptive Card, convert it to rich Telegram
    // content: HTML text, inline keyboard buttons, images for sendPhoto.
    if let Some(ac_raw) = provider_common::helpers::resolve_adaptive_card(&envelope)
        .map(|v| serde_json::to_string(&v).unwrap_or_default())
        .as_deref()
        && let Some(content) = ac_to_telegram(ac_raw)
    {
        envelope.text = Some(content.html);
        envelope
            .metadata
            .insert("parse_mode".to_string(), "HTML".to_string());
        if !content.actions.is_empty() {
            let aj = serde_json::to_string(&content.actions).unwrap_or_default();
            envelope.metadata.insert("ac_actions".to_string(), aj);
        }
        if !content.images.is_empty() {
            let ij = serde_json::to_string(&content.images).unwrap_or_default();
            envelope.metadata.insert("ac_images".to_string(), ij);
        }
        if !content.inputs.is_empty() {
            // `ac_pending_inputs` is consumed by `ops::send::handle_send` via
            // `ac_helpers::has_pending_text_inputs` / `first_input_placeholder`
            // to drive Telegram `ForceReply` mode for Adaptive Card text inputs.
            // See `ops::ac_inputs` top-of-file docs for the full contract.
            let ij = serde_json::to_string(&content.inputs).unwrap_or_default();
            envelope
                .metadata
                .insert("ac_pending_inputs".to_string(), ij);
            // Store the submit action data so ingest_http can auto-submit
            // after all text inputs are collected.
            if let Some(submit_action) = content.actions.iter().find(|a| a.get("data").is_some())
                && let Some(data) = submit_action.get("data")
            {
                envelope
                    .metadata
                    .insert("ac_submit_data".to_string(), data.to_string());
            }
        }
    }

    // ── Map tg_* metadata fields and attachments to internal keys ──

    // 1. Strip operator double-quotes from all tg_* values
    let tg_keys: Vec<String> = envelope
        .metadata
        .keys()
        .filter(|k| k.starts_with("tg_"))
        .cloned()
        .collect();
    for key in tg_keys {
        if let Some(val) = envelope.metadata.get_mut(&key) {
            let trimmed = val.trim_matches('"').to_string();
            *val = trimmed;
        }
    }

    // 2. Map URL-suffixed keys to internal keys
    for (src, dst) in [
        ("tg_video_url", "tg_video"),
        ("tg_audio_url", "tg_audio"),
        ("tg_document_url", "tg_document"),
        ("tg_voice_url", "tg_voice"),
        ("tg_animation_url", "tg_animation"),
        ("tg_sticker_url", "tg_sticker"),
        ("tg_photo_url", "tg_photo"),
    ] {
        if let Some(val) = envelope.metadata.get(src).cloned()
            && !val.is_empty()
        {
            envelope.metadata.entry(dst.to_string()).or_insert(val);
        }
    }

    // 3. Build location from lat/lon metadata
    {
        let lat = envelope.metadata.get("tg_location_latitude").cloned();
        let lon = envelope.metadata.get("tg_location_longitude").cloned();
        let loc_name = envelope.metadata.get("tg_location_name").cloned();
        let loc_addr = envelope.metadata.get("tg_location_address").cloned();
        if let (Some(lat), Some(lon)) = (lat, lon)
            && !lat.is_empty()
            && !lon.is_empty()
            && !envelope.metadata.contains_key("tg_location")
        {
            let mut loc = json!({"latitude": lat, "longitude": lon});
            if let Some(name) = loc_name
                && !name.is_empty()
            {
                loc["name"] = json!(name);
            }
            if let Some(addr) = loc_addr
                && !addr.is_empty()
            {
                loc["address"] = json!(addr);
            }
            envelope.metadata.insert(
                "tg_location".to_string(),
                serde_json::to_string(&loc).unwrap_or_default(),
            );
        }
    }

    // 4. Map attachments by mime_type (metadata takes precedence)
    let att_fields: Vec<(String, String)> = envelope
        .attachments
        .iter()
        .map(|att| {
            let mt = att.mime_type.to_lowercase();
            if mt.starts_with("video/") {
                ("tg_video".to_string(), att.url.clone())
            } else if mt == "audio/ogg" || mt.starts_with("audio/ogg;") || mt == "audio/opus" {
                ("tg_voice".to_string(), att.url.clone())
            } else if mt.starts_with("audio/") {
                ("tg_audio".to_string(), att.url.clone())
            } else if mt == "image/webp" {
                ("tg_sticker".to_string(), att.url.clone())
            } else if mt == "image/gif" {
                ("tg_animation".to_string(), att.url.clone())
            } else if mt.starts_with("image/") {
                ("tg_photo".to_string(), att.url.clone())
            } else {
                ("tg_document".to_string(), att.url.clone())
            }
        })
        .collect();
    for (k, v) in att_fields {
        envelope.metadata.entry(k).or_insert(v);
    }

    let has_text = envelope
        .text
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .is_some();
    if !has_text {
        envelope.text = Some("universal telegram payload".to_string());
    }
    let body_bytes = serde_json::to_vec(&envelope).unwrap_or_else(|_| b"{}".to_vec());
    let payload = ProviderPayloadV1 {
        content_type: "application/json".to_string(),
        body_b64: STANDARD.encode(&body_bytes),
        metadata: BTreeMap::new(),
    };
    json_bytes(&json!({"ok": true, "payload": payload}))
}
