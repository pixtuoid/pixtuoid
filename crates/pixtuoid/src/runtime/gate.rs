//! The connection-gate functional core — the pure heart of `reducer_task`'s
//! event/presence/sweep loop. `driver.rs::reducer_task` is the thin imperative
//! shell that wires the tokio channels to these functions; nothing here touches
//! IO, tokio, or the clock (`now` is a parameter), so the gate decision itself
//! is measured and mutation-checked rather than living in the excluded shell.

use std::collections::HashSet;
use std::time::SystemTime;

use pixtuoid_core::source::{daemon, registry};
use pixtuoid_core::{AgentEvent, Reducer, SceneState, Transport};

use super::ConnectedSources;

fn event_source<'a>(scene: &'a SceneState, ev: &'a AgentEvent) -> Option<&'a str> {
    match ev {
        AgentEvent::SessionStart { source, .. } | AgentEvent::Identity { source, .. }
            if !source.is_empty() =>
        {
            Some(source)
        }
        _ => scene.agents.get(&ev.agent_id()).map(|s| s.source.as_ref()),
    }
}

/// Apply one incoming event through the connection gate. Drops it (returns
/// `false`) when its source resolves and is not connected.
///
/// The drop is logged once per registered source — the only breadcrumb below the
/// gate, and never per-event (a disconnected watcher streams indefinitely). Keyed
/// by the REGISTRY, not the wire: keying raw `_pixtuoid_source` names would let a
/// long-lived `run` accumulate one entry per distinct name seen.
pub(crate) fn apply_gated_event(
    reducer: &mut Reducer,
    scene: &mut SceneState,
    ev: AgentEvent,
    transport: Transport,
    connected: &ConnectedSources,
    now: SystemTime,
    gate_logged: &mut HashSet<&'static str>,
) -> bool {
    if let Some(src) = event_source(scene, &ev).filter(|s| !connected.is_connected(s)) {
        if let Some(known) = registry::descriptor_for(src)
            && gate_logged.insert(known.name)
        {
            tracing::debug!(source = known.name, "dropping events: source not connected");
        }
        return false;
    }
    tracing::debug!(?transport, ?ev, "event");
    reducer.apply(scene, ev, now, transport);
    true
}

/// One reconcile-sweep tick — the cadence `reducer_task` runs so exit-grace
/// sweeps fire even when no events arrive. Walks out every slot whose source is
/// not in the connected snapshot. Stateless and registry-driven on purpose: no
/// prev-set bookkeeping, and an Nth daemon needs no edit here.
pub(crate) fn reconcile_sweep_tick(
    reducer: &mut Reducer,
    scene: &mut SceneState,
    connected: &ConnectedSources,
    now: SystemTime,
) {
    let cur = connected.snapshot();
    reducer.reconcile_connected(scene, &cur, now);
    reducer.tick(scene, now);
    daemon::sweep_registered(scene, now, |source| connected.is_connected(source));
}

/// `Applied` carries the gateway pid the shell should arm the abrupt-down exit
/// watch on (`None` when the delta arms nothing). The enum makes "dropped but has
/// a pid to arm" unrepresentable.
pub(crate) enum PresenceGate {
    Dropped,
    Applied { arm_pid: Option<i32> },
}

