//! One session's two streams folded in every order they can reach the reducer
//! in: a hook at its forwarded-at stamp
//! ([`FORWARDED_AT_STAMP`](crate::source::decoder::FORWARDED_AT_STAMP)), and a
//! transcript line no sooner than it was written and at most the watcher's
//! `DEFAULT_POLL_INTERVAL` after, behind every line above it, since the
//! watcher reads a file in order. A line's write stamp dates its write, or
//! for a line with none, the stamped lines around it do; one ahead of the
//! transcript's first stamp was written no sooner than the earliest stamp
//! either stream records.

use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::reads::{Spelling, WRITE_STAMPS};
use super::{BreadcrumbSink, CASCADE_TICK, Decode, Delivered, Drive, Driven, Fold};
use crate::source::DEFAULT_POLL_INTERVAL;
use crate::source::decoder::{FORWARDED_AT_STAMP, rfc3339_to_epoch};
use crate::state::ActivityState;
use crate::{AgentEvent, Transport};

/// One event a merge applied.
#[derive(Debug, Clone)]
pub struct Step {
    /// The stream its line rode.
    pub transport: Transport,
    /// Its line's index among its stream's non-blank lines, from 0. A
    /// transcript line is a read of its own, and its seed rides in with it.
    pub line: usize,
    /// When it reached the reducer.
    pub at: SystemTime,
    /// The event.
    pub event: AgentEvent,
    /// Its agent's state once it applied; `None` with no slot to hold it.
    pub state: Option<ActivityState>,
}

/// One order's fold.
#[derive(Debug)]
pub struct Merged {
    /// What the pipeline observed, its events in arrival order.
    pub driven: Driven,
    /// Each event as it applied.
    pub steps: Vec<Step>,
}

impl Merged {
    /// Its lines in the order they arrived, as each one's stream and index, a
    /// line's run of steps once.
    pub fn arrivals(&self) -> Vec<(Transport, usize)> {
        let mut lines: Vec<(Transport, usize)> =
            self.steps.iter().map(|s| (s.transport, s.line)).collect();
        lines.dedup();
        lines
    }
}

/// A line `read` delivered events for, its seed alone included, and when its
/// CLI wrote it: no sooner than `at`, and no later than `until`'s stamp allows.
#[derive(Debug)]
struct Written {
    line: usize,
    at: SystemTime,
    /// The stamp that bounds its write from above, as `(until, within)`: the
    /// stamped write is exactly `until` when `within` is zero, else in
    /// `until..until + within`. Its own, else the next stamped line's, as a
    /// file is written in order; `None` past the file's last stamp.
    until: Option<(SystemTime, Duration)>,
    events: Vec<Delivered>,
}

/// Every order a session's two streams can reach the reducer in, each folded.
#[derive(Debug)]
pub struct Merges {
    hooks: Vec<Written>,
    transcript: Vec<Written>,
    /// For each transcript line, the fewest and the most hooks that can reach
    /// the reducer before it.
    bounds: Vec<(usize, usize)>,
    /// How many hooks precede each transcript line in the next order.
    next: Option<Vec<usize>>,
    /// What decoding counted, which every order shares.
    decoded: Driven,
    /// The last order folded, kept so the next resumes where they part.
    trail: Trail,
}

/// An order's fold, with a [`Mark`] after each of its transcript lines: the
/// odometer moves the later lines fastest, so most orders share all but their
/// tail with the one before, which is folded once instead of once per order.
#[derive(Debug, Default)]
struct Trail {
    order: Vec<usize>,
    marks: Vec<Mark>,
    steps: Vec<Step>,
}

/// The fold just past one transcript line, and how much of the trail it had.
#[derive(Debug)]
struct Mark {
    fold: Fold,
    steps: usize,
    last: SystemTime,
    taken: usize,
}

