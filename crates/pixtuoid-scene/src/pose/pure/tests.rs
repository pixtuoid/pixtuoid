use super::*;
use crate::character::test_support::make_slot;
use crate::pack::{Piece, Walk};
use crate::physics::Segment;
use pixtuoid_core::state::{ActivityState, ToolKind, ToolUse};
use std::time::Duration;

fn layout() -> SceneLayout {
    SceneLayout::compute(crate::layout::Viewport::full(120, 96), Some(4), 0).expect("fits")
}

fn typing() -> ActivityState {
    ActivityState::Running {
        item: Some(ToolUse {
            tool_use_id: Some("t".into()),
            detail: Some("Edit".into()),
            kind: ToolKind::Edit,
        }),
    }
}

#[test]
fn a_typist_keys_every_frame_and_rests_on_the_first() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let s = AgentSlot {
        state_started_at: now,
        ..make_slot(AgentId::from_transcript_path("/p/a.jsonl"), typing())
    };
    let pack = crate::pack::test_office();
    let anim = pack.piece(Piece::Typing);
    let ms = u64::from(anim.frame_ms());
    let frame = |at| typing_frame(&s, crate::anim::Beat::at_ms(at), anim);
    assert_ne!(frame(0), frame(ms), "it keys every frame");
    assert_eq!(frame(0), frame(ms * anim.frames().len() as u64));
    assert_eq!(
        typing_frame(&s, crate::anim::Motion::Still.beat(now), anim),
        0,
        "at rest a typist holds the first frame"
    );
}

/// Regression: a pantry-bound agent must walk to a WALKABLE stand cell, not the
/// blocked counter center — that forced A* to detour and the sprite to pop on
/// arrival.
#[test]
fn pantry_walk_destination_is_walkable() {
    let l = layout();
    let pantry_idx = l
        .waypoints
        .iter()
        .position(|w| w.kind == WaypointKind::Pantry)
        .expect("standard floor has a pantry");
    let (id, n) = (0..8000u64)
        .find_map(|i| {
            let id = AgentId::from_transcript_path(&format!("/p/pw{i}.jsonl"));
            let n = (0..300u64).find(|n| {
                takes_trip(id, *n)
                    && !is_aimless_cycle(id, *n)
                    && waypoint_index_for_cycle(id, *n, l.waypoints.len()) == pantry_idx
            })?;
            Some((id, n))
        })
        .expect("some agent lands at the pantry");

    let dest = resolve_wander_target(id, n, &l, l.home_desks[0], &SpotClaims::default()).dest;
    assert!(
        l.walkable.is_walkable(dest.x, dest.y),
        "pantry walk dest {dest:?} is not walkable (center={:?})",
        l.waypoints[pantry_idx].pos
    );
}

fn aid(i: usize) -> AgentId {
    AgentId::from_transcript_path(&format!("/p/dwell{i}.jsonl"))
}

#[test]
fn dwell_ms_is_within_per_kind_range() {
    let cases = [
        (WaypointKind::Couch, 20_000u64, 40_000u64),
        (WaypointKind::MeetingSofa, 20_000, 40_000),
        (WaypointKind::MeetingChair, 20_000, 40_000),
        (WaypointKind::Pantry, 10_000, 18_000),
        (WaypointKind::PhoneBooth, 8_000, 30_000),
        (WaypointKind::StandingDesk, 8_000, 30_000),
        (WaypointKind::VendingMachine, 4_000, 8_000),
        (WaypointKind::Printer, 4_000, 8_000),
    ];
    for (kind, lo, hi) in cases {
        for i in 0..256 {
            let d = dwell_ms(kind, aid(i));
            assert!(
                (lo..hi).contains(&d),
                "{kind:?} dwell {d} out of [{lo},{hi}) for agent {i}"
            );
        }
    }
}

#[test]
fn dwell_ms_varies_across_agents() {
    let vals: std::collections::HashSet<u64> = (0..64)
        .map(|i| dwell_ms(WaypointKind::Couch, aid(i)))
        .collect();
    assert!(vals.len() >= 16, "expected dwell jitter across agents");
}

