//! The agents' tool calls: one record per call id, whose phase is the max of
//! its facts within a generation, so a call's facts merge in whatever order and
//! from whichever transport they arrive. A generation opens only where copy ids
//! mark a reused call id on one ordered stream ([`CallLedger::observe`]), whose
//! order no merge reorders. A round opens the same way, where gates name their
//! requests. The reducer gates each of a call's state changes on the rise
//! `observe` reports, but two answers to a wait: an approval, once, since an
//! answered member leaves its request, and a call's first start on the
//! transport that raised a keyless wait. A fact that does not raise its record
//! otherwise only merges idempotently (its registration, its count flag), so a
//! fact applied twice applies once. Its registration is all the agent's
//! requests bind, its settle shows and its delegation reads of the call.

use std::collections::HashMap;
use std::sync::Arc;

use crate::AgentId;
use crate::source::{AgentEvent, CopyId, Resolution, ToolDetail, Transport};
use crate::state::{ToolKind, ToolUse, TurnKey};

/// How far a call got. An end outranks every open step, so no fact of its
/// generation raises the call again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Open {
        /// The round its gates reached: a request id its call meets after a
        /// resume or after another id opens the next, so a re-asked call waits
        /// again past its resume, and a replayed gate rises past nothing.
        round: usize,
        step: Step,
    },
    Ended,
}

/// What a fact says of its call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fact<'a> {
    /// It is about to run, under its tool's wire name and its own turn where
    /// its wire names them.
    PreUse {
        tool_name: Option<&'a str>,
        turn: Option<&'a TurnKey>,
    },
    /// It started, under its copy id where its wire names one.
    Start {
        copy: Option<&'a CopyId>,
        detail: Option<&'a ToolDetail>,
    },
    /// It waits on the user, under the request's own id where its wire names
    /// one.
    Gate(Option<&'a str>),
    /// The user approved it, and it runs on.
    Resume,
    /// It ended.
    End,
}

