mod attach;
mod first_sight;
mod liveness;
mod sources;
mod tailing;
mod today;
mod unclaim;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use filetime::{FileTime, set_file_mtime};
use tokio::sync::mpsc;

use pixtuoid_core::harness::{Drive, fixtures_dir, sources_root, transcript_in};
use pixtuoid_core::source::copilot::CopilotSource;
use pixtuoid_core::source::jsonl::{JsonlWatcher, ProbeSnapshot, force_polling_backend_for_tests};
use pixtuoid_core::source::{AgentEvent, Source, Transport};
use pixtuoid_core::{Reducer, SceneState};

/// Run every watcher test's root watch on a fast `PollWatcher` backend instead
/// of the native FSEvents/inotify watcher, whose stream setup/teardown
/// dominated test time.
/// Idempotent / set-once. Call at the top of any test that drives a real
/// `JsonlWatcher`/`Source::run`.
fn fast_watch() {
    force_polling_backend_for_tests(POLL);
}

/// [`fast_watch`]'s scan period.
const POLL: Duration = Duration::from_millis(25);

/// A HEALTHY probe snapshot vouching exactly `ids`, each bound to this live
/// process's pid — a placeholder that can never spuriously instant-exit. The
/// id→pid exit-watch join belongs to `vouch_snapshot_with_pid`.
fn vouch_snapshot(ids: &[&str]) -> Option<ProbeSnapshot> {
    let pid = std::process::id() as i32;
    Some(ProbeSnapshot {
        pid_of: ids.iter().map(|s| (s.to_string(), pid)).collect(),
    })
}

fn cc_watcher(root: std::path::PathBuf) -> JsonlWatcher {
    fast_watch();
    JsonlWatcher::new(root, "claude-code".to_string()).expect("a transcript source")
}

/// Write a complete transcript (each line + trailing newline) in one shot, so
/// the watcher's first sight always reads the full fixture content.
async fn write_lines(path: &std::path::Path, lines: &[serde_json::Value]) {
    let mut content = String::new();
    for l in lines {
        content.push_str(&l.to_string());
        content.push('\n');
    }
    tokio::fs::write(path, content).await.unwrap();
}

fn backdate(path: &std::path::Path, secs: u64) {
    set_file_mtime(
        path,
        FileTime::from_system_time(SystemTime::now() - Duration::from_secs(secs)),
    )
    .unwrap();
}

fn cc_session_start_line(uuid: &str, cwd: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "system",
        "subtype": "session_start",
        "sessionId": uuid,
        "cwd": cwd
    })
}

fn cc_tool_use_line(
    uuid: &str,
    cwd: &str,
    tool_use_id: &str,
    name: &str,
    input: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "type": "assistant",
        "sessionId": uuid,
        "cwd": cwd,
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": tool_use_id, "name": name, "input": input }
            ]
        }
    })
}

fn cc_subagent_line(stem: &str, cwd: &str, tool_use_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "assistant",
        "sessionId": stem,
        "cwd": cwd,
        "attributionAgent": "feature-dev:code-explorer",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": tool_use_id, "name": "Read",
                  "input": { "file_path": "/x" } }
            ]
        }
    })
}

fn codex_watcher(root: std::path::PathBuf) -> JsonlWatcher {
    fast_watch();
    JsonlWatcher::new(root, "codex".to_string()).expect("a transcript source")
}

fn codex_meta_line(uuid: &str, cwd: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "session_meta",
        "payload": { "id": uuid, "cwd": cwd }
    })
}

/// Decodes to a call's `ToolUseStart`.
fn codex_work_line() -> serde_json::Value {
    serde_json::json!({
        "type": "response_item",
        "payload": { "type": "function_call", "name": "exec_command", "call_id": "c1" }
    })
}

/// Runs the production `CopilotSource` wiring: a bare `JsonlWatcher`, no probe
/// or label seams to differ from.
fn spawn_copilot(
    sessions_root: std::path::PathBuf,
    tx: tokio::sync::mpsc::Sender<(Transport, AgentEvent)>,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    fast_watch();
    tokio::spawn(async move { Box::new(CopilotSource { sessions_root }).run(tx).await })
}

