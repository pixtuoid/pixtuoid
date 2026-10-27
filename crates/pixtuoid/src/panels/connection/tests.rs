use super::*;
use crate::sources::{RowFacts, RowInput, build_rows_from};

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::manager::SourceDeath;
use pixtuoid_core::state::{ActivityState, AgentSlot, GlobalDeskIndex};

use crate::install::target::Target;

fn claude_target() -> &'static Target {
    crate::install::target::by_name("claude").expect("claude target registered")
}

/// A slot with the fields `live_for` reads; the rest are inert.
fn mk_slot(id: AgentId, source: &str, last_event_at: SystemTime) -> AgentSlot {
    AgentSlot {
        agent_id: id,
        source: Arc::from(source),
        session_id: Arc::from("s"),
        cwd: Arc::from(Path::new("/repo")),
        label: "x".into(),
        state: ActivityState::Idle,
        state_started_at: SystemTime::UNIX_EPOCH,
        created_at: SystemTime::UNIX_EPOCH,
        last_event_at,
        exiting_at: None,
        desk_index: GlobalDeskIndex(0),
        floor_idx: 0,
        tool_call_count: 0,
        running_ms: 0,
        unknown_cwd: false,
        parent_id: None,
        pid: None,
        model: None,
        effort: None,
        tokens_used: 0,
        last_usage: None,
    }
}

#[test]
fn build_rows_from_state_follows_connected_set_with_nocli_override() {
    let cc = claude_target();
    let present = |connected| RowInput {
        source_id: "claude",
        label_prefix: "cc",
        target: Some(cc),
        facts: Some(RowFacts {
            present: true,
            config_path: Some(PathBuf::from("/c")),
        }),
        connected,
        health: None,
    };
    let absent = |connected| RowInput {
        source_id: "claude",
        label_prefix: "cc",
        target: Some(cc),
        facts: Some(RowFacts {
            present: false,
            config_path: Some(PathBuf::from("/c")),
        }),
        connected,
        health: None,
    };
    let no_target = |connected| RowInput {
        source_id: "copilot",
        label_prefix: "cp",
        target: None,
        facts: None,
        connected,
        health: None,
    };
    let rows = build_rows_from(vec![
        present(true),    // present + connected → Connected
        present(false),   // present + unbound → Disconnected
        absent(true),     // CLI absent → NoCli even if a stale flag says connected
        no_target(true),  // no-target source binds via flag only → Connected
        no_target(false), // → Disconnected
    ]);
    assert_eq!(rows[0].state, ConnState::Connected);
    assert_eq!(rows[1].state, ConnState::Disconnected);
    // Without the carried bit a connected-but-absent NoCli would be
    // indistinguishable from a never-connected one, and un-disconnectable via
    // the panel — its hooks live in the config, not the missing binary.
    assert_eq!(rows[2].state, ConnState::NoCli { connected: true });
    assert_eq!(rows[3].state, ConnState::Connected);
    assert_eq!(rows[4].state, ConnState::Disconnected);
    assert_eq!(rows[0].display_name, "Claude Code");
    assert_eq!(rows[0].config_path, Some(PathBuf::from("/c")));
    assert_eq!(rows[3].display_name, "Copilot CLI");
    assert!(rows[3].target.is_none());
    assert!(rows[3].config_path.is_none());
}

