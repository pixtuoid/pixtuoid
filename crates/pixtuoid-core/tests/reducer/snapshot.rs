//! Full-scene serialization regression net: a fixed-timestamp script through
//! the reducer, snapshotted whole, so any added / renamed / reshaped
//! `SceneState` / `AgentSlot` / `ActivityState` field — or a change to which
//! timestamp the reducer stamps — surfaces as a reviewable snapshot diff.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, ToolDetail, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::Reducer;

/// Fixed wall-clock so the snapshot's `SystemTime` fields are deterministic.
fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn full_scene_serialization_is_stable() {
    let mut r = Reducer::new();
    let mut scene = SceneState::uniform(8);

    let parent = AgentId::from_transcript_path("/proj/parent.jsonl");
    let child = AgentId::from_parts("claude-code", "/proj/parent/subagents/agent-1.jsonl");
    let solo = AgentId::from_transcript_path("/other/solo.jsonl");
    let idle = AgentId::from_transcript_path("/idle/sess.jsonl");
    let winding = AgentId::from_transcript_path("/wind/sess.jsonl");

    // The five agents cover all three ActivityState variants (Active in both
    // ToolDetail shapes) plus a populated Option<SystemTime>, across two sources.
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "claude-code".into(),
            session_id: "p".into(),
            cwd: PathBuf::from("/proj"),
            parent_id: None,
        },
        at(0),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: parent,
            tool_use_id: Some("task-1".into()),
            detail: Some(ToolDetail::Task),
        },
        at(1),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/proj"),
            parent_id: Some(parent),
        },
        at(1),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: child,
            tool_use_id: Some("tool-9".into()),
            detail: Some(ToolDetail::Generic {
                display: "Read · src/main.rs".into(),
            }),
        },
        at(2),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: solo,
            source: "codex".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/other"),
            parent_id: None,
        },
        at(3),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::Waiting {
            agent_id: solo,
            reason: "permission: Bash".into(),
            tool_use_id: None,
        },
        at(4),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: idle,
            source: "claude-code".into(),
            session_id: "i".into(),
            cwd: PathBuf::from("/idle"),
            parent_id: None,
        },
        at(5),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: winding,
            source: "claude-code".into(),
            session_id: "w".into(),
            cwd: PathBuf::from("/wind"),
            parent_id: None,
        },
        at(5),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: winding,
            tool_use_id: Some("w-1".into()),
            detail: Some(ToolDetail::Generic {
                display: "Bash · cargo test".into(),
            }),
        },
        at(6),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::ActivityEnd {
            agent_id: winding,
            tool_use_id: Some("w-1".into()),
        },
        at(7),
        Transport::Hook,
    );
    // Populates `exiting_at`: the snapshot's one non-null Option<SystemTime>.
    r.apply(
        &mut scene,
        AgentEvent::SessionEnd {
            agent_id: winding,
            as_child: false,
        },
        at(8),
        Transport::Hook,
    );

    insta::assert_yaml_snapshot!(scene);
}
