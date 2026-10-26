use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::decoder::decode_hook_payload;
use pixtuoid_core::source::{AgentEvent, ToolDetail, Transport};
use pixtuoid_core::state::reducer::{
    ACTIVE_GRACE_WINDOW, B1_CASCADE_GRACE, HOOK_WINS_WINDOW, Reducer,
};
use pixtuoid_core::state::{ActivityState, SceneState};
use serde_json::json;

use crate::{
    DRAIN_INFERRED, HOOK_AND_TRANSCRIPT, UNNAMED_CHILD_HOOKS, act_end, act_start, delegating_pair,
    delegating_pair_on, start, start_as, turn_end, waiting, waiting_for,
};

#[track_caller]
fn assert_delegating(scene: &SceneState, id: AgentId, msg: &str) {
    match &scene.agents.get(&id).unwrap().state {
        ActivityState::Active { detail, .. } => {
            assert_eq!(detail.as_deref(), Some("Delegating"), "{msg}");
        }
        other => panic!("expected Active(Delegating), got {other:?} — {msg}"),
    }
}

#[test]
fn hook_activity_during_active_task_is_suppressed() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "parent");
    start_as(&mut r, &mut scene, parent, UNNAMED_CHILD_HOOKS);

    let t0 = SystemTime::now();

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("subagent-R"),
        Some("Read: /foo"),
        t0 + Duration::from_millis(50),
        Transport::Hook,
    );

    let slot = scene.agents.get(&parent).unwrap();
    match &slot.state {
        ActivityState::Active { detail, .. } => {
            assert_eq!(detail.as_deref(), Some("Delegating"));
        }
        other => panic!("expected Active(Delegating), got {other:?}"),
    }

    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("subagent-R"),
        t0 + Duration::from_millis(60),
        Transport::Hook,
    );
    let slot = scene.agents.get(&parent).unwrap();
    assert!(
        matches!(slot.state, ActivityState::Active { .. }),
        "parent must remain Active(Task) while task in flight"
    );
    assert!(
        slot.pending_idle_at.is_none(),
        "a suppressed subagent End must not arm the parent's pending-idle"
    );

    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t0 + Duration::from_millis(200),
        Transport::Hook,
    );
    let slot = scene.agents.get(&parent).unwrap();
    assert!(matches!(slot.state, ActivityState::Active { .. }));
    assert!(slot.pending_idle_at.is_some());
    r.tick(&mut scene, t0 + Duration::from_millis(2000));
    assert_eq!(
        scene.agents.get(&parent).unwrap().state,
        ActivityState::Idle
    );
}

/// The turn end is the child's, so it must not answer the parent's own prompt.
#[test]
fn a_hook_turn_end_during_a_task_is_suppressed() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "parent");
    start_as(&mut r, &mut scene, parent, UNNAMED_CHILD_HOOKS);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );
    waiting_for(
        &mut r,
        &mut scene,
        parent,
        "permission",
        Some("own-1"),
        t0 + Duration::from_millis(50),
        Transport::Hook,
    );
    let end = t0 + Duration::from_millis(100);
    turn_end(&mut r, &mut scene, parent, end, Transport::Hook);
    r.tick(
        &mut scene,
        end + ACTIVE_GRACE_WINDOW + Duration::from_millis(10),
    );
    assert!(
        matches!(scene.agents[&parent].state, ActivityState::Waiting { .. }),
        "got {:?}",
        scene.agents[&parent].state
    );
}

#[test]
fn active_tasks_drained_by_jsonl_end_even_if_hook_end_arrived_first() {
    use pixtuoid_core::source::ToolDetail;

    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);

    let t0 = SystemTime::now();

    act_end(&mut r, &mut scene, id, Some("task-X"), t0, Transport::Hook);

    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: id,
            tool_use_id: Some("task-X".into()),
            detail: Some(ToolDetail::Task),
        },
        t0 + Duration::from_millis(700),
        Transport::Jsonl,
    );

    act_end(
        &mut r,
        &mut scene,
        id,
        Some("task-X"),
        t0 + Duration::from_millis(800),
        Transport::Jsonl,
    );

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("other"),
        Some("Bash: ls"),
        t0 + Duration::from_millis(900),
        Transport::Hook,
    );

    match &scene.agents.get(&id).unwrap().state {
        ActivityState::Active { detail, .. } => {
            assert_eq!(
                detail.as_deref(),
                Some("Bash: ls"),
                "active_tasks must drain so subsequent hook events apply"
            );
        }
        other => panic!("expected Active(Bash: ls), got {other:?}"),
    }
}

