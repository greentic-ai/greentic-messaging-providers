//! Shared infrastructure for universal provider E2E tests.
//!
//! Contains `ProviderId`, `ProviderSpec`, `ProviderHarness`, and common helpers
//! used across the universal_ops_* test files.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use greentic_types::messaging::universal_dto::{
    Header, HttpInV1, HttpOutV1, ProviderPayloadV1, RenderPlanOutV1, SendPayloadInV1,
};
use greentic_types::{
    Actor, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};
use provider_common::component_v0_6::{canonical_cbor_bytes, decode_cbor};
use serde::Deserialize;
use serde_json::{Value, json};
use wasmtime::Store;
use wasmtime::Trap;
use wasmtime::component::{Component, ComponentExportIndex, Instance, Linker, TypedFunc};

use crate::harness::{
    TestHostState, add_wasi_to_linker, add_wasmtime_hosts, component_path, ensure_components_built,
    new_engine,
};

// ---------------------------------------------------------------------------
// Provider identity & specification
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProviderId {
    Slack,
    Telegram,
    Teams,
    Webchat,
    Webex,
    Whatsapp,
    Email,
    Dummy,
}

impl ProviderId {
    pub fn component_name(&self) -> &'static str {
        match self {
            ProviderId::Slack => "messaging-provider-slack",
            ProviderId::Telegram => "messaging-provider-telegram",
            ProviderId::Teams => "messaging-provider-teams",
            ProviderId::Webchat => "messaging-provider-webchat",
            ProviderId::Webex => "messaging-provider-webex",
            ProviderId::Whatsapp => "messaging-provider-whatsapp",
            ProviderId::Email => "messaging-provider-email",
            ProviderId::Dummy => "messaging-provider-dummy",
        }
    }
}

pub struct ProviderSpec {
    pub id: ProviderId,
    pub provider_type: &'static str,
    pub fixture: &'static str,
    pub ingest_supported: bool,
    pub challenge_fixture: Option<&'static str>,
    pub challenge_response: Option<&'static str>,
    pub skip_universal_ops: bool,
}

pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: ProviderId::Slack,
        provider_type: "messaging.slack.api",
        fixture: "slack.json",
        ingest_supported: false,
        challenge_fixture: None,
        challenge_response: None,
        skip_universal_ops: true,
    },
    ProviderSpec {
        id: ProviderId::Telegram,
        provider_type: "messaging.telegram.bot",
        fixture: "telegram.json",
        ingest_supported: true,
        challenge_fixture: None,
        challenge_response: None,
        skip_universal_ops: false,
    },
    ProviderSpec {
        id: ProviderId::Teams,
        provider_type: "messaging.teams.bot",
        fixture: "teams.json",
        ingest_supported: true,
        challenge_fixture: None,
        challenge_response: None,
        skip_universal_ops: false,
    },
    ProviderSpec {
        id: ProviderId::Webchat,
        provider_type: "messaging.webchat",
        fixture: "webchat.json",
        ingest_supported: true,
        challenge_fixture: None,
        challenge_response: None,
        skip_universal_ops: false,
    },
    ProviderSpec {
        id: ProviderId::Whatsapp,
        provider_type: "messaging.whatsapp.cloud",
        fixture: "whatsapp.json",
        ingest_supported: true,
        challenge_fixture: Some("whatsapp_challenge.json"),
        challenge_response: Some("verify123"),
        skip_universal_ops: false,
    },
    ProviderSpec {
        id: ProviderId::Webex,
        provider_type: "messaging.webex.bot",
        fixture: "webex.json",
        ingest_supported: false,
        challenge_fixture: None,
        challenge_response: None,
        skip_universal_ops: false,
    },
    ProviderSpec {
        id: ProviderId::Email,
        provider_type: "messaging.email.smtp",
        fixture: "email.json",
        ingest_supported: false,
        challenge_fixture: None,
        challenge_response: None,
        skip_universal_ops: false,
    },
    ProviderSpec {
        id: ProviderId::Dummy,
        provider_type: "messaging.dummy",
        fixture: "dummy.json",
        ingest_supported: false,
        challenge_fixture: None,
        challenge_response: None,
        skip_universal_ops: true,
    },
];

