use super::*;
use crate::layout::{SceneLayout, WallSegment};

fn make_layout() -> SceneLayout {
    SceneLayout::compute(160, 200, Some(4)).expect("layout fits")
}

#[test]
fn straight_line_when_unobstructed() {
    let l = make_layout();
    let overlay = OccupancyOverlay::new();
    let from = Point {
        x: l.corridor.x + 10,
        y: l.corridor.y + 2,
    };
    let to = Point {
        x: l.corridor.x + 60,
        y: l.corridor.y + 2,
    };
    let path = find_path(&l.walkable, &overlay, None, from, to).expect("path");
    assert!(path.len() >= 2);
    assert_eq!(path[0], from);
    assert_eq!(*path.last().unwrap(), to);
}

#[test]
fn simplify_collapses_collinear() {
    let pts = vec![
        Point { x: 0, y: 0 },
        Point { x: 4, y: 0 },
        Point { x: 8, y: 0 },
        Point { x: 12, y: 0 },
        Point { x: 12, y: 4 },
    ];
    let s = simplify_polyline(&WalkableMask::new_open(64, 64), pts);
    assert_eq!(s.len(), 3);
}

// Diagonal and off-origin on purpose: an axis-aligned or origin-anchored run
// zeroes the collinearity determinant's x-delta terms. 3 points also pins the
// `len < 3` early-return boundary.
#[test]
fn simplify_collapses_diagonal_collinear() {
    let pts = vec![
        Point { x: 1, y: 1 },
        Point { x: 3, y: 3 },
        Point { x: 5, y: 5 },
    ];
    assert_eq!(
        simplify_polyline(&WalkableMask::new_open(64, 64), pts).len(),
        2
    );
}

#[test]
fn simplify_keeps_genuine_corner() {
    let pts = vec![
        Point { x: 0, y: 0 },
        Point { x: 2, y: 0 },
        Point { x: 2, y: 2 },
    ];
    assert_eq!(
        simplify_polyline(&WalkableMask::new_open(64, 64), pts).len(),
        3
    );
}

#[test]
fn routes_around_meeting_room_wall() {
    let l = make_layout();
    let overlay = OccupancyOverlay::new();
    let from = l.home_desks[0].at;
    let pantry = l
        .waypoints
        .iter()
        .find(|w| w.kind == crate::layout::WaypointKind::Pantry)
        .expect("pantry wp")
        .pos;
    let path = find_path(&l.walkable, &overlay, None, from, pantry).expect("path");
    assert!(path.len() >= 3, "expected routed path, got {path:?}");
}

#[test]
fn vertical_wall_is_impassable_except_through_the_door() {
    // A vertical divider's `WALL_THICK_V` footprint at a bad 4-alignment splits
    // across two coarse cells, both staying "walkable" so A* threads STRAIGHT
    // THROUGH — which is why `WALL_ROUTING_MARGIN_X` widens the stamp.
    let l = make_layout();
    let overlay = OccupancyOverlay::new();
    let (wall_x, north) = l
        .room_walls
        .iter()
        .find_map(|w| match *w {
            WallSegment::Vertical { x, y0, .. } => Some((x, y0)),
            WallSegment::Horizontal { .. } => None,
        })
        .expect("layout has a vertical wall");
    // A y inside the wall body, near its top — clear of the mid door gap.
    let y = north + 3;
    let from = Point {
        x: wall_x.saturating_sub(12),
        y,
    };
    let to = Point { x: wall_x + 12, y };
    let path = find_path(&l.walkable, &overlay, None, from, to)
        .expect("rooms stay connected through the door gap");
    let direct = crate::physics::octile_distance(from, to);
    let routed: u32 = path
        .windows(2)
        .map(|w| crate::physics::octile_distance(w[0], w[1]))
        .sum();
    assert!(
        routed > direct * 2,
        "expected a detour around the wall (routed {routed} vs direct {direct}); \
         a near-direct path means A* crossed the wall. path={path:?}"
    );
}