#[test]
fn jsonl_event_after_dedup_window_is_applied() {
    let mut scene = SceneState::uniform(2);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);

    let t0 = SystemTime::now();
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t-1"),
        Some("hook-side"),
        t0,
        Transport::Hook,
    );

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t-1"),
        Some("jsonl-side"),
        t0 + Duration::from_millis(600),
        Transport::Jsonl,
    );

    let slot = scene.agents.get(&id).unwrap();
    match &slot.state {
        ActivityState::Active { detail, .. } => assert_eq!(
            detail.as_deref(),
            Some("jsonl-side"),
            "JSONL event outside the dedup window must be applied"
        ),
        other => panic!("expected Active, got {other:?}"),
    }
}

#[test]
fn hook_wins_dedup_drops_jsonl_duplicate_within_window() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/dedup-hw.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);

    let t0 = SystemTime::now();

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("dedup-1"),
        Some("Edit: hook.rs"),
        t0,
        Transport::Hook,
    );
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 1);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("dedup-1"),
        Some("Edit: jsonl.rs"),
        t0 + Duration::from_millis(200),
        Transport::Jsonl,
    );

    assert_eq!(
        scene.agents.get(&id).unwrap().tool_call_count,
        1,
        "JSONL duplicate inside hook-wins window must be dropped"
    );
    match &scene.agents.get(&id).unwrap().state {
        ActivityState::Active { detail, .. } => {
            assert_eq!(detail.as_deref(), Some("Edit: hook.rs"));
        }
        other => panic!("expected Active from hook, got {other:?}"),
    }
}

#[test]
fn subagent_is_removed_promptly_when_its_parent_task_completes() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "orch", t0);
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        child,
        Some("c1"),
        Some("Read: /x"),
        t0 + Duration::from_secs(2),
        Transport::Jsonl,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t0 + Duration::from_secs(10),
        Transport::Hook,
    );

    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "the b1 cascade is grace-deferred (#151) — never immediate"
    );
    r.tick(
        &mut scene,
        t0 + Duration::from_secs(10) + B1_CASCADE_GRACE + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "a completed subagent must leave promptly (within the b1 grace) when its parent's Task drains, not linger to the 30-min idle sweep"
    );
    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_none(),
        "the parent keeps running after a Task completes"
    );
}

/// An id-less Task end walks a child out only under `AnyTaskEnd`: a reasonix
/// skill call's child fires no end of its own, while codex's `spawn_agent`
/// returns at launch and its child ends itself.
#[test]
fn an_untracked_task_end_walks_out_a_child_only_under_any_task_end() {
    for (source, parent_tool, walks_out) in [
        ("reasonix", "Agent", true),
        ("codex", "Agent", false),
        ("reasonix", "Bash: ls", false),
    ] {
        let mut scene = SceneState::uniform(8);
        let mut r = Reducer::new();
        let parent = AgentId::from_parts(source, "root");
        let child = AgentId::from_parts(source, "root:subagent:call_1");
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let session_start = |agent_id, parent_id| AgentEvent::SessionStart {
            agent_id,
            source: source.into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id,
        };
        let at = |s| t0 + Duration::from_secs(s);
        r.apply(
            &mut scene,
            session_start(parent, None),
            at(0),
            Transport::Hook,
        );
        act_start(
            &mut r,
            &mut scene,
            parent,
            None,
            Some(parent_tool),
            at(1),
            Transport::Hook,
        );
        r.apply(
            &mut scene,
            session_start(child, Some(parent)),
            at(2),
            Transport::Hook,
        );
        act_start(
            &mut r,
            &mut scene,
            child,
            None,
            Some("read_file"),
            at(2),
            Transport::Hook,
        );
        act_end(&mut r, &mut scene, child, None, at(3), Transport::Hook);
        act_end(&mut r, &mut scene, parent, None, at(4), Transport::Hook);
        r.tick(
            &mut scene,
            at(4) + B1_CASCADE_GRACE + Duration::from_millis(10),
        );
        assert_eq!(
            scene.agents[&child].exiting_at.is_some(),
            walks_out,
            "{source}: the child's walk-out on its parent's id-less {parent_tool} end"
        );
        assert!(
            scene.agents[&parent].exiting_at.is_none(),
            "{source} {parent_tool}"
        );
    }
}

