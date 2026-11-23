//! The agents' turns: the one turn each has open and the turns its slot has
//! closed, and the rules a [`AgentEvent::TurnStart`] or [`AgentEvent::TurnEnd`]
//! follows. A thread runs one turn at a time, so a start of another turn proves
//! the open one ended, and so does another turn's end where one ordered stream
//! carries both brackets ([`Brackets::OneStream`]).
//!
//! [`AgentEvent::TurnStart`]: crate::AgentEvent::TurnStart
//! [`AgentEvent::TurnEnd`]: crate::AgentEvent::TurnEnd

use std::collections::{HashMap, HashSet};

use crate::AgentId;
use crate::state::TurnKey;

/// How an agent's turn brackets reach the ledger (`SourceCaps::brackets_turns`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Brackets {
    /// One ordered stream carries its starts and its ends.
    OneStream,
    /// Its starts and ends ride different transports, in no order between
    /// them (grok's hook start and transcript end).
    Split,
}

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
    /// The agent's work ends, and the turn's start went unseen (an oversized
    /// skip, a late attach) while no turn of its slot was open, so it was the
    /// current turn and what no turn scoped was its.
    StartUnseen,
    /// The turn is over, a replay or an older turn's end, or another turn is
    /// open on [`Brackets::Split`]: it ends nothing.
    Stale,
}

/// Every agent's open turn and closed turns.
#[derive(Debug, Default, Clone)]
pub(crate) struct TurnLedger {
    open: HashMap<AgentId, TurnKey>,
    closed: HashMap<AgentId, HashSet<TurnKey>>,
    /// The newest position each agent's ordered stream has named, start or
    /// end: what is older belongs to a turn already over.
    newest: HashMap<AgentId, TurnKey>,
}

impl TurnLedger {
    /// How many entries it keeps.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        let Self {
            open,
            closed,
            newest,
        } = self;
        open.len() + closed.values().map(HashSet::len).sum::<usize>() + newest.len()
    }

    /// `id`'s turn `key` started.
    pub(crate) fn start(&mut self, id: AgentId, key: TurnKey) -> Start {
        if self.is_closed(id, &key) {
            return Start::Stale;
        }
        // An in-order stream never goes back to an older position.
        if self.is_older_than_newest(id, &key) {
            self.close(id, key);
            return Start::Stale;
        }
        self.note_newest(id, &key);
        let superseded = self
            .open
            .insert(id, key.clone())
            .filter(|prev| *prev != key);
        if let Some(prev) = &superseded {
            self.close(id, prev.clone());
        }
        Start::Opened { superseded }
    }

    /// `id`'s turn `key` ended, its brackets arriving as `brackets`; `None` is
    /// an end naming no turn, which ends whatever is open.
    pub(crate) fn end(&mut self, id: AgentId, key: Option<&TurnKey>, brackets: Brackets) -> End {
        let Some(key) = key else {
            let implied = self.open.remove(&id);
            if let Some(open) = &implied {
                self.close(id, open.clone());
            }
            return End::Applied { implied };
        };
        if self.is_closed(id, key) {
            return End::Stale;
        }
        self.close(id, key.clone());
        let older = self.is_older_than_newest(id, key);
        self.note_newest(id, key);
        match self.open.remove(&id) {
            Some(open) if open == *key => End::Applied { implied: None },
            // A position before the open turn's is a turn already over.
            Some(open) if key.newer_than(&open) == Some(false) => {
                self.open.insert(id, open);
                End::Stale
            }
            // The stream is in order, so another turn's end proves the open
            // one ended.
            Some(open) if brackets == Brackets::OneStream => {
                self.close(id, open.clone());
                End::Applied {
                    implied: Some(open),
                }
            }
            // This end may lag the open turn's start, which rode the other
            // transport.
            Some(open) => {
                self.open.insert(id, open);
                End::Stale
            }
            None if older => End::Stale,
            None => End::StartUnseen,
        }
    }

    /// `id`'s open turn.
    pub(crate) fn open(&self, id: &AgentId) -> Option<&TurnKey> {
        self.open.get(id)
    }

    /// The slot went, or revived: the life's turns are over, so its open turn
    /// closes. Closed turns and the newest position stay until
    /// [`Self::retain`] drops the slot, so a revived slot refuses the dead
    /// life's late brackets.
    pub(crate) fn end_life(&mut self, id: AgentId) {
        if let Some(open) = self.open.remove(&id) {
            self.close(id, open);
        }
    }

    /// Keep the turns of the agents `keep` names.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&AgentId) -> bool) {
        self.open.retain(|id, _| keep(id));
        self.closed.retain(|id, _| keep(id));
        self.newest.retain(|id, _| keep(id));
    }

    fn close(&mut self, id: AgentId, key: TurnKey) {
        self.closed.entry(id).or_default().insert(key);
    }

    fn is_closed(&self, id: AgentId, key: &TurnKey) -> bool {
        self.closed.get(&id).is_some_and(|keys| keys.contains(key))
    }

    fn is_older_than_newest(&self, id: AgentId, key: &TurnKey) -> bool {
        self.newest
            .get(&id)
            .is_some_and(|newest| newest.newer_than(key) == Some(true))
    }

    /// Only an ordered position is kept: an opaque id names no newest.
    fn note_newest(&mut self, id: AgentId, key: &TurnKey) {
        let newer = self
            .newest
            .get(&id)
            .is_none_or(|newest| key.newer_than(newest) == Some(true));
        if matches!(key, TurnKey::Seq(_)) && newer {
            self.newest.insert(id, key.clone());
        }
    }
}

