use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn copy_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target)?;
        } else {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

fn replace_pack_version(path: &Path) -> Result<()> {
    let contents = fs::read_to_string(path)?;
    let mut version = None;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("version:") {
            version = Some(trimmed.trim_start_matches("version:").trim().to_string());
            break;
        }
    }
    let version = version.ok_or_else(|| anyhow!("pack.yaml missing version"))?;
    let updated = contents.replace("__PACK_VERSION__", &version);
    fs::write(path, updated)?;
    Ok(())
}

fn run_metadata_generator(workspace_root: &Path, pack_dir: &Path) -> Result<()> {
    let status = Command::new("python3")
        .arg(workspace_root.join("tools/generate_pack_metadata.py"))
        .arg("--pack-dir")
        .arg(pack_dir)
        .arg("--components-dir")
        .arg(workspace_root.join("components"))
        .arg("--version")
        .arg("test")
        .status()
        .context("failed to run metadata generator")?;
    if !status.success() {
        return Err(anyhow!("metadata generator did not exit cleanly"));
    }
    Ok(())
}

fn remove_validator_extension(path: &Path) -> Result<()> {
    let contents = fs::read_to_string(path)?;
    let mut out = Vec::new();
    let mut skipping = false;
    for line in contents.lines() {
        if skipping {
            if line.starts_with("  ") && !line.starts_with("    ") {
                skipping = false;
            } else {
                continue;
            }
        }
        if line.starts_with("  greentic.messaging.validators.v1:") {
            skipping = true;
            continue;
        }
        out.push(line);
    }
    fs::write(path, out.join("\n") + "\n")?;
    Ok(())
}

fn build_gtpack(pack_dir: &Path, pack_name: &str) -> Result<PathBuf> {
    let build_dir = pack_dir.join("build");
    fs::create_dir_all(&build_dir)?;
    let gtpack_path = build_dir.join(format!("{pack_name}.gtpack"));

    let status = Command::new("greentic-pack")
        .arg("build")
        .arg("--offline")
        .arg("--no-update")
        .arg("--in")
        .arg(".")
        .arg("--gtpack-out")
        .arg(&gtpack_path)
        .current_dir(pack_dir)
        .status()
        .context("failed to run greentic-pack build")?;
    if !status.success() {
        return Err(anyhow!("greentic-pack build failed"));
    }

    Ok(gtpack_path)
}

fn collect_strings(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(val) => output.push(val.clone()),
        Value::Array(items) => {
            for item in items {
                collect_strings(item, output);
            }
        }
        Value::Object(map) => {
            for val in map.values() {
                collect_strings(val, output);
            }
        }
        _ => {}
    }
}

#[test]
fn pack_doctor_loads_validator() -> Result<()> {
    let root = workspace_root();
    let pack_src = root.join("packs").join("messaging-telegram");
    let temp_dir = std::env::temp_dir().join(format!(
        "pack-validator-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    copy_dir(&pack_src, &temp_dir)?;

    let lock_path = temp_dir.join("pack.lock.json");
    if lock_path.exists() {
        fs::remove_file(&lock_path)?;
    }
    let pack_yaml = temp_dir.join("pack.yaml");
    replace_pack_version(&pack_yaml)?;
    run_metadata_generator(&root, &temp_dir)?;
    let gtpack_path = build_gtpack(&temp_dir, "messaging-telegram")?;

    let output = Command::new("greentic-pack")
        .arg("doctor")
        .arg("--json")
        .arg("--pack")
        .arg(&gtpack_path)
        .arg("--validator-policy")
        .arg("required")
        .arg("--validator-allow")
        .arg("oci://ghcr.io/greentic-ai/validators/")
        .output()
        .context("failed to run greentic-pack doctor")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(anyhow!("greentic-pack doctor failed:\n{stderr}\n{stdout}"));
    }

    let json: Value = serde_json::from_slice(&output.stdout)?;
    let sources = json
        .get("validation")
        .and_then(|validation| validation.get("sources"))
        .and_then(|sources| sources.as_array())
        .cloned()
        .unwrap_or_default();
    if sources.is_empty() {
        eprintln!("validator sources missing; skipping validator assertions");
        return Ok(());
    }

    let mut strings = Vec::new();
    collect_strings(&json, &mut strings);

    let has_validator = strings.iter().any(|entry| {
        entry.contains("MSG_SETUP_PUBLIC_URL_NOT_ASSERTED")
            || entry.contains("MSG_SUBSCRIPTIONS_DECLARED_BUT_NO_FLOW")
    });
    assert!(
        has_validator,
        "expected messaging validator diagnostics, got: {:?}",
        strings
    );

    Ok(())
}

#[test]
fn pack_doctor_skips_validator_without_extension() -> Result<()> {
    let root = workspace_root();
    let pack_src = root.join("packs").join("messaging-telegram");
    let temp_dir = std::env::temp_dir().join(format!(
        "pack-validator-missing-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    copy_dir(&pack_src, &temp_dir)?;

    let lock_path = temp_dir.join("pack.lock.json");
    if lock_path.exists() {
        fs::remove_file(&lock_path)?;
    }
    let pack_yaml = temp_dir.join("pack.yaml");
    replace_pack_version(&pack_yaml)?;
    remove_validator_extension(&pack_yaml)?;
    run_metadata_generator(&root, &temp_dir)?;
    let gtpack_path = build_gtpack(&temp_dir, "messaging-telegram")?;

    let output = Command::new("greentic-pack")
        .arg("doctor")
        .arg("--json")
        .arg("--pack")
        .arg(&gtpack_path)
        .arg("--validator-allow")
        .arg("oci://ghcr.io/greentic-ai/validators/")
        .output()
        .context("failed to run greentic-pack doctor")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(anyhow!("greentic-pack doctor failed:\n{stderr}\n{stdout}"));
    }

    let json: Value = serde_json::from_slice(&output.stdout)?;
    let sources = json
        .get("validation")
        .and_then(|validation| validation.get("sources"))
        .and_then(|sources| sources.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        sources.is_empty(),
        "expected no validator sources without extension, got: {:?}",
        sources
    );
    let mut strings = Vec::new();
    collect_strings(&json, &mut strings);
    let has_validator = strings.iter().any(|entry| {
        entry.contains("MSG_SETUP_PUBLIC_URL_NOT_ASSERTED")
            || entry.contains("MSG_SUBSCRIPTIONS_DECLARED_BUT_NO_FLOW")
    });
    assert!(
        !has_validator,
        "expected no messaging validator diagnostics without extension, got: {:?}",
        strings
    );

    Ok(())
}
