use pixtuoid_core::AgentId;
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::source::antigravity;
use pixtuoid_core::source::claude_code::decode_cc_line;
use pixtuoid_core::source::decoder::decode_hook_payload;
use serde_json::json;

fn load(name: &str) -> serde_json::Value {
    let s = std::fs::read_to_string(format!("tests/sources/decode/fixtures/hooks/{name}.json"))
        .unwrap();
    serde_json::from_str(&s).unwrap()
}

fn decode_single(v: serde_json::Value) -> AgentEvent {
    let mut evs = decode_hook_payload(v).expect("decodes");
    assert_eq!(evs.len(), 1, "expected exactly one event, got {evs:?}");
    evs.pop().expect("one event")
}

fn decode_activity(v: serde_json::Value) -> AgentEvent {
    let mut evs = decode_hook_payload(v).expect("decodes");
    assert_eq!(evs.len(), 2, "expected Identity + activity, got {evs:?}");
    assert!(
        matches!(evs[0], AgentEvent::Identity { .. }),
        "tool/permission arms must lead with Identity, got {evs:?}"
    );
    let activity = evs.pop().expect("activity event");
    assert_eq!(
        evs[0].agent_id(),
        activity.agent_id(),
        "Identity must coalesce with its activity event"
    );
    activity
}

fn load_jsonl(name: &str) -> serde_json::Value {
    let s = std::fs::read_to_string(format!("tests/sources/decode/fixtures/jsonl/{name}.json"))
        .unwrap();
    serde_json::from_str(&s).unwrap()
}

#[test]
fn decode_session_start() {
    let ev = decode_single(load("session_start"));
    let expected_id = AgentId::from_parts("claude-code", "ses-abc");
    match ev {
        AgentEvent::SessionStart {
            agent_id,
            session_id,
            source,
            ..
        } => {
            assert_eq!(agent_id, expected_id);
            assert_eq!(session_id, "ses-abc");
            assert_eq!(source, "claude-code");
        }
        other => panic!("expected SessionStart, got {other:?}"),
    }
}

#[test]
fn decode_session_start_with_custom_source() {
    let mut payload = load("session_start");
    payload["_pixtuoid_source"] = serde_json::Value::String("codex".into());
    let ev = decode_single(payload);
    match ev {
        AgentEvent::SessionStart { source, .. } => {
            assert_eq!(source, "codex");
        }
        other => panic!("expected SessionStart, got {other:?}"),
    }
}

#[test]
fn decode_pre_tool_use_write_maps_to_typing() {
    let ev = decode_activity(load("pre_tool_use_write"));
    match ev {
        AgentEvent::ActivityStart { detail, .. } => {
            assert!(detail.unwrap().display().contains("Write"));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn decode_post_tool_use_is_activity_end() {
    let ev = decode_activity(load("post_tool_use_write"));
    assert!(matches!(ev, AgentEvent::ActivityEnd { .. }));
}

#[test]
fn decode_notification_is_waiting() {
    let ev = decode_activity(load("notification"));
    match ev {
        AgentEvent::Waiting { reason, .. } => assert!(reason.contains("permission")),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn decode_session_end() {
    let ev = decode_single(load("session_end"));
    assert!(matches!(
        ev,
        AgentEvent::SessionEnd {
            as_child: false,
            ..
        }
    ));
}

#[test]
fn decode_unknown_event_returns_err() {
    let mut bad = load("session_start");
    bad["hook_event_name"] = serde_json::Value::String("UnknownThing".into());
    assert!(decode_hook_payload(bad).is_err());
}

#[test]
fn empty_session_id_is_rejected() {
    assert!(
        decode_hook_payload(json!({
            "hook_event_name": "SessionStart",
            "session_id": "",
            "transcript_path": "/p/a.jsonl",
            "cwd": "/repo"
        }))
        .is_err(),
        "empty session_id must Err, not mint AgentId(source, \"\")"
    );
}

#[test]
fn cc_empty_attribution_agent_emits_no_rename() {
    let events = decode_cc_line(
        "/p/parent.jsonl",
        "claude-code",
        json!({"type": "assistant", "attributionAgent": "", "message": {"content": []}}),
    )
    .unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Rename { .. })),
        "empty attributionAgent must not emit a (label-blanking) Rename, got {events:?}"
    );
}

#[test]
fn cc_trailing_colon_attribution_agent_emits_no_rename() {
    let events = decode_cc_line(
        "/p/parent.jsonl",
        "claude-code",
        json!({"type": "assistant", "attributionAgent": "ns:", "message": {"content": []}}),
    )
    .unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Rename { .. })),
        "trailing-colon attributionAgent must not emit a (label-blanking) Rename, got {events:?}"
    );
}

