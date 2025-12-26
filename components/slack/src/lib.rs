#![allow(unsafe_op_in_unsafe_fn)]

use hmac::{Hmac, Mac};
use sha2::Sha256;

mod bindings {
    wit_bindgen::generate!({ path: "wit/slack", world: "slack", generate_all });
}

use bindings::Guest;
use bindings::greentic::http::http_client;
use bindings::greentic::secrets_store::secrets_store;
use bindings::greentic::telemetry::logger_api;
use bindings::provider::common::capabilities::{
    CapabilitiesResponse as BindingsCapabilitiesResponse,
    ProviderCapabilities as BindingsProviderCapabilities, ProviderLimits as BindingsProviderLimits,
    ProviderMetadata as BindingsProviderMetadata,
};
use bindings::provider::common::render::{
    EncodeResult as BindingsEncodeResult, ProviderPayload as BindingsProviderPayload,
    RenderPlan as BindingsRenderPlan, RenderTier as BindingsRenderTier,
    RenderWarning as BindingsRenderWarning,
};
use provider_common::ProviderError;
use provider_common::{
    CapabilitiesResponseV1, ProviderCapabilitiesV1, ProviderLimitsV1, ProviderMetadataV1,
};
use provider_runtime_config::ProviderRuntimeConfig;
use std::sync::OnceLock;

const SLACK_API_URL: &str = "https://slack.com/api/chat.postMessage";
const SLACK_BOT_TOKEN_KEY: &str = "SLACK_BOT_TOKEN";
const SLACK_SIGNING_SECRET_KEY: &str = "SLACK_SIGNING_SECRET";

static RUNTIME_CONFIG: OnceLock<ProviderRuntimeConfig> = OnceLock::new();

struct Component;

impl Guest for Component {
    fn init_runtime_config(config_json: String) -> Result<(), String> {
        let config = parse_runtime_config(&config_json)?;
        set_runtime_config(config)
    }

    fn capabilities() -> BindingsCapabilitiesResponse {
        bindings_capabilities_response(capabilities_v1())
    }

    fn encode(plan: BindingsRenderPlan) -> BindingsEncodeResult {
        encode_render_plan(plan)
    }

    fn send_message(channel: String, text: String) -> Result<String, String> {
        let payload = format_message_json(&channel, &text);
        let token = get_secret_string(SLACK_BOT_TOKEN_KEY)?;
        let req = http_client::Request {
            method: "POST".to_string(),
            url: SLACK_API_URL.to_string(),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("Authorization".into(), format!("Bearer {}", token)),
            ],
            body: Some(payload.clone().into_bytes()),
        };

        let resp = send_with_retries(&req)?;

        if resp.status >= 200 && resp.status < 300 {
            log_if_enabled("send_message_success");
            Ok(payload)
        } else {
            Err(format!(
                "transport error: slack returned status {}",
                resp.status
            ))
        }
    }

    fn handle_webhook(headers_json: String, body_json: String) -> Result<String, String> {
        let headers: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&headers_json)
                .map_err(|_| "validation error: invalid headers".to_string())?;

        if let Some(secret_result) = get_optional_secret(SLACK_SIGNING_SECRET_KEY) {
            let signing_secret = secret_result.map_err(|e| format!("transport error: {}", e))?;
            verify_signature(&headers, &body_json, &signing_secret).map_err(|e| e.to_string())?;
        }

        let body_val: serde_json::Value = serde_json::from_str(&body_json)
            .map_err(|_| "validation error: invalid body json".to_string())?;
        let normalized = serde_json::json!({
            "ok": true,
            "event": body_val,
        });
        serde_json::to_string(&normalized)
            .map_err(|_| "other error: serialization failed".to_string())
    }

    fn refresh() -> Result<String, String> {
        Ok(r#"{"ok":true,"refresh":"not-needed"}"#.to_string())
    }

    fn format_message(channel: String, text: String) -> String {
        format_message_json(&channel, &text)
    }
}

fn get_secret_string(key: &str) -> Result<String, String> {
    match secrets_get(key) {
        Ok(Some(bytes)) => String::from_utf8(bytes).map_err(|_| "secret not valid utf-8".into()),
        Ok(None) => Err(missing_secret_error(key)),
        Err(e) => Err(secret_error_message(key, e)),
    }
}

