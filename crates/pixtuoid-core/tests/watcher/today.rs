// `harness::Drive` folds lines with no watcher, so it can't see a tailer's
// `Backlog`: what a replay leaves of the day's count takes the real `Source::run`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use pixtuoid_core::source::antigravity::AntigravitySource;
use pixtuoid_core::source::claude_code::ClaudeCodeSource;
use pixtuoid_core::source::codex::CodexSource;
use pixtuoid_core::source::copilot::CopilotSource;
use pixtuoid_core::source::grok::GrokSource;
use pixtuoid_core::source::omp::OmpSource;
use pixtuoid_core::source::registry::{descriptor_for, registered_source_names};
use pixtuoid_core::source::{AgentEvent, DynSource, TaggedSender, Transport};
use pixtuoid_core::{Reducer, SceneState};

use crate::{
    Fold, append, codex_meta_line, codex_work_line, collect_until, fast_watch, recorded,
    split_before_last, stage, write_lines,
};

/// Past the watcher's default first-sight window: no clock may be what tells a
/// replay from live work.
const HELD_PAST_ANY_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);

fn utc_day(now: SystemTime) -> NaiveDate {
    let secs = now.duration_since(UNIX_EPOCH).unwrap().as_secs();
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .unwrap()
        .date_naive()
}

/// Transcript source `name` over `root`, short of production's unclaims and omp's profiles.
fn spawn_source(name: &str, root: PathBuf, tx: TaggedSender) -> JoinHandle<anyhow::Result<()>> {
    fast_watch();
    let source: Box<dyn DynSource> = match name {
        "claude-code" => {
            let mut src = ClaudeCodeSource::default_paths();
            src.projects_root = root;
            Box::new(src)
        }
        "codex" => Box::new(CodexSource {
            sessions_root: root,
            child_end_unclaims: None,
        }),
        "copilot" => Box::new(CopilotSource {
            sessions_root: root,
        }),
        "grok" => Box::new(GrokSource {
            sessions_root: root,
            child_end_unclaims: None,
        }),
        "omp" => Box::new(OmpSource::single_root(root)),
        "antigravity" => Box::new(AntigravitySource { brain_root: root }),
        other => panic!("{other} reads transcripts: drive it here"),
    };
    tokio::spawn(source.run(tx))
}

/// Whether `event` is one the day counts.
fn counts(event: &AgentEvent) -> bool {
    match event {
        AgentEvent::Usage { .. } => true,
        AgentEvent::ToolUseStart { detail, .. } => !detail.as_ref().is_some_and(|d| d.is_task()),
        _ => false,
    }
}

/// The slots' own totals: fresh tokens and tool calls.
fn totals(scene: &SceneState) -> (u64, u64) {
    scene.agents.values().fold((0, 0), |(tokens, calls), s| {
        (tokens + s.tokens_used, calls + u64::from(s.tool_call_count))
    })
}

/// `head` as a session active now wrote it: claude-code's first sight judges a
/// transcript by its lines' own stamps, and a recording's are as old as it is.
fn stamped_now(head: &str) -> String {
    let now = chrono::DateTime::from_timestamp(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        0,
    )
    .unwrap();
    let stamp = format!("{}T{}Z", now.date_naive(), now.time());
    let mut text = String::new();
    for line in head.lines() {
        let mut value: serde_json::Value = serde_json::from_str(line).expect("recorded JSON");
        if value.get("timestamp").is_some() {
            value["timestamp"] = stamp.clone().into();
        }
        text.push_str(&format!("{value}\n"));
    }
    text
}

