use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::Reducer;

use crate::{
    DRAIN_INFERRED, HOOK_AND_TRANSCRIPT, delegating_pair, proof_of_life, sess_end, tool_use_start,
    turn_end, user_action_request,
};

#[test]
fn stale_idle_agent_is_marked_exiting_after_timeout() {
    use pixtuoid_core::state::reducer::STALE_IDLE_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let id = AgentId::from_transcript_path("/p/stale.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    reducer.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    assert!(scene.agents.get(&id).unwrap().exiting_at.is_none());

    reducer.tick(&mut scene, t0 + STALE_IDLE_TIMEOUT - Duration::from_secs(1));
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_none(),
        "should not mark exiting before timeout"
    );

    reducer.tick(&mut scene, t0 + STALE_IDLE_TIMEOUT + Duration::from_secs(1));
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_some(),
        "should mark exiting after timeout"
    );
}

#[test]
fn stale_sweep_spares_a_slot_at_exactly_the_threshold() {
    use pixtuoid_core::state::reducer::STALE_IDLE_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let id = AgentId::from_transcript_path("/p/boundary.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    reducer.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    reducer.tick(&mut scene, t0 + STALE_IDLE_TIMEOUT);
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_none(),
        "age == threshold is not yet stale (strict >)"
    );
    reducer.tick(
        &mut scene,
        t0 + STALE_IDLE_TIMEOUT + Duration::from_millis(1),
    );
    assert!(scene.agents.get(&id).unwrap().exiting_at.is_some());
}

#[test]
fn exit_gc_spares_a_slot_at_exactly_the_grace_window() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let id = AgentId::from_transcript_path("/p/grace.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    reducer.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    sess_end(&mut reducer, &mut scene, id, false, t0, Transport::Hook);
    assert!(scene.agents.get(&id).unwrap().exiting_at.is_some());
    reducer.tick(&mut scene, t0 + EXIT_GRACE_WINDOW);
    assert!(
        scene.agents.contains_key(&id),
        "walkout age == grace window is not yet GC-able (strict >)"
    );
    reducer.tick(
        &mut scene,
        t0 + EXIT_GRACE_WINDOW + Duration::from_millis(1),
    );
    assert!(!scene.agents.contains_key(&id));
}

#[test]
fn stale_active_agent_uses_shorter_timeout_than_idle() {
    use pixtuoid_core::state::reducer::{STALE_ACTIVE_TIMEOUT, STALE_IDLE_TIMEOUT};
    assert!(
        STALE_ACTIVE_TIMEOUT < STALE_IDLE_TIMEOUT,
        "active timeout should be shorter than idle"
    );

    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let id = AgentId::from_transcript_path("/p/active.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    reducer.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    tool_use_start(
        &mut reducer,
        &mut scene,
        id,
        Some("t"),
        None,
        t0,
        Transport::Hook,
    );

    reducer.tick(
        &mut scene,
        t0 + STALE_ACTIVE_TIMEOUT + Duration::from_secs(1),
    );
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_some(),
        "active agent should be reaped after STALE_ACTIVE_TIMEOUT"
    );
}

#[test]
fn codex_idle_agent_reaps_faster_than_claude_idle() {
    use pixtuoid_core::state::reducer::{STALE_IDLE_TIMEOUT, STALE_SHORT_IDLE_TIMEOUT};
    assert!(
        STALE_SHORT_IDLE_TIMEOUT < STALE_IDLE_TIMEOUT,
        "codex idle timeout must be shorter than the generic idle timeout"
    );

    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    let cx = AgentId::from_transcript_path("/p/codex-sess.jsonl");
    let cc = AgentId::from_transcript_path("/p/cc-sess.jsonl");
    for (id, source) in [(cx, "codex"), (cc, "claude-code")] {
        reducer.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: id,
                source: source.into(),
                session_id: "s".into(),
                cwd: PathBuf::from("/repo"),
                parent_id: None,
            },
            t0,
            Transport::Hook,
        );
    }

    reducer.tick(
        &mut scene,
        t0 + STALE_SHORT_IDLE_TIMEOUT + Duration::from_secs(1),
    );
    assert!(
        scene.agents.get(&cx).unwrap().exiting_at.is_some(),
        "codex idle agent should reap after STALE_SHORT_IDLE_TIMEOUT"
    );
    assert!(
        scene.agents.get(&cc).unwrap().exiting_at.is_none(),
        "claude-code idle agent must NOT reap on the codex-fast window"
    );
}

