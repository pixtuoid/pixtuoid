//! reasonix's DELEGATING runs, recorded by the hermetic tier: a dispatch
//! (`spawn_agents`, or a skill tool such as `explore`) brackets nothing, and
//! its child's hooks carry a session id of its own, so each run is two sprites
//! and cannot sit under the conformance one-AgentId rule.

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::Drive;
use serde_json::Value;

/// Each recorded delegating scenario, by fixture dir, and its dispatch tool.
const DELEGATIONS: &[(&str, &str)] = &[
    ("subagent-hermetic", "spawn_agents"),
    ("explore-hermetic", "explore"),
];

#[test]
fn a_dispatchs_child_registers_under_it_and_its_return_walks_the_child_out() {
    for (scenario, tool) in DELEGATIONS {
        let mut lines = super::captures::fixture_lines(
            &super::captures::sources_root()
                .join("reasonix/fixtures")
                .join(scenario)
                .join("hook-payloads.jsonl"),
        );
        let payloads: Vec<Value> = lines
            .iter()
            .map(|l| serde_json::from_str(l).expect("a captured payload is JSON"))
            .collect();
        let dispatcher = payloads
            .iter()
            .find(|v| v["event"] == "PreToolUse" && v["toolName"] == *tool)
            .and_then(|v| v["sessionId"].as_str())
            .map(|s| AgentId::from_parts("reasonix", s))
            .unwrap_or_else(|| panic!("{scenario}: no {tool} dispatch"));
        // Up to the call's return, before the turn's own end could end the
        // child instead.
        lines.truncate(
            payloads
                .iter()
                .position(|v| v["event"] == "Stop")
                .expect("the turn ends"),
        );
        let d = Drive::hooks().lines(lines);
        d.assert_clean(scenario);
        let (child, parent) = d.linked_child().expect("the recording's child session");
        assert_eq!(parent, dispatcher, "{scenario}: the child's parent");
        assert!(
            d.scene
                .agents
                .get(&child)
                .is_none_or(|s| s.exiting_at.is_some()),
            "{scenario}: the call returned and its child stayed"
        );
    }
}
