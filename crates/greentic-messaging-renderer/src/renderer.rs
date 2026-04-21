use crate::{
    ac_extract::extract_planner_card,
    context::RenderContext,
    mode::RendererMode,
    plan::{RenderItem, RenderPlan, RenderTier},
    planner::{PlannerCapabilities, plan_render},
};
use greentic_types::ChannelMessageEnvelope;
use serde_json::{Value, json};

/// Resolve an Adaptive Card from extensions (typed Value) or metadata (legacy JSON string).
fn resolve_adaptive_card(envelope: &ChannelMessageEnvelope) -> Option<Value> {
    if let Some(ac) = envelope.extensions.get("adaptive_card") {
        return Some(ac.clone());
    }
    envelope
        .metadata
        .get("adaptive_card")
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
}

/// Trait describing a renderer that turns an envelope into a plan.
pub trait CardRenderer {
    fn render_plan(
        &self,
        envelope: &ChannelMessageEnvelope,
        context: &RenderContext,
        mode: RendererMode,
    ) -> RenderPlan;
}

/// No-op renderer that passes text and saved Adaptive Cards through unchanged.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopCardRenderer;

impl CardRenderer for NoopCardRenderer {
    fn render_plan(
        &self,
        envelope: &ChannelMessageEnvelope,
        context: &RenderContext,
        mode: RendererMode,
    ) -> RenderPlan {
        let summary_text = envelope
            .text
            .as_ref()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());

        let mut items = Vec::new();
        if let Some(text) = summary_text.clone() {
            items.push(RenderItem::Text(text));
        }
        if let Some(card) = resolve_adaptive_card(envelope) {
            items.push(RenderItem::AdaptiveCard(card));
        }

        let debug = json!({
            "mode": format!("{:?}", mode),
            "target": context.target.clone(),
        });

        RenderPlan {
            tier: RenderTier::TierA,
            summary_text,
            items,
            warnings: Vec::new(),
            debug: Some(debug),
        }
    }
}

/// Convenience helper that builds a plan using the no-op renderer.
pub fn render_plan_from_envelope(
    envelope: &ChannelMessageEnvelope,
    context: &RenderContext,
    mode: RendererMode,
) -> RenderPlan {
    NoopCardRenderer.render_plan(envelope, context, mode)
}

/// Renderer that applies deterministic downsampling based on channel capabilities.
pub struct DownsampleCardRenderer {
    pub capabilities: PlannerCapabilities,
}

impl CardRenderer for DownsampleCardRenderer {
    fn render_plan(
        &self,
        envelope: &ChannelMessageEnvelope,
        context: &RenderContext,
        mode: RendererMode,
    ) -> RenderPlan {
        // In Passthrough mode, delegate to NoopCardRenderer
        if mode == RendererMode::Passthrough {
            return NoopCardRenderer.render_plan(envelope, context, mode);
        }

        let ac = resolve_adaptive_card(envelope);

        match ac {
            Some(ac_value) => {
                let card = extract_planner_card(&ac_value);
                let ac_ref = if self.capabilities.supports_adaptive_cards {
                    Some(&ac_value)
                } else {
                    None
                };
                let mut plan = plan_render(&card, &self.capabilities, ac_ref);
                // If AC-capable but planner didn't include the card (shouldn't happen for TierA/B),
                // ensure it's included
                if self.capabilities.supports_adaptive_cards
                    && !plan
                        .items
                        .iter()
                        .any(|i| matches!(i, RenderItem::AdaptiveCard(_)))
                {
                    plan.items.push(RenderItem::AdaptiveCard(ac_value));
                }
                plan
            }
            None => {
                // No AC present - text-only plan
                let summary_text = envelope
                    .text
                    .as_ref()
                    .map(|v| v.trim().to_owned())
                    .filter(|v| !v.is_empty());

                let mut items = Vec::new();
                if let Some(text) = summary_text.clone() {
                    items.push(RenderItem::Text(text));
                }

                RenderPlan {
                    tier: RenderTier::TierD,
                    summary_text,
                    items,
                    warnings: Vec::new(),
                    debug: None,
                }
            }
        }
    }
}
