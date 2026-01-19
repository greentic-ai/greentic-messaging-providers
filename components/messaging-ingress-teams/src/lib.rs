mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-ingress-teams",
        world: "messaging-ingress-teams",
        generate_all
    });
}

use bindings::exports::provider::common::ingress::Guest as IngressGuest;
use bindings::exports::provider::common::subscriptions::Guest as SubscriptionsGuest;
use bindings::greentic::http::http_client;
use bindings::greentic::secrets_store::secrets_store;
use bindings::greentic::state::state_store;
use serde::Deserialize;
use serde_json::{Value, json};
use urlencoding::encode;

const DEFAULT_GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const DEFAULT_AUTH_BASE: &str = "https://login.microsoftonline.com";
const DEFAULT_TOKEN_SCOPE: &str = "https://graph.microsoft.com/.default";
const DEFAULT_CLIENT_SECRET_KEY: &str = "MS_GRAPH_CLIENT_SECRET";
const STATE_KEY: &str = "messaging.teams.subscriptions";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    tenant_id: String,
    client_id: String,
    #[serde(default)]
    client_secret_key: Option<String>,
    #[serde(default)]
    refresh_token_key: Option<String>,
    #[serde(default)]
    graph_base_url: Option<String>,
    #[serde(default)]
    auth_base_url: Option<String>,
    #[serde(default)]
    token_scope: Option<String>,
}

#[derive(Debug, Clone)]
struct SubscriptionSpec {
    resource: String,
    change_type: String,
    expiration_datetime: Option<String>,
    client_state: Option<String>,
}

#[derive(Debug, Clone)]
struct ExistingSubscription {
    id: String,
    resource: String,
    change_type: String,
    expiration_datetime: Option<String>,
    notification_url: Option<String>,
}

struct Component;

impl IngressGuest for Component {
    fn handle_webhook(_headers_json: String, body_json: String) -> Result<String, String> {
        let parsed: Value = serde_json::from_str(&body_json)
            .map_err(|_| "validation error: invalid body".to_string())?;
        let normalized = json!({ "ok": true, "event": parsed });
        serde_json::to_string(&normalized)
            .map_err(|_| "other error: serialization failed".to_string())
    }
}

impl SubscriptionsGuest for Component {
    fn sync_subscriptions(config_json: String, state_json: String) -> Result<String, String> {
        let config = parse_config(&config_json)?;
        let state_val = parse_state(&state_json)?;
        let webhook_url = state_val
            .get("webhook_url")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing webhook_url".to_string())?;
        let desired = parse_desired_subscriptions(&state_val)?;
        if desired.is_empty() {
            return Err("no desired_subscriptions provided".into());
        }

        let token = acquire_token(&config)?;
        let mut existing = list_subscriptions(&config, &token)?;
        let mut actions: Vec<Value> = Vec::new();

        for spec in desired {
            if let Some(found) = find_matching(&existing, &spec, webhook_url) {
                if let Some(expiration) = spec.expiration_datetime.clone() {
                    renew_subscription(&config, &token, &found.id, &expiration)?;
                    actions.push(json!({
                        "action": "renewed",
                        "id": found.id,
                        "resource": found.resource,
                        "change_type": found.change_type,
                        "expiration_datetime": expiration,
                    }));
                }
            } else {
                let created = create_subscription(&config, &token, webhook_url, &spec)?;
                actions.push(json!({
                    "action": "created",
                    "id": created.id,
                    "resource": created.resource,
                    "change_type": created.change_type,
                    "expiration_datetime": created.expiration_datetime,
                }));
                existing.push(created);
            }
        }

        let state_out = json!({
            "ok": true,
            "webhook_url": webhook_url,
            "desired_subscriptions": desired_specs_to_json(&state_val),
            "subscriptions": existing_subscriptions_to_json(&existing),
            "actions": actions,
        });

        write_state(&state_out)?;
        serde_json::to_string(&state_out)
            .map_err(|_| "other error: serialization failed".to_string())
    }
}

bindings::exports::provider::common::ingress::__export_provider_common_ingress_0_0_2_cabi!(
    Component with_types_in bindings::exports::provider::common::ingress
);
bindings::exports::provider::common::subscriptions::__export_provider_common_subscriptions_0_0_2_cabi!(
    Component with_types_in bindings::exports::provider::common::subscriptions
);

