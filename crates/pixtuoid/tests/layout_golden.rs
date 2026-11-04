//! Layout geometry golden snapshots: the key fields of `SceneLayout` at fixed
//! buffer sizes, so any refactor to `compute` that
//! accidentally shifts desk positions, room bounds, or waypoint coords fails
//! immediately.
//!
//! Regenerating after an INTENTIONAL layout change: run the suite to produce
//! `*.snap.new`, eyeball the diff, then `cargo insta accept`. The same applies
//! to `walkable_golden.rs`.

use pixtuoid_scene::layout::{SceneLayout, Viewport};

#[test]
fn layout_standard_96x72_desks() {
    let l = SceneLayout::compute(Viewport::full(96, 72), Some(4), 0).unwrap();
    insta::assert_debug_snapshot!("desks_96x72", l.home_desks);
}

#[test]
fn layout_standard_96x72_waypoints() {
    let l = SceneLayout::compute(Viewport::full(96, 72), Some(4), 0).unwrap();
    insta::assert_debug_snapshot!("waypoints_96x72", l.waypoints);
}

#[test]
fn layout_standard_96x72_meeting() {
    let l = SceneLayout::compute(Viewport::full(96, 72), Some(4), 0).unwrap();
    insta::assert_debug_snapshot!("meeting_96x72", l.meeting_room_bounds(0));
}

#[test]
fn layout_standard_96x72_room_walls() {
    let l = SceneLayout::compute(Viewport::full(96, 72), Some(4), 0).unwrap();
    insta::assert_debug_snapshot!("room_walls_96x72", l.room_walls);
}

#[test]
fn layout_standard_96x72_zones() {
    let l = SceneLayout::compute(Viewport::full(96, 72), Some(4), 0).unwrap();
    insta::assert_debug_snapshot!("cubicle_band_96x72", l.cubicle_band);
    insta::assert_debug_snapshot!("cubicle_aisle_96x72", l.cubicle_aisle);
    insta::assert_debug_snapshot!("pantry_96x72", l.pantry.map(|p| p.bounds));
}

#[test]
fn layout_standard_192x160_desks() {
    let l = SceneLayout::compute(Viewport::full(192, 160), Some(8), 0).unwrap();
    insta::assert_debug_snapshot!("desks_192x160", l.home_desks);
}

#[test]
fn layout_standard_192x160_waypoints() {
    let l = SceneLayout::compute(Viewport::full(192, 160), Some(8), 0).unwrap();
    insta::assert_debug_snapshot!("waypoints_192x160", l.waypoints);
}

#[test]
fn layout_standard_192x160_meeting() {
    let l = SceneLayout::compute(Viewport::full(192, 160), Some(8), 0).unwrap();
    insta::assert_debug_snapshot!("meeting_192x160", l.meeting_room_bounds(0));
}

#[test]
fn layout_standard_192x160_room_walls() {
    let l = SceneLayout::compute(Viewport::full(192, 160), Some(8), 0).unwrap();
    insta::assert_debug_snapshot!("room_walls_192x160", l.room_walls);
}

// Open plan: seed 11 hashes to it (no meeting room, open pantry).

#[test]
fn layout_open_plan_seed11_desks() {
    let l = SceneLayout::compute(Viewport::full(160, 120), Some(4), 11).unwrap();
    insta::assert_debug_snapshot!("desks_open_plan_seed11", l.home_desks);
}

#[test]
fn layout_open_plan_seed11_waypoints() {
    let l = SceneLayout::compute(Viewport::full(160, 120), Some(4), 11).unwrap();
    insta::assert_debug_snapshot!("waypoints_open_plan_seed11", l.waypoints);
}

#[test]
fn layout_open_plan_seed11_meeting() {
    let l = SceneLayout::compute(Viewport::full(160, 120), Some(4), 11).unwrap();
    insta::assert_debug_snapshot!("meeting_open_plan_seed11", l.meeting_room_bounds(0));
}

#[test]
fn layout_open_plan_seed11_room_walls() {
    let l = SceneLayout::compute(Viewport::full(160, 120), Some(4), 11).unwrap();
    insta::assert_debug_snapshot!("room_walls_open_plan_seed11", l.room_walls);
}

// Dense layout: seed 3 hashes to it (dual meeting rooms if tall enough).

#[test]
fn layout_dense_seed3_desks() {
    let l = SceneLayout::compute(Viewport::full(192, 160), Some(4), 3).unwrap();
    insta::assert_debug_snapshot!("desks_dense_seed3", l.home_desks);
}

#[test]
fn layout_dense_seed3_waypoints() {
    let l = SceneLayout::compute(Viewport::full(192, 160), Some(4), 3).unwrap();
    insta::assert_debug_snapshot!("waypoints_dense_seed3", l.waypoints);
}

#[test]
fn layout_dense_seed3_meeting() {
    let l = SceneLayout::compute(Viewport::full(192, 160), Some(4), 3).unwrap();
    insta::assert_debug_snapshot!("meeting_dense_seed3", l.meeting_room_bounds(0));
}

#[test]
fn layout_dense_seed3_room_walls() {
    let l = SceneLayout::compute(Viewport::full(192, 160), Some(4), 3).unwrap();
    insta::assert_debug_snapshot!("room_walls_dense_seed3", l.room_walls);
}

// A seed collision here makes adjacent floors look identical.

#[test]
fn floor_variant_hash_gives_unique_layouts_per_floor() {
    use std::collections::HashSet;

    let signatures: HashSet<_> = (0..5)
        .map(|i| {
            let seed = pixtuoid_scene::floor::floor_seed(i);
            let l = SceneLayout::compute(Viewport::full(192, 160), Some(4), seed).unwrap();
            (l.meeting_rooms.clone(), l.pantry, l.home_desks)
        })
        .collect();

    assert_eq!(
        signatures.len(),
        5,
        "floors 0..5 must produce 5 distinct layout variants, got {} unique",
        signatures.len(),
    );
}
