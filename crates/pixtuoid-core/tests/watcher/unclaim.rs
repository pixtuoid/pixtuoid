use std::time::{Duration, SystemTime};

use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use pixtuoid_core::AgentId;
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::source::Source;
use pixtuoid_core::source::Transport;
use pixtuoid_core::source::codex::CodexSource;
use pixtuoid_core::source::jsonl::ChildEndUnclaims;

use crate::today::spawn_source;
use crate::{Fold, append, fast_watch, recorded, split_before_last, stage, write_lines};

#[tokio::test]
async fn in_flight_multi_turn_codex_child_revives_and_relinks_via_unclaim() {
    use pixtuoid_core::source::decoder::decode_hook_payload;
    use pixtuoid_core::state::reducer::{CHILD_END_GATE_TTL, EXIT_GRACE_WINDOW};
    use pixtuoid_core::{Reducer, SceneState};

    fast_watch();
    let dir = TempDir::new().unwrap();
    let sessions_root = dir.path().to_path_buf();
    let child_uuid = "0e000000-0000-7000-8000-0000000000e1";
    let rollout = sessions_root.join(format!("rollout-2026-06-11T10-00-00-{child_uuid}.jsonl"));
    let parent = AgentId::from_parts("codex", "parent-sess");
    let child = AgentId::from_parts("codex", child_uuid);

    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let apply_hook = |r: &mut Reducer, scene: &mut SceneState, payload: serde_json::Value| {
        for ev in decode_hook_payload(payload).expect("hook payload decodes") {
            r.apply(scene, ev, SystemTime::now(), Transport::Hook);
        }
    };

    apply_hook(
        &mut r,
        &mut scene,
        serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "parent-sess",
            "_pixtuoid_source": "codex",
            "cwd": "/repo",
        }),
    );
    apply_hook(
        &mut r,
        &mut scene,
        serde_json::json!({
            "hook_event_name": "SubagentStart",
            "session_id": "parent-sess",
            "agent_id": child_uuid,
            "cwd": "/repo",
            "_pixtuoid_source": "codex",
        }),
    );
    assert_eq!(
        scene.agents.get(&child).map(|s| s.parent_id),
        Some(Some(parent)),
        "first life: the child registers with the parent link"
    );

    let unclaims = ChildEndUnclaims::new();
    let src = CodexSource {
        sessions_root,
        child_end_unclaims: Some(unclaims.clone()),
    };
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher_task = tokio::spawn(async move { Box::new(src).run(tx).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": child_uuid, "cwd": "/repo" }
    });
    let work = serde_json::json!({
        "type": "event_msg",
        "payload": { "type": "task_started", "turn_id": "t1" }
    });
    write_lines(&rollout, &[meta, work.clone()]).await;
    let mut claimed = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline && !claimed {
        if let Ok(Some((transport, ev))) =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
        {
            claimed = matches!(
                &ev,
                AgentEvent::SessionStart { agent_id, .. } if *agent_id == child
            );
            r.apply(&mut scene, ev, SystemTime::now(), transport);
        }
    }
    assert!(claimed, "the watcher must claim the child's flat rollout");

    apply_hook(
        &mut r,
        &mut scene,
        serde_json::json!({
            "hook_event_name": "SubagentStop",
            "session_id": "parent-sess",
            "agent_id": child_uuid,
            "_pixtuoid_source": "codex",
        }),
    );
    unclaims.push(child);
    r.tick(
        &mut scene,
        SystemTime::now() + EXIT_GRACE_WINDOW + Duration::from_secs(1),
    );
    assert!(
        !scene.agents.contains_key(&child),
        "the child's first life is GC'd after the SubagentStop"
    );

    let mut relinked = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline && !relinked {
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .await
            .unwrap();
        f.write_all(format!("{work}\n").as_bytes()).await.unwrap();
        f.flush().await.unwrap();
        drop(f);
        let wait_until = tokio::time::Instant::now() + Duration::from_millis(300);
        while tokio::time::Instant::now() < wait_until {
            if let Ok(Some((transport, ev))) =
                tokio::time::timeout(Duration::from_millis(100), rx.recv()).await
            {
                // Inside the end gate a transcript start of the child is its
                // echo (#244).
                let turn = SystemTime::now() + CHILD_END_GATE_TTL;
                r.apply(&mut scene, ev, turn, transport);
                relinked = scene.agents.get(&child).map(|s| s.parent_id) == Some(Some(parent));
                if relinked {
                    break;
                }
            }
        }
    }
    assert!(
        scene.agents.contains_key(&child),
        "turn N+1 must re-register the child through the released claim"
    );
    assert_eq!(
        scene.agents.get(&child).map(|s| s.parent_id),
        Some(Some(parent)),
        "the revived child must re-link to the remembered parent (the #249 \
         ledger), not register as an orphan"
    );
    watcher_task.abort();
}

