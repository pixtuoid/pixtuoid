use super::*;
use crate::anim::{PAINT_FRAME, frames_in};
use crate::character::test_support::make_slot;
use crate::physics::{LegProgress, octile_distance, octile_path_len, walk_profile};
use crate::walk::settle_len;
use crate::walkable::WalkableMask;
use pixtuoid_core::state::{ActivityState, FloorDesk, ToolKind, ToolUse};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Stub router: returns a pre-baked polyline instead of running A*.
#[derive(Debug)]
struct StubRouter {
    path: Vec<Point>,
}

impl StubRouter {
    /// Straight-line: `route` returns `[from, to]` regardless of input.
    fn straight() -> Self {
        Self { path: vec![] }
    }
    /// Hardcoded polyline, returned regardless of the requested endpoints.
    fn corners(path: Vec<Point>) -> Self {
        Self { path }
    }
}

impl Router for StubRouter {
    fn new_layout(&mut self, _: crate::layout::Bounds) {}
    fn route(
        &mut self,
        _: &WalkableMask,
        _: &crate::walkable::OccupancyOverlay,
        from: Point,
        to: Point,
    ) -> Vec<Point> {
        if self.path.is_empty() {
            vec![from, to]
        } else {
            self.path.clone()
        }
    }
}

fn layout() -> SceneLayout {
    SceneLayout::compute(crate::layout::Viewport::full(120, 96), Some(4), 0).expect("fits")
}

/// Returns a stable polyline (`first`) for its first few calls then a DIFFERENT
/// one (`rest`) — an overlay-driven A* reroute mid-walk. Counts calls.
#[derive(Debug)]
struct ChangingRouter {
    calls: usize,
    first: Vec<Point>,
    rest: Vec<Point>,
}
impl Router for ChangingRouter {
    fn new_layout(&mut self, _: crate::layout::Bounds) {}
    fn route(
        &mut self,
        _: &WalkableMask,
        _: &crate::walkable::OccupancyOverlay,
        _from: Point,
        _to: Point,
    ) -> Vec<Point> {
        self.calls += 1;
        // Frame 1 makes two calls (entry-profile snapshot + route_walking_pose)
        // that must agree, so only call #3+ can be a later-frame reroute.
        if self.calls <= 2 {
            self.first.clone()
        } else {
            self.rest.clone()
        }
    }
}

#[test]
fn walk_leg_freezes_path_against_midleg_reroute() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let door = l.door_threshold;
    let desk = l.home_desks[0];
    let desk_target = Point {
        x: desk.at.x + 6,
        y: desk.at.y + 4,
    };
    let mid_a = Point {
        x: door.x,
        y: u16::midpoint(door.y, desk_target.y),
    };
    let mid_b = Point {
        x: desk_target.x,
        y: door.y,
    };
    assert_ne!(mid_a, mid_b, "test setup: corners must differ");

    let mut rig = RouteRig::new(ChangingRouter {
        calls: 0,
        first: vec![door, mid_a, desk_target],
        rest: vec![door, mid_b, desk_target],
    });

    let slot1 = entry_slot(now - Duration::from_millis(200));
    let _ = derive_with_routing(&slot1, now, &l, &mut rig.rctx());
    let calls_after_frame1 = rig.router.calls;

    let slot2 = entry_slot(now - Duration::from_millis(200));
    let later = now + Duration::from_millis(100);
    let _ = derive_with_routing(&slot2, later, &l, &mut rig.rctx());

    assert_eq!(
        rig.router.calls,
        calls_after_frame1,
        "frozen leg must not re-route on a later frame (got {} extra calls)",
        rig.router.calls - calls_after_frame1
    );

    let frozen = rig
        .walks
        .get(&slot2.agent_id)
        .and_then(|walk| walk.walk_path.as_ref())
        .expect("walk_path must be snapshotted while walking");
    assert!(
        frozen.path.contains(&mid_a),
        "frozen path must keep the first leg's corner {mid_a:?}, got {:?}",
        frozen.path
    );
    assert!(
        !frozen.path.contains(&mid_b),
        "frozen path must NOT adopt the rerouted corner {mid_b:?} mid-leg, got {:?}",
        frozen.path
    );
}

fn running_slot(state_started_at: SystemTime, arrived_at: SystemTime) -> AgentSlot {
    AgentSlot {
        cwd: Arc::from(PathBuf::from("/p").as_path()),
        label: "cc".into(),
        state_started_at,
        last_event_at: arrived_at,
        created_at: arrived_at,
        arrived_at: Some(arrived_at),
        ..make_slot(
            AgentId::from_transcript_path("/snap.jsonl"),
            ActivityState::Running {
                item: Some(ToolUse {
                    tool_use_id: Some(Arc::from("t")),
                    detail: Some(Arc::from("Edit")),
                    kind: ToolKind::Edit,
                }),
            },
        )
    }
}

fn entry_slot(arrived_at: SystemTime) -> AgentSlot {
    let mut s = running_slot(arrived_at, arrived_at);
    s.state = ActivityState::Idle;
    s
}

/// The entry walk runs from a slot's arrival, never its age, in the stateless
/// pose, the routed one and the door's frames: an old slot that just arrived
/// walks in, and a fresh one that never arrived sits at its desk.
#[test]
fn the_entry_walk_keys_on_arrival_not_age() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let probe = now + Duration::from_millis(ENTRY_ANIMATION_MS / 4);
    let arrived = AgentSlot {
        arrived_at: Some(now),
        ..running_slot(now, now - Duration::from_secs(3_600))
    };
    let seated = AgentSlot {
        arrived_at: None,
        ..running_slot(now, now)
    };
    let walking = |pose: Option<Pose>| matches!(pose, Some(Pose::Walking { .. }));
    assert!(walking(crate::pose::pure::derive(&arrived, probe, &l)));
    assert!(!walking(crate::pose::pure::derive(&seated, probe, &l)));
    let mut rig = RouteRig::new(StubRouter::straight());
    assert!(walking(derive_with_routing(
        &arrived,
        probe,
        &l,
        &mut rig.rctx()
    )));
    let mut rig = RouteRig::new(StubRouter::straight());
    assert!(!walking(derive_with_routing(
        &seated,
        probe,
        &l,
        &mut rig.rctx()
    )));
    let door = |slot: &AgentSlot| {
        crate::sim::anchors::compute_door_frame_idx(std::slice::from_ref(slot), probe, 0)
    };
    assert_ne!(door(&arrived), 0, "the door opens for an arrival");
    assert_eq!(door(&seated), 0, "and stays shut for one already in");
}

#[test]
fn seated_waypoint_snap_back_starts_from_the_seat_not_the_approach_cell() {
    // Unlike the hand-injected snap-back tests, this drives the real AtWaypoint
    // arm — the one seated departure the WalkingBack Settle machinery misses.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let desk = l.home_desks[0];
    // Distinct seat vs approach cells, both far from the desk chair (snap arms).
    let seat = Point {
        x: desk.at.x + 50,
        y: desk.at.y + 30,
    };
    let approach = Point {
        x: desk.at.x + 56,
        y: desk.at.y + 30,
    };
    let mut rig = RouteRig::new(StubRouter::straight());

    let idle = entry_slot(now - Duration::from_secs(60));
    let mut walk = WalkState::default();
    walk.wander.phase = crate::walk::WanderPhase::AtWaypoint(walk_profile(
        100,
        WalkIntent::WanderBack,
        idle.agent_id,
    ));
    walk.wander.phase_started_at = now;
    walk.wander.last_advanced_at = now; // pin the phase (advance_wander no-ops at now)
    walk.wander.target = crate::walk::WanderTarget {
        dest: approach,
        kind: crate::walk::WanderKind::Named {
            wp_idx: 0,
            kind: crate::layout::WaypointKind::Couch,
            seat: Some(seat),
        },
    };
    rig.walks.insert(idle.agent_id, walk);
    match derive_with_routing(&idle, now, &l, &mut rig.rctx()) {
        Some(Pose::AtWaypoint { .. }) => {}
        other => panic!("expected AtWaypoint pose, got {other:?}"),
    }

    let running = running_slot(now, now - Duration::from_secs(60));
    let then = now + Duration::from_millis(50);
    match derive_with_routing(&running, then, &l, &mut rig.rctx()) {
        Some(Pose::Walking { segment, .. }) => assert_eq!(
            segment.from(),
            seat,
            "snap-back must start from the rendered seat {seat:?}, not the approach cell {approach:?}"
        ),
        other => panic!("expected snap-back Walking pose, got {other:?}"),
    }
}

#[test]
fn snap_back_origin_is_frozen_across_frames() {
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    // state_started_at == now0: the leg arms on frame 0 and stays armed (the
    // re-arm guard keys on state_started_at, which is constant here).
    let slot = running_slot(now0, now0 - Duration::from_secs(60));
    let desk = l.home_desks[0];
    // Far from the desk so the pre-fix integer-pixel drift surfaces inside the
    // 8-frame window; a snap near SNAP_BACK_MIN_DIST could delay the first
    // drift past the window and false-pass on broken code.
    let prev0 = Point {
        x: desk.at.x + 50,
        y: desk.at.y + 30,
    };
    let mut rig = RouteRig::new(StubRouter::straight());
    rig.history.set(slot.agent_id, Some(prev0));

    // Eight frames stay well inside `SNAP_BACK_MS`; re-derive each frame so
    // `derive_with_routing` records the advancing walker, as the render loop does.
    let mut origins = Vec::new();
    for i in 0..8u32 {
        let t = now0 + PAINT_FRAME * i;
        match derive_with_routing(&slot, t, &l, &mut rig.rctx()) {
            Some(Pose::Walking { segment, .. }) => origins.push((i, segment.from())),
            other => panic!("frame {i}: expected Walking pose mid snap-back, got {other:?}"),
        }
    }
    for (i, from) in origins {
        assert_eq!(
            from, prev0,
            "frame {i}: snap-back origin drifted to {from:?}; it must stay frozen at \
             the interruption point {prev0:?} for the whole leg"
        );
    }
}

#[test]
fn snap_back_cornered_leg_freezes_path_no_reroute() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let desk = l.home_desks[0];
    let snap_target = Point {
        x: desk.at.x + 6,
        y: desk.at.y + 4,
    };
    let prev0 = Point {
        x: desk.at.x + 50,
        y: desk.at.y + 30,
    };
    let corner_a = Point {
        x: prev0.x,
        y: snap_target.y,
    };
    let corner_b = Point {
        x: snap_target.x,
        y: prev0.y,
    };
    assert_ne!(corner_a, corner_b, "test setup: corners must differ");

    let mut rig = RouteRig::new(ChangingRouter {
        calls: 0,
        first: vec![prev0, corner_a, snap_target],
        rest: vec![prev0, corner_b, snap_target],
    });

    // State flipped 100ms ago — inside `SNAP_BACK_MS` on both frames.
    let slot = running_slot(
        now - Duration::from_millis(100),
        now - Duration::from_secs(60),
    );
    rig.history.set(slot.agent_id, Some(prev0));

    // The arm routes once and route_walking_pose once — both inside
    // ChangingRouter's `first` window, so they agree on the shape.
    let _ = derive_with_routing(&slot, now, &l, &mut rig.rctx());
    let calls_after_frame1 = rig.router.calls;
    assert!(
        calls_after_frame1 >= 1,
        "frame 1 must route once to snapshot the cornered leg"
    );

    let later = now + Duration::from_millis(100);
    let _ = derive_with_routing(&slot, later, &l, &mut rig.rctx());

    assert_eq!(
        rig.router.calls,
        calls_after_frame1,
        "frozen cornered snap-back must not re-route on a later frame (got {} extra calls)",
        rig.router.calls - calls_after_frame1
    );
    let frozen = rig
        .walks
        .get(&slot.agent_id)
        .and_then(|walk| walk.walk_path.as_ref())
        .expect("walk_path must be snapshotted while snapping back");
    assert!(
        frozen.path.contains(&corner_a),
        "frozen path must keep the first corner {corner_a:?}, got {:?}",
        frozen.path
    );
    assert!(
        !frozen.path.contains(&corner_b),
        "frozen path must NOT adopt the rerouted corner {corner_b:?} mid-leg, got {:?}",
        frozen.path
    );
}

