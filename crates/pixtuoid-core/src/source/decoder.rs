//! Shared decoder utilities used by per-source decoders. Hook payload decoding
//! lives here because the hook socket is shared; a non-CC-shaped envelope is
//! dispatched out to its own source module before the shared field requirements
//! apply.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::AgentId;
use crate::id::normalize_path_key;
use crate::source::{AgentEvent, RequestKeys, ToolDetail};

/// The JSONL line-decoder fn pointer: `(transcript_path, source, raw_line) ->
/// events`. Defined HERE, NOT in the `native`-gated `jsonl` module, so the
/// registry's `SourceDescriptor` can name it in a `--no-default-features` (wasm)
/// build.
pub type LineDecoder = fn(&str, &str, Value) -> Vec<AgentEvent>;

/// A decoder's answer: its events, or why it refused the input.
pub type DecodeResult<T> = std::result::Result<T, DecodeError>;

/// Why a decoder refused a hook payload. The caller logs it and drops the input;
/// every wire-borne string in it is `display_safe`d.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The payload isn't a JSON object.
    #[non_exhaustive]
    #[error("{cli} payload must be an object")]
    NotAnObject {
        /// The wire's source name.
        cli: String,
    },
    /// A field the input needs is absent or empty.
    #[non_exhaustive]
    #[error("{cli} {} has no {field}", .context.as_deref().unwrap_or("payload"))]
    Missing {
        /// The wire's source name.
        cli: String,
        /// The event or part of the payload the field belongs to, when it
        /// isn't the payload itself.
        context: Option<String>,
        /// The field, or `|`-joined fields any one of which would do.
        field: &'static str,
    },
    /// An event name the decoder has no arm for.
    #[non_exhaustive]
    #[error("unsupported {cli} event: {event}")]
    Unsupported {
        /// The wire's source name.
        cli: String,
        /// The event name.
        event: String,
    },
    /// A field present but outside what the wire allows.
    #[non_exhaustive]
    #[error("{cli} {field}: {reason}")]
    Invalid {
        /// The wire's source name.
        cli: String,
        /// The field.
        field: &'static str,
        /// What was wrong with it.
        reason: String,
    },
}

impl DecodeError {
    pub(crate) fn not_an_object(cli: &str) -> Self {
        Self::NotAnObject {
            cli: display_safe(cli),
        }
    }

    pub(crate) fn missing(cli: &str, field: &'static str) -> Self {
        Self::Missing {
            cli: display_safe(cli),
            context: None,
            field,
        }
    }

    pub(crate) fn missing_in(cli: &str, context: &str, field: &'static str) -> Self {
        Self::Missing {
            cli: display_safe(cli),
            context: Some(display_safe(context)),
            field,
        }
    }

    pub(crate) fn unsupported(cli: &str, event: &str) -> Self {
        Self::Unsupported {
            cli: display_safe(cli),
            event: display_safe(event),
        }
    }

    pub(crate) fn invalid(cli: &str, field: &'static str, reason: impl std::fmt::Display) -> Self {
        Self::Invalid {
            cli: display_safe(cli),
            field,
            reason: display_safe(&reason.to_string()),
        }
    }
}

/// The first-sight cwd-extractor fn pointer: ONE parsed transcript line → the
/// working dir it carries, if any. Each transcript-bearing source's registry row
/// names its own extractor and the JSONL walker's head scan dispatches by the
/// source being scanned, so one source's shape is never tried against another
/// source's transcript (a foreign-shaped line — e.g. a codex-style
/// `payload.cwd` inside a CC transcript — must not label the session with a
/// foreign, identity-bearing cwd).
pub(crate) type CwdExtractor = fn(&Value) -> Option<PathBuf>;

/// The first-sight parent-extractor fn pointer: ONE parsed transcript line → the
/// parent's session key it names, for a transcript whose flat path names none
/// (`detect_parent_id` reads a nested one), on or before its cwd's line.
pub(crate) type ParentExtractor = fn(&Value) -> Option<String>;

/// Derives the opaque session-id string a transcript PATH keys its agent on —
/// what the JSONL watcher's first-sight `SessionStart` carries, and therefore
/// what a hook event must agree with or one session renders as two sprites.
pub type IdDeriver = fn(&Path) -> String;

pub(crate) fn default_id_from_path(p: &Path) -> String {
    normalize_path_key(&p.to_string_lossy())
}

/// Decides which `.jsonl` FILES a source's transcripts are — checked after the
/// extension gate, so it filters files, never directories. A source dir often
/// holds SIBLINGS that must never be walked: grok's rewrite-on-resume
/// `chat_history.jsonl`, Antigravity's duplicate `transcript_full.jsonl`, CC's
/// foreign-schema workflow `journal.jsonl`. The default (`accept_all_paths`)
/// admits every transcript.
pub type PathFilter = fn(&Path) -> bool;

pub(crate) fn accept_all_paths(_p: &Path) -> bool {
    true
}

/// Narrow a raw JSON integer to a valid POSIX pid: `i32` range AND strictly
/// positive — `kill(0)`/`kill(-n)` target process GROUPS, so a bogus or zero `_pid`
/// would synthesize a phantom exit that flaps a LIVE gateway Down. Every JSON pid
/// ingress rides it; `fd_probe` deliberately does not (kernel-enumerated `pid_t`).
/// `i64` in, so a wild value is a per-entry skip, not a whole-document fail (#831).
pub(crate) fn checked_pid(raw: i64) -> Option<i32> {
    i32::try_from(raw).ok().filter(|&p| p > 0)
}

pub(crate) fn extract_top_level_cwd(v: &Value) -> Option<PathBuf> {
    v.get("cwd").and_then(Value::as_str).map(PathBuf::from)
}

/// The directory a CC subagent transcript sits under: `<parent>/subagents/
/// agent-*.jsonl`. Single source of truth for both `is_subagent_path` and the
/// watcher's `detect_parent_id` so they cannot diverge.
pub(crate) const SUBAGENTS_DIR: &str = "subagents";

/// Whether a transcript path is a CC subagent transcript. Matched as a whole
/// path COMPONENT (never a substring) so a project dir merely *containing* the
/// word (e.g. `subagents-paper`) is not mistaken for one, and so Windows
/// backslash-separated paths match too.
pub(crate) fn is_subagent_path(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == SUBAGENTS_DIR)
}

/// `"{prefix}·{basename}"` from a working directory, or `None` when `cwd` has no
/// final component (empty / the filesystem root). The cwd is untrusted
/// transcript/hook content and a slashless crafted value makes the WHOLE string the
/// basename, so the cap lands here — one chokepoint for all three derivers.
pub(crate) fn cwd_basename_label(prefix: &str, cwd: &Path) -> Option<String> {
    let base = cwd.file_name().and_then(|n| n.to_str())?;
    Some(format!(
        "{prefix}·{}",
        ellipsize(base, MAX_DECODED_FIELD_CHARS)
    ))
}

/// The registered 2-char display prefix for `source`, or the raw source name
/// when it has no row — the single authority, so no deriver hardcodes a prefix
/// that could drift from the registry.
pub(crate) fn label_prefix_for(source: &str) -> &str {
    crate::source::registry::descriptor_for(source)
        .map(|d| d.label_prefix)
        .unwrap_or(source)
}

/// `"{prefix}·{basename}"` from a working directory, prefix looked up from the
/// registry by `source`; falls back to the bare prefix when `cwd` has no
/// basename.
#[cfg(any(feature = "native", test))]
pub(crate) fn derive_prefixed_label(source: &str, cwd: &Path) -> String {
    let prefix = label_prefix_for(source);
    cwd_basename_label(prefix, cwd).unwrap_or_else(|| prefix.to_string())
}

/// The first key in `keys` (priority order) whose value on `obj` is a string.
pub(crate) fn first_present_str<'a>(obj: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let m = obj.as_object()?;
    keys.iter().find_map(|k| m.get(*k).and_then(|v| v.as_str()))
}

/// Parse every COMPLETE line of a tail-scan window as JSON, silently dropping
/// empty, torn, and non-JSON lines. The ONE tail-parse scaffold the per-source
/// `*_session_ended` checkers share — each passes only its own STRUCTURAL
/// end-marker predicate, never a substring scan (user-controllable content — a
/// tool result QUOTING the marker — must not drive lifecycle).
pub(crate) fn parsed_tail_lines(tail: &[u8]) -> impl Iterator<Item = Value> + '_ {
    crate::source::admit::lines(tail).filter_map(|line| serde_json::from_str(line.ok()?).ok())
}

/// What a transcript tail says about when its SESSION last wrote — the per-source
/// refinement of the file mtime the first-sight gate used to trust outright. Three
/// states, `non_exhaustive` while unpublished because a fourth is plausible.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailActivity {
    /// Newest agent-activity line in the tail, epoch seconds.
    At(u64),
    /// The tail parses and NONE of it is agent activity — positive evidence the
    /// recent bytes were not the session writing.
    SidecarOnly,
    /// No verdict: no probe supplied, nothing parseable, or activity lines with no
    /// readable stamp. The gate keeps mtime's answer.
    Unknown,
}

/// Minimal RFC3339 → epoch seconds (`YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)`).
/// Core deliberately carries no date dependency for the handful of wire
/// timestamps it reads; every caller treats `None` as "no information".
pub(crate) fn rfc3339_to_epoch_secs(s: &str) -> Option<u64> {
    rfc3339_to_epoch(s).map(|d| d.as_secs())
}

