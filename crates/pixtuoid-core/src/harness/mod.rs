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
//! Two things here are load-bearing rather than incidental:
//!
//! 1. **Registration comes from the WATCHER, not the decoder.** A JSONL event
//!    for an unknown id is a documented no-op, so a transcript driven with no
//!    seed registers nothing however well it decodes. [`crate::harness::Drive::seeded`] stands
//!    in for `emit_registration`, through `Sight::registers`, the head scan and
//!    `first_sight_start` from the SAME registry row. The seed-at-EOF decision
//!    (`should_seed_at_eof`) and a claim's `LabelHint` stay out, as does every
//!    reader in the registry's native-only `NativeTranscript` column (the title,
//!    the label, a path-derived cwd, and the tail-activity reader whose
//!    sidecar-only verdict vetoes a registration), which a wasm-clean build lacks.
//! 2. **Transport is load-bearing** (the reducer's hook-wins dedup keys on it),
//!    so it is not a free parameter.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::source::daemon::{PresenceMsg, apply_presence};
use crate::source::decoder::{DecodeError, LineDecoder, decode_hook_payload, display_safe};
use crate::source::first_sight::{
    HEAD_BYTES, Life, Registration, Sight, each_life, extract_head_fields, first_sight_start,
    holds_life, last_life, transcript_session,
};
use crate::source::registry;
use crate::state::{ActivityState, SceneState, ToolKind, ToolUse};
use crate::{AgentEvent, AgentId, Reducer, Transport};

mod captures;
pub mod fuzz;
pub mod hermetic;
pub mod host;
pub mod merge;
pub mod plan;
pub mod provenance;
pub mod reads;
#[cfg(feature = "native")]
pub mod tap;

#[cfg(feature = "native")]
pub use captures::outlived;
pub(crate) use captures::source_home;
pub use captures::{
    BOOT, CAPTURE_NOTE, CAPTURE_RECORD, CORPUS_REFUSAL, Canonical, Capture, CaptureRecord,
    DRIVER_COPY, DRIVER_DIR_ENV, GAVE_UP_NOTE, HOOK_PAYLOADS, NOTE_FILE, OUTSIDE_CONFORMANCE,
    PROVENANCE, ProcessExit, SecondTurn, ServerStop, canonical_capture, capture_base, capture_root,
    capture_sandbox, capture_workspace, capture_workspace_tail, captures, files_under,
    fixtures_dir, jsonl_files_under, required_version_probe, slashed_under, transcript_in,
    transcripts_under, version_answer, wait_until,
};

/// The committed corpus: every recorded capture, fixture and hermetic script.
pub fn sources_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sources")
}

/// A registered id as the modules for its CLI spell it (`claude-code` →
/// `claude_code`): its decoder, its profile, its test module and their dirs.
pub fn module_of(source: &str) -> String {
    source.replace('-', "_")
}

/// The registered id a module name spells, the inverse of [`module_of`].
pub(crate) fn source_of(module: &str) -> String {
    module.replace('_', "-")
}

/// The `T` the JSON file at `path` holds.
///
/// # Errors
///
/// The file cannot be read or holds no `T`, either naming `path`.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> std::io::Result<T> {
    let named = |kind, e: &dyn std::fmt::Display| {
        std::io::Error::new(kind, format!("{}: {e}", path.display()))
    };
    let text = std::fs::read_to_string(path).map_err(|e| named(e.kind(), &e))?;
    serde_json::from_str(&text).map_err(|e| named(std::io::ErrorKind::InvalidData, &e))
}

/// A lifecycle CLASS a driven wire pushed the slot through — asserted as
/// reached-at-some-point, never as the terminal state.
///
/// A class, not a `ToolKind`: `from_display` is case-sensitive, so a lowercase
/// wire tool name (`"bash"`) renders `Running(Other)` where `"Bash"` renders
/// `Running(Bash)` — a cosmetic per-source difference that must not be frozen
/// into a lifecycle assertion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    /// A tool call is in flight.
    Running,
    /// A turn is open, showing no item.
    RunningNoItem,
    /// Blocked on a permission/input prompt.
    RequiresAction,
    /// A subagent dispatch is in flight.
    Delegating,
}

/// One line the pipeline could not carry, identified by position and SHAPE.
///
/// Never by content: a real hook payload carries the user's prompts, code and
/// paths, and these render on a terminal.
#[derive(Clone, Debug)]
pub(crate) struct LineFailure {
    /// 1-based index among the non-blank lines driven.
    pub(crate) line: usize,
    /// `type="…" keys=[…]` — structure only.
    pub(crate) shape: String,
    /// The decoder's own error.
    pub(crate) message: String,
}

impl std::fmt::Display for LineFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}: {}", self.line, self.shape, self.message)
    }
}