/// A quit opencode TUI leaves no end signal, so the next prompt's
/// `session.execution.started` (decoded to `SessionStart`) is what heals the
/// sweep, during the walkout and after it.
#[test]
fn a_quit_opencode_idle_slot_is_swept_short_and_walks_back_in_on_its_next_turn() {
    use pixtuoid_core::state::reducer::{EXIT_GRACE_WINDOW, STALE_SHORT_IDLE_TIMEOUT};
    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let oc = AgentId::from_parts("opencode", "ses_q");
    let start = || AgentEvent::SessionStart {
        agent_id: oc,
        source: "opencode".into(),
        session_id: "ses_q".into(),
        cwd: PathBuf::from("/repo"),
        parent_id: None,
    };
    let exiting = |scene: &SceneState| scene.agents.get(&oc).map(|s| s.exiting_at.is_some());
    reducer.apply(&mut scene, start(), t0, Transport::Hook);

    let swept = t0 + STALE_SHORT_IDLE_TIMEOUT + Duration::from_secs(1);
    reducer.tick(&mut scene, swept);
    assert_eq!(
        exiting(&scene),
        Some(true),
        "an idle slot takes the short reaper"
    );
    let walking_out = swept + Duration::from_secs(1);
    reducer.apply(&mut scene, start(), walking_out, Transport::Hook);
    assert_eq!(
        exiting(&scene),
        Some(false),
        "a turn mid-walkout resurrects it"
    );

    let swept = walking_out + STALE_SHORT_IDLE_TIMEOUT + Duration::from_secs(1);
    reducer.tick(&mut scene, swept);
    let gone = swept + EXIT_GRACE_WINDOW + Duration::from_millis(1);
    reducer.tick(&mut scene, gone);
    assert_eq!(exiting(&scene), None);
    reducer.apply(&mut scene, start(), gone, Transport::Hook);
    assert_eq!(
        exiting(&scene),
        Some(false),
        "a turn after the walkout re-registers it"
    );
    let slot = &scene.agents[&oc];
    assert_eq!(
        (&*slot.cwd, &*slot.label),
        (std::path::Path::new("/repo"), "oc·repo"),
        "with the identity the turn's SessionStart carries"
    );
}

#[test]
fn proof_of_life_lapse_restores_normal_sweep() {
    use pixtuoid_core::state::reducer::{PROOF_OF_LIFE_TTL, STALE_ACTIVE_TIMEOUT};
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/pol-lapse.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: HOOK_AND_TRANSCRIPT.into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    tool_use_start(&mut r, &mut scene, id, Some("t"), None, t0, Transport::Hook);

    // Last vouch lands mid-window; the process then exits, so emissions stop.
    let vouch_at = t0 + STALE_ACTIVE_TIMEOUT - Duration::from_secs(100);
    proof_of_life(&mut r, &mut scene, id, vouch_at, Transport::Jsonl);

    let exempt_at = t0 + STALE_ACTIVE_TIMEOUT + Duration::from_secs(1);
    r.tick(&mut scene, exempt_at);
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_none(),
        "still inside the vouch TTL — exempt"
    );

    let lapsed_at = vouch_at + PROOF_OF_LIFE_TTL + Duration::from_secs(1);
    r.tick(&mut scene, lapsed_at);
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_some(),
        "a lapsed vouch must fall back to the normal stale sweep"
    );
}

#[test]
fn proof_of_life_for_unknown_id_is_a_no_op() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts(HOOK_AND_TRANSCRIPT, "/p/pol-unknown.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    proof_of_life(&mut r, &mut scene, id, t0, Transport::Jsonl);
    assert!(
        scene.agents.is_empty(),
        "ProofOfLife must never create a slot — only hook tool/permission events synthesize"
    );
}

