//! opencode source — HOOK-ONLY, via a bundled TS plugin.
//!
//! opencode keeps sessions in SQLite with no tailable transcript, so its one
//! external seam is the plugin API: the bundled plugin
//! (`install/opencode_plugin.ts`) reads `ctx.event.subscribe()` and pipes an
//! allowlisted copy of each event into the `pixtuoid-hook` shim:
//!
//! ```json
//! {"type":"session.execution.started","data":{"sessionID":"ses_…","location":{"directory":"/repo"},"parentID":"ses_…","model":{"id":"…"}},"_pid":12345}
//! ```
//!
//! The upstream facts the decode rests on, at opencode v2:
//!
//! - `sessionID` is on every session event and is the session's durable key,
//!   so slots key on it.
//! - A subagent is a child SESSION: the `subagent` tool creates it with
//!   `parentID` (`packages/core/src/tool/plugin/subagent.ts:188-189`), so the
//!   record its first start carries links it.
//! - A session registers at its first turn's start, the one signal for it: the
//!   plugin's subscription may not be live yet when opencode publishes the
//!   session's creation (#1934).
//! - An event published in a location carries it on its envelope, beside
//!   `data` (`packages/schema/src/event.ts:114`, `:138`): the recording's tool
//!   and permission events do, its `session.execution.*` events do not.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::AgentId;
use crate::source::decoder::{
    DecodeError, DecodeResult as Result, MAX_DECODED_FIELD_CHARS, ellipsize, generic_tool_display,
};
use crate::source::registry::{
    ChildEndInference, FocusChannel, HookDecoding, HookObject, Hyphen, SourceCaps,
    SourceDescriptor, SourceKind,
};
use crate::source::{AgentEvent, RequestKeys, ToolDetail};
use crate::state::TurnKey;

/// The opencode CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "opencode";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    label_prefix: "oc",
    verified_version: "2.2.0",
    version_probe: Some(&["opencode", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll(decode_oc_hook_payload)),
        caps: SourceCaps {
            // A TUI quit ends nothing: the session lives on in the server.
            has_exit_signal: false,
            // The next prompt's `session.execution.started` decodes to
            // `SessionStart`.
            resurrects_on_prompt: true,
            // A `subagent` child is parented by its first start's `parentID`, so
            // its events keep the parent fresh (`scope::refresh_lineage`).
            delegations_are_hook_silent: false,
            // A `subagent` call can return while its child runs on
            // (`packages/core/src/tool/plugin/subagent.ts:229-243` at v2.0.32),
            // and a session ends only by `session.deleted`.
            child_end_inference: ChildEndInference::Durable,
            child_hooks_unnamed: false,
            brackets_turns: true,
        },
        // The plugin runs in the background server every TUI attaches to
        // (`packages/client/src/service-contender.ts:19-20`), not in a TUI.
        focus: FocusChannel::ServerStamp,
    },
};

const SESSION_DELETED: &str = "session.deleted";
const EXECUTION_STARTED: &str = "session.execution.started";
const EXECUTION_SUCCEEDED: &str = "session.execution.succeeded";
const EXECUTION_FAILED: &str = "session.execution.failed";
const EXECUTION_INTERRUPTED: &str = "session.execution.interrupted";
const TOOL_STARTED: &str = "session.tool.input.started";
const TOOL_SUCCESS: &str = "session.tool.success";
const TOOL_FAILED: &str = "session.tool.failed";
const PERMISSION_ASKED: &str = "permission.asked";

/// The envelope's turn: an execution start's durable `seq`, which the plugin
/// also stamps on that execution's settle; one whose start it never saw names
/// none. Pinned to the plugin's key by its selftest, through
/// `the_plugin_contract_is_the_committed_file`.
#[doc(hidden)]
pub const TURN: &str = "turn";

/// The subagent dispatch tool (`packages/core/src/tool/plugin/subagent.ts:17`
/// at v2.0.32).
const DISPATCH_TOOL: &str = "subagent";