/// Everything the pipeline observed.
#[derive(Debug, Clone)]
pub struct Driven {
    /// The folded scene — what a painter would observe.
    pub scene: SceneState,
    /// Every event folded for the reads fed, in order: each life's decoded
    /// events, led by its seed when the drive is [`Drive::seeded`] and
    /// `Sight::registers` admits it.
    pub events: Vec<AgentEvent>,
    /// Every daemon envelope's presence deltas, which [`Fold::feed`] merges
    /// into `scene` on their own channel, never through the reducer.
    pub(crate) presence: Vec<PresenceMsg>,
    /// The index in `events` of each SEED, which stands in for the watcher's
    /// registration, not the wire's own output. A census reports
    /// [`Self::wire_events`]: a seed registers a slot unconditionally, so
    /// counting it scores garbage 1/1.
    pub(crate) seeds: Vec<usize>,
    /// Non-blank lines driven (blank lines are skipped, not counted).
    pub lines: usize,
    /// Lines the watcher skips too, outside the decoder contract: not UTF-8, or
    /// not JSON.
    pub unparseable: usize,
    /// Hook lines the dispatcher or a daemon's presence decoder refused. A refusal
    /// of bytes the source itself wrote is drift in our decoder.
    pub(crate) decode_errors: Vec<LineFailure>,
    /// Each slot's lifecycle classes, as each was first reached: the one record
    /// [`Self::reached`] unions.
    pub reaches: Vec<(AgentId, Reach)>,
    /// The drift breadcrumbs the decode and fold logged, each as its fields: a
    /// field whose only use is the breadcrumb its absence logs is still read.
    pub(crate) breadcrumbs: Vec<String>,
}

impl Driven {
    /// Nothing driven yet, on [`DRIVEN_DESKS`].
    fn empty() -> Self {
        Self {
            scene: SceneState::uniform(DRIVEN_DESKS),
            events: Vec::new(),
            presence: Vec::new(),
            seeds: Vec::new(),
            lines: 0,
            unparseable: 0,
            decode_errors: Vec::new(),
            reaches: Vec::new(),
            breadcrumbs: Vec::new(),
        }
    }

    /// The classes any slot passed through, in first-reached order.
    pub fn reached(&self) -> Vec<Reach> {
        let mut out = Vec::new();
        for &(_, class) in &self.reaches {
            if !out.contains(&class) {
                out.push(class);
            }
        }
        out
    }

    /// Slots the fold registered — "did the wire reach the scene at all".
    pub fn registered(&self) -> usize {
        self.scene.agents.len()
    }

    /// The first session the events start with no parent: the run's root.
    pub fn root(&self) -> Option<AgentId> {
        self.events.iter().find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: None,
                ..
            } => Some(*agent_id),
            _ => None,
        })
    }

    /// The first session the events start under a parent, and that parent.
    pub fn linked_child(&self) -> Option<(AgentId, AgentId)> {
        self.events.iter().find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: Some(parent),
                ..
            } => Some((*agent_id, *parent)),
            _ => None,
        })
    }

    /// Events the WIRE produced, excluding the registration seeds — the honest
    /// numerator for any census.
    pub fn wire_events(&self) -> usize {
        self.events.len() - self.seeds.len()
    }

    /// [`Self::events`] less its seeds, in order: what the wire alone produced.
    pub(crate) fn into_wire(self) -> impl Iterator<Item = AgentEvent> {
        let seeds = self.seeds;
        (self.events.into_iter().enumerate())
            .filter(move |(at, _)| seeds.binary_search(at).is_err())
            .map(|(_, event)| event)
    }

    /// Assert bytes WE control drove cleanly. Not for unbounded bytes (the
    /// corpus shell reports the same counts instead).
    ///
    /// # Panics
    ///
    /// If a line was not valid JSON or the decoder returned `Err`.
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
    }
}

/// The instant [`Drive::lines`] and [`Drive::body`] fold at: with one instant
/// for every event, no sweep's elapsed-time threshold can fire mid-drive and reap
/// a slot the caller is about to assert on.
const DEFAULT_NOW_EPOCH_SECS: u64 = 1_800_000_000;

fn default_now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(DEFAULT_NOW_EPOCH_SECS)
}

/// How far past `now` the cascade tick runs: past a deferred b1 cascade's
/// grace, so it fires, and inside the exit grace, so a wire carrying its own
/// `SessionEnd` still leaves the slot to observe.
const CASCADE_TICK: Duration = Duration::from_secs(3);
const _: () = assert!(
    crate::state::reducer::B1_CASCADE_GRACE.as_nanos() < CASCADE_TICK.as_nanos()
        && CASCADE_TICK.as_nanos() < crate::state::reducer::EXIT_GRACE_WINDOW.as_nanos()
);

/// PER-FLOOR desk capacity for the driven scene. Registration past the TOTAL is
/// kept unseated: in the scene but drawn on no floor, so `wire_to_pixels` would
/// measure no pixels for it.
///
/// SECOND consumer, in another crate: `wire_to_pixels` shapes its EMPTY pixel
/// baseline with this same value, so changing it moves that test's measured
/// delta.
pub const DRIVEN_DESKS: usize = 64;

/// The seed's working directory when its head names none. Production then
/// derives one from the path (`cwd_derive`, native-only) or leaves it empty, and
/// an empty cwd registers the reducer's unknown-cwd ghost, which is not what any
/// driver wants to observe.
const SEED_CWD: &str = "/pixtuoid/harness";

