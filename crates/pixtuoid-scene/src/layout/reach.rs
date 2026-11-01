//! Coarse-cell reachability over a [`WalkableMask`], so the geometry layer
//! (`layout`) can ask "is this cell actually *routable*?" without depending on
//! the pathfind layer. It rides the SHARED `super::coarse` primitives that
//! [`crate::pathfind`]'s `AStarRouter` rides, so "reachable here" means the same
//! thing A\* will find at route time BY CONSTRUCTION. Reachability is over STATIC
//! geometry only, so it passes an EMPTY occupancy overlay.

use std::collections::VecDeque;

use super::{COARSE_CELL_SIZE, CoarseGrid, Point, snap};
use crate::grid::Grid;
use crate::walkable::{OccupancyOverlay, WalkableMask};

/// How far (in coarse cells) to snap a blocked seed to the nearest walkable
/// coarse cell — mirrors the router's start-snap so a seed sitting on a blocked
/// pixel (a door on a wall edge, a desk) still lands in the right component.
const SEED_SNAP_CELLS: u16 = 3;

/// The set of coarse cells reachable (by `CoarseGrid::neighbors` steps) from a
/// seed — i.e. the agent's connected walkable component. Built once per layout
/// from a known in-component seed (the door, or a home desk).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ReachSet {
    /// Coarse-cell reachability; the grid dims are `mask` dims / `COARSE_CELL_SIZE`.
    grid: Grid<bool>,
}

impl ReachSet {
    /// Coarse BFS over `CoarseGrid::neighbors` from `seed`'s cell (snapped to the
    /// nearest walkable cell when `seed` lands on a blocked one). An
    /// empty/degenerate mask yields an all-unreachable set.
    pub(crate) fn from_mask(mask: &WalkableMask, seed: Point) -> ReachSet {
        let cell_w = mask.width() / COARSE_CELL_SIZE;
        let cell_h = mask.height() / COARSE_CELL_SIZE;
        let mut grid = Grid::filled(cell_w, cell_h, false);
        if cell_w == 0 || cell_h == 0 {
            return ReachSet { grid };
        }
        let empty = OccupancyOverlay::new();
        let seed_cell = (seed.x / COARSE_CELL_SIZE, seed.y / COARSE_CELL_SIZE);
        if let Some(start) = snap(mask, &empty, seed_cell, cell_w, cell_h, SEED_SNAP_CELLS) {
            let mut coarse = CoarseGrid::new(mask, &empty);
            let mut q = VecDeque::new();
            grid.set(start.0, start.1, true);
            q.push_back(start);
            while let Some((cx, cy)) = q.pop_front() {
                for ((nx, ny), _) in coarse.neighbors((cx, cy)) {
                    if !grid.get_or(nx, ny, false) {
                        grid.set(nx, ny, true);
                        q.push_back((nx, ny));
                    }
                }
            }
        }
        ReachSet { grid }
    }

