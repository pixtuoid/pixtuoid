//! Hermes Agent (Nous Research) source — HOOK-ONLY, via a bundled Python plugin.
//!
//! Hermes runs every turn in its gateway daemon, whose own cwd is its install
//! dir, so no shell hook can name a session's workspace. The bundled plugin
//! (`install/hermes_plugin.py`) runs in that gateway, reads the workspace from
//! `ctx.session_cwd()`, and pipes an allowlisted copy of each event's kwargs into
//! the `pixtuoid-hook` shim:
//!
//! ```json
//! {"hook_event_name":"pre_tool_call","cwd":"/repo","_pid":43677,"session_id":"local-…","tool_name":"read_file","tool_call_id":"call_…","args":{"path":"NOTE.txt"}}
//! ```
//!
//! Keyed on `session_id`, not the workspace: a user may run several Hermes
//! sessions in ONE project and cwd-keying would merge them (the Cursor lesson).
//! The envelope reuses CC's `hook_event_name` field NAME with snake_case VALUES
//! alien to the shared CC-shaped arms, so the decoder is
//! `HookDecoding::ClaimsAll`: it claims EVERY event.

use std::path::PathBuf;

use serde_json::Value;

use crate::AgentId;
use crate::source::AgentEvent;
use crate::source::decoder::{
    DecodeError, DecodeResult as Result, MAX_DECODED_FIELD_CHARS, ellipsize, generic_keyed_detail,
};
use crate::source::registry::HookObject;

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

const ON_SESSION_START: &str = "on_session_start";
const PRE_TOOL_CALL: &str = "pre_tool_call";
const PRE_APPROVAL_REQUEST: &str = "pre_approval_request";
const POST_TOOL_CALL: &str = "post_tool_call";
const ON_SESSION_END: &str = "on_session_end";
const ON_SESSION_FINALIZE: &str = "on_session_finalize";

/// The events this decoder turns into events. Pinned to its arms by
/// `the_decoded_event_set_is_exactly_what_the_arms_match`, and to the plugin's
/// `FORWARD` by `the_plugin_forwards_exactly_what_the_decoder_reads`.
#[cfg(any(test, feature = "harness"))]
#[doc(hidden)]
pub const DECODED_EVENTS: &[&str] = &[
    ON_SESSION_START,
    PRE_TOOL_CALL,
    PRE_APPROVAL_REQUEST,
    POST_TOOL_CALL,
    ON_SESSION_END,
    ON_SESSION_FINALIZE,
];

/// The events hermes fires outside a session's turn, where `ctx.session_cwd()`
/// is undefined (`website/docs/user-guide/features/plugins.md:1109`): the gateway
/// finalizes a session at its viewer's close or its own stop. The plugin sends
/// them no cwd, and no arm reads one: `only_an_event_inside_a_turn_reads_the_cwd`.
#[cfg(any(test, feature = "harness"))]
#[doc(hidden)]
pub const OUTSIDE_A_TURN: &[&str] = &[ON_SESSION_FINALIZE];

/// Every envelope field [`field`] reads, pinned by `every_decoded_field_is_read`.
/// The plugin sends these and `args` cut to [`DETAIL_KEYS`], nothing more:
/// `the_plugin_copies_exactly_the_fields_the_decoder_reads`.
#[cfg(any(test, feature = "harness"))]
#[doc(hidden)]
pub const DECODED_FIELDS: &[&str] = &[
    "session_id",
    "cwd",
    "tool_name",
    "tool_call_id",
    "description",
    "model",
];

/// The `args` keys a tool's detail is read from.
const KEYS: &[&str] = &["command", "file_path", "path", "pattern", "url"];

/// [`KEYS`], for the plugin's pin.
#[cfg(any(test, feature = "harness"))]
#[doc(hidden)]
pub const DETAIL_KEYS: &[&str] = KEYS;