/// A reasonix skill call returns before its turn can end.
#[test]
fn a_turn_end_over_an_untracked_task_walks_out_its_child_under_any_task_end() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let (parent, child) = (
        AgentId::from_parts("reasonix", "root"),
        AgentId::from_parts("reasonix", "root:subagent:call_1"),
    );
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for (agent_id, parent_id) in [(parent, None), (child, Some(parent))] {
        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id,
                source: "reasonix".into(),
                session_id: "s".into(),
                cwd: PathBuf::from("/repo"),
                parent_id,
            },
            t0,
            Transport::Hook,
        );
    }
    act_start(
        &mut r,
        &mut scene,
        parent,
        None,
        Some("Agent"),
        t0,
        Transport::Hook,
    );
    let end = t0 + Duration::from_secs(1);
    turn_end(&mut r, &mut scene, parent, end, Transport::Hook);
    r.tick(
        &mut scene,
        end + B1_CASCADE_GRACE + Duration::from_millis(10),
    );
    assert!(scene.agents[&child].exiting_at.is_some());
    assert!(scene.agents[&parent].exiting_at.is_none());
}

/// Two reasonix skill calls overlap. The first return walks out the other's
/// live child too; that child comes back on its next hook, and the other
/// call's return must still walk it out, though the parent went Idle between.
#[test]
fn overlapping_skill_calls_each_walk_their_child_out_on_return() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    fn feed(r: &mut Reducer, scene: &mut SceneState, v: serde_json::Value, t: SystemTime) {
        for ev in decode_hook_payload(v).expect("reasonix payload decodes") {
            r.apply(scene, ev, t, Transport::Hook);
        }
    }
    let hook = |event: &str, session: &str, tool: &str| {
        json!({
            "event": event, "sessionId": session, "cwd": "/repo",
            "toolName": tool, "_pixtuoid_source": "reasonix",
        })
    };
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let at = |ms| t0 + Duration::from_millis(ms);
    let late_child = AgentId::from_parts("reasonix", "root:subagent:call_b");

    feed(&mut r, &mut scene, hook("SessionStart", "root", ""), at(0));
    feed(
        &mut r,
        &mut scene,
        hook("PreToolUse", "root", "explore"),
        at(100),
    );
    feed(
        &mut r,
        &mut scene,
        hook("PreToolUse", "root", "research"),
        at(200),
    );
    feed(
        &mut r,
        &mut scene,
        hook("PreToolUse", "root:subagent:call_a", "read_file"),
        at(300),
    );
    feed(
        &mut r,
        &mut scene,
        hook("PreToolUse", "root:subagent:call_b", "read_file"),
        at(400),
    );
    let first_return = at(1_000);
    feed(
        &mut r,
        &mut scene,
        hook("PostToolUse", "root", "explore"),
        first_return,
    );
    let gone = first_return + B1_CASCADE_GRACE + EXIT_GRACE_WINDOW + Duration::from_secs(1);
    r.tick(&mut scene, first_return + B1_CASCADE_GRACE);
    r.tick(&mut scene, gone);
    assert!(
        !scene.agents.contains_key(&late_child),
        "the first return walked it out"
    );

    let back = gone + Duration::from_secs(1);
    feed(
        &mut r,
        &mut scene,
        hook("PreToolUse", "root:subagent:call_b", "read_file"),
        back,
    );
    assert!(
        scene.agents.contains_key(&late_child),
        "its next hook brought it back"
    );
    let second_return = back + Duration::from_secs(1);
    feed(
        &mut r,
        &mut scene,
        hook("PostToolUse", "root", "research"),
        second_return,
    );
    r.tick(&mut scene, second_return + B1_CASCADE_GRACE);
    assert!(
        scene.agents[&late_child].exiting_at.is_some(),
        "the second return walks its own child out"
    );
}

/// Two shims a few ms apart can be decoded in either order (#242): a Task's
/// hook End landing first must not leave its Start opening a call that already
/// returned, which would put the parent back to Delegating after each own tool.
#[test]
fn a_task_end_decoded_before_its_start_does_not_reopen_the_call() {
    // Past the window the End's record is gone and the Start opens the call.
    for (after_end, dropped) in [
        (HOOK_WINS_WINDOW / 10, true),
        (HOOK_WINS_WINDOW + Duration::from_millis(1), false),
    ] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let id = AgentId::from_parts("reasonix", "root");
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: id,
                source: "reasonix".into(),
                session_id: "root".into(),
                cwd: PathBuf::from("/repo"),
                parent_id: None,
            },
            t0,
            Transport::Hook,
        );
        let end = t0 + Duration::from_secs(1);
        act_end(&mut r, &mut scene, id, Some("call_1"), end, Transport::Hook);
        let start = end + after_end;
        act_start(
            &mut r,
            &mut scene,
            id,
            Some("call_1"),
            Some("Agent"),
            start,
            Transport::Hook,
        );
        let own = start + Duration::from_secs(1);
        act_start(
            &mut r,
            &mut scene,
            id,
            None,
            Some("Bash: ls"),
            own,
            Transport::Hook,
        );
        let own_end = own + Duration::from_secs(1);
        act_end(&mut r, &mut scene, id, None, own_end, Transport::Hook);
        let shown = match &scene.agents[&id].state {
            ActivityState::Active { detail, .. } => detail.as_deref().map(str::to_owned),
            other => panic!("expected Active, got {other:?}"),
        };
        let want = if dropped { "Bash: ls" } else { "Delegating" };
        assert_eq!(shown.as_deref(), Some(want), "{after_end:?} after the End");
    }
}

