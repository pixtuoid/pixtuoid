//! CodeWhale source — HOOK-ONLY. There is no tailable per-session transcript
//! to watch: `rollout_path` is never written in production, saved sessions are
//! full-snapshot `{id}.json` rewrites, and headless `codewhale exec` fires no
//! hooks at all.
//!
//! CodeWhale's hooks DON'T hand the command a CC-shaped JSON payload on stdin —
//! identity travels as `DEEPSEEK_*` ENV VARS, and only
//! `message_submit`/`turn_end`/`subagent_*` pass any stdin. So the shim runs in
//! **env-mode** (`pixtuoid-hook --event <name>`): it reads the env vars and
//! synthesizes this envelope, stamped `_pixtuoid_source: "codewhale"`:
//!
//! ```json
//! {"event":"tool_call_before","cwd":"/repo","tool":"exec_shell","tool_args":"{\"command\":\"ls -la\"}"}
//! ```
//!
//! - **Key on `cwd`, NOT `session_id`.** `DEEPSEEK_SESSION_ID` is INCONSISTENT
//!   across a single session's events — `sess_<8hex>` on session/turn/tool-after
//!   events but a raw turn UUID on `tool_call_before`, which a different code
//!   path builds — so keying on it splits every ActivityStart into a second
//!   ghost sprite. `DEEPSEEK_WORKSPACE` (the cwd) is the ONE field present and
//!   identical on EVERY event. Two deliberate consequences: two concurrent
//!   sessions in ONE workspace render as one sprite, and `tool_use_id` is always
//!   `None` (harmless under a single transport with no JSONL twin).
//!
//! - **No Waiting/permission state.** CodeWhale exposes no approval hook to the
//!   TUI shell-command system, so a tool parked on an approval prompt shows
//!   Active until the user approves. No signal exists to do better.
//!
//! - **Exit profile.** `session_end` fires on a clean quit, so
//!   `has_exit_signal: true`. An ABRUPT exit fires none — the shim resolves
//!   CodeWhale's pid by walking past the runner's shell (on Windows too, #528)
//!   and `hook::HookPidWatch` ends the sprite when that pid dies.

use crate::source::decoder::{DecodeError, DecodeResult as Result};
use serde_json::Value;

use crate::AgentId;
use crate::source::registry::HookObject;
use crate::source::{AgentEvent, ToolDetail};

/// The CodeWhale CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "codewhale";

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
/// `_pixtuoid_source == "codewhale"`), keyed on `cwd`. An unhandled event
/// bails: registered-vs-decoded drift must be loud, not a silent drop.
///
/// The activity arms prepend an [`AgentEvent::Identity`]: CodeWhale is
/// HOOK-ONLY, so a slot the reducer's proof-of-life pre-pass synthesizes
/// mid-turn has no JSONL back-fill path.
///
/// # Errors
///
/// If the payload lacks `event`, names an unhandled event, or lacks a non-empty `cwd` (a subagent event needs a non-empty `agent_id` instead).
pub(crate) fn decode_cw_hook_payload(obj: &HookObject) -> Result<Vec<AgentEvent>> {
    let event = obj
        .get("event")
        .and_then(|s| s.as_str())
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "event"))?;

    // Subagent observer hooks are forwarded RAW from CodeWhale's stdin, so they
    // carry CodeWhale's OWN field names and no `cwd` at all — they must be
    // handled before the cwd requirement below.
    if let Some(events) = decode_cw_subagent(event, obj)? {
        return Ok(events);
    }

    // An empty cwd would mint a phantom agent nothing coalesces with.
    let cwd = obj
        .get("cwd")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "cwd"))?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, cwd);

    // No usable upstream session id exists; the cwd IS the session key, so this
    // mirrors the SessionStart arm and coalescing holds.
    let identity = || AgentEvent::identity(agent_id, SOURCE_NAME, cwd, Some(cwd.into()));

    match event {
        // message_submit (every prompt) is the RESURRECT path — a stale-swept
        // session walks back in on its next prompt, CodeWhale's only
        // re-creation signal. The reducer ignores it when the slot exists.
        "session_start" | "message_submit" => Ok(vec![AgentEvent::SessionStart {
            agent_id,
            source: SOURCE_NAME.to_string(),
            session_id: cwd.to_string(),
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
                AgentEvent::ActivityStart {
                    agent_id,
                    tool_use_id: None,
                    detail: Some(cw_tool_detail(tool, obj.get("tool_args"))),
                },
            ])
        }
        "tool_call_after" => Ok(vec![
            identity(),
            AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id: None,
            },
        ]),
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