/// Teleport guard (#22): a waypoint A\* can't reach on the coarse grid makes an
/// idle agent SNAP there — `find_path` returns None and `route()` falls back to
/// a straight `[from,to]` line through furniture.
///
/// The size list stays narrow ON PURPOSE: the blocked furniture CENTRE is only a
/// PROXY for the destination, and on some production floors a vending machine's
/// own coarse cell is under the `cell_walkable` floor while its APPROACH — the
/// cell the router actually aims at — routes fine. Widen
/// `placement_sweep::every_wander_destination_is_routable_from_its_desk`, which
/// rides the real contract, not this proxy.
#[test]
fn every_wander_waypoint_is_routable_on_the_coarse_grid() {
    use crate::layout::TEST_DEFAULT_DESKS;
    let overlay = OccupancyOverlay::new();
    let sizes = [
        (96u16, 70u16),
        (128, 80),
        (160, 120),
        (192, 160),
        (240, 160),
    ];
    for (w, h) in sizes {
        for seed in 0..5u64 {
            let Some(l) = SceneLayout::compute_with_seed(w, h, Some(TEST_DEFAULT_DESKS), seed)
            else {
                continue;
            };
            let origin = l.door_threshold;
            for wp in &l.waypoints {
                assert!(
                    find_path(&l.walkable, &overlay, None, origin, wp.pos).is_some(),
                    "seed {seed} {w}x{h}: {:?} at ({},{}) is unreachable on the coarse \
                     routing grid — an idle agent sent there would teleport",
                    wp.kind,
                    wp.pos.x,
                    wp.pos.y
                );
            }
        }
    }
}

#[test]
fn every_approach_point_is_routable_from_its_home_desk() {
    // Stronger than the test above, which uses the DOOR origin + the blocked
    // furniture CENTER and so can pass while a specific desk's chosen approach
    // side is unroutable. When NO allowed+reachable side exists `approach_point`
    // returns the `wp.pos` sentinel, which isn't a destination — excluded below.
    use crate::layout::TEST_DEFAULT_DESKS;
    use crate::layout::approach_point;
    let overlay = OccupancyOverlay::new();
    for (w, h) in [
        (96u16, 70u16),
        (128, 80),
        (160, 120),
        (192, 160),
        (240, 160),
    ] {
        for seed in 0..5u64 {
            let Some(l) = SceneLayout::compute_with_seed(w, h, Some(TEST_DEFAULT_DESKS), seed)
            else {
                continue;
            };
            for &crate::layout::HomeDesk { at: desk, .. } in &l.home_desks {
                for wp in &l.waypoints {
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
                        continue;
                    }
                    assert!(
                        find_path(&l.walkable, &overlay, None, desk, a).is_some(),
                        "{w}x{h} seed {seed}: {:?} approach_point {a:?} unroutable from \
                         desk {desk:?} — the agent would teleport",
                        wp.kind,
                    );
                }
            }
        }
    }
}

#[test]
fn reachset_never_claims_an_unroutable_cell() {
    // One-directional on purpose: false POSITIVES are the bug (approach_point
    // would select an unroutable side), while conservative false negatives at
    // coarse boundaries are fine — approach_point simply won't pick those.
    use crate::layout::TEST_DEFAULT_DESKS;
    let overlay = OccupancyOverlay::new();
    for (w, h) in [(160u16, 120u16), (200, 80), (96, 70)] {
        for seed in 0..3u64 {
            let Some(l) = SceneLayout::compute_with_seed(w, h, Some(TEST_DEFAULT_DESKS), seed)
            else {
                continue;
            };
            let door = l.door_threshold;
            let mut y = 0;
            while y < l.buf_h {
                let mut x = 0;
                while x < l.buf_w {
                    let p = Point { x, y };
                    if l.reachable.reaches(p) {
                        assert!(
                            find_path(&l.walkable, &overlay, None, door, p).is_some(),
                            "{w}x{h} seed {seed}: ReachSet claims {p:?} reachable but \
                             find_path can't route there from the door {door:?}",
                        );
                    }
                    x += 8;
                }
                y += 8;
            }
        }
    }
}

