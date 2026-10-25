use super::*;
use crate::character::test_support::make_slot;
use crate::physics::{LegProgress, Segment};
use pixtuoid_core::sprite::format::{Piece, Walk};
use pixtuoid_core::state::{GlobalDeskIndex, ToolKind};
use std::path::PathBuf;
use std::time::Duration;

/// Is `segment`'s `travelled` between a straight walk of its own ends `lo` and
/// `hi` fifths through?
fn within_fifths(segment: Segment, lo: u64, hi: u64) -> bool {
    let at = |fifths| {
        Segment::straight(segment.from(), segment.to(), LegProgress::of(fifths, 5)).travelled()
    };
    (at(lo)..=at(hi)).contains(&segment.travelled())
}

fn slot(state: ActivityState, age_ms: u64) -> (AgentSlot, SystemTime) {
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let now = started + Duration::from_millis(age_ms);
    // created_at well before `started` so the entry-animation override
    // doesn't fire in tests that probe regular state→pose mapping.
    let created = started - Duration::from_secs(60);
    let s = AgentSlot {
        session_id: std::sync::Arc::from("abc"),
        cwd: std::sync::Arc::from(PathBuf::from("/repo").as_path()),
        label: "cc".into(),
        state_started_at: started,
        created_at: created,
        last_event_at: created,
        ..make_slot(id, state)
    };
    (s, now)
}

fn layout() -> SceneLayout {
    SceneLayout::compute(120, 96, Some(4)).expect("fits")
}

fn typing() -> ActivityState {
    ActivityState::Active {
        tool_use_id: Some("t".into()),
        detail: Some("Edit".into()),
        kind: ToolKind::Edit,
    }
}

/// Phase boundary helper mirroring idle_pose's absolute estimate timeline.
fn phases(agent_id: AgentId) -> (u64, u64, u64, u64) {
    let seated_end = seated_dwell_ms(agent_id);
    let walk_out_end = seated_end + WANDER_WALK_EST_MS;
    let at_wp_end = walk_out_end + WANDER_DWELL_EST_MS;
    (
        seated_end,
        walk_out_end,
        at_wp_end,
        est_wander_cycle_ms(agent_id),
    )
}

fn first_trip_cycle(agent_id: AgentId) -> u64 {
    (0u64..1000)
        .find(|n| takes_trip(agent_id, *n))
        .expect("agent should trip within first 1000 cycles")
}

