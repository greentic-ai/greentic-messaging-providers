use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use greentic_types::ProviderManifest;
use serde_json::{Value, json};
use wasmtime::component::{Component, ComponentExportIndex, Linker, ResourceTable, TypedFunc};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn candidate_artifacts() -> Vec<PathBuf> {
    let root = workspace_root();
    vec![
        root.join("target/components/messaging-provider-dummy.wasm"),
        root.join("target/wasm32-wasip2/release/messaging_provider_dummy.wasm"),
        root.join("target/wasm32-wasip2/wasm32-wasip2/release/messaging_provider_dummy.wasm"),
        root.join("components/messaging-provider-dummy/target/wasm32-wasip2/release/messaging_provider_dummy.wasm"),
        root.join("packs/messaging-dummy/components/messaging-provider-dummy.wasm"),
    ]
}

fn ensure_component_artifact() -> Result<PathBuf> {
    for path in candidate_artifacts() {
        if path.exists() {
            return Ok(path);
        }
    }

    let status = Command::new("cargo")
        .args([
            "component",
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--package",
            "messaging-provider-dummy",
        ])
        .current_dir(workspace_root())
        .status()
        .context("running cargo component build for messaging-provider-dummy")?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "cargo component build failed with status {status}"
        ));
    }

    for path in candidate_artifacts() {
        if path.exists() {
            // Mirror into target/components for consistency with build script.
            let target = workspace_root().join("target/components/messaging-provider-dummy.wasm");
            if !target.exists() {
                if let Some(dir) = target.parent() {
                    let _ = fs::create_dir_all(dir);
                }
                let _ = fs::copy(&path, &target);
            }
            return Ok(path);
        }
    }

    Err(anyhow::anyhow!(
        "component artifact missing after cargo component build"
    ))
}

fn new_engine() -> Engine {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.cache(None);
    Engine::new(&config).expect("engine")
}

struct HostState {
    table: ResourceTable,
    wasi_ctx: WasiCtx,
}

impl HostState {
    fn new() -> Self {
        Self {
            table: ResourceTable::new(),
            wasi_ctx: WasiCtxBuilder::new().inherit_stdio().build(),
        }
    }
}

impl Default for HostState {
    fn default() -> Self {
        HostState::new()
    }
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.table,
        }
    }
}

fn add_wasi_to_linker(linker: &mut Linker<HostState>) {
    wasmtime_wasi::p2::add_to_linker_sync(linker).expect("add wasi");
}

fn invoke_send(
    engine: &Engine,
    component: &Component,
    linker: &Linker<HostState>,
    input_bytes: Vec<u8>,
) -> Result<Value> {
    let mut store = Store::new(engine, HostState::default());
    let instance = linker
        .instantiate(&mut store, component)
        .context("instantiate for invoke")?;
    let api_index: ComponentExportIndex = instance
        .get_export_index(
            &mut store,
            None,
            "greentic:provider-schema-core/schema-core-api@1.0.0",
        )
        .context("get schema-core-api export index for invoke")?;
    let invoke_index = instance
        .get_export_index(&mut store, Some(&api_index), "invoke")
        .context("get invoke export index")?;
    let invoke: TypedFunc<(String, Vec<u8>), (Vec<u8>,)> = instance
        .get_typed_func(&mut store, &invoke_index)
        .context("get invoke func")?;
    let (bytes,) = invoke
        .call(&mut store, ("send".to_string(), input_bytes))
        .context("call invoke send")?;
    let json: Value = serde_json::from_slice(&bytes).context("parse invoke output")?;
    Ok(json)
}

#[test]
fn builds_dummy_component() -> Result<()> {
    let path = ensure_component_artifact()?;
    assert!(
        path.exists(),
        "component artifact should exist at {:?}",
        path
    );
    Ok(())
}

