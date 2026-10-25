use super::*;

#[test]
fn bounds_overlap_is_half_open_and_a_zero_sized_box_overlaps_nothing() {
    let b = |x, y, width, height| Bounds {
        x,
        y,
        width,
        height,
    };
    let a = b(10, 10, 4, 4);
    assert!(a.overlaps(b(13, 13, 4, 4)), "one shared pixel");
    assert!(!a.overlaps(b(14, 10, 4, 4)), "touching edges share none");
    assert!(!a.overlaps(b(11, 11, 0, 2)), "zero width, inside");
    assert!(!b(11, 11, 2, 0).overlaps(a), "zero height, inside");
}

/// A cell on the far edge, which `CellArea` saturates to `u16::MAX`, ends one
/// past the last representable column and row.
#[test]
fn a_box_on_the_far_edge_overlaps_by_its_pixels() {
    let b = |x, y, width, height| Bounds {
        x,
        y,
        width,
        height,
    };
    let edge = b(u16::MAX, u16::MAX, 1, 1);
    let near = b(u16::MAX - 1, u16::MAX - 1, 2, 2);
    assert!(edge.overlaps(near));
    assert!(near.overlaps(edge));
    assert!(!edge.overlaps(b(0, 0, 4, 4)));
}

/// The union holds both boxes, overlapping or apart, and a box on the far
/// edge neither overflows it nor wraps it.
#[test]
fn a_union_holds_both_boxes_to_the_far_edge() {
    let b = |x, y, width, height| Bounds {
        x,
        y,
        width,
        height,
    };
    assert_eq!(b(10, 10, 4, 4).union(b(13, 2, 4, 4)), b(10, 2, 7, 12));
    assert_eq!(b(0, 0, 1, 1).union(b(5, 5, 1, 1)), b(0, 0, 6, 6));
    let edge = b(u16::MAX, u16::MAX, 1, 1);
    assert_eq!(
        edge.union(b(u16::MAX - 3, u16::MAX - 3, 2, 2)),
        b(u16::MAX - 3, u16::MAX - 3, 4, 4)
    );
}

#[test]
fn widening_against_column_0_keeps_the_east_edge() {
    let b = |x, width| Bounds {
        x,
        y: 5,
        width,
        height: 3,
    };
    assert_eq!(b(4, 6).widened(2), b(2, 10));
    assert_eq!(b(0, 6).widened(2), b(0, 8), "the east edge stays at 6 + 2");
    assert_eq!(b(1, 6).widened(2), b(0, 9));
}

#[test]
fn kitchen_island_places_on_roomy_pantries_and_refuses_small() {
    let l = SceneLayout::compute_with_seed(240, 160, None, 2).expect("fits");
    let island = l
        .pantry
        .and_then(|p| p.kitchen_island)
        .expect("84-wide Senior pantry hosts the island");
    let pr = l.pantry.map(|p| p.bounds).expect("pantry");
    assert!(island.x >= pr.x && island.x < pr.x + pr.width);
    let stands: Vec<_> = l
        .waypoints
        .iter()
        .filter(|w| matches!(w.kind, WaypointKind::Island))
        .collect();
    assert_eq!(stands.len(), 4, "E + W flanks + 2 bartenders");
    for s in &stands {
        assert!(
            s.pos.x >= pr.x && s.pos.x < pr.x + pr.width && s.pos.y >= pr.y,
            "stand {:?} must stay in the pantry",
            s.pos
        );
    }
    let mut bartenders: Vec<Point> = stands
        .iter()
        .filter(|s| s.facing == Facing::South)
        .map(|s| s.pos)
        .collect();
    bartenders.sort_by_key(|p| p.x);
    let quarter = furniture_def(Furniture::KitchenIsland).visual.w / 4;
    assert_eq!(
        bartenders,
        vec![
            Point {
                x: island.x - quarter,
                y: island.y,
            },
            Point {
                x: island.x + quarter,
                y: island.y,
            },
        ],
        "two South-facing bartender slots at the island's quarter points"
    );
    // Small Standard floor: a 26-wide pantry, under the ≥42 the island + stands +
    // clearances need.
    let s = SceneLayout::compute_with_seed(96, 70, None, 0).expect("fits");
    assert_eq!(
        s.pantry.and_then(|p| p.kitchen_island),
        None,
        "26-wide pantry refuses the island"
    );
    assert!(
        !s.waypoints
            .iter()
            .any(|w| matches!(w.kind, WaypointKind::Island)),
        "no island ⇒ no island stands"
    );
}

#[test]
fn meeting_room_donates_surplus_height_to_the_pantry() {
    // The drift guard pinning `pantry_content_h`'s inverse-clamp math to the island
    // block's real clamps: if either side changes alone, the island vanishes here.
    let l = SceneLayout::compute_with_seed(215, 98, None, 0).expect("fits");
    assert!(
        l.pantry.and_then(|p| p.kitchen_island).is_some(),
        "island must place on the short floor after the height transfer"
    );
    assert!(
        l.waypoints
            .iter()
            .any(|w| matches!(w.kind, WaypointKind::SnackShelf)),
        "snack shelf must place too (its y-bound == the island's)"
    );
    let mr = l.meeting_room_bounds(0).expect("meeting room");
    let pr = l.pantry.map(|p| p.bounds).expect("pantry");
    assert!(
        mr.height < pr.height,
        "the surplus flowed south: meeting {} !< pantry {}",
        mr.height,
        pr.height
    );
    let tall = SceneLayout::compute_with_seed(240, 160, None, 0).expect("fits");
    let (tmr, tpr) = (
        tall.meeting_room_bounds(0).expect("meeting room"),
        tall.pantry.map(|p| p.bounds).expect("pantry"),
    );
    let usable = tmr.height + tpr.height;
    assert_eq!(
        tmr.height,
        usable / 2,
        "tall floors keep the old half-split exactly"
    );
    // Deliberately asymmetric: if the island's clamps RELAX while pantry_content_h
    // stays, the split merely over-donates a benign row or two — only the
    // starve/rescue boundary is load-bearing.
    let short = SceneLayout::compute_with_seed(215, 87, None, 0).expect("fits");
    assert_eq!(
        short.pantry.and_then(|p| p.kitchen_island),
        None,
        "below the donation floor the pantry stays starved (no forced cram)"
    );
    let (smr, spr) = (
        short.meeting_room_bounds(0).expect("meeting room"),
        short.pantry.map(|p| p.bounds).expect("pantry"),
    );
    assert_eq!(
        smr.height,
        u16::midpoint(smr.height, spr.height),
        "below the floor the half-split stands"
    );
}

