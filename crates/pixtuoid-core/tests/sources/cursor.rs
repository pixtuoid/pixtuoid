//! Cursor's DELEGATING run, from a capture of a real `cursor-agent -p` turn that
//! was asked for a subagent. Conformance skips it (`OUTSIDE_CONFORMANCE`): a child
//! is an INDEPENDENT session — two sprites, which the conformance harness's
//! one-AgentId rule cannot hold.
//!
//! It pins that the child is a session of its own and that the parent's `Task`
//! dispatch ends on its own post.

use std::collections::BTreeSet;

use pixtuoid_core::harness::Drive;
use pixtuoid_core::source::{AgentEvent, ToolDetail};

fn lines() -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root().join("cursor/fixtures/delegation/hook-payloads.jsonl"),
    )
}

#[test]
fn a_delegating_run_is_two_sessions_only_the_parent_starts() {
    let mut ids = BTreeSet::new();
    let mut starts = 0;
    let d = Drive::hooks().lines(lines());
    d.assert_clean("cursor delegation hooks");
    for ev in d.events {
        ids.insert(ev.agent_id());
        if let AgentEvent::SessionStart { parent_id, .. } = ev {
            starts += 1;
            assert_eq!(parent_id, None, "the parent is a root");
        }
    }
    assert_eq!(ids.len(), 2, "parent + subagent, keyed apart by session_id");
    assert_eq!(starts, 1, "only the parent's sessionStart is registered");
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
