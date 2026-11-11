//! Liveness for the sources whose hook plane supplies the agent CLI's pid, the
//! set [`bind_targets`](super::bind_targets) picks.
//!
//! A hook-only source has no tailable transcript and therefore none of the JSONL
//! watcher's liveness ladder; its ONLY exit signal is the best-effort
//! `session_end` hook on a CLEAN quit, so an abrupt exit ghosts the sprite until
//! the reducer's stale sweep (`stale_threshold`). When the shim can stamp the CLI's pid (`_pid`, an
//! ancestor walk past the runner's interposed shell where the OS allows one),
//! [`ExitWatch`] emits a
//! `SessionEnd` the moment that pid dies. Fed ONLY from the hook decode path,
//! so it is inert for sources whose payloads carry no `_pid`.
//!
//! A pid wrong on EVERY hook does not self-heal: acting on the first sighting
//! walked the sprite out and back in for a whole session (#896). So a
//! shim-GUESSED pid arms only when an agent reports it twice in a row, unless its
//! [`Corroboration`](crate::source::registry::Corroboration) is `OnceViaShell`; a
//! pid the source stamped itself arms on sight.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::AgentId;
use crate::source::exit_watch::ExitWatch;
use crate::source::{AgentEvent, TaggedSender, Transport};

/// Cloneable handle (one per hook connection task) over a shared pid→agents
/// registry + the process-exit watcher.
#[derive(Debug, Clone)]
pub(crate) struct HookPidWatch {
    exit: Arc<ExitWatch>,
    bindings: Arc<Mutex<Bindings>>,
}

/// One agent's pid sighting. An agent is armed on AT MOST ONE pid, which is why
/// this is an enum and not a pair of maps: "armed on two pids at once" was
/// reachable when the two states lived in separate collections.
#[derive(Debug)]
enum Sighting {
    /// A shim GUESS seen once — armed only if the next sighting repeats it.
    Candidate(i32),
    /// Watched: its death ends this agent.
    Armed(i32),
}

#[derive(Debug, Default)]
struct Bindings {
    by_agent: HashMap<AgentId, Sighting>,
}

impl HookPidWatch {
    /// Spawn the exit watcher + the drain task that turns a dead pid into a
    /// `SessionEnd` for each agent bound to it. `None` if this platform has no
    /// exit-watch backend or it failed to init — the source then falls back to
    /// `session_end` + the stale-sweep.
    pub(crate) fn spawn(tx: TaggedSender) -> Option<Self> {
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel::<i32>();
        let exit = Arc::new(ExitWatch::spawn(exit_tx)?);
        let this = Self {
            exit,
            bindings: Arc::new(Mutex::new(Bindings::default())),
        };
        let drain = this.clone();
        tokio::spawn(async move {
            while let Some(dead) = exit_rx.recv().await {
                // A SessionEnd for an already-ended agent (the clean-quit case,
                // where `session_end` ended it first) is a reducer no-op.
                for agent_id in drain.take(dead) {
                    if tx
                        .send((
                            Transport::Hook,
                            AgentEvent::SessionEnd {
                                agent_id,
                                as_child: false,
                            },
                        ))
                        .await
                        .is_err()
                    {
                        return; // reducer gone → daemon shutdown
                    }
                }
            }
        });
        Some(this)
    }

    /// Bind `agent_id` to `pid`, arming the exit watch (idempotent). With
    /// `corroborate`, that takes a second consecutive sighting — so callers must
    /// note at most once per hook PAYLOAD, or a batch would confirm its own pid.
    pub(crate) fn note(&self, pid: i32, agent_id: AgentId, corroborate: bool) {
        if self.lock().sight(pid, agent_id, corroborate) {
            self.exit.watch(pid);
        }
    }

