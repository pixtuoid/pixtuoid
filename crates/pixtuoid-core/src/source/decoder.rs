//! Shared decoder utilities used by per-source decoders. Hook payload decoding
//! lives here because the hook socket is shared: each payload's envelope is read
//! here, then dispatched to its source's own arms or the shared CC-shaped ones.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::AgentId;
use crate::id::normalize_path_key;
use crate::source::registry::{Envelope, HookDecoding, SessionField};
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
    #[error("{cli} {} has no {field}", .context.as_deref().unwrap_or(PAYLOAD))]
    Missing {
        /// The wire's source name.
        cli: String,
        /// The event or part of the payload the field belongs to, when it
        /// isn't the payload itself.
        context: Option<String>,
        /// The field.
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

    fn missing(cli: &str, context: Option<&str>, field: &'static str) -> Self {
        Self::Missing {
            cli: display_safe(cli),
            context: context.map(display_safe),
            field,
        }
    }

    fn unsupported(cli: &str, event: &str) -> Self {
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

/// Decides by PATH which `.jsonl` FILES can be a source's transcripts — checked
/// after the extension gate, so it filters files, never directories. A source dir
/// often holds SIBLINGS that must never be walked: grok's rewrite-on-resume
/// `chat_history.jsonl`, Antigravity's duplicate `transcript_full.jsonl`, CC's
/// foreign-schema workflow `journal.jsonl`. The default (`accept_all_paths`)
/// admits every transcript; a source whose own loader decides by content adds a
/// `HeadFilter`.
pub type PathFilter = fn(&Path) -> bool;

pub(crate) fn accept_all_paths(_p: &Path) -> bool {
    true
}

/// What a `.jsonl` file's opening bytes say of whether it is a transcript, for a
/// source whose own loader decides that by content: a sibling with a
/// transcript's path shape but a foreign schema is refused by what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadVerdict {
    /// It opens the way the source's transcripts do.
    Admits,
    /// Its first deciding record proves it no transcript; nothing appended
    /// changes that.
    Refuses,
    /// No deciding record yet (a transcript being born, or a head that could not
    /// be read), so it is judged again on its next walk.
    Undecided,
}

/// Judges a file's opening bytes, its whole lines only (see [`HeadVerdict`]).
pub(crate) type HeadFilter = fn(&[u8]) -> HeadVerdict;

/// Narrow a raw JSON integer to a valid POSIX pid: `i32` range AND strictly
/// positive — `kill(0)`/`kill(-n)` target process GROUPS, so a bogus or zero `_pid`
/// would synthesize a phantom exit that flaps a LIVE gateway Down. Every JSON pid
/// ingress rides it; `fd_probe` deliberately does not (kernel-enumerated `pid_t`).
/// `i64` in, so a wild value is a per-entry skip, not a whole-document fail (#831).
pub(crate) fn checked_pid(raw: i64) -> Option<i32> {
    i32::try_from(raw).ok().filter(|&p| p > 0)
}

/// The hook-envelope key that carries the agent CLI's pid: a plugin stamps it
/// from inside the CLI, else the shim from its ancestry (`pixtuoid-hook`'s `cli_pid`).
/// Pinned by `each_key_the_shim_stamps_is_the_one_core_reads` to the shim's copy.
#[doc(hidden)]
pub const PID_STAMP: &str = "_pid";

pub(crate) fn extract_top_level_cwd(v: &Value) -> Option<PathBuf> {
    v.get("cwd").and_then(Value::as_str).map(PathBuf::from)
}

/// The directory a subagent's transcript nests in under its parent's:
/// `<parent>/subagents/<child>.jsonl`.
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
    Some(crate::state::SlotLabel::join(
        prefix,
        &ellipsize(base, MAX_DECODED_FIELD_CHARS),
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
pub(crate) fn derive_prefixed_label(source: &str, cwd: &Path) -> String {
    let prefix = label_prefix_for(source);
    cwd_basename_label(prefix, cwd).unwrap_or_else(|| prefix.to_string())
}

/// The first key in `keys` (priority order) whose value on `obj` is a
/// [`non_blank`] string.
pub(crate) fn first_non_blank_str<'a>(obj: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let m = obj.as_object()?;
    keys.iter()
        .find_map(|k| m.get(*k).and_then(|v| v.as_str()).and_then(non_blank))
}

/// `s` unless it is empty or whitespace: a model can leave an optional tool
/// argument blank, and a blank target names nothing.
pub(crate) fn non_blank(s: &str) -> Option<&str> {
    (!s.trim().is_empty()).then_some(s)
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

/// An [RFC 3339](https://www.rfc-editor.org/rfc/rfc3339#section-5.6) stamp →
/// epoch seconds; every caller treats `None` as "no information".
#[cfg(any(feature = "native", test))]
pub(crate) fn rfc3339_to_epoch_secs(s: &str) -> Option<u64> {
    rfc3339_to_epoch(s).map(|d| d.as_secs())
}

/// [`rfc3339_to_epoch_secs`] with the fraction kept, to the nanosecond.
#[cfg(any(feature = "native", feature = "harness", test))]
pub(crate) fn rfc3339_to_epoch(s: &str) -> Option<std::time::Duration> {
    let t: jiff::Timestamp = rfc3339_date_time(s)?.parse().ok()?;
    Some(std::time::Duration::new(
        u64::try_from(t.as_second()).ok()?,
        u32::try_from(t.subsec_nanosecond()).ok()?,
    ))
}

/// `s` for jiff's parser when it has RFC 3339's `date-time` shape: that parser
/// reads Temporal's wider grammar (jiff 1.0.2 `src/fmt/temporal/mod.rs:1-56`)
/// (no seconds, a comma fraction, a `[zone]` annotation, `+0200`, `+02:00:30`),
/// so the shape is held here and jiff checks the ranges.
#[cfg(any(feature = "native", feature = "harness", test))]
fn rfc3339_date_time(s: &str) -> Option<std::borrow::Cow<'_, str>> {
    /// `full-date`, the separator and `partial-time` up to its seconds: `9` is
    /// any digit, and §5.6's NOTE lets a space stand for the `T`.
    const HEAD: &[u8] = b"9999-99-99T99:99:99";
    /// jiff refuses a fraction digit past the nanosecond (`src/fmt/util.rs:761-765`),
    /// which is cut instead: it is no reason to drop the stamp.
    const NANOSECOND_DIGITS: usize = 9;
    let head = s.as_bytes().get(..HEAD.len())?;
    let head_fits = HEAD.iter().zip(head).all(|(&want, &got)| match want {
        b'9' => got.is_ascii_digit(),
        b'T' => matches!(got, b'T' | b't' | b' '),
        _ => got == want,
    });
    if !head_fits {
        return None;
    }
    let rest = &s[HEAD.len()..];
    let (digits, offset) = match rest.strip_prefix('.') {
        Some(fraction) => {
            let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return None;
            }
            (digits, &fraction[digits..])
        }
        None => (0, rest),
    };
    /// RFC 3339's `time-hour` ends at 23, where jiff's offset hours run to 25
    /// (jiff-core 1.0.1 `src/bounds.rs:277`).
    const OFFSET_HOUR_END: [u8; 2] = *b"24";
    let offset_fits = match *offset.as_bytes() {
        [b'Z' | b'z'] => true,
        [b'+' | b'-', h1, h2, b':', m1, m2] => {
            [h1, h2, m1, m2].iter().all(u8::is_ascii_digit) && [h1, h2] < OFFSET_HOUR_END
        }
        _ => false,
    };
    if !offset_fits {
        return None;
    }
    Some(if digits <= NANOSECOND_DIGITS {
        std::borrow::Cow::Borrowed(s)
    } else {
        let kept = HEAD.len() + ".".len() + NANOSECOND_DIGITS;
        std::borrow::Cow::Owned(format!("{}{offset}", &s[..kept]))
    })
}

/// The key the shim stamps with the CLI whose hook ran it. Pinned by
/// `each_key_the_shim_stamps_is_the_one_core_reads` to the shim's copy.
pub const SOURCE_STAMP: &str = "_pixtuoid_source";

/// The key the shim stamps with when it forwarded the envelope, in
/// milliseconds since the epoch. Pinned by
/// `each_key_the_shim_stamps_is_the_one_core_reads` to the shim's copy.
#[doc(hidden)]
pub const FORWARDED_AT_STAMP: &str = "_shim_ts_ms";

/// Every key the shim stamps on a hook envelope.
#[doc(hidden)]
pub const SHIM_STAMPS: [&str; 4] = [FORWARDED_AT_STAMP, SOURCE_STAMP, EVENT_STAMP, PID_STAMP];

/// The CLI a hook envelope belongs to — read ONLY from the shim-owned
/// [`SOURCE_STAMP`]. CC's public `source` field carries the SessionStart REASON
/// (startup/resume/clear/compact), so reading it would namespace the agent under
/// "startup" and split it from the claude-code-keyed tool/JSONL/SessionEnd events:
/// an un-reapable ghost.
pub(crate) fn hook_source(obj: &serde_json::Map<String, Value>) -> Option<&str> {
    non_empty(obj, SOURCE_STAMP)
}

/// A test payload as the object a decoder reads.
#[cfg(test)]
pub(crate) fn test_obj(v: &Value) -> &crate::source::registry::HookObject {
    v.as_object().expect("a test payload is an object")
}

/// A test payload decoded as `source`'s, through the entry every payload takes.
#[cfg(test)]
pub(crate) fn test_decode(source: &str, v: &Value) -> DecodeResult<Vec<AgentEvent>> {
    let mut stamped = v.clone();
    stamped[SOURCE_STAMP] = Value::from(source);
    decode_hook_payload(stamped)
}

/// Envelopes that decode to NOTHING, quietly. A DAEMON source's payloads ride the
/// sibling presence channel and carry no `hook_event_name` at all. grok scans
/// `~/.claude/settings.json` AND `~/.cursor/hooks.json` BY DEFAULT
/// (`crates/codegen/xai-grok-pager/docs/user-guide/10-hooks.md:67-77` at
/// `xai-org/grok-build` `2bdd1d6a63`, grok's cite base: [`crate::source::grok`]'s module doc)
/// and fires the shim installed there with its OWN camelCase envelope, so a
/// duplicate arrives tagged `claude-code`/`cursor` beside the grok-tagged one.
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
    if (cc || source == crate::source::cursor::SOURCE_NAME)
        && obj.contains_key(crate::source::grok::HOOK_EVENT_NAME)
    {
        tracing::trace!(source, "dropping grok cross-fired hook envelope");
        return true;
    }
    // cursor's duplicates through Claude Code's hooks, `source/cursor.rs`'s module doc.
    if cc && obj.contains_key("cursor_version") {
        tracing::trace!("dropping a cursor hook envelope on the claude-code channel");
        return true;
    }
    false
}

