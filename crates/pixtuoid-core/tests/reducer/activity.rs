use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::reducer::Reducer;
use pixtuoid_core::state::{ActivityState, SceneState};

use crate::{
    HOOK_AND_TRANSCRIPT, UNNAMED_CHILD_HOOKS, start, start_as, tool_use_end, tool_use_start,
    turn_end, user_action_request,
};

#[test]
fn a_tool_end_settles_the_slot_at_once() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::now();
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );
    let end = t0 + Duration::from_millis(100);
    tool_use_end(&mut r, &mut scene, id, Some("t1"), end, Transport::Hook);

    let slot = &scene.agents[&id];
    assert_eq!(slot.state, ActivityState::Idle);
    assert_eq!(slot.state_started_at, end);
}

/// Parallel calls: the end of one that is not shown leaves the shown one
/// running.
#[test]
fn an_end_of_a_call_not_shown_keeps_the_shown_one() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/par.jsonl");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::now();
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("a"),
        Some("Read: a"),
        t0,
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("b"),
        Some("Read: b"),
        t0,
        Transport::Hook,
    );
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("a"),
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );
    match &scene.agents[&id].state {
        ActivityState::Running { tool_use_id, .. } => assert_eq!(tool_use_id.as_deref(), Some("b")),
        other => panic!("expected b still running, got {other:?}"),
    }
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("b"),
        t0 + Duration::from_millis(200),
        Transport::Hook,
    );
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

/// A request holding no call leaves on the agent's next start; one holding a
/// call leaves on that call, and another call's start only counts.
#[test]
fn a_start_answers_only_a_request_holding_no_other_call() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/note.jsonl");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::now();
    user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        Some("Bash: ls"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents[&id].state,
        ActivityState::Running { .. }
    ));

    let t2 = t0 + Duration::from_secs(2);
    user_action_request(&mut r, &mut scene, id, "permission", t2, Transport::Hook);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t2"),
        Some("Bash: pwd"),
        t2 + Duration::from_secs(1),
        Transport::Hook,
    );
    let slot = &scene.agents[&id];
    assert!(
        matches!(slot.state, ActivityState::RequiresAction { .. }),
        "t1 holds the request, got {:?}",
        slot.state
    );
    assert_eq!(slot.tool_call_count, 2, "t2 still counts");
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        t2 + Duration::from_secs(2),
        Transport::Hook,
    );
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

/// The transcript ends a turn it may have read before a hook's fresh prompt,
/// so its turn end answers only a prompt the transcript raised.
#[test]
fn a_jsonl_turn_end_resolves_only_a_transcript_raised_wait() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for (raised_on, resolves) in [(Transport::Jsonl, true), (Transport::Hook, false)] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let id = AgentId::from_parts("codex", "sess-1");
        start_as(&mut r, &mut scene, id, "codex");
        user_action_request(&mut r, &mut scene, id, "permission", t0, raised_on);
        turn_end(
            &mut r,
            &mut scene,
            id,
            t0 + Duration::from_millis(200),
            Transport::Jsonl,
        );
        assert_eq!(
            scene.agents[&id].state == ActivityState::Idle,
            resolves,
            "raised on {raised_on:?}: got {:?}",
            scene.agents[&id].state
        );
    }
}

#[test]
fn a_user_action_request_without_a_reason_leaves_the_state_bare() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start(&mut r, &mut scene, id);

    r.apply(
        &mut scene,
        AgentEvent::UserActionRequest {
            agent_id: id,
            reason: None,
            tool_use_id: None,
        },
        SystemTime::now(),
        Transport::Hook,
    );

    assert_eq!(
        scene.agents[&id].state,
        ActivityState::RequiresAction { reason: None }
    );
}

#[test]
fn user_action_request_sets_state_with_reason() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start(&mut r, &mut scene, id);

    user_action_request(
        &mut r,
        &mut scene,
        id,
        "Bash: rm -rf?",
        SystemTime::now(),
        Transport::Hook,
    );

    match &scene.agents.get(&id).unwrap().state {
        ActivityState::RequiresAction { reason } => {
            assert_eq!(reason.as_deref(), Some("Bash: rm -rf?"))
        }
        other => panic!("unexpected state: {other:?}"),
    }
}