#[test]
fn codex_subagent_start_links_child_to_parent() {
    let ev = decode_single(json!({
        "hook_event_name": "SubagentStart",
        "session_id": "parent-sess",
        "agent_id": "child-agent",
        "agent_type": "default",
        "turn_id": "turn-1",
        "cwd": "/home/user/demo-project",
        "_pixtuoid_source": "codex"
    }));
    match ev {
        AgentEvent::SessionStart {
            agent_id,
            source,
            cwd,
            parent_id,
            ..
        } => {
            assert_eq!(source, "codex");
            assert_eq!(
                agent_id,
                AgentId::from_parts("codex", "child-agent"),
                "child keyed on agent_id (coalesces with the subagent rollout UUID)"
            );
            assert_eq!(
                parent_id,
                Some(AgentId::from_parts("codex", "parent-sess")),
                "linked to the parent session"
            );
            assert_eq!(cwd, std::path::PathBuf::from("/home/user/demo-project"));
        }
        other => panic!("expected SessionStart, got {other:?}"),
    }
}

#[test]
fn codex_subagent_stop_ends_child_not_parent() {
    let ev = decode_single(json!({
        "hook_event_name": "SubagentStop",
        "session_id": "parent-sess",
        "agent_id": "child-agent",
        "agent_type": "default",
        "stop_hook_active": false,
        "_pixtuoid_source": "codex"
    }));
    match ev {
        AgentEvent::SessionEnd { agent_id, as_child } => {
            assert_eq!(
                agent_id,
                AgentId::from_parts("codex", "child-agent"),
                "ends the CHILD (keyed on agent_id), never the parent session"
            );
            assert!(
                as_child,
                "a SubagentStop end must carry the as_child stamp (the reducer's \
                 child ledger keys on it, #244/#246)"
            );
        }
        other => panic!("expected SessionEnd, got {other:?}"),
    }
}

#[test]
fn codex_subagent_hooks_reject_missing_or_empty_agent_id() {
    for event in ["SubagentStart", "SubagentStop"] {
        assert!(
            decode_hook_payload(json!({
                "hook_event_name": event,
                "session_id": "parent-sess",
                "_pixtuoid_source": "codex"
            }))
            .is_err(),
            "{event} without agent_id must Err"
        );
        assert!(
            decode_hook_payload(json!({
                "hook_event_name": event,
                "session_id": "parent-sess",
                "agent_id": "",
                "_pixtuoid_source": "codex"
            }))
            .is_err(),
            "{event} with empty agent_id must Err"
        );
    }
}