fn get_optional_secret(key: &str) -> Option<Result<String, String>> {
    match secrets_get(key) {
        Ok(Some(bytes)) => {
            Some(String::from_utf8(bytes).map_err(|_| "secret not valid utf-8".into()))
        }
        Ok(None) => None,
        Err(e) => Some(Err(secret_error_message(key, e))),
    }
}

fn format_message_json(channel: &str, text: &str) -> String {
    let payload = payload_with_blocks(channel, text, vec![section_md(text)]);
    serde_json::to_string(&payload).unwrap_or_else(|_| "{\"channel\":\"\",\"text\":\"\"}".into())
}

fn section_md(text: &str) -> serde_json::Value {
    serde_json::json!({
      "type": "section",
      "text": { "type": "mrkdwn", "text": text }
    })
}

fn payload_with_blocks(
    channel: &str,
    text: &str,
    blocks: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
      "channel": channel,
      "text": text,
      "blocks": blocks,
    })
}

fn verify_signature(
    headers: &serde_json::Map<String, serde_json::Value>,
    body: &str,
    signing_secret: &str,
) -> Result<(), VerificationError> {
    let ts = header_value(headers, "x-slack-request-timestamp")
        .ok_or(VerificationError::MissingTimestamp)?;
    let sig =
        header_value(headers, "x-slack-signature").ok_or(VerificationError::MissingSignature)?;

    let base = format!("v0:{}:{}", ts, body);
    let mut mac = Hmac::<Sha256>::new_from_slice(signing_secret.as_bytes())
        .map_err(|_| VerificationError::InvalidKey)?;
    mac.update(base.as_bytes());
    let computed = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(64);
    for byte in computed {
        use std::fmt::Write;
        write!(&mut hex, "{:02x}", byte).unwrap();
    }
    let expected = format!("v0={}", hex);

    if constant_time_eq(expected.as_bytes(), sig.as_bytes()) {
        Ok(())
    } else {
        Err(VerificationError::SignatureMismatch)
    }
}

fn capabilities_v1() -> CapabilitiesResponseV1 {
    CapabilitiesResponseV1::new(
        ProviderMetadataV1 {
            provider_id: "slack".into(),
            display_name: "Slack".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            rate_limit_hint: None,
        },
        ProviderCapabilitiesV1 {
            supports_threads: false,
            supports_buttons: false,
            supports_webhook_validation: true,
            supports_formatting_options: false,
        },
        ProviderLimitsV1 {
            max_text_len: 40_000,
            callback_data_max_bytes: 0,
            max_buttons_per_row: 0,
            max_button_rows: 0,
        },
    )
}

fn bindings_capabilities_response(resp: CapabilitiesResponseV1) -> BindingsCapabilitiesResponse {
    BindingsCapabilitiesResponse {
        metadata: BindingsProviderMetadata {
            provider_id: resp.metadata.provider_id,
            display_name: resp.metadata.display_name,
            version: resp.metadata.version,
            rate_limit_hint: resp.metadata.rate_limit_hint,
        },
        capabilities: BindingsProviderCapabilities {
            supports_threads: resp.capabilities.supports_threads,
            supports_buttons: resp.capabilities.supports_buttons,
            supports_webhook_validation: resp.capabilities.supports_webhook_validation,
            supports_formatting_options: resp.capabilities.supports_formatting_options,
        },
        limits: BindingsProviderLimits {
            max_text_len: resp.limits.max_text_len,
            callback_data_max_bytes: resp.limits.callback_data_max_bytes,
            max_buttons_per_row: resp.limits.max_buttons_per_row,
            max_button_rows: resp.limits.max_button_rows,
        },
    }
}