pub fn provider_spec(id: ProviderId) -> &'static ProviderSpec {
    PROVIDERS
        .iter()
        .find(|spec| spec.id == id)
        .expect("provider spec exists")
}

// ---------------------------------------------------------------------------
// Response DTOs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct HttpFixture {
    pub method: String,
    pub path: String,
    pub headers: Option<HashMap<String, String>>,
    pub query: Option<String>,
    pub route_hint: Option<String>,
    pub body: Option<Value>,
}

#[derive(Deserialize)]
pub struct RenderPlanResponse {
    pub ok: bool,
    pub plan: Option<RenderPlanOutV1>,
}

#[derive(Deserialize)]
pub struct EncodeResponse {
    pub ok: bool,
    pub payload: Option<ProviderPayloadV1>,
}

// ---------------------------------------------------------------------------
// WASM harness
// ---------------------------------------------------------------------------

pub struct ProviderHarness {
    _instance: Instance,
    store: Store<TestHostState>,
    invoke: TypedFunc<(String, Vec<u8>), (Vec<u8>,)>,
}

impl ProviderHarness {
    pub fn new(spec: &ProviderSpec) -> Result<Self> {
        let engine = new_engine();
        let component_file = component_path(spec.id.component_name());
        log_component_artifact(&component_file);
        let component = Component::from_file(&engine, &component_file)
            .map_err(|err| Error::msg(format!("load component: {err}")))?;
        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        add_wasmtime_hosts(&mut linker)?;
        let mut store = Store::new(&engine, TestHostState::with_default_secrets());
        let instance = linker
            .instantiate(&mut store, &component)
            .map_err(|e| instantiate_error(spec, e))?;
        let api_index: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "greentic:component/runtime@0.6.0")
            .context("runtime export index")?;
        let invoke_index = instance
            .get_export_index(&mut store, Some(&api_index), "invoke")
            .context("invoke export index")?;
        let invoke = instance
            .get_typed_func(&mut store, invoke_index)
            .map_err(|err| Error::msg(format!("typed invoke func: {err}")))?;
        Ok(Self {
            _instance: instance,
            store,
            invoke,
        })
    }

    pub fn new_with_state(spec: &ProviderSpec, state: TestHostState) -> Result<Self> {
        let engine = new_engine();
        let component_file = component_path(spec.id.component_name());
        log_component_artifact(&component_file);
        let component = Component::from_file(&engine, &component_file)
            .map_err(|err| Error::msg(format!("load component: {err}")))?;
        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        add_wasmtime_hosts(&mut linker)?;
        let mut store = Store::new(&engine, state);
        let instance = linker
            .instantiate(&mut store, &component)
            .map_err(|e| instantiate_error(spec, e))?;
        let api_index: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "greentic:component/runtime@0.6.0")
            .context("runtime export index")?;
        let invoke_index = instance
            .get_export_index(&mut store, Some(&api_index), "invoke")
            .context("invoke export index")?;
        let invoke = instance
            .get_typed_func(&mut store, invoke_index)
            .map_err(|err| Error::msg(format!("typed invoke func: {err}")))?;
        Ok(Self {
            _instance: instance,
            store,
            invoke,
        })
    }

    pub fn call(&mut self, op: &str, payload: Vec<u8>) -> Result<Vec<u8>> {
        let payload_cbor = match serde_json::from_slice::<Value>(&payload) {
            Ok(value) => canonical_cbor_bytes(&value),
            Err(_) => payload,
        };
        let (result,) = self
            .invoke
            .call(&mut self.store, (op.to_string(), payload_cbor))
            .inspect_err(|err| {
                if let Some(trap) = err.downcast_ref::<Trap>() {
                    eprintln!("trap trace: {:?}", trap);
                }
            })
            .map_err(|err| Error::msg(format!("invoke {op}: {err}")))?;
        let value: Value = decode_cbor(&result).map_err(|err| Error::msg(err.to_string()))?;
        serde_json::to_vec(&value).context("serialize runtime output")
    }
}

fn log_component_artifact(path: &Path) {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let metadata = fs::metadata(&canonical).ok();
    let (len, mtime) = if let Some(meta) = metadata {
        let len = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        (len, mtime)
    } else {
        (0, 0)
    };
    eprintln!(
        "[provider-tests] Instantiating component: {} (bytes={}, mtime={})",
        canonical.display(),
        len,
        mtime
    );
}

