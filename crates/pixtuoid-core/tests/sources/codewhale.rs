//! Regressions for the CodeWhale hook lifecycle: the tier's recorded child-error
//! run (the conformance harness's one-AgentId rule can't hold a two-sprite
//! scenario), and a cold `session_idle`.

use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, Fold, Reach};
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::state::{ActivityState, ToolKind, ToolUse};

#[test]
fn codewhale_subagent_outlives_its_dispatch_and_ends_on_its_own_complete() {
    let lines = super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("codewhale/fixtures/child-error-hermetic/hook-payloads.jsonl"),
    );
    let complete = lines
        .iter()
        .position(|l| l.contains(r#""event":"subagent_complete""#))
        .expect("the child completes");
    let (hooks, mut fold) = (Drive::hooks(), Fold::default());
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    fold.feed(&hooks, &lines[..complete], t0);
    let running = fold.driven();
    running.assert_clean("codewhale child-error hooks");
    let registered = |root: bool| {
        running.events.iter().find_map(|e| match e {
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
    // What makes the run tell the inferences apart: the dispatch returned, so
    // the parent held no Task through the fold's ticks past the b1 grace.
    assert!(
        running.reaches.contains(&(parent, Reach::Delegating))
            && !matches!(
                running.scene.agents[&parent].state,
                ActivityState::Running {
                    item: Some(ToolUse {
                        kind: ToolKind::Task,
                        ..
                    })
                }
            ),
        "the recording no longer returns its dispatch while the child runs"
    );
    assert!(
        running
            .scene
            .agents
            .get(&child)
            .expect("subagent_spawn must create the child sprite")
            .exiting_at
            .is_none(),
        "the child walked out before its own subagent_complete"
    );

    fold.feed(&hooks, &lines[complete..=complete], t0);
    let d = fold.finish();
    let child_slot = &d.scene.agents[&child];
    assert_eq!(
        child_slot.parent_id,
        Some(parent),
        "subagent_spawn's session_id names the session the env-mode events key on"
    );
    assert!(
        child_slot.exiting_at.is_some(),
        "subagent_complete must mark the child exiting"
    );
    assert!(
        d.scene.agents[&parent].exiting_at.is_none(),
        "the parent must keep running after the subagent completes"
    );
}

#[test]
fn a_cold_session_idle_registers_its_own_session_not_a_blank_slot() {
    let idle = super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("codewhale/fixtures/permission-hermetic/hook-payloads.jsonl"),
    )
    .into_iter()
    .find(|l| l.contains(r#""event":"session_idle""#))
    .expect("the recorded run settles idle");
    let v: serde_json::Value = serde_json::from_str(&idle).expect("valid hook json");
    let d = Drive::hooks().lines([&idle]);
    d.assert_clean("a cold codewhale session_idle");
    let id = AgentId::from_parts("codewhale", v["session_id"].as_str().expect("session_id"));
    let slot = d
        .scene
        .agents
        .get(&id)
        .expect("the idle registers its session");
    assert_eq!(&*slot.source, "codewhale");
    assert_eq!(
        &*slot.cwd,
        std::path::Path::new(v["cwd"].as_str().expect("cwd")),
        "the session's own cwd, not the unknown-cwd ghost"
    );
}
