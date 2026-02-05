use anyhow::anyhow;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct SetupSpec {
    pub provider_id: String,
    pub version: u32,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub questions: Vec<QuestionDef>,
}

#[derive(Debug, Deserialize)]
pub struct QuestionDef {
    pub name: String,
    pub title: String,
    pub kind: QuestionKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub choices: Vec<Value>,
    #[serde(default)]
    pub validate: Option<QuestionValidate>,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum QuestionKind {
    String,
    Bool,
    Number,
    Choice,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct QuestionValidate {
    #[serde(default)]
    pub regex: Option<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QuestionsSpec {
    pub id: String,
    pub title: String,
    pub questions: Vec<QuestionSpecItem>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QuestionSpecItem {
    pub name: String,
    pub title: String,
    pub kind: QuestionKind,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validate: Option<QuestionValidate>,
    pub secret: bool,
}

impl TryFrom<&QuestionDef> for QuestionSpecItem {
    type Error = anyhow::Error;

    fn try_from(value: &QuestionDef) -> Result<Self, Self::Error> {
        if let Some(validate) = value.validate.as_ref()
            && let Some(regex) = validate.regex.as_ref()
        {
            Regex::new(regex).map_err(|e| anyhow!("invalid regex for {}: {e}", value.name))?;
        }
        Ok(Self {
            name: value.name.clone(),
            title: value.title.clone(),
            kind: value.kind.clone(),
            required: value.required,
            default: value.default.clone(),
            help: value.help.clone(),
            choices: value.choices.clone(),
            validate: value.validate.clone(),
            secret: value.secret,
        })
    }
}
