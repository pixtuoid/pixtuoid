//! The shared, daemon-AGNOSTIC presence layer. A "daemon" source (the OpenClaw
//! gateway is instance #1) produces NO agent activity — it has no desk, no
//! `AgentSlot`. Instead each running daemon INSTANCE earns one presence-gated
//! wandering mascot whose motion encodes that instance's liveness. This module
//! owns the state machine + lifecycle identical for EVERY daemon; the per-daemon
//! WIRE decode stays in the daemon's own module, exactly like an agent source
//! owns its own line/hook decoder.
//!
//! Presence rides a SIBLING channel (invariant #2: NOT the one `AgentEvent`
//! channel), carrying `PresenceMsg { key: DaemonInstanceKey, delta }` so N daemons
//! AND N instances of one daemon land in DISTINCT `SceneState::daemons` entries.
//! The reducer task merges them via
//! [`apply_presence`](crate::source::daemon::apply_presence), NEVER through
//! `Reducer::apply` (which is `AgentId`-pure).
//!
//! **The two identity concepts are deliberately separate.** The
//! [`DaemonInstanceId`](crate::state::DaemonInstanceId) is STABLE — a gateway
//! restarting on the same port keeps its mascot — while
//! `DaemonPresence::current_pid` is the PROCESS incarnation, rebound by each
//! `GatewayUp`, so a late exit receipt for a replaced process is a no-op instead
//! of a kill of its replacement.

use std::time::SystemTime;

use crate::state::{DaemonInstanceId, DaemonLiveness, DaemonPresence, SceneState};

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::{PresenceExitWatch, PresenceSender, spawn_presence_exit_watch};

/// One presence delta for a daemon mascot — the SHARED vocabulary every daemon
/// emits, all consumed by [`apply_presence`]. Identity-agnostic ON PURPOSE: a
/// 2nd daemon — or a 2nd instance of one — needs ZERO new variants, because the
/// routing [`DaemonInstanceKey`] rides the channel message, not the enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonPresenceUpdate {
    /// `gateway_start` — the daemon is up. UP-winning + idempotent; resets the
    /// session count and rebinds the armed pid.
    GatewayUp {
        /// The gateway's `process.pid`, armed for the abrupt-down exit watch (`None` if unknown).
        pid: Option<i32>,
    },
    /// `gateway_stop` — clean shutdown.
    GatewayDown,
    /// `session_start` — a multiplexed session began (bumps the bubble count).
    SessionStarted,
    /// `session_end` — a session ended.
    SessionEnded,
    /// `before_agent_run` — a turn entered flight, keyed for self-healing busy.
    RunStarted {
        /// Correlates this turn with its later `RunEnded`/`RunFailed`.
        run_key: String,
    },
    /// A turn ended without a backend failure — completed, or cancelled by the
    /// user.
    RunEnded {
        /// The completed turn's correlation key (matches its `RunStarted`).
        run_key: String,
    },
    /// A turn failed on the daemon's model backend (auth revoked, provider down).
    /// Drives `Degraded`.
    RunFailed {
        /// The failed turn's correlation key (matches its `RunStarted`).
        run_key: String,
    },
    /// A live gateway pid OBSERVED on any event carrying `_pid` — adopted into
    /// `current_pid` ONLY when it was `None`, so a MID-ATTACH or a
    /// reconnect-while-alive can still arm the abrupt-down exit watch without
    /// having seen `gateway_start`. A pure pid adoption: no `DaemonState` change,
    /// and `GatewayUp` still owns restart-rebinds.
    PidSeen {
        /// The live gateway pid observed on the event.
        pid: i32,
    },
    /// The armed gateway pid died (from the `ExitWatch` drain, not a decoder).
    PidExited {
        /// The gateway pid that exited.
        pid: i32,
    },
}

/// WHICH daemon mascot a presence delta belongs to: the registry source name
/// plus the source-owned [`DaemonInstanceId`]. A named struct, not a tuple, so
/// it can't be read positionally at any of the four seams it crosses (hook demux
/// → side channel → state machine → exit watch).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DaemonInstanceKey {
    source: String,
    instance: DaemonInstanceId,
}

impl DaemonInstanceKey {
    /// Bind a source name to one of its instance ids.
    pub fn new(source: impl Into<String>, instance: DaemonInstanceId) -> Self {
        Self {
            source: source.into(),
            instance,
        }
    }

    /// The registry source name (the mascot definition + the connection gate).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The instance within that source.
    pub fn instance(&self) -> &DaemonInstanceId {
        &self.instance
    }
}

/// A presence delta tagged with the exact mascot it belongs to. A named struct
/// (not a `(Key, Update)` tuple) so the routing key can't be read positionally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceMsg {
    /// The exact daemon instance this delta routes to.
    pub key: DaemonInstanceKey,
    /// The presence delta to apply to that instance.
    pub delta: DaemonPresenceUpdate,
}

/// One decoded daemon envelope: WHICH instance sent it plus the presence deltas
/// it implies. The return type of every `presence_decoder`; the instance id is
/// mandatory at the wire boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPresence {
    /// The sending instance, as the source's own decoder resolved it.
    pub instance: DaemonInstanceId,
    /// The presence deltas the envelope implies (empty for a benign skip).
    pub updates: Vec<DaemonPresenceUpdate>,
}

/// Per-daemon decay/stale knobs. A daemon has no per-session pid, so silence is
/// the only abrupt-exit signal — these bound how long busy/up linger without
/// fresh deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresenceTtl {
    /// Grace before busy → idle when no `before_agent_run`/`agent_end` arrives
    /// (a dropped `agent_end` must self-heal, never strand perpetual busy).
    pub(crate) busy_decay_ms: u64,
    /// With no activity for this long the daemon is presumed DOWN (covers a
    /// killed gateway, which fires neither `session_end` nor `gateway_stop`).
    pub(crate) presence_ttl_ms: u64,
    /// How long a `Down` presence lingers (drawn walking out) before it is
    /// REMOVED — generously past the renderer's elevator walk-out, so the leave
    /// animation always completes first.
    pub(crate) down_remove_ms: u64,
}

