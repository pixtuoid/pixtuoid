use crate::source::decoder::DecodeResult as Result;
use serde_json::Value;
use std::path::Path;

use crate::AgentId;
use crate::source::decoder::{DecodeError, EVENT_STAMP, first_present_str, generic_tool_display};
use crate::source::registry::HookObject;
use crate::source::{AgentEvent, RequestKeys};

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::AntigravitySource;

/// The Antigravity CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "antigravity";

/// The CLI's app data dir, under [`gemini_dir`]: NOT `antigravity`, the App's,
/// which shares the global hooks file but is not this source.
pub const CLI_APP_DATA_DIR: &str = "antigravity-cli";

/// agy's `.gemini` dir, home-rooted on every platform (Go's `os.UserHomeDir`,
/// never `%APPDATA%`): its app data dir and its global `config/` both sit here.
#[cfg(feature = "native")]
pub fn gemini_dir() -> std::path::PathBuf {
    crate::platform::user_home().join(".gemini")
}

/// Antigravity-cli writes one conversation's `step_index` stream several times
/// over: `transcript.jsonl` (truncated), `transcript_full.jsonl` (untruncated)
/// and `chunks/<kind>/<n>.jsonl` pieces. Walking each would decode every step
/// once per copy, so only `transcript.jsonl` is watched; the decoder ignores
/// content length, so the others lose nothing.
pub(crate) fn admits_transcript(path: &Path) -> bool {
    path.file_name().and_then(|s| s.to_str()) == Some(TRANSCRIPT)
}

const TRANSCRIPT: &str = "transcript.jsonl";

/// The session id = the conversation's dir, `<app_data_dir>/brain/<conversationId>/
/// .system_generated/logs/` (<https://antigravity.google/docs/hooks.md>,
/// `transcriptPath`): the id a hook names as `conversationId`, whichever log
/// file its `transcriptPath` points at.
pub(crate) fn ag_id_from_path(path: &Path) -> String {
    path.ancestors()
        .nth(3)
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// The one tool-call id both transports mint: the step the call runs as, which a
/// hook names as `stepIdx` and the call's result step carries as `step_index`.
fn step_tool_id(step: i64) -> String {
    format!("ag-{step}")
}

/// Decode one Antigravity CLI transcript line into `AgentEvent`s (the step_index / tool_calls JSONL schema).
pub fn decode_ag_line(transcript_path: &str, source: &str, v: Value) -> Vec<AgentEvent> {
    let agent_id = AgentId::from_parts(source, &ag_id_from_path(Path::new(transcript_path)));
    let Some(obj) = v.as_object() else {
        return vec![];
    };

    // A present-but-non-integer OR negative `step_index` must skip the line: a
    // negative would mint an id no result step can end, leaving the slot stuck
    // Running until the reducer's stale-sweep.
    let Some(step_index) = obj
        .get("step_index")
        .and_then(|v| v.as_i64())
        .filter(|&s| s >= 0)
    else {
        return vec![];
    };
    let step_type = obj.get("type").and_then(|s| s.as_str()).unwrap_or("");

    let mut out = Vec::new();

    if step_type == "PLANNER_RESPONSE" {
        if let Some(Value::Array(tool_calls)) = obj.get("tool_calls") {
            // agy runs a step's calls in order, each as the next step: two calls
            // at step 1 ran as steps 2 and 3, the hook's `stepIdx` naming each.
            for (step, tc) in (step_index + 1..).zip(tool_calls) {
                let Some(tc_obj) = tc.as_object() else {
                    continue;
                };
                let name = tc_obj
                    .get("name")
                    .and_then(|s| s.as_str())
                    .unwrap_or_else(|| {
                        crate::source::drift::missing_field(
                            SOURCE_NAME,
                            "PLANNER_RESPONSE",
                            "name",
                        );
                        "?"
                    });
                let args = tc_obj.get("args");
                out.push(decode_ag_tool_call(
                    agent_id,
                    name,
                    args,
                    Some(step_tool_id(step)),
                ));
            }
        }
    } else {
        // Step lines have no fetchable schema, so a breadcrumb and the hermetic
        // replay are their currency backstop. A step carrying `tool_calls` under a
        // type that ISN'T `PLANNER_RESPONSE` is the signal that upstream RENAMED
        // that step. Result/input steps carry no `tool_calls`, so this can't
        // false-positive on them.
        if matches!(obj.get("tool_calls"), Some(Value::Array(a)) if !a.is_empty()) {
            crate::source::drift::unknown_event(SOURCE_NAME, step_type);
        }
        if step_type != "USER_INPUT" && step_type != "CONVERSATION_HISTORY" && step_index > 0 {
            out.push(AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id: Some(step_tool_id(step_index)),
            });
        }
    }

    out
}

