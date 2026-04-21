use base64::{Engine, engine::general_purpose::STANDARD};
use greentic_types::ChannelMessageEnvelope;
use provider_common::{RenderPlan, RenderTier};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderPayload {
    pub content_type: String,
    pub body_b64: String,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderWarning {
    pub code: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub details: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncodeResult {
    pub ok: bool,
    #[serde(default)]
    pub payload: Option<ProviderPayload>,
    #[serde(default)]
    pub warnings: Vec<RenderWarning>,
    #[serde(default)]
    pub error: Option<String>,
}

impl EncodeResult {
    fn success(payload: ProviderPayload, warnings: Vec<RenderWarning>) -> Self {
        Self {
            ok: true,
            payload: Some(payload),
            warnings,
            error: None,
        }
    }

    fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            payload: None,
            warnings: Vec::new(),
            error: Some(error.into()),
        }
    }
}

pub fn encode_from_render_plan(
    render_plan_json: &str,
    envelope: &ChannelMessageEnvelope,
    provider_hint: Option<&str>,
) -> EncodeResult {
    match serde_json::from_str::<Value>(render_plan_json) {
        Ok(mut value) => {
            normalize_tier_string(&mut value);
            match serde_json::from_value::<RenderPlan>(value) {
                Ok(plan) => build_encode_result(plan, envelope, provider_hint),
                Err(err) => EncodeResult::failure(format!("render_plan json invalid: {err}")),
            }
        }
        Err(err) => EncodeResult::failure(format!("render_plan json invalid: {err}")),
    }
}

fn build_encode_result(
    plan: RenderPlan,
    envelope: &ChannelMessageEnvelope,
    provider_hint: Option<&str>,
) -> EncodeResult {
    let mut warnings = plan
        .warnings
        .iter()
        .cloned()
        .map(convert_warning)
        .collect::<Vec<_>>();
    if matches!(
        plan.tier,
        RenderTier::TierA | RenderTier::TierB | RenderTier::TierC
    ) {
        warnings.push(RenderWarning {
            code: "passthrough_no_downsample".to_string(),
            message: Some(format!(
                "provider {} returned {} plan, pass-through enforced",
                provider_hint.unwrap_or("<unknown>"),
                tier_label(plan.tier)
            )),
            details: Some(json!({ "tier": tier_label(plan.tier) })),
        });
    }

    let payload = if let Some((bytes, metadata)) = extract_plan_body(&plan) {
        ProviderPayload {
            content_type: "application/json".to_string(),
            body_b64: STANDARD.encode(bytes),
            metadata,
        }
    } else {
        let prepared = prepare_envelope(
            envelope,
            plan.tier,
            plan.summary_text.as_deref(),
            provider_hint,
        );
        let bytes = envelope_body_bytes(&prepared);
        ProviderPayload {
            content_type: "application/json".to_string(),
            body_b64: STANDARD.encode(bytes),
            metadata: Map::new(),
        }
    };

    EncodeResult::success(payload, warnings)
}

fn convert_warning(warning: provider_common::RenderWarning) -> RenderWarning {
    RenderWarning {
        code: warning.code,
        message: warning.message,
        details: warning.path.map(|path| json!({"path": path})),
    }
}

fn extract_plan_body(plan: &RenderPlan) -> Option<(Vec<u8>, Map<String, Value>)> {
    let debug = plan.debug.as_ref()?;
    let fields = debug.as_object()?;
    let metadata = fields.clone();

    if let Some(Value::String(encoded)) = fields.get("body_b64")
        && let Ok(bytes) = STANDARD.decode(encoded)
    {
        return Some((bytes, metadata));
    }

    for key in ["payload", "body", "envelope"] {
        if let Some(value) = fields.get(key)
            && let Ok(bytes) = serde_json::to_vec(&json!({ key: value }))
        {
            return Some((bytes, metadata));
        }
    }

    None
}

fn normalize_tier_string(value: &mut Value) {
    if let Some(map) = value.as_object_mut() {
        if let Some(tier) = map.get_mut("tier") {
            normalize_tier_value(tier);
        }
        if let Some(plan) = map.get_mut("plan") {
            normalize_tier_string(plan);
        }
    }
}

fn normalize_tier_value(value: &mut Value) {
    if let Some(tier_str) = value.as_str() {
        let lower = tier_str.to_ascii_lowercase();
        let normalized = match lower.as_str() {
            "tier_a" | "tier-a" | "tiera" => "tier_a",
            "tier_b" | "tier-b" | "tierb" => "tier_b",
            "tier_c" | "tier-c" | "tierc" => "tier_c",
            "tier_d" | "tier-d" | "tierd" => "tier_d",
            other => other,
        };
        *value = Value::String(normalized.to_string());
    }
}

fn tier_label(tier: RenderTier) -> &'static str {
    match tier {
        RenderTier::TierA => "TierA",
        RenderTier::TierB => "TierB",
        RenderTier::TierC => "TierC",
        RenderTier::TierD => "TierD",
    }
}

