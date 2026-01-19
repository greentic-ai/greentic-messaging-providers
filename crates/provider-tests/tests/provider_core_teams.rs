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
        path: "../../components/messaging-provider-teams/wit/messaging-provider-teams",
        world: "messaging-provider-teams",
    });
}

const CLIENT_SECRET_KEY: &str = "MS_GRAPH_CLIENT_SECRET";
const REFRESH_TOKEN_KEY: &str = "MS_GRAPH_REFRESH_TOKEN";

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
        root.join("target/components/messaging-provider-teams.wasm"),
        root.join("target/wasm32-wasip2/release/messaging_provider_teams.wasm"),
        root.join("target/wasm32-wasip2/wasm32-wasip2/release/messaging_provider_teams.wasm"),
        root.join("components/messaging-provider-teams/target/wasm32-wasip2/release/messaging_provider_teams.wasm"),
        root.join("packs/messaging-teams/components/messaging-provider-teams.wasm"),
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
            "messaging-provider-teams",
        ])
        .current_dir(workspace_root())
        .status()
        .context("running cargo component build for messaging-provider-teams")?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "cargo component build failed with status {status}"
        ));
    }

    for path in candidate_artifacts() {
        if path.exists() {
            let target = workspace_root().join("target/components/messaging-provider-teams.wasm");
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
    secrets: HashMap<String, String>,
    responses: RefCell<Vec<bindings::greentic::http::http_client::Response>>, // queued responses
    sent_requests: RefCell<Vec<bindings::greentic::http::http_client::Request>>,
}

impl HostState {
    fn with_secret(key: &str, value: &str) -> Self {
        let mut secrets = HashMap::new();
        secrets.insert(key.to_string(), value.to_string());
        Self {
            table: ResourceTable::new(),
            wasi_ctx: WasiCtxBuilder::new().inherit_stdio().build(),
            secrets,
            responses: RefCell::new(vec![]),
            sent_requests: RefCell::new(vec![]),
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
        self.sent_requests.borrow_mut().push(req);
        if let Some(resp) = self.responses.borrow_mut().pop() {
            Ok(resp)
        } else {
            Ok(bindings::greentic::http::http_client::Response {
                status: 200,
                headers: vec![],
                body: None,
            })
        }
    }
}

impl bindings::greentic::secrets_store::secrets_store::Host for HostState {
    fn get(
        &mut self,
        key: String,
    ) -> Result<Option<Vec<u8>>, bindings::greentic::secrets_store::secrets_store::SecretsError>
    {
        Ok(self.secrets.get(&key).map(|v| v.as_bytes().to_vec()))
    }
}

impl bindings::greentic::interfaces_types::types::Host for HostState {}

fn add_wasi_to_linker(linker: &mut Linker<HostState>) {
    wasmtime_wasi::p2::add_to_linker_sync(linker).expect("add wasi");
}

#[test]
fn builds_teams_component() -> Result<()> {
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
    let pack_dir = workspace_root().join("packs/messaging-teams");
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
        Some("messaging.teams.graph")
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
        "assets/schemas/messaging/teams/config.schema.json"
    );
    assert!(
        pack_dir.join(schema_ref).exists(),
        "pack schema should exist"
    );
    assert!(
        workspace_root()
            .join("assets/schemas/messaging/teams/config.schema.json")
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
        Some("messaging-ingress-teams")
    );

    let subs_ext = manifest
        .get("extensions")
        .and_then(|ext| ext.get("messaging.subscriptions.v1"))
        .expect("pack should include subscriptions extension");
    assert_eq!(
        subs_ext
            .get("inline")
            .and_then(|inline| inline.get("component_ref"))
            .and_then(|v| v.as_str()),
        Some("messaging-ingress-teams")
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
        Some("teams")
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

    let mut describe_store = Store::new(&engine, HostState::default());
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
    assert_eq!(manifest.provider_type, "messaging.teams.graph");
    assert!(manifest.ops.contains(&"send".to_string()));

    let mut state = HostState::with_secret(CLIENT_SECRET_KEY, "super-secret");
    state
        .secrets
        .insert(REFRESH_TOKEN_KEY.into(), "refresh-token".into());
    state
        .responses
        .borrow_mut()
        .push(bindings::greentic::http::http_client::Response {
            status: 201,
            headers: vec![],
            body: Some(serde_json::to_vec(&json!({"id":"msg-1"}))?),
        });
    state
        .responses
        .borrow_mut()
        .push(bindings::greentic::http::http_client::Response {
            status: 200,
            headers: vec![],
            body: Some(serde_json::to_vec(&json!({"access_token":"tok-123"}))?),
        });

    let mut store = Store::new(&engine, state);
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
        "text": "hello teams",
        "team_id": "team-1",
        "channel_id": "channel-1",
        "config": {
            "tenant_id": "tenant-123",
            "client_id": "client-123",
            "client_secret_key": CLIENT_SECRET_KEY,
            "refresh_token_key": REFRESH_TOKEN_KEY
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
        Some(&Value::String("messaging.teams.graph".into()))
    );
    assert_eq!(
        first_json.get("message_id"),
        Some(&Value::String("msg-1".into()))
    );

    // Inspect recorded requests from the original state.
    let sent_requests = store.data().sent_requests.borrow();
    assert_eq!(sent_requests.len(), 2, "token + send requests expected");
    let token_req = &sent_requests[0];
    assert!(
        token_req
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/x-www-form-urlencoded")
    );
    assert!(token_req.url.contains("/tenant-123/oauth2/v2.0/token"));
    let token_body = String::from_utf8(token_req.body.as_ref().expect("token body").clone())?;
    assert!(
        token_body.contains("grant_type=refresh_token")
            || token_body.contains("grant_type=client_credentials"),
        "token body must declare grant_type"
    );

    let send_req = &sent_requests[1];
    assert!(
        send_req
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer tok-123"),
        "send request must include bearer token"
    );
    assert!(
        send_req
            .url
            .contains("teams/team-1/channels/channel-1/messages")
    );
    let body_json: Value = serde_json::from_slice(&send_req.body.as_ref().expect("body")[..])?;
    assert_eq!(
        body_json.get("body").and_then(|v| v.get("content")),
        Some(&Value::String("hello teams".into()))
    );

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

    let mut state = HostState::with_secret(CLIENT_SECRET_KEY, "super-secret");
    state
        .secrets
        .insert(REFRESH_TOKEN_KEY.into(), "refresh-token".into());
    state
        .responses
        .borrow_mut()
        .push(bindings::greentic::http::http_client::Response {
            status: 201,
            headers: vec![],
            body: Some(serde_json::to_vec(&json!({"id":"reply-1"}))?),
        });
    state
        .responses
        .borrow_mut()
        .push(bindings::greentic::http::http_client::Response {
            status: 200,
            headers: vec![],
            body: Some(serde_json::to_vec(&json!({"access_token":"tok-123"}))?),
        });

    let mut store = Store::new(&engine, state);
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
        "text": "reply to teams",
        "team_id": "team-1",
        "channel_id": "channel-1",
        "reply_to_id": "thread-42",
        "config": {
            "tenant_id": "tenant-123",
            "client_id": "client-123",
            "client_secret_key": CLIENT_SECRET_KEY,
            "refresh_token_key": REFRESH_TOKEN_KEY
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
    assert_eq!(
        resp_json.get("provider_type"),
        Some(&Value::String("messaging.teams.graph".into()))
    );

    let sent = store.data().sent_requests.borrow().clone();
    assert_eq!(sent.len(), 2, "expected token + reply requests");
    let reply_req = sent.last().expect("reply request recorded");
    assert!(
        reply_req.url.contains("/replies"),
        "reply URL should target replies endpoint"
    );

    Ok(())
}

#[test]
fn reply_requires_thread_id() -> Result<()> {
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

    let mut store = Store::new(
        &engine,
        HostState::with_secret(CLIENT_SECRET_KEY, "super-secret"),
    );
    let instance = linker
        .instantiate(&mut store, &component)
        .context("instantiate for reply missing thread")?;
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
        "text": "reply",
        "team_id": "team-1",
        "channel_id": "channel-1",
        "config": {
            "tenant_id": "tenant-123",
            "client_id": "client-123",
            "client_secret_key": CLIENT_SECRET_KEY
        }
    });
    let (resp,) = invoke
        .call(
            &mut store,
            ("reply".to_string(), serde_json::to_vec(&input)?),
        )
        .context("call invoke reply missing thread")?;
    let resp_json: Value = serde_json::from_slice(&resp).context("parse invoke output")?;
    assert_eq!(resp_json.get("ok"), Some(&Value::Bool(false)));

    Ok(())
}