#[test]
fn island_bartenders_approach_from_behind_never_through_the_front() {
    // 240×160 = the roomy tall floor; 215×98 = the donated content-fit floor where
    // the island sits at its single valid y (the tightest lane).
    let mut exercised = false;
    for (bw, bh) in [(240u16, 160u16), (215, 98)] {
        for seed in 0..5u64 {
            let l = SceneLayout::compute_with_seed(bw, bh, None, seed).expect("fits");
            let Some(island) = l.pantry.and_then(|p| p.kitchen_island) else {
                continue;
            };
            exercised = true;
            let origin = l.home_desks.first().expect("desks").at;
            for wp in l
                .waypoints
                .iter()
                .filter(|w| matches!(w.kind, WaypointKind::Island) && w.facing == Facing::South)
            {
                let a = approach_point(
                    wp.kind.furniture(),
                    wp.pos,
                    wp.facing,
                    l.pantry_counter_size(),
                    &l.walkable,
                    origin,
                    &l.reachable,
                );
                assert_ne!(
                    a, wp.pos,
                    "{bw}x{bh} seed {seed}: bartender slot has no approach"
                );
                assert!(
                    a.y <= island.y,
                    "{bw}x{bh} seed {seed}: approach {a:?} is south of the island \
                     center — the glide would cross the counter's front face"
                );
            }
        }
    }
    assert!(
        exercised,
        "no size/seed hosted the island — test lost its teeth"
    );
}

#[test]
fn snack_shelf_hugs_the_west_wall_and_refuses_narrow_rooms() {
    let l = SceneLayout::compute_with_seed(240, 160, None, 2).expect("fits");
    let pr = l.pantry.map(|p| p.bounds).expect("pantry");
    let shelf = l
        .waypoints
        .iter()
        .find(|w| matches!(w.kind, WaypointKind::SnackShelf))
        .expect("roomy pantry hosts the shelf");
    let vis = furniture_def(Furniture::SnackShelf).visual;
    assert_eq!(shelf.pos.x, pr.x + 1 + vis.w / 2, "west-wall hug");
    // Seed 11 at the narrowest buffer leaves the pantry under the shelf's `vis.w + 4`
    // width gate — the gate is a property of size AND seed, not of size alone.
    let s = SceneLayout::compute_with_seed(crate::layout::compute::MIN_LAYOUT_W, 100, None, 11)
        .expect("fits");
    assert!(
        !s.waypoints
            .iter()
            .any(|w| matches!(w.kind, WaypointKind::SnackShelf)),
        "a pantry under the shelf's width gate must refuse it"
    );
}

#[test]
fn dense_inter_meeting_wall_is_solid_with_a_corridor_door_each() {
    // #557 door policy (owner call): two stacked meeting rooms do NOT interconnect
    // — their shared wall renders as ONE solid segment — while each room keeps its
    // own centered corridor door in the east wall.
    let mut saw_dual = false;
    for seed in 0..10u64 {
        let l = SceneLayout::compute_with_seed(192, 160, Some(8), seed).expect("fits");
        if l.meeting_rooms.len() < 2 {
            continue;
        }
        saw_dual = true;
        let split_y = l.meeting_rooms[1].bounds.y;
        let h: Vec<_> = l
            .room_walls
            .iter()
            .filter_map(|w| match *w {
                WallSegment::Horizontal { y, x0, x1 } if y == split_y => Some((x0, x1)),
                _ => None,
            })
            .collect();
        assert_eq!(h.len(), 1, "seed {seed}: ONE solid shared wall, got {h:?}");
        assert_eq!(
            h[0],
            (0, l.meeting_rooms[1].bounds.width),
            "seed {seed}: no inter-meeting door gap"
        );
        for (id, room) in l.meeting_rooms.iter().enumerate() {
            let b = room.bounds;
            let vx = b.x + b.width;
            let mut v: Vec<_> = l
                .room_walls
                .iter()
                .filter_map(|w| match *w {
                    WallSegment::Vertical { x, y0, y1 }
                        if x == vx && y0 >= b.y && y1 <= b.y + b.height =>
                    {
                        Some((y0, y1))
                    }
                    _ => None,
                })
                .collect();
            v.sort_unstable();
            assert_eq!(
                v.len(),
                2,
                "seed {seed} room {id}: east wall split by its door"
            );
            assert!(
                v[0].1 < v[1].0,
                "seed {seed} room {id}: the corridor door gap must be real"
            );
        }
    }
    assert!(saw_dual, "192x160 seeds 0..10 must reach a dual floor");
}

