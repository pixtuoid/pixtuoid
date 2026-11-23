use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, RequestKeys, Transport};
use pixtuoid_core::state::reducer::Reducer;
use pixtuoid_core::state::{ActivityState, SceneState, ToolKind, ToolUse};

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
        ActivityState::Running {
            item: Some(ToolUse { tool_use_id, .. }),
        } => assert_eq!(tool_use_id.as_deref(), Some("b")),
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
    assert!(
        matches!(
            &scene.agents[&id].state,
            ActivityState::Running {
                item: Some(ToolUse { tool_use_id: Some(t), .. })
            } if &**t == "t2"
        ),
        "t2 still runs: {:?}",
        scene.agents[&id].state
    );
    tool_use_end(
        &mut r,
        &mut scene,
        id,
        Some("t2"),
        t2 + Duration::from_secs(3),
        Transport::Hook,
    );
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

#[test]
fn a_user_action_request_without_a_reason_leaves_the_state_bare() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start(&mut r, &mut scene, id);

    r.apply(
        &mut scene,
        AgentEvent::user_action_request(id, None, RequestKeys::default()),
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
        AgentEvent::tool_use_start(id, Some("task-1".into()), Some(ToolDetail::Task)),
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

// A keyless prompt raised while one call is pending on its transport binds
// that call (CC: PreToolUse(t1) → Notification → PostToolUse(t1)), so the call's
// end answers it.
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

/// CC's recorded prompt fires PermissionRequest (naming its tool) and then, if
/// the prompt sits, the idle Notification (naming nothing): a second raise with
/// no key keeps the calls the first bound, or the approved tool's PostToolUse
/// resolves nothing and the slot never leaves RequiresAction.
#[test]
fn a_keyless_second_raise_keeps_the_calls_the_first_bound() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/gate.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, id);
    r.apply(
        &mut scene,
        AgentEvent::ToolUsePending {
            agent_id: id,
            tool_use_id: "t1".into(),
            tool_name: Some("Bash".into()),
            turn: None,
        },
        t0,
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        Some("Bash: ls"),
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::user_action_request(id, Some("permission".into()), {
            let mut keys = RequestKeys::default();
            keys.tool_name = Some("Bash".into());
            keys
        }),
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

// Line shapes follow the recorded ask round (`omp/fixtures/ask-hermetic`), sanitized.
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
        r#"{"type":"message","id":"f689ad6b","parentId":"71a1e3cb","timestamp":"2026-07-05T20:39:46.078Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"toolu_01ASKASKASKASKASKASKASKA","name":"ask","arguments":{"i":"Resolving packages/ui collision","html":"<form><p>packages/ui already exists.</p><label><input type=\"radio\" name=\"ui\" value=\"replace\"> Replace</label><label><input type=\"radio\" name=\"ui\" value=\"merge\" checked> Merge</label></form>"}}],"stopReason":"toolUse","timestamp":1783283963575}}"#,
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
        r#"{"type":"message","id":"4a4176db","parentId":"98404700","timestamp":"2026-07-05T20:40:39.589Z","message":{"role":"toolResult","toolCallId":"toolu_01ASKASKASKASKASKASKASKA","toolName":"ask","content":[{"type":"text","text":"User answers:\n{\n  \"ui\": \"merge\"\n}"}],"isError":false,"timestamp":1783284039588}}"#,
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

/// An ask that precedes a sibling call in one message: the sibling's start
/// only counts while the ask stands, and once the ask is answered the slot
/// shows the sibling, still running, until its own result (CodeRabbit on
/// #1940, omp.rs's ask arm).
#[test]
fn an_answered_omp_ask_shows_the_sibling_call_still_running() {
    use pixtuoid_core::source::omp::decode_omp_line;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let path = "/h/.omp/agent/sessions/-p/2026-07-05T20-37-08-710Z_01000000-0000-7000-8000-000000000002.jsonl";
    let id = AgentId::from_parts(
        "omp",
        "2026-07-05T20-37-08-710Z_01000000-0000-7000-8000-000000000002",
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
        r#"{"type":"session","version":3,"id":"01000000-0000-7000-8000-000000000002","timestamp":"2026-07-05T20:37:08.710Z","cwd":"/repo"}"#,
        t0,
    );
    feed(
        &mut r,
        &mut scene,
        r#"{"type":"message","id":"mB","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[{"type":"toolCall","id":"tool_ASK5","name":"ask","arguments":{"i":"Confirming scope"}},{"type":"toolCall","id":"t7","name":"bash","arguments":{"command":"cargo check"}}],"timestamp":1}}"#,
        t0,
    );
    assert!(requires_action(&scene, id), "the ask stands");
    feed(
        &mut r,
        &mut scene,
        r#"{"type":"message","id":"mC","parentId":"mB","timestamp":"t","message":{"role":"toolResult","toolCallId":"tool_ASK5","toolName":"ask","content":[],"isError":false,"timestamp":2}}"#,
        t0 + Duration::from_secs(5),
    );
    assert!(
        matches!(
            &scene.agents[&id].state,
            ActivityState::Running {
                item: Some(ToolUse { tool_use_id: Some(t), .. })
            } if &**t == "t7"
        ),
        "the answered ask shows the sibling call still running: {:?}",
        scene.agents[&id].state
    );
    feed(
        &mut r,
        &mut scene,
        r#"{"type":"message","id":"mD","parentId":"mC","timestamp":"t","message":{"role":"toolResult","toolCallId":"t7","toolName":"bash","content":[],"isError":false,"timestamp":3}}"#,
        t0 + Duration::from_secs(9),
    );
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

/// claude-code's parallel calls under a prompt: the gated call's answer shows
/// the call still running beside it.
#[test]
fn an_answered_cc_prompt_shows_the_parallel_call_still_running() {
    use pixtuoid_core::source::decoder::decode_hook_payload;
    use serde_json::json;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let hook = |r: &mut Reducer, scene: &mut SceneState, v: serde_json::Value| {
        let mut v = v;
        v["_pixtuoid_source"] = json!("claude-code");
        v["session_id"] = json!("ses_par");
        v["cwd"] = json!("/repo");
        for ev in decode_hook_payload(v).expect("decodes") {
            r.apply(scene, ev, t0, Transport::Hook);
        }
    };
    let pre = |tool: &str, call: &str| {
        json!({"hook_event_name": "PreToolUse", "tool_name": tool,
               "tool_input": {"command": "ls"}, "tool_use_id": call})
    };
    hook(&mut r, &mut scene, pre("Bash", "tA"));
    hook(&mut r, &mut scene, pre("Read", "tB"));
    hook(
        &mut r,
        &mut scene,
        json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash",
               "tool_input": {"command": "ls"}}),
    );
    let id = *scene.agents.keys().next().expect("the session registered");
    assert!(requires_action(&scene, id), "the Bash call is gated");
    hook(
        &mut r,
        &mut scene,
        json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_use_id": "tA"}),
    );
    assert!(
        matches!(
            &scene.agents[&id].state,
            ActivityState::Running {
                item: Some(ToolUse { tool_use_id: Some(t), .. })
            } if &**t == "tB"
        ),
        "the answered prompt shows the parallel Read still running: {:?}",
        scene.agents[&id].state
    );
    hook(
        &mut r,
        &mut scene,
        json!({"hook_event_name": "PostToolUse", "tool_name": "Read", "tool_use_id": "tB"}),
    );
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
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
    // A wait landing after its call's hook End, however late, parks nothing.
    for (what, late) in [("hook", hook("PreToolUse")), ("transcript", asked)] {
        for after in [Duration::from_millis(50), Duration::from_secs(10 * 60)] {
            let mut scene = SceneState::uniform(4);
            let mut r = Reducer::new();
            start_as(&mut r, &mut scene, id, SOURCE_NAME);
            feed(&mut r, &mut scene, &[hook("PostToolUse")], t0);
            feed(&mut r, &mut scene, std::slice::from_ref(&late), t0 + after);
            assert!(
                !requires_action(&scene, id),
                "{what}: a wait {after:?} after its call's hook End parked the slot"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Approval rounds whose wait names its call: each binds the wait to that member,
// which its approval or its end answers.
// ---------------------------------------------------------------------------

use crate::user_action_request_for;

/// `call`'s approval, which resumes it.
fn approve(
    r: &mut Reducer,
    scene: &mut SceneState,
    id: AgentId,
    call: &str,
    at: SystemTime,
    tp: Transport,
) {
    r.apply(
        scene,
        resolved(id, Resolution::Approved(call.into())),
        at,
        tp,
    );
}

#[test]
fn parallel_approvals_resolve_on_the_last_member() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/e.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    for (call, detail) in [("x1", "bash: a"), ("x2", "bash: b")] {
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some(call),
            Some(detail),
            t0,
            Transport::Jsonl,
        );
    }
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 2);
    for call in ["x1", "x2"] {
        user_action_request_for(
            &mut r,
            &mut scene,
            id,
            "bash",
            Some(call),
            t0,
            Transport::Jsonl,
        );
    }

    // x1's approval retires it; x2 still waits on the human.
    let t1 = t0 + Duration::from_secs(2);
    approve(&mut r, &mut scene, id, "x1", t1, Transport::Jsonl);
    assert!(matches!(
        scene.agents.get(&id).unwrap().state,
        ActivityState::RequiresAction { .. }
    ));
    let t2 = t1 + Duration::from_secs(2);
    approve(&mut r, &mut scene, id, "x2", t2, Transport::Jsonl);
    let slot = scene.agents.get(&id).unwrap();
    assert!(matches!(slot.state, ActivityState::Running { .. }));
    assert_eq!(
        slot.tool_call_count, 2,
        "two logical calls, each counted once"
    );
}

/// A delegating parent's own approval round, its hooks under the parent's key
/// while a Task is in flight (#951): its pre-use and start are dropped as the
/// child's on either side of its gate, so the round ends alike whichever lands
/// first, and its end, a member's, still answers the wait.
#[test]
fn a_delegating_parents_own_approval_round_ends_alike_in_either_order() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut ends = Vec::new();
    for gate_first in [false, true] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let id = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "f");
        start_as(&mut r, &mut scene, id, UNNAMED_CHILD_HOOKS);
        // A Task in flight arms `suppress_subagent_leak` for this id.
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some("task-1"),
            Some("Agent"),
            t0,
            Transport::Hook,
        );
        let start = |r: &mut Reducer, scene: &mut SceneState| {
            let pending = AgentEvent::ToolUsePending {
                agent_id: id,
                tool_use_id: "x1".into(),
                tool_name: Some("bash".into()),
                turn: None,
            };
            r.apply(scene, pending, t0, Transport::Hook);
            tool_use_start(
                r,
                scene,
                id,
                Some("x1"),
                Some("bash: rm"),
                t0,
                Transport::Hook,
            );
        };
        let gate = |r: &mut Reducer, scene: &mut SceneState| {
            user_action_request_for(r, scene, id, "bash", Some("x1"), t0, Transport::Hook);
        };
        if gate_first {
            gate(&mut r, &mut scene);
            start(&mut r, &mut scene);
        } else {
            start(&mut r, &mut scene);
            gate(&mut r, &mut scene);
        }
        assert!(requires_action(&scene, id), "gate first: {gate_first}");
        let ended = t0 + Duration::from_secs(5);
        tool_use_end(&mut r, &mut scene, id, Some("x1"), ended, Transport::Hook);
        assert!(
            !requires_action(&scene, id),
            "gate first: {gate_first}: the member's end was dropped as the child's"
        );
        let slot = &scene.agents[&id];
        ends.push((slot.state.clone(), slot.tool_call_count));
    }
    assert_eq!(ends[0], ends[1]);
}

