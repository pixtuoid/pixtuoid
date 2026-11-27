//! hermes's DELEGATING run, recorded from a one-shot `delegate_task`.
//! Conformance skips it (`OUTSIDE_CONFORMANCE`): the child is a hermes session
//! of its own, a second sprite the one-AgentId rule refuses.

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, HOOK_PAYLOADS, Reach};
use pixtuoid_core::source::AgentEvent;
use serde_json::Value;

use super::captures::{fixture_lines, sources_root};

#[test]
fn a_delegated_child_works_nested_under_its_parent_until_its_stop_ends_it_alone() {
    let lines = fixture_lines(
        &sources_root()
            .join("hermes/fixtures/subagent-hermetic")
            .join(HOOK_PAYLOADS),
    );
    let payloads: Vec<Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("a captured payload is JSON"))
        .collect();
    let event_at = |name: &str| {
        payloads
            .iter()
            .position(|v| v["hook_event_name"] == name)
            .unwrap_or_else(|| panic!("the run fires {name}"))
    };
    let start = &payloads[event_at("subagent_start")];
    let id = |key: &str| AgentId::from_parts("hermes", start[key].as_str().expect("an id"));
    let (parent, child) = (id("parent_session_id"), id("child_session_id"));
    let stop = event_at("subagent_stop");

    let working = Drive::hooks().lines(&lines[..stop]);
    working.assert_clean("hermes delegation, up to its child's stop");
    assert_eq!(working.root(), Some(parent));
    assert_eq!(
        working.scene.agents.get(&child).map(|s| s.parent_id),
        Some(Some(parent)),
        "the child registers under its parent"
    );
    assert_eq!(working.registered(), 2, "one sprite per session");
    assert!(working.reaches.contains(&(parent, Reach::Delegating)));
    assert!(
        working.events.iter().any(|e| matches!(e,
            AgentEvent::ToolUseStart { agent_id, detail: Some(d), .. }
                if *agent_id == child && !d.is_task())),
        "the child's own call lands on the child"
    );
    assert!(
        working.scene.agents[&child].exiting_at.is_none(),
        "the child stays until its stop"
    );

    let stopped = Drive::hooks().lines(&lines[..=stop]);
    stopped.assert_clean("hermes delegation, through its child's stop");
    assert!(
        stopped.scene.agents[&child].exiting_at.is_some(),
        "its stop ends the child"
    );
    assert!(
        stopped.scene.agents[&parent].exiting_at.is_none(),
        "and the child alone"
    );
}
