//! Reasonix source — HOOK-ONLY (no JSONL watcher).
//!
//! Reasonix (github.com/esengine/DeepSeek-Reasonix) ships a CC-style hook
//! system; connecting it links a GLOBAL plugin whose hooks run the shim —
//! project-scope hooks are trust-gated and would silently not fire. Payloads
//! arrive on the shared hook socket stamped
//! `_pixtuoid_source: "reasonix"` in Reasonix's own envelope, NOT the CC shape:
//!
//! ```json
//! {"event":"PreToolUse","cwd":"/repo","toolName":"bash","toolArgs":{"command":"ls"}}
//! ```
//!
//! camelCase fields, `event` discriminator (not `hook_event_name`), no
//! transcript path, and a tool-call id only on `SubagentStart`/`SubagentStop`
//! (`internal/ext/hook/hook.go` Payload).
//! It DOES carry `sessionId` — every recorded payload does — so
//! that is the AgentId key, cwd only as the fallback for a build that omits it.
//! Keying on the workspace instead would merge two sessions in ONE project into
//! a single sprite (the Cursor lesson; `hermes.rs` states it too).
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
use crate::source::{AgentEvent, ToolDetail};

/// The Reasonix CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "reasonix";

/// Each version publishes a "Reasonix CLI" and a "Reasonix Studio" release
/// (`studio-v…`, github.com/esengine/DeepSeek-Reasonix/releases), so
/// `releases/latest` may name the app; the CLI is the npm package's `latest`
/// (registry.npmjs.org/reasonix).
#[cfg(any(test, feature = "upstream"))]
pub const REPO: crate::source::upstream::Repo = crate::source::upstream::Repo {
    name: "esengine/DeepSeek-Reasonix",
    release: crate::source::upstream::Release::Npm {
        package: "reasonix",
        tag_prefix: "v",
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
/// The planner's and guardian's roles (`internal/assembly/boot/roles.go:80`,
/// `:119`): phases of the parent's own turn, not delegations.
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

/// Read outside the field census, so a rename goes unbreadcrumbed; each
/// watch's `finding` names what it breaks.
#[cfg(any(test, feature = "upstream"))]
pub(crate) const UPSTREAM: &[crate::source::upstream::Watch] = {
    use crate::source::upstream::{Finding, Read, Watch, When};
    &[
        Watch {
            id: "reasonix.subagent_role",
            ours: || vec![SUBAGENT_ROLE.trim_start_matches(':')],
            docs: &[REPO.doc(
                "internal/runtime/delegation/task.go",
                r"func subagentRecoveryTaskID\b",
                "`subagentRecoveryTaskID`",
            )],
            read: Read::Each {
                pattern: r#""{name}" \+"#,
            },
            when: When::Vanishes,
            finding: Finding::Breaking(
                "every subagent becomes a root sprite that no drain walks out",
            ),
            siblings: None,
        },
        // A skill tool's child names its role on its own path.
        Watch {
            id: "reasonix.skill_subagent_role",
            ours: || vec![SUBAGENT_ROLE.trim_start_matches(':')],
            docs: &[REPO.doc(
                "internal/assembly/boot/delegation.go",
                r"func skillSubagentRole\b",
                "`skillSubagentRole`",
            )],
            read: Read::Each {
                pattern: r#""{name}" \+"#,
            },
            when: When::Vanishes,
            finding: Finding::Breaking(
                "a skill tool's subagent becomes a root sprite that its parent's end leaves behind",
            ),
            siblings: None,
        },
        Watch {
            id: "reasonix.turn_roles",
            ours: || {
                TURN_ROLES
                    .iter()
                    .map(|role| role.trim_start_matches(':'))
                    .collect()
            },
            docs: &[REPO.doc(
                "internal/assembly/boot/roles.go",
                r"ForRole\(",
                "the role runners",
            )],
            read: Read::Each {
                pattern: r#"ForRole\("{name}"\)"#,
            },
            when: When::Vanishes,
            finding: Finding::Breaking("the planner or guardian becomes a sprite of its own"),
            siblings: None,
        },
        Watch {
            id: "reasonix.spawning_tools",
            ours: || SUBAGENT_TOOLS.to_vec(),
            docs: &[REPO.doc(
                "internal/ext/hook/hook_claude_agent.go",
                r"var claudeAgentSpawningTools\b",
                "`claudeAgentSpawningTools`",
            )],
            read: Read::Each {
                pattern: r#""{name}""#,
            },
            when: When::Vanishes,
            finding: Finding::Review(
                "a parent spawning through it may no longer read Delegating, nor its child walk out",
            ),
            siblings: None,
        },
    ]
};

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
/// If the payload is not an object, lacks `event` or a non-empty `cwd`, or names an unregistered event.
pub(crate) fn decode_rx_hook_payload(v: &Value) -> Result<Vec<AgentEvent>> {
    let obj = v
        .as_object()
        .ok_or_else(|| DecodeError::not_an_object(SOURCE_NAME))?;
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
    // Merged on cwd, either session's end walks the shared sprite out.
    let key = obj
        .get("sessionId")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            // The fallback is for OLD builds that carried no id, and an upstream
            // RENAME is indistinguishable from one — but its blast radius is
            // every session merging onto cwd, so it must not be silent.
            crate::source::drift::missing_field(SOURCE_NAME, event, "sessionId");
            cwd
        });
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
            AgentEvent::ActivityStart {
                agent_id,
                tool_use_id: call_id(),
                detail: Some(ToolDetail::Task),
            },
        ]),
        "SubagentStop" => Ok(vec![AgentEvent::ActivityEnd {
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
                AgentEvent::ActivityStart {
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
                events.push(AgentEvent::ActivityStart {
                    agent_id,
                    tool_use_id: None,
                    detail: Some(ToolDetail::Task),
                });
            }
            events.push(AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id: None,
            });
            Ok(events)
        }
        // Deliberately Identity-LESS: an end doesn't prove a session worth
        // registering, so an unknown id here rides the reducer's fallback.
        "Stop" => Ok(vec![AgentEvent::ActivityEnd {
            agent_id,
            tool_use_id: None,
        }]),
        "Notification" => {
            // Every producer waits on the user: the approval gate, a question,
            // Auto Guard (`internal/session/control/approval_orchestration.go:311`,
            // `:621`, `recovery.go:341`).
            let msg = obj
                .get("message")
                .and_then(|s| s.as_str())
                .unwrap_or("waiting");
            Ok(vec![
                register(),
                AgentEvent::Waiting {
                    agent_id,
                    reason: ellipsize(msg, MAX_DECODED_FIELD_CHARS),
                    tool_use_id: None,
                },
            ])
        }
        // The STRUCTURED approval gate: fires BEFORE the prompt with the tool +
        // subject, and ALONGSIDE `Notification` for the same approval — both →
        // Waiting is idempotent (the gated tool's PostToolUse/Stop resolves it).
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
                AgentEvent::Waiting {
                    agent_id,
                    reason: ellipsize(&reason, MAX_DECODED_FIELD_CHARS),
                    tool_use_id: None,
                },
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
    use serde_json::json;

    fn decode_all(v: Value) -> Vec<AgentEvent> {
        decode_rx_hook_payload(&v).expect("decodes")
    }

    fn decode(v: Value) -> AgentEvent {
        decode_all(v).pop().expect("at least one event")
    }

    /// An absent `sessionId` is breadcrumbed, not merged in silence.
    ///
    /// The cwd fallback stays for old builds, but an upstream RENAME looks
    /// identical to one and collapses every session in a workspace onto a
    /// single sprite — so the drop has to say so.
    #[test]
    fn a_missing_session_id_breadcrumbs_before_falling_back_to_cwd() {
        let payload = json!({"event": "SessionStart", "cwd": "/repo"});
        let mut evs = 0;
        let logs = crate::test_capture::capture_logs(|| {
            evs = decode_rx_hook_payload(&payload).expect("decodes").len();
        });
        assert!(evs > 0, "the fallback still decodes the event");
        assert!(
            logs.contains(crate::source::drift::TARGET),
            "an absent sessionId must breadcrumb; got {logs:?}"
        );

        let with_id = json!({"event": "SessionStart", "cwd": "/repo", "sessionId": "s1"});
        let quiet = crate::test_capture::capture_logs(|| {
            decode_rx_hook_payload(&with_id).expect("decodes");
        });
        assert!(
            !quiet.contains(crate::source::drift::TARGET),
            "a present sessionId must stay quiet; got {quiet:?}"
        );
    }

    #[test]
    fn two_sessions_in_one_workspace_stay_two_agents() {
        let ev = |sid: &str| {
            decode_rx_hook_payload(&json!({
                "event": "SessionStart", "cwd": "/repo", "sessionId": sid
            }))
            .expect("decodes")
            .remove(0)
        };
        let (a, b) = (ev("s-1"), ev("s-2"));
        assert_ne!(a.agent_id(), b.agent_id(), "one project, two sessions");
        assert_eq!(a.agent_id(), AgentId::from_parts(SOURCE_NAME, "s-1"));
    }

    #[test]
    fn a_build_without_a_session_id_still_keys_on_cwd() {
        let ev = decode_rx_hook_payload(&json!({"event": "SessionStart", "cwd": "/repo"}))
            .expect("decodes")
            .remove(0);
        assert_eq!(ev.agent_id(), AgentId::from_parts(SOURCE_NAME, "/repo"));
    }

    #[test]
    fn session_start_falls_back_to_cwd_without_a_session_id() {
        let ev = decode(json!({
            "event": "SessionStart",
            "cwd": "/Users/dev/zirs"
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
                assert_eq!(
                    agent_id,
                    AgentId::from_parts(SOURCE_NAME, "/Users/dev/zirs")
                );
                assert_eq!(cwd, std::path::PathBuf::from("/Users/dev/zirs"));
                assert_eq!(parent_id, None);
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn user_prompt_submit_is_the_resurrect_session_start() {
        let ev = decode(json!({
            "event": "UserPromptSubmit",
            "cwd": "/Users/dev/zirs",
            "prompt": "add a feature"
        }));
        assert!(matches!(ev, AgentEvent::SessionStart { agent_id, .. }
                if agent_id == AgentId::from_parts(SOURCE_NAME, "/Users/dev/zirs")));
    }

    #[test]
    fn pre_tool_use_is_activity_start_with_no_tool_id() {
        let ev = decode(json!({
            "event": "PreToolUse",
            "cwd": "/repo",
            "toolName": "read_file",
            "toolArgs": {"path": "src/main.go"}
        }));
        match ev {
            AgentEvent::ActivityStart {
                tool_use_id,
                detail,
                ..
            } => {
                assert_eq!(tool_use_id, None);
                assert_eq!(detail.unwrap().display(), "read_file: src/main.go");
            }
            other => panic!("expected ActivityStart, got {other:?}"),
        }
    }

    #[test]
    fn tool_target_uses_reasonix_arg_vocabulary() {
        let bash = decode(json!({
            "event": "PreToolUse", "cwd": "/r",
            "toolName": "bash", "toolArgs": {"command": "go test ./..."}
        }));
        assert!(
            matches!(bash, AgentEvent::ActivityStart { detail: Some(d), .. }
            if d.display() == "bash: go test ./...")
        );
        let grep = decode(json!({
            "event": "PreToolUse", "cwd": "/r",
            "toolName": "grep", "toolArgs": {"pattern": "TODO", "path": "src"}
        }));
        assert!(
            matches!(grep, AgentEvent::ActivityStart { detail: Some(d), .. }
            if d.display() == "grep: src")
        );
    }

    #[test]
    fn long_targets_are_truncated() {
        let long = "x".repeat(60);
        let ev = decode(json!({
            "event": "PreToolUse", "cwd": "/r",
            "toolName": "bash", "toolArgs": {"command": long}
        }));
        match ev {
            AgentEvent::ActivityStart {
                detail: Some(d), ..
            } => {
                let display = d.display();
                assert!(display.starts_with("bash: "));
                assert!(display.ends_with('…'));
                assert_eq!(display.chars().count(), "bash: ".chars().count() + 41);
            }
            other => panic!("expected ActivityStart, got {other:?}"),
        }
    }

    #[test]
    fn long_tool_name_is_truncated_at_the_decode_boundary() {
        let long = "T".repeat(MAX_DECODED_FIELD_CHARS * 3);
        let ev = decode(json!({
            "event": "PreToolUse", "cwd": "/r",
            "toolName": long, "toolArgs": {}
        }));
        match ev {
            AgentEvent::ActivityStart {
                detail: Some(d), ..
            } => {
                let display = d.display();
                assert!(
                    display.ends_with('…'),
                    "name should be ellipsized: {display}"
                );
                assert_eq!(display.chars().count(), MAX_DECODED_FIELD_CHARS + 1);
            }
            other => panic!("expected ActivityStart, got {other:?}"),
        }
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
                "event": "PreToolUse", "cwd": "/r",
                "toolName": tool, "toolArgs": {"prompt": "do a thing"}
            }));
            assert!(
                matches!(&ev, AgentEvent::ActivityStart { detail: Some(d), .. } if d.is_task()),
                "{tool} must map to ToolDetail::Task"
            );
        }
        for (tool, args) in [
            ("run_skill", json!({"name": "lint"})),
            ("task", json!({"prompt": "do a thing"})),
        ] {
            let ev = decode(json!({
                "event": "PreToolUse", "cwd": "/r", "toolName": tool, "toolArgs": args
            }));
            assert!(
                matches!(&ev, AgentEvent::ActivityStart { detail: Some(d), .. } if !d.is_task()),
                "{tool} must not map to ToolDetail::Task"
            );
        }
    }

    #[test]
    fn post_tool_use_and_stop_are_activity_end() {
        for event in ["PostToolUse", "Stop"] {
            let ev = decode(json!({"event": event, "cwd": "/r"}));
            assert!(
                matches!(
                    &ev,
                    AgentEvent::ActivityEnd {
                        tool_use_id: None,
                        ..
                    }
                ),
                "{event} must decode to ActivityEnd with no tool id"
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
                    AgentEvent::ActivityStart { tool_use_id: None, detail: Some(d), .. },
                    AgentEvent::ActivityEnd { tool_use_id: None, .. },
                ] if d.is_task()
            ),
            "{:?}",
            post("explore")
        );
        assert!(
            matches!(
                post("read_file").as_slice(),
                [_, AgentEvent::ActivityEnd { .. }]
            ),
            "{:?}",
            post("read_file")
        );
    }

    #[test]
    fn notification_maps_to_waiting_with_message() {
        let ev = decode(json!({
            "event": "Notification",
            "cwd": "/r",
            "message": "approval needed: bash rm -rf ./build"
        }));
        assert!(matches!(ev, AgentEvent::Waiting { reason, .. }
            if reason == "approval needed: bash rm -rf ./build"));
    }

    #[test]
    fn session_end_maps_to_session_end() {
        let ev = decode(json!({"event": "SessionEnd", "cwd": "/r"}));
        assert!(matches!(ev, AgentEvent::SessionEnd { .. }));
    }

    #[test]
    fn permission_request_maps_to_waiting_with_tool_and_subject() {
        let ev = decode(json!({
            "event": "PermissionRequest",
            "cwd": "/r",
            "toolName": "bash",
            "subject": "rm -rf ./build",
            "toolArgs": {"command": "rm -rf ./build"}
        }));
        assert!(matches!(ev, AgentEvent::Waiting { reason, .. }
            if reason == "approval: bash rm -rf ./build"));
    }

    #[test]
    fn permission_request_without_subject_uses_tool_only() {
        let ev = decode(json!({
            "event": "PermissionRequest", "cwd": "/r", "toolName": "write_file"
        }));
        assert!(matches!(ev, AgentEvent::Waiting { reason, .. }
            if reason == "approval: write_file"));
    }

    #[test]
    fn permission_request_without_tool_falls_back() {
        let ev = decode(json!({"event": "PermissionRequest", "cwd": "/r"}));
        assert!(matches!(ev, AgentEvent::Waiting { reason, .. }
            if reason == "approval required"));
    }

    #[test]
    fn permission_request_reason_is_capped_at_the_decode_boundary() {
        let long = "é".repeat(MAX_DECODED_FIELD_CHARS * 10);
        let ev = decode(json!({
            "event": "PermissionRequest", "cwd": "/r",
            "toolName": "bash", "subject": long
        }));
        match &ev {
            AgentEvent::Waiting { reason, .. } => {
                assert_eq!(reason.chars().count(), MAX_DECODED_FIELD_CHARS + 1);
                assert!(reason.ends_with('…'));
            }
            other => panic!("expected Waiting, got {other:?}"),
        }
    }

    #[test]
    fn without_a_session_id_one_cwd_is_still_one_agent() {
        let events = [
            json!({"event": "SessionStart", "cwd": "/Users/dev/p"}),
            json!({"event": "UserPromptSubmit", "cwd": "/Users/dev/p"}),
            json!({"event": "PreToolUse", "cwd": "/Users/dev/p", "toolName": "bash",
                   "toolArgs": {"command": "ls"}}),
            json!({"event": "PostToolUse", "cwd": "/Users/dev/p", "toolName": "bash"}),
            json!({"event": "PermissionRequest", "cwd": "/Users/dev/p", "toolName": "bash", "subject": "ls"}),
            json!({"event": "Notification", "cwd": "/Users/dev/p", "message": "approval needed: bash ls"}),
            json!({"event": "Stop", "cwd": "/Users/dev/p"}),
            json!({"event": "SessionEnd", "cwd": "/Users/dev/p"}),
        ];
        let ids: std::collections::BTreeSet<_> = events
            .iter()
            .flat_map(|v| decode_rx_hook_payload(v).unwrap())
            .map(|e| e.agent_id())
            .collect();
        assert_eq!(ids.len(), 1, "all events must coalesce to one AgentId");
    }

    #[test]
    fn activity_arms_prepend_identity_with_the_session_key() {
        let payloads = [
            json!({"event": "PreToolUse", "cwd": "/Users/dev/p", "toolName": "bash",
                   "toolArgs": {"command": "ls"}}),
            json!({"event": "PostToolUse", "cwd": "/Users/dev/p", "toolName": "bash"}),
            json!({"event": "Notification", "cwd": "/Users/dev/p", "message": "approval needed"}),
            json!({"event": "PermissionRequest", "cwd": "/Users/dev/p", "toolName": "bash", "subject": "ls"}),
        ];
        for payload in payloads {
            let name = payload["event"].clone();
            let events = decode_all(payload);
            assert_eq!(events.len(), 2, "{name}: Identity + activity");
            match &events[0] {
                AgentEvent::Identity {
                    agent_id,
                    source,
                    session_id,
                    cwd,
                    pid: None,
                } => {
                    assert_eq!(*agent_id, AgentId::from_parts(SOURCE_NAME, "/Users/dev/p"));
                    assert_eq!(source, SOURCE_NAME);
                    assert_eq!(
                        session_id, "/Users/dev/p",
                        "no sessionId on the wire, so cwd is the key"
                    );
                    assert_eq!(
                        cwd.as_deref(),
                        Some(std::path::Path::new("/Users/dev/p")),
                        "rx hooks always know their cwd"
                    );
                }
                other => panic!("{name}: expected leading Identity, got {other:?}"),
            }
        }
    }

    #[test]
    fn stop_session_events_and_session_end_carry_no_identity() {
        for payload in [
            json!({"event": "Stop", "cwd": "/r"}),
            json!({"event": "SessionStart", "cwd": "/r"}),
            json!({"event": "UserPromptSubmit", "cwd": "/r"}),
            json!({"event": "SessionEnd", "cwd": "/r"}),
        ] {
            let name = payload["event"].clone();
            let events = decode_all(payload);
            assert_eq!(events.len(), 1, "{name}: exactly one event");
            assert!(
                !matches!(events[0], AgentEvent::Identity { .. }),
                "{name} must not emit Identity"
            );
        }
    }

    #[test]
    fn empty_or_missing_cwd_is_malformed() {
        assert!(decode_rx_hook_payload(&json!({"event": "Stop", "cwd": ""})).is_err());
        assert!(decode_rx_hook_payload(&json!({"event": "Stop"})).is_err());
    }

    #[test]
    fn unknown_event_bails_loudly() {
        for ev in ["PostLLMCall", "PreCompact", "PostToolUseFailure", "Bogus"] {
            assert!(
                decode_rx_hook_payload(&json!({"event": ev, "cwd": "/r"})).is_err(),
                "{ev} must bail (not registered, must not decode silently)"
            );
        }
    }

    #[test]
    fn non_object_payload_is_malformed() {
        assert!(decode_rx_hook_payload(&json!("just a string")).is_err());
        assert!(decode_rx_hook_payload(&json!(42)).is_err());
    }

    #[test]
    fn notification_without_message_falls_back_to_waiting() {
        let ev = decode(json!({"event": "Notification", "cwd": "/r"}));
        assert!(matches!(ev, AgentEvent::Waiting { reason, .. } if reason == "waiting"));
    }

    #[test]
    fn pre_tool_use_without_tool_name_displays_question_mark() {
        let ev = decode(json!({"event": "PreToolUse", "cwd": "/r"}));
        assert!(
            matches!(ev, AgentEvent::ActivityStart { detail: Some(d), .. }
            if d.display() == "?")
        );
    }

    #[test]
    fn tool_args_spoofed_subagent_type_does_not_make_task() {
        let ev = decode(json!({
            "event": "PreToolUse", "cwd": "/r",
            "toolName": "read_file",
            "toolArgs": {"path": "x.go", "subagent_type": null}
        }));
        assert!(
            matches!(&ev, AgentEvent::ActivityStart { detail: Some(d), .. } if !d.is_task()),
            "spoofed subagent_type must stay Generic"
        );
    }

    #[test]
    fn notification_reason_is_capped_at_the_decode_boundary() {
        let long = "é".repeat(MAX_DECODED_FIELD_CHARS * 10);
        let ev = decode(json!({
            "event": "Notification",
            "cwd": "/r",
            "message": long
        }));
        match &ev {
            AgentEvent::Waiting { reason, .. } => {
                assert_eq!(reason.chars().count(), MAX_DECODED_FIELD_CHARS + 1);
                assert!(reason.ends_with('…'));
            }
            other => panic!("expected Waiting, got {other:?}"),
        }
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
                |e| matches!(e, AgentEvent::ActivityStart { agent_id, .. } if *agent_id == child)
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
            matches!(&start, AgentEvent::ActivityStart { agent_id, tool_use_id, detail: Some(d) }
                if *agent_id == parent && *tool_use_id == call && d.is_task()),
            "{start:?}"
        );
        let stop = decode(json!({
            "event": "SubagentStop", "cwd": "/r", "sessionId": "p", "callId": "call_1"
        }));
        assert_eq!(
            stop,
            AgentEvent::ActivityEnd {
                agent_id: parent,
                tool_use_id: call
            }
        );
    }
}
