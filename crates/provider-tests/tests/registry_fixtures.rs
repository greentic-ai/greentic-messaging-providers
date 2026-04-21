use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use provider_common::component_v0_6::{
    DescribePayload, QaSpec, SchemaIr, canonical_cbor_bytes, decode_cbor, schema_hash, sha256_hex,
};
use provider_common::lifecycle_keys::{
    ProviderProvenance, legacy_messaging_config_keys, legacy_messaging_provenance_keys,
    messaging_config_key, messaging_provenance_key, messaging_state_key,
};
use provider_tests::harness::{
    TestHostState, add_wasi_to_linker, add_wasmtime_hosts, component_path, ensure_components_built,
    new_engine,
};
use serde_json::{Value, json};
use wasmtime::Store;
use wasmtime::component::{Component, ComponentExportIndex, Instance, Linker, TypedFunc};

mod qa_bindings {
    wasmtime::component::bindgen!({
        path: "../../components/messaging-provider-dummy/wit/messaging-provider-dummy",
        world: "component-v0-v6-v0",
    });
}

use qa_bindings::exports::greentic::component::qa::Mode as QaMode;

#[derive(Clone, Copy)]
struct ProviderSpec {
    id: &'static str,
    component: &'static str,
}

const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "dummy",
        component: "messaging-provider-dummy",
    },
    ProviderSpec {
        id: "email",
        component: "messaging-provider-email",
    },
    ProviderSpec {
        id: "slack",
        component: "messaging-provider-slack",
    },
    ProviderSpec {
        id: "teams",
        component: "messaging-provider-teams",
    },
    ProviderSpec {
        id: "telegram",
        component: "messaging-provider-telegram",
    },
    ProviderSpec {
        id: "webchat",
        component: "messaging-provider-webchat",
    },
    ProviderSpec {
        id: "webex",
        component: "messaging-provider-webex",
    },
    ProviderSpec {
        id: "whatsapp",
        component: "messaging-provider-whatsapp",
    },
];

const PROVIDERS_REQUIRING_SECRET_PROMPTS: &[&str] = &[
    "dummy", "email", "slack", "teams", "telegram", "webex", "whatsapp",
];

struct ProviderFixtureBytes {
    describe: Vec<u8>,
    qa_setup: Vec<u8>,
    apply_setup_config: Vec<u8>,
}

struct ComponentHarness {
    _instance: Instance,
    store: Store<TestHostState>,
    describe: TypedFunc<(), (Vec<u8>,)>,
    qa_spec: TypedFunc<(QaMode,), (Vec<u8>,)>,
    apply_answers: TypedFunc<(QaMode, Vec<u8>), (Vec<u8>,)>,
    i18n_keys: TypedFunc<(), (Vec<String>,)>,
    i18n_bundle: TypedFunc<(String,), (Vec<u8>,)>,
}

impl ComponentHarness {
    fn new(component_name: &str) -> Result<Self> {
        let engine = new_engine();
        let component = Component::from_file(&engine, component_path(component_name))
            .map_err(|err| anyhow!("load component: {err}"))?;

        let mut linker = Linker::new(&engine);
        add_wasi_to_linker(&mut linker);
        add_wasmtime_hosts(&mut linker)?;

        let mut store = Store::new(&engine, TestHostState::with_default_secrets());
        let instance = linker
            .instantiate(&mut store, &component)
            .map_err(|err| anyhow!("instantiate component: {err}"))?;

        let descriptor_idx: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "greentic:component/descriptor@0.6.0")
            .context("descriptor export index")?;
        let describe_idx = instance
            .get_export_index(&mut store, Some(&descriptor_idx), "describe")
            .context("describe export index")?;
        let describe = instance
            .get_typed_func(&mut store, describe_idx)
            .map_err(|err| anyhow!("describe typed func: {err}"))?;