/// `obj`'s string field `key`, `None` when absent or EMPTY: an empty id or
/// name keys nothing.
pub(crate) fn non_empty<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// A required key `read` finds, breadcrumbed under `event` when it finds none.
/// One spelling of the key serves the read and the report, so a renamed key
/// can't leave the report naming the old one. Only for an event we've committed
/// to decoding: on a transcript line's type read a missing value just means "a
/// line we ignore", and breadcrumbing those would flood.
pub(crate) fn required<T>(
    source: &str,
    event: &str,
    key: &'static str,
    read: impl FnOnce(&'static str) -> Option<T>,
) -> Option<T> {
    let found = read(key);
    if found.is_none() {
        super::drift::missing_field(source, event, key);
    }
    found
}

/// [`required`] for a key whose absence fails the decode, from the same one
/// spelling: the breadcrumb and the error name `context`, the event the key
/// belongs to, unless it is the payload's own.
pub(crate) fn required_or_err<T>(
    source: &str,
    context: Option<&str>,
    key: &'static str,
    read: impl FnOnce(&'static str) -> Option<T>,
) -> DecodeResult<T> {
    required(source, context.unwrap_or(PAYLOAD), key, read)
        .ok_or_else(|| DecodeError::missing(source, context, key))
}

/// What a decode error or breadcrumb names a field of the payload itself by.
const PAYLOAD: &str = "payload";

/// The hook field naming the root session.
const SESSION_ID: &str = "session_id";

/// The CC-shaped envelope the shared arms key on.
pub(crate) const SHARED_ENVELOPE: Envelope = Envelope {
    event_field: HOOK_EVENT_NAME,
    session_field: SessionField {
        within: None,
        field: SESSION_ID,
    },
};

/// A hook's `tool_name`, the key a request binds its pending calls by, so the
/// pre-use and the prompt derive it alike; its absence is drift.
pub(crate) fn hook_tool_name(
    obj: &serde_json::Map<String, Value>,
    source: &str,
    event: &str,
) -> Option<String> {
    required(source, event, "tool_name", |k| non_empty(obj, k))
        .map(|n| ellipsize(n, MAX_DECODED_FIELD_CHARS))
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

/// The identity context a hook arm attaches ahead of an event that may be its
/// session's first sight (an activity event or a turn end). `cwd` is on the
/// wire for CC tool hooks but absent on e.g. Codex `PermissionRequest` — absent
/// or empty maps to `None`, so the reducer's cwd-less registration path
/// (ordinal label, reap-exempt) applies.
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
        non_empty(obj, "cwd").map(std::path::PathBuf::from),
    )
}

/// Where the shim stamps an installed command's `--hook-event`, for a CLI whose
/// payload names no event.
pub const EVENT_STAMP: &str = "_pixtuoid_event";

/// Decode one hook payload into the event sequence the reducer applies.
///
/// The shared tool/permission arms (PreToolUse / PostToolUse / Notification /
/// PermissionRequest) lead with an [`AgentEvent::Identity`] carrying
/// source/session_id/cwd (#221), the claim an unknown id registers from; the
/// shared lifecycle arms do NOT: SessionStart carries identity, an end for an
/// unknown id proves nothing, and on `Stop` an `Identity` would re-create a CC
/// child past its cascade. A source's own arms set their own.
/// A payload without a [`SOURCE_STAMP`] names no CLI and decodes to no events.
///
/// # Errors
///
/// If the payload is not an object, names no event or an unsupported one, or
/// its arm rejects it, a missing or empty session key included.
pub fn decode_hook_payload(v: Value) -> DecodeResult<Vec<AgentEvent>> {
    decode_hook_object(hook_object(&v)?)
}

/// The one object gate for a hook payload, shared by the listener and [`decode_hook_payload`].
pub(crate) fn hook_object(v: &Value) -> DecodeResult<&crate::source::registry::HookObject> {
    v.as_object()
        .ok_or_else(|| DecodeError::not_an_object("hook"))
}

/// A hook payload as a source's arms get it, its event already read.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct Hook<'a> {
    pub(crate) obj: &'a crate::source::registry::HookObject,
    /// The source its stamp names, which need not be a registry row's.
    pub(crate) source: &'a str,
    pub(crate) event: &'a str,
    pub(crate) session_field: SessionField,
}

