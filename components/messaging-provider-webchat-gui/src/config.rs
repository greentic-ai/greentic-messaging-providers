use base64::{Engine as _, engine::general_purpose};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PresentationMode {
    #[default]
    Standalone,
    EmbedWebcomponent,
}

pub(crate) fn default_presentation_mode() -> PresentationMode {
    PresentationMode::Standalone
}

pub(crate) fn default_skin() -> String {
    crate::DEFAULT_SKIN.to_string()
}

pub(crate) fn default_oauth_enabled() -> Option<bool> {
    Some(crate::DEFAULT_OAUTH_ENABLED)
}

pub(crate) fn default_text_input_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ProviderConfig {
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
    pub(crate) public_base_url: String,
    #[serde(default = "default_mode")]
    pub(crate) mode: String,
    #[serde(default)]
    pub(crate) route: Option<String>,
    #[serde(default)]
    pub(crate) tenant_channel_id: Option<String>,
    #[serde(default)]
    pub(crate) base_url: Option<String>,
    #[serde(default = "default_presentation_mode")]
    pub(crate) presentation_mode: PresentationMode,
    #[serde(default = "default_skin")]
    pub(crate) skin: String,
    #[serde(default = "default_text_input_enabled")]
    pub(crate) text_input_enabled: bool,
    #[serde(default)]
    pub(crate) nav_links: Vec<Value>,
    #[serde(default = "default_oauth_enabled")]
    pub(crate) oauth_enabled: Option<bool>,
    #[serde(default)]
    pub(crate) oauth_providers: Option<String>,
    #[serde(default)]
    pub(crate) oidc_issuer: Option<String>,
    #[serde(default)]
    pub(crate) oidc_audience: Option<String>,
    #[serde(default)]
    pub(crate) oidc_required_scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProviderConfigOut {
    pub(crate) enabled: bool,
    pub(crate) public_base_url: String,
    pub(crate) mode: String,
    pub(crate) route: Option<String>,
    pub(crate) tenant_channel_id: Option<String>,
    pub(crate) base_url: Option<String>,
    #[serde(default = "default_presentation_mode")]
    pub(crate) presentation_mode: PresentationMode,
    #[serde(default = "default_skin")]
    pub(crate) skin: String,
    #[serde(default = "default_text_input_enabled")]
    pub(crate) text_input_enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) nav_links: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) jwt_signing_key_b64: Option<String>,
    #[serde(
        default = "default_oauth_enabled",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) oauth_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) oauth_providers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) oidc_issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) oidc_audience: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) oidc_required_scope: Option<String>,
}

pub(crate) fn default_enabled() -> bool {
    true
}

pub(crate) fn default_mode() -> String {
    "websocket".to_string()
}

pub(crate) fn default_config_out() -> ProviderConfigOut {
    ProviderConfigOut {
        enabled: true,
        public_base_url: String::new(),
        mode: default_mode(),
        route: None,
        tenant_channel_id: None,
        base_url: None,
        presentation_mode: PresentationMode::Standalone,
        skin: default_skin(),
        text_input_enabled: default_text_input_enabled(),
        nav_links: Vec::new(),
        jwt_signing_key_b64: None,
        oauth_enabled: default_oauth_enabled(),
        oauth_providers: None,
        oidc_issuer: None,
        oidc_audience: None,
        oidc_required_scope: None,
    }
}

pub(crate) fn normalize_nav_links(config: &mut ProviderConfigOut) {
    if config.presentation_mode == PresentationMode::EmbedWebcomponent {
        config.nav_links.clear();
    }
}

/// True when `oauth_providers` (the composed JSON array) lists a `greentic`
/// entry. Used to cross-check the toggle against `oidc_issuer` — a greentic
/// provider with no issuer renders no login button but still shouldn't ship.
fn has_greentic_provider(config: &ProviderConfigOut) -> bool {
    let Some(raw) = config.oauth_providers.as_deref() else {
        return false;
    };
    let Ok(Value::Array(providers)) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    providers
        .iter()
        .any(|p| p.get("type").and_then(Value::as_str) == Some("greentic"))
}

pub(crate) fn validate_config_out(config: &ProviderConfigOut) -> Result<(), String> {
    if config.public_base_url.trim().is_empty() {
        return Err("config validation failed: public_base_url is required".to_string());
    }
    if config.mode.trim().is_empty() {
        return Err("config validation failed: mode is required".to_string());
    }
    if !(config.public_base_url.starts_with("http://")
        || config.public_base_url.starts_with("https://"))
    {
        return Err(
            "config validation failed: public_base_url must be an absolute URL".to_string(),
        );
    }
    if config.skin.trim().is_empty() {
        return Err("config validation failed: skin is required".to_string());
    }
    if has_greentic_provider(config)
        && !config
            .oidc_issuer
            .as_deref()
            .is_some_and(|issuer| issuer.starts_with("https://"))
    {
        return Err(
            "config validation failed: Greentic SSO is enabled (oauth_enable_greentic) but \
             oidc_issuer is missing or is not an https:// URL — set oauth_greentic_issuer"
                .to_string(),
        );
    }
    Ok(())
}