        let qa_idx: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "greentic:component/qa@0.6.0")
            .context("qa export index")?;
        let qa_spec_idx = instance
            .get_export_index(&mut store, Some(&qa_idx), "qa-spec")
            .context("qa-spec export index")?;
        let apply_answers_idx = instance
            .get_export_index(&mut store, Some(&qa_idx), "apply-answers")
            .context("apply-answers export index")?;
        let qa_spec = instance
            .get_typed_func(&mut store, qa_spec_idx)
            .map_err(|err| anyhow!("qa-spec typed func: {err}"))?;
        let apply_answers = instance
            .get_typed_func(&mut store, apply_answers_idx)
            .map_err(|err| anyhow!("apply-answers typed func: {err}"))?;

        let i18n_idx: ComponentExportIndex = instance
            .get_export_index(&mut store, None, "greentic:component/component-i18n@0.6.0")
            .context("component-i18n export index")?;
        let i18n_keys_idx = instance
            .get_export_index(&mut store, Some(&i18n_idx), "i18n-keys")
            .context("i18n-keys export index")?;
        let i18n_bundle_idx = instance
            .get_export_index(&mut store, Some(&i18n_idx), "i18n-bundle")
            .context("i18n-bundle export index")?;
        let i18n_keys = instance
            .get_typed_func(&mut store, i18n_keys_idx)
            .map_err(|err| anyhow!("i18n-keys typed func: {err}"))?;
        let i18n_bundle = instance
            .get_typed_func(&mut store, i18n_bundle_idx)
            .map_err(|err| anyhow!("i18n-bundle typed func: {err}"))?;

        Ok(Self {
            _instance: instance,
            store,
            describe,
            qa_spec,
            apply_answers,
            i18n_keys,
            i18n_bundle,
        })
    }

    fn call_describe(&mut self) -> Result<Vec<u8>> {
        let (bytes,) = self.describe.call(&mut self.store, ())?;
        Ok(bytes)
    }

    fn call_qa_setup(&mut self) -> Result<Vec<u8>> {
        self.call_qa(QaMode::Setup)
    }

    fn call_qa(&mut self, mode: QaMode) -> Result<Vec<u8>> {
        let (bytes,) = self.qa_spec.call(&mut self.store, (mode,))?;
        Ok(bytes)
    }

    fn call_apply_setup(&mut self, answers: Value) -> Result<Vec<u8>> {
        self.call_apply(QaMode::Setup, answers)
    }

    fn call_apply(&mut self, mode: QaMode, answers: Value) -> Result<Vec<u8>> {
        let answers_cbor = canonical_cbor_bytes(&answers);
        let (bytes,) = self
            .apply_answers
            .call(&mut self.store, (mode, answers_cbor))
            .map_err(|err| anyhow::anyhow!("call apply-answers: {err}"))?;
        Ok(bytes)
    }

    fn call_i18n_keys(&mut self) -> Result<Vec<String>> {
        let (keys,) = self.i18n_keys.call(&mut self.store, ())?;
        Ok(keys)
    }

    fn call_i18n_bundle(&mut self, locale: &str) -> Result<Value> {
        let (bundle_bytes,) = self
            .i18n_bundle
            .call(&mut self.store, (locale.to_string(),))
            .map_err(|err| anyhow::anyhow!("call i18n-bundle: {err}"))?;
        decode_cbor(&bundle_bytes).map_err(anyhow::Error::msg)
    }
}

fn fixture_root() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    root.join("tests/fixtures/registry")
}

fn provider_fixture_dir(id: &str) -> PathBuf {
    fixture_root().join(id)
}

fn sample_answer_for_key(key: &str) -> Value {
    match key {
        "enabled" => Value::Bool(true),
        "port" => json!(587),
        "tls_mode" => Value::String("starttls".to_string()),
        "token_scope" => Value::String("https://graph.microsoft.com/.default".to_string()),
        "mode" => Value::String("local_queue".to_string()),
        _ if key.ends_with("_url") || key.contains("url") || key.contains("endpoint") => {
            Value::String("https://example.test".to_string())
        }
        _ if key.contains("email") || key.contains("address") => {
            Value::String("user@example.test".to_string())
        }
        _ if key.contains("host") => Value::String("smtp.example.test".to_string()),
        _ if key.contains("username") => Value::String("username".to_string()),
        _ if key.contains("token") || key.contains("secret") || key.contains("password") => {
            Value::String("secret-value".to_string())
        }
        _ if key.contains("phone") => Value::String("15551234567".to_string()),
        _ if key.contains("chat_id") => Value::String("123456".to_string()),
        _ if key.ends_with("_id") || key == "id" => Value::String(format!("{key}-value")),
        _ => Value::String(format!("{key}-value")),
    }
}

fn setup_answers_from_spec(spec: &QaSpec) -> Value {
    let mut map = BTreeMap::new();
    for q in &spec.questions {
        map.insert(q.id.clone(), sample_answer_for_key(&q.id));
    }
    serde_json::to_value(map).unwrap_or_else(|_| json!({}))
}

fn setup_answers_with_invalid_urls(spec: &QaSpec) -> Value {
    let mut map = BTreeMap::new();
    let mut mutated = false;
    for q in &spec.questions {
        let mut value = sample_answer_for_key(&q.id);
        if !mutated
            && (q.id.contains("url") || q.id.contains("endpoint"))
            && q.required
            && value.is_string()
        {
            value = Value::String("not-a-url".to_string());
            mutated = true;
        }
        map.insert(q.id.clone(), value);
    }
    serde_json::to_value(map).unwrap_or_else(|_| json!({}))
}