/// A record is the last place the agent was drawn: one drawn seated since it
/// stood at a waypoint, its floor hidden meanwhile, changes state or quits
/// from its desk, not from the waypoint, whether it went to work or sat back
/// down idle.
#[test]
fn a_seated_draw_forgets_the_waypoint_it_last_stood_at() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let desk = l.home_desks[0];
    let waypoint = Point {
        x: desk.at.x + 16,
        y: desk.at.y + 12,
    };
    let stood = |slot: &AgentSlot| {
        let mut rig = RouteRig::new(StubRouter::straight());
        rig.history.set(slot.agent_id, Some(waypoint));
        rig
    };
    // to work while hidden, shown again past the snap-back window, then asked
    let mut slot = running_slot(t0 + Duration::from_secs(1), t0 - Duration::from_secs(60));
    let mut rig = stood(&slot);
    let shown = slot.state_started_at + Duration::from_millis(SNAP_BACK_MS) + PAINT_FRAME;
    let seated = derive_with_routing(&slot, shown, &l, &mut rig.rctx());
    assert_eq!(
        seated,
        Some(Pose::SeatedTyping),
        "premise: shown again at its desk"
    );
    let asked = shown + Duration::from_secs(1);
    slot.state = ActivityState::RequiresAction { reason: None };
    slot.state_started_at = asked;
    let pose = derive_with_routing(&slot, asked, &l, &mut rig.rctx());
    assert!(
        !matches!(pose, Some(Pose::Walking { .. })),
        "asked, it walked back from the waypoint: {pose:?}"
    );
    // idle again while hidden, shown re-seated, then quits
    let mut slot = AgentSlot {
        state: ActivityState::Idle,
        ..running_slot(t0 + Duration::from_secs(1), t0 - Duration::from_secs(60))
    };
    let mut rig = stood(&slot);
    let shown = t0 + Duration::from_secs(2);
    let seated = derive_with_routing(&slot, shown, &l, &mut rig.rctx());
    assert_eq!(seated, Some(Pose::SeatedIdle), "premise: shown re-seated");
    let quit = shown + Duration::from_secs(1);
    slot.exiting_at = Some(quit);
    let pose = derive_with_routing(&slot, quit, &l, &mut rig.rctx());
    assert!(
        !matches!(pose, Some(Pose::Walking { segment, .. }) if segment.from() == waypoint),
        "quitting, it walked out from the waypoint: {pose:?}"
    );
}

#[test]
fn snap_back_derive_is_idempotent_within_a_frame() {
    // A repeat derive at the SAME `now` must change neither the pose nor the
    // shared history and walks — hence the inner k-loop.
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = running_slot(now0, now0 - Duration::from_secs(60));
    let desk = l.home_desks[0];
    // Far enough from the CHAIR to arm (≥ SNAP_BACK_MIN_DIST); the chair-distance
    // gate then stops it re-arming once the walk reaches the chair, so it settles.
    let prev0 = Point {
        x: desk.at.x + 16,
        y: desk.at.y + 12,
    };
    let mut rig = RouteRig::new(StubRouter::straight());
    rig.history.set(slot.agent_id, Some(prev0));

    let mut arrived_frame: Option<u32> = None;
    for i in 0..60u32 {
        let t = now0 + PAINT_FRAME * i;
        let p0 = derive_with_routing(&slot, t, &l, &mut rig.rctx());
        let h0 = rig.history.last_drawn(slot.agent_id);
        if arrived_frame.is_none() && matches!(p0, Some(Pose::SeatedTyping | Pose::SeatedIdle)) {
            arrived_frame = Some(i);
        }
        for k in 1..4 {
            let pk = derive_with_routing(&slot, t, &l, &mut rig.rctx());
            let hk = rig.history.last_drawn(slot.agent_id);
            assert_eq!(
                p0, pk,
                "frame {i} call {k}: pose differs within one frame ({p0:?} vs {pk:?}) — K-call desync"
            );
            assert_eq!(
                h0, hk,
                "frame {i} call {k}: history advanced within one frame ({h0:?} vs {hk:?})"
            );
        }
    }
    assert!(
        arrived_frame.is_some(),
        "snap-back should reach the desk (settle to seated) within the run"
    );
}

#[test]
fn wander_derive_is_idempotent_within_a_frame() {
    use crate::pathfind::AStarRouter;
    use crate::sim::anchors::character_top_left;

    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let trip_id = (0u64..1000)
        .map(|i| AgentId::from_transcript_path(&format!("/idem/{i}.jsonl")))
        .find(|id| takes_trip(*id, 0))
        .expect("find a trip agent");
    let old = now0 - Duration::from_secs(120);
    let mut slot = entry_slot(old);
    slot.agent_id = trip_id;
    slot.last_event_at = old;

    let mut rig = RouteRig::new(AStarRouter::new());
    rig.router.new_layout(l.corridor);

    for i in 0..200u32 {
        let t = now0 + PAINT_FRAME * i;
        let a0 = character_top_left(&slot, &l, t, &mut rig.rctx());
        for k in 1..4 {
            let ak = character_top_left(&slot, &l, t, &mut rig.rctx());
            assert_eq!(
                a0, ak,
                "frame {i} call {k}: wander anchor differs within one frame ({a0:?} vs {ak:?}) — K-call desync"
            );
        }
    }
}

#[test]
fn snap_back_long_distance_renders_past_window_by_physics() {
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = running_slot(now0, now0 - Duration::from_secs(60));
    let desk = l.home_desks[0];
    // Far prev → SnapBack physics duration well over the `SNAP_BACK_MS` arm window.
    let prev = Point {
        x: desk.at.x + 50,
        y: desk.at.y + 30,
    };
    let mut rig = RouteRig::new(StubRouter::straight());
    rig.history.set(slot.agent_id, Some(prev));

    let (mut walking_after_window, mut arrived) = (false, false);
    for i in 0..90u32 {
        let t = now0 + PAINT_FRAME * i;
        match derive_with_routing(&slot, t, &l, &mut rig.rctx()) {
            Some(Pose::Walking { .. }) if PAINT_FRAME * i > Duration::from_millis(SNAP_BACK_MS) => {
                walking_after_window = true
            }
            Some(Pose::Walking { .. }) => {}
            Some(Pose::SeatedTyping | Pose::SeatedIdle) if walking_after_window => {
                arrived = true;
                break;
            }
            _ => {}
        }
    }
    assert!(
        walking_after_window,
        "far snap-back must keep WALKING past the {SNAP_BACK_MS}ms arm window — not compressed or window-capped"
    );
    assert!(
        arrived,
        "far snap-back must eventually settle to the seated pose by physics"
    );
}

#[test]
fn snap_back_routes_via_the_approach_cell_then_settles_onto_the_chair() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let desk_index = (0..l.home_desks.len())
        .find(|&i| desk_approach_cell(l.home_desks[i], &l).is_some())
        .expect("a desk with a valid approach cell");
    let desk = l.home_desks[desk_index];
    let chair = desk_walk_anchor_facing(desk.at, desk.facing, l.inner);
    let approach = desk_approach_cell(desk, &l).expect("approach cell");

    let mut slot = running_slot(now, now - Duration::from_secs(60));
    slot.home_desk = Some(FloorDesk::new(0, desk_index));
    slot.agent_id = AgentId::from_transcript_path("/snapapproach/slot.jsonl");
    // Far from the chair (≥ SNAP_BACK_MIN_DIST) so the snap-back arms.
    let prev = Point {
        x: chair.x + 40,
        y: chair.y + 25,
    };
    let mut rig = RouteRig::new(StubRouter::straight());
    rig.history.set(slot.agent_id, Some(prev));

    let pose =
        derive_with_routing(&slot, now, &l, &mut rig.rctx()).expect("snap-back renders a pose");
    assert!(
        matches!(pose, Pose::Walking { .. }),
        "snap-back must be Walking, got {pose:?}"
    );
    let snap = rig.walks[&slot.agent_id]
        .walk_path
        .as_ref()
        .expect("the cornered snap-back leg is frozen (… approach, chair, len > 2)");
    assert_eq!(
        snap.path.last(),
        Some(&chair),
        "snap-back must settle onto the chair; got {:?}",
        snap.path
    );
    assert_eq!(
        snap.path[snap.path.len() - 2],
        approach,
        "snap-back must arrive via the N/E/W approach cell, not straight at the chair; got {:?}",
        snap.path
    );
}

#[test]
fn snap_back_skipped_when_prev_within_min_distance() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = running_slot(now, now - Duration::from_secs(60));
    let desk = l.home_desks[0];
    // 3 px from the SEAT, not from the desk origin — the threshold is measured
    // against where the agent sits, and the seat carries a per-desk nudge.
    let seat = crate::layout::desk_walk_anchor_facing(desk.at, desk.facing, l.inner);
    let close = Point {
        x: seat.x,
        y: seat.y.saturating_sub(3),
    };
    let mut rig = RouteRig::new(StubRouter::straight());
    rig.history.set(slot.agent_id, Some(close));
    let p = derive_with_routing(&slot, now, &l, &mut rig.rctx());
    assert!(
        matches!(p, Some(Pose::SeatedTyping)),
        "close prev should NOT trigger snap-back, got {p:?}"
    );
}

#[test]
fn snap_back_skipped_past_its_window() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = running_slot(
        now - Duration::from_millis(2 * SNAP_BACK_MS),
        now - Duration::from_secs(60),
    );
    let desk = l.home_desks[0];
    let prev = Point {
        x: desk.at.x + 50,
        y: desk.at.y + 30,
    };
    let mut rig = RouteRig::new(StubRouter::straight());
    rig.history.set(slot.agent_id, Some(prev));
    let p = derive_with_routing(&slot, now, &l, &mut rig.rctx());
    assert!(
        matches!(p, Some(Pose::SeatedTyping)),
        "snap-back window should be expired at 1.5s, got {p:?}"
    );
}

#[test]
fn snap_back_skipped_without_history() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = running_slot(now, now - Duration::from_secs(60));
    let mut rig = RouteRig::new(StubRouter::straight());
    let p = derive_with_routing(&slot, now, &l, &mut rig.rctx());
    assert!(
        matches!(p, Some(Pose::SeatedTyping)),
        "no prev history → raw pose, got {p:?}"
    );
}

#[test]
fn multi_segment_path_maps_progress_to_segment_via_octile_distance() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = entry_slot(now - Duration::from_millis(400));
    let door = l.door_threshold;
    let desk = l.home_desks[0];
    let mid = Point {
        x: u16::midpoint(door.x, desk.at.x),
        y: u16::midpoint(door.y, desk.at.y),
    };
    let mut rig = RouteRig::new(StubRouter::corners(vec![door, mid, desk.at]));
    let p = derive_with_routing(&slot, now, &l, &mut rig.rctx());
    match p {
        Some(Pose::Walking { segment, .. }) => {
            let from = segment.from();
            let to = segment.to();
            assert_eq!(from, door, "first segment starts at door, got {from:?}");
            assert_eq!(to, mid, "first segment ends at mid, got {to:?}");
            assert!(
                segment.travelled()
                    <= Segment::straight(door, mid, LegProgress::of(1, 2)).travelled(),
                "expected first-segment progress in [0,half], got {segment:?}"
            );
            assert!(rig.history.last_drawn(slot.agent_id).is_some());
        }
        other => panic!("expected Walking on segment 0, got {other:?}"),
    }
}

#[test]
fn at_waypoint_pose_records_position_to_history() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = AgentSlot {
        cwd: Arc::from(PathBuf::from("/p").as_path()),
        label: "cc".into(),
        state_started_at: now,
        created_at: now - Duration::from_secs(60),
        arrived_at: Some(now - Duration::from_secs(60)),
        last_event_at: now - Duration::from_secs(60),
        ..make_slot(
            AgentId::from_transcript_path("/idle.jsonl"),
            ActivityState::Idle,
        )
    };
    let mut rig = RouteRig::new(StubRouter::straight());
    let _ = derive_with_routing(&slot, now, &l, &mut rig.rctx());
    assert!(
        rig.history.last_drawn(slot.agent_id).is_none(),
        "SeatedIdle should not write history"
    );
}

#[test]
fn delegates_to_derive_for_oob_desk() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let mut slot = running_slot(now, now - Duration::from_secs(60));
    slot.home_desk = Some(FloorDesk::new(0, 999));
    let mut rig = RouteRig::new(StubRouter::straight());
    assert!(derive_with_routing(&slot, now, &l, &mut rig.rctx()).is_none());
}