/// Decode one Hermes plugin envelope (already identified by
/// `_pixtuoid_source == "hermes"`); an event outside the plugin's forward set
/// bails, so plugin-vs-decoder drift is loud.
///
/// The activity arms prepend an [`AgentEvent::Identity`] because Hermes is HOOK-ONLY:
/// a slot the reducer synthesizes mid-turn has no transcript back-fill path, so
/// without the attached identity it would stay a blank `#N` ghost.
///
/// # Errors
///
/// If the payload lacks `hook_event_name` or `session_id`, or names an event
/// the plugin does not forward.
pub(crate) fn decode_hermes_hook_payload(obj: &HookObject) -> Result<Vec<AgentEvent>> {
    let event = obj
        .get("hook_event_name")
        .and_then(Value::as_str)
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "hook_event_name"))?;
    let session_id = field(obj, "session_id")
        .ok_or_else(|| DecodeError::missing_in(SOURCE_NAME, event, "session_id"))?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, session_id);
    let cwd = field(obj, "cwd");
    let identity =
        || AgentEvent::identity(agent_id, SOURCE_NAME, session_id, cwd.map(PathBuf::from));
    // A call's start, its approval wait and its end each carry it
    // (`_CallIds` at `model_tools.py:643`, `tools/approval_context.py:67` at v0.22.5),
    // so a wait names the call whose end clears it.
    let tool_use_id = || field(obj, "tool_call_id").map(String::from);

    let mut evs = match event {
        ON_SESSION_START => vec![AgentEvent::SessionStart {
            agent_id,
            source: SOURCE_NAME.to_string(),
            session_id: session_id.to_string(),
            cwd: cwd.unwrap_or_default().into(),
            parent_id: None,
        }],
        PRE_TOOL_CALL => {
            let tool = field(obj, "tool_name").unwrap_or_else(|| {
                crate::source::drift::missing_field(SOURCE_NAME, event, "tool_name");
                "?"
            });
            vec![
                identity(),
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id: tool_use_id(),
                    detail: Some(generic_keyed_detail(tool, obj.get("args"), KEYS)),
                },
            ]
        }
        POST_TOOL_CALL => vec![
            identity(),
            AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id: tool_use_id(),
            },
        ],
        // The gate's WHY rides `description` (`website/docs/user-guide/features/hooks.md:484`).
        PRE_APPROVAL_REQUEST => {
            let reason = field(obj, "description")
                .map(|d| ellipsize(d, MAX_DECODED_FIELD_CHARS))
                .unwrap_or_else(|| "permission".to_string());
            vec![
                identity(),
                AgentEvent::UserActionRequest {
                    agent_id,
                    reason: Some(reason),
                    tool_use_id: tool_use_id(),
                },
            ]
        }
        // THE session end, as `on_session_end` is not: `finalize_session` closes the
        // Relay conversation under the same `session_id` (`hermes_cli/lifecycle.py:81-93`).
        // The gateway fires it when a session's last viewer closes it
        // (`gateway/session_viewer_close.py:1-7`) or it stops mid-turn
        // (`gateway/run_shutdown.py:1173-1180`); a one-shot session idle at the stop
        // gets none, and ends with the gateway's pid.
        ON_SESSION_FINALIZE => vec![AgentEvent::SessionEnd {
            agent_id,
            as_child: false,
        }],
        // A TURN boundary, not the session's end: it fires "at each turn
        // finalization" (`website/docs/user-guide/features/hooks.md:471`). Mapping it
        // to SessionEnd walked the agent out of the office at every turn.
        //
        // An ACTIVITY arm, so it takes that class's Identity with it: for a
        // tool-less chat turn this is the ONLY event the session emits, and a
        // daemon attaching mid-session would otherwise get the blank `#N` ghost
        // the decode fn's doc names, until the next tool call.
        ON_SESSION_END => vec![identity(), AgentEvent::turn_end(agent_id)],
        other => {
            crate::source::drift::unknown_event(SOURCE_NAME, other);
            return Err(DecodeError::unsupported(SOURCE_NAME, other));
        }
    };
    // Only the session events carry it (`website/docs/user-guide/features/hooks.md:470-471`),
    // so an event without one must not clear it.
    if let Some(model) = field(obj, "model") {
        evs.push(AgentEvent::ModelInfo {
            agent_id,
            model: Some(ellipsize(model, MAX_DECODED_FIELD_CHARS)),
            effort: None,
        });
    }
    Ok(evs)
}

