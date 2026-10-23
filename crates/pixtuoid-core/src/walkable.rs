//! Walkability primitives — pure data, no terminal deps. Coordinates are
//! `(x, y)` u16 pixel positions; origin top-left.
//!
//! The static [`WalkableMask`] is kept separate from the per-frame
//! [`OccupancyOverlay`] so the mask can be cached and shipped over the wire.

use crate::grid::Grid;

/// Static obstacle mask sized `width × height` pixels — a `Grid<bool>`
/// (`true` = open floor, `false` = obstacle). An ALIAS, not a wrapper: the
/// mask's dims ARE the grid's `width()`/`height()` accessors, and the obstacle
/// ops live in the `impl Grid<bool>` below.
pub type WalkableMask = Grid<bool>;

// Accepted residual of the alias form: these become visible on EVERY
// `Grid<bool>`, including `ReachSet`'s private inner grid where they are
// semantically off. Harmless — that grid is private and never calls them — and
// an extension trait would cost an import at every call site.
impl Grid<bool> {
    /// Create a fully-open mask. Caller fills obstacles via `mark_blocked`.
    #[inline]
    pub fn new_open(width: u16, height: u16) -> Self {
        Grid::filled(width, height, true)
    }

    /// Mark a rect (with `pad` extra pixels on each side) as blocked.
    /// Out-of-bounds pixels are clipped — caller doesn't need to bounds-check.
    #[inline]
    pub fn mark_blocked(&mut self, x: u16, y: u16, w: u16, h: u16, pad: u16) {
        let min_x = x.saturating_sub(pad);
        let max_x = x.saturating_add(w).saturating_add(pad).min(self.width);
        let min_y = y.saturating_sub(pad);
        let max_y = y.saturating_add(h).saturating_add(pad).min(self.height);
        for yy in min_y..max_y {
            for xx in min_x..max_x {
                self.set(xx, yy, false);
            }
        }
    }

    /// Carve `true` back into a rect — used for door cutouts in the wall band.
    pub fn mark_walkable(&mut self, x: u16, y: u16, w: u16, h: u16) {
        let max_x = x.saturating_add(w).min(self.width);
        let max_y = y.saturating_add(h).min(self.height);
        for yy in y..max_y {
            for xx in x..max_x {
                self.set(xx, yy, true);
            }
        }
    }

    /// O(1) walkability lookup. Out-of-bounds queries return `false` so
    /// routers can probe near the edges without bounds checks.
    #[inline]
    pub fn is_walkable(&self, x: u16, y: u16) -> bool {
        self.get_or(x, y, false)
    }
}

/// Dynamic per-frame occupancy — rebuilt each render tick from current agent
/// positions, so routers can avoid live agents without touching the mask.
#[derive(Debug, Clone, Default)]
pub struct OccupancyOverlay {
    /// `(x, y, width, height)` of each blocked rect.
    rects: Vec<(u16, u16, u16, u16)>,
}

impl OccupancyOverlay {
    /// An empty overlay — no rects blocked.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every blocked rect, ready to rebuild for the next frame.
    pub fn clear(&mut self) {
        self.rects.clear();
    }

    /// Block the rect at `(x, y)` of size `w × h`.
    pub fn add(&mut self, x: u16, y: u16, w: u16, h: u16) {
        self.rects.push((x, y, w, h));
    }

