//! opencode's DELEGATING run from a `subagent` call, recorded by the hermetic
//! tier. It lives here, not under `sources/fixtures/opencode/`, because the
//! child is its own session: two sprites, which the conformance harness's
//! one-AgentId rule cannot hold (the same reason `grok/mod.rs` exists).

use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, Driven};
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::{B1_CASCADE_GRACE, EXIT_GRACE_WINDOW, Reducer};

fn hooks() -> Driven {
    let d = Drive::hooks().lines(super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("opencode/fixtures/subagent-hermetic/hook-payloads.jsonl"),
    ));
    d.assert_clean("opencode subagent hooks");
    d
}

/// The recording's child and the parent its registration names.
fn child_of(d: &Driven) -> (AgentId, AgentId) {
    d.events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: Some(parent),
                ..
            } => Some((*agent_id, *parent)),
            _ => None,
        })
        .expect("the recording's child session")
}

#[test]
fn the_child_registers_under_its_parent() {
    let d = hooks();
    let root = d
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: None,
                ..
            } => Some(*agent_id),
            _ => None,
        })
        .expect("the root's first start registers it");
    let (child, linked) = child_of(&d);
    assert_eq!(linked, root, "the child names the root as its parent");
    assert_eq!(
        d.scene
            .agents
            .get(&child)
            .expect("the child's first start registers it")
            .parent_id,
        Some(root)
    );
}

/// Its drain says nothing about the child, a session of its own: still standing
/// past the b1 grace.
#[test]
fn the_dispatch_end_leaves_its_child_standing() {
    let d = hooks();
    let (child, _) = child_of(&d);
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
