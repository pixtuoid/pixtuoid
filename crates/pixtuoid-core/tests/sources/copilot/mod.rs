//! copilot's recorded endings at 1.0.98: two refusals, the `-p` auto-deny, whose
//! call then ends, and Esc at an interactive prompt, which aborts the turn with
//! no end for the call, each one call that resolves its wait; and a child whose
//! request errs, which its own failure ends.

use pixtuoid_core::harness::{Drive, Driven, Reach};
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::state::ActivityState;

fn assert_the_refusal_resolves_its_wait(what: &str, d: &Driven) {
    d.assert_clean(what);
    assert!(
        d.reached().contains(&Reach::Waiting),
        "{what}: never waited"
    );
    let slots: Vec<_> = d.scene.agents.values().collect();
    let [slot] = slots[..] else {
        panic!("{what}: {} slots", slots.len());
    };
    assert!(
        matches!(slot.state, ActivityState::Idle),
        "{what}: the wait never resolved (the sprite hangs until the stale-wait timeout)"
    );
    assert_eq!(
        slot.tool_call_count, 1,
        "{what}: a refused call is one call"
    );
}

#[test]
fn the_recorded_auto_deny_resolves_its_wait() {
    let t = super::captures::transcript_in(
        &super::captures::sources_root().join("fixtures/copilot/permission-hermetic"),
    );
    // Its shutdown would end the slot this reads.
    let lines = super::captures::fixture_lines(&t)
        .into_iter()
        .filter(|l| !l.contains(r#""type":"session.shutdown""#));
    let d = Drive::transcript_at("copilot", &t)
        .expect("copilot has a line decoder")
        .lines(lines);
    assert_the_refusal_resolves_its_wait("auto-deny", &d);
}

/// No headless run reaches the prompt, so these lines are hand-trimmed from a
/// pty run against aimock: unread keys dropped or blanked, and the turn's
/// `assistant.turn_end` and `abort` kept to show no `tool.execution_complete`
/// follows.
#[test]
fn an_esc_at_the_prompt_resolves_its_wait() {
    let d = Drive::transcript(
        "copilot",
        "/p/session-state/492b82f3-6fb8-427d-be59-e06c2d723d5c/events.jsonl",
    )
    .expect("copilot has a line decoder")
    .seeded()
    .lines([
        r#"{"type":"tool.execution_start","data":{"toolCallId":"call_-Dbzv4Ev_Get6cFh","toolName":"bash","arguments":{"command":"rm NOTE.txt","description":""},"turnId":"","model":"","toolTitle":""},"id":"","timestamp":"","parentId":""}"#,
        r#"{"type":"permission.requested","data":{"requestId":"","permissionRequest":{"kind":"shell","toolCallId":"call_-Dbzv4Ev_Get6cFh","fullCommandText":"","shellType":""},"agentMode":"","permissionMode":""},"id":"","timestamp":"","parentId":""}"#,
        r#"{"type":"permission.completed","data":{"requestId":"","toolCallId":"call_-Dbzv4Ev_Get6cFh","result":{"kind":"cancelled","reason":""}},"id":"","timestamp":"","parentId":""}"#,
        r#"{"type":"assistant.turn_end","data":{"turnId":""},"id":"","timestamp":"","parentId":""}"#,
        r#"{"type":"abort","data":{"reason":""},"id":"","timestamp":"","parentId":""}"#,
    ]);
    assert_the_refusal_resolves_its_wait("Esc cancel", &d);
}

/// A child whose request errs, recorded: `subagent.failed` ends it before its
/// parent's failed dispatch could drain it, and the parent stops delegating.
#[test]
fn a_failed_child_leaves_and_its_parent_stops_delegating() {
    let t = super::captures::transcript_in(
        &super::captures::sources_root().join("copilot/fixtures/child-error-hermetic"),
    );
    // Its shutdown would end the parent this reads.
    let lines = super::captures::fixture_lines(&t)
        .into_iter()
        .filter(|l| !l.contains(r#""type":"session.shutdown""#));
    let d = Drive::transcript_at("copilot", &t)
        .expect("copilot has a line decoder")
        .seeded()
        .lines(lines);
    d.assert_clean("child error");
    let child = d
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: Some(parent),
                ..
            } => Some((*agent_id, *parent)),
            _ => None,
        })
        .expect("the recording's child");
    assert!(d.reaches.contains(&(child.1, Reach::Delegating)));
    let at = |end: &dyn Fn(&AgentEvent) -> bool| d.events.iter().position(end);
    let failed =
        at(&|e| matches!(e, AgentEvent::SessionEnd { agent_id, .. } if *agent_id == child.0));
    let drained = at(
        &|e| matches!(e, AgentEvent::ToolUseEnd { agent_id, tool_use_id: Some(_) } if *agent_id == child.1),
    );
    assert!(
        failed.is_some_and(|f| drained.is_none_or(|d| f < d)),
        "the child's own failure never ended it"
    );
    assert!(
        d.scene
            .agents
            .get(&child.0)
            .is_none_or(|s| s.exiting_at.is_some()),
        "the failed child stayed"
    );
    let parent = d.scene.agents.get(&child.1).expect("the parent's slot");
    assert!(
        !matches!(&parent.state, ActivityState::Active { detail: Some(d), .. } if &**d == "Delegating"),
        "the parent still delegates: {:?}",
        parent.state
    );
}
