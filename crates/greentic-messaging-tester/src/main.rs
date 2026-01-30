mod http_mock;
mod requirements;
mod values;
mod wasm_harness;

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
    process,
};

use anyhow::anyhow;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::StatusCode,
    response::IntoResponse,
};
use base64::{Engine as _, engine::general_purpose};
use clap::{ArgGroup, Parser, Subcommand};
use greentic_interfaces_wasmtime::host_helpers::v1::http_client;
use greentic_types::{ChannelMessageEnvelope, EnvId, MessageMetadata, TenantCtx, TenantId};
use http::Request;
use messaging_universal_dto::{
    EncodeInV1, Header, HttpInV1, HttpOutV1, ProviderPayloadV1, RenderPlanInV1, SendPayloadInV1,
    SendPayloadResultV1,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::runtime::Builder;
use tokio::signal;

use crate::http_mock::new_history;
use crate::requirements::ValidationReport;
use crate::values::Values;
use crate::wasm_harness::{ComponentHarness, WasmHarness, find_component_wasm_path};

#[derive(Parser)]
#[command(name = "greentic-messaging-tester")]
#[command(about = "Minimal CLI to drive provider WASM components", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Requirements {
        #[arg(long)]
        provider: String,
    },
    #[command(group(
        ArgGroup::new("message")
            .required(true)
            .args(["text", "card"])
    ))]
    Send {
        #[arg(long)]
        provider: String,
        #[arg(long, value_name = "VALUES_JSON")]
        values: PathBuf,
        #[arg(long, group = "message")]
        text: Option<String>,
        #[arg(long, value_name = "CARD_JSON", group = "message")]
        card: Option<PathBuf>,
    },
    Ingress {
        #[arg(long)]
        provider: String,
        #[arg(long, value_name = "VALUES_JSON")]
        values: PathBuf,
        #[arg(long, value_name = "HTTP_IN_JSON")]
        http_in: PathBuf,
        #[arg(long, value_name = "PUBLIC_BASE_URL")]
        public_base_url: String,
    },
    Listen {
        #[arg(long)]
        provider: String,
        #[arg(long, value_name = "VALUES_JSON")]
        values: PathBuf,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8080)]
        port: u16,
        #[arg(long, default_value = "/")]
        path: String,
        #[arg(long, value_name = "HTTP_IN_JSON")]
        http_in: Option<PathBuf>,
        #[arg(long, alias = "method", default_value = "POST")]
        http_method: String,
        #[arg(long = "query", value_name = "QUERY")]
        http_query: Option<String>,
        #[arg(long = "body", value_name = "BODY")]
        http_body: Option<String>,
        #[arg(long = "body-file", value_name = "BODY_FILE")]
        http_body_file: Option<PathBuf>,
        #[arg(long = "header", value_name = "HEADER")]
        http_header: Vec<String>,
        #[arg(long, value_name = "PUBLIC_BASE_URL")]
        public_base_url: String,
    },
    Webhook {
        #[arg(long)]
        provider: String,
        #[arg(long, value_name = "VALUES_JSON")]
        values: PathBuf,
        #[arg(long, value_name = "SECRET_TOKEN")]
        secret_token: Option<String>,
        #[arg(long, value_name = "PUBLIC_BASE_URL")]
        public_base_url: String,
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let exit_code = match run(cli) {
        Ok(_) => 0,
        Err(err) => {
            if !matches!(err, CliError::Validation { .. }) {
                eprintln!("error: {err}");
            }
            err.exit_code()
        }
    };
    process::exit(exit_code);
}

fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Command::Requirements { provider } => handle_requirements(provider),
        Command::Send {
            provider,
            values,
            text,
            card,
        } => handle_send(provider, values, text, card),
        Command::Ingress {
            provider,
            values,
            http_in,
            public_base_url,
        } => handle_ingress(provider, values, http_in, public_base_url),
        Command::Listen {
            provider,
            values,
            host,
            port,
            path,
            http_in,
            http_method,
            http_query,
            http_body,
            http_body_file,
            http_header,
            public_base_url,
        } => handle_listen(
            provider,
            values,
            host,
            port,
            path,
            http_in,
            http_method,
            http_query,
            http_body,
            http_body_file,
            http_header,
            public_base_url,
        ),
        Command::Webhook {
            provider,
            values,
            secret_token,
            public_base_url,
            dry_run,
        } => handle_webhook(provider, values, secret_token, public_base_url, dry_run),
    }
}