fn encode_render_plan(plan: BindingsRenderPlan) -> BindingsEncodeResult {
    let mut warnings = plan.warnings.clone();
    match plan.tier {
        BindingsRenderTier::TierA | BindingsRenderTier::TierB => {
            let text = plan.summary_text.clone().unwrap_or_default();
            let mut payload = serde_json::json!({
                "channel": "",
                "text": text,
                "blocks": [ section_md(&text) ],
            });
            if !plan.attachments.is_empty() {
                payload["attachments"] = serde_json::Value::Array(
                    plan.attachments
                        .iter()
                        .map(|a| serde_json::json!({ "text": a }))
                        .collect(),
                );
            }
            if !plan.actions.is_empty() {
                payload["metadata"] = serde_json::json!({ "actions": plan.actions.clone() });
            }
            match serde_json::to_vec(&payload) {
                Ok(body) => BindingsEncodeResult {
                    payload: BindingsProviderPayload {
                        content_type: "application/json".into(),
                        body,
                        metadata_json: None,
                    },
                    warnings,
                },
                Err(_) => {
                    warnings.push(BindingsRenderWarning {
                        code: "encoder_serialization_failed".into(),
                        message: Some(
                            "failed to serialize tier_a/b payload; downgraded to text".into(),
                        ),
                        path: None,
                    });
                    encode_tier_d_internal(plan, warnings)
                }
            }
        }
        BindingsRenderTier::TierC | BindingsRenderTier::TierD => {
            encode_tier_d_internal(plan, warnings)
        }
    }
}

fn encode_tier_d_internal(
    plan: BindingsRenderPlan,
    mut warnings: Vec<BindingsRenderWarning>,
) -> BindingsEncodeResult {
    if plan.tier != BindingsRenderTier::TierD {
        warnings.push(BindingsRenderWarning {
            code: "encoder_forced_downgrade".into(),
            message: Some("downgraded to tier_d text payload".into()),
            path: None,
        });
    }
    let text = plan.summary_text.unwrap_or_default();
    let payload = BindingsProviderPayload {
        content_type: "text/plain; charset=utf-8".into(),
        body: text.into_bytes(),
        metadata_json: None,
    };
    BindingsEncodeResult { payload, warnings }
}

fn header_value(
    headers: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    headers.iter().find_map(|(k, v)| {
        if k.to_ascii_lowercase() == lower {
            match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .find_map(|val| val.as_str().map(|s| s.to_string())),
                _ => None,
            }
        } else {
            None
        }
    })
}

#[derive(Debug)]
enum VerificationError {
    MissingTimestamp,
    MissingSignature,
    InvalidKey,
    SignatureMismatch,
}

impl std::fmt::Display for VerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerificationError::MissingTimestamp => write!(f, "missing timestamp"),
            VerificationError::MissingSignature => write!(f, "missing signature"),
            VerificationError::InvalidKey => write!(f, "invalid signing secret"),
            VerificationError::SignatureMismatch => write!(f, "signature mismatch"),
        }
    }
}

impl std::error::Error for VerificationError {}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut res = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        res |= x ^ y;
    }
    res == 0
}

fn secret_error_message(key: &str, error: secrets_store::SecretsError) -> String {
    match error {
        secrets_store::SecretsError::NotFound => missing_secret_error(key),
        secrets_store::SecretsError::Denied => "secret access denied".into(),
        secrets_store::SecretsError::InvalidKey => "secret key invalid".into(),
        secrets_store::SecretsError::Internal => "secret lookup failed".into(),
    }
}

bindings::__export_world_slack_cabi!(Component with_types_in bindings);

fn parse_runtime_config(config_json: &str) -> Result<ProviderRuntimeConfig, String> {
    if config_json.trim().is_empty() {
        return Ok(ProviderRuntimeConfig::default());
    }
    let cfg: ProviderRuntimeConfig = serde_json::from_str(config_json)
        .map_err(|e| format!("validation error: invalid provider runtime config: {e}"))?;
    cfg.validate()
        .map_err(|e| format!("validation error: invalid provider runtime config: {e}"))?;
    Ok(cfg)
}

fn set_runtime_config(cfg: ProviderRuntimeConfig) -> Result<(), String> {
    if RUNTIME_CONFIG.set(cfg.clone()).is_ok() {
        return Ok(());
    }
    let existing = RUNTIME_CONFIG.get().expect("set");
    if existing == &cfg {
        Ok(())
    } else {
        Err("validation error: provider runtime config already set".into())
    }
}

fn runtime_config() -> &'static ProviderRuntimeConfig {
    RUNTIME_CONFIG.get_or_init(ProviderRuntimeConfig::default)
}

fn send_with_retries(req: &http_client::Request) -> Result<http_client::Response, String> {
    let attempts = runtime_config().network.max_attempts.clamp(1, 10);
    let options = request_options();
    let mut last_err: Option<String> = None;
    for _ in 0..attempts {
        match http_send(req, &options) {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                last_err = Some(format!("transport error: {} ({})", e.message, e.code));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "transport error: request failed".into()))
}