fn instantiate_error<E: std::fmt::Display>(spec: &ProviderSpec, source: E) -> Error {
    let text = source.to_string();
    let hint = if text.contains("expected 2-tuple") {
        Some(
            "component still imports legacy http-client signature (request, ctx); rebuild against http-client with RequestOptions (3-tuple)",
        )
    } else if text.contains("expected 3-tuple") {
        Some(
            "harness exposes 3-tuple signature (request, requestOptions, ctx); rebuild provider so it targets the new contract",
        )
    } else if text.contains("matching implementation was not found") {
        Some(
            "legacy http-client import is unsupported; rebuild the provider so it imports the 3-tuple signature (request, requestOptions, ctx)",
        )
    } else {
        None
    };
    let detail = if let Some(h) = hint {
        format!("{} ({})", text, h)
    } else {
        text
    };
    Error::msg(format!(
        "failed to instantiate {} ({}): {}",
        spec.id.component_name(),
        spec.provider_type,
        detail
    ))
}

// ---------------------------------------------------------------------------
// Fixture & envelope helpers
// ---------------------------------------------------------------------------

pub fn fixtures_root() -> PathBuf {
    ensure_components_built();
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    root.push("tests/fixtures/universal");
    root
}

pub fn load_http_fixture(name: &str) -> Result<HttpFixture> {
    let path = fixtures_root().join(name);
    let raw = fs::read_to_string(&path).context("load fixture")?;
    serde_json::from_str(&raw).context("parse fixture")
}

pub fn http_input_from_fixture(fixture: HttpFixture) -> HttpInV1 {
    let headers = fixture
        .headers
        .unwrap_or_default()
        .into_iter()
        .map(|(name, value)| Header { name, value })
        .collect();
    let body_bytes = fixture
        .body
        .map(|body| serde_json::to_vec(&body).unwrap_or_default())
        .unwrap_or_default();
    HttpInV1 {
        method: fixture.method,
        path: fixture.path,
        query: fixture.query,
        headers,
        body_b64: STANDARD.encode(&body_bytes),
        route_hint: fixture.route_hint,
        binding_id: None,
        config: None,
    }
}

pub fn build_envelope(id: ProviderId) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("default env");
    let tenant = TenantId::try_from("default").expect("default tenant");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    let channel = match id {
        ProviderId::Slack => "slack",
        ProviderId::Telegram => "telegram",
        ProviderId::Teams => "teams",
        ProviderId::Webchat => "webchat",
        ProviderId::Webex => "webex",
        ProviderId::Whatsapp => "whatsapp",
        ProviderId::Email => "email",
        ProviderId::Dummy => "dummy",
    };
    ChannelMessageEnvelope {
        id: format!("{channel}-envelope"),
        tenant: TenantCtx::new(env, tenant),
        channel: channel.to_string(),
        session_id: channel.to_string(),
        reply_scope: None,
        from: Some(Actor {
            id: "universal-user".to_string(),
            kind: Some("user".into()),
        }),
        correlation_id: None,
        to: match id {
            ProviderId::Email => vec![Destination {
                id: "test@example.com".to_string(),
                kind: Some("email".into()),
            }],
            _ => Vec::new(),
        },
        text: Some(format!("universal {} message", channel)),
        attachments: Vec::new(),
        metadata,
        extensions: Default::default(),
    }
}

pub fn send_payload_body(id: ProviderId) -> Value {
    match id {
        ProviderId::Slack => json!({
            "to": {"kind": "channel", "id": "C_UNIVERSAL"},
            "text": "universal slack",
            "config": {"default_channel": "C_UNIVERSAL"}
        }),
        ProviderId::Telegram => json!({
            "chat_id": "922337",
            "text": "universal telegram",
            "config": {"default_chat_id": "922337"}
        }),
        ProviderId::Teams => json!({
            "text": "universal teams",
            "team_id": "team-universal",
            "channel_id": "channel-universal",
            "config": {
                "tenant_id": "tenant-universal",
                "client_id": "client-universal",
                "team_id": "team-universal",
                "channel_id": "channel-universal"
            }
        }),
        ProviderId::Webchat => json!({
            "text": "universal webchat",
            "route": "universal-route",
            "tenant_channel_id": "tenant-channel",
            "config": {
                "route": "universal-route",
                "tenant_channel_id": "tenant-channel",
                "mode": "universal-mode",
                "base_url": "https://webchat.example"
            }
        }),
        ProviderId::Webex => json!({
            "text": "universal webex",
            "to": {"kind": "room", "id": "room-universal"},
            "config": {"default_room_id": "room-universal"}
        }),
        ProviderId::Whatsapp => json!({
            "text": "universal whatsapp",
            "to": {"kind": "user", "id": "whatsapp-user"},
            "config": {"phone_number_id": "phone-universal"}
        }),
        ProviderId::Email => json!({
            "to": "recipient@example.com",
            "subject": "universal email",
            "body": "hello universal",
            "config": {
                "host": "smtp.example",
                "username": "user",
                "from_address": "sender@example.com"
            }
        }),
        ProviderId::Dummy => json!({
            "text": "universal dummy"
        }),
    }
}

