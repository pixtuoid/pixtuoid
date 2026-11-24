use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::decoder::decode_hook_payload;
use pixtuoid_core::source::{AgentEvent, Resolution, Transport};
use pixtuoid_core::state::reducer::{B1_CASCADE_GRACE, Reducer};
use pixtuoid_core::state::{ActivityState, SceneState, ToolUse};
use serde_json::json;

use crate::{
    DRAIN_INFERRED, HOOK_AND_TRANSCRIPT, UNNAMED_CHILD_HOOKS, delegating_pair, delegating_pair_on,
    start, start_as, tool_use_end, tool_use_start, turn_end, user_action_request,
    user_action_request_for,
};

#[track_caller]
fn assert_delegating(scene: &SceneState, id: AgentId, msg: &str) {
    match &scene.agents.get(&id).unwrap().state {
        ActivityState::Running {
            item: Some(ToolUse { detail, .. }),
        } => {
            assert_eq!(detail.as_deref(), Some("Delegating"), "{msg}");
        }
        other => panic!("expected Running(Delegating), got {other:?} — {msg}"),
    }
}

#[test]
fn hook_activity_during_a_task_in_flight_is_suppressed() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "parent");
    start_as(&mut r, &mut scene, parent, UNNAMED_CHILD_HOOKS);

    let t0 = SystemTime::now();

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );

    tool_use_start(
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
        ActivityState::Running {
            item: Some(ToolUse { detail, .. }),
        } => {
            assert_eq!(detail.as_deref(), Some("Delegating"));
        }
        other => panic!("expected Running(Delegating), got {other:?}"),
    }

    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("subagent-R"),
        t0 + Duration::from_millis(60),
        Transport::Hook,
    );
    assert_delegating(
        &scene,
        parent,
        "a suppressed subagent End must not settle the parent while its task is in flight",
    );

    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t0 + Duration::from_millis(200),
        Transport::Hook,
    );
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

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );
    user_action_request_for(
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
    assert!(
        matches!(
            scene.agents[&parent].state,
            ActivityState::RequiresAction { .. }
        ),
        "got {:?}",
        scene.agents[&parent].state
    );
}

#[test]
fn a_transcript_echo_of_an_ended_call_never_reopens_it() {
    use pixtuoid_core::source::CopyId;
    for lag in [
        Duration::ZERO,
        Duration::from_secs(1),
        Duration::from_secs(10 * 60),
    ] {
        let mut scene = SceneState::uniform(2);
        let mut r = Reducer::new();
        let id = AgentId::from_transcript_path("/p/echo.jsonl");
        start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some("t-1"),
            Some("Edit: a.rs"),
            t0,
            Transport::Hook,
        );
        let ended = t0 + Duration::from_millis(100);
        tool_use_end(&mut r, &mut scene, id, Some("t-1"), ended, Transport::Hook);
        let settled = scene.agents[&id].clone();
        r.apply(
            &mut scene,
            AgentEvent::tool_use_start(id, Some("t-1".into()), Some("Edit: a.rs".into()))
                .with_copy_id(CopyId::new("t-1")),
            ended + lag,
            Transport::Jsonl,
        );
        let slot = &scene.agents[&id];
        assert_eq!(
            (&slot.state, slot.state_started_at, slot.tool_call_count),
            (&settled.state, settled.state_started_at, 1),
            "{lag:?} after its end"
        );
    }
}

#[test]
fn subagent_is_removed_promptly_when_its_parent_task_completes() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "orch", t0);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        child,
        Some("c1"),
        Some("Read: /x"),
        t0 + Duration::from_secs(2),
        Transport::Jsonl,
    );
    tool_use_end(
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
        tool_use_start(
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
        tool_use_start(
            &mut r,
            &mut scene,
            child,
            None,
            Some("read_file"),
            at(2),
            Transport::Hook,
        );
        tool_use_end(&mut r, &mut scene, child, None, at(3), Transport::Hook);
        tool_use_end(&mut r, &mut scene, parent, None, at(4), Transport::Hook);
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
    tool_use_start(
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

fn feed_reasonix(
    r: &mut Reducer,
    scene: &mut SceneState,
    (event, session, tool): (&str, &str, &str),
    t: SystemTime,
) {
    let payload = json!({
        "event": event, "sessionId": session, "cwd": "/repo",
        "toolName": tool, "_pixtuoid_source": "reasonix",
    });
    for ev in decode_hook_payload(payload).expect("reasonix payload decodes") {
        r.apply(scene, ev, t, Transport::Hook);
    }
}

/// Two reasonix skill calls overlap. The first return walks out the other's
/// live child too; that child comes back on its next hook, and the other
/// call's return must still walk it out, though the parent went Idle between.
#[test]
fn overlapping_skill_calls_each_walk_their_child_out_on_return() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let at = |ms| t0 + Duration::from_millis(ms);
    let late_child = AgentId::from_parts("reasonix", "root:subagent:call_b");

    feed_reasonix(&mut r, &mut scene, ("SessionStart", "root", ""), at(0));
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PreToolUse", "root", "explore"),
        at(100),
    );
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PreToolUse", "root", "research"),
        at(200),
    );
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PreToolUse", "root:subagent:call_a", "read_file"),
        at(300),
    );
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PreToolUse", "root:subagent:call_b", "read_file"),
        at(400),
    );
    let first_return = at(1_000);
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PostToolUse", "root", "explore"),
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
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PreToolUse", "root:subagent:call_b", "read_file"),
        back,
    );
    assert!(
        scene.agents.contains_key(&late_child),
        "its next hook brought it back"
    );
    let second_return = back + Duration::from_secs(1);
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PostToolUse", "root", "research"),
        second_return,
    );
    r.tick(&mut scene, second_return + B1_CASCADE_GRACE);
    assert!(
        scene.agents[&late_child].exiting_at.is_some(),
        "the second return walks its own child out"
    );
}