#[test]
fn a_call_whose_start_lands_after_its_end_still_counts() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("reasonix", "root");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "reasonix".into(),
            session_id: "root".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    let end = t0 + Duration::from_secs(1);
    act_end(&mut r, &mut scene, id, Some("call_1"), end, Transport::Hook);
    let start = end + HOOK_WINS_WINDOW / 10;
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("call_1"),
        Some("Bash: ls"),
        start,
        Transport::Hook,
    );
    assert_eq!(scene.agents[&id].tool_call_count, 1);
}

#[test]
fn oversized_attach_synthesized_task_start_arms_b1() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts(DRAIN_INFERRED, "/p/att.jsonl");
    let child = AgentId::from_parts(DRAIN_INFERRED, "/p/att/subagents/agent-1.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for (id, parent_id, at) in [
        (parent, None, t0),
        (child, Some(parent), t0 + Duration::from_millis(100)),
    ] {
        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: id,
                source: DRAIN_INFERRED.into(),
                session_id: "s".into(),
                cwd: PathBuf::from("/repo"),
                parent_id,
            },
            at,
            Transport::Jsonl,
        );
    }

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("tu_task"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Jsonl,
    );
    assert_delegating(
        &scene,
        parent,
        "the synthesized Jsonl Task start must seed active_tasks — no hook record exists at mid-attach to dedup-eat it",
    );

    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("tu_task"),
        t0 + Duration::from_secs(10),
        Transport::Jsonl,
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "the b1 cascade is grace-deferred (#151) — never immediate"
    );
    r.tick(
        &mut scene,
        t0 + Duration::from_secs(10) + B1_CASCADE_GRACE + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "the synthesized Task start must arm b1: the drain cascades the completed subagent out"
    );
    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_none(),
        "the parent keeps running after the Task drains"
    );
}

#[test]
fn a_dispatch_inside_the_grace_cancels_the_pending_cascade() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "orch-late", t0);
    let t1 = t0 + Duration::from_secs(1);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        t1 + Duration::from_millis(200),
        Transport::Hook,
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "the drain must not cascade-exit the subtree immediately (#151)"
    );

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-2"),
        Some("Agent"),
        t1 + Duration::from_secs(1),
        Transport::Hook,
    );
    r.tick(
        &mut scene,
        t1 + Duration::from_millis(200) + B1_CASCADE_GRACE + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "a Task insert inside the grace must cancel the pending cascade — the subtree is still working"
    );
    assert_delegating(&scene, parent, "parent stays Delegating on the second Task");

    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-2"),
        t1 + Duration::from_secs(5),
        Transport::Jsonl,
    );
    r.tick(
        &mut scene,
        t1 + Duration::from_secs(5) + B1_CASCADE_GRACE + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "the last Task's drain must still cascade-exit the completed subtree after the grace"
    );
}

#[test]
fn second_drain_inside_grace_restarts_the_cascade_clock() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "orch-rearm", t0);
    let t1 = t0 + Duration::from_secs(1);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        t1 + Duration::from_millis(200),
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-2"),
        Some("Agent"),
        t1 + Duration::from_secs(1),
        Transport::Jsonl,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-2"),
        t1 + Duration::from_secs(2),
        Transport::Jsonl,
    );

    r.tick(
        &mut scene,
        t1 + Duration::from_millis(200) + B1_CASCADE_GRACE + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "the second drain must restart the grace clock — firing on the FIRST drain's timestamp re-opens the #151A window"
    );
    r.tick(
        &mut scene,
        t1 + Duration::from_secs(2) + B1_CASCADE_GRACE + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "the cascade still fires one grace after the last drain"
    );
}

