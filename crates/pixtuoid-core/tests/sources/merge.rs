//! The two-transport merge: each recorded run below, its hook and transcript
//! lines folded in every order they can reach the reducer in
//! (`harness::merge`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use pixtuoid_core::harness::Drive;
use pixtuoid_core::harness::merge::{Merged, Step};
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::state::TurnKey;
use pixtuoid_core::{ActivityState, AgentId, ToolUse, Transport};
use serde_json::Value;

use super::captures::{fixture_lines, fixtures_root, transcript_in};

/// One recorded run's two streams.
struct Run {
    name: String,
    source: String,
    transcript: PathBuf,
    hooks: Vec<String>,
    lines: Vec<String>,
    hook_values: Vec<Value>,
    line_values: Vec<Value>,
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
        Self {
            name: format!("{source}/{scenario}"),
            source: source.to_owned(),
            transcript,
            hook_values: parsed(&hooks),
            line_values: parsed(&lines),
            hooks,
            lines,
        }
    }

    /// `check` over every order the run's lines can arrive in, folded one at a
    /// time: a run has tens of thousands.
    fn each(&self, mut check: impl FnMut(&Merged)) {
        let merges = Drive::transcript_at(&self.source, &self.transcript)
            .expect("a transcript source")
            .seeded()
            .merges(&self.hooks, &self.lines)
            .expect("its transcript carries a write stamp");
        let mut orders = 0;
        for m in merges {
            m.driven.assert_clean(&self.name);
            check(&m);
            orders += 1;
        }
        assert!(orders > 0, "{}: no order to merge in", self.name);
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
/// the one `PreToolUse` before it of its turn and tool. The strip keeps no
/// input, so turn and tool name are the whole join, and an ambiguous one fails.
struct Request {
    line: usize,
    turn: String,
    call: String,
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
            let tool = text(raise, "/tool_name");
            let pre_uses: Vec<&Value> = hooks[..line]
                .iter()
                .filter(|v| {
                    event(v) == "PreToolUse"
                        && text(v, "/turn_id") == Some(turn)
                        && text(v, "/tool_name") == tool
                })
                .collect();
            let [pre_use] = pre_uses[..] else {
                panic!(
                    "{}: the request at hook line {line} joins {} PreToolUses of {tool:?} in its turn",
                    run.name,
                    pre_uses.len()
                );
            };
            Request {
                line,
                turn: turn.to_owned(),
                call: text(pre_use, "/tool_use_id")
                    .expect("a PreToolUse names its call")
                    .to_owned(),
            }
        })
        .collect()
}

