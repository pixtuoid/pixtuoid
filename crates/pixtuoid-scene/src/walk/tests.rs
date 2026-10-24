use super::*;
use crate::character::test_support::make_slot;
use crate::pose::{RouteRig, is_aimless_cycle, waypoint_index_for_cycle};
use pixtuoid_core::AgentId;

#[test]
fn walk_state_default_fields() {
    let walk = WalkState::default();
    assert!(walk.entry.is_none());
    assert!(walk.exit.is_none());
    assert!(walk.snap_back.is_none());
    assert_eq!(walk.wander.cycle_n, 0);
    assert_eq!(walk.wander.phase, WanderPhase::Seated);
    assert_eq!(walk.wander.phase_started_at, SystemTime::UNIX_EPOCH);
    assert_eq!(walk.wander.last_advanced_at, SystemTime::UNIX_EPOCH);
    assert!(matches!(walk.wander.phase, WanderPhase::Seated));
    assert!(matches!(walk.wander.target.kind, WanderKind::Aimless));
    assert!(walk.walk_path.is_none());
}

#[test]
fn path_len_empty_is_zero() {
    assert_eq!(octile_path_len(&[]), 0);
}

#[test]
fn path_len_single_point_is_zero() {
    let p = Point { x: 10, y: 20 };
    assert_eq!(octile_path_len(&[p]), 0);
}

#[test]
fn path_len_diagonal_segment() {
    // 3 px diagonal: octile = 14*3 = 42
    let a = Point { x: 0, y: 0 };
    let b = Point { x: 3, y: 3 };
    assert_eq!(octile_path_len(&[a, b]), 42);
}

#[test]
fn path_len_multi_segment_sums() {
    // right 4 (40) + down 3 (30) = 70
    let a = Point { x: 0, y: 0 };
    let b = Point { x: 4, y: 0 };
    let c = Point { x: 4, y: 3 };
    assert_eq!(octile_path_len(&[a, b, c]), 70);
}

use crate::layout::SceneLayout;
use crate::pathfind::Router;
use crate::pose::{
    WANDER_DWELL_EST_MS, dwell_ms, est_wander_cycle_ms, seated_dwell_ms, stale_resume_gap_ms,
    takes_trip,
};
use pixtuoid_core::state::ActivityState;
use pixtuoid_core::walkable::{OccupancyOverlay, WalkableMask};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug)]
struct Straight;
impl Router for Straight {
    fn route(
        &mut self,
        _: &WalkableMask,
        _: &OccupancyOverlay,
        from: Point,
        to: Point,
    ) -> Vec<Point> {
        vec![from, to]
    }
}

/// Captures every `(from, to)` route request — pins WHICH goal the profile
/// snapshots route to.
#[derive(Debug)]
struct Recording {
    calls: Vec<(Point, Point)>,
}
impl Router for Recording {
    fn route(
        &mut self,
        _: &WalkableMask,
        _: &OccupancyOverlay,
        from: Point,
        to: Point,
    ) -> Vec<Point> {
        self.calls.push((from, to));
        vec![from, to]
    }
}

/// Synthesises a horizontal path of the requested octile length starting at
/// `from`, IGNORING `to` — gives phase-transition tests a predictable walk.
#[derive(Debug)]
struct FixedLen {
    octile_len: u32,
}
impl Router for FixedLen {
    fn route(
        &mut self,
        _: &WalkableMask,
        _: &OccupancyOverlay,
        from: Point,
        _to: Point,
    ) -> Vec<Point> {
        let steps = (self.octile_len / crate::physics::OCTILE_STRAIGHT_COST) as u16;
        let mid = Point {
            x: from.x + steps / 2,
            y: from.y,
        };
        let end = Point {
            x: from.x + steps,
            y: from.y,
        };
        vec![from, mid, end]
    }
}

fn t0() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn idle_slot(path: &str, state_started: SystemTime) -> AgentSlot {
    AgentSlot {
        cwd: Arc::from(PathBuf::from("/p").as_path()),
        label: "cc".into(),
        state_started_at: state_started,
        created_at: state_started
            .checked_sub(Duration::from_secs(90))
            .unwrap_or(state_started),
        last_event_at: state_started
            .checked_sub(Duration::from_secs(90))
            .unwrap_or(state_started),
        ..make_slot(AgentId::from_transcript_path(path), ActivityState::Idle)
    }
}

