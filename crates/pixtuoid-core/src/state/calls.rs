//! The agents' tool calls: one record per call id, whose phase is the max of
//! its facts within a generation, so a call's facts merge in whatever order and
//! from whichever transport they arrive. A generation opens only where copy ids
//! mark a reused call id on one ordered stream ([`CallLedger::observe`]), whose
//! order no merge reorders. A round opens the same way, where gates name their
//! requests. The reducer gates each of a call's state changes on the rise
//! `observe` reports, but two answers to a wait: an approval, once, since an
//! answered member leaves its request, and a call's first start on the
//! transport that raised a keyless wait. A fact that does not raise its record
//! otherwise only merges idempotently (its pending fields, a Task's tracking,
//! its count flag), so a fact applied twice applies once.

use std::collections::HashMap;
use std::sync::Arc;

use crate::AgentId;
use crate::source::{AgentEvent, CopyId, Resolution};

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
    /// It is about to run.
    PreUse,
    /// It started, under its copy id where its wire names one.
    Start(Option<&'a CopyId>),
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
                ..
            } => (id, Fact::Start(copy_id.as_ref())),
            AgentEvent::ToolUseEnd {
                tool_use_id: Some(id),
                ..
            } => (id, Fact::End),
            AgentEvent::ToolUsePending { tool_use_id, .. } => (tool_use_id, Fact::PreUse),
            AgentEvent::UserActionRequest {
                tool_use_id: Some(id),
                request_id,
                ..
            } => (id, Fact::Gate(request_id.as_deref())),
            AgentEvent::UserActionResolved {
                by: Resolution::Approved(id),
                ..
            } => (id, Fact::Resume),
            // codex's PostToolUse answers its request and leaves its call's
            // end to the rollout.
            AgentEvent::UserActionResolved {
                by: Resolution::Call(_) | Resolution::Turn(_),
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
            | AgentEvent::Ultracode { .. }
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
            Fact::PreUse => Step::PreUse,
            Fact::Start(_) => Step::Started,
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
}

/// Every agent's calls, each kept for its slot's life: a replay of any age
/// meets its call's record.
#[derive(Debug, Default, Clone)]
pub(crate) struct CallLedger {
    calls: HashMap<AgentId, HashMap<Arc<str>, Call>>,
}

impl CallLedger {
    /// `agent`'s `call` met `fact`. A start under a copy id its ended record
    /// never held, while it holds another, opens the call's next generation,
    /// and a held copy id is that call's replay.
    pub(crate) fn observe(&mut self, agent: AgentId, call: &str, fact: Fact<'_>) -> Observed {
        let (starts, copy) = match fact {
            Fact::Start(copy) => (true, copy),
            _ => (false, None),
        };
        let calls = self.calls.entry(agent).or_default();
        let Some(record) = calls.get_mut(call) else {
            let mut record = Call {
                phase: Phase::of(Fact::PreUse),
                copies: copy.cloned().into_iter().collect(),
                counted: starts,
                requests: Vec::new(),
            };
            record.phase = record.phase_of(fact);
            let ended = record.phase == Phase::Ended;
            calls.insert(Arc::from(call), record);
            return Observed {
                rose: true,
                counts: starts,
                ended,
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
            record.phase = Phase::of(Fact::Start(None));
            record.requests.clear();
            return Observed {
                rose: true,
                counts: true,
                ended: false,
            };
        }
        let counts = starts && !record.counted;
        record.counted |= starts;
        let into = record.phase_of(fact);
        let rose = into > record.phase;
        if rose {
            record.phase = into;
        }
        Observed {
            rose,
            counts,
            ended: record.phase == Phase::Ended,
        }
    }

    /// `agent`'s slot revived: every call its dead life left open ended with
    /// it, and each record stays, so that life's replays stay refused.
    pub(crate) fn end_life(&mut self, agent: &AgentId) {
        for record in self
            .calls
            .get_mut(agent)
            .into_iter()
            .flat_map(HashMap::values_mut)
        {
            record.phase = Phase::Ended;
        }
    }

    /// `agent`'s slot went, and its calls with it.
    pub(crate) fn forget(&mut self, agent: &AgentId) {
        self.calls.remove(agent);
    }

    /// How many calls it keeps.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.calls.values().map(HashMap::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn agent() -> AgentId {
        AgentId::from_parts("claude-code", "s")
    }

