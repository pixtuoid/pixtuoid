//! GitHub Copilot CLI source. Watches the agentic `copilot` (`@github/copilot`)
//! session transcript (`<copilot_home>/session-state/<sessionId>/events.jsonl`)
//! via `JsonlWatcher`. Transcript-ONLY: the whole lifecycle is persisted to
//! `events.jsonl`, so there is no hook install target. Only streaming events
//! (`session.idle`, `*_delta`, `*_progress`, …) carry `ephemeral` and never hit
//! disk; the decoder simply ignores everything it doesn't map.
//!
//! Sharp edges (real-byte-confirmed):
//! - **Session id = the PARENT-DIR UUID** of `events.jsonl` (the filename stem is
//!   the constant `events`, NOT the id).
//! - **Sub-agents INTERLEAVE in the root file**: each line of a child carries its
//!   own envelope `agentId` (a UUID, not the spawning `task` call's id, which rides
//!   `data.toolCallId` / `data.parentToolCallId`); there is no per-agent file split.
//! - `subagent.completed` is **minimal** on disk (`toolCallId`/`agentName`/
//!   `agentDisplayName` only) — never require model/token/duration fields.
//! - The `ephemeral` envelope flag is inconsistent across CLI versions — never
//!   rely on it; map by `type` and ignore the rest.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::AgentId;
use crate::source::decoder::{MAX_DECODED_FIELD_CHARS, accept_all_paths, ellipsize};
use crate::source::registry::{
    ChildEndInference, FocusChannel, Hyphen, SourceCaps, SourceDescriptor, SourceKind, Transcript,
};
use crate::source::{AgentEvent, RequestKeys, ToolDetail};

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::CopilotSource;

/// The GitHub Copilot CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "copilot";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    label_prefix: "cp",
    verified_version: "1.0.104",
    version_probe: Some(&["copilot", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: Some("COPILOT_HOME"),
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: decode_copilot_line,
            id_from_path: copilot_id_from_path,
            path_filter: accept_all_paths,
            cwd_extractor: extract_copilot_cwd,
            parent_extractor: None,
        }),
        hook: None,
        caps: SourceCaps {
            // `session.shutdown` is a real persisted exit marker.
            has_exit_signal: true,
            // sessionId is constant across `--resume`, so a swept session does
            // not silently walk back in.
            resurrects_on_prompt: false,
            // The `task` dispatch emits `tool.execution_start`/`complete` AND
            // explicit `subagent.started`/`completed` events.
            delegations_are_hook_silent: false,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
            brackets_turns: false,
        },
        focus: FocusChannel::Unsupported,
    },
    #[cfg(feature = "native")]
    native: native::NATIVE,
};

/// `$COPILOT_HOME` if set, else `~/.copilot` — mirroring copilot 1.0.78's
/// `resolveCopilotHome` (in the `runtime.node` addon, probed directly): empty
/// is unset, `~` is NOT expanded, `<home>/.copilot` on every platform (the
/// nearby `%LOCALAPPDATA%` branch is the CACHE dir, not this one).
///
/// Whitespace-only, `--config-dir` and XDG are the deliberate divergences: this
/// mirrors copilot's own resolver, not a generic union of the CLIs' axes.
#[cfg(feature = "native")]
pub(crate) fn copilot_home() -> PathBuf {
    match crate::platform::path_env("COPILOT_HOME") {
        Some(v) => crate::platform::warn_if_relative_override("COPILOT_HOME", v),
        None => crate::platform::user_home().join(".copilot"),
    }
}