#[test]
fn late_jsonl_replay_of_drained_task_end_does_not_false_resolve_waiting() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-152", t0);
    let t1 = t0 + Duration::from_secs(1);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    waiting(
        &mut r,
        &mut scene,
        parent,
        "permission",
        t1 + Duration::from_millis(100),
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t1 + Duration::from_secs(1),
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t1 + Duration::from_secs(1) + HOOK_WINS_WINDOW + Duration::from_millis(100),
        Transport::Jsonl,
    );
    r.tick(
        &mut scene,
        t1 + Duration::from_secs(1)
            + HOOK_WINS_WINDOW
            + Duration::from_millis(100)
            + ACTIVE_GRACE_WINDOW
            + Duration::from_millis(10),
    );
    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "a late JSONL replay of the drained Task END must not match the stale gate and false-resolve a still-pending permission Waiting"
    );
}

#[test]
fn task_drain_keeps_parallel_ordinary_tool_gate() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-keep", t0);
    let t1 = t0 + Duration::from_secs(1);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        Some("Bash: ls"),
        t1 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    waiting(
        &mut r,
        &mut scene,
        parent,
        "permission",
        t1 + Duration::from_millis(200),
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t1 + Duration::from_secs(1),
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        t1 + Duration::from_secs(2),
        Transport::Jsonl,
    );
    r.tick(
        &mut scene,
        t1 + Duration::from_secs(2) + ACTIVE_GRACE_WINDOW + Duration::from_millis(10),
    );
    assert_eq!(
        scene.agents.get(&parent).unwrap().state,
        ActivityState::Idle,
        "the kept gate must let the ordinary tool's END resolve the Waiting"
    );
}

#[test]
fn suppressed_child_event_keeps_parents_own_parallel_tool_gate() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, _child) =
        delegating_pair_on(&mut r, &mut scene, "orch-own-gate", t0, UNNAMED_CHILD_HOOKS);
    let t1 = t0 + Duration::from_secs(1);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        Some("Bash: ls"),
        t1 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    waiting(
        &mut r,
        &mut scene,
        parent,
        "permission",
        t1 + Duration::from_millis(200),
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("sub-R"),
        Some("Read: /foo"),
        t1 + Duration::from_millis(300),
        Transport::Hook,
    );
    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "a suppressed child event must not hide the parent's own still-pending permission Waiting"
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        t1 + Duration::from_secs(1),
        Transport::Jsonl,
    );
    r.tick(
        &mut scene,
        t1 + Duration::from_secs(1) + ACTIVE_GRACE_WINDOW + Duration::from_millis(10),
    );
    assert!(
        !matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "the kept gate must let the own tool's END resolve the Waiting"
    );
}

#[test]
fn own_parallel_tool_end_mid_delegation_returns_parent_to_delegating() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "orch-own-end", t0);
    let t1 = t0 + Duration::from_secs(1);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        Some("Bash: ls"),
        t1 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        t1 + Duration::from_millis(500),
        Transport::Jsonl,
    );
    r.tick(
        &mut scene,
        t1 + Duration::from_millis(500) + ACTIVE_GRACE_WINDOW + Duration::from_millis(10),
    );
    assert_delegating(
        &scene,
        parent,
        "parent must stay Delegating while its Task is still in flight — not settle to Idle",
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "the own tool's END must not have cascaded the live subtree"
    );
}

#[test]
fn a_turn_end_mid_delegation_returns_the_parent_to_delegating() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "orch-turn-end", t0);
    let t1 = t0 + Duration::from_secs(1);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        Some("Bash: ls"),
        t1 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    let end = t1 + Duration::from_millis(500);
    turn_end(&mut r, &mut scene, parent, end, Transport::Hook);
    r.tick(
        &mut scene,
        end + ACTIVE_GRACE_WINDOW + Duration::from_millis(10),
    );
    assert_delegating(&scene, parent, "a Task still in flight outlives the turn");
    assert!(scene.agents[&child].exiting_at.is_none());
}

#[test]
fn late_batched_jsonl_pair_after_delivered_hook_end_is_fully_dropped() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/batched.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t-fast"),
        Some("Read: /x"),
        t0,
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        id,
        Some("t-fast"),
        t0 + HOOK_WINS_WINDOW / 10,
        Transport::Hook,
    );
    let armed_at = scene.agents.get(&id).unwrap().pending_idle_at;
    assert!(armed_at.is_some(), "hook END arms the idle debounce");
    let count = scene.agents.get(&id).unwrap().tool_call_count;

    // These offsets put the lagged pair PAST the START record's expiry (t0 + W) but
    // still inside the END record's window, so only the END record's both-kinds
    // dominance can drop the stale START.
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t-fast"),
        Some("Read: /x"),
        t0 + HOOK_WINS_WINDOW + HOOK_WINS_WINDOW / 20,
        Transport::Jsonl,
    );
    act_end(
        &mut r,
        &mut scene,
        id,
        Some("t-fast"),
        t0 + HOOK_WINS_WINDOW + HOOK_WINS_WINDOW / 20,
        Transport::Jsonl,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(
        slot.pending_idle_at, armed_at,
        "stale JSONL replay must not cancel or re-arm the idle debounce"
    );
    assert_eq!(
        slot.tool_call_count, count,
        "stale JSONL replay must not double-count the tool"
    );
}

