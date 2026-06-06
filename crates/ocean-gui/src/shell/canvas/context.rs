//! Slice 7 (OCEAN-154): native canvas → next-turn prompt context.
//!
//! This is the consumer half of the GPUI Masterbuild keystone. The agent half
//! (ocean-os `crates/ocean-agent`) tells the model *that* a canvas exists and to
//! drive it with `surface_patch`; this module produces *what* the canvas
//! currently holds so the model can choose stable ids, coordinates, containers,
//! and update targets instead of guessing.
//!
//! # The injection channel (and the trap it avoids)
//!
//! Per OCEAN-143 the daemon **discards** the `AgentTurnRequest::guidance` field.
//! So this context must NOT ride on `guidance` — that would be a silent no-op.
//! Instead it is folded into the **prompt** itself (the field the daemon always
//! reads and forwards to the model). The shell wraps the user's prompt with
//! [`canvas_context_block`] before sending, exactly as the older tldraw-era
//! `prompt_with_surface_context` does for the webview ledger.
//!
//! The payload is sourced from [`CanvasLedger::compact_context`] (Slice 4): the
//! active canvas id, all known canvas ids, components (id / kind / rect /
//! optional title), edges, selection, mode, viewport, and revision — nothing
//! heavy (no patch log, no per-component provenance, no full content bodies).

use serde::{Deserialize, Serialize};

use super::ledger::{CanvasLedger, CompactCanvasContext};

/// The instruction header that precedes the JSON snapshot. Short and explicit:
/// the model is told the snapshot is authoritative shared state and that canvas
/// mutations go through `surface_patch`, not chat ASCII.
const CANVAS_CONTEXT_CONTRACT: &str = "\
The block below is the live state of the Ocean native canvas for this session — \
shared working memory for you and the human. It is authoritative. \
Reuse the component ids, kinds, rects, edges, selection, and viewport shown here \
when you read or mutate the canvas. To change the canvas, call `surface_patch`; \
do not draw ASCII diagrams in chat and do not ask the human to draw manually. \
If a component already exists, update it by id rather than creating a duplicate. \
If exact x/y does not matter, omit it and let the app place the component.";

/// Compact, agent-facing snapshot of the GPUI shell's native canvas state for a
/// single turn.
///
/// `canvas_ids` lists every canvas the shell currently tracks (today the native
/// shell holds a single active ledger, so this is usually one entry, but the
/// shape is plural so the contract is stable as multi-canvas lands). `active`
/// carries the full [`CompactCanvasContext`] for the active canvas; `None` when
/// no native canvas is present yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasTurnContext {
    /// Id of the active canvas, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_canvas_id: Option<String>,
    /// Every canvas id the shell currently tracks.
    pub canvas_ids: Vec<String>,
    /// Full compact snapshot of the active canvas (components, edges, selection,
    /// mode, viewport, revision).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<CompactCanvasContext>,
}

impl CanvasTurnContext {
    /// Build the turn context from the shell's active native ledger.
    ///
    /// `None` ledger → an empty context (`canvas_ids` empty, `active` `None`),
    /// which serializes to a tiny block the model can read as "no native canvas
    /// yet".
    #[must_use]
    pub fn from_ledger(ledger: Option<&CanvasLedger>) -> Self {
        match ledger {
            Some(ledger) => {
                let compact = ledger.compact_context();
                let active_id = compact.canvas_id.to_string();
                Self {
                    active_canvas_id: Some(active_id.clone()),
                    canvas_ids: vec![active_id],
                    active: Some(compact),
                }
            }
            None => Self {
                active_canvas_id: None,
                canvas_ids: Vec::new(),
                active: None,
            },
        }
    }

    /// Whether the context describes any native canvas at all. Used by the shell
    /// to skip injection entirely when there is nothing to say (keeps prompts
    /// clean on non-canvas turns).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_none() && self.canvas_ids.is_empty()
    }

    /// JSON string suitable for embedding in a prompt.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Render the full prompt-injection block (contract header + JSON snapshot) for
