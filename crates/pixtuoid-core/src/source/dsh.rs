//! DeepSeek Harness (`dsh`) source. Hook-only: the transcript is not
//! line-readable (the `DSH` registry row owns that story), so the only plane
//! is a pixtuoid-owned cordis plugin (`pixtuoid/src/install/dsh_plugin.mjs`)
//! mounted through the home-level `$DSH_HOME/cordis.patch.yml`, forwarding
//! emit-only events through the shim. A REMOTE subagent provider
//! (`dsh-subagent-claude-code`/`-codex`/`-acp`) publishes no local child
//! session, so such a delegation paints nothing here — at most the spawned
//! CLI's own source shows a parentless sprite. Wire shape: upstream `packages/core/session/src/types.ts` (the
//! `session/event` feed), `packages/interaction/user-approval/src/types.ts`
//! (the log-only approval audit pair), `packages/core/agent/src/runtime-types.ts`
//! (#928). One dsh process hosts MANY sessions (the `web` profile is a
//! server), so `sessionId` is the identity and `_pid` only drives liveness.

use crate::AgentId;
use crate::source::decoder::{MAX_DECODED_FIELD_CHARS, ellipsize, non_empty};
use crate::source::registry::HookObject;
use crate::source::{AgentEvent, RequestKeys, ToolDetail};

/// Stable source id; MUST equal the registry row's `name`.
pub const SOURCE_NAME: &str = "dsh";

/// The payload types the plugin sends — one per subscribed upstream event.
/// The plugin owns the upstream-name → wire-name mapping; the decoder owns
/// nothing upstream-shaped, so an upstream rename lands in ONE file.
const SESSION_START: &str = "session_start";
const SESSION_END: &str = "session_end";
const TOOL_CALL: &str = "tool_call";
const TOOL_RESULT: &str = "tool_result";
const APPROVAL_ASKED: &str = "approval_asked";
const APPROVAL_DECIDED: &str = "approval_decided";
const MODEL: &str = "model";
const USAGE: &str = "usage";

/// Upstream's fail-closed grant is exactly `allowed-once`
/// (`ApprovalOutcome`); every other outcome leaves the call unrun.
const OUTCOME_ALLOWED: &str = "allowed-once";

/// dsh's subagent dispatch tools. The first is capture-verified
/// (`delegation/fixtures/dsh`: the parent streams a `tool_call` naming it,
/// the child arrives as its own `parentSession`-linked session, and the
/// parent's `tool_result` lands at launch, before the child's first request). The
/// second is preset-verified: the tool NAME is
/// per-instance config (`dsh-tool-subagent` `toolName`, default `subagent`),
/// and the standard preset ships BOTH instances enabled
/// (`packages/preset/agent-presets/presets/standard/agent.cordis.yml`,
/// fetched 2026-09-01) while `child-agent.ts` stamps the same
/// `parentSession`/`origin` header pair for every provider. The two
/// `disabled: true` remote instances (`subagent_codex`,
/// `subagent_claude_code`) publish no local child and stay out. Name-keyed
/// like the delegation suite's other members, and structurally so: the
/// plugin's privacy
/// allowlist forwards only `callId`/`toolName` for a tool call, so no
/// argument — spoofed or real — ever reaches this decoder.
const SUBAGENT_TOOLS: &[&str] = &["subagent", "subagent_fork"];

