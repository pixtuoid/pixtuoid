//! grok's DELEGATING run, captured live from a `spawn_subagent` the model was
//! asked to run BLOCKING. It lives here rather than under
//! `sources/fixtures/grok/` because a child is its own grok session with its own
//! transcript — two sprites, which the conformance harness's one-AgentId rule
//! cannot hold (the same reason `cursor/mod.rs` exists).
//!
//! Both halves of the one run are here: the parent's transcript, whose
//! `subagent_spawned` line registers the child, and the hook wire, where the
//! parent's `subagent_start` names the child and the CHILD's own `subagent_stop`
//! ends it.

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, Driven};
use pixtuoid_core::source::{AgentEvent, ToolDetail};

const PARENT: &str = "01a006a5-e63d-7543-b1af-da3127a85c3b";
const CHILD: &str = "01a006a6-0659-7690-a9cc-20da7c827b72";

fn lines(name: &str) -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("grok/fixtures/subagent-recorded")
            .join(name),
    )
}

fn hooks() -> Driven {
    let d = Drive::hooks().lines(lines("hook-payloads.jsonl"));
    d.assert_clean("grok delegation hooks");
    d
}

/// SEEDED, and via the harness rather than a hand-built `SessionStart`: the
/// parent registers from the watcher's first-sight seed keyed by grok's own
/// registry row, so this cannot drift from what production does. The logical
/// path reproduces grok's real layout because the id comes from the PARENT dir.
fn transcript() -> Driven {
    let d = Drive::transcript("grok", &format!("{PARENT}/updates.jsonl"))
        .expect("grok has a line decoder")
        .seeded()
        .lines(lines("parent-updates.jsonl"));
    d.assert_clean("grok delegation transcript");
    d
}

#[test]
fn a_blocking_spawn_is_a_task_and_the_parent_goes_delegating() {
    // Whose Task it is, asserted on the EVENT rather than on `reached` or on the
    // final slot: `reached` is a union over every live slot (`Driven::reached`)
    // and this run drives two, while the parent's final state is Idle because the
    // turn completed. The event carries the agent_id, so it answers the half this
    // name promises — that the PARENT delegated.
    let parent = AgentId::from_parts("grok", PARENT);
    let delegator = transcript().events.into_iter().find_map(|e| match e {
        AgentEvent::ToolUseStart {
            agent_id,
            detail: Some(ToolDetail::Task),
            ..
        } => Some(agent_id),
        _ => None,
    });
    assert_eq!(
        delegator,
        Some(parent),
        "the blocking spawn's Task must belong to the parent, not the child"
    );
}

#[test]
fn the_transcripts_spawn_line_registers_the_child_under_its_parent() {
    let d = transcript();
    let child = d
        .scene
        .agents
        .get(&AgentId::from_parts("grok", CHILD))
        .expect("subagent_spawned registers the child");
    assert_eq!(
        child.parent_id,
        Some(AgentId::from_parts("grok", PARENT)),
        "child links to its parent"
    );
}

#[test]
fn only_the_blocking_spawn_carries_the_task_detail() {
    let tasks = transcript()
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::ToolUseStart {
                    detail: Some(ToolDetail::Task),
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        tasks, 1,
        "exactly the one `background: false` spawn mints Task"
    );
}

#[test]
fn subagent_stop_ends_the_child_and_stamps_it_as_a_child() {
    // grok puts the CHILD's own sessionId on `subagent_stop` — the parent's
    // `session_end` is a separate event later in the same capture, so "the child
    // ended" has to be read off the child's id, not off the scene being empty.
    let ends: Vec<_> = hooks()
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SessionEnd { agent_id, as_child } => Some((*agent_id, *as_child)),
            _ => None,
        })
        .collect();
    let child = AgentId::from_parts("grok", CHILD);
    assert!(
        ends.contains(&(child, true)),
        "subagent_stop must end the CHILD with the as_child stamp the reducer's \
         child ledger keys on, got {ends:?}"
    );
}