fn handle_requirements(provider: String) -> Result<(), CliError> {
    let (req, raw) = requirements::Requirements::load_with_raw(&provider)
        .map_err(|_| CliError::RequirementsMissing(provider.clone()))?;
    let maybe_sample = req.values.clone().map(|values| {
        serde_json::to_value(&values)
            .map_err(|err| CliError::RequirementsParse(provider.clone(), err.into()))
    });
    let output = match maybe_sample {
        Some(Ok(value)) => value,
        Some(Err(err)) => return Err(err),
        None => raw,
    };
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    Ok(())
}

fn handle_send(
    provider: String,
    values_path: PathBuf,
    text: Option<String>,
    card: Option<PathBuf>,
) -> Result<(), CliError> {
    let values = Values::load(&values_path)
        .map_err(|err| CliError::ValuesLoad(values_path.clone(), err.into()))?;
    let requirements = requirements::Requirements::load(&provider)
        .map_err(|_| CliError::RequirementsMissing(provider.clone()))?;
    let report = requirements.validate(&values);
    if !report.is_empty() {
        print_missing(&report);
        return Err(CliError::Validation { report });
    }

    if text.is_none() && card.is_none() {
        return Err(CliError::ProviderOp(anyhow!(
            "send requires --text or --card"
        )));
    }

    let card_value = if let Some(card_path) = card {
        let file = File::open(&card_path)
            .map_err(|err| CliError::CardFile(card_path.clone(), err.into()))?;
        Some(
            serde_json::from_reader(file)
                .map_err(|err| CliError::CardParse(card_path.clone(), err.into()))?,
        )
    } else {
        None
    };

    let metadata = values.to_metadata();
    let message = build_message_envelope(&provider, text, card_value, metadata);
    let plan_in = RenderPlanInV1 {
        message: message.clone(),
        metadata: HashMap::new(),
    };
    let harness = WasmHarness::new(&provider).map_err(CliError::WasmLoad)?;
    let history = new_history();
    let secrets = values.secret_bytes();
    let http_mode = values.http_mode();

    let plan_input =
        serde_json::to_vec(&plan_in).map_err(|err| CliError::ProviderOp(err.into()))?;
    let plan_output = harness
        .invoke(
            "render_plan",
            plan_input,
            &secrets,
            http_mode,
            history.clone(),
        )
        .map_err(map_invoke_error)?;
    let plan_value: Value =
        serde_json::from_slice(&plan_output).map_err(|err| CliError::ProviderOp(err.into()))?;
    ensure_ok(&plan_value, "render_plan")?;

    let encode_in = EncodeInV1 {
        message: message.clone(),
        plan: plan_in.clone(),
    };
    let encode_input =
        serde_json::to_vec(&encode_in).map_err(|err| CliError::ProviderOp(err.into()))?;
    let encode_output = harness
        .invoke("encode", encode_input, &secrets, http_mode, history.clone())
        .map_err(map_invoke_error)?;
    let encode_value: Value =
        serde_json::from_slice(&encode_output).map_err(|err| CliError::ProviderOp(err.into()))?;
    ensure_ok(&encode_value, "encode")?;
    let payload_value = encode_value
        .get("payload")
        .cloned()
        .ok_or_else(|| CliError::ProviderOp(anyhow!("encode output missing payload")))?;
    let payload: ProviderPayloadV1 = serde_json::from_value(payload_value.clone())
        .map_err(|err| CliError::ProviderOp(err.into()))?;

    let send_in = SendPayloadInV1 {
        provider_type: harness.provider_type().to_string(),
        tenant_id: None,
        auth_user: None,
        payload,
    };
    let send_input =
        serde_json::to_vec(&send_in).map_err(|err| CliError::ProviderOp(err.into()))?;
    let send_output = harness
        .invoke(
            "send_payload",
            send_input,
            &secrets,
            http_mode,
            history.clone(),
        )
        .map_err(map_invoke_error)?;
    let send_result: SendPayloadResultV1 =
        serde_json::from_slice(&send_output).map_err(|err| CliError::ProviderOp(err.into()))?;
    if !send_result.ok {
        return Err(CliError::ProviderOp(anyhow!(
            "send_payload failed: {}",
            send_result
                .message
                .unwrap_or_else(|| "unknown error".to_string())
        )));
    }

    let http_calls = history
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let output = json!({
        "plan": plan_value,
        "encoded": encode_value,
        "http_calls": http_calls,
        "result": send_result,
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    Ok(())
}

fn handle_ingress(
    provider: String,
    values_path: PathBuf,
    http_in_path: PathBuf,
    public_base_url: String,
) -> Result<(), CliError> {
    let mut values = Values::load(&values_path)
        .map_err(|err| CliError::ValuesLoad(values_path.clone(), err.into()))?;
    let requirements = requirements::Requirements::load(&provider)
        .map_err(|_| CliError::RequirementsMissing(provider.clone()))?;
    inject_public_base_url(&mut values, &public_base_url);
    let report = requirements.validate(&values);
    if !report.is_empty() {
        print_missing(&report);
        return Err(CliError::Validation { report });
    }

    let harness = WasmHarness::new(&provider).map_err(CliError::WasmLoad)?;
    let history = new_history();
    let secrets = values.secret_bytes();
    let http_mode = values.http_mode();

    let http_in = parse_http_in(&http_in_path)?;
    let http_bytes =
        serde_json::to_vec(&http_in).map_err(|err| CliError::ProviderOp(err.into()))?;
    let out_bytes = harness
        .invoke("ingest_http", http_bytes, &secrets, http_mode, history)
        .map_err(map_invoke_error)?;
    let http_out: HttpOutV1 =
        serde_json::from_slice(&out_bytes).map_err(|err| CliError::ProviderOp(err.into()))?;
    let output = json!({
        "envelopes": http_out.events,
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    Ok(())
}

fn handle_listen(
    provider: String,
    values_path: PathBuf,
    host: String,
    port: u16,
    path: String,
    http_in: Option<PathBuf>,
    http_method: String,
    http_query: Option<String>,
    http_body: Option<String>,
    http_body_file: Option<PathBuf>,
    http_header: Vec<String>,
    public_base_url: String,
) -> Result<(), CliError> {
    let mut values = Values::load(&values_path)
        .map_err(|err| CliError::ValuesLoad(values_path.clone(), err.into()))?;
    let requirements = requirements::Requirements::load(&provider)
        .map_err(|_| CliError::RequirementsMissing(provider.clone()))?;
    inject_public_base_url(&mut values, &public_base_url);
    let report = requirements.validate(&values);
    if !report.is_empty() {
        print_missing(&report);
        return Err(CliError::Validation { report });
    }

    if let Some(http_in_path) = http_in {
        let payload = build_http_in_payload(
            http_method,
            path,
            http_query,
            http_body,
            http_body_file,
            http_header,
        )?;
        let json = serde_json::to_string_pretty(&payload)
            .map_err(|err| CliError::ProviderOp(err.into()))?;
        std::fs::write(&http_in_path, json.as_bytes())
            .map_err(|err| CliError::HttpOutput(http_in_path.clone(), err.into()))?;
        println!("{json}");
        eprintln!("http-in payload saved to {}", http_in_path.display());
        return Ok(());
    }

    run_listener(host, port, path)
}

fn handle_webhook(
    provider: String,
    values_path: PathBuf,
    secret_token: Option<String>,
    public_base_url: String,
    dry_run: bool,
) -> Result<(), CliError> {
    let values = Values::load(&values_path)
        .map_err(|err| CliError::ValuesLoad(values_path.clone(), err.into()))?;

    let component = webhook_component_for(&provider)
        .ok_or_else(|| CliError::WebhookUnsupported(provider.clone()))?;
    let component_path = find_component_wasm_path(component).map_err(CliError::Webhook)?;
    let harness = ComponentHarness::new(&component_path).map_err(CliError::Webhook)?;
    let secrets = values.secret_bytes();
    let http_mode = values.http_mode();
    let history = new_history();
    let input = build_webhook_input(public_base_url, secret_token, dry_run)?;
    let input_bytes = serde_json::to_vec(&input).map_err(|err| CliError::ProviderOp(err.into()))?;
    let out_bytes = harness
        .invoke(
            "reconcile_webhook",
            input_bytes,
            &secrets,
            http_mode,
            history,
        )
        .map_err(map_invoke_error)?;
    let output: Value =
        serde_json::from_slice(&out_bytes).map_err(|err| CliError::ProviderOp(err.into()))?;
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    Ok(())
}

fn inject_public_base_url(values: &mut Values, public_base_url: &str) {
    values.config.insert(
        "public_base_url".to_string(),
        Value::String(public_base_url.to_string()),
    );
}

fn webhook_component_for(provider: &str) -> Option<&'static str> {
    match provider {
        "telegram" => Some("telegram-webhook"),
        "webex" => Some("webex-webhook"),
        _ => None,
    }
}

#[derive(Clone)]
struct ListenerState {
    expected_path: String,
}

fn run_listener(host: String, port: u16, path: String) -> Result<(), CliError> {
    let bind_addr = format!("{host}:{port}");
    let listener_state = ListenerState {
        expected_path: path.clone(),
    };
    println!("listening on http://{bind_addr} (logging requests for {path})");

    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err: io::Error| CliError::Listen(err.to_string()))?;
    let bind_addr_clone = bind_addr.clone();
    runtime.block_on(async move {
        let listener = TcpListener::bind(bind_addr_clone)
            .await
            .map_err(|err| CliError::Listen(err.to_string()))?;
        let app = Router::new()
            .fallback(handle_listener_request)
            .with_state(listener_state);
        axum::serve(listener, app)
            .with_graceful_shutdown(wait_for_shutdown())
            .await
            .map_err(|err| CliError::Listen(err.to_string()))
    })
}

async fn handle_listener_request(
    state: State<ListenerState>,
    req: Request<Body>,
) -> impl IntoResponse {
    let expected_path = state.0.expected_path.clone();
    let method = req.method().to_string();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query = uri.query().map(|q| q.to_string());
    let headers = req
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect::<Vec<_>>();
    let body_bytes = to_bytes(req.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    let detail = json!({
        "method": method,
        "path": path,
        "query": query,
        "headers": headers,
        "body": body_text,
        "body_length": body_bytes.len(),
        "expected_path": expected_path,
    });
    println!("{}", serde_json::to_string_pretty(&detail).unwrap());
    std::io::stdout().flush().ok();
    (StatusCode::OK, "ok")
}

async fn wait_for_shutdown() {
    signal::ctrl_c().await.ok();
}

fn build_http_in_payload(
    method: String,
    path: String,
    query: Option<String>,
    body: Option<String>,
    body_file: Option<PathBuf>,
    headers: Vec<String>,
) -> Result<HttpInFile, CliError> {
    let resolved_body = resolve_body(body, body_file)?;
    let mut header_map = HashMap::new();
    for header in headers {
        let (name, value) = parse_header(&header)?;
        header_map.insert(name, value);
    }
    Ok(HttpInFile {
        method: method.to_ascii_uppercase(),
        path,
        query,
        headers: header_map,
        body: resolved_body,
    })
}

fn resolve_body(
    body: Option<String>,
    body_file: Option<PathBuf>,
) -> Result<Option<String>, CliError> {
    match (body, body_file) {
        (Some(b), None) => Ok(Some(b)),
        (None, Some(file)) => {
            let bytes = std::fs::read(&file)
                .map_err(|err| CliError::HttpInput(file.clone(), err.into()))?;
            Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
        }
        (Some(_), Some(_)) => Err(CliError::Listen(
            "--body and --body-file cannot be provided together".to_string(),
        )),
        (None, None) => Ok(None),
    }
}

fn parse_header(raw: &str) -> Result<(String, String), CliError> {
    let separator = raw.find(':').or_else(|| raw.find('='));
    match separator {
        Some(index) if index + 1 < raw.len() => {
            let name = raw[..index].trim().to_ascii_lowercase();
            let value = raw[index + 1..].trim().to_string();
            Ok((name, value))
        }
        _ => Err(CliError::Listen(format!(
            "invalid header '{}', expected 'name:value'",
            raw
        ))),
    }
}

fn print_missing(report: &ValidationReport) {
    let message = json!({
        "error": "missing required values",
        "missing": {
            "config": report.missing_config,
            "secrets": report.missing_secrets,
            "to": report.missing_to,
        }
    });
    println!("{}", serde_json::to_string_pretty(&message).unwrap());
}

fn build_message_envelope(
    provider: &str,
    text: Option<String>,
    card: Option<Value>,
    metadata: HashMap<String, String>,
) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("manual").expect("manual env id");
    let tenant = TenantId::try_from("manual").expect("manual tenant id");
    let channel = metadata
        .get("channel")
        .cloned()
        .unwrap_or_else(|| provider.to_string());
    let mut message_metadata: MessageMetadata = MessageMetadata::new();
    for (key, value) in metadata {
        message_metadata.insert(key, value);
    }
    if let Some(card_value) = card {
        if let Ok(card_str) = serde_json::to_string(&card_value) {
            message_metadata.insert("adaptive_card".to_string(), card_str);
        }
    }
    ChannelMessageEnvelope {
        id: format!("tester-{provider}-{channel}"),
        tenant: TenantCtx::new(env, tenant),
        channel: channel.clone(),
        session_id: channel.clone(),
        reply_scope: None,
        user_id: None,
        correlation_id: None,
        text,
        attachments: Vec::new(),
        metadata: message_metadata,
    }
}

fn ensure_ok(value: &Value, op: &str) -> Result<(), CliError> {
    if let Some(ok) = value.get("ok").and_then(Value::as_bool) {
        if !ok {
            return Err(CliError::ProviderOp(anyhow!("{} reported failure", op)));
        }
    }
    Ok(())
}

fn map_invoke_error(err: anyhow::Error) -> CliError {
    if let Some(http_err) = err.downcast_ref::<http_client::HttpClientErrorV1_1>() {
        CliError::Network(format!("{}: {}", http_err.code, http_err.message))
    } else {
        CliError::ProviderOp(err)
    }
}

fn parse_http_in(path: &Path) -> Result<HttpInV1, CliError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| CliError::HttpInput(path.to_path_buf(), err.into()))?;
    let spec: HttpInFile = serde_json::from_str(&contents)
        .map_err(|err| CliError::HttpInput(path.to_path_buf(), err.into()))?;
    let body_bytes = spec.body.map(|body| body.into_bytes()).unwrap_or_default();
    let body_b64 = general_purpose::STANDARD.encode(body_bytes);
    let headers = spec
        .headers
        .into_iter()
        .map(|(name, value)| Header { name, value })
        .collect();
    Ok(HttpInV1 {
        method: spec.method,
        path: spec.path,
        query: spec.query,
        headers,
        body_b64,
        route_hint: None,
        binding_id: None,
        config: None,
    })
}