/// An inline skill returns beside a parallel subagent skill's live child. Its
/// return names no call, so it walks that child out too; the child comes back
/// on its next hook, and its own call's return walks it out.
#[test]
fn an_inline_skills_return_walks_a_live_sibling_child_out_until_its_next_hook() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let at = |ms| t0 + Duration::from_millis(ms);
    let child = AgentId::from_parts("reasonix", "root:subagent:call_a");
    let mut feed = |event: (&str, &str, &str), t| feed_reasonix(&mut r, &mut scene, event, t);
    feed(("SessionStart", "root", ""), at(0));
    feed(("PreToolUse", "root", "read_only_skill"), at(100));
    feed(("PreToolUse", "root", "read_only_skill"), at(200));
    feed(("PreToolUse", "root:subagent:call_a", "read_file"), at(300));
    let inline_return = at(400);
    feed(("PostToolUse", "root", "read_only_skill"), inline_return);
    let gone = inline_return + B1_CASCADE_GRACE + EXIT_GRACE_WINDOW + Duration::from_secs(1);
    r.tick(&mut scene, inline_return + B1_CASCADE_GRACE);
    r.tick(&mut scene, gone);
    assert!(
        !scene.agents.contains_key(&child),
        "the inline return walked it out"
    );

    let back = gone + Duration::from_secs(1);
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PreToolUse", "root:subagent:call_a", "read_file"),
        back,
    );
    assert!(
        scene.agents.contains_key(&child),
        "its next hook brought it back"
    );
    let own_return = back + Duration::from_secs(1);
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PostToolUse", "root", "read_only_skill"),
        own_return,
    );
    r.tick(&mut scene, own_return + B1_CASCADE_GRACE);
    assert!(
        scene.agents[&child].exiting_at.is_some(),
        "its own call's return walks it out"
    );
}

fn returned_reasonix_dispatch(r: &mut Reducer, scene: &mut SceneState) -> SystemTime {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let at = |ms| t0 + Duration::from_millis(ms);
    let returned = 1_000;
    for (hook, ms) in [
        (("SessionStart", "root", ""), 0),
        (("PreToolUse", "root", "explore"), 100),
        (("PreToolUse", "root:subagent:call_a", "read_file"), 200),
        (("PostToolUse", "root:subagent:call_a", "read_file"), 300),
        (("PostToolUse", "root", "explore"), returned),
    ] {
        feed_reasonix(r, scene, hook, at(ms));
    }
    at(returned)
}

/// A reasonix `SUBAGENT_TOOLS` call carries no call id, so one started inside an earlier
/// return's grace must cancel that cascade as a tracked dispatch does (#151).
#[test]
fn an_untracked_dispatch_inside_the_grace_cancels_the_pending_cascade() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let first_child = AgentId::from_parts("reasonix", "root:subagent:call_a");
    let second_child = AgentId::from_parts("reasonix", "root:subagent:call_b");
    let first_return = returned_reasonix_dispatch(&mut r, &mut scene);

    for (hook, t) in [
        (
            ("PreToolUse", "root", "explore"),
            first_return + B1_CASCADE_GRACE / 2,
        ),
        (
            ("PreToolUse", "root:subagent:call_b", "read_file"),
            first_return + B1_CASCADE_GRACE * 3 / 4,
        ),
    ] {
        feed_reasonix(&mut r, &mut scene, hook, t);
    }
    r.tick(&mut scene, first_return + B1_CASCADE_GRACE);
    assert!(
        scene.agents[&second_child].exiting_at.is_none(),
        "the first return's cascade walked out the second dispatch's live child"
    );

    let second_return = first_return + B1_CASCADE_GRACE * 2;
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PostToolUse", "root", "explore"),
        second_return,
    );
    r.tick(&mut scene, second_return + B1_CASCADE_GRACE);
    for child in [first_child, second_child] {
        assert!(
            scene.agents[&child].exiting_at.is_some(),
            "the second return walks the whole subtree out"
        );
    }
}