#[test]
fn tool_call_count_increments_on_tool_use_start() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/stats.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, id);

    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 0);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 1);

    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        t0 + Duration::from_millis(500),
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t2"),
        None,
        t0 + Duration::from_millis(600),
        Transport::Hook,
    );
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 2);
}

#[test]
fn running_ms_does_not_double_count_on_duplicate_tool_use_end() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/dedup.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );

    let t1 = t0 + Duration::from_secs(2);
    tool_use_end(&mut r, &mut scene, id, Some("t1"), t1, Transport::Hook);
    // The 2nd end is deliberately PAST the hook-wins window, so it is NOT deduped.
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        t1 + Duration::from_millis(600),
        Transport::Jsonl,
    );

    r.tick(&mut scene, t1 + Duration::from_secs(3));
    let slot = scene.agents.get(&id).unwrap();
    // A second folded span would read ~4600.
    assert_eq!(
        slot.running_ms, 2000,
        "running_ms should be the single t0→t1 span (2000ms), not double-counted"
    );
}

#[test]
fn running_ms_preserved_when_task_arrives_during_running_tool() {
    use pixtuoid_core::source::ToolDetail;

    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/task-active.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, id);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );

    let t1 = t0 + Duration::from_secs(2);
    r.apply(
        &mut scene,
        AgentEvent::ToolUseStart {
            agent_id: id,
            tool_use_id: Some("task-1".into()),
            detail: Some(ToolDetail::Task),
        },
        t1,
        Transport::Hook,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert!(
        slot.running_ms >= 2000,
        "expected >= 2000ms running from pre-Task tool span, got {}",
        slot.running_ms
    );
}

#[test]
fn running_ms_preserved_when_requires_action_interrupts_running() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/waiting.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, id);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );

    let t1 = t0 + Duration::from_secs(3);
    user_action_request(&mut r, &mut scene, id, "permission", t1, Transport::Hook);

    let slot = scene.agents.get(&id).unwrap();
    assert!(
        slot.running_ms >= 3000,
        "expected >= 3000ms running before RequiresAction, got {}",
        slot.running_ms
    );
}

// CC wire fact (captured live): when a permission Notification fires mid-tool
// (PreToolUse(t1) → Notification → PostToolUse(t1)), the member call's
// ToolUseEnd carries the same tool_use_id that was Running when RequiresAction began —
// so PostToolUse(t1) means the permission was granted and the RequiresAction resolved.
#[test]
fn a_member_end_while_requires_action_resolves_to_idle() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/wait.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, id);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
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

    let end = t0 + Duration::from_millis(1000);
    tool_use_end(&mut r, &mut scene, id, Some("t1"), end, Transport::Hook);
    assert!(
        matches!(scene.agents.get(&id).unwrap().state, ActivityState::Idle),
        "resolved permission must settle to Idle, got {:?}",
        scene.agents.get(&id).unwrap().state
    );
}

/// CC's recorded prompt fires PermissionRequest (no tool_use_id) and then, if the
/// prompt sits, the idle Notification — both decode to UserActionRequest. The second one
/// must not erase the tool the first remembered, or the approved tool's
/// PostToolUse resolves nothing and the slot never leaves RequiresAction.
#[test]
fn a_second_user_action_request_keeps_the_member_the_first_one_remembered() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/gate.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, id);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
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
    user_action_request(
        &mut r,
        &mut scene,
        id,
        "Claude is waiting for your input",
        t0 + Duration::from_millis(900),
        Transport::Hook,
    );

    let end = t0 + Duration::from_millis(1200);
    tool_use_end(&mut r, &mut scene, id, Some("t1"), end, Transport::Hook);
    assert!(
        matches!(scene.agents.get(&id).unwrap().state, ActivityState::Idle),
        "the approved tool's end must still resolve the wait, got {:?}",
        scene.agents.get(&id).unwrap().state
    );
}

#[test]
fn parallel_tool_end_while_requires_action_keeps_requires_action() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/wait.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, id);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
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

    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("t2"),
        t0 + Duration::from_millis(1000),
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(
        matches!(slot.state, ActivityState::RequiresAction { .. }),
        "parallel tool end must keep RequiresAction, got {:?}",
        slot.state
    );
}

