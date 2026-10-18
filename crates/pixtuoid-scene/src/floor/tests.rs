use super::*;
use crate::anim::Motion;
use pixtuoid_core::id::AgentId;
use pixtuoid_core::state::{ActivityState, FloorLocalDeskIndex};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn frame_layout_memo_matches_fresh_compute_across_hits_resizes_and_none() {
    let mut ctx = FloorCtx::new();
    let fresh = crate::layout::SceneLayout::compute_with_seed(192, 156, None, 0).unwrap();
    let a = ctx.frame_layout(192, 156, 0).unwrap();
    let b = ctx.frame_layout(192, 156, 0).unwrap();
    // Pointer identity first: the value-equality below would still pass a reverted
    // `Arc::new((**l).clone())`.
    assert!(
        std::sync::Arc::ptr_eq(&a, &b),
        "a memo hit must share the memoized Arc, not deep-clone it"
    );
    for l in [&a, &b] {
        assert_eq!(l.walkable, fresh.walkable);
        assert_eq!(l.reachable, fresh.reachable);
        assert_eq!(l.home_desks.len(), fresh.home_desks.len());
    }
    let resized = ctx.frame_layout(120, 100, 0).unwrap();
    let fresh_resized = crate::layout::SceneLayout::compute_with_seed(120, 100, None, 0).unwrap();
    assert_eq!(resized.walkable, fresh_resized.walkable);
    // A too-small buffer is None and must not poison the memo.
    assert!(ctx.frame_layout(3, 3, 0).is_none());
    assert_eq!(
        ctx.frame_layout(192, 156, 0).unwrap().walkable,
        fresh.walkable
    );
}

/// A new layout drops the router's cached paths, a height-only resize's too:
/// `route` revalidates a cached path against the overlay alone, never the
/// mask, so a stale one would walk through the new layout's walls.
#[test]
fn a_new_layout_drops_the_routers_cached_paths() {
    use crate::pathfind::Router;
    use pixtuoid_core::walkable::OccupancyOverlay;
    let cache_a_route = |ctx: &mut FloorCtx, l: &crate::layout::SceneLayout| {
        let mut walkable = (0..l.buf_h)
            .flat_map(|y| (0..l.buf_w).map(move |x| crate::layout::Point { x, y }))
            .filter(|p| l.is_walkable(p.x, p.y));
        let from = walkable.next().expect("a walkable cell");
        let to = walkable.next_back().expect("another walkable cell");
        ctx.router
            .route(&l.walkable, &OccupancyOverlay::new(), from, to);
        assert!(!ctx.router.is_empty(), "the route was cached");
    };
    let mut ctx = FloorCtx::new();
    let l = ctx.frame_layout(192, 156, 0).unwrap();
    cache_a_route(&mut ctx, &l);
    ctx.frame_layout(192, 156, 0).unwrap();
    assert!(!ctx.router.is_empty(), "the same layout keeps its paths");
    ctx.frame_layout(120, 100, 0).unwrap();
    assert!(ctx.router.is_empty(), "a new layout drops its paths");
    for h in 40..=240 {
        let Some(l) = ctx.frame_layout(192, h, 0) else {
            continue;
        };
        cache_a_route(&mut ctx, &l);
        if ctx.frame_layout(192, h + 1, 0).is_some() {
            assert!(
                ctx.router.is_empty(),
                "192x{h} to 192x{} kept its paths",
                h + 1
            );
        }
    }
}

#[test]
fn daemons_projects_onto_the_ground_floor_only() {
    use pixtuoid_core::state::{DaemonInstanceId, DaemonLiveness, DaemonPresence};
    let mut scene = SceneState::uniform(16);
    scene.floor_capacities[1] = 16;
    // TWO gateways of one source, so the projection is also pinned to carry the
    // WHOLE roster (an `Option`-shaped projection would drop the second lobster).
    for port in ["18789", "19789"] {
        scene.insert_daemon(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            DaemonInstanceId::new(port).expect("non-empty"),
            DaemonPresence {
                liveness: DaemonLiveness::UP,
                active_sessions: 0,
                last_seen: SystemTime::UNIX_EPOCH,
                entered_at: SystemTime::UNIX_EPOCH,
                in_flight_runs: Default::default(),
                current_pid: Some(1),
            },
        );
    }
    assert_eq!(
        project_floor_scene(&scene, 0).daemons().count(),
        2,
        "floor 0 carries EVERY gateway mascot"
    );
    assert_eq!(
        project_floor_scene(&scene, 1).daemons().count(),
        0,
        "floor 1+ must NOT (render-once invariant)"
    );
}

/// A floor past the building's projects as an empty one, agreeing with
/// `build_floor_scene`: the last real floor's agents don't leak through.
#[test]
fn a_floor_past_the_building_projects_empty() {
    use pixtuoid_core::state::MAX_FLOORS;
    let scene = make_scene(2 * MAX_FLOORS, 2);
    assert!(scene.agents.values().any(|a| a.floor_idx == MAX_FLOORS - 1));
    assert!(build_floor_scene(&scene, MAX_FLOORS).is_empty());
    let past = project_floor_scene(&scene, MAX_FLOORS);
    assert!(past.agents.is_empty());
    assert_eq!(past.total_capacity(), 0);
}

#[test]
fn door_anim_excludes_arrived_entry_profiles() {
    use crate::walk::WalkState;
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let id = AgentId::from_transcript_path("/p/door.jsonl");
    let mut fctx = FloorCtx::new();
    let mut walk = WalkState::default();
    walk.entry = Some(crate::walk::WalkLeg {
        started_at: t0,
        profile: WalkProfile {
            duration_ms: 2000,
            pause_ms: 300,
            path_len_octile: 500,
            v_cruise: 0.36,
            accel: 6.5e-4,
        },
        from: crate::layout::Point { x: 0, y: 0 },
    });
    fctx.walks.insert(id, walk);

    fctx.recompute_door_anim_max_ms(t0 + Duration::from_millis(1000));
    assert_eq!(
        fctx.door_anim_max_ms, 2300,
        "in-flight entry walk should drive the door cosmetic window"
    );

    // Past arrival, even though WalkState.entry is never cleared for this agent.
    fctx.recompute_door_anim_max_ms(t0 + Duration::from_millis(3000));
    assert_eq!(
        fctx.door_anim_max_ms, 0,
        "an arrived entry profile must not hold the door open for the agent's lifetime"
    );
}

#[test]
fn floor_ctx_default_equals_new() {
    let d = FloorCtx::default();
    assert_eq!(
        d.door_anim_max_ms, 0,
        "FloorCtx::default() must match new() (door_anim_max_ms == 0)"
    );
    assert!(d.walks.is_empty(), "default FloorCtx has no walks");
}

#[test]
fn vacancy_dim_default_equals_new() {
    assert_eq!(
        VacancyDim::default().level(),
        VacancyDim::new().level(),
        "VacancyDim::default() must equal new()"
    );
    assert_eq!(
        VacancyDim::default().level(),
        1.0,
        "a fresh VacancyDim is fully lit"
    );
}

fn make_scene(n: usize, max_desks: usize) -> SceneState {
    let mut s = SceneState::uniform(max_desks);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for i in 0..n {
        let id = AgentId::from_transcript_path(&format!("/p/{i}.jsonl"));
        let floor_idx = s.floor_of(GlobalDeskIndex(i));
        s.agents.insert(
            id,
            AgentSlot {
                agent_id: id,
                source: Arc::from("cc"),
                session_id: Arc::from(format!("s{i}").as_str()),
                cwd: Arc::from(Path::new("/repo")),
                label: format!("a{i}").into(),
                state: ActivityState::Idle,
                state_started_at: now,
                created_at: now,
                last_event_at: now,
                exiting_at: None,
                pending_idle_at: None,

                desk_index: GlobalDeskIndex(i),
                floor_idx,
                tool_call_count: 0,
                active_ms: 0,
                unknown_cwd: false,
                parent_id: None,
                pid: None,
                model: None,
                effort: None,
                tokens_used: 0,
                last_usage: None,
            },
        );
    }
    s
}

#[test]
fn floor_of_maps_desk_to_floor() {
    let s = SceneState::uniform(16);
    assert_eq!(s.floor_of(GlobalDeskIndex(0)), 0);
    assert_eq!(s.floor_of(GlobalDeskIndex(15)), 0);
    assert_eq!(s.floor_of(GlobalDeskIndex(16)), 1);
    assert_eq!(s.floor_of(GlobalDeskIndex(31)), 1);
    assert_eq!(s.floor_of(GlobalDeskIndex(32)), 2);
}

#[test]
fn floor_local_desk_remaps_to_floor_range() {
    let s = SceneState::uniform(16);
    assert_eq!(
        s.floor_local_desk(GlobalDeskIndex(0)),
        FloorLocalDeskIndex(0)
    );
    assert_eq!(
        s.floor_local_desk(GlobalDeskIndex(16)),
        FloorLocalDeskIndex(0)
    );
    assert_eq!(
        s.floor_local_desk(GlobalDeskIndex(17)),
        FloorLocalDeskIndex(1)
    );
    assert_eq!(
        s.floor_local_desk(GlobalDeskIndex(31)),
        FloorLocalDeskIndex(15)
    );
}

#[test]
fn num_floors_with_overflow() {
    let scene = make_scene(20, 16);
    assert_eq!(num_floors(&scene), 2);
}

#[test]
fn num_floors_exact_fit() {
    let scene = make_scene(16, 16);
    assert_eq!(num_floors(&scene), 1);
}

#[test]
fn num_floors_empty() {
    let scene = make_scene(0, 16);
    assert_eq!(num_floors(&scene), 1);
}

#[test]
fn build_floor_scene_filters_and_remaps() {
    let scene = make_scene(20, 16);

    let floor0 = build_floor_scene(&scene, 0);
    assert_eq!(floor0.len(), 16);
    for p in &floor0 {
        assert!(p.desk.0 < 16, "local desk {} out of range", p.desk.0);
    }

    let floor1 = build_floor_scene(&scene, 1);
    assert_eq!(floor1.len(), 4);
    let mut indices: Vec<usize> = floor1.iter().map(|p| p.desk.0).collect();
    indices.sort();
    assert_eq!(indices, vec![0, 1, 2, 3]);
    // The slot's GLOBAL desk_index is untouched: floor 1's agents keep their real
    // allocation until project_floor_scene's documented re-host.
    let mut globals: Vec<usize> = floor1.iter().map(|p| p.slot.desk_index.0).collect();
    globals.sort();
    assert_eq!(globals, vec![16, 17, 18, 19]);
}

#[test]
fn build_floor_scene_remap_is_local_global_coincident() {
    // Within a projected `uniform(cap)` scene the global desk space coincides with
    // its only floor's local space — what makes `single_floor_local` an identity.
    let scene = make_scene(20, 16);
    for floor_idx in 0..num_floors(&scene) {
        let projected = project_floor_scene(&scene, floor_idx);
        for slot in projected.agents.values() {
            assert_eq!(projected.floor_of(slot.desk_index), 0);
            assert_eq!(
                projected.floor_local_desk(slot.desk_index).0,
                slot.desk_index.0,
                "projected scene: bridge must be the identity"
            );
            assert_eq!(
                projected.floor_local_desk(slot.desk_index),
                slot.desk_index.single_floor_local(),
                "typed bridge and identity cast must agree in a projection"
            );
        }
    }
}

