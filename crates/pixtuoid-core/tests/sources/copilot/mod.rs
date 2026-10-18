//! copilot's two refusal endings at 1.0.98: the `-p` auto-deny, whose call
//! then ends, and Esc at an interactive prompt, which aborts the turn with no
//! end for the call. Each is one call, and each resolves its wait.

use pixtuoid_core::harness::{Drive, Driven, Reach};
use pixtuoid_core::state::ActivityState;

fn assert_the_refusal_resolves_its_wait(what: &str, d: &Driven) {
    d.assert_clean(what);
    assert!(d.reached.contains(&Reach::Waiting), "{what}: never waited");
    let slots: Vec<_> = d.scene.agents.values().collect();
    let [slot] = slots[..] else {
        panic!("{what}: {} slots", slots.len());
    };
    assert!(
        !matches!(slot.state, ActivityState::Waiting { .. }) || slot.pending_idle_at.is_some(),
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

/// No headless run reaches the prompt, so these line shapes come from a pty
/// run against aimock, sanitized to what the decoder reads.
#[test]
fn an_esc_at_the_prompt_resolves_its_wait() {
    let d = Drive::transcript(
        "copilot",
        "/p/session-state/492b82f3-6fb8-427d-be59-e06c2d723d5c/events.jsonl",
    )
    .expect("copilot has a line decoder")
    .lines([
        r#"{"type":"session.start","data":{"sessionId":"492b82f3-6fb8-427d-be59-e06c2d723d5c","context":{"cwd":"/repo"}},"id":"c8e243d1-c712-43c6-bda4-f27d253f34eb","timestamp":"2026-10-18T08:08:50.000Z","parentId":null}"#,
        r#"{"type":"tool.execution_start","data":{"toolCallId":"call_-Dbzv4Ev_Get6cFh","toolName":"bash","arguments":{"command":"rm NOTE.txt","description":"Remove the note"},"turnId":"0","model":"gpt-4.1","toolTitle":"Running command"},"id":"4878b7e1-8a8a-4708-8ffc-2dd8fdfbe3f0","timestamp":"2026-10-18T08:08:58.980Z","parentId":"c8e243d1-c712-43c6-bda4-f27d253f34eb"}"#,
        r#"{"type":"permission.requested","data":{"requestId":"b310d4e0-1e0e-4600-955d-871b5a951e13","permissionRequest":{"kind":"shell","toolCallId":"call_-Dbzv4Ev_Get6cFh","fullCommandText":"rm NOTE.txt","shellType":"bash"},"agentMode":"interactive","permissionMode":"manual"},"id":"571e3185-d040-4403-b319-738711e0bb62","timestamp":"2026-10-18T08:08:59.011Z","parentId":"4878b7e1-8a8a-4708-8ffc-2dd8fdfbe3f0"}"#,
        r#"{"type":"permission.completed","data":{"requestId":"b310d4e0-1e0e-4600-955d-871b5a951e13","toolCallId":"call_-Dbzv4Ev_Get6cFh","result":{"kind":"cancelled","reason":"Session aborted"}},"id":"f36f04ef-3c73-4b7e-88a0-f00c036e2185","timestamp":"2026-10-18T08:09:01.252Z","parentId":"571e3185-d040-4403-b319-738711e0bb62"}"#,
        r#"{"type":"assistant.turn_end","data":{"turnId":"0"},"id":"820d3dba-d109-4f6f-95d8-cb0d0067b4b6","timestamp":"2026-10-18T08:09:01.252Z","parentId":"f36f04ef-3c73-4b7e-88a0-f00c036e2185"}"#,
        r#"{"type":"abort","data":{"reason":"user_initiated"},"id":"2a182f9e-177a-43fd-a079-25e4d15f83f5","timestamp":"2026-10-18T08:09:01.253Z","parentId":"820d3dba-d109-4f6f-95d8-cb0d0067b4b6"}"#,
    ]);
    assert_the_refusal_resolves_its_wait("Esc cancel", &d);
}