#[test]
fn proof_of_life_does_not_touch_activity_state() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/pol-state.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: HOOK_AND_TRANSCRIPT.into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        Some("Edit: foo.rs"),
        t0,
        Transport::Hook,
    );
    let before = scene.agents.get(&id).unwrap().clone();

    proof_of_life(
        &mut r,
        &mut scene,
        id,
        t0 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    let after = scene.agents.get(&id).unwrap();
    assert_eq!(
        after.state, before.state,
        "ProofOfLife must not change activity state"
    );
    assert_eq!(
        after.last_event_at, before.last_event_at,
        "ProofOfLife must not refresh last_event_at — it is not a real event"
    );
}

#[test]
fn proof_of_life_does_not_block_session_end() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/pol-end.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: HOOK_AND_TRANSCRIPT.into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    proof_of_life(&mut r, &mut scene, id, t0, Transport::Jsonl);
    sess_end(
        &mut r,
        &mut scene,
        id,
        false,
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_some(),
        "SessionEnd must mark a vouched slot exiting immediately"
    );
    r.tick(
        &mut scene,
        t0 + Duration::from_secs(1) + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(
        !scene.agents.contains_key(&id),
        "the vouch must not delay the exit GC"
    );
}

#[test]
fn codex_vouched_idle_slot_outlives_short_idle_reap() {
    use pixtuoid_core::state::reducer::{PROOF_OF_LIFE_TTL, STALE_SHORT_IDLE_TIMEOUT};
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let vouched = AgentId::from_transcript_path("/p/codex-vouched.jsonl");
    let ghost = AgentId::from_transcript_path("/p/codex-ghost.jsonl");
    for id in [vouched, ghost] {
        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: id,
                source: "codex".into(),
                session_id: "s".into(),
                cwd: PathBuf::from("/repo"),
                parent_id: None,
            },
            t0,
            Transport::Hook,
        );
    }
    let vouch_at = t0 + STALE_SHORT_IDLE_TIMEOUT - Duration::from_secs(100);
    proof_of_life(&mut r, &mut scene, vouched, vouch_at, Transport::Jsonl);

    let sweep_at = t0 + STALE_SHORT_IDLE_TIMEOUT + Duration::from_secs(1);
    assert!(sweep_at.duration_since(vouch_at).unwrap() < PROOF_OF_LIFE_TTL);
    r.tick(&mut scene, sweep_at);
    assert!(
        scene.agents.get(&vouched).unwrap().exiting_at.is_none(),
        "an fd-vouched codex slot must outlive the short-idle reap"
    );
    assert!(
        scene.agents.get(&ghost).unwrap().exiting_at.is_some(),
        "an unvouched codex slot keeps the 5-min short-idle reap"
    );
}

#[test]
fn proof_of_life_on_delegating_parent_shields_its_active_subtree() {
    use pixtuoid_core::state::reducer::{PROOF_OF_LIFE_TTL, STALE_ACTIVE_TIMEOUT};
    // The probe never vouches subagent ids — their transcript stems are
    // `agent-<id>`, not session UUIDs — so only the parent can be vouched.
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "pol-shield", t0);
    let grandchild = AgentId::from_parts(
        DRAIN_INFERRED,
        "/p/pol-shield/subagents/agent-1/subagents/agent-2.jsonl",
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: grandchild,
            source: DRAIN_INFERRED.into(),
            session_id: "g".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(child),
        },
        t0 + Duration::from_millis(150),
        Transport::Jsonl,
    );
    // Dispatching a Task is what makes tasks_in_flight[parent] non-empty.
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
    tool_use_start(
        &mut r,
        &mut scene,
        grandchild,
        Some("g1"),
        Some("Read: /y"),
        t0 + Duration::from_secs(3),
        Transport::Jsonl,
    );

    // The probe re-vouches the PARENT only.
    let vouch_at = t0 + STALE_ACTIVE_TIMEOUT + Duration::from_secs(60);
    proof_of_life(&mut r, &mut scene, parent, vouch_at, Transport::Jsonl);

    let sweep_at = vouch_at + Duration::from_secs(1);
    assert!(sweep_at.duration_since(vouch_at).unwrap() < PROOF_OF_LIFE_TTL);
    r.tick(&mut scene, sweep_at);
    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_none(),
        "the vouched parent survives via its own-id exemption"
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "a vouched delegating parent must shield its silent Active child"
    );
    assert!(
        scene.agents.get(&grandchild).unwrap().exiting_at.is_none(),
        "the shield must walk the whole ancestor chain, not one level"
    );
}