fn envelope_body_bytes(envelope: &ChannelMessageEnvelope) -> Vec<u8> {
    serde_json::to_vec(envelope).unwrap_or_else(|_| b"{}".to_vec())
}

fn prepare_envelope(
    envelope: &ChannelMessageEnvelope,
    tier: RenderTier,
    summary: Option<&str>,
    provider_hint: Option<&str>,
) -> ChannelMessageEnvelope {
    if tier == RenderTier::TierD {
        let mut trimmed = envelope.clone();
        let has_text = trimmed
            .text
            .as_ref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
        if !has_text {
            trimmed.text = Some(
                summary
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| default_summary(provider_hint)),
            );
        }
        trimmed.metadata.remove("adaptive_card");
        trimmed
    } else {
        envelope.clone()
    }
}

fn default_summary(provider_hint: Option<&str>) -> String {
    provider_hint
        .map(|hint| format!("{hint} universal payload"))
        .unwrap_or_else(|| "universal payload".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use greentic_types::{EnvId, MessageMetadata, TenantCtx, TenantId};
    use insta::assert_json_snapshot;
    use serde_json::json;

    fn envelope_with_text(text: Option<&str>) -> ChannelMessageEnvelope {
        let env = EnvId::try_from("planned-env").expect("env id");
        let tenant = TenantId::try_from("planned-tenant").expect("tenant id");
        let mut metadata = MessageMetadata::new();
        metadata.insert("source".to_string(), "planned-test".to_string());
        ChannelMessageEnvelope {
            id: "planned-envelope".to_string(),
            tenant: TenantCtx::new(env, tenant),
            channel: "planned".to_string(),
            session_id: "planned-session".to_string(),
            reply_scope: None,
            from: None,
            to: Vec::new(),
            correlation_id: None,
            text: text.map(|value| value.to_string()),
            attachments: Vec::new(),
            metadata,
            extensions: Default::default(),
        }
    }

    fn plan_json(tier: &str, debug: Option<Value>) -> String {
        json!({
            "tier": tier,
            "summary_text": "preview",
            "actions": [],
            "attachments": [],
            "warnings": [],
            "debug": debug,
        })
        .to_string()
    }

    #[test]
    fn provider_payload_serialization_snapshot() {
        let mut metadata = Map::new();
        metadata.insert("provider".to_string(), Value::String("planned".to_string()));
        let payload = ProviderPayload {
            content_type: "application/json".to_string(),
            body_b64: "ZGF0YQ==".to_string(),
            metadata,
        };
        assert_json_snapshot!(payload);
    }

    #[test]
    fn encode_result_serialization_snapshot() {
        let mut metadata = Map::new();
        metadata.insert("kind".to_string(), Value::String("fallback".to_string()));
        let payload = ProviderPayload {
            content_type: "application/json".to_string(),
            body_b64: "ZmFsbGJhY2s=".to_string(),
            metadata,
        };
        let result = EncodeResult {
            ok: true,
            payload: Some(payload),
            warnings: vec![RenderWarning {
                code: "example".to_string(),
                message: Some("ok".to_string()),
                details: None,
            }],
            error: None,
        };
        assert_json_snapshot!(result);
    }

    #[test]
    fn encode_with_debug_body_preserved() {
        let envelope = envelope_with_text(Some("hello"));
        let debug = json!({
            "payload": {
                "card": {
                    "title": "test"
                }
            }
        });
        let plan = plan_json("tier_a", Some(debug.clone()));
        let result = encode_from_render_plan(&plan, &envelope, Some("universal-provider"));
        assert!(result.ok);
        let payload = result.payload.expect("payload");
        let decoded = STANDARD.decode(&payload.body_b64).expect("decode");
        let decoded_value: Value = serde_json::from_slice(&decoded).expect("json");
        assert_eq!(decoded_value["payload"]["card"]["title"], "test");
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.code == "passthrough_no_downsample")
        );
        assert_eq!(
            payload.metadata.get("payload"),
            Some(&debug["payload"].clone())
        );
    }

    #[test]
    fn encode_fallback_inserts_text() {
        let envelope = envelope_with_text(None);
        let plan = plan_json("tier_d", None);
        let result = encode_from_render_plan(&plan, &envelope, Some("webex"));
        assert!(result.ok);
        let payload = result.payload.expect("payload");
        let decoded = STANDARD.decode(&payload.body_b64).expect("decode");
        let decoded_value: Value = serde_json::from_slice(&decoded).expect("json");
        assert_eq!(decoded_value["text"], "preview");
    }

    #[test]
    fn encode_invalid_plan_reports_error() {
        let envelope = envelope_with_text(Some("payload"));
        let result = encode_from_render_plan("not json", &envelope, None);
        assert!(!result.ok);
        assert!(result.payload.is_none());
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("render_plan json invalid")
        );
    }
}
