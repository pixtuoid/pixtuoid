//! CodeWhale source — HOOK-ONLY. There is no tailable per-session transcript
//! to watch: `rollout_path` is never written in production, saved sessions are
//! full-snapshot `{id}.json` rewrites, and headless `codewhale exec` fires no
//! session lifecycle. Cites are `codewhale-hq/Codewhale` at `v0.10.2`.
//!
//! CodeWhale's hooks DON'T hand the command a CC-shaped JSON payload on stdin —
//! identity travels as ENV VARS (`docs/HOOKS.md:291-315`). So the shim runs in
//! **env-mode** (`pixtuoid-hook --event <name>`): it reads the env vars and
//! synthesizes this envelope, stamped `_pixtuoid_source: "codewhale"`:
//!
//! ```json
//! {"event":"tool_call_before","session_id":"sess_1a2b3c4d","cwd":"/repo","tool":"exec_shell","tool_args":"{\"command\":\"ls -la\"}","tool_call_id":"7f7a…"}
//! ```
//!
//! - **Key on `session_id`.** Every event of one TUI session carries the same
//!   `CODEWHALE_SESSION_ID` (`docs/HOOKS.md:278-285`), so two sessions in one
//!   workspace are two sprites. A tool call's before and after share
//!   `CODEWHALE_TOOL_CALL_ID` (`crates/tui/src/hooks/executor.rs:371-374`).
//!
//! - **[`AgentEvent::UserActionRequest`].** `waiting_for_user` fires as an approval prompt opens, a
//!   question is asked or a goal continuation parks (`docs/HOOKS.md:248`); its
//!   `reason` rides stdin (`:251-252`), which env-mode never reads, so every wait
//!   reads alike. `session_idle` fires as a turn or a wait settles to idle
//!   (`crates/tui/src/tui/ui/observer_hooks.rs:178-196`): the end of a wait no
//!   tool end resolves.
//!
//! - **Exit profile.** `session_end` fires on a clean quit, so
//!   `has_exit_signal: true`. An ABRUPT exit fires none — the shim resolves
//!   CodeWhale's pid by walking past the runner's shell (on Windows too, #528)
//!   and `hook::HookPidWatch` ends the sprite when that pid dies.

use crate::source::decoder::{DecodeError, DecodeResult as Result};
use serde_json::Value;

use crate::AgentId;
use crate::source::registry::{
    ChildEndInference, Corroboration, FocusChannel, HookDecoding, HookObject, Hyphen, SourceCaps,
    SourceDescriptor, SourceKind,
};
use crate::source::{AgentEvent, RequestKeys, ToolDetail};

/// The CodeWhale CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "codewhale";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    label_prefix: "cw",
    verified_version: "0.10.2",
    version_probe: Some(&["codewhale", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll(decode_cw_hook_payload)),
        caps: SourceCaps {
            // session_end fires on a clean TUI quit — best-effort counts.
            has_exit_signal: true,
            // message_submit re-emits SessionStart, so a swept-but-live session
            // walks back in on the next prompt.
            resurrects_on_prompt: true,
            // The recorded dispatch fires nothing of the child's own between
            // `subagent_spawn` and `subagent_complete`.
            delegations_are_hook_silent: true,
            // `agent start` returns at dispatch, its child running on until its
            // own `subagent_complete`.
            child_end_inference: ChildEndInference::None,
            child_hooks_unnamed: false,
            brackets_turns: false,
        },
        focus: FocusChannel::ShimStamp(Corroboration::Twice),
    },
    #[cfg(feature = "native")]
    native: crate::source::registry::Native::NONE,
};

/// CodeWhale's one sub-agent tool; its `action` picks the verb
/// (`parse_agent_tool_action`, `codewhale-hq/Codewhale`
/// `crates/tui/src/tools/subagent/mod.rs`). The CHILD gets its own sprite from the
/// `subagent_spawn`/`subagent_complete` observer hooks.
pub(crate) const SUBAGENT_TOOL: &str = "agent";