fn validate_value_against_schema(value: &Value, schema: &SchemaIr, path: &str) -> Result<()> {
    match schema {
        SchemaIr::Bool { .. } => {
            if !value.is_boolean() {
                return Err(anyhow!("{path} must be bool"));
            }
            Ok(())
        }
        SchemaIr::String { .. } => {
            if !(value.is_string() || value.is_number()) {
                return Err(anyhow!("{path} must be string-compatible scalar"));
            }
            Ok(())
        }
        SchemaIr::Object {
            fields,
            additional_properties,
            ..
        } => {
            let obj = value
                .as_object()
                .ok_or_else(|| anyhow!("{path} must be object"))?;
            for (field, field_schema) in fields {
                if field_schema.required && !obj.contains_key(field) {
                    return Err(anyhow!("{path}.{field} is required"));
                }
                if let Some(field_value) = obj.get(field) {
                    validate_value_against_schema(
                        field_value,
                        &field_schema.schema,
                        &format!("{path}.{field}"),
                    )?;
                }
            }
            if !*additional_properties {
                for key in obj.keys() {
                    if !fields.contains_key(key) {
                        return Err(anyhow!("{path}.{key} is not allowed"));
                    }
                }
            }
            Ok(())
        }
    }
}

fn apply_result_config(apply_result: &Value) -> Result<Value> {
    apply_result
        .get("config")
        .cloned()
        .ok_or_else(|| anyhow!("apply result missing config"))
}

#[derive(Default)]
struct UpgradeSeed {
    existing_config: Option<Value>,
    migrated_config_from: Option<String>,
    migrated_provenance_from: Option<String>,
    diagnostics: Vec<String>,
}

fn resolve_upgrade_seed(
    store: &mut BTreeMap<String, Vec<u8>>,
    provider: &str,
    tenant: &str,
    team: Option<&str>,
) -> UpgradeSeed {
    let mut seed = UpgradeSeed::default();
    let config_key = messaging_config_key(provider, tenant, team);
    let provenance_key = messaging_provenance_key(provider, tenant, team);

    if let Some(bytes) = store.get(&config_key) {
        match decode_cbor::<Value>(bytes) {
            Ok(value) if value.is_object() => {
                seed.existing_config = Some(value);
            }
            Ok(_) => seed.diagnostics.push(format!(
                "canonical config key `{config_key}` did not contain an object"
            )),
            Err(err) => seed.diagnostics.push(format!(
                "canonical config key `{config_key}` could not decode: {err}"
            )),
        }
    }

    if seed.existing_config.is_none() {
        for legacy_key in legacy_messaging_config_keys(provider, tenant, team) {
            let Some(bytes) = store.get(&legacy_key).cloned() else {
                continue;
            };
            match decode_cbor::<Value>(&bytes) {
                Ok(value) if value.is_object() => {
                    store.insert(config_key.clone(), canonical_cbor_bytes(&value));
                    store.remove(&legacy_key);
                    seed.existing_config = Some(value);
                    seed.migrated_config_from = Some(legacy_key);
                    break;
                }
                Ok(_) => seed.diagnostics.push(format!(
                    "legacy key `{legacy_key}` had non-object config payload"
                )),
                Err(err) => seed
                    .diagnostics
                    .push(format!("legacy key `{legacy_key}` could not decode: {err}")),
            }
        }
    }

    if !store.contains_key(&provenance_key) {
        for legacy_key in legacy_messaging_provenance_keys(provider, tenant, team) {
            let Some(bytes) = store.get(&legacy_key).cloned() else {
                continue;
            };
            match decode_cbor::<ProviderProvenance>(&bytes) {
                Ok(value) => {
                    store.insert(provenance_key.clone(), canonical_cbor_bytes(&value));
                    store.remove(&legacy_key);
                    seed.migrated_provenance_from = Some(legacy_key);
                    break;
                }
                Err(err) => seed.diagnostics.push(format!(
                    "legacy provenance `{legacy_key}` could not decode: {err}"
                )),
            }
        }
    }

    seed
}

#[derive(Clone, Copy)]
struct CleanupCapabilities {
    state: bool,
    http: bool,
    secrets: bool,
}

#[derive(Default)]
struct CleanupExecution {
    diagnostics: Vec<String>,
    http_actions: Vec<String>,
}

fn provider_owned_secret_key(provider: &str, tenant: &str, team: Option<&str>) -> String {
    let mut key = messaging_state_key(provider, tenant, team, "secrets");
    key.push_str(":token");
    key
}