pub fn send_payload_in(spec: &ProviderSpec) -> Result<Vec<u8>> {
    let body = send_payload_body(spec.id);
    let body_bytes = serde_json::to_vec(&body)?;
    let payload = ProviderPayloadV1 {
        content_type: "application/json".to_string(),
        body_b64: STANDARD.encode(&body_bytes),
        metadata: BTreeMap::new(),
    };
    let payload_in = SendPayloadInV1 {
        provider_type: spec.provider_type.to_string(),
        tenant_id: None,
        auth_user: None,
        payload,
    };
    Ok(serde_json::to_vec(&payload_in)?)
}

pub fn decode_challenge(out: &HttpOutV1) -> Option<String> {
    if out.body_b64.is_empty() {
        return None;
    }
    STANDARD
        .decode(&out.body_b64)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

// ---------------------------------------------------------------------------
// Schema-core-api harness (JSON invoke path)
// ---------------------------------------------------------------------------

/// Harness for calling provider ops through the `greentic:provider-schema-core/schema-core-api@1.0.0`
/// JSON invoke path, as the operator does at runtime.
///
/// Unlike [`ProviderHarness`] which uses the v0.6 CBOR runtime interface, this
/// harness exercises the JSON-based schema-core-api export that bridges QA ops
/// (`qa-spec`, `apply-answers`, `i18n-keys`).
pub struct SchemaCoreHarness {
    _instance: Instance,
    store: Store<TestHostState>,
    invoke: TypedFunc<(String, Vec<u8>), (Vec<u8>,)>,
}

impl SchemaCoreHarness {
    pub fn new(spec: &ProviderSpec) -> Result<Self> {
        let engine = new_engine();
        let component_file = component_path(spec.id.component_name());
        log_component_artifact(&component_file);
        let component = Component::from_file(&engine, &component_file)
            .map_err(|err| Error::msg(format!("load component: {err}")))?;
        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        add_wasmtime_hosts(&mut linker)?;
        let mut store = Store::new(&engine, TestHostState::with_default_secrets());
        let instance = linker
            .instantiate(&mut store, &component)
            .map_err(|e| instantiate_error(spec, e))?;
        let api_index: ComponentExportIndex = instance
            .get_export_index(
                &mut store,
                None,
                "greentic:provider-schema-core/schema-core-api@1.0.0",
            )
            .context("schema-core-api export index")?;
        let invoke_index = instance
            .get_export_index(&mut store, Some(&api_index), "invoke")
            .context("schema-core-api invoke export index")?;
        let invoke = instance
            .get_typed_func(&mut store, invoke_index)
            .map_err(|err| Error::msg(format!("typed schema-core-api invoke func: {err}")))?;
        Ok(Self {
            _instance: instance,
            store,
            invoke,
        })
    }

    /// Call the schema-core-api invoke with raw JSON bytes in / JSON bytes out.
    pub fn call(&mut self, op: &str, input_json: Vec<u8>) -> Result<Vec<u8>> {
        let (result,) = self
            .invoke
            .call(&mut self.store, (op.to_string(), input_json))
            .inspect_err(|err| {
                if let Some(trap) = err.downcast_ref::<Trap>() {
                    eprintln!("trap trace: {:?}", trap);
                }
            })
            .map_err(|err| Error::msg(format!("schema-core-api invoke {op}: {err}")))?;
        Ok(result)
    }
}
