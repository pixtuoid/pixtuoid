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
//! - **Sub-agents INTERLEAVE in the root file**, distinguished by the top-level
//!   envelope `agentId` (== the spawning `task` tool's `data.toolCallId`); there
//!   is no per-agent file split. A line with `agentId` set belongs to that child.
//! - `subagent.completed` is **minimal** on disk (`toolCallId`/`agentName`/
//!   `agentDisplayName` only) — never require model/token/duration fields.
//! - The `ephemeral` envelope flag is inconsistent across CLI versions — never
//!   rely on it; map by `type` and ignore the rest.

use std::path::{Path, PathBuf};

use crate::source::decoder::DecodeResult as Result;
use serde_json::Value;

use crate::AgentId;
use crate::source::decoder::{MAX_DECODED_FIELD_CHARS, ellipsize};
#[cfg(any(test, feature = "upstream"))]
use crate::source::upstream::{At, Doc, Finding, Read, Release, Repo, Watch, When};
use crate::source::{AgentEvent, ToolDetail};

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::CopilotSource;

/// The GitHub Copilot CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "copilot";

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

/// The `kind` values this decoder turns into events. Pinned to the arms by
/// `the_decoded_kind_set_is_exactly_what_the_arms_match`.
///
/// Off shipped builds: only the drift watch and tests read it. The ARMS are what
/// production dispatches on, and a second copy of the vocabulary must not be
/// something the shipped crate can read and drift against.
#[cfg(any(test, feature = "upstream"))]
pub(crate) const DECODED_KINDS: &[&str] = &[
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
];

/// The payload fields `decode_copilot_line` / `extract_copilot_cwd` read.
///
/// `type` is excluded: it is the discriminator, watched as `DECODED_KINDS`.
/// Pinned to the decoder's ACTUAL reads by
/// `the_field_set_is_exactly_what_the_decoder_reads`, because a field the
/// decoder starts reading without being added here is a dependency nothing
/// watches — which is how `tokenCount` and `tokenDetails` came to be read and
/// unwatched.
///
/// Off shipped builds: only the drift watch and tests read it.
#[cfg(any(test, feature = "upstream"))]
pub(crate) const DECODED_FIELDS: &[&str] = &[
    "agentDisplayName",
    "agentId",
    "arguments",
    "context",
    "cwd",
    "data",
    "kind",
    "model",
    "permissionRequest",
    "result",
    "sessionId",
    "tokenCount",
    "tokenDetails",
    "toolCallId",
    "toolName",
];

#[cfg(any(test, feature = "upstream"))]
pub(crate) const REPO: Repo = Repo {
    name: "github/copilot-cli",
    release: Release::GithubLatest,
};

/// Since 1.0.85 the npm packages carry no schema; the release's platform
/// tarball does, which is where github/copilot-sdk's codegen reads it
/// (`java/scripts/codegen/fetch-schemas.mjs`).
#[cfg(any(test, feature = "upstream"))]
const SESSION_EVENTS_SCHEMA: &[Doc] = &[Doc {
    at: At::ReleaseAsset {
        repo: &REPO,
        asset: "github-copilot-{version}-linux-x64.tgz",
        member: "package/schemas/session-events.schema.json",
        sums: "SHA256SUMS.txt",
    },
    anchor: r#""SessionEvent"\s*:"#,
    owns: "the `SessionEvent` union",
}];

#[cfg(any(test, feature = "upstream"))]
pub(crate) const UPSTREAM: &[Watch] = &[
    Watch {
        id: "copilot.kinds",
        ours: || DECODED_KINDS.to_vec(),
        docs: SESSION_EVENTS_SCHEMA,
        read: Read::JsonUnion {
            def: "SessionEvent",
            prop: Some("type"),
        },
        when: When::Vanishes,
        finding: Finding::Breaking(
            "the transcript still carries it, but the decoder maps it to nothing \
             (no sprite, no activity)",
        ),
        // `session.*` is a product-wide bus a sibling ledger would just mirror.
        siblings: None,
    },
    Watch {
        id: "copilot.payload_fields",
        ours: || DECODED_FIELDS.to_vec(),
        docs: SESSION_EVENTS_SCHEMA,
        read: Read::JsonKeys,
        when: When::Vanishes,
        finding: Finding::Breaking(
            "the decoder reads None: a wrong register, no parent link, no tool \
             label, or a permission that never gates",
        ),
        siblings: None,
    },
];

