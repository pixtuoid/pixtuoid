use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::{CHILD_END_GATE_TTL, Reducer, STRAGGLER_TTL};

use crate::{HOOK_AND_TRANSCRIPT, act_end, act_start, sess_end, start, start_as};

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
    let parent = AgentId::from_parts(HOOK_AND_TRANSCRIPT, "parent-sess");
    let child = AgentId::from_parts(HOOK_AND_TRANSCRIPT, "agent-late-file");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    start_as(&mut r, &mut scene, parent, HOOK_AND_TRANSCRIPT);
    sess_end(&mut r, &mut scene, child, true, t0, Transport::Hook);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: HOOK_AND_TRANSCRIPT.into(),
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

const OMP_ROOT_FILE: &str = "/h/.omp/agent/sessions/-repo/2026-08-30T01-00-00-000Z_01a00000-0000-7000-8000-0000000000aa.jsonl";
const OMP_CHILD_FILE: &str = "/h/.omp/agent/sessions/-repo/2026-08-30T01-00-00-000Z_01a00000-0000-7000-8000-0000000000aa/Alpha.jsonl";

fn ended_omp_child(r: &mut Reducer, scene: &mut SceneState) -> (AgentId, AgentId, SystemTime) {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    use serde_json::json;
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let start = |sess: &str, file: &str| {
        json!({
            "type": "session_start", "sessionFile": file,
            "sessionId": sess, "cwd": "/repo", "_pixtuoid_source": "omp",
        })
    };
    apply_hook_payload(r, scene, start("x", OMP_ROOT_FILE), t0);
    let root = *scene.agents.keys().next().expect("the root registered");
    apply_hook_payload(
        r,
        scene,
        start("y", OMP_CHILD_FILE),
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
        r,
        scene,
        json!({
            "type": "session_shutdown", "sessionFile": OMP_CHILD_FILE,
            "sessionId": "y", "cwd": "/repo", "_pixtuoid_source": "omp",
        }),
        stop,
    );
    r.tick(scene, stop + EXIT_GRACE_WINDOW + Duration::from_secs(1));
    assert!(!scene.agents.contains_key(&child), "child GC'd");
    (root, child, stop)
}

fn omp_transcript_start(
    r: &mut Reducer,
    scene: &mut SceneState,
    (root, child): (AgentId, AgentId),
    at: SystemTime,
) {
    r.apply(
        scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "omp".into(),
            session_id: "y".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(root),
        },
        at,
        Transport::Jsonl,
    );
}

