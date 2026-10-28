//! Reasonix source — HOOK-ONLY (no JSONL watcher).
//!
//! Reasonix (github.com/esengine/DeepSeek-Reasonix) ships a CC-style hook
//! system; connecting it links a GLOBAL plugin whose hooks run the shim —
//! project-scope hooks are trust-gated and would silently not fire. Payloads
//! arrive on the shared hook socket stamped
//! `_pixtuoid_source: "reasonix"` in Reasonix's own envelope, NOT the CC shape:
//!
//! ```json
//! {"event":"PreToolUse","sessionId":"s1","cwd":"/repo","toolName":"bash","toolArgs":{"command":"ls"}}
//! ```
//!
//! camelCase fields, `event` discriminator (not `hook_event_name`), no
//! transcript path, and a tool-call id only on `SubagentStart`/`SubagentStop`
//! (`internal/ext/hook/hook.go` Payload).
//! It DOES carry `sessionId` — every recorded payload does — so that is the
//! AgentId key. Keying on the workspace instead would merge two sessions in ONE
//! project into a single sprite (the Cursor lesson; `hermes.rs` states it too).
//!
//! Why no JSONL transport: v2 session files are FULL-REWRITTEN (tmp+rename)
//! once per turn — zero mid-turn writes, so a tail watcher shows nothing while
//! the agent works; headless `reasonix run` writes no session file at all; and
//! with no shared key between the file name and hook payloads, a JSONL agent
//! could never coalesce with the hook agent (guaranteed two sprites).

use crate::source::decoder::{DecodeError, DecodeResult as Result};
use serde_json::Value;

use crate::AgentId;
use crate::source::decoder::{MAX_DECODED_FIELD_CHARS, ellipsize};
use crate::source::registry::{
    ChildEndInference, Corroboration, FocusChannel, HookDecoding, HookObject, Hyphen, SourceCaps,
    SourceDescriptor, SourceKind,
};
use crate::source::{AgentEvent, RequestKeys, ToolDetail};

/// The Reasonix CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "reasonix";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    label_prefix: "rx",
    verified_version: "2.53.1",
    version_probe: Some(&["reasonix", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll(decode_rx_hook_payload)),
        caps: SourceCaps {
            // SessionEnd fires on clean exit — best-effort counts.
            has_exit_signal: true,
            // UserPromptSubmit re-emits SessionStart, so a swept-but-live
            // session walks back in on the next prompt.
            resurrects_on_prompt: true,
            // A subagent's hooks carry its own session id, the parent's
            // suffixed with its role (`internal/ext/hook/runner.go:57-66` at
            // v2.39.0), so the parent's slot emits NOTHING until the dispatch
            // tool's PostToolUse.
            delegations_are_hook_silent: true,
            // A child's session fires no end; a skill tool's child is keyed
            // by a call id its parent's tool hooks don't carry
            // (`internal/assembly/boot/delegation.go:127-133`,
            // `internal/ext/hook/hook.go:827-847` at v2.39.0).
            child_end_inference: ChildEndInference::AnyTaskEnd,
            child_hooks_unnamed: false,
            brackets_turns: false,
        },
        focus: FocusChannel::ShimStamp(Corroboration::Twice),
    },
};

/// Reasonix tools that spawn a subagent, so the slot reads "Delegating" while
/// it works (`internal/ext/hook/hook_claude_agent.go:7-10`). `task` is not
/// here: `SubagentStart` marks a foreground one with its call id, and a
/// background one leaves the parent working. `run_skill` is excluded: it is
/// only sometimes a subagent and the args don't say which.
const SUBAGENT_TOOLS: &[&str] = &[
    "read_only_task",
    "parallel_tasks",
    "explore",
    "locate",
    "research",
    "review",
    "security_review",
];

/// A role's hooks carry its parent's session id suffixed `:<role>`
/// (`internal/ext/hook/runner.go:57-66`). A subagent's role is
/// `subagent:<id>` (`internal/runtime/delegation/task.go:901-909`).
const SUBAGENT_ROLE: &str = ":subagent:";
/// The planner's and guardian's roles (`internal/assembly/boot/roles.go:79`,
/// `:118`): phases of the parent's own turn, not delegations.
const TURN_ROLES: &[&str] = &[":planner", ":guardian"];