impl<'a> Hook<'a> {
    /// `obj` as `source`'s payload, refused when `envelope` finds no event.
    fn read(
        obj: &'a crate::source::registry::HookObject,
        source: &'a str,
        envelope: Envelope,
    ) -> DecodeResult<Self> {
        let event = required_or_err(source, None, envelope.event_field, |k| non_empty(obj, k))?;
        Ok(Self {
            obj,
            source,
            event,
            session_field: envelope.session_field,
        })
    }

    /// The session the envelope names, refused when missing or EMPTY: an empty
    /// id keys no session. An arm reads it only where it uses it, as its key or
    /// a child's parent: a subagent's end keys on the child alone, so a wire that
    /// leaves it off there costs nothing.
    pub(crate) fn session(&self) -> DecodeResult<&'a str> {
        let SessionField { within, field } = self.session_field;
        required_or_err(self.source, Some(self.event), field, |k| match within {
            Some(holder) => non_empty(self.obj.get(holder)?.as_object()?, k),
            None => non_empty(self.obj, k),
        })
    }
}

/// [`decode_hook_payload`] past its object gate.
pub(crate) fn decode_hook_object(
    obj: &crate::source::registry::HookObject,
) -> DecodeResult<Vec<AgentEvent>> {
    let Some(source) = hook_source(obj) else {
        // Not `warn!`: a hook that carries no stamp fires once per tool call.
        tracing::debug!(event = ?obj.get(HOOK_EVENT_NAME), "unstamped hook payload dropped");
        return Ok(vec![]);
    };
    let desc = crate::source::registry::descriptor_for(source);
    if decodes_to_nothing(source, obj, desc) {
        return Ok(vec![]);
    }
    let decoding = desc.and_then(|d| d.hook());
    let hook = Hook::read(obj, source, envelope_of(decoding))?;
    let decoded = match decoding {
        Some(HookDecoding::ClaimsAll { decode, .. }) => decode(&hook)?,
        Some(HookDecoding::Shared { extend, .. }) => match extend(&hook)? {
            None => shared_hook_arms(&hook, desc)?,
            claimed => claimed,
        },
        None => shared_hook_arms(&hook, desc)?,
    };
    decoded.ok_or_else(|| refuse_unknown_event(&hook))
}

/// Where `decoding`'s payloads name their event and session: an unregistered
/// source's take the shared arms', so they are CC-shaped.
fn envelope_of(decoding: Option<&HookDecoding>) -> Envelope {
    decoding.map_or(SHARED_ENVELOPE, HookDecoding::envelope)
}

/// A test payload as `source`'s arms are handed it.
#[cfg(test)]
pub(crate) fn test_hook<'a>(source: &'a str, v: &'a Value) -> Hook<'a> {
    let decoding = crate::source::registry::descriptor_for(source).and_then(|d| d.hook());
    Hook::read(test_obj(v), source, envelope_of(decoding)).expect("a test payload names its event")
}

/// The refusal of an event no arm lists.
pub(crate) fn refuse_unknown_event(hook: &Hook<'_>) -> DecodeError {
    super::drift::unknown_event(hook.source, hook.event);
    DecodeError::unsupported(hook.source, hook.event)
}

/// The shared CC-shaped arms, for every source whose row has no claiming decoder.
/// `UserPromptSubmit` ALSO emits `SessionStart` (idempotent in the reducer) because
/// some Codex tools fire no tool hook (codex's `decode_call_hook`).
pub(crate) fn shared_hook_arms(
    hook: &Hook<'_>,
    desc: Option<&'static crate::source::registry::SourceDescriptor>,
) -> DecodeResult<Option<Vec<AgentEvent>>> {
    let Hook {
        obj, source, event, ..
    } = *hook;
    let session_id = hook.session()?;
    let agent_id = AgentId::from_parts(source, session_id);
    let tool_id_key = hook_tool_id_key(desc);
    let start = || AgentEvent::SessionStart {
        agent_id,
        source: source.to_string(),
        session_id: session_id.to_string(),
        cwd: obj.get("cwd").and_then(|s| s.as_str()).unwrap_or("").into(),
        parent_id: None,
    };
    // Beside a transcript, the hook's stands in until the transcript names one.
    let hook_model = |model: Option<&str>| {
        if desc.is_some_and(|d| d.line_decoder().is_some()) {
            AgentEvent::model_hint(agent_id, model)
        } else {
            AgentEvent::model_info(agent_id, model, None)
        }
    };

    Ok(Some(match event {
        // Of CC's `SessionStart` `model`, "Claude Code doesn't always include it"
        // (<https://code.claude.com/docs/en/hooks#common-input-fields>). A codex
        // prompt's `model` is its turn's (`codex-rs/core/src/hook_runtime.rs:723`),
        // so an `ephemeral` thread, which "should not be materialized on disk"
        // (`codex-rs/app-server-protocol/src/protocol/v2/thread_data.rs:228`,
        // both at rust-v0.172.0), follows a switch by it. The JSONL rollout stays
        // the system of record for Codex tool activity.
        "SessionStart" | "UserPromptSubmit" => {
            let mut evs = vec![start()];
            evs.extend(hook_model(non_empty(obj, "model")));
            evs
        }
        // "use a PostModelSwitch hook to follow the model as it changes during a
        // session" (<https://code.claude.com/docs/en/hooks#common-input-fields>):
        // a session with no transcript has no other way. `requested_model` may be
        // an alias or null (<https://code.claude.com/docs/en/hooks#premodelswitch-input>).
        "PostModelSwitch" => {
            let model = required(source, event, "to_model", |k| non_empty(obj, k));
            hook_model(model).into_iter().collect()
        }
        "PreToolUse" => {
            let tool_name = hook_tool_name(obj, source, event);
            let tool_use_id = non_empty(obj, tool_id_key).map(String::from);
            // Named ahead of its item: a permission prompt names only the
            // tool (`claude_code/fixtures/permission-hermetic`), so the request
            // binds the calls pending under that name.
            let pending = tool_use_id.clone().map(|call| AgentEvent::ToolUsePending {
                agent_id,
                tool_use_id: call,
                tool_name: tool_name.clone(),
                turn: None,
            });
            let mut evs = vec![hook_identity(agent_id, source, session_id, obj)];
            evs.extend(pending);
            evs.push(AgentEvent::ToolUseStart {
                agent_id,
                tool_use_id,
                detail: Some(make_tool_detail(
                    source,
                    tool_name.as_deref().unwrap_or("?"),
                    obj.get("tool_input"),
                )),
                copy_id: None,
            });
            evs
        }
        // A failed call fires PostToolUseFailure in PostToolUse's place
        // (<https://code.claude.com/docs/en/hooks#posttoolusefailure>;
        // `packages/agent-core-v2/src/features/externalHooks/agent/agentExternalHooksService.ts:347-361`
        // at `@moonshot-ai/kimi-code@2.8.0`).
        "PostToolUse" | "PostToolUseFailure" => {
            let tool_use_id = non_empty(obj, tool_id_key).map(String::from);
            vec![
                hook_identity(agent_id, source, session_id, obj),
                AgentEvent::ToolUseEnd {
                    agent_id,
                    tool_use_id,
                },
            ]
        }
        "Notification" => {
            let msg = obj.get("message").and_then(|s| s.as_str());
            vec![
                hook_identity(agent_id, source, session_id, obj),
                AgentEvent::user_action_request(
                    agent_id,
                    msg.map(|m| ellipsize(m, MAX_DECODED_FIELD_CHARS)),
                    RequestKeys::default(),
                ),
            ]
        }
        // The prompt names its tool, and kimi's its call too
        // (`kimi/fixtures/permission-hermetic`); an empty name binds nothing.
        "PermissionRequest" => vec![
            hook_identity(agent_id, source, session_id, obj),
            AgentEvent::user_action_request(
                agent_id,
                Some("permission".into()),
                RequestKeys {
                    tool_use_id: non_empty(obj, tool_id_key).map(String::from),
                    tool_name: hook_tool_name(obj, source, event),
                    ..RequestKeys::default()
                },
            ),
        ],
        // Stop ends a turn, not the session: it fires per turn
        // (<https://code.claude.com/docs/en/hooks>'s lifecycle), kimi's on each
        // completed turn's end
        // (`packages/agent-core-v2/src/features/externalHooks/agent/agentExternalHooksService.ts:419-432`
        // at `@moonshot-ai/kimi-code@2.8.0`).
        "Stop" => vec![AgentEvent::turn_end(agent_id)],
        "SessionEnd" => vec![AgentEvent::SessionEnd {
            agent_id,
            as_child: false,
        }],
        // SubagentStart/Stop change the event's SUBJECT to the child AgentId, which
        // these session-keyed arms cannot express: the source's own arms do.
        _ => return Ok(None),
    }))
}

