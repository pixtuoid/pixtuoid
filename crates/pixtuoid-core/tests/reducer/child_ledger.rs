use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::{CHILD_END_GATE_TTL, Reducer, STRAGGLER_TTL};

use crate::{act_end, act_start, sess_end, start};

#[test]
fn hook_session_end_blocks_reordered_trailing_event_synthesis() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "exited-invisible");
    let other = AgentId::from_parts("claude-code", "still-alive");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    sess_end(&mut r, &mut scene, id, false, t0, Transport::Hook);
    act_end(
        &mut r,
        &mut scene,
        id,
        None,
        t0 + Duration::from_millis(50),
        Transport::Hook,
    );
    assert!(
        !scene.agents.contains_key(&id),
        "a reordered trailing event must not resurrect a session that just ended"
    );

    act_end(
        &mut r,
        &mut scene,
        other,
        None,
        t0 + Duration::from_millis(50),
        Transport::Hook,
    );
    assert!(
        scene.agents.contains_key(&other),
        "the straggler window must be per-id, not a global synthesis gate"
    );
}

#[test]
fn hook_event_after_the_straggler_window_synthesizes_again() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("claude-code", "revived-later");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    sess_end(&mut r, &mut scene, id, false, t0, Transport::Hook);
    act_start(
        &mut r,
        &mut scene,
        id,
        Some("t1"),
        None,
        t0 + STRAGGLER_TTL + Duration::from_secs(1),
        Transport::Hook,
    );
    assert!(
        scene.agents.contains_key(&id),
        "past the TTL a hook event is fresh proof of life and must synthesize"
    );
}

/// The 5 s straggler window is the ONLY guard, and past it nothing cares
/// which arm arrives: a turn-boundary `ActivityEnd` re-registers exactly as an
/// `Identity` does. hermes has no other reaper (`resurrects_on_prompt: false`),
/// so a turn finishing after the user rotates with `/new` leaves the outgoing id
/// on the floor until the 30-minute stale sweep — the cost of "a hook is proof of
/// life" for a source with no short reap, NOT something the `identity()` on that
/// arm introduced.
#[test]
fn past_the_straggler_window_any_hook_arm_re_registers_including_a_turn_boundary_end() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let ended = AgentId::from_parts("hermes", "turn-after-finalize");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let past = t0 + STRAGGLER_TTL + Duration::from_secs(1);

    sess_end(&mut r, &mut scene, ended, false, t0, Transport::Hook);
    act_end(&mut r, &mut scene, ended, None, past, Transport::Hook);
    assert!(
        scene.agents.contains_key(&ended),
        "an ActivityEnd past the TTL re-registers — the pre-existing behaviour"
    );

    let with_id = AgentId::from_parts("hermes", "same-but-with-identity");
    sess_end(&mut r, &mut scene, with_id, false, t0, Transport::Hook);
    r.apply(
        &mut scene,
        AgentEvent::Identity {
            agent_id: with_id,
            source: "hermes".into(),
            session_id: "same-but-with-identity".into(),
            cwd: Some(PathBuf::from("/repo")),
            pid: None,
        },
        past,
        Transport::Hook,
    );
    assert!(
        scene.agents.contains_key(&with_id),
        "and so does an Identity — the two arms are not distinguishable here"
    );
}

#[test]
fn jsonl_child_session_start_within_the_straggler_window_is_gated_too() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts("claude-code", "parent-sess");
    let child = AgentId::from_parts("claude-code", "agent-late-file");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, parent);
    sess_end(&mut r, &mut scene, child, true, t0, Transport::Hook);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: "agent-late-file".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(200),
        Transport::Jsonl,
    );
    assert!(
        !scene.agents.contains_key(&child),
        "a JSONL child SessionStart racing its own hook Stop must not register"
    );
}

#[test]
fn non_child_session_end_alone_gates_a_parented_start() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts("claude-code", "parent-sess");
    let child = AgentId::from_parts("claude-code", "agent-nonchild-end");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, parent);
    sess_end(&mut r, &mut scene, child, false, t0, Transport::Hook);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: "agent-nonchild-end".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(200),
        Transport::Hook,
    );
    assert!(
        !scene.agents.contains_key(&child),
        "an as_child: false end arms ONLY the 5s #242 straggler window — that gate \
         alone must block the parented Start inside the TTL"
    );
}

