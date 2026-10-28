//! omp's bridge-extension rounds, recorded from real `omp` runs with the
//! extension installed (#951). They live here rather than under
//! `sources/fixtures/omp/` because the hook decoder derives its keys through
//! the watcher's `normalize_path_key` FOLD — platform-dependent on Windows,
//! where omp's stems carry case — while the conformance goldens are
//! deliberately platform-invariant.

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

/// The hook payloads of the capture at `capture`, under `tests/sources`.
fn lines_at(capture: &str) -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join(capture)
            .join("hook-payloads.jsonl"),
    )
}

fn lines(scenario: &str) -> Vec<String> {
    lines_at(&format!("omp/fixtures/{scenario}"))
}

fn events_at(capture: &str) -> Vec<AgentEvent> {
    let d = Drive::hooks().lines(lines_at(capture));
    d.assert_clean(capture);
    d.events
}

fn events(scenario: &str) -> Vec<AgentEvent> {
    events_at(&format!("omp/fixtures/{scenario}"))
}

/// The session `new-session-hermetic`'s switch names, by `key`: `sessionFile`
/// the one it starts, `previousSessionFile` the one it leaves.
fn switched(key: &str) -> AgentId {
    let raw: Vec<serde_json::Value> = lines("new-session-hermetic")
        .iter()
        .map(|l| serde_json::from_str(l).expect("valid json"))
        .collect();
    let switch = raw
        .iter()
        .find(|v| v["type"] == "session_switch")
        .expect("the round switches sessions");
    AgentId::from_parts("omp", &folded_key(switch[key].as_str().expect("named")))
}

/// The issue's empty-session criterion: a session that shuts down before any
/// assistant message writes no `session_exit`, so the bridge's
/// `session_shutdown` is the only end signal, and it must remove the slot on
/// the exit grace, not the stale sweep. `/new`'s session is that one: the
/// round quits before prompting it.
#[test]
fn an_empty_session_round_registers_then_leaves_on_the_exit_grace() {
    let evs = events("new-session-hermetic");
    let id = switched("sessionFile");
    let last = evs.len() as u64;
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
        t0 + Duration::from_secs(last) + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(
        !scene.agents.contains_key(&id),
        "an empty session leaves on the exit grace, not the stale sweep"
    );
}

/// `omp --resume` in a fresh process fires `session_start` on the SAME
/// stem-keyed id the earlier life used — the ended session walks back in
/// through ordinary parentless re-registration (registry row doc).
#[test]
fn a_resumed_session_re_registers_after_its_earlier_end() {
    let evs = events("resume-hermetic");
    let id = evs[0].agent_id();
    let resumed = evs
        .iter()
        .rposition(|e| matches!(e, AgentEvent::SessionStart { .. }))
        .filter(|&at| at > 0)
        .expect("the resume fires its own session_start");
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    // First life: the recorded round up to the resume; let the slot leave.
    for (i, ev) in evs[..resumed].iter().cloned().enumerate() {
        r.apply(
            &mut scene,
            ev,
            t0 + Duration::from_secs(i as u64),
            Transport::Hook,
        );
    }
    r.tick(
        &mut scene,
        t0 + Duration::from_secs(resumed as u64) + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(!scene.agents.contains_key(&id), "first life left");

    // Second life: the resume's start must register the SAME id again.
    assert_eq!(
        evs[resumed].agent_id(),
        id,
        "the resume keys on the same file"
    );
    let t1 = t0 + Duration::from_secs(600);
    r.apply(&mut scene, evs[resumed].clone(), t1, Transport::Hook);
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
    const TASK: &str = "delegation/fixtures/omp";
    let evs = events_at(TASK);
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
    let raw: Vec<serde_json::Value> = lines_at(TASK)
        .iter()
        .map(|l| serde_json::from_str(l).expect("valid json"))
        .collect();
    let files: Vec<&str> = raw
        .iter()
        .filter(|v| v["type"] == "session_start")
        .filter_map(|v| v["sessionFile"].as_str())
        .collect();
    let [root_file, child_file] = files[..] else {
        panic!("two session_start payloads, each naming its file: {files:?}");
    };
    assert!(
        child_file.starts_with(root_file.trim_end_matches(".jsonl")),
        "the child's file nests under the root's: {child_file} vs {root_file}"
    );
    assert_eq!(
        *child_key,
        folded_key(child_file),
        "the child keys on the nested stem chain, same as the transcript side"
    );
    assert!(
        child_key.starts_with(&**parent_key),
        "the chain nests under the parent's own key: {child_key} vs {parent_key}"
    );
    assert_eq!(*child, AgentId::from_parts("omp", child_key));
    let stem = std::path::Path::new(child_file)
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("a named file");
    assert!(
        child_key
            .rsplit('/')
            .next()
            .is_some_and(|last| last.eq_ignore_ascii_case(stem)),
        "the chain ends on the nested task file's stem: {child_key}"
    );

    let ends: std::collections::BTreeSet<(AgentId, bool)> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SessionEnd { agent_id, as_child } => Some((*agent_id, *as_child)),
            _ => None,
        })
        .collect();
    assert_eq!(
        ends,
        [(*parent, false), (*child, true)].into(),
        "the root ends as a root, the nested child as a child"
    );

    let pids: std::collections::BTreeSet<i64> = raw
        .iter()
        .map(|v| v["_pid"].as_i64().expect("the extension stamps _pid"))
        .collect();
    assert_eq!(pids.len(), 1, "extensions run in-process: one pid");
}

/// The omp module doc's load-bearing claim, pinned hermetically per recorded
/// round: BOTH transports mint one folded AgentId, and the approval round's
/// Start twins — separated by human latency — count ONE call through a real
/// `Reducer`.
#[test]
pub(super) fn each_bridge_round_coalesces_both_transports_onto_one_id() {
    for scenario in [
        "approve-hermetic",
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
        if scenario != "tool-run-hermetic" {
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
    let prev_id = switched("previousSessionFile");
    let cur_id = switched("sessionFile");
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
