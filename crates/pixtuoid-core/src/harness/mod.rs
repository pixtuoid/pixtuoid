//! ONE offline driver for the owner's contract: *transcripts in → "did we
//! parse it AND would the UI show it" out.*
//!
//! ```text
//!   raw line → JSON → the registry's decoder → a real Reducer → SceneState
//! ```
//!
//! Whether the resulting scene would PAINT is the render layer's question,
//! asked one crate up; this half stops at the state the painter reads.
//!
//! Three things here are load-bearing rather than incidental:
//!
//! 1. **Registration comes from the WATCHER, not the decoder.** A JSONL event
//!    for an unknown id is a documented no-op, so a transcript driven with no
//!    seed registers nothing however well it decodes. [`crate::harness::Drive::seeded`] stands
//!    in for `emit_first_sight`, keyed by the SAME registry row the watcher
//!    reads, so the seed can't drift from production.
//! 2. **Transport is load-bearing** (the reducer's hook-wins dedup keys on it),
//!    so it is not a free parameter.
//! 3. **A decoder panic is a contract violation, everywhere.** The watcher and
//!    hook listener log-and-continue on malformed input; a panic takes the
//!    whole watcher down. Every line therefore runs under `catch_unwind`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::source::decoder::{LineDecoder, decode_hook_payload, display_safe};
use crate::source::registry;
use crate::state::{ActivityState, SceneState, ToolKind};
use crate::{AgentEvent, AgentId, Reducer, Transport};

mod captures;
pub mod hermetic;
pub mod host;
pub mod reads;
#[cfg(feature = "native")]
pub mod tap;

pub use captures::{
    CAPTURE_NOTE, CAPTURE_RECORD, CORPUS_REFUSAL, Capture, CaptureRecord, HOOK_PAYLOADS,
    PROVENANCE, capture_base, capture_workspace, capture_workspace_tail, captures,
    jsonl_files_under, recorder_flag, required_version_probe, transcripts_under, version_answer,
};

/// The committed corpus: every recorded capture, fixture and hermetic script.
pub(crate) fn sources_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sources")
}

/// A lifecycle CLASS a driven wire pushed the slot through — asserted as
/// reached-at-some-point, never as the terminal state.
///
/// A class, not a `ToolKind`: `from_display` is case-sensitive, so a lowercase
/// wire tool name (`"bash"`) renders `Active(Other)` where `"Bash"` renders
/// `Active(Bash)` — a cosmetic per-source difference that must not be frozen
/// into a lifecycle assertion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    /// A tool call is in flight.
    Active,
    /// Blocked on a permission/input prompt.
    Waiting,
    /// A subagent dispatch is in flight.
    Delegating,
}

/// One line the pipeline could not carry, identified by position and SHAPE.
///
/// Never by content: a real transcript line carries the user's prose, code and
/// paths, and these render on a terminal.
#[derive(Clone, Debug)]
pub struct LineFailure {
    /// 1-based index among the non-blank lines driven.
    pub(crate) line: usize,
    /// `type="…" keys=[…]` — structure only.
    pub(crate) shape: String,
    /// The decoder's own error; `None` for a panic.
    pub(crate) message: Option<String>,
}

impl std::fmt::Display for LineFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.shape)?;
        if let Some(m) = &self.message {
            write!(f, ": {m}")?;
        }
        Ok(())
    }
}

/// Everything the pipeline observed.
#[derive(Debug)]
pub struct Driven {
    /// The folded scene — what a painter would observe.
    pub scene: SceneState,
    /// Every decoded event in wire order, seed first when there is one.
    pub events: Vec<AgentEvent>,
    /// How many leading `events` are the first-sight SEED, not the wire's own
    /// output (0 or 1). A census must report [`Self::wire_events`] instead — the
    /// seed registers a slot unconditionally, so counting it scores garbage 1/1.
    pub(crate) seed_events: usize,
    /// Non-blank lines driven (blank lines are skipped, not counted).
    pub lines: usize,
    /// Lines that were not valid JSON — a torn write, outside the decoder
    /// contract (the watcher skips them too).
    pub unparseable: usize,
    /// Lines whose decoder returned `Err`. ALWAYS a defect on bytes the source
    /// itself wrote: the contract is log-and-continue, never a hard error.
    pub decode_errors: Vec<LineFailure>,
    /// Lines whose decoder PANICKED — the never-panic contract violated.
    pub panics: Vec<LineFailure>,
    /// Lifecycle classes any slot passed through, in first-reached order.
    pub reached: Vec<Reach>,
    /// The drift breadcrumbs the decode and fold logged, each as its fields: a
    /// field whose only use is the breadcrumb its absence logs is still read.
    pub(crate) breadcrumbs: Vec<String>,
}

