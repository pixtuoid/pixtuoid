use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;
use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, CopyId, Transport};
use pixtuoid_core::state::reducer::{COPY_RETENTION, EXIT_GRACE_WINDOW, Reducer};
use pixtuoid_core::state::{SceneState, Today};

use crate::start;

use Beat::{
    Both, Call, Day, End, HookCall, HookCallEnd, HookDispatch, HookTokens, Live, Message, Reap,
    Replay, Slot, Start, Tick, Tokens,
};

const A_DAY: Duration = Duration::from_secs(24 * 60 * 60);
const HOOK: Transport = Transport::Hook;
const TAIL: Transport = Transport::Jsonl;

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

fn usage(agent_id: AgentId, fresh_tokens: u64, reading_id: Option<&str>) -> AgentEvent {
    AgentEvent::usage(agent_id, fresh_tokens, reading_id.map(Into::into))
}

/// 100 tokens under reading `m1`, which a copy into another transcript keeps as `copy`.
fn usage_with_copy(agent_id: AgentId, copy: Option<&str>) -> AgentEvent {
    let usage = AgentEvent::usage(agent_id, 100, Some("m1".into()));
    match copy {
        Some(copy) => usage.with_copy_id(CopyId::new(copy)),
        None => usage,
    }
}

fn backlog(agent_id: AgentId, replaying: bool) -> AgentEvent {
    AgentEvent::Backlog {
        agent_id,
        replaying,
    }
}

fn call(agent_id: AgentId, tool_use_id: &str, tool: &str) -> AgentEvent {
    AgentEvent::tool_use_start(agent_id, Some(tool_use_id.into()), Some(tool.into()))
}

/// Call `t1`, which a copy into another transcript keeps as `copy`.
fn call_with_copy(agent_id: AgentId, copy: Option<&str>) -> AgentEvent {
    let call = AgentEvent::tool_use_start(agent_id, Some("t1".into()), Some("Bash".into()));
    match copy {
        Some(copy) => call.with_copy_id(CopyId::new(copy)),
        None => call,
    }
}

/// Two agents seated and live: a parent and the fork that copied its transcript.
fn parent_and_fork(r: &mut Reducer, scene: &mut SceneState) -> (AgentId, AgentId) {
    let (parent, fork) = (id("parent"), id("fork"));
    for a in [parent, fork] {
        start(r, scene, a);
        r.apply(scene, backlog(a, false), t0(), TAIL);
    }
    (parent, fork)
}

fn counted(scene: &SceneState) -> (u64, u64) {
    scene.today.map_or((0, 0), |t| (t.tokens, t.tool_calls))
}

fn slot_of(scene: &SceneState, a: AgentId) -> (u64, u32) {
    let slot = &scene.agents[&a];
    (slot.tokens_used, slot.tool_call_count)
}