#[test]
fn jsonl_task_start_duplicate_does_not_clobber_waiting_parent() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-wait", t0);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    waiting(
        &mut r,
        &mut scene,
        parent,
        "permission",
        t0 + Duration::from_secs(1) + HOOK_WINS_WINDOW / 50,
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1) + HOOK_WINS_WINDOW / 5,
        Transport::Jsonl,
    );

    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "a dedup-dropped JSONL duplicate of the dispatch must not clobber the parent's pending permission Waiting"
    );
}

#[test]
fn jsonl_task_start_replay_outside_dedup_window_does_not_clobber_waiting_parent() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-wait-late", t0);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    waiting(
        &mut r,
        &mut scene,
        parent,
        "permission",
        t0 + Duration::from_secs(1) + HOOK_WINS_WINDOW / 50,
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1) + HOOK_WINS_WINDOW * 2,
        Transport::Jsonl,
    );

    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "an out-of-dedup-window JSONL replay of an already-tracked dispatch must not clobber the parent's pending permission Waiting"
    );
}

#[test]
fn lagged_jsonl_task_pair_after_drain_does_not_clobber_waiting_parent() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-drained", t0);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );

    waiting(
        &mut r,
        &mut scene,
        parent,
        "permission",
        t0 + Duration::from_secs(3),
        Transport::Hook,
    );

    let replay_at = t0 + Duration::from_secs(2) + B1_CASCADE_GRACE + Duration::from_millis(100);
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        replay_at,
        Transport::Jsonl,
    );
    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "a replayed Start of an already-drained Task must not re-enter Delegating over a pending Waiting"
    );

    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        replay_at,
        Transport::Jsonl,
    );
    r.tick(
        &mut scene,
        replay_at + ACTIVE_GRACE_WINDOW + Duration::from_millis(100),
    );
    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "the replayed pair must leave the still-pending permission Waiting, got {:?}",
        scene.agents.get(&parent).unwrap().state
    );
}

#[test]
fn jsonl_ordinary_tool_end_drains_when_hook_end_drops() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/fastdrop.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t-1"),
        Some("Read: /x"),
        t0,
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        id,
        Some("t-1"),
        t0 + HOOK_WINS_WINDOW / 5,
        Transport::Jsonl,
    );
    assert!(
        scene.agents.get(&id).unwrap().pending_idle_at.is_some(),
        "the JSONL END is the fallback for the dropped hook END — it must arm the idle debounce, not be eaten by the START's dedup record"
    );
}

#[test]
fn a_parallel_dispatch_keeps_the_first_drain_from_cascading() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "orch-par", t0);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-2"),
        Some("Agent"),
        t0 + Duration::from_secs(1) + HOOK_WINS_WINDOW / 10,
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        t0 + Duration::from_secs(1) + HOOK_WINS_WINDOW * 2 / 5,
        Transport::Hook,
    );

    r.tick(
        &mut scene,
        t0 + Duration::from_secs(1)
            + HOOK_WINS_WINDOW * 2 / 5
            + B1_CASCADE_GRACE
            + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "first Task's drain must not cascade-exit the subtree while the second Task is still in flight"
    );
    assert_delegating(
        &scene,
        parent,
        "parent must stay Delegating on the second Task",
    );

    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-2"),
        t0 + Duration::from_secs(5),
        Transport::Jsonl,
    );
    r.tick(
        &mut scene,
        t0 + Duration::from_secs(5) + B1_CASCADE_GRACE + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "last Task's drain must cascade-exit the completed subtree"
    );
}

#[test]
fn jsonl_task_self_end_drains_when_hook_end_drops() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "orch-drop", t0);

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t0 + Duration::from_secs(1) + HOOK_WINS_WINDOW / 5,
        Transport::Jsonl,
    );

    r.tick(
        &mut scene,
        t0 + Duration::from_secs(1)
            + HOOK_WINS_WINDOW / 5
            + B1_CASCADE_GRACE
            + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "the JSONL Task self-END must drain active_tasks and fire the b1 cascade (after the #151 grace) — it is the fallback for the dropped hook END"
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("b-1"),
        Some("Bash: ls"),
        t0 + Duration::from_secs(5),
        Transport::Hook,
    );
    match &scene.agents.get(&parent).unwrap().state {
        ActivityState::Active { detail, .. } => {
            assert_eq!(
                detail.as_deref(),
                Some("Bash: ls"),
                "suppression must release once the Task drained via JSONL"
            );
        }
        other => panic!("expected Active(Bash: ls), got {other:?}"),
    }
}