#[test]
fn live_for_counts_groups_and_ages() {
    let base = SystemTime::UNIX_EPOCH;
    let now = base + Duration::from_secs(100);
    let mut scene = SceneState::uniform(8);
    let a = AgentId::from_transcript_path("/p/a.jsonl");
    let b = AgentId::from_transcript_path("/p/b.jsonl");
    let c = AgentId::from_transcript_path("/p/c.jsonl");
    // two claude-code agents (t=90 and t=95 → max → age 5s), one codex (t=80).
    scene
        .agents
        .insert(a, mk_slot(a, "claude-code", base + Duration::from_secs(90)));
    scene
        .agents
        .insert(b, mk_slot(b, "claude-code", base + Duration::from_secs(95)));
    scene
        .agents
        .insert(c, mk_slot(c, "codex", base + Duration::from_secs(80)));

    let none: &[SourceDeath] = &[];
    let cc = live_for(now, "claude-code", &scene, none);
    assert_eq!(
        cc.facet,
        LiveFacet::Agents {
            agents: 2,
            last_event_age: Some(Duration::from_secs(5))
        }
    );
    assert!(!cc.dead);

    let empty = live_for(now, "reasonix", &scene, none);
    assert_eq!(
        empty.facet,
        LiveFacet::Agents {
            agents: 0,
            last_event_age: None
        }
    );

    let health = [SourceDeath::new("codex", "boom")];
    let cx = live_for(now, "codex", &scene, &health);
    assert_eq!(
        cx.facet,
        LiveFacet::Agents {
            agents: 1,
            last_event_age: Some(Duration::from_secs(20))
        }
    );
    assert!(cx.dead);
}

#[test]
fn live_for_reports_a_daemons_instances_not_the_agent_slots_it_never_creates() {
    // A DAEMON source creates NO AgentSlot — its liveness is the instance
    // roster. Registry-driven (`is_daemon`), so this is about the source CLASS,
    // not the name openclaw.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let src = pixtuoid_core::source::openclaw::SOURCE_NAME;
    let none: &[SourceDeath] = &[];

    let mut scene = SceneState::uniform(8);
    assert_eq!(
        live_for(now, src, &scene, none).facet,
        LiveFacet::Daemon(None),
        "a daemon with no instance must not report an agent count"
    );

    // A slot that (impossibly) claimed the daemon's name must NOT be counted as
    // its liveness — the class decides the facet, not the data lying around.
    let ghost = AgentId::from_transcript_path("/p/ghost.jsonl");
    scene.agents.insert(
        ghost,
        mk_slot(ghost, src, SystemTime::UNIX_EPOCH + Duration::from_secs(99)),
    );

    let mut add = |port: &str, turns: &[&str]| {
        scene.insert_daemon(
            src,
            pixtuoid_core::state::DaemonInstanceId::new(port).expect("non-empty"),
            pixtuoid_core::state::DaemonPresence {
                liveness: pixtuoid_core::state::DaemonLiveness::UP,
                active_sessions: 0,
                last_seen: now,
                entered_at: now,
                in_flight_turns: turns
                    .iter()
                    .map(|r| (pixtuoid_core::state::TurnKey::Id(r.to_string().into()), now))
                    .collect(),
                current_pid: Some(1),
            },
        );
    };
    add("18789", &[]);
    add("19789", &["r1"]);
    assert_eq!(
        live_for(now, src, &scene, none).facet,
        LiveFacet::Daemon(Some(DaemonRollup {
            instances: std::num::NonZeroUsize::new(2).expect("two gateways"),
            state: pixtuoid_core::state::DaemonState::Running
        })),
        "two gateways, one running → 2 instances rolled up to Running"
    );

    // An AGENT source in the same scene is unaffected — the split is per class.
    assert!(matches!(
        live_for(now, "claude-code", &scene, none).facet,
        LiveFacet::Agents { .. }
    ));
}

#[test]
fn move_selection_clamps_at_both_ends() {
    let cc = claude_target();
    let rows = build_rows_from(vec![
        RowInput {
            source_id: "claude",
            label_prefix: "cc",
            target: Some(cc),
            facts: Some(RowFacts {
                present: true,
                config_path: None,
            }),
            connected: true,
            health: None,
        },
        RowInput {
            source_id: "copilot",
            label_prefix: "cp",
            target: None,
            facts: None,
            connected: false,
            health: None,
        },
    ]);
    assert_eq!(move_selection(&rows, 0, -1), 0);
    assert_eq!(move_selection(&rows, 0, 1), 1);
    assert_eq!(move_selection(&rows, 1, 1), 1);
}