/// The field `SubagentStart`/`SubagentStop` pair a call on.
const CALL_ID: &str = "callId";

/// The agent a session id names, and its parent's id when it is a subagent's:
/// every role hangs off the session's root runner
/// (`internal/assembly/boot/delegation.go:81`, `:122`), so a subagent's own
/// subagent is the root's too.
fn session_of(id: &str) -> (&str, Option<&str>) {
    let own = TURN_ROLES
        .iter()
        .find_map(|role| id.strip_suffix(role))
        .unwrap_or(id);
    (own, own.split_once(SUBAGENT_ROLE).map(|(root, _)| root))
}

/// Decode one Reasonix hook payload (already identified by
/// `_pixtuoid_source == "reasonix"`). Envelope verified against
/// `internal/ext/hook/hook.go`. An unregistered event bails rather than
/// dropping silently, so registered-vs-decoded drift is loud.
///
/// The activity arms prepend a registration, an [`AgentEvent::Identity`] (#221)
/// or a subagent's parented `SessionStart`, because Reasonix is HOOK-ONLY: a
/// slot the reducer's proof-of-life pre-pass synthesizes mid-turn has no JSONL
/// back-fill path, so without it it would stay a blank `#N` ghost until the
/// next `UserPromptSubmit`.
///
/// # Errors
///
/// If the payload lacks `event`, a non-empty `cwd` or `sessionId`, or names an unregistered event.
pub(crate) fn decode_rx_hook_payload(obj: &HookObject) -> Result<Vec<AgentEvent>> {
    let event = obj
        .get("event")
        .and_then(|s| s.as_str())
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "event"))?;
    // An empty cwd would mint a phantom agent that nothing else coalesces with.
    let cwd = obj
        .get("cwd")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "cwd"))?;
    let key = obj
        .get("sessionId")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            crate::source::drift::missing_field(SOURCE_NAME, event, "sessionId");
            DecodeError::missing_in(SOURCE_NAME, event, "sessionId")
        })?;
    let (key, parent) = session_of(key);
    let agent_id = AgentId::from_parts(SOURCE_NAME, key);
    let parent_id = parent.map(|p| AgentId::from_parts(SOURCE_NAME, p));
    let start = || AgentEvent::SessionStart {
        agent_id,
        source: SOURCE_NAME.to_string(),
        session_id: key.to_string(),
        cwd: cwd.into(),
        parent_id,
    };
    // A subagent fires no start of its own, so each of its events carries the
    // link to its parent.
    let register = || match parent_id {
        Some(_) => start(),
        None => AgentEvent::identity(agent_id, SOURCE_NAME, key, Some(cwd.into())),
    };
    // A foreground `task` brackets its child with these, keyed by the call
    // (`internal/runtime/agent/execute_one.go:674-679`): the call id lets the
    // reducer's drain walk the child out when it returns.
    let call_id = || {
        let id = obj
            .get(CALL_ID)
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty());
        if id.is_none() {
            crate::source::drift::missing_field(SOURCE_NAME, event, CALL_ID);
        }
        id.map(str::to_owned)
    };

    match event {
        // The `UserPromptSubmit` duplicate is the RESURRECT path — a
        // stale-swept session walks back in on its next prompt, and Reasonix
        // has no other re-creation signal.
        "SessionStart" | "UserPromptSubmit" => Ok(vec![start()]),
        "SubagentStart" => Ok(vec![
            register(),
            AgentEvent::ToolUseStart {
                agent_id,
                tool_use_id: call_id(),
                detail: Some(ToolDetail::Task),
            },
        ]),
        "SubagentStop" => Ok(vec![AgentEvent::ToolUseEnd {
            agent_id,
            tool_use_id: call_id(),
        }]),
        "PreToolUse" => {
            let tool = obj
                .get("toolName")
                .and_then(|s| s.as_str())
                .unwrap_or_else(|| {
                    crate::source::drift::missing_field(SOURCE_NAME, "PreToolUse", "toolName");
                    "?"
                });
            Ok(vec![
                register(),
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id: None,
                    detail: Some(rx_tool_detail(tool, obj.get("toolArgs"))),
                },
            ])
        }
        "PostToolUse" => {
            let mut events = vec![register()];
            // It names its call's tool, on a failure too
            // (`internal/ext/hook/runner.go:166-192`), and a skill call's return
            // is its child's end: re-stating the Task makes this End close one,
            // whatever the parent started since.
            let tool = obj.get("toolName").and_then(|s| s.as_str());
            if tool.is_none() {
                crate::source::drift::missing_field(SOURCE_NAME, "PostToolUse", "toolName");
            }
            if tool.is_some_and(|t| SUBAGENT_TOOLS.contains(&t)) {
                events.push(AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id: None,
                    detail: Some(ToolDetail::Task),
                });
            }
            events.push(AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id: None,
            });
            Ok(events)
        }
        // Identity-less: an end alone proves too little to label a session, so
        // an unknown id registers blank (`shows_activity`).
        "Stop" => Ok(vec![AgentEvent::turn_end(agent_id)]),
        "Notification" => {
            // Every producer waits on the user: the approval gate, a question,
            // Auto Guard (`internal/session/control/approval_orchestration.go:311`,
            // `:621`, `recovery.go:341`).
            let msg = obj.get("message").and_then(|s| s.as_str());
            Ok(vec![
                register(),
                AgentEvent::user_action_request(
                    agent_id,
                    msg.map(|m| ellipsize(m, MAX_DECODED_FIELD_CHARS)),
                    RequestKeys::default(),
                ),
            ])
        }
        // The STRUCTURED approval gate: fires BEFORE the prompt with the tool +
        // subject, and ALONGSIDE `Notification` for the same approval — both →
        // RequiresAction is idempotent (the gated tool's PostToolUse/Stop resolves it).
        "PermissionRequest" => {
            let tool = obj
                .get("toolName")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty());
            if tool.is_none() {
                crate::source::drift::missing_field(SOURCE_NAME, "PermissionRequest", "toolName");
            }
            let subject = obj
                .get("subject")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty());
            // Pre-cap each component before concatenating so an oversized
            // `subject` can't force a large intermediate allocation; the outer
            // `ellipsize` stays the authoritative cap.
            let reason = match (tool, subject) {
                (Some(t), Some(s)) => format!(
                    "approval: {} {}",
                    ellipsize(t, MAX_DECODED_FIELD_CHARS),
                    ellipsize(s, MAX_DECODED_FIELD_CHARS),
                ),
                (Some(t), None) => format!("approval: {}", ellipsize(t, MAX_DECODED_FIELD_CHARS)),
                (None, _) => "approval required".to_string(),
            };
            Ok(vec![
                register(),
                AgentEvent::user_action_request(
                    agent_id,
                    Some(ellipsize(&reason, MAX_DECODED_FIELD_CHARS)),
                    RequestKeys::default(),
                ),
            ])
        }
        "SessionEnd" => Ok(vec![AgentEvent::SessionEnd {
            agent_id,
            as_child: parent_id.is_some(),
        }]),
        other => {
            crate::source::drift::unknown_event(SOURCE_NAME, other);
            Err(DecodeError::unsupported(SOURCE_NAME, other))
        }
    }
}

