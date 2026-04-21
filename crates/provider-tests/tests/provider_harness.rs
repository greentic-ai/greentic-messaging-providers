#![allow(non_snake_case)]

use std::fs;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use greentic_types::{
    Actor, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};
use provider_common::RenderPlan;
use provider_common::component_v0_6::{canonical_cbor_bytes, decode_cbor};
use serde::Deserialize;
use serde_json::{Value, json};
use wasmtime::Store;
use wasmtime::component::{Component, ComponentExportIndex, Instance, Linker, TypedFunc};

use provider_tests::harness::{
    TestHostState, add_wasi_to_linker, add_wasmtime_hosts, component_path, new_engine,
};

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
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    root.join("tests/fixtures")
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

fn test_message_from_plan(plan: &RenderPlan) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("valid env");
    let tenant = TenantId::try_from("default").expect("valid tenant");
    let mut metadata = MessageMetadata::new();
    metadata.insert("provider_harness".to_string(), "true".to_string());
    ChannelMessageEnvelope {
        id: "provider-harness-message".to_string(),
        tenant: TenantCtx::new(env, tenant),
        channel: "provider-harness".to_string(),
        session_id: "provider-harness-session".to_string(),
        reply_scope: None,
        from: Some(Actor {
            id: "provider-harness-user".to_string(),
            kind: Some("user".into()),
        }),
        correlation_id: None,
        to: vec![Destination {
            id: "test-destination-id".to_string(),
            kind: Some("channel".to_string()),
        }],
        text: plan.summary_text.clone(),
        attachments: Vec::new(),
        metadata,
        extensions: Default::default(),
    }
}

struct ProviderHarness {
    _instance: Instance,
    store: Store<TestHostState>,
    invoke: TypedFunc<(String, Vec<u8>), (Vec<u8>,)>,
}

impl ProviderHarness {
    fn new(provider: ProviderId) -> Self {
        let engine = new_engine();
        let component = Component::from_file(&engine, component_path(provider.component_name()))
            .expect("component");

        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        add_wasmtime_hosts(&mut linker).expect("hosts");

        let mut store = Store::new(&engine, TestHostState::with_default_secrets());
        let instance = linker
            .instantiate(&mut store, &component)
            .expect("instance");

        let runtime_idx: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "greentic:component/runtime@0.6.0")
            .expect("runtime export index");
        let invoke_idx = instance
            .get_export_index(&mut store, Some(&runtime_idx), "invoke")
            .expect("invoke export index");
        let invoke = instance
            .get_typed_func(&mut store, invoke_idx)
            .expect("invoke func");

        Self {
            _instance: instance,
            store,
            invoke,
        }
    }

    fn call_json(&mut self, op: &str, input: Value) -> Value {
        let input_cbor = canonical_cbor_bytes(&input);
        let (out_cbor,) = self
            .invoke
            .call(&mut self.store, (op.to_string(), input_cbor))
            .expect("invoke");
        decode_cbor::<Value>(&out_cbor).expect("decode cbor")
    }

    fn encode(&mut self, plan: &RenderPlan) -> Value {
        let message = test_message_from_plan(plan);
        let message = serde_json::to_value(&message).expect("message json");
        let message = json!({
            "message": message.clone(),
            "plan": {
                "message": message,
                "metadata": {}
            }
        });
        let out = self.call_json("encode", message);

        if let Some(payload) = out.get("payload") {
            json!({
                "content_type": payload.get("content_type").cloned().unwrap_or(Value::String(String::new())),
                "body": payload.get("body").cloned().unwrap_or(Value::Null),
                "metadata": payload.get("metadata_json").cloned().unwrap_or(Value::Null),
                "warnings": out.get("warnings").cloned().unwrap_or(Value::Array(vec![])),
            })
        } else {
            out
        }
    }

    fn handle_webhook(&mut self, headers: &Value, body: &Value) -> Value {
        let headers = headers
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(name, value)| {
                        json!({
                            "name": name,
                            "value": value.as_str().unwrap_or_default()
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let body_json = serde_json::to_vec(body).unwrap_or_default();
        let out = self.call_json(
            "ingest_http",
            json!({
                "method": "POST",
                "path": "/webhook",
                "query": null,
                "headers": headers,
                "body_b64": STANDARD.encode(body_json),
                "route_hint": null,
                "binding_id": null,
                "config": null
            }),
        );

        if let Some(event) = out
            .get("events")
            .and_then(Value::as_array)
            .and_then(|events| events.first())
            .cloned()
        {
            json!({
                "ok": out.get("status").and_then(Value::as_u64).unwrap_or(500) < 400,
                "event": event
            })
        } else {
            out
        }
    }
}

impl ProviderId {
    fn component_name(&self) -> &'static str {
        match self {
            ProviderId::Slack => "messaging-provider-slack",
            ProviderId::Teams => "messaging-provider-teams",
            ProviderId::Telegram => "messaging-provider-telegram",
            ProviderId::Webchat => "messaging-provider-webchat",
            ProviderId::Webex => "messaging-provider-webex",
            ProviderId::Whatsapp => "messaging-provider-whatsapp",
        }
    }

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

    fn all() -> [ProviderId; 6] {
        [
            ProviderId::Slack,
            ProviderId::Teams,
            ProviderId::Telegram,
            ProviderId::Webchat,
            ProviderId::Webex,
            ProviderId::Whatsapp,
        ]
    }
}

fn run_adaptive_snapshot(provider: ProviderId, case: &str) {
    let mut harness = ProviderHarness::new(provider);
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
    let mut harness = ProviderHarness::new(provider);
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
    let mut harness = ProviderHarness::new(provider);
    for fixture in inbound_expected_fixture_names(provider) {
        let fixture_data = load_inbound_fixture(provider, &fixture);
        let actual = normalize(harness.handle_webhook(&fixture_data.headers, &fixture_data.body));
        assert_eq!(
            actual.get("ok").and_then(Value::as_bool),
            Some(true),
            "inbound fixture should return ok=true for {} {}",
            provider.as_str(),
            fixture
        );
        assert!(
            actual.get("event").is_some(),
            "inbound fixture should emit event for {} {}",
            provider.as_str(),
            fixture
        );
    }
}

fn run_outbound_fixture_expectations(provider: ProviderId) {
    let mut harness = ProviderHarness::new(provider);
    for fixture in outbound_fixture_names(provider) {
        let plan = load_render_plan_fixture(&fixture);
        let actual = harness.encode(&plan);
        assert!(
            actual.get("error").is_none(),
            "outbound fixture should not return error for {} {}: {}",
            provider.as_str(),
            fixture,
            actual
        );
        assert!(
            actual.get("content_type").is_some() || actual.get("payload").is_some(),
            "outbound fixture should include payload-ish content for {} {}",
            provider.as_str(),
            fixture
        );
    }
}

#[test]
fn adaptivecard_translation_snapshots() {
    for provider in ProviderId::all() {
        for case in [
            "adaptivecard_basic",
            "adaptivecard_inputs",
            "adaptivecard_actions",
            "adaptivecard_columns",
        ] {
            run_adaptive_snapshot(provider, case);
        }
    }
}

#[test]
fn inbound_snapshots() {
    for provider in ProviderId::all() {
        run_inbound_snapshots(provider);
    }
}

#[test]
fn inbound_fixture_expectations() {
    for provider in ProviderId::all() {
        run_inbound_fixture_expectations(provider);
    }
}

#[test]
fn outbound_fixture_expectations() {
    for provider in ProviderId::all() {
        run_outbound_fixture_expectations(provider);
    }
}