/// The [`SUBAGENT_TOOL`] actions that hand the turn to a child, so the PARENT
/// reads "Delegating": upstream's `Start` aliases dispatch one and its `Wait`
/// aliases block on it, while the other actions only read or steer one.
pub(crate) const DELEGATING_ACTIONS: &[&str] =
    &["start", "spawn", "run", "wait", "join", "await", "block"];

/// Whether an [`SUBAGENT_TOOL`] call is one of the [`DELEGATING_ACTIONS`].
/// Mirrors upstream's resolver: the first non-null, non-blank `action`/`op`,
/// case-folded, and start when absent.
fn agent_call_delegates(args: Option<&Value>) -> bool {
    let action = ["action", "op"]
        .into_iter()
        .find_map(|k| match args?.get(k)? {
            Value::Null => None,
            Value::String(s) if s.trim().is_empty() => None,
            v => Some(v),
        });
    action.is_none_or(|v| {
        v.as_str()
            .is_some_and(|a| DELEGATING_ACTIONS.contains(&a.trim().to_ascii_lowercase().as_str()))
    })
}

/// Decode one CodeWhale hook envelope (already identified by
/// `_pixtuoid_source == "codewhale"`), keyed on `session_id`. An unhandled event
/// bails: registered-vs-decoded drift must be loud, not a silent drop.
///
/// The activity arms prepend an [`AgentEvent::Identity`]: CodeWhale is
/// HOOK-ONLY, so a slot the reducer's proof-of-life pre-pass synthesizes
/// mid-turn has no JSONL back-fill path.
///
/// # Errors
///
/// If the payload lacks `event`, names an unhandled event, or lacks a non-empty field its event carries.
pub(crate) fn decode_cw_hook_payload(obj: &HookObject) -> Result<Vec<AgentEvent>> {
    let event = obj
        .get("event")
        .and_then(|s| s.as_str())
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "event"))?;

    // Subagent observer hooks are forwarded RAW from CodeWhale's stdin, so they
    // carry CodeWhale's OWN field names and no `cwd` at all — they must be
    // handled before the requirements below.
    if let Some(events) = decode_cw_subagent(event, obj)? {
        return Ok(events);
    }

    let session = required(obj, event, "session_id")?;
    let cwd = required(obj, event, "cwd")?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, session);
    let identity = || AgentEvent::identity(agent_id, SOURCE_NAME, session, Some(cwd.into()));
    let tool_use_id = || {
        obj.get("tool_call_id")
            .and_then(|s| s.as_str())
            .map(str::to_owned)
    };

    match event {
        // message_submit (every prompt) is the RESURRECT path — a stale-swept
        // session walks back in on its next prompt, CodeWhale's only
        // re-creation signal. The reducer ignores it when the slot exists.
        "session_start" | "message_submit" => Ok(vec![AgentEvent::SessionStart {
            agent_id,
            source: SOURCE_NAME.to_string(),
            session_id: session.to_string(),
            cwd: cwd.into(),
            parent_id: None,
        }]),
        "tool_call_before" => {
            let tool = obj.get("tool").and_then(|s| s.as_str()).unwrap_or_else(|| {
                crate::source::drift::missing_field(SOURCE_NAME, "tool_call_before", "tool");
                "?"
            });
            Ok(vec![
                identity(),
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id: tool_use_id(),
                    detail: Some(cw_tool_detail(tool, obj.get("tool_args"))),
                },
            ])
        }
        "tool_call_after" => Ok(vec![
            identity(),
            AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id: tool_use_id(),
            },
        ]),
        // Its exit hook is submitted at least one loop pass later, and both drain one FIFO across
        // `OBSERVER_DISPATCH_WORKERS` threads (`event_loop.rs:2087`, `observer_hooks.rs:178-196`,
        // `hooks/executor.rs:1189-1250`), so only a cancel pending as the dialog opens can land
        // this after its exit; the next tool start, `session_idle` or end clears that RequiresAction.
        "waiting_for_user" => Ok(vec![
            identity(),
            AgentEvent::user_action_request(agent_id, None, RequestKeys::default()),
        ]),
        "session_idle" => Ok(vec![identity(), AgentEvent::turn_end(agent_id)]),
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

