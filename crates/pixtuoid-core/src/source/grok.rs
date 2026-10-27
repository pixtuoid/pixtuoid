//! Grok Build source (`grok`, xai-org/grok-build) — TRANSCRIPT-BEARING with a
//! hook install target (the CC/Codex class: both transports).
//!
//! - **Transcript**: `{grok_home}/sessions/<enc-cwd>/<session-id>/updates.jsonl`
//!   is append-ONLY for the session's whole life (even `/rewind` appends a
//!   `rewind_marker` instead of truncating). The SIBLING `chat_history.jsonl`
//!   is REWRITTEN via temp+rename on resume/compaction/rewind — never tail it.
//! - **Hooks**: JSON envelope on stdin with **camelCase field names and
//!   snake_case event values** (`hookEventName`, `sessionId`, `toolUseId`, …)
//!   — the only spelling pre-1.0 sent, hence the claims-all custom decoder
//!   below; 1.0.x mirrors every key in CC's snake_case too
//!   (`grok/tool-run-recorded`), which this decoder still does not read. Hooks
//!   dispatch SEQUENTIALLY inline on the session actor, so the shim's
//!   `WRITE_TIMEOUT` bound matters here.
//! - **Keying**: `sessionId` is consistent across every event of a session, ==
//!   the transcript's parent-DIR name, == a subagent's `subagentId`. Hook and
//!   watcher keys therefore coalesce, and a child's tool hooks carry the
//!   CHILD's `sessionId`.
//! - **Subagents**: in-process children persisted as FLAT siblings in the
//!   normal sessions tree. Children fire NO `session_start` hook of their own
//!   — the `subagent_start` hook (or the parent transcript's
//!   `subagent_spawned` line) is the child's registration carrier.
//! - **Exit profile**: `session_end` fires on shutdown and channel-closed
//!   teardown but NOT on a plain TUI quit, and not on kill. The reliable exit
//!   signal is the liveness ladder over grok's own crash-recovery registry
//!   `{grok_home}/active_sessions.json` (removed on clean quit, left on crash).
//!   No open-FD probe is possible: every append opens and drops the file
//!   handle, unlike Codex's for-lifetime rollout fd.

use crate::source::decoder::{DecodeError, DecodeResult as Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::AgentId;
use crate::source::decoder::{MAX_DECODED_FIELD_CHARS, ellipsize, generic_tool_display};
use crate::source::registry::HookObject;
use crate::source::{AgentEvent, ToolDetail};

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::GrokSource;
#[cfg(feature = "native")]
pub(crate) use native::live_grok_session_ids;

/// The Grok Build source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "grok";

/// xAI's private JSON-RPC method namespace, named once so the decode arm and
/// the session-end scan cannot drift apart.
const XAI_SESSION_UPDATE_METHOD: &str = "_x.ai/session/update";

const XAI_SUBAGENT_SPAWNED: &str = "subagent_spawned";
const XAI_SUBAGENT_FINISHED: &str = "subagent_finished";
const XAI_TURN_COMPLETED: &str = "turn_completed";
const XAI_HOOK_EXECUTION: &str = "hook_execution";

/// Decode one grok hook payload (already identified by
/// `_pixtuoid_source == "grok"`), keyed on `sessionId`.
///
/// `user_prompt_submit` maps to `SessionStart` too — it is the resurrect
/// carrier, because grok's `session_end` is unreliable (a TUI quit fires none)
/// so a stale-swept LIVE session must walk back in on its next prompt.
/// Anything unrecognized bails: registered-vs-decoded drift must be loud.
///
/// # Errors
///
/// If the payload lacks `hookEventName`, carries none of `sessionId`, `cwd` or `workspaceRoot`, names an unrecognized event, or is a subagent event without `subagentId`.
pub(crate) fn decode_grok_hook_payload(obj: &HookObject) -> Result<Vec<AgentEvent>> {
    let event = obj
        .get("hookEventName")
        .and_then(|s| s.as_str())
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "hookEventName"))?;
    let cwd = obj
        .get("cwd")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            obj.get("workspaceRoot")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
        });
    let key = obj
        .get("sessionId")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .or(cwd)
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "sessionId|cwd|workspaceRoot"))?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, key);
    let cwd_path = || cwd.map(PathBuf::from);

    let identity = || AgentEvent::identity(agent_id, SOURCE_NAME, key, cwd_path());
    let tool_use_id = || {
        obj.get("toolUseId")
            .and_then(|s| s.as_str())
            .map(String::from)
    };

    match event {
        "session_start" => {
            // grok's payload ALSO carries a public `source` field ("new"/"load"
            // — the start REASON, exactly CC's overload). Never read it:
            // attribution comes ONLY from `_pixtuoid_source`, else agents split
            // into un-reapable ghosts.
            let mut evs = vec![AgentEvent::SessionStart {
                agent_id,
                source: SOURCE_NAME.to_string(),
                session_id: key.to_string(),
                cwd: cwd.unwrap_or("").into(),
                parent_id: None,
            }];
            // The fire site passes None today; take `modelId` if a future
            // build offers it.
            if let Some(model) = obj
                .get("modelId")
                .and_then(|m| m.as_str())
                .filter(|m| !m.is_empty())
            {
                evs.push(AgentEvent::ModelInfo {
                    agent_id,
                    model: Some(ellipsize(model, MAX_DECODED_FIELD_CHARS)),
                    effort: None,
                });
            }
            Ok(evs)
        }
        "user_prompt_submit" => Ok(vec![AgentEvent::SessionStart {
            agent_id,
            source: SOURCE_NAME.to_string(),
            session_id: key.to_string(),
            cwd: cwd.unwrap_or("").into(),
            parent_id: None,
        }]),
        "pre_tool_use" => {
            let tool = obj
                .get("toolName")
                .and_then(|s| s.as_str())
                .unwrap_or_else(|| {
                    crate::source::drift::missing_field(SOURCE_NAME, "pre_tool_use", "toolName");
                    "?"
                });
            Ok(vec![
                identity(),
                AgentEvent::ActivityStart {
                    agent_id,
                    tool_use_id: tool_use_id(),
                    detail: Some(grok_tool_detail(tool, obj.get("toolInput"))),
                },
            ])
        }
        // A FAILED tool fires `post_tool_use_failure` INSTEAD OF
        // `post_tool_use`; a DENIED one fires `permission_denied` and never
        // runs at all. All three close the activity, which also resolves a
        // permission `Waiting` gated on this tool — right for the denied case:
        // the prompt is answered, so the sprite must not stay Waiting.
        "post_tool_use" | "post_tool_use_failure" | "permission_denied" => Ok(vec![
            identity(),
            AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id: tool_use_id(),
            },
        ]),
        "notification" => {
            let kind = obj
                .get("notificationType")
                .and_then(|s| s.as_str())
                .unwrap_or_else(|| {
                    crate::source::drift::missing_field(
                        SOURCE_NAME,
                        "notification",
                        "notificationType",
                    );
                    "?"
                });
            match kind {
                // Fires BEFORE the prompt shows. Resolution: approval fires NO
                // hook (the tool proceeds → its post_tool_use End clears the
                // gate); denial fires permission_denied (same End, above).
                "permission_prompt" | "elicitation_dialog" => {
                    let msg = obj
                        .get("message")
                        .and_then(|s| s.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(kind);
                    Ok(vec![
                        identity(),
                        AgentEvent::Waiting {
                            agent_id,
                            reason: ellipsize(msg, MAX_DECODED_FIELD_CHARS),
                            tool_use_id: None,
                        },
                    ])
                }
                // `idle_prompt` is the 60s-idle nudge — idle, not blocked; a
                // Waiting would misrender every lunch break as a permission
                // prompt. `agent_error` is the retry-exhausted toast, an
                // errored TURN whose state signal is the `stop_failure` arm.
                // `task_complete` announces a BACKGROUNDED shell/monitor task
                // finishing — no `toolUseId`, and its spawning tool call
                // already Ended at backgrounding time, so nothing to close.
                // All matched explicitly so none spams the breadcrumb.
                "idle_prompt" | "agent_error" | "task_complete" => Ok(vec![]),
                other => {
                    crate::source::drift::unknown_event(
                        SOURCE_NAME,
                        &format!("notification:{other}"),
                    );
                    Ok(vec![])
                }
            }
        }
        // Identity-less: an end alone proves too little to label a session, so
        // an unknown id registers blank (`shows_activity`). All three of
        // upstream's turn-end causes land here.
        "stop" | "stop_failure" | "stop_cancelled" => Ok(vec![AgentEvent::turn_end(agent_id)]),
        "subagent_start" => {
            let Some(child_session_id) = child_key(obj) else {
                crate::source::drift::missing_field(SOURCE_NAME, event, "subagentId");
                return Err(DecodeError::missing_in(SOURCE_NAME, event, "subagentId"));
            };
            let child = AgentId::from_parts(SOURCE_NAME, &child_session_id);
            let mut evs = vec![AgentEvent::SessionStart {
                agent_id: child,
                source: SOURCE_NAME.to_string(),
                session_id: child_session_id,
                // The envelope cwd is the PARENT's — correct for the default
                // inherited-cwd child. A worktree-ISOLATED child runs
                // elsewhere, leaving only its outfit-palette cwd key
                // parent-tinted: accepted residual.
                cwd: cwd.unwrap_or("").into(),
                parent_id: Some(agent_id),
            }];
            if let Some(label) = obj
                .get("description")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| obj.get("subagentType").and_then(|s| s.as_str()))
                .filter(|s| !s.is_empty())
            {
                evs.push(AgentEvent::Rename {
                    agent_id: child,
                    label: ellipsize(label, MAX_DECODED_FIELD_CHARS),
                });
            }
            Ok(evs)
        }
        "subagent_stop" => Ok(vec![AgentEvent::SessionEnd {
            agent_id: subagent_child_id(obj, event)?,
            as_child: true,
        }]),
        "session_end" => Ok(vec![AgentEvent::SessionEnd {
            agent_id,
            as_child: false,
        }]),
        other => {
            crate::source::drift::unknown_event(SOURCE_NAME, other);
            Err(DecodeError::unsupported(SOURCE_NAME, other))
        }
    }
}

