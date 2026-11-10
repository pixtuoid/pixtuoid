//! opencode's DELEGATING run from a `subagent` call, recorded by the hermetic
//! tier. Conformance skips it (`OUTSIDE_CONFORMANCE`): the child is its own session,
//! two sprites, which the conformance harness's one-AgentId rule cannot hold
//! (the same reason `grok.rs` exists).

use pixtuoid_core::harness::{Drive, Driven};

fn hooks() -> Driven {
    let d = Drive::hooks().lines(super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("opencode/fixtures/subagent-hermetic/hook-payloads.jsonl"),
    ));
    d.assert_clean("opencode subagent hooks");
    d
}

#[test]
fn the_child_registers_under_its_parent() {
    let d = hooks();
    let root = d.root().expect("the root's first start registers it");
    let (child, linked) = d.linked_child().expect("the recording's child session");
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
/// past the b1 grace, which the fold's tick after each event crosses.
#[test]
fn the_dispatch_end_leaves_its_child_standing() {
    let d = hooks();
    let (child, _) = d.linked_child().expect("the recording's child session");
    assert!(
        d.scene
            .agents
            .get(&child)
            .is_some_and(|s| s.exiting_at.is_none()),
        "the dispatch's drain walked its child out"
    );
}
