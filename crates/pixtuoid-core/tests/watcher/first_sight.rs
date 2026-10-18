use std::time::{Duration, SystemTime};

use filetime::{FileTime, set_file_mtime};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use pixtuoid_core::AgentId;
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::source::Transport;
use pixtuoid_core::source::claude_code::{cc_derive_label, cc_session_ended, decode_cc_line};
use pixtuoid_core::source::jsonl::JsonlWatcher;

use crate::{cc_watcher, fast_watch, vouch_snapshot, write_lines};

#[tokio::test]
async fn watcher_skips_session_start_for_stale_files_on_startup() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-stale");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();

    let stale = project_dir.join("old.jsonl");
    let line = serde_json::json!({
        "type": "assistant",
        "sessionId": "old",
        "cwd": "/repo",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_old", "name": "Bash",
                  "input": { "command": "ls" } }
            ]
        }
    });
    tokio::fs::write(&stale, format!("{line}\n")).await.unwrap();
    let backdated = FileTime::from_system_time(SystemTime::now() - Duration::from_secs(3600));
    set_file_mtime(&stale, backdated).unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone()).with_initial_window(Duration::from_secs(60));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut events = Vec::new();
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
        events.push(ev);
    }
    assert!(
        events.is_empty(),
        "stale file must not produce events on startup, got {events:?}"
    );
    handle.abort();
}

#[tokio::test]
async fn watcher_registers_stale_file_when_probe_says_live() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-idle-live");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();

    let uuid = "01000000-0000-7000-8000-0000000000aa";
    let stale = project_dir.join(format!("{uuid}.jsonl"));
    let line = serde_json::json!({
        "type": "assistant",
        "sessionId": uuid,
        "cwd": "/repo",
        "message": { "role": "assistant", "content": [] }
    });
    tokio::fs::write(&stale, format!("{line}\n")).await.unwrap();
    let backdated = FileTime::from_system_time(SystemTime::now() - Duration::from_secs(3600));
    set_file_mtime(&stale, backdated).unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone())
        .with_initial_window(Duration::from_secs(60))
        .with_liveness_probe(std::sync::Arc::new(move || vouch_snapshot(&[uuid])));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let expected = AgentId::from_parts("claude-code", uuid);
    let mut start_id = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((_, AgentEvent::SessionStart { agent_id, .. }))) =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
        {
            start_id = Some(agent_id);
            break;
        }
    }
    assert_eq!(
        start_id,
        Some(expected),
        "a probe-live stale transcript must register on startup"
    );
    handle.abort();
}

#[tokio::test]
async fn codex_watcher_registers_stale_rollout_when_probe_says_live() {
    fast_watch();
    use pixtuoid_core::source::codex::decode_codex_line;

    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let day_dir = root.join("2026").join("06").join("10");
    tokio::fs::create_dir_all(&day_dir).await.unwrap();
    let uuid = "019e7762-9ded-7e33-be41-946ecf105bf4";
    let rollout = day_dir.join(format!("rollout-2026-06-10T08-00-00-{uuid}.jsonl"));
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": uuid, "cwd": "/Users/me/dotfiles" }
    });
    tokio::fs::write(&rollout, format!("{meta}\n"))
        .await
        .unwrap();
    let backdated = FileTime::from_system_time(SystemTime::now() - Duration::from_secs(3600));
    set_file_mtime(&rollout, backdated).unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = JsonlWatcher::new(root.clone(), "codex".to_string(), decode_codex_line, |_t| {
        false
    })
    .with_initial_window(Duration::from_secs(60))
    .with_liveness_probe(std::sync::Arc::new(move || vouch_snapshot(&[uuid])));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let expected = AgentId::from_parts("codex", uuid);
    let mut start_id = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((_, AgentEvent::SessionStart { agent_id, .. }))) =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
        {
            start_id = Some(agent_id);
            break;
        }
    }
    assert_eq!(
        start_id,
        Some(expected),
        "a probe-live stale rollout must register on startup, UUID-keyed"
    );
    handle.abort();
}

