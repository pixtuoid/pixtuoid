//! Tests that need the NATIVE watch backend. Their own binary, so they get their
//! own process: `watcher`'s `fast_watch()` forces polling process-wide, and a
//! polled root tolerates the failure these assert, so sharing a process with it
//! (as `cargo test` does) made them wait forever.

use std::time::Duration;

use pixtuoid_core::AgentId;
use pixtuoid_core::source::jsonl::JsonlWatcher;
use pixtuoid_core::source::{AgentEvent, Transport};
use tempfile::TempDir;
use tokio::sync::mpsc;

#[path = "watcher/wait.rs"]
mod wait;
use wait::{collect_for, collect_until};

/// Past any native watch setup, short of hanging CI: every wait's hang guard,
/// and the held-open tests' verdict, so it stays under the sources' own poll.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long a check listens past its positive event: the same read's events
/// are queued behind it.
const TRAIL: Duration = Duration::from_millis(300);

/// Set far past [`DEADLINE`], so a wait that ends proves the poll did not do
/// the attaching.
const POLL: Duration = Duration::from_secs(3600);

/// The first-sight recency window: a transcript a test just wrote is inside it.
const INITIAL_WINDOW: Duration = Duration::from_secs(60);

fn codex_watcher(root: std::path::PathBuf) -> JsonlWatcher {
    JsonlWatcher::new(root, "codex".to_string())
        .expect("a transcript source")
        .with_initial_window(INITIAL_WINDOW)
        .with_poll_interval(POLL)
}

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
        DEADLINE,
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
    let watcher = codex_watcher(root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let first = "aa000000-0000-7000-8000-0000000000a1";
    write_session(&root, first).await;
    expect_session_start(&mut rx, first, "the root present at boot").await;

    tokio::fs::remove_dir_all(&root).await.unwrap();
    tokio::fs::create_dir(&root).await.unwrap();
    let second = "bb000000-0000-7000-8000-0000000000b2";
    write_session(&root, second).await;
    expect_session_start(&mut rx, second, "a session under the recreated root").await;
    assert!(!handle.is_finished(), "the watcher stopped: {handle:?}");
    handle.abort();
}

/// An empty anchor's own removal must wake a reconcile: on Linux an open
/// handle on it would hold back inotify's `DELETE_SELF` until it closed
/// (`fs/dcache.c`'s `d_delete`), leaving the recreated root to the poll.
/// Windows reports nothing for a watched dir's own removal (notify's
/// `windows.rs` unwatches it silently), so there it does wait for the poll.
#[cfg(unix)]
#[tokio::test]
async fn a_root_recreated_under_a_removed_empty_anchor_is_watched() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join(".copilot");
    tokio::fs::create_dir(&home).await.unwrap();
    let root = home.join("session-state");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = codex_watcher(root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    // Each step lands after the reconcile the previous one woke.
    let settle = Duration::from_millis(300);
    tokio::time::sleep(settle).await;
    tokio::fs::remove_dir(&home).await.unwrap();
    tokio::time::sleep(settle).await;
    tokio::fs::create_dir_all(&root).await.unwrap();
    let uuid = "cc000000-0000-7000-8000-0000000000c3";
    write_session(&root, uuid).await;
    expect_session_start(&mut rx, uuid, "a session under the recreated root").await;
    assert!(!handle.is_finished(), "the watcher stopped: {handle:?}");
    handle.abort();
}

/// The anchor watches the root's parent, whose own files (CC's
/// `history.jsonl` beside its projects root) are no transcripts — on Linux
/// and Windows one watcher hosts both watches and routes by path.
#[tokio::test]
async fn a_file_beside_the_root_is_never_decoded() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("projects");
    tokio::fs::create_dir(&root).await.unwrap();
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = codex_watcher(root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(300)).await;
    let beside = "dd000000-0000-7000-8000-0000000000d4";
    write_session(dir.path(), beside).await;
    let inside = "ee000000-0000-7000-8000-0000000000e5";
    write_session(&root, inside).await;
    let inside_id = AgentId::from_parts("codex", inside);
    let mut events =
        collect_until(&mut rx, "the session inside", |e| is_start(e, &inside_id)).await;
    events.extend(collect_for(&mut rx, TRAIL, "past the session inside").await);
    let beside_id = AgentId::from_parts("codex", beside);
    assert!(
        !events.iter().any(|(_, e)| is_start(e, &beside_id)),
        "a file beside the root was decoded as a session"
    );
    handle.abort();
}

/// Restores the process cwd when dropped.
struct CwdGuard(std::path::PathBuf);

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

