//! Kimi Code CLI source — HOOK-ONLY (no transcript watcher).
//!
//! Session transcripts DO exist (`<KIMI_CODE_HOME>/sessions/<workDirKey>/
//! <sessionId>/agents/main/wire.jsonl`, default home `~/.kimi-code`) but the wire
//! format is EXPLICITLY unstable: a `WIRE_PROTOCOL_VERSION` in a first-line
//! `metadata` envelope, a `wire/migration/` module, docs saying *"do not manually
//! edit files inside `sessions/`"*, and undocumented version-specific op `type`
//! strings. So pixtuoid does NOT tail it. The seam is **hooks**
//! (`<KIMI_CODE_HOME>/config.toml` `[[hooks]]`), whose envelope is
//! CLAUDE-CODE-SHAPED: snake_case
//! `hook_event_name`/`session_id`/`cwd`/`tool_name`/`tool_input` with PascalCase
//! event VALUES — **except the per-call id, which Kimi spells `tool_call_id`**
//! (capture-verified: `fixtures/kimi/tool-run` never carries `tool_use_id`). That
//! is a registry row, `ToolIdKey::ToolCall`, and NOT a reason to re-implement the
//! tool arms here: the name is the whole difference, and a copy of those arms
//! would silently stop tracking whatever they learn next.
//!
//! So every CC-shaped event rides the SHARED arms
//! (`decoder::decode_hook_payload`) UNCHANGED, keyed on `session_id`; this
//! module's custom `Extend` decoder handles only Kimi's `PostToolUseFailure` /
//! `StopFailure` variants and DECLINES (`Ok(None)`) for everything else.
//!
//! **Subagents** (Kimi's "agent swarm") are deliberately NOT modeled: the
//! `SubagentStart`/`SubagentStop` payload shape is uncaptured, so sessions render
//! FLAT until a byte-real capture can add nesting.

use std::path::PathBuf;

use crate::source::decoder::{DecodeError, DecodeResult as Result};
use serde_json::{Map, Value};

use crate::AgentId;
use crate::source::registry::HookObject;
use crate::source::{AgentEvent, registry};

/// The Kimi CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "kimi";

/// Decode Kimi's two failure-variant hook events (no CC equivalent) and DECLINE
/// (`Ok(None)`) for the rest, so the CC-shaped remainder falls through to
/// `decoder::decode_hook_payload`'s shared arms.
///
/// - `PostToolUseFailure` — a FAILED tool fires this beside/instead of
///   `PostToolUse`; it must close the activity or the tool's `ToolUseStart`
///   lingers Active. Firing BOTH is harmless: the second `ToolUseEnd` no-ops
///   on an already-idle slot.
/// - `StopFailure` — a failed turn end; settle to idle like `Stop`.
///   Identity-less: an end alone proves too little to label a session, so an
///   unknown id registers blank (`shows_activity`).
pub(crate) fn decode_kimi_hook_custom(obj: &HookObject) -> Result<Option<Vec<AgentEvent>>> {
    match obj.get("hook_event_name").and_then(|s| s.as_str()) {
        Some("PostToolUseFailure") => {
            let session_id = kimi_session_id(obj)?;
            let agent_id = AgentId::from_parts(SOURCE_NAME, &session_id);
            let cwd = kimi_cwd(obj);
            let tool_use_id = kimi_tool_call_id(obj);
            Ok(Some(vec![
                AgentEvent::identity(agent_id, SOURCE_NAME, session_id, cwd),
                AgentEvent::ToolUseEnd {
                    agent_id,
                    tool_use_id,
                },
            ]))
        }
        Some("StopFailure") => {
            let agent_id = AgentId::from_parts(SOURCE_NAME, &kimi_session_id(obj)?);
            Ok(Some(vec![AgentEvent::turn_end(agent_id)]))
        }
        _ => Ok(None),
    }
}

/// Kimi's per-call id, read through the registry variant the shared arms read —
/// a second copy of the string here is exactly the drift that made the id
/// decode to `None` for this whole source.
fn kimi_tool_call_id(obj: &Map<String, Value>) -> Option<String> {
    obj.get(registry::ToolIdKey::ToolCall.wire_name())
        .and_then(|s| s.as_str())
        .map(String::from)
}