/// omp's bridge forwards each event live, with no buffer or replay
/// (install/omp_extension.ts), so a hook `session_start` 30 s after the child's
/// `session_shutdown` is omp starting it again; its transcript's first sight of
/// the same child inside the gate is the echo.
#[test]
fn an_omp_child_started_again_after_shutdown_is_readmitted_but_its_transcript_echo_is_not() {
    use serde_json::json;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let (root, child, stop) = ended_omp_child(&mut r, &mut scene);
    let late = stop + Duration::from_secs(30);
    omp_transcript_start(&mut r, &mut scene, (root, child), late);
    assert!(
        !scene.agents.contains_key(&child),
        "the transcript's late first sight of the ended child registered"
    );
    apply_hook_payload(
        &mut r,
        &mut scene,
        json!({
            "type": "session_start", "sessionFile": OMP_CHILD_FILE,
            "sessionId": "y", "cwd": "/repo", "_pixtuoid_source": "omp",
        }),
        late,
    );
    assert!(
        scene.agents.get(&child).and_then(|s| s.parent_id) == Some(root),
        "omp's live session_start was refused, or readmitted parentless"
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

/// A transcript's late first sight of a child that already ended is its echo
/// (#244); a hook start is not (`a_resumed_subagent_past_its_exit_is_readmitted_under_its_parent`).
#[test]
fn a_late_transcript_restart_of_an_ended_child_is_gated_by_the_child_end_gate() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let (root, child, stop) = ended_omp_child(&mut r, &mut scene);
    let ids = (root, child);
    omp_transcript_start(&mut r, &mut scene, ids, stop + Duration::from_secs(30));
    assert!(
        !scene.agents.contains_key(&child),
        "a late transcript restart of an ENDED child inside the end gate must not re-register a phantom"
    );
    omp_transcript_start(
        &mut r,
        &mut scene,
        ids,
        stop + CHILD_END_GATE_TTL + Duration::from_secs(1),
    );
    assert!(
        scene.agents.contains_key(&child),
        "past CHILD_END_GATE_TTL the registration resumes"
    );
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
/// say `as_child` (opencode's `session.deleted`): a hook activity inside its
/// straggler window stays refused, one past it readmits under its parent.
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
    let gone = end + EXIT_GRACE_WINDOW + Duration::from_millis(10);
    r.tick(&mut scene, gone);
    assert!(gone < end + STRAGGLER_TTL);
    let straggle = |r: &mut Reducer, scene: &mut SceneState, at| {
        act_start(r, scene, child, None, Some("read"), at, Transport::Hook);
    };
    straggle(&mut r, &mut scene, gone);
    assert!(
        !scene.agents.contains_key(&child),
        "a straggler of the reported end registered"
    );
    straggle(
        &mut r,
        &mut scene,
        end + STRAGGLER_TTL + Duration::from_millis(1),
    );
    assert_eq!(
        scene.agents.get(&child).and_then(|s| s.parent_id),
        Some(parent)
    );
}

/// Inside the end gate, a hook activity of an ended child readmits it under its
/// remembered parent, never blank and parentless; with that parent gone it
/// registers nothing.
#[test]
fn inside_the_end_gate_a_hook_activity_readmits_an_ended_child_only_under_its_parent() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    for parent_left in [false, true] {
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let (parent, child, stop) = ended_child(&mut r, &mut scene);
        if parent_left {
            sess_end(&mut r, &mut scene, parent, false, stop, Transport::Hook);
            r.tick(
                &mut scene,
                stop + EXIT_GRACE_WINDOW + Duration::from_secs(1),
            );
        }
        act_start(
            &mut r,
            &mut scene,
            child,
            None,
            Some("read_file"),
            stop + Duration::from_secs(30),
            Transport::Hook,
        );
        let readmitted = scene.agents.get(&child).map(|s| s.parent_id);
        let want = (!parent_left).then_some(Some(parent));
        assert_eq!(readmitted, want, "parent_left={parent_left}");
    }
}

/// A grok hook envelope, the shape its recorded subagent run carries.
fn grok_hook(event: &str, session: &str) -> serde_json::Value {
    serde_json::json!({
        "hookEventName": event, "sessionId": session, "workspaceRoot": "/repo",
        "_pixtuoid_source": "grok",
    })
}

/// A grok child started under its parent, in the recorded run's hook order.
/// Returns the parent, the child's session id, the child and its start.
fn linked_grok_child(
    r: &mut Reducer,
    scene: &mut SceneState,
) -> (AgentId, &'static str, AgentId, SystemTime) {
    let parent = AgentId::from_parts("grok", "parent-sess");
    let child_sess = "02000000-0000-7000-8000-0000000000cd";
    let child = AgentId::from_parts("grok", child_sess);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    apply_hook_payload(r, scene, grok_hook("user_prompt_submit", "parent-sess"), t0);
    let mut start = grok_hook("subagent_start", "parent-sess");
    start["subagentId"] = child_sess.into();
    let started = t0 + Duration::from_secs(1);
    apply_hook_payload(r, scene, start, started);
    assert_eq!(
        scene.agents.get(&child).map(|s| s.parent_id),
        Some(Some(parent)),
        "first life: child registered with the parent link"
    );
    (parent, child_sess, child, started)
}

/// A [`linked_grok_child`] that then ended and was GC'd; returns its stop in
/// place of its start.
fn ended_grok_child(
    r: &mut Reducer,
    scene: &mut SceneState,
) -> (AgentId, &'static str, AgentId, SystemTime) {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let (parent, child_sess, child, started) = linked_grok_child(r, scene);
    let stop = started + Duration::from_secs(1);
    let mut end = grok_hook("subagent_stop", child_sess);
    end["subagentId"] = child_sess.into();
    apply_hook_payload(r, scene, end, stop);
    apply_hook_payload(
        r,
        scene,
        grok_hook("session_end", child_sess),
        stop + Duration::from_millis(1),
    );
    r.tick(scene, stop + EXIT_GRACE_WINDOW + Duration::from_secs(1));
    assert!(!scene.agents.contains_key(&child), "child GC'd");
    (parent, child_sess, child, stop)
}

