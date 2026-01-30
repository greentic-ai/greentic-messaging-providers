#![allow(dead_code)]

use greentic_interfaces_guest::component::node::{InvokeResult, NodeError};
use greentic_interfaces_guest::component_entrypoint;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod bindings {
    wit_bindgen::generate!({
        path: "wit/webex-webhook",
        world: "webex-webhook",
        generate_all
    });
}

use bindings::greentic::http::client;
use bindings::greentic::secrets_store::secrets_store;

const DEFAULT_API_BASE: &str = "https://webexapis.com/v1";
const DEFAULT_RESOURCE: &str = "messages";
const DEFAULT_EVENT: &str = "created";
const TOKEN_SECRET: &str = "WEBEX_BOT_TOKEN";
const DEFAULT_WEBHOOK_NAME: &str = "greentic:webex";
const SIGNATURE_HEADER: &str = "X-Webex-Signature";

#[derive(Deserialize)]
struct ReconcileInput {
    public_base_url: String,
    #[serde(default)]
    secret_token: Option<String>,
    #[serde(default)]
    dry_run: Option<bool>,
    #[serde(default)]
    api_base_url: Option<String>,
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    env_id: Option<String>,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    team_id: Option<String>,
}

#[derive(Serialize)]
struct ReconcileOutput {
    ok: bool,
    provider: String,
    target_url: String,
    webhook_name: String,
    actions: Vec<String>,
    webhooks: Vec<WebhookSummary>,
    notes: Vec<String>,
}

#[derive(Clone, Serialize)]
struct WebhookSummary {
    id: Option<String>,
    name: String,
    resource: String,
    event: String,
    target_url: String,
    status: Option<String>,
}

component_entrypoint!({
    manifest: describe_manifest,
    invoke: handle_message,
    invoke_stream: false,
});

fn describe_manifest() -> String {
    include_str!("../component.manifest.json").to_string()
}

fn handle_message(operation: String, input: String) -> InvokeResult {
    match operation.as_str() {
        "reconcile_webhook" => invoke(reconcile_webhook(&input)),
        _ => InvokeResult::Err(NodeError {
            code: "UNKNOWN_OPERATION".to_string(),
            message: format!("unsupported operation {operation}"),
            retryable: false,
            backoff_ms: None,
            details: None,
        }),
    }
}

fn invoke(result: Result<String, String>) -> InvokeResult {
    match result {
        Ok(value) => InvokeResult::Ok(value),
        Err(message) => InvokeResult::Err(NodeError {
            code: "INVALID_INPUT".to_string(),
            message,
            retryable: false,
            backoff_ms: None,
            details: None,
        }),
    }
}

