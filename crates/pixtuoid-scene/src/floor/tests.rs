use super::office::PAST_THE_TURN;
use super::*;
use crate::anim::Motion;
use crate::anim::REST_TICK;
use crate::character::test_support::make_slot;
use crate::layout::Viewport;
use pixtuoid_core::id::AgentId;
use pixtuoid_core::state::{ActivityState, ToolUse};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn frame_layout_memo_matches_fresh_compute_across_hits_resizes_and_none() {
    let mut ctx = FloorCtx::new();
    let fresh =
        crate::layout::SceneLayout::compute(crate::layout::Viewport::full(192, 156), None, 0)
            .unwrap();
    let a = ctx.frame_layout(Viewport::full(192, 156), 0).unwrap();
    let b = ctx.frame_layout(Viewport::full(192, 156), 0).unwrap();
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
    let resized = ctx.frame_layout(Viewport::full(120, 100), 0).unwrap();
    let fresh_resized =
        crate::layout::SceneLayout::compute(crate::layout::Viewport::full(120, 100), None, 0)
            .unwrap();
    assert_eq!(resized.walkable, fresh_resized.walkable);
    // A too-small buffer is None and must not poison the memo.
    assert!(ctx.frame_layout(Viewport::full(3, 3), 0).is_none());
    assert_eq!(
        ctx.frame_layout(Viewport::full(192, 156), 0)
            .unwrap()
            .walkable,
        fresh.walkable
    );
}

/// A new layout drops the router's cached paths, a height-only resize's too:
/// `route` revalidates a cached path against the overlay alone, never the
/// mask, so a stale one would walk through the new layout's walls.
#[test]
fn a_new_layout_drops_the_routers_cached_paths() {
    use crate::pathfind::Router;
    use crate::walkable::OccupancyOverlay;
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
    let l = ctx.frame_layout(Viewport::full(192, 156), 0).unwrap();
    cache_a_route(&mut ctx, &l);
    ctx.frame_layout(Viewport::full(192, 156), 0).unwrap();
    assert!(!ctx.router.is_empty(), "the same layout keeps its paths");
    ctx.frame_layout(Viewport::full(120, 100), 0).unwrap();
    assert!(ctx.router.is_empty(), "a new layout drops its paths");
    for h in 40..=240 {
        let Some(l) = ctx.frame_layout(Viewport::full(192, h), 0) else {
            continue;
        };
        cache_a_route(&mut ctx, &l);
        if ctx.frame_layout(Viewport::full(192, h + 1), 0).is_some() {
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
                in_flight_turns: Default::default(),
                current_pid: Some(1),
            },
        );
    }
    assert_eq!(
        project_floor_scene(&scene, 0).scene().daemons().count(),
        2,
        "floor 0 carries EVERY gateway mascot"
    );
    assert_eq!(
        project_floor_scene(&scene, 1).scene().daemons().count(),
        0,
        "floor 1+ must NOT (render-once invariant)"
    );
}

#[test]
fn every_floor_carries_the_days_count() {
    let mut scene = make_scene(8, 2);
    let day = chrono::NaiveDate::from_ymd_opt(2027, 1, 15).expect("a date");
    let today = pixtuoid_core::state::Today::new(day, 1_200_000, 38);
    scene.today = Some(today);
    for floor in 0..2 {
        assert_eq!(
            project_floor_scene(&scene, floor).scene().today,
            Some(today),
            "the sign upstairs reads it too, floor {floor}"
        );
    }
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
    assert!(past.scene().agents.is_empty());
    assert_eq!(past.scene().total_capacity(), 0);
}