/// Each transcript source's own recorded run, split at its last counted line: the
/// head replays at boot, however late it is folded, and the tail is live work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_transcript_source_replays_uncounted_and_then_reaches_live() {
    let sources: Vec<&str> = registered_source_names()
        .filter(|name| descriptor_for(name).is_some_and(|d| d.line_decoder().is_some()))
        .collect();
    let mut replayed_tokens = 0;
    for source in sources {
        let (transcript, recorded) = recorded(source, "tool-run-hermetic");
        let (head, tail) = split_before_last(source, &transcript, counts);
        let head = if source == "claude-code" {
            stamped_now(&head)
        } else {
            head
        };
        let (dir, path) = stage(&transcript, &recorded, &head);

        let (tx, rx) = mpsc::channel(64);
        let handle = spawn_source(source, dir.path().to_path_buf(), tx);
        let mut fold = Fold::new(
            rx,
            Reducer::new().counting(utc_day),
            SystemTime::now() + HELD_PAST_ANY_WINDOW,
        );
        fold.until(source, |e| matches!(e, AgentEvent::SessionStart { .. }))
            .await;
        assert!(!fold.scene.agents.is_empty(), "{source}: nobody registered");
        assert_eq!(fold.scene.today, None, "{source}: its replay counted");
        let (tokens, calls) = totals(&fold.scene);
        replayed_tokens += tokens;

        append(&path, &tail);
        fold.until(source, counts).await;
        handle.abort();

        let (tokens_after, calls_after) = totals(&fold.scene);
        let today = fold.scene.today.expect("the appended line counts");
        assert_eq!(today.day, utc_day(fold.at), "{source}");
        assert_eq!(
            (today.tokens, today.tool_calls),
            (tokens_after - tokens, calls_after - calls),
            "{source}: the count is the rise past the replay, not the slots' totals"
        );
        assert!(today.tokens + today.tool_calls > 0, "{source}");
    }
    assert!(
        replayed_tokens > 0,
        "no replay held tokens, so none proved they don't count"
    );
}

fn backlogs(events: &[(Transport, AgentEvent)]) -> Vec<bool> {
    (events.iter())
        .filter_map(|(_, e)| match e {
            AgentEvent::Backlog { replaying, .. } => Some(*replaying),
            _ => None,
        })
        .collect()
}

const UUID: &str = "0a0a0a0a-0b0b-7c0c-8d0d-0e0e0e0e0e0e";

async fn write_codex_session(dir: &Path) {
    let path = dir.join(format!("rollout-2026-01-01T00-00-00-{UUID}.jsonl"));
    write_lines(&path, &[codex_meta_line(UUID, "/repo"), codex_work_line()]).await;
}

fn is_tool(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::ToolUseStart { .. })
}

#[tokio::test]
async fn a_session_that_begins_under_the_watch_is_live_from_its_first_read() {
    let dir = TempDir::new().unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let handle = spawn_source("codex", dir.path().to_path_buf(), tx);
    tokio::time::sleep(Duration::from_millis(100)).await;

    write_codex_session(dir.path()).await;
    let events = collect_until(&mut rx, "codex", is_tool).await;
    handle.abort();

    assert_eq!(backlogs(&events), [false], "{events:?}");
}

/// The attach pass reads what predates its watch, like a boot's.
#[tokio::test]
async fn what_a_late_root_already_held_is_a_replay() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("sessions");
    let (tx, mut rx) = mpsc::channel(64);
    let handle = spawn_source("codex", root.clone(), tx);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let staged = dir.path().join("staged");
    tokio::fs::create_dir(&staged).await.unwrap();
    write_codex_session(&staged).await;
    tokio::fs::rename(&staged, &root).await.unwrap();
    let events = collect_until(&mut rx, "codex", is_tool).await;
    handle.abort();

    assert_eq!(backlogs(&events), [true, false], "{events:?}");
}

/// A retargeted symlink shows no anchor a thing, so only the poll finds the root.
#[cfg(unix)]
#[tokio::test]
async fn what_a_root_the_poll_attaches_already_held_is_a_replay() {
    use crate::codex_watcher;
    const POLL_ATTACHES: Duration = Duration::from_millis(40);

    let dir = TempDir::new().unwrap();
    let (empty, full) = (dir.path().join("empty"), dir.path().join("full"));
    tokio::fs::create_dir(&empty).await.unwrap();
    tokio::fs::create_dir_all(full.join("sessions"))
        .await
        .unwrap();
    write_codex_session(&full.join("sessions")).await;
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&empty, &link).unwrap();

    let (tx, mut rx) = mpsc::channel(64);
    let watcher = codex_watcher(link.join("sessions")).with_poll_interval(POLL_ATTACHES);
    let handle = tokio::spawn(watcher.run(tx));
    tokio::time::sleep(5 * POLL_ATTACHES).await;

    let next = dir.path().join("next");
    std::os::unix::fs::symlink(&full, &next).unwrap();
    std::fs::rename(&next, &link).unwrap();
    let events = collect_until(&mut rx, "codex", is_tool).await;
    handle.abort();

    assert_eq!(backlogs(&events), [true, false], "{events:?}");
}
