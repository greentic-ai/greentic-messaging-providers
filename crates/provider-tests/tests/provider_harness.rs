#![allow(non_snake_case)]
use std::fs;
use std::path::{Path, PathBuf};

use provider_common::{RenderPlan, RenderTier};
use serde::Deserialize;
use serde_json::{Value, json};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

macro_rules! impl_common_hosts {
    ($bindings:ident) => {
        impl $bindings::greentic::http::http_client::Host for HostState {
            fn send(
                &mut self,
                _req: $bindings::greentic::http::http_client::Request,
                _options: Option<$bindings::greentic::http::http_client::RequestOptions>,
                _ctx: Option<$bindings::greentic::interfaces_types::types::TenantCtx>,
            ) -> Result<
                $bindings::greentic::http::http_client::Response,
                $bindings::greentic::http::http_client::HostError,
            > {
                Ok($bindings::greentic::http::http_client::Response {
                    status: 200,
                    headers: vec![],
                    body: None,
                })
            }
        }

        impl $bindings::greentic::secrets_store::secrets_store::Host for HostState {
            fn get(
                &mut self,
                _key: String,
            ) -> Result<
                Option<Vec<u8>>,
                $bindings::greentic::secrets_store::secrets_store::SecretsError,
            > {
                Ok(None)
            }
        }

        impl $bindings::greentic::state::state_store::Host for HostState {
            fn read(
                &mut self,
                _key: $bindings::greentic::interfaces_types::types::StateKey,
                _ctx: Option<$bindings::greentic::interfaces_types::types::TenantCtx>,
            ) -> Result<Vec<u8>, $bindings::greentic::state::state_store::HostError> {
                Err($bindings::greentic::state::state_store::HostError {
                    code: "unimplemented".into(),
                    message: "state not available in tests".into(),
                })
            }

            fn write(
                &mut self,
                _key: $bindings::greentic::interfaces_types::types::StateKey,
                _bytes: Vec<u8>,
                _ctx: Option<$bindings::greentic::interfaces_types::types::TenantCtx>,
            ) -> Result<
                $bindings::greentic::state::state_store::OpAck,
                $bindings::greentic::state::state_store::HostError,
            > {
                Err($bindings::greentic::state::state_store::HostError {
                    code: "unimplemented".into(),
                    message: "state not available in tests".into(),
                })
            }

            fn delete(
                &mut self,
                _key: $bindings::greentic::interfaces_types::types::StateKey,
                _ctx: Option<$bindings::greentic::interfaces_types::types::TenantCtx>,
            ) -> Result<
                $bindings::greentic::state::state_store::OpAck,
                $bindings::greentic::state::state_store::HostError,
            > {
                Err($bindings::greentic::state::state_store::HostError {
                    code: "unimplemented".into(),
                    message: "state not available in tests".into(),
                })
            }
        }

        impl $bindings::greentic::telemetry::logger_api::Host for HostState {
            fn log(
                &mut self,
                _span: $bindings::greentic::interfaces_types::types::SpanContext,
                _fields: Vec<(String, String)>,
                _ctx: Option<$bindings::greentic::interfaces_types::types::TenantCtx>,
            ) -> Result<
                $bindings::greentic::telemetry::logger_api::OpAck,
                $bindings::greentic::telemetry::logger_api::HostError,
            > {
                Ok($bindings::greentic::telemetry::logger_api::OpAck::Ok)
            }
        }

        impl $bindings::provider::common::capabilities::Host for HostState {}
        impl $bindings::provider::common::render::Host for HostState {}
        impl $bindings::greentic::interfaces_types::types::Host for HostState {}
    };
}

macro_rules! to_bindings_plan {
    ($bindings:ident, $plan:expr) => {{
        let plan = $plan;
        $bindings::provider::common::render::RenderPlan {
            tier: match plan.tier {
                RenderTier::TierA => $bindings::provider::common::render::RenderTier::TierA,
                RenderTier::TierB => $bindings::provider::common::render::RenderTier::TierB,
                RenderTier::TierC => $bindings::provider::common::render::RenderTier::TierC,
                RenderTier::TierD => $bindings::provider::common::render::RenderTier::TierD,
            },
            summary_text: plan.summary_text.clone(),
            actions: plan.actions.clone(),
            attachments: plan.attachments.clone(),
            warnings: plan
                .warnings
                .iter()
                .map(|w| $bindings::provider::common::render::RenderWarning {
                    code: w.code.clone(),
                    message: w.message.clone(),
                    path: w.path.clone(),
                })
                .collect(),
            debug_json: plan
                .debug
                .as_ref()
                .map(|d| serde_json::to_string(d).expect("debug json")),
        }
    }};
}

