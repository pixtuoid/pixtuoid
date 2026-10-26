//! Regression for the CodeWhale subagent hook lifecycle, driving the tier's
//! recorded child-error run through the reducer (the conformance harness's
//! one-AgentId rule can't hold a two-sprite scenario).

use std::time::SystemTime;

use pixtuoid_core::source::decoder::decode_hook_payload;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::Reducer;

fn hook_events() -> Vec<AgentEvent> {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("codewhale/fixtures/child-error-hermetic/hook-payloads.jsonl"),
    )
    .iter()
    .flat_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).expect("valid hook json");
        decode_hook_payload(v).expect("CodeWhale hook payload must decode")
    })
    .collect()
}

#[test]
fn codewhale_subagent_spawn_links_child_and_complete_ends_it() {
    let events = hook_events();
    let registered = |root: bool| {
        events.iter().find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id,
                ..
            } if parent_id.is_none() == root => Some(*agent_id),
            _ => None,
        })
    };
    let parent = registered(true).expect("the session registers");
    let child = registered(false).expect("subagent_spawn registers a child");
    assert_ne!(parent, child, "the child is a sprite of its own");

    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();
    let mut completed = false;
    for ev in events {
        let child_ends = matches!(ev, AgentEvent::SessionEnd { agent_id, .. } if agent_id == child);
        r.apply(&mut scene, ev, now, Transport::Hook);
        if !child_ends {
            continue;
        }
        completed = true;
        let child_slot = scene
            .agents
            .get(&child)
            .expect("subagent_spawn must create the child sprite");
        assert_eq!(
            child_slot.parent_id,
            Some(parent),
            "subagent_spawn's session_id names the session the env-mode events key on"
        );
        assert!(
            child_slot.exiting_at.is_some(),
            "subagent_complete must mark the child exiting"
        );
        let parent_slot = scene.agents.get(&parent).expect("parent still present");
        assert!(
            parent_slot.exiting_at.is_none(),
            "the parent must keep running after the subagent completes"
        );
    }
    assert!(completed, "the recorded run must complete its subagent");
}
