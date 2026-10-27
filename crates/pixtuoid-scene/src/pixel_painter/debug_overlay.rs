//! Live debug layer toggled by `w`: the walkable mask (red), allowed approach
//! sides (green) / on-furniture seat cells (magenta), and the live A* route
//! polylines of walking agents (cyan), composited over the finished scene.
//!
//! A VIEW over the SAME data the renderer + router already use — never a second
//! source.

use std::collections::HashMap;

use pixtuoid_core::sprite::{Rgb, RgbBuffer};
use pixtuoid_core::{AgentId, SceneState};

use super::palette::blend_pixel;
use crate::layout::{
    Furniture, Point, SceneLayout, Size, WaypointKind, desk_walk_anchor_facing, furniture_def,
};
use crate::walk::WalkState;

const BLOCKED: Rgb = Rgb {
    r: 220,
    g: 60,
    b: 60,
};
const APPROACH: Rgb = Rgb {
    r: 70,
    g: 220,
    b: 110,
};
const SEAT: Rgb = Rgb {
    r: 235,
    g: 80,
    b: 215,
};
const ROUTE: Rgb = Rgb {
    r: 70,
    g: 210,
    b: 235,
};

/// N, S, E, W unit dirs (same axes `ApproachSides::allows` expects).
const DIRS: [(i32, i32); 4] = [(0, -1), (0, 1), (1, 0), (-1, 0)];

pub(super) fn paint(
    buf: &mut RgbBuffer,
    layout: &SceneLayout,
    scene: &SceneState,
    walks: &HashMap<AgentId, WalkState>,
) {
    paint_mask(buf, layout);
    paint_approach(buf, layout);
    paint_routes(buf, scene, walks);
}

fn tint(buf: &mut RgbBuffer, x: i32, y: i32, c: Rgb, t: f32) {
    if x < 0 || y < 0 {
        return;
    }
    let (x, y) = (x as u16, y as u16);
    if x >= buf.width() || y >= buf.height() {
        return;
    }
    blend_pixel(buf, x, y, c, t);
}

fn blob(buf: &mut RgbBuffer, cx: i32, cy: i32, c: Rgb, t: f32) {
    for dy in -1..=1 {
        for dx in -1..=1 {
            tint(buf, cx + dx, cy + dy, c, t);
        }
    }
}

fn paint_mask(buf: &mut RgbBuffer, layout: &SceneLayout) {
    for y in 0..layout.buf_h {
        for x in 0..layout.buf_w {
            if !layout.is_walkable(x, y) {
                tint(buf, i32::from(x), i32::from(y), BLOCKED, 0.38);
            }
        }
    }
}

/// The seat-approach scan, borrowed from the sim's own authority rather than
/// mirrored here: a second copy of the ladder (or of its scan bound) could make
/// the green dots disagree with where agents actually route.
fn first_reachable_on_side(layout: &SceneLayout, origin: Point, dx: i32, dy: i32) -> Option<Point> {
    crate::layout::first_reachable_on_side(&layout.walkable, &layout.reachable, origin, dx, dy)
}

