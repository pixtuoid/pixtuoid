use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::reducer::{B1_CASCADE_GRACE, Reducer};
use pixtuoid_core::state::{ActivityState, GlobalDeskIndex, SceneState};

use crate::{act_end, act_start, delegating_pair, sess_end, start, waiting};

#[test]
fn session_start_creates_idle_slot_at_first_free_desk() {
    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");

    reducer.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "abc".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        SystemTime::now(),
        Transport::Hook,
    );

    let slot = scene.agents.get(&id).expect("agent inserted");
    assert_eq!(slot.desk_index, GlobalDeskIndex(0));
    assert_eq!(
        &*slot.label, "cc·repo",
        "label = source prefix + cwd basename"
    );
    assert_eq!(slot.state, ActivityState::Idle);
}

#[test]
fn session_end_marks_slot_exiting_then_tick_removes_it_after_grace() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(2);
    let mut r = Reducer::new();
    let a = AgentId::from_transcript_path("/p/a.jsonl");
    let b = AgentId::from_transcript_path("/p/b.jsonl");
    start(&mut r, &mut scene, a);
    start(&mut r, &mut scene, b);

    let t0 = SystemTime::now();
    sess_end(&mut r, &mut scene, a, false, t0, Transport::Hook);

    let slot = scene
        .agents
        .get(&a)
        .expect("slot still present during exit walk-out");
    assert!(
        slot.exiting_at.is_some(),
        "SessionEnd should mark exiting_at"
    );

    r.tick(
        &mut scene,
        t0 + EXIT_GRACE_WINDOW + std::time::Duration::from_millis(100),
    );
    assert!(
        !scene.agents.contains_key(&a),
        "tick should sweep expired exit"
    );
    assert_eq!(scene.next_free_desk(), Some(GlobalDeskIndex(0)));
}

#[test]
fn session_start_overflows_to_floor1_with_heterogeneous_capacity() {
    let mut r = Reducer::new();
    let mut scene = SceneState::new([2, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    for i in 0..3 {
        let id = AgentId::from_transcript_path(&format!("/proj/{i}.jsonl"));
        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: id,
                source: "cc".into(),
                session_id: format!("s{i}"),
                cwd: std::path::PathBuf::from("/repo"),
                parent_id: None,
            },
            t0,
            Transport::Jsonl,
        );
    }
    assert_eq!(scene.agents.len(), 3);
    let desks: Vec<usize> = scene.agents.values().map(|a| a.desk_index.0).collect();
    assert!(desks.contains(&0));
    assert!(desks.contains(&1));
    assert!(
        desks.contains(&2),
        "third agent should get desk 2 (floor 1)"
    );
    assert_eq!(scene.floor_of(GlobalDeskIndex(2)), 1);
}

#[test]
fn session_start_dropped_when_all_desks_occupied() {
    let mut r = Reducer::new();
    let mut scene = SceneState::new([2, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    for i in 0..2 {
        let id = AgentId::from_transcript_path(&format!("/proj/{i}.jsonl"));
        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: id,
                source: "cc".into(),
                session_id: format!("s{i}"),
                cwd: PathBuf::from("/repo"),
                parent_id: None,
            },
            t0,
            Transport::Hook,
        );
    }
    assert_eq!(scene.agents.len(), 2);
    assert!(scene.next_free_desk().is_none());

    let overflow_id = AgentId::from_transcript_path("/proj/overflow.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: overflow_id,
            source: "cc".into(),
            session_id: "s-overflow".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    assert_eq!(
        scene.agents.len(),
        2,
        "third SessionStart must be silently dropped when desks are full"
    );
    assert!(
        !scene.agents.contains_key(&overflow_id),
        "overflow agent should not exist"
    );
}

#[test]
fn session_start_on_exiting_slot_resurrects_in_place() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("reasonix", "/Users/dev/proj");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, id);
    sess_end(&mut r, &mut scene, id, false, t0, Transport::Hook);
    assert!(scene.agents.get(&id).unwrap().exiting_at.is_some());

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "reasonix".into(),
            session_id: "/Users/dev/proj".into(),
            cwd: "/Users/dev/proj".into(),
            parent_id: None,
        },
        t0 + Duration::from_millis(20),
        Transport::Hook,
    );
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_none(),
        "SessionStart on an exiting slot must cancel the walkout"
    );

    act_start(
        &mut r,
        &mut scene,
        id,
        None,
        None,
        t0 + Duration::from_millis(500),
        Transport::Hook,
    );
    r.tick(&mut scene, t0 + EXIT_GRACE_WINDOW + Duration::from_secs(1));
    let slot = scene
        .agents
        .get(&id)
        .expect("slot survives the grace window");
    assert!(matches!(slot.state, ActivityState::Active { .. }));
}