#[test]
fn meeting_rooms_vec_indexes_are_the_waypoint_room_ids() {
    let mut saw_dual = false;
    for seed in 0..10u64 {
        let l = SceneLayout::compute_with_seed(192, 160, Some(8), seed).expect("fits");
        assert_eq!(
            l.meeting_room_bounds(l.meeting_rooms.len()),
            None,
            "no room past the Vec"
        );
        for wp in l.waypoints.iter().filter(|w| {
            matches!(
                w.kind,
                WaypointKind::MeetingSofa | WaypointKind::MeetingChair
            )
        }) {
            let id = wp.room_id.expect("meeting slots carry a room_id");
            let b = l
                .meeting_room_bounds(id)
                .unwrap_or_else(|| panic!("seed {seed}: room_id {id} has no room"));
            assert!(
                l.meeting_rooms[id].trio.is_some(),
                "seed {seed}: room {id} emits waypoints, so it must host its trio"
            );
            assert!(
                wp.pos.x >= b.x
                    && wp.pos.x < b.x + b.width
                    && wp.pos.y >= b.y
                    && wp.pos.y < b.y + b.height,
                "seed {seed}: waypoint {:?} (room {id}) outside its room {b:?}",
                wp.pos
            );
        }
        if l.meeting_rooms.len() == 2 {
            saw_dual = true;
            let r2 = l.meeting_rooms[1].bounds;
            if let Some(mf) = l.meeting_rooms[1].trio {
                for p in mf.sofas.iter().chain([&mf.table]) {
                    assert!(
                        p.x >= r2.x
                            && p.x < r2.x + r2.width
                            && p.y >= r2.y
                            && p.y < r2.y + r2.height,
                        "seed {seed}: room-1 furniture {p:?} must sit inside its own room {r2:?}"
                    );
                }
            }
        }
    }
    assert!(
        saw_dual,
        "192x160 x seeds 0..10 must reach a dual-meeting Dense floor"
    );
}

#[test]
fn desk_is_walk_behind_the_monitor() {
    let l = SceneLayout::compute(200, 90, Some(64)).expect("fits");
    let vis = crate::layout::desk_furniture_def().visual;
    let foot_h = crate::layout::DESK_FOOT_H;
    for &d in &l.home_desks {
        let cx = d.at.x + vis.w / 2;
        assert!(
            l.walkable.is_walkable(cx, d.at.y),
            "desk {d:?}: north row (monitor overhang) must be walkable (walk-behind)"
        );
        let south = d.at.y + vis.h.saturating_sub(1);
        assert!(
            !l.walkable.is_walkable(cx, south.saturating_sub(foot_h / 2)),
            "desk {d:?}: front-contact ground must be blocked"
        );
    }
}

#[test]
fn partial_bottom_row_caps_mid_fill_when_agents_run_out() {
    // Sizes chosen empirically: each has a 2-desk partial bottom row whose first
    // desk appears at num_agents = cap-1 and second at cap, so cap-1 breaks the
    // `'partial_y` loop MID-ROW. A smaller num_agents caps in the earlier full-pod
    // phase instead and never reaches it.
    for (w, h, cap) in [(88u16, 100u16, 6usize), (88, 125, 8), (88, 160, 10)] {
        let full = SceneLayout::compute_with_seed(w, h, Some(TEST_DEFAULT_DESKS), 0).expect("fits");
        assert_eq!(
            full.home_desks.len(),
            cap,
            "{w}x{h}: expected total desk capacity {cap}"
        );
        let max_y = full.home_desks.iter().map(|d| d.at.y).max().unwrap();
        let band_at = |n: usize| {
            SceneLayout::compute_with_seed(w, h, Some(n), 0)
                .expect("fits")
                .home_desks
                .iter()
                .filter(|d| d.at.y == max_y)
                .count()
        };
        for n in 1..=cap {
            assert_eq!(
                SceneLayout::compute_with_seed(w, h, Some(n), 0)
                    .expect("fits")
                    .home_desks
                    .len(),
                n,
                "{w}x{h}: num_agents {n} must truncate to exactly {n} desks"
            );
        }
        assert_eq!(
            band_at(cap),
            2,
            "{w}x{h}: partial bottom row seats 2 at cap"
        );
        assert_eq!(
            band_at(cap - 1),
            1,
            "{w}x{h}: cap-1 leaves the partial row half-filled (break mid-row)"
        );
    }
}

#[test]
fn every_role_enum_variant_maps_to_a_furniture_row() {
    for &wd in WallDecor::ALL {
        let f = wd.furniture();
        assert!(
            furniture_def(f).visual.w > 0 && furniture_def(f).visual.h > 0,
            "{wd:?} → {f:?} must resolve a sized Furniture row"
        );
    }
    for &pk in PlantKind::ALL {
        let f = pk.furniture();
        let def = furniture_def(f);
        assert!(
            def.footprint.is_some(),
            "{pk:?} → {f:?} must have a ground footprint"
        );
        assert!(
            def.visual.w > 0 && def.visual.h > 0,
            "{pk:?} → {f:?} must have a sized visual"
        );
    }
}

#[test]
fn compute_does_not_overflow_on_huge_terminal() {
    for &seed in &[0u64, 1, 2, 3, 4] {
        // 4000×4000 px buffer → the percentage math's `buf_h * 30` = 120_000, well
        // past u16::MAX.
        let l = SceneLayout::compute_with_seed(4000, 4000, Some(TEST_DEFAULT_DESKS), seed);
        assert!(
            l.is_some(),
            "huge terminal (seed {seed}) must lay out, not overflow"
        );
    }
}

#[test]
fn none_fills_desks_past_the_old_cap_on_a_large_buffer() {
    let l = SceneLayout::compute_with_seed(800, 500, None, 0).expect("large buffer lays out");
    assert!(
        l.home_desks.len() > TEST_DEFAULT_DESKS,
        "None must fill past the old {TEST_DEFAULT_DESKS}-desk cap, got {}",
        l.home_desks.len()
    );
}

#[test]
fn compute_returns_none_at_exact_boundary() {
    let min_w = crate::layout::compute::MIN_LAYOUT_W;
    let min_h = crate::layout::compute::MIN_LAYOUT_H;
    assert!(
        SceneLayout::compute(min_w - 1, min_h, Some(1)).is_none(),
        "one pixel below MIN_LAYOUT_W should return None"
    );
    assert!(
        SceneLayout::compute(min_w, min_h - 1, Some(1)).is_none(),
        "one pixel below MIN_LAYOUT_H should return None"
    );
    assert!(
        SceneLayout::compute(min_w, min_h, Some(1)).is_some(),
        "exactly at boundary should return Some"
    );
}

