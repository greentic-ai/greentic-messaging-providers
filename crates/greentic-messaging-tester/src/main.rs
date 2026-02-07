mod http_mock;
mod requirements;
mod values;
mod wasm_harness;

use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
    process,
    sync::Arc,
};

use anyhow::anyhow;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::StatusCode,
    response::IntoResponse,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{ArgGroup, Parser, Subcommand};
use greentic_interfaces_wasmtime::host_helpers::v1::http_client;
use greentic_messaging_planned::encode_from_render_plan;
use greentic_types::messaging::universal_dto::{
    Header, HttpInV1, HttpOutV1, ProviderPayloadV1, RenderPlanInV1, SendPayloadInV1,
    SendPayloadResultV1,
};
use greentic_types::{
    ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx, TenantId,
};
use http::Request;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::runtime::Builder;
use tokio::signal;

use crate::http_mock::{HttpHistory, HttpMode, new_history};
use crate::requirements::ValidationReport;
use crate::values::Values;
use crate::wasm_harness::{ComponentHarness, WasmHarness, find_component_wasm_path};
use hmac::{Hmac, Mac};
use sha2::Sha256;

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
        #[arg(long, value_name = "DESTINATION")]
        to: Option<String>,
        #[arg(long, value_name = "DESTINATION_KIND")]
        to_kind: Option<String>,
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

