//! DeepSeek Harness (`dsh`) source. Hook-only: dsh persists a session as
//! zstd-compressed concatenated frames (`session.jsonl.zstd`, format v0 with no
//! migration promise), which no line reader can tail, so the only plane is a
//! pixtuoid-owned cordis plugin (`pixtuoid/src/install/dsh_plugin.mjs`; its
//! mount and never-block stories live on the template and its tests) mounted
//! through the home-level `$DSH_HOME/cordis.patch.yml`, forwarding emit-only
//! events through the shim. A REMOTE subagent provider
//! (`dsh-subagent-claude-code`/`-codex`/`-acp`) publishes no local child
//! session, so such a delegation paints nothing here — at most the spawned
//! CLI's own source shows a parentless sprite. Wire shape: upstream `packages/core/session/src/types.ts` (the
//! `session/event` feed), `packages/interaction/user-approval/src/types.ts`
//! (the log-only approval audit pair), `packages/core/agent/src/runtime-types.ts`
//! (#928). One dsh process hosts MANY sessions (the `web` profile is a
//! server; the opencode pid model), so `sessionId` is the identity and `_pid`
//! only drives liveness.

use crate::AgentId;
use crate::source::decoder::Hook;
use crate::source::decoder::{MAX_DECODED_FIELD_CHARS, ellipsize, non_empty, required};
use crate::source::registry::{
    ChildEndInference, Envelope, FocusChannel, HookDecoding, Hyphen, SessionField, SourceCaps,
    SourceDescriptor, SourceKind,
};
use crate::source::{AgentEvent, RequestKeys, Resolution, ToolDetail};

/// The dsh source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "dsh";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    display_name: "DeepSeek Harness",
    label_prefix: "ds",
    verified_version: "0.2.2-rc.5",
    version_probe: Some(&["dsh", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    // `DSH_HOME` is honored INSTALLER-side (the plugin file + patch row live
    // under it); hook-only, so core resolves no root to relocate.
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll {
            envelope: ENVELOPE,
            decode: decode_dsh_payload,
        }),
        caps: SourceCaps {
            // `agent/disposed` + the plugin's own effect-disposer sweep fire
            // on a clean quit (`dsh/fixtures/tool-run-hermetic`). SIGKILL fires
            // nothing, and an uncaught crash exits through `installFailLoud`,
            // whose only teardown is the terminal owner's `release`
            // (`packages/boot/app-boot/src/index.ts:860-899` at `dsh-v0.2.2-rc.5`):
            // both fall to `HookPidWatch` via the stamped pid.
            has_exit_signal: true,
            // Sessions are persistent; a `dsh tui --resume <session>`
            // (`docs/cli-help.md:55` at `dsh-v0.2.2-rc.5`) boots a NEW dsh whose
            // plugin emits a fresh `session_start` for the session's header id
            // on its `agent/created` (`install/dsh_plugin.mjs`) — the
            // SessionStart arm's ordinary re-registration, not this flag.
            resurrects_on_prompt: false,
            delegations_are_hook_silent: false,
            // The dispatch returns at launch (`packages/subagent/tool-subagent/src/index.ts:210`
            // at `dsh-v0.2.2-rc.5`), and the child's own `session_end` ends it
            // (`dsh/fixtures/subagent-hermetic`).
            child_end_inference: ChildEndInference::None,
            child_hooks_unnamed: false,
            // A Ctrl-C'd turn's `turn/end` is written only as the `agents`
            // service unloads (`packages/core/agent-loop/src/index.ts:494-538`
            // at `dsh-v0.2.2-rc.5`). Our plugin consumes that service, and a
            // consumer unloads before its provider
            // (`packages/session/session-persistence-jsonl/src/index.ts:1817-1819`,
            // same tag), so the plugin never sees that end.
            brackets_turns: false,
        },
        // dsh's `boot` mounts every patch-layer entry, our plugin's row
        // included, into one root `Context` in its own process
        // (`packages/boot/app-boot/src/index.ts:1220-1247` at `dsh-v0.2.2-rc.5`),
        // so the plugin's stamped `process.pid` (`install/dsh_plugin.mjs`) is
        // dsh's own.
        focus: FocusChannel::PluginStamp,
    },
    #[cfg(feature = "native")]
    native: crate::source::registry::Native::NONE,
};

/// The payload types the plugin sends — one per subscribed upstream event.
/// The plugin owns the upstream-name → wire-name mapping; the decoder owns
/// nothing upstream-shaped, so an upstream rename lands in ONE file.
const SESSION_START: &str = "session_start";
const SESSION_END: &str = "session_end";
const TURN_END: &str = "turn_end";
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
/// (`dsh/fixtures/subagent-hermetic`: the parent streams a `tool_call` naming it,
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