#[test]
fn pack_has_extension_and_schema() -> Result<()> {
    let pack_dir = workspace_root().join("packs/messaging-dummy");
    let manifest_path = pack_dir.join("pack.manifest.json");
    let manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).context("reading pack.manifest.json")?)
            .context("parsing pack.manifest.json")?;

    let providers = manifest
        .get("extensions")
        .and_then(|ext| ext.get("greentic.ext.provider"))
        .and_then(|ext| ext.get("inline"))
        .and_then(|inline| inline.get("providers"))
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(providers.len(), 1, "expected one provider entry");
    let provider = providers.first().expect("provider entry");
    assert_eq!(
        provider.get("provider_type").and_then(|v| v.as_str()),
        Some("messaging.dummy")
    );
    assert_eq!(
        provider
            .get("runtime")
            .and_then(|r| r.get("world"))
            .and_then(|v| v.as_str()),
        Some("greentic:provider/schema-core@1.0.0")
    );

    let schema_ref = provider
        .get("config_schema_ref")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_eq!(schema_ref, "schemas/messaging/dummy/config.schema.json");
    assert!(
        pack_dir.join(schema_ref).exists(),
        "pack schema should exist"
    );
    assert!(
        workspace_root()
            .join("schemas/messaging/dummy/config.schema.json")
            .exists(),
        "workspace schema should exist"
    );
    Ok(())
}

#[test]
fn invoke_send_smoke_test() -> Result<()> {
    let component_path = ensure_component_artifact()?;
    let engine = new_engine();
    let component = Component::from_file(&engine, &component_path).context("loading component")?;
    let mut linker = Linker::new(&engine);
    add_wasi_to_linker(&mut linker);
    let mut store = Store::new(&engine, HostState::default());
    let instance = linker
        .instantiate(&mut store, &component)
        .context("instantiate for describe")?;

    let api_index: ComponentExportIndex = instance
        .get_export_index(
            &mut store,
            None,
            "greentic:provider-schema-core/schema-core-api@1.0.0",
        )
        .context("get schema-core-api export index")?;

    let describe_index = instance
        .get_export_index(&mut store, Some(&api_index), "describe")
        .context("get describe export index")?;
    let describe: TypedFunc<(), (Vec<u8>,)> = instance
        .get_typed_func(&mut store, &describe_index)
        .context("get describe func")?;
    let (described,) = describe.call(&mut store, ()).context("call describe")?;
    let manifest: ProviderManifest =
        serde_json::from_slice(&described).context("decode describe output")?;
    assert_eq!(manifest.provider_type, "messaging.dummy");
    assert!(manifest.ops.contains(&"send".to_string()));

    // Fresh store/instance to exercise invoke without re-entry surprises.
    drop(store);
    let mut store = Store::new(&engine, HostState::default());
    let instance = linker
        .instantiate(&mut store, &component)
        .context("instantiate for invoke")?;

    let api_index: ComponentExportIndex = instance
        .get_export_index(
            &mut store,
            None,
            "greentic:provider-schema-core/schema-core-api@1.0.0",
        )
        .context("get schema-core-api export index for invoke")?;
    let invoke_index = instance
        .get_export_index(&mut store, Some(&api_index), "invoke")
        .context("get invoke export index")?;
    let invoke: TypedFunc<(String, Vec<u8>), (Vec<u8>,)> = instance
        .get_typed_func(&mut store, &invoke_index)
        .context("get invoke func")?;
    let input = json!({"text":"hello"});
    let input_bytes = serde_json::to_vec(&input)?;
    let (first,) = invoke
        .call(&mut store, ("send".to_string(), input_bytes.clone()))
        .context("call invoke send")?;
    let first_json: Value = serde_json::from_slice(&first).context("parse first invoke")?;

    // Re-instantiate to check determinism without re-entry traps.
    let second_json = invoke_send(&engine, &component, &linker, serde_json::to_vec(&input)?)?;
    assert_eq!(
        first_json.get("status"),
        Some(&Value::String("sent".into()))
    );
    assert_eq!(
        first_json.get("provider_type"),
        Some(&Value::String("messaging.dummy".into()))
    );

    let first_id = first_json
        .get("message_id")
        .and_then(|v| v.as_str())
        .context("missing message_id")?;
    let second_id = second_json
        .get("message_id")
        .and_then(|v| v.as_str())
        .context("missing second message_id")?;
    assert_eq!(first_id, second_id, "deterministic message id expected");

    Ok(())
}