// An approval prompt BLOCKS a Codex/Reasonix turn, so a hook `Stop` arriving
// while RequiresAction means the prompt was already resolved upstream — and Reasonix has
// no second transport to self-heal a stale one.
#[test]
fn turn_end_stop_hook_resolves_stale_requires_action() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("reasonix", "/Users/dev/proj");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start_as(&mut r, &mut scene, id, "reasonix");
    user_action_request(
        &mut r,
        &mut scene,
        id,
        "approval needed: bash rm -rf ./build",
        t0,
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents.get(&id).unwrap().state,
        ActivityState::RequiresAction { .. }
    ));

    let end = t0 + Duration::from_millis(800);
    turn_end(&mut r, &mut scene, id, end, Transport::Hook);
    assert!(
        matches!(scene.agents.get(&id).unwrap().state, ActivityState::Idle),
        "turn-end Stop must resolve the stale RequiresAction to Idle, got {:?}",
        scene.agents.get(&id).unwrap().state
    );
}

// Reasonix's prompt and its PostToolUse both name no call.
#[test]
fn a_hook_tool_end_naming_no_call_resolves_a_wait_naming_none() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("reasonix", "/Users/dev/proj");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start_as(&mut r, &mut scene, id, "reasonix");
    user_action_request(&mut r, &mut scene, id, "approval", t0, Transport::Hook);
    let end = t0 + Duration::from_millis(800);
    tool_use_end(&mut r, &mut scene, id, None, end, Transport::Hook);
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

// kimi kills a still-running PostToolUse(Failure) hook when its turn ends, so a
// call may never get its own End; the turn's Stop must still settle the slot.
#[test]
fn a_turn_end_settles_a_call_whose_own_end_never_came() {
    use crate::user_action_request_for;
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for requested in [false, true] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let id = AgentId::from_parts("kimi", "session_abc");
        start_as(&mut r, &mut scene, id, "kimi");
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some("call_1"),
            None,
            t0,
            Transport::Hook,
        );
        if requested {
            user_action_request_for(
                &mut r,
                &mut scene,
                id,
                "permission",
                Some("call_1"),
                t0,
                Transport::Hook,
            );
        }
        let stop = t0 + Duration::from_millis(800);
        turn_end(&mut r, &mut scene, id, stop, Transport::Hook);
        assert_eq!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::Idle,
            "requested: {requested}"
        );
    }
}

// A transcript line can race in just after a fresh hook PermissionRequest, so
// only a HOOK end naming no call may resolve a RequiresAction.
#[test]
fn jsonl_none_id_end_while_requires_action_keeps_requires_action() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "sess-1");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start_as(&mut r, &mut scene, id, "claude-code");
    user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        None,
        t0 + Duration::from_millis(200),
        Transport::Jsonl,
    );
    assert!(
        matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "a racing transcript tool end naming no call must keep the permission prompt up"
    );
}

#[test]
fn codex_permission_then_jsonl_output_resumes_to_running() {
    // The hook UserActionRequest and the JSONL resume coalesce on the session UUID.
    let mut reducer = Reducer::new();
    let mut scene = SceneState::uniform(4);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    let uuid = "019e7762-9ded-7e33-be41-946ecf105bf4";
    let id = AgentId::from_parts("codex", uuid);

    reducer.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "codex".into(),
            session_id: uuid.into(),
            cwd: PathBuf::from("/Users/me/dotfiles"),
            parent_id: None,
        },
        now,
        Transport::Hook,
    );

    user_action_request(
        &mut reducer,
        &mut scene,
        id,
        "permission",
        now,
        Transport::Hook,
    );
    assert!(
        matches!(
            scene.agents[&id].state,
            ActivityState::RequiresAction { .. }
        ),
        "should be RequiresAction on permission"
    );

    tool_use_start(
        &mut reducer,
        &mut scene,
        id,
        None,
        Some("exec_command"),
        now,
        Transport::Jsonl,
    );
    assert!(
        matches!(scene.agents[&id].state, ActivityState::Running { .. }),
        "resume must return to Running"
    );
}