fn apply_remove_cleanup(
    store: &mut BTreeMap<String, Vec<u8>>,
    secrets: &mut BTreeMap<String, Vec<u8>>,
    provider: &str,
    tenant: &str,
    team: Option<&str>,
    cleanup: &[Value],
    capabilities: CleanupCapabilities,
) -> CleanupExecution {
    let mut execution = CleanupExecution::default();

    for step in cleanup.iter().filter_map(Value::as_str) {
        match step {
            "delete_config_key" => {
                store.remove(&messaging_config_key(provider, tenant, team));
            }
            "delete_provenance_key" => {
                store.remove(&messaging_provenance_key(provider, tenant, team));
            }
            "delete_provider_state_namespace" => {
                if !capabilities.state {
                    execution.diagnostics.push(
                        "skipped delete_provider_state_namespace: missing state capability"
                            .to_string(),
                    );
                    continue;
                }
                let prefix = messaging_state_key(provider, tenant, team, "");
                let keys_to_remove = store
                    .keys()
                    .filter(|key| key.starts_with(&prefix))
                    .cloned()
                    .collect::<Vec<_>>();
                for key in keys_to_remove {
                    store.remove(&key);
                }
            }
            "best_effort_revoke_webhooks" => {
                if !capabilities.http {
                    execution.diagnostics.push(
                        "skipped best_effort_revoke_webhooks: missing http capability".to_string(),
                    );
                    continue;
                }
                execution
                    .http_actions
                    .push("best_effort_revoke_webhooks".to_string());
            }
            "best_effort_revoke_tokens" => {
                if !capabilities.http {
                    execution.diagnostics.push(
                        "skipped best_effort_revoke_tokens: missing http capability".to_string(),
                    );
                    continue;
                }
                execution
                    .http_actions
                    .push("best_effort_revoke_tokens".to_string());
            }
            "best_effort_delete_provider_owned_secrets" => {
                if !capabilities.secrets {
                    execution.diagnostics.push(
                        "skipped best_effort_delete_provider_owned_secrets: missing secrets capability"
                            .to_string(),
                    );
                    continue;
                }
                secrets.remove(&provider_owned_secret_key(provider, tenant, team));
            }
            other => execution
                .diagnostics
                .push(format!("unknown cleanup step: {other}")),
        }
    }

    execution
}

fn generate_provider_fixtures(spec: ProviderSpec) -> Result<ProviderFixtureBytes> {
    let mut harness = ComponentHarness::new(spec.component)?;
    let describe = harness.call_describe()?;
    let qa_setup = harness.call_qa_setup()?;
    let qa_spec: QaSpec = decode_cbor(&qa_setup).map_err(anyhow::Error::msg)?;
    if qa_spec.mode != "setup" {
        return Err(anyhow!(
            "{} setup QA mode mismatch: got {}",
            spec.id,
            qa_spec.mode
        ));
    }
    let apply_setup_config = harness.call_apply_setup(setup_answers_from_spec(&qa_spec))?;
    Ok(ProviderFixtureBytes {
        describe,
        qa_setup,
        apply_setup_config,
    })
}

fn write_provider_fixtures(spec: ProviderSpec) -> Result<()> {
    let fixtures = generate_provider_fixtures(spec)?;
    let dir = provider_fixture_dir(spec.id);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("describe.cbor"), fixtures.describe)?;
    fs::write(dir.join("qa_setup.cbor"), fixtures.qa_setup)?;
    fs::write(
        dir.join("apply_setup_config.cbor"),
        fixtures.apply_setup_config,
    )?;
    Ok(())
}

