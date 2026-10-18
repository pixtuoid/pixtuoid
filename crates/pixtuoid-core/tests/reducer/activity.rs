use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::reducer::Reducer;
use pixtuoid_core::state::{ActivityState, SceneState};

use crate::{
    HOOK_AND_TRANSCRIPT, UNNAMED_CHILD_HOOKS, act_end, act_start, start, start_as, waiting,
};

#[test]
fn activity_start_sets_state_active() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start(&mut r, &mut scene, id);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        Some("Edit: foo.rs"),
        SystemTime::now(),
        Transport::Hook,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert!(matches!(slot.state, ActivityState::Active { .. }));
}

#[test]
fn activity_end_arms_debounce_then_tick_flips_to_idle() {
    use std::time::Duration;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::now();
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert!(matches!(slot.state, ActivityState::Active { .. }));
    assert!(slot.pending_idle_at.is_some());

    r.tick(&mut scene, t0 + Duration::from_millis(900));
    assert!(matches!(
        scene.agents.get(&id).unwrap().state,
        ActivityState::Active { .. }
    ));

    r.tick(&mut scene, t0 + Duration::from_millis(2000));
    assert_eq!(scene.agents.get(&id).unwrap().state, ActivityState::Idle);
    assert!(scene.agents.get(&id).unwrap().pending_idle_at.is_none());
}

#[test]
fn activity_start_inside_grace_window_cancels_debounce() {
    use std::time::Duration;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start(&mut r, &mut scene, id);
    let t0 = SystemTime::now();
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );
    assert!(scene.agents.get(&id).unwrap().pending_idle_at.is_some());
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t2"),
        None,
        t0 + Duration::from_millis(300),
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(matches!(slot.state, ActivityState::Active { .. }));
    assert!(
        slot.pending_idle_at.is_none(),
        "ActivityStart inside grace must cancel pending idle"
    );
    r.tick(&mut scene, t0 + Duration::from_millis(2500));
    assert!(matches!(
        scene.agents.get(&id).unwrap().state,
        ActivityState::Active { .. }
    ));
}

#[test]
fn waiting_sets_state_with_reason() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start(&mut r, &mut scene, id);

    waiting(
        &mut r,
        &mut scene,
        id,
        "Bash: rm -rf?",
        SystemTime::now(),
        Transport::Hook,
    );

    match &scene.agents.get(&id).unwrap().state {
        ActivityState::Waiting { reason } => assert_eq!(&**reason, "Bash: rm -rf?"),
        other => panic!("unexpected state: {other:?}"),
    }
}

#[test]
fn tool_call_count_increments_on_activity_start() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/stats.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, id);

    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 0);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 1);

    act_end(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        t0 + Duration::from_millis(500),
        Transport::Hook,
    );
    act_start(
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
fn active_ms_accumulates_on_state_transitions() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/active.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, id);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );
    assert_eq!(scene.agents.get(&id).unwrap().active_ms, 0);

    let t1 = t0 + Duration::from_secs(1);
    act_end(&mut r, &mut scene, id, Some("t1"), t1, Transport::Hook);
    r.tick(&mut scene, t1 + Duration::from_secs(3));
    let slot = scene.agents.get(&id).unwrap();
    assert!(
        slot.active_ms >= 1000,
        "expected >= 1000ms active, got {}",
        slot.active_ms
    );
}

#[test]
fn active_ms_does_not_double_count_on_duplicate_activity_end() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/dedup.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );

    let t1 = t0 + Duration::from_secs(2);
    act_end(&mut r, &mut scene, id, Some("t1"), t1, Transport::Hook);
    // The 2nd end is deliberately PAST the hook-wins window, so it is NOT deduped.
    act_end(
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
        slot.active_ms, 2600,
        "active_ms should be the single t0→t1+600 span (2600ms), not double-counted"
    );
}

#[test]
fn active_ms_preserved_when_task_arrives_during_active_tool() {
    use pixtuoid_core::source::ToolDetail;

    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/task-active.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, id);

    act_start(
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
        AgentEvent::ActivityStart {
            agent_id: id,
            tool_use_id: Some("task-1".into()),
            detail: Some(ToolDetail::Task),
        },
        t1,
        Transport::Hook,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert!(
        slot.active_ms >= 2000,
        "expected >= 2000ms active from pre-Task tool span, got {}",
        slot.active_ms
    );
}