#[test]
fn vouch_lapse_restores_subtree_sweep() {
    use pixtuoid_core::state::reducer::{PROOF_OF_LIFE_TTL, STALE_ACTIVE_TIMEOUT};
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "pol-lapse-tree", t0);
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

    // Last vouch lands mid-window; the process then exits, so emissions stop.
    let vouch_at = t0 + STALE_ACTIVE_TIMEOUT - Duration::from_secs(100);
    proof_of_life(&mut r, &mut scene, parent, vouch_at, Transport::Jsonl);

    let lapsed_at = vouch_at + PROOF_OF_LIFE_TTL + Duration::from_secs(1);
    r.tick(&mut scene, lapsed_at);
    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_some(),
        "a lapsed vouch must restore the parent's normal stale sweep"
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "the child must be swept too once the ancestor vouch lapses"
    );
}

#[test]
fn vouched_idle_parent_without_tasks_does_not_shield_idle_child() {
    use pixtuoid_core::state::reducer::{PROOF_OF_LIFE_TTL, STALE_IDLE_TIMEOUT};
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "pol-backstop", t0);
    // NO Task dispatch: tasks_in_flight[parent] stays empty; both slots sit Idle.

    let vouch_at = t0 + STALE_IDLE_TIMEOUT + Duration::from_secs(60);
    proof_of_life(&mut r, &mut scene, parent, vouch_at, Transport::Jsonl);

    let sweep_at = vouch_at + Duration::from_secs(1);
    assert!(sweep_at.duration_since(vouch_at).unwrap() < PROOF_OF_LIFE_TTL);
    r.tick(&mut scene, sweep_at);
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "a vouched but non-delegating parent must NOT shield its idle child — the 30-min backstop holds"
    );
    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_none(),
        "the vouched parent itself keeps the own-id exemption"
    );
}

#[test]
fn fresh_event_resets_stale_timer() {
    use pixtuoid_core::state::reducer::STALE_IDLE_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let id = AgentId::from_transcript_path("/p/fresh.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    reducer.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );

    let almost = t0 + STALE_IDLE_TIMEOUT - Duration::from_secs(60);
    user_action_request(
        &mut reducer,
        &mut scene,
        id,
        "perm",
        almost,
        Transport::Hook,
    );

    reducer.tick(
        &mut scene,
        t0 + STALE_IDLE_TIMEOUT + Duration::from_secs(60),
    );
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_none(),
        "fresh event should have reset the stale timer"
    );
}

#[test]
fn unknown_cwd_agent_reaps_faster() {
    use pixtuoid_core::state::reducer::STALE_UNKNOWN_CWD_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let id = AgentId::from_parts("codex", "/p/ghost.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    reducer.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "codex".into(),
            session_id: "s".into(),
            cwd: PathBuf::new(),
            parent_id: None,
        },
        t0,
        Transport::Jsonl,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(slot.unknown_cwd, "empty cwd should set unknown_cwd");
    let label = slot.label.clone();
    assert!(
        label.contains('#'),
        "empty cwd should produce source#N label, got {label}"
    );

    reducer.tick(
        &mut scene,
        t0 + STALE_UNKNOWN_CWD_TIMEOUT + Duration::from_secs(1),
    );
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_some(),
        "unknown-cwd agent should reap after STALE_UNKNOWN_CWD_TIMEOUT"
    );
}

