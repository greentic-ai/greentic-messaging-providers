use provider_common::component_v0_6::{canonical_cbor_bytes, decode_cbor};
use provider_common::helpers::{
    cbor_json_invoke_bridge, existing_config_from_answers, json_bytes, optional_string_from,
    schema_core_describe, schema_core_healthcheck, schema_core_validate_config, string_or_default,
};
use provider_common::qa_helpers::ApplyAnswersResult;
use serde_json::{Value, json};

mod bindings {
    wit_bindgen::generate!({
        path: "wit/messaging-provider-whatsapp",
        world: "component-v0-v6-v0",
        generate_all
    });
}

mod ac_converter;
mod config;
mod describe;
mod ops;

use config::{ProviderConfigOut, default_config_out, validate_config_out};
use describe::{I18N_KEYS, I18N_PAIRS, build_describe_payload, build_qa_spec};

const PROVIDER_ID: &str = "messaging-provider-whatsapp";
const PROVIDER_TYPE: &str = "messaging.whatsapp.cloud";
const WORLD_ID: &str = "component-v0-v6-v0";
const DEFAULT_API_BASE: &str = "https://graph.facebook.com";
const DEFAULT_API_VERSION: &str = "v19.0";
const DEFAULT_TOKEN_KEY: &str = "WHATSAPP_TOKEN";

struct Component;

impl bindings::exports::greentic::component::descriptor::Guest for Component {
    fn describe() -> Vec<u8> {
        canonical_cbor_bytes(&build_describe_payload())
    }
}

impl bindings::exports::greentic::component::runtime::Guest for Component {
    fn invoke(op: String, input_cbor: Vec<u8>) -> Vec<u8> {
        cbor_json_invoke_bridge(&op, &input_cbor, Some("send"), |op, input| {
            dispatch_json_invoke(op, input)
        })
    }
}

impl bindings::exports::greentic::component::qa::Guest for Component {
    fn qa_spec(mode: bindings::exports::greentic::component::qa::Mode) -> Vec<u8> {
        canonical_cbor_bytes(&build_qa_spec(mode))
    }

    fn apply_answers(
        mode: bindings::exports::greentic::component::qa::Mode,
        answers_cbor: Vec<u8>,
    ) -> Vec<u8> {
        use bindings::exports::greentic::component::qa::Mode;
        let mode_str = match mode {
            Mode::Default => "default",
            Mode::Setup => "setup",
            Mode::Upgrade => "upgrade",
            Mode::Remove => "remove",
        };
        apply_answers_impl(mode_str, answers_cbor)
    }
}

impl bindings::exports::greentic::component::component_i18n::Guest for Component {
    fn i18n_keys() -> Vec<String> {
        provider_common::helpers::i18n_keys_from(I18N_KEYS)
    }

    fn i18n_bundle(locale: String) -> Vec<u8> {
        provider_common::helpers::i18n_bundle_from_pairs(locale, I18N_PAIRS)
    }
}

// Backward-compatible schema-core-api export for operator v0.4.x
impl bindings::exports::greentic::provider_schema_core::schema_core_api::Guest for Component {
    fn describe() -> Vec<u8> {
        schema_core_describe(&build_describe_payload())
    }

    fn validate_config(_config_json: Vec<u8>) -> Vec<u8> {
        schema_core_validate_config()
    }

    fn healthcheck() -> Vec<u8> {
        schema_core_healthcheck()
    }

    fn invoke(op: String, input_json: Vec<u8>) -> Vec<u8> {
        if let Some(result) = provider_common::qa_invoke_bridge::dispatch_qa_ops_with_i18n(
            &op,
            &input_json,
            "whatsapp",
            describe::SETUP_QUESTIONS,
            describe::DEFAULT_KEYS,
            I18N_KEYS,
            I18N_PAIRS,
            apply_answers_impl,
        ) {
            return result;
        }
        let op = if op == "run" { "send".to_string() } else { op };
        dispatch_json_invoke(&op, &input_json)
    }
}

bindings::export!(Component with_types_in bindings);

