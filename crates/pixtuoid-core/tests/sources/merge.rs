//! The two-transport merge: each recorded run below, its hook and transcript
//! lines folded in every order they can reach the reducer in
//! (`harness::merge`), holds the turn axis's oracles in all of them.

use std::collections::BTreeSet;

use pixtuoid_core::harness::Drive;
use pixtuoid_core::harness::merge::{Merged, Step};
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::{ActivityState, Transport};
use serde_json::Value;

use super::captures::{fixture_lines, fixtures_root, transcript_in};

/// One recorded run's two streams, and its every merge.
struct Run {
    name: String,
    hook_values: Vec<Value>,
    line_values: Vec<Value>,
    every: Vec<Merged>,
}

impl Run {
    fn of(source: &str, scenario: &str) -> Self {
        let dir = fixtures_root().join(source).join(scenario);
        let transcript = transcript_in(&dir);
        let hooks = fixture_lines(&dir.join("hook-payloads.jsonl"));
        let lines = fixture_lines(&transcript);
        let parsed = |raw: &[String]| -> Vec<Value> {
            raw.iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).expect("a recorded line is JSON"))
                .collect()
        };
        let merges = Drive::transcript_at(source, &transcript)
            .expect("a transcript source")
            .seeded()
            .merges(&hooks, &lines)
            .expect("its transcript carries a write stamp");
        let every: Vec<Merged> = merges.collect();
        let name = format!("{source}/{scenario}");
        assert!(!every.is_empty(), "{name}: no order to merge in");
        for m in &every {
            m.driven.assert_clean(&name);
        }
        Self {
            name,
            hook_values: parsed(&hooks),
            line_values: parsed(&lines),
            every,
        }
    }

    /// The raw line a step's event decoded from.
    fn line(&self, step: &Step) -> &Value {
        match step.transport {
            Transport::Hook => &self.hook_values[step.line],
            _ => &self.line_values[step.line],
        }
    }
}

fn text<'v>(v: &'v Value, pointer: &str) -> Option<&'v str> {
    v.pointer(pointer).and_then(Value::as_str)
}

/// A codex approval request: its hook line, its turn, and the call it gates,
/// which the wire names by the same turn's `PreToolUse` of the same command.
struct Request {
    line: usize,
    turn: String,
    call: Option<String>,
}

fn codex_requests(run: &Run) -> Vec<Request> {
    let hooks = &run.hook_values;
    let event = |v: &Value| text(v, "/hook_event_name").unwrap_or_default().to_owned();
    hooks
        .iter()
        .enumerate()
        .filter(|(_, v)| event(v) == "PermissionRequest")
        .map(|(line, raise)| {
            let turn = text(raise, "/turn_id").expect("a request names its turn");
            let command = text(raise, "/tool_input/command");
            let call = hooks[..line]
                .iter()
                .rev()
                .find(|v| {
                    event(v) == "PreToolUse"
                        && text(v, "/turn_id") == Some(turn)
                        && text(v, "/tool_input/command") == command
                })
                .and_then(|v| text(v, "/tool_use_id"))
                .map(str::to_owned);
            Request {
                line,
                turn: turn.to_owned(),
                call,
            }
        })
        .collect()
}

/// Whether `step`'s line is one the spec resolves `req` by: its call's end
/// on either channel, its turn's Stop or end, or the session's.
fn resolves(run: &Run, req: &Request, step: &Step) -> bool {
    let v = run.line(step);
    let call = req.call.as_deref();
    match step.transport {
        Transport::Hook => match text(v, "/hook_event_name") {
            Some("PostToolUse") => call.is_some() && text(v, "/tool_use_id") == call,
            Some("Stop") => text(v, "/turn_id") == Some(req.turn.as_str()),
            Some("SessionEnd") => true,
            _ => false,
        },
        _ => match text(v, "/payload/type") {
            Some("function_call_output" | "custom_tool_call_output") => {
                call.is_some() && text(v, "/payload/call_id") == call
            }
            Some("task_complete" | "turn_aborted") => {
                text(v, "/payload/turn_id") == Some(req.turn.as_str())
            }
            _ => false,
        },
    }
}