/// The event types this decoder turns into events. Pinned to the arms below
/// by `every_exported_set_is_exactly_its_dispatch_arms`, and to the plugin's
/// forwarding by its selftest, through `the_plugin_contract_is_the_committed_file`.
#[cfg(any(test, feature = "harness"))]
#[doc(hidden)]
pub const DECODED_EVENTS: &[&str] = &[
    SESSION_DELETED,
    EXECUTION_STARTED,
    EXECUTION_SUCCEEDED,
    EXECUTION_FAILED,
    EXECUTION_INTERRUPTED,
    TOOL_STARTED,
    TOOL_SUCCESS,
    TOOL_FAILED,
    PERMISSION_ASKED,
];

/// Every `data` path `field` reads, dotted, pinned by
/// `every_decoded_data_field_is_read`. The plugin's `allowlisted` copies these
/// and nothing more, which its selftest holds it to through
/// `the_plugin_contract_is_the_committed_file`.
#[cfg(any(test, feature = "harness"))]
#[doc(hidden)]
pub const DECODED_DATA_FIELDS: &[&str] = &[
    "sessionID",
    "parentID",
    "location.directory",
    "model.id",
    "id",
    "name",
    "action",
    "source.id",
];

/// Decode one opencode plugin envelope (already identified by
/// `_pixtuoid_source == "opencode"`). An unmapped `type` is a benign skip: the
/// forward filter lives in the plugin.
///
/// # Errors
///
/// If the payload lacks `type`, or a mapped event lacks a
/// non-empty `data.sessionID`.
pub(crate) fn decode_oc_hook_payload(obj: &HookObject) -> Result<Vec<AgentEvent>> {
    let event = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| DecodeError::missing(SOURCE_NAME, "type"))?;
    let empty = Map::new();
    let data = obj.get("data").and_then(Value::as_object).unwrap_or(&empty);
    let at = obj
        .get("location")
        .and_then(|l| l.get("directory"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());

    let turn = obj.get(TURN).and_then(Value::as_u64).map(TurnKey::Seq);
    match event {
        // A new prompt re-emits SessionStart, the event that walks a swept
        // session back in, mid-walkout too. Its own `data` is only `sessionID`,
        // so the plugin adds the session's record (`ctx.session.get`).
        EXECUTION_STARTED => {
            let mut out = session_start(data, at, event)?;
            match turn {
                Some(turn) => out.push(AgentEvent::TurnStart {
                    agent_id: session(data, event)?.0,
                    turn,
                }),
                None => crate::source::drift::missing_field(SOURCE_NAME, event, TURN),
            }
            Ok(out)
        }
        // `data` is only `sessionID` (`packages/schema/src/session-event.ts:178`,
        // `Base`), so a child's end cannot be told from a root's here.
        SESSION_DELETED => Ok(vec![AgentEvent::SessionEnd {
            agent_id: session(data, event)?.0,
            as_child: false,
        }]),
        EXECUTION_SUCCEEDED | EXECUTION_FAILED | EXECUTION_INTERRUPTED => {
            ended(data, at, event, |agent_id| match turn {
                Some(turn) => AgentEvent::keyed_turn_end(agent_id, turn),
                None => AgentEvent::turn_end(agent_id),
            })
        }
        TOOL_STARTED => tool_started(data, at),
        TOOL_SUCCESS | TOOL_FAILED => {
            let tool_use_id = field(data, "id").map(str::to_owned);
            ended(data, at, event, |agent_id| AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id,
            })
        }
        PERMISSION_ASKED => permission_asked(data, at),
        _ => Ok(vec![]),
    }
}

/// The session an event concerns: its agent id and its `sessionID`.
fn session<'d>(data: &'d Map<String, Value>, event: &str) -> Result<(AgentId, &'d str)> {
    let session_id = field(data, "sessionID")
        .ok_or_else(|| DecodeError::missing_in(SOURCE_NAME, event, "sessionID"))?;
    Ok((AgentId::from_parts(SOURCE_NAME, session_id), session_id))
}