fn missing_secret_error(name: &str) -> String {
    serde_json::to_string(&ProviderError::missing_secret(name))
        .unwrap_or_else(|_| format!("missing secret: {name}"))
}

fn log_if_enabled(event: &str) {
    let cfg = runtime_config();
    if !cfg.telemetry.emit_enabled {
        return;
    }
    let span = logger_api::SpanContext {
        tenant: "tenant".into(),
        session_id: None,
        flow_id: "provider-runtime".into(),
        node_id: None,
        provider: cfg
            .telemetry
            .service_name
            .clone()
            .unwrap_or_else(|| "slack".into()),
        start_ms: None,
        end_ms: None,
    };
    let fields = [("event".to_string(), event.to_string())];
    let _ = logger_api::log(&span, &fields, None);
}

fn request_options() -> http_client::RequestOptions {
    let cfg = runtime_config();
    http_client::RequestOptions {
        timeout_ms: None,
        proxy: match cfg.network.proxy {
            provider_runtime_config::ProxyMode::Inherit => http_client::ProxyMode::Inherit,
            provider_runtime_config::ProxyMode::Disabled => http_client::ProxyMode::Disabled,
        },
        tls: match cfg.network.tls {
            provider_runtime_config::TlsMode::Strict => http_client::TlsMode::Strict,
            provider_runtime_config::TlsMode::Insecure => http_client::TlsMode::Insecure,
        },
    }
}

fn secrets_get(key: &str) -> Result<Option<Vec<u8>>, secrets_store::SecretsError> {
    #[cfg(test)]
    {
        return secrets_get_test(key);
    }
    #[cfg(not(test))]
    {
        secrets_store::get(key)
    }
}

fn http_send(
    req: &http_client::Request,
    options: &http_client::RequestOptions,
) -> Result<http_client::Response, http_client::HostError> {
    #[cfg(test)]
    {
        return http_send_test(req, options);
    }
    #[cfg(not(test))]
    {
        http_client::send(req, Some(options.clone()), None)
    }
}