/// At the grace boundary the untracked start only shows on the slot once
/// applied, after the pre-pass fires, so the start itself must cancel the
/// cascade, as a tracked start's pre-pass insert does.
#[test]
fn an_untracked_dispatch_at_the_grace_boundary_cancels_the_pending_cascade() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let first_child = AgentId::from_parts("reasonix", "root:subagent:call_a");
    let first_return = returned_reasonix_dispatch(&mut r, &mut scene);

    feed_reasonix(
        &mut r,
        &mut scene,
        ("PreToolUse", "root", "explore"),
        first_return + B1_CASCADE_GRACE,
    );
    assert!(
        scene.agents[&first_child].exiting_at.is_none(),
        "the second dispatch's own start fired the first return's cascade"
    );

    let second_return = first_return + B1_CASCADE_GRACE * 2;
    feed_reasonix(
        &mut r,
        &mut scene,
        ("PostToolUse", "root", "explore"),
        second_return,
    );
    r.tick(&mut scene, second_return + B1_CASCADE_GRACE);
    assert!(
        scene.agents[&first_child].exiting_at.is_some(),
        "the second return walks the first child out"
    );
}

#[test]
fn only_the_parents_own_dispatch_at_the_grace_boundary_cancels_its_cascade() {
    for (session, tool) in [("root", "read_file"), ("other", "explore")] {
        let mut scene = SceneState::uniform(8);
        let mut r = Reducer::new();
        let first_child = AgentId::from_parts("reasonix", "root:subagent:call_a");
        let first_return = returned_reasonix_dispatch(&mut r, &mut scene);

        feed_reasonix(
            &mut r,
            &mut scene,
            ("PreToolUse", session, tool),
            first_return + B1_CASCADE_GRACE,
        );
        assert!(
            scene.agents[&first_child].exiting_at.is_some(),
            "{session}'s {tool} at the boundary cancelled root's cascade"
        );
    }
}

