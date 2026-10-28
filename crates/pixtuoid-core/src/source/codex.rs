//! Codex CLI source. Watches the Codex session transcript
//! (`~/.codex/sessions/**/rollout-<ts>-<UUID>.jsonl`) via `JsonlWatcher` for
//! the lifecycle signals the shared hook socket lacks — most importantly the
//! post-approval resume (`function_call_output`).
//!
//! Coalescing: a root's hook `session_id` (a child's: its `agent_id`) ==
//! session_meta.id == filename UUID (verified), so both transports merge onto
//! one sprite.

use std::path::{Path, PathBuf};

use crate::source::decoder::{DecodeError, DecodeResult as Result};
use serde_json::{Map, Value};

use crate::AgentId;
use crate::source::decoder::{
    HookSubject, MAX_DECODED_FIELD_CHARS, accept_all_paths, ellipsize, generic_tool_display,
    hook_identity, hook_session_id, hook_tool_id_key, hook_tool_name, make_tool_detail, non_empty,
};
use crate::source::registry::{
    ChildEndInference, FocusChannel, HookDecoding, HookObject, Hyphen, SourceCaps,
    SourceDescriptor, SourceKind, ToolIdKey, Transcript,
};
use crate::source::{AgentEvent, RequestKeys, Resolution, ToolDetail};
use crate::state::TurnKey;

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::CodexSource;

/// The Codex source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "codex";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    label_prefix: "cx",
    verified_version: "0.164.0",
    version_probe: Some(&["codex", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: Some("CODEX_HOME"),
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: decode_codex_line,
            // The rollout filename's trailing UUID (the stem also has a timestamp).
            id_from_path: codex_id_from_path,
            path_filter: accept_all_paths,
            cwd_extractor: extract_codex_cwd,
            parent_extractor: Some(extract_codex_parent),
        }),
        hook: Some(HookDecoding::Shared {
            tool_id_key: ToolIdKey::ToolUse,
            // Its arms and why: `decode_codex_hook_custom`.
            extend: decode_codex_hook_custom,
        }),
        caps: SourceCaps {
            // Still false after #710 registered Codex's SessionEnd hook: it is
            // teardown-only, so flipping this regresses abrupt exits to a 30-min reap.
            has_exit_signal: false,
            resurrects_on_prompt: true,
            delegations_are_hook_silent: false,
            // `spawn_agent` returns at launch, and a child's SubagentStop ends
            // it (`codex-rs/core/src/hook_runtime.rs:401-437` at rust-v0.163.0).
            child_end_inference: ChildEndInference::None,
            child_hooks_unnamed: false,
            brackets_turns: true,
        },
        focus: FocusChannel::TranscriptProbe,
    },
    #[cfg(feature = "native")]
    native: native::NATIVE,
};

/// Trailing canonical UUID (`8-4-4-4-12`) of a `rollout-<ts>-<UUID>.jsonl`
/// filename. Equals the hook payload's `session_id`, so hook and JSONL events
/// coalesce. Falls back to the full stem if no trailing UUID is present.
pub(crate) fn codex_id_from_path(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    // `.get()` (not `&stem[..]`): this runs on every file under the watched
    // tree, and a byte split landing mid-codepoint would panic.
    let tail = stem.get(stem.len().saturating_sub(36)..).unwrap_or("");
    if is_uuid(tail) {
        tail.to_string()
    } else {
        stem.to_string()
    }
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The subagent dispatch tool, `multi_agent_v1`'s `spawn_agent`.
pub(crate) const DISPATCH_TOOL: &str = "spawn_agent";

/// The `session_meta` field naming a child rollout's parent.
const PARENT_THREAD_ID: &str = "parent_thread_id";

/// The field naming the turn on codex's rollout payloads and hooks.
const TURN_ID: &str = "turn_id";

/// Codex's source-specific hook arms. The hooks carry only what the rollout
/// lacks: a call about to run, the approval prompt, and its answers; the
/// rollout owns the turns and the items. `SubagentStart`/`SubagentStop`
/// change the event's SUBJECT to a child's AgentId; every other session event
/// falls through to the shared arms, a child's under its own id.
pub(crate) fn decode_codex_hook_custom(obj: &HookObject) -> Result<Option<Vec<AgentEvent>>> {
    let event = obj
        .get("hook_event_name")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    // A malformed SubagentStart/Stop is an Err, not Ok(None): an empty
    // `session_id`/`agent_id` would mint a phantom that never coalesces with
    // the real rollout.
    let guards = |obj: &Map<String, Value>| -> Result<(String, Option<String>)> {
        let subject = HookSubject::of(obj);
        let session_id = subject
            .root
            .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "session_id"))?;
        Ok((session_id.to_owned(), subject.child.map(String::from)))
    };
    match event {
        // The subagent owns a SEPARATE rollout (filename UUID == this payload's
        // `agent_id`). Keying the child on `agent_id` coalesces with that
        // rollout and joins the parent's scope tree (cascade / liveness /
        // readiness).
        "SubagentStart" => {
            let (session_id, child) = guards(obj)?;
            let child = child.ok_or_else(|| {
                DecodeError::missing_in(
                    SOURCE_NAME,
                    "SubagentStart",
                    crate::source::decoder::SUBAGENT_ID,
                )
            })?;
            let cwd = obj.get("cwd").and_then(|s| s.as_str()).unwrap_or("").into();
            Ok(Some(vec![AgentEvent::SessionStart {
                agent_id: AgentId::from_parts(SOURCE_NAME, &child),
                source: SOURCE_NAME.to_string(),
                session_id: child,
                cwd,
                parent_id: Some(AgentId::from_parts(SOURCE_NAME, &session_id)),
            }]))
        }
        // End the CHILD promptly, else its rollout lingers to the 30-min
        // stale-sweep. Losing the race against the child's slot creation
        // leaves a harmless no-op plus that same fallback.
        "SubagentStop" => {
            let (_session_id, child) = guards(obj)?;
            let child = child.ok_or_else(|| {
                DecodeError::missing_in(
                    SOURCE_NAME,
                    "SubagentStop",
                    crate::source::decoder::SUBAGENT_ID,
                )
            })?;
            Ok(Some(vec![AgentEvent::SessionEnd {
                agent_id: AgentId::from_parts(SOURCE_NAME, &child),
                as_child: true,
            }]))
        }
        "PreToolUse" | "PostToolUse" | "PermissionRequest" | "Stop" => {
            decode_approval_hook(obj, event).map(Some)
        }
        // A child's other events name it in `agent_id` beside the root's
        // `session_id`; a root's name none (`codex-rs/hooks/src/schema.rs:282-285`,
        // `core/src/hook_runtime.rs:1042-1061` at rust-v0.163.0).
        _ => crate::source::decoder::subagent_fired(obj, SOURCE_NAME, str::to_owned),
    }
}