#[test]
fn parent_waiting_on_subagent_permission_resolves_when_the_subagent_resumes() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "orch");
    let child = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "/p/orch/subagents/agent-1.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: UNNAMED_CHILD_HOOKS.into(),
            session_id: "p".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: UNNAMED_CHILD_HOOKS.into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    waiting(
        &mut r,
        &mut scene,
        parent,
        "permission?",
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );
    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "parent goes Waiting on the subagent's permission"
    );

    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("sub-bash"),
        Some("Bash: ls"),
        t0 + Duration::from_secs(3),
        Transport::Hook,
    );

    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Active { .. }
        ),
        "parent resumes Active(Delegating) once the subagent works again — no stale Waiting"
    );
    assert!(scene.agents.get(&child).unwrap().exiting_at.is_none());
}

#[test]
fn parent_waiting_on_subagent_permission_resolves_when_the_subagent_ends_a_tool() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "orch2");
    let child = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "/p/orch2/subagents/agent-1.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: UNNAMED_CHILD_HOOKS.into(),
            session_id: "p".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: UNNAMED_CHILD_HOOKS.into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    waiting(
        &mut r,
        &mut scene,
        parent,
        "permission?",
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );
    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "parent goes Waiting on the subagent's permission"
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("sub-bash"),
        t0 + Duration::from_secs(3),
        Transport::Hook,
    );
    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::Active { .. }
        ),
        "a suppressed child END must ALSO restore Active(Delegating), not leave a stale Waiting"
    );
}

#[test]
fn task_drain_while_parent_waiting_keeps_waiting() {
    use pixtuoid_core::source::ToolDetail;
    use pixtuoid_core::state::reducer::ACTIVE_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/wait.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, id);

    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: id,
            tool_use_id: Some("task-T".into()),
            detail: Some(ToolDetail::Task),
        },
        t0,
        Transport::Hook,
    );
    waiting(
        &mut r,
        &mut scene,
        id,
        "permission",
        t0 + Duration::from_millis(500),
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents[&id].state,
        ActivityState::Waiting { .. }
    ));

    act_end(
        &mut r,
        &mut scene,
        id,
        Some("task-T"),
        t0 + Duration::from_millis(1000),
        Transport::Hook,
    );
    assert!(
        scene.agents[&id].pending_idle_at.is_none(),
        "Task drain must not arm idle-resolve on a Waiting parent"
    );

    r.tick(
        &mut scene,
        t0 + Duration::from_millis(1000) + ACTIVE_GRACE_WINDOW + Duration::from_millis(100),
    );
    assert!(
        matches!(scene.agents[&id].state, ActivityState::Waiting { .. }),
        "parent's permission must stay Waiting through a Task drain, got {:?}",
        scene.agents[&id].state
    );
}

#[test]
fn desk_exhausted_task_dispatch_leaves_no_ghost_slot() {
    let mut scene = SceneState::new([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut r = Reducer::new();
    let seated = AgentId::from_transcript_path("/proj/seated.jsonl");
    start(&mut r, &mut scene, seated);
    assert_eq!(scene.agents.len(), 1, "the single desk is occupied");

    let t0 = SystemTime::now();
    let orphan = AgentId::from_transcript_path("/proj/orphan.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: orphan,
            tool_use_id: Some("t1".into()),
            detail: Some(ToolDetail::Task),
        },
        t0,
        Transport::Hook,
    );
    assert!(
        !scene.agents.contains_key(&orphan),
        "the desk-starved dispatch mints no ghost slot"
    );
    assert_eq!(
        scene.agents.len(),
        1,
        "the orphan never registered a session"
    );

    r.tick(&mut scene, t0 + Duration::from_secs(2));
    assert!(
        !scene.agents.contains_key(&orphan),
        "tick introduced no ghost slot for the orphan"
    );
    assert_eq!(scene.agents.len(), 1, "the seated agent is untouched");
}

/// The `handled_by_task_tracking` guard's REMOVAL was invisible to all 36 test
/// binaries: every existing test drains the LAST task, where the drain path and
/// the general arm happen to agree. With parallel Tasks in flight they diverge —
/// without the guard the general arm re-runs `enter_delegating`, resetting
/// `state_started_at` on a parent that never left Delegating.
///
/// Asserting the resulting STATE alone would reproduce the gap (it is Delegating
/// either way); the clock is what pins the guard.
#[test]
fn ending_one_of_two_parallel_tasks_does_not_restart_the_parents_delegating_clock() {
    let mut r = Reducer::new();
    let mut scene = SceneState::uniform(4);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let parent = AgentId::from_transcript_path("/p/parallel.jsonl");
    start(&mut r, &mut scene, parent);

    // TWO Task dispatches in flight — the case no existing test covers.
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-a"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-b"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );
    let armed_at = scene.agents[&parent].state_started_at;

    // Drain ONE. The other is still live, so the parent stays Delegating.
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-a"),
        t0 + Duration::from_secs(30),
        Transport::Hook,
    );

    let slot = &scene.agents[&parent];
    assert!(
        matches!(slot.state, ActivityState::Active { .. }),
        "one of two parallel Tasks ending must leave the parent Delegating"
    );
    assert_eq!(
        slot.state_started_at, armed_at,
        "the drain already applied this event; the general arm must be SKIPPED, \
         or it re-enters Delegating and restarts the clock mid-delegation"
    );
}

