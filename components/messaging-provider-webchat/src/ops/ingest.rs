//! Inbound HTTP / ingress path for the webchat provider.
//!
//! Two entry points:
//! - `handle_ingest` — legacy raw JSON ingest (used by the `ingest` op).
//! - `ingest_http`   — the main HTTP ingress router: dispatches OAuth,
//!   Direct Line (`/v3/directline/*`, `/token` shorthand), and a generic
//!   webhook fallback. This is where conversation/activity envelopes are
//!   emitted back to the operator so flows can auto-start.

use base64::{Engine as _, engine::general_purpose};
use greentic_types::messaging::universal_dto::{Header, HttpInV1, HttpOutV1};
use provider_common::helpers::json_bytes;
use provider_common::http_compat::{http_out_error, http_out_v1_bytes, parse_operator_http_in};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::directline::{ConfigAwareSecretStore, HostStateStore, handle_directline_request};

use super::envelope::{build_webchat_envelope, build_webchat_envelope_with_ctx};
use super::helpers::{
    decode_body_json, extract_activity_text, extract_text, non_empty_string, route_from_value,
    tenant_channel_from_value, user_from_value,
};
use super::oauth::{handle_auth_config, handle_oauth_token_exchange};

pub(crate) fn handle_ingest(input_json: &[u8]) -> Vec<u8> {
    let parsed: Value = match serde_json::from_slice(input_json) {
        Ok(val) => val,
        Err(err) => {
            return json_bytes(&json!({"ok": false, "error": format!("invalid json: {err}")}));
        }
    };
    let text = parsed
        .get("text")
        .or_else(|| parsed.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let user = parsed
        .get("user_id")
        .or_else(|| parsed.get("from"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let envelope = json!({
        "from": user,
        "text": text,
        "raw": parsed,
    });
    json_bytes(&json!({"ok": true, "envelope": envelope}))
}

pub(crate) fn ingest_http(input_json: &[u8]) -> Vec<u8> {
    // Try native greentic-types format first, fall back to operator format
    let request = match serde_json::from_slice::<HttpInV1>(input_json) {
        Ok(req) => req,
        Err(_) => match parse_operator_http_in(input_json) {
            Ok(req) => req,
            Err(err) => return http_out_error(400, &format!("invalid http input: {err}")),
        },
    };
    // Serve OAuth configuration for the frontend SPA.
    if request.path.ends_with("/auth/config") && request.method.eq_ignore_ascii_case("GET") {
        return handle_auth_config(&request);
    }
    // Server-side OAuth token exchange — keeps client_secret off the browser.
    if request.path.ends_with("/oauth/token-exchange")
        && request.method.eq_ignore_ascii_case("POST")
    {
        return handle_oauth_token_exchange(&request);
    }

    // Translate shorthand /token path to the canonical DirectLine token endpoint.
    // The operator routes /v1/messaging/webchat/{tenant}/token through generic ingress,
    // so the provider component must recognize and forward it.
    if request.path.ends_with("/token") && !request.path.contains("/v3/directline") {
        let token_request = HttpInV1 {
            method: "POST".to_string(),
            path: "/v3/directline/tokens/generate".to_string(),
            query: request.query.clone(),
            headers: request.headers.clone(),
            body_b64: request.body_b64.clone(),
            route_hint: request.route_hint.clone(),
            binding_id: request.binding_id.clone(),
            config: request.config.clone(),
        };
        let mut state_driver = HostStateStore;
        let secrets_driver = ConfigAwareSecretStore::new(request.config.clone());
        let out = handle_directline_request(&token_request, &mut state_driver, &secrets_driver);
        return http_out_v1_bytes(&out);
    }

    // Extract Direct Line sub-path from operator-prefixed or direct paths.
    // Operator forwards full URI like /messaging/ingress/webchat/default/_/v3/directline/...
    if let Some(offset) = request.path.find("/v3/directline") {
        return handle_directline_path(&request, offset);
    }

    // Generic webhook fallback: normalise the raw body + emit an envelope.
    let body_bytes = match general_purpose::STANDARD.decode(&request.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return http_out_error(400, &format!("invalid body encoding: {err}")),
    };
    let body_val: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let text = extract_text(&body_val);
    let user = user_from_value(&body_val);
    let route =
        non_empty_string(request.route_hint.as_deref()).or_else(|| route_from_value(&body_val));
    let tenant_channel_id = tenant_channel_from_value(&body_val);
    let envelope = build_webchat_envelope(
        text.clone(),
        user.clone(),
        route.clone(),
        tenant_channel_id.clone(),
    );
    let normalized = json!({
        "ok": true,
        "event": body_val,
        "route": route,
        "tenant_channel_id": tenant_channel_id,
    });
    let normalized_bytes = serde_json::to_vec(&normalized).unwrap_or_else(|_| b"{}".to_vec());
    let out = HttpOutV1 {
        status: 200,
        headers: Vec::new(),
        body_b64: general_purpose::STANDARD.encode(&normalized_bytes),
        events: vec![envelope],
    };
    http_out_v1_bytes(&out)
}

/// Handle a Direct Line sub-request (anything under `/v3/directline/...`).
///
/// Delegates to the directline crate for the actual protocol response, then
/// post-processes to emit `ChannelMessageEnvelope` events for `POST conversations`
/// (auto-start) and `POST activities` (forward user messages into the flow engine).
fn handle_directline_path(request: &HttpInV1, offset: usize) -> Vec<u8> {
    let dl_path = &request.path[offset..];
    let dl_request = HttpInV1 {
        method: request.method.clone(),
        path: dl_path.to_string(),
        query: request.query.clone(),
        headers: request.headers.clone(),
        body_b64: request.body_b64.clone(),
        route_hint: request.route_hint.clone(),
        binding_id: request.binding_id.clone(),
        config: request.config.clone(),
    };
    let mut state_driver = HostStateStore;
    // Use ConfigAwareSecretStore to check injected secrets from request config first,
    // falling back to host secrets_store interface if not found.
    let secrets_driver = ConfigAwareSecretStore::new(request.config.clone());
    let mut out = handle_directline_request(&dl_request, &mut state_driver, &secrets_driver);

    // Emit ChannelMessageEnvelope for POST /conversations so the operator can
    // auto-start the default flow when a new conversation is created.
    // The welcome experience is driven entirely by the flow — the JS-side
    // welcome overlay has been removed in favour of this server-side trigger.
    if request.method.eq_ignore_ascii_case("POST")
        && dl_path == "/v3/directline/conversations"
        && out.status == 201
    {
        let (env_id, tenant_id) = extract_context_from_response_headers(&out.headers)
            .unwrap_or_else(|| ("default".to_string(), "default".to_string()));
        let user_id = out
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("X-Greentic-User"))
            .map(|h| h.value.clone());
        let conv_id = out
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("X-Greentic-ConversationId"))
            .map(|h| h.value.clone());
        let mut envelope = build_webchat_envelope_with_ctx(
            String::new(),
            user_id,
            conv_id,
            None,
            &env_id,
            &tenant_id,
            BTreeMap::new(),
        );
        envelope
            .metadata
            .insert("autoStart".to_string(), "true".to_string());
        out.events.push(envelope);
    }

    // Emit ChannelMessageEnvelope for POST /activities so the operator can
    // forward user messages to the flow engine.
    if request.method.eq_ignore_ascii_case("POST")
        && dl_path.contains("/activities")
        && out.status == 201
    {
        let body = decode_body_json(&request.body_b64).unwrap_or(Value::Null);
        let text = extract_activity_text(&body);
        let action_value = body.get("value"); // Action.Submit data from AC buttons
        let user = body
            .get("from")
            .and_then(|f| f.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let conv_id = dl_path
            .strip_prefix("/v3/directline/conversations/")
            .and_then(|rest| rest.split('/').next())
            .map(|s| s.to_string());
        // Extract env and tenant from response headers (set by handle_post_activities)
        // This ensures we use the same context that was validated from the JWT
        let (env_id, tenant_id) = extract_context_from_response_headers(&out.headers)
            .unwrap_or_else(|| ("default".to_string(), "default".to_string()));
        // For Action.Submit, derive a text label from the action data.
        let effective_text = if text.is_empty() {
            action_value
                .and_then(|v| {
                    v.get("step")
                        .or(v.get("routeToCardId"))
                        .or(v.get("toCardId"))
                        .or(v.get("mcp_wizard"))
                        .or(v.get("mcp_operation"))
                })
                .and_then(|s| s.as_str())
                .unwrap_or("message")
                .to_string()
        } else {
            text
        };
        let extensions = collect_directline_extensions(&body);
        let mut envelope = build_webchat_envelope_with_ctx(
            effective_text,
            user,
            conv_id.clone(),
            None,
            &env_id,
            &tenant_id,
            extensions,
        );
        // Forward locale from the activity so the runner can resolve
        // i18n translations in card responses.
        if let Some(locale) = body.get("locale").and_then(Value::as_str)
            && !locale.is_empty()
        {
            envelope
                .metadata
                .insert("locale".to_string(), locale.to_string());
        }
        // Forward ALL Action.Submit data fields to metadata so the
        // operator can handle MCP actions, token saves, card routing, etc.
        if let Some(val) = action_value
            && let Some(obj) = val.as_object()
        {
            for (k, v) in obj {
                let s = match v {
                    Value::String(s) => s.clone(),
                    _ => v.to_string(),
                };
                envelope.metadata.insert(k.clone(), s);
            }
        }
        out.events.push(envelope);
    }

    http_out_v1_bytes(&out)
}

/// Extract env and tenant from X-Greentic-Env and X-Greentic-Tenant response headers.
/// These headers are set by handle_post_activities after JWT validation.
fn extract_context_from_response_headers(headers: &[Header]) -> Option<(String, String)> {
    let env = headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("X-Greentic-Env"))
        .map(|h| h.value.clone())?;
    let tenant = headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("X-Greentic-Tenant"))
        .map(|h| h.value.clone())?;
    Some((env, tenant))
}