#[derive(Debug, Clone, Copy)]
enum ProviderId {
    Slack,
    Teams,
    Telegram,
    Webchat,
    Webex,
    Whatsapp,
}

#[derive(Deserialize)]
struct WebhookFixture {
    headers: Value,
    body: Value,
}

fn fixtures_root() -> PathBuf {
    workspace_root().join("tests/fixtures")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn packs_root() -> PathBuf {
    workspace_root().join("packs/messaging-provider-bundle/components")
}

fn component_path(provider: ProviderId) -> PathBuf {
    let name = provider.as_str();
    let candidates = [
        workspace_root().join(format!("target/components/{name}.wasm")),
        workspace_root().join(format!("target/wasm32-wasip2/release/{name}.wasm")),
        workspace_root().join(format!("target/wasm32-wasip2/debug/{name}.wasm")),
        workspace_root().join(format!(
            "target/wasm32-wasip2/wasm32-wasip2/release/{name}.wasm"
        )),
        workspace_root().join(format!(
            "target/wasm32-wasip2/wasm32-wasip2/debug/{name}.wasm"
        )),
        workspace_root().join(format!(
            "components/{name}/target/wasm32-wasip2/release/{name}.wasm"
        )),
        workspace_root().join(format!(
            "components/{name}/target/wasm32-wasip2/debug/{name}.wasm"
        )),
        packs_root().join(format!("{name}.wasm")),
    ];

    for path in candidates {
        if path.exists() {
            return path;
        }
    }

    panic!("no component binary found for {name}");
}

fn load_render_plan(name: &str) -> RenderPlan {
    let path = fixtures_root()
        .join("adaptive_cards")
        .join(format!("{name}.json"));
    let raw = fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {path:?}"));
    serde_json::from_str(&raw).expect("render plan json")
}

fn load_render_plan_fixture(name: &str) -> RenderPlan {
    let path = fixtures_root()
        .join("render_plans")
        .join(format!("{name}.json"));
    let raw = fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {path:?}"));
    serde_json::from_str(&raw).expect("render plan json")
}

fn inbound_fixture_names(provider: ProviderId) -> Vec<String> {
    let dir = fixtures_root().join(provider.as_str()).join("inbound");
    if !dir.exists() {
        return vec![];
    }
    let mut names: Vec<String> = fs::read_dir(&dir)
        .expect("fixture dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter_map(|e| {
            e.path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .collect();
    names.sort();
    names
}

fn inbound_expected_fixture_names(provider: ProviderId) -> Vec<String> {
    let dir = fixtures_root()
        .join(provider.as_str())
        .join("inbound_expected");
    if !dir.exists() {
        return vec![];
    }
    let mut names: Vec<String> = fs::read_dir(&dir)
        .expect("fixture dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter_map(|e| {
            e.path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .collect();
    names.sort();
    names
}

fn load_inbound_fixture(provider: ProviderId, name: &str) -> WebhookFixture {
    let path = fixtures_root()
        .join(provider.as_str())
        .join("inbound")
        .join(format!("{name}.json"));
    let raw = fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {path:?}"));
    serde_json::from_str(&raw).expect("fixture json")
}

fn load_inbound_expected_fixture(provider: ProviderId, name: &str) -> Value {
    let path = fixtures_root()
        .join(provider.as_str())
        .join("inbound_expected")
        .join(format!("{name}.json"));
    let raw = fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {path:?}"));
    serde_json::from_str(&raw).expect("expected fixture json")
}

fn outbound_fixture_names(provider: ProviderId) -> Vec<String> {
    let dir = fixtures_root()
        .join("expected_payloads")
        .join(provider.as_str());
    if !dir.exists() {
        return vec![];
    }
    let mut names: Vec<String> = fs::read_dir(&dir)
        .expect("fixture dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter_map(|e| {
            e.path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .collect();
    names.sort();
    names
}

fn load_outbound_expected_fixture(provider: ProviderId, name: &str) -> Value {
    let path = fixtures_root()
        .join("expected_payloads")
        .join(provider.as_str())
        .join(format!("{name}.json"));
    let raw = fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {path:?}"));
    serde_json::from_str(&raw).expect("expected fixture json")
}

fn make_engine() -> Engine {
    let mut config = Config::new();
    config.wasm_component_model(true);
    Engine::new(&config).expect("engine")
}

fn normalize(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut normalized = serde_json::Map::new();
            for (k, v) in entries {
                let mut v = normalize(v);
                if matches!(
                    k.as_str(),
                    "id" | "message_id"
                        | "messageId"
                        | "timestamp"
                        | "ts"
                        | "update_id"
                        | "event_ts"
                ) && (v.is_string() || v.is_number())
                {
                    v = Value::String("<FIXED>".into());
                }
                normalized.insert(k, v);
            }
            Value::Object(normalized)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
        other => other,
    }
}

fn parse_body(content_type: &str, body: &[u8]) -> Value {
    if body.is_empty() {
        return Value::Null;
    }
    if content_type.contains("json") {
        serde_json::from_slice(body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()))
    } else {
        Value::String(String::from_utf8_lossy(body).into_owned())
    }
}

fn parse_metadata(metadata_json: &Option<String>) -> Value {
    if let Some(meta) = metadata_json {
        serde_json::from_str(meta).unwrap_or_else(|_| Value::String(meta.clone()))
    } else {
        Value::Null
    }
}

fn normalize_outbound_expected(value: Value) -> Value {
    let mut out = value;
    if let Some(body) = out.get_mut("body_json") {
        let normalized = normalize(body.take());
        *body = normalized;
    }
    out
}

fn encode_to_expected_shape(encoded: Value) -> Value {
    let content_type = encoded
        .get("content_type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let warnings = encoded
        .get("warnings")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|w| w.get("code").and_then(Value::as_str))
                .map(|code| Value::String(code.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut out = serde_json::Map::new();
    out.insert("content_type".into(), Value::String(content_type));
    out.insert("warnings".into(), Value::Array(warnings));
    if let Some(body) = encoded.get("body") {
        if body.is_string() {
            out.insert("body_text".into(), body.clone());
        } else {
            out.insert("body_json".into(), normalize(body.clone()));
        }
    }
    Value::Object(out)
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
        Self::new()
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
    wasmtime_wasi::p2::add_to_linker_sync(linker).expect("wasi");
}

mod slack {
    use super::*;

    mod bindings {
        wasmtime::component::bindgen!({
            path: "../../components/slack/wit/slack",
            world: "slack",
        });
    }
    impl_common_hosts!(bindings);

    pub struct Harness {
        store: Store<HostState>,
        bindings: bindings::Slack,
    }

    impl Harness {
        pub fn new(engine: &Engine, component_path: &Path) -> Self {
            let component = Component::from_file(engine, component_path).expect("component");
            let mut linker = Linker::new(engine);
            add_wasi_to_linker(&mut linker);
            bindings::Slack::add_to_linker::<_, HasSelf<HostState>>(
                &mut linker,
                |s: &mut HostState| s,
            )
            .expect("linker");
            let mut store = Store::new(engine, HostState::default());
            let bindings =
                bindings::Slack::instantiate(&mut store, &component, &linker).expect("instance");
            bindings
                .call_init_runtime_config(&mut store, "{}")
                .expect("call")
                .expect("init");
            Self { store, bindings }
        }

        pub fn encode(&mut self, plan: &RenderPlan) -> Value {
            let plan = to_bindings_plan!(bindings, plan);
            let res = self
                .bindings
                .call_encode(&mut self.store, &plan)
                .expect("encode");
            let warnings: Vec<Value> = res
                .warnings
                .into_iter()
                .map(|w| {
                    json!({
                        "code": w.code,
                        "message": w.message,
                        "path": w.path
                    })
                })
                .collect();
            json!({
                "content_type": res.payload.content_type,
                "body": parse_body(&res.payload.content_type, &res.payload.body),
                "metadata": parse_metadata(&res.payload.metadata_json),
                "warnings": warnings
            })
        }

        pub fn handle_webhook(&mut self, headers: &Value, body: &Value) -> Value {
            let headers_json = serde_json::to_string(headers).expect("headers json");
            let body_json = serde_json::to_string(body).expect("body json");
            let res = self
                .bindings
                .call_handle_webhook(&mut self.store, &headers_json, &body_json)
                .expect("handle");
            match res {
                Ok(json) => serde_json::from_str(&json).expect("normalized json"),
                Err(err) => json!({ "error": err }),
            }
        }
    }
}

mod teams {
    use super::*;

    mod bindings {
        wasmtime::component::bindgen!({
            path: "../../components/teams/wit/teams",
            world: "teams",
        });
    }
    impl_common_hosts!(bindings);

    pub struct Harness {
        store: Store<HostState>,
        bindings: bindings::Teams,
    }

    impl Harness {
        pub fn new(engine: &Engine, component_path: &Path) -> Self {
            let component = Component::from_file(engine, component_path).expect("component");
            let mut linker = Linker::new(engine);
            add_wasi_to_linker(&mut linker);
            bindings::Teams::add_to_linker::<_, HasSelf<HostState>>(
                &mut linker,
                |s: &mut HostState| s,
            )
            .expect("linker");
            let mut store = Store::new(engine, HostState::default());
            let bindings =
                bindings::Teams::instantiate(&mut store, &component, &linker).expect("instance");
            bindings
                .call_init_runtime_config(&mut store, "{}")
                .expect("call")
                .expect("init");
            Self { store, bindings }
        }

        pub fn encode(&mut self, plan: &RenderPlan) -> Value {
            let plan = to_bindings_plan!(bindings, plan);
            let res = self
                .bindings
                .call_encode(&mut self.store, &plan)
                .expect("encode");
            let warnings: Vec<Value> = res
                .warnings
                .into_iter()
                .map(|w| {
                    json!({
                        "code": w.code,
                        "message": w.message,
                        "path": w.path
                    })
                })
                .collect();
            json!({
                "content_type": res.payload.content_type,
                "body": parse_body(&res.payload.content_type, &res.payload.body),
                "metadata": parse_metadata(&res.payload.metadata_json),
                "warnings": warnings
            })
        }

        pub fn handle_webhook(&mut self, headers: &Value, body: &Value) -> Value {
            let headers_json = serde_json::to_string(headers).expect("headers json");
            let body_json = serde_json::to_string(body).expect("body json");
            let res = self
                .bindings
                .call_handle_webhook(&mut self.store, &headers_json, &body_json)
                .expect("handle");
            match res {
                Ok(json) => serde_json::from_str(&json).expect("normalized json"),
                Err(err) => json!({ "error": err }),
            }
        }
    }
}

mod telegram {
    use super::*;

    mod bindings {
        wasmtime::component::bindgen!({
            path: "../../components/telegram/wit/telegram",
            world: "telegram",
        });
    }
    impl_common_hosts!(bindings);
    impl bindings::provider::telegram::types::Host for HostState {}

    pub struct Harness {
        store: Store<HostState>,
        bindings: bindings::Telegram,
    }

    impl Harness {
        pub fn new(engine: &Engine, component_path: &Path) -> Self {
            let component = Component::from_file(engine, component_path).expect("component");
            let mut linker = Linker::new(engine);
            add_wasi_to_linker(&mut linker);
            bindings::Telegram::add_to_linker::<_, HasSelf<HostState>>(
                &mut linker,
                |s: &mut HostState| s,
            )
            .expect("linker");
            let mut store = Store::new(engine, HostState::default());
            let bindings =
                bindings::Telegram::instantiate(&mut store, &component, &linker).expect("instance");
            bindings
                .call_init_runtime_config(&mut store, "{}")
                .expect("call")
                .expect("init");
            Self { store, bindings }
        }

        pub fn encode(&mut self, plan: &RenderPlan) -> Value {
            let plan = to_bindings_plan!(bindings, plan);
            let res = self
                .bindings
                .call_encode(&mut self.store, &plan)
                .expect("encode");
            let warnings: Vec<Value> = res
                .warnings
                .into_iter()
                .map(|w| {
                    json!({
                        "code": w.code,
                        "message": w.message,
                        "path": w.path
                    })
                })
                .collect();
            json!({
                "content_type": res.payload.content_type,
                "body": parse_body(&res.payload.content_type, &res.payload.body),
                "metadata": parse_metadata(&res.payload.metadata_json),
                "warnings": warnings
            })
        }

        pub fn handle_webhook(&mut self, headers: &Value, body: &Value) -> Value {
            let headers_json = serde_json::to_string(headers).expect("headers json");
            let body_json = serde_json::to_string(body).expect("body json");
            let res = self
                .bindings
                .call_handle_webhook(&mut self.store, &headers_json, &body_json)
                .expect("handle");
            match res {
                Ok(res) => {
                    let normalized = res
                        .normalized_event_json
                        .as_deref()
                        .map(|s| serde_json::from_str::<Value>(s).expect("normalized event"));
                    json!({
                        "validation": match res.validation {
                            bindings::provider::telegram::types::ValidationOutcome::Ok => Value::String("ok".into()),
                            bindings::provider::telegram::types::ValidationOutcome::Reject(msg) => json!({"reject": msg}),
                            bindings::provider::telegram::types::ValidationOutcome::Warn(msg) => json!({"warn": msg}),
                        },
                        "normalized_event": normalized.unwrap_or(Value::Null),
                        "warnings": res.warnings,
                        "suggested_http_status": res.suggested_http_status
                    })
                }
                Err(err) => json!({ "error": err }),
            }
        }
    }
}

mod webchat {
    use super::*;

    mod bindings {
        wasmtime::component::bindgen!({
            path: "../../components/webchat/wit/webchat",
            world: "webchat",
        });
    }
    impl_common_hosts!(bindings);

    pub struct Harness {
        store: Store<HostState>,
        bindings: bindings::Webchat,
    }

    impl Harness {
        pub fn new(engine: &Engine, component_path: &Path) -> Self {
            let component = Component::from_file(engine, component_path).expect("component");
            let mut linker = Linker::new(engine);
            add_wasi_to_linker(&mut linker);
            bindings::Webchat::add_to_linker::<_, HasSelf<HostState>>(
                &mut linker,
                |s: &mut HostState| s,
            )
            .expect("linker");
            let mut store = Store::new(engine, HostState::default());
            let bindings =
                bindings::Webchat::instantiate(&mut store, &component, &linker).expect("instance");
            bindings
                .call_init_runtime_config(&mut store, "{}")
                .expect("call")
                .expect("init");
            Self { store, bindings }
        }

        pub fn encode(&mut self, plan: &RenderPlan) -> Value {
            let plan = to_bindings_plan!(bindings, plan);
            let res = self
                .bindings
                .call_encode(&mut self.store, &plan)
                .expect("encode");
            let warnings: Vec<Value> = res
                .warnings
                .into_iter()
                .map(|w| {
                    json!({
                        "code": w.code,
                        "message": w.message,
                        "path": w.path
                    })
                })
                .collect();
            json!({
                "content_type": res.payload.content_type,
                "body": parse_body(&res.payload.content_type, &res.payload.body),
                "metadata": parse_metadata(&res.payload.metadata_json),
                "warnings": warnings
            })
        }

        pub fn handle_webhook(&mut self, headers: &Value, body: &Value) -> Value {
            let headers_json = serde_json::to_string(headers).expect("headers json");
            let body_json = serde_json::to_string(body).expect("body json");
            let res = self
                .bindings
                .call_handle_webhook(&mut self.store, &headers_json, &body_json)
                .expect("handle");
            match res {
                Ok(json) => serde_json::from_str(&json).expect("normalized json"),
                Err(err) => json!({ "error": err }),
            }
        }
    }
}

mod webex {
    use super::*;

    mod bindings {
        wasmtime::component::bindgen!({
            path: "../../components/webex/wit/webex",
            world: "webex",
        });
    }
    impl_common_hosts!(bindings);

    pub struct Harness {
        store: Store<HostState>,
        bindings: bindings::Webex,
    }

    impl Harness {
        pub fn new(engine: &Engine, component_path: &Path) -> Self {
            let component = Component::from_file(engine, component_path).expect("component");
            let mut linker = Linker::new(engine);
            add_wasi_to_linker(&mut linker);
            bindings::Webex::add_to_linker::<_, HasSelf<HostState>>(
                &mut linker,
                |s: &mut HostState| s,
            )
            .expect("linker");
            let mut store = Store::new(engine, HostState::default());
            let bindings =
                bindings::Webex::instantiate(&mut store, &component, &linker).expect("instance");
            bindings
                .call_init_runtime_config(&mut store, "{}")
                .expect("call")
                .expect("init");
            Self { store, bindings }
        }

        pub fn encode(&mut self, plan: &RenderPlan) -> Value {
            let plan = to_bindings_plan!(bindings, plan);
            let res = self
                .bindings
                .call_encode(&mut self.store, &plan)
                .expect("encode");
            let warnings: Vec<Value> = res
                .warnings
                .into_iter()
                .map(|w| {
                    json!({
                        "code": w.code,
                        "message": w.message,
                        "path": w.path
                    })
                })
                .collect();
            json!({
                "content_type": res.payload.content_type,
                "body": parse_body(&res.payload.content_type, &res.payload.body),
                "metadata": parse_metadata(&res.payload.metadata_json),
                "warnings": warnings
            })
        }

        pub fn handle_webhook(&mut self, headers: &Value, body: &Value) -> Value {
            let headers_json = serde_json::to_string(headers).expect("headers json");
            let body_json = serde_json::to_string(body).expect("body json");
            let res = self
                .bindings
                .call_handle_webhook(&mut self.store, &headers_json, &body_json)
                .expect("handle");
            match res {
                Ok(json) => serde_json::from_str(&json).expect("normalized json"),
                Err(err) => json!({ "error": err }),
            }
        }
    }
}

mod whatsapp {
    use super::*;

    mod bindings {
        wasmtime::component::bindgen!({
            path: "../../components/whatsapp/wit/whatsapp",
            world: "whatsapp",
        });
    }
    impl_common_hosts!(bindings);

    pub struct Harness {
        store: Store<HostState>,
        bindings: bindings::Whatsapp,
    }

    impl Harness {
        pub fn new(engine: &Engine, component_path: &Path) -> Self {
            let component = Component::from_file(engine, component_path).expect("component");
            let mut linker = Linker::new(engine);
            add_wasi_to_linker(&mut linker);
            bindings::Whatsapp::add_to_linker::<_, HasSelf<HostState>>(
                &mut linker,
                |s: &mut HostState| s,
            )
            .expect("linker");
            let mut store = Store::new(engine, HostState::default());
            let bindings =
                bindings::Whatsapp::instantiate(&mut store, &component, &linker).expect("instance");
            bindings
                .call_init_runtime_config(&mut store, "{}")
                .expect("call")
                .expect("init");
            Self { store, bindings }
        }

        pub fn encode(&mut self, plan: &RenderPlan) -> Value {
            let plan = to_bindings_plan!(bindings, plan);
            let res = self
                .bindings
                .call_encode(&mut self.store, &plan)
                .expect("encode");
            let warnings: Vec<Value> = res
                .warnings
                .into_iter()
                .map(|w| {
                    json!({
                        "code": w.code,
                        "message": w.message,
                        "path": w.path
                    })
                })
                .collect();
            json!({
                "content_type": res.payload.content_type,
                "body": parse_body(&res.payload.content_type, &res.payload.body),
                "metadata": parse_metadata(&res.payload.metadata_json),
                "warnings": warnings
            })
        }

        pub fn handle_webhook(&mut self, headers: &Value, body: &Value) -> Value {
            let headers_json = serde_json::to_string(headers).expect("headers json");
            let body_json = serde_json::to_string(body).expect("body json");
            let res = self
                .bindings
                .call_handle_webhook(&mut self.store, &headers_json, &body_json)
                .expect("handle");
            match res {
                Ok(json) => serde_json::from_str(&json).expect("normalized json"),
                Err(err) => json!({ "error": err }),
            }
        }
    }
}

enum ProviderHarness {
    Slack(slack::Harness),
    Teams(teams::Harness),
    Telegram(telegram::Harness),
    Webchat(webchat::Harness),
    Webex(webex::Harness),
    Whatsapp(whatsapp::Harness),
}

impl ProviderHarness {
    fn new(provider: ProviderId, engine: &Engine) -> Self {
        let component_path = component_path(provider);
        match provider {
            ProviderId::Slack => {
                ProviderHarness::Slack(slack::Harness::new(engine, &component_path))
            }
            ProviderId::Teams => {
                ProviderHarness::Teams(teams::Harness::new(engine, &component_path))
            }
            ProviderId::Telegram => {
                ProviderHarness::Telegram(telegram::Harness::new(engine, &component_path))
            }
            ProviderId::Webchat => {
                ProviderHarness::Webchat(webchat::Harness::new(engine, &component_path))
            }
            ProviderId::Webex => {
                ProviderHarness::Webex(webex::Harness::new(engine, &component_path))
            }
            ProviderId::Whatsapp => {
                ProviderHarness::Whatsapp(whatsapp::Harness::new(engine, &component_path))
            }
        }
    }

    fn encode(&mut self, plan: &RenderPlan) -> Value {
        match self {
            ProviderHarness::Slack(h) => h.encode(plan),
            ProviderHarness::Teams(h) => h.encode(plan),
            ProviderHarness::Telegram(h) => h.encode(plan),
            ProviderHarness::Webchat(h) => h.encode(plan),
            ProviderHarness::Webex(h) => h.encode(plan),
            ProviderHarness::Whatsapp(h) => h.encode(plan),
        }
    }

    fn handle_webhook(&mut self, headers: &Value, body: &Value) -> Value {
        match self {
            ProviderHarness::Slack(h) => h.handle_webhook(headers, body),
            ProviderHarness::Teams(h) => h.handle_webhook(headers, body),
            ProviderHarness::Telegram(h) => h.handle_webhook(headers, body),
            ProviderHarness::Webchat(h) => h.handle_webhook(headers, body),
            ProviderHarness::Webex(h) => h.handle_webhook(headers, body),
            ProviderHarness::Whatsapp(h) => h.handle_webhook(headers, body),
        }
    }
}

impl ProviderId {
    fn as_str(&self) -> &'static str {
        match self {
            ProviderId::Slack => "slack",
            ProviderId::Teams => "teams",
            ProviderId::Telegram => "telegram",
            ProviderId::Webchat => "webchat",
            ProviderId::Webex => "webex",
            ProviderId::Whatsapp => "whatsapp",
        }
    }
}

fn run_adaptive_snapshot(provider: ProviderId, case: &str) {
    let engine = make_engine();
    let mut harness = ProviderHarness::new(provider, &engine);
    let plan = load_render_plan(case);
    let value = normalize(harness.encode(&plan));
    insta::assert_json_snapshot!(
        format!(
            "adaptivecard_translation_snapshot_{}__{}",
            provider.as_str(),
            case
        ),
        value
    );
}

fn run_inbound_snapshots(provider: ProviderId) {
    let engine = make_engine();
    let mut harness = ProviderHarness::new(provider, &engine);
    for fixture in inbound_fixture_names(provider) {
        let fixture_data = load_inbound_fixture(provider, &fixture);
        let value = normalize(harness.handle_webhook(&fixture_data.headers, &fixture_data.body));
        insta::assert_json_snapshot!(
            format!("inbound_snapshot_{}__{}", provider.as_str(), fixture),
            value
        );
    }
}

fn run_inbound_fixture_expectations(provider: ProviderId) {
    let engine = make_engine();
    let mut harness = ProviderHarness::new(provider, &engine);
    for fixture in inbound_expected_fixture_names(provider) {
        let fixture_data = load_inbound_fixture(provider, &fixture);
        let expected = normalize(load_inbound_expected_fixture(provider, &fixture));
        let actual = normalize(harness.handle_webhook(&fixture_data.headers, &fixture_data.body));
        assert_eq!(
            actual,
            expected,
            "inbound fixture mismatch for {} {}",
            provider.as_str(),
            fixture
        );
    }
}

fn run_outbound_fixture_expectations(provider: ProviderId) {
    let engine = make_engine();
    let mut harness = ProviderHarness::new(provider, &engine);
    for fixture in outbound_fixture_names(provider) {
        let plan = load_render_plan_fixture(&fixture);
        let expected =
            normalize_outbound_expected(load_outbound_expected_fixture(provider, &fixture));
        let actual = encode_to_expected_shape(harness.encode(&plan));
        assert_eq!(
            actual,
            expected,
            "outbound fixture mismatch for {} {}",
            provider.as_str(),
            fixture
        );
    }
}

#[test]
fn adaptivecard_translation_snapshot_slack__basic() {
    run_adaptive_snapshot(ProviderId::Slack, "adaptivecard_basic");
}

#[test]
fn adaptivecard_translation_snapshot_slack__inputs() {
    run_adaptive_snapshot(ProviderId::Slack, "adaptivecard_inputs");
}

#[test]
fn adaptivecard_translation_snapshot_slack__actions() {
    run_adaptive_snapshot(ProviderId::Slack, "adaptivecard_actions");
}

#[test]
fn adaptivecard_translation_snapshot_slack__columns() {
    run_adaptive_snapshot(ProviderId::Slack, "adaptivecard_columns");
}

#[test]
fn adaptivecard_translation_snapshot_teams__basic() {
    run_adaptive_snapshot(ProviderId::Teams, "adaptivecard_basic");
}

#[test]
fn adaptivecard_translation_snapshot_teams__inputs() {
    run_adaptive_snapshot(ProviderId::Teams, "adaptivecard_inputs");
}

#[test]
fn adaptivecard_translation_snapshot_teams__actions() {
    run_adaptive_snapshot(ProviderId::Teams, "adaptivecard_actions");
}

#[test]
fn adaptivecard_translation_snapshot_teams__columns() {
    run_adaptive_snapshot(ProviderId::Teams, "adaptivecard_columns");
}

#[test]
fn adaptivecard_translation_snapshot_telegram__basic() {
    run_adaptive_snapshot(ProviderId::Telegram, "adaptivecard_basic");
}

#[test]
fn adaptivecard_translation_snapshot_telegram__inputs() {
    run_adaptive_snapshot(ProviderId::Telegram, "adaptivecard_inputs");
}

#[test]
fn adaptivecard_translation_snapshot_telegram__actions() {
    run_adaptive_snapshot(ProviderId::Telegram, "adaptivecard_actions");
}

#[test]
fn adaptivecard_translation_snapshot_telegram__columns() {
    run_adaptive_snapshot(ProviderId::Telegram, "adaptivecard_columns");
}

#[test]
fn adaptivecard_translation_snapshot_webchat__basic() {
    run_adaptive_snapshot(ProviderId::Webchat, "adaptivecard_basic");
}

#[test]
fn adaptivecard_translation_snapshot_webchat__inputs() {
    run_adaptive_snapshot(ProviderId::Webchat, "adaptivecard_inputs");
}

#[test]
fn adaptivecard_translation_snapshot_webchat__actions() {
    run_adaptive_snapshot(ProviderId::Webchat, "adaptivecard_actions");
}

#[test]
fn adaptivecard_translation_snapshot_webchat__columns() {
    run_adaptive_snapshot(ProviderId::Webchat, "adaptivecard_columns");
}

#[test]
fn adaptivecard_translation_snapshot_webex__basic() {
    run_adaptive_snapshot(ProviderId::Webex, "adaptivecard_basic");
}

#[test]
fn adaptivecard_translation_snapshot_webex__inputs() {
    run_adaptive_snapshot(ProviderId::Webex, "adaptivecard_inputs");
}

#[test]
fn adaptivecard_translation_snapshot_webex__actions() {
    run_adaptive_snapshot(ProviderId::Webex, "adaptivecard_actions");
}

#[test]
fn adaptivecard_translation_snapshot_webex__columns() {
    run_adaptive_snapshot(ProviderId::Webex, "adaptivecard_columns");
}

#[test]
fn adaptivecard_translation_snapshot_whatsapp__basic() {
    run_adaptive_snapshot(ProviderId::Whatsapp, "adaptivecard_basic");
}

#[test]
fn adaptivecard_translation_snapshot_whatsapp__inputs() {
    run_adaptive_snapshot(ProviderId::Whatsapp, "adaptivecard_inputs");
}

#[test]
fn adaptivecard_translation_snapshot_whatsapp__actions() {
    run_adaptive_snapshot(ProviderId::Whatsapp, "adaptivecard_actions");
}

#[test]
fn adaptivecard_translation_snapshot_whatsapp__columns() {
    run_adaptive_snapshot(ProviderId::Whatsapp, "adaptivecard_columns");
}

#[test]
fn inbound_snapshots_slack() {
    run_inbound_snapshots(ProviderId::Slack);
}

#[test]
fn inbound_snapshots_teams() {
    run_inbound_snapshots(ProviderId::Teams);
}

#[test]
fn inbound_snapshots_telegram() {
    run_inbound_snapshots(ProviderId::Telegram);
}

#[test]
fn inbound_snapshots_webchat() {
    run_inbound_snapshots(ProviderId::Webchat);
}

#[test]
fn inbound_snapshots_webex() {
    run_inbound_snapshots(ProviderId::Webex);
}

#[test]
fn inbound_snapshots_whatsapp() {
    run_inbound_snapshots(ProviderId::Whatsapp);
}

#[test]
fn inbound_fixture_expectations_slack() {
    run_inbound_fixture_expectations(ProviderId::Slack);
}

#[test]
fn inbound_fixture_expectations_teams() {
    run_inbound_fixture_expectations(ProviderId::Teams);
}

#[test]
fn inbound_fixture_expectations_telegram() {
    run_inbound_fixture_expectations(ProviderId::Telegram);
}

#[test]
fn inbound_fixture_expectations_webchat() {
    run_inbound_fixture_expectations(ProviderId::Webchat);
}

#[test]
fn inbound_fixture_expectations_webex() {
    run_inbound_fixture_expectations(ProviderId::Webex);
}

#[test]
fn inbound_fixture_expectations_whatsapp() {
    run_inbound_fixture_expectations(ProviderId::Whatsapp);
}

#[test]
fn outbound_fixture_expectations_slack() {
    run_outbound_fixture_expectations(ProviderId::Slack);
}

#[test]
fn outbound_fixture_expectations_teams() {
    run_outbound_fixture_expectations(ProviderId::Teams);
}

#[test]
fn outbound_fixture_expectations_telegram() {
    run_outbound_fixture_expectations(ProviderId::Telegram);
}

#[test]
fn outbound_fixture_expectations_webchat() {
    run_outbound_fixture_expectations(ProviderId::Webchat);
}

#[test]
fn outbound_fixture_expectations_webex() {
    run_outbound_fixture_expectations(ProviderId::Webex);
}

#[test]
fn outbound_fixture_expectations_whatsapp() {
    run_outbound_fixture_expectations(ProviderId::Whatsapp);
}