fn parse_config(config_json: &str) -> Result<ProviderConfig, String> {
    if config_json.trim().is_empty() {
        return Err("config_json required".into());
    }
    serde_json::from_str::<ProviderConfig>(config_json).map_err(|e| format!("invalid config: {e}"))
}

fn parse_state(state_json: &str) -> Result<Value, String> {
    if state_json.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str::<Value>(state_json).map_err(|_| "invalid state_json".to_string())
}

fn parse_desired_subscriptions(state: &Value) -> Result<Vec<SubscriptionSpec>, String> {
    let desired = state
        .get("desired_subscriptions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    desired
        .into_iter()
        .map(|entry| {
            let resource = entry
                .get("resource")
                .and_then(Value::as_str)
                .ok_or_else(|| "desired_subscriptions.resource required".to_string())?;
            let change_type = entry
                .get("change_type")
                .and_then(Value::as_str)
                .unwrap_or("created");
            let expiration_datetime = entry
                .get("expiration_datetime")
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            let client_state = entry
                .get("client_state")
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            Ok(SubscriptionSpec {
                resource: resource.to_string(),
                change_type: change_type.to_string(),
                expiration_datetime,
                client_state,
            })
        })
        .collect()
}

fn desired_specs_to_json(state: &Value) -> Value {
    state
        .get("desired_subscriptions")
        .cloned()
        .unwrap_or_else(|| json!([]))
}

fn existing_subscriptions_to_json(existing: &[ExistingSubscription]) -> Value {
    let list: Vec<Value> = existing
        .iter()
        .map(|sub| {
            json!({
                "id": sub.id,
                "resource": sub.resource,
                "change_type": sub.change_type,
                "expiration_datetime": sub.expiration_datetime,
                "notification_url": sub.notification_url,
            })
        })
        .collect();
    Value::Array(list)
}

fn find_matching<'a>(
    existing: &'a [ExistingSubscription],
    desired: &SubscriptionSpec,
    webhook_url: &str,
) -> Option<&'a ExistingSubscription> {
    existing.iter().find(|sub| {
        sub.resource == desired.resource
            && sub.change_type == desired.change_type
            && sub
                .notification_url
                .as_ref()
                .map(|url| url == webhook_url)
                .unwrap_or(false)
    })
}

fn acquire_token(cfg: &ProviderConfig) -> Result<String, String> {
    let auth_base = cfg
        .auth_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_AUTH_BASE.to_string());
    let token_url = format!("{}/{}/oauth2/v2.0/token", auth_base, cfg.tenant_id);
    let scope = cfg
        .token_scope
        .clone()
        .unwrap_or_else(|| DEFAULT_TOKEN_SCOPE.to_string());

    if let Some(rt_key) = cfg.refresh_token_key.as_ref() {
        let refresh_token = get_secret(rt_key)?;
        let mut form = format!(
            "client_id={}&grant_type=refresh_token&refresh_token={}&scope={}",
            encode(&cfg.client_id),
            encode(&refresh_token),
            encode(&scope)
        );
        if let Some(secret_key) = cfg.client_secret_key.as_ref()
            && let Ok(secret) = get_secret(secret_key)
        {
            form.push_str(&format!("&client_secret={}", encode(&secret)));
        }
        return send_token_request(&token_url, &form);
    }

    let client_secret_key = cfg
        .client_secret_key
        .as_deref()
        .unwrap_or(DEFAULT_CLIENT_SECRET_KEY);
    let client_secret = get_secret(client_secret_key)?;
    let form = format!(
        "client_id={}&client_secret={}&grant_type=client_credentials&scope={}",
        encode(&cfg.client_id),
        encode(&client_secret),
        encode(&scope)
    );
    send_token_request(&token_url, &form)
}

fn send_token_request(url: &str, form: &str) -> Result<String, String> {
    let request = http_client::Request {
        method: "POST".into(),
        url: url.to_string(),
        headers: vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body: Some(form.as_bytes().to_vec()),
    };

    let resp = http_client::send(&request, None, None)
        .map_err(|e| format!("transport error: {}", e.message))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(format!("token endpoint returned status {}", resp.status));
    }
    let body = resp.body.unwrap_or_default();
    let json: Value =
        serde_json::from_slice(&body).map_err(|e| format!("invalid token response: {e}"))?;
    let token = json
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "token response missing access_token".to_string())?;
    Ok(token.to_string())
}