    /// Is the coarse cell containing pixel `p` in the reachable component?
    /// Out-of-bounds or blocked → `false`.
    ///
    /// **Conservative at cell boundaries** (a walkable px in a coarse cell whose
    /// largest `walk_piece` is under `COARSE_CELL_WALKABLE_MIN` reads unreachable), but
    /// NEVER a false positive:
    /// `reaches(p) ⇒ A* can route to p`, so `approach_point` can safely drop any
    /// side `reaches` rejects.
    pub(crate) fn reaches(&self, p: Point) -> bool {
        self.grid
            .get_or(p.x / COARSE_CELL_SIZE, p.y / COARSE_CELL_SIZE, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_field_reaches_everywhere() {
        let m = WalkableMask::new_open(64, 64);
        let r = ReachSet::from_mask(&m, Point { x: 4, y: 4 });
        assert!(r.reaches(Point { x: 4, y: 4 }));
        assert!(r.reaches(Point { x: 60, y: 60 }));
        assert!(r.reaches(Point { x: 32, y: 8 }));
        assert!(r.reaches(Point { x: 0, y: 32 }), "column-0 cell reachable");
        assert!(r.reaches(Point { x: 32, y: 0 }), "row-0 cell reachable");
    }

    #[test]
    fn walled_pocket_is_unreachable_but_main_is_reachable() {
        let mut m = WalkableMask::new_open(64, 64);
        m.mark_blocked(28, 0, 8, 64, 0);
        let r = ReachSet::from_mask(&m, Point { x: 8, y: 32 });
        assert!(r.reaches(Point { x: 8, y: 32 }), "seed side reachable");
        assert!(
            r.reaches(Point { x: 20, y: 50 }),
            "rest of seed side reachable"
        );
        assert!(
            !r.reaches(Point { x: 50, y: 32 }),
            "walled-off pocket must be unreachable"
        );
    }

    #[test]
    fn blocked_seed_snaps_into_the_component() {
        let mut m = WalkableMask::new_open(64, 64);
        m.mark_blocked(30, 0, 4, 64, 0);
        let r = ReachSet::from_mask(&m, Point { x: 31, y: 32 }); // on the wall
        assert!(
            r.reaches(Point { x: 8, y: 32 }) || r.reaches(Point { x: 56, y: 32 }),
            "a blocked seed must snap into SOME component, not vanish"
        );
    }

    #[test]
    fn seed_in_fully_blocked_pocket_snaps_to_nothing() {
        // The blocked pocket must extend ≥ (SEED_SNAP_CELLS + 1) coarse cells past
        // the seed cell in every direction, so every cell the snap scan probes
        // (incl. its ring-interior skips) is blocked and `snap` returns None.
        let mut m = WalkableMask::new_open(64, 64);
        m.mark_blocked(0, 0, 40, 40, 0);
        let r = ReachSet::from_mask(&m, Point { x: 8, y: 8 });
        assert!(!r.reaches(Point { x: 8, y: 8 }), "seed cell is blocked");
        assert!(!r.reaches(Point { x: 50, y: 50 }), "open area never seeded");
        let all_unreachable = (0..r.grid.height()).all(|cy| {
            (0..r.grid.width()).all(|cx| {
                !r.reaches(Point {
                    x: cx * crate::layout::COARSE_CELL_SIZE,
                    y: cy * crate::layout::COARSE_CELL_SIZE,
                })
            })
        });
        assert!(
            all_unreachable,
            "no-snap seed must yield an all-unreachable set"
        );
    }

    #[test]
    fn corner_seed_ring_scan_handles_negative_coords() {
        // The seed's cell is at the grid corner, so the snap ring scan walks to
        // negative cell coords before finding a walkable cell a few rings out.
        let mut m = WalkableMask::new_open(64, 64);
        m.mark_blocked(0, 0, 8, 8, 0);
        let r = ReachSet::from_mask(&m, Point { x: 1, y: 1 }); // → cell (0,0)
        assert!(
            r.reaches(Point { x: 40, y: 40 }),
            "a corner-blocked seed must snap into the open field"
        );
    }

    #[test]
    fn sub_cell_mask_yields_empty_reachset() {
        // Width 3 is narrower than COARSE_CELL_SIZE, so cell_w collapses to 0.
        let r = ReachSet::from_mask(&WalkableMask::new_open(3, 64), Point { x: 0, y: 0 });
        assert!(!r.reaches(Point { x: 0, y: 0 }));
        assert_eq!(r.grid.width(), 0, "degenerate mask has a 0-width cell grid");
    }

    #[test]
    fn reaches_is_false_out_of_bounds() {
        let m = WalkableMask::new_open(64, 64);
        let r = ReachSet::from_mask(&m, Point { x: 4, y: 4 });
        assert!(
            !r.reaches(Point { x: 9999, y: 9999 }),
            "OOB cell → unreachable"
        );
    }
}
