use base64::{Engine as _, engine::general_purpose};
use chrono::Utc;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use urlencoding::decode;
use uuid::Uuid;

use greentic_types::messaging::universal_dto::{Header, HttpInV1, HttpOutV1};

use super::jwt::{DirectLineContext, TTL_SECONDS, issue_token, verify_token};
use super::state::{ConversationState, StoredActivity, conversation_key, sanitize_team};
use super::store::{RateLimitState, SecretStore, StateStore};

const DIRECTLINE_PREFIX: &str = "/v3/directline";
const JSON_CONTENT_TYPE: &str = "application/json";
const TOKEN_SECRET_KEY: &str = "jwt_signing_key";
const RATE_LIMIT_WINDOW_SECONDS: i64 = 60;
const RATE_LIMIT_REQUESTS: u32 = 5;
const MAX_ATTACHMENT_BYTES: usize = 512 * 1024;
const ALLOWED_ATTACHMENT_TYPES: &[&str] = &[
    "text/plain",
    "application/json",
    "image/png",
    "image/jpeg",
    "image/gif",
    "application/vnd.microsoft.card.adaptive",
    "application/vnd.microsoft.card.hero",
    "application/vnd.microsoft.card.thumbnail",
];

pub fn handle_directline_request<S, SE>(
    request: &HttpInV1,
    state_store: &mut S,
    secrets: &SE,
) -> HttpOutV1
where
    S: StateStore,
    SE: SecretStore,
{
    if !request.path.starts_with(DIRECTLINE_PREFIX) {
        return respond_not_found("missing directline prefix");
    }

    let segments = request
        .path
        .trim_start_matches('/')
        .split('/')
        .collect::<Vec<_>>();

    match segments.as_slice() {
        ["v3", "directline", "tokens", "generate"] if method_is(request, "POST") => {
            handle_tokens(request, state_store, secrets)
        }
        ["v3", "directline", "tokens", "generate"] => method_not_allowed(),
        ["v3", "directline", "conversations"] if method_is(request, "POST") => {
            handle_conversations(request, state_store, secrets)
        }
        ["v3", "directline", "conversations"] => method_not_allowed(),
        ["v3", "directline", "conversations", conv_id, "activities"] => {
            match request.method.as_str() {
                m if m.eq_ignore_ascii_case("POST") => {
                    handle_post_activities(request, state_store, secrets, conv_id)
                }
                m if m.eq_ignore_ascii_case("GET") => {
                    handle_get_activities(request, state_store, secrets, conv_id)
                }
                _ => method_not_allowed(),
            }
        }
        ["v3", "directline", "conversations", _conv_id, "stream"] => respond_not_implemented(),
        _ => respond_not_found("unknown directline endpoint"),
    }
}

fn handle_tokens<S, SE>(request: &HttpInV1, state_store: &mut S, secrets: &SE) -> HttpOutV1
where
    S: StateStore,
    SE: SecretStore,
{
    let ctx = parse_context(request.query.as_deref());
    let user_id = match decode_json_body(request).and_then(|payload| extract_user_id(&payload)) {
        Ok(Some(id)) => id,
        Ok(None) => "anonymous".to_string(),
        Err(resp) => return resp,
    };
    let now = Utc::now().timestamp();
    let rate_key = rate_limit_key(&ctx, &user_id);
    if let Err(resp) = enforce_rate_limit(state_store, &rate_key, now) {
        return resp;
    }

    let signing_key = match load_signing_key(secrets) {
        Ok(key) => key,
        Err(resp) => return resp,
    };

    match issue_token(&signing_key, ctx.clone(), &user_id, None) {
        Ok((token, _exp)) => respond_json(
            200,
            json!({
                "token": token,
                "expires_in": TTL_SECONDS,
            }),
        ),
        Err(err) => respond_error(
            500,
            "token_issue_failed",
            format!("failed to mint token: {err:?}"),
        ),
    }
}

