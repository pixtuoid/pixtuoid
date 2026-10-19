//! opencode source — HOOK-ONLY, via a bundled TS plugin.
//!
//! opencode keeps sessions in SQLite with no tailable transcript, so its one
//! external seam is the plugin API: the bundled plugin
//! (`install/opencode_plugin.ts`) reads `ctx.event.subscribe()` and pipes an
//! allowlisted copy of each event into the `pixtuoid-hook` shim:
//!
//! ```json
//! {"type":"session.created","location":{"directory":"/repo"},"data":{"sessionID":"ses_…","location":{"directory":"/repo"},"parentID":"ses_…","model":{"id":"…"}},"_pid":12345}
//! ```
//!
//! The upstream facts the decode rests on, at opencode v2:
//!
//! - `sessionID` is on every session event and is the session's durable key,
//!   so slots key on it.
//! - A subagent is a child SESSION: the `subagent` tool creates it with
//!   `parentID` (`packages/core/src/tool/plugin/subagent.ts:188-189`), so its
//!   `session.created` links it.
//! - An event published in a location carries it on its envelope, beside
//!   `data` (`packages/schema/src/event.ts:114`, `:138`): the recording's tool
//!   and permission events do, its `session.execution.*` events do not.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::AgentId;
use crate::source::decoder::{
    DecodeError, DecodeResult as Result, MAX_DECODED_FIELD_CHARS, ellipsize, generic_tool_display,
};
use crate::source::{AgentEvent, ToolDetail};

/// The opencode CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "opencode";

const SESSION_CREATED: &str = "session.created";
const SESSION_DELETED: &str = "session.deleted";
const EXECUTION_STARTED: &str = "session.execution.started";
const EXECUTION_SUCCEEDED: &str = "session.execution.succeeded";
const EXECUTION_FAILED: &str = "session.execution.failed";
const EXECUTION_INTERRUPTED: &str = "session.execution.interrupted";
const TOOL_STARTED: &str = "session.tool.input.started";
const TOOL_SUCCESS: &str = "session.tool.success";
const TOOL_FAILED: &str = "session.tool.failed";
const PERMISSION_ASKED: &str = "permission.asked";

/// The subagent dispatch tool (`packages/core/src/tool/plugin/subagent.ts:17`
/// at v2.0.32).
const DISPATCH_TOOL: &str = "subagent";