/// A grok child's next `user_prompt_submit` names no parent: inside the end
/// gate it is the CLI starting the child again, readmitted under its
/// remembered parent.
#[test]
fn a_grok_childs_parentless_next_start_is_readmitted_under_its_remembered_parent() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let (parent, child_sess, child, stop) = ended_grok_child(&mut r, &mut scene);
    apply_hook_payload(
        &mut r,
        &mut scene,
        grok_hook("user_prompt_submit", child_sess),
        stop + Duration::from_secs(20),
    );
    assert_eq!(
        scene.agents.get(&child).map(|s| s.parent_id),
        Some(Some(parent)),
        "the child's next turn was refused or orphaned"
    );
}

/// A linked grok child the stale sweep walks out, under a live parent, revives
/// in place on its next `user_prompt_submit`, though that names no parent.
#[test]
fn a_walking_out_grok_childs_parentless_next_start_revives_it_under_its_parent() {
    use pixtuoid_core::state::reducer::{EXIT_GRACE_WINDOW, STALE_WAITING_TIMEOUT};
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let (parent, child_sess, child, started) = linked_grok_child(&mut r, &mut scene);
    let sweep = started + STALE_WAITING_TIMEOUT + Duration::from_secs(1);
    let mut busy = grok_hook("pre_tool_use", "parent-sess");
    busy["toolName"] = "read_file".into();
    apply_hook_payload(&mut r, &mut scene, busy, sweep - Duration::from_secs(1));
    r.tick(&mut scene, sweep);
    assert!(scene.agents[&parent].exiting_at.is_none(), "parent swept");
    assert!(scene.agents[&child].exiting_at.is_some(), "child not swept");

    apply_hook_payload(
        &mut r,
        &mut scene,
        grok_hook("user_prompt_submit", child_sess),
        sweep + EXIT_GRACE_WINDOW / 2,
    );
    let slot = &scene.agents[&child];
    assert!(
        slot.exiting_at.is_none(),
        "the child's next turn left it walking out"
    );
    assert_eq!(slot.parent_id, Some(parent));
}

/// A grok child's transcript names no parent, so the watcher's late first
/// sight of it is parentless: inside the end gate it is still the dead child's
/// echo (#244).
#[test]
fn an_ended_grok_childs_late_parentless_first_sight_is_refused() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let (_, child_sess, child, stop) = ended_grok_child(&mut r, &mut scene);
    let first_sight = stop + STRAGGLER_TTL + Duration::from_secs(1);
    assert!(first_sight < stop + CHILD_END_GATE_TTL);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "grok".into(),
            session_id: child_sess.into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        first_sight,
        Transport::Jsonl,
    );
    assert!(
        !scene.agents.contains_key(&child),
        "the ended child's late first sight came back"
    );
}

/// Past the end gate, a grok child's tool hook decoded ahead of its turn's
/// `user_prompt_submit` registers the child under its remembered parent too.
#[test]
fn a_grok_childs_tool_hook_past_the_end_gate_registers_under_its_remembered_parent() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let (parent, child_sess, child, stop) = ended_grok_child(&mut r, &mut scene);
    let mut tool = grok_hook("pre_tool_use", child_sess);
    tool["toolName"] = "read_file".into();
    apply_hook_payload(
        &mut r,
        &mut scene,
        tool,
        stop + CHILD_END_GATE_TTL + Duration::from_secs(1),
    );
    assert_eq!(
        scene.agents.get(&child).map(|s| s.parent_id),
        Some(Some(parent)),
        "the child's next turn came back an orphan"
    );
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
        t0 + Duration::from_secs(2) + CHILD_END_GATE_TTL + Duration::from_secs(1),
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

/// The hook shapes a claude-code subagent's turns arrive in
/// (`claude/fixtures/subagent-hermetic`), around one root session.
struct CcTurns {
    root: AgentId,
    child: AgentId,
    t0: SystemTime,
}