// An end naming no call that answers a wait leaves no request behind, so its
// member call's late transcript Start is plain work, not a re-observed approval. A
// tool end naming no call answers only a wait that named none either.
#[test]
fn an_end_naming_no_call_that_answers_a_wait_closes_its_request() {
    use crate::user_action_request_for;
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for turn in [true, false] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let id = AgentId::from_parts(HOOK_AND_TRANSCRIPT, "sess-1");
        start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
        if turn {
            user_action_request_for(
                &mut r,
                &mut scene,
                id,
                "permission",
                Some("call_1"),
                t0,
                Transport::Hook,
            );
        } else {
            // Outside the dedup window, so its transcript twin below lands.
            tool_use_start(
                &mut r,
                &mut scene,
                id,
                Some("call_1"),
                None,
                t0 - HOOK_WINS_WINDOW,
                Transport::Hook,
            );
            user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
        }
        let end = t0 + Duration::from_millis(200);
        if turn {
            turn_end(&mut r, &mut scene, id, end, Transport::Hook);
        } else {
            tool_use_end(&mut r, &mut scene, id, None, end, Transport::Hook);
        }
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some("call_1"),
            Some("Bash: ls"),
            end + Duration::from_millis(100),
            Transport::Jsonl,
        );
        assert!(
            matches!(scene.agents[&id].state, ActivityState::Running { .. }),
            "turn: {turn}, got {:?}",
            scene.agents[&id].state
        );
    }
}

// Line shapes follow a captured omp ask round, sanitized.
#[test]
fn omp_ask_round_waits_then_answer_clears_through_the_reducer() {
    use pixtuoid_core::source::omp::decode_omp_line;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let path = "/h/.omp/agent/sessions/-p/2026-07-05T20-37-08-710Z_01000000-0000-7000-8000-000000000001.jsonl";
    let id = AgentId::from_parts(
        "omp",
        "2026-07-05T20-37-08-710Z_01000000-0000-7000-8000-000000000001",
    );
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let feed = |r: &mut Reducer, scene: &mut SceneState, line: &str, at: SystemTime| {
        for ev in decode_omp_line(path, "omp", serde_json::from_str(line).unwrap()) {
            r.apply(scene, ev, at, Transport::Jsonl);
        }
    };
    feed(
        &mut r,
        &mut scene,
        r#"{"type":"session","version":3,"id":"01000000-0000-7000-8000-000000000001","timestamp":"2026-07-05T20:37:08.710Z","cwd":"/repo"}"#,
        t0,
    );
    feed(
        &mut r,
        &mut scene,
        r#"{"type":"message","id":"f689ad6b","parentId":"71a1e3cb","timestamp":"2026-07-05T20:39:46.078Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"toolu_01ASKASKASKASKASKASKASKA","name":"ask","arguments":{"i":"Resolving packages/ui collision","questions":[{"id":"ui_collision","question":"packages/ui already exists. What should happen?","options":[{"label":"Replace"},{"label":"Merge"}],"recommended":1}]}}],"stopReason":"toolUse","timestamp":1783283963575}}"#,
        t0 + Duration::from_millis(100),
    );
    assert!(
        matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "the ask toolCall should park the slot RequiresAction"
    );
    // The user answers: the ask's toolResult (same toolCallId) arrives.
    feed(
        &mut r,
        &mut scene,
        r#"{"type":"message","id":"4a4176db","parentId":"98404700","timestamp":"2026-07-05T20:40:39.589Z","message":{"role":"toolResult","toolCallId":"toolu_01ASKASKASKASKASKASKASKA","toolName":"ask","content":[{"type":"text","text":"User selected: Merge"}],"isError":false,"timestamp":1783284039588}}"#,
        t0 + Duration::from_millis(500),
    );
    assert!(
        !matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "the answered ask must clear RequiresAction (else the sprite hangs 60 min)"
    );
}