impl PresenceTtl {
    /// The default decay profile (OpenClaw's).
    pub(crate) const DEFAULT: PresenceTtl = PresenceTtl {
        busy_decay_ms: 30_000,
        presence_ttl_ms: 5 * 60 * 1_000,
        down_remove_ms: 5_000,
    };
}

impl DaemonPresenceUpdate {
    /// The gateway pid this update arms the abrupt-down `ExitWatch` on, if any.
    /// The variant→pid mapping lives HERE (one place) so the driver's watch-arm
    /// and `apply_presence`'s `current_pid` adoption can't drift.
    pub fn armable_pid(&self) -> Option<i32> {
        match self {
            DaemonPresenceUpdate::GatewayUp { pid } => *pid,
            DaemonPresenceUpdate::PidSeen { pid } => Some(*pid),
            _ => None,
        }
    }
}

impl DaemonPresence {
    /// Zero the "live work" pair — one concept, always reset together on every
    /// restart-or-down path. (The busy-decay arm deliberately keeps the session
    /// count, so it does NOT use this.)
    fn clear_concurrency(&mut self) {
        self.active_sessions = 0;
        self.in_flight_runs.clear();
    }

    /// Transition to `Down` + clear the live-work pair AND the armed pid, so a
    /// must-clear-on-down field can't be forgotten at one of the four down sites.
    /// `current_pid` is cleared because a Down daemon has no live gateway pid:
    /// leaving it set strands the binding on the dead pid, so a reconnect as a new
    /// pid whose `gateway_start` is missed can't re-adopt via `PidSeen` (None-only)
    /// and the instant abrupt-down rung silently disarms until the presence sweep.
    ///
    /// `last_seen` is anchored to `now` HERE because the sweep removes a Down
    /// entry on that clock: on an abrupt death, where `last_seen` can be
    /// minutes stale, the entry would be swept before any frame showed the
    /// gateway Down. Taking `now` makes entering Down without anchoring the
    /// clock unrepresentable.
    fn enter_down(&mut self, now: SystemTime) {
        self.liveness = DaemonLiveness::Down;
        self.clear_concurrency();
        self.current_pid = None;
        self.last_seen = now;
    }
}

/// Merge one presence delta into `key`'s `(source, instance)` entry of
/// `scene.daemons` — never a source-wide one. Called by the reducer task off the
/// SIBLING channel — NEVER through `Reducer::apply` (which is `AgentId`-pure). A
/// proof-of-life update refreshes `last_seen` and "any event implies UP"
/// resurrects a wrongly-DOWN daemon; `PidExited` is the exception — a DEATH
/// signal that never materializes an absent daemon.
pub fn apply_presence(
    scene: &mut SceneState,
    key: &DaemonInstanceKey,
    update: DaemonPresenceUpdate,
    now: SystemTime,
) {
    use DaemonPresenceUpdate::*;
    let (source, instance) = (key.source(), key.instance());
    // A `PidExited` must NEVER materialize a daemon: a fresh entry has
    // `current_pid == None`, so the arm's `current_pid == Some(pid)` guard fails
    // and the entry is left UP — a phantom live mascot for a dead gateway (the
    // exit watch races the removal sweep). Every OTHER delta (re)creates UP.
    let p = if matches!(update, PidExited { .. }) {
        let Some(p) = scene.daemons.get_mut(source, instance) else {
            return;
        };
        p
    } else {
        scene
            .daemons
            .get_or_insert_with(source, instance, || DaemonPresence {
                liveness: DaemonLiveness::UP,
                active_sessions: 0,
                last_seen: now,
                entered_at: now,
                in_flight_runs: Default::default(),
                current_pid: None,
            })
    };
    let was_down = p.liveness == DaemonLiveness::Down;
    // Proof-of-life ONLY. A `PidExited` on the ordinary clean stop lands after
    // `GatewayDown` already cleared `current_pid`, so its arm is a no-op — yet
    // stamping the clock here would restart the walk-out the renderer times off
    // `last_seen` (and push out the sweep's removal), making the mascot vanish and
    // then leave a second time. This skips BOTH `PidExited` sub-cases, which is why
    // the MATCHING one (the abrupt death) anchors the clock in `enter_down`.
    if !matches!(update, PidExited { .. }) {
        p.last_seen = now;
    }
    match update {
        GatewayUp { pid } => {
            p.current_pid = pid;
            p.clear_concurrency();
            p.liveness = DaemonLiveness::UP;
        }
        GatewayDown => {
            p.enter_down(now);
        }
        SessionStarted => {
            p.active_sessions = p.active_sessions.saturating_add(1);
            if p.liveness == DaemonLiveness::Down {
                p.liveness = DaemonLiveness::UP; // any event ⇒ up
            }
        }
        SessionEnded => {
            // Saturating: a pre-attach session_start we never saw must not underflow.
            p.active_sessions = p.active_sessions.saturating_sub(1);
            // Deliberately does NOT resurrect, unlike its sibling arms.
            // Pinned by `session_end_after_a_gateway_stop_leaves_it_down`.
        }
        RunStarted { run_key } => {
            // Stamped with THIS observation, so the run ages on its own clock.
            p.in_flight_runs.insert(run_key, now);
            // Busy itself is DERIVED from the now-non-empty run set by
            // `display_state()`, never stored.
            p.liveness = DaemonLiveness::UP;
        }
        RunEnded { run_key } => {
            p.in_flight_runs.remove(&run_key);
            if p.in_flight_runs.is_empty() {
                // A clean run heals a prior Degraded and resurrects a Down daemon.
                p.liveness = DaemonLiveness::UP;
            }
        }
        // The gateway is alive but its model backend broke: Degraded persists until
        // the next RunEnded / RunStarted / GatewayUp.
        RunFailed { run_key } => {
            p.in_flight_runs.remove(&run_key);
            // Degraded regardless of any OTHER run still in flight — the projection
            // renders Degraded over Busy (degraded checked first).
            p.liveness = DaemonLiveness::Up { degraded: true };
        }
        PidSeen { pid } => {
            if p.current_pid.is_none() {
                p.current_pid = Some(pid);
            }
        }
        // Only the CURRENTLY-armed pid dying takes the daemon down: a stale receipt
        // for an old pid after a restart is a no-op, so the live daemon stays up.
        PidExited { pid } => {
            if p.current_pid == Some(pid) {
                p.enter_down(now);
            }
        }
    }
    // Re-anchor the walk-in on a Down → up resurrection (the sim walks the
    // mascot in from `entered_at`); Idle↔Busy leaves it, so a mascot already in
    // the room does not walk in again.
    if was_down && p.liveness != DaemonLiveness::Down {
        p.entered_at = now;
    }
}