/// The hook field naming the event.
pub(crate) const HOOK_EVENT_NAME: &str = "hook_event_name";

/// The hook field naming the subagent that fired an event.
pub(crate) const SUBAGENT_ID: &str = "agent_id";

/// An event a subagent fired, which names it in `agent_id` beside its root's
/// `session_id` (`claude_code/fixtures/subagent-hermetic`,
/// `codex/fixtures/subagent-hermetic`), decoded as the subagent's own session:
/// `child_session` maps the wire's `agent_id` to that session's id, and a start
/// it decodes to is parented on the root. An event naming no subagent declines
/// (`Ok(None)`).
pub(crate) fn subagent_fired(
    hook: &Hook<'_>,
    child_session: fn(&str) -> String,
) -> DecodeResult<Option<Vec<AgentEvent>>> {
    let Some(agent) = non_empty(hook.obj, SUBAGENT_ID) else {
        return Ok(None);
    };
    let desc = crate::source::registry::descriptor_for(hook.source);
    let mut own = hook.obj.clone();
    own.insert(SESSION_ID.into(), Value::from(child_session(agent)));
    let mut evs = shared_hook_arms(&Hook { obj: &own, ..*hook }, desc)?
        .ok_or_else(|| refuse_unknown_event(hook))?;
    for e in &mut evs {
        if let AgentEvent::SessionStart { parent_id, .. } = e {
            // Read for a start alone: no other arm names the root.
            *parent_id = hook
                .session()
                .ok()
                .map(|root| AgentId::from_parts(hook.source, root));
        }
    }
    Ok(Some(evs))
}

/// The subagent a CC-shaped SubagentStart/Stop names in `agent_id` (the
/// recordings [`subagent_fired`] cites), refused when missing or empty:
/// declined, the entry would refuse the event as unknown.
pub(crate) fn subagent_id<'a>(hook: &Hook<'a>) -> DecodeResult<&'a str> {
    required_or_err(hook.source, Some(hook.event), SUBAGENT_ID, |k| {
        non_empty(hook.obj, k)
    })
}

/// A CC-shaped SubagentStart: the subagent's own session, which `child_session`
/// maps the wire's `agent_id` to, started parented on the root.
pub(crate) fn subagent_start(
    hook: &Hook<'_>,
    child_session: fn(&str) -> String,
) -> DecodeResult<Vec<AgentEvent>> {
    let child = child_session(subagent_id(hook)?);
    let parent = hook.session()?;
    Ok(vec![AgentEvent::SessionStart {
        agent_id: AgentId::from_parts(hook.source, &child),
        source: hook.source.to_string(),
        session_id: child,
        cwd: hook
            .obj
            .get("cwd")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .into(),
        parent_id: Some(AgentId::from_parts(hook.source, parent)),
    }])
}

/// The dispatch tool's current NAME, renamed from `Task` in CC v2.1.63 with no
/// announcement. `Workflow` (CC's fleet dispatcher) is DELIBERATELY not a second
/// entry: its children fire no per-agent tool_use, so one months-long Task in
/// flight would sweep-EXEMPT every FINISHED fleet subagent until the workflow ends —
/// worse desk starvation than the gap. Fleet lifecycle rides the Subagent hooks.
const DISPATCH_TOOL: &str = "Agent";

/// The detail for one tool call. The subagent dispatch is detected SEMANTICALLY, by
/// the PRESENCE of a `subagent_type` input field — presence, not value, so a renamed
/// tool emitting `subagent_type: null` is still caught AND still breadcrumbs the
/// drift. The reducer keys Delegating, an unnamed child's suppression and
/// Task-drain completion on `is_task()`, so a missed dispatch silently disables
/// them.
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
        .and_then(non_blank)
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
    generic_tool_display(tool, args.and_then(|a| first_non_blank_str(a, keys)))
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
/// reach stderr under whatever `tracing` subscriber the host installs — and for
/// Jcode's decoded fields and what a [`crate::remote`] line brings into a scene.
pub(crate) fn display_safe(s: &str) -> String {
    let stripped: String = s.chars().filter(|c| !is_terminal_unsafe(*c)).collect();
    ellipsize(&stripped, MAX_DECODED_FIELD_CHARS)
}

