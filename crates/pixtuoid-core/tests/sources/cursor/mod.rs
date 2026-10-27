//! Cursor's DELEGATING run, from a capture of a real `cursor-agent -p` turn that
//! was asked for a subagent. It lives here rather than under
//! `sources/fixtures/cursor/` because a child is an INDEPENDENT session — two
//! sprites, which the conformance harness's one-AgentId rule cannot hold.
//!
//! It pins the flat render `source/cursor.rs` documents, and that the parent's
//! `Task` dispatch ends on its own post.

use std::collections::BTreeSet;

use pixtuoid_core::harness::Drive;
use pixtuoid_core::source::{AgentEvent, ToolDetail};

fn lines() -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root().join("cursor/fixtures/delegation/hook-payloads.jsonl"),
    )
}

#[test]
fn a_delegating_run_is_two_unlinked_sessions() {
    let mut ids = BTreeSet::new();
    let mut starts = 0;
    let d = Drive::hooks().lines(lines());
    d.assert_clean("cursor delegation hooks");
    for ev in d.events {
        ids.insert(ev.agent_id());
        if let AgentEvent::SessionStart { parent_id, .. } = ev {
            starts += 1;
            assert_eq!(parent_id, None, "cursor carries no parent link on the wire");
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
