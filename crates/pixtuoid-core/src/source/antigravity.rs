use crate::source::decoder::DecodeResult as Result;
use serde_json::Value;
use std::path::Path;

use crate::AgentId;
use crate::source::decoder::Hook;
use crate::source::decoder::{
    EVENT_STAMP, extract_top_level_cwd, first_non_blank_str, generic_tool_display, non_empty,
    required,
};
use crate::source::registry::{
    ChildEndInference, Corroboration, Envelope, FocusChannel, HookDecoding, Hyphen, SessionField,
    SourceCaps, SourceDescriptor, SourceKind, Transcript,
};
use crate::source::{AgentEvent, RequestKeys};

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::AntigravitySource;

/// The Antigravity CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "antigravity";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    display_name: "Antigravity CLI",
    label_prefix: "ag",
    verified_version: "1.5.18",
    version_probe: Some(&["agy", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: decode_ag_line,
            id_from_path: ag_id_from_path,
            path_filter: admits_transcript,
            head_filter: None,
            // AG step lines carry no cwd field at all
            // (`antigravity/fixtures/tool-run-hermetic`); the cwd rides the hooks'
            // `workspacePaths`.
            cwd_extractor: extract_top_level_cwd,
            parent_extractor: None,
            #[cfg(feature = "native")]
            native: native::TRANSCRIPT,
        }),
        hook: Some(HookDecoding::ClaimsAll {
            envelope: ENVELOPE,
            decode: decode_ag_hook_payload,
        }),
        caps: SourceCaps {
            // agy has no session-end hook (hooks.md lists none) and the recordings'
            // transcripts no end line (`antigravity/fixtures/tool-run-hermetic`), so
            // an exit rides the shim-stamped pid, which a session no hook stamped lacks.
            exits_can_go_unseen: true,
            // No `AG_EVENTS` hook fires at a prompt, `Stop` carries no `Identity`,
            // and a `USER_INPUT` step or a tool-less `PLANNER_RESPONSE` decodes to
            // nothing: only a tool call brings a swept session back.
            re_registers_each_turn: false,
            delegations_are_hook_silent: false,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
            brackets_turns: false,
        },
        focus: FocusChannel::ShimStamp(Corroboration::Twice),
    },
    #[cfg(feature = "native")]
    native: native::NATIVE,
};

/// The CLI's app data dir, under [`gemini_dir`]: NOT `antigravity`, the App's,
/// which shares the global hooks file but is not this source.
pub const CLI_APP_DATA_DIR: &str = "antigravity-cli";

/// agy's `.gemini` dir, under Go's `os.UserHomeDir` on every platform, and none
/// where that errs, as agy's own is (agy 1.5.7 `fs/local.GeminiDir`,
/// `local.go:23-30,65-71`): its app data dir and its global `config/` both sit here.
#[cfg(feature = "native")]
pub fn gemini_dir() -> Option<std::path::PathBuf> {
    Some(crate::platform::go_user_home_dir()?.join(".gemini"))
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
            // Closed, since `..` overflows at the wire's largest index.
            let steps = (step_index.checked_add(1).into_iter()).flat_map(|next| next..=i64::MAX);
            for (step, tc) in steps.zip(tool_calls) {
                let Some(tc_obj) = tc.as_object() else {
                    continue;
                };
                let name = required(SOURCE_NAME, step_type, "name", |k| tc_obj.get(k)?.as_str())
                    .unwrap_or("?");
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
        copy_id: None,
    }
}

/// Where agy's payload names its event and session: its documented input
/// names no event (hooks.md, "common input fields"), so the shim stamps the
/// one the installer registered the command for.
const ENVELOPE: Envelope = Envelope {
    event_field: EVENT_STAMP,
    session_field: SessionField {
        within: None,
        field: "conversationId",
    },
};