fn assert_fixture_stable(path: &PathBuf, expected: &[u8], label: &str) -> Result<()> {
    let current = fs::read(path).with_context(|| format!("read fixture {}", path.display()))?;
    if current != expected {
        // On failure, also dump the generated (expected) bytes next to the
        // committed one so CI can upload the diverging blob as an artifact —
        // lets us inspect what CI produces without round-tripping another
        // debug commit.
        let dump_path = path.with_extension("cbor.ci-generated");
        let _ = fs::write(&dump_path, expected);

        let len_cur = current.len();
        let len_exp = expected.len();
        let diff_at = current
            .iter()
            .zip(expected.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(len_cur.min(len_exp));
        let window_start = diff_at.saturating_sub(8);
        let window_end_cur = (diff_at + 16).min(len_cur);
        let window_end_exp = (diff_at + 16).min(len_exp);
        let cur_hex: String = current[window_start..window_end_cur]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let exp_hex: String = expected[window_start..window_end_exp]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");

        return Err(anyhow!(
            "fixture drift for {label} at {path} (run tools/regenerate_registry_fixtures.sh)\n  \
             committed: {len_cur} bytes, generated: {len_exp} bytes, first diff at offset {diff_at}\n  \
             committed[{window_start}..]: {cur_hex}\n  \
             generated[{window_start}..]: {exp_hex}\n  \
             (CI-generated bytes dumped to {dump} for artifact capture)",
            path = path.display(),
            dump = dump_path.display(),
        ));
    }
    Ok(())
}

fn validate_provider_fixtures(spec: ProviderSpec, fixtures: &ProviderFixtureBytes) -> Result<()> {
    let describe: DescribePayload =
        decode_cbor(&fixtures.describe).map_err(|e| anyhow!("decode describe: {e}"))?;
    let qa_setup: QaSpec =
        decode_cbor(&fixtures.qa_setup).map_err(|e| anyhow!("decode qa_setup: {e}"))?;
    let apply_result: Value = decode_cbor(&fixtures.apply_setup_config)
        .map_err(|e| anyhow!("decode apply_setup_config: {e}"))?;

    if describe.operations.is_empty() {
        return Err(anyhow!("{} describe.operations is empty", spec.id));
    }
    let recomputed = schema_hash(
        &describe.input_schema,
        &describe.output_schema,
        &describe.config_schema,
    );
    if describe.schema_hash != recomputed {
        return Err(anyhow!(
            "{} schema_hash mismatch: expected {}, got {}",
            spec.id,
            recomputed,
            describe.schema_hash
        ));
    }
    if qa_setup.mode != "setup" {
        return Err(anyhow!("{} qa_setup.mode must be setup", spec.id));
    }
    let ok = apply_result
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !ok {
        return Err(anyhow!(
            "{} apply_setup_config returned non-ok payload: {}",
            spec.id,
            apply_result
        ));
    }
    let apply_config = apply_result_config(&apply_result)?;
    validate_value_against_schema(&apply_config, &describe.config_schema, "$.config")
        .with_context(|| format!("{} setup config schema validation failed", spec.id))?;

    let mut harness = ComponentHarness::new(spec.component)?;
    let i18n_keys = harness.call_i18n_keys()?;
    let i18n_bundle = harness.call_i18n_bundle("en")?;
    let bundle_messages = i18n_bundle
        .get("messages")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("{} i18n bundle missing messages object", spec.id))?;
    for question in &qa_setup.questions {
        if !i18n_keys.contains(&question.label.key) {
            return Err(anyhow!(
                "{} missing i18n key referenced by QA setup: {}",
                spec.id,
                question.label.key
            ));
        }
    }
    if !i18n_keys.contains(&qa_setup.title.key) {
        return Err(anyhow!(
            "{} missing i18n key referenced by QA setup title: {}",
            spec.id,
            qa_setup.title.key
        ));
    }
    for key in &i18n_keys {
        let value = bundle_messages
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if value.is_empty() {
            return Err(anyhow!("{} i18n value is empty for key {}", spec.id, key));
        }
        if value == key {
            return Err(anyhow!(
                "{} i18n value should be human-readable, got key echo for {}",
                spec.id,
                key
            ));
        }
    }

    Ok(())
}

fn resolver_read(provider_id: &str, file_name: &str) -> Result<Vec<u8>> {
    fs::read(provider_fixture_dir(provider_id).join(file_name))
        .with_context(|| format!("resolve {provider_id}/{file_name}"))
}

#[test]
fn registry_fixtures_are_stable_and_valid() -> Result<()> {
    ensure_components_built();
    for spec in PROVIDERS {
        let generated = generate_provider_fixtures(*spec)?;
        let dir = provider_fixture_dir(spec.id);
        assert_fixture_stable(
            &dir.join("describe.cbor"),
            &generated.describe,
            &format!("{}/describe.cbor", spec.id),
        )?;
        assert_fixture_stable(
            &dir.join("qa_setup.cbor"),
            &generated.qa_setup,
            &format!("{}/qa_setup.cbor", spec.id),
        )?;
        assert_fixture_stable(
            &dir.join("apply_setup_config.cbor"),
            &generated.apply_setup_config,
            &format!("{}/apply_setup_config.cbor", spec.id),
        )?;
        validate_provider_fixtures(*spec, &generated)?;
    }
    Ok(())
}