/// the given ledger. Returns `None` when there is no native canvas, so the
/// caller can leave the prompt untouched.
///
/// The block is wrapped in `<ocean_canvas_context>` tags so it is visually and
/// structurally distinct from the user's own text and from the older
/// `<ocean_surface_context>` (tldraw webview) block.
#[must_use]
pub fn canvas_context_block(ledger: Option<&CanvasLedger>) -> Option<String> {
    let ctx = CanvasTurnContext::from_ledger(ledger);
    if ctx.is_empty() {
        return None;
    }
    Some(format!(
        "<ocean_canvas_context>\n{CANVAS_CONTEXT_CONTRACT}\n\n{}\n</ocean_canvas_context>",
        ctx.to_json()
    ))
}

/// Fold the native canvas context into an outgoing prompt. When there is a
/// native canvas, the block is appended after the user's prompt; otherwise the
/// prompt is returned unchanged.
///
/// This is the single function the shell calls on the send path so the canvas
/// state reaches the model through the **prompt** field (which the daemon reads),
/// never the discarded `guidance` field.
#[must_use]
pub fn prompt_with_canvas_context(prompt: &str, ledger: Option<&CanvasLedger>) -> String {
    match canvas_context_block(ledger) {
        Some(block) => format!("{prompt}\n\n{block}"),
        None => prompt.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::canvas::ledger::CanvasMode;
    use crate::shell::canvas::patch::{
        ActorRef, CanvasComponentPatch, ComponentId, Rect, SurfacePatch,
    };
    use serde_json::json;

    fn ledger_with_card() -> CanvasLedger {
        let mut ledger = CanvasLedger::new("canvas:main", "sess-1", CanvasMode::default());
        ledger.apply_patch(
            SurfacePatch::UpsertComponent {
                component: CanvasComponentPatch {
                    id: ComponentId::new("brief-1"),
                    kind: "brief_card".to_string(),
                    rect: Some(Rect::new(420.0, 120.0, 320.0, 220.0)),
                    z_index: None,
                    content: json!({ "title": "Sales Brief" }),
                    metadata: serde_json::Value::Null,
                },
            },
            ActorRef::agent(Some("sage".into())),
            1_000,
        );
        ledger
    }

    #[test]
    fn empty_when_no_native_canvas() {
        let ctx = CanvasTurnContext::from_ledger(None);
        assert!(ctx.is_empty());
        assert!(ctx.active.is_none());
        assert!(ctx.canvas_ids.is_empty());
        // No block is produced, so the prompt is untouched.
        assert!(canvas_context_block(None).is_none());
        assert_eq!(prompt_with_canvas_context("hi", None), "hi");
    }

    #[test]
    fn context_carries_compact_ledger_fields() {
        let ledger = ledger_with_card();
        let ctx = CanvasTurnContext::from_ledger(Some(&ledger));

        assert_eq!(ctx.active_canvas_id.as_deref(), Some("canvas:main"));
        assert_eq!(ctx.canvas_ids, vec!["canvas:main".to_string()]);

        let active = ctx.active.as_ref().expect("active canvas present");
        assert_eq!(active.canvas_id.to_string(), "canvas:main");
        assert_eq!(active.components.len(), 1);
        assert_eq!(active.components[0].id.to_string(), "brief-1");
        assert_eq!(active.components[0].kind, "brief_card");
        assert_eq!(active.components[0].title.as_deref(), Some("Sales Brief"));
    }

    #[test]
    fn block_includes_surface_patch_contract_and_component_id() {
        let ledger = ledger_with_card();
        let block = canvas_context_block(Some(&ledger)).expect("block present");

        // Contract header: model is told to use the tool, not ASCII.
        assert!(block.contains("surface_patch"));
        assert!(block.contains("do not draw ASCII diagrams"));
        assert!(block.contains("<ocean_canvas_context>"));
        assert!(block.contains("</ocean_canvas_context>"));

        // Payload: the live component id and rect actually reach the model.
        assert!(block.contains("brief-1"));
        assert!(block.contains("brief_card"));
        assert!(block.contains("\"x\":420"));
    }

    #[test]
    fn prompt_injection_appends_block_after_user_text() {
        let ledger = ledger_with_card();
        let out = prompt_with_canvas_context("what is on the canvas?", Some(&ledger));

        assert!(out.starts_with("what is on the canvas?"));
        assert!(out.contains("<ocean_canvas_context>"));
        assert!(out.contains("brief-1"));
    }
}