#[tokio::test]
async fn codex_first_sight_session_start_carries_bare_uuid_session_id() {
    fast_watch();
    use pixtuoid_core::source::codex::decode_codex_line;

    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let day_dir = root.join("2026").join("06").join("10");
    tokio::fs::create_dir_all(&day_dir).await.unwrap();
    let uuid = "019e7762-9ded-7e33-be41-946ecf105bf5";
    let rollout = day_dir.join(format!("rollout-2026-06-10T08-00-00-{uuid}.jsonl"));
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": uuid, "cwd": "/Users/me/dotfiles" }
    });
    tokio::fs::write(&rollout, format!("{meta}\n"))
        .await
        .unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = JsonlWatcher::new(root.clone(), "codex".to_string(), decode_codex_line, |_t| {
        false
    });
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let mut got = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((
            _,
            AgentEvent::SessionStart {
                agent_id,
                session_id,
                ..
            },
        ))) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
        {
            got = Some((agent_id, session_id));
            break;
        }
    }
    let (agent_id, session_id) = got.expect("expected SessionStart from the codex watcher");
    assert_eq!(agent_id, AgentId::from_parts("codex", uuid));
    assert_eq!(
        session_id, uuid,
        "first-sight session_id must be the IdDeriver's bare UUID, not the rollout file stem"
    );
    handle.abort();
}

#[tokio::test]
async fn watcher_emits_session_start_for_recent_files_on_startup() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-fresh");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();

    let fresh = project_dir.join("fresh.jsonl");
    let line = serde_json::json!({
        "type": "assistant",
        "sessionId": "fresh",
        "cwd": "/repo",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_fresh", "name": "Bash",
                  "input": { "command": "ls" } }
            ]
        }
    });
    std::fs::write(&fresh, format!("{line}\n")).unwrap();
    // fsync the parent dir: APFS metadata propagation can otherwise race with
    // the watcher's initial seed walk. Unix-only — Windows can't open a
    // directory as a plain file.
    #[cfg(unix)]
    std::fs::File::open(&project_dir)
        .unwrap()
        .sync_all()
        .unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone()).with_initial_window(Duration::from_secs(3600));
    let fresh_path = fresh.clone();
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    // A no-op append as a fallback notification, in case the initial seed
    // missed the file under I/O contention.
    tokio::time::sleep(Duration::from_millis(500)).await;
    tokio::fs::OpenOptions::new()
        .append(true)
        .open(&fresh_path)
        .await
        .unwrap()
        .sync_all()
        .await
        .unwrap();

    let mut got_start = false;
    let mut got_activity = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some((_, AgentEvent::SessionStart { .. }))) => got_start = true,
            Ok(Some((_, AgentEvent::ActivityStart { .. }))) => got_activity = true,
            _ => {}
        }
        if got_start && got_activity {
            break;
        }
    }
    assert!(got_start, "fresh file should produce SessionStart");
    assert!(got_activity, "fresh file content should be replayed");
    handle.abort();
}

#[tokio::test]
async fn first_sight_extracts_cwd_past_non_json_prefix() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-cwd");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let transcript = project_dir.join("ses-cwd.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // SessionStart fires exactly ONCE per file, with cwd taken from whatever
    // bytes are present at first read — an incremental write leaves a window
    // where the poll sees a partial file and latches cwd="" permanently. Stage
    // in a `.partial` sibling (excluded by the `.jsonl` filter), then rename.
    let sys_line = serde_json::json!({
        "type": "system",
        "subtype": "session_start",
        "sessionId": "ses-cwd",
        "cwd": "/real-repo"
    });
    let content = format!("not-json-prefix\n{sys_line}\n");
    let staging = project_dir.join("ses-cwd.jsonl.partial");
    tokio::fs::write(&staging, content.as_bytes())
        .await
        .unwrap();
    tokio::fs::rename(&staging, &transcript).await.unwrap();

    let mut found_cwd = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((_, AgentEvent::SessionStart { cwd, .. }))) =
            tokio::time::timeout(Duration::from_millis(100), rx.recv()).await
        {
            found_cwd = Some(cwd);
            break;
        }
    }
    assert_eq!(
        found_cwd,
        Some(std::path::PathBuf::from("/real-repo")),
        "the head scan must pass non-JSON lines to find cwd"
    );
    handle.abort();
}

