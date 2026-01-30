use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use greentic_types::{ProviderManifest, provider::PROVIDER_EXTENSION_ID};
use serde_json::{Value, json};
use wasmtime::component::{
    Component, ComponentExportIndex, HasSelf, Linker, ResourceTable, TypedFunc,
};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../../components/messaging-provider-webchat/wit/messaging-provider-webchat",
        world: "messaging-provider-webchat",
    });
}

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
        root.join("target/components/messaging-provider-webchat.wasm"),
        root.join("target/wasm32-wasip2/release/messaging_provider_webchat.wasm"),
        root.join("target/wasm32-wasip2/wasm32-wasip2/release/messaging_provider_webchat.wasm"),
        root.join("components/messaging-provider-webchat/target/wasm32-wasip2/release/messaging_provider_webchat.wasm"),
        root.join("packs/messaging-webchat/components/messaging-provider-webchat.wasm"),
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
            "messaging-provider-webchat",
        ])
        .current_dir(workspace_root())
        .status()
        .context("running cargo component build for messaging-provider-webchat")?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "cargo component build failed with status {status}"
        ));
    }

    for path in candidate_artifacts() {
        if path.exists() {
            let target = workspace_root().join("target/components/messaging-provider-webchat.wasm");
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

#[derive(Default)]
struct HostState {
    table: ResourceTable,
    wasi_ctx: WasiCtx,
    writes: RefCell<HashMap<String, Vec<u8>>>,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.table,
        }
    }
}

impl HostState {
    fn new() -> Self {
        Self {
            table: ResourceTable::new(),
            wasi_ctx: WasiCtxBuilder::new().inherit_stdio().build(),
            writes: RefCell::new(HashMap::new()),
        }
    }
}

impl bindings::greentic::state::state_store::Host for HostState {
    fn read(
        &mut self,
        _key: bindings::greentic::interfaces_types::types::StateKey,
        _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
    ) -> Result<Vec<u8>, bindings::greentic::state::state_store::HostError> {
        Err(bindings::greentic::state::state_store::HostError {
            code: "unimplemented".into(),
            message: "read not supported".into(),
        })
    }

    fn write(
        &mut self,
        key: bindings::greentic::interfaces_types::types::StateKey,
        bytes: Vec<u8>,
        _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
    ) -> Result<
        bindings::greentic::state::state_store::OpAck,
        bindings::greentic::state::state_store::HostError,
    > {
        self.writes.borrow_mut().insert(key, bytes);
        Ok(bindings::greentic::state::state_store::OpAck::Ok)
    }

    fn delete(
        &mut self,
        _key: bindings::greentic::interfaces_types::types::StateKey,
        _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
    ) -> Result<
        bindings::greentic::state::state_store::OpAck,
        bindings::greentic::state::state_store::HostError,
    > {
        Ok(bindings::greentic::state::state_store::OpAck::Ok)
    }
}

impl bindings::greentic::interfaces_types::types::Host for HostState {}

impl bindings::greentic::secrets_store::secrets_store::Host for HostState {
    fn get(
        &mut self,
        _key: String,
    ) -> Result<Option<Vec<u8>>, bindings::greentic::secrets_store::secrets_store::SecretsError>
    {
        Ok(None)
    }
}

fn add_wasi_to_linker(linker: &mut Linker<HostState>) {
    wasmtime_wasi::p2::add_to_linker_sync(linker).expect("add wasi");
}

#[test]
fn builds_webchat_component() -> Result<()> {
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
    let pack_dir = workspace_root().join("packs/messaging-webchat");
    let manifest_path = pack_dir.join("pack.manifest.json");
    let manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).context("reading pack.manifest.json")?)
            .context("parsing pack.manifest.json")?;

    let provider_ext = manifest
        .get("extensions")
        .and_then(|ext| ext.get(PROVIDER_EXTENSION_ID))
        .expect("pack should include provider extension");
    assert_eq!(
        provider_ext
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        PROVIDER_EXTENSION_ID
    );

    let providers = provider_ext
        .get("inline")
        .and_then(|inline| inline.get("providers"))
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(providers.len(), 1, "expected one provider entry");
    let provider = providers.first().expect("provider entry");
    assert_eq!(
        provider.get("provider_type").and_then(|v| v.as_str()),
        Some("messaging.webchat")
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
    assert_eq!(
        schema_ref,
        "schemas/messaging/webchat/public.config.schema.json"
    );
    assert!(
        pack_dir.join(schema_ref).exists(),
        "pack schema should exist"
    );
    assert!(
        workspace_root()
            .join("schemas/messaging/webchat/public.config.schema.json")
            .exists(),
        "workspace schema should exist"
    );
    Ok(())
}