/// Collect DirectLine-native fields from an activity body into a typed
/// extensions map. Only fields that are present (and non-null) are inserted.
fn collect_directline_extensions(body: &Value) -> BTreeMap<String, Value> {
    let mut ext = BTreeMap::new();
    let passthroughs: &[(&str, &str)] = &[
        ("attachments", "attachments"),
        ("channelData", "channel_data"),
        ("entities", "entities"),
        ("name", "name"),
        ("inputHint", "input_hint"),
        ("speak", "speak"),
        ("suggestedActions", "suggested_actions"),
    ];
    for (src_key, ext_key) in passthroughs {
        if let Some(value) = body.get(*src_key)
            && !value.is_null()
        {
            ext.insert(ext_key.to_string(), value.clone());
        }
    }
    ext
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collect_directline_extensions_preserves_all_known_fields() {
        let body = json!({
            "type": "message",
            "text": "hi",
            "from": {"id": "user-1"},
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": {"type": "AdaptiveCard", "body": []},
            }],
            "channelData": {"webchat": {"feature": "x"}, "rag": {"citations": [{"id": "c1"}]}},
            "entities": [{"type": "mention", "text": "@bot"}],
            "name": "event/custom",
            "inputHint": "acceptingInput",
            "speak": "hello there",
            "suggestedActions": {"actions": [{"type": "imBack", "title": "Yes", "value": "yes"}]},
        });

        let ext = collect_directline_extensions(&body);

        assert_eq!(
            ext.get("attachments"),
            Some(&json!([{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": {"type": "AdaptiveCard", "body": []},
            }]))
        );
        assert_eq!(
            ext.get("channel_data"),
            Some(&json!({"webchat": {"feature": "x"}, "rag": {"citations": [{"id": "c1"}]}}))
        );
        assert_eq!(
            ext.get("entities"),
            Some(&json!([{"type": "mention", "text": "@bot"}]))
        );
        assert_eq!(ext.get("name"), Some(&json!("event/custom")));
        assert_eq!(ext.get("input_hint"), Some(&json!("acceptingInput")));
        assert_eq!(ext.get("speak"), Some(&json!("hello there")));
        assert_eq!(
            ext.get("suggested_actions"),
            Some(&json!({"actions": [{"type": "imBack", "title": "Yes", "value": "yes"}]}))
        );
    }

    #[test]
    fn collect_directline_extensions_omits_missing_fields() {
        let body = json!({"type": "message", "text": "hi"});
        let ext = collect_directline_extensions(&body);
        assert!(ext.is_empty());
    }

    #[test]
    fn collect_directline_extensions_omits_null_fields() {
        let body = json!({
            "type": "message",
            "attachments": null,
            "channelData": null,
        });
        let ext = collect_directline_extensions(&body);
        assert!(ext.is_empty());
    }

    #[test]
    fn collect_directline_extensions_preserves_rag_citations_via_channel_data() {
        // Scenario from RAG component: citations smuggled in channelData.rag
        // (replacement for GTRC_RAG_BEGIN/END sentinel workaround).
        let body = json!({
            "type": "message",
            "text": "Based on your docs...",
            "from": {"id": "bot"},
            "channelData": {
                "rag": {
                    "citations": [
                        {"id": "c1", "source": "docs/x.md", "snippet": "..."},
                        {"id": "c2", "source": "docs/y.md", "snippet": "..."}
                    ]
                }
            }
        });

        let ext = collect_directline_extensions(&body);
        let channel_data = ext.get("channel_data").expect("channel_data forwarded");
        let citations = channel_data
            .pointer("/rag/citations")
            .and_then(|v| v.as_array())
            .expect("citations preserved inside channel_data");
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0]["id"], "c1");
    }
}
