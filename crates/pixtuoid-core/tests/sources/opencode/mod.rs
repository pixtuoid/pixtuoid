//! opencode's DELEGATING runs from a `subagent` call, one captured live and one by the
//! hermetic tier. They live here, not under `sources/fixtures/opencode/`, because each
//! child is its own session: two sprites, which the conformance harness's one-AgentId
//! rule cannot hold (the same reason `grok/mod.rs` exists).

use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, Driven};
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::{B1_CASCADE_GRACE, EXIT_GRACE_WINDOW, Reducer};

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

/// Its drain says nothing about the child, a session of its own: still standing
/// past the b1 grace.
#[test]
fn the_dispatch_end_leaves_its_child_standing() {
    let d = Drive::hooks().lines(super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("opencode/fixtures/subagent-hermetic/hook-payloads.jsonl"),
    ));
    d.assert_clean("opencode subagent hooks");
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
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for ev in d.events {
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
            .is_some_and(|s| s.exiting_at.is_none()),
        "the dispatch's drain walked its child out"
    );
}