impl Driven {
    /// Slots the fold registered — "did the wire reach the scene at all".
    pub fn registered(&self) -> usize {
        self.scene.agents.len()
    }

    /// Events the WIRE produced, excluding the first-sight seed — the honest
    /// numerator for any census.
    pub fn wire_events(&self) -> usize {
        self.events.len() - self.seed_events
    }

    /// Assert bytes WE control drove cleanly. Not for unbounded bytes (the
    /// corpus shell reports the same three counts instead).
    ///
    /// # Panics
    ///
    /// If a line was not valid JSON, the decoder returned `Err`, or the decoder panicked.
    pub fn assert_clean(&self, what: &str) {
        assert_eq!(
            self.unparseable, 0,
            "{what}: {} line(s) are not valid JSON",
            self.unparseable
        );
        assert!(
            self.decode_errors.is_empty(),
            "{what}: decode error(s): {:?}",
            self.decode_errors
        );
        assert!(
            self.panics.is_empty(),
            "{what}: the decoder PANICKED (never-panic contract): {:?}",
            self.panics
        );
    }
}

/// The fixed wall clock the fold runs at: with one instant for every event, no
/// sweep's elapsed-time threshold can fire mid-drive and reap a slot the caller
/// is about to assert on.
const DEFAULT_NOW_EPOCH_SECS: u64 = 1_800_000_000;

/// How far past `now` the settle tick runs. Long enough for the reducer's
/// `ACTIVE_GRACE_WINDOW` pending-idle to realize (so a resolved tool really
/// reads Idle), short enough to stay inside `EXIT_GRACE_WINDOW` (so a wire
/// carrying its own `SessionEnd` still leaves the slot to observe).
const SETTLE: Duration = Duration::from_secs(3);

/// PER-FLOOR desk capacity for the driven scene. Registration past the TOTAL is
/// REFUSED, and SILENTLY — so a wire carrying more agents than that reads as
/// "decoded but never reached the scene", exactly the false verdict every driver
/// here exists to avoid.
///
/// SECOND consumer, in another crate: `wire_to_pixels` shapes its EMPTY pixel
/// baseline with this same value, so changing it moves that test's measured
/// delta.
pub const DRIVEN_DESKS: usize = 64;

/// The seed session's working directory. Non-empty on purpose: an empty cwd
/// registers the reducer's unknown-cwd ghost, which is not what any driver wants
/// to observe.
const SEED_CWD: &str = "/pixtuoid/harness";

/// The decode-and-fold pipeline for one source's bytes.
#[derive(Debug)]
pub struct Drive {
    transport: Transport,
    decode: Decode,
    seeded: bool,
    now: SystemTime,
}

/// Private so the decoder↔transport pairing stays a constructor's job, never a
/// caller's.
#[derive(Debug)]
enum Decode {
    Transcript {
        decode: LineDecoder,
        source: String,
        logical: String,
    },
    /// Carries NO source: `decode_hook_payload` routes on the envelope's own
    /// `_pixtuoid_source` stamp, so a source passed in here would be inert at
    /// best and a lie at worst.
    Hooks,
}

impl Drive {
    /// Drive the transcript AT `path` — the production form: the logical key is
    /// `normalize_path_key` of it, exactly the string `walk_jsonl` hands the
    /// decoder AND runs the id deriver over.
    ///
    /// Use this whenever the transcript is a real file: passing a raw Windows
    /// path to [`Drive::transcript`] instead keys the seed normalized while the
    /// decoder keeps the raw string, landing every decoded event on a phantom id.
    pub fn transcript_at(source: &str, path: &Path) -> Option<Self> {
        Self::transcript(
            source,
            &crate::id::normalize_path_key(&path.to_string_lossy()),
        )
    }