fn layout() -> SceneLayout {
    SceneLayout::compute(120, 96, Some(4)).expect("fits")
}

fn trip_agent(prefix: &str) -> AgentId {
    (0u64..500)
        .map(|i| AgentId::from_transcript_path(&format!("/p/{prefix}_{i}.jsonl")))
        .find(|id| takes_trip(*id, 0))
        .expect("should find a trip agent quickly")
}

/// The variant KIND of a `WanderPhase`. The walk variants carry a non-`Eq`
/// `WalkProfile`, so this is the phase's identity for "has the agent left this
/// phase?" — the transition tests never key on which profile got frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhaseKind {
    Seated,
    WalkingOut,
    AtWaypoint,
    WalkingBack,
}

fn phase_kind(phase: WanderPhase) -> PhaseKind {
    match phase {
        WanderPhase::Seated => PhaseKind::Seated,
        WanderPhase::WalkingOut(_) => PhaseKind::WalkingOut,
        WanderPhase::AtWaypoint(_) => PhaseKind::AtWaypoint,
        WanderPhase::WalkingBack(_) => PhaseKind::WalkingBack,
    }
}

/// The wander tests' poll step, well under [`stale_resume_gap_ms`] so a poll
/// reads as per-frame rendering, never an off-screen gap.
const POLL_STEP_MS: u64 = 1_000;

/// One agent's wander machine over the standard layout: the slot plus every
/// store `advance_wander` threads, so a test drives it by `now` alone.
struct WanderRig<R: Router> {
    slot: AgentSlot,
    layout: SceneLayout,
    route: RouteRig<R>,
}

impl<R: Router> WanderRig<R> {
    fn new(slot: AgentSlot, router: R) -> Self {
        Self {
            slot,
            layout: layout(),
            route: RouteRig::new(router),
        }
    }

    fn advance(&mut self, now: SystemTime) {
        let desk = self
            .layout
            .home_desk(self.slot.desk_index.single_floor_local())
            .expect("the slot's desk exists in the standard layout");
        advance_wander(
            &self.slot,
            now,
            &self.layout,
            desk,
            &mut self.route.router,
            &self.route.overlay,
            &mut self.route.walks,
        );
    }

    fn state(&self) -> &WalkState {
        self.route
            .walks
            .get(&self.slot.agent_id)
            .expect("state inserted")
    }

    /// The dwell the machine will apply at the agent's current wander
    /// destination. Only valid once a destination is picked (WalkingOut onward).
    fn current_dwell_dur(&self) -> u64 {
        match self.state().wander.target.kind {
            WanderKind::Named { kind, .. } => dwell_ms(kind, self.slot.agent_id),
            WanderKind::Aimless => WANDER_DWELL_EST_MS,
        }
    }

    /// Poll [`WanderRig::advance`] until the agent's phase KIND is no longer
    /// `from_phase`, returning the new `now`.
    fn advance_until_leaves(
        &mut self,
        mut now: SystemTime,
        from_phase: PhaseKind,
        timeout_ms: u64,
    ) -> SystemTime {
        let start = now;
        while self
            .route
            .walks
            .get(&self.slot.agent_id)
            .map(|walk| phase_kind(walk.wander.phase))
            == Some(from_phase)
        {
            let elapsed = now
                .duration_since(start)
                .unwrap_or(Duration::ZERO)
                .as_millis() as u64;
            assert!(
                elapsed <= timeout_ms,
                "phase {from_phase:?} did not transition within {timeout_ms}ms"
            );
            now += Duration::from_millis(POLL_STEP_MS);
            self.advance(now);
        }
        now
    }
}

#[test]
fn fresh_idle_inits_to_seated_phase() {
    let now = t0();
    let slot = idle_slot("/p/a.jsonl", now);
    let mut rig = WanderRig::new(slot, Straight);

    rig.advance(now);

    let walk = rig.state();
    assert!(
        matches!(walk.wander.phase, WanderPhase::Seated),
        "fresh idle should init to Seated, got {:?}",
        walk.wander.phase
    );
    assert_eq!(walk.wander.cycle_n, 0);
}

