#![allow(unsafe_op_in_unsafe_fn)]

mod bindings {
    wit_bindgen::generate!({ path: "wit/webchat", world: "webchat", generate_all });
}

use bindings::Guest;
use bindings::greentic::http::http_client;
use bindings::greentic::secrets_store::secrets_store;
use bindings::greentic::telemetry::logger_api;
use provider_runtime_config::ProviderRuntimeConfig;
use serde_json::Value;
use std::sync::OnceLock;

const DEFAULT_WEBCHAT_URL: &str = "https://example.invalid/webchat/send";
const WEBCHAT_BEARER: &str = "WEBCHAT_BEARER_TOKEN";

static RUNTIME_CONFIG: OnceLock<ProviderRuntimeConfig> = OnceLock::new();

struct Component;

impl Guest for Component {
    fn init_runtime_config(config_json: String) -> Result<(), String> {
        let config = parse_runtime_config(&config_json)?;
        set_runtime_config(config)
    }

    fn send_message(session_id: String, text: String) -> Result<String, String> {
        let payload = format_message_json(&session_id, &text);
        let token = get_optional_secret(WEBCHAT_BEARER);

        let req = http_client::Request {
            method: "POST".into(),
            url: DEFAULT_WEBCHAT_URL.into(),
            headers: match token {
                Some(Ok(t)) => vec![
                    ("Content-Type".into(), "application/json".into()),
                    ("Authorization".into(), format!("Bearer {}", t)),
                ],
                _ => vec![("Content-Type".into(), "application/json".into())],
            },
            body: Some(payload.clone().into_bytes()),
        };

        let resp = send_with_retries(&req)?;

        if (200..300).contains(&resp.status) {
            log_if_enabled("send_message_success");
            Ok(payload)
        } else {
            Err(format!(
                "transport error: webchat returned status {}",
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

    fn format_message(session_id: String, text: String) -> String {
        format_message_json(&session_id, &text)
    }
}

fn get_optional_secret(key: &str) -> Option<Result<String, String>> {
    match secrets_get(key) {
        Ok(Some(bytes)) => {
            Some(String::from_utf8(bytes).map_err(|_| "secret not valid utf-8".into()))
        }
        Ok(None) => None,
        Err(secrets_store::SecretsError::NotFound) => None,
        Err(e) => Some(secret_error(e)),
    }
}

fn secret_error(error: secrets_store::SecretsError) -> Result<String, String> {
    Err(match error {
        secrets_store::SecretsError::NotFound => "secret not found".into(),
        secrets_store::SecretsError::Denied => "secret access denied".into(),
        secrets_store::SecretsError::InvalidKey => "secret key invalid".into(),
        secrets_store::SecretsError::Internal => "secret lookup failed".into(),
    })
}

fn format_message_json(session_id: &str, text: &str) -> String {
    let payload = serde_json::json!({
        "session_id": session_id,
        "text": text,
    });
    serde_json::to_string(&payload).unwrap_or_else(|_| "{\"session_id\":\"\",\"text\":\"\"}".into())
}

bindings::__export_world_webchat_cabi!(Component with_types_in bindings);

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
            .unwrap_or_else(|| "webchat".into()),
        start_ms: None,
        end_ms: None,
    };
    let fields = [("event".to_string(), event.to_string())];
    let _ = logger_api::log(&span, &fields, None);
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
    fn formats_payload() {
        let json = format_message_json("sess-1", "hello");
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["session_id"], "sess-1");
        assert_eq!(v["text"], "hello");
    }

    #[test]
    fn normalizes_webhook() {
        let res = Component::handle_webhook("{}".into(), r#"{"message":"hi"}"#.into()).unwrap();
        let v: Value = serde_json::from_str(&res).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["event"]["message"], "hi");
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
    fn optional_secret_not_found_is_not_an_error() {
        let res = super::with_secrets_get_mock(
            |_| Err(secrets_store::SecretsError::NotFound),
            || get_optional_secret(WEBCHAT_BEARER),
        );
        assert!(res.is_none());
    }
}