#[test]
fn resurrect_in_place_folds_the_active_span_into_active_ms() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("reasonix", "/Users/dev/proj");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, id);
    act_start(&mut r, &mut scene, id, None, None, t0, Transport::Hook);
    sess_end(&mut r, &mut scene, id, false, t0, Transport::Hook);
    assert_eq!(
        scene.agents.get(&id).unwrap().active_ms,
        0,
        "span not folded yet (mark_exiting doesn't accumulate)"
    );

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "reasonix".into(),
            session_id: "/Users/dev/proj".into(),
            cwd: "/Users/dev/proj".into(),
            parent_id: None,
        },
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert!(slot.exiting_at.is_none(), "walkout cancelled");
    assert_eq!(
        slot.active_ms, 2000,
        "the 2s Active span must fold into active_ms on resurrect"
    );
}

#[test]
fn session_start_on_exiting_subagent_does_not_resurrect() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_transcript_path("/p/resurr-parent.jsonl");
    let child = AgentId::from_parts("claude-code", "/p/resurr-parent/subagents/agent-1.jsonl");
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
        Transport::Jsonl,
    );

    sess_end(
        &mut r,
        &mut scene,
        parent,
        false,
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "child cascaded to exiting when the parent ended"
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
        t0 + Duration::from_secs(2),
        Transport::Jsonl,
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "an exiting SUBAGENT must NOT resurrect (the root-only gate)"
    );

    r.tick(
        &mut scene,
        t0 + Duration::from_secs(1) + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(
        !scene.agents.contains_key(&child),
        "the cascaded subagent is reaped after its grace window"
    );
}

#[test]
fn duplicate_root_session_start_does_not_resurrect_a_live_session() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let root = AgentId::from_transcript_path("/p/live-root.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: root,
            source: "claude-code".into(),
            session_id: "r".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        root,
        Some("t1"),
        Some("Edit: foo.rs"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    assert!(matches!(
        scene.agents.get(&root).unwrap().state,
        ActivityState::Active { .. }
    ));
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: root,
            source: "claude-code".into(),
            session_id: "r".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );
    let slot = scene.agents.get(&root).unwrap();
    assert!(
        slot.exiting_at.is_none(),
        "the live session was never exiting"
    );
    assert!(
        matches!(slot.state, ActivityState::Active { .. }),
        "a duplicate root start on a LIVE session must NOT reset it to Idle: a \
         resurrect here (the surviving mutant) drops Active + evicts active_tasks"
    );
}

#[test]
fn resurrect_in_place_clears_stale_active_tasks_so_fresh_session_hooks_apply() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/p/resurr-tasks.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "res".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("task-stale"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    sess_end(
        &mut r,
        &mut scene,
        id,
        false,
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "res".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0 + Duration::from_millis(2_500),
        Transport::Hook,
    );
    assert!(
        scene.agents.get(&id).unwrap().exiting_at.is_none(),
        "resurrected"
    );

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t-new"),
        Some("Read: /x"),
        t0 + Duration::from_secs(3),
        Transport::Hook,
    );
    match &scene.agents.get(&id).unwrap().state {
        ActivityState::Active { detail, .. } => {
            assert_eq!(
                detail.as_deref(),
                Some("Read: /x"),
                "the fresh session's hook tool must apply, not render as Delegating"
            );
        }
        other => panic!(
            "the resurrected session's first hook tool must NOT be suppressed by the dead life's active_tasks residue — got {other:?}"
        ),
    }
}