/// The approval's hooks, which the rollout lacks, on their [`HookSubject`].
/// `PostToolUse` fires only on success and `PreToolUse` only for a tool with a
/// pre-use payload (`codex-rs/core/src/tools/registry.rs` at rust-v0.164.0), so
/// neither is an item: the rollout's call and output are.
fn decode_approval_hook(obj: &HookObject, event: &str) -> Result<Vec<AgentEvent>> {
    let session_id = match HookSubject::of(obj).child {
        Some(child) => child.to_owned(),
        None => hook_session_id(obj, SOURCE_NAME, event)?,
    };
    let agent_id = AgentId::from_parts(SOURCE_NAME, &session_id);
    let turn = turn_of(obj, event);
    let identity = || hook_identity(agent_id, SOURCE_NAME, &session_id, obj);
    let tool_id_key = hook_tool_id_key(crate::source::registry::descriptor_for(SOURCE_NAME));
    let call = || {
        let call = non_empty(obj, tool_id_key).map(str::to_owned);
        if call.is_none() {
            crate::source::drift::missing_field(SOURCE_NAME, event, tool_id_key);
        }
        call
    };
    let tool_name = || hook_tool_name(obj, SOURCE_NAME, event);
    Ok(match event {
        "PreToolUse" => {
            let tool_name = tool_name();
            let pending = call().map(|tool_use_id| AgentEvent::ToolUsePending {
                agent_id,
                tool_use_id,
                tool_name,
                turn,
            });
            [identity()].into_iter().chain(pending).collect()
        }
        "PostToolUse" => {
            let ran = call().map(|c| AgentEvent::UserActionResolved {
                agent_id,
                by: Resolution::Call(c),
            });
            [identity()].into_iter().chain(ran).collect()
        }
        // The rollout never persists an approval (`codex-rs/rollout/src/policy.rs:195-200`
        // at rust-v0.164.0), so this is its only signal. A `write_stdin`
        // prompt names its own call (`core/src/tools/approvals.rs:156-187`),
        // which no `PreToolUse` announces.
        "PermissionRequest" => vec![
            identity(),
            AgentEvent::user_action_request(
                agent_id,
                Some("permission".into()),
                RequestKeys {
                    tool_use_id: obj
                        .get("tool_input")
                        .and_then(Value::as_object)
                        .and_then(|input| non_empty(input, "approval_id"))
                        .map(str::to_owned),
                    tool_name: tool_name(),
                    turn,
                },
            ),
        ],
        // Stop runs only with no follow-up pending, yet a blocking Stop hook
        // continues the turn (`core/src/session/turn.rs:722-759` at
        // rust-v0.164.0): it answers the turn's requests, and ends nothing.
        _ => turn
            .map(|turn| AgentEvent::UserActionResolved {
                agent_id,
                by: Resolution::Turn(turn),
            })
            .into_iter()
            .collect(),
    })
}

/// The turn a hook or rollout payload names; codex stamps every one
/// (`codex-rs/hooks/src/schema.rs` at rust-v0.164.0), so a missing one is
/// drift.
fn turn_of(obj: &Map<String, Value>, event: &str) -> Option<TurnKey> {
    let turn = non_empty(obj, TURN_ID).map(|t| TurnKey::Id(t.into()));
    if turn.is_none() {
        crate::source::drift::missing_field(SOURCE_NAME, event, TURN_ID);
    }
    turn
}

/// `payload.cwd`, on the head `session_meta` and each `turn_context`.
pub(crate) fn extract_codex_cwd(v: &Value) -> Option<PathBuf> {
    v.get("payload")?.get("cwd")?.as_str().map(PathBuf::from)
}