/// The session id = the **parent directory name** of `events.jsonl`
/// (`…/session-state/<sessionId>/events.jsonl`). The filename stem is the
/// constant `events`, so — unlike CC/Codex — the id is the containing dir.
pub(crate) fn copilot_id_from_path(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// The root's `SessionStart`, at the cwd its event's `context` names.
fn root_start(root: AgentId, source: &str, session_id: &str, data: Option<&Value>) -> AgentEvent {
    let cwd = data
        .and_then(|d| d.get("context"))
        .and_then(|c| str_at(c, "cwd"))
        .unwrap_or("");
    AgentEvent::SessionStart {
        agent_id: root,
        source: source.to_string(),
        session_id: session_id.to_string(),
        cwd: PathBuf::from(cwd),
        parent_id: None,
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

fn copilot_child_key<'a>(v: &'a Value, data: Option<&'a Value>) -> Option<&'a str> {
    // BOTH branches filter empty: an empty `toolCallId` would otherwise mint a
    // child `AgentId` keyed on "", colliding every such child onto one slot.
    str_at(v, "agentId")
        .filter(|s| !s.is_empty())
        .or_else(|| data.and_then(|d| str_at(d, "toolCallId")))
        .filter(|s| !s.is_empty())
}

/// First-sight cwd extractor for the walker's head scan. Without it a copilot
/// transcript gated at first sight then tail-revived registers an empty-cwd root
/// (→ the short unknown-cwd reap) and its label degrades from `cp·<dir>` to `cp#N`.
pub(crate) fn extract_copilot_cwd(v: &Value) -> Option<PathBuf> {
    v.get("data")?
        .get("context")?
        .get("cwd")?
        .as_str()
        .map(PathBuf::from)
}

const SESSION_START: &str = "session.start";
const SESSION_RESUME: &str = "session.resume";
const TOOL_EXECUTION_START: &str = "tool.execution_start";
const TOOL_EXECUTION_COMPLETE: &str = "tool.execution_complete";
const PERMISSION_REQUESTED: &str = "permission.requested";
const PERMISSION_COMPLETED: &str = "permission.completed";
const SUBAGENT_STARTED: &str = "subagent.started";
const SUBAGENT_COMPLETED: &str = "subagent.completed";
const SUBAGENT_FAILED: &str = "subagent.failed";
const SESSION_TASK_COMPLETE: &str = "session.task_complete";
const SESSION_SHUTDOWN: &str = "session.shutdown";

/// Decode one `events.jsonl` line into zero or more `AgentEvent`s. Unknown,
/// ephemeral, or malformed shapes return `vec![]` and never panic — real files
/// carry embedded-newline / U+2028 corruption (upstream copilot-cli #2649/#2012).
pub(crate) fn decode_copilot_line(
    transcript_path: &str,
    source: &str,
    v: Value,
) -> Vec<AgentEvent> {
    let root = AgentId::from_parts(source, &copilot_id_from_path(Path::new(transcript_path)));
    let Some(obj) = v.as_object() else {
        return vec![];
    };
    let kind = obj.get("type").and_then(|s| s.as_str()).unwrap_or("");
    let data = obj.get("data");

    let acting = match str_at(&v, "agentId") {
        Some(aid) if !aid.is_empty() => AgentId::from_parts(source, aid),
        _ => root,
    };

    match kind {
        SESSION_START => {
            let session_id = data
                .and_then(|d| str_at(d, "sessionId"))
                .unwrap_or_else(|| {
                    crate::source::drift::missing_field(source, "session.start", "sessionId");
                    ""
                });
            vec![root_start(root, source, session_id, data)]
        }
        // The id rides the path: a resume keeps the sessionId, and its
        // `ResumeData` carries none.
        SESSION_RESUME => vec![root_start(
            root,
            source,
            &copilot_id_from_path(Path::new(transcript_path)),
            data,
        )],
        TOOL_EXECUTION_START => {
            let Some(d) = data else {
                crate::source::drift::missing_field(source, "tool.execution_start", "data");
                return vec![];
            };
            let Some(tool_call_id) = str_at(d, "toolCallId") else {
                crate::source::drift::missing_field(source, "tool.execution_start", "toolCallId");
                return vec![];
            };
            let tool_name = str_at(d, "toolName").unwrap_or_else(|| {
                crate::source::drift::missing_field(source, "tool.execution_start", "toolName");
                ""
            });
            let detail = copilot_tool_detail(tool_name, d.get("arguments"));
            vec![AgentEvent::ToolUseStart {
                agent_id: acting,
                tool_use_id: Some(tool_call_id.to_string()),
                detail: Some(detail),
            }]
        }
        TOOL_EXECUTION_COMPLETE => {
            let Some(d) = data else {
                crate::source::drift::missing_field(source, "tool.execution_complete", "data");
                return vec![];
            };
            let Some(tool_call_id) = str_at(d, "toolCallId") else {
                crate::source::drift::missing_field(
                    source,
                    "tool.execution_complete",
                    "toolCallId",
                );
                return vec![];
            };
            let mut out = vec![AgentEvent::ToolUseEnd {
                agent_id: acting,
                tool_use_id: Some(tool_call_id.to_string()),
            }];
            // Copilot stamps the model PER TOOL (`data.model` can differ
            // mid-session), attributed to the ACTING agent so a subagent's
            // tool doesn't repaint the root.
            if let Some(model) = str_at(d, "model").filter(|m| !m.is_empty()) {
                out.push(AgentEvent::ModelInfo {
                    agent_id: acting,
                    model: Some(ellipsize(model, MAX_DECODED_FIELD_CHARS)),
                    effort: None,
                });
            }
            out
        }
        PERMISSION_REQUESTED => {
            let request = data.and_then(|d| d.get("permissionRequest"));
            let reason = request
                .and_then(|p| str_at(p, "kind"))
                // Capped at the decode boundary: `kind` is raw wire content that
                // persists in the slot + egresses on the headless summary.
                .map(|k| ellipsize(&format!("permission: {k}"), MAX_DECODED_FIELD_CHARS))
                .unwrap_or_else(|| "permission".to_string());
            vec![AgentEvent::user_action_request(
                acting,
                Some(reason),
                RequestKeys {
                    // The gated call (#951), so its own end resolves the wait.
                    tool_use_id: request
                        .and_then(|p| str_at(p, "toolCallId"))
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned),
                    ..RequestKeys::default()
                },
            )]
        }
        // A refusal ends the gated call: a denial's `tool.execution_complete`
        // follows, a cancel's (Esc at the prompt) never does, and the second
        // end is a no-op. An approved call ends on its own.
        PERMISSION_COMPLETED => {
            let approved = data
                .and_then(|d| d.get("result"))
                .and_then(|r| str_at(r, "kind"))
                .is_some_and(|k| k.starts_with("approved"));
            match data.and_then(|d| str_at(d, "toolCallId")) {
                Some(id) if !approved && !id.is_empty() => vec![AgentEvent::ToolUseEnd {
                    agent_id: acting,
                    tool_use_id: Some(id.to_owned()),
                }],
                _ => vec![],
            }
        }
        SUBAGENT_STARTED => {
            let Some(child_key) = copilot_child_key(&v, data) else {
                return vec![];
            };
            let child = AgentId::from_parts(source, child_key);
            let mut evs = vec![AgentEvent::SessionStart {
                agent_id: child,
                source: source.to_string(),
                session_id: child_key.to_string(),
                cwd: PathBuf::new(), // sub-agents carry no cwd
                parent_id: Some(root),
            }];
            if let Some(name) = data
                .and_then(|d| str_at(d, "agentDisplayName"))
                .filter(|s| !s.is_empty())
            {
                // Capped at decode: `agentDisplayName` is transcript content that
                // persists in slot state and egresses on the headless summary. The
                // non-empty filter above keeps an empty name from emitting a
                // blanking `Rename { label: "" }`.
                evs.push(AgentEvent::Rename {
                    agent_id: child,
                    label: ellipsize(name, MAX_DECODED_FIELD_CHARS),
                });
            }
            evs
        }
        SUBAGENT_COMPLETED | SUBAGENT_FAILED => {
            let Some(child_key) = copilot_child_key(&v, data) else {
                return vec![];
            };
            vec![AgentEvent::SessionEnd {
                agent_id: AgentId::from_parts(source, child_key),
                as_child: true,
            }]
        }
        // Only the root's ends its turn: neither listener of it in 1.0.98's
        // bundled `cli-main.js` (:2122, :3100) passes `includeSubAgents`, without
        // which `SessionClient._dispatchEvent` (:1536) skips one with an `agentId`.
        SESSION_TASK_COMPLETE if acting == root => vec![AgentEvent::turn_end(root)],
        // `modelMetrics` is the one usage a shutdown always carries, hosted or
        // BYOK (1.0.103's `schemas/session-events.schema.json`, `ShutdownData`'s
        // `required`); its `inputTokens` counts cache reads, which are not fresh.
        SESSION_SHUTDOWN => {
            let mut evs = vec![AgentEvent::SessionEnd {
                agent_id: root,
                as_child: false,
            }];
            let fresh = data
                .and_then(|d| d.get("modelMetrics"))
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|models| models.values())
                .filter_map(|metric| metric.get("usage"))
                .map(|usage| {
                    let count = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
                    count("inputTokens")
                        .saturating_sub(count("cacheReadTokens"))
                        .saturating_add(count("outputTokens"))
                })
                .fold(0u64, u64::saturating_add);
            if fresh > 0 {
                evs.push(AgentEvent::Usage {
                    agent_id: root,
                    fresh_tokens: fresh,
                });
            }
            evs
        }
        _ => vec![],
    }
}

/// Copilot's tool-detail dispatch. The sub-agent dispatch is the `task` tool,
/// detected by NAME — routing non-`task` tools through the shared
/// [`crate::source::decoder::make_tool_detail`] (which flags a Task on the mere
/// PRESENCE of a `subagent_type` input key) would let a model-authored /
/// hallucinated `subagent_type` arg spoof an ordinary tool into a delegation,
/// prematurely draining `tasks_in_flight` and evicting real children.
fn copilot_tool_detail(tool: &str, args: Option<&Value>) -> ToolDetail {
    if tool == "task" {
        return ToolDetail::Task;
    }
    // Copilot's own arg vocabulary (bash→command, view/read/write→path,
    // grep→pattern) — `describe_tool_target` knows only CC's keys.
    const KEYS: &[&str] = &["command", "path", "filePath", "pattern", "query"];
    crate::source::decoder::generic_keyed_detail(tool, args, KEYS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each kind reaches an arm. The discriminator is REACHING an arm, not
    /// emitting an event — several arms legitimately decode a well-formed line
    /// to nothing — so a real arm breadcrumbs a missing field and the catch-all
    /// stays silent.
    #[test]
    fn each_kind_reaches_an_arm() {
        let path = "/p/11111111-2222-3333-4444-555555555555/events.jsonl";
        let drive = |kind: &str| {
            let line = serde_json::json!({
                "type": kind, "data": {"toolCallId": "t1"}, "id": "e1",
                "timestamp": "ts", "parentId": null,
            });
            let mut evs = 0;
            let logs = crate::test_capture::capture_logs(|| {
                evs = decode_copilot_line(path, "copilot", line).len();
            });
            evs > 0 || logs.contains(crate::source::drift::TARGET)
        };
        for kind in [
            SESSION_START,
            SESSION_RESUME,
            TOOL_EXECUTION_START,
            TOOL_EXECUTION_COMPLETE,
            PERMISSION_REQUESTED,
            PERMISSION_COMPLETED,
            SUBAGENT_STARTED,
            SUBAGENT_COMPLETED,
            SUBAGENT_FAILED,
            SESSION_TASK_COMPLETE,
            SESSION_SHUTDOWN,
        ] {
            assert!(drive(kind), "{kind} must reach a real arm");
        }
        assert!(
            !drive("session.archived"),
            "an unhandled kind reaches neither"
        );
    }

    #[test]
    fn execution_complete_surfaces_the_per_tool_model() {
        let line = r#"{"type":"tool.execution_complete","data":{"toolCallId":"t1","model":"claude-haiku-4.5","success":true},"id":"e1","timestamp":"ts","parentId":null}"#;
        let evs = decode_copilot_line(
            "/p/11111111-2222-3333-4444-555555555555/events.jsonl",
            "copilot",
            serde_json::from_str(line).unwrap(),
        );
        assert!(
            evs.iter().any(|e| matches!(e, AgentEvent::ModelInfo { model: Some(m), effort: None, .. } if m == "claude-haiku-4.5")),
            "per-tool model must surface, got {evs:?}"
        );
        let line = r#"{"type":"tool.execution_complete","data":{"toolCallId":"t2","success":true},"id":"e2","timestamp":"ts","parentId":null}"#;
        let evs = decode_copilot_line(
            "/p/11111111-2222-3333-4444-555555555555/events.jsonl",
            "copilot",
            serde_json::from_str(line).unwrap(),
        );
        assert!(
            !evs.iter()
                .any(|e| matches!(e, AgentEvent::ModelInfo { .. })),
            "got {evs:?}"
        );
    }
    use serde_json::json;

    const PATH: &str = "/p/session-state/65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3/events.jsonl";

    fn root() -> AgentId {
        AgentId::from_parts(SOURCE_NAME, "65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3")
    }
    fn decode(line: &str) -> Vec<AgentEvent> {
        decode_copilot_line(PATH, SOURCE_NAME, serde_json::from_str(line).unwrap())
    }

    #[test]
    fn id_from_path_uses_the_parent_dir_not_the_stem() {
        assert_eq!(
            copilot_id_from_path(Path::new(PATH)),
            "65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3"
        );
    }

    #[test]
    fn empty_agent_id_attributes_to_the_root() {
        let line = r#"{"type":"tool.execution_start","agentId":"","data":{"toolCallId":"call_1","toolName":"bash","arguments":{"command":"ls"}}}"#;
        match &decode(line)[..] {
            [AgentEvent::ToolUseStart { agent_id, .. }] => {
                assert_eq!(*agent_id, root(), "empty agentId must key to the root");
            }
            other => panic!("expected one ToolUseStart, got {other:?}"),
        }
    }

    #[test]
    fn missing_tool_call_id_drops_with_a_drift_breadcrumb() {
        let out = crate::test_capture::capture_logs(|| {
            let line = r#"{"type":"tool.execution_start","data":{"toolName":"bash"}}"#;
            assert!(
                decode(line).is_empty(),
                "an unkeyable tool start yields no event"
            );
        });
        for needle in [
            crate::source::drift::TARGET,
            "missing_field",
            "tool.execution_start",
            "toolCallId",
            SOURCE_NAME,
        ] {
            assert!(
                out.contains(needle),
                "missing {needle:?} in captured log:\n{out}"
            );
        }
    }

    #[test]
    fn subagent_completed_keys_off_the_envelope_agent_id() {
        let line = r#"{"type":"subagent.completed","agentId":"call_7","data":{}}"#;
        match &decode(line)[..] {
            [AgentEvent::SessionEnd { agent_id, as_child }] => {
                assert_eq!(*agent_id, AgentId::from_parts(SOURCE_NAME, "call_7"));
                assert!(as_child);
            }
            other => panic!("expected one child SessionEnd, got {other:?}"),
        }
    }

    /// A `--resume` keeps the sessionId and appends to the same events.jsonl
    /// past its `session.shutdown`, so its structural marker is what revives
    /// the session; the schema's `ResumeData` carries no sessionId.
    #[test]
    fn session_resume_revives_the_root_with_its_cwd() {
        let line = r#"{"type":"session.resume","data":{"resumeTime":"2026-05-22T06:20:00.000Z","eventCount":42,"context":{"cwd":"d:\\contentforge-fullstack (1)"}},"id":"r1","timestamp":"2026-05-22T06:20:00.010Z","parentId":"220c4131"}"#;
        match &decode(line)[..] {
            [
                AgentEvent::SessionStart {
                    agent_id,
                    session_id,
                    cwd,
                    parent_id,
                    ..
                },
            ] => {
                assert_eq!(*agent_id, root());
                assert_eq!(session_id, "65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3");
                assert_eq!(cwd, Path::new(r"d:\contentforge-fullstack (1)"));
                assert_eq!(*parent_id, None);
            }
            other => panic!("expected one SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn real_task_tool_is_delegating() {
        let line = r#"{"type":"tool.execution_start","data":{"toolCallId":"call_SGMJ1yjMtpgFUbZct2fEo2Hk","toolName":"task","arguments":{"description":"Incident command response","agent_type":"sisko","name":"sisko-incident-command","mode":"sync"},"turnId":"0"},"id":"a","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ToolUseStart {
                    detail: Some(d), ..
                },
            ] => assert!(d.is_task(), "task tool must be Delegating, got {d:?}"),
            other => panic!("expected Delegating ToolUseStart, got {other:?}"),
        }
    }

    #[test]
    fn spoofed_subagent_type_arg_does_not_make_a_task() {
        let line = r#"{"type":"tool.execution_start","data":{"toolCallId":"c1","toolName":"view","arguments":{"path":"x.rs","subagent_type":null}},"id":"a","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ToolUseStart {
                    detail: Some(d), ..
                },
            ] => assert!(
                !d.is_task(),
                "a spoofed subagent_type arg must stay Generic, got {d:?}"
            ),
            other => panic!("expected Generic ToolUseStart, got {other:?}"),
        }
    }

    #[test]
    fn ordinary_tool_shows_its_own_arg_target() {
        let line = r#"{"type":"tool.execution_start","data":{"toolCallId":"c2","toolName":"bash","arguments":{"command":"cargo test"}},"id":"a","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ToolUseStart {
                    detail: Some(ToolDetail::Generic { display }),
                    ..
                },
            ] => assert!(
                display.contains("cargo test"),
                "bash tool should show its command target, got {display:?}"
            ),
            other => panic!("expected Generic ToolUseStart with target, got {other:?}"),
        }
    }

    #[test]
    fn real_subagent_started_registers_child_parented_to_root_then_renamed() {
        let line = r#"{"type":"subagent.started","data":{"toolCallId":"call_SGMJ1yjMtpgFUbZct2fEo2Hk","agentName":"sisko","agentDisplayName":"Sisko - Incident Commander / SRE Lead","agentDescription":"Sisko"},"id":"d171d290","timestamp":"2026-05-26T14:14:22.773Z","parentId":"83d641f1","agentId":"call_SGMJ1yjMtpgFUbZct2fEo2Hk"}"#;
        let child = AgentId::from_parts(SOURCE_NAME, "call_SGMJ1yjMtpgFUbZct2fEo2Hk");
        match &decode(line)[..] {
            [
                AgentEvent::SessionStart {
                    agent_id,
                    parent_id,
                    ..
                },
                AgentEvent::Rename { agent_id: r, label },
            ] => {
                assert_eq!(*agent_id, child);
                assert_eq!(*parent_id, Some(root()));
                assert_eq!(*r, child);
                assert_eq!(label, "Sisko - Incident Commander / SRE Lead");
            }
            other => panic!("expected SessionStart+Rename, got {other:?}"),
        }
    }

    #[test]
    fn subagent_display_name_is_capped_and_empty_is_dropped() {
        let over = "x".repeat(MAX_DECODED_FIELD_CHARS + 50);
        let line = serde_json::json!({
            "type": "subagent.started",
            "data": {"toolCallId": "call_X", "agentDisplayName": over},
            "parentId": "p",
            "agentId": "call_X"
        })
        .to_string();
        match &decode(&line)[..] {
            [
                AgentEvent::SessionStart { .. },
                AgentEvent::Rename { label, .. },
            ] => {
                assert!(
                    label.chars().count() <= MAX_DECODED_FIELD_CHARS + 1,
                    "label not capped: {} chars",
                    label.chars().count()
                );
                assert!(
                    label.ends_with('…'),
                    "expected an ellipsis on the capped label"
                );
            }
            other => panic!("expected SessionStart + capped Rename, got {other:?}"),
        }
        let empty = serde_json::json!({
            "type": "subagent.started",
            "data": {"toolCallId": "call_Y", "agentDisplayName": ""},
            "parentId": "p",
            "agentId": "call_Y"
        })
        .to_string();
        match &decode(&empty)[..] {
            [AgentEvent::SessionStart { .. }] => {}
            other => panic!("expected SessionStart only (no blanking Rename), got {other:?}"),
        }
    }

    #[test]
    fn real_subagent_completed_ends_child_as_child() {
        let line = r#"{"type":"subagent.completed","data":{"toolCallId":"call_kuB1BVYZyE3ih6ClBvbyKtZk","agentName":"rom","agentDisplayName":"Rom - Database Reliability Engineer"},"id":"e7ab205e","timestamp":"2026-05-26T14:14:43.099Z","parentId":"f85ba2bd","agentId":"call_kuB1BVYZyE3ih6ClBvbyKtZk"}"#;
        match &decode(line)[..] {
            [AgentEvent::SessionEnd { agent_id, as_child }] => {
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts(SOURCE_NAME, "call_kuB1BVYZyE3ih6ClBvbyKtZk")
                );
                assert!(*as_child);
            }
            other => panic!("expected child SessionEnd, got {other:?}"),
        }
    }

    #[test]
    fn real_subagent_failed_ends_child_as_child() {
        let line = r#"{"type":"subagent.failed","data":{"toolCallId":"toolu_bdrk_014wc1joyQCq3f6RBzGcxVRb","agentName":"general-purpose","agentDisplayName":"General Purpose Agent","model":"claude-haiku-4.5","totalToolCalls":0,"durationMs":2183,"error":"No response generated"},"id":"225a0bef-8b18-4d4d-a643-4cedd7f2e603","timestamp":"2026-06-14T21:30:31.494Z","parentId":"5a8b7e5e-9d2c-43b7-82b2-1d8f98f820de","agentId":"toolu_bdrk_014wc1joyQCq3f6RBzGcxVRb"}"#;
        match &decode(line)[..] {
            [AgentEvent::SessionEnd { agent_id, as_child }] => {
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts(SOURCE_NAME, "toolu_bdrk_014wc1joyQCq3f6RBzGcxVRb")
                );
                assert!(*as_child, "a subagent failure is a child end");
            }
            other => panic!("expected child SessionEnd, got {other:?}"),
        }
    }

    /// BOTH `copilot_child_key` branches must reject an empty string. The
    /// primary (`agentId`) always did; the `data.toolCallId` fallback did not,
    /// so an empty id minted a child `AgentId` keyed on "" — every such child
    /// colliding onto one slot.
    #[test]
    fn an_empty_child_key_is_rejected_on_both_branches() {
        for line in [
            r#"{"type":"subagent.completed","agentId":"","data":{"toolCallId":""}}"#,
            r#"{"type":"subagent.completed","data":{"toolCallId":""}}"#,
            r#"{"type":"subagent.failed","agentId":"","data":{"toolCallId":""}}"#,
            // The STARTED arm is the other `copilot_child_key` call site, and
            // `data.toolCallId` is its fallback key without an `agentId` — the
            // existing started-arm test omits the key entirely, so the
            // empty-string shape was unpinned there.
            r#"{"type":"subagent.started","data":{"toolCallId":"","agentName":"x"}}"#,
        ] {
            assert!(
                decode(line).is_empty(),
                "an empty child key must emit nothing, not a \"\"-keyed child: {line}"
            );
        }
        // The FIRST filter's whole job, and nothing else pins it: an empty
        // envelope `agentId` must FALL THROUGH to `data.toolCallId` rather than
        // short-circuit the `or_else` into `None`. Delete it and the assertions
        // above still pass, while every real child silently stops registering.
        let line = r#"{"type":"subagent.completed","agentId":"","data":{"toolCallId":"call_1"}}"#;
        assert_eq!(
            decode(line).len(),
            1,
            "an empty agentId must fall back to toolCallId, not drop the child"
        );
    }

    #[test]
    fn child_tool_line_attributes_to_the_child_via_envelope_agent_id() {
        let line = json!({
            "type": "tool.execution_start",
            "data": {"toolCallId": "tooluse_child1", "toolName": "view", "arguments": {}},
            "id": "x", "timestamp": "t", "parentId": null,
            "agentId": "call_SGMJ1yjMtpgFUbZct2fEo2Hk"
        })
        .to_string();
        match &decode(&line)[..] {
            [AgentEvent::ToolUseStart { agent_id, .. }] => assert_eq!(
                *agent_id,
                AgentId::from_parts(SOURCE_NAME, "call_SGMJ1yjMtpgFUbZct2fEo2Hk"),
                "a line with envelope agentId must attribute to the CHILD, not root"
            ),
            other => panic!("expected child ToolUseStart, got {other:?}"),
        }
    }

    #[test]
    fn a_permission_waits_on_its_call_and_a_refusal_ends_it() {
        let req = r#"{"type":"permission.requested","data":{"requestId":"8c508e21-0a6c-4a06-8824-3930476499ea","permissionRequest":{"kind":"shell","toolCallId":"call_K8WLZkwufHsI9bTvkZmMKec2","fullCommandText":"cat /etc/hostname","intention":"Print /etc/hostname contents","commands":[{"identifier":"cat","readOnly":true}],"possiblePaths":["/etc/hostname"],"possibleUrls":[],"hasWriteFileRedirection":false,"canOfferSessionApproval":true},"promptRequest":{"kind":"path","accessKind":"shell","paths":["/etc/hostname"],"toolCallId":"call_K8WLZkwufHsI9bTvkZmMKec2"}},"id":"1f975691-a108-4d6f-924b-d48263d46274","timestamp":"2026-06-14T21:35:55.637Z","parentId":"e0a534c6-d548-4def-b0bd-316c83efe5fd"}"#;
        match &decode(req)[..] {
            [
                AgentEvent::UserActionRequest {
                    agent_id,
                    reason: Some(reason),
                    tool_use_id,
                    ..
                },
            ] => {
                assert_eq!(*agent_id, root());
                assert!(reason.contains("shell"), "reason names the gate: {reason}");
                assert_eq!(
                    tool_use_id.as_deref(),
                    Some("call_K8WLZkwufHsI9bTvkZmMKec2"),
                    "the wait names the call it gates"
                );
            }
            other => panic!("expected UserActionRequest, got {other:?}"),
        }
        let approved = r#"{"type":"permission.completed","data":{"requestId":"8c508e21-0a6c-4a06-8824-3930476499ea","toolCallId":"call_K8WLZkwufHsI9bTvkZmMKec2","result":{"kind":"approved"}},"id":"8123a44a-3471-4262-9191-b3cddaf5224d","timestamp":"2026-06-14T21:35:58.218Z","parentId":"1f975691-a108-4d6f-924b-d48263d46274"}"#;
        assert!(
            decode(approved).is_empty(),
            "approved → no event (the call ends on its own)"
        );

        // The REAL refusals: the interactive reject, the non-interactive
        // no-rule auto-deny, and Esc at the prompt (1.0.98).
        for (refused, call) in [
            (
                r#"{"type":"permission.completed","data":{"requestId":"954afe31-559a-4afc-9eb6-13e30cf48aea","toolCallId":"call_nf1RvU9GxssNg2g7WtPgHqQ4","result":{"kind":"denied-interactively-by-user"}},"id":"60dae716-c76c-45e2-84e1-c3248ce3790c","timestamp":"2026-06-14T21:38:43.086Z","parentId":"5240af45-3ad2-4bf7-bc37-83c329c9c2ea"}"#,
                "call_nf1RvU9GxssNg2g7WtPgHqQ4",
            ),
            (
                r#"{"type":"permission.completed","data":{"requestId":"eab9bd2c-ca42-4ab6-8567-1c11906500a6","toolCallId":"toolu_bdrk_015JoceQkzNKnLkeCj5NaLzT","result":{"kind":"denied-no-approval-rule-and-could-not-request-from-user"}},"id":"2cc6bffe-6443-4d8b-9765-dbfeda13c4de","timestamp":"2026-06-14T21:27:17.209Z","parentId":"c113f81d-6f12-4080-bd13-8613526543dc"}"#,
                "toolu_bdrk_015JoceQkzNKnLkeCj5NaLzT",
            ),
            (
                r#"{"type":"permission.completed","data":{"requestId":"b310d4e0-1e0e-4600-955d-871b5a951e13","toolCallId":"call_-Dbzv4Ev_Get6cFh","result":{"kind":"cancelled","reason":"Session aborted"}},"id":"f36f04ef-3c73-4b7e-88a0-f00c036e2185","timestamp":"2026-10-18T08:09:01.252Z","parentId":"571e3185-d040-4403-b319-738711e0bb62"}"#,
                "call_-Dbzv4Ev_Get6cFh",
            ),
        ] {
            match &decode(refused)[..] {
                [AgentEvent::ToolUseEnd { tool_use_id, .. }] => {
                    assert_eq!(tool_use_id.as_deref(), Some(call), "{refused}");
                }
                other => panic!("a refusal must end the gated call, got {other:?}"),
            }
        }
    }

    #[test]
    fn real_session_shutdown_ends_the_root() {
        let line = r#"{"type":"session.shutdown","data":{"shutdownType":"routine","totalPremiumRequests":1},"id":"220c4131","timestamp":"2026-05-22T06:17:01.077Z","parentId":"cd21bd01"}"#;
        match &decode(line)[..] {
            [AgentEvent::SessionEnd { agent_id, as_child }] => {
                assert_eq!(*agent_id, root());
                assert!(!*as_child, "a root shutdown is NOT a child end");
            }
            other => panic!("expected root SessionEnd, got {other:?}"),
        }
    }

    #[test]
    fn a_shutdown_sums_each_models_fresh_usage_into_one_reading() {
        let line = r#"{"type":"session.shutdown","data":{"shutdownType":"routine","modelMetrics":{"gpt-4.1":{"usage":{"inputTokens":1000,"outputTokens":77,"cacheReadTokens":300,"cacheWriteTokens":222}},"gpt-5-mini":{"usage":{"inputTokens":50,"outputTokens":5,"cacheReadTokens":0,"cacheWriteTokens":0}}}},"id":"e1","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::SessionEnd { .. },
                AgentEvent::Usage {
                    agent_id,
                    fresh_tokens,
                },
            ] => {
                assert_eq!(*agent_id, root());
                assert_eq!(
                    *fresh_tokens,
                    (1000 - 300 + 77) + (50 + 5),
                    "input less cache reads, plus output, over every model"
                );
            }
            other => panic!("expected [SessionEnd, Usage], got {other:?}"),
        }
    }

    #[test]
    fn a_shutdown_with_no_fresh_usage_reads_nothing() {
        // A reading of zero would be pushed into a monotone accumulator.
        for data in [
            r#"{"modelMetrics":{}}"#,
            r#"{"modelMetrics":{"m":{"usage":{"inputTokens":900,"outputTokens":0,"cacheReadTokens":900,"cacheWriteTokens":0}}}}"#,
            r#"{"modelMetrics":{"m":{"requests":{"count":1}}}}"#,
        ] {
            let line = format!(
                r#"{{"type":"session.shutdown","data":{data},"id":"e2","timestamp":"t","parentId":null}}"#
            );
            assert!(
                !decode(&line)
                    .iter()
                    .any(|e| matches!(e, AgentEvent::Usage { .. })),
                "{data}"
            );
        }
    }

    #[test]
    fn ephemeral_unknown_and_malformed_lines_are_ignored_not_panicked() {
        assert!(
            decode(r#"{"type":"session.idle","data":{},"id":"i","timestamp":"t","parentId":null}"#)
                .is_empty()
        );
        assert!(decode(r#"{"type":"assistant.message_delta","data":{},"id":"d","timestamp":"t","parentId":null}"#).is_empty());
        assert!(
            decode(r#"{"type":"tool.execution_start","id":"n","timestamp":"t","parentId":null}"#)
                .is_empty()
        );
        assert!(decode_copilot_line(PATH, SOURCE_NAME, json!("not an object")).is_empty());
        assert!(decode_copilot_line(PATH, SOURCE_NAME, json!(["array"])).is_empty());
    }

    #[test]
    fn no_event_namespace_breadcrumbs_however_new() {
        for kind in [
            "telepathy.transmit",
            "assistant.message_delta",
            "mcp.tools.list_changed",
            "hook.progress",
            "session.idle",
            "user.message",
            "abort",
            "factory.run_updated",
        ] {
            let line = format!(
                r#"{{"type":"{kind}","data":{{}},"id":"i","timestamp":"t","parentId":null}}"#
            );
            let quiet = crate::test_capture::capture_logs(|| {
                assert!(decode(&line).is_empty());
            });
            assert!(
                !quiet.contains(crate::source::drift::TARGET),
                "event {kind:?} carries nothing we read, so it must stay out of the drift log:\n{quiet}"
            );
        }
    }

    #[test]
    fn session_start_without_session_id_registers_root_with_empty_id() {
        let line = r#"{"type":"session.start","data":{"version":1},"id":"x","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::SessionStart {
                    agent_id,
                    source,
                    session_id,
                    cwd,
                    parent_id,
                },
            ] => {
                assert_eq!(*agent_id, root());
                assert_eq!(source, "copilot");
                assert_eq!(session_id, "", "missing sessionId → empty fallback");
                assert_eq!(cwd, Path::new(""), "no context.cwd → empty path");
                assert_eq!(*parent_id, None);
            }
            other => panic!("expected one SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn tool_execution_start_with_data_but_no_tool_call_id_is_ignored() {
        let line = r#"{"type":"tool.execution_start","data":{"toolName":"view"},"id":"x","timestamp":"t","parentId":null}"#;
        assert!(
            decode(line).is_empty(),
            "no toolCallId → no ToolUseStart (can't key the tool)"
        );
    }

    #[test]
    fn tool_execution_complete_with_data_but_no_tool_call_id_is_ignored() {
        let line = r#"{"type":"tool.execution_complete","data":{"success":true},"id":"x","timestamp":"t","parentId":null}"#;
        assert!(
            decode(line).is_empty(),
            "no toolCallId → no ToolUseEnd (can't key the tool)"
        );
    }

    #[test]
    fn tool_execution_start_without_tool_name_still_emits_tool_use_start_keyed_on_call_id() {
        let line = r#"{"type":"tool.execution_start","data":{"toolCallId":"tc1","arguments":{}},"id":"x","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id,
                    detail: Some(d),
                },
            ] => {
                assert_eq!(*agent_id, root());
                assert_eq!(tool_use_id.as_deref(), Some("tc1"));
                assert!(!d.is_task(), "an empty tool name is NOT the task dispatch");
            }
            other => panic!("expected one ToolUseStart, got {other:?}"),
        }
    }

    #[test]
    fn session_task_complete_ends_the_roots_turn() {
        let line = r#"{"type":"session.task_complete","data":{},"id":"x","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [AgentEvent::TurnEnd { agent_id, .. }] => assert_eq!(*agent_id, root()),
            other => panic!("expected one root TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn a_subagents_task_complete_ends_no_activity() {
        let line = serde_json::json!({
            "type": "session.task_complete",
            "data": {"summary": "done", "success": true},
            "agentId": "call_X",
            "parentId": "p"
        })
        .to_string();
        let evs = decode(&line);
        assert!(evs.is_empty(), "got {evs:?}");
    }

    #[test]
    fn subagent_started_without_any_child_key_is_ignored() {
        let line = r#"{"type":"subagent.started","data":{"agentDisplayName":"X"},"id":"x","timestamp":"t","parentId":null}"#;
        assert!(
            decode(line).is_empty(),
            "un-keyable child → no SessionStart/Rename"
        );
        let empty_aid = r#"{"type":"subagent.started","data":{"agentDisplayName":"X"},"id":"x","timestamp":"t","parentId":null,"agentId":""}"#;
        assert!(
            decode(empty_aid).is_empty(),
            "an empty agentId is not a usable key"
        );
    }

    #[test]
    fn subagent_completed_without_any_child_key_is_ignored() {
        let completed = r#"{"type":"subagent.completed","data":{"agentName":"rom"},"id":"x","timestamp":"t","parentId":null}"#;
        assert!(
            decode(completed).is_empty(),
            "un-keyable completed child → no SessionEnd"
        );
        let failed = r#"{"type":"subagent.failed","data":{"agentName":"rom"},"id":"x","timestamp":"t","parentId":null}"#;
        assert!(
            decode(failed).is_empty(),
            "the failed arm shares the same keying gate"
        );
    }

    #[cfg(feature = "native")]
    #[test]
    fn copilot_home_honors_non_empty_env_override() {
        temp_env::with_var("COPILOT_HOME", Some("/custom/cp"), || {
            assert_eq!(
                copilot_home(),
                PathBuf::from("/custom/cp"),
                "a non-empty COPILOT_HOME is used verbatim"
            );
        });

        temp_env::with_var("COPILOT_HOME", Some(""), || {
            assert!(
                copilot_home().ends_with(".copilot"),
                "empty COPILOT_HOME → ~/.copilot fallback"
            );
        });

        temp_env::with_var("COPILOT_HOME", Some("   "), || {
            assert!(
                copilot_home().ends_with(".copilot"),
                "whitespace-only COPILOT_HOME → ~/.copilot fallback"
            );
        });

        temp_env::with_var_unset("COPILOT_HOME", || {
            assert!(
                copilot_home().ends_with(".copilot"),
                "unset COPILOT_HOME → ~/.copilot fallback"
            );
        });
    }
}