#[test]
fn parented_empty_cwd_subagent_is_not_ghost_reaped() {
    use pixtuoid_core::state::reducer::STALE_UNKNOWN_CWD_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts("copilot", "root-sess");
    let child = AgentId::from_parts("copilot", "call_child1");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "copilot".into(),
            session_id: "root-sess".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Jsonl,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "copilot".into(),
            session_id: "call_child1".into(),
            cwd: PathBuf::new(),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(10),
        Transport::Jsonl,
    );
    assert!(
        !scene.agents.get(&child).unwrap().unknown_cwd,
        "a parented (subagent) slot must NOT be flagged unknown_cwd"
    );
    r.tick(
        &mut scene,
        t0 + STALE_UNKNOWN_CWD_TIMEOUT + Duration::from_secs(30),
    );
    assert!(
        scene
            .agents
            .get(&child)
            .is_some_and(|s| s.exiting_at.is_none()),
        "a parented empty-cwd subagent must not be reaped on the 3-min ghost timer"
    );
}

#[test]
fn session_end_cascades_to_grandchildren() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let grandparent = AgentId::from_transcript_path("/p/gp.jsonl");
    let parent = AgentId::from_parts("claude-code", "/p/gp/subagents/agent-p.jsonl");
    let child = AgentId::from_parts("claude-code", "/p/gp/subagents/agent-c.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: grandparent,
            source: "claude-code".into(),
            session_id: "gp".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "claude-code".into(),
            session_id: "p".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(grandparent),
        },
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(200),
        Transport::Hook,
    );

    sess_end(
        &mut r,
        &mut scene,
        grandparent,
        false,
        t0 + Duration::from_secs(10),
        Transport::Hook,
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "grandchild should cascade to exiting via BFS"
    );
}

#[test]
fn session_end_cascade_marks_all_descendants_exiting() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_transcript_path("/p/cascade-parent.jsonl");
    let child_a = AgentId::from_parts("claude-code", "/p/cascade-parent/subagents/agent-a.jsonl");
    let child_b = AgentId::from_parts("claude-code", "/p/cascade-parent/subagents/agent-b.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "claude-code".into(),
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
            agent_id: child_a,
            source: "claude-code".into(),
            session_id: "ca".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child_b,
            source: "claude-code".into(),
            session_id: "cb".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(200),
        Transport::Hook,
    );

    assert!(scene.agents.get(&child_a).unwrap().exiting_at.is_none());
    assert!(scene.agents.get(&child_b).unwrap().exiting_at.is_none());

    sess_end(
        &mut r,
        &mut scene,
        parent,
        false,
        t0 + Duration::from_secs(5),
        Transport::Hook,
    );

    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_some(),
        "parent must be marked exiting"
    );
    assert!(
        scene.agents.get(&child_a).unwrap().exiting_at.is_some(),
        "child_a must cascade to exiting when parent ends"
    );
    assert!(
        scene.agents.get(&child_b).unwrap().exiting_at.is_some(),
        "child_b must cascade to exiting when parent ends"
    );
}

#[test]
fn stale_sweep_already_cascaded_child_is_skipped_in_pass_two() {
    use pixtuoid_core::state::reducer::STALE_IDLE_TIMEOUT;
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_transcript_path("/p/double-stale.jsonl");
    let child = AgentId::from_parts("claude-code", "/p/double-stale/subagents/agent-1.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "claude-code".into(),
            session_id: "parent".into(),
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
            source: "claude-code".into(),
            session_id: "child".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );

    // No heartbeat for either — unlike stale_sweep_cascades_to_grandchildren, that puts BOTH
    // in the pass-1 `stale` vec, so one of them must hit the write-once skip.
    let now = t0 + STALE_IDLE_TIMEOUT + Duration::from_secs(1);
    r.tick(&mut scene, now);

    let parent_exit = scene.agents.get(&parent).unwrap().exiting_at;
    let child_exit = scene.agents.get(&child).unwrap().exiting_at;
    assert!(parent_exit.is_some(), "stale parent marked exiting");
    assert!(
        child_exit.is_some(),
        "independently-stale child also marked exiting (write-once, no double-stamp)"
    );
    assert_eq!(parent_exit, Some(now));
    assert_eq!(child_exit, Some(now));
}

