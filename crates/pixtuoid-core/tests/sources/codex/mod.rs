//! The Codex subagent lifecycle, recorded hermetically (the codex profile's
//! Subagent scenario): the root's `spawn_agent` starts a child, the child's own
//! hooks name it in `agent_id`, and its SubagentStop ends it. Its rollout is
//! flat, so the hooks and the rollout head's `parent_thread_id` carry the
//! parent link.

use std::path::PathBuf;
use std::time::SystemTime;

use pixtuoid_core::AgentId;
use pixtuoid_core::source::decoder::decode_hook_payload;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::Reducer;

const PARENT: &str = "01a14e51-5f02-7a71-aed3-a441edeac91d";
const CHILD: &str = "01a14e51-6b67-7d71-bf09-674db3be3c5e";

fn captured_hook_events() -> Vec<AgentEvent> {
    super::captures::fixture_lines(
        &super::captures::sources_root().join("codex/fixtures/hook-payloads.jsonl"),
    )
    .iter()
    .flat_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).expect("valid hook json");
        decode_hook_payload(v).expect("captured Codex hook payload must decode")
    })
    .collect()
}

#[test]
fn a_codex_child_works_under_its_root_and_its_stop_ends_it_alone() {
    let parent = AgentId::from_parts("codex", PARENT);
    let child = AgentId::from_parts("codex", CHILD);
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();

    // Up to the root's own end, which the capture closes with.
    let events = captured_hook_events();
    let root_ends = events
        .iter()
        .position(|e| matches!(e, AgentEvent::SessionEnd { agent_id, .. } if *agent_id == parent))
        .expect("the root's SessionEnd");
    for ev in &events[..root_ends] {
        r.apply(&mut scene, ev.clone(), now, Transport::Hook);
    }

    let tools = |id: AgentId| -> Vec<bool> {
        events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ActivityStart {
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
    let child_slot = scene
        .agents
        .get(&child)
        .expect("SubagentStart must create the subagent sprite");
    assert_eq!(child_slot.parent_id, Some(parent));
    assert!(
        child_slot.exiting_at.is_some(),
        "SubagentStop must mark the subagent exiting"
    );
    let parent_slot = scene.agents.get(&parent).expect("parent still present");
    assert!(
        parent_slot.exiting_at.is_none(),
        "parent must keep running after the subagent stops"
    );
}

/// `spawn_agent` returns at launch, and a hook delivery rides its own shim
/// process, so the child's SubagentStart may land before that return: the
/// return must not walk the running child out.
#[test]
fn a_spawn_returning_after_its_child_started_leaves_the_child_running() {
    let parent = AgentId::from_parts("codex", PARENT);
    let child = AgentId::from_parts("codex", CHILD);
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();

    let mut events = captured_hook_events();
    let returned = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ActivityEnd { agent_id, .. } if *agent_id == parent))
        .expect("the spawn's return");
    let started = events
        .iter()
        .position(|e| matches!(e, AgentEvent::SessionStart { agent_id, .. } if *agent_id == child))
        .expect("the child's SubagentStart");
    let start = events.remove(started);
    events.insert(returned, start);
    let child_stops = events
        .iter()
        .position(|e| matches!(e, AgentEvent::SessionEnd { agent_id, .. } if *agent_id == child))
        .expect("the child's SubagentStop");
    for ev in &events[..child_stops] {
        r.apply(&mut scene, ev.clone(), now, Transport::Hook);
    }
    r.tick(
        &mut scene,
        now + pixtuoid_core::state::reducer::B1_CASCADE_GRACE + std::time::Duration::from_millis(1),
    );

    let slot = scene.agents.get(&child).expect("the child is registered");
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
    let parent = AgentId::from_parts("codex", PARENT);
    let child = AgentId::from_parts("codex", CHILD);
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

#[test]
fn codex_subagent_stop_before_start_is_a_safe_noop() {
    let parent = AgentId::from_parts("codex", PARENT);
    let child = AgentId::from_parts("codex", CHILD);
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "codex".into(),
            session_id: PARENT.into(),
            cwd: PathBuf::from("/private/tmp/pixtuoid-capture/proj"),
            parent_id: None,
        },
        now,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionEnd {
            agent_id: child,
            as_child: true,
        },
        now,
        Transport::Hook,
    );
    assert!(
        !scene.agents.contains_key(&child),
        "a SessionEnd for an absent child must not create a phantom slot"
    );
    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_none(),
        "an orphan SubagentStop must not cascade the unrelated parent"
    );
}