/// The AIMLESS wander branch is the one destination producer that never
/// consults `ReachSet`, so it can hand A\* a goal in a coarse-unroutable pocket
/// and walk the agent through furniture for the whole leg.
///
/// Swept over the PRODUCTION floor seeds with `max_desks: None` deliberately —
/// capping the desk count hides the very floors that fail.
#[test]
fn every_aimless_wander_destination_is_routable_from_its_home_desk() {
    use crate::floor::floor_seed;
    use crate::pose::{aimless_wander_seed, desk_leg_endpoint, pick_aimless_dest};
    use pixtuoid_core::AgentId;
    use pixtuoid_core::state::MAX_FLOORS;

    let overlay = OccupancyOverlay::new();
    for (w, h) in [
        (96u16, 70u16),
        (120, 96),
        (160, 120),
        (192, 158),
        (240, 160),
    ] {
        for floor in 0..MAX_FLOORS {
            let Some(l) = SceneLayout::compute_with_seed(w, h, None, floor_seed(floor)) else {
                continue;
            };
            let origins: Vec<Point> = [l.home_desks.first(), l.home_desks.last()]
                .into_iter()
                .flatten()
                .map(|&desk| (desk, desk_leg_endpoint(desk, &l).0))
                .map(|(_, origin)| origin)
                .collect();
            for &desk in [l.home_desks.first(), l.home_desks.last()]
                .into_iter()
                .flatten()
            {
                for agent in 0..32u32 {
                    let id = AgentId::from_parts("probe", &format!("agent-{agent}"));
                    for cycle in 0..8u64 {
                        let seed = aimless_wander_seed(id, cycle);
                        let dest = pick_aimless_dest(&l, seed, desk);
                        if dest == crate::layout::desk_walk_anchor_facing(desk.at, desk.facing) {
                            continue; // `pick_aimless_dest`'s last resort: the agent's own seat
                        }
                        for &origin in &origins {
                            assert!(
                                find_path(&l.walkable, &overlay, None, origin, dest).is_some(),
                                "{w}x{h} floor {floor}: aimless dest {dest:?} unroutable from \
                                 desk approach {origin:?} — the leg degrades to a straight line \
                                 through furniture",
                            );
                        }
                    }
                }
            }
        }
    }
}

/// The cache is observed through a SEALED mask: on a fully-blocked grid
/// `find_path` returns `None` and `route` mints the 2-point `[from, to]`
/// fallback, so a multi-point answer proves the cached polyline was served
/// instead of a fresh A* run. A `router.len()` assertion cannot do this —
/// `route`'s miss arm re-`insert`s the SAME key, so it is green either way. The
/// mask is a sound probe because the cache is deliberately mask-BLIND: it keys
/// on `(from, to)` and invalidates on the OVERLAY signature.
#[test]
fn router_serves_the_cached_path_instead_of_re_running_astar() {
    let l = make_layout();
    let mut router = AStarRouter::new();
    let overlay = OccupancyOverlay::new();
    let from = Point { x: 30, y: 80 };
    let to = Point { x: 30, y: 120 };

    let first = router.route(&l.walkable, &overlay, from, to);
    assert!(
        first.len() > 2,
        "the fixture must CORNER, else a hit is indistinguishable from the \
         straight-line fallback: {first:?}"
    );

    let sealed = WalkableMask::filled(l.walkable.width(), l.walkable.height(), false);
    let second = router.route(&sealed, &overlay, from, to);
    assert_eq!(
        second, first,
        "a repeat leg must be served from the cache — a re-route on the sealed \
         mask could only yield the 2-point fallback"
    );
}

/// Both directions ride the sealed-mask oracle. The MISS direction is what pins
/// the optimisation itself (`retain`, not `clear`): swapping the per-path retain
/// for a global wipe passes every other test in the crate.
#[test]
fn an_overlay_evicts_only_the_paths_it_actually_crosses() {
    let l = make_layout();
    let mut router = AStarRouter::new();
    let mut overlay = OccupancyOverlay::new();
    let from = Point { x: 30, y: 80 };
    let to = Point { x: 30, y: 120 };
    let sealed = WalkableMask::filled(l.walkable.width(), l.walkable.height(), false);

    let first = router.route(&l.walkable, &overlay, from, to);
    assert!(first.len() > 2, "fixture must corner: {first:?}");

    // Far from the polyline: changes the overlay SIGNATURE (so the invalidation
    // branch runs) but crosses nothing.
    overlay.add(0, 0, 8, 8);
    assert_eq!(
        router.route(&sealed, &overlay, from, to),
        first,
        "a rect clear of the polyline must leave its entry cached — `retain`, not `clear`"
    );

    let mid = first[first.len() / 2];
    overlay.add(mid.x.saturating_sub(4), mid.y.saturating_sub(4), 8, 8);
    assert_eq!(
        router.route(&sealed, &overlay, from, to),
        vec![from, to],
        "the crossed entry must be evicted and re-routed, not served stale"
    );
}