fn list_subscriptions(
    cfg: &ProviderConfig,
    token: &str,
) -> Result<Vec<ExistingSubscription>, String> {
    let graph_base = cfg
        .graph_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());
    let url = format!("{}/subscriptions", graph_base);
    let request = http_client::Request {
        method: "GET".into(),
        url,
        headers: vec![("Authorization".into(), format!("Bearer {}", token))],
        body: None,
    };
    let resp = http_client::send(&request, None, None)
        .map_err(|e| format!("transport error: {}", e.message))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(format!("graph returned status {}", resp.status));
    }
    let body = resp.body.unwrap_or_default();
    let json: Value = serde_json::from_slice(&body)
        .map_err(|e| format!("invalid subscriptions response: {e}"))?;
    let list = json
        .get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for item in list {
        let id = item.get("id").and_then(Value::as_str);
        let resource = item.get("resource").and_then(Value::as_str);
        let change_type = item.get("changeType").and_then(Value::as_str);
        if let (Some(id), Some(resource), Some(change_type)) = (id, resource, change_type) {
            out.push(ExistingSubscription {
                id: id.to_string(),
                resource: resource.to_string(),
                change_type: change_type.to_string(),
                expiration_datetime: item
                    .get("expirationDateTime")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string()),
                notification_url: item
                    .get("notificationUrl")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string()),
            });
        }
    }
    Ok(out)
}

fn create_subscription(
    cfg: &ProviderConfig,
    token: &str,
    webhook_url: &str,
    spec: &SubscriptionSpec,
) -> Result<ExistingSubscription, String> {
    let graph_base = cfg
        .graph_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());
    let url = format!("{}/subscriptions", graph_base);
    let expiration = spec
        .expiration_datetime
        .clone()
        .ok_or_else(|| "desired_subscriptions.expiration_datetime required".to_string())?;

    let mut payload = json!({
        "changeType": spec.change_type,
        "notificationUrl": webhook_url,
        "resource": spec.resource,
        "expirationDateTime": expiration,
    });
    if let Some(client_state) = spec.client_state.as_ref() {
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("clientState".into(), Value::String(client_state.clone()));
    }

    let request = http_client::Request {
        method: "POST".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {}", token)),
        ],
        body: Some(serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())),
    };
    let resp = http_client::send(&request, None, None)
        .map_err(|e| format!("transport error: {}", e.message))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(format!("create subscription status {}", resp.status));
    }
    let body = resp.body.unwrap_or_default();
    let json: Value =
        serde_json::from_slice(&body).map_err(|e| format!("invalid create response: {e}"))?;
    let id = json
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "create response missing id".to_string())?;
    Ok(ExistingSubscription {
        id: id.to_string(),
        resource: spec.resource.clone(),
        change_type: spec.change_type.clone(),
        expiration_datetime: json
            .get("expirationDateTime")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .or(Some(expiration)),
        notification_url: Some(webhook_url.to_string()),
    })
}

fn renew_subscription(
    cfg: &ProviderConfig,
    token: &str,
    subscription_id: &str,
    expiration: &str,
) -> Result<(), String> {
    let graph_base = cfg
        .graph_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());
    let url = format!("{}/subscriptions/{}", graph_base, subscription_id);
    let payload = json!({ "expirationDateTime": expiration });
    let request = http_client::Request {
        method: "PATCH".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {}", token)),
        ],
        body: Some(serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())),
    };
    let resp = http_client::send(&request, None, None)
        .map_err(|e| format!("transport error: {}", e.message))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(format!("renew subscription status {}", resp.status));
    }
    Ok(())
}

fn write_state(state: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(state).map_err(|_| "invalid state payload".to_string())?;
    state_store::write(STATE_KEY, &bytes, None)
        .map_err(|e| format!("state store error: {}", e.message))
        .map(|_| ())
}

fn get_secret(key: &str) -> Result<String, String> {
    match secrets_store::get(key) {
        Ok(Some(bytes)) => String::from_utf8(bytes).map_err(|_| format!("secret {key} not utf-8")),
        Ok(None) => Err(format!("missing secret: {key}")),
        Err(e) => Err(format!("secret store error: {e:?}")),
    }
}
