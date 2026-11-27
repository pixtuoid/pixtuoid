//! Cursor's DELEGATING run, from a capture of a real `cursor-agent -p` turn that
//! was asked for a subagent. Conformance skips it (`OUTSIDE_CONFORMANCE`): a child
//! is an INDEPENDENT session — two sprites, which the conformance harness's
//! one-AgentId rule cannot hold.
//!
//! It pins that the child is a session of its own, linked to its parent by its
//! nested transcript path or else by the parent's `Task` post, and that the
//! dispatch ends on that post.

use std::collections::BTreeSet;

use pixtuoid_core::harness::Drive;
use pixtuoid_core::source::{AgentEvent, ToolDetail};

fn lines() -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root().join("cursor/fixtures/delegation/hook-payloads.jsonl"),
    )
}

#[test]
fn a_delegating_run_is_two_sessions_the_child_linked_to_the_parent() {
    let mut ids = BTreeSet::new();
    let mut parents = Vec::new();
    let d = Drive::hooks().lines(lines());
    d.assert_clean("cursor delegation hooks");
    for ev in d.events {
        ids.insert(ev.agent_id());
        if let AgentEvent::SessionStart {
            agent_id,
            parent_id,
            ..
        } = ev
        {
            parents.push((agent_id, parent_id));
        }
    }
    assert_eq!(ids.len(), 2, "parent + subagent, keyed apart by session_id");
    assert_eq!(
        parents.len(),
        2,
        "the parent's sessionStart and the child's link"
    );
    let roots: Vec<_> = parents.iter().filter(|(_, p)| p.is_none()).collect();
    assert_eq!(roots.len(), 1, "the parent is the one root");
    assert_eq!(
        parents
            .iter()
            .find_map(|(a, p)| p.filter(|_| *a != roots[0].0)),
        Some(roots[0].0),
        "the child's link names the parent"
    );
}

/// The parent leaves Delegating when its child is done: the post that ends
/// the dispatch carries the id its start does.
#[test]
fn a_task_dispatch_ends_on_the_post_that_carries_its_id() {
    let d = Drive::hooks().lines(lines());
    d.assert_clean("cursor delegation hooks");
    let (parent, id) = d
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolUseStart {
                agent_id,
                tool_use_id,
                detail: Some(ToolDetail::Task),
                ..
            } => Some((*agent_id, tool_use_id.clone())),
            _ => None,
        })
        .expect("the parent dispatches a Task");
    let id = id.expect("the dispatch carries its wire id");
    assert!(
        d.events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolUseEnd { agent_id, tool_use_id: Some(t) } if *agent_id == parent && *t == id
        )),
        "its post ends it"
    );
}

/// Every post of a short child can precede its transcript (`source::cursor`'s
/// module doc), so none names its parent. With the child's posts and the
/// parent's `sessionEnd` cut, the parent's `Task` post alone links the child
/// and ends it.
#[test]
fn a_child_naming_no_parent_is_linked_and_ended_by_its_parents_task_post() {
    let rows: Vec<(String, serde_json::Value)> = lines()
        .into_iter()
        .map(|l| {
            let v = serde_json::from_str(&l).expect("a payload");
            (l, v)
        })
        .collect();
    let session = |v: &serde_json::Value| v["session_id"].as_str().expect("keyed").to_owned();
    let event = |v: &serde_json::Value| v["hook_event_name"].as_str().map(str::to_owned);
    let parent = rows
        .iter()
        .find(|(_, v)| event(v).as_deref() == Some("sessionStart"))
        .map(|(_, v)| session(v))
        .expect("the parent starts");
    let cut = rows.iter().filter(|(_, v)| match event(v).as_deref() {
        Some("sessionEnd") => false,
        Some("postToolUse") => session(v) == parent,
        _ => true,
    });
    let d = Drive::hooks().lines(cut.map(|(l, _)| l.clone()));
    d.assert_clean("cursor delegation, the child's posts cut");
    assert!(
        !d.events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStart {
                parent_id: Some(_),
                ..
            }
        )),
        "no event of the child's names its parent"
    );
    let parent = pixtuoid_core::AgentId::from_parts("cursor", &parent);
    let child = d
        .scene
        .agents
        .keys()
        .copied()
        .find(|id| *id != parent)
        .expect("the child registers");
    let slot = &d.scene.agents[&child];
    assert_eq!(slot.parent_id, Some(parent), "the Task post links it");
    assert!(slot.exiting_at.is_some(), "the Task post ends it");
}

/// The child appears on its first tool event, before the wire names its
/// parent; the post whose transcript nests under the parent's links it then,
/// and the parent's `Task` post ends it.
#[test]
fn a_child_is_linked_to_its_parent_once_its_post_names_the_nested_transcript() {
    let parent = "38e379e1-c3e2-4c41-bb40-1be6466c5b84";
    let child = "22810a37-661d-41dd-a514-367c266a5f39";
    let line = |event: &str, session: &str, tool: &str, path: Option<&str>| {
        serde_json::json!({
            "hook_event_name": event, "session_id": session, "_pixtuoid_source": "cursor",
            "workspace_roots": ["/repo"], "tool_name": tool, "tool_use_id": tool,
            "transcript_path": path.map(|p| format!(
                "/Users/dev/.cursor/projects/repo/agent-transcripts/{parent}/{p}"
            )),
        })
        .to_string()
    };
    let nested = format!("subagents/{child}.jsonl");
    let child_id = pixtuoid_core::AgentId::from_parts("cursor", child);
    let lines = [
        line("sessionStart", parent, "", None),
        line("preToolUse", parent, "Task", None),
        line("preToolUse", child, "Read", None),
        line("postToolUse", child, "Read", Some(&nested)),
    ];
    let d = Drive::hooks().lines(lines.clone());
    d.assert_clean("a nested child's post");
    assert_eq!(
        d.scene.agents.get(&child_id).map(|s| s.parent_id),
        Some(Some(pixtuoid_core::AgentId::from_parts("cursor", parent)))
    );
    let done = Drive::hooks().lines(lines.into_iter().chain([line(
        "postToolUse",
        parent,
        "Task",
        Some(&format!("{parent}.jsonl")),
    )]));
    assert!(
        done.scene
            .agents
            .get(&child_id)
            .is_none_or(|s| s.exiting_at.is_some()),
        "the parent's Task post ends its child"
    );
}