/// A claude-code background launch: the Agent call returns at launch
/// (hooks.md: `async_launched`), its PostToolUse beside the child's
/// SubagentStart in either order, so its drain says nothing about the child:
/// still standing past the b1 grace, gone only on its own SubagentStop (#1701).
#[test]
fn a_claude_code_background_child_outlives_its_launch() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    const SID: &str = "5717f13f-3c42-4a73-bfa5-3cd14932f155";
    let hook = |r: &mut Reducer, scene: &mut SceneState, mut v: serde_json::Value, at| {
        v["session_id"] = json!(SID);
        v["cwd"] = json!("/repo");
        v["_pixtuoid_source"] = json!("claude-code");
        for ev in decode_hook_payload(v).expect("a recorded CC payload decodes") {
            r.apply(scene, ev, at, Transport::Hook);
        }
    };
    let launch = json!({"hook_event_name": "PreToolUse", "tool_name": "Agent", "tool_use_id": "toolu_1",
        "tool_input": {"description": "Child", "prompt": "p", "subagent_type": "general-purpose"}});
    let started = json!({"hook_event_name": "SubagentStart", "agent_id": "a234ddfa51533488f",
        "agent_type": "general-purpose"});
    let returned = json!({"hook_event_name": "PostToolUse", "tool_name": "Agent", "tool_use_id": "toolu_1",
        "tool_response": {"status": "async_launched", "agentId": "a234ddfa51533488f"}});
    let stopped = json!({"hook_event_name": "SubagentStop", "agent_id": "a234ddfa51533488f",
        "agent_transcript_path": format!("/p/{SID}/subagents/agent-a234ddfa51533488f.jsonl")});
    let child = AgentId::from_parts("claude-code", "agent-a234ddfa51533488f");
    for start_first in [true, false] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        hook(
            &mut r,
            &mut scene,
            json!({"hook_event_name": "SessionStart"}),
            t0,
        );
        hook(&mut r, &mut scene, launch.clone(), t0);
        let (first, second) = if start_first {
            (&started, &returned)
        } else {
            (&returned, &started)
        };
        hook(
            &mut r,
            &mut scene,
            first.clone(),
            t0 + Duration::from_millis(18),
        );
        hook(
            &mut r,
            &mut scene,
            second.clone(),
            t0 + Duration::from_millis(18),
        );
        let past = t0 + B1_CASCADE_GRACE + EXIT_GRACE_WINDOW + Duration::from_secs(1);
        r.tick(&mut scene, past);
        assert!(
            scene
                .agents
                .get(&child)
                .is_some_and(|s| s.exiting_at.is_none()),
            "start_first={start_first}: the launch's drain walked the running child out"
        );
        hook(&mut r, &mut scene, stopped.clone(), past);
        assert!(
            scene.agents[&child].exiting_at.is_some(),
            "start_first={start_first}: its own SubagentStop ends it"
        );
    }
}

/// A source whose subagents' hooks name the subagent drops nothing: with a
/// Task in flight, the parent's own hook tool shows.
#[test]
fn a_named_childs_parent_shows_its_own_tool_during_a_task() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts("claude-code", "parent");
    start(&mut r, &mut scene, parent);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );
    let own = t0 + Duration::from_secs(1);
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("own"),
        Some("Bash: ls"),
        own,
        Transport::Hook,
    );
    match &scene.agents[&parent].state {
        ActivityState::Active { detail, .. } => assert_eq!(detail.as_deref(), Some("Bash: ls")),
        other => panic!("expected the parent's own tool, got {other:?}"),
    }
}
