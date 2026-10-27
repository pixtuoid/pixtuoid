//! Cursor's DELEGATING run, from a capture of a real `cursor-agent -p` turn that
//! was asked for a subagent. It lives here rather than under
//! `sources/fixtures/cursor/` because a child is an INDEPENDENT session — two
//! sprites, which the conformance harness's one-AgentId rule cannot hold.
//!
//! Both facts pinned here are the wire premises `source/cursor.rs` documents and
//! declines to "fix": the flat render, and the `Task` id that never pairs.

use std::collections::BTreeSet;

use pixtuoid_core::harness::Drive;
use pixtuoid_core::source::AgentEvent;

fn lines() -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root().join("cursor/fixtures/delegation/hook-payloads.jsonl"),
    )
}

/// The RAW envelopes, for the one assertion that is about the WIRE rather than
/// about what we decode from it.
fn payloads() -> Vec<serde_json::Value> {
    lines()
        .iter()
        .map(|l| serde_json::from_str(l).expect("valid hook json"))
        .collect()
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

#[test]
fn every_task_dispatch_id_is_unpaired_on_the_wire() {
    let (mut dispatched, mut ended) = (BTreeSet::new(), BTreeSet::new());
    for v in payloads() {
        let (Some(event), Some(id)) = (
            v.get("hook_event_name").and_then(|s| s.as_str()),
            v.get("tool_use_id").and_then(|s| s.as_str()),
        ) else {
            continue;
        };
        match event {
            "preToolUse" if v.get("tool_name").and_then(|s| s.as_str()) == Some("Task") => {
                dispatched.insert(id.to_string());
            }
            "postToolUse" | "postToolUseFailure" => {
                ended.insert(id.to_string());
            }
            _ => {}
        }
    }
    assert!(!dispatched.is_empty(), "the capture must contain a Task");
    assert!(
        dispatched.is_disjoint(&ended),
        "a Task id that DOES pair would make passing it through safe: {dispatched:?}"
    );
}
