//! Cursor CLI source — HOOK-ONLY (no JSONL watcher).
//!
//! Only one of Cursor's three seams is reachable by a passive observer:
//! `--output-format stream-json` is stdout of an invocation we would have to
//! launch, and on-disk sessions are SQLite hex blobs. So **Cursor Hooks**
//! (`~/.cursor/hooks.json`) is the seam. A per-session JSONL transcript does
//! exist and the envelope carries its `transcript_path` (`null` on
//! `sessionStart`, set from the first tool event on) — the seam if a watcher is
//! ever wanted.
//!
//! The envelope reuses CC's `hook_event_name` field NAME with **camelCase
//! values**; `tool_name` is PascalCase and the TOP-LEVEL `cwd` is empty —
//! `workspace_roots[0]` is the real workspace. Keyed on `session_id`, present
//! and CONSISTENT across every CLI event, so concurrent sessions in one project
//! stay distinct.
//!
//! - **cursor also runs Claude Code's hooks** (`~/.claude/settings.json` and the
//!   project's, on by default: cursor.com/docs/reference/third-party-hooks), so a
//!   user who connected both CLIs gets each cursor event again through CC's shim,
//!   stamped `claude-code`, or unstamped by an install without the env prefix.
//!   `decoder`'s cross-fire guard drops every one on `cursor_version`. Lossless —
//!   the `cursor`-stamped copy carries the whole arc.
//! - **A child is its own session, linked late.** pixtuoid registers no
//!   subagent hook (`CURSOR_EVENTS`) and a child fires no `sessionStart`
//!   (`cursor/fixtures/delegation`), so it registers on its first tool event.
//!   A hook's `transcript_path` is the first of the child's transcripts that
//!   exists when the hook runs, else null; once cursor-agent records the
//!   child's parent (`3863.index.js:1` `setTranscriptParentConversationId`),
//!   the first is `<parent>/subagents/<child>.jsonl` (cursor-agent
//!   2026.10.22: `1841.index.js:1` `getTranscriptPathIfExists`,
//!   `7923.index.js:1` `agent-transcript/dist/paths.js`). That path links the
//!   child (`nested_parent`), and its parent's `Task` post then ends it; the
//!   committed delegation's child posts carry null, so its child ages out.
//! - `sessionEnd` FIRES on clean completion; `stop` is turn-end and did NOT fire
//!   under `-p`. An abrupt exit rides the shim's `_pid` BEST-EFFORT: the walk
//!   skips the `$SHELL -c` wrapper Cursor `eval`s hooks inside, but some hooks
//!   arrive through a different non-shell ancestor, so the pid is not stable and
//!   corroboration often withholds the arm (#896).

use crate::source::decoder::{DecodeError, DecodeResult as Result};
use serde_json::Value;

use crate::AgentId;
use crate::source::decoder::{MAX_DECODED_FIELD_CHARS, SUBAGENTS_DIR, ellipsize, non_empty};
use crate::source::registry::HookObject;
use crate::source::{AgentEvent, ToolDetail};

/// The Cursor CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "cursor";