impl<'a> Fact<'a> {
    /// The call `event` names and what it says of it; `None` for an event
    /// that moves no call's phase. Every variant is sorted here, so a new one
    /// fails to compile until it is.
    pub(crate) fn of(event: &'a AgentEvent) -> Option<(&'a str, Self)> {
        let fact = match event {
            AgentEvent::ToolUseStart {
                tool_use_id: Some(id),
                copy_id,
                detail,
                ..
            } => (
                id,
                Fact::Start {
                    copy: copy_id.as_ref(),
                    detail: detail.as_ref(),
                },
            ),
            AgentEvent::ToolUseEnd {
                tool_use_id: Some(id),
                ..
            } => (id, Fact::End),
            AgentEvent::ToolUsePending {
                tool_use_id,
                tool_name,
                turn,
                ..
            } => (
                tool_use_id,
                Fact::PreUse {
                    tool_name: tool_name.as_deref(),
                    turn: turn.as_ref(),
                },
            ),
            AgentEvent::UserActionRequest {
                tool_use_id: Some(id),
                request_id,
                ..
            } => (id, Fact::Gate(request_id.as_deref())),
            AgentEvent::UserActionResolved {
                by: Resolution::Approved(id),
                ..
            } => (id, Fact::Resume),
            AgentEvent::UserActionResolved {
                by: Resolution::Turn(_),
                ..
            }
            | AgentEvent::ToolUseStart {
                tool_use_id: None, ..
            }
            | AgentEvent::ToolUseEnd {
                tool_use_id: None, ..
            }
            | AgentEvent::UserActionRequest {
                tool_use_id: None, ..
            }
            | AgentEvent::SessionStart { .. }
            | AgentEvent::TurnStart { .. }
            | AgentEvent::TurnEnd { .. }
            | AgentEvent::Rename { .. }
            | AgentEvent::LabelHint { .. }
            | AgentEvent::SessionEnd { .. }
            | AgentEvent::ProofOfLife { .. }
            | AgentEvent::Identity { .. }
            | AgentEvent::ModelInfo { .. }
            | AgentEvent::Usage { .. }
            | AgentEvent::Backlog { .. } => return None,
        };
        Some((fact.0.as_str(), fact.1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Step {
    PreUse,
    Started,
    Gated,
    Resumed,
}

impl Phase {
    /// `fact`'s phase before any request id opened a round.
    fn of(fact: Fact<'_>) -> Self {
        let step = match fact {
            Fact::PreUse { .. } => Step::PreUse,
            Fact::Start { .. } => Step::Started,
            Fact::Gate(_) => Step::Gated,
            Fact::Resume => Step::Resumed,
            Fact::End => return Self::Ended,
        };
        Self::Open { round: 0, step }
    }
}

/// What observing one fact did to its call's record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Observed {
    /// It raised its call's record into its own phase, or opened the call's
    /// next generation.
    pub(crate) rose: bool,
    /// It is the call's first start, or its next generation's: the call ran,
    /// so it counts, a start after its end included.
    pub(crate) counts: bool,
    /// The call has ended.
    pub(crate) ended: bool,
    /// It is a pre-use or start of the open call, which registered it pending.
    pub(crate) registered: bool,
    /// It is a start of the open call on a transport none of its starts came
    /// on before.
    pub(crate) first_here: bool,
    /// It ended a Task its agent dispatched.
    pub(crate) drained: bool,
}

#[derive(Debug, Clone)]
struct Call {
    phase: Phase,
    /// The copy ids its starts named, one per generation.
    copies: Vec<CopyId>,
    counted: bool,
    /// The request ids its generation's gates named, each with the round it
    /// opened or joined.
    requests: Vec<(Arc<str>, usize)>,
    /// What its open generation's pre-use and starts said; its end clears it.
    registration: Registration,
}

impl Call {
    /// `fact`'s phase on this call: a gate's round is the one its request id
    /// opened or joined, and a resume resumes the round the call is in.
    fn phase_of(&mut self, fact: Fact<'_>) -> Phase {
        let round = match (fact, self.phase) {
            (Fact::Gate(Some(id)), phase) => match self.requests.iter().find(|(r, _)| &**r == id) {
                Some(&(_, round)) => round,
                None => {
                    let round = match phase {
                        // A first id before any resume is of the call's first
                        // round, joining a wait a gate naming none raised: omp's
                        // ask block, then the request a `tools.approval.ask:
                        // prompt` override records, honored in every mode,
                        // though no mode gates a `read` tool like ask
                        // (`packages/coding-agent/src/tools/ask.ts:400`,
                        // `docs/approval-mode.md:24-29`, `:72` at v19.1.0).
                        Phase::Open { round, step }
                            if self.requests.is_empty() && step != Step::Resumed =>
                        {
                            round
                        }
                        Phase::Open { round, .. } => round + 1,
                        Phase::Ended => 0,
                    };
                    self.requests.push((Arc::from(id), round));
                    round
                }
            },
            (Fact::Resume, Phase::Open { round, .. }) => round,
            _ => 0,
        };
        match Phase::of(fact) {
            Phase::Open { step, .. } => Phase::Open { round, step },
            Phase::Ended => Phase::Ended,
        }
    }

    /// Whether its first start dispatched a Task that has not ended.
    fn is_task(&self) -> bool {
        self.registration
            .shows
            .as_ref()
            .is_some_and(|(_, item)| item.kind == ToolKind::Task)
    }

    fn end(&mut self) {
        self.phase = Phase::Ended;
        self.registration = Registration::default();
    }
}

/// What an open call's pre-use and starts said of it.
#[derive(Debug, Clone, Default)]
struct Registration {
    /// Its tool's wire name, which a request naming that tool binds it by.
    tool_name: Option<Arc<str>>,
    /// Its own turn, else the agent's open one.
    turn: Option<TurnKey>,
    /// What its first start shows, and that start's place among its agent's.
    shows: Option<(u64, ToolUse)>,
    /// The transports its pre-use and starts came on.
    seen_on: Vec<Transport>,
    /// The transports its starts came on.
    started_on: Vec<Transport>,
    /// A Task whose turn ended: its subagent runs on, past every turn's
    /// requests.
    outlived_turn: bool,
}

/// One agent's calls.
#[derive(Debug, Default, Clone)]
struct Calls {
    by_id: HashMap<Arc<str>, Call>,
    /// The calls a pre-use or start registered that have not ended, oldest
    /// first: what a request binds and a settle lands on, read without walking
    /// every call the slot's life kept.
    pending: Vec<Arc<str>>,
    /// The place of the next call to start.
    next_start: u64,
}

impl Calls {
    /// `call` met `fact`: its phase rises to the fact's, or its record or next
    /// generation opens.
    fn advance(&mut self, call: &str, fact: Fact<'_>) -> Observed {
        let (starts, copy) = match fact {
            Fact::Start { copy, .. } => (true, copy),
            _ => (false, None),
        };
        let Some(record) = self.by_id.get_mut(call) else {
            let mut record = Call {
                phase: Phase::Open {
                    round: 0,
                    step: Step::PreUse,
                },
                copies: copy.cloned().into_iter().collect(),
                counted: starts,
                requests: Vec::new(),
                registration: Registration::default(),
            };
            record.phase = record.phase_of(fact);
            let ended = record.phase == Phase::Ended;
            self.by_id.insert(Arc::from(call), record);
            return Observed {
                rose: true,
                counts: starts,
                ended,
                registered: false,
                first_here: false,
                drained: false,
            };
        };
        // A provider reusing an id names a new call (the `copy_id` comment in
        // `source/omp.rs`'s `decode_omp_line`). Only omp's copy ids can differ
        // for one call id, and every fact of its calls rides its one
        // transcript; claude-code's copy id is the call id itself.
        let generation = record.phase == Phase::Ended
            && copy.is_some_and(|c| !record.copies.is_empty() && !record.copies.contains(c));
        if let Some(c) = copy
            && !record.copies.contains(c)
        {
            record.copies.push(c.clone());
        }
        if generation {
            record.phase = Phase::of(fact);
            record.requests.clear();
            return Observed {
                rose: true,
                counts: true,
                ended: false,
                registered: false,
                first_here: false,
                drained: false,
            };
        }
        let counts = starts && !record.counted;
        record.counted |= starts;
        let into = record.phase_of(fact);
        let rose = into > record.phase;
        let ends = rose && into == Phase::Ended;
        let drained = ends && record.is_task();
        let ended = ends || record.phase == Phase::Ended;
        if ends {
            self.end(call);
        } else if rose {
            record.phase = into;
        }
        Observed {
            rose,
            counts,
            ended,
            registered: false,
            first_here: false,
            drained,
        }
    }

    /// `call` ended: its record drops its registration, and it leaves the
    /// pending calls.
    fn end(&mut self, call: &str) {
        if let Some(record) = self.by_id.get_mut(call) {
            record.end();
        }
        self.pending.retain(|c| **c != *call);
    }

    /// `call`, open, met `fact` on `on` while `open_turn` is open: a pre-use or
    /// start registers it pending and merges what it says. Whether it is a
    /// start on a transport none of the call's starts came on before; `None`
    /// for a fact that registers nothing.
    fn register(
        &mut self,
        call: &str,
        fact: Fact<'_>,
        on: Transport,
        open_turn: Option<&TurnKey>,
    ) -> Option<bool> {
        let (tool_name, turn, detail) = match fact {
            Fact::PreUse { tool_name, turn } => (tool_name, turn.or(open_turn), None),
            Fact::Start { detail, .. } => (None, open_turn, Some(detail)),
            Fact::Gate(_) | Fact::Resume | Fact::End => return None,
        };
        let Self {
            by_id,
            pending,
            next_start,
        } = self;
        let key = Arc::clone(by_id.get_key_value(call)?.0);
        if !pending.contains(&key) {
            pending.push(Arc::clone(&key));
        }
        let record = by_id.get_mut(call)?;
        let p = &mut record.registration;
        if p.tool_name.is_none() {
            p.tool_name = tool_name.map(Arc::from);
        }
        // A call first seen while no turn was open (an oversized skip, a late
        // attach) takes the turn a later fact names.
        if p.turn.is_none() {
            p.turn = turn.cloned();
        }
        if !p.seen_on.contains(&on) {
            p.seen_on.push(on);
        }
        let Some(detail) = detail else {
            return Some(false);
        };
        // A call's first start makes it the newest in flight.
        if p.shows.is_none() {
            p.shows = Some((*next_start, ToolUse::started(Some(key), detail)));
            *next_start += 1;
        }
        let first_here = !p.started_on.contains(&on);
        if first_here {
            p.started_on.push(on);
        }
        Some(first_here)
    }
}

/// A pending call, as a request binds it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingCall<'a> {
    pub(crate) call: &'a Arc<str>,
    /// Its tool's wire name.
    pub(crate) tool_name: Option<&'a str>,
    seen_on: &'a [Transport],
}

impl PendingCall<'_> {
    /// Whether its pre-use or a start of it came on `on`.
    pub(crate) fn seen_on(&self, on: Transport) -> bool {
        self.seen_on.contains(&on)
    }
}

/// Every agent's calls, each kept for its slot's life: a replay of any age
/// meets its call's record.
#[derive(Debug, Default, Clone)]
pub(crate) struct CallLedger {
    calls: HashMap<AgentId, Calls>,
}

impl CallLedger {
    /// `agent`'s `call` met `fact` on `on`, while `open_turn` is the agent's
    /// open turn. A start under a copy id its ended record never held, while
    /// it holds another, opens the call's next generation, and a held copy id
    /// is that call's replay. A pre-use or start registers its open call
    /// pending whatever its rise: a start decoded after its gate (#242) is
    /// below its record's phase, yet its call runs.
    pub(crate) fn observe(
        &mut self,
        agent: AgentId,
        call: &str,
        fact: Fact<'_>,
        on: Transport,
        open_turn: Option<&TurnKey>,
    ) -> Observed {
        let calls = self.calls.entry(agent).or_default();
        let mut observed = calls.advance(call, fact);
        if !observed.ended
            && let Some(first_here) = calls.register(call, fact, on, open_turn)
        {
            observed.registered = true;
            observed.first_here = first_here;
        }
        observed
    }

