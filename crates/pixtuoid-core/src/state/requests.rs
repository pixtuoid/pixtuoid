//! The agents' open user-action requests (a slot's `RequiresAction`) and the
//! calls pending that one can hold, with the two rules every event meets them
//! through: `bind`, which calls a request holds, and `resolves`, which event
//! answers it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::AgentId;
use crate::source::{Resolution, Transport};
use crate::state::{ToolUse, TurnKey};

/// What an end names.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Ended<'a> {
    /// A call's own end.
    Call(&'a str),
    /// A tool end naming no call: its wire carries none (Reasonix's
    /// PostToolUse).
    Unnamed,
    /// A turn's end: the turn it names, if any, the open turn it proved
    /// ended, if that is another, and whether it also ends what no turn
    /// scoped: it names none, or its start went unseen.
    Turn {
        turn: Option<&'a TurnKey>,
        implied: Option<&'a TurnKey>,
        turnless: bool,
    },
}

/// An event as a request's resolution reads it.
#[derive(Clone, Copy, Debug)]
enum Seen<'a> {
    /// A call started; its id when the wire names one.
    Start(Option<&'a str>),
    End(Ended<'a>),
    Resolved(&'a Resolution),
    /// An unnamed child's event, dropped while these Tasks are in flight: the
    /// child resumed.
    ChildResumed(&'a HashSet<String>),
}

/// A raise as the wire gives it, its turn already scoped.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Raise<'a> {
    /// The call the wire names.
    pub(crate) call: Option<&'a str>,
    /// The tool the wire names.
    pub(crate) tool_name: Option<&'a str>,
    /// Its own turn, else the agent's open one.
    pub(crate) turn: Option<&'a TurnKey>,
    pub(crate) from: Transport,
}

/// A call that started and has not ended, as a request binds it.
#[derive(Debug)]
pub(crate) struct PendingStart {
    pub(crate) call: Arc<str>,
    /// The tool's wire name, when the start names it.
    pub(crate) tool_name: Option<Arc<str>>,
    /// Its own turn, else the agent's open one.
    pub(crate) turn: Option<TurnKey>,
    pub(crate) on: Transport,
    /// The item its start shows; `None` for a pre-use, which starts nothing.
    pub(crate) item: Option<ToolUse>,
}

/// A pending call, once per id however many of its starts arrive: a pre-use
/// and its start, or the two transports' starts of one call.
#[derive(Debug)]
struct Pending {
    call: Arc<str>,
    tool_name: Option<Arc<str>>,
    turn: Option<TurnKey>,
    seen_on: Vec<Transport>,
    /// What its start showed, once it started: the call is in flight.
    item: Option<ToolUse>,
}

#[derive(Debug)]
struct Member {
    call: Arc<str>,
    /// The wire named it, so its start on the raising transport is the
    /// approval's resume (#951); a bound member's own start is its pre-use.
    named: bool,
}

#[derive(Debug)]
struct ActionRequest {
    /// The calls it holds on. The last member's end means the prompt was
    /// answered; a parallel call's end is no member's, so it cannot.
    members: Vec<Member>,
    /// A raise named its call on the wire.
    named: bool,
    tool_name: Option<Arc<str>>,
    turn: Option<TurnKey>,
    raised_on: Transport,
}

impl ActionRequest {
    fn member(&self, call: &str) -> Option<&Member> {
        self.members.iter().find(|m| &*m.call == call)
    }

    fn is_last_member(&self, call: &str) -> bool {
        matches!(&self.members[..], [m] if &*m.call == call)
    }

    /// The wire gave no key to bind it by, so the agent's next start on its
    /// transport is the agent working again.
    fn is_keyless(&self) -> bool {
        self.members.is_empty() && self.tool_name.is_none()
    }

    fn holds(&mut self, call: &Arc<str>, named: bool) {
        if self.member(call).is_none() {
            self.members.push(Member {
                call: Arc::clone(call),
                named,
            });
        }
    }

    /// Whether `pending` is a call this request binds by its tool name.
    fn binds_by_name(&self, pending: &Pending) -> bool {
        self.tool_name.is_some()
            && pending.tool_name == self.tool_name
            && pending.is_in(self.turn.as_ref(), self.raised_on)
    }
}

/// Every agent's open request and pending starts. A request lives until an
/// event answers it or its slot goes; whether it stands is the slot's
/// `RequiresAction`. A pending start lives until its call's end, its turn's
/// end, or its slot goes.
#[derive(Debug, Default)]
pub(crate) struct ActionRequests {
    open: HashMap<AgentId, ActionRequest>,
    pending: HashMap<AgentId, Vec<Pending>>,
}

impl ActionRequests {
    /// `id` raised a request. `requires_action` is whether one already
    /// stands: a second raise keeps the first one's members and identity, and
    /// adds only the calls its own key binds. A raise for another turn replaces
    /// it: a thread runs one turn at a time, so the standing request's turn is
    /// over, and its lagging ends must not answer this one.
    pub(crate) fn raise(&mut self, id: AgentId, raise: Raise<'_>, requires_action: bool) {
        let pending = self.pending.get(&id).map_or(&[][..], Vec::as_slice);
        let standing = self.open.get(&id).filter(|req| {
            requires_action
                && !matches!((&req.turn, raise.turn), (Some(standing), Some(new)) if standing != new)
        });
        if standing.is_none() {
            let mut fresh = ActionRequest {
                members: Vec::new(),
                named: false,
                tool_name: raise.tool_name.map(Arc::from),
                turn: raise.turn.cloned(),
                raised_on: raise.from,
            };
            // The sole pending start is a deduction, not a key, so only a
            // fresh request the wire gave no key draws it.
            if raise.call.is_none()
                && raise.tool_name.is_none()
                && let [sole] = in_scope(pending, raise.turn, raise.from).as_slice()
            {
                fresh.holds(&sole.call, false);
            }
            self.open.insert(id, fresh);
        }
        if let Some(req) = self.open.get_mut(&id) {
            bind(req, pending, raise);
        }
    }

    /// A call started without ending yet, on `start.on`. A request naming its
    /// tool binds it now, so a pre-use delivered after its request still joins
    /// (#242).
    pub(crate) fn register(&mut self, id: AgentId, start: PendingStart) {
        let starts = self.pending.entry(id).or_default();
        let mut at = match starts.iter().position(|p| p.call == start.call) {
            Some(at) => at,
            None => {
                starts.push(Pending {
                    call: start.call,
                    tool_name: None,
                    turn: None,
                    seen_on: Vec::new(),
                    item: None,
                });
                starts.len() - 1
            }
        };
        // A call's first start makes it the newest in flight.
        if let Some(item) = start.item
            && starts[at].item.is_none()
        {
            let mut started = starts.remove(at);
            started.item = Some(item);
            starts.push(started);
            at = starts.len() - 1;
        }
        let pending = &mut starts[at];
        if pending.tool_name.is_none() {
            pending.tool_name = start.tool_name;
        }
        // A start seen while no turn was open (an oversized skip, a late
        // attach) takes the turn a later registration names.
        if pending.turn.is_none() {
            pending.turn = start.turn;
        }
        if !pending.seen_on.contains(&start.on) {
            pending.seen_on.push(start.on);
        }
        if let Some(req) = self.open.get_mut(&id)
            && req.binds_by_name(pending)
        {
            req.holds(&pending.call, false);
        }
    }

    /// A start of `call` on `from`: whether the slot runs it, else it only
    /// counts. `requires_action` is whether a request stands.
    pub(crate) fn start(
        &mut self,
        id: AgentId,
        call: Option<&str>,
        from: Transport,
        requires_action: bool,
    ) -> bool {
        if !requires_action {
            self.open.remove(&id);
            return true;
        }
        let resolved = self.resolves(&id, Seen::Start(call), from);
        if let Some(req) = self.open.get_mut(&id)
            && from == req.raised_on
            && let Some(c) = call
            && req.member(c).is_some_and(|m| m.named)
        {
            req.members.retain(|m| &*m.call != c);
        }
        if resolved {
            self.open.remove(&id);
        }
        resolved
    }

    /// An end on `from`: whether it answers the standing request. A call's
    /// end retires that call whether or not it answered (a denial, or a
    /// parallel call that was a member too); a turn's end retires its pending
    /// starts.
    pub(crate) fn end(
        &mut self,
        id: AgentId,
        ended: Ended<'_>,
        from: Transport,
        requires_action: bool,
    ) -> bool {
        let resolved = requires_action && self.resolves(&id, Seen::End(ended), from);
        match ended {
            Ended::Call(c) => self.retire(&id, c),
            Ended::Turn {
                turn,
                implied,
                turnless,
            } => self.retire_turn(&id, turn, implied, turnless, from),
            Ended::Unnamed => {}
        }
        if resolved {
            self.open.remove(&id);
        }
        resolved
    }

    /// An answer that ends no item: whether it answers the standing request.
    pub(crate) fn resolved(&mut self, id: AgentId, by: &Resolution, requires_action: bool) -> bool {
        let resolved = requires_action && self.resolves(&id, Seen::Resolved(by), Transport::Hook);
        if let Resolution::Call(c) = by {
            self.retire(&id, c);
        }
        if resolved {
            self.open.remove(&id);
        }
        resolved
    }

    /// An unnamed child's event dropped while `tasks` are in flight: whether
    /// it answers the standing request, which it then closes.
    pub(crate) fn child_resumed(
        &mut self,
        id: AgentId,
        tasks: &HashSet<String>,
        requires_action: bool,
    ) -> bool {
        let resolved =
            requires_action && self.resolves(&id, Seen::ChildResumed(tasks), Transport::Hook);
        if resolved {
            self.open.remove(&id);
        }
        resolved
    }

    /// Whether `seen`, arriving on `from`, answers `id`'s standing request.
    fn resolves(&self, id: &AgentId, seen: Seen<'_>, from: Transport) -> bool {
        let Some(req) = self.open.get(id) else {
            return false;
        };
        match seen {
            // The other transport's start of the same work lands while the
            // human still decides (#951), so only the raising transport's
            // start counts: a named member's resume, or any start once the
            // wire gave no key.
            Seen::Start(call) => {
                from == req.raised_on
                    && match call.and_then(|c| req.member(c)) {
                        Some(m) => m.named && req.is_last_member(&m.call),
                        None => req.is_keyless(),
                    }
            }
            Seen::End(Ended::Call(c)) => req.is_last_member(c),
            Seen::Resolved(Resolution::Call(c)) => req.is_last_member(c),
            // Reasonix: its prompt and its tool's end both name no call.
            Seen::End(Ended::Unnamed) => from == Transport::Hook && !req.named,
            Seen::End(Ended::Turn {
                turn,
                implied,
                turnless,
            }) => match &req.turn {
                Some(t) => Some(t) == turn || Some(t) == implied,
                // The transcript may read a turn's end past a hook's fresh
                // raise, so it answers only its own.
                None => turnless && (from == Transport::Hook || req.raised_on == Transport::Jsonl),
            },
            Seen::Resolved(Resolution::Turn(k)) => req.turn.as_ref() == Some(k),
            // The child resumed, unless a member is the parent's own call.
            Seen::ChildResumed(tasks) => req.members.iter().all(|m| tasks.contains(&*m.call)),
        }
    }

    /// The item of `id`'s newest call that started and has not ended.
    pub(crate) fn newest_in_flight(&self, id: &AgentId) -> Option<ToolUse> {
        self.pending
            .get(id)?
            .iter()
            .rev()
            .find_map(|p| p.item.clone())
    }

    pub(crate) fn is_member(&self, id: &AgentId, call: &str) -> bool {
        self.open
            .get(id)
            .is_some_and(|req| req.member(call).is_some())
    }

    /// `call` ended: it is no longer pending nor a member. Retiring the last
    /// member answers nothing: a drained Task's end may be a subagent's, past
    /// its parent's own prompt (#152).
    pub(crate) fn retire(&mut self, id: &AgentId, call: &str) {
        if let Some(req) = self.open.get_mut(id) {
            req.members.retain(|m| &*m.call != call);
        }
        if let Some(starts) = self.pending.get_mut(id) {
            starts.retain(|s| &*s.call != call);
        }
    }

    /// The pending starts a turn's end retires: its turn's and the implied
    /// one's, and the turnless ones when it is `turnless`, a transcript's only
    /// its own: grok's transcript reads a turn's end past its hooks' next call.
    fn retire_turn(
        &mut self,
        id: &AgentId,
        turn: Option<&TurnKey>,
        implied: Option<&TurnKey>,
        turnless: bool,
        from: Transport,
    ) {
        let Some(starts) = self.pending.get_mut(id) else {
            return;
        };
        for p in starts.iter_mut() {
            match &p.turn {
                Some(t) if Some(t) == turn || Some(t) == implied => p.seen_on.clear(),
                None if turnless && from == Transport::Jsonl => {
                    p.seen_on.retain(|on| *on != Transport::Jsonl);
                }
                None if turnless => p.seen_on.clear(),
                _ => {}
            }
        }
        starts.retain(|p| !p.seen_on.is_empty());
    }

    /// The slot went: its request and pending starts with it.
    pub(crate) fn forget(&mut self, id: &AgentId) {
        self.open.remove(id);
        self.pending.remove(id);
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&AgentId) -> bool) {
        self.open.retain(|id, _| keep(id));
        self.pending.retain(|id, _| keep(id));
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, id: &AgentId) -> bool {
        self.open.contains_key(id)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.open.len()
    }
}

impl Pending {
    /// Whether it is pending on `on` in `turn`.
    fn is_in(&self, turn: Option<&TurnKey>, on: Transport) -> bool {
        self.seen_on.contains(&on) && self.turn.as_ref() == turn
    }
}

/// The calls pending on `on` in `turn`.
fn in_scope<'a>(pending: &'a [Pending], turn: Option<&TurnKey>, on: Transport) -> Vec<&'a Pending> {
    pending.iter().filter(|p| p.is_in(turn, on)).collect()
}