/// CodeWhale's `subagent_spawn` / `subagent_complete` observer hooks, forwarded
/// RAW from stdin so the payload is CodeWhale's own shape: `agent_id` = the
/// CHILD, `workspace` = the parent's cwd. `Ok(None)` for any other event.
///
/// The child is keyed on its `agent_id` and parent-linked to the
/// WORKSPACE-keyed parent sprite — a MIXED keying, because a subagent runs in
/// the same workspace as its parent, so cwd-keying alone would coalesce it INTO
/// the parent.
fn decode_cw_subagent(
    event: &str,
    obj: &serde_json::Map<String, Value>,
) -> Result<Option<Vec<AgentEvent>>> {
    let is_spawn = match event {
        "subagent_spawn" => true,
        "subagent_complete" => false,
        _ => return Ok(None),
    };
    let child = obj
        .get("agent_id")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| DecodeError::missing_in(SOURCE_NAME, event, "agent_id"))?;
    let child_id = AgentId::from_parts(SOURCE_NAME, child);

    if !is_spawn {
        // `as_child: true` puts the parent link + cascade on the reducer's
        // child ledger / scope tree.
        return Ok(Some(vec![AgentEvent::SessionEnd {
            agent_id: child_id,
            as_child: true,
        }]));
    }

    // `workspace` is OPTIONAL: if CodeWhale hasn't resolved it yet, register the
    // child as a parentless root rather than dropping it — it still shows, just
    // not nested.
    let workspace = obj
        .get("workspace")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty());
    let parent_id = workspace.map(|ws| AgentId::from_parts(SOURCE_NAME, ws));
    Ok(Some(vec![AgentEvent::SessionStart {
        agent_id: child_id,
        source: SOURCE_NAME.to_string(),
        session_id: child.to_string(),
        cwd: workspace.unwrap_or("").into(),
        parent_id,
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
    use crate::source::decoder::MAX_TOOL_TARGET_CHARS;
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
    fn session_start_keys_on_cwd() {
        let ev = decode(json!({"event": "session_start", "cwd": "/Users/dev/cwproj"}));
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
                    AgentId::from_parts(SOURCE_NAME, "/Users/dev/cwproj")
                );
                assert_eq!(cwd, std::path::PathBuf::from("/Users/dev/cwproj"));
                assert_eq!(parent_id, None);
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn message_submit_is_the_resurrect_session_start() {
        let ev = decode(json!({
            "event": "message_submit",
            "cwd": "/Users/dev/cwproj"
        }));
        assert!(matches!(ev, AgentEvent::SessionStart { agent_id, .. }
                if agent_id == AgentId::from_parts(SOURCE_NAME, "/Users/dev/cwproj")));
    }

    #[test]
    fn tool_call_before_is_activity_start_with_no_tool_id() {
        let ev = decode(json!({
            "event": "tool_call_before",
            "cwd": "/repo",
            "tool": "exec_shell",
            "tool_args": "{\"command\":\"ls -la\",\"cwd\":\"/repo\"}"
        }));
        match ev {
            AgentEvent::ActivityStart {
                tool_use_id,
                detail,
                ..
            } => {
                assert_eq!(tool_use_id, None);
                assert_eq!(detail.unwrap().display(), "exec_shell: ls -la");
            }
            other => panic!("expected ActivityStart, got {other:?}"),
        }
    }

    #[test]
    fn only_an_agent_call_that_hands_off_the_turn_is_a_task() {
        let is_task = |tool: &str, args: Value| {
            matches!(
                decode(json!({
                    "event": "tool_call_before", "cwd": "/r",
                    "tool": tool, "tool_args": args.to_string()
                })),
                AgentEvent::ActivityStart { detail: Some(d), .. } if d.is_task()
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
    fn tool_call_after_is_activity_end() {
        let ev = decode(json!({
            "event": "tool_call_after", "cwd": "/r", "tool": "exec_shell"
        }));
        assert!(matches!(
            ev,
            AgentEvent::ActivityEnd {
                tool_use_id: None,
                ..
            }
        ));
    }

    #[test]
    fn session_end_maps_to_session_end() {
        let ev = decode(json!({"event": "session_end", "cwd": "/r"}));
        assert!(matches!(
            ev,
            AgentEvent::SessionEnd {
                as_child: false,
                ..
            }
        ));
    }

    #[test]
    fn all_events_for_one_cwd_share_one_agent_id() {
        let events = [
            json!({"event": "session_start", "cwd": "/Users/dev/p"}),
            json!({"event": "message_submit", "cwd": "/Users/dev/p"}),
            json!({"event": "tool_call_before", "cwd": "/Users/dev/p", "tool": "exec_shell",
                   "tool_args": "{\"command\":\"ls\"}"}),
            json!({"event": "tool_call_after", "cwd": "/Users/dev/p", "tool": "exec_shell"}),
            json!({"event": "session_end", "cwd": "/Users/dev/p"}),
        ];
        let ids: std::collections::BTreeSet<_> = events
            .iter()
            .flat_map(|v| decode_cw_hook_payload(test_obj(v)).unwrap())
            .map(|e| e.agent_id())
            .collect();
        assert_eq!(ids.len(), 1, "all events must coalesce to one AgentId");
    }

    #[test]
    fn activity_arms_prepend_identity_with_cwd_keyed_session() {
        for payload in [
            json!({"event": "tool_call_before", "cwd": "/Users/dev/p", "tool": "exec_shell",
                   "tool_args": "{\"command\":\"ls\"}"}),
            json!({"event": "tool_call_after", "cwd": "/Users/dev/p", "tool": "exec_shell"}),
        ] {
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
                    assert_eq!(session_id, "/Users/dev/p", "cwd IS the session key");
                    assert_eq!(
                        cwd.as_deref(),
                        Some(std::path::Path::new("/Users/dev/p")),
                        "cw hooks always know their workspace"
                    );
                }
                other => panic!("{name}: expected leading Identity, got {other:?}"),
            }
        }
    }

    #[test]
    fn session_lifecycle_events_carry_no_separate_identity() {
        for payload in [
            json!({"event": "session_start", "cwd": "/r"}),
            json!({"event": "message_submit", "cwd": "/r"}),
            json!({"event": "session_end", "cwd": "/r"}),
        ] {
            let name = payload["event"].clone();
            let events = decode_all(payload);
            assert_eq!(events.len(), 1, "{name}: exactly one event");
            assert!(
                !matches!(events[0], AgentEvent::Identity { .. }),
                "{name} must not emit a separate Identity"
            );
        }
    }

    #[test]
    fn empty_or_missing_cwd_is_malformed() {
        assert!(
            decode_cw_hook_payload(test_obj(&json!({"event": "session_end", "cwd": ""}))).is_err()
        );
        assert!(decode_cw_hook_payload(test_obj(&json!({"event": "session_end"}))).is_err());
    }

    #[test]
    fn unknown_event_bails_loudly() {
        for ev in ["turn_end", "mode_change", "on_error", "shell_env", "Bogus"] {
            assert!(
                decode_cw_hook_payload(test_obj(&json!({"event": ev, "cwd": "/r"}))).is_err(),
                "{ev} must bail (not registered, must not decode silently)"
            );
        }
    }

    #[test]
    fn subagent_spawn_registers_a_child_parented_to_the_workspace_sprite() {
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
                    Some(AgentId::from_parts(SOURCE_NAME, "/Users/dev/cwproj")),
                    "parent link is the WORKSPACE-keyed sprite (= the session_start/message_submit agent)"
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
    fn subagent_spawn_without_workspace_registers_a_parentless_root() {
        let ev = decode(json!({"event": "subagent_spawn", "agent_id": "agent-xy"}));
        assert!(
            matches!(ev, AgentEvent::SessionStart { parent_id: None, agent_id, .. }
            if agent_id == AgentId::from_parts(SOURCE_NAME, "agent-xy"))
        );
    }

    #[test]
    fn subagent_event_without_agent_id_is_malformed() {
        assert!(
            decode_cw_hook_payload(test_obj(
                &json!({"event": "subagent_spawn", "workspace": "/r"})
            ))
            .is_err()
        );
        assert!(decode_cw_hook_payload(test_obj(&json!({"event": "subagent_complete"}))).is_err());
    }

    #[test]
    fn a_subagent_does_not_coalesce_with_its_workspace_parent() {
        let parent = decode(json!({"event": "session_start", "cwd": "/ws"}));
        let child =
            decode(json!({"event": "subagent_spawn", "agent_id": "agent-1", "workspace": "/ws"}));
        assert_ne!(
            parent.agent_id(),
            child.agent_id(),
            "parent (cwd-keyed) and child (agent_id-keyed) must be distinct sprites"
        );
    }

    #[test]
    fn tool_call_before_without_tool_displays_question_mark() {
        let ev = decode(json!({"event": "tool_call_before", "cwd": "/r"}));
        assert!(
            matches!(ev, AgentEvent::ActivityStart { detail: Some(d), .. }
            if d.display() == "?")
        );
    }

    #[test]
    fn malformed_tool_args_string_degrades_to_no_target() {
        let ev = decode(json!({
            "event": "tool_call_before", "cwd": "/r",
            "tool": "exec_shell", "tool_args": "not json {"
        }));
        assert!(
            matches!(ev, AgentEvent::ActivityStart { detail: Some(d), .. }
            if d.display() == "exec_shell")
        );
    }

    #[test]
    fn long_tool_target_is_truncated_at_the_decode_boundary() {
        let long = "x".repeat(MAX_TOOL_TARGET_CHARS * 3);
        let args = format!("{{\"command\":\"{long}\"}}");
        let ev = decode(json!({
            "event": "tool_call_before", "cwd": "/r",
            "tool": "exec_shell", "tool_args": args
        }));
        match ev {
            AgentEvent::ActivityStart {
                detail: Some(d), ..
            } => {
                let display = d.display();
                assert!(display.starts_with("exec_shell: "));
                assert!(display.ends_with('…'));
            }
            other => panic!("expected ActivityStart, got {other:?}"),
        }
    }
}