#[test]
fn active_ms_preserved_when_waiting_interrupts_active() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/waiting.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, id);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );

    let t1 = t0 + Duration::from_secs(3);
    waiting(&mut r, &mut scene, id, "permission", t1, Transport::Hook);

    let slot = scene.agents.get(&id).unwrap();
    assert!(
        slot.active_ms >= 3000,
        "expected >= 3000ms active before Waiting, got {}",
        slot.active_ms
    );
}

// CC wire fact (captured live): when a permission Notification fires mid-tool
// (PreToolUse(t1) → Notification → PostToolUse(t1)), the gated tool's
// ActivityEnd carries the same tool_use_id that was Active when Waiting began —
// so PostToolUse(t1) means the permission was granted and the Waiting resolved.
#[test]
fn gated_tool_end_while_waiting_resolves_to_idle_after_grace() {
    use pixtuoid_core::state::reducer::ACTIVE_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/wait.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, id);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
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

    let end = t0 + Duration::from_millis(1000);
    act_end(&mut r, &mut scene, id, Some("t1"), end, Transport::Hook);
    let slot = scene.agents.get(&id).unwrap();
    assert!(
        matches!(slot.state, ActivityState::Waiting { .. }),
        "still Waiting during grace, got {:?}",
        slot.state
    );
    assert!(
        slot.pending_idle_at.is_some(),
        "gated tool end must arm the resolve debounce"
    );

    r.tick(
        &mut scene,
        end + ACTIVE_GRACE_WINDOW + Duration::from_millis(100),
    );
    assert!(
        matches!(scene.agents.get(&id).unwrap().state, ActivityState::Idle),
        "resolved permission must settle to Idle, got {:?}",
        scene.agents.get(&id).unwrap().state
    );
}

/// CC's recorded gate fires PermissionRequest (no tool_use_id) and then, if the
/// prompt sits, the idle Notification — both decode to Waiting. The second one
/// must not erase the tool the first remembered, or the approved tool's
/// PostToolUse resolves nothing and the slot never leaves Waiting.
#[test]
fn a_second_waiting_keeps_the_gate_the_first_one_remembered() {
    use pixtuoid_core::state::reducer::ACTIVE_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/gate.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, id);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
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
    waiting(
        &mut r,
        &mut scene,
        id,
        "Claude is waiting for your input",
        t0 + Duration::from_millis(900),
        Transport::Hook,
    );

    let end = t0 + Duration::from_millis(1200);
    act_end(&mut r, &mut scene, id, Some("t1"), end, Transport::Hook);
    r.tick(
        &mut scene,
        end + ACTIVE_GRACE_WINDOW + Duration::from_millis(100),
    );
    assert!(
        matches!(scene.agents.get(&id).unwrap().state, ActivityState::Idle),
        "the approved tool's end must still resolve the wait, got {:?}",
        scene.agents.get(&id).unwrap().state
    );
}

#[test]
fn parallel_tool_end_while_waiting_keeps_waiting() {
    use pixtuoid_core::state::reducer::ACTIVE_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/wait.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, id);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
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

    act_end(
        &mut r,
        &mut scene,
        id,
        Some("t2"),
        t0 + Duration::from_millis(1000),
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(
        matches!(slot.state, ActivityState::Waiting { .. }),
        "parallel tool end must keep Waiting, got {:?}",
        slot.state
    );
    assert!(
        slot.pending_idle_at.is_none(),
        "parallel tool end must not arm the resolve debounce"
    );

    r.tick(
        &mut scene,
        t0 + Duration::from_millis(1000) + ACTIVE_GRACE_WINDOW + Duration::from_millis(100),
    );
    assert!(
        matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "still Waiting — permission t1 never resolved"
    );
}

// An approval prompt BLOCKS a Codex/Reasonix turn, so a turn-end `Stop` (hook,
// no tool_use_id) arriving while Waiting means the prompt was already resolved
// upstream — and Reasonix has no second transport to self-heal a stale one.
#[test]
fn turn_end_stop_hook_resolves_stale_waiting() {
    use pixtuoid_core::state::reducer::ACTIVE_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("reasonix", "/Users/dev/proj");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start_as(&mut r, &mut scene, id, "reasonix");
    waiting(
        &mut r,
        &mut scene,
        id,
        "approval needed: bash rm -rf ./build",
        t0,
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents.get(&id).unwrap().state,
        ActivityState::Waiting { .. }
    ));

    let end = t0 + Duration::from_millis(800);
    act_end(&mut r, &mut scene, id, None, end, Transport::Hook);
    r.tick(
        &mut scene,
        end + ACTIVE_GRACE_WINDOW + Duration::from_millis(100),
    );
    assert!(
        matches!(scene.agents.get(&id).unwrap().state, ActivityState::Idle),
        "turn-end Stop must resolve the stale Waiting to Idle, got {:?}",
        scene.agents.get(&id).unwrap().state
    );
}

