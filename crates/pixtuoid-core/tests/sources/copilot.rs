//! copilot's recorded runs: two refusals, the `-p` auto-deny, whose call then
//! ends, and Esc at an interactive prompt, which aborts the turn with no end for
//! the call, each one call that resolves its wait; a child whose request errs,
//! which its own failure ends; and the meter of a resume, a re-read and a child.

use std::collections::BTreeMap;
use std::path::PathBuf;

use pixtuoid_core::harness::{Drive, Driven, Reach};
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::state::{ActivityState, ToolUse};

fn assert_the_refusal_resolves_its_wait(what: &str, d: &Driven) {
    d.assert_clean(what);
    assert!(
        d.reached().contains(&Reach::RequiresAction),
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
        &super::captures::sources_root().join("copilot/fixtures/permission-hermetic"),
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
        !matches!(&parent.state, ActivityState::Running { item: Some(ToolUse { detail: Some(d), .. }) } if &**d == "Delegating"),
        "the parent still delegates: {:?}",
        parent.state
    );
}

/// A recording's lines, and the fresh tokens of its `session.usage_record`s by
/// the child each names in its envelope `agentId`, `None` for the root.
fn usage_records(scenario: &str) -> (PathBuf, Vec<String>, BTreeMap<Option<String>, u64>) {
    let t = super::captures::transcript_in(&super::captures::sources_root().join(scenario));
    let lines = super::captures::fixture_lines(&t);
    let mut by_agent = BTreeMap::new();
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("recorded JSON");
        if v["type"] != "session.usage_record" {
            continue;
        }
        let usage = &v["data"]["usage"];
        let count = |k: &str| usage[k].as_u64().expect("a recorded count");
        let agent = v["agentId"].as_str().map(str::to_owned);
        *by_agent.entry(agent).or_default() +=
            count("inputTokens") - count("cacheReadTokens") + count("outputTokens");
    }
    (t, lines, by_agent)
}

/// A resumed session's second `session.shutdown` totals both lifetimes, so the
/// meter is each call's own usage record.
#[test]
fn a_resumed_run_counts_each_call_once() {
    let (t, lines, records) = usage_records("copilot/fixtures/resume-hermetic");
    let shutdowns = (lines.iter())
        .filter(|l| l.contains(r#""type":"session.shutdown""#))
        .count();
    let expected: u64 = records.values().sum();
    assert!(
        shutdowns > 1 && expected > 0,
        "no resumed lifetime or no usage record here, so this recording proves nothing"
    );
    let d = Drive::transcript_at("copilot", &t)
        .expect("copilot has a line decoder")
        .seeded()
        .lines(lines);
    d.assert_clean("resume");
    let meter: u64 = d.scene.agents.values().map(|a| a.tokens_used).sum();
    assert_eq!(meter, expected);
}

/// A re-read from the top (`walk_jsonl`'s truncation reset) of a live, resumed
/// session counts no usage record twice, its earlier lifetime's included.
#[test]
fn a_rewinds_re_read_counts_no_call_twice() {
    let (t, lines, records) = usage_records("copilot/fixtures/resume-hermetic");
    let last_end = (lines.iter())
        .rposition(|l| l.contains(r#""type":"session.shutdown""#))
        .expect("a recorded shutdown");
    let live = &lines[..last_end];
    let d = Drive::transcript_at("copilot", &t)
        .expect("copilot has a line decoder")
        .seeded()
        .lines(live.iter().chain(live));
    let meter: u64 = d.scene.agents.values().map(|a| a.tokens_used).sum();
    assert_eq!(meter, records.values().sum::<u64>());
}

/// A child's usage records name it, so its spend lands on its own slot.
#[test]
fn a_childs_usage_is_its_own() {
    let (t, lines, records) = usage_records("copilot/fixtures/subagent-hermetic");
    assert!(
        records.len() > 1,
        "no child usage record here, so this recording proves nothing"
    );
    let d = Drive::transcript_at("copilot", &t)
        .expect("copilot has a line decoder")
        .seeded()
        .lines(lines);
    d.assert_clean("subagent");
    let meters: BTreeMap<Option<String>, u64> = (d.scene.agents.values())
        .map(|a| (a.parent_id.map(|_| a.session_id.to_string()), a.tokens_used))
        .collect();
    assert_eq!(meters, records);
}