#[test]
fn build_floor_scene_skips_agent_below_grown_offset() {
    // Desk 5 is assigned on floor 1 while floor 0 has capacity 4; floor 0 then
    // grows to 8, so `floor_range(1).start` passes the agent's own desk.
    let mut s = SceneState::new([4, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let id = AgentId::from_transcript_path("/p/stale.jsonl");
    s.agents.insert(
        id,
        AgentSlot {
            agent_id: id,
            source: Arc::from("cc"),
            session_id: Arc::from("s"),
            cwd: Arc::from(Path::new("/repo")),
            label: "stale".into(),
            state: ActivityState::Idle,
            state_started_at: now,
            created_at: now,
            last_event_at: now,
            exiting_at: None,
            pending_idle_at: None,
            desk_index: GlobalDeskIndex(5),
            floor_idx: 1,
            tool_call_count: 0,
            active_ms: 0,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        },
    );
    s.floor_capacities = [8, 4, 0, 0, 0, 0, 0, 0, 0, 0];
    let floor1 = build_floor_scene(&s, 1);
    assert!(
        floor1.is_empty(),
        "agent below grown offset must be skipped, not mapped to desk 0"
    );
}

#[test]
fn num_floors_variable_capacities() {
    // F0: 0..4, F1: 4..12 — 6 agents span 2 floors
    let mut s = SceneState::new([4, 8, 6, 4, 2, 0, 0, 0, 0, 0]);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for i in 0..6 {
        let id = AgentId::from_transcript_path(&format!("/p/{i}.jsonl"));
        let floor_idx = s.floor_of(GlobalDeskIndex(i));
        s.agents.insert(
            id,
            AgentSlot {
                agent_id: id,
                source: Arc::from("cc"),
                session_id: Arc::from(format!("s{i}").as_str()),
                cwd: Arc::from(Path::new("/repo")),
                label: format!("a{i}").into(),
                state: ActivityState::Idle,
                state_started_at: now,
                created_at: now,
                last_event_at: now,
                exiting_at: None,
                pending_idle_at: None,
                desk_index: GlobalDeskIndex(i),
                floor_idx,
                tool_call_count: 0,
                active_ms: 0,
                unknown_cwd: false,
                parent_id: None,
                pid: None,
                model: None,
                effort: None,
                tokens_used: 0,
                last_usage: None,
            },
        );
    }
    assert_eq!(num_floors(&s), 2);
}

#[test]
fn transition_t_progresses() {
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let tr = FloorTransition::new(0, 1, start);

    assert!((tr.t(start) - 0.0).abs() < f32::EPSILON);

    let mid = start + Duration::from_millis(450);
    let t_mid = tr.t(mid);
    assert!(
        t_mid > 0.0 && t_mid < 1.0,
        "mid should be between 0 and 1, got {t_mid}"
    );

    let end = start + Duration::from_millis(900);
    assert!((tr.t(end) - 1.0).abs() < f32::EPSILON);
    assert!(!tr.is_done(start + Duration::from_millis(450)));
    assert!(tr.is_done(end));
}

#[test]
fn transition_t_clamps_past_duration() {
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let tr = FloorTransition::new(0, 1, start);

    let past = start + Duration::from_millis(1000);
    assert!((tr.t(past) - 1.0).abs() < f32::EPSILON);
    assert!(tr.is_done(past));
}

fn t0() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
}

#[test]
fn light_steady_state_populated() {
    let mut dim = VacancyDim::new();
    let start = t0();
    for ms in (0..3_000).step_by(crate::anim::PAINT_FRAME_MS as usize) {
        let level = dim.tick(false, start + Duration::from_millis(ms));
        assert!(
            (level - 1.0).abs() < 1e-6,
            "populated steady state drifted: ms={ms} level={level}"
        );
    }
}

#[test]
fn light_holds_during_debounce_window() {
    let mut dim = VacancyDim::new();
    let start = t0();
    dim.tick(true, start);
    // 4 s after going empty, inside the 5 s debounce.
    let level = dim.tick(true, start + Duration::from_millis(4_000));
    assert!(
        (level - 1.0).abs() < 1e-6,
        "level dropped before debounce expired: {level}"
    );
}

#[test]
fn light_eases_toward_min_after_debounce() {
    let mut dim = VacancyDim::new();
    let start = t0();
    dim.tick(true, start);
    // the last full-light tick, so the fade below spans only its own second
    dim.tick(
        true,
        start + Duration::from_millis(VacancyDim::EMPTY_DEBOUNCE_MS - 1),
    );
    // 6 s: the debounce expired 1 s ago, ~1.25 tau of fade.
    let level = dim.tick(true, start + Duration::from_millis(6_000));
    assert!(level < 0.95, "no fade started after debounce: {level}");
    assert!(level > VacancyDim::MIN_LEVEL, "overshot floor: {level}");
}

#[test]
fn light_converges_to_min_when_empty_long_enough() {
    let mut dim = VacancyDim::new();
    let start = t0();
    // A realistic frame cadence for 30 s, so the exponential ease has fully landed.
    for ms in (0..30_000).step_by(crate::anim::PAINT_FRAME_MS as usize) {
        dim.tick(true, start + Duration::from_millis(ms));
    }
    let level = dim.level();
    assert!(
        (level - VacancyDim::MIN_LEVEL).abs() < 1e-3,
        "did not converge to MIN_LEVEL: {level}"
    );
}

#[test]
fn light_rises_back_when_repopulated() {
    let mut dim = VacancyDim::new();
    let start = t0();
    for ms in (0..20_000).step_by(crate::anim::PAINT_FRAME_MS as usize) {
        dim.tick(true, start + Duration::from_millis(ms));
    }
    assert!(dim.level() < 0.2);
    let later = start + Duration::from_millis(20_000);
    for ms in (0..3_000).step_by(crate::anim::PAINT_FRAME_MS as usize) {
        dim.tick(false, later + Duration::from_millis(ms));
    }
    let level = dim.level();
    assert!(level > 0.95, "did not rise back when repopulated: {level}");
}

#[test]
fn light_resets_empty_since_when_repopulated() {
    let mut dim = VacancyDim::new();
    let start = t0();
    dim.tick(true, start);
    dim.tick(true, start + Duration::from_millis(3_000));
    dim.tick(false, start + Duration::from_millis(3_500));
    // Empty again: the debounce must restart here, so the 7.5 s sample is only
    // 3.9 s into the new window and must still hold at 1.0.
    dim.tick(true, start + Duration::from_millis(3_600));
    let level = dim.tick(true, start + Duration::from_millis(7_500));
    assert!(
        (level - 1.0).abs() < 1e-6,
        "empty_since did not reset on repopulate: {level}"
    );
}

#[test]
fn light_large_dt_does_not_overshoot_or_nan() {
    let mut dim = VacancyDim::new();
    let start = t0();
    dim.tick(true, start);
    let later = start + Duration::from_millis(VacancyDim::EMPTY_DEBOUNCE_MS + 1_000);
    let level = dim.tick(true, later);
    assert!(level.is_finite(), "level went non-finite: {level}");
    assert!(
        level >= VacancyDim::MIN_LEVEL - 1e-6,
        "level undershot floor: {level}"
    );
}

#[test]
fn light_backward_clock_jump_does_not_move_level() {
    let mut dim = VacancyDim::new();
    let start = t0();
    dim.tick(false, start);
    let before = dim.level();
    // A backward "now" makes duration_since() error; the impl's `.ok()` collapses
    // dt to 0.
    let backward = start - Duration::from_millis(500);
    let level = dim.tick(true, backward);
    assert!(
        (level - before).abs() < 1e-9,
        "backward clock jump moved level: before={before} after={level}"
    );
}

#[test]
fn light_snap_to_empty_forces_min_level() {
    let mut dim = VacancyDim::new();
    dim.snap_to_empty();
    assert!((dim.level() - VacancyDim::MIN_LEVEL).abs() < f32::EPSILON);
}

#[test]
fn coffee_record_stamps_only_new_carriers_and_evict_follows_the_scene() {
    let id = AgentId::from_parts("claude-code", "coffee-test");
    let t0 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let t1 = t0 + std::time::Duration::from_secs(60);
    let mut coffee = CoffeeState::default();
    coffee.record([id], t0);
    assert_eq!(coffee.map().get(&id), Some(&t0), "a new carrier is stamped");
    coffee.record([id], t1);
    assert_eq!(
        coffee.map().get(&id),
        Some(&t0),
        "an already-recorded carrier keeps its original fetch stamp"
    );
    let empty = SceneState::new([8; MAX_FLOORS]);
    coffee.evict_missing(&empty);
    assert!(coffee.map().is_empty());
}

#[test]
fn coffee_second_trip_after_steam_window_restamps() {
    let id = AgentId::from_parts("claude-code", "coffee-refetch");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut coffee = CoffeeState::default();
    coffee.record([id], t0);
    let within = t0 + Duration::from_secs(CoffeeState::STEAM_WINDOW_SECS - 1);
    coffee.record([id], within);
    assert_eq!(
        coffee.map().get(&id),
        Some(&t0),
        "a re-report within the steam window keeps the original stamp"
    );
    let refetch = t0 + Duration::from_secs(CoffeeState::STEAM_WINDOW_SECS * 3);
    coffee.record([id], refetch);
    assert_eq!(
        coffee.map().get(&id),
        Some(&refetch),
        "a fetch after the steam window expired must restamp"
    );
}

#[test]
fn coffee_record_keeps_stamp_on_a_backward_clock_step() {
    // An NTP/suspend step makes `duration_since` err, which must read as
    // not-expired: the two forward tests can't catch a treat-error-as-expired
    // regression because their `duration_since` never errs.
    let id = AgentId::from_parts("claude-code", "coffee-backclock");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut coffee = CoffeeState::default();
    coffee.record([id], t0);
    coffee.record([id], t0 - Duration::from_secs(10));
    assert_eq!(
        coffee.map().get(&id),
        Some(&t0),
        "a backward clock step must keep the original stamp, not rewind it"
    );
}

#[test]
fn floor_capacity_clamps_to_zero_on_a_too_small_buffer() {
    assert_eq!(floor_capacity(3, 3, 0), 0);
    assert!(floor_capacity(192, 160, 0) > 0);
}

#[test]
fn transition_escapes_a_backward_clock_step() {
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let tr = FloorTransition::new(0, 1, start);
    let wobble = start - Duration::from_millis(100);
    assert!(
        !tr.is_done(wobble),
        "a small wobble must not abort the slide"
    );
    assert!((tr.t(wobble) - 0.0).abs() < f32::EPSILON);
    // Without the escape the renderer stays wedged in the transition composite —
    // no labels, tooltips or hit-testing — until the clock re-passes started_at.
    let stepped = start - Duration::from_millis(tr.duration_ms * 2);
    assert!(
        tr.is_done(stepped),
        "a large backward clock step must complete the transition"
    );
}

/// One classic frame of `scene` at `now` through `session`'s entry.
fn classic_frame(
    session: &mut FloorSession,
    pack: &Arc<crate::pack::OfficeArt>,
    scene: &SceneState,
    now: SystemTime,
    floor: FloorMeta,
    size: Size,
) -> Option<Arc<crate::layout::SceneLayout>> {
    session.render(
        crate::look::Look::Classic,
        crate::look::RenderInputs {
            world: FloorInputs {
                scene,
                pack,
                now,
                floor,
                pets: PetInputs::default(),
            },
            theme: crate::theme::theme_by_name("normal").expect("normal theme exists"),
            size,
            place: crate::look::Place::default(),
            debug_walkable: false,
            dim: crate::look::Dim::NONE,
        },
    );
    session.picture().map(|p| Arc::clone(p.hits.layout()))
}

#[test]
fn the_classic_paints_the_flame_crown_for_a_top_tier_agent() {
    // Driven through the FULL pass: a projection or sim/paint hop dropping
    // slot.model/effort fails here while the unit-level paint test stays green.
    let pack = Arc::new(crate::pack::test_office());
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let mut scene = make_scene(1, 8);
    let slot = scene.agents.values_mut().next().expect("one agent");
    slot.model = Some("claude-fable-5".into());
    slot.effort = Some(pixtuoid_core::state::EffortObservation::new(
        "ultra".into(),
        now,
    ));
    let mut session = FloorSession::new(Arc::clone(&pack));
    classic_frame(
        &mut session,
        &pack,
        &scene,
        now,
        FloorMeta::ground(),
        Size { w: 192, h: 160 },
    )
    .expect("layout");
    // Against the SAME scene with the crown's inputs cleared: the foreground's
    // day/night wash blends every drawable, so the crown's authored RGB never
    // reaches the buffer and an equality probe pins nothing. What must survive
    // the full pass is that the crown CHANGES the frame.
    let mut plain = scene.clone();
    let slot = plain.agents.values_mut().next().expect("one agent");
    slot.model = None;
    slot.effort = None;
    let mut session2 = FloorSession::new(Arc::clone(&pack));
    classic_frame(
        &mut session2,
        &pack,
        &plain,
        now,
        FloorMeta::ground(),
        Size { w: 192, h: 160 },
    )
    .expect("layout");
    let (buf, buf2) = (
        session.picture().expect("classic buffer").pixels,
        session2.picture().expect("classic buffer").pixels,
    );
    // What this pins is that model/effort REACH the painter through projection
    // and the sim/paint hop — not that the crown itself drew. The burn tier also
    // tints the sprite over the crown's own pixels, so no scoping of this diff
    // can isolate the crown; `paint_flame_crown_draws_its_pattern` owns that half.
    let differing = (0..buf.height())
        .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| buf.get(x, y) != buf2.get(x, y))
        .count();
    assert!(
        differing > 0,
        "a top-tier agent rendered byte-identical to a model-less one — \
         slot.model/effort were dropped before the painter"
    );
}