#[test]
fn child_session_start_past_the_straggler_window_registers() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts("claude-code", "parent-sess");
    let child = AgentId::from_parts("claude-code", "agent-recycled");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start(&mut r, &mut scene, parent);
    sess_end(&mut r, &mut scene, child, true, t0, Transport::Hook);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: "agent-recycled".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + CHILD_END_GATE_TTL + Duration::from_secs(1),
        Transport::Hook,
    );
    assert!(
        scene.agents.contains_key(&child),
        "past the end gate a child SessionStart is a fresh registration"
    );
}

#[test]
fn a_parentless_session_start_inside_the_straggler_window_still_registers() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("reasonix", "/Users/dev/proj");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    sess_end(&mut r, &mut scene, id, false, t0, Transport::Hook);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "reasonix".into(),
            session_id: "/Users/dev/proj".into(),
            cwd: PathBuf::from("/Users/dev/proj"),
            parent_id: None,
        },
        t0 + Duration::from_millis(20),
        Transport::Hook,
    );
    assert!(
        scene.agents.contains_key(&id),
        "a parentless SessionStart must register straight through a fresh \
         straggler window (the Reasonix resurrect)"
    );
}

fn apply_hook_payload(
    r: &mut Reducer,
    scene: &mut SceneState,
    payload: serde_json::Value,
    now: SystemTime,
) {
    for ev in pixtuoid_core::source::decoder::decode_hook_payload(payload).expect("decodes") {
        r.apply(scene, ev, now, Transport::Hook);
    }
}

/// omp's session batches trail an Identity behind the parented Start; when
/// the child end gate refuses that Start, the parent-less Identity one event
/// later must not re-register the ended child as an orphan (the gate's
/// window would otherwise be one event wide for omp).
#[test]
fn a_trailing_identity_cannot_revive_the_child_its_start_was_refused_for() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    use serde_json::json;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let root_file = "/h/.omp/agent/sessions/-repo/2026-08-30T01-00-00-000Z_01a00000-0000-7000-8000-0000000000aa.jsonl";
    let child_file = "/h/.omp/agent/sessions/-repo/2026-08-30T01-00-00-000Z_01a00000-0000-7000-8000-0000000000aa/Alpha.jsonl";
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    apply_hook_payload(
        &mut r,
        &mut scene,
        json!({
            "type": "session_start", "sessionFile": root_file,
            "sessionId": "x", "cwd": "/repo", "_pixtuoid_source": "omp",
        }),
        t0,
    );
    apply_hook_payload(
        &mut r,
        &mut scene,
        json!({
            "type": "session_start", "sessionFile": child_file,
            "sessionId": "y", "cwd": "/repo", "_pixtuoid_source": "omp",
        }),
        t0 + Duration::from_secs(1),
    );
    let child = *scene
        .agents
        .keys()
        .find(|id| {
            scene.agents[id].session_id.contains("alpha")
                || scene.agents[id].session_id.contains("Alpha")
        })
        .expect("child registered");
    let stop = t0 + Duration::from_secs(2);
    apply_hook_payload(
        &mut r,
        &mut scene,
        json!({
            "type": "session_shutdown", "sessionFile": child_file,
            "sessionId": "y", "cwd": "/repo", "_pixtuoid_source": "omp",
        }),
        stop,
    );
    r.tick(
        &mut scene,
        stop + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(!scene.agents.contains_key(&child), "child GC'd");

    // Within the end gate, PAST the 5s straggler window: the batch's Start is
    // refused, and the trailing Identity must be refused with it.
    apply_hook_payload(
        &mut r,
        &mut scene,
        json!({
            "type": "session_start", "sessionFile": child_file,
            "sessionId": "y", "cwd": "/repo", "_pixtuoid_source": "omp",
        }),
        stop + Duration::from_secs(30),
    );
    assert!(
        !scene.agents.contains_key(&child),
        "the parent-less trailing Identity revived the gate-refused child"
    );
}

