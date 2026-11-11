//! Walkability primitives — pure data, no terminal deps. Coordinates are
//! `(x, y)` u16 pixel positions; origin top-left.

use crate::grid::Grid;
use crate::layout::Bounds;

/// Static obstacle mask over a `width × height` canvas (`true` = open floor,
/// `false` = obstacle), and the area its coarse routing grid tiles from that
/// area's north-west corner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkableMask {
    open: Grid<bool>,
    /// The office's [`routed_area`](crate::layout::routed_area), so its routing
    /// cells stand where its floor does whatever insets frame it.
    routed: Bounds,
}

impl WalkableMask {
    /// A fully-open `width × height` mask whose coarse grid tiles `routed`.
    /// Caller fills obstacles via `mark_blocked`.
    #[inline]
    pub(crate) fn new(width: u16, height: u16, routed: Bounds) -> Self {
        Self {
            open: Grid::filled(width, height, true),
            routed,
        }
    }

    /// A fully-open mask whose coarse grid tiles all of it.
    #[cfg(test)]
    pub(crate) fn new_open(width: u16, height: u16) -> Self {
        Self::new(
            width,
            height,
            Bounds {
                x: 0,
                y: 0,
                width,
                height,
            },
        )
    }

    /// The area the coarse routing grid tiles.
    #[inline]
    pub(crate) fn routed(&self) -> Bounds {
        self.routed
    }

    /// Canvas width in pixels.
    #[inline]
    pub(crate) fn width(&self) -> u16 {
        self.open.width()
    }

    /// Canvas height in pixels.
    #[inline]
    pub(crate) fn height(&self) -> u16 {
        self.open.height()
    }

    /// Mark a rect (with `pad` extra pixels on each side) as blocked.
    /// Out-of-bounds pixels are clipped — caller doesn't need to bounds-check.
    #[inline]
    pub(crate) fn mark_blocked(&mut self, x: u16, y: u16, w: u16, h: u16, pad: u16) {
        let min_x = x.saturating_sub(pad);
        let max_x = x.saturating_add(w).saturating_add(pad).min(self.width());
        let min_y = y.saturating_sub(pad);
        let max_y = y.saturating_add(h).saturating_add(pad).min(self.height());
        for yy in min_y..max_y {
            for xx in min_x..max_x {
                self.open.set(xx, yy, false);
            }
        }
    }

    /// Carve `true` back into a rect.
    #[cfg(test)]
    pub(crate) fn mark_walkable(&mut self, x: u16, y: u16, w: u16, h: u16) {
        let max_x = x.saturating_add(w).min(self.width());
        let max_y = y.saturating_add(h).min(self.height());
        for yy in y..max_y {
            for xx in x..max_x {
                self.open.set(xx, yy, true);
            }
        }
    }

    /// O(1) walkability lookup. Out-of-bounds queries return `false` so
    /// routers can probe near the edges without bounds checks.
    #[inline]
    pub(crate) fn is_walkable(&self, x: u16, y: u16) -> bool {
        self.open.get_or(x, y, false)
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
    }
}
