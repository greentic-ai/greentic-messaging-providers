use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result, anyhow};
use greentic_interfaces_wasmtime::host_helpers::v1::{
    HostFns, add_all_v1_to_linker, http_client, secrets_store, state_store,
};
use greentic_interfaces_wasmtime::http_client_client_v1_1::greentic::http::http_client as http_client_client_alias;
use greentic_types::ProviderManifest;
use serde::Deserialize;
use wasmtime::{
    Config, Engine, Store,
    component::{Component, ComponentExportIndex, Instance, Linker, ResourceTable},
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

mod component_node_bindings {
    wasmtime::component::bindgen!({
        path: "../../wit/component-node",
        world: "greentic:component/node@0.5.0",
    });
}

use crate::http_mock::{
    self, HttpCall, HttpHistory, HttpMode, HttpRequest, HttpResponseQueue, HttpResponseRecord,
};

const NODE_WORLD: &str = "greentic:component/node@0.5.0";
const SCHEMA_CORE_WORLD: &str = "greentic:provider-schema-core/schema-core-api@1.0.0";

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InvokeStrategy {
    Node,
    SchemaCore,
}

pub struct WasmHarness {
    engine: Engine,
    component: Component,
    manifest: ProviderManifest,
    invoke_strategy: InvokeStrategy,
}

impl WasmHarness {
    pub fn new(provider: &str) -> Result<Self> {
        let wasm_path = find_wasm_path(provider)?;
        let engine = new_engine();
        let component = Component::from_file(&engine, &wasm_path)
            .context("failed to load provider component")?;
        let (manifest, invoke_strategy) = describe_manifest(&engine, &component, &wasm_path)?;
        Ok(Self {
            engine,
            component,
            manifest,
            invoke_strategy,
        })
    }

    #[cfg(test)]
    pub fn new_with_path(component_path: &Path) -> Result<Self> {
        let engine = new_engine();
        let component = Component::from_file(&engine, component_path)
            .context("failed to load provider component")?;
        let (manifest, invoke_strategy) = describe_manifest(&engine, &component, component_path)?;
        Ok(Self {
            engine,
            component,
            manifest,
            invoke_strategy,
        })
    }

    pub fn provider_type(&self) -> &str {
        &self.manifest.provider_type
    }

    pub fn invoke(
        &self,
        op: &str,
        input: Vec<u8>,
        secrets: &HashMap<String, Vec<u8>>,
        http_mode: HttpMode,
        history: HttpHistory,
        mock_responses: Option<HttpResponseQueue>,
    ) -> Result<Vec<u8>> {
        match self.invoke_strategy {
            InvokeStrategy::Node => self.invoke_node_world(
                op,
                input,
                secrets,
                http_mode,
                history,
                mock_responses.clone(),
            ),
            InvokeStrategy::SchemaCore => {
                self.invoke_schema_core(op, input, secrets, http_mode, history, mock_responses)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn invoke_strategy(&self) -> InvokeStrategy {
        self.invoke_strategy
    }

    fn invoke_node_world(
        &self,
        op: &str,
        input: Vec<u8>,
        secrets: &HashMap<String, Vec<u8>>,
        http_mode: HttpMode,
        history: HttpHistory,
        mock_responses: Option<HttpResponseQueue>,
    ) -> Result<Vec<u8>> {
        let input_str = String::from_utf8(input).map_err(|err| anyhow!(err))?;
        let state = TesterHostState::new(secrets.clone(), http_mode, history, mock_responses);
        execute_with_state(&self.engine, &self.component, state, |store, instance| {
            let invoke_index = node_function_index(&mut *store, instance, "invoke")?;
            let invoke = instance.get_typed_func::<
                (component_node_bindings::ExecCtx, String, String),
                (component_node_bindings::InvokeResult,),
            >(&mut *store, invoke_index)?;
            let ctx = build_exec_ctx();
            let (result,) = invoke.call(&mut *store, (ctx, op.to_string(), input_str.clone()))?;
            match result {
                component_node_bindings::InvokeResult::Ok(body) => Ok(body.into_bytes()),
                component_node_bindings::InvokeResult::Err(err) => Err(anyhow!("{}", err.message)),
            }
        })
    }

    fn invoke_schema_core(
        &self,
        op: &str,
        input: Vec<u8>,
        secrets: &HashMap<String, Vec<u8>>,
        http_mode: HttpMode,
        history: HttpHistory,
        mock_responses: Option<HttpResponseQueue>,
    ) -> Result<Vec<u8>> {
        let state = TesterHostState::new(secrets.clone(), http_mode, history, mock_responses);
        execute_with_state(&self.engine, &self.component, state, |store, instance| {
            let api_index = instance
                .get_export_index(&mut *store, None, SCHEMA_CORE_WORLD)
                .context("missing schema-core export for invoke")?;
            let invoke_index = instance
                .get_export_index(&mut *store, Some(&api_index), "invoke")
                .context("missing schema-core invoke export")?;
            let invoke = instance
                .get_typed_func::<(String, Vec<u8>), (Vec<u8>,)>(&mut *store, invoke_index)?;
            let (result,) = invoke.call(&mut *store, (op.to_string(), input))?;
            Ok(result)
        })
    }
}

pub struct ComponentHarness {
    engine: Engine,
    component: Component,
    component_path: PathBuf,
}

impl ComponentHarness {
    pub fn new(component_path: &Path) -> Result<Self> {
        let engine = new_engine();
        let component =
            Component::from_file(&engine, component_path).context("failed to load component")?;
        Ok(Self {
            engine,
            component,
            component_path: component_path.to_path_buf(),
        })
    }

    pub fn invoke(
        &self,
        op: &str,
        input: Vec<u8>,
        secrets: &HashMap<String, Vec<u8>>,
        http_mode: HttpMode,
        history: HttpHistory,
    ) -> Result<Vec<u8>> {
        eprintln!(
            "node world invoke: wasm={} op={}",
            self.component_path.display(),
            op
        );
        let input_json = String::from_utf8(input).map_err(|err| anyhow!(err))?;
        let state = TesterHostState::new(secrets.clone(), http_mode, history, None);
        execute_with_state(&self.engine, &self.component, state, |store, instance| {
            let invoke_index = node_function_index(&mut *store, instance, "invoke")?;
            let invoke = instance.get_typed_func::<
                (component_node_bindings::ExecCtx, String, String),
                (component_node_bindings::InvokeResult,),
            >(&mut *store, invoke_index)?;
            let ctx = build_exec_ctx();
            let (result,) = invoke.call(&mut *store, (ctx, op.to_string(), input_json.clone()))?;
            match result {
                component_node_bindings::InvokeResult::Ok(body) => Ok(body.into_bytes()),
                component_node_bindings::InvokeResult::Err(err) => Err(anyhow!(err.message)),
            }
        })
    }
}

fn build_exec_ctx() -> component_node_bindings::ExecCtx {
    component_node_bindings::ExecCtx {
        tenant: component_node_bindings::TenantCtx {
            tenant: "manual".into(),
            team: None,
            user: None,
            trace_id: None,
            i18n_id: None,
            correlation_id: None,
            deadline_unix_ms: None,
            attempt: 0,
            idempotency_key: None,
        },
        i18n_id: None,
        flow_id: "manual".into(),
        node_id: None,
    }
}

fn describe_manifest(
    engine: &Engine,
    component: &Component,
    component_path: &Path,
) -> Result<(ProviderManifest, InvokeStrategy)> {
    let schema_result = describe_manifest_from_schema(engine, component);
    let schema_err = schema_result.as_ref().err().map(|e| e.to_string());
    if let Ok(manifest) = schema_result {
        return Ok((manifest, InvokeStrategy::SchemaCore));
    }

    if let Some(manifest_path) = manifest_from_component_path(component_path) {
        let contents = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read manifest {}", manifest_path.display()))?;
        let manifest: ProviderManifest =
            serde_json::from_str(&contents).context("failed to parse manifest file")?;
        return Ok((manifest, InvokeStrategy::SchemaCore));
    }

    let node_result = describe_manifest_from_node(engine, component, component_path);
    let node_err = node_result.as_ref().err().map(|e| e.to_string());
    if let Ok(manifest) = node_result {
        return Ok((manifest, InvokeStrategy::Node));
    }

    if let Some(manifest_path) = manifest_from_component_path(component_path) {
        let contents = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read manifest {}", manifest_path.display()))?;
        let manifest: ProviderManifest =
            serde_json::from_str(&contents).context("failed to parse manifest file")?;
        return Ok((manifest, InvokeStrategy::SchemaCore));
    }

    let available_worlds = detect_available_worlds(engine, component).unwrap_or_default();
    let world_desc = if available_worlds.is_empty() {
        "<none>".to_string()
    } else {
        available_worlds.join(", ")
    };

    Err(anyhow!(
        "missing invocation exports (available worlds: {}) (node: {}; schema-core: {})",
        world_desc,
        node_err.unwrap_or_else(|| "<missing>".to_string()),
        schema_err.unwrap_or_else(|| "<missing>".to_string()),
    ))
}

fn describe_manifest_from_node(
    engine: &Engine,
    component: &Component,
    component_path: &Path,
) -> Result<ProviderManifest> {
    let history = http_mock::new_history();
    let state = TesterHostState::new(HashMap::new(), HttpMode::Mock, history, None);
    eprintln!(
        "node world describe: wasm={} op=get-manifest",
        component_path.display()
    );
    execute_with_state(engine, component, state, |store, instance| {
        let get_manifest_index = node_function_index(&mut *store, instance, "get-manifest")?;
        let get_manifest =
            instance.get_typed_func::<(), (String,)>(&mut *store, get_manifest_index)?;
        let (manifest_json,) = get_manifest.call(&mut *store, ())?;
        let _ = node_function_index(&mut *store, instance, "invoke")?;
        let manifest = match serde_json::from_str::<ProviderManifest>(&manifest_json) {
            Ok(manifest) => manifest,
            Err(parse_err) => fallback_provider_manifest_from_node(&manifest_json, component_path)
                .map_err(|fallback_err| {
                    anyhow!(
                        "failed to parse node manifest: {parse_err}; fallback error: {fallback_err}"
                    )
                })?,
        };
        Ok(manifest)
    })
}

#[derive(Deserialize)]
struct NodeComponentManifest {
    #[serde(default)]
    operations: Vec<NodeComponentOperation>,
    #[serde(default)]
    supports: Vec<String>,
}

#[derive(Deserialize)]
struct NodeComponentOperation {
    name: String,
}

fn fallback_provider_manifest_from_node(
    manifest_json: &str,
    component_path: &Path,
) -> Result<ProviderManifest> {
    let parsed: NodeComponentManifest = serde_json::from_str(manifest_json)
        .context("failed to decode node component manifest for fallback")?;
    let ops = parsed
        .operations
        .into_iter()
        .map(|op| op.name)
        .filter(|name| !name.trim().is_empty())
        .collect();
    let provider_type = component_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|s| s.replace('_', "-"))
        .unwrap_or_else(|| "node-component".to_string());
    Ok(ProviderManifest {
        provider_type,
        capabilities: parsed.supports,
        ops,
        config_schema_ref: None,
        state_schema_ref: None,
    })
}

fn describe_manifest_from_schema(
    engine: &Engine,
    component: &Component,
) -> Result<ProviderManifest> {
    let history = http_mock::new_history();
    let state = TesterHostState::new(HashMap::new(), HttpMode::Mock, history, None);
    execute_with_state(engine, component, state, |store, instance| {
        let api_index = instance
            .get_export_index(&mut *store, None, SCHEMA_CORE_WORLD)
            .context("missing schema-core export")?;
        let describe_index = instance
            .get_export_index(&mut *store, Some(&api_index), "describe")
            .context("missing describe export")?;
        let _ = instance
            .get_export_index(&mut *store, Some(&api_index), "invoke")
            .context("missing schema-core invoke export")?;
        let describe = instance.get_typed_func::<(), (Vec<u8>,)>(&mut *store, describe_index)?;
        let (bytes,) = describe.call(&mut *store, ())?;
        let manifest: ProviderManifest =
            serde_json::from_slice(&bytes).context("failed to parse provider manifest")?;
        Ok(manifest)
    })
}

fn execute_with_state<R>(
    engine: &Engine,
    component: &Component,
    state: TesterHostState,
    action: impl FnOnce(&mut Store<TesterHostState>, &Instance) -> Result<R>,
) -> Result<R> {
    let mut store = Store::new(engine, state);
    let mut linker = Linker::new(engine);
    add_wasi_to_linker(&mut linker);
    add_greentic_hosts(&mut linker)?;
    let instance = linker
        .instantiate(&mut store, component)
        .context("failed to instantiate component")?;
    action(&mut store, &instance)
}

fn add_greentic_hosts(linker: &mut Linker<TesterHostState>) -> Result<()> {
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

fn add_http_client_http_client_world(linker: &mut Linker<TesterHostState>) -> Result<()> {
    let mut inst = linker.instance("greentic:http/http-client@1.1.0")?;
    inst.func_wrap(
        "send",
        move |mut caller: wasmtime::StoreContextMut<'_, TesterHostState>,
              (req, _opts, _ctx): (
            http_client::RequestV1_1,
            Option<http_client::RequestOptionsV1_1>,
            Option<http_client::TenantCtxV1_1>,
        )| {
            let host = http_client_host(caller.data_mut());
            let result = host.send(req, None, None);
            Ok((result,))
        },
    )?;
    Ok(())
}

fn add_http_client_client_world(linker: &mut Linker<TesterHostState>) -> Result<()> {
    let mut inst = linker.instance("greentic:http/client@1.1.0")?;
    inst.func_wrap(
        "send",
        move |mut caller: wasmtime::StoreContextMut<'_, TesterHostState>,
              (req, opts, ctx): (
            http_client_client_alias::Request,
            Option<http_client_client_alias::RequestOptions>,
            Option<http_client_client_alias::TenantCtx>,
        )| {
            let host = http_client_host(caller.data_mut());
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

fn http_client_host(state: &mut TesterHostState) -> &mut dyn http_client::HttpClientHostV1_1 {
    state
}

fn node_world_export(
    store: &mut Store<TesterHostState>,
    instance: &Instance,
) -> Option<ComponentExportIndex> {
    instance.get_export_index(store, None, NODE_WORLD)
}

fn node_function_index(
    store: &mut Store<TesterHostState>,
    instance: &Instance,
    name: &str,
) -> Result<ComponentExportIndex> {
    let parent = node_world_export(store, instance);
    instance
        .get_export_index(store, parent.as_ref(), name)
        .with_context(|| format!("missing node {name} export"))
}

pub struct TesterHostState {
    table: ResourceTable,
    wasi_ctx: WasiCtx,
    secrets: HashMap<String, Vec<u8>>,
    state_store: HashMap<String, Vec<u8>>,
    http_mode: HttpMode,
    http_history: HttpHistory,
    mock_responses: Option<HttpResponseQueue>,
}

impl Default for TesterHostState {
    fn default() -> Self {
        Self::new(
            HashMap::new(),
            HttpMode::Mock,
            http_mock::new_history(),
            None,
        )
    }
}

impl TesterHostState {
    pub fn new(
        secrets: HashMap<String, Vec<u8>>,
        http_mode: HttpMode,
        http_history: HttpHistory,
        mock_responses: Option<HttpResponseQueue>,
    ) -> Self {
        Self {
            table: ResourceTable::new(),
            wasi_ctx: WasiCtxBuilder::new().inherit_stdio().build(),
            secrets,
            state_store: HashMap::new(),
            http_mode,
            http_history,
            mock_responses,
        }
    }
}

impl WasiView for TesterHostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.table,
        }
    }
}

impl http_client::HttpClientHostV1_1 for TesterHostState {
    fn send(
        &mut self,
        req: http_client::RequestV1_1,
        _opts: Option<http_client::RequestOptionsV1_1>,
        _ctx: Option<http_client::TenantCtxV1_1>,
    ) -> Result<http_client::ResponseV1_1, http_client::HttpClientErrorV1_1> {
        let response = match self.http_mode {
            HttpMode::Mock => http_mock::mock_response(self.mock_responses.as_ref()),
            HttpMode::Real => http_mock::send_real_request(&req)?,
        };
        let call = HttpCall {
            request: HttpRequest::from_host(&req),
            response: HttpResponseRecord::from_host(&response),
        };
        if let Ok(mut history) = self.http_history.lock() {
            history.push(call);
        }
        Ok(response)
    }
}

impl secrets_store::SecretsStoreHostV1_1 for TesterHostState {
    fn get(&mut self, key: String) -> Result<Option<Vec<u8>>, secrets_store::SecretsErrorV1_1> {
        Ok(self.secrets.get(&key).cloned())
    }

    fn put(&mut self, key: String, value: Vec<u8>) {
        self.secrets.insert(key, value);
    }
}

impl state_store::StateStoreHost for TesterHostState {
    fn read(
        &mut self,
        key: state_store::StateKey,
        _ctx: Option<state_store::TenantCtx>,
    ) -> Result<Vec<u8>, state_store::StateStoreError> {
        match self.state_store.get(&key) {
            Some(value) => Ok(value.clone()),
            None => Err(state_store::StateStoreError {
                code: "not_found".into(),
                message: format!("state key '{}' not found", key),
            }),
        }
    }

    fn write(
        &mut self,
        key: state_store::StateKey,
        _bytes: Vec<u8>,
        _ctx: Option<state_store::TenantCtx>,
    ) -> Result<state_store::OpAck, state_store::StateStoreError> {
        self.state_store.insert(key, _bytes);
        Ok(state_store::OpAck::Ok)
    }

    fn delete(
        &mut self,
        key: state_store::StateKey,
        _ctx: Option<state_store::TenantCtx>,
    ) -> Result<state_store::OpAck, state_store::StateStoreError> {
        self.state_store.remove(&key);
        Ok(state_store::OpAck::Ok)
    }
}

impl component_node_bindings::greentic::component::control::Host for TesterHostState {
    fn should_cancel(&mut self) -> bool {
        false
    }

    fn yield_now(&mut self) {}
}

fn add_wasi_to_linker(linker: &mut Linker<TesterHostState>) {
    wasmtime_wasi::p2::add_to_linker_sync(linker).expect("add wasi");
}

fn new_engine() -> Engine {
    let mut config = Config::new();
    config.wasm_component_model(true);
    Engine::new(&config).expect("engine")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn find_wasm_path(provider: &str) -> Result<PathBuf> {
    eprintln!("find_wasm_path provider={provider}");
    if let Ok(override_path) = std::env::var("GREENTIC_PROVIDER_WASM") {
        eprintln!("find_wasm_path override={override_path}");
        return Ok(PathBuf::from(override_path));
    }
    let root = workspace_root();
    let provider_dir = root.join(format!("components/messaging-provider-{provider}"));
    let mut candidates = Vec::new();
    let dist_wasm = root
        .join("dist/wasms")
        .join(format!("messaging-provider-{provider}.wasm"));
    if dist_wasm.exists() {
        candidates.push(dist_wasm);
    }
    if let Ok(dist_entries) = fs::read_dir(provider_dir.join("dist")) {
        for entry in dist_entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
                candidates.push(path);
            }
        }
    }
    let builtin = root.join(format!(
        "target/components/messaging-provider-{provider}.wasm"
    ));
    if builtin.exists() {
        candidates.push(builtin);
    }
    for suffix in ["release", "debug"] {
        let path = provider_dir.join(format!(
            "target/wasm32-wasip2/{suffix}/messaging-provider-{provider}.wasm"
        ));
        if path.exists() {
            candidates.push(path);
        }
    }
    let mut best: Option<(PathBuf, SystemTime)> = None;
    for candidate in candidates {
        if let Ok(metadata) = fs::metadata(&candidate) {
            let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if best
                .as_ref()
                .is_none_or(|(_, best_time)| mtime > *best_time)
            {
                best = Some((candidate, mtime));
            }
        }
    }
    best.map(|(path, _)| path)
        .ok_or_else(|| anyhow!("no wasm component found for {}", provider))
}

pub fn find_component_wasm_path(component: &str) -> Result<PathBuf> {
    let root = workspace_root();
    let component_dir = root.join("components").join(component);
    let target_priority = ["wasm32-wasip2"];

    for target in target_priority {
        if let Some(path) = best_target_candidate(&root, &component_dir, component, target) {
            return Ok(path);
        }
    }

    let mut fallback_candidates = Vec::new();
    for name in wasm_name_variants(component) {
        add_candidate_if_exists(
            &mut fallback_candidates,
            root.join("dist/wasms").join(format!("{name}.wasm")),
        );
        add_candidate_if_exists(
            &mut fallback_candidates,
            root.join("target/components").join(format!("{name}.wasm")),
        );
    }

    if let Ok(dist_entries) = fs::read_dir(component_dir.join("dist")) {
        for entry in dist_entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
                add_candidate_if_exists(&mut fallback_candidates, path);
            }
        }
    }

    if let Some(path) = latest_candidate(&fallback_candidates) {
        return Ok(path);
    }

    Err(anyhow!("no wasm component found for {}", component))
}

fn wasm_name_variants(component: &str) -> Vec<String> {
    let mut names = vec![component.to_string()];
    let underscored = component.replace('-', "_");
    if underscored != component {
        names.push(underscored);
    }
    names
}

fn best_target_candidate(
    root: &Path,
    component_dir: &Path,
    component: &str,
    target: &str,
) -> Option<PathBuf> {
    let suffixes = ["release", "debug"];
    let mut candidates = Vec::new();
    for suffix in suffixes {
        for name in wasm_name_variants(component) {
            add_candidate_if_exists(
                &mut candidates,
                root.join("target")
                    .join(target)
                    .join(suffix)
                    .join(format!("{name}.wasm")),
            );
            add_candidate_if_exists(
                &mut candidates,
                root.join("target")
                    .join("components")
                    .join(target)
                    .join(suffix)
                    .join(format!("{name}.wasm")),
            );
            add_candidate_if_exists(
                &mut candidates,
                component_dir
                    .join("target")
                    .join(target)
                    .join(suffix)
                    .join(format!("{name}.wasm")),
            );
        }
    }
    latest_candidate(&candidates)
}

fn add_candidate_if_exists(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    if path.exists() && !candidates.contains(&path) {
        candidates.push(path);
    }
}

fn latest_candidate(candidates: &[PathBuf]) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, SystemTime)> = None;
    for candidate in candidates {
        if let Ok(metadata) = fs::metadata(candidate) {
            let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if best
                .as_ref()
                .is_none_or(|(_, best_time)| mtime > *best_time)
            {
                best = Some((candidate.clone(), mtime));
            }
        }
    }
    best.map(|(path, _)| path)
}

