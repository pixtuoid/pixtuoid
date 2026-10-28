use std::path::{Path, PathBuf};

use crate::source::decoder::{DecodeError, DecodeResult as Result};
use serde_json::Value;

use crate::source::decoder::{
    MAX_DECODED_FIELD_CHARS, TailActivity, cwd_basename_label, ellipsize, extract_top_level_cwd,
    make_tool_detail, parsed_tail_lines,
};

use crate::AgentId;
use crate::source::AgentEvent;
use crate::source::registry::{
    ChildEndInference, FocusChannel, HookDecoding, HookObject, Hyphen, SourceCaps,
    SourceDescriptor, SourceKind, ToolIdKey, Transcript,
};

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::{ClaudeCodeSource, cc_watcher, live_cc_session_ids};

/// The Claude Code source's registry name (its `SourceDescriptor.name`).
///
/// homebrew-core contract: the id the formula's `test do` connects, which fails
/// with no agent CLI installed, so a rename owes a test update there right after
/// the release that ships it (CONTRIBUTING.md#releasing).
pub const SOURCE_NAME: &str = "claude-code";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    label_prefix: "cc",
    verified_version: "2.1.311",
    version_probe: Some(&["claude", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: Some("CLAUDE_CONFIG_DIR"),
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: decode_cc_line,
            // The filename stem IS the session UUID the hook keys on.
            id_from_path: cc_id_from_path,
            // The workflow orchestrator's foreign-schema `journal.jsonl` lives
            // in the SAME projects tree.
            path_filter: admits_transcript,
            cwd_extractor: extract_top_level_cwd,
            parent_extractor: None,
        }),
        hook: Some(HookDecoding::Shared {
            tool_id_key: ToolIdKey::ToolUse,
            // They swap the event's SUBJECT (child AgentId ≠ session AgentId), which
            // the shared arms can't express; Stop is a fleet subagent's ONLY end (#241).
            extend: decode_cc_hook_custom,
        }),
        caps: SourceCaps {
            has_exit_signal: true,
            // CC's JSONL SessionStart is first-sight-only, so a swept slot
            // would not walk back in — moot with a real exit signal anyway.
            resurrects_on_prompt: false,
            delegations_are_hook_silent: false,
            // A subagent turn fires SubagentStart, and SubagentStop once it has
            // finished responding; an Agent call that backgrounds its subagent
            // returns `async_launched` at launch (hooks.md).
            child_end_inference: ChildEndInference::None,
            child_hooks_unnamed: false,
            brackets_turns: false,
        },
        focus: FocusChannel::TranscriptProbe,
    },
};

/// The label the attachment decoder synthesizes for the `ultra_effort_exit`
/// marker — deliberately NOT a real effort word, so last-seen-wins kills the
/// flame instantly, and suppressed by `pixtuoid_scene::burn::fresh_effort` so
/// the sentinel never reaches the dossier. In-workspace decoder↔painter
/// contract token, not a stable API.
#[doc(hidden)]
pub const ULTRA_EXIT_LABEL: &str = "ultra_exit";

const ATTACHMENT_ULTRA_ENTER: &str = "ultra_effort_enter";
const ATTACHMENT_ULTRATHINK: &str = "ultrathink_effort";
const ATTACHMENT_ULTRA_EXIT: &str = "ultra_effort_exit";

/// CC's session/agent id = the transcript filename stem, which is
/// cwd-independent (the cwd-derived project-dir is the *parent* dir, not the
/// stem): `<uuid>.jsonl` → `<uuid>` for a root, `agent-<id>.jsonl` →
/// `agent-<id>` for a subagent. CC session UUIDs and agent-ids are lowercase,
/// so the Windows path fold is inert here.
pub fn cc_id_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// CC's source-specific hook arms, which change the event's SUBJECT to a
/// subagent's AgentId where the shared session-keyed arms cannot:
/// `SubagentStart`/`SubagentStop`, and any other event a subagent fires, which
/// names it in `agent_id` beside the ROOT's `session_id` (hooks.md: the
/// `agent_id` and `agent_type` "that identify the subagent"). A main-thread event
/// falls through (`Ok(None)`). Needed despite JSONL registration: a
/// Workflow-tool fleet's subagents carry no `Agent` tool_use and no end marker,
/// so without `SubagentStop` they hold desks until the stale sweep.
pub(crate) fn decode_cc_hook_custom(obj: &HookObject) -> Result<Option<Vec<AgentEvent>>> {
    let event = obj
        .get("hook_event_name")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    if event != "SubagentStart" && event != "SubagentStop" {
        return crate::source::decoder::subagent_fired(obj, SOURCE_NAME, subagent_session);
    }
    // Registry contract: claim our two events FULLY (Err on malformed), never
    // fall through — an empty id mints a phantom that never coalesces.
    let session_id = obj
        .get("session_id")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| DecodeError::missing_in(SOURCE_NAME, event, "session_id"))?;
    let wire_agent_id = obj
        .get(crate::source::decoder::SUBAGENT_ID)
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            DecodeError::missing_in(SOURCE_NAME, event, crate::source::decoder::SUBAGENT_ID)
        })?;
    let prefixed = subagent_session(wire_agent_id);
    if event == "SubagentStart" {
        let cwd = obj.get("cwd").and_then(|s| s.as_str()).unwrap_or("").into();
        Ok(Some(vec![AgentEvent::SessionStart {
            agent_id: AgentId::from_parts(SOURCE_NAME, &prefixed),
            source: SOURCE_NAME.to_string(),
            session_id: prefixed,
            cwd,
            parent_id: Some(AgentId::from_parts(SOURCE_NAME, session_id)),
        }]))
    } else {
        // `SubagentStop` also carries `agent_transcript_path`, whose stem is the
        // authoritative key — exact parity with the watcher's own deriver.
        let path_key = obj
            .get("agent_transcript_path")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .map(|p| cc_id_from_path(Path::new(&crate::id::normalize_path_key(p))))
            .filter(|s| !s.is_empty());
        if let Some(ref k) = path_key
            && *k != prefixed
        {
            // Upstream scheme change: hook-FIRST Start registrations go phantom.
            crate::source::drift::shape_drift(
                SOURCE_NAME,
                &format!(
                    "SubagentStop transcript stem `{k}` != prefixed agent_id \
                         `{prefixed}`; keying on the stem"
                ),
            );
        }
        Ok(Some(vec![AgentEvent::SessionEnd {
            agent_id: AgentId::from_parts(SOURCE_NAME, &path_key.unwrap_or(prefixed)),
            as_child: true,
        }]))
    }
}