/// The advertised minimum is a promise the painter repeats to the user, so it
/// has to be a size an agent can actually SIT at — the variants that take a wide
/// left column used to lay out an office with no desk in it.
#[test]
fn every_floor_variant_seats_a_desk_at_the_minimum_layout_size() {
    let min_w = crate::layout::compute::MIN_LAYOUT_W;
    let min_h = crate::layout::compute::MIN_LAYOUT_H;
    let mut narrowest_band = u16::MAX;
    let mut shortest_band = u16::MAX;
    for seed in 0..64u64 {
        let l = SceneLayout::compute_with_seed(min_w, min_h, None, seed)
            .unwrap_or_else(|| panic!("seed {seed}: {min_w}x{min_h} is the minimum, must lay out"));
        assert!(
            !l.home_desks.is_empty(),
            "seed {seed}: {min_w}x{min_h} lays out an office with no desk to seat anyone"
        );
        narrowest_band = narrowest_band.min(l.cubicle_band.width);
        shortest_band = shortest_band.min(l.cubicle_band.height);
    }
    // The width floor is the north wall's, wider than one desk's band needs:
    // `neither_floor_carries_a_safety_margin` holds it tight.
    assert!(
        narrowest_band >= crate::layout::compute::DESK_BAND_MIN_W,
        "the width floor leaves a band narrower than one desk needs"
    );
    assert_eq!(
        shortest_band,
        crate::layout::compute::DESK_BAND_MIN_H,
        "the height floor overshoots: the band is taller than one desk needs"
    );
}

#[test]
fn compute_zones_are_ordered_top_to_bottom_and_nonoverlapping() {
    let l = SceneLayout::compute(120, 80, Some(6)).expect("fits");
    assert!(l.cubicle_band.y < l.cubicle_aisle.y);
    let c_bot = l.cubicle_band.y + l.cubicle_band.height;
    assert!(c_bot <= l.cubicle_aisle.y, "cubicle overlaps cubicle_aisle");
    let w_bot = l.cubicle_aisle.y + l.cubicle_aisle.height;
    assert!(w_bot <= l.buf_h);
}

#[test]
fn narrow_width_desks_stay_inside_the_band_with_anchors_on_buffer() {
    // 34-66px-wide buffers force one pod column (`pod_cols` floors at 1) even when
    // the 36px pod doesn't fit — so the pod's 2nd desk column is the one that can
    // land past the band's right edge, or entirely off-buffer.
    for &w in &[40u16, 50, 60] {
        for seed in 0..6u64 {
            let Some(l) = SceneLayout::compute_with_seed(w, 70, Some(8), seed) else {
                continue;
            };
            let band_right = l.cubicle_band.x + l.cubicle_band.width;
            for d in &l.home_desks {
                assert!(
                    d.at.x >= l.cubicle_band.x,
                    "{w}x70 seed {seed}: desk x={} west of band x={}",
                    d.at.x,
                    l.cubicle_band.x
                );
                assert!(
                    d.at.x + DESK_W <= band_right,
                    "{w}x70 seed {seed}: desk x={} overflows the band's right edge {band_right}",
                    d.at.x
                );
                let a = desk_walk_anchor_facing(d.at, crate::layout::Facing::South);
                assert!(
                    a.x < l.buf_w && a.y < l.buf_h,
                    "{w}x70 seed {seed}: desk_walk_anchor_facing {a:?} is off-buffer ({}x{})",
                    l.buf_w,
                    l.buf_h
                );
            }
        }
    }
}

#[test]
fn compute_places_all_waypoint_kinds() {
    let l = SceneLayout::compute(120, 96, Some(1)).expect("fits");
    // Only Couch + Pantry are unconditional; the rest ride a random pod_decor pick.
    assert!(l.waypoints.len() >= 2);
    let kinds: std::collections::HashSet<_> = l.waypoints.iter().map(|w| w.kind).collect();
    assert!(kinds.contains(&WaypointKind::Couch));
    assert!(kinds.contains(&WaypointKind::Pantry));
    for w in &l.waypoints {
        match w.kind {
            WaypointKind::Pantry => {
                let pr = l.pantry.map(|p| p.bounds).expect("pantry");
                assert!(w.pos.y >= pr.y && w.pos.y < pr.y + pr.height);
                assert!(w.pos.x >= pr.x && w.pos.x < pr.x + pr.width);
            }
            WaypointKind::Couch => {
                assert!(w.pos.y >= l.top_margin);
                assert!(w.pos.y < l.cubicle_band.y + DESK_GAP_Y);
            }
            WaypointKind::Island | WaypointKind::SnackShelf => {
                let pr = l
                    .pantry
                    .map(|p| p.bounds)
                    .expect("pantry room hosts the new kinds");
                assert!(w.pos.y >= pr.y && w.pos.y < pr.y + pr.height);
                assert!(w.pos.x >= pr.x && w.pos.x < pr.x + pr.width);
            }
            WaypointKind::PhoneBooth | WaypointKind::StandingDesk => {
                assert!(w.pos.y >= l.top_margin);
            }
            WaypointKind::VendingMachine | WaypointKind::Printer => {
                assert!(w.pos.y >= l.top_margin);
            }
            WaypointKind::MeetingSofa | WaypointKind::MeetingChair => {
                assert!(!l.meeting_rooms.is_empty());
                assert!(w.room_id.is_some());
            }
        }
    }
}