#[test]
fn cc_subagent_start_keys_prefixed_child_and_links_parent() {
    for agent_type in ["general-purpose", "workflow-subagent"] {
        let ev = decode_single(json!({
            "hook_event_name": "SubagentStart",
            "session_id": "01000000-0000-7000-8000-0000000000cc",
            "transcript_path": "/home/user/.claude/projects/-home-user-demo-project/01000000-0000-7000-8000-0000000000cc.jsonl",
            "cwd": "/home/user/demo-project",
            "agent_id": "a0000000000000001",
            "agent_type": agent_type
        }));
        match ev {
            AgentEvent::SessionStart {
                agent_id,
                source,
                cwd,
                parent_id,
                ..
            } => {
                assert_eq!(source, "claude-code");
                assert_eq!(
                    agent_id,
                    AgentId::from_parts("claude-code", "agent-a0000000000000001"),
                    "{agent_type}: child keyed `agent-<id>` — the bare wire id \
                     lacks the prefix the transcript stem carries"
                );
                assert_eq!(
                    parent_id,
                    Some(AgentId::from_parts(
                        "claude-code",
                        "01000000-0000-7000-8000-0000000000cc"
                    )),
                    "{agent_type}: linked to the parent session"
                );
                assert_eq!(cwd, std::path::PathBuf::from("/home/user/demo-project"));
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
    }
}

#[test]
fn cc_subagent_stop_keys_on_agent_transcript_path_stem() {
    for nested_path in [
        "/home/user/.claude/projects/-home-user-demo-project/01000000-0000-7000-8000-0000000000cc/subagents/agent-a0000000000000001.jsonl",
        "/home/user/.claude/projects/-home-user-demo-project/01000000-0000-7000-8000-0000000000cc/subagents/workflows/wf_00000000-000/agent-a0000000000000001.jsonl",
    ] {
        let ev = decode_single(json!({
            "hook_event_name": "SubagentStop",
            "session_id": "01000000-0000-7000-8000-0000000000cc",
            "transcript_path": "/home/user/.claude/projects/-home-user-demo-project/01000000-0000-7000-8000-0000000000cc.jsonl",
            "cwd": "/home/user/demo-project",
            "agent_id": "a0000000000000001",
            "agent_type": "general-purpose",
            "agent_transcript_path": nested_path,
            "stop_hook_active": false,
            "last_assistant_message": "done"
        }));
        match ev {
            AgentEvent::SessionEnd { agent_id, as_child } => {
                assert_eq!(
                    agent_id,
                    AgentId::from_parts("claude-code", "agent-a0000000000000001"),
                    "ends the CHILD keyed on the agent transcript's filename stem \
                     (path: {nested_path})"
                );
                assert!(
                    as_child,
                    "a SubagentStop end must carry the as_child stamp (the reducer's \
                     child ledger keys on it, #244/#246)"
                );
            }
            other => panic!("expected SessionEnd, got {other:?}"),
        }
    }
}

#[test]
fn cc_subagent_stop_without_transcript_path_falls_back_to_prefixed_agent_id() {
    for payload in [
        json!({
            "hook_event_name": "SubagentStop",
            "session_id": "parent-sess",
            "agent_id": "a0000000000000001"
        }),
        json!({
            "hook_event_name": "SubagentStop",
            "session_id": "parent-sess",
            "agent_id": "a0000000000000001",
            "agent_transcript_path": null
        }),
        json!({
            "hook_event_name": "SubagentStop",
            "session_id": "parent-sess",
            "agent_id": "a0000000000000001",
            "agent_transcript_path": ""
        }),
    ] {
        let ev = decode_single(payload);
        match ev {
            AgentEvent::SessionEnd { agent_id, as_child } => {
                assert!(
                    as_child,
                    "fallback-path SubagentStop must stamp as_child: true \
                     (the child ledger keys on it, #244/#246)"
                );
                assert_eq!(
                    agent_id,
                    AgentId::from_parts("claude-code", "agent-a0000000000000001")
                );
            }
            other => panic!("expected SessionEnd, got {other:?}"),
        }
    }
}

#[test]
fn cc_subagent_start_and_stop_coalesce_on_one_child_id() {
    let start = decode_single(json!({
        "hook_event_name": "SubagentStart",
        "session_id": "01000000-0000-7000-8000-0000000000cc",
        "cwd": "/home/user/demo-project",
        "agent_id": "a0000000000000001",
        "agent_type": "workflow-subagent"
    }))
    .agent_id();
    let stop = decode_single(json!({
        "hook_event_name": "SubagentStop",
        "session_id": "01000000-0000-7000-8000-0000000000cc",
        "agent_id": "a0000000000000001",
        "agent_type": "workflow-subagent",
        "agent_transcript_path": "/home/user/.claude/projects/-home-user-demo-project/01000000-0000-7000-8000-0000000000cc/subagents/workflows/wf_00000000-000/agent-a0000000000000001.jsonl"
    }))
    .agent_id();
    assert_eq!(
        start, stop,
        "Start (prefix fallback) and Stop (transcript stem) must coalesce"
    );
}

#[test]
fn cc_subagent_start_does_not_double_prefix_an_already_prefixed_agent_id() {
    for wire_id in ["abc123", "agent-abc123"] {
        let ev = decode_single(json!({
            "hook_event_name": "SubagentStart",
            "session_id": "parent-sess",
            "cwd": "/home/user/demo-project",
            "agent_id": wire_id,
            "agent_type": "general-purpose"
        }));
        assert_eq!(
            ev.agent_id(),
            AgentId::from_parts("claude-code", "agent-abc123"),
            "wire form {wire_id:?} must key as agent-abc123"
        );
    }
}

#[test]
fn cc_subagent_hooks_reject_missing_or_empty_agent_id() {
    for event in ["SubagentStart", "SubagentStop"] {
        for payload in [
            json!({"hook_event_name": event, "session_id": "parent-sess"}),
            json!({"hook_event_name": event, "session_id": "parent-sess", "agent_id": ""}),
            json!({"hook_event_name": event, "agent_id": "abc"}),
        ] {
            assert!(
                decode_hook_payload(payload.clone()).is_err(),
                "CC {event} with missing/empty ids must Err, got Ok for {payload}"
            );
        }
    }
}

// Windows-only: `codex_id_from_path`'s file_stem split needs `\` to act as a
// separator (on Unix `\` is an ordinary filename byte).
#[cfg(windows)]
#[test]
fn codex_subagent_hook_coalesces_with_its_windows_rollout_path() {
    use pixtuoid_core::source::{codex, registry};
    let uuid = "019e7762-9ded-7e33-be41-946ecf105bf4";
    let rollout =
        format!(r"C:\Users\Me\.codex\sessions\2026\06\08\rollout-2026-06-08T22-36-52-{uuid}.jsonl");

    let child = decode_single(json!({
        "hook_event_name": "SubagentStart",
        "session_id": "parent-sess",
        "agent_id": uuid,
        "agent_type": "default",
        "cwd": r"C:\Users\Me\demo",
        "_pixtuoid_source": "codex"
    }))
    .agent_id();

    let watcher = AgentId::from_parts(
        codex::SOURCE_NAME,
        &registry::id_deriver_for(codex::SOURCE_NAME)(std::path::Path::new(&rollout)),
    );

    assert_eq!(
        child, watcher,
        "a Codex subagent hook (agent_id) and its Windows rollout file must coalesce \
         to one AgentId — a mismatch leaves the subagent orphaned from the scope tree"
    );
}

#[test]
fn cc_jsonl_assistant_tool_use_is_activity_start() {
    let transcript = "/Users/me/.claude/projects/x/ses-abc.jsonl";
    let events =
        decode_cc_line(transcript, "claude-code", load_jsonl("assistant_tool_use")).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentEvent::ActivityStart {
            tool_use_id,
            detail,
            ..
        } => {
            assert_eq!(tool_use_id.as_deref(), Some("tu_123"));
            assert!(detail.as_ref().unwrap().display().contains("Write"));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn cc_jsonl_tool_result_is_activity_end() {
    let transcript = "/Users/me/.claude/projects/x/ses-abc.jsonl";
    let events = decode_cc_line(transcript, "claude-code", load_jsonl("tool_result")).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentEvent::ActivityEnd { tool_use_id, .. } => {
            assert_eq!(tool_use_id.as_deref(), Some("tu_123"));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn decode_hook_payload_with_multibyte_tool_input_does_not_panic() {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "ses-zh",
        "transcript_path": "/tmp/zh.jsonl",
        "cwd": "/tmp",
        "tool_name": "Bash",
        "tool_input": {
            "command": "echo 这是一个非常长的中文命令需要被截断这是一个非常长的中文命令需要被截断"
        }
    });
    let ev = decode_activity(payload);
    match ev {
        AgentEvent::ActivityStart { detail, .. } => {
            let d = detail.expect("detail set");
            assert!(d.display().contains("Bash"), "got: {}", d.display());
        }
        other => panic!("expected ActivityStart, got {other:?}"),
    }
}

#[test]
fn decode_pre_tool_use_carries_tool_use_id_from_payload() {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "ses-abc",
        "transcript_path": "/Users/me/.claude/projects/x/ses-abc.jsonl",
        "cwd": "/repo",
        "tool_name": "Agent",
        "tool_use_id": "toolu_01ABC",
        "tool_input": { "description": "go" }
    });
    let ev = decode_activity(payload);
    match ev {
        AgentEvent::ActivityStart {
            tool_use_id,
            detail,
            ..
        } => {
            assert_eq!(tool_use_id.as_deref(), Some("toolu_01ABC"));
            assert!(detail.expect("detail set").is_task());
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn decode_pre_tool_use_agent_tool_is_task() {
    for tool in ["Agent", "Task"] {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "ses-abc",
            "transcript_path": "/p/ses-abc.jsonl",
            "cwd": "/repo",
            "tool_name": tool,
            "tool_use_id": "toolu_01ABC",
            "tool_input": { "description": "go", "subagent_type": "Explore" }
        });
        match decode_activity(payload) {
            AgentEvent::ActivityStart { detail, .. } => assert!(
                detail.expect("detail set").is_task(),
                "{tool} must be Task-detected"
            ),
            other => panic!("got {other:?}"),
        }
    }
}

#[test]
fn subagent_dispatch_detected_by_subagent_type_under_novel_name() {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "ses-abc",
        "transcript_path": "/p/ses-abc.jsonl",
        "cwd": "/repo",
        "tool_name": "Delegate2027",
        "tool_use_id": "toolu_01ZZ",
        "tool_input": { "description": "go", "subagent_type": "Explore" }
    });
    match decode_activity(payload) {
        AgentEvent::ActivityStart { detail, .. } => assert!(
            detail.expect("detail").is_task(),
            "a tool carrying subagent_type is a dispatch regardless of its name"
        ),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn non_dispatch_tool_is_not_task() {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s",
        "transcript_path": "/p/s.jsonl",
        "cwd": "/repo",
        "tool_name": "Read",
        "tool_use_id": "t",
        "tool_input": { "file_path": "/x" }
    });
    match decode_activity(payload) {
        AgentEvent::ActivityStart { detail, .. } => {
            assert!(!detail.expect("detail").is_task(), "Read is not a dispatch")
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn cc_jsonl_agent_tool_use_is_task() {
    let line = serde_json::json!({
        "type": "assistant",
        "message": {"content": [
            {"type": "tool_use", "id": "t1", "name": "Agent",
             "input": {"description": "x", "subagent_type": "general-purpose"}}
        ]}
    });
    let events = decode_cc_line("/p/parent.jsonl", "claude-code", line).unwrap();
    let task = events.iter().find_map(|e| match e {
        AgentEvent::ActivityStart { detail, .. } => detail.as_ref(),
        _ => None,
    });
    assert!(
        task.expect("ActivityStart present").is_task(),
        "the JSONL 'Agent' tool_use must be Task-detected too"
    );
}

#[test]
fn decode_post_tool_use_carries_tool_use_id_from_payload() {
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "ses-abc",
        "transcript_path": "/Users/me/.claude/projects/x/ses-abc.jsonl",
        "cwd": "/repo",
        "tool_name": "Task",
        "tool_use_id": "toolu_01ABC"
    });
    let ev = decode_activity(payload);
    match ev {
        AgentEvent::ActivityEnd { tool_use_id, .. } => {
            assert_eq!(tool_use_id.as_deref(), Some("toolu_01ABC"));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn cc_jsonl_subagent_line_with_attribution_emits_rename() {
    let transcript = "/Users/me/.claude/projects/x/sess/subagents/agent-abc.jsonl";
    let v = serde_json::json!({
        "type": "assistant",
        "sessionId": "sess",
        "cwd": "/repo",
        "attributionAgent": "example-plugin:example-agent",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_1", "name": "Read",
                  "input": { "file_path": "/repo/src/a.rs" } }
            ]
        }
    });
    let events = decode_cc_line(transcript, "claude-code", v).unwrap();
    let has_rename = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::Rename { label, .. } if label == "example-agent"
        )
    });
    assert!(has_rename, "expected Rename event, got {events:?}");
}

