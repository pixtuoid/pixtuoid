//! One session's two streams folded in every order they can reach the reducer
//! in: a hook at its shim stamp, and a transcript line no sooner than its write
//! stamp and at most the watcher's `DEFAULT_POLL_INTERVAL` after it, behind
//! every line above it, since the watcher reads a file in order.

use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::reads::{Spelling, WRITE_STAMPS};
use super::{BreadcrumbSink, CASCADE_TICK, Decode, Drive, Driven, note_reached};
use crate::source::DEFAULT_POLL_INTERVAL;
use crate::source::decoder::rfc3339_to_epoch;
use crate::state::ActivityState;
use crate::{AgentEvent, Reducer, Transport};

/// What the shim stamps on every envelope: when it forwarded it, in
/// milliseconds since the epoch. Pinned by
/// `each_key_the_shim_stamps_is_the_one_core_reads` to the shim's copy.
pub const SHIM_STAMP: &str = "_shim_ts_ms";

/// One event a merge applied.
#[derive(Debug, Clone)]
pub struct Step {
    /// The stream its line rode.
    pub transport: Transport,
    /// Its line's index among its stream's non-blank lines, from 0. The
    /// first-sight seed rides in with the transcript's first line.
    pub line: usize,
    /// When it reached the reducer.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "its test pins the arrival rule")
    )]
    pub(crate) at: SystemTime,
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

/// A line that decoded to events, and when its CLI wrote it: exactly `at` when
/// `within` is zero, else in `at..at + within`.
#[derive(Debug)]
struct Written {
    line: usize,
    at: SystemTime,
    within: Duration,
    events: Vec<AgentEvent>,
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
        let seed = self.seed();
        decoded.seed_events = usize::from(seed.is_some());
        let sink = BreadcrumbSink::default();
        let (hooks, transcript) = tracing::subscriber::with_default(sink.clone(), || {
            let hooks = written(&Decode::Hooks, hooks, None, &mut decoded, |v| {
                let ms = v.get(SHIM_STAMP)?.as_u64()?;
                Some((Duration::from_millis(ms), Duration::ZERO))
            });
            let transcript = written(&self.decode, lines, seed, &mut decoded, |v| {
                stamp_of(v.get(key)?, spelling)
            });
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
        })
    }
}

/// The instant `v` spells as `spelling`, and how long after it the write could
/// have been.
fn stamp_of(v: &Value, spelling: Spelling) -> Option<(Duration, Duration)> {
    match spelling {
        Spelling::Rfc3339 => Some((rfc3339_to_epoch(v.as_str()?)?, Duration::ZERO)),
        Spelling::EpochSecs => Some((Duration::from_secs(v.as_u64()?), Duration::from_secs(1))),
    }
}

