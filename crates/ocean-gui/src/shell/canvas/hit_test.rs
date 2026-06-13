//! Pure viewport transform + hit-testing for the native canvas (Slice 5).
//!
//! This module holds *no* GPUI types and launches *no* window. It is the
//! coordinate-math core the renderer ([`super::render`]) and the shell's pointer
//! handlers call into:
//!
//! - [`ViewportTransform`] converts between **canvas space** (the stable x/y/w/h
//!   the ledger stores) and **screen space** (pixels inside the canvas viewport
//!   element), honoring the ledger's [`Viewport`] pan (`x`,`y`) and `zoom`.
//! - [`hit_test`] maps a screen-space point back to the topmost
//!   [`ComponentId`] under it, respecting `z_index` and the same paint order the
//!   renderer uses.
//!
//! Keeping this logic window-free is what makes the renderer testable: the
//! geometry can be exercised headlessly while the GPUI element tree is built on
//! top of the same numbers.

use super::ledger::CanvasLedger;
use super::patch::{ComponentId, Rect, Viewport};

/// A point in either canvas space or screen space (the space is conveyed by
/// which transform method produced/consumes it). Plain `f32` pair so no GPUI
/// dependency leaks into the pure layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Screen↔canvas coordinate transform for one viewport.
///
/// Canvas space is the ledger's coordinate system. Screen space is pixels
/// relative to the **top-left of the canvas viewport element**. The mapping is:
///
/// ```text
/// screen = (canvas - pan) * zoom
/// canvas = screen / zoom + pan
/// ```
///
/// where `pan = (viewport.x, viewport.y)` and `zoom = viewport.zoom`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportTransform {
    pan_x: f32,
    pan_y: f32,
    zoom: f32,
}

/// Smallest zoom we will divide by, so a degenerate `zoom: 0.0` viewport can't
/// produce infinities when mapping screen→canvas.
const MIN_ZOOM: f32 = 0.0001;

impl ViewportTransform {
    /// Build a transform from a ledger [`Viewport`]. A non-positive zoom is
    /// clamped to [`MIN_ZOOM`].
    pub fn new(viewport: Viewport) -> Self {
        Self {
            pan_x: viewport.x,
            pan_y: viewport.y,
            zoom: if viewport.zoom > MIN_ZOOM {
                viewport.zoom
            } else {
                MIN_ZOOM
            },
        }
    }

    /// The effective (clamped) zoom factor.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Map a canvas-space point to screen space.
    pub fn canvas_to_screen(&self, canvas: Vec2) -> Vec2 {
        Vec2::new(
            (canvas.x - self.pan_x) * self.zoom,
            (canvas.y - self.pan_y) * self.zoom,
        )
    }

    /// Map a screen-space point back to canvas space.
    pub fn screen_to_canvas(&self, screen: Vec2) -> Vec2 {
        Vec2::new(
            screen.x / self.zoom + self.pan_x,
            screen.y / self.zoom + self.pan_y,
        )
    }

    /// Map a canvas-space [`Rect`] to its screen-space rectangle.
    pub fn canvas_rect_to_screen(&self, rect: Rect) -> Rect {
        let origin = self.canvas_to_screen(Vec2::new(rect.x, rect.y));
        Rect::new(origin.x, origin.y, rect.w * self.zoom, rect.h * self.zoom)
    }

    /// Scale a single canvas-space length by the zoom factor (for line widths,
    /// padding, etc. that should track zoom).
    pub fn scale(&self, length: f32) -> f32 {
        length * self.zoom
    }
}

/// True if `point` (canvas space) lies inside `rect` (canvas space). Inclusive of
/// the top-left edge, exclusive of the bottom-right, matching the renderer's
/// fill convention.
pub fn rect_contains(rect: &Rect, point: Vec2) -> bool {
    point.x >= rect.x && point.x < rect.x + rect.w && point.y >= rect.y && point.y < rect.y + rect.h
}

/// Hit-test a **screen-space** point against the ledger's components.
///
/// Returns the [`ComponentId`] of the topmost component under the point, or
/// `None` if the point is over empty canvas. "Topmost" follows the renderer's
/// paint order: components are painted by ascending `z_index`, ties broken by
/// insertion order (the `IndexMap` order). The last-painted component that
/// contains the point therefore wins, so we scan in reverse paint order and
/// return the first containing hit.
pub fn hit_test(
    ledger: &CanvasLedger,
    transform: &ViewportTransform,
    screen_point: Vec2,
) -> Option<ComponentId> {
    let canvas_point = transform.screen_to_canvas(screen_point);
    paint_order(ledger)
        .into_iter()
        .rev()
        .find(|(_, rect)| rect_contains(rect, canvas_point))
        .map(|(id, _)| id)
}