#[test]
fn a_denial_resolving_one_member_keeps_the_queued_siblings() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/g.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    for (call, detail) in [("x1", "bash: a"), ("x2", "bash: b")] {
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some(call),
            Some(detail),
            t0,
            Transport::Jsonl,
        );
    }
    for call in ["x1", "x2"] {
        user_action_request_for(
            &mut r,
            &mut scene,
            id,
            "bash",
            Some(call),
            t0,
            Transport::Jsonl,
        );
    }

    // Denying x1 must not forget x2's still-pending membership.
    let t1 = t0 + Duration::from_secs(3);
    tool_use_end(&mut r, &mut scene, id, Some("x1"), t1, Transport::Jsonl);
    assert!(
        matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "x2 still waits on the human"
    );
    let t2 = t1 + Duration::from_secs(3);
    approve(&mut r, &mut scene, id, "x2", t2, Transport::Jsonl);
    let slot = scene.agents.get(&id).unwrap();
    assert!(
        matches!(slot.state, ActivityState::Running { .. }),
        "x2's approval must still read as its approval round"
    );
    assert_eq!(
        slot.tool_call_count, 2,
        "the denial round re-counted a call"
    );
}

#[test]
fn an_auto_approved_parallel_tool_does_not_strip_a_pending_member() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/i.jsonl");
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
        t0,
        Transport::Jsonl,
    );
    // A rules-approved sibling runs while x1 still awaits the human.
    let t1 = t0 + Duration::from_secs(1);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("y"),
        Some("read: notes"),
        t1,
        Transport::Jsonl,
    );
    // x1's denial must still resolve the wait as a member's End.
    let t2 = t1 + Duration::from_secs(2);
    tool_use_end(&mut r, &mut scene, id, Some("x1"), t2, Transport::Jsonl);
    assert!(
        !matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::RequiresAction { .. }
        ),
        "y's start stripped x1's membership — the denial could no longer resolve it"
    );
}