#[test]
fn seated_dwell_and_est_cycle_are_consistent() {
    for i in 0..128 {
        let id = aid(i);
        let sd = seated_dwell_ms(id);
        assert!((15_000..30_000).contains(&sd), "seated dwell {sd}");
        assert_eq!(
            est_wander_cycle_ms(id),
            sd + 2 * WANDER_WALK_EST_MS + WANDER_DWELL_EST_MS
        );
    }
}

#[test]
fn personality_varies_across_agents() {
    let ps: Vec<Personality> = (0..20)
        .map(|i| personality_for(AgentId::from_transcript_path(&format!("/p/{i}.jsonl"))))
        .collect();
    let trip_chances: std::collections::HashSet<u8> =
        ps.iter().map(|p| p.trip_chance_pct).collect();
    assert!(
        trip_chances.len() >= 5,
        "expected variance in trip_chance_pct"
    );
    for p in &ps {
        assert!((25..=60).contains(&p.trip_chance_pct));
        assert!(p.aimless_pref_pct <= 70);
    }
}

#[test]
fn waypoint_index_is_zero_when_no_waypoints() {
    let id = AgentId::from_transcript_path("/p/wp.jsonl");
    assert_eq!(waypoint_index_for_cycle(id, 3, 0), 0);
}

#[test]
fn stale_resume_gap_ms_varies_across_agents() {
    let ids: Vec<AgentId> = (0..10)
        .map(|i| AgentId::from_transcript_path(&format!("/p/{i}.jsonl")))
        .collect();
    let cycles: std::collections::HashSet<u64> =
        ids.iter().map(|id| stale_resume_gap_ms(*id)).collect();
    assert!(
        cycles.len() >= 3,
        "expected multiple distinct cycle lengths, got {cycles:?}"
    );
    for c in &cycles {
        assert!(
            *c >= STALE_RESUME_GAP_BASE_MS
                && *c < STALE_RESUME_GAP_BASE_MS + STALE_RESUME_GAP_RANGE_MS
        );
    }
}

#[test]
fn waypoint_choice_changes_across_cycles_for_same_agent() {
    let l = layout();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    let dest_xs: std::collections::HashSet<u16> = (0..50u64)
        .filter(|&n| takes_trip(id, n))
        .map(|n| {
            resolve_wander_target(id, n, &l, l.home_desks[0], &SpotClaims::default())
                .dest
                .x
        })
        .collect();
    assert!(
        dest_xs.len() >= 2,
        "destination should vary across cycles, got {dest_xs:?}"
    );
}

/// Forcing the fallback needs a mask with ONE small open pocket, so the seeds
/// whose weighted zone isn't the corridor spend all 32 probes in a fully blocked
/// zone. The pocket is sized past the coarse router's `cell_walkable` floor —
/// a lone walkable pixel is an island `find_path` would refuse.
#[test]
fn aimless_fallback_scans_the_midline_for_a_walkable_cell() {
    let mut l = layout();
    let c = l.corridor;
    let mid_y = c.y + c.height / 2;
    const POCKET_PX: u16 = 12;
    let open = Point {
        x: c.x + c.width - POCKET_PX / 2,
        y: mid_y,
    };
    let mut mask = crate::walkable::WalkableMask::new_open(l.buf_w, l.buf_h);
    mask.mark_blocked(0, 0, l.buf_w, l.buf_h, 0);
    mask.mark_walkable(
        open.x - POCKET_PX / 2,
        open.y - POCKET_PX / 2,
        POCKET_PX,
        POCKET_PX,
    );
    // `reachable` is DERIVED from `walkable` in a real layout — rebuild it, or
    // the routability filter reads a ReachSet describing a different office.
    l.reachable = crate::layout::ReachSet::from_mask(&mask, open);
    l.walkable = mask;

    let desk = l.home_desks[0];
    for seed in 0..16u64 {
        let p = pick_aimless_dest(&l, seed, desk);
        assert!(
            l.is_walkable(p.x, p.y) && l.reachable.reaches(p),
            "seed {seed}: fallback returned an unroutable cell {p:?}"
        );
        assert_eq!(
            p,
            pick_aimless_dest(&l, seed, desk),
            "must stay deterministic in (layout, seed)"
        );
    }
}