/// A subagent's session id from the wire's `agent_id`: BARE hex, while the
/// watcher's id space is `agent-<id>`; the CC docs' example shows one already
/// prefixed.
fn subagent_session(wire_agent_id: &str) -> String {
    if wire_agent_id.starts_with("agent-") {
        wire_agent_id.to_string()
    } else {
        format!("agent-{wire_agent_id}")
    }
}

/// Resolve `CLAUDE_CONFIG_DIR`. Upstream (read out of the 2.1.226 binary) is
/// `(env.CLAUDE_CONFIG_DIR ?? join(homedir(), ".claude")).normalize("NFC")` —
/// nothing else layers on, no XDG / `%APPDATA%` / profile / legacy dir. The two
/// deliberate divergences are `""`/`"  "` treated as unset, and NFC dropped.
/// Internal cross-crate helper, not a stable API.
#[doc(hidden)]
pub fn claude_config_dir() -> Option<PathBuf> {
    crate::platform::path_env("CLAUDE_CONFIG_DIR")
        .map(|d| crate::platform::warn_if_relative_override("CLAUDE_CONFIG_DIR", d))
}

const ASSISTANT: &str = "assistant";
const USER: &str = "user";

/// The CC transcript types whose `message` payload this module decodes. CC's
/// TRANSCRIPT has no fetchable schema: the nightly's latest-release census and
/// this breadcrumb guard it, but only these two names can break rendering, so
/// only they may raise one. Pinned to the decode arms
/// by `the_decoded_set_is_exactly_what_the_arms_match`.
const DECODED_TYPES: &[&str] = &[ASSISTANT, USER];

/// Is this line a turn's record? A timestamped `message.role` +
/// `message.content` is what a renamed [`DECODED_TYPES`] member would still
/// arrive with, so it separates a rename from a merely-new sidecar type WITHOUT
/// mirroring CC's vocabulary (#935); `api-request-blob` stores a request's
/// message untimestamped (#1694).
fn is_turn_record(obj: &serde_json::Map<String, Value>) -> bool {
    obj.contains_key("timestamp")
        && obj
            .get("message")
            .and_then(Value::as_object)
            .is_some_and(|m| m.contains_key("role") && m.contains_key("content"))
}

/// The `attributionAgent` label, namespace-stripped and capped. `None` for an
/// EMPTY value or a trailing colon ("ns:" splits to an empty segment): a
/// `Rename { label: "" }` blanks a good hook-derived label with no recovery
/// until the next Rename. Capped at decode because transcript content persists
/// in slot state.
fn attribution_label(obj: &serde_json::Map<String, Value>) -> Option<String> {
    let name = obj
        .get("attributionAgent")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?;
    let seg = name.rsplit_once(':').map_or(name, |(_, tail)| tail);
    (!seg.is_empty()).then(|| ellipsize(seg, MAX_DECODED_FIELD_CHARS))
}

/// The effort an `attachment` line implies. CC stamps a periodic reminder while
/// ultra-class effort is active, plus an EXIT marker on leaving it, and the wire
/// carries no effort VALUE — so each marker's label is synthesized here, the
/// exit's as `ULTRA_EXIT_LABEL`. The `/effort` picker's chosen level is not
/// derivable from the transcript (empty command args); the hook path reads it
/// (`hook_effort`).
fn attachment_effort(obj: &serde_json::Map<String, Value>) -> Option<&'static str> {
    match obj
        .get("attachment")
        .and_then(|a| a.get("type"))
        .and_then(|t| t.as_str())?
    {
        ATTACHMENT_ULTRA_ENTER => Some("ultra"),
        ATTACHMENT_ULTRATHINK => Some("ultrathink"),
        ATTACHMENT_ULTRA_EXIT => Some(ULTRA_EXIT_LABEL),
        _ => None,
    }
}

/// FRESH spend from an assistant line's `usage` — new input + cache WRITES +
/// output. `cache_read_input_tokens` is re-served context, not new spend, and
/// dwarfs the rest. Sidechain lines carry usage too and key to the same session
/// id, so the meter is the SESSION's burn, subagents included.
fn fresh_spend(message: &serde_json::Map<String, Value>) -> u64 {
    let Some(usage) = message.get("usage").and_then(|u| u.as_object()) else {
        return 0;
    };
    let field = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    field("input_tokens")
        .saturating_add(field("cache_creation_input_tokens"))
        .saturating_add(field("output_tokens"))
}

/// Decode one CC JSONL transcript line into 0..N AgentEvents, keyed on the
/// filename STEM — the hook decoder's `session_id` and the watcher's
/// deriver key the same way, so every CC keying site coalesces onto one sprite.
/// There is deliberately NO user-content arm: content is user-controllable and a
/// message QUOTING the slash-command wrapper would false-positive, so lifecycle
/// is the SessionEnd hook + the idle sweep.
pub fn decode_cc_line(transcript_path: &str, source: &str, v: Value) -> Vec<AgentEvent> {
    let agent_id = AgentId::from_parts(source, &cc_id_from_path(Path::new(transcript_path)));
    let Some(obj) = v.as_object() else {
        return vec![];
    };

    let mut out = Vec::new();
    let ty = obj.get("type").and_then(|s| s.as_str()).unwrap_or("");

    // Both directions of the one rename that can freeze a sprite.
    let message = obj.get("message").and_then(|m| m.as_object());
    if DECODED_TYPES.contains(&ty) {
        if message.is_none() {
            crate::source::drift::missing_field(source, ty, "message");
        }
    } else if !ty.is_empty() && is_turn_record(obj) {
        crate::source::drift::unknown_event(source, ty);
    }

    if let Some(label) = attribution_label(obj) {
        out.push(AgentEvent::Rename { agent_id, label });
    }

    if ty == "attachment"
        && let Some(effort) = attachment_effort(obj)
    {
        out.push(AgentEvent::ModelInfo {
            agent_id,
            model: None,
            effort: Some(effort.to_string()),
        });
    }

    let Some(message) = message else {
        return out;
    };
    // `<synthetic>` is CC's marker for tool-generated/error turns, not a model.
    if ty == ASSISTANT {
        if let Some(model) = message
            .get("model")
            .and_then(|m| m.as_str())
            .filter(|m| !m.is_empty() && *m != "<synthetic>")
        {
            out.push(AgentEvent::ModelInfo {
                agent_id,
                model: Some(ellipsize(model, MAX_DECODED_FIELD_CHARS)),
                effort: None,
            });
        }
        let fresh = fresh_spend(message);
        if fresh > 0 {
            out.push(AgentEvent::Usage {
                agent_id,
                fresh_tokens: fresh,
            });
        }
    }
    let content = message.get("content");
    match (ty, content) {
        (ASSISTANT, Some(Value::Array(blocks))) => {
            for block in blocks {
                let Some(bobj) = block.as_object() else {
                    continue;
                };
                let btype = bobj.get("type").and_then(|s| s.as_str()).unwrap_or("");
                if btype != "tool_use" {
                    continue;
                }
                let id = bobj.get("id").and_then(|s| s.as_str()).map(String::from);
                let name = bobj
                    .get("name")
                    .and_then(|s| s.as_str())
                    .unwrap_or_else(|| {
                        crate::source::drift::missing_field(SOURCE_NAME, "tool_use", "name");
                        "?"
                    });
                out.push(AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id: id,
                    detail: Some(make_tool_detail(SOURCE_NAME, name, bobj.get("input"))),
                });
            }
        }
        (USER, Some(Value::Array(blocks))) => {
            for block in blocks {
                let Some(bobj) = block.as_object() else {
                    continue;
                };
                let btype = bobj.get("type").and_then(|s| s.as_str()).unwrap_or("");
                if btype != "tool_result" {
                    continue;
                }
                let id = bobj
                    .get("tool_use_id")
                    .and_then(|s| s.as_str())
                    .map(String::from);
                out.push(AgentEvent::ToolUseEnd {
                    agent_id,
                    tool_use_id: id,
                });
            }
        }
        _ => {}
    }
    out
}