#[test]
fn pose_history_holds_the_last_draw_or_nothing() {
    let id = AgentId::from_transcript_path("/test/a.jsonl");
    let pt = Point { x: 42, y: 99 };
    let mut history = PoseHistory::new();
    assert!(history.last_drawn(id).is_none());
    history.set(id, Some(pt));
    assert_eq!(history.last_drawn(id), Some(pt));
    history.set(id, None);
    assert_eq!(history.last_drawn(id), None, "a draw elsewhere forgets it");
}

#[test]
fn snap_back_progress_is_physics_eased_not_linear() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = running_slot(now, now - Duration::from_secs(60));
    let desk = l.home_desks[0];
    // Far from the CHAIR (≥ SNAP_BACK_MIN_DIST) so the snap-back arms.
    let prev = Point {
        x: desk.at.x + 20,
        y: desk.at.y + 18,
    };

    let mut rig = RouteRig::new(StubRouter::straight());
    rig.history.set(slot.agent_id, Some(prev));

    let _pose0 = derive_with_routing(&slot, now, &l, &mut rig.rctx());
    let walk = rig
        .walks
        .get(&slot.agent_id)
        .expect("WalkState created on frame 0");
    let profile = &walk
        .snap_back
        .as_ref()
        .expect("snap_back profile stored")
        .profile;
    let dur_ms = profile.duration_ms;
    let path_len = profile.path_len_octile;
    assert!(
        dur_ms > 0,
        "profile duration must be > 0 for a non-trivial distance"
    );

    // Last drawn at `prev`.
    let slot_q = running_slot(now, now - Duration::from_secs(60));
    let quarter_now = now + Duration::from_millis(dur_ms / 4);
    rig.history = PoseHistory::new();
    rig.history.set(slot_q.agent_id, Some(prev));
    let p = derive_with_routing(&slot_q, quarter_now, &l, &mut rig.rctx());

    match p {
        Some(Pose::Walking { segment, .. }) => {
            let travelled = segment.travelled();
            let route = &rig.walks[&slot_q.agent_id]
                .walk_path
                .as_ref()
                .expect("the chair settle corners the route, so it is frozen")
                .path;
            let linear = Segment::on_route(route, LegProgress::of(dur_ms / 4, dur_ms)).travelled();
            assert!(
                travelled < linear,
                "physics ease-in: expected behind a linear walk at 25% of duration, got {travelled} of {path_len}, linear {linear}"
            );
        }
        other => panic!("expected Walking pose at 25% of snap-back duration, got {other:?}"),
    }
}

#[test]
fn snap_back_rearms_on_new_state_transition() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let desk = l.home_desks[0];
    let mut rig = RouteRig::new(StubRouter::straight());

    let t0 = now;
    let slot0 = running_slot(t0, now - Duration::from_secs(60));
    let prev0 = Point {
        x: desk.at.x + 50,
        y: desk.at.y + 30,
    };
    rig.history.set(slot0.agent_id, Some(prev0));
    let _ = derive_with_routing(&slot0, t0, &l, &mut rig.rctx());
    let stored0 = rig
        .walks
        .get(&slot0.agent_id)
        .and_then(|walk| walk.snap_back.as_ref())
        .map(|leg| leg.started_at)
        .expect("snap_back armed at T0");
    assert_eq!(stored0, t0, "first arm should key on T0 state_started_at");

    // A NEW transition inside the window. Same agent_id (running_slot uses a fixed
    // transcript path) so its walk state is reused; only state_started_at moved.
    let t1_state = t0 + Duration::from_millis(400);
    let slot1 = running_slot(t1_state, now - Duration::from_secs(60));
    let now1 = t1_state;
    let prev1 = Point {
        x: desk.at.x + 40,
        y: desk.at.y + 25,
    };
    rig.history.set(slot1.agent_id, Some(prev1));
    let _ = derive_with_routing(&slot1, now1, &l, &mut rig.rctx());
    let stored1 = rig
        .walks
        .get(&slot1.agent_id)
        .and_then(|walk| walk.snap_back.as_ref())
        .map(|leg| leg.started_at)
        .expect("snap_back still present after new transition");
    assert_eq!(
        stored1, t1_state,
        "snap-back must re-arm to the NEW state_started_at, not the stale T0"
    );
    assert_ne!(
        stored1, t0,
        "re-armed clock must differ from the old T0 clock"
    );
}

fn entry_slot_near(arrived_at: SystemTime) -> AgentSlot {
    let mut s = running_slot(arrived_at, arrived_at);
    s.state = pixtuoid_core::state::ActivityState::Idle;
    s.home_desk = Some(FloorDesk::new(0, 0));
    s
}

fn entry_slot_far(arrived_at: SystemTime, desk_index: usize) -> AgentSlot {
    let mut s = entry_slot_near(arrived_at);
    s.home_desk = Some(FloorDesk::new(0, desk_index));
    // Give each far slot a distinct agent_id so speed_mult differs.
    s.agent_id = AgentId::from_transcript_path(&format!("/far/{desk_index}.jsonl"));
    s
}

fn exiting_slot(exiting_at: SystemTime, created_at: SystemTime) -> AgentSlot {
    let mut s = running_slot(exiting_at - Duration::from_secs(30), created_at);
    s.exiting_at = Some(exiting_at);
    s.agent_id = AgentId::from_transcript_path("/exit/slot.jsonl");
    s
}

/// `(near, far)` desk indices by octile distance from the door.
fn near_far_desk_indices(l: &SceneLayout) -> (usize, usize) {
    let door = l.door_threshold;
    let dists: Vec<u32> = l
        .home_desks
        .iter()
        .map(|d| {
            let target = Point {
                x: d.at.x + 6,
                y: d.at.y + 4,
            };
            octile_distance(door, target)
        })
        .collect();
    let near_idx = dists
        .iter()
        .enumerate()
        .min_by_key(|&(_, d)| d)
        .map(|(i, _)| i)
        .unwrap();
    let far_idx = dists
        .iter()
        .enumerate()
        .max_by_key(|&(_, d)| d)
        .map(|(i, _)| i)
        .unwrap();
    assert_ne!(
        dists[near_idx], dists[far_idx],
        "need distinct near/far distances for this test"
    );
    assert!(
        dists[far_idx] >= dists[near_idx] * 3 / 2,
        "far dist ({}) must be ≥ 1.5× near dist ({}) for a meaningful test",
        dists[far_idx],
        dists[near_idx]
    );
    (near_idx, far_idx)
}

#[test]
fn nearer_desk_arrives_before_farther_desk() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let (near_idx, far_idx) = near_far_desk_indices(&l);

    let near = entry_slot_far(now, near_idx);
    let far = entry_slot_far(now, far_idx);

    let mut rig_near = RouteRig::new(StubRouter::straight());
    let mut rig_far = RouteRig::new(StubRouter::straight());

    let _ = derive_with_routing(&near, now, &l, &mut rig_near.rctx());
    let _ = derive_with_routing(&far, now, &l, &mut rig_far.rctx());

    // One ms past the near desk's full trip, still inside the far desk's window.
    let near_profile = rig_near.walks[&near.agent_id]
        .entry
        .as_ref()
        .unwrap()
        .profile;
    let done_ms = near_profile.duration_ms + near_profile.pause_ms + 1;
    let t1 = now + Duration::from_millis(done_ms);

    let p_near = derive_with_routing(&near, t1, &l, &mut rig_near.rctx());
    let p_far = derive_with_routing(&far, t1, &l, &mut rig_far.rctx());

    assert!(
        !matches!(p_near, Some(Pose::Walking { .. })),
        "near desk must have arrived (no longer Walking), got {p_near:?}"
    );
    assert!(
        matches!(p_far, Some(Pose::Walking { .. })),
        "far desk must still be Walking, got {p_far:?}"
    );
}

#[test]
fn five_same_created_at_agents_have_distinct_entry_durations() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let mut rig = RouteRig::new(StubRouter::straight());

    let ids: Vec<AgentId> = (0..5)
        .map(|i| AgentId::from_transcript_path(&format!("/stagger/{i}.jsonl")))
        .collect();

    let mut durations = Vec::new();
    for &id in &ids {
        let mut slot = entry_slot_near(now);
        slot.agent_id = id;
        let _ = derive_with_routing(&slot, now, &l, &mut rig.rctx());
        let dur = rig.walks[&id]
            .entry
            .as_ref()
            .expect("entry profile set")
            .profile
            .duration_ms;
        durations.push(dur);
    }

    let unique: std::collections::HashSet<u64> = durations.iter().copied().collect();
    assert!(
        unique.len() >= 4,
        "expected ≥4 distinct durations among 5 agents, got {unique:?}"
    );
}

#[test]
fn exit_profile_snapshotted_once_not_on_subsequent_calls() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = exiting_slot(now, now - Duration::from_secs(60));
    let mut rig = RouteRig::new(StubRouter::straight());

    let _ = derive_with_routing(&slot, now, &l, &mut rig.rctx());
    let started_at_1 = rig.walks[&slot.agent_id]
        .exit
        .as_ref()
        .expect("exit profile set on first call")
        .started_at;

    let t1 = now + Duration::from_millis(100);
    let _ = derive_with_routing(&slot, t1, &l, &mut rig.rctx());
    let started_at_2 = rig.walks[&slot.agent_id]
        .exit
        .as_ref()
        .expect("exit profile still present")
        .started_at;

    assert_eq!(
        started_at_1, started_at_2,
        "exit started_at must not change on subsequent calls"
    );
}

#[test]
fn exit_far_completes_before_grace_window_no_vanish() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let door = l.door_threshold;
    let desk = l.home_desks[0];
    let from = Point {
        x: desk.at.x + 6,
        y: desk.at.y + 4,
    };
    // A long synthetic route, so the physics exit duration exceeds the exit
    // budget and the compression path is the one under test.
    let mid1 = Point {
        x: from.x.saturating_add(80),
        y: from.y,
    };
    let mid2 = Point {
        x: mid1.x,
        y: mid1.y.saturating_add(80),
    };
    let mut rig = RouteRig::new(StubRouter::corners(vec![from, mid1, mid2, door]));
    let slot = exiting_slot(
        now - (pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW - Duration::from_millis(200)),
        now - Duration::from_secs(60),
    );
    match derive_with_routing(&slot, now, &l, &mut rig.rctx()) {
        // Walking at the end of the path, or already arrived (None, GC
        // imminent) — either way NOT stuck mid-corridor.
        Some(Pose::Walking { segment, .. }) => {
            let travelled = segment.travelled();
            let route_len = rig.walks[&slot.agent_id]
                .exit
                .as_ref()
                .expect("exit profile snapshotted")
                .profile
                .path_len_octile;
            assert!(
                u64::from(travelled) * 20 >= 19 * u64::from(route_len),
                "far exit must reach the door by the grace window (no mid-corridor vanish), got {travelled} of {route_len}"
            );
        }
        None => {}
        other => panic!("expected Walking near the door or None (arrived), got {other:?}"),
    }
    let dur = rig.walks[&slot.agent_id]
        .exit
        .as_ref()
        .expect("exit profile snapshotted")
        .profile
        .duration_ms;
    assert!(
        dur > 4200,
        "test setup: exit duration {dur}ms should exceed the ~4200ms exit budget"
    );
}

#[test]
fn exit_uses_commute_speed_faster_than_wander() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = exiting_slot(now, now - Duration::from_secs(60));
    let mut rig = RouteRig::new(StubRouter::straight());

    let _ = derive_with_routing(&slot, now, &l, &mut rig.rctx());
    let profile = &rig.walks[&slot.agent_id]
        .exit
        .as_ref()
        .expect("exit profile set")
        .profile;
    let min_commute = crate::physics::V_CRUISE_COMMUTE * crate::physics::SPEED_MULT_MIN;
    let max_wander = crate::physics::V_CRUISE_WANDER * crate::physics::SPEED_MULT_MAX;
    assert!(
        min_commute > max_wander,
        "test invariant: commute and wander speed ranges must not overlap"
    );
    assert!(
        profile.v_cruise >= min_commute * 0.99, // small f32 tolerance
        "exit v_cruise {:.4} must be in commute range (>= {min_commute:.4})",
        profile.v_cruise
    );
}

/// One frame's max per-axis (Chebyshev) top-left jump: above a cruise step plus a
/// pose-type boundary, under any desk↔waypoint teleport on this layout.
const MAX_FRAME_STEP_PX: i32 = 20;