fn manifest_from_component_path(component_path: &Path) -> Option<PathBuf> {
    let workspace = workspace_root();
    let mut dir = component_path.parent();
    while let Some(current) = dir {
        if !current.starts_with(&workspace) {
            break;
        }
        let manifest = current.join("component.manifest.json");
        if manifest.exists() {
            return Some(manifest);
        }
        if current == workspace {
            break;
        }
        dir = current.parent();
    }
    None
}

fn detect_available_worlds(engine: &Engine, component: &Component) -> Result<Vec<&'static str>> {
    let history = http_mock::new_history();
    let state = TesterHostState::new(HashMap::new(), HttpMode::Mock, history, None);
    execute_with_state(engine, component, state, |store, instance| {
        let mut worlds = Vec::new();
        if node_world_export(store, instance).is_some() {
            worlds.push("node");
        }
        if instance
            .get_export_index(store, None, SCHEMA_CORE_WORLD)
            .is_some()
        {
            worlds.push("schema-core");
        }
        Ok(worlds)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use greentic_types::{ChannelMessageEnvelope, EnvId, MessageMetadata, TenantCtx, TenantId};
    use messaging_universal_dto::RenderPlanInV1;
    use serde_json::{Value, json};
    use std::{collections::HashMap, path::PathBuf, process::Command};

    #[test]
    fn node_world_strategy_detected() {
        let wasm = ensure_component_built("telegram-webhook");
        let harness = WasmHarness::new_with_path(&wasm).expect("instantiate node component");
        assert_eq!(harness.invoke_strategy(), InvokeStrategy::Node);
    }

    #[test]
    fn schema_core_strategy_detected() {
        ensure_component_built("messaging-provider-webchat");
        let harness = WasmHarness::new("webchat").expect("instantiate schema-core component");
        assert_eq!(harness.invoke_strategy(), InvokeStrategy::SchemaCore);
    }

    #[test]
    fn node_world_can_invoke_reconcile_webhook() {
        let wasm = ensure_component_built("telegram-webhook");
        let harness = WasmHarness::new_with_path(&wasm).expect("instantiate node component");
        assert_eq!(harness.invoke_strategy(), InvokeStrategy::Node);

        let input = json!({
            "public_base_url": "https://example.invalid/webhook",
            "webhook_path": "",
            "dry_run": true,
        });
        let secrets = HashMap::from([("TELEGRAM_BOT_TOKEN".to_string(), b"token".to_vec())]);
        let history = http_mock::new_history();
        let output = harness
            .invoke(
                "reconcile_webhook",
                serde_json::to_vec(&input).expect("serialize input"),
                &secrets,
                HttpMode::Mock,
                history,
                None,
            )
            .expect("invoke");
        let value: Value = serde_json::from_slice(&output).expect("parse json");
        assert_eq!(value.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(
            value
                .get("expected_url")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "https://example.invalid/webhook"
        );
    }

    #[test]
    fn schema_core_can_invoke_render_plan() {
        ensure_component_built("messaging-provider-webchat");
        let harness = WasmHarness::new("webchat").expect("instantiate schema-core component");
        assert_eq!(harness.invoke_strategy(), InvokeStrategy::SchemaCore);

        let envelope = build_test_envelope("webchat");
        let plan_in = RenderPlanInV1 {
            message: envelope,
            metadata: HashMap::new(),
        };
        let input_bytes = serde_json::to_vec(&plan_in).expect("serialize render_plan input");
        let history = http_mock::new_history();
        let secrets = HashMap::new();
        let output = harness
            .invoke(
                "render_plan",
                input_bytes,
                &secrets,
                HttpMode::Mock,
                history,
                None,
            )
            .expect("render_plan invoke");
        let value: Value = serde_json::from_slice(&output).expect("parse render_plan output");
        assert_eq!(value.get("ok").and_then(Value::as_bool), Some(true));
        assert!(value.get("plan").is_some());
    }

    fn ensure_component_built(package: &str) -> PathBuf {
        let root = workspace_root();
        let wasm_path = root
            .join("target/components")
            .join(format!("{package}.wasm"));
        if !wasm_path.exists() {
            let status = Command::new("cargo")
                .current_dir(&root)
                .args(["component", "build", "-p", package])
                .status()
                .expect("failed to spawn cargo component build");
            assert!(status.success());
        }
        find_component_wasm_path(package).expect("wasm component should exist after build")
    }

    fn build_test_envelope(channel: &str) -> ChannelMessageEnvelope {
        let env = EnvId::try_from("manual").expect("env id");
        let tenant = TenantId::try_from("manual").expect("tenant id");
        let mut metadata = MessageMetadata::new();
        metadata.insert("universal".to_string(), "true".to_string());
        metadata.insert("channel".to_string(), channel.to_string());
        ChannelMessageEnvelope {
            id: format!("tester-{channel}"),
            tenant: TenantCtx::new(env.clone(), tenant.clone()),
            channel: channel.to_string(),
            session_id: channel.to_string(),
            reply_scope: None,
            from: None,
            to: Vec::new(),
            correlation_id: None,
            text: Some("plan input".to_string()),
            attachments: Vec::new(),
            metadata,
        }
    }
}