#[test]
fn aimless_fallback_on_a_fully_blocked_mask_returns_the_desk_anchor() {
    // Degenerate layout (nothing walkable at all): the desk anchor is a
    // destination every consumer already handles (A* snap / render anchor).
    let mut l = layout();
    let mut mask = crate::walkable::WalkableMask::new_open(l.buf_w, l.buf_h);
    mask.mark_blocked(0, 0, l.buf_w, l.buf_h, 0);
    l.walkable = mask;

    let desk = l.home_desks[0];
    for seed in 0..8u64 {
        assert_eq!(
            pick_aimless_dest(&l, seed, desk),
            // The desk's OWN facing, not South: the fallback is whatever anchor
            // production would use, and desk 0 is not guaranteed viewer-facing.
            crate::layout::desk_walk_anchor_facing(desk.at, desk.facing, l.inner),
            "seed {seed}: fully blocked corridor must fall back to the desk anchor"
        );
    }
}

/// A walking person steps by the ground covered, never the clock: walks at
/// different paces turn their frames once per `stride / frames` of ground each.
#[test]
fn a_walking_person_never_slides() {
    use crate::physics::{SPEED_MULT_MAX, WalkIntent, speed_mult, walk_profile, walk_progress};
    let pack = crate::pack::test_office();
    let walk = pack.piece(Piece::Walking);
    let stride = pack.inner().stride(Walk::Walking);
    let per_frame = f32::from(stride.get()) / walk.frames().len() as f32;
    // sampled as a painter paints: a frame stepped past between two paints is lost
    let paint_ms = crate::anim::PAINT_FRAME_MS;
    let steps = |total_ms: u64, travelled_at: &dyn Fn(u64) -> u32| {
        let (mut turns, mut last) = (0.0_f32, None);
        for ms in (0..=total_ms).step_by(paint_ms as usize) {
            let frame = walk_frame(travelled_at(ms), walk, stride);
            turns += f32::from(u8::from(last.is_some_and(|f| f != frame)));
            last = Some(frame);
        }
        turns
    };
    let judge = |what: &str, turns: f32, px: f32| {
        let expected = px / per_frame;
        assert!(
            (turns - expected).abs() <= 1.0,
            "{what}: {turns} steps over {px} px, {expected} by its stride"
        );
    };
    // every routed walk at the fastest agent's pace, snap-back the fastest of all
    let fastest = (0..256)
        .map(|i| AgentId::from_transcript_path(&format!("/p/fast{i}.jsonl")))
        .max_by(|a, b| speed_mult(*a).total_cmp(&speed_mult(*b)))
        .expect("agents");
    assert!(
        speed_mult(fastest) > SPEED_MULT_MAX * 0.98,
        "a near-fastest agent"
    );
    let length = 200 * crate::physics::OCTILE_STRAIGHT_COST;
    for intent in [
        WalkIntent::Entry,
        WalkIntent::WanderOut,
        WalkIntent::SnapBack,
    ] {
        let profile = walk_profile(length, intent, fastest);
        let turns = steps(profile.duration_ms, &|ms| {
            Segment::straight(
                Point { x: 0, y: 0 },
                Point {
                    x: (length / crate::physics::OCTILE_STRAIGHT_COST) as u16,
                    y: 0,
                },
                walk_progress(&profile, ms),
            )
            .travelled()
        });
        judge(
            &format!("{intent:?}"),
            turns,
            length as f32 / crate::physics::OCTILE_STRAIGHT_COST as f32,
        );
    }
}

/// A walk steps one frame each `stride / frames` of ground, wrapping each
/// stride.
#[test]
fn a_walk_steps_by_the_ground_it_covers() {
    let pack = crate::pack::test_office();
    let walk = pack.piece(Piece::Walking);
    let stride = pack.inner().stride(Walk::Walking);
    let frames = walk.frames().len() as u32;
    let per_cycle = u32::from(stride.get()) * crate::physics::OCTILE_STRAIGHT_COST;
    // the first distance the second frame shows at
    let next = per_cycle.div_ceil(frames);
    for (travelled, frame) in [(0, 0), (next - 1, 0), (next, 1), (per_cycle, 0)] {
        assert_eq!(
            walk_frame(travelled, walk, stride),
            frame as usize,
            "{travelled} in"
        );
    }
}