/// Two shims a few ms apart can be decoded in either order (#242): a Task's
/// hook End landing first must not leave its Start opening a call that already
/// returned, which would put the parent back to Delegating after each own tool.
#[test]
fn a_task_end_decoded_before_its_start_does_not_reopen_the_call() {
    for after_end in [Duration::from_millis(50), Duration::from_secs(10 * 60)] {
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
        tool_use_end(&mut r, &mut scene, id, Some("call_1"), end, Transport::Hook);
        let start = end + after_end;
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some("call_1"),
            Some("Agent"),
            start,
            Transport::Hook,
        );
        let own = start + Duration::from_secs(1);
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            None,
            Some("Bash: ls"),
            own,
            Transport::Hook,
        );
        let own_end = own + Duration::from_secs(1);
        tool_use_end(&mut r, &mut scene, id, None, own_end, Transport::Hook);
        let shown = match &scene.agents[&id].state {
            ActivityState::Running {
                item: Some(ToolUse { detail, .. }),
            } => detail.as_deref(),
            ActivityState::Idle => None,
            other => panic!("expected Running or Idle, got {other:?}"),
        };
        assert_eq!(shown, None, "{after_end:?} after the End");
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
    tool_use_end(&mut r, &mut scene, id, Some("call_1"), end, Transport::Hook);
    let start = end + Duration::from_secs(10 * 60);
    tool_use_start(
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

    tool_use_start(
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
        "the synthesized Jsonl Task start must seed tasks_in_flight",
    );

    tool_use_end(
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

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    tool_use_end(
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

    tool_use_start(
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

    tool_use_end(
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

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        t1 + Duration::from_millis(200),
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-2"),
        Some("Agent"),
        t1 + Duration::from_secs(1),
        Transport::Jsonl,
    );
    tool_use_end(
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

/// A drained Task's END replayed however late answers no standing prompt.
#[test]
fn late_jsonl_replay_of_drained_task_end_does_not_false_resolve_requires_action() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-152", t0);
    let t1 = t0 + Duration::from_secs(1);

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    user_action_request(
        &mut r,
        &mut scene,
        parent,
        "permission",
        t1 + Duration::from_millis(100),
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t1 + Duration::from_secs(1),
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t1 + Duration::from_secs(1) + Duration::from_secs(10 * 60),
        Transport::Jsonl,
    );
    assert!(
        matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "a late JSONL replay of the drained Task END must not false-resolve a still-pending permission RequiresAction"
    );
}

/// #152: a subagent returning answers a prompt only when the prompt named its
/// Task on the wire, never one a deduction bound to it.
#[test]
fn a_drained_task_answers_only_a_request_naming_it() {
    for (what, names, answered) in [
        ("bound by deduction", None, false),
        ("naming the Task", Some("task-T"), true),
    ] {
        let mut scene = SceneState::uniform(8);
        let mut r = Reducer::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-152-named", t0);
        let t1 = t0 + Duration::from_secs(1);
        tool_use_start(
            &mut r,
            &mut scene,
            parent,
            Some("task-T"),
            Some("Agent"),
            t1,
            Transport::Hook,
        );
        user_action_request_for(
            &mut r,
            &mut scene,
            parent,
            "permission",
            names,
            t1 + Duration::from_millis(100),
            Transport::Hook,
        );
        tool_use_end(
            &mut r,
            &mut scene,
            parent,
            Some("task-T"),
            t1 + Duration::from_secs(1),
            Transport::Hook,
        );
        let waits = matches!(
            scene.agents[&parent].state,
            ActivityState::RequiresAction { .. }
        );
        assert_eq!(waits, !answered, "{what}");
    }
}

/// A parent that dispatched one Task at `t1`, by hook.
fn dispatching(slug: &str) -> (Reducer, SceneState, AgentId, SystemTime) {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let parent = AgentId::from_parts(DRAIN_INFERRED, slug);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: DRAIN_INFERRED.into(),
            session_id: slug.into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    let t1 = t0 + Duration::from_secs(1);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    (r, scene, parent, t1)
}

/// A drained Task's transcript replay, its start and its end however late,
/// leaves the parent as the drain and anything since left it.
#[test]
fn a_drained_tasks_replayed_pair_leaves_the_parent_as_it_was() {
    for raised in [false, true] {
        let (mut r, mut scene, parent, t1) = dispatching("replayed-pair");
        let drained = t1 + Duration::from_secs(1);
        tool_use_end(
            &mut r,
            &mut scene,
            parent,
            Some("task-T"),
            drained,
            Transport::Hook,
        );
        if raised {
            user_action_request(
                &mut r,
                &mut scene,
                parent,
                "permission",
                drained + Duration::from_secs(1),
                Transport::Hook,
            );
        }
        let was = scene.agents[&parent].clone();
        let late = drained + Duration::from_secs(10 * 60);
        tool_use_start(
            &mut r,
            &mut scene,
            parent,
            Some("task-T"),
            Some("Agent"),
            late,
            Transport::Jsonl,
        );
        tool_use_end(
            &mut r,
            &mut scene,
            parent,
            Some("task-T"),
            late,
            Transport::Jsonl,
        );
        let slot = &scene.agents[&parent];
        assert_eq!(
            (&slot.state, slot.state_started_at),
            (&was.state, was.state_started_at),
            "raised: {raised}"
        );
    }
}

/// A drained Task's refused late start leaves nothing in flight, so the
/// parent's next own call settles it Idle, not back to Delegating.
#[test]
fn a_refused_late_task_start_tracks_nothing_in_flight() {
    let (mut r, mut scene, parent, t1) = dispatching("refused-start");
    let drained = t1 + Duration::from_secs(1);
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        drained,
        Transport::Hook,
    );
    let late = drained + Duration::from_secs(10 * 60);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        late,
        Transport::Jsonl,
    );
    let own = late + Duration::from_secs(1);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        Some("Bash: ls"),
        own,
        Transport::Jsonl,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        own,
        Transport::Jsonl,
    );
    assert_eq!(scene.agents[&parent].state, ActivityState::Idle);
}

/// A drained Task's transcript replay, its start and its end however late,
/// ends nothing the parent shows: its drain already ran.
#[test]
fn a_drained_tasks_replayed_end_leaves_the_parents_own_call_shown() {
    let (mut r, mut scene, parent, t1) = dispatching("replayed-end");
    let drained = t1 + Duration::from_secs(1);
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        drained,
        Transport::Hook,
    );
    let own = drained + Duration::from_millis(100);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        None,
        Some("Bash: ls"),
        own,
        Transport::Jsonl,
    );
    let late = drained + Duration::from_secs(10 * 60);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        late,
        Transport::Jsonl,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        late,
        Transport::Jsonl,
    );
    match &scene.agents[&parent].state {
        ActivityState::Running {
            item: Some(ToolUse { detail, .. }),
        } => assert_eq!(detail.as_deref(), Some("Bash: ls")),
        other => panic!("expected the parent's own call, got {other:?}"),
    }
}

/// A Task's lagging duplicate start keeps the delegating clock its first set.
#[test]
fn a_tasks_duplicate_start_keeps_the_delegating_clock() {
    let (mut r, mut scene, parent, t1) = dispatching("duplicate-start");
    let late = t1 + Duration::from_secs(10 * 60);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        late,
        Transport::Jsonl,
    );
    assert_delegating(&scene, parent, "the Task is still in flight");
    assert_eq!(scene.agents[&parent].state_started_at, t1);
}

#[test]
fn task_drain_keeps_parallel_ordinary_tool_member() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-keep", t0);
    let t1 = t0 + Duration::from_secs(1);

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        Some("Bash: ls"),
        t1 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    // Its own call, as omp's and kimi's raises name it: a keyless raise
    // would bind the sole call pending on the hook, the Task.
    user_action_request_for(
        &mut r,
        &mut scene,
        parent,
        "permission",
        Some("bash-1"),
        t1 + Duration::from_millis(200),
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t1 + Duration::from_secs(1),
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        t1 + Duration::from_secs(2),
        Transport::Jsonl,
    );
    assert_eq!(
        scene.agents.get(&parent).unwrap().state,
        ActivityState::Idle,
        "the kept member must let the ordinary tool's END resolve the RequiresAction"
    );
}