/// Decode one Cursor hook payload (already identified by
/// `_pixtuoid_source == "cursor"`). Envelope per `cursor.com/docs/hooks`; an
/// unregistered event bails, so registered-vs-decoded drift is loud.
///
/// The activity arms prepend an [`AgentEvent::Identity`] (#221) because Cursor
/// is HOOK-ONLY: a slot the reducer's proof-of-life pre-pass synthesizes
/// mid-turn has no JSONL back-fill path, so without the attached identity it
/// would stay a blank `#N` ghost.
///
/// # Errors
///
/// If the payload lacks `hook_event_name` or `session_id`, or names an unregistered event.
pub(crate) fn decode_cursor_hook_payload(obj: &HookObject) -> Result<Vec<AgentEvent>> {
    let event = obj
        .get("hook_event_name")
        .and_then(|s| s.as_str())
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "hook_event_name"))?;
    // The top-level `cwd` is EMPTY/absent in CLI hook payloads —
    // `workspace_roots[0]` is the real one. Label/cwd only, NOT the AgentId key.
    let workspace = obj
        .get("cwd")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            obj.get("workspace_roots")
                .and_then(|r| r.as_array())
                .and_then(|a| a.first())
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
        });
    // Key on `session_id` — present and CONSISTENT across every CLI hook event,
    // so it distinguishes concurrent sessions in one project AND coalesces all of
    // a session's events.
    let key = obj
        .get("session_id")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "session_id"))?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, key);
    let cwd = workspace.unwrap_or("");

    let identity = || {
        AgentEvent::identity(
            agent_id,
            SOURCE_NAME,
            key,
            (!cwd.is_empty()).then(|| cwd.into()),
        )
    };

    let decoded: Result<Vec<AgentEvent>> = match event {
        "sessionStart" => Ok(vec![AgentEvent::SessionStart {
            agent_id,
            source: SOURCE_NAME.to_string(),
            session_id: key.to_string(),
            cwd: cwd.into(),
            parent_id: None,
        }]),
        "preToolUse" => {
            let tool = obj
                .get("tool_name")
                .and_then(|s| s.as_str())
                .unwrap_or_else(|| {
                    crate::source::drift::missing_field(SOURCE_NAME, "preToolUse", "tool_name");
                    "?"
                });
            Ok(vec![
                identity(),
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id: non_empty(obj, "tool_use_id").map(String::from),
                    detail: Some(cursor_tool_detail(tool, obj.get("tool_input"))),
                },
            ])
        }
        // A FAILED tool fires `postToolUseFailure` INSTEAD OF `postToolUse`.
        // Without this arm a failed tool's ToolUseStart never ends under `-p`
        // (where `stop` doesn't fire) and the sprite lingers Running.
        "postToolUse" | "postToolUseFailure" => Ok(nested_parent(obj)
            .map(|parent| AgentEvent::SessionStart {
                agent_id,
                source: SOURCE_NAME.to_string(),
                session_id: key.to_string(),
                cwd: cwd.into(),
                parent_id: Some(AgentId::from_parts(SOURCE_NAME, parent)),
            })
            .into_iter()
            .chain([
                identity(),
                AgentEvent::ToolUseEnd {
                    agent_id,
                    tool_use_id: non_empty(obj, "tool_use_id").map(String::from),
                },
            ])
            .collect()),
        // Identity-less: an end alone proves too little to label a session, so
        // an unknown id registers blank (`shows_activity`).
        "stop" => Ok(vec![AgentEvent::turn_end(agent_id)]),
        "sessionEnd" => Ok(vec![AgentEvent::SessionEnd {
            agent_id,
            as_child: false,
        }]),
        other => {
            crate::source::drift::unknown_event(SOURCE_NAME, other);
            Err(DecodeError::unsupported(SOURCE_NAME, other))
        }
    };
    let mut evs = decoded?;
    // Re-read per event: the model changes mid-session, so reading it once
    // would show the wrong one for the rest of the turn.
    if let Some(model) = obj
        .get("model")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
    {
        evs.push(AgentEvent::ModelInfo {
            agent_id,
            model: Some(ellipsize(model, MAX_DECODED_FIELD_CHARS)),
            effort: None,
        });
    }
    Ok(evs)
}

fn cursor_tool_detail(tool: &str, args: Option<&Value>) -> ToolDetail {
    // Cursor's `Task` tool carries a `subagent_type` (capture-verified) — the
    // SAME stable semantic signal CC's `make_tool_detail` keys on. A child links
    // to its parent late if at all (module doc), so the parent's `Task` is what
    // renders the delegation.
    let has_subagent_type = args.and_then(|a| a.get("subagent_type")).is_some();
    if tool == "Task" || has_subagent_type {
        return ToolDetail::Task;
    }
    const KEYS: &[&str] = &["command", "file_path", "path", "pattern", "url"];
    crate::source::decoder::generic_keyed_detail(tool, args, KEYS)
}