/// Decode one tool call, from either transport: a permission/question prompt
/// becomes [`AgentEvent::UserActionRequest`], anything else a `ToolUseStart`, each keyed `tool_use_id`.
fn decode_ag_tool_call(
    agent_id: AgentId,
    name: &str,
    args: Option<&Value>,
    tool_use_id: Option<String>,
) -> AgentEvent {
    // The tools that wait on the human (hooks.md, "Supported tools").
    if name == "ask_permission" || name == "ask_question" {
        return AgentEvent::user_action_request(
            agent_id,
            Some("asking permission".to_string()),
            RequestKeys {
                tool_use_id,
                ..RequestKeys::default()
            },
        );
    }
    let target = ag_tool_target(args);
    AgentEvent::ToolUseStart {
        agent_id,
        tool_use_id,
        detail: Some(generic_tool_display(name, target.as_deref())),
    }
}

/// Decode one agy hook payload, its event named by [`EVENT_STAMP`]: agy names
/// none in its payload.
///
/// # Errors
///
/// If the payload lacks the event stamp or `conversationId`,
/// or fires an event the installer never registers.
pub(crate) fn decode_ag_hook_payload(obj: &HookObject) -> Result<Vec<AgentEvent>> {
    let event = obj
        .get(EVENT_STAMP)
        .and_then(Value::as_str)
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, EVENT_STAMP))?;
    // The App and IDE fire the same global hooks file under app data dirs of
    // their own (hooks.md, `transcriptPath`).
    if obj
        .get("transcriptPath")
        .and_then(Value::as_str)
        .is_some_and(|p| {
            !p.is_empty()
                && !Path::new(p)
                    .components()
                    .any(|c| c.as_os_str() == CLI_APP_DATA_DIR)
        })
    {
        tracing::trace!(event, "dropping a hook another Antigravity app fired");
        return Ok(vec![]);
    }
    let key = obj
        .get("conversationId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            crate::source::drift::missing_field(SOURCE_NAME, event, "conversationId");
            DecodeError::missing_in(SOURCE_NAME, event, "conversationId")
        })?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, key);
    let cwd = obj
        .get("workspacePaths")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            crate::source::drift::missing_field(SOURCE_NAME, event, "workspacePaths");
            ""
        });
    let identity = || {
        AgentEvent::identity(
            agent_id,
            SOURCE_NAME,
            key,
            (!cwd.is_empty()).then(|| cwd.into()),
        )
    };
    let call = || {
        let step = obj.get("stepIdx").and_then(Value::as_i64);
        if step.is_none() {
            crate::source::drift::missing_field(SOURCE_NAME, event, "stepIdx");
        }
        (step.map(step_tool_id), obj.get("toolCall"))
    };
    match event {
        // The focus pid rides an `Identity`, so a session that never calls a
        // tool still names it.
        "SessionStart" => Ok(vec![
            AgentEvent::SessionStart {
                agent_id,
                source: SOURCE_NAME.to_string(),
                session_id: key.to_string(),
                cwd: cwd.into(),
                parent_id: None,
            },
            identity(),
        ]),
        "PreToolUse" => {
            let (tool_use_id, tool) = call();
            let name = tool
                .and_then(|t| t.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    crate::source::drift::missing_field(SOURCE_NAME, event, "toolCall.name");
                    "?"
                });
            let args = tool.and_then(|t| t.get("args"));
            Ok(vec![
                identity(),
                decode_ag_tool_call(agent_id, name, args, tool_use_id),
            ])
        }
        "PostToolUse" => Ok(vec![
            identity(),
            AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id: call().0,
            },
        ]),
        // Turn end: the session lives on to the next prompt.
        "Stop" => Ok(vec![AgentEvent::turn_end(agent_id)]),
        other => {
            crate::source::drift::unknown_event(SOURCE_NAME, other);
            Err(DecodeError::unsupported(SOURCE_NAME, other))
        }
    }
}