/// A claude-code child that started under its parent, stopped, and was GC'd:
/// its lifecycle remembers it. Returns the parent, the child and the stop.
fn ended_child(r: &mut Reducer, scene: &mut SceneState) -> (AgentId, AgentId, SystemTime) {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    use serde_json::json;
    let parent = AgentId::from_parts("claude-code", "01000000-0000-7000-8000-0000000000cc");
    let child = AgentId::from_parts("claude-code", "agent-a0000000000000001");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    apply_hook_payload(
        r,
        scene,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": "01000000-0000-7000-8000-0000000000cc",
            "_pixtuoid_source": "claude-code",
            "cwd": "/repo",
        }),
        t0,
    );
    apply_hook_payload(
        r,
        scene,
        json!({
            "hook_event_name": "SubagentStart",
            "session_id": "01000000-0000-7000-8000-0000000000cc",
            "agent_id": "a0000000000000001",
            "cwd": "/repo",
            "_pixtuoid_source": "claude-code",
        }),
        t0 + Duration::from_secs(1),
    );
    assert!(scene.agents.contains_key(&child), "child registered");
    let stop = t0 + Duration::from_secs(2);
    apply_hook_payload(
        r,
        scene,
        json!({
            "hook_event_name": "SubagentStop",
            "session_id": "01000000-0000-7000-8000-0000000000cc",
            "agent_id": "a0000000000000001",
            "_pixtuoid_source": "claude-code",
        }),
        stop,
    );
    r.tick(scene, stop + EXIT_GRACE_WINDOW + Duration::from_secs(1));
    assert!(!scene.agents.contains_key(&child), "child GC'd");
    (parent, child, stop)
}

#[test]
fn late_parented_restart_of_an_ended_child_is_gated_by_the_child_end_gate() {
    for transport in [Transport::Jsonl, Transport::Hook] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let (parent, child, stop) = ended_child(&mut r, &mut scene);

        let late_start = |r: &mut Reducer, scene: &mut SceneState, at: SystemTime| {
            r.apply(
                scene,
                AgentEvent::SessionStart {
                    agent_id: child,
                    source: "claude-code".into(),
                    session_id: "agent-a0000000000000001".into(),
                    cwd: PathBuf::from("/repo"),
                    parent_id: Some(parent),
                },
                at,
                transport,
            );
        };
        late_start(&mut r, &mut scene, stop + Duration::from_secs(30));
        assert!(
            !scene.agents.contains_key(&child),
            "a late parented restart of an ENDED child inside the end gate \
             must not re-register a phantom (#244-w2, {transport:?})"
        );

        late_start(
            &mut r,
            &mut scene,
            stop + CHILD_END_GATE_TTL + Duration::from_secs(1),
        );
        assert!(
            scene.agents.contains_key(&child),
            "past CHILD_END_GATE_TTL the registration resumes ({transport:?})"
        );
    }
}

/// A reasonix child its parent's Task drain walked out (b1) and the sweep
/// removed, so its end is only inferred. Returns the parent, the child
/// and the time it was gone.
fn walked_out_child(r: &mut Reducer, scene: &mut SceneState) -> (AgentId, AgentId, SystemTime) {
    use pixtuoid_core::state::reducer::{B1_CASCADE_GRACE, EXIT_GRACE_WINDOW};
    let parent = AgentId::from_parts("reasonix", "root");
    let child = AgentId::from_parts("reasonix", "root:subagent:sa_1");
    let session_start = |agent_id, parent_id| AgentEvent::SessionStart {
        agent_id,
        source: "reasonix".into(),
        session_id: "s".into(),
        cwd: PathBuf::from("/repo"),
        parent_id,
    };
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let at = |s| t0 + Duration::from_secs(s);
    r.apply(scene, session_start(parent, None), at(0), Transport::Hook);
    act_start(
        r,
        scene,
        parent,
        Some("call_1"),
        Some("Agent"),
        at(1),
        Transport::Hook,
    );
    r.apply(
        scene,
        session_start(child, Some(parent)),
        at(2),
        Transport::Hook,
    );
    act_end(r, scene, parent, Some("call_1"), at(3), Transport::Hook);
    let gone = at(3) + B1_CASCADE_GRACE + EXIT_GRACE_WINDOW + Duration::from_secs(1);
    r.tick(scene, at(3) + B1_CASCADE_GRACE);
    r.tick(scene, gone);
    assert!(
        !scene.agents.contains_key(&child),
        "the drain walked it out"
    );
    (parent, child, gone)
}