impl Drive {
    /// Every order `hooks` and this transcript's `lines` can reach the reducer
    /// in ([`Merges`]); `None` for a drive of hooks alone, or of a source whose
    /// transcript carries no write stamp (`reads::WRITE_STAMPS`).
    pub fn merges<S: AsRef<str>>(&self, hooks: &[S], lines: &[S]) -> Option<Merges> {
        let Decode::Transcript { source, .. } = &self.decode else {
            return None;
        };
        let &(_, key, spelling) = WRITE_STAMPS.iter().find(|(s, ..)| s == source)?;
        let mut decoded = Driven::empty();
        // Each line arrives alone, so it is a read of its own.
        let (own, mut fold) = (self.own_agent(), Fold::default());
        let read = |line: &str, events| fold.deliver(self, own, &[Some(line)], events);
        let sink = BreadcrumbSink::default();
        let (hooks, transcript) = tracing::subscriber::with_default(sink.clone(), || {
            let hooks = written(
                &Decode::Hooks,
                hooks,
                |_, e| e.into_iter().map(Delivered::Wire).collect(),
                &mut decoded,
                |v| {
                    let ms = v.get(FORWARDED_AT_STAMP)?.as_u64()?;
                    Some((Duration::from_millis(ms), Duration::ZERO))
                },
                None,
            );
            let transcript = written(
                &self.decode,
                lines,
                read,
                &mut decoded,
                |v| stamp_of(v.get(key)?, spelling),
                hooks.iter().map(|h| h.at).min(),
            );
            (hooks, transcript)
        });
        decoded.breadcrumbs = sink.take();
        let bounds = bounds(&hooks, &transcript);
        let next = first_order(&bounds);
        Some(Merges {
            hooks,
            transcript,
            bounds,
            next,
            decoded,
            trail: Trail::default(),
        })
    }
}

/// The earliest instant `v`, spelled as `spelling`, admits for its write, and
/// how long after it the write could have been.
fn stamp_of(v: &Value, spelling: Spelling) -> Option<(Duration, Duration)> {
    match spelling {
        Spelling::Rfc3339 => Some((rfc3339_to_epoch(v.as_str()?)?, Duration::ZERO)),
        Spelling::Rfc3339Secs => {
            let second = Duration::from_secs(1);
            let at = rfc3339_to_epoch(v.as_str()?)?.checked_sub(second)?;
            Some((at, 2 * second))
        }
        Spelling::EpochMicros => Some((Duration::from_micros(v.as_u64()?), Duration::ZERO)),
    }
}

/// `lines`' lines `read` delivers events for, each stamped by `stamp` and
/// counted in `d`. A line with no stamp of its own was written between the
/// stamped lines around it, and one ahead of the file's first stamp no sooner
/// than the earlier of that stamp and `other_earliest`, the other stream's
/// earliest stamp (`None` for the hooks, which are read first).
fn written<S: AsRef<str>>(
    decode: &Decode,
    lines: &[S],
    mut read: impl FnMut(&str, Vec<AgentEvent>) -> Vec<Delivered>,
    d: &mut Driven,
    stamp: impl Fn(&Value) -> Option<(Duration, Duration)>,
    other_earliest: Option<SystemTime>,
) -> Vec<Written> {
    let mut out = Vec::new();
    let mut stamps = Vec::new();
    let mut at = SystemTime::UNIX_EPOCH;
    let lines = lines
        .iter()
        .map(AsRef::as_ref)
        .filter(|l| !l.trim().is_empty());
    for (index, line) in lines.enumerate() {
        let mut own = None;
        let parsed = |v: &Value| {
            own = stamp(v).map(|(since, within)| (SystemTime::UNIX_EPOCH + since, within));
        };
        let decoded = decode.line(Some(line), d, parsed);
        stamps.push(own);
        if let Some((stamped, _)) = own {
            at = stamped;
        }
        let Some(events) = decoded else {
            continue;
        };
        let events = read(line, events);
        if !events.is_empty() {
            out.push(Written {
                line: index,
                at,
                until: own,
                events,
            });
        }
    }
    let mut next = None;
    let mut next_after = vec![None; stamps.len()];
    for (index, own) in stamps.iter().enumerate().rev() {
        next_after[index] = next;
        next = own.or(next);
    }
    // Never at the epoch, which would age the slot first sight registers past
    // every stale threshold by the next arrival: a reap production never makes.
    let first = stamps.iter().flatten().map(|&(at, _)| at).next();
    let floor = first.into_iter().chain(other_earliest).min();
    for w in &mut out {
        w.until = w.until.or(next_after[w.line]);
        w.at = floor.map_or(w.at, |floor| w.at.max(floor));
    }
    out
}

