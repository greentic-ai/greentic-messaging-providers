//! Small value-extraction helpers shared across webchat ops modules.
//!
//! These are pure functions that inspect `serde_json::Value` trees and return
//! trimmed/optional strings. They have no I/O, no state, and no provider
//! dependencies — safe to use from any ops submodule.

use base64::{Engine as _, engine::general_purpose};
use greentic_types::messaging::Attachment;
use serde_json::{Map, Value};

pub(super) fn extract_text(value: &Value) -> String {
    value
        .get("text")
        .or_else(|| value.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub(super) fn extract_activity_text(value: &Value) -> String {
    let direct = extract_text(value);
    if !direct.is_empty() {
        return direct;
    }
    value
        .get("activity")
        .and_then(|activity| activity.get("text").or_else(|| activity.get("message")))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub(super) fn decode_body_json(body_b64: &str) -> Option<Value> {
    if body_b64.trim().is_empty() {
        return None;
    }
    let decoded = general_purpose::STANDARD
        .decode(body_b64)
        .or_else(|_| general_purpose::URL_SAFE.decode(body_b64))
        .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(body_b64))
        .ok()?;
    serde_json::from_slice::<Value>(&decoded).ok()
}

pub(super) fn user_from_value(value: &Value) -> Option<String> {
    value
        .get("user_id")
        .or_else(|| value.get("from"))
        .and_then(|v| v.as_str())
        .and_then(|s| non_empty_string(Some(s)))
}

pub(super) fn route_from_value(value: &Value) -> Option<String> {
    value_as_trimmed_string(value.get("route"))
}

pub(super) fn tenant_channel_from_value(value: &Value) -> Option<String> {
    value_as_trimmed_string(value.get("tenant_channel_id"))
}

pub(super) fn public_base_url_from_value(value: &Value) -> Option<String> {
    value_as_trimmed_string(value.get("public_base_url"))
}

pub(super) fn value_as_trimmed_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub(super) fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Convert a `ChannelMessageEnvelope::Attachment` into a DirectLine-shaped JSON
/// attachment object (`{contentType, contentUrl, name?}`).
///
/// Envelope attachments carry the platform's generic shape; DirectLine clients
/// (WebChat SDK, Bot Framework) expect camelCase keys and `contentUrl` rather
/// than `url`. Inline content (e.g. Adaptive Cards) is delivered through the
/// separate `extensions` / `metadata.adaptive_card` paths, not this helper.
pub(crate) fn attachment_to_directline(attachment: &Attachment) -> Value {
    let mut map = Map::new();
    map.insert(
        "contentType".to_string(),
        Value::String(attachment.mime_type.clone()),
    );
    map.insert(
        "contentUrl".to_string(),
        Value::String(attachment.url.clone()),
    );
    if let Some(name) = &attachment.name {
        map.insert("name".to_string(), Value::String(name.clone()));
    }
    Value::Object(map)
}

/// Convert a slice of envelope attachments into a JSON array ready to embed in
/// payload bodies or DirectLine activities.
pub(crate) fn envelope_attachments_to_directline(attachments: &[Attachment]) -> Value {
    Value::Array(attachments.iter().map(attachment_to_directline).collect())
}

/// Normalise a raw JSON attachments array (as it arrives in the envelope from
/// upstream components) into DirectLine shape.
///
/// Accepts two input conventions per entry:
/// - **DirectLine / Bot Framework** (`contentType`, optional `content`,
///   optional `contentUrl`, `name`): passed through verbatim, which is the
///   canonical shape expected by DirectLine clients and lets components emit
///   inline Adaptive Card content without a URL.
/// - **Greentic generic** (`mime_type`, `url`, `name?`, `size_bytes?`): remapped
///   to DirectLine (`contentType`, `contentUrl`, `name?`).
///
/// Entries that don't match either convention are passed through as-is so the
/// function doesn't silently drop anything unrecognised — that preserves
/// forward-compatibility with shapes we haven't modelled yet (e.g.
/// `application/vnd.microsoft.card.hero` rich cards).
pub(crate) fn normalize_attachments_to_directline(entries: &[Value]) -> Value {
    Value::Array(entries.iter().map(normalize_one_attachment).collect())
}

fn normalize_one_attachment(entry: &Value) -> Value {
    let Some(obj) = entry.as_object() else {
        return entry.clone();
    };
    if obj.contains_key("contentType") {
        // Already DirectLine-shaped (covers inline Adaptive Cards and other
        // Bot Framework card types emitted natively by RAG/flow components).
        return entry.clone();
    }
    let mut out = Map::new();
    if let Some(mime) = obj.get("mime_type").and_then(|v| v.as_str()) {
        out.insert("contentType".to_string(), Value::String(mime.to_string()));
    }
    if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
        out.insert("contentUrl".to_string(), Value::String(url.to_string()));
    }
    if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
        out.insert("name".to_string(), Value::String(name.to_string()));
    }
    if out.is_empty() {
        // Neither convention matched — preserve the entry unchanged so the
        // downstream DirectLine client can decide what to do with it.
        return entry.clone();
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_attachment() -> Attachment {
        Attachment {
            mime_type: "image/png".to_string(),
            url: "https://cdn.example.com/img.png".to_string(),
            name: Some("diagram.png".to_string()),
            size_bytes: Some(1024),
        }
    }

    #[test]
    fn attachment_to_directline_maps_known_fields() {
        let directline = attachment_to_directline(&sample_attachment());
        assert_eq!(directline["contentType"], "image/png");
        assert_eq!(directline["contentUrl"], "https://cdn.example.com/img.png");
        assert_eq!(directline["name"], "diagram.png");
        // size_bytes is not a DirectLine attachment field — intentionally dropped.
        assert!(directline.get("sizeBytes").is_none());
        assert!(directline.get("size_bytes").is_none());
    }

    #[test]
    fn attachment_to_directline_omits_missing_name() {
        let mut att = sample_attachment();
        att.name = None;
        let directline = attachment_to_directline(&att);
        assert!(directline.get("name").is_none());
    }

    #[test]
    fn normalize_passes_through_directline_shape() {
        // Paul's case: RAG component emits {contentType, content} for an
        // inline Adaptive Card. Must be preserved verbatim — no remap, no
        // drop.
        let raw = serde_json::json!([
            {
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": {"type": "AdaptiveCard", "version": "1.5", "body": []}
            }
        ]);
        let arr = raw.as_array().unwrap();
        let normalized = normalize_attachments_to_directline(arr);
        let items = normalized.as_array().expect("array");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0]["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        assert_eq!(items[0]["content"]["type"], "AdaptiveCard");
    }

    #[test]
    fn normalize_remaps_greentic_shape() {
        let raw = serde_json::json!([
            {"mime_type": "image/png", "url": "https://e/i.png", "name": "i.png", "size_bytes": 42}
        ]);
        let arr = raw.as_array().unwrap();
        let normalized = normalize_attachments_to_directline(arr);
        let items = normalized.as_array().expect("array");
        assert_eq!(items[0]["contentType"], "image/png");
        assert_eq!(items[0]["contentUrl"], "https://e/i.png");
        assert_eq!(items[0]["name"], "i.png");
        // size_bytes intentionally dropped — not a DirectLine attachment field.
        assert!(items[0].get("size_bytes").is_none());
    }

    #[test]
    fn normalize_preserves_unknown_shape() {
        // Forward-compat for attachment shapes we don't know about yet.
        let raw = serde_json::json!([{"weirdField": "value"}]);
        let arr = raw.as_array().unwrap();
        let normalized = normalize_attachments_to_directline(arr);
        assert_eq!(normalized[0]["weirdField"], "value");
    }

    #[test]
    fn normalize_mixed_array() {
        let raw = serde_json::json!([
            {"contentType": "image/png", "contentUrl": "https://e/1.png"},
            {"mime_type": "application/pdf", "url": "https://e/d.pdf"}
        ]);
        let arr = raw.as_array().unwrap();
        let normalized = normalize_attachments_to_directline(arr);
        let items = normalized.as_array().expect("array");
        assert_eq!(items[0]["contentType"], "image/png");
        assert_eq!(items[0]["contentUrl"], "https://e/1.png");
        assert_eq!(items[1]["contentType"], "application/pdf");
        assert_eq!(items[1]["contentUrl"], "https://e/d.pdf");
    }

    #[test]
    fn envelope_attachments_to_directline_preserves_order() {
        let attachments = vec![
            Attachment {
                mime_type: "image/png".to_string(),
                url: "https://e/1".to_string(),
                name: None,
                size_bytes: None,
            },
            Attachment {
                mime_type: "application/pdf".to_string(),
                url: "https://e/2".to_string(),
                name: Some("doc.pdf".to_string()),
                size_bytes: None,
            },
        ];
        let arr = envelope_attachments_to_directline(&attachments);
        let items = arr.as_array().expect("array");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["contentType"], "image/png");
        assert_eq!(items[1]["name"], "doc.pdf");
    }
}