#[test]
fn door_anim_excludes_arrived_entry_profiles() {
    use crate::walk::WalkState;
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let id = AgentId::from_transcript_path("/p/door.jsonl");
    let mut fctx = FloorCtx::new();
    let mut walk = WalkState::default();
    let profile = WalkProfile {
        duration_ms: 2000,
        pause_ms: 300,
        path_len_octile: 500,
        v_cruise: 0.36,
        accel: 6.5e-4,
    };
    let window = profile.duration_ms + profile.pause_ms;
    walk.entry = Some(crate::walk::WalkLeg {
        started_at: t0,
        profile,
        from: crate::layout::Point { x: 0, y: 0 },
    });
    fctx.walks.insert(id, walk);

    fctx.recompute_door_anim_max_ms(t0 + Duration::from_millis(window - 1));
    assert_eq!(
        fctx.door_anim_max_ms, window,
        "in-flight entry walk should drive the door cosmetic window"
    );

    // Past arrival, even though WalkState.entry is never cleared for this agent.
    fctx.recompute_door_anim_max_ms(t0 + Duration::from_millis(window));
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

fn make_scene(n: usize, max_desks: usize) -> SceneState {
    let mut s = SceneState::uniform(max_desks);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for i in 0..n {
        let id = AgentId::from_transcript_path(&format!("/p/{i}.jsonl"));
        let floor_idx = s.floor_of(GlobalDeskIndex(i));
        s.agents.insert(
            id,
            AgentSlot {
                source: Arc::from("cc"),
                session_id: Arc::from(format!("s{i}").as_str()),
                cwd: Arc::from(Path::new("/repo")),
                label: format!("a{i}").into(),
                state_started_at: now,
                created_at: now,
                arrived_at: Some(now),
                last_event_at: now,
                desk_index: GlobalDeskIndex(i),
                floor_idx,
                ..make_slot(id, ActivityState::Idle)
            },
        );
    }
    s
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
        let floor_scene = project_floor_scene(&scene, floor_idx);
        let projected = floor_scene.scene();
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
            source: Arc::from("cc"),
            cwd: Arc::from(Path::new("/repo")),
            label: "stale".into(),
            state_started_at: now,
            created_at: now,
            arrived_at: Some(now),
            last_event_at: now,
            desk_index: GlobalDeskIndex(5),
            floor_idx: 1,
            ..make_slot(id, ActivityState::Idle)
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
                source: Arc::from("cc"),
                session_id: Arc::from(format!("s{i}").as_str()),
                cwd: Arc::from(Path::new("/repo")),
                label: format!("a{i}").into(),
                state_started_at: now,
                created_at: now,
                arrived_at: Some(now),
                last_event_at: now,
                desk_index: GlobalDeskIndex(i),
                floor_idx,
                ..make_slot(id, ActivityState::Idle)
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

    let mid = start + Duration::from_millis(TRANSITION_DURATION_MS / 2);
    let t_mid = tr.t(mid);
    assert!(
        t_mid > 0.0 && t_mid < 1.0,
        "mid should be between 0 and 1, got {t_mid}"
    );

    let end = start + Duration::from_millis(TRANSITION_DURATION_MS);
    assert!((tr.t(end) - 1.0).abs() < f32::EPSILON);
    assert!(!tr.is_done(mid));
    assert!(tr.is_done(end));
}

#[test]
fn transition_t_clamps_past_duration() {
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let tr = FloorTransition::new(0, 1, start);

    let past = start + Duration::from_millis(TRANSITION_DURATION_MS + 1);
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
    for ms in (0..2 * VacancyDim::VACANCY_DEBOUNCE_MS).step_by(crate::anim::PAINT_FRAME_MS as usize)
    {
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
    let level = dim.tick(
        true,
        start + Duration::from_millis(VacancyDim::VACANCY_DEBOUNCE_MS - 1),
    );
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
    // the last full-light tick, so the fade below spans only the probe's own step
    dim.tick(
        true,
        start + Duration::from_millis(VacancyDim::VACANCY_DEBOUNCE_MS - 1),
    );
    // one time constant of fade past the debounce
    let level = dim.tick(
        true,
        start + Duration::from_millis(VacancyDim::VACANCY_DEBOUNCE_MS + VacancyDim::FADE_TAU_MS),
    );
    assert!(level < 0.95, "no fade started after debounce: {level}");
    assert!(level > VacancyDim::MIN_LEVEL, "overshot floor: {level}");
}

#[test]
fn light_converges_to_min_when_empty_long_enough() {
    let mut dim = VacancyDim::new();
    let start = t0();
    // Frames through the debounce and ten time constants, so the ease has landed.
    let landed = VacancyDim::VACANCY_DEBOUNCE_MS + 10 * VacancyDim::FADE_TAU_MS;
    for ms in (0..landed).step_by(crate::anim::PAINT_FRAME_MS as usize) {
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
    let landed = VacancyDim::VACANCY_DEBOUNCE_MS + 10 * VacancyDim::FADE_TAU_MS;
    for ms in (0..landed).step_by(crate::anim::PAINT_FRAME_MS as usize) {
        dim.tick(true, start + Duration::from_millis(ms));
    }
    assert!(dim.level() < 0.2);
    let later = start + Duration::from_millis(landed);
    for ms in (0..4 * VacancyDim::FADE_TAU_MS).step_by(crate::anim::PAINT_FRAME_MS as usize) {
        dim.tick(false, later + Duration::from_millis(ms));
    }
    let level = dim.level();
    assert!(level > 0.95, "did not rise back when repopulated: {level}");
}

#[test]
fn light_resets_vacant_since_when_repopulated() {
    let mut dim = VacancyDim::new();
    let start = t0();
    let debounce = VacancyDim::VACANCY_DEBOUNCE_MS;
    let at = |ms| start + Duration::from_millis(ms);
    dim.tick(true, start);
    dim.tick(false, at(debounce - 1));
    // Vacant again: the debounce restarts here, so a probe past the first window
    // but inside the new one still holds at 1.0.
    dim.tick(true, at(debounce));
    let level = dim.tick(true, at(2 * debounce - 1));
    assert!(
        (level - 1.0).abs() < 1e-6,
        "vacant_since did not reset on repopulate: {level}"
    );
}

/// A vacant floor's light eases from the debounce's end however long before
/// it the floor was last drawn, so a painter sleeping to that end shows the
/// light a painter drawing every frame does.
#[test]
fn light_eases_from_the_debounces_end_however_long_since_drawn() {
    let fades_at = t0() + Duration::from_millis(VacancyDim::VACANCY_DEBOUNCE_MS);
    let level_drawn_before = |before: u64| {
        let mut dim = VacancyDim::new();
        dim.tick(true, t0());
        dim.tick(true, fades_at - Duration::from_millis(before));
        dim.tick(true, fades_at + PAST_THE_TURN)
    };
    assert_eq!(
        level_drawn_before(crate::anim::PAINT_FRAME_MS),
        level_drawn_before(crate::anim::FULL_TICK_MS)
    );
}

#[test]
fn light_large_dt_does_not_overshoot_or_nan() {
    let mut dim = VacancyDim::new();
    let start = t0();
    dim.tick(true, start);
    let later =
        start + Duration::from_millis(VacancyDim::VACANCY_DEBOUNCE_MS + VacancyDim::FADE_TAU_MS);
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
fn an_offline_floor_dims_as_an_emptied_one_does() {
    let pack = Arc::new(crate::pack::test_office());
    let [offline, emptied] = [
        (
            make_scene(3, 8),
            FloorMeta {
                offline: true,
                ..FloorMeta::ground()
            },
        ),
        (make_scene(0, 8), FloorMeta::ground()),
    ]
    .map(|(scene, floor)| {
        let mut session = FloorSession::new(Arc::clone(&pack));
        (0..3 * VacancyDim::VACANCY_DEBOUNCE_MS / crate::anim::PAINT_FRAME_MS)
            .map(|i| {
                let now = t0() + FRAME * i as u32;
                let world = FloorInputs {
                    scene: &scene,
                    now,
                    floor,
                    pets: PetInputs::default(),
                };
                let frame = session
                    .step(world, Viewport::full(160, 96))
                    .expect("lays out")
                    .frame;
                let dimmed = session.floor().ctx.vacancy_dim.dimmed();
                (frame.indoor_scale, dimmed, frame.neon)
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(offline, emptied);
    assert!(
        offline
            .last()
            .is_some_and(|&(lit, dimmed, _)| lit == VacancyDim::MIN_LEVEL && dimmed),
        "premise: the emptied floor dims"
    );
}

#[test]
fn every_floor_of_an_office_shares_its_offline() {
    let scene = make_scene(3, 2);
    let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
    office.prepare(&scene, t0());
    let world = FloorInputs {
        scene: &scene,
        now: t0(),
        floor: FloorMeta {
            offline: true,
            ..FloorMeta::ground()
        },
        pets: PetInputs::default(),
    };
    for floor in 0..2 {
        let projected = project_floor_scene(&scene, floor);
        let seen = office.floor_world(world, &projected, floor, &[]).floor;
        assert!(seen.offline && seen.floor_idx == floor, "floor {floor}");
    }
}

#[test]
fn light_snap_to_vacant_forces_min_level() {
    let mut dim = VacancyDim::new();
    dim.snap_to_vacant();
    assert!((dim.level() - VacancyDim::MIN_LEVEL).abs() < f32::EPSILON);
}

#[test]
fn coffee_record_stamps_only_new_carriers_and_evict_follows_the_scene() {
    let id = AgentId::from_parts("claude-code", "coffee-test");
    let t0 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let t1 = t0 + std::time::Duration::from_secs(CoffeeState::STEAM_WINDOW_SECS / 2);
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
    let refetch = t0 + Duration::from_secs(CoffeeState::STEAM_WINDOW_SECS);
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
    use crate::layout::Viewport;
    assert_eq!(floor_capacity(Viewport::full(3, 3), 0), 0);
    assert!(floor_capacity(Viewport::full(192, 160), 0) > 0);
}

/// Under top and side insets a floor seats what the bare canvas of its width
/// does, a top inset moving only the sky, and an area narrower than the
/// minimum seats no one, though the whole viewport would.
#[test]
fn floor_capacity_under_top_and_side_insets_seats_what_the_bare_canvas_does() {
    use crate::layout::{Insets, Viewport, min_layout_size};
    let insets = Insets {
        top: 9,
        right: 7,
        bottom: 0,
        left: 5,
    };
    let inside = |w, h| crate::layout::inset_viewport(w, h, insets);
    let office = inside(192, 160);
    for seed in (0..4).map(floor_seed) {
        assert_eq!(
            floor_capacity(office, seed),
            floor_capacity(Viewport::full(192, office.size.h), seed)
        );
    }
    let under = inside(min_layout_size().w - 1, min_layout_size().h);
    assert_eq!(floor_capacity(under, 0), 0);
    assert!(floor_capacity(Viewport::from(under.size), 0) > 0);
}

/// Insets alone re-lay the office out: the memo keys on the whole viewport. A
/// top inset moves only the sky, the floor keeping its rows, so the area inside
/// the insets is what tells the two layouts apart.
#[test]
fn a_change_of_insets_alone_re_lays_the_office_out() {
    use crate::layout::{Insets, Viewport};
    let mut ctx = FloorCtx::new();
    let bare = Viewport::full(192, 160);
    let inset = Viewport {
        insets: Insets {
            top: 9,
            ..Insets::default()
        },
        ..bare
    };
    let a = ctx.frame_layout(bare, 0).expect("lays out");
    let b = ctx.frame_layout(inset, 0).expect("lays out");
    assert_ne!(a.inner, b.inner, "the memo served the bare layout");
    let fresh = crate::layout::SceneLayout::compute(inset, None, 0).expect("lays out");
    assert_eq!(b.inner, fresh.inner);
    assert_eq!(b.walkable, fresh.walkable);
}

#[test]
fn transition_escapes_a_backward_clock_step() {
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let tr = FloorTransition::new(0, 1, start);
    let wobble = start - Duration::from_millis(tr.duration_ms);
    assert!(
        !tr.is_done(wobble),
        "a small wobble must not abort the slide"
    );
    assert!((tr.t(wobble) - 0.0).abs() < f32::EPSILON);
    // Without the escape the renderer stays wedged in the transition composite —
    // no labels, tooltips or hit-testing — until the clock re-passes started_at.
    let stepped = start - Duration::from_millis(tr.duration_ms + 1);
    assert!(
        tr.is_done(stepped),
        "a large backward clock step must complete the transition"
    );
}

/// One classic frame of `scene` at `now` through `session`'s entry.
fn classic_frame(
    session: &mut FloorSession,
    scene: &SceneState,
    now: SystemTime,
    floor: FloorMeta,
    viewport: Viewport,
) -> Option<Arc<crate::layout::SceneLayout>> {
    session.render(
        crate::look::Look::HALF_BLOCKS,
        crate::look::RenderInputs {
            world: FloorInputs {
                scene,
                now,
                floor,
                pets: PetInputs::default(),
            },
            theme: crate::theme::theme_by_name("normal").expect("normal theme exists"),
            viewport,
            place: crate::look::Place::default(),
            debug_walkable: false,
            dim: crate::look::Dim::NONE,
        },
    );
    session.picture().map(|p| Arc::clone(p.hits.layout()))
}

/// The office keeps no tier of model: one at its top effort is drawn, in both
/// looks, as the same agent on any other model, the same effects riding on it.
#[test]
fn a_top_model_at_max_effort_draws_as_any_other_agent() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let as_model = |model: &str| {
        let mut scene = make_scene(1, 8);
        let slot = scene.agents.values_mut().next().expect("one agent");
        slot.model = Some(model.into());
        slot.effort = Some(pixtuoid_core::state::EffortObservation::new(
            "ultra".into(),
            now,
        ));
        scene
    };
    let (top, other) = (as_model("claude-fable-5"), as_model("claude-opus-4-8"));
    let floor = FloorMeta::ground();
    assert!(
        both_painters(&top, floor, None, now) == both_painters(&other, floor, None, now),
        "a top model drew apart"
    );
    let effects = |scene: &SceneState| {
        let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
        let world = FloorInputs {
            scene,
            now,
            floor,
            pets: PetInputs::default(),
        };
        let frame = session.step(world, PAINTED).expect("lays out").frame;
        frame
            .characters
            .iter()
            .flat_map(|c| c.effects.iter().map(|e| e.kind))
            .collect::<Vec<_>>()
    };
    assert_eq!(effects(&top), effects(&other), "something rode a top model");
}

/// A subagent wears a cap, which reaches both painters and leaves the badge
/// where it was; the lead who dispatched them wears none.
#[test]
fn a_subagent_wears_a_cap_and_a_lead_does_not() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let floor = FloorMeta::ground();
    let plain = make_scene(2, 8);
    let [lead, child] = [0, 1].map(|i| AgentId::from_transcript_path(&format!("/p/{i}.jsonl")));
    let mut dispatched = plain.clone();
    dispatched
        .agents
        .get_mut(&child)
        .expect("the child")
        .parent_id = Some(lead);
    let placed = |scene: &SceneState| {
        let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
        let world = FloorInputs {
            scene,
            now,
            floor,
            pets: PetInputs::default(),
        };
        let frame = session.step(world, PAINTED).expect("lays out").frame;
        frame
            .characters
            .iter()
            .map(|c| (frame.agents[c.agent_idx].agent_id, c.clone()))
            .collect::<std::collections::HashMap<_, _>>()
    };
    let caps = |c: &crate::sim::CharacterPlacement| {
        c.effects
            .iter()
            .filter(|e| matches!(e.kind, crate::effects::EffectKind::SubagentCap(_)))
            .count()
    };
    let (before, after) = (placed(&plain), placed(&dispatched));
    assert!(before.values().all(|c| caps(c) == 0), "a lead wore a cap");
    assert_eq!(caps(&after[&lead]), 0, "the lead wore a cap");
    assert_eq!(caps(&after[&child]), 1, "the subagent wore no cap");
    assert_eq!(
        after[&child].label_anchor, before[&child].label_anchor,
        "the cap moved the classic's badge"
    );
    let (classic, cutaway) = both_painters(&dispatched, floor, None, now);
    let (bare_classic, bare_cutaway) = both_painters(&plain, floor, None, now);
    assert!(classic != bare_classic, "the classic drew no cap");
    assert!(cutaway != bare_cutaway, "the cutaway drew no cap");
}

#[test]
fn the_classic_paints_records_coffee_state_and_survives_a_tiny_buffer() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let scene = SceneState::new([8; MAX_FLOORS]);
    let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));

    let none = classic_frame(
        &mut session,
        &scene,
        now,
        FloorMeta::ground(),
        Viewport::full(8, 8),
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
        &scene,
        now,
        FloorMeta::ground(),
        Viewport::full(160, 96),
    );
    assert!(layout.is_some(), "a layoutable size returns the layout");
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let bg = theme.surface.bg_fallback;
    let buf = session.picture().expect("classic buffer").pixels;
    assert!(
        buf.as_slice()
            .iter()
            .any(|p| *p != crate::sprite::Rgb { r: 0, g: 0, b: 0 } && *p != bg),
        "the pixel pass painted office content"
    );
}

#[test]
fn floor_session_render_owns_the_dual_eviction() {
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let gone = AgentId::from_parts("claude-code", "session-evict");
    let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
    session
        .floor_mut()
        .ctx
        .walks
        .insert(gone, WalkState::default());
    session.office.coffee.insert(gone, now);

    let scene = SceneState::new([8; MAX_FLOORS]);
    session.render(
        crate::look::Look::HALF_BLOCKS,
        crate::look::RenderInputs {
            world: FloorInputs {
                scene: &scene,
                now,
                floor: FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            theme,
            viewport: Viewport::full(160, 96),
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
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut scene = make_scene(1, 8);
    for slot in scene.agents.values_mut() {
        slot.created_at = now0;
        slot.arrived_at = Some(slot.created_at);
        slot.state_started_at = now0;
        slot.last_event_at = now0;
    }
    let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
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
            crate::look::Look::HALF_BLOCKS,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                viewport: Viewport::full(160, 96),
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
        crate::look::Look::HALF_BLOCKS,
        crate::look::RenderInputs {
            world: FloorInputs {
                scene: &scene,
                now: now0,
                floor: FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            theme,
            viewport: Viewport::full(8, 8),
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
    let scene = make_scene(1, 8);
    let id = AgentId::from_transcript_path("/p/0.jsonl");
    let t = t0() + FRAME; // a frame in: entry walk in flight
    let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));

    let frame = session
        .step(
            crate::floor::FloorInputs {
                scene: &scene,
                now: t,
                floor: FloorMeta::ground(),
                pets: crate::floor::PetInputs::default(),
            },
            Viewport::full(160, 96),
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
                    now: t,
                    floor: FloorMeta::ground(),
                    pets: crate::floor::PetInputs::default()
                },
                Viewport::full(8, 8)
            )
            .is_none(),
        "an unlayoutable size steps nothing"
    );
}

/// `step` hands back the memoized layout itself, not an equal copy.
#[test]
fn step_hands_back_the_layout_the_sim_stepped_on() {
    let scene = make_scene(1, 8);
    let viewport = Viewport::full(160, 96);
    let meta = FloorMeta::ground();
    let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
    let stepped = session
        .step(
            crate::floor::FloorInputs {
                scene: &scene,
                now: t0(),
                floor: meta,
                pets: crate::floor::PetInputs::default(),
            },
            viewport,
        )
        .expect("a layoutable size steps");
    let memoized = session
        .floor_mut()
        .ctx
        .frame_layout(viewport, meta.floor_seed)
        .expect("the memoized layout");
    assert!(Arc::ptr_eq(&stepped.layout, &memoized));
}

/// A painter steps one floor through its projected scene, which holds no
/// other floor's agents: their coffee must outlive it.
#[test]
fn stepping_a_projected_floor_keeps_other_floors_coffee() {
    let scene = make_scene(17, 16);
    let downstairs = AgentId::from_transcript_path("/p/0.jsonl");
    assert_eq!(scene.agents[&downstairs].floor_idx, 0);
    let mut office = PerOffice::new();
    office.coffee.insert(downstairs, t0());
    let mut upstairs = PerFloor::new(Arc::new(crate::pack::test_office()));
    let projected = project_floor_scene(&scene, 1);
    let stepped = step_floor(
        &mut upstairs,
        &mut office.coffee,
        &mut office.chitchat,
        crate::floor::FloorInputs {
            scene: projected.scene(),
            now: t0(),
            floor: FloorMeta::for_floor(1, num_floors(&scene)),
            pets: crate::floor::PetInputs::default(),
        },
        Viewport::full(160, 96),
    );
    assert!(stepped.is_some());
    assert!(office.coffee.map().contains_key(&downstairs));
}

#[test]
fn audio_observer_frame_composes_stems_and_music_from_the_scene() {
    // Noon, where a storm-dimmed light would read short of full day.
    let now = crate::localclock::at_hour(12);
    let scene = make_scene(4, 16);
    let occupied = std::collections::HashSet::new();
    let mut obs = AudioObserver::default();
    let storm = crate::sky::WeatherPolicy::Forced(crate::sky::Weather::Storm);
    let floor = FloorMeta {
        weather: storm,
        ..FloorMeta::ground()
    };
    let frame = obs.frame(&scene, &occupied, |_| None, floor, now);
    let sky = crate::sky::Sky::at(crate::anim::Motion::Full.timing(now), storm);
    let counts = crate::tally::per_floor_counts(&scene)[0];
    assert_eq!(
        frame.stems,
        crate::audio::stem_levels(&counts, sky.rain()),
        "stems must equal stem_levels(per_floor_counts[floor], rain)"
    );
    assert_eq!(
        frame.music,
        crate::audio::music_controls(&counts, sky.nightfall(), sky.rain()),
        "music must ask music_controls of the same counts and sky"
    );
    assert_eq!(
        frame.music.rain, 1.0,
        "a storm passes its rain to the music"
    );
    let clear = crate::sky::Sky::at(
        crate::anim::Motion::Full.timing(now),
        crate::sky::WeatherPolicy::Forced(crate::sky::Weather::Clear),
    );
    assert_eq!(
        frame.music.daylight,
        crate::audio::music_controls(&counts, clear.nightfall(), 0.0).daylight,
        "the storm's daylight is a clear sky's: a storm never flips the family"
    );
    assert_eq!(
        frame.music.daylight, 1.0,
        "a stormy noon is full day to the music"
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
        let scene = make_scene(6, 8);
        let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
        let layout = classic_frame(
            &mut session,
            &scene,
            now,
            FloorMeta::ground().with_weather(clear),
            Viewport::full(192, 160),
        )
        .expect("layout");
        (session, layout)
    };
    let ((noon_session, layout), (night_session, _)) = (render(at_hour(12)), render(at_hour(2)));
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
    for y in layout.top_margin..(h * 92 / 100) {
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

fn neon_mood(running: usize, requires_action: usize, idle: usize) -> crate::neon_sign::OfficeMood {
    crate::neon_sign::OfficeMood::of(crate::tally::StateCounts {
        running,
        requires_action,
        idle,
        exiting: 0,
        total: running + requires_action + idle,
    })
}

const ROOM_LIT: bool = false;
const ROOM_DIMMED: bool = true;
/// A live painter's frame tick — well under the shortest stutter flash.
const FRAME: Duration = Duration::from_millis(crate::anim::PAINT_FRAME_MS);

#[test]
fn neon_first_tick_snaps_to_its_target() {
    for (mood, room, want) in [
        (neon_mood(2, 1, 0), ROOM_LIT, NeonLevels::ALERT),
        (neon_mood(2, 0, 3), ROOM_LIT, NeonLevels::RUNNING),
        (neon_mood(0, 0, 3), ROOM_LIT, NeonLevels::CALM),
        (neon_mood(0, 0, 0), ROOM_DIMMED, NeonLevels::VACANT),
        (neon_mood(0, 0, 0), ROOM_LIT, NeonLevels::CALM),
        (neon_mood(2, 1, 0), ROOM_DIMMED, NeonLevels::VACANT),
    ] {
        assert_eq!(
            NeonState::new().tick(mood, room, Motion::Full.timing(t0())),
            want,
            "{mood:?}"
        );
    }
}

/// The `--empty` still: one tick after the snap must still judge the floor vacant.
#[test]
fn light_snap_to_vacant_survives_its_first_tick_and_reads_dimmed() {
    let mut dim = VacancyDim::new();
    dim.snap_to_vacant();
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
    let debounce = Duration::from_millis(VacancyDim::VACANCY_DEBOUNCE_MS);
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
        NeonLevels::RUNNING,
        "the change frame still shows the old mood"
    );
    let mid = neon.tick(alert, ROOM_LIT, Motion::Full.timing(changed + fade / 2));
    assert!(
        mid.alert > NeonLevels::RUNNING.alert && mid.alert < NeonLevels::ALERT.alert,
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
    let (calm, running) = (neon_mood(0, 0, 1), neon_mood(3, 0, 0));
    let mut fresh = NeonState::new();
    fresh.tick(calm, ROOM_LIT, Motion::Full.timing(t0()));
    assert_eq!(
        fresh.tick(running, ROOM_LIT, Motion::Full.timing(t0() + fade)),
        NeonLevels::CALM,
        "fades"
    );
    let mut stale = NeonState::new();
    stale.tick(calm, ROOM_LIT, Motion::Full.timing(t0()));
    let later = t0() + fade + Duration::from_millis(1);
    assert_eq!(
        stale.tick(running, ROOM_LIT, Motion::Full.timing(later)),
        NeonLevels::RUNNING,
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
        NeonLevels::RUNNING
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
    for (ms, levels) in starved_cycle(Duration::from_millis(1)).into_iter().skip(1) {
        let in_window = NeonState::STUTTER_FLASHES_MS
            .iter()
            .any(|&(start, end)| (start..end).contains(&ms));
        let want = if in_window {
            NeonLevels::FLASH
        } else {
            NeonLevels::VACANT
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
        assert_eq!(levels, NeonLevels::VACANT, "{ms}ms");
    }
}

/// A painter stepping further than a flash can't draw it: a still (one tick)
/// gets the steady tube, never a held flash.
#[test]
fn neon_a_painter_slower_than_a_flash_never_shows_one() {
    let shortest = Duration::from_millis(NeonState::shortest_flash_ms());
    let slower = shortest + Duration::from_millis(crate::anim::FULL_TICK_MS);
    for (ms, levels) in starved_cycle(slower) {
        assert_eq!(levels, NeonLevels::VACANT, "{ms}ms at a {slower:?} tick");
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
        NeonLevels::VACANT,
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
        now += FRAME;
        last = coasting.tick(neon_mood(0, 0, 0), ROOM_DIMMED, Motion::Full.timing(now));
    }
    assert_ne!(last, NeonLevels::FLASH);
    assert!(last.power > NeonLevels::VACANT.power, "{last:?}");
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
            &scene,
            now,
            floor_meta,
            Viewport::full(192, 160),
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

/// An office with every ambient loop running at `t0`, long settled: two
/// typists, a waiter and a wandering gateway mascot; at rest also idle
/// sleepers, whose wander leaves the sim's walks for real time.
fn ambient_office(t0: SystemTime, resting: bool) -> SceneState {
    use pixtuoid_core::state::ToolKind;
    let typing = || ActivityState::Running {
        item: Some(ToolUse {
            tool_use_id: None,
            detail: None,
            kind: ToolKind::Edit,
        }),
    };
    let settled = t0 - Duration::from_secs(3_600);
    let n = if resting { 5 } else { 3 };
    let mut scene = make_scene(n, 8);
    for (i, slot) in scene.agents.values_mut().enumerate() {
        slot.created_at = settled;
        slot.arrived_at = Some(slot.created_at);
        slot.state_started_at = settled;
        slot.last_event_at = settled;
        slot.state = match i {
            0 | 1 => typing(),
            2 => ActivityState::RequiresAction {
                reason: Some(Arc::from("ok?")),
            },
            _ => ActivityState::Idle,
        };
    }
    gateway_at(&mut scene, settled);
    scene
}

/// A gateway in `scene` up since `at`.
fn gateway_at(scene: &mut SceneState, at: SystemTime) {
    use pixtuoid_core::source::daemon::{DaemonInstanceKey, DaemonPresenceUpdate, apply_presence};
    let key = DaemonInstanceKey::new(
        pixtuoid_core::source::openclaw::SOURCE_NAME,
        pixtuoid_core::state::DaemonInstanceId::new("18789".to_string()).expect("id"),
    );
    apply_presence(
        scene,
        &key,
        DaemonPresenceUpdate::GatewayUp { pid: Some(7) },
        at,
    );
}

/// The painters' office extent and the cutaway's scale over it.
const PAINTED: Viewport = Viewport::full(192, 80);

fn looks() -> [crate::look::Look; 2] {
    let scale = crate::render_scale::RenderScale::new(4).expect("nonzero");
    [
        crate::look::Look::HALF_BLOCKS,
        crate::look::Look::Cutaway {
            scale,
            text: crate::look::WorldText::Baked,
        },
    ]
}

/// `session`'s pixels for `scene` on `floor` at `now`, in `look`.
fn paint(
    session: &mut FloorSession,
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
            now,
            floor,
            pets: PetInputs { pet, petting: None },
        },
        theme,
        viewport: PAINTED,
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
        paint(&mut session, look, scene, floor, pet, now)
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

/// What lets a painter sleep: through each case below, fed between two beats,
/// a frame is the frame painted until the wait
/// [`OfficeSession::until_next_paint`] names after it runs out, short of a new
/// feed, which a painter wakes for; and an office painted only when those
/// waits and feeds say, each paint up to a frame late as an event loop wakes,
/// paints what an office painted every frame does.
#[test]
fn a_frame_holds_for_the_wait_its_office_names() {
    use crate::sky::{Weather, WeatherPolicy};
    /// What a case plays under, its scene before and after a feed at
    /// `switch`, how long it runs, and whether a wander must set off in it.
    struct Case<'a> {
        name: &'static str,
        motion: Motion,
        weather: Weather,
        pet: Option<&'a Pet>,
        petted: bool,
        before: &'a SceneState,
        after: &'a SceneState,
        switch: Duration,
        window: Duration,
        sets_off: bool,
    }
    let cat = Pet {
        kind: crate::pet::PetKind::Cat,
        name: "cat".into(),
    };
    // between two beats, as a feed lands
    let t0 = crate::localclock::at_hour(23) + Duration::from_millis(5_037);
    let pack = Arc::new(crate::pack::test_office());
    let lived_in = ambient_office(t0, false);
    // no gateway: its mascot's walks would outlast the arrival's
    let mut arriving = make_scene(3, 8);
    for slot in arriving.agents.values_mut() {
        (slot.created_at, slot.state_started_at, slot.last_event_at) = (t0, t0, t0);
        slot.arrived_at = Some(t0);
    }
    let empty = make_scene(0, 8);
    let frame = Duration::from_millis(crate::anim::PAINT_FRAME_MS);
    let switch = Duration::from_secs(2);
    // a room of typists, then one turns to the user: the sign fades to its
    // alert while nobody walks
    let typing = settled_typists(t0);
    let mut asking = typing.clone();
    if let Some(slot) = asking.agents.values_mut().next() {
        slot.state = ActivityState::RequiresAction {
            reason: Some(Arc::from("ok?")),
        };
        slot.state_started_at = t0 + switch;
        slot.last_event_at = t0 + switch;
    }
    let mut reading = typing.clone();
    if let Some(slot) = reading.agents.values_mut().next() {
        // a tower for the sheet to fall onto
        slot.tokens_used = 1_000_000;
        slot.last_usage = Some(pixtuoid_core::state::UsageObservation::new(
            crate::token_meter::SHEET_MIN_DELTA_TOKENS,
            t0 + switch,
        ));
    }
    let mut leaving = typing.clone();
    for slot in leaving.agents.values_mut() {
        slot.exiting_at = Some(t0 + switch);
    }
    let mut gateway_up = empty.clone();
    gateway_at(&mut gateway_up, t0 + switch);
    // idle at their desks since long before: each sets off on a wander once
    // its desk dwell (`pose::seated_dwell_ms`) is up
    let mut idling = make_scene(3, 8);
    for slot in idling.agents.values_mut() {
        let settled = t0 - Duration::from_secs(3_600);
        (slot.created_at, slot.state_started_at, slot.last_event_at) = (settled, settled, settled);
        slot.arrived_at = Some(settled);
    }
    let a_second_past = |at: Duration| at + Duration::from_secs(1);
    let calm = FloorMeta::ground().with_motion(Motion::Calm);
    // The first of them to stand at a waypoint, its wander set off, and a
    // feed about 400 ms after the resting paint of the first Calm beat a
    // second into its stay: older than a frame's history, which must still
    // place it.
    let (dweller, at_waypoint) = {
        let mut office = OfficeSession::new(Arc::clone(&pack));
        // A BUDGET for a wander to reach its waypoint, not part of the
        // assertion: two of the longest wander cycles.
        let cycles = (idling.agents.keys())
            .map(|id| Duration::from_millis(2 * crate::pose::est_wander_cycle_ms(*id)))
            .max()
            .expect("agents");
        (0..)
            .map(|tick| t0 + tick * frame)
            .take_while(|&now| now < t0 + cycles)
            .find_map(|now| {
                render_office_with(
                    &mut office,
                    crate::look::Look::HALF_BLOCKS,
                    &idling,
                    calm,
                    now,
                    (&[], None),
                );
                let floor = office.floor(0).expect("drawn");
                (floor.ctx.walks.iter())
                    .find(|(_, w)| {
                        matches!(w.wander.phase, crate::walk::WanderPhase::AtWaypoint(_))
                    })
                    .map(|(id, _)| (*id, now))
            })
            .expect("a wander reaches its waypoint")
    };
    let settled_in = at_waypoint + Duration::from_secs(1);
    let turn = settled_in
        + Motion::Calm
            .until_next_beat(settled_in)
            .expect("Calm turns");
    let prompt_at = (turn + Duration::from_millis(400))
        .duration_since(t0)
        .expect("after t0");
    let mut prompted = idling.clone();
    if let Some(slot) = prompted.agents.get_mut(&dweller) {
        slot.state = ActivityState::Running { item: None };
        (slot.state_started_at, slot.last_event_at) = (t0 + prompt_at, t0 + prompt_at);
    }
    let mut quit = idling.clone();
    if let Some(slot) = quit.agents.get_mut(&dweller) {
        slot.exiting_at = Some(t0 + prompt_at);
    }
    // past the switch, the debounce and the ease to a still room
    let emptied = switch
        + Duration::from_millis(VacancyDim::VACANCY_DEBOUNCE_MS + 6 * VacancyDim::FADE_TAU_MS);
    let petting = crate::pet::PetState {
        petted_at: t0 + switch,
        kind: cat.kind,
        floor_idx: 0,
    };
    let fed = |name, motion, before, after| Case {
        name,
        motion,
        weather: Weather::Clear,
        pet: None,
        petted: false,
        before,
        after,
        switch,
        window: Duration::from_secs(8),
        sets_off: false,
    };
    // the cat walks most of a window, so it rides only where nothing else must
    let cases = [
        Case {
            weather: Weather::Storm,
            ..fed("arrival", Motion::Full, &arriving, &arriving)
        },
        Case {
            pet: Some(&cat),
            window: Duration::from_secs(12),
            ..fed("calm", Motion::Calm, &lived_in, &lived_in)
        },
        Case {
            window: emptied,
            ..fed("emptying", Motion::Full, &typing, &empty)
        },
        Case {
            window: switch + 2 * Duration::from_millis(u64::from(NeonState::FADE_MS)),
            ..fed("asking", Motion::Full, &typing, &asking)
        },
        fed("gateway", Motion::Full, &empty, &gateway_up),
        Case {
            switch: prompt_at,
            window: a_second_past(prompt_at),
            sets_off: true,
            ..fed("prompted at a waypoint", Motion::Calm, &idling, &prompted)
        },
        Case {
            switch: prompt_at,
            window: a_second_past(prompt_at),
            sets_off: true,
            ..fed("quit at a waypoint", Motion::Calm, &idling, &quit)
        },
        Case {
            window: a_second_past(
                switch + Duration::from_millis(crate::token_meter::SHEET_FALL_MS),
            ),
            ..fed("reading", Motion::Full, &typing, &reading)
        },
        Case {
            pet: Some(&cat),
            petted: true,
            window: a_second_past(switch + Duration::from_millis(crate::pet::PET_DURATION_MS)),
            ..fed("petting", Motion::Full, &typing, &typing)
        },
        Case {
            window: a_second_past(switch + pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW),
            ..fed("leaving", Motion::Full, &typing, &leaving)
        },
        Case {
            switch: emptied,
            window: a_second_past(emptied),
            ..fed("refilling a dimmed floor", Motion::Still, &empty, &typing)
        },
    ];
    // the beat is the cutaway's at every scale, so the cheapest checks it
    let looks = [
        crate::look::Look::HALF_BLOCKS,
        crate::look::Look::Cutaway {
            scale: crate::render_scale::RenderScale::new(1).expect("nonzero"),
            text: crate::look::WorldText::Baked,
        },
    ];
    for case in cases {
        let floor = FloorMeta::ground()
            .with_weather(WeatherPolicy::Forced(case.weather))
            .with_motion(case.motion);
        let pets: Vec<Pet> = case.pet.into_iter().cloned().collect();
        let name = case.name;
        for look in looks {
            let mut office = OfficeSession::new(Arc::clone(&pack));
            let mut sparse = OfficeSession::new(Arc::clone(&pack));
            let mut sparse_due = t0;
            let mut held: Option<(SystemTime, Vec<Rgb>)> = None;
            let (mut slept, mut set_off, mut ticks) = (0, false, 0);
            while ticks * frame < case.window {
                let now = t0 + ticks * frame;
                let fed = ticks * frame >= case.switch;
                let feed = fed && (ticks == 0 || (ticks - 1) * frame < case.switch);
                if feed {
                    held = None;
                }
                let scene = if fed { case.after } else { case.before };
                let petting = (case.petted && fed).then_some(&petting);
                let draw = |office: &mut OfficeSession| {
                    render_office_with(office, look, scene, floor, now, (&pets, petting));
                    let picture = office.picture().expect("lays out");
                    picture.pixels().as_slice().to_vec()
                };
                let px = draw(&mut office);
                if let Some((until, held_px)) = &held
                    && now < *until
                {
                    assert!(
                        *held_px == px,
                        "{name} {look:?}: the frame moved before its wait ran out, at {} ms",
                        (ticks * frame).as_millis()
                    );
                }
                // painted at the first frame its wait or a feed reaches, as
                // late as an event loop's wake
                if feed || now >= sparse_due {
                    assert!(
                        draw(&mut sparse) == px,
                        "{name} {look:?}: painted only when it said, it drew another frame at {} ms",
                        (ticks * frame).as_millis()
                    );
                    sparse_due = now + sparse.until_next_paint(now).unwrap_or(frame);
                }
                let wait = office.until_next_paint(now);
                set_off |= ticks > 0 && wait.is_none();
                held = wait.map(|wait| (now + wait - PAST_THE_TURN, px));
                slept += usize::from(held.is_some());
                ticks += 1;
            }
            assert!(slept > 0, "{name} {look:?}: never still off the beat");
            assert!(
                set_off || !case.sets_off,
                "{name} {look:?}: no wander set off"
            );
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

/// `office`'s frame of `scene` at `now` in `look`, its floors moving at `motion`.
fn render_office(
    office: &mut OfficeSession,
    look: crate::look::Look,
    scene: &SceneState,
    motion: Motion,
    now: SystemTime,
) {
    let floor = FloorMeta::ground().with_motion(motion);
    render_office_with(office, look, scene, floor, now, (&[], None));
}

/// [`render_office`] under `floor`, its floors picking their pet of `pets`,
/// `petting` playing.
fn render_office_with(
    office: &mut OfficeSession,
    look: crate::look::Look,
    scene: &SceneState,
    floor: FloorMeta,
    now: SystemTime,
    (pets, petting): (&[Pet], Option<&crate::pet::PetState>),
) {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let _ = office.render(
        look,
        crate::look::RenderInputs {
            world: FloorInputs {
                scene,
                now,
                floor,
                pets: PetInputs { pet: None, petting },
            },
            theme,
            viewport: PAINTED,
            place: crate::look::Place {
                gateway: crate::tally::office_gateway(scene),
                floor: None,
            },
            debug_walkable: false,
            dim: crate::look::Dim::NONE,
        },
        pets,
        theme.surface.bg_fallback,
    );
}

/// Three typists long at their desks: a room nobody walks in.
fn settled_typists(t0: SystemTime) -> SceneState {
    let settled = t0 - Duration::from_secs(3_600);
    let mut scene = make_scene(3, 8);
    for slot in scene.agents.values_mut() {
        (slot.created_at, slot.state_started_at, slot.last_event_at) = (settled, settled, settled);
        slot.arrived_at = Some(settled);
        slot.state = ActivityState::Running {
            item: Some(ToolUse {
                tool_use_id: None,
                detail: None,
                kind: pixtuoid_core::state::ToolKind::Edit,
            }),
        };
    }
    scene
}

/// Under Still a token sheet lands as it drops and a petted pet's hearts hold
/// their first frame, as every motion does there: the frame a reading or a
/// petting brings holds while they last.
#[test]
fn under_still_a_reading_and_a_petting_hold_their_frame() {
    let t0 = crate::localclock::at_hour(12) + Duration::from_millis(37);
    let mut scene = settled_typists(t0);
    if let Some(slot) = scene.agents.values_mut().next() {
        // a tower for the sheet to fall onto
        slot.tokens_used = 1_000_000;
        slot.last_usage = Some(pixtuoid_core::state::UsageObservation::new(
            crate::token_meter::SHEET_MIN_DELTA_TOKENS,
            t0,
        ));
    }
    let cat = Pet {
        kind: crate::pet::PetKind::Cat,
        name: "cat".into(),
    };
    let petting = crate::pet::PetState {
        petted_at: t0,
        kind: cat.kind,
        floor_idx: 0,
    };
    let floor = FloorMeta::ground().with_motion(Motion::Still);
    for look in looks() {
        let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
        let mut frame_at = |ms| {
            let now = t0 + Duration::from_millis(ms);
            let pets = (std::slice::from_ref(&cat), Some(&petting));
            render_office_with(&mut office, look, &scene, floor, now, pets);
            office
                .picture()
                .expect("lays out")
                .pixels()
                .as_slice()
                .to_vec()
        };
        let first = frame_at(crate::anim::PAINT_FRAME_MS);
        assert!(
            first == frame_at(crate::token_meter::SHEET_FALL_MS - 1),
            "{look:?}: the frame moved"
        );
    }
}

/// Under Still, the tier a Reduce Motion user rests on, a lived-in room waits
/// the rest tick through a turn's token reading, a gateway coming up, a
/// petting and an idle agent's dwell running out: none of them moves there,
/// and a mascot that appears a wait late skips nothing.
#[test]
fn under_still_a_lived_in_room_waits_the_rest_tick() {
    let t0 = crate::localclock::at_hour(12) + Duration::from_millis(37);
    let mut idle = make_scene(5, 8);
    for slot in idle.agents.values_mut() {
        let settled = t0 - Duration::from_secs(3_600);
        (slot.created_at, slot.state_started_at, slot.last_event_at) = (settled, settled, settled);
        slot.arrived_at = Some(settled);
    }
    let mut turn_ended = idle.clone();
    if let Some(slot) = turn_ended.agents.values_mut().next() {
        (slot.state_started_at, slot.last_event_at) = (t0, t0);
        // a tower for the sheet to fall onto
        slot.tokens_used = 1_000_000;
        slot.last_usage = Some(pixtuoid_core::state::UsageObservation::new(
            crate::token_meter::SHEET_MIN_DELTA_TOKENS,
            t0,
        ));
    }
    let mut gateway_up = idle.clone();
    gateway_at(&mut gateway_up, t0);
    let cat = Pet {
        kind: crate::pet::PetKind::Cat,
        name: "cat".into(),
    };
    let petting = crate::pet::PetState {
        petted_at: t0,
        kind: cat.kind,
        floor_idx: 0,
    };
    let floor = FloorMeta::ground().with_motion(Motion::Still);
    let now = t0 + Duration::from_millis(crate::anim::PAINT_FRAME_MS);
    for (case, scene, petting) in [
        ("a turn just ended", &turn_ended, None),
        ("a gateway just came up", &gateway_up, None),
        ("a pet being petted", &idle, Some(&petting)),
    ] {
        let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
        let pets = (std::slice::from_ref(&cat), petting);
        render_office_with(
            &mut office,
            crate::look::Look::HALF_BLOCKS,
            scene,
            floor,
            now,
            pets,
        );
        assert_eq!(office.until_next_paint(now), Some(REST_TICK), "{case}");
    }
    let dwell = (idle.agents.keys())
        .map(|id| crate::pose::seated_dwell_ms(*id))
        .min()
        .expect("agents");
    let late = t0 + Duration::from_millis(dwell - crate::anim::CALM_TICK_MS);
    let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
    // drawn on each rest tick, as a painter resting on Still draws it
    let ticks = (0..).map(|tick| t0 + tick * REST_TICK);
    for at in ticks.take_while(|&at| at < late).chain([late]) {
        render_office(
            &mut office,
            crate::look::Look::HALF_BLOCKS,
            &idle,
            Motion::Still,
            at,
        );
    }
    assert_eq!(
        office.until_next_paint(late),
        Some(REST_TICK),
        "a dwell running out"
    );
}

/// A change its floor scheduled wakes an office while it stands, and once it
/// fires, or is called off, or the wall runs past it with no step past it (a
/// paused clock), the wait is the rest tick again.
#[test]
fn a_scheduled_change_wakes_its_office_only_while_it_stands() {
    let t0 = crate::localclock::at_hour(12) + Duration::from_millis(37);
    let look = crate::look::Look::HALF_BLOCKS;
    let empty = make_scene(0, 8);
    let fades_at = t0 + Duration::from_millis(VacancyDim::VACANCY_DEBOUNCE_MS);
    let soon = Duration::from_millis(crate::anim::CALM_TICK_MS);
    let drawn = |at: &[(SystemTime, &SceneState)]| {
        let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
        for &(now, scene) in at {
            render_office(&mut office, look, scene, Motion::Still, now);
        }
        office
    };
    assert_eq!(
        drawn(&[(t0, &empty)]).until_next_paint(t0),
        Some(REST_TICK),
        "a fade further off than the rest tick"
    );
    assert_eq!(
        drawn(&[(t0, &empty), (fades_at - soon, &empty)]).until_next_paint(fades_at - soon),
        Some(soon + PAST_THE_TURN),
        "a fade sooner than the rest tick"
    );
    assert_eq!(
        drawn(&[(t0, &empty), (fades_at + PAST_THE_TURN, &empty)])
            .until_next_paint(fades_at + PAST_THE_TURN),
        None,
        "the fade under way"
    );
    let faded = fades_at + Duration::from_millis(6 * VacancyDim::FADE_TAU_MS);
    assert_eq!(
        drawn(&[
            (t0, &empty),
            (fades_at + PAST_THE_TURN, &empty),
            (faded, &empty)
        ])
        .until_next_paint(faded),
        Some(REST_TICK),
        "the fade done"
    );
    let refilled = settled_typists(t0);
    assert_eq!(
        drawn(&[(t0, &empty), (fades_at - soon, &refilled)]).until_next_paint(fades_at - soon),
        Some(REST_TICK),
        "the fade called off"
    );
    assert_eq!(
        drawn(&[(t0, &empty)]).until_next_paint(fades_at + PAST_THE_TURN),
        Some(REST_TICK),
        "the fade passed unstepped"
    );
}

/// An office that drew `scene` in `look` under `motion` at `t0`.
fn office_drawn_at(
    look: crate::look::Look,
    scene: &SceneState,
    motion: Motion,
    t0: SystemTime,
) -> OfficeSession {
    let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
    render_office(&mut office, look, scene, motion, t0);
    office
}

/// An office never rendered has no frame to hold: it paints now.
#[test]
fn an_office_never_rendered_paints_now() {
    let office = OfficeSession::new(Arc::new(crate::pack::test_office()));
    assert_eq!(
        office.until_next_paint(crate::localclock::at_hour(12)),
        None
    );
}

/// An office paints every frame of a slide between two floors.
#[test]
fn an_office_mid_slide_paints_every_frame() {
    let scene = make_scene(3, 2);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + 3_600);
    let look = crate::look::Look::HALF_BLOCKS;
    let mut office = office_drawn_at(look, &scene, Motion::Still, t0);
    assert!(office.navigate(1, t0), "a second floor to slide to");
    let mid = t0 + Duration::from_millis(TRANSITION_DURATION_MS / 2);
    render_office(&mut office, look, &scene, Motion::Still, mid);
    assert!(
        !office.moves_off_beat(),
        "nothing moves on the floor it leaves"
    );
    assert_eq!(office.until_next_paint(mid), None);
}

/// A room nothing moves in between beats paints just past the next turn of
/// its tier's beat.
#[test]
fn a_room_nothing_moves_in_paints_just_past_its_beats_turn() {
    let t0 = crate::localclock::at_hour(12) + Duration::from_millis(37);
    let scene = settled_typists(t0);
    for motion in [Motion::Full, Motion::Calm] {
        let office = office_drawn_at(crate::look::Look::HALF_BLOCKS, &scene, motion, t0);
        assert!(
            !office.moves_off_beat(),
            "{motion:?}: nothing moves off the beat"
        );
        let wait = office
            .until_next_paint(t0)
            .expect("a moving tier's beat turns");
        let turn = t0 + wait - PAST_THE_TURN;
        assert_ne!(
            motion.beat(turn),
            motion.beat(t0),
            "{motion:?}: painted before the turn"
        );
        assert_eq!(
            motion.beat(turn - Duration::from_millis(1)),
            motion.beat(t0),
            "{motion:?}: painted past the next turn"
        );
    }
}

/// The wait follows the tier of the last render, one whose picture held
/// included: a switch from Still to Full that draws nothing new still turns
/// the office's beat.
#[test]
fn the_wait_follows_the_last_renders_tier_though_its_picture_held() {
    // The epoch's first beat, where Full's loop clock reads Still's 0.
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_millis(37);
    let scene = make_scene(0, 8);
    let [_, look] = looks();
    let mut office = office_drawn_at(look, &scene, Motion::Still, t0);
    render_office(&mut office, look, &scene, Motion::Full, t0);
    assert_eq!(
        office.picture().expect("lays out").dirty(),
        &crate::look::Dirty::Unchanged,
        "the switch held the picture"
    );
    assert_eq!(
        office.until_next_paint(t0),
        Motion::Full
            .until_next_beat(t0)
            .map(|until| until + PAST_THE_TURN)
    );
}

/// A slide never begins during one; it lands on its destination when it
/// finishes, is dropped when its floor goes, and a cancel lands it at once.
#[test]
fn floor_nav_slides_lands_and_clamps() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let done = t0 + Duration::from_millis(TRANSITION_DURATION_MS);
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
        project_floor_scene(&scene, 1).scene().agents.len()
    );
}

/// Every painter's floor reads the one rule for its pet: a petting plays on
/// its own floor alone, and only while it lasts.
#[test]
fn a_petting_plays_only_on_its_own_floor() {
    let scene = make_scene(3, 2);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
    office.prepare(&scene, t0);
    let petting = crate::pet::PetState {
        petted_at: t0,
        kind: crate::pet::PetKind::Cat,
        floor_idx: 0,
    };
    let pets = [crate::pet::Pet::defaulted(crate::pet::PetKind::Cat)];
    let world = |now| FloorInputs {
        scene: &scene,
        now,
        floor: FloorMeta::ground(),
        pets: PetInputs {
            pet: None,
            petting: Some(&petting),
        },
    };
    let petted = |floor, now| {
        office
            .floor_world(
                world(now),
                &project_floor_scene(&scene, floor),
                floor,
                &pets,
            )
            .pets
            .petting
            .is_some()
    };
    assert!(petted(0, t0), "the petted floor");
    assert!(!petted(1, t0), "another floor");
    let over = t0 + Duration::from_millis(crate::pet::PET_DURATION_MS);
    assert!(!petted(0, over), "a petting over");
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
    nav.settle(2, t0 + Duration::from_millis(TRANSITION_DURATION_MS));
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
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let scene = make_scene(3, 2);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
    let render = |office: &mut OfficeSession, now| {
        office.render(
            crate::look::Look::HALF_BLOCKS,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                viewport: Viewport::full(160, 96),
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
    let mid = t0 + Duration::from_millis(TRANSITION_DURATION_MS / 2);
    let _ = render(&mut office, mid);
    let slide = office.picture().expect("the slide is composed");
    assert!(
        matches!(slide, crate::look::Picture::Slide(_)),
        "a slide has no one layout"
    );
    assert!(slide.hits().is_none(), "a slide hits nothing");
    let _ = render(
        &mut office,
        t0 + Duration::from_millis(TRANSITION_DURATION_MS),
    );
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

/// The whole office's [`FloorNamesakes`](crate::nameplate::FloorNamesakes)
/// name each agent as its floor's badge does, so a namesake upstairs leaves a
/// label bare.
#[test]
fn every_floors_badges_read_the_offices_floor_nameplates() {
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let mut scene = make_scene(4, 2);
    let labels = ["cc\u{b7}api", "cc\u{b7}api", "cc\u{b7}web", "cc\u{b7}api"];
    for (i, label) in labels.into_iter().enumerate() {
        let id = AgentId::from_transcript_path(&format!("/p/{i}.jsonl"));
        let agent = scene.agents.get_mut(&id).expect("make_scene's agent");
        agent.label = label.into();
    }
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
    let namesakes = crate::nameplate::FloorNamesakes::of(&scene);
    let mut badged = 0;
    let landed = t0 + Duration::from_millis(TRANSITION_DURATION_MS);
    for (floor, now) in [(0, t0), (1, landed)] {
        if floor > 0 {
            assert!(office.navigate(floor, t0));
        }
        let _ = office.render(
            crate::look::Look::HALF_BLOCKS,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                viewport: Viewport::full(160, 96),
                place: crate::look::Place::default(),
                debug_walkable: false,
                dim: crate::look::Dim::NONE,
            },
            &[],
            theme.surface.bg_fallback,
        );
        let Some(crate::look::Picture::Floor(picture)) = office.picture() else {
            panic!("floor {floor} lays out");
        };
        let text = picture.text.expect("a classic floor hands its text over");
        for badge in text.badges {
            let whole = namesakes.nameplate(&scene.agents[&badge.agent]);
            assert_eq!(badge.name.text(), whole.to_string(), "floor {floor}");
            badged += 1;
        }
    }
    assert_eq!(badged, labels.len(), "premise: every agent wears a badge");
    let upstairs = &scene.agents[&AgentId::from_transcript_path("/p/3.jsonl")];
    assert!(
        crate::nameplate::Namesakes::of(scene.agents.values())
            .nameplate(upstairs)
            .suffix()
            .is_some(),
        "premise: one count over the whole office would suffix the namesake upstairs"
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
                        now,
                        floor: FloorMeta::ground(),
                        pets: PetInputs::default(),
                    },
                    theme,
                    viewport: Viewport::full(160, 96),
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
        frame(WorldText::Host, Some(TRANSITION_DURATION_MS / 2)),
        frame(WorldText::Baked, Some(TRANSITION_DURATION_MS / 2)),
        "a slide bakes it"
    );
}

/// Under a host's dim, the gap a slide leaves between its floors is dimmed
/// as the floors are: no band of it shows at full brightness.
#[test]
fn a_slides_gap_dims_with_its_floors() {
    use crate::look::{Dim, Look};
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let scene = make_scene(3, 2);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let gap = crate::sprite::Rgb {
        r: u8::MAX,
        g: 0,
        b: u8::MAX,
    };
    let dim = Dim::from_brightness(0.4);
    let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
    let render = |office: &mut OfficeSession, now| {
        office.render(
            Look::HALF_BLOCKS,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                viewport: Viewport::full(160, 96),
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
    let _ = render(
        &mut office,
        t0 + Duration::from_millis(TRANSITION_DURATION_MS / 2),
    );
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
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut scene = make_scene(1, 8);
    for slot in scene.agents.values_mut() {
        slot.created_at = now0;
        slot.arrived_at = Some(slot.created_at);
        slot.state_started_at = now0;
        slot.last_event_at = now0;
    }
    let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
    // A BUDGET for the wander to bring someone to a waypoint, not part of
    // the assertion.
    let occupied = (0..600u64).any(|step| {
        let _ = office.render(
            crate::look::Look::HALF_BLOCKS,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    now: now0 + Duration::from_secs(3 * step),
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                viewport: Viewport::full(160, 96),
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
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut office = OfficeSession::new(Arc::new(crate::pack::test_office()));
    let render = |office: &mut OfficeSession, scene: &SceneState| {
        office.render(
            crate::look::Look::HALF_BLOCKS,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene,
                    now: t0,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                theme,
                viewport: Viewport::full(160, 96),
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
    let scene = make_scene(1, 4);
    let id = *scene.agents.keys().next().expect("one agent");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
    let viewport = Viewport::full(160, 96);
    let step = |session: &mut FloorSession, now| {
        session
            .step(
                FloorInputs {
                    scene: &scene,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                viewport,
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

/// `scene` stepped on `session` at `now`, its floor moving at `motion`.
fn frame_at(
    session: &mut FloorSession,
    scene: &SceneState,
    motion: Motion,
    now: SystemTime,
) -> crate::sim::SimFrame {
    session
        .step(
            FloorInputs {
                scene,
                now,
                floor: FloorMeta::ground().with_motion(motion),
                pets: PetInputs::default(),
            },
            Viewport::full(160, 96),
        )
        .expect("lays out")
        .frame
}

/// The elevator follows each walk, not the tier: a walk set off under Full and
/// still under way when Still comes on opens it, and one Still ended at once
/// opens it for no one when Full comes back.
#[test]
fn the_elevator_opens_for_a_walk_through_it_across_a_tier_change() {
    let scene = make_scene(1, 4);
    let frame = Duration::from_millis(crate::anim::PAINT_FRAME_MS);
    let pack = Arc::new(crate::pack::test_office());
    let door = |first: Motion, then: Motion| {
        let mut session = FloorSession::new(Arc::clone(&pack));
        frame_at(&mut session, &scene, first, t0() + frame);
        frame_at(&mut session, &scene, then, t0() + 10 * frame).door_frame
    };
    assert_ne!(
        door(Motion::Full, Motion::Full),
        0,
        "premise: it opens under Full"
    );
    assert_ne!(
        door(Motion::Full, Motion::Still),
        0,
        "a walk under way opens it"
    );
    assert_eq!(
        door(Motion::Still, Motion::Full),
        0,
        "a walk Still ended opens nothing"
    );
}

/// Under Still the pairing swap takes one frame: the frame the device's office
/// changes machine draws the demo cast gone and the live cast seated.
#[test]
fn under_still_a_machine_change_swaps_the_cast_in_one_frame() {
    use crate::pose::Pose;
    use pixtuoid_core::remote::{Machine, Os, RemoteOffice, encode};
    use pixtuoid_core::state::MAX_FLOORS;
    let cast = |names: [&str; 2]| {
        let mut scene = SceneState::uniform(MAX_FLOORS);
        for name in names {
            let id = AgentId::from_transcript_path(&format!("/{name}.jsonl"));
            scene.agents.insert(id, make_slot(id, ActivityState::Idle));
        }
        scene
    };
    let received = |office: &mut RemoteOffice, scene: &SceneState, from: &Machine, at| {
        let line = encode(scene, from, at).expect("a scene encodes");
        office.apply(line.as_bytes(), at).expect("it applies");
    };
    let (demo, live) = (cast(["a", "b"]), cast(["c", "d"]));
    let switch = t0() + Duration::from_secs(1);
    // The demo cast's poses before the switch, and everyone's at it.
    let swap = |motion: Motion| {
        let mut office = RemoteOffice::new([2; MAX_FLOORS]);
        let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
        received(&mut office, &demo, &Machine::new("demo", Os::Other), t0());
        let before = frame_at(&mut session, office.scene(), motion, t0()).poses;
        let live_machine = Machine::new("studio", Os::Macos);
        received(&mut office, &live, &live_machine, switch);
        (
            before,
            frame_at(&mut session, office.scene(), motion, switch).poses,
        )
    };
    let (_, full) = swap(Motion::Full);
    assert!(
        live.agents
            .keys()
            .all(|id| matches!(full.get(id), Some(Some(Pose::Walking { .. })))),
        "premise: under Full the live cast walks in"
    );
    let (before, after) = swap(Motion::Still);
    for id in demo.agents.keys() {
        assert_eq!(before.get(id), Some(&Some(Pose::SeatedIdle)), "premise");
    }
    for id in demo.agents.keys() {
        assert_eq!(
            after.get(id).copied().flatten(),
            None,
            "the demo cast is gone"
        );
    }
    for id in live.agents.keys() {
        assert_eq!(
            after.get(id),
            Some(&Some(Pose::SeatedIdle)),
            "the live cast sits"
        );
    }
}

/// Calm walks every agent walk as Full does, since it slows only the ambient
/// loops: each frame poses each agent alike.
#[test]
fn calm_walks_as_full_does() {
    use crate::pose::Pose;
    let arriving = make_scene(2, 4);
    let mut leaving = arriving.clone();
    let exit = t0() + Duration::from_secs(1);
    if let Some(slot) = leaving.agents.values_mut().nth(1) {
        slot.exiting_at = Some(exit);
    }
    let pack = Arc::new(crate::pack::test_office());
    let (mut full, mut calm) = (
        FloorSession::new(Arc::clone(&pack)),
        FloorSession::new(pack),
    );
    let frame = Duration::from_millis(crate::anim::PAINT_FRAME_MS);
    let mut walked = false;
    for i in 0..200 {
        let now = t0() + frame * i;
        let scene = if now < exit { &arriving } else { &leaving };
        let poses = frame_at(&mut full, scene, Motion::Full, now).poses;
        assert_eq!(
            poses,
            frame_at(&mut calm, scene, Motion::Calm, now).poses,
            "frame {i}"
        );
        walked |= (poses.values().flatten()).any(|pose| matches!(pose, Pose::Walking { .. }));
    }
    assert!(walked, "premise: the agents walk");
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
                    now: t0 + Duration::from_millis(ms),
                    floor: FloorMeta::ground(),
                    pets: PetInputs {
                        pet: Some(&cat),
                        petting: None,
                    },
                },
                Viewport::full(160, 96),
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
                later
                    .take(300)
                    .any(|f| matches!(f.poses[&a], Some(Pose::SeatedIdle | Pose::SeatedTyping))),
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
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let viewport = Viewport::full(160, 96);
    let layout =
        crate::layout::SceneLayout::compute(viewport, None, FloorMeta::ground().floor_seed)
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
    let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
    let step = |session: &mut FloorSession, now| {
        session
            .step(
                FloorInputs {
                    scene: &scene,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                viewport,
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
    use crate::hit::HoverTarget;
    use crate::hit::SceneHit;
    use crate::interact::{Figure, Gesture};
    use crate::layout::{Bounds, Point};
    let pack = Arc::new(crate::pack::test_office());
    let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
    let mut scene = make_scene(1, 4);
    let id = *scene.agents.keys().next().expect("one agent");
    // At work, so it stays in its chair.
    for slot in scene.agents.values_mut() {
        slot.state = ActivityState::Running {
            item: Some(ToolUse {
                tool_use_id: None,
                detail: None,
                kind: pixtuoid_core::state::ToolKind::Edit,
            }),
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
            in_flight_turns: Default::default(),
            current_pid: Some(1),
        },
    );
    let cat = crate::pet::Pet {
        kind: crate::pet::PetKind::Cat,
        name: "Mochi".into(),
    };
    let viewport = Viewport::full(160, 96);
    let render = |session: &mut FloorSession, look, now| {
        session.render(
            look,
            crate::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs {
                        pet: Some(&cat),
                        petting: None,
                    },
                },
                theme,
                viewport,
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
        crate::look::Look::HALF_BLOCKS,
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
        let head = (0..viewport.size.h)
            .flat_map(|y| (0..viewport.size.w).map(move |x| Point { x, y }))
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

mod figure_box {
    use super::*;
    use crate::look::{Look, LookKind, WorldText};
    use crate::render_scale::RenderScale;

    /// A test per look, and the roster [`every_look_kind_has_a_figure_test`]
    /// reads, from the one list: a look here is a look tested.
    macro_rules! looks {
        ($($test:ident: $look:expr,)+) => {
            $(
                #[test]
                fn $test() {
                    holds_its_outfit_every_frame_in($look);
                }
            )+
            const LOOKS: &[fn(RenderScale) -> Look] = &[$($look),+];
        };
    }

    looks! {
        holds_its_outfit_every_frame_in_half_blocks: |_| Look::HALF_BLOCKS,
        holds_its_outfit_every_frame_in_the_classic_at_its_densest_art: |densest| Look::Classic {
            scale: densest,
            text: WorldText::Host,
        },
        holds_its_outfit_every_frame_in_the_classic_past_its_densest_art: |densest| Look::Classic {
            scale: past(densest),
            text: WorldText::Host,
        },
        holds_its_outfit_every_frame_in_the_cutaway_at_one: |_| Look::Cutaway {
            scale: RenderScale::ONE,
            text: WorldText::Host,
        },
        holds_its_outfit_every_frame_in_the_cutaway_at_its_densest_art: |densest| Look::Cutaway {
            scale: densest,
            text: WorldText::Host,
        },
        holds_its_outfit_every_frame_in_the_cutaway_past_its_densest_art: |densest| Look::Cutaway {
            scale: past(densest),
            text: WorldText::Host,
        },
    }

    /// A kind added to [`LookKind`] fails here until [`looks!`] tests a look of it.
    #[test]
    fn every_look_kind_has_a_figure_test() {
        let densest = RenderScale::from(crate::pack::test_office().max_density_variant());
        for kind in LookKind::ALL {
            assert!(
                LOOKS.iter().any(|at| at(densest).kind() == *kind),
                "{kind:?}"
            );
        }
    }

    /// Past its densest art a look blits each art pixel larger, and
    /// `paint_figure` cuts hair by whole art rows.
    fn past(densest: RenderScale) -> RenderScale {
        RenderScale::new(2 * densest.get()).expect("nonzero")
    }

    /// A host crops and rings a person by the box its picture's own hits give: on
    /// every frame of a walk in the look `at` its pack's densest art gives, at the
    /// canvas's edges too, the box lies on the canvas, holds what the person's
    /// outfit draws at the picture's scale, and stands on the half blocks' columns
    /// and feet; it leaves with its agent's floor and its agent. A test per look,
    /// so the suite runs them side by side.
    fn holds_its_outfit_every_frame_in(at: fn(RenderScale) -> Look) {
        use crate::hit::HoverTarget;
        use crate::interact::{Figure, Gesture};
        use crate::layout::{Bounds, Point};
        let pack = Arc::new(crate::pack::test_office());
        let theme = crate::theme::theme_by_name("normal").expect("normal theme exists");
        let viewport = Viewport::full(160, 96);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        // Long idle, their trips walk one to the canvas's west edge and one to its
        // east; the third is held at its top.
        let [west, east, held] = ["/edge/9.jsonl", "/edge/22.jsonl", "/edge/held5.jsonl"]
            .map(AgentId::from_transcript_path);
        // Twins but for their cwd, so for their outfit: where their pictures
        // differ, a sprite drew.
        let mut twins = ["/alpha", "/omega"].map(|cwd| {
            let mut scene = SceneState::uniform(16);
            let idle = t0 - Duration::from_secs(900);
            for (desk, id) in [(1, west), (6, east), (0, held)] {
                scene.agents.insert(
                    id,
                    AgentSlot {
                        cwd: Arc::from(Path::new(cwd)),
                        state_started_at: idle,
                        created_at: idle,
                        arrived_at: Some(idle),
                        last_event_at: idle,
                        desk_index: GlobalDeskIndex(desk),
                        ..make_slot(id, ActivityState::Idle)
                    },
                );
            }
            scene
        });
        let densest = RenderScale::from(pack.max_density_variant());
        let look = at(densest);
        let scale = |look| match look {
            Look::Classic { scale, .. } | Look::Cutaway { scale, .. } => scale,
        };
        let looks: Vec<Look> = [Look::HALF_BLOCKS]
            .into_iter()
            .chain((look != Look::HALF_BLOCKS).then_some(look))
            .collect();
        let mut offices: Vec<[OfficeSession; 2]> = looks
            .iter()
            .map(|_| [(); 2].map(|()| OfficeSession::new(Arc::clone(&pack))))
            .collect();
        // Each look's agent boxes at `now`, the twins' alike, every pixel where
        // the twins differ inside one of them, and whose outfit showed.
        let frame = |offices: &mut [OfficeSession; 2], twins: &[SceneState; 2], look, now| {
            let [a, b] = [0, 1].map(|i| {
                let _feed = offices[i].render(
                    look,
                    crate::look::RenderInputs {
                        world: FloorInputs {
                            scene: &twins[i],
                            now,
                            floor: FloorMeta::ground(),
                            pets: PetInputs::default(),
                        },
                        theme,
                        viewport,
                        place: crate::look::Place::default(),
                        debug_walkable: false,
                        dim: crate::look::Dim::NONE,
                    },
                    &[],
                    crate::sprite::Rgb { r: 0, g: 0, b: 0 },
                );
                let picture = offices[i].picture().expect("lays out");
                let hits = picture.hits().expect("a floor, not a slide");
                let boxes: Vec<(AgentId, Bounds)> = hits
                    .figures()
                    .filter_map(|(target, at)| match target {
                        HoverTarget::Agent(id) => Some((*id, at)),
                        _ => None,
                    })
                    .collect();
                (picture.pixels().clone(), boxes, hits.layout().clone())
            });
            assert_eq!(a.1, b.1, "{look:?}: premise: the twins draw alike");
            let (pixels, boxes, layout) = a;
            let per_unit = scale(look).to_buffer(1);
            let mut shown = vec![false; boxes.len()];
            let width = usize::from(pixels.width());
            let rows = pixels
                .as_slice()
                .chunks(width)
                .zip(b.0.as_slice().chunks(width));
            for (y, (ours, theirs)) in (0..).zip(rows) {
                if ours == theirs {
                    continue;
                }
                let differ = (0..)
                    .zip(ours.iter().zip(theirs))
                    .filter(|(_, (o, t))| o != t);
                for (x, _) in differ {
                    let (ux, uy) = (x / per_unit, y / per_unit);
                    let whose = boxes.iter().position(|(_, at)| {
                        (at.x..at.x + at.width).contains(&ux)
                            && (at.y..at.y + at.height).contains(&uy)
                    });
                    let whose = whose.unwrap_or_else(|| {
                        panic!("{look:?} at {now:?}: ({x}, {y}) drawn outside {boxes:?}")
                    });
                    shown[whose] = true;
                }
            }
            for (id, at) in &boxes {
                assert!(
                    at.x + at.width <= layout.buf_w && at.y + at.height <= layout.buf_h,
                    "{look:?}: {id:?}'s {at:?} leaves the canvas"
                );
            }
            let showed = boxes.iter().zip(shown).filter(|(_, s)| *s);
            let showed: Vec<AgentId> = showed.map(|((id, _), _)| *id).collect();
            (boxes, showed)
        };
        let (mut west_edge, mut east_edge, mut top_edge, mut rose) = (false, false, false, false);
        let mut showed = std::collections::HashSet::new();
        for step in 0..140 {
            let now = t0 + Duration::from_millis(250) * step;
            let mut flat = Vec::new();
            for (shown_in, offices) in looks.iter().zip(&mut offices) {
                let (boxes, shown) = frame(offices, &twins, *shown_in, now);
                if *shown_in == look {
                    showed.extend(shown);
                }
                let look = shown_in;
                assert_eq!(boxes.len(), 3, "{look:?}: premise: all three are drawn");
                if *look == Look::HALF_BLOCKS {
                    for &(_, at) in &boxes {
                        west_edge |= at.x == 0;
                        east_edge |= at.right() == viewport.size.w;
                        top_edge |= at.y == 0;
                    }
                    flat = boxes;
                    continue;
                }
                for (id, at) in &boxes {
                    let (_, f) = flat.iter().find(|(f, _)| f == id).expect("drawn flat too");
                    assert_eq!(
                        (at.x, at.width, at.y + at.height),
                        (f.x, f.width, f.y + f.height),
                        "{look:?}: {id:?} on other columns or feet than the classic's"
                    );
                    assert!(at.y <= f.y, "{look:?}: {id:?}'s {at:?} sinks below {f:?}");
                    rose |= at.y < f.y;
                }
            }
            // Lifted mid-room, where its hair rises over its frame, then carried
            // to the top, where the canvas cuts it.
            let x = viewport.size.w / 2;
            let grip = match step {
                0 => Gesture::Lift {
                    figure: Figure::Agent(held),
                    at: Point {
                        x,
                        y: viewport.size.h / 2,
                    },
                },
                1 => Gesture::Carry(Point { x, y: 0 }),
                _ => continue,
            };
            for office in offices.iter_mut().flatten() {
                office.grip(&grip);
            }
        }
        assert!(west_edge && east_edge && top_edge, "premise: the edges");
        let dense_cutaway =
            matches!(look, Look::Cutaway { scale, .. } if scale.get() >= densest.get());
        assert_eq!(
            rose, dense_cutaway,
            "premise: only the cutaway's dense art rises over the frame"
        );
        assert_eq!(showed.len(), 3, "premise: every outfit shows");
        let now = t0 + Duration::from_secs(60);
        for twin in &mut twins {
            let slot = twin.agents.get_mut(&west).expect("west");
            slot.desk_index = GlobalDeskIndex(16);
            slot.floor_idx = 1;
            twin.agents.remove(&east);
        }
        for (look, offices) in looks.iter().zip(&mut offices) {
            let (boxes, _) = frame(offices, &twins, *look, now);
            assert_eq!(
                boxes.iter().map(|&(id, _)| id).collect::<Vec<_>>(),
                [held],
                "{look:?}: one upstairs and one gone"
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
            in_flight_turns: Default::default(),
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
                    now: t0 + FRAME * paint,
                    floor: FloorMeta::ground(),
                    pets: PetInputs {
                        pet: Some(&cat),
                        petting: None,
                    },
                },
                Viewport::full(160, 96),
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
    let mut scene = make_scene(3, 4);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut slots = scene.agents.values_mut();
    let seated = slots.next().expect("three agents");
    seated.created_at = t0 - Duration::from_secs(120);
    seated.arrived_at = Some(seated.created_at);
    let leaving = slots.next().expect("three agents");
    leaving.exiting_at = Some(t0);
    let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
    let stepped = session
        .step(
            FloorInputs {
                scene: &scene,
                now: t0 + FRAME,
                floor: FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            Viewport::full(160, 96),
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
    let scene = make_scene(1, 4);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let cat = crate::pet::Pet {
        kind: crate::pet::PetKind::Cat,
        name: "Mochi".into(),
    };
    let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
    let pet_at = |session: &mut FloorSession, now| {
        session
            .step(
                FloorInputs {
                    scene: &scene,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs {
                        pet: Some(&cat),
                        petting: None,
                    },
                },
                Viewport::full(160, 96),
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
    let scene = make_scene(1, 4);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let cat = crate::pet::Pet {
        kind: crate::pet::PetKind::Cat,
        name: "Mochi".into(),
    };
    let mut session = FloorSession::new(Arc::new(crate::pack::test_office()));
    let viewport = Viewport::full(160, 96);
    let pet_at = |session: &mut FloorSession, now| {
        let stepped = session
            .step(
                FloorInputs {
                    scene: &scene,
                    now,
                    floor: FloorMeta::ground(),
                    pets: PetInputs {
                        pet: Some(&cat),
                        petting: None,
                    },
                },
                viewport,
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
