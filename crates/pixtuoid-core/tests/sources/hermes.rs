//! hermes's DELEGATING run, recorded from a one-shot `delegate_task`.
//! Conformance skips it (`OUTSIDE_CONFORMANCE`): the child is a hermes session
//! of its own, a second sprite the one-AgentId rule refuses.

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, HOOK_PAYLOADS, Reach};
use pixtuoid_core::source::AgentEvent;
use serde_json::Value;

use super::captures::{fixture_lines, sources_root};

/// The recording's lines, the parent and child its `subagent_start` names, and
/// the child's own session id.
struct Recording {
    lines: Vec<String>,
    parent: AgentId,
    child: AgentId,
    child_session: String,
}

fn recording() -> Recording {
    let lines = fixture_lines(
        &sources_root()
            .join("hermes/fixtures/subagent-hermetic")
            .join(HOOK_PAYLOADS),
    );
    let start =
        payloads(&lines).swap_remove(at(&lines, |v| v["hook_event_name"] == "subagent_start"));
    let session = |key: &str| start[key].as_str().expect("an id").to_owned();
    let id = |key: &str| AgentId::from_parts("hermes", &session(key));
    Recording {
        parent: id("parent_session_id"),
        child: id("child_session_id"),
        child_session: session("child_session_id"),
        lines,
    }
}

fn payloads(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("a captured payload is JSON"))
        .collect()
}

/// The first line `is` holds for.
fn at(lines: &[String], is: impl Fn(&Value) -> bool) -> usize {
    payloads(lines)
        .iter()
        .position(is)
        .expect("the run fires it")
}

fn stop_of(lines: &[String]) -> usize {
    at(lines, |v| v["hook_event_name"] == "subagent_stop")
}

#[test]
fn a_delegated_child_works_nested_under_its_parent_until_its_stop_ends_it_alone() {
    let Recording {
        lines,
        parent,
        child,
        ..
    } = recording();
    let stop = stop_of(&lines);

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

/// The same lines in an interactive session's order: its dispatch returns a
/// handle (`run_agent.py:1399-1402` at v0.23.3) while the child it started runs
/// on (`tools/delegate_tool_dispatch.py:509` at v0.23.3): the child still stands
/// past the b1 grace, which the fold's tick after each event crosses.
#[test]
fn a_dispatch_that_returns_before_its_child_leaves_the_child_working_until_its_stop() {
    let Recording {
        mut lines,
        parent,
        child,
        child_session,
    } = recording();
    let dispatch = payloads(&lines).swap_remove(at(&lines, |v| v["tool_name"] == "delegate_task"));
    let call = dispatch["tool_call_id"]
        .as_str()
        .expect("the dispatch names its call");
    let returned = lines.remove(at(&lines, |v| {
        v["hook_event_name"] == "post_tool_call" && v["tool_call_id"] == call
    }));
    let running = at(&lines, |v| {
        v["hook_event_name"] == "on_session_start" && v["session_id"] == child_session.as_str()
    });
    lines.insert(running + 1, returned);
    let stop = stop_of(&lines);

    let working = Drive::hooks().lines(&lines[..stop]);
    working.assert_clean("hermes background delegation, up to its child's stop");
    assert!(working.reaches.contains(&(parent, Reach::Delegating)));
    assert!(
        working.scene.agents[&child].exiting_at.is_none(),
        "the dispatch's return leaves its child working"
    );

    let stopped = Drive::hooks().lines(&lines[..=stop]);
    stopped.assert_clean("hermes background delegation, through its child's stop");
    assert!(
        stopped.scene.agents[&child].exiting_at.is_some(),
        "its stop ends the child"
    );
}
