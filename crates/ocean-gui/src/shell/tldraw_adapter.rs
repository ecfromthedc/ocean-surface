//! tldraw adapter — the **optional** sketch / freehand projection layer
//! (gpui_masterbuild.md §3 "tldraw as optional sketch/freehand projection",
//! §4 "tldraw adapter: optional projection/import/export/freehand layer",
//! §14.9 "tldraw adapter demotion", OCEAN-168 / Slice 9).
//!
//! # Demotion, in one sentence
//!
//! The authoritative agent-controlled surface is the native [`CanvasLedger`]
//! (OCEAN-156/163/167). tldraw is no longer the default agent-render target and
//! is no longer a prompt-context source — it is a *projection/import/export*
//! adapter you reach only through the explicit toolbar toggle. This module is the
//! seam between the two worlds; it never reaches back into the agent loop.
//!
//! # Two directions
//!
//! - **Export (ledger → tldraw):** project the components of the canonical native
//!   [`CanvasLedger`] into the legacy [`LedgerComponent`] / [`SurfaceIpcCommand`]
//!   shapes the webview bridge ([`canvas-web/src/oceanBridge.ts`]) already knows
//!   how to `upsert_component` into tldraw. This lets a human flip into the
//!   sketch pane and see what the agent built, then doodle on top.
//!
//! - **Import (tldraw shape → ledger):** map a tldraw shape — surfaced to Rust as
//!   a [`LedgerComponent`] inside a [`SurfaceIpcEvent::LedgerSnapshot`] from the
//!   webview — back into a native [`SurfacePatch::UpsertComponent`] so freehand /
//!   sketched shapes become real [`CanvasComponent`]s in the authoritative ledger.
//!
//! Both directions are pure, window-free functions over the wire types, so the
//! mapping is unit-testable without a webview or a GPUI window. The shell wires
//! them onto the toggle/snapshot paths; this module owns only the translation.
//!
//! The translation helpers are exercised by the unit tests below; the shell-side
//! wiring that calls them on the toggle/snapshot paths lands next, so dead-code
//! lints are silenced module-wide rather than peppering each item.
#![allow(dead_code)]

use serde_json::{json, Value};

use super::canvas::{
    component_summary, component_title, ActorRef as CanvasActorRef, CanvasComponent,
    CanvasComponentPatch, CanvasLedger, ComponentId, Rect, SurfacePatch,
};
use super::surface::{LedgerComponent, SurfaceIpcCommand};

/// Metadata key the adapter stamps on an exported tldraw shape so a later import
/// can recover the originating native template/kind string instead of falling
/// back to the structural primitive.
const OCEAN_TEMPLATE_META_KEY: &str = "ocean_template";

// ---------------------------------------------------------------------------
// Export: native CanvasLedger -> tldraw (projection)
// ---------------------------------------------------------------------------

/// Project a single native [`CanvasComponent`] into the legacy
/// [`LedgerComponent`] shape the tldraw webview bridge renders.
///
/// Content is flattened to a `{ "text": … }` body (tldraw geo shapes carry rich
/// text, not arbitrary JSON), preferring an explicit title, then a one-line
/// summary. The component's native `template` string is preserved under
/// `metadata.ocean_template` so [`import_shape_to_patch`] can round-trip the kind.
#[must_use]
pub fn component_to_ledger(component: &CanvasComponent) -> LedgerComponent {
    let text = {
        let title = component_title(component);
        let summary = component_summary(component);
        if summary.is_empty() || summary == title {
            title
        } else {
            format!("{title}\n{summary}")
        }
    };

    // Carry the native template forward so the import direction can restore it,
    // and keep any agent metadata the component already had.
    let mut metadata = match &component.metadata {
        Value::Object(_) => component.metadata.clone(),
        _ => json!({}),
    };
    if let Value::Object(map) = &mut metadata {
        map.insert(
            OCEAN_TEMPLATE_META_KEY.to_string(),
            Value::String(component.template.clone()),
        );
    }

    LedgerComponent {
        id: component.id.to_string(),
        component_type: component.template.clone(),
        x: component.rect.x,
        y: component.rect.y,
        width: component.rect.w,
        height: component.rect.h,
        content: json!({ "text": text }),
        metadata,
        connections: Vec::new(),
    }
}

