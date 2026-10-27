//! The agents' turns from their bracket stream: the one turn each has open and
//! the recently closed, and the rules a [`AgentEvent::TurnStart`] or
//! [`AgentEvent::TurnEnd`] follows. A thread runs one turn at a time and its
//! stream is in order, so a bracket of another turn proves the open one ended.
//!
//! [`AgentEvent::TurnStart`]: crate::AgentEvent::TurnStart
//! [`AgentEvent::TurnEnd`]: crate::AgentEvent::TurnEnd

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use crate::AgentId;
use crate::source::DEFAULT_POLL_INTERVAL;
use crate::state::TurnKey;
use crate::state::correlation::is_fresh;

/// How long a closed turn refuses a replay of its brackets: a transcript lags
/// its write by up to one poll, and a re-read lands within one more.
pub(crate) const CLOSED_TURN_TTL: Duration = DEFAULT_POLL_INTERVAL.saturating_mul(2);

/// What a [`TurnLedger::start`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// The turn is open; `superseded` is the one it implicitly ended.
    Opened { superseded: Option<TurnKey> },
    /// The turn already closed: a replay.
    Stale,
}

/// What a [`TurnLedger::end`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// The agent's work ends; `implied` is the open turn this end proves
    /// ended, when it is not the turn named.
    Applied { implied: Option<TurnKey> },
    /// The turn already closed: a replay that ends nothing.
    Stale,
}

/// Every agent's open turn and recently closed turns.
#[derive(Debug, Default)]
pub(crate) struct TurnLedger {
    open: HashMap<AgentId, TurnKey>,
    closed: HashMap<(AgentId, TurnKey), SystemTime>,
}

impl TurnLedger {
    /// `id`'s turn `key` started.
    pub(crate) fn start(&mut self, id: AgentId, key: TurnKey, now: SystemTime) -> Start {
        if self.is_closed(id, &key, now) {
            return Start::Stale;
        }
        let superseded = self
            .open
            .insert(id, key.clone())
            .filter(|prev| *prev != key);
        if let Some(prev) = &superseded {
            self.closed.insert((id, prev.clone()), now);
        }
        Start::Opened { superseded }
    }

    /// `id`'s turn `key` ended; `None` is an end naming no turn, which ends
    /// whatever is open.
    pub(crate) fn end(&mut self, id: AgentId, key: Option<&TurnKey>, now: SystemTime) -> End {
        let Some(key) = key else {
            let implied = self.open.remove(&id);
            if let Some(open) = &implied {
                self.closed.insert((id, open.clone()), now);
            }
            return End::Applied { implied };
        };
        if self.is_closed(id, key, now) {
            return End::Stale;
        }
        self.closed.insert((id, key.clone()), now);
        match self.open.remove(&id) {
            Some(open) if open == *key => End::Applied { implied: None },
            // The stream is in order, so another turn's end proves the open
            // one ended.
            Some(open) => {
                self.closed.insert((id, open.clone()), now);
                End::Applied {
                    implied: Some(open),
                }
            }
            // Its start went unseen (an oversized skip, a late attach), and
            // the one ordered stream proves it was the current turn.
            None => End::Applied { implied: None },
        }
    }

    /// `id`'s open turn.
    pub(crate) fn open(&self, id: &AgentId) -> Option<&TurnKey> {
        self.open.get(id)
    }

    /// The slot went, or revived: its open turn goes with its life. A closed
    /// turn ages out on its own clock, so a revived slot still refuses its
    /// replays.
    pub(crate) fn forget(&mut self, id: &AgentId) {
        self.open.remove(id);
    }