/// The CC transcript types whose lines are AGENT TURN activity — membership by
/// TURN-AUTHORSHIP, not by whether we decode the type. Anything else is session
/// metadata unless it is a turn record (see `cc_activity_recency`): a
/// STARTING session writes `bridge-session` / `pr-link` runs into an OTHER,
/// long-dead transcript, bumping the mtime first-sight trusted as liveness.
const ACTIVITY_TYPES: &[&str] = &[
    "assistant",
    "attachment",
    "file-history-delta",
    "queue-operation",
    // Carries no `AgentEvent` but IS the session speaking — it closes a live tail.
    "system",
    "user",
];

/// The workflow/skills orchestrator writes a `journal.jsonl` sidecar under
/// `<uuid>/subagents/workflows/wf_*/` — a FOREIGN schema (top-level
/// `type:"started"`/`"result"`) in the SAME tree the CC watcher walks. A
/// DENYLIST: real subagent transcripts nested under `workflows/wf_*/` stay
/// admitted — misfiltering one costs a sprite, a foreign sidecar breadcrumbs.
pub(crate) fn admits_transcript(path: &Path) -> bool {
    path.file_name().and_then(|s| s.to_str()) != Some("journal.jsonl")
}

/// Whether one tail line is a turn: `None` when it classifies as NOTHING — a
/// non-object, or an object with no `type` — which is not evidence the recent
/// bytes were metadata. An UNNAMED type still counts as a turn when it is a
/// turn record; the day CC renames one, reading it as "not a turn" would
/// gate every live session at once.
fn classifies_as_turn(v: &Value) -> Option<bool> {
    let obj = v.as_object()?;
    let ty = obj.get("type").and_then(|t| t.as_str())?;
    Some(ACTIVITY_TYPES.contains(&ty) || is_turn_record(obj))
}

/// When this transcript's SESSION last wrote, read from its tail — the honest
/// TIGHTENING of the file's mtime in the first-sight recency gate (`ACTIVITY_TYPES`
/// documents the write that made mtime lie). Widening the tail window is NOT an
/// alternative: these lines carry file contents, so no fixed window bounds them.
/// Accepted residual — an UNVOUCHED session (headless `claude -p`, a non-standard
/// projects root, EVERY CC session on Windows) whose tail is momentarily all
/// sidecar is invisible until its next turn; with hooks it registers regardless.
pub fn cc_activity_recency(tail: &[u8]) -> TailActivity {
    let mut newest: Option<u64> = None;
    let mut saw_turn = false;
    let mut saw_classified = false;
    let mut saw_unclassifiable = false;
    for v in parsed_tail_lines(tail) {
        let Some(is_turn) = classifies_as_turn(&v) else {
            saw_unclassifiable = true;
            continue;
        };
        saw_classified = true;
        if !is_turn {
            continue;
        }
        saw_turn = true;
        if let Some(secs) = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(crate::source::decoder::rfc3339_to_epoch_secs)
        {
            newest = Some(newest.map_or(secs, |n: u64| n.max(secs)));
        }
    }
    match newest {
        Some(secs) => TailActivity::At(secs),
        // A stampless turn still means the session wrote here, so it must not gate.
        None if saw_classified && !saw_turn && !saw_unclassifiable => TailActivity::SidecarOnly,
        None => TailActivity::Unknown,
    }
}

/// CC session-end checker: structural lifecycle markers only, never a byte
/// scan of message content.
pub fn cc_session_ended(tail: &[u8]) -> bool {
    // Last-marker-wins: a later `session_start` resets a `session_end` earlier
    // in the window, so this can't reduce to a simple `.any(predicate)`.
    let mut last_is_end = false;
    for v in parsed_tail_lines(tail) {
        let subtype = v.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
        let hook = v
            .get("hook_event_name")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        if subtype == "session_start" {
            last_is_end = false;
        }
        if subtype == "session_end" || hook == "SessionEnd" {
            last_is_end = true;
        }
    }
    last_is_end
}