#[test]
fn resurrect_in_place_cancels_stale_pending_b1_cascade() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let parent = AgentId::from_transcript_path("/p/resurr-b1.jsonl");
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
    act_start(
        &mut r,
        &mut scene,
        parent,
        Some("task-old"),
        Some("Agent"),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    act_end(
        &mut r,
        &mut scene,
        parent,
        Some("task-old"),
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );
    sess_end(
        &mut r,
        &mut scene,
        parent,
        false,
        t0 + Duration::from_millis(2_200),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "claude-code".into(),
            session_id: "p".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0 + Duration::from_millis(2_400),
        Transport::Hook,
    );
    let child = AgentId::from_parts("claude-code", "/p/resurr-b1/subagents/agent-1.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(2_600),
        Transport::Jsonl,
    );

    r.tick(
        &mut scene,
        t0 + Duration::from_secs(2) + B1_CASCADE_GRACE + Duration::from_millis(10),
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_none(),
        "the dead life's armed b1 cascade must not fire into the resurrected session's fresh subagent"
    );
}

#[test]
fn duplicate_session_start_refreshes_liveness() {
    use pixtuoid_core::state::reducer::STALE_IDLE_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("reasonix", "/Users/dev/proj");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, id);

    let near = t0 + STALE_IDLE_TIMEOUT - Duration::from_secs(10);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "reasonix".into(),
            session_id: "/Users/dev/proj".into(),
            cwd: "/Users/dev/proj".into(),
            parent_id: None,
        },
        near,
        Transport::Hook,
    );
    r.tick(
        &mut scene,
        t0 + STALE_IDLE_TIMEOUT + Duration::from_secs(60),
    );
    assert!(
        scene
            .agents
            .get(&id)
            .is_some_and(|s| s.exiting_at.is_none()),
        "duplicate SessionStart must refresh last_event_at"
    );
}

fn codex_session_start(
    r: &mut Reducer,
    scene: &mut SceneState,
    id: AgentId,
    parent: Option<AgentId>,
    transport: Transport,
) {
    r.apply(
        scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "codex".into(),
            session_id: "sid".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: parent,
        },
        SystemTime::now(),
        transport,
    );
}

#[test]
fn session_start_enriches_parent_id_on_existing_orphan() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts("codex", "parent-sess");
    let child = AgentId::from_parts("codex", "child-agent");

    codex_session_start(&mut r, &mut scene, child, None, Transport::Jsonl);
    assert!(
        scene.agents.get(&child).unwrap().parent_id.is_none(),
        "JSONL-created subagent starts orphaned"
    );

    codex_session_start(&mut r, &mut scene, child, Some(parent), Transport::Hook);
    assert_eq!(
        scene.agents.get(&child).unwrap().parent_id,
        Some(parent),
        "existing orphan must be enriched with the parent link"
    );
}

#[test]
fn session_start_does_not_reparent_when_parent_already_set() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let child = AgentId::from_parts("codex", "child");
    let p1 = AgentId::from_parts("codex", "p1");
    let p2 = AgentId::from_parts("codex", "p2");

    codex_session_start(&mut r, &mut scene, child, Some(p1), Transport::Hook);
    codex_session_start(&mut r, &mut scene, child, Some(p2), Transport::Hook);
    assert_eq!(
        scene.agents.get(&child).unwrap().parent_id,
        Some(p1),
        "an established parent link is never overwritten"
    );
}

#[test]
fn codex_subagent_cascades_with_parent_on_session_end() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts("codex", "parent-sess");
    let child = AgentId::from_parts("codex", "child-agent");
    let now = SystemTime::now();

    codex_session_start(&mut r, &mut scene, parent, None, Transport::Hook);
    codex_session_start(&mut r, &mut scene, child, Some(parent), Transport::Hook);

    sess_end(&mut r, &mut scene, parent, false, now, Transport::Hook);
    assert!(
        scene.agents.get(&parent).unwrap().exiting_at.is_some(),
        "parent should be exiting"
    );
    assert!(
        scene.agents.get(&child).unwrap().exiting_at.is_some(),
        "subagent should cascade out with its parent"
    );
}

#[test]
fn hook_activity_start_for_unknown_id_synthesizes_slot_and_goes_active() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "gated-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        Some("Edit: foo.rs"),
        t0,
        Transport::Hook,
    );

    let slot = scene
        .agents
        .get(&id)
        .expect("hook event must synthesize the slot");
    assert!(
        matches!(slot.state, ActivityState::Active { .. }),
        "the synthesizing event itself applies to the fresh slot"
    );
    assert_eq!(&*slot.label, "#1");
    assert!(slot.cwd.as_os_str().is_empty(), "no cwd on the event");
}