fn reconcile_webhook(input: &str) -> Result<String, String> {
    let parsed: ReconcileInput =
        serde_json::from_str(input).map_err(|err| format!("invalid input: {err}"))?;
    let target = parsed.public_base_url.trim();
    if target.is_empty() {
        return Err("public_base_url is required".to_string());
    }

    let api_base = parsed
        .api_base_url
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_API_BASE)
        .trim_end_matches('/')
        .to_string();

    let webhook_name = build_webhook_name(&parsed);
    let mut actions = Vec::new();
    let mut notes = vec![format!(
        "Webex subscribes to {resource}.{event} and signs callbacks with the {header} header.",
        resource = DEFAULT_RESOURCE,
        event = DEFAULT_EVENT,
        header = SIGNATURE_HEADER
    )];

    if parsed.dry_run.unwrap_or(false) {
        actions.push("dry-run".to_string());
        if parsed
            .secret_token
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .is_some()
        {
            notes.push(format!(
                "Provided secret will be shared with Webex so the {header} header can be verified on ingress.",
                header = SIGNATURE_HEADER
            ));
        }
        let planned = WebhookSummary {
            id: None,
            name: webhook_name.clone(),
            resource: DEFAULT_RESOURCE.to_string(),
            event: DEFAULT_EVENT.to_string(),
            target_url: target.to_string(),
            status: Some("planned".to_string()),
        };
        let output = ReconcileOutput {
            ok: true,
            provider: "webex".to_string(),
            target_url: target.to_string(),
            webhook_name,
            actions,
            webhooks: vec![planned],
            notes,
        };
        return serde_json::to_string(&output)
            .map_err(|err| format!("serialization failed: {err}"));
    }

    let token = load_token()?;
    let mut webhooks = parse_webhooks(&list_webhooks(&api_base, &token)?);
    actions.push("list".to_string());

    let primary_index = find_primary_webhook_index(&webhooks, &webhook_name, target);

    let final_webhook = if let Some(idx) = primary_index {
        let current = webhooks.get(idx).cloned().expect("index valid");
        if current.target_url != target
            || current.resource != DEFAULT_RESOURCE
            || current.event != DEFAULT_EVENT
        {
            let response = update_webhook(
                &api_base,
                &token,
                &current.id,
                &webhook_name,
                target,
                &parsed.secret_token,
            )?;
            actions.push("update".to_string());
            let updated = WebhookDetails::from_value(&response)?;
            webhooks[idx] = updated.clone();
            updated
        } else {
            actions.push("noop".to_string());
            current
        }
    } else {
        let response = create_webhook(
            &api_base,
            &token,
            &webhook_name,
            target,
            &parsed.secret_token,
        )?;
        actions.push("create".to_string());
        let created = WebhookDetails::from_value(&response)?;
        webhooks.push(created.clone());
        created
    };

    let duplicates: Vec<String> = webhooks
        .iter()
        .filter(|hook| hook.name == webhook_name && hook.id != final_webhook.id)
        .map(|hook| hook.id.clone())
        .collect();
    for dup in &duplicates {
        delete_webhook(&api_base, &token, dup)?;
        actions.push("delete".to_string());
        notes.push(format!("Removed duplicate webhook with id {dup}."));
    }
    webhooks.retain(|hook| !(hook.name == webhook_name && hook.id != final_webhook.id));

    let summaries = webhooks
        .iter()
        .filter(|hook| hook.name == webhook_name)
        .map(|hook| WebhookSummary {
            id: Some(hook.id.clone()),
            name: hook.name.clone(),
            resource: hook.resource.clone(),
            event: hook.event.clone(),
            target_url: hook.target_url.clone(),
            status: hook.status.clone(),
        })
        .collect();

    let output = ReconcileOutput {
        ok: true,
        provider: "webex".to_string(),
        target_url: target.to_string(),
        webhook_name,
        actions,
        webhooks: summaries,
        notes,
    };

    serde_json::to_string(&output).map_err(|err| format!("serialization failed: {err}"))
}

fn build_webhook_name(input: &ReconcileInput) -> String {
    let env_candidates = [input.env.as_deref(), input.env_id.as_deref()];
    let tenant_candidates = [input.tenant.as_deref(), input.tenant_id.as_deref()];
    let team_candidates = [input.team.as_deref(), input.team_id.as_deref()];
    let env_value = first_non_empty(&env_candidates);
    let tenant_value = first_non_empty(&tenant_candidates);
    let team_value = first_non_empty(&team_candidates);

    if let (Some(env), Some(tenant), Some(team)) = (env_value, tenant_value, team_value) {
        format!(
            "greentic:{}:{}:{}:webex",
            env.trim(),
            tenant.trim(),
            team.trim()
        )
    } else {
        DEFAULT_WEBHOOK_NAME.to_string()
    }
}

