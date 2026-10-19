//! opencode's DELEGATING run, captured live from a `subagent` call. It lives
//! here rather than under `sources/fixtures/opencode/` because the child is its
//! own session — two sprites, which the conformance harness's one-AgentId rule
//! cannot hold (the same reason `grok/mod.rs` exists).

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, Driven};
use pixtuoid_core::source::{AgentEvent, ToolDetail};

const PARENT: &str = "ses_ecde1cdf7ffef0K19OMixy5WUV";
const CHILD: &str = "ses_ecde1c6b5ffexy67GfWtNzu6TA";

fn hooks() -> Driven {
    let d = Drive::hooks().lines(super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("opencode/fixtures/delete-recorded/hook-payloads.jsonl"),
    ));
    d.assert_clean("opencode delegation hooks");
    d
}

#[test]
fn the_child_registers_under_its_parent() {
    let d = hooks();
    let child = d
        .scene
        .agents
        .get(&AgentId::from_parts("opencode", CHILD))
        .expect("the child's session.created registers it");
    assert_eq!(
        child.parent_id,
        Some(AgentId::from_parts("opencode", PARENT))
    );
}

/// This capture's call blocked, but the decoder cannot tell it from one that
/// returns before its child finishes, so it mints no Task either.
#[test]
fn the_subagent_dispatch_mints_no_task() {
    let d = hooks();
    let starts: Vec<&AgentEvent> = d
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ActivityStart { .. }))
        .collect();
    assert!(
        starts.iter().any(|e| matches!(e,
            AgentEvent::ActivityStart { detail: Some(d), .. } if d.display() == "subagent")),
        "the capture must hold the dispatch, else this rule is untested: {starts:?}"
    );
    assert!(
        !starts.iter().any(|e| matches!(
            e,
            AgentEvent::ActivityStart {
                detail: Some(ToolDetail::Task),
                ..
            }
        )),
        "{starts:?}"
    );
}