/// Where the plugin's payload names its event and session.
const ENVELOPE: Envelope = Envelope {
    event_field: "type",
    session_field: SessionField {
        within: None,
        field: "sessionId",
    },
};

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
/// If the payload lacks `sessionId`.
pub(crate) fn decode_dsh_payload(
    hook: &Hook<'_>,
) -> crate::source::decoder::DecodeResult<Option<Vec<AgentEvent>>> {
    const CALL_ID: &str = "callId";
    const TOOL_NAME: &str = "toolName";
    let (obj, ty) = (hook.obj, hook.event);
    let session_id = hook.session()?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, session_id);
    let parent_id = non_empty(obj, "parentSession").map(|p| AgentId::from_parts(SOURCE_NAME, p));
    let cwd = || {
        obj.get("cwd")
            .and_then(|c| c.as_str())
            .map(std::path::PathBuf::from)
    };
    let identity = || AgentEvent::identity(agent_id, SOURCE_NAME, session_id.to_string(), cwd());
    let call_id = || non_empty(obj, CALL_ID).map(str::to_string);
    let tool_display = || {
        non_empty(obj, TOOL_NAME)
            .map(|t| ellipsize(t, MAX_DECODED_FIELD_CHARS))
            .unwrap_or_default()
    };
    let tool_detail = || {
        if non_empty(obj, TOOL_NAME).is_some_and(|t| SUBAGENT_TOOLS.contains(&t)) {
            ToolDetail::Task
        } else {
            ToolDetail::Generic {
                display: tool_display(),
            }
        }
    };

    Ok(Some(match ty {
        SESSION_START => vec![
            AgentEvent::SessionStart {
                agent_id,
                source: SOURCE_NAME.to_string(),
                session_id: session_id.to_string(),
                cwd: cwd().unwrap_or_default(),
                parent_id,
            },
            identity(),
        ],
        SESSION_END => vec![AgentEvent::SessionEnd {
            agent_id,
            as_child: parent_id.is_some(),
        }],
        TURN_END => vec![identity(), AgentEvent::turn_end(agent_id)],
        TOOL_CALL => {
            let tool_use_id = required(SOURCE_NAME, TOOL_CALL, CALL_ID, |k| non_empty(obj, k))
                .map(str::to_string);
            required(SOURCE_NAME, TOOL_CALL, TOOL_NAME, |k| non_empty(obj, k));
            vec![
                identity(),
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id,
                    detail: Some(tool_detail()),
                    copy_id: None,
                },
            ]
        }
        TOOL_RESULT => vec![
            identity(),
            AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id: required(SOURCE_NAME, TOOL_RESULT, CALL_ID, |k| non_empty(obj, k))
                    .map(str::to_string),
            },
        ],
        APPROVAL_ASKED => {
            let reason = match (non_empty(obj, TOOL_NAME), non_empty(obj, "reason")) {
                (Some(t), Some(r)) => format!("{t}: {r}"),
                (Some(t), None) => t.to_string(),
                (None, Some(r)) => r.to_string(),
                (None, None) => "approval".to_string(),
            };
            vec![
                identity(),
                AgentEvent::user_action_request(
                    agent_id,
                    Some(ellipsize(&reason, MAX_DECODED_FIELD_CHARS)),
                    RequestKeys {
                        tool_use_id: call_id(),
                        ..RequestKeys::default()
                    },
                ),
            ]
        }
        APPROVAL_DECIDED => {
            // A renamed/dropped `outcome` would fail CLOSED (every grant
            // decodes as the deny arm's End) — breadcrumb it, because no
            // event-name drift row can see a field rename.
            let outcome = required(SOURCE_NAME, APPROVAL_DECIDED, "outcome", |k| {
                non_empty(obj, k)
            });
            let event = match (outcome == Some(OUTCOME_ALLOWED), call_id()) {
                // The resume: the gated call now runs; its `tool_call` was
                // already streamed before the gate (`dsh/fixtures/permission-hermetic`),
                // so only this lifts the wait.
                (true, Some(call)) => AgentEvent::UserActionResolved {
                    agent_id,
                    by: Resolution::Approved(call),
                },
                // A wait that named no call is answered by the agent's next
                // start (`ActionRequests::resolves`).
                (true, None) => AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id: None,
                    detail: Some(ToolDetail::Generic {
                        display: tool_display(),
                    }),
                    copy_id: None,
                },
                // rejected | cancelled | unavailable
                // (`packages/interaction/user-approval/src/types.ts:29-32` at
                // `dsh-v0.2.2-rc.5`): the call never runs.
                (false, tool_use_id) => AgentEvent::ToolUseEnd {
                    agent_id,
                    tool_use_id,
                },
            };
            vec![identity(), event]
        }
        MODEL => std::iter::once(identity())
            .chain(AgentEvent::model_info(
                agent_id,
                non_empty(obj, "model"),
                non_empty(obj, "reasoningEffort"),
            ))
            .collect(),
        USAGE => {
            let n = |k: &str| obj.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            // Fresh spend only: input + output + cache WRITES; cache reads are
            // re-served context (the CC `fresh_spend` semantics).
            vec![
                identity(),
                AgentEvent::Usage {
                    agent_id,
                    fresh_tokens: n("inputTokens")
                        .saturating_add(n("outputTokens"))
                        .saturating_add(n("cacheWriteTokens")),
                    reading_id: None,
                    copy_id: None,
                },
            ]
        }
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_decode;
    use serde_json::{Value, json};

    const SID: &str = "01b00000-0000-7000-8000-000000000001";

    fn decode(v: Value) -> Vec<AgentEvent> {
        test_decode(SOURCE_NAME, &v).unwrap()
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
            [
                AgentEvent::Identity { .. },
                AgentEvent::UserActionResolved { by: Resolution::Approved(t), .. },
            ] if t == "call_9"
        ));
        // A wait that named no call is answered by the agent's next start.
        let evs = decode(json!({
            "type": "approval_decided", "sessionId": SID,
            "toolName": "bash", "outcome": "allowed-once",
        }));
        assert!(matches!(
            &evs[..],
            [
                _,
                AgentEvent::ToolUseStart {
                    tool_use_id: None,
                    ..
                }
            ]
        ));
    }

    /// dsh's turn ends name no turn here: it does not bracket its turns
    /// (its `DESCRIPTOR` says why).
    #[test]
    fn a_turns_end_ends_the_agents_work() {
        let evs = decode(json!({"type": "turn_end", "sessionId": SID}));
        assert!(
            matches!(&evs[..], [AgentEvent::Identity { .. }, AgentEvent::TurnEnd { turn: None, agent_id }]
                if *agent_id == id()),
            "{evs:?}"
        );
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
}