/// C37: the reducer forgets a claimed session on its own (the stale sweep, a
/// disconnect's reconcile, a desk it refused), and the session's next read that
/// shows life registers it again, renaming nothing yet under the label its
/// reads last gave it: omp's is its transcript's title.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_the_reducer_forgot_registers_again_on_its_next_live_read() {
    use std::collections::HashSet;

    use pixtuoid_core::SceneState;
    use pixtuoid_core::harness::Drive;
    use pixtuoid_core::state::reducer::{EXIT_GRACE_WINDOW, Reducer, STALE_BLOCKED_TIMEOUT};

    const DESKS: usize = 8;
    /// What forgets the session, and the desks first sight meets.
    type Forget = (&'static str, usize, fn(&mut Fold));
    let forgets: [Forget; 3] = [
        ("the stale sweep", DESKS, |fold| {
            let swept = fold.at + STALE_BLOCKED_TIMEOUT * 2;
            fold.reducer.tick(&mut fold.scene, swept);
            fold.at = swept + EXIT_GRACE_WINDOW * 2;
            fold.reducer.tick(&mut fold.scene, fold.at);
        }),
        ("a disconnect's reconcile", DESKS, |fold| {
            fold.reducer
                .reconcile_connected(&mut fold.scene, &HashSet::new(), fold.at);
            fold.at += EXIT_GRACE_WINDOW * 2;
            fold.reducer.tick(&mut fold.scene, fold.at);
        }),
        ("a desk refusal", 0, |fold| {
            fold.scene.floor_capacities = SceneState::uniform(DESKS).floor_capacities;
        }),
    ];
    let is_call = |e: &AgentEvent| matches!(e, AgentEvent::ToolUseStart { .. });
    for (source, scenario) in [("copilot", "tool-run-hermetic"), ("omp", "ask-hermetic")] {
        let (transcript, dir) = recorded(source, scenario);
        let (head, tail) = split_before_last(source, &transcript, is_call);
        let drive = Drive::transcript_at(source, &transcript).expect("a transcript source");
        let renames = |line: &str| {
            (drive.lines([line]).events.iter()).any(|e| matches!(e, AgentEvent::Rename { .. }))
        };
        // Cut where the transcript names its session again, so only the
        // re-registration can name the slot it brings back.
        let tail: String = (tail.lines().take_while(|line| !renames(line)))
            .map(|line| format!("{line}\n"))
            .collect();
        let calls = (drive.lines(tail.lines()).events.iter())
            .filter(|e| is_call(e))
            .count();
        for (forget, desks, forgotten) in forgets {
            let case = format!("{source}, {forget}");
            let (root, path) = stage(&transcript, &dir, &head);
            let (tx, rx) = mpsc::channel(64);
            let handle = spawn_source(source, root.path().to_path_buf(), tx);
            let mut fold = Fold::new(rx, Reducer::new(), SystemTime::now());
            fold.scene = SceneState::uniform(desks);
            let first_sight = fold
                .until(&case, |e| matches!(e, AgentEvent::SessionStart { .. }))
                .await;
            let id = first_sight
                .iter()
                .find_map(|e| match e {
                    AgentEvent::SessionStart { agent_id, .. } => Some(*agent_id),
                    _ => None,
                })
                .expect("first sight registers it");
            let named = |events: &[AgentEvent]| {
                events.iter().rev().find_map(|e| match e {
                    AgentEvent::Rename { agent_id, label } if *agent_id == id => {
                        Some(label.clone())
                    }
                    _ => None,
                })
            };
            let label = named(&first_sight).expect("first sight names it");
            forgotten(&mut fold);
            assert!(fold.scene.agents.is_empty(), "{case}: not forgotten");

            append(&path, &tail);
            let again = fold.until(&case, is_call).await;
            handle.abort();
            let slot = fold.scene.agents.get(&id);
            assert_eq!(
                slot.map(|s| (s.tool_call_count as usize, s.label.text())),
                Some((calls, label.as_str().into())),
                "{case}: {again:?}"
            );
            assert_eq!(named(&again), None, "{case}: a re-registration renames");
        }
    }
}