#[test]
fn suppressed_child_event_keeps_parents_own_parallel_tool_member() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, _child) =
        delegating_pair_on(&mut r, &mut scene, "orch-own-gate", t0, UNNAMED_CHILD_HOOKS);
    let t1 = t0 + Duration::from_secs(1);

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        Some("Bash: ls"),
        t1 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    // Its own call, as omp's and kimi's raises name it: a keyless raise
    // would bind the sole call pending on the hook, the Task.
    user_action_request_for(
        &mut r,
        &mut scene,
        parent,
        "permission",
        Some("bash-1"),
        t1 + Duration::from_millis(200),
        Transport::Hook,
    );
    tool_use_start(
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
            ActivityState::RequiresAction { .. }
        ),
        "a suppressed child event must not hide the parent's own still-pending permission RequiresAction"
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        t1 + Duration::from_secs(1),
        Transport::Jsonl,
    );
    assert!(
        !matches!(
            scene.agents.get(&parent).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "the kept member must let the own tool's END resolve the RequiresAction"
    );
}

#[test]
fn own_parallel_tool_end_mid_delegation_returns_parent_to_delegating() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "orch-own-end", t0);
    let t1 = t0 + Duration::from_secs(1);

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        Some("Bash: ls"),
        t1 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("bash-1"),
        t1 + Duration::from_millis(500),
        Transport::Jsonl,
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

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t1,
        Transport::Hook,
    );
    tool_use_start(
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
    assert_delegating(&scene, parent, "a Task still in flight outlives the turn");
    assert!(scene.agents[&child].exiting_at.is_none());
}

/// A settle that lands on Delegating while the slot already delegates is no
/// new state, so its clock runs on.
#[test]
fn a_turn_end_while_delegating_keeps_the_delegating_clock() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-clock", t0);
    let dispatched = t0 + Duration::from_secs(1);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        dispatched,
        Transport::Hook,
    );
    turn_end(
        &mut r,
        &mut scene,
        parent,
        dispatched + Duration::from_secs(5),
        Transport::Hook,
    );
    assert_delegating(&scene, parent, "the Task is still in flight");
    assert_eq!(scene.agents[&parent].state_started_at, dispatched);
}

/// A Task its turn's end left running delegates on outside the later turns'
/// requests, so a keyless prompt still binds the one call of its own turn,
/// whose end answers it.
#[test]
fn a_task_that_outlived_its_turn_binds_no_later_request() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_transcript_path("/p/outlived.jsonl");
    start(&mut r, &mut scene, parent);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let at = |s| t0 + Duration::from_secs(s);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        at(1),
        Transport::Hook,
    );
    turn_end(&mut r, &mut scene, parent, at(2), Transport::Hook);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("c"),
        Some("Bash: ls"),
        at(3),
        Transport::Hook,
    );
    user_action_request(
        &mut r,
        &mut scene,
        parent,
        "permission",
        at(4),
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("c"),
        at(5),
        Transport::Hook,
    );
    assert_delegating(&scene, parent, "c's end answered the prompt");
}

/// The drain settles only a slot showing a Task: a parent showing its own call,
/// named or not, keeps it until that call ends.
#[test]
fn a_task_drain_leaves_the_parents_own_call_shown() {
    for own in [Some("bash-1"), None] {
        let mut scene = SceneState::uniform(8);
        let mut r = Reducer::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-drain-own", t0);
        let t1 = t0 + Duration::from_secs(1);
        tool_use_start(
            &mut r,
            &mut scene,
            parent,
            Some("task-T"),
            Some("Agent"),
            t1,
            Transport::Hook,
        );
        tool_use_start(
            &mut r,
            &mut scene,
            parent,
            own,
            Some("Bash: ls"),
            t1 + Duration::from_millis(100),
            Transport::Jsonl,
        );
        let drained = t1 + Duration::from_secs(1);
        tool_use_end(
            &mut r,
            &mut scene,
            parent,
            Some("task-T"),
            drained,
            Transport::Hook,
        );
        r.tick(&mut scene, drained + Duration::from_secs(2));
        match &scene.agents[&parent].state {
            ActivityState::Running {
                item: Some(ToolUse { detail, .. }),
            } => assert_eq!(detail.as_deref(), Some("Bash: ls"), "{own:?}"),
            other => panic!("expected the parent's own call {own:?}, got {other:?}"),
        }
        tool_use_end(
            &mut r,
            &mut scene,
            parent,
            own,
            drained + Duration::from_secs(3),
            Transport::Jsonl,
        );
        assert_eq!(scene.agents[&parent].state, ActivityState::Idle, "{own:?}");
    }
}

