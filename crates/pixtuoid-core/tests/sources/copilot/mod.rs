//! copilot's two refusal endings at 1.0.98: the `-p` auto-deny, whose call
//! then ends, and Esc at an interactive prompt, which aborts the turn with no
//! end for the call. Each is one call, and each resolves its wait.

use pixtuoid_core::harness::{Drive, Driven, Reach};
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