/// The non-empty string at dotted `path` in `data`.
fn field<'d>(data: &'d Map<String, Value>, path: &str) -> Option<&'d str> {
    // The plugin strips every field outside the list, so a read missing from it
    // would see nothing in production while an inline test payload passes.
    #[cfg(test)]
    assert!(
        DECODED_DATA_FIELDS.contains(&path),
        "{path} is not in DECODED_DATA_FIELDS"
    );
    let mut keys = path.split('.');
    let mut at = data.get(keys.next()?)?;
    for key in keys {
        at = at.get(key)?;
    }
    at.as_str().filter(|s| !s.is_empty())
}

fn identity(agent_id: AgentId, session_id: &str, at: Option<&str>) -> AgentEvent {
    AgentEvent::identity(agent_id, SOURCE_NAME, session_id, at.map(PathBuf::from))
}

/// `location.directory` is the cwd, `parentID` the parent session, and
/// `model.id` the raw model slug.
fn session_start(
    data: &Map<String, Value>,
    at: Option<&str>,
    event: &str,
) -> Result<Vec<AgentEvent>> {
    let (agent_id, session_id) = session(data, event)?;
    let cwd = field(data, "location.directory").or(at);
    let mut out = Vec::new();
    // A start the plugin could not read a record for has no cwd, and a
    // cwd-less SessionStart registers on the short unknown-cwd window; an
    // Identity marks the slot a live process's instead (`apply_identity`).
    if cwd.is_none() {
        out.push(identity(agent_id, session_id, None));
    }
    out.push(AgentEvent::SessionStart {
        agent_id,
        source: SOURCE_NAME.to_string(),
        session_id: session_id.to_string(),
        cwd: cwd.unwrap_or_default().into(),
        parent_id: field(data, "parentID").map(|p| AgentId::from_parts(SOURCE_NAME, p)),
    });
    if let Some(model) = field(data, "model.id") {
        out.push(AgentEvent::ModelInfo {
            agent_id,
            model: Some(ellipsize(model, MAX_DECODED_FIELD_CHARS)),
            effort: None,
        });
    }
    Ok(out)
}

/// An end, led by Identity (#221) as a tool start and a permission ask are,
/// so one for a slot the reaper removed registers it with its identity, not
/// as a blank `#N`.
fn ended(
    data: &Map<String, Value>,
    at: Option<&str>,
    event: &str,
    end: impl FnOnce(AgentId) -> AgentEvent,
) -> Result<Vec<AgentEvent>> {
    let (agent_id, session_id) = session(data, event)?;
    Ok(vec![identity(agent_id, session_id, at), end(agent_id)])
}

/// The tool's `id` is the call id its `success`/`failed` pair on. The input
/// rides a later event, so the detail is the tool's name alone, or a Task.
fn tool_started(data: &Map<String, Value>, at: Option<&str>) -> Result<Vec<AgentEvent>> {
    let (agent_id, session_id) = session(data, TOOL_STARTED)?;
    let name = field(data, "name").unwrap_or_else(|| {
        crate::source::drift::missing_field(SOURCE_NAME, TOOL_STARTED, "name");
        "?"
    });
    Ok(vec![
        identity(agent_id, session_id, at),
        AgentEvent::ToolUseStart {
            agent_id,
            tool_use_id: field(data, "id").map(str::to_owned),
            detail: Some(if name == DISPATCH_TOOL {
                ToolDetail::Task
            } else {
                generic_tool_display(name, None)
            }),
        },
    ])
}