// Line shapes follow agy's `ask-question` recording.
#[test]
fn an_answered_agy_question_clears_requires_action_on_either_transport() {
    use pixtuoid_core::source::antigravity::{SOURCE_NAME, decode_ag_line};
    use pixtuoid_core::source::decoder::decode_hook_payload;
    use serde_json::{Value, json};
    const TRANSCRIPT: &str =
        "/h/.gemini/antigravity-cli/brain/c1/.system_generated/logs/transcript.jsonl";
    fn feed(r: &mut Reducer, scene: &mut SceneState, wire: &[(Value, Transport)], at: SystemTime) {
        for (v, tp) in wire {
            let events = match tp {
                Transport::Hook => decode_hook_payload(v.clone()).expect("agy's hook decodes"),
                _ => decode_ag_line(TRANSCRIPT, SOURCE_NAME, v.clone()),
            };
            for ev in events {
                r.apply(scene, ev, at, *tp);
            }
        }
    }
    let hook = |event: &str| {
        (
            json!({
                "_pixtuoid_source": SOURCE_NAME, "_pixtuoid_event": event,
                "conversationId": "c1", "workspacePaths": ["/repo"], "stepIdx": 2,
                "toolCall": { "name": "ask_question", "args": {} },
            }),
            Transport::Hook,
        )
    };
    let asked = (
        json!({ "step_index": 1, "type": "PLANNER_RESPONSE",
                "tool_calls": [{ "name": "ask_question", "args": {} }] }),
        Transport::Jsonl,
    );
    let answered = (
        json!({ "step_index": 2, "type": "GENERIC" }),
        Transport::Jsonl,
    );
    let id = AgentId::from_parts(SOURCE_NAME, "c1");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let t1 = t0 + Duration::from_secs(5);
    for (what, asks, answers) in [
        ("transcript", vec![asked.clone()], vec![answered.clone()]),
        ("hooks", vec![hook("PreToolUse")], vec![hook("PostToolUse")]),
        (
            "both",
            vec![asked.clone(), hook("PreToolUse")],
            vec![hook("PostToolUse"), answered.clone()],
        ),
        (
            "both, reversed",
            vec![hook("PreToolUse"), asked.clone()],
            vec![answered.clone(), hook("PostToolUse")],
        ),
    ] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        start_as(&mut r, &mut scene, id, SOURCE_NAME);
        feed(&mut r, &mut scene, &asks, t0);
        assert!(
            matches!(
                scene.agents[&id].state,
                ActivityState::RequiresAction { .. }
            ),
            "{what}: the question parks the slot RequiresAction"
        );
        feed(&mut r, &mut scene, &answers, t1);
        assert!(
            matches!(scene.agents[&id].state, ActivityState::Idle),
            "{what}: the answer clears RequiresAction before the turn ends"
        );
    }
    // Both sides of the window the hook End is remembered for.
    let inside = HOOK_WINS_WINDOW / 10;
    let past = HOOK_WINS_WINDOW + Duration::from_millis(1);
    for (what, late, after, parks) in [
        ("hook", hook("PreToolUse"), inside, false),
        ("transcript", asked.clone(), inside, false),
        ("hook", hook("PreToolUse"), past, true),
        ("transcript", asked, past, true),
    ] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        start_as(&mut r, &mut scene, id, SOURCE_NAME);
        feed(&mut r, &mut scene, &[hook("PostToolUse")], t0);
        feed(&mut r, &mut scene, &[late], t0 + after);
        assert_eq!(
            matches!(
                scene.agents[&id].state,
                ActivityState::RequiresAction { .. }
            ),
            parks,
            "{what}: a wait {after:?} after its call's hook End parks the slot only past the window"
        );
    }
}

// ---------------------------------------------------------------------------
// omp extension-bridge approval rounds (#951): the interleavings human approval
// latency makes reachable past `HOOK_WINS_WINDOW` (`counted_calls`' own doc) —
// each must bind the wait to the member call id and count it ONCE.
// ---------------------------------------------------------------------------

use crate::user_action_request_for;
use pixtuoid_core::state::reducer::HOOK_WINS_WINDOW;

/// Past the dedup window: the cross-transport pair separated by a human.
fn beyond_dedup(t: SystemTime) -> SystemTime {
    t + HOOK_WINS_WINDOW + Duration::from_millis(100)
}

#[test]
fn approval_transcript_start_first_requires_action_then_resumes_running_counting_once() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: rm"),
        t0,
        Transport::Jsonl,
    );
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 1);

    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        beyond_dedup(t0),
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents.get(&id).unwrap().state,
        ActivityState::RequiresAction { .. }
    ));

    // The approval resume.
    let t2 = beyond_dedup(beyond_dedup(t0)) + Duration::from_secs(5);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        None,
        t2,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(
        matches!(slot.state, ActivityState::Running { .. }),
        "approval resume returns the slot to Running"
    );
    assert_eq!(slot.tool_call_count, 1, "the resume must not re-count x1");
}