struct ListenParams {
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
            to,
            to_kind,
        } => handle_send(provider, values, text, card, to, to_kind),
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
        } => handle_listen(ListenParams {
            provider,
            values_path: values,
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
        }),
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
    to: Option<String>,
    to_kind: Option<String>,
) -> Result<(), CliError> {
    let values =
        Values::load(&values_path).map_err(|err| CliError::ValuesLoad(values_path.clone(), err))?;
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

    let explicit_text = text.and_then(|t| {
        let trimmed = t.trim();
        if trimmed.is_empty() { None } else { Some(t) }
    });
    let card_text = card_value
        .as_ref()
        .and_then(|card_val: &Value| extract_card_text(card_val));
    let final_text = explicit_text
        .or(card_text)
        .or_else(|| card_value.as_ref().map(|_| "adaptive card".to_string()));

    let metadata = values.to_metadata();
    let mut destinations = Vec::new();
    if let Some(destination) = to {
        let trimmed = destination.trim();
        if trimmed.is_empty() {
            return Err(CliError::ProviderOp(anyhow!("--to cannot be empty")));
        }
        destinations.push(Destination {
            id: trimmed.to_string(),
            kind: to_kind,
        });
    }
    let message = build_message_envelope(&provider, final_text, card_value, metadata, destinations);
    let plan_in = RenderPlanInV1 {
        message: message.clone(),
        metadata: BTreeMap::new(),
    };
    let harness = WasmHarness::new(&provider).map_err(CliError::WasmLoad)?;
    let history = new_history();
    let secrets = values.secret_bytes();
    let http_mode = values.http_mode();

    let plan_input =
        serde_json::to_vec(&plan_in).map_err(|err| CliError::ProviderOp(err.into()))?;
    let plan_output = match harness.invoke(
        "render_plan",
        plan_input,
        &secrets,
        http_mode,
        history.clone(),
        None,
    ) {
        Ok(bytes) => bytes,
        Err(err) => {
            log_http_history("render_plan", &history);
            return Err(map_invoke_error(err));
        }
    };
    let plan_value: Value =
        serde_json::from_slice(&plan_output).map_err(|err| CliError::ProviderOp(err.into()))?;
    ensure_ok(&plan_value, "render_plan")?;
    let plan_json = plan_value
        .get("plan")
        .and_then(|plan| plan.get("plan_json"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| CliError::ProviderOp(anyhow!("render_plan missing plan_json")))?;
    let encode_result = encode_from_render_plan(plan_json, &message, Some(harness.provider_type()));
    if !encode_result.ok {
        return Err(CliError::ProviderOp(anyhow!(
            "encode_from_render_plan failed: {}",
            encode_result.error.unwrap_or_else(|| "unknown".to_string())
        )));
    }
    let provider_payload = encode_result
        .payload
        .as_ref()
        .ok_or_else(|| CliError::ProviderOp(anyhow!("encode_from_render_plan missing payload")))?;
    let payload = ProviderPayloadV1 {
        content_type: provider_payload.content_type.clone(),
        body_b64: provider_payload.body_b64.clone(),
        metadata: provider_payload.metadata.clone().into_iter().collect(),
    };

    let send_in = SendPayloadInV1 {
        provider_type: harness.provider_type().to_string(),
        tenant_id: None,
        auth_user: None,
        payload,
    };
    let send_input =
        serde_json::to_vec(&send_in).map_err(|err| CliError::ProviderOp(err.into()))?;
    let send_output = match harness.invoke(
        "send_payload",
        send_input,
        &secrets,
        http_mode,
        history.clone(),
        None,
    ) {
        Ok(bytes) => bytes,
        Err(err) => {
            log_http_history("send_payload", &history);
            return Err(map_invoke_error(err));
        }
    };
    let send_result: SendPayloadResultV1 =
        serde_json::from_slice(&send_output).map_err(|err| CliError::ProviderOp(err.into()))?;
    if !send_result.ok {
        log_http_history("send_payload", &history);
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
        "encode_result": encode_result,
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
    let mut values =
        Values::load(&values_path).map_err(|err| CliError::ValuesLoad(values_path.clone(), err))?;
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
        .invoke(
            "ingest_http",
            http_bytes,
            &secrets,
            http_mode,
            history,
            None,
        )
        .map_err(map_invoke_error)?;
    let http_out: HttpOutV1 =
        serde_json::from_slice(&out_bytes).map_err(|err| CliError::ProviderOp(err.into()))?;
    let output = json!({
        "envelopes": http_out.events,
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    Ok(())
}

fn handle_listen(params: ListenParams) -> Result<(), CliError> {
    let ListenParams {
        provider,
        values_path,
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
    } = params;
    let mut values =
        Values::load(&values_path).map_err(|err| CliError::ValuesLoad(values_path.clone(), err))?;
    let requirements = requirements::Requirements::load(&provider)
        .map_err(|_| CliError::RequirementsMissing(provider.clone()))?;
    inject_public_base_url(&mut values, &public_base_url);
    let report = requirements.validate(&values);
    if !report.is_empty() {
        print_missing(&report);
        return Err(CliError::Validation { report });
    }

    let secrets = Arc::new(values.secret_bytes());
    let http_mode = values.http_mode();
    let signature_secret = load_webhook_signature_secret(&values, &provider);

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

    run_listener(
        host,
        port,
        path,
        provider,
        secrets,
        http_mode,
        signature_secret,
    )
}

fn handle_webhook(
    provider: String,
    values_path: PathBuf,
    secret_token: Option<String>,
    public_base_url: String,
    dry_run: bool,
) -> Result<(), CliError> {
    let values =
        Values::load(&values_path).map_err(|err| CliError::ValuesLoad(values_path.clone(), err))?;

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
    provider: String,
    secrets: Arc<HashMap<String, Vec<u8>>>,
    http_mode: HttpMode,
    signature_secret: Option<Vec<u8>>,
}

fn run_listener(
    host: String,
    port: u16,
    path: String,
    provider: String,
    secrets: Arc<HashMap<String, Vec<u8>>>,
    http_mode: HttpMode,
    signature_secret: Option<Vec<u8>>,
) -> Result<(), CliError> {
    let bind_addr = format!("{host}:{port}");
    let listener_state = ListenerState {
        expected_path: path.clone(),
        provider,
        secrets,
        http_mode,
        signature_secret,
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
    let headers_map: HashMap<String, String> = headers.iter().cloned().collect();

    if state.0.provider == "webex"
        && let Some(secret) = state.0.signature_secret.as_ref()
        && !verify_webex_signature(secret, &headers, &body_text)
    {
        let err_msg = "invalid webex webhook signature";
        eprintln!("{err_msg}");
        return (StatusCode::UNAUTHORIZED, err_msg.to_string());
    }
    let http_in = HttpInFile {
        method: method.to_ascii_uppercase(),
        path: path.clone(),
        query,
        headers: headers_map,
        body: Some(body_text.clone()),
    };
    let state_clone = state.0.clone();
    match tokio::task::spawn_blocking(move || ingest_http_request(&state_clone, http_in)).await {
        Ok(Ok(envelopes)) => {
            let output = json!({ "ingress_envelopes": envelopes });
            println!("{}", serde_json::to_string_pretty(&output).unwrap());
            std::io::stdout().flush().ok();
            (StatusCode::OK, "ok".to_string())
        }
        Ok(Err(err)) => {
            eprintln!("ingress failed: {}", err);
            (StatusCode::INTERNAL_SERVER_ERROR, err)
        }
        Err(join_err) => {
            let err_msg = format!("ingest runtime panic: {join_err}");
            eprintln!("{err_msg}");
            (StatusCode::INTERNAL_SERVER_ERROR, err_msg)
        }
    }
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

fn log_http_history(op: &str, history: &HttpHistory) {
    if let Ok(calls) = history.lock() {
        if calls.is_empty() {
            println!("http history [{}]: <empty>", op);
            return;
        }
        for (idx, call) in calls.iter().enumerate() {
            println!(
                "http history [{}] call #{idx} {} {} status={} body={}",
                op,
                call.request.method,
                call.request.url,
                call.response.status,
                call.request.body_b64.as_deref().unwrap_or("<no body>")
            );
        }
    }
}

fn build_message_envelope(
    provider: &str,
    text: Option<String>,
    card: Option<Value>,
    metadata: HashMap<String, String>,
    destinations: Vec<Destination>,
) -> ChannelMessageEnvelope {
    println!("tester envelope to={:?}", destinations);
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
    if let Some(card_value) = card
        && let Ok(card_str) = serde_json::to_string(&card_value)
    {
        message_metadata.insert("adaptive_card".to_string(), card_str);
    }
    ChannelMessageEnvelope {
        id: format!("tester-{provider}-{channel}"),
        tenant: TenantCtx::new(env, tenant),
        channel: channel.clone(),
        session_id: channel.clone(),
        reply_scope: None,
        from: None,
        to: destinations,
        correlation_id: None,
        text,
        attachments: Vec::new(),
        metadata: message_metadata,
    }
}

fn extract_card_text(card: &Value) -> Option<String> {
    if let Some(text) = card
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return Some(text.to_string());
    }

    if let Some(body_array) = card.get("body").and_then(Value::as_array) {
        let mut segments = Vec::new();
        for block in body_array {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    segments.push(trimmed.to_string());
                }
            }
        }
        if !segments.is_empty() {
            return Some(segments.join(" "));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_mock::{self, HttpMode, HttpResponseQueue, new_history, new_response_queue};
    use crate::wasm_harness::WasmHarness;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use greentic_types::messaging::universal_dto::{HttpInV1, HttpOutV1};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn build_message_envelope_stores_destination() {
        let metadata = HashMap::new();
        let dest = Destination {
            id: "room-123".to_string(),
            kind: Some("room".to_string()),
        };
        let envelope = build_message_envelope(
            "webex",
            Some("hi".to_string()),
            None,
            metadata,
            vec![dest.clone()],
        );
        assert_eq!(envelope.to, vec![dest]);
    }

    #[test]
    fn build_message_envelope_preserves_destination_kind() {
        let metadata = HashMap::new();
        let dest = Destination {
            id: "dm-123".to_string(),
            kind: Some("user".to_string()),
        };
        let envelope = build_message_envelope(
            "slack",
            Some("hey".to_string()),
            None,
            metadata,
            vec![dest.clone()],
        );
        assert_eq!(envelope.to.len(), 1);
        let stored = &envelope.to[0];
        assert_eq!(stored.kind, dest.kind);
    }

    fn queue_webex_response(queue: &HttpResponseQueue, status: u16, body: Value) {
        http_mock::clear_mock_responses(queue);
        http_mock::queue_mock_response(
            queue,
            status,
            serde_json::to_vec(&body).expect("serialize response"),
        );
    }

    fn build_webhook_payload() -> Value {
        json!({
            "resource": "messages",
            "event": "created",
            "data": {
                "id": "MSG123",
                "roomId": "ROOM1",
                "personEmail": "sender@example.com"
            }
        })
    }

    fn call_ingest(
        payload: &Value,
        secrets: &HashMap<String, Vec<u8>>,
        mock_responses: HttpResponseQueue,
    ) -> HttpOutV1 {
        let harness = WasmHarness::new("webex").expect("load harness");
        let history = new_history();
        let http_in = HttpInV1 {
            method: "POST".to_string(),
            path: "/webhook/webex".to_string(),
            query: None,
            headers: Vec::new(),
            body_b64: STANDARD
                .encode(serde_json::to_vec(payload).expect("serialize webhook payload")),
            route_hint: None,
            binding_id: None,
            config: None,
        };
        let out_bytes = harness
            .invoke(
                "ingest_http",
                serde_json::to_vec(&http_in).expect("serialize http in"),
                secrets,
                HttpMode::Mock,
                history,
                Some(mock_responses.clone()),
            )
            .expect("invoke ingest");
        serde_json::from_slice(&out_bytes).expect("parse http out")
    }

    #[test]
    fn webhook_webex_ingest_fetches_message_body() {
        let message_body = json!({
            "id": "MSG123",
            "markdown": "Hello world",
            "roomId": "ROOM1",
            "personEmail": "sender@example.com"
        });
        let mock_responses = new_response_queue();
        queue_webex_response(&mock_responses, 200, message_body);
        let secrets = HashMap::from([("WEBEX_BOT_TOKEN".to_string(), b"token".to_vec())]);
        let http_out = call_ingest(&build_webhook_payload(), &secrets, mock_responses.clone());
        assert_eq!(http_out.status, 200);
        let envelope = http_out.events.first().expect("event missing");
        assert_eq!(envelope.text.as_deref(), Some("Hello world"));
        assert_eq!(envelope.session_id, "ROOM1");
        assert_eq!(
            envelope.metadata.get("webex.messageId"),
            Some(&"MSG123".to_string())
        );
    }

    #[test]
    fn webhook_webex_ingest_failure_includes_metadata() {
        let mock_responses = new_response_queue();
        queue_webex_response(&mock_responses, 404, json!({"message": "not found"}));
        let secrets = HashMap::from([("WEBEX_BOT_TOKEN".to_string(), b"token".to_vec())]);
        let http_out = call_ingest(&build_webhook_payload(), &secrets, mock_responses.clone());
        assert_eq!(http_out.status, 502);
        let envelope = http_out.events.first().expect("missing envelope");
        assert_eq!(envelope.text.as_deref(), Some(""));
        let normalized: Value =
            serde_json::from_slice(&STANDARD.decode(&http_out.body_b64).expect("decode body"))
                .expect("parse normalized");
        assert_eq!(normalized["ok"], Value::Bool(false));
        assert!(
            normalized["error"]
                .as_str()
                .unwrap_or("")
                .contains("webex returned status 404")
        );
        assert!(
            envelope
                .metadata
                .get("webex.ingestError")
                .map(|value| value.contains("404"))
                .unwrap_or(false)
        );
    }
}

fn ensure_ok(value: &Value, op: &str) -> Result<(), CliError> {
    if let Some(ok) = value.get("ok").and_then(Value::as_bool)
        && !ok
    {
        return Err(CliError::ProviderOp(anyhow!("{} reported failure", op)));
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
    Ok(http_in_file_to_v1(spec))
}

fn http_in_file_to_v1(spec: HttpInFile) -> HttpInV1 {
    let body_bytes = spec.body.map(|body| body.into_bytes()).unwrap_or_default();
    let body_b64 = STANDARD.encode(body_bytes);
    let headers = spec
        .headers
        .into_iter()
        .map(|(name, value)| Header { name, value })
        .collect();
    HttpInV1 {
        method: spec.method,
        path: spec.path,
        query: spec.query,
        headers,
        body_b64,
        route_hint: None,
        binding_id: None,
        config: None,
    }
}

fn ingest_http_request(
    state: &ListenerState,
    http_in: HttpInFile,
) -> Result<Vec<ChannelMessageEnvelope>, String> {
    let harness = WasmHarness::new(&state.provider).map_err(|err| err.to_string())?;
    let http_in_v1 = http_in_file_to_v1(http_in);
    let history = new_history();
    let http_bytes = serde_json::to_vec(&http_in_v1).map_err(|err| err.to_string())?;
    let out_bytes = harness
        .invoke(
            "ingest_http",
            http_bytes,
            state.secrets.as_ref(),
            state.http_mode,
            history,
            None,
        )
        .map_err(|err| map_invoke_error(err).to_string())?;
    let http_out: HttpOutV1 = serde_json::from_slice(&out_bytes).map_err(|err| err.to_string())?;
    Ok(http_out.events)
}

fn load_webhook_signature_secret(values: &Values, provider: &str) -> Option<Vec<u8>> {
    let candidates = [
        format!("{provider}_signature_secret"),
        format!("{provider}_webhook_signature_secret"),
    ];
    for key in candidates {
        if let Some(Value::String(secret)) = values.config.get(&key) {
            return Some(secret.as_bytes().to_vec());
        }
    }
    None
}

fn verify_webex_signature(secret: &[u8], headers: &[(String, String)], body: &str) -> bool {
    let header_value = find_header_value(headers, "x-webex-signature")
        .or_else(|| find_header_value(headers, "x-spark-signature"));
    let header_value = match header_value {
        Some(value) => value,
        None => return false,
    };
    let sha256_part = header_value
        .split(',')
        .find_map(|segment| segment.trim().strip_prefix("SHA-256=").map(|v| v.trim()));
    let hex = match sha256_part {
        Some(value) => value.trim_matches('"'),
        None => return false,
    };
    let sig_bytes = match decode_hex(hex) {
        Some(bytes) => bytes,
        None => return false,
    };
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = match HmacSha256::new_from_slice(secret) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(body.as_bytes());
    mac.verify_slice(&sig_bytes).is_ok()
}

fn find_header_value(headers: &[(String, String)], key: &str) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.clone())
}

fn decode_hex(input: &str) -> Option<Vec<u8>> {
    if !input.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(input.len() / 2);
    let normalized = input.trim();
    for chunk in normalized.as_bytes().chunks(2) {
        let hex_str = std::str::from_utf8(chunk).ok()?;
        bytes.push(u8::from_str_radix(hex_str, 16).ok()?);
    }
    Some(bytes)
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