/// The decode-and-fold pipeline for one source's bytes.
#[derive(Debug)]
pub struct Drive {
    transport: Transport,
    decode: Decode,
    seeded: bool,
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

impl Decode {
    /// A line's events, counted in `d`: blank, `None` uncounted; not JSON or
    /// refused by the decoder, `None` counted as such; a daemon's, `None` with its
    /// presence in `d`. `parsed` reads the line before the decoder takes it.
    fn line(
        &self,
        line: Option<&str>,
        d: &mut Driven,
        parsed: impl FnOnce(&Value),
    ) -> Option<Vec<AgentEvent>> {
        if line.is_some_and(|l| l.trim().is_empty()) {
            return None;
        }
        d.lines += 1;
        let Some((line, Ok(v))) = line.map(|l| (l, serde_json::from_str::<Value>(l))) else {
            d.unparseable += 1;
            return None;
        };
        parsed(&v);
        let refused = |d: &mut Driven, e: DecodeError| {
            d.decode_errors.push(LineFailure {
                line: d.lines,
                shape: shape_of(line),
                message: e.to_string(),
            });
        };
        match self {
            Self::Transcript {
                decode,
                source,
                logical,
            } => Some(decode(logical, source, v)),
            Self::Hooks => {
                if let Some((source, presence)) = v.as_object().and_then(registry::presence_of) {
                    match presence {
                        Ok(decoded) => d.presence.extend(decoded.into_msgs(source)),
                        Err(e) => refused(d, e),
                    }
                    return None;
                }
                match decode_hook_payload(v) {
                    Ok(events) => Some(events),
                    Err(e) => {
                        refused(d, e);
                        None
                    }
                }
            }
        }
    }
}

impl Drive {
    /// [`Drive::transcript`] over the transcript at `path`.
    pub fn transcript_at(source: &str, path: &Path) -> Option<Self> {
        Self::transcript(source, &path.to_string_lossy())
    }

    /// Drive a transcript's lines through `source`'s own `LineDecoder` on
    /// `Transport::Jsonl`, keyed on `logical` normalized as the watcher
    /// normalizes a transcript path: the ONE key the decoder and the seed both
    /// read, as `walk_jsonl` hands the decoder and the id deriver one.
    ///
    /// `None` when the source is hook-only, a daemon, or unregistered: a caller
    /// that silently fell back to some other decoder would census the wrong one.
    pub fn transcript(source: &str, logical: &str) -> Option<Self> {
        Self::keyed(source, &crate::id::normalize_path_key(logical))
    }

    /// [`Drive::transcript`] on a `key` already normalized, so a test can
    /// normalize it as another host does.
    fn keyed(source: &str, key: &str) -> Option<Self> {
        let decode = registry::descriptor_for(source)?.line_decoder()?;
        Some(Self {
            transport: Transport::Jsonl,
            decode: Decode::Transcript {
                decode,
                source: source.to_string(),
                logical: key.to_string(),
            },
            seeded: false,
        })
    }

    /// Drive hook envelopes through the shared `decode_hook_payload` on
    /// `Transport::Hook`, a daemon's through its presence decoder instead. Takes NO
    /// source: the dispatcher routes by the envelope's own `_pixtuoid_source`
    /// stamp.
    ///
    /// There is no seed here by construction: a hook that may register carries
    /// its identity (`SessionStart`/`Identity`), and a seed would hide one that
    /// doesn't.
    pub fn hooks() -> Self {
        Self {
            transport: Transport::Hook,
            decode: Decode::Hooks,
            seeded: false,
        }
    }

    /// Lead each life of the transcript that `Sight::registers` admits, a life
    /// after its own end within one read included, with the `SessionStart` the
    /// watcher's `emit_registration` would emit for it, keyed by the source's
    /// registry row.
    ///
    /// Not a free choice: a transcript carrying no `SessionStart` of its own is
    /// registered in production by the watcher, so an offline driver must seed
    /// it; one whose head line IS a `SessionStart` would only get a duplicate.
    #[must_use]
    pub fn seeded(mut self) -> Self {
        self.seeded = true;
        self
    }

    /// Drive a recording's `lines` end to end, at one instant, as one live read.
    /// A caller that renders the scene feeds a [`Fold`] at its render's instant.
    pub fn lines<I, S>(&self, lines: I) -> Driven
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut fold = Fold::default();
        fold.feed(self, lines, default_now());
        fold.finish()
    }

    /// [`Self::lines`] over a recording's bytes ([`Fold::feed_body`]).
    pub fn body(&self, body: &[u8]) -> Driven {
        let mut fold = Fold::default();
        fold.feed_body(self, body, default_now());
        fold.finish()
    }

    /// The source and key a transcript drive's own agent keys on.
    fn keyed_on(&self) -> Option<(&str, &Path)> {
        let Decode::Transcript {
            source, logical, ..
        } = &self.decode
        else {
            return None;
        };
        // The id deriver reads the normalized key, as the watcher's does:
        // nothing heals a slot's session_id, so any other spelling keys a twin
        // of the hook-created slot forever.
        Some((source, Path::new(logical.as_str())))
    }

    /// The agent a transcript drive's lifecycle evidence is judged for, as the
    /// watcher's `own_agent`.
    fn own_agent(&self) -> Option<AgentId> {
        let (source, path) = self.keyed_on()?;
        Some(transcript_session(source, path, registry::id_deriver_for(source)).1)
    }

