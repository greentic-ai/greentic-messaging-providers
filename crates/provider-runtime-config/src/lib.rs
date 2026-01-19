use serde::{Deserialize, Serialize};

pub const PROVIDER_RUNTIME_CONFIG_SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 {
    PROVIDER_RUNTIME_CONFIG_SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderRuntimeConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
}

impl Default for ProviderRuntimeConfig {
    fn default() -> Self {
        Self {
            schema_version: PROVIDER_RUNTIME_CONFIG_SCHEMA_VERSION,
            telemetry: TelemetryConfig::default(),
            network: NetworkConfig::default(),
            runtime: RuntimeConfig::default(),
        }
    }
}

impl ProviderRuntimeConfig {
    pub fn validate(&self) -> Result<(), ProviderRuntimeConfigError> {
        if self.schema_version != PROVIDER_RUNTIME_CONFIG_SCHEMA_VERSION {
            return Err(ProviderRuntimeConfigError::UnsupportedSchemaVersion {
                expected: PROVIDER_RUNTIME_CONFIG_SCHEMA_VERSION,
                got: self.schema_version,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub emit_enabled: bool,
    #[serde(default)]
    pub service_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    #[serde(default)]
    pub max_attempts: u32,
    #[serde(default)]
    pub proxy: ProxyMode,
    #[serde(default)]
    pub tls: TlsMode,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            proxy: ProxyMode::Inherit,
            tls: TlsMode::Strict,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyMode {
    Inherit,
    Disabled,
}

impl Default for ProxyMode {
    fn default() -> Self {
        Self::Inherit
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    Strict,
    Insecure,
}

impl Default for TlsMode {
    fn default() -> Self {
        Self::Strict
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub max_concurrency: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProviderRuntimeConfigError {
    UnsupportedSchemaVersion { expected: u32, got: u32 },
}

impl std::fmt::Display for ProviderRuntimeConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderRuntimeConfigError::UnsupportedSchemaVersion { expected, got } => write!(
                f,
                "unsupported schema version: expected {expected}, got {got}"
            ),
        }
    }
}

impl std::error::Error for ProviderRuntimeConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_defaults() {
        let cfg = ProviderRuntimeConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let decoded: ProviderRuntimeConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, cfg);
        decoded.validate().expect("validate");
    }

    #[test]
    fn rejects_unknown_fields() {
        let json = r#"{"schema_version":1,"telemetry":{"emit_enabled":true},"extra":42}"#;
        let err = serde_json::from_str::<ProviderRuntimeConfig>(json).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let cfg = ProviderRuntimeConfig {
            schema_version: 999,
            ..ProviderRuntimeConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert_eq!(
            err,
            ProviderRuntimeConfigError::UnsupportedSchemaVersion {
                expected: 1,
                got: 999
            }
        );
    }
}