#[test]
fn approval_hook_user_action_request_first_survives_a_late_transcript_start() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/b.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t0,
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents.get(&id).unwrap().state,
        ActivityState::RequiresAction { .. }
    ));

    // The transcript's Start for the SAME member call, while the human decides.
    let t1 = beyond_dedup(t0);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: rm"),
        t1,
        Transport::Jsonl,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(
        matches!(slot.state, ActivityState::RequiresAction { .. }),
        "a member call's own transcript Start must not fake an approval"
    );
    assert_eq!(slot.tool_call_count, 1);

    let t2 = beyond_dedup(t1) + Duration::from_secs(5);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        None,
        t2,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(matches!(slot.state, ActivityState::Running { .. }));
    assert_eq!(slot.tool_call_count, 1, "still one logical call");
}

#[test]
fn approval_resume_before_the_transcript_start_counts_once() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/c.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t0,
        Transport::Hook,
    );
    let t1 = beyond_dedup(t0) + Duration::from_secs(5);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        None,
        t1,
        Transport::Hook,
    );
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 1);

    // The transcript's Start straggles in past the dedup window.
    let t2 = beyond_dedup(t1);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: rm"),
        t2,
        Transport::Jsonl,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(
        slot.tool_call_count, 1,
        "the straggler must not re-count x1"
    );
    assert!(matches!(slot.state, ActivityState::Running { .. }));
}

#[test]
fn denied_approval_clears_requires_action_via_the_hook_end() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/d.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: rm"),
        t0,
        Transport::Jsonl,
    );
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        beyond_dedup(t0),
        Transport::Hook,
    );

    let t2 = beyond_dedup(beyond_dedup(t0)) + Duration::from_secs(5);
    tool_use_end(&mut r, &mut scene, id, Some("x1"), t2, Transport::Hook);
    assert!(
        !matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "a denial must clear the RequiresAction (else the sprite hangs 60 min)"
    );

    // The transcript's error toolResult follows; it must not disturb anything.
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        beyond_dedup(t2),
        Transport::Jsonl,
    );
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 1);
}

#[test]
fn parallel_approvals_resolve_member_by_member() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/e.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: a"),
        t0,
        Transport::Jsonl,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x2"),
        Some("bash: b"),
        t0,
        Transport::Jsonl,
    );
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 2);

    let t1 = beyond_dedup(t0);
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t1,
        Transport::Hook,
    );
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t1,
        Transport::Hook,
    );

    // Resolving x1 resumes work; x2's membership must survive it.
    let t2 = beyond_dedup(t1) + Duration::from_secs(2);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        None,
        t2,
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents.get(&id).unwrap().state,
        ActivityState::Running { .. }
    ));

    // omp re-raises the still-pending request; its resolve must work identically.
    let t3 = t2 + Duration::from_secs(1);
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t3,
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents.get(&id).unwrap().state,
        ActivityState::RequiresAction { .. }
    ));
    let t4 = beyond_dedup(t3) + Duration::from_secs(2);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x2"),
        None,
        t4,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(matches!(slot.state, ActivityState::Running { .. }));
    assert_eq!(
        slot.tool_call_count, 2,
        "two logical calls, each counted once"
    );
}

#[test]
fn a_delegating_parents_own_approval_round_survives_subagent_suppression() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "f");
    start_as(&mut r, &mut scene, id, UNNAMED_CHILD_HOOKS);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    // A Task in flight arms `suppress_subagent_leak` for this id.
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("task-1"),
        Some("Agent"),
        t0,
        Transport::Jsonl,
    );

    // The parent's OWN member tool (parallel to the Task), then its approval.
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: rm"),
        t0,
        Transport::Jsonl,
    );
    let t1 = beyond_dedup(t0);
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t1,
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents.get(&id).unwrap().state,
        ActivityState::RequiresAction { .. }
    ));

    let t2 = beyond_dedup(t1) + Duration::from_secs(5);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        None,
        t2,
        Transport::Hook,
    );
    assert!(
        !matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "the approval resume must not be eaten as a subagent leak"
    );
    let count_after_resume = scene.agents.get(&id).unwrap().tool_call_count;

    // Denial path on a second member call: the hook End must clear the wait too.
    let t3 = beyond_dedup(t2);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x2"),
        Some("bash: b"),
        t3,
        Transport::Jsonl,
    );
    let t4 = beyond_dedup(t3);
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t4,
        Transport::Hook,
    );
    let t5 = beyond_dedup(t4) + Duration::from_secs(5);
    tool_use_end(&mut r, &mut scene, id, Some("x2"), t5, Transport::Hook);
    assert!(
        !matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "the denial End must not be eaten as a subagent leak"
    );
    assert_eq!(
        scene.agents.get(&id).unwrap().tool_call_count,
        count_after_resume + 1
    );
}