#[test]
fn every_home_desk_has_a_reachable_approach_on_its_own_far_side() {
    // A pod's back row faces the front row across the thin INTRA_POD_GAP_Y, whose
    // edge cell sits in a ReachSet-rejected coarse cell straddling the desk — only
    // the deeper reachable-aware scan finds it, and only a far-side probe prefers it.
    use crate::layout::{Facing, Furniture, approach_point, desk_walk_anchor_facing};
    for (w, h) in [(192u16, 158u16), (160, 120), (240, 160)] {
        let l = SceneLayout::compute(w, h, Some(64)).expect("fits");
        // Without this the loop is vacuous: chair, probe and assertion agree under
        // any single facing, so an all-`South` layout would pass it untouched.
        assert!(
            l.home_desks.iter().any(|d| d.facing == Facing::North),
            "{w}x{h}: no desk faces north — the pod back row never turned around, \
             so nothing below probes the gap-side approach"
        );
        for &crate::layout::HomeDesk { at: desk, facing } in &l.home_desks {
            let chair = desk_walk_anchor_facing(desk, facing);
            const PROBE_DIST: u16 = 40;
            let origin = match facing {
                Facing::North => Point {
                    x: chair.x,
                    y: chair.y.saturating_add(PROBE_DIST),
                },
                _ => Point {
                    x: chair.x,
                    y: chair.y.saturating_sub(PROBE_DIST),
                },
            };
            let a = approach_point(
                Furniture::Desk,
                chair,
                facing,
                l.pantry_counter_size(),
                &l.walkable,
                origin,
                &l.reachable,
            );
            assert_ne!(
                a, chair,
                "desk {desk:?} ({facing:?}): no reachable approach"
            );
            // The contract is the ROTATED approach set: an E/W cell at the chair's row is legitimate.
            match facing {
                Facing::North => assert!(
                    a.y >= chair.y,
                    "desk {desk:?} faces north: approach {a:?} is NORTH of {chair:?}, \
                     the side its occupant sits on"
                ),
                _ => assert!(
                    a.y <= chair.y,
                    "desk {desk:?} faces south: approach {a:?} is SOUTH of {chair:?}, \
                     the side its occupant sits on"
                ),
            }
            assert!(
                l.reachable.reaches(a),
                "desk {desk:?}: approach {a:?} must be A*-reachable"
            );
        }
    }
}

#[test]
fn sofas_seat_three_people() {
    // 120 wide so the meeting room clears MEETING_FURNITURE_MIN_W: a 96-wide room
    // is too narrow to route to the sofa seats and is intentionally left bare.
    let l = SceneLayout::compute(120, 80, Some(4)).expect("fits");

    let couch: Vec<_> = l
        .waypoints
        .iter()
        .filter(|w| w.kind == WaypointKind::Couch)
        .collect();
    assert_eq!(couch.len(), 3, "lounge couch should seat 3");
    assert!(
        couch.iter().all(|w| w.room_id.is_none()),
        "couch keeps room_id None (grouping is at the chitchat layer)"
    );
    let mut xs: Vec<u16> = couch.iter().map(|w| w.pos.x).collect();
    xs.sort_unstable();
    assert_eq!(xs[1] - xs[0], 6, "couch seats are 6px apart");
    assert_eq!(xs[2] - xs[1], 6, "couch seats are 6px apart");
    let center = l
        .couch_sprite_center()
        .expect("couch sprite center recorded");
    assert_eq!(center.x, xs[1], "sprite center sits on the middle seat");

    assert!(!l.meeting_rooms.is_empty(), "expected a meeting room");
    let sofa_seats = l
        .waypoints
        .iter()
        .filter(|w| w.kind == WaypointKind::MeetingSofa)
        .count();
    let total_sofas: usize = l
        .meeting_rooms
        .iter()
        .filter_map(|r| r.trio.as_ref())
        .map(|t| t.sofas.len())
        .sum();
    assert_eq!(sofa_seats, 3 * total_sofas, "each meeting sofa seats 3");
}

#[test]
fn meeting_slots_track_meeting_trios() {
    let mut saw_room = false;
    let mut saw_no_room = false;
    let mut saw_dual = false;
    for seed in 0..40u64 {
        let l = SceneLayout::compute_with_seed(160, 120, Some(8), seed).expect("fits");
        let sofa_slots: Vec<_> = l
            .waypoints
            .iter()
            .filter(|w| {
                matches!(
                    w.kind,
                    WaypointKind::MeetingSofa | WaypointKind::MeetingChair
                )
            })
            .collect();
        if !l.meeting_rooms.is_empty() {
            saw_room = true;
            assert!(
                sofa_slots
                    .iter()
                    .any(|w| w.kind == WaypointKind::MeetingSofa),
                "seed {seed}: meeting room but no sofa slot"
            );
            assert!(
                sofa_slots
                    .iter()
                    .any(|w| w.kind == WaypointKind::MeetingChair),
                "seed {seed}: meeting room but no chair seat"
            );
            let rooms = l.meeting_rooms.len();
            for w in &sofa_slots {
                let rid = w.room_id.expect("meeting slot has room_id");
                assert!(
                    rid < rooms,
                    "seed {seed}: room_id {rid} out of range {rooms}"
                );
            }
            if rooms == 2 {
                saw_dual = true;
                assert!(
                    sofa_slots.iter().any(|w| w.room_id == Some(1)),
                    "seed {seed}: dual meeting but no room-1 slot"
                );
            }
        } else {
            saw_no_room = true;
            assert!(
                sofa_slots.is_empty(),
                "seed {seed}: no meeting room but {} meeting slots",
                sofa_slots.len()
            );
        }
    }
    assert!(saw_room, "no seed produced a meeting room");
    assert!(saw_no_room, "no seed produced a meeting-less floor");
    assert!(saw_dual, "no seed produced a dual-meeting floor");
}