/// `key`'s non-empty string, which `event`'s payload carries.
fn required<'a>(obj: &'a HookObject, event: &str, key: &'static str) -> Result<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| DecodeError::missing_in(SOURCE_NAME, event, key))
}

/// CodeWhale's `subagent_spawn` / `subagent_complete` observer hooks, forwarded
/// RAW from stdin so the payload is CodeWhale's own shape: `agent_id` = the
/// CHILD, `session_id` = its parent's session, `workspace` = the parent's cwd
/// (`docs/HOOKS.md:648-665`). `Ok(None)` for any other event.
fn decode_cw_subagent(event: &str, obj: &HookObject) -> Result<Option<Vec<AgentEvent>>> {
    let is_spawn = match event {
        "subagent_spawn" => true,
        "subagent_complete" => false,
        _ => return Ok(None),
    };
    let child = required(obj, event, "agent_id")?;
    let child_id = AgentId::from_parts(SOURCE_NAME, child);

    if !is_spawn {
        // `as_child: true` puts the parent link + cascade on the reducer's
        // child ledger / scope tree.
        return Ok(Some(vec![AgentEvent::SessionEnd {
            agent_id: child_id,
            as_child: true,
        }]));
    }

    let parent = required(obj, event, "session_id")?;
    Ok(Some(vec![AgentEvent::SessionStart {
        agent_id: child_id,
        source: SOURCE_NAME.to_string(),
        session_id: child.to_string(),
        cwd: required(obj, event, "workspace")?.into(),
        parent_id: Some(AgentId::from_parts(SOURCE_NAME, parent)),
    }]))
}