#[test]
fn invoke_send_and_ingest_smoke_test() -> Result<()> {
    let component_path = ensure_component_artifact()?;
    let engine = new_engine();
    let component = Component::from_file(&engine, &component_path).context("loading component")?;
    let mut linker = Linker::new(&engine);
    add_wasi_to_linker(&mut linker);
    bindings::greentic::state::state_store::add_to_linker::<HostState, HasSelf<HostState>>(
        &mut linker,
        |state: &mut HostState| state,
    )
    .expect("link state");
    bindings::greentic::secrets_store::secrets_store::add_to_linker::<HostState, HasSelf<HostState>>(
        &mut linker,
        |state: &mut HostState| state,
    )
    .expect("link secrets");
    bindings::greentic::interfaces_types::types::add_to_linker::<HostState, HasSelf<HostState>>(
        &mut linker,
        |state: &mut HostState| state,
    )
    .expect("link interfaces types");

    let mut describe_store = Store::new(&engine, HostState::new());
    let instance = linker
        .instantiate(&mut describe_store, &component)
        .context("instantiate for describe")?;

    let api_index: ComponentExportIndex = instance
        .get_export_index(
            &mut describe_store,
            None,
            "greentic:provider-schema-core/schema-core-api@1.0.0",
        )
        .context("get schema-core-api export index")?;

    let describe_index = instance
        .get_export_index(&mut describe_store, Some(&api_index), "describe")
        .context("get describe export index")?;
    let describe: TypedFunc<(), (Vec<u8>,)> = instance
        .get_typed_func(&mut describe_store, describe_index)
        .context("get describe func")?;
    let (described,) = describe
        .call(&mut describe_store, ())
        .context("call describe")?;
    let manifest: ProviderManifest =
        serde_json::from_slice(&described).context("decode describe output")?;
    assert_eq!(manifest.provider_type, "messaging.webchat");
    assert!(manifest.ops.contains(&"send".to_string()));

    drop(describe_store);
    let mut store = Store::new(&engine, HostState::new());
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
        .get_typed_func(&mut store, invoke_index)
        .context("get invoke func")?;

    let input = json!({
        "route": "chat:abc",
        "text": "hello webchat",
        "mode": "local_queue",
        "config": {
            "route": "chat:abc",
            "public_base_url": "https://example.invalid"
        }
    });
    let input_bytes = serde_json::to_vec(&input)?;
    let (resp,) = invoke
        .call(&mut store, ("send".to_string(), input_bytes))
        .context("call invoke send")?;
    let resp_json: Value = serde_json::from_slice(&resp).context("parse invoke output")?;
    assert_eq!(resp_json.get("status"), Some(&Value::String("sent".into())));
    assert_eq!(
        resp_json.get("provider_type"),
        Some(&Value::String("messaging.webchat".into()))
    );

    {
        let writes = store.data().writes.borrow();
        assert!(writes.contains_key("chat:abc"));
    }

    let _ = invoke;
    let _ = instance;
    drop(store);

    let mut store = Store::new(&engine, HostState::new());
    let instance = linker
        .instantiate(&mut store, &component)
        .context("instantiate for ingest")?;
    let api_index: ComponentExportIndex = instance
        .get_export_index(
            &mut store,
            None,
            "greentic:provider-schema-core/schema-core-api@1.0.0",
        )
        .context("get schema-core-api export index for ingest")?;
    let invoke_index = instance
        .get_export_index(&mut store, Some(&api_index), "invoke")
        .context("get invoke export index for ingest")?;
    let invoke: TypedFunc<(String, Vec<u8>), (Vec<u8>,)> = instance
        .get_typed_func(&mut store, invoke_index)
        .context("get invoke func for ingest")?;

    let ingest_input = json!({"user_id":"u1","text":"hi"});
    let (ingest_resp,) = invoke
        .call(
            &mut store,
            ("ingest".to_string(), serde_json::to_vec(&ingest_input)?),
        )
        .context("call invoke ingest")?;
    let ingest_json: Value = serde_json::from_slice(&ingest_resp).context("parse ingest output")?;
    assert_eq!(ingest_json.get("ok"), Some(&Value::Bool(true)));

    Ok(())
}