/// A relative root (a relative `CLAUDE_CONFIG_DIR`) is spelled once: notify's
/// events come back absolute, so a scan under the relative spelling keys one
/// transcript under two cursors and replays it from the top.
#[tokio::test]
async fn a_relative_root_keys_each_transcript_once() {
    let dir = TempDir::new().unwrap();
    // Canonical, as FSEvents reports it: macOS's temp dir sits under a symlink.
    let base = dunce::canonicalize(dir.path()).unwrap();
    let root = base.join("sessions");
    tokio::fs::create_dir(&root).await.unwrap();
    let uuid = "ff000000-0000-7000-8000-0000000000f6";
    write_session(&root, uuid).await;
    // Declared after `dir`, so the cwd is restored before the dir is removed.
    let _cwd = CwdGuard(std::env::current_dir().unwrap());
    std::env::set_current_dir(&base).unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = codex_watcher(std::path::PathBuf::from("sessions"));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let id = AgentId::from_parts("codex", uuid);
    let canary = "ff000000-0000-7000-8000-0000000000f7";
    let canary_id = AgentId::from_parts("codex", canary);
    // The boot scan sees the first transcript; only a live watch sees the
    // canary, written after it, so the append below can't land unwatched.
    let mut events =
        collect_until(&mut rx, "the boot scan's transcript", |e| is_start(e, &id)).await;
    write_session(&root, canary).await;
    events
        .extend(collect_until(&mut rx, "the watch going live", |e| is_start(e, &canary_id)).await);

    let work = serde_json::json!({
        "type": "response_item",
        "payload": { "type": "function_call", "name": "exec_command", "call_id": "c1" }
    });
    // Closed at once: on macOS the write's FSEvents event came only at close.
    std::io::Write::write_all(
        &mut std::fs::OpenOptions::new()
            .append(true)
            .open(root.join(format!("{uuid}.jsonl")))
            .unwrap(),
        format!("{work}\n").as_bytes(),
    )
    .unwrap();
    let is_activity =
        |e: &AgentEvent| matches!(e, AgentEvent::ToolUseStart { agent_id, .. } if *agent_id == id);
    events.extend(collect_until(&mut rx, "the append's activity", is_activity).await);
    // A second cursor's replay would land beside the activity.
    events.extend(collect_for(&mut rx, TRAIL, "past the activity").await);
    let events: Vec<AgentEvent> = events.into_iter().map(|(_, e)| e).collect();
    // A first sight, unlike a re-registration, names its session.
    let first_sights = (events.iter())
        .filter(|e| matches!(e, AgentEvent::Rename { agent_id, .. } if *agent_id == id))
        .count();
    let activities = events.iter().filter(|e| is_activity(e)).count();
    assert_eq!(
        (first_sights, activities),
        (1, 1),
        "one transcript must decode under one cursor: {events:?}"
    );
    handle.abort();
}

/// codex and omp hold their transcript open for the whole session, and macOS's
/// FSEvents reports a file's writes only at its close (xnu
/// `bsd/vfs/vfs_vnops.c:887-893` at `f6217f89`). Each source's own poll is
/// `DEFAULT_POLL_INTERVAL`, past [`DEADLINE`].
///
/// Not on Windows: NTFS reports a held-open file's writes "when the last
/// handle to a file object is closed"
/// (<https://devblogs.microsoft.com/oldnewthing/20111226-00/?p=8813>), and
/// the open-fd probe the tail's release rides is a no-op there.
#[cfg(unix)]
mod held_open {
    use std::io::Write;
    use std::path::Path;

    use pixtuoid_core::source::Source;
    use pixtuoid_core::source::codex::CodexSource;
    use pixtuoid_core::source::omp::OmpSource;

    use super::*;

    /// What a source whose writer holds its transcript open writes, and where.
    struct Wire {
        source: &'static str,
        id: &'static str,
        /// The transcript, relative to the source's root.
        path: &'static str,
        head: serde_json::Value,
        activity: serde_json::Value,
    }

    impl Wire {
        /// The same session under another id, for a canary beside it.
        fn renamed(&self, from: &str, to: &str) -> (String, String, String) {
            (
                self.id.replace(from, to),
                self.path.replace(from, to),
                self.head.to_string().replace(from, to),
            )
        }
    }

    const CODEX_ID: &str = "01a10000-0000-7000-8000-0000000000a1";

    fn codex() -> Wire {
        Wire {
            source: "codex",
            id: CODEX_ID,
            path: "2026/10/26/rollout-2026-10-26T00-00-00-01a10000-0000-7000-8000-0000000000a1.jsonl",
            head: serde_json::json!({
                "type": "session_meta",
                "payload": { "id": CODEX_ID, "cwd": "/Users/me/proj" }
            }),
            activity: serde_json::json!({
                "type": "response_item",
                "payload": { "type": "function_call", "name": "exec_command", "call_id": "c1" }
            }),
        }
    }

