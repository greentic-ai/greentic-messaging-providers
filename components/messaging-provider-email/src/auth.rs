use super::bindings::greentic::http::client;
use super::bindings::greentic::secrets_store::secrets_store;
use super::{AuthUserRefV1, ProviderConfig};
use serde_json::Value;
use urlencoding::encode as url_encode;

const DEFAULT_GRAPH_AUTHORITY: &str = "https://login.microsoftonline.com";
const DEFAULT_GRAPH_SCOPE: &str = "https://graph.microsoft.com/.default offline_access openid";
const MS_GRAPH_CLIENT_ID_KEY: &str = "MS_GRAPH_CLIENT_ID";
const MS_GRAPH_CLIENT_SECRET_KEY: &str = "MS_GRAPH_CLIENT_SECRET";

pub(crate) fn acquire_graph_token(
    cfg: &ProviderConfig,
    user: &AuthUserRefV1,
) -> Result<String, String> {
    let refresh_token = get_secret(&user.token_key)?;
    let client_id = get_secret(MS_GRAPH_CLIENT_ID_KEY)?;
    let client_secret = get_secret(MS_GRAPH_CLIENT_SECRET_KEY)?;
    let endpoint = graph_token_endpoint(cfg, user)?;
    let scope = cfg.graph_scope.as_deref().unwrap_or(DEFAULT_GRAPH_SCOPE);
    let form = format!(
        "client_id={}&client_secret={}&grant_type=refresh_token&refresh_token={}&scope={}",
        url_encode(&client_id),
        url_encode(&client_secret),
        url_encode(&refresh_token),
        url_encode(scope)
    );
    request_token(&endpoint, form.as_bytes())
}

fn graph_token_endpoint(cfg: &ProviderConfig, user: &AuthUserRefV1) -> Result<String, String> {
    if let Some(endpoint) = cfg.graph_token_endpoint.as_ref() {
        return Ok(endpoint.clone());
    }
    let tenant = user
        .tenant_id
        .as_deref()
        .or(cfg.graph_tenant_id.as_deref())
        .ok_or_else(|| "missing Graph tenant id".to_string())?;
    let authority = cfg
        .graph_authority
        .as_deref()
        .unwrap_or(DEFAULT_GRAPH_AUTHORITY);
    Ok(format!(
        "{}/{}/oauth2/v2.0/token",
        authority.trim_end_matches('/'),
        tenant.trim_matches('/')
    ))
}

fn get_secret(key: &str) -> Result<String, String> {
    match secrets_store::get(key) {
        Ok(Some(bytes)) => String::from_utf8(bytes).map_err(|_| format!("secret {key} not utf-8")),
        Ok(None) => Err(format!("missing secret: {key}")),
        Err(err) => Err(format!("secret store error: {err:?}")),
    }
}

fn request_token(url: &str, body: &[u8]) -> Result<String, String> {
    let request = client::Request {
        method: "POST".into(),
        url: url.to_string(),
        headers: vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body: Some(body.to_vec()),
    };
    let resp = client::send(&request, None, None)
        .map_err(|e| format!("token exchange error: {}", e.message))?;
    if resp.status < 200 || resp.status >= 300 {
        return Err(format!("token endpoint returned status {}", resp.status));
    }
    let body = resp.body.unwrap_or_default();
    let parsed: Value =
        serde_json::from_slice(&body).map_err(|e| format!("invalid token response: {e}"))?;
    parsed
        .get("access_token")
        .and_then(Value::as_str)
        .map(|token| token.to_string())
        .ok_or_else(|| "token response missing access_token".to_string())
}