pub(crate) fn validate_provider_config(mut cfg: ProviderConfig) -> Result<ProviderConfig, String> {
    if cfg.public_base_url.trim().is_empty() {
        return Err("invalid config: public_base_url cannot be empty".to_string());
    }
    let mode = cfg.mode.trim();
    if mode != "local_queue" && mode != "websocket" && mode != "pubsub" {
        return Err("invalid config: mode must be local_queue|websocket|pubsub".to_string());
    }
    // A blank answer is no answer: the emitted setup template fills an
    // unanswered question with "", which would otherwise route under an
    // empty key instead of being refused here.
    cfg.route = cfg.route.filter(|value| !value.trim().is_empty());
    cfg.tenant_channel_id = cfg
        .tenant_channel_id
        .filter(|value| !value.trim().is_empty());
    if cfg.route.is_none() && cfg.tenant_channel_id.is_none() {
        return Err("invalid config: route or tenant_channel_id required".to_string());
    }
    if cfg.skin.trim().is_empty() {
        return Err("invalid config: skin cannot be empty".to_string());
    }
    if cfg.presentation_mode == PresentationMode::EmbedWebcomponent {
        cfg.nav_links.clear();
    }
    Ok(cfg)
}

fn parse_config_value(val: &Value) -> Result<ProviderConfig, String> {
    let cfg = serde_json::from_value::<ProviderConfig>(val.clone())
        .map_err(|e| format!("invalid config: {e}"))?;
    validate_provider_config(cfg)
}

fn decode_injected_config_field(input: &Value, key: &str) -> Option<Value> {
    let encoded = input.get(format!("{key}_b64"))?.as_str()?.trim();
    if encoded.is_empty() {
        return None;
    }
    let decoded = general_purpose::STANDARD
        .decode(encoded)
        .or_else(|_| general_purpose::URL_SAFE.decode(encoded))
        .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(encoded))
        .ok()?;
    let decoded_str = String::from_utf8(decoded).ok()?;
    let trimmed = decoded_str.trim();
    if trimmed.is_empty() {
        return None;
    }

    match key {
        "enabled" | "oauth_enabled" | "text_input_enabled" => {
            trimmed.parse::<bool>().ok().map(Value::Bool)
        }
        "nav_links" => serde_json::from_str(trimmed).ok(),
        _ => Some(Value::String(trimmed.to_string())),
    }
}

