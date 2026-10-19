//! reasonix's DELEGATING runs, recorded by the hermetic tier: a foreground
//! `task` brackets its child with `SubagentStart`/`SubagentStop`, a skill
//! tool (`explore`) brackets nothing, and either child's hooks carry a
//! session id of its own, so each run is two sprites and cannot sit under the
//! conformance one-AgentId rule.

use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::Drive;
use pixtuoid_core::source::{AgentEvent, ToolDetail, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::{B1_CASCADE_GRACE, EXIT_GRACE_WINDOW, Reducer};
use serde_json::Value;

fn lines() -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("reasonix/fixtures/subagent-hermetic/hook-payloads.jsonl"),
    )
}

fn events() -> Vec<AgentEvent> {
    let d = Drive::hooks().lines(lines());
    d.assert_clean("reasonix delegation hooks");
    d.events
}

/// The `SubagentStart` payload's own fields: its session, and its call.
fn spawn() -> (String, String) {
    let start = lines()
        .iter()
        .map(|l| serde_json::from_str::<Value>(l).expect("a captured payload is JSON"))
        .find(|v| v["event"] == "SubagentStart")
        .expect("the run spawns a foreground child");
    let field = |k: &str| start[k].as_str().expect("a string field").to_string();
    (field("sessionId"), field("callId"))
}

#[test]
fn the_runs_one_task_sits_on_the_spawning_calls_id() {
    let (_, call) = spawn();
    let tasks: Vec<Option<String>> = events()
        .into_iter()
        .filter_map(|e| match e {
            AgentEvent::ActivityStart {
                tool_use_id,
                detail: Some(ToolDetail::Task),
                ..
            } => Some(tool_use_id),
            _ => None,
        })
        .collect();
    assert_eq!(tasks, [Some(call)]);
}

#[test]
fn the_childs_session_registers_under_its_parent() {
    let (parent, _) = spawn();
    let parent = AgentId::from_parts("reasonix", &parent);
    let children: Vec<AgentId> = events()
        .into_iter()
        .filter_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: Some(p),
                ..
            } if p == parent => Some(agent_id),
            _ => None,
        })
        .collect();
    assert!(
        !children.is_empty(),
        "no session registered under the parent"
    );
    assert!(
        children.iter().all(|c| *c != parent),
        "the parent is its own child"
    );
}

#[test]
fn a_skill_calls_return_walks_its_child_out() {
    let d = Drive::hooks().lines(super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("reasonix/fixtures/explore-hermetic/hook-payloads.jsonl"),
    ));
    d.assert_clean("reasonix explore hooks");
    let child = d
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: Some(_),
                ..
            } => Some(*agent_id),
            _ => None,
        })
        .expect("the recording's child session");
    // Up to the call's return, its re-stated Task and End, before the turn's
    // own end could end the child instead.
    let restated = d
        .events
        .iter()
        .rposition(|e| {
            matches!(
                e,
                AgentEvent::ActivityStart {
                    detail: Some(ToolDetail::Task),
                    ..
                }
            )
        })
        .expect("the return re-states the Task");
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for ev in d.events.into_iter().take(restated + 2) {
        r.apply(&mut scene, ev, t0, Transport::Hook);
    }
    r.tick(
        &mut scene,
        t0 + B1_CASCADE_GRACE + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(
        scene
            .agents
            .get(&child)
            .is_none_or(|s| s.exiting_at.is_some()),
        "the call returned and its child stayed"
    );
}
