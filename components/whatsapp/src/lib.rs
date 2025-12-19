#![allow(unsafe_op_in_unsafe_fn)]

mod bindings {
    wit_bindgen::generate!({ path: "wit/whatsapp", world: "whatsapp", generate_all });
}

use bindings::Guest;
use bindings::greentic::http::http_client;
use bindings::greentic::secrets_store::secrets_store;
use bindings::greentic::telemetry::logger_api;
use provider_common::ProviderError;
use provider_runtime_config::ProviderRuntimeConfig;
use serde_json::Value;
use std::sync::OnceLock;

const WHATSAPP_API: &str = "https://graph.facebook.com/v18.0";
const WHATSAPP_TOKEN: &str = "WHATSAPP_TOKEN";
const WHATSAPP_VERIFY_TOKEN: &str = "WHATSAPP_VERIFY_TOKEN";

static RUNTIME_CONFIG: OnceLock<ProviderRuntimeConfig> = OnceLock::new();

struct Component;

impl Guest for Component {
    fn init_runtime_config(config_json: String) -> Result<(), String> {
        let config = parse_runtime_config(&config_json)?;
        set_runtime_config(config)
    }

    fn send_message(destination_json: String, text: String) -> Result<String, String> {
        let dest = parse_destination(&destination_json)?;
        let token = get_secret(WHATSAPP_TOKEN)?;

        let url = format!("{}/{}/messages", WHATSAPP_API, dest.phone_id);
        let payload = format_message_json(&destination_json, &text);

        let req = http_client::Request {
            method: "POST".into(),
            url,
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
                "transport error: whatsapp returned status {}",
                resp.status
            ))
        }
    }

    fn handle_webhook(headers_json: String, body_json: String) -> Result<String, String> {
        // Parse headers for validation if needed; currently unused.
        let _headers: Value = serde_json::from_str(&headers_json)
            .map_err(|_| "validation error: invalid headers".to_string())?;

        let parsed: Value = serde_json::from_str(&body_json)
            .map_err(|_| "validation error: invalid body".to_string())?;

        if let Some(token) = parsed
            .get("hub.verify_token")
            .or_else(|| parsed.get("verify_token"))
            .and_then(Value::as_str)
        {
            let expected = get_secret(WHATSAPP_VERIFY_TOKEN)?;
            if token != expected {
                return Err("validation error: verify token mismatch".into());
            }
        }

        let normalized = serde_json::json!({ "ok": true, "event": parsed });
        serde_json::to_string(&normalized).map_err(|_| "other error: serialization failed".into())
    }

    fn refresh() -> Result<String, String> {
        Ok(r#"{"ok":true,"refresh":"not-needed"}"#.to_string())
    }

    fn format_message(destination_json: String, text: String) -> String {
        format_message_json(&destination_json, &text)
    }
}

#[derive(Debug)]
struct Destination {
    phone_id: String,
    to: String,
}

fn parse_destination(json: &str) -> Result<Destination, String> {
    let value: Value = serde_json::from_str(json)
        .map_err(|_| "validation error: invalid destination".to_string())?;
    let phone_id = value
        .get("phone_number_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "validation error: missing phone_number_id".to_string())?;
    let to = value
        .get("to")
        .and_then(Value::as_str)
        .ok_or_else(|| "validation error: missing to".to_string())?;
    Ok(Destination {
        phone_id: phone_id.to_string(),
        to: to.to_string(),
    })
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

fn format_message_json(destination_json: &str, text: &str) -> String {
    let dest = parse_destination(destination_json).ok();
    let payload = serde_json::json!({
        "messaging_product": "whatsapp",
        "to": dest.as_ref().map(|d| d.to.as_str()).unwrap_or(""),
        "type": "text",
        "text": { "body": text },
    });
    serde_json::to_string(&payload)
        .unwrap_or_else(|_| "{\"to\":\"\",\"text\":{\"body\":\"\"}}".into())
}

bindings::__export_world_whatsapp_cabi!(Component with_types_in bindings);

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

fn log_if_enabled(event: &str) -> () {
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
            .unwrap_or_else(|| "whatsapp".into()),
        start_ms: None,
        end_ms: None,
    };
    let fields = [("event".to_string(), event.to_string())];
    let _ = logger_api::log(&span, &fields, None);
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
    use std::rc::Rc;

    #[test]
    fn parses_destination() {
        let dest = parse_destination(r#"{"phone_number_id":"pn1","to":"+100"}"#).unwrap();
        assert_eq!(dest.phone_id, "pn1");
        assert_eq!(dest.to, "+100");
    }

    #[test]
    fn formats_payload() {
        let json = format_message_json(r#"{"phone_number_id":"pn1","to":"+100"}"#, "hi");
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["messaging_product"], "whatsapp");
        assert_eq!(v["to"], "+100");
        assert_eq!(v["text"]["body"], "hi");
    }

    #[test]
    fn webhook_normalizes() {
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
            super::with_secrets_get_mock(|_| Ok(None), || get_secret(WHATSAPP_TOKEN)).unwrap_err();
        let value: serde_json::Value = serde_json::from_str(&err).expect("json error");
        assert!(
            value.get("MissingSecret").is_some(),
            "expected MissingSecret"
        );
        assert_eq!(value["MissingSecret"]["name"], WHATSAPP_TOKEN);
        assert_eq!(value["MissingSecret"]["scope"], "tenant");
        assert!(value["MissingSecret"]["remediation"].is_string());
    }
}
