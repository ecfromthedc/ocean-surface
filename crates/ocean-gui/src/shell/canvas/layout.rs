//! Placement and layout for the [`CanvasLedger`](super::CanvasLedger).
//!
//! Per the placement rules in gpui_masterbuild.md §6, **the app owns final
//! placement** — agents may suggest an exact `rect`, but they are never asked to
//! solve collision avoidance. When a patch omits coordinates the ledger calls
//! into here to allocate a deterministic, non-overlapping slot.

use super::patch::{ComponentId, Rect};
use super::ledger::CanvasComponent;

/// Default footprint for a component the agent created without a `rect`.
pub const DEFAULT_COMPONENT_WIDTH: f32 = 320.0;
/// Default footprint height.
pub const DEFAULT_COMPONENT_HEIGHT: f32 = 220.0;

/// Top-left origin of the placement grid.
const SLOT_ORIGIN_X: f32 = 80.0;
const SLOT_ORIGIN_Y: f32 = 80.0;
/// Gap between allocated slots.
const SLOT_GAP: f32 = 32.0;
/// How far the scan walks before giving up. Bounds the search so a pathological
/// canvas can't loop forever; the column count wraps to the next row.
const SLOT_SCAN_COLUMNS: usize = 64;
const SLOT_SCAN_ROWS: usize = 4096;

/// Find the first grid-aligned slot of size `width`×`height` that does not
/// intersect any rect in `occupied`.
///
/// Deterministic: the same set of occupied rects always yields the same slot, so
/// two no-coordinate upserts in sequence land in distinct, stable positions
/// (the first occupies slot 0, the second sees it occupied and takes slot 1).
///
/// Returns `None` only if the bounded scan is exhausted, which for any realistic
/// canvas means the search space is saturated.
pub fn next_available_slot<'a, I>(occupied: I, width: f32, height: f32) -> Option<Rect>
where
    I: IntoIterator<Item = &'a Rect> + Clone,
{
    for row in 0..SLOT_SCAN_ROWS {
        for column in 0..SLOT_SCAN_COLUMNS {
            let candidate = Rect::new(
                SLOT_ORIGIN_X + column as f32 * (width + SLOT_GAP),
                SLOT_ORIGIN_Y + row as f32 * (height + SLOT_GAP),
                width,
                height,
            );
            if occupied
                .clone()
                .into_iter()
                .all(|r| !r.intersects(&candidate))
            {
                return Some(candidate);
            }
        }
    }
    None
}

/// Geometric layout strategies the ledger can run over a set of components.
///
/// This is the pure-geometry engine; it returns the new rect each component
/// should take. The ledger is responsible for applying the results and bumping
/// its revision. Only `Grid`/`Row`/`Column`/`Stack` are implemented here — the
/// graph/tree strategies arrive with the renderer slice.
pub struct LayoutEngine;

impl LayoutEngine {
    /// Lay the given components out in a left-to-right, top-to-bottom grid.
    /// `columns` controls wrapping. Returns `(id, new_rect)` pairs in input order.
    pub fn grid(components: &[&CanvasComponent], columns: usize) -> Vec<(ComponentId, Rect)> {
        let columns = columns.max(1);
        components
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let col = i % columns;
                let row = i / columns;
                let rect = Rect::new(
                    SLOT_ORIGIN_X + col as f32 * (c.rect.w + SLOT_GAP),
                    SLOT_ORIGIN_Y + row as f32 * (c.rect.h + SLOT_GAP),
                    c.rect.w,
                    c.rect.h,
                );
                (c.id.clone(), rect)
            })
            .collect()
    }

    /// Lay components out in a single horizontal row.
    pub fn row(components: &[&CanvasComponent]) -> Vec<(ComponentId, Rect)> {
        let mut x = SLOT_ORIGIN_X;
        components
            .iter()
            .map(|c| {
                let rect = Rect::new(x, SLOT_ORIGIN_Y, c.rect.w, c.rect.h);
                x += c.rect.w + SLOT_GAP;
                (c.id.clone(), rect)
            })
            .collect()
    }

    /// Lay components out in a single vertical column.
    pub fn column(components: &[&CanvasComponent]) -> Vec<(ComponentId, Rect)> {
        let mut y = SLOT_ORIGIN_Y;
        components
            .iter()
            .map(|c| {
                let rect = Rect::new(SLOT_ORIGIN_X, y, c.rect.w, c.rect.h);
                y += c.rect.h + SLOT_GAP;
                (c.id.clone(), rect)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_slot_is_the_origin() {
        let occupied: Vec<Rect> = vec![];
        let slot = next_available_slot(&occupied, 100.0, 50.0).unwrap();
        assert_eq!((slot.x, slot.y), (SLOT_ORIGIN_X, SLOT_ORIGIN_Y));
    }

    #[test]
    fn second_slot_avoids_the_first() {
        let first = Rect::new(SLOT_ORIGIN_X, SLOT_ORIGIN_Y, 320.0, 220.0);
        let occupied = vec![first];
        let slot = next_available_slot(&occupied, 320.0, 220.0).unwrap();
        assert!(!slot.intersects(&first), "second slot must not overlap first");
    }

    #[test]
    fn allocation_is_deterministic() {
        let occupied: Vec<Rect> = vec![];
        let a = next_available_slot(&occupied, 200.0, 100.0).unwrap();
        let b = next_available_slot(&occupied, 200.0, 100.0).unwrap();
        assert_eq!(a, b, "same occupancy -> same slot");
    }
}