fn apply_answers_impl(mode: &str, answers_cbor: Vec<u8>) -> Vec<u8> {
    let answers: Value = match decode_cbor(&answers_cbor) {
        Ok(value) => value,
        Err(err) => {
            return canonical_cbor_bytes(&ApplyAnswersResult::<ProviderConfigOut>::decode_error(
                format!("invalid answers cbor: {err}"),
            ));
        }
    };

    if mode == "remove" {
        return canonical_cbor_bytes(&ApplyAnswersResult::<ProviderConfigOut>::remove_default());
    }

    let mut merged = existing_config_from_answers(&answers).unwrap_or_else(default_config_out);
    let answer_obj = answers.as_object();
    let has = |key: &str| answer_obj.is_some_and(|obj| obj.contains_key(key));

    if mode == "setup" || mode == "default" {
        merged.enabled = answers
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(merged.enabled);
        merged.phone_number_id =
            string_or_default(&answers, "phone_number_id", &merged.phone_number_id);
        merged.public_base_url =
            string_or_default(&answers, "public_base_url", &merged.public_base_url);
        merged.business_account_id = optional_string_from(&answers, "business_account_id")
            .or(merged.business_account_id.clone());
        merged.api_base_url = string_or_default(&answers, "api_base_url", &merged.api_base_url);
        if merged.api_base_url.trim().is_empty() {
            merged.api_base_url = DEFAULT_API_BASE.to_string();
        }
        merged.api_version = string_or_default(&answers, "api_version", &merged.api_version);
        if merged.api_version.trim().is_empty() {
            merged.api_version = DEFAULT_API_VERSION.to_string();
        }
        merged.token = optional_string_from(&answers, "token").or(merged.token.clone());
    }

    if mode == "upgrade" {
        if has("enabled") {
            merged.enabled = answers
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(merged.enabled);
        }
        if has("phone_number_id") {
            merged.phone_number_id =
                string_or_default(&answers, "phone_number_id", &merged.phone_number_id);
        }
        if has("public_base_url") {
            merged.public_base_url =
                string_or_default(&answers, "public_base_url", &merged.public_base_url);
        }
        if has("business_account_id") {
            merged.business_account_id = optional_string_from(&answers, "business_account_id");
        }
        if has("api_base_url") {
            merged.api_base_url = string_or_default(&answers, "api_base_url", &merged.api_base_url);
        }
        if has("api_version") {
            merged.api_version = string_or_default(&answers, "api_version", &merged.api_version);
        }
        if has("token") {
            merged.token = optional_string_from(&answers, "token");
        }
        if merged.api_base_url.trim().is_empty() {
            merged.api_base_url = DEFAULT_API_BASE.to_string();
        }
        if merged.api_version.trim().is_empty() {
            merged.api_version = DEFAULT_API_VERSION.to_string();
        }
    }

    if let Err(error) = validate_config_out(&merged) {
        return canonical_cbor_bytes(&ApplyAnswersResult::<ProviderConfigOut>::validation_error(
            error,
        ));
    }

    canonical_cbor_bytes(&ApplyAnswersResult::success(merged))
}

