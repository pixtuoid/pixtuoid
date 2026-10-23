//! Per-furniture approach geometry: where does an agent stand when it visits a
//! waypoint?
//!
//! The waypoint's `pos` is the furniture's geometric CENTER, which for obstacle
//! furniture is a blocked cell — an agent can't stand on it. [`stand_point`]
//! resolves the walkable cell just off the footprint, on the side nearest the
//! agent's home desk.
//!
//! Pure geometry over [`WalkableMask`] — no A*, no terminal deps — so the
//! stateless `pose::pure::idle_pose` and the stateful `walk` destinations
//! stay in lockstep with where the render stands the sprite (all three call
//! this with the same `origin = home desk`).

use super::decor::{Facing, Furniture, WaypointKind, furniture_def};
use super::reach::ReachSet;
use super::{Point, Size};
use pixtuoid_core::walkable::WalkableMask;

/// How far past `half + def.stand_clearance()` to keep probing for a walkable
/// cell before giving up on a side.
const STAND_SCAN: i32 = 4;

/// The four cardinal unit axes (N, S, W, E). `debug_overlay`'s DIRS is a
/// different order and stays separate.
const CARDINAL_DIRS: [(i32, i32); 4] = [(0, -1), (0, 1), (-1, 0), (1, 0)];

/// Squared Euclidean distance, in `i64` to avoid `u16` overflow before squaring;
/// never sqrt'd, only compared.
fn squared_distance(from: Point, to: Point) -> u64 {
    let ex = i64::from(from.x) - i64::from(to.x);
    let ey = i64::from(from.y) - i64::from(to.y);
    (ex * ex + ey * ey) as u64
}

/// Ground-footprint `(w, h)` the walkable mask stamps for a waypoint, or `None`
/// for slots that add no obstacle. `Pantry` is special-cased: its counter scales
/// with terminal width, so it isn't a static [`furniture_def`] row.
pub(super) fn obstacle_footprint(kind: WaypointKind, pantry_counter_size: Size) -> Option<Size> {
    if matches!(kind, WaypointKind::Pantry) {
        return Some(pantry_counter_size);
    }
    furniture_def(kind.furniture()).footprint
}

/// The stand cell for the furniture of `kind` centered at `pos`: where an agent
/// stands to use it — a seat (`occupies_pos`) its own `pos`, OBSTACLE furniture
/// the walk goal [`approach_point`] returns (else the sprite pops onto a
/// different face on arrival), which it DELEGATES to rather than re-scanning,
/// the "no valid approach" sentinel `pos` included.
pub(crate) fn stand_point(
    kind: WaypointKind,
    pos: Point,
    pantry_counter_size: Size,
    mask: &WalkableMask,
    origin: Point,
    facing: Facing,
    reachable: &ReachSet,
) -> Point {
    // approach_point would return the off-side approach cell, not the seat.
    if furniture_def(kind.furniture()).occupies_pos {
        return pos;
    }
    // NB `approach_point` takes `facing` BEFORE `pantry_counter_size` — the
    // reverse of `stand_point`'s order.
    approach_point(
        kind.furniture(),
        pos,
        facing,
        pantry_counter_size,
        mask,
        origin,
        reachable,
    )
}

/// A seat body (sofa) is wider than an appliance footprint, so its side approach
/// cells sit farther out than [`STAND_SCAN`] would reach.
const SEAT_APPROACH_SCAN: i32 = 14;

/// The first A\*-REACHABLE walkable cell scanning `(dx, dy)` from `origin`, or
/// `None` when that side offers none within [`SEAT_APPROACH_SCAN`].
///
/// Steps DEEPER through the contiguous walkable run past a coarse-rejected EDGE
/// cell (a back-row desk's gap edge is walkable yet ReachSet-rejected, and
/// dropping the whole side there loses a real approach). Once inside the run the
/// first blocked pixel STOPS the scan, so it can never hop a SECOND obstacle
/// onto a far strip — that would resurrect a back-side approach.
///
/// Single-sourced: [`approach_point`]'s seat branch ROUTES agents with it and
/// `pixel_painter::debug_overlay` DRAWS it, so the `w` overlay's green dots land
/// exactly where the agent goes.
pub(crate) fn first_reachable_on_side(
    mask: &WalkableMask,
    reachable: &ReachSet,
    origin: Point,
    dx: i32,
    dy: i32,
) -> Option<Point> {
    let mut entered = false;
    for dist in 1..=SEAT_APPROACH_SCAN {
        let cx = i32::from(origin.x) + dx * dist;
        let cy = i32::from(origin.y) + dy * dist;
        if cx < 0 || cy < 0 {
            break;
        }
        let c = Point {
            x: cx as u16,
            y: cy as u16,
        };
        if mask.is_walkable(c.x, c.y) {
            entered = true;
            if reachable.reaches(c) {
                return Some(c);
            }
        } else if entered {
            break;
        }
    }
    None
}