/// An approval resumes the call its start showed, with that start's detail,
/// though another call of its message started after it.
#[test]
fn an_approval_resume_shows_its_calls_own_start() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/j.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    for (call, detail) in [("x1", "bash: rm -rf /tmp/x"), ("y", "read: notes")] {
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some(call),
            Some(detail),
            t0,
            Transport::Jsonl,
        );
    }
    user_action_request_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t0,
        Transport::Jsonl,
    );
    let t1 = t0 + Duration::from_secs(2);
    approve(&mut r, &mut scene, id, "x1", t1, Transport::Jsonl);
    match &scene.agents.get(&id).unwrap().state {
        ActivityState::Running {
            item: Some(ToolUse { detail, .. }),
        } => {
            assert_eq!(detail.as_deref(), Some("bash: rm -rf /tmp/x"));
        }
        other => panic!("expected Running after the resume, got {other:?}"),
    }
}

/// The CC shape: a hook Start whose JSONL twin lands however late
/// (an FSEvents-coalesced poll) counts once.
#[test]
fn a_transcript_twin_counts_once_at_any_lag() {
    for lag in [Duration::ZERO, Duration::from_secs(10 * 60)] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let id = AgentId::from_transcript_path("/p/k.jsonl");
        start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        for (at, tp) in [(t0, Transport::Hook), (t0 + lag, Transport::Jsonl)] {
            tool_use_start(
                &mut r,
                &mut scene,
                id,
                Some("t1"),
                Some("Edit: a.rs"),
                at,
                tp,
            );
        }
        assert_eq!(scene.agents[&id].tool_call_count, 1, "{lag:?}");
    }
}

// ---------------------------------------------------------------------------
// Turn keys: an agent's turns, and the requests and pending calls they scope
// (codex: the rollout's brackets and items, the hooks' answers; grok: the
// hook's starts, the transcript's ends).
// ---------------------------------------------------------------------------

use pixtuoid_core::source::Resolution;
use pixtuoid_core::state::TurnKey;
use pixtuoid_core::state::reducer::STRAGGLER_TTL;

fn turn(k: &str) -> TurnKey {
    TurnKey::Id(k.into())
}

fn apply(r: &mut Reducer, scene: &mut SceneState, ev: AgentEvent, at: SystemTime, tp: Transport) {
    r.apply(scene, ev, at, tp);
}

fn turn_start(id: AgentId, k: &str) -> AgentEvent {
    AgentEvent::TurnStart {
        agent_id: id,
        turn: turn(k),
    }
}

fn keyed_turn_end(id: AgentId, k: &str) -> AgentEvent {
    AgentEvent::TurnEnd {
        agent_id: id,
        turn: Some(turn(k)),
    }
}

fn pending(id: AgentId, call: &str, tool: &str, k: &str) -> AgentEvent {
    AgentEvent::ToolUsePending {
        agent_id: id,
        tool_use_id: call.into(),
        tool_name: Some(tool.into()),
        turn: Some(turn(k)),
    }
}

fn asked(id: AgentId, tool: &str, k: &str) -> AgentEvent {
    let mut keys = RequestKeys::default();
    keys.tool_name = Some(tool.into());
    keys.turn = Some(turn(k));
    AgentEvent::user_action_request(id, Some("permission".into()), keys)
}

fn resolved(id: AgentId, by: Resolution) -> AgentEvent {
    AgentEvent::UserActionResolved { agent_id: id, by }
}

