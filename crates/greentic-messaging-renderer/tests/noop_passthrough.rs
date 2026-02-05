use greentic_messaging_renderer::{
    RenderContext, RenderItem, RendererMode, render_plan_from_envelope,
};
use greentic_types::{ChannelMessageEnvelope, EnvId, MessageMetadata, TenantCtx, TenantId};
use serde_json::{Value, json};

fn build_envelope_with_card(card: Value, text: &str) -> ChannelMessageEnvelope {
    let env = EnvId::try_from("manual").expect("env id");
    let tenant = TenantId::try_from("manual").expect("tenant id");
    let mut metadata = MessageMetadata::new();
    metadata.insert(
        "adaptive_card".to_string(),
        serde_json::to_string(&card).expect("serialize card"),
    );
    ChannelMessageEnvelope {
        id: "renderer-card".to_string(),
        tenant: TenantCtx::new(env, tenant),
        channel: "renderer".to_string(),
        session_id: "renderer-session".to_string(),
        reply_scope: None,
        from: None,
        to: Vec::new(),
        correlation_id: None,
        text: Some(text.to_string()),
        attachments: Vec::new(),
        metadata,
    }
}

#[test]
fn v13_card_unchanged() {
    let card = json!({
        "type": "AdaptiveCard",
        "version": "1.3",
        "body": [{ "type": "TextBlock", "text": "Hello v1.3" }]
    });
    let envelope = build_envelope_with_card(card.clone(), "hello");
    let plan = render_plan_from_envelope(
        &envelope,
        &RenderContext::default(),
        RendererMode::Passthrough,
    );
    assert!(matches!(plan.items.first(), Some(RenderItem::Text(t)) if t == "hello"));
    assert_eq!(
        plan.items.get(1),
        Some(&RenderItem::AdaptiveCard(card.clone()))
    );
}

#[test]
fn v14_card_unchanged() {
    let card = json!({
        "type": "AdaptiveCard",
        "version": "1.4",
        "body": [{ "type": "TextBlock", "text": "Hello v1.4" }]
    });
    let envelope = build_envelope_with_card(card.clone(), "world");
    let plan = render_plan_from_envelope(
        &envelope,
        &RenderContext::default(),
        RendererMode::Passthrough,
    );
    assert_eq!(
        plan.items.get(1),
        Some(&RenderItem::AdaptiveCard(card.clone()))
    );
}

#[test]
fn mixed_content_preserved() {
    let card = json!({
        "type": "AdaptiveCard",
        "version": "1.4",
        "body": [{ "type": "TextBlock", "text": "mixed content" }]
    });
    let envelope = build_envelope_with_card(card.clone(), "mix");
    let plan = render_plan_from_envelope(
        &envelope,
        &RenderContext::new(Some("webex".to_string())),
        RendererMode::Passthrough,
    );
    assert_eq!(
        plan.items.get(1),
        Some(&RenderItem::AdaptiveCard(card.clone()))
    );
    assert_eq!(plan.summary_text.as_deref(), Some("mix"));
}

#[test]
fn renderer_mode_parse_env() {
    unsafe { std::env::set_var("GREENTIC_MESSAGING_RENDERER_MODE", "DownSample") };
    assert_eq!(RendererMode::from_env(), RendererMode::Downsample);
    unsafe { std::env::remove_var("GREENTIC_MESSAGING_RENDERER_MODE") };
    assert_eq!(RendererMode::from_env(), RendererMode::Passthrough);
}