/// The event types this decoder turns into events. Pinned to the arms below
/// by `every_exported_set_is_exactly_its_dispatch_arms`, and to the plugin's
/// `FORWARD` set by `the_plugin_forwards_exactly_what_the_decoder_reads`.
#[cfg(any(test, feature = "harness"))]
#[doc(hidden)]
pub const DECODED_EVENTS: &[&str] = &[
    SESSION_CREATED,
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
/// and nothing more, pinned by
/// `the_plugin_copies_exactly_the_fields_the_decoder_reads`.
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
/// If the payload is not an object or lacks `type`, or a mapped event lacks a
/// non-empty `data.sessionID`.
pub(crate) fn decode_oc_hook_payload(v: &Value) -> Result<Vec<AgentEvent>> {
    let obj = v
        .as_object()
        .ok_or_else(|| DecodeError::not_an_object(SOURCE_NAME))?;
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

    match event {
        // A new prompt re-emits SessionStart, the event that walks a swept
        // session back in, mid-walkout too. Its own `data` is only `sessionID`,
        // so the plugin adds the session's record (`ctx.session.get`).
        SESSION_CREATED | EXECUTION_STARTED => session_start(data, at, event),
        // `data` is only `sessionID` (`packages/schema/src/session-event.ts:178`,
        // `Base`), so a child's end cannot be told from a root's here.
        SESSION_DELETED => Ok(vec![AgentEvent::SessionEnd {
            agent_id: session(data, event)?.0,
            as_child: false,
        }]),
        // A turn's end, as cursor's `stop`: no call to pair, so it ends the
        // session's activity rather than one tool's.
        EXECUTION_SUCCEEDED | EXECUTION_FAILED | EXECUTION_INTERRUPTED => {
            ended(data, at, event, None)
        }
        TOOL_STARTED => tool_started(data, at),
        TOOL_SUCCESS | TOOL_FAILED => ended(data, at, event, field(data, "id").map(str::to_owned)),
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
    tool_use_id: Option<String>,
) -> Result<Vec<AgentEvent>> {
    let (agent_id, session_id) = session(data, event)?;
    Ok(vec![
        identity(agent_id, session_id, at),
        AgentEvent::ActivityEnd {
            agent_id,
            tool_use_id,
        },
    ])
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
        AgentEvent::ActivityStart {
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

/// Waiting, labelled by the request's `action` and keyed on its tool call
/// (`source.id`) when it has one.
fn permission_asked(data: &Map<String, Value>, at: Option<&str>) -> Result<Vec<AgentEvent>> {
    let (agent_id, session_id) = session(data, PERMISSION_ASKED)?;
    let reason = field(data, "action").map_or_else(
        || "permission".to_owned(),
        |a| ellipsize(a, MAX_DECODED_FIELD_CHARS),
    );
    Ok(vec![
        identity(agent_id, session_id, at),
        AgentEvent::Waiting {
            agent_id,
            reason,
            tool_use_id: field(data, "source.id").map(str::to_owned),
        },
    ])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn decode_all(v: Value) -> Vec<AgentEvent> {
        decode_oc_hook_payload(&v).expect("decodes")
    }

    /// The payload's MAIN event: the last decoded, after any Identity.
    fn decode(v: Value) -> AgentEvent {
        decode_all(v).pop().expect("at least one event")
    }

    fn created(data: Value) -> Value {
        json!({"type": SESSION_CREATED, "data": data, "_pid": 13358})
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
                decode_oc_hook_payload(&json!({"type": ev, "data": data}))
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
    fn session_created_keys_on_its_session_id_and_reads_its_location() {
        let evs = decode_all(created(json!({
            "sessionID": "ses_140762860ffe0d", "projectID": "prj_1", "slug": "shiny-canyon",
            "location": {"directory": "/private/tmp/oc-capture/ws"},
            "model": {"id": "claude-sonnet-4-5", "providerID": "anthropic"}, "version": "2.0.27"
        })));
        match &evs[0] {
            AgentEvent::SessionStart {
                agent_id,
                source,
                session_id,
                cwd,
                parent_id,
            } => {
                assert_eq!(source, SOURCE_NAME);
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts(SOURCE_NAME, "ses_140762860ffe0d")
                );
                assert_eq!(session_id, "ses_140762860ffe0d");
                assert_eq!(*cwd, std::path::PathBuf::from("/private/tmp/oc-capture/ws"));
                assert_eq!(*parent_id, None);
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
        assert!(
            matches!(&evs[1], AgentEvent::ModelInfo { model: Some(m), effort: None, .. }
            if m == "claude-sonnet-4-5")
        );
        let bare = decode_all(created(
            json!({"sessionID": "ses_n", "location": {"directory": "/r"}}),
        ));
        assert_eq!(bare.len(), 1, "no model, no ModelInfo: {bare:?}");
    }

    #[test]
    fn a_subagent_session_links_to_its_parent_and_keeps_its_own_id() {
        let parent = decode(created(
            json!({"sessionID": "ses_p", "location": {"directory": "/r"}}),
        ));
        let child = decode_all(created(json!({
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
    fn a_tool_start_is_activity_start_keyed_on_its_id() {
        let evs = decode_all(
            json!({"type": TOOL_STARTED, "location": {"directory": "/repo"},
            "data": {"sessionID": "ses_x", "assistantMessageID": "msg_1", "id": "call_abc", "name": "bash"}}),
        );
        assert!(
            matches!(&evs[0], AgentEvent::Identity { session_id, cwd: Some(cwd), .. }
            if session_id == "ses_x" && cwd.as_path() == std::path::Path::new("/repo"))
        );
        match &evs[1] {
            AgentEvent::ActivityStart {
                agent_id,
                tool_use_id,
                detail,
            } => {
                assert_eq!(*agent_id, AgentId::from_parts(SOURCE_NAME, "ses_x"));
                assert_eq!(tool_use_id.as_deref(), Some("call_abc"));
                assert_eq!(detail.as_ref().map(ToolDetail::display), Some("bash"));
            }
            other => panic!("expected ActivityStart, got {other:?}"),
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
            matches!(ev, AgentEvent::ActivityStart { detail: Some(d), .. } if d.display() == "?")
        );
    }

    #[test]
    fn a_tool_success_or_failure_ends_its_call() {
        for ev in [TOOL_SUCCESS, TOOL_FAILED] {
            let got = decode_all(json!({"type": ev, "location": {"directory": "/repo"},
                "data": {"sessionID": "ses_x", "id": "call_abc"}}));
            assert!(
                matches!(got.as_slice(), [AgentEvent::Identity { session_id, cwd: Some(_), .. },
                    AgentEvent::ActivityEnd { tool_use_id: Some(id), .. }]
                if session_id == "ses_x" && id == "call_abc"),
                "{ev}: {got:?}"
            );
        }
    }

    #[test]
    fn an_execution_start_is_a_session_start_with_its_record_and_its_end_closes_the_turn() {
        let start = decode_all(
            json!({"type": EXECUTION_STARTED, "data": {"sessionID": "ses_c",
            "location": {"directory": "/repo"}, "parentID": "ses_p", "model": {"id": "m-1"}}}),
        );
        assert!(
            matches!(start.as_slice(), [
                AgentEvent::SessionStart { session_id, cwd, parent_id: Some(p), .. },
                AgentEvent::ModelInfo { model: Some(m), .. },
            ] if session_id == "ses_c" && cwd.as_path() == std::path::Path::new("/repo")
                && *p == AgentId::from_parts(SOURCE_NAME, "ses_p") && m == "m-1"),
            "{start:?}"
        );
        let bare = decode_all(json!({"type": EXECUTION_STARTED, "data": {"sessionID": "ses_c"}}));
        assert!(
            matches!(
                bare.as_slice(),
                [
                    AgentEvent::Identity { cwd: None, .. },
                    AgentEvent::SessionStart { .. }
                ]
            ),
            "a start with no record is process-proven: {bare:?}"
        );
        for ev in [EXECUTION_SUCCEEDED, EXECUTION_FAILED, EXECUTION_INTERRUPTED] {
            let got = decode_all(json!({"type": ev, "data": {"sessionID": "ses_x"}}));
            assert!(
                matches!(
                    got.as_slice(),
                    [
                        AgentEvent::Identity { .. },
                        AgentEvent::ActivityEnd {
                            tool_use_id: None,
                            ..
                        }
                    ]
                ),
                "{ev}: {got:?}"
            );
        }
    }

    #[test]
    fn a_permission_ask_is_waiting_on_its_tool_call() {
        let evs = decode_all(json!({"type": PERMISSION_ASKED, "data": {
            "id": "per_1", "sessionID": "ses_x", "action": "bash", "resources": ["rm -rf /tmp/x"],
            "source": {"type": "tool", "messageID": "msg_1", "id": "call_abc"}}}));
        assert!(matches!(&evs[0], AgentEvent::Identity { .. }));
        assert!(
            matches!(&evs[1], AgentEvent::Waiting { reason, tool_use_id: Some(id), .. }
            if reason == "bash" && id == "call_abc")
        );
        let bare = decode(json!({"type": PERMISSION_ASKED, "data": {"sessionID": "ses_x"}}));
        assert!(
            matches!(bare, AgentEvent::Waiting { reason, tool_use_id: None, .. } if reason == "permission")
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
        assert!(decode_oc_hook_payload(&json!("x")).is_err());
        assert!(decode_oc_hook_payload(&json!({"data": {"sessionID": "s"}})).is_err());
        for ev in DECODED_EVENTS {
            assert!(
                decode_oc_hook_payload(&json!({"type": ev, "data": {}})).is_err(),
                "{ev} without a sessionID"
            );
            assert!(
                decode_oc_hook_payload(&json!({"type": ev, "data": {"sessionID": ""}})).is_err(),
                "{ev} with an empty sessionID"
            );
        }
    }

    #[test]
    fn a_long_model_slug_is_truncated_at_the_decode_boundary() {
        let long = "m".repeat(MAX_DECODED_FIELD_CHARS + 10);
        let evs = decode_all(created(
            json!({"sessionID": "s", "location": {"directory": "/r"}, "model": {"id": long}}),
        ));
        assert!(
            matches!(&evs[1], AgentEvent::ModelInfo { model: Some(m), .. }
            if m.chars().count() <= MAX_DECODED_FIELD_CHARS + 1)
        );
    }
}
