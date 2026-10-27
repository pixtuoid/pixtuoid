//! omp's bridge-extension rounds, recorded from real `omp` runs with the
//! extension installed (#951). They live here rather than under
//! `sources/fixtures/omp/` because the hook decoder derives its keys through
//! the watcher's `normalize_path_key` FOLD — platform-dependent on Windows,
//! where omp's stems carry case — while the conformance goldens are
//! deliberately platform-invariant. (Two rounds also break conformance's
//! shape anyway: a resume's file predates the run, and a task run is two
//! sprites.)

use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::Drive;
use pixtuoid_core::id::normalize_path_key;
use pixtuoid_core::source::omp::{decode_omp_line, omp_id_from_path};
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::{EXIT_GRACE_WINDOW, Reducer};

/// A raw path's session key through the decoder's own fold — the ONE way an
/// expectation may be spelled here (a literal holds on Unix and reds only in
/// `windows-test`).
fn folded_key(raw: &str) -> String {
    omp_id_from_path(std::path::Path::new(&normalize_path_key(raw)))
}

fn lines(scenario: &str) -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join(format!("omp/fixtures/{scenario}/hook-payloads.jsonl")),
    )
}

fn events(scenario: &str) -> Vec<AgentEvent> {
    let d = Drive::hooks().lines(lines(scenario));
    d.assert_clean(scenario);
    d.events
}

/// The issue's empty-session criterion: a session that shuts down before any
/// assistant message writes no `session_exit` (at the recorded version, no
/// transcript at all), so the bridge's `session_shutdown` is the only end
/// signal, and it must remove the slot on the exit grace, not the stale sweep.
#[test]
fn an_empty_session_round_registers_then_leaves_on_the_exit_grace() {
    let evs = events("empty-session-recorded");
    let id = evs[0].agent_id();
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for (i, ev) in evs.into_iter().enumerate() {
        r.apply(
            &mut scene,
            ev,
            t0 + Duration::from_secs(i as u64),
            Transport::Hook,
        );
    }
    assert!(
        scene
            .agents
            .get(&id)
            .is_some_and(|s| s.exiting_at.is_some()),
        "the bridge shutdown must start the walkout"
    );
    r.tick(
        &mut scene,
        t0 + Duration::from_secs(2) + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(
        !scene.agents.contains_key(&id),
        "an empty session leaves on the exit grace, not the stale sweep"
    );
}

/// `omp -c` in a fresh process fires `session_start` on the SAME
/// stem-keyed id the earlier life used — the ended session walks back in
/// through ordinary parentless re-registration (registry row doc).
#[test]
fn a_resumed_session_re_registers_after_its_earlier_end() {
    let evs = events("resume-recorded");
    let id = evs[0].agent_id();
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    // First life: the recorded round is start → shutdown; let the slot leave.
    for (i, ev) in events("resume-recorded").into_iter().enumerate() {
        r.apply(
            &mut scene,
            ev,
            t0 + Duration::from_secs(i as u64),
            Transport::Hook,
        );
    }
    r.tick(
        &mut scene,
        t0 + Duration::from_secs(2) + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(!scene.agents.contains_key(&id), "first life left");

    // Second life: the SAME recorded start must register again.
    let t1 = t0 + Duration::from_secs(600);
    r.apply(&mut scene, evs[0].clone(), t1, Transport::Hook);
    assert!(
        scene.agents.contains_key(&id),
        "a resume's session_start re-registers the ended id"
    );
}

/// A task run under the bridge: the parent AND the in-process child each fire
/// their own lifecycle, the child keyed by the NESTED file so both transports
/// mint one id, parent-linked by path, both payloads carrying the ONE omp
/// `_pid` on the wire.
#[test]
fn a_task_round_is_two_linked_lifecycles_under_one_pid() {
    let evs = events("task-recorded");
    let starts: Vec<&AgentEvent> = evs
        .iter()
        .filter(|e| matches!(e, AgentEvent::SessionStart { .. }))
        .collect();
    let [
        AgentEvent::SessionStart {
            agent_id: parent,
            session_id: parent_key,
            parent_id: None,
            ..
        },
        AgentEvent::SessionStart {
            agent_id: child,
            session_id: child_key,
            parent_id: Some(linked),
            ..
        },
    ] = starts.as_slice()
    else {
        panic!("expected a parentless start then a linked child start: {starts:?}");
    };
    assert_eq!(linked, parent, "the child links to the parent by path");
    let child_file = lines("task-recorded")
        .iter()
        .find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            let f = v["sessionFile"].as_str()?;
            f.contains("SayHi").then(|| f.to_string())
        })
        .expect("the child payload names its nested file");
    assert_eq!(
        *child_key,
        folded_key(&child_file),
        "the child keys on the nested stem chain, same as the transcript side"
    );
    assert!(
        child_key.starts_with(&**parent_key),
        "the chain nests under the parent's own key: {child_key} vs {parent_key}"
    );
    assert_eq!(*child, AgentId::from_parts("omp", child_key));
    assert!(
        child_key.ends_with("/sayhi") || child_key.ends_with("/SayHi"),
        "the chain ends on the nested task file's stem: {child_key}"
    );

    let ends: Vec<bool> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SessionEnd { as_child, .. } => Some(*as_child),
            _ => None,
        })
        .collect();
    assert_eq!(
        ends,
        vec![false, true],
        "the root ends as a root, the nested child as a child"
    );

    let pids: std::collections::BTreeSet<i64> = lines("task-recorded")
        .iter()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).expect("valid json")["_pid"]
                .as_i64()
                .expect("the extension stamps _pid")
        })
        .collect();
    assert_eq!(pids.len(), 1, "extensions run in-process: one pid");
}