/// Decay one daemon source's stale presence on the reducer's sweep tick,
/// INSTANCE-LOCALLY: each instance decays on its OWN `last_seen`, so traffic from
/// gateway A can never renew gateway B. Per instance: each in-flight RUN expires
/// `ttl.busy_decay_ms` after ITS OWN last observation (so a dropped `agent_end`
/// self-heals even while the gateway keeps serving other runs — never a latch),
/// any live state → DOWN after `ttl.presence_ttl_ms` of total silence (a kill),
/// and a `Down` entry is REMOVED after `ttl.down_remove_ms`. Expiring a run lease
/// never clears `degraded` — a separate axis only a real
/// `RunEnded`/`RunStarted`/`GatewayUp` heals.
pub fn sweep_presence_ttl(scene: &mut SceneState, source: &str, ttl: PresenceTtl, now: SystemTime) {
    let mut doomed: Vec<DaemonInstanceId> = Vec::new();
    for (instance, p) in scene.daemons.instances_of_mut(source) {
        let idle_ms = now
            .duration_since(p.last_seen)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if p.liveness == DaemonLiveness::Down {
            // Keep the Down entry only until the walk-out has had time to finish.
            if idle_ms >= ttl.down_remove_ms {
                doomed.push(instance.clone());
            }
        } else if idle_ms >= ttl.presence_ttl_ms {
            p.enter_down(now);
        } else {
            p.in_flight_runs.retain(|_, started| {
                now.duration_since(*started)
                    .map(|d| (d.as_millis() as u64) < ttl.busy_decay_ms)
                    // A clock regression keeps the lease: a backwards step must not
                    // expire every run at once.
                    .unwrap_or(true)
            });
        }
    }
    scene.daemons.remove_instances(source, &doomed);
}

