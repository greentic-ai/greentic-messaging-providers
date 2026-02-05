use std::path::Path;

use anyhow::{Context, Result, anyhow};
use glob::glob;
use serde_yaml_bw::Value;

fn load_flow(path: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_yaml_bw::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn get_mapping<'a>(value: &'a Value, label: &str) -> Result<&'a serde_yaml_bw::Mapping> {
    value
        .as_mapping()
        .ok_or_else(|| anyhow!("{label} is not a mapping"))
}

fn flow_root(value: &Value) -> Result<&serde_yaml_bw::Mapping> {
    let map = get_mapping(value, "flow")?;
    if let Some(root) = map.get(Value::String("flow".to_string(), None)) {
        return get_mapping(root, "flow");
    }
    Ok(map)
}

fn map_get<'a>(map: &'a serde_yaml_bw::Mapping, key: &str) -> Result<&'a Value> {
    map.get(Value::String(key.to_string(), None))
        .ok_or_else(|| anyhow!("missing key {key}"))
}

#[test]
fn setup_flows_use_questions_emit_and_validate() -> Result<()> {
    for entry in glob("packs/*/flows/setup_*.ygtc")? {
        let path = entry?;
        let flow = load_flow(&path)?;
        let flow_root = flow_root(&flow)?;
        let nodes = map_get(flow_root, "nodes")?;
        let nodes = get_mapping(nodes, "nodes")?;

        let prefix = if path.file_name().and_then(|v| v.to_str()) == Some("setup_default.ygtc") {
            "setup_default"
        } else {
            "setup_custom"
        };
        let emit_id = format!("{prefix}__emit_questions");
        let validate_id = format!("{prefix}__validate");

        let entry_node = flow_root
            .get(Value::String("in".to_string(), None))
            .or_else(|| flow_root.get(Value::String("start".to_string(), None)))
            .ok_or_else(|| anyhow!("missing flow entry"))?
            .as_str()
            .ok_or_else(|| anyhow!("flow entry is not a string"))?;
        if entry_node != emit_id {
            return Err(anyhow!(
                "{} entry expected {emit_id}, got {entry_node}",
                path.display()
            ));
        }

        let emit_node = map_get(nodes, &emit_id)?;
        let emit_node = get_mapping(emit_node, &emit_id)?;
        if !emit_node.contains_key(Value::String("emit".to_string(), None)) {
            return Err(anyhow!("{} missing emit op", path.display()));
        }

        let validate_node = map_get(nodes, &validate_id)?;
        let validate_node = get_mapping(validate_node, &validate_id)?;
        let validate = map_get(validate_node, "validate")?;
        let validate = get_mapping(validate, "validate")?;
        let spec_json = map_get(validate, "spec_json")?
            .as_str()
            .ok_or_else(|| anyhow!("spec_json must be string"))?;
        let answers_json = map_get(validate, "answers_json")?
            .as_str()
            .ok_or_else(|| anyhow!("answers_json must be string"))?;

        if spec_json != format!("{{{{ node.{emit_id} }}}}") {
            return Err(anyhow!(
                "{} spec_json template mismatch: {spec_json}",
                path.display()
            ));
        }
        if answers_json != "{{ state.input.answers_json }}" {
            return Err(anyhow!(
                "{} answers_json template mismatch: {answers_json}",
                path.display()
            ));
        }
    }

    Ok(())
}
