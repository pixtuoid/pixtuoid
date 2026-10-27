//! The agents' open user-action requests (a slot's `Waiting`): each one's
//! member calls, the transport that raised it, and `ActionRequests::resolves`,
//! the one rule deciding which event answers it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::AgentId;
use crate::source::Transport;

/// What an end names.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Ended<'a> {
    /// A call's own end.
    Call(&'a str),
    /// A tool end naming no call: its wire carries none (Reasonix's
    /// PostToolUse), or its decoder drops it (Cursor, deliberately).
    Unnamed,
    /// An [`AgentEvent::TurnEnd`](crate::AgentEvent::TurnEnd).
    Turn,
}

/// An event as a request's resolution reads it.
#[derive(Clone, Copy, Debug)]
enum Seen<'a> {
    /// A call started; its id when the wire names one.
    Start(Option<&'a str>),
    End(Ended<'a>),
    /// An unnamed child's event, dropped while these Tasks are in flight: the
    /// child resumed.
    ChildResumed(&'a HashSet<String>),
}

#[derive(Debug)]
struct ActionRequest {
    /// The calls it holds on: named by the wire, which can queue several
    /// (#951), or the one in flight when it was raised (a CC permission
    /// Notification fires mid-tool). A member's end means the prompt was
    /// answered; a parallel call's end is no member's, so it cannot.
    members: Vec<Arc<str>>,
    /// A raise named its call on the wire.
    named: bool,
    raised_on: Transport,
}

impl ActionRequest {
    fn new(raised_on: Transport) -> Self {
        Self {
            members: Vec::new(),
            named: false,
            raised_on,
        }
    }

    fn is_member(&self, call: &str) -> bool {
        self.members.iter().any(|m| &**m == call)
    }
}

/// Every agent's open request. An entry lives until a fresh start outside a
/// wait, an answer naming no call, or the slot's removal; whether it stands is
/// the slot's `Waiting`.
#[derive(Debug, Default)]
pub(crate) struct ActionRequests(HashMap<AgentId, ActionRequest>);

impl ActionRequests {
    /// `id` raised a request on `from`. `call` is the call its wire names,
    /// `in_flight` the call its slot was running, and `waiting` whether one
    /// already stands, which a raise naming nothing new re-notifies (CC follows
    /// its tool-less PermissionRequest with a Notification) and which keeps the
    /// transport that raised it.
    pub(crate) fn raise(
        &mut self,
        id: AgentId,
        call: Option<&str>,
        in_flight: Option<&Arc<str>>,
        waiting: bool,
        from: Transport,
    ) {
        let req = match (call, in_flight) {
            // Replaced, not pushed: an inferred member is a guess, and stale
            // guesses would accrete with nothing to clear them.
            (None, Some(t)) => {
                let mut req = ActionRequest::new(from);
                req.members.push(Arc::clone(t));
                self.0.insert(id, req);
                return;
            }
            (None, None) if !waiting => {
                self.0.insert(id, ActionRequest::new(from));
                return;
            }
            _ => self.0.entry(id).or_insert_with(|| ActionRequest::new(from)),
        };
        if !waiting {
            req.raised_on = from;
        }
        if let Some(c) = call {
            // Pushed: a sibling a parallel approval queued stays.
            if !req.is_member(c) {
                req.members.push(Arc::from(c));
            }
            req.named = true;
        }
    }

    /// Whether `seen`, arriving on `from`, answers `id`'s standing request.
    fn resolves(&self, id: &AgentId, seen: Seen<'_>, from: Transport) -> bool {
        let Some(req) = self.0.get(id) else {
            return false;
        };
        match seen {
            // On a hook it is the approval's resume. The transcript's start of
            // the same call lands while the human still decides (#951).
            Seen::Start(Some(c)) if req.is_member(c) => from == Transport::Hook,
            // A request with no member is answered once the agent works again
            // (codex's rollout resume answers its hook's prompt); a start of a
            // call that is no member only counts.
            Seen::Start(_) => req.members.is_empty(),
            Seen::End(Ended::Call(c)) => req.is_member(c),
            // Reasonix: its prompt and its tool's end both name no call.
            Seen::End(Ended::Unnamed) => from == Transport::Hook && !req.named,
            // A pending request blocks the turn. The transcript may read a
            // turn's end past a hook's fresh raise, so it answers only its own.
            Seen::End(Ended::Turn) => from == Transport::Hook || req.raised_on == Transport::Jsonl,
            // The child resumed, unless a member is the parent's own call.
            Seen::ChildResumed(tasks) => req.members.iter().all(|m| tasks.contains(&**m)),
        }
    }

    /// A start of `call` on `from`: whether the slot runs it, else it only
    /// counts. `waiting` is whether a request stands; `counted`, whether the
    /// call was seen before (a late transcript twin, whose approval round may
    /// have queued siblings).
    pub(crate) fn start(
        &mut self,
        id: AgentId,
        call: Option<&str>,
        from: Transport,
        waiting: bool,
        counted: bool,
    ) -> bool {
        let member = call.is_some_and(|c| self.is_member(&id, c));
        let runs = if waiting {
            self.resolves(&id, Seen::Start(call), from)
        } else {
            // A queued member's transcript start: its approval is still to come.
            !(member && from == Transport::Jsonl)
        };
        match call {
            Some(c) if runs && member => self.retire(&id, c),
            // A start outside a wait moots its stale members.
            _ if runs && !waiting && !counted => self.close(&id),
            _ => {}
        }
        runs
    }

    /// An end on `from`: whether it answers the standing request. A call's
    /// end retires that call whether or not it answered (a denial, or a
    /// parallel call that was a member too); an end naming no call that
    /// answers closes the whole request.
    pub(crate) fn end(
        &mut self,
        id: AgentId,
        ended: Ended<'_>,
        from: Transport,
        waiting: bool,
    ) -> bool {
        let resolved = waiting && self.resolves(&id, Seen::End(ended), from);
        match ended {
            Ended::Call(c) => self.retire(&id, c),
            _ if resolved => self.close(&id),
            _ => {}
        }
        resolved
    }

    /// An unnamed child's event dropped while `tasks` are in flight: whether
    /// it answers the standing request, which it then closes.
    pub(crate) fn child_resumed(
        &mut self,
        id: AgentId,
        tasks: &HashSet<String>,
        waiting: bool,
    ) -> bool {
        let resolved = waiting && self.resolves(&id, Seen::ChildResumed(tasks), Transport::Hook);
        if resolved {
            self.close(&id);
        }
        resolved
    }

    pub(crate) fn is_member(&self, id: &AgentId, call: &str) -> bool {
        self.0.get(id).is_some_and(|req| req.is_member(call))
    }

    /// `call` is no longer a member. Retiring the last one answers nothing: a
    /// drained Task's end may be a subagent's, past its parent's own prompt
    /// (#152).
    pub(crate) fn retire(&mut self, id: &AgentId, call: &str) {
        if let Some(req) = self.0.get_mut(id) {
            req.members.retain(|m| &**m != call);
        }
    }

    pub(crate) fn close(&mut self, id: &AgentId) {
        self.0.remove(id);
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&AgentId) -> bool) {
        self.0.retain(|id, _| keep(id));
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, id: &AgentId) -> bool {
        self.0.contains_key(id)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.len()
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

    /// A standing request raised on `from`, naming `named` or inferring
    /// `in_flight`.
    fn raised(named: Option<&str>, in_flight: Option<&str>, from: Transport) -> ActionRequests {
        let mut reqs = ActionRequests::default();
        let in_flight = in_flight.map(Arc::<str>::from);
        reqs.raise(id(), named, in_flight.as_ref(), false, from);
        reqs
    }

    #[test]
    fn resolves_holds_each_rule() {
        let none = HashSet::new();
        let tasks: HashSet<String> = ["task".to_string()].into();
        let named = || raised(Some("x"), None, HOOK);
        let inferred = || raised(None, Some("x"), HOOK);
        let bare = |from| raised(None, None, from);
        for (what, reqs, seen, from, want) in [
            (
                "a member's resume",
                named(),
                Seen::Start(Some("x")),
                HOOK,
                true,
            ),
            (
                "a member's transcript start",
                named(),
                Seen::Start(Some("x")),
                JSONL,
                false,
            ),
            (
                "a non-member's start",
                inferred(),
                Seen::Start(Some("y")),
                HOOK,
                false,
            ),
            (
                "a bare request, its own transport",
                bare(HOOK),
                Seen::Start(Some("y")),
                HOOK,
                true,
            ),
            (
                "a bare request, the other transport",
                bare(HOOK),
                Seen::Start(None),
                JSONL,
                true,
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
            (
                "an id-less hook end, nothing named",
                inferred(),
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
                bare(JSONL),
                Seen::End(Ended::Unnamed),
                JSONL,
                false,
            ),
            (
                "a hook turn end",
                named(),
                Seen::End(Ended::Turn),
                HOOK,
                true,
            ),
            (
                "a transcript turn end, hook-raised",
                bare(HOOK),
                Seen::End(Ended::Turn),
                JSONL,
                false,
            ),
            (
                "a transcript turn end, its own",
                bare(JSONL),
                Seen::End(Ended::Turn),
                JSONL,
                true,
            ),
            (
                "a child under its Task's prompt",
                raised(None, Some("task"), HOOK),
                Seen::ChildResumed(&tasks),
                HOOK,
                true,
            ),
            (
                "a child past its parent's own call",
                inferred(),
                Seen::ChildResumed(&tasks),
                HOOK,
                false,
            ),
            (
                "a child under a bare prompt",
                bare(HOOK),
                Seen::ChildResumed(&none),
                HOOK,
                true,
            ),
        ] {
            assert_eq!(reqs.resolves(&id(), seen, from), want, "{what}");
        }
        assert!(
            !ActionRequests::default().resolves(&id(), Seen::End(Ended::Turn), HOOK),
            "no request, nothing to answer"
        );
    }

    #[test]
    fn a_re_raise_keeps_the_first_raise_and_a_named_one_queues() {
        let mut reqs = raised(None, Some("x"), HOOK);
        reqs.raise(id(), None, None, true, JSONL);
        assert!(
            reqs.is_member(&id(), "x"),
            "a re-notification keeps the member"
        );
        assert!(
            !reqs.resolves(&id(), Seen::End(Ended::Turn), JSONL),
            "and the transport that raised it"
        );
        reqs.raise(id(), Some("y"), None, true, HOOK);
        assert!(reqs.is_member(&id(), "x") && reqs.is_member(&id(), "y"));
    }

    #[test]
    fn a_drained_last_member_answers_nothing() {
        let mut reqs = raised(None, Some("task"), HOOK);
        reqs.retire(&id(), "task");
        assert!(reqs.contains(&id()), "#152: the request is kept");
        assert!(!reqs.resolves(&id(), Seen::End(Ended::Call("task")), HOOK));
    }

    #[test]
    fn a_resumed_child_closes_its_parents_request_only_while_it_waits() {
        let tasks: HashSet<String> = ["task".to_string()].into();
        let mut reqs = raised(None, Some("task"), HOOK);
        assert!(!reqs.child_resumed(id(), &tasks, false), "no wait stood");
        assert!(reqs.contains(&id()));
        assert!(reqs.child_resumed(id(), &tasks, true));
        assert!(!reqs.contains(&id()));
    }

    #[test]
    fn a_start_outside_a_wait_holds_a_queued_member_and_moots_the_rest() {
        let mut reqs = raised(Some("x"), None, HOOK);
        assert!(
            !reqs.start(id(), Some("x"), JSONL, false, false),
            "a queued member's transcript start only counts"
        );
        assert!(reqs.start(id(), Some("x"), HOOK, false, false));
        assert!(reqs.contains(&id()) && !reqs.is_member(&id(), "x"));
        let mut reqs = raised(Some("x"), None, HOOK);
        assert!(reqs.start(id(), Some("y"), HOOK, false, true));
        assert!(reqs.is_member(&id(), "x"), "a counted twin moots nothing");
        assert!(reqs.start(id(), Some("y"), HOOK, false, false));
        assert!(!reqs.contains(&id()), "a fresh start moots the request");
    }

    #[test]
    fn an_end_retires_its_call_and_a_bare_answer_closes() {
        let mut reqs = raised(Some("x"), None, HOOK);
        reqs.raise(id(), Some("y"), None, true, HOOK);
        assert!(reqs.end(id(), Ended::Call("x"), HOOK, true));
        assert!(reqs.is_member(&id(), "y"), "the queued sibling stays");
        assert!(
            !reqs.end(id(), Ended::Call("y"), HOOK, false),
            "no wait stood"
        );
        assert!(reqs.contains(&id()) && !reqs.is_member(&id(), "y"));
        let mut reqs = raised(None, None, HOOK);
        assert!(reqs.end(id(), Ended::Turn, HOOK, true));
        assert!(!reqs.contains(&id()));
    }
}