/// [`rfc3339_to_epoch_secs`] with the fraction kept, to the nanosecond.
pub(crate) fn rfc3339_to_epoch(s: &str) -> Option<std::time::Duration> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' && b[10] != b't' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut nanos = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        // The first digit is tenths of a second; one past nanoseconds is cut.
        let mut place = std::time::Duration::from_millis(100).subsec_nanos();
        while let Some(digit) = b.get(i).filter(|c| c.is_ascii_digit()) {
            nanos += u32::from(digit - b'0') * place;
            place /= 10;
            i += 1;
        }
    }
    let offset_secs: i64 = match b.get(i) {
        Some(b'Z') | Some(b'z') if i + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) if i + 6 == b.len() && b.get(i + 3) == Some(&b':') => {
            let oh = num(i + 1..i + 3)?;
            let om = num(i + 4..i + 6)?;
            let mag = oh * 3600 + om * 60;
            if *sign == b'+' { mag } else { -mag }
        }
        _ => return None,
    };
    // Howard Hinnant's `days_from_civil`:
    // https://howardhinnant.github.io/date_algorithms.html#days_from_civil
    let (y_adj, era_m) = if mo <= 2 {
        (y - 1, mo + 9)
    } else {
        (y, mo - 3)
    };
    let era = y_adj.div_euclid(400);
    let yoe = y_adj - era * 400;
    let doy = (153 * era_m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset_secs;
    Some(std::time::Duration::new(u64::try_from(secs).ok()?, nanos))
}

/// The CLI a hook envelope belongs to — read ONLY from the shim-owned
/// `_pixtuoid_source`. CC's public `source` field carries the SessionStart REASON
/// (startup/resume/clear/compact), so reading it would namespace the agent under
/// "startup" and split it from the claude-code-keyed tool/JSONL/SessionEnd events:
/// an un-reapable ghost.
pub(crate) fn hook_source(obj: &serde_json::Map<String, Value>) -> &str {
    obj.get("_pixtuoid_source")
        .and_then(|s| s.as_str())
        .unwrap_or(crate::source::claude_code::SOURCE_NAME)
}

/// A test payload as the object every hook decoder is handed.
#[cfg(test)]
pub(crate) fn test_obj(v: &Value) -> &crate::source::registry::HookObject {
    v.as_object().expect("a test payload is an object")
}

/// Envelopes that decode to NOTHING, quietly. A DAEMON source's payloads ride the
/// sibling presence channel and carry no `hook_event_name` at all. grok scans
/// `~/.claude/settings.json` AND `~/.cursor/hooks.json` BY DEFAULT and fires the
/// shim installed there with its OWN camelCase envelope, so a duplicate arrives
/// tagged `claude-code`/`cursor` beside the grok-tagged one.
fn decodes_to_nothing(
    source: &str,
    obj: &serde_json::Map<String, Value>,
    desc: Option<&'static crate::source::registry::SourceDescriptor>,
) -> bool {
    if desc.is_some_and(|d| d.is_daemon()) {
        return true;
    }
    let cc = source == crate::source::claude_code::SOURCE_NAME;
    // trace, not warn — and scoped to those two targets, so a FUTURE camelCase
    // source falls through and surfaces as an OBSERVED decode error instead.
    if (cc || source == crate::source::cursor::SOURCE_NAME) && obj.contains_key("hookEventName") {
        tracing::trace!(source, "dropping grok cross-fired hook envelope");
        return true;
    }
    // cursor's own unstamped duplicates, counted in `source/cursor.rs`'s module doc.
    if cc && obj.contains_key("cursor_version") && !obj.contains_key("_pixtuoid_source") {
        tracing::trace!("dropping an unstamped cursor hook invocation");
        return true;
    }
    false
}

/// A source's OWN hook arms, run before the shared field requirements so an alien
/// envelope (Reasonix: camelCase, `event` discriminator, no `session_id` at all) or
/// a subject-changing event (a subagent's, whose AgentId is the CHILD's)
/// decodes in its own module. An `Extend` decoder that declines (`Ok(None)`) falls
/// through to the shared CC-shaped arms; a `ClaimsAll` decoder cannot.
fn source_owned_arms(
    desc: Option<&'static crate::source::registry::SourceDescriptor>,
    obj: &crate::source::registry::HookObject,
) -> DecodeResult<Option<Vec<AgentEvent>>> {
    use crate::source::registry::HookDecoding;
    match desc.and_then(|d| d.hook()) {
        Some(HookDecoding::ClaimsAll(decode)) => decode(obj).map(Some),
        Some(HookDecoding::Shared { extend, .. }) => extend(obj),
        None => Ok(None),
    }
}

/// `obj`'s string field `key`, `None` when absent or EMPTY: an empty id or
/// name keys nothing.
pub(crate) fn non_empty<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Whose session a hook speaks for: a subagent names itself in [`SUBAGENT_ID`]
/// beside its root's `session_id`, a root in `session_id` alone.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HookSubject<'a> {
    /// The subagent's own id, when a subagent fired it.
    pub(crate) child: Option<&'a str>,
    /// The root session's id.
    pub(crate) root: Option<&'a str>,
}

impl<'a> HookSubject<'a> {
    pub(crate) fn of(obj: &'a serde_json::Map<String, Value>) -> Self {
        Self {
            child: non_empty(obj, SUBAGENT_ID),
            root: non_empty(obj, "session_id"),
        }
    }
}

/// A hook's `tool_name`, the key a request binds its pending calls by, so the
/// pre-use and the prompt derive it alike; its absence is drift.
pub(crate) fn hook_tool_name(
    obj: &serde_json::Map<String, Value>,
    source: &str,
    event: &str,
) -> Option<String> {
    let name = non_empty(obj, "tool_name").map(|n| ellipsize(n, MAX_DECODED_FIELD_CHARS));
    if name.is_none() {
        super::drift::missing_field(source, event, "tool_name");
    }
    name
}

/// The per-call id's wire name on `desc`'s hooks: its row's
/// [`ToolIdKey`](crate::source::registry::ToolIdKey), `tool_use_id` for a row
/// with no shared arms.
pub(crate) fn hook_tool_id_key(
    desc: Option<&'static crate::source::registry::SourceDescriptor>,
) -> &'static str {
    match desc.and_then(|d| d.hook()) {
        Some(crate::source::registry::HookDecoding::Shared { tool_id_key, .. }) => *tool_id_key,
        _ => crate::source::registry::ToolIdKey::ToolUse,
    }
    .wire_name()
}

/// The payload's `session_id`, rejected when missing or EMPTY: an empty one passes
/// `as_str` but, for Codex (which keys the AgentId on it), would mint a phantom
/// agent that never coalesces with any rollout. This and `hook_event_name`
/// breadcrumb undeduped — the two fields are the whole hook plane's chokepoint.
pub(crate) fn hook_session_id(
    obj: &serde_json::Map<String, Value>,
    source: &str,
    event: &str,
) -> DecodeResult<String> {
    HookSubject::of(obj)
        .root
        .ok_or_else(|| {
            super::drift::missing_field(source, event, "session_id");
            DecodeError::missing_in(source, event, "session_id")
        })
        .map(str::to_string)
}

/// The identity context the tool/permission arms attach ahead of their activity
/// event. `cwd` is on the wire for CC tool hooks but absent on e.g. Codex
/// `PermissionRequest` — absent or empty maps to `None`, so the reducer's cwd-less
/// registration path (ordinal label, reap-exempt) applies.
pub(crate) fn hook_identity(
    agent_id: AgentId,
    source: &str,
    session_id: &str,
    obj: &serde_json::Map<String, Value>,
) -> AgentEvent {
    AgentEvent::identity(
        agent_id,
        source,
        session_id,
        obj.get("cwd")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from),
    )
}

/// Burn-tier effort observation (CC): tool-context hook payloads carry an
/// `effort: {level}` object — hooks.md lists low/medium/high/xhigh/max and notes
/// that ULTRACODE "is not a distinct level and reports as xhigh". Codex hook
/// payloads carry no such field, and absent emits nothing.
fn hook_effort(agent_id: AgentId, obj: &serde_json::Map<String, Value>) -> Option<AgentEvent> {
    obj.get("effort")
        .and_then(|e| e.get("level"))
        .and_then(|l| l.as_str())
        .filter(|l| !l.is_empty())
        .map(|level| AgentEvent::ModelInfo {
            agent_id,
            model: None,
            effort: Some(ellipsize(level, MAX_DECODED_FIELD_CHARS)),
        })
}

/// Where the shim stamps an installed command's `--hook-event`, for a CLI whose
/// payload names no event.
pub const EVENT_STAMP: &str = "_pixtuoid_event";

/// Decode one hook payload into the event sequence the reducer applies.
///
/// The tool/permission arms (PreToolUse / PostToolUse / Notification /
/// PermissionRequest) lead with an [`AgentEvent::Identity`] carrying the payload's
/// source/session_id/cwd (#221), so the reducer's proof-of-life registration for an
/// unknown id lands with REAL identity instead of a blank `#N` slot. The
/// session-lifecycle and custom Subagent arms deliberately do NOT: SessionStart
/// already carries identity, and an end for an unknown agent proves nothing.
///
/// # Errors
///
/// If the payload is not an object, a source-owned decoder rejects it, `hook_event_name` or `session_id` is missing or empty, or the event is unsupported.
pub fn decode_hook_payload(v: Value) -> DecodeResult<Vec<AgentEvent>> {
    decode_hook_object(hook_object(&v)?)
}