fn paint_approach(buf: &mut RgbBuffer, layout: &SceneLayout) {
    for wp in &layout.waypoints {
        let def = furniture_def(wp.kind.furniture());
        if def.occupies_pos {
            blob(buf, i32::from(wp.pos.x), i32::from(wp.pos.y), SEAT, 0.7);
            // A* routes to an APPROACH POINT off an allowed side, distinct from
            // the seat — so the viewer can confirm the agent enters from its
            // natural side, not the backrest.
            for (dx, dy) in DIRS {
                if !def.approach.allows(wp.facing, (dx, dy)) {
                    continue;
                }
                if let Some(c) = first_reachable_on_side(layout, wp.pos, dx, dy) {
                    blob(buf, i32::from(c.x), i32::from(c.y), APPROACH, 0.7);
                }
            }
            continue;
        }
        // Pantry's footprint is runtime-sized.
        let fp = if wp.kind == WaypointKind::Pantry {
            Some(layout.pantry_counter_size())
        } else {
            def.footprint
        };
        let Some(Size { w, h }) = fp else {
            continue;
        };
        let (hx, hy) = (i32::from(w / 2), i32::from(h / 2));
        for (dx, dy) in DIRS {
            if def.approach.allows(wp.facing, (dx, dy)) {
                blob(
                    buf,
                    i32::from(wp.pos.x) + dx * (hx + 1),
                    i32::from(wp.pos.y) + dy * (hy + 1),
                    APPROACH,
                    0.7,
                );
            }
        }
    }
    // Home desks get the same seat/approach split. Scan from the CHAIR, not the
    // desk's top-left corner, or the east side never shows — the corner scan
    // can't clear the desk body.
    let desk_def = furniture_def(Furniture::Desk);
    for &crate::layout::HomeDesk { at, facing } in &layout.home_desks {
        let chair = desk_walk_anchor_facing(at, facing);
        // SEAT last: the 3x3 blobs overlap when the approach cell lands a pixel off the chair.
        for (dx, dy) in DIRS {
            // The desk's OWN facing, as production reads it — a fixed South here
            // mirrors the blobs on every back-turned desk, and this overlay is
            // what the next reader reaches for to check that very geometry.
            if !desk_def.approach.allows(facing, (dx, dy)) {
                continue;
            }
            if let Some(c) = first_reachable_on_side(layout, chair, dx, dy) {
                blob(buf, i32::from(c.x), i32::from(c.y), APPROACH, 0.7);
            }
        }
        blob(buf, i32::from(chair.x), i32::from(chair.y), SEAT, 0.7);
    }
}

fn paint_routes(buf: &mut RgbBuffer, scene: &SceneState, walks: &HashMap<AgentId, WalkState>) {
    for agent in scene.agents.values() {
        let Some(walk) = walks.get(&agent.agent_id) else {
            continue;
        };
        let Some(wp) = &walk.walk_path else {
            continue;
        };
        for seg in wp.path.windows(2) {
            line(buf, seg[0], seg[1], ROUTE);
        }
    }
}