/// Step `slot` for `frames` frames of [`PAINT_FRAME`], sampling
/// `character_top_left` against a real `AStarRouter`. Returns
/// `(max_chebyshev_step, walking_frame_count)`. `churn` toggles an interior
/// obstacle every other frame to force A* cache invalidation.
fn max_top_left_step(
    slot: &AgentSlot,
    l: &SceneLayout,
    start: SystemTime,
    frames: u32,
    churn: bool,
) -> (i32, usize) {
    use crate::pathfind::AStarRouter;
    use crate::sim::anchors::character_top_left;

    let mut rig = RouteRig::new(AStarRouter::new());
    rig.router.new_layout(l.corridor);

    let ob = Point {
        x: l.corridor.x + l.corridor.width / 2,
        y: l.corridor.y + 2,
    };

    let mut prev: Option<Point> = None;
    let mut max_step = 0i32;
    let mut walking = 0usize;
    for i in 0..frames {
        let now = start + PAINT_FRAME * i;
        if churn {
            rig.overlay.clear();
            if i % 2 == 0 {
                rig.overlay
                    .add(ob.x.saturating_sub(5), ob.y.saturating_sub(5), 12, 12);
            }
        }
        if let Some(a) = character_top_left(slot, l, now, &mut rig.rctx()) {
            if let Some(p) = prev {
                let step = (i32::from(a.x) - i32::from(p.x))
                    .abs()
                    .max((i32::from(a.y) - i32::from(p.y)).abs());
                max_step = max_step.max(step);
            }
            prev = Some(a);
            walking += 1;
        }
    }
    (max_step, walking)
}

#[test]
fn entry_walk_coordinates_are_continuous() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = entry_slot(now);
    let (max_step, walking) = max_top_left_step(&slot, &l, now, 150, true);
    assert!(walking > 20, "entry walk should render many frames");
    assert!(
        max_step <= MAX_FRAME_STEP_PX,
        "entry walk teleported: max frame jump {max_step}px (> {MAX_FRAME_STEP_PX})"
    );
}

/// Whether a chair is BLOCKED follows from where the facing puts it relative to
/// the desk's stamped ground — it is not a property every chair has.
#[test]
fn desk_approach_cell_is_never_inside_the_blocked_desk() {
    use crate::layout::{Facing, Furniture, OBSTACLE_PAD_PX, furniture_def};
    let l = layout();
    // The ONE table `mask::stamp_ground` reads, so this can't drift when the sprite is resized.
    let def = furniture_def(Furniture::Desk);
    let fp = def.footprint.expect("the desk carries a static footprint");
    let (gx, gy) = (
        def.ground_x.offset(def.visual.w, fp.w),
        def.ground_y.offset(def.visual.h, fp.h),
    );
    let mut any_some = false;
    let mut facings: Vec<Facing> = Vec::new();
    for &home in &l.home_desks {
        let crate::layout::HomeDesk { at: desk, facing } = home;
        if !facings.contains(&facing) {
            facings.push(facing);
        }
        let chair = desk_walk_anchor_facing(desk, facing, l.inner);
        let (x0, y0) = (desk.x + gx, desk.y + gy);
        let in_pad = (x0.saturating_sub(OBSTACLE_PAD_PX)..x0 + fp.w + OBSTACLE_PAD_PX)
            .contains(&chair.x)
            && (y0.saturating_sub(OBSTACLE_PAD_PX)..y0 + fp.h + OBSTACLE_PAD_PX).contains(&chair.y);
        assert_eq!(
            l.is_walkable(chair.x, chair.y),
            !in_pad,
            "desk {desk:?} facing {facing:?} seats its occupant at {chair:?}, \
             {} the desk's routing pad — so the mask must report it \
             {walkable}",
            if in_pad { "inside" } else { "outside" },
            walkable = if in_pad { "blocked" } else { "walkable" },
        );
        // None = degenerate layout (every allowed side walled off); the entry then
        // falls back to the direct chair target. Acceptable, so not asserted.
        if let Some(cell) = desk_approach_cell(home, &l) {
            any_some = true;
            assert!(
                l.is_walkable(cell.x, cell.y),
                "approach cell {cell:?} for desk {desk:?} must be walkable \
                 (so it is neither inside the desk's blocked band nor the chair)"
            );
            assert_ne!(cell, chair, "approach cell must differ from the chair");
        }
    }
    assert!(
        any_some,
        "at least one desk in an open layout must have a valid approach cell"
    );
    assert!(
        facings.len() >= 2,
        "a pod seats its two rows on opposite sides, so this layout must exercise \
         more than one facing — got {facings:?}. Under a single facing the pad \
         check above only ever sees one side of the desk."
    );
}

#[test]
fn desk_entry_routes_around_the_desk_then_settles_onto_the_chair() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let door = l.door_threshold;

    let desk_index = (0..l.home_desks.len())
        .find(|&i| desk_approach_cell(l.home_desks[i], &l).is_some())
        .expect("a desk with a valid approach cell");
    let desk = l.home_desks[desk_index];
    let chair = desk_walk_anchor_facing(desk.at, desk.facing, l.inner);
    let approach = desk_approach_cell(desk, &l).expect("approach cell");

    let slot = entry_slot_far(now, desk_index);
    let mut rig = RouteRig::new(StubRouter::straight());

    let pose = derive_with_routing(&slot, now, &l, &mut rig.rctx())
        .expect("entering agent renders a pose");
    assert!(
        matches!(pose, Pose::Walking { .. }),
        "a fresh entry must be Walking, got {pose:?}"
    );

    let snap = rig.walks[&slot.agent_id]
        .walk_path
        .as_ref()
        .expect("the cornered entry+settle leg is frozen (len > 2)");
    assert_eq!(
        snap.path,
        vec![door, approach, chair],
        "the leg must go door→approach→chair, settling onto the chair"
    );
    assert_ne!(
        snap.path,
        vec![door, chair],
        "entry must not straight-line door→chair through the desk body"
    );
}

#[test]
fn wander_legs_approach_the_desk_via_an_allowed_side_not_through_the_front() {
    // `find_path` snaps a blocked goal to the NEAREST walkable coarse cell, which
    // for the south-facing chair is the SOUTH (corridor) side — so aiming a leg at
    // `desk_walk_anchor_facing` walks the agent up THROUGH the desk front.
    use crate::pathfind::AStarRouter;
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();

    let desk_index = (0..l.home_desks.len())
        .find(|&i| desk_approach_cell(l.home_desks[i], &l).is_some())
        .expect("a desk with a valid approach cell");
    let desk = l.home_desks[desk_index];
    let chair = desk_walk_anchor_facing(desk.at, desk.facing, l.inner);
    let approach = desk_approach_cell(desk, &l).expect("approach cell");

    let trip_id = (0u64..3000)
        .map(|i| AgentId::from_transcript_path(&format!("/deskleg/{i}.jsonl")))
        .find(|id| takes_trip(*id, 0))
        .expect("a trip agent");
    let old = now - Duration::from_secs(120);
    let mut slot = entry_slot(old);
    slot.agent_id = trip_id;
    slot.home_desk = Some(FloorDesk::new(0, desk_index));
    slot.last_event_at = old;

    let mut rig = RouteRig::new(AStarRouter::new());
    rig.router.new_layout(l.corridor);

    let (mut saw_out, mut saw_back) = (false, false);
    let mut seen_ends: Vec<(Point, Point)> = Vec::new();
    for i in 0..6000u32 {
        let t = now + PAINT_FRAME * i;
        let _ = derive_with_routing(&slot, t, &l, &mut rig.rctx());
        let Some(snap) = rig
            .walks
            .get(&trip_id)
            .and_then(|walk| walk.walk_path.as_ref())
        else {
            continue;
        };
        if let (Some(&f), Some(&la)) = (snap.path.first(), snap.path.last())
            && !seen_ends.contains(&(f, la))
        {
            seen_ends.push((f, la));
        }
        if snap.path.first() == Some(&chair) {
            saw_out = true;
            assert_eq!(
                snap.path.get(1),
                Some(&approach),
                "walk-out must leave the desk via the N/E/W approach cell, not \
                 straight through the south front; got {:?}",
                snap.path
            );
        }
        if snap.path.last() == Some(&chair) && snap.path.len() >= 2 {
            saw_back = true;
            assert_eq!(
                snap.path[snap.path.len() - 2],
                approach,
                "walk-back must arrive at the desk via the N/E/W approach cell, \
                 not the south front; got {:?}",
                snap.path
            );
        }
    }
    assert!(
        saw_out,
        "expected to observe a walk-out leg; chair={chair:?} approach={approach:?} \
         ends seen={seen_ends:?}"
    );
    assert!(
        saw_back,
        "expected to observe a walk-back leg; chair={chair:?} approach={approach:?} \
         ends seen={seen_ends:?}"
    );
}

#[test]
fn exit_walk_coordinates_are_continuous() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = exiting_slot(now, now - Duration::from_secs(60));
    let (max_step, walking) = max_top_left_step(&slot, &l, now, 200, true);
    assert!(walking > 20, "exit walk should render many frames");
    assert!(
        max_step <= MAX_FRAME_STEP_PX,
        "exit walk teleported: max frame jump {max_step}px (> {MAX_FRAME_STEP_PX})"
    );
}

#[test]
fn exit_from_desk_rises_off_the_chair_via_the_approach_cell() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let desk_index = (0..l.home_desks.len())
        .find(|&i| desk_approach_cell(l.home_desks[i], &l).is_some())
        .expect("a desk with a valid approach cell");
    let desk = l.home_desks[desk_index];
    let chair = desk_walk_anchor_facing(desk.at, desk.facing, l.inner);
    let approach = desk_approach_cell(desk, &l).expect("approach cell");

    let mut slot = exiting_slot(now, now - Duration::from_secs(300));
    slot.home_desk = Some(FloorDesk::new(0, desk_index));
    slot.agent_id = AgentId::from_transcript_path("/exitdesk/slot.jsonl");

    let mut rig = RouteRig::new(StubRouter::straight());
    // Empty history ⇒ the agent is exiting from the seated state (not mid-wander),
    // so the stored exit origin is the chair and the desk-departure path applies.

    let pose =
        derive_with_routing(&slot, now, &l, &mut rig.rctx()).expect("exiting agent renders a pose");
    assert!(
        matches!(pose, Pose::Walking { .. }),
        "a fresh exit must be Walking, got {pose:?}"
    );

    let snap = rig.walks[&slot.agent_id]
        .walk_path
        .as_ref()
        .expect("the cornered exit leg is frozen (chair → approach → door, len > 2)");
    assert_eq!(
        snap.path.first(),
        Some(&chair),
        "exit must START at the chair (Settle::Start glides off it); got {:?}",
        snap.path
    );
    assert_eq!(
        snap.path.get(1),
        Some(&approach),
        "exit must rise off the chair via the N/E/W approach cell, not dip south; \
         got {:?}",
        snap.path
    );
}

#[test]
fn wander_coffee_run_coordinates_continuous_under_churn() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let trip_id = (0u64..1000)
        .map(|i| AgentId::from_transcript_path(&format!("/cont/{i}.jsonl")))
        .find(|id| takes_trip(*id, 0))
        .expect("find a trip agent");
    // Long-idle, past entry.
    let old = now - Duration::from_secs(120);
    let mut slot = entry_slot(old);
    slot.agent_id = trip_id;
    slot.last_event_at = old;

    // 1500 frames ≈ 50 s — several full Seated→WalkingOut→AtWaypoint→WalkingBack
    // cycles, so every leg and boundary is exercised.
    let (max_step, walking) = max_top_left_step(&slot, &l, now, 1500, true);
    assert!(walking > 1000, "idle agent should render every frame");
    assert!(
        max_step <= MAX_FRAME_STEP_PX,
        "wander trip teleported: max frame jump {max_step}px (> {MAX_FRAME_STEP_PX})"
    );
}