/// An end pixtuoid only inferred (its slot swept after its parent's cascade)
/// is a guess a later hook from the child disproves: a background child a
/// sibling's drain walked out, or reasonix's `continue_from` resuming the same
/// id. A transcript first-sight can be a replay, and a parent that is gone
/// re-admits nothing.
#[test]
fn a_hook_start_re_admits_a_child_whose_end_was_only_inferred() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let session_start = |agent_id, parent_id| AgentEvent::SessionStart {
        agent_id,
        source: "reasonix".into(),
        session_id: "s".into(),
        cwd: PathBuf::from("/repo"),
        parent_id,
    };

    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let (parent, child, gone) = walked_out_child(&mut r, &mut scene);
    let back = gone + Duration::from_secs(1);
    r.apply(
        &mut scene,
        session_start(child, Some(parent)),
        back,
        Transport::Hook,
    );
    assert_eq!(
        scene.agents.get(&child).map(|s| s.parent_id),
        Some(Some(parent)),
        "a hook proves the walked-out child alive"
    );

    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let (parent, child, gone) = walked_out_child(&mut r, &mut scene);
    let back = gone + Duration::from_secs(1);
    r.apply(
        &mut scene,
        session_start(child, Some(parent)),
        back,
        Transport::Jsonl,
    );
    assert!(
        !scene.agents.contains_key(&child),
        "a first-sight can be a replay"
    );

    // Its parent exiting, then removed.
    for after in [
        Duration::from_secs(1),
        EXIT_GRACE_WINDOW + Duration::from_secs(1),
    ] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let (parent, child, gone) = walked_out_child(&mut r, &mut scene);
        sess_end(&mut r, &mut scene, parent, false, gone, Transport::Hook);
        let back = gone + after;
        r.tick(&mut scene, back);
        r.apply(
            &mut scene,
            session_start(child, Some(parent)),
            back,
            Transport::Hook,
        );
        assert!(
            !scene.agents.contains_key(&child),
            "its parent is gone ({after:?})"
        );
    }
}

/// A hook that leads with Identity (dsh, opencode), or with a bare activity,
/// brings back a child whose end was only inferred as a start does, under the
/// parent it remembers.
#[test]
fn an_identity_or_bare_activity_re_admits_an_inferred_child_under_its_parent() {
    for led_by_identity in [true, false] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let (parent, child, gone) = walked_out_child(&mut r, &mut scene);
        let back = gone + Duration::from_secs(1);
        if led_by_identity {
            r.apply(
                &mut scene,
                AgentEvent::Identity {
                    agent_id: child,
                    source: "reasonix".into(),
                    session_id: "s".into(),
                    cwd: Some(PathBuf::from("/repo")),
                    pid: None,
                },
                back,
                Transport::Hook,
            );
        } else {
            act_start(
                &mut r,
                &mut scene,
                child,
                None,
                Some("read_file"),
                back,
                Transport::Hook,
            );
        }
        assert_eq!(
            scene.agents.get(&child).map(|s| s.parent_id),
            Some(Some(parent)),
            "led by identity: {led_by_identity}"
        );
    }
}

/// A re-admission takes its parent through the #240 cycle refusal: under
/// a parent since re-parented onto it, the child comes back parentless.
#[test]
fn a_readmission_under_a_parent_that_would_close_a_cycle_comes_back_parentless() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let (parent, child, gone) = walked_out_child(&mut r, &mut scene);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "reasonix".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(child),
        },
        gone,
        Transport::Hook,
    );
    assert_eq!(scene.agents[&parent].parent_id, Some(child), "precondition");
    let back = gone + Duration::from_secs(1);
    act_start(
        &mut r,
        &mut scene,
        child,
        None,
        Some("read_file"),
        back,
        Transport::Hook,
    );
    assert_eq!(scene.agents.get(&child).map(|s| s.parent_id), Some(None));
}