/// The calls `raise`'s own key binds: the call it names, else every pending
/// start of the tool it names in its turn. Hooks may rewrite a call's input
/// but never its name, so the name is the key.
fn bind(req: &mut ActionRequest, pending: &[Pending], raise: Raise<'_>) {
    if let Some(call) = raise.call {
        req.holds(&Arc::from(call), true);
        req.named = true;
    } else if let Some(name) = raise.tool_name {
        for start in in_scope(pending, raise.turn, raise.from) {
            if start.tool_name.as_deref() == Some(name) {
                req.holds(&start.call, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOOK: Transport = Transport::Hook;
    const JSONL: Transport = Transport::Jsonl;

    fn id() -> AgentId {
        AgentId::from_parts("claude-code", "s")
    }

    fn turn(k: &str) -> TurnKey {
        TurnKey::Id(k.into())
    }

    fn pending(call: &str, tool_name: Option<&str>, on: Transport) -> PendingStart {
        PendingStart {
            call: call.into(),
            tool_name: tool_name.map(Arc::from),
            turn: None,
            on,
            item: None,
        }
    }

    fn started(call: &str) -> PendingStart {
        PendingStart {
            item: Some(ToolUse {
                tool_use_id: Some(call.into()),
                detail: None,
                kind: crate::state::ToolKind::Other,
            }),
            ..pending(call, None, HOOK)
        }
    }

    fn in_flight(reqs: &ActionRequests) -> Option<String> {
        Some(reqs.newest_in_flight(&id())?.tool_use_id?.to_string())
    }

    #[test]
    fn the_newest_call_in_flight_is_the_last_to_start_and_not_end() {
        let mut reqs = ActionRequests::default();
        reqs.register(id(), pending("a", Some("Bash"), HOOK));
        assert_eq!(in_flight(&reqs), None, "a pre-use starts nothing");
        reqs.register(id(), started("b"));
        reqs.register(id(), started("a"));
        assert_eq!(
            in_flight(&reqs).as_deref(),
            Some("a"),
            "a's start came last, though its pre-use came first"
        );
        reqs.register(id(), started("b"));
        assert_eq!(
            in_flight(&reqs).as_deref(),
            Some("a"),
            "a second start of b is no newer"
        );
        reqs.end(id(), Ended::Call("a"), HOOK, false);
        assert_eq!(in_flight(&reqs).as_deref(), Some("b"));
        reqs.end(id(), Ended::Call("b"), HOOK, false);
        assert_eq!(in_flight(&reqs), None);
    }

    fn raise(
        call: Option<&'static str>,
        tool_name: Option<&'static str>,
        from: Transport,
    ) -> Raise<'static> {
        Raise {
            call,
            tool_name,
            turn: None,
            from,
        }
    }

    /// A standing request raised on `from` over `starts`.
    fn raised(starts: Vec<PendingStart>, r: Raise<'_>) -> ActionRequests {
        let mut reqs = ActionRequests::default();
        for s in starts {
            reqs.register(id(), s);
        }
        reqs.raise(id(), r, false);
        reqs
    }

    fn members(reqs: &ActionRequests) -> Vec<&str> {
        reqs.open[&id()].members.iter().map(|m| &*m.call).collect()
    }

    #[test]
    fn bind_takes_the_strongest_key_the_wire_gives() {
        let two = || {
            vec![
                pending("a", Some("Bash"), HOOK),
                pending("b", Some("Read"), HOOK),
            ]
        };
        for (what, starts, r, want) in [
            (
                "a named call",
                two(),
                raise(Some("x"), Some("Bash"), HOOK),
                vec!["x"],
            ),
            (
                "the tool name",
                two(),
                raise(None, Some("Bash"), HOOK),
                vec!["a"],
            ),
            (
                "the sole pending start",
                vec![pending("a", None, HOOK), pending("j", None, JSONL)],
                raise(None, None, HOOK),
                vec!["a"],
            ),
            (
                "none: two pending, no key",
                two(),
                raise(None, None, HOOK),
                vec![],
            ),
            (
                "none: a name no pending start has",
                two(),
                raise(None, Some("Edit"), HOOK),
                vec![],
            ),
            (
                "none: the other transport's start",
                vec![pending("a", Some("Bash"), JSONL)],
                raise(None, Some("Bash"), HOOK),
                vec![],
            ),
        ] {
            assert_eq!(members(&raised(starts, r)), want, "{what}");
        }
    }

    #[test]
    fn a_named_request_binds_every_pending_start_of_its_tool_in_its_turn() {
        let mut reqs = ActionRequests::default();
        for (call, t) in [("a", "t1"), ("b", "t1"), ("c", "t0")] {
            reqs.register(
                id(),
                PendingStart {
                    turn: Some(turn(t)),
                    ..pending(call, Some("Bash"), HOOK)
                },
            );
        }
        let t1 = turn("t1");
        reqs.raise(
            id(),
            Raise {
                turn: Some(&t1),
                ..raise(None, Some("Bash"), HOOK)
            },
            false,
        );
        assert_eq!(members(&reqs), ["a", "b"]);
    }

    #[test]
    fn a_pre_use_delivered_after_its_request_joins_it() {
        let mut reqs = raised(vec![], raise(None, Some("Bash"), HOOK));
        reqs.register(id(), pending("other", Some("Read"), HOOK));
        reqs.register(id(), pending("a", Some("Bash"), HOOK));
        assert_eq!(members(&reqs), ["a"]);
        assert!(
            !reqs.start(id(), Some("a"), HOOK, true),
            "a bound member's own start is its pre-use, not an answer"
        );
    }

    #[test]
    fn a_re_raise_keeps_the_first_raise_and_adds_only_its_own_key() {
        let mut reqs = raised(vec![pending("a", None, HOOK)], raise(None, None, HOOK));
        reqs.raise(id(), raise(None, None, JSONL), true);
        assert_eq!(members(&reqs), ["a"], "a raise with no key changes nothing");
        assert!(
            !reqs.resolves(&id(), Seen::End(turn_end(None)), JSONL),
            "and keeps the transport that raised it"
        );
        reqs.raise(id(), raise(Some("y"), None, HOOK), true);
        assert_eq!(members(&reqs), ["a", "y"]);
    }

    fn turn_end(k: Option<&TurnKey>) -> Ended<'_> {
        Ended::Turn {
            turn: k,
            implied: None,
            turnless: k.is_none(),
        }
    }

    #[test]
    fn resolves_holds_each_rule() {
        let none = HashSet::new();
        let tasks: HashSet<String> = ["task".to_string()].into();
        let (t1, t2) = (turn("t1"), turn("t2"));
        let named = || raised(vec![], raise(Some("x"), None, HOOK));
        let bound = || raised(vec![pending("x", None, HOOK)], raise(None, None, HOOK));
        let keyless = |from| raised(vec![], raise(None, None, from));
        let scoped = || {
            let mut reqs = ActionRequests::default();
            reqs.raise(
                id(),
                Raise {
                    turn: Some(&t1),
                    ..raise(None, Some("Bash"), HOOK)
                },
                false,
            );
            reqs
        };
        let ran = Resolution::Call("x".into());
        let stopped = Resolution::Turn(t1.clone());
        for (what, reqs, seen, from, want) in [
            (
                "a named member's resume",
                named(),
                Seen::Start(Some("x")),
                HOOK,
                true,
            ),
            (
                "a named member's transcript start",
                named(),
                Seen::Start(Some("x")),
                JSONL,
                false,
            ),
            (
                "a bound member's own start",
                bound(),
                Seen::Start(Some("x")),
                HOOK,
                false,
            ),
            (
                "a non-member's start",
                bound(),
                Seen::Start(Some("y")),
                HOOK,
                false,
            ),
            (
                "a keyless request, its own transport",
                keyless(HOOK),
                Seen::Start(Some("y")),
                HOOK,
                true,
            ),
            (
                "a keyless request, the other transport",
                keyless(HOOK),
                Seen::Start(None),
                JSONL,
                false,
            ),
            (
                "a request waiting on its tool",
                scoped(),
                Seen::Start(Some("y")),
                HOOK,
                false,
            ),
            (
                "a member's end, either channel",
                named(),
                Seen::End(Ended::Call("x")),
                JSONL,
                true,
            ),
            (
                "a non-member's end",
                named(),
                Seen::End(Ended::Call("y")),
                HOOK,
                false,
            ),
            ("a member's run", bound(), Seen::Resolved(&ran), HOOK, true),
            (
                "its turn's stop",
                scoped(),
                Seen::Resolved(&stopped),
                HOOK,
                true,
            ),
            (
                "a turnless request's stop",
                bound(),
                Seen::Resolved(&stopped),
                HOOK,
                false,
            ),
            (
                "an id-less hook end, nothing named",
                bound(),
                Seen::End(Ended::Unnamed),
                HOOK,
                true,
            ),
            (
                "an id-less hook end, a call named",
                named(),
                Seen::End(Ended::Unnamed),
                HOOK,
                false,
            ),
            (
                "an id-less transcript end",
                keyless(JSONL),
                Seen::End(Ended::Unnamed),
                JSONL,
                false,
            ),
            (
                "its turn's end",
                scoped(),
                Seen::End(turn_end(Some(&t1))),
                JSONL,
                true,
            ),
            (
                "another turn's end",
                scoped(),
                Seen::End(turn_end(Some(&t2))),
                HOOK,
                false,
            ),
            (
                "an end proving its turn ended",
                scoped(),
                Seen::End(Ended::Turn {
                    turn: Some(&t2),
                    implied: Some(&t1),
                    turnless: false,
                }),
                JSONL,
                true,
            ),
            (
                "a turnless request, a keyed end",
                bound(),
                Seen::End(turn_end(Some(&t1))),
                HOOK,
                false,
            ),
            (
                "a turnless request, the end of a turn whose start went unseen",
                bound(),
                Seen::End(Ended::Turn {
                    turn: Some(&t1),
                    implied: None,
                    turnless: true,
                }),
                HOOK,
                true,
            ),
            (
                "a hook turn end",
                named(),
                Seen::End(turn_end(None)),
                HOOK,
                true,
            ),
            (
                "a transcript turn end, hook-raised",
                keyless(HOOK),
                Seen::End(turn_end(None)),
                JSONL,
                false,
            ),
            (
                "a transcript turn end, its own",
                keyless(JSONL),
                Seen::End(turn_end(None)),
                JSONL,
                true,
            ),
            (
                "a child under its Task's prompt",
                raised(vec![pending("task", None, HOOK)], raise(None, None, HOOK)),
                Seen::ChildResumed(&tasks),
                HOOK,
                true,
            ),
            (
                "a child past its parent's own call",
                bound(),
                Seen::ChildResumed(&tasks),
                HOOK,
                false,
            ),
            (
                "a child under a bare prompt",
                keyless(HOOK),
                Seen::ChildResumed(&none),
                HOOK,
                true,
            ),
        ] {
            assert_eq!(reqs.resolves(&id(), seen, from), want, "{what}");
        }
        assert!(
            !ActionRequests::default().resolves(&id(), Seen::End(turn_end(None)), HOOK),
            "no request, nothing to answer"
        );
    }

    #[test]
    fn a_raise_for_another_turn_replaces_the_standing_request() {
        let (n, m) = (turn("n"), turn("m"));
        let scoped = |call, t| Raise {
            turn: Some(t),
            ..raise(Some(call), None, HOOK)
        };
        let mut reqs = ActionRequests::default();
        reqs.raise(id(), scoped("xn", &n), false);
        reqs.raise(id(), scoped("xn2", &n), true);
        assert_eq!(members(&reqs), ["xn", "xn2"], "the same turn's raise joins");
        reqs.raise(id(), scoped("xm", &m), true);
        assert_eq!(members(&reqs), ["xm"]);
        assert!(!reqs.resolves(&id(), Seen::End(turn_end(Some(&n))), JSONL));
        assert!(reqs.resolves(&id(), Seen::End(turn_end(Some(&m))), JSONL));
    }

    #[test]
    fn only_the_last_member_answers() {
        let mut reqs = raised(vec![], raise(Some("x"), None, HOOK));
        reqs.raise(id(), raise(Some("y"), None, HOOK), true);
        assert!(!reqs.start(id(), Some("x"), HOOK, true), "y still waits");
        assert_eq!(members(&reqs), ["y"], "x's resume retired it");
        assert!(!reqs.end(id(), Ended::Call("z"), HOOK, true));
        assert!(reqs.end(id(), Ended::Call("y"), JSONL, true), "a denial");
        assert!(!reqs.contains(&id()));
    }

    #[test]
    fn a_drained_last_member_answers_nothing() {
        let mut reqs = raised(vec![pending("task", None, HOOK)], raise(None, None, HOOK));
        reqs.retire(&id(), "task");
        assert!(reqs.contains(&id()), "#152: the request is kept");
        assert!(!reqs.resolves(&id(), Seen::End(Ended::Call("task")), HOOK));
        assert!(
            reqs.pending.get(&id()).is_none_or(Vec::is_empty),
            "nor is the Task pending"
        );
    }

    #[test]
    fn a_resumed_child_closes_its_parents_request_only_while_it_waits() {
        let tasks: HashSet<String> = ["task".to_string()].into();
        let mut reqs = raised(vec![pending("task", None, HOOK)], raise(None, None, HOOK));
        assert!(!reqs.child_resumed(id(), &tasks, false), "no wait stood");
        assert!(reqs.contains(&id()));
        assert!(reqs.child_resumed(id(), &tasks, true));
        assert!(!reqs.contains(&id()));
    }

    #[test]
    fn a_start_outside_a_wait_runs_and_drops_a_stale_request() {
        let mut reqs = raised(vec![], raise(Some("x"), None, HOOK));
        assert!(reqs.start(id(), Some("y"), JSONL, false));
        assert!(!reqs.contains(&id()));
    }

    #[test]
    fn an_answer_retires_its_call_from_the_pending_starts() {
        let mut reqs = raised(
            vec![pending("x", Some("Bash"), HOOK)],
            raise(None, Some("Bash"), HOOK),
        );
        assert!(reqs.resolved(id(), &Resolution::Call("x".into()), true));
        assert!(reqs.pending[&id()].is_empty());
    }

    #[test]
    fn a_turn_end_retires_its_turns_pending_starts() {
        let (t1, t2) = (turn("t1"), turn("t2"));
        let mut reqs = ActionRequests::default();
        for (call, t, on) in [
            ("a", Some(&t1), HOOK),
            ("b", Some(&t2), HOOK),
            ("c", None, HOOK),
            ("d", None, JSONL),
        ] {
            reqs.register(
                id(),
                PendingStart {
                    turn: t.cloned(),
                    ..pending(call, None, on)
                },
            );
        }
        let calls = |reqs: &ActionRequests| -> Vec<String> {
            reqs.pending[&id()]
                .iter()
                .map(|s| s.call.to_string())
                .collect()
        };
        reqs.end(id(), turn_end(Some(&t1)), JSONL, false);
        assert_eq!(calls(&reqs), ["b", "c", "d"]);
        reqs.end(id(), turn_end(None), JSONL, false);
        assert_eq!(
            calls(&reqs),
            ["b", "c"],
            "a transcript end leaves the hook's"
        );
        let start_unseen = Ended::Turn {
            turn: Some(&t1),
            implied: None,
            turnless: true,
        };
        reqs.end(id(), start_unseen, HOOK, false);
        assert_eq!(calls(&reqs), ["b"], "a turnless end retires the turnless");
    }

    /// A pre-use and its start, or the two transports' starts of one call,
    /// are one pending call.
    #[test]
    fn a_calls_second_registration_is_idempotent() {
        let mut reqs = ActionRequests::default();
        reqs.register(id(), pending("a", Some("Bash"), HOOK));
        reqs.register(id(), pending("a", None, HOOK));
        let once = format!("{:?}", reqs.pending[&id()]);
        reqs.register(id(), pending("a", Some("Bash"), HOOK));
        assert_eq!(format!("{:?}", reqs.pending[&id()]), once);
        reqs.register(id(), pending("a", None, JSONL));
        let [a] = &reqs.pending[&id()][..] else {
            panic!("one entry per call: {:?}", reqs.pending[&id()]);
        };
        assert_eq!(a.tool_name.as_deref(), Some("Bash"), "the name is kept");
        assert_eq!(a.seen_on, [HOOK, JSONL]);
    }
}
