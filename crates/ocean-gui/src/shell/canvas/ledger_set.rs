//! A set of [`CanvasLedger`]s keyed by `canvas_id` (OCEAN-257).
//!
//! The native surface used to hold exactly one [`CanvasLedger`]. But every
//! [`SurfacePatchEnvelope`](super::patch::SurfacePatchEnvelope) carries the
//! `canvas_id` it targets, and an agent can legitimately maintain several canvases
//! at once — a storyboard *and* a workflow board, say. With a single ledger, a
//! patch for a second canvas would discard the first (the apply path keyed on a
//! lone `canvas_id` match), collapsing every canvas into whichever was patched
//! last.
//!
//! [`CanvasLedgerSet`] fixes that: it stores one ledger per `canvas_id` and tracks
//! which one is *active* (the canvas the operator is currently viewing). Patches
//! route to the ledger named in their envelope; switching the active canvas just
//! changes which ledger the renderer is pointed at — no canvas is lost.
//!
//! The set itself owns no placement / apply logic; it is a keyed container plus an
//! active pointer. Callers pull a ledger out with [`take`](CanvasLedgerSet::take),
//! run the existing persistence-aware apply over it, and put the result back with
//! [`put`](CanvasLedgerSet::put). This keeps the single-canvas
//! [`CanvasLedger`] (and every consumer of it — renderer, hit-test, persistence,
//! compact-context) completely unchanged; multi-canvas is layered strictly on top.

use indexmap::IndexMap;

use super::ledger::CanvasLedger;
use super::patch::CanvasId;

/// All canvases for the active session, keyed by `canvas_id`, with a pointer to
/// the one currently shown.
///
/// Ordering follows first-seen insertion order (`IndexMap`), so a tab strip built
/// from [`canvas_ids`](Self::canvas_ids) is stable across frames: a canvas keeps
/// its position as more patches arrive.
#[derive(Debug, Clone, Default)]
pub struct CanvasLedgerSet {
    canvases: IndexMap<CanvasId, CanvasLedger>,
    /// The canvas the renderer is pointed at. Stays `None` until the first canvas
    /// appears; thereafter it always names a present canvas (see [`set_active`]).
    ///
    /// [`set_active`]: Self::set_active
    active: Option<CanvasId>,
}

impl CanvasLedgerSet {
    /// An empty set (no canvases, no active pointer).
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no canvas exists yet.
    pub fn is_empty(&self) -> bool {
        self.canvases.is_empty()
    }

    /// How many distinct canvases are present.
    pub fn len(&self) -> usize {
        self.canvases.len()
    }

    /// The canvas ids present, in stable first-seen tab order.
    pub fn canvas_ids(&self) -> Vec<CanvasId> {
        self.canvases.keys().cloned().collect()
    }

    /// The active canvas id, if any canvas exists.
    pub fn active_id(&self) -> Option<&CanvasId> {
        self.active.as_ref()
    }

    /// Borrow the active canvas's ledger, if one is active.
    pub fn active(&self) -> Option<&CanvasLedger> {
        self.active
            .as_ref()
            .and_then(|id| self.canvases.get(id))
    }

    /// Borrow a specific canvas's ledger.
    pub fn get(&self, canvas_id: &CanvasId) -> Option<&CanvasLedger> {
        self.canvases.get(canvas_id)
    }

    /// Remove and return the ledger for `canvas_id`, if present. Callers run the
    /// persistence-aware apply over the returned (or freshly created) ledger and
    /// hand the result back via [`put`](Self::put). Removing rather than borrowing
    /// keeps the apply step working on an owned value (it consumes and returns the
    /// ledger), avoiding a clone of what can be a large component map.
    pub fn take(&mut self, canvas_id: &CanvasId) -> Option<CanvasLedger> {
        self.canvases.shift_remove(canvas_id)
    }

    /// Insert or replace a canvas's ledger, keyed on the ledger's own `canvas_id`.
    /// If this is the first canvas, it also becomes the active one.
    pub fn put(&mut self, ledger: CanvasLedger) {
        let id = ledger.canvas_id.clone();
        self.canvases.insert(id.clone(), ledger);
        if self.active.is_none() {
            self.active = Some(id);
        }
    }