fn handle_conversations<S, SE>(request: &HttpInV1, state_store: &mut S, secrets: &SE) -> HttpOutV1
where
    S: StateStore,
    SE: SecretStore,
{
    let authorization = match extract_bearer(request.headers.as_slice()) {
        Some(header) => header,
        None => return respond_unauthorized("missing Authorization header"),
    };
    let signing_key = match load_signing_key(secrets) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let claims = match verify_token(&signing_key, &authorization) {
        Ok(claims) => claims,
        Err(err) => return respond_unauthorized(&format!("invalid token: {err:?}")),
    };

    if claims.conv.is_some() {
        return respond_forbidden("token already bound to a conversation");
    }

    let ctx = claims.ctx.clone();
    let conversation_id = Uuid::new_v4().to_string();
    let key = conversation_key(&ctx, &conversation_id);
    let conversation = ConversationState::new(ctx.clone());

    if let Err(resp) = write_conversation_state(state_store, &key, &conversation) {
        return resp;
    }

    let (token, _exp) = match issue_token(
        &signing_key,
        ctx.clone(),
        &claims.sub,
        Some(conversation_id.clone()),
    ) {
        Ok(pair) => pair,
        Err(err) => {
            return respond_error(
                500,
                "token_issue_failed",
                format!("failed to mint conversation token: {err:?}"),
            );
        }
    };

    respond_json(
        201,
        json!({
            "conversationId": conversation_id,
            "token": token,
            "expires_in": TTL_SECONDS,
            "streamUrl": Value::Null,
        }),
    )
}

fn handle_post_activities<S, SE>(
    request: &HttpInV1,
    state_store: &mut S,
    secrets: &SE,
    conversation_id: &str,
) -> HttpOutV1
where
    S: StateStore,
    SE: SecretStore,
{
    let authorization = match extract_bearer(request.headers.as_slice()) {
        Some(token) => token,
        None => return respond_unauthorized("missing Authorization header"),
    };
    let signing_key = match load_signing_key(secrets) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let claims = match verify_token(&signing_key, &authorization) {
        Ok(claims) => claims,
        Err(err) => return respond_unauthorized(&format!("invalid token: {err:?}")),
    };

    if claims.conv.as_deref() != Some(conversation_id) {
        return respond_forbidden("token bound to different conversation");
    }

    let conv_key = conversation_key(&claims.ctx, conversation_id);
    let mut conversation = match load_conversation_state(state_store, &conv_key) {
        Ok(state) => state,
        Err(resp) => return resp,
    };

    if conversation.ctx != claims.ctx {
        return respond_forbidden("token context mismatch");
    }

    let watermark = conversation.bump_watermark();
    let body = match decode_json_body(request) {
        Ok(value) => value,
        Err(resp) => return resp,
    };

    if let Err(resp) = validate_attachments(&body) {
        return resp;
    }

    let activity = StoredActivity {
        id: Uuid::new_v4().to_string(),
        type_: body
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("message")
            .to_string(),
        text: body
            .get("text")
            .and_then(|value| value.as_str())
            .map(|s| s.to_string()),
        from: body
            .get("from")
            .and_then(|from| from.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        timestamp: Utc::now().timestamp_millis(),
        watermark,
        raw: body.clone(),
    };

    conversation.activities.push(activity.clone());

    if let Err(resp) = write_conversation_state(state_store, &conv_key, &conversation) {
        return resp;
    }

    respond_json(201, json!({"id": activity.id}))
}

fn handle_get_activities<S, SE>(
    request: &HttpInV1,
    state_store: &mut S,
    secrets: &SE,
    conversation_id: &str,
) -> HttpOutV1
where
    S: StateStore,
    SE: SecretStore,
{
    let authorization = match extract_bearer(request.headers.as_slice()) {
        Some(token) => token,
        None => return respond_unauthorized("missing Authorization header"),
    };
    let signing_key = match load_signing_key(secrets) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let claims = match verify_token(&signing_key, &authorization) {
        Ok(claims) => claims,
        Err(err) => return respond_unauthorized(&format!("invalid token: {err:?}")),
    };

    if claims.conv.as_deref() != Some(conversation_id) {
        return respond_forbidden("token bound to different conversation");
    }

    let conv_key = conversation_key(&claims.ctx, conversation_id);
    let conversation = match load_conversation_state(state_store, &conv_key) {
        Ok(state) => state,
        Err(resp) => return resp,
    };

    if conversation.ctx != claims.ctx {
        return respond_forbidden("token context mismatch");
    }

    let watermark = match parse_watermark(request.query.as_deref()) {
        Ok(value) => value,
        Err(resp) => return resp,
    };

    let activities = conversation
        .activities
        .iter()
        .filter(|activity| match watermark {
            Some(watermark) => activity.watermark > watermark,
            None => true,
        })
        .map(activity_to_value)
        .collect::<Vec<_>>();

    respond_json(
        200,
        json!({
            "activities": activities,
            "watermark": conversation.next_watermark.to_string(),
        }),
    )
}

fn enforce_rate_limit<S: StateStore>(store: &mut S, key: &str, now: i64) -> Result<(), HttpOutV1> {
    let mut state = match read_rate_limit_state(store, key) {
        Ok(Some(state)) => state,
        Ok(None) => RateLimitState::new(now),
        Err(resp) => return Err(resp),
    };

    if state
        .bump(now, RATE_LIMIT_WINDOW_SECONDS, RATE_LIMIT_REQUESTS)
        .is_err()
    {
        return Err(respond_error(
            429,
            "rate_limited",
            "token rate limit exceeded",
        ));
    }

    let bytes = match serde_json::to_vec(&state) {
        Ok(bytes) => bytes,
        Err(err) => return Err(respond_error(500, "state_serialize", err.to_string())),
    };

    store
        .write(key, &bytes)
        .map_err(|err| respond_error(500, "state_write", err))
}

fn read_rate_limit_state<S: StateStore>(
    store: &mut S,
    key: &str,
) -> Result<Option<RateLimitState>, HttpOutV1> {
    match store.read(key) {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| respond_error(500, "state_parse", err.to_string())),
        Ok(None) => Ok(None),
        Err(err) => Err(respond_error(500, "state_read", err)),
    }
}