/// The first present path/command field of an Antigravity tool call's `args`,
/// quote-stripped — the `: target` half of the Generic display.
fn ag_tool_target(args: Option<&Value>) -> Option<String> {
    // Priority order. `Path` is `read_file`'s, `write_file`'s and `edit_file`'s
    // in 1.4.8's tool declarations, as its request to the model lists them.
    const KEYS: &[&str] = &[
        "DirectoryPath",
        "Path",
        "CommandLine",
        "SearchPath",
        "query",
    ];
    let raw = first_present_str(args?, KEYS)?;
    let clean = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    Some(clean.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;

    #[test]
    fn negative_step_index_is_skipped_not_minted() {
        let v = serde_json::json!({
            "type": "PLANNER_RESPONSE",
            "step_index": -1,
            "tool_calls": [ { "name": "read_file", "args": {} } ],
        });
        let out = decode_ag_line("/x/t.jsonl", SOURCE_NAME, v);
        assert!(
            out.is_empty(),
            "negative step_index must emit nothing: {out:?}"
        );

        let v = serde_json::json!({
            "type": "PLANNER_RESPONSE",
            "step_index": 0,
            "tool_calls": [ { "name": "read_file", "args": {} } ],
        });
        let out = decode_ag_line("/x/t.jsonl", SOURCE_NAME, v);
        assert_eq!(out.len(), 1, "step_index 0 still emits: {out:?}");
    }

    #[test]
    fn only_a_result_step_ends_the_call_it_ran() {
        for (ty, idx) in [
            ("USER_INPUT", 3),
            ("CONVERSATION_HISTORY", 2),
            ("EXECUTION_RESULT", 0),
        ] {
            let v = serde_json::json!({ "type": ty, "step_index": idx });
            let out = decode_ag_line("/x/t.jsonl", SOURCE_NAME, v);
            assert!(
                out.is_empty(),
                "{ty} at step {idx} must not end anything: {out:?}"
            );
        }
        let v = serde_json::json!({ "type": "EXECUTION_RESULT", "step_index": 2 });
        let out = decode_ag_line("/x/t.jsonl", SOURCE_NAME, v);
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentEvent::ToolUseEnd { tool_use_id, .. } => {
                assert_eq!(tool_use_id.as_deref(), Some("ag-2"));
            }
            other => panic!("expected ToolUseEnd, got {other:?}"),
        }
    }

    #[test]
    fn each_call_of_a_step_is_keyed_on_the_step_it_runs_as() {
        let v = serde_json::json!({
            "type": "PLANNER_RESPONSE",
            "step_index": 1,
            "tool_calls": [
                { "name": "read_file", "args": {} },
                { "name": "read_file", "args": {} },
            ],
        });
        let ids: Vec<_> = decode_ag_line("/x/t.jsonl", SOURCE_NAME, v)
            .into_iter()
            .map(|e| match e {
                AgentEvent::ToolUseStart { tool_use_id, .. } => tool_use_id,
                other => panic!("expected ToolUseStart, got {other:?}"),
            })
            .collect();
        assert_eq!(ids, [Some("ag-2".to_string()), Some("ag-3".to_string())]);
    }

    #[test]
    fn renamed_planner_step_with_tool_calls_breadcrumbs_and_ends_only() {
        let renamed = serde_json::json!({
            "type": "PLANNER_REPLY",
            "step_index": 2,
            "tool_calls": [ { "name": "read_file", "args": {} } ],
        });
        let logs = crate::test_capture::capture_logs(|| {
            let out = decode_ag_line("/x/t.jsonl", SOURCE_NAME, renamed);
            assert_eq!(out.len(), 1, "renamed step must mint no start: {out:?}");
            match &out[0] {
                AgentEvent::ToolUseEnd { tool_use_id, .. } => {
                    assert_eq!(tool_use_id.as_deref(), Some("ag-2"));
                }
                other => panic!("expected ToolUseEnd only, got {other:?}"),
            }
        });
        assert!(
            logs.contains("unknown_event") && logs.contains("PLANNER_REPLY"),
            "a tool-call step under a renamed type must fire the drift breadcrumb, got:\n{logs}"
        );

        let result_step = serde_json::json!({ "type": "TOOL_RESULT", "step_index": 2 });
        let quiet = crate::test_capture::capture_logs(|| {
            decode_ag_line("/x/t.jsonl", SOURCE_NAME, result_step);
        });
        assert!(
            !quiet.contains("unknown_event"),
            "a tool_calls-less result step must NOT breadcrumb, got:\n{quiet}"
        );
    }

    #[test]
    fn tool_call_display_carries_the_quote_stripped_target() {
        use crate::source::ToolDetail;
        let v = serde_json::json!({
            "type": "PLANNER_RESPONSE",
            "step_index": 1,
            "tool_calls": [
                { "name": "run_command", "args": { "CommandLine": "\"git status\"" } },
                { "name": "grep_search", "args": { "SearchPath": "/repo", "query": "TODO" } },
                { "name": "read_file", "args": {} },
            ],
        });
        let out = decode_ag_line("/x/t.jsonl", SOURCE_NAME, v);
        let displays: Vec<&str> = out
            .iter()
            .map(|e| match e {
                AgentEvent::ToolUseStart {
                    detail: Some(ToolDetail::Generic { display }),
                    ..
                } => display.as_str(),
                other => panic!("expected Generic ToolUseStart, got {other:?}"),
            })
            .collect();
        assert_eq!(
            displays,
            ["run_command: git status", "grep_search: /repo", "read_file",]
        );
    }

    #[test]
    fn ag_tool_target_falls_through_a_present_non_string_key() {
        let args = serde_json::json!({ "DirectoryPath": 42, "Path": "/repo/x" });
        assert_eq!(ag_tool_target(Some(&args)).as_deref(), Some("/repo/x"));
    }

    const LOGS: &str = "/h/.gemini/antigravity-cli/brain/c1/.system_generated/logs";

    fn hook(event: &str, extra: Value) -> Value {
        let mut v = serde_json::json!({
            EVENT_STAMP: event,
            "conversationId": "c1",
            "workspacePaths": ["/repo"],
        });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    }

    #[test]
    fn a_hook_keys_on_the_conversation_whose_transcript_the_watcher_walks() {
        let out =
            decode_ag_hook_payload(test_obj(&hook("SessionStart", serde_json::json!({})))).unwrap();
        let key = ag_id_from_path(&Path::new(LOGS).join(TRANSCRIPT));
        assert_eq!(key, "c1");
        match out.as_slice() {
            [
                AgentEvent::SessionStart {
                    agent_id,
                    session_id,
                    cwd,
                    ..
                },
                AgentEvent::Identity { .. },
            ] => {
                assert_eq!(*agent_id, AgentId::from_parts(SOURCE_NAME, &key));
                assert_eq!(*session_id, key);
                assert_eq!(cwd, Path::new("/repo"));
            }
            other => panic!("expected a SessionStart and its Identity, got {other:?}"),
        }
    }

    #[test]
    fn a_hook_tool_call_pairs_on_its_step() {
        let call = serde_json::json!({
            "stepIdx": 2,
            "toolCall": { "name": "read_file", "args": { "Path": "/repo/NOTE.txt" } },
        });
        let start = decode_ag_hook_payload(test_obj(&hook("PreToolUse", call.clone()))).unwrap();
        assert!(
            start.iter().any(|e| matches!(e,
                AgentEvent::ToolUseStart { tool_use_id: Some(t), detail: Some(d), .. }
                    if t == "ag-2" && d.display() == "read_file: /repo/NOTE.txt")),
            "{start:?}"
        );
        let end = decode_ag_hook_payload(test_obj(&hook("PostToolUse", call))).unwrap();
        assert!(
            end.iter().any(|e| matches!(e,
                AgentEvent::ToolUseEnd { tool_use_id: Some(t), .. } if t == "ag-2")),
            "{end:?}"
        );
        let stop = decode_ag_hook_payload(test_obj(&hook("Stop", serde_json::json!({})))).unwrap();
        assert!(
            matches!(stop.as_slice(), [AgentEvent::TurnEnd { .. }]),
            "{stop:?}"
        );
    }

    #[test]
    fn a_hooked_question_waits_on_the_human() {
        let ask = serde_json::json!({
            "stepIdx": 4,
            "toolCall": { "name": "ask_question", "args": {} },
        });
        let out = decode_ag_hook_payload(test_obj(&hook("PreToolUse", ask))).unwrap();
        assert!(
            out.iter().any(|e| matches!(e,
                AgentEvent::UserActionRequest { tool_use_id: Some(t), .. } if t == "ag-4")),
            "{out:?}"
        );
    }

    #[test]
    fn only_the_clis_own_hooks_decode() {
        let at = |app: &str| {
            hook(
                "SessionStart",
                serde_json::json!({
                    "transcriptPath":
                        format!("/h/.gemini/{app}/brain/c1/.system_generated/logs/transcript_full.jsonl"),
                }),
            )
        };
        for app in ["antigravity", "antigravity-ide"] {
            assert!(
                decode_ag_hook_payload(test_obj(&at(app)))
                    .unwrap()
                    .is_empty(),
                "{app}"
            );
        }
        assert!(
            !decode_ag_hook_payload(test_obj(&at(CLI_APP_DATA_DIR)))
                .unwrap()
                .is_empty()
        );
        let blanked = hook("SessionStart", serde_json::json!({ "transcriptPath": "" }));
        assert!(
            !decode_ag_hook_payload(test_obj(&blanked))
                .unwrap()
                .is_empty(),
            "a recording blanks the unread path, and still decodes"
        );
    }

    #[test]
    fn an_unregistered_hook_event_is_a_breadcrumbed_error() {
        let logs = crate::test_capture::capture_logs(|| {
            assert!(
                decode_ag_hook_payload(test_obj(&hook("PreInvocation", serde_json::json!({}))))
                    .is_err()
            );
        });
        assert!(
            logs.contains("unknown_event") && logs.contains("PreInvocation"),
            "{logs}"
        );
    }

    #[test]
    fn admits_the_canonical_transcript_alone() {
        let dir = Path::new(LOGS);
        assert!(admits_transcript(&dir.join("transcript.jsonl")));
        assert!(!admits_transcript(&dir.join("transcript_full.jsonl")));
        assert!(!admits_transcript(
            &dir.join("chunks").join("transcript").join("00000000.jsonl")
        ));
    }
}