#[test]
fn stale_sweep_cascades_to_grandchildren() {
    use pixtuoid_core::state::reducer::STALE_IDLE_TIMEOUT;
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let grandparent = AgentId::from_transcript_path("/p/stale-gp.jsonl");
    let parent = AgentId::from_parts("claude-code", "/p/stale-gp/subagents/agent-p.jsonl");
    let child = AgentId::from_parts("claude-code", "/p/stale-gp/subagents/agent-c.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: grandparent,
            source: "claude-code".into(),
            session_id: "gp".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "claude-code".into(),
            session_id: "p".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(grandparent),
        },
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(200),
        Transport::Hook,
    );
    // Heartbeat the middle + leaf so only the grandparent is independently stale.
    for (id, label) in [(parent, "cc·p"), (child, "cc·c")] {
        r.apply(
            &mut scene,
            AgentEvent::Rename {
                agent_id: id,
                label: label.into(),
            },
            t0 + Duration::from_secs(25 * 60),
            Transport::Jsonl,
        );
    }

    r.tick(&mut scene, t0 + STALE_IDLE_TIMEOUT + Duration::from_secs(1));

    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "grandchild should cascade-exit via BFS through the stale grandparent"
    );
}

#[test]
fn stale_sweep_cascade_skips_unrelated_fresh_agents() {
    use pixtuoid_core::state::reducer::STALE_IDLE_TIMEOUT;
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_transcript_path("/p/stale-host.jsonl");
    let child = AgentId::from_parts("claude-code", "/p/stale-host/subagents/agent-1.jsonl");
    let unrelated = AgentId::from_transcript_path("/p/other-session.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "claude-code".into(),
            session_id: "parent".into(),
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
            source: "claude-code".into(),
            session_id: "child".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: unrelated,
            source: "claude-code".into(),
            session_id: "other".into(),
            cwd: PathBuf::from("/other-repo"),
            parent_id: None,
        },
        t0 + Duration::from_millis(150),
        Transport::Hook,
    );
    // Heartbeat the child and the unrelated agent so neither is independently
    // stale: only the parent crosses the threshold.
    for (id, label) in [(child, "cc·sub"), (unrelated, "cc·other")] {
        r.apply(
            &mut scene,
            AgentEvent::Rename {
                agent_id: id,
                label: label.into(),
            },
            t0 + Duration::from_secs(25 * 60),
            Transport::Jsonl,
        );
    }

    r.tick(&mut scene, t0 + STALE_IDLE_TIMEOUT + Duration::from_secs(1));

    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "the stale parent's child must cascade-exit"
    );
    assert!(
        scene.agents.get(&unrelated).unwrap().exiting_at.is_none(),
        "a fresh, unrelated agent must NOT be cascaded out"
    );
}

#[test]
fn long_delegation_keeps_parent_and_live_subagent_alive() {
    use pixtuoid_core::state::reducer::STALE_ACTIVE_TIMEOUT;
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_transcript_path("/p/deleg.jsonl");
    let child = AgentId::from_parts("claude-code", "/p/deleg/subagents/agent-1.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "claude-code".into(),
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
            source: "claude-code".into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );

    // The Task-start arm does NOT bump last_event_at: the parent's liveness
    // stays frozen at t0.
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );

    // The subagent's own tool calls refresh its parent's lineage.
    for (mins, tuid) in [(5u64, "sub-R1"), (9u64, "sub-R2")] {
        tool_use_start(
            &mut r,
            &mut scene,
            child,
            Some(tuid),
            Some("Read: /x"),
            t0 + Duration::from_secs(mins * 60),
            Transport::Hook,
        );
    }

    r.tick(
        &mut scene,
        t0 + STALE_ACTIVE_TIMEOUT + Duration::from_secs(1),
    );

    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_none(),
        "a delegating parent must stay alive while its subagent emits events"
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "the live subagent must NOT be cascaded out by a falsely-stale parent"
    );
}

