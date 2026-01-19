#![allow(unsafe_op_in_unsafe_fn)]

#[allow(clippy::too_many_arguments)]
mod bindings {
    wit_bindgen::generate!({ path: "wit/webex", world: "webex", generate_all });
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
use serde_json::Value;
use std::sync::OnceLock;

const WEBEX_API: &str = "https://webexapis.com/v1/messages";
const WEBEX_BOT_TOKEN: &str = "WEBEX_BOT_TOKEN";

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
        encode_tier_d(plan)
    }

    fn send_message(room_id: String, text: String) -> Result<String, String> {
        let token = get_secret(WEBEX_BOT_TOKEN)?;
        let payload = format_message_json(&room_id, &text);

        let req = http_client::Request {
            method: "POST".into(),
            url: WEBEX_API.into(),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("Authorization".into(), format!("Bearer {}", token)),
            ],
            body: Some(payload.clone().into_bytes()),
        };

        let resp = send_with_retries(&req)?;

        if (200..300).contains(&resp.status) {
            log_if_enabled("send_message_success");
            Ok(payload)
        } else {
            Err(format!(
                "transport error: webex returned status {}",
                resp.status
            ))
        }
    }

    fn handle_webhook(_headers_json: String, body_json: String) -> Result<String, String> {
        let parsed: Value = serde_json::from_str(&body_json)
            .map_err(|_| "validation error: invalid body".to_string())?;
        let normalized = serde_json::json!({ "ok": true, "event": parsed });
        serde_json::to_string(&normalized).map_err(|_| "other error: serialization failed".into())
    }

    fn refresh() -> Result<String, String> {
        Ok(r#"{"ok":true,"refresh":"not-needed"}"#.to_string())
    }

    fn format_message(room_id: String, text: String) -> String {
        format_message_json(&room_id, &text)
    }
}

fn capabilities_v1() -> CapabilitiesResponseV1 {
    CapabilitiesResponseV1::new(
        ProviderMetadataV1 {
            provider_id: "webex".into(),
            display_name: "Webex".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            rate_limit_hint: None,
        },
        ProviderCapabilitiesV1 {
            supports_threads: false,
            supports_buttons: false,
            supports_webhook_validation: false,
            supports_formatting_options: false,
        },
        ProviderLimitsV1 {
            max_text_len: 4_096,
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

fn encode_tier_d(plan: BindingsRenderPlan) -> BindingsEncodeResult {
    let mut warnings = plan.warnings.clone();
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

fn get_secret(key: &str) -> Result<String, String> {
    match secrets_get(key) {
        Ok(Some(bytes)) => String::from_utf8(bytes).map_err(|_| "secret not valid utf-8".into()),
        Ok(None) => Err(missing_secret_error(key)),
        Err(e) => secret_error(key, e),
    }
}

fn secret_error(key: &str, error: secrets_store::SecretsError) -> Result<String, String> {
    Err(match error {
        secrets_store::SecretsError::NotFound => missing_secret_error(key),
        secrets_store::SecretsError::Denied => "secret access denied".into(),
        secrets_store::SecretsError::InvalidKey => "secret key invalid".into(),
        secrets_store::SecretsError::Internal => "secret lookup failed".into(),
    })
}

fn format_message_json(room_id: &str, text: &str) -> String {
    let payload = serde_json::json!({
        "roomId": room_id,
        "text": text,
    });
    serde_json::to_string(&payload).unwrap_or_else(|_| "{\"roomId\":\"\",\"text\":\"\"}".into())
}

bindings::__export_world_webex_cabi!(Component with_types_in bindings);

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
            Err(e) => last_err = Some(format!("transport error: {} ({})", e.message, e.code)),
        }
    }
    Err(last_err.unwrap_or_else(|| "transport error: request failed".into()))
}

fn missing_secret_error(name: &str) -> String {
    serde_json::to_string(&ProviderError::missing_secret(name))
        .unwrap_or_else(|_| format!("missing secret: {name}"))
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
            .unwrap_or_else(|| "webex".into()),
        start_ms: None,
        end_ms: None,
    };
    let fields = [("event".to_string(), event.to_string())];
    let _ = logger_api::log(&span, &fields, None);
}

fn secrets_get(key: &str) -> Result<Option<Vec<u8>>, secrets_store::SecretsError> {
    #[cfg(test)]
    {
        secrets_get_test(key)
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
        http_send_test(req, options)
    }
    #[cfg(not(test))]
    {
        http_client::send(req, Some(*options), None)
    }
}

#[cfg(test)]
type SecretsGetMock = dyn Fn(&str) -> Result<Option<Vec<u8>>, secrets_store::SecretsError>;

#[cfg(test)]
type HttpSendMock = dyn Fn(
    &http_client::Request,
    &http_client::RequestOptions,
) -> Result<http_client::Response, http_client::HostError>;

#[cfg(test)]
thread_local! {
    static SECRETS_GET_MOCK: std::cell::RefCell<Option<Box<SecretsGetMock>>> =
        std::cell::RefCell::new(None);
    static HTTP_SEND_MOCK: std::cell::RefCell<Option<Box<HttpSendMock>>> =
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
        warnings: Vec<String>,
    }

    #[test]
    fn capabilities_version_and_shape() {
        let caps = capabilities_v1();
        assert_eq!(caps.version, provider_common::PROVIDER_CAPABILITIES_VERSION);
        assert_eq!(caps.metadata.provider_id, "webex");
        assert!(!caps.capabilities.supports_buttons);
        assert_eq!(caps.limits.max_text_len, 4_096);
    }

    #[test]
    fn publishes_capabilities() {
        let caps = Component::capabilities();
        assert_eq!(caps.metadata.provider_id, "webex");
        assert!(!caps.capabilities.supports_buttons);
        assert_eq!(caps.limits.max_text_len, 4096);
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
    fn encode_downgrades_other_tiers() {
        let res = Component::encode(BindingsRenderPlan {
            tier: BindingsRenderTier::TierB,
            summary_text: Some("hi".into()),
            actions: vec![],
            attachments: vec![],
            warnings: vec![],
            debug_json: None,
        });
        assert_eq!(res.payload.content_type, "text/plain; charset=utf-8");
        assert!(!res.warnings.is_empty());
        assert_eq!(res.warnings[0].code, "encoder_forced_downgrade");
    }

    #[test]
    fn golden_tier_a_card() {
        let plan = load_plan("tier_a_card.json");
        let expected = load_expected("webex", "tier_a_card.json");
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

    #[test]
    fn golden_tier_d_text() {
        let plan = load_plan("tier_d_text.json");
        let expected = load_expected("webex", "tier_d_text.json");
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

    #[test]
    fn formats_payload() {
        let json = format_message_json("room123", "hello");
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["roomId"], "room123");
        assert_eq!(v["text"], "hello");
    }

    #[test]
    fn normalizes_webhook() {
        let res = Component::handle_webhook("{}".into(), r#"{"id":"1"}"#.into()).unwrap();
        let v: Value = serde_json::from_str(&res).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["event"]["id"], "1");
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
            super::with_secrets_get_mock(|_| Ok(None), || get_secret(WEBEX_BOT_TOKEN)).unwrap_err();
        let value: serde_json::Value = serde_json::from_str(&err).expect("json error");
        assert!(
            value.get("MissingSecret").is_some(),
            "expected MissingSecret"
        );
        assert_eq!(value["MissingSecret"]["name"], WEBEX_BOT_TOKEN);
        assert_eq!(value["MissingSecret"]["scope"], "tenant");
        assert!(value["MissingSecret"]["remediation"].is_string());
    }
}
