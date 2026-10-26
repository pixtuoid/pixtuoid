//! Tests that need the NATIVE watch backend. Their own binary, so they get their
//! own process: `watcher`'s `fast_watch()` forces polling process-wide, and a
//! polled root tolerates the failure these assert, so sharing a process with it
//! (as `cargo test` does) made them wait forever.

use std::time::Duration;

use pixtuoid_core::AgentId;
use pixtuoid_core::source::codex::decode_codex_line;
use pixtuoid_core::source::jsonl::JsonlWatcher;
use pixtuoid_core::source::{AgentEvent, Transport};
use tempfile::TempDir;
use tokio::sync::mpsc;

/// Past any native watch setup, short of hanging CI.
const RUN_DEADLINE: Duration = Duration::from_secs(30);

#[cfg(unix)]
#[tokio::test]
async fn omp_source_run_reports_total_watch_failure_instead_of_swallowing_it() {
    use pixtuoid_core::source::Source;
    use pixtuoid_core::source::omp::OmpSource;
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let sealed = dir.path().join("sealed");
    tokio::fs::create_dir(&sealed).await.unwrap();
    let mut perms = std::fs::metadata(&sealed).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&sealed, perms.clone()).unwrap();

    let (tx, _rx) = mpsc::channel::<(Transport, AgentEvent)>(8);
    let result = tokio::time::timeout(
        RUN_DEADLINE,
        Box::new(OmpSource::single_root(sealed.join("sessions"))).run(tx),
    )
    .await;
    perms.set_mode(0o755);
    std::fs::set_permissions(&sealed, perms).unwrap();
    assert!(
        matches!(result, Ok(Err(_))),
        "every watcher failing must propagate to the deaths surface (#157)"
    );
}

/// inotify and Windows drop a watch whose directory is deleted, so a root the
/// CLI deletes and recreates must be watched again — by the next event, not
/// the poll.
#[tokio::test]
async fn a_recreated_root_is_rewatched() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("sessions");
    tokio::fs::create_dir(&root).await.unwrap();
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = JsonlWatcher::new(root.clone(), "codex".to_string(), decode_codex_line, |_| {
        false
    })
    .with_initial_window(Duration::from_secs(60))
    .with_poll_interval(Duration::from_secs(3600));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let first = "aa000000-0000-7000-8000-0000000000a1";
    write_session(&root, first).await;
    assert!(
        session_started(&mut rx, first, RUN_DEADLINE).await,
        "the root present at boot was not watched"
    );

    tokio::fs::remove_dir_all(&root).await.unwrap();
    tokio::fs::create_dir(&root).await.unwrap();
    let second = "bb000000-0000-7000-8000-0000000000b2";
    write_session(&root, second).await;
    assert!(
        session_started(&mut rx, second, Duration::from_secs(5)).await,
        "a session under the recreated root was not seen"
    );
    assert!(!handle.is_finished(), "the watcher stopped: {handle:?}");
    handle.abort();
}

async fn write_session(root: &std::path::Path, uuid: &str) {
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": uuid, "cwd": "/Users/me/proj" }
    });
    tokio::fs::write(root.join(format!("{uuid}.jsonl")), format!("{meta}\n"))
        .await
        .unwrap();
}

async fn session_started(
    rx: &mut mpsc::Receiver<(Transport, AgentEvent)>,
    uuid: &str,
    within: Duration,
) -> bool {
    let want = AgentId::from_parts("codex", uuid);
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((_, AgentEvent::SessionStart { agent_id, .. }))) =
            tokio::time::timeout(Duration::from_millis(100), rx.recv()).await
            && agent_id == want
        {
            return true;
        }
    }
    false
}