#[test]
fn seated_transitions_to_walking_out_on_trip_cycle() {
    let trip_id = trip_agent("trip");
    let now = t0();
    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", now)
    };

    let mut rig = WanderRig::new(slot, Straight);

    rig.advance(now);
    rig.advance_until_leaves(now, PhaseKind::Seated, 60_000);

    let walk = rig.state();
    assert!(
        matches!(walk.wander.phase, WanderPhase::WalkingOut(_)),
        "after seated dwell on trip cycle, expected WalkingOut, got {:?}",
        walk.wander.phase
    );
}

#[test]
fn non_trip_cycle_stays_seated() {
    let stay_id = (0u64..500)
        .map(|i| AgentId::from_transcript_path(&format!("/p/stay_{i}.jsonl")))
        .find(|id| !takes_trip(*id, 0))
        .expect("should find a stay-seated agent");

    let now = t0();
    let slot = AgentSlot {
        agent_id: stay_id,
        ..idle_slot("/dummy", now)
    };

    let mut rig = WanderRig::new(slot, Straight);

    rig.advance(now);
    let past_dwell = now + Duration::from_millis(seated_dwell_ms(stay_id) + POLL_STEP_MS);
    let mut t = now;
    while t < past_dwell {
        t += Duration::from_millis(POLL_STEP_MS);
        rig.advance(t);
        assert!(
            matches!(rig.state().wander.phase, WanderPhase::Seated),
            "non-trip cycle must stay Seated"
        );
    }
}

#[test]
fn walking_out_transitions_to_at_waypoint_on_arrival() {
    let trip_id = trip_agent("wp");
    let now = t0();
    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", now)
    };

    let short_len: u32 = 200;
    let mut rig = WanderRig::new(
        slot,
        FixedLen {
            octile_len: short_len,
        },
    );

    rig.advance(now);
    let t1 = rig.advance_until_leaves(now, PhaseKind::Seated, 60_000);
    assert!(matches!(
        rig.state().wander.phase,
        WanderPhase::WalkingOut(_)
    ));
    rig.advance_until_leaves(t1, PhaseKind::WalkingOut, 20_000);

    let walk = rig.state();
    assert!(
        matches!(walk.wander.phase, WanderPhase::AtWaypoint(_)),
        "expected AtWaypoint after walk-out arrival, got {:?}",
        walk.wander.phase
    );
}

#[test]
fn at_waypoint_transitions_to_walking_back_after_dwell() {
    let trip_id = trip_agent("dwell");
    let now = t0();
    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", now)
    };

    let short_len: u32 = 200;
    let mut rig = WanderRig::new(
        slot,
        FixedLen {
            octile_len: short_len,
        },
    );

    rig.advance(now);
    let t1 = rig.advance_until_leaves(now, PhaseKind::Seated, 60_000);
    let t2 = rig.advance_until_leaves(t1, PhaseKind::WalkingOut, 20_000);
    assert!(matches!(
        rig.state().wander.phase,
        WanderPhase::AtWaypoint(_)
    ));
    rig.advance_until_leaves(t2, PhaseKind::AtWaypoint, 60_000);

    let walk = rig.state();
    assert!(
        matches!(walk.wander.phase, WanderPhase::WalkingBack(_)),
        "expected WalkingBack after dwell, got {:?}",
        walk.wander.phase
    );
}

#[test]
fn walking_back_arrival_increments_cycle_n_and_resets_to_seated() {
    let trip_id = trip_agent("cyc");
    let now = t0();
    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", now)
    };

    let short_len: u32 = 200;
    let mut rig = WanderRig::new(
        slot,
        FixedLen {
            octile_len: short_len,
        },
    );

    rig.advance(now);
    let t = rig.advance_until_leaves(now, PhaseKind::Seated, 60_000);
    let t = rig.advance_until_leaves(t, PhaseKind::WalkingOut, 20_000);
    let t = rig.advance_until_leaves(t, PhaseKind::AtWaypoint, 60_000);
    rig.advance_until_leaves(t, PhaseKind::WalkingBack, 20_000);

    let walk = rig.state();
    assert!(
        matches!(walk.wander.phase, WanderPhase::Seated),
        "completed cycle must reset to Seated, got {:?}",
        walk.wander.phase
    );
    assert_eq!(walk.wander.cycle_n, 1, "cycle_n must increment once");
}