#[test]
fn cc_jsonl_plain_user_message_yields_no_events() {
    let transcript = "/Users/me/.claude/projects/x/ses-abc.jsonl";
    let events = decode_cc_line(transcript, "claude-code", load_jsonl("user_message")).unwrap();
    assert!(events.is_empty());
}

#[test]
fn cc_jsonl_slash_command_user_lines_yield_no_events() {
    let transcript = "/Users/me/.claude/projects/x/ses-abc.jsonl";
    for cmd in ["/exit", "/quit", "/clear", "/compact"] {
        let v = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": format!("<command-name>{cmd}</command-name>") }
        });
        let events = decode_cc_line(transcript, "claude-code", v).unwrap();
        assert!(
            events.is_empty(),
            "{cmd} content must not drive lifecycle: {events:?}"
        );
    }
}

#[test]
fn cc_jsonl_quoted_exit_wrapper_mid_prose_yields_no_events() {
    let transcript = "/Users/me/.claude/projects/x/ses-abc.jsonl";
    let v = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": "I saw <command-name>/exit</command-name> in a transcript — what writes that?"
        }
    });
    let events = decode_cc_line(transcript, "claude-code", v).unwrap();
    assert!(
        events.is_empty(),
        "quoting the wrapper must not emit SessionEnd: {events:?}"
    );
}

