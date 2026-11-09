use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::Reducer;

use crate::{start, tool_use_start};

#[test]
fn session_start_with_cwd_derives_label_from_basename() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "abc".into(),
            cwd: PathBuf::from("/Users/me/Desktop/pixtuoid"),
            parent_id: None,
        },
        SystemTime::now(),
        Transport::Hook,
    );
    assert_eq!(&*scene.agents.get(&id).unwrap().label, "cc·pixtuoid");
}

#[test]
fn session_start_without_cwd_falls_back_to_cc_label() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "abc".into(),
            cwd: PathBuf::from(""),
            parent_id: None,
        },
        SystemTime::now(),
        Transport::Hook,
    );
    assert_eq!(&*scene.agents.get(&id).unwrap().label, "cc#1");
}

#[test]
fn session_start_label_caps_a_pathologically_long_cwd_basename() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let long_cwd = PathBuf::from(format!("/tmp/{}", "x".repeat(300)));
    let t0 = SystemTime::now();

    let direct = AgentId::from_transcript_path("/p/direct.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: direct,
            source: "claude-code".into(),
            session_id: "d".into(),
            cwd: long_cwd.clone(),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );

    let upgraded = AgentId::from_transcript_path("/p/upgraded.jsonl");
    tool_use_start(
        &mut r,
        &mut scene,
        upgraded,
        Some("t-1"),
        Some("Bash: ls"),
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: upgraded,
            source: "claude-code".into(),
            session_id: "u".into(),
            cwd: long_cwd,
            parent_id: None,
        },
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );

    let direct_label = scene.agents.get(&direct).unwrap().label.clone();
    let upgraded_label = scene.agents.get(&upgraded).unwrap().label.clone();
    assert!(
        upgraded_label.ends_with('…'),
        "sanity: the 300-char basename is ellipsized on the backfill path, got {upgraded_label:?}"
    );
    assert_eq!(
        &*direct_label, &*upgraded_label,
        "register_slot must mint the same capped label as the backfill upgrade"
    );
}

#[test]
fn ghost_label_counter_is_contiguous_after_named_sessions() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let named = AgentId::from_transcript_path("/p/named.jsonl");
    let ghost = AgentId::from_transcript_path("/p/ghost.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: named,
            source: "claude-code".into(),
            session_id: "named".into(),
            cwd: PathBuf::from("/Users/me/Desktop/pixtuoid"),
            parent_id: None,
        },
        SystemTime::now(),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: ghost,
            source: "claude-code".into(),
            session_id: "ghost".into(),
            cwd: PathBuf::from(""),
            parent_id: None,
        },
        SystemTime::now(),
        Transport::Hook,
    );
    assert_eq!(&*scene.agents.get(&named).unwrap().label, "cc·pixtuoid");
    assert_eq!(&*scene.agents.get(&ghost).unwrap().label, "cc#1");
}

#[test]
fn session_start_codex_source_gets_cx_label() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("codex", "sess-1");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "codex".into(),
            session_id: "sess-1".into(),
            cwd: PathBuf::from("/Users/me/work/myrepo"),
            parent_id: None,
        },
        SystemTime::now(),
        Transport::Hook,
    );
    assert_eq!(&*scene.agents.get(&id).unwrap().label, "cx·myrepo");
}

#[test]
fn rename_updates_slot_label() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start(&mut r, &mut scene, id);
    r.apply(
        &mut scene,
        AgentEvent::Rename {
            agent_id: id,
            label: "feature-dev:code-explorer".into(),
        },
        SystemTime::now(),
        Transport::Jsonl,
    );
    assert_eq!(
        &*scene.agents.get(&id).unwrap().label,
        "feature-dev:code-explorer"
    );
}

#[test]
fn rename_for_unknown_agent_is_noop() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/missing.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::Rename {
            agent_id: id,
            label: "x".into(),
        },
        SystemTime::now(),
        Transport::Jsonl,
    );
    assert!(!scene.agents.contains_key(&id));
}