/// The session a subagent's `transcript_path` nests under,
/// `<parent>/subagents/<child>.jsonl` (module doc).
fn nested_parent(obj: &HookObject) -> Option<&str> {
    let dir = std::path::Path::new(obj.get("transcript_path")?.as_str()?).parent()?;
    if dir.file_name()? != SUBAGENTS_DIR {
        return None;
    }
    dir.parent()?.file_name()?.to_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;
    use serde_json::json;

    fn decode_all(v: Value) -> Vec<AgentEvent> {
        decode_cursor_hook_payload(test_obj(&v)).expect("decodes")
    }

    /// The payload's MAIN event — the LAST one (activity arms prepend an Identity).
    fn decode(v: Value) -> AgentEvent {
        decode_all(v).pop().expect("at least one event")
    }

    #[test]
    fn every_event_carries_the_model_and_it_can_change_mid_session() {
        // Real wire shape: a session opened on the `-fast` composer while its
        // tool events ran on the full one.
        let start = decode_all(json!({
            "hook_event_name": "sessionStart", "session_id": "s1",
            "workspace_roots": ["/repo"], "model": "composer-2.5-fast"
        }));
        let tool = decode_all(json!({
            "hook_event_name": "preToolUse", "session_id": "s1",
            "workspace_roots": ["/repo"], "tool_name": "read", "model": "composer-2.5"
        }));
        let model_of = |evs: &[AgentEvent]| {
            evs.iter().find_map(|e| match e {
                AgentEvent::ModelInfo { model, .. } => model.clone(),
                _ => None,
            })
        };
        assert_eq!(model_of(&start).as_deref(), Some("composer-2.5-fast"));
        assert_eq!(model_of(&tool).as_deref(), Some("composer-2.5"));
    }

    #[test]
    fn session_id_distinguishes_two_sessions_in_one_workspace() {
        let a = decode(
            json!({"hook_event_name": "sessionStart", "session_id": "sess-A",
                              "workspace_roots": ["/repo"]}),
        );
        let b = decode(
            json!({"hook_event_name": "sessionStart", "session_id": "sess-B",
                              "workspace_roots": ["/repo"]}),
        );
        assert_ne!(
            a.agent_id(),
            b.agent_id(),
            "two sessions in one repo must be distinct"
        );
    }

    /// A child's transcript nests under its parent's, which is the one place
    /// the wire names the parent: the child's tool events carry the path.
    #[test]
    fn a_tool_event_from_a_nested_transcript_links_its_session_to_the_parent() {
        let child = |path: Value| {
            decode_all(json!({
                "hook_event_name": "postToolUse", "session_id": "child",
                "workspace_roots": ["/repo"], "tool_use_id": "t",
                "transcript_path": path,
            }))
        };
        let linked = child(json!(
            "/h/.cursor/projects/p/agent-transcripts/parent/subagents/child.jsonl"
        ));
        assert!(
            matches!(
                linked.first(),
                Some(AgentEvent::SessionStart { agent_id, parent_id: Some(p), .. })
                    if *agent_id == AgentId::from_parts(SOURCE_NAME, "child")
                        && *p == AgentId::from_parts(SOURCE_NAME, "parent")
            ),
            "{linked:?}"
        );
        for unnested in [
            json!("/h/.cursor/projects/p/agent-transcripts/child/child.jsonl"),
            json!(null),
        ] {
            assert!(
                !child(unnested.clone())
                    .iter()
                    .any(|e| matches!(e, AgentEvent::SessionStart { .. })),
                "{unnested}"
            );
        }
    }

    /// The wire's `tool_use_id` pairs a start with its post.
    #[test]
    fn a_start_and_its_post_keep_their_tool_use_id() {
        const ID: &str = "tool_b3a89c28-dbe4-487e-8c70-ea05a83083a";
        for (event, kept) in [
            ("preToolUse", Some(ID)),
            ("postToolUse", Some(ID)),
            ("postToolUseFailure", Some(ID)),
        ] {
            let ev = decode(json!({
                "hook_event_name": event, "session_id": "s", "workspace_roots": ["/repo"],
                "tool_name": "Shell", "tool_input": {"command": "ls"},
                "tool_use_id": ID
            }));
            let tuid = match &ev {
                AgentEvent::ToolUseStart { tool_use_id, .. }
                | AgentEvent::ToolUseEnd { tool_use_id, .. } => tool_use_id.clone(),
                other => panic!("{event}: expected an activity event, got {other:?}"),
            };
            assert_eq!(tuid.as_deref(), kept, "{event}");
        }
    }

    #[test]
    fn task_dispatch_with_subagent_type_is_delegating() {
        let ev = decode(json!({
            "hook_event_name": "preToolUse",
            "session_id": "parent",
            "workspace_roots": ["/repo"],
            "tool_name": "Task",
            "tool_input": {"subagent_type": "code-explorer", "description": "investigate the build"}
        }));
        assert!(
            matches!(&ev, AgentEvent::ToolUseStart { detail: Some(d), .. } if d.is_task()),
            "Task + subagent_type must map to ToolDetail::Task, got {ev:?}"
        );
        let read = decode(json!({
            "hook_event_name": "preToolUse", "session_id": "p", "workspace_roots": ["/r"],
            "tool_name": "Read", "tool_input": {"file_path": "/r/x.rs"}
        }));
        assert!(matches!(&read, AgentEvent::ToolUseStart { detail: Some(d), .. } if !d.is_task()));
        let bare_task = decode(json!({
            "hook_event_name": "preToolUse", "session_id": "p", "workspace_roots": ["/r"],
            "tool_name": "Task"
        }));
        assert!(
            matches!(&bare_task, AgentEvent::ToolUseStart { detail: Some(d), .. } if d.is_task()),
            "an input-less Task dispatch must still read as Delegating, got {bare_task:?}"
        );
        let renamed = decode(json!({
            "hook_event_name": "preToolUse", "session_id": "p", "workspace_roots": ["/r"],
            "tool_name": "Delegate", "tool_input": {"subagent_type": "code-explorer"}
        }));
        assert!(
            matches!(&renamed, AgentEvent::ToolUseStart { detail: Some(d), .. } if d.is_task()),
            "the semantic field must catch a renamed dispatch, got {renamed:?}"
        );
    }

    #[test]
    fn tool_target_uses_cursor_arg_vocabulary() {
        let shell = decode(json!({
            "hook_event_name": "preToolUse", "session_id": "s", "cwd": "/r",
            "tool_name": "shell", "tool_input": {"command": "cargo test"}
        }));
        assert!(
            matches!(shell, AgentEvent::ToolUseStart { detail: Some(d), .. }
            if d.display() == "shell: cargo test")
        );
        let edit = decode(json!({
            "hook_event_name": "preToolUse", "session_id": "s", "cwd": "/r",
            "tool_name": "edit", "tool_input": {"file_path": "src/lib.rs"}
        }));
        assert!(
            matches!(edit, AgentEvent::ToolUseStart { detail: Some(d), .. }
            if d.display() == "edit: src/lib.rs")
        );
    }

    #[test]
    fn post_tool_use_without_an_id_is_an_id_less_end_and_stop_a_turn_end() {
        for event in ["postToolUse", "postToolUseFailure"] {
            let ev = decode(
                json!({"hook_event_name": event, "session_id": "s", "cwd": "/r",
                                   "tool_use_id": ""}),
            );
            assert!(
                matches!(
                    &ev,
                    AgentEvent::ToolUseEnd {
                        tool_use_id: None,
                        ..
                    }
                ),
                "{event} must decode to ToolUseEnd with no tool id"
            );
        }
        let evs = decode_all(json!({"hook_event_name": "stop", "session_id": "s", "cwd": "/r"}));
        assert!(
            matches!(evs.as_slice(), [AgentEvent::TurnEnd { .. }]),
            "got {evs:?}"
        );
    }

    #[test]
    fn all_events_for_one_session_share_one_agent_id() {
        let sid = "c7cef226-sess";
        let events = [
            json!({"hook_event_name": "sessionStart", "session_id": sid, "workspace_roots": ["/repo"]}),
            json!({"hook_event_name": "preToolUse", "session_id": sid, "cwd": "", "workspace_roots": ["/repo"],
                   "tool_name": "Shell", "tool_input": {"command": "ls"}}),
            json!({"hook_event_name": "postToolUse", "session_id": sid, "workspace_roots": ["/repo"], "tool_name": "Shell"}),
            json!({"hook_event_name": "stop", "session_id": sid, "workspace_roots": ["/repo"]}),
            json!({"hook_event_name": "sessionEnd", "session_id": sid, "reason": "completed", "workspace_roots": ["/repo"]}),
        ];
        let ids: std::collections::BTreeSet<_> = events
            .iter()
            .flat_map(|v| decode_cursor_hook_payload(test_obj(v)).unwrap())
            .map(|e| e.agent_id())
            .collect();
        assert_eq!(ids.len(), 1, "all events must coalesce to one AgentId");
    }

    #[test]
    fn stop_session_events_and_session_end_carry_no_identity() {
        for payload in [
            json!({"hook_event_name": "stop", "session_id": "s", "cwd": "/r"}),
            json!({"hook_event_name": "sessionStart", "session_id": "s", "cwd": "/r"}),
            json!({"hook_event_name": "sessionEnd", "session_id": "s", "cwd": "/r"}),
        ] {
            let name = payload["hook_event_name"].clone();
            let events = decode_all(payload);
            assert_eq!(events.len(), 1, "{name}: exactly one event");
            assert!(
                !matches!(events[0], AgentEvent::Identity { .. }),
                "{name} must not emit Identity"
            );
        }
    }

    #[test]
    fn no_session_id_is_malformed_but_session_id_alone_is_ok() {
        assert!(decode_cursor_hook_payload(test_obj(&json!({"hook_event_name": "stop"}))).is_err());
        assert!(
            decode_cursor_hook_payload(test_obj(
                &json!({"hook_event_name": "sessionStart", "workspace_roots": ["/repo"]})
            ))
            .is_err(),
            "a workspace is a label, never a session's key"
        );
        assert!(
            decode_cursor_hook_payload(test_obj(
                &json!({"hook_event_name": "stop", "session_id": "s"})
            ))
            .is_ok()
        );
    }

    #[test]
    fn unknown_event_bails_loudly() {
        // subagentStart/Stop are deliberately unregistered — they do not fire.
        for ev in [
            "subagentStart",
            "subagentStop",
            "beforeShellExecution",
            "Bogus",
        ] {
            assert!(
                decode_cursor_hook_payload(test_obj(&json!({"hook_event_name": ev, "cwd": "/r"})))
                    .is_err(),
                "{ev} must bail (not registered, must not decode silently)"
            );
        }
    }

    #[test]
    fn pre_tool_use_without_tool_name_displays_question_mark() {
        let ev = decode(json!({"hook_event_name": "preToolUse", "session_id": "s", "cwd": "/r"}));
        assert!(
            matches!(ev, AgentEvent::ToolUseStart { detail: Some(d), .. }
            if d.display() == "?")
        );
    }
}