#[tokio::test]
async fn first_sight_cwd_ignores_foreign_source_shapes() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-foreign");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let transcript = project_dir.join("ses-foreign.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Staged + renamed so first sight always reads the full content.
    let codex_shaped = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": "ses-foreign", "cwd": "/foreign/repo" }
    });
    let cc_line = serde_json::json!({
        "type": "system",
        "subtype": "session_start",
        "sessionId": "ses-foreign"
    });
    let content = format!("{codex_shaped}\n{cc_line}\n");
    let staging = project_dir.join("ses-foreign.jsonl.partial");
    tokio::fs::write(&staging, content.as_bytes())
        .await
        .unwrap();
    tokio::fs::rename(&staging, &transcript).await.unwrap();

    let mut found_cwd = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((_, AgentEvent::SessionStart { cwd, .. }))) =
            tokio::time::timeout(Duration::from_millis(100), rx.recv()).await
        {
            found_cwd = Some(cwd);
            break;
        }
    }
    assert_eq!(
        found_cwd,
        Some(std::path::PathBuf::new()),
        "a foreign source's cwd shape must not label a CC session"
    );
    handle.abort();
}

#[tokio::test]
async fn stale_file_emits_session_start_when_written_to() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-revive");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();

    let revived = project_dir.join("revive.jsonl");
    tokio::fs::write(&revived, "{}\n").await.unwrap();
    set_file_mtime(
        &revived,
        FileTime::from_system_time(SystemTime::now() - Duration::from_secs(3600)),
    )
    .unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone()).with_initial_window(Duration::from_secs(60));
    let handle = tokio::spawn(async move { watcher.run(tx).await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    while tokio::time::timeout(Duration::from_millis(20), rx.recv())
        .await
        .is_ok()
    {}

    let line = serde_json::json!({
        "type": "assistant",
        "sessionId": "revive",
        "cwd": "/repo",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_new", "name": "Bash",
                  "input": { "command": "ls" } }
            ]
        }
    });
    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&revived)
        .await
        .unwrap();
    f.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    f.flush().await.unwrap();
    drop(f);

    let mut got_start = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((_, AgentEvent::SessionStart { .. }))) =
            tokio::time::timeout(Duration::from_millis(100), rx.recv()).await
        {
            got_start = true;
            break;
        }
    }
    assert!(got_start, "appending to a stale file should bring it live");
    handle.abort();
}

#[tokio::test]
async fn watcher_skips_recent_file_with_session_end_marker() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-ended");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();

    let ended = project_dir.join("ended.jsonl");
    let content = r#"{"type":"system","subtype":"session_start","sessionId":"ended","cwd":"/repo"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu_1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"system","subtype":"session_end","sessionId":"ended"}
"#;
    tokio::fs::write(&ended, content).await.unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone()).with_initial_window(Duration::from_secs(3600));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut events = Vec::new();
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
        events.push(ev);
    }
    let has_session_start = events
        .iter()
        .any(|(_, ev)| matches!(ev, AgentEvent::SessionStart { .. }));
    assert!(
        !has_session_start,
        "recent file with session_end marker must not produce SessionStart, got {events:?}"
    );
    handle.abort();
}

fn custom_label(_path: &std::path::Path, _source: &str, _cwd: &std::path::Path) -> String {
    "custom-label-ok".to_string()
}

#[tokio::test]
async fn watcher_custom_label_deriver() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-y");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let transcript = project_dir.join("ses-xyz.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = JsonlWatcher::new(
        projects_root.clone(),
        "claude-code".to_string(),
        decode_cc_line,
        cc_session_ended,
    )
    .with_label_deriver(custom_label);
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    let start_line = serde_json::json!({
        "type": "system",
        "subtype": "session_start",
        "sessionId": "ses-xyz",
        "cwd": "/repo"
    });
    f.write_all(format!("{start_line}\n").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    let mut got_custom_rename = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some((_, AgentEvent::Rename { label, .. }))) => {
                if label == "custom-label-ok" {
                    got_custom_rename = true;
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {}
        }
    }
    assert!(
        got_custom_rename,
        "expected Rename event with custom label from label deriver fn"
    );
    handle.abort();
}

