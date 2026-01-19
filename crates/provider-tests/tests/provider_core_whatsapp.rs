use std::cell::RefCell;
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
        path: "../../components/messaging-provider-whatsapp/wit/messaging-provider-whatsapp",
        world: "messaging-provider-whatsapp",
    });
}

const TOKEN_KEY: &str = "WHATSAPP_TOKEN";

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
        root.join("target/components/messaging-provider-whatsapp.wasm"),
        root.join("target/wasm32-wasip2/release/messaging_provider_whatsapp.wasm"),
        root.join("target/wasm32-wasip2/wasm32-wasip2/release/messaging_provider_whatsapp.wasm"),
        root.join("components/messaging-provider-whatsapp/target/wasm32-wasip2/release/messaging_provider_whatsapp.wasm"),
        root.join("packs/messaging-whatsapp/components/messaging-provider-whatsapp.wasm"),
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
            "messaging-provider-whatsapp",
        ])
        .current_dir(workspace_root())
        .status()
        .context("running cargo component build for messaging-provider-whatsapp")?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "cargo component build failed with status {status}"
        ));
    }

    for path in candidate_artifacts() {
        if path.exists() {
            let target =
                workspace_root().join("target/components/messaging-provider-whatsapp.wasm");
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
    last_request: RefCell<Option<bindings::greentic::http::http_client::Request>>,
    secret_value: String,
}

impl HostState {
    fn new(secret: &str) -> Self {
        Self {
            table: ResourceTable::new(),
            wasi_ctx: WasiCtxBuilder::new().inherit_stdio().build(),
            last_request: RefCell::new(None),
            secret_value: secret.to_string(),
        }
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

impl bindings::greentic::http::http_client::Host for HostState {
    fn send(
        &mut self,
        req: bindings::greentic::http::http_client::Request,
        _options: Option<bindings::greentic::http::http_client::RequestOptions>,
        _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
    ) -> Result<
        bindings::greentic::http::http_client::Response,
        bindings::greentic::http::http_client::HostError,
    > {
        self.last_request.replace(Some(req));
        Ok(bindings::greentic::http::http_client::Response {
            status: 200,
            headers: vec![],
            body: Some(
                serde_json::to_vec(&json!({"messages":[{"id":"wamid.123"}]})).expect("resp bytes"),
            ),
        })
    }
}

impl bindings::greentic::secrets_store::secrets_store::Host for HostState {
    fn get(
        &mut self,
        key: String,
    ) -> Result<Option<Vec<u8>>, bindings::greentic::secrets_store::secrets_store::SecretsError>
    {
        if key == TOKEN_KEY {
            Ok(Some(self.secret_value.as_bytes().to_vec()))
        } else {
            Ok(None)
        }
    }
}

impl bindings::greentic::interfaces_types::types::Host for HostState {}

fn add_wasi_to_linker(linker: &mut Linker<HostState>) {
    wasmtime_wasi::p2::add_to_linker_sync(linker).expect("add wasi");
}

#[test]
fn builds_whatsapp_component() -> Result<()> {
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
    let pack_dir = workspace_root().join("packs/messaging-whatsapp");
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
        Some("messaging.whatsapp.cloud")
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
        "assets/schemas/messaging/whatsapp/config.schema.json"
    );
    assert!(
        pack_dir.join(schema_ref).exists(),
        "pack schema should exist"
    );
    assert!(
        workspace_root()
            .join("assets/schemas/messaging/whatsapp/config.schema.json")
            .exists(),
        "workspace schema should exist"
    );

    let ingress_ext = manifest
        .get("extensions")
        .and_then(|ext| ext.get("messaging.provider_ingress.v1"))
        .expect("pack should include ingress extension");
    assert_eq!(
        ingress_ext
            .get("inline")
            .and_then(|inline| inline.get("component_ref"))
            .and_then(|v| v.as_str()),
        Some("messaging-ingress-whatsapp")
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
    bindings::greentic::http::http_client::add_to_linker::<HostState, HasSelf<HostState>>(
        &mut linker,
        |state: &mut HostState| state,
    )
    .expect("link http");
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

    let mut describe_store = Store::new(&engine, HostState::new("token-value"));
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
    assert_eq!(manifest.provider_type, "messaging.whatsapp.cloud");
    assert!(manifest.ops.contains(&"send".to_string()));
    assert!(manifest.ops.contains(&"reply".to_string()));

    drop(describe_store);
    let mut store = Store::new(&engine, HostState::new("secret-token"));
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
        "to": {"kind": "user", "id": "+15551234567"},
        "text": "hello whatsapp",
        "config": {
            "access_token": TOKEN_KEY,
            "phone_number_id": "12345",
            "api_version": "v19.0"
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
        Some(&Value::String("messaging.whatsapp.cloud".into()))
    );
    assert_eq!(
        resp_json.get("provider_message_id"),
        Some(&Value::String("whatsapp:wamid.123".into()))
    );

    let last_req = store
        .data()
        .last_request
        .borrow()
        .clone()
        .expect("request recorded");
    assert!(last_req.url.contains("/v19.0/12345/messages"));
    assert!(
        last_req
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer secret-token")
    );
    let body_json: Value = serde_json::from_slice(last_req.body.as_ref().expect("body set"))?;
    assert_eq!(
        body_json.get("to"),
        Some(&Value::String("+15551234567".into()))
    );
    assert_eq!(
        body_json.get("messaging_product"),
        Some(&Value::String("whatsapp".into()))
    );

    Ok(())
}

#[test]
fn reply_requires_context() -> Result<()> {
    let component_path = ensure_component_artifact()?;
    let engine = new_engine();
    let component = Component::from_file(&engine, &component_path).context("loading component")?;
    let mut linker = Linker::new(&engine);
    add_wasi_to_linker(&mut linker);
    bindings::greentic::http::http_client::add_to_linker::<HostState, HasSelf<HostState>>(
        &mut linker,
        |state: &mut HostState| state,
    )
    .expect("link http");
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

    let mut store = Store::new(&engine, HostState::new("secret-token"));
    let instance = linker
        .instantiate(&mut store, &component)
        .context("instantiate for invoke reply failure")?;

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
        "to": {"kind": "user", "id": "+15551234567"},
        "text": "reply whatsapp",
        "config": {
            "access_token": TOKEN_KEY,
            "phone_number_id": "12345"
        }
    });
    let (resp,) = invoke
        .call(
            &mut store,
            ("reply".to_string(), serde_json::to_vec(&input)?),
        )
        .context("call invoke reply failure")?;
    let resp_json: Value = serde_json::from_slice(&resp).context("parse invoke output")?;
    assert_eq!(resp_json.get("ok"), Some(&Value::Bool(false)));

    Ok(())
}

#[test]
fn invoke_reply_smoke_test() -> Result<()> {
    let component_path = ensure_component_artifact()?;
    let engine = new_engine();
    let component = Component::from_file(&engine, &component_path).context("loading component")?;
    let mut linker = Linker::new(&engine);
    add_wasi_to_linker(&mut linker);
    bindings::greentic::http::http_client::add_to_linker::<HostState, HasSelf<HostState>>(
        &mut linker,
        |state: &mut HostState| state,
    )
    .expect("link http");
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

    let mut store = Store::new(&engine, HostState::new("secret-token"));
    let instance = linker
        .instantiate(&mut store, &component)
        .context("instantiate for invoke reply")?;

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
        "to": {"kind": "user", "id": "+15551234567"},
        "text": "reply whatsapp",
        "reply_to_id": "wamid.abc",
        "config": {
            "access_token": TOKEN_KEY,
            "phone_number_id": "12345",
            "api_version": "v19.0"
        }
    });
    let (resp,) = invoke
        .call(
            &mut store,
            ("reply".to_string(), serde_json::to_vec(&input)?),
        )
        .context("call invoke reply")?;
    let resp_json: Value = serde_json::from_slice(&resp).context("parse invoke output")?;
    assert_eq!(
        resp_json.get("status"),
        Some(&Value::String("replied".into()))
    );
    assert!(
        resp_json
            .get("provider_message_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .starts_with("whatsapp:"),
        "provider_message_id should start with whatsapp:"
    );

    let last_req = store
        .data()
        .last_request
        .borrow()
        .clone()
        .expect("request recorded");
    let body_json: Value = serde_json::from_slice(last_req.body.as_ref().expect("body set"))?;
    assert_eq!(
        body_json.get("context").and_then(|c| c.get("message_id")),
        Some(&Value::String("wamid.abc".into()))
    );

    Ok(())
}