    /// `agent`'s `call` ended by an end the suppression of its child's hooks
    /// dropped before any record met it; a call the ledger never met stays
    /// unrecorded.
    pub(crate) fn end(&mut self, agent: &AgentId, call: &str) {
        if let Some(calls) = self.calls.get_mut(agent) {
            calls.end(call);
        }
    }

    /// The turns of `agent`'s that `closes` names ended, and their pending
    /// calls with them, but a Task, whose subagent runs on past its turn: it
    /// leaves the turn's requests and stays in flight until its own end.
    pub(crate) fn end_turn(&mut self, agent: &AgentId, closes: impl Fn(Option<&TurnKey>) -> bool) {
        let Some(calls) = self.calls.get_mut(agent) else {
            return;
        };
        let closed: Vec<Arc<str>> = (calls.pending.iter())
            .filter(|call| {
                (calls.by_id.get(*call)).is_some_and(|r| closes(r.registration.turn.as_ref()))
            })
            .cloned()
            .collect();
        for call in closed {
            match calls.by_id.get_mut(&call) {
                Some(record) if record.is_task() => record.registration.outlived_turn = true,
                _ => calls.end(&call),
            }
        }
    }

    /// `agent`'s slot revived: every call its dead life left open ended with
    /// it, and each record stays, so that life's replays stay refused.
    pub(crate) fn end_life(&mut self, agent: &AgentId) {
        if let Some(calls) = self.calls.get_mut(agent) {
            calls.by_id.values_mut().for_each(Call::end);
            calls.pending.clear();
        }
    }