/// Drive EVERY instance of a source to `Down` (arming the renderer's walk-out),
/// skipping any already Down — idempotent, so the `down_remove_ms` removal timer
/// in [`sweep_presence_ttl`] isn't reset on every tick. The runtime calls this
/// when a source is DISCONNECTED in the Sources panel: the presence side-channel
/// is separate from the `AgentEvent` connection gate, so a disconnect must
/// reconcile presence too. Source-WIDE by design.
pub fn mark_presence_down(scene: &mut SceneState, source: &str, now: SystemTime) {
    for (_, p) in scene.daemons.instances_of_mut(source) {
        if p.liveness != DaemonLiveness::Down {
            p.enter_down(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DaemonState;

    // Every assertion runs against TWO synthetic sources, to prove the state
    // machine is daemon-AGNOSTIC: a 2nd daemon needs zero new code here.
    const SOURCES: [&str; 2] = ["openclaw", "daemon2"];

    /// Every timing test derives its offsets FROM the profile, so mutating a
    /// literal also mutates each test's expectation — this direct pin is the only
    /// guard on the literals themselves.
    #[test]
    fn default_presence_profile_has_its_intended_durations() {
        assert_eq!(PresenceTtl::DEFAULT.busy_decay_ms, 30_000); // 30 s
        assert_eq!(PresenceTtl::DEFAULT.presence_ttl_ms, 300_000); // 5 min
        assert_eq!(PresenceTtl::DEFAULT.down_remove_ms, 5_000); // 5 s
    }

    fn ms(m: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(m)
    }

    /// The canonical SINGLE instance the state-machine tests run against; the
    /// multi-INSTANCE guarantees have their own suite (`inst`/`apply_at`).
    fn ikey(src: &str) -> DaemonInstanceKey {
        DaemonInstanceKey::new(src, inst("1"))
    }
    fn inst(id: &str) -> DaemonInstanceId {
        DaemonInstanceId::new(id).expect("non-empty test instance id")
    }
    fn apply(s: &mut SceneState, src: &str, u: DaemonPresenceUpdate, at: SystemTime) {
        apply_presence(s, &ikey(src), u, at);
    }
    fn apply_at(s: &mut SceneState, src: &str, id: &str, u: DaemonPresenceUpdate, at: u64) {
        apply_presence(s, &DaemonInstanceKey::new(src, inst(id)), u, ms(at));
    }
    fn p_opt<'a>(s: &'a SceneState, src: &str) -> Option<&'a DaemonPresence> {
        s.daemon(src, ikey(src).instance())
    }
    fn p<'a>(s: &'a SceneState, src: &str) -> &'a DaemonPresence {
        p_opt(s, src).expect("presence entry")
    }
    fn st_at(s: &SceneState, src: &str, id: &str) -> Option<DaemonState> {
        s.daemon(src, &inst(id)).map(|p| p.display_state())
    }
    fn st(s: &SceneState, src: &str) -> DaemonState {
        p(s, src).display_state()
    }
    fn sessions(s: &SceneState, src: &str) -> u32 {
        p(s, src).active_sessions
    }
    fn entered_at(s: &SceneState, src: &str) -> SystemTime {
        p(s, src).entered_at
    }
    fn last_seen(s: &SceneState, src: &str) -> SystemTime {
        p(s, src).last_seen
    }
    fn up(s: &mut SceneState, src: &str, pid: i32, at: u64) {
        apply(
            s,
            src,
            DaemonPresenceUpdate::GatewayUp { pid: Some(pid) },
            ms(at),
        );
    }

    #[test]
    fn gateway_up_sets_idle_and_records_pid() {
        for src in SOURCES {
            let mut s = SceneState::default();
            up(&mut s, src, 4242, 0);
            assert_eq!(st(&s, src), DaemonState::Idle);
            assert_eq!(p(&s, src).current_pid, Some(4242));
        }
    }

    #[test]
    fn gateway_up_resets_sessions_and_in_flight_runs() {
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(&mut s, src, DaemonPresenceUpdate::SessionStarted, ms(0));
            apply(&mut s, src, DaemonPresenceUpdate::SessionStarted, ms(1));
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "r".into(),
                },
                ms(2),
            );
            assert_eq!(st(&s, src), DaemonState::Busy);
            up(&mut s, src, 1, 3);
            assert_eq!(st(&s, src), DaemonState::Idle);
            assert_eq!(sessions(&s, src), 0);
            assert!(p(&s, src).in_flight_runs.is_empty());
        }
    }

    #[test]
    fn gateway_down_sets_down() {
        for src in SOURCES {
            let mut s = SceneState::default();
            up(&mut s, src, 1, 0);
            apply(&mut s, src, DaemonPresenceUpdate::GatewayDown, ms(1));
            assert_eq!(st(&s, src), DaemonState::Down);
        }
    }

    #[test]
    fn pid_exited_never_materializes_a_daemon() {
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::PidExited { pid: 100 },
                ms(1),
            );
            assert!(
                p_opt(&s, src).is_none(),
                "PidExited on an absent daemon must not create an entry"
            );
        }
    }

    #[test]
    fn session_count_increments_and_saturates_at_zero() {
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(&mut s, src, DaemonPresenceUpdate::SessionStarted, ms(0));
            apply(&mut s, src, DaemonPresenceUpdate::SessionStarted, ms(1));
            assert_eq!(sessions(&s, src), 2);
            for i in 0..3 {
                apply(&mut s, src, DaemonPresenceUpdate::SessionEnded, ms(2 + i));
            }
            assert_eq!(
                sessions(&s, src),
                0,
                "saturating — a pre-attach miss never underflows"
            );
        }
    }

    #[test]
    fn busy_holds_until_the_last_run_ends() {
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "a".into(),
                },
                ms(0),
            );
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "b".into(),
                },
                ms(1),
            );
            assert_eq!(st(&s, src), DaemonState::Busy);
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunEnded {
                    run_key: "a".into(),
                },
                ms(2),
            );
            assert_eq!(st(&s, src), DaemonState::Busy, "b still in flight");
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunEnded {
                    run_key: "b".into(),
                },
                ms(3),
            );
            assert_eq!(st(&s, src), DaemonState::Idle);
        }
    }

    #[test]
    fn failed_run_degrades_the_daemon() {
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "r".into(),
                },
                ms(0),
            );
            assert_eq!(st(&s, src), DaemonState::Busy);
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunFailed {
                    run_key: "r".into(),
                },
                ms(1),
            );
            assert_eq!(
                st(&s, src),
                DaemonState::Degraded,
                "a failed run ⇒ degraded"
            );
            assert!(
                p(&s, src).in_flight_runs.is_empty(),
                "the failed run leaves the in-flight set"
            );
        }
    }

    #[test]
    fn a_new_run_clears_degraded_back_to_busy() {
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunFailed {
                    run_key: "a".into(),
                },
                ms(0),
            );
            assert_eq!(st(&s, src), DaemonState::Degraded);
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "b".into(),
                },
                ms(1),
            );
            assert_eq!(
                st(&s, src),
                DaemonState::Busy,
                "a fresh attempt re-enters Busy (the gateway is trying again)"
            );
        }
    }

    #[test]
    fn a_successful_run_heals_degraded_to_idle() {
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunFailed {
                    run_key: "a".into(),
                },
                ms(0),
            );
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "b".into(),
                },
                ms(1),
            );
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunEnded {
                    run_key: "b".into(),
                },
                ms(2),
            );
            assert_eq!(
                st(&s, src),
                DaemonState::Idle,
                "a clean run drains the in-flight set ⇒ heals to idle"
            );
        }
    }

    #[test]
    fn gateway_restart_clears_degraded() {
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunFailed {
                    run_key: "a".into(),
                },
                ms(0),
            );
            assert_eq!(st(&s, src), DaemonState::Degraded);
            up(&mut s, src, 9, 1);
            assert_eq!(
                st(&s, src),
                DaemonState::Idle,
                "a restart (re-auth, provider back) clears the degraded latch"
            );
        }
    }

    #[test]
    fn pid_seen_adopts_when_current_pid_is_none() {
        for src in SOURCES {
            // Mid-attach: pixtuoid never saw `gateway_start`, so the entry is first
            // created by a plain activity event carrying `_pid`.
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::PidSeen { pid: 555 },
                ms(0),
            );
            assert_eq!(
                p(&s, src).current_pid,
                Some(555),
                "the live pid is adopted so the instant abrupt-down rung can arm"
            );
            assert_eq!(st(&s, src), DaemonState::Idle);
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::PidExited { pid: 555 },
                ms(1),
            );
            assert_eq!(st(&s, src), DaemonState::Down);
        }
    }

    #[test]
    fn pid_seen_never_clobbers_a_known_pid() {
        for src in SOURCES {
            let mut s = SceneState::default();
            up(&mut s, src, 100, 0);
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::PidSeen { pid: 999 },
                ms(1),
            );
            assert_eq!(
                p(&s, src).current_pid,
                Some(100),
                "PidSeen is adopt-only-when-None; GatewayUp owns rebinds"
            );
        }
    }

    #[test]
    fn pid_seen_is_pure_adoption_and_does_not_change_state() {
        // The decoder ALWAYS prepends PidSeen to a state-bearing update, so
        // resurrection rides on that sibling, never on PidSeen alone.
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(&mut s, src, DaemonPresenceUpdate::GatewayDown, ms(0));
            assert_eq!(st(&s, src), DaemonState::Down);
            apply(&mut s, src, DaemonPresenceUpdate::PidSeen { pid: 7 }, ms(1));
            assert_eq!(
                st(&s, src),
                DaemonState::Down,
                "PidSeen is pure pid adoption — it does NOT resurrect by itself"
            );
            assert_eq!(p(&s, src).current_pid, Some(7));
        }
    }

    #[test]
    fn armable_pid_is_only_gateway_up_some_and_pid_seen() {
        use DaemonPresenceUpdate::*;
        assert_eq!(GatewayUp { pid: Some(7) }.armable_pid(), Some(7));
        assert_eq!(GatewayUp { pid: None }.armable_pid(), None);
        assert_eq!(PidSeen { pid: 9 }.armable_pid(), Some(9));
        assert_eq!(GatewayDown.armable_pid(), None);
        assert_eq!(SessionStarted.armable_pid(), None);
        assert_eq!(
            RunStarted {
                run_key: "r".into()
            }
            .armable_pid(),
            None
        );
        assert_eq!(PidExited { pid: 3 }.armable_pid(), None);
    }

    #[test]
    fn pid_seen_re_adopts_after_an_abrupt_down_so_the_second_cycle_arms() {
        use DaemonPresenceUpdate::*;
        for src in SOURCES {
            let mut s = SceneState::default();
            up(&mut s, src, 100, 0); // P1 live
            apply(&mut s, src, PidExited { pid: 100 }, ms(1)); // P1 dies → Down
            assert_eq!(st(&s, src), DaemonState::Down);
            // Reconnect as P2; gateway_start missed → only a normal event + PidSeen.
            apply(&mut s, src, PidSeen { pid: 200 }, ms(2));
            apply(&mut s, src, SessionStarted, ms(3)); // any event ⇒ up
            assert_eq!(
                p(&s, src).current_pid,
                Some(200),
                "PidSeen must re-adopt the live pid after a Down"
            );
            apply(&mut s, src, PidExited { pid: 200 }, ms(4));
            assert_eq!(
                st(&s, src),
                DaemonState::Down,
                "the second-cycle PidExited re-armed the instant abrupt-down rung"
            );
        }
    }

    #[test]
    fn pid_exit_matching_current_takes_the_daemon_down() {
        for src in SOURCES {
            let mut s = SceneState::default();
            up(&mut s, src, 7, 0);
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::PidExited { pid: 7 },
                ms(1),
            );
            assert_eq!(st(&s, src), DaemonState::Down);
        }
    }

    #[test]
    fn stale_pid_exit_after_restart_leaves_the_daemon_up() {
        for src in SOURCES {
            let mut s = SceneState::default();
            up(&mut s, src, 1, 0);
            up(&mut s, src, 2, 1);
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::PidExited { pid: 1 },
                ms(2),
            );
            assert_eq!(
                st(&s, src),
                DaemonState::Idle,
                "P2 stays up; stale P1 exit ignored"
            );
            assert_eq!(p(&s, src).current_pid, Some(2));
        }
    }

    #[test]
    fn session_end_after_a_gateway_stop_leaves_it_down() {
        // The recorded clean shutdown's own order, 2 ms apart.
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(&mut s, src, DaemonPresenceUpdate::GatewayDown, ms(0));
            assert_eq!(st(&s, src), DaemonState::Down);
            apply(&mut s, src, DaemonPresenceUpdate::SessionEnded, ms(1));
            assert_eq!(
                st(&s, src),
                DaemonState::Down,
                "a stopped gateway must not walk back in on its trailing session_end"
            );
            assert_eq!(
                sessions(&s, src),
                0,
                "saturating — the pre-attach session_start miss must not underflow"
            );
        }
    }

    #[test]
    fn a_real_event_still_resurrects_a_down_gateway() {
        // The resurrect that IS wanted: a TTL-downed gateway that turns out to
        // be alive says so with a session START, not a session end.
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(&mut s, src, DaemonPresenceUpdate::GatewayDown, ms(0));
            apply(&mut s, src, DaemonPresenceUpdate::SessionStarted, ms(1));
            assert_eq!(st(&s, src), DaemonState::Idle);

            let mut s = SceneState::default();
            apply(&mut s, src, DaemonPresenceUpdate::GatewayDown, ms(0));
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "r1".into(),
                },
                ms(1),
            );
            assert_eq!(st(&s, src), DaemonState::Busy);
        }
    }

    #[test]
    fn entered_at_reanchors_on_resurrection_but_not_on_idle_busy() {
        for src in SOURCES {
            let mut s = SceneState::default();
            up(&mut s, src, 1, 0);
            assert_eq!(entered_at(&s, src), ms(0));
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "r".into(),
                },
                ms(2000),
            );
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunEnded {
                    run_key: "r".into(),
                },
                ms(3000),
            );
            assert_eq!(
                entered_at(&s, src),
                ms(0),
                "idle↔busy must not move entered_at"
            );
            apply(&mut s, src, DaemonPresenceUpdate::GatewayDown, ms(4000));
            apply(&mut s, src, DaemonPresenceUpdate::SessionStarted, ms(9000));
            assert_eq!(st(&s, src), DaemonState::Idle);
            assert_eq!(
                entered_at(&s, src),
                ms(9000),
                "resurrection re-anchors the walk-in"
            );
        }
    }

    #[test]
    fn mark_presence_down_arms_the_walkout_idempotently() {
        for src in SOURCES {
            let mut s = SceneState::default();
            up(&mut s, src, 1, 0);
            mark_presence_down(&mut s, src, ms(1000));
            assert_eq!(st(&s, src), DaemonState::Down);
            assert_eq!(
                last_seen(&s, src),
                ms(1000),
                "Down re-anchors last_seen for the walk-out"
            );
            mark_presence_down(&mut s, src, ms(5000));
            assert_eq!(
                last_seen(&s, src),
                ms(1000),
                "idempotent: already-Down is untouched"
            );
        }
        let mut s = SceneState::default();
        up(&mut s, "openclaw", 1, 0);
        mark_presence_down(&mut s, "not-a-source", ms(6000));
        assert_eq!(s.daemons().count(), 1);
    }

    #[test]
    fn sweep_takes_the_daemon_down_after_presence_ttl() {
        let ttl = PresenceTtl::DEFAULT;
        for src in SOURCES {
            let mut s = SceneState::default();
            up(&mut s, src, 1, 0);
            sweep_presence_ttl(&mut s, src, ttl, ms(ttl.presence_ttl_ms + 1));
            assert_eq!(
                st(&s, src),
                DaemonState::Down,
                "silence past the TTL ⇒ down (covers a kill)"
            );
            assert_eq!(sessions(&s, src), 0);
            assert_eq!(
                last_seen(&s, src),
                ms(ttl.presence_ttl_ms + 1),
                "walk-out anchor re-stamped"
            );
        }
    }

    #[test]
    fn sweep_removes_a_down_entry_after_the_walkout_window() {
        let ttl = PresenceTtl::DEFAULT;
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(&mut s, src, DaemonPresenceUpdate::GatewayDown, ms(0));
            sweep_presence_ttl(&mut s, src, ttl, ms(ttl.down_remove_ms - 1));
            assert!(p_opt(&s, src).is_some(), "still present mid walk-out");
            sweep_presence_ttl(&mut s, src, ttl, ms(ttl.down_remove_ms + 1));
            assert!(
                p_opt(&s, src).is_none(),
                "removed once the walk-out window elapsed"
            );
        }
    }

    #[test]
    fn sweep_self_heals_a_stranded_busy_after_the_grace_window() {
        let ttl = PresenceTtl::DEFAULT;
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "stranded".into(),
                },
                ms(0),
            );
            assert_eq!(st(&s, src), DaemonState::Busy);
            sweep_presence_ttl(&mut s, src, ttl, ms(ttl.busy_decay_ms + 1));
            assert_eq!(
                st(&s, src),
                DaemonState::Idle,
                "stranded busy self-heals to idle"
            );
            assert!(p(&s, src).in_flight_runs.is_empty());
        }
    }

    #[test]
    fn a_clock_regression_keeps_an_in_flight_lease() {
        let ttl = PresenceTtl::DEFAULT;
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "r".into(),
                },
                ms(10_000),
            );
            assert_eq!(st(&s, src), DaemonState::Busy);
            // Sweep with `now` BEFORE the run's own stamp.
            sweep_presence_ttl(&mut s, src, ttl, ms(0));
            assert_eq!(
                st(&s, src),
                DaemonState::Busy,
                "a backwards clock must not expire a live run"
            );
        }
    }

    #[test]
    fn sweep_does_not_busy_decay_a_degraded_daemon_but_ttl_takes_it_down() {
        let ttl = PresenceTtl::DEFAULT;
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunFailed {
                    run_key: "r".into(),
                },
                ms(0),
            );
            assert_eq!(st(&s, src), DaemonState::Degraded);
            sweep_presence_ttl(&mut s, src, ttl, ms(ttl.busy_decay_ms + 1));
            assert_eq!(
                st(&s, src),
                DaemonState::Degraded,
                "Degraded must NOT busy-decay to Idle (only Busy does)"
            );
            sweep_presence_ttl(&mut s, src, ttl, ms(ttl.presence_ttl_ms + 1));
            assert_eq!(
                st(&s, src),
                DaemonState::Down,
                "silence past the TTL takes even a Degraded daemon down"
            );
        }
    }

    #[test]
    fn sweep_on_an_unknown_source_is_a_noop() {
        let ttl = PresenceTtl::DEFAULT;
        let mut s = SceneState::default();
        assert_eq!(s.daemons().count(), 0);
        sweep_presence_ttl(&mut s, "never-seen", ttl, ms(ttl.presence_ttl_ms + 1));
        assert_eq!(
            s.daemons().count(),
            0,
            "sweeping an unknown source mints no phantom entry"
        );
        assert!(p_opt(&s, "never-seen").is_none());
    }

    #[test]
    fn sweep_within_the_grace_window_keeps_busy() {
        let ttl = PresenceTtl::DEFAULT;
        for src in SOURCES {
            let mut s = SceneState::default();
            apply(
                &mut s,
                src,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "r".into(),
                },
                ms(0),
            );
            sweep_presence_ttl(&mut s, src, ttl, ms(ttl.busy_decay_ms - 1));
            assert_eq!(
                st(&s, src),
                DaemonState::Busy,
                "still within the decay grace"
            );
        }
    }

    #[test]
    fn two_daemons_coexist_with_independent_presence() {
        let mut s = SceneState::default();
        up(&mut s, "openclaw", 1, 0);
        apply(
            &mut s,
            "daemon2",
            DaemonPresenceUpdate::RunStarted {
                run_key: "r".into(),
            },
            ms(1),
        );
        assert_eq!(st(&s, "openclaw"), DaemonState::Idle);
        assert_eq!(st(&s, "daemon2"), DaemonState::Busy);
        apply(&mut s, "openclaw", DaemonPresenceUpdate::GatewayDown, ms(2));
        assert_eq!(st(&s, "openclaw"), DaemonState::Down);
        assert_eq!(
            st(&s, "daemon2"),
            DaemonState::Busy,
            "daemon2 unaffected by openclaw down"
        );
        assert_eq!(s.daemons().count(), 2);
    }

    // OpenClaw officially supports several isolated gateways on one host, which is
    // why `daemons` keys on (source, instance): every assertion in the suite below
    // fails against a source-only key.
    const A: &str = "18789";
    const B: &str = "19789";

    #[test]
    fn two_gateway_instances_of_one_source_hold_independent_state() {
        let src = "openclaw";
        let mut s = SceneState::default();
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::GatewayUp { pid: None },
            0,
        );
        apply_at(
            &mut s,
            src,
            B,
            DaemonPresenceUpdate::GatewayUp { pid: None },
            0,
        );
        assert_eq!(s.daemons().count(), 2, "two ports ⇒ two mascots");
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::RunStarted {
                run_key: "r1".into(),
            },
            1,
        );
        assert_eq!(st_at(&s, src, A), Some(DaemonState::Busy));
        assert_eq!(
            st_at(&s, src, B),
            Some(DaemonState::Idle),
            "gateway A's run must not make gateway B busy"
        );
        apply_at(&mut s, src, A, DaemonPresenceUpdate::GatewayDown, 2);
        assert_eq!(st_at(&s, src, A), Some(DaemonState::Down));
        assert_eq!(
            st_at(&s, src, B),
            Some(DaemonState::Idle),
            "gateway A's stop must not take gateway B down"
        );
    }

    #[test]
    fn restart_on_the_same_port_reuses_the_mascot_and_rebinds_its_process() {
        let src = "openclaw";
        let mut s = SceneState::default();
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::GatewayUp { pid: Some(100) },
            0,
        );
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::GatewayUp { pid: Some(200) },
            10,
        );
        assert_eq!(s.daemons().count(), 1, "a restart is the SAME instance");
        assert_eq!(
            s.daemon(src, &inst(A)).and_then(|p| p.current_pid),
            Some(200)
        );
    }

    #[test]
    fn an_abrupt_matching_exit_anchors_the_walk_out_clock_at_the_death_instant() {
        // An abrupt death (SIGKILL/OOM) lands no `GatewayDown`, so the synthesized
        // receipt's pid STILL MATCHES and this arm really transitions to Down — with
        // a `last_seen` that an idle gateway leaves minutes stale.
        let src = "openclaw";
        let mut s = SceneState::default();
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::GatewayUp { pid: Some(7) },
            0,
        );
        // 60s of idle silence, then the kill.
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::PidExited { pid: 7 },
            60_000,
        );
        let p = s.daemon(src, &inst(A)).expect("present");
        assert_eq!(
            p.liveness,
            DaemonLiveness::Down,
            "the matching pid downs it"
        );
        assert_eq!(
            p.last_seen,
            ms(60_000),
            "entering Down must anchor the walk-out clock at the DEATH instant, not \
             leave it at the last proof-of-life 60s earlier"
        );
    }

    #[test]
    fn a_non_matching_exit_receipt_does_not_restart_the_walk_out_clock() {
        // The ORDINARY clean stop: `GatewayDown` clears `current_pid`, so the armed
        // pid's receipt arrives NON-matching. Upstream awaits its stop hook before
        // closing, so this ordering is the norm, not a race.
        let src = "openclaw";
        let mut s = SceneState::default();
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::GatewayUp { pid: Some(7) },
            0,
        );
        apply_at(&mut s, src, A, DaemonPresenceUpdate::GatewayDown, 1_000);
        let at_down = s.daemon(src, &inst(A)).expect("present").last_seen;
        // The disarmed pid's receipt lands 3s later.
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::PidExited { pid: 7 },
            4_000,
        );
        assert_eq!(
            s.daemon(src, &inst(A)).expect("present").last_seen,
            at_down,
            "a death receipt must not refresh the presence clock — the walk-out is \
             timed off it"
        );
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::PidSeen { pid: 9 },
            5_000,
        );
        assert!(
            s.daemon(src, &inst(A)).expect("present").last_seen > at_down,
            "proof of life must still refresh it"
        );
    }

    #[test]
    fn a_stale_exit_receipt_for_the_replaced_process_is_a_no_op() {
        let src = "openclaw";
        let mut s = SceneState::default();
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::GatewayUp { pid: Some(100) },
            0,
        );
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::GatewayUp { pid: Some(200) },
            10,
        );
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::PidExited { pid: 100 },
            11,
        );
        assert_eq!(
            st_at(&s, src, A),
            Some(DaemonState::Idle),
            "the old generation's exit must not down the replacement"
        );
        apply_at(
            &mut s,
            src,
            B,
            DaemonPresenceUpdate::GatewayUp { pid: None },
            11,
        );
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::PidExited { pid: 200 },
            12,
        );
        assert_eq!(st_at(&s, src, A), Some(DaemonState::Down));
        assert_eq!(
            st_at(&s, src, B),
            Some(DaemonState::Idle),
            "one gateway's process exit is instance-local"
        );
    }

    #[test]
    fn exit_receipt_for_an_unseen_instance_creates_nothing() {
        let mut s = SceneState::default();
        apply_at(
            &mut s,
            "openclaw",
            A,
            DaemonPresenceUpdate::PidExited { pid: 7 },
            0,
        );
        assert_eq!(s.daemons().count(), 0);
    }

    #[test]
    fn ttl_decay_is_instance_local() {
        let src = "openclaw";
        let ttl = PresenceTtl::DEFAULT;
        let mut s = SceneState::default();
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::GatewayUp { pid: None },
            0,
        );
        let late = ttl.presence_ttl_ms + 1;
        apply_at(
            &mut s,
            src,
            B,
            DaemonPresenceUpdate::GatewayUp { pid: None },
            late,
        );
        sweep_presence_ttl(&mut s, src, ttl, ms(late));
        assert_eq!(
            st_at(&s, src, A),
            Some(DaemonState::Down),
            "the silent gateway expires on its own clock"
        );
        assert_eq!(
            st_at(&s, src, B),
            Some(DaemonState::Idle),
            "the fresh sibling is untouched by its neighbour's expiry"
        );
        sweep_presence_ttl(&mut s, src, ttl, ms(late + ttl.down_remove_ms));
        assert!(st_at(&s, src, A).is_none(), "the Down instance is pruned");
        assert_eq!(st_at(&s, src, B), Some(DaemonState::Idle));
    }

    #[test]
    fn busy_decay_is_instance_local() {
        let src = "openclaw";
        let ttl = PresenceTtl::DEFAULT;
        let mut s = SceneState::default();
        for id in [A, B] {
            apply_at(
                &mut s,
                src,
                id,
                DaemonPresenceUpdate::RunStarted {
                    run_key: "r".into(),
                },
                0,
            );
        }
        // B keeps working; A's `agent_end` was dropped.
        let t = ttl.busy_decay_ms;
        apply_at(
            &mut s,
            src,
            B,
            DaemonPresenceUpdate::RunStarted {
                run_key: "r2".into(),
            },
            t,
        );
        sweep_presence_ttl(&mut s, src, ttl, ms(t));
        assert_eq!(
            st_at(&s, src, A),
            Some(DaemonState::Idle),
            "A's stranded run decays on A's own clock"
        );
        assert_eq!(
            st_at(&s, src, B),
            Some(DaemonState::Busy),
            "B is still genuinely busy"
        );
    }

    #[test]
    fn removing_the_last_instance_leaves_no_husk_source_entry() {
        // A source level with ZERO instances would be a third state no consumer
        // models: `daemons()` flattens, `gateway_rollup`'s `None` means ABSENT, and
        // the floating tick gate's `.all()` is vacuously true on an empty set.
        let src = "openclaw";
        let ttl = PresenceTtl::DEFAULT;
        let mut s = SceneState::default();
        apply_at(&mut s, src, A, DaemonPresenceUpdate::GatewayDown, 0);
        assert_eq!(s.daemons().count(), 1);
        sweep_presence_ttl(&mut s, src, ttl, ms(ttl.down_remove_ms));
        assert_eq!(s.daemons().count(), 0, "the instance is gone");
        assert!(
            s.daemon(src, &inst(A)).is_none() && st_at(&s, src, A).is_none(),
            "no husk source entry survives its last instance"
        );
        // Every accessor FLATTENS, so the serialized shape is the only place the
        // prune is observable — without this assertion, disabling it passes.
        assert_eq!(
            serde_json::to_string(&s.daemons).expect("the roster serializes"),
            "{}",
            "the emptied source level must be GONE, not an empty husk map"
        );
        apply_at(
            &mut s,
            src,
            B,
            DaemonPresenceUpdate::GatewayUp { pid: None },
            1,
        );
        assert_eq!(s.daemons().count(), 1, "a later gateway re-creates cleanly");
    }

    #[test]
    fn a_stranded_run_expires_while_the_gateway_keeps_serving() {
        let src = "openclaw";
        let ttl = PresenceTtl::DEFAULT;
        let mut s = SceneState::default();
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::RunStarted {
                run_key: "stranded".into(),
            },
            0,
        );
        // The gateway stays chatty right up to the decay boundary, so the daemon-wide
        // `last_seen` is always fresh — only a per-RUN clock can expire the lease.
        let mut t = 0;
        while t < ttl.busy_decay_ms {
            t += ttl.busy_decay_ms / 4;
            apply_at(&mut s, src, A, DaemonPresenceUpdate::SessionStarted, t);
            sweep_presence_ttl(&mut s, src, ttl, ms(t));
        }
        assert_eq!(
            st_at(&s, src, A),
            Some(DaemonState::Idle),
            "the stranded run must expire on its own clock, not the daemon's"
        );
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::RunStarted {
                run_key: "live".into(),
            },
            t,
        );
        sweep_presence_ttl(&mut s, src, ttl, ms(t + ttl.busy_decay_ms - 1));
        assert_eq!(st_at(&s, src, A), Some(DaemonState::Busy));
    }

    #[test]
    fn expiring_a_stranded_run_never_heals_a_degraded_gateway() {
        let src = "openclaw";
        let ttl = PresenceTtl::DEFAULT;
        let mut s = SceneState::default();
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::RunStarted {
                run_key: "stranded".into(),
            },
            0,
        );
        apply_at(
            &mut s,
            src,
            A,
            DaemonPresenceUpdate::RunFailed {
                run_key: "other".into(),
            },
            1,
        );
        assert_eq!(st_at(&s, src, A), Some(DaemonState::Degraded));
        sweep_presence_ttl(&mut s, src, ttl, ms(ttl.busy_decay_ms + 1));
        assert_eq!(
            st_at(&s, src, A),
            Some(DaemonState::Degraded),
            "a lapsed run lease must not silently heal a broken backend"
        );
    }

    #[test]
    fn source_wide_disconnect_walks_out_every_instance() {
        let src = "openclaw";
        let mut s = SceneState::default();
        for id in [A, B] {
            apply_at(
                &mut s,
                src,
                id,
                DaemonPresenceUpdate::GatewayUp { pid: None },
                0,
            );
        }
        apply_at(
            &mut s,
            "daemon2",
            A,
            DaemonPresenceUpdate::GatewayUp { pid: None },
            0,
        );
        mark_presence_down(&mut s, src, ms(5));
        assert_eq!(st_at(&s, src, A), Some(DaemonState::Down));
        assert_eq!(st_at(&s, src, B), Some(DaemonState::Down));
        assert_eq!(
            st_at(&s, "daemon2", A),
            Some(DaemonState::Idle),
            "a different source's daemon keeps running"
        );
    }
}
