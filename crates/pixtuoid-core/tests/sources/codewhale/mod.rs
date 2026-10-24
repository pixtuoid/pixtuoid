//! Regression for the CodeWhale subagent hook lifecycle, driving a recorded
//! start→status→wait run through the reducer (the conformance harness's
//! one-AgentId rule can't hold a two-sprite scenario).

use std::time::SystemTime;

use pixtuoid_core::AgentId;
use pixtuoid_core::source::decoder::decode_hook_payload;
use pixtuoid_core::source::{AgentEvent, ToolDetail, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::Reducer;

const WORKSPACE: &str = "/private/tmp/pixtuoid-capture/proj";
const CHILD: &str = "agent_d50b1b7e";

fn hook_events() -> Vec<AgentEvent> {
    super::captures::fixture_lines(
        &super::captures::sources_root().join("codewhale/fixtures/hook-payloads.jsonl"),
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
    let parent = AgentId::from_parts("codewhale", WORKSPACE);
    let child = AgentId::from_parts("codewhale", CHILD);
    assert_ne!(
        parent, child,
        "mixed keying (cwd parent / agent_id child) must not collapse the two"
    );

    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();
    let mut completed = false;
    for ev in hook_events() {
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
            "the child links to the workspace-keyed parent"
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

/// The recorder blanks the start and wait args (no action reads as start), so
/// telling wait apart is the unit test's job.
#[test]
fn of_the_recorded_agent_calls_only_status_is_not_a_delegation() {
    let delegating: Vec<bool> = hook_events()
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ActivityStart { detail, .. } => {
                Some(matches!(detail, Some(ToolDetail::Task)))
            }
            _ => None,
        })
        .collect();
    assert_eq!(delegating, [true, false, true]);
}