#[test]
fn path_cache_is_bounded_and_still_routes_after_the_clear() {
    // Aimless wander destinations + snap-back/exit origins mint ever-new
    // (from, to) keys, so without the cap the cache (and the per-overlay retain
    // scan over it) grows without bound in an always-on office.
    let mask = WalkableMask::new_open(400, 400);
    let overlay = OccupancyOverlay::new();
    let mut router = AStarRouter::new();
    // On a cell-center (x % 4 == 2) so same-row routes collapse to the
    // straight 2-point polyline.
    let from = Point { x: 10, y: 50 };
    // Strictly more distinct pairs than the cap holds, so the overflow clear
    // provably fires at least once.
    let distinct_routes = PATH_CACHE_CAP + 100;
    for i in 0..distinct_routes {
        let to = Point {
            x: (8 + (i % 90) * 4) as u16,
            y: (8 + (i / 90) * 4) as u16,
        };
        let _ = router.route(&mask, &overlay, from, to);
        assert!(
            router.len() <= PATH_CACHE_CAP,
            "cache must stay bounded: {} entries after {} distinct routes",
            router.len(),
            i + 1
        );
    }
    let to = Point { x: 90, y: 50 };
    assert_eq!(
        router.route(&mask, &overlay, from, to),
        vec![from, to],
        "post-clear routing must still return correct paths"
    );
}

#[test]
fn routes_around_dynamic_obstacle() {
    let mask = pixtuoid_core::walkable::WalkableMask::new_open(100, 100);
    let mut overlay = OccupancyOverlay::new();
    let from = Point { x: 10, y: 50 };
    let to = Point { x: 90, y: 50 };
    let baseline = find_path(&mask, &overlay, None, from, to).expect("baseline");
    assert_eq!(baseline.len(), 2, "open mask should yield straight line");

    overlay.add(40, 40, 20, 20);
    let detour = find_path(&mask, &overlay, None, from, to).expect("detour");
    assert!(
        detour.len() > 2,
        "detour must add at least one corner around the dynamic block, got {detour:?}"
    );
}

#[test]
fn path_clear_under_empty_overlay_always_true() {
    let overlay = OccupancyOverlay::new();
    let path = vec![Point { x: 0, y: 0 }, Point { x: 100, y: 100 }];
    assert!(path_clear_under(&path, &overlay));
}

#[test]
fn path_clear_under_blocked_returns_false() {
    let mut overlay = OccupancyOverlay::new();
    overlay.add(50, 50, 10, 10);
    let path = vec![Point { x: 0, y: 0 }, Point { x: 100, y: 100 }];
    assert!(!path_clear_under(&path, &overlay));
}

#[test]
fn snap_to_walkable_returns_cell_when_already_walkable() {
    let l = make_layout();
    let overlay = OccupancyOverlay::new();
    let corridor = l.corridor;
    let cell_w = l.buf_w / 4;
    let cell_h = l.buf_h / 4;
    let cx = (corridor.x + corridor.width / 2) / 4;
    let cy = (corridor.y + corridor.height / 2) / 4;
    let result = snap(
        &l.walkable,
        &overlay,
        (cx, cy),
        cell_w,
        cell_h,
        MAX_SNAP_RADIUS,
    );
    assert_eq!(result, Some((cx, cy)));
}

#[test]
fn snap_to_walkable_finds_nearby_cell_when_blocked() {
    let l = make_layout();
    let cell_w = l.buf_w / 4;
    let cell_h = l.buf_h / 4;
    let wall_cell_y = l.top_margin / CELL_SIZE;
    let result = snap(
        &l.walkable,
        &OccupancyOverlay::new(),
        (0, wall_cell_y),
        cell_w,
        cell_h,
        MAX_SNAP_RADIUS,
    );
    assert!(result.is_some(), "should snap to a nearby walkable cell");
}

#[test]
fn heuristic_straight_horizontal() {
    assert_eq!(heuristic((5, 5), (8, 5)), 30);
}