#[derive(Serialize, Deserialize)]
struct HttpInFile {
    method: String,
    path: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    query: Option<String>,
}

#[derive(Serialize)]
struct WebhookInput {
    public_base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dry_run: Option<bool>,
}

fn build_webhook_input(
    public_base_url: String,
    secret_token: Option<String>,
    dry_run: bool,
) -> Result<WebhookInput, CliError> {
    let trimmed = public_base_url.trim();
    if trimmed.is_empty() {
        return Err(CliError::Webhook(anyhow!("public_base_url is required")));
    }
    Ok(WebhookInput {
        public_base_url: trimmed.to_string(),
        secret_token,
        dry_run: if dry_run { Some(true) } else { None },
    })
}

#[derive(thiserror::Error, Debug)]
enum CliError {
    #[error("requirements missing for provider {0}")]
    RequirementsMissing(String),
    #[error("failed to parse requirements for {0}: {1}")]
    RequirementsParse(String, #[source] anyhow::Error),
    #[error("values load failed ({0}): {1}")]
    ValuesLoad(PathBuf, #[source] anyhow::Error),
    #[error("http input load failed ({0}): {1}")]
    HttpInput(PathBuf, #[source] anyhow::Error),
    #[error("failed to write http-in file ({0}): {1}")]
    HttpOutput(PathBuf, #[source] anyhow::Error),
    #[error("card file failed ({0}): {1}")]
    CardFile(PathBuf, #[source] anyhow::Error),
    #[error("card parse failed ({0}): {1}")]
    CardParse(PathBuf, #[source] anyhow::Error),
    #[error("validation failed")]
    Validation { report: ValidationReport },
    #[error("wasm load failure: {0}")]
    WasmLoad(#[source] anyhow::Error),
    #[error("provider operation failed: {0}")]
    ProviderOp(#[source] anyhow::Error),
    #[error("webhook reconciliation failed: {0}")]
    Webhook(#[source] anyhow::Error),
    #[error("webhook component not available for provider {0}")]
    WebhookUnsupported(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("listen helper failure: {0}")]
    Listen(String),
}

impl CliError {
    fn exit_code(&self) -> i32 {
        match self {
            CliError::RequirementsMissing(_) => 2,
            CliError::RequirementsParse(_, _) => 2,
            CliError::Validation { .. } => 2,
            CliError::ValuesLoad(_, _) => 1,
            CliError::HttpInput(_, _) => 1,
            CliError::HttpOutput(_, _) => 6,
            CliError::CardFile(_, _) => 1,
            CliError::CardParse(_, _) => 1,
            CliError::WasmLoad(_) => 3,
            CliError::ProviderOp(_) => 4,
            CliError::Webhook(_) => 8,
            CliError::WebhookUnsupported(_) => 9,
            CliError::Network(_) => 5,
            CliError::Listen(_) => 7,
        }
    }
}