/// Decode one `events.jsonl` line into zero or more `AgentEvent`s. Unknown,
/// ephemeral, or malformed shapes return `vec![]` and never panic — real files
/// carry embedded-newline / U+2028 corruption (upstream copilot-cli #2649/#2012).
///
/// # Errors
///
/// Never: the `Result` is the [`LineDecoder`](crate::source::decoder::LineDecoder) signature, and a malformed line decodes to `vec![]`.
pub fn decode_copilot_line(
    transcript_path: &str,
    source: &str,
    v: Value,
) -> Result<Vec<AgentEvent>> {
    let root = AgentId::from_parts(source, &copilot_id_from_path(Path::new(transcript_path)));
    let Some(obj) = v.as_object() else {
        return Ok(vec![]);
    };
    let kind = obj.get("type").and_then(|s| s.as_str()).unwrap_or("");
    let data = obj.get("data");

    let acting = match str_at(&v, "agentId") {
        Some(aid) if !aid.is_empty() => AgentId::from_parts(source, aid),
        _ => root,
    };

    let out = match kind {
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
                return Ok(vec![]);
            };
            let Some(tool_call_id) = str_at(d, "toolCallId") else {
                crate::source::drift::missing_field(source, "tool.execution_start", "toolCallId");
                return Ok(vec![]);
            };
            let tool_name = str_at(d, "toolName").unwrap_or_else(|| {
                crate::source::drift::missing_field(source, "tool.execution_start", "toolName");
                ""
            });
            let detail = copilot_tool_detail(tool_name, d.get("arguments"));
            vec![AgentEvent::ActivityStart {
                agent_id: acting,
                tool_use_id: Some(tool_call_id.to_string()),
                detail: Some(detail),
            }]
        }
        TOOL_EXECUTION_COMPLETE => {
            let Some(d) = data else {
                crate::source::drift::missing_field(source, "tool.execution_complete", "data");
                return Ok(vec![]);
            };
            let Some(tool_call_id) = str_at(d, "toolCallId") else {
                crate::source::drift::missing_field(
                    source,
                    "tool.execution_complete",
                    "toolCallId",
                );
                return Ok(vec![]);
            };
            let mut out = vec![AgentEvent::ActivityEnd {
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
            let reason = data
                .and_then(|d| d.get("permissionRequest"))
                .and_then(|p| str_at(p, "kind"))
                // Capped at the decode boundary: `kind` is raw wire content that
                // persists in the slot + egresses on the headless summary.
                .map(|k| ellipsize(&format!("permission: {k}"), MAX_DECODED_FIELD_CHARS))
                .unwrap_or_else(|| "permission".to_string());
            vec![AgentEvent::Waiting {
                agent_id: acting,
                reason,
                tool_use_id: None,
            }]
        }
        // On APPROVED the gated tool's own `tool.execution_start` follows and
        // clears the Waiting gate, so emit nothing (a detail-less ActivityStart
        // here would only inflate tool_call_count). On a DENIAL/cancel no tool
        // runs, so emit the clearing ActivityStart ourselves.
        PERMISSION_COMPLETED => {
            let approved = data
                .and_then(|d| d.get("result"))
                .and_then(|r| str_at(r, "kind"))
                .is_some_and(|k| k.starts_with("approved"));
            if approved {
                vec![]
            } else {
                vec![AgentEvent::ActivityStart {
                    agent_id: acting,
                    tool_use_id: None,
                    detail: None,
                }]
            }
        }
        SUBAGENT_STARTED => {
            let Some(child_key) = copilot_child_key(&v, data) else {
                return Ok(vec![]);
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
                return Ok(vec![]);
            };
            vec![AgentEvent::SessionEnd {
                agent_id: AgentId::from_parts(source, child_key),
                as_child: true,
            }]
        }
        SESSION_TASK_COMPLETE => vec![AgentEvent::ActivityEnd {
            agent_id: root,
            tool_use_id: None,
        }],
        // Copilot's ONLY usage wire is this shutdown summary — one final delta
        // as the session ends. `tokenDetails.input` already EXCLUDES cache
        // reads, so fresh = input + cache_write + output. `cache_write` is an
        // INFERRED snake_case key (from the sibling `cache_read`): no fixture
        // carries a nonzero bucket, and a differently-spelled key only
        // UNDERcounts.
        SESSION_SHUTDOWN => {
            let mut evs = vec![AgentEvent::SessionEnd {
                agent_id: root,
                as_child: false,
            }];
            if let Some(details) = data
                .and_then(|d| d.get("tokenDetails"))
                .and_then(|t| t.as_object())
            {
                let bucket = |k: &str| {
                    details
                        .get(k)
                        .and_then(|b| b.get("tokenCount"))
                        .and_then(|n| n.as_u64())
                        .unwrap_or(0)
                };
                let fresh = bucket("input")
                    .saturating_add(bucket("cache_write"))
                    .saturating_add(bucket("output"));
                if fresh > 0 {
                    evs.push(AgentEvent::Usage {
                        agent_id: root,
                        fresh_tokens: fresh,
                    });
                }
            }
            evs
        }
        _ => vec![],
    };
    Ok(out)
}