/// The non-empty string at top-level `name`.
fn field<'o>(obj: &'o HookObject, name: &'static str) -> Option<&'o str> {
    // The plugin sends only the listed fields, so a read missing from the list
    // would see nothing in production while an inline test payload passes.
    #[cfg(test)]
    assert!(
        DECODED_FIELDS.contains(&name),
        "{name} is not in DECODED_FIELDS"
    );
    obj.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
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

    /// No key is guessed: a workspace-keyed slot would merge two sessions, and
    /// the cwd is not the session's identity.
    #[test]
    fn a_payload_without_a_session_id_is_an_error_never_keyed_on_its_cwd() {
        for ev in DECODED_EVENTS {
            for payload in [
                json!({"hook_event_name": ev, "cwd": "/Users/dev/proj"}),
                json!({"hook_event_name": ev, "session_id": "", "cwd": "/Users/dev/proj"}),
            ] {
                assert!(
                    decode_hermes_hook_payload(test_obj(&payload)).is_err(),
                    "{payload}"
                );
            }
        }
    }

    #[test]
    fn the_session_cwd_names_the_workspace_and_an_empty_one_names_none() {
        let start = decode(
            json!({"hook_event_name": "on_session_start", "session_id": "s", "cwd": "/repo"}),
        );
        assert!(
            matches!(&start, AgentEvent::SessionStart { cwd, .. } if cwd == &PathBuf::from("/repo")),
            "{start:?}"
        );
        let with = decode_all(
            json!({"hook_event_name": "post_tool_call", "session_id": "s",
            "cwd": "/repo"}),
        );
        let without = decode_all(
            json!({"hook_event_name": "post_tool_call", "session_id": "s",
            "cwd": ""}),
        );
        assert!(
            matches!(&with[0], AgentEvent::Identity { cwd: Some(c), .. } if c == &PathBuf::from("/repo")),
            "{with:?}"
        );
        assert!(
            matches!(&without[0], AgentEvent::Identity { cwd: None, .. }),
            "{without:?}"
        );
    }

    #[test]
    fn a_calls_start_wait_and_end_name_it_and_its_start_names_the_tool() {
        let id = |ev: &str| {
            decode(
                json!({"hook_event_name": ev, "session_id": "s", "cwd": "/repo",
                "tool_name": "read_file", "tool_call_id": "call_1",
                "args": {"path": "NOTE.txt"}, "description": "recursive delete"}),
            )
        };
        let start = id("pre_tool_call");
        let AgentEvent::ToolUseStart {
            tool_use_id,
            detail: Some(detail),
            ..
        } = &start
        else {
            panic!("{start:?}");
        };
        assert_eq!(tool_use_id.as_deref(), Some("call_1"));
        assert!(detail.display().contains("NOTE.txt"), "{detail:?}");
        assert!(
            matches!(id("pre_approval_request"), AgentEvent::UserActionRequest {
            tool_use_id: Some(t), reason: Some(r), .. } if t == "call_1" && r == "recursive delete")
        );
        assert!(matches!(id("post_tool_call"), AgentEvent::ToolUseEnd {
            tool_use_id: Some(t), .. } if t == "call_1"));
    }

    /// The name says session; upstream's emitter and payload say TURN. Mapping it
    /// to `SessionEnd` swept the slot at every turn boundary — hermes was invisible
    /// while the user read or typed.
    #[test]
    fn on_session_end_is_a_turn_boundary_not_the_sessions_end() {
        let evs = decode_all(
            json!({"hook_event_name": "on_session_end", "session_id": "s",
            "cwd": "/r", "model": "hermetic"}),
        );
        assert!(
            evs.iter().any(|e| matches!(e, AgentEvent::TurnEnd { .. })),
            "{evs:?}"
        );
        assert!(
            evs.iter().any(
                |e| matches!(e, AgentEvent::ModelInfo { model: Some(m), .. } if m == "hermetic")
            ),
            "{evs:?}"
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
    fn the_decoded_event_set_is_exactly_what_the_arms_match() {
        for ev in DECODED_EVENTS {
            let got = decode_all(json!({"hook_event_name": ev, "session_id": "s"}));
            assert!(!got.is_empty(), "{ev} must reach a real arm");
        }
        for ev in ["subagent_stop", "pre_message", "on_session_reset", "Bogus"] {
            assert!(
                decode_hermes_hook_payload(test_obj(
                    &json!({"hook_event_name": ev, "session_id": "s", "cwd": "/r"})
                ))
                .is_err(),
                "{ev} must bail (not forwarded, must not decode silently)"
            );
        }
    }

    /// [`field`]'s assertion holds every read to the list; this holds every
    /// listed field to a read, by blanking it.
    #[test]
    fn every_decoded_field_is_read() {
        let full = json!({"session_id": "s", "cwd": "/r", "tool_name": "terminal",
            "tool_call_id": "c", "description": "why", "model": "m"});
        let decoded = |ev: &str, payload: &Value| {
            let mut payload = payload.clone();
            payload["hook_event_name"] = json!(ev);
            format!("{:?}", decode_hermes_hook_payload(test_obj(&payload)))
        };
        for name in DECODED_FIELDS {
            let mut blanked = full.clone();
            blanked[*name] = json!("");
            assert!(
                DECODED_EVENTS
                    .iter()
                    .any(|ev| decoded(ev, &full) != decoded(ev, &blanked)),
                "{name} is in DECODED_FIELDS, but no event reads it"
            );
        }
    }

    /// A cwd outside a turn is not the session's, so it registers nothing.
    #[test]
    fn only_an_event_inside_a_turn_reads_the_cwd() {
        for ev in DECODED_EVENTS {
            let decoded = |payload: Value| format!("{:?}", decode_all(payload));
            let with = decoded(json!({"hook_event_name": ev, "session_id": "s", "cwd": "/repo"}));
            let without = decoded(json!({"hook_event_name": ev, "session_id": "s"}));
            assert_eq!(with == without, OUTSIDE_A_TURN.contains(ev), "{ev}");
        }
        let finalize = decode_all(json!({"hook_event_name": "on_session_finalize",
            "session_id": "s", "cwd": "/repo"}));
        assert!(
            matches!(finalize[..], [AgentEvent::SessionEnd { .. }]),
            "{finalize:?}"
        );
    }

    #[test]
    fn malformed_payloads_are_errors() {
        assert!(
            decode_hermes_hook_payload(test_obj(&json!({"session_id": "s", "cwd": "/r"}))).is_err()
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
}