#[test]
fn dwell_time_independent_of_path_length() {
    let trip_id = trip_agent("dwell2");
    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", t0())
    };
    let mut measured: Vec<u64> = Vec::new();
    for short_len in [150u32, 800u32] {
        let now = t0();
        let mut rig = WanderRig::new(
            slot.clone(),
            FixedLen {
                octile_len: short_len,
            },
        );

        rig.advance(now);
        let t1 = rig.advance_until_leaves(now, PhaseKind::Seated, 60_000);
        let at_wp_enter = rig.advance_until_leaves(t1, PhaseKind::WalkingOut, 20_000);
        let walk_back_enter = rig.advance_until_leaves(at_wp_enter, PhaseKind::AtWaypoint, 60_000);
        let dwell = walk_back_enter
            .duration_since(at_wp_enter)
            .unwrap()
            .as_millis() as u64;
        measured.push(dwell);
    }

    // The slack is one poll step.
    let diff = measured[0].abs_diff(measured[1]);
    assert!(
        diff <= POLL_STEP_MS,
        "dwell must be path-length-independent: {measured:?}"
    );
}

#[test]
fn settle_collapses_a_seat_pair_and_gives_it_back() {
    let p = Point { x: 1, y: 1 };
    let q = Point { x: 2, y: 2 };
    let cases = [
        (None, None, Settle::None),
        (Some(p), None, Settle::Start(p)),
        (None, Some(q), Settle::End(q)),
        (Some(p), Some(q), Settle::Both { start: p, end: q }),
    ];
    for (start, end, want) in cases {
        let got = Settle::from_pair(start, end);
        assert_eq!(got, want, "({start:?}, {end:?})");
        assert_eq!((got.start(), got.end()), (start, end), "{got:?}");
    }
}

#[test]
fn snapshot_leg_profile_measures_the_routed_leg_plus_settles() {
    use crate::physics::{WalkIntent, walk_profile};

    let mask = WalkableMask::new_open(64, 64);
    let overlay = OccupancyOverlay::new();
    let id = AgentId::from_parts("test", "leg-profile");
    let from = Point { x: 4, y: 8 };
    let to = Point { x: 40, y: 8 };
    let seat = Point { x: 42, y: 10 };

    // Pins that measurement runs on the ROUTED polyline plus the settles, not
    // on the raw `from`→`to` line.
    let got = snapshot_leg_profile(
        &mut Straight,
        &mask,
        &overlay,
        id,
        LegPlan {
            from,
            to,
            settle: Settle::End(seat),
            intent: WalkIntent::WanderOut,
        },
    );
    let path = route_jittered(&mut Straight, &mask, &overlay, id, from, to);
    let expect = walk_profile(
        measured_leg_len(&path, Settle::End(seat)),
        WalkIntent::WanderOut,
        id,
    );
    assert_eq!(got.path_len_octile, expect.path_len_octile);
    assert_eq!(got.duration_ms, expect.duration_ms);

    let no_settle = snapshot_leg_profile(
        &mut Straight,
        &mask,
        &overlay,
        id,
        LegPlan {
            from,
            to,
            settle: Settle::None,
            intent: WalkIntent::WanderOut,
        },
    );
    assert!(
        got.path_len_octile > no_settle.path_len_octile,
        "an end settle must add octile length to the measured leg"
    );
}