#[test]
fn cc_jsonl_plain_string_user_message_yields_no_events() {
    let transcript = "/Users/me/.claude/projects/x/ses-abc.jsonl";
    let v = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": "please fix the /exit bug" }
    });
    let events = decode_cc_line(transcript, "claude-code", v).unwrap();
    assert!(
        events.is_empty(),
        "prose mentioning /exit is not a command: {events:?}"
    );
}

#[test]
fn ag_planner_response_keys_each_call_on_the_step_it_runs_as() {
    let transcript = "/Users/me/.gemini/antigravity-cli/brain/sess/transcript.jsonl";
    let v = serde_json::json!({
        "step_index": 2,
        "source": "MODEL",
        "type": "PLANNER_RESPONSE",
        "tool_calls": [
            { "name": "list_dir", "args": { "DirectoryPath": "\"/repo/src\"" } },
            { "name": "read_file", "args": { "AbsolutePath": "\"/repo/README.md\"" } }
        ]
    });
    let events = antigravity::decode_ag_line(transcript, "antigravity", v).unwrap();
    assert_eq!(events.len(), 2);
    match &events[0] {
        AgentEvent::ActivityStart { tool_use_id, .. } => {
            assert_eq!(tool_use_id.as_deref(), Some("ag-3"));
        }
        other => panic!("got {other:?}"),
    }
    match &events[1] {
        AgentEvent::ActivityStart { tool_use_id, .. } => {
            assert_eq!(tool_use_id.as_deref(), Some("ag-4"));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn ag_tool_result_emits_activity_end() {
    let transcript = "/Users/me/.gemini/antigravity-cli/brain/sess/transcript.jsonl";
    let v = serde_json::json!({
        "step_index": 3,
        "type": "LIST_DIRECTORY",
        "content": "output"
    });
    let events = antigravity::decode_ag_line(transcript, "antigravity", v).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentEvent::ActivityEnd { tool_use_id, .. } => {
            assert_eq!(tool_use_id.as_deref(), Some("ag-3"));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn ag_uses_source_namespaced_agent_id() {
    let transcript = "/shared/path.jsonl";
    let v = serde_json::json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "tool_calls": [] });
    let _events = antigravity::decode_ag_line(transcript, "antigravity", v).unwrap();
    let ag_id = AgentId::from_parts("antigravity", transcript);
    let cc_id = AgentId::from_parts("claude-code", transcript);
    assert_ne!(
        ag_id, cc_id,
        "different sources must produce different AgentIds"
    );
}

#[test]
fn ag_ask_permission_and_question_emits_waiting() {
    let transcript = "/Users/me/.gemini/antigravity-cli/brain/sess/transcript.jsonl";

    let v_perm = serde_json::json!({
        "step_index": 4,
        "type": "PLANNER_RESPONSE",
        "tool_calls": [
            { "name": "ask_permission", "args": { "Reason": "read a file" } }
        ]
    });
    let events_perm = antigravity::decode_ag_line(transcript, "antigravity", v_perm).unwrap();
    assert_eq!(events_perm.len(), 1);
    match &events_perm[0] {
        AgentEvent::Waiting {
            reason,
            tool_use_id,
            ..
        } => {
            assert_eq!(reason, "asking permission");
            assert_eq!(tool_use_id.as_deref(), Some("ag-5"));
        }
        other => panic!("expected Waiting, got {other:?}"),
    }

    let v_quest = serde_json::json!({
        "step_index": 5,
        "type": "PLANNER_RESPONSE",
        "tool_calls": [
            { "name": "ask_question", "args": { "questions": [] } }
        ]
    });
    let events_quest = antigravity::decode_ag_line(transcript, "antigravity", v_quest).unwrap();
    assert_eq!(events_quest.len(), 1);
    match &events_quest[0] {
        AgentEvent::Waiting {
            reason,
            tool_use_id,
            ..
        } => {
            assert_eq!(reason, "asking permission");
            assert_eq!(tool_use_id.as_deref(), Some("ag-6"));
        }
        other => panic!("expected Waiting, got {other:?}"),
    }
}

#[test]
fn cc_session_ended_detects_session_end_subtype() {
    use pixtuoid_core::source::claude_code::cc_session_ended;
    let tail = br#"{"type":"system","subtype":"session_start","sessionId":"s1"}
{"type":"assistant","message":{"role":"assistant","content":[]}}
{"type":"system","subtype":"session_end","sessionId":"s1"}
"#;
    assert!(cc_session_ended(tail));
}

#[test]
fn cc_session_ended_returns_false_for_active_session() {
    use pixtuoid_core::source::claude_code::cc_session_ended;
    let tail = br#"{"type":"system","subtype":"session_start","sessionId":"s1"}
{"type":"assistant","message":{"role":"assistant","content":[]}}
"#;
    assert!(!cc_session_ended(tail));
}

#[test]
fn cc_session_ended_ignores_string_content_containing_session_end() {
    use pixtuoid_core::source::claude_code::cc_session_ended;
    let tail = br#"{"type":"system","subtype":"session_start","sessionId":"s1"}
{"type":"user","message":{"content":[{"type":"tool_result","output":"cat session_end.sh"}]}}
"#;
    assert!(
        !cc_session_ended(tail),
        "should not false-positive on session_end inside tool output"
    );
}

#[test]
fn cc_session_ended_ignores_slash_command_content() {
    use pixtuoid_core::source::claude_code::cc_session_ended;
    let tail = br#"{"type":"system","subtype":"session_start","sessionId":"s1"}
{"type":"assistant","message":{"role":"assistant","content":[]}}
{"type":"user","message":{"role":"user","content":"<command-name>/exit</command-name>\n            <command-message>exit</command-message>"}}
"#;
    assert!(
        !cc_session_ended(tail),
        "an /exit-wrapper user line is content, not a structural end marker"
    );
    let quoted = br#"{"type":"system","subtype":"session_start","sessionId":"s1"}
{"type":"user","message":{"role":"user","content":"why does <command-name>/quit</command-name> show up wrapped?"}}
"#;
    assert!(
        !cc_session_ended(quoted),
        "quoting the wrapper mid-prose must not end the session"
    );
}

#[test]
fn cc_session_ended_end_then_session_start_is_not_ended() {
    use pixtuoid_core::source::claude_code::cc_session_ended;
    let tail = br#"{"type":"system","subtype":"session_end","sessionId":"s1"}
{"type":"system","subtype":"session_start","sessionId":"s1"}
"#;
    assert!(
        !cc_session_ended(tail),
        "session resumed after a structural end — last marker wins"
    );
}

#[test]
fn decode_hook_payload_missing_session_id_returns_err() {
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/repo"
    });
    assert!(
        decode_hook_payload(payload).is_err(),
        "missing session_id must return Err"
    );
}

#[test]
fn decode_cc_hook_keys_on_session_id_ignoring_transcript_path() {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "ses-abc",
        "transcript_path": "/Users/me/.claude/projects/-Worktree-B/OTHER-stem.jsonl",
        "cwd": "/repo",
        "tool_name": "Bash",
        "tool_input": { "command": "ls" }
    });
    let ev = decode_activity(payload);
    let agent_id = match ev {
        pixtuoid_core::source::AgentEvent::ActivityStart { agent_id, .. } => agent_id,
        other => panic!("expected ActivityStart, got {other:?}"),
    };
    assert_eq!(
        agent_id,
        pixtuoid_core::AgentId::from_parts(
            pixtuoid_core::source::claude_code::SOURCE_NAME,
            "ses-abc"
        ),
        "CC must key on session_id, ignoring transcript_path"
    );
}