/// A re-admission records its link, so its next end restarts the gate's clock: a
/// late transcript echo of that end is refused like any child's.
#[test]
fn a_readmitted_childs_next_end_gates_from_that_end() {
    use pixtuoid_core::state::reducer::{B1_CASCADE_GRACE, EXIT_GRACE_WINDOW};
    for led_by_identity in [true, false] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let (parent, child, gone) = walked_out_child(&mut r, &mut scene);
        let back = gone + Duration::from_secs(1);
        if led_by_identity {
            r.apply(
                &mut scene,
                AgentEvent::Identity {
                    agent_id: child,
                    source: "reasonix".into(),
                    session_id: "s".into(),
                    cwd: Some(PathBuf::from("/repo")),
                    pid: None,
                },
                back,
                Transport::Hook,
            );
        } else {
            act_start(
                &mut r,
                &mut scene,
                child,
                None,
                Some("read_file"),
                back,
                Transport::Hook,
            );
        }
        assert!(scene.agents.contains_key(&child), "re-admitted");

        let dispatch = back + CHILD_END_GATE_TTL;
        act_start(
            &mut r,
            &mut scene,
            parent,
            Some("call_2"),
            Some("Agent"),
            dispatch,
            Transport::Hook,
        );
        let drain = dispatch + Duration::from_secs(1);
        act_end(
            &mut r,
            &mut scene,
            parent,
            Some("call_2"),
            drain,
            Transport::Hook,
        );
        r.tick(&mut scene, drain + B1_CASCADE_GRACE);
        let swept = drain + B1_CASCADE_GRACE + EXIT_GRACE_WINDOW + Duration::from_secs(1);
        r.tick(&mut scene, swept);
        assert!(!scene.agents.contains_key(&child), "walked out again");

        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: child,
                source: "reasonix".into(),
                session_id: "s".into(),
                cwd: PathBuf::from("/repo"),
                parent_id: Some(parent),
            },
            swept + Duration::from_secs(30),
            Transport::Jsonl,
        );
        assert!(
            !scene.agents.contains_key(&child),
            "the second end's echo registered (identity: {led_by_identity})"
        );
    }
}

/// A child's own SessionEnd is its CLI's word whether or not the decoder can
/// say `as_child` (opencode's `session.deleted`): its late echo stays refused.
#[test]
fn a_known_childs_own_end_is_reported_without_as_child() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts("opencode", "parent");
    let child = AgentId::from_parts("opencode", "child");
    let session_start = |agent_id, parent_id| AgentEvent::SessionStart {
        agent_id,
        source: "opencode".into(),
        session_id: "s".into(),
        cwd: PathBuf::from("/repo"),
        parent_id,
    };
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    r.apply(&mut scene, session_start(parent, None), t0, Transport::Hook);
    let born = t0 + Duration::from_secs(1);
    r.apply(
        &mut scene,
        session_start(child, Some(parent)),
        born,
        Transport::Hook,
    );
    let end = t0 + Duration::from_secs(2);
    sess_end(&mut r, &mut scene, child, false, end, Transport::Hook);
    let gone = end + EXIT_GRACE_WINDOW + Duration::from_secs(1);
    r.tick(&mut scene, gone);
    r.apply(
        &mut scene,
        session_start(child, Some(parent)),
        gone,
        Transport::Hook,
    );
    assert!(
        !scene.agents.contains_key(&child),
        "the ended child's echo registered"
    );
}

/// The identity-less fallback is no looser than the Identity's end gate (#244-w2):
/// a hook activity of an ended child, paired with its refused start or a bare
/// straggler, never synthesizes it back blank and parentless.
#[test]
fn inside_the_end_gate_no_hook_activity_synthesizes_an_ended_child() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let (parent, child, stop) = ended_child(&mut r, &mut scene);
    let at = stop + Duration::from_secs(30);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: "agent-a0000000000000001".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        at,
        Transport::Hook,
    );
    let straggle = |r: &mut Reducer, scene: &mut SceneState, at: SystemTime| {
        act_start(
            r,
            scene,
            child,
            None,
            Some("read_file"),
            at,
            Transport::Hook,
        );
    };
    for after in [Duration::from_millis(1), Duration::from_secs(1)] {
        straggle(&mut r, &mut scene, at + after);
        assert!(
            !scene.agents.contains_key(&child),
            "an activity {after:?} after the refused start synthesized the ended child"
        );
    }
    straggle(
        &mut r,
        &mut scene,
        stop + CHILD_END_GATE_TTL + Duration::from_secs(1),
    );
    assert!(
        scene.agents.contains_key(&child),
        "past the end gate a hook activity registers again"
    );
}