#[test]
fn heuristic_diagonal_uses_octile() {
    let h = heuristic((0, 0), (2, 2));
    assert_eq!(h, 28);
}

#[test]
fn cell_of_maps_pixel_to_cell() {
    assert_eq!(cell_of(Point { x: 0, y: 0 }), (0, 0));
    assert_eq!(cell_of(Point { x: 7, y: 11 }), (1, 2));
    assert_eq!(cell_of(Point { x: 4, y: 4 }), (1, 1));
}

#[test]
fn cell_center_is_midpoint_of_cell() {
    let c = cell_center(0, 0);
    assert_eq!(c, Point { x: 2, y: 2 });
    let c = cell_center(3, 5);
    assert_eq!(c, Point { x: 14, y: 22 });
}

#[test]
fn cell_in_zone_false_when_none() {
    assert!(!cell_in_zone(None, 5, 5));
}

// A cell center landing EXACTLY on the exclusive far edge (x+width / y+height)
// is OUTSIDE — the bound is a strict `<`. The zone edge is derived from
// `cell_center` so the alignment holds regardless of CELL_SIZE. Without this,
// a `<`->`<=` mutation is invisible (no on-edge cell is ever tested).
#[test]
fn cell_in_zone_false_on_exclusive_edges() {
    let right_edge = cell_center(2, 1).x;
    let zone_x = Bounds {
        x: 0,
        y: 0,
        width: right_edge,
        height: 40,
    };
    assert!(!cell_in_zone(Some(zone_x), 2, 1));

    let bottom_edge = cell_center(1, 2).y;
    let zone_y = Bounds {
        x: 0,
        y: 0,
        width: 40,
        height: bottom_edge,
    };
    assert!(!cell_in_zone(Some(zone_y), 1, 2));
}

// Inside on ONE axis but outside on the other is OUTSIDE — the four bounds
// are AND-joined. The both-axes-outside test above leaves an `&&`->`||`
// mutation on the middle joins invisible (F||F is still F); a single-axis
// miss (T on one pair, F on the other) is what makes the conjunction observable.
#[test]
fn cell_in_zone_false_when_outside_on_one_axis_only() {
    let zone = Bounds {
        x: 0,
        y: 0,
        width: 10,
        height: 10,
    };
    assert!(!cell_in_zone(Some(zone), 20, 1)); // outside x, inside y
    assert!(!cell_in_zone(Some(zone), 1, 20)); // inside x, outside y
}

// The complement of the exclusive-edge test: a cell center landing EXACTLY
// on the INCLUSIVE near edge (x / y) is INSIDE — the lower bound is `>=`.
// Without this a `>=`->`>` mutation on either lower bound survives (the
// mirror of the `<`->`<=` gap above; sibling-set-spans-axes).
#[test]
fn cell_in_zone_true_on_inclusive_lower_edges() {
    let near = cell_center(1, 1);
    let zone = Bounds {
        x: near.x,
        y: near.y,
        width: 40,
        height: 40,
    };
    assert!(cell_in_zone(Some(zone), 1, 1));
}

#[test]
fn find_path_returns_none_when_target_completely_surrounded() {
    // The mask is oversized so the wall around (100,100) can't saturate to
    // origin and cover `from` too: `snap` must succeed on `from`, fail on the goal.
    let mask = WalkableMask::new_open(200, 200);
    let mut overlay = OccupancyOverlay::new();
    let target = Point { x: 100, y: 100 };
    let wall_size = (MAX_SNAP_RADIUS + 1) * CELL_SIZE * 2;
    let wall_origin = 100u16 - wall_size / 2;
    overlay.add(wall_origin, wall_origin, wall_size, wall_size);

    let from = Point { x: 4, y: 4 };
    let result = find_path(&mask, &overlay, None, from, target);
    assert!(
        result.is_none(),
        "completely surrounded target should return None, got {result:?}"
    );
}