/// Extent [`approach_point`] scans past for obstacle furniture — the VISUAL
/// (whole sprite), so the approach cell lands clear of everything drawn. NOT the
/// mask footprint, a shallow south strip: deriving the stand distance from it
/// would pull the user INSIDE the sprite. `None` for kinds with no ground
/// footprint (seats / wall decor).
///
/// INVARIANT: the `visual/2` half-extent derived from this assumes
/// `Pivot::Center` placement. A future `TopLeft`-placed obstacle waypoint would
/// compute the stand cell off a wrong center and must pass a center, not the raw
/// origin.
fn approach_clearance_extent(kind: Furniture, pantry_counter_size: Size) -> Option<Size> {
    if matches!(kind, Furniture::Pantry) {
        return Some(pantry_counter_size);
    }
    let def = furniture_def(kind);
    def.footprint.map(|_| def.visual)
}

/// The walkable, A\*-REACHABLE cell A\* routes to when an agent visits furniture
/// `kind` at `pos` — the **approach point** (A\*'s goal). Honours the furniture's
/// allowed approach sides (`ApproachSides`, rotated by `facing`) AND coarse-grid
/// `reachable`-ility from `origin`, so A\* never targets a walled-off cell.
///
/// - **`occupies_pos` seat/desk:** an allowed-side walkable+reachable cell
///   adjacent to the seat, nearest `origin`. The back (excluded) side is never
///   scanned — you can't sit by climbing over the backrest.
/// - **obstacle:** the first reachable cell past the footprint on the allowed
///   side nearest `origin` (render == approach point).
///
/// Returns the blocked `pos` as a **"no valid approach" sentinel** when no allowed
/// reachable side exists. Callers MUST treat `== pos` as "skip this furniture"
/// rather than routing to it — routing to `pos` would let A\* snap onto the back.
pub(crate) fn approach_point(
    kind: Furniture,
    pos: Point,
    facing: Facing,
    pantry_counter_size: Size,
    mask: &WalkableMask,
    origin: Point,
    reachable: &ReachSet,
) -> Point {
    let def = furniture_def(kind);
    let mut best: Option<(u64, Point)> = None;
    if def.occupies_pos {
        let mut allowed: Option<(u64, Point)> = None;
        for (dx, dy) in CARDINAL_DIRS {
            if !def.approach.allows(facing, (dx, dy)) {
                continue;
            }
            if let Some(c) = first_reachable_on_side(mask, reachable, pos, dx, dy) {
                let d2 = squared_distance(c, origin);
                if allowed.is_none_or(|(b, _)| d2 < b) {
                    allowed = Some((d2, c));
                }
            }
        }
        return allowed.map(|(_, p)| p).unwrap_or(pos);
    } else if let Some(Size { w: fw, h: fh }) = approach_clearance_extent(kind, pantry_counter_size)
    {
        let (hx, hy) = (i32::from(fw) / 2, i32::from(fh) / 2);
        for (dx, dy) in CARDINAL_DIRS {
            if !def.approach.allows(facing, (dx, dy)) {
                continue;
            }
            let half = if dx != 0 { hx } else { hy };
            for step in 0..=STAND_SCAN {
                let dist = half + i32::from(def.stand_clearance()) + step;
                let cx = i32::from(pos.x) + dx * dist;
                let cy = i32::from(pos.y) + dy * dist;
                if cx < 0 || cy < 0 {
                    break;
                }
                let c = Point {
                    x: cx as u16,
                    y: cy as u16,
                };
                if mask.is_walkable(c.x, c.y) {
                    if reachable.reaches(c) {
                        let d2 = squared_distance(c, origin);
                        if best.is_none_or(|(bd, _)| d2 < bd) {
                            best = Some((d2, c));
                        }
                    }
                    break;
                }
            }
        }
    }
    best.map(|(_, p)| p).unwrap_or(pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overhanging_obstacle_stand_clearance_uses_visual_not_footprint() {
        let booth = furniture_def(Furniture::PhoneBooth);
        let ext = approach_clearance_extent(Furniture::PhoneBooth, Size { w: 0, h: 0 })
            .expect("booth is an approachable obstacle");
        assert_eq!(
            ext.h, booth.visual.h,
            "stand clearance must use the visual height, not the footprint"
        );
        assert!(
            ext.h > booth.footprint.unwrap().h,
            "visual extent ({}) must exceed the shallow footprint's ({})",
            ext.h,
            booth.footprint.unwrap().h
        );
        let ext = approach_clearance_extent(Furniture::StandingDesk, Size { w: 0, h: 0 }).unwrap();
        assert_eq!(ext.h, furniture_def(Furniture::StandingDesk).visual.h);
    }

    /// Build an open mask with a centred obstacle stamped with the pad `mask.rs`
    /// gives a waypoint.
    fn mask_with_obstacle(w: u16, h: u16, pos: Point, fw: u16, fh: u16) -> WalkableMask {
        let mut m = WalkableMask::new_open(w, h);
        m.mark_blocked(
            pos.x - fw / 2,
            pos.y - fh / 2,
            fw,
            fh,
            furniture_def(Furniture::Pantry).pad,
        );
        m
    }

    #[test]
    fn pantry_stand_is_walkable_and_off_center() {
        let pos = Point { x: 50, y: 50 };
        let m = mask_with_obstacle(100, 100, pos, 32, 10);
        let reach = ReachSet::from_mask(&m, Point { x: 5, y: 5 });
        let s = stand_point(
            WaypointKind::Pantry,
            pos,
            Size { w: 32, h: 10 },
            &m,
            Point { x: 50, y: 8 },
            Facing::South,
            &reach,
        );
        assert!(m.is_walkable(s.x, s.y), "stand cell must be walkable");
        assert_ne!(s, pos, "must move off the blocked center");
        assert!(s.y < pos.y, "north origin → north stand, got {s:?}");
    }

    #[test]
    fn pantry_stand_follows_origin_side() {
        let pos = Point { x: 50, y: 50 };
        let m = mask_with_obstacle(100, 100, pos, 32, 10);
        let reach = ReachSet::from_mask(&m, Point { x: 5, y: 5 });
        let south = stand_point(
            WaypointKind::Pantry,
            pos,
            Size { w: 32, h: 10 },
            &m,
            Point { x: 50, y: 95 },
            Facing::South,
            &reach,
        );
        assert!(south.y > pos.y, "south origin → south stand, got {south:?}");
        let east = stand_point(
            WaypointKind::Pantry,
            pos,
            Size { w: 32, h: 10 },
            &m,
            Point { x: 98, y: 50 },
            Facing::South,
            &reach,
        );
        assert!(east.x > pos.x, "east origin → east stand, got {east:?}");
    }

    #[test]
    fn seat_kinds_pass_through_pos() {
        let pos = Point { x: 50, y: 50 };
        let m = mask_with_obstacle(100, 100, pos, 20, 7);
        let reach = ReachSet::from_mask(&m, Point { x: 10, y: 10 });
        for kind in [
            WaypointKind::Couch,
            WaypointKind::MeetingSofa,
            WaypointKind::MeetingChair,
        ] {
            assert_eq!(
                stand_point(
                    kind,
                    pos,
                    Size { w: 0, h: 0 },
                    &m,
                    Point { x: 10, y: 10 },
                    Facing::South,
                    &reach,
                ),
                pos,
                "{kind:?} pos is already the seat cell"
            );
        }
    }

    #[test]
    fn real_layout_stand_points_are_walkable() {
        // Seat kinds return their on-furniture seat cell — which IS blocked —
        // so they're excluded from the walkability assertion.
        use crate::layout::SceneLayout;
        let seat = |k| {
            matches!(
                k,
                WaypointKind::Couch
                    | WaypointKind::MeetingSofa
                    | WaypointKind::MeetingChair
                    | WaypointKind::Island
            )
        };
        // 160×150 (cubicle_aisle ≥10) is the size at which vending + printer
        // spawn, so their stand cells are covered too.
        for (bw, bh) in [(160u16, 120u16), (160, 150)] {
            for seed in 0..5u64 {
                let l = SceneLayout::compute_with_seed(bw, bh, Some(6), seed).unwrap();
                let origin = l
                    .home_desks
                    .first()
                    .copied()
                    .unwrap_or(Point { x: 0, y: 0 });
                for wp in &l.waypoints {
                    let s = super::stand_point(
                        wp.kind,
                        wp.pos,
                        l.pantry_counter_size(),
                        &l.walkable,
                        origin,
                        wp.facing,
                        &l.reachable,
                    );
                    if seat(wp.kind) {
                        assert_eq!(s, wp.pos, "seed {seed}: {:?} should pass through", wp.kind);
                    } else {
                        assert!(
                            l.walkable.is_walkable(s.x, s.y) && s != wp.pos,
                            "{bw}x{bh} seed {seed}: {:?} stand {s:?} not a walkable off-center cell (center {:?})",
                            wp.kind,
                            wp.pos
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn real_layout_obstacle_stand_is_a_reachable_allowed_off_visual_cell() {
        // `stand_point ≡ approach_point` is true BY CONSTRUCTION (stand_point
        // delegates), so an equality assert would be a tautology — pin the
        // obstacle branch's real invariants instead.
        use crate::layout::{SceneLayout, furniture_def};
        let obstacle = |k| {
            !matches!(
                k,
                WaypointKind::Couch
                    | WaypointKind::MeetingSofa
                    | WaypointKind::MeetingChair
                    | WaypointKind::Island
            )
        };
        for (bw, bh) in [
            (160u16, 120u16),
            (192, 160),
            (240, 160),
            (256, 200),
            (200, 140),
        ] {
            for seed in 0..16u64 {
                let Some(l) = SceneLayout::compute_with_seed(bw, bh, Some(6), seed) else {
                    continue;
                };
                for &desk in &l.home_desks {
                    for wp in l.waypoints.iter().filter(|w| obstacle(w.kind)) {
                        let s = stand_point(
                            wp.kind,
                            wp.pos,
                            l.pantry_counter_size(),
                            &l.walkable,
                            desk,
                            wp.facing,
                            &l.reachable,
                        );
                        if s == wp.pos {
                            continue; // "no valid approach" sentinel
                        }
                        assert!(
                            l.walkable.is_walkable(s.x, s.y),
                            "{bw}x{bh} seed {seed}: {:?} stand {s:?} not walkable",
                            wp.kind
                        );
                        assert!(
                            l.reachable.reaches(s),
                            "{bw}x{bh} seed {seed}: {:?} stand {s:?} not coarse-reachable",
                            wp.kind
                        );
                        let dx = i32::from(s.x) - i32::from(wp.pos.x);
                        let dy = i32::from(s.y) - i32::from(wp.pos.y);
                        let dir = if dx.abs() >= dy.abs() {
                            (dx.signum(), 0)
                        } else {
                            (0, dy.signum())
                        };
                        let def = furniture_def(wp.kind.furniture());
                        assert!(
                            def.approach.allows(wp.facing, dir),
                            "{bw}x{bh} seed {seed}: {:?} stand {s:?} on a FORBIDDEN side {dir:?}",
                            wp.kind
                        );
                        // Read the visual from an INDEPENDENT authority, NOT
                        // `approach_clearance_extent` — the fn `approach_point`
                        // itself uses to place the cell — or a visual→footprint
                        // regression would shift the placement AND this oracle
                        // together, leaving it toothless.
                        let visual = if wp.kind == WaypointKind::Pantry {
                            l.pantry_counter_size()
                        } else {
                            def.visual
                        };
                        let (half, off) = if dx != 0 {
                            (i32::from(visual.w) / 2, dx.abs())
                        } else {
                            (i32::from(visual.h) / 2, dy.abs())
                        };
                        assert!(
                            off >= half + i32::from(def.stand_clearance()),
                            "{bw}x{bh} seed {seed}: {:?} stand {s:?} sits INSIDE its visual \
                             (off {off} < half {half} + clearance {})",
                            wp.kind,
                            def.stand_clearance()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn stand_point_breaks_on_negative_coords_near_the_origin_corner() {
        // `pos` this close to (0,0) drives the N/W probe below 0 before any
        // walkable cell is found, exercising the negative-coordinate break.
        let pos = Point { x: 2, y: 2 };
        let counter = Size { w: 4, h: 4 };
        let m = mask_with_obstacle(100, 100, pos, counter.w, counter.h);
        // Reach seeded from the far-open corner, so the filter is a no-op.
        let reach = ReachSet::from_mask(&m, Point { x: 90, y: 90 });
        let s = stand_point(
            WaypointKind::Pantry,
            pos,
            counter,
            &m,
            Point { x: 0, y: 0 },
            Facing::South,
            &reach,
        );
        assert!(m.is_walkable(s.x, s.y), "stand cell must be walkable");
        assert!(
            s.x > pos.x || s.y > pos.y,
            "N/W scans go negative and break → an E or S cell wins, got {s:?}"
        );
    }

    #[test]
    fn picks_only_open_side_when_walled() {
        // A vending machine recessed into a wall: everything down to y=54 is
        // blocked, so N/E/W are walled and only the south side is open.
        let pos = Point { x: 50, y: 50 };
        let mut m = WalkableMask::new_open(100, 100);
        m.mark_blocked(0, 0, 100, 54, 0);
        let reach = ReachSet::from_mask(&m, Point { x: 50, y: 80 });
        let s = stand_point(
            WaypointKind::VendingMachine,
            pos,
            Size { w: 0, h: 0 },
            &m,
            Point { x: 50, y: 5 },
            Facing::South,
            &reach,
        );
        assert!(m.is_walkable(s.x, s.y));
        assert!(s.y > pos.y, "only south is open, got {s:?}");
    }

    #[test]
    fn approach_point_seat_never_approaches_through_the_back() {
        // Open field, so the reachability filter is a no-op and only the
        // allowed-side (no-back) rule is under test.
        let pos = Point { x: 50, y: 50 };
        let m = WalkableMask::new_open(100, 100);
        let reach = ReachSet::from_mask(&m, Point { x: 5, y: 5 });
        let s = approach_point(
            Furniture::MeetingSofa,
            pos,
            Facing::South,
            Size { w: 0, h: 0 },
            &m,
            Point { x: 50, y: 5 },
            &reach,
        );
        assert!(s != pos && reach.reaches(s));
        assert!(
            !(s.x == pos.x && s.y < pos.y),
            "south-facing sofa back is north — must not approach due-north: {s:?}"
        );
        let n = approach_point(
            Furniture::MeetingSofa,
            pos,
            Facing::North,
            Size { w: 0, h: 0 },
            &m,
            Point { x: 50, y: 95 },
            &reach,
        );
        assert!(n != pos && reach.reaches(n));
        assert!(
            !(n.x == pos.x && n.y > pos.y),
            "north-facing sofa back is south — must not approach due-south: {n:?}"
        );
    }

    #[test]
    fn seat_approach_is_never_behind_the_backrest_on_real_layouts() {
        use crate::layout::{SceneLayout, furniture_def};
        for (w, h) in [(120u16, 96u16), (160, 120), (192, 160), (240, 160)] {
            for seed in 0..4u64 {
                let Some(l) = SceneLayout::compute_with_seed(w, h, Some(4), seed) else {
                    continue;
                };
                for &desk in &l.home_desks {
                    for wp in &l.waypoints {
                        let def = furniture_def(wp.kind.furniture());
                        if !def.occupies_pos {
                            continue;
                        }
                        let a = approach_point(
                            wp.kind.furniture(),
                            wp.pos,
                            wp.facing,
                            l.pantry_counter_size(),
                            &l.walkable,
                            desk,
                            &l.reachable,
                        );
                        if a == wp.pos {
                            continue; // skip sentinel
                        }
                        let dx = i32::from(a.x) - i32::from(wp.pos.x);
                        let dy = i32::from(a.y) - i32::from(wp.pos.y);
                        let dir = if dx.abs() >= dy.abs() {
                            (dx.signum(), 0)
                        } else {
                            (0, dy.signum())
                        };
                        assert!(
                            def.approach.allows(wp.facing, dir),
                            "{w}x{h} seed{seed}: {:?} at {:?} (facing {:?}) approach {a:?} is on \
                             a FORBIDDEN side {dir:?} — a back-approach was chosen",
                            wp.kind,
                            wp.pos,
                            wp.facing,
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn approach_point_seat_never_approaches_from_the_back_even_when_walled_in() {
        // A North-facing sofa boxed in on its three ALLOWED sides, with ONLY the
        // south backrest side opening onto a reachable corridor — so there is NO
        // valid approach, not even as a last resort.
        let pos = Point { x: 50, y: 50 };
        let mut m = WalkableMask::new_open(100, 100);
        m.mark_blocked(0, 0, 100, 100, 0);
        m.mark_walkable(48, 54, 5, 44); // a corridor SOUTH, behind the backrest
        let reach = ReachSet::from_mask(&m, Point { x: 50, y: 90 });
        let s = approach_point(
            Furniture::MeetingSofa,
            pos,
            Facing::North,
            Size { w: 0, h: 0 },
            &m,
            Point { x: 50, y: 95 }, // origin in the south corridor
            &reach,
        );
        assert_eq!(
            s, pos,
            "only the backrest side is reachable → no valid approach → return pos (skip), got {s:?}"
        );
    }

    #[test]
    fn def_footprint_matches_obstacle_footprint() {
        let dummy = Size { w: 32, h: 10 };
        for &kind in WaypointKind::ALL {
            if kind == WaypointKind::Pantry {
                continue;
            }
            assert_eq!(
                furniture_def(kind.furniture()).footprint,
                obstacle_footprint(kind, dummy),
                "{kind:?}: furniture_def.footprint must equal obstacle_footprint",
            );
        }
    }

    #[test]
    fn approachable_obstacle_resolves_clearance_extent() {
        let dummy = Size { w: 32, h: 10 };
        for &kind in WaypointKind::ALL {
            let def = furniture_def(kind.furniture());
            let has_footprint = def.footprint.is_some() || kind == WaypointKind::Pantry;
            if has_footprint && !def.occupies_pos {
                assert!(
                    approach_clearance_extent(kind.furniture(), dummy).is_some(),
                    "{kind:?}: an approachable obstacle must resolve a clearance extent",
                );
            }
        }
    }

    #[test]
    fn dwell_range_is_nonzero() {
        // pose::dwell_ms does `% range.max(1)`, so a zero range would silently
        // collapse the jitter.
        for &kind in WaypointKind::ALL {
            assert!(
                furniture_def(kind.furniture()).dwell.range_ms > 0,
                "{kind:?}: dwell range must be > 0",
            );
        }
    }

    #[test]
    fn pod_and_waypoint_twins_resolve_to_one_furniture_row() {
        use crate::layout::PodDecor;
        for (pod, wp) in [
            (PodDecor::PhoneBooth, WaypointKind::PhoneBooth),
            (PodDecor::StandingDesk, WaypointKind::StandingDesk),
        ] {
            assert_eq!(
                pod.furniture(),
                wp.furniture(),
                "{pod:?}/{wp:?}: pod + waypoint twins must share one Furniture row",
            );
        }
    }

    #[test]
    fn the_seat_scan_steps_through_a_dead_edge_but_never_hops_a_second_obstacle() {
        let mut m = WalkableMask::new_open(40, 40);
        // Seat body rows 19-20, a 1-cell pocket at row 21 sealed on all four
        // sides (walkable, unreachable), a second body rows 22-23, floor beyond.
        m.mark_blocked(8, 19, 5, 2, 0);
        m.mark_blocked(8, 22, 5, 2, 0);
        m.mark_blocked(9, 21, 1, 1, 0);
        m.mark_blocked(11, 21, 1, 1, 0);
        let reach = ReachSet::from_mask(&m, Point { x: 2, y: 2 });
        let seat = Point { x: 10, y: 18 };
        assert!(m.is_walkable(10, 21), "the pocket cell is walkable");
        assert!(
            !reach.reaches(Point { x: 10, y: 21 }),
            "the sealed pocket must be coarse-unreachable, or the rig proves nothing"
        );
        assert_eq!(
            first_reachable_on_side(&m, &reach, seat, 0, 1),
            None,
            "the scan must stop at the blocked pixel ending the run, not reach the \
             far floor past a second obstacle"
        );
        assert_eq!(
            first_reachable_on_side(&m, &reach, seat, 0, -1),
            Some(Point { x: 10, y: 17 }),
            "an unobstructed side returns its first reachable cell"
        );
    }

    #[test]
    fn waypoint_kind_all_is_unique_and_complete() {
        use std::collections::HashSet;
        let set: HashSet<_> = WaypointKind::ALL.iter().copied().collect();
        assert_eq!(
            set.len(),
            WaypointKind::ALL.len(),
            "ALL contains duplicates"
        );
        // furniture_def's match is compiler-forced exhaustive; this count catches
        // a variant added there but not to ALL.
        assert_eq!(
            WaypointKind::ALL.len(),
            10,
            "a WaypointKind variant was added/removed — update ALL",
        );
    }
}