#[test]
fn parentless_revival_start_of_an_ended_codex_child_relinks_to_its_remembered_parent() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    use serde_json::json;
    for transport in [Transport::Jsonl, Transport::Hook] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let parent = AgentId::from_parts("codex", "parent-sess");
        let child_uuid = "02000000-0000-7000-8000-0000000000cd";
        let child = AgentId::from_parts("codex", child_uuid);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

        apply_hook_payload(
            &mut r,
            &mut scene,
            json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": "parent-sess",
                "_pixtuoid_source": "codex",
                "cwd": "/repo",
            }),
            t0,
        );
        apply_hook_payload(
            &mut r,
            &mut scene,
            json!({
                "hook_event_name": "SubagentStart",
                "session_id": "parent-sess",
                "agent_id": child_uuid,
                "cwd": "/repo",
                "_pixtuoid_source": "codex",
            }),
            t0 + Duration::from_secs(1),
        );
        assert_eq!(
            scene.agents.get(&child).map(|s| s.parent_id),
            Some(Some(parent)),
            "first life: child registered with the parent link ({transport:?})"
        );
        let stop = t0 + Duration::from_secs(2);
        apply_hook_payload(
            &mut r,
            &mut scene,
            json!({
                "hook_event_name": "SubagentStop",
                "session_id": "parent-sess",
                "agent_id": child_uuid,
                "_pixtuoid_source": "codex",
            }),
            stop,
        );
        r.tick(
            &mut scene,
            stop + EXIT_GRACE_WINDOW + Duration::from_secs(1),
        );
        assert!(
            !scene.agents.contains_key(&child),
            "child GC'd after its first life"
        );

        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: child,
                source: "codex".into(),
                session_id: child_uuid.into(),
                cwd: PathBuf::from("/repo"),
                parent_id: None,
            },
            stop + Duration::from_secs(20),
            transport,
        );
        assert_eq!(
            scene.agents.get(&child).map(|s| s.parent_id),
            Some(Some(parent)),
            "the parentless revival start must re-link to its \
             remembered parent, not register as an orphan ({transport:?})"
        );
    }
}