/// Decode one plugin payload into `AgentEvent`s (`HookDecoding::ClaimsAll`).
///
/// Every arm except `session_end` leads or trails an
/// `AgentEvent::identity`: activity and telemetry arms lead (the
/// hook-family pattern — the event may target a slot registered before this
/// daemon attached, and a chat-only `web` session may never run a tool),
/// `session_start` trails (the focus-pid carrier, armed from birth);
/// `session_end` emits none — never re-register a closing session. The
/// approval pair maps onto the reducer's gated-wait mechanics keyed by
/// `callId`: `asked` opens the wait naming the call it gates,
/// `allowed-once` resumes it, anything else ends it.
///
/// # Errors
///
/// Never: the `Result` is the `HookDecoding::ClaimsAll` decoder signature, and a malformed payload decodes to `vec![]`.
#[doc(hidden)]
pub fn decode_dsh_payload(
    obj: &HookObject,
) -> crate::source::decoder::DecodeResult<Vec<AgentEvent>> {
    let Some(ty) = obj.get("type").and_then(|t| t.as_str()) else {
        crate::source::drift::missing_field(SOURCE_NAME, "plugin", "type");
        return Ok(vec![]);
    };
    let Some(session_id) = non_empty(obj, "sessionId") else {
        crate::source::drift::missing_field(SOURCE_NAME, ty, "sessionId");
        return Ok(vec![]);
    };
    let agent_id = AgentId::from_parts(SOURCE_NAME, session_id);
    let parent_id = non_empty(obj, "parentSession").map(|p| AgentId::from_parts(SOURCE_NAME, p));
    let cwd = || {
        obj.get("cwd")
            .and_then(|c| c.as_str())
            .map(std::path::PathBuf::from)
    };
    let identity = || AgentEvent::identity(agent_id, SOURCE_NAME, session_id.to_string(), cwd());
    let call_id = || non_empty(obj, "callId").map(str::to_string);
    let tool_display = || {
        non_empty(obj, "toolName")
            .map(|t| ellipsize(t, MAX_DECODED_FIELD_CHARS))
            .unwrap_or_default()
    };
    let tool_detail = || {
        if non_empty(obj, "toolName").is_some_and(|t| SUBAGENT_TOOLS.contains(&t)) {
            ToolDetail::Task
        } else {
            ToolDetail::Generic {
                display: tool_display(),
            }
        }
    };

    match ty {
        SESSION_START => Ok(vec![
            AgentEvent::SessionStart {
                agent_id,
                source: SOURCE_NAME.to_string(),
                session_id: session_id.to_string(),
                cwd: cwd().unwrap_or_default(),
                parent_id,
            },
            identity(),
        ]),
        SESSION_END => Ok(vec![AgentEvent::SessionEnd {
            agent_id,
            as_child: parent_id.is_some(),
        }]),
        TOOL_CALL => {
            if non_empty(obj, "callId").is_none() {
                crate::source::drift::missing_field(SOURCE_NAME, TOOL_CALL, "callId");
            }
            if non_empty(obj, "toolName").is_none() {
                crate::source::drift::missing_field(SOURCE_NAME, TOOL_CALL, "toolName");
            }
            Ok(vec![
                identity(),
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id: call_id(),
                    detail: Some(tool_detail()),
                },
            ])
        }
        TOOL_RESULT => {
            if non_empty(obj, "callId").is_none() {
                crate::source::drift::missing_field(SOURCE_NAME, TOOL_RESULT, "callId");
            }
            Ok(vec![
                identity(),
                AgentEvent::ToolUseEnd {
                    agent_id,
                    tool_use_id: call_id(),
                },
            ])
        }
        APPROVAL_ASKED => {
            let reason = match (non_empty(obj, "toolName"), non_empty(obj, "reason")) {
                (Some(t), Some(r)) => format!("{t}: {r}"),
                (Some(t), None) => t.to_string(),
                (None, Some(r)) => r.to_string(),
                (None, None) => "approval".to_string(),
            };
            Ok(vec![
                identity(),
                AgentEvent::user_action_request(
                    agent_id,
                    Some(ellipsize(&reason, MAX_DECODED_FIELD_CHARS)),
                    RequestKeys {
                        tool_use_id: call_id(),
                        ..RequestKeys::default()
                    },
                ),
            ])
        }
        APPROVAL_DECIDED => {
            // A renamed/dropped `outcome` would fail CLOSED (every grant
            // decodes as the deny arm's End) — breadcrumb it, because no
            // event-name drift row can see a field rename.
            let outcome = non_empty(obj, "outcome");
            if outcome.is_none() {
                crate::source::drift::missing_field(SOURCE_NAME, APPROVAL_DECIDED, "outcome");
            }
            let event = if outcome == Some(OUTCOME_ALLOWED) {
                // The resume: the gated call now runs; its `tool_call` was
                // already streamed before the gate, so only this Start lifts
                // the wait. GENERIC even for a dispatch: the reducer's Task
                // pre-pass keyed this tuid at the original `tool_call`
                // (Delegating already entered), and a second Task Start for
                // a known tuid is treated as a replay — `handled_by_task_start`
                // skips the RequiresAction→Running resume arm and the slot would
                // strand in RequiresAction for the whole delegation.
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id: call_id(),
                    detail: Some(ToolDetail::Generic {
                        display: tool_display(),
                    }),
                }
            } else {
                // rejected | cancelled | unavailable: the call never runs.
                AgentEvent::ToolUseEnd {
                    agent_id,
                    tool_use_id: call_id(),
                }
            };
            Ok(vec![identity(), event])
        }
        MODEL => Ok(vec![
            identity(),
            AgentEvent::ModelInfo {
                agent_id,
                model: non_empty(obj, "model").map(|m| ellipsize(m, MAX_DECODED_FIELD_CHARS)),
                effort: non_empty(obj, "reasoningEffort")
                    .map(|e| ellipsize(e, MAX_DECODED_FIELD_CHARS)),
            },
        ]),
        USAGE => {
            let n = |k: &str| obj.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            // Fresh spend only: input + output + cache WRITES; cache reads are
            // re-served context (the CC `fresh_spend` semantics).
            Ok(vec![
                identity(),
                AgentEvent::Usage {
                    agent_id,
                    fresh_tokens: n("inputTokens")
                        .saturating_add(n("outputTokens"))
                        .saturating_add(n("cacheWriteTokens")),
                },
            ])
        }
        _ => {
            crate::source::drift::unknown_event(SOURCE_NAME, ty);
            Ok(vec![])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;
    use serde_json::{Value, json};

    const SID: &str = "01b00000-0000-7000-8000-000000000001";

    fn decode(v: Value) -> Vec<AgentEvent> {
        decode_dsh_payload(test_obj(&v)).unwrap()
    }

    fn id() -> AgentId {
        AgentId::from_parts("dsh", SID)
    }

    #[test]
    fn session_start_registers_and_trails_the_pid_carrier() {
        let evs = decode(json!({
            "type": "session_start", "sessionId": SID, "cwd": "/repo",
        }));
        match &evs[..] {
            [
                AgentEvent::SessionStart {
                    agent_id,
                    source,
                    session_id,
                    cwd,
                    parent_id,
                },
                AgentEvent::Identity { pid: None, .. },
            ] => {
                assert_eq!(*agent_id, id());
                assert_eq!(source, "dsh");
                assert_eq!(session_id, SID);
                assert_eq!(cwd, &std::path::PathBuf::from("/repo"));
                assert_eq!(*parent_id, None);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_subagent_session_start_links_its_parent() {
        let parent = "01b00000-0000-7000-8000-0000000000aa";
        let evs = decode(json!({
            "type": "session_start", "sessionId": SID, "cwd": "/repo",
            "parentSession": parent,
        }));
        assert!(
            matches!(&evs[..], [AgentEvent::SessionStart { parent_id: Some(p), .. }, AgentEvent::Identity { .. }]
                if *p == AgentId::from_parts("dsh", parent)),
            "a forwarded parentSession is the delegation link (the plugin \
             forwards it only for `origin: subagent` headers): {evs:?}"
        );
    }

    #[test]
    fn session_end_flags_childness_from_the_carried_parent() {
        let evs = decode(json!({
            "type": "session_end", "sessionId": SID,
            "parentSession": "01b00000-0000-7000-8000-0000000000aa",
        }));
        assert!(matches!(
            &evs[..],
            [AgentEvent::SessionEnd { as_child: true, .. }]
        ));
        let evs = decode(json!({"type": "session_end", "sessionId": SID}));
        assert!(matches!(
            &evs[..],
            [AgentEvent::SessionEnd {
                as_child: false,
                ..
            }]
        ));
    }

    #[test]
    fn a_tool_round_maps_call_and_result_onto_the_real_call_id() {
        let evs = decode(json!({
            "type": "tool_call", "sessionId": SID,
            "callId": "call_1", "toolName": "bash",
        }));
        match &evs[..] {
            [
                AgentEvent::Identity { .. },
                AgentEvent::ToolUseStart {
                    tool_use_id: Some(t),
                    detail: Some(ToolDetail::Generic { display }),
                    ..
                },
            ] => {
                assert_eq!(t, "call_1");
                assert_eq!(display, "bash");
            }
            other => panic!("unexpected: {other:?}"),
        }
        let evs = decode(json!({
            "type": "tool_result", "sessionId": SID, "callId": "call_1",
        }));
        assert!(matches!(
            &evs[..],
            [AgentEvent::Identity { .. }, AgentEvent::ToolUseEnd { tool_use_id: Some(t), .. }]
                if t == "call_1"
        ));
    }

    #[test]
    fn approval_asked_waits_on_the_named_call_and_allowed_once_resumes_it() {
        let evs = decode(json!({
            "type": "approval_asked", "sessionId": SID,
            "callId": "call_9",
            "toolName": "bash", "reason": "rm -rf",
        }));
        match &evs[..] {
            [
                AgentEvent::Identity { .. },
                AgentEvent::UserActionRequest {
                    reason,
                    tool_use_id: Some(t),
                    ..
                },
            ] => {
                assert_eq!(reason.as_deref(), Some("bash: rm -rf"));
                assert_eq!(t, "call_9");
            }
            other => panic!("unexpected: {other:?}"),
        }
        let evs = decode(json!({
            "type": "approval_decided", "sessionId": SID,
            "callId": "call_9",
            "toolName": "bash", "outcome": "allowed-once",
        }));
        assert!(matches!(
            &evs[..],
            [AgentEvent::Identity { .. }, AgentEvent::ToolUseStart { tool_use_id: Some(t), .. }]
                if t == "call_9"
        ));
        // A GATED dispatch resumes GENERIC on purpose: the Task pre-pass
        // keyed the tuid at the original `tool_call`, so a second Task Start
        // for it reads as a replay and would strand the slot in RequiresAction (the
        // decoder arm's comment owns the mechanism).
        let evs = decode(json!({
            "type": "approval_decided", "sessionId": SID,
            "callId": "call_10",
            "toolName": "subagent", "outcome": "allowed-once",
        }));
        assert!(matches!(
            &evs[..],
            [
                _,
                AgentEvent::ToolUseStart {
                    detail: Some(ToolDetail::Generic { .. }),
                    ..
                }
            ]
        ));
    }

    #[test]
    fn every_non_grant_outcome_ends_the_gated_call() {
        for outcome in ["rejected", "cancelled", "unavailable"] {
            let evs = decode(json!({
                "type": "approval_decided", "sessionId": SID,
                "callId": "call_9", "outcome": outcome,
            }));
            assert!(
                matches!(
                    &evs[..],
                    [AgentEvent::Identity { .. }, AgentEvent::ToolUseEnd { tool_use_id: Some(t), .. }]
                        if t == "call_9"
                ),
                "{outcome}: {evs:?}"
            );
        }
    }

    #[test]
    fn model_and_usage_map_to_their_events_with_fresh_spend_semantics() {
        let evs = decode(json!({
            "type": "model", "sessionId": SID,
            "model": "deepseek-v3", "reasoningEffort": "high",
        }));
        assert!(matches!(
            &evs[..],
            [AgentEvent::Identity { .. }, AgentEvent::ModelInfo { model: Some(m), effort: Some(e), .. }]
                if m == "deepseek-v3" && e == "high"
        ));
        let evs = decode(json!({
            "type": "usage", "sessionId": SID,
            "inputTokens": 100, "outputTokens": 20,
            "cacheWriteTokens": 7, "cacheReadTokens": 100000,
        }));
        assert!(
            matches!(
                &evs[..],
                [
                    AgentEvent::Identity { .. },
                    AgentEvent::Usage {
                        fresh_tokens: 127,
                        ..
                    }
                ]
            ),
            "cache reads are re-served context, never fresh spend: {evs:?}"
        );
    }

    #[test]
    fn the_subagent_dispatch_mints_task_and_ordinary_tools_stay_generic() {
        let evs = decode(json!({
            "type": "tool_call", "sessionId": SID,
            "callId": "c1", "toolName": "subagent",
        }));
        assert!(
            matches!(
                &evs[..],
                [
                    _,
                    AgentEvent::ToolUseStart {
                        detail: Some(ToolDetail::Task),
                        ..
                    }
                ]
            ),
            "{evs:?}"
        );
        let evs = decode(json!({
            "type": "tool_call", "sessionId": SID,
            "callId": "c3", "toolName": "subagent_fork",
        }));
        assert!(
            matches!(
                &evs[..],
                [
                    _,
                    AgentEvent::ToolUseStart {
                        detail: Some(ToolDetail::Task),
                        ..
                    }
                ]
            ),
            "{evs:?}"
        );
        let evs = decode(json!({
            "type": "tool_call", "sessionId": SID,
            "callId": "c2", "toolName": "bash",
        }));
        assert!(
            matches!(
                &evs[..],
                [
                    _,
                    AgentEvent::ToolUseStart {
                        detail: Some(ToolDetail::Generic { .. }),
                        ..
                    }
                ]
            ),
            "{evs:?}"
        );
    }

    #[test]
    fn unknown_and_field_less_payloads_breadcrumb_and_decode_nothing() {
        let quiet = crate::test_capture::capture_logs(|| {
            assert!(decode(json!({"type": "mystery_event", "sessionId": SID})).is_empty());
            assert!(decode(json!({"type": "tool_call"})).is_empty());
        });
        assert!(
            quiet.contains("unknown_event") && quiet.contains("mystery_event"),
            "an unknown type must leave a drift breadcrumb: {quiet}"
        );
        assert!(
            quiet.contains("missing_field"),
            "a sessionId-less payload must leave a breadcrumb: {quiet}"
        );
    }
}