// Codex's JSONL emits None-id ActivityEnds per tool (it opts out of dedup), and
// one can race in just after a fresh PermissionRequest — so only the HOOK-side
// turn-end signal may resolve a Waiting.
#[test]
fn jsonl_none_id_end_while_waiting_keeps_waiting() {
    use pixtuoid_core::state::reducer::ACTIVE_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("codex", "sess-1");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start_as(&mut r, &mut scene, id, "codex");
    waiting(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    act_end(
        &mut r,
        &mut scene,
        id,
        None,
        t0 + Duration::from_millis(200),
        Transport::Jsonl,
    );
    r.tick(
        &mut scene,
        t0 + Duration::from_millis(200) + ACTIVE_GRACE_WINDOW + Duration::from_millis(100),
    );
    assert!(
        matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "a racing JSONL None-id end must keep the permission prompt up"
    );
}

#[test]
fn codex_permission_then_jsonl_output_resumes_to_active() {
    // The hook Waiting and the JSONL resume coalesce on the session UUID.
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

    waiting(
        &mut reducer,
        &mut scene,
        id,
        "permission",
        now,
        Transport::Hook,
    );
    assert!(
        matches!(scene.agents[&id].state, ActivityState::Waiting { .. }),
        "should be Waiting on permission"
    );

    act_start(
        &mut reducer,
        &mut scene,
        id,
        None,
        Some("exec_command"),
        now,
        Transport::Jsonl,
    );
    assert!(
        matches!(scene.agents[&id].state, ActivityState::Active { .. }),
        "resume must return to Active"
    );
}

// Line shapes follow a captured omp ask round, sanitized.
#[test]
fn omp_ask_round_waits_then_answer_clears_through_the_reducer() {
    use pixtuoid_core::source::omp::decode_omp_line;
    use pixtuoid_core::state::reducer::ACTIVE_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let path = "/h/.omp/agent/sessions/-p/2026-07-05T20-37-08-710Z_01000000-0000-7000-8000-000000000001.jsonl";
    let id = AgentId::from_parts(
        "omp",
        "2026-07-05T20-37-08-710Z_01000000-0000-7000-8000-000000000001",
    );
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let feed = |r: &mut Reducer, scene: &mut SceneState, line: &str, at: SystemTime| {
        for ev in decode_omp_line(path, "omp", serde_json::from_str(line).unwrap()).unwrap() {
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
            ActivityState::Waiting { .. }
        ),
        "the ask toolCall should park the slot Waiting"
    );
    // The user answers: the ask's toolResult (same toolCallId) arrives.
    feed(
        &mut r,
        &mut scene,
        r#"{"type":"message","id":"4a4176db","parentId":"98404700","timestamp":"2026-07-05T20:40:39.589Z","message":{"role":"toolResult","toolCallId":"toolu_01ASKASKASKASKASKASKASKA","toolName":"ask","content":[{"type":"text","text":"User selected: Merge"}],"isError":false,"timestamp":1783284039588}}"#,
        t0 + Duration::from_millis(500),
    );
    r.tick(
        &mut scene,
        t0 + Duration::from_millis(500) + ACTIVE_GRACE_WINDOW + Duration::from_millis(100),
    );
    assert!(
        !matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "the answered ask must clear Waiting (else the sprite hangs 60 min)"
    );
}

// ---------------------------------------------------------------------------
// omp extension-bridge approval rounds (#951): the interleavings human approval
// latency makes reachable past `HOOK_WINS_WINDOW` (`counted_calls`' own doc) —
// each must bind the wait to the gated call id and count it ONCE.
// ---------------------------------------------------------------------------

use crate::waiting_for;
use pixtuoid_core::state::reducer::HOOK_WINS_WINDOW;

/// Past the dedup window: the cross-transport pair separated by a human.
fn beyond_dedup(t: SystemTime) -> SystemTime {
    t + HOOK_WINS_WINDOW + Duration::from_millis(100)
}

#[test]
fn approval_transcript_start_first_waits_then_resumes_active_counting_once() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: rm"),
        t0,
        Transport::Jsonl,
    );
    assert_eq!(scene.agents.get(&id).unwrap().tool_call_count, 1);

    waiting_for(
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
        ActivityState::Waiting { .. }
    ));

    // The approval resume.
    let t2 = beyond_dedup(beyond_dedup(t0)) + Duration::from_secs(5);
    act_start(
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
        matches!(slot.state, ActivityState::Active { .. }),
        "approval resume returns the slot to Active"
    );
    assert_eq!(slot.tool_call_count, 1, "the resume must not re-count x1");
}