impl CcTurns {
    const SID: &'static str = "01000000-0000-7000-8000-0000000000cc";
    const AGENT: &'static str = "a0000000000000001";

    fn new(r: &mut Reducer, scene: &mut SceneState) -> Self {
        let t = Self {
            root: AgentId::from_parts("claude-code", Self::SID),
            child: AgentId::from_parts("claude-code", &format!("agent-{}", Self::AGENT)),
            t0: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
        };
        t.hook(
            r,
            scene,
            serde_json::json!({"hook_event_name": "SessionStart"}),
            t.t0,
        );
        t
    }

    fn hook(
        &self,
        r: &mut Reducer,
        scene: &mut SceneState,
        mut v: serde_json::Value,
        at: SystemTime,
    ) {
        v["session_id"] = serde_json::json!(Self::SID);
        v["cwd"] = serde_json::json!("/repo");
        v["_pixtuoid_source"] = serde_json::json!("claude-code");
        apply_hook_payload(r, scene, v, at);
    }

    fn start(&self, r: &mut Reducer, scene: &mut SceneState, at: SystemTime) {
        let v = serde_json::json!({"hook_event_name": "SubagentStart", "agent_id": Self::AGENT,
            "agent_type": "general-purpose"});
        self.hook(r, scene, v, at);
    }

    fn stop(&self, r: &mut Reducer, scene: &mut SceneState, at: SystemTime) {
        let v = serde_json::json!({"hook_event_name": "SubagentStop", "agent_id": Self::AGENT,
            "agent_transcript_path": format!("/p/{}/subagents/agent-{}.jsonl", Self::SID, Self::AGENT)});
        self.hook(r, scene, v, at);
    }

    fn tool(&self, r: &mut Reducer, scene: &mut SceneState, at: SystemTime) {
        let v = serde_json::json!({"hook_event_name": "PreToolUse", "agent_id": Self::AGENT,
            "agent_type": "general-purpose", "tool_name": "Read", "tool_use_id": "toolu_r",
            "tool_input": {"file_path": "/repo/NOTE.txt"}});
        self.hook(r, scene, v, at);
    }

    fn live(&self, scene: &SceneState) -> bool {
        scene
            .agents
            .get(&self.child)
            .is_some_and(|s| s.exiting_at.is_none())
    }
}

/// #1701: CC resumes a subagent under the same `agent_id`, a SubagentStart per
/// turn. Inside `EXIT_GRACE_WINDOW`, its exit still animating, it revives in place.
#[test]
fn a_resumed_subagent_revives_in_place_while_its_exit_animates() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    cc.start(&mut r, &mut scene, cc.t0 + Duration::from_secs(1));
    let stop = cc.t0 + Duration::from_secs(2);
    cc.stop(&mut r, &mut scene, stop);
    assert!(!cc.live(&scene), "its Stop ends the turn");
    cc.start(&mut r, &mut scene, stop + Duration::from_millis(37));
    assert!(cc.live(&scene), "the next turn's Start revives it");
    assert_eq!(scene.agents[&cc.child].parent_id, Some(cc.root));
}

/// A subagent first seen by its own tool hook registers parentless; its
/// SubagentStart reordered after its Stop is that life's, not a new turn's.
#[test]
fn an_identity_first_subagent_is_not_revived_by_its_delayed_start() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    cc.tool(&mut r, &mut scene, cc.t0 + Duration::from_secs(1));
    assert_eq!(scene.agents[&cc.child].parent_id, None, "Identity-first");
    let stop = cc.t0 + Duration::from_secs(2);
    cc.stop(&mut r, &mut scene, stop);
    cc.start(&mut r, &mut scene, stop + Duration::from_millis(40));
    assert!(
        !cc.live(&scene),
        "a reordered Start revived a finished life"
    );
}

