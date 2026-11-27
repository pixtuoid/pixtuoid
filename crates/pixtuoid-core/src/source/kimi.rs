//! Kimi Code CLI (`kimi`, MoonshotAI) source — HOOK-ONLY (no transcript watcher).
//!
//! Session transcripts DO exist (`<KIMI_CODE_HOME>/sessions/<workDirKey>/
//! <sessionId>/agents/main/wire.jsonl`, default home `~/.kimi-code`) but the wire
//! format is EXPLICITLY unstable: a `WIRE_PROTOCOL_VERSION` in a first-line
//! `metadata` record, a `wire/migration/` module, docs saying *"do not manually
//! edit files inside `sessions/`"*, and undocumented version-specific op `type`
//! strings. So pixtuoid does NOT tail it. The seam is **hooks**
//! (`<KIMI_CODE_HOME>/config.toml` `[[hooks]]`), whose payload is
//! CLAUDE-CODE-SHAPED: snake_case
//! `hook_event_name`/`session_id`/`cwd`/`tool_name`/`tool_input` with PascalCase
//! event VALUES — **except the per-call id, which Kimi spells `tool_call_id`**
//! (capture-verified: `kimi/fixtures/tool-run-hermetic` never carries `tool_use_id`). That
//! is a registry row, `ToolIdKey::ToolCall`, and NOT a reason to re-implement the
//! tool arms here: the name is the whole difference, and a copy of those arms
//! would silently stop tracking whatever they learn next.
//!
//! So every CC-shaped event rides the SHARED arms
//! (`decoder::decode_hook_payload`), keyed on `session_id`;
//! `decode_kimi_hook_custom` claims only what Kimi needs read unlike CC and
//! DECLINES (`Ok(None)`) for everything else.
//!
//! **Subagents** (Kimi's "agent swarm") are deliberately NOT modeled: the
//! `SubagentStart`/`SubagentStop` payload shape is uncaptured, so sessions render
//! FLAT until a byte-real capture can add nesting.

use crate::source::decoder::{
    DecodeResult as Result, hook_identity, refuse_unknown_event, shared_hook_arms,
};

use crate::AgentId;
use crate::source::decoder::Hook;
use crate::source::registry::{
    ChildEndInference, Corroboration, FocusChannel, HookDecoding, Hyphen, SourceCaps,
    SourceDescriptor, SourceKind, ToolIdKey,
};
use crate::source::{AgentEvent, registry};

/// The Kimi CLI source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "kimi";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    display_name: "Kimi Code CLI",
    label_prefix: "km",
    verified_version: "2.8.1",
    version_probe: Some(&["kimi", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::Shared {
            // The ONE source that does not spell the per-call id `tool_use_id`.
            tool_id_key: ToolIdKey::ToolCall,
            extend: decode_kimi_hook_custom,
        }),
        caps: SourceCaps {
            // `SessionEnd` fires on exit — best-effort counts; abrupt exits fall
            // to the stale-sweep.
            has_exit_signal: true,
            // kimi installs no `UserPromptSubmit` (`KIMI_EVENTS`), so no prompt
            // re-registers: a swept session returns on its next tool call or turn end.
            resurrects_on_prompt: false,
            // Consumed via the `Agent` tool: 0.36.1's `AgentToolInputSchema` injects
            // `subagent_type: "coder"` when the model omits it, so `make_tool_detail`
            // mints Task. `AgentSwarm` has NO such injection, so an omitting swarm
            // call mints Generic instead; `true` can only over-retain a dead slot.
            delegations_are_hook_silent: true,
            child_end_inference: ChildEndInference::TaskDrain,
            // A subagent's tool hooks carry the session's id alone
            // (kimi-code@2.2.0 `agentExternalHooksService.ts:301-313`).
            child_hooks_unnamed: true,
            brackets_turns: false,
        },
        // kimi-code 2.2.0 spawns every hook with `shell: true`, which Node runs as
        // `/bin/sh -c` on Unix and `%ComSpec%` on Windows
        // (`packages/agent-core-v2/src/features/externalHooks/internal/runHook.ts:66-67`,
        // https://nodejs.org/api/child_process.html): re-check both if either changes.
        focus: FocusChannel::ShimStamp(Corroboration::OnceViaShell),
    },
    #[cfg(feature = "native")]
    native: crate::source::registry::Native::NONE,
};