/// The components in the order the renderer paints them: ascending `z_index`,
/// ties broken by stable insertion order. Returns `(id, canvas_rect)` pairs.
///
/// The renderer must paint in this exact order so that hit-testing (which walks
/// the reverse) agrees with what the user sees on top.
pub fn paint_order(ledger: &CanvasLedger) -> Vec<(ComponentId, Rect)> {
    let mut items: Vec<(usize, ComponentId, Rect, i32)> = ledger
        .components
        .values()
        .enumerate()
        .map(|(index, c)| (index, c.id.clone(), c.rect, c.z_index))
        .collect();
    // Stable sort by z_index; `enumerate` index preserves insertion order for ties.
    items.sort_by(|a, b| a.3.cmp(&b.3).then(a.0.cmp(&b.0)));
    items
        .into_iter()
        .map(|(_, id, rect, _)| (id, rect))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::canvas::{
        ActorRef, CanvasComponentPatch, CanvasMode, ComponentId, SurfacePatch,
    };
    use serde_json::Value;

    fn ledger() -> CanvasLedger {
        CanvasLedger::new("canvas:main", "sess-1", CanvasMode::Freeform)
    }

    fn upsert(id: &str, rect: Rect, z: i32) -> SurfacePatch {
        SurfacePatch::UpsertComponent {
            component: CanvasComponentPatch {
                id: ComponentId::new(id),
                kind: "card".to_string(),
                rect: Some(rect),
                z_index: Some(z),
                content: Value::Null,
                metadata: Value::Null,
            },
        }
    }

    fn place(l: &mut CanvasLedger, id: &str, rect: Rect, z: i32) {
        l.apply_patch(upsert(id, rect, z), ActorRef::system(), 0);
    }

    // ---- transform math ---------------------------------------------------

    #[test]
    fn identity_transform_is_a_passthrough() {
        let t = ViewportTransform::new(Viewport::default());
        let p = Vec2::new(120.0, 80.0);
        assert_eq!(t.canvas_to_screen(p), p);
        assert_eq!(t.screen_to_canvas(p), p);
    }

    #[test]
    fn pan_shifts_canvas_under_the_viewport() {
        let t = ViewportTransform::new(Viewport {
            x: 100.0,
            y: 50.0,
            zoom: 1.0,
        });
        // A component at canvas (100,50) sits at the screen origin when panned there.
        assert_eq!(
            t.canvas_to_screen(Vec2::new(100.0, 50.0)),
            Vec2::new(0.0, 0.0)
        );
        assert_eq!(
            t.screen_to_canvas(Vec2::new(0.0, 0.0)),
            Vec2::new(100.0, 50.0)
        );
    }

    #[test]
    fn zoom_scales_about_the_pan_origin() {
        let t = ViewportTransform::new(Viewport {
            x: 0.0,
            y: 0.0,
            zoom: 2.0,
        });
        assert_eq!(
            t.canvas_to_screen(Vec2::new(10.0, 20.0)),
            Vec2::new(20.0, 40.0)
        );
        assert_eq!(
            t.screen_to_canvas(Vec2::new(20.0, 40.0)),
            Vec2::new(10.0, 20.0)
        );
    }

    #[test]
    fn roundtrip_is_stable_under_pan_and_zoom() {
        let t = ViewportTransform::new(Viewport {
            x: 37.5,
            y: -12.0,
            zoom: 1.75,
        });
        let canvas = Vec2::new(421.0, 188.0);
        let back = t.screen_to_canvas(t.canvas_to_screen(canvas));
        assert!(
            (back.x - canvas.x).abs() < 1e-3,
            "x roundtrip drift: {back:?}"
        );
        assert!(
            (back.y - canvas.y).abs() < 1e-3,
            "y roundtrip drift: {back:?}"
        );
    }

    #[test]
    fn rect_maps_origin_and_size_through_zoom() {
        let t = ViewportTransform::new(Viewport {
            x: 10.0,
            y: 10.0,
            zoom: 2.0,
        });
        let screen = t.canvas_rect_to_screen(Rect::new(10.0, 10.0, 100.0, 50.0));
        assert_eq!(screen, Rect::new(0.0, 0.0, 200.0, 100.0));
    }

    #[test]
    fn degenerate_zoom_does_not_produce_infinities() {
        let t = ViewportTransform::new(Viewport {
            x: 0.0,
            y: 0.0,
            zoom: 0.0,
        });
        let mapped = t.screen_to_canvas(Vec2::new(100.0, 100.0));
        assert!(mapped.x.is_finite() && mapped.y.is_finite());
    }

    // ---- hit testing ------------------------------------------------------

    #[test]
    fn hit_test_maps_screen_point_to_the_component_under_it() {
        let mut l = ledger();
        place(&mut l, "a", Rect::new(0.0, 0.0, 100.0, 100.0), 0);
        place(&mut l, "b", Rect::new(200.0, 200.0, 100.0, 100.0), 0);
        let t = ViewportTransform::new(Viewport::default());

        assert_eq!(
            hit_test(&l, &t, Vec2::new(50.0, 50.0)),
            Some(ComponentId::new("a"))
        );
        assert_eq!(
            hit_test(&l, &t, Vec2::new(250.0, 250.0)),
            Some(ComponentId::new("b"))
        );
    }

    #[test]
    fn hit_test_returns_none_over_empty_canvas() {
        let mut l = ledger();
        place(&mut l, "a", Rect::new(0.0, 0.0, 100.0, 100.0), 0);
        let t = ViewportTransform::new(Viewport::default());
        assert_eq!(hit_test(&l, &t, Vec2::new(500.0, 500.0)), None);
    }

    #[test]
    fn hit_test_picks_the_top_z_index_on_overlap() {
        let mut l = ledger();
        // Two stacked components; "top" has the higher z_index.
        place(&mut l, "under", Rect::new(0.0, 0.0, 100.0, 100.0), 0);
        place(&mut l, "top", Rect::new(0.0, 0.0, 100.0, 100.0), 5);
        let t = ViewportTransform::new(Viewport::default());
        assert_eq!(
            hit_test(&l, &t, Vec2::new(50.0, 50.0)),
            Some(ComponentId::new("top"))
        );
    }

    #[test]
    fn hit_test_breaks_z_ties_by_last_painted() {
        let mut l = ledger();
        // Same z_index: the later-inserted one is painted last and wins.
        place(&mut l, "first", Rect::new(0.0, 0.0, 100.0, 100.0), 0);
        place(&mut l, "second", Rect::new(0.0, 0.0, 100.0, 100.0), 0);
        let t = ViewportTransform::new(Viewport::default());
        assert_eq!(
            hit_test(&l, &t, Vec2::new(50.0, 50.0)),
            Some(ComponentId::new("second"))
        );
    }

    #[test]
    fn hit_test_accounts_for_pan_and_zoom() {
        let mut l = ledger();
        place(&mut l, "a", Rect::new(100.0, 100.0, 100.0, 100.0), 0);
        // Pan so canvas (100,100) is at screen origin, zoom 2x.
        let t = ViewportTransform::new(Viewport {
            x: 100.0,
            y: 100.0,
            zoom: 2.0,
        });
        // Screen (10,10) -> canvas (105,105): inside.
        assert_eq!(
            hit_test(&l, &t, Vec2::new(10.0, 10.0)),
            Some(ComponentId::new("a"))
        );
        // Screen (-10,-10) -> canvas (95,95): outside.
        assert_eq!(hit_test(&l, &t, Vec2::new(-10.0, -10.0)), None);
    }

    #[test]
    fn paint_order_is_ascending_z_then_insertion() {
        let mut l = ledger();
        place(&mut l, "a", Rect::new(0.0, 0.0, 10.0, 10.0), 2);
        place(&mut l, "b", Rect::new(0.0, 0.0, 10.0, 10.0), 0);
        place(&mut l, "c", Rect::new(0.0, 0.0, 10.0, 10.0), 0);
        let order: Vec<String> = paint_order(&l)
            .into_iter()
            .map(|(id, _)| id.into_inner())
            .collect();
        // z=0 (b,c in insertion order) then z=2 (a).
        assert_eq!(order, vec!["b", "c", "a"]);
    }
}