/// [`AgentEvent::UserActionRequest`], labelled by the request's `action` and keyed on its tool call
/// (`source.id`) when it has one.
fn permission_asked(data: &Map<String, Value>, at: Option<&str>) -> Result<Vec<AgentEvent>> {
    let (agent_id, session_id) = session(data, PERMISSION_ASKED)?;
    let reason = field(data, "action").map_or_else(
        || "permission".to_owned(),
        |a| ellipsize(a, MAX_DECODED_FIELD_CHARS),
    );
    Ok(vec![
        identity(agent_id, session_id, at),
        AgentEvent::user_action_request(
            agent_id,
            Some(reason),
            RequestKeys {
                tool_use_id: field(data, "source.id").map(str::to_owned),
                ..RequestKeys::default()
            },
        ),
    ])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::source::decoder::test_obj;

    fn decode_all(v: Value) -> Vec<AgentEvent> {
        decode_oc_hook_payload(test_obj(&v)).expect("decodes")
    }

    /// The payload's MAIN event: the last decoded, after any Identity.
    fn decode(v: Value) -> AgentEvent {
        decode_all(v).pop().expect("at least one event")
    }

    fn started(data: Value) -> Value {
        json!({"type": EXECUTION_STARTED, "data": data, "_pid": 13358})
    }

    /// [`field`]'s assertion holds every read to the list; this holds every
    /// listed path to a read, by blanking it.
    #[test]
    fn every_decoded_data_field_is_read() {
        let full = json!({"sessionID": "s", "parentID": "p", "location": {"directory": "/r"},
            "model": {"id": "m"}, "id": "c", "name": "bash", "action": "shell",
            "source": {"id": "c"}});
        let decoded = |ev: &str, data: &Value| {
            format!(
                "{:?}",
                decode_oc_hook_payload(test_obj(&json!({"type": ev, "data": data})))
            )
        };
        for path in DECODED_DATA_FIELDS {
            let mut blanked = full.clone();
            *blanked
                .pointer_mut(&format!("/{}", path.replace('.', "/")))
                .expect("the full payload holds every listed path") = json!("");
            assert!(
                DECODED_EVENTS
                    .iter()
                    .any(|ev| decoded(ev, &full) != decoded(ev, &blanked)),
                "{path} is in DECODED_DATA_FIELDS, but no event reads it"
            );
        }
    }

    /// Every exported type reaches an arm and decodes to an event, and an
    /// unmapped one is a benign skip.
    #[test]
    fn the_decoded_event_set_is_exactly_what_the_arms_match() {
        for ev in DECODED_EVENTS {
            let got = decode_all(json!({"type": ev, "data": {
                "sessionID": "ses_x", "id": "c", "name": "bash",
                "location": {"directory": "/repo"}}}));
            assert!(!got.is_empty(), "{ev} must reach a real arm");
        }
        assert!(
            decode_all(json!({"type": "session.viewed", "data": {"sessionID": "ses_x"}}))
                .is_empty()
        );
    }

    #[test]
    fn a_subagent_session_links_to_its_parent_and_keeps_its_own_id() {
        let parent = decode(started(
            json!({"sessionID": "ses_p", "location": {"directory": "/r"}}),
        ));
        let child = decode_all(started(json!({
            "sessionID": "ses_c", "location": {"directory": "/r"}, "parentID": "ses_p"})));
        match &child[0] {
            AgentEvent::SessionStart {
                agent_id,
                parent_id,
                ..
            } => {
                assert_eq!(*agent_id, AgentId::from_parts(SOURCE_NAME, "ses_c"));
                assert_eq!(*parent_id, Some(parent.agent_id()));
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn a_tool_start_is_tool_use_start_keyed_on_its_id() {
        let evs = decode_all(
            json!({"type": TOOL_STARTED, "location": {"directory": "/repo"},
            "data": {"sessionID": "ses_x", "assistantMessageID": "msg_1", "id": "call_abc", "name": "bash"}}),
        );
        assert!(
            matches!(&evs[0], AgentEvent::Identity { session_id, cwd: Some(cwd), .. }
            if session_id == "ses_x" && cwd.as_path() == std::path::Path::new("/repo"))
        );
        match &evs[1] {
            AgentEvent::ToolUseStart {
                agent_id,
                tool_use_id,
                detail,
            } => {
                assert_eq!(*agent_id, AgentId::from_parts(SOURCE_NAME, "ses_x"));
                assert_eq!(tool_use_id.as_deref(), Some("call_abc"));
                assert_eq!(detail.as_ref().map(ToolDetail::display), Some("bash"));
            }
            other => panic!("expected ToolUseStart, got {other:?}"),
        }
    }

    /// `session.forked`'s `parentID` is the session it was copied from, beside
    /// a message `boundary` (`packages/schema/src/session-event.ts:187-200`),
    /// not a parent.
    #[test]
    fn a_fork_is_no_child() {
        assert!(
            decode_all(json!({"type": "session.forked", "data": {
                "sessionID": "ses_f", "parentID": "ses_p", "boundary": {}}}))
            .is_empty()
        );
    }

    #[test]
    fn a_tool_without_a_name_degrades_to_a_question_mark() {
        let ev = decode(json!({"type": TOOL_STARTED, "data": {"sessionID": "ses_x", "id": "c"}}));
        assert!(
            matches!(ev, AgentEvent::ToolUseStart { detail: Some(d), .. } if d.display() == "?")
        );
    }

    #[test]
    fn a_tool_success_or_failure_ends_its_call() {
        for ev in [TOOL_SUCCESS, TOOL_FAILED] {
            let got = decode_all(json!({"type": ev, "location": {"directory": "/repo"},
                "data": {"sessionID": "ses_x", "id": "call_abc"}}));
            assert!(
                matches!(got.as_slice(), [AgentEvent::Identity { session_id, cwd: Some(_), .. },
                    AgentEvent::ToolUseEnd { tool_use_id: Some(id), .. }]
                if session_id == "ses_x" && id == "call_abc"),
                "{ev}: {got:?}"
            );
        }
    }

    #[test]
    fn an_execution_start_is_a_session_start_with_its_record_opening_its_turn() {
        let start = decode_all(
            json!({"type": EXECUTION_STARTED, "turn": 7, "data": {"sessionID": "ses_c",
            "location": {"directory": "/repo"}, "parentID": "ses_p", "model": {"id": "m-1"}}}),
        );
        assert!(
            matches!(start.as_slice(), [
                AgentEvent::SessionStart { session_id, cwd, parent_id: Some(p), .. },
                AgentEvent::ModelInfo { model: Some(m), .. },
                AgentEvent::TurnStart { turn: TurnKey::Seq(7), .. },
            ] if session_id == "ses_c" && cwd.as_path() == std::path::Path::new("/repo")
                && *p == AgentId::from_parts(SOURCE_NAME, "ses_p") && m == "m-1"),
            "{start:?}"
        );
        let bare = decode_all(
            json!({"type": EXECUTION_STARTED, "turn": 7, "data": {"sessionID": "ses_c"}}),
        );
        assert!(
            matches!(
                bare.as_slice(),
                [
                    AgentEvent::Identity { cwd: None, .. },
                    AgentEvent::SessionStart { .. },
                    AgentEvent::TurnStart { .. },
                ]
            ),
            "a start with no record is process-proven: {bare:?}"
        );
    }

    /// The plugin stamps every start with its durable `seq`, so one without it
    /// is drift, and opens no turn.
    #[test]
    fn an_execution_start_naming_no_turn_opens_none_and_breadcrumbs() {
        let logs = crate::test_capture::capture_logs(|| {
            let got = decode_all(json!({"type": EXECUTION_STARTED,
                "data": {"sessionID": "ses_c", "location": {"directory": "/repo"}}}));
            assert!(
                matches!(got.as_slice(), [AgentEvent::SessionStart { .. }]),
                "{got:?}"
            );
        });
        assert!(
            logs.contains("missing_field") && logs.contains(TURN),
            "{logs}"
        );
    }

    /// A settle names its start's turn; one whose start this plugin never saw
    /// names none, which ends whatever is open.
    #[test]
    fn a_settle_ends_the_turn_its_start_opened() {
        for ev in [EXECUTION_SUCCEEDED, EXECUTION_FAILED, EXECUTION_INTERRUPTED] {
            let got = decode_all(json!({"type": ev, "turn": 7, "data": {"sessionID": "ses_x"}}));
            assert!(
                matches!(
                    got.as_slice(),
                    [
                        AgentEvent::Identity { .. },
                        AgentEvent::TurnEnd {
                            turn: Some(TurnKey::Seq(7)),
                            ..
                        }
                    ]
                ),
                "{ev}: {got:?}"
            );
            let logs = crate::test_capture::capture_logs(|| {
                let got = decode_all(json!({"type": ev, "data": {"sessionID": "ses_x"}}));
                assert!(
                    matches!(
                        got.as_slice(),
                        [
                            AgentEvent::Identity { .. },
                            AgentEvent::TurnEnd { turn: None, .. }
                        ]
                    ),
                    "{ev}: {got:?}"
                );
            });
            assert!(!logs.contains("missing_field"), "{ev}: {logs}");
        }
    }

    #[test]
    fn a_permission_ask_is_a_user_action_request_on_its_tool_call() {
        let evs = decode_all(json!({"type": PERMISSION_ASKED, "data": {
            "id": "per_1", "sessionID": "ses_x", "action": "bash", "resources": ["rm -rf /tmp/x"],
            "source": {"type": "tool", "messageID": "msg_1", "id": "call_abc"}}}));
        assert!(matches!(&evs[0], AgentEvent::Identity { .. }));
        assert!(
            matches!(&evs[1], AgentEvent::UserActionRequest { reason, tool_use_id: Some(id), .. }
            if reason.as_deref() == Some("bash") && id == "call_abc")
        );
        let bare = decode(json!({"type": PERMISSION_ASKED, "data": {"sessionID": "ses_x"}}));
        assert!(
            matches!(bare, AgentEvent::UserActionRequest { reason, tool_use_id: None, .. } if reason.as_deref() == Some("permission"))
        );
    }

    #[test]
    fn a_deleted_session_ends() {
        let ev = decode(json!({"type": SESSION_DELETED, "data": {"sessionID": "ses_x"}}));
        assert!(
            matches!(ev, AgentEvent::SessionEnd { agent_id, as_child: false }
            if agent_id == AgentId::from_parts(SOURCE_NAME, "ses_x"))
        );
    }

    #[test]
    fn every_event_of_one_session_shares_its_agent_id() {
        let want = AgentId::from_parts(SOURCE_NAME, "ses_x");
        for ev in DECODED_EVENTS {
            for got in decode_all(json!({"type": ev, "data": {"sessionID": "ses_x", "id": "c"}})) {
                assert_eq!(got.agent_id(), want, "{ev}");
            }
        }
    }

    #[test]
    fn malformed_payloads_are_errors_not_panics() {
        assert!(decode_oc_hook_payload(test_obj(&json!({"data": {"sessionID": "s"}}))).is_err());
        for ev in DECODED_EVENTS {
            assert!(
                decode_oc_hook_payload(test_obj(&json!({"type": ev, "data": {}}))).is_err(),
                "{ev} without a sessionID"
            );
            assert!(
                decode_oc_hook_payload(test_obj(&json!({"type": ev, "data": {"sessionID": ""}})))
                    .is_err(),
                "{ev} with an empty sessionID"
            );
        }
    }

    #[test]
    fn a_long_model_slug_is_truncated_at_the_decode_boundary() {
        let long = "m".repeat(MAX_DECODED_FIELD_CHARS + 10);
        let evs = decode_all(started(
            json!({"sessionID": "s", "location": {"directory": "/r"}, "model": {"id": long}}),
        ));
        assert!(
            matches!(&evs[1], AgentEvent::ModelInfo { model: Some(m), .. }
            if m.chars().count() <= MAX_DECODED_FIELD_CHARS + 1)
        );
    }
}
