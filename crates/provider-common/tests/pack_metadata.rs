use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, anyhow};
use serde_json::{Map, Value};
use tempfile::tempdir;

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

fn build_gtpack(src_dir: &Path, output: &Path) -> Result<()> {
    let file = fs::File::create(output)?;
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    let mut stack = vec![src_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path.strip_prefix(src_dir).expect("relative path");
            let mut contents = Vec::new();
            fs::File::open(&path)?.read_to_end(&mut contents)?;
            zip.start_file(rel.to_string_lossy(), options)?;
            zip.write_all(&contents)?;
        }
    }

    zip.finish()?;
    Ok(())
}

fn strip_entry_from_gtpack(src: &Path, dest: &Path, excluded: &str) -> Result<()> {
    let archive = fs::File::open(src)?;
    let mut zip = zip::ZipArchive::new(archive)?;
    let file = fs::File::create(dest)?;
    let mut writer = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        if name == excluded {
            continue;
        }
        if entry.is_dir() {
            writer.add_directory(name, options)?;
            continue;
        }
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents)?;
        writer.start_file(name, options)?;
        writer.write_all(&contents)?;
    }

    writer.finish()?;
    Ok(())
}

fn read_from_gtpack(gtpack: &Path, file: &str) -> Result<Vec<u8>> {
    let archive = fs::File::open(gtpack)?;
    let mut zip = zip::ZipArchive::new(archive)?;
    let mut file = zip.by_name(file)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

fn run_metadata_generator(workspace_root: &Path, pack_dir: &Path) {
    let status = Command::new("python3")
        .arg(workspace_root.join("tools/generate_pack_metadata.py"))
        .arg("--pack-dir")
        .arg(pack_dir)
        .arg("--components-dir")
        .arg(workspace_root.join("components"))
        .arg("--include-capabilities-cache")
        .arg("--version")
        .arg("test")
        .status()
        .expect("failed to run metadata generator");
    assert!(status.success(), "metadata generator did not exit cleanly");
}

fn use_online_pack_build() -> bool {
    matches!(
        std::env::var("GREENTIC_PACK_TEST_ONLINE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    )
}

fn build_dummy_pack() -> Result<(tempfile::TempDir, PathBuf)> {
    let root = workspace_root();
    let temp = tempdir()?;
    let pack_src = root.join("packs").join("messaging-dummy");
    let pack_dir = temp.path().join("messaging-dummy");
    copy_dir(&pack_src, &pack_dir)?;

    let secrets_path = pack_dir.join("assets").join("secret-requirements.json");
    if !secrets_path.exists() {
        if let Some(parent) = secrets_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&secrets_path, b"[]\n")?;
    }
    let root_secrets_path = pack_dir.join("secret-requirements.json");
    if !root_secrets_path.exists() {
        fs::copy(&secrets_path, &root_secrets_path)?;
    }

    let lock_path = pack_dir.join("pack.lock.json");
    if lock_path.exists() {
        fs::remove_file(&lock_path)?;
    }

    let gtpack_path = pack_dir.join("build").join("messaging-dummy.gtpack");
    let mut command = Command::new("greentic-pack");
    command
        .arg("build")
        .arg("--no-update")
        .arg("--in")
        .arg(".")
        .arg("--gtpack-out")
        .arg(&gtpack_path)
        .current_dir(&pack_dir);
    if !use_online_pack_build() {
        command.arg("--offline");
    }
    let status = command.status().expect("failed to run greentic-pack build");
    assert!(
        status.success(),
        "greentic-pack build failed for dummy pack"
    );

    Ok((temp, gtpack_path))
}

fn manifest_components(manifest_path: &Path) -> Result<Vec<String>> {
    let manifest: Value = serde_json::from_slice(&fs::read(manifest_path)?)?;
    let comps = manifest
        .get("components")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("manifest missing components array"))?;
    Ok(comps
        .iter()
        .filter_map(Value::as_str)
        .map(|s| s.to_string())
        .collect())
}

fn collect_expected_requirements(
    components_dir: &Path,
    component_names: &[String],
) -> Result<BTreeMap<(String, String), Map<String, Value>>> {
    let mut merged: BTreeMap<(String, String), Map<String, Value>> = BTreeMap::new();
    for component in component_names {
        let manifest_path = components_dir
            .join(component)
            .join("component.manifest.json");
        if !manifest_path.exists() {
            continue;
        }
        let manifest: Value = serde_json::from_slice(&fs::read(manifest_path)?)?;
        if let Some(reqs) = manifest
            .get("secret_requirements")
            .and_then(|v| v.as_array())
        {
            for req in reqs {
                let obj = req
                    .as_object()
                    .cloned()
                    .ok_or_else(|| anyhow!("requirement must be an object"))?;
                let name = obj
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("requirement missing name"))?
                    .to_string();
                let scope = obj
                    .get("scope")
                    .and_then(Value::as_str)
                    .unwrap_or("tenant")
                    .to_string();
                merged.entry((name, scope)).or_insert(obj);
            }
        }
    }
    Ok(merged)
}