#[test]
fn decode_pre_tool_use_long_command_is_ellipsis_truncated() {
    let long_cmd = "echo ".to_string() + &"a".repeat(60);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "ses-trunc",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/repo",
        "tool_name": "Bash",
        "tool_input": { "command": long_cmd }
    });
    match decode_activity(payload) {
        AgentEvent::ActivityStart { detail, .. } => {
            let d = detail.expect("detail set");
            assert!(
                d.display().ends_with('…'),
                "a >40-char Bash command must be ellipsis-truncated, got {}",
                d.display()
            );
        }
        other => panic!("expected ActivityStart, got {other:?}"),
    }
}

#[test]
fn decode_pre_tool_use_missing_target_field_has_no_suffix() {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "ses-nocmd",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/repo",
        "tool_name": "Bash",
        "tool_input": {}
    });
    match decode_activity(payload) {
        AgentEvent::ActivityStart { detail, .. } => {
            let d = detail.expect("detail set");
            assert_eq!(
                d.display(),
                "Bash",
                "absent target field must produce no `: <target>` suffix"
            );
        }
        other => panic!("expected ActivityStart, got {other:?}"),
    }
}

#[test]
fn ag_non_object_and_missing_step_index_emit_nothing() {
    let transcript = "/Users/me/.gemini/antigravity-cli/brain/sess/transcript.jsonl";
    assert!(
        antigravity::decode_ag_line(transcript, "antigravity", json!("x"))
            .unwrap()
            .is_empty()
    );
    assert!(
        antigravity::decode_ag_line(transcript, "antigravity", json!({ "foo": 1 }))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ag_non_integer_step_index_is_skipped() {
    let transcript = "/Users/me/.gemini/antigravity-cli/brain/sess/transcript.jsonl";
    let v = json!({
        "step_index": "not-a-number",
        "type": "PLANNER_RESPONSE",
        "tool_calls": [ { "name": "run_command", "args": { "CommandLine": "ls" } } ]
    });
    assert!(
        antigravity::decode_ag_line(transcript, "antigravity", v)
            .unwrap()
            .is_empty(),
        "a present-but-non-integer step_index must be skipped, not coerced to 0"
    );
}

#[test]
fn ag_skips_non_object_tool_call_and_keys_run_command() {
    let transcript = "/Users/me/.gemini/antigravity-cli/brain/sess/transcript.jsonl";
    let v = json!({
        "step_index": 3,
        "type": "PLANNER_RESPONSE",
        "tool_calls": [
            42,
            { "name": "run_command", "args": { "CommandLine": "\"git status\"" } }
        ]
    });
    let events = antigravity::decode_ag_line(transcript, "antigravity", v).unwrap();
    assert_eq!(
        events.len(),
        1,
        "non-object tool_call must be skipped: {events:?}"
    );
    match &events[0] {
        AgentEvent::ActivityStart { tool_use_id, .. } => {
            assert_eq!(tool_use_id.as_deref(), Some("ag-5"));
        }
        other => panic!("expected ActivityStart, got {other:?}"),
    }
}

#[test]
fn ag_planner_response_without_tool_calls_emits_nothing() {
    let transcript = "/Users/me/.gemini/antigravity-cli/brain/sess/transcript.jsonl";
    let v = json!({ "step_index": 2, "type": "PLANNER_RESPONSE" });
    assert!(
        antigravity::decode_ag_line(transcript, "antigravity", v)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ag_grep_search_decodes_to_activity_start() {
    let transcript = "/Users/me/.gemini/antigravity-cli/brain/sess/transcript.jsonl";
    let v = json!({
        "step_index": 4,
        "type": "PLANNER_RESPONSE",
        "tool_calls": [
            { "name": "grep_search", "args": { "SearchPath": "/repo", "query": "TODO" } }
        ]
    });
    let events = antigravity::decode_ag_line(transcript, "antigravity", v).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentEvent::ActivityStart { tool_use_id, .. } => {
            assert_eq!(tool_use_id.as_deref(), Some("ag-5"));
        }
        other => panic!("expected ActivityStart, got {other:?}"),
    }
}

#[test]
fn decode_hook_payload_missing_tool_name_still_succeeds() {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "ses-abc",
        "transcript_path": "/tmp/t.jsonl"
    });
    let ev = decode_activity(payload);
    match ev {
        AgentEvent::ActivityStart { detail, .. } => {
            let d = detail.expect("detail set");
            assert!(
                d.display().contains("?"),
                "missing tool_name should fall back to '?'"
            );
        }
        other => panic!("expected ActivityStart, got {other:?}"),
    }
}

#[tokio::test]
async fn hook_and_watcher_keys_coalesce_for_one_file() {
    use pixtuoid_core::source::Transport;
    use pixtuoid_core::source::claude_code::{cc_derive_label, cc_session_ended, decode_cc_line};
    use pixtuoid_core::source::jsonl::{JsonlWatcher, force_polling_backend_for_tests};
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::mpsc;

    force_polling_backend_for_tests(Duration::from_millis(25));

    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-coalesce");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let transcript = project_dir.join("ses-coalesce.jsonl");

    let transcript_str = transcript.to_string_lossy().to_string();
    let hook_payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "ses-coalesce",
        "transcript_path": transcript_str,
        "cwd": "/repo"
    });
    let hook_id = decode_single(hook_payload).agent_id();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = JsonlWatcher::new(
        projects_root.clone(),
        "claude-code".to_string(),
        decode_cc_line,
        cc_session_ended,
    )
    .with_label_deriver(cc_derive_label);
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    let start_line = serde_json::json!({
        "type": "system",
        "subtype": "session_start",
        "sessionId": "ses-coalesce",
        "cwd": "/repo"
    });
    f.write_all(format!("{start_line}\n").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    let mut watcher_id: Option<AgentId> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some((_, ev @ AgentEvent::SessionStart { .. }))) => {
                watcher_id = Some(ev.agent_id());
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {}
        }
    }
    handle.abort();

    let watcher_id = watcher_id.expect("watcher must emit SessionStart");
    assert_eq!(
        hook_id, watcher_id,
        "hook AgentId ({hook_id}) must equal watcher AgentId ({watcher_id}) for the \
         same file — mismatching IDs split one session into two sprites"
    );
}

