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
//! This slice is **logic only** — no rendering. `CanvasLedger` owns placement,
//! collision avoidance, revision bumping, the patch log, and the compact context
//! it hands the next agent turn.
//!
//! Several types here are not yet referenced by the rest of the shell — they are
//! consumed by Slice 5 (renderer) and Slice 7 (prompt/context). They are exercised
//! by this module's own tests, so dead-code lints are silenced module-wide rather
//! than peppering `#[allow]` on each item. The re-exports below are this module's
//! public API for those later slices; until they are wired in they read as unused.
#![allow(dead_code, unused_imports)]

mod layout;
mod ledger;
mod patch;

pub use layout::{next_available_slot, LayoutEngine, DEFAULT_COMPONENT_HEIGHT, DEFAULT_COMPONENT_WIDTH};
pub use ledger::{
    CanvasComponent, CanvasEdge, CanvasLedger, CanvasMode, ComponentKind, EdgeKind, EdgeRoute,
    Port, SelectionState,
};
pub use patch::{
    ActorRef, CanvasComponentPatch, CanvasEdgePatch, CanvasId, ComponentId, EdgeId, Endpoint,
    FocusTarget, LayoutStrategy, LayoutTarget, PatchId, Rect, SurfaceId, SurfacePatch,
    SurfacePatchEnvelope, Viewport,
};