fn requirement_keys(
    requirements: &[Value],
) -> Result<BTreeMap<(String, String), Map<String, Value>>> {
    let mut merged: BTreeMap<(String, String), Map<String, Value>> = BTreeMap::new();
    for req in requirements {
        let obj = req
            .as_object()
            .cloned()
            .ok_or_else(|| anyhow!("requirement must be an object"))?;
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("requirement missing name"))?
            .to_string();
        let scope = obj
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("tenant")
            .to_string();
        merged.insert((name, scope), obj);
    }
    Ok(merged)
}

#[test]
fn gtpack_contains_secret_requirements_metadata() -> Result<()> {
    let root = workspace_root();
    let pack_source = root.join("packs").join("messaging-provider-bundle");
    let temp = tempdir()?;
    let pack_copy = temp.path().join("messaging-provider-bundle");
    copy_dir(&pack_source, &pack_copy)?;

    let pack_yaml = fs::read_to_string(pack_copy.join("pack.yaml"))?;
    assert!(
        pack_yaml.contains("x-provider-runtime-config:"),
        "pack.yaml must declare x-provider-runtime-config (host injected blob contract)"
    );
    assert!(
        pack_yaml.contains("injected_as: provider_runtime_config.json"),
        "pack.yaml must reference provider_runtime_config.json"
    );

    run_metadata_generator(&root, &pack_copy);

    let gtpack_path = temp.path().join("messaging-provider-bundle.gtpack");
    build_gtpack(&pack_copy, &gtpack_path)?;

    let manifest_bytes = read_from_gtpack(&gtpack_path, "pack.manifest.json")?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;

    let schema_version = manifest
        .get("config_schema")
        .and_then(|v| v.get("provider_runtime_config"))
        .and_then(|v| v.get("schema_version"))
        .and_then(Value::as_u64);
    assert_eq!(
        schema_version,
        Some(1),
        "pack manifest must declare config_schema.provider_runtime_config.schema_version = 1"
    );

    let requirements = manifest
        .get("secret_requirements")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("pack manifest missing secret_requirements"))?;

    assert!(
        !requirements.is_empty(),
        "secret_requirements should be populated for messaging provider bundle"
    );

    let components = manifest_components(&pack_copy.join("pack.manifest.json"))?;
    let expected = collect_expected_requirements(&root.join("components"), &components)?;
    let actual = requirement_keys(requirements)?;

    assert_eq!(
        requirements.len(),
        actual.len(),
        "secret_requirements should be deduplicated by name+scope"
    );

    let expected_keys: BTreeSet<_> = expected.keys().cloned().collect();
    let actual_keys: BTreeSet<_> = actual.keys().cloned().collect();
    assert_eq!(
        expected_keys, actual_keys,
        "secret requirement keys should match component manifests"
    );

    for key in expected_keys {
        let expected_req = expected.get(&key).unwrap();
        let actual_req = actual.get(&key).unwrap();
        assert_eq!(
            actual_req.get("description"),
            expected_req.get("description"),
            "description should be preserved for {:?}",
            key
        );
        assert_eq!(
            actual_req.get("example"),
            expected_req.get("example"),
            "example should be preserved for {:?}",
            key
        );
        for field in actual_req.keys() {
            assert!(
                matches!(field.as_str(), "name" | "scope" | "description" | "example"),
                "unexpected field {} in requirement {:?}",
                field,
                key
            );
        }
    }

    let cache = manifest
        .get("capabilities_cache")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("pack manifest missing capabilities_cache"))?;
    assert_eq!(
        cache.len(),
        components.len(),
        "capabilities_cache must contain entries for each component"
    );
    for entry in cache {
        let obj = entry
            .as_object()
            .ok_or_else(|| anyhow!("capabilities_cache entry must be object"))?;
        let component = obj
            .get("component")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("capabilities_cache entry missing component"))?;
        assert!(
            components.contains(&component.to_string()),
            "capabilities_cache component {} not in manifest components",
            component
        );
        let path = obj
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("capabilities_cache entry missing path"))?;
        let cache_bytes = read_from_gtpack(&gtpack_path, path)?;
        assert!(
            !cache_bytes.is_empty(),
            "capabilities cache file {} should be present",
            path
        );
    }

    Ok(())
}