fn requires_action(scene: &SceneState, id: AgentId) -> bool {
    matches!(
        scene.agents[&id].state,
        ActivityState::RequiresAction { .. }
    )
}

fn codex(r: &mut Reducer, scene: &mut SceneState) -> AgentId {
    let id = AgentId::from_parts("codex", "019e7762-9ded-7e33-be41-946ecf105bf4");
    start_as(r, scene, id, "codex");
    id
}

#[test]
fn a_keyed_turn_end_settles_its_turn_once() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = codex(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    apply(
        &mut r,
        &mut scene,
        turn_start(id, "t1"),
        t0,
        Transport::Jsonl,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("c1"),
        None,
        t0,
        Transport::Jsonl,
    );
    apply(
        &mut r,
        &mut scene,
        keyed_turn_end(id, "t1"),
        t0,
        Transport::Jsonl,
    );
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);

    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("c2"),
        None,
        t0,
        Transport::Jsonl,
    );
    apply(
        &mut r,
        &mut scene,
        keyed_turn_end(id, "t1"),
        t0,
        Transport::Jsonl,
    );
    assert!(
        matches!(scene.agents[&id].state, ActivityState::Running { .. }),
        "a replayed end of a closed turn ends nothing"
    );
}

#[test]
fn a_bracketed_turn_runs_between_its_items_and_settles_at_its_end() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = codex(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let no_item = ActivityState::Running { item: None };
    apply(
        &mut r,
        &mut scene,
        turn_start(id, "t1"),
        t0,
        Transport::Jsonl,
    );
    assert_eq!(scene.agents[&id].state, no_item);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("c1"),
        Some("exec_command"),
        t0,
        Transport::Jsonl,
    );
    assert!(matches!(
        scene.agents[&id].state,
        ActivityState::Running { item: Some(_) }
    ));
    tool_use_end(&mut r, &mut scene, id, Some("c1"), t0, Transport::Jsonl);
    assert_eq!(scene.agents[&id].state, no_item, "between its items");
    apply(
        &mut r,
        &mut scene,
        keyed_turn_end(id, "t1"),
        t0,
        Transport::Jsonl,
    );
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

fn grok(r: &mut Reducer, scene: &mut SceneState) -> AgentId {
    let id = AgentId::from_parts("grok", "019e7762-9ded-7e33-be41-946ecf105bf4");
    start_as(r, scene, id, "grok");
    id
}

#[test]
fn a_turn_start_scopes_but_does_not_show_on_a_source_that_does_not_bracket() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = grok(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    apply(&mut r, &mut scene, turn_start(id, "k"), t0, Transport::Hook);
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
    user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    apply(
        &mut r,
        &mut scene,
        resolved(id, Resolution::Turn(turn("k"))),
        t0,
        Transport::Hook,
    );
    assert_eq!(
        scene.agents[&id].state,
        ActivityState::Idle,
        "the raise took turn k's scope"
    );
}

/// grok's hooks start each turn and its transcript ends it, so the end of a
/// turn can land past the next turn's start.
#[test]
fn a_lagging_end_of_the_last_grok_turn_neither_answers_nor_ends_the_next() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = grok(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for k in ["k1", "k2"] {
        apply(&mut r, &mut scene, turn_start(id, k), t0, Transport::Hook);
    }
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("c2"),
        Some("bash"),
        t0,
        Transport::Hook,
    );
    user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    apply(
        &mut r,
        &mut scene,
        keyed_turn_end(id, "k1"),
        t0,
        Transport::Jsonl,
    );
    assert!(requires_action(&scene, id), "{:?}", scene.agents[&id].state);
    // An answer that ends no call settles onto the newest call in flight.
    tool_use_end(&mut r, &mut scene, id, None, t0, Transport::Hook);
    assert!(
        matches!(
            &scene.agents[&id].state,
            ActivityState::Running {
                item: Some(ToolUse { tool_use_id: Some(t), .. })
            } if &**t == "c2"
        ),
        "c2 is still in flight: {:?}",
        scene.agents[&id].state
    );
}

#[test]
fn a_turn_start_leaves_a_standing_request_to_its_answer() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = codex(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    apply(
        &mut r,
        &mut scene,
        AgentEvent::user_action_request(id, None, RequestKeys::default()),
        t0,
        Transport::Hook,
    );
    apply(
        &mut r,
        &mut scene,
        turn_start(id, "t1"),
        t0,
        Transport::Jsonl,
    );
    assert!(requires_action(&scene, id));
}

/// codex's rollout carries each turn's start and its end in order, so either
/// bracket of another turn proves the open one ended.
#[test]
fn on_one_ordered_stream_another_turns_start_or_end_ends_the_open_one() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let next_turn_open = ActivityState::Running { item: None };
    for (next, then) in [
        (
            turn_start as fn(AgentId, &str) -> AgentEvent,
            next_turn_open,
        ),
        (keyed_turn_end, ActivityState::Idle),
    ] {
        let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
        let id = codex(&mut r, &mut scene);
        apply(
            &mut r,
            &mut scene,
            turn_start(id, "t1"),
            t0,
            Transport::Jsonl,
        );
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some("c1"),
            None,
            t0,
            Transport::Jsonl,
        );
        apply(&mut r, &mut scene, next(id, "t2"), t0, Transport::Jsonl);
        assert_eq!(scene.agents[&id].state, then);
    }
}