#[test]
fn hook_waiting_for_unknown_id_synthesizes_slot_in_waiting_state() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "parked-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    waiting(&mut r, &mut scene, id, "permission", t0, Transport::Hook);

    let slot = scene
        .agents
        .get(&id)
        .expect("hook Waiting must synthesize the slot");
    assert!(
        matches!(slot.state, ActivityState::Waiting { .. }),
        "slot enters Waiting so the permission prompt is visible"
    );
}

#[test]
fn jsonl_event_for_unknown_id_stays_a_no_op() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "replayed-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Jsonl,
    );
    waiting(&mut r, &mut scene, id, "perm", t0, Transport::Jsonl);

    assert!(
        scene.agents.is_empty(),
        "JSONL events for an unknown id must not synthesize a slot"
    );
}

#[test]
fn hook_session_end_for_unknown_id_does_not_create_slot() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "already-gone");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    sess_end(&mut r, &mut scene, id, false, t0, Transport::Hook);

    assert!(scene.agents.is_empty(), "SessionEnd must not synthesize");
}

#[test]
fn jsonl_session_start_after_hook_synthesis_coalesces_into_same_slot() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "revived-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );
    let desk = scene.agents.get(&id).expect("synthesized").desk_index;

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "revived-sess".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0 + Duration::from_secs(1),
        Transport::Jsonl,
    );

    assert_eq!(scene.agents.len(), 1, "no duplicate sprite");
    assert_eq!(
        scene.agents.get(&id).unwrap().desk_index,
        desk,
        "the agent keeps its desk"
    );
}

#[test]
fn hook_synthesized_slot_is_exempt_from_unknown_cwd_reap() {
    use pixtuoid_core::state::reducer::STALE_UNKNOWN_CWD_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "parked-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    waiting(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    assert!(
        !scene.agents.get(&id).expect("synthesized").unknown_cwd,
        "process-proven slot must not carry the startup-ghost flag"
    );

    r.tick(
        &mut scene,
        t0 + STALE_UNKNOWN_CWD_TIMEOUT + Duration::from_secs(60),
    );
    let slot = scene.agents.get(&id).expect("still present after 4 min");
    assert!(
        slot.exiting_at.is_none(),
        "a parked-on-permission synthesized slot must ride the Waiting timeout, not the 3-min ghost reap"
    );
}

#[test]
fn refused_hook_registration_does_not_poison_dedup_for_the_later_jsonl_copy() {
    use pixtuoid_core::state::MAX_FLOORS;
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut caps = [0usize; MAX_FLOORS];
    caps[0] = 1;
    let mut scene = SceneState::new(caps);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    let occupant = AgentId::from_transcript_path("/p/occupant.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: occupant,
            source: "claude-code".into(),
            session_id: "o".into(),
            cwd: PathBuf::from("/Users/me/proj"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    sess_end(&mut r, &mut scene, occupant, false, t0, Transport::Hook);

    let id = AgentId::from_parts("claude-code", "gated-sess");
    let th = t0 + EXIT_GRACE_WINDOW - Duration::from_millis(100);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t9"),
        None,
        th,
        Transport::Hook,
    );
    assert!(
        !scene.agents.contains_key(&id),
        "desk exhausted — registration must be refused"
    );

    let tj = t0 + EXIT_GRACE_WINDOW + Duration::from_millis(200);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "gated-sess".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        tj,
        Transport::Jsonl,
    );
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t9"),
        None,
        tj,
        Transport::Jsonl,
    );

    let slot = scene
        .agents
        .get(&id)
        .expect("registered once the desk freed");
    assert!(
        matches!(slot.state, ActivityState::Active { .. }),
        "the JSONL ActivityStart must not be dedup-eaten by the refused hook's record"
    );
}

#[test]
fn duplicate_session_start_backfills_hook_synthesized_slot() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "gated-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );
    assert_eq!(&*scene.agents.get(&id).unwrap().label, "#1");

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "gated-sess".into(),
            cwd: PathBuf::from("/Users/me/repo"),
            parent_id: None,
        },
        t0 + Duration::from_secs(1),
        Transport::Jsonl,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(&*slot.cwd, std::path::Path::new("/Users/me/repo"));
    assert!(!slot.unknown_cwd);
    assert_eq!(&*slot.source, "claude-code", "empty source back-filled");
    assert_eq!(
        &*slot.session_id, "gated-sess",
        "empty session_id back-filled"
    );
    assert_eq!(
        &*slot.label, "cc·repo",
        "ordinal fallback upgraded with the back-filled source's prefix"
    );
}

