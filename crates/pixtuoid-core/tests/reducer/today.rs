use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;
use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::reducer::{EXIT_GRACE_WINDOW, Reducer};
use pixtuoid_core::state::{SceneState, Today};

use crate::{sess_end, start, tool_use_end, tool_use_start};

const A_DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Noon UTC, so `T0 ± minutes` stays on one date.
fn t0() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000 / 86_400 * 86_400 + 43_200)
}

/// UTC, so nothing here reads the runner's zone.
fn utc_day(now: SystemTime) -> NaiveDate {
    let secs = now.duration_since(UNIX_EPOCH).unwrap().as_secs();
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .unwrap()
        .date_naive()
}

fn counting() -> (Reducer, SceneState) {
    (Reducer::new().counting(utc_day), SceneState::uniform(4))
}

fn id(name: &str) -> AgentId {
    AgentId::from_parts("claude-code", name)
}

fn usage(r: &mut Reducer, scene: &mut SceneState, id: AgentId, tokens: u64, tp: Transport) {
    let event = AgentEvent::Usage {
        agent_id: id,
        fresh_tokens: tokens,
        reading_id: None,
    };
    r.apply(scene, event, t0(), tp);
}

fn backlog(r: &mut Reducer, scene: &mut SceneState, id: AgentId, replaying: bool) {
    let event = AgentEvent::Backlog {
        agent_id: id,
        replaying,
    };
    r.apply(scene, event, t0(), Transport::Jsonl);
}

fn counted(scene: &SceneState) -> (u64, u64) {
    scene.today.map_or((0, 0), |t| (t.tokens, t.tool_calls))
}

#[test]
fn a_plain_reducer_keeps_no_count() {
    let mut r = Reducer::new();
    let mut scene = SceneState::uniform(4);
    let a = id("a");
    start(&mut r, &mut scene, a);
    tool_use_start(
        &mut r,
        &mut scene,
        a,
        Some("t1"),
        Some("Bash"),
        t0(),
        Transport::Hook,
    );
    usage(&mut r, &mut scene, a, 100, Transport::Hook);
    assert!(scene.today.is_none());
    assert_eq!(scene.agents[&a].tool_call_count, 1);
}

#[test]
fn a_hook_event_is_live_and_counts_with_no_declaration() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    tool_use_start(
        &mut r,
        &mut scene,
        a,
        Some("t1"),
        Some("Bash"),
        t0(),
        Transport::Hook,
    );
    usage(&mut r, &mut scene, a, 100, Transport::Hook);
    assert_eq!(scene.today, Some(Today::new(utc_day(t0()), 100, 1)));
}

/// A call's end decoded ahead of its start reaches the count through the
/// pre-pass, the second place `tool_call_count` rises.
#[test]
fn a_reordered_hook_start_still_counts() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    tool_use_end(&mut r, &mut scene, a, Some("t1"), t0(), Transport::Hook);
    tool_use_start(
        &mut r,
        &mut scene,
        a,
        Some("t1"),
        Some("Bash"),
        t0(),
        Transport::Hook,
    );
    assert_eq!(counted(&scene), (0, 1));
    assert_eq!(scene.agents[&a].tool_call_count, 1);
}

#[test]
fn a_transcript_event_counts_only_between_its_agents_backlogs() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    let call = |r: &mut Reducer, scene: &mut SceneState, tuid: &str| {
        tool_use_start(
            r,
            scene,
            a,
            Some(tuid),
            Some("Bash"),
            t0(),
            Transport::Jsonl,
        );
    };

    usage(&mut r, &mut scene, a, 100, Transport::Jsonl);
    call(&mut r, &mut scene, "t1");
    assert_eq!(counted(&scene), (0, 0), "closed until declared");
    assert_eq!(scene.agents[&a].tokens_used, 100, "the meter is untouched");
    assert_eq!(scene.agents[&a].tool_call_count, 1);

    backlog(&mut r, &mut scene, a, false);
    usage(&mut r, &mut scene, a, 10, Transport::Jsonl);
    call(&mut r, &mut scene, "t2");
    assert_eq!(counted(&scene), (10, 1), "open");

    backlog(&mut r, &mut scene, a, true);
    usage(&mut r, &mut scene, a, 1_000, Transport::Jsonl);
    call(&mut r, &mut scene, "t3");
    assert_eq!(counted(&scene), (10, 1), "closed again for a re-read");
    assert_eq!(scene.agents[&a].tokens_used, 1_110);
    assert_eq!(scene.agents[&a].tool_call_count, 3);
}