pub(crate) fn load_config(input: &Value) -> Result<ProviderConfig, String> {
    if let Some(cfg) = input.get("config") {
        return parse_config_value(cfg);
    }
    let mut partial = serde_json::Map::new();
    for key in [
        "enabled",
        "public_base_url",
        "mode",
        "route",
        "tenant_channel_id",
        "base_url",
        "presentation_mode",
        "skin",
        "text_input_enabled",
        "nav_links",
        "oauth_enabled",
        "oauth_providers",
        // Surfaced via ConfigAwareSecretStore, not parsed into ProviderConfig.
        "oauth_greentic_issuer",
        "oauth_greentic_client_id",
        "oidc_issuer",
        "oidc_audience",
        "oidc_required_scope",
    ] {
        if let Some(v) = input.get(key) {
            partial.insert(key.to_string(), v.clone());
        }
    }
    for key in [
        "enabled",
        "public_base_url",
        "mode",
        "route",
        "tenant_channel_id",
        "base_url",
        "presentation_mode",
        "skin",
        "text_input_enabled",
        "nav_links",
        "oauth_enabled",
        "oauth_providers",
        // Surfaced via ConfigAwareSecretStore, not parsed into ProviderConfig.
        "oauth_greentic_issuer",
        "oauth_greentic_client_id",
        "oidc_issuer",
        "oidc_audience",
        "oidc_required_scope",
    ] {
        if partial.contains_key(key) {
            continue;
        }
        if let Some(v) = decode_injected_config_field(input, key) {
            partial.insert(key.to_string(), v);
        }
    }
    if !partial.is_empty() {
        return parse_config_value(&Value::Object(partial));
    }

    Err("config required".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn presentation_mode_defaults_to_standalone() {
        let cfg: ProviderConfig = serde_json::from_value(json!({
            "public_base_url": "https://example.com",
            "route": "webchat"
        }))
        .unwrap();
        assert_eq!(cfg.presentation_mode, PresentationMode::Standalone);
        assert_eq!(cfg.skin, crate::DEFAULT_SKIN);
        assert!(cfg.text_input_enabled);
        assert_eq!(cfg.mode, "websocket");
    }

    #[test]
    fn rejects_unknown_presentation_mode() {
        let err = serde_json::from_value::<ProviderConfig>(json!({
            "public_base_url": "https://example.com",
            "mode": "local_queue",
            "route": "webchat",
            "presentation_mode": "skin"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn embed_mode_clears_nav_links() {
        let cfg = validate_provider_config(
            serde_json::from_value(json!({
                "public_base_url": "https://example.com",
                "mode": "local_queue",
                "route": "webchat",
                "presentation_mode": "embed_webcomponent",
                "nav_links": [{"label": "Help", "url": "/help"}]
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(cfg.presentation_mode, PresentationMode::EmbedWebcomponent);
        assert!(cfg.nav_links.is_empty());
    }

    #[test]
    fn load_config_decodes_base64_injected_setup_fields() {
        let nav_links = r#"[{"label":"Docs","url":"/docs"}]"#;
        let input = json!({
            "public_base_url_b64": general_purpose::STANDARD.encode("https://chat.example.com"),
            "mode_b64": general_purpose::STANDARD.encode("local_queue"),
            "route_b64": general_purpose::STANDARD.encode("webchat"),
            "presentation_mode_b64": general_purpose::STANDARD.encode("standalone"),
            "skin_b64": general_purpose::STANDARD.encode("3aigent"),
            "text_input_enabled_b64": general_purpose::STANDARD.encode("false"),
            "nav_links_b64": general_purpose::STANDARD.encode(nav_links)
        });

        let cfg = load_config(&input).expect("config");

        assert_eq!(cfg.public_base_url, "https://chat.example.com");
        assert_eq!(cfg.route.as_deref(), Some("webchat"));
        assert_eq!(cfg.presentation_mode, PresentationMode::Standalone);
        assert_eq!(cfg.skin, "3aigent");
        assert!(!cfg.text_input_enabled);
        assert_eq!(cfg.nav_links.len(), 1);
        assert_eq!(cfg.nav_links[0]["label"], "Docs");
    }

    #[test]
    fn load_config_rejects_empty_or_unknown_mode() {
        let missing = load_config(&json!({})).unwrap_err();
        assert_eq!(missing, "config required");

        let invalid = load_config(&json!({
            "public_base_url": "https://chat.example.com",
            "mode": "invalid",
            "route": "webchat"
        }))
        .unwrap_err();
        assert_eq!(
            invalid,
            "invalid config: mode must be local_queue|websocket|pubsub"
        );
    }

    #[test]
    fn validate_config_out_requires_absolute_public_url_and_skin() {
        let mut cfg = default_config_out();
        cfg.public_base_url = "/relative".to_string();

        let err = validate_config_out(&cfg).unwrap_err();
        assert!(err.contains("absolute URL"), "{err}");

        cfg.public_base_url = "https://chat.example.com".to_string();
        cfg.skin.clear();
        let err = validate_config_out(&cfg).unwrap_err();
        assert!(err.contains("skin is required"), "{err}");
    }

    fn valid_base_config() -> ProviderConfigOut {
        let mut cfg = default_config_out();
        cfg.public_base_url = "https://chat.example.com".to_string();
        cfg
    }

    #[test]
    fn validate_config_out_rejects_greentic_provider_without_https_issuer() {
        let mut cfg = valid_base_config();
        cfg.oauth_providers = Some(
            json!([{"id": "greentic", "type": "greentic", "client_id": "webchat-gui"}]).to_string(),
        );
        cfg.oidc_issuer = None;
        let err = validate_config_out(&cfg).unwrap_err();
        assert!(err.contains("oidc_issuer"), "{err}");

        cfg.oidc_issuer = Some("acme.greentic-id.com".to_string());
        let err = validate_config_out(&cfg).unwrap_err();
        assert!(err.contains("oidc_issuer"), "{err}");
    }

    #[test]
    fn validate_config_out_accepts_greentic_provider_with_https_issuer() {
        let mut cfg = valid_base_config();
        cfg.oauth_providers = Some(
            json!([{"id": "greentic", "type": "greentic", "client_id": "webchat-gui"}]).to_string(),
        );
        cfg.oidc_issuer = Some("https://acme.greentic-id.com".to_string());
        assert!(validate_config_out(&cfg).is_ok());
    }

    #[test]
    fn validate_config_out_ignores_missing_issuer_without_greentic_provider() {
        let cfg = valid_base_config();
        assert!(cfg.oauth_providers.is_none());
        assert!(validate_config_out(&cfg).is_ok());
    }
}