/// Whether `step`'s line is one the spec resolves `req` by: its call's end
/// on either channel, its turn's Stop or end, the session's, or another
/// turn's request, which replaces it.
fn resolves(run: &Run, req: &Request, step: &Step) -> bool {
    let v = run.line(step);
    let call = req.call.as_str();
    match step.transport {
        Transport::Hook => match text(v, "/hook_event_name") {
            Some("PostToolUse") => text(v, "/tool_use_id") == Some(call),
            Some("Stop") => text(v, "/turn_id") == Some(req.turn.as_str()),
            Some("PermissionRequest") => text(v, "/turn_id") != Some(req.turn.as_str()),
            Some("SessionEnd") => true,
            _ => false,
        },
        _ => match text(v, "/payload/type") {
            Some("function_call_output" | "custom_tool_call_output") => {
                text(v, "/payload/call_id") == Some(call)
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
    run.each(|m| {
        for req in &requests {
            let steps = &m.steps;
            let raised = steps
                .iter()
                .position(|s| {
                    s.transport == Transport::Hook
                        && s.line == req.line
                        && matches!(s.event, AgentEvent::UserActionRequest { .. })
                })
                .expect("the request's line raised it");
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
    });
}

/// A top-level request is resolved once its call's rollout output applied,
/// in every order, unless a later request already stands in its place.
fn a_top_level_request_resolves_by_its_calls_output(run: &Run) {
    let requests = codex_requests(run);
    run.each(|m| {
        let raised = |line: usize| {
            m.steps.iter().position(|s| {
                s.transport == Transport::Hook
                    && s.line == line
                    && matches!(s.event, AgentEvent::UserActionRequest { .. })
            })
        };
        for req in &requests {
            let call = req.call.as_str();
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
            let replaced = requests
                .iter()
                .filter(|later| later.line > req.line)
                .filter_map(|later| raised(later.line))
                .any(|at| at < last);
            if replaced {
                continue;
            }
            assert!(
                !requires_action(&m.steps[last]),
                "{}: the request of hook line {} still stood once {call}'s output applied",
                run.name,
                req.line
            );
        }
    });
}

/// An item a Hook start set survives a Jsonl turn end that lags behind it,
/// in every order.
fn a_hook_set_item_survives_a_lagging_jsonl_turn_end(run: &Run) {
    let mut lagged = false;
    run.each(|m| {
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
                    if let Some(ActivityState::Running { item: Some(ToolUse {
                        tool_use_id: Some(id),
                        ..
                    }) }) = before
                        && hook_set.contains(&**id)
                    {
                        lagged = true;
                        assert!(
                            matches!(&s.state, Some(ActivityState::Running { item: Some(ToolUse { tool_use_id: Some(after), .. }) }) if after == id),
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
    });
    assert!(
        lagged,
        "{}: no order lags a Jsonl turn end behind a Hook-set item",
        run.name
    );
}

/// On a source that brackets its turns (codex's registry row), a turn the
/// stream delivered open shows its agent working, in every order: its start
/// arrived, and its end has not.
fn a_delivered_open_turn_is_never_idle(run: &Run) {
    let mut opened = false;
    run.each(|m| {
        let mut open: BTreeMap<AgentId, TurnKey> = BTreeMap::new();
        for s in &m.steps {
            match &s.event {
                AgentEvent::TurnStart { agent_id, turn } => {
                    open.insert(*agent_id, turn.clone());
                    opened = true;
                }
                AgentEvent::TurnEnd { agent_id, turn }
                    if turn.is_none() || open.get(agent_id) == turn.as_ref() =>
                {
                    open.remove(agent_id);
                }
                _ => {}
            }
            let agent = s.event.agent_id();
            if let (Some(turn), Some(state)) = (open.get(&agent), &s.state) {
                assert_ne!(
                    *state,
                    ActivityState::Idle,
                    "{}: turn {turn:?} was delivered open, yet {:?} line {} ({:?}) left its agent Idle",
                    run.name,
                    s.transport,
                    s.line,
                    s.event
                );
            }
        }
    });
    assert!(opened, "{}: no order opens a turn", run.name);
}

#[test]
fn a_codex_turn_delivered_open_shows_its_agent_working_through_its_items() {
    a_delivered_open_turn_is_never_idle(&Run::of("codex", "tool-run-hermetic"));
}

#[test]
fn a_codex_turn_cancelled_or_failed_shows_its_agent_working_until_its_end() {
    for scenario in ["cancel-hermetic", "model-error-hermetic"] {
        a_delivered_open_turn_is_never_idle(&Run::of("codex", scenario));
    }
}

#[test]
fn a_codex_turn_a_stop_hook_continues_shows_its_agent_working() {
    a_delivered_open_turn_is_never_idle(&Run::of("codex", "stop-hook-hermetic"));
}

#[test]
fn a_codex_code_mode_turn_shows_its_agent_working_around_its_cell() {
    a_delivered_open_turn_is_never_idle(&Run::of("codex", "code-mode-failure-hermetic"));
}

#[test]
fn a_codex_request_refused_in_the_tui_stands_until_its_resolution() {
    let run = Run::of("codex", "permission-hermetic");
    a_request_stands_until_its_resolution(&run);
    a_top_level_request_resolves_by_its_calls_output(&run);
    a_delivered_open_turn_is_never_idle(&run);
}

/// A refused turn's end can lag past the next turn's raise: it answers its
/// own turn's request, never the next one's.
#[test]
fn a_refused_turn_s_lagging_end_leaves_the_next_turn_s_request_standing() {
    let run = Run::of("codex", "refuse-then-prompt-hermetic");
    a_request_stands_until_its_resolution(&run);
    a_top_level_request_resolves_by_its_calls_output(&run);
    a_delivered_open_turn_is_never_idle(&run);
}

#[test]
fn a_codex_request_approved_in_the_tui_stands_until_its_resolution() {
    let run = Run::of("codex", "approve-hermetic");
    a_request_stands_until_its_resolution(&run);
    a_top_level_request_resolves_by_its_calls_output(&run);
    a_delivered_open_turn_is_never_idle(&run);
}

/// A policy hook's deny fires no PostToolUse: the call's rollout output alone
/// resolves its request.
#[test]
fn a_codex_request_a_policy_hook_denied_resolves_by_its_calls_output() {
    let run = Run::of("codex", "policy-deny-hermetic");
    a_request_stands_until_its_resolution(&run);
    a_top_level_request_resolves_by_its_calls_output(&run);
}

/// A nested call has no rollout output: its request stands past its cell's
/// own end, until its own PostToolUse or its turn's end.
#[test]
fn a_code_mode_cell_s_gated_call_stands_until_its_resolution() {
    for scenario in ["code-mode-gate-hermetic", "code-mode-approve-hermetic"] {
        a_request_stands_until_its_resolution(&Run::of("codex", scenario));
    }
}

#[test]
fn grok_s_hook_set_item_survives_its_last_turn_s_lagging_end() {
    a_hook_set_item_survives_a_lagging_jsonl_turn_end(&Run::of("grok", "two-turn-hermetic"));
}