/// Past its exit window and the straggler window, inside the child-end gate, a
/// hook start is a new life under a live parent.
#[test]
fn a_resumed_subagent_past_its_exit_is_readmitted_under_its_parent() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    cc.start(&mut r, &mut scene, cc.t0 + Duration::from_secs(1));
    let stop = cc.t0 + Duration::from_secs(2);
    cc.stop(&mut r, &mut scene, stop);
    r.tick(
        &mut scene,
        stop + EXIT_GRACE_WINDOW + Duration::from_millis(1),
    );
    assert!(
        !scene.agents.contains_key(&cc.child),
        "gone after its walkout"
    );
    let resume = stop + Duration::from_millis(16_874);
    assert!(resume > stop + STRAGGLER_TTL && resume < stop + CHILD_END_GATE_TTL);
    cc.start(&mut r, &mut scene, resume);
    assert!(cc.live(&scene), "the resumed turn was refused");
    assert_eq!(scene.agents[&cc.child].parent_id, Some(cc.root));
}

/// The same resume with its parent session over proves nothing to readmit under.
#[test]
fn a_resumed_subagent_whose_parent_left_is_refused() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    cc.start(&mut r, &mut scene, cc.t0 + Duration::from_secs(1));
    let stop = cc.t0 + Duration::from_secs(2);
    cc.stop(&mut r, &mut scene, stop);
    cc.hook(
        &mut r,
        &mut scene,
        serde_json::json!({"hook_event_name": "SessionEnd"}),
        stop,
    );
    r.tick(
        &mut scene,
        stop + EXIT_GRACE_WINDOW + Duration::from_millis(1),
    );
    cc.start(&mut r, &mut scene, stop + Duration::from_millis(16_874));
    assert!(
        !scene.agents.contains_key(&cc.child),
        "readmitted under a parent that left"
    );
}

/// A background subagent with no SubagentStop of its own leaves with its
/// session's end.
#[test]
fn a_background_subagent_leaves_with_its_session() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    cc.start(&mut r, &mut scene, cc.t0 + Duration::from_secs(1));
    cc.hook(
        &mut r,
        &mut scene,
        serde_json::json!({"hook_event_name": "SessionEnd", "reason": "prompt_input_exit"}),
        cc.t0 + Duration::from_secs(9),
    );
    assert!(!cc.live(&scene), "the killed subagent outlived its session");
}

/// A subagent its session's end cascaded out is not revived by a Start that
/// lands while both walk out.
#[test]
fn a_cascaded_subagent_is_not_revived_by_a_late_start() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    cc.start(&mut r, &mut scene, cc.t0 + Duration::from_secs(1));
    let end = cc.t0 + Duration::from_secs(2);
    cc.hook(
        &mut r,
        &mut scene,
        serde_json::json!({"hook_event_name": "SessionEnd"}),
        end,
    );
    cc.start(&mut r, &mut scene, end + Duration::from_millis(37));
    assert!(
        !cc.live(&scene),
        "a late Start un-exited a cascaded subagent"
    );
}

/// A revive is a live link again: the old end no longer ages its parent memory
/// out, so a hook after a much later end still finds the parent.
#[test]
fn a_revived_subagent_keeps_its_parent_memory_past_the_old_ends_ttl() {
    use pixtuoid_core::state::reducer::{CHILD_END_RELINK_TTL, EXIT_GRACE_WINDOW};
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    cc.start(&mut r, &mut scene, cc.t0 + Duration::from_secs(1));
    let stop = cc.t0 + Duration::from_secs(2);
    cc.stop(&mut r, &mut scene, stop);
    cc.start(&mut r, &mut scene, stop + Duration::from_millis(37));
    let later = stop + CHILD_END_RELINK_TTL + Duration::from_secs(1);
    r.tick(&mut scene, later);
    cc.stop(&mut r, &mut scene, later);
    r.tick(
        &mut scene,
        later + EXIT_GRACE_WINDOW + Duration::from_millis(1),
    );
    cc.tool(
        &mut r,
        &mut scene,
        later + STRAGGLER_TTL + Duration::from_secs(1),
    );
    assert_eq!(
        scene.agents.get(&cc.child).and_then(|s| s.parent_id),
        Some(cc.root),
        "the revived life's parent memory aged out with its old end"
    );
}