#[test]
fn a_childs_turn_end_refreshes_its_parents_lineage() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "lineage-turn-end", t0);
    let end = t0 + Duration::from_secs(60);
    turn_end(&mut r, &mut scene, child, end, Transport::Hook);
    assert_eq!(scene.agents[&parent].last_event_at, end);
}

#[test]
fn stale_sweep_spares_subagent_blocked_under_a_waiting_parent() {
    use pixtuoid_core::state::reducer::STALE_ACTIVE_TIMEOUT;
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_transcript_path("/p/perm-parent.jsonl");
    let child = AgentId::from_parts("claude-code", "/p/perm-parent/subagents/agent-1.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "claude-code".into(),
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
            source: "claude-code".into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );
    tool_use_start(
        &mut r,
        &mut scene,
        child,
        Some("c-tool"),
        Some("WebFetch: /x"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    // The child's tool needs permission, but CC's Notification hook lands on
    // the PARENT.
    user_action_request(
        &mut r,
        &mut scene,
        parent,
        "permission?",
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );

    r.tick(
        &mut scene,
        t0 + STALE_ACTIVE_TIMEOUT + Duration::from_secs(60),
    );

    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_none(),
        "Waiting parent (60-min threshold) must survive a 10-min wait"
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "a subagent blocked under a Waiting parent must NOT be reaped on the Active timer"
    );
}

#[test]
fn stale_sweep_spares_grandchild_under_a_waiting_ancestor() {
    use pixtuoid_core::state::reducer::STALE_ACTIVE_TIMEOUT;
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let gp = AgentId::from_transcript_path("/p/perm-gp.jsonl");
    let parent = AgentId::from_parts("claude-code", "/p/perm-gp/subagents/agent-p.jsonl");
    let child = AgentId::from_parts("claude-code", "/p/perm-gp/subagents/agent-c.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: gp,
            source: "claude-code".into(),
            session_id: "gp".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "claude-code".into(),
            session_id: "p".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(gp),
        },
        t0 + Duration::from_millis(100),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(200),
        Transport::Hook,
    );
    for id in [parent, child] {
        tool_use_start(
            &mut r,
            &mut scene,
            id,
            Some("t"),
            Some("WebFetch: /x"),
            t0 + Duration::from_secs(1),
            Transport::Hook,
        );
    }
    user_action_request(
        &mut r,
        &mut scene,
        gp,
        "permission?",
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );

    r.tick(
        &mut scene,
        t0 + STALE_ACTIVE_TIMEOUT + Duration::from_secs(60),
    );

    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "a grandchild under a Waiting ancestor must NOT be reaped on the Active timer"
    );
    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_none(),
        "the middle agent under a Waiting ancestor must NOT be reaped either"
    );
}

#[test]
fn active_subagent_keeps_parent_alive_via_jsonl_events() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (parent, child) = delegating_pair(&mut r, &mut scene, "deleg2", t0);
    // The parent's OWN last event is frozen at t0+1s from here on.
    tool_use_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-T"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    // The subagent emits ONLY JSONL — no hook event reaches the parent.
    for mins in [4u64, 8, 12] {
        tool_use_start(
            &mut r,
            &mut scene,
            child,
            Some("c"),
            Some("Read: /x"),
            t0 + Duration::from_secs(mins * 60),
            Transport::Jsonl,
        );
    }
    // Shortly after the last child event, but ~12 min past the parent's own.
    r.tick(
        &mut scene,
        t0 + Duration::from_secs(12 * 60) + Duration::from_secs(30),
    );

    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_none(),
        "a delegating parent must stay alive while its subagent emits JSONL events"
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "the live subagent must not be cascaded out by a falsely-stale parent"
    );
}

