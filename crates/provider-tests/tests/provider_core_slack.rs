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
        path: "../../components/messaging-provider-slack/wit/messaging-provider-slack",
        world: "messaging-provider-slack",
    });
}

const BOT_TOKEN_KEY: &str = "SLACK_BOT_TOKEN";

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
        root.join("target/components/messaging-provider-slack.wasm"),
        root.join("target/wasm32-wasip2/release/messaging_provider_slack.wasm"),
        root.join("target/wasm32-wasip2/wasm32-wasip2/release/messaging_provider_slack.wasm"),
        root.join("components/messaging-provider-slack/target/wasm32-wasip2/release/messaging_provider_slack.wasm"),
        root.join("packs/messaging-slack/components/messaging-provider-slack.wasm"),
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
            "messaging-provider-slack",
        ])
        .current_dir(workspace_root())
        .status()
        .context("running cargo component build for messaging-provider-slack")?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "cargo component build failed with status {status}"
        ));
    }

    for path in candidate_artifacts() {
        if path.exists() {
            let target = workspace_root().join("target/components/messaging-provider-slack.wasm");
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
            body: Some(serde_json::to_vec(&json!({"ts":"123.456"})).expect("resp bytes")),
        })
    }
}

impl bindings::greentic::secrets_store::secrets_store::Host for HostState {
    fn get(
        &mut self,
        key: String,
    ) -> Result<Option<Vec<u8>>, bindings::greentic::secrets_store::secrets_store::SecretsError>
    {
        if key == BOT_TOKEN_KEY {
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
fn builds_slack_component() -> Result<()> {
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
    let pack_dir = workspace_root().join("packs/messaging-slack");
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
        Some("messaging.slack.api")
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
        "assets/schemas/messaging/slack/config.schema.json"
    );
    assert!(
        pack_dir.join(schema_ref).exists(),
        "pack schema should exist"
    );
    assert!(
        workspace_root()
            .join("assets/schemas/messaging/slack/config.schema.json")
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
        Some("messaging-ingress-slack")
    );

    let oauth_ext = manifest
        .get("extensions")
        .and_then(|ext| ext.get("messaging.oauth.v1"))
        .expect("pack should include oauth extension");
    assert_eq!(
        oauth_ext
            .get("inline")
            .and_then(|inline| inline.get("provider_id"))
            .and_then(|v| v.as_str()),
        Some("slack")
    );
    Ok(())
}

#[test]
fn invoke_send_smoke_test() -> Result<()> {
    let component_path = ensure_component_artifact()?
        .canonicalize()
        .expect("canonicalize");
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

    let mut describe_store = Store::new(&engine, HostState::new("bot-token"));
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
    assert_eq!(manifest.provider_type, "messaging.slack.api");
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
        "to": {"kind": "channel", "id": "C123"},
        "text": "hello slack",
        "rich": {"format": "slack_blocks", "blocks": [{"type":"section","text":{"type":"mrkdwn","text":"hello"}}]},
        "config": {
            "bot_token": BOT_TOKEN_KEY,
            "api_base_url": "https://slack.com/api"
        }
    });
    let input_bytes = serde_json::to_vec(&input)?;
    let (first,) = invoke
        .call(&mut store, ("send".to_string(), input_bytes))
        .context("call invoke send")?;
    let first_json: Value = serde_json::from_slice(&first).context("parse invoke output")?;

    assert_eq!(
        first_json.get("status"),
        Some(&Value::String("sent".into()))
    );
    assert_eq!(
        first_json.get("provider_type"),
        Some(&Value::String("messaging.slack.api".into()))
    );
    assert_eq!(
        first_json.get("message_id"),
        Some(&Value::String("123.456".into()))
    );
    assert_eq!(
        first_json.get("provider_message_id"),
        Some(&Value::String("slack:123.456".into()))
    );

    let last_req = store
        .data()
        .last_request
        .borrow()
        .clone()
        .expect("request recorded");
    assert!(last_req.url.ends_with("/chat.postMessage"));
    assert!(
        last_req
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer secret-token")
    );

    let body_json: Value = serde_json::from_slice(last_req.body.as_ref().expect("body set"))?;
    assert_eq!(
        body_json.get("channel"),
        Some(&Value::String("C123".into()))
    );
    assert_eq!(
        body_json.get("text"),
        Some(&Value::String("hello slack".into()))
    );
    assert!(body_json.get("blocks").is_some());

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
    .expect("link http client");
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
        "thread_id": "ts-thread",
        "text": "reply slack",
        "to": {"kind":"channel","id":"C123"},
        "config": {"bot_token": BOT_TOKEN_KEY}
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
    let last_req = store
        .data()
        .last_request
        .borrow()
        .clone()
        .expect("request recorded");
    let body_json: Value = serde_json::from_slice(last_req.body.as_ref().expect("body set"))?;
    assert_eq!(
        body_json.get("thread_ts"),
        Some(&Value::String("ts-thread".into()))
    );

    Ok(())
}

#[test]
fn reply_fails_without_channel() -> Result<()> {
    let component_path = ensure_component_artifact()?;
    let engine = new_engine();
    let component = Component::from_file(&engine, &component_path).context("loading component")?;
    let mut linker = Linker::new(&engine);
    add_wasi_to_linker(&mut linker);
    bindings::greentic::http::http_client::add_to_linker::<HostState, HasSelf<HostState>>(
        &mut linker,
        |state: &mut HostState| state,
    )
    .expect("link http client");
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
        "text": "reply slack",
        "thread_id": "ts-thread",
        "config": {"bot_token": BOT_TOKEN_KEY}
    });
    let (resp,) = invoke
        .call(
            &mut store,
            ("reply".to_string(), serde_json::to_vec(&input)?),
        )
        .context("call invoke reply failure")?;
    let resp_json: Value = serde_json::from_slice(&resp).context("parse invoke output")?;
    assert_eq!(resp_json.get("ok"), Some(&Value::Bool(false)));
    assert!(
        resp_json
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("channel required")
    );

    Ok(())
}