#[test]
fn arrival_pause_holds_walking_out_phase() {
    use crate::physics::{WalkIntent, walk_arrived, walk_profile};

    let trip_id = trip_agent("pause");
    let now = t0();
    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", now)
    };

    let short_len: u32 = 200;
    let profile = walk_profile(short_len, WalkIntent::WanderOut, trip_id);
    let mid_pause_elapsed = profile.duration_ms + profile.pause_ms / 2;
    assert!(
        !walk_arrived(&profile, mid_pause_elapsed),
        "walk_arrived must be false mid-pause"
    );

    let mut rig = WanderRig::new(
        slot,
        FixedLen {
            octile_len: short_len,
        },
    );

    rig.advance(now);
    let t1 = rig.advance_until_leaves(now, PhaseKind::Seated, 60_000);
    let out_started = rig.state().wander.phase_started_at;
    let actual_profile = match rig.state().wander.phase {
        WanderPhase::WalkingOut(p) => p,
        other => panic!("expected WalkingOut, got {other:?}"),
    };
    let actual_mid_elapsed = actual_profile.duration_ms + actual_profile.pause_ms / 2;

    // This sample is within ~1 s of t1, far below the stale trigger.
    let _ = t1;
    let mid = out_started + Duration::from_millis(actual_mid_elapsed);
    rig.advance(mid);
    assert!(
        matches!(rig.state().wander.phase, WanderPhase::WalkingOut(_)),
        "must stay WalkingOut during arrival pause"
    );
}

#[test]
fn idempotent_same_now_does_not_mutate_state() {
    let trip_id = trip_agent("idem");
    let now = t0();
    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", now)
    };

    let mut rig = WanderRig::new(slot, Straight);

    rig.advance(now);
    let t1 = rig.advance_until_leaves(now, PhaseKind::Seated, 60_000);

    let (phase_before, cycle_before) = {
        let walk = rig.state();
        (walk.wander.phase, walk.wander.cycle_n)
    };

    rig.advance(t1);

    let walk = rig.state();
    assert_eq!(
        walk.wander.phase, phase_before,
        "2nd call with same now must not change phase"
    );
    assert_eq!(
        walk.wander.cycle_n, cycle_before,
        "2nd call with same now must not change cycle_n"
    );
}

#[test]
fn bootstrap_fast_forwards_cycle_n() {
    let id = AgentId::from_transcript_path("/p/bootstrap.jsonl");
    let now = t0();
    let cycle = est_wander_cycle_ms(id);
    let state_started = now
        .checked_sub(Duration::from_millis(10 * cycle))
        .expect("time arithmetic ok");
    let slot = idle_slot("/p/bootstrap.jsonl", state_started);

    let mut rig = WanderRig::new(slot, Straight);

    rig.advance(now);

    let walk = rig.state();
    assert_eq!(
        walk.wander.cycle_n, 10,
        "bootstrap: elapsed = 10*est_cycle => cycle_n must equal exactly 10"
    );
}

#[test]
fn stale_resume_resyncs_without_replay() {
    let trip_id = trip_agent("stale");
    let now = t0();
    let est_cycle = est_wander_cycle_ms(trip_id);

    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", now)
    };

    let mut rig = WanderRig::new(slot, Straight);

    rig.advance(now);
    let t1 = rig.advance_until_leaves(now, PhaseKind::Seated, 60_000);
    assert!(
        matches!(rig.state().wander.phase, WanderPhase::WalkingOut(_)),
        "precondition: agent should be WalkingOut before the gap"
    );

    // Floor goes off-screen for ~20 cycles: advance_wander is NOT called, and a
    // SINGLE call on return must resync.
    assert!(20 * est_cycle > stale_resume_gap_ms(trip_id));
    let resume = t1 + Duration::from_millis(20 * est_cycle);
    rig.advance(resume);

    let walk = rig.state();
    assert!(
        matches!(walk.wander.phase, WanderPhase::Seated),
        "stale resume must resync to Seated (no per-frame replay), got {:?}",
        walk.wander.phase
    );
    assert!(
        walk.wander.cycle_n >= 18,
        "stale resume must fast-forward cycle_n across the gap, got {}",
        walk.wander.cycle_n
    );
}