#[test]
fn wander_interrupted_by_running_does_not_teleport() {
    use crate::pathfind::AStarRouter;
    use crate::sim::anchors::character_top_left;

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let trip_id = (0u64..1000)
        .map(|i| AgentId::from_transcript_path(&format!("/intr/{i}.jsonl")))
        .find(|id| takes_trip(*id, 0))
        .expect("find a trip agent");
    let old = now - Duration::from_secs(120);
    let mut idle = entry_slot(old);
    idle.agent_id = trip_id;
    idle.last_event_at = old;

    let mut rig = RouteRig::new(AStarRouter::new());
    rig.router.new_layout(l.corridor);
    // The desk's seated top-left, on throwaway stores — the "far from desk" reference.
    let seated = crate::sim::anchors::character_top_left(
        &idle,
        &l,
        now,
        &mut RouteRig::new(AStarRouter::new()).rctx(),
    )
    .expect("top-left");

    let mut last_pos = seated;
    let mut flip_frame = None;
    for i in 0..1500u32 {
        let t = now + PAINT_FRAME * i;
        if let Some(a) = character_top_left(&idle, &l, t, &mut rig.rctx()) {
            let d = (i32::from(a.x) - i32::from(seated.x))
                .abs()
                .max((i32::from(a.y) - i32::from(seated.y)).abs());
            last_pos = a;
            if d > 30 {
                flip_frame = Some(i);
                break;
            }
        }
    }
    let flip_frame = flip_frame.expect("agent should walk away from its desk within 50 s");

    let running = AgentSlot {
        state: ActivityState::Running {
            item: Some(ToolUse {
                tool_use_id: Some(Arc::from("t")),
                detail: Some(Arc::from("Edit")),
                kind: ToolKind::Edit,
            }),
        },
        state_started_at: now + PAINT_FRAME * flip_frame,
        ..idle.clone()
    };
    let mut prev = last_pos;
    let mut max_step = 0i32;
    for i in (flip_frame + 1)..(flip_frame + 46) {
        let t = now + PAINT_FRAME * i;
        if let Some(a) = character_top_left(&running, &l, t, &mut rig.rctx()) {
            let step = (i32::from(a.x) - i32::from(prev.x))
                .abs()
                .max((i32::from(a.y) - i32::from(prev.y)).abs());
            max_step = max_step.max(step);
            prev = a;
        }
    }
    assert!(
        max_step <= MAX_FRAME_STEP_PX,
        "interrupted wander teleported back to desk: max frame jump {max_step}px (> {MAX_FRAME_STEP_PX})"
    );
}

#[test]
fn floor_offscreen_then_resume_does_not_replay() {
    // An off-screen floor is simply not rendered, so its walks freeze: the
    // fixture warms up, SKIPS a long gap (no calls at all), then resumes.
    use crate::pathfind::AStarRouter;
    use crate::sim::anchors::character_top_left;

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let trip_id = (0u64..1000)
        .map(|i| AgentId::from_transcript_path(&format!("/floor/{i}.jsonl")))
        .find(|id| takes_trip(*id, 0))
        .expect("find a trip agent");
    let old = now - Duration::from_secs(120);
    let mut slot = entry_slot(old);
    slot.agent_id = trip_id;
    slot.last_event_at = old;

    let mut rig = RouteRig::new(AStarRouter::new());
    rig.router.new_layout(l.corridor);

    for i in 0..60u32 {
        let t = now + PAINT_FRAME * i;
        let _ = character_top_left(&slot, &l, t, &mut rig.rctx());
    }

    // Frames 60..1000 are NOT rendered — the off-screen gap.
    let mut prev: Option<Point> = None;
    let mut max_step = 0i32;
    for i in 1000..1120u32 {
        let t = now + PAINT_FRAME * i;
        if let Some(a) = character_top_left(&slot, &l, t, &mut rig.rctx()) {
            if let Some(p) = prev {
                let step = (i32::from(a.x) - i32::from(p.x))
                    .abs()
                    .max((i32::from(a.y) - i32::from(p.y)).abs());
                max_step = max_step.max(step);
            }
            prev = Some(a);
        }
    }
    assert!(
        max_step <= MAX_FRAME_STEP_PX,
        "floor resume replayed/teleported: max frame jump {max_step}px (> {MAX_FRAME_STEP_PX})"
    );
}

#[test]
fn exit_while_wandering_does_not_teleport_to_desk() {
    use crate::pathfind::AStarRouter;
    use crate::sim::anchors::character_top_left;

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    // A real-sized floor, not the tiny 120×96 `layout()`: in the tiny room the
    // meeting sofas are boxed in to their backrest, so a trip agent skips them and
    // never wanders far — and this test needs the agent to genuinely walk out.
    let l =
        SceneLayout::compute(crate::layout::Viewport::full(160, 120), Some(4), 0).expect("fits");
    let trip_id = (0u64..1000)
        .map(|i| AgentId::from_transcript_path(&format!("/exitw/{i}.jsonl")))
        .find(|id| takes_trip(*id, 0))
        .expect("find a trip agent");
    let old = now - Duration::from_secs(120);
    let mut idle = entry_slot(old);
    idle.agent_id = trip_id;
    idle.last_event_at = old;

    let mut rig = RouteRig::new(AStarRouter::new());
    rig.router.new_layout(l.corridor);

    let seat = character_top_left(
        &idle,
        &l,
        now,
        &mut RouteRig::new(AStarRouter::new()).rctx(),
    )
    .expect("top-left");

    let mut last = seat;
    let mut away_frame = None;
    // Several wander cycles' worth of frames, so one near-desk cycle
    // pick can't starve the away-detection.
    for i in 0..3000u32 {
        let t = now + PAINT_FRAME * i;
        if let Some(a) = character_top_left(&idle, &l, t, &mut rig.rctx()) {
            last = a;
            let d = (i32::from(a.x) - i32::from(seat.x))
                .abs()
                .max((i32::from(a.y) - i32::from(seat.y)).abs());
            // 20, not 30: "clearly away" must hold for the NEAREST legitimate trip
            // destination, and which one a cycle picks re-rolls whenever the
            // waypoint SET changes. The assertion under test is
            // exit-from-current-position, not trip length.
            if d > 20 {
                away_frame = Some(i);
                break;
            }
        }
    }
    let away_frame = away_frame.expect("agent should walk away from desk within 100 s");

    let exit_at = now + PAINT_FRAME * away_frame;
    let exiting = AgentSlot {
        exiting_at: Some(exit_at),
        ..idle.clone()
    };
    let t_next = exit_at + PAINT_FRAME;
    let first_exit = character_top_left(&exiting, &l, t_next, &mut rig.rctx()).expect("exit pose");
    let jump = (i32::from(first_exit.x) - i32::from(last.x))
        .abs()
        .max((i32::from(first_exit.y) - i32::from(last.y)).abs());
    assert!(
        jump <= MAX_FRAME_STEP_PX,
        "exit-while-wandering teleported {jump}px from the waypoint ({last:?}) to the exit start ({first_exit:?})"
    );

    let mut prev = first_exit;
    let mut max_step = 0i32;
    for i in 2..200u32 {
        let t = exit_at + PAINT_FRAME * i;
        match character_top_left(&exiting, &l, t, &mut rig.rctx()) {
            Some(a) => {
                let step = (i32::from(a.x) - i32::from(prev.x))
                    .abs()
                    .max((i32::from(a.y) - i32::from(prev.y)).abs());
                max_step = max_step.max(step);
                prev = a;
            }
            None => break, // arrived at door & GC'd
        }
    }
    assert!(
        max_step <= MAX_FRAME_STEP_PX,
        "exit-from-wander walk to door teleported: max frame jump {max_step}px"
    );
}

/// One agent on a real floor, painted a frame at a time, a pointer acting on
/// it through the floor's grip: the largest per-axis jump of its sprite
/// between two painted frames, from its first.
struct Hand {
    scene: pixtuoid_core::SceneState,
    id: AgentId,
    session: crate::floor::FloorSession,
    now: SystemTime,
    layout: Option<Arc<SceneLayout>>,
    /// Its sprite's top-left last paint, `None` where it was not drawn.
    drawn: Option<Point>,
    /// Where the pointer holds it.
    at: Option<Point>,
    max_step: i32,
}

impl Hand {
    fn new(slot: AgentSlot, now: SystemTime) -> Self {
        let mut scene = pixtuoid_core::SceneState::uniform(4);
        let id = slot.agent_id;
        scene.agents.insert(id, slot);
        let mut hand = Self {
            session: crate::floor::FloorSession::new(Arc::new(crate::pack::test_office())),
            scene,
            id,
            now,
            layout: None,
            drawn: None,
            at: None,
            max_step: 0,
        };
        hand.paint();
        hand
    }

    fn layout(&self) -> Arc<SceneLayout> {
        Arc::clone(self.layout.as_ref().expect("painted"))
    }

    fn slot(&mut self) -> &mut AgentSlot {
        self.scene.agents.get_mut(&self.id).expect("its slot")
    }

    fn grip(&mut self, gesture: crate::interact::Gesture) {
        self.session.floor_mut().grip(&gesture);
    }

    /// The next paint, carrying it on if a pointer holds it.
    fn paint(&mut self) -> Option<Pose> {
        if let Some(at) = self.at {
            self.grip(crate::interact::Gesture::Carry(at));
        }
        let stepped = self
            .session
            .step(
                crate::floor::FloorInputs {
                    scene: &self.scene,
                    now: self.now,
                    floor: crate::floor::FloorMeta::ground(),
                    pets: crate::floor::PetInputs::default(),
                },
                crate::layout::Viewport::full(160, 96),
            )
            .expect("lays out");
        self.now += PAINT_FRAME;
        let idx = stepped
            .frame
            .agents
            .iter()
            .position(|a| a.agent_id == self.id);
        let drawn = stepped
            .frame
            .characters
            .iter()
            .find(|c| Some(c.agent_idx) == idx)
            .map(|c| c.top_left);
        if let (Some(a), Some(b)) = (self.drawn, drawn) {
            let step = (i32::from(a.x) - i32::from(b.x))
                .abs()
                .max((i32::from(a.y) - i32::from(b.y)).abs());
            self.max_step = self.max_step.max(step);
        }
        self.drawn = drawn;
        self.layout = Some(stepped.layout);
        stepped.frame.poses.get(&self.id).copied().flatten()
    }

    /// Lifted by its feet, as drawn.
    fn lift(&mut self) {
        let tl = self.drawn.expect("drawn to be lifted");
        let at = Point {
            x: tl.x + crate::layout::CHARACTER_SPRITE_W / 2,
            y: tl.y + crate::layout::WALKING_Y_OFF,
        };
        self.grip(crate::interact::Gesture::Lift {
            figure: crate::interact::Figure::Agent(self.id),
            at,
        });
        self.at = Some(at);
        self.paint();
    }

    /// Carried to `to`, `step` px a paint.
    fn carry_to(&mut self, to: Point, step: u16) {
        while let Some(at) = self.at.filter(|&at| at != to) {
            let toward = |a: u16, b: u16| match a.cmp(&b) {
                std::cmp::Ordering::Less => a + step.min(b - a),
                std::cmp::Ordering::Greater => a - step.min(a - b),
                std::cmp::Ordering::Equal => a,
            };
            self.at = Some(Point {
                x: toward(at.x, to.x),
                y: toward(at.y, to.y),
            });
            self.paint();
        }
    }

    /// Set down where it is held.
    fn drop_it(&mut self) {
        let at = self.at.expect("in hand");
        self.drop_at(at);
    }

    /// Released at `to`.
    fn drop_at(&mut self, to: Point) {
        self.at = None;
        self.grip(crate::interact::Gesture::Drop(to));
    }

    /// Where its feet were drawn last paint.
    fn feet(&self) -> Point {
        let tl = self.drawn.expect("drawn");
        Point {
            x: tl.x + crate::layout::CHARACTER_SPRITE_W / 2,
            y: tl.y + crate::layout::WALKING_Y_OFF,
        }
    }

    /// Painted until it is no longer drawn, or `paints` run out: how many drew it.
    fn paint_out(&mut self, paints: u32) -> u32 {
        let mut drawn = 0;
        for _ in 0..paints {
            self.paint();
            if self.drawn.is_none() {
                break;
            }
            drawn += 1;
        }
        drawn
    }

    /// Painted until it sits at its desk, at most `paints`.
    fn paint_until_seated(&mut self, paints: u32) -> bool {
        (0..paints).any(|_| matches!(self.paint(), Some(Pose::SeatedIdle | Pose::SeatedTyping)))
    }

    /// Reachable open floor, well clear of `from`'s and the door's, that it is
    /// carried to.
    fn far_floor(&self, from: Point) -> Point {
        let l = self.layout();
        let cheb = |a: Point, b: Point| a.x.abs_diff(b.x).max(a.y.abs_diff(b.y));
        let (w, h) = (l.walkable.width(), l.walkable.height());
        let far = (0..w)
            .step_by(4)
            .flat_map(|x| (0..h).step_by(4).map(move |y| Point { x, y }))
            .filter(|&p| {
                l.walkable.is_walkable(p.x, p.y)
                    && l.reachable.reaches(p)
                    && l.is_visually_clear(p)
                    && cheb(p, l.door_threshold) > 2 * MAX_FRAME_STEP_PX as u16
            })
            .max_by_key(|&p| cheb(p, from))
            .expect("open floor");
        assert!(
            cheb(far, from) > 2 * MAX_FRAME_STEP_PX as u16,
            "setup: {far:?} must be far from {from:?} for a pop to show"
        );
        far
    }
}

