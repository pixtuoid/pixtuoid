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

use crate::{fast_watch, write_lines};

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