/// Project an entire native [`CanvasLedger`] into the ordered set of
/// [`SurfaceIpcCommand::UpsertComponent`]s that, applied in order to a fresh
/// tldraw canvas, reproduce the ledger's components.
///
/// Edges are intentionally not projected: tldraw is the freehand/sketch adapter,
/// not the workflow-graph authority (that stays native). Components are emitted in
/// the ledger's stable insertion order so the projection is deterministic.
#[must_use]
pub fn ledger_to_tldraw_commands(ledger: &CanvasLedger) -> Vec<SurfaceIpcCommand> {
    let canvas_id = ledger.canvas_id.to_string();
    ledger
        .components
        .values()
        .map(|component| SurfaceIpcCommand::UpsertComponent {
            canvas_id: canvas_id.clone(),
            component: component_to_ledger(component),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Import: tldraw shape -> native CanvasLedger (freehand capture)
// ---------------------------------------------------------------------------

/// Map a single tldraw shape (surfaced as a [`LedgerComponent`] in a webview
/// `ledger_snapshot`) into a native [`SurfacePatch::UpsertComponent`].
///
/// This is the import leg of the adapter: a freehand / sketched shape drawn in
/// the tldraw pane becomes a first-class native [`CanvasComponent`] in the
/// authoritative ledger. The shape's geometry maps straight onto a [`Rect`]; its
/// `metadata.ocean_template` (stamped on export) is restored as the patch `kind`
/// when present, otherwise the tldraw `component_type` is used, so a shape that
/// never came from Ocean (a human's fresh sketch) still imports as its tldraw
/// kind.
#[must_use]
pub fn import_shape_to_patch(shape: &LedgerComponent) -> SurfacePatch {
    let kind = shape
        .metadata
        .get(OCEAN_TEMPLATE_META_KEY)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| shape.component_type.clone());

    SurfacePatch::UpsertComponent {
        component: CanvasComponentPatch {
            id: ComponentId::new(shape.id.clone()),
            kind,
            rect: Some(Rect::new(shape.x, shape.y, shape.width, shape.height)),
            z_index: None,
            content: shape.content.clone(),
            metadata: strip_adapter_metadata(&shape.metadata),
        },
    }
}

/// Apply a tldraw `ledger_snapshot` (a batch of shapes from the webview) into the
/// native [`CanvasLedger`], importing each shape as an upsert patch. Returns the
/// touched native component ids. The shapes are attributed to `actor` (a human
/// sketching in the tldraw pane) and stamped at `created_at_ms`.
///
/// This is the function the shell calls when the operator sketches in the tldraw
/// pane and the changes should land in the authoritative ledger.
pub fn import_snapshot_into_ledger(
    ledger: &mut CanvasLedger,
    shapes: &[LedgerComponent],
    actor: CanvasActorRef,
    created_at_ms: i64,
) -> Vec<ComponentId> {
    let mut touched = Vec::with_capacity(shapes.len());
    for shape in shapes {
        let patch = import_shape_to_patch(shape);
        touched.extend(ledger.apply_patch(patch, actor.clone(), created_at_ms));
    }
    touched
}

/// Drop the adapter's bookkeeping key from imported metadata so the native
/// component's metadata reflects the shape's own data, not the projection seam.
fn strip_adapter_metadata(metadata: &Value) -> Value {
    match metadata {
        Value::Object(map) => {
            let mut cleaned = map.clone();
            cleaned.remove(OCEAN_TEMPLATE_META_KEY);
            if cleaned.is_empty() {
                Value::Null
            } else {
                Value::Object(cleaned)
            }
        }
        // A non-object (or absent) metadata carries nothing to import.
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::canvas::{CanvasMode, ComponentId};

    fn ledger_with_card() -> CanvasLedger {
        let mut ledger = CanvasLedger::new("canvas:main", "sess-1", CanvasMode::Freeform);
        ledger.apply_patch(
            SurfacePatch::UpsertComponent {
                component: CanvasComponentPatch {
                    id: ComponentId::new("brief-1"),
                    kind: "brief_card".to_string(),
                    rect: Some(Rect::new(420.0, 120.0, 320.0, 220.0)),
                    z_index: None,
                    content: json!({ "title": "Sales Brief", "body": "Draft brief" }),
                    metadata: json!({ "source": "longhouse.sales" }),
                },
            },
            CanvasActorRef::agent(Some("sage".into())),
            1_000,
        );
        ledger
    }

    // ---- export ---------------------------------------------------------

    #[test]
    fn component_projects_to_ledger_shape_preserving_geometry_and_template() {
        let ledger = ledger_with_card();
        let component = ledger.component(&ComponentId::new("brief-1")).unwrap();
        let projected = component_to_ledger(component);

        assert_eq!(projected.id, "brief-1");
        assert_eq!(projected.component_type, "brief_card");
        assert_eq!(
            (projected.x, projected.y, projected.width, projected.height),
            (420.0, 120.0, 320.0, 220.0)
        );
        // Content is flattened to text for the tldraw geo shape.
        let text = projected.content.get("text").and_then(Value::as_str).unwrap();
        assert!(text.contains("Sales Brief"));
        // Native template is preserved for a later round-trip import…
        assert_eq!(
            projected.metadata.get("ocean_template").and_then(Value::as_str),
            Some("brief_card")
        );
        // …alongside the original agent metadata.
        assert_eq!(
            projected.metadata.get("source").and_then(Value::as_str),
            Some("longhouse.sales")
        );
    }

    #[test]
    fn ledger_projects_to_one_upsert_command_per_component_in_order() {
        let mut ledger = ledger_with_card();
        ledger.apply_patch(
            SurfacePatch::UpsertComponent {
                component: CanvasComponentPatch {
                    id: ComponentId::new("note-2"),
                    kind: "card".to_string(),
                    rect: Some(Rect::new(800.0, 120.0, 200.0, 160.0)),
                    z_index: None,
                    content: json!({ "title": "Note" }),
                    metadata: Value::Null,
                },
            },
            CanvasActorRef::system(),
            2_000,
        );

        let commands = ledger_to_tldraw_commands(&ledger);
        assert_eq!(commands.len(), 2);
        let ids: Vec<&str> = commands
            .iter()
            .map(|c| match c {
                SurfaceIpcCommand::UpsertComponent { component, .. } => component.id.as_str(),
                _ => panic!("expected upsert_component"),
            })
            .collect();
        assert_eq!(ids, vec!["brief-1", "note-2"], "stable insertion order");
        // Each command targets the ledger's canvas.
        match &commands[0] {
            SurfaceIpcCommand::UpsertComponent { canvas_id, .. } => {
                assert_eq!(canvas_id, "canvas:main")
            }
            _ => panic!("expected upsert_component"),
        }
    }

    // ---- import ---------------------------------------------------------

    #[test]
    fn tldraw_shape_imports_to_upsert_patch_with_geometry() {
        // A freehand shape drawn by a human in the tldraw pane, with no Ocean
        // template metadata (a genuine sketch).
        let shape = LedgerComponent {
            id: "sketch-7".to_string(),
            component_type: "geo".to_string(),
            x: 64.0,
            y: 48.0,
            width: 240.0,
            height: 160.0,
            content: json!({ "text": "rough idea" }),
            metadata: json!({}),
            connections: Vec::new(),
        };

        let patch = import_shape_to_patch(&shape);
        let SurfacePatch::UpsertComponent { component } = patch else {
            panic!("import must produce an upsert_component patch");
        };
        assert_eq!(component.id, ComponentId::new("sketch-7"));
        // No ocean_template meta -> fall back to the tldraw shape type.
        assert_eq!(component.kind, "geo");
        let rect = component.rect.expect("imported shape carries geometry");
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (64.0, 48.0, 240.0, 160.0));
        assert_eq!(component.content.get("text").and_then(Value::as_str), Some("rough idea"));
    }

    #[test]
    fn import_restores_native_template_from_round_tripped_metadata() {
        // Export then re-import: the native template survives the round trip.
        let ledger = ledger_with_card();
        let component = ledger.component(&ComponentId::new("brief-1")).unwrap();
        let projected = component_to_ledger(component);

        let patch = import_shape_to_patch(&projected);
        let SurfacePatch::UpsertComponent { component } = patch else {
            panic!("expected upsert_component");
        };
        assert_eq!(
            component.kind, "brief_card",
            "round-tripped shape restores the native template, not the tldraw type"
        );
        // The adapter bookkeeping key is stripped from the imported metadata.
        assert!(component.metadata.get("ocean_template").is_none());
        // The original agent metadata survives.
        assert_eq!(
            component.metadata.get("source").and_then(Value::as_str),
            Some("longhouse.sales")
        );
    }

    #[test]
    fn import_snapshot_lands_shapes_as_native_components() {
        let mut ledger = CanvasLedger::new("canvas:main", "sess-1", CanvasMode::Freeform);
        let shapes = vec![
            LedgerComponent {
                id: "s1".to_string(),
                component_type: "geo".to_string(),
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
                content: json!({ "text": "a" }),
                metadata: json!({}),
                connections: Vec::new(),
            },
            LedgerComponent {
                id: "s2".to_string(),
                component_type: "geo".to_string(),
                x: 200.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
                content: json!({ "text": "b" }),
                metadata: json!({}),
                connections: Vec::new(),
            },
        ];

        let touched = import_snapshot_into_ledger(
            &mut ledger,
            &shapes,
            CanvasActorRef::human(Some("john".into())),
            5_000,
        );
        assert_eq!(touched, vec![ComponentId::new("s1"), ComponentId::new("s2")]);
        assert_eq!(ledger.components.len(), 2);
        assert!(ledger.component(&ComponentId::new("s1")).is_some());
        assert!(ledger.component(&ComponentId::new("s2")).is_some());
        // Imported shapes are attributed to the human who sketched them.
        let c = ledger.component(&ComponentId::new("s1")).unwrap();
        assert_eq!(c.created_by.kind, "human");
    }
}
