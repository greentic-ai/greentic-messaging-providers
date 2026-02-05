use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCard(pub Value);

impl MessageCard {
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

impl From<Value> for MessageCard {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl From<MessageCard> for Value {
    fn from(card: MessageCard) -> Self {
        card.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Tier {
    #[default]
    Basic,
    Advanced,
    Premium,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityProfile {
    pub allow_images: bool,
    pub allow_factset: bool,
    pub allow_inputs: bool,
    pub allow_postbacks: bool,
}

impl CapabilityProfile {
    pub fn for_tier(tier: Tier) -> Self {
        match tier {
            Tier::Premium => Self {
                allow_images: true,
                allow_factset: true,
                allow_inputs: true,
                allow_postbacks: true,
            },
            Tier::Advanced => Self {
                allow_images: true,
                allow_factset: true,
                allow_inputs: true,
                allow_postbacks: false,
            },
            Tier::Basic => Self {
                allow_images: false,
                allow_factset: false,
                allow_inputs: false,
                allow_postbacks: false,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderIntent {
    Card,
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageCardKind {
    Standard,
}

#[derive(Clone, Debug)]
pub struct AuthRenderSpec {
    pub kind: MessageCardKind,
}

impl AuthRenderSpec {
    pub fn new(kind: MessageCardKind) -> Self {
        Self { kind }
    }
}

#[derive(Clone, Debug)]
pub struct RenderSpec {
    pub intent: RenderIntent,
    pub card: MessageCard,
    pub kind: MessageCardKind,
}

impl RenderSpec {
    pub fn card(card: MessageCard) -> Self {
        Self {
            intent: RenderIntent::Card,
            kind: MessageCardKind::Standard,
            card,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RenderOutput {
    pub payload: Value,
    pub warnings: Vec<String>,
    pub used_modal: bool,
    pub limit_exceeded: bool,
    pub sanitized_count: usize,
    pub url_blocked_count: usize,
}

#[derive(Clone, Debug)]
pub struct RenderSnapshot {
    pub tier: Tier,
    pub target_tier: Tier,
    pub downgraded: bool,
    pub output: RenderOutput,
}

#[derive(Clone, Debug)]
pub struct PlatformPreview {
    pub payload: Value,
    pub tier: Tier,
    pub target_tier: Tier,
    pub downgraded: bool,
    pub used_modal: bool,
    pub limit_exceeded: bool,
    pub sanitized_count: usize,
    pub url_blocked_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RenderResponse {
    pub intent: RenderIntent,
    pub payload: Value,
    pub preview: PlatformPreview,
    pub warnings: Vec<String>,
    pub downgraded: bool,
    pub capability: Option<CapabilityProfile>,
}
