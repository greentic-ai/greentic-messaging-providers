use base64::{engine::general_purpose::STANDARD, Engine as _};
use greentic_types::{ChannelMessageEnvelope, EnvId, MessageMetadata, TenantCtx, TenantId};
use messaging_universal_dto::{
    EncodeInV1, HttpInV1, HttpOutV1, ProviderPayloadV1, RenderPlanInV1, RenderPlanOutV1,
    SendPayloadInV1, SendPayloadResultV1,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-dummy",
        world: "schema-core",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use greentic_types::ProviderManifest;

const PROVIDER_TYPE: &str = "messaging.dummy";
const CONFIG_SCHEMA_REF: &str = "schemas/messaging/dummy/public.config.schema.json";

struct Component;

impl Guest for Component {
    fn describe() -> Vec<u8> {
        let manifest = ProviderManifest {
            provider_type: PROVIDER_TYPE.to_string(),
            capabilities: vec![],
            ops: vec!["send".to_string(), "reply".to_string()],
            config_schema_ref: Some(CONFIG_SCHEMA_REF.to_string()),
            state_schema_ref: None,
        };
        json_bytes(&manifest)
    }

    fn validate_config(config_json: Vec<u8>) -> Vec<u8> {
        match serde_json::from_slice::<Value>(&config_json) {
            Ok(value) => json_bytes(&json!({"ok": true, "config": value})),
            Err(err) => json_bytes(&json!({"ok": false, "error": err.to_string()})),
        }
    }

    fn healthcheck() -> Vec<u8> {
        json_bytes(&json!({"status": "ok"}))
    }

    fn invoke(op: String, input_json: Vec<u8>) -> Vec<u8> {
        eprintln!(
            "messaging-provider-dummy: invoke op={} input_bytes={}",
            op,
            input_json.len()
        );
        match op.as_str() {
            "send" | "reply" => handle_send_like(op.as_str(), &input_json),
            "ingest_http" => ingest_http(&input_json),
            "render_plan" => render_plan(&input_json),
            "encode" => encode_op(&input_json),
            "send_payload" => send_payload(&input_json),
            other => json_bytes(&json!({"ok": false, "error": format!("unsupported op: {other}")})),
        }
    }
}

bindings::exports::greentic::provider_schema_core::schema_core_api::__export_greentic_provider_schema_core_schema_core_api_1_0_0_cabi!(
    Component with_types_in bindings::exports::greentic::provider_schema_core::schema_core_api
);

fn handle_send_like(op: &str, input_json: &[u8]) -> Vec<u8> {
    let parsed = serde_json::from_slice::<Value>(input_json);
    let canonical_bytes = match &parsed {
        Ok(val) => serde_json::to_vec(val).unwrap_or_else(|_| input_json.to_vec()),
        Err(_) => input_json.to_vec(),
    };
    let hash_hex = hex_sha256(&canonical_bytes);
    let message_id = pseudo_uuid_from_hex(&hash_hex);
    let status = if op == "reply" { "replied" } else { "sent" };

    let mut payload = json!({
        "ok": parsed.is_ok(),
        "provider_type": PROVIDER_TYPE,
        "op": op,
        "message_id": message_id,
        "provider_message_id": format!("dummy:{hash_hex}"),
        "status": status,
    });

    if let Ok(value) = parsed {
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("input".into(), value);
    } else if let Err(err) = parsed {
        payload
            .as_object_mut()
            .expect("payload object")
            .insert("error".into(), Value::String(err.to_string()));
    }

    json_bytes(&payload)
}

fn ingest_http(input_json: &[u8]) -> Vec<u8> {
    let request = match serde_json::from_slice::<HttpInV1>(input_json) {
        Ok(req) => req,
        Err(err) => return http_out_error(400, &format!("invalid http input: {err}")),
    };
    let body = match STANDARD.decode(&request.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => return http_out_error(400, &format!("invalid body encoding: {err}")),
    };
    let text = String::from_utf8_lossy(&body).to_string();
    let envelope = build_dummy_envelope(text.clone(), request.path.clone());
    let out = HttpOutV1 {
        status: 200,
        headers: Vec::new(),
        body_b64: STANDARD.encode(&body),
        events: vec![envelope],
    };
    json_bytes(&out)
}

fn render_plan(input_json: &[u8]) -> Vec<u8> {
    let plan_in = match serde_json::from_slice::<RenderPlanInV1>(input_json) {
        Ok(value) => value,
        Err(err) => return render_plan_error(&format!("invalid render input: {err}")),
    };
    let summary = plan_in
        .message
        .text
        .clone()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "dummy message".to_string());
    let plan_obj = json!({
        "tier": "TierD",
        "summary_text": summary,
        "actions": [],
        "attachments": [],
        "warnings": [],
        "debug": plan_in.metadata,
    });
    let plan_json =
        serde_json::to_string(&plan_obj).unwrap_or_else(|_| "{\"tier\":\"TierD\"}".to_string());
    let plan_out = RenderPlanOutV1 { plan_json };
    json_bytes(&json!({"ok": true, "plan": plan_out}))
}

