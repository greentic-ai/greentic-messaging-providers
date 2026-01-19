use sha2::{Digest, Sha256};

mod bindings {
    wit_bindgen::generate!({
        path: "../../wit/provider-core",
        world: "schema-core",
        generate_all
    });
}

use bindings::exports::greentic::provider_schema_core::schema_core_api::Guest;
use greentic_types::ProviderManifest;
use serde_json::{Value, json};

const PROVIDER_TYPE: &str = "messaging.dummy";
const CONFIG_SCHEMA_REF: &str = "assets/schemas/messaging/dummy/config.schema.json";

struct Component;

impl Guest for Component {
    fn describe() -> Vec<u8> {
        let manifest = ProviderManifest {
            provider_type: PROVIDER_TYPE.to_string(),
            capabilities: vec![],
            ops: vec!["send".to_string()],
            config_schema_ref: Some(CONFIG_SCHEMA_REF.to_string()),
            state_schema_ref: None,
        };
        json_bytes(&manifest)
    }

    fn validate_config(config_json: Vec<u8>) -> Vec<u8> {
        match serde_json::from_slice::<Value>(&config_json) {
            Ok(value) => json_bytes(&json!({
                "ok": true,
                "config": value,
            })),
            Err(err) => json_bytes(&json!({
                "ok": false,
                "error": err.to_string(),
            })),
        }
    }

    fn healthcheck() -> Vec<u8> {
        json_bytes(&json!({ "status": "ok" }))
    }

    fn invoke(op: String, input_json: Vec<u8>) -> Vec<u8> {
        match op.as_str() {
            "send" | "reply" => handle_send_like(&op, &input_json),
            other => json_bytes(&json!({
                "ok": false,
                "error": format!("unsupported op: {other}"),
            })),
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

fn json_bytes<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec())
}
