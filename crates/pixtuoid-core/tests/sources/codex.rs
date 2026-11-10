//! The Codex subagent lifecycle, recorded hermetically (the codex profile's
//! Subagent scenario): the root's `spawn_agent` starts a child, the child's own
//! hooks name it in `agent_id`, and its SubagentStop ends it. Its rollout is
//! flat, so the hooks and the rollout head's `parent_thread_id` carry the
//! parent link. The hooks carry no item, only the calls each one has pending.

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, Driven};
use pixtuoid_core::source::AgentEvent;
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

    let pending = |id: AgentId| -> Vec<&str> {
        d.events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolUsePending {
                    agent_id,
                    tool_name,
                    ..
                } if *agent_id == id => tool_name.as_deref(),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        pending(parent),
        ["spawn_agent"],
        "the root's one call is its spawn"
    );
    assert_eq!(pending(child).len(), 1, "the child's shell call is its own");
    assert!(
        !d.events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolUseStart { .. })),
        "the rollout owns the items"
    );
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

// A SubagentStop decoded before its SubagentStart tombstones the unknown child
// id: Codex has no further end signal of any kind, so a phantom slot registered
// by the late Start would ride the stale sweeps forever (#242).
#[test]
fn codex_reordered_subagent_stop_before_start_does_not_mint_a_phantom() {
    let (parent, child) = ids();
    let mut lines = hook_lines();
    // Short of the root's own end, which the capture closes with.
    lines.truncate(line_of(&lines, "SessionEnd", false));
    let stop = lines.remove(line_of(&lines, "SubagentStop", true));
    lines.insert(0, stop);
    let d = drive(&lines);

    assert!(
        !d.scene.agents.contains_key(&child),
        "a SubagentStart reordered after its own Stop must not register"
    );
    let parent_slot = d.scene.agents.get(&parent).expect("parent registered");
    assert!(
        parent_slot.exiting_at.is_none(),
        "the child's tombstone must not affect the parent"
    );
}

/// The child's rollout names its parent and cwd only in its head, which the
/// watcher's first sight reads: the seed carries both.
#[test]
fn a_seeded_child_rollout_starts_under_the_parent_its_head_names() {
    let (parent, child) = ids();
    let dir = super::captures::sources_root().join("codex/fixtures/subagent-hermetic");
    let lines = super::captures::transcripts_in(&dir)
        .into_iter()
        .map(|t| (super::captures::fixture_lines(&t), t))
        .find(|(lines, _)| parsed(&lines[0])["payload"]["parent_thread_id"].is_string())
        .expect("the child's rollout");
    let (lines, rollout) = lines;
    let head = parsed(&lines[0]);
    let d = Drive::transcript_at("codex", &rollout)
        .expect("codex has a line decoder")
        .seeded()
        .lines(&lines);
    d.assert_clean("codex child rollout");
    let AgentEvent::SessionStart {
        agent_id,
        cwd,
        parent_id,
        ..
    } = &d.events[0]
    else {
        panic!("the seed leads: {:?}", d.events.first());
    };
    assert_eq!(*agent_id, child);
    assert_eq!(*parent_id, Some(parent), "the head's parent_thread_id");
    assert_eq!(
        cwd,
        std::path::Path::new(head["payload"]["cwd"].as_str().expect("a head cwd")),
        "the head's cwd"
    );
}
