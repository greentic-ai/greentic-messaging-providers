use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, Once},
};

use anyhow::Result;
use greentic_interfaces_wasmtime::host_helpers::v1::{
    HostFns, add_all_v1_to_linker, http_client, secrets_store, state_store,
};
use greentic_interfaces_wasmtime::http_client_client_v1_1::greentic::http::http_client as http_client_client_alias;
use serde_json::json;
use wasmtime::{
    Config, Engine,
    component::{Linker, ResourceTable},
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

type HttpResponder = dyn Fn(
        http_client::RequestV1_1,
    ) -> Result<http_client::ResponseV1_1, http_client::HttpClientErrorV1_1>
    + Send
    + Sync;

pub struct TestHostState {
    pub table: ResourceTable,
    pub wasi_ctx: WasiCtx,
    pub last_request: RefCell<Option<http_client::RequestV1_1>>,
    secrets: HashMap<String, Vec<u8>>,
    http_handler: Arc<Mutex<Box<HttpResponder>>>,
}

impl TestHostState {
    pub fn with_secrets<F>(secrets: HashMap<String, Vec<u8>>, handler: F) -> Self
    where
        F: Fn(
                http_client::RequestV1_1,
            ) -> Result<http_client::ResponseV1_1, http_client::HttpClientErrorV1_1>
            + Send
            + Sync
            + 'static,
    {
        Self {
            table: ResourceTable::new(),
            wasi_ctx: WasiCtxBuilder::new().inherit_stdio().build(),
            last_request: RefCell::new(None),
            secrets,
            http_handler: Arc::new(Mutex::new(Box::new(handler))),
        }
    }

    pub fn with_default_secrets() -> Self {
        Self::with_secrets(default_secret_values(), default_http_handler)
    }
}

impl Default for TestHostState {
    fn default() -> Self {
        Self::with_default_secrets()
    }
}

impl WasiView for TestHostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.table,
        }
    }
}

fn default_http_handler(
    _req: http_client::RequestV1_1,
) -> Result<http_client::ResponseV1_1, http_client::HttpClientErrorV1_1> {
    let body = serde_json::to_vec(&json!({"status": "ok"})).unwrap_or_else(|_| b"{}".to_vec());
    Ok(http_client::ResponseV1_1 {
        status: 200,
        headers: Vec::new(),
        body: Some(body),
    })
}

impl http_client::HttpClientHostV1_1 for TestHostState {
    fn send(
        &mut self,
        req: http_client::RequestV1_1,
        _opts: Option<http_client::RequestOptionsV1_1>,
        _ctx: Option<http_client::TenantCtxV1_1>,
    ) -> Result<http_client::ResponseV1_1, http_client::HttpClientErrorV1_1> {
        self.last_request.replace(Some(req.clone()));
        let handler = self.http_handler.lock().unwrap();
        handler(req)
    }
}

impl secrets_store::SecretsStoreHostV1_1 for TestHostState {
    fn get(&mut self, key: String) -> Result<Option<Vec<u8>>, secrets_store::SecretsErrorV1_1> {
        Ok(self.secrets.get(&key).cloned())
    }

    fn put(&mut self, key: String, value: Vec<u8>) {
        self.secrets.insert(key, value);
    }
}

impl state_store::StateStoreHost for TestHostState {
    fn read(
        &mut self,
        _key: state_store::StateKey,
        _ctx: Option<state_store::TenantCtx>,
    ) -> Result<Vec<u8>, state_store::StateStoreError> {
        Err(state_store::StateStoreError {
            code: "unimplemented".into(),
            message: "state store not available in universal tests".into(),
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
            message: "state store not available in universal tests".into(),
        })
    }

    fn delete(
        &mut self,
        _key: state_store::StateKey,
        _ctx: Option<state_store::TenantCtx>,
    ) -> Result<state_store::OpAck, state_store::StateStoreError> {
        Err(state_store::StateStoreError {
            code: "unimplemented".into(),
            message: "state store not available in universal tests".into(),
        })
    }
}

pub fn add_wasmtime_hosts(linker: &mut Linker<TestHostState>) -> Result<()> {
    add_all_v1_to_linker(
        linker,
        HostFns {
            secrets_store_v1_1: Some(|state| state as &mut dyn secrets_store::SecretsStoreHostV1_1),
            state_store: Some(|state| state as &mut dyn state_store::StateStoreHost),
            ..Default::default()
        },
    )?;
    add_http_client_http_client_world(linker)?;
    add_http_client_client_world(linker)?;
    Ok(())
}

