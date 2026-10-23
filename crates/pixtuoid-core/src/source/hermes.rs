//! Hermes Agent (Nous Research) source — HOOK-ONLY: pixtuoid never spawns the user's
//! agent and Hermes's on-disk sessions are not tailable JSONL, so the shell hooks in
//! the `config.yaml` under [`hermes_home`] are the only seam a passive observer can
//! reach. That home is `$HERMES_HOME` when set, else `~/.hermes` — but
//! `%LOCALAPPDATA%\hermes` on Windows, which is why every caller resolves it through
//! that fn rather than joining `.hermes` itself.
//!
//! Keyed on `session_id`, not the workspace: a user may run several Hermes sessions
//! in ONE project and cwd-keying would merge them (the Cursor lesson).
//!
//! The envelope reuses CC's `hook_event_name` field NAME but with snake_case VALUES
//! alien to the shared CC-shaped arms, so the decoder is `HookDecoding::ClaimsAll`:
//! it claims EVERY event.
//!
//! `subagent_stop` is deliberately absent from `HERMES_EVENTS`: the SHELL-hook payload
//! carries a parent id but NO child session/agent id, so a decode could only end a
//! child that was never started. Hermes's Python PLUGIN API does define a
//! `subagent_start` with a child id, but plugin hooks are in-process callbacks that
//! never reach a shell command's stdin — don't "fix" this by modelling them.

use std::path::PathBuf;

use crate::source::decoder::{DecodeError, DecodeResult as Result};
use serde_json::Value;

use crate::AgentId;
use crate::source::decoder::{MAX_DECODED_FIELD_CHARS, ellipsize};
use crate::source::registry::HookObject;
use crate::source::{AgentEvent, ToolDetail};

/// The Hermes CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "hermes";

/// The Hermes home dir (`config.yaml` lives directly in it), mirroring hermes's
/// own `hermes_constants._hermes_home_from_env` (2026.8.3): `HERMES_HOME`
/// STRIPPED and taken verbatim when what's left is non-empty — even for a dir
/// that does not exist, unlike Codex's exists-check — else the PLATFORM-NATIVE
/// default, which is `%LOCALAPPDATA%\hermes` on Windows and only elsewhere
/// `<user_home>/.hermes`.
///
/// Hermes PROFILES are applied out-of-band and are deliberately not mirrored:
/// each CLI's resolver is mirrored on its OWN axes, never a generic union.
pub fn hermes_home() -> Option<PathBuf> {
    let raw = crate::platform::path_env_trimmed("HERMES_HOME");
    let from_env = raw.is_some();
    let dir = resolve_hermes_home(
        raw,
        crate::platform::path_env_trimmed("LOCALAPPDATA"),
        cfg!(windows),
        crate::platform::user_home_opt(),
    )?;
    // Only the ENV branch may be blamed on HERMES_HOME. A relative DEFAULT comes
    // from HOME/LOCALAPPDATA, and naming an unset var as the cause sends the
    // reader chasing the wrong one (#881 review).
    Some(if from_env {
        crate::platform::warn_if_relative_override("HERMES_HOME", dir)
    } else {
        dir
    })
}

/// Pure precedence core, separated so the Windows arm is unit-testable on any
/// host. `user_home` mirrors `Path.home()`, whose Windows arm is
/// `USERPROFILE`-first (`ntpath.expanduser`) exactly like [`user_home_opt`].
///
/// [`user_home_opt`]: crate::platform::user_home_opt
/// The env values arrive ALREADY trimmed and blank-filtered from
/// `platform::path_env_trimmed` — normalized at the read boundary, not here.
fn resolve_hermes_home(
    hermes_home_env: Option<PathBuf>,
    local_appdata: Option<PathBuf>,
    windows: bool,
    user_home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(h) = hermes_home_env {
        return Some(h);
    }
    if !windows {
        return user_home.map(|h| h.join(".hermes"));
    }
    let base = match local_appdata {
        Some(l) => l,
        None => user_home?.join("AppData").join("Local"),
    };
    Some(base.join("hermes"))
}