/// Integer Bresenham line between two pixel points.
fn line(buf: &mut RgbBuffer, a: Point, b: Point, c: Rgb) {
    let (mut x0, mut y0) = (i32::from(a.x), i32::from(a.y));
    let (x1, y1) = (i32::from(b.x), i32::from(b.y));
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        tint(buf, x0, y0, c, 0.8);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::SceneLayout;

    fn greenish(c: Rgb) -> bool {
        c.g > c.r && c.g > c.b
    }
    fn magentaish(c: Rgb) -> bool {
        c.r > c.g && c.b > c.g
    }

    #[test]
    fn overlay_marks_seat_approach_sides_distinct_from_the_seat_cell() {
        let l = SceneLayout::compute_with_seed(200, 130, Some(8), 0).unwrap();
        let couch = l
            .waypoints
            .iter()
            .find(|w| w.kind == WaypointKind::Couch)
            .expect("a lounge couch seat");
        let mut buf = RgbBuffer::filled(l.buf_w, l.buf_h, Rgb { r: 0, g: 0, b: 0 });
        paint_approach(&mut buf, &l);

        assert!(
            magentaish(buf.get(couch.pos.x, couch.pos.y)),
            "seat cell must be tinted toward SEAT (magenta), got {:?}",
            buf.get(couch.pos.x, couch.pos.y)
        );

        // Expected cells come from the SIM's own scan, so this asserts the overlay
        // marks where the router routes — not "something green near the couch".
        let def = furniture_def(couch.kind.furniture());
        let found_green_approach = DIRS.iter().any(|&(dx, dy)| {
            def.approach.allows(couch.facing, (dx, dy))
                && first_reachable_on_side(&l, couch.pos, dx, dy)
                    .is_some_and(|c| greenish(buf.get(c.x, c.y)))
        });
        assert!(
            found_green_approach,
            "at least one allowed, reachable approach cell must be tinted toward APPROACH (green)"
        );
    }

    #[test]
    fn overlay_marks_desk_approach_distinct_from_the_chair() {
        use crate::layout::{Facing, Furniture};
        let l = SceneLayout::compute_with_seed(200, 130, Some(8), 0).unwrap();
        let desk = *l.home_desks.first().expect("a home desk");
        let chair = desk_walk_anchor_facing(desk.at, desk.facing);
        let mut buf = RgbBuffer::filled(l.buf_w, l.buf_h, Rgb { r: 0, g: 0, b: 0 });
        paint_approach(&mut buf, &l);

        assert!(
            magentaish(buf.get(chair.x, chair.y)),
            "the desk chair must be tinted toward SEAT (magenta), got {:?}",
            buf.get(chair.x, chair.y)
        );

        // Scan from the CHAIR (== production `desk_approach_cell`), not the desk corner.
        let def = furniture_def(Furniture::Desk);
        let found_green_approach = DIRS.iter().any(|&(dx, dy)| {
            def.approach.allows(Facing::South, (dx, dy))
                && first_reachable_on_side(&l, chair, dx, dy)
                    .is_some_and(|c| greenish(buf.get(c.x, c.y)))
        });
        assert!(
            found_green_approach,
            "at least one allowed, reachable desk approach cell must be tinted green"
        );
    }

    fn slot(id: AgentId) -> pixtuoid_core::AgentSlot {
        use pixtuoid_core::state::{ActivityState, GlobalDeskIndex};
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::time::SystemTime;
        let now = SystemTime::UNIX_EPOCH;
        pixtuoid_core::AgentSlot {
            agent_id: id,
            source: Arc::from("claude-code"),
            session_id: Arc::from("s"),
            cwd: Arc::from(PathBuf::from("/x").as_path()),
            label: "x".into(),
            state: ActivityState::Idle,
            state_started_at: now,
            created_at: now,
            last_event_at: now,
            exiting_at: None,
            desk_index: GlobalDeskIndex(0),
            floor_idx: 0,
            tool_call_count: 0,
            running_ms: 0,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        }
    }

    #[test]
    fn paint_routes_draws_frozen_paths_and_skips_the_rest() {
        use crate::walk::{WalkPathSnapshot, WalkState};
        use std::collections::HashMap;

        let mut scene = SceneState::uniform(8);
        let id_path = AgentId::from_transcript_path("/with_path.jsonl");
        let id_nopath = AgentId::from_transcript_path("/no_path.jsonl");
        let id_absent = AgentId::from_transcript_path("/absent.jsonl");
        for id in [id_path, id_nopath, id_absent] {
            scene.agents.insert(id, slot(id));
        }
        let mut walks: HashMap<AgentId, WalkState> = HashMap::new();
        let ms_path = WalkState {
            walk_path: Some(WalkPathSnapshot {
                from: Point { x: 5, y: 5 },
                to: Point { x: 80, y: 80 },
                path: vec![Point { x: 5, y: 5 }, Point { x: 80, y: 80 }],
            }),
            ..WalkState::default()
        };
        walks.insert(id_path, ms_path);
        walks.insert(id_nopath, WalkState::default());
        // id_absent is intentionally NOT in the walks map.

        let bg = Rgb { r: 0, g: 0, b: 0 };
        let mut buf = RgbBuffer::filled(50, 50, bg);
        paint_routes(&mut buf, &scene, &walks);
        let painted = (0..50u16)
            .flat_map(|y| (0..50u16).map(move |x| (x, y)))
            .any(|(x, y)| buf.get(x, y) != bg);
        assert!(painted, "the frozen walk_path must draw a route polyline");
    }

    #[test]
    fn first_reachable_on_side_breaks_on_negative_coords() {
        let l = SceneLayout::compute_with_seed(200, 130, Some(8), 0).unwrap();
        assert_eq!(
            first_reachable_on_side(&l, Point { x: 0, y: 0 }, -1, 0),
            None,
            "scanning into negative x must break and return None"
        );
        assert_eq!(
            first_reachable_on_side(&l, Point { x: 0, y: 0 }, 0, -1),
            None,
            "scanning into negative y must break and return None"
        );
    }
}