/// Decode the hook events Kimi needs read unlike CC's, and DECLINE
/// (`Ok(None)`) for the rest, so the CC-shaped remainder falls through to
/// `decoder::decode_hook_payload`'s shared arms.
///
/// `Stop`, and `StopFailure` and `Interrupt`, a failed and a cancelled turn
/// end decoded as `Stop` — kimi's only per-turn hooks (`KIMI_EVENTS`), so each
/// claims its session: a chat-only session running before the office, or
/// swept since, registers on its next turn end. Claimed here, not in the
/// shared `Stop` arm, whose CC children an `Identity` would re-create past
/// their cascade.
pub(crate) fn decode_kimi_hook_custom(hook: &Hook<'_>) -> Result<Option<Vec<AgentEvent>>> {
    match hook.event {
        // A cancelled turn fires `Interrupt` and a failed one `StopFailure` in place
        // of `Stop`
        // (`packages/agent-core-v2/src/features/externalHooks/agent/agentExternalHooksService.ts:423-429`
        // at `@moonshot-ai/kimi-code@2.8.0`; `kimi/fixtures/model-error-hermetic`).
        "Stop" | "StopFailure" | "Interrupt" => {}
        _ => return Ok(None),
    }
    // Read under the event's own name, so a missing id breadcrumbs as sent.
    let session_id = hook.session()?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, session_id);
    let mut evs = vec![hook_identity(agent_id, SOURCE_NAME, session_id, hook.obj)];
    let stop = Hook {
        event: "Stop",
        ..*hook
    };
    let desc = registry::descriptor_for(SOURCE_NAME);
    evs.extend(shared_hook_arms(&stop, desc)?.ok_or_else(|| refuse_unknown_event(&stop))?);
    Ok(Some(evs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::{test_decode, test_hook};
    use serde_json::json;

    /// A failed or a cancelled turn's end decodes as `Stop` does.
    #[test]
    fn a_failure_or_cancel_decodes_as_its_shared_arm() {
        for variant in ["StopFailure", "Interrupt"] {
            let decode = |event| {
                crate::source::decoder::decode_hook_payload(json!({
                    "hook_event_name": event,
                    "session_id": "s",
                    "cwd": "/repo",
                    "_pixtuoid_source": SOURCE_NAME,
                }))
                .expect("decodes")
            };
            assert_eq!(decode(variant), decode("Stop"), "{variant}");
        }
    }

    #[test]
    fn unclaimed_events_decline_to_the_shared_arms() {
        // SubagentStart is deliberately UNregistered — included to pin that the
        // custom decoder declines it too.
        for ev in [
            "SessionStart",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "PermissionRequest",
            "SessionEnd",
            "UserPromptSubmit",
            "SubagentStart",
        ] {
            let v = json!({"hook_event_name": ev, "session_id": "s", "cwd": "/repo"});
            let out = decode_kimi_hook_custom(&test_hook(SOURCE_NAME, &v)).expect("decodes");
            assert!(
                out.is_none(),
                "{ev} must be declined (Ok(None)) by the custom decoder, got {out:?}"
            );
        }
    }

    /// The hook plane's `session_id` chokepoint: its drift breadcrumb too.
    #[test]
    fn a_claimed_event_without_session_id_errs_and_breadcrumbs() {
        use crate::source::drift::{DriftKind, breadcrumbed};
        for ev in ["Stop", "StopFailure", "Interrupt"] {
            for payload in [
                json!({ "hook_event_name": ev, "cwd": "/repo" }),
                json!({ "hook_event_name": ev, "session_id": "" }),
            ] {
                let logs = crate::test_capture::capture_logs(|| {
                    assert!(test_decode(SOURCE_NAME, &payload).is_err(), "{payload}");
                });
                let fields = [("event", ev), ("field", "session_id")];
                assert!(
                    breadcrumbed(&logs, SOURCE_NAME, DriftKind::MissingField, &fields),
                    "{fields:?} in\n{logs}"
                );
            }
        }
    }

    /// The negative control for kimi's `ToolIdKey::ToolCall` registry row: the CC
    /// name kimi never sends must NOT be read, or the row would be decorative.
    #[test]
    fn the_cc_spelling_is_not_read_for_kimi() {
        let evs = crate::source::decoder::decode_hook_payload(json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session_abc",
            "cwd": "/w",
            "tool_name": "Read",
            "tool_use_id": "call_1",
            "_pixtuoid_source": SOURCE_NAME,
        }))
        .expect("decodes");
        assert!(
            evs.iter().any(|e| matches!(
                e,
                AgentEvent::ToolUseStart {
                    tool_use_id: None,
                    ..
                }
            )),
            "kimi must ignore tool_use_id, got {evs:?}"
        );
    }
}