/// Decode one Hermes hook payload (already identified by
/// `_pixtuoid_source == "hermes"`); an unregistered event bails so
/// registered-vs-decoded drift is loud.
///
/// The activity arms prepend an [`AgentEvent::Identity`] because Hermes is HOOK-ONLY:
/// a slot the reducer synthesizes mid-turn has no transcript back-fill path, so
/// without the attached identity it would stay a blank `#N` ghost.
///
/// # Errors
///
/// If the payload lacks `hook_event_name`, carries neither `session_id` nor `cwd`, or names an unregistered event.
pub(crate) fn decode_hermes_hook_payload(obj: &HookObject) -> Result<Vec<AgentEvent>> {
    let event = obj
        .get("hook_event_name")
        .and_then(|s| s.as_str())
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "hook_event_name"))?;
    let cwd = obj
        .get("cwd")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty());
    // The cwd fallback is only for a future event that omits `session_id` — it keeps
    // coalescing best-effort instead of dropping the event.
    let key = obj
        .get("session_id")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .or(cwd)
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "session_id|cwd"))?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, key);
    let cwd = cwd.unwrap_or("");

    let identity = || {
        AgentEvent::identity(
            agent_id,
            SOURCE_NAME,
            key,
            (!cwd.is_empty()).then(|| cwd.into()),
        )
    };
    // A call's start, its approval wait and its end each carry it
    // (`_CallIds` at `model_tools.py:643`, `tools/approval_context.py:67` at v0.22.2),
    // so a wait names the call whose end clears it.
    let tool_use_id = || {
        obj.get("extra")
            .and_then(|e| e.get("tool_call_id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };

    let decoded: Result<Vec<AgentEvent>> = match event {
        "on_session_start" => Ok(vec![AgentEvent::SessionStart {
            agent_id,
            source: SOURCE_NAME.to_string(),
            session_id: key.to_string(),
            cwd: cwd.into(),
            parent_id: None,
        }]),
        "pre_tool_call" => {
            let tool = obj
                .get("tool_name")
                .and_then(|s| s.as_str())
                .unwrap_or_else(|| {
                    crate::source::drift::missing_field(SOURCE_NAME, "pre_tool_call", "tool_name");
                    "?"
                });
            Ok(vec![
                identity(),
                AgentEvent::ActivityStart {
                    agent_id,
                    tool_use_id: tool_use_id(),
                    detail: Some(hermes_tool_detail(tool, obj.get("tool_input"))),
                },
            ])
        }
        "post_tool_call" => Ok(vec![
            identity(),
            AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id: tool_use_id(),
            },
        ]),
        // Observer-only for a SHELL hook: `_BLOCKING_EVENTS` in upstream's
        // `agent/shell_hooks.py` is `{pre_tool_call}` alone, so our silent exit-0
        // cannot stall the decision. `tool_name` is null here and the WHY rides
        // `extra.description` — capture-verified, `permission-hermetic`.
        "pre_approval_request" => {
            let reason = obj
                .get("extra")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .filter(|s| !s.is_empty())
                .map(|d| crate::source::decoder::ellipsize(d, MAX_DECODED_FIELD_CHARS))
                .unwrap_or_else(|| "permission".to_string());
            Ok(vec![
                identity(),
                AgentEvent::Waiting {
                    agent_id,
                    reason,
                    tool_use_id: tool_use_id(),
                },
            ])
        }
        // THE session end, as `on_session_end` is not: `lifecycle.py::finalize_session`
        // hard-closes the Relay conversation under the SAME `session_id` it sends
        // us, so the id in the payload is the one being terminated. Upstream's
        // atexit-registered `_run_cleanup` fires it with `reason="shutdown"`, which
        // is why the registry row claims an exit signal. Its sibling
        // `on_session_reset` is deliberately NOT here — see the omit ledger.
        "on_session_finalize" => Ok(vec![AgentEvent::SessionEnd {
            agent_id,
            as_child: false,
        }]),
        // A TURN boundary, not the session's end — upstream's own doc says it is
        // "emitted from agent/turn_finalizer.py" carrying `turn_id`/`completed`
        // ("True when the turn produced a final response"), and turn_finalizer's
        // comment adds that "run_conversation() is called once per user message
        // in multi-turn sessions". Mapping it to SessionEnd walked the agent out of
        // the office at every turn.
        //
        // An ACTIVITY arm, so it takes that class's Identity with it: for a
        // tool-less chat turn this is the ONLY event the session emits, and a
        // daemon attaching mid-session would otherwise get the blank `#N` ghost
        // the module doc names, until the next tool call.
        "on_session_end" => Ok(vec![
            identity(),
            AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id: None,
            },
        ]),
        other => {
            crate::source::drift::unknown_event(SOURCE_NAME, other);
            Err(DecodeError::unsupported(SOURCE_NAME, other))
        }
    };
    let mut evs = decoded?;
    // hermes nests it one level down and only the session events carry it, so a
    // top-level read finds nothing and a tool event must not clear it.
    if let Some(model) = obj
        .get("extra")
        .and_then(|e| e.get("model"))
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

fn hermes_tool_detail(tool: &str, args: Option<&Value>) -> ToolDetail {
    const KEYS: &[&str] = &["command", "file_path", "path", "pattern", "url"];
    crate::source::decoder::generic_keyed_detail(tool, args, KEYS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;
    use serde_json::json;

    fn decode_all(v: Value) -> Vec<AgentEvent> {
        decode_hermes_hook_payload(test_obj(&v)).expect("decodes")
    }

    fn decode(v: Value) -> AgentEvent {
        decode_all(v).pop().expect("at least one event")
    }

    #[test]
    fn session_start_reports_the_model_from_extra() {
        // The shape from fixtures/hermes/tool-run-hermetic: hermes nests it one
        // level down, and only the session events carry it.
        let evs = decode_all(json!({
            "hook_event_name": "on_session_start", "session_id": "s1", "cwd": "/repo",
            "extra": {"model": "gpt-4o-mini"}
        }));
        assert!(
            evs.iter().any(
                |e| matches!(e, AgentEvent::ModelInfo { model: Some(m), .. } if m == "gpt-4o-mini")
            ),
            "extra.model must reach ModelInfo, got {evs:?}"
        );
    }

    #[test]
    fn session_start_keys_on_session_id_with_real_cwd() {
        let ev = decode(json!({
            "hook_event_name": "on_session_start",
            "tool_name": null, "tool_input": null,
            "session_id": "sess-1", "cwd": "/Users/dev/proj", "extra": {}
        }));
        match ev {
            AgentEvent::SessionStart {
                agent_id,
                source,
                session_id,
                cwd,
                parent_id,
            } => {
                assert_eq!(source, SOURCE_NAME);
                assert_eq!(agent_id, AgentId::from_parts(SOURCE_NAME, "sess-1"));
                assert_eq!(session_id, "sess-1", "key on session_id, not cwd");
                assert_eq!(cwd, std::path::PathBuf::from("/Users/dev/proj"));
                assert_eq!(parent_id, None);
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn session_id_distinguishes_two_sessions_in_one_workspace() {
        let a = decode(
            json!({"hook_event_name": "on_session_start", "session_id": "A", "cwd": "/repo"}),
        );
        let b = decode(
            json!({"hook_event_name": "on_session_start", "session_id": "B", "cwd": "/repo"}),
        );
        assert_ne!(
            a.agent_id(),
            b.agent_id(),
            "two sessions in one repo must be distinct"
        );
    }

    #[test]
    fn key_falls_back_to_cwd_when_session_id_absent() {
        let ev = decode(json!({
            "hook_event_name": "on_session_start", "cwd": "/Users/dev/proj"
        }));
        assert!(matches!(ev, AgentEvent::SessionStart { agent_id, .. }
            if agent_id == AgentId::from_parts(SOURCE_NAME, "/Users/dev/proj")));
    }

    #[test]
    fn a_call_its_wait_and_its_end_name_one_tool_id() {
        let call = |event: &str| {
            decode(json!({
                "hook_event_name": event,
                "session_id": "s", "cwd": "/repo",
                "tool_name": "terminal", "tool_input": {"command": "echo hello"},
                "extra": {"task_id": "t", "tool_call_id": "c", "description": "why"}
            }))
        };
        match call("pre_tool_call") {
            AgentEvent::ActivityStart {
                tool_use_id,
                detail,
                ..
            } => {
                assert_eq!(tool_use_id.as_deref(), Some("c"));
                assert_eq!(detail.unwrap().display(), "terminal: echo hello");
            }
            other => panic!("expected ActivityStart, got {other:?}"),
        }
        assert!(matches!(
            call("pre_approval_request"),
            AgentEvent::Waiting { tool_use_id: Some(t), .. } if t == "c"
        ));
        assert!(matches!(
            call("post_tool_call"),
            AgentEvent::ActivityEnd { tool_use_id: Some(t), .. } if t == "c"
        ));
    }

    #[test]
    fn post_tool_call_is_activity_end() {
        let ev = decode(json!({
            "hook_event_name": "post_tool_call", "session_id": "s", "cwd": "/repo",
            "tool_name": "terminal",
            "extra": {"result": "{\"output\":\"hi\"}", "duration_ms": 42}
        }));
        assert!(matches!(
            &ev,
            AgentEvent::ActivityEnd {
                tool_use_id: None,
                ..
            }
        ));
    }

    /// The name says session; upstream's emitter and payload say TURN. Mapping it
    /// to `SessionEnd` swept the slot at every turn boundary — hermes was invisible
    /// while the user read or typed.
    #[test]
    fn on_session_end_is_a_turn_boundary_not_the_sessions_end() {
        let ev =
            decode(json!({"hook_event_name": "on_session_end", "session_id": "s", "cwd": "/r"}));
        assert!(
            matches!(
                ev,
                AgentEvent::ActivityEnd {
                    tool_use_id: None,
                    ..
                }
            ),
            "got {ev:?}"
        );
    }

    /// The shape no capture shows: a recorded run ends its only turn and its
    /// session together.
    #[test]
    fn a_second_turn_in_one_session_does_not_end_the_session() {
        let sid = "sess-multi";
        let mut evs = Vec::new();
        for name in [
            "on_session_start",
            "pre_tool_call",
            "post_tool_call",
            "on_session_end",
            "pre_tool_call",
            "post_tool_call",
            "on_session_end",
        ] {
            evs.extend(
                decode_hermes_hook_payload(test_obj(
                    &json!({"hook_event_name": name, "session_id": sid, "cwd": "/r"}),
                ))
                .expect("decodes"),
            );
        }
        let ids: std::collections::BTreeSet<_> = evs.iter().map(AgentEvent::agent_id).collect();
        assert_eq!(ids.len(), 1, "one session must stay one AgentId");
        assert_eq!(
            evs.iter()
                .filter(|e| matches!(e, AgentEvent::SessionEnd { .. }))
                .count(),
            0,
            "a turn boundary must not end the session: {evs:?}"
        );
    }

    #[test]
    fn all_events_for_one_session_share_one_agent_id() {
        let sid = "sess-1";
        let events = [
            json!({"hook_event_name": "on_session_start", "session_id": sid, "cwd": "/repo"}),
            json!({"hook_event_name": "pre_tool_call", "session_id": sid, "cwd": "/repo",
                   "tool_name": "terminal", "tool_input": {"command": "ls"}}),
            json!({"hook_event_name": "post_tool_call", "session_id": sid, "cwd": "/repo", "tool_name": "terminal"}),
            json!({"hook_event_name": "on_session_end", "session_id": sid, "cwd": "/repo"}),
        ];
        let ids: std::collections::BTreeSet<_> = events
            .iter()
            .flat_map(|v| decode_hermes_hook_payload(test_obj(v)).unwrap())
            .map(|e| e.agent_id())
            .collect();
        assert_eq!(ids.len(), 1, "all events must coalesce to one AgentId");
    }

    #[test]
    fn activity_arms_prepend_identity_keyed_on_session_id() {
        for payload in [
            json!({"hook_event_name": "pre_tool_call", "session_id": "s", "cwd": "/repo",
                   "tool_name": "terminal", "tool_input": {"command": "ls"}}),
            json!({"hook_event_name": "post_tool_call", "session_id": "s", "cwd": "/repo", "tool_name": "terminal"}),
            json!({"hook_event_name": "on_session_end", "session_id": "s", "cwd": "/repo"}),
        ] {
            let name = payload["hook_event_name"].clone();
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
                    assert_eq!(*agent_id, AgentId::from_parts(SOURCE_NAME, "s"));
                    assert_eq!(source, SOURCE_NAME);
                    assert_eq!(session_id, "s");
                    assert_eq!(cwd.as_deref(), Some(std::path::Path::new("/repo")));
                }
                other => panic!("{name}: expected leading Identity, got {other:?}"),
            }
        }
    }

    #[test]
    /// The partition is by decoded CLASS, not by wire name — `on_session_end`
    /// moved to the activity arms and its Identity had to move with it.
    fn session_events_carry_no_identity() {
        for payload in [
            json!({"hook_event_name": "on_session_start", "session_id": "s", "cwd": "/r"}),
            json!({"hook_event_name": "on_session_finalize", "session_id": "s", "cwd": "/r"}),
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
    fn unknown_event_bails_loudly() {
        for ev in ["subagent_stop", "pre_message", "on_error", "Bogus"] {
            assert!(
                decode_hermes_hook_payload(test_obj(&json!({"hook_event_name": ev, "cwd": "/r"})))
                    .is_err(),
                "{ev} must bail (not registered, must not decode silently)"
            );
        }
    }

    #[test]
    fn malformed_payloads_are_errors() {
        assert!(
            decode_hermes_hook_payload(test_obj(&json!({"hook_event_name": "on_session_end"})))
                .is_err()
        );
    }

    /// An UNSET `HERMES_HOME` must never be blamed for a relative default: the
    /// default is derived from HOME/LOCALAPPDATA, so naming the wrong variable
    /// sends the reader chasing a var they never set (#881 review).
    #[test]
    fn an_unset_hermes_home_is_never_named_as_the_cause_of_a_relative_default() {
        // A relative HOME makes the DERIVED default relative, with no override set.
        let relative_home = [
            ("HOME", Some("relative-home")),
            ("USERPROFILE", Some("relative-home")),
        ];
        temp_env::with_vars(relative_home, || {
            let quiet = temp_env::with_var_unset("HERMES_HOME", || {
                crate::test_capture::capture_logs(|| {
                    let _ = hermes_home();
                })
            });
            assert!(
                !quiet.contains("HERMES_HOME"),
                "an unset HERMES_HOME must not be named as the cause:\n{quiet}"
            );

            // Positive control: the ENV branch still warns, or the gate is inert.
            let loud = temp_env::with_var("HERMES_HOME", Some("rel/hm"), || {
                crate::test_capture::capture_logs(|| {
                    let _ = hermes_home();
                })
            });
            assert!(
                loud.contains("HERMES_HOME"),
                "a RELATIVE HERMES_HOME must still warn:\n{loud}"
            );
        });
    }

    /// Pinned against a live probe of hermes 2026.8.3, the STRIP included.
    /// `hermes_home` must read through the TRIMMING twin: upstream applies
    /// `os.environ.get(K, "").strip()`, so a padded override IS the stripped
    /// path. Nothing else pins that binding — swap `path_env_trimmed` for
    /// `path_env` in `hermes_home()` and every other test still passes.
    #[test]
    fn hermes_home_strips_a_padded_env_override_like_upstream() {
        let got = temp_env::with_var("HERMES_HOME", Some("  /custom/hm  "), hermes_home);

        assert_eq!(got, Some(PathBuf::from("/custom/hm")));
    }

    #[test]
    fn hermes_home_takes_the_env_value_then_falls_back_to_dot_hermes() {
        let unix = |env: Option<&str>| {
            resolve_hermes_home(
                env.map(PathBuf::from),
                None,
                false,
                Some(PathBuf::from("/home/u")),
            )
        };
        let default = Some(PathBuf::from("/home/u").join(".hermes"));

        assert_eq!(unix(Some("/custom/hm")), Some(PathBuf::from("/custom/hm")));
        assert_eq!(unix(None), default);
        assert_eq!(resolve_hermes_home(None, None, false, None), None);
    }

    /// `%LOCALAPPDATA%\hermes`, NOT `<home>/.hermes` — the generic answer wrote
    /// hooks into a config.yaml hermes never reads (#880).
    #[test]
    fn hermes_home_on_windows_is_local_appdata_not_dot_hermes() {
        let win = |env: Option<&str>, appdata: Option<&str>| {
            resolve_hermes_home(
                env.map(PathBuf::from),
                appdata.map(PathBuf::from),
                true,
                Some(PathBuf::from(r"C:\Users\ada")),
            )
        };

        assert_eq!(
            win(None, Some(r"C:\Users\ada\AppData\Local")),
            Some(PathBuf::from(r"C:\Users\ada\AppData\Local").join("hermes"))
        );
        let relative_default = Some(
            PathBuf::from(r"C:\Users\ada")
                .join("AppData")
                .join("Local")
                .join("hermes"),
        );
        assert_eq!(win(None, None), relative_default);
        assert_eq!(
            win(
                Some(r"D:\profiles\work"),
                Some(r"C:\Users\ada\AppData\Local")
            ),
            Some(PathBuf::from(r"D:\profiles\work")),
            "HERMES_HOME still outranks the platform default"
        );
        assert_eq!(resolve_hermes_home(None, None, true, None), None);
    }

    /// The arms MUST disagree — the assertion that would have caught the bug.
    #[test]
    fn the_windows_and_unix_defaults_diverge() {
        let home = Some(PathBuf::from(r"C:\Users\ada"));
        assert_ne!(
            resolve_hermes_home(None, None, true, home.clone()),
            resolve_hermes_home(None, None, false, home),
        );
    }
}