#[test]
fn reasonix_delegating_slot_survives_the_active_timeout() {
    use pixtuoid_core::source::ToolDetail;
    use pixtuoid_core::state::reducer::{STALE_ACTIVE_TIMEOUT, STALE_WAITING_TIMEOUT};
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("reasonix", "/Users/dev/proj");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "reasonix".into(),
            session_id: "/Users/dev/proj".into(),
            cwd: "/Users/dev/proj".into(),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    // Reasonix hooks carry no tool id.
    r.apply(
        &mut scene,
        AgentEvent::ToolUseStart {
            agent_id: id,
            tool_use_id: None,
            detail: Some(ToolDetail::Task),
        },
        t0,
        Transport::Hook,
    );

    r.tick(
        &mut scene,
        t0 + STALE_ACTIVE_TIMEOUT + Duration::from_secs(60),
    );
    assert!(
        scene
            .agents
            .get(&id)
            .is_some_and(|s| s.exiting_at.is_none()),
        "a hook-silent Delegating rx slot must not be swept on the 10-min Active timer"
    );
    r.tick(
        &mut scene,
        t0 + STALE_WAITING_TIMEOUT + Duration::from_secs(60),
    );
    assert!(
        scene.agents.get(&id).is_none_or(|s| s.exiting_at.is_some()),
        "the carve-out must not make the slot immortal"
    );
}

// Crafted input: two SessionStarts each naming the other as parent.
#[test]
fn waiting_parent_cycle_is_still_reaped_by_the_stale_sweep() {
    use pixtuoid_core::state::reducer::STALE_WAITING_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let a = AgentId::from_transcript_path("/p/cycle-a.jsonl");
    let b = AgentId::from_transcript_path("/p/cycle-b.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: a,
            source: "claude-code".into(),
            session_id: "cyc-a".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(b),
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: b,
            source: "claude-code".into(),
            session_id: "cyc-b".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(a),
        },
        t0,
        Transport::Hook,
    );
    user_action_request(&mut r, &mut scene, b, "permission", t0, Transport::Hook);

    r.tick(
        &mut scene,
        t0 + STALE_WAITING_TIMEOUT + Duration::from_secs(60),
    );
    for id in [a, b] {
        assert!(
            scene.agents.get(&id).is_none_or(|s| s.exiting_at.is_some()),
            "a Waiting parent-cycle member must not self-exempt from the stale sweep"
        );
    }
}

// A 2-cycle whose members are BOTH Waiting would mutually exempt — each has
// the OTHER as a genuine Waiting ancestor, so `has_waiting_ancestor` skips
// both every sweep tick (#238). The fix is upstream of the sweep: the
// SessionStart arm refuses a cycle-closing parent link, so the sweep itself
// needs no cycle awareness.
#[test]
fn mutual_waiting_parent_cycle_is_refused_at_the_link_seam_and_reaped() {
    use pixtuoid_core::state::reducer::STALE_WAITING_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let a = AgentId::from_transcript_path("/p/mutual-a.jsonl");
    let b = AgentId::from_transcript_path("/p/mutual-b.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: b,
            source: "claude-code".into(),
            session_id: "mut-b".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: a,
            source: "claude-code".into(),
            session_id: "mut-a".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(b),
        },
        t0,
        Transport::Hook,
    );
    // B's duplicate SessionStart proposes parent A, which would close the
    // cycle A → B → A: the link must be refused, leaving B parentless.
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: b,
            source: "claude-code".into(),
            session_id: "mut-b".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(a),
        },
        t0,
        Transport::Hook,
    );
    assert_eq!(
        scene.agents.get(&b).and_then(|s| s.parent_id),
        None,
        "a cycle-closing enrichment must degrade to parentless"
    );
    for id in [a, b] {
        user_action_request(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    }

    r.tick(
        &mut scene,
        t0 + STALE_WAITING_TIMEOUT + Duration::from_secs(60),
    );
    for id in [a, b] {
        assert!(
            scene.agents.get(&id).is_none_or(|s| s.exiting_at.is_some()),
            "a mutual-Waiting pair must not exempt each other from the stale sweep"
        );
    }
}