#[test]
fn meeting_table_is_centered_between_its_two_sofas() {
    // The two sofas face each other across the table, so the table must sit
    // vertically EQUIDISTANT from both — otherwise the near sofa's front packs
    // against it and its seats lose their approach. Sofa/table positions are
    // window-height-driven, so this is a relative invariant swept across
    // sizes × seeds, NOT a fixed pixel offset.
    for (w, h) in [(128u16, 80u16), (160, 120), (192, 160), (240, 160)] {
        for seed in 0..8u64 {
            let Some(l) = SceneLayout::compute_with_seed(w, h, Some(8), seed) else {
                continue;
            };
            for (room_id, room) in l
                .meeting_rooms
                .iter()
                .enumerate()
                .filter_map(|(i, r)| r.trio.as_ref().map(|t| (i, t)))
            {
                let table = room.table;
                let north = room.sofas[0];
                let south = room.sofas[1];
                let gap_n = table.y.abs_diff(north.y);
                let gap_s = south.y.abs_diff(table.y);
                assert!(
                    gap_n.abs_diff(gap_s) <= 1,
                    "{w}x{h} seed {seed} room {room_id}: table not centered \
                     between sofas (north gap {gap_n}px, south gap {gap_s}px)"
                );
            }
        }
    }
}

#[test]
fn meeting_slots_face_the_table() {
    for seed in 0..40u64 {
        let l = SceneLayout::compute_with_seed(160, 120, Some(8), seed).expect("fits");
        for w in &l.waypoints {
            let Some(room_id) = w.room_id else { continue };
            let table = l.meeting_rooms[room_id]
                .trio
                .expect("slot's room hosts a trio")
                .table;
            match w.kind {
                WaypointKind::MeetingSofa => {
                    let want = if w.pos.y < table.y {
                        Facing::South
                    } else {
                        Facing::North
                    };
                    assert_eq!(
                        w.facing, want,
                        "seed {seed}: sofa {:?} vs table {:?}",
                        w.pos, table
                    );
                }
                WaypointKind::MeetingChair => {
                    let want = if w.pos.x < table.x {
                        Facing::East
                    } else {
                        Facing::West
                    };
                    assert_eq!(
                        w.facing, want,
                        "seed {seed}: stand {:?} vs table {:?}",
                        w.pos, table
                    );
                }
                _ => {}
            }
        }
    }
}

// DELIBERATELY walkable, unlike the island's in-body slots: the chair has no
// furniture body to sit inside (`Furniture::MeetingChair` keeps `footprint: None`),
// so blocking the cell would ripple the mask/approach for a piece a walker can
// visually clip at worst.
#[test]
fn meeting_chair_cells_stay_walkable() {
    for seed in 0..40u64 {
        for (w, h) in [(160u16, 120u16), (200, 100), (240, 140)] {
            let l = SceneLayout::compute_with_seed(w, h, Some(8), seed).expect("fits");
            for wp in &l.waypoints {
                if wp.kind == WaypointKind::MeetingChair {
                    assert!(
                        l.is_walkable(wp.pos.x, wp.pos.y),
                        "seed {seed} @ {w}x{h}: MeetingChair {:?} is non-walkable",
                        wp.pos
                    );
                }
            }
        }
    }
}

#[test]
fn compute_places_bookshelf_on_wall_and_whiteboard_in_walkway() {
    let l = SceneLayout::compute(120, 96, Some(1)).expect("fits");
    let bookshelf = l.wall_decor.iter().find(|i| i.kind == WallDecor::Bookshelf);
    let whiteboard = l
        .wall_decor
        .iter()
        .find(|i| i.kind == WallDecor::Whiteboard);
    assert!(bookshelf.is_some());
    assert!(whiteboard.is_some());
    assert!(bookshelf.unwrap().pos.y < l.cubicle_band.y);
    assert!(whiteboard.unwrap().pos.y > l.cubicle_band.y);
}

#[test]
fn whiteboard_blocks_only_its_wheel_base_not_the_elevated_panel() {
    let l = SceneLayout::compute(120, 96, Some(1)).expect("fits");
    let pos = l
        .wall_decor
        .iter()
        .find(|i| i.kind == WallDecor::Whiteboard)
        .expect("a free-standing whiteboard")
        .pos;
    // TopLeft-anchored: the 14×11 sprite's wheels sit at rows 8-10.
    assert!(
        l.is_walkable(pos.x + 5, pos.y + 2),
        "the elevated whiteboard panel must NOT block the floor (invariant #6)"
    );
    assert!(
        !l.is_walkable(pos.x + 5, pos.y + 9),
        "the whiteboard wheel base must block the floor"
    );
}

