use std::time::Duration;

use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::source::Transport;

use crate::{copilot_start_line, copilot_tool_line, spawn_copilot};

#[tokio::test]
async fn watcher_emits_session_start_then_activity_for_tool_use() {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("ses-abc");
    tokio::fs::create_dir_all(&session_dir).await.unwrap();
    let transcript = session_dir.join("events.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let handle = spawn_copilot(dir.path().to_path_buf(), tx);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    let lines = [
        copilot_start_line("ses-abc", "/repo"),
        copilot_tool_line("tu_1", "bash"),
    ];
    for line in lines {
        f.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    }
    f.flush().await.unwrap();
    drop(f);

    let mut starts = Vec::new();
    let mut activity = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some((t, AgentEvent::SessionStart { agent_id, .. }))) => {
                starts.push((t, agent_id));
            }
            Ok(Some((t, AgentEvent::ActivityStart { agent_id, .. }))) => {
                activity = Some((t, agent_id));
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {}
        }
    }
    let activity = activity.expect("expected ActivityStart from JSONL watcher");
    assert!(
        !starts.is_empty(),
        "expected SessionStart from JSONL watcher"
    );
    // The SessionStart key (id_derive) and the per-line decode key
    // (transcript_path_str) are computed at two different walk_jsonl sites —
    // they diverged on the windows runner.
    assert!(
        starts
            .iter()
            .all(|start| *start == (Transport::Jsonl, activity.1)),
        "SessionStart and per-line events must share one AgentId: {starts:?} vs {activity:?}"
    );
    assert_eq!(activity.0, Transport::Jsonl);
    handle.abort();
}

#[tokio::test]
async fn watcher_does_not_consume_partial_trailing_line() {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("ses-abc");
    tokio::fs::create_dir_all(&session_dir).await.unwrap();
    let transcript = session_dir.join("events.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let handle = spawn_copilot(dir.path().to_path_buf(), tx);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let complete = copilot_tool_line("tu_1", "bash");
    let partial_head = r#"{"type":"tool.execution_start","data":{"toolCallId":"tu_2","toolName":"view","arguments":{"path":"/etc/host"#;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("{complete}\n{partial_head}").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut seen_tool_use_ids: Vec<String> = Vec::new();
    while let Ok(Some((_t, ev))) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await
    {
        if let AgentEvent::ActivityStart {
            tool_use_id: Some(id),
            ..
        } = ev
        {
            seen_tool_use_ids.push(id);
        }
    }
    assert!(
        seen_tool_use_ids.contains(&"tu_1".to_string()),
        "expected tu_1 from complete line, got {seen_tool_use_ids:?}"
    );
    assert!(
        !seen_tool_use_ids.contains(&"tu_2".to_string()),
        "tu_2 came from a partial line and must not be emitted yet"
    );

    let partial_tail = "s\"}}}\n";
    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(partial_tail.as_bytes()).await.unwrap();
    f.flush().await.unwrap();
    drop(f);

    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut got_tu_2 = false;
    while let Ok(Some((_t, ev))) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await
    {
        if let AgentEvent::ActivityStart { tool_use_id, .. } = ev
            && tool_use_id.as_deref() == Some("tu_2")
        {
            got_tu_2 = true;
        }
    }
    assert!(
        got_tu_2,
        "tu_2 should appear after partial line is completed"
    );

    handle.abort();
}

#[tokio::test]
async fn watcher_resets_cursor_on_truncation_below_cursor() {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("trunc");
    tokio::fs::create_dir_all(&session_dir).await.unwrap();
    let transcript = session_dir.join("events.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let handle = spawn_copilot(dir.path().to_path_buf(), tx);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let tool_line = |id: &str| copilot_tool_line(id, "bash").to_string();

    // Long first line so the cursor advances well past a later short one.
    let long = tool_line("tu_long") + &" ".repeat(400);
    tokio::fs::write(&transcript, format!("{long}\n"))
        .await
        .unwrap();

    let mut saw_long = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((_, AgentEvent::ActivityStart { tool_use_id, .. }))) =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
            && tool_use_id.as_deref() == Some("tu_long")
        {
            saw_long = true;
            break;
        }
    }
    assert!(saw_long, "expected the first long line to decode");

    let fresh = tool_line("tu_fresh");
    tokio::fs::write(&transcript, format!("{fresh}\n"))
        .await
        .unwrap();

    let mut saw_fresh = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((_, AgentEvent::ActivityStart { tool_use_id, .. }))) =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
            && tool_use_id.as_deref() == Some("tu_fresh")
        {
            saw_fresh = true;
            break;
        }
    }
    assert!(
        saw_fresh,
        "after truncation the cursor must reset so the fresh line decodes"
    );
    handle.abort();
}

#[tokio::test]
async fn watcher_skips_non_utf8_line_and_keeps_going() {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("nonutf8");
    tokio::fs::create_dir_all(&session_dir).await.unwrap();
    let transcript = session_dir.join("events.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let handle = spawn_copilot(dir.path().to_path_buf(), tx);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let valid = copilot_tool_line("tu_valid", "bash");
    // Invalid-UTF8 bytes can't go through serde_json — write them raw.
    let mut bytes: Vec<u8> = vec![0xff, 0xfe, b'\n'];
    bytes.extend_from_slice(format!("{valid}\n").as_bytes());
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(&bytes).await.unwrap();
    f.flush().await.unwrap();
    drop(f);

    let mut got_valid = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((_, AgentEvent::ActivityStart { tool_use_id, .. }))) =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
            && tool_use_id.as_deref() == Some("tu_valid")
        {
            got_valid = true;
            break;
        }
    }
    assert!(
        got_valid,
        "a non-UTF8 line must be skipped, not block the following valid line"
    );
    handle.abort();
}