    /// Drive a transcript's lines through `source`'s own `LineDecoder` on
    /// `Transport::Jsonl`, keyed on the LOGICAL string `logical` — for a caller
    /// whose key is not a filesystem path. For a real file use
    /// [`Drive::transcript_at`], which applies production's normalization.
    ///
    /// `None` when the source is hook-only, a daemon, or unregistered: a caller
    /// that silently fell back to some other decoder would report a green
    /// never-panic run having exercised the wrong one.
    pub fn transcript(source: &str, logical: &str) -> Option<Self> {
        let decode = registry::descriptor_for(source)?.line_decoder()?;
        Some(Self {
            transport: Transport::Jsonl,
            decode: Decode::Transcript {
                decode,
                source: source.to_string(),
                logical: logical.to_string(),
            },
            seeded: false,
            now: SystemTime::UNIX_EPOCH + Duration::from_secs(DEFAULT_NOW_EPOCH_SECS),
        })
    }

    /// Drive hook envelopes through the shared `decode_hook_payload` on
    /// `Transport::Hook`. Takes NO source: the dispatcher routes by the
    /// envelope's own `_pixtuoid_source` stamp.
    ///
    /// There is no seed here by construction — a hook for an unknown id
    /// REGISTERS it (hooks are proof of life), so hook bytes never need one.
    pub fn hooks() -> Self {
        Self {
            transport: Transport::Hook,
            decode: Decode::Hooks,
            seeded: false,
            now: SystemTime::UNIX_EPOCH + Duration::from_secs(DEFAULT_NOW_EPOCH_SECS),
        }
    }

    /// Prepend the `SessionStart` the watcher's `emit_first_sight` would emit
    /// for this transcript, keyed by the source's registry row.
    ///
    /// Not a free choice: a transcript carrying no `SessionStart` of its own is
    /// registered in production by the watcher, so an offline driver must seed
    /// it; one whose head line IS a `SessionStart` would only get a duplicate.
    #[must_use]
    pub fn seeded(mut self) -> Self {
        self.seeded = true;
        self
    }

    /// Fold at `now` instead of the default fixed instant. Only a caller that
    /// RENDERS the resulting scene needs this — the render must read the same
    /// clock the fold wrote.
    #[must_use]
    pub fn at(mut self, now: SystemTime) -> Self {
        self.now = now;
        self
    }

    fn seed_id(source: &str, logical: &str) -> AgentId {
        AgentId::from_parts(
            source,
            &registry::id_deriver_for(source)(Path::new(logical)),
        )
    }

    /// Drive `lines` end to end: blank lines skipped, every other line parsed,
    /// decoded under `catch_unwind`, and the decoded stream folded through a
    /// real `Reducer`.
    pub fn lines<I, S>(&self, lines: I) -> Driven
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut d = Driven {
            scene: SceneState::uniform(DRIVEN_DESKS),
            events: Vec::new(),
            seed_events: 0,
            lines: 0,
            unparseable: 0,
            decode_errors: Vec::new(),
            panics: Vec::new(),
            reached: Vec::new(),
            breadcrumbs: Vec::new(),
        };
        let sink = BreadcrumbSink::default();
        tracing::subscriber::with_default(sink.clone(), || self.drive(lines, &mut d));
        d.breadcrumbs = sink.take();
        d
    }

    fn drive<I, S>(&self, lines: I, d: &mut Driven)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if let (
            true,
            Decode::Transcript {
                source, logical, ..
            },
        ) = (self.seeded, &self.decode)
        {
            d.seed_events = 1;
            d.events.push(AgentEvent::SessionStart {
                agent_id: Self::seed_id(source, logical),
                source: source.clone(),
                session_id: format!("{source}-harness-seed"),
                cwd: PathBuf::from(SEED_CWD),
                parent_id: None,
            });
        }

        for line in lines {
            let line = line.as_ref();
            if line.trim().is_empty() {
                continue;
            }
            d.lines += 1;
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                d.unparseable += 1;
                continue;
            };
            // `v` moves into the decoder: keeping a copy for the failure shape
            // would clone whole transcript bodies on the corpus path, and a
            // failure is rare enough to re-parse for.
            match catch_unwind(AssertUnwindSafe(|| self.decode_one(v))) {
                Ok(Ok(evs)) => d.events.extend(evs),
                Ok(Err(e)) => d.decode_errors.push(LineFailure {
                    line: d.lines,
                    shape: shape_of(line),
                    message: Some(e.to_string()),
                }),
                Err(_) => d.panics.push(LineFailure {
                    line: d.lines,
                    shape: shape_of(line),
                    message: None,
                }),
            }
        }

        self.fold(d);
    }

    fn decode_one(&self, v: Value) -> crate::source::decoder::DecodeResult<Vec<AgentEvent>> {
        match &self.decode {
            Decode::Transcript {
                decode,
                source,
                logical,
            } => decode(logical, source, v),
            Decode::Hooks => decode_hook_payload(v),
        }
    }

    /// Each event is observed BEFORE and AFTER a settle tick, so a transient
    /// class a later event resolves still counts as reached.
    fn fold(&self, d: &mut Driven) {
        let mut reducer = Reducer::new();
        for ev in &d.events {
            reducer.apply(&mut d.scene, ev.clone(), self.now, self.transport);
            note_reached(&d.scene, &mut d.reached);
            reducer.tick(&mut d.scene, self.now + SETTLE);
            note_reached(&d.scene, &mut d.reached);
        }
    }
}

