//! The Codex subagent lifecycle, recorded hermetically (the codex profile's
//! Subagent scenario): the root's `spawn_agent` starts a child, the child's own
//! hooks name it in `agent_id`, and its SubagentStop ends it. Its rollout is
//! flat, so the hooks and the rollout head's `parent_thread_id` carry the
//! parent link.

use std::time::SystemTime;

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, Driven};
use pixtuoid_core::source::decoder::decode_hook_payload;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::Reducer;
use serde_json::Value;

fn hook_lines() -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("codex/fixtures/subagent-hermetic/hook-payloads.jsonl"),
    )
}

fn parsed(line: &str) -> Value {
    serde_json::from_str(line).expect("valid hook json")
}

/// The root and the child the capture's SubagentStart names.
fn ids() -> (AgentId, AgentId) {
    let start = hook_lines()
        .iter()
        .map(|l| parsed(l))
        .find(|v| v["hook_event_name"] == "SubagentStart")
        .expect("a SubagentStart");
    let id = |key: &str| AgentId::from_parts("codex", start[key].as_str().expect("an id"));
    (id("session_id"), id("agent_id"))
}

/// The line of `event` the root (`by_child: false`) or the child fired.
fn line_of(lines: &[String], event: &str, by_child: bool) -> usize {
    lines
        .iter()
        .position(|l| {
            let v = parsed(l);
            v["hook_event_name"] == event
                && v["agent_id"].as_str().is_some_and(|a| !a.is_empty()) == by_child
        })
        .unwrap_or_else(|| panic!("a {event} line"))
}

fn drive(lines: &[String]) -> Driven {
    let d = Drive::hooks().lines(lines);
    d.assert_clean("codex subagent hooks");
    d
}

#[test]
fn a_codex_child_works_under_its_root_and_its_stop_ends_it_alone() {
    let (parent, child) = ids();
    let lines = hook_lines();
    // Short of the root's own end, which the capture closes with.
    let d = drive(&lines[..line_of(&lines, "SessionEnd", false)]);

    let tools = |id: AgentId| -> Vec<bool> {
        d.events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolUseStart {
                    agent_id,
                    detail: Some(d),
                    ..
                } if *agent_id == id => Some(d.is_task()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(tools(parent), [true], "the root's one call is its spawn");
    assert_eq!(tools(child), [false], "the child's shell call is its own");
    assert!(
        d.events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionStart {
            agent_id, parent_id: Some(p), .. } if *agent_id == child && *p == parent)),
        "the child starts under its root"
    );
    assert!(
        d.scene
            .agents
            .get(&child)
            .is_none_or(|s| s.exiting_at.is_some()),
        "SubagentStop must end the subagent"
    );
    let parent_slot = d.scene.agents.get(&parent).expect("parent still present");
    assert!(
        parent_slot.exiting_at.is_none(),
        "parent must keep running after the subagent stops"
    );
}

/// `spawn_agent` returns at launch, and a hook delivery rides its own shim
/// process, so the child's SubagentStart may land before that return: the
/// return must not walk the running child out. Cut at the return, so no later
/// hook of the child's revives it.
#[test]
fn a_spawn_returning_after_its_child_started_leaves_the_child_running() {
    let (_, child) = ids();
    let mut lines = hook_lines();
    let start = lines.remove(line_of(&lines, "SubagentStart", true));
    let returned = line_of(&lines, "PostToolUse", false);
    lines.insert(returned, start);
    let d = drive(&lines[..=returned + 1]);

    let slot = d.scene.agents.get(&child).expect("the child is registered");
    assert!(
        slot.exiting_at.is_none(),
        "the spawn's return walked the child out"
    );
}

fn captured_hook_events() -> Vec<AgentEvent> {
    hook_lines()
        .iter()
        .flat_map(|l| {
            decode_hook_payload(parsed(l)).expect("captured Codex hook payload must decode")
        })
        .collect()
}

// A SubagentStop decoded before its SubagentStart tombstones the unknown child
// id: Codex has no further end signal of any kind, so a phantom slot registered
// by the late Start would ride the stale sweeps forever (#242).
#[test]
fn codex_reordered_subagent_stop_before_start_does_not_mint_a_phantom() {
    let (parent, child) = ids();
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();

    let (stops, rest): (Vec<_>, Vec<_>) = captured_hook_events()
        .into_iter()
        .partition(|ev| matches!(ev, AgentEvent::SessionEnd { as_child: true, .. }));
    for ev in stops {
        r.apply(&mut scene, ev, now, Transport::Hook);
    }
    let later = now + std::time::Duration::from_millis(50);
    // Short of the root's own end, which the capture closes with.
    for ev in rest
        .into_iter()
        .filter(|ev| !matches!(ev, AgentEvent::SessionEnd { .. }))
    {
        r.apply(&mut scene, ev, later, Transport::Hook);
    }

    assert!(
        !scene.agents.contains_key(&child),
        "a SubagentStart reordered after its own Stop must not register"
    );
    let parent_slot = scene.agents.get(&parent).expect("parent registered");
    assert!(
        parent_slot.exiting_at.is_none(),
        "the child's tombstone must not affect the parent"
    );
}
