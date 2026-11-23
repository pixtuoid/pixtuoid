//! The agents' tool calls: one record per call id, whose phase is the max of
//! its facts within a generation, so a call's facts merge in whatever order and
//! from whichever transport they arrive. A generation opens only where copy ids
//! mark a reused call id on one ordered stream ([`CallLedger::observe`]), whose
//! order no merge reorders. The reducer gates each of a call's state changes
//! but its approval on the rise `observe` reports, and an approval answers its
//! request once, since an answered member leaves it; a fact that rises into
//! nothing otherwise only merges idempotently (its pending fields, a Task's
//! tracking, its count flag), so a fact applied twice applies once.

use std::collections::HashMap;
use std::sync::Arc;

use crate::AgentId;
use crate::source::{AgentEvent, CopyId, Resolution};

/// How far a call got. An end outranks every step, so no fact of its
/// generation raises the call again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Phase {
    ended: bool,
    step: Step,
}

/// What a fact says of its call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fact<'a> {
    /// It is about to run.
    PreUse,
    /// It started, under its copy id where its wire names one.
    Start(Option<&'a CopyId>),
    /// It waits on the user.
    Gate,
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
                ..
            } => (id, Fact::Gate),
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
    const STARTED: Self = Self {
        ended: false,
        step: Step::Started,
    };

    fn of(fact: Fact<'_>) -> Self {
        let step = |step| Self { ended: false, step };
        match fact {
            Fact::PreUse => step(Step::PreUse),
            Fact::Start(_) => Self::STARTED,
            Fact::Gate => step(Step::Gated),
            Fact::Resume => step(Step::Resumed),
            Fact::End => Self {
                ended: true,
                step: Step::PreUse,
            },
        }
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
        let into = Phase::of(fact);
        let (starts, copy) = match fact {
            Fact::Start(copy) => (true, copy),
            _ => (false, None),
        };
        let calls = self.calls.entry(agent).or_default();
        let Some(record) = calls.get_mut(call) else {
            calls.insert(
                Arc::from(call),
                Call {
                    phase: into,
                    copies: copy.cloned().into_iter().collect(),
                    counted: starts,
                },
            );
            return Observed {
                rose: true,
                counts: starts,
                ended: into.ended,
            };
        };
        // A provider reusing an id names a new call (the `copy_id` comment in
        // `source/omp.rs`'s `decode_omp_line`). Only omp's copy ids can differ
        // for one call id, and every fact of its calls rides its one
        // transcript; claude-code's copy id is the call id itself.
        let generation = record.phase.ended
            && copy.is_some_and(|c| !record.copies.is_empty() && !record.copies.contains(c));
        if let Some(c) = copy
            && !record.copies.contains(c)
        {
            record.copies.push(c.clone());
        }
        if generation {
            record.phase = Phase::STARTED;
            return Observed {
                rose: true,
                counts: true,
                ended: false,
            };
        }
        let counts = starts && !record.counted;
        record.counted |= starts;
        let rose = into > record.phase;
        if rose {
            record.phase = into;
        }
        Observed {
            rose,
            counts,
            ended: record.phase.ended,
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
            record.phase.ended = true;
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

    /// A fact by index, its start naming the one copy id `c` or none, so no
    /// start opens a generation.
    fn fact(n: u8, c: &CopyId) -> Fact<'_> {
        match n {
            0 => Fact::PreUse,
            1 => Fact::Start(None),
            2 => Fact::Start(Some(c)),
            3 => Fact::Gate,
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