fn requires_action(step: &Step) -> bool {
    matches!(step.state, Some(ActivityState::RequiresAction { .. }))
}

/// A RequiresAction stands from its raise until its resolution event, in
/// every order.
fn a_request_stands_until_its_resolution(run: &Run) {
    let requests = codex_requests(run);
    assert!(!requests.is_empty(), "{}: no request recorded", run.name);
    for m in &run.every {
        for req in &requests {
            let steps = &m.steps;
            let raised = steps
                .iter()
                .position(|s| s.transport == Transport::Hook && s.line == req.line)
                .expect("the request's line applied");
            let until = steps[raised..]
                .iter()
                .position(|s| resolves(run, req, s))
                .map_or(steps.len(), |k| raised + k);
            for s in &steps[raised..until] {
                assert!(
                    requires_action(s),
                    "{}: the request of hook line {} fell at {:?} line {} ({:?}) before \
                     its resolution",
                    run.name,
                    req.line,
                    s.transport,
                    s.line,
                    s.event
                );
            }
        }
    }
}

/// A top-level request is resolved once its call's rollout output applied,
/// in every order.
fn a_top_level_request_resolves_by_its_calls_output(run: &Run) {
    let requests = codex_requests(run);
    for m in &run.every {
        for req in &requests {
            let call = req.call.as_deref().expect("a top-level request's call");
            let output = |s: &Step| {
                s.transport == Transport::Jsonl
                    && matches!(
                        text(run.line(s), "/payload/type"),
                        Some("function_call_output" | "custom_tool_call_output")
                    )
                    && text(run.line(s), "/payload/call_id") == Some(call)
            };
            let last = m
                .steps
                .iter()
                .rposition(output)
                .unwrap_or_else(|| panic!("{}: no rollout output of {call}", run.name));
            assert!(
                !requires_action(&m.steps[last]),
                "{}: the request of hook line {} still stood once {call}'s output applied",
                run.name,
                req.line
            );
        }
    }
}

/// An item a Hook start set survives a Jsonl turn end that lags behind it,
/// in every order.
fn a_hook_set_item_survives_a_lagging_jsonl_turn_end(run: &Run) {
    let mut lagged = false;
    for m in &run.every {
        // The calls whose first start came on Hook, until any end of theirs.
        let mut started = BTreeSet::new();
        let mut hook_set = BTreeSet::new();
        let mut before: Option<&ActivityState> = None;
        for s in &m.steps {
            match &s.event {
                AgentEvent::ToolUseStart {
                    tool_use_id: Some(id),
                    ..
                } => {
                    if started.insert(id.as_str()) && s.transport == Transport::Hook {
                        hook_set.insert(id.as_str());
                    }
                }
                AgentEvent::ToolUseEnd {
                    tool_use_id: Some(id),
                    ..
                } => {
                    hook_set.remove(id.as_str());
                }
                AgentEvent::TurnEnd { .. } if s.transport == Transport::Jsonl => {
                    if let Some(ActivityState::Running {
                        tool_use_id: Some(id),
                        ..
                    }) = before
                        && hook_set.contains(&**id)
                    {
                        lagged = true;
                        assert!(
                            matches!(&s.state, Some(ActivityState::Running { tool_use_id: Some(after), .. }) if after == id),
                            "{}: a Jsonl turn end at line {} took the Hook-set item {id}",
                            run.name,
                            s.line
                        );
                    }
                }
                _ => {}
            }
            before = s.state.as_ref();
        }
    }
    assert!(
        lagged,
        "{}: no order lags a Jsonl turn end behind a Hook-set item",
        run.name
    );
}

#[test]
fn a_codex_request_denied_in_the_tui_stands_until_its_resolution() {
    let run = Run::of("codex", "permission-hermetic");
    a_request_stands_until_its_resolution(&run);
    a_top_level_request_resolves_by_its_calls_output(&run);
}

#[test]
fn grok_s_hook_set_item_survives_its_last_turn_s_lagging_end() {
    a_hook_set_item_survives_a_lagging_jsonl_turn_end(&Run::of("grok", "two-turn-hermetic"));
}