#[test]
fn long_dwell_never_trips_stale_resume_on_screen() {
    let trip_id = trip_agent("longdwell");
    let now = t0();
    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", now)
    };

    let short_len: u32 = 200;
    let mut rig = WanderRig::new(
        slot,
        FixedLen {
            octile_len: short_len,
        },
    );

    rig.advance(now);
    let t1 = rig.advance_until_leaves(now, PhaseKind::Seated, 60_000);
    let t2 = rig.advance_until_leaves(t1, PhaseKind::WalkingOut, 20_000);
    assert!(matches!(
        rig.state().wander.phase,
        WanderPhase::AtWaypoint(_)
    ));

    // Base the window on the ACTUAL AtWaypoint phase start — the poll-observed
    // `t2` can lag the real transition by up to one poll step — and leave a 2 s
    // margin so the loop stops before the dwell genuinely ends.
    let at_wp_start = rig.state().wander.phase_started_at;
    let dwell_dur = rig.current_dwell_dur();
    let mut t = t2;
    let end = at_wp_start + Duration::from_millis(dwell_dur.saturating_sub(2_000));
    while t < end {
        t += Duration::from_millis(crate::anim::PAINT_FRAME_MS);
        rig.advance(t);
        assert!(
            !matches!(rig.state().wander.phase, WanderPhase::Seated),
            "long on-screen dwell wrongly tripped stale-resume (snapped to Seated mid-dwell)"
        );
    }
    assert!(
        matches!(rig.state().wander.phase, WanderPhase::AtWaypoint(_)),
        "agent should still be AtWaypoint just before the dwell ends"
    );
}

use crate::pose::jitter_dest;

/// A trip agent whose [`crate::pose::JITTER_MAX_PX`] goal jitter is nonzero, so the lockstep
/// assertions below have teeth.
fn jittering_trip_agent(prefix: &str) -> AgentId {
    let probe = Point { x: 50, y: 50 };
    (0u64..2000)
        .map(|i| AgentId::from_transcript_path(&format!("/p/{prefix}_{i}.jsonl")))
        .find(|id| takes_trip(*id, 0) && jitter_dest(*id, probe) != probe)
        .expect("should find a jittering trip agent quickly")
}

#[test]
fn wander_out_profile_routes_to_the_jittered_goal_the_render_uses() {
    let trip_id = jittering_trip_agent("jout");
    let now = t0();
    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", now)
    };
    let mut rig = WanderRig::new(slot, Recording { calls: Vec::new() });

    rig.advance(now);
    rig.advance_until_leaves(now, PhaseKind::Seated, 60_000);

    let walk = rig.state();
    assert!(matches!(walk.wander.phase, WanderPhase::WalkingOut(_)));
    let (_, routed_to) = *rig
        .route
        .router
        .calls
        .last()
        .expect("the trip snapshot routed");
    assert_eq!(
        routed_to,
        jitter_dest(trip_id, walk.wander.target.dest),
        "the WalkingOut profile must route to the render's jittered goal"
    );
}

#[test]
fn back_profile_routes_to_the_jittered_desk_goal_the_render_uses() {
    let trip_id = jittering_trip_agent("jback");
    let now = t0();
    let slot = AgentSlot {
        agent_id: trip_id,
        ..idle_slot("/dummy", now)
    };
    let l = layout();
    let overlay = OccupancyOverlay::new();
    let mut router = Recording { calls: Vec::new() };
    let mut walk = WalkState::default();
    walk.wander.target.dest = Point { x: 40, y: 60 };

    let _ = snapshot_back_profile(&slot, &walk, &l, l.home_desks[0], &mut router, &overlay);

    let (snap_to, _) = desk_leg_endpoint(l.home_desks[0], &l);
    assert_eq!(
        router.calls,
        vec![(walk.wander.target.dest, jitter_dest(trip_id, snap_to))],
        "the walk-back profile must route to the render's jittered desk goal"
    );
}

#[test]
fn wander_dest_for_pantry_is_the_home_desk_stand_point() {
    let l = layout();
    let pantry_idx = l
        .waypoints
        .iter()
        .position(|w| w.kind == WaypointKind::Pantry)
        .expect("standard floor has a pantry");
    let (path, _id) = (0u64..8000)
        .find_map(|i| {
            let p = format!("/p/mirror_{i}.jsonl");
            let id = AgentId::from_transcript_path(&p);
            (takes_trip(id, 0)
                && !is_aimless_cycle(id, 0)
                && waypoint_index_for_cycle(id, 0, l.waypoints.len()) == pantry_idx)
                .then_some((p, id))
        })
        .expect("an agent lands at the pantry on cycle 0");

    let now = t0();
    let mut rig = WanderRig {
        layout: l,
        ..WanderRig::new(idle_slot(&path, now), Straight) // desk_index 0
    };
    rig.advance(now);
    rig.advance_until_leaves(now, PhaseKind::Seated, 120_000);

    let l = &rig.layout;
    let walk = rig.state();
    assert!(matches!(
        walk.wander.target.kind,
        WanderKind::Named {
            kind: WaypointKind::Pantry,
            seat: None, // Pantry is an obstacle (stands AT, not sits ON)
            ..
        }
    ));
    let desk = l.home_desks[0];
    let expected = crate::layout::approach_point(
        WaypointKind::Pantry.furniture(),
        l.waypoints[pantry_idx].pos,
        l.waypoints[pantry_idx].facing,
        l.pantry_counter_size(),
        &l.walkable,
        desk,
        &l.reachable,
    );
    assert_eq!(
        walk.wander.target.dest, expected,
        "the wander dest must equal the home-desk approach_point (core↔tui mirror)"
    );
}