/// Decode one agy hook payload.
///
/// # Errors
///
/// If the payload lacks `conversationId`.
pub(crate) fn decode_ag_hook_payload(hook: &Hook<'_>) -> Result<Option<Vec<AgentEvent>>> {
    let (obj, event) = (hook.obj, hook.event);
    // The App and the IDE load installed plugins too (hooks.md, "Managing hooks
    // in the Antigravity App"; antigravity.google/docs/plugins.md, "Standalone IDE
    // plugin installation"), each naming transcripts under an app data dir of its
    // own (hooks.md, `transcriptPath`).
    if non_empty(obj, "transcriptPath").is_some_and(|p| {
        !Path::new(p)
            .components()
            .any(|c| c.as_os_str() == CLI_APP_DATA_DIR)
    }) {
        tracing::trace!(event, "dropping a hook another Antigravity app fired");
        return Ok(Some(vec![]));
    }
    let key = hook.session()?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, key);
    let cwd = required(SOURCE_NAME, event, "workspacePaths", |k| {
        obj.get(k)?.as_array()?.first()?.as_str()
    })
    .unwrap_or("");
    let identity = || {
        AgentEvent::identity(
            agent_id,
            SOURCE_NAME,
            key,
            (!cwd.is_empty()).then(|| cwd.into()),
        )
    };
    let call_id =
        || required(SOURCE_NAME, event, "stepIdx", |k| obj.get(k)?.as_i64()).map(step_tool_id);
    Ok(Some(match event {
        // The focus pid rides an `Identity`, so a session that never calls a
        // tool still names it.
        "SessionStart" => vec![
            AgentEvent::SessionStart {
                agent_id,
                source: SOURCE_NAME.to_string(),
                session_id: key.to_string(),
                cwd: cwd.into(),
                parent_id: None,
            },
            identity(),
        ],
        "PreToolUse" => {
            let tool_use_id = call_id();
            let tool = required(SOURCE_NAME, event, "toolCall", |k| obj.get(k));
            let name = tool
                .and_then(|t| required(SOURCE_NAME, event, "name", |k| t.get(k)?.as_str()))
                .unwrap_or("?");
            let args = tool.and_then(|t| t.get("args"));
            vec![
                identity(),
                decode_ag_tool_call(agent_id, name, args, tool_use_id),
            ]
        }
        "PostToolUse" => vec![
            identity(),
            AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id: call_id(),
            },
        ],
        // Turn end, as the execution loop terminates (hooks.md, `Stop`): the
        // session lives on to the next prompt.
        "Stop" => vec![AgentEvent::turn_end(agent_id)],
        _ => return Ok(None),
    }))
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
    let raw = first_non_blank_str(args?, KEYS)?;
    let clean = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    Some(clean.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_decode;

    #[cfg(feature = "native")]
    #[test]
    fn gemini_dir_sits_under_go_s_home_and_never_reads_the_other_variable() {
        let (own, other) = if cfg!(windows) {
            ("USERPROFILE", "HOME")
        } else {
            ("HOME", "USERPROFILE")
        };
        temp_env::with_vars(
            [(own, Some("/home/u")), (other, Some("/elsewhere"))],
            || {
                assert_eq!(
                    gemini_dir(),
                    Some(std::path::PathBuf::from("/home/u").join(".gemini"))
                );
            },
        );
        temp_env::with_vars([(own, None), (other, Some("/elsewhere"))], || {
            assert_eq!(crate::source::resolved_source_root(SOURCE_NAME), None);
        });
    }

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
    fn calls_past_the_last_step_index_mint_no_wrapped_id() {
        let ids = |step_index: i64| -> Vec<_> {
            let v = serde_json::json!({
                "type": "PLANNER_RESPONSE",
                "step_index": step_index,
                "tool_calls": [{ "name": "read_file" }, { "name": "read_file" }],
            });
            decode_ag_line("/x/t.jsonl", SOURCE_NAME, v)
                .into_iter()
                .map(|e| match e {
                    AgentEvent::ToolUseStart { tool_use_id, .. } => tool_use_id,
                    other => panic!("expected ToolUseStart, got {other:?}"),
                })
                .collect()
        };
        assert_eq!(ids(i64::MAX - 1), [Some(step_tool_id(i64::MAX))]);
        assert_eq!(ids(i64::MAX), []);
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
            crate::source::drift::breadcrumbed(
                &logs,
                SOURCE_NAME,
                crate::source::drift::DriftKind::UnknownEvent,
                &[("name", "PLANNER_REPLY")]
            ),
            "a tool-call step under a renamed type must fire the drift breadcrumb, got:\n{logs}"
        );

        let result_step = serde_json::json!({ "type": "TOOL_RESULT", "step_index": 2 });
        let quiet = crate::test_capture::capture_logs(|| {
            decode_ag_line("/x/t.jsonl", SOURCE_NAME, result_step);
        });
        assert!(
            !quiet.contains(crate::source::drift::TARGET),
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
        let out = test_decode(SOURCE_NAME, &hook("SessionStart", serde_json::json!({}))).unwrap();
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
        let start = test_decode(SOURCE_NAME, &hook("PreToolUse", call.clone())).unwrap();
        assert!(
            start.iter().any(|e| matches!(e,
                AgentEvent::ToolUseStart { tool_use_id: Some(t), detail: Some(d), .. }
                    if t == "ag-2" && d.display() == "read_file: /repo/NOTE.txt")),
            "{start:?}"
        );
        let end = test_decode(SOURCE_NAME, &hook("PostToolUse", call)).unwrap();
        assert!(
            end.iter().any(|e| matches!(e,
                AgentEvent::ToolUseEnd { tool_use_id: Some(t), .. } if t == "ag-2")),
            "{end:?}"
        );
        let stop = test_decode(SOURCE_NAME, &hook("Stop", serde_json::json!({}))).unwrap();
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
        let out = test_decode(SOURCE_NAME, &hook("PreToolUse", ask)).unwrap();
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
                test_decode(SOURCE_NAME, &at(app)).unwrap().is_empty(),
                "{app}"
            );
        }
        assert!(
            !test_decode(SOURCE_NAME, &at(CLI_APP_DATA_DIR))
                .unwrap()
                .is_empty()
        );
        let blanked = hook("SessionStart", serde_json::json!({ "transcriptPath": "" }));
        assert!(
            !test_decode(SOURCE_NAME, &blanked).unwrap().is_empty(),
            "a recording blanks the unread path, and still decodes"
        );
    }

    #[test]
    fn an_unregistered_hook_event_is_a_breadcrumbed_error() {
        let logs = crate::test_capture::capture_logs(|| {
            assert!(
                test_decode(SOURCE_NAME, &hook("PreInvocation", serde_json::json!({}))).is_err()
            );
        });
        assert!(
            crate::source::drift::breadcrumbed(
                &logs,
                SOURCE_NAME,
                crate::source::drift::DriftKind::UnknownEvent,
                &[("name", "PreInvocation")]
            ),
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