#[test]
fn approval_hook_waiting_first_survives_a_late_transcript_start() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/b.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    waiting_for(
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
        ActivityState::Waiting { .. }
    ));

    // The transcript's Start for the SAME gated call, while the human decides.
    let t1 = beyond_dedup(t0);
    act_start(
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
        matches!(slot.state, ActivityState::Waiting { .. }),
        "a gated call's own transcript Start must not fake an approval"
    );
    assert_eq!(slot.tool_call_count, 1);

    let t2 = beyond_dedup(t1) + Duration::from_secs(5);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        None,
        t2,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(matches!(slot.state, ActivityState::Active { .. }));
    assert_eq!(slot.tool_call_count, 1, "still one logical call");
}

#[test]
fn approval_resume_before_the_transcript_start_counts_once() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/c.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t0,
        Transport::Hook,
    );
    let t1 = beyond_dedup(t0) + Duration::from_secs(5);
    act_start(
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
    act_start(
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
    assert!(matches!(slot.state, ActivityState::Active { .. }));
}

#[test]
fn denied_approval_clears_waiting_via_the_hook_end() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/d.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: rm"),
        t0,
        Transport::Jsonl,
    );
    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        beyond_dedup(t0),
        Transport::Hook,
    );

    let t2 = beyond_dedup(beyond_dedup(t0)) + Duration::from_secs(5);
    act_end(&mut r, &mut scene, id, Some("x1"), t2, Transport::Hook);
    // The resolved wait settles through the ordinary Active→Idle debounce.
    r.tick(
        &mut scene,
        t2 + pixtuoid_core::state::reducer::ACTIVE_GRACE_WINDOW + Duration::from_millis(100),
    );
    assert!(
        !matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "a denial must clear the Waiting (else the sprite hangs 60 min)"
    );

    // The transcript's error toolResult follows; it must not disturb anything.
    act_end(
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
fn parallel_gated_approvals_resolve_member_by_member() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/e.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: a"),
        t0,
        Transport::Jsonl,
    );
    act_start(
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
    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t1,
        Transport::Hook,
    );
    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t1,
        Transport::Hook,
    );

    // Resolving x1 resumes work; x2's gate must survive it.
    let t2 = beyond_dedup(t1) + Duration::from_secs(2);
    act_start(
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
        ActivityState::Active { .. }
    ));

    // omp re-raises the still-pending gate; its resolve must work identically.
    let t3 = t2 + Duration::from_secs(1);
    waiting_for(
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
        ActivityState::Waiting { .. }
    ));
    let t4 = beyond_dedup(t3) + Duration::from_secs(2);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x2"),
        None,
        t4,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(matches!(slot.state, ActivityState::Active { .. }));
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
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("task-1"),
        Some("Agent"),
        t0,
        Transport::Jsonl,
    );

    // The parent's OWN gated tool (parallel to the Task), then its approval.
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: rm"),
        t0,
        Transport::Jsonl,
    );
    let t1 = beyond_dedup(t0);
    waiting_for(
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
        ActivityState::Waiting { .. }
    ));

    let t2 = beyond_dedup(t1) + Duration::from_secs(5);
    act_start(
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
            ActivityState::Waiting { .. }
        ),
        "the approval resume must not be eaten as a subagent leak"
    );
    let count_after_resume = scene.agents.get(&id).unwrap().tool_call_count;

    // Denial path on a second gated call: the hook End must clear the wait too.
    let t3 = beyond_dedup(t2);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x2"),
        Some("bash: b"),
        t3,
        Transport::Jsonl,
    );
    let t4 = beyond_dedup(t3);
    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t4,
        Transport::Hook,
    );
    let t5 = beyond_dedup(t4) + Duration::from_secs(5);
    act_end(&mut r, &mut scene, id, Some("x2"), t5, Transport::Hook);
    assert!(
        !matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "the denial End must not be eaten as a subagent leak"
    );
    assert_eq!(
        scene.agents.get(&id).unwrap().tool_call_count,
        count_after_resume + 1
    );
}