#[test]
fn registry_cwd_extractor_matches_each_sources_real_head_shape() {
    use std::path::{Path, PathBuf};
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sources/fixtures");
    // A hermetic recording names its session dir after the conversation, which
    // every re-record changes.
    let agy = pixtuoid_core::harness::transcripts_under(
        "antigravity",
        &fixtures.join("antigravity/tool-run-hermetic"),
    );
    let cases: [(&str, PathBuf, Option<&str>); 4] = [
        (
            "claude-code",
            fixtures.join("claude-code/permission-recorded/947851e2-a15a-4de8-8375-46e9cddb5c8f.jsonl"),
            Some("/private/tmp/pixtuoid-capture/proj"),
        ),
        (
            "codex",
            fixtures.join("codex/permission-recorded/rollout-2026-08-15T06-28-11-01a0059b-cb9d-7a92-8821-85ebb7604464.jsonl"),
            Some("/private/tmp/pixtuoid-capture/proj"),
        ),
        (
            "copilot",
            fixtures.join("copilot/tool-run/65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3/events.jsonl"),
            Some(r"d:\contentforge-fullstack (1)"),
        ),
        (
            "antigravity",
            agy.first().expect("antigravity's recorded transcript").clone(),
            None,
        ),
    ];
    for (source, path, expected) in cases {
        let extract = pixtuoid_core::source::registry::cwd_extractor_for(source);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
        let got = content.lines().find_map(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| extract(&v))
        });
        assert_eq!(
            got,
            expected.map(PathBuf::from),
            "cwd extracted from {source}'s real fixture head"
        );
    }
}