/// CodeWhale-side tool detail: the dispatch is keyed on tool name and action,
/// because CodeWhale args carry no `subagent_type` for the shared semantic
/// detection to see. `tool_args` arrives as the raw `DEEPSEEK_TOOL_ARGS` JSON
/// STRING, so it is parsed first.
fn cw_tool_detail(tool: &str, raw_args: Option<&Value>) -> ToolDetail {
    let parsed: Option<Value> = raw_args
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str(s).ok());
    if tool == SUBAGENT_TOOL && agent_call_delegates(parsed.as_ref()) {
        return ToolDetail::Task;
    }
    const KEYS: &[&str] = &["command", "file_path", "path", "pattern", "url"];
    crate::source::decoder::generic_keyed_detail(tool, parsed.as_ref(), KEYS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;
    use serde_json::json;

    fn decode_all(v: Value) -> Vec<AgentEvent> {
        decode_cw_hook_payload(test_obj(&v)).expect("decodes")
    }

    /// The payload's MAIN event: the LAST decoded one, since activity arms
    /// prepend an `Identity`.
    fn decode(v: Value) -> AgentEvent {
        decode_all(v).pop().expect("at least one event")
    }

    #[test]
    fn only_an_agent_call_that_hands_off_the_turn_is_a_task() {
        let is_task = |tool: &str, args: Value| {
            matches!(
                decode(json!({
                    "event": "tool_call_before", "session_id": "s", "cwd": "/r",
                    "tool": tool, "tool_args": args.to_string()
                })),
                AgentEvent::ToolUseStart { detail: Some(d), .. } if d.is_task()
            )
        };
        for args in [
            json!({"prompt": "x"}),
            json!({"action": "start"}),
            json!({"action": " Wait "}),
            json!({"action": "", "op": "join"}),
            json!({"action": null, "op": "spawn"}),
        ] {
            assert!(is_task("agent", args.clone()), "{args}");
        }
        for args in [
            json!({"action": "status"}),
            json!({"action": "peek"}),
            json!({"action": "cancel"}),
            json!({"op": "roster"}),
            json!({"action": 1}),
        ] {
            assert!(!is_task("agent", args.clone()), "{args}");
        }
        for tool in ["agent_spawn", "spawn_agent", "read_file"] {
            assert!(!is_task(tool, json!({"prompt": "x"})), "{tool}");
        }
    }

    #[test]
    fn an_empty_or_missing_session_or_cwd_is_malformed() {
        for v in [
            json!({"event": "session_end", "session_id": "s", "cwd": ""}),
            json!({"event": "session_end", "session_id": "s"}),
            json!({"event": "session_end", "session_id": "", "cwd": "/r"}),
            json!({"event": "session_end", "cwd": "/r"}),
        ] {
            assert!(decode_cw_hook_payload(test_obj(&v)).is_err(), "{v}");
        }
    }

    #[test]
    fn unknown_event_bails_loudly() {
        for ev in ["turn_end", "mode_change", "on_error", "shell_env", "Bogus"] {
            assert!(
                decode_cw_hook_payload(test_obj(
                    &json!({"event": ev, "session_id": "s", "cwd": "/r"})
                ))
                .is_err(),
                "{ev} must bail (not registered, must not decode silently)"
            );
        }
    }

    #[test]
    fn subagent_spawn_registers_a_child_parented_to_its_session_sprite() {
        let ev = decode(json!({
            "event": "subagent_spawn",
            "agent_id": "agent-abc123",
            "session_id": "sess_dead",
            "workspace": "/Users/dev/cwproj",
            "prompt_preview": "investigate X"
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
                assert_eq!(agent_id, AgentId::from_parts(SOURCE_NAME, "agent-abc123"));
                assert_eq!(
                    parent_id,
                    Some(AgentId::from_parts(SOURCE_NAME, "sess_dead")),
                    "parent link is the session-keyed sprite (= the session_start/message_submit agent)"
                );
                assert_eq!(cwd, std::path::PathBuf::from("/Users/dev/cwproj"));
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn subagent_complete_ends_the_child_as_a_child() {
        let ev = decode(json!({
            "event": "subagent_complete",
            "agent_id": "agent-abc123",
            "session_id": "sess_dead",
            "workspace": "/Users/dev/cwproj",
            "status": "completed"
        }));
        match ev {
            AgentEvent::SessionEnd { agent_id, as_child } => {
                assert_eq!(agent_id, AgentId::from_parts(SOURCE_NAME, "agent-abc123"));
                assert!(
                    as_child,
                    "subagent_complete is a CHILD end (drives the scope cascade)"
                );
            }
            other => panic!("expected SessionEnd, got {other:?}"),
        }
    }

    #[test]
    fn a_subagent_event_missing_a_field_it_carries_is_malformed() {
        let spawn = json!({
            "event": "subagent_spawn", "agent_id": "agent-xy",
            "session_id": "sess_dead", "workspace": "/r"
        });
        assert!(decode_cw_hook_payload(test_obj(&spawn)).is_ok());
        for key in ["agent_id", "session_id", "workspace"] {
            let mut v = spawn.clone();
            v[key] = json!("");
            assert!(decode_cw_hook_payload(test_obj(&v)).is_err(), "{key}");
        }
        assert!(decode_cw_hook_payload(test_obj(&json!({"event": "subagent_complete"}))).is_err());
    }

    #[test]
    fn tool_call_before_without_tool_displays_question_mark() {
        let ev = decode(json!({"event": "tool_call_before", "session_id": "s", "cwd": "/r"}));
        assert!(
            matches!(ev, AgentEvent::ToolUseStart { detail: Some(d), .. }
            if d.display() == "?")
        );
    }

    #[test]
    fn malformed_tool_args_string_degrades_to_no_target() {
        let ev = decode(json!({
            "event": "tool_call_before", "session_id": "s", "cwd": "/r",
            "tool": "exec_shell", "tool_args": "not json {"
        }));
        assert!(
            matches!(ev, AgentEvent::ToolUseStart { detail: Some(d), .. }
            if d.display() == "exec_shell")
        );
    }
}
