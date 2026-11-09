//! kimi's turn end is an identity-carrying claim: no installed kimi hook names
//! the session per turn (`KIMI_EVENTS`), so a chat-only session that was running
//! before the office (or was swept) becomes a sprite on its next turn end.

use pixtuoid_core::harness::Drive;
use serde_json::Value;

/// The recorded payload `event` in `scenario`'s hook run.
fn payload(scenario: &str, event: &str) -> String {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join(format!("kimi/fixtures/{scenario}/hook-payloads.jsonl")),
    )
    .into_iter()
    .find(|l| {
        serde_json::from_str::<Value>(l).expect("a captured payload is JSON")["hook_event_name"]
            == event
    })
    .unwrap_or_else(|| panic!("{scenario} records a {event}"))
}

#[test]
fn a_turn_end_alone_registers_its_session() {
    for (scenario, event) in [
        ("tool-run-hermetic", "Stop"),
        ("model-error-hermetic", "StopFailure"),
    ] {
        let line = payload(scenario, event);
        let session: Value = serde_json::from_str(&line).expect("JSON");
        let d = Drive::hooks().lines([&line]);
        d.assert_clean(event);
        let slots: Vec<_> = d
            .scene
            .agents
            .values()
            .map(|s| (&*s.source, &*s.session_id))
            .collect();
        assert_eq!(
            slots,
            [("kimi", session["session_id"].as_str().expect("a session"))],
            "{event}"
        );
    }
}