/// A step of a row: an event of agent `a`'s, a tick of the reducer's clock, or a
/// check of what came before.
#[derive(Clone, Copy)]
enum Beat {
    Start,
    End,
    /// A tick past the exit grace, which reaps an ended slot.
    Reap,
    Tick,
    /// A reader says a replay begins.
    Replay,
    /// A reader says what follows is live.
    Live,
    Tokens(u64),
    HookTokens(u64),
    /// 100 tokens under an id, so a repeat of it is the same reading.
    Message,
    Call(&'static str),
    HookCall(&'static str),
    HookCallEnd(&'static str),
    HookDispatch,
    /// The day's tokens and tool calls.
    Day(u64, u64),
    /// The slot's tokens and tool calls, which a replay still raises.
    Slot(u64, u32),
    /// [`Beat::Day`] and [`Beat::Slot`] alike.
    Both(u64, u32),
}

/// Plays each row against a fresh counting reducer, naming the row a check fails in.
fn play(rows: &[(&str, &[Beat])]) {
    for (row, beats) in rows {
        let (mut r, mut scene) = counting();
        let (a, at) = (id("a"), t0());
        for beat in *beats {
            let (event, transport) = match *beat {
                Start => {
                    start(&mut r, &mut scene, a);
                    continue;
                }
                Reap => {
                    let past_grace = at + EXIT_GRACE_WINDOW + Duration::from_millis(1);
                    r.tick(&mut scene, past_grace);
                    continue;
                }
                Tick => {
                    r.tick(&mut scene, at);
                    continue;
                }
                Day(tokens, calls) => {
                    assert_eq!(counted(&scene), (tokens, calls), "{row}");
                    continue;
                }
                Slot(tokens, calls) => {
                    assert_eq!(slot_of(&scene, a), (tokens, calls), "{row}");
                    continue;
                }
                Both(tokens, calls) => {
                    assert_eq!(counted(&scene), (tokens, u64::from(calls)), "{row}");
                    assert_eq!(slot_of(&scene, a), (tokens, calls), "{row}");
                    continue;
                }
                End => {
                    let event = AgentEvent::SessionEnd {
                        agent_id: a,
                        as_child: false,
                    };
                    (event, TAIL)
                }
                Replay => (backlog(a, true), TAIL),
                Live => (backlog(a, false), TAIL),
                Tokens(n) => (usage(a, n, None), TAIL),
                HookTokens(n) => (usage(a, n, None), HOOK),
                Message => (usage(a, 100, Some("m1")), TAIL),
                Call(tuid) => (call(a, tuid, "Bash"), TAIL),
                HookCall(tuid) => (call(a, tuid, "Bash"), HOOK),
                HookDispatch => (call(a, "t1", "Agent"), HOOK),
                HookCallEnd(tuid) => {
                    let event = AgentEvent::ToolUseEnd {
                        agent_id: a,
                        tool_use_id: Some(tuid.into()),
                    };
                    (event, HOOK)
                }
            };
            r.apply(&mut scene, event, at, transport);
        }
    }
}

/// A replay rises on the meter, which has always counted it, and never on the day.
#[test]
fn a_replay_never_reaches_the_day() {
    play(&[
        (
            "closed until declared",
            &[Start, Tokens(100), Call("t1"), Day(0, 0), Slot(100, 1)],
        ),
        (
            "open once declared",
            &[Start, Live, Tokens(10), Call("t1"), Day(10, 1)],
        ),
        (
            "closed again for a re-read",
            &[Start, Live, Tokens(1), Replay, Tokens(2), Day(1, 0)],
        ),
        (
            "a hook counts either side of a re-read",
            &[Start, Replay, HookTokens(7), HookCall("t1"), Day(7, 1)],
        ),
        (
            "a child's declaration lands ahead of the start its own file sends",
            &[Live, Start, Tokens(3), Day(3, 0)],
        ),
        (
            "a declaration for nobody does not outlive a tick",
            &[Live, Tick, Start, Tokens(100), Day(0, 0)],
        ),
        (
            "a session's end closes its next life",
            &[Start, Live, End, Reap, Start, Tokens(100), Day(0, 0)],
        ),
        (
            "which can go live again",
            &[Start, Live, End, Reap, Start, Live, Tokens(5), Day(5, 0)],
        ),
        (
            "a resurrected slot starts closed",
            &[Start, Live, End, Start, Tokens(100), Day(0, 0)],
        ),
        (
            "and can go live again",
            &[Start, Live, End, Start, Live, Tokens(5), Day(5, 0)],
        ),
    ]);
}

/// One piece of work is one on the day, however its wire repeats it.
#[test]
fn work_counts_once() {
    play(&[
        (
            "a call seen on both transports",
            &[Start, Live, Call("t1"), HookCall("t1"), Both(0, 1)],
        ),
        (
            "a call's end decoded ahead of its start, through the pre-pass",
            &[Start, HookCallEnd("t1"), HookCall("t1"), Both(0, 1)],
        ),
        (
            "a task dispatch is no call here, as it is none on the slot",
            &[Start, HookDispatch, Both(0, 0)],
        ),
        (
            "nor one whose end came first, past the tracker that takes the rest",
            &[Start, HookCallEnd("t1"), HookDispatch, Both(0, 0)],
        ),
        (
            "a message either side of a replay's end: its reading was the replay's",
            &[Start, Message, Live, Message, Day(0, 0), Slot(100, 0)],
        ),
        (
            "a live message read again by a truncation's replay",
            &[Start, Live, Message, Replay, Message, Live, Both(100, 0)],
        ),
    ]);
}

/// Only a slot counts, and a hook is proof of life enough to take one.
#[test]
fn work_counts_for_a_seated_agent() {
    play(&[
        (
            "a hook's call seats its agent",
            &[HookCall("t1"), Day(0, 1)],
        ),
        (
            "a transcript's seats nobody, nor does a backlog",
            &[Live, Tokens(100), Call("t1"), Day(0, 0)],
        ),
    ]);
}

#[test]
fn a_hostile_reading_saturates_the_count() {
    const MOST: u64 = u64::MAX;
    play(&[(
        "twice the most it holds",
        &[Start, HookTokens(MOST), HookTokens(MOST), Day(MOST, 0)],
    )]);
}

#[test]
fn one_agents_backlog_leaves_anothers_alone() {
    let (mut r, mut scene) = counting();
    let (a, b) = (id("a"), id("b"));
    start(&mut r, &mut scene, a);
    start(&mut r, &mut scene, b);
    r.apply(&mut scene, backlog(a, false), t0(), TAIL);
    r.apply(&mut scene, usage(b, 1_000, None), t0(), TAIL);
    r.apply(&mut scene, usage(a, 7, None), t0(), TAIL);
    assert_eq!(counted(&scene), (7, 0));
}

#[test]
fn a_backlog_touches_neither_the_slot_nor_its_liveness() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    let before = scene.agents[&a].clone();
    let later = t0() + Duration::from_secs(60);
    for replaying in [false, true] {
        r.apply(&mut scene, backlog(a, replaying), later, TAIL);
    }
    let slot = &scene.agents[&a];
    assert_eq!(slot.last_event_at, before.last_event_at);
    assert_eq!(slot.state, before.state);
    assert_eq!(slot.state_started_at, before.state_started_at);
}

#[test]
fn the_first_event_after_midnight_starts_the_day_again() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    r.apply(&mut scene, usage(a, 100, None), t0(), HOOK);
    let next_day = t0() + A_DAY;
    r.apply(&mut scene, usage(a, 5, None), next_day, HOOK);
    assert_eq!(scene.today, Some(Today::new(utc_day(next_day), 5, 0)));
}

/// The writer owns the day: every reader sees `None` with no clock of its own.
#[test]
fn a_tick_past_midnight_retires_the_day() {
    let (mut r, mut scene) = counting();
    let a = id("a");
    start(&mut r, &mut scene, a);
    r.apply(&mut scene, usage(a, 100, None), t0(), HOOK);
    r.tick(&mut scene, t0() + A_DAY / 4);
    assert_eq!(counted(&scene), (100, 0), "still its day");
    r.tick(&mut scene, t0() + A_DAY);
    assert_eq!(scene.today, None);
}

/// Only a counting reducer keeps or retires a day: a device's own has no clock of
/// the server's to judge the line's by.
#[test]
fn a_plain_reducer_keeps_no_count_and_leaves_one_it_was_handed() {
    let mut r = Reducer::new();
    let mut scene = SceneState::uniform(4);
    let a = id("a");
    start(&mut r, &mut scene, a);
    r.apply(&mut scene, usage(a, 100, None), t0(), HOOK);
    assert_eq!(scene.today, None);

    let today = Today::new(utc_day(t0()), 100, 0);
    scene.today = Some(today);
    r.tick(&mut scene, t0() + A_DAY);
    assert_eq!(scene.today, Some(today));
}

/// A line a CLI copies into another session's transcript keeps its `copy_id`,
/// so the copy is the same work: the agent that read it first owns it.
#[test]
fn a_copy_counts_once_across_agents() {
    let (mut r, mut scene) = counting();
    let (parent, fork) = parent_and_fork(&mut r, &mut scene);
    for a in [parent, fork] {
        r.apply(&mut scene, usage_with_copy(a, Some("c1")), t0(), TAIL);
        r.apply(&mut scene, call_with_copy(a, Some("k1")), t0(), TAIL);
    }
    assert_eq!(counted(&scene), (100, 1));
    assert_eq!(slot_of(&scene, parent), (100, 1));
    assert_eq!(slot_of(&scene, fork), (0, 0));
}

/// An id with no `copy_id` is unique only in its own transcript, as codex's
/// running total is, so two agents' equal ids are two pieces of work.
#[test]
fn ids_without_a_copy_id_are_each_agents_own() {
    let (mut r, mut scene) = counting();
    let (parent, fork) = parent_and_fork(&mut r, &mut scene);
    for a in [parent, fork] {
        r.apply(&mut scene, usage_with_copy(a, None), t0(), TAIL);
        r.apply(&mut scene, call_with_copy(a, None), t0(), TAIL);
    }
    assert_eq!(counted(&scene), (200, 2));
}

/// A hook names no `copy_id`, so the call it counted claims one when its
/// transcript line arrives, though that line counts nothing.
#[test]
fn a_hook_first_call_claims_its_copy_id_from_the_transcript() {
    let (mut r, mut scene) = counting();
    let (parent, fork) = parent_and_fork(&mut r, &mut scene);
    r.apply(&mut scene, call(parent, "t1", "Bash"), t0(), HOOK);
    r.apply(&mut scene, call_with_copy(parent, Some("k1")), t0(), TAIL);
    r.apply(&mut scene, call_with_copy(fork, Some("k1")), t0(), TAIL);
    assert_eq!(counted(&scene), (0, 1));
    assert_eq!(slot_of(&scene, fork), (0, 0));
}

/// Ends `a` at `at` and ticks past its walkout, vouching for `live` so it
/// isn't swept with it; returns the tick's time.
fn end_and_reap(
    r: &mut Reducer,
    scene: &mut SceneState,
    a: AgentId,
    live: &[AgentId],
    at: SystemTime,
) -> SystemTime {
    let end = AgentEvent::SessionEnd {
        agent_id: a,
        as_child: false,
    };
    r.apply(scene, end, at, TAIL);
    let reaped = at + EXIT_GRACE_WINDOW + Duration::from_millis(1);
    for &l in live {
        r.apply(scene, AgentEvent::ProofOfLife { agent_id: l }, reaped, TAIL);
    }
    r.tick(scene, reaped);
    assert!(!scene.agents.contains_key(&a), "precondition: reaped");
    reaped
}

/// A copy id outlives the last agent holding it by [`COPY_RETENTION`]: a
/// fork made within it of an ended session counts nothing it copied, and one
/// made later counts it again.
#[test]
fn an_ended_readers_copy_ids_outlive_it_by_the_retention() {
    let (mut r, mut scene) = counting();
    let (parent, fork) = parent_and_fork(&mut r, &mut scene);
    r.apply(&mut scene, usage_with_copy(parent, Some("c1")), t0(), TAIL);
    let reaped = end_and_reap(&mut r, &mut scene, parent, &[fork], t0());
    r.apply(&mut scene, usage_with_copy(fork, Some("c1")), reaped, TAIL);
    assert_eq!(slot_of(&scene, fork), (0, 0), "within it");

    let gone = end_and_reap(&mut r, &mut scene, fork, &[], reaped);
    let later = gone + COPY_RETENTION;
    let late = id("late");
    start(&mut r, &mut scene, late);
    r.apply(&mut scene, backlog(late, false), later, TAIL);
    r.apply(&mut scene, usage_with_copy(late, Some("c1")), later, TAIL);
    assert_eq!(slot_of(&scene, late), (100, 0), "past it");
}

/// A fork of a fork: the first fork, still live, holds what it copied after
/// its parent ends, past any retention (omp's `fork()` closes the parent's
/// writer, so the parent goes idle and is reaped first).
#[test]
fn a_fork_of_a_fork_counts_nothing_its_grandparent_read() {
    let (mut r, mut scene) = counting();
    let (parent, fork) = parent_and_fork(&mut r, &mut scene);
    for a in [parent, fork] {
        r.apply(&mut scene, usage_with_copy(a, Some("c1")), t0(), TAIL);
    }
    let reaped = end_and_reap(&mut r, &mut scene, parent, &[fork], t0());
    let later = reaped + COPY_RETENTION;
    r.apply(
        &mut scene,
        AgentEvent::ProofOfLife { agent_id: fork },
        later,
        TAIL,
    );
    r.tick(&mut scene, later);
    assert!(
        scene.agents[&fork].exiting_at.is_none(),
        "precondition: the fork lives"
    );
    let grandchild = id("grandchild");
    start(&mut r, &mut scene, grandchild);
    r.apply(&mut scene, backlog(grandchild, false), later, TAIL);
    r.apply(
        &mut scene,
        usage_with_copy(grandchild, Some("c1")),
        later,
        TAIL,
    );
    assert_eq!(slot_of(&scene, grandchild), (0, 0));
}

/// A reaped agent's next life rebuilds its meter from a re-read of its own
/// lines, copy ids included, as it does from its plain readings; another
/// agent reading them still counts none of them.
#[test]
fn a_reaped_agents_next_life_counts_its_own_copy_ids_again() {
    let (mut r, mut scene) = counting();
    let (parent, fork) = parent_and_fork(&mut r, &mut scene);
    for a in [parent, fork] {
        r.apply(&mut scene, usage_with_copy(a, Some("c1")), t0(), TAIL);
        r.apply(&mut scene, call_with_copy(a, Some("k1")), t0(), TAIL);
    }
    let first_life = slot_of(&scene, parent);
    let reaped = end_and_reap(&mut r, &mut scene, parent, &[fork], t0());
    start(&mut r, &mut scene, parent);
    r.apply(&mut scene, backlog(parent, true), reaped, TAIL);
    r.apply(
        &mut scene,
        usage_with_copy(parent, Some("c1")),
        reaped,
        TAIL,
    );
    r.apply(&mut scene, call_with_copy(parent, Some("k1")), reaped, TAIL);
    assert_eq!(slot_of(&scene, parent), first_life);
    let another = id("another");
    start(&mut r, &mut scene, another);
    r.apply(
        &mut scene,
        usage_with_copy(another, Some("c1")),
        reaped,
        TAIL,
    );
    r.apply(
        &mut scene,
        call_with_copy(another, Some("k1")),
        reaped,
        TAIL,
    );
    assert_eq!(slot_of(&scene, another), (0, 0));
}

/// The same with the roles swapped: a reaped fork's next life re-reading its
/// copies counts none of them, while its parent lives.
#[test]
fn a_reaped_forks_next_life_counts_none_of_its_copies() {
    let (mut r, mut scene) = counting();
    let (parent, fork) = parent_and_fork(&mut r, &mut scene);
    for a in [parent, fork] {
        r.apply(&mut scene, usage_with_copy(a, Some("c1")), t0(), TAIL);
        r.apply(&mut scene, call_with_copy(a, Some("k1")), t0(), TAIL);
    }
    let reaped = end_and_reap(&mut r, &mut scene, fork, &[parent], t0());
    start(&mut r, &mut scene, fork);
    r.apply(&mut scene, backlog(fork, true), reaped, TAIL);
    r.apply(&mut scene, usage_with_copy(fork, Some("c1")), reaped, TAIL);
    r.apply(&mut scene, call_with_copy(fork, Some("k1")), reaped, TAIL);
    assert_eq!(slot_of(&scene, fork), (0, 0));
}