fn http_client_v1_1_host(state: &mut TestHostState) -> &mut dyn http_client::HttpClientHostV1_1 {
    state
}

fn add_http_client_http_client_world(linker: &mut Linker<TestHostState>) -> Result<()> {
    let mut inst = linker.instance("greentic:http/http-client@1.1.0")?;
    inst.func_wrap(
        "send",
        move |mut caller: wasmtime::StoreContextMut<'_, TestHostState>,
              (req, opts, ctx): (
            http_client::RequestV1_1,
            Option<http_client::RequestOptionsV1_1>,
            Option<http_client::TenantCtxV1_1>,
        )| {
            let host = http_client_v1_1_host(caller.data_mut());
            let result = host.send(req, opts, ctx);
            Ok((result,))
        },
    )?;
    Ok(())
}

fn add_http_client_client_world(linker: &mut Linker<TestHostState>) -> Result<()> {
    let mut inst = linker.instance("greentic:http/client@1.1.0")?;
    inst.func_wrap(
        "send",
        move |mut caller: wasmtime::StoreContextMut<'_, TestHostState>,
              (req, opts, ctx): (
            http_client_client_alias::Request,
            Option<http_client_client_alias::RequestOptions>,
            Option<http_client_client_alias::TenantCtx>,
        )| {
            let host = http_client_v1_1_host(caller.data_mut());
            let result = host.send(
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

pub fn add_wasi_to_linker(linker: &mut Linker<TestHostState>) {
    wasmtime_wasi::p2::add_to_linker_sync(linker).expect("add wasi");
}

pub fn new_engine() -> Engine {
    let mut config = Config::new();
    config.wasm_component_model(true);
    Engine::new(&config).expect("engine")
}

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

static BUILD_COMPONENTS_ONCE: Once = Once::new();

pub fn ensure_components_built() {
    BUILD_COMPONENTS_ONCE.call_once(|| {
        let script = workspace_root().join("tools/build_components.sh");
        if !script.exists() {
            return;
        }
        let status = Command::new("bash")
            .arg(&script)
            .env("SKIP_WASM_TOOLS_VALIDATION", "1")
            .current_dir(workspace_root())
            .status()
            .expect("failed to run tools/build_components.sh");
        assert!(status.success(), "tools/build_components.sh failed");
    });
}

pub fn component_path(name: &str) -> PathBuf {
    ensure_components_built();
    let root = workspace_root();
    let candidates = [
        root.join(format!("target/components/{name}.wasm")),
        root.join(format!("target/wasm32-wasip2/release/{name}.wasm")),
        root.join(format!("target/wasm32-wasip2/debug/{name}.wasm")),
        root.join(format!(
            "target/wasm32-wasip2/wasm32-wasip2/release/{name}.wasm"
        )),
        root.join(format!(
            "target/wasm32-wasip2/wasm32-wasip2/debug/{name}.wasm"
        )),
        root.join(format!(
            "components/{name}/target/wasm32-wasip2/release/{name}.wasm"
        )),
        root.join(format!(
            "components/{name}/target/wasm32-wasip2/debug/{name}.wasm"
        )),
    ];
    for path in candidates {
        if path.exists() {
            return path;
        }
    }
    panic!("component {name} missing; expected one of the standard paths");
}

pub mod instantiate;

const SECRET_PAIRS: &[(&str, &str)] = &[
    ("SLACK_BOT_TOKEN", "slack-token"),
    ("SLACK_SIGNING_SECRET", "slack-signing"),
    ("TELEGRAM_BOT_TOKEN", "telegram-token"),
    ("MS_GRAPH_CLIENT_SECRET", "ms-graph-secret"),
    ("MS_GRAPH_REFRESH_TOKEN", "ms-graph-refresh"),
    ("WEBEX_BOT_TOKEN", "webex-token"),
    ("WHATSAPP_TOKEN", "whatsapp-token"),
    ("WHATSAPP_VERIFY_TOKEN", "whatsapp-verify"),
    ("EMAIL_PASSWORD", "email-secret"),
];

pub fn default_secret_values() -> HashMap<String, Vec<u8>> {
    let mut map = HashMap::new();
    for &(key, value) in SECRET_PAIRS {
        map.insert(key.to_string(), value.as_bytes().to_vec());
    }
    map
}