#[tokio::test]
async fn codex_rollout_yields_uuid_keyed_session_start() {
    fast_watch();
    use pixtuoid_core::source::codex::decode_codex_line;

    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let uuid = "019e7762-9ded-7e33-be41-946ecf105bf4";
    let transcript = root.join(format!("rollout-2026-05-29T22-36-52-{uuid}.jsonl"));

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = JsonlWatcher::new(root.clone(), "codex".to_string(), decode_codex_line, |_t| {
        false
    });
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": uuid, "cwd": "/Users/me/dotfiles" }
    });
    f.write_all(format!("{meta}\n").as_bytes()).await.unwrap();
    let task_started = serde_json::json!({
        "type": "event_msg",
        "payload": { "type": "task_started", "turn_id": "t" }
    });
    f.write_all(format!("{task_started}\n").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    let expected = AgentId::from_parts("codex", uuid);
    let mut saw_session_start = false;
    // NO `.with_label_deriver` override: the only watcher-level cover that the
    // default LabelDeriver wired in `JsonlWatcher::new` is live.
    let mut default_label = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some((_t, AgentEvent::SessionStart { agent_id, .. }))) => {
                assert_eq!(agent_id, expected, "Codex SessionStart must be UUID-keyed");
                saw_session_start = true;
            }
            Ok(Some((_t, AgentEvent::Rename { label, .. }))) => default_label = Some(label),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {}
        }
        if saw_session_start && default_label.is_some() {
            break;
        }
    }
    assert!(saw_session_start, "expected a SessionStart event");
    assert_eq!(
        default_label.as_deref(),
        Some("cx·dotfiles"),
        "the default LabelDeriver must emit the source-prefixed cwd basename"
    );
    handle.abort();
}

#[tokio::test]
async fn default_id_deriver_stays_path_keyed() {
    // `antigravity`'s registry row carries the `default_id_from_path` deriver,
    // where CC/Codex/copilot/grok/omp rows carry their own session-key fn.
    fast_watch();
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let project_dir = root.join("proj-y");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let transcript = project_dir.join("abc.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = JsonlWatcher::new(
        root.clone(),
        "antigravity".to_string(),
        decode_cc_line,
        cc_session_ended,
    )
    .with_label_deriver(cc_derive_label);
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    let start_line = serde_json::json!({
        "type": "system",
        "subtype": "session_start",
        "sessionId": "abc",
        "cwd": "/repo"
    });
    f.write_all(format!("{start_line}\n").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    // The EXACT id (`from_parts(source, normalize_path_key(path))`) is
    // platform-dependent, so assert only that it is NOT the stem-keyed one; the
    // value itself is pinned at the unit level in `jsonl/tests.rs` + `id.rs`.
    let stem_keyed = AgentId::from_parts("antigravity", "abc");
    let mut ok = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some((_t, AgentEvent::SessionStart { agent_id, .. }))) => {
                assert_ne!(
                    agent_id, stem_keyed,
                    "default deriver must be path-keyed, not stem-keyed"
                );
                ok = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {}
        }
    }
    assert!(ok, "expected a path-keyed SessionStart");
    handle.abort();
}

#[tokio::test]
async fn watcher_skips_oversized_pending_tail() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-big");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let transcript = project_dir.join("big.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = cc_watcher(projects_root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let junk = vec![b'x'; (1 << 20) + 1024];
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(&junk).await.unwrap();
    f.flush().await.unwrap();
    drop(f);

    let mut got_start = false;
    let mut got_rename = None;
    let mut activity_before = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some((_, AgentEvent::SessionStart { cwd, .. }))) => {
                got_start = true;
                assert_eq!(
                    cwd,
                    std::path::PathBuf::from(""),
                    "a no-newline head yields an empty-cwd SessionStart"
                );
            }
            Ok(Some((_, AgentEvent::Rename { label, .. }))) => got_rename = Some(label),
            Ok(Some((_, AgentEvent::ActivityStart { .. }))) => activity_before += 1,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                if got_start && got_rename.is_some() {
                    break;
                }
            }
        }
    }
    assert!(
        got_start,
        "a first-sight oversized transcript must register an agent (#204), not stay invisible"
    );
    assert_eq!(
        got_rename.as_deref(),
        Some("cc·big"),
        "empty-cwd Rename falls back to the project-dir basename"
    );
    assert_eq!(
        activity_before, 0,
        "the oversized backlog must not be replayed (got {activity_before} ActivityStart)"
    );

    let valid = serde_json::json!({
        "type": "assistant",
        "sessionId": "big",
        "cwd": "/repo",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_after_junk", "name": "Bash", "input": { "command": "ls" } }
            ]
        }
    });
    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("\n{valid}\n").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    let mut got_after = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((_, AgentEvent::ActivityStart { tool_use_id, .. }))) =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
            && tool_use_id.as_deref() == Some("tu_after_junk")
        {
            got_after = true;
            break;
        }
    }
    assert!(
        got_after,
        "the post-skip valid line must decode after the oversized tail is skipped"
    );
    handle.abort();
}

