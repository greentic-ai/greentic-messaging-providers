use base64::{Engine as _, engine::general_purpose};
use greentic_types::messaging::universal_dto::ProviderPayloadV1;
use provider_common::helpers::{decode_encode_message, encode_error, json_bytes};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn encode_op(input_json: &[u8]) -> Vec<u8> {
    use provider_common::helpers::extract_ac_summary;

    let encode_message = match decode_encode_message(input_json) {
        Ok(value) => value,
        Err(err) => return encode_error(&err),
    };

    // If the message carries an Adaptive Card (extensions or legacy metadata),
    // extract rich content for WhatsApp.
    let ac_raw_str = provider_common::helpers::resolve_adaptive_card(&encode_message)
        .map(|v| serde_json::to_string(&v).unwrap_or_default());
    let wa_content = ac_raw_str
        .as_deref()
        .and_then(crate::ac_converter::ac_to_whatsapp);

    let text = if let Some(ref content) = wa_content {
        content.body.clone()
    } else {
        let caps = greentic_messaging_renderer::capabilities_for("whatsapp")
            .expect("whatsapp capabilities must be registered");
        ac_raw_str
            .as_deref()
            .and_then(|ac_raw| extract_ac_summary(ac_raw, &caps))
            .or_else(|| encode_message.text.clone().filter(|t| !t.trim().is_empty()))
            .unwrap_or_else(|| "universal whatsapp payload".to_string())
    };

    // Destination: try metadata["from"] (ingress path), then message.to[0].id (demo send path)
    let to_id = encode_message
        .metadata
        .get("from")
        .cloned()
        .or_else(|| encode_message.to.first().map(|d| d.id.clone()))
        .unwrap_or_else(|| "whatsapp-user".to_string());
    let to_kind = encode_message
        .to
        .first()
        .and_then(|d| d.kind.clone())
        .unwrap_or_else(|| "phone".to_string());
    let phone_number_id = encode_message
        .metadata
        .get("phone_number_id")
        .cloned()
        .or({
            // Fallback: read from secrets store so `demo send` works without --arg.
            #[cfg(target_arch = "wasm32")]
            {
                use crate::bindings::greentic::secrets_store::secrets_store;
                match secrets_store::get("PHONE_NUMBER_ID") {
                    Ok(Some(bytes)) => String::from_utf8(bytes)
                        .ok()
                        .filter(|s| !s.trim().is_empty()),
                    _ => None,
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                None
            }
        })
        .unwrap_or_else(|| "phone-universal".to_string());
    let to = json!({
        "kind": to_kind,
        "id": to_id,
    });
    let config = json!({
        "phone_number_id": phone_number_id,
        "enabled": true,
        "public_base_url": "https://localhost",
    });
    let mut payload_body = json!({
        "text": text,
        "to": to,
        "config": config,
    });
    // Store WhatsApp-specific content in the payload for handle_send.
    if let Some(content) = wa_content {
        let obj = payload_body.as_object_mut().unwrap();
        if let Some(header) = content.header {
            obj.insert("wa_header".into(), json!(header));
        }
        if let Some(image_url) = content.image_url {
            obj.insert("wa_image".into(), json!(image_url));
        }
        if !content.buttons.is_empty() {
            obj.insert("wa_buttons".into(), json!(content.buttons));
        }
    }

    // Enrich payload with wa_* media fields from message metadata.
    // Note: operator may wrap metadata values with extra quotes — strip them.
    {
        let meta = &encode_message.metadata;
        let obj = payload_body.as_object_mut().unwrap();
        let strip_quotes = |s: &str| -> String {
            s.strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(s)
                .to_string()
        };

        if let Some(v) = meta.get("wa_video_url") {
            obj.insert("wa_video".into(), json!(strip_quotes(v)));
        }
        if let Some(v) = meta.get("wa_video_caption") {
            obj.insert("wa_video_caption".into(), json!(strip_quotes(v)));
        }
        if let Some(v) = meta.get("wa_audio_url") {
            obj.insert("wa_audio".into(), json!(strip_quotes(v)));
        }
        if let Some(v) = meta.get("wa_document_url") {
            obj.insert("wa_document".into(), json!(strip_quotes(v)));
        }
        if let Some(v) = meta.get("wa_document_filename") {
            obj.insert("wa_document_filename".into(), json!(strip_quotes(v)));
        }
        if let Some(v) = meta.get("wa_document_caption") {
            obj.insert("wa_document_caption".into(), json!(strip_quotes(v)));
        }
        if let Some(v) = meta.get("wa_sticker_url") {
            obj.insert("wa_sticker".into(), json!(strip_quotes(v)));
        }
        if let Some(v) = meta.get("wa_image_url") {
            obj.entry("wa_image")
                .or_insert_with(|| json!(strip_quotes(v)));
        }

        // Location: build wa_location object from individual lat/lon/name/address metadata fields.
        if let (Some(lat), Some(lon)) = (
            meta.get("wa_location_latitude"),
            meta.get("wa_location_longitude"),
        ) {
            let mut loc = json!({ "latitude": strip_quotes(lat), "longitude": strip_quotes(lon) });
            if let Some(name) = meta.get("wa_location_name") {
                loc.as_object_mut()
                    .unwrap()
                    .insert("name".into(), json!(strip_quotes(name)));
            }
            if let Some(addr) = meta.get("wa_location_address") {
                loc.as_object_mut()
                    .unwrap()
                    .insert("address".into(), json!(strip_quotes(addr)));
            }
            obj.insert("wa_location".into(), loc);
        }

        // Map attachments by mime_type (only if corresponding wa_* not already set).
        for att in &encode_message.attachments {
            let mime = att.mime_type.as_str();
            if mime.starts_with("video/") && !obj.contains_key("wa_video") {
                obj.insert("wa_video".into(), json!(att.url));
                if let Some(ref name) = att.name {
                    obj.entry("wa_video_caption").or_insert_with(|| json!(name));
                }
            } else if mime.starts_with("audio/") && !obj.contains_key("wa_audio") {
                obj.insert("wa_audio".into(), json!(att.url));
            } else if mime == "image/webp" && !obj.contains_key("wa_sticker") {
                obj.insert("wa_sticker".into(), json!(att.url));
            } else if mime.starts_with("image/") && !obj.contains_key("wa_image") {
                obj.insert("wa_image".into(), json!(att.url));
            } else if !obj.contains_key("wa_document") {
                obj.insert("wa_document".into(), json!(att.url));
                if let Some(ref name) = att.name {
                    obj.insert("wa_document_filename".into(), json!(name));
                }
            }
        }
    }
    let body_bytes = serde_json::to_vec(&payload_body).unwrap_or_else(|_| b"{}".to_vec());
    let mut metadata = BTreeMap::new();
    metadata.insert("method".to_string(), Value::String("POST".to_string()));
    let payload = ProviderPayloadV1 {
        content_type: "application/json".to_string(),
        body_b64: general_purpose::STANDARD.encode(&body_bytes),
        metadata,
    };
    json_bytes(&json!({"ok": true, "payload": payload}))
}