    /// Keep the open turns of the agents `keep` names.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&AgentId) -> bool) {
        self.open.retain(|id, _| keep(id));
    }

    /// Age out the closed turns past [`CLOSED_TURN_TTL`].
    pub(crate) fn gc(&mut self, now: SystemTime) {
        self.closed
            .retain(|_, at| is_fresh(now, *at, CLOSED_TURN_TTL));
    }

    fn is_closed(&self, id: AgentId, key: &TurnKey, now: SystemTime) -> bool {
        self.closed
            .get(&(id, key.clone()))
            .is_some_and(|at| is_fresh(now, *at, CLOSED_TURN_TTL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn id() -> AgentId {
        AgentId::from_parts("codex", "s")
    }

    fn key(k: &str) -> TurnKey {
        TurnKey::Id(k.into())
    }

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
    }

    #[test]
    fn a_turn_opens_and_its_own_end_applies() {
        let mut turns = TurnLedger::default();
        assert_eq!(
            turns.start(id(), key("a"), t0()),
            Start::Opened { superseded: None }
        );
        assert_eq!(turns.open(&id()), Some(&key("a")));
        assert_eq!(
            turns.end(id(), Some(&key("a")), t0()),
            End::Applied { implied: None }
        );
        assert_eq!(turns.open(&id()), None);
    }

    #[test]
    fn a_start_of_another_turn_supersedes_the_open_one() {
        let mut turns = TurnLedger::default();
        turns.start(id(), key("a"), t0());
        assert_eq!(
            turns.start(id(), key("b"), t0()),
            Start::Opened {
                superseded: Some(key("a"))
            }
        );
        assert_eq!(
            turns.start(id(), key("a"), t0()),
            Start::Stale,
            "the superseded turn closed"
        );
        assert_eq!(
            turns.start(id(), key("b"), t0()),
            Start::Opened { superseded: None },
            "a repeated start of the open turn changes nothing"
        );
    }

    #[test]
    fn an_end_of_another_turn_implies_the_open_one_ended() {
        let mut turns = TurnLedger::default();
        turns.start(id(), key("a"), t0());
        assert_eq!(
            turns.end(id(), Some(&key("b")), t0()),
            End::Applied {
                implied: Some(key("a"))
            }
        );
        assert_eq!(turns.open(&id()), None);
        assert_eq!(turns.start(id(), key("a"), t0()), Start::Stale);
    }

    #[test]
    fn an_end_with_no_turn_open_applies_once() {
        let mut turns = TurnLedger::default();
        assert_eq!(
            turns.end(id(), Some(&key("a")), t0()),
            End::Applied { implied: None },
            "the stream's current turn, its start unseen"
        );
        assert_eq!(turns.end(id(), Some(&key("a")), t0()), End::Stale);
        assert_eq!(turns.start(id(), key("a"), t0()), Start::Stale);
    }

    #[test]
    fn a_replayed_end_of_a_closed_turn_leaves_the_open_one() {
        let mut turns = TurnLedger::default();
        turns.start(id(), key("a"), t0());
        turns.end(id(), Some(&key("a")), t0());
        turns.start(id(), key("b"), t0());
        assert_eq!(turns.end(id(), Some(&key("a")), t0()), End::Stale);
        assert_eq!(turns.open(&id()), Some(&key("b")));
    }

    #[test]
    fn an_end_naming_no_turn_ends_the_open_one() {
        let mut turns = TurnLedger::default();
        assert_eq!(turns.end(id(), None, t0()), End::Applied { implied: None });
        turns.start(id(), key("a"), t0());
        assert_eq!(
            turns.end(id(), None, t0()),
            End::Applied {
                implied: Some(key("a"))
            }
        );
        assert_eq!(turns.start(id(), key("a"), t0()), Start::Stale);
    }

    #[test]
    fn a_closed_turn_refuses_its_replay_until_its_ttl() {
        let mut turns = TurnLedger::default();
        turns.start(id(), key("a"), t0());
        turns.end(id(), Some(&key("a")), t0());
        let just_inside = t0() + CLOSED_TURN_TTL - Duration::from_millis(1);
        turns.gc(just_inside);
        assert_eq!(turns.start(id(), key("a"), just_inside), Start::Stale);
        turns.gc(t0() + CLOSED_TURN_TTL);
        assert_eq!(
            turns.start(id(), key("a"), t0() + CLOSED_TURN_TTL),
            Start::Opened { superseded: None }
        );
    }

    #[test]
    fn a_removed_slot_keeps_its_closed_turns() {
        let mut turns = TurnLedger::default();
        turns.start(id(), key("a"), t0());
        turns.end(id(), Some(&key("a")), t0());
        turns.start(id(), key("b"), t0());
        turns.forget(&id());
        assert_eq!(turns.open(&id()), None);
        assert_eq!(turns.start(id(), key("a"), t0()), Start::Stale);
    }

    #[derive(Clone, Debug)]
    enum Op {
        Start(u8),
        End(Option<u8>),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..4).prop_map(Op::Start),
            proptest::option::of(0u8..4).prop_map(Op::End),
        ]
    }

    proptest! {
        /// Over any bracket sequence: an open turn is never a closed one, a
        /// turn leaves `open` only into `closed`, and an applied end leaves
        /// nothing open.
        #[test]
        fn the_ledger_never_reopens_or_loses_a_turn(ops in proptest::collection::vec(op(), 0..32)) {
            let mut turns = TurnLedger::default();
            for op in ops {
                let before = turns.open(&id()).cloned();
                match op {
                    Op::Start(k) => {
                        let outcome = turns.start(id(), key(&k.to_string()), t0());
                        if outcome == Start::Stale {
                            prop_assert_eq!(turns.open(&id()).cloned(), before.clone());
                        }
                    }
                    Op::End(k) => {
                        let k = k.map(|k| key(&k.to_string()));
                        if let End::Applied { .. } = turns.end(id(), k.as_ref(), t0()) {
                            prop_assert_eq!(turns.open(&id()), None);
                        }
                    }
                }
                if let Some(open) = turns.open(&id()) {
                    prop_assert!(!turns.is_closed(id(), open, t0()));
                }
                if let Some(prev) = before
                    && turns.open(&id()) != Some(&prev)
                {
                    prop_assert!(turns.is_closed(id(), &prev, t0()));
                }
            }
        }
    }
}