/// A base-art walker is the base art's standing person with only its feet
/// and hands moved: the same head, neck, shirt and leg rows, so a sit,
/// stand or walk never reshapes them. A hand may hang beside the hips, a
/// foot may step out a column or clear the ground.
#[test]
fn a_base_walker_keeps_the_standing_figure() {
    let pack = crate::pack::test_office();
    let opaque = |f: &crate::sprite::Frame, y: u16| -> Vec<u16> {
        (0..f.width())
            .filter(|&x| f.get(x, y).copied().flatten().is_some())
            .collect()
    };
    let stand = pack.piece(Piece::Standing).first();
    let (w, h) = (stand.width(), stand.height());
    // a dropped hand hangs past the shirt's last full-width row
    let hips = 1
        + (0..h)
            .rev()
            .find(|&y| opaque(stand, y).len() == usize::from(w))
            .expect("a full-width row");
    let ground = h - 1;
    let sides = [0, w - 1];
    for walk in crate::sim::WALKS {
        let name = walk.piece().name();
        for (i, f) in pack.piece(walk.piece()).frames().iter().enumerate() {
            assert_eq!((f.width(), f.height()), (w, h), "{name} {i}");
            for y in 0..h {
                let (walk, rest) = (opaque(f, y), opaque(stand, y));
                if y == hips {
                    let extra: Vec<u16> =
                        walk.iter().filter(|x| !rest.contains(x)).copied().collect();
                    assert!(
                        extra.iter().all(|x| sides.contains(x)),
                        "{name} {i}: hips {walk:?}"
                    );
                    assert!(
                        rest.iter().all(|x| walk.contains(x)),
                        "{name} {i}: hips {walk:?}"
                    );
                } else if y == ground {
                    let steps = |x: &u16| rest.iter().any(|r| r.abs_diff(*x) <= 1);
                    assert!(walk.iter().all(steps), "{name} {i}: feet {walk:?}");
                } else {
                    assert_eq!(walk, rest, "{name} {i}: row {y}");
                }
            }
        }
    }
}

/// A walker's dust rises under a foot on the ground, in every frame of its
/// walk, heading east or mirrored west; on a passing frame, where only one
/// foot touches the ground, under that one.
#[test]
fn a_walker_s_dust_rises_under_its_planted_foot() {
    let pack = crate::pack::test_office();
    let mut passing = 0;
    for walk in crate::sim::WALKS {
        let name = walk.piece().name();
        let frames = pack.piece(walk.piece()).frames();
        for (i, f) in frames.iter().enumerate() {
            for flip in [false, true] {
                let ground = f.height() - 1;
                let on_ground = |foot: usize| {
                    let x =
                        crate::effects::look::walking_dust_foot(Point { x: 0, y: 0 }, foot as u64)
                            .x;
                    let art_x = if flip { f.width() - 1 - x } else { x };
                    f.get(art_x, ground).copied().flatten().is_some()
                };
                let foot = crate::effects::planted_foot(i, frames.len(), flip);
                assert!(
                    on_ground(foot),
                    "{name} {i} (flip {flip}): dust under a lifted foot"
                );
                if !on_ground(1 - foot) {
                    passing += 1;
                }
            }
        }
    }
    assert!(
        passing > 0,
        "the walks must pass a foot to tell the feet apart"
    );
}

/// A walk bears its weight on the art's east foot from the strike that opens
/// its cycle and on its west from the one halfway; mirrored, the other.
#[test]
fn the_planted_foot_changes_at_the_halfway_strike() {
    let cycles: [&[usize]; 2] = [&[0, 1], &[0, 0, 0, 0, 1, 1, 1, 1]];
    for east in cycles {
        let frames = east.len();
        for (i, &foot) in east.iter().enumerate() {
            assert_eq!(
                crate::effects::planted_foot(i, frames, false),
                foot,
                "{i} of {frames}"
            );
            assert_eq!(
                crate::effects::planted_foot(i, frames, true),
                1 - foot,
                "{i} of {frames}, mirrored"
            );
        }
    }
}
