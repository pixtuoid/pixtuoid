//! Shared **ACP** (Agent Client Protocol) decode — a published multi-vendor wire
//! STANDARD, so hosting its vocabulary once is the documented exception to
//! architecture invariant #3 (the workspace `AGENTS.md`). What stays PER-SOURCE
//! and must be injected by the caller: the tool-detail vocabulary + any
//! Task-detection, and a source's OWN `_`-prefixed extension namespace (grok's
//! `_x.ai/session/update` is NOT ACP and stays bespoke in grok.rs).
//!
//! Scope is ACP **v1**'s `session/update` notification. **v2**'s tag set is
//! materially different and is a future ADDITIVE arm, not built until a source
//! emits it.

use serde_json::{Map, Value};

use crate::AgentId;
use crate::source::{AgentEvent, ToolDetail};

const TOOL_CALL: &str = "tool_call";
const TOOL_CALL_UPDATE: &str = "tool_call_update";

const STATUS_COMPLETED: &str = "completed";
const STATUS_FAILED: &str = "failed";

/// The notification [`decode_session_update`] decodes; a source gates its
/// call on this method.
pub(crate) const SESSION_UPDATE_METHOD: &str = "session/update";

/// Decode one ACP `session/update` notification's `update` object into activity
/// events; the caller injects `agent_id` and its own `tool_detail` (invariant #3).
///
/// A FRESH `tool_call` OMITS `status` (Pending is the serde skip-default), so
/// absence still starts. Only a TERMINAL `tool_call_update` status
/// (`completed`/`failed`) ends — `in_progress` and status-less content deltas
/// are not completions.
pub(crate) fn decode_session_update(
    agent_id: AgentId,
    update: &Map<String, Value>,
    tool_detail: impl Fn(&str, Option<&Value>) -> ToolDetail,
) -> Vec<AgentEvent> {
    let str_field = |key: &str| update.get(key).and_then(|s| s.as_str());
    let tool_call_id = || str_field("toolCallId").map(String::from);

    match str_field("sessionUpdate").unwrap_or("") {
        TOOL_CALL => vec![AgentEvent::ActivityStart {
            agent_id,
            tool_use_id: tool_call_id(),
            detail: Some(tool_detail(
                str_field("title").unwrap_or("?"),
                update.get("rawInput"),
            )),
        }],
        TOOL_CALL_UPDATE => match str_field("status") {
            Some(STATUS_COMPLETED) | Some(STATUS_FAILED) => vec![AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id: tool_call_id(),
            }],
            _ => vec![],
        },
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SRC: &str = "grok";
    fn agent() -> AgentId {
        AgentId::from_parts(SRC, "sess")
    }
    fn detail(title: &str, _raw: Option<&Value>) -> ToolDetail {
        ToolDetail::Generic {
            display: title.to_string(),
        }
    }
    fn decode(update: Value) -> Vec<AgentEvent> {
        decode_session_update(agent(), update.as_object().unwrap(), detail)
    }

    #[test]
    fn tool_call_starts_and_terminal_update_ends_keyed_by_tool_call_id() {
        match decode(json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "title": "grep"}))
            .as_slice()
        {
            [
                AgentEvent::ActivityStart {
                    tool_use_id: Some(id),
                    detail: Some(ToolDetail::Generic { display }),
                    ..
                },
            ] => {
                assert_eq!(id, "c1");
                assert_eq!(display, "grep");
            }
            other => panic!("expected one ActivityStart, got {other:?}"),
        }
        for status in [STATUS_COMPLETED, STATUS_FAILED] {
            match decode(
                json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": status}),
            )
            .as_slice()
            {
                [
                    AgentEvent::ActivityEnd {
                        tool_use_id: Some(id),
                        ..
                    },
                ] => assert_eq!(id, "c1"),
                other => panic!("expected one ActivityEnd for {status}, got {other:?}"),
            }
        }
        for status in ["in_progress", "pending"] {
            assert!(
                decode(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": status})).is_empty(),
                "{status} must not end the activity"
            );
        }
        assert!(
            decode(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1"})).is_empty()
        );
    }

    /// Each decoded tag reaches a real arm and a non-member decodes to nothing.
    #[test]
    fn each_decoded_tag_reaches_an_arm() {
        for tag in [TOOL_CALL, TOOL_CALL_UPDATE] {
            let update = json!({
                "sessionUpdate": tag,
                "toolCallId": "c1",
                "title": "grep",
                "status": "completed",
            });
            assert!(!decode(update).is_empty(), "{tag} must reach a real arm");
        }
        assert!(decode(json!({"sessionUpdate": "tool_call_pending"})).is_empty());
    }

    /// Blank, per-token and brand-new tags alike decode to nothing IN SILENCE:
    /// a tag we never read cannot cost us an event, and breadcrumbing one meant
    /// hand-maintaining the whole v1 vocabulary to hold back a per-token flood.
    #[test]
    fn no_session_update_tag_breadcrumbs_however_new() {
        for update in [
            json!({}),
            json!({"sessionUpdate": ""}),
            json!({"sessionUpdate": "agent_message_chunk"}),
            json!({"sessionUpdate": "usage_update"}),
            json!({"sessionUpdate": "teleport_update"}),
        ] {
            let logs = crate::test_capture::capture_logs(|| {
                assert!(
                    decode(update.clone()).is_empty(),
                    "{update} decodes to no events"
                );
            });
            assert!(
                !logs.contains(crate::source::drift::TARGET),
                "{update} reached the drift log:\n{logs}"
            );
        }
    }
}