/// The one object gate for a hook payload, shared by the listener and [`decode_hook_payload`].
pub(crate) fn hook_object(v: &Value) -> DecodeResult<&crate::source::registry::HookObject> {
    v.as_object()
        .ok_or_else(|| DecodeError::not_an_object("hook"))
}

/// [`decode_hook_payload`] past its object gate.
pub(crate) fn decode_hook_object(
    obj: &crate::source::registry::HookObject,
) -> DecodeResult<Vec<AgentEvent>> {
    let source = hook_source(obj);
    let desc = crate::source::registry::descriptor_for(source);
    if decodes_to_nothing(source, obj, desc) {
        return Ok(vec![]);
    }
    if let Some(evs) = source_owned_arms(desc, obj)? {
        return Ok(evs);
    }
    shared_hook_arms(obj, source, desc)
}

/// The shared CC-shaped arms, for every source whose row has no claiming decoder.
/// `UserPromptSubmit` ALSO emits `SessionStart` (idempotent in the reducer) because
/// Codex's tool hooks fire for only some handlers — ~25 others fire nothing
/// (openai/codex#20204) — and hook firing is version-unstable: a
/// `matcher="*"` group is silently dropped, some builds emit none (openai/codex#21639).
pub(crate) fn shared_hook_arms(
    obj: &serde_json::Map<String, Value>,
    source: &str,
    desc: Option<&'static crate::source::registry::SourceDescriptor>,
) -> DecodeResult<Vec<AgentEvent>> {
    let event = obj
        .get("hook_event_name")
        .and_then(|s| s.as_str())
        .ok_or_else(|| {
            super::drift::missing_field(source, "hook", "hook_event_name");
            DecodeError::missing(source, "hook_event_name")
        })?;
    let session_id = hook_session_id(obj, source, event)?;
    let agent_id = AgentId::from_parts(source, &session_id);
    let tool_id_key = hook_tool_id_key(desc);

    match event {
        "SessionStart" => {
            let cwd = obj.get("cwd").and_then(|s| s.as_str()).unwrap_or("").into();
            let mut evs = vec![AgentEvent::SessionStart {
                agent_id,
                source: source.to_string(),
                session_id,
                cwd,
                parent_id: None,
            }];
            // "Only SessionStart hooks can receive a `model` field, and it is not
            // guaranteed to be present" (hooks.md) — take it when offered.
            if let Some(model) = obj
                .get("model")
                .and_then(|m| m.as_str())
                .filter(|m| !m.is_empty())
            {
                evs.push(AgentEvent::ModelInfo {
                    agent_id,
                    model: Some(ellipsize(model, MAX_DECODED_FIELD_CHARS)),
                    effort: None,
                });
            }
            Ok(evs)
        }
        "PreToolUse" => {
            let tool_name = hook_tool_name(obj, source, event);
            let tool_use_id = non_empty(obj, tool_id_key).map(String::from);
            // Named ahead of its item: a permission prompt names only the
            // tool, so the request binds the calls pending under that name.
            let pending = tool_use_id.clone().map(|call| AgentEvent::ToolUsePending {
                agent_id,
                tool_use_id: call,
                tool_name: tool_name.clone(),
                turn: None,
            });
            let mut evs = vec![hook_identity(agent_id, source, &session_id, obj)];
            evs.extend(pending);
            evs.push(AgentEvent::ToolUseStart {
                agent_id,
                tool_use_id,
                detail: Some(make_tool_detail(
                    source,
                    tool_name.as_deref().unwrap_or("?"),
                    obj.get("tool_input"),
                )),
            });
            evs.extend(hook_effort(agent_id, obj));
            Ok(evs)
        }
        "PostToolUse" => {
            let tool_use_id = non_empty(obj, tool_id_key).map(String::from);
            let mut evs = vec![
                hook_identity(agent_id, source, &session_id, obj),
                AgentEvent::ToolUseEnd {
                    agent_id,
                    tool_use_id,
                },
            ];
            evs.extend(hook_effort(agent_id, obj));
            Ok(evs)
        }
        "Notification" => {
            let msg = obj.get("message").and_then(|s| s.as_str());
            Ok(vec![
                hook_identity(agent_id, source, &session_id, obj),
                AgentEvent::user_action_request(
                    agent_id,
                    msg.map(|m| ellipsize(m, MAX_DECODED_FIELD_CHARS)),
                    RequestKeys::default(),
                ),
            ])
        }
        // The prompt names its tool, and kimi's its call too; an empty name
        // binds nothing.
        "PermissionRequest" => Ok(vec![
            hook_identity(agent_id, source, &session_id, obj),
            AgentEvent::user_action_request(
                agent_id,
                Some("permission".into()),
                RequestKeys {
                    tool_use_id: non_empty(obj, tool_id_key).map(String::from),
                    tool_name: hook_tool_name(obj, source, event),
                    ..RequestKeys::default()
                },
            ),
        ]),
        // The JSONL rollout stays the system of record for Codex tool activity.
        "UserPromptSubmit" => {
            let cwd = obj.get("cwd").and_then(|s| s.as_str()).unwrap_or("").into();
            Ok(vec![AgentEvent::SessionStart {
                agent_id,
                source: source.to_string(),
                session_id,
                cwd,
                parent_id: None,
            }])
        }
        // Stop must NOT end the session: turns end many times per session.
        "Stop" => Ok(vec![AgentEvent::turn_end(agent_id)]),
        "SessionEnd" => Ok(vec![AgentEvent::SessionEnd {
            agent_id,
            as_child: false,
        }]),
        // SubagentStart/Stop change the event's SUBJECT to the child AgentId, which
        // these session-keyed arms cannot express — see `source_owned_arms`.
        other => {
            super::drift::unknown_event(source, other);
            Err(DecodeError::unsupported(source, other))
        }
    }
}

/// The hook field naming the subagent that fired an event.
pub(crate) const SUBAGENT_ID: &str = "agent_id";

/// An event a subagent fired, which names it in `agent_id` beside its root's
/// `session_id`, decoded as the subagent's own session: `session` maps the
/// wire's `agent_id` to that session's id, and a start it decodes to is
/// parented on the root. An event naming no subagent declines (`Ok(None)`).
pub(crate) fn subagent_fired(
    obj: &serde_json::Map<String, Value>,
    source: &str,
    session: fn(&str) -> String,
) -> DecodeResult<Option<Vec<AgentEvent>>> {
    let subject = HookSubject::of(obj);
    let Some(agent) = subject.child else {
        return Ok(None);
    };
    let desc = crate::source::registry::descriptor_for(source);
    let root = subject.root.map(|root| AgentId::from_parts(source, root));
    let mut own = obj.clone();
    own.insert("session_id".into(), Value::from(session(agent)));
    let mut evs = shared_hook_arms(&own, source, desc)?;
    for e in &mut evs {
        if let AgentEvent::SessionStart { parent_id, .. } = e {
            *parent_id = parent_id.or(root);
        }
    }
    Ok(Some(evs))
}

/// The dispatch tool's current NAME, renamed from `Task` in CC v2.1.63 with no
/// announcement. `Workflow` (CC's fleet dispatcher) is DELIBERATELY not a second
/// entry: its children fire no per-agent tool_use, so one months-long `tasks_in_flight`
/// entry would sweep-EXEMPT every FINISHED fleet subagent until the workflow ends —
/// worse desk starvation than the gap. Fleet lifecycle rides the Subagent hooks.
const DISPATCH_TOOL: &str = "Agent";

/// The detail for one tool call. The subagent dispatch is detected SEMANTICALLY, by
/// the PRESENCE of a `subagent_type` input field — presence, not value, so a renamed
/// tool emitting `subagent_type: null` is still caught AND still breadcrumbs the
/// drift. The reducer keys Delegating and an unnamed child's suppression
/// (`tasks_in_flight`) and Task-drain completion on `is_task()`, so a missed dispatch
/// silently disables them.
pub(crate) fn make_tool_detail(source: &str, tool_name: &str, input: Option<&Value>) -> ToolDetail {
    let has_subagent_type = input.and_then(|v| v.get("subagent_type")).is_some();
    let known_name = tool_name == DISPATCH_TOOL;
    if has_subagent_type || known_name {
        if has_subagent_type && !known_name {
            super::drift::unknown_dispatch(source, tool_name);
        }
        ToolDetail::Task
    } else {
        generic_tool_display(tool_name, describe_tool_target(tool_name, input))
    }
}

/// The format-agnostic Generic-tool fallback display, shared by every source's
/// `*_tool_detail` so the cap policy can't drift between them. `tool` is capped
/// at [`MAX_DECODED_FIELD_CHARS`], `target` at [`MAX_TOOL_TARGET_CHARS`] and
/// rendered as a `: …` suffix.
pub(crate) fn generic_tool_display(tool: &str, target: Option<&str>) -> ToolDetail {
    let suffix = target
        .map(|t| format!(": {}", ellipsize(t, MAX_TOOL_TARGET_CHARS)))
        .unwrap_or_default();
    ToolDetail::Generic {
        display: format!("{}{suffix}", ellipsize(tool, MAX_DECODED_FIELD_CHARS)),
    }
}