#[test]
fn fixture_resolver_flow_add_update_remove_smoke() -> Result<()> {
    ensure_components_built();
    const TENANT_ID: &str = "tenant-fixture";

    for spec in PROVIDERS {
        let mut harness = ComponentHarness::new(spec.component)?;
        let describe_bytes = harness.call_describe()?;
        let describe: DescribePayload =
            decode_cbor(&describe_bytes).map_err(|e| anyhow!("decode describe: {e}"))?;
        let qa_setup_bytes = harness.call_qa_setup()?;
        let qa_setup: QaSpec =
            decode_cbor(&qa_setup_bytes).map_err(|e| anyhow!("decode qa setup: {e}"))?;
        let setup_answers = setup_answers_from_spec(&qa_setup);
        let setup_bytes = harness.call_apply(QaMode::Setup, setup_answers)?;
        let setup_value: Value =
            decode_cbor(&setup_bytes).map_err(|e| anyhow!("decode setup apply: {e}"))?;
        let setup_ok = setup_value
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !setup_ok {
            return Err(anyhow!(
                "{} setup apply was not ok: {}",
                spec.id,
                setup_value
            ));
        }
        let setup_config = apply_result_config(&setup_value)?;
        validate_value_against_schema(&setup_config, &describe.config_schema, "$.config")
            .with_context(|| format!("{} setup config schema validation failed", spec.id))?;

        let config_key = messaging_config_key(spec.id, TENANT_ID, None);
        let provenance_key = messaging_provenance_key(spec.id, TENANT_ID, None);
        let state_key = messaging_state_key(spec.id, TENANT_ID, None, "session");
        let describe_hash = sha256_hex(&describe_bytes);
        let artifact_bytes = fs::read(component_path(spec.component))
            .with_context(|| format!("read component artifact for {}", spec.id))?;
        let artifact_digest = sha256_hex(&artifact_bytes);
        let provenance = ProviderProvenance {
            describe_hash,
            artifact_digest,
            schema_hash: describe.schema_hash.clone(),
        };

        let mut store = BTreeMap::new();
        let mut secrets_store = BTreeMap::new();
        store.insert(config_key.clone(), canonical_cbor_bytes(&setup_config));
        store.insert(provenance_key.clone(), canonical_cbor_bytes(&provenance));
        store.insert(
            state_key.clone(),
            canonical_cbor_bytes(&json!({"ok": true})),
        );
        let provider_secret_key = provider_owned_secret_key(spec.id, TENANT_ID, None);
        secrets_store.insert(provider_secret_key.clone(), b"secret".to_vec());

        let upgrade_seed = resolve_upgrade_seed(&mut store, spec.id, TENANT_ID, None);
        let mut update_answers = serde_json::Map::new();
        if let Some(existing_config) = upgrade_seed.existing_config {
            update_answers.insert("existing_config".to_string(), existing_config);
        }
        let update_bytes = harness.call_apply(QaMode::Upgrade, Value::Object(update_answers))?;
        let update_value: Value =
            decode_cbor(&update_bytes).map_err(|e| anyhow!("decode update apply: {e}"))?;
        let update_ok = update_value
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !update_ok {
            return Err(anyhow!(
                "{} update apply was not ok: {}",
                spec.id,
                update_value
            ));
        }
        let update_config = apply_result_config(&update_value)?;
        validate_value_against_schema(&update_config, &describe.config_schema, "$.config")
            .with_context(|| format!("{} update config schema validation failed", spec.id))?;
        store.insert(config_key.clone(), canonical_cbor_bytes(&update_config));

        let remove_bytes = harness.call_apply(QaMode::Remove, json!({}))?;
        let remove_value: Value =
            decode_cbor(&remove_bytes).map_err(|e| anyhow!("decode remove apply: {e}"))?;
        let remove_ok = remove_value
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !remove_ok {
            return Err(anyhow!(
                "{} remove apply was not ok: {}",
                spec.id,
                remove_value
            ));
        }
        let cleanup = remove_value
            .get("remove")
            .and_then(|value| value.get("cleanup"))
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("{} remove result missing cleanup steps", spec.id))?;

        let execution = apply_remove_cleanup(
            &mut store,
            &mut secrets_store,
            spec.id,
            TENANT_ID,
            None,
            cleanup,
            CleanupCapabilities {
                state: false,
                http: false,
                secrets: false,
            },
        );
        if store.contains_key(&config_key) {
            return Err(anyhow!("{} remove did not delete config key", spec.id));
        }
        if store.contains_key(&provenance_key) {
            return Err(anyhow!("{} remove did not delete provenance key", spec.id));
        }
        if !store.contains_key(&state_key) {
            return Err(anyhow!(
                "{} remove should keep state when state capability is missing",
                spec.id
            ));
        }
        if execution.diagnostics.is_empty() {
            return Err(anyhow!(
                "{} remove should emit skipped diagnostics when state capability is missing",
                spec.id
            ));
        }
        if !execution.http_actions.is_empty() {
            return Err(anyhow!(
                "{} should not execute HTTP cleanup actions without http capability",
                spec.id
            ));
        }
        if !secrets_store.contains_key(&provider_secret_key) {
            return Err(anyhow!(
                "{} remove should keep provider secrets when secrets capability is missing",
                spec.id
            ));
        }

        let execution = apply_remove_cleanup(
            &mut store,
            &mut secrets_store,
            spec.id,
            TENANT_ID,
            None,
            cleanup,
            CleanupCapabilities {
                state: true,
                http: true,
                secrets: true,
            },
        );
        if !execution.diagnostics.is_empty() {
            return Err(anyhow!(
                "{} remove emitted diagnostics unexpectedly with state capability",
                spec.id
            ));
        }
        let expects_http_actions = cleanup.iter().filter_map(Value::as_str).any(|step| {
            step == "best_effort_revoke_webhooks" || step == "best_effort_revoke_tokens"
        });
        if expects_http_actions && execution.http_actions.is_empty() {
            return Err(anyhow!(
                "{} remove did not execute expected HTTP best-effort cleanup actions",
                spec.id
            ));
        }
        if store.contains_key(&state_key) {
            return Err(anyhow!(
                "{} remove did not delete provider state keys",
                spec.id
            ));
        }
        if cleanup
            .iter()
            .filter_map(Value::as_str)
            .any(|step| step == "best_effort_delete_provider_owned_secrets")
            && secrets_store.contains_key(&provider_secret_key)
        {
            return Err(anyhow!(
                "{} remove did not delete provider-owned secrets",
                spec.id
            ));
        }
    }
    Ok(())
}

