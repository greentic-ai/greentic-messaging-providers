#![allow(unsafe_op_in_unsafe_fn)]

#[allow(clippy::too_many_arguments)]
mod bindings {
    wit_bindgen::generate!({ path: "wit/telegram", world: "telegram", generate_all });
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
use bindings::provider::telegram::types::{
    Button, Messagerender as MessageRender, Sendmessagerequest as SendMessageRequest,
    ValidationOutcome, Webhookresult as WebhookResult,
};
use provider_common::ProviderError;
use provider_common::{
    CapabilitiesResponseV1, ProviderCapabilitiesV1, ProviderLimitsV1, ProviderMetadataV1,
};
use provider_runtime_config::ProviderRuntimeConfig;
use std::sync::OnceLock;

const TELEGRAM_API: &str = "https://api.telegram.org";
const TELEGRAM_BOT_TOKEN: &str = "TELEGRAM_BOT_TOKEN";
const MAX_TEXT_LEN: u32 = 4000;
const CALLBACK_DATA_MAX_BYTES: u32 = 64;
const MAX_BUTTONS_PER_ROW: u32 = 5;
const MAX_BUTTON_ROWS: u32 = 8;

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

    fn send_message(req: SendMessageRequest) -> Result<MessageRender, String> {
        let token = get_secret(TELEGRAM_BOT_TOKEN)?;
        let url = format!("{}/bot{}/sendMessage", TELEGRAM_API, token);
        let render = format_message_internal(&req);

        let request = http_client::Request {
            method: "POST".into(),
            url,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some(render.payload_json.clone().into_bytes()),
        };

        let resp = send_with_retries(&request)?;

        if (200..300).contains(&resp.status) {
            log_if_enabled("send_message_success");
            Ok(render)
        } else {
            Err(format!(
                "transport error: telegram returned status {}",
                resp.status
            ))
        }
    }

    fn handle_webhook(_headers_json: String, body_json: String) -> Result<WebhookResult, String> {
        let parsed: serde_json::Value = serde_json::from_str(&body_json)
            .map_err(|_| "validation error: invalid body".to_string())?;
        let normalized = serde_json::json!({ "ok": true, "event": parsed });
        let normalized_json = serde_json::to_string(&normalized)
            .map_err(|_| "other error: serialization failed".to_string())?;
        Ok(WebhookResult {
            validation: ValidationOutcome::Ok,
            normalized_event_json: Some(normalized_json),
            warnings: vec![],
            suggested_http_status: None,
        })
    }