#[test]
fn build_rows_covers_every_registry_source() {
    let rows = build_rows(&HashSet::new(), "");
    assert!(rows.len() >= 5, "expected the 5 install targets + sources");
    assert!(
        rows.iter()
            .any(|r| r.source_id == "copilot" && r.target.is_none()),
        "copilot must appear as a no-target (JSONL) row"
    );
}

/// A disconnected row carries no health line, even with drift its connected
/// self would report.
#[test]
fn a_disconnected_row_shows_no_health_line() {
    let log = format!(
        "ts  WARN {}: source=codex kind=unknown_event name=OtherHook",
        pixtuoid_core::source::drift::TARGET
    );
    let rows = build_rows(&HashSet::new(), &log);
    let cx = rows.iter().find(|r| r.source_id == "codex").unwrap();
    assert_ne!(cx.state, ConnState::Connected);
    assert_eq!(cx.health, None);
}

#[test]
fn build_rows_honors_the_connected_set() {
    let mut set = HashSet::new();
    set.insert("copilot".to_string()); // no-target → flag alone drives it
    let rows = build_rows(&set, "");
    let cp = rows.iter().find(|r| r.source_id == "copilot").unwrap();
    assert_eq!(cp.state, ConnState::Connected);
    for r in &rows {
        if r.source_id != "copilot" {
            assert_ne!(
                r.state,
                ConnState::Connected,
                "source {:?} not in the set but rendered Connected",
                r.source_id
            );
        }
    }
}

// A namespace drift between `SourceDescriptor.name` and `Target.core_source`
// would silently make a flagship row non-actionable.
#[test]
fn build_rows_makes_every_source_with_a_target_actionable() {
    use pixtuoid_core::source::registry::REGISTRY;
    let rows = build_rows(&HashSet::new(), "");
    for d in REGISTRY {
        let row = rows
            .iter()
            .find(|r| r.source_id == d.name)
            .unwrap_or_else(|| panic!("no Connection row for registered source {:?}", d.name));
        let has_target = crate::install::target::by_source(d.name).is_some();
        // `target.is_some()` IS actionability — the connection state itself
        // depends on FS presence, which is environment-specific.
        assert_eq!(
            row.target.is_some(),
            has_target,
            "source {:?} target join drifted (row target={:?}, registry has_target={has_target})",
            d.name,
            row.target.map(|t| t.name),
        );
    }
    let claude = rows.iter().find(|r| r.source_id == "claude-code").unwrap();
    assert!(
        claude.target.is_some(),
        "Claude Code row must be actionable"
    );
    assert_eq!(claude.display_name, "Claude Code");
}

#[test]
fn every_no_target_row_has_an_explicit_display_name_not_the_raw_id() {
    // A NO-TARGET source's name comes from `display_name_for`, whose
    // `other => other` fallthrough leaks the lowercase registry id into the
    // panel. Target-bearing rows are exempt: they use `Target.display_name`,
    // which may be a deliberate lowercase brand (e.g. "opencode").
    for row in build_rows(&HashSet::new(), "") {
        if row.target.is_none() {
            assert_ne!(
                row.display_name, row.source_id,
                "no-target source {:?} shows its raw id — add a title-cased arm to display_name_for",
                row.source_id
            );
        }
    }
}

#[test]
fn format_connect_result_renders_connected_plus_path_note() {
    use crate::install::{InstallOutcome, InstallReport};
    let base = |outcome, path_warning| InstallReport {
        outcome,
        path_warning,
        post_install_hint: None,
    };
    // Both outcomes read as "connected" (the flag flip is the real action).
    let plain = format_connect_result(&base(InstallOutcome::Installed, false), "Claude Code");
    assert_eq!(plain, "\u{2713} Claude Code connected");
    assert_eq!(
        format_connect_result(&base(InstallOutcome::AlreadyUpToDate, false), "Claude Code"),
        "\u{2713} Claude Code connected"
    );
    let noted = format_connect_result(&base(InstallOutcome::Installed, true), "Claude Code");
    assert!(noted.contains("connected"), "{noted}");
    assert!(noted.contains("PATH"), "{noted}");

    let hinted = format_connect_result(
        &InstallReport {
            outcome: InstallOutcome::Installed,
            path_warning: false,
            post_install_hint: Some("restart the gateway to load it"),
        },
        "OpenClaw",
    );
    assert!(hinted.contains("connected"), "{hinted}");
    assert!(
        hinted.contains("restart the gateway"),
        "a post-install step must ride the connect line: {hinted}"
    );
    // …and a target with no such step is byte-unchanged.
    assert_eq!(plain, "\u{2713} Claude Code connected");
}