/// A character that can drive a terminal or reorder the text it shows: what
/// terminal egress strips, or escapes in log fields.
#[doc(hidden)]
pub fn is_terminal_unsafe(c: char) -> bool {
    c.is_control() || is_bidi_control(c)
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
            DecodeError::missing(hostile, None, "cwd"),
            DecodeError::missing(hostile, Some(hostile), "cwd"),
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
            DecodeError::missing("cursor", None, "cwd").to_string(),
            "cursor payload has no cwd"
        );
    }

    #[test]
    fn rfc3339_parses_utc_shapes_and_rejects_garbage() {
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
    fn rfc3339_rejects_each_wrong_separator() {
        for bad in [
            "2026x07-16T12:00:05Z",
            "2026-07x16T12:00:05Z",
            "2026-07-16x12:00:05Z",
        ] {
            assert_eq!(rfc3339_to_epoch_secs(bad), None, "{bad:?}");
        }
        // A lower-case `t` is accepted too, and grok's
        // `normalize_path_key`'d stamps arrive lowercased.
        assert_eq!(
            rfc3339_to_epoch_secs("2026-07-16t12:00:05Z"),
            Some(1_784_203_205)
        );
        assert_eq!(
            rfc3339_to_epoch_secs("2026-07-16 12:00:05Z"),
            Some(1_784_203_205),
            "RFC 3339 §5.6's NOTE lets a space stand for the `T`"
        );
    }

    #[test]
    fn rfc3339_refuses_the_wider_grammar_its_parser_reads() {
        for bad in [
            "2026-07-16T12:00Z",
            "2026-07-16T12:00:05,5Z",
            "2026-07-16T12:00:05Z[UTC]",
            "20260716T120005Z",
            "+002026-07-16T12:00:05Z",
        ] {
            assert_eq!(rfc3339_to_epoch_secs(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn rfc3339_field_ranges_admit_the_last_valid_value_and_stop_at_the_next() {
        for (ok, bad) in [
            ("2026-07-31T12:00:05Z", "2026-07-32T12:00:05Z"),
            ("2026-07-16T23:00:05Z", "2026-07-16T24:00:05Z"),
            ("2026-07-16T12:59:05Z", "2026-07-16T12:60:05Z"),
            // 60 is the LEAP second, still in range; 61 never is.
            ("2026-07-16T12:00:60Z", "2026-07-16T12:00:61Z"),
            ("2026-07-16T12:00:05+23:00", "2026-07-16T12:00:05+24:00"),
            ("2026-07-16T12:00:05-00:59", "2026-07-16T12:00:05-00:60"),
        ] {
            assert!(rfc3339_to_epoch_secs(ok).is_some(), "{ok:?} must parse");
            assert_eq!(rfc3339_to_epoch_secs(bad), None, "{bad:?} must not");
        }
        assert_eq!(
            rfc3339_to_epoch_secs("2026-07-16T12:00:60Z"),
            rfc3339_to_epoch_secs("2026-07-16T12:00:59Z"),
            "the leap second lands on :59"
        );
        for bad in [
            "2026-07-00T12:00:05Z",
            "2026-02-31T12:00:05Z",
            "2026-07-16T12:00:05.Z",
            "2026-07-16T-1:00:05Z",
            "2026-07-16T+1:00:05Z",
            "2026-07-16T12:00:05+-1:00",
        ] {
            assert_eq!(rfc3339_to_epoch_secs(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn rfc3339_offset_grammar_is_exact_and_the_sign_moves_the_instant() {
        for bad in [
            "2026-07-16T12:00:05Zjunk",
            "2026-07-16T12:00:05+02-00",
            "2026-07-16T12:00:05+02:00extra",
            "2026-07-16T12:00:05+0200",
            "2026-07-16T12:00:05+02:00:30",
            "2026-07-16T12:00:05\u{2212}02:00",
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

    /// CC's tool hooks carry the level, which its transcript's assistant
    /// lines own (`claude_code::turn_effort`), and so no hook decodes it.
    #[test]
    fn a_tool_hooks_effort_level_decodes_nothing() {
        for event in ["PreToolUse", "PostToolUse", "PostToolUseFailure"] {
            let v = serde_json::json!({
                "_pixtuoid_source": "claude-code",
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
                !evs.iter()
                    .any(|e| matches!(e, AgentEvent::ModelInfo { .. })),
                "{event} got {evs:?}"
            );
        }
    }

    /// CC's and codex's transcripts carry the model, so a hook's is a hint
    /// beside one; kimi's hook is the only one. `PostModelSwitch` names the
    /// model in `to_model`, beside a `requested_model` that may be an alias
    /// (<https://code.claude.com/docs/en/hooks#premodelswitch-input>).
    #[test]
    fn a_hook_s_model_is_a_hint_beside_a_transcript() {
        let long = "m".repeat(MAX_DECODED_FIELD_CHARS + 1);
        let capped = ellipsize(&long, MAX_DECODED_FIELD_CHARS);
        for (event, key) in [
            ("SessionStart", "model"),
            ("PostModelSwitch", "to_model"),
            ("UserPromptSubmit", "model"),
        ] {
            for (source, hint) in [("claude-code", true), ("codex", true), ("kimi", false)] {
                let models = |model: Option<&str>| {
                    let mut v = serde_json::json!({
                        "_pixtuoid_source": source,
                        "hook_event_name": event,
                        "session_id": "ses-m",
                        "cwd": "/repo",
                        "requested_model": "opus"
                    });
                    if let Some(model) = model {
                        v[key] = model.into();
                    }
                    let evs = decode_hook_payload(v).unwrap();
                    let models = evs.iter().filter_map(|e| match e {
                        AgentEvent::ModelHint { model, .. } => Some((true, model.clone())),
                        AgentEvent::ModelInfo { model, .. } => Some((false, model.clone()?)),
                        _ => None,
                    });
                    models.collect::<Vec<_>>()
                };
                let row = format!("{source} {event}");
                assert_eq!(models(Some(&long)), [(hint, capped.clone())], "{row}");
                assert_eq!(models(Some("")), [], "{row}");
                assert_eq!(models(None), [], "{row}");
            }
        }
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

    /// A failed call's hook ends it as a successful one's does, under the
    /// call-id key each source spells.
    #[test]
    fn a_post_tool_use_failure_ends_its_call() {
        for (source, key) in [("claude-code", "tool_use_id"), ("kimi", "tool_call_id")] {
            let decode = |event: &str| {
                let mut v = json!({
                    "_pixtuoid_source": source, "hook_event_name": event,
                    "session_id": "s", "cwd": "/repo", "tool_name": "Bash",
                });
                v[key] = json!("t1");
                decode_hook_payload(v).expect("decodes")
            };
            let failed = decode("PostToolUseFailure");
            assert!(
                failed.iter().any(|e| matches!(e,
                    AgentEvent::ToolUseEnd { tool_use_id: Some(id), .. } if id == "t1")),
                "{source}: {failed:?}"
            );
            assert_eq!(failed, decode("PostToolUse"), "{source}");
        }
    }

    #[test]
    fn post_tool_use_without_cwd_decodes_to_identity_with_none_cwd() {
        // Real CC PostToolUse payloads can omit cwd.
        let evs = decode_hook_payload(json!({
            "_pixtuoid_source": "claude-code",
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
            "_pixtuoid_source": "claude-code",
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
            "_pixtuoid_source": "claude-code",
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

    /// A blank stamp names no CLI, as an absent one: a writer past the shim
    /// that stamps `""` mints no slot whose source the gate cannot resolve.
    #[test]
    fn an_unstamped_hook_payload_decodes_to_nothing() {
        let v = json!({
            "hook_event_name": "SessionStart",
            "session_id": "ses-abc",
            "cwd": "/repo",
        });
        for stamp in [None, Some("")] {
            let evs = match stamp {
                Some(stamp) => test_decode(stamp, &v),
                None => decode_hook_payload(v.clone()),
            }
            .expect("no decode error");
            assert!(evs.is_empty(), "{stamp:?}: {evs:?}");
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

    #[cfg(feature = "native")]
    #[test]
    fn transcript_deriver_empty_cwd_fallback_equals_registry_prefix() {
        use crate::source::registry;
        for d in registry::REGISTRY {
            let Some(row) = registry::transcript_for(d.name) else {
                continue;
            };
            let got = row.native.derive_label.map_or_else(
                || derive_prefixed_label(d.name, Path::new("")),
                |derive| derive(Path::new(""), d.name, Path::new("")),
            );
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
            crate::source::drift::breadcrumbed(
                &renamed,
                "claude-code",
                crate::source::drift::DriftKind::UnknownDispatch,
                &[("tool", "DelegateZ")]
            ),
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
            !known.contains(crate::source::drift::TARGET),
            "the known dispatch name must stay breadcrumb-silent, got:\n{known}"
        );
    }

    #[test]
    fn the_grok_cross_fire_drop_leaves_no_breadcrumb() {
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
            !cross_fire.contains(crate::source::drift::TARGET),
            "the grok cross-fire drop must stay breadcrumb-silent, got:\n{cross_fire}"
        );
    }

    #[test]
    fn a_permission_request_naming_no_tool_leaves_a_drift_breadcrumb() {
        use crate::source::drift::{DriftKind, TARGET, breadcrumbed};
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
                let fields = [("event", "PermissionRequest"), ("field", "tool_name")];
                assert!(
                    breadcrumbed(&logs, source, DriftKind::MissingField, &fields),
                    "{fields:?} in\n{logs}"
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
        // A model may leave an optional key blank: the scan reads past it.
        match generic_keyed_detail("grep", Some(&json!({"command": " ", "path": "src"})), KEYS) {
            ToolDetail::Generic { display } => assert_eq!(display, "grep: src"),
            other => panic!("expected Generic, got {other:?}"),
        }
        // A target found without the scan (CC's, codex's) is blank-checked too.
        match generic_tool_display("Read", Some("  ")) {
            ToolDetail::Generic { display } => assert_eq!(display, "Read"),
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    #[test]
    fn every_agent_decoder_caps_its_tool_display() {
        use crate::source::{
            antigravity, claude_code, codewhale, codex, copilot, cursor, dsh, grok, hermes, jcode,
            kimi, omp, opencode, reasonix, registry,
        };
        use serde_json::json;
        use std::collections::HashSet;

        let name_s = "N".repeat(MAX_DECODED_FIELD_CHARS * 2);
        let tgt_s = "T".repeat(MAX_TOOL_TARGET_CHARS * 5);
        // codewhale's `tool_args` is a JSON STRING, not an object
        // (`codewhale/fixtures/tool-run-hermetic`) — embed the
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
                jcode::SOURCE_NAME,
                Box::new(|| {
                    test_decode(
                        jcode::SOURCE_NAME,
                        &json!({
                        "event":"pre_tool","session_id":"session_fox_123_a","tool_name":name}),
                    )
                    .expect("jcode decodes")
                }),
            ),
            (
                reasonix::SOURCE_NAME,
                Box::new(|| {
                    test_decode(reasonix::SOURCE_NAME, &json!({
                        "event":"PreToolUse","sessionId":"s","cwd":"/r","toolName":name,"toolArgs":{"command":tgt}}))
                    .expect("reasonix decodes")
                }),
            ),
            (
                dsh::SOURCE_NAME,
                Box::new(|| {
                    test_decode(
                        dsh::SOURCE_NAME,
                        &json!({
                        "type":"tool_call","sessionId":"s1","callId":"c1","toolName":name}),
                    )
                    .expect("dsh decodes")
                }),
            ),
            (
                codewhale::SOURCE_NAME,
                Box::new(|| {
                    test_decode(
                        codewhale::SOURCE_NAME,
                        &json!({
                        "event":"tool_call_before","session_id":"s","cwd":"/r","tool":name,
                        "tool_args":cw_args}),
                    )
                    .expect("codewhale decodes")
                }),
            ),
            (
                opencode::SOURCE_NAME,
                Box::new(|| {
                    test_decode(
                        opencode::SOURCE_NAME,
                        &json!({
                        "type":"session.tool.input.started",
                        "data":{"sessionID":"ses-1","id":"c1","name":name}}),
                    )
                    .expect("opencode decodes")
                }),
            ),
            (
                cursor::SOURCE_NAME,
                Box::new(|| {
                    test_decode(
                        cursor::SOURCE_NAME,
                        &json!({
                        "hook_event_name":"preToolUse","session_id":"s",
                        "tool_name":name,"tool_input":{"command":tgt}}),
                    )
                    .expect("cursor decodes")
                }),
            ),
            (
                hermes::SOURCE_NAME,
                Box::new(|| {
                    test_decode(
                        hermes::SOURCE_NAME,
                        &json!({
                        "hook_event_name":"pre_tool_call","session_id":"s","cwd":"/r",
                        "tool_name":name,"args":{"command":tgt}}),
                    )
                    .expect("hermes decodes")
                }),
            ),
            (
                grok::SOURCE_NAME,
                Box::new(|| {
                    test_decode(
                        grok::SOURCE_NAME,
                        &json!({
                        "hookEventName":"pre_tool_use","sessionId":"s","cwd":"/r",
                        "workspaceRoot":"/r","toolName":name,"toolUseId":"c1",
                        "toolInput":{"command":tgt},"toolInputTruncated":false}),
                    )
                    .expect("grok decodes")
                }),
            ),
            (
                // kimi's `extend` arms decline PreToolUse, so the shared arm's cap applies.
                kimi::SOURCE_NAME,
                Box::new(|| {
                    test_decode(
                        kimi::SOURCE_NAME,
                        &json!({
                        "hook_event_name":"PreToolUse","session_id":"s","cwd":"/r",
                        "tool_name":name,"tool_input":{"command":tgt},"tool_use_id":"c1"}),
                    )
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
            antigravity, claude_code, codewhale, codex, copilot, cursor, dsh, grok, hermes, jcode,
            kimi, omp, opencode, reasonix, registry,
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
                    test_decode(
                        claude_code::SOURCE_NAME,
                        &json!({
                        "hook_event_name":"Notification","session_id":"s","cwd":"/r",
                        "message":raw}),
                    )
                    .expect("cc decodes")
                }),
            ),
            (
                codex::SOURCE_NAME,
                ReasonKind::Fixed,
                Box::new(|| {
                    test_decode(
                        codex::SOURCE_NAME,
                        &json!({
                        "hook_event_name":"PermissionRequest","session_id":"s","cwd":"/r",
                        "message":raw}),
                    )
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
                            {"type":"toolCall","id":"t1","name":"ask","arguments":{},"intent":raw}]}}),
                    )
                }),
            ),
            (
                reasonix::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    test_decode(
                        reasonix::SOURCE_NAME,
                        &json!({"event":"Notification","sessionId":"s","cwd":"/r","message":raw}),
                    )
                    .expect("reasonix decodes")
                }),
            ),
            (
                dsh::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    test_decode(
                        dsh::SOURCE_NAME,
                        &json!({
                        "type":"approval_asked","sessionId":"s1","toolName":"bash","reason":raw}),
                    )
                    .expect("dsh decodes")
                }),
            ),
            (
                opencode::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    test_decode(
                        opencode::SOURCE_NAME,
                        &json!({
                        "type":"permission.asked",
                        "data":{"sessionID":"ses-1","action":raw}}),
                    )
                    .expect("opencode decodes")
                }),
            ),
            (
                grok::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    test_decode(
                        grok::SOURCE_NAME,
                        &json!({
                        "hookEventName":"notification","sessionId":"s","cwd":"/r",
                        "workspaceRoot":"/r","notificationType":"permission_prompt",
                        "message":raw}),
                    )
                    .expect("grok decodes")
                }),
            ),
            (
                codewhale::SOURCE_NAME,
                ReasonKind::NoReason,
                Box::new(|| {
                    test_decode(
                        codewhale::SOURCE_NAME,
                        &json!({
                        "event":"waiting_for_user","session_id":"s","cwd":"/r","message":raw}),
                    )
                    .expect("codewhale decodes")
                }),
            ),
            (
                jcode::SOURCE_NAME,
                ReasonKind::NoReason,
                Box::new(|| {
                    test_decode(
                        jcode::SOURCE_NAME,
                        &json!({
                        "event":"pre_tool","session_id":"session_fox_123_a",
                        "tool_name":"bash","message":raw}),
                    )
                    .expect("jcode decodes")
                }),
            ),
            (
                cursor::SOURCE_NAME,
                ReasonKind::NoReason,
                Box::new(|| {
                    test_decode(
                        cursor::SOURCE_NAME,
                        &json!({
                        "hook_event_name":"preToolUse","session_id":"s",
                        "tool_name":"Shell","tool_input":{"command":"ls"},"message":raw}),
                    )
                    .expect("cursor decodes")
                }),
            ),
            (
                // The recorded gate carries its WHY on `description` ("recursive
                // delete": `hermes/fixtures/permission-hermetic`), so the reason must
                // route through `ellipsize` too.
                hermes::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    test_decode(
                        hermes::SOURCE_NAME,
                        &json!({
                        "hook_event_name":"pre_approval_request","session_id":"s","cwd":"/r",
                        "description":raw}),
                    )
                    .expect("hermes decodes")
                }),
            ),
            (
                // kimi's `extend` arms decline Notification, so the shared arm's cap applies.
                kimi::SOURCE_NAME,
                ReasonKind::Wire,
                Box::new(|| {
                    test_decode(
                        kimi::SOURCE_NAME,
                        &json!({
                        "hook_event_name":"Notification","session_id":"s","cwd":"/r",
                        "message":raw}),
                    )
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

    /// cursor's envelopes on the claude-code channel, which used to reach CC's
    /// arms and bail.
    #[test]
    fn a_cursor_envelope_on_the_claude_code_channel_is_dropped_quietly() {
        let envelope = json!({
            "hook_event_name": "preToolUse",
            "session_id": "s1",
            "cursor_version": "2026.10.22-27dcf5f",
            "workspace_roots": ["/w"],
            "tool_name": "Read",
            "tool_use_id": "t1",
        });
        assert!(
            test_decode(crate::source::claude_code::SOURCE_NAME, &envelope)
                .expect("must not error")
                .is_empty(),
            "a claude-code-stamped cursor envelope is a duplicate of the cursor-stamped one"
        );

        // The cursor-stamped copy is the one that must survive — dropping both
        // would erase cursor from the office entirely.
        assert!(
            !test_decode(crate::source::cursor::SOURCE_NAME, &envelope)
                .expect("Ok")
                .is_empty(),
            "the stamped copy carries the arc"
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
        assert!(
            decode_hook_payload(grok_envelope("some-future-cli")).is_err(),
            "a non-cc/cursor camelCase envelope must bail (observed), not drop silently"
        );
    }

    #[test]
    fn cc_envelopes_still_decode_normally_despite_the_guard() {
        let evs = decode_hook_payload(json!({
            "_pixtuoid_source": "claude-code",
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

    use crate::source::drift::breadcrumbed;
    /// `v` stamped as `source`'s, decoded, with what it logged.
    fn stamped_decode(source: &str, v: &Value) -> (DecodeResult<Vec<AgentEvent>>, String) {
        let mut out = None;
        let logs = crate::test_capture::capture_logs(|| out = Some(test_decode(source, v)));
        (out.expect("ran"), logs)
    }

    /// The hook-decoding rows, which each `every_hook_source_*` test covers exactly.
    fn hook_sources() -> std::collections::BTreeSet<&'static str> {
        crate::source::registry::REGISTRY
            .iter()
            .filter(|d| d.hook().is_some())
            .map(|d| d.name)
            .collect()
    }

    /// A session key an arm keys on breadcrumbs its absence, for `pixtuoid
    /// doctor` to count, however the source spells it.
    #[test]
    fn every_hook_source_breadcrumbs_a_missing_session_key() {
        use crate::source::drift::DriftKind;
        use crate::source::{
            antigravity, claude_code, codewhale, codex, cursor, dsh, grok, hermes, jcode, kimi,
            omp, opencode, reasonix,
        };
        let rows = [
            (
                claude_code::SOURCE_NAME,
                json!({"hook_event_name": "PreToolUse"}),
                "session_id",
            ),
            (
                codex::SOURCE_NAME,
                json!({"hook_event_name": "PreToolUse"}),
                "session_id",
            ),
            (
                kimi::SOURCE_NAME,
                json!({"hook_event_name": "Stop"}),
                "session_id",
            ),
            (
                cursor::SOURCE_NAME,
                json!({"hook_event_name": "preToolUse"}),
                "session_id",
            ),
            // A real envelope always carries its cwd and workspace root:
            // neither is an `Option` (`crates/codegen/xai-grok-hooks/src/event.rs:362-363`
            // at `xai-org/grok-build` `2bdd1d6a63`).
            (
                grok::SOURCE_NAME,
                json!({
                    "hookEventName": "pre_tool_use", "cwd": "/r", "workspaceRoot": "/r", "toolName": "t",
                }),
                "sessionId",
            ),
            (
                codewhale::SOURCE_NAME,
                json!({"event": "tool_call_before", "cwd": "/r"}),
                "session_id",
            ),
            (
                codewhale::SOURCE_NAME,
                json!({"event": "subagent_spawn", "agent_id": "a", "workspace": "/r"}),
                "session_id",
            ),
            (
                hermes::SOURCE_NAME,
                json!({"hook_event_name": "pre_tool_call"}),
                "session_id",
            ),
            (
                jcode::SOURCE_NAME,
                json!({"event": "pre_tool"}),
                "session_id",
            ),
            (
                reasonix::SOURCE_NAME,
                json!({"event": "PreToolUse", "cwd": "/r"}),
                "sessionId",
            ),
            (
                antigravity::SOURCE_NAME,
                json!({EVENT_STAMP: "PreToolUse"}),
                "conversationId",
            ),
            (dsh::SOURCE_NAME, json!({"type": "tool_call"}), "sessionId"),
            (
                omp::SOURCE_NAME,
                json!({"type": "session_start"}),
                "sessionId",
            ),
            (
                opencode::SOURCE_NAME,
                json!({"type": "session.tool.input.started", "data": {}}),
                "sessionID",
            ),
        ];
        let failed: Vec<_> = rows
            .iter()
            .filter_map(|(source, v, key)| {
                let (decoded, logs) = stamped_decode(source, v);
                let refused =
                    matches!(decoded, Err(DecodeError::Missing { field, .. }) if field == *key);
                let noted = breadcrumbed(&logs, source, DriftKind::MissingField, &[("field", key)]);
                (!(refused && noted))
                    .then(|| format!("{source}: {decoded:?}, breadcrumbed: {noted}"))
            })
            .collect();
        assert!(failed.is_empty(), "{failed:#?}");
        let covered = rows.iter().map(|(s, ..)| *s).collect();
        assert_eq!(hook_sources(), covered, "a row per hook-decoding source");
    }

    /// A key whose absence refuses the decode breadcrumbs that absence, under the
    /// event the key belongs to or the payload's own; a key it finds leaves none.
    #[test]
    fn a_refused_key_breadcrumbs_under_its_event_or_the_payload() {
        use crate::source::drift::DriftKind;
        let obj = json!({"found": "v"});
        let read = |k: &'static str| obj.get(k).and_then(Value::as_str);
        for (context, event) in [(Some("ev"), "ev"), (None, PAYLOAD)] {
            let mut out = None;
            let logs = crate::test_capture::capture_logs(|| {
                out = Some(required_or_err("src", context, "gone", read));
            });
            assert!(
                matches!(out, Some(Err(DecodeError::Missing { field: "gone", .. }))),
                "{out:?}"
            );
            assert!(
                breadcrumbed(
                    &logs,
                    "src",
                    DriftKind::MissingField,
                    &[("event", event), ("field", "gone")]
                ),
                "{context:?}:\n{logs}"
            );
        }
        let logs = crate::test_capture::capture_logs(|| {
            assert_eq!(
                required_or_err("src", Some("ev"), "found", read).ok(),
                Some("v")
            );
        });
        assert!(
            !breadcrumbed(&logs, "src", DriftKind::MissingField, &[]),
            "{logs}"
        );
    }

    /// An envelope naming no event breadcrumbs that, whichever field the
    /// source names its events in.
    #[test]
    fn every_hook_source_breadcrumbs_a_missing_event() {
        let failed: Vec<_> = hook_sources()
            .into_iter()
            .filter_map(|source| {
                let (decoded, logs) = stamped_decode(source, &json!({}));
                let refused = matches!(decoded, Err(DecodeError::Missing { context: None, .. }));
                let noted = breadcrumbed(
                    &logs,
                    source,
                    crate::source::drift::DriftKind::MissingField,
                    &[("event", PAYLOAD)],
                );
                (!(refused && noted))
                    .then(|| format!("{source}: {decoded:?}, breadcrumbed: {noted}"))
            })
            .collect();
        assert!(failed.is_empty(), "{failed:#?}");
    }

    /// An event no arm lists is refused and breadcrumbed, never decoded as
    /// a listed one's events.
    #[test]
    fn every_hook_source_breadcrumbs_an_unknown_event() {
        use crate::source::drift::DriftKind;
        use crate::source::{
            antigravity, claude_code, codewhale, codex, cursor, dsh, grok, hermes, jcode, kimi,
            omp, opencode, reasonix,
        };
        const UNKNOWN: &str = "NoSuchEvent";
        let rows = [
            (
                claude_code::SOURCE_NAME,
                json!({"hook_event_name": UNKNOWN, "session_id": "s"}),
            ),
            (
                codex::SOURCE_NAME,
                json!({"hook_event_name": UNKNOWN, "session_id": "s"}),
            ),
            (
                codex::SOURCE_NAME,
                json!({"hook_event_name": UNKNOWN, "session_id": "s", "agent_id": "c", "turn_id": "t"}),
            ),
            (
                kimi::SOURCE_NAME,
                json!({"hook_event_name": UNKNOWN, "session_id": "s"}),
            ),
            (
                cursor::SOURCE_NAME,
                json!({"hook_event_name": UNKNOWN, "session_id": "s"}),
            ),
            (
                grok::SOURCE_NAME,
                json!({"hookEventName": UNKNOWN, "sessionId": "s"}),
            ),
            (
                codewhale::SOURCE_NAME,
                json!({"event": UNKNOWN, "session_id": "s", "cwd": "/r"}),
            ),
            (
                hermes::SOURCE_NAME,
                json!({"hook_event_name": UNKNOWN, "session_id": "s"}),
            ),
            (
                jcode::SOURCE_NAME,
                json!({"event": UNKNOWN, "session_id": "s"}),
            ),
            (
                reasonix::SOURCE_NAME,
                json!({"event": UNKNOWN, "sessionId": "s", "cwd": "/r"}),
            ),
            (
                antigravity::SOURCE_NAME,
                json!({EVENT_STAMP: UNKNOWN, "conversationId": "s"}),
            ),
            (dsh::SOURCE_NAME, json!({"type": UNKNOWN, "sessionId": "s"})),
            (omp::SOURCE_NAME, json!({"type": UNKNOWN, "sessionId": "s"})),
            (
                opencode::SOURCE_NAME,
                json!({"type": UNKNOWN, "data": {"sessionID": "s"}}),
            ),
        ];
        let failed: Vec<_> = rows
            .iter()
            .filter_map(|(source, v)| {
                let (decoded, logs) = stamped_decode(source, v);
                let refused = matches!(&decoded, Err(DecodeError::Unsupported { event, .. }) if event == UNKNOWN);
                let noted = breadcrumbed(&logs, source, DriftKind::UnknownEvent, &[("name", UNKNOWN)]);
                (!(refused && noted)).then(|| format!("{source}: {decoded:?}, breadcrumbed: {noted}"))
            })
            .collect();
        assert!(failed.is_empty(), "{failed:#?}");
        let covered = rows.iter().map(|(s, _)| *s).collect();
        assert_eq!(hook_sources(), covered, "a row per hook-decoding source");
    }
}