    /// The number of blocked rects.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.rects.len()
    }

    /// True when no rect is blocked.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// True if (`x`, `y`) falls inside any current occupancy rect.
    /// Linear scan — fine while N stays in the low tens.
    #[inline]
    pub fn blocks(&self, x: u16, y: u16) -> bool {
        self.rects.iter().any(|&(rx, ry, rw, rh)| {
            x >= rx && x < rx.saturating_add(rw) && y >= ry && y < ry.saturating_add(rh)
        })
    }

    /// Order-stable hash of the current occupancy set — rects are sorted
    /// before hashing, so push order cannot change the signature the router
    /// cache invalidates on.
    pub fn signature(&self) -> u64 {
        let mut sorted: Vec<(u16, u16, u16, u16)> = self.rects.clone();
        sorted.sort_unstable();
        crate::id::fnv1a(
            sorted
                .iter()
                .flat_map(|&(x, y, w, h)| [x, y, w, h])
                .map(u64::from),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_walkable_carves_a_cutout() {
        let mut m = WalkableMask::new_open(10, 10);
        m.mark_blocked(0, 0, 10, 4, 0);
        assert!(!m.is_walkable(5, 2));
        m.mark_walkable(4, 0, 3, 4);
        assert!(m.is_walkable(5, 2));
    }

    #[test]
    fn overlay_blocks_inside_rects() {
        let mut o = OccupancyOverlay::new();
        assert_eq!(o.len(), 0);
        o.add(10, 10, 5, 5);
        o.add(20, 20, 3, 3);
        assert_eq!(o.len(), 2);
        assert!(o.blocks(12, 12));
        assert!(!o.blocks(9, 10));
        assert!(!o.blocks(15, 10));
        o.clear();
        assert_eq!(o.len(), 0);
    }

    #[test]
    fn overlay_signature_changes_with_contents() {
        let mut o = OccupancyOverlay::new();
        let s_empty = o.signature();
        o.add(1, 2, 3, 4);
        let s_one = o.signature();
        assert_ne!(s_empty, s_one);
        o.clear();
        assert_eq!(o.signature(), s_empty);
    }

    #[test]
    fn overlay_signature_is_order_independent() {
        let mut a = OccupancyOverlay::new();
        a.add(10, 20, 5, 5);
        a.add(30, 40, 8, 8);
        let mut b = OccupancyOverlay::new();
        b.add(30, 40, 8, 8);
        b.add(10, 20, 5, 5);
        assert_eq!(a.signature(), b.signature());
    }
}

#[cfg(test)]
mod prop {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn open_mask_is_walkable_iff_in_bounds(
            w in 1u16..=256, h in 1u16..=256, x in 0u16..512, y in 0u16..512,
        ) {
            let m = WalkableMask::new_open(w, h);
            prop_assert_eq!(m.is_walkable(x, y), x < w && y < h);
        }

        #[test]
        fn mark_blocked_blocks_exactly_its_clipped_padded_rect(
            w in 1u16..=48, h in 1u16..=48,
            x in 0u16..160, y in 0u16..160, rw in 0u16..160, rh in 0u16..160, pad in 0u16..24,
        ) {
            let mut m = WalkableMask::new_open(w, h);
            m.mark_blocked(x, y, rw, rh, pad);
            let min_x = x.saturating_sub(pad);
            let max_x = x.saturating_add(rw).saturating_add(pad).min(w);
            let min_y = y.saturating_sub(pad);
            let max_y = y.saturating_add(rh).saturating_add(pad).min(h);
            for yy in 0..h {
                for xx in 0..w {
                    let blocked = (min_x..max_x).contains(&xx) && (min_y..max_y).contains(&yy);
                    prop_assert_eq!(m.is_walkable(xx, yy), !blocked, "cell ({}, {})", xx, yy);
                }
            }
        }

        #[test]
        fn mark_walkable_over_the_whole_mask_restores_walkability(
            w in 1u16..=48, h in 1u16..=48,
            x in 0u16..96, y in 0u16..96, rw in 0u16..96, rh in 0u16..96, pad in 0u16..16,
        ) {
            let mut m = WalkableMask::new_open(w, h);
            m.mark_blocked(x, y, rw, rh, pad);
            m.mark_walkable(0, 0, w, h);
            for yy in 0..h {
                for xx in 0..w {
                    prop_assert!(m.is_walkable(xx, yy));
                }
            }
        }
    }
}