    fn take(&self, pid: i32) -> Vec<AgentId> {
        self.lock().take(pid)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Bindings> {
        self.bindings.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Split from the [`ExitWatch`] side so it is unit-testable without spawning the
/// platform watcher thread.
impl Bindings {
    /// Record a sighting; `true` while the pairing is ARMED — not merely on the
    /// sighting that arms it, so every later one re-asserts the watch. The
    /// watcher dedupes an already-registered pid but DROPS a registration that
    /// failed (`exit_watch.rs`'s `Registered::Failed`), so re-asserting is the
    /// only retry; noting once would strand it.
    ///
    /// `corroborate` asks for a second, consecutive sighting before arming —
    /// right for a shim-resolved pid, which is a guess about which ancestor is
    /// the CLI, and wrong for a source that stamps its OWN `process.pid`: that
    /// is a fact, and a session may report it only once. A different pid demotes
    /// an armed agent back to a candidate.
    fn sight(&mut self, pid: i32, agent_id: AgentId, corroborate: bool) -> bool {
        match self.by_agent.get(&agent_id) {
            Some(Sighting::Armed(p)) if *p == pid => return true,
            Some(Sighting::Candidate(p)) if *p == pid => {}
            _ if corroborate => {
                self.by_agent.insert(agent_id, Sighting::Candidate(pid));
                return false;
            }
            _ => {}
        }
        self.by_agent.insert(agent_id, Sighting::Armed(pid));
        true
    }

    /// Remove and return the agents armed on `pid` — the ones its death ends.
    fn take(&mut self, pid: i32) -> Vec<AgentId> {
        let dead: Vec<AgentId> = self
            .by_agent
            .iter()
            .filter(|(_, s)| matches!(s, Sighting::Armed(p) if *p == pid))
            .map(|(agent, _)| *agent)
            .collect();
        for agent in &dead {
            self.by_agent.remove(agent);
        }
        dead
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_returns_a_watch_exactly_where_a_backend_exists() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        assert_eq!(
            HookPidWatch::spawn(tx).is_some(),
            crate::source::exit_watch::HAS_BACKEND
        );
    }

    const GUESS: bool = true;
    const FACT: bool = false;

    /// Two sightings each, so both agents are ARMED on the shared pid.
    #[test]
    fn take_returns_every_agent_armed_on_a_pid_and_only_once() {
        let mut b = Bindings::default();
        let a1 = AgentId::from_parts("codewhale", "/ws");
        let a2 = AgentId::from_parts("codewhale", "agent-child");
        let other = AgentId::from_parts("codewhale", "/other");
        for _ in 0..2 {
            b.sight(4242, a1, GUESS);
            b.sight(4242, a2, GUESS);
            b.sight(99, other, GUESS);
        }

        let mut taken = b.take(4242);
        taken.sort_unstable();
        let mut expected = vec![a1, a2];
        expected.sort_unstable();
        assert_eq!(taken, expected, "both agents armed on the pid are ended");
        assert!(b.take(4242).is_empty(), "removed on the first take");
        assert_eq!(b.take(99).len(), 1);
    }

    #[test]
    fn the_second_sighting_arms_and_later_ones_re_assert_the_watch() {
        let mut b = Bindings::default();
        let a = AgentId::from_parts("codewhale", "/ws");
        assert!(!b.sight(7, a, GUESS), "a first sighting is not a repeat");
        assert!(b.sight(7, a, GUESS), "the second sighting is the repeat");
        assert!(
            b.sight(7, a, GUESS),
            "an armed pairing stays armed, so the caller re-asserts the watch — \
             a registration that FAILED is retried only by re-asserting it"
        );
        assert_eq!(b.take(7).len(), 1, "a re-sighted (pid, agent) is deduped");
    }

    /// A pid the SOURCE stamped is not a guess, and a session with no tool call
    /// reports it exactly once, so it arms on sight.
    #[test]
    fn a_source_stamped_pid_arms_on_the_first_sighting() {
        let mut b = Bindings::default();
        let a = AgentId::from_parts("opencode", "ses_x");
        assert!(
            b.sight(4242, a, FACT),
            "a stamped pid needs no corroboration"
        );
        assert_eq!(b.take(4242), vec![a]);
    }

    /// #896: a per-invocation wrapper reports a FRESH pid each hook, so no
    /// sighting ever repeats and nothing is ever armed.
    #[test]
    fn a_fresh_pid_per_hook_never_arms_and_never_accumulates() {
        let mut b = Bindings::default();
        let agent = AgentId::from_parts("cursor", "sess-A");
        for wrapper_pid in [4808, 5379, 8924, 9869, 13594] {
            assert!(
                !b.sight(wrapper_pid, agent, GUESS),
                "wrapper pid {wrapper_pid} must never read as corroborated"
            );
        }
        assert!(b.take(13594).is_empty(), "nothing may be armed");
        assert_eq!(
            b.by_agent.len(),
            1,
            "only the LATEST sighting is kept — one entry per agent, not per hook"
        );
    }

    /// Corroboration is CONSECUTIVE, not "seen twice ever": an interleaved pid
    /// replaces the candidate. So recycling a pid somewhere in a session cannot
    /// arm a wrapper — the OS would have to hand out the same pid on two
    /// back-to-back spawns for one agent.
    #[test]
    fn an_interleaved_pid_resets_the_corroboration() {
        let mut b = Bindings::default();
        let a = AgentId::from_parts("cursor", "sess-A");
        assert!(!b.sight(100, a, GUESS));
        assert!(
            !b.sight(200, a, GUESS),
            "a different pid replaces the candidate"
        );
        assert!(
            !b.sight(100, a, GUESS),
            "100 was seen before, but not CONSECUTIVELY — it must not arm"
        );
        assert!(
            b.take(100).is_empty(),
            "a recycled pid must not arm a wrapper"
        );
        assert!(
            b.sight(100, a, GUESS),
            "back-to-back 100 is real corroboration"
        );
    }

    /// An ARMED pid must not shield a stale candidate into a later arm: with the
    /// two states in separate maps, sighting an armed pid skipped the candidate
    /// bookkeeping, so 9,7,9 armed 9 non-consecutively AND left the agent armed
    /// on two pids at once. One `Sighting` per agent makes both unrepresentable.
    #[test]
    fn sighting_an_armed_pid_cannot_resurrect_a_stale_candidate() {
        let mut b = Bindings::default();
        let a = AgentId::from_parts("codewhale", "/ws");
        assert!(!b.sight(7, a, GUESS));
        assert!(b.sight(7, a, GUESS), "7 is armed");
        assert!(!b.sight(9, a, GUESS), "9 becomes the candidate");
        assert!(
            !b.sight(7, a, GUESS),
            "7 is armed but not consecutive with 9"
        );
        assert!(
            !b.sight(9, a, GUESS),
            "9 must NOT arm — its candidacy was broken by the 7 sighting"
        );
        assert!(
            b.take(7).is_empty() || b.take(9).is_empty(),
            "an agent is armed on at most ONE pid"
        );
    }

    /// One entry per agent however many sightings arrive.
    #[test]
    fn candidates_are_bounded_by_live_agents() {
        let mut b = Bindings::default();
        for hook in 0..50 {
            for n in 0..3 {
                b.sight(
                    10_000 + hook,
                    AgentId::from_parts("cursor", &format!("s{n}")),
                    GUESS,
                );
            }
        }
        assert_eq!(b.by_agent.len(), 3, "one entry per agent, 150 sightings");
    }

    #[tokio::test]
    async fn killing_a_watched_pid_emits_session_end_for_its_agent() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let Some(watch) = HookPidWatch::spawn(tx) else {
            return; // no exit-watch backend on this platform — nothing to assert
        };
        let mut child = crate::test_child::sleeper();
        let pid = i32::try_from(child.id()).expect("pid fits i32");
        let agent = AgentId::from_parts("codewhale", "/ws");
        watch.note(pid, agent, true);
        watch.note(pid, agent, true); // the CLI's pid rides every hook event
        child.kill().expect("kill the watched child");
        let _ = child.wait();

        let (transport, ev) = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("a SessionEnd within 5s of the watched pid dying")
            .expect("channel still open");
        assert_eq!(transport, Transport::Hook);
        assert!(
            matches!(ev, AgentEvent::SessionEnd { agent_id, as_child: false } if agent_id == agent),
            "the bound agent must be ended when its pid dies, got {ev:?}"
        );
    }

    /// The #896 guard end to end: one sighting, then that pid dies.
    #[tokio::test]
    async fn a_once_seen_pid_dying_ends_nothing() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let Some(watch) = HookPidWatch::spawn(tx) else {
            return; // no exit-watch backend on this platform — nothing to assert
        };
        let mut wrapper = crate::test_child::sleeper();
        let pid = i32::try_from(wrapper.id()).expect("pid fits i32");
        watch.note(pid, AgentId::from_parts("cursor", "sess-A"), true);
        wrapper.kill().expect("kill the wrapper");
        let _ = wrapper.wait();

        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .is_err(),
            "an uncorroborated pid's death must not end the session"
        );
    }
}
