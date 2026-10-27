//! Regressions for the CodeWhale hook lifecycle: the tier's recorded child-error
//! run through the reducer at its own clock (the conformance harness's
//! one-AgentId rule can't hold a two-sprite scenario), and a cold `session_idle`.

use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::Drive;
use pixtuoid_core::source::decoder::decode_hook_payload;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::{B1_CASCADE_GRACE, Reducer};

/// Each decoded event at the shim's stamp on its payload.
fn timed_events() -> Vec<(SystemTime, AgentEvent)> {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("codewhale/fixtures/child-error-hermetic/hook-payloads.jsonl"),
    )
    .iter()
    .flat_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).expect("valid hook json");
        let ms = v["_shim_ts_ms"]
            .as_u64()
            .expect("the shim stamps each payload");
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(ms);
        decode_hook_payload(v)
            .expect("CodeWhale hook payload must decode")
            .into_iter()
            .map(move |ev| (at, ev))
    })
    .collect()
}

#[test]
fn codewhale_subagent_outlives_its_dispatch_and_ends_on_its_own_complete() {
    let events = timed_events();
    let registered = |root: bool| {
        events.iter().find_map(|(_, e)| match e {
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

    // What makes the run tell the inferences apart: the dispatch returned and
    // the parent held no Task past the b1 grace while its child ran.
    let at = |pick: &dyn Fn(&AgentEvent) -> bool, after: SystemTime| {
        events
            .iter()
            .find(|(t, e)| *t >= after && pick(e))
            .map(|(t, _)| *t)
    };
    let dispatched = at(
        &|e| matches!(e, AgentEvent::ToolUseEnd { agent_id, tool_use_id: Some(_) } if *agent_id == parent),
        SystemTime::UNIX_EPOCH,
    )
    .expect("the dispatch call ends");
    let completed = at(
        &|e| matches!(e, AgentEvent::SessionEnd { agent_id, .. } if *agent_id == child),
        dispatched,
    )
    .expect("the child completes");
    let next_task = at(
        &|e| matches!(e, AgentEvent::ToolUseStart { agent_id, detail: Some(d), .. } if *agent_id == parent && d.is_task()),
        dispatched,
    )
    .unwrap_or(completed);
    let untasked = next_task.min(completed);
    assert!(
        untasked.duration_since(dispatched).expect("ordered") > B1_CASCADE_GRACE,
        "the recording no longer holds the child past the b1 grace with no Task in flight"
    );

    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let mut completed_seen = false;
    for (t, ev) in events {
        r.tick(&mut scene, t);
        let child_ends = matches!(ev, AgentEvent::SessionEnd { agent_id, .. } if agent_id == child);
        if child_ends {
            let child_slot = scene
                .agents
                .get(&child)
                .expect("subagent_spawn must create the child sprite");
            assert!(
                child_slot.exiting_at.is_none(),
                "the child walked out before its own subagent_complete"
            );
        }
        r.apply(&mut scene, ev, t, Transport::Hook);
        if !child_ends {
            continue;
        }
        completed_seen = true;
        let child_slot = &scene.agents[&child];
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
            scene.agents[&parent].exiting_at.is_none(),
            "the parent must keep running after the subagent completes"
        );
    }
    assert!(
        completed_seen,
        "the recorded run must complete its subagent"
    );
}

#[test]
fn a_cold_session_idle_registers_its_own_session_not_a_blank_slot() {
    let idle = super::captures::fixture_lines(
        &super::captures::fixtures_root().join("codewhale/permission-hermetic/hook-payloads.jsonl"),
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