#[test]
fn pick_wander_dest_falls_back_to_aimless_when_boxed_in() {
    use crate::layout::ReachSet;

    let mut l = layout();
    assert!(!l.waypoints.is_empty(), "layout must have waypoints");
    // Box EVERY waypoint in, so approach_point finds no allowed+reachable side
    // and returns its blocked-`pos` sentinel.
    l.walkable
        .mark_blocked(0, 0, l.walkable.width(), l.walkable.height(), 0);
    l.reachable = ReachSet::from_mask(&l.walkable, Point { x: 0, y: 0 });

    let id = (0u64..2000)
        .map(|i| AgentId::from_transcript_path(&format!("/p/boxed_{i}.jsonl")))
        .find(|id| takes_trip(*id, 0) && !is_aimless_cycle(*id, 0))
        .expect("should find a directed-trip agent");

    let origin = l.home_desks[0];
    let target = pick_wander_dest(id, 0, &l, origin, &SpotClaims::default());

    assert!(
        matches!(target.kind, WanderKind::Aimless),
        "a boxed-in waypoint (no reachable approach side) must amble aimlessly \
         (no waypoint index / kind / seat)",
    );
}

/// Dropping or forging the settle cell shows up as a mid-walk pop / wrong
/// render top-left, so pin the `Named{seat:None}`-vs-seat boundary directly.
#[test]
fn wander_named_seat_is_some_iff_the_destination_is_sat_on() {
    use crate::layout::furniture_def;
    // A full-size floor, because the tiny 120x96 `layout()` fixture has no seat
    // waypoints — only the obstacle side of the boundary.
    let l = SceneLayout::compute(240, 160, None).expect("fits");
    let origin = l.home_desks[0];
    let (mut saw_obstacle, mut saw_seat) = (false, false);
    for i in 0u64..5000 {
        let id = AgentId::from_transcript_path(&format!("/p/seatpin_{i}.jsonl"));
        if !takes_trip(id, 0) || is_aimless_cycle(id, 0) {
            continue;
        }
        if let WanderKind::Named { kind, seat, .. } =
            pick_wander_dest(id, 0, &l, origin, &SpotClaims::default()).kind
        {
            assert_eq!(
                seat.is_some(),
                furniture_def(kind.furniture()).occupies_pos,
                "Named.seat.is_some() must equal occupies_pos for {kind:?}",
            );
            if seat.is_some() {
                saw_seat = true;
            } else {
                saw_obstacle = true;
            }
        }
    }
    assert!(
        saw_obstacle,
        "sweep must resolve at least one obstacle (seat:None) waypoint"
    );
    assert!(
        saw_seat,
        "sweep must resolve at least one seat (seat:Some) waypoint"
    );
}

fn tripping_at(id: AgentId, wp_idx: usize, kind: WaypointKind) -> WalkState {
    let mut walk = WalkState::default();
    walk.wander.phase =
        WanderPhase::AtWaypoint(crate::physics::walk_profile(100, WalkIntent::WanderOut, id));
    walk.wander.target = WanderTarget {
        dest: Point { x: 0, y: 0 },
        kind: WanderKind::Named {
            wp_idx,
            kind,
            seat: None,
        },
    };
    walk
}