#[test]
fn transient_no_path_fallback_is_not_served_from_the_cache() {
    // `path_clear_under` checks only the OVERLAY, never the static mask, so a
    // cached wall-crossing fallback would survive every retain() and the agent
    // would walk through the wall on every future leg.
    let mut mask = WalkableMask::new_open(80, 48);
    // Blocked strip x ∈ [36, 44) for y ∈ [0, 32); gap open at y ∈ [32, 48).
    mask.mark_blocked(36, 0, 8, 32, 0);
    let from = Point { x: 10, y: 10 };
    let to = Point { x: 70, y: 10 };

    let open = find_path(&mask, &OccupancyOverlay::new(), None, from, to).expect("gap routes");
    assert!(
        open.len() > 2,
        "expected a detour via the gap, got {open:?}"
    );

    let mut router = AStarRouter::new();
    let mut blocked = OccupancyOverlay::new();
    blocked.add(36, 32, 8, 16); // close the gap → no path at all
    assert_eq!(
        router.route(&mask, &blocked, from, to),
        vec![from, to],
        "with the gap closed the router falls back to the straight line"
    );

    let recovered = router.route(&mask, &OccupancyOverlay::new(), from, to);
    assert!(
        recovered.len() > 2,
        "the transient no-path fallback must not be cached; got {recovered:?}"
    );
}

#[test]
fn snap_point_to_walkable_returns_walkable_cell() {
    let l = make_layout();
    // A point inside a desk footprint (blocked, with obstacle pad).
    let desk = l.home_desks[0].at;
    let blocked_p = Point {
        x: desk.x + 4,
        y: desk.y + 2,
    };
    let snapped =
        snap_point_to_walkable(&l.walkable, blocked_p).expect("blocked desk should snap nearby");
    assert!(
        l.walkable.is_walkable(snapped.x, snapped.y),
        "snapped point ({},{}) must be walkable",
        snapped.x,
        snapped.y
    );
    let c = l.corridor;
    let open_p = Point {
        x: c.x + c.width / 2,
        y: c.y + c.height / 2,
    };
    let open = snap_point_to_walkable(&l.walkable, open_p).expect("corridor center snaps");
    assert!(
        l.walkable.is_walkable(open.x, open.y),
        "open-floor snap walkable"
    );
}

#[test]
fn degenerate_grid_returns_fallbacks() {
    // Sub-CELL_SIZE on both axes, so `grid_dims` is None.
    let mask = WalkableMask::new_open(3, 3);
    let overlay = OccupancyOverlay::new();
    let a = Point { x: 0, y: 0 };
    let b = Point { x: 2, y: 2 };
    assert_eq!(
        find_path(&mask, &overlay, None, a, b),
        Some(vec![a, b]),
        "degenerate grid must fall back to the straight [from,to]"
    );
    assert!(
        !point_in_walkable_cell(&mask, a),
        "degenerate grid: no point is in a walkable cell"
    );
}

#[test]
fn snap_to_walkable_skips_out_of_bounds_corner_neighbours() {
    // Blocking the bottom-right CORNER cell makes the expanding ring poke PAST
    // the grid's far edge, forcing the out-of-range `continue` before it lands
    // on an interior walkable cell.
    let mut mask = WalkableMask::new_open(40, 40);
    let overlay = OccupancyOverlay::new();
    let (cell_w, cell_h) = grid_dims(&mask).expect("non-degenerate");
    let corner_px = ((cell_w - 1) * CELL_SIZE, (cell_h - 1) * CELL_SIZE);
    mask.mark_blocked(corner_px.0, corner_px.1, CELL_SIZE, CELL_SIZE, 0);

    let result = snap(
        &mask,
        &overlay,
        (cell_w - 1, cell_h - 1),
        cell_w,
        cell_h,
        MAX_SNAP_RADIUS,
    );
    assert!(
        result.is_some(),
        "snap from the corner must still find an interior walkable cell"
    );
}

#[test]
fn find_path_none_when_two_regions_split_by_a_full_wall() {
    // Both endpoints snap successfully, so the None comes from the A* loop
    // EXHAUSTING the open set — distinct from the goal-snap-fails None.
    let mut mask = WalkableMask::new_open(80, 40);
    let overlay = OccupancyOverlay::new();
    // Two fully-blocked cell columns, impassable to the coarse diagonal stepper.
    mask.mark_blocked(36, 0, 8, 40, 0);

    let from = Point { x: 10, y: 20 };
    let to = Point { x: 70, y: 20 };
    assert!(point_in_walkable_cell(&mask, from));
    assert!(point_in_walkable_cell(&mask, to));

    assert!(
        find_path(&mask, &overlay, None, from, to).is_none(),
        "a wall with no gap must leave the two regions unconnected (loop exhausts → None)"
    );
}