fn dispatch_json_invoke(op: &str, input_json: &[u8]) -> Vec<u8> {
    match op {
        "send" => ops::handle_send(input_json),
        "reply" => ops::handle_reply(input_json),
        "ingest_http" => ops::ingest_http(input_json),
        "render_plan" => ops::render_plan(input_json),
        "encode" => ops::encode_op(input_json),
        "send_payload" => ops::send_payload(input_json),
        other => json_bytes(&json!({"ok": false, "error": format!("unsupported op: {other}")})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose};
    use config::parse_config_bytes;

    #[test]
    fn parse_config_requires_new_fields() {
        let cfg = br#"{"enabled":true,"phone_number_id":"pn","public_base_url":"https://example.com","api_base_url":"https://graph.facebook.com","api_version":"v19.0"}"#;
        let parsed = parse_config_bytes(cfg).expect("valid config");
        assert!(parsed.enabled);
        assert_eq!(parsed.phone_number_id, "pn");
    }

    #[test]
    fn parse_config_rejects_unknown() {
        let cfg = br#"{"enabled":true,"phone_number_id":"p","public_base_url":"https://example.com","api_base_url":"https://graph.facebook.com","api_version":"v19.0","unexpected":true}"#;
        let err = parse_config_bytes(cfg).unwrap_err();
        assert!(err.contains("unknown field"));
    }

    #[test]
    fn load_config_prefers_nested() {
        let input = json!({
            "config": {
                "enabled": true,
                "phone_number_id":"pn",
                "public_base_url":"https://example.com",
                "api_base_url":"https://graph.facebook.com",
                "api_version":"v20.0"
            },
            "api_version": "outer"
        });
        let cfg = config::load_config(&input).unwrap();
        assert_eq!(cfg.api_version.as_deref(), Some("v20.0"));
        assert_eq!(cfg.phone_number_id, "pn");
    }

    provider_common::standard_provider_tests! {
        describe_fn: build_describe_payload,
        qa_spec_fn: build_qa_spec,
        i18n_keys: I18N_KEYS,
        world_id: WORLD_ID,
        provider_id: PROVIDER_ID,
        schema_hash: "31837e4b20141730d62652092825e5ad641048cdbab052c0697990074ee7d5c3",
        qa_default_keys: ["phone_number_id", "public_base_url"],
        mode_type: bindings::exports::greentic::component::qa::Mode,
        component_type: Component,
        qa_guest_path: bindings::exports::greentic::component::qa::Guest,
        validation_answers: {"phone_number_id": "123", "public_base_url": "not-a-url"},
        validation_field: "public_base_url",
    }

    #[test]
    fn apply_answers_upgrade_preserves_unspecified_fields() {
        use bindings::exports::greentic::component::qa::Guest as QaGuest;
        use bindings::exports::greentic::component::qa::Mode;
        let answers = json!({
            "existing_config": {
                "enabled": true,
                "phone_number_id": "123",
                "public_base_url": "https://example.com",
                "business_account_id": "old-business",
                "api_base_url": "https://graph.facebook.com",
                "api_version": "v19.0",
                "token": "token-a"
            },
            "business_account_id": "new-business"
        });
        let out =
            <Component as QaGuest>::apply_answers(Mode::Upgrade, canonical_cbor_bytes(&answers));
        let out_json: Value = decode_cbor(&out).expect("decode apply output");
        assert_eq!(out_json.get("ok"), Some(&Value::Bool(true)));
        let config = out_json.get("config").expect("config object");
        assert_eq!(
            config.get("phone_number_id"),
            Some(&Value::String("123".to_string()))
        );
        assert_eq!(
            config.get("business_account_id"),
            Some(&Value::String("new-business".to_string()))
        );
    }

    /// Helper: build a minimal EncodeInV1 JSON with given metadata, text, and attachments.
    fn make_encode_input(
        text: &str,
        metadata: serde_json::Map<String, Value>,
        attachments: Vec<Value>,
    ) -> Vec<u8> {
        use greentic_types::messaging::universal_dto::{EncodeInV1, RenderPlanInV1};
        use greentic_types::{
            Attachment, ChannelMessageEnvelope, Destination, EnvId, MessageMetadata, TenantCtx,
            TenantId,
        };
        use std::collections::BTreeMap;

        let env = EnvId::try_from("dev").unwrap();
        let tenant = TenantId::try_from("demo").unwrap();
        let mut meta_map = MessageMetadata::new();
        for (k, v) in &metadata {
            if let Some(s) = v.as_str() {
                meta_map.insert(k.clone(), s.to_string());
            }
        }
        let atts: Vec<Attachment> = attachments
            .iter()
            .map(|a| Attachment {
                mime_type: a
                    .get("mime_type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                url: a
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name: a.get("name").and_then(Value::as_str).map(String::from),
                size_bytes: None,
            })
            .collect();
        let msg = ChannelMessageEnvelope {
            id: "test-msg".to_string(),
            tenant: TenantCtx::new(env.clone(), tenant.clone()),
            channel: "whatsapp".to_string(),
            session_id: "sess".to_string(),
            reply_scope: None,
            from: None,
            to: vec![Destination {
                id: "123456".to_string(),
                kind: Some("phone".to_string()),
            }],
            correlation_id: None,
            text: Some(text.to_string()),
            attachments: atts,
            metadata: meta_map,
            extensions: Default::default(),
        };
        let plan = RenderPlanInV1 {
            message: msg.clone(),
            metadata: BTreeMap::new(),
        };
        let input = EncodeInV1 { message: msg, plan };
        serde_json::to_vec(&input).unwrap()
    }

    /// Helper: decode encode_op output and return the inner payload body JSON.
    fn decode_encode_payload(result: &[u8]) -> Value {
        let out: Value = serde_json::from_slice(result).unwrap();
        if out.get("ok") != Some(&Value::Bool(true)) {
            panic!(
                "encode_op returned error: {}",
                serde_json::to_string_pretty(&out).unwrap()
            );
        }
        let body_b64 = out
            .get("payload")
            .and_then(|p| p.get("body_b64"))
            .and_then(Value::as_str)
            .unwrap();
        let body_bytes = general_purpose::STANDARD.decode(body_b64).unwrap();
        serde_json::from_slice(&body_bytes).unwrap()
    }

    #[test]
    fn encode_op_video_metadata() {
        let mut meta = serde_json::Map::new();
        meta.insert("wa_video_url".into(), json!("https://example.com/vid.mp4"));
        meta.insert("wa_video_caption".into(), json!("My video"));
        let input = make_encode_input("hello", meta, vec![]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        assert_eq!(
            payload.get("wa_video").and_then(Value::as_str),
            Some("https://example.com/vid.mp4")
        );
        assert_eq!(
            payload.get("wa_video_caption").and_then(Value::as_str),
            Some("My video")
        );
    }

    #[test]
    fn encode_op_audio_metadata() {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "wa_audio_url".into(),
            json!("https://example.com/audio.mp3"),
        );
        let input = make_encode_input("hello", meta, vec![]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        assert_eq!(
            payload.get("wa_audio").and_then(Value::as_str),
            Some("https://example.com/audio.mp3")
        );
    }

    #[test]
    fn encode_op_document_metadata() {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "wa_document_url".into(),
            json!("https://example.com/doc.pdf"),
        );
        meta.insert("wa_document_filename".into(), json!("report.pdf"));
        meta.insert("wa_document_caption".into(), json!("Q4 report"));
        let input = make_encode_input("hello", meta, vec![]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        assert_eq!(
            payload.get("wa_document").and_then(Value::as_str),
            Some("https://example.com/doc.pdf")
        );
        assert_eq!(
            payload.get("wa_document_filename").and_then(Value::as_str),
            Some("report.pdf")
        );
        assert_eq!(
            payload.get("wa_document_caption").and_then(Value::as_str),
            Some("Q4 report")
        );
    }

    #[test]
    fn encode_op_sticker_metadata() {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "wa_sticker_url".into(),
            json!("https://example.com/sticker.webp"),
        );
        let input = make_encode_input("hello", meta, vec![]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        assert_eq!(
            payload.get("wa_sticker").and_then(Value::as_str),
            Some("https://example.com/sticker.webp")
        );
    }

    #[test]
    fn encode_op_location_metadata() {
        let mut meta = serde_json::Map::new();
        meta.insert("wa_location_latitude".into(), json!("51.5074"));
        meta.insert("wa_location_longitude".into(), json!("-0.1278"));
        meta.insert("wa_location_name".into(), json!("London"));
        meta.insert("wa_location_address".into(), json!("Westminster, London"));
        let input = make_encode_input("hello", meta, vec![]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        let loc = payload.get("wa_location").expect("wa_location present");
        assert_eq!(loc.get("latitude").and_then(Value::as_str), Some("51.5074"));
        assert_eq!(
            loc.get("longitude").and_then(Value::as_str),
            Some("-0.1278")
        );
        assert_eq!(loc.get("name").and_then(Value::as_str), Some("London"));
        assert_eq!(
            loc.get("address").and_then(Value::as_str),
            Some("Westminster, London")
        );
    }

    #[test]
    fn encode_op_attachment_video() {
        let att = json!({
            "mime_type": "video/mp4",
            "url": "https://example.com/clip.mp4",
            "name": "clip.mp4"
        });
        let input = make_encode_input("hello", serde_json::Map::new(), vec![att]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        assert_eq!(
            payload.get("wa_video").and_then(Value::as_str),
            Some("https://example.com/clip.mp4")
        );
    }

    #[test]
    fn encode_op_attachment_audio() {
        let att = json!({
            "mime_type": "audio/ogg",
            "url": "https://example.com/voice.ogg"
        });
        let input = make_encode_input("hello", serde_json::Map::new(), vec![att]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        assert_eq!(
            payload.get("wa_audio").and_then(Value::as_str),
            Some("https://example.com/voice.ogg")
        );
    }

    #[test]
    fn encode_op_attachment_sticker_webp() {
        let att = json!({
            "mime_type": "image/webp",
            "url": "https://example.com/sticker.webp"
        });
        let input = make_encode_input("hello", serde_json::Map::new(), vec![att]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        // image/webp → sticker, not image
        assert_eq!(
            payload.get("wa_sticker").and_then(Value::as_str),
            Some("https://example.com/sticker.webp")
        );
        assert!(payload.get("wa_image").is_none());
    }

    #[test]
    fn encode_op_attachment_image() {
        let att = json!({
            "mime_type": "image/png",
            "url": "https://example.com/photo.png"
        });
        let input = make_encode_input("hello", serde_json::Map::new(), vec![att]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        assert_eq!(
            payload.get("wa_image").and_then(Value::as_str),
            Some("https://example.com/photo.png")
        );
    }

    #[test]
    fn encode_op_attachment_document() {
        let att = json!({
            "mime_type": "application/pdf",
            "url": "https://example.com/doc.pdf",
            "name": "report.pdf"
        });
        let input = make_encode_input("hello", serde_json::Map::new(), vec![att]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        assert_eq!(
            payload.get("wa_document").and_then(Value::as_str),
            Some("https://example.com/doc.pdf")
        );
        assert_eq!(
            payload.get("wa_document_filename").and_then(Value::as_str),
            Some("report.pdf")
        );
    }

    #[test]
    fn encode_op_metadata_takes_precedence_over_attachments() {
        // Metadata wa_video_url should win over attachment video
        let mut meta = serde_json::Map::new();
        meta.insert("wa_video_url".into(), json!("https://meta.com/v.mp4"));
        let att = json!({
            "mime_type": "video/mp4",
            "url": "https://attach.com/v.mp4"
        });
        let input = make_encode_input("hello", meta, vec![att]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        // Metadata wins
        assert_eq!(
            payload.get("wa_video").and_then(Value::as_str),
            Some("https://meta.com/v.mp4")
        );
    }

    #[test]
    fn encode_op_backward_compat_ac_with_image() {
        // Existing AC → image path should still work
        let ac = json!({
            "type": "AdaptiveCard",
            "body": [
                { "type": "TextBlock", "text": "Hello World", "weight": "Bolder", "size": "Large" },
                { "type": "Image", "url": "https://example.com/img.png" },
                { "type": "TextBlock", "text": "Description text" }
            ]
        });
        let mut meta = serde_json::Map::new();
        meta.insert("adaptive_card".into(), json!(ac.to_string()));
        let input = make_encode_input("fallback", meta, vec![]);
        let result = ops::encode_op(&input);
        let payload = decode_encode_payload(&result);
        assert_eq!(
            payload.get("wa_image").and_then(Value::as_str),
            Some("https://example.com/img.png")
        );
        assert_eq!(
            payload.get("wa_header").and_then(Value::as_str),
            Some("Hello World")
        );
    }

    #[test]
    fn ingest_http_cloud_api_webhook() {
        let webhook_body = json!({
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "123456",
                "changes": [{
                    "value": {
                        "messaging_product": "whatsapp",
                        "metadata": {"display_phone_number": "1234567890", "phone_number_id": "100875836196955"},
                        "contacts": [{"profile": {"name": "Test User"}, "wa_id": "6282371863566"}],
                        "messages": [{
                            "from": "6282371863566",
                            "id": "wamid.test123",
                            "timestamp": "1708000000",
                            "text": {"body": "Halo dari WhatsApp!"},
                            "type": "text"
                        }]
                    },
                    "field": "messages"
                }]
            }]
        });
        let body_bytes = serde_json::to_vec(&webhook_body).unwrap();
        let body_b64 = general_purpose::STANDARD.encode(&body_bytes);
        // Simulate EXACT operator format (v, provider, query as tuples, headers as tuples)
        let http_in = json!({
            "v": 1,
            "provider": "messaging-whatsapp",
            "method": "POST",
            "path": "/messaging/ingress/messaging-whatsapp/default/_/",
            "body_b64": body_b64,
            "headers": [["content-type", "application/json"]],
            "query": [],
            "tenant_hint": "default",
            "team_hint": "_"
        });
        let input = serde_json::to_vec(&http_in).unwrap();
        let result_bytes = ops::ingest_http(&input);
        let result: Value = serde_json::from_slice(&result_bytes).unwrap();
        // Check events array
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events array");
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(
            event.get("text").and_then(Value::as_str),
            Some("Halo dari WhatsApp!")
        );
        assert_eq!(
            event
                .get("metadata")
                .and_then(|m| m.get("from"))
                .and_then(Value::as_str),
            Some("6282371863566")
        );
        assert_eq!(
            event
                .get("metadata")
                .and_then(|m| m.get("phone_number_id"))
                .and_then(Value::as_str),
            Some("100875836196955")
        );
        // Check body_b64 response contains the event
        let resp_body_b64 = result.get("body_b64").and_then(Value::as_str).unwrap();
        let resp_bytes = general_purpose::STANDARD.decode(resp_body_b64).unwrap();
        let resp: Value = serde_json::from_slice(&resp_bytes).unwrap();
        assert_eq!(
            resp.get("text").and_then(Value::as_str),
            Some("Halo dari WhatsApp!")
        );
        assert_eq!(
            resp.get("from").and_then(Value::as_str),
            Some("6282371863566")
        );
    }
}