/// An idle agent long settled at its desk, lifted and carried far off.
fn carried_off(now: SystemTime) -> Hand {
    let mut hand = Hand::new(entry_slot(now - Duration::from_secs(120)), now);
    hand.lift();
    let held = hand.at.expect("in hand");
    let far = hand.far_floor(held);
    hand.carry_to(far, 4);
    hand
}

#[test]
fn an_agent_whose_session_ends_in_hand_walks_out_from_it() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut hand = carried_off(now);
    let end = hand.now;
    hand.slot().exiting_at = Some(end);
    assert!(hand.paint_out(600) > 20, "it walks out");
    assert!(
        hand.max_step <= MAX_FRAME_STEP_PX,
        "max frame jump {}px (> {MAX_FRAME_STEP_PX})",
        hand.max_step
    );
}

#[test]
fn an_agent_whose_session_ends_as_it_is_set_down_walks_out_from_the_drop() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut hand = carried_off(now);
    let held = hand.at.expect("in hand");
    let l = hand.layout();
    // Released cells on from the last carry, so leaving from the hold and
    // leaving from the drop part.
    let cell = crate::pathfind::CELL_SIZE;
    let to = [held.x.checked_add(3 * cell), held.x.checked_sub(3 * cell)]
        .into_iter()
        .flatten()
        .map(|x| Point { x, y: held.y })
        .find(|&p| l.reachable.reaches(p) && l.is_visually_clear(p))
        .expect("open floor beside the hold");
    hand.drop_at(to);
    let end = hand.now;
    hand.slot().exiting_at = Some(end);
    hand.paint();
    let feet = hand.feet();
    assert!(
        feet.x.abs_diff(to.x) <= cell && feet.y.abs_diff(to.y) <= cell,
        "it walks out from the drop {to:?}, not the hold {held:?}: {feet:?}"
    );
    assert!(hand.paint_out(600) > 20, "it walks out");
    assert!(
        hand.max_step <= MAX_FRAME_STEP_PX,
        "max frame jump {}px (> {MAX_FRAME_STEP_PX})",
        hand.max_step
    );
}

/// Lifted on the very frame it comes back from an exit, it walks home from
/// the hand and sits: the walk out it came back from never resumes.
#[test]
fn an_agent_lifted_as_it_comes_back_from_an_exit_never_resumes_the_walk_out() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut hand = Hand::new(entry_slot(now - Duration::from_secs(120)), now);
    let end = hand.now;
    hand.slot().exiting_at = Some(end);
    for _ in 0..10 {
        hand.paint();
    }
    let back = hand.now;
    let slot = hand.slot();
    slot.exiting_at = None;
    slot.state_started_at = back;
    hand.lift();
    // Past the walk out's end, so a replay of it re-enters from the door.
    for _ in 0..frames_in(pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW) {
        hand.paint();
    }
    hand.drop_it();
    assert!(hand.paint_until_seated(900), "home, it sits");
    for _ in 0..300 {
        hand.paint();
    }
    assert!(
        hand.max_step <= MAX_FRAME_STEP_PX,
        "max frame jump {}px (> {MAX_FRAME_STEP_PX})",
        hand.max_step
    );
}

/// Set down by a pocket the walls close off, an agent lands on the floor its
/// legs reach beside it, not home, wherever such floor lies within reach.
#[test]
fn an_agent_set_down_by_a_pocket_lands_on_the_floor_beside_it() {
    let cell = crate::pathfind::CELL_SIZE;
    let home = Point {
        x: u16::MAX,
        y: u16::MAX,
    };
    let mut checked = 0u32;
    for (w, h) in [(55, 45), (80, 46), (120, 46)] {
        for seed in 0..8 {
            let Some(l) = SceneLayout::compute(crate::layout::Viewport::full(w, h), None, seed)
            else {
                continue;
            };
            let near = |at: Point| {
                (-2i16..=2).any(|dx| {
                    (-2i16..=2).any(|dy| {
                        let (Some(x), Some(y)) = (
                            at.x.checked_add_signed(dx * cell as i16),
                            at.y.checked_add_signed(dy * cell as i16),
                        ) else {
                            return false;
                        };
                        l.reachable.reaches(Point { x, y })
                    })
                })
            };
            for at in (0..w)
                .step_by(2)
                .flat_map(|x| (0..h).step_by(2).map(move |y| Point { x, y }))
                .filter(|&at| near(at))
            {
                let landed = landing(at, home, &l);
                assert!(
                    landed != home && l.reachable.reaches(landed),
                    "{w}x{h} seed {seed}: set down at {at:?} lands {landed:?}"
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked > 1_000,
        "the sweep must set agents down, saw {checked}"
    );
}

/// Lifted on its way in and set down at its desk, it sits: the walk in it
/// was lifted off never resumes.
#[test]
fn an_agent_lifted_on_its_way_in_never_resumes_it() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut hand = Hand::new(entry_slot(now), now);
    for _ in 0..10 {
        hand.paint();
    }
    hand.lift();
    let l = hand.layout();
    let desk = l.home_desks[0];
    hand.carry_to(desk_leg_endpoint(desk, &l).0, 8);
    hand.drop_it();
    assert!(hand.paint_until_seated(600), "home, it sits");
    for _ in 0..600 {
        hand.paint();
    }
    assert!(
        hand.max_step <= MAX_FRAME_STEP_PX,
        "max frame jump {}px (> {MAX_FRAME_STEP_PX})",
        hand.max_step
    );
}

/// A session that ends in hand walks the agent out of it: back before it is
/// through the door, it walks in from where it is, not into the hand, and the
/// next end walks it out again.
#[test]
fn an_agent_back_from_an_exit_in_hand_walks_in_from_where_it_is() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut hand = carried_off(now);
    let end = hand.now;
    hand.slot().exiting_at = Some(end);
    for _ in 0..15 {
        hand.paint();
    }
    let back = hand.now;
    let slot = hand.slot();
    slot.exiting_at = None;
    slot.state_started_at = back;
    for _ in 0..30 {
        hand.paint();
    }
    hand.drop_it();
    assert!(hand.paint_until_seated(900), "home, it sits");
    let end = hand.now;
    hand.slot().exiting_at = Some(end);
    assert!(hand.paint_out(600) > 20, "it walks out again");
    assert!(
        hand.max_step <= MAX_FRAME_STEP_PX,
        "max frame jump {}px (> {MAX_FRAME_STEP_PX})",
        hand.max_step
    );
}

#[test]
fn wander_continuous_across_layouts_and_agents() {
    use crate::layout::TEST_DEFAULT_DESKS;

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let geometries: [(u16, u16, u64); 5] = [
        (120, 96, 0),
        (120, 96, 7),
        (160, 100, 3),
        (96, 80, 11),
        (200, 120, 5),
    ];

    for (w, h, seed) in geometries {
        let Some(l) = SceneLayout::compute(
            crate::layout::Viewport::full(w, h),
            Some(TEST_DEFAULT_DESKS),
            seed,
        ) else {
            continue;
        };
        if l.home_desks.is_empty() || l.waypoints.is_empty() {
            continue;
        }
        let n = l.home_desks.len().min(4);
        for k in 0..n {
            let id = AgentId::from_transcript_path(&format!("/geo/{w}x{h}-{seed}/{k}.jsonl"));
            let old = now - Duration::from_secs(120);
            let mut slot = entry_slot(old);
            slot.agent_id = id;
            slot.home_desk = Some(FloorDesk::new(0, k));
            slot.last_event_at = old;
            // ~20 s ⇒ 2–3 full wander cycles per agent.
            let (max_step, _) = max_top_left_step(&slot, &l, now, 600, true);
            assert!(
                max_step <= MAX_FRAME_STEP_PX,
                "geometry {w}x{h} seed={seed} desk={k}: max frame jump {max_step}px (> {MAX_FRAME_STEP_PX})"
            );
        }
    }
}

/// Returns shape `a` until `flipped`, then `b` — switches the A* result mid-leg.
#[derive(Debug)]
struct FlipRouter {
    flipped: bool,
    a: Vec<Point>,
    b: Vec<Point>,
}
impl Router for FlipRouter {
    fn new_layout(&mut self, _: crate::layout::Bounds) {}
    fn route(
        &mut self,
        _: &WalkableMask,
        _: &crate::walkable::OccupancyOverlay,
        _from: Point,
        _to: Point,
    ) -> Vec<Point> {
        if self.flipped {
            self.b.clone()
        } else {
            self.a.clone()
        }
    }
}

#[test]
fn frozen_leg_anchor_continuous_across_router_shape_change() {
    // The freeze-specific guard: it fails when the freeze is reverted.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let door = l.door_threshold;
    let desk = l.home_desks[0];
    let desk_t = Point {
        x: desk.at.x + 6,
        y: desk.at.y + 4,
    };
    // Shape A is a long DOWN-then-across detour so the walk lasts many frames;
    // shape B is short and very differently routed. Both share the endpoints.
    let a = vec![
        door,
        Point {
            x: door.x,
            y: door.y + 40,
        },
        Point {
            x: desk_t.x,
            y: door.y + 40,
        },
        desk_t,
    ];
    let b = vec![
        door,
        Point {
            x: desk_t.x,
            y: door.y,
        },
        desk_t,
    ];

    // ~40% of the frozen entry duration — mid-walk, where A and B diverge most.
    let entry_id = entry_slot(now).agent_id;
    let dur = walk_profile(octile_path_len(&a).max(1), WalkIntent::Entry, entry_id).duration_ms;
    let flip_frame = frames_in(Duration::from_millis(dur * 2 / 5)).max(2);

    let mut rig = RouteRig::new(FlipRouter {
        flipped: false,
        a,
        b,
    });

    let mut prev: Option<Point> = None;
    let mut max_step = 0i32;
    for i in 0..(flip_frame + 8) {
        if i == flip_frame {
            rig.router.flipped = true;
        }
        let slot = entry_slot(now - Duration::from_millis(200));
        let t = now + PAINT_FRAME * i;
        if let Some(Pose::Walking { segment, .. }) =
            derive_with_routing(&slot, t, &l, &mut rig.rctx())
        {
            let pos = segment.at();
            if let Some(p) = prev {
                let step = (i32::from(pos.x) - i32::from(p.x))
                    .abs()
                    .max((i32::from(pos.y) - i32::from(p.y)).abs());
                max_step = max_step.max(step);
            }
            prev = Some(pos);
        }
    }
    assert!(
        max_step <= 20,
        "frozen leg must keep the anchor continuous despite a mid-leg router shape change (max jump {max_step}px)"
    );
}

#[test]
fn multiple_agents_share_overlay_without_teleport() {
    // NOTE: the real AStarRouter is stable enough that this scenario does not by
    // itself reproduce the freeze regression —
    // `frozen_leg_anchor_continuous_across_router_shape_change` is that guard.
    use crate::pathfind::AStarRouter;
    use crate::sim::anchors::character_top_left;

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let n = l.home_desks.len().min(3);
    let old = now - Duration::from_secs(120);
    let slots: Vec<AgentSlot> = (0..n)
        .map(|k| {
            let mut s = entry_slot(old);
            s.agent_id = AgentId::from_transcript_path(&format!("/multi/{k}.jsonl"));
            s.home_desk = Some(FloorDesk::new(0, k));
            s.last_event_at = old;
            s
        })
        .collect();

    let mut rig = RouteRig::new(AStarRouter::new());
    rig.router.new_layout(l.corridor);
    let mut prev: HashMap<AgentId, Point> = HashMap::new();
    let mut max_step = 0i32;

    for i in 0..700u32 {
        let t = now + PAINT_FRAME * i;
        // Rebuild the shared overlay as the pixel pass does — this is the churn
        // that re-routes the other walkers.
        rig.overlay.clear();
        for s in &slots {
            if let Some(Pose::AtWaypoint { wp, .. }) = derive(s, t, &l)
                && let Some(w) = l.waypoints.get(wp)
            {
                rig.overlay
                    .add(w.pos.x.saturating_sub(4), w.pos.y.saturating_sub(6), 8, 12);
            }
        }
        for s in &slots {
            if let Some(a) = character_top_left(s, &l, t, &mut rig.rctx()) {
                if let Some(p) = prev.get(&s.agent_id) {
                    let step = (i32::from(a.x) - i32::from(p.x))
                        .abs()
                        .max((i32::from(a.y) - i32::from(p.y)).abs());
                    max_step = max_step.max(step);
                }
                prev.insert(s.agent_id, a);
            }
        }
    }
    assert!(
        max_step <= MAX_FRAME_STEP_PX,
        "agents sharing a churning overlay must not teleport (max frame jump {max_step}px)"
    );
}

fn unit_slot(now: SystemTime) -> AgentSlot {
    running_slot(now, now - Duration::from_secs(60))
}

#[test]
fn route_walking_pose_straight_leg_is_drawn_at_the_lerp_and_clears_walk_path() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = unit_slot(now);
    let from = Point { x: 10, y: 20 };
    let to = Point { x: 30, y: 20 };

    let mut rig = RouteRig::new(StubRouter::straight());

    let (p, drawn_at) = route_walking_pose(
        &slot,
        &l,
        &mut rig.rctx(),
        LegPose {
            from,
            to,
            progress: LegProgress::of(1, 2),
            carrying_coffee: false,
        },
        Settle::None,
    );

    match p {
        Pose::Walking { segment, .. } => {
            assert_eq!(
                segment,
                Segment::straight(from, to, LegProgress::of(1, 2)),
                "straight leg keeps original endpoints and passes progress through"
            );
        }
        other => panic!("expected straight Walking, got {other:?}"),
    }
    assert!(
        rig.walks
            .get(&slot.agent_id)
            .is_some_and(|walk| walk.walk_path.is_none()),
        "straight 2-point walk must clear walk_path"
    );
    assert_eq!(
        drawn_at,
        Some(Segment::straight(from, to, LegProgress::of(1, 2)).at()),
        "straight leg is drawn at the lerped position"
    );
}

