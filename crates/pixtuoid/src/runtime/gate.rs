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

/// One reconcile-sweep tick — the 1-Hz cadence `reducer_task` runs so exit-grace
/// sweeps fire even when no events arrive. Walks out every slot whose source is
/// the COMPLEMENT of the connected snapshot, so a blank-source slot that slipped
/// the per-event gate is swept too. Stateless and registry-driven on purpose: no
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
    for (source, ttl) in registry::daemon_sources() {
        if !connected.is_connected(source) {
            daemon::mark_presence_down(scene, source, now);
        }
        daemon::sweep_presence_ttl(scene, source, ttl, now);
    }
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

    // Returns (slots BEFORE the reconcile, live slots AFTER) so the gate and the
    // 1-Hz reconciler get independent teeth: a source-carrying event is dropped by
    // the GATE — 0 before reconcile — while a bare activity event only clears at
    // the RECONCILE. A single "after" count would pass even with the gate removed.
    fn drive_hook_gate(payload: &serde_json::Value, connected: &[&str]) -> (usize, usize) {
        use pixtuoid_core::source::decoder::decode_hook_payload;
        let cs = ConnectedSources::new(connected.iter().map(|s| s.to_string()).collect());
        let mut scene = SceneState::uniform(8);
        let mut reducer = Reducer::new();
        let mut gate_logged = HashSet::new();
        let now = SystemTime::now();
        for ev in decode_hook_payload(payload.clone()).expect("decode hook") {
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
        let before = scene.agents.len();
        reconcile_sweep_tick(&mut reducer, &mut scene, &cs, now);
        let live = scene
            .agents
            .values()
            .filter(|s| s.exiting_at.is_none())
            .count();
        (before, live)
    }

    fn cursor_hook(event: &str) -> serde_json::Value {
        // `_pixtuoid_source` is the only attribution the gate trusts — never the
        // public `source` field, which CC overloads for the SessionStart reason.
        serde_json::json!({
            "hook_event_name": event, "session_id": "c7-sess", "cwd": "",
            "conversation_id": "c7-sess", "workspace_roots": ["/x/proj"],
            "tool_name": "Shell", "tool_input": {"command": "ls"},
            "_pixtuoid_source": "cursor"
        })
    }

    #[test]
    fn the_gate_drops_a_disconnected_sources_carrying_hook_event_outright() {
        // A `sessionStart` decodes to a single source-carrying `SessionStart`, so
        // the per-event gate drops it before it ever registers — no transient.
        let (before, live) = drive_hook_gate(&cursor_hook("sessionStart"), &["claude-code"]);
        assert_eq!(
            before, 0,
            "the gate must drop a disconnected SessionStart pre-reconcile"
        );
        assert_eq!(live, 0, "and no sprite survives");
    }

    #[test]
    fn a_disconnected_sources_hook_activity_leaves_no_persistent_sprite() {
        // A bare activity event slips the gate once (its id has no slot yet) and
        // synthesizes a transient blank slot the reconcile sweeps. Pins the
        // CONTRACT, not the transient count, so a fix that closes the one-slot
        // window stays green.
        let (_before, live) = drive_hook_gate(&cursor_hook("preToolUse"), &["claude-code"]);
        assert_eq!(
            live, 0,
            "a disconnected source's hook activity must leave no live sprite after the reconcile"
        );
    }

    #[test]
    fn hook_events_for_a_connected_source_render() {
        let (_before, live) = drive_hook_gate(&cursor_hook("preToolUse"), &["cursor"]);
        assert_eq!(
            live, 1,
            "a connected source's hook activity must render exactly one live sprite"
        );
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

        let act = AgentEvent::ToolUseStart {
            agent_id: id,
            tool_use_id: None,
            detail: None,
        };
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
}
