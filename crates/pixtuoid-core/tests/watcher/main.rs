mod attach;
mod first_sight;
mod liveness;
mod sources;
mod tailing;
mod unclaim;

use std::time::{Duration, SystemTime};

use filetime::{FileTime, set_file_mtime};

use pixtuoid_core::source::codex::decode_codex_line;
use pixtuoid_core::source::copilot::CopilotSource;
use pixtuoid_core::source::jsonl::{JsonlWatcher, ProbeSnapshot, force_polling_backend_for_tests};
use pixtuoid_core::source::{AgentEvent, Source, Transport};

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
    // THE production chain, not a mirror of it — hand-mirroring drifted three
    // times (#203, #632, the activity clock).
    pixtuoid_core::source::claude_code::cc_watcher(root)
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
    JsonlWatcher::new(root, "codex".to_string(), decode_codex_line, |_t| false)
}

fn codex_meta_line(uuid: &str, cwd: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "session_meta",
        "payload": { "id": uuid, "cwd": cwd }
    })
}

/// Decodes to an id-less `ToolUseStart`.
fn codex_work_line() -> serde_json::Value {
    serde_json::json!({
        "type": "event_msg",
        "payload": { "type": "task_started", "turn_id": "t" }
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