#[test]
fn active_state_is_seated_typing_with_cycling_frame() {
    let (s, now) = slot(typing(), 0);
    let l = layout();
    assert_eq!(derive(&s, now, &l), Some(Pose::SeatedTyping));
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

/// Waiting gets NO pose of its own — it rides `SeatedIdle`, so a back-turned
/// desk needs no extra art and the cue reads identically whichever way the seat
/// faces. The RENDER side splits it back out (`sim::resolve_characters` forces
/// the awake `seated` base), because the sleeping sprite reads as the opposite
/// of "wants you".
#[test]
fn waiting_state_stays_seated_and_lets_the_bubble_say_so() {
    let (s, now) = slot(
        ActivityState::Waiting {
            reason: "perm".into(),
        },
        5_000,
    );
    let l = layout();
    assert_eq!(derive(&s, now, &l), Some(Pose::SeatedIdle));
}

#[test]
fn idle_phase_0_is_seated_idle() {
    let (test_slot, _) = slot(ActivityState::Idle, 0);
    let (seated_end, _, _, _) = phases(test_slot.agent_id);
    let (s, now) = slot(ActivityState::Idle, seated_end - 1);
    let l = layout();
    assert_eq!(derive(&s, now, &l), Some(Pose::SeatedIdle));
}

#[test]
fn idle_phase_1_is_walking_out() {
    let (test_slot, _) = slot(ActivityState::Idle, 0);
    let (seated_end, walk_out_end, _, _) = phases(test_slot.agent_id);
    let cycle = est_wander_cycle_ms(test_slot.agent_id);
    let trip_n = first_trip_cycle(test_slot.agent_id);
    let midpoint = trip_n * cycle + seated_end + (walk_out_end - seated_end) / 2;
    let (s, now) = slot(ActivityState::Idle, midpoint);
    let l = layout();
    match derive(&s, now, &l).expect("pose") {
        Pose::Walking { segment, .. } => {
            let travelled = segment.travelled();
            assert!(
                within_fifths(segment, 2, 3),
                "travelled={travelled} of {segment:?}"
            );
            assert!(travelled > 0, "halfway down the leg, it has walked");
        }
        other => panic!("expected Walking, got {other:?}"),
    }
}

#[test]
fn idle_phase_2_is_at_waypoint() {
    let (test_slot, _) = slot(ActivityState::Idle, 0);
    let (_, walk_out_end, at_wp_end, _) = phases(test_slot.agent_id);
    let cycle = est_wander_cycle_ms(test_slot.agent_id);
    let trip_n = first_trip_cycle(test_slot.agent_id);
    let midpoint = trip_n * cycle + walk_out_end + (at_wp_end - walk_out_end) / 2;
    let (s, now) = slot(ActivityState::Idle, midpoint);
    let l = layout();
    match derive(&s, now, &l).expect("pose") {
        Pose::AtWaypoint { wp, .. } => assert!(wp < l.waypoints.len()),
        Pose::AimlessAt { .. } => {}
        other => panic!("expected AtWaypoint or AimlessAt, got {other:?}"),
    }
}

#[test]
fn idle_phase_3_is_walking_back() {
    let (test_slot, _) = slot(ActivityState::Idle, 0);
    let (_, _, at_wp_end, cycle) = phases(test_slot.agent_id);
    let trip_n = first_trip_cycle(test_slot.agent_id);
    let midpoint = trip_n * cycle + at_wp_end + (cycle - at_wp_end) / 2;
    let (s, now) = slot(ActivityState::Idle, midpoint);
    let l = layout();
    match derive(&s, now, &l).expect("pose") {
        Pose::Walking { segment, .. } => {
            assert!(within_fifths(segment, 2, 3), "{segment:?}");
        }
        other => panic!("expected Walking, got {other:?}"),
    }
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

    let (seated_end, walk_out_end, _, cycle) = phases(id);
    let midpoint = n * cycle + seated_end + (walk_out_end - seated_end) / 2;
    let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let now = started + Duration::from_millis(midpoint);
    let mut s = slot(ActivityState::Idle, 0).0;
    s.agent_id = id; // desk_index 0 is valid for the 4-agent layout

    match derive(&s, now, &l).expect("pose") {
        Pose::Walking { segment, .. } => assert!(
            l.walkable.is_walkable(segment.to().x, segment.to().y),
            "pantry walk dest {:?} is not walkable (center={:?})",
            segment.to(),
            l.waypoints[pantry_idx].pos
        ),
        other => panic!("expected Walking at walk-out midpoint, got {other:?}"),
    }
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
fn idle_pose_holds_at_waypoint_for_the_whole_dwell_window() {
    let (test_slot, _) = slot(ActivityState::Idle, 0);
    let (_, walk_out_end, at_wp_end, cycle) = phases(test_slot.agent_id);
    let trip_n = first_trip_cycle(test_slot.agent_id);
    let l = layout();
    let window = at_wp_end - walk_out_end;
    assert!(
        window >= WANDER_DWELL_EST_MS,
        "dwell window too short: {window}"
    );
    for k in 0..=10 {
        let t = trip_n * cycle + walk_out_end + (k * window / 10).min(window - 1);
        let (s, now) = slot(ActivityState::Idle, t);
        match derive(&s, now, &l).expect("pose") {
            Pose::AtWaypoint { .. } | Pose::AimlessAt { .. } => {}
            other => panic!("at t={t} (k={k}) expected resting pose, got {other:?}"),
        }
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
fn non_trip_cycle_is_seated_idle_throughout() {
    let (test_slot, _) = slot(ActivityState::Idle, 0);
    let id = test_slot.agent_id;
    let cycle = est_wander_cycle_ms(id);
    let stay_n = (0u64..100)
        .find(|n| !takes_trip(id, *n))
        .expect("agent should have a non-trip cycle");
    for k in 0..10 {
        let t = stay_n * cycle + (k * cycle / 10);
        let (s, now) = slot(ActivityState::Idle, t);
        let l = layout();
        assert_eq!(
            derive(&s, now, &l),
            Some(Pose::SeatedIdle),
            "t={t} should be SeatedIdle on non-trip cycle"
        );
    }
}

#[test]
fn entry_animation_overrides_normal_pose_while_it_runs() {
    let id = AgentId::from_transcript_path("/p/entry.jsonl");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    // created_at == now0, so since_spawn is the probe offset
    let s = AgentSlot {
        session_id: std::sync::Arc::from("abc"),
        cwd: std::sync::Arc::from(PathBuf::from("/repo").as_path()),
        label: "cc".into(),
        state_started_at: now0,
        created_at: now0,
        last_event_at: now0,
        ..make_slot(id, ActivityState::Idle)
    };
    let probe = now0 + Duration::from_millis(ENTRY_ANIMATION_MS * 3 / 8);
    let l = layout();
    match derive(&s, probe, &l).expect("pose") {
        Pose::Walking { segment, .. } => {
            assert_eq!(
                segment,
                Segment::straight(segment.from(), segment.to(), LegProgress::of(3, 8))
            );
        }
        other => panic!("expected Walking entry, got {other:?}"),
    }
}

#[test]
fn derive_returns_none_when_desk_index_out_of_range() {
    let (mut s, now) = slot(ActivityState::Idle, 0);
    s.desk_index = GlobalDeskIndex(999);
    assert!(derive(&s, now, &layout()).is_none());
}

#[test]
fn exit_override_walks_desk_to_door_within_window() {
    let (mut s, now) = slot(ActivityState::Idle, 0);
    s.exiting_at = Some(now - Duration::from_secs(1));
    let l = layout();
    let desk = l.home_desks[s.desk_index.0];
    match derive(&s, now, &l).expect("pose") {
        Pose::Walking { segment, .. } => {
            let from = segment.from();
            let to = segment.to();
            assert_eq!(
                from,
                desk_walk_anchor_facing(desk.at, desk.facing),
                "exit walk starts at desk anchor"
            );
            assert_eq!(to, l.door_threshold, "exit walk targets the door threshold");
        }
        other => panic!("expected exit Walking, got {other:?}"),
    }
}

#[test]
fn exit_override_returns_none_past_window() {
    let (mut s, now) = slot(ActivityState::Idle, 0);
    s.exiting_at = Some(now - Duration::from_millis(ENTRY_ANIMATION_MS + 1000));
    assert!(derive(&s, now, &layout()).is_none());
}

#[test]
fn waypoint_index_is_zero_when_no_waypoints() {
    let id = AgentId::from_transcript_path("/p/wp.jsonl");
    assert_eq!(waypoint_index_for_cycle(id, 3, 0), 0);
}

#[test]
fn entry_window_fall_through_uses_state_driven_pose() {
    // since_spawn >= ENTRY_ANIMATION_MS, so the entry override's `if` is false
    // and derive falls through.
    let (mut s, now) = slot(
        ActivityState::Waiting {
            reason: "perm".into(),
        },
        ENTRY_ANIMATION_MS + 5_000,
    );
    let l = layout();
    s.created_at = now - Duration::from_millis(ENTRY_ANIMATION_MS + 10_000);
    assert_eq!(derive(&s, now, &l), Some(Pose::SeatedIdle));
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
    let (test_slot, _) = slot(ActivityState::Idle, 0);
    let cycle = est_wander_cycle_ms(test_slot.agent_id);
    let (_, walk_out_end, at_wp_end, _) = phases(test_slot.agent_id);
    let mid_at_wp = walk_out_end + (at_wp_end - walk_out_end) / 2;

    let mut dest_xs = std::collections::HashSet::new();
    for n in 0..50u64 {
        let t = n * cycle + mid_at_wp;
        let (s, now) = slot(ActivityState::Idle, t);
        match derive(&s, now, &l) {
            Some(Pose::AtWaypoint { wp, .. }) => {
                dest_xs.insert(l.waypoints[wp].pos.x);
            }
            Some(Pose::AimlessAt { dest }) => {
                dest_xs.insert(dest.x);
            }
            _ => {}
        }
    }
    assert!(
        dest_xs.len() >= 2,
        "destination should vary across cycles, got {dest_xs:?}"
    );
}

#[test]
fn idle_within_thinking_window_returns_seated_thinking() {
    let (mut s, now) = slot(ActivityState::Idle, 5_000);
    s.last_event_at = now - Duration::from_secs(5);
    let l = layout();
    let p = derive(&s, now, &l).unwrap();
    assert_eq!(p, Pose::SeatedThinking);
}

#[test]
fn idle_past_thinking_window_returns_idle_pose() {
    let (mut s, now) = slot(ActivityState::Idle, 25_000);
    s.last_event_at = now - Duration::from_secs(25);
    let l = layout();
    let p = derive(&s, now, &l).unwrap();
    assert_ne!(p, Pose::SeatedThinking);
}

// `in_thinking_window` is the ONE gate both `state_driven_pose` and
// `derive_with_routing` consult, so pin it directly at the interface.
#[test]
fn in_thinking_window_gates_on_recency_and_prior_activity() {
    let (mut s, now) = slot(ActivityState::Idle, 5_000);

    s.created_at = now - Duration::from_secs(THINKING_WINDOW_SECS - 1);
    s.last_event_at = s.created_at;
    assert!(!in_thinking_window(&s, now));

    s.created_at = now - Duration::from_secs(THINKING_WINDOW_SECS * 3);
    s.last_event_at = now - Duration::from_secs(THINKING_WINDOW_SECS - 1);
    assert!(in_thinking_window(&s, now));

    s.last_event_at = now - Duration::from_secs(THINKING_WINDOW_SECS + 1);
    assert!(!in_thinking_window(&s, now));
}

fn first_trip_cycle_to_kind(
    agent_id: AgentId,
    layout: &SceneLayout,
    target_kind: WaypointKind,
) -> Option<u64> {
    (0u64..2000).find(|n| {
        takes_trip(agent_id, *n) && !is_aimless_cycle(agent_id, *n) && {
            let idx = waypoint_index_for_cycle(agent_id, *n, layout.waypoints.len());
            layout.waypoints[idx].kind == target_kind
        }
    })
}

#[test]
fn walk_back_from_pantry_carries_coffee() {
    let (test_slot, _) = slot(ActivityState::Idle, 0);
    let l = layout();
    let cycle = est_wander_cycle_ms(test_slot.agent_id);
    let (_, _, at_wp_end, _) = phases(test_slot.agent_id);
    let trip_n = first_trip_cycle_to_kind(test_slot.agent_id, &l, WaypointKind::Pantry)
        .expect("agent should visit Pantry within 2000 cycles");
    let midpoint = trip_n * cycle + at_wp_end + (cycle - at_wp_end) / 2;
    let (s, now) = slot(ActivityState::Idle, midpoint);
    match derive(&s, now, &l).expect("pose") {
        Pose::Walking {
            carrying_coffee, ..
        } => {
            assert!(carrying_coffee, "walk-back from Pantry must carry coffee");
        }
        other => panic!("expected Walking, got {other:?}"),
    }
}

#[test]
fn walk_back_from_non_pantry_no_coffee() {
    let (test_slot, _) = slot(ActivityState::Idle, 0);
    let l = layout();
    let cycle = est_wander_cycle_ms(test_slot.agent_id);
    let (_, _, at_wp_end, _) = phases(test_slot.agent_id);
    let trip_n = (0u64..2000)
        .find(|n| {
            takes_trip(test_slot.agent_id, *n) && !is_aimless_cycle(test_slot.agent_id, *n) && {
                let idx = waypoint_index_for_cycle(test_slot.agent_id, *n, l.waypoints.len());
                l.waypoints[idx].kind != WaypointKind::Pantry
            }
        })
        .expect("agent should visit a non-Pantry waypoint within 2000 cycles");
    let midpoint = trip_n * cycle + at_wp_end + (cycle - at_wp_end) / 2;
    let (s, now) = slot(ActivityState::Idle, midpoint);
    match derive(&s, now, &l).expect("pose") {
        Pose::Walking {
            carrying_coffee, ..
        } => {
            assert!(
                !carrying_coffee,
                "walk-back from non-Pantry must NOT carry coffee"
            );
        }
        other => panic!("expected Walking, got {other:?}"),
    }
}

#[test]
fn entry_walk_does_not_carry_coffee() {
    let id = AgentId::from_transcript_path("/p/entry-coffee.jsonl");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let s = AgentSlot {
        session_id: std::sync::Arc::from("abc"),
        cwd: std::sync::Arc::from(PathBuf::from("/repo").as_path()),
        label: "cc".into(),
        state_started_at: now0,
        created_at: now0,
        last_event_at: now0,
        ..make_slot(id, ActivityState::Idle)
    };
    let probe = now0 + Duration::from_millis(1500);
    let l = layout();
    match derive(&s, probe, &l).expect("pose") {
        Pose::Walking {
            carrying_coffee, ..
        } => {
            assert!(!carrying_coffee, "entry walk must not carry coffee");
        }
        other => panic!("expected Walking (entry), got {other:?}"),
    }
}

/// `derive_state_only` must NOT emit the door→desk entry Walking pose `derive`
/// would return here — that would double-walk an agent whose routed walk
/// layer is already driving its own entry walk.
#[test]
fn derive_state_only_skips_entry_override() {
    let id = AgentId::from_transcript_path("/p/entry-so.jsonl");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    // Slot with created_at == now0; probe at 1500 ms → inside entry window.
    let s = AgentSlot {
        session_id: std::sync::Arc::from("abc"),
        cwd: std::sync::Arc::from(PathBuf::from("/repo").as_path()),
        label: "cc".into(),
        state_started_at: now0,
        created_at: now0,
        last_event_at: now0,
        ..make_slot(
            id,
            ActivityState::Active {
                tool_use_id: Some("t".into()),
                detail: Some("Edit".into()),
                kind: ToolKind::Edit,
            },
        )
    };
    let probe = now0 + Duration::from_millis(1500);
    let l = layout();

    match derive(&s, probe, &l).expect("derive pose") {
        Pose::Walking { .. } => {} // expected — entry override fires in derive
        other => panic!("derive should return Walking in entry window, got {other:?}"),
    }

    match derive_state_only(&s, probe, &l).expect("derive_state_only pose") {
        Pose::SeatedTyping => {}
        other => panic!(
            "derive_state_only should return SeatedTyping for Active slot in entry window, got {other:?}"
        ),
    }
}

/// Personality/dwell are id-hashed, so a test needing a specific combination
/// has to scan the id space for one.
fn agent_matching(pred: impl Fn(AgentId) -> bool) -> AgentId {
    (0..100_000u64)
        .map(|i| AgentId::from_transcript_path(&format!("/think/{i}.jsonl")))
        .find(|id| pred(*id))
        .expect("id space should contain a matching agent")
}

/// How long before its Idle period an agent's last event landed.
const SETTLE_LAG_MS: u64 = 2_000;
/// How far into the Idle period [`thinking_slot`]'s SeatedThinking gate releases.
const RELEASE_MS: u64 = THINKING_WINDOW_SECS * 1000 - SETTLE_LAG_MS;

/// Build an Idle slot whose SeatedThinking gate releases [`RELEASE_MS`] into
/// the Idle period.
fn thinking_slot(id: AgentId) -> AgentSlot {
    let (mut s, _) = slot(ActivityState::Idle, 0);
    s.agent_id = id;
    s.last_event_at = s.state_started_at - Duration::from_millis(SETTLE_LAG_MS);
    s
}

#[test]
fn thinking_gate_release_is_continuous_with_seated_thinking() {
    // The wander clock runs from `state_started_at` throughout, so a release
    // landing PAST the cycle's seated phase would pop the agent straight from
    // SeatedThinking at the desk to mid-Walking in the corridor.
    let l = layout();
    let id = agent_matching(|id| takes_trip(id, 0) && seated_dwell_ms(id) + 500 < RELEASE_MS);
    let s = thinking_slot(id);

    let before = s.state_started_at + Duration::from_millis(RELEASE_MS - 100);
    assert_eq!(derive(&s, before, &l), Some(Pose::SeatedThinking));

    let after = s.state_started_at + Duration::from_millis(RELEASE_MS + 200);
    assert_eq!(
        derive(&s, after, &l),
        Some(Pose::SeatedIdle),
        "gate release must land on a desk-seated pose, not mid-wander"
    );
}

#[test]
fn walk_out_intact_when_gate_releases_during_the_seated_phase() {
    // A release inside the seated phase masked nothing, so the continuity guard
    // must not suppress this cycle's walk-out.
    let l = layout();
    let id = agent_matching(|id| takes_trip(id, 0) && seated_dwell_ms(id) > RELEASE_MS + 1_000);
    let s = thinking_slot(id);
    let desk = l.home_desks[0].at;

    let mid_walk_out = seated_dwell_ms(id) + WANDER_WALK_EST_MS / 2;
    let now = s.state_started_at + Duration::from_millis(mid_walk_out);
    match derive(&s, now, &l).expect("pose") {
        Pose::Walking { segment, .. } => {
            let from = segment.from();
            assert_eq!(from, desk, "cycle-0 walk-out starts from the desk");
            assert!(within_fifths(segment, 2, 3), "{segment:?}");
        }
        other => panic!("expected the cycle-0 walk-out, got {other:?}"),
    }
}

#[test]
fn cycle_after_a_suppressed_release_walks_out_from_its_beginning() {
    let l = layout();
    let id = agent_matching(|id| {
        takes_trip(id, 0) && takes_trip(id, 1) && seated_dwell_ms(id) + 500 < RELEASE_MS
    });
    let s = thinking_slot(id);
    let desk = l.home_desks[0].at;

    let cycle = est_wander_cycle_ms(id);
    let mid_walk_out = cycle + seated_dwell_ms(id) + WANDER_WALK_EST_MS / 2;
    let now = s.state_started_at + Duration::from_millis(mid_walk_out);
    match derive(&s, now, &l).expect("pose") {
        Pose::Walking { segment, .. } => {
            let from = segment.from();
            assert_eq!(from, desk, "cycle-1 walk-out starts from the desk");
            assert!(within_fifths(segment, 2, 3), "{segment:?}");
        }
        other => panic!("expected the cycle-1 walk-out, got {other:?}"),
    }
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
    let mut mask = pixtuoid_core::walkable::WalkableMask::new_open(l.buf_w, l.buf_h);
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
    let mut mask = pixtuoid_core::walkable::WalkableMask::new_open(l.buf_w, l.buf_h);
    mask.mark_blocked(0, 0, l.buf_w, l.buf_h, 0);
    l.walkable = mask;

    let desk = l.home_desks[0];
    for seed in 0..8u64 {
        assert_eq!(
            pick_aimless_dest(&l, seed, desk),
            // The desk's OWN facing, not South: the fallback is whatever anchor
            // production would use, and desk 0 is not guaranteed viewer-facing.
            crate::layout::desk_walk_anchor_facing(desk.at, desk.facing),
            "seed {seed}: fully blocked corridor must fall back to the desk anchor"
        );
    }
}

/// A walking person steps by the ground covered, never the clock: a short
/// leg and a long one walked in the same time (so at different speeds) turn
/// their frames once per `stride / frames` of ground each.
#[test]
fn a_walking_person_never_slides() {
    use crate::physics::{SPEED_MULT_MAX, WalkIntent, speed_mult, walk_profile, walk_progress};
    let pack = crate::pack::test_office();
    let walk = pack.piece(Piece::Walking);
    let stride = pack.stride(Walk::Walking);
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
    for length in [12u16, 70] {
        let (from, to) = (
            Point { x: 10, y: 40 },
            Point {
                x: 10 + length,
                y: 40,
            },
        );
        let turns = steps(ENTRY_ANIMATION_MS, &|ms| {
            let Pose::Walking { segment, .. } = linear_walk_pose(ms, from, to) else {
                panic!("a walk");
            };
            segment.travelled()
        });
        judge("the entry walk", turns, f32::from(length));
    }
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
    let stride = pack.stride(Walk::Walking);
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
    let opaque = |f: &pixtuoid_core::sprite::Frame, y: u16| -> Vec<u16> {
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