#[test]
fn snap_back_profile_length_measures_the_routed_polyline() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = running_slot(now, now - Duration::from_secs(60));
    let desk = l.home_desks[0];
    let (snap_target, chair_settle) = crate::pose::desk_leg_endpoint(desk, &l);
    let prev = Point {
        x: desk.at.x + 50,
        y: desk.at.y + 30,
    };
    let corner = Point {
        x: prev.x,
        y: prev.y + 40,
    };
    let detour = vec![prev, corner, snap_target];
    let mut rig = RouteRig::new(StubRouter::corners(detour.clone()));
    rig.history.set(slot.agent_id, Some(prev));

    let _ = derive_with_routing(&slot, now, &l, &mut rig.rctx());

    let leg = rig
        .walks
        .get(&slot.agent_id)
        .and_then(|walk| walk.snap_back.as_ref())
        .expect("snap-back must be armed");
    let settle = settle_len(snap_target, chair_settle);
    let routed = octile_path_len(&detour) + settle;
    let straight = octile_path_len(&[prev, snap_target]) + settle;
    assert!(
        routed > straight,
        "test setup: the detour must be longer than the straight line"
    );
    assert_eq!(
        leg.profile.path_len_octile,
        routed.max(1),
        "armed profile must cover the routed polyline (+ chair settle), not the straight line"
    );
}

/// A cornered walk's `travelled` is how far along the WHOLE polyline it is,
/// not along the segment it is on: the frame keeps stepping across a corner.
#[test]
fn route_walking_pose_measures_travelled_along_the_whole_route() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let l = layout();
    let slot = unit_slot(now);
    let a = Point { x: 10, y: 10 };
    let b = Point { x: 30, y: 10 };
    let c = Point { x: 30, y: 30 };
    let mut rig = RouteRig::new(StubRouter::corners(vec![a, b, c]));
    for t in (0..=4).map(|q| LegProgress::of(q, 4)) {
        let (out, _) = route_walking_pose(
            &slot,
            &l,
            &mut rig.rctx(),
            LegPose {
                from: a,
                to: c,
                progress: t,
                carrying_coffee: false,
            },
            Settle::None,
        );
        let Pose::Walking { segment, .. } = out else {
            panic!("a walk at {t:?}, got {out:?}");
        };
        assert_eq!(segment, Segment::on_route(&[a, b, c], t), "at {t:?}");
    }
}

#[test]
fn on_route_starts_and_ends_where_the_leg_does() {
    let a = Point { x: 10, y: 10 };
    let b = Point { x: 30, y: 10 };
    let c = Point { x: 30, y: 30 };
    let path = [a, b, c];

    let start = Segment::on_route(&path, LegProgress::ZERO);
    assert_eq!((start.at(), start.travelled()), (a, 0));
    let end = Segment::on_route(&path, LegProgress::DONE);
    assert_eq!(
        (end.from(), end.to(), end.at(), end.travelled()),
        (b, c, c, octile_path_len(&path)),
    );
}

#[test]
fn on_route_never_walks_back() {
    let path = [
        Point { x: 10, y: 10 },
        Point { x: 30, y: 10 },
        Point { x: 50, y: 30 },
        Point { x: 50, y: 60 },
    ];
    let mut last = (0, 0);
    for q in 0..=64 {
        let segment = Segment::on_route(&path, LegProgress::of(q, 64));
        let index = path
            .iter()
            .position(|&p| p == segment.from())
            .expect("a segment starts on a route point");
        assert!(
            segment.travelled() >= last.0 && index >= last.1,
            "step {q}: {segment:?} walked back from {last:?}"
        );
        last = (segment.travelled(), index);
    }
}

#[test]
fn a_resurrect_after_the_walkout_arrived_re_enters_through_the_door() {
    let l = layout();
    let door = l.door_threshold;
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    // Arrived long before the entry window, so only a re-arm can produce an entry.
    let created = t0 - Duration::from_secs(3600);
    let mut slot = entry_slot(created);
    slot.exiting_at = Some(t0);

    let mut rig = RouteRig::new(StubRouter::straight());

    // Let the leg arrive: the sprite is off-floor (`None`) while `history` still
    // holds the exit's first point, so only the arrival check sends it to the door.
    derive_with_routing(&slot, t0, &l, &mut rig.rctx());
    assert!(
        rig.walks[&slot.agent_id].exit.is_some(),
        "test setup: the walkout must have snapshotted a leg"
    );
    let arrived = t0 + pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    assert!(
        derive_with_routing(&slot, arrived, &l, &mut rig.rctx()).is_none(),
        "test setup: the walkout must have reached the door"
    );

    // exiting_at cleared, state_started_at re-stamped, arrived_at untouched —
    // exactly what `fsm::resurrect_in_place` leaves behind.
    slot.exiting_at = None;
    slot.state_started_at = arrived;

    let p = derive_with_routing(&slot, arrived, &l, &mut rig.rctx());
    match p {
        Some(Pose::Walking { segment, .. }) => assert_eq!(
            segment.from(),
            door,
            "a resurrect past its walkout must re-enter from the door"
        ),
        other => panic!("expected a fresh entry walk, got {other:?}"),
    }
    assert!(
        rig.walks[&slot.agent_id].exit.is_none(),
        "the spent exit leg must be cleared, or the NEXT exit replays an \
         already-arrived profile and the sprite vanishes instead of walking out"
    );
}

#[test]
fn a_resurrect_mid_walkout_re_enters_from_the_live_position() {
    let l = layout();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let created = t0 - Duration::from_secs(3600);
    let mut slot = entry_slot(created);
    slot.exiting_at = Some(t0);

    let mut rig = RouteRig::new(StubRouter::straight());

    derive_with_routing(&slot, t0, &l, &mut rig.rctx());
    let mid = t0 + Duration::from_millis(200);
    assert!(
        matches!(
            derive_with_routing(&slot, mid, &l, &mut rig.rctx()),
            Some(Pose::Walking { .. })
        ),
        "test setup: the walkout must still be in flight"
    );
    let live = rig
        .history
        .last_drawn(slot.agent_id)
        .expect("the walkout renders every frame, so history holds a position");

    slot.exiting_at = None;
    slot.state_started_at = mid;
    let pose = derive_with_routing(&slot, mid, &l, &mut rig.rctx());

    let leg = rig.walks[&slot.agent_id]
        .entry
        .as_ref()
        .expect("an in-flight resurrect must re-arm entry");
    assert_eq!(
        leg.from, live,
        "it re-enters from the sprite's real position, not the door"
    );
    assert_ne!(
        leg.from, l.door_threshold,
        "starting at the door would jump the walker backwards"
    );
    assert!(
        matches!(pose, Some(Pose::Walking { .. })),
        "and it RENDERS that walk — the entry branch runs before the wander \
         dispatch, which would otherwise return a seated pose and teleport it"
    );
}

/// A device's cast switched back to within its grace comes back as a local
/// resurrect leaves a slot: `exiting_at` cleared and its arrival untouched,
/// here the first line's `None`. So the routed pose, the one every frame renders,
/// turns it round where it walks rather than seating it.
#[test]
fn a_cast_switched_back_mid_walkout_turns_round_as_a_resurrect_does() {
    use pixtuoid_core::SceneState;
    use pixtuoid_core::remote::{Machine, Os, RemoteOffice, encode};
    use pixtuoid_core::state::MAX_FLOORS;
    fn received(office: &mut RemoteOffice, scene: &SceneState, from: &Machine, at: SystemTime) {
        let line = encode(scene, from, at).expect("a scene encodes");
        office.apply(line.as_bytes(), at).expect("it applies");
    }

    let l = layout();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let demo = Machine::new("demo", Os::Other);
    let walker = entry_slot(t0 - Duration::from_secs(3600));
    let id = walker.agent_id;
    let mut cast = SceneState::uniform(4);
    cast.agents.insert(id, walker);
    let mut office = RemoteOffice::new([4; MAX_FLOORS]);
    received(&mut office, &cast, &demo, t0);
    let switch = t0 + Duration::from_secs(1);
    let live = Machine::new("studio", Os::Macos);
    received(&mut office, &SceneState::uniform(4), &live, switch);

    let mut rig = RouteRig::new(StubRouter::straight());
    let leaving = office.scene().agents[&id].clone();
    derive_with_routing(&leaving, switch, &l, &mut rig.rctx());
    let mid = switch + Duration::from_millis(200);
    assert!(
        matches!(
            derive_with_routing(&leaving, mid, &l, &mut rig.rctx()),
            Some(Pose::Walking { .. })
        ),
        "premise: the walkout is in flight"
    );
    let walking_at = (rig.history)
        .last_drawn(id)
        .expect("the walkout renders every frame, so history holds a position");

    received(&mut office, &cast, &demo, mid);
    let back = &office.scene().agents[&id];
    assert_eq!(
        (back.exiting_at, back.arrived_at),
        (None, None),
        "premise: back as a resurrect leaves it, with no arrival"
    );
    let pose = derive_with_routing(back, mid, &l, &mut rig.rctx());
    let leg = (rig.walks[&id].entry.as_ref()).expect("the switch-back re-arms entry");
    assert_eq!(leg.from, walking_at, "it turns round where it walks");
    assert!(
        matches!(pose, Some(Pose::Walking { .. })),
        "and renders that walk, never its desk"
    );
}

/// What a walk's stores hold before its first frame.
type Seed = Box<dyn Fn(&mut RouteRig<StubRouter>)>;

/// One walk an agent takes, for `under_still_no_walk_is_drawn`.
struct AgentWalk {
    name: &'static str,
    /// The agent's slot at each instant.
    slot: Box<dyn Fn(SystemTime) -> AgentSlot>,
    before: Seed,
    /// The frames that count: from when its walk can start until it must be
    /// over.
    counted: std::ops::Range<SystemTime>,
}

