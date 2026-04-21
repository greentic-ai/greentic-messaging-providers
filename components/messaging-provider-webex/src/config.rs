use crate::bindings::greentic::secrets_store::secrets_store;
use crate::{DEFAULT_API_BASE, DEFAULT_TOKEN_KEY, ProviderConfig, ProviderConfigOut};
use greentic_types::{Destination, MessageMetadata};
use serde_json::Value;

pub(crate) fn default_enabled() -> bool {
    true
}

pub(crate) fn default_config_out() -> ProviderConfigOut {
    ProviderConfigOut {
        enabled: true,
        public_base_url: String::new(),
        default_room_id: None,
        default_to_person_email: None,
        api_base_url: DEFAULT_API_BASE.to_string(),
        bot_token: None,
        default_locale: None,
    }
}

pub(crate) fn validate_config_out(config: &ProviderConfigOut) -> Result<(), String> {
    if config.public_base_url.trim().is_empty() {
        return Err("invalid config: public_base_url cannot be empty".to_string());
    }
    if !(config.public_base_url.starts_with("http://")
        || config.public_base_url.starts_with("https://"))
    {
        return Err("invalid config: public_base_url must be an absolute URL".to_string());
    }
    if config.api_base_url.trim().is_empty() {
        return Err("invalid config: api_base_url cannot be empty".to_string());
    }
    if !(config.api_base_url.starts_with("http://") || config.api_base_url.starts_with("https://"))
    {
        return Err("invalid config: api_base_url must be an absolute URL".to_string());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn parse_config_bytes(bytes: &[u8]) -> Result<ProviderConfig, String> {
    let cfg = serde_json::from_slice::<ProviderConfig>(bytes)
        .map_err(|e| format!("invalid config: {e}"))?;
    validate_provider_config(cfg)
}

pub(crate) fn parse_config_value(val: &Value) -> Result<ProviderConfig, String> {
    let cfg = serde_json::from_value::<ProviderConfig>(val.clone())
        .map_err(|e| format!("invalid config: {e}"))?;
    validate_provider_config(cfg)
}

pub(crate) fn load_config(input: &Value) -> Result<ProviderConfig, String> {
    if let Some(cfg) = input.get("config") {
        return parse_config_value(cfg);
    }
    let mut partial = serde_json::Map::new();
    for key in [
        "enabled",
        "public_base_url",
        "default_room_id",
        "default_to_person_email",
        "api_base_url",
        "bot_token",
        "default_locale",
    ] {
        if let Some(v) = input.get(key) {
            partial.insert(key.to_string(), v.clone());
        }
    }
    if !partial.is_empty() {
        return parse_config_value(&Value::Object(partial));
    }

    Ok(ProviderConfig {
        enabled: true,
        public_base_url: "https://invalid.local".to_string(),
        default_room_id: None,
        default_to_person_email: None,
        api_base_url: Some(DEFAULT_API_BASE.to_string()),
        bot_token: None,
        default_locale: None,
    })
}

pub(crate) fn override_config_from_metadata(cfg: &mut ProviderConfig, metadata: &MessageMetadata) {
    if let Some(api) = metadata.get("config.api_base_url") {
        cfg.api_base_url = Some(api.clone());
    }
    if let Some(public_base_url) = metadata.get("config.public_base_url") {
        cfg.public_base_url = public_base_url.clone();
    }
    if let Some(email) = metadata.get("config.default_to_person_email") {
        cfg.default_to_person_email = Some(email.clone());
    }
}

pub(crate) fn validate_provider_config(cfg: ProviderConfig) -> Result<ProviderConfig, String> {
    if cfg.public_base_url.trim().is_empty() {
        return Err("invalid config: public_base_url cannot be empty".to_string());
    }
    Ok(cfg)
}

/// Auto-detect Webex destination kind from the ID format.
/// Webex room/person IDs are base64-encoded URNs starting with "Y2lz".
/// Emails contain "@".
pub(crate) fn detect_destination_kind(dest_id: &str) -> &'static str {
    if dest_id.contains('@') {
        "email"
    } else if dest_id.starts_with("Y2lz") {
        // Base64 prefix for "ciscospark://..." URNs — could be room or person.
        // Try roomId first as it's the most common API target.
        "room"
    } else {
        "email"
    }
}

pub(crate) fn get_token(cfg: &ProviderConfig) -> Result<String, String> {
    if let Some(token) = cfg.bot_token.clone() {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    get_secret_string(DEFAULT_TOKEN_KEY)
}

pub(crate) fn get_secret_string(key: &str) -> Result<String, String> {
    match secrets_store::get(key) {
        Ok(Some(bytes)) => String::from_utf8(bytes).map_err(|_| "secret not valid utf-8".into()),
        Ok(None) => Err(format!("missing secret: {key}")),
        Err(e) => Err(format!("secret store error: {e:?}")),
    }
}

pub(crate) fn build_send_envelope_from_input(
    parsed: &Value,
    cfg: &ProviderConfig,
) -> Result<greentic_types::ChannelMessageEnvelope, String> {
    use greentic_types::{EnvId, TenantCtx, TenantId};

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
    let mut metadata = greentic_types::MessageMetadata::new();
    metadata.insert("synthetic".to_string(), "true".to_string());
    if let Some(kind) = &destination.kind {
        metadata.insert("destination_kind".to_string(), kind.clone());
    }
    let channel_name = destination.id.clone();

    Ok(greentic_types::ChannelMessageEnvelope {
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
        extensions: Default::default(),
    })
}

pub(crate) fn parse_send_destination(parsed: &Value, cfg: &ProviderConfig) -> Option<Destination> {
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

pub(crate) fn parsed_to_destination(parsed: &Value) -> Option<Destination> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_config_out() -> ProviderConfigOut {
        ProviderConfigOut {
            enabled: true,
            public_base_url: "https://example.com".to_string(),
            default_room_id: Some("room-1".to_string()),
            default_to_person_email: None,
            api_base_url: "https://webex.example.com/v1".to_string(),
            bot_token: Some("token".to_string()),
            default_locale: None,
        }
    }

    fn valid_provider_config() -> ProviderConfig {
        ProviderConfig {
            enabled: true,
            public_base_url: "https://example.com".to_string(),
            default_room_id: Some("room-1".to_string()),
            default_to_person_email: None,
            api_base_url: Some("https://webex.example.com/v1".to_string()),
            bot_token: Some("token".to_string()),
            default_locale: None,
        }
    }

    #[test]
    fn validate_config_out_rejects_invalid_urls() {
        let mut config = valid_config_out();
        config.public_base_url = String::new();
        assert_eq!(
            validate_config_out(&config),
            Err("invalid config: public_base_url cannot be empty".to_string())
        );

        let mut config = valid_config_out();
        config.public_base_url = "relative".to_string();
        assert_eq!(
            validate_config_out(&config),
            Err("invalid config: public_base_url must be an absolute URL".to_string())
        );

        let mut config = valid_config_out();
        config.api_base_url = "relative".to_string();
        assert_eq!(
            validate_config_out(&config),
            Err("invalid config: api_base_url must be an absolute URL".to_string())
        );
    }

    #[test]
    fn load_config_parses_nested_top_level_and_defaults() {
        let nested = load_config(&json!({
            "config": {
                "public_base_url": "https://example.com",
                "default_room_id": "nested-room"
            }
        }))
        .expect("nested config");
        assert_eq!(nested.default_room_id.as_deref(), Some("nested-room"));

        let top_level = load_config(&json!({
            "public_base_url": "https://example.com",
            "default_to_person_email": "person@example.com"
        }))
        .expect("top-level config");
        assert_eq!(
            top_level.default_to_person_email.as_deref(),
            Some("person@example.com")
        );

        let defaulted = load_config(&json!({})).expect("default config");
        assert_eq!(defaulted.public_base_url, "https://invalid.local");
        assert_eq!(defaulted.api_base_url.as_deref(), Some(DEFAULT_API_BASE));
    }

    #[test]
    fn override_and_destination_helpers_apply_fallbacks() {
        let mut cfg = valid_provider_config();
        let mut metadata = MessageMetadata::new();
        metadata.insert(
            "config.default_to_person_email".to_string(),
            "person@example.com".to_string(),
        );
        metadata.insert(
            "config.public_base_url".to_string(),
            "https://override.example.com".to_string(),
        );
        override_config_from_metadata(&mut cfg, &metadata);
        assert_eq!(cfg.public_base_url, "https://override.example.com");
        assert_eq!(
            cfg.default_to_person_email.as_deref(),
            Some("person@example.com")
        );

        let room_dest = parse_send_destination(&json!({ "text": "hi" }), &cfg)
            .expect("fallback room destination");
        assert_eq!(room_dest.id, "room-1");
        assert_eq!(room_dest.kind.as_deref(), Some("room"));

        cfg.default_room_id = None;
        let email_dest = parse_send_destination(&json!({ "text": "hi" }), &cfg)
            .expect("fallback email destination");
        assert_eq!(email_dest.id, "person@example.com");
        assert_eq!(email_dest.kind.as_deref(), Some("email"));
    }

    #[test]
    fn parsed_to_destination_and_envelope_builder_trim_values() {
        let destination = parsed_to_destination(&json!({
            "to": { "id": " room-2 ", "kind": " room " }
        }))
        .expect("parsed destination");
        assert_eq!(destination.id, "room-2");
        assert_eq!(destination.kind.as_deref(), Some("room"));

        let envelope = build_send_envelope_from_input(
            &json!({ "text": " hello ", "to": " room-3 " }),
            &valid_provider_config(),
        )
        .expect("envelope");
        assert_eq!(envelope.channel, "room-3");
        assert_eq!(envelope.text.as_deref(), Some("hello"));
        assert_eq!(
            envelope
                .metadata
                .get("destination_kind")
                .map(String::as_str),
            Some("room")
        );
    }
}
