//! The two-transport merge: each recorded run below, its hook and transcript
//! lines folded in every order they can reach the reducer in
//! (`harness::merge`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pixtuoid_core::harness::merge::{Merged, Merges, Step};
use pixtuoid_core::harness::{Drive, Driven, HOOK_PAYLOADS};
use pixtuoid_core::source::{AgentEvent, Resolution, registry};
use pixtuoid_core::state::TurnKey;
use pixtuoid_core::{ActivityState, AgentId, ToolUse, Transport};
use serde_json::Value;

use super::captures::{Capture, every_capture, fixture_lines, sources_root, transcript_in};

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
        let dir = pixtuoid_core::harness::fixtures_dir(&sources_root(), source).join(scenario);
        let transcript = transcript_in(&dir);
        Self::of_transcript(source, &dir.join(HOOK_PAYLOADS), transcript)
    }

    /// The run of `transcript` against its capture's hook payloads at `hooks`.
    fn of_transcript(source: &str, hooks: &Path, transcript: PathBuf) -> Self {
        let hooks = fixture_lines(hooks);
        let lines = fixture_lines(&transcript);
        let parsed = |raw: &[String]| -> Vec<Value> {
            raw.iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).expect("a recorded line is JSON"))
                .collect()
        };
        Self {
            name: pixtuoid_core::harness::slashed_under(&sources_root(), &transcript)
                .expect("every capture is under tests/sources"),
            source: source.to_owned(),
            transcript,
            hook_values: parsed(&hooks),
            line_values: parsed(&lines),
            hooks,
            lines,
        }
    }

    /// Every order the run's lines can arrive in; `None` when its source's
    /// transcript carries no write stamp.
    fn merges(&self) -> Option<Merges> {
        Drive::transcript_at(&self.source, &self.transcript)
            .expect("a transcript source")
            .seeded()
            .merges(&self.hooks, &self.lines)
    }

    /// `check` over every order the run's lines can arrive in, folded one at a
    /// time, never collected, as a run's orders outgrow memory. Returns how
    /// many there were.
    fn each(&self, mut check: impl FnMut(&Merged)) -> usize {
        let merges = self.merges().expect("its transcript carries a write stamp");
        let mut orders = 0;
        for m in merges {
            m.driven.assert_clean(&self.name);
            check(&m);
            orders += 1;
        }
        assert!(orders > 0, "{}: no order to merge in", self.name);
        orders
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
    let requests = hooks
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
        .collect::<Vec<_>>();
    assert!(!requests.is_empty(), "{}: no request recorded", run.name);
    requests
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

/// A RequiresAction stands from its raise until its resolution event, in `m`'s
/// order.
fn a_request_stands_until_its_resolution(run: &Run, requests: &[Request], m: &Merged) {
    for req in requests {
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
}

/// A top-level request is resolved once its call's rollout output applied in
/// `m`'s order, unless a later request already stands in its place.
fn a_top_level_request_resolves_by_its_calls_output(run: &Run, requests: &[Request], m: &Merged) {
    let raised = |line: usize| {
        m.steps.iter().position(|s| {
            s.transport == Transport::Hook
                && s.line == line
                && matches!(s.event, AgentEvent::UserActionRequest { .. })
        })
    };
    for req in requests {
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
/// stream delivered open shows its agent working in `m`'s order: its start
/// arrived, and its end has not.
fn a_delivered_open_turn_is_never_idle(run: &Run, m: &Merged) {
    let mut open: BTreeMap<AgentId, TurnKey> = BTreeMap::new();
    for s in &m.steps {
        match &s.event {
            AgentEvent::TurnStart { agent_id, turn } => {
                open.insert(*agent_id, turn.clone());
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
}

/// Whether `m`'s order opens a turn at all.
fn opens_a_turn(m: &Merged) -> bool {
    (m.steps.iter()).any(|s| matches!(s.event, AgentEvent::TurnStart { .. }))
}

/// A turn delivered open shows its agent working in every order of `run`.
fn turns_stay_working(run: &Run) {
    let mut opened = false;
    run.each(|m| {
        a_delivered_open_turn_is_never_idle(run, m);
        opened |= opens_a_turn(m);
    });
    assert!(opened, "{}: no order opens a turn", run.name);
}

/// `run`'s requests and turns over one enumeration of its orders, each order
/// folded once for all three checks.
fn requests_and_turns_hold(run: &Run) {
    let requests = codex_requests(run);
    let mut opened = false;
    run.each(|m| {
        a_request_stands_until_its_resolution(run, &requests, m);
        a_top_level_request_resolves_by_its_calls_output(run, &requests, m);
        a_delivered_open_turn_is_never_idle(run, m);
        opened |= opens_a_turn(m);
    });
    assert!(opened, "{}: no order opens a turn", run.name);
}

#[test]
fn a_codex_turn_delivered_open_shows_its_agent_working_through_its_items() {
    turns_stay_working(&Run::of("codex", "tool-run-hermetic"));
}

#[test]
fn a_codex_turn_cancelled_or_failed_shows_its_agent_working_until_its_end() {
    for scenario in ["cancel-hermetic", "model-error-hermetic"] {
        turns_stay_working(&Run::of("codex", scenario));
    }
}

#[test]
fn a_codex_turn_a_stop_hook_continues_shows_its_agent_working() {
    turns_stay_working(&Run::of("codex", "stop-hook-hermetic"));
}

#[test]
fn a_codex_code_mode_turn_shows_its_agent_working_around_its_cell() {
    turns_stay_working(&Run::of("codex", "code-mode-failure-hermetic"));
}

#[test]
fn a_codex_request_refused_in_the_tui_stands_until_its_resolution() {
    requests_and_turns_hold(&Run::of("codex", "permission-hermetic"));
}

/// A refused turn's end can lag past the next turn's raise: it answers its
/// own turn's request, never the next one's.
#[test]
fn a_refused_turn_s_lagging_end_leaves_the_next_turn_s_request_standing() {
    requests_and_turns_hold(&Run::of("codex", "refuse-then-prompt-hermetic"));
}

#[test]
fn a_codex_request_approved_in_the_tui_stands_until_its_resolution() {
    requests_and_turns_hold(&Run::of("codex", "approve-hermetic"));
}

/// A policy hook's deny fires no PostToolUse: the call's rollout output alone
/// resolves its request.
#[test]
fn a_codex_request_a_policy_hook_denied_resolves_by_its_calls_output() {
    let run = Run::of("codex", "policy-deny-hermetic");
    let requests = codex_requests(&run);
    run.each(|m| {
        a_request_stands_until_its_resolution(&run, &requests, m);
        a_top_level_request_resolves_by_its_calls_output(&run, &requests, m);
    });
}

/// A nested call has no rollout output: its request stands past its cell's
/// own end, until its own PostToolUse or its turn's end.
#[test]
fn a_code_mode_cell_s_gated_call_stands_until_its_resolution() {
    for scenario in ["code-mode-gate-hermetic", "code-mode-approve-hermetic"] {
        let run = Run::of("codex", scenario);
        let requests = codex_requests(&run);
        run.each(|m| a_request_stands_until_its_resolution(&run, &requests, m));
    }
}

#[test]
fn grok_s_hook_set_item_survives_its_last_turn_s_lagging_end() {
    a_hook_set_item_survives_a_lagging_jsonl_turn_end(&Run::of("grok", "two-turn-hermetic"));
}

/// What an order leaves of a slot that the order must not decide: its
/// presence, whether it is exiting, its parent, its activity with its item's
/// call id and tool kind, its tool-call count, its tokens, its model, its
/// effort level and whether ultracode is on. Its presence
/// and `exiting` still read the clock: the fold's final tick is the run's
/// last arrival plus `harness::CASCADE_TICK`, and a slot goes once its exit is
/// `EXIT_GRACE_WINDOW` old, so a slot whose exit can start on either side of
/// `EXIT_GRACE_WINDOW` before that tick can be gone in one order and still
/// exiting in another, whatever the merge does.
#[derive(Debug, PartialEq)]
struct Final {
    exiting: bool,
    parent_id: Option<AgentId>,
    state: ActivityState,
    tool_call_count: u32,
    tokens_used: u64,
    model: Option<Arc<str>>,
    effort: Option<Arc<str>>,
    ultracode: bool,
}

/// Each slot `m` left, by agent: a slot only some orders leave differs too.
fn finals(m: &Merged) -> BTreeMap<AgentId, Final> {
    let slots = m.driven.scene.agents.iter();
    slots
        .map(|(&agent, slot)| {
            let state = match &slot.state {
                ActivityState::Running { item } => ActivityState::Running {
                    item: item.clone().map(|i| ToolUse { detail: None, ..i }),
                },
                ActivityState::RequiresAction { .. } => {
                    ActivityState::RequiresAction { reason: None }
                }
                ActivityState::Idle => ActivityState::Idle,
            };
            let last = Final {
                exiting: slot.exiting_at.is_some(),
                parent_id: slot.parent_id,
                state,
                tool_call_count: slot.tool_call_count,
                tokens_used: slot.tokens_used,
                model: slot.model.clone(),
                // Its `seen_at` is the instant an order applied it.
                effort: slot.effort.as_ref().map(|e| Arc::clone(&e.value)),
                ultracode: slot.ultracode,
            };
            (agent, last)
        })
        .collect()
}

/// The registry rows with both a transcript and hooks: a CLI that gains the
/// second transport joins here, and fails until its transcript is stamped.
fn two_transport_sources() -> Vec<&'static str> {
    registry::registered_source_names()
        .filter(|&s| {
            registry::descriptor_for(s)
                .is_some_and(|d| d.line_decoder().is_some() && d.hook().is_some())
        })
        .collect()
}

/// `source`'s captures that recorded its hooks too, each with its hook payloads.
fn two_stream_captures(source: &str) -> Vec<(Capture, PathBuf)> {
    (every_capture().into_iter())
        .filter(|c| c.source == source)
        .filter_map(|c| c.hook_payloads().map(|hooks| (c, hooks)))
        .collect()
}

/// `source`'s recorded runs: each transcript of each capture that recorded its
/// hooks too.
fn runs_of(source: &str) -> Vec<Run> {
    (two_stream_captures(source).into_iter())
        .flat_map(|(c, hooks)| {
            let transcripts = c.transcripts().into_iter();
            transcripts.map(move |t| Run::of_transcript(source, &hooks, t))
        })
        .collect()
}

/// A fact no key merges across transports, so with two producers their
/// arrival order would decide the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Unkeyed {
    Model,
    /// A hook's model, which fills only an unset one: one producer of it, apart
    /// from the transcript's.
    ModelHint,
    Effort,
    Ultracode,
    Label,
    /// A start supersedes the open turn whatever its key, so starts are
    /// ordered across turns.
    TurnStart,
    TurnEnd,
    /// A wait naming no call: a tool's name binds every call pending under it.
    Wait,
    Tool,
    Usage,
    /// A variant newer than this match, held to one producer until sorted.
    Unsorted,
}

/// Each unkeyed fact `event` states.
fn unkeyed(event: &AgentEvent) -> Vec<Unkeyed> {
    match event {
        AgentEvent::ModelInfo { model, effort, .. } => [
            model.as_ref().map(|_| Unkeyed::Model),
            effort.as_ref().map(|_| Unkeyed::Effort),
        ]
        .into_iter()
        .flatten()
        .collect(),
        AgentEvent::ModelHint { .. } => vec![Unkeyed::ModelHint],
        AgentEvent::Ultracode { .. } => vec![Unkeyed::Ultracode],
        AgentEvent::Rename { .. } | AgentEvent::LabelHint { .. } => vec![Unkeyed::Label],
        AgentEvent::TurnStart { .. } => vec![Unkeyed::TurnStart],
        AgentEvent::TurnEnd { turn: None, .. } => vec![Unkeyed::TurnEnd],
        AgentEvent::UserActionRequest {
            tool_use_id: None, ..
        } => vec![Unkeyed::Wait],
        AgentEvent::ToolUseStart {
            tool_use_id: None, ..
        }
        | AgentEvent::ToolUseEnd {
            tool_use_id: None, ..
        } => vec![Unkeyed::Tool],
        AgentEvent::Usage {
            reading_id: None, ..
        } => vec![Unkeyed::Usage],
        // Keyed: a call, a turn or a reading names the record both merge on.
        AgentEvent::ToolUseStart { .. }
        | AgentEvent::ToolUseEnd { .. }
        | AgentEvent::ToolUsePending { .. }
        | AgentEvent::TurnEnd { .. }
        | AgentEvent::UserActionRequest { .. }
        | AgentEvent::UserActionResolved {
            by: Resolution::Approved(_) | Resolution::Turn(_),
            ..
        }
        | AgentEvent::Usage { .. }
        // Lifecycle: `Reducer::authorizes` rules on its transport.
        | AgentEvent::SessionStart { .. }
        | AgentEvent::SessionEnd { .. }
        | AgentEvent::Identity { .. }
        // The watcher's own: no decoder emits them.
        | AgentEvent::ProofOfLife { .. }
        | AgentEvent::Backlog { .. } => vec![],
        _ => vec![Unkeyed::Unsorted],
    }
}

/// Each unkeyed fact `driven` decoded, with its agent.
fn unkeyed_facts(driven: &Driven, what: &str) -> BTreeSet<(AgentId, Unkeyed)> {
    driven.assert_clean(what);
    let events = driven.events.iter();
    events
        .flat_map(|e| unkeyed(e).into_iter().map(|f| (e.agent_id(), f)))
        .collect()
}

/// Every recorded capture of a two-transport source decodes each unkeyed fact
/// of an agent from one transport alone, and some capture decodes one from
/// each.
#[test]
fn every_two_transport_capture_decodes_each_unkeyed_fact_from_one_transport() {
    let (mut both, mut hooked, mut transcribed) = (Vec::new(), false, false);
    for source in two_transport_sources() {
        let captures = two_stream_captures(source);
        assert!(
            !captures.is_empty(),
            "{source}: no capture records both streams"
        );
        for (c, hooks) in captures {
            let name = c.label();
            let hooks = unkeyed_facts(&Drive::hooks().lines(fixture_lines(&hooks)), &name);
            let transcripts: BTreeSet<_> = (c.transcripts().iter())
                .flat_map(|t| {
                    let drive = Drive::transcript_at(source, t).expect("a transcript source");
                    unkeyed_facts(&drive.lines(fixture_lines(t)), &name)
                })
                .collect();
            hooked |= !hooks.is_empty();
            transcribed |= !transcripts.is_empty();
            both.extend(
                (hooks.intersection(&transcripts))
                    .map(|(agent, fact)| format!("{name}: {fact:?} of {agent:?}")),
            );
        }
    }
    assert!(
        both.is_empty(),
        "decoded from both transports:\n{}",
        both.join("\n")
    );
    assert!(hooked && transcribed, "a transport decodes no unkeyed fact");
}

/// Asserts that no step of `m`'s order after an end of a call shows that
/// call or raises a wait from a gate naming it, and returns whether a fact of
/// a call applied after its end.
fn assert_ended_calls_stay_ended(run: &Run, m: &Merged) -> bool {
    let mut ended: BTreeSet<(AgentId, &str)> = BTreeSet::new();
    let mut before: BTreeMap<AgentId, Option<&ActivityState>> = BTreeMap::new();
    let mut echoed = false;
    let raised =
        |s: Option<&ActivityState>| matches!(s, Some(ActivityState::RequiresAction { .. }));
    for s in &m.steps {
        let agent = s.event.agent_id();
        let was = before.insert(agent, s.state.as_ref()).flatten();
        match &s.event {
            AgentEvent::ToolUseStart {
                tool_use_id: Some(call),
                ..
            } => echoed |= ended.contains(&(agent, call.as_str())),
            AgentEvent::ToolUseEnd {
                tool_use_id: Some(call),
                ..
            } => {
                ended.insert((agent, call.as_str()));
            }
            AgentEvent::UserActionRequest {
                tool_use_id: Some(call),
                ..
            } if ended.contains(&(agent, call.as_str())) => {
                echoed = true;
                assert!(
                    raised(was) || !raised(s.state.as_ref()),
                    "{}: {:?} line {} raised a wait from a gate of {call}, which ended",
                    run.name,
                    s.transport,
                    s.line
                );
            }
            _ => {}
        }
        if let Some(ActivityState::Running {
            item:
                Some(ToolUse {
                    tool_use_id: Some(shown),
                    ..
                }),
        }) = &s.state
        {
            assert!(
                !ended.contains(&(agent, &**shown)),
                "{}: {:?} line {} ({:?}) shows {shown}, which ended",
                run.name,
                s.transport,
                s.line,
                s.event
            );
        }
    }
    echoed
}

/// Every order of every recorded run of a two-transport source ends alike, and
/// shows no call again once an end of it applied; some order applies a fact of
/// an ended call after its end. A source with no write stamp proves nothing, nor
/// does a run whose lines admit one order. One walk folds each order once for
/// both checks.
#[test]
fn every_order_of_a_two_transport_run_ends_alike() {
    let mut unstamped = Vec::new();
    let mut echoed = false;
    for source in two_transport_sources() {
        let runs = runs_of(source);
        assert!(
            !runs.is_empty(),
            "{source}: no capture records both streams"
        );
        if runs[0].merges().is_none() {
            unstamped.push(source);
            continue;
        }
        for run in &runs {
            let mut first = None;
            let orders = run.each(|m| {
                echoed |= assert_ended_calls_stay_ended(run, m);
                match &first {
                    None => first = Some((m.arrivals(), finals(m))),
                    Some((order, first_finals)) => assert_eq!(
                        finals(m),
                        *first_finals,
                        "{}: {:?} leaves its slots otherwise than {order:?}",
                        run.name,
                        m.arrivals()
                    ),
                }
            });
            assert!(orders >= 2, "{}: one order", run.name);
        }
    }
    assert!(
        unstamped.is_empty(),
        "no write stamp (`harness::reads::WRITE_STAMPS`) for {unstamped:?}"
    );
    assert!(echoed, "no order applies a fact of a call after its end");
}