/// Between its walkout and the straggler window's end a resumed subagent is
/// readmitted too: a reordered event of its last life would have landed within
/// milliseconds of the Stop, not after the walkout.
#[test]
fn a_resumed_subagent_between_its_walkout_and_the_straggler_ttl_is_readmitted() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    cc.start(&mut r, &mut scene, cc.t0 + Duration::from_secs(1));
    let stop = cc.t0 + Duration::from_secs(2);
    cc.stop(&mut r, &mut scene, stop);
    let resume = stop + EXIT_GRACE_WINDOW + (STRAGGLER_TTL - EXIT_GRACE_WINDOW) / 2;
    r.tick(&mut scene, resume);
    assert!(
        !scene.agents.contains_key(&cc.child),
        "gone after its walkout"
    );
    cc.start(&mut r, &mut scene, resume);
    assert_eq!(
        scene.agents.get(&cc.child).map(|s| s.parent_id),
        Some(Some(cc.root)),
        "a resume after the walkout, inside STRAGGLER_TTL, was refused"
    );
}

/// #242: a short subagent's Stop decoded ahead of its own Start, milliseconds
/// apart, registers nothing.
#[test]
fn a_subagents_start_decoded_after_its_own_stop_is_refused() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    let stop = cc.t0 + Duration::from_secs(1);
    cc.stop(&mut r, &mut scene, stop);
    cc.start(&mut r, &mut scene, stop + Duration::from_millis(5));
    assert!(
        !scene.agents.contains_key(&cc.child),
        "a reordered Start minted a phantom"
    );
}

/// A Stop that lands on a subagent still walking out ends a turn whose own
/// Start comes after it, reordered (#242): that Start is no next turn.
#[test]
fn a_turns_start_reordered_after_its_stop_onto_a_walking_out_subagent_is_no_revive() {
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    cc.start(&mut r, &mut scene, cc.t0 + Duration::from_secs(1));
    let stop = cc.t0 + Duration::from_secs(2);
    cc.stop(&mut r, &mut scene, stop);
    let next_stop = stop + Duration::from_secs(1);
    cc.stop(&mut r, &mut scene, next_stop);
    cc.start(&mut r, &mut scene, next_stop + Duration::from_millis(5));
    assert!(!cc.live(&scene), "a turn's reordered Start revived it");
}

/// The walkout bound from below: a Start just inside it, its Stop decoded
/// first and no slot ever, is still #242's reorder.
#[test]
fn a_subagents_start_just_inside_the_walkout_after_its_own_stop_is_refused() {
    use pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    let stop = cc.t0 + Duration::from_secs(1);
    cc.stop(&mut r, &mut scene, stop);
    cc.start(
        &mut r,
        &mut scene,
        stop + EXIT_GRACE_WINDOW - Duration::from_millis(1),
    );
    assert!(
        !scene.agents.contains_key(&cc.child),
        "the walkout bound let a reorder in"
    );
}

/// A subagent blocked on its own permission prompt keeps its delegating parent
/// alive with it: no probe vouches either (Windows), and the parent's own Active
/// window would otherwise sweep both mid-gate.
#[test]
fn a_parent_waits_with_its_subagent_on_a_permission_prompt() {
    use pixtuoid_core::state::reducer::STALE_ACTIVE_TIMEOUT;
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let cc = CcTurns::new(&mut r, &mut scene);
    let t1 = cc.t0 + Duration::from_secs(1);
    let dispatch = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Agent",
        "tool_use_id": "toolu_a", "tool_input": {"description": "d", "prompt": "p",
        "subagent_type": "general-purpose"}});
    cc.hook(&mut r, &mut scene, dispatch, t1);
    cc.start(&mut r, &mut scene, t1);
    let ask = serde_json::json!({"hook_event_name": "PermissionRequest", "agent_id": CcTurns::AGENT,
        "agent_type": "general-purpose", "tool_name": "Bash", "tool_input": {"command": "ls"}});
    cc.hook(&mut r, &mut scene, ask, t1 + Duration::from_secs(1));
    r.tick(
        &mut scene,
        t1 + Duration::from_secs(1) + STALE_ACTIVE_TIMEOUT + Duration::from_secs(1),
    );
    assert!(cc.live(&scene), "the waiting subagent was swept");
    assert!(
        scene.agents[&cc.root].exiting_at.is_none(),
        "the parent waiting on its subagent's prompt was swept"
    );
}