/// A thread-spawned child's rollout names its parent on the head
/// `session_meta` line, beside the cwd where the head scan stops
/// (`codex-rs/protocol/src/protocol.rs:3262` at rust-v0.163.0); its flat path
/// names none.
pub(crate) fn extract_codex_parent(v: &Value) -> Option<String> {
    v.get("payload")?
        .get(PARENT_THREAD_ID)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub(crate) const EVENT_MSG: &str = "event_msg";
pub(crate) const RESPONSE_ITEM: &str = "response_item";
pub(crate) const TURN_CONTEXT: &str = "turn_context";

/// The rollout inners, grouped BY BEHAVIOUR: each group is the matcher (the
/// arms below guard on `contains`).
const EM_TURN_START: &[&str] = &["task_started"];
const EM_TURN_END: &[&str] = &["task_complete", "turn_aborted"];
const EM_TOKENS: &[&str] = &["token_count"];

const RI_TOOL_START: &[&str] = &["function_call", "custom_tool_call"];
const RI_CALL_OUTPUT: &str = "function_call_output";
const RI_CUSTOM_OUTPUT: &str = "custom_tool_call_output";
const RI_CALL_END: &[&str] = &[RI_CALL_OUTPUT, RI_CUSTOM_OUTPUT];
/// The call an output answers. Optional on a `function_call_output`, which
/// codex also writes for a host-delivered output that answers no call
/// (`codex-rs/protocol/src/models.rs:1151-1153` at `rust-v0.163.0`), and
/// required on a `custom_tool_call_output` (`:1195`).
const CALL_ID: &str = "call_id";

/// Decode one rollout line: the turns' brackets and the items, each keyed by
/// the wire's own id.
pub fn decode_codex_line(transcript_path: &str, source: &str, v: Value) -> Vec<AgentEvent> {
    let agent_id = AgentId::from_parts(source, &codex_id_from_path(Path::new(transcript_path)));
    let Some(obj) = v.as_object() else {
        return vec![];
    };
    let outer = obj.get("type").and_then(|s| s.as_str()).unwrap_or("");
    let payload = obj.get("payload").and_then(|p| p.as_object());
    let inner = payload
        .and_then(|p| p.get("type"))
        .and_then(|s| s.as_str())
        .unwrap_or("");

    // The turns ride the rollout, which is complete and in order; their keys
    // are `sub_id` (`codex-rs/core/src/tasks/mod.rs:848-850` at rust-v0.164.0).
    let turn = || payload.and_then(|p| turn_of(p, inner));
    match (outer, inner) {
        (EVENT_MSG, i) if EM_TURN_START.contains(&i) => turn()
            .map(|turn| AgentEvent::TurnStart { agent_id, turn })
            .into_iter()
            .collect(),
        // `custom_tool_call` is the SAME item under codex's custom-tool API and is
        // what a modern session serializes — `a_custom_tool_call_is_a_tool_call`.
        (RESPONSE_ITEM, i) if RI_TOOL_START.contains(&i) => {
            vec![codex_tool_start(agent_id, i, payload)]
        }
        // A call's output ends it, answering a request that holds it: a
        // refused call gets no `PostToolUse`, so this is its only end.
        (RESPONSE_ITEM, i) if RI_CALL_END.contains(&i) => {
            let call = payload.and_then(|p| non_empty(p, CALL_ID));
            if call.is_none() && i == RI_CUSTOM_OUTPUT {
                crate::source::drift::missing_field(SOURCE_NAME, i, CALL_ID);
            }
            call.map(|c| AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id: Some(c.to_owned()),
            })
            .into_iter()
            .collect()
        }
        (EVENT_MSG, i) if EM_TURN_END.contains(&i) => vec![match turn() {
            Some(turn) => AgentEvent::keyed_turn_end(agent_id, turn),
            None => AgentEvent::turn_end(agent_id),
        }],
        // `last_token_usage` is that turn's reading; the cumulative twin
        // `total_token_usage` must NOT be read — the reducer accumulates
        // deltas, so summing a running total would double-count. codex's
        // `input_tokens` INCLUDES the cached share (verified), so fresh input
        // = input − cached, saturating because upstream reporting quirks must
        // not wrap. `output_tokens` already holds `reasoning_output_tokens`
        // (codex-rs/codex-api/src/sse/responses.rs `parses_cache_write_token_usage`).
        (EVENT_MSG, i) if EM_TOKENS.contains(&i) => {
            let last = payload
                .and_then(|p| p.get("info"))
                .and_then(|i| i.get("last_token_usage"))
                .and_then(|u| u.as_object());
            let fresh = last.map_or(0, |u| {
                let field = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                field("input_tokens")
                    .saturating_sub(field("cached_input_tokens"))
                    .saturating_add(field("output_tokens"))
            });
            if fresh > 0 {
                vec![AgentEvent::Usage {
                    agent_id,
                    fresh_tokens: fresh,
                }]
            } else {
                vec![]
            }
        }
        // `turn_context` opens every turn carrying the model and, on reasoning
        // turns only, the effort — both RAW verbatim, last-seen-wins, so a
        // mid-session switch tracks. Absent effort ≠ downgrade: the reducer
        // only refreshes on Some.
        (TURN_CONTEXT, _) => {
            let model = payload
                .and_then(|p| p.get("model"))
                .and_then(|m| m.as_str())
                .filter(|m| !m.is_empty());
            let effort = payload
                .and_then(|p| p.get("effort"))
                .and_then(|e| e.as_str())
                .filter(|e| !e.is_empty());
            if model.is_some() || effort.is_some() {
                vec![AgentEvent::ModelInfo {
                    agent_id,
                    model: model.map(|m| ellipsize(m, MAX_DECODED_FIELD_CHARS)),
                    effort: effort.map(|e| ellipsize(e, MAX_DECODED_FIELD_CHARS)),
                }]
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

/// codex's code-mode tool: a JavaScript cell whose nested calls run inside it,
/// so the cell is the item (`codex-rs/code-mode-protocol/src/lib.rs:54` at
/// rust-v0.164.0).
const CODE_MODE_TOOL: &str = "exec";

/// Where a code-mode cell finds its nested tools, each named by a JavaScript
/// identifier (`codex-rs/code-mode-protocol/src/description.rs:25` at
/// rust-v0.164.0).
const CELL_TOOLS: &str = "tools.";

fn codex_tool_start(
    agent_id: AgentId,
    inner: &str,
    payload: Option<&Map<String, Value>>,
) -> AgentEvent {
    let field = |k: &str| payload.and_then(|p| p.get(k)).and_then(Value::as_str);
    let name = field("name").unwrap_or_else(|| {
        crate::source::drift::missing_field(SOURCE_NAME, "tool call", "name");
        "tool"
    });
    let tool_use_id = payload
        .and_then(|p| non_empty(p, CALL_ID))
        .map(str::to_owned);
    if tool_use_id.is_none() {
        crate::source::drift::missing_field(SOURCE_NAME, inner, CALL_ID);
    }
    AgentEvent::ToolUseStart {
        agent_id,
        tool_use_id,
        detail: Some(match name {
            DISPATCH_TOOL => ToolDetail::Task,
            CODE_MODE_TOOL => generic_tool_display(name, field("input").and_then(cell_tool)),
            _ => make_tool_detail(SOURCE_NAME, name, None),
        }),
    }
}

/// The first nested tool a code-mode cell calls by name, the informative part
/// of its source.
fn cell_tool(source: &str) -> Option<&str> {
    let mut rest = source;
    while let Some((_, after)) = rest.split_once(CELL_TOOLS) {
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .unwrap_or(after.len());
        if let Some(name) = after.get(..end).filter(|n| !n.is_empty()) {
            return Some(name);
        }
        rest = after;
    }
    None
}

/// The Codex home dir, matching codex's own precedence (`codex-rs`
/// `find_codex_home`): `CODEX_HOME` if it's set to an EXISTING directory, else
/// `<user_home>/.codex`. The installer reads Codex's `config.toml` through here
/// too, so the watched sessions root and the plugin state it reads can never
/// disagree.
///
/// Not mirrored: upstream `canonicalize`s the ENV value (the default branch it
/// leaves alone) — the inverse scoping of [`super::grok::grok_home`]'s, and
/// unobservable for the same reason.
pub fn codex_home() -> PathBuf {
    resolve_codex_home(
        crate::platform::path_env("CODEX_HOME"),
        crate::platform::user_home(),
    )
}

/// Pure precedence core, separated so it's unit-testable without env mutation.
/// On a set-but-absent `CODEX_HOME` upstream codex returns a FATAL error; we
/// deliberately fall back to `~/.codex` — benign for a visualizer, since codex
/// itself won't run (and writes no rollouts) when its home dir is missing.
fn resolve_codex_home(codex_home_env: Option<PathBuf>, home: PathBuf) -> PathBuf {
    if let Some(p) = codex_home_env.filter(|p| p.is_dir()) {
        return p;
    }
    home.join(".codex")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;

    #[test]
    fn codex_home_uses_env_when_it_points_at_an_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_codex_home(Some(tmp.path().to_path_buf()), PathBuf::from("/home/u")),
            tmp.path()
        );
    }

    #[test]
    fn codex_home_falls_back_to_dot_codex_when_env_unset_or_missing_dir() {
        let expected = PathBuf::from("/home/u").join(".codex");
        assert_eq!(resolve_codex_home(None, PathBuf::from("/home/u")), expected);
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing");
        assert_eq!(
            resolve_codex_home(Some(missing.clone()), PathBuf::from("/home/u")),
            expected
        );
    }
    use serde_json::json;

    /// Every name in a behaviour group reaches a real arm, and one outside them
    /// reaches none.
    #[test]
    fn each_rollout_inner_reaches_an_arm() {
        let drive = |outer: &str, inner: &str| {
            let v = json!({
                "type": outer,
                "payload": {"type": inner, "turn_id": "t1", "cwd": "/r",
                            "name": "bash", "call_id": "c1", "arguments": "{}",
                            "model": "gpt-5.6-sol", "effort": "xhigh",
                            "info": {"last_token_usage": {"input_tokens": 1}}},
            });
            !decode_codex_line("/p/rollout-2026-07-10-abc.jsonl", "codex", v).is_empty()
        };
        for inner in [EM_TURN_START, EM_TURN_END, EM_TOKENS].concat() {
            assert!(
                drive(EVENT_MSG, inner),
                "event_msg/{inner} must reach an arm"
            );
        }
        for inner in [RI_TOOL_START, RI_CALL_END].concat() {
            assert!(
                drive(RESPONSE_ITEM, inner),
                "response_item/{inner} must reach an arm",
            );
        }
        assert!(
            drive(TURN_CONTEXT, "anything"),
            "turn_context matches on `_`"
        );
        assert!(
            !drive(EVENT_MSG, "turn_paused"),
            "an unread inner reaches none"
        );
        assert!(
            !drive("session_meta", "task_started"),
            "an unread outer reaches none"
        );
    }

    #[test]
    fn turn_context_surfaces_model_and_effort_verbatim() {
        let v = serde_json::json!({
            "type": "turn_context",
            "payload": {"turn_id": "t1", "cwd": "/r", "model": "gpt-5.6-sol", "effort": "xhigh"}
        });
        let evs = decode_codex_line("/p/rollout-2026-07-10-abc.jsonl", "codex", v);
        assert!(
            evs.iter().any(
                |e| matches!(e, AgentEvent::ModelInfo { model: Some(m), effort: Some(f), .. }
                if m == "gpt-5.6-sol" && f == "xhigh")
            ),
            "turn_context must surface model+effort, got {evs:?}"
        );
        let v = serde_json::json!({
            "type": "turn_context",
            "payload": {"turn_id": "t2", "cwd": "/r", "model": "gpt-5.5"}
        });
        let evs = decode_codex_line("/p/rollout-2026-07-10-abc.jsonl", "codex", v);
        assert!(
            evs.iter().any(|e| matches!(e, AgentEvent::ModelInfo { model: Some(m), effort: None, .. } if m == "gpt-5.5")),
            "got {evs:?}"
        );
        let v = serde_json::json!({"type": "turn_context", "payload": {"turn_id": "t3"}});
        assert!(decode_codex_line("/p/rollout-2026-07-10-abc.jsonl", "codex", v).is_empty());
    }

    #[test]
    fn is_uuid_requires_length_dashes_and_hex() {
        assert!(is_uuid("0196fdb2-99d1-7db2-9ded-93a4a0d4a90e"));
        for bad in [
            "",
            "abc",
            "0196fdb299d17db29ded93a4a0d4a90e",
            "0196fdb2-99d1-7db2-9ded-93a4a0d4a90ez",
            "0196fdb2x99d1-7db2-9ded-93a4a0d4a90e",
            "0196fdb2-99d1-7db2-9ded-93a4a0d4a90g",
        ] {
            assert!(!is_uuid(bad), "{bad:?} must not read as a UUID");
        }
    }

    #[test]
    fn subagent_hooks_with_empty_ids_are_err_not_fallthrough() {
        for event in ["SubagentStart", "SubagentStop"] {
            let no_session = json!({"hook_event_name": event, "agent_id": "child"});
            assert!(
                decode_codex_hook_custom(test_obj(&no_session)).is_err(),
                "{event} without session_id must Err (claim-fully), not fall through"
            );
            let empty_child = json!({"hook_event_name": event, "session_id": "s", "agent_id": ""});
            assert!(
                decode_codex_hook_custom(test_obj(&empty_child)).is_err(),
                "{event} with empty agent_id must Err — a phantom child never coalesces"
            );
        }
    }

    #[test]
    fn a_roots_session_events_fall_through_to_shared_arms() {
        for event in ["SessionStart", "UserPromptSubmit", "SessionEnd"] {
            let v = json!({"hook_event_name": event, "session_id": "s"});
            assert!(
                matches!(decode_codex_hook_custom(test_obj(&v)), Ok(None)),
                "{event}"
            );
        }
    }

    const ROOT: &str = "01a14e36-774b-7a43-a766-5159e4eb32f6";
    const CHILD: &str = "01a14e36-77e4-7220-8c8e-4b2d4c31dd41";

    /// Line shapes from a 0.163.0 `codex exec` whose root spawned a child,
    /// sanitized of its paths.
    fn hook(event: &str, child: bool, extra: Value) -> Vec<AgentEvent> {
        let mut v = json!({
            "session_id": ROOT, "turn_id": "01a14e36-7827-7f92-a603-b700dc3aa71c",
            "transcript_path": "/h/.codex/sessions/rollout.jsonl", "cwd": "/repo",
            "hook_event_name": event, "model": "gpt-5.5",
            "permission_mode": "bypassPermissions", "_pixtuoid_source": SOURCE_NAME,
        });
        if child {
            v["agent_id"] = json!(CHILD);
            v["agent_type"] = json!("default");
        }
        for (k, x) in extra.as_object().expect("an object") {
            v[k] = x.clone();
        }
        crate::source::decoder::decode_hook_payload(v).expect("decodes")
    }

    #[test]
    fn a_childs_own_events_land_on_the_child() {
        let child = AgentId::from_parts(SOURCE_NAME, CHILD);
        let root = AgentId::from_parts(SOURCE_NAME, ROOT);
        let tool = json!({"tool_name": "Bash", "tool_input": {"command": "cat NOTE.txt"},
                          "tool_use_id": "call_nNP7xMfvOrkUQWwa", "tool_response": "pong"});
        for event in ["PreToolUse", "PostToolUse"] {
            let evs = hook(event, true, tool.clone());
            assert!(!evs.is_empty(), "{event}");
            assert!(
                evs.iter().all(|e| e.agent_id() == child),
                "{event}: {evs:?}"
            );
            assert!(
                evs.iter()
                    .any(|e| matches!(e, AgentEvent::Identity { session_id, .. }
                    if session_id == CHILD)),
                "{event}: the Identity must name the child's session: {evs:?}"
            );
            let evs = hook(event, false, tool.clone());
            assert!(evs.iter().all(|e| e.agent_id() == root), "{event}: {evs:?}");
        }
    }

    #[test]
    fn a_childs_prompt_starts_it_under_its_root() {
        let evs = hook("UserPromptSubmit", true, json!({"prompt": "CHILD-TASK"}));
        let [
            AgentEvent::SessionStart {
                agent_id,
                parent_id,
                ..
            },
        ] = &evs[..]
        else {
            panic!("one SessionStart: {evs:?}");
        };
        assert_eq!(*agent_id, AgentId::from_parts(SOURCE_NAME, CHILD));
        assert_eq!(*parent_id, Some(AgentId::from_parts(SOURCE_NAME, ROOT)));
        let evs = hook("UserPromptSubmit", false, json!({"prompt": "go"}));
        assert!(
            matches!(
                &evs[..],
                [AgentEvent::SessionStart {
                    parent_id: None,
                    ..
                }]
            ),
            "{evs:?}"
        );
    }

    #[test]
    fn a_spawn_agent_call_is_a_task_on_the_rollout_and_only_pending_on_the_hook() {
        const CALL: &str = "call_Fxnld0qmszdIwSfB";
        let spawn = json!({"tool_name": DISPATCH_TOOL, "tool_use_id": CALL,
                           "tool_input": {"message": "CHILD-TASK"}});
        for child in [false, true] {
            let evs = hook("PreToolUse", child, spawn.clone());
            assert!(
                matches!(&evs[..], [AgentEvent::Identity { .. }, AgentEvent::ToolUsePending {
                    tool_use_id, tool_name, .. }]
                    if tool_use_id == CALL && tool_name.as_deref() == Some(DISPATCH_TOOL)),
                "child={child}: {evs:?}"
            );
        }
        let line = json!({"type": "response_item", "payload": {"type": "function_call",
            "name": DISPATCH_TOOL, "namespace": "multi_agent_v1",
            "arguments": "{\"message\":\"CHILD-TASK\"}", "call_id": CALL}});
        let evs = decode_codex_line("/p/rollout-2026-10-18T01-52-34-x.jsonl", "codex", line);
        assert!(
            matches!(&evs[..], [AgentEvent::ToolUseStart {
                tool_use_id: Some(c), detail: Some(d), .. }] if c == CALL && d.is_task()),
            "{evs:?}"
        );
    }

    #[test]
    fn the_hooks_carry_only_what_the_rollout_lacks() {
        const TURN: &str = "01a14e36-7827-7f92-a603-b700dc3aa71c";
        let turn = Some(TurnKey::Id(TURN.into()));
        let tool = json!({"tool_name": "Bash", "tool_input": {"command": "rm NOTE.txt"},
                          "tool_use_id": "call_P60"});
        let pre = hook("PreToolUse", false, tool.clone());
        assert!(
            matches!(&pre[..], [AgentEvent::Identity { .. }, AgentEvent::ToolUsePending {
                tool_use_id, tool_name, turn: t, .. }]
                if tool_use_id == "call_P60" && tool_name.as_deref() == Some("Bash") && *t == turn),
            "{pre:?}"
        );
        let post = hook("PostToolUse", false, tool);
        assert!(
            matches!(&post[..], [AgentEvent::Identity { .. }, AgentEvent::UserActionResolved {
                by: Resolution::Call(c), .. }] if c == "call_P60"),
            "{post:?}"
        );
        let stop = hook("Stop", false, json!({"stop_hook_active": false}));
        assert!(
            matches!(&stop[..], [AgentEvent::UserActionResolved {
                by: Resolution::Turn(t), .. }] if Some(t) == turn.as_ref()),
            "{stop:?}"
        );
        let asked = hook(
            "PermissionRequest",
            false,
            json!({"tool_name": "Bash", "tool_input": {"command": "rm NOTE.txt"}}),
        );
        assert!(
            matches!(&asked[..], [AgentEvent::Identity { .. }, AgentEvent::UserActionRequest {
                tool_use_id: None, tool_name: Some(n), turn: t, .. }]
                if n == "Bash" && *t == turn),
            "{asked:?}"
        );
        let stdin = hook(
            "PermissionRequest",
            false,
            json!({"tool_name": "write_stdin",
                   "tool_input": {"chars": "y\n", "approval_id": "call_stdin"}}),
        );
        assert!(
            matches!(&stdin[..], [_, AgentEvent::UserActionRequest {
                tool_use_id: Some(c), .. }] if c == "call_stdin"),
            "a write_stdin prompt names its own call: {stdin:?}"
        );
    }

    #[test]
    fn a_hook_missing_its_keys_breadcrumbs_and_decodes_what_it_can() {
        let logs = crate::test_capture::capture_logs(|| {
            let mut v = json!({"hook_event_name": "Stop", "session_id": ROOT,
                               "_pixtuoid_source": SOURCE_NAME});
            assert!(
                crate::source::decoder::decode_hook_payload(v.clone())
                    .expect("decodes")
                    .is_empty()
            );
            v["hook_event_name"] = json!("PreToolUse");
            v["turn_id"] = json!("t");
            assert!(matches!(
                &crate::source::decoder::decode_hook_payload(v).expect("decodes")[..],
                [AgentEvent::Identity { .. }]
            ));
        });
        assert!(
            logs.contains(TURN_ID) && logs.contains("tool_use_id"),
            "{logs}"
        );
    }

    #[test]
    fn session_end_hook_decodes_to_a_clean_session_end() {
        let payload = json!({
            "hook_event_name": "SessionEnd",
            "session_id": "019e7762-9ded-7e33-be41-946ecf105bf4",
            "cwd": "/repo",
            "reason": "other",
            "_pixtuoid_source": SOURCE_NAME,
        });
        assert!(matches!(
            decode_codex_hook_custom(test_obj(&payload)),
            Ok(None)
        ));
        let events = crate::source::decoder::decode_hook_payload(payload).unwrap();
        let expected = AgentId::from_parts(SOURCE_NAME, "019e7762-9ded-7e33-be41-946ecf105bf4");
        assert!(
            matches!(
                events.as_slice(),
                [AgentEvent::SessionEnd { agent_id, as_child: false }] if *agent_id == expected
            ),
            "SessionEnd must decode to exactly one clean SessionEnd for the \
             rollout-coalesced id, got {events:?}"
        );
    }

    fn ev(line: Value) -> Vec<AgentEvent> {
        decode_codex_line(
            "/x/rollout-1-019e7762-9ded-7e33-be41-946ecf105bf4.jsonl",
            SOURCE_NAME,
            line,
        )
    }

    #[test]
    fn task_started_opens_its_turn() {
        let out = ev(json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t"}}));
        assert!(
            matches!(out.as_slice(), [
                AgentEvent::TurnStart { turn: TurnKey::Id(t), .. },
            ] if &**t == "t"),
            "{out:?}"
        );
    }

    /// codex's serde aliases are read-only (`codex-rs/protocol/src/protocol.rs`
    /// at rust-v0.161.0: `rename = "task_started", alias = "turn_started"`), so
    /// a rollout never carries them.
    #[test]
    fn a_turn_alias_codex_never_writes_decodes_to_nothing() {
        for t in ["turn_started", "turn_complete"] {
            let out = ev(json!({"type":"event_msg","payload":{"type":t,"turn_id":"t"}}));
            assert!(out.is_empty(), "{t}");
        }
    }

    /// Captured off codex 2026-08: a real rollout serializes its tool calls as
    /// `custom_tool_call`, never the `function_call` the composed fixture used, so
    /// reading only the latter let four tool calls decode to nothing while the
    /// turn still showed Running from `task_started`.
    #[test]
    fn a_custom_tool_call_is_a_tool_call() {
        let out = ev(json!({"type":"response_item","payload":{
            "type":"custom_tool_call","call_id":"call_x","name":"apply_patch","input":"*** Begin"}}));
        assert!(
            matches!(out.as_slice(), [AgentEvent::ToolUseStart {
                tool_use_id: Some(c), detail: Some(d), .. }]
                if c == "call_x" && d.display() == "apply_patch"),
            "{out:?}"
        );
        let ended = ev(json!({"type":"response_item","payload":{
            "type":"custom_tool_call_output","call_id":"call_x","output":"ok"}}));
        assert!(
            matches!(ended.as_slice(), [
                AgentEvent::ToolUseEnd { tool_use_id: Some(c), .. },
            ] if c == "call_x"),
            "its output must end its call like function_call_output does"
        );
    }

    #[test]
    fn a_calls_output_ends_it() {
        let out = ev(
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":"ok"}}),
        );
        assert!(matches!(out.as_slice(), [
            AgentEvent::ToolUseEnd { tool_use_id: Some(c), .. },
        ] if c == "c"));
        let logs = crate::test_capture::capture_logs(|| {
            let unpaired = ev(
                json!({"type":"response_item","payload":{"type":"function_call_output","output":"ok"}}),
            );
            assert!(unpaired.is_empty(), "{unpaired:?}");
        });
        assert!(
            !logs.contains("missing_field"),
            "an output that answers no call is codex's own shape, got:\n{logs}"
        );
        for call in [json!(null), json!("")] {
            let logs = crate::test_capture::capture_logs(|| {
                let mut line = json!({"type":"response_item","payload":{"type":"custom_tool_call_output","output":"ok"}});
                if !call.is_null() {
                    line["payload"]["call_id"] = call.clone();
                }
                assert!(ev(line).is_empty());
            });
            assert!(
                logs.contains("missing_field") && logs.contains("call_id"),
                "a custom output owes its call, {call}: {logs}"
            );
        }
        let logs = crate::test_capture::capture_logs(|| {
            let keyed = ev(json!({"type":"response_item","payload":{
                "type":"custom_tool_call_output","call_id":"call_x","output":"ok"}}));
            assert!(matches!(keyed.as_slice(), [
                AgentEvent::ToolUseEnd { tool_use_id: Some(c), .. },
            ] if c == "call_x"));
        });
        assert!(
            !logs.contains("missing_field"),
            "a keyed custom output owes nothing: {logs}"
        );
    }

    /// A cell's nested calls run inside it, so its detail is the first tool
    /// it calls: slicing JS for a "first statement" has no answer without a
    /// parser (a cell opening `try {`, a comment, a `;` inside a string).
    #[test]
    fn a_code_mode_cell_shows_the_first_tool_it_calls() {
        for (input, want) in [
            (
                "// @exec: {}\ntry {\n  text(await tools.exec_command({ cmd: \"cd src; ls\" }));\n} catch (e) {}",
                Some("exec_command"),
            ),
            (
                "// read the note\nconst r = await tools.mcp__fs__read_file$2({});",
                Some("mcp__fs__read_file$2"),
            ),
            ("text(\"tools. alone\"); yield_control();", None),
            ("const t = tools[name];", None),
        ] {
            assert_eq!(cell_tool(input), want, "{input:?}");
        }
        for (input, want) in [
            ("text(await tools.apply_patch(p))", Some("apply_patch")),
            ("text(1)", None),
        ] {
            let out = ev(json!({"type":"response_item","payload":{
                "type":"custom_tool_call","call_id":"call_x","name":CODE_MODE_TOOL,"input":input}}));
            assert!(
                matches!(out.as_slice(), [AgentEvent::ToolUseStart { detail: Some(d), .. }]
                    if *d == generic_tool_display(CODE_MODE_TOOL, want)),
                "{input:?}: {out:?}"
            );
        }
    }

    #[test]
    fn the_approval_hooks_read_the_rows_tool_id_key() {
        let key = crate::source::decoder::hook_tool_id_key(
            crate::source::registry::descriptor_for(SOURCE_NAME),
        );
        let evs = hook(
            "PostToolUse",
            false,
            json!({"tool_name": "Bash", key: "call_k"}),
        );
        assert!(
            matches!(&evs[..], [_, AgentEvent::UserActionResolved {
                by: Resolution::Call(c), .. }] if c == "call_k"),
            "{evs:?}"
        );
    }

    /// No name on the wire is no binding key: the pending call still counts
    /// for the sole-pending rule.
    #[test]
    fn a_pre_use_naming_no_tool_binds_by_no_name() {
        let evs = hook("PreToolUse", false, json!({"tool_use_id": "c"}));
        assert!(
            matches!(
                &evs[..],
                [
                    _,
                    AgentEvent::ToolUsePending {
                        tool_name: None,
                        ..
                    }
                ]
            ),
            "{evs:?}"
        );
    }

    #[test]
    fn a_function_call_is_its_calls_start() {
        let out = ev(
            json!({"type":"response_item","payload":{"type":"function_call",
            "name":"exec_command","arguments":r#"{"cmd":"ls"}"#,"call_id":"call_1"}}),
        );
        assert!(
            matches!(out.as_slice(), [AgentEvent::ToolUseStart { tool_use_id: Some(c), .. }]
                if c == "call_1"),
            "{out:?}"
        );
    }

    #[test]
    fn task_complete_and_abort_end_their_turn() {
        for t in ["task_complete", "turn_aborted"] {
            let out = ev(json!({"type":"event_msg","payload":{"type":t,"turn_id":"t"}}));
            assert!(
                matches!(out.as_slice(), [AgentEvent::TurnEnd { turn: Some(TurnKey::Id(k)), .. }]
                    if &**k == "t"),
                "{t}: {out:?}"
            );
        }
        let logs = crate::test_capture::capture_logs(|| {
            let out = ev(json!({"type":"event_msg","payload":{"type":"task_complete"}}));
            assert!(
                matches!(out.as_slice(), [AgentEvent::TurnEnd { turn: None, .. }]),
                "an end naming no turn still ends the work: {out:?}"
            );
        });
        assert!(logs.contains(TURN_ID), "{logs}");
    }

    #[test]
    fn session_meta_and_unknown_emit_nothing() {
        assert!(ev(json!({"type":"session_meta","payload":{"id":"u","cwd":"/r"}})).is_empty());
        // A token_count without `info` is a rate-limit-only ping.
        assert!(ev(json!({"type":"event_msg","payload":{"type":"token_count"}})).is_empty());
    }

    #[test]
    fn token_count_emits_fresh_usage_from_last_reading() {
        // fresh = (11480−9088) + 87: the 15 reasoning tokens are part of the 87.
        let out = ev(
            json!({"type":"event_msg","payload":{"type":"token_count","info":{
            "total_token_usage":{"input_tokens":999999,"cached_input_tokens":0,"output_tokens":999999},
            "last_token_usage":{"input_tokens":11480,"cached_input_tokens":9088,
                                 "output_tokens":87,"reasoning_output_tokens":15}}}}),
        );
        assert!(
            matches!(
                out.as_slice(),
                [AgentEvent::Usage {
                    fresh_tokens: 2479,
                    ..
                }]
            ),
            "expected fresh=2479 from last_token_usage (never the totals), got {out:?}"
        );
    }

    #[test]
    fn token_count_saturates_and_skips_zero() {
        // cached > input is an upstream reporting quirk.
        let out = ev(
            json!({"type":"event_msg","payload":{"type":"token_count","info":{
            "last_token_usage":{"input_tokens":10,"cached_input_tokens":50,"output_tokens":7}}}}),
        );
        assert!(
            matches!(
                out.as_slice(),
                [AgentEvent::Usage {
                    fresh_tokens: 7,
                    ..
                }]
            ),
            "got {out:?}"
        );
        let out = ev(
            json!({"type":"event_msg","payload":{"type":"token_count","info":{
            "last_token_usage":{"input_tokens":0,"cached_input_tokens":0,"output_tokens":0}}}}),
        );
        assert!(out.is_empty(), "zero reading must be silent, got {out:?}");
    }

    #[test]
    fn id_from_rollout_path_is_trailing_uuid() {
        let p = Path::new(
            "/Users/me/.codex/sessions/2026/05/29/rollout-2026-05-29T22-36-52-019e7762-9ded-7e33-be41-946ecf105bf4.jsonl",
        );
        // Must equal the hook session_id for coalescing.
        assert_eq!(
            codex_id_from_path(p),
            "019e7762-9ded-7e33-be41-946ecf105bf4"
        );
    }

    // `codex_id_from_path` is invoked in three places that must agree — the
    // per-line decode, the registry row's `id_from_path`, and the fixture test
    // above. A disagreement splits one Codex session into two sprites.
    #[test]
    fn decode_line_keys_agent_id_on_codex_id_from_path() {
        let path = "/x/rollout-1-019e7762-9ded-7e33-be41-946ecf105bf4.jsonl";
        let events = decode_codex_line(
            path,
            SOURCE_NAME,
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t"}}),
        );
        let expected = AgentId::from_parts(SOURCE_NAME, &codex_id_from_path(Path::new(path)));
        assert_eq!(
            events[0].agent_id(),
            expected,
            "decode_codex_line must key its AgentId on codex_id_from_path (the deriver)"
        );
    }

    #[test]
    fn id_falls_back_to_stem_without_uuid() {
        let p = Path::new("/tmp/notarollout.jsonl");
        assert_eq!(codex_id_from_path(p), "notarollout");
    }

    #[test]
    fn id_handles_non_ascii_filename_without_panic() {
        // A stem whose len-36 byte split lands mid-codepoint.
        let p = Path::new("/tmp/rollout-日本語のとてもながいファイルめい.jsonl");
        let _ = codex_id_from_path(p);
    }

    #[test]
    fn non_object_line_emits_nothing() {
        assert!(ev(json!("just a string")).is_empty());
        assert!(ev(json!(42)).is_empty());
        assert!(ev(json!([1, 2, 3])).is_empty());
    }

    #[test]
    fn function_call_without_name_falls_back_to_tool_label() {
        use crate::source::ToolDetail;
        let out = ev(json!({
            "type": "response_item",
            "payload": { "type": "function_call", "arguments": r#"{"cmd":"ls"}"# }
        }));
        match out.as_slice() {
            [
                AgentEvent::ToolUseStart {
                    detail: Some(ToolDetail::Generic { display }),
                    ..
                },
            ] => assert_eq!(display, "tool"),
            other => panic!("expected one Generic-detail ToolUseStart, got {other:?}"),
        }
    }

    #[test]
    fn no_rollout_outer_breadcrumbs_however_new() {
        for outer in [
            "brand_new_outer_2027",
            "session_meta",
            "event_msg",
            "compacted",
            "world_state",
            "inter_agent_communication",
            "inter_agent_communication_metadata",
            "response_item",
        ] {
            let line =
                serde_json::json!({ "type": outer, "payload": { "type": "x", "message": "y" } });
            let quiet = crate::test_capture::capture_logs(|| {
                decode_codex_line("/x/rollout.jsonl", SOURCE_NAME, line);
            });
            assert!(
                !quiet.contains(crate::source::drift::TARGET),
                "outer {outer:?} carries nothing we read, so it must stay out of the drift log:\n{quiet}"
            );
        }
    }
}