/// The shown Task's drain re-points the slot at a Task still in flight, on the
/// same Delegating clock.
#[test]
fn a_drain_of_the_shown_task_shows_one_still_in_flight() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let parent = AgentId::from_transcript_path("/p/shown.jsonl");
    start(&mut r, &mut scene, parent);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-a"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-b"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    let since = scene.agents[&parent].state_started_at;
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-b"),
        t0 + Duration::from_secs(30),
        Transport::Hook,
    );
    let slot = &scene.agents[&parent];
    match &slot.state {
        ActivityState::Running {
            item: Some(ToolUse { tool_use_id, .. }),
        } => {
            assert_eq!(tool_use_id.as_deref(), Some("task-a"));
        }
        other => panic!("expected Delegating on task-a, got {other:?}"),
    }
    assert_eq!(slot.state_started_at, since);
}

/// A Task dispatched while a prompt holds another call only counts: the slot
/// keeps RequiresAction with the Task tracked, and the prompt's answer lands on
/// Delegating.
#[test]
fn a_task_dispatch_over_a_prompt_holding_another_call_keeps_requires_action() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/dispatch-held.jsonl");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("bash-1"),
        Some("Bash: rm"),
        t0,
        Transport::Hook,
    );
    user_action_request(
        &mut r,
        &mut scene,
        id,
        "permission",
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_millis(200),
        Transport::Hook,
    );
    assert!(
        matches!(
            scene.agents[&id].state,
            ActivityState::RequiresAction { .. }
        ),
        "got {:?}",
        scene.agents[&id].state
    );
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("bash-1"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    assert_delegating(&scene, id, "the answered prompt lands on the tracked Task");
}

/// A Task dispatched while a prompt holds no call answers it.
#[test]
fn a_task_dispatch_answers_a_prompt_holding_no_call() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/dispatch-bare.jsonl");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    assert_delegating(&scene, id, "the dispatch is the agent working again");
}

#[test]
fn late_batched_jsonl_pair_after_delivered_hook_end_is_fully_dropped() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/batched.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t-fast"),
        Some("Read: /x"),
        t0,
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("t-fast"),
        t0 + Duration::from_millis(50),
        Transport::Hook,
    );
    let settled = scene.agents[&id].clone();
    assert_eq!(
        settled.state,
        ActivityState::Idle,
        "the hook END settles it"
    );

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t-fast"),
        Some("Read: /x"),
        t0 + Duration::from_secs(10 * 60),
        Transport::Jsonl,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("t-fast"),
        t0 + Duration::from_secs(10 * 60),
        Transport::Jsonl,
    );

    let slot = &scene.agents[&id];
    assert_eq!(
        (&slot.state, slot.state_started_at),
        (&settled.state, settled.state_started_at),
        "stale JSONL replay must not re-run the ended call"
    );
    assert_eq!(
        slot.tool_call_count, settled.tool_call_count,
        "stale JSONL replay must not double-count the tool"
    );
}

/// A transcript replay of a dispatch already in flight, however late, leaves
/// the parent's pending permission RequiresAction, though the prompt named no
/// call and so takes the agent's next start.
#[test]
fn jsonl_task_start_replay_does_not_clobber_requires_action_parent() {
    for lag in [Duration::from_millis(100), Duration::from_secs(10 * 60)] {
        let mut scene = SceneState::uniform(8);
        let mut r = Reducer::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let (parent, _child) = delegating_pair(&mut r, &mut scene, "orch-wait", t0);
        let t1 = t0 + Duration::from_secs(1);
        // A second call in flight, so the prompt binds neither by deduction.
        for (call, detail) in [("task-T", "Agent"), ("x", "Bash: ls")] {
            tool_use_start(
                &mut r,
                &mut scene,
                parent,
                Some(call),
                Some(detail),
                t1,
                Transport::Hook,
            );
        }
        user_action_request(
            &mut r,
            &mut scene,
            parent,
            "permission",
            t1 + Duration::from_millis(10),
            Transport::Hook,
        );
        tool_use_start(
            &mut r,
            &mut scene,
            parent,
            Some("task-T"),
            Some("Agent"),
            t1 + lag,
            Transport::Jsonl,
        );
        assert!(
            matches!(
                scene.agents.get(&parent).unwrap().state,
                ActivityState::RequiresAction { .. }
            ),
            "{lag:?}: a replayed dispatch clobbered the parent's pending permission"
        );
    }
}

#[test]
fn jsonl_ordinary_tool_end_drains_when_hook_end_drops() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/fastdrop.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t-1"),
        Some("Read: /x"),
        t0,
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("t-1"),
        t0 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    assert_eq!(
        scene.agents[&id].state,
        ActivityState::Idle,
        "the JSONL END is the fallback for the dropped hook END — it must settle the slot"
    );
}