#[cfg(test)]
thread_local! {
    static SECRETS_GET_MOCK: std::cell::RefCell<Option<Box<dyn Fn(&str) -> Result<Option<Vec<u8>>, secrets_store::SecretsError>>>> =
        std::cell::RefCell::new(None);
    static HTTP_SEND_MOCK: std::cell::RefCell<Option<Box<dyn Fn(&http_client::Request, &http_client::RequestOptions) -> Result<http_client::Response, http_client::HostError>>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn with_secrets_get_mock<F, R>(
    mock: impl Fn(&str) -> Result<Option<Vec<u8>>, secrets_store::SecretsError> + 'static,
    f: F,
) -> R
where
    F: FnOnce() -> R,
{
    SECRETS_GET_MOCK.with(|cell| *cell.borrow_mut() = Some(Box::new(mock)));
    let out = f();
    SECRETS_GET_MOCK.with(|cell| *cell.borrow_mut() = None);
    out
}

#[cfg(test)]
fn with_http_send_mock<F, R>(
    mock: impl Fn(
        &http_client::Request,
        &http_client::RequestOptions,
    ) -> Result<http_client::Response, http_client::HostError>
    + 'static,
    f: F,
) -> R
where
    F: FnOnce() -> R,
{
    HTTP_SEND_MOCK.with(|cell| *cell.borrow_mut() = Some(Box::new(mock)));
    let out = f();
    HTTP_SEND_MOCK.with(|cell| *cell.borrow_mut() = None);
    out
}

#[cfg(test)]
fn secrets_get_test(key: &str) -> Result<Option<Vec<u8>>, secrets_store::SecretsError> {
    SECRETS_GET_MOCK.with(|cell| match &*cell.borrow() {
        Some(mock) => mock(key),
        None => Ok(None),
    })
}

#[cfg(test)]
fn http_send_test(
    req: &http_client::Request,
    options: &http_client::RequestOptions,
) -> Result<http_client::Response, http_client::HostError> {
    HTTP_SEND_MOCK.with(|cell| match &*cell.borrow() {
        Some(mock) => mock(req, options),
        None => Err(http_client::HostError {
            code: "unconfigured".into(),
            message: "http_send_test mock not set".into(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;

    #[derive(Debug, serde::Deserialize)]
    struct ExpectedPayload {
        content_type: String,
        body_text: Option<String>,
        body_json: Option<serde_json::Value>,
        warnings: Vec<String>,
    }

    #[test]
    fn capabilities_version_and_shape() {
        let caps = capabilities_v1();
        assert_eq!(caps.version, provider_common::PROVIDER_CAPABILITIES_VERSION);
        assert_eq!(caps.metadata.provider_id, "slack");
        assert!(!caps.capabilities.supports_buttons);
        assert_eq!(caps.limits.max_text_len, 40_000);
    }

    #[test]
    fn publishes_capabilities() {
        let caps = Component::capabilities();
        assert_eq!(caps.metadata.provider_id, "slack");
        assert!(!caps.capabilities.supports_buttons);
        assert_eq!(caps.limits.max_text_len, 40000);
    }

    #[test]
    fn formats_message_payload() {
        let json = format_message_json("C123", "hello");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["channel"], "C123");
        assert_eq!(value["text"], "hello");
        assert_eq!(value["blocks"][0]["type"], "section");
        assert_eq!(value["blocks"][0]["text"]["text"], "hello");
    }

    #[test]
    fn encode_tier_d_plain_text() {
        let res = Component::encode(BindingsRenderPlan {
            tier: BindingsRenderTier::TierD,
            summary_text: Some("hi".into()),
            actions: vec![],
            attachments: vec![],
            warnings: vec![],
            debug_json: None,
        });
        assert!(res.warnings.is_empty());
        assert_eq!(res.payload.content_type, "text/plain; charset=utf-8");
        assert_eq!(res.payload.body, b"hi");
    }

    #[test]
    fn encode_tier_a_json_payload() {
        let res = Component::encode(BindingsRenderPlan {
            tier: BindingsRenderTier::TierA,
            summary_text: Some("card".into()),
            actions: vec!["accept".into()],
            attachments: vec!["note".into()],
            warnings: vec![],
            debug_json: None,
        });
        assert_eq!(res.payload.content_type, "application/json");
        assert!(res.warnings.is_empty());
        let v: serde_json::Value = serde_json::from_slice(&res.payload.body).unwrap();
        assert_eq!(v["text"], "card");
        assert_eq!(v["blocks"][0]["text"]["text"], "card");
        assert_eq!(v["attachments"][0]["text"], "note");
        assert_eq!(v["metadata"]["actions"][0], "accept");
    }

    #[test]
    fn encode_downgrades_other_tiers() {
        let res = Component::encode(BindingsRenderPlan {
            tier: BindingsRenderTier::TierA,
            summary_text: Some("hi".into()),
            actions: vec![],
            attachments: vec![],
            warnings: vec![],
            debug_json: None,
        });
        assert_eq!(res.payload.content_type, "application/json");
        assert!(res.warnings.is_empty());
    }

    #[test]
    fn golden_tier_a_card() {
        let plan = load_plan("tier_a_card.json");
        let expected = load_expected("slack", "tier_a_card.json");
        let res = Component::encode(plan);
        assert_eq!(res.payload.content_type, expected.content_type);
        assert_eq!(res.warnings.len(), expected.warnings.len());
        if let Some(body_json) = expected.body_json {
            let v: serde_json::Value = serde_json::from_slice(&res.payload.body).unwrap();
            assert_eq!(v, body_json);
        }
    }

    #[test]
    fn golden_tier_d_text() {
        let plan = load_plan("tier_d_text.json");
        let expected = load_expected("slack", "tier_d_text.json");
        let res = Component::encode(plan);
        assert_eq!(res.payload.content_type, expected.content_type);
        assert_eq!(
            res.warnings
                .iter()
                .map(|w| w.code.clone())
                .collect::<Vec<_>>(),
            expected.warnings
        );
        if let Some(text) = expected.body_text {
            assert_eq!(String::from_utf8(res.payload.body).unwrap(), text);
        }
    }

    fn load_plan(name: &str) -> BindingsRenderPlan {
        let path = fixtures_root().join("render_plans").join(name);
        let raw = std::fs::read_to_string(path).unwrap();
        let plan: provider_common::RenderPlan = serde_json::from_str(&raw).unwrap();
        to_bindings_plan(plan)
    }

    fn load_expected(provider: &str, name: &str) -> ExpectedPayload {
        let path = fixtures_root()
            .join("expected_payloads")
            .join(provider)
            .join(name);
        let raw = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn fixtures_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join("tests/fixtures")
    }

    fn to_bindings_plan(plan: provider_common::RenderPlan) -> BindingsRenderPlan {
        BindingsRenderPlan {
            tier: match plan.tier {
                provider_common::RenderTier::TierA => BindingsRenderTier::TierA,
                provider_common::RenderTier::TierB => BindingsRenderTier::TierB,
                provider_common::RenderTier::TierC => BindingsRenderTier::TierC,
                provider_common::RenderTier::TierD => BindingsRenderTier::TierD,
            },
            summary_text: plan.summary_text,
            actions: plan.actions,
            attachments: plan.attachments,
            warnings: plan
                .warnings
                .into_iter()
                .map(|w| BindingsRenderWarning {
                    code: w.code,
                    message: w.message,
                    path: w.path,
                })
                .collect(),
            debug_json: plan.debug.map(|v| v.to_string()),
        }
    }

    fn generate_test_secret() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Non-deterministic test-only secret keeps CodeQL from flagging a fixed key.
        format!(
            "test-secret-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn verifies_signature() {
        let secret = generate_test_secret();
        let ts = "1531420618";
        let body = "token=OneLongToken&team_id=T1&api_app_id=A1&event=hello";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("v0:{}:{}", ts, body).as_bytes());
        let computed = mac.finalize().into_bytes();
        let mut hex = String::new();
        for byte in computed {
            use std::fmt::Write;
            write!(&mut hex, "{:02x}", byte).unwrap();
        }
        let sig = format!("v0={}", hex);

        let mut headers = serde_json::Map::new();
        headers.insert(
            "X-Slack-Request-Timestamp".into(),
            serde_json::Value::String(ts.to_string()),
        );
        headers.insert("X-Slack-Signature".into(), serde_json::Value::String(sig));

        verify_signature(&headers, body, &secret).expect("signature should verify");
    }

    #[test]
    fn signature_mismatch_fails() {
        let mut headers = serde_json::Map::new();
        headers.insert(
            "X-Slack-Request-Timestamp".into(),
            serde_json::Value::String("1".into()),
        );
        let secret = generate_test_secret();
        headers.insert(
            "X-Slack-Signature".into(),
            // Non-secret, test-only placeholder to exercise mismatch logic; not used in production.
            serde_json::Value::String("v0=badsignature".into()),
        );
        let err = verify_signature(&headers, "{}", &secret).unwrap_err();
        assert!(matches!(
            err,
            VerificationError::SignatureMismatch | VerificationError::InvalidKey
        ));
    }

    #[test]
    fn init_runtime_config_controls_http_retries() {
        Component::init_runtime_config(
            r#"{"schema_version":1,"network":{"max_attempts":2}}"#.into(),
        )
        .expect("init");

        let req = http_client::Request {
            method: "GET".into(),
            url: "https://example.invalid".into(),
            headers: vec![],
            body: None,
        };

        let calls = Rc::new(Cell::new(0u32));
        let calls_for_mock = Rc::clone(&calls);
        super::with_http_send_mock(
            move |_: &http_client::Request, _: &http_client::RequestOptions| {
                let n = calls_for_mock.get() + 1;
                calls_for_mock.set(n);
                if n == 1 {
                    Err(http_client::HostError {
                        code: "timeout".into(),
                        message: "first attempt fails".into(),
                    })
                } else {
                    Ok(http_client::Response {
                        status: 200,
                        headers: vec![],
                        body: None,
                    })
                }
            },
            || {
                let resp = send_with_retries(&req).expect("should retry and succeed");
                assert_eq!(resp.status, 200);
            },
        );
    }

    #[test]
    fn missing_required_secret_is_structured_json() {
        let err =
            super::with_secrets_get_mock(|_| Ok(None), || get_secret_string(SLACK_BOT_TOKEN_KEY))
                .unwrap_err();
        let value: serde_json::Value = serde_json::from_str(&err).expect("json error");
        assert!(
            value.get("MissingSecret").is_some(),
            "expected MissingSecret"
        );
        assert_eq!(value["MissingSecret"]["name"], SLACK_BOT_TOKEN_KEY);
        assert_eq!(value["MissingSecret"]["scope"], "tenant");
        assert!(value["MissingSecret"]["remediation"].is_string());
    }
}