#[test]
fn fish_tank_sits_east_of_the_lounge_lamp_clear_of_the_elevator() {
    let l = SceneLayout::compute(192, 160, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let lamp = l.floor_lamp().expect("lounge fits at this size");
    let tank = l.fish_tank().expect("tank fits at this size");
    let half_w = furniture_def(Furniture::FishTank).visual.w / 2;
    let lamp_east = lamp.x + (furniture_def(Furniture::FloorLamp).visual.w - 1) / 2;
    assert_eq!(
        tank.x - half_w,
        lamp_east + 2,
        "tank west edge sits exactly the pinned gap past the lamp's east edge"
    );
    let door_west = l.door.x;
    assert!(
        tank.x + half_w + super::compute::FISH_TANK_ELEVATOR_CLEARANCE <= door_west,
        "tank + clearance stays west of the elevator door column"
    );
    for (w, h) in [(96u16, 70u16), (120, 80), (150, 68), (215, 98), (240, 160)] {
        let Some(l) = SceneLayout::compute(w, h, Some(TEST_DEFAULT_DESKS)) else {
            continue;
        };
        if l.fish_tank().is_some() {
            assert!(
                l.couch_sprite_center().is_some(),
                "{w}x{h}: tank requires the lounge"
            );
        }
    }
}

#[test]
fn meeting_table_ends_are_chair_seats_not_stands() {
    let l = SceneLayout::compute(192, 160, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let trio = l.meeting_rooms[0].trio.as_ref().expect("trio");
    let chairs: Vec<_> = l
        .waypoints
        .iter()
        .filter(|w| w.kind == WaypointKind::MeetingChair)
        .collect();
    assert_eq!(chairs.len(), 2, "one chair per table end");
    let (west, east) = (
        chairs
            .iter()
            .find(|w| w.pos.x < trio.table.x)
            .expect("west chair"),
        chairs
            .iter()
            .find(|w| w.pos.x > trio.table.x)
            .expect("east chair"),
    );
    assert_eq!(west.facing, Facing::East, "west chair faces the table");
    assert_eq!(east.facing, Facing::West, "east chair faces the table");
    assert_eq!(west.pos.y, trio.table.y, "chairs sit on the table's row");
    assert_eq!(
        trio.table.x - west.pos.x,
        east.pos.x - trio.table.x,
        "chair offsets mirror around the table center"
    );
}

#[test]
fn coat_rack_yields_to_the_east_chair_in_narrow_fitted_rooms() {
    // On fitted rooms ≲40 wide the rack's coats would overprint the east chair body
    // and its sitter; bare rooms and roomy floors keep it.
    let narrow = SceneLayout::compute(120, 160, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let r = &narrow.meeting_rooms[0];
    assert!(r.trio.is_some(), "120x160 hosts a fitted room");
    assert_eq!(
        r.coat_rack_pos(),
        None,
        "narrow fitted room drops the rack instead of interpenetrating the chair"
    );
    let roomy = SceneLayout::compute(192, 160, Some(TEST_DEFAULT_DESKS)).expect("fits");
    assert!(
        roomy.meeting_rooms[0].coat_rack_pos().is_some(),
        "roomy fitted room keeps the rack"
    );
}

#[test]
fn a_room_too_short_for_the_rack_drops_it() {
    let room = MeetingRoom {
        bounds: Bounds {
            x: 0,
            y: 0,
            width: 30,
            height: 4,
        },
        trio: None,
    };
    assert_eq!(room.coat_rack_pos(), None);
}

#[test]
fn pod_grid_fills_every_desk_row_that_fits() {
    // #552: a phantom trailing aisle below the last pod row starved residual_h, so
    // a bottom row that physically fits never fired.
    for w in [140u16, 160, 192, 215, 240] {
        for h in (100..=240u16).step_by(10) {
            let Some(l) = SceneLayout::compute(w, h, Some(64)) else {
                continue;
            };
            if l.home_desks.is_empty() {
                continue;
            }
            let lowest = l.home_desks.iter().map(|d| d.at.y).max().unwrap();
            let next_row_y = lowest + DESK_H + INTER_POD_AISLE_Y;
            let desk_y_max = (l.cubicle_band.y + l.cubicle_band.height)
                .saturating_sub(super::decor::DESK_GROUND_H);
            assert!(
                next_row_y > desk_y_max,
                "{w}x{h}: a whole desk row fits below the grid (next_row_y {next_row_y} <= desk_y_max {desk_y_max})"
            );
        }
    }
}

#[test]
fn desk_columns_stay_on_one_lattice_and_never_shift_with_width() {
    // #553: widening only ADDS columns (band-relative offsets never move), so a
    // resize can't make inter-desk spacing jump.
    use std::collections::BTreeSet;
    let stride_x = POD_SIDE * DESK_W + (POD_SIDE - 1) * INTRA_POD_GAP_X + INTER_POD_AISLE_X;
    let mut prev: BTreeSet<u16> = BTreeSet::new();
    for w in (140..=260u16).step_by(4) {
        let Some(l) = SceneLayout::compute(w, 160, Some(64)) else {
            continue;
        };
        let origin = l.cubicle_band.x + INTER_POD_AISLE_X / 2;
        let offs: BTreeSet<u16> = l.home_desks.iter().map(|d| d.at.x - origin).collect();
        for &off in &offs {
            let in_pod = off % stride_x;
            assert!(
                in_pod == 0 || in_pod == DESK_W + INTRA_POD_GAP_X,
                "{w}x160: column offset {off} is off the pod lattice (in_pod {in_pod})"
            );
        }
        assert!(
            prev.is_subset(&offs),
            "{w}x160: existing columns moved or vanished on widen: {prev:?} -> {offs:?}"
        );
        prev = offs;
    }
}

#[test]
fn desk_rows_stay_on_one_lattice() {
    let stride_y = POD_SIDE * DESK_H + (POD_SIDE - 1) * INTRA_POD_GAP_Y + INTER_POD_AISLE_Y;
    for w in [140u16, 160, 192, 240] {
        for h in (100..=240u16).step_by(20) {
            let Some(l) = SceneLayout::compute(w, h, Some(64)) else {
                continue;
            };
            let Some(first) = l.home_desks.first() else {
                continue;
            };
            let origin_y = first.at.y;
            for d in &l.home_desks {
                let off = d.at.y - origin_y;
                let in_pod = off % stride_y;
                assert!(
                    in_pod == 0 || in_pod == DESK_H + INTRA_POD_GAP_Y,
                    "{w}x{h}: desk row top offset {off} is off the row lattice (in_pod {in_pod})"
                );
            }
        }
    }
}

#[test]
fn ficus_greets_at_the_elevator_and_fills_the_lounge_west_flank() {
    let l = SceneLayout::compute(192, 160, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let ficus: Vec<_> = l
        .plants
        .iter()
        .filter(|p| p.kind == PlantKind::Ficus)
        .collect();
    assert_eq!(ficus.len(), 2, "both ratified Ficus spots place at 192x160");
    let door = l.door;
    assert!(
        ficus
            .iter()
            .any(|p| p.pos.x + 12 > door.x && p.pos.y < l.cubicle_band.y + 12),
        "one ficus greets beside the elevator"
    );
    let couch = l.couch_sprite_center().expect("lounge");
    assert!(
        ficus.iter().any(|p| p.pos.x < couch.x),
        "one ficus fills the lounge west flank"
    );
}

#[test]
fn pantry_and_meeting_procedural_rects_match_the_painted_geometry() {
    // These expected boxes ARE the pixels
    // paint_water_cooler/paint_trash_bin/paint_doormat draw, so the hover hit-test
    // in the binary can't strand itself across the crate boundary.
    let pantry = PantryRoom {
        bounds: Bounds {
            x: 10,
            y: 20,
            width: 40,
            height: 40,
        },
        counter_size: Size { w: 20, h: 8 },
        kitchen_island: None,
    };
    assert_eq!(
        pantry.water_cooler_rect(),
        Some(Bounds {
            x: 10 + 40 - 7,
            y: 20 + 6,
            width: 4,
            height: 9,
        }),
    );
    assert_eq!(
        pantry.trash_bin_rect(),
        Some(Bounds {
            x: 10 + 3,
            y: 20 + 40 - 14,
            width: 4,
            height: 5,
        }),
    );
    let meeting = MeetingRoom {
        bounds: Bounds {
            x: 100,
            y: 30,
            width: 44,
            height: 40,
        },
        trio: None,
    };
    assert_eq!(
        meeting.doormat_rect(),
        Some(Bounds {
            x: 100 + 44 + 1,
            y: 30 + 40 / 2 - 2,
            width: 4,
            height: 5,
        }),
    );

    // A too-small room yields None (the painters no-op there), so the hit-test's
    // box vanishes in lockstep with the sprite.
    let tiny_pantry = PantryRoom {
        bounds: Bounds {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        },
        counter_size: Size { w: 20, h: 8 },
        kitchen_island: None,
    };
    assert_eq!(tiny_pantry.water_cooler_rect(), None);
    assert_eq!(tiny_pantry.trash_bin_rect(), None);
    let tiny_meeting = MeetingRoom {
        bounds: Bounds {
            x: 0,
            y: 0,
            width: 10,
            height: 40,
        },
        trio: None,
    };
    assert_eq!(tiny_meeting.doormat_rect(), None);
}

#[test]
fn sofa_east_drain_edge_reads_the_placed_sofa_and_is_none_when_bare() {
    let bounds = Bounds {
        x: 10,
        y: 10,
        width: 40,
        height: 60,
    };
    let trio = MeetingRoom::place_trio(bounds, false);
    let room = MeetingRoom {
        bounds,
        trio: Some(trio),
    };
    // place_trio centres the sofa on the room's mid-column.
    assert_eq!(trio.sofas[0].x, bounds.x + bounds.width / 2);
    let mask = mask::build_walkable_mask(&mask::MaskObstacles {
        meeting_rooms: &[room],
        ..mask::MaskObstacles::bare(80, 100)
    });
    assert_eq!(
        room.sofa_east_drain_edge(),
        Some(mask::first_walkable_east(
            &mask,
            trio.sofas[0].x,
            trio.sofas[0].y
        ))
    );

    let bare = MeetingRoom { bounds, trio: None };
    assert_eq!(bare.sofa_east_drain_edge(), None);
}

/// A standing fixture's height as a percent band of [`CHARACTER_SPRITE_H`].
struct HeightBand {
    min_pct: u16,
    max_pct: u16,
}

/// Person-height: a figure reaches the brand panel.
const VENDING_MACHINE_HEIGHT: HeightBand = HeightBand {
    min_pct: 100,
    max_pct: 115,
};
/// A floor-standing copier: its lid at a figure's waist.
const PRINTER_HEIGHT: HeightBand = HeightBand {
    min_pct: 60,
    max_pct: 75,
};
/// Its taps at a standing figure's hand.
const WATER_COOLER_HEIGHT: HeightBand = HeightBand {
    min_pct: 70,
    max_pct: 80,
};
/// Coats hang at a figure's shoulders.
const COAT_RACK_HEIGHT: HeightBand = HeightBand {
    min_pct: 95,
    max_pct: 105,
};
/// The shade at a figure's head.
const FLOOR_LAMP_HEIGHT: HeightBand = HeightBand {
    min_pct: 95,
    max_pct: 110,
};
/// A room its occupant steps into.
const PHONE_BOOTH_HEIGHT: HeightBand = HeightBand {
    min_pct: 120,
    max_pct: 135,
};

#[test]
fn standing_fixtures_are_in_proportion_to_a_figure() {
    let visual_h = |f: Furniture| furniture_def(f).visual.h;
    for (name, h, band) in [
        (
            "vending machine",
            visual_h(Furniture::VendingMachine),
            VENDING_MACHINE_HEIGHT,
        ),
        ("printer", visual_h(Furniture::Printer), PRINTER_HEIGHT),
        (
            "water cooler",
            super::rooms::pantry::WATER_COOLER.h,
            WATER_COOLER_HEIGHT,
        ),
        (
            "coat rack",
            coat_rack_rect_at(Point { x: 10, y: 10 }).height,
            COAT_RACK_HEIGHT,
        ),
        (
            "floor lamp",
            visual_h(Furniture::FloorLamp),
            FLOOR_LAMP_HEIGHT,
        ),
        (
            "phone booth",
            visual_h(Furniture::PhoneBooth),
            PHONE_BOOTH_HEIGHT,
        ),
    ] {
        let pct = h * 100 / CHARACTER_SPRITE_H;
        assert!(
            (band.min_pct..=band.max_pct).contains(&pct),
            "{name}: {h}px is {pct}% of a figure, outside {}..={}%",
            band.min_pct,
            band.max_pct
        );
    }
}

#[test]
fn waypoint_depth_baseline_is_its_grounds_south_row() {
    let pos = Point { x: 40, y: 40 };
    for kind in [WaypointKind::VendingMachine, WaypointKind::Printer] {
        let def = furniture_def(kind.furniture());
        let (tl, size) = def.ground_rect(Pivot::Center, pos).expect("has footprint");
        assert_eq!(
            sort_row_at(Pivot::Center, pos, def.visual.h),
            tl.y + size.h - 1,
            "{kind:?}: sorts on the row it stands on"
        );
    }
}