/// grok's hook starts a turn and its transcript ends one, in no order between
/// them, so another turn's end proves only its own over.
#[test]
fn on_split_brackets_another_turns_end_leaves_the_open_one() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = grok(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    apply(
        &mut r,
        &mut scene,
        turn_start(id, "k1"),
        t0,
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("c1"),
        None,
        t0,
        Transport::Hook,
    );
    apply(
        &mut r,
        &mut scene,
        keyed_turn_end(id, "k2"),
        t0,
        Transport::Jsonl,
    );
    assert_eq!(
        scene.agents[&id].state,
        ActivityState::Running {
            item: Some(ToolUse {
                tool_use_id: Some("c1".into()),
                detail: None,
                kind: ToolKind::Other,
            }),
        }
    );
}

/// A dead life's turns are over, so its transcript's late end of one ends
/// nothing of the revived slot's.
#[test]
fn a_dead_lifes_late_turn_end_leaves_the_revived_slots_call() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = grok(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    apply(
        &mut r,
        &mut scene,
        turn_start(id, "k1"),
        t0,
        Transport::Hook,
    );
    apply(
        &mut r,
        &mut scene,
        AgentEvent::SessionEnd {
            agent_id: id,
            as_child: false,
        },
        t0,
        Transport::Hook,
    );
    let revived = t0 + Duration::from_millis(20);
    apply(
        &mut r,
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "grok".into(),
            session_id: "abc".into(),
            cwd: "/".into(),
            parent_id: None,
        },
        revived,
        Transport::Hook,
    );
    assert_eq!(scene.agents[&id].exiting_at, None, "the slot revived");
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("c2"),
        None,
        revived,
        Transport::Hook,
    );
    apply(
        &mut r,
        &mut scene,
        keyed_turn_end(id, "k1"),
        revived + STRAGGLER_TTL,
        Transport::Jsonl,
    );
    assert!(
        matches!(
            &scene.agents[&id].state,
            ActivityState::Running {
                item: Some(ToolUse { tool_use_id: Some(t), .. })
            } if &**t == "c2"
        ),
        "{:?}",
        scene.agents[&id].state
    );
}

/// A late attach: the reducer never saw the turn's start, yet the stream keys
/// its end by it.
#[test]
fn the_end_of_a_turn_whose_start_went_unseen_ends_what_the_turn_left() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let start = |id, k| AgentEvent::TurnStart {
        agent_id: id,
        turn: TurnKey::Seq(k),
    };
    let end = |id, k| AgentEvent::TurnEnd {
        agent_id: id,
        turn: Some(TurnKey::Seq(k)),
    };
    let opencode = |r: &mut Reducer, scene: &mut SceneState| {
        let id = AgentId::from_parts("opencode", "ses_p");
        start_as(r, scene, id, "opencode");
        id
    };

    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = opencode(&mut r, &mut scene);
    user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    apply(&mut r, &mut scene, end(id, 5), t0, Transport::Hook);
    assert_eq!(
        scene.agents[&id].state,
        ActivityState::Idle,
        "it answers the request no call did"
    );

    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = opencode(&mut r, &mut scene);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("c1"),
        Some("bash"),
        t0,
        Transport::Hook,
    );
    apply(&mut r, &mut scene, end(id, 5), t0, Transport::Hook);
    assert_eq!(
        scene.agents[&id].state,
        ActivityState::Idle,
        "it retires the call whose end never came"
    );
    apply(&mut r, &mut scene, start(id, 7), t0, Transport::Hook);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("c2"),
        Some("bash"),
        t0,
        Transport::Hook,
    );
    tool_use_end(&mut r, &mut scene, id, Some("c2"), t0, Transport::Hook);
    assert_eq!(
        scene.agents[&id].state,
        ActivityState::Running { item: None },
        "the next turn shows none of its calls"
    );
    apply(&mut r, &mut scene, end(id, 7), t0, Transport::Hook);
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

/// A request that names no turn takes the one open when it was raised, so it
/// ends with that turn: by its own key, or by the turn that supersedes it.
#[test]
fn a_turnless_request_ends_with_the_turn_it_was_raised_in() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let id = AgentId::from_parts("opencode", "ses_p");
    for close in [keyed_turn_end(id, "t1"), turn_start(id, "t2")] {
        let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
        start_as(&mut r, &mut scene, id, "opencode");
        apply(
            &mut r,
            &mut scene,
            turn_start(id, "t1"),
            t0,
            Transport::Hook,
        );
        user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
        apply(&mut r, &mut scene, close.clone(), t0, Transport::Hook);
        assert!(
            !matches!(
                scene.agents[&id].state,
                ActivityState::RequiresAction { .. }
            ),
            "{close:?}"
        );
    }
}

#[test]
fn a_codex_request_binds_its_tools_pending_call_and_resolves_on_its_output() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = codex(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    apply(
        &mut r,
        &mut scene,
        turn_start(id, "t1"),
        t0,
        Transport::Jsonl,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("c1"),
        Some("exec_command"),
        t0,
        Transport::Jsonl,
    );
    apply(
        &mut r,
        &mut scene,
        pending(id, "c1", "Bash", "t1"),
        t0,
        Transport::Hook,
    );
    apply(
        &mut r,
        &mut scene,
        pending(id, "c2", "Read", "t1"),
        t0,
        Transport::Hook,
    );
    apply(
        &mut r,
        &mut scene,
        asked(id, "Bash", "t1"),
        t0,
        Transport::Hook,
    );
    assert!(requires_action(&scene, id));
    tool_use_end(&mut r, &mut scene, id, Some("c2"), t0, Transport::Jsonl);
    assert!(
        requires_action(&scene, id),
        "another call's output answers nothing"
    );
    tool_use_end(&mut r, &mut scene, id, Some("c1"), t0, Transport::Jsonl);
    assert_eq!(
        scene.agents[&id].state,
        ActivityState::Running { item: None },
        "answered, inside its open turn"
    );
}