    /// The `SessionStart` the watcher's first sight emits for this transcript:
    /// its head read off `read`, or off `head`, the transcript's start, where
    /// `read` lacks what only a head carries.
    fn seed(&self, read: &[u8], head: &[u8]) -> Option<AgentEvent> {
        let (source, path) = self.keyed_on()?;
        let parent = registry::parent_extractor_for(source);
        let scan = |bytes: &[u8]| {
            // The title deriver is in the native-only `NativeTranscript` column.
            extract_head_fields(bytes, registry::cwd_extractor_for(source), None, parent)
        };
        let mut fields = scan(read);
        if fields.lack_the_head(parent) {
            fields = scan(head);
        }
        Some(first_sight_start(
            path,
            source,
            registry::id_deriver_for(source),
            fields.cwd.unwrap_or_else(|| PathBuf::from(SEED_CWD)),
            fields.parent,
        ))
    }
}

/// Lines joined back into the bytes a first sight reads.
fn joined<S: AsRef<str>>(lines: impl Iterator<Item = S>) -> Vec<u8> {
    lines
        .map(|l| l.as_ref().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

/// The harness's one reducer fold: batches of lines from any drives, each at
/// its own instant, and ticks between them.
#[derive(Debug, Clone)]
pub struct Fold {
    reducer: Reducer,
    driven: Driven,
    /// Each seeded transcript's claim, through [`Sight::registers`].
    sights: HashMap<AgentId, Sighted>,
}

/// A seeded transcript's claim and its head: the first read stands in for the
/// file head the watcher reads back.
#[derive(Debug, Clone)]
struct Sighted {
    sight: Sight,
    head: Vec<u8>,
}

/// One event a read delivers: a seed, or the wire's own.
#[derive(Debug, Clone)]
enum Delivered {
    Seed(AgentEvent),
    Wire(AgentEvent),
}

impl Delivered {
    fn event(&self) -> &AgentEvent {
        match self {
            Self::Seed(event) | Self::Wire(event) => event,
        }
    }
}

impl Default for Fold {
    fn default() -> Self {
        Self {
            reducer: Reducer::new(),
            driven: Driven::empty(),
            sights: HashMap::new(),
        }
    }
}

impl Fold {
    /// Decode `lines` through `drive` (blank lines skipped, every other line
    /// parsed and decoded) as one live read, and fold its lives at `at`.
    pub fn feed<I, S>(&mut self, drive: &Drive, lines: I, at: SystemTime)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.run(drive, lines.into_iter().map(Some), at);
    }

    /// [`Self::feed`] over a recording's bytes, cut into lines as the watcher cuts
    /// them: a line that is not UTF-8 is unparseable, and costs only itself.
    pub fn feed_body(&mut self, drive: &Drive, body: &[u8], at: SystemTime) {
        use crate::source::admit::{lines, whole};
        let lines = lines(whole(body)).map(Result::ok);
        self.run(drive, lines, at);
    }

    /// Advance to `at` with no line: past a grace window, its sweep fires.
    pub fn tick(&mut self, at: SystemTime) {
        self.reducer.tick(&mut self.driven.scene, at);
        note_reached(&self.driven.scene, &mut self.driven.reaches);
    }

    /// Everything observed so far.
    pub fn driven(&self) -> &Driven {
        &self.driven
    }

    /// Everything observed.
    pub fn finish(self) -> Driven {
        self.driven
    }

    /// Apply `delivered` at `at` as `transport` delivered it, observing the
    /// classes it reaches.
    fn apply(&mut self, transport: Transport, delivered: Delivered, at: SystemTime) {
        let event = match delivered {
            Delivered::Seed(event) => {
                self.driven.seeds.push(self.driven.events.len());
                event
            }
            Delivered::Wire(event) => event,
        };
        self.reducer
            .apply(&mut self.driven.scene, event.clone(), at, transport);
        note_reached(&self.driven.scene, &mut self.driven.reaches);
        self.driven.events.push(event);
    }

    /// The seed `drive` leads `life` with, one life of a read of `lines`, when
    /// `drive` is seeded and [`Sight::registers`] admits it, else `None`. A
    /// seeded `drive`'s claim then follows `life`'s verdict.
    fn registration<S: AsRef<str>>(
        &mut self,
        drive: &Drive,
        own: Option<AgentId>,
        lines: &[Option<S>],
        life: &[AgentEvent],
    ) -> Option<AgentEvent> {
        let id = own.filter(|_| drive.seeded)?;
        let verdict = last_life(life, id);
        let read = joined(lines.iter().flatten());
        let sighted = self.sights.get(&id);
        let registers =
            Sight::registers(sighted.map(|s| &s.sight), holds_life(life), verdict, false);
        let seed = match sighted {
            _ if !registers => None,
            // A claim re-sends the start its first sight built, as the watcher's
            // `Registration` does.
            Some(Sighted {
                sight: Sight::Claimed(claim),
                ..
            }) => Some(claim.start.clone()),
            Some(Sighted { head, .. }) => drive.seed(&read, head),
            None => drive.seed(&read, &read),
        };
        let sight = match (verdict, &seed) {
            (Some(Life::Ended), _) => Sight::Ended,
            (_, Some(start)) => Sight::Claimed(Registration {
                start: start.clone(),
                label: None,
            }),
            (_, None) => return seed,
        };
        match self.sights.entry(id) {
            Entry::Occupied(mut held) => held.get_mut().sight = sight,
            Entry::Vacant(new) => {
                let head = read.get(..HEAD_BYTES).unwrap_or(&read).to_vec();
                new.insert(Sighted { sight, head });
            }
        }
        seed
    }

    /// `None` is a line whose bytes are not UTF-8, so not JSON either.
    fn run<I, S>(&mut self, drive: &Drive, lines: I, at: SystemTime)
    where
        I: IntoIterator<Item = Option<S>>,
        S: AsRef<str>,
    {
        let own = drive.own_agent();
        let sink = BreadcrumbSink::default();
        tracing::subscriber::with_default(sink.clone(), || {
            let presence = self.driven.presence.len()..;
            let (mut read, mut decoded) = (Vec::new(), Vec::new());
            for line in lines {
                let text = line.as_ref().map(AsRef::as_ref);
                decoded.extend(
                    drive
                        .decode
                        .line(text, &mut self.driven, |_| {})
                        .unwrap_or_default(),
                );
                read.push(line);
            }
            self.send(drive, own, &read, decoded, at);
            let d = &mut self.driven;
            for msg in &d.presence[presence] {
                apply_presence(&mut d.scene, &msg.key, msg.delta.clone(), at);
            }
        });
        self.driven.breadcrumbs.extend(sink.take());
    }

    /// What one live read of `lines` delivers: each life of `decoded`
    /// ([`each_life`]) as a read of its own, led by any seed
    /// [`Self::registration`] gives it, which also keeps the claim. Folds
    /// nothing: [`Self::send`] does.
    fn deliver<S: AsRef<str>>(
        &mut self,
        drive: &Drive,
        own: Option<AgentId>,
        lines: &[Option<S>],
        decoded: Vec<AgentEvent>,
    ) -> Vec<Delivered> {
        let lives = match own {
            Some(own) => each_life(decoded, own),
            None => vec![decoded],
        };
        let mut fed = Vec::new();
        for life in lives {
            fed.extend(
                self.registration(drive, own, lines, &life)
                    .map(Delivered::Seed),
            );
            fed.extend(life.into_iter().map(Delivered::Wire));
        }
        fed
    }

    /// Fold [`Self::deliver`]'s delivery for one read.
    fn send<S: AsRef<str>>(
        &mut self,
        drive: &Drive,
        own: Option<AgentId>,
        lines: &[Option<S>],
        decoded: Vec<AgentEvent>,
        at: SystemTime,
    ) {
        for event in self.deliver(drive, own, lines, decoded) {
            self.apply(drive.transport, event, at);
            // Each event is observed BEFORE and AFTER a cascade tick, so a
            // transient class a later event resolves still counts as reached.
            self.tick(at + CASCADE_TICK);
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

/// Record the lifecycle class each live slot is in, once per slot and class:
/// two slots reaching one class each must not read as one slot reaching both.
fn note_reached(scene: &SceneState, reaches: &mut Vec<(AgentId, Reach)>) {
    for (&id, slot) in &scene.agents {
        let r = match &slot.state {
            ActivityState::Running {
                item:
                    Some(ToolUse {
                        kind: ToolKind::Task,
                        ..
                    }),
            } => Reach::Delegating,
            ActivityState::Running {
                item: Some(ToolUse { .. }),
            } => Reach::Running,
            ActivityState::Running { item: None } => Reach::RunningNoItem,
            ActivityState::RequiresAction { .. } => Reach::RequiresAction,
            ActivityState::Idle => continue,
        };
        if !reaches.contains(&(id, r)) {
            reaches.push((id, r));
        }
    }
}

/// A line's STRUCTURE — its top-level `type` and key names, never its values.
/// The one content-safe formatter for failure reporting.
fn shape_of(line: &str) -> String {
    let v = serde_json::from_str::<Value>(line).expect("drive parsed this line");
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
    fn a_daemon_envelope_reaches_the_scene_on_the_presence_channel() {
        let up = r#"{"type":"gateway_start","gatewayPort":49242,"_pixtuoid_source":"openclaw"}"#;
        let d = Drive::hooks().lines([up]);
        assert_eq!(d.scene.daemons().count(), 1, "{d:?}");
        assert!(d.events.is_empty(), "never an AgentEvent: {d:?}");
    }

    #[test]
    fn a_daemon_envelope_its_decoder_refuses_is_a_decode_error() {
        let bad = r#"{"gatewayPort":49242,"_pixtuoid_source":"openclaw"}"#;
        let d = Drive::hooks().lines([bad]);
        assert_eq!(d.decode_errors.len(), 1, "{d:?}");
        assert_eq!(d.scene.daemons().count(), 0, "{d:?}");
    }

    #[test]
    fn a_breadcrumb_a_decode_logs_is_part_of_what_it_decoded() {
        let file = sources_root()
            .join("dsh/fixtures/subagent-hermetic")
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

    /// An omp transcript line carrying only tool activity — nothing in it
    /// registers a session.
    fn tool_line() -> String {
        serde_json::json!({
            "type": "message",
            "message": {"role": "assistant", "content": [
                {"type": "toolCall", "id": "tu-1", "name": "bash", "arguments": {"command": "ls"}}
            ]}
        })
        .to_string()
    }

    const TRANSCRIPT: &str = "/h/.omp/agent/sessions/-h-p/2026-09-19T18-16-13-370Z_01a0bae2-133a-769f-851a-927926fbca45.jsonl";
    const SOURCE: &str = "omp";

    #[test]
    fn an_unseeded_activity_only_transcript_decodes_but_registers_nothing() {
        let d = Drive::transcript(SOURCE, TRANSCRIPT)
            .unwrap()
            .lines([tool_line()]);
        assert!(!d.events.is_empty(), "the line must decode");
        assert_eq!(
            d.registered(),
            0,
            "a JSONL event for an unknown id is a no-op"
        );
        assert!(d.reaches.is_empty());
    }

    #[test]
    fn the_first_sight_seed_registers_and_the_wire_drives_the_slot_running() {
        let d = Drive::transcript(SOURCE, TRANSCRIPT)
            .unwrap()
            .seeded()
            .lines([tool_line()]);
        assert_eq!(d.registered(), 1);
        assert_eq!(d.reached(), vec![Reach::Running]);
    }

    /// A claimed transcript re-registers on a feed showing life and on no
    /// other, and re-registering its live slot leaves one slot as it was.
    #[test]
    fn a_claimed_transcript_re_registers_only_on_a_feed_showing_life() {
        let drive = Drive::transcript(SOURCE, TRANSCRIPT).unwrap().seeded();
        let at = default_now();
        let title = serde_json::json!({"type": "title_change", "id": "x", "title": "Named"});
        let no_life = serde_json::json!({"type": "thinking_level_change", "thinkingLevel": "high"});
        let slots = |fold: &Fold| {
            (fold.driven().scene.agents.values())
                .map(|slot| (slot.label.clone(), slot.parent_id))
                .collect::<Vec<_>>()
        };
        let mut fold = Fold::default();
        fold.feed(&drive, [tool_line(), title.to_string()], at);
        let first = slots(&fold);
        assert!(
            matches!(&first[..], [(label, None)] if &*label.text() == "om·Named"),
            "{first:?}"
        );
        fold.feed(&drive, [no_life.to_string()], at);
        assert_eq!(fold.driven().seeds.len(), 1, "a feed showing no life");
        fold.feed(&drive, [tool_line()], at);
        assert_eq!(fold.driven().seeds.len(), 2, "a feed showing life");
        assert_eq!(slots(&fold), first);

        let rollout = "/h/.codex/sessions/2026/10/27/rollout-2026-10-27T04-47-02-019e7762-9ded-7e33-8e5c-3b1c8f6f0a02.jsonl";
        let child = Drive::transcript("codex", rollout).unwrap().seeded();
        let head = serde_json::json!({"type": "session_meta", "payload": {
            "id": "019e7762-9ded-7e33-8e5c-3b1c8f6f0a02",
            "parent_thread_id": "019e7762-9ded-7e33-8e5c-3b1c8f6f0a01",
            "cwd": "/repo",
        }});
        let call = serde_json::json!({"type": "response_item", "payload": {
            "type": "function_call", "name": "exec_command", "arguments": "{}", "call_id": "call_1",
        }});
        let mut fold = Fold::default();
        fold.feed(&child, [head.to_string(), call.to_string()], at);
        let linked = slots(&fold);
        assert!(matches!(linked[..], [(_, Some(_))]), "{linked:?}");
        fold.feed(&child, [call.to_string()], at);
        assert_eq!(fold.driven().seeds.len(), 2, "a child's feed showing life");
        assert_eq!(slots(&fold), linked);
    }

    /// An ended session's transcript showing activity again is a new life,
    /// which the watcher first-sights again.
    #[test]
    fn an_ended_transcript_showing_activity_again_is_first_sighted_again() {
        let drive = Drive::transcript(SOURCE, TRANSCRIPT).unwrap().seeded();
        let at = default_now();
        let exit = serde_json::json!({"type": "custom", "customType": "session_exit"});
        let mut fold = Fold::default();
        fold.feed(&drive, [tool_line(), exit.to_string()], at);
        let resumed = at + crate::state::reducer::EXIT_GRACE_WINDOW + CASCADE_TICK;
        fold.tick(resumed);
        assert_eq!(fold.driven().registered(), 0, "the first life walked out");
        fold.feed(&drive, [tool_line()], resumed);
        let d = fold.finish();
        assert_eq!(d.seeds.len(), 2);
        assert_eq!(d.registered(), 1, "the second life registers");
    }

    /// A trailer flushed after the end line decodes, but shows no activity: no
    /// new life.
    #[test]
    fn an_ended_transcripts_trailer_is_no_new_life() {
        let drive = Drive::transcript(SOURCE, TRANSCRIPT).unwrap().seeded();
        let at = default_now();
        let exit = serde_json::json!({"type": "custom", "customType": "session_exit"});
        let mut fold = Fold::default();
        fold.feed(&drive, [tool_line(), exit.to_string()], at);
        let after = at + crate::state::reducer::EXIT_GRACE_WINDOW + CASCADE_TICK;
        fold.tick(after);
        let fed = fold.driven().events.len();
        let trailer = serde_json::json!({"type": "thinking_level_change", "thinkingLevel": "high"});
        fold.feed(&drive, [trailer.to_string()], after);
        let d = fold.finish();
        assert!(
            d.events.len() > fed,
            "the trailer must decode, or this proves nothing"
        );
        assert_eq!(d.seeds.len(), 1);
        assert_eq!(d.registered(), 0);
    }

    /// A live batch that works and ends its session walks it in and out, as a
    /// short hook session does, its header's decoded start sent too.
    #[test]
    fn a_live_batch_that_works_and_ends_walks_in_and_out() {
        let drive = Drive::transcript(SOURCE, TRANSCRIPT).unwrap().seeded();
        let head = serde_json::json!({"type": "session", "cwd": "/repo/head"});
        let exit = serde_json::json!({"type": "custom", "customType": "session_exit"});
        let mut fold = Fold::default();
        let batch = [head.to_string(), tool_line(), exit.to_string()];
        fold.feed(&drive, batch, default_now());
        let d = fold.finish();
        let lifecycle: Vec<&str> = (d.events.iter())
            .filter_map(|e| match e {
                AgentEvent::SessionStart { .. } => Some("start"),
                AgentEvent::SessionEnd { .. } => Some("end"),
                _ => None,
            })
            .collect();
        assert_eq!(
            (lifecycle, d.seeds.len()),
            (vec!["start", "start", "end"], 1)
        );
        let walked_out = d
            .scene
            .agents
            .values()
            .all(|slot| slot.exiting_at.is_some());
        assert!(d.registered() == 1 && walked_out, "{:?}", d.scene.agents);
    }

    /// A recording reads as the watcher reads it live: each life of it registers
    /// as a read of its own, an end stays in its life, and an end alone registers
    /// nothing.
    #[test]
    fn a_recording_is_read_live_each_life_a_read_of_its_own() {
        let drive = Drive::transcript(SOURCE, TRANSCRIPT).unwrap().seeded();
        let (tool, exit, trailer) = (
            tool_line(),
            serde_json::json!({"type": "custom", "customType": "session_exit"}).to_string(),
            serde_json::json!({"type": "thinking_level_change", "thinkingLevel": "high"})
                .to_string(),
        );
        let lifecycle = |recording: &[&String]| {
            let d = drive.lines(recording);
            let starts_and_ends = (d.events.iter()).filter_map(|e| match e {
                AgentEvent::SessionStart { .. } => Some("start"),
                AgentEvent::SessionEnd { .. } => Some("end"),
                _ => None,
            });
            (starts_and_ends.collect::<Vec<_>>(), d.seeds.len())
        };
        assert_eq!(lifecycle(&[&tool, &tool]), (vec!["start"], 1));
        assert_eq!(
            lifecycle(&[&tool, &exit, &trailer]),
            (vec!["start", "end"], 1)
        );
        assert_eq!(lifecycle(&[&exit, &trailer]), (vec!["end"], 0));
        let resumed = lifecycle(&[&tool, &exit, &tool]);
        assert_eq!(resumed, (vec!["start", "end", "start"], 2));
        let lives = lifecycle(&[&tool, &exit, &tool, &exit]);
        assert_eq!(lives, (vec!["start", "end", "start", "end"], 2));
    }

    /// A recording's wire is its events less each life's seed, a seed past its
    /// first life included.
    #[test]
    fn the_wire_is_every_event_but_each_lifes_seed() {
        let exit = serde_json::json!({"type": "custom", "customType": "session_exit"});
        let recording = [tool_line(), exit.to_string(), tool_line()];
        let drive = || Drive::transcript(SOURCE, TRANSCRIPT).unwrap();
        let seeded = drive().seeded().lines(&recording);
        assert_eq!(seeded.seeds.len(), 2, "{:?}", seeded.events);
        let wire: Vec<AgentEvent> = seeded.into_wire().collect();
        assert_eq!(wire, drive().lines(&recording).events);
    }

    /// A new life's read that lacks the head's cwd reads the transcript's start
    /// back for it, as the watcher reads the file's head.
    #[test]
    fn a_new_lifes_seed_reads_back_the_transcripts_head() {
        let drive = Drive::transcript(SOURCE, TRANSCRIPT).unwrap().seeded();
        let at = default_now();
        let head = serde_json::json!({"type": "session", "cwd": "/repo/head"});
        let exit = serde_json::json!({"type": "custom", "customType": "session_exit"});
        let mut fold = Fold::default();
        fold.feed(
            &drive,
            [head.to_string(), tool_line(), exit.to_string()],
            at,
        );
        let resumed = at + crate::state::reducer::EXIT_GRACE_WINDOW + CASCADE_TICK;
        fold.tick(resumed);
        let fed = fold.driven().events.len();
        fold.feed(&drive, [tool_line()], resumed);
        let d = fold.finish();
        assert!(
            matches!(&d.events[fed], AgentEvent::SessionStart { cwd, .. } if cwd == Path::new("/repo/head")),
            "the second life's seed: {:?}",
            d.events[fed]
        );
    }

    /// One feed holding a life, its own end and a life after it registers the
    /// second life again before its events, as the watcher's read does.
    #[test]
    fn a_life_after_its_own_end_registers_again_within_one_feed() {
        let drive = Drive::transcript(SOURCE, TRANSCRIPT).unwrap().seeded();
        let exit = serde_json::json!({"type": "custom", "customType": "session_exit"});
        let mut fold = Fold::default();
        let feed = [tool_line(), exit.to_string(), tool_line()];
        fold.feed(&drive, feed, default_now());
        let d = fold.finish();
        let lifecycle: Vec<&str> = (d.events.iter())
            .filter_map(|e| match e {
                AgentEvent::SessionStart { .. } => Some("start"),
                AgentEvent::SessionEnd { .. } => Some("end"),
                AgentEvent::ToolUseStart { .. } => Some("tool"),
                _ => None,
            })
            .collect();
        let want = vec!["start", "tool", "end", "start", "tool"];
        assert_eq!((lifecycle, d.seeds.len()), (want, 2));
    }

    /// Under Windows' path-key normalization, which rewrites `TRANSCRIPT`'s
    /// case, the seed still registers the slot the tool line drives: both key
    /// on ONE normalized string.
    #[test]
    fn the_seed_and_the_decoder_share_the_normalized_key() {
        let key = crate::id::normalize_key_inner(true, TRANSCRIPT);
        assert_ne!(
            key, TRANSCRIPT,
            "a key normalization leaves alone proves nothing"
        );
        let d = Drive::keyed(SOURCE, &key)
            .unwrap()
            .seeded()
            .lines([tool_line()]);
        assert_eq!(d.registered(), 1);
        assert_eq!(d.reached(), vec![Reach::Running]);
    }

    #[test]
    fn blank_lines_are_skipped_and_non_json_counts_as_unparseable() {
        let d = Drive::transcript(SOURCE, TRANSCRIPT).unwrap().lines([
            "",
            "   ",
            "not json at all",
            "{}",
        ]);
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

    /// A hook envelope the dispatcher rejects (no `hook_event_name`), carrying
    /// prose under a key — what the failure report must NOT echo back to a terminal.
    const PROSE_LINE: &str =
        r#"{"_pixtuoid_source":"claude-code","type":"assistant","secret":"do not print me"}"#;

    #[test]
    fn a_decoder_error_is_recorded_with_the_lines_shape_not_its_content() {
        let d = Drive::hooks().lines([PROSE_LINE]);

        assert_eq!(d.decode_errors.len(), 1);
        assert_eq!(d.decode_errors[0].line, 1);
        assert_eq!(
            d.decode_errors[0].message,
            "claude-code payload has no hook_event_name"
        );
        let shape = &d.decode_errors[0].shape;
        assert!(
            !shape.contains("do not print me"),
            "the shape must carry key NAMES only, got {shape}"
        );
        assert!(shape.contains("type=\"assistant\""), "got {shape}");
        assert!(shape.contains("secret"), "got {shape}");
    }

    #[test]
    fn wire_events_excludes_the_seed_so_garbage_scores_zero() {
        let garbage = ["not json at all", r#"{"totally":"unrelated"}"#];
        let d = Drive::transcript(SOURCE, TRANSCRIPT)
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

        let d = Drive::transcript(SOURCE, TRANSCRIPT)
            .unwrap()
            .seeded()
            .lines([tool_line()]);
        assert_eq!(d.seeds.len(), 1);
        assert_eq!(d.wire_events(), d.events.len() - 1);

        let d = Drive::transcript(SOURCE, TRANSCRIPT)
            .unwrap()
            .lines([tool_line()]);
        assert_eq!(d.seeds.len(), 0);
        assert_eq!(d.wire_events(), d.events.len());
    }

    #[test]
    fn line_failure_display_carries_position_and_shape_not_content() {
        let d = Drive::hooks().lines([PROSE_LINE]);
        let rendered = d.decode_errors[0].to_string();
        assert!(rendered.starts_with("line 1: "), "got {rendered}");
        assert!(rendered.contains("type=\"assistant\""), "got {rendered}");
        assert!(
            rendered.ends_with(": claude-code payload has no hook_event_name"),
            "got {rendered}"
        );
        assert!(
            !rendered.contains("do not print me"),
            "Display must not echo the line's values, got {rendered}"
        );
    }

    #[test]
    fn a_transient_class_resolved_by_a_later_line_still_counts_as_reached() {
        let start = tool_line();
        let end = serde_json::json!({
            "type": "message",
            "message": {"role": "toolResult", "toolCallId": "tu-1"}
        })
        .to_string();
        let d = Drive::transcript(SOURCE, TRANSCRIPT)
            .unwrap()
            .seeded()
            .lines([start, end]);
        assert!(
            d.reached().contains(&Reach::Running),
            "the resolved tool call must still count as reached, got {:?}",
            d.reaches
        );
    }
}