#[test]
fn duplicate_session_start_with_real_cwd_heals_an_unknown_cwd_ghost() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("codex", "cx-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "codex".into(),
            session_id: "cx-sess".into(),
            cwd: PathBuf::from(""),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert!(slot.unknown_cwd);
    assert_eq!(&*slot.label, "cx#1");

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "codex".into(),
            session_id: "cx-sess".into(),
            cwd: PathBuf::from("/Users/me/myrepo"),
            parent_id: None,
        },
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(&*slot.cwd, std::path::Path::new("/Users/me/myrepo"));
    assert!(!slot.unknown_cwd, "healed ghost leaves the 3-min reap");
    assert_eq!(&*slot.label, "cx·myrepo");
}

#[test]
fn duplicate_session_start_never_overwrites_established_cwd_or_label() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    for cwd in ["/Users/me/repo-a", "/Users/me/repo-b"] {
        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: id,
                source: "claude-code".into(),
                session_id: "sess".into(),
                cwd: PathBuf::from(cwd),
                parent_id: None,
            },
            t0,
            Transport::Hook,
        );
    }

    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(&*slot.cwd, std::path::Path::new("/Users/me/repo-a"));
    assert_eq!(&*slot.label, "cc·repo-a");
}

#[test]
fn backfill_does_not_clobber_a_renamed_label() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "gated-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::Rename {
            agent_id: id,
            label: "code-explorer".into(),
        },
        t0,
        Transport::Jsonl,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "gated-sess".into(),
            cwd: PathBuf::from("/Users/me/repo"),
            parent_id: None,
        },
        t0 + Duration::from_secs(1),
        Transport::Jsonl,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(
        &*slot.cwd,
        std::path::Path::new("/Users/me/repo"),
        "cwd healed"
    );
    assert_eq!(&*slot.label, "code-explorer", "renamed label kept");
}

#[test]
fn two_step_backfill_source_first_then_cwd_still_upgrades_the_label() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "gated-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    waiting(&mut r, &mut scene, id, "permission", t0, Transport::Hook);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "gated-sess".into(),
            cwd: PathBuf::from(""),
            parent_id: None,
        },
        t0 + Duration::from_secs(1),
        Transport::Jsonl,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(&*slot.source, "claude-code", "source back-filled first");
    assert_eq!(&*slot.label, "#1", "no cwd yet — label unchanged");

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "gated-sess".into(),
            cwd: PathBuf::from("/Users/me/repo"),
            parent_id: None,
        },
        t0 + Duration::from_secs(2),
        Transport::Jsonl,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(&*slot.cwd, std::path::Path::new("/Users/me/repo"));
    assert_eq!(
        &*slot.label, "cc·repo",
        "fallback still upgrades after the two-step heal"
    );
}

#[test]
fn hook_identity_pid_fills_refreshes_and_never_downgrades_slot_pid() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("dsh", "ses_pid");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let pid_id = |pid: i32| pixtuoid_core::source::PidIdentity::new(pid, Some(1_000));
    let identity = |pid: Option<pixtuoid_core::source::PidIdentity>| AgentEvent::Identity {
        agent_id: id,
        source: "dsh".into(),
        session_id: "ses_pid".into(),
        cwd: Some(PathBuf::from("/w")),
        pid,
    };

    r.apply(&mut scene, identity(Some(pid_id(41))), t0, Transport::Hook);
    assert_eq!(
        scene.agents[&id].pid,
        Some(pid_id(41)),
        "registration fills pid"
    );

    r.apply(&mut scene, identity(Some(pid_id(42))), t0, Transport::Hook);
    assert_eq!(
        scene.agents[&id].pid,
        Some(pid_id(42)),
        "backfill refreshes pid"
    );

    r.apply(&mut scene, identity(None), t0, Transport::Hook);
    assert_eq!(
        scene.agents[&id].pid,
        Some(pid_id(42)),
        "None never downgrades"
    );
}