fn encode_op(input_json: &[u8]) -> Vec<u8> {
    let encode_in = match serde_json::from_slice::<EncodeInV1>(input_json) {
        Ok(value) => value,
        Err(err) => return encode_error(&format!("invalid encode input: {err}")),
    };
    let text = encode_in
        .message
        .text
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "dummy payload".to_string());
    let payload_body = json!({ "body": text.clone() });
    let body_bytes = serde_json::to_vec(&payload_body).unwrap_or_else(|_| b"{}".to_vec());
    let mut metadata = HashMap::new();
    metadata.insert("text".to_string(), Value::String(text));
    metadata.insert("method".to_string(), Value::String("POST".to_string()));
    let payload = ProviderPayloadV1 {
        content_type: "application/json".to_string(),
        body_b64: STANDARD.encode(&body_bytes),
        metadata,
    };
    json_bytes(&json!({"ok": true, "payload": payload}))
}

fn send_payload(input_json: &[u8]) -> Vec<u8> {
    let send_in = match serde_json::from_slice::<SendPayloadInV1>(input_json) {
        Ok(value) => value,
        Err(err) => {
            return send_payload_error(&format!("invalid send_payload input: {err}"), false);
        }
    };
    if send_in.provider_type != PROVIDER_TYPE {
        return send_payload_error("provider type mismatch", false);
    }
    let payload_bytes = match STANDARD.decode(&send_in.payload.body_b64) {
        Ok(bytes) => bytes,
        Err(err) => {
            return send_payload_error(&format!("payload decode failed: {err}"), false);
        }
    };
    if payload_bytes.is_empty() {
        return send_payload_error("payload empty", false);
    }
    send_payload_success()
}

fn build_dummy_envelope(text: String, session_id: String) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("default").expect("env id");
    let tenant = TenantId::try_from("default").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert("universal".to_string(), "true".to_string());
    ChannelMessageEnvelope {
        id: format!("dummy-{session_id}"),
        tenant: TenantCtx::new(env.clone(), tenant.clone()),
        channel: session_id.clone(),
        session_id,
        reply_scope: None,
        user_id: None,
        correlation_id: None,
        text: Some(text),
        attachments: Vec::new(),
        metadata,
    }
}

fn http_out_error(status: u16, message: &str) -> Vec<u8> {
    let out = HttpOutV1 {
        status,
        headers: Vec::new(),
        body_b64: STANDARD.encode(message.as_bytes()),
        events: Vec::new(),
    };
    json_bytes(&out)
}

fn render_plan_error(message: &str) -> Vec<u8> {
    json_bytes(&json!({"ok": false, "error": message}))
}

fn encode_error(message: &str) -> Vec<u8> {
    json_bytes(&json!({"ok": false, "error": message}))
}

fn send_payload_error(message: &str, retryable: bool) -> Vec<u8> {
    let result = SendPayloadResultV1 {
        ok: false,
        message: Some(message.to_string()),
        retryable,
    };
    json_bytes(&result)
}

fn send_payload_success() -> Vec<u8> {
    let result = SendPayloadResultV1 {
        ok: true,
        message: None,
        retryable: false,
    };
    json_bytes(&result)
}

fn json_bytes(value: &impl serde::Serialize) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{:02x}", byte);
    }
    out
}

fn pseudo_uuid_from_hex(hex: &str) -> String {
    let padded = if hex.len() < 32 {
        format!("{hex:0<32}")
    } else {
        hex[..32].to_string()
    };
    format!(
        "{}-{}-{}-{}-{}",
        &padded[0..8],
        &padded[8..12],
        &padded[12..16],
        &padded[16..20],
        &padded[20..32]
    )
}