#[test]
fn only_restart_bound_targets_declare_a_post_install_step_naming_it() {
    // A hint exists only where the write does not take effect on the CLI's next
    // run; each target's own row says why.
    let mut with_hint = Vec::new();
    for t in crate::install::TARGETS {
        if let Some(h) = t.post_install_hint {
            assert!(
                h.contains("restart"),
                "{}: a post-install hint must name the action: {h}",
                t.name
            );
            with_hint.push(t.core_source);
        }
    }
    assert_eq!(
        with_hint,
        vec![
            pixtuoid_core::source::codex::SOURCE_NAME,
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            pixtuoid_core::source::omp::SOURCE_NAME,
            pixtuoid_core::source::dsh::SOURCE_NAME,
        ],
        "only codex, openclaw, omp and dsh need a post-install step today"
    );
}

#[test]
fn format_disconnect_result_renders_disconnected_plus_registration_note() {
    use crate::install::{UninstallOutcome, UninstallReport};
    let removed = UninstallReport {
        outcome: UninstallOutcome::Removed,
        plugin_left_registered: None,
    };
    let s = format_disconnect_result(&removed, "Claude Code");
    assert_eq!(s, "\u{2713} Claude Code disconnected");

    let stranded = UninstallReport {
        plugin_left_registered: crate::install::target::CODEX.host.map(|h| h.cli),
        ..removed
    };
    let s = format_disconnect_result(&stranded, "Codex");
    assert!(
        s.contains("plugin left registered (codex not on PATH)"),
        "the CLI it could not reach, by name: {s}"
    );

    let nothing = UninstallReport {
        outcome: UninstallOutcome::NothingToRemove,
        plugin_left_registered: None,
    };
    let s2 = format_disconnect_result(&nothing, "Codex");
    assert_eq!(s2, "\u{2713} Codex disconnected");
}

#[test]
fn the_panel_words_every_failure_through_one_formatter() {
    let connect = format_failure(FailedOp::Connect, "Cursor", "boom");
    let disconnect = format_failure(FailedOp::Disconnect, "Cursor", "boom");

    assert_eq!(connect, "Cursor: connect failed \u{2014} boom");
    // Exact, not `contains`: "disconnect failed" CONTAINS "connect failed", so a
    // loose check passes for the opposite operation.
    assert_eq!(disconnect, "Cursor: disconnect failed \u{2014} boom");
}

#[test]
fn no_action_hint_distinguishes_nocli_from_actionable() {
    let cc = claude_target();
    let rows = build_rows_from(vec![
        RowInput {
            source_id: "claude",
            label_prefix: "cc",
            target: Some(cc),
            facts: Some(RowFacts {
                present: false, // → NoCli
                config_path: None,
            }),
            connected: false,
            health: None,
        },
        RowInput {
            source_id: "claude",
            label_prefix: "cc",
            target: Some(cc),
            facts: Some(RowFacts {
                present: true, // → Disconnected (actionable)
                config_path: None,
            }),
            connected: false,
            health: None,
        },
    ]);
    assert_eq!(rows[0].state, ConnState::NoCli { connected: false });
    assert!(
        no_action_hint(&rows[0]).contains("not detected"),
        "NoCli hint: {}",
        no_action_hint(&rows[0])
    );
    // The fallback arm the painter never routes to, but the toggle effect calls.
    assert!(
        no_action_hint(&rows[1]).contains("nothing to do"),
        "fallback hint: {}",
        no_action_hint(&rows[1])
    );
}