#[test]
fn session_start_registration_leaves_pid_unset() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_transcript_path("/t/pidless.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "pidless".into(),
            cwd: PathBuf::from("/t"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    assert_eq!(scene.agents[&id].pid, None);
}

#[test]
fn model_info_caches_model_and_effort_on_an_existing_slot() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "ses_m");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "ses_m".into(),
            cwd: PathBuf::from("/w"),
            parent_id: None,
        },
        t0,
        Transport::Jsonl,
    );
    let info = |model: Option<&str>, effort: Option<&str>| AgentEvent::ModelInfo {
        agent_id: id,
        model: model.map(String::from),
        effort: effort.map(String::from),
    };

    r.apply(
        &mut scene,
        info(Some("claude-opus-4-8"), None),
        t0,
        Transport::Jsonl,
    );
    assert_eq!(scene.agents[&id].model.as_deref(), Some("claude-opus-4-8"));
    r.apply(
        &mut scene,
        info(Some("claude-fable-5"), None),
        t0,
        Transport::Jsonl,
    );
    assert_eq!(scene.agents[&id].model.as_deref(), Some("claude-fable-5"));
    let t1 = t0 + Duration::from_secs(60);
    r.apply(&mut scene, info(None, Some("ultra")), t1, Transport::Jsonl);
    assert_eq!(scene.agents[&id].model.as_deref(), Some("claude-fable-5"));
    let eff = scene.agents[&id].effort.clone().expect("effort cached");
    assert_eq!(&*eff.value, "ultra");
    assert_eq!(eff.seen_at, t1);
    let t2 = t1 + Duration::from_secs(120);
    r.apply(&mut scene, info(None, Some("ultra")), t2, Transport::Jsonl);
    assert_eq!(scene.agents[&id].effort.clone().unwrap().seen_at, t2);
}

#[test]
fn model_info_for_an_unknown_agent_never_registers() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let ghost = AgentId::from_parts("claude-code", "ses_ghost");
    for transport in [Transport::Jsonl, Transport::Hook] {
        r.apply(
            &mut scene,
            AgentEvent::ModelInfo {
                agent_id: ghost,
                model: Some("claude-fable-5".into()),
                effort: Some("ultra".into()),
            },
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
            transport,
        );
        assert!(
            scene.agents.is_empty(),
            "ModelInfo must not synthesize a slot ({transport:?})"
        );
    }
}

#[test]
fn usage_accumulates_saturating_and_stamps_the_last_reading() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "ses_u");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "ses_u".into(),
            cwd: PathBuf::from("/w"),
            parent_id: None,
        },
        t0,
        Transport::Jsonl,
    );
    let usage = |fresh_tokens: u64| AgentEvent::Usage {
        agent_id: id,
        fresh_tokens,
    };
    r.apply(&mut scene, usage(56_500), t0, Transport::Jsonl);
    let t1 = t0 + Duration::from_secs(30);
    r.apply(&mut scene, usage(120_000), t1, Transport::Jsonl);
    let slot = &scene.agents[&id];
    assert_eq!(slot.tokens_used, 176_500);
    let reading = slot.last_usage.expect("reading stamped");
    assert_eq!(reading.delta, 120_000);
    assert_eq!(reading.seen_at, t1);
    r.apply(&mut scene, usage(u64::MAX), t1, Transport::Jsonl);
    assert_eq!(scene.agents[&id].tokens_used, u64::MAX);
}

#[test]
fn usage_for_an_unknown_agent_never_registers() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let ghost = AgentId::from_parts("claude-code", "ses_ghost_u");
    for transport in [Transport::Jsonl, Transport::Hook] {
        r.apply(
            &mut scene,
            AgentEvent::Usage {
                agent_id: ghost,
                fresh_tokens: 1_000_000,
            },
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
            transport,
        );
        assert!(
            scene.agents.is_empty(),
            "Usage must not synthesize a slot ({transport:?})"
        );
    }
}

#[test]
fn hook_identity_registers_unknown_id_with_real_identity() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "gated-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::Identity {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "gated-sess".into(),
            cwd: Some(PathBuf::from("/Users/me/repo")),
            pid: None,
        },
        t0,
        Transport::Hook,
    );

    let slot = scene
        .agents
        .get(&id)
        .expect("hook Identity must register the slot");
    assert_eq!(
        &*slot.label, "cc·repo",
        "the normal minted label, NOT a blank #N ordinal"
    );
    assert_eq!(&*slot.source, "claude-code");
    assert_eq!(&*slot.session_id, "gated-sess");
    assert_eq!(&*slot.cwd, std::path::Path::new("/Users/me/repo"));
    assert!(
        !slot.unknown_cwd,
        "real-cwd registration — not an unknown-cwd ghost"
    );
    assert_eq!(slot.floor_idx, scene.floor_of(slot.desk_index));
    assert!(
        matches!(slot.state, ActivityState::Idle),
        "Identity itself sets no activity state — the paired activity event does"
    );
}