#[test]
fn a_codex_request_resolves_on_its_calls_run_its_turns_stop_or_its_turns_end() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    type Answer = fn(AgentId) -> AgentEvent;
    let answers: [(&str, Answer, Transport, bool); 4] = [
        (
            "its call ran",
            |id| resolved(id, Resolution::Call("c1".into())),
            Transport::Hook,
            true,
        ),
        (
            "its turn stopped",
            |id| resolved(id, Resolution::Turn(turn("t1"))),
            Transport::Hook,
            true,
        ),
        (
            "another turn stopped",
            |id| resolved(id, Resolution::Turn(turn("t0"))),
            Transport::Hook,
            false,
        ),
        (
            "its turn ended",
            |id| keyed_turn_end(id, "t1"),
            Transport::Jsonl,
            true,
        ),
    ];
    for (what, answer, tp, answered) in answers {
        let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
        let id = codex(&mut r, &mut scene);
        apply(
            &mut r,
            &mut scene,
            turn_start(id, "t1"),
            t0,
            Transport::Jsonl,
        );
        apply(
            &mut r,
            &mut scene,
            pending(id, "c1", "Bash", "t1"),
            t0,
            Transport::Hook,
        );
        apply(
            &mut r,
            &mut scene,
            asked(id, "Bash", "t1"),
            t0,
            Transport::Hook,
        );
        apply(&mut r, &mut scene, answer(id), t0, tp);
        assert_eq!(!requires_action(&scene, id), answered, "{what}");
    }
}

#[test]
fn a_pre_use_delivered_after_its_request_joins_it() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = codex(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    apply(
        &mut r,
        &mut scene,
        turn_start(id, "t1"),
        t0,
        Transport::Jsonl,
    );
    apply(
        &mut r,
        &mut scene,
        asked(id, "Bash", "t1"),
        t0,
        Transport::Hook,
    );
    apply(
        &mut r,
        &mut scene,
        pending(id, "c1", "Bash", "t1"),
        t0,
        Transport::Hook,
    );
    assert!(requires_action(&scene, id), "a pre-use is no item");
    tool_use_end(&mut r, &mut scene, id, Some("c1"), t0, Transport::Jsonl);
    assert_eq!(
        scene.agents[&id].state,
        ActivityState::Running { item: None }
    );
}