    /// `agent`'s slot went, and its calls with it.
    pub(crate) fn forget(&mut self, agent: &AgentId) {
        self.calls.remove(agent);
    }

    /// `agent`'s pending calls in `turn`, but a Task that outlived its turn.
    pub(crate) fn pending_in<'a>(
        &'a self,
        agent: &AgentId,
        turn: Option<&'a TurnKey>,
    ) -> impl Iterator<Item = PendingCall<'a>> + use<'a> {
        let calls = self.calls.get(agent);
        calls.into_iter().flat_map(move |calls| {
            calls.pending.iter().filter_map(move |call| {
                let p = &calls.by_id.get(call)?.registration;
                (!p.outlived_turn && p.turn.as_ref() == turn).then_some(PendingCall {
                    call,
                    tool_name: p.tool_name.as_deref(),
                    seen_on: &p.seen_on,
                })
            })
        })
    }

    /// What `agent`'s `call`'s first start showed, while it has not ended.
    pub(crate) fn in_flight(&self, agent: &AgentId, call: &str) -> Option<ToolUse> {
        let (_, item) = self
            .calls
            .get(agent)?
            .by_id
            .get(call)?
            .registration
            .shows
            .as_ref()?;
        Some(item.clone())
    }

    /// The item of `agent`'s newest call that started and has not ended.
    pub(crate) fn newest_in_flight(&self, agent: &AgentId) -> Option<ToolUse> {
        let calls = self.calls.get(agent)?;
        let (_, item) = calls
            .pending
            .iter()
            .filter_map(|call| calls.by_id.get(call)?.registration.shows.as_ref())
            .max_by_key(|(at, _)| *at)?;
        Some(item.clone())
    }