#[test]
fn a_denial_resolving_one_gate_keeps_the_queued_siblings() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/g.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: a"),
        t0,
        Transport::Jsonl,
    );
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x2"),
        Some("bash: b"),
        t0,
        Transport::Jsonl,
    );
    let t1 = beyond_dedup(t0);
    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t1,
        Transport::Hook,
    );
    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t1,
        Transport::Hook,
    );

    // Denying x1 must not forget x2's still-pending gate.
    let t2 = beyond_dedup(t1) + Duration::from_secs(3);
    act_end(&mut r, &mut scene, id, Some("x1"), t2, Transport::Hook);
    let t3 = t2 + Duration::from_secs(1);
    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t3,
        Transport::Hook,
    );
    let t4 = beyond_dedup(t3) + Duration::from_secs(3);
    act_start(
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
        matches!(slot.state, ActivityState::Active { .. }),
        "x2's resume must still read as the gated approval round"
    );
    assert_eq!(
        slot.tool_call_count, 2,
        "the denial round re-counted a call"
    );
}

#[test]
fn an_already_resolved_calls_late_transcript_start_keeps_a_sibling_gate() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/h.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    // x1's whole round runs hook-first; its transcript Start straggles in
    // AFTER x1 resolved, while x2's approval is still pending.
    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t0,
        Transport::Hook,
    );
    let t1 = beyond_dedup(t0) + Duration::from_secs(2);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        None,
        t1,
        Transport::Hook,
    );
    let t2 = t1 + Duration::from_secs(1);
    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x2"),
        t2,
        Transport::Hook,
    );

    let t3 = beyond_dedup(t2);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: a"),
        t3,
        Transport::Jsonl,
    );
    // The straggler must not have wiped x2's gate.
    let t4 = beyond_dedup(t3) + Duration::from_secs(2);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x2"),
        None,
        t4,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(matches!(slot.state, ActivityState::Active { .. }));
    assert_eq!(
        slot.tool_call_count, 2,
        "x1 once (resume), x2 once (resume) — the straggler added nothing"
    );
}

#[test]
fn an_auto_approved_parallel_tool_does_not_strip_a_pending_gate() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/i.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    waiting_for(
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
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("y"),
        Some("read: notes"),
        t1,
        Transport::Jsonl,
    );
    // x1's denial must still resolve the (re-raised) wait as a gated End.
    let t2 = beyond_dedup(t1);
    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t2,
        Transport::Hook,
    );
    let t3 = beyond_dedup(t2) + Duration::from_secs(2);
    act_end(&mut r, &mut scene, id, Some("x1"), t3, Transport::Hook);
    r.tick(
        &mut scene,
        t3 + pixtuoid_core::state::reducer::ACTIVE_GRACE_WINDOW + Duration::from_millis(100),
    );
    assert!(
        !matches!(
            scene.agents.get(&id).unwrap().state,
            ActivityState::Waiting { .. }
        ),
        "y's start stripped x1's gate — the denial could no longer resolve it"
    );
}

/// Hook-first ordering: the transcript's richer detail lands while the slot
/// Waits and is deliberately dropped, so the resume re-labels from the wire's
/// toolName — the gated-Jsonl arm's documented label-only cost.
#[test]
fn the_approval_resume_labels_from_the_wire_tool_name() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/j.jsonl");
    start_as(&mut r, &mut scene, id, HOOK_AND_TRANSCRIPT);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    waiting_for(
        &mut r,
        &mut scene,
        id,
        "bash",
        Some("x1"),
        t0,
        Transport::Hook,
    );
    let t1 = beyond_dedup(t0);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash: rm -rf /tmp/x"),
        t1,
        Transport::Jsonl,
    );
    let t2 = beyond_dedup(t1) + Duration::from_secs(2);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("x1"),
        Some("bash"),
        t2,
        Transport::Hook,
    );
    match &scene.agents.get(&id).unwrap().state {
        ActivityState::Active { detail, .. } => {
            assert_eq!(detail.as_deref(), Some("bash"));
        }
        other => panic!("expected Active after the resume, got {other:?}"),
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

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        Some("Edit: a.rs"),
        t0,
        Transport::Hook,
    );
    act_start(
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