#[test]
fn the_classic_paints_records_coffee_state_and_survives_a_tiny_buffer() {
    let pack = Arc::new(crate::pack::test_office());
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let scene = SceneState::new([8; MAX_FLOORS]);
    let mut session = FloorSession::new(Arc::clone(&pack));

    let none = classic_frame(
        &mut session,
        &pack,
        &scene,
        now,
        FloorMeta::ground(),
        Size { w: 8, h: 8 },
    );
    assert!(none.is_none(), "an unlayoutable size returns None");
    let buf = session.floor().raster.pixels().expect("classic buffer");
    assert_eq!(
        (buf.width(), buf.height()),
        (8, 8),
        "the buffer was still sized"
    );

    let layout = classic_frame(
        &mut session,
        &pack,
        &scene,
        now,
        FloorMeta::ground(),
        Size { w: 160, h: 96 },
    );
    assert!(layout.is_some(), "a layoutable size returns the layout");
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let bg = theme.surface.bg_fallback;
    let buf = session.picture().expect("classic buffer").pixels;
    assert!(
        buf.as_slice()
            .iter()
            .any(|p| *p != pixtuoid_core::sprite::Rgb { r: 0, g: 0, b: 0 } && *p != bg),
        "the pixel pass painted office content"
    );
}

#[test]
fn floor_session_render_owns_the_dual_eviction() {
    let pack = Arc::new(crate::pack::test_office());
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let gone = AgentId::from_parts("claude-code", "session-evict");
    let mut session = FloorSession::new(Arc::clone(&pack));
    session
        .floor_mut()
        .ctx
        .walks
        .insert(gone, WalkState::default());
    session.office.coffee.insert(gone, now);

    let scene = SceneState::new([8; MAX_FLOORS]);
    session.render(
        crate::look::Look::Classic,
        crate::look::RenderInputs {
            world: FloorInputs {
                scene: &scene,
                pack: &pack,
                now,
                floor: FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            theme,
            size: Size { w: 160, h: 96 },
            place: crate::look::Place::default(),
            debug_walkable: false,
            dim: crate::look::Dim::NONE,
        },
    );
    assert!(session.picture().is_some(), "a layoutable size renders");
    assert!(
        !session.floor().ctx.walks.contains_key(&gone),
        "render() evicts the floor half (walks) — the floating-leak class"
    );
    assert!(
        !session.office.coffee.map().contains_key(&gone),
        "render() evicts the office half (coffee) — the cup leaves with the agent"
    );
}

#[test]
fn floor_session_render_surfaces_the_sims_occupied_waypoints() {
    // `last_occupied` is the set the shared `AudioObserver` reads, so recording it
    // here is what lets a windowed painter avoid re-running the sim.
    let pack = Arc::new(crate::pack::test_office());
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut scene = make_scene(1, 8);
    for slot in scene.agents.values_mut() {
        slot.created_at = now0;
        slot.state_started_at = now0;
        slot.last_event_at = now0;
    }
    let mut session = FloorSession::new(Arc::clone(&pack));
    assert!(
        session.view.last_occupied.is_empty(),
        "empty before any render"
    );
    // Requiring the FALL back to empty is the anti-stick tooth: an accumulating
    // `last_occupied` is monotone non-decreasing and can never produce it.
    let mut occupied_ever = false;
    let mut fell_back_empty = false;
    for step in 0..600u64 {
        let now = now0 + Duration::from_secs(3 * step);
        session.render(
            crate::look::Look::Classic,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                size: Size { w: 160, h: 96 },
                place: crate::look::Place::default(),
                debug_walkable: false,
                dim: crate::look::Dim::NONE,
            },
        );
        let layout = Arc::clone(session.picture().expect("160x96 lays out").hits.layout());
        if session.view.last_occupied.is_empty() {
            if occupied_ever {
                fell_back_empty = true;
                break;
            }
        } else {
            for &wp in &session.view.last_occupied {
                assert!(
                    wp < layout.waypoints.len(),
                    "occupied index {wp} must be a real waypoint"
                );
            }
            occupied_ever = true;
        }
    }
    assert!(
        occupied_ever,
        "the idle agent never occupied a waypoint in 30 min of sim"
    );
    assert!(
        fell_back_empty,
        "occupancy never fell back to empty — last_occupied accumulates instead of tracking the frame"
    );
    session.render(
        crate::look::Look::Classic,
        crate::look::RenderInputs {
            world: FloorInputs {
                scene: &scene,
                pack: &pack,
                now: now0,
                floor: FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            theme,
            size: Size { w: 8, h: 8 },
            place: crate::look::Place::default(),
            debug_walkable: false,
            dim: crate::look::Dim::NONE,
        },
    );
    assert!(session.picture().is_none());
    assert!(
        session.view.last_occupied.is_empty(),
        "an unlayoutable render clears the stale occupancy"
    );
}

#[test]
fn floor_session_step_advances_the_world_without_a_pixel_buffer() {
    let pack = Arc::new(crate::pack::test_office());
    let scene = make_scene(1, 8);
    let id = AgentId::from_transcript_path("/p/0.jsonl");
    let t = t0() + Duration::from_millis(100); // 100ms in: entry walk in flight
    let mut session = FloorSession::new(Arc::clone(&pack));

    let frame = session
        .step(
            crate::floor::FloorInputs {
                scene: &scene,
                pack: &pack,
                now: t,
                floor: FloorMeta::ground(),
                pets: crate::floor::PetInputs::default(),
            },
            Size { w: 160, h: 96 },
        )
        .expect("a layoutable size steps")
        .frame;
    assert!(
        frame.poses.contains_key(&id),
        "the frame carries the agent's routed pose"
    );
    assert!(
        session.floor().ctx.walks.contains_key(&id),
        "the sim advanced: the entry leg was snapshotted into walks"
    );
    assert!(
        session.floor().ctx.door_anim_max_ms > 0,
        "the epilogue ran headlessly: the in-flight entry drives the door clamp"
    );
    assert!(
        session.floor().raster.pixels().is_none(),
        "no pixel buffer was bought"
    );

    assert!(
        session
            .step(
                crate::floor::FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now: t,
                    floor: FloorMeta::ground(),
                    pets: crate::floor::PetInputs::default()
                },
                Size { w: 8, h: 8 }
            )
            .is_none(),
        "an unlayoutable size steps nothing"
    );
}

/// `step` hands back the memoized layout itself, not an equal copy.
#[test]
fn step_hands_back_the_layout_the_sim_stepped_on() {
    let pack = Arc::new(crate::pack::test_office());
    let scene = make_scene(1, 8);
    let size = Size { w: 160, h: 96 };
    let meta = FloorMeta::ground();
    let mut session = FloorSession::new(Arc::clone(&pack));
    let stepped = session
        .step(
            crate::floor::FloorInputs {
                scene: &scene,
                pack: &pack,
                now: t0(),
                floor: meta,
                pets: crate::floor::PetInputs::default(),
            },
            size,
        )
        .expect("a layoutable size steps");
    let memoized = session
        .floor_mut()
        .ctx
        .frame_layout(size.w, size.h, meta.floor_seed)
        .expect("the memoized layout");
    assert!(Arc::ptr_eq(&stepped.layout, &memoized));
}

/// A painter steps one floor through its projected scene, which holds no
/// other floor's agents: their coffee must outlive it.
#[test]
fn stepping_a_projected_floor_keeps_other_floors_coffee() {
    let pack = Arc::new(crate::pack::test_office());
    let scene = make_scene(17, 16);
    let downstairs = AgentId::from_transcript_path("/p/0.jsonl");
    assert_eq!(scene.agents[&downstairs].floor_idx, 0);
    let mut office = PerOffice::new();
    office.coffee.insert(downstairs, t0());
    let mut upstairs = PerFloor::new(Arc::clone(&pack));
    let projected = project_floor_scene(&scene, 1);
    let stepped = step_floor(
        &mut upstairs.ctx,
        &mut office.coffee,
        &mut office.chitchat,
        crate::floor::FloorInputs {
            scene: &projected,
            pack: &pack,
            now: t0(),
            floor: FloorMeta::for_floor(1, num_floors(&scene)),
            pets: crate::floor::PetInputs::default(),
        },
        Size { w: 160, h: 96 },
    );
    assert!(stepped.is_some());
    assert!(office.coffee.map().contains_key(&downstairs));
}

#[test]
fn session_types_default_equals_new() {
    let floor = PerFloor::new(Arc::new(crate::pack::test_office()));
    assert_eq!(floor.ctx.door_anim_max_ms, 0);
    assert!(floor.raster.pixels().is_none());
    assert!(PerOffice::default().coffee.map().is_empty());
    assert!(PerOffice::default().chitchat.is_empty());
}

#[test]
fn audio_observer_frame_composes_stems_from_the_scene() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = make_scene(4, 16);
    let occupied = std::collections::HashSet::new();
    let mut obs = AudioObserver::default();
    let frame = obs.frame(&scene, &occupied, |_| None, FloorMeta::ground(), now);
    let precip = crate::sky::rain_at(now, crate::sky::WeatherPolicy::Clock);
    assert_eq!(
        frame.stems,
        crate::audio::stem_levels(&crate::tally::per_floor_counts(&scene)[0], precip),
        "stems must equal stem_levels(per_floor_counts[floor], precip)"
    );
}

#[test]
fn audio_observer_reprimes_on_floor_switch_so_the_new_floor_is_silent() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = make_scene(4, 16); // agents live on floor 0
    let printer = |i: usize| (i == 0 || i == 1).then_some(crate::layout::WaypointKind::Printer);
    let mut obs = AudioObserver::default();

    let _ = obs.frame(
        &scene,
        &std::collections::HashSet::new(),
        printer,
        FloorMeta::ground(),
        now,
    );
    assert_eq!(obs.primed_floor(), Some(0));

    // Switch to floor 1 with an appliance ALREADY occupied — this would fire
    // PrinterWhir without the reprime.
    let occ0: std::collections::HashSet<usize> = [0usize].into_iter().collect();
    let switch = obs.frame(&scene, &occ0, printer, FloorMeta::for_floor(1, 2), now);
    assert_eq!(obs.primed_floor(), Some(1));
    assert!(
        switch.events.is_empty(),
        "a floor switch reprimes silently — no cue volley for the new floor"
    );

    let occ01: std::collections::HashSet<usize> = [0usize, 1usize].into_iter().collect();
    let next = obs.frame(&scene, &occ01, printer, FloorMeta::for_floor(1, 2), now);
    assert!(
        next.events.contains(&crate::audio::OneShot::PrinterWhir),
        "after the reprime, a newly occupied printer still fires"
    );
}

#[test]
fn audio_observer_keeps_cue_edges_warm_so_delivery_resume_fires_no_volley() {
    // Mute-gating: the painter calls frame() EVERY world-frame and gates only
    // DELIVERY, so an arrival during a muted stretch is still consumed here.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let empty = make_scene(0, 16);
    let one = make_scene(1, 16);
    let occ = std::collections::HashSet::new();
    let mut obs = AudioObserver::default();

    let _ = obs.frame(&empty, &occ, |_| None, FloorMeta::ground(), now);
    let arrival = obs.frame(&one, &occ, |_| None, FloorMeta::ground(), now);
    assert!(
        arrival.events.contains(&crate::audio::OneShot::DoorChime),
        "an arrival chimes on the frame it happens"
    );
    let resumed = obs.frame(&one, &occ, |_| None, FloorMeta::ground(), now);
    assert!(
        !resumed.events.contains(&crate::audio::OneShot::DoorChime),
        "no volley on resume — the observer saw the agent while muted"
    );
}

// The frame-seam scale-invariance property lives in `render_scale.rs` — the
// version here asserted only desk count + buffer width, which hold on a garbled frame.