/// The turn-axis oracle over one `Seq` stream: its open turn is its newest
/// start, unless that or a newer turn has ended.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct SeqOracle {
    newest_start: Option<u64>,
    newest_end: Option<u64>,
    newest_ended: bool,
}

#[cfg(test)]
impl SeqOracle {
    pub(crate) fn start(&mut self, k: u64) {
        if self.newest_start.is_none_or(|m| k > m) {
            self.newest_start = Some(k);
            self.newest_ended = self.newest_end.is_some_and(|e| e >= k);
        }
    }

    /// `None` is an end naming no turn.
    pub(crate) fn end(&mut self, k: Option<u64>) {
        match k {
            Some(k) => {
                self.newest_end = self.newest_end.max(Some(k));
                self.newest_ended |= self.newest_start.is_some_and(|m| k >= m);
            }
            None => self.newest_ended = true,
        }
    }

    pub(crate) fn open(&self) -> Option<TurnKey> {
        self.newest_start
            .filter(|_| !self.newest_ended)
            .map(TurnKey::Seq)
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

    #[test]
    fn a_turn_opens_and_its_own_end_applies() {
        let mut turns = TurnLedger::default();
        assert_eq!(
            turns.start(id(), key("a")),
            Start::Opened { superseded: None }
        );
        assert_eq!(turns.open(&id()), Some(&key("a")));
        assert_eq!(
            turns.end(id(), Some(&key("a")), Brackets::OneStream),
            End::Applied { implied: None }
        );
        assert_eq!(turns.open(&id()), None);
    }

    #[test]
    fn a_start_of_another_turn_supersedes_the_open_one() {
        let mut turns = TurnLedger::default();
        turns.start(id(), key("a"));
        assert_eq!(
            turns.start(id(), key("b")),
            Start::Opened {
                superseded: Some(key("a"))
            }
        );
        assert_eq!(
            turns.start(id(), key("a")),
            Start::Stale,
            "the superseded turn closed"
        );
        assert_eq!(
            turns.start(id(), key("b")),
            Start::Opened { superseded: None },
            "a repeated start of the open turn changes nothing"
        );
    }

    #[test]
    fn an_end_of_another_turn_on_one_stream_implies_the_open_one_ended() {
        let mut turns = TurnLedger::default();
        turns.start(id(), key("a"));
        assert_eq!(
            turns.end(id(), Some(&key("b")), Brackets::OneStream),
            End::Applied {
                implied: Some(key("a"))
            }
        );
        assert_eq!(turns.open(&id()), None);
        assert_eq!(turns.start(id(), key("a")), Start::Stale);
    }

    #[test]
    fn an_end_of_an_unseen_turn_on_split_brackets_leaves_the_open_turn_open() {
        let mut turns = TurnLedger::default();
        turns.start(id(), key("b"));
        assert_eq!(
            turns.end(id(), Some(&key("a")), Brackets::Split),
            End::Stale
        );
        assert_eq!(turns.open(&id()), Some(&key("b")));
        assert_eq!(
            turns.start(id(), key("a")),
            Start::Stale,
            "its own turn closed"
        );
    }

    #[test]
    fn an_end_with_no_turn_open_applies_once() {
        let mut turns = TurnLedger::default();
        assert_eq!(
            turns.end(id(), Some(&key("a")), Brackets::OneStream),
            End::StartUnseen,
            "the stream's current turn"
        );
        assert_eq!(
            turns.end(id(), Some(&key("a")), Brackets::OneStream),
            End::Stale
        );
        assert_eq!(turns.start(id(), key("a")), Start::Stale);
    }

    #[test]
    fn a_replayed_end_of_a_closed_turn_leaves_the_open_one() {
        let mut turns = TurnLedger::default();
        turns.start(id(), key("a"));
        turns.end(id(), Some(&key("a")), Brackets::OneStream);
        turns.start(id(), key("b"));
        assert_eq!(
            turns.end(id(), Some(&key("a")), Brackets::OneStream),
            End::Stale
        );
        assert_eq!(turns.open(&id()), Some(&key("b")));
    }

    #[test]
    fn an_end_naming_no_turn_ends_the_open_one() {
        let mut turns = TurnLedger::default();
        assert_eq!(
            turns.end(id(), None, Brackets::OneStream),
            End::Applied { implied: None }
        );
        turns.start(id(), key("a"));
        assert_eq!(
            turns.end(id(), None, Brackets::OneStream),
            End::Applied {
                implied: Some(key("a"))
            }
        );
        assert_eq!(turns.start(id(), key("a")), Start::Stale);
    }

    #[test]
    fn a_closed_turn_refuses_its_replay_until_its_slot_goes() {
        let mut turns = TurnLedger::default();
        turns.start(id(), key("a"));
        turns.end(id(), Some(&key("a")), Brackets::OneStream);
        turns.start(id(), key("b"));
        assert_eq!(turns.start(id(), key("a")), Start::Stale);
        turns.end_life(id());
        assert_eq!(turns.open(&id()), None);
        assert_eq!(
            turns.start(id(), key("a")),
            Start::Stale,
            "a revive keeps it"
        );
        assert_eq!(
            turns.end(id(), Some(&key("b")), Brackets::Split),
            End::Stale,
            "and closes the turn the old life left open"
        );
        turns.retain(|_| false);
        assert_eq!(
            turns.start(id(), key("a")),
            Start::Opened { superseded: None },
            "it goes with its slot"
        );
    }

    #[test]
    fn a_seq_start_older_than_the_newest_is_stale_and_closes() {
        let mut turns = TurnLedger::default();
        turns.start(id(), TurnKey::Seq(5));
        assert_eq!(turns.start(id(), TurnKey::Seq(3)), Start::Stale);
        assert_eq!(turns.open(&id()), Some(&TurnKey::Seq(5)));
        assert!(turns.is_closed(id(), &TurnKey::Seq(3)));
    }

    #[test]
    fn a_seq_turn_open_at_a_revive_never_restarts() {
        let mut turns = TurnLedger::default();
        turns.start(id(), TurnKey::Seq(5));
        assert_eq!(
            turns.start(id(), TurnKey::Seq(5)),
            Start::Opened { superseded: None },
            "a repeated start of the open turn changes nothing"
        );
        turns.end_life(id());
        assert_eq!(turns.start(id(), TurnKey::Seq(5)), Start::Stale);
        assert_eq!(turns.open(&id()), None);
    }

    #[test]
    fn a_seq_end_older_than_the_open_turn_is_closed_only() {
        let mut turns = TurnLedger::default();
        turns.start(id(), TurnKey::Seq(5));
        assert_eq!(
            turns.end(id(), Some(&TurnKey::Seq(3)), Brackets::OneStream),
            End::Stale
        );
        assert_eq!(turns.open(&id()), Some(&TurnKey::Seq(5)));
        assert_eq!(turns.start(id(), TurnKey::Seq(3)), Start::Stale);
    }

    #[test]
    fn a_seq_end_newer_than_the_open_turn_ends_it_and_every_older_start() {
        let mut turns = TurnLedger::default();
        turns.start(id(), TurnKey::Seq(5));
        assert_eq!(
            turns.end(id(), Some(&TurnKey::Seq(7)), Brackets::OneStream),
            End::Applied {
                implied: Some(TurnKey::Seq(5))
            }
        );
        assert_eq!(turns.start(id(), TurnKey::Seq(6)), Start::Stale);
        assert_eq!(turns.open(&id()), None);
    }

    #[test]
    fn a_seq_end_older_than_the_newest_with_no_turn_open_is_closed_only() {
        let mut turns = TurnLedger::default();
        turns.start(id(), TurnKey::Seq(5));
        turns.end(id(), Some(&TurnKey::Seq(5)), Brackets::OneStream);
        assert_eq!(
            turns.end(id(), Some(&TurnKey::Seq(3)), Brackets::OneStream),
            End::Stale
        );
        assert_eq!(
            turns.end(id(), Some(&TurnKey::Seq(9)), Brackets::OneStream),
            End::StartUnseen,
            "a newer turn"
        );
    }

    #[test]
    fn the_newest_position_outlives_a_revive_and_goes_with_the_slot() {
        let mut turns = TurnLedger::default();
        turns.start(id(), TurnKey::Seq(5));
        turns.end_life(id());
        assert_eq!(
            turns.start(id(), TurnKey::Seq(3)),
            Start::Stale,
            "a revived session's stream goes on where it was"
        );
        turns.retain(|_| false);
        assert_eq!(
            turns.start(id(), TurnKey::Seq(4)),
            Start::Opened { superseded: None }
        );
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
        /// Over any bracket sequence of opaque ids: an open turn is never a
        /// closed one, a turn leaves `open` only into `closed`, an end closes
        /// its own turn, and an open turn closes only by its own end, an end
        /// naming no turn, another turn's start, or, on one stream, another
        /// turn's end.
        #[test]
        fn the_ledger_never_reopens_or_loses_a_turn(
            ops in proptest::collection::vec(op(), 0..32),
            one_stream in any::<bool>(),
        ) {
            let brackets = if one_stream { Brackets::OneStream } else { Brackets::Split };
            let mut turns = TurnLedger::default();
            for op in ops {
                let before = turns.open(&id()).cloned();
                match op {
                    Op::Start(k) => {
                        let outcome = turns.start(id(), key(&k.to_string()));
                        if outcome == Start::Stale {
                            prop_assert_eq!(turns.open(&id()).cloned(), before.clone());
                        }
                    }
                    Op::End(None) => {
                        turns.end(id(), None, brackets);
                        prop_assert_eq!(turns.open(&id()), None);
                    }
                    Op::End(Some(k)) => {
                        let k = key(&k.to_string());
                        let replay = turns.is_closed(id(), &k);
                        turns.end(id(), Some(&k), brackets);
                        prop_assert!(turns.is_closed(id(), &k));
                        let open = turns.open(&id()).cloned();
                        let another = before.as_ref() != Some(&k);
                        if replay || (another && brackets == Brackets::Split) {
                            prop_assert_eq!(open, before.clone());
                        } else {
                            prop_assert_eq!(open, None);
                        }
                    }
                }
                if let Some(open) = turns.open(&id()) {
                    prop_assert!(!turns.is_closed(id(), open));
                }
                if let Some(prev) = before
                    && turns.open(&id()) != Some(&prev)
                {
                    prop_assert!(turns.is_closed(id(), &prev));
                }
            }
        }

        #[test]
        fn a_seq_streams_open_turn_is_its_newest_unended_start(ops in proptest::collection::vec(op(), 0..32)) {
            let mut turns = TurnLedger::default();
            let mut oracle = SeqOracle::default();
            for op in ops {
                match op {
                    Op::Start(k) => {
                        let k = u64::from(k);
                        turns.start(id(), TurnKey::Seq(k));
                        oracle.start(k);
                    }
                    Op::End(k) => {
                        let k = k.map(u64::from);
                        turns.end(id(), k.map(TurnKey::Seq).as_ref(), Brackets::OneStream);
                        oracle.end(k);
                    }
                }
                prop_assert_eq!(turns.open(&id()).cloned(), oracle.open());
            }
        }
    }
}