    fn omp() -> Wire {
        Wire {
            source: "omp",
            id: "2026-10-26T00-00-00-000Z_01a10000-0000-7000-8000-0000000000b2",
            path: "--Users-me-proj--/2026-10-26T00-00-00-000Z_01a10000-0000-7000-8000-0000000000b2.jsonl",
            head: serde_json::json!({
                "type": "session", "version": 3, "id": "01a10000-0000-7000-8000-0000000000b2",
                "timestamp": "2026-10-26T00:00:00.000Z", "cwd": "/Users/me/proj"
            }),
            activity: serde_json::json!({
                "type": "message", "id": "m1", "parentId": null,
                "timestamp": "2026-10-26T00:00:01.000Z",
                "message": { "role": "assistant", "content": [
                    { "type": "toolCall", "id": "call_1", "name": "read",
                      "arguments": { "path": "NOTE.txt" } }
                ] }
            }),
        }
    }

    /// A root whose transcript directory exists before the watch, as a live
    /// session's does: inotify reports nothing written into a directory it
    /// has not yet watched.
    fn root_for(dir: &TempDir, wire: &Wire) -> std::path::PathBuf {
        let root = dunce::canonicalize(dir.path()).unwrap().join("sessions");
        std::fs::create_dir_all(root.join(wire.path).parent().unwrap()).unwrap();
        root
    }

    /// Returns once a closed write beside the transcript registers, so the
    /// watch is live before a test times its own write.
    async fn live_canary(
        root: &Path,
        wire: &Wire,
        rx: &mut mpsc::Receiver<(Transport, AgentEvent)>,
    ) {
        let (id, path, head) = wire.renamed("01a10000", "01a1cafe");
        tokio::fs::write(root.join(path), format!("{head}\n"))
            .await
            .unwrap();
        let id = AgentId::from_parts(wire.source, &id);
        collect_until(rx, "the watch going live", |e| is_start(e, &id)).await;
    }

    fn held(path: &Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
    }

    async fn an_append_decodes_while_held_open<S: Source>(source: S, root: &Path, wire: Wire) {
        let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
        let handle = tokio::spawn(Box::new(source).run(tx));
        live_canary(root, &wire, &mut rx).await;

        let mut file = held(&root.join(wire.path));
        file.write_all(format!("{}\n", wire.head).as_bytes())
            .unwrap();
        let id = AgentId::from_parts(wire.source, wire.id);
        let what = format!("{}: the held-open transcript's registration", wire.source);
        collect_until(&mut rx, &what, |e| is_start(e, &id)).await;
        file.write_all(format!("{}\n", wire.activity).as_bytes())
            .unwrap();
        file.flush().unwrap();
        let is_activity = |e: &AgentEvent| matches!(e, AgentEvent::ToolUseStart { agent_id, .. } if *agent_id == id);
        let what = format!("{}: an append to the still-open transcript", wire.source);
        collect_until(&mut rx, &what, is_activity).await;
        assert!(!handle.is_finished(), "the watcher stopped: {handle:?}");
        handle.abort();
    }

    #[tokio::test]
    async fn a_held_open_codex_append_decodes_without_close_or_poll() {
        let dir = TempDir::new().unwrap();
        let root = root_for(&dir, &codex());
        let source = CodexSource {
            sessions_root: root.clone(),
        };
        an_append_decodes_while_held_open(source, &root, codex()).await;
    }

    #[tokio::test]
    async fn a_held_open_omp_append_decodes_without_close_or_poll() {
        let dir = TempDir::new().unwrap();
        let root = root_for(&dir, &omp());
        let source = OmpSource::single_root(root.clone());
        an_append_decodes_while_held_open(source, &root, omp()).await;
    }

    /// codex opens its rollout, then runs `git` before writing the first line
    /// (`codex-rs/rollout/src/recorder.rs:1906-1917`, `:2022-2049` at
    /// `rust-v0.164.0`): the create event's walk finds an empty file.
    #[tokio::test]
    async fn a_late_first_line_registers_without_close_or_poll() {
        let dir = TempDir::new().unwrap();
        let wire = codex();
        let root = root_for(&dir, &wire);
        let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
        let source = CodexSource {
            sessions_root: root.clone(),
        };
        let handle = tokio::spawn(Box::new(source).run(tx));
        live_canary(&root, &wire, &mut rx).await;

        let mut file = held(&root.join(wire.path));
        // Past the create event's walk.
        tokio::time::sleep(Duration::from_millis(100)).await;
        file.write_all(format!("{}\n", wire.head).as_bytes())
            .unwrap();
        file.flush().unwrap();
        let id = AgentId::from_parts("codex", CODEX_ID);
        collect_until(
            &mut rx,
            "a first line written after the create event",
            |e| is_start(e, &id),
        )
        .await;
        assert!(!handle.is_finished(), "the watcher stopped: {handle:?}");
        handle.abort();
    }
}

fn is_start(event: &AgentEvent, id: &AgentId) -> bool {
    matches!(event, AgentEvent::SessionStart { agent_id, .. } if agent_id == id)
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

/// Waits for `uuid`'s codex session to register.
async fn expect_session_start(
    rx: &mut mpsc::Receiver<(Transport, AgentEvent)>,
    uuid: &str,
    what: &str,
) {
    let want = AgentId::from_parts("codex", uuid);
    collect_until(rx, what, |e| is_start(e, &want)).await;
}