fn write_conversation_state<S: StateStore>(
    store: &mut S,
    key: &str,
    state: &ConversationState,
) -> Result<(), HttpOutV1> {
    let bytes = serde_json::to_vec(state)
        .map_err(|err| respond_error(500, "state_serialize", err.to_string()))?;
    store
        .write(key, &bytes)
        .map_err(|err| respond_error(500, "state_write", err))
}

fn load_conversation_state<S: StateStore>(
    store: &mut S,
    key: &str,
) -> Result<ConversationState, HttpOutV1> {
    match store.read(key) {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes)
            .map_err(|err| respond_error(500, "state_parse", err.to_string())),
        Ok(None) => Err(respond_not_found("conversation not found")),
        Err(err) => Err(respond_error(500, "state_read", err)),
    }
}

fn activity_to_value(activity: &StoredActivity) -> Value {
    let mut map = match activity.raw.clone() {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("data".to_string(), other);
            map
        }
    };
    map.insert("id".to_string(), Value::String(activity.id.clone()));
    map.insert("type".to_string(), Value::String(activity.type_.clone()));
    map.insert(
        "timestamp".to_string(),
        Value::String(activity.timestamp.to_string()),
    );
    map.insert(
        "watermark".to_string(),
        Value::String(activity.watermark.to_string()),
    );
    if let Some(text) = &activity.text {
        map.insert("text".to_string(), Value::String(text.clone()));
    }
    if let Some(from) = &activity.from {
        let mut from_map = Map::new();
        from_map.insert("id".to_string(), Value::String(from.clone()));
        map.insert("from".to_string(), Value::Object(from_map));
    }
    Value::Object(map)
}

fn validate_attachments(body: &Value) -> Result<(), HttpOutV1> {
    let attachments = match body.get("attachments") {
        Some(Value::Array(items)) => items,
        _ => return Ok(()),
    };

    for attachment in attachments {
        let content_type = attachment
            .get("contentType")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if !ALLOWED_ATTACHMENT_TYPES.contains(&content_type) {
            return Err(respond_bad_request(&format!(
                "unsupported content type: {content_type}"
            )));
        }

        if let Some(content) = attachment.get("content")
            && let Some(text) = content.as_str()
            && text.len() > MAX_ATTACHMENT_BYTES
        {
            return Err(respond_bad_request("attachment too large"));
        }
    }

    Ok(())
}

fn parse_watermark(query: Option<&str>) -> Result<Option<u64>, HttpOutV1> {
    let params = parse_query(query);
    if let Some(value) = params.get("watermark") {
        value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| respond_bad_request("watermark must be a number"))
    } else {
        Ok(None)
    }
}

fn rate_limit_key(ctx: &DirectLineContext, user_id: &str) -> String {
    format!(
        "webchat:rate:tokens:{}:{}:{}:{}",
        ctx.env,
        ctx.tenant,
        sanitize_team(ctx.team.as_deref()),
        user_id
    )
}

fn load_signing_key<SE: SecretStore>(secrets: &SE) -> Result<Vec<u8>, HttpOutV1> {
    match secrets.get(TOKEN_SECRET_KEY) {
        Ok(Some(bytes)) if !bytes.is_empty() => Ok(bytes),
        Ok(Some(_)) => Err(respond_error(500, "invalid_secret", "signing key is empty")),
        Ok(None) => Err(respond_error(
            500,
            "missing_secret",
            format!("secret {TOKEN_SECRET_KEY} not found"),
        )),
        Err(err) => Err(respond_error(500, "secret_error", err)),
    }
}