/// #951: a keyless wait the hooks raised (CC's Notification, grok's
/// `permission_prompt`) is answered by a call's first start on the hooks, never
/// by a transcript-only copy of a queued call, which can land on either side of
/// the wait.
#[test]
fn a_keyless_hook_wait_takes_a_calls_first_hook_start_in_either_order() {
    use pixtuoid_core::source::CopyId;
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut wrong = Vec::new();
    // CC's PreToolUse decodes a pre-use beside its start, and its transcript
    // line names a copy id; grok's names neither.
    for (source, pre_use, copy) in [("claude-code", true, true), ("grok", false, false)] {
        for transcript_first in [false, true] {
            let mut scene = SceneState::uniform(4);
            let mut r = Reducer::new();
            let id = AgentId::from_parts(source, "keyless");
            start_as(&mut r, &mut scene, id, source);
            let hook_start = |r: &mut Reducer, scene: &mut SceneState, call: &str| {
                if pre_use {
                    let pending = AgentEvent::ToolUsePending {
                        agent_id: id,
                        tool_use_id: call.into(),
                        tool_name: Some("Bash".into()),
                        turn: None,
                    };
                    r.apply(scene, pending, t0, Transport::Hook);
                }
                tool_use_start(
                    r,
                    scene,
                    id,
                    Some(call),
                    Some("Bash: ls"),
                    t0,
                    Transport::Hook,
                );
            };
            let transcript = |r: &mut Reducer, scene: &mut SceneState| {
                let mut line =
                    AgentEvent::tool_use_start(id, Some("c3".into()), Some("Bash: ls".into()));
                if copy {
                    line = line.with_copy_id(CopyId::new("c3"));
                }
                r.apply(scene, line, t0, Transport::Jsonl);
            };
            let what = format!("{source}, transcript first: {transcript_first}");
            // Two calls in flight, so the wait binds neither by deduction.
            for call in ["c1", "c2"] {
                hook_start(&mut r, &mut scene, call);
            }
            if transcript_first {
                transcript(&mut r, &mut scene);
            }
            user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
            if !transcript_first {
                transcript(&mut r, &mut scene);
            }
            if !requires_action(&scene, id) {
                wrong.push(format!("{what}: the copy answered"));
            }
            hook_start(&mut r, &mut scene, "c3");
            let slot = &scene.agents[&id];
            let shows_c3 = matches!(
                &slot.state,
                ActivityState::Running {
                    item: Some(ToolUse { tool_use_id: Some(c), .. })
                } if &**c == "c3"
            );
            if !shows_c3 || slot.tool_call_count != 3 {
                wrong.push(format!(
                    "{what}: then {:?}, {} calls",
                    slot.state, slot.tool_call_count
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// #951 extended: the other transport's copy of a call already started lands
/// while the human decides, however late: it answers no wait, which the agent's
/// next call does.
#[test]
fn the_other_transports_copy_of_a_started_call_answers_no_wait() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/other-transport.jsonl");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    // Two calls in flight, so the prompt binds neither by deduction.
    for call in ["c1", "c2"] {
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some(call),
            Some("Read: a"),
            t0,
            Transport::Hook,
        );
    }
    user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    for call in ["c1", "c2"] {
        let copy = t0 + Duration::from_secs(10 * 60);
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some(call),
            Some("Read: a"),
            copy,
            Transport::Jsonl,
        );
    }
    assert!(requires_action(&scene, id));
    assert_eq!(scene.agents[&id].tool_call_count, 2, "each counts once");
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("h1"),
        Some("Bash: ls"),
        t0 + Duration::from_secs(10 * 60),
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents[&id].state,
        ActivityState::Running { .. }
    ));
}

/// One CC hook payload, decoded by the real decoder: a Bash call unless
/// `extra` says otherwise.
fn cc_hook(event: &str, extra: serde_json::Value) -> Vec<AgentEvent> {
    use serde_json::json;
    let mut v = json!({"hook_event_name": event, "session_id": "sess-cc", "cwd": "/repo",
                       "tool_name": "Bash", "tool_input": {"command": "rm NOTE.txt"},
                       "_pixtuoid_source": "claude-code"});
    v.as_object_mut()
        .expect("an object")
        .extend(extra.as_object().expect("an object").clone());
    pixtuoid_core::source::decoder::decode_hook_payload(v).expect("decodes")
}

fn feed(r: &mut Reducer, scene: &mut SceneState, evs: Vec<AgentEvent>, at: SystemTime) {
    for ev in evs {
        r.apply(scene, ev, at, Transport::Hook);
    }
}

/// CC's prompt names its tool but no call: it holds the call pending under that
/// name, so the approved call's PostToolUse answers it.
#[test]
fn a_cc_prompt_holds_its_tools_pending_call() {
    use serde_json::json;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "sess-cc");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let read = json!({"tool_name": "Read", "tool_use_id": "r1"});
    let bash = json!({"tool_use_id": "t1"});
    feed(&mut r, &mut scene, cc_hook("PreToolUse", bash.clone()), t0);
    feed(&mut r, &mut scene, cc_hook("PreToolUse", read.clone()), t0);
    feed(
        &mut r,
        &mut scene,
        cc_hook("PermissionRequest", json!({})),
        t0,
    );
    assert!(requires_action(&scene, id));
    feed(&mut r, &mut scene, cc_hook("PostToolUse", read), t0);
    assert!(
        requires_action(&scene, id),
        "another tool's end answers nothing"
    );
    feed(&mut r, &mut scene, cc_hook("PostToolUse", bash), t0);
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

/// #242: hook deliveries ride per-connection tasks, so CC's PermissionRequest
/// can land before its call's PreToolUse. The pre-use joins the request, and its
/// start is no answer.
#[test]
fn a_cc_pre_use_delivered_after_its_prompt_joins_it() {
    use serde_json::json;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "sess-cc");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let bash = json!({"tool_use_id": "t1"});
    feed(
        &mut r,
        &mut scene,
        cc_hook("PermissionRequest", json!({})),
        t0,
    );
    feed(&mut r, &mut scene, cc_hook("PreToolUse", bash.clone()), t0);
    assert!(requires_action(&scene, id), "its pre-use is no answer");
    let read = json!({"tool_name": "Read", "tool_use_id": "r1"});
    feed(&mut r, &mut scene, cc_hook("PreToolUse", read.clone()), t0);
    assert!(requires_action(&scene, id), "nor is another tool's start");
    feed(&mut r, &mut scene, cc_hook("PostToolUse", bash), t0);
    assert!(
        matches!(
            &scene.agents[&id].state,
            ActivityState::Running {
                item: Some(ToolUse { tool_use_id: Some(t), .. })
            } if &**t == "r1"
        ),
        "the Read still runs: {:?}",
        scene.agents[&id].state
    );
    feed(&mut r, &mut scene, cc_hook("PostToolUse", read), t0);
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

/// #242 the other way round: a call's PreToolUse decoded after its
/// PostToolUse, however late, leaves nothing pending, so the next prompt for
/// its tool holds only that prompt's own call.
#[test]
fn a_pre_use_decoded_after_its_calls_end_holds_no_later_prompt() {
    use serde_json::json;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "sess-cc");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (old, new) = (json!({"tool_use_id": "t1"}), json!({"tool_use_id": "t2"}));
    feed(&mut r, &mut scene, cc_hook("PostToolUse", old.clone()), t0);
    let late = t0 + Duration::from_secs(10 * 60);
    feed(&mut r, &mut scene, cc_hook("PreToolUse", old), late);
    feed(&mut r, &mut scene, cc_hook("PreToolUse", new.clone()), late);
    feed(
        &mut r,
        &mut scene,
        cc_hook("PermissionRequest", json!({})),
        late,
    );
    assert!(
        requires_action(&scene, id),
        "premise: the new call is gated"
    );
    feed(&mut r, &mut scene, cc_hook("PostToolUse", new), late);
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

/// Only Reasonix's end names no call, and its starts name none either.
#[test]
fn an_end_naming_no_call_ends_only_a_call_named_by_none() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for (shown, ends) in [(Some("x"), false), (None, true)] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let id = AgentId::from_parts("reasonix", "/Users/dev/proj");
        start_as(&mut r, &mut scene, id, "reasonix");
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            shown,
            Some("bash"),
            t0,
            Transport::Hook,
        );
        tool_use_end(&mut r, &mut scene, id, None, t0, Transport::Hook);
        assert_eq!(
            scene.agents[&id].state == ActivityState::Idle,
            ends,
            "shown {shown:?}"
        );
    }
}