    /// Whether `call` is a Task `agent` dispatched that has not ended.
    pub(crate) fn is_task_in_flight(&self, agent: &AgentId, call: &str) -> bool {
        self.calls
            .get(agent)
            .and_then(|calls| calls.by_id.get(call))
            .is_some_and(Call::is_task)
    }

    /// Whether `agent` dispatched a Task that has not ended.
    pub(crate) fn any_task_in_flight(&self, agent: &AgentId) -> bool {
        self.calls.get(agent).is_some_and(|calls| {
            calls
                .pending
                .iter()
                .any(|call| calls.by_id.get(call).is_some_and(Call::is_task))
        })
    }

    /// How many calls it keeps.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.calls.values().map(|calls| calls.by_id.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const HOOK: Transport = Transport::Hook;
    const JSONL: Transport = Transport::Jsonl;

    fn agent() -> AgentId {
        AgentId::from_parts("claude-code", "s")
    }

    fn turn(k: &str) -> TurnKey {
        TurnKey::Id(k.into())
    }

    const PRE_USE: Fact<'static> = Fact::PreUse {
        tool_name: None,
        turn: None,
    };

    fn start(copy: Option<&CopyId>) -> Fact<'_> {
        Fact::Start { copy, detail: None }
    }

    /// `fact` of `call`, on the hooks while no turn is open.
    fn observe(ledger: &mut CallLedger, call: &str, fact: Fact<'_>) -> Observed {
        ledger.observe(agent(), call, fact, HOOK, None)
    }

    /// A fact by index, its start naming the one copy id `c` or none and its
    /// gate no request, so no start opens a generation and no gate a round.
    fn fact(n: u8, c: &CopyId) -> Fact<'_> {
        match n {
            0 => PRE_USE,
            1 => start(None),
            2 => start(Some(c)),
            3 => Fact::Gate(None),
            4 => Fact::Resume,
            _ => Fact::End,
        }
    }

    fn phase(ledger: &CallLedger) -> Option<Phase> {
        Some(ledger.calls.get(&agent())?.by_id.get("c")?.phase)
    }

    proptest! {
        #[test]
        fn a_records_phase_is_the_max_of_its_facts_in_any_order(
            facts in proptest::collection::vec(0u8..6, 1..10),
        ) {
            let c = CopyId::new("c");
            let mut ledger = CallLedger::default();
            for &n in &facts {
                let before = phase(&ledger);
                let observed = observe(&mut ledger, "c", fact(n, &c));
                prop_assert_eq!(observed.rose, phase(&ledger) != before);
            }
            let max = facts.iter().map(|&n| Phase::of(fact(n, &c))).max();
            prop_assert_eq!(phase(&ledger), max);
        }

        #[test]
        fn a_fact_applied_twice_applies_once(
            facts in proptest::collection::vec(0u8..6, 1..10),
        ) {
            let c = CopyId::new("c");
            let mut ledger = CallLedger::default();
            for &n in &facts {
                observe(&mut ledger, "c", fact(n, &c));
                let once = format!("{ledger:?}");
                let again = observe(&mut ledger, "c", fact(n, &c));
                prop_assert!(!again.rose);
                prop_assert!(!again.counts);
                prop_assert!(!again.first_here);
                prop_assert_eq!(format!("{ledger:?}"), once);
            }
        }
    }

    #[test]
    fn a_call_counts_at_its_first_start_even_after_its_end() {
        let mut ledger = CallLedger::default();
        let end = observe(&mut ledger, "c", Fact::End);
        assert_eq!((end.rose, end.counts), (true, false));
        let started = observe(&mut ledger, "c", start(None));
        assert_eq!((started.rose, started.counts), (false, true));
        assert!(!observe(&mut ledger, "c", start(None)).counts);
    }

    #[test]
    fn a_start_under_a_copy_id_its_ended_call_never_held_opens_its_next_generation() {
        let (first, second) = (CopyId::new("e1@t#0"), CopyId::new("e2@t#0"));
        let mut ledger = CallLedger::default();
        observe(&mut ledger, "c", start(Some(&first)));
        observe(&mut ledger, "c", Fact::End);
        let replay = observe(&mut ledger, "c", start(Some(&first)));
        assert_eq!((replay.rose, replay.counts), (false, false), "a replay");
        let next = observe(&mut ledger, "c", start(Some(&second)));
        assert_eq!(
            next,
            Observed {
                rose: true,
                counts: true,
                ended: false,
                registered: true,
                first_here: true,
                drained: false,
            }
        );
        assert!(
            !observe(&mut ledger, "c", start(Some(&second))).rose,
            "the new generation's replay"
        );
    }

    /// A hook start names no copy id and a transcript's names the call's own
    /// (claude-code's), so its echo after the end opens no generation.
    #[test]
    fn a_first_copy_id_after_the_end_opens_no_generation() {
        let copy = CopyId::new("toolu_c");
        let mut ledger = CallLedger::default();
        observe(&mut ledger, "c", start(None));
        observe(&mut ledger, "c", Fact::End);
        let echo = observe(&mut ledger, "c", start(Some(&copy)));
        assert_eq!((echo.rose, echo.ended), (false, true));
    }

    #[test]
    fn a_second_gate_on_a_resumed_call_waits_again() {
        let mut ledger = CallLedger::default();
        observe(&mut ledger, "c", start(None));
        assert!(observe(&mut ledger, "c", Fact::Gate(Some("g1"))).rose);
        assert!(observe(&mut ledger, "c", Fact::Resume).rose);
        assert_eq!(
            observe(&mut ledger, "c", Fact::Gate(Some("g2"))),
            Observed {
                rose: true,
                counts: false,
                ended: false,
                registered: false,
                first_here: false,
                drained: false,
            }
        );
        assert!(
            observe(&mut ledger, "c", Fact::Resume).rose,
            "its own resume"
        );
    }

    #[test]
    fn a_replayed_gate_adds_no_round() {
        let mut ledger = CallLedger::default();
        observe(&mut ledger, "c", Fact::Gate(Some("g1")));
        observe(&mut ledger, "c", Fact::Resume);
        for replay in [Fact::Gate(Some("g1")), Fact::Gate(None), Fact::Resume] {
            assert!(!observe(&mut ledger, "c", replay).rose, "{replay:?}");
        }
    }

    /// omp's ask raises its wait from its block, naming no request, and a
    /// `tools.approval.ask: prompt` override then records a request for that
    /// same wait.
    #[test]
    fn a_first_request_id_joins_the_wait_a_gate_naming_none_raised() {
        let mut ledger = CallLedger::default();
        observe(&mut ledger, "c", start(None));
        assert!(observe(&mut ledger, "c", Fact::Gate(None)).rose);
        assert!(!observe(&mut ledger, "c", Fact::Gate(Some("g1"))).rose);
        assert!(observe(&mut ledger, "c", Fact::Resume).rose);
        assert!(
            observe(&mut ledger, "c", Fact::Gate(Some("g2"))).rose,
            "a re-ask"
        );
    }

    #[test]
    fn a_first_request_id_after_a_resume_waits_again() {
        let mut ledger = CallLedger::default();
        for fact in [start(None), Fact::Gate(None), Fact::Resume] {
            observe(&mut ledger, "c", fact);
        }
        assert!(observe(&mut ledger, "c", Fact::Gate(Some("g1"))).rose);
        assert!(!observe(&mut ledger, "c", Fact::Gate(Some("g1"))).rose);
    }

    #[test]
    fn a_request_id_after_another_waits_again() {
        let mut ledger = CallLedger::default();
        observe(&mut ledger, "c", start(None));
        assert!(observe(&mut ledger, "c", Fact::Gate(Some("g1"))).rose);
        assert!(observe(&mut ledger, "c", Fact::Gate(Some("g2"))).rose);
        assert!(!observe(&mut ledger, "c", Fact::Gate(Some("g1"))).rose);
    }

    #[test]
    fn a_generation_starts_its_rounds_afresh() {
        let (first, second) = (CopyId::new("e1@t#0"), CopyId::new("e2@t#0"));
        let mut ledger = CallLedger::default();
        for fact in [
            start(Some(&first)),
            Fact::Gate(None),
            Fact::Gate(Some("g1")),
            Fact::End,
            start(Some(&second)),
        ] {
            observe(&mut ledger, "c", fact);
        }
        assert!(observe(&mut ledger, "c", Fact::Gate(None)).rose);
        assert!(!observe(&mut ledger, "c", Fact::Gate(Some("g2"))).rose);
    }

    #[test]
    fn a_revived_slot_ends_its_dead_lifes_calls_and_keeps_them() {
        let mut ledger = CallLedger::default();
        observe(&mut ledger, "c", start(None));
        ledger.end_life(&agent());
        assert_eq!(ledger.newest_in_flight(&agent()), None);
        let replay = observe(&mut ledger, "c", start(None));
        assert_eq!((replay.rose, replay.ended), (false, true));
        ledger.forget(&agent());
        assert_eq!(ledger.len(), 0);
    }

    fn newest(ledger: &CallLedger) -> Option<String> {
        Some(ledger.newest_in_flight(&agent())?.tool_use_id?.to_string())
    }

    #[test]
    fn the_newest_call_in_flight_is_the_last_to_start_and_not_end() {
        let mut ledger = CallLedger::default();
        observe(&mut ledger, "a", PRE_USE);
        assert_eq!(newest(&ledger), None, "a pre-use starts nothing");
        observe(&mut ledger, "b", start(None));
        observe(&mut ledger, "a", start(None));
        assert_eq!(
            newest(&ledger).as_deref(),
            Some("a"),
            "a's start came last, though its pre-use came first"
        );
        ledger.observe(agent(), "b", start(None), JSONL, None);
        assert_eq!(
            newest(&ledger).as_deref(),
            Some("a"),
            "a second start of b is no newer"
        );
        observe(&mut ledger, "a", Fact::End);
        assert_eq!(newest(&ledger).as_deref(), Some("b"));
        observe(&mut ledger, "b", Fact::End);
        assert_eq!(newest(&ledger), None);
    }

    /// A pre-use and its start, or the two transports' starts of one call,
    /// are one pending call, its name and turn the first that named them.
    #[test]
    fn a_calls_pre_use_and_starts_merge_into_one_pending_call() {
        let t1 = turn("t1");
        let mut ledger = CallLedger::default();
        let bash = Fact::PreUse {
            tool_name: Some("Bash"),
            turn: None,
        };
        assert!(
            !ledger
                .observe(agent(), "a", bash, HOOK, Some(&t1))
                .first_here
        );
        assert!(
            ledger
                .observe(agent(), "a", start(None), HOOK, None)
                .first_here
        );
        assert!(
            !ledger
                .observe(agent(), "a", start(None), HOOK, None)
                .first_here
        );
        assert!(
            ledger
                .observe(agent(), "a", start(None), JSONL, None)
                .first_here
        );
        let [a] = &ledger.pending_in(&agent(), Some(&t1)).collect::<Vec<_>>()[..] else {
            panic!("one pending call");
        };
        assert_eq!((&**a.call, a.tool_name), ("a", Some("Bash")));
        assert!(a.seen_on(HOOK) && a.seen_on(JSONL));
    }

    /// A turn's end ends its calls and those no turn scoped when it says so,
    /// but a Task, which delegates on outside any turn's requests.
    #[test]
    fn a_turn_end_ends_its_turns_calls_but_a_task() {
        let (t1, t2) = (turn("t1"), turn("t2"));
        let task = ToolDetail::Task;
        let mut ledger = CallLedger::default();
        for (call, open) in [("a", Some(&t1)), ("b", Some(&t2)), ("c", None)] {
            ledger.observe(agent(), call, start(None), HOOK, open);
        }
        let dispatch = Fact::Start {
            copy: None,
            detail: Some(&task),
        };
        ledger.observe(agent(), "task", dispatch, HOOK, Some(&t1));
        let pending = |ledger: &CallLedger, turn| -> Vec<String> {
            let mut calls: Vec<String> = (ledger.pending_in(&agent(), turn))
                .map(|p| p.call.to_string())
                .collect();
            calls.sort();
            calls
        };
        ledger.end_turn(&agent(), |t| t == Some(&t1));
        assert_eq!(pending(&ledger, Some(&t1)), Vec::<String>::new());
        assert!(
            !observe(&mut ledger, "a", Fact::End).rose,
            "a ended with t1"
        );
        assert!(ledger.is_task_in_flight(&agent(), "task"));
        assert_eq!(pending(&ledger, Some(&t2)), ["b"]);
        ledger.end_turn(&agent(), |t| t.is_none());
        assert_eq!(pending(&ledger, None), Vec::<String>::new());
        assert_eq!(newest(&ledger).as_deref(), Some("task"));
        assert!(observe(&mut ledger, "task", Fact::End).drained);
        assert!(!ledger.any_task_in_flight(&agent()));
    }
}