#[tokio::test]
async fn watcher_registers_large_transcript_on_first_sight() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("-Users-me-bigrepo");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let transcript = project_dir.join("big-session.jsonl");

    let first = serde_json::json!({
        "type": "system",
        "subtype": "session_start",
        "sessionId": "big-session",
        "cwd": "/Users/me/work/bigrepo"
    });
    let mut contents = format!("{first}\n");
    let backlog_line = serde_json::json!({
        "type": "assistant",
        "sessionId": "big-session",
        "cwd": "/Users/me/work/bigrepo",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_backlog", "name": "Bash", "input": { "command": "ls" } }
            ]
        }
    });
    let backlog_line = format!("{backlog_line}\n");
    while contents.len() <= (1usize << 20) + 4096 {
        contents.push_str(&backlog_line);
    }
    assert!(
        contents.len() > (1usize << 20),
        "test transcript must exceed MAX_PENDING_BYTES"
    );

    // Write the whole file BEFORE the watcher first sees it, so the body is one
    // oversized first-sight pending tail.
    tokio::fs::write(&transcript, contents.as_bytes())
        .await
        .unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(256);
    let watcher = cc_watcher(projects_root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let mut start_id = None;
    let mut start_cwd = None;
    let mut label = None;
    let mut activity_count = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some((_, AgentEvent::SessionStart { agent_id, cwd, .. }))) => {
                start_id = Some(agent_id);
                start_cwd = Some(cwd);
            }
            Ok(Some((_, AgentEvent::Rename { label: l, .. }))) => {
                label = Some(l);
            }
            Ok(Some((_, AgentEvent::ActivityStart { .. }))) => {
                activity_count += 1;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {}
        }
        if start_id.is_some() && label.is_some() {
            tokio::time::sleep(Duration::from_millis(200)).await;
            while let Ok(Some((_, ev))) =
                tokio::time::timeout(Duration::from_millis(20), rx.recv()).await
            {
                if matches!(ev, AgentEvent::ActivityStart { .. }) {
                    activity_count += 1;
                }
            }
            break;
        }
    }

    let _start_id = start_id.expect("expected SessionStart for the large first-sight transcript");
    let start_cwd = start_cwd.expect("SessionStart should carry the head-derived cwd");
    let label = label.expect("expected a Rename label for the large transcript");
    assert_eq!(
        start_cwd,
        std::path::PathBuf::from("/Users/me/work/bigrepo"),
        "cwd must come from the bounded head read of the first line"
    );
    assert_eq!(
        label, "cc·bigrepo",
        "label must derive from the head-read cwd basename"
    );
    assert!(
        activity_count < 5,
        "the giant backlog must not be replayed wholesale (got {activity_count} ActivityStart)"
    );
    handle.abort();
}

#[tokio::test]
async fn watcher_derives_parent_id_for_subagent_path() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let subagent_dir = projects_root.join("proj").join("parent").join("subagents");
    tokio::fs::create_dir_all(&subagent_dir).await.unwrap();
    let transcript = subagent_dir.join("agent-1.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let line = serde_json::json!({
        "type": "assistant",
        "sessionId": "agent-1",
        "cwd": "/repo",
        "attributionAgent": "feature-dev:code-explorer",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_1", "name": "Read", "input": { "file_path": "/x" } }
            ]
        }
    });
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    f.flush().await.unwrap();
    drop(f);

    let expected = AgentId::from_parts("claude-code", "parent");

    let mut found_parent = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((
            _,
            AgentEvent::SessionStart {
                parent_id: Some(pid),
                ..
            },
        ))) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
        {
            found_parent = Some(pid);
            break;
        }
    }
    let found = found_parent.expect("expected a SessionStart carrying parent_id");
    assert_eq!(
        found, expected,
        "parent_id must key on the <parent-uuid> dir component; got {found:?}"
    );
    handle.abort();
}