/// The registry row's load-bearing claim, pinned hermetically per recorded
/// round: BOTH transports mint one folded AgentId, and the approval round's
/// Start twins — separated by human latency — count ONE call through a real
/// `Reducer`.
#[test]
fn each_bridge_round_coalesces_both_transports_onto_one_id() {
    for scenario in [
        "approval-recorded",
        "denial-recorded",
        "bridge-run-recorded",
        "permission-hermetic",
        "tool-run-hermetic",
    ] {
        let dir = super::captures::sources_root().join(format!("omp/fixtures/{scenario}"));
        let transcript = super::captures::transcripts_in(&dir)
            .into_iter()
            .next()
            .expect("each relocated round ships its transcript");
        let tpath = normalize_path_key(&transcript.to_string_lossy());
        let hook_events = events(scenario);
        let id = hook_events[0].agent_id();
        assert_eq!(
            id,
            AgentId::from_parts("omp", &folded_key(&tpath)),
            "{scenario}: the hook id must be the transcript file's own folded key"
        );

        let jsonl: Vec<AgentEvent> = super::captures::fixture_lines(&transcript)
            .into_iter()
            .flat_map(|line| {
                let v: serde_json::Value = serde_json::from_str(&line).expect("fixture json");
                decode_omp_line(&tpath, "omp", v)
            })
            .collect();
        for ev in &jsonl {
            assert_eq!(ev.agent_id(), id, "{scenario}: transcript half split");
        }
        for ev in &hook_events {
            assert_eq!(ev.agent_id(), id, "{scenario}: hook half split");
        }

        // The recorded human ordering: whole-transcript-first would end the
        // FIRST life, making the hook round a resurrect that restarts counting.
        let cut = jsonl
            .iter()
            .position(|e| matches!(e, AgentEvent::ToolUseEnd { .. }))
            .unwrap_or(jsonl.len());
        let mut scene = SceneState::uniform(4);
        let mut r = Reducer::new();
        let mut at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let hook_tail = hook_events.len().saturating_sub(1);
        let mut apply = |r: &mut Reducer, scene: &mut SceneState, ev: AgentEvent, tp| {
            r.apply(scene, ev, at, tp);
            at += Duration::from_secs(1);
        };
        apply(&mut r, &mut scene, hook_events[0].clone(), Transport::Hook);
        for ev in &jsonl[..cut] {
            apply(&mut r, &mut scene, ev.clone(), Transport::Jsonl);
        }
        for ev in &hook_events[1..hook_tail] {
            apply(&mut r, &mut scene, ev.clone(), Transport::Hook);
        }
        for ev in &jsonl[cut..] {
            apply(&mut r, &mut scene, ev.clone(), Transport::Jsonl);
        }
        apply(
            &mut r,
            &mut scene,
            hook_events[hook_tail].clone(),
            Transport::Hook,
        );
        if !matches!(scenario, "bridge-run-recorded" | "tool-run-hermetic") {
            // One gated bash call per round; the cross-transport twins and the
            // approval resume must fold into ONE count.
            let count = scene
                .agents
                .get(&id)
                .expect("the round's slot survives to the shutdown walkout")
                .tool_call_count;
            assert_eq!(count, 1, "{scenario}: the approval round re-counted");
        }
    }
}

/// A real in-TUI `/new`: the switch fires with the NEW session already
/// current, and the decoder turns it into End(previous) + Start(current) —
/// the previous slot leaves without waiting for the stale sweep.
#[test]
fn a_recorded_switch_ends_the_previous_session_and_starts_the_current() {
    let evs = events("new-session-hermetic");
    let raw: Vec<serde_json::Value> = lines("new-session-hermetic")
        .iter()
        .map(|l| serde_json::from_str(l).expect("valid json"))
        .collect();
    let prev_id = AgentId::from_parts(
        "omp",
        &folded_key(raw[1]["previousSessionFile"].as_str().expect("named")),
    );
    let cur_id = AgentId::from_parts(
        "omp",
        &folded_key(raw[1]["sessionFile"].as_str().expect("named")),
    );
    let kinds: Vec<String> = evs
        .iter()
        .map(|e| match e {
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == prev_id => "start-prev",
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == cur_id => "start-cur",
            AgentEvent::SessionEnd { agent_id, .. } if *agent_id == prev_id => "end-prev",
            AgentEvent::SessionEnd { agent_id, .. } if *agent_id == cur_id => "end-cur",
            // The pid carrier behind each session arm
            // (`decode_omp_hook_payload`'s doc owns the story).
            AgentEvent::Identity { agent_id, .. } if *agent_id == cur_id => "identity-cur",
            AgentEvent::Identity { agent_id, .. } if *agent_id == prev_id => "identity-prev",
            other => panic!("unexpected event in the switch round: {other:?}"),
        })
        .map(str::to_string)
        .collect();
    assert_eq!(
        kinds,
        [
            "start-prev",
            "identity-prev",
            "end-prev",
            "start-cur",
            "identity-cur",
            "end-cur",
        ],
        "the switch decodes as End(previous) + Start(current), each session arm trailing its Identity"
    );
}