/// A thread runs one turn at a time, so a raise for turn m proves turn n is
/// over: n's lagging rollout lines (a refused call's output, `turn_aborted`)
/// must not clear m's prompt.
#[test]
fn a_lagging_end_of_a_refused_turn_keeps_the_next_turns_prompt() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = codex(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    apply(
        &mut r,
        &mut scene,
        turn_start(id, "n"),
        t0,
        Transport::Jsonl,
    );
    apply(
        &mut r,
        &mut scene,
        pending(id, "xn", "Bash", "n"),
        t0,
        Transport::Hook,
    );
    apply(
        &mut r,
        &mut scene,
        asked(id, "Bash", "n"),
        t0,
        Transport::Hook,
    );
    assert!(requires_action(&scene, id));
    // Esc: codex writes xn's output and turn_aborted(n), unread yet.
    let t1 = t0 + Duration::from_secs(5);
    apply(
        &mut r,
        &mut scene,
        pending(id, "xm", "Bash", "m"),
        t1,
        Transport::Hook,
    );
    apply(
        &mut r,
        &mut scene,
        asked(id, "Bash", "m"),
        t1,
        Transport::Hook,
    );
    let t2 = t1 + Duration::from_secs(5);
    tool_use_end(&mut r, &mut scene, id, Some("xn"), t2, Transport::Jsonl);
    apply(
        &mut r,
        &mut scene,
        keyed_turn_end(id, "n"),
        t2,
        Transport::Jsonl,
    );
    apply(
        &mut r,
        &mut scene,
        turn_start(id, "m"),
        t2,
        Transport::Jsonl,
    );
    assert!(
        requires_action(&scene, id),
        "m's prompt still waits on the human: {:?}",
        scene.agents[&id].state
    );
    tool_use_end(&mut r, &mut scene, id, Some("xm"), t2, Transport::Jsonl);
    assert_eq!(
        scene.agents[&id].state,
        ActivityState::Running { item: None },
        "m's own call answers it, inside m"
    );
}

/// One kimi hook payload, decoded by the real decoder.
fn kimi_hook(event: &str, tool: &str, call: &str) -> Vec<AgentEvent> {
    pixtuoid_core::source::decoder::decode_hook_payload(serde_json::json!({
        "hook_event_name": event, "session_id": "sess-k", "cwd": "/repo",
        "tool_name": tool, "tool_input": {"command": "ls"}, "tool_call_id": call,
        "_pixtuoid_source": UNNAMED_CHILD_HOOKS}))
    .expect("decodes")
}

/// kimi's subagent fires its hooks under its parent's key: its pre-use is the
/// child's, so it must not become a call the parent's prompt binds, whether
/// the prompt names its tool with no call or names nothing.
#[test]
fn an_unnamed_childs_pre_use_stays_out_of_its_parents_request() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for (child_call, prompt) in [
        (true, "PermissionRequest"),
        (true, "Notification"),
        (false, "PermissionRequest"),
        (false, "Notification"),
    ] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let id = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "sess-k");
        start_as(&mut r, &mut scene, id, UNNAMED_CHILD_HOOKS);
        feed(
            &mut r,
            &mut scene,
            kimi_hook("PreToolUse", "Agent", "T"),
            t0,
        );
        if child_call {
            feed(&mut r, &mut scene, kimi_hook("PreToolUse", "Bash", "C"), t0);
            feed(
                &mut r,
                &mut scene,
                kimi_hook("PostToolUse", "Bash", "C"),
                t0,
            );
        }
        feed(
            &mut r,
            &mut scene,
            kimi_hook("PostToolUse", "Agent", "T"),
            t0,
        );
        feed(&mut r, &mut scene, kimi_hook("PreToolUse", "Bash", "D"), t0);
        feed(&mut r, &mut scene, kimi_hook(prompt, "Bash", ""), t0);
        assert!(requires_action(&scene, id), "{prompt}");
        feed(
            &mut r,
            &mut scene,
            kimi_hook("PostToolUse", "Bash", "D"),
            t0,
        );
        assert!(
            !requires_action(&scene, id),
            "child call {child_call}, {prompt}: the parent's own call ended, got {:?}",
            scene.agents[&id].state
        );
    }
}

/// kimi's parent call that ends while its Task runs: the end is dropped as
/// possibly the child's, yet the call still ended.
#[test]
fn a_parents_call_ending_while_its_task_runs_is_not_shown_after_the_drain() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts(UNNAMED_CHILD_HOOKS, "sess-k");
    start_as(&mut r, &mut scene, id, UNNAMED_CHILD_HOOKS);
    feed(&mut r, &mut scene, kimi_hook("PreToolUse", "Bash", "P"), t0);
    feed(
        &mut r,
        &mut scene,
        kimi_hook("PreToolUse", "Agent", "T"),
        t0,
    );
    feed(
        &mut r,
        &mut scene,
        kimi_hook("PostToolUse", "Bash", "P"),
        t0,
    );
    feed(
        &mut r,
        &mut scene,
        kimi_hook("PostToolUse", "Agent", "T"),
        t0,
    );
    assert_eq!(scene.agents[&id].state, ActivityState::Idle);
}

/// After an oversized skip or a late attach no turn is open, so the rollout's
/// start of a call registers it turnless; the hook's pre-use then names its
/// turn, which the prompt is scoped to.
#[test]
fn a_call_first_seen_turnless_takes_its_pre_uses_turn() {
    let (mut r, mut scene) = (Reducer::new(), SceneState::uniform(4));
    let id = codex(&mut r, &mut scene);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("c"),
        Some("exec_command"),
        t0,
        Transport::Jsonl,
    );
    apply(
        &mut r,
        &mut scene,
        pending(id, "c", "Bash", "t1"),
        t0,
        Transport::Hook,
    );
    apply(
        &mut r,
        &mut scene,
        asked(id, "Bash", "t1"),
        t0,
        Transport::Hook,
    );
    assert!(requires_action(&scene, id));
    apply(
        &mut r,
        &mut scene,
        resolved(id, Resolution::Call("c".into())),
        t0,
        Transport::Hook,
    );
    assert_eq!(
        scene.agents[&id].state,
        ActivityState::Idle,
        "the approved call's run answers the prompt"
    );
}