#[test]
fn a_parallel_dispatch_keeps_the_first_drain_from_cascading() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "orch-par", t0);

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-2"),
        Some("Agent"),
        t0 + Duration::from_millis(1_050),
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-1"),
        t0 + Duration::from_millis(1_200),
        Transport::Hook,
    );

    r.tick(
        &mut scene,
        t0 + Duration::from_millis(1_200) + B1_CASCADE_GRACE + Duration::from_millis(10),
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

    tool_use_end(
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

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        t0 + Duration::from_millis(1_100),
        Transport::Jsonl,
    );

    r.tick(
        &mut scene,
        t0 + Duration::from_millis(1_100) + B1_CASCADE_GRACE + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "the JSONL Task self-END must drain tasks_in_flight and fire the b1 cascade (after the #151 grace) — it is the fallback for the dropped hook END"
    );
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("b-1"),
        Some("Bash: ls"),
        t0 + Duration::from_secs(5),
        Transport::Hook,
    );
    match &scene.agents.get(&parent).unwrap().state {
        ActivityState::Running {
            item: Some(ToolUse { detail, .. }),
        } => {
            assert_eq!(
                detail.as_deref(),
                Some("Bash: ls"),
                "suppression must release once the Task drained via JSONL"
            );
        }
        other => panic!("expected Running(Bash: ls), got {other:?}"),
    }
}

#[test]
fn parent_requires_action_on_subagent_permission_resolves_when_the_subagent_resumes() {
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
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    user_action_request(
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
            ActivityState::RequiresAction { .. }
        ),
        "the parent requires action on the subagent's permission"
    );

    tool_use_start(
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
            ActivityState::Running { .. }
        ),
        "parent resumes Running(Delegating) once the subagent works again — no stale RequiresAction"
    );
    assert!(scene.agents.get(&child).unwrap().exiting_at.is_none());
}

#[test]
fn parent_requires_action_on_subagent_permission_resolves_when_the_subagent_ends_a_tool() {
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
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    user_action_request(
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
            ActivityState::RequiresAction { .. }
        ),
        "the parent requires action on the subagent's permission"
    );
    tool_use_end(
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
            ActivityState::Running { .. }
        ),
        "a suppressed child END must ALSO restore Running(Delegating), not leave a stale RequiresAction"
    );
}

#[test]
fn task_drain_while_parent_requires_action_keeps_requires_action() {
    use pixtuoid_core::source::ToolDetail;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/wait.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, id);

    r.apply(
        &mut scene,
        AgentEvent::tool_use_start(id, Some("task-T".into()), Some(ToolDetail::Task)),
        t0,
        Transport::Hook,
    );
    user_action_request(
        &mut r,
        &mut scene,
        id,
        "permission",
        t0 + Duration::from_millis(500),
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents[&id].state,
        ActivityState::RequiresAction { .. }
    ));

    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("task-T"),
        t0 + Duration::from_millis(1000),
        Transport::Hook,
    );
    assert!(
        matches!(
            scene.agents[&id].state,
            ActivityState::RequiresAction { .. }
        ),
        "parent's permission must stay RequiresAction through a Task drain, got {:?}",
        scene.agents[&id].state
    );
}

/// The drain of the Task the parent shows settles it onto the other one in
/// flight, the same state, so its clock keeps running: asserting the state
/// alone would pass a settle that restarts it.
#[test]
fn ending_one_of_two_parallel_tasks_does_not_restart_the_parents_delegating_clock() {
    let mut r = Reducer::new();
    let mut scene = SceneState::uniform(4);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let parent = AgentId::from_transcript_path("/p/parallel.jsonl");
    start(&mut r, &mut scene, parent);

    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-a"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-b"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );
    let armed_at = scene.agents[&parent].state_started_at;

    // Drain the one it shows. The other is still live, so the parent stays
    // Delegating.
    tool_use_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-b"),
        t0 + Duration::from_secs(30),
        Transport::Hook,
    );

    let slot = &scene.agents[&parent];
    assert!(
        matches!(slot.state, ActivityState::Running { .. }),
        "one of two parallel Tasks ending must leave the parent Delegating"
    );
    assert_eq!(
        slot.state_started_at, armed_at,
        "the settle onto the other Task restarted the clock mid-delegation"
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
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0,
        Transport::Hook,
    );
    let own = t0 + Duration::from_secs(1);
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("own"),
        Some("Bash: ls"),
        own,
        Transport::Hook,
    );
    match &scene.agents[&parent].state {
        ActivityState::Running {
            item: Some(ToolUse { detail, .. }),
        } => assert_eq!(detail.as_deref(), Some("Bash: ls")),
        other => panic!("expected the parent's own tool, got {other:?}"),
    }
}

/// A gated Task, raised in the order the `approve-hermetic` recording writes a
/// gated call: its block, then its approval request, then its outcome, all in
/// one transcript. The outcome so meets a Task already in flight.
fn a_raised_task_wait() -> (Reducer, SceneState, AgentId, SystemTime) {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts(HOOK_AND_TRANSCRIPT, "gated-task");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("T"),
        Some("Agent"),
        t0,
        Transport::Jsonl,
    );
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "task",
        Some("T"),
        t0,
        Transport::Jsonl,
    );
    (r, scene, id, t0 + Duration::from_secs(2))
}