#[test]
fn the_foreground_layer_is_lit_by_the_clock() {
    // The day/night overlays sweep the floor band BEFORE the drawables paint, so
    // every enqueued piece — desks, appliances, plants, wall decor, characters,
    // chairs, partitions — carried no time-of-day term at all. Measured at the
    // whole LAYER rather than one piece: the paint loop is the shared seam, so a
    // new DrawableKind inherits this without touching the test.
    use crate::localclock::at_hour;
    // The weather is picked per UTC slot, so the same two local hours land on
    // different weathers in different zones: pinned, the pair differs by the
    // clock alone.
    let clear = crate::sky::WeatherPolicy::Forced(crate::sky::Weather::Clear);
    let render = |now: SystemTime| {
        let pack = Arc::new(crate::pack::test_office());
        let scene = make_scene(6, 8);
        let mut session = FloorSession::new(Arc::clone(&pack));
        classic_frame(
            &mut session,
            &pack,
            &scene,
            now,
            FloorMeta::ground().with_weather(clear),
            Size { w: 192, h: 160 },
        )
        .expect("layout");
        session
    };
    let (noon_session, night_session) = (render(at_hour(12)), render(at_hour(2)));
    let (noon, night) = (
        noon_session.picture().expect("classic buffer").pixels,
        night_session.picture().expect("classic buffer").pixels,
    );
    let (w, h) = (noon.width(), noon.height());
    let frozen = |x: u16, y: u16| noon.get(x, y) == night.get(x, y);
    // CLUSTERED, not counted. Two different blends can round to one u8, so a few
    // scattered matches are arithmetic — a painter left outside the clock instead
    // freezes a whole PIECE, and every pixel of it has a frozen neighbour. A
    // count needs a threshold, and a threshold is what let the corridor runner
    // and the pantry mats pass at 8.3% under a 12% ceiling.
    let mut clustered = Vec::new();
    for y in (h * 30 / 100)..(h * 92 / 100) {
        for x in 0..w {
            if frozen(x, y)
                && [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|(dx, dy)| {
                        let (nx, ny) = (i32::from(x) + dx, i32::from(y) + dy);
                        (0..i32::from(w)).contains(&nx)
                            && (0..i32::from(h)).contains(&ny)
                            && frozen(nx as u16, ny as u16)
                    })
            {
                clustered.push((x, y));
            }
        }
    }
    assert!(
        clustered.is_empty(),
        "{} interior pixels are byte-identical at noon and midnight AND adjacent \
         to another such pixel — a painter is running outside the clock, first at \
         {:?}",
        clustered.len(),
        clustered.first()
    );
}

fn neon_mood(active: usize, waiting: usize, idle: usize) -> crate::neon_sign::OfficeMood {
    crate::neon_sign::OfficeMood::of(crate::tally::StateCounts {
        active,
        waiting,
        idle,
        exiting: 0,
        total: active + waiting + idle,
    })
}

const ROOM_LIT: bool = false;
const ROOM_DIMMED: bool = true;
/// A live painter's frame tick — well under the shortest stutter flash.
const FRAME: Duration = Duration::from_millis(crate::anim::PAINT_FRAME_MS);

#[test]
fn neon_first_tick_snaps_to_the_mood() {
    for (mood, room, want) in [
        (neon_mood(2, 1, 0), ROOM_LIT, NeonLevels::ALERT),
        (neon_mood(2, 0, 3), ROOM_LIT, NeonLevels::BUSY),
        (neon_mood(0, 0, 3), ROOM_LIT, NeonLevels::CALM),
        (neon_mood(0, 0, 0), ROOM_DIMMED, NeonLevels::EMPTY),
        (neon_mood(0, 0, 0), ROOM_LIT, NeonLevels::CALM),
    ] {
        assert_eq!(
            NeonState::new().tick(mood, room, Motion::Full.timing(t0())),
            want,
            "{mood:?}"
        );
    }
}

/// A room that once dimmed and was repopulated keeps its sign lit when the last
/// agent walks out: the sign reads the room's VERDICT, which the debounce holds.
#[test]
fn neon_holds_through_a_walkout_in_a_room_that_once_dimmed() {
    let mut dim = VacancyDim::new();
    let mut neon = NeonState::new();
    let mut now = t0();
    let mut run = |empty: bool, mood: crate::neon_sign::OfficeMood, ms: u64| {
        let mut last = NeonLevels::CALM;
        for _ in 0..ms / FRAME.as_millis() as u64 {
            now += FRAME;
            dim.tick(empty, now);
            last = neon.tick(mood, dim.dimmed(), Motion::Full.timing(now));
        }
        (last, dim.level())
    };
    let (starved, _) = run(true, neon_mood(0, 0, 0), VacancyDim::EMPTY_DEBOUNCE_MS * 3);
    assert_eq!(starved, NeonLevels::EMPTY);
    let (_, level) = run(false, neon_mood(2, 0, 0), 60_000);
    assert_eq!(level, 1.0, "the ease lands on full light");
    // The last agent walks out: the tally is Empty, the room is lit and populated.
    let (walkout, _) = run(false, neon_mood(0, 0, 0), u64::from(NeonState::FADE_MS) * 2);
    assert_eq!(walkout, NeonLevels::CALM, "a lit room keeps its sign");
}

/// The `--empty` still: one tick after the snap must still judge the floor empty.
#[test]
fn light_snap_to_empty_survives_its_first_tick_and_reads_dimmed() {
    let mut dim = VacancyDim::new();
    dim.snap_to_empty();
    assert!(dim.dimmed());
    let level = dim.tick(true, t0());
    assert_eq!(level, VacancyDim::MIN_LEVEL);
    assert!(dim.dimmed(), "the debounce was back-dated, not re-armed");
    dim.tick(false, t0() + FRAME);
    assert!(!dim.dimmed(), "and a populated floor clears it");
}

#[test]
fn light_is_dimmed_exactly_once_the_debounce_runs_out() {
    let mut dim = VacancyDim::new();
    let debounce = Duration::from_millis(VacancyDim::EMPTY_DEBOUNCE_MS);
    dim.tick(true, t0());
    dim.tick(true, t0() + debounce - Duration::from_millis(1));
    assert!(!dim.dimmed());
    dim.tick(true, t0() + debounce);
    assert!(dim.dimmed());
}

#[test]
fn neon_eases_into_a_new_mood_and_lands_on_it() {
    let mut neon = NeonState::new();
    let fade = Duration::from_millis(u64::from(NeonState::FADE_MS));
    let alert = neon_mood(2, 1, 0);
    neon.tick(neon_mood(2, 0, 0), ROOM_LIT, Motion::Full.timing(t0()));
    let changed = t0() + FRAME;
    let first = neon.tick(alert, ROOM_LIT, Motion::Full.timing(changed));
    assert_eq!(
        first,
        NeonLevels::BUSY,
        "the change frame still shows the old mood"
    );
    let mid = neon.tick(alert, ROOM_LIT, Motion::Full.timing(changed + fade / 2));
    assert!(
        mid.alert > NeonLevels::BUSY.alert && mid.alert < NeonLevels::ALERT.alert,
        "mid-fade alert is between the moods: {mid:?}"
    );
    // Not `fade - 1ms`: an ease-out's last millisecond rounds to the target in f32.
    let late = neon.tick(alert, ROOM_LIT, Motion::Full.timing(changed + fade * 3 / 4));
    assert_ne!(
        late,
        NeonLevels::ALERT,
        "still crossing over late in the fade"
    );
    assert_eq!(
        neon.tick(alert, ROOM_LIT, Motion::Full.timing(changed + fade)),
        NeonLevels::ALERT
    );
}

/// A sign nobody has drawn for longer than a fade has no light to ease from — the
/// wasm still's warm-up step, a floor switched back to.
#[test]
fn neon_snaps_when_its_last_light_is_older_than_a_fade() {
    let fade = Duration::from_millis(u64::from(NeonState::FADE_MS));
    let (calm, busy) = (neon_mood(0, 0, 1), neon_mood(3, 0, 0));
    let mut fresh = NeonState::new();
    fresh.tick(calm, ROOM_LIT, Motion::Full.timing(t0()));
    assert_eq!(
        fresh.tick(busy, ROOM_LIT, Motion::Full.timing(t0() + fade)),
        NeonLevels::CALM,
        "fades"
    );
    let mut stale = NeonState::new();
    stale.tick(calm, ROOM_LIT, Motion::Full.timing(t0()));
    let later = t0() + fade + Duration::from_millis(1);
    assert_eq!(
        stale.tick(busy, ROOM_LIT, Motion::Full.timing(later)),
        NeonLevels::BUSY,
        "snaps"
    );
}

#[test]
fn neon_ignores_a_count_change_within_a_mood() {
    let mut neon = NeonState::new();
    neon.tick(neon_mood(0, 2, 0), ROOM_LIT, Motion::Full.timing(t0()));
    assert_eq!(
        neon.tick(
            neon_mood(0, 3, 0),
            ROOM_LIT,
            Motion::Full.timing(t0() + FRAME)
        ),
        NeonLevels::ALERT
    );
}

#[test]
fn neon_reversing_mid_fade_starts_from_the_current_light() {
    let mut neon = NeonState::new();
    neon.tick(neon_mood(0, 0, 3), ROOM_LIT, Motion::Full.timing(t0()));
    let half = Duration::from_millis(u64::from(NeonState::FADE_MS) / 2);
    neon.tick(neon_mood(0, 1, 3), ROOM_LIT, Motion::Full.timing(t0()));
    let mid = neon.tick(
        neon_mood(0, 1, 3),
        ROOM_LIT,
        Motion::Full.timing(t0() + half),
    );
    let reversed = neon.tick(
        neon_mood(0, 0, 3),
        ROOM_LIT,
        Motion::Full.timing(t0() + half),
    );
    assert_eq!(
        reversed, mid,
        "the reversal frame holds the light it interrupted"
    );
}

#[test]
fn neon_holds_on_a_backward_clock() {
    let mut neon = NeonState::new();
    neon.tick(
        neon_mood(2, 0, 0),
        ROOM_LIT,
        Motion::Full.timing(t0() + Duration::from_secs(5)),
    );
    neon.tick(
        neon_mood(0, 0, 3),
        ROOM_LIT,
        Motion::Full.timing(t0() + Duration::from_secs(5)),
    );
    let earlier = t0() + Duration::from_secs(1);
    assert_eq!(
        neon.tick(neon_mood(0, 0, 3), ROOM_LIT, Motion::Full.timing(earlier)),
        NeonLevels::BUSY
    );
}

/// The instant `offset` ms into the stutter cycle that contains `t0()`.
fn in_stutter_cycle(offset: u64) -> SystemTime {
    let base = crate::anim::epoch_ms(t0());
    SystemTime::UNIX_EPOCH + Duration::from_millis(base - base % NeonState::STUTTER_MS + offset)
}

/// A starved sign ticked every `step` across one whole stutter cycle.
fn starved_cycle(step: Duration) -> Vec<(u64, NeonLevels)> {
    let mut neon = NeonState::new();
    let empty = neon_mood(0, 0, 0);
    (0..NeonState::STUTTER_MS)
        .step_by(step.as_millis() as usize)
        .map(|ms| {
            (
                ms,
                neon.tick(
                    empty,
                    ROOM_DIMMED,
                    Motion::Full.timing(in_stutter_cycle(ms)),
                ),
            )
        })
        .collect()
}

#[test]
fn neon_a_starved_tube_flashes_inside_its_windows_and_only_there() {
    for (ms, levels) in starved_cycle(Duration::from_millis(10)).into_iter().skip(1) {
        let in_window = NeonState::STUTTER_FLASHES_MS
            .iter()
            .any(|&(start, end)| (start..end).contains(&ms));
        let want = if in_window {
            NeonLevels::FLASH
        } else {
            NeonLevels::EMPTY
        };
        assert_eq!(levels, want, "{ms}ms");
    }
}

/// Calm plays every loop slower, the stutter too: an empty, dimmed sign
/// repainted at Calm's cadence flashes within one stutter cycle of its loop
/// time.
#[test]
fn neon_a_starved_tube_stutters_at_the_calm_pace() {
    use crate::anim::{CALM_TICK_MS, FULL_TICK_MS};
    let mut neon = NeonState::new();
    let repaints = NeonState::STUTTER_MS / FULL_TICK_MS;
    let flashed = (0..=repaints)
        .map(|n| {
            let at = in_stutter_cycle(0) + Duration::from_millis(n * CALM_TICK_MS);
            neon.tick(neon_mood(0, 0, 0), ROOM_DIMMED, Motion::Calm.timing(at))
        })
        .any(|levels| levels == NeonLevels::FLASH);
    assert!(flashed, "the stutter never played at Calm");
}