    /// A fact by index, its start naming the one copy id `c` or none and its
    /// gate no request, so no start opens a generation and no gate a round.
    fn fact(n: u8, c: &CopyId) -> Fact<'_> {
        match n {
            0 => Fact::PreUse,
            1 => Fact::Start(None),
            2 => Fact::Start(Some(c)),
            3 => Fact::Gate(None),
            4 => Fact::Resume,
            _ => Fact::End,
        }
    }

    fn phase(ledger: &CallLedger) -> Option<Phase> {
        Some(ledger.calls.get(&agent())?.get("c")?.phase)
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
                let observed = ledger.observe(agent(), "c", fact(n, &c));
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
                ledger.observe(agent(), "c", fact(n, &c));
                let once = format!("{ledger:?}");
                let again = ledger.observe(agent(), "c", fact(n, &c));
                prop_assert!(!again.rose);
                prop_assert!(!again.counts);
                prop_assert_eq!(format!("{ledger:?}"), once);
            }
        }
    }

    #[test]
    fn a_call_counts_at_its_first_start_even_after_its_end() {
        let mut ledger = CallLedger::default();
        let end = ledger.observe(agent(), "c", Fact::End);
        assert_eq!((end.rose, end.counts), (true, false));
        let start = ledger.observe(agent(), "c", Fact::Start(None));
        assert_eq!((start.rose, start.counts), (false, true));
        assert!(!ledger.observe(agent(), "c", Fact::Start(None)).counts);
    }

    #[test]
    fn a_start_under_a_copy_id_its_ended_call_never_held_opens_its_next_generation() {
        let (first, second) = (CopyId::new("e1@t#0"), CopyId::new("e2@t#0"));
        let mut ledger = CallLedger::default();
        ledger.observe(agent(), "c", Fact::Start(Some(&first)));
        ledger.observe(agent(), "c", Fact::End);
        let replay = ledger.observe(agent(), "c", Fact::Start(Some(&first)));
        assert_eq!((replay.rose, replay.counts), (false, false), "a replay");
        let next = ledger.observe(agent(), "c", Fact::Start(Some(&second)));
        assert_eq!(
            next,
            Observed {
                rose: true,
                counts: true,
                ended: false,
            }
        );
        assert!(
            !ledger
                .observe(agent(), "c", Fact::Start(Some(&second)))
                .rose,
            "the new generation's replay"
        );
    }

    /// A hook start names no copy id and a transcript's names the call's own
    /// (claude-code's), so its echo after the end opens no generation.
    #[test]
    fn a_first_copy_id_after_the_end_opens_no_generation() {
        let copy = CopyId::new("toolu_c");
        let mut ledger = CallLedger::default();
        ledger.observe(agent(), "c", Fact::Start(None));
        ledger.observe(agent(), "c", Fact::End);
        let echo = ledger.observe(agent(), "c", Fact::Start(Some(&copy)));
        assert_eq!((echo.rose, echo.ended), (false, true));
    }

    #[test]
    fn a_second_gate_on_a_resumed_call_waits_again() {
        let mut ledger = CallLedger::default();
        ledger.observe(agent(), "c", Fact::Start(None));
        assert!(ledger.observe(agent(), "c", Fact::Gate(Some("g1"))).rose);
        assert!(ledger.observe(agent(), "c", Fact::Resume).rose);
        assert_eq!(
            ledger.observe(agent(), "c", Fact::Gate(Some("g2"))),
            Observed {
                rose: true,
                counts: false,
                ended: false,
            }
        );
        assert!(
            ledger.observe(agent(), "c", Fact::Resume).rose,
            "its own resume"
        );
    }

    #[test]
    fn a_replayed_gate_adds_no_round() {
        let mut ledger = CallLedger::default();
        ledger.observe(agent(), "c", Fact::Gate(Some("g1")));
        ledger.observe(agent(), "c", Fact::Resume);
        for replay in [Fact::Gate(Some("g1")), Fact::Gate(None), Fact::Resume] {
            assert!(!ledger.observe(agent(), "c", replay).rose, "{replay:?}");
        }
    }

    /// omp's ask raises its wait from its block, naming no request, and a
    /// `tools.approval.ask: prompt` override then records a request for that
    /// same wait.
    #[test]
    fn a_first_request_id_joins_the_wait_a_gate_naming_none_raised() {
        let mut ledger = CallLedger::default();
        ledger.observe(agent(), "c", Fact::Start(None));
        assert!(ledger.observe(agent(), "c", Fact::Gate(None)).rose);
        assert!(!ledger.observe(agent(), "c", Fact::Gate(Some("g1"))).rose);
        assert!(ledger.observe(agent(), "c", Fact::Resume).rose);
        assert!(
            ledger.observe(agent(), "c", Fact::Gate(Some("g2"))).rose,
            "a re-ask"
        );
    }

    #[test]
    fn a_first_request_id_after_a_resume_waits_again() {
        let mut ledger = CallLedger::default();
        for fact in [Fact::Start(None), Fact::Gate(None), Fact::Resume] {
            ledger.observe(agent(), "c", fact);
        }
        assert!(ledger.observe(agent(), "c", Fact::Gate(Some("g1"))).rose);
        assert!(!ledger.observe(agent(), "c", Fact::Gate(Some("g1"))).rose);
    }

    #[test]
    fn a_request_id_after_another_waits_again() {
        let mut ledger = CallLedger::default();
        ledger.observe(agent(), "c", Fact::Start(None));
        assert!(ledger.observe(agent(), "c", Fact::Gate(Some("g1"))).rose);
        assert!(ledger.observe(agent(), "c", Fact::Gate(Some("g2"))).rose);
        assert!(!ledger.observe(agent(), "c", Fact::Gate(Some("g1"))).rose);
    }

    #[test]
    fn a_generation_starts_its_rounds_afresh() {
        let (first, second) = (CopyId::new("e1@t#0"), CopyId::new("e2@t#0"));
        let mut ledger = CallLedger::default();
        for fact in [
            Fact::Start(Some(&first)),
            Fact::Gate(None),
            Fact::Gate(Some("g1")),
            Fact::End,
            Fact::Start(Some(&second)),
        ] {
            ledger.observe(agent(), "c", fact);
        }
        assert!(ledger.observe(agent(), "c", Fact::Gate(None)).rose);
        assert!(!ledger.observe(agent(), "c", Fact::Gate(Some("g2"))).rose);
    }

    #[test]
    fn a_revived_slot_ends_its_dead_lifes_calls_and_keeps_them() {
        let mut ledger = CallLedger::default();
        ledger.observe(agent(), "c", Fact::Start(None));
        ledger.end_life(&agent());
        let replay = ledger.observe(agent(), "c", Fact::Start(None));
        assert_eq!((replay.rose, replay.ended), (false, true));
        ledger.forget(&agent());
        assert_eq!(ledger.len(), 0);
    }
}
