//! `ChannelMessageEnvelope` constructors for inbound webchat events.
//!
//! Two variants exist:
//! - `build_webchat_envelope_with_ctx` — used by the Direct Line ingest path,
//!   carries explicit env/tenant IDs that were validated from the JWT.
//! - `build_webchat_envelope` — used by the fallback generic HTTP ingest path,
//!   defaults env/tenant to "default".

use greentic_types::{Actor, ChannelMessageEnvelope, EnvId, MessageMetadata, TenantCtx, TenantId};
use serde_json::Value;
use std::collections::BTreeMap;

/// Build envelope with explicit env/tenant context (used by Direct Line ingest).
pub(super) fn build_webchat_envelope_with_ctx(
    text: String,
    user_id: Option<String>,
    session_id: Option<String>,
    tenant_channel_id: Option<String>,
    env_id: &str,
    tenant_id: &str,
    extensions: BTreeMap<String, Value>,
) -> ChannelMessageEnvelope {
    let env =
        EnvId::try_from(env_id).unwrap_or_else(|_| EnvId::try_from("default").expect("env id"));
    let tenant = TenantId::try_from(tenant_id)
        .unwrap_or_else(|_| TenantId::try_from("default").expect("tenant id"));
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    metadata.insert("env".to_string(), env_id.to_string());
    metadata.insert("tenant".to_string(), tenant_id.to_string());
    if let Some(channel) = &tenant_channel_id {
        metadata.insert("tenant_channel_id".to_string(), channel.clone());
    }
    let channel = session_id
        .clone()
        .or_else(|| tenant_channel_id.clone())
        .unwrap_or_else(|| "webchat".to_string());
    if let Some(sid) = &session_id {
        metadata.insert("route".to_string(), sid.clone());
    }
    ChannelMessageEnvelope {
        id: format!("webchat-{channel}"),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: channel.clone(),
        session_id: channel,
        reply_scope: None,
        from: user_id.map(|id| Actor {
            id,
            kind: Some("user".into()),
        }),
        to: Vec::new(),
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
        extensions,
    }
}

pub(super) fn build_webchat_envelope(
    text: String,
    user_id: Option<String>,
    route: Option<String>,
    tenant_channel_id: Option<String>,
) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    if let Some(route) = &route {
        metadata.insert("route".to_string(), route.clone());
    }
    if let Some(channel) = &tenant_channel_id {
        metadata.insert("tenant_channel_id".to_string(), channel.clone());
    }
    let channel = route
        .clone()
        .or_else(|| tenant_channel_id.clone())
        .unwrap_or_else(|| "webchat".to_string());
    ChannelMessageEnvelope {
        id: format!("webchat-{channel}"),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: channel.clone(),
        session_id: channel,
        reply_scope: None,
        from: user_id.map(|id| Actor {
            id,
            kind: Some("user".into()),
        }),
        to: Vec::new(),
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
        extensions: Default::default(),
    }
}