#[test]
fn octile_cost_is_the_shared_diag_straight_formula() {
    // The ONE formula the A* heuristic and `physics::octile_distance` both call.
    assert_eq!(
        octile_cost(3, 5),
        OCTILE_DIAGONAL_COST * 3 + OCTILE_STRAIGHT_COST * 2
    );
    assert_eq!(octile_cost(5, 3), octile_cost(3, 5), "symmetric in dx/dy");
    assert_eq!(
        octile_cost(0, 4),
        OCTILE_STRAIGHT_COST * 4,
        "pure orthogonal"
    );
    assert_eq!(octile_cost(4, 4), OCTILE_DIAGONAL_COST * 4, "pure diagonal");
    let a = crate::layout::Point { x: 2, y: 7 };
    let b = crate::layout::Point { x: 9, y: 3 };
    assert_eq!(crate::physics::octile_distance(a, b), octile_cost(7, 4));
}

#[test]
fn snap_lands_on_an_open_pixel_when_the_cell_centre_itself_is_blocked() {
    // The coarse grid calls a cell walkable at `COARSE_CELL_WALKABLE_MIN` of its
    // 16 px open, so the cell it snaps to can have a BLOCKED centre.
    let mut mask = pixtuoid_core::walkable::WalkableMask::new_open(64, 64);
    let cell = (4u16, 4u16);
    let centre = cell_center(cell.0, cell.1);
    // 2x2 leaves 12 of the cell's 16 px open — walkable to the coarse grid, centre blocked.
    mask.mark_blocked(centre.x, centre.y, 2, 2, 0);
    assert!(
        !mask.is_walkable(centre.x, centre.y),
        "fixture must block the cell centre, else this pins nothing"
    );
    assert!(
        cell_walkable(&mask, &OccupancyOverlay::new(), cell.0, cell.1),
        "fixture must leave the cell walkable to the COARSE grid, else snap skips it"
    );

    let snapped = snap_point_to_walkable(&mask, centre).expect("an open pixel exists in the cell");
    assert!(
        mask.is_walkable(snapped.x, snapped.y),
        "snap must return a point that passes the predicate its name promises: \
         {snapped:?}"
    );
}

#[test]
fn a_route_turns_on_the_open_pixel_nearest_a_blocked_centre() {
    let mut mask = pixtuoid_core::walkable::WalkableMask::new_open(64, 64);
    let cell = (4u16, 4u16);
    let centre = cell_center(cell.0, cell.1);
    // The cell's south half: still walkable to the coarse grid, centre inside.
    mask.mark_blocked(centre.x - 2, centre.y, 4, 2, 0);
    assert!(cell_walkable(
        &mask,
        &OccupancyOverlay::new(),
        cell.0,
        cell.1
    ));
    assert_eq!(
        cell_anchor(&mask, cell.0, cell.1),
        Point {
            x: centre.x,
            y: centre.y - 1
        },
        "one step north of the centre is the nearest open pixel"
    );
    mask.mark_walkable(centre.x, centre.y, 1, 1);
    assert_eq!(
        cell_anchor(&mask, cell.0, cell.1),
        centre,
        "an open centre is kept"
    );
}

/// At 127x45 the anchors of two half-open cells beside a door's wall stub
/// share a column the stub stands in, so neither the straight segment nor an L
/// clears it; the route steps through the one-pixel gap instead.
#[test]
fn a_route_through_a_one_pixel_gap_stays_on_open_floor() {
    let l = SceneLayout::compute_with_seed(127, 45, None, 0).expect("127x45 lays out");
    let (from, to) = (Point { x: 55, y: 31 }, Point { x: 33, y: 39 });
    let path = find_path(&l.walkable, &OccupancyOverlay::new(), None, from, to).expect("routable");
    let off: Vec<Point> = path
        .windows(2)
        .flat_map(|segment| crate::physics::segment_pixels(segment[0], segment[1]))
        .filter(|p| !l.walkable.is_walkable(p.x, p.y))
        .collect();
    assert_eq!(off, [], "{path:?}");
}
