use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::values::Values;

#[derive(Debug, Deserialize)]
pub struct Requirements {
    #[allow(dead_code)]
    pub provider: String,
    #[serde(default)]
    pub config: RequirementGroup,
    #[serde(default)]
    pub secrets: RequirementGroup,
    #[serde(default)]
    pub to: ToRequirement,
    #[serde(default)]
    pub values: Option<Values>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RequirementGroup {
    #[serde(default)]
    pub required: Vec<FieldRequirement>,
}

#[derive(Debug, Deserialize)]
pub struct FieldRequirement {
    pub key: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub r#type: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub example: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ToRequirement {
    #[serde(default)]
    pub shape: Map<String, Value>,
}

#[derive(Debug, Default)]
pub struct ValidationReport {
    pub missing_config: Vec<String>,
    pub missing_secrets: Vec<String>,
    pub missing_to: Vec<String>,
}

impl ValidationReport {
    pub fn is_empty(&self) -> bool {
        self.missing_config.is_empty()
            && self.missing_secrets.is_empty()
            && self.missing_to.is_empty()
    }
}

impl Requirements {
    pub fn path(provider: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("providers")
            .join(format!("{provider}.requirements.json"))
    }

    pub fn load(provider: &str) -> Result<Self> {
        Ok(Self::load_with_raw(provider)?.0)
    }

    pub fn load_with_raw(provider: &str) -> Result<(Self, Value)> {
        let path = Self::path(provider);
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read requirements {}", path.display()))?;
        let raw: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse requirements {}", path.display()))?;
        let req: Requirements = serde_json::from_value(raw.clone())
            .with_context(|| format!("failed to deserialize requirements {}", path.display()))?;
        Ok((req, raw))
    }

    pub fn validate(&self, values: &Values) -> ValidationReport {
        let mut report = ValidationReport::default();
        for field in &self.config.required {
            if !values.config.contains_key(&field.key) {
                report.missing_config.push(field.key.clone());
            }
        }
        for field in &self.secrets.required {
            if !values.secrets.contains_key(&field.key) {
                report.missing_secrets.push(field.key.clone());
            }
        }
        for key in self.to.shape.keys() {
            if !values.to.contains_key(key) {
                report.missing_to.push(key.clone());
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn loads_provider_requirements() {
        let req = Requirements::load("telegram")
            .expect("should load telegram requirements from providers");
        assert_eq!(req.provider, "telegram");
        assert_eq!(req.config.required.len(), 1);
    }

    #[test]
    fn validation_reports_missing_values() {
        let mut values = Values {
            config: Map::new(),
            secrets: Map::new(),
            to: Map::new(),
            http: None,
            state: Map::new(),
        };
        values.config.insert(
            "api_base".to_string(),
            Value::String("https://api".to_string()),
        );
        let req = Requirements {
            provider: "test".to_string(),
            config: RequirementGroup {
                required: vec![FieldRequirement {
                    key: "api_base".to_string(),
                    r#type: None,
                    example: None,
                }],
            },
            secrets: RequirementGroup {
                required: vec![FieldRequirement {
                    key: "SOME_SECRET".to_string(),
                    r#type: None,
                    example: None,
                }],
            },
            to: ToRequirement {
                shape: {
                    let mut map = Map::new();
                    map.insert("chat_id".to_string(), Value::String("1".to_string()));
                    map
                },
            },
            values: None,
        };
        let report = req.validate(&values);
        assert!(report.missing_secrets.contains(&"SOME_SECRET".to_string()));
        assert!(report.missing_to.contains(&"chat_id".to_string()));
        assert!(report.missing_config.is_empty());
    }
}