/// Under Still no frame draws an agent mid-walk, whichever walk it takes:
/// each poses where its walk ends at once. Under Full each of them walks.
#[test]
fn under_still_no_walk_is_drawn() {
    use crate::anim::Motion;
    use crate::walk::Lifted;
    let l = layout();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let settled = t0 - Duration::from_secs(3_600);
    let desk = l.home_desks[0];
    let away = Point {
        x: desk.at.x + 50,
        y: desk.at.y + 30,
    };
    let entry = Duration::from_millis(ENTRY_ANIMATION_MS);
    let exit = pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let back = t0 + Duration::from_millis(200);
    let leaving = move || AgentSlot {
        exiting_at: Some(t0),
        ..entry_slot(settled)
    };
    let walks = [
        AgentWalk {
            name: "in",
            slot: Box::new(move |_| entry_slot(t0)),
            before: Box::new(|_| {}),
            counted: t0..t0 + entry,
        },
        AgentWalk {
            name: "out",
            slot: Box::new(move |_| leaving()),
            before: Box::new(|_| {}),
            counted: t0..t0 + exit,
        },
        AgentWalk {
            name: "back in",
            slot: Box::new(move |now| {
                if now < back {
                    leaving()
                } else {
                    AgentSlot {
                        state_started_at: back,
                        ..entry_slot(settled)
                    }
                }
            }),
            before: Box::new(|_| {}),
            counted: back..back + entry,
        },
        AgentWalk {
            name: "to its desk",
            slot: Box::new(move |_| running_slot(t0, settled)),
            before: Box::new(move |rig| {
                let id = running_slot(t0, settled).agent_id;
                rig.history.set(id, Some(away));
            }),
            counted: t0..t0 + entry,
        },
        AgentWalk {
            name: "home from a drop",
            slot: Box::new(move |_| entry_slot(settled)),
            before: Box::new(move |rig| {
                let id = entry_slot(settled).agent_id;
                rig.walks.entry(id).or_default().lifted = Some(Lifted::Dropped(away));
            }),
            counted: t0..t0 + entry,
        },
        AgentWalk {
            name: "a trip",
            slot: Box::new(move |_| tripper(t0)),
            before: Box::new(|_| {}),
            counted: t0..t0 + Duration::from_secs(50),
        },
    ];
    let walked = |motion: Motion, walk: &AgentWalk| {
        let mut rig = RouteRig::new(StubRouter::straight());
        rig.motion = motion;
        (walk.before)(&mut rig);
        (0..)
            .map(|i| t0 + PAINT_FRAME * i)
            .take_while(|now| *now < walk.counted.end)
            .any(|now| {
                let pose = derive_with_routing(&(walk.slot)(now), now, &l, &mut rig.rctx());
                walk.counted.contains(&now) && matches!(pose, Some(Pose::Walking { .. }))
            })
    };
    for walk in &walks {
        assert!(
            walked(Motion::Full, walk),
            "premise: {} walks under Full",
            walk.name
        );
        assert!(
            !walked(Motion::Still, walk),
            "{} walked under Still",
            walk.name
        );
    }
}

/// An agent arrived long before `t0` and idle from it, so its wander starts at
/// cycle 0, one that takes a trip.
fn tripper(t0: SystemTime) -> AgentSlot {
    let id = (0u64..1000)
        .map(|i| AgentId::from_transcript_path(&format!("/tripper/{i}.jsonl")))
        .find(|id| takes_trip(*id, 0))
        .expect("a trip agent");
    AgentSlot {
        agent_id: id,
        state_started_at: t0,
        ..entry_slot(t0 - Duration::from_secs(120))
    }
}

/// Whether `rig` holds `id` on its trip's way out.
fn tripping(rig: &RouteRig<StubRouter>, id: AgentId) -> bool {
    matches!(
        rig.walks[&id].wander.phase,
        crate::walk::WanderPhase::WalkingOut(_)
    )
}

/// A walk back made instant leaves the agent drawn at its chair, so an exit
/// under Full right after it starts at the desk, not at the waypoint the agent
/// last stood at.
#[test]
fn an_exit_after_an_instant_walk_back_starts_at_the_desk() {
    use crate::anim::Motion;
    let l = layout();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let slot = tripper(t0);
    let id = slot.agent_id;
    let at = |i: u32| t0 + PAINT_FRAME * i;
    let mut rig = RouteRig::new(StubRouter::straight());
    let out = (0..3_000)
        .find(|&i| {
            derive_with_routing(&slot, at(i), &l, &mut rig.rctx());
            tripping(&rig, id)
        })
        .expect("premise: it trips under Full");
    rig.motion = Motion::Still;
    let home = (out + 1..out + 3_000)
        .find(|&i| {
            derive_with_routing(&slot, at(i), &l, &mut rig.rctx());
            matches!(
                rig.walks[&id].wander.phase,
                crate::walk::WanderPhase::Seated
            )
        })
        .expect("it comes home under Still");
    rig.motion = Motion::Full;
    let leave = at(home + 1);
    let leaving = AgentSlot {
        exiting_at: Some(leave),
        ..slot.clone()
    };
    derive_with_routing(&leaving, leave, &l, &mut rig.rctx());
    let desk = l.home_desks[0];
    let exit = rig.walks[&id].exit.as_ref().expect("an exit leg");
    assert_eq!(
        exit.from,
        desk_walk_anchor_facing(desk.at, desk.facing, l.inner),
        "it leaves from its desk"
    );
}

/// A wander under way when Still comes on finishes on its frozen leg, then
/// comes home at once: the walk back, set off under Still, takes no time.
#[test]
fn a_wander_under_way_when_still_comes_on_finishes_then_lands_home() {
    use crate::anim::Motion;
    let l = layout();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let slot = tripper(t0);
    let at = |i: u32| t0 + PAINT_FRAME * i;
    let walking = |pose: &Option<Pose>| matches!(pose, Some(Pose::Walking { .. }));
    let mut rig = RouteRig::new(StubRouter::straight());
    let id = slot.agent_id;
    let out = (0..3_000)
        .find(|&i| {
            derive_with_routing(&slot, at(i), &l, &mut rig.rctx());
            tripping(&rig, id)
        })
        .expect("premise: it trips under Full");
    rig.motion = Motion::Still;
    let poses: Vec<_> = (out + 1..out + 3_000)
        .map(|i| derive_with_routing(&slot, at(i), &l, &mut rig.rctx()))
        .collect();
    assert!(walking(&poses[0]), "the walk under way goes on");
    let ended = poses.iter().position(|p| !walking(p)).expect("it arrives");
    assert!(
        matches!(
            poses[ended],
            Some(Pose::AtWaypoint { .. } | Pose::AimlessAt { .. })
        ),
        "at its trip's end: {:?}",
        poses[ended]
    );
    assert!(!poses[ended..].iter().any(walking), "it walks no more");
    assert_eq!(poses.last(), Some(&Some(Pose::SeatedIdle)), "it is home");
}

/// A walk back is timed as it sets off, not when its route was frozen at the
/// waypoint: a wander that reached its waypoint under Still walks home if
/// Full is back by the dwell's end.
#[test]
fn a_walk_back_is_timed_as_it_sets_off() {
    use crate::anim::Motion;
    let l = layout();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let slot = tripper(t0);
    let at = |i: u32| t0 + PAINT_FRAME * i;
    let walking = |pose: &Option<Pose>| matches!(pose, Some(Pose::Walking { .. }));
    let mut rig = RouteRig::new(StubRouter::straight());
    let id = slot.agent_id;
    let out = (0..3_000)
        .find(|&i| {
            derive_with_routing(&slot, at(i), &l, &mut rig.rctx());
            tripping(&rig, id)
        })
        .expect("premise: it trips under Full");
    rig.motion = Motion::Still;
    let there = (out + 1..out + 3_000)
        .find(|&i| {
            let pose = derive_with_routing(&slot, at(i), &l, &mut rig.rctx());
            matches!(pose, Some(Pose::AtWaypoint { .. } | Pose::AimlessAt { .. }))
        })
        .expect("it reaches its trip's end under Still");
    rig.motion = Motion::Full;
    let left = (there + 1..there + 3_000)
        .map(|i| derive_with_routing(&slot, at(i), &l, &mut rig.rctx()))
        .find(|pose| !matches!(pose, Some(Pose::AtWaypoint { .. } | Pose::AimlessAt { .. })));
    assert!(
        left.as_ref().is_some_and(walking),
        "it walks home under Full: {left:?}"
    );
}

/// A stretch of Still leaves the wander's clock current, so Full coming back
/// replays nothing it missed: a phase changes once per due event, never frame
/// on frame. The clock keeps time whether Still finds the agent at a
/// waypoint, for less than a stale resume, or seated through its dwells.
#[test]
fn full_after_a_stretch_of_still_replays_no_overdue_wander() {
    use crate::anim::Motion;
    let l = layout();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let slot = tripper(t0);
    let id = slot.agent_id;
    let at = |i: u32| t0 + PAINT_FRAME * i;
    let frames_in_ms = |ms: u64| frames_in(Duration::from_millis(ms));
    let phase = |rig: &RouteRig<StubRouter>| std::mem::discriminant(&rig.walks[&id].wander.phase);
    let mut full = RouteRig::new(StubRouter::straight());
    let back = (0..6_000)
        .find(|&i| {
            derive_with_routing(&slot, at(i), &l, &mut full.rctx());
            matches!(
                full.walks[&id].wander.phase,
                crate::walk::WanderPhase::WalkingBack(_)
            )
        })
        .expect("premise: it trips under Full and walks back");
    let at_waypoint = back - frames_in_ms(500)..back + frames_in_ms(6_000);
    assert!(
        PAINT_FRAME * (at_waypoint.end - at_waypoint.start)
            < Duration::from_millis(STALE_RESUME_GAP_BASE_MS),
        "premise: shorter than any stale resume"
    );
    let seated = 0..frames_in_ms(10 * seated_dwell_ms(id));

    for still in [at_waypoint, seated] {
        let mut rig = RouteRig::new(StubRouter::straight());
        let mut changes = 0;
        let mut last = None;
        for i in 0..still.end + frames_in_ms(1_000) {
            rig.motion = if still.contains(&i) {
                Motion::Still
            } else {
                Motion::Full
            };
            derive_with_routing(&slot, at(i), &l, &mut rig.rctx());
            let now = phase(&rig);
            if i >= still.end && last.is_some_and(|was| was != now) {
                changes += 1;
            }
            last = Some(now);
        }
        assert!(
            changes <= 1,
            "{still:?}: {changes} phase changes in the second after"
        );
    }
}

/// The exit render and the resurrect check must read "has the walkout finished?"
/// through the SAME time-compressed clock. Every other resurrect test routes
/// straight-line on the small layout, where the compression branch is dead — so
/// only this fixture can catch a raw-elapsed read at the resurrect site.
#[test]
fn the_resurrect_check_reads_the_same_compressed_clock_as_the_exit_render() {
    let l = layout();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut slot = entry_slot(t0 - Duration::from_secs(3600));
    slot.exiting_at = Some(t0);

    // A long cornered route: physics duration lands well past the compression
    // budget, so raw and compressed elapsed genuinely disagree.
    let far: Vec<Point> = (0..24)
        .map(|i| Point {
            x: if i % 2 == 0 { 8 } else { 110 },
            y: 20 + i * 3,
        })
        .collect();
    let mut rig = RouteRig::new(StubRouter::corners(far));

    derive_with_routing(&slot, t0, &l, &mut rig.rctx());
    let profile = rig.walks[&slot.agent_id]
        .exit
        .as_ref()
        .expect("exit leg snapshotted")
        .profile;
    let budget = (pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW.as_millis() as u64)
        .saturating_sub(EXIT_BUDGET_MARGIN_MS);
    assert!(
        profile.duration_ms + profile.pause_ms > budget,
        "fixture must exceed the compression budget, else this pins nothing \
         (duration {} + pause {} vs budget {budget})",
        profile.duration_ms,
        profile.pause_ms
    );

    // The first instant the COMPRESSED clock reports arrival. Compression scales
    // elapsed by duration/budget, so arrival lands past the budget edge — find it
    // rather than assume it.
    let arrived_at = (1..20_000u64)
        .find(|e| walk_arrived(&profile, exit_elapsed_ms(&profile, *e)))
        .expect("compressed clock must arrive within the search range");
    assert!(
        !walk_arrived(&profile, arrived_at),
        "raw elapsed must still read in-flight at {arrived_at}ms — otherwise the \
         two clocks agree and this fixture cannot tell them apart"
    );
    let at = t0 + Duration::from_millis(arrived_at);

    assert!(
        rig.history.last_drawn(slot.agent_id).is_some(),
        "setup: history holds where the walkout was last drawn, so a raw clock \
         would re-enter from there instead of the door"
    );
    slot.exiting_at = None;
    slot.state_started_at = at;
    derive_with_routing(&slot, at, &l, &mut rig.rctx());
    let leg = rig.walks[&slot.agent_id]
        .entry
        .as_ref()
        .expect("a resurrect must re-arm entry");
    assert_eq!(
        leg.started_at, at,
        "the re-armed leg runs on a fresh clock, not the un-restamped birth"
    );
    assert_eq!(
        leg.from, l.door_threshold,
        "past the COMPRESSED arrival the sprite is gone — the door is the only \
         honest origin, and raw elapsed would re-enter from where the walkout was last drawn"
    );
}