/// Hook events count on either side of a transcript's re-read.
#[test]
fn a_backlog_being_read_leaves_the_agents_hooks_counting() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    backlog(&mut r, &mut scene, a, true);
    usage(&mut r, &mut scene, a, 7, Transport::Hook);
    assert_eq!(counted(&scene), (7, 0));
}

#[test]
fn one_agents_backlog_leaves_anothers_alone() {
    let (mut r, mut scene) = counting();
    let (a, b) = (id("a"), id("b"));
    start(&mut r, &mut scene, a);
    start(&mut r, &mut scene, b);
    backlog(&mut r, &mut scene, a, false);
    usage(&mut r, &mut scene, b, 1_000, Transport::Jsonl);
    usage(&mut r, &mut scene, a, 7, Transport::Jsonl);
    assert_eq!(counted(&scene), (7, 0));
}

#[test]
fn a_call_seen_on_both_transports_counts_once() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    backlog(&mut r, &mut scene, a, false);
    tool_use_start(
        &mut r,
        &mut scene,
        a,
        Some("t1"),
        Some("Bash"),
        t0(),
        Transport::Jsonl,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        a,
        Some("t1"),
        Some("Bash"),
        t0(),
        Transport::Hook,
    );
    assert_eq!(counted(&scene), (0, 1));
    assert_eq!(scene.agents[&a].tool_call_count, 1);
}

#[test]
fn a_task_dispatch_is_no_call_here_as_it_is_no_call_on_the_slot() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    tool_use_start(
        &mut r,
        &mut scene,
        a,
        Some("t1"),
        Some("Agent"),
        t0(),
        Transport::Hook,
    );
    assert_eq!(counted(&scene), (0, 0));
    assert_eq!(scene.agents[&a].tool_call_count, 0);
}

#[test]
fn the_first_event_after_midnight_starts_the_day_again() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    usage(&mut r, &mut scene, a, 100, Transport::Hook);
    let next_day = t0() + A_DAY;
    let event = AgentEvent::Usage {
        agent_id: a,
        fresh_tokens: 5,
        reading_id: None,
    };
    r.apply(&mut scene, event, next_day, Transport::Hook);
    assert_eq!(scene.today, Some(Today::new(utc_day(next_day), 5, 0)));
}

/// The writer owns the day: every reader sees `None` with no clock of its own.
#[test]
fn a_tick_past_midnight_retires_the_day() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    usage(&mut r, &mut scene, a, 100, Transport::Hook);
    r.tick(&mut scene, t0() + A_DAY / 4);
    assert_eq!(counted(&scene), (100, 0), "still its day");
    r.tick(&mut scene, t0() + A_DAY);
    assert_eq!(scene.today, None);
}

/// Nothing but a counting reducer retires a day: a device's own has no clock of
/// the server's to judge the line's by.
#[test]
fn a_plain_reducer_leaves_a_count_it_was_handed() {
    let mut r = Reducer::new();
    let mut scene = SceneState::uniform(4);
    let today = Today::new(utc_day(t0()), 100, 0);
    scene.today = Some(today);
    r.tick(&mut scene, t0() + A_DAY);
    assert_eq!(scene.today, Some(today));
}

#[test]
fn a_count_saved_by_an_earlier_run_carries_on() {
    let (mut r, mut scene) = counting();
    scene.today = Some(Today::new(utc_day(t0()), 1_000, 10));
    let a = id("a");
    start(&mut r, &mut scene, a);
    usage(&mut r, &mut scene, a, 1, Transport::Hook);
    assert_eq!(counted(&scene), (1_001, 10));
}

#[test]
fn a_hostile_reading_saturates_the_count() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    usage(&mut r, &mut scene, a, u64::MAX, Transport::Hook);
    usage(&mut r, &mut scene, a, u64::MAX, Transport::Hook);
    assert_eq!(counted(&scene), (u64::MAX, 0));
}

#[test]
fn work_nobody_seated_counts_nothing() {
    let (mut r, mut scene) = counting();
    let ghost = id("ghost");
    backlog(&mut r, &mut scene, ghost, false);
    usage(&mut r, &mut scene, ghost, 100, Transport::Jsonl);
    tool_use_start(
        &mut r,
        &mut scene,
        ghost,
        Some("t1"),
        Some("Bash"),
        t0(),
        Transport::Jsonl,
    );
    assert!(scene.agents.is_empty(), "a backlog registers nobody");
    assert!(scene.today.is_none());
}