/// On every moving tier, ticked at a live painter's [`FRAME`], a starved tube
/// catches on exactly the frames whose loop time lies in a window, and each
/// catch and each dark between, timed in loop time from its first frame to
/// the next's, lasts at least the photosensitive floor, across the cycle's
/// wrap too.
#[test]
fn neon_a_starved_tube_holds_each_flash_and_dark_the_floor() {
    const CYCLES: u64 = 2;
    for motion in Motion::ALL {
        let Some(pace) = motion.pace() else {
            continue;
        };
        let mut neon = NeonState::new();
        let mut runs: Vec<(bool, u64)> = Vec::new();
        let frames = CYCLES * NeonState::STUTTER_MS * pace / FRAME.as_millis() as u64;
        for n in 0..frames {
            let timing = motion.timing(in_stutter_cycle(0) + FRAME * n as u32);
            let lit = neon.tick(neon_mood(0, 0, 0), ROOM_DIMMED, timing) == NeonLevels::FLASH;
            let loop_ms = timing.beat.ms();
            let in_window = NeonState::STUTTER_FLASHES_MS
                .iter()
                .any(|&(start, end)| (start..end).contains(&(loop_ms % NeonState::STUTTER_MS)));
            // The first frame has no step to be drawn across.
            if n > 0 {
                assert_eq!(lit, in_window, "{motion:?} at {loop_ms} ms of loop time");
            }
            if runs.last().is_none_or(|&(was, _)| was != lit) {
                runs.push((lit, loop_ms));
            }
        }
        assert!(runs.iter().any(|&(lit, _)| lit), "{motion:?} never flashed");
        // The first run opens with the frames, not with a catch or a dark.
        for pair in runs[1..].windows(2) {
            let ms = pair[1].1 - pair[0].1;
            assert!(
                ms >= crate::anim::PHOTOSENSITIVE_PHASE_MIN_MS,
                "{motion:?}: {runs:?}"
            );
        }
    }
}

/// A starved tube records the catch it draws, from the tick that draws it to
/// the one that ends it.
#[test]
fn neon_records_the_catch_it_draws() {
    let mut neon = NeonState::new();
    let mut tick = |ms| {
        let at = in_stutter_cycle(ms);
        let levels = neon.tick(neon_mood(0, 0, 0), ROOM_DIMMED, Motion::Full.timing(at));
        (levels == NeonLevels::FLASH, neon.stutters())
    };
    let (start, end) = NeonState::STUTTER_FLASHES_MS[0];
    assert_eq!(tick(start - crate::anim::FULL_TICK_MS), (false, false));
    assert_eq!(tick(start), (true, true));
    assert_eq!(tick(end), (false, false));
}

/// On every tier, ticked at a live painter's [`FRAME`], a starved tube
/// flashes at most [`PHOTOSENSITIVE_FLASHES_PER_SECOND`] times in any second
/// of wall time.
///
/// [`PHOTOSENSITIVE_FLASHES_PER_SECOND`]: crate::anim::PHOTOSENSITIVE_FLASHES_PER_SECOND
#[test]
fn neon_a_starved_tube_flashes_at_most_three_times_a_second() {
    use crate::anim::{PHOTOSENSITIVE_FLASHES_PER_SECOND, most_flashes_in_a_second};
    const CYCLES: u64 = 2;
    let frame_ms = FRAME.as_millis() as u64;
    for motion in Motion::ALL {
        let mut neon = NeonState::new();
        let span = CYCLES * NeonState::STUTTER_MS * motion.pace().unwrap_or(1);
        let samples = (0..span / frame_ms).map(|n| {
            let timing = motion.timing(in_stutter_cycle(0) + FRAME * n as u32);
            let lit = neon.tick(neon_mood(0, 0, 0), ROOM_DIMMED, timing) == NeonLevels::FLASH;
            (n * frame_ms, if lit { 1.0 } else { 0.0 })
        });
        let most = most_flashes_in_a_second(samples);
        assert!(
            most <= PHOTOSENSITIVE_FLASHES_PER_SECOND,
            "{motion:?}: {most}"
        );
        assert!(
            motion == Motion::Still || most > 0,
            "{motion:?} never flashed"
        );
    }
}

/// At rest a starved tube holds steady: no flash, for the photosensitive.
#[test]
fn neon_a_starved_tube_never_flashes_at_rest() {
    let mut neon = NeonState::new();
    for ms in (0..NeonState::STUTTER_MS).step_by(10) {
        let timing = Motion::Still.timing(in_stutter_cycle(ms));
        let levels = neon.tick(neon_mood(0, 0, 0), ROOM_DIMMED, timing);
        assert_eq!(levels, NeonLevels::EMPTY, "{ms}ms");
    }
}

/// A painter stepping further than a flash can't draw it: a still (one tick)
/// gets the steady tube, never a held flash.
#[test]
fn neon_a_painter_slower_than_a_flash_never_shows_one() {
    let shortest = Duration::from_millis(NeonState::shortest_flash_ms());
    let slower = shortest + Duration::from_millis(crate::anim::FULL_TICK_MS);
    for (ms, levels) in starved_cycle(slower) {
        assert_eq!(levels, NeonLevels::EMPTY, "{ms}ms at a {slower:?} tick");
    }
    assert!(
        starved_cycle(shortest)
            .iter()
            .any(|(_, levels)| *levels == NeonLevels::FLASH),
        "a painter stepping a flash at a time draws each"
    );
    let in_a_flash = in_stutter_cycle(NeonState::STUTTER_FLASHES_MS[0].0);
    assert_eq!(
        NeonState::new().tick(
            neon_mood(0, 0, 0),
            ROOM_DIMMED,
            Motion::Full.timing(in_a_flash)
        ),
        NeonLevels::EMPTY,
        "a still's single tick"
    );
}

#[test]
fn neon_never_flashes_while_lit_or_while_still_coasting_down() {
    let flash_at = in_stutter_cycle(NeonState::STUTTER_FLASHES_MS[0].0);
    let mut lit = NeonState::new();
    lit.tick(
        neon_mood(0, 0, 3),
        ROOM_LIT,
        Motion::Full.timing(flash_at - FRAME),
    );
    assert_eq!(
        lit.tick(neon_mood(0, 0, 3), ROOM_LIT, Motion::Full.timing(flash_at)),
        NeonLevels::CALM
    );
    let mut coasting = NeonState::new();
    let mut now = flash_at - Duration::from_millis(u64::from(NeonState::FADE_MS) / 2);
    coasting.tick(
        neon_mood(2, 0, 0),
        ROOM_LIT,
        Motion::Full.timing(now - FRAME),
    );
    let mut last = coasting.tick(neon_mood(0, 0, 0), ROOM_DIMMED, Motion::Full.timing(now));
    while now < flash_at {
        now += Duration::from_millis(10);
        last = coasting.tick(neon_mood(0, 0, 0), ROOM_DIMMED, Motion::Full.timing(now));
    }
    assert_ne!(last, NeonLevels::FLASH);
    assert!(last.power > NeonLevels::EMPTY.power, "{last:?}");
}

/// The classic looks out from its own floor.
#[test]
fn the_classic_sees_the_skyline_from_its_floors_altitude() {
    let pack = Arc::new(crate::pack::test_office());
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = make_scene(1, 8);
    let render = |floor_meta: FloorMeta| {
        let mut session = FloorSession::new(Arc::clone(&pack));
        classic_frame(
            &mut session,
            &pack,
            &scene,
            now,
            floor_meta,
            Size { w: 192, h: 160 },
        )
        .expect("layout");
        session
    };
    let (ground_session, top_session) = (
        render(FloorMeta::ground()),
        render(FloorMeta {
            altitude: 1.0,
            ..FloorMeta::ground()
        }),
    );
    let ground = ground_session.picture().expect("classic buffer").pixels;
    let top = top_session.picture().expect("classic buffer").pixels;
    assert!(
        (0..ground.height())
            .flat_map(|y| (0..ground.width()).map(move |x| (x, y)))
            .any(|(x, y)| ground.get(x, y) != top.get(x, y)),
        "the top floor's windows show the ground floor's skyline"
    );
}

/// An office with every ambient loop running at `t0`, long settled: a typist,
/// a burning typist, a waiter and a wandering gateway mascot; at rest also
/// idle sleepers, whose wander leaves the sim's walks for real time.
fn ambient_office(t0: SystemTime, resting: bool) -> SceneState {
    use pixtuoid_core::source::daemon::{DaemonInstanceKey, DaemonPresenceUpdate, apply_presence};
    use pixtuoid_core::state::{DaemonInstanceId, EffortObservation, ToolKind};
    let typing = || ActivityState::Active {
        tool_use_id: None,
        detail: None,
        kind: ToolKind::Edit,
    };
    let settled = t0 - Duration::from_secs(3_600);
    let n = if resting { 5 } else { 3 };
    let mut scene = make_scene(n, 8);
    for (i, slot) in scene.agents.values_mut().enumerate() {
        slot.created_at = settled;
        slot.state_started_at = settled;
        slot.last_event_at = settled;
        slot.state = match i {
            0 | 1 => typing(),
            2 => ActivityState::Waiting {
                reason: Arc::from("ok?"),
            },
            _ => ActivityState::Idle,
        };
        if i == 1 {
            slot.model = Some(Arc::from("claude-fable"));
            slot.effort = Some(EffortObservation::new(Arc::from("max"), t0));
        }
    }
    let key = DaemonInstanceKey::new(
        pixtuoid_core::source::openclaw::SOURCE_NAME,
        DaemonInstanceId::new("18789".to_string()).expect("id"),
    );
    apply_presence(
        &mut scene,
        &key,
        DaemonPresenceUpdate::GatewayUp { pid: Some(7) },
        settled,
    );
    scene
}

/// The painters' office extent and the cutaway's scale over it.
const PAINTED: crate::layout::Size = crate::layout::Size { w: 192, h: 80 };

fn looks() -> [crate::look::Look; 2] {
    let scale = crate::render_scale::RenderScale::new(4).expect("nonzero");
    [
        crate::look::Look::Classic,
        crate::look::Look::Cutaway {
            scale,
            text: crate::look::WorldText::Baked,
        },
    ]
}

/// `session`'s pixels for `scene` on `floor` at `now`, in `look`.
fn paint(
    (session, pack): (&mut FloorSession, &Arc<OfficeArt>),
    look: crate::look::Look,
    scene: &SceneState,
    floor: FloorMeta,
    pet: Option<&Pet>,
    now: SystemTime,
) -> Vec<Rgb> {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let inputs = crate::look::RenderInputs {
        world: FloorInputs {
            scene,
            pack,
            now,
            floor,
            pets: PetInputs { pet, petting: None },
        },
        theme,
        size: PAINTED,
        place: crate::look::Place {
            gateway: crate::tally::office_gateway(scene),
            floor: None,
        },
        debug_walkable: false,
        dim: crate::look::Dim::NONE,
    };
    session.render(look, inputs);
    session
        .picture()
        .expect("lays out")
        .pixels
        .as_slice()
        .to_vec()
}

/// Both looks' pixels for `scene` on `floor` at `now`, each from fresh
/// stores so only the instant differs.
fn both_painters(
    scene: &SceneState,
    floor: FloorMeta,
    pet: Option<&Pet>,
    now: SystemTime,
) -> (Vec<Rgb>, Vec<Rgb>) {
    let pack = Arc::new(crate::pack::test_office());
    let [classic, cutaway_px] = looks().map(|look| {
        let mut session = FloorSession::new(Arc::clone(&pack));
        paint((&mut session, &pack), look, scene, floor, pet, now)
    });
    (classic, cutaway_px)
}