#[tokio::test]
async fn watcher_links_subagent_across_project_dirs() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();

    let parent_uuid = "abc123def456";
    let project_a = projects_root.join("-Users-me-PROJECT-A");
    tokio::fs::create_dir_all(&project_a).await.unwrap();
    let parent_transcript = project_a.join(format!("{parent_uuid}.jsonl"));
    let subagent_dir = projects_root
        .join("-Users-me-PROJECT-B")
        .join(parent_uuid)
        .join("subagents");
    tokio::fs::create_dir_all(&subagent_dir).await.unwrap();
    let sub_transcript = subagent_dir.join("agent-1.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let parent_line = serde_json::json!({
        "type": "system",
        "subtype": "session_start",
        "sessionId": parent_uuid,
        "cwd": "/Users/me/PROJECT-A"
    });
    let mut pf = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&parent_transcript)
        .await
        .unwrap();
    pf.write_all(format!("{parent_line}\n").as_bytes())
        .await
        .unwrap();
    pf.flush().await.unwrap();
    drop(pf);

    let sub_line = serde_json::json!({
        "type": "assistant",
        "sessionId": "agent-1",
        "cwd": "/Users/me/PROJECT-B",
        "attributionAgent": "feature-dev:code-explorer",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_1", "name": "Read", "input": { "file_path": "/x" } }
            ]
        }
    });
    let mut sf = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&sub_transcript)
        .await
        .unwrap();
    sf.write_all(format!("{sub_line}\n").as_bytes())
        .await
        .unwrap();
    sf.flush().await.unwrap();
    drop(sf);

    let mut parent_agent_id: Option<AgentId> = None;
    let mut sub_parent_id: Option<AgentId> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some((
                _,
                AgentEvent::SessionStart {
                    agent_id,
                    parent_id,
                    ..
                },
            ))) => match parent_id {
                Some(pid) => sub_parent_id = Some(pid),
                None => parent_agent_id = Some(agent_id),
            },
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {}
        }
        if parent_agent_id.is_some() && sub_parent_id.is_some() {
            break;
        }
    }
    let parent_agent_id = parent_agent_id.expect("expected the parent's SessionStart");
    let sub_parent_id = sub_parent_id.expect("expected the subagent's SessionStart with parent_id");
    assert_eq!(
        sub_parent_id, parent_agent_id,
        "subagent parent_id must equal the parent's agent_id across a cwd-split (different project dirs)"
    );
    handle.abort();
}

#[tokio::test]
async fn the_registry_row_filters_the_workflow_journal_and_the_builder_overrides_it() {
    fn admit_every_jsonl(_p: &std::path::Path) -> bool {
        true
    }

    async fn first_sight_starts(root: std::path::PathBuf, admit_all: bool, want: usize) -> usize {
        let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
        let mut watcher = JsonlWatcher::new(
            root,
            "claude-code".to_string(),
            decode_cc_line,
            cc_session_ended,
        );
        if admit_all {
            watcher = watcher.with_path_filter(admit_every_jsonl);
        }
        let handle = tokio::spawn(async move { watcher.run(tx).await });

        let mut starts = 0;
        let mut deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some((_, AgentEvent::SessionStart { .. }))) =
                tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
            {
                starts += 1;
                // both files exist before the first scan: a start past `want`
                // lands within a few more scans, not seconds later
                if starts == want {
                    deadline = tokio::time::Instant::now() + 20 * super::POLL;
                }
            }
        }
        handle.abort();
        starts
    }

    fast_watch();
    let dir = TempDir::new().unwrap();
    let wf = dir
        .path()
        .join("proj")
        .join("01000000-0000-7000-8000-0000000000cc")
        .join("subagents")
        .join("workflows")
        .join("wf_1");
    tokio::fs::create_dir_all(&wf).await.unwrap();
    write_lines(
        &wf.join("journal.jsonl"),
        &[serde_json::json!({"type": "started", "name": "wf"})],
    )
    .await;
    write_lines(
        &wf.join("agent-xyz.jsonl"),
        &[serde_json::json!({
            "type": "assistant",
            "cwd": "/repo",
            "message": {"content": [{"type": "text", "text": "hi"}]}
        })],
    )
    .await;

    assert_eq!(
        first_sight_starts(dir.path().to_path_buf(), false, 1).await,
        1,
        "the claude-code row's path_filter must exclude journal.jsonl — only the \
         real subagent transcript first-sights"
    );
    assert_eq!(
        first_sight_starts(dir.path().to_path_buf(), true, 2).await,
        2,
        "an explicit with_path_filter must OVERRIDE the row (both files first-sight)"
    );
}