/// CC label: subagent paths → "subagent", else "cc·" + cwd basename. With `cwd`
/// unknown (a seed line carrying no `cwd` still fires the JSONL Rename), fall back
/// to the CC **project dir**, whose name encodes the cwd with '/'→'-' — or an
/// empty-cwd Rename silently degrades a good hook-derived `cc·dotfiles` to `cc`.
pub fn cc_derive_label(path: &Path, source: &str, cwd: &Path) -> String {
    // Slash-bounded, and shared with `detect_parent_id`: a loose `"subagents"`
    // substring mislabels a `subagents-paper` repo's parent transcript.
    if crate::source::decoder::is_subagent_path(path) {
        return "subagent".to_string();
    }
    // The `cc` prefix is a registry fact, not a literal (invariant #3).
    let prefix = crate::source::decoder::label_prefix_for(source);
    if let Some(label) = cwd_basename_label(prefix, cwd) {
        return label;
    }
    if let Some(base) = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .and_then(|proj| proj.rsplit('-').find(|s| !s.is_empty()))
    {
        // This branch BYPASSES `cwd_basename_label`'s cap chokepoint.
        return format!(
            "{prefix}·{}",
            crate::source::decoder::ellipsize(
                base,
                crate::source::decoder::MAX_DECODED_FIELD_CHARS
            )
        );
    }
    prefix.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;
    use serde_json::json;

    /// A subagent's tool hook names it in `agent_id` (hooks.md;
    /// `claude_code/fixtures/subagent-hermetic`): every event it decodes to lands on
    /// the subagent, Identity's session too, whatever `session_id` says.
    #[test]
    fn a_subagents_tool_hook_lands_on_the_subagent() {
        let sid = "5717f13f-3c42-4a73-bfa5-3cd14932f155";
        let child = AgentId::from_parts(SOURCE_NAME, "agent-a234ddfa51533488f");
        let root = AgentId::from_parts(SOURCE_NAME, sid);
        for event in [
            "PreToolUse",
            "PostToolUse",
            "Notification",
            "PermissionRequest",
        ] {
            let payload = |agent_id: Option<&str>| {
                let mut v = json!({
                    "hook_event_name": event, "session_id": sid, "cwd": "/repo",
                    "transcript_path": format!("/h/.claude/projects/-repo/{sid}.jsonl"),
                    "agent_type": "general-purpose", "tool_name": "Read", "tool_use_id": "toolu_1",
                    "tool_input": {"file_path": "/repo/NOTE.txt"}, "message": "m",
                    "_pixtuoid_source": SOURCE_NAME,
                });
                if let Some(a) = agent_id {
                    v["agent_id"] = json!(a);
                }
                crate::source::decoder::decode_hook_payload(v).expect("decodes")
            };
            let evs = payload(Some("a234ddfa51533488f"));
            assert!(!evs.is_empty(), "{event}");
            assert!(
                evs.iter().all(|e| e.agent_id() == child),
                "{event}: {evs:?}"
            );
            assert!(
                evs.iter()
                    .any(|e| matches!(e, AgentEvent::Identity { session_id, .. }
                    if session_id == "agent-a234ddfa51533488f")),
                "{event}: the Identity must name the subagent's session: {evs:?}"
            );
            for main_thread in [payload(None), payload(Some(""))] {
                assert!(main_thread.iter().all(|e| e.agent_id() == root), "{event}");
            }
        }
    }

    /// The half that still has to alarm: a RENAMED turn type. It is recognised
    /// by the payload we decode, not by absence from a list, so nothing has to
    /// track CC's vocabulary for this to fire.
    #[test]
    fn a_renamed_turn_type_still_alarms_because_it_carries_the_payload_we_read() {
        let out = crate::test_capture::capture_logs(|| {
            let line = json!({
                "type": "assistant-v2",
                "timestamp": "2026-10-17T00:00:00Z",
                "message": {"role": "assistant", "content": [], "model": "m"},
            });
            let _ = decode_cc_line("/p/s.jsonl", SOURCE_NAME, line);
        });
        assert!(
            out.contains(crate::source::drift::TARGET) && out.contains("assistant-v2"),
            "a renamed turn type went unreported:\n{out}"
        );
    }

    /// `DECODED_TYPES` is a SECOND copy of what the decode arms match, and the
    /// arms cannot be enumerated at runtime — so shrinking the list would
    /// silently stop the lost-payload alarm for the dropped name.
    #[test]
    fn the_decoded_set_is_exactly_what_the_arms_match() {
        assert_eq!(DECODED_TYPES, ["assistant", "user"]);
        // Each name reaches a real arm — the block shape each arm actually matches.
        let block = |ty: &str| match ty {
            "user" => json!({"type": "tool_result", "tool_use_id": "t1"}),
            _ => json!({"type": "tool_use", "id": "t1", "name": "Bash"}),
        };
        let out = crate::test_capture::capture_logs(|| {
            for ty in DECODED_TYPES {
                let line = json!({
                    "type": ty,
                    "timestamp": "2026-10-17T00:00:00Z",
                    "message": {"role": "assistant", "content": [block(ty)]},
                });
                assert!(
                    !decode_cc_line("/p/s.jsonl", SOURCE_NAME, line).is_empty(),
                    "{ty} is in DECODED_TYPES but reaches no decode arm"
                );
            }
        });
        assert!(
            !out.contains(crate::source::drift::TARGET),
            "a healthy decoded line reached the drift log — these lines DO carry \
             `message.role`+`content`, so only the arms being mutually exclusive \
             keeps them out; split them and every line of the two hottest types \
             breadcrumbs, #935's flood aimed at what it protects:\n{out}"
        );
    }

    /// The inverse rename: the NAME survives and the payload moves. Nothing
    /// caught this before — `decode_cc_line` returned early on the absent
    /// `message` without a word.
    #[test]
    fn a_decoded_type_that_lost_its_payload_alarms() {
        let out = crate::test_capture::capture_logs(|| {
            for ty in DECODED_TYPES {
                let line = json!({"type": ty, "turn": {"role": "assistant"}});
                let _ = decode_cc_line("/p/s.jsonl", SOURCE_NAME, line);
            }
        });
        assert!(
            out.contains(crate::source::drift::TARGET) && out.contains("message"),
            "a decoded type losing `message` must name the field it lost:\n{out}"
        );
        for ty in DECODED_TYPES {
            assert!(
                out.contains(ty),
                "{ty} lost `message` and decoded to nothing, silently:\n{out}"
            );
        }
    }

    /// The tail classifier's two directions, which no other test could tell
    /// apart from the old name-list one: a type we have never seen reads as a
    /// TURN when it carries the payload, and as SIDECAR when it does not.
    #[test]
    fn an_unnamed_type_is_a_turn_only_when_it_carries_the_payload() {
        let line = |ty: &str, body: &str| {
            format!(r#"{{"type":"{ty}","timestamp":"2026-07-29T05:46:24.525Z"{body}}}"#)
        };
        let turn = r#","message":{"role":"assistant","content":[]}"#;

        // The scan visits EVERY line, so a two-line tail is the shape to assert on.
        let two = |a: String, b: String| format!("{a}\n{b}\n").into_bytes();
        assert!(
            matches!(
                cc_activity_recency(&two(line("pr-link", ""), line("assistant", ""))),
                TailActivity::At(_)
            ),
            "a sidecar line before a turn must not stop the scan"
        );
        for odd in [r#"{"timestamp":"2026-07-29T05:46:24.525Z"}"#, "42"] {
            assert_eq!(
                cc_activity_recency(&two(odd.to_string(), line("pr-link", ""))),
                TailActivity::Unknown,
                "{odd} must leave the tail unclassifiable, never SidecarOnly — both \
                 bails are separate lines of code (a valid-JSON NON-object, and an \
                 object with no `type`), and SidecarOnly gates the session while \
                 Unknown does not, so the sidecars around it must not vouch for it"
            );
        }

        match cc_activity_recency(line("assistant-v2", turn).as_bytes()) {
            TailActivity::At(_) => {}
            other => panic!("a payload-carrying unnamed type must date the tail, got {other:?}"),
        }
        assert_eq!(
            cc_activity_recency(line("history-suppression", "").as_bytes()),
            TailActivity::SidecarOnly,
            "a payload-less unnamed type must read as sidecar — it is metadata, and \
             must not vouch for a dead session's mtime"
        );
        // Spelled out, NOT iterated from `ACTIVITY_TYPES`: a loop over the list the
        // classifier reads would shrink on a drop instead of failing.
        let by_name = [
            "assistant",
            "attachment",
            "file-history-delta",
            "queue-operation",
            "system",
            "user",
        ];
        assert_eq!(ACTIVITY_TYPES, by_name, "the turn-by-NAME set changed");
        for ty in by_name {
            assert!(
                matches!(
                    cc_activity_recency(line(ty, "").as_bytes()),
                    TailActivity::At(_)
                ),
                "{ty} is a turn by NAME and must date the tail"
            );
        }
        for half in [
            r#","message":{"role":"assistant"}"#,
            r#","message":{"content":[]}"#,
        ] {
            assert_eq!(
                cc_activity_recency(line("some-new-type", half).as_bytes()),
                TailActivity::SidecarOnly,
                "half a message envelope must not read as a turn — the predicate is \
                 the CONJUNCTION, or a future CC sidecar type floods: {half}"
            );
        }
    }

    #[test]
    fn a_metadata_only_tail_does_not_refresh_the_activity_clock() {
        // A session whose last real turn was days ago, then a run of sidecar
        // lines a DIFFERENT live session appended.
        let tail = concat!(
            r#"{"type":"assistant","timestamp":"2026-07-29T05:46:24.525Z"}"#,
            "\n",
            r#"{"type":"bridge-session","sessionId":"s"}"#,
            "\n",
            r#"{"type":"custom-title","sessionId":"s"}"#,
            "\n",
            r#"{"type":"pr-link","timestamp":"2026-08-02T05:56:43.894Z"}"#,
            "\n",
        );
        assert_eq!(
            cc_activity_recency(tail.as_bytes()),
            TailActivity::At(1_785_303_984),
            "the July turn is the newest ACTIVITY, whatever the sidecars stamp"
        );
    }

    #[test]
    fn a_live_tail_reports_its_newest_turn_and_a_stampless_tail_reports_nothing() {
        let live = concat!(
            r#"{"type":"user","timestamp":"2026-08-02T06:04:54.650Z"}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-08-02T06:05:26.261Z"}"#,
            "\n",
            r#"{"type":"system","timestamp":"2026-08-02T06:05:26.613Z"}"#,
            "\n",
        );
        assert_eq!(
            cc_activity_recency(live.as_bytes()),
            TailActivity::At(1_785_650_726)
        );
        assert_eq!(
            cc_activity_recency(br#"{"type":"pr-link","timestamp":"2026-08-02T05:56:43.894Z"}"#),
            TailActivity::SidecarOnly
        );
        for blind in [
            &b""[..],
            br#"not json at all"#,
            br#"{"type":"assistant"}"#,
            br#"{"type":"assistant","timestamp":"not-a-date"}"#,
        ] {
            assert_eq!(
                cc_activity_recency(blind),
                TailActivity::Unknown,
                "{blind:?}"
            );
        }
    }

    #[test]
    fn assistant_line_model_becomes_a_model_info_observation() {
        let v = json!({
            "type": "assistant",
            "message": {"model": "claude-fable-5", "content": []}
        });
        let evs = decode_cc_line("/p/ses-1.jsonl", "claude-code", v);
        assert!(
            evs.iter().any(|e| matches!(e, AgentEvent::ModelInfo { model: Some(m), effort: None, .. } if m == "claude-fable-5")),
            "assistant model must surface, got {evs:?}"
        );
    }

    #[test]
    fn assistant_usage_becomes_a_fresh_token_observation() {
        let v = json!({
            "type": "assistant",
            "message": {"model": "claude-fable-5", "content": [], "usage": {
                "input_tokens": 1200, "cache_creation_input_tokens": 50000,
                "cache_read_input_tokens": 940000, "output_tokens": 5300}}
        });
        let evs = decode_cc_line("/p/ses-1.jsonl", "claude-code", v);
        assert!(
            evs.iter().any(|e| matches!(
                e,
                AgentEvent::Usage {
                    fresh_tokens: 56500,
                    ..
                }
            )),
            "expected fresh=1200+50000+5300 (cache reads excluded), got {evs:?}"
        );
    }

    #[test]
    fn usage_free_and_zero_usage_assistant_lines_stay_silent() {
        let v = json!({"type": "assistant", "message": {"content": []}});
        let evs = decode_cc_line("/p/ses-1.jsonl", "claude-code", v);
        assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
        let v = json!({
            "type": "assistant",
            "message": {"content": [], "usage": {
                "input_tokens": 0, "cache_read_input_tokens": 123456, "output_tokens": 0}}
        });
        let evs = decode_cc_line("/p/ses-1.jsonl", "claude-code", v);
        assert!(
            !evs.iter().any(|e| matches!(e, AgentEvent::Usage { .. })),
            "cache-read-only reading must be silent, got {evs:?}"
        );
    }

    #[test]
    fn synthetic_and_empty_models_are_not_observations() {
        for model in ["<synthetic>", ""] {
            let v = json!({
                "type": "assistant",
                "message": {"model": model, "content": []}
            });
            let evs = decode_cc_line("/p/ses-1.jsonl", "claude-code", v);
            assert!(
                !evs.iter()
                    .any(|e| matches!(e, AgentEvent::ModelInfo { .. })),
                "{model:?} must not emit ModelInfo, got {evs:?}"
            );
        }
    }

    #[test]
    fn ultra_effort_attachments_become_effort_observations() {
        for (kind, label) in [
            ("ultra_effort_enter", "ultra"),
            ("ultrathink_effort", "ultrathink"),
            ("ultra_effort_exit", "ultra_exit"),
        ] {
            let v = json!({
                "type": "attachment",
                "attachment": {"type": kind}
            });
            let evs = decode_cc_line("/p/ses-1.jsonl", "claude-code", v);
            assert!(
                evs.iter().any(|e| matches!(e, AgentEvent::ModelInfo { model: None, effort: Some(f), .. } if f == label)),
                "{kind} must synthesize effort {label:?}, got {evs:?}"
            );
        }
        let v = json!({"type": "attachment", "attachment": {"type": "task_reminder"}});
        let evs = decode_cc_line("/p/ses-1.jsonl", "claude-code", v);
        assert!(
            evs.is_empty(),
            "unrelated attachments are inert, got {evs:?}"
        );
    }

    #[test]
    fn subagent_hooks_with_empty_ids_are_err_not_fallthrough() {
        for event in ["SubagentStart", "SubagentStop"] {
            let no_session = json!({"hook_event_name": event, "agent_id": "abc"});
            assert!(
                decode_cc_hook_custom(test_obj(&no_session)).is_err(),
                "{event} without session_id must Err (claim-fully), not fall through"
            );
            let empty_child = json!({"hook_event_name": event, "session_id": "s", "agent_id": ""});
            assert!(
                decode_cc_hook_custom(test_obj(&empty_child)).is_err(),
                "{event} with empty agent_id must Err — a phantom child never coalesces"
            );
        }
    }

    #[test]
    fn subagent_stop_warns_only_when_stem_and_wire_id_disagree() {
        let capture = |payload: serde_json::Value| {
            crate::test_capture::capture_logs(|| {
                decode_cc_hook_custom(test_obj(&payload))
                    .expect("decodes")
                    .expect("claimed");
            })
        };
        let matched = capture(json!({
            "hook_event_name": "SubagentStop",
            "session_id": "s",
            "agent_id": "abc123",
            "agent_transcript_path": "/p/parent/subagents/agent-abc123.jsonl"
        }));
        assert!(
            !matched.contains("shape_drift"),
            "an agreeing stem must stay silent, got:\n{matched}"
        );
        let drifted = capture(json!({
            "hook_event_name": "SubagentStop",
            "session_id": "s",
            "agent_id": "abc123",
            "agent_transcript_path": "/p/parent/subagents/agent-zzz999.jsonl"
        }));
        assert!(
            drifted.contains("shape_drift") && drifted.contains("agent-zzz999"),
            "a disagreeing stem must fire the drift alarm, got:\n{drifted}"
        );
    }

    #[test]
    fn non_subagent_events_fall_through_to_shared_arms() {
        let start = json!({"hook_event_name": "SessionStart", "session_id": "s"});
        assert!(matches!(decode_cc_hook_custom(test_obj(&start)), Ok(None)));
    }

    #[test]
    fn subagent_stop_keys_on_stem_when_it_disagrees_with_prefixed_wire_id() {
        let evs = decode_cc_hook_custom(test_obj(&json!({
            "hook_event_name": "SubagentStop",
            "session_id": "s",
            "agent_id": "abc",
            "agent_transcript_path": "/p/q/01-deadbeef/subagents/agent-zzz.jsonl"
        })))
        .unwrap()
        .unwrap();
        assert_eq!(evs.len(), 1, "SubagentStop emits exactly one event");
        match &evs[0] {
            AgentEvent::SessionEnd { agent_id, as_child } => {
                assert!(*as_child, "a SubagentStop end is stamped as_child");
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts(SOURCE_NAME, "agent-zzz"),
                    "the transcript STEM agent-zzz must win over the prefixed wire id agent-abc"
                );
                assert_ne!(
                    *agent_id,
                    AgentId::from_parts(SOURCE_NAME, "agent-abc"),
                    "the prefixed wire id must NOT be the key when the stem differs"
                );
            }
            other => panic!("expected SessionEnd, got {other:?}"),
        }
    }

    #[test]
    fn subagent_stop_with_stemless_path_falls_back_to_prefixed_id() {
        let evs = decode_cc_hook_custom(test_obj(&json!({
            "hook_event_name": "SubagentStop",
            "session_id": "s",
            "agent_id": "abc",
            "agent_transcript_path": "/"
        })))
        .unwrap()
        .unwrap();
        assert_eq!(
            evs[0].agent_id(),
            crate::AgentId::from_parts(SOURCE_NAME, "agent-abc")
        );
    }

    #[test]
    fn label_prefers_cwd_basename_when_present() {
        let path = Path::new("/x/.claude/projects/-Users-me-repo/abc.jsonl");
        assert_eq!(
            cc_derive_label(path, "claude-code", Path::new("/Users/me/work/myrepo")),
            "cc·myrepo"
        );
    }

    #[test]
    fn label_falls_back_to_project_dir_when_cwd_empty() {
        let path = Path::new("/Users/me/.claude/projects/-Users-me-dotfiles/abc.jsonl");
        assert_eq!(
            cc_derive_label(path, "claude-code", Path::new("")),
            "cc·dotfiles"
        );
    }

    /// The project-dir fallback BYPASSES the `cwd_basename_label` chokepoint,
    /// so it needs the cap applied at its own site — an uncapped label persists
    /// in `AgentSlot.label` and reaches the painter.
    #[test]
    fn label_project_dir_fallback_is_capped_like_the_cwd_branch() {
        let long = "z".repeat(MAX_DECODED_FIELD_CHARS * 2);
        let path = PathBuf::from(format!("/Users/me/.claude/projects/-{long}/abc.jsonl"));
        let label = cc_derive_label(&path, "claude-code", Path::new(""));
        assert!(
            label.chars().count() < long.chars().count(),
            "project-dir fallback label must be capped, got {} chars: {label:?}",
            label.chars().count()
        );
        let via_cwd = cc_derive_label(
            Path::new("/x/.claude/projects/-p/abc.jsonl"),
            "claude-code",
            &PathBuf::from(format!("/{long}")),
        );
        assert_eq!(
            label, via_cwd,
            "both label branches must cap identically — comparing to the sibling, \
             which caps via the `cwd_basename_label` chokepoint, rather than to a \
             hand-computed width keeps this honest if `ellipsize`'s accounting moves"
        );
    }

    #[test]
    fn label_marks_subagent_paths() {
        let path = Path::new("/x/projects/proj/subagents/agent-1.jsonl");
        assert_eq!(
            cc_derive_label(path, "claude-code", Path::new("/repo")),
            "subagent"
        );
    }

    #[test]
    fn label_does_not_false_positive_on_subagents_in_project_name() {
        let path = Path::new("/Users/me/.claude/projects/-Users-me-subagents-paper/abc.jsonl");
        assert_eq!(
            cc_derive_label(path, "claude-code", Path::new("/Users/me/subagents-paper")),
            "cc·subagents-paper"
        );
    }

    #[test]
    fn label_uses_project_dir_when_cwd_is_root() {
        let path = Path::new("/Users/me/.claude/projects/-Users-me-dotfiles/abc.jsonl");
        assert_eq!(
            cc_derive_label(path, "claude-code", Path::new("/")),
            "cc·dotfiles"
        );
    }

    #[test]
    fn label_uses_project_dir_when_cwd_has_no_basename() {
        // ".." is non-empty and non-root but has no `file_name()`.
        let path = Path::new("/Users/me/.claude/projects/-Users-me-dotfiles/abc.jsonl");
        assert_eq!(
            cc_derive_label(path, "claude-code", Path::new("..")),
            "cc·dotfiles"
        );
    }

    #[test]
    fn label_final_fallback_to_cc_when_no_project_dir() {
        assert_eq!(
            cc_derive_label(Path::new("abc.jsonl"), "claude-code", Path::new("")),
            "cc"
        );
    }

    // CC on Windows slugs `C:\Users\foo\bar` into the project dir name
    // `C--Users-foo-bar` (upstream's `[^a-zA-Z0-9]→'-'`, drive letter kept).
    #[test]
    fn label_falls_back_to_project_dir_for_windows_slug() {
        let path = Path::new("/Users/me/.claude/projects/C--Users-foo-bar/abc.jsonl");
        assert_eq!(
            cc_derive_label(path, "claude-code", Path::new("")),
            "cc·bar"
        );
    }

    // If the per-line decode ever keys differently from the registry row's
    // `id_from_path` deriver, one CC session splits into two sprites.
    #[test]
    fn decode_cc_line_keys_agent_id_on_cc_id_from_path() {
        let path = "/Users/me/.claude/projects/p/01000000-0000-7000-8000-0000000000cc.jsonl";
        let events = decode_cc_line(
            path,
            "claude-code",
            serde_json::json!({"type":"assistant","attributionAgent":"explorer","message":{"content":[]}}),
        );
        let expected =
            AgentId::from_parts("claude-code", &cc_id_from_path(std::path::Path::new(path)));
        assert_eq!(
            events[0].agent_id(),
            expected,
            "decode_cc_line must key its AgentId on cc_id_from_path (the deriver)"
        );
    }

    #[test]
    fn quoted_exit_wrapper_in_user_content_never_ends_the_session() {
        let prose =
            "the transcript shows <command-name>/exit</command-name> as a wrapped line — why?";
        let v = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": prose }
        });
        let events = decode_cc_line("/x/.claude/projects/p/s.jsonl", "claude-code", v);
        assert!(
            events.is_empty(),
            "quoting the wrapper must not emit SessionEnd: {events:?}"
        );

        let tail = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": prose }
        })
        .to_string();
        assert!(
            !cc_session_ended(tail.as_bytes()),
            "tail scan must not end a session on quoted wrapper text"
        );
    }

    #[test]
    fn string_content_turns_emit_no_tool_events() {
        for ty in ["assistant", "user"] {
            let v = serde_json::json!({
                "type": ty,
                "message": { "role": ty, "content": "just some prose, no tool blocks" }
            });
            let out = decode_cc_line("/x/.claude/projects/p/s.jsonl", "claude-code", v);
            assert!(
                out.is_empty(),
                "{ty} turn with string content must emit no events"
            );
        }
        let exit = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": "<command-name>/exit</command-name>" }
        });
        let out = decode_cc_line("/x/.claude/projects/p/s.jsonl", "claude-code", exit);
        assert!(
            out.is_empty(),
            "slash-command content must not emit lifecycle events: {out:?}"
        );
    }

    #[test]
    fn decode_cc_line_non_object_value_decodes_to_nothing() {
        let path = "/x/.claude/projects/p/s.jsonl";
        assert!(
            decode_cc_line(path, "claude-code", serde_json::json!([1, 2, 3])).is_empty(),
            "a bare array line must emit no events"
        );
        assert!(
            decode_cc_line(path, "claude-code", serde_json::json!("raw string line")).is_empty(),
            "a bare string line must emit no events"
        );
    }

    #[test]
    fn no_sidecar_line_breadcrumbs_whether_or_not_we_have_seen_its_type() {
        let path = "/x/.claude/projects/p/s.jsonl";
        let quiet = crate::test_capture::capture_logs(|| {
            for v in [
                json!({"type": "quantum-line", "foo": 1}),
                // #935's instance: shipped by CC and read by nothing here.
                json!({"type": "history-suppression", "cause": "x"}),
                // #959's instances — message-less sidecars seen since CC 2.1.211
                // (atis-latch is in the hermetic recordings' `dropped`; cost-state is
                // corpus-sampled and abridged).
                json!({"type": "atis-latch", "atis": "", "sessionId": "s"}),
                json!({"type": "cost-state", "totalCostUSD": 0.5, "sessionId": "s"}),
                // #1694's: a request message stored by hash since CC 2.1.300,
                // message-shaped but untimestamped.
                json!({"type": "api-request-blob", "hash": "h", "sessionId": "s",
                       "message": {"role": "user", "content": [{"type": "text", "text": "hi"}]}}),
                // TYPELESS but payload-carrying: the `!ty.is_empty()` guard, or
                // the breadcrumb names nothing at all.
                json!({"timestamp": "t", "message": {"role": "assistant", "content": []}}),
                json!({"type": "mode", "mode": "acceptEdits"}),
                json!({"type": "last-prompt", "prompt": "hi"}),
                json!({"type": "worktree-state"}),
                json!({"type": "frame-link", "url": "x"}),
                json!({"type": "pr-link"}),
                json!({"type": "bridge-session"}),
                json!({"type": "system", "subtype": "brand_new_subtype_2027"}),
                // INNER discriminator: the guard reads the top-level `type`.
                json!({"type": "attachment", "attachment": {"type": "brand_new_kind_2027"}}),
            ] {
                assert!(
                    decode_cc_line(path, "claude-code", v).is_empty(),
                    "a sidecar line decodes to no events"
                );
            }
        });
        assert!(
            !quiet.contains(crate::source::drift::TARGET),
            "a line carrying nothing we read reached the drift log, got:\n{quiet}"
        );
    }

    #[test]
    fn tool_use_without_name_emits_tool_use_start_with_question_mark_detail() {
        let out = decode_cc_line(
            "/x/.claude/projects/p/s.jsonl",
            "claude-code",
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu1"}]}}),
        );
        assert_eq!(out.len(), 1, "one tool_use block → one ToolUseStart");
        match &out[0] {
            AgentEvent::ToolUseStart {
                tool_use_id,
                detail,
                ..
            } => {
                assert_eq!(tool_use_id.as_deref(), Some("tu1"));
                assert_eq!(
                    detail.as_ref(),
                    Some(&make_tool_detail(SOURCE_NAME, "?", None)),
                    "a name-less tool_use substitutes the \"?\" fallback name"
                );
            }
            other => panic!("expected ToolUseStart, got {other:?}"),
        }
    }

    #[test]
    fn assistant_content_skips_non_object_and_non_tool_use_blocks() {
        let out = decode_cc_line(
            "/x/.claude/projects/p/s.jsonl",
            "claude-code",
            json!({"type":"assistant","message":{"content":[
                42,
                {"type":"text","text":"hi"},
                {"type":"tool_use","id":"tu","name":"Read","input":{"file_path":"/a"}}
            ]}}),
        );
        assert_eq!(
            out.len(),
            1,
            "only the real tool_use block decodes; the int + text block are skipped: {out:?}"
        );
        match &out[0] {
            AgentEvent::ToolUseStart {
                tool_use_id,
                detail,
                ..
            } => {
                assert_eq!(tool_use_id.as_deref(), Some("tu"));
                assert_eq!(
                    detail.as_ref(),
                    Some(&make_tool_detail(
                        SOURCE_NAME,
                        "Read",
                        Some(&json!({"file_path":"/a"}))
                    )),
                    "the surviving block is the Read tool_use"
                );
            }
            other => panic!("expected ToolUseStart, got {other:?}"),
        }
    }

    #[test]
    fn user_content_skips_non_object_and_non_tool_result_blocks() {
        let out = decode_cc_line(
            "/x/.claude/projects/p/s.jsonl",
            "claude-code",
            json!({"type":"user","message":{"content":[
                "str",
                {"type":"text","text":"hi"},
                {"type":"tool_result","tool_use_id":"tu"}
            ]}}),
        );
        assert_eq!(
            out.len(),
            1,
            "only the real tool_result block decodes; the string + text block are skipped: {out:?}"
        );
        match &out[0] {
            AgentEvent::ToolUseEnd { tool_use_id, .. } => {
                assert_eq!(tool_use_id.as_deref(), Some("tu"));
            }
            other => panic!("expected ToolUseEnd, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod cc_id_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn cc_id_from_path_root_is_filename_uuid() {
        let p = Path::new(
            "/Users/me/.claude/projects/-Users-me-proj/01000000-0000-7000-8000-0000000000cc.jsonl",
        );
        assert_eq!(cc_id_from_path(p), "01000000-0000-7000-8000-0000000000cc");
    }

    #[test]
    fn cc_id_from_path_subagent_is_agent_stem() {
        let p = Path::new(
            "/Users/me/.claude/projects/-Users-me-proj/01000000-0000-7000-8000-0000000000cc/subagents/agent-a0a7dc28dd772bd0d.jsonl",
        );
        assert_eq!(cc_id_from_path(p), "agent-a0a7dc28dd772bd0d");
    }

    #[test]
    fn cc_id_from_path_empty_for_no_stem() {
        assert_eq!(cc_id_from_path(Path::new("")), "");
    }

    #[test]
    fn cc_id_from_path_is_stable_across_path_separators() {
        // The first-sight deriver gets a raw &Path while the per-line decoder
        // gets the `normalize_path_key`'d string — lowercased on Windows.
        let raw =
            Path::new("/Users/me/.claude/projects/p/01000000-0000-7000-8000-0000000000cc.jsonl");
        let normalized =
            Path::new("/users/me/.claude/projects/p/01000000-0000-7000-8000-0000000000cc.jsonl");
        assert_eq!(cc_id_from_path(raw), cc_id_from_path(normalized));
    }

    #[test]
    fn attribution_agent_label_is_capped_at_the_decode_boundary() {
        let path = "/p/x/s.jsonl";
        let long = "é".repeat(MAX_DECODED_FIELD_CHARS * 10);
        let events = decode_cc_line(
            path,
            "claude-code",
            serde_json::json!({"type":"assistant","attributionAgent": long, "message":{"content":[]}}),
        );
        match &events[0] {
            AgentEvent::Rename { label, .. } => {
                assert_eq!(label.chars().count(), MAX_DECODED_FIELD_CHARS + 1);
                assert!(label.ends_with('…'));
            }
            other => panic!("expected Rename, got {other:?}"),
        }

        let events = decode_cc_line(
            path,
            "claude-code",
            serde_json::json!({"type":"assistant","attributionAgent":"explorer","message":{"content":[]}}),
        );
        assert!(
            matches!(&events[0], AgentEvent::Rename { label, .. } if label == "explorer"),
            "a short label must pass through unchanged, got {events:?}"
        );
    }

    #[test]
    fn admits_every_transcript_except_the_orchestrator_journal() {
        let wf = Path::new("/h/.claude/projects/proj/uuid/subagents/workflows/wf_abc");
        assert!(!admits_transcript(&wf.join("journal.jsonl")));
        assert!(admits_transcript(&wf.join("agent-xyz.jsonl")));
        assert!(admits_transcript(Path::new(
            "/h/.claude/projects/proj/uuid.jsonl"
        )));
        assert!(admits_transcript(Path::new(
            "/h/.claude/projects/proj/uuid/subagents/agent-xyz.jsonl"
        )));
    }
}