/// For each transcript line, the fewest and the most `hooks` that can precede
/// it: every hook stamped before it was written does, none stamped past the
/// poll interval after it can, and neither bound may break the file's order.
fn bounds(hooks: &[Written], transcript: &[Written]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = transcript
        .iter()
        .map(|t| {
            let most = t.until.map_or(hooks.len(), |(until, within)| {
                let latest = until + within + DEFAULT_POLL_INTERVAL;
                let reachable =
                    |h: &&Written| h.at < latest || (within.is_zero() && h.at == latest);
                hooks.iter().filter(reachable).count()
            });
            (hooks.iter().filter(|h| h.at < t.at).count(), most)
        })
        .collect();
    for i in 1..out.len() {
        out[i].0 = out[i].0.max(out[i - 1].0);
    }
    for i in (1..out.len()).rev() {
        out[i - 1].1 = out[i - 1].1.min(out[i].1);
    }
    out
}

/// The order with every transcript line as early as `bounds` let it be, `None`
/// when they admit none.
fn first_order(bounds: &[(usize, usize)]) -> Option<Vec<usize>> {
    bounds
        .iter()
        .all(|(lo, hi)| lo <= hi)
        .then(|| bounds.iter().map(|&(lo, _)| lo).collect())
}

impl Merges {
    /// The order after `order`, as an odometer counts: the last line that can
    /// move one hook later does, and every line after it goes back to its
    /// earliest.
    fn after(&self, order: &[usize]) -> Option<Vec<usize>> {
        let mut next = order.to_vec();
        let k = (0..next.len())
            .rev()
            .find(|&k| next[k] < self.bounds[k].1)?;
        next[k] += 1;
        for j in k + 1..next.len() {
            next[j] = next[j - 1].max(self.bounds[j].0);
        }
        Some(next)
    }

    /// `order` folded: each line at the earliest instant it can arrive there,
    /// resumed from the last order's [`Mark`] before the first line whose
    /// hooks differ.
    fn folded(&mut self, order: &[usize]) -> Merged {
        let trail = &mut self.trail;
        let shared = (trail.order.iter().zip(order))
            .take_while(|(a, b)| a == b)
            .count();
        trail.marks.truncate(shared);
        let (mut fold, mut last, mut taken) = match trail.marks.last() {
            Some(mark) => {
                trail.steps.truncate(mark.steps);
                (mark.fold.clone(), mark.last, mark.taken)
            }
            None => {
                trail.steps.clear();
                let fold = Fold {
                    driven: Driven {
                        lines: self.decoded.lines,
                        unparseable: self.decoded.unparseable,
                        decode_errors: self.decoded.decode_errors.clone(),
                        ..Driven::empty()
                    },
                    ..Fold::default()
                };
                (fold, SystemTime::UNIX_EPOCH, 0)
            }
        };
        // Each line at its arrival instant, never `Fold::feed`'s cascade tick
        // after each event: that tick's gc expires `HOOK_WINS_WINDOW` before the
        // other transport's twin arrives.
        for (t, &before) in self.transcript.iter().zip(order).skip(shared) {
            for h in self.hooks.get(taken..before).unwrap_or_default() {
                arrive(&mut fold, &mut trail.steps, &mut last, Transport::Hook, h);
            }
            taken = taken.max(before);
            arrive(&mut fold, &mut trail.steps, &mut last, Transport::Jsonl, t);
            trail.marks.push(Mark {
                fold: fold.clone(),
                steps: trail.steps.len(),
                last,
                taken,
            });
        }
        for h in self.hooks.get(taken..).unwrap_or_default() {
            arrive(&mut fold, &mut trail.steps, &mut last, Transport::Hook, h);
        }
        fold.tick(last + CASCADE_TICK);
        trail.order = order.to_vec();
        // A fold applies decoded events and logs no drift: decoding's
        // breadcrumbs are every order's.
        let mut driven = fold.finish();
        driven.breadcrumbs = self.decoded.breadcrumbs.clone();
        Merged {
            driven,
            steps: trail.steps.clone(),
        }
    }
}