#[test]
fn spot_claims_holds_only_exclusive_spots_of_other_agents_out_on_a_trip() {
    let me = AgentId::from_transcript_path("/p/claims-me.jsonl");
    let sitter = AgentId::from_transcript_path("/p/claims-sitter.jsonl");
    let caller = AgentId::from_transcript_path("/p/claims-caller.jsonl");
    let stander = AgentId::from_transcript_path("/p/claims-stander.jsonl");
    let mut walks = HashMap::new();
    walks.insert(sitter, tripping_at(sitter, 3, WaypointKind::MeetingChair));
    walks.insert(caller, tripping_at(caller, 9, WaypointKind::PhoneBooth));
    walks.insert(stander, tripping_at(stander, 7, WaypointKind::Printer));
    walks.insert(me, tripping_at(me, 5, WaypointKind::Couch));

    let claims = spot_claims(&walks, me);
    assert!(claims.holds(3), "another agent's seat must be claimed");
    assert!(claims.holds(9), "a phone booth is exclusive → claimed");
    assert!(
        !claims.holds(7),
        "a shareable queue obstacle is not claimed"
    );
    assert!(
        !claims.holds(5),
        "an agent must not claim a spot against itself"
    );
}

/// The bootstrap / stale-resume path re-seats an agent at its desk WITHOUT
/// touching its target, so a stale `Named` spot would linger and block it for
/// everyone — the phase, not the kind, is the honest "out at the spot" signal.
#[test]
fn spot_claims_ignores_a_seated_agents_stale_target() {
    let me = AgentId::from_transcript_path("/p/claims-me.jsonl");
    let resumed = AgentId::from_transcript_path("/p/claims-resumed.jsonl");
    let mut walk = tripping_at(resumed, 3, WaypointKind::MeetingChair);
    walk.wander.phase = WanderPhase::Seated; // what the stale-resume bootstrap does
    let mut walks = HashMap::new();
    walks.insert(resumed, walk);

    assert!(
        !spot_claims(&walks, me).holds(3),
        "an agent re-seated at its desk must not keep holding a meeting seat"
    );
}

/// The probe works because a venue's seats are CONTIGUOUS in `layout.waypoints`
/// (`compute_waypoints` pushes a sofa's 3 / a table's 2 in one run), so probing
/// forward from the hashed index lands on a neighbouring seat of the SAME venue.
#[test]
fn a_claimed_seat_sends_the_agent_to_the_next_seat_of_the_same_venue() {
    let l = SceneLayout::compute(240, 160, None).expect("fits");
    let origin = l.home_desks[0];
    // A cycle-0 pick that is a MEETING seat, so the venue has siblings.
    let (id, taken) = (0u64..5000)
        .map(|i| AgentId::from_transcript_path(&format!("/p/probe_{i}.jsonl")))
        .find_map(|id| {
            if !takes_trip(id, 0) || is_aimless_cycle(id, 0) {
                return None;
            }
            match pick_wander_dest(id, 0, &l, origin, &SpotClaims::default()).kind {
                WanderKind::Named {
                    wp_idx,
                    kind: WaypointKind::MeetingSofa | WaypointKind::MeetingChair,
                    ..
                } => {
                    // The probe walks FORWARD, so a venue's LAST seat has no
                    // sibling ahead and the agent legitimately walks off.
                    let sibling_ahead = l
                        .waypoints
                        .get(wp_idx + 1)
                        .is_some_and(|w| w.room_id == l.waypoints[wp_idx].room_id);
                    sibling_ahead.then_some((id, wp_idx))
                }
                _ => None,
            }
        })
        .expect("some agent must pick a meeting seat on cycle 0");

    let mut claims = SpotClaims::default();
    claims.claim(taken);
    let target = pick_wander_dest(id, 0, &l, origin, &claims);
    let WanderKind::Named { wp_idx, kind, .. } = target.kind else {
        panic!("a claimed seat must not strand the agent into an aimless amble");
    };
    assert_ne!(wp_idx, taken, "the claimed seat must be refused");
    assert_eq!(
        l.waypoints[wp_idx].room_id, l.waypoints[taken].room_id,
        "the probe should land in the same meeting room, not walk off to another floor spot"
    );
    assert!(
        matches!(kind, WaypointKind::MeetingSofa | WaypointKind::MeetingChair),
        "the probe should find another meeting slot, got {kind:?}"
    );
}
