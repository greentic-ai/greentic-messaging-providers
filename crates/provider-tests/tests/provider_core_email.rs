use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use greentic_interfaces_wasmtime::host_helpers::v1::http_client::HttpClientHostV1_1;
use greentic_interfaces_wasmtime::host_helpers::v1::{
    HostFns, add_all_v1_to_linker, http_client, secrets_store, state_store,
};
use greentic_interfaces_wasmtime::http_client_client_v1_1::greentic::http::http_client as http_client_client_alias;
use greentic_types::{ProviderManifest, provider::PROVIDER_EXTENSION_ID};
use serde_json::{Value, json};
use wasmtime::component::{Component, ComponentExportIndex, Linker, ResourceTable, TypedFunc};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView, p2};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../../components/messaging-provider-email/wit/messaging-provider-email",
        world: "messaging-provider-email",
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
        root.join("target/components/messaging-provider-email.wasm"),
        root.join("target/wasm32-wasip2/release/messaging_provider_email.wasm"),
        root.join("target/wasm32-wasip2/wasm32-wasip2/release/messaging_provider_email.wasm"),
        root.join("components/messaging-provider-email/target/wasm32-wasip2/release/messaging_provider_email.wasm"),
        root.join("packs/messaging-email/components/messaging-provider-email.wasm"),
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
            "messaging-provider-email",
        ])
        .current_dir(workspace_root())
        .status()
        .context("running cargo component build for messaging-provider-email")?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "cargo component build failed with status {status}"
        ));
    }

    for path in candidate_artifacts() {
        if path.exists() {
            let target = workspace_root().join("target/components/messaging-provider-email.wasm");
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

impl Default for HostState {
    fn default() -> Self {
        Self {
            table: ResourceTable::new(),
            wasi_ctx: WasiCtxBuilder::new().inherit_stdio().build(),
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

impl http_client::HttpClientHostV1_1 for HostState {
    fn send(
        &mut self,
        _req: http_client::RequestV1_1,
        _opts: Option<http_client::RequestOptionsV1_1>,
        _ctx: Option<http_client::TenantCtxV1_1>,
    ) -> Result<http_client::ResponseV1_1, http_client::HttpClientErrorV1_1> {
        let body = serde_json::to_vec(&json!({"status":"ok"})).unwrap_or_else(|_| b"{}".to_vec());
        Ok(http_client::ResponseV1_1 {
            status: 200,
            headers: Vec::new(),
            body: Some(body),
        })
    }
}

fn add_wasi_to_linker(linker: &mut Linker<HostState>) {
    add_all_v1_to_linker(
        linker,
        HostFns {
            secrets_store_v1_1: Some(|state| state as &mut dyn secrets_store::SecretsStoreHostV1_1),
            state_store: Some(|state| state as &mut dyn state_store::StateStoreHost),
            ..Default::default()
        },
    )
    .expect("add wasi");
    p2::add_to_linker_sync(linker).expect("add wasi p2");
}

fn add_http_client_http_client_world(linker: &mut Linker<HostState>) -> Result<()> {
    let mut inst = linker.instance("greentic:http/http-client@1.1.0")?;
    inst.func_wrap(
        "send",
        move |_caller: wasmtime::StoreContextMut<'_, HostState>,
              (req, opts, ctx): (
            http_client::RequestV1_1,
            Option<http_client::RequestOptionsV1_1>,
            Option<http_client::TenantCtxV1_1>,
        )| {
            let mut state = HostState::default();
            let result = state.send(req, opts, ctx);
            Ok((result,))
        },
    )?;
    Ok(())
}

fn add_http_client_client_world(linker: &mut Linker<HostState>) -> Result<()> {
    let mut inst = linker.instance("greentic:http/client@1.1.0")?;
    inst.func_wrap(
        "send",
        move |_caller: wasmtime::StoreContextMut<'_, HostState>,
              (req, opts, ctx): (
            http_client_client_alias::Request,
            Option<http_client_client_alias::RequestOptions>,
            Option<http_client_client_alias::TenantCtx>,
        )| {
            let mut state = HostState::default();
            let result = state.send(
                alias_request_to_host(req),
                opts.map(alias_request_options_to_host),
                ctx.map(alias_tenant_ctx_to_host),
            );
            Ok((match result {
                Ok(resp) => Ok(alias_response_from_host(resp)),
                Err(err) => Err(alias_error_from_host(err)),
            },))
        },
    )?;
    Ok(())
}

fn alias_request_to_host(req: http_client_client_alias::Request) -> http_client::RequestV1_1 {
    http_client::RequestV1_1 {
        method: req.method,
        url: req.url,
        headers: req.headers,
        body: req.body,
    }
}

fn alias_request_options_to_host(
    opts: http_client_client_alias::RequestOptions,
) -> http_client::RequestOptionsV1_1 {
    http_client::RequestOptionsV1_1 {
        timeout_ms: opts.timeout_ms,
        allow_insecure: opts.allow_insecure,
        follow_redirects: opts.follow_redirects,
    }
}

fn alias_tenant_ctx_to_host(
    ctx: http_client_client_alias::TenantCtx,
) -> http_client::TenantCtxV1_1 {
    http_client::TenantCtxV1_1 {
        env: ctx.env,
        tenant: ctx.tenant,
        tenant_id: ctx.tenant_id,
        team: ctx.team,
        team_id: ctx.team_id,
        user: ctx.user,
        user_id: ctx.user_id,
        i18n_id: ctx.i18n_id,
        trace_id: ctx.trace_id,
        correlation_id: ctx.correlation_id,
        attributes: ctx.attributes,
        session_id: ctx.session_id,
        flow_id: ctx.flow_id,
        node_id: ctx.node_id,
        provider_id: ctx.provider_id,
        deadline_ms: ctx.deadline_ms,
        attempt: ctx.attempt,
        idempotency_key: ctx.idempotency_key,
        impersonation: ctx.impersonation,
    }
}

fn alias_response_from_host(resp: http_client::ResponseV1_1) -> http_client_client_alias::Response {
    http_client_client_alias::Response {
        status: resp.status,
        headers: resp.headers,
        body: resp.body,
    }
}

fn alias_error_from_host(
    err: http_client::HttpClientErrorV1_1,
) -> http_client_client_alias::HostError {
    http_client_client_alias::HostError {
        code: err.code,
        message: err.message,
    }
}

impl secrets_store::SecretsStoreHostV1_1 for HostState {
    fn get(&mut self, _key: String) -> Result<Option<Vec<u8>>, secrets_store::SecretsErrorV1_1> {
        Ok(None)
    }

    fn put(&mut self, _key: String, _value: Vec<u8>) {}
}

impl state_store::StateStoreHost for HostState {
    fn read(
        &mut self,
        _key: state_store::StateKey,
        _ctx: Option<state_store::TenantCtx>,
    ) -> Result<Vec<u8>, state_store::StateStoreError> {
        Err(state_store::StateStoreError {
            code: "unimplemented".into(),
            message: "state store not available in provider_core_email tests".into(),
        })
    }

    fn write(
        &mut self,
        _key: state_store::StateKey,
        _bytes: Vec<u8>,
        _ctx: Option<state_store::TenantCtx>,
    ) -> Result<state_store::OpAck, state_store::StateStoreError> {
        Err(state_store::StateStoreError {
            code: "unimplemented".into(),
            message: "state store not available in provider_core_email tests".into(),
        })
    }

    fn delete(
        &mut self,
        _key: state_store::StateKey,
        _ctx: Option<state_store::TenantCtx>,
    ) -> Result<state_store::OpAck, state_store::StateStoreError> {
        Err(state_store::StateStoreError {
            code: "unimplemented".into(),
            message: "state store not available in provider_core_email tests".into(),
        })
    }
}

#[test]
fn builds_email_component() -> Result<()> {
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
    let pack_dir = workspace_root().join("packs/messaging-email");
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
        Some("messaging.email.smtp")
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
        "schemas/messaging/email/public.config.schema.json"
    );
    assert!(
        pack_dir.join(schema_ref).exists(),
        "pack schema should exist"
    );
    assert!(
        workspace_root()
            .join("schemas/messaging/email/public.config.schema.json")
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
    add_http_client_http_client_world(&mut linker)?;
    add_http_client_client_world(&mut linker)?;

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
    assert_eq!(manifest.provider_type, "messaging.email.smtp");
    assert!(manifest.ops.contains(&"send".to_string()));
    assert!(manifest.ops.contains(&"reply".to_string()));

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
        .get_typed_func(&mut store, invoke_index)
        .context("get invoke func")?;

    let input = json!({
        "to": "test@example.com",
        "subject": "hello",
        "body": "hi there",
        "config": {
            "host": "smtp.example.com",
            "port": 2525,
            "username": "user",
            "from_address": "no-reply@example.com"
        }
    });
    let input_bytes = serde_json::to_vec(&input)?;
    let (resp,) = invoke
        .call(&mut store, ("send".to_string(), input_bytes))
        .context("call invoke send")?;
    let resp_json: Value = serde_json::from_slice(&resp).context("parse invoke output")?;
    eprintln!("resp_json = {resp_json:?}");
    assert_eq!(resp_json.get("status"), Some(&Value::String("sent".into())));
    assert_eq!(
        resp_json.get("provider_type"),
        Some(&Value::String("messaging.email.smtp".into()))
    );
    assert!(resp_json.get("message_id").is_some());
    assert!(resp_json.get("provider_message_id").is_some());

    Ok(())
}

#[test]
fn invoke_reply_smoke_test() -> Result<()> {
    let component_path = ensure_component_artifact()?;
    let engine = new_engine();
    let component = Component::from_file(&engine, &component_path).context("loading component")?;
    let mut linker = Linker::new(&engine);
    add_wasi_to_linker(&mut linker);
    add_http_client_http_client_world(&mut linker)?;
    add_http_client_client_world(&mut linker)?;

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
        .get_typed_func(&mut store, invoke_index)
        .context("get invoke func")?;

    let input = json!({
        "to": "user@example.com",
        "subject": "Re: hello",
        "body": "reply body",
        "reply_to_id": "msg-123",
        "config": {
            "host": "smtp.example.com",
            "port": 25,
            "username": "u",
            "from_address": "noreply@example.com"
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
            .starts_with("smtp-reply:"),
        "provider_message_id should be smtp-reply:*"
    );

    Ok(())
}

#[test]
fn reply_requires_to() -> Result<()> {
    let component_path = ensure_component_artifact()?;
    let engine = new_engine();
    let component = Component::from_file(&engine, &component_path).context("loading component")?;
    let mut linker = Linker::new(&engine);
    add_wasi_to_linker(&mut linker);
    add_http_client_http_client_world(&mut linker)?;
    add_http_client_client_world(&mut linker)?;

    let mut store = Store::new(&engine, HostState::default());
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
        "subject": "Re: hello",
        "body": "reply body",
        "reply_to_id": "msg-123",
        "config": {
            "host": "smtp.example.com",
            "port": 25,
            "username": "u",
            "from_address": "noreply@example.com"
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