fn parse_context(query: Option<&str>) -> DirectLineContext {
    let params = parse_query(query);
    let env = params
        .get("env")
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| "default".to_string());
    let tenant = params
        .get("tenant")
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| "default".to_string());
    let team = params.get("team").and_then(|team| {
        let trimmed = team.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    DirectLineContext { env, tenant, team }
}

fn parse_query(query: Option<&str>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(query) = query {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let mut split = pair.splitn(2, '=');
            let key = split.next().unwrap_or_default();
            let value = split.next().unwrap_or_default();
            if let (Ok(key), Ok(value)) = (decode(key), decode(value)) {
                map.insert(key.into_owned(), value.into_owned());
            }
        }
    }
    map
}

fn decode_json_body(request: &HttpInV1) -> Result<Value, HttpOutV1> {
    if request.body_b64.trim().is_empty() {
        return Ok(Value::Null);
    }
    let bytes = match general_purpose::STANDARD.decode(&request.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => {
            return Err(respond_bad_request(&format!(
                "invalid body encoding: {err}"
            )));
        }
    };
    serde_json::from_slice(&bytes)
        .map_err(|err| respond_bad_request(&format!("invalid json payload: {err}")))
}

fn extract_user_id(body: &Value) -> Result<Option<String>, HttpOutV1> {
    if let Some(value) = body.get("user")
        && let Some(id) = value.get("id").and_then(|v| v.as_str())
    {
        return Ok(Some(id.to_string()));
    }
    Ok(None)
}

fn extract_bearer(headers: &[Header]) -> Option<String> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("Authorization"))
        .and_then(|header| {
            let value = header.value.trim();
            let mut parts = value.splitn(2, ' ');
            let scheme = parts.next().unwrap_or_default();
            if scheme.eq_ignore_ascii_case("bearer") {
                Some(parts.next().unwrap_or_default().trim().to_string())
            } else {
                None
            }
        })
}

fn method_is(request: &HttpInV1, method: &str) -> bool {
    request.method.eq_ignore_ascii_case(method)
}

fn json_headers() -> Vec<Header> {
    vec![Header {
        name: "Content-Type".to_string(),
        value: JSON_CONTENT_TYPE.to_string(),
    }]
}

fn respond_json(status: u16, payload: Value) -> HttpOutV1 {
    let body = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
    HttpOutV1 {
        status,
        headers: json_headers(),
        body_b64: general_purpose::STANDARD.encode(&body),
        events: Vec::new(),
    }
}

fn respond_error(status: u16, error: &str, message: impl Into<String>) -> HttpOutV1 {
    respond_json(
        status,
        json!({
            "error": error,
            "message": message.into(),
        }),
    )
}

fn respond_bad_request(message: &str) -> HttpOutV1 {
    respond_error(400, "bad_request", message)
}

fn respond_not_found(message: &str) -> HttpOutV1 {
    respond_error(404, "not_found", message)
}

fn method_not_allowed() -> HttpOutV1 {
    respond_error(
        405,
        "method_not_allowed",
        "method not allowed on this endpoint",
    )
}

fn respond_not_implemented() -> HttpOutV1 {
    respond_error(501, "not_implemented", "streaming not supported")
}

fn respond_unauthorized(message: &str) -> HttpOutV1 {
    respond_error(401, "unauthorized", message)
}