fn first_non_empty<'a>(candidates: &'a [Option<&'a str>]) -> Option<&'a str> {
    for candidate in candidates {
        if let Some(value) = candidate {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn find_primary_webhook_index(
    webhooks: &[WebhookDetails],
    name: &str,
    target_url: &str,
) -> Option<usize> {
    webhooks
        .iter()
        .position(|hook| {
            hook.name == name && hook.resource == DEFAULT_RESOURCE && hook.event == DEFAULT_EVENT
        })
        .or_else(|| {
            webhooks.iter().position(|hook| {
                hook.target_url == target_url
                    && hook.resource == DEFAULT_RESOURCE
                    && hook.event == DEFAULT_EVENT
            })
        })
}

fn load_token() -> Result<String, String> {
    match secrets_store::get(TOKEN_SECRET) {
        Ok(Some(bytes)) => String::from_utf8(bytes).map_err(|_| "bot token not utf-8".to_string()),
        Ok(None) => Err(format!("missing secret: {TOKEN_SECRET}")),
        Err(err) => Err(format!("secret store error: {err:?}")),
    }
}

fn list_webhooks(api_base: &str, token: &str) -> Result<Value, String> {
    let request = client::Request {
        method: "GET".into(),
        url: format!("{}/webhooks", api_base),
        headers: vec![("Authorization".into(), format!("Bearer {token}"))],
        body: None,
    };
    send_request(&request)
}

fn create_webhook(
    api_base: &str,
    token: &str,
    name: &str,
    target_url: &str,
    secret_token: &Option<String>,
) -> Result<Value, String> {
    let mut payload = json!({
        "name": name,
        "targetUrl": target_url,
        "resource": DEFAULT_RESOURCE,
        "event": DEFAULT_EVENT,
    });
    if let Some(secret) = secret_token.as_deref().filter(|s| !s.trim().is_empty()) {
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("secret".to_string(), Value::String(secret.to_string()));
    }
    let request = client::Request {
        method: "POST".into(),
        url: format!("{}/webhooks", api_base),
        headers: vec![
            ("Authorization".into(), format!("Bearer {token}")),
            ("Content-Type".into(), "application/json".into()),
        ],
        body: Some(
            serde_json::to_vec(&payload)
                .map_err(|err| format!("payload serialization failed: {err}"))?,
        ),
    };
    send_request(&request)
}

fn update_webhook(
    api_base: &str,
    token: &str,
    webhook_id: &str,
    name: &str,
    target_url: &str,
    secret_token: &Option<String>,
) -> Result<Value, String> {
    let mut payload = json!({
        "name": name,
        "targetUrl": target_url,
    });
    if let Some(secret) = secret_token.as_deref().filter(|s| !s.trim().is_empty()) {
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("secret".to_string(), Value::String(secret.to_string()));
    }
    let request = client::Request {
        method: "PUT".into(),
        url: format!("{}/webhooks/{webhook_id}", api_base),
        headers: vec![
            ("Authorization".into(), format!("Bearer {token}")),
            ("Content-Type".into(), "application/json".into()),
        ],
        body: Some(
            serde_json::to_vec(&payload)
                .map_err(|err| format!("payload serialization failed: {err}"))?,
        ),
    };
    send_request(&request)
}

fn delete_webhook(api_base: &str, token: &str, webhook_id: &str) -> Result<(), String> {
    let request = client::Request {
        method: "DELETE".into(),
        url: format!("{}/webhooks/{webhook_id}", api_base),
        headers: vec![("Authorization".into(), format!("Bearer {token}"))],
        body: None,
    };
    let _ = send_request(&request)?;
    Ok(())
}

fn send_request(request: &client::Request) -> Result<Value, String> {
    let resp = client::send(request, None, None).map_err(|err| err.message.clone())?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(format!("webex returned status {}", resp.status));
    }
    let body = resp.body.as_deref().unwrap_or(&[]);
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(body).map_err(|err| format!("invalid json: {err}"))
}

fn parse_webhooks(body: &Value) -> Vec<WebhookDetails> {
    match body.get("items").and_then(|items| items.as_array()) {
        Some(array) => array
            .iter()
            .filter_map(|item| WebhookDetails::from_value(item).ok())
            .collect(),
        None => Vec::new(),
    }
}

#[derive(Clone)]
struct WebhookDetails {
    id: String,
    name: String,
    resource: String,
    event: String,
    target_url: String,
    status: Option<String>,
}

impl WebhookDetails {
    fn from_value(value: &Value) -> Result<Self, String> {
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "webhook missing id".to_string())?
            .to_string();
        let name = value
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_WEBHOOK_NAME)
            .to_string();
        let resource = value
            .get("resource")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_RESOURCE)
            .to_string();
        let event = value
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_EVENT)
            .to_string();
        let target_url = value
            .get("targetUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let status = value
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Ok(WebhookDetails {
            id,
            name,
            resource,
            event,
            target_url,
            status,
        })
    }
}