/// Though a call of its parent's started after it.
#[test]
fn an_approved_task_call_delegates_again() {
    let (mut r, mut scene, id, answered) = a_raised_task_wait();
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("y"),
        Some("Bash: ls"),
        answered,
        Transport::Jsonl,
    );
    r.apply(
        &mut scene,
        AgentEvent::UserActionResolved {
            agent_id: id,
            by: Resolution::Approved("T".into()),
        },
        answered,
        Transport::Jsonl,
    );
    assert_delegating(&scene, id, "the approval's resume lifts the wait");
}

#[test]
fn a_denied_task_call_lifts_its_wait() {
    let (mut r, mut scene, id, answered) = a_raised_task_wait();
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("T"),
        answered,
        Transport::Jsonl,
    );
    assert!(
        !matches!(
            scene.agents[&id].state,
            ActivityState::RequiresAction { .. }
        ),
        "the denial left the wait standing"
    );
}

/// The same transport's replay of a dispatch already in flight, under a prompt
/// that named no call and so takes the agent's next start: it answers nothing.
#[test]
fn a_replayed_task_start_answers_no_prompt_that_did_not_name_it() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts(HOOK_AND_TRANSCRIPT, "keyless-wait");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let task = |r: &mut Reducer, scene: &mut SceneState, at| {
        tool_use_start(r, scene, id, Some("T"), Some("Agent"), at, Transport::Jsonl);
    };
    task(&mut r, &mut scene, t0);
    // A second call in flight, so the prompt binds neither by deduction.
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x"),
        Some("bash: ls"),
        t0,
        Transport::Jsonl,
    );
    user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Jsonl);
    task(&mut r, &mut scene, t0 + Duration::from_secs(2));
    assert!(matches!(
        scene.agents[&id].state,
        ActivityState::RequiresAction { .. }
    ));
}

/// `payloads` decoded by the real hook decoder and folded in order, a second
/// apart: the parent's activity, and how many calls it counted.
fn fold_hooks(payloads: &[serde_json::Value]) -> (ActivityState, u32) {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut id = None;
    for (n, v) in (0u64..).zip(payloads) {
        for ev in decode_hook_payload(v.clone()).expect("decodes") {
            id.get_or_insert(ev.agent_id());
            r.apply(&mut scene, ev, t0 + Duration::from_secs(n), Transport::Hook);
        }
    }
    let slot = &scene.agents[&id.expect("a payload")];
    (slot.state.clone(), slot.tool_call_count)
}

/// A gated dispatch whose gate is decoded before its start (#242) ends as the
/// start-then-gate order does, on kimi (a keyed PermissionRequest, then its
/// child resuming under the parent's key) and on dsh (`approval_asked`, then
/// the approval).
#[test]
fn a_gated_dispatch_decoded_before_its_start_ends_as_in_order() {
    let kimi = |event: &str, call: &str, tool: &str| {
        json!({"hook_event_name": event, "session_id": "s-kimi", "cwd": "/repo",
               "_pixtuoid_source": "kimi", "tool_name": tool, "tool_input": {},
               "tool_call_id": call})
    };
    let dsh = |ty: &str, extra: serde_json::Value| {
        let mut v = json!({"type": ty, "sessionId": "s-dsh", "cwd": "/repo",
                           "_pixtuoid_source": "dsh"});
        v.as_object_mut()
            .expect("an object")
            .extend(extra.as_object().expect("an object").clone());
        v
    };
    let dsh_call = json!({"callId": "T", "toolName": "subagent"});
    let (mut want, mut got) = (Vec::new(), Vec::new());
    for (what, start, dispatch, gate, after) in [
        (
            "kimi",
            kimi("SessionStart", "", ""),
            kimi("PreToolUse", "T", "Agent"),
            kimi("PermissionRequest", "T", "Agent"),
            kimi("PreToolUse", "c1", "Bash"),
        ),
        (
            "dsh",
            dsh("session_start", json!({})),
            dsh("tool_call", dsh_call.clone()),
            dsh("approval_asked", dsh_call.clone()),
            dsh(
                "approval_decided",
                json!({"callId": "T", "toolName": "subagent", "outcome": "allowed-once"}),
            ),
        ),
    ] {
        let in_order = fold_hooks(&[start.clone(), dispatch.clone(), gate.clone(), after.clone()]);
        assert_eq!(
            in_order.0.clone(),
            ActivityState::Running {
                item: Some(ToolUse {
                    tool_use_id: Some("T".into()),
                    detail: Some("Delegating".into()),
                    kind: pixtuoid_core::state::ToolKind::Task,
                }),
            },
            "{what}: premise"
        );
        want.push((what, in_order));
        got.push((what, fold_hooks(&[start, gate, dispatch, after])));
    }
    assert_eq!(got, want);
}