/// The non-CC "scan a key list, then assemble" last mile. Bundling the scan and
/// the cap in ONE call means a source's Generic fallback cannot scan a target and
/// format it raw, bypassing [`MAX_TOOL_TARGET_CHARS`]. The per-source `keys`
/// vocabulary is passed IN as data, so this stays format-agnostic.
pub(crate) fn generic_keyed_detail(tool: &str, args: Option<&Value>, keys: &[&str]) -> ToolDetail {
    generic_tool_display(tool, args.and_then(|a| first_present_str(a, keys)))
}

/// CC's per-tool target key dispatch: the RAW `file/cmd` descriptor for the
/// Generic display, or `None` for a tool with no keyed target. The cap + `: …`
/// formatting is applied by [`generic_tool_display`].
pub(crate) fn describe_tool_target<'a>(tool: &str, input: Option<&'a Value>) -> Option<&'a str> {
    let key = match tool {
        "Write" | "Edit" | "MultiEdit" | "Read" => "file_path",
        "Bash" => "command",
        "Grep" | "Glob" => "pattern",
        _ => return None,
    };
    input?.get(key).and_then(|v| v.as_str())
}

/// Tighter cap for the tool-target descriptor (the `: file/cmd` suffix on a
/// Generic tool display) — a glanceable fragment, not a full field.
pub(crate) const MAX_TOOL_TARGET_CHARS: usize = 40;

/// Cap for content-derived strings that become slot state ([`AgentEvent::UserActionRequest`] reason,
/// Rename label) — generous against every legitimate value, tight against a
/// crafted ~1 MiB hook/transcript line: every TUI display site is individually
/// bounded, but the headless summary line is not, and the uncapped value would
/// sit in `AgentSlot` for the session's lifetime either way.
#[doc(hidden)]
pub const MAX_DECODED_FIELD_CHARS: usize = 80;

/// What `ellipsize` ends a capped value with.
#[doc(hidden)]
pub const ELLIPSIS: char = '\u{2026}';

/// Make an untrusted wire value safe to DISPLAY: strip control characters, then cap
/// at [`MAX_DECODED_FIELD_CHARS`]. For the HUMAN sinks that are NOT cell buffers —
/// the [`super::drift`] breadcrumbs and [`DecodeError`]'s wire strings, which
/// reach stderr under whatever `tracing` subscriber the host installs. The
/// binary's `strip_control_chars` is a COPY — keep the two in step.
pub(crate) fn display_safe(s: &str) -> String {
    let stripped: String = s
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .collect();
    ellipsize(&stripped, MAX_DECODED_FIELD_CHARS)
}

/// The Unicode Bidi_Control characters — category Cf, so `char::is_control` (Cc
/// only) misses them while they REORDER displayed text in a terminal: exactly the
/// gap the "Trojan Source" class (CVE-2021-42574) rides, where a value renders
/// differently from its bytes. In range order: ALM; LRM, RLM; LRE, RLE, PDF, LRO,
/// RLO; LRI, RLI, FSI, PDI.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200E}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
    )
}

