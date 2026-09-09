use base64::{Engine as _, engine::general_purpose};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // OAuth fields are deserialized but read via secrets store in ops.rs
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
    #[serde(default)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) jwt_signing_key_b64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    "local_queue".to_string()
}

pub(crate) fn default_config_out() -> ProviderConfigOut {
    ProviderConfigOut {
        enabled: true,
        public_base_url: String::new(),
        mode: default_mode(),
        route: None,
        tenant_channel_id: None,
        base_url: None,
        jwt_signing_key_b64: None,
        oauth_enabled: None,
        oauth_providers: None,
        oidc_issuer: None,
        oidc_audience: None,
        oidc_required_scope: None,
    }
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
        "enabled" | "oauth_enabled" => trimmed.parse::<bool>().ok().map(Value::Bool),
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
#[allow(dead_code)]
pub(crate) fn parse_config_bytes(bytes: &[u8]) -> Result<ProviderConfig, String> {
    let cfg = serde_json::from_slice::<ProviderConfig>(bytes)
        .map_err(|e| format!("invalid config: {e}"))?;
    validate_provider_config(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_config_out() -> ProviderConfigOut {
        ProviderConfigOut {
            enabled: true,
            public_base_url: "https://example.com".to_string(),
            mode: "local_queue".to_string(),
            route: Some("route-a".to_string()),
            tenant_channel_id: None,
            base_url: Some("https://base.example.com".to_string()),
            jwt_signing_key_b64: None,
            oauth_enabled: None,
            oauth_providers: None,
            oidc_issuer: None,
            oidc_audience: None,
            oidc_required_scope: None,
        }
    }

    #[test]
    fn validate_config_out_rejects_empty_or_relative_urls() {
        let mut config = valid_config_out();
        config.public_base_url = String::new();
        assert_eq!(
            validate_config_out(&config),
            Err("config validation failed: public_base_url is required".to_string())
        );

        let mut config = valid_config_out();
        config.mode = String::new();
        assert_eq!(
            validate_config_out(&config),
            Err("config validation failed: mode is required".to_string())
        );

        let mut config = valid_config_out();
        config.public_base_url = "/relative".to_string();
        assert_eq!(
            validate_config_out(&config),
            Err("config validation failed: public_base_url must be an absolute URL".to_string())
        );
    }

    #[test]
    fn validate_provider_config_checks_mode_and_routing() {
        let err = validate_provider_config(ProviderConfig {
            enabled: true,
            public_base_url: "https://example.com".to_string(),
            mode: "invalid".to_string(),
            route: Some("route-a".to_string()),
            tenant_channel_id: None,
            base_url: None,
            oauth_enabled: None,
            oauth_providers: None,
            oidc_issuer: None,
            oidc_audience: None,
            oidc_required_scope: None,
        })
        .unwrap_err();
        assert_eq!(
            err,
            "invalid config: mode must be local_queue|websocket|pubsub".to_string()
        );

        let err = validate_provider_config(ProviderConfig {
            enabled: true,
            public_base_url: "https://example.com".to_string(),
            mode: "pubsub".to_string(),
            route: None,
            tenant_channel_id: None,
            base_url: None,
            oauth_enabled: None,
            oauth_providers: None,
            oidc_issuer: None,
            oidc_audience: None,
            oidc_required_scope: None,
        })
        .unwrap_err();
        assert_eq!(
            err,
            "invalid config: route or tenant_channel_id required".to_string()
        );
    }

    #[test]
    fn a_blank_route_and_tenant_channel_id_are_refused_not_routed_under_an_empty_key() {
        let base = |route: Option<&str>, tenant: Option<&str>| ProviderConfig {
            enabled: true,
            public_base_url: "https://example.com".to_string(),
            mode: "websocket".to_string(),
            route: route.map(str::to_string),
            tenant_channel_id: tenant.map(str::to_string),
            base_url: None,
            oauth_enabled: None,
            oauth_providers: None,
            oidc_issuer: None,
            oidc_audience: None,
            oidc_required_scope: None,
        };

        // What an unanswered setup question emits.
        assert_eq!(
            validate_provider_config(base(Some(""), Some(""))).unwrap_err(),
            "invalid config: route or tenant_channel_id required".to_string()
        );
        assert_eq!(
            validate_provider_config(base(Some("   "), None)).unwrap_err(),
            "invalid config: route or tenant_channel_id required".to_string()
        );

        // Either one alone still satisfies it, and a blank sibling is dropped
        // rather than beating the answered one in `route.or(tenant_channel_id)`.
        let cfg = validate_provider_config(base(Some(""), Some("tenant:channel"))).unwrap();
        assert_eq!(cfg.route, None);
        assert_eq!(cfg.tenant_channel_id.as_deref(), Some("tenant:channel"));
    }

    #[test]
    fn load_config_supports_nested_and_top_level_inputs() {
        let nested = load_config(&json!({
            "config": {
                "public_base_url": "https://example.com",
                "mode": "websocket",
                "tenant_channel_id": "tenant:channel"
            }
        }))
        .expect("nested config");
        assert_eq!(nested.mode, "websocket");
        assert_eq!(nested.tenant_channel_id.as_deref(), Some("tenant:channel"));

        let top_level = load_config(&json!({
            "public_base_url": "https://example.com",
            "mode": "local_queue",
            "route": "route-b"
        }))
        .expect("top-level config");
        assert_eq!(top_level.route.as_deref(), Some("route-b"));

        let injected = load_config(&serde_json::json!({
            "public_base_url_b64": general_purpose::STANDARD.encode("https://example.com"),
            "mode_b64": general_purpose::STANDARD.encode("websocket"),
            "tenant_channel_id_b64": general_purpose::STANDARD.encode("demo:webchat")
        }))
        .expect("injected config");
        assert_eq!(injected.mode, "websocket");
        assert_eq!(injected.public_base_url, "https://example.com");
        assert_eq!(injected.tenant_channel_id.as_deref(), Some("demo:webchat"));
    }

    #[test]
    fn load_config_requires_any_config_shape() {
        assert_eq!(
            load_config(&json!({})).unwrap_err(),
            "config required".to_string()
        );
    }
}
