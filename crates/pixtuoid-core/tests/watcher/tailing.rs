use std::time::Duration;

use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::source::Transport;

use crate::{
    TRAIL, collect_for, collect_until, copilot_start_line, copilot_tool_line, spawn_copilot,
    starts_call,
};

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

    let mut events = collect_until(&mut rx, "a ToolUseStart from the JSONL watcher", |e| {
        matches!(e, AgentEvent::ToolUseStart { .. })
    })
    .await;
    let activity = events
        .pop()
        .map(|(t, e)| (t, e.agent_id()))
        .expect("collect_until ends on its event");
    let starts: Vec<_> = events
        .iter()
        .filter(|(_, e)| matches!(e, AgentEvent::SessionStart { .. }))
        .map(|(t, e)| (*t, e.agent_id()))
        .collect();
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

    let mut seen = collect_until(&mut rx, "tu_1 from the complete line", starts_call("tu_1")).await;
    seen.extend(collect_for(&mut rx, TRAIL, "past the complete line").await);
    assert!(
        !seen.iter().any(|(_, e)| starts_call("tu_2")(e)),
        "tu_2 came from a partial line and must not be emitted yet: {seen:?}"
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

    collect_until(
        &mut rx,
        "tu_2 should appear after partial line is completed",
        starts_call("tu_2"),
    )
    .await;

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

    collect_until(&mut rx, "the first long line", starts_call("tu_long")).await;

    let fresh = tool_line("tu_fresh");
    tokio::fs::write(&transcript, format!("{fresh}\n"))
        .await
        .unwrap();

    collect_until(
        &mut rx,
        "after truncation the cursor must reset so the fresh line decodes",
        starts_call("tu_fresh"),
    )
    .await;
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

    collect_until(
        &mut rx,
        "a non-UTF8 line must be skipped, not block the following valid line",
        starts_call("tu_valid"),
    )
    .await;
    handle.abort();
}