/// Collects the events logged on [`crate::source::drift::TARGET`] while it is the
/// thread's default subscriber.
#[derive(Clone, Default)]
struct BreadcrumbSink(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl BreadcrumbSink {
    fn take(&self) -> Vec<String> {
        let mut logged = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *logged)
    }
}

impl tracing::Subscriber for BreadcrumbSink {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == crate::source::drift::TARGET
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut line = String::new();
        event.record(
            &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                line.push_str(&format!("{}={value:?} ", field.name()));
            },
        );
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line.trim_end().to_owned());
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// Union the lifecycle classes every live slot is in into `reached`.
///
/// Every slot, not the first: a `HashMap`'s iteration order is arbitrary, so
/// reading one slot makes a multi-agent wire report a different class run to
/// run. Strictly weaker as a result — one slot reaching both classes and two
/// slots reaching one each are indistinguishable.
fn note_reached(scene: &SceneState, reached: &mut Vec<Reach>) {
    for slot in scene.agents.values() {
        let r = match &slot.state {
            ActivityState::Active {
                kind: ToolKind::Task,
                ..
            } => Reach::Delegating,
            ActivityState::Active { .. } => Reach::Active,
            ActivityState::Waiting { .. } => Reach::Waiting,
            ActivityState::Idle => continue,
        };
        if !reached.contains(&r) {
            reached.push(r);
        }
    }
}

