//! Native **CanvasLedger** for the GPUI surface (Slice 4 of the GPUI Masterbuild
//! epic, OCEAN-151).
//!
//! This is the agent-native data layer that consumes the [`SurfacePatch`] wire
//! contract (Slice 1, merged in `ocean-os/crates/ocean-agent-sdk/src/surface.rs`)
//! and maintains the visible surface state the renderer (Slice 5) will draw and
//! the prompt builder (Slice 7) will inject.
//!
//! # Why the patch types are mirrored, not imported
//!
//! The canonical `SurfacePatch` vocabulary lives in `ocean-agent-sdk`, which is a
//! member of the *ocean-os* workspace. `ocean-surface` is a separate repository
//! with its own workspace and no path/git dependency on ocean-os — pulling in
//! `ocean-agent-sdk` would drag `ocean-protocol`, `uuid`, and the rest of the
//! ocean-os build tree into this crate just to read a JSON shape that crosses the
//! wire as text anyway.
//!
//! The contract between the two halves is **serde-stable JSON**, not a shared Rust
//! type. So this module mirrors the exact wire shape (internally tagged on `"op"`,
//! `snake_case`, transparent string ids, numeric geometry) in
//! [`patch`]. The SDK's own roundtrip tests pin the JSON shape on the producer
//! side; the tests here pin it on the consumer side. The one deliberate deviation
//! is `session_id`: ocean-surface already represents session ids as plain
//! `String` throughout `shell::surface`, so the ledger follows that convention
//! rather than re-introducing a `uuid`-backed newtype.
//!
//! The data layer ([`ledger`], [`patch`], [`layout`]) is logic only — the
//! `CanvasLedger` owns placement, collision avoidance, revision bumping, the
//! patch log, and the compact context it hands the next agent turn.
//!
//! Slice 5 adds the native renderer on top: [`hit_test`] (window-free
//! screen↔canvas transform + component hit testing) and [`render`] (the
//! [`OceanCanvasView`] GPUI view plus the pure geometry/style helpers it is built
//! from). The view renders directly from a `CanvasLedger`, so the native canvas
//! draws without any tldraw webview mounted.
//!
//! Some re-exports are consumed only by later slices (e.g. Slice 7
//! prompt/context) or by tests, so dead-code lints are silenced module-wide
//! rather than peppering `#[allow]` on each item.
#![allow(dead_code, unused_imports)]

mod context;
mod hit_test;
mod layout;
mod ledger;
mod patch;
mod persistence;
mod render;
mod templates;

pub use context::{
    canvas_context_block, prompt_with_canvas_context, CanvasTurnContext,
};
pub use hit_test::{hit_test, paint_order, rect_contains, Vec2, ViewportTransform};
pub use layout::{next_available_slot, LayoutEngine, DEFAULT_COMPONENT_HEIGHT, DEFAULT_COMPONENT_WIDTH};
pub use ledger::{
    CanvasComponent, CanvasEdge, CanvasLedger, CanvasMode, CompactCanvasContext, CompactComponent,
    CompactEdge, ComponentKind, EdgeKind, EdgeRoute, Port, SelectionState,
};
pub use patch::{
    ActorRef, CanvasComponentPatch, CanvasEdgePatch, CanvasId, ComponentId, EdgeId, Endpoint,
    FocusTarget, LayoutStrategy, LayoutTarget, PatchId, Rect, SurfaceId, SurfacePatch,
    SurfacePatchEnvelope, Viewport,
};
pub use persistence::{CanvasStore, SNAPSHOT_EVERY_N_PATCHES};
pub use render::{
    component_summary, component_title, edge_anchors, edge_endpoints, grid_line_offsets,
    rect_center, style_for_kind, template_content_for, CanvasInteraction, ComponentStyle,
    LedgerSink, LedgerSource, OceanCanvasView, OutlineState, GRID_SIZE, PORT_RADIUS,
};
pub use templates::{
    NodeStatus, TallyRow, Template, TemplateContent, TemplateExpansion,
};