#[test]
fn hook_identity_without_cwd_registers_reap_exempt_blank_cwd() {
    use pixtuoid_core::state::reducer::STALE_UNKNOWN_CWD_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("codex", "cx-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::Identity {
            agent_id: id,
            source: "codex".into(),
            session_id: "cx-sess".into(),
            cwd: None,
            pid: None,
        },
        t0,
        Transport::Hook,
    );

    let slot = scene.agents.get(&id).expect("registered");
    assert_eq!(&*slot.label, "cx#1", "no cwd → ordinal label");
    assert_eq!(&*slot.source, "codex", "source still lands");
    assert_eq!(&*slot.session_id, "cx-sess", "session_id still lands");
    assert!(!slot.unknown_cwd, "process-proven — reap-exempt");

    r.tick(
        &mut scene,
        t0 + STALE_UNKNOWN_CWD_TIMEOUT + Duration::from_secs(60),
    );
    assert!(
        scene
            .agents
            .get(&id)
            .is_some_and(|s| s.exiting_at.is_none()),
        "must outlive the 3-min unknown-cwd reap"
    );
}

#[test]
fn hook_identity_on_existing_unknown_cwd_slot_clears_ghost_reap() {
    use pixtuoid_core::state::reducer::STALE_UNKNOWN_CWD_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("codex", "cx-revive");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "codex".into(),
            session_id: "cx-revive".into(),
            cwd: PathBuf::from(""),
            parent_id: None,
        },
        t0,
        Transport::Jsonl,
    );
    assert!(
        scene.agents.get(&id).unwrap().unknown_cwd,
        "empty-cwd JSONL registration arms the ghost reap"
    );

    r.apply(
        &mut scene,
        AgentEvent::Identity {
            agent_id: id,
            source: "codex".into(),
            session_id: "cx-revive".into(),
            cwd: None,
            pid: None,
        },
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    waiting(
        &mut r,
        &mut scene,
        id,
        "permission",
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    assert!(
        !scene.agents.get(&id).unwrap().unknown_cwd,
        "a hook Identity is proof of life — the ghost reap must disarm on the existing-slot branch too"
    );

    r.tick(
        &mut scene,
        t0 + STALE_UNKNOWN_CWD_TIMEOUT + Duration::from_secs(60),
    );
    assert!(
        scene
            .agents
            .get(&id)
            .is_some_and(|s| s.exiting_at.is_none()),
        "a process-proven-alive Waiting session must outlive the 3-min unknown-cwd reap"
    );
}

#[test]
fn hook_identity_backfills_blank_synthesized_slot() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("reasonix", "/Users/dev/proj");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    act_end(&mut r, &mut scene, id, None, t0, Transport::Hook);
    assert_eq!(&*scene.agents.get(&id).unwrap().label, "#1", "blank slot");

    r.apply(
        &mut scene,
        AgentEvent::Identity {
            agent_id: id,
            source: "reasonix".into(),
            session_id: "/Users/dev/proj".into(),
            cwd: Some(PathBuf::from("/Users/dev/proj")),
            pid: None,
        },
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(&*slot.source, "reasonix", "empty source healed");
    assert_eq!(&*slot.session_id, "/Users/dev/proj", "session_id healed");
    assert_eq!(&*slot.cwd, std::path::Path::new("/Users/dev/proj"));
    assert!(!slot.unknown_cwd);
    assert_eq!(
        &*slot.label, "#1",
        "Identity carries no label authority — upgrade stays on SessionStart"
    );
}

#[test]
fn hook_identity_does_not_clobber_existing_identity() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "sess".into(),
            cwd: PathBuf::from("/Users/me/repo-a"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::Identity {
            agent_id: id,
            source: "codex".into(),
            session_id: "other-sess".into(),
            cwd: Some(PathBuf::from("/Users/me/repo-b")),
            pid: None,
        },
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );

    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(&*slot.source, "claude-code");
    assert_eq!(&*slot.session_id, "sess");
    assert_eq!(&*slot.cwd, std::path::Path::new("/Users/me/repo-a"));
    assert_eq!(&*slot.label, "cc·repo-a");
}