/// The presence twin of [`apply_gated_event`]: a disconnected daemon's delta is
/// dropped here and its slot walked out by [`reconcile_sweep_tick`] instead. The
/// `ExitWatch` registration and the scene publish stay in the shell.
pub(crate) fn apply_gated_presence(
    scene: &mut SceneState,
    key: &daemon::DaemonInstanceKey,
    delta: daemon::DaemonPresenceUpdate,
    connected: &ConnectedSources,
    now: SystemTime,
) -> PresenceGate {
    // The gate is SOURCE-level (one Sources-panel row per CLI): the instance
    // dimension is rendering identity, never a second connection axis.
    if !connected.is_connected(key.source()) {
        return PresenceGate::Dropped;
    }
    let arm_pid = delta.armable_pid();
    daemon::apply_presence(scene, key, delta, now);
    PresenceGate::Applied { arm_pid }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `source`'s recorded `scenario` payloads, stamped `_pixtuoid_source` as
    /// the shim relays them (the only attribution the gate trusts).
    fn recorded_lines(source: &str, scenario: &str) -> Vec<String> {
        use pixtuoid_core::harness::{HOOK_PAYLOADS, fixtures_dir, sources_root};
        let path = fixtures_dir(&sources_root(), source)
            .join(scenario)
            .join(HOOK_PAYLOADS);
        std::fs::read_to_string(&path)
            .expect("the recorded run")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// The recorded cursor tool run's payloads: each one's event name, and what
    /// `Drive` decodes it to.
    fn cursor_payloads() -> Vec<(String, Vec<AgentEvent>)> {
        recorded_lines("cursor", "tool-run")
            .into_iter()
            .map(|l| {
                let payload: serde_json::Value =
                    serde_json::from_str(&l).expect("a recorded payload is JSON");
                let event = payload["hook_event_name"].as_str().expect("a named event");
                let events = pixtuoid_core::harness::Drive::hooks().lines([&l]).events;
                (event.to_string(), events)
            })
            .collect()
    }

    fn cursor_payload(event: &str) -> Vec<AgentEvent> {
        cursor_payloads()
            .into_iter()
            .find_map(|(name, events)| (name == event).then_some(events))
            .unwrap_or_else(|| panic!("the run records a {event}"))
    }

    // Returns (slots BEFORE the reconcile, live slots AFTER) so the gate and the
    // sweep reconciler get independent teeth: a single "after" count would pass
    // even with the gate removed. The gate runs on `connected_at_gate`, the
    // reconcile on `connected_at_sweep`.
    fn drive_hook_gate(
        events: Vec<AgentEvent>,
        connected_at_gate: &[&str],
        connected_at_sweep: &[&str],
    ) -> (usize, usize) {
        let sources =
            |names: &[&str]| ConnectedSources::new(names.iter().map(|s| s.to_string()).collect());
        let cs = sources(connected_at_gate);
        let mut scene = SceneState::uniform(8);
        let mut reducer = Reducer::new();
        let mut gate_logged = HashSet::new();
        let now = SystemTime::now();
        for ev in events {
            apply_gated_event(
                &mut reducer,
                &mut scene,
                ev,
                Transport::Hook,
                &cs,
                now,
                &mut gate_logged,
            );
        }
        let gated = scene.agents.len();
        reconcile_sweep_tick(&mut reducer, &mut scene, &sources(connected_at_sweep), now);
        let live = scene
            .agents
            .values()
            .filter(|s| s.exiting_at.is_none())
            .count();
        (gated, live)
    }

    /// No payload of a disconnected source's recorded run registers.
    #[test]
    fn a_disconnected_sources_hook_payloads_never_register() {
        let run = cursor_payloads().into_iter().flat_map(|(_, events)| events);
        assert_eq!(
            drive_hook_gate(run.collect(), &["claude-code"], &["claude-code"]),
            (0, 0)
        );
    }

    #[test]
    fn hook_events_for_a_connected_source_render() {
        assert_eq!(
            drive_hook_gate(cursor_payload("preToolUse"), &["cursor"], &["cursor"]),
            (1, 1)
        );
    }

    #[test]
    fn the_sweep_tick_walks_out_a_source_disconnected_after_it_registered() {
        let run = cursor_payload("preToolUse");
        assert_eq!(drive_hook_gate(run, &["cursor"], &["claude-code"]), (1, 0));
    }

    #[test]
    fn event_source_extracts_source_for_the_connection_gate() {
        use pixtuoid_core::AgentId;
        use pixtuoid_core::state::MAX_FLOORS;
        use std::path::PathBuf;

        use crate::runtime::FALLBACK_DESKS;

        let now = SystemTime::now();
        let mut scene = SceneState::new([FALLBACK_DESKS; MAX_FLOORS]);
        let mut reducer = Reducer::new();
        let id = AgentId::from_transcript_path("/p/a.jsonl");

        let ss = AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        };
        assert_eq!(event_source(&scene, &ss), Some("claude-code"));

        let idy = AgentEvent::Identity {
            agent_id: id,
            source: "codex".into(),
            session_id: "s".into(),
            cwd: None,
            pid: None,
        };
        assert_eq!(event_source(&scene, &idy), Some("codex"));

        let act = AgentEvent::tool_use_start(id, None, None);
        assert_eq!(event_source(&scene, &act), None);

        reducer.apply(&mut scene, ss, now, Transport::Jsonl);
        assert_eq!(event_source(&scene, &act), Some("claude-code"));

        let empty = AgentEvent::Identity {
            agent_id: id,
            source: String::new(),
            session_id: "s".into(),
            cwd: None,
            pid: None,
        };
        assert_eq!(event_source(&scene, &empty), Some("claude-code"));
    }

    fn utc_day(now: SystemTime) -> chrono::NaiveDate {
        chrono::DateTime::<chrono::Utc>::from(now).date_naive()
    }

    /// Through the gate as the tailer sends it, a declaration passing or falling
    /// by its slot's source as any event does.
    #[test]
    fn a_backlog_passes_the_gate_as_its_agents_events_do() {
        use pixtuoid_core::AgentId;
        use std::path::PathBuf;

        let id = AgentId::from_parts("claude-code", "s");
        let start = || AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        };
        let opens = || AgentEvent::Backlog {
            agent_id: id,
            replaying: false,
        };
        let usage = || AgentEvent::usage(id, 7, None);
        let on = ConnectedSources::new(["claude-code".to_string()].into());
        let off = ConnectedSources::new(HashSet::new());
        let rows = [
            (
                "ahead of its agent's slot it names no source, and reaches the reducer",
                [(opens(), &on), (start(), &on), (usage(), &on)],
                Some(7),
            ),
            (
                "a disconnected source's agent stays closed until its next read",
                [(start(), &on), (opens(), &off), (usage(), &on)],
                None,
            ),
        ];
        for (row, events, tokens) in rows {
            let mut scene = SceneState::uniform(8);
            let mut reducer = Reducer::new().counting(utc_day);
            let mut gate_logged = HashSet::new();
            for (ev, connected) in events {
                apply_gated_event(
                    &mut reducer,
                    &mut scene,
                    ev,
                    Transport::Jsonl,
                    connected,
                    SystemTime::now(),
                    &mut gate_logged,
                );
            }
            assert_eq!(scene.today.map(|today| today.tokens), tokens, "{row}");
        }
    }

    /// The driver's one tick is the reducer's, so a quiet midnight retires the
    /// day in a live run too.
    #[test]
    fn the_sweep_tick_retires_a_day_that_has_passed() {
        let cs = ConnectedSources::new(HashSet::new());
        let mut scene = SceneState::uniform(8);
        let mut reducer = Reducer::new().counting(utc_day);
        let now = SystemTime::now();
        let yesterday = utc_day(now).pred_opt().expect("a date");
        scene.today = Some(pixtuoid_core::state::Today::new(yesterday, 1, 1));
        reconcile_sweep_tick(&mut reducer, &mut scene, &cs, now);
        assert_eq!(scene.today, None);
    }

    fn daemon_key(source: &str, instance: &str) -> daemon::DaemonInstanceKey {
        daemon::DaemonInstanceKey::new(
            source,
            pixtuoid_core::state::DaemonInstanceId::new(instance).expect("non-empty"),
        )
    }

    #[test]
    fn presence_gate_drops_a_disconnected_daemon_and_applies_a_connected_one() {
        use pixtuoid_core::source::daemon::DaemonPresenceUpdate;
        use pixtuoid_core::state::DaemonState;

        let cs = ConnectedSources::new(["openclaw".to_string()].into_iter().collect());
        let mut scene = SceneState::uniform(8);
        let now = SystemTime::now();

        let other = daemon_key("not-connected", "18789");
        let dropped = apply_gated_presence(
            &mut scene,
            &other,
            DaemonPresenceUpdate::GatewayUp { pid: Some(4321) },
            &cs,
            now,
        );
        assert!(
            matches!(dropped, PresenceGate::Dropped),
            "a disconnected daemon's presence must be dropped"
        );
        assert!(scene.daemon(other.source(), other.instance()).is_none());

        let oc = daemon_key("openclaw", "18789");
        let applied = apply_gated_presence(
            &mut scene,
            &oc,
            DaemonPresenceUpdate::GatewayUp { pid: Some(4321) },
            &cs,
            now,
        );
        assert!(
            matches!(
                applied,
                PresenceGate::Applied {
                    arm_pid: Some(4321)
                }
            ),
            "a connected daemon's presence must apply and arm its GatewayUp pid"
        );
        assert_eq!(
            scene
                .daemon(oc.source(), oc.instance())
                .map(|p| p.display_state()),
            Some(DaemonState::Idle),
            "apply_presence must land the GatewayUp in scene.daemons"
        );
    }

    #[test]
    fn the_connection_gate_is_source_wide_across_every_instance() {
        use pixtuoid_core::source::daemon::DaemonPresenceUpdate;

        let cs = ConnectedSources::new(["openclaw".to_string()].into_iter().collect());
        let mut scene = SceneState::uniform(8);
        let now = SystemTime::now();
        for port in ["18789", "19789"] {
            let k = daemon_key("openclaw", port);
            assert!(matches!(
                apply_gated_presence(
                    &mut scene,
                    &k,
                    DaemonPresenceUpdate::GatewayUp { pid: None },
                    &cs,
                    now
                ),
                PresenceGate::Applied { .. }
            ));
        }
        assert_eq!(scene.daemons().count(), 2, "both gateways render");
        for port in ["18789", "19789"] {
            let k = daemon_key("other-daemon", port);
            assert!(matches!(
                apply_gated_presence(
                    &mut scene,
                    &k,
                    DaemonPresenceUpdate::GatewayUp { pid: None },
                    &cs,
                    now
                ),
                PresenceGate::Dropped
            ));
        }
        assert_eq!(
            scene.daemons().count(),
            2,
            "a disconnected source contributes no instance"
        );
    }

    /// The recorded openclaw clean stop's payloads, in order: `gateway_start`
    /// through the trailing `session_end`, each stamped with the gateway's pid.
    fn openclaw_clean_stop() -> Vec<String> {
        recorded_lines("openclaw", "tool-run-hermetic")
    }

    /// The recorded payload of type `ty`, and the pid it carries.
    fn recorded(lines: &[String], ty: &str) -> (String, i32) {
        lines
            .iter()
            .find_map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).expect("a recorded payload");
                (v["type"] == ty).then(|| {
                    let pid = v[pixtuoid_core::source::decoder::PID_STAMP]
                        .as_i64()
                        .expect("a stamped pid");
                    (l.clone(), i32::try_from(pid).expect("a pid"))
                })
            })
            .unwrap_or_else(|| panic!("the run records a {ty}"))
    }

    /// `reducer_task`'s presence arm: each line's deltas through the gate, each
    /// armed pid bound to its daemon as the driver's `PresenceExitWatch` binds it.
    struct PresenceRun {
        scene: SceneState,
        connected: ConnectedSources,
        armed: std::collections::HashMap<i32, HashSet<daemon::DaemonInstanceKey>>,
    }

    impl PresenceRun {
        fn new() -> Self {
            Self {
                scene: SceneState::uniform(8),
                connected: ConnectedSources::new(["openclaw".to_string()].into()),
                armed: Default::default(),
            }
        }

        fn line(&mut self, line: &str, at: SystemTime) {
            for msg in pixtuoid_core::harness::Drive::hooks()
                .lines([line])
                .presence
            {
                self.gate(msg.key, msg.delta, at);
            }
        }

        fn gate(
            &mut self,
            key: daemon::DaemonInstanceKey,
            delta: daemon::DaemonPresenceUpdate,
            at: SystemTime,
        ) {
            let gated = apply_gated_presence(&mut self.scene, &key, delta, &self.connected, at);
            if let PresenceGate::Applied { arm_pid: Some(pid) } = gated {
                self.armed.entry(pid).or_default().insert(key);
            }
        }

        /// `pid` dies: its receipt reaches every daemon bound to it, as many
        /// as there were.
        fn exit(&mut self, pid: i32, at: SystemTime) -> usize {
            let keys = self.armed.remove(&pid).unwrap_or_default();
            for key in &keys {
                self.gate(
                    key.clone(),
                    daemon::DaemonPresenceUpdate::PidExited { pid },
                    at,
                );
            }
            keys.len()
        }

        fn gateway(&self) -> &pixtuoid_core::state::DaemonPresence {
            let mut daemons = self.scene.daemons();
            let (_, _, p) = daemons.next().expect("the gateway's entry");
            assert!(daemons.next().is_none(), "one gateway");
            p
        }
    }

    fn at_ms(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_800_000_000_000 + ms)
    }

    /// Its process exits seconds after the stop: the receipt finds no adopted pid,
    /// so the Down entry, and the removal clock it carries, stay the stop's.
    #[test]
    fn the_recorded_clean_stops_exit_leaves_its_down_entry_as_it_was() {
        use pixtuoid_core::state::DaemonLiveness;

        let lines = openclaw_clean_stop();
        let mut run = PresenceRun::new();
        for (i, line) in lines.iter().enumerate() {
            run.line(line, at_ms(i as u64));
        }
        let stopped = run.gateway().clone();
        assert_eq!(
            (stopped.liveness, stopped.current_pid),
            (DaemonLiveness::Down, None),
            "a Down gateway has no live pid"
        );
        let (_, pid) = recorded(&lines, "session_end");
        assert_eq!(run.exit(pid, at_ms(3_000)), 1, "its exit watch was armed");
        assert_eq!(run.gateway(), &stopped);
    }

    /// The Sources panel walked the gateway out while it kept serving; its next
    /// session brings it back with its pid adopted, so its death takes it down.
    #[test]
    fn a_gateway_back_from_a_disconnect_adopts_its_pid_again() {
        use pixtuoid_core::state::DaemonState;

        let lines = openclaw_clean_stop();
        let (start, pid) = recorded(&lines, "gateway_start");
        let (session, _) = recorded(&lines, "session_start");
        let mut run = PresenceRun::new();
        run.line(&start, at_ms(0));
        let mut reducer = Reducer::new();
        let off = ConnectedSources::new(HashSet::new());
        reconcile_sweep_tick(&mut reducer, &mut run.scene, &off, at_ms(1_000));
        assert_eq!(run.gateway().display_state(), DaemonState::Down);
        run.line(&session, at_ms(2_000));
        assert_eq!(
            (run.gateway().display_state(), run.gateway().current_pid),
            (DaemonState::Idle, Some(pid))
        );
        assert_eq!(run.exit(pid, at_ms(3_000)), 1);
        assert_eq!(run.gateway().display_state(), DaemonState::Down);
    }
}