/// The child's `subagentId` — upstream sets `child_session_id = subagent_id`,
/// so this key coalesces with the child's own tool hooks AND its flat
/// transcript dir name.
fn child_key(obj: &serde_json::Map<String, Value>) -> Option<String> {
    obj.get("subagentId")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// The TRANSCRIPT lane's twin of [`child_key`]: the same id under
/// `child_session_id`. Shared by both subagent arms so the non-empty guard
/// cannot be applied to one and forgotten on the other — an `""` mints a
/// phantom child parented to the real session.
fn transcript_child_key(update: &serde_json::Map<String, Value>) -> Option<&str> {
    update
        .get("child_session_id")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
}

fn subagent_child_id(obj: &serde_json::Map<String, Value>, event: &str) -> Result<AgentId> {
    match child_key(obj) {
        Some(id) => Ok(AgentId::from_parts(SOURCE_NAME, &id)),
        None => {
            crate::source::drift::missing_field(SOURCE_NAME, event, "subagentId");
            Err(DecodeError::missing_in(SOURCE_NAME, event, "subagentId"))
        }
    }
}

/// Grok tool detail: `"name: target"` over grok's snake_case tool vocabulary.
///
/// **`spawn_subagent` maps to `ToolDetail::Task` ONLY for an explicit
/// `background: false` (blocking) dispatch — NOT on the CC-style semantic
/// `subagent_type` detection.** grok's spawn defaults to background=TRUE, where
/// `post_tool_use` fires at SPAWN time, not completion; a Task-detail Start
/// whose End arrives immediately would drain `tasks_in_flight` while the child is
/// alive, and the reducer's b1 drain-cascade would then `cascade_exit` the LIVE
/// child subtree, unrecoverably. Blocking spawns are the one shape where
/// End == completion. Skipping Task detail elsewhere loses nothing: grok
/// children are FIRST-CLASS (child-keyed tool hooks) and their ends are
/// wire-carried, so b1 has no end to infer.
fn grok_tool_detail(tool: &str, args: Option<&Value>) -> ToolDetail {
    let is_spawn = tool == "spawn_subagent" || args.and_then(|a| a.get("subagent_type")).is_some();
    if is_spawn && spawn_is_blocking(args) {
        return ToolDetail::Task;
    }
    const KEYS: &[&str] = &[
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "description",
    ];
    crate::source::decoder::generic_keyed_detail(tool, args, KEYS)
}

/// Decode one `updates.jsonl` line. Envelope: `{"timestamp":<unix-secs>,
/// "method":…,"params":{"sessionId":…,"update":{"sessionUpdate":"<tag>",…}}}`,
/// where ACP notifications use camelCase fields and the xAI extension's fields
/// are verbatim snake_case Rust names (`rename_all` covers only the tag).
///
/// The message/thought/plan chunks decode to nothing: a chunk has no paired
/// end, and the coalescer may even land an xAI line BEFORE the buffered text
/// that preceded it, so chunk ordering is not activity truth.
///
/// The agent id is derived from the PATH (`grok_id_from_path`), NEVER the
/// line's `sessionId`: the path is the watcher's id space, and the two are
/// equal by construction. The hook transport keys on the same string, so
/// cross-transport dedup (hook `toolUseId` == ACP `toolCallId`) actually fires.
pub(crate) fn decode_grok_line(path: &str, source: &str, v: Value) -> Vec<AgentEvent> {
    let agent_id = AgentId::from_parts(source, &grok_id_from_path(Path::new(path)));
    let Some(method) = v.get("method").and_then(|m| m.as_str()) else {
        return vec![];
    };
    let Some(update) = v.pointer("/params/update").and_then(|u| u.as_object()) else {
        return vec![];
    };
    let Some(tag) = update.get("sessionUpdate").and_then(|t| t.as_str()) else {
        return vec![];
    };
    let str_field = |key: &str| update.get(key).and_then(|s| s.as_str());

    let mut evs = match method {
        crate::source::acp::SESSION_UPDATE_METHOD => {
            crate::source::acp::decode_session_update(agent_id, update, grok_transcript_tool_detail)
        }
        // xAI private extension namespace — ACP reserves the `_` prefix for
        // implementation-specific methods, so none of this is ACP vocabulary.
        XAI_SESSION_UPDATE_METHOD => match tag {
            XAI_SUBAGENT_SPAWNED => {
                let Some(child_key) = transcript_child_key(update) else {
                    crate::source::drift::missing_field(SOURCE_NAME, tag, "child_session_id");
                    return vec![];
                };
                let child = AgentId::from_parts(SOURCE_NAME, child_key);
                let mut evs = vec![AgentEvent::SessionStart {
                    agent_id: child,
                    source: SOURCE_NAME.to_string(),
                    session_id: child_key.to_string(),
                    // The line carries no cwd; the child's own flat transcript
                    // or its tool hooks back-fill it.
                    cwd: PathBuf::new(),
                    parent_id: Some(agent_id),
                }];
                if let Some(label) = str_field("description")
                    .filter(|s| !s.is_empty())
                    .or_else(|| str_field("subagent_type"))
                    .filter(|s| !s.is_empty())
                {
                    evs.push(AgentEvent::Rename {
                        agent_id: child,
                        label: ellipsize(label, MAX_DECODED_FIELD_CHARS),
                    });
                }
                evs
            }
            XAI_SUBAGENT_FINISHED => {
                let Some(child_key) = transcript_child_key(update) else {
                    crate::source::drift::missing_field(SOURCE_NAME, tag, "child_session_id");
                    return vec![];
                };
                vec![AgentEvent::SessionEnd {
                    agent_id: AgentId::from_parts(SOURCE_NAME, child_key),
                    as_child: true,
                }]
            }
            // The transcript twin of the `stop` hook: a tool-less turn's only
            // end signal for transcript-only setups.
            XAI_TURN_COMPLETED => vec![AgentEvent::turn_end(agent_id)],
            XAI_HOOK_EXECUTION => {
                if str_field("event_name") == Some("session_end") {
                    vec![AgentEvent::SessionEnd {
                        agent_id,
                        as_child: false,
                    }]
                } else {
                    vec![]
                }
            }
            // grok emits many cosmetic xAI updates (diff_review, compaction,
            // rewind_marker, …), so an unhandled extension tag is a silent skip.
            _ => vec![],
        },
        _ => vec![],
    };
    // The model rides `_meta` on an ORDINARY update; keying on the
    // `model_changed` tag a real session never sends left the flame dark.
    if let Some(model) = update
        .get("_meta")
        .and_then(|m| m.get("modelId"))
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
    {
        evs.push(AgentEvent::ModelInfo {
            agent_id,
            model: Some(ellipsize(model, MAX_DECODED_FIELD_CHARS)),
            effort: None,
        });
    }
    evs
}

/// grok transcript lines carry NO cwd anywhere in their content — it exists
/// only as the URL-encoded GROUP-DIR name one level up — so the content
/// head-scan always yields nothing and [`grok_cwd_from_path`] is the real
/// cwd source.
pub(crate) fn extract_grok_cwd(_v: &Value) -> Option<PathBuf> {
    None
}

/// Transcript tool detail: a FRESH `tool_call`'s `title` is the RAW tool name
/// (the human label like "Execute `cat note.txt`" appears only on later
/// `tool_call_update`s, which this fn never sees), so the title IS the display.
/// Task detection follows the SAME blocking-only rule as the hook side — see
/// [`grok_tool_detail`] for the b1 WHY.
fn grok_transcript_tool_detail(title: &str, raw_input: Option<&Value>) -> ToolDetail {
    if raw_input.is_some_and(|a| a.get("subagent_type").is_some()) && spawn_is_blocking(raw_input) {
        return ToolDetail::Task;
    }
    generic_tool_display(title, None)
}

/// Whether spawn args explicitly request a BLOCKING run (`false`): the model's
/// `background` on both transports, since the hook's `toolInput` and a fresh
/// `tool_call`'s `rawInput` carry its arguments as sent; the struct's own
/// `run_in_background` rides only the later `tool_call_update` (1.0.62's run).
/// Upstream parses the flag LENIENTLY — a model-emitted `"false"` STRING still
/// runs blocking — which this `as_bool` read misses toward the SAFE side only:
/// a missed `false` skips the Task detail, never over-mints it into b1.
fn spawn_is_blocking(args: Option<&Value>) -> bool {
    args.and_then(|a| a.get("background"))
        .and_then(Value::as_bool)
        == Some(false)
}

/// The first-sight gate's session-ended checker: an ended grok session is
/// recognizable ONLY by the best-effort `hook_execution{event_name:
/// "session_end"}` line our own installed hook causes. The per-line parse must
/// stay STRUCTURAL — a substring scan would false-positive on a tool result
/// QUOTING this marker inside a JSON string.
#[cfg(feature = "native")]
pub(crate) fn grok_session_ended(tail: &[u8]) -> bool {
    crate::source::decoder::parsed_tail_lines(tail).any(|v| {
        v.get("method").and_then(|m| m.as_str()) == Some(XAI_SESSION_UPDATE_METHOD)
            && v.pointer("/params/update/sessionUpdate")
                .and_then(|t| t.as_str())
                == Some("hook_execution")
            && v.pointer("/params/update/event_name")
                .and_then(|e| e.as_str())
                == Some("session_end")
    })
}

/// Only `updates.jsonl` is the tailable transcript. Every session dir carries
/// SIBLING `.jsonl` files that must never be walked: `chat_history.jsonl` and
/// `rewind_points.jsonl` are REWRITTEN via temp+rename (a tail would replay
/// whole files as fresh events and mint a second path-keyed sprite), and
/// `feedback.jsonl`/`btw_history.jsonl`/`prompt_history.jsonl` are not session
/// streams at all.
pub(crate) fn is_updates_jsonl(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()) == Some("updates.jsonl")
}

