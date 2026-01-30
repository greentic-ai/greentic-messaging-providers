use std::io::Read;
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose};
use greentic_interfaces_wasmtime::host_helpers::v1::http_client;
use http::{Request, Response as RawResponse};
use serde::Serialize;
use ureq::{Body, agent};

pub type HttpHistory = Arc<Mutex<Vec<HttpCall>>>;

#[derive(Debug, Clone, Copy)]
pub enum HttpMode {
    Mock,
    Real,
}

impl Default for HttpMode {
    fn default() -> Self {
        HttpMode::Mock
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HttpCall {
    pub request: HttpRequest,
    pub response: HttpResponseRecord,
}

#[derive(Debug, Clone, Serialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<Header>,
    pub body_b64: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HttpResponseRecord {
    pub status: u16,
    pub headers: Vec<Header>,
    pub body_b64: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

impl HttpRequest {
    pub fn from_host(req: &http_client::RequestV1_1) -> Self {
        let body_b64 = req
            .body
            .as_ref()
            .map(|bytes| general_purpose::STANDARD.encode(bytes));
        let headers = req
            .headers
            .iter()
            .map(|(name, value)| Header {
                name: name.clone(),
                value: value.clone(),
            })
            .collect();
        Self {
            method: req.method.clone(),
            url: req.url.clone(),
            headers,
            body_b64,
        }
    }
}

impl HttpResponseRecord {
    pub fn from_host(resp: &http_client::ResponseV1_1) -> Self {
        let body_b64 = resp
            .body
            .as_ref()
            .map(|bytes| general_purpose::STANDARD.encode(bytes));
        let headers = resp
            .headers
            .iter()
            .map(|(name, value)| Header {
                name: name.clone(),
                value: value.clone(),
            })
            .collect();
        Self {
            status: resp.status,
            headers,
            body_b64,
        }
    }
}

pub fn new_history() -> HttpHistory {
    Arc::new(Mutex::new(Vec::new()))
}

const MOCK_RESPONSE_BODY: &[u8] =
    br#"{"ok":true,"result":{"url":"https://example.invalid/mock-webhook"},"description":"mock response"}"#;

pub fn mock_response() -> http_client::ResponseV1_1 {
    http_client::ResponseV1_1 {
        status: 200,
        headers: Vec::new(),
        body: Some(MOCK_RESPONSE_BODY.to_vec()),
    }
}

pub fn send_real_request(
    req: &http_client::RequestV1_1,
) -> Result<http_client::ResponseV1_1, http_client::HttpClientErrorV1_1> {
    let agent = agent();
    let mut builder = Request::builder().method(req.method.as_str()).uri(&req.url);
    for (name, value) in &req.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let body_bytes = req.body.clone().unwrap_or_default();
    let request =
        builder
            .body(body_bytes.clone())
            .map_err(|err| http_client::HttpClientErrorV1_1 {
                code: "http_request_build".into(),
                message: err.to_string(),
            })?;
    let response = agent.run(request);
    match response {
        Ok(resp) => build_response(resp),
        Err(err) => Err(http_client::HttpClientErrorV1_1 {
            code: "http_transport_error".into(),
            message: err.to_string(),
        }),
    }
}

fn build_response(
    resp: RawResponse<Body>,
) -> Result<http_client::ResponseV1_1, http_client::HttpClientErrorV1_1> {
    let status = resp.status();
    let headers = resp
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let mut reader = resp.into_body().into_reader();
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .map_err(|err| http_client::HttpClientErrorV1_1 {
            code: "http_read_error".into(),
            message: err.to_string(),
        })?;
    Ok(http_client::ResponseV1_1 {
        status: status.as_u16(),
        headers,
        body: Some(body),
    })
}