    fn refresh() -> Result<String, String> {
        Ok(r#"{"ok":true,"refresh":"not-needed"}"#.to_string())
    }

    fn format_message(req: SendMessageRequest) -> MessageRender {
        format_message_internal(&req)
    }
}

fn capabilities_v1() -> CapabilitiesResponseV1 {
    CapabilitiesResponseV1::new(
        ProviderMetadataV1 {
            provider_id: "telegram".into(),
            display_name: "Telegram".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            rate_limit_hint: None,
        },
        ProviderCapabilitiesV1 {
            supports_threads: true,
            supports_buttons: true,
            supports_webhook_validation: true,
            supports_formatting_options: true,
        },
        ProviderLimitsV1 {
            max_text_len: MAX_TEXT_LEN,
            callback_data_max_bytes: CALLBACK_DATA_MAX_BYTES,
            max_buttons_per_row: MAX_BUTTONS_PER_ROW,
            max_button_rows: MAX_BUTTON_ROWS,
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

fn format_message_internal(req: &SendMessageRequest) -> MessageRender {
    let mut warnings = Vec::new();
    let text = sanitize_and_truncate(&req.text, &mut warnings);
    let parse_mode = req
        .format_options
        .as_ref()
        .and_then(|o| o.parse_mode.clone())
        .unwrap_or_else(|| "HTML".to_string());

    let keyboard = build_inline_keyboard(&req.buttons, &mut warnings);

    let mut payload = serde_json::json!({
        "chat_id": req.chat_id,
        "text": text,
        "parse_mode": parse_mode,
    });

    if let Some(thread_id) = req.message_thread_id {
        payload["message_thread_id"] = serde_json::json!(thread_id);
    }
    if let Some(reply_id) = req.reply_to_message_id {
        payload["reply_to_message_id"] = serde_json::json!(reply_id);
    }
    if let Some(kb) = keyboard {
        payload["reply_markup"] = serde_json::json!({ "inline_keyboard": kb });
    }

    let payload_json = serde_json::to_string(&payload)
        .unwrap_or_else(|_| "{\"chat_id\":\"\",\"text\":\"\"}".into());

    MessageRender {
        payload_json,
        warnings,
    }
}

fn sanitize_and_truncate(text: &str, warnings: &mut Vec<String>) -> String {
    let escaped = htmlescape::encode_minimal(text);
    let mut bytes = escaped.into_bytes();
    if bytes.len() as u32 > MAX_TEXT_LEN {
        bytes.truncate(MAX_TEXT_LEN as usize);
        warnings.push(format!(
            "text truncated to {} bytes to satisfy limit",
            MAX_TEXT_LEN
        ));
    }
    String::from_utf8(bytes).unwrap_or_default()
}

fn build_inline_keyboard(
    buttons: &[Button],
    warnings: &mut Vec<String>,
) -> Option<Vec<Vec<serde_json::Value>>> {
    if buttons.is_empty() {
        return None;
    }
    let mut rows: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut current: Vec<serde_json::Value> = Vec::new();

    for btn in buttons {
        if rows.len() as u32 >= MAX_BUTTON_ROWS {
            warnings.push("button rows exceeded max; dropping remaining buttons".into());
            break;
        }
        if current.len() as u32 >= MAX_BUTTONS_PER_ROW {
            rows.push(current);
            current = Vec::new();
        }
        match btn {
            Button::OpenUrl(v) => current.push(serde_json::json!({
                "text": v.text,
                "url": v.url,
            })),
            Button::Postback(v) => {
                let data = v.callback_data.as_bytes();
                if data.len() as u32 > CALLBACK_DATA_MAX_BYTES {
                    warnings.push(format!(
                        "callback_data too long ({} bytes), max {}: dropped button '{}'",
                        data.len(),
                        CALLBACK_DATA_MAX_BYTES,
                        v.text
                    ));
                    continue;
                }
                current.push(serde_json::json!({
                    "text": v.text,
                    "callback_data": v.callback_data,
                }));
            }
        }
    }
    if !current.is_empty() && (rows.len() as u32) < MAX_BUTTON_ROWS {
        rows.push(current);
    }
    if rows.is_empty() { None } else { Some(rows) }
}

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
            .unwrap_or_else(|| "telegram".into()),
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

bindings::__export_world_telegram_cabi!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
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
        assert_eq!(caps.metadata.provider_id, "telegram");
        assert!(caps.capabilities.supports_buttons);
        assert_eq!(caps.limits.max_text_len, MAX_TEXT_LEN);
    }

    #[test]
    fn publishes_capabilities() {
        let caps = Component::capabilities();
        assert_eq!(caps.metadata.provider_id, "telegram");
        assert!(caps.capabilities.supports_buttons);
        assert_eq!(caps.limits.max_text_len, MAX_TEXT_LEN);
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
            tier: BindingsRenderTier::TierA,
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
        let expected = load_expected("telegram", "tier_a_card.json");
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
        let expected = load_expected("telegram", "tier_d_text.json");
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
    fn formats_payload_with_threads_and_buttons() {
        let req = SendMessageRequest {
            chat_id: "123".into(),
            text: "<b>hello</b>".into(),
            message_thread_id: Some(42),
            reply_to_message_id: Some(24),
            buttons: vec![
                Button::OpenUrl(bindings::provider::telegram::types::Buttonopenurl {
                    text: "Docs".into(),
                    url: "https://example.com".into(),
                }),
                Button::Postback(bindings::provider::telegram::types::Buttonpostback {
                    text: "Ack".into(),
                    callback_data: "ack".into(),
                }),
            ],
            format_options: Some(bindings::provider::telegram::types::Formatoptions {
                parse_mode: Some("HTML".into()),
            }),
        };

        let render = format_message_internal(&req);
        assert!(render.warnings.is_empty());
        let v: Value = serde_json::from_str(&render.payload_json).unwrap();
        assert_eq!(v["chat_id"], "123");
        assert_eq!(v["text"], "&lt;b&gt;hello&lt;/b&gt;");
        assert_eq!(v["parse_mode"], "HTML");
        assert_eq!(v["message_thread_id"], 42);
        assert_eq!(v["reply_to_message_id"], 24);
        let buttons = v["reply_markup"]["inline_keyboard"].as_array().unwrap();
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].as_array().unwrap().len(), 2);
    }

    #[test]
    fn truncates_text_and_warns() {
        let req = SendMessageRequest {
            chat_id: "123".into(),
            text: "a".repeat((MAX_TEXT_LEN + 10) as usize),
            message_thread_id: None,
            reply_to_message_id: None,
            buttons: vec![],
            format_options: None,
        };
        let render = format_message_internal(&req);
        assert!(
            render.warnings.iter().any(|w| w.contains("truncated")),
            "expected truncation warning"
        );
        let v: Value = serde_json::from_str(&render.payload_json).unwrap();
        assert!(v["text"].as_str().unwrap().len() as u32 <= MAX_TEXT_LEN);
    }

    #[test]
    fn drops_oversize_callback_data_and_warns() {
        let req = SendMessageRequest {
            chat_id: "123".into(),
            text: "hi".into(),
            message_thread_id: None,
            reply_to_message_id: None,
            buttons: vec![
                Button::Postback(bindings::provider::telegram::types::Buttonpostback {
                    text: "TooBig".into(),
                    callback_data: "x".repeat((CALLBACK_DATA_MAX_BYTES + 1) as usize),
                }),
                Button::Postback(bindings::provider::telegram::types::Buttonpostback {
                    text: "Ok".into(),
                    callback_data: "ok".into(),
                }),
            ],
            format_options: None,
        };
        let render = format_message_internal(&req);
        assert!(
            render.warnings.iter().any(|w| w.contains("dropped button")),
            "expected warning about dropped button"
        );
        let v: Value = serde_json::from_str(&render.payload_json).unwrap();
        let buttons = v["reply_markup"]["inline_keyboard"]
            .as_array()
            .expect("inline keyboard");
        assert_eq!(buttons[0].as_array().unwrap().len(), 1);
        assert_eq!(buttons[0][0]["text"], "Ok");
    }

    #[test]
    fn normalizes_webhook() {
        let res = Component::handle_webhook("{}".into(), r#"{"update_id":1}"#.into()).unwrap();
        assert!(matches!(res.validation, ValidationOutcome::Ok));
        let normalized = res.normalized_event_json.unwrap();
        let v: Value = serde_json::from_str(&normalized).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["event"]["update_id"], 1);
        assert!(res.warnings.is_empty());
    }

    #[test]
    fn init_runtime_config_controls_http_retries() {
        let _ = Component::init_runtime_config(
            r#"{"schema_version":1,"network":{"max_attempts":2},"telemetry":{"emit_enabled":false}}"#.into(),
        );

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
        let err = super::with_secrets_get_mock(|_| Ok(None), || get_secret(TELEGRAM_BOT_TOKEN))
            .unwrap_err();
        let value: serde_json::Value = serde_json::from_str(&err).expect("json error");
        assert!(
            value.get("MissingSecret").is_some(),
            "expected MissingSecret"
        );
        assert_eq!(value["MissingSecret"]["name"], TELEGRAM_BOT_TOKEN);
        assert_eq!(value["MissingSecret"]["scope"], "tenant");
        assert!(value["MissingSecret"]["remediation"].is_string());
    }

    #[test]
    fn send_message_retries_and_uses_secret() {
        let _ = Component::init_runtime_config(
            r#"{"schema_version":1,"network":{"max_attempts":2},"telemetry":{"emit_enabled":false}}"#.into(),
        );
        let msg_req = SendMessageRequest {
            chat_id: "chat-1".into(),
            text: "hi".into(),
            message_thread_id: None,
            reply_to_message_id: None,
            buttons: vec![],
            format_options: None,
        };

        let calls = Rc::new(Cell::new(0u32));
        let calls_for_mock = Rc::clone(&calls);
        let resp = super::with_secrets_get_mock(
            |_| Ok(Some(b"token".to_vec())),
            || {
                super::with_http_send_mock(
                    move |_req: &http_client::Request, _opts: &http_client::RequestOptions| {
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
                    || Component::send_message(msg_req.clone()),
                )
            },
        )
        .expect("send succeeds");

        assert!(resp.payload_json.contains("\"chat_id\":\"chat-1\""));
        assert_eq!(calls.get(), 2);
    }
}