/// A transcript's child is seated by its own file's `SessionStart`, which a
/// read's declaration comes ahead of.
#[test]
fn a_declaration_may_land_ahead_of_its_agents_start() {
    let (mut r, mut scene) = counting();
    let child = id("child");
    backlog(&mut r, &mut scene, child, false);
    start(&mut r, &mut scene, child);
    usage(&mut r, &mut scene, child, 3, Transport::Jsonl);
    assert_eq!(counted(&scene), (3, 0));
}

#[test]
fn a_declaration_for_nobody_does_not_outlive_a_tick() {
    let (mut r, mut scene) = counting();
    let ghost = id("ghost");
    backlog(&mut r, &mut scene, ghost, false);
    r.tick(&mut scene, t0());
    start(&mut r, &mut scene, ghost);
    usage(&mut r, &mut scene, ghost, 100, Transport::Jsonl);
    assert_eq!(counted(&scene), (0, 0));
}

#[test]
fn a_backlog_touches_neither_the_slot_nor_its_liveness() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    let before = scene.agents[&a].clone();
    let later = t0() + Duration::from_secs(60);
    for replaying in [false, true] {
        let event = AgentEvent::Backlog {
            agent_id: a,
            replaying,
        };
        r.apply(&mut scene, event, later, Transport::Jsonl);
    }
    let slot = &scene.agents[&a];
    assert_eq!(slot.last_event_at, before.last_event_at);
    assert_eq!(slot.state, before.state);
    assert_eq!(slot.state_started_at, before.state_started_at);
}

#[test]
fn a_sessions_end_closes_it_for_its_next_life() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    let t = t0();
    start(&mut r, &mut scene, a);
    backlog(&mut r, &mut scene, a, false);
    sess_end(&mut r, &mut scene, a, false, t, Transport::Jsonl);
    r.tick(&mut scene, t + EXIT_GRACE_WINDOW + Duration::from_millis(1));
    assert!(!scene.agents.contains_key(&a), "the slot is gone");

    start(&mut r, &mut scene, a);
    usage(&mut r, &mut scene, a, 100, Transport::Jsonl);
    assert_eq!(counted(&scene), (0, 0), "the new life starts closed");

    backlog(&mut r, &mut scene, a, false);
    usage(&mut r, &mut scene, a, 5, Transport::Jsonl);
    assert_eq!(counted(&scene), (5, 0), "and can go live again");
}

#[test]
fn a_resurrected_slot_starts_closed_and_can_go_live_again() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    let t = t0();
    start(&mut r, &mut scene, a);
    backlog(&mut r, &mut scene, a, false);
    sess_end(&mut r, &mut scene, a, false, t, Transport::Jsonl);
    assert!(scene.agents[&a].exiting_at.is_some());

    let revive = AgentEvent::SessionStart {
        agent_id: a,
        source: "claude-code".into(),
        session_id: "abc".into(),
        cwd: PathBuf::from("/"),
        parent_id: None,
    };
    r.apply(&mut scene, revive, t, Transport::Jsonl);
    assert!(scene.agents[&a].exiting_at.is_none(), "it walked back in");
    usage(&mut r, &mut scene, a, 100, Transport::Jsonl);
    assert_eq!(counted(&scene), (0, 0));

    backlog(&mut r, &mut scene, a, false);
    usage(&mut r, &mut scene, a, 5, Transport::Jsonl);
    assert_eq!(counted(&scene), (5, 0));
}

/// One message's lines either side of a replay's end: its reading landed on the
/// meter during the replay, and its repeat is no new work.
#[test]
fn a_message_straddling_a_replays_end_counts_once_and_never_today() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    let line = || AgentEvent::Usage {
        agent_id: a,
        fresh_tokens: 100,
        reading_id: Some("msg_1".into()),
    };
    r.apply(&mut scene, line(), t0(), Transport::Jsonl);
    backlog(&mut r, &mut scene, a, false);
    r.apply(&mut scene, line(), t0(), Transport::Jsonl);
    assert_eq!(scene.agents[&a].tokens_used, 100);
    assert_eq!(counted(&scene), (0, 0));
}

/// A live message read again by a truncation's replay.
#[test]
fn a_replay_of_a_counted_reading_leaves_the_meter_and_the_day() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    backlog(&mut r, &mut scene, a, false);
    let line = || AgentEvent::Usage {
        agent_id: a,
        fresh_tokens: 100,
        reading_id: Some("msg_1".into()),
    };
    r.apply(&mut scene, line(), t0(), Transport::Jsonl);
    backlog(&mut r, &mut scene, a, true);
    r.apply(&mut scene, line(), t0(), Transport::Jsonl);
    backlog(&mut r, &mut scene, a, false);
    assert_eq!(scene.agents[&a].tokens_used, 100);
    assert_eq!(counted(&scene), (100, 0));
}
