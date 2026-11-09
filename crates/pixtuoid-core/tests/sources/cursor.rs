//! Cursor's DELEGATING run, from a capture of a real `cursor-agent -p` turn that
//! was asked for a subagent. Conformance skips it (`OUTSIDE_CONFORMANCE`): a child
//! is an INDEPENDENT session — two sprites, which the conformance harness's
//! one-AgentId rule cannot hold.
//!
//! It pins that the child is a session of its own, linked to its parent by its
//! post, and that the parent's `Task` dispatch ends on its own post.

use std::collections::BTreeSet;

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, Driven};
use pixtuoid_core::source::{AgentEvent, ToolDetail};
use serde_json::Value;

fn lines() -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root().join("cursor/fixtures/delegation/hook-payloads.jsonl"),
    )
}

/// Each `SessionStart` in `d`: its agent, its parent and its wire session.
fn starts(d: &Driven) -> Vec<(AgentId, Option<AgentId>, &str)> {
    d.events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id,
                session_id,
                ..
            } => Some((*agent_id, *parent_id, session_id.as_str())),
            _ => None,
        })
        .collect()
}

/// The parent's `Task` dispatch in `d`: its agent and wire id.
fn dispatch(d: &Driven) -> (AgentId, String) {
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
    (parent, id.expect("the dispatch carries its wire id"))
}

/// `lines` through the first payload `stop_at` accepts.
fn through(lines: &[String], stop_at: impl Fn(&Value) -> bool) -> &[String] {
    let at = lines
        .iter()
        .position(|l| stop_at(&serde_json::from_str(l).expect("a recorded payload is JSON")))
        .expect("the recording holds that payload");
    &lines[..=at]
}

#[test]
fn a_delegating_run_is_two_sessions_the_child_starting_under_its_parent() {
    let d = Drive::hooks().lines(lines());
    d.assert_clean("cursor delegation hooks");
    let ids: BTreeSet<AgentId> = d.events.iter().map(AgentEvent::agent_id).collect();
    assert_eq!(ids.len(), 2, "parent + subagent, keyed apart by session_id");
    let (parent, _) = dispatch(&d);
    let child = ids.into_iter().find(|id| *id != parent).expect("the child");
    let starts: Vec<_> = starts(&d).into_iter().map(|(a, p, _)| (a, p)).collect();
    assert_eq!(
        starts,
        [(parent, None), (child, Some(parent))],
        "the parent is a root; the child's sessionStart never fires, so its nested post starts it"
    );
}

/// The parent leaves Delegating when its child is done: the post that ends
/// the dispatch carries the id its start does.
#[test]
fn a_task_dispatch_ends_on_the_post_that_carries_its_id() {
    let d = Drive::hooks().lines(lines());
    d.assert_clean("cursor delegation hooks");
    let (parent, id) = dispatch(&d);
    assert!(
        d.events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolUseEnd { agent_id, tool_use_id: Some(t) } if *agent_id == parent && *t == id
        )),
        "its post ends it"
    );
}

/// The child appears on its first tool event, before the wire names its
/// parent; its post, whose transcript nests under the parent's, links it then,
/// and the parent's `Task` post ends it before the parent's `sessionEnd` could.
#[test]
fn a_child_is_linked_by_its_nested_post_and_ended_by_the_parents_task_post() {
    let lines = lines();
    let d = Drive::hooks().lines(&lines);
    let starts = starts(&d);
    let [(parent, _, _), (child, _, child_session)] = starts.as_slice() else {
        panic!("a parent and a child start: {starts:?}");
    };
    let (_, task) = dispatch(&d);
    let post = |v: &Value| v["hook_event_name"] == "postToolUse";
    let linked = Drive::hooks().lines(through(&lines, |v| {
        post(v) && v["session_id"] == *child_session
    }));
    assert_eq!(
        linked.scene.agents.get(child).map(|s| s.parent_id),
        Some(Some(*parent))
    );
    let ended = Drive::hooks().lines(through(&lines, |v| {
        post(v) && v["tool_use_id"] == task.as_str()
    }));
    assert!(
        ended
            .scene
            .agents
            .get(child)
            .is_none_or(|s| s.exiting_at.is_some()),
        "the parent's Task post ends its child"
    );
}
