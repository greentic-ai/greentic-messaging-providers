#![allow(dead_code)]

pub mod spec;

use regex::Regex;
use spec::{QuestionKind, QuestionSpecItem, QuestionsSpec, SetupSpec};

use anyhow::{Result, anyhow};
use greentic_interfaces_guest::component::node::{InvokeResult, NodeError};
use greentic_interfaces_guest::component_entrypoint;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct EmitInput {
    id: String,
    spec_ref: String,
    #[serde(default)]
    context: Option<Context>,
}

#[derive(Debug, Deserialize)]
struct Context {
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    env: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ValidateInput {
    spec_json: String,
    answers_json: String,
}

#[derive(Debug, Deserialize)]
struct ExampleInput {
    spec_json: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ValidateOutput {
    ok: bool,
    errors: Vec<ValidationError>,
}

#[derive(Debug, serde::Serialize)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

component_entrypoint!({
    manifest: describe_payload,
    invoke: handle_message,
    invoke_stream: false,
});

fn describe_payload() -> String {
    serde_json::json!({
        "component": {
            "name": "questions",
            "org": "ai.greentic",
            "version": "0.1.0",
            "world": "greentic:component/component@0.5.0"
        }
    })
    .to_string()
}

fn handle_message(operation: String, input: String) -> InvokeResult {
    match operation.as_str() {
        "emit" => invoke_operation(emit(input)),
        "validate" => invoke_operation(validate(input)),
        "example-answers" => invoke_operation(example_answers(input)),
        other => InvokeResult::Err(NodeError {
            code: "UNKNOWN_OPERATION".to_string(),
            message: format!("unsupported operation {other}"),
            retryable: false,
            backoff_ms: None,
            details: None,
        }),
    }
}

fn invoke_operation(result: Result<String, String>) -> InvokeResult {
    match result {
        Ok(value) => InvokeResult::Ok(value),
        Err(message) => InvokeResult::Err(NodeError {
            code: "INVALID_INPUT".to_string(),
            message,
            retryable: false,
            backoff_ms: None,
            details: None,
        }),
    }
}

fn emit(input_json: String) -> Result<String, String> {
    let input: EmitInput = serde_json::from_str(&input_json).map_err(err_string)?;
    touch_context(&input.context);
    let spec = load_spec(&input.spec_ref).map_err(err_string)?;
    let title = spec
        .title
        .clone()
        .unwrap_or_else(|| format!("{} setup", spec.provider_id));
    let questions = spec
        .questions
        .iter()
        .map(QuestionSpecItem::try_from)
        .collect::<Result<Vec<_>>>()
        .map_err(err_string)?;
    let spec = QuestionsSpec {
        id: input.id,
        title,
        questions,
    };
    serde_json::to_string(&spec).map_err(err_string)
}

fn validate(input_json: String) -> Result<String, String> {
    let input: ValidateInput = serde_json::from_str(&input_json).map_err(err_string)?;
    let spec: QuestionsSpec = serde_json::from_str(&input.spec_json).map_err(err_string)?;
    let answers: Value = serde_json::from_str(&input.answers_json).map_err(err_string)?;
    let answer_map = answers.as_object().cloned().unwrap_or_else(Map::new);
    let errors = validate_answers_for_spec(&spec.questions, &answer_map);
    let output = ValidateOutput {
        ok: errors.is_empty(),
        errors,
    };
    serde_json::to_string(&output).map_err(err_string)
}

fn example_answers(input_json: String) -> Result<String, String> {
    let input: ExampleInput = serde_json::from_str(&input_json).map_err(err_string)?;
    let spec: QuestionsSpec = serde_json::from_str(&input.spec_json).map_err(err_string)?;
    let value = example_answers_for_spec(&spec.questions);
    serde_json::to_string(&value).map_err(err_string)
}

fn err_string(err: impl std::fmt::Display) -> String {
    format!("{err}")
}

fn touch_context(context: &Option<Context>) {
    if let Some(ctx) = context {
        let _ = (&ctx.tenant_id, &ctx.env);
    }
}

fn load_spec(spec_ref: &str) -> Result<SetupSpec> {
    let path = resolve_spec_path(spec_ref);
    let contents = fs::read_to_string(&path)
        .map_err(|e| anyhow!("failed to read spec at {}: {}", path.display(), e))?;
    let spec: SetupSpec = serde_yaml_bw::from_str(&contents)?;
    Ok(spec)
}

fn resolve_spec_path(spec_ref: &str) -> PathBuf {
    if let Some(stripped) = spec_ref.strip_prefix("assets/") {
        return PathBuf::from("/assets").join(stripped);
    }
    PathBuf::from(spec_ref)
}

pub fn validate_answers_for_spec(
    questions: &[QuestionSpecItem],
    answers: &Map<String, Value>,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    for question in questions {
        let value = answers.get(&question.name);
        if question.required && is_missing(value) {
            errors.push(ValidationError {
                path: question.name.clone(),
                message: "required".to_string(),
            });
            continue;
        }
        let Some(value) = value else { continue };
        if value.is_null() {
            continue;
        }
        match question.kind {
            QuestionKind::String => {
                let Some(text) = value.as_str() else {
                    errors.push(type_error(&question.name, "string"));
                    continue;
                };
                if let Some(validate) = question.validate.as_ref()
                    && let Some(regex) = validate.regex.as_ref()
                    && let Ok(pattern) = Regex::new(regex)
                    && !pattern.is_match(text)
                {
                    errors.push(ValidationError {
                        path: question.name.clone(),
                        message: "regex".to_string(),
                    });
                }
                if !question.choices.is_empty()
                    && !question.choices.iter().any(|choice| choice == value)
                {
                    errors.push(ValidationError {
                        path: question.name.clone(),
                        message: "choice".to_string(),
                    });
                }
            }
            QuestionKind::Bool => {
                if !value.is_boolean() {
                    errors.push(type_error(&question.name, "bool"));
                }
            }
            QuestionKind::Number => {
                let Some(num) = value.as_f64() else {
                    errors.push(type_error(&question.name, "number"));
                    continue;
                };
                if let Some(validate) = question.validate.as_ref() {
                    if let Some(min) = validate.min
                        && num < min
                    {
                        errors.push(ValidationError {
                            path: question.name.clone(),
                            message: "min".to_string(),
                        });
                    }
                    if let Some(max) = validate.max
                        && num > max
                    {
                        errors.push(ValidationError {
                            path: question.name.clone(),
                            message: "max".to_string(),
                        });
                    }
                }
            }
            QuestionKind::Choice => {
                if question.choices.is_empty() {
                    if !value.is_string() {
                        errors.push(type_error(&question.name, "string"));
                    }
                } else if !question.choices.iter().any(|choice| choice == value) {
                    errors.push(ValidationError {
                        path: question.name.clone(),
                        message: "choice".to_string(),
                    });
                }
            }
        }
    }
    errors
}

pub fn example_answers_for_spec(questions: &[QuestionSpecItem]) -> Value {
    let mut out = Map::new();
    for question in questions {
        let value = if let Some(default) = question.default.clone() {
            default
        } else {
            match question.kind {
                QuestionKind::String => Value::String(String::new()),
                QuestionKind::Bool => Value::Bool(false),
                QuestionKind::Number => Value::Number(0.into()),
                QuestionKind::Choice => question
                    .choices
                    .first()
                    .cloned()
                    .unwrap_or_else(|| Value::String(String::new())),
            }
        };
        out.insert(question.name.clone(), value);
    }
    Value::Object(out)
}

fn is_missing(value: Option<&Value>) -> bool {
    match value {
        None => true,
        Some(Value::Null) => true,
        Some(Value::String(s)) if s.trim().is_empty() => true,
        _ => false,
    }
}

fn type_error(path: &str, expected: &str) -> ValidationError {
    ValidationError {
        path: path.to_string(),
        message: format!("expected {expected}"),
    }
}