#[test]
fn packs_lock_has_digest() -> Result<()> {
    use sha2::{Digest, Sha256};

    let root = workspace_root();
    let lock_path = root.join("packs.lock.json");
    let gtpack_path = root
        .join("dist")
        .join("packs")
        .join("messaging-provider-bundle.gtpack");
    let lock_json: Value = serde_json::from_slice(&std::fs::read(&lock_path)?)?;
    let packs = lock_json
        .get("packs")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("packs.lock.json missing packs array"))?;
    let entry = packs
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some("messaging-provider-bundle"))
        .ok_or_else(|| anyhow!("packs.lock.json missing messaging-provider-bundle entry"))?;
    let digest = entry
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("packs.lock.json missing digest"))?;
    let bytes = std::fs::read(&gtpack_path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hex = format!("{:x}", hasher.finalize());
    assert_eq!(digest, format!("sha256:{hex}"));
    Ok(())
}

#[test]
fn greentic_pack_doctor_requires_config_schema() -> Result<()> {
    let (_temp, src) = build_dummy_pack()?;

    let temp = tempdir()?;
    let broken = temp.path().join("messaging-dummy-missing-schema.gtpack");
    strip_entry_from_gtpack(&src, &broken, "schemas/messaging/dummy/config.schema.json")?;

    let output = Command::new("greentic-pack")
        .arg("doctor")
        .arg("--json")
        .arg("--pack")
        .arg(&broken)
        .output()
        .expect("failed to run greentic-pack doctor");
    assert!(
        !output.status.success(),
        "greentic-pack doctor should fail when config schema is missing"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("config schema") || combined.contains("config.schema.json"),
        "expected missing schema error, got: {combined}"
    );

    Ok(())
}

#[test]
fn dummy_pack_includes_schema_and_secret_requirements_asset() -> Result<()> {
    let (_temp, gtpack_path) = build_dummy_pack()?;

    let schema = read_from_gtpack(&gtpack_path, "schemas/messaging/dummy/config.schema.json")?;
    assert!(
        !schema.is_empty(),
        "config schema should be bundled in the dummy pack"
    );

    let secret_requirements = read_from_gtpack(&gtpack_path, "assets/secret-requirements.json")?;
    assert!(
        !secret_requirements.is_empty(),
        "secret requirements asset should be bundled in the dummy pack"
    );

    Ok(())
}
