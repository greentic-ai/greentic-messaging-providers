use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use wasmtime::component::{
    Component, ComponentExportIndex, HasSelf, Linker, ResourceTable, TypedFunc,
};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn ensure_component_artifact(package: &str, artifact_name: &str) -> Result<PathBuf> {
    let root = workspace_root();
    let candidates = vec![
        root.join(format!("target/components/{artifact_name}.wasm")),
        root.join(format!(
            "target/wasm32-wasip2/release/{}.wasm",
            artifact_name.replace('-', "_")
        )),
        root.join(format!(
            "target/wasm32-wasip2/wasm32-wasip2/release/{}.wasm",
            artifact_name.replace('-', "_")
        )),
        root.join(format!(
            "components/{package}/target/wasm32-wasip2/release/{}.wasm",
            artifact_name.replace('-', "_")
        )),
        root.join(format!(
            "packs/messaging-slack/components/{artifact_name}.wasm"
        )),
        root.join(format!(
            "packs/messaging-teams/components/{artifact_name}.wasm"
        )),
        root.join(format!(
            "packs/messaging-telegram/components/{artifact_name}.wasm"
        )),
        root.join(format!(
            "packs/messaging-whatsapp/components/{artifact_name}.wasm"
        )),
    ];

    for path in candidates.iter() {
        if path.exists() {
            return Ok(path.clone());
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
            package,
        ])
        .current_dir(&root)
        .status()
        .context(format!("running cargo component build for {package}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "cargo component build failed with status {status}"
        ));
    }

    for path in candidates {
        if path.exists() {
            let target = root.join(format!("target/components/{artifact_name}.wasm"));
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

fn add_wasi_to_linker<T: WasiView>(linker: &mut Linker<T>) {
    wasmtime_wasi::p2::add_to_linker_sync(linker).expect("add wasi");
}

mod slack {
    use super::*;

    mod bindings {
        wasmtime::component::bindgen!({
            path: "../../components/messaging-ingress-slack/wit/messaging-ingress-slack",
            world: "messaging-ingress-slack",
        });
    }

    #[derive(Default)]
    struct HostState {
        table: ResourceTable,
        wasi_ctx: WasiCtx,
        secrets: HashMap<String, String>,
    }

    impl HostState {
        fn with_secret(key: &str, value: &str) -> Self {
            let mut secrets = HashMap::new();
            secrets.insert(key.to_string(), value.to_string());
            Self {
                table: ResourceTable::new(),
                wasi_ctx: WasiCtxBuilder::new().inherit_stdio().build(),
                secrets,
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
            _req: bindings::greentic::http::http_client::Request,
            _options: Option<bindings::greentic::http::http_client::RequestOptions>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::http::http_client::Response,
            bindings::greentic::http::http_client::HostError,
        > {
            Ok(bindings::greentic::http::http_client::Response {
                status: 200,
                headers: vec![],
                body: None,
            })
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

    impl bindings::greentic::state::state_store::Host for HostState {
        fn read(
            &mut self,
            _key: bindings::greentic::interfaces_types::types::StateKey,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<Vec<u8>, bindings::greentic::state::state_store::HostError> {
            Err(bindings::greentic::state::state_store::HostError {
                code: "unavailable".into(),
                message: "state not available".into(),
            })
        }

        fn write(
            &mut self,
            _key: bindings::greentic::interfaces_types::types::StateKey,
            _bytes: Vec<u8>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::state::state_store::OpAck,
            bindings::greentic::state::state_store::HostError,
        > {
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

    impl bindings::greentic::telemetry::logger_api::Host for HostState {
        fn log(
            &mut self,
            _span: bindings::greentic::interfaces_types::types::SpanContext,
            _fields: Vec<(String, String)>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::telemetry::logger_api::OpAck,
            bindings::greentic::telemetry::logger_api::HostError,
        > {
            Ok(bindings::greentic::telemetry::logger_api::OpAck::Ok)
        }
    }

    impl bindings::greentic::interfaces_types::types::Host for HostState {}

    #[test]
    fn handles_webhook_without_secret() -> Result<()> {
        let path = ensure_component_artifact("messaging-ingress-slack", "messaging-ingress-slack")?;
        let engine = new_engine();
        let component = Component::from_file(&engine, &path).context("loading component")?;
        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        bindings::greentic::http::http_client::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link http");
        bindings::greentic::secrets_store::secrets_store::add_to_linker::<
            HostState,
            HasSelf<HostState>,
        >(&mut linker, |state| state)
        .expect("link secrets");
        bindings::greentic::state::state_store::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link state");
        bindings::greentic::telemetry::logger_api::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link logger");
        bindings::greentic::interfaces_types::types::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link interfaces");

        let mut store = Store::new(&engine, HostState::default());
        let instance = linker
            .instantiate(&mut store, &component)
            .context("instantiate")?;
        let ingress_index: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "provider:common/ingress@0.0.2")
            .context("get ingress export index")?;
        let handle_index = instance
            .get_export_index(&mut store, Some(&ingress_index), "handle-webhook")
            .context("get handle-webhook export index")?;
        let handle: TypedFunc<(String, String), (Result<String, String>,)> = instance
            .get_typed_func(&mut store, handle_index)
            .context("get handle-webhook func")?;
        let headers = json!({});
        let body = json!({"event": {"type": "message"}});
        let (res,) = handle
            .call(&mut store, (headers.to_string(), body.to_string()))
            .context("call handle_webhook")?;
        assert!(res.is_ok(), "expected ok response");
        Ok(())
    }

    #[test]
    fn handles_webhook_with_secret() -> Result<()> {
        let path = ensure_component_artifact("messaging-ingress-slack", "messaging-ingress-slack")?;
        let engine = new_engine();
        let component = Component::from_file(&engine, &path).context("loading component")?;
        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        bindings::greentic::http::http_client::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link http");
        bindings::greentic::secrets_store::secrets_store::add_to_linker::<
            HostState,
            HasSelf<HostState>,
        >(&mut linker, |state| state)
        .expect("link secrets");
        bindings::greentic::state::state_store::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link state");
        bindings::greentic::telemetry::logger_api::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link logger");
        bindings::greentic::interfaces_types::types::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link interfaces");

        let secret = "signing-secret";
        let body = r#"{"type":"event_callback"}"#;
        let timestamp = "1700000000";
        let basestring = format!("v0:{timestamp}:{body}");
        let signature = slack_signature(secret, &basestring);
        let headers = json!({
            "x-slack-signature": signature,
            "x-slack-request-timestamp": timestamp,
        });

        let mut store = Store::new(
            &engine,
            HostState::with_secret("SLACK_SIGNING_SECRET", secret),
        );
        let instance = linker
            .instantiate(&mut store, &component)
            .context("instantiate")?;
        let ingress_index: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "provider:common/ingress@0.0.2")
            .context("get ingress export index")?;
        let handle_index = instance
            .get_export_index(&mut store, Some(&ingress_index), "handle-webhook")
            .context("get handle-webhook export index")?;
        let handle: TypedFunc<(String, String), (Result<String, String>,)> = instance
            .get_typed_func(&mut store, handle_index)
            .context("get handle-webhook func")?;
        let (res,) = handle
            .call(&mut store, (headers.to_string(), body.to_string()))
            .context("call handle_webhook")?;
        assert!(res.is_ok(), "expected ok response");
        Ok(())
    }

    fn slack_signature(secret: &str, basestring: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac");
        mac.update(basestring.as_bytes());
        let bytes = mac.finalize().into_bytes();
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push_str(&format!("{:02x}", b));
        }
        format!("v0={out}")
    }
}

mod telegram {
    use super::*;

    mod bindings {
        wasmtime::component::bindgen!({
            path: "../../components/messaging-ingress-telegram/wit/messaging-ingress-telegram",
            world: "messaging-ingress-telegram",
        });
    }

    #[derive(Default)]
    struct HostState {
        table: ResourceTable,
        wasi_ctx: WasiCtx,
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
            _req: bindings::greentic::http::http_client::Request,
            _options: Option<bindings::greentic::http::http_client::RequestOptions>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::http::http_client::Response,
            bindings::greentic::http::http_client::HostError,
        > {
            Ok(bindings::greentic::http::http_client::Response {
                status: 200,
                headers: vec![],
                body: None,
            })
        }
    }

    impl bindings::greentic::secrets_store::secrets_store::Host for HostState {
        fn get(
            &mut self,
            _key: String,
        ) -> Result<Option<Vec<u8>>, bindings::greentic::secrets_store::secrets_store::SecretsError>
        {
            Ok(None)
        }
    }

    impl bindings::greentic::state::state_store::Host for HostState {
        fn read(
            &mut self,
            _key: bindings::greentic::interfaces_types::types::StateKey,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<Vec<u8>, bindings::greentic::state::state_store::HostError> {
            Ok(Vec::new())
        }

        fn write(
            &mut self,
            _key: bindings::greentic::interfaces_types::types::StateKey,
            _bytes: Vec<u8>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::state::state_store::OpAck,
            bindings::greentic::state::state_store::HostError,
        > {
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

    impl bindings::greentic::telemetry::logger_api::Host for HostState {
        fn log(
            &mut self,
            _span: bindings::greentic::interfaces_types::types::SpanContext,
            _fields: Vec<(String, String)>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::telemetry::logger_api::OpAck,
            bindings::greentic::telemetry::logger_api::HostError,
        > {
            Ok(bindings::greentic::telemetry::logger_api::OpAck::Ok)
        }
    }

    impl bindings::greentic::interfaces_types::types::Host for HostState {}

    #[test]
    fn handles_webhook() -> Result<()> {
        let path =
            ensure_component_artifact("messaging-ingress-telegram", "messaging-ingress-telegram")?;
        let engine = new_engine();
        let component = Component::from_file(&engine, &path).context("loading component")?;
        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        bindings::greentic::http::http_client::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link http");
        bindings::greentic::secrets_store::secrets_store::add_to_linker::<
            HostState,
            HasSelf<HostState>,
        >(&mut linker, |state| state)
        .expect("link secrets");
        bindings::greentic::state::state_store::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link state");
        bindings::greentic::telemetry::logger_api::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link logger");
        bindings::greentic::interfaces_types::types::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link interfaces");

        let mut store = Store::new(&engine, HostState::default());
        let instance = linker
            .instantiate(&mut store, &component)
            .context("instantiate")?;
        let ingress_index: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "provider:common/ingress@0.0.2")
            .context("get ingress export index")?;
        let handle_index = instance
            .get_export_index(&mut store, Some(&ingress_index), "handle-webhook")
            .context("get handle-webhook export index")?;
        let handle: TypedFunc<(String, String), (Result<String, String>,)> = instance
            .get_typed_func(&mut store, handle_index)
            .context("get handle-webhook func")?;
        let headers = json!({});
        let body = json!({"update_id": 1});
        let (res,) = handle
            .call(&mut store, (headers.to_string(), body.to_string()))
            .context("call handle_webhook")?;
        assert!(res.is_ok(), "expected ok response");
        Ok(())
    }
}

mod whatsapp {
    use super::*;

    mod bindings {
        wasmtime::component::bindgen!({
            path: "../../components/messaging-ingress-whatsapp/wit/messaging-ingress-whatsapp",
            world: "messaging-ingress-whatsapp",
        });
    }

    #[derive(Default)]
    struct HostState {
        table: ResourceTable,
        wasi_ctx: WasiCtx,
        secrets: HashMap<String, String>,
    }

    impl HostState {
        fn with_secret(key: &str, value: &str) -> Self {
            let mut secrets = HashMap::new();
            secrets.insert(key.to_string(), value.to_string());
            Self {
                table: ResourceTable::new(),
                wasi_ctx: WasiCtxBuilder::new().inherit_stdio().build(),
                secrets,
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
            _req: bindings::greentic::http::http_client::Request,
            _options: Option<bindings::greentic::http::http_client::RequestOptions>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::http::http_client::Response,
            bindings::greentic::http::http_client::HostError,
        > {
            Ok(bindings::greentic::http::http_client::Response {
                status: 200,
                headers: vec![],
                body: None,
            })
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

    impl bindings::greentic::state::state_store::Host for HostState {
        fn read(
            &mut self,
            _key: bindings::greentic::interfaces_types::types::StateKey,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<Vec<u8>, bindings::greentic::state::state_store::HostError> {
            Ok(Vec::new())
        }

        fn write(
            &mut self,
            _key: bindings::greentic::interfaces_types::types::StateKey,
            _bytes: Vec<u8>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::state::state_store::OpAck,
            bindings::greentic::state::state_store::HostError,
        > {
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

    impl bindings::greentic::telemetry::logger_api::Host for HostState {
        fn log(
            &mut self,
            _span: bindings::greentic::interfaces_types::types::SpanContext,
            _fields: Vec<(String, String)>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::telemetry::logger_api::OpAck,
            bindings::greentic::telemetry::logger_api::HostError,
        > {
            Ok(bindings::greentic::telemetry::logger_api::OpAck::Ok)
        }
    }

    impl bindings::greentic::interfaces_types::types::Host for HostState {}

    #[test]
    fn handles_webhook_without_verify_token() -> Result<()> {
        let path =
            ensure_component_artifact("messaging-ingress-whatsapp", "messaging-ingress-whatsapp")?;
        let engine = new_engine();
        let component = Component::from_file(&engine, &path).context("loading component")?;
        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        bindings::greentic::http::http_client::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link http");
        bindings::greentic::secrets_store::secrets_store::add_to_linker::<
            HostState,
            HasSelf<HostState>,
        >(&mut linker, |state| state)
        .expect("link secrets");
        bindings::greentic::state::state_store::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link state");
        bindings::greentic::telemetry::logger_api::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link logger");
        bindings::greentic::interfaces_types::types::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link interfaces");

        let mut store = Store::new(&engine, HostState::default());
        let instance = linker
            .instantiate(&mut store, &component)
            .context("instantiate")?;
        let ingress_index: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "provider:common/ingress@0.0.2")
            .context("get ingress export index")?;
        let handle_index = instance
            .get_export_index(&mut store, Some(&ingress_index), "handle-webhook")
            .context("get handle-webhook export index")?;
        let handle: TypedFunc<(String, String), (Result<String, String>,)> = instance
            .get_typed_func(&mut store, handle_index)
            .context("get handle-webhook func")?;
        let headers = json!({});
        let body = json!({"entry": []});
        let (res,) = handle
            .call(&mut store, (headers.to_string(), body.to_string()))
            .context("call handle_webhook")?;
        assert!(res.is_ok(), "expected ok response");
        Ok(())
    }

    #[test]
    fn rejects_mismatched_verify_token() -> Result<()> {
        let path =
            ensure_component_artifact("messaging-ingress-whatsapp", "messaging-ingress-whatsapp")?;
        let engine = new_engine();
        let component = Component::from_file(&engine, &path).context("loading component")?;
        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        bindings::greentic::http::http_client::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link http");
        bindings::greentic::secrets_store::secrets_store::add_to_linker::<
            HostState,
            HasSelf<HostState>,
        >(&mut linker, |state| state)
        .expect("link secrets");
        bindings::greentic::state::state_store::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link state");
        bindings::greentic::telemetry::logger_api::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link logger");
        bindings::greentic::interfaces_types::types::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link interfaces");

        let mut store = Store::new(
            &engine,
            HostState::with_secret("WHATSAPP_VERIFY_TOKEN", "ok"),
        );
        let instance = linker
            .instantiate(&mut store, &component)
            .context("instantiate")?;
        let ingress_index: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "provider:common/ingress@0.0.2")
            .context("get ingress export index")?;
        let handle_index = instance
            .get_export_index(&mut store, Some(&ingress_index), "handle-webhook")
            .context("get handle-webhook export index")?;
        let handle: TypedFunc<(String, String), (Result<String, String>,)> = instance
            .get_typed_func(&mut store, handle_index)
            .context("get handle-webhook func")?;
        let headers = json!({});
        let body = json!({"hub.verify_token": "bad"});
        let (res,) = handle
            .call(&mut store, (headers.to_string(), body.to_string()))
            .context("call handle_webhook")?;
        assert!(res.is_err(), "expected validation error");
        Ok(())
    }
}

mod teams {
    use super::*;

    mod bindings {
        wasmtime::component::bindgen!({
            path: "../../components/messaging-ingress-teams/wit/messaging-ingress-teams",
            world: "messaging-ingress-teams",
        });
    }

    #[derive(Default)]
    struct HostState {
        table: ResourceTable,
        wasi_ctx: WasiCtx,
        secrets: HashMap<String, String>,
        responses: RefCell<Vec<bindings::greentic::http::http_client::Response>>,
        last_state_write: RefCell<Option<(String, Vec<u8>)>>,
    }

    impl HostState {
        fn with_secret(key: &str, value: &str) -> Self {
            let mut secrets = HashMap::new();
            secrets.insert(key.to_string(), value.to_string());
            Self {
                table: ResourceTable::new(),
                wasi_ctx: WasiCtxBuilder::new().inherit_stdio().build(),
                secrets,
                responses: RefCell::new(Vec::new()),
                last_state_write: RefCell::new(None),
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
            _req: bindings::greentic::http::http_client::Request,
            _options: Option<bindings::greentic::http::http_client::RequestOptions>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::http::http_client::Response,
            bindings::greentic::http::http_client::HostError,
        > {
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

    impl bindings::greentic::state::state_store::Host for HostState {
        fn read(
            &mut self,
            _key: bindings::greentic::interfaces_types::types::StateKey,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<Vec<u8>, bindings::greentic::state::state_store::HostError> {
            Ok(Vec::new())
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
            self.last_state_write.borrow_mut().replace((key, bytes));
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

    impl bindings::greentic::telemetry::logger_api::Host for HostState {
        fn log(
            &mut self,
            _span: bindings::greentic::interfaces_types::types::SpanContext,
            _fields: Vec<(String, String)>,
            _ctx: Option<bindings::greentic::interfaces_types::types::TenantCtx>,
        ) -> Result<
            bindings::greentic::telemetry::logger_api::OpAck,
            bindings::greentic::telemetry::logger_api::HostError,
        > {
            Ok(bindings::greentic::telemetry::logger_api::OpAck::Ok)
        }
    }

    impl bindings::greentic::interfaces_types::types::Host for HostState {}

    #[test]
    fn syncs_subscriptions() -> Result<()> {
        let path = ensure_component_artifact("messaging-ingress-teams", "messaging-ingress-teams")?;
        let engine = new_engine();
        let component = Component::from_file(&engine, &path).context("loading component")?;
        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        bindings::greentic::http::http_client::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link http");
        bindings::greentic::secrets_store::secrets_store::add_to_linker::<
            HostState,
            HasSelf<HostState>,
        >(&mut linker, |state| state)
        .expect("link secrets");
        bindings::greentic::state::state_store::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link state");
        bindings::greentic::telemetry::logger_api::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link logger");
        bindings::greentic::interfaces_types::types::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            |state| state,
        )
        .expect("link interfaces");

        let host = HostState::with_secret("MS_GRAPH_CLIENT_SECRET", "secret");
        host.responses
            .borrow_mut()
            .push(bindings::greentic::http::http_client::Response {
                status: 201,
                headers: vec![],
                body: Some(serde_json::to_vec(&json!({
                    "id": "sub-1",
                    "resource": "/teams/abc/channels/def/messages",
                    "changeType": "created",
                    "expirationDateTime": "2025-01-01T00:00:00Z",
                    "notificationUrl": "https://example.test/webhook"
                }))?),
            });
        host.responses
            .borrow_mut()
            .push(bindings::greentic::http::http_client::Response {
                status: 200,
                headers: vec![],
                body: Some(serde_json::to_vec(&json!({ "value": [] }))?),
            });
        host.responses
            .borrow_mut()
            .push(bindings::greentic::http::http_client::Response {
                status: 200,
                headers: vec![],
                body: Some(serde_json::to_vec(&json!({ "access_token": "tok-123" }))?),
            });

        let config_json = json!({
            "tenant_id": "tenant",
            "client_id": "client",
            "client_secret_key": "MS_GRAPH_CLIENT_SECRET"
        })
        .to_string();
        let state_json = json!({
            "webhook_url": "https://example.test/webhook",
            "desired_subscriptions": [
                {
                    "resource": "/teams/abc/channels/def/messages",
                    "change_type": "created",
                    "expiration_datetime": "2025-01-01T00:00:00Z"
                }
            ]
        })
        .to_string();

        let mut store = Store::new(&engine, host);
        let instance = linker
            .instantiate(&mut store, &component)
            .context("instantiate")?;
        let subs_index: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "provider:common/subscriptions@0.0.2")
            .context("get subscriptions export index")?;
        let sync_index = instance
            .get_export_index(&mut store, Some(&subs_index), "sync-subscriptions")
            .context("get sync-subscriptions export index")?;
        let sync: TypedFunc<(String, String), (Result<String, String>,)> = instance
            .get_typed_func(&mut store, sync_index)
            .context("get sync-subscriptions func")?;
        let (res,) = sync
            .call(&mut store, (config_json, state_json))
            .context("call sync_subscriptions")?;
        assert!(res.is_ok(), "expected ok response");
        let (_, state_bytes) = store
            .data()
            .last_state_write
            .borrow()
            .clone()
            .expect("state write");
        let state_written: Value = serde_json::from_slice(&state_bytes).context("state json")?;
        assert!(state_written.get("subscriptions").is_some());
        Ok(())
    }
}