#[test]
fn fixture_resolver_legacy_key_migration_smoke() -> Result<()> {
    ensure_components_built();
    const TENANT_ID: &str = "tenant-legacy";

    for spec in PROVIDERS {
        let mut harness = ComponentHarness::new(spec.component)?;
        let describe_bytes = harness.call_describe()?;
        let describe: DescribePayload =
            decode_cbor(&describe_bytes).map_err(|e| anyhow!("decode describe: {e}"))?;
        let qa_setup_bytes = harness.call_qa_setup()?;
        let qa_setup: QaSpec =
            decode_cbor(&qa_setup_bytes).map_err(|e| anyhow!("decode qa setup: {e}"))?;
        let setup_bytes = harness.call_apply(QaMode::Setup, setup_answers_from_spec(&qa_setup))?;
        let setup_value: Value =
            decode_cbor(&setup_bytes).map_err(|e| anyhow!("decode setup apply: {e}"))?;
        let setup_config = apply_result_config(&setup_value)?;
        let config_key = messaging_config_key(spec.id, TENANT_ID, None);
        let provenance_key = messaging_provenance_key(spec.id, TENANT_ID, None);

        let mut legacy_store = BTreeMap::new();
        let legacy_config_key = legacy_messaging_config_keys(spec.id, TENANT_ID, None)
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("{} no legacy config key candidates", spec.id))?;
        let legacy_provenance_key = legacy_messaging_provenance_keys(spec.id, TENANT_ID, None)
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("{} no legacy provenance key candidates", spec.id))?;
        legacy_store.insert(
            legacy_config_key.clone(),
            canonical_cbor_bytes(&setup_config),
        );
        legacy_store.insert(
            legacy_provenance_key.clone(),
            canonical_cbor_bytes(&ProviderProvenance {
                describe_hash: "describe".to_string(),
                artifact_digest: "artifact".to_string(),
                schema_hash: describe.schema_hash.clone(),
            }),
        );

        let migrated = resolve_upgrade_seed(&mut legacy_store, spec.id, TENANT_ID, None);
        if migrated.existing_config.is_none() {
            return Err(anyhow!(
                "{} legacy config was not migrated to canonical key",
                spec.id
            ));
        }
        if migrated.migrated_config_from.is_none() {
            return Err(anyhow!(
                "{} expected legacy config migration marker",
                spec.id
            ));
        }
        if migrated.migrated_provenance_from.is_none() {
            return Err(anyhow!(
                "{} expected legacy provenance migration marker",
                spec.id
            ));
        }
        if !legacy_store.contains_key(&config_key) || !legacy_store.contains_key(&provenance_key) {
            return Err(anyhow!(
                "{} canonical keys missing after legacy migration",
                spec.id
            ));
        }
        if legacy_store.contains_key(&legacy_config_key)
            || legacy_store.contains_key(&legacy_provenance_key)
        {
            return Err(anyhow!(
                "{} legacy keys should be removed after migration",
                spec.id
            ));
        }

        let mut migrated_answers = serde_json::Map::new();
        migrated_answers.insert(
            "existing_config".to_string(),
            migrated
                .existing_config
                .ok_or_else(|| anyhow!("{} missing migrated existing config", spec.id))?,
        );
        let migrated_update =
            harness.call_apply(QaMode::Upgrade, Value::Object(migrated_answers))?;
        let migrated_update_value: Value =
            decode_cbor(&migrated_update).map_err(|e| anyhow!("decode migrated update: {e}"))?;
        if !migrated_update_value
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(anyhow!(
                "{} upgrade failed with migrated legacy config: {}",
                spec.id,
                migrated_update_value
            ));
        }

        let mut invalid_store = BTreeMap::new();
        invalid_store.insert(legacy_config_key, b"not-cbor".to_vec());
        let fallback_seed = resolve_upgrade_seed(&mut invalid_store, spec.id, TENANT_ID, None);
        if fallback_seed.existing_config.is_some() {
            return Err(anyhow!(
                "{} invalid legacy payload unexpectedly converted",
                spec.id
            ));
        }
        if fallback_seed.diagnostics.is_empty() {
            return Err(anyhow!(
                "{} expected diagnostics for invalid legacy payload",
                spec.id
            ));
        }

        let qa_upgrade_bytes = harness.call_qa(QaMode::Upgrade)?;
        let qa_upgrade: QaSpec =
            decode_cbor(&qa_upgrade_bytes).map_err(|e| anyhow!("decode qa upgrade: {e}"))?;
        let mut fallback_answers = setup_answers_from_spec(&qa_upgrade);
        if let Some(map) = fallback_answers.as_object_mut() {
            map.insert("existing_config".to_string(), json!({}));
        }
        let fallback_update = harness.call_apply(QaMode::Upgrade, fallback_answers)?;
        let fallback_update_value: Value =
            decode_cbor(&fallback_update).map_err(|e| anyhow!("decode fallback update: {e}"))?;
        if !fallback_update_value
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(anyhow!(
                "{} upgrade fallback did not produce valid config: {}",
                spec.id,
                fallback_update_value
            ));
        }
        let fallback_config = apply_result_config(&fallback_update_value)?;
        validate_value_against_schema(&fallback_config, &describe.config_schema, "$.config")
            .with_context(|| {
                format!(
                    "{} fallback update config schema validation failed",
                    spec.id
                )
            })?;
        invalid_store.insert(config_key, canonical_cbor_bytes(&fallback_config));
    }

    Ok(())
}