#[test]
fn a_denial_resolving_one_member_keeps_the_queued_siblings() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/g.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: a"),
        t0,
        Transport::Jsonl,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x2"),
        Some("bash: b"),
        t0,
        Transport::Jsonl,
    );
    let t1 = beyond_dedup(t0);
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t1,
        Transport::Hook,
    );
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t1,
        Transport::Hook,
    );

    // Denying x1 must not forget x2's still-pending membership.
    let t2 = beyond_dedup(t1) + Duration::from_secs(3);
    tool_use_end(&mut r, &mut scene, id, Some("x1"), t2, Transport::Hook);
    let t3 = t2 + Duration::from_secs(1);
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t3,
        Transport::Hook,
    );
    let t4 = beyond_dedup(t3) + Duration::from_secs(3);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x2"),
        None,
        t4,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(
        matches!(slot.state, ActivityState::Running { .. }),
        "x2's resume must still read as its approval round"
    );
    assert_eq!(
        slot.tool_call_count, 2,
        "the denial round re-counted a call"
    );
}

#[test]
fn an_already_resolved_calls_late_transcript_start_keeps_a_sibling_member() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/h.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    // x1's whole round runs hook-first; its transcript Start straggles in
    // AFTER x1 resolved, while x2's approval is still pending.
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t0,
        Transport::Hook,
    );
    let t1 = beyond_dedup(t0) + Duration::from_secs(2);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        None,
        t1,
        Transport::Hook,
    );
    let t2 = t1 + Duration::from_secs(1);
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t2,
        Transport::Hook,
    );

    let t3 = beyond_dedup(t2);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: a"),
        t3,
        Transport::Jsonl,
    );
    // The straggler must not have wiped x2's membership.
    let t4 = beyond_dedup(t3) + Duration::from_secs(2);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x2"),
        None,
        t4,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(matches!(slot.state, ActivityState::Running { .. }));
    assert_eq!(
        slot.tool_call_count, 2,
        "x1 once (resume), x2 once (resume) — the straggler added nothing"
    );
}

#[test]
fn an_auto_approved_parallel_tool_does_not_strip_a_pending_member() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/i.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t0,
        Transport::Hook,
    );
    // A rules-approved sibling runs while x1 still awaits the human.
    let t1 = beyond_dedup(t0);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("y"),
        Some("read: notes"),
        t1,
        Transport::Jsonl,
    );
    // x1's denial must still resolve the (re-raised) wait as a member's End.
    let t2 = beyond_dedup(t1);
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t2,
        Transport::Hook,
    );
    let t3 = beyond_dedup(t2) + Duration::from_secs(2);
    tool_use_end(&mut r, &mut scene, id, Some("x1"), t3, Transport::Hook);
    assert!(
        !matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "y's start stripped x1's membership — the denial could no longer resolve it"
    );
}

/// Hook-first ordering: the transcript's richer detail lands while the slot
/// Waits and is deliberately dropped, so the resume re-labels from the wire's
/// toolName — a held transcript start's documented label-only cost.
#[test]
fn the_approval_resume_labels_from_the_wire_tool_name() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/j.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t0,
        Transport::Hook,
    );
    let t1 = beyond_dedup(t0);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: rm -rf /tmp/x"),
        t1,
        Transport::Jsonl,
    );
    let t2 = beyond_dedup(t1) + Duration::from_secs(2);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash"),
        t2,
        Transport::Hook,
    );
    match &scene.agents.get(&id).unwrap().state {
        ActivityState::Running { detail, .. } => {
            assert_eq!(detail.as_deref(), Some("bash"));
        }
        other => panic!("expected Running after the resume, got {other:?}"),
    }
}

/// The CC shape, not omp's: a hook Start whose JSONL twin lands past
/// `HOOK_WINS_WINDOW` (an FSEvents-coalesced poll). Before `counted_calls` this
/// double-incremented the HUD count.
#[test]
fn a_transcript_twin_past_the_dedup_window_counts_once() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/k.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        Some("Edit: a.rs"),
        t0,
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        Some("Edit: a.rs"),
        beyond_dedup(t0),
        Transport::Jsonl,
    );
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 1);
}
