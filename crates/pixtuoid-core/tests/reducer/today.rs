use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;
use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::reducer::{EXIT_GRACE_WINDOW, Reducer};
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
    let reading_id = reading_id.map(Into::into);
    AgentEvent::Usage {
        agent_id,
        fresh_tokens,
        reading_id,
    }
}

fn backlog(agent_id: AgentId, replaying: bool) -> AgentEvent {
    AgentEvent::Backlog {
        agent_id,
        replaying,
    }
}

fn call(agent_id: AgentId, tool_use_id: &str, tool: &str) -> AgentEvent {
    AgentEvent::ToolUseStart {
        agent_id,
        tool_use_id: Some(tool_use_id.into()),
        detail: Some(tool.into()),
    }
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