/// Reasonix-side tool detail: the dispatch family is name-keyed because Reasonix
/// args carry no `subagent_type` for the shared semantic detection to see.
/// Everything else gets a `"name: target"` display from the keys the v1.2.0
/// builtin tools actually emit — `path`, NOT CC's `file_path` (no builtin uses
/// it); `url` is `web_fetch`.
fn rx_tool_detail(tool: &str, args: Option<&Value>) -> ToolDetail {
    if SUBAGENT_TOOLS.contains(&tool) {
        return ToolDetail::Task;
    }
    const KEYS: &[&str] = &["command", "path", "pattern", "url"];
    crate::source::decoder::generic_keyed_detail(tool, args, KEYS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;
    use serde_json::json;

    fn decode_all(v: Value) -> Vec<AgentEvent> {
        decode_rx_hook_payload(test_obj(&v)).expect("decodes")
    }

    fn decode(v: Value) -> AgentEvent {
        decode_all(v).pop().expect("at least one event")
    }

    /// An absent `sessionId` is a breadcrumbed decode error: an upstream RENAME
    /// looks identical to a build that omits it.
    #[test]
    fn a_missing_session_id_breadcrumbs_and_is_malformed() {
        let payload = json!({"event": "SessionStart", "cwd": "/repo"});
        let logs = crate::test_capture::capture_logs(|| {
            assert!(decode_rx_hook_payload(test_obj(&payload)).is_err());
        });
        assert!(
            logs.contains(crate::source::drift::TARGET),
            "an absent sessionId must breadcrumb; got {logs:?}"
        );

        let with_id = json!({"event": "SessionStart", "cwd": "/repo", "sessionId": "s1"});
        let quiet = crate::test_capture::capture_logs(|| {
            decode_rx_hook_payload(test_obj(&with_id)).expect("decodes");
        });
        assert!(
            !quiet.contains(crate::source::drift::TARGET),
            "a present sessionId must stay quiet; got {quiet:?}"
        );
    }

    #[test]
    fn two_sessions_in_one_workspace_stay_two_agents() {
        let ev = |sid: &str| {
            decode_rx_hook_payload(test_obj(&json!({
                "event": "SessionStart", "cwd": "/repo", "sessionId": sid
            })))
            .expect("decodes")
            .remove(0)
        };
        let (a, b) = (ev("s-1"), ev("s-2"));
        assert_ne!(a.agent_id(), b.agent_id(), "one project, two sessions");
        assert_eq!(a.agent_id(), AgentId::from_parts(SOURCE_NAME, "s-1"));
    }

    #[test]
    fn session_start_carries_source_cwd_and_the_session_key() {
        let ev = decode(json!({
            "event": "SessionStart",
            "sessionId": "s", "cwd": "/Users/dev/zirs"
        }));
        match ev {
            AgentEvent::SessionStart {
                agent_id,
                source,
                cwd,
                parent_id,
                ..
            } => {
                assert_eq!(source, SOURCE_NAME);
                assert_eq!(agent_id, AgentId::from_parts(SOURCE_NAME, "s"));
                assert_eq!(cwd, std::path::PathBuf::from("/Users/dev/zirs"));
                assert_eq!(parent_id, None);
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn tool_target_uses_reasonix_arg_vocabulary() {
        let bash = decode(json!({
            "event": "PreToolUse", "sessionId": "s", "cwd": "/r",
            "toolName": "bash", "toolArgs": {"command": "go test ./..."}
        }));
        assert!(
            matches!(bash, AgentEvent::ToolUseStart { detail: Some(d), .. }
            if d.display() == "bash: go test ./...")
        );
        let grep = decode(json!({
            "event": "PreToolUse", "sessionId": "s", "cwd": "/r",
            "toolName": "grep", "toolArgs": {"pattern": "TODO", "path": "src"}
        }));
        assert!(
            matches!(grep, AgentEvent::ToolUseStart { detail: Some(d), .. }
            if d.display() == "grep: src")
        );
    }

    #[test]
    fn subagent_dispatch_family_maps_to_task() {
        for tool in [
            "read_only_task",
            "parallel_tasks",
            "explore",
            "locate",
            "research",
            "review",
            "security_review",
        ] {
            let ev = decode(json!({
                "event": "PreToolUse", "sessionId": "s", "cwd": "/r",
                "toolName": tool, "toolArgs": {"prompt": "do a thing"}
            }));
            assert!(
                matches!(&ev, AgentEvent::ToolUseStart { detail: Some(d), .. } if d.is_task()),
                "{tool} must map to ToolDetail::Task"
            );
        }
        for (tool, args) in [
            ("run_skill", json!({"name": "lint"})),
            ("task", json!({"prompt": "do a thing"})),
        ] {
            let ev = decode(json!({
                "event": "PreToolUse", "sessionId": "s", "cwd": "/r", "toolName": tool, "toolArgs": args
            }));
            assert!(
                matches!(&ev, AgentEvent::ToolUseStart { detail: Some(d), .. } if !d.is_task()),
                "{tool} must not map to ToolDetail::Task"
            );
        }
    }

    #[test]
    fn a_post_tool_use_without_its_tool_name_breadcrumbs() {
        let post = |v: Value| crate::test_capture::capture_logs(|| drop(decode_all(v)));
        let bare = post(json!({"event": "PostToolUse", "sessionId": "s", "cwd": "/r"}));
        assert!(bare.contains(crate::source::drift::TARGET), "{bare:?}");
        let named = post(
            json!({"event": "PostToolUse", "sessionId": "s", "cwd": "/r", "toolName": "bash"}),
        );
        assert!(!named.contains(crate::source::drift::TARGET), "{named:?}");
    }

    #[test]
    fn a_skill_calls_return_restates_its_task_before_the_end() {
        let post = |tool| {
            decode_all(
                json!({"event": "PostToolUse", "sessionId": "s", "cwd": "/r", "toolName": tool}),
            )
        };
        assert!(
            matches!(
                post("explore").as_slice(),
                [
                    _,
                    AgentEvent::ToolUseStart { tool_use_id: None, detail: Some(d), .. },
                    AgentEvent::ToolUseEnd { tool_use_id: None, .. },
                ] if d.is_task()
            ),
            "{:?}",
            post("explore")
        );
        assert!(
            matches!(
                post("read_file").as_slice(),
                [_, AgentEvent::ToolUseEnd { .. }]
            ),
            "{:?}",
            post("read_file")
        );
    }

    #[test]
    fn permission_request_maps_to_user_action_request_with_tool_and_subject() {
        let ev = decode(json!({
            "event": "PermissionRequest",
            "sessionId": "s", "cwd": "/r",
            "toolName": "bash",
            "subject": "rm -rf ./build",
            "toolArgs": {"command": "rm -rf ./build"}
        }));
        assert!(matches!(ev, AgentEvent::UserActionRequest { reason, .. }
            if reason.as_deref() == Some("approval: bash rm -rf ./build")));
    }

    #[test]
    fn permission_request_without_subject_uses_tool_only() {
        let ev = decode(json!({
            "event": "PermissionRequest", "sessionId": "s", "cwd": "/r", "toolName": "write_file"
        }));
        assert!(matches!(ev, AgentEvent::UserActionRequest { reason, .. }
            if reason.as_deref() == Some("approval: write_file")));
    }

    #[test]
    fn permission_request_without_tool_falls_back() {
        let ev = decode(json!({"event": "PermissionRequest", "sessionId": "s", "cwd": "/r"}));
        assert!(matches!(ev, AgentEvent::UserActionRequest { reason, .. }
            if reason.as_deref() == Some("approval required")));
    }

    #[test]
    fn permission_request_reason_is_capped_at_the_decode_boundary() {
        let long = "é".repeat(MAX_DECODED_FIELD_CHARS * 10);
        let ev = decode(json!({
            "event": "PermissionRequest", "sessionId": "s", "cwd": "/r",
            "toolName": "bash", "subject": long
        }));
        match &ev {
            AgentEvent::UserActionRequest { reason, .. } => {
                let reason = reason.as_deref().expect("a reason");
                assert_eq!(reason.chars().count(), MAX_DECODED_FIELD_CHARS + 1);
                assert!(reason.ends_with('…'));
            }
            other => panic!("expected UserActionRequest, got {other:?}"),
        }
    }

    #[test]
    fn one_session_is_one_agent() {
        let events = [
            json!({"event": "SessionStart", "sessionId": "s", "cwd": "/Users/dev/p"}),
            json!({"event": "UserPromptSubmit", "sessionId": "s", "cwd": "/Users/dev/p"}),
            json!({"event": "PreToolUse", "sessionId": "s", "cwd": "/Users/dev/p", "toolName": "bash",
                   "toolArgs": {"command": "ls"}}),
            json!({"event": "PostToolUse", "sessionId": "s", "cwd": "/Users/dev/p", "toolName": "bash"}),
            json!({"event": "PermissionRequest", "sessionId": "s", "cwd": "/Users/dev/p", "toolName": "bash", "subject": "ls"}),
            json!({"event": "Notification", "sessionId": "s", "cwd": "/Users/dev/p", "message": "approval needed: bash ls"}),
            json!({"event": "Stop", "sessionId": "s", "cwd": "/Users/dev/p"}),
            json!({"event": "SessionEnd", "sessionId": "s", "cwd": "/Users/dev/p"}),
        ];
        let ids: std::collections::BTreeSet<_> = events
            .iter()
            .flat_map(|v| decode_rx_hook_payload(test_obj(v)).unwrap())
            .map(|e| e.agent_id())
            .collect();
        assert_eq!(ids.len(), 1, "all events must coalesce to one AgentId");
    }

    #[test]
    fn empty_or_missing_cwd_is_malformed() {
        assert!(
            decode_rx_hook_payload(test_obj(
                &json!({"event": "Stop", "sessionId": "s", "cwd": ""})
            ))
            .is_err()
        );
        assert!(decode_rx_hook_payload(test_obj(&json!({"event": "Stop"}))).is_err());
    }

    #[test]
    fn unknown_event_bails_loudly() {
        for ev in ["PostLLMCall", "PreCompact", "PostToolUseFailure", "Bogus"] {
            assert!(
                decode_rx_hook_payload(test_obj(
                    &json!({"event": ev, "sessionId": "s", "cwd": "/r"})
                ))
                .is_err(),
                "{ev} must bail (not registered, must not decode silently)"
            );
        }
    }

    #[test]
    fn notification_without_message_carries_no_reason() {
        let ev = decode(json!({"event": "Notification", "sessionId": "s", "cwd": "/r"}));
        assert!(matches!(
            ev,
            AgentEvent::UserActionRequest { reason: None, .. }
        ));
    }

    #[test]
    fn pre_tool_use_without_tool_name_displays_question_mark() {
        let ev = decode(json!({"event": "PreToolUse", "sessionId": "s", "cwd": "/r"}));
        assert!(
            matches!(ev, AgentEvent::ToolUseStart { detail: Some(d), .. }
            if d.display() == "?")
        );
    }

    #[test]
    fn tool_args_spoofed_subagent_type_does_not_make_task() {
        let ev = decode(json!({
            "event": "PreToolUse", "sessionId": "s", "cwd": "/r",
            "toolName": "read_file",
            "toolArgs": {"path": "x.go", "subagent_type": null}
        }));
        assert!(
            matches!(&ev, AgentEvent::ToolUseStart { detail: Some(d), .. } if !d.is_task()),
            "spoofed subagent_type must stay Generic"
        );
    }

    #[test]
    fn a_subagents_events_register_it_under_its_parent() {
        let child = AgentId::from_parts(SOURCE_NAME, "p:subagent:sa_1");
        let evs = decode_all(json!({
            "event": "PreToolUse", "cwd": "/r", "sessionId": "p:subagent:sa_1",
            "toolName": "read_file", "toolArgs": {"path": "NOTE.txt"}
        }));
        assert!(
            evs.iter().any(
                |e| matches!(e, AgentEvent::SessionStart { agent_id, parent_id, .. }
                if *agent_id == child && *parent_id == Some(AgentId::from_parts(SOURCE_NAME, "p")))
            ),
            "{evs:?}"
        );
        assert!(
            evs.iter().any(
                |e| matches!(e, AgentEvent::ToolUseStart { agent_id, .. } if *agent_id == child)
            ),
            "{evs:?}"
        );
    }

    #[test]
    fn a_planner_or_guardian_is_its_parents_own_turn() {
        for role in ["planner", "guardian"] {
            let evs = decode_all(json!({
                "event": "PreToolUse", "cwd": "/r", "sessionId": format!("p:{role}"),
                "toolName": "read_file", "toolArgs": {"path": "NOTE.txt"}
            }));
            assert!(
                evs.iter()
                    .all(|e| e.agent_id() == AgentId::from_parts(SOURCE_NAME, "p")),
                "{role}: {evs:?}"
            );
        }
    }

    #[test]
    fn subagent_start_and_stop_bracket_the_parents_task_by_its_call_id() {
        let parent = AgentId::from_parts(SOURCE_NAME, "p");
        let call = Some("call_1".to_string());
        let start = decode(json!({
            "event": "SubagentStart", "cwd": "/r", "sessionId": "p", "callId": "call_1",
            "toolArgs": {"prompt": "read the note"}
        }));
        assert!(
            matches!(&start, AgentEvent::ToolUseStart { agent_id, tool_use_id, detail: Some(d) }
                if *agent_id == parent && *tool_use_id == call && d.is_task()),
            "{start:?}"
        );
        let stop = decode(json!({
            "event": "SubagentStop", "cwd": "/r", "sessionId": "p", "callId": "call_1"
        }));
        assert_eq!(
            stop,
            AgentEvent::ToolUseEnd {
                agent_id: parent,
                tool_use_id: call
            }
        );
    }
}