/// Copilot's tool-detail dispatch. The sub-agent dispatch is the `task` tool,
/// detected by NAME — routing non-`task` tools through the shared
/// [`crate::source::decoder::make_tool_detail`] (which flags a Task on the mere
/// PRESENCE of a `subagent_type` input key) would let a model-authored /
/// hallucinated `subagent_type` arg spoof an ordinary tool into a delegation,
/// prematurely draining `active_tasks` and evicting real children.
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

    /// The exported set IS the arms. The arms dispatch on the SAME consts, so a
    /// value cannot drift; this catches a member dropped from the set, or kept
    /// after its arm went away. The discriminator is REACHING an arm, not
    /// emitting an event — several arms legitimately decode a well-formed line
    /// to nothing — so each kind is fed an EMPTY `data`, where every real arm
    /// breadcrumbs a missing field and the catch-all stays silent.
    #[test]
    fn the_decoded_kind_set_is_exactly_what_the_arms_match() {
        let path = "/p/11111111-2222-3333-4444-555555555555/events.jsonl";
        let drive = |kind: &str| {
            let line = serde_json::json!({
                "type": kind, "data": {"toolCallId": "t1"}, "id": "e1",
                "timestamp": "ts", "parentId": null,
            });
            let mut evs = 0;
            let logs = crate::test_capture::capture_logs(|| {
                evs = decode_copilot_line(path, "copilot", line).map_or(0, |e| e.len());
            });
            evs > 0 || logs.contains(crate::source::drift::TARGET)
        };
        for kind in DECODED_KINDS {
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
        )
        .unwrap();
        assert!(
            evs.iter().any(|e| matches!(e, AgentEvent::ModelInfo { model: Some(m), effort: None, .. } if m == "claude-haiku-4.5")),
            "per-tool model must surface, got {evs:?}"
        );
        let line = r#"{"type":"tool.execution_complete","data":{"toolCallId":"t2","success":true},"id":"e2","timestamp":"ts","parentId":null}"#;
        let evs = decode_copilot_line(
            "/p/11111111-2222-3333-4444-555555555555/events.jsonl",
            "copilot",
            serde_json::from_str(line).unwrap(),
        )
        .unwrap();
        assert!(
            !evs.iter()
                .any(|e| matches!(e, AgentEvent::ModelInfo { .. })),
            "got {evs:?}"
        );
    }
    use serde_json::json;

    #[test]
    fn a_shutdown_with_no_fresh_tokens_emits_no_usage_reading() {
        // `tokenDetails` present but every FRESH bucket zero (a cache-read-only
        // turn): a `Usage { fresh_tokens: 0 }` would be a reading of nothing,
        // pushed into a monotone accumulator.
        let line = r#"{"type":"session.shutdown","data":{"tokenDetails":{"input":{"tokenCount":0},"cache_read":{"tokenCount":900}}},"id":"e9","timestamp":"ts","parentId":null}"#;
        let evs = decode_copilot_line(
            "/p/11111111-2222-3333-4444-555555555555/events.jsonl",
            "copilot",
            serde_json::from_str(line).unwrap(),
        )
        .unwrap();
        assert!(
            !evs.iter().any(|e| matches!(e, AgentEvent::Usage { .. })),
            "zero fresh tokens must not surface a Usage, got {evs:?}"
        );
        assert!(
            evs.iter()
                .any(|e| matches!(e, AgentEvent::SessionEnd { .. }))
        );
    }

    const PATH: &str = "/p/session-state/65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3/events.jsonl";

    fn root() -> AgentId {
        AgentId::from_parts(SOURCE_NAME, "65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3")
    }
    fn decode(line: &str) -> Vec<AgentEvent> {
        decode_copilot_line(PATH, SOURCE_NAME, serde_json::from_str(line).unwrap()).unwrap()
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
            [AgentEvent::ActivityStart { agent_id, .. }] => {
                assert_eq!(*agent_id, root(), "empty agentId must key to the root");
            }
            other => panic!("expected one ActivityStart, got {other:?}"),
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
    fn real_session_start_registers_root_with_cwd_and_session_id() {
        let line = r#"{"type":"session.start","data":{"sessionId":"65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3","version":1,"producer":"copilot-agent","copilotVersion":"unknown","startTime":"2026-05-22T05:59:45.408Z","selectedModel":"claude-haiku-4.5","context":{"cwd":"d:\\contentforge-fullstack (1)"},"alreadyInUse":false},"id":"0bc5f1ba-1abe-49c9-a303-d843bd0c3fa8","timestamp":"2026-05-22T05:59:45.488Z","parentId":null}"#;
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
                assert_eq!(session_id, "65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3");
                assert_eq!(cwd, Path::new(r"d:\contentforge-fullstack (1)"));
                assert_eq!(*parent_id, None);
            }
            other => panic!("expected one SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn real_tool_round_is_active_then_idle_keyed_on_tool_call_id() {
        let start = r#"{"type":"tool.execution_start","data":{"toolCallId":"tooluse_9CoqZL2lZlJUsz7TjJsSUk","toolName":"report_intent","arguments":{"intent":"Exploring project setup"}},"id":"595a6493-1763-4c80-b75a-936d4f263a11","timestamp":"2026-05-22T06:00:14.298Z","parentId":"2902a578-0304-4abc-8402-afefefff9e70"}"#;
        match &decode(start)[..] {
            [
                AgentEvent::ActivityStart {
                    agent_id,
                    tool_use_id,
                    detail: Some(_),
                },
            ] => {
                assert_eq!(*agent_id, root());
                assert_eq!(
                    tool_use_id.as_deref(),
                    Some("tooluse_9CoqZL2lZlJUsz7TjJsSUk")
                );
            }
            other => panic!("expected ActivityStart, got {other:?}"),
        }
        let complete = r#"{"type":"tool.execution_complete","data":{"toolCallId":"tooluse_9CoqZL2lZlJUsz7TjJsSUk","model":"claude-haiku-4.5","interactionId":"65f25156-0095-4746-ac3e-fa52340df72b","success":true,"result":{"content":"Intent logged","detailedContent":"Exploring project setup"},"toolTelemetry":{}},"id":"cd7e82e8","timestamp":"2026-05-22T06:00:14.323Z","parentId":"d97de833"}"#;
        match &decode(complete)[..] {
            [
                AgentEvent::ActivityEnd {
                    agent_id,
                    tool_use_id,
                },
                ..,
            ] => {
                assert_eq!(*agent_id, root());
                assert_eq!(
                    tool_use_id.as_deref(),
                    Some("tooluse_9CoqZL2lZlJUsz7TjJsSUk")
                );
            }
            other => panic!("expected ActivityEnd first, got {other:?}"),
        }
    }

    #[test]
    fn real_task_tool_is_delegating() {
        let line = r#"{"type":"tool.execution_start","data":{"toolCallId":"call_SGMJ1yjMtpgFUbZct2fEo2Hk","toolName":"task","arguments":{"description":"Incident command response","agent_type":"sisko","name":"sisko-incident-command","mode":"sync"},"turnId":"0"},"id":"a","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ActivityStart {
                    detail: Some(d), ..
                },
            ] => assert!(d.is_task(), "task tool must be Delegating, got {d:?}"),
            other => panic!("expected Delegating ActivityStart, got {other:?}"),
        }
    }

    #[test]
    fn spoofed_subagent_type_arg_does_not_make_a_task() {
        let line = r#"{"type":"tool.execution_start","data":{"toolCallId":"c1","toolName":"view","arguments":{"path":"x.rs","subagent_type":null}},"id":"a","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ActivityStart {
                    detail: Some(d), ..
                },
            ] => assert!(
                !d.is_task(),
                "a spoofed subagent_type arg must stay Generic, got {d:?}"
            ),
            other => panic!("expected Generic ActivityStart, got {other:?}"),
        }
    }

    #[test]
    fn ordinary_tool_shows_its_own_arg_target() {
        let line = r#"{"type":"tool.execution_start","data":{"toolCallId":"c2","toolName":"bash","arguments":{"command":"cargo test"}},"id":"a","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ActivityStart {
                    detail: Some(ToolDetail::Generic { display }),
                    ..
                },
            ] => assert!(
                display.contains("cargo test"),
                "bash tool should show its command target, got {display:?}"
            ),
            other => panic!("expected Generic ActivityStart with target, got {other:?}"),
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
            // `data.toolCallId` is the documented key for a spawn — the
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
            [AgentEvent::ActivityStart { agent_id, .. }] => assert_eq!(
                *agent_id,
                AgentId::from_parts(SOURCE_NAME, "call_SGMJ1yjMtpgFUbZct2fEo2Hk"),
                "a line with envelope agentId must attribute to the CHILD, not root"
            ),
            other => panic!("expected child ActivityStart, got {other:?}"),
        }
    }

    #[test]
    fn permission_requested_reason_is_capped_at_the_decode_boundary() {
        use crate::source::decoder::MAX_DECODED_FIELD_CHARS;
        let kind = "x".repeat(MAX_DECODED_FIELD_CHARS * 4);
        let line = format!(
            r#"{{"type":"permission.requested","data":{{"permissionRequest":{{"kind":"{kind}"}}}},"id":"a","timestamp":"t","parentId":null}}"#
        );
        match &decode(&line)[..] {
            [AgentEvent::Waiting { reason, .. }] => {
                assert_eq!(reason.chars().count(), MAX_DECODED_FIELD_CHARS + 1);
            }
            other => panic!("expected Waiting, got {other:?}"),
        }
    }

    #[test]
    fn permission_requested_waits_and_completed_clears() {
        let req = r#"{"type":"permission.requested","data":{"requestId":"8c508e21-0a6c-4a06-8824-3930476499ea","permissionRequest":{"kind":"shell","toolCallId":"call_K8WLZkwufHsI9bTvkZmMKec2","fullCommandText":"cat /etc/hostname","intention":"Print /etc/hostname contents","commands":[{"identifier":"cat","readOnly":true}],"possiblePaths":["/etc/hostname"],"possibleUrls":[],"hasWriteFileRedirection":false,"canOfferSessionApproval":true},"promptRequest":{"kind":"path","accessKind":"shell","paths":["/etc/hostname"],"toolCallId":"call_K8WLZkwufHsI9bTvkZmMKec2"}},"id":"1f975691-a108-4d6f-924b-d48263d46274","timestamp":"2026-06-14T21:35:55.637Z","parentId":"e0a534c6-d548-4def-b0bd-316c83efe5fd"}"#;
        match &decode(req)[..] {
            [
                AgentEvent::Waiting {
                    agent_id, reason, ..
                },
            ] => {
                assert_eq!(*agent_id, root());
                assert!(reason.contains("shell"), "reason names the gate: {reason}");
            }
            other => panic!("expected Waiting, got {other:?}"),
        }
        let approved = r#"{"type":"permission.completed","data":{"requestId":"8c508e21-0a6c-4a06-8824-3930476499ea","toolCallId":"call_K8WLZkwufHsI9bTvkZmMKec2","result":{"kind":"approved"}},"id":"8123a44a-3471-4262-9191-b3cddaf5224d","timestamp":"2026-06-14T21:35:58.218Z","parentId":"1f975691-a108-4d6f-924b-d48263d46274"}"#;
        assert!(
            decode(approved).is_empty(),
            "approved → no event (tool start clears the gate)"
        );

        // Two REAL deny variants: the interactive user reject and the
        // non-interactive no-rule auto-deny.
        for denied in [
            r#"{"type":"permission.completed","data":{"requestId":"954afe31-559a-4afc-9eb6-13e30cf48aea","toolCallId":"call_nf1RvU9GxssNg2g7WtPgHqQ4","result":{"kind":"denied-interactively-by-user"}},"id":"60dae716-c76c-45e2-84e1-c3248ce3790c","timestamp":"2026-06-14T21:38:43.086Z","parentId":"5240af45-3ad2-4bf7-bc37-83c329c9c2ea"}"#,
            r#"{"type":"permission.completed","data":{"requestId":"eab9bd2c-ca42-4ab6-8567-1c11906500a6","toolCallId":"toolu_bdrk_015JoceQkzNKnLkeCj5NaLzT","result":{"kind":"denied-no-approval-rule-and-could-not-request-from-user"}},"id":"2cc6bffe-6443-4d8b-9765-dbfeda13c4de","timestamp":"2026-06-14T21:27:17.209Z","parentId":"c113f81d-6f12-4080-bd13-8613526543dc"}"#,
        ] {
            assert!(
                matches!(&decode(denied)[..], [AgentEvent::ActivityStart { .. }]),
                "a non-approved result must clear Waiting: {denied}"
            );
        }
    }

    #[test]
    fn real_session_shutdown_ends_the_root() {
        // A tokenDetails-less shutdown (older wire / crashy teardown).
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
    fn shutdown_usage_sums_a_cache_write_bucket_into_fresh() {
        // The only fixture with a nonzero `cache_write` bucket — no captured
        // shutdown carries one (the key is inferred; see the decoder arm).
        let line = r#"{"type":"session.shutdown","data":{"shutdownType":"routine","tokenDetails":{"input":{"tokenCount":11175},"cache_write":{"tokenCount":500},"cache_read":{"tokenCount":1664},"output":{"tokenCount":212}},"currentModel":"gpt-5-mini"},"id":"56992353","timestamp":"2026-06-14T21:38:47.162Z","parentId":"3079df1f"}"#;
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
                    *fresh_tokens, 11_887,
                    "fresh = input 11175 + cache_write 500 + output 212 (cache_read excluded)"
                );
            }
            other => panic!("expected [SessionEnd, Usage], got {other:?}"),
        }
    }

    #[test]
    fn real_session_shutdown_usage_summary_lands_one_final_delta() {
        let line = r#"{"type":"session.shutdown","data":{"shutdownType":"routine","tokenDetails":{"input":{"tokenCount":11175},"cache_read":{"tokenCount":1664},"output":{"tokenCount":212}},"currentModel":"gpt-5-mini"},"id":"56992353","timestamp":"2026-06-14T21:38:47.162Z","parentId":"3079df1f"}"#;
        match &decode(line)[..] {
            [
                AgentEvent::SessionEnd { agent_id, as_child },
                AgentEvent::Usage {
                    agent_id: u_id,
                    fresh_tokens,
                },
            ] => {
                assert_eq!(*u_id, root());
                assert_eq!(
                    *fresh_tokens, 11_387,
                    "fresh = input 11175 + output 212, cache_read excluded"
                );
                assert_eq!(*agent_id, root());
                assert!(!*as_child);
            }
            other => panic!("expected [SessionEnd, Usage], got {other:?}"),
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
        assert!(
            decode_copilot_line(PATH, SOURCE_NAME, json!("not an object"))
                .unwrap()
                .is_empty()
        );
        assert!(
            decode_copilot_line(PATH, SOURCE_NAME, json!(["array"]))
                .unwrap()
                .is_empty()
        );
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
            "no toolCallId → no ActivityStart (can't key the tool)"
        );
    }

    #[test]
    fn tool_execution_complete_with_data_but_no_tool_call_id_is_ignored() {
        let line = r#"{"type":"tool.execution_complete","data":{"success":true},"id":"x","timestamp":"t","parentId":null}"#;
        assert!(
            decode(line).is_empty(),
            "no toolCallId → no ActivityEnd (can't key the tool)"
        );
    }

    #[test]
    fn tool_execution_start_without_tool_name_still_emits_activity_start_keyed_on_call_id() {
        let line = r#"{"type":"tool.execution_start","data":{"toolCallId":"tc1","arguments":{}},"id":"x","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ActivityStart {
                    agent_id,
                    tool_use_id,
                    detail: Some(d),
                },
            ] => {
                assert_eq!(*agent_id, root());
                assert_eq!(tool_use_id.as_deref(), Some("tc1"));
                assert!(!d.is_task(), "an empty tool name is NOT the task dispatch");
            }
            other => panic!("expected one ActivityStart, got {other:?}"),
        }
    }

    #[test]
    fn session_task_complete_ends_root_activity_with_no_tool_id() {
        let line = r#"{"type":"session.task_complete","data":{},"id":"x","timestamp":"t","parentId":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ActivityEnd {
                    agent_id,
                    tool_use_id,
                },
            ] => {
                assert_eq!(*agent_id, root());
                assert!(tool_use_id.is_none(), "the root settle carries no tool id");
            }
            other => panic!("expected one root ActivityEnd, got {other:?}"),
        }
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

    /// Every field the decoder reads by a LITERAL key is declared here — reads
    /// through a variable (the `bucket()` totals: `input`, `cache_write`,
    /// `output`) are outside the scan entirely. A literal read added without a
    /// declaration fails this test; a helper-routed one does not.
    #[test]
    fn the_field_set_is_exactly_what_the_decoder_reads() {
        let src = include_str!("copilot.rs");
        let mut read: Vec<&str> = Vec::new();
        for m in src.match_indices("str_at(") {
            let rest = &src[m.0..];
            if let Some(q) = rest.find('"')
                && let Some(end) = rest[q + 1..].find('"')
            {
                read.push(&rest[q + 1..q + 1 + end]);
            }
        }
        for pat in ["\t.get(\"", ".get(\""] {
            for m in src.match_indices(pat) {
                let rest = &src[m.0 + pat.len()..];
                if let Some(end) = rest.find('"') {
                    read.push(&rest[..end]);
                }
            }
        }
        // `type` is watched as DECODED_KINDS; a `/` marks a JSON pointer; the
        // alphanumeric clause drops the scanner's own non-key artifacts.
        read.retain(|f| {
            *f != "type" && !f.contains('/') && f.chars().all(|c| c.is_ascii_alphanumeric())
        });
        read.sort_unstable();
        read.dedup();
        let mut declared = DECODED_FIELDS.to_vec();
        declared.sort_unstable();
        assert_eq!(read, declared, "decoder reads vs DECODED_FIELDS");
    }
}