fn kimi_cwd(obj: &Map<String, Value>) -> Option<PathBuf> {
    obj.get("cwd")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// The non-empty `session_id` a claimed failure event keys on — the SAME key the
/// shared arms derive, so a failure event coalesces with the session's other
/// events. `Err` rather than `Ok(None)` on a missing/empty id: having claimed
/// the event, falling through would hit the shared arms' "unsupported
/// hook_event_name" bail with a misleading message.
fn kimi_session_id(obj: &Map<String, Value>) -> Result<String> {
    obj.get("session_id")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| DecodeError::missing_in(SOURCE_NAME, "failure-hook", "session_id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;
    use serde_json::json;

    #[test]
    fn post_tool_use_failure_closes_activity_with_identity() {
        let evs = decode_kimi_hook_custom(test_obj(&json!({
            "hook_event_name": "PostToolUseFailure",
            "session_id": "session_abc",
            "cwd": "/Users/dev/proj",
            "tool_name": "Bash",
            "tool_call_id": "t1"
        })))
        .expect("decodes")
        .expect("claims the event");
        assert_eq!(evs.len(), 2, "Identity + ToolUseEnd, got {evs:?}");
        match &evs[0] {
            AgentEvent::Identity {
                agent_id,
                source,
                session_id,
                cwd,
                pid: None,
            } => {
                assert_eq!(*agent_id, AgentId::from_parts(SOURCE_NAME, "session_abc"));
                assert_eq!(source, SOURCE_NAME);
                assert_eq!(session_id, "session_abc");
                assert_eq!(
                    cwd.as_deref(),
                    Some(std::path::Path::new("/Users/dev/proj"))
                );
            }
            other => panic!("expected leading Identity, got {other:?}"),
        }
        match &evs[1] {
            AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id,
            } => {
                assert_eq!(*agent_id, AgentId::from_parts(SOURCE_NAME, "session_abc"));
                assert_eq!(tool_use_id.as_deref(), Some("t1"));
            }
            other => panic!("expected ToolUseEnd, got {other:?}"),
        }
    }

    #[test]
    fn post_tool_use_failure_without_cwd_has_none_cwd() {
        let evs = decode_kimi_hook_custom(test_obj(&json!({
            "hook_event_name": "PostToolUseFailure",
            "session_id": "s",
            "cwd": "",
            "tool_name": "Read"
        })))
        .unwrap()
        .unwrap();
        match &evs[0] {
            AgentEvent::Identity { cwd, .. } => {
                assert_eq!(*cwd, None, "empty cwd must map to None, not Some(\"\")")
            }
            other => panic!("expected Identity, got {other:?}"),
        }
    }

    #[test]
    fn stop_failure_is_a_turn_end_without_identity() {
        let evs = decode_kimi_hook_custom(test_obj(&json!({
            "hook_event_name": "StopFailure",
            "session_id": "s"
        })))
        .unwrap()
        .unwrap();
        assert_eq!(evs.len(), 1);
        assert!(
            matches!(&evs[0], AgentEvent::TurnEnd { agent_id }
                if *agent_id == AgentId::from_parts(SOURCE_NAME, "s")),
            "got {evs:?}"
        );
    }

    #[test]
    fn non_failure_events_decline_to_the_shared_arms() {
        // SubagentStart is deliberately UNregistered — included to pin that the
        // custom decoder declines it too.
        for ev in [
            "SessionStart",
            "PreToolUse",
            "PostToolUse",
            "PermissionRequest",
            "Stop",
            "SessionEnd",
            "UserPromptSubmit",
            "SubagentStart",
        ] {
            let out = decode_kimi_hook_custom(test_obj(&json!({
                "hook_event_name": ev,
                "session_id": "s",
                "cwd": "/repo"
            })))
            .expect("decodes");
            assert!(
                out.is_none(),
                "{ev} must be declined (Ok(None)) by the custom decoder, got {out:?}"
            );
        }
    }

    #[test]
    fn claimed_failure_event_without_session_id_errs() {
        for ev in ["PostToolUseFailure", "StopFailure"] {
            assert!(
                decode_kimi_hook_custom(test_obj(
                    &json!({ "hook_event_name": ev, "cwd": "/repo" })
                ))
                .is_err(),
                "{ev} with no session_id must err"
            );
            assert!(
                decode_kimi_hook_custom(test_obj(
                    &json!({ "hook_event_name": ev, "session_id": "" })
                ))
                .is_err(),
                "{ev} with empty session_id must err"
            );
        }
    }

    /// The negative control for kimi's `ToolIdKey::ToolCall` registry row: the CC
    /// name kimi never sends must NOT be read, or the row would be decorative.
    #[test]
    fn the_cc_spelling_is_not_read_for_kimi() {
        let evs = crate::source::decoder::decode_hook_payload(json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session_abc",
            "cwd": "/w",
            "tool_name": "Read",
            "tool_use_id": "call_1",
            "_pixtuoid_source": SOURCE_NAME,
        }))
        .expect("decodes");
        assert!(
            evs.iter().any(|e| matches!(
                e,
                AgentEvent::ToolUseStart {
                    tool_use_id: None,
                    ..
                }
            )),
            "kimi must ignore tool_use_id, got {evs:?}"
        );
    }
}