#[test]
fn fixture_resolver_pack_doctor_strict_smoke() -> Result<()> {
    for spec in PROVIDERS {
        let describe_bytes = resolver_read(spec.id, "describe.cbor")?;
        let describe: DescribePayload = decode_cbor(&describe_bytes).map_err(anyhow::Error::msg)?;
        if describe.operations.is_empty() {
            return Err(anyhow!("{} fixture describe has no operations", spec.id));
        }
        let recomputed = schema_hash(
            &describe.input_schema,
            &describe.output_schema,
            &describe.config_schema,
        );
        if recomputed != describe.schema_hash {
            return Err(anyhow!("{} fixture schema hash mismatch", spec.id));
        }
    }
    Ok(())
}

#[test]
fn fixture_resolver_operator_setup_smoke() -> Result<()> {
    for spec in PROVIDERS {
        let apply_bytes = resolver_read(spec.id, "apply_setup_config.cbor")?;
        let apply_value: Value = decode_cbor(&apply_bytes).map_err(anyhow::Error::msg)?;
        let ok = apply_value
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !ok {
            return Err(anyhow!("{} fixture apply setup is not ok", spec.id));
        }
    }
    Ok(())
}

#[test]
fn fixture_resolver_negative_validation_smoke() -> Result<()> {
    ensure_components_built();
    for spec in PROVIDERS {
        let mut harness = ComponentHarness::new(spec.component)?;
        let qa_setup_bytes = harness.call_qa_setup()?;
        let qa_setup: QaSpec =
            decode_cbor(&qa_setup_bytes).map_err(|e| anyhow!("decode qa setup: {e}"))?;
        let has_url_question = qa_setup
            .questions
            .iter()
            .any(|question| question.required && question.id.contains("url"));
        if !has_url_question {
            continue;
        }
        let invalid_answers = setup_answers_with_invalid_urls(&qa_setup);
        let apply_bytes = harness.call_apply(QaMode::Setup, invalid_answers)?;
        let apply_value: Value = decode_cbor(&apply_bytes).map_err(anyhow::Error::msg)?;
        let ok = apply_value
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if ok {
            return Err(anyhow!(
                "{} setup accepted invalid URL input: {}",
                spec.id,
                apply_value
            ));
        }
    }
    Ok(())
}

#[test]
fn fixture_resolver_missing_secret_prompt_smoke() -> Result<()> {
    ensure_components_built();
    for spec in PROVIDERS {
        if !PROVIDERS_REQUIRING_SECRET_PROMPTS.contains(&spec.id) {
            continue;
        }
        let mut harness = ComponentHarness::new(spec.component)?;
        let qa_setup_bytes = harness.call_qa_setup()?;
        let qa_setup: QaSpec =
            decode_cbor(&qa_setup_bytes).map_err(|e| anyhow!("decode qa setup: {e}"))?;
        let has_secretish_prompt = qa_setup.questions.iter().any(|question| {
            question.id.contains("token")
                || question.id.contains("secret")
                || question.id.contains("password")
        });
        if !has_secretish_prompt {
            return Err(anyhow!(
                "{} setup QA does not include secret prompt",
                spec.id
            ));
        }
    }
    Ok(())
}

#[test]
#[ignore = "manual fixture generation helper"]
fn regenerate_registry_fixtures() -> Result<()> {
    ensure_components_built();
    fs::create_dir_all(fixture_root())?;
    for spec in PROVIDERS {
        write_provider_fixtures(*spec)?;
    }
    Ok(())
}