fn respond_forbidden(message: &str) -> HttpOutV1 {
    respond_error(403, "forbidden", message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose;
    use serde_json::json;
    use std::collections::HashMap;

    struct InMemoryStateStore {
        data: HashMap<String, Vec<u8>>,
    }

    impl InMemoryStateStore {
        fn new() -> Self {
            Self {
                data: HashMap::new(),
            }
        }
    }

    impl StateStore for InMemoryStateStore {
        fn read(&mut self, key: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.data.get(key).cloned())
        }

        fn write(&mut self, key: &str, value: &[u8]) -> Result<(), String> {
            self.data.insert(key.to_string(), value.to_vec());
            Ok(())
        }
    }

    struct TestSecretStore {
        data: HashMap<String, Vec<u8>>,
    }

    impl TestSecretStore {
        fn new() -> Self {
            Self {
                data: HashMap::new(),
            }
        }

        fn insert(&mut self, key: &str, value: &[u8]) {
            self.data.insert(key.to_string(), value.to_vec());
        }
    }

    impl SecretStore for TestSecretStore {
        fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.data.get(key).cloned())
        }
    }

    fn build_request(
        method: &str,
        path: &str,
        query: Option<&str>,
        body: Option<&Value>,
        headers: Vec<Header>,
    ) -> HttpInV1 {
        let body_b64 = match body {
            Some(payload) => general_purpose::STANDARD.encode(serde_json::to_vec(payload).unwrap()),
            None => String::new(),
        };
        HttpInV1 {
            method: method.to_string(),
            path: path.to_string(),
            query: query.map(|value| value.to_string()),
            headers,
            body_b64,
            route_hint: None,
            binding_id: None,
            config: None,
        }
    }

    fn decode_body(response: &HttpOutV1) -> Value {
        let bytes = general_purpose::STANDARD
            .decode(&response.body_b64)
            .expect("base64 decode");
        serde_json::from_slice(&bytes).expect("valid json")
    }

    #[test]
    fn directline_polling_flow() {
        let mut state = InMemoryStateStore::new();
        let mut secrets = TestSecretStore::new();
        secrets.insert(TOKEN_SECRET_KEY, b"test-secret");

        let token_request = build_request(
            "POST",
            "/v3/directline/tokens/generate",
            Some("env=default&tenant=default"),
            Some(&json!({"user": {"id": "alice"}})),
            vec![],
        );
        let token_response = handle_directline_request(&token_request, &mut state, &secrets);
        assert_eq!(token_response.status, 200);
        let token_body = decode_body(&token_response);
        let user_token = token_body["token"].as_str().expect("token returned");

        let conversation_request = build_request(
            "POST",
            "/v3/directline/conversations",
            None,
            None,
            vec![Header {
                name: "Authorization".into(),
                value: format!("Bearer {user_token}"),
            }],
        );
        let conversation_response =
            handle_directline_request(&conversation_request, &mut state, &secrets);
        assert_eq!(conversation_response.status, 201);
        let conversation_body = decode_body(&conversation_response);
        let conversation_id = conversation_body["conversationId"]
            .as_str()
            .expect("conversation id");
        let conv_token = conversation_body["token"]
            .as_str()
            .expect("conversation token");

        let reuse_response = handle_directline_request(
            &build_request(
                "POST",
                "/v3/directline/conversations",
                None,
                None,
                vec![Header {
                    name: "Authorization".into(),
                    value: format!("Bearer {conv_token}"),
                }],
            ),
            &mut state,
            &secrets,
        );
        assert_eq!(reuse_response.status, 403);

        let activity = json!({
            "type": "message",
            "text": "hello",
            "from": {"id": "alice"},
        });
        let post_activity_response = handle_directline_request(
            &build_request(
                "POST",
                &format!("/v3/directline/conversations/{conversation_id}/activities"),
                None,
                Some(&activity),
                vec![Header {
                    name: "Authorization".into(),
                    value: format!("Bearer {conv_token}"),
                }],
            ),
            &mut state,
            &secrets,
        );
        assert_eq!(post_activity_response.status, 201);
        let posted = decode_body(&post_activity_response);
        assert!(posted.get("id").is_some());

        let get_response = handle_directline_request(
            &build_request(
                "GET",
                &format!("/v3/directline/conversations/{conversation_id}/activities"),
                None,
                None,
                vec![Header {
                    name: "Authorization".into(),
                    value: format!("Bearer {conv_token}"),
                }],
            ),
            &mut state,
            &secrets,
        );
        assert_eq!(get_response.status, 200);
        let get_body = decode_body(&get_response);
        let activities = get_body["activities"].as_array().unwrap();
        assert_eq!(activities.len(), 1);
        assert_eq!(get_body["watermark"], Value::String("1".to_string()));

        let empty_response = handle_directline_request(
            &build_request(
                "GET",
                &format!("/v3/directline/conversations/{conversation_id}/activities"),
                Some("watermark=1"),
                None,
                vec![Header {
                    name: "Authorization".into(),
                    value: format!("Bearer {conv_token}"),
                }],
            ),
            &mut state,
            &secrets,
        );
        assert_eq!(empty_response.status, 200);
        let empty_body = decode_body(&empty_response);
        assert!(empty_body["activities"].as_array().unwrap().is_empty());
        assert_eq!(empty_body["watermark"], Value::String("1".to_string()));

        let wrong_conv_response = handle_directline_request(
            &build_request(
                "POST",
                "/v3/directline/conversations/other/activities",
                None,
                Some(&activity),
                vec![Header {
                    name: "Authorization".into(),
                    value: format!("Bearer {conv_token}"),
                }],
            ),
            &mut state,
            &secrets,
        );
        assert_eq!(wrong_conv_response.status, 403);
    }
}