#[test]
fn a_multi_turn_child_idle_past_the_end_gate_still_revives_adopted_not_orphaned() {
    use pixtuoid_core::state::reducer::{CHILD_END_RELINK_TTL, EXIT_GRACE_WINDOW};
    use serde_json::json;
    for transport in [Transport::Jsonl, Transport::Hook] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let parent = AgentId::from_parts("codex", "parent-sess");
        let child_uuid = "02000000-0000-7000-8000-0000000000ce";
        let child = AgentId::from_parts("codex", child_uuid);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

        apply_hook_payload(
            &mut r,
            &mut scene,
            json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": "parent-sess",
                "_pixtuoid_source": "codex",
                "cwd": "/repo",
            }),
            t0,
        );
        apply_hook_payload(
            &mut r,
            &mut scene,
            json!({
                "hook_event_name": "SubagentStart",
                "session_id": "parent-sess",
                "agent_id": child_uuid,
                "cwd": "/repo",
                "_pixtuoid_source": "codex",
            }),
            t0 + Duration::from_secs(1),
        );
        let stop = t0 + Duration::from_secs(2);
        apply_hook_payload(
            &mut r,
            &mut scene,
            json!({
                "hook_event_name": "SubagentStop",
                "session_id": "parent-sess",
                "agent_id": child_uuid,
                "_pixtuoid_source": "codex",
            }),
            stop,
        );
        r.tick(
            &mut scene,
            stop + EXIT_GRACE_WINDOW + Duration::from_secs(1),
        );
        assert!(
            !scene.agents.contains_key(&child),
            "child GC'd ({transport:?})"
        );

        let revival = stop + CHILD_END_GATE_TTL + Duration::from_secs(1);
        let start = |agent_id| AgentEvent::SessionStart {
            agent_id,
            source: "codex".into(),
            session_id: child_uuid.into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        };
        r.apply(&mut scene, start(child), revival, transport);
        assert_eq!(
            scene.agents.get(&child).map(|s| s.parent_id),
            Some(Some(parent)),
            "a child idle past the END GATE must revive ADOPTED, not orphaned ({transport:?})"
        );

        let mut scene2 = SceneState::uniform(4);
        let mut r2 = Reducer::new();
        apply_hook_payload(
            &mut r2,
            &mut scene2,
            json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": "parent-sess",
                "_pixtuoid_source": "codex",
                "cwd": "/repo",
            }),
            t0,
        );
        apply_hook_payload(
            &mut r2,
            &mut scene2,
            json!({
                "hook_event_name": "SubagentStart",
                "session_id": "parent-sess",
                "agent_id": child_uuid,
                "cwd": "/repo",
                "_pixtuoid_source": "codex",
            }),
            t0 + Duration::from_secs(1),
        );
        apply_hook_payload(
            &mut r2,
            &mut scene2,
            json!({
                "hook_event_name": "SubagentStop",
                "session_id": "parent-sess",
                "agent_id": child_uuid,
                "_pixtuoid_source": "codex",
            }),
            stop,
        );
        r2.tick(
            &mut scene2,
            stop + EXIT_GRACE_WINDOW + Duration::from_secs(1),
        );
        r2.apply(
            &mut scene2,
            start(child),
            stop + CHILD_END_RELINK_TTL + Duration::from_secs(1),
            transport,
        );
        assert_eq!(
            scene2.agents.get(&child).map(|s| s.parent_id),
            Some(None),
            "past the relink budget the memory is gone, so the revival is parentless \
             ({transport:?}) — the retention is bounded, not a leak"
        );
    }
}

#[test]
fn parentless_session_start_enriching_a_parentless_child_slot_adopts_its_remembered_parent() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts("codex", "parent-sess");
    let child_uuid = "05000000-0000-7000-8000-0000000000d0";
    let child = AgentId::from_parts("codex", child_uuid);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let session_start = |agent_id, sid: &str, parent_id| AgentEvent::SessionStart {
        agent_id,
        source: "codex".into(),
        session_id: sid.into(),
        cwd: PathBuf::from("/repo"),
        parent_id,
    };

    r.apply(
        &mut scene,
        session_start(parent, "parent-sess", None),
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        session_start(child, child_uuid, Some(parent)),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    sess_end(
        &mut r,
        &mut scene,
        child,
        true,
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );
    let gone = t0 + Duration::from_secs(2) + EXIT_GRACE_WINDOW + Duration::from_secs(1);
    r.tick(&mut scene, gone);
    assert!(!scene.agents.contains_key(&child), "child GC'd");

    // Past the end gate, inside the parent memory.
    let late = t0 + Duration::from_secs(2) + CHILD_END_GATE_TTL + Duration::from_secs(1);
    act_start(
        &mut r,
        &mut scene,
        child,
        Some("t-straggler"),
        None,
        late,
        Transport::Hook,
    );
    assert_eq!(
        scene.agents.get(&child).map(|s| s.parent_id),
        Some(None),
        "precondition: the straggler re-registered the child parentless"
    );

    r.apply(
        &mut scene,
        session_start(child, child_uuid, None),
        late + Duration::from_secs(1),
        Transport::Jsonl,
    );
    assert_eq!(
        scene.agents.get(&child).map(|s| s.parent_id),
        Some(Some(parent)),
        "the enrichment path must adopt its remembered parent for a \
         parentless child slot"
    );
}