/// Every ambient loop either painter draws reads its floor's beat: two
/// instants on one beat paint one frame, and at rest every instant does — no
/// loop, wander or flash moves.
#[test]
fn both_painters_paint_one_frame_per_beat() {
    use crate::anim::{CALM_TICK_MS, FULL_TICK_MS};
    use crate::sky::{Weather, WeatherPolicy};
    let cat = Pet {
        kind: crate::pet::PetKind::Cat,
        name: "cat".into(),
    };
    for (hour, weather) in [(23, Weather::Storm), (12, Weather::Clear)] {
        // On every tier's tick: an hour is a whole number of beats.
        let t0 = crate::localclock::at_hour(hour) + Duration::from_secs(5);
        for (motion, later_ms) in [
            (Motion::Full, FULL_TICK_MS - 1),
            (Motion::Calm, CALM_TICK_MS - 1),
            (Motion::Still, 3 * CALM_TICK_MS),
        ] {
            let resting = motion == Motion::Still;
            let scene = ambient_office(t0, resting);
            let floor = FloorMeta::ground()
                .with_weather(WeatherPolicy::Forced(weather))
                .with_motion(motion);
            let pet = Some(&cat);
            let (classic, cutaway_px) = both_painters(&scene, floor, pet, t0);
            let later = both_painters(&scene, floor, pet, t0 + Duration::from_millis(later_ms));
            assert!(
                classic == later.0,
                "{motion:?} {weather:?}: the classic moved"
            );
            assert!(
                cutaway_px == later.1,
                "{motion:?} {weather:?}: the cutaway moved"
            );
        }
    }
}

/// What lets a painter sleep to the next beat: through arrivals, wanders, a
/// storm, the calm tier and a floor emptying, a frame after which the floor
/// reports nothing off the beat is the frame painted until the beat turns.
#[test]
fn a_floor_still_off_the_beat_paints_one_frame_until_the_beat_turns() {
    use crate::sky::{Weather, WeatherPolicy};
    let cat = Pet {
        kind: crate::pet::PetKind::Cat,
        name: "cat".into(),
    };
    let t0 = crate::localclock::at_hour(23) + Duration::from_secs(5);
    let lived_in = ambient_office(t0, false);
    // no gateway: its mascot's walks would outlast the arrival's
    let mut arriving = make_scene(3, 8);
    for slot in arriving.agents.values_mut() {
        (slot.created_at, slot.state_started_at, slot.last_event_at) = (t0, t0, t0);
    }
    let empty = make_scene(0, 8);
    let frame = Duration::from_millis(crate::anim::PAINT_FRAME_MS);
    let switch = Duration::from_secs(2);
    // a room of typists, then one turns to the user: the sign fades to its
    // alert while nobody walks
    let mut typing = lived_in.clone();
    for slot in typing.agents.values_mut() {
        slot.state = ActivityState::Active {
            tool_use_id: None,
            detail: None,
            kind: pixtuoid_core::state::ToolKind::Edit,
        };
    }
    let mut asking = typing.clone();
    if let Some(slot) = asking.agents.values_mut().next() {
        slot.state = ActivityState::Waiting {
            reason: Arc::from("ok?"),
        };
        slot.state_started_at = t0 + switch;
        slot.last_event_at = t0 + switch;
    }
    // past the switch, the debounce and the ease to a still room
    let emptied =
        switch + Duration::from_millis(VacancyDim::EMPTY_DEBOUNCE_MS + 6 * VacancyDim::FADE_TAU_MS);
    // (case, tier, weather, pet, the scene before and after `switch`, window);
    // the cat walks most of a window, so it rides only where nothing else must
    let cases = [
        (
            "arrival",
            Motion::Full,
            Weather::Storm,
            None,
            &arriving,
            &arriving,
            Duration::from_secs(8),
        ),
        (
            "calm",
            Motion::Calm,
            Weather::Clear,
            Some(&cat),
            &lived_in,
            &lived_in,
            Duration::from_secs(12),
        ),
        (
            "emptying",
            Motion::Full,
            Weather::Clear,
            None,
            &lived_in,
            &empty,
            emptied,
        ),
        (
            "asking",
            Motion::Full,
            Weather::Clear,
            None,
            &typing,
            &asking,
            Duration::from_secs(6),
        ),
    ];
    // the beat is the cutaway's at every scale, so the cheapest checks it
    let looks = [
        crate::look::Look::Classic,
        crate::look::Look::Cutaway {
            scale: crate::render_scale::RenderScale::new(1).expect("nonzero"),
            text: crate::look::WorldText::Baked,
        },
    ];
    let pack = Arc::new(crate::pack::test_office());
    for (case, motion, weather, pet, before, after, window) in cases {
        let floor = FloorMeta::ground()
            .with_weather(WeatherPolicy::Forced(weather))
            .with_motion(motion);
        for look in looks {
            let mut session = FloorSession::new(Arc::clone(&pack));
            let mut held: Option<(u64, Vec<Rgb>)> = None;
            let (mut slept, mut ticks) = (0, 0);
            while ticks * frame < window {
                let now = t0 + ticks * frame;
                let scene = if ticks * frame < switch {
                    before
                } else {
                    after
                };
                let px = paint((&mut session, &pack), look, scene, floor, pet, now);
                let beat = motion.timing(now).beat.ms();
                if let Some((held_beat, held_px)) = &held
                    && *held_beat == beat
                {
                    assert!(
                        *held_px == px,
                        "{case} {look:?}: the frame moved off the beat at {} ms",
                        (ticks * frame).as_millis()
                    );
                }
                held = (!session.moves_off_beat()).then_some((beat, px));
                slept += usize::from(held.is_some());
                ticks += 1;
            }
            assert!(slept > 0, "{case} {look:?}: never still off the beat");
        }
    }
}

/// The census above has teeth: the same office a beat on paints anew.
#[test]
fn a_full_beat_on_moves_both_painters() {
    use crate::anim::FULL_TICK_MS;
    let t0 = crate::localclock::at_hour(23) + Duration::from_secs(5);
    let scene = ambient_office(t0, false);
    let floor = FloorMeta::ground().with_motion(Motion::Full);
    let (classic, cutaway_px) = both_painters(&scene, floor, None, t0);
    let later = both_painters(
        &scene,
        floor,
        None,
        t0 + Duration::from_millis(FULL_TICK_MS),
    );
    assert!(classic != later.0, "the classic held still");
    assert!(cutaway_px != later.1, "the cutaway held still");
}

/// The loop clock is closed-form, never accumulated: whatever renderer paints
/// it, one instant on one tier is one frame.
#[test]
fn a_frame_is_a_function_of_its_instant_and_tier() {
    let t = crate::localclock::at_hour(23) + Duration::from_millis(5_321);
    let scene = ambient_office(t, false);
    for motion in Motion::ALL {
        let floor = FloorMeta::ground().with_motion(motion);
        let (a, b) = (
            both_painters(&scene, floor, None, t),
            both_painters(&scene, floor, None, t),
        );
        assert!(a == b, "{motion:?}");
    }
}

/// A slide never begins during one; it lands on its destination when it
/// finishes, is dropped when its floor goes, and a cancel lands it at once.
#[test]
fn floor_nav_slides_lands_and_clamps() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let done = t0 + Duration::from_secs(2);
    let mut nav = FloorNav::default();
    assert!(!nav.navigate(0, t0), "no floor slides to itself");
    assert!(nav.navigate(1, t0));
    assert!(!nav.navigate(2, t0), "no slide begins during one");
    nav.settle(3, t0);
    assert_eq!(
        (nav.current(), nav.transition().is_some()),
        (0, true),
        "a slide shows its floor until it lands"
    );
    nav.settle(3, done);
    assert_eq!((nav.current(), nav.transition().is_none()), (1, true));
    assert!(nav.navigate(2, done));
    nav.settle(2, done);
    assert_eq!(
        (nav.current(), nav.transition().is_none()),
        (1, true),
        "a slide to a floor gone is dropped"
    );
    assert!(nav.navigate(0, done));
    nav.cancel(3);
    assert_eq!((nav.current(), nav.transition().is_none()), (0, true));
    assert!(nav.navigate(2, done));
    nav.cancel(3);
    nav.settle(1, done);
    assert_eq!(nav.current(), 0, "the floor showing stays in the building");
}

/// A slide's footer speaks for its destination from the first frame, so its
/// count matches the breadcrumb it names.
#[test]
fn a_slides_footer_speaks_for_its_destination() {
    let pack = Arc::new(crate::pack::test_office());
    let scene = make_scene(3, 2);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut office = OfficeSession::new(pack);
    office.prepare(&scene, t0);
    assert_eq!(office.footer_floor(&scene).map(|f| f.current), Some(1));
    assert!(office.navigate(1, t0));
    assert_eq!(office.footer_floor(&scene).map(|f| f.current), Some(2));
    assert_eq!(
        office.footer_scene(&scene).agents.len(),
        project_floor_scene(&scene, 1).agents.len()
    );
}

/// Every painter's floor reads the one rule for its pet: a petting plays on
/// its own floor alone, and only while it lasts.
#[test]
fn a_petting_plays_only_on_its_own_floor() {
    let pack = Arc::new(crate::pack::test_office());
    let scene = make_scene(3, 2);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut office = OfficeSession::new(Arc::clone(&pack));
    office.prepare(&scene, t0);
    let petting = crate::pet::PetState {
        petted_at: t0,
        kind: crate::pet::PetKind::Cat,
        floor_idx: 0,
    };
    let pets = [crate::pet::Pet::defaulted(crate::pet::PetKind::Cat)];
    let world = |now| FloorInputs {
        scene: &scene,
        pack: &pack,
        now,
        floor: FloorMeta::ground(),
        pets: PetInputs {
            pet: None,
            petting: Some(&petting),
        },
    };
    let petted = |floor, now| {
        office
            .floor_world(world(now), &scene, floor, &pets)
            .pets
            .petting
            .is_some()
    };
    assert!(petted(0, t0), "the petted floor");
    assert!(!petted(1, t0), "another floor");
    assert!(!petted(0, t0 + Duration::from_secs(3600)), "a petting over");
}

/// A lift goes to the floor showing and nothing during a slide; its carry and
/// drop go to the floor it lifted on, whatever shows since, and nothing after.
#[test]
fn a_grip_stays_on_the_floor_it_lifted_on() {
    use crate::interact::{Figure, Gesture, GripFloor};
    use crate::layout::Point;
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let at = Point { x: 4, y: 4 };
    let lift = Gesture::Lift {
        figure: Figure::Pet(crate::pet::PetKind::Cat),
        at,
    };
    let mut nav = FloorNav::default();
    let mut gripped = GripFloor::default();
    assert_eq!(gripped.of(&lift, &nav), Some(0));
    assert!(nav.navigate(1, t0));
    assert_eq!(gripped.of(&Gesture::Carry(at), &nav), Some(0));
    nav.settle(2, t0 + Duration::from_secs(2));
    assert_eq!(nav.current(), 1);
    assert_eq!(gripped.of(&Gesture::Drop(at), &nav), Some(0));
    assert_eq!(
        gripped.of(&Gesture::Carry(at), &nav),
        None,
        "a drop ends the grip"
    );
    assert!(nav.navigate(0, t0));
    assert_eq!(
        gripped.of(&lift, &nav),
        None,
        "a slide shows no floor to lift on"
    );
    assert_eq!(gripped.of(&Gesture::Drop(at), &nav), None);
}

/// An office shows the floor its navigation holds, of the floors its scene
/// fills, with that floor's breadcrumb; a slide composes both floors, hits
/// nothing, and lands on its destination.
#[test]
fn an_office_session_shows_each_floor_and_slides_between_them() {
    let pack = Arc::new(crate::pack::test_office());
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let scene = make_scene(3, 2);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut office = OfficeSession::new(Arc::clone(&pack));
    let render = |office: &mut OfficeSession, now| {
        office.render(
            crate::look::Look::Classic,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                size: Size { w: 160, h: 96 },
                place: crate::look::Place::default(),
                debug_walkable: false,
                dim: crate::look::Dim::NONE,
            },
            &[],
            theme.surface.bg_fallback,
        )
    };
    let _ = render(&mut office, t0);
    assert!(
        matches!(office.picture(), Some(crate::look::Picture::Floor(_))),
        "floor 1 lays out"
    );
    assert_eq!(office.n_floors(), 2);
    let bread = |office: &OfficeSession| office.footer_floor(&scene).map(|f| f.current);
    assert_eq!(bread(&office), Some(1));
    assert!(office.navigate(1, t0));
    let mid = t0 + Duration::from_millis(400);
    let _ = render(&mut office, mid);
    let slide = office.picture().expect("the slide is composed");
    assert!(
        matches!(slide, crate::look::Picture::Slide(_)),
        "a slide has no one layout"
    );
    assert!(slide.hits().is_none(), "a slide hits nothing");
    let _ = render(&mut office, t0 + Duration::from_secs(2));
    assert!(matches!(
        office.picture(),
        Some(crate::look::Picture::Floor(_))
    ));
    assert_eq!((office.nav().current(), bread(&office)), (1, Some(2)));
    let pets: Vec<crate::pet::Pet> = (0..8)
        .map(|i| crate::pet::Pet {
            kind: crate::pet::PetKind::Cat,
            name: format!("cat{i}"),
        })
        .collect();
    assert_eq!(
        office.showing_pet(&pets).map(|p| p.name.as_str()),
        crate::pet::select_pet_for_floor(FloorMeta::for_floor(1, 2).floor_seed, &pets)
            .map(|p| p.name.as_str()),
        "the floor showing's pet"
    );
}