#[test]
fn hook_identity_respects_the_straggler_window() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "exited-invisible");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    sess_end(&mut r, &mut scene, id, false, t0, Transport::Hook);
    r.apply(
        &mut scene,
        AgentEvent::Identity {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "exited-invisible".into(),
            cwd: Some(PathBuf::from("/Users/me/repo")),
            pid: None,
        },
        t0 + Duration::from_millis(50),
        Transport::Hook,
    );
    assert!(
        !scene.agents.contains_key(&id),
        "an id that just ended must not be re-registered by a reordered Identity"
    );
}

#[test]
fn jsonl_identity_is_a_no_op() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "replayed-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::Identity {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "replayed-sess".into(),
            cwd: Some(PathBuf::from("/Users/me/repo")),
            pid: None,
        },
        t0,
        Transport::Jsonl,
    );
    assert!(
        scene.agents.is_empty(),
        "a JSONL Identity must not register a slot"
    );
}

#[test]
fn hook_identity_desk_refusal_is_quiet() {
    use pixtuoid_core::state::MAX_FLOORS;
    let caps = [0usize; MAX_FLOORS];
    let mut scene = SceneState::new(caps);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "gated-sess");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    r.apply(
        &mut scene,
        AgentEvent::Identity {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "gated-sess".into(),
            cwd: Some(PathBuf::from("/Users/me/repo")),
            pid: None,
        },
        t0,
        Transport::Hook,
    );
    assert!(
        scene.agents.is_empty(),
        "no desks — the registration must be quietly refused"
    );
}

fn connected_set<const N: usize>(srcs: [&str; N]) -> std::collections::HashSet<String> {
    srcs.iter().map(|s| s.to_string()).collect()
}

#[test]
fn reconcile_connected_evicts_disconnected_sources_and_cascades_subtree() {
    let mut scene = SceneState::uniform(8);
    let mut reducer = Reducer::new();
    let now = SystemTime::now();

    let (cc_parent, cc_child) = delegating_pair(&mut reducer, &mut scene, "x", now);
    let cx = AgentId::from_transcript_path("/p/cx.jsonl");
    reducer.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: cx,
            source: "codex".into(),
            session_id: "cx".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        now,
        Transport::Hook,
    );

    reducer.reconcile_connected(&mut scene, &connected_set(["codex"]), now);

    assert!(
        scene.agents.get(&cc_parent).unwrap().exiting_at.is_some(),
        "the disconnected source's agent exits"
    );
    assert!(
        scene.agents.get(&cc_child).unwrap().exiting_at.is_some(),
        "its subtree cascades out"
    );
    assert!(
        scene.agents.get(&cx).unwrap().exiting_at.is_none(),
        "a CONNECTED source is untouched"
    );
}

#[test]
fn reconcile_connected_is_idempotent_on_already_exiting() {
    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    start(&mut reducer, &mut scene, id);

    let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    let none = connected_set([]);
    reducer.reconcile_connected(&mut scene, &none, t1);
    let first = scene.agents.get(&id).unwrap().exiting_at;
    assert!(first.is_some(), "first reconcile marks exiting");

    reducer.reconcile_connected(&mut scene, &none, t1 + Duration::from_secs(2));
    assert_eq!(
        scene.agents.get(&id).unwrap().exiting_at,
        first,
        "already-exiting slot's exiting_at is left untouched"
    );
}

#[test]
fn reconcile_connected_evicts_a_blank_source_gate_slipper() {
    let mut scene = SceneState::uniform(4);
    let mut reducer = Reducer::new();
    let now = SystemTime::now();
    let blank = AgentId::from_transcript_path("/p/blank.jsonl");

    act_start(
        &mut reducer,
        &mut scene,
        blank,
        None,
        None,
        now,
        Transport::Hook,
    );
    assert_eq!(
        scene.agents.get(&blank).unwrap().source.as_ref(),
        "",
        "the synthesized slot is blank-source"
    );

    reducer.reconcile_connected(&mut scene, &connected_set(["claude-code", "codex"]), now);
    assert!(
        scene.agents.get(&blank).unwrap().exiting_at.is_some(),
        "a blank-source gate-slipper is evicted within one reconcile tick"
    );
}