#[test]
fn an_ended_codex_childs_flat_first_sight_relinks_inside_the_end_gate() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    use serde_json::json;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let parent = AgentId::from_parts("codex", "parent-sess");
    let child_uuid = "03000000-0000-7000-8000-0000000000ce";
    let child = AgentId::from_parts("codex", child_uuid);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    apply_hook_payload(
        &mut r,
        &mut scene,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "parent-sess",
            "_pixtuoid_source": "codex",
            "cwd": "/repo",
        }),
        t0,
    );
    let subagent_stop = json!({
        "hook_event_name": "SubagentStop",
        "session_id": "parent-sess",
        "agent_id": child_uuid,
        "_pixtuoid_source": "codex",
    });
    apply_hook_payload(
        &mut r,
        &mut scene,
        json!({
            "hook_event_name": "SubagentStart",
            "session_id": "parent-sess",
            "agent_id": child_uuid,
            "cwd": "/repo",
            "_pixtuoid_source": "codex",
        }),
        t0 + Duration::from_secs(1),
    );
    let stop = t0 + Duration::from_secs(2);
    apply_hook_payload(&mut r, &mut scene, subagent_stop.clone(), stop);
    r.tick(
        &mut scene,
        stop + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(!scene.agents.contains_key(&child), "child GC'd");

    let straggler = stop + EXIT_GRACE_WINDOW + Duration::from_secs(2);
    apply_hook_payload(&mut r, &mut scene, subagent_stop, straggler);

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "codex".into(),
            session_id: child_uuid.into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        straggler + Duration::from_millis(500),
        Transport::Jsonl,
    );
    assert_eq!(
        scene.agents.get(&child).map(|s| s.parent_id),
        Some(Some(parent)),
        "the ended child's parentless flat first-sight must register \
         parent-LINKED to its remembered parent (#244-w1), not as an orphan phantom"
    );
}

#[test]
fn an_adopted_remembered_parent_still_runs_the_cycle_filter() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let p = AgentId::from_parts("codex", "p-root");
    let x = AgentId::from_parts("codex", "x-child");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let session_start = |agent_id, sid: &str, parent_id| AgentEvent::SessionStart {
        agent_id,
        source: "codex".into(),
        session_id: sid.into(),
        cwd: PathBuf::from("/repo"),
        parent_id,
    };

    r.apply(
        &mut scene,
        session_start(p, "p-root", None),
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        session_start(x, "x-child", Some(p)),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    sess_end(
        &mut r,
        &mut scene,
        x,
        true,
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );
    r.tick(
        &mut scene,
        t0 + Duration::from_secs(2) + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(!scene.agents.contains_key(&x), "X GC'd");

    r.apply(
        &mut scene,
        session_start(p, "p-root", Some(x)),
        t0 + Duration::from_secs(10),
        Transport::Hook,
    );
    assert_eq!(
        scene.agents.get(&p).map(|s| s.parent_id),
        Some(Some(x)),
        "precondition: P now dangles on the dead X"
    );

    r.apply(
        &mut scene,
        session_start(x, "x-child", None),
        t0 + Duration::from_secs(11),
        Transport::Jsonl,
    );
    let slot = scene.agents.get(&x).expect("X re-registers");
    assert_eq!(
        slot.parent_id, None,
        "an adopted remembered parent that would close a cycle must degrade to \
         parentless (the #240 filter runs on adopted links too)"
    );
}

#[test]
fn reasonix_resurrect_is_unaffected_by_another_ids_lifecycle() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let codex_parent = AgentId::from_parts("codex", "parent-sess");
    let codex_child = AgentId::from_parts("codex", "04000000-0000-7000-8000-0000000000cf");
    let rx = AgentId::from_parts("reasonix", "/Users/dev/proj");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    start(&mut r, &mut scene, codex_parent);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: codex_child,
            source: "codex".into(),
            session_id: "04000000-0000-7000-8000-0000000000cf".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(codex_parent),
        },
        t0,
        Transport::Hook,
    );
    sess_end(
        &mut r,
        &mut scene,
        codex_child,
        true,
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );

    sess_end(
        &mut r,
        &mut scene,
        rx,
        false,
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: rx,
            source: "reasonix".into(),
            session_id: "/Users/dev/proj".into(),
            cwd: PathBuf::from("/Users/dev/proj"),
            parent_id: None,
        },
        t0 + Duration::from_secs(2) + Duration::from_millis(20),
        Transport::Hook,
    );
    let slot = scene
        .agents
        .get(&rx)
        .expect("the Reasonix resurrect registers");
    assert_eq!(
        slot.parent_id, None,
        "a lifecycle record for a DIFFERENT id must never re-parent a Reasonix \
         session (its ids never get a parent link by construction)"
    );
}