/// A cutaway slide bakes both floors' text whoever sets it on a floor
/// showing, so the text moves with its floor: it starts on the leaving
/// floor's own frame, its board and badges included.
#[test]
fn a_cutaway_slide_bakes_its_floors_text() {
    use crate::look::{Look, WorldText};
    let pack = Arc::new(crate::pack::test_office());
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let scene = make_scene(3, 2);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    // The slide `after` its start, or the floor showing.
    let frame = |text, after: Option<u64>| {
        let mut office = OfficeSession::new(Arc::clone(&pack));
        let look = Look::Cutaway {
            scale: crate::render_scale::RenderScale::new(4).expect("nonzero"),
            text,
        };
        let render = |office: &mut OfficeSession, now| {
            office.render(
                look,
                crate::look::RenderInputs {
                    world: FloorInputs {
                        scene: &scene,
                        pack: &pack,
                        now,
                        floor: FloorMeta::ground(),
                        pets: PetInputs::default(),
                    },
                    theme,
                    size: Size { w: 160, h: 96 },
                    place: crate::look::Place::default(),
                    debug_walkable: false,
                    dim: crate::look::Dim::NONE,
                },
                &[],
                theme.surface.bg_fallback,
            )
        };
        let _ = render(&mut office, t0);
        if let Some(after) = after {
            assert!(office.navigate(1, t0));
            let _ = render(&mut office, t0 + Duration::from_millis(after));
        }
        office
            .picture()
            .expect("composed")
            .pixels()
            .as_slice()
            .to_vec()
    };
    let baked = frame(WorldText::Baked, None);
    assert_ne!(
        frame(WorldText::Host, None),
        baked,
        "a floor showing leaves its text to the host"
    );
    assert_eq!(
        frame(WorldText::Host, Some(0)),
        baked,
        "a slide starts on it"
    );
    assert_eq!(
        frame(WorldText::Host, Some(400)),
        frame(WorldText::Baked, Some(400)),
        "a slide bakes it"
    );
}

/// Under a host's dim, the gap a slide leaves between its floors is dimmed
/// as the floors are: no band of it shows at full brightness.
#[test]
fn a_slides_gap_dims_with_its_floors() {
    use crate::look::{Dim, Look};
    let pack = Arc::new(crate::pack::test_office());
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let scene = make_scene(3, 2);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let gap = pixtuoid_core::sprite::Rgb {
        r: u8::MAX,
        g: 0,
        b: u8::MAX,
    };
    let dim = Dim::from_brightness(0.4);
    let mut office = OfficeSession::new(Arc::clone(&pack));
    let render = |office: &mut OfficeSession, now| {
        office.render(
            Look::Classic,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                size: Size { w: 160, h: 96 },
                place: crate::look::Place::default(),
                debug_walkable: false,
                dim,
            },
            &[],
            gap,
        )
    };
    let _ = render(&mut office, t0);
    assert!(office.navigate(1, t0));
    let _ = render(&mut office, t0 + Duration::from_millis(400));
    let slide = office.picture().expect("composed").pixels();
    let rows = |rgb| {
        (0..slide.height())
            .filter(|&y| (0..slide.width()).all(|x| slide.get(x, y) == rgb))
            .count()
    };
    assert!(rows(dim.of(gap)) > 0, "the slide shows its gap");
    assert_eq!(rows(gap), 0, "a gap row at full brightness");
}

/// A frame that drew no office keeps nothing of the last: a pointer hits
/// nothing and the audio hears no one at the floor's waypoints.
#[test]
fn a_frame_without_the_office_forgets_the_last() {
    let pack = Arc::new(crate::pack::test_office());
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut scene = make_scene(1, 8);
    for slot in scene.agents.values_mut() {
        slot.created_at = now0;
        slot.state_started_at = now0;
        slot.last_event_at = now0;
    }
    let mut office = OfficeSession::new(Arc::clone(&pack));
    // A BUDGET for the wander to bring someone to a waypoint, not part of
    // the assertion.
    let occupied = (0..600u64).any(|step| {
        let _ = office.render(
            crate::look::Look::Classic,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now: now0 + Duration::from_secs(3 * step),
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                size: Size { w: 160, h: 96 },
                place: crate::look::Place::default(),
                debug_walkable: false,
                dim: crate::look::Dim::NONE,
            },
            &[],
            theme.surface.bg_fallback,
        );
        office.picture().is_some() && !office.heard_occupied().is_empty()
    });
    assert!(occupied, "someone reaches a waypoint the audio hears");
    let whole = crate::layout::Bounds {
        x: 0,
        y: 0,
        width: 160,
        height: 96,
    };
    assert!(
        office
            .picture()
            .and_then(|p| p.hits().map(|h| h.at(whole, None).is_some()))
            .unwrap_or_default(),
        "the frame drawn shows its agents"
    );
    let _ = office.drew_no_office(&scene, FloorMeta::ground(), now0);
    assert!(office.picture().is_none(), "a pointer hits nothing");
    assert!(office.heard_occupied().is_empty());
}

/// An office whose upper floor empties shows one floor: the count and the
/// breadcrumb follow the live scene, not the views kept.
#[test]
fn an_office_session_follows_its_floors_down() {
    let pack = Arc::new(crate::pack::test_office());
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut office = OfficeSession::new(Arc::clone(&pack));
    let render = |office: &mut OfficeSession, scene: &SceneState| {
        office.render(
            crate::look::Look::Classic,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene,
                    pack: &pack,
                    now: t0,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                size: Size { w: 160, h: 96 },
                place: crate::look::Place::default(),
                debug_walkable: false,
                dim: crate::look::Dim::NONE,
            },
            &[],
            theme.surface.bg_fallback,
        )
    };
    let two = make_scene(3, 2);
    let _ = render(&mut office, &two);
    assert_eq!(office.n_floors(), 2);
    let one = make_scene(2, 2);
    let _ = render(&mut office, &one);
    assert_eq!(office.n_floors(), 1);
    assert!(
        office.footer_floor(&one).is_none(),
        "one floor has no breadcrumb"
    );
}

/// A lifted agent hangs where the pointer is, in front of the room; set
/// down, it walks home from the floor near the drop and sits.
#[test]
fn a_lifted_agent_hangs_from_the_pointer_and_walks_home_when_set_down() {
    use crate::interact::{Figure, Gesture};
    use crate::layout::Point;
    use crate::pose::Pose;
    let pack = Arc::new(crate::pack::test_office());
    let scene = make_scene(1, 4);
    let id = *scene.agents.keys().next().expect("one agent");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut session = FloorSession::new(Arc::clone(&pack));
    let size = Size { w: 160, h: 96 };
    let step = |session: &mut FloorSession, now| {
        session
            .step(
                FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                size,
            )
            .expect("lays out")
    };
    step(&mut session, t0);
    let at = Point { x: 40, y: 70 };
    session.floor_mut().grip(&Gesture::Lift {
        figure: Figure::Agent(id),
        at,
    });
    let held = step(&mut session, t0 + Duration::from_millis(100));
    assert_eq!(held.frame.poses[&id], Some(Pose::Held { at }));
    assert_eq!(
        held.frame.characters.iter().map(|c| c.sort_row).max(),
        Some(u16::MAX),
        "drawn in front of the room"
    );
    let at = Point { x: 50, y: 72 };
    session.floor_mut().grip(&Gesture::Carry(at));
    session.floor_mut().grip(&Gesture::Drop(at));
    let dropped = step(&mut session, t0 + Duration::from_millis(200));
    let Some(Pose::Walking { segment, .. }) = dropped.frame.poses[&id] else {
        panic!("set down, it walks home: {:?}", dropped.frame.poses[&id]);
    };
    let from = segment.from();
    assert!(
        from.x.abs_diff(at.x) <= 8 && from.y.abs_diff(at.y) <= 8,
        "from the floor near the drop: {from:?}"
    );
    // Set down where no leg reaches, it lands home rather than off the floor.
    let lift_again = Gesture::Lift {
        figure: Figure::Agent(id),
        at,
    };
    let far = Point {
        x: u16::MAX,
        y: u16::MAX,
    };
    session.floor_mut().grip(&lift_again);
    session.floor_mut().grip(&Gesture::Drop(far));
    let landed = step(&mut session, t0 + Duration::from_millis(300));
    let desk = landed
        .layout
        .home_desk(scene.agents[&id].desk_index.single_floor_local())
        .expect("a desk");
    let home = crate::pose::desk_leg_endpoint(desk, &landed.layout).0;
    match landed.frame.poses[&id] {
        Some(Pose::Walking { segment, .. }) => {
            assert_eq!(segment.from(), home, "it walks from home");
        }
        Some(Pose::Held { .. }) | None => panic!("not landed: {:?}", landed.frame.poses[&id]),
        Some(_) => {}
    }
    let sat = (1..300)
        .map(|tenth| step(&mut session, t0 + Duration::from_millis(200 + 100 * tenth)))
        .find_map(|s| s.frame.poses[&id].filter(|p| !matches!(p, Pose::Walking { .. })));
    assert_eq!(sat, Some(Pose::SeatedIdle), "home, it sits");
}

/// A drop and another figure's lift that reach a floor between two of its
/// steps both land: the one set down leaves the hand, an agent to walk home
/// and sit, a pet to rest and roam on.
#[test]
fn a_drop_and_the_next_lift_between_two_steps_both_land() {
    use crate::interact::{Figure, Gesture};
    use crate::layout::Point;
    use crate::pose::Pose;
    let pack = Arc::new(crate::pack::test_office());
    let scene = make_scene(2, 4);
    let mut ids = scene.agents.keys().copied();
    let (a, b) = (
        ids.next().expect("two agents"),
        ids.next().expect("two agents"),
    );
    let cat = crate::pet::Pet {
        kind: crate::pet::PetKind::Cat,
        name: "Mochi".into(),
    };
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let step = |session: &mut FloorSession, ms: u64| {
        session
            .step(
                FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now: t0 + Duration::from_millis(ms),
                    floor: FloorMeta::ground(),
                    pets: PetInputs {
                        pet: Some(&cat),
                        petting: None,
                    },
                },
                Size { w: 160, h: 96 },
            )
            .expect("lays out")
            .frame
    };
    let at = Point { x: 40, y: 70 };
    for first in [Figure::Agent(a), Figure::Pet(cat.kind)] {
        let mut session = FloorSession::new(Arc::clone(&pack));
        step(&mut session, 0);
        session.floor_mut().grip(&Gesture::Lift {
            figure: first.clone(),
            at,
        });
        step(&mut session, 100);
        session.floor_mut().grip(&Gesture::Drop(at));
        session.floor_mut().grip(&Gesture::Lift {
            figure: Figure::Agent(b),
            at,
        });
        let set_down = step(&mut session, 200);
        session.floor_mut().grip(&Gesture::Drop(at));
        let later = (3..).map(|tenth| step(&mut session, 100 * tenth));
        match first {
            Figure::Agent(_) => assert!(
                later.take(300).any(|f| matches!(
                    f.poses[&a],
                    Some(Pose::SeatedIdle | Pose::SeatedThinking | Pose::SeatedTyping)
                )),
                "the agent set down walks home and sits"
            ),
            _ => {
                let rests = set_down.pet.expect("the cat is drawn").pos;
                let paints = crate::creatures::PET_LONGEST_REST_MS / 100 + 50;
                assert!(
                    later
                        .take(paints as usize)
                        .any(|f| f.pet.is_some_and(|p| p.pos != rests)),
                    "the cat set down rests, then roams on from {rests:?}"
                );
            }
        }
    }
}