pub(crate) fn ellipsize(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push(ELLIPSIS);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every wire string a `DecodeError` carries is [`display_safe`]d, so a
    /// raw-stderr log of one can't carry a terminal escape.
    #[test]
    fn a_decode_error_carries_no_wire_escape() {
        let hostile = "x\u{1b}]0;pwned\u{7}\u{202e}y";
        for e in [
            DecodeError::not_an_object(hostile),
            DecodeError::missing(hostile, "cwd"),
            DecodeError::missing_in(hostile, hostile, "cwd"),
            DecodeError::unsupported(hostile, hostile),
            DecodeError::invalid(hostile, "port", hostile),
        ] {
            let shown = e.to_string();
            assert!(
                !shown.chars().any(|c| c.is_control() || c == '\u{202e}'),
                "{shown:?}"
            );
        }
        assert_eq!(
            DecodeError::missing("cursor", "cwd").to_string(),
            "cursor payload has no cwd"
        );
    }

    #[test]
    fn rfc3339_parses_chrono_utc_shapes_and_rejects_garbage() {
        assert_eq!(
            rfc3339_to_epoch_secs("2026-07-16T12:00:05Z"),
            Some(1_784_203_205)
        );
        assert_eq!(
            rfc3339_to_epoch_secs("2026-07-16T12:00:05.123456+00:00"),
            Some(1_784_203_205)
        );
        assert_eq!(
            rfc3339_to_epoch_secs("2026-07-16T14:00:05+02:00"),
            Some(1_784_203_205)
        );
        assert_eq!(rfc3339_to_epoch_secs("1970-01-01T00:00:00Z"), Some(0));
        for bad in ["", "2026-07-16", "not a date", "2026-13-01T00:00:00Z"] {
            assert_eq!(rfc3339_to_epoch_secs(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn rfc3339_to_epoch_keeps_the_fraction_the_seconds_drop() {
        use std::time::Duration;
        assert_eq!(
            rfc3339_to_epoch("2026-07-16T12:00:05.123456+00:00"),
            Some(Duration::new(1_784_203_205, 123_456_000))
        );
        assert_eq!(
            rfc3339_to_epoch("2026-07-16T12:00:05.1234567891Z"),
            Some(Duration::new(1_784_203_205, 123_456_789)),
            "a digit past the nanosecond is cut"
        );
        assert_eq!(
            rfc3339_to_epoch("2026-07-16T12:00:05Z"),
            Some(Duration::from_secs(1_784_203_205))
        );
    }

    #[test]
    fn rfc3339_shape_gate_rejects_each_separator_independently() {
        for bad in [
            "2026x07-16T12:00:05Z",
            "2026-07x16T12:00:05Z",
            "2026-07-16 12:00:05Z",
        ] {
            assert_eq!(rfc3339_to_epoch_secs(bad), None, "{bad:?}");
        }
        // A lower-case `t` is the OTHER accepted separator, and grok's
        // `normalize_path_key`'d stamps arrive lowercased.
        assert_eq!(
            rfc3339_to_epoch_secs("2026-07-16t12:00:05Z"),
            Some(1_784_203_205)
        );
    }

    #[test]
    fn rfc3339_field_ranges_admit_the_last_valid_value_and_stop_at_the_next() {
        for (ok, bad) in [
            ("2026-07-31T12:00:05Z", "2026-07-32T12:00:05Z"),
            ("2026-07-16T23:00:05Z", "2026-07-16T24:00:05Z"),
            ("2026-07-16T12:59:05Z", "2026-07-16T12:60:05Z"),
            // 60 is the LEAP second, still in range; 61 never is.
            ("2026-07-16T12:00:60Z", "2026-07-16T12:00:61Z"),
        ] {
            assert!(rfc3339_to_epoch_secs(ok).is_some(), "{ok:?} must parse");
            assert_eq!(rfc3339_to_epoch_secs(bad), None, "{bad:?} must not");
        }
        assert_eq!(rfc3339_to_epoch_secs("2026-07-00T12:00:05Z"), None);
    }

    #[test]
    fn rfc3339_offset_grammar_is_exact_and_the_sign_moves_the_instant() {
        for bad in [
            "2026-07-16T12:00:05Zjunk",
            "2026-07-16T12:00:05+02-00",
            "2026-07-16T12:00:05+02:00extra",
            "2026-07-16T12:00:05+0200",
        ] {
            assert_eq!(rfc3339_to_epoch_secs(bad), None, "{bad:?}");
        }
        // Same instant three ways: the sign inverts, and the offset MINUTES
        // carry weight (the `+05:30` class real wire stamps actually use).
        let utc = rfc3339_to_epoch_secs("2026-07-16T12:00:05Z").unwrap();
        assert_eq!(
            rfc3339_to_epoch_secs("2026-07-16T09:00:05-03:00"),
            Some(utc)
        );
        assert_eq!(
            rfc3339_to_epoch_secs("2026-07-16T17:30:05+05:30"),
            Some(utc)
        );
    }

    #[test]
    fn parsed_tail_lines_yields_only_complete_parseable_lines() {
        // A tail byte-window can begin mid-line (torn leading partial), carry
        // CRLF terminators, and hold empty/torn segments.
        let tail = b"3,\"torn\":tru\n{\"type\":\"a\"}\n{\"type\":\"b\"}\r\n\n{\"type\":\"c\"";
        let kinds: Vec<String> = parsed_tail_lines(tail)
            .filter_map(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned))
            .collect();
        assert_eq!(kinds, vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(
            parsed_tail_lines(b"").count(),
            0,
            "empty tail yields nothing"
        );
        assert_eq!(
            parsed_tail_lines(b"not json at all\n").count(),
            0,
            "non-JSON lines are skipped, never panic",
        );
    }

    #[test]
    fn tool_hooks_surface_the_effort_level() {
        for event in ["PreToolUse", "PostToolUse"] {
            let v = serde_json::json!({
                "hook_event_name": event,
                "session_id": "ses-e",
                "transcript_path": "/p/ses-e.jsonl",
                "cwd": "/repo",
                "tool_name": "Bash",
                "tool_use_id": "t1",
                "effort": {"level": "xhigh"}
            });
            let evs = decode_hook_payload(v).unwrap();
            assert!(
                evs.iter().any(|e| matches!(e, AgentEvent::ModelInfo { model: None, effort: Some(f), .. } if f == "xhigh")),
                "{event} must surface effort, got {evs:?}"
            );
        }
        let v = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "ses-e",
            "transcript_path": "/p/ses-e.jsonl",
            "tool_name": "Bash"
        });
        let evs = decode_hook_payload(v).unwrap();
        assert!(
            !evs.iter()
                .any(|e| matches!(e, AgentEvent::ModelInfo { .. })),
            "got {evs:?}"
        );
    }

    #[test]
    fn session_start_hook_surfaces_the_model_when_offered() {
        let v = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "ses-m",
            "transcript_path": "/p/ses-m.jsonl",
            "cwd": "/repo",
            "model": "claude-fable-5"
        });
        let evs = decode_hook_payload(v).unwrap();
        assert!(
            evs.iter().any(|e| matches!(e, AgentEvent::ModelInfo { model: Some(m), effort: None, .. } if m == "claude-fable-5")),
            "got {evs:?}"
        );
        let v = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "ses-m",
            "transcript_path": "/p/ses-m.jsonl",
            "cwd": "/repo"
        });
        let evs = decode_hook_payload(v).unwrap();
        assert!(
            !evs.iter()
                .any(|e| matches!(e, AgentEvent::ModelInfo { .. })),
            "got {evs:?}"
        );
    }
    use serde_json::json;

    /// The dispatch name is only a FALLBACK: `subagent_type` presence detects the
    /// dispatch on its own, so a tool outside the name still becomes a Task when
    /// it carries the field.
    #[test]
    fn the_dispatch_name_is_a_fallback_to_subagent_type_presence() {
        assert!(matches!(
            make_tool_detail("claude-code", DISPATCH_TOOL, None),
            ToolDetail::Task
        ));
        assert!(!matches!(
            make_tool_detail("claude-code", "Bash", None),
            ToolDetail::Task
        ));
        assert!(matches!(
            make_tool_detail(
                "claude-code",
                "Renamed",
                Some(&json!({"subagent_type": null}))
            ),
            ToolDetail::Task
        ));
    }

    #[test]
    fn task_prefixed_tools_without_subagent_type_are_not_the_dispatch() {
        for name in [
            "TaskCreate",
            "TaskUpdate",
            "TaskList",
            "TaskStop",
            "TaskOutput",
        ] {
            assert!(
                !make_tool_detail("test", name, Some(&json!({"id": "t-1"}))).is_task(),
                "{name} (no subagent_type) must be a Generic tool, not the subagent dispatch"
            );
        }
        assert!(!make_tool_detail("test", "Task", None).is_task());
        assert!(make_tool_detail("test", "Agent", None).is_task());
        assert!(
            make_tool_detail("test", "Task", Some(&json!({"subagent_type": "x"}))).is_task(),
            "a legacy-named dispatch is still caught by the subagent_type field"
        );
        assert!(
            make_tool_detail(
                "test",
                "WhateverUpstreamRenamesItTo",
                Some(&json!({"subagent_type": "x"}))
            )
            .is_task(),
            "a renamed dispatch is still caught by the subagent_type field"
        );
    }

    fn decode_single(v: Value) -> AgentEvent {
        let mut evs = decode_hook_payload(v).expect("decodes");
        assert_eq!(evs.len(), 1, "expected exactly one event, got {evs:?}");
        evs.pop().expect("one event")
    }

    #[test]
    fn a_permission_request_names_its_tool_and_kimis_its_call() {
        for (source, extra, call) in [
            ("claude-code", json!({}), None),
            ("kimi", json!({"tool_call_id": "call_k"}), Some("call_k")),
            ("kimi", json!({"tool_call_id": ""}), None),
        ] {
            let mut v = json!({
                "hook_event_name": "PermissionRequest", "session_id": "s",
                "tool_name": "Bash", "tool_input": {"command": "ls"},
                "_pixtuoid_source": source,
            });
            v.as_object_mut()
                .expect("an object")
                .extend(extra.as_object().expect("an object").clone());
            let evs = decode_hook_payload(v).expect("decodes");
            assert!(
                matches!(&evs[..], [AgentEvent::Identity { .. }, AgentEvent::UserActionRequest {
                    tool_use_id, tool_name: Some(n), turn: None, .. }]
                    if tool_use_id.as_deref() == call && n == "Bash"),
                "{source}: {evs:?}"
            );
        }
    }

    #[test]
    fn codex_permission_request_maps_to_identity_plus_user_action_request() {
        // The captured Codex shape carries no cwd.
        let evs = decode_hook_payload(json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "s",
            "_pixtuoid_source": "codex"
        }))
        .expect("decodes");
        assert_eq!(evs.len(), 2, "Identity + UserActionRequest, got {evs:?}");
        match &evs[0] {
            AgentEvent::Identity {
                source,
                session_id,
                cwd,
                ..
            } => {
                assert_eq!(source, "codex");
                assert_eq!(session_id, "s");
                assert_eq!(*cwd, None, "no cwd on the wire → None");
            }
            other => panic!("expected leading Identity, got {other:?}"),
        }
        assert!(matches!(evs[1], AgentEvent::UserActionRequest { .. }));
    }

    #[test]
    fn a_shared_notification_without_a_message_carries_no_reason() {
        let evs = decode_hook_payload(json!({
            "hook_event_name": "Notification",
            "session_id": "s",
            "cwd": "/r",
            "_pixtuoid_source": "kimi"
        }))
        .expect("decodes");
        assert!(
            matches!(
                evs.as_slice(),
                [
                    AgentEvent::Identity { .. },
                    AgentEvent::UserActionRequest { reason: None, .. }
                ]
            ),
            "{evs:?}"
        );
    }

    #[test]
    fn post_tool_use_without_cwd_decodes_to_identity_with_none_cwd() {
        // Real CC PostToolUse payloads can omit cwd.
        let evs = decode_hook_payload(json!({
            "hook_event_name": "PostToolUse",
            "session_id": "ses-abc",
            "transcript_path": "/p/ses-abc.jsonl",
            "tool_name": "Bash",
            "tool_use_id": "t1"
        }))
        .expect("decodes");
        assert_eq!(evs.len(), 2, "Identity + ToolUseEnd, got {evs:?}");
        match &evs[0] {
            AgentEvent::Identity {
                source,
                session_id,
                cwd,
                ..
            } => {
                assert_eq!(source, crate::source::claude_code::SOURCE_NAME);
                assert_eq!(session_id, "ses-abc");
                assert_eq!(*cwd, None, "absent cwd must map to None");
            }
            other => panic!("expected leading Identity, got {other:?}"),
        }
        assert!(matches!(evs[1], AgentEvent::ToolUseEnd { .. }));
    }

    #[test]
    fn empty_cwd_on_tool_hook_decodes_to_identity_with_none_cwd() {
        let evs = decode_hook_payload(json!({
            "hook_event_name": "Notification",
            "session_id": "ses-abc",
            "transcript_path": "/p/ses-abc.jsonl",
            "cwd": "",
            "message": "permission?"
        }))
        .expect("decodes");
        match &evs[0] {
            AgentEvent::Identity { cwd, .. } => {
                assert_eq!(*cwd, None, "empty cwd must map to None, not Some(\"\")");
            }
            other => panic!("expected leading Identity, got {other:?}"),
        }
    }

    #[test]
    fn cc_session_start_reason_source_does_not_hijack_cli_source() {
        let ev = decode_single(json!({
            "hook_event_name": "SessionStart",
            "session_id": "ses-abc",
            "transcript_path": "/Users/me/.claude/projects/x/ses-abc.jsonl",
            "cwd": "/repo",
            "source": "startup"
        }));
        match ev {
            AgentEvent::SessionStart {
                agent_id, source, ..
            } => {
                assert_eq!(source, crate::source::claude_code::SOURCE_NAME);
                assert_eq!(
                    agent_id,
                    AgentId::from_parts(crate::source::claude_code::SOURCE_NAME, "ses-abc"),
                    "must coalesce with tool/JSONL/SessionEnd events on the claude-code id"
                );
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn subagent_hooks_from_sources_without_a_custom_decoder_bail() {
        for event in ["SubagentStart", "SubagentStop"] {
            let ev = decode_hook_payload(json!({
                "hook_event_name": event,
                "session_id": "s",
                "agent_id": "child",
                "cwd": "/repo",
                "_pixtuoid_source": "some-future-cli"
            }));
            assert!(ev.is_err(), "an unregistered source's {event} must bail");
        }
    }

    #[test]
    fn unknown_reasonix_event_errs_end_to_end_not_falls_through() {
        let ev = decode_hook_payload(json!({
            "_pixtuoid_source": "reasonix",
            "event": "PreCompact",
            "cwd": "/repo",
            "sessionId": "s"
        }));
        let msg = ev.expect_err("unknown rx event must bail").to_string();
        assert!(
            msg.contains("reasonix") && msg.contains("PreCompact") && !msg.contains("sessionId"),
            "error must be the rx decoder's unknown-event bail, got: {msg}"
        );
    }

    #[test]
    fn unknown_source_decodes_cc_shaped_under_its_own_namespace() {
        let ev = decode_single(json!({
            "hook_event_name": "Stop",
            "session_id": "s-1",
            "_pixtuoid_source": "some-future-cli"
        }));
        assert_eq!(
            ev.agent_id(),
            AgentId::from_parts("some-future-cli", "s-1"),
            "unknown source keys under its own namespace, not claude-code's"
        );
    }

    #[test]
    fn absent_source_still_defaults_to_claude() {
        let ev = decode_single(json!({
            "hook_event_name": "SessionStart",
            "session_id": "s",
            "transcript_path": "/p/a.jsonl",
            "cwd": "/repo"
        }));
        match ev {
            AgentEvent::SessionStart { source, .. } => {
                assert_eq!(source, crate::source::claude_code::SOURCE_NAME)
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
    }

    #[test]
    fn ellipsize_caps_on_chars_only_past_the_limit() {
        // Multi-byte chars, so a byte-slicing regression panics or garbles.
        let at = "é".repeat(MAX_DECODED_FIELD_CHARS);
        assert_eq!(ellipsize(&at, MAX_DECODED_FIELD_CHARS), at);
        let over = "é".repeat(MAX_DECODED_FIELD_CHARS + 1);
        let capped = ellipsize(&over, MAX_DECODED_FIELD_CHARS);
        assert_eq!(capped.chars().count(), MAX_DECODED_FIELD_CHARS + 1);
        assert!(capped.ends_with('…'), "cap must be marked: {capped:?}");
    }

    #[test]
    fn cwd_basename_label_caps_a_content_derived_basename() {
        let long = "é".repeat(MAX_DECODED_FIELD_CHARS * 10);
        let label = cwd_basename_label("cc", Path::new(&long)).expect("a basename exists");
        assert_eq!(
            label.chars().count(),
            "cc·".chars().count() + MAX_DECODED_FIELD_CHARS + 1
        );
        assert!(label.ends_with('…'));
        assert_eq!(
            cwd_basename_label("cc", Path::new("/repo/app")),
            Some("cc·app".to_string())
        );
    }

    #[test]
    fn cwd_basename_label_is_none_for_empty_and_root() {
        assert_eq!(cwd_basename_label("cc", Path::new("")), None);
        assert_eq!(cwd_basename_label("cc", Path::new("/")), None);
    }

    #[test]
    fn transcript_deriver_empty_cwd_fallback_equals_registry_prefix() {
        use crate::source::{claude_code, omp, registry};
        // `line_decoder().is_some()` == transcript-bearing == has a LabelDeriver.
        // A source with a CUSTOM deriver must be routed through it here, or the
        // empty-cwd floor it actually ships goes unchecked.
        for d in registry::REGISTRY
            .iter()
            .filter(|d| d.line_decoder().is_some())
        {
            let got = if d.name == claude_code::SOURCE_NAME {
                claude_code::cc_derive_label(Path::new(""), d.name, Path::new(""))
            } else if d.name == omp::SOURCE_NAME {
                omp::omp_derive_label(Path::new(""), d.name, Path::new(""))
            } else {
                derive_prefixed_label(d.name, Path::new(""))
            };
            assert_eq!(
                got, d.label_prefix,
                "{} deriver empty-cwd fallback must equal its registry prefix",
                d.name
            );
        }
        assert_eq!(
            derive_prefixed_label("codex", Path::new("/Users/me/dotfiles")),
            "cx·dotfiles"
        );
    }

    #[test]
    fn describe_tool_target_keys_each_cc_tool_family() {
        for tool in ["Write", "Edit", "MultiEdit", "Read"] {
            assert_eq!(
                describe_tool_target(tool, Some(&json!({"file_path": "/a/b.rs"}))),
                Some("/a/b.rs"),
                "{tool} must key on file_path"
            );
        }
        assert_eq!(
            describe_tool_target("Bash", Some(&json!({"command": "ls"}))),
            Some("ls")
        );
        assert_eq!(
            describe_tool_target("Grep", Some(&json!({"pattern": "fn "}))),
            Some("fn ")
        );
        assert_eq!(
            describe_tool_target("WebFetch", Some(&json!({"url": "u"}))),
            None
        );
    }

    use crate::test_capture::capture_logs;

    #[test]
    fn unknown_dispatch_breadcrumb_fires_only_for_a_renamed_dispatch() {
        let renamed = capture_logs(|| {
            let d = make_tool_detail(
                "claude-code",
                "DelegateZ",
                Some(&json!({"subagent_type": "explorer"})),
            );
            assert!(d.is_task());
        });
        assert!(
            renamed.contains("unknown_dispatch") && renamed.contains("DelegateZ"),
            "a dispatch under an unrecognised name must leave the drift breadcrumb, got:\n{renamed}"
        );
        let known = capture_logs(|| {
            let d = make_tool_detail(
                "claude-code",
                "Agent",
                Some(&json!({"subagent_type": "explorer"})),
            );
            assert!(d.is_task());
        });
        assert!(
            !known.contains("unknown_dispatch"),
            "the known dispatch name must stay breadcrumb-silent, got:\n{known}"
        );
    }

    #[test]
    fn the_hook_planes_required_field_bails_leave_drift_breadcrumbs() {
        let renamed_event = capture_logs(|| {
            assert!(
                decode_hook_payload(json!({
                    "hookEventNameZ": "Stop",
                    "session_id": "ses-1",
                    "_pixtuoid_source": "claude-code",
                }))
                .is_err()
            );
        });
        for needle in [
            crate::source::drift::TARGET,
            "missing_field",
            "hook_event_name",
        ] {
            assert!(
                renamed_event.contains(needle),
                "missing {needle:?} in captured log:\n{renamed_event}"
            );
        }

        let renamed_session = capture_logs(|| {
            assert!(
                decode_hook_payload(json!({
                    "hook_event_name": "Stop",
                    "sessionIdZ": "ses-1",
                    "_pixtuoid_source": "claude-code",
                }))
                .is_err()
            );
        });
        for needle in [crate::source::drift::TARGET, "missing_field", "session_id"] {
            assert!(
                renamed_session.contains(needle),
                "missing {needle:?} in captured log:\n{renamed_session}"
            );
        }

        let cross_fire = capture_logs(|| {
            assert!(
                decode_hook_payload(json!({
                    "hookEventName": "pre_tool_use",
                    "sessionId": "ses-1",
                    "_pixtuoid_source": "claude-code",
                }))
                .is_ok()
            );
        });
        assert!(
            !cross_fire.contains("missing_field"),
            "the grok cross-fire drop must stay breadcrumb-silent, got:\n{cross_fire}"
        );
    }

    #[test]
    fn a_permission_request_naming_no_tool_leaves_a_drift_breadcrumb() {
        use crate::source::drift::{DriftKind, KIND_FIELD, SOURCE_FIELD, TARGET};
        for (source, extra) in [
            ("claude-code", json!({})),
            ("codex", json!({"turn_id": "t"})),
        ] {
            for name in [None, Some("Bash")] {
                let mut v = json!({
                    "hook_event_name": "PermissionRequest", "session_id": "s",
                    "_pixtuoid_source": source,
                });
                let obj = v.as_object_mut().expect("an object");
                obj.extend(extra.as_object().expect("an object").clone());
                if let Some(n) = name {
                    obj.insert("tool_name".into(), json!(n));
                }
                let logs = capture_logs(|| {
                    decode_hook_payload(v).expect("decodes");
                });
                if name.is_some() {
                    assert!(!logs.contains(TARGET), "{source}: {logs}");
                    continue;
                }
                let line = format!(
                    "{SOURCE_FIELD}={source} {KIND_FIELD}=\"{}\" event=PermissionRequest field=tool_name",
                    <&str>::from(DriftKind::MissingField),
                );
                assert!(
                    logs.contains(TARGET) && logs.contains(&line),
                    "{line:?} in\n{logs}"
                );
            }
        }
    }

    #[test]
    fn generic_tool_name_is_capped_in_the_display() {
        let long = "T".repeat(MAX_DECODED_FIELD_CHARS * 10);
        match make_tool_detail("test", &long, None) {
            ToolDetail::Generic { display } => {
                assert_eq!(display.chars().count(), MAX_DECODED_FIELD_CHARS + 1);
                assert!(display.ends_with('…'));
            }
            other => panic!("expected Generic, got {other:?}"),
        }
        match make_tool_detail("test", "Read", None) {
            ToolDetail::Generic { display } => assert_eq!(display, "Read"),
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    #[test]
    fn generic_keyed_detail_scans_keys_then_caps_the_target() {
        const KEYS: &[&str] = &["command", "path"];
        match generic_keyed_detail("bash", Some(&json!({"command": "ls -la"})), KEYS) {
            ToolDetail::Generic { display } => assert_eq!(display, "bash: ls -la"),
            other => panic!("expected Generic, got {other:?}"),
        }
        let long = "T".repeat(MAX_TOOL_TARGET_CHARS * 5);
        match generic_keyed_detail("run", Some(&json!({ "path": long })), KEYS) {
            ToolDetail::Generic { display } => {
                let target = display.strip_prefix("run: ").expect("has a target suffix");
                assert_eq!(target.chars().count(), MAX_TOOL_TARGET_CHARS + 1);
                assert!(target.ends_with('…'));
            }
            other => panic!("expected Generic, got {other:?}"),
        }
        match generic_keyed_detail("noop", Some(&json!({"other": "x"})), KEYS) {
            ToolDetail::Generic { display } => assert_eq!(display, "noop"),
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    #[test]
    fn every_agent_decoder_caps_its_tool_display() {
        use crate::source::{
            antigravity, claude_code, codewhale, codex, copilot, cursor, dsh, grok, hermes, kimi,
            omp, opencode, reasonix, registry,
        };
        use serde_json::json;
        use std::collections::HashSet;

        let name_s = "N".repeat(MAX_DECODED_FIELD_CHARS * 2);
        let tgt_s = "T".repeat(MAX_TOOL_TARGET_CHARS * 5);
        // codewhale's `tool_args` is a JSON STRING, not an object — embed the
        // over-cap target inside it so its target chokepoint is exercised too.
        let cw_args_s = format!("{{\"command\":\"{tgt_s}\"}}");
        // Borrow as &str (Copy) so each row's closure captures by copy — a String
        // would be MOVED into its first json! and break the `Fn` table.
        let (name, tgt, cw_args) = (name_s.as_str(), tgt_s.as_str(), cw_args_s.as_str());
        // Widest a capped display can be: capped name + ": " + capped target,
        // each gaining one '…'.
        let bound = MAX_DECODED_FIELD_CHARS + 1 + ": ".len() + MAX_TOOL_TARGET_CHARS + 1;

        type Row<'a> = (&'static str, Box<dyn Fn() -> Vec<AgentEvent> + 'a>);
        let table: Vec<Row> = vec![
            (
                claude_code::SOURCE_NAME,
                Box::new(|| {
                    claude_code::decode_cc_line(
                        "/p/ses-a.jsonl",
                        "claude-code",
                        json!({"type":"assistant","message":{"content":[
                            {"type":"tool_use","id":"t1","name":name,"input":{"file_path":tgt}}]}}),
                    )
                }),
            ),
            (
                codex::SOURCE_NAME,
                Box::new(|| {
                    codex::decode_codex_line(
                        "/p/rollout.jsonl",
                        "codex",
                        json!({"type":"response_item","payload":{"type":"function_call","name":name}}),
                    )
                }),
            ),
            (
                antigravity::SOURCE_NAME,
                Box::new(|| {
                    antigravity::decode_ag_line(
                        "/x/transcript.jsonl",
                        "antigravity",
                        json!({"type":"PLANNER_RESPONSE","step_index":0,"tool_calls":[
                            {"name":name,"args":{"CommandLine":tgt}}]}),
                    )
                }),
            ),
            (
                copilot::SOURCE_NAME,
                Box::new(|| {
                    copilot::decode_copilot_line(
                        "/c/id/events.jsonl",
                        "copilot",
                        json!({"type":"tool.execution_start","data":{
                            "toolCallId":"tc1","toolName":name,"arguments":{"command":tgt}}}),
                    )
                }),
            ),
            (
                omp::SOURCE_NAME,
                Box::new(|| {
                    omp::decode_omp_line(
                        "/o/s.jsonl",
                        "omp",
                        json!({"type":"message","message":{"role":"assistant","content":[
                            {"type":"toolCall","id":"t1","name":name,"arguments":{"command":tgt}}]}}),
                    )
                }),
            ),
            (
                reasonix::SOURCE_NAME,
                Box::new(|| {
                    reasonix::decode_rx_hook_payload(test_obj(&json!({
                        "event":"PreToolUse","sessionId":"s","cwd":"/r","toolName":name,"toolArgs":{"command":tgt}})))
                    .expect("reasonix decodes")
                }),
            ),
            (
                dsh::SOURCE_NAME,
                Box::new(|| {
                    dsh::decode_dsh_payload(test_obj(&json!({
                        "type":"tool_call","sessionId":"s1","callId":"c1","toolName":name})))
                    .expect("dsh decodes")
                }),
            ),
            (
                codewhale::SOURCE_NAME,
                Box::new(|| {
                    codewhale::decode_cw_hook_payload(test_obj(&json!({
                        "event":"tool_call_before","session_id":"s","cwd":"/r","tool":name,
                        "tool_args":cw_args})))
                    .expect("codewhale decodes")
                }),
            ),
            (
                opencode::SOURCE_NAME,
                Box::new(|| {
                    opencode::decode_oc_hook_payload(test_obj(&json!({
                        "type":"session.tool.input.started",
                        "data":{"sessionID":"ses-1","id":"c1","name":name}})))
                    .expect("opencode decodes")
                }),
            ),
            (
                cursor::SOURCE_NAME,
                Box::new(|| {
                    cursor::decode_cursor_hook_payload(test_obj(&json!({
                        "hook_event_name":"preToolUse","session_id":"s",
                        "tool_name":name,"tool_input":{"command":tgt}})))
                    .expect("cursor decodes")
                }),
            ),
            (
                hermes::SOURCE_NAME,
                Box::new(|| {
                    hermes::decode_hermes_hook_payload(test_obj(&json!({
                        "hook_event_name":"pre_tool_call","session_id":"s","cwd":"/r",
                        "tool_name":name,"tool_input":{"command":tgt}})))
                    .expect("hermes decodes")
                }),
            ),
            (
                grok::SOURCE_NAME,
                Box::new(|| {
                    grok::decode_grok_hook_payload(test_obj(&json!({
                        "hookEventName":"pre_tool_use","sessionId":"s","cwd":"/r",
                        "workspaceRoot":"/r","toolName":name,"toolUseId":"c1",
                        "toolInput":{"command":tgt},"toolInputTruncated":false})))
                    .expect("grok decodes")
                }),
            ),
            (
                // Kimi's Extend decoder declines PreToolUse, so route the whole
                // payload through the dispatcher to reach `make_tool_detail`.
                kimi::SOURCE_NAME,
                Box::new(|| {
                    decode_hook_payload(json!({
                        "hook_event_name":"PreToolUse","session_id":"s","cwd":"/r",
                        "tool_name":name,"tool_input":{"command":tgt},"tool_use_id":"c1",
                        "_pixtuoid_source":"kimi"}))
                    .expect("kimi decodes")
                }),
            ),
        ];

        for (src, decode) in &table {
            let evs = decode();
            let display = evs
                .iter()
                .find_map(|e| match e {
                    AgentEvent::ToolUseStart {
                        detail: Some(d), ..
                    } => Some(d.display().to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{src}: decoder emitted no ToolUseStart with a detail"));
            assert!(
                display.chars().count() <= bound,
                "{src}: tool display {} chars > cap bound {bound} — a chokepoint bypass leaks raw content",
                display.chars().count()
            );
            assert!(
                display.ends_with('…'),
                "{src}: display {display:?} did not end with the ellipsis — cap did not fire"
            );
        }

        let covered: HashSet<&str> = table.iter().map(|(n, _)| *n).collect();
        for s in crate::source::registry::registered_source_names() {
            let daemon = registry::descriptor_for(s).is_some_and(|d| d.is_daemon());
            if !daemon {
                assert!(covered.contains(s), "add {s} to the decoder cap table");
            }
        }
        for &c in &covered {
            let daemon = registry::descriptor_for(c).is_some_and(|d| d.is_daemon());
            assert!(!daemon, "{c} is a daemon — remove it from the cap table");
        }
    }

    /// How a source derives its [`AgentEvent::UserActionRequest`]`.reason` — the axis this gate keys on.
    enum ReasonKind {
        /// Minted from raw wire content: must route through `ellipsize`.
        Wire,
        /// A fixed `&'static str`: the over-cap wire value must NOT leak in, so
        /// a later switch to a wire field cannot slip past uncapped.
        Fixed,
        /// No [`AgentEvent::UserActionRequest`] reason on the wire today. The row exists so adding one forces a visit.
        NoReason,
    }

    #[test]
    fn every_agent_decoder_caps_its_user_action_request_reason() {
        use crate::source::{
            antigravity, claude_code, codewhale, codex, copilot, cursor, dsh, grok, hermes, kimi,
            omp, opencode, reasonix, registry,
        };
        use serde_json::json;
        use std::collections::HashSet;

        let raw_s = "R".repeat(MAX_DECODED_FIELD_CHARS * 4);
        let raw = raw_s.as_str();
        // A prefix long enough that a Fixed reason cannot contain it by accident.
        let marker = &raw_s[..MAX_DECODED_FIELD_CHARS / 2];
        let bound = MAX_DECODED_FIELD_CHARS + 1;

        type Row<'a> = (
            &'static str,
            ReasonKind,
            Box<dyn Fn() -> Vec<AgentEvent> + 'a>,
        );
        let table: Vec<Row> = vec![
            (
                claude_code::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    decode_hook_payload(json!({
                        "hook_event_name":"Notification","session_id":"s","cwd":"/r",
                        "message":raw,"_pixtuoid_source":"claude-code"}))
                    .expect("cc decodes")
                }),
            ),
            (
                codex::SOURCE_NAME,
                ReasonKind::Fixed,
                Box::new(|| {
                    decode_hook_payload(json!({
                        "hook_event_name":"PermissionRequest","session_id":"s","cwd":"/r",
                        "message":raw,"_pixtuoid_source":"codex"}))
                    .expect("codex decodes")
                }),
            ),
            (
                antigravity::SOURCE_NAME,
                ReasonKind::Fixed,
                Box::new(|| {
                    antigravity::decode_ag_line(
                        "/x/transcript.jsonl",
                        "antigravity",
                        json!({"type":"PLANNER_RESPONSE","step_index":0,"tool_calls":[
                            {"name":"ask_permission","args":{"CommandLine":raw}}]}),
                    )
                }),
            ),
            (
                copilot::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    copilot::decode_copilot_line(
                        "/c/id/events.jsonl",
                        "copilot",
                        json!({"type":"permission.requested","data":{
                            "permissionRequest":{"kind":raw}}}),
                    )
                }),
            ),
            (
                omp::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    omp::decode_omp_line(
                        "/o/s.jsonl",
                        "omp",
                        json!({"type":"message","message":{"role":"assistant","content":[
                            {"type":"toolCall","id":"t1","name":"ask","arguments":{"i":raw}}]}}),
                    )
                }),
            ),
            (
                reasonix::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    reasonix::decode_rx_hook_payload(test_obj(
                        &json!({"event":"Notification","sessionId":"s","cwd":"/r","message":raw}),
                    ))
                    .expect("reasonix decodes")
                }),
            ),
            (
                dsh::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    dsh::decode_dsh_payload(test_obj(&json!({
                        "type":"approval_asked","sessionId":"s1","toolName":"bash","reason":raw})))
                    .expect("dsh decodes")
                }),
            ),
            (
                opencode::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    opencode::decode_oc_hook_payload(test_obj(&json!({
                        "type":"permission.asked",
                        "data":{"sessionID":"ses-1","action":raw}})))
                    .expect("opencode decodes")
                }),
            ),
            (
                grok::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    grok::decode_grok_hook_payload(test_obj(&json!({
                        "hookEventName":"notification","sessionId":"s","cwd":"/r",
                        "workspaceRoot":"/r","notificationType":"permission_prompt",
                        "message":raw})))
                    .expect("grok decodes")
                }),
            ),
            (
                codewhale::SOURCE_NAME,
                ReasonKind::NoReason,
                Box::new(|| {
                    codewhale::decode_cw_hook_payload(test_obj(&json!({
                        "event":"waiting_for_user","session_id":"s","cwd":"/r","message":raw})))
                    .expect("codewhale decodes")
                }),
            ),
            (
                cursor::SOURCE_NAME,
                ReasonKind::NoReason,
                Box::new(|| {
                    cursor::decode_cursor_hook_payload(test_obj(&json!({
                        "hook_event_name":"preToolUse","session_id":"s",
                        "tool_name":"Shell","tool_input":{"command":"ls"},"message":raw})))
                    .expect("cursor decodes")
                }),
            ),
            (
                // The recorded gate carries its WHY on `extra.description` ("delete
                // in root path"), so the reason must route through `ellipsize` too.
                hermes::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    hermes::decode_hermes_hook_payload(test_obj(&json!({
                        "hook_event_name":"pre_approval_request","session_id":"s","cwd":"/r",
                        "extra":{"description":raw}})))
                    .expect("hermes decodes")
                }),
            ),
            (
                // Kimi's Extend decoder declines Notification, so it lands on the
                // SHARED capped arm — the same chokepoint CC rides.
                kimi::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    decode_hook_payload(json!({
                        "hook_event_name":"Notification","session_id":"s","cwd":"/r",
                        "message":raw,"_pixtuoid_source":"kimi"}))
                    .expect("kimi decodes")
                }),
            ),
        ];

        for (src, kind, decode) in &table {
            let evs = decode();
            let reason = evs.iter().find_map(|e| match e {
                AgentEvent::UserActionRequest { reason, .. } => reason.clone(),
                _ => None,
            });
            match kind {
                ReasonKind::Wire => {
                    let reason = reason.unwrap_or_else(|| {
                        panic!("{src}: expected a reason, got {evs:?}");
                    });
                    assert!(
                        reason.chars().count() <= bound,
                        "{src}: UserActionRequest reason {} chars > cap bound {bound} — a \
                         chokepoint bypass leaks raw content into slot state",
                        reason.chars().count()
                    );
                    assert!(
                        reason.ends_with('…'),
                        "{src}: reason {reason:?} did not end with the ellipsis — cap did not fire"
                    );
                }
                ReasonKind::Fixed => {
                    let reason = reason.unwrap_or_else(|| {
                        panic!("{src}: expected a reason, got {evs:?}");
                    });
                    assert!(
                        !reason.contains(marker) && reason.chars().count() <= bound,
                        "{src}: reason {reason:?} is no longer a fixed string — route \
                         the wire value through `ellipsize` and move this row to Wire"
                    );
                }
                ReasonKind::NoReason => assert!(
                    reason.is_none(),
                    "{src}: grew a reason ({reason:?}) — classify it in this table"
                ),
            }
        }

        let covered: HashSet<&str> = table.iter().map(|(n, _, _)| *n).collect();
        for s in registry::registered_source_names() {
            let daemon = registry::descriptor_for(s).is_some_and(|d| d.is_daemon());
            if !daemon {
                assert!(
                    covered.contains(s),
                    "add {s} to the UserActionRequest-reason cap table"
                );
            }
        }
        for &c in &covered {
            let daemon = registry::descriptor_for(c).is_some_and(|d| d.is_daemon());
            assert!(!daemon, "{c} is a daemon — remove it from the cap table");
        }
    }

    #[test]
    fn daemon_source_payload_decodes_to_zero_agent_events() {
        let v = json!({"_pixtuoid_source": "openclaw", "type": "gateway_start", "_pid": 1});
        let evs = decode_hook_payload(v).expect("a daemon payload must not error");
        assert!(
            evs.is_empty(),
            "a daemon source decodes to zero AgentEvents (presence rides the sibling channel), got {evs:?}"
        );
    }

    fn grok_envelope(tag: &str) -> Value {
        json!({
            "_pixtuoid_source": tag,
            "hookEventName": "pre_tool_use",
            "sessionId": "0197fa30-sess",
            "cwd": "/repo",
            "workspaceRoot": "/repo",
            "timestamp": "2026-07-16T12:00:00Z",
            "toolName": "run_terminal_command",
            "toolUseId": "call_1",
            "toolInput": {"command": "ls"},
            "toolInputTruncated": false
        })
    }

    #[test]
    fn grok_tagged_grok_envelope_decodes_via_the_custom_decoder() {
        let evs = decode_hook_payload(grok_envelope("grok")).expect("decodes");
        assert_eq!(evs.len(), 2, "Identity + ToolUseStart");
        assert!(
            evs.iter()
                .all(|e| e.agent_id() == crate::AgentId::from_parts("grok", "0197fa30-sess"))
        );
    }

    /// cursor's UNSTAMPED invocations, which used to reach CC's arms and bail.
    #[test]
    fn an_unstamped_cursor_invocation_is_dropped_quietly() {
        let unstamped = json!({
            "hook_event_name": "preToolUse",
            "session_id": "s1",
            "cursor_version": "2026.08.11-e8db854",
            "workspace_roots": ["/w"],
            "tool_name": "Read",
            "tool_use_id": "t1",
        });
        assert!(
            decode_hook_payload(unstamped.clone())
                .expect("must not error")
                .is_empty(),
            "an unstamped cursor envelope is a duplicate of the stamped one"
        );

        // The STAMPED copy is the one that must survive — dropping both would
        // erase cursor from the office entirely.
        let mut stamped = unstamped;
        stamped["_pixtuoid_source"] = json!("cursor");
        assert!(
            !decode_hook_payload(stamped).expect("Ok").is_empty(),
            "the stamped copy carries the arc"
        );

        // An install written before CC's command gained its env prefix sends a bare,
        // unstamped payload, so this guard must leave the default that catches it.
        let cc = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "cc1",
            "cwd": "/w",
            "tool_name": "Read",
        });
        assert!(
            !decode_hook_payload(cc).expect("Ok").is_empty(),
            "an unstamped CC payload must still decode as claude-code"
        );
    }

    #[test]
    fn cross_fired_grok_envelopes_are_dropped_quietly() {
        for tag in ["claude-code", "cursor"] {
            let evs = decode_hook_payload(grok_envelope(tag))
                .unwrap_or_else(|e| panic!("{tag}: cross-fired envelope must be Ok, got {e}"));
            assert!(
                evs.is_empty(),
                "{tag}: cross-fired grok envelope must decode to zero events, got {evs:?}"
            );
        }
        // An untagged envelope defaults to claude-code — same quiet drop.
        let mut untagged = grok_envelope("x");
        untagged.as_object_mut().unwrap().remove("_pixtuoid_source");
        assert!(decode_hook_payload(untagged).expect("Ok").is_empty());

        assert!(
            decode_hook_payload(grok_envelope("some-future-cli")).is_err(),
            "a non-cc/cursor camelCase envelope must bail (observed), not drop silently"
        );
    }

    #[test]
    fn cc_envelopes_still_decode_normally_despite_the_guard() {
        let evs = decode_hook_payload(json!({
            "hook_event_name": "PreToolUse",
            "session_id": "cc-sess",
            "cwd": "/repo",
            "tool_name": "Bash",
            "tool_use_id": "toolu_1",
            "tool_input": {"command": "ls"}
        }))
        .expect("decodes");
        assert!(!evs.is_empty());
    }
}
