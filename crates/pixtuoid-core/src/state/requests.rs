//! The agents' open user-action requests (a slot's `RequiresAction`), with the
//! two rules every event meets them through: which of the call ledger's pending
//! calls a request holds (the call its raise names, the sole one by deduction,
//! its tools' by `by_name`), and `resolves`, which event answers it.

use std::collections::HashMap;
use std::sync::Arc;

use crate::AgentId;
use crate::source::{Resolution, Transport};
use crate::state::TurnKey;
use crate::state::calls::{CallLedger, PendingCall};

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

impl Ended<'_> {
    /// Whether it ends what `scope` scopes: that turn, or what no turn scoped.
    pub(crate) fn closes(&self, scope: Option<&TurnKey>) -> bool {
        match (self, scope) {
            (Ended::Turn { turn, implied, .. }, Some(t)) => Some(t) == *turn || Some(t) == *implied,
            (Ended::Turn { turnless, .. }, None) => *turnless,
            (Ended::Call(_) | Ended::Unnamed, _) => false,
        }
    }
}

/// An event as a request's resolution reads it.
#[derive(Clone, Copy, Debug)]
enum Seen<'a> {
    /// A call started, on this transport.
    Start(Transport),
    /// An end, and the transport it arrived on.
    End(Ended<'a>, Transport),
    Resolved(&'a Resolution),
    /// An unnamed child's event, dropped while its parent's Tasks are in
    /// flight: the child resumed.
    ChildResumed(&'a CallLedger),
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
    /// The call that dispatched the one it names, when the wire names it.
    pub(crate) parent: Option<&'a str>,
}

#[derive(Debug, Clone)]
struct Member {
    call: Arc<str>,
    /// The wire named it, so its approval answers for it (#951), and a
    /// drained Task answers for it alone (#152).
    named: bool,
    /// The call that dispatched it, whose end ends it too: omp closes an
    /// interrupted eval's nested requests with the eval's tool result alone
    /// (`session/tool-approval-records.ts:30-36` at v19.1.0).
    parent: Option<Arc<str>>,
}

impl Member {
    /// Whether `call`'s end ends it: its own, or its parent's.
    fn ended_by(&self, call: &str) -> bool {
        &*self.call == call || self.parent.as_deref() == Some(call)
    }
}

#[derive(Debug, Clone)]
struct ActionRequest {
    /// The calls it holds on. Its last member's end, or the end of the call
    /// that dispatched it, means the prompt was answered; a parallel call's
    /// end is neither, so it cannot.
    members: Vec<Member>,
    /// A raise named its call on the wire.
    named: bool,
    /// The tools its raises naming no call named: its keys to their pending
    /// calls in its turn.
    tool_names: Vec<Arc<str>>,
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

    /// Whether `call`'s end leaves it holding no call.
    fn ended_by(&self, call: &str) -> bool {
        !self.members.is_empty() && self.members.iter().all(|m| m.ended_by(call))
    }

    /// The wire gave no key to bind it by, so a start on the transport that
    /// raised it, its call's first there or one naming no call, is the agent
    /// working again.
    fn is_keyless(&self) -> bool {
        self.members.is_empty() && self.tool_names.is_empty()
    }

    /// The pending calls of its turn it holds by the name of their tool, which
    /// neither Claude Code's hooks nor codex's rewrite: Claude Code's may
    /// replace a call's input alone
    /// (code.claude.com/docs/en/hooks#pretooluse-decision-control,
    /// #permissionrequest-decision-control), and codex's outputs name no tool
    /// (`codex-rs/hooks/schema/generated/pre-tool-use.command.output.schema.json:12-43`,
    /// `permission-request.command.output.schema.json:12-40` at rust-v0.171.0).
    fn by_name<'a>(
        &'a self,
        id: &AgentId,
        calls: &'a CallLedger,
    ) -> impl Iterator<Item = PendingCall<'a>> + use<'a> {
        calls.pending_in(id, self.turn.as_ref()).filter(|p| {
            p.tool_name
                .is_some_and(|name| self.tool_names.iter().any(|t| &**t == name))
        })
    }

    fn holds(&mut self, call: &Arc<str>, named: bool, parent: Option<&str>) {
        if self.member(call).is_none() {
            self.members.push(Member {
                call: Arc::clone(call),
                named,
                parent: parent.map(Arc::from),
            });
        }
    }
}

/// Every agent's open request. A request lives until an event answers it or
/// its slot goes; whether it stands is the slot's `RequiresAction`.
#[derive(Debug, Default, Clone)]
pub(crate) struct ActionRequests {
    open: HashMap<AgentId, ActionRequest>,
}

impl ActionRequests {
    /// `id` raised a request over `calls`. `requires_action` is whether one
    /// already stands: a second raise keeps the first one's members and
    /// identity, and adds only the calls its own key binds. A raise for another
    /// turn replaces it: a thread runs one turn at a time, so the standing
    /// request's turn is over, and its lagging ends must not answer this one.
    pub(crate) fn raise(
        &mut self,
        id: AgentId,
        raise: Raise<'_>,
        requires_action: bool,
        calls: &CallLedger,
    ) {
        let standing = self.open.get(&id).filter(|req| {
            requires_action
                && !matches!((&req.turn, raise.turn), (Some(standing), Some(new)) if standing != new)
        });
        if standing.is_none() {
            let mut fresh = ActionRequest {
                members: Vec::new(),
                named: false,
                tool_names: Vec::new(),
                turn: raise.turn.cloned(),
                raised_on: raise.from,
            };
            // The sole pending call is a deduction, not a key, so only a fresh
            // request the wire gave no key draws it, and only from the calls
            // seen on its transport: the other's copy of a queued call can land
            // while the human still decides (#951).
            if raise.call.is_none() && raise.tool_name.is_none() {
                let mut seen_here = calls
                    .pending_in(&id, raise.turn)
                    .filter(|p| p.seen_on(raise.from));
                if let (Some(sole), None) = (seen_here.next(), seen_here.next()) {
                    fresh.holds(sole.call, false, None);
                }
            }
            self.open.insert(id, fresh);
        }
        let Some(req) = self.open.get_mut(&id) else {
            return;
        };
        // The strongest key the raise gives: the call it names, else the tool.
        if let Some(call) = raise.call {
            req.holds(&Arc::from(call), true, raise.parent);
            req.named = true;
        } else if let Some(name) = raise.tool_name {
            if !req.tool_names.iter().any(|t| &**t == name) {
                req.tool_names.push(Arc::from(name));
            }
            let bound: Vec<Arc<str>> = req
                .by_name(&id, calls)
                .map(|p| Arc::clone(p.call))
                .collect();
            for call in &bound {
                req.holds(call, false, None);
            }
        }
    }

    /// A pre-use or start registered `call` pending: the standing request
    /// binds it now by the tools its raises named, so a pre-use delivered
    /// after its request still joins (#242).
    pub(crate) fn join(&mut self, id: AgentId, call: &str, calls: &CallLedger) {
        let Some(req) = self.open.get_mut(&id) else {
            return;
        };
        let joins = (req.by_name(&id, calls))
            .find(|p| &**p.call == call)
            .map(|p| Arc::clone(p.call));
        if let Some(call) = joins {
            req.holds(&call, false, None);
        }
    }

    /// A start on `on` that raised its call's record, its call's first start
    /// there, or one naming no call: whether the slot runs it, else it only
    /// counts. `requires_action` is whether a request stands.
    pub(crate) fn start(&mut self, id: AgentId, requires_action: bool, on: Transport) -> bool {
        let resolved = !requires_action || self.resolves(&id, Seen::Start(on));
        if resolved {
            self.open.remove(&id);
        }
        resolved
    }

    /// An end on `from`: whether it answers the standing request. A call's
    /// end retires that call whether or not it answered (a denial, or a
    /// parallel call that was a member too).
    pub(crate) fn end(
        &mut self,
        id: AgentId,
        ended: Ended<'_>,
        from: Transport,
        requires_action: bool,
    ) -> bool {
        let resolved = requires_action && self.resolves(&id, Seen::End(ended, from));
        if let Ended::Call(c) = ended {
            self.retire(&id, c);
        }
        if resolved {
            self.open.remove(&id);
        }
        resolved
    }

    /// Whether `id`'s request names `call` on the wire.
    fn names(&self, id: &AgentId, call: &str) -> bool {
        self.open
            .get(id)
            .and_then(|req| req.member(call))
            .is_some_and(|m| m.named)
    }

    /// A drained Task's end, as [`Self::end`], answering only a request that
    /// [names](Self::names) it: a subagent returning answers no prompt a
    /// deduction bound it to (#152).
    pub(crate) fn drained(
        &mut self,
        id: AgentId,
        task: &str,
        from: Transport,
        requires_action: bool,
    ) -> bool {
        let named = self.names(&id, task);
        self.end(id, Ended::Call(task), from, requires_action && named)
    }

    /// An answer that ends no item: whether it answers the standing request.
    pub(crate) fn resolved(&mut self, id: AgentId, by: &Resolution, requires_action: bool) -> bool {
        let resolved = requires_action && self.resolves(&id, Seen::Resolved(by));
        match by {
            // An answered member leaves its request, so a replayed approval
            // answers nothing; an approved call runs on, so it leaves only the
            // request.
            Resolution::Approved(c) if self.names(&id, c) => {
                if let Some(req) = self.open.get_mut(&id) {
                    req.members.retain(|m| &*m.call != c);
                }
            }
            Resolution::Approved(_) | Resolution::Turn(_) => {}
        }
        if resolved {
            self.open.remove(&id);
        }
        resolved
    }

    /// An unnamed child's event dropped while `id`'s Tasks in `calls` are in
    /// flight: whether it answers the standing request, which it then closes.
    pub(crate) fn child_resumed(
        &mut self,
        id: AgentId,
        calls: &CallLedger,
        requires_action: bool,
    ) -> bool {
        let resolved = requires_action && self.resolves(&id, Seen::ChildResumed(calls));
        if resolved {
            self.open.remove(&id);
        }
        resolved
    }

    /// Whether `seen` answers `id`'s standing request.
    fn resolves(&self, id: &AgentId, seen: Seen<'_>) -> bool {
        let Some(req) = self.open.get(id) else {
            return false;
        };
        match seen {
            // Only a request the wire gave no key takes a start: a member's own
            // start answers nothing, and an approval resumes by its own fact.
            // The other transport's copy of a queued call can land while the
            // human still decides (#951).
            Seen::Start(on) => req.is_keyless() && on == req.raised_on,
            Seen::End(Ended::Call(c), _) => req.ended_by(c),
            Seen::Resolved(Resolution::Approved(c)) => self.names(id, c) && req.is_last_member(c),
            // Reasonix: its prompt and its tool's end both name no call.
            Seen::End(Ended::Unnamed, from) => from == Transport::Hook && !req.named,
            Seen::End(ended @ Ended::Turn { .. }, _) => ended.closes(req.turn.as_ref()),
            Seen::Resolved(Resolution::Turn(k)) => req.turn.as_ref() == Some(k),
            // The child resumed, unless a member is the parent's own call.
            Seen::ChildResumed(calls) => req
                .members
                .iter()
                .all(|m| calls.is_task_in_flight(id, &m.call)),
        }
    }

    pub(crate) fn is_member(&self, id: &AgentId, call: &str) -> bool {
        self.open
            .get(id)
            .is_some_and(|req| req.member(call).is_some())
    }

    /// `call` ended: it is no longer a member, nor are the calls it
    /// dispatched.
    pub(crate) fn retire(&mut self, id: &AgentId, call: &str) {
        if let Some(req) = self.open.get_mut(id) {
            req.members.retain(|m| !m.ended_by(call));
        }
    }

    /// The slot went: its request with it.
    pub(crate) fn forget(&mut self, id: &AgentId) {
        self.open.remove(id);
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, id: &AgentId) -> bool {
        self.open.contains_key(id)
    }

    /// How many requests stand.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.open.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ToolDetail;
    use crate::state::calls::Fact;

    const HOOK: Transport = Transport::Hook;
    const JSONL: Transport = Transport::Jsonl;

    fn id() -> AgentId {
        AgentId::from_parts("claude-code", "s")
    }

    fn turn(k: &str) -> TurnKey {
        TurnKey::Id(k.into())
    }

    /// A pre-use of a call on a transport, naming its tool where it says one.
    type PreUse = (&'static str, Option<&'static str>, Transport);

    /// An agent's calls and its request.
    #[derive(Default)]
    struct Agent {
        calls: CallLedger,
        reqs: ActionRequests,
    }

    impl Agent {
        /// `call`'s pre-use, scoped to `turn`.
        fn pre_use_in(&mut self, (call, tool_name, on): PreUse, turn: Option<&TurnKey>) {
            let fact = Fact::PreUse { tool_name, turn };
            self.calls.observe(id(), call, fact, on, None);
            self.reqs.join(id(), call, &self.calls);
        }

        fn pre_use(&mut self, pre_use: PreUse) {
            self.pre_use_in(pre_use, None);
        }

        /// `call`'s start on `on`, showing `detail`.
        fn start(&mut self, call: &str, detail: Option<&ToolDetail>, on: Transport) {
            let fact = Fact::Start { copy: None, detail };
            self.calls.observe(id(), call, fact, on, None);
            self.reqs.join(id(), call, &self.calls);
        }

        fn raise(&mut self, r: Raise<'_>, requires_action: bool) {
            self.reqs.raise(id(), r, requires_action, &self.calls);
        }

        fn members(&self) -> Vec<&str> {
            self.reqs.open[&id()]
                .members
                .iter()
                .map(|m| &*m.call)
                .collect()
        }

        fn resolves(&self, seen: Seen<'_>) -> bool {
            self.reqs.resolves(&id(), seen)
        }
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
            parent: None,
        }
    }

    /// A standing request raised over `pre_uses`.
    fn raised(pre_uses: Vec<PreUse>, r: Raise<'_>) -> Agent {
        let mut agent = Agent::default();
        for p in pre_uses {
            agent.pre_use(p);
        }
        agent.raise(r, false);
        agent
    }

    /// A standing request raised over a dispatched Task.
    fn raised_over_a_task(r: Raise<'_>) -> Agent {
        let mut agent = Agent::default();
        agent.start("task", Some(&ToolDetail::Task), HOOK);
        agent.raise(r, false);
        agent
    }

    #[test]
    fn bind_takes_the_strongest_key_the_wire_gives() {
        let two = || vec![("a", Some("Bash"), HOOK), ("b", Some("Read"), HOOK)];
        for (what, pre_uses, r, want) in [
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
                "the sole pending call",
                vec![("a", None, HOOK), ("j", None, JSONL)],
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
                "none: a name no pending call has",
                two(),
                raise(None, Some("Edit"), HOOK),
                vec![],
            ),
        ] {
            assert_eq!(raised(pre_uses, r).members(), want, "{what}");
        }
    }

    /// A tool's name binds its pending calls on either transport, as a call's
    /// id binds its call.
    #[test]
    fn a_raise_naming_a_tool_binds_its_calls_whichever_transport_saw_them() {
        let agent = raised(
            vec![("a", Some("Bash"), JSONL)],
            raise(None, Some("Bash"), HOOK),
        );
        assert_eq!(agent.members(), ["a"], "a raise after its call");
        let mut agent = raised(vec![], raise(None, Some("Bash"), HOOK));
        agent.pre_use(("a", Some("Bash"), JSONL));
        assert_eq!(agent.members(), ["a"], "a pre-use after its raise");
    }

    #[test]
    fn a_raise_naming_a_tool_binds_every_pending_call_of_it_in_its_turn() {
        let mut agent = Agent::default();
        for (call, t) in [("a", "t1"), ("b", "t1"), ("c", "t0")] {
            agent.pre_use_in((call, Some("Bash"), HOOK), Some(&turn(t)));
        }
        let t1 = turn("t1");
        agent.raise(
            Raise {
                turn: Some(&t1),
                ..raise(None, Some("Bash"), HOOK)
            },
            false,
        );
        assert_eq!(agent.members(), ["a", "b"]);
    }

    #[test]
    fn a_pre_use_delivered_after_its_request_joins_it() {
        let mut agent = raised(vec![], raise(None, Some("Bash"), HOOK));
        agent.pre_use(("other", Some("Read"), HOOK));
        agent.pre_use(("a", Some("Bash"), HOOK));
        assert_eq!(agent.members(), ["a"]);
        assert!(
            !agent.reqs.start(id(), true, HOOK),
            "a request naming its tool takes no start"
        );
    }

    #[test]
    fn a_re_raise_keeps_the_first_raise_and_adds_only_its_own_key() {
        let mut agent = raised(vec![("a", None, HOOK)], raise(None, None, HOOK));
        agent.raise(raise(None, None, JSONL), true);
        assert_eq!(
            agent.members(),
            ["a"],
            "a raise with no key changes nothing"
        );
        assert_eq!(
            agent.reqs.open[&id()].raised_on,
            HOOK,
            "and keeps the transport that raised it"
        );
        agent.raise(raise(Some("y"), None, HOOK), true);
        assert_eq!(agent.members(), ["a", "y"]);
        agent.pre_use(("e", Some("Edit"), HOOK));
        agent.raise(raise(None, Some("Edit"), HOOK), true);
        assert_eq!(agent.members(), ["a", "y", "e"]);
    }

    /// A raise naming its call binds that call alone, whichever side of it
    /// another call of its tool lands.
    #[test]
    fn a_raise_naming_its_call_binds_no_other_call_of_its_tool() {
        let mut agent = raised(
            vec![("y", Some("Bash"), HOOK)],
            raise(Some("x"), Some("Bash"), HOOK),
        );
        agent.pre_use(("z", Some("Bash"), HOOK));
        assert_eq!(agent.members(), ["x"]);
    }

    /// A re-raise's tool binds its calls whichever side of the re-raise their
    /// pre-use lands (#242), after a first raise for another tool or for none.
    #[test]
    fn a_re_raises_tool_binds_its_calls_whichever_side_of_it_their_pre_use_lands() {
        let edit = ("e", Some("Edit"), HOOK);
        for first in [raise(None, None, HOOK), raise(None, Some("Bash"), HOOK)] {
            let mut early = raised(vec![edit, ("j", None, HOOK)], first);
            early.raise(raise(None, Some("Edit"), HOOK), true);
            let mut late = raised(vec![], first);
            late.raise(raise(None, Some("Edit"), HOOK), true);
            late.pre_use(edit);
            for (side, agent) in [("before", early), ("after", late)] {
                assert_eq!(agent.members(), ["e"], "{first:?}, a pre-use {side} it");
            }
        }
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
        let none = CallLedger::default();
        let tasks = raised_over_a_task(raise(None, None, HOOK)).calls;
        let (t1, t2) = (turn("t1"), turn("t2"));
        let named = || raised(vec![], raise(Some("x"), None, HOOK));
        let bound = || raised(vec![("x", None, HOOK)], raise(None, None, HOOK));
        let keyless = |from| raised(vec![], raise(None, None, from));
        let scoped = || {
            let mut agent = Agent::default();
            agent.raise(
                Raise {
                    turn: Some(&t1),
                    ..raise(None, Some("Bash"), HOOK)
                },
                false,
            );
            agent
        };
        let approved = Resolution::Approved("x".into());
        let stopped = Resolution::Turn(t1.clone());
        for (what, agent, seen, want) in [
            (
                "a named member's approval",
                named(),
                Seen::Resolved(&approved),
                true,
            ),
            (
                "a bound member's approval",
                bound(),
                Seen::Resolved(&approved),
                false,
            ),
            (
                "a named request, a start",
                named(),
                Seen::Start(HOOK),
                false,
            ),
            (
                "a bound request, a start",
                bound(),
                Seen::Start(HOOK),
                false,
            ),
            (
                "a keyless request, its transport's start",
                keyless(HOOK),
                Seen::Start(HOOK),
                true,
            ),
            (
                "a keyless request, the other transport's start",
                keyless(HOOK),
                Seen::Start(JSONL),
                false,
            ),
            (
                "a request waiting on its tool",
                scoped(),
                Seen::Start(HOOK),
                false,
            ),
            (
                "a member's end, either channel",
                named(),
                Seen::End(Ended::Call("x"), JSONL),
                true,
            ),
            (
                "a non-member's end",
                named(),
                Seen::End(Ended::Call("y"), HOOK),
                false,
            ),
            (
                "a bound member's end",
                bound(),
                Seen::End(Ended::Call("x"), HOOK),
                true,
            ),
            (
                "the end of the call that dispatched a member",
                raised(
                    vec![],
                    Raise {
                        parent: Some("p"),
                        ..raise(Some("n"), None, HOOK)
                    },
                ),
                Seen::End(Ended::Call("p"), JSONL),
                true,
            ),
            ("its turn's stop", scoped(), Seen::Resolved(&stopped), true),
            (
                "a turnless request's stop",
                bound(),
                Seen::Resolved(&stopped),
                false,
            ),
            (
                "an id-less hook end, nothing named",
                bound(),
                Seen::End(Ended::Unnamed, HOOK),
                true,
            ),
            (
                "an id-less hook end, a call named",
                named(),
                Seen::End(Ended::Unnamed, HOOK),
                false,
            ),
            (
                "an id-less transcript end",
                keyless(JSONL),
                Seen::End(Ended::Unnamed, JSONL),
                false,
            ),
            (
                "its turn's end",
                scoped(),
                Seen::End(turn_end(Some(&t1)), JSONL),
                true,
            ),
            (
                "another turn's end",
                scoped(),
                Seen::End(turn_end(Some(&t2)), HOOK),
                false,
            ),
            (
                "an end proving its turn ended",
                scoped(),
                Seen::End(
                    Ended::Turn {
                        turn: Some(&t2),
                        implied: Some(&t1),
                        turnless: false,
                    },
                    JSONL,
                ),
                true,
            ),
            (
                "a turnless request, a keyed end",
                bound(),
                Seen::End(turn_end(Some(&t1)), HOOK),
                false,
            ),
            (
                "a turnless request, the end of a turn whose start went unseen",
                bound(),
                Seen::End(
                    Ended::Turn {
                        turn: Some(&t1),
                        implied: None,
                        turnless: true,
                    },
                    HOOK,
                ),
                true,
            ),
            (
                "a hook turn end",
                named(),
                Seen::End(turn_end(None), HOOK),
                true,
            ),
            (
                "a transcript turn end, hook-raised",
                keyless(HOOK),
                Seen::End(turn_end(None), JSONL),
                true,
            ),
            (
                "a child under its Task's prompt",
                raised_over_a_task(raise(None, None, HOOK)),
                Seen::ChildResumed(&tasks),
                true,
            ),
            (
                "a child past its parent's own call",
                bound(),
                Seen::ChildResumed(&tasks),
                false,
            ),
            (
                "a child under a bare prompt",
                keyless(HOOK),
                Seen::ChildResumed(&none),
                true,
            ),
        ] {
            assert_eq!(agent.resolves(seen), want, "{what}");
        }
        assert!(
            !ActionRequests::default().resolves(&id(), Seen::End(turn_end(None), HOOK)),
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
        let mut agent = Agent::default();
        agent.raise(scoped("xn", &n), false);
        agent.raise(scoped("xn2", &n), true);
        assert_eq!(
            agent.members(),
            ["xn", "xn2"],
            "the same turn's raise joins"
        );
        agent.raise(scoped("xm", &m), true);
        assert_eq!(agent.members(), ["xm"]);
        assert!(!agent.resolves(Seen::End(turn_end(Some(&n)), JSONL)));
        assert!(agent.resolves(Seen::End(turn_end(Some(&m)), JSONL)));
    }

    #[test]
    fn only_the_last_member_answers() {
        let mut agent = raised(vec![], raise(Some("x"), None, HOOK));
        agent.raise(raise(Some("y"), None, HOOK), true);
        let reqs = &mut agent.reqs;
        let x = Resolution::Approved("x".into());
        assert!(!reqs.resolved(id(), &x, true), "y still waits");
        assert_eq!(agent.members(), ["y"], "x's approval retired it");
        let reqs = &mut agent.reqs;
        assert!(!reqs.end(id(), Ended::Call("z"), HOOK, true));
        assert!(reqs.end(id(), Ended::Call("y"), JSONL, true), "a denial");
        assert!(!reqs.contains(&id()));
    }

    #[test]
    fn a_calls_end_answers_for_the_calls_it_dispatched() {
        let nested = Raise {
            parent: Some("p"),
            ..raise(Some("n"), None, JSONL)
        };
        let mut agent = raised(vec![], nested);
        agent.raise(raise(Some("x"), None, JSONL), true);
        assert!(
            !agent.reqs.end(id(), Ended::Call("p"), JSONL, true),
            "x waits"
        );
        assert_eq!(agent.members(), ["x"], "p's end retired n");
        let mut agent = raised(vec![], nested);
        assert!(agent.reqs.end(id(), Ended::Call("p"), JSONL, true));
        assert!(!agent.reqs.contains(&id()));
    }

    #[test]
    fn a_drained_member_bound_by_deduction_answers_nothing() {
        let mut agent = raised_over_a_task(raise(None, None, HOOK));
        assert_eq!(agent.members(), ["task"], "premise: bound by deduction");
        assert!(!agent.reqs.drained(id(), "task", HOOK, true));
        assert!(agent.reqs.contains(&id()), "#152: the request is kept");
    }

    #[test]
    fn a_resumed_child_closes_its_parents_request_only_while_it_waits() {
        let mut agent = raised_over_a_task(raise(None, None, HOOK));
        assert!(
            !agent.reqs.child_resumed(id(), &agent.calls, false),
            "no wait stood"
        );
        assert!(agent.reqs.contains(&id()));
        assert!(agent.reqs.child_resumed(id(), &agent.calls, true));
        assert!(!agent.reqs.contains(&id()));
    }

    #[test]
    fn a_start_outside_a_wait_runs_and_drops_a_stale_request() {
        let mut agent = raised(vec![], raise(Some("x"), None, HOOK));
        assert!(agent.reqs.start(id(), false, JSONL));
        assert!(!agent.reqs.contains(&id()));
    }
}