/// `w` folded as `transport` delivered it, at the latest arrival yet: a line
/// read past a later-written one still lands after it.
fn arrive(
    fold: &mut Fold,
    steps: &mut Vec<Step>,
    last: &mut SystemTime,
    transport: Transport,
    w: &Written,
) {
    *last = (*last).max(w.at);
    fold.tick(*last);
    for delivered in &w.events {
        let event = delivered.event();
        fold.apply(transport, delivered.clone(), *last);
        steps.push(Step {
            transport,
            line: w.line,
            at: *last,
            event: event.clone(),
            state: (fold.driven.scene.agents.get(&event.agent_id())).map(|s| s.state.clone()),
        });
    }
}

impl Iterator for Merges {
    type Item = Merged;

    fn next(&mut self) -> Option<Merged> {
        let order = self.next.take()?;
        self.next = self.after(&order);
        Some(self.folded(&order))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SESSION: &str = "019e7762-9ded-7e33-8e5c-3b1c8f6f0a01";

    fn rollout() -> String {
        format!("/h/.codex/sessions/2026/10/27/rollout-2026-10-27T04-47-02-{SESSION}.jsonl")
    }

    fn drive() -> Drive {
        Drive::transcript("codex", &rollout())
            .expect("codex has a transcript")
            .seeded()
    }

    /// The RFC 3339 spelling of `ms` after the epoch.
    fn iso(ms: u64) -> String {
        const DAY_MS: u64 = 86_400_000;
        assert!(ms < DAY_MS, "a day is all these tests span");
        let (s, ms) = (ms / 1000, ms % 1000);
        format!(
            "1970-01-01T{:02}:{:02}:{:02}.{ms:03}Z",
            s / 3600,
            s / 60 % 60,
            s % 60
        )
    }

    fn hook(event: &str, ms: u64) -> String {
        json!({
            "_pixtuoid_source": "codex",
            "hook_event_name": event,
            "session_id": SESSION,
            "cwd": "/repo",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
            "tool_use_id": "call_1",
            FORWARDED_AT_STAMP: ms,
        })
        .to_string()
    }

    fn call(ms: u64) -> String {
        json!({
            "timestamp": iso(ms),
            "type": "response_item",
            "payload": {"type": "function_call", "name": "exec_command",
                        "arguments": "{\"cmd\":\"ls\"}", "call_id": "call_1"},
        })
        .to_string()
    }

    fn output(ms: u64) -> String {
        json!({
            "timestamp": iso(ms),
            "type": "response_item",
            "payload": {"type": "function_call_output", "call_id": "call_1", "output": "ok"},
        })
        .to_string()
    }

    /// `line` with no stamp of its own.
    fn unstamped(line: &str) -> String {
        let mut line: Value = serde_json::from_str(line).expect("json");
        line.as_object_mut().expect("a line").remove("timestamp");
        line.to_string()
    }

    /// Each order's [`Merged::arrivals`].
    fn orders(merges: Merges) -> Vec<Vec<(Transport, usize)>> {
        merges.map(|m| m.arrivals()).collect()
    }

    const H: Transport = Transport::Hook;
    const J: Transport = Transport::Jsonl;

    #[test]
    fn a_transcript_line_reaches_the_reducer_after_every_hook_stamped_before_it_was_written() {
        let hooks = [hook("SessionStart", 1_000), hook("PreToolUse", 3_000)];
        let lines = [call(2_000)];
        let merges = drive().merges(&hooks, &lines).expect("codex merges");
        assert_eq!(
            orders(merges),
            [vec![(H, 0), (J, 0), (H, 1)], vec![(H, 0), (H, 1), (J, 0)],]
        );
    }

    #[test]
    fn a_transcript_line_reaches_the_reducer_within_the_poll_interval() {
        let deadline = u64::try_from(DEFAULT_POLL_INTERVAL.as_millis()).expect("ms") + 1_000;
        let lines = [call(1_000)];
        let orders_with = |pre_use: u64| {
            let hooks = [hook("SessionStart", 1_000), hook("PreToolUse", pre_use)];
            orders(drive().merges(&hooks, &lines).expect("codex merges"))
        };
        assert_eq!(
            orders_with(deadline + 1),
            [vec![(J, 0), (H, 0), (H, 1)], vec![(H, 0), (J, 0), (H, 1)],],
            "a tie may go either way, and a hook past the interval never precedes it"
        );
        assert_eq!(
            orders_with(deadline),
            [
                vec![(J, 0), (H, 0), (H, 1)],
                vec![(H, 0), (J, 0), (H, 1)],
                vec![(H, 0), (H, 1), (J, 0)],
            ],
            "the interval's last instant is in it"
        );
    }

    #[test]
    fn only_an_exact_stamps_deadline_admits_a_hook() {
        let written = |at: SystemTime, within| Written {
            line: 0,
            at,
            until: Some((at, within)),
            events: Vec::new(),
        };
        /// Not 1 ns: Windows' `SystemTime` counts 100 ns intervals and drops a
        /// smaller step (std's `sys/pal/windows/time.rs`, `checked_dur2intervals`).
        const TICK: Duration = Duration::from_micros(1);
        for (within, last_in) in [
            (Duration::ZERO, Duration::ZERO),
            (Duration::from_secs(1), TICK),
        ] {
            let line = written(SystemTime::UNIX_EPOCH + Duration::from_secs(1), within);
            let deadline = line.at + within + DEFAULT_POLL_INTERVAL;
            let hooks = [deadline - last_in, deadline - last_in + TICK]
                .map(|at| written(at, Duration::ZERO));
            assert_eq!(bounds(&hooks, &[line]), [(0, 1)], "{within:?}");
        }
    }

    /// A line with no stamp of its own was written between the stamped lines
    /// around it, so a hook stamped before the next one may precede it.
    #[test]
    fn an_unstamped_line_may_follow_a_hook_stamped_before_the_next_stamp() {
        let poll = u64::try_from(DEFAULT_POLL_INTERVAL.as_millis()).expect("ms");
        // The first hook floors the line's earliest write; the second, past a
        // poll after it, can precede the line only through the next line's
        // stamp.
        let hooks = [
            hook("SessionStart", poll),
            hook("PreToolUse", 2 * poll + 500),
        ];
        let lines = [unstamped(&call(0)), output(2 * poll + 1_000)];
        let merges = drive().merges(&hooks, &lines).expect("codex merges");
        assert_eq!(
            orders(merges),
            [
                vec![(J, 0), (H, 0), (H, 1), (J, 1)],
                vec![(H, 0), (J, 0), (H, 1), (J, 1)],
                vec![(H, 0), (H, 1), (J, 0), (J, 1)],
            ]
        );
    }

    /// A stamped line bounds the unstamped line before it, whether or not it
    /// decodes to anything: a hook past its stamp's poll interval never
    /// precedes that line.
    #[test]
    fn a_stamped_line_with_no_events_still_bounds_the_unstamped_line_before_it() {
        let ms = |d: Duration| u64::try_from(d.as_millis()).expect("ms");
        let poll = ms(DEFAULT_POLL_INTERVAL);
        let stamped_at = poll;
        let eventless = json!({"timestamp": iso(stamped_at)}).to_string();
        let hooks = [
            hook("SessionStart", stamped_at + poll / 2),
            hook("PreToolUse", stamped_at + poll + poll / 2),
        ];
        let lines = [
            unstamped(&call(0)),
            eventless,
            output(stamped_at + 3 * poll),
        ];
        let merges = drive().merges(&hooks, &lines).expect("codex merges");
        assert_eq!(
            orders(merges),
            [
                vec![(J, 0), (H, 0), (H, 1), (J, 2)],
                vec![(H, 0), (J, 0), (H, 1), (J, 2)],
            ]
        );
    }

    /// A line ahead of the file's first stamp lands no sooner than the earliest
    /// stamp either stream records, never at the epoch, where the stale sweep
    /// would reap its slot.
    #[test]
    fn an_unstamped_head_line_lands_no_sooner_than_the_earliest_stamp_either_stream_records() {
        let earliest = crate::state::reducer::STALE_BLOCKED_TIMEOUT + DEFAULT_POLL_INTERVAL;
        let ms = u64::try_from(earliest.as_millis()).expect("ms");
        // The hook's stamp first, then the transcript's, with and without a
        // hook after it.
        for (hooks, first_stamp) in [
            (vec![hook("SessionStart", ms)], ms + 1_000),
            (vec![], ms),
            (vec![hook("SessionStart", ms + 500)], ms),
        ] {
            let lines = vec![unstamped(&call(0)), output(first_stamp)];
            let mut led = 0;
            for merged in drive().merges(&hooks, &lines).expect("codex merges") {
                let first = merged.steps.first().expect("a step");
                if (first.transport, first.line) == (J, 0) {
                    led += 1;
                    let at = SystemTime::UNIX_EPOCH + earliest;
                    assert_eq!(first.at, at, "{:?}", merged.arrivals());
                }
            }
            assert!(led > 0, "{hooks:?}: no order leads with the head line");
        }
    }

    /// A line past the file's last stamp was written at no instant the file
    /// bounds, so any hook may precede it.
    #[test]
    fn a_line_past_the_last_stamp_may_follow_every_hook() {
        let poll = u64::try_from(DEFAULT_POLL_INTERVAL.as_millis()).expect("ms");
        let hooks = [hook("SessionStart", poll), hook("PreToolUse", 3 * poll)];
        let lines = [call(poll), unstamped(&output(0))];
        let merges = drive().merges(&hooks, &lines).expect("codex merges");
        assert_eq!(
            orders(merges),
            [
                vec![(J, 0), (J, 1), (H, 0), (H, 1)],
                vec![(J, 0), (H, 0), (J, 1), (H, 1)],
                vec![(J, 0), (H, 0), (H, 1), (J, 1)],
                vec![(H, 0), (J, 0), (J, 1), (H, 1)],
                vec![(H, 0), (J, 0), (H, 1), (J, 1)],
            ]
        );
    }

    #[test]
    fn the_transcript_keeps_its_file_order() {
        let hooks = [hook("SessionStart", 500), hook("PostToolUse", 3_000)];
        let lines = [call(1_000), output(2_000)];
        let merges = drive().merges(&hooks, &lines).expect("codex merges");
        let got = orders(merges);
        assert_eq!(got.len(), 3, "{got:?}");
        for order in &got {
            let jsonl: Vec<usize> = order
                .iter()
                .filter(|(t, _)| *t == J)
                .map(|(_, l)| *l)
                .collect();
            assert_eq!(jsonl, [0, 1], "{order:?}");
        }
    }

    #[test]
    fn each_event_applies_on_its_lines_transport_when_that_line_arrives() {
        let hooks = [hook("SessionStart", 1_000), hook("PostToolUse", 3_000)];
        let lines = [call(2_000)];
        let merged: Vec<Merged> = drive()
            .merges(&hooks, &lines)
            .expect("codex merges")
            .collect();
        let last = merged.last().expect("an order");
        let hook = last
            .steps
            .iter()
            .find(|s| s.transport == H && s.line == 1)
            .expect("the second hook's step");
        assert_eq!(
            hook.at,
            SystemTime::UNIX_EPOCH + Duration::from_millis(3_000)
        );
        let late_call = last.steps.last().expect("a step");
        assert_eq!(late_call.transport, J);
        assert_eq!(
            late_call.at, hook.at,
            "a line arriving behind a hook arrives no sooner than it"
        );
        assert!(
            last.steps
                .iter()
                .any(|s| matches!(s.event, AgentEvent::SessionStart { .. }) && s.transport == J),
            "the seed rides in with the transcript's first line"
        );
        assert_eq!(last.driven.events.len(), last.steps.len());
    }

    /// The transcript lines whose steps a Jsonl `SessionStart` leads.
    fn led_by_a_start(merged: &Merged) -> Vec<usize> {
        let (mut led, mut last) = (Vec::new(), None);
        for s in merged.steps.iter().filter(|s| s.transport == J) {
            if last != Some(s.line) && matches!(s.event, AgentEvent::SessionStart { .. }) {
                led.push(s.line);
            }
            last = Some(s.line);
        }
        led
    }

    /// A seeded drive leads each transcript line that shows life with the
    /// watcher's registration: each line arrives alone, a read of its own.
    #[test]
    fn a_seeded_drive_registers_ahead_of_each_line_that_lives() {
        let (hooks, lines) = ([hook("SessionStart", 1_000)], [call(2_000), output(3_000)]);
        for merged in drive().merges(&hooks, &lines).expect("codex merges") {
            let registered = (led_by_a_start(&merged), merged.driven.seeds.len());
            assert_eq!(registered, (vec![0, 1], 2), "{:?}", merged.steps);
        }
    }

    /// A grok `updates.jsonl` line written at `secs`, as the recorded ones are.
    fn grok_line(secs: u64, method: &str, update: Value) -> String {
        let params = json!({"sessionId": "", "update": update, "_meta": {}});
        json!({"timestamp": secs, "method": method, "params": params}).to_string()
    }

    /// A line that ends its own session registers nothing, and a life after an
    /// end registers again.
    #[test]
    fn an_ending_line_registers_nothing_and_a_life_after_it_registers_again() {
        let drive = Drive::transcript("grok", "/h/.grok/sessions/-repo/s-1/updates.jsonl")
            .expect("grok has a transcript")
            .seeded();
        let tool = |secs| {
            let call =
                json!({"sessionUpdate": "tool_call", "toolCallId": "call_1", "title": "read_file"});
            grok_line(secs, "session/update", call)
        };
        let end = |secs| {
            let end = json!({"sessionUpdate": "hook_execution", "event_name": "session_end"});
            grok_line(secs, "_x.ai/session/update", end)
        };
        let recordings = [
            (vec![tool(1), end(2), tool(3)], vec![0, 2], 2),
            (vec![end(1), tool(2)], vec![1], 1),
        ];
        for (lines, led, seeds) in recordings {
            for merged in drive.merges(&[], &lines).expect("grok merges") {
                let registered = (led_by_a_start(&merged), merged.driven.seeds.len());
                assert_eq!(registered, (led.clone(), seeds), "{:?}", merged.steps);
            }
        }
    }

    /// Only a seeded drive leads its merges with the watcher's registration:
    /// a transcript that carries its own start would get a duplicate.
    #[test]
    fn an_unseeded_drives_merges_carry_no_seed() {
        let unseeded = Drive::transcript("codex", &rollout()).expect("codex has a transcript");
        let (hooks, lines) = ([hook("SessionStart", 1_000)], [call(2_000)]);
        for merged in unseeded.merges(&hooks, &lines).expect("codex merges") {
            let seeds = (merged.steps.iter())
                .filter(|s| s.transport == J && matches!(s.event, AgentEvent::SessionStart { .. }));
            assert_eq!(seeds.count(), 0, "{:?}", merged.steps);
        }
    }

    #[test]
    fn an_epoch_microseconds_stamp_is_its_instant() {
        let (at, within) =
            stamp_of(&json!(1_795_429_878_652_539_u64), Spelling::EpochMicros).expect("a stamp");
        assert_eq!(at, Duration::from_micros(1_795_429_878_652_539));
        assert_eq!(within, Duration::ZERO);
        assert_eq!(
            stamp_of(&json!("1795429878652539"), Spelling::EpochMicros),
            None
        );
    }

    #[test]
    fn a_whole_second_rfc3339_stamp_spans_a_second_either_side() {
        let (at, within) =
            stamp_of(&json!("2026-11-23T04:47:14Z"), Spelling::Rfc3339Secs).expect("a stamp");
        assert_eq!(at, Duration::from_secs(1_795_409_233));
        assert_eq!(within, Duration::from_secs(2));
        assert_eq!(
            stamp_of(&json!(1_795_409_234_u64), Spelling::Rfc3339Secs),
            None
        );
    }

    /// The shim (`pixtuoid-hook`) is the key's one writer, a crate this one
    /// cannot import from, so its own recorded output pins the spelling.
    #[test]
    fn a_recorded_envelope_carries_the_forwarded_at_stamp_by_this_key() {
        let hooks = crate::harness::sources_root()
            .join("codex/fixtures/tool-run-hermetic")
            .join(crate::harness::HOOK_PAYLOADS);
        let text = std::fs::read_to_string(&hooks).expect("a recorded hook capture");
        for line in text.lines() {
            let v: Value = serde_json::from_str(line).expect("json");
            assert!(
                v[FORWARDED_AT_STAMP].as_u64().is_some_and(|ms| ms > 0),
                "{line}"
            );
        }
    }

    #[test]
    fn a_drive_with_no_write_stamp_to_order_by_has_no_merge() {
        let none: [&str; 0] = [];
        assert!(Drive::hooks().merges(&none, &none).is_none());
        let copilot = "/h/.copilot/session-state/65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3/events.jsonl";
        assert!(
            Drive::transcript("copilot", copilot)
                .expect("copilot has a transcript")
                .merges(&none, &none)
                .is_none()
        );
    }
}