/// A line's STRUCTURE — its top-level `type` and key names, never its values.
/// The one content-safe formatter for failure reporting.
fn shape_of(line: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return "<unparseable>".to_string();
    };
    // Key NAMES are wire-controlled too, so they route through the crate's ONE
    // terminal-egress policy; a second, laxer sanitizer here would be a
    // chokepoint bypass.
    let ty = display_safe(v.get("type").and_then(Value::as_str).unwrap_or(""));
    let keys = v.as_object().map_or_else(
        || "<non-object>".to_string(),
        |o| display_safe(&o.keys().cloned().collect::<Vec<_>>().join(",")),
    );
    format!("type={ty:?} keys=[{keys}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_breadcrumb_a_decode_logs_is_part_of_what_it_decoded() {
        let file = sources_root()
            .join("delegation/fixtures/dsh")
            .join(HOOK_PAYLOADS);
        let text = std::fs::read_to_string(&file).expect("the recorded delegation");
        let call = text
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).expect("json"))
            .find(|v| v["type"] == "tool_call")
            .expect("a tool_call line");
        let crumbs = Drive::hooks().lines([call.to_string()]).breadcrumbs;
        assert!(crumbs.is_empty(), "{crumbs:?}");
        let mut dropped = call;
        dropped.as_object_mut().expect("an object").remove("callId");
        let crumbs = Drive::hooks().lines([dropped.to_string()]).breadcrumbs;
        assert!(crumbs.iter().any(|c| c.contains("callId")), "{crumbs:?}");
    }

    /// A CC transcript line carrying only tool activity — nothing in it
    /// registers a session.
    fn cc_tool_line() -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "id": "tu-1", "name": "Bash", "input": {"command": "ls"}}
            ]}
        })
        .to_string()
    }

    const CC_TRANSCRIPT: &str =
        "/h/.claude/projects/-h-p/01000000-0000-7000-8000-0000000000cc.jsonl";

    #[test]
    fn an_unseeded_activity_only_transcript_decodes_but_registers_nothing() {
        let d = Drive::transcript("claude-code", CC_TRANSCRIPT)
            .unwrap()
            .lines([cc_tool_line()]);
        assert!(!d.events.is_empty(), "the line must decode");
        assert_eq!(
            d.registered(),
            0,
            "a JSONL event for an unknown id is a no-op"
        );
        assert!(d.reached.is_empty());
    }

    #[test]
    fn the_first_sight_seed_registers_and_the_wire_drives_the_slot_active() {
        let d = Drive::transcript("claude-code", CC_TRANSCRIPT)
            .unwrap()
            .seeded()
            .lines([cc_tool_line()]);
        assert_eq!(d.registered(), 1);
        assert_eq!(d.reached, vec![Reach::Active]);
    }

    #[test]
    fn the_seed_coalesces_with_the_decoders_own_agent_id() {
        let d = Drive::transcript("claude-code", CC_TRANSCRIPT)
            .unwrap()
            .seeded()
            .lines([cc_tool_line()]);
        let ids: std::collections::BTreeSet<_> =
            d.events.iter().map(AgentEvent::agent_id).collect();
        assert_eq!(ids.len(), 1, "seed + decoded activity must be ONE agent");
        assert_eq!(
            ids.into_iter().next().unwrap(),
            AgentId::from_parts("claude-code", "01000000-0000-7000-8000-0000000000cc"),
            "the seed must key on the row's derivation (the CC filename stem)"
        );
    }

    #[test]
    fn transcript_at_keys_on_the_normalized_path_like_the_walker_does() {
        let path = Path::new(CC_TRANSCRIPT);
        let by_path = Drive::transcript_at("claude-code", path)
            .unwrap()
            .seeded()
            .lines([cc_tool_line()]);
        let by_key = Drive::transcript(
            "claude-code",
            &crate::id::normalize_path_key(&path.to_string_lossy()),
        )
        .unwrap()
        .seeded()
        .lines([cc_tool_line()]);
        assert_eq!(
            by_path.events[0].agent_id(),
            by_key.events[0].agent_id(),
            "transcript_at must fold the path exactly as walk_jsonl does"
        );
    }

    #[test]
    fn blank_lines_are_skipped_and_non_json_counts_as_unparseable() {
        let d = Drive::transcript("claude-code", CC_TRANSCRIPT)
            .unwrap()
            .lines(["", "   ", "not json at all", "{}"]);
        assert_eq!(d.lines, 2, "only the non-blank lines are driven");
        assert_eq!(d.unparseable, 1);
        assert!(d.decode_errors.is_empty());
    }

    #[test]
    fn a_hook_envelope_registers_itself_with_no_seed() {
        let payload = serde_json::json!({
            "_pixtuoid_source": "claude-code",
            "hook_event_name": "SessionStart",
            "session_id": "ses-h",
            "cwd": "/repo"
        })
        .to_string();
        let d = Drive::hooks().lines([payload]);
        assert_eq!(d.registered(), 1);
    }

    #[test]
    fn transcript_is_none_for_hook_only_daemon_and_unknown_sources() {
        assert!(Drive::transcript("cursor", "/x.jsonl").is_none());
        assert!(Drive::transcript("openclaw", "/x.jsonl").is_none());
        assert!(Drive::transcript("not-a-source", "/x.jsonl").is_none());
    }

    /// A `Drive` over a STAND-IN decoder: no real decoder rejects or panics on
    /// any corpus bytes, so neither failure lane below has a real fixture.
    fn drive_with(decode: LineDecoder) -> Drive {
        Drive {
            transport: Transport::Jsonl,
            decode: Decode::Transcript {
                decode,
                source: "claude-code".to_string(),
                logical: CC_TRANSCRIPT.to_string(),
            },
            seeded: false,
            now: SystemTime::UNIX_EPOCH + Duration::from_secs(DEFAULT_NOW_EPOCH_SECS),
        }
    }

    /// A line carrying prose under a key — what the failure report must NOT
    /// echo back to a terminal.
    const PROSE_LINE: &str = r#"{"type":"assistant","secret":"do not print me"}"#;

    #[test]
    fn a_decoder_error_is_recorded_with_the_lines_shape_not_its_content() {
        fn refuse(
            _p: &str,
            _s: &str,
            _v: Value,
        ) -> crate::source::decoder::DecodeResult<Vec<AgentEvent>> {
            Err(crate::source::decoder::DecodeError::unsupported(
                "fixture", "event",
            ))
        }
        let d = drive_with(refuse).lines([PROSE_LINE]);

        assert_eq!(d.decode_errors.len(), 1);
        assert_eq!(d.decode_errors[0].line, 1);
        assert_eq!(
            d.decode_errors[0].message.as_deref(),
            Some("unsupported fixture event: event")
        );
        let shape = &d.decode_errors[0].shape;
        assert!(
            !shape.contains("do not print me"),
            "the shape must carry key NAMES only, got {shape}"
        );
        assert!(shape.contains("type=\"assistant\""), "got {shape}");
        assert!(shape.contains("secret"), "got {shape}");
        assert!(d.panics.is_empty(), "a decode error is not a panic");
    }

    #[test]
    fn wire_events_excludes_the_seed_so_garbage_scores_zero() {
        let garbage = ["not json at all", r#"{"totally":"unrelated"}"#];
        let d = Drive::transcript("claude-code", CC_TRANSCRIPT)
            .unwrap()
            .seeded()
            .lines(garbage);
        assert_eq!(
            d.registered(),
            1,
            "the seed registers a slot even for garbage — that is WHY the census \
             cannot count it"
        );
        assert_eq!(d.wire_events(), 0, "no wire event came out of garbage");
        assert_eq!(d.events.len(), 1, "the one event is the seed itself");

        let d = Drive::transcript("claude-code", CC_TRANSCRIPT)
            .unwrap()
            .seeded()
            .lines([cc_tool_line()]);
        assert_eq!(d.seed_events, 1);
        assert_eq!(d.wire_events(), d.events.len() - 1);

        let d = Drive::transcript("claude-code", CC_TRANSCRIPT)
            .unwrap()
            .lines([cc_tool_line()]);
        assert_eq!(d.seed_events, 0);
        assert_eq!(d.wire_events(), d.events.len());
    }

    #[test]
    fn line_failure_display_carries_position_and_shape_not_content() {
        fn refuse(
            _p: &str,
            _s: &str,
            _v: Value,
        ) -> crate::source::decoder::DecodeResult<Vec<AgentEvent>> {
            Err(crate::source::decoder::DecodeError::unsupported(
                "fixture", "event",
            ))
        }
        let d = drive_with(refuse).lines([PROSE_LINE]);
        let rendered = d.decode_errors[0].to_string();
        assert!(rendered.starts_with("line 1: "), "got {rendered}");
        assert!(rendered.contains("type=\"assistant\""), "got {rendered}");
        assert!(
            rendered.ends_with(": unsupported fixture event: event"),
            "got {rendered}"
        );
        assert!(
            !rendered.contains("do not print me"),
            "Display must not echo the line's values, got {rendered}"
        );

        fn boom(
            _p: &str,
            _s: &str,
            _v: Value,
        ) -> crate::source::decoder::DecodeResult<Vec<AgentEvent>> {
            panic!("decoder blew up")
        }
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let d = drive_with(boom).lines([PROSE_LINE]);
        std::panic::set_hook(prev);
        let rendered = d.panics[0].to_string();
        assert!(!rendered.contains("blew up"), "got {rendered}");
        assert!(
            rendered.ends_with(']'),
            "shape is the whole line, got {rendered}"
        );
    }

    #[test]
    fn a_panicking_decoder_is_caught_and_recorded_by_shape() {
        fn boom(
            _p: &str,
            _s: &str,
            _v: Value,
        ) -> crate::source::decoder::DecodeResult<Vec<AgentEvent>> {
            panic!("decoder blew up")
        }
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let d = drive_with(boom).lines([PROSE_LINE]);
        std::panic::set_hook(prev);

        assert_eq!(d.panics.len(), 1, "the panic must be captured, not unwound");
        assert_eq!(d.panics[0].line, 1);
        assert!(
            d.panics[0].shape.contains("type=\"assistant\""),
            "got {}",
            d.panics[0].shape
        );
        assert!(
            !d.panics[0].shape.contains("do not print me"),
            "got {}",
            d.panics[0].shape
        );
        assert!(d.decode_errors.is_empty(), "a panic is not a decode error");
    }

    #[test]
    fn a_transient_class_resolved_by_a_later_line_still_counts_as_reached() {
        let start = serde_json::json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "id": "tu-9", "name": "Bash", "input": {"command": "ls"}}
            ]}
        })
        .to_string();
        let end = serde_json::json!({
            "type": "user",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": "tu-9", "content": "ok"}
            ]}
        })
        .to_string();
        let d = Drive::transcript("claude-code", CC_TRANSCRIPT)
            .unwrap()
            .seeded()
            .lines([start, end]);
        assert!(
            d.reached.contains(&Reach::Active),
            "the resolved tool call must still count as reached, got {:?}",
            d.reached
        );
    }
}