/// Session id from a transcript path: the PARENT-DIR name, the filename stem
/// being the constant `updates`. Equal to every hook event's `sessionId`, so
/// the two transports coalesce.
pub(crate) fn grok_id_from_path(path: &Path) -> String {
    path.parent()
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// The session's cwd from a transcript path: the GRANDPARENT dir name is
/// grok's `encode_cwd_dirname(cwd)`. Mirrors upstream `decode_cwd_from_dirname`
/// exactly — URL-decode the name and accept it only when it looks absolute;
/// otherwise it is the `{slug}-{blake3_hex16}` long-path form, whose original
/// cwd upstream records in a sibling `.cwd` file.
#[cfg(feature = "native")]
pub(crate) fn grok_cwd_from_path(path: &Path) -> Option<PathBuf> {
    let group = path.parent()?.parent()?;
    let name = group.file_name()?.to_str()?;
    if let Some(decoded) = percent_decode(name) {
        // Upstream's own absolute-path test distinguishes the two encodings.
        if decoded.starts_with('/') || (cfg!(windows) && decoded.chars().nth(1) == Some(':')) {
            return Some(PathBuf::from(decoded));
        }
    }
    let raw = read_bounded(&group.join(".cwd"), MAX_CWD_FILE_BYTES)?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// A `.cwd` file holds one path, so the cap only guards a planted file.
#[cfg(feature = "native")]
const MAX_CWD_FILE_BYTES: u64 = 4096;

#[cfg(feature = "native")]
fn read_bounded(path: &Path, cap: u64) -> Option<String> {
    use std::io::Read;
    let f = std::fs::File::open(path).ok()?;
    let mut buf = String::new();
    f.take(cap).read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// Pure `%XX` percent-decoding. Upstream's `urlencoding::encode` never emits
/// `+` for a space, so `+` passes through literally.
#[cfg(feature = "native")]
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hi = (hex[0] as char).to_digit(16)?;
            let lo = (hex[1] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The grok home dir, matching grok-build's own resolution (`xai-grok-config`
/// `paths.rs::grok_home`): `GROK_HOME` UNCONDITIONALLY when set (upstream takes
/// the env var without an exists-check and `create_dir_all`s it — the opposite
/// of codex's existing-dir gate), else `<home>/.grok` on EVERY OS (no XDG, no
/// APPDATA). Upstream resolves home via `std::env::home_dir()` (USERPROFILE on
/// Windows, `$HOME` never consulted there), which `user_home` mirrors. The
/// watcher, the installer and the liveness probe all route through here, so
/// the watched root, the installed hooks file and the probed registry can never
/// disagree.
///
/// Not mirrored: upstream `dunce::canonicalize`s the DEFAULT home before joining
/// `.grok` (never `$GROK_HOME`), so under a SYMLINKED `$HOME` its path string
/// differs from ours while naming the same directory. Unobservable because the
/// exposed surface — the sessions WATCH ROOT — is opened, not string-compared,
/// and both forms resolve through the link to one dir.
pub fn grok_home() -> PathBuf {
    resolve_grok_home(
        crate::platform::path_env("GROK_HOME"),
        crate::platform::user_home(),
    )
}

/// Pure precedence core, separated so it's unit-testable without env mutation.
fn resolve_grok_home(grok_home_env: Option<PathBuf>, home: PathBuf) -> PathBuf {
    grok_home_env.unwrap_or_else(|| home.join(".grok"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;

    #[test]
    fn grok_home_takes_env_unconditionally_even_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing");
        assert_eq!(
            resolve_grok_home(Some(missing.clone()), PathBuf::from("/home/u")),
            missing
        );
    }

    #[test]
    fn grok_home_falls_back_to_dot_grok_when_env_unset() {
        let expected = PathBuf::from("/home/u").join(".grok");
        assert_eq!(resolve_grok_home(None, PathBuf::from("/home/u")), expected);
    }
    use serde_json::json;

    fn decode_all(v: Value) -> Vec<AgentEvent> {
        decode_grok_hook_payload(test_obj(&v)).expect("decodes")
    }

    /// The payload's MAIN event is the LAST decoded one — activity arms
    /// prepend an `Identity`, subagent_start appends a `Rename`.
    fn decode(v: Value) -> AgentEvent {
        decode_all(v).pop().expect("at least one event")
    }

    fn envelope(event: &str) -> Value {
        json!({
            "hookEventName": event,
            "sessionId": "0197fa30-sess",
            "cwd": "/Users/dev/proj",
            "workspaceRoot": "/Users/dev/proj",
            "timestamp": "2026-07-16T12:00:00Z"
        })
    }

    #[test]
    fn session_start_public_source_field_never_drives_attribution() {
        for reason in ["new", "load"] {
            let mut v = envelope("session_start");
            v["source"] = json!(reason);
            match decode(v) {
                AgentEvent::SessionStart { source, .. } => assert_eq!(source, SOURCE_NAME),
                other => panic!("expected SessionStart, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_ordinary_updates_meta_carries_the_model() {
        // The shape from fixtures/grok/permission-hermetic — `model_changed`
        // never appears in a real session.
        let evs = decode_line(json!({
            "method": "session/update",
            "params": {"sessionId": "s", "update": {
                "sessionUpdate": "user_message_chunk",
                "content": {"type": "text", "text": "hi"},
                "_meta": {"modelId": "grok-4.6"}
            }}
        }));
        assert!(
            evs.iter().any(
                |e| matches!(e, AgentEvent::ModelInfo { model: Some(m), .. } if m == "grok-4.6")
            ),
            "an update's _meta.modelId must reach ModelInfo, got {evs:?}"
        );
    }

    #[test]
    fn session_start_takes_model_id_when_offered() {
        let mut v = envelope("session_start");
        v["modelId"] = json!("grok-4-code");
        let evs = decode_all(v);
        assert_eq!(evs.len(), 2);
        assert!(
            matches!(&evs[1], AgentEvent::ModelInfo { model: Some(m), effort: None, .. }
            if m == "grok-4-code")
        );
        assert_eq!(decode_all(envelope("session_start")).len(), 1);
    }

    #[test]
    fn blocking_spawn_is_task_background_and_default_are_not() {
        let blocking = grok_tool_detail(
            "spawn_subagent",
            Some(&json!({"subagent_type": "explore", "background": false})),
        );
        assert!(blocking.is_task(), "blocking spawn must read Delegating");

        // background:true AND the absent-field DEFAULT (grok's spawn defaults
        // to background) must both stay generic.
        for input in [
            json!({"subagent_type": "explore", "background": true}),
            json!({"subagent_type": "explore", "description": "map the build"}),
        ] {
            let detail = grok_tool_detail("spawn_subagent", Some(&input));
            assert!(
                !detail.is_task(),
                "background/default spawn must NOT be Task (b1 would evict the live child): {input}"
            );
        }
        assert!(!grok_tool_detail("spawn_subagent", None).is_task());
        let renamed = grok_tool_detail(
            "task",
            Some(&json!({"subagent_type": "explore", "background": false})),
        );
        assert!(
            renamed.is_task(),
            "semantic detection still applies when blocking"
        );
    }

    #[test]
    fn blocking_flag_is_the_models_background_on_both_transports() {
        let blocking = json!({"subagent_type": "explore", "background": false});
        assert!(grok_tool_detail("spawn_subagent", Some(&blocking)).is_task());
        assert!(grok_transcript_tool_detail("Spawn subagent", Some(&blocking)).is_task());
        let background = json!({"subagent_type": "explore", "background": true});
        assert!(!grok_tool_detail("spawn_subagent", Some(&background)).is_task());
        assert!(!grok_transcript_tool_detail("Spawn subagent", Some(&background)).is_task());
        // The struct's spelling rides no line either transport reads.
        let structs_spelling = json!({"subagent_type": "explore", "run_in_background": false});
        assert!(!grok_tool_detail("spawn_subagent", Some(&structs_spelling)).is_task());
        assert!(!grok_transcript_tool_detail("Spawn subagent", Some(&structs_spelling)).is_task());
        // A model-emitted STRING "false" (upstream parses leniently).
        let lenient = json!({"subagent_type": "explore", "background": "false"});
        assert!(!grok_tool_detail("spawn_subagent", Some(&lenient)).is_task());
    }

    #[test]
    fn background_spawn_displays_description_as_target() {
        let detail = grok_tool_detail(
            "spawn_subagent",
            Some(&json!({"subagent_type": "explore", "description": "map the build"})),
        );
        assert_eq!(detail.display(), "spawn_subagent: map the build");
    }

    #[test]
    fn tool_target_uses_grok_arg_vocabulary() {
        let mut v = envelope("pre_tool_use");
        v["toolName"] = json!("read_file");
        v["toolInput"] = json!({"path": "src/lib.rs"});
        assert!(
            matches!(decode(v), AgentEvent::ActivityStart { detail: Some(d), .. }
            if d.display() == "read_file: src/lib.rs")
        );
    }

    #[test]
    fn permission_and_elicitation_notifications_are_waiting() {
        for kind in ["permission_prompt", "elicitation_dialog"] {
            let mut v = envelope("notification");
            v["notificationType"] = json!(kind);
            v["message"] = json!("Tool permission requested");
            let evs = decode_all(v);
            assert_eq!(evs.len(), 2, "{kind}: Identity + Waiting");
            assert!(
                matches!(&evs[1], AgentEvent::Waiting { reason, .. }
                    if reason == "Tool permission requested"),
                "{kind} must decode to Waiting"
            );
        }
    }

    #[test]
    fn waiting_reason_falls_back_to_the_notification_type() {
        let mut v = envelope("notification");
        v["notificationType"] = json!("permission_prompt");
        assert!(matches!(decode(v), AgentEvent::Waiting { reason, .. }
            if reason == "permission_prompt"));
    }

    /// Zero events is the SAME observable for a knowingly-ignored type and an
    /// unrecognized one, so the test above cannot tell them apart — this one
    /// does, on the drift breadcrumb. `task_complete` is dispatched by upstream
    /// on every background-task completion (`tools/notification_bridge.rs`),
    /// and `drift::unknown_event` is undeduped on the hook plane, so leaving it
    /// unmatched turns routine background work into a recurring "the wire
    /// changed" warning that `pixtuoid doctor` surfaces.
    #[test]
    fn known_notification_types_stay_silent_while_a_novel_one_breadcrumbs() {
        for known in ["idle_prompt", "agent_error", "task_complete"] {
            let mut v = envelope("notification");
            v["notificationType"] = json!(known);
            let logs = crate::test_capture::capture_logs(|| {
                assert!(decode_all(v).is_empty());
            });
            assert!(
                !logs.contains("unknown_event"),
                "{known} is a KNOWN non-waiting type — it must not breadcrumb, got:\n{logs}"
            );
        }
        // Control: the breadcrumb still fires for a genuinely new type, so the
        // assertions above are silence-by-recognition, not a dead detector.
        let mut novel = envelope("notification");
        novel["notificationType"] = json!("some_future_nudge");
        let logs = crate::test_capture::capture_logs(|| {
            assert!(decode_all(novel).is_empty());
        });
        assert!(
            logs.contains("unknown_event") && logs.contains("notification:some_future_nudge"),
            "an unrecognized notificationType must still breadcrumb, got:\n{logs}"
        );
    }

    #[test]
    fn subagent_start_registers_the_child_under_the_parent() {
        let mut v = envelope("subagent_start");
        v["subagentId"] = json!("0197fa31-child");
        v["subagentType"] = json!("explore");
        v["description"] = json!("map the build");
        let evs = decode_all(v);
        assert_eq!(evs.len(), 2);
        match &evs[0] {
            AgentEvent::SessionStart {
                agent_id,
                session_id,
                parent_id,
                ..
            } => {
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts(SOURCE_NAME, "0197fa31-child"),
                    "child keys on subagentId (== child session id)"
                );
                assert_eq!(session_id, "0197fa31-child");
                assert_eq!(
                    *parent_id,
                    Some(AgentId::from_parts(SOURCE_NAME, "0197fa30-sess")),
                    "parent link from the envelope's (parent) sessionId"
                );
            }
            other => panic!("expected child SessionStart, got {other:?}"),
        }
        assert!(
            matches!(&evs[1], AgentEvent::Rename { agent_id, label }
                if *agent_id == AgentId::from_parts(SOURCE_NAME, "0197fa31-child")
                    && label == "map the build"),
            "description is the primary label (grok's own precedence)"
        );
    }

    #[test]
    fn subagent_rename_falls_back_to_type_when_description_absent() {
        let mut v = envelope("subagent_start");
        v["subagentId"] = json!("c");
        v["subagentType"] = json!("explore");
        let evs = decode_all(v);
        assert!(matches!(&evs[1], AgentEvent::Rename { label, .. } if label == "explore"));
    }

    #[test]
    fn subagent_stop_ends_the_child_as_child() {
        let mut v = envelope("subagent_stop");
        v["subagentId"] = json!("0197fa31-child");
        v["subagentType"] = json!("explore");
        v["exitCode"] = json!(0);
        let evs = decode_all(v);
        assert_eq!(evs.len(), 1);
        assert!(
            matches!(&evs[0], AgentEvent::SessionEnd { agent_id, as_child: true }
                if *agent_id == AgentId::from_parts(SOURCE_NAME, "0197fa31-child")),
            "subagent_stop must end the CHILD with the as_child stamp"
        );
    }

    #[test]
    fn subagent_events_without_subagent_id_are_malformed() {
        for event in ["subagent_start", "subagent_stop"] {
            assert!(
                decode_grok_hook_payload(test_obj(&envelope(event))).is_err(),
                "{event} without subagentId must bail"
            );
        }
    }

    #[test]
    fn key_falls_back_to_cwd_when_session_id_absent() {
        let ev = decode(json!({
            "hookEventName": "stop",
            "cwd": "/Users/dev/proj"
        }));
        assert!(matches!(ev, AgentEvent::TurnEnd { agent_id }
            if agent_id == AgentId::from_parts(SOURCE_NAME, "/Users/dev/proj")));
    }

    #[test]
    fn every_turn_end_cause_is_a_bare_turn_end() {
        for event in ["stop", "stop_failure", "stop_cancelled"] {
            let evs = decode_all(envelope(event));
            assert!(
                matches!(evs.as_slice(), [AgentEvent::TurnEnd { .. }]),
                "{event}: {evs:?}"
            );
        }
    }

    #[test]
    fn nothing_to_key_on_is_malformed() {
        assert!(decode_grok_hook_payload(test_obj(&json!({"hookEventName": "stop"}))).is_err());
        assert!(
            decode_grok_hook_payload(test_obj(
                &json!({"hookEventName": "stop", "cwd": "", "workspaceRoot": ""})
            ))
            .is_err()
        );
        assert!(decode_grok_hook_payload(test_obj(&json!({"sessionId": "s"}))).is_err());
    }

    #[test]
    fn unregistered_events_bail_loudly() {
        // pre/post_compact are deliberately unregistered.
        for ev in ["pre_compact", "post_compact", "PreToolUse", "bogus"] {
            assert!(
                decode_grok_hook_payload(test_obj(&envelope(ev))).is_err(),
                "{ev} must bail"
            );
        }
    }

    const TRANSCRIPT: &str =
        "/home/u/.grok/sessions/%2Fhome%2Fu%2Fproj/0197fa30-sess/updates.jsonl";

    fn decode_line(v: Value) -> Vec<AgentEvent> {
        decode_grok_line(TRANSCRIPT, SOURCE_NAME, v)
    }

    fn acp_line(update: Value) -> Value {
        json!({"timestamp": 1721131200u64, "method": "session/update",
               "params": {"sessionId": "0197fa30-sess", "update": update}})
    }

    #[test]
    fn a_bare_spawn_subagent_call_is_task_detail_without_a_subagent_type_arg() {
        // `background: false` is what makes a spawn BLOCKING; the args carry no
        // `subagent_type`, so only the tool NAME can classify this one.
        assert_eq!(
            grok_tool_detail("spawn_subagent", Some(&json!({"background": false}))),
            ToolDetail::Task,
            "the tool NAME alone marks a blocking spawn"
        );
        assert_ne!(
            grok_tool_detail("read_file", Some(&json!({"background": false}))),
            ToolDetail::Task
        );
    }

    #[test]
    fn grok_content_never_supplies_a_cwd_so_the_path_deriver_still_runs() {
        // `walk.rs` combines these as `content.or_else(path_deriver)`, so a
        // `Some(empty)` here would SHORT-CIRCUIT `grok_cwd_from_path` and land
        // the slot on the unknown-cwd short reap.
        assert_eq!(extract_grok_cwd(&json!({"cwd": "/Users/dev/proj"})), None);
    }

    #[cfg(all(feature = "native", not(windows)))]
    #[test]
    fn a_drive_letter_group_dir_is_not_absolute_off_windows() {
        // `C:/proj` percent-encoded: only the `cfg!(windows)` arm may accept a
        // drive letter, so off Windows this must fall through to the `.cwd`
        // sibling rather than read as an absolute path.
        let dir = tempfile::tempdir().unwrap();
        let group = dir.path().join("C%3A%2Fproj");
        std::fs::create_dir_all(group.join("0197fa30-sess")).unwrap();
        assert_eq!(
            grok_cwd_from_path(&group.join("0197fa30-sess").join("updates.jsonl")),
            None,
            "no .cwd sibling and a non-absolute decode resolves nothing"
        );
    }

    fn xai_line(update: Value) -> Value {
        json!({"timestamp": 1721131200u64, "method": "_x.ai/session/update",
               "params": {"sessionId": "0197fa30-sess", "update": update,
                          "_meta": {"eventId": "s-1"}}})
    }

    #[test]
    fn fresh_tool_call_line_is_activity_start_keyed_by_path() {
        // A FRESH tool_call OMITS `status` (Pending is the ACP schema's serde
        // skip-default) and its `title` is the RAW tool name.
        let evs = decode_line(acp_line(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_42",
            "title": "run_terminal_command",
            "kind": "execute",
            "rawInput": {"command": "cat note.txt"}
        })));
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            AgentEvent::ActivityStart {
                agent_id,
                tool_use_id,
                detail,
            } => {
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts(SOURCE_NAME, "0197fa30-sess"),
                    "keyed by the PATH's parent-dir name, coalescing with hooks"
                );
                assert_eq!(tool_use_id.as_deref(), Some("call_42"));
                assert_eq!(
                    detail.as_ref().unwrap().display(),
                    "run_terminal_command",
                    "title IS the display (ACP carries no tool name)"
                );
            }
            other => panic!("expected ActivityStart, got {other:?}"),
        }
    }

    #[test]
    fn terminal_tool_call_updates_end_the_activity_others_do_not() {
        for status in ["completed", "failed"] {
            let evs = decode_line(acp_line(json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_42",
                "status": status
            })));
            assert!(
                matches!(&evs[..], [AgentEvent::ActivityEnd { tool_use_id: Some(id), .. }]
                    if id == "call_42"),
                "{status} must end call_42"
            );
        }
        for update in [
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c", "status": "in_progress"}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c",
                   "content": [{"type": "content"}]}),
        ] {
            assert!(decode_line(acp_line(update)).is_empty());
        }
    }

    #[test]
    fn transcript_blocking_spawn_is_task_background_is_not() {
        // In a live capture the transcript's rawInput carried the CLIENT-form
        // `background` key, not the struct's `run_in_background`.
        let blocking = decode_line(acp_line(json!({
            "sessionUpdate": "tool_call", "toolCallId": "call-0b8fe95b-2070-4e76-a5c7-036d4ad88f12-0",
            "title": "spawn_subagent",
            "rawInput": {"subagent_type": "general-purpose", "background": false,
                         "description": "Reply with single word", "prompt": "reply done"}
        })));
        assert!(
            matches!(&blocking[..], [AgentEvent::ActivityStart { detail: Some(d), .. }] if d.is_task())
        );
        let background = decode_line(acp_line(json!({
            "sessionUpdate": "tool_call", "toolCallId": "call-0b8fe95b-2070-4e76-a5c7-036d4ad88f12-1",
            "title": "spawn_subagent",
            "rawInput": {"subagent_type": "general-purpose", "background": true,
                         "description": "Reply with single word", "prompt": "reply done"}
        })));
        assert!(
            matches!(&background[..], [AgentEvent::ActivityStart { detail: Some(d), .. }] if !d.is_task()),
            "default (background) spawn must NOT be Task — b1 would evict the live child"
        );
    }

    /// Each xAI tag reaches a real arm. The bare-word probe is a control — an
    /// unread tag must reach neither arm.
    #[test]
    fn each_xai_tag_reaches_an_arm() {
        let reaches = |tag: &str| {
            let mut evs = 0;
            let logs = crate::test_capture::capture_logs(|| {
                evs = decode_line(xai_line(json!({
                    "sessionUpdate": tag,
                    "child_session_id": "c", "subagent_id": "s",
                    "event_name": "session_end", "runs": [],
                })))
                .len();
            });
            evs > 0 || logs.contains(crate::source::drift::TARGET)
        };
        for tag in [
            XAI_SUBAGENT_SPAWNED,
            XAI_SUBAGENT_FINISHED,
            XAI_TURN_COMPLETED,
            XAI_HOOK_EXECUTION,
        ] {
            assert!(reaches(tag), "{tag} must reach a real arm");
        }
        assert!(
            !reaches("checkpoint_saved"),
            "an unread tag reaches neither"
        );
    }

    #[test]
    fn turn_completed_settles_the_turn_to_idle() {
        let evs = decode_line(xai_line(json!({"sessionUpdate": "turn_completed"})));
        assert!(
            matches!(&evs[..], [AgentEvent::TurnEnd { .. }]),
            "got {evs:?}"
        );
    }

    #[test]
    fn message_chunks_plan_and_cosmetic_updates_decode_to_nothing() {
        for update in [
            json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "hi"}}),
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "yo"}}),
            json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hm"}}),
            json!({"sessionUpdate": "plan", "entries": []}),
            json!({"sessionUpdate": "available_commands_update", "availableCommands": []}),
        ] {
            assert!(decode_line(acp_line(update)).is_empty());
        }
        for update in [
            json!({"sessionUpdate": "rewind_marker"}),
            json!({"sessionUpdate": "diff_review"}),
            json!({"sessionUpdate": "task_backgrounded", "task_id": "t"}),
            json!({"sessionUpdate": "task_completed", "task_id": "t"}),
        ] {
            assert!(decode_line(xai_line(update)).is_empty());
        }
    }

    #[test]
    fn subagent_spawned_line_registers_child_under_parent() {
        // Fields are snake_case verbatim; rename_all covers only the tag.
        let evs = decode_line(xai_line(json!({
            "sessionUpdate": "subagent_spawned",
            "subagent_id": "0197fa31-child",
            "parent_session_id": "0197fa30-sess",
            "child_session_id": "0197fa31-child",
            "subagent_type": "general-purpose",
            "description": "Investigate the bug"
        })));
        assert_eq!(evs.len(), 2);
        match &evs[0] {
            AgentEvent::SessionStart {
                agent_id,
                parent_id,
                session_id,
                ..
            } => {
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts(SOURCE_NAME, "0197fa31-child")
                );
                assert_eq!(session_id, "0197fa31-child");
                assert_eq!(
                    *parent_id,
                    Some(AgentId::from_parts(SOURCE_NAME, "0197fa30-sess")),
                    "parent = the transcript's own (path-derived) id"
                );
            }
            other => panic!("expected child SessionStart, got {other:?}"),
        }
        assert!(matches!(&evs[1], AgentEvent::Rename { label, .. }
            if label == "Investigate the bug"));
    }

    #[test]
    fn transcript_subagent_arms_reject_an_empty_child_id_like_the_hook_twin() {
        for tag in ["subagent_spawned", "subagent_finished"] {
            let evs = decode_line(xai_line(json!({
                "sessionUpdate": tag,
                "child_session_id": "",
            })));
            assert!(
                evs.is_empty(),
                "{tag} with an empty child_session_id must decode to nothing, got {evs:?}"
            );
        }
    }

    /// `child_session_id` is required on every subagent update, so it alone keys
    /// the child (`crates/codegen/xai-grok-shell/src/extensions/notification.rs:679,753`
    /// at `37949780c1`): a line without it is not one grok writes.
    #[test]
    fn a_subagent_update_keys_its_child_by_child_session_id_alone() {
        for tag in ["subagent_spawned", "subagent_finished"] {
            let evs = decode_line(xai_line(json!({
                "sessionUpdate": tag,
                "subagent_id": "0197fa31-child",
            })));
            assert!(evs.is_empty(), "{tag}: {evs:?}");
        }
    }

    #[test]
    fn subagent_finished_line_ends_the_child_as_child() {
        let evs = decode_line(xai_line(json!({
            "sessionUpdate": "subagent_finished",
            "subagent_id": "0197fa31-child",
            "child_session_id": "0197fa31-child",
            "status": "completed",
            "tool_calls": 3, "turns": 2, "duration_ms": 4200
        })));
        assert!(
            matches!(&evs[..], [AgentEvent::SessionEnd { agent_id, as_child: true }]
                if *agent_id == AgentId::from_parts(SOURCE_NAME, "0197fa31-child"))
        );
    }

    #[test]
    fn hook_execution_session_end_is_the_persisted_end_marker() {
        // This line exists only because a SessionEnd hook is registered.
        let end = xai_line(json!({
            "sessionUpdate": "hook_execution",
            "event_name": "session_end",
            "runs": [{"name": "pixtuoid", "status": {"status": "success", "elapsedMs": 12}}]
        }));
        let evs = decode_line(end.clone());
        assert!(matches!(
            &evs[..],
            [AgentEvent::SessionEnd {
                as_child: false,
                ..
            }]
        ));
        for name in ["stop", "pre_tool_use"] {
            let evs = decode_line(xai_line(json!({
                "sessionUpdate": "hook_execution", "event_name": name, "runs": []
            })));
            assert!(
                evs.is_empty(),
                "hook_execution {name} must decode to nothing"
            );
        }
    }

    #[test]
    fn malformed_transcript_lines_never_panic_and_decode_to_nothing() {
        for v in [
            json!("just a string"),
            json!({"timestamp": 1}),
            json!({"method": "session/update"}),
            json!({"method": "session/update", "params": {"sessionId": "s"}}),
            json!({"method": "session/update", "params": {"update": "not an object"}}),
            json!({"method": "session/update", "params": {"update": {"noTag": true}}}),
            json!({"method": "bogus/method", "params": {"update": {"sessionUpdate": "tool_call"}}}),
        ] {
            assert!(decode_grok_line(TRANSCRIPT, SOURCE_NAME, v).is_empty());
        }
    }

    /// Nothing on grok's TRANSCRIPT axis breadcrumbs — not a new method, not a
    /// new ACP tag, not a per-token chunk. A name we never read is noise on both
    /// sides.
    #[test]
    fn no_transcript_line_breadcrumbs_however_new_its_method_or_tag() {
        for v in [
            json!({"timestamp": 1721131200u64, "method": "_x.ai/session/telemetry",
                   "params": {"sessionId": "s", "update": {"sessionUpdate": "beam"}}}),
            acp_line(json!({"sessionUpdate": "future_acp_capability_2027"})),
            acp_line(json!({"sessionUpdate": "agent_message_chunk"})),
            acp_line(json!({"sessionUpdate": "user_message_chunk"})),
            xai_line(json!({"sessionUpdate": "diff_review"})),
            xai_line(json!({"sessionUpdate": "rewind_marker_2027"})),
        ] {
            let quiet = crate::test_capture::capture_logs(|| {
                assert!(decode_line(v).is_empty(), "decodes to no events");
            });
            assert!(
                !quiet.contains(crate::source::drift::TARGET),
                "a transcript line we read nothing from reached the drift log:\n{quiet}"
            );
        }
    }

    #[cfg(feature = "native")]
    #[test]
    fn session_ended_checker_matches_only_the_structural_marker() {
        let end_line = serde_json::to_string(&xai_line(json!({
            "sessionUpdate": "hook_execution",
            "event_name": "session_end",
            "runs": []
        })))
        .unwrap();
        let stop_line = serde_json::to_string(&xai_line(json!({
            "sessionUpdate": "hook_execution",
            "event_name": "stop",
            "runs": []
        })))
        .unwrap();
        assert!(grok_session_ended(end_line.as_bytes()));
        assert!(!grok_session_ended(stop_line.as_bytes()));
        let tail = format!("{stop_line}\n{end_line}\n");
        assert!(grok_session_ended(tail.as_bytes()));
        // A torn leading line must not break the scan.
        let torn = format!("truncated-garbage}}\n{end_line}\n");
        assert!(grok_session_ended(torn.as_bytes()));
    }

    #[cfg(feature = "native")]
    #[test]
    fn session_ended_checker_is_immune_to_quoted_content() {
        let quoted = serde_json::to_string(&acp_line(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "c",
            "rawOutput": {"text":
                "{\"method\":\"_x.ai/session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"hook_execution\",\"event_name\":\"session_end\"}}}"}
        })))
        .unwrap();
        assert!(!grok_session_ended(quoted.as_bytes()));
    }

    #[test]
    fn id_is_the_parent_dir_name() {
        let p = Path::new("/home/u/.grok/sessions/%2Fhome%2Fu%2Fproj/0197fa30-sess/updates.jsonl");
        assert_eq!(grok_id_from_path(p), "0197fa30-sess");
    }

    #[cfg(feature = "native")]
    #[test]
    fn cwd_decodes_from_the_urlencoded_group_dir() {
        let p = Path::new(
            "/home/u/.grok/sessions/%2FUsers%2Fdev%2Fmy%20proj/0197fa30-sess/updates.jsonl",
        );
        assert_eq!(
            grok_cwd_from_path(p),
            Some(PathBuf::from("/Users/dev/my proj"))
        );
    }

    #[cfg(feature = "native")]
    #[test]
    fn cwd_slug_form_reads_the_dot_cwd_file() {
        // The >255-byte encoded form is `{slug}-{blake3_hex16}`, never absolute
        // after decoding, so upstream records the real cwd in a `.cwd` sibling.
        let tmp = tempfile::tempdir().unwrap();
        let group = tmp
            .path()
            .join("sessions")
            .join("deep-project-a1b2c3d4e5f60718");
        let session = group.join("0197fa30-sess");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(group.join(".cwd"), "/very/deep/project\n").unwrap();
        let p = session.join("updates.jsonl");
        assert_eq!(
            grok_cwd_from_path(&p),
            Some(PathBuf::from("/very/deep/project"))
        );
        std::fs::remove_file(group.join(".cwd")).unwrap();
        assert_eq!(grok_cwd_from_path(&p), None);
    }

    #[cfg(feature = "native")]
    #[test]
    fn percent_decode_handles_escapes_and_rejects_malformed() {
        assert_eq!(percent_decode("%2Fa%20b"), Some("/a b".into()));
        assert_eq!(percent_decode("plain"), Some("plain".into()));
        assert_eq!(percent_decode("a+b"), Some("a+b".into()));
        assert_eq!(percent_decode("%2"), None, "truncated escape");
        assert_eq!(percent_decode("%zz"), None, "non-hex escape");
        assert_eq!(percent_decode("%FF"), None, "invalid UTF-8 byte");
    }

    #[test]
    fn path_filter_admits_only_updates_jsonl() {
        let dir = Path::new("/h/.grok/sessions/%2Fr/0197-sess");
        assert!(is_updates_jsonl(&dir.join("updates.jsonl")));
        for sibling in [
            "chat_history.jsonl",
            "rewind_points.jsonl",
            "feedback.jsonl",
            "btw_history.jsonl",
            "prompt_history.jsonl",
        ] {
            assert!(
                !is_updates_jsonl(&dir.join(sibling)),
                "{sibling} must be filtered (rewrite-on-resume / not a session stream)"
            );
        }
    }
}