/// A lifted agent stands at no waypoint, whatever its timeline says: in hand,
/// or walking home, it reserves no cell another's route steps around.
#[test]
fn a_lifted_agent_reserves_no_cell() {
    use crate::interact::{Figure, Gesture};
    use crate::layout::Point;
    use crate::pose::Pose;
    let pack = Arc::new(crate::pack::test_office());
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let size = Size { w: 160, h: 96 };
    let layout = crate::layout::SceneLayout::compute_with_seed(
        size.w,
        size.h,
        None,
        FloorMeta::ground().floor_seed,
    )
    .expect("lays out");
    let at_waypoint = |slot: &AgentSlot, now| {
        matches!(
            crate::pose::derive(slot, now, &layout),
            Some(Pose::AtWaypoint { .. })
        )
    };
    // Long enough at its waypoint to be lifted, set down and walk a step.
    let stays = Duration::from_millis(500);
    let mut scene = make_scene(8, 8);
    let (id, t) = scene
        .agents
        .values()
        .find_map(|a| {
            (1..3_000u64)
                .map(|tenth| t0 + Duration::from_millis(100 * tenth))
                .find(|&t| at_waypoint(a, t) && at_waypoint(a, t + stays))
                .map(|t| (a.agent_id, t))
        })
        .expect("a timeline stands someone at a waypoint");
    scene.agents.retain(|&a, _| a == id);
    let mut session = FloorSession::new(Arc::clone(&pack));
    let step = |session: &mut FloorSession, now| {
        session
            .step(
                FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                size,
            )
            .expect("lays out")
    };
    step(&mut session, t);
    assert!(
        !session.floor().ctx.overlay.is_empty(),
        "standing there, it reserves its cell"
    );
    let at = Point { x: 40, y: 70 };
    session.floor_mut().grip(&Gesture::Lift {
        figure: Figure::Agent(id),
        at,
    });
    step(&mut session, t + Duration::from_millis(100));
    assert!(session.floor().ctx.overlay.is_empty(), "in hand");
    session.floor_mut().grip(&Gesture::Drop(at));
    let home = step(&mut session, t + Duration::from_millis(200));
    assert!(
        matches!(home.frame.poses[&id], Some(Pose::Walking { .. })),
        "set down, it walks home"
    );
    assert!(session.floor().ctx.overlay.is_empty(), "walking home");
}

/// A pet or a mascot in hand is in front of the whole room, as an agent in
/// hand is: held over a sitter's head, it is what the pointer finds there.
#[test]
fn a_lifted_creature_is_in_front_of_the_room() {
    use crate::display::HoverTarget;
    use crate::hit::SceneHit;
    use crate::interact::{Figure, Gesture};
    use crate::layout::{Bounds, Point};
    let pack = Arc::new(crate::pack::test_office());
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let mut scene = make_scene(1, 4);
    let id = *scene.agents.keys().next().expect("one agent");
    // At work, so it stays in its chair.
    for slot in scene.agents.values_mut() {
        slot.state = ActivityState::Active {
            tool_use_id: None,
            detail: None,
            kind: pixtuoid_core::state::ToolKind::Edit,
        };
    }
    let lobster = crate::creatures::openclaw_key("18789");
    scene.insert_daemon(
        lobster.source(),
        lobster.instance().clone(),
        pixtuoid_core::state::DaemonPresence {
            liveness: pixtuoid_core::state::DaemonLiveness::UP,
            active_sessions: 0,
            last_seen: SystemTime::UNIX_EPOCH,
            entered_at: SystemTime::UNIX_EPOCH,
            in_flight_runs: Default::default(),
            current_pid: Some(1),
        },
    );
    let cat = crate::pet::Pet {
        kind: crate::pet::PetKind::Cat,
        name: "Mochi".into(),
    };
    let size = Size { w: 160, h: 96 };
    let render = |session: &mut FloorSession, look, now| {
        session.render(
            look,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs {
                        pet: Some(&cat),
                        petting: None,
                    },
                },
                theme,
                size,
                place: crate::look::Place::default(),
                debug_walkable: false,
                dim: crate::look::Dim::NONE,
            },
        );
        assert!(session.picture().is_some(), "lays out");
    };
    let cell = |at: Point| Bounds {
        x: at.x,
        y: at.y,
        width: 1,
        height: 1,
    };
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_060);
    let looks = [
        crate::look::Look::Classic,
        crate::look::Look::Cutaway {
            scale: crate::render_scale::RenderScale::new(4).expect("nonzero"),
            text: crate::look::WorldText::Baked,
        },
    ];
    for (look, figure) in looks.into_iter().flat_map(|look| {
        [Figure::Pet(cat.kind), Figure::Mascot(lobster.clone())].map(|figure| (look, figure))
    }) {
        let mut session = FloorSession::new(Arc::clone(&pack));
        render(&mut session, look, now);
        let head = (0..size.h)
            .flat_map(|y| (0..size.w).map(move |x| Point { x, y }))
            .find(|&p| {
                session.picture().is_some_and(|picture| {
                    matches!(
                        picture.hits.at(cell(p), None),
                        Some(SceneHit::Figure(HoverTarget::Agent(a))) if *a == id
                    )
                })
            })
            .expect("the sitter is drawn");
        session.floor_mut().grip(&Gesture::Lift {
            figure: figure.clone(),
            at: head,
        });
        // Every frame of the carry, so it never flickers behind.
        for paint in 1..=crate::anim::PAINT_FPS {
            session.floor_mut().grip(&Gesture::Carry(head));
            render(&mut session, look, now + FRAME * paint);
            let picture = session.picture().expect("drawn");
            let hit = picture.hits.at(cell(head), None);
            assert!(
                matches!(
                    (&figure, hit),
                    (Figure::Pet(_), Some(SceneHit::Figure(HoverTarget::Pet(_))))
                ) || matches!(
                    (&figure, hit),
                    (Figure::Mascot(k), Some(SceneHit::Figure(HoverTarget::Mascot(m)))) if k == m
                ),
                "{look:?} paint {paint}, {figure:?} in hand over {head:?}: {hit:?}"
            );
        }
    }
}

/// A creature not in hand sorts on its frame's south row, both painters'
/// order: the pet and the mascot alike.
#[test]
fn a_creature_not_in_hand_sorts_on_its_frames_south_row() {
    use crate::layout::{Pivot, sort_row_at};
    let pack = Arc::new(crate::pack::test_office());
    let mut scene = make_scene(1, 4);
    let lobster = crate::creatures::openclaw_key("18789");
    scene.insert_daemon(
        lobster.source(),
        lobster.instance().clone(),
        pixtuoid_core::state::DaemonPresence {
            liveness: pixtuoid_core::state::DaemonLiveness::UP,
            active_sessions: 0,
            last_seen: SystemTime::UNIX_EPOCH,
            entered_at: SystemTime::UNIX_EPOCH,
            in_flight_runs: Default::default(),
            current_pid: Some(1),
        },
    );
    let cat = crate::pet::Pet {
        kind: crate::pet::PetKind::Cat,
        name: "Mochi".into(),
    };
    let mut session = FloorSession::new(Arc::clone(&pack));
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (mut pets, mut mascots) = (0u32, 0u32);
    for paint in 0..crate::anim::PAINT_FPS * 60 {
        let frame = session
            .step(
                FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now: t0 + FRAME * paint,
                    floor: FloorMeta::ground(),
                    pets: PetInputs {
                        pet: Some(&cat),
                        petting: None,
                    },
                },
                Size { w: 160, h: 96 },
            )
            .expect("lays out")
            .frame;
        let row = |pos, anim, frame_idx| {
            let h = crate::sim::pack_frame_size(&pack, anim, frame_idx).h;
            sort_row_at(Pivot::Center, pos, h)
        };
        if let Some(p) = &frame.pet {
            assert_eq!(p.sort_row, row(p.pos, p.anim_name, p.frame_idx), "the pet");
            pets += 1;
        }
        for m in &frame.mascots {
            assert_eq!(
                m.sort_row,
                row(m.pos, m.anim_name, m.frame_idx),
                "the mascot"
            );
            mascots += 1;
        }
    }
    assert!(pets > 0 && mascots > 0, "both drawn: {pets}/{mascots}");
}

/// A lift names an agent a frame drew, and every agent a frame draws has a
/// walk for the lift to land on, from its first frame: walking in, seated,
/// or walking out.
#[test]
fn every_drawn_agent_has_a_walk_a_lift_lands_on() {
    let pack = Arc::new(crate::pack::test_office());
    let mut scene = make_scene(3, 4);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut slots = scene.agents.values_mut();
    let seated = slots.next().expect("three agents");
    seated.created_at = t0 - Duration::from_secs(120);
    let leaving = slots.next().expect("three agents");
    leaving.exiting_at = Some(t0);
    let mut session = FloorSession::new(Arc::clone(&pack));
    let stepped = session
        .step(
            FloorInputs {
                scene: &scene,
                pack: &pack,
                now: t0 + FRAME,
                floor: FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            Size { w: 160, h: 96 },
        )
        .expect("lays out");
    assert_eq!(stepped.frame.characters.len(), 3, "all three drawn");
    for c in &stepped.frame.characters {
        let id = stepped.frame.agents[c.agent_idx].agent_id;
        assert!(
            session.floor().ctx.walks.contains_key(&id),
            "{id:?} is drawn with no walk"
        );
    }
}

/// A pet lifted and set down within one paint lands at the drop, as an agent
/// does.
#[test]
fn a_pet_flicked_within_one_paint_lands_at_the_drop() {
    use crate::interact::{Figure, Gesture};
    use crate::layout::Point;
    let pack = Arc::new(crate::pack::test_office());
    let scene = make_scene(1, 4);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let cat = crate::pet::Pet {
        kind: crate::pet::PetKind::Cat,
        name: "Mochi".into(),
    };
    let mut session = FloorSession::new(Arc::clone(&pack));
    let pet_at = |session: &mut FloorSession, now| {
        session
            .step(
                FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs {
                        pet: Some(&cat),
                        petting: None,
                    },
                },
                Size { w: 160, h: 96 },
            )
            .expect("lays out")
            .frame
            .pet
            .expect("the cat is drawn")
            .pos
    };
    let before = pet_at(&mut session, t0);
    let at = Point { x: 130, y: 70 };
    let near = |p: Point| p.x.abs_diff(at.x) <= 8 && p.y.abs_diff(at.y) <= 8;
    assert!(!near(before), "setup: the cat starts away from {at:?}");
    session.floor_mut().grip(&Gesture::Lift {
        figure: Figure::Pet(cat.kind),
        at,
    });
    session.floor_mut().grip(&Gesture::Drop(at));
    let landed = pet_at(&mut session, t0 + Duration::from_millis(100));
    assert!(near(landed), "it lands at the drop {at:?}: {landed:?}");
}

/// A lifted pet hangs where the pointer is; set down, it rests on the floor
/// there.
#[test]
fn a_lifted_pet_rests_where_it_is_set_down() {
    use crate::interact::{Figure, Gesture};
    use crate::layout::Point;
    let pack = Arc::new(crate::pack::test_office());
    let scene = make_scene(1, 4);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let cat = crate::pet::Pet {
        kind: crate::pet::PetKind::Cat,
        name: "Mochi".into(),
    };
    let mut session = FloorSession::new(Arc::clone(&pack));
    let size = Size { w: 160, h: 96 };
    let pet_at = |session: &mut FloorSession, now| {
        let stepped = session
            .step(
                FloorInputs {
                    scene: &scene,
                    pack: &pack,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs {
                        pet: Some(&cat),
                        petting: None,
                    },
                },
                size,
            )
            .expect("lays out");
        stepped.frame.pet.expect("the cat is drawn").pos
    };
    let before = pet_at(&mut session, t0);
    let at = Point { x: 60, y: 60 };
    session.floor_mut().grip(&Gesture::Lift {
        figure: Figure::Pet(crate::pet::PetKind::Cat),
        at,
    });
    let held = pet_at(&mut session, t0 + Duration::from_millis(100));
    assert_ne!(held, before, "it left where it was");
    session.floor_mut().grip(&Gesture::Drop(at));
    let set_down = pet_at(&mut session, t0 + Duration::from_millis(200));
    let later = pet_at(&mut session, t0 + Duration::from_millis(1_200));
    assert_eq!(set_down, later, "it rests where it was set down");
    assert!(
        set_down.x.abs_diff(held.x) <= 8 && set_down.y.abs_diff(held.y) <= 8,
        "on the floor near the drop: {set_down:?} vs held {held:?}"
    );
}