/// `lines`' lines that decoded to events, each stamped by `stamp`, counted in
/// `d`; `seed` rides in with the first that decoded at all. A line with no
/// stamp of its own was written no sooner than the one above it, and the
/// file's first no sooner than the epoch.
fn written<S: AsRef<str>>(
    decode: &Decode,
    lines: &[S],
    mut seed: Option<AgentEvent>,
    d: &mut Driven,
    stamp: impl Fn(&Value) -> Option<(Duration, Duration)>,
) -> Vec<Written> {
    let mut out = Vec::new();
    let (mut at, mut within) = (SystemTime::UNIX_EPOCH, Duration::ZERO);
    let lines = lines
        .iter()
        .map(AsRef::as_ref)
        .filter(|l| !l.trim().is_empty());
    for (index, line) in lines.enumerate() {
        let parsed = |v: &Value| {
            if let Some((since, w)) = stamp(v) {
                (at, within) = (SystemTime::UNIX_EPOCH + since, w);
            }
        };
        let Some(own) = decode.line(Some(line), d, parsed) else {
            continue;
        };
        let events: Vec<AgentEvent> = seed.take().into_iter().chain(own).collect();
        if !events.is_empty() {
            out.push(Written {
                line: index,
                at,
                within,
                events,
            });
        }
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
            let latest = t.at + t.within + DEFAULT_POLL_INTERVAL;
            let reachable = |h: &&Written| h.at < latest || (t.within.is_zero() && h.at == latest);
            (
                hooks.iter().filter(|h| h.at < t.at).count(),
                hooks.iter().filter(reachable).count(),
            )
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

    /// `order` folded: each line at the earliest instant it can arrive there.
    fn folded(&self, order: &[usize]) -> Merged {
        let mut lines: Vec<(Transport, &Written)> = Vec::new();
        let mut taken = 0;
        for (t, &before) in self.transcript.iter().zip(order) {
            let hooks = self.hooks.get(taken..before).unwrap_or_default();
            lines.extend(hooks.iter().map(|h| (Transport::Hook, h)));
            taken = taken.max(before);
            lines.push((Transport::Jsonl, t));
        }
        let rest = self.hooks.get(taken..).unwrap_or_default();
        lines.extend(rest.iter().map(|h| (Transport::Hook, h)));
        let mut last = SystemTime::UNIX_EPOCH;
        let arrivals: Vec<(Transport, &Written, SystemTime)> = lines
            .into_iter()
            .map(|(transport, w)| {
                last = last.max(w.at);
                (transport, w, last)
            })
            .collect();

        let mut driven = Driven {
            lines: self.decoded.lines,
            unparseable: self.decoded.unparseable,
            decode_errors: self.decoded.decode_errors.clone(),
            seed_events: self.decoded.seed_events,
            ..Driven::empty()
        };
        let mut steps = Vec::new();
        let sink = BreadcrumbSink::default();
        tracing::subscriber::with_default(sink.clone(), || {
            let mut reducer = Reducer::new();
            for &(transport, w, at) in &arrivals {
                reducer.tick(&mut driven.scene, at);
                note_reached(&driven.scene, &mut driven.reaches);
                for event in &w.events {
                    reducer.apply(&mut driven.scene, event.clone(), at, transport);
                    note_reached(&driven.scene, &mut driven.reaches);
                    driven.events.push(event.clone());
                    steps.push(Step {
                        transport,
                        line: w.line,
                        at,
                        event: event.clone(),
                        state: driven
                            .scene
                            .agents
                            .get(&event.agent_id())
                            .map(|s| s.state.clone()),
                    });
                }
            }
            reducer.tick(&mut driven.scene, last + CASCADE_TICK);
            note_reached(&driven.scene, &mut driven.reaches);
        });
        driven.breadcrumbs = self.decoded.breadcrumbs.clone();
        driven.breadcrumbs.extend(sink.take());
        Merged { driven, steps }
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
            SHIM_STAMP: ms,
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

    /// Each order, as the stream and line of each step, a line's run of steps
    /// once.
    fn orders(merges: Merges) -> Vec<Vec<(Transport, usize)>> {
        merges
            .map(|m| {
                let mut lines: Vec<(Transport, usize)> =
                    m.steps.iter().map(|s| (s.transport, s.line)).collect();
                lines.dedup();
                lines
            })
            .collect()
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
            within,
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

    #[test]
    fn an_epoch_seconds_stamp_spans_its_whole_second() {
        let (at, within) =
            stamp_of(&json!(1_793_078_269_u64), Spelling::EpochSecs).expect("a stamp");
        assert_eq!(at, Duration::from_secs(1_793_078_269));
        assert_eq!(within, Duration::from_secs(1));
        assert_eq!(stamp_of(&json!("1793078269"), Spelling::EpochSecs), None);
    }

    /// The shim (`pixtuoid-hook`) is the key's one writer, a crate this one
    /// cannot import from, so its own recorded output pins the spelling.
    #[test]
    fn a_recorded_envelope_carries_the_shim_stamp_by_this_key() {
        let hooks = crate::harness::sources_root()
            .join("fixtures/codex/tool-run-hermetic")
            .join(crate::harness::HOOK_PAYLOADS);
        let text = std::fs::read_to_string(&hooks).expect("a recorded hook capture");
        for line in text.lines() {
            let v: Value = serde_json::from_str(line).expect("json");
            assert!(v[SHIM_STAMP].as_u64().is_some_and(|ms| ms > 0), "{line}");
        }
    }

    #[test]
    fn a_drive_with_no_write_stamp_to_order_by_has_no_merge() {
        let none: [&str; 0] = [];
        assert!(Drive::hooks().merges(&none, &none).is_none());
        let omp = "/h/.omp/agent/sessions/-h-p/2026-09-19T18-16-13-370Z_01a0bae2-133a-769f-851a-927926fbca45.jsonl";
        assert!(
            Drive::transcript("omp", omp)
                .expect("omp has a transcript")
                .merges(&none, &none)
                .is_none()
        );
    }
}