fn copilot_start_line(sid: &str, cwd: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "session.start",
        "data": { "sessionId": sid, "context": { "cwd": cwd } }
    })
}

fn copilot_tool_line(call_id: &str, tool: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "tool.execution_start",
        "data": { "toolCallId": call_id, "toolName": tool, "arguments": { "path": "/repo" } }
    })
}

fn ag_step_line() -> serde_json::Value {
    serde_json::json!({ "step_index": 0, "type": "PLANNER_RESPONSE", "tool_calls": [] })
}

/// [`collect_until`]'s bound on reaching its event, and the quiet that ends it.
const DEADLINE: Duration = Duration::from_secs(15);
const QUIET: Duration = Duration::from_millis(300);

/// Every event up to the stream's first quiet after `done` accepts one.
async fn collect_until(
    rx: &mut mpsc::Receiver<(Transport, AgentEvent)>,
    what: &str,
    done: impl Fn(&AgentEvent) -> bool,
) -> Vec<(Transport, AgentEvent)> {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut events = Vec::new();
    let mut reached = false;
    loop {
        match tokio::time::timeout(QUIET, rx.recv()).await {
            Ok(Some(tagged)) => {
                reached |= done(&tagged.1);
                events.push(tagged);
            }
            Ok(None) => panic!("{what}: the stream closed"),
            Err(_) if reached => return events,
            Err(_) => {}
        }
        assert!(tokio::time::Instant::now() < deadline, "{what}: no end");
    }
}

/// A reducer folding a source's stream, every event at `at`.
struct Fold {
    rx: mpsc::Receiver<(Transport, AgentEvent)>,
    reducer: Reducer,
    scene: SceneState,
    at: SystemTime,
}

impl Fold {
    fn new(rx: mpsc::Receiver<(Transport, AgentEvent)>, reducer: Reducer, at: SystemTime) -> Self {
        Self {
            rx,
            reducer,
            scene: SceneState::uniform(8),
            at,
        }
    }

    /// Folds what [`collect_until`] gathers, and returns it.
    async fn until(&mut self, what: &str, done: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
        let mut folded = Vec::new();
        for (transport, event) in collect_until(&mut self.rx, what, done).await {
            folded.push(event.clone());
            self.reducer
                .apply(&mut self.scene, event, self.at, transport);
        }
        folded
    }
}

/// `source`'s one transcript in its recorded `scenario`, and that scenario's dir.
fn recorded(source: &str, scenario: &str) -> (PathBuf, PathBuf) {
    let dir = fixtures_dir(&sources_root(), source).join(scenario);
    let transcript = transcript_in(source, &dir)
        .unwrap_or_else(|found| panic!("{source}: one recorded transcript, found {found:?}"));
    (transcript, dir)
}

/// `transcript`'s lines before the last one `source` decodes to an event `at`
/// accepts, and the rest.
fn split_before_last(
    source: &str,
    transcript: &Path,
    at: impl Fn(&AgentEvent) -> bool,
) -> (String, String) {
    let body = std::fs::read_to_string(transcript).expect("a committed transcript");
    let drive = Drive::transcript_at(source, transcript).expect("a transcript source");
    let lines: Vec<&str> = body.lines().collect();
    let last = lines
        .iter()
        .rposition(|line| drive.lines([*line]).events.iter().any(&at))
        .unwrap_or_else(|| panic!("{source}: no line decodes to it"));
    assert!(last > 0, "{source}: nothing before it");
    let text = |part: &[&str]| part.iter().map(|l| format!("{l}\n")).collect();
    (text(&lines[..last]), text(&lines[last..]))
}

/// `head` written where `transcript` sits under `dir`, in a fresh root.
fn stage(transcript: &Path, dir: &Path, head: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::TempDir::new().unwrap();
    let path = root
        .path()
        .join(transcript.strip_prefix(dir).expect("under it"));
    std::fs::create_dir_all(path.parent().expect("a parent")).unwrap();
    std::fs::write(&path, head).unwrap();
    (root, path)
}

fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    std::io::Write::write_all(&mut file, text.as_bytes()).unwrap();
}