    /// Point the renderer at `canvas_id`. No-op if that canvas isn't present, so
    /// the active pointer never dangles.
    pub fn set_active(&mut self, canvas_id: &CanvasId) {
        if self.canvases.contains_key(canvas_id) {
            self.active = Some(canvas_id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::canvas::ledger::CanvasMode;
    use crate::shell::canvas::patch::{
        ActorRef, CanvasComponentPatch, ComponentId, Rect, SurfacePatch,
    };
    use serde_json::{json, Value};

    fn ledger_with(canvas: &str, component: &str) -> CanvasLedger {
        let mut l = CanvasLedger::new(canvas, "sess-1", CanvasMode::Freeform);
        l.apply_patch(
            SurfacePatch::UpsertComponent {
                component: CanvasComponentPatch {
                    id: ComponentId::new(component),
                    kind: "card".to_string(),
                    rect: Some(Rect::new(0.0, 0.0, 10.0, 10.0)),
                    z_index: None,
                    content: json!({ "title": component }),
                    metadata: Value::Null,
                },
            },
            ActorRef::system(),
            0,
        );
        l
    }

    #[test]
    fn distinct_canvas_ids_coexist_as_separate_ledgers() {
        let mut set = CanvasLedgerSet::new();
        set.put(ledger_with("canvas:storyboard", "frame-1"));
        set.put(ledger_with("canvas:workflow", "node-1"));

        assert_eq!(set.len(), 2, "two canvases must coexist");

        let story = set.get(&CanvasId::new("canvas:storyboard")).unwrap();
        assert!(story.component(&ComponentId::new("frame-1")).is_some());
        assert!(
            story.component(&ComponentId::new("node-1")).is_none(),
            "the workflow node must not bleed into the storyboard canvas",
        );

        let flow = set.get(&CanvasId::new("canvas:workflow")).unwrap();
        assert!(flow.component(&ComponentId::new("node-1")).is_some());
        assert!(flow.component(&ComponentId::new("frame-1")).is_none());
    }

    #[test]
    fn first_canvas_becomes_active_and_set_active_switches() {
        let mut set = CanvasLedgerSet::new();
        assert!(set.active().is_none());

        set.put(ledger_with("canvas:main", "a"));
        assert_eq!(
            set.active_id(),
            Some(&CanvasId::new("canvas:main")),
            "first canvas is active",
        );

        set.put(ledger_with("canvas:secondary", "b"));
        // Adding a second canvas does NOT steal focus.
        assert_eq!(set.active_id(), Some(&CanvasId::new("canvas:main")));

        set.set_active(&CanvasId::new("canvas:secondary"));
        assert_eq!(set.active_id(), Some(&CanvasId::new("canvas:secondary")));

        // Switching to an absent canvas is a no-op (pointer stays valid).
        set.set_active(&CanvasId::new("canvas:ghost"));
        assert_eq!(set.active_id(), Some(&CanvasId::new("canvas:secondary")));
    }

    #[test]
    fn take_then_put_roundtrips_and_preserves_other_canvases() {
        let mut set = CanvasLedgerSet::new();
        set.put(ledger_with("canvas:a", "a1"));
        set.put(ledger_with("canvas:b", "b1"));

        // Pull canvas:a out, mutate it, put it back — canvas:b is untouched.
        let mut a = set.take(&CanvasId::new("canvas:a")).expect("canvas:a present");
        assert_eq!(set.len(), 1, "take removes the canvas from the set");
        a.apply_patch(
            SurfacePatch::UpsertComponent {
                component: CanvasComponentPatch {
                    id: ComponentId::new("a2"),
                    kind: "card".to_string(),
                    rect: Some(Rect::new(20.0, 0.0, 10.0, 10.0)),
                    z_index: None,
                    content: Value::Null,
                    metadata: Value::Null,
                },
            },
            ActorRef::system(),
            1,
        );
        set.put(a);

        assert_eq!(set.len(), 2, "put restores it");
        let a = set.get(&CanvasId::new("canvas:a")).unwrap();
        assert_eq!(a.components.len(), 2, "the mutation landed");
        let b = set.get(&CanvasId::new("canvas:b")).unwrap();
        assert_eq!(b.components.len(), 1, "canvas:b is unaffected");
    }
}
