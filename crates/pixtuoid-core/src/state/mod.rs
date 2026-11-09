use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::id::AgentId;

mod correlation;
mod fsm;
/// The event coordinator: [`reducer::Reducer`] folds `AgentEvent`s into `SceneState`.
pub mod reducer;
mod requests;
mod scope;
mod turn_key;
mod turns;
pub use turn_key::TurnKey;

/// Maximum number of office floors a `SceneState` tracks.
pub const MAX_FLOORS: usize = 10;

// serde has no blanket `Arc<T>` impl, and its opt-in `rc` feature wouldn't
// cover `Arc<Path>` anyway (no `Box<Path>: Deserialize`), so the snapshot
// crosses through an owned `String` / `PathBuf`.
mod arc_str_serde {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(v: &Arc<str>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(v)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<str>, D::Error> {
        Ok(Arc::from(String::deserialize(d)?.as_str()))
    }
}

mod opt_arc_str_serde {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<S: Serializer>(v: &Option<Arc<str>>, s: S) -> Result<S::Ok, S::Error> {
        v.as_deref().serialize(s)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Arc<str>>, D::Error> {
        Ok(Option::<String>::deserialize(d)?.map(|s| Arc::from(s.as_str())))
    }
}

mod arc_path_serde {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<S: Serializer>(v: &Arc<Path>, s: S) -> Result<S::Ok, S::Error> {
        let p: &Path = v;
        p.serialize(s)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<Path>, D::Error> {
        Ok(Arc::from(PathBuf::deserialize(d)?.as_path()))
    }
}

/// Floor-local desk index — indexes a single floor's
/// `SceneLayout::home_desks` (see `SceneLayout::home_desk`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FloorLocalDeskIndex(pub usize);

/// A desk on one floor. Floor-local, never an office-wide index: a desk keeps
/// its index when another floor's capacity changes. Ordered floor first, the
/// order desks are handed out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FloorDesk {
    /// The floor, `0..MAX_FLOORS`.
    pub floor: usize,
    /// The desk's index on that floor.
    pub index: FloorLocalDeskIndex,
}

impl FloorDesk {
    /// Desk `index` of floor `floor`.
    pub const fn new(floor: usize, index: usize) -> Self {
        Self {
            floor,
            index: FloorLocalDeskIndex(index),
        }
    }
}

/// Semantic category of the tool a `Running` slot is running, derived ONCE at
/// slot entry ([`ToolKind::from_detail`]) so downstream deciders — the
/// reducer's stale-window policy and the painter's monitor-glow tint — match on
/// a typed kind instead of re-parsing the human-facing `detail` string.
///
/// Deliberately NOT `#[non_exhaustive]`: the painter's glow map matches every
/// variant, so adding a kind is a compile error there rather than a silent
/// fall into a wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    /// Subagent dispatch (the typed `ToolDetail::Task`, displayed "Delegating").
    Task,
    /// Edit / Write / MultiEdit.
    Edit,
    /// Read.
    Read,
    /// Bash.
    Bash,
    /// Grep / Glob.
    Search,
    /// Anything else, including a detail-less Running.
    Other,
}

impl ToolKind {
    /// The one production derivation, run by the reducer at slot entry.
    pub fn from_detail(detail: &crate::source::ToolDetail) -> Self {
        match detail {
            crate::source::ToolDetail::Task => ToolKind::Task,
            crate::source::ToolDetail::Generic { display } => Self::from_display(display),
        }
    }

    /// The Generic-display half: first alphanumeric token → kind. Deliberately
    /// has NO `"Agent" | "Task" | "Delegating"` arm — delegation is carried by
    /// the typed `ToolDetail::Task`, and a Generic tool merely spelling those
    /// words must not inherit delegation policy (the stale-window carve-out).
    pub fn from_display(display: &str) -> Self {
        match display
            .split_once(|c: char| !c.is_alphanumeric())
            .map_or(display, |(head, _)| head)
        {
            "Edit" | "Write" | "MultiEdit" => ToolKind::Edit,
            "Read" => ToolKind::Read,
            "Bash" => ToolKind::Bash,
            "Grep" | "Glob" => ToolKind::Search,
            _ => ToolKind::Other,
        }
    }
}

/// What an agent slot is doing right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityState {
    /// Nothing the wire shows in flight.
    Idle,
    /// Working: a turn is open, or an item runs.
    Running {
        /// The work in flight; `None` while an open turn shows none.
        item: Option<ToolUse>,
    },
    /// Blocked on a permission/input prompt.
    RequiresAction {
        /// Why the agent requires action (the prompt reason), when the wire gave one.
        #[serde(with = "opt_arc_str_serde")]
        reason: Option<Arc<str>>,
    },
}

/// A tool call inside a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUse {
    /// The call's id (pairs Start↔End).
    #[serde(with = "opt_arc_str_serde")]
    pub tool_use_id: Option<Arc<str>>,
    /// The tool's display detail, when known.
    #[serde(with = "opt_arc_str_serde")]
    pub detail: Option<Arc<str>>,
    /// The bucketed tool kind (drives the per-tool glow/tally).
    pub kind: ToolKind,
}

/// How an [`AgentSlot`]'s display label came to be — recorded at mint time so
/// whether a later label replaces it ([`SlotLabel::yields_to`])
/// doesn't rest on string-shape sniffing. One variant per REAL mint site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum LabelProvenance {
    /// Monotonic `{prefix}#N` ordinal minted with no cwd — no information, upgradable.
    OrdinalGhost,
    /// Bare registry prefix from an empty-cwd deriver fallback — upgradable.
    PrefixFallback,
    /// Derived from the cwd basename (`cc·repo`-style) — real information.
    CwdDerived,
    /// Externally supplied display name (subagent name, a rename) — real information.
    Renamed,
}

/// An [`AgentSlot`]'s display label + the provenance it was minted with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotLabel {
    #[serde(with = "arc_str_serde")]
    text: Arc<str>,
    provenance: LabelProvenance,
}

impl SlotLabel {
    /// The separator between a label's source prefix and the rest of it
    /// (`cc·repo`).
    pub const SEP: char = '·';

    /// A label's text: `prefix`, [`SEP`](Self::SEP), then `rest`, joined here
    /// alone so every writer spells the separator one way.
    pub(crate) fn join(prefix: &str, rest: &str) -> String {
        format!("{prefix}{}{rest}", Self::SEP)
    }

    pub(crate) fn new(text: impl Into<Arc<str>>, provenance: LabelProvenance) -> Self {
        Self {
            text: text.into(),
            provenance,
        }
    }

    pub(crate) fn ordinal_ghost(text: impl Into<Arc<str>>) -> Self {
        Self::new(text, LabelProvenance::OrdinalGhost)
    }

    pub(crate) fn prefix_fallback(text: impl Into<Arc<str>>) -> Self {
        Self::new(text, LabelProvenance::PrefixFallback)
    }

    pub(crate) fn cwd_derived(text: impl Into<Arc<str>>) -> Self {
        Self::new(text, LabelProvenance::CwdDerived)
    }

    pub(crate) fn renamed(text: impl Into<Arc<str>>) -> Self {
        Self::new(text, LabelProvenance::Renamed)
    }

    /// The label text as a shared handle (for cheap clones into UI rows).
    pub fn text(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }

    #[cfg(test)]
    pub(crate) fn provenance(&self) -> LabelProvenance {
        self.provenance
    }

    /// Whether this label is a placeholder, which any later label replaces.
    pub(crate) fn is_upgradable(&self) -> bool {
        matches!(
            self.provenance,
            LabelProvenance::OrdinalGhost | LabelProvenance::PrefixFallback
        )
    }

    /// Whether `incoming` replaces this label: a rename's own name replaces
    /// any, and anything else only a placeholder.
    pub(crate) fn yields_to(&self, incoming: &Self) -> bool {
        incoming.provenance == LabelProvenance::Renamed || self.is_upgradable()
    }
}

impl std::ops::Deref for SlotLabel {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl AsRef<str> for SlotLabel {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for SlotLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// Test-fixture convenience: a plain string reads as a non-upgradable `Renamed`
/// label. Production mint sites use the explicit constructors.
impl From<&str> for SlotLabel {
    fn from(text: &str) -> Self {
        Self::renamed(text)
    }
}

impl From<String> for SlotLabel {
    fn from(text: String) -> Self {
        Self::renamed(text)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One live agent's full state: identity, working directory, current activity,
/// desk/floor assignment, and the per-session meters. The Arc-backed
/// strings/paths keep the per-frame `SceneState` clone allocation-free.
pub struct AgentSlot {
    /// This agent's stable identity — the `SceneState::agents` map key.
    pub agent_id: AgentId,
    /// Registry name of the source that produced this agent (e.g. `claude-code`, `codex`).
    #[serde(with = "arc_str_serde")]
    pub source: Arc<str>,
    /// Source-native session identifier this slot is keyed under.
    #[serde(with = "arc_str_serde")]
    pub session_id: Arc<str>,
    /// The agent's working directory; its basename drives the derived label.
    #[serde(with = "arc_path_serde")]
    pub cwd: Arc<Path>,
    /// Display name + how it was derived (see `SlotLabel`).
    pub label: SlotLabel,
    /// Current activity — Idle, Running (running a tool), or RequiresAction.
    pub state: ActivityState,
    /// Wall-clock time the current `state` was entered (reset on every state
    /// change).
    pub state_started_at: SystemTime,
    /// Wall-clock time of the most recent event (any type) from this agent —
    /// the stale-agent sweep's primary liveness signal.
    pub last_event_at: SystemTime,
    /// Wall-clock time the slot was first created: its age, unaffected by later
    /// state transitions.
    pub created_at: SystemTime,
    /// The entry walk's anchor; `None` seats the slot at its desk from its first
    /// frame.
    pub arrived_at: Option<SystemTime>,
    /// Set when `SessionEnd` arrived but the slot is held alive long enough for
    /// the exit animation to play.
    pub exiting_at: Option<SystemTime>,
    /// Its desk; `None` while it waits for a free one, counted but drawn
    /// nowhere. Only a new layout ([`SceneState::set_capacities`]) moves it.
    pub home_desk: Option<FloorDesk>,
    /// Monotonic count of tool calls this agent has made this session.
    pub tool_call_count: u32,
    /// Cumulative milliseconds spent in the `Running` state.
    pub running_ms: u64,
    /// Whether `cwd` is a placeholder rather than a real working directory.
    pub unknown_cwd: bool,
    /// The dispatching parent, for a subagent slot (`None` for a top-level session).
    pub parent_id: Option<AgentId>,
    /// The agent process's pid + recycle marker — the focus-jump channel for
    /// hook-only sources, refreshed per event and never downgraded to `None`.
    /// The click-time guard re-reads the marker and refuses a recycled pid
    /// (#527). Transcript-family sources stay `None` here — their pid channel
    /// is the liveness probe, queried at click time.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pid: Option<crate::source::PidIdentity>,
    /// The RAW model string last observed on this agent's wire —
    /// last-seen-wins, so a mid-session `/model` switch tracks; this stays
    /// uninterpreted wire truth.
    #[serde(
        with = "opt_arc_str_serde",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub model: Option<Arc<str>>,
    /// The RAW effort observation last seen. The scene layer shows the value
    /// only within its TTL — no sighting means it drops off.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub effort: Option<EffortObservation>,
    /// Session-cumulative FRESH tokens (new input + cache writes + output —
    /// cache READS excluded), accumulated from `AgentEvent::Usage` deltas. Flat
    /// (not inside `last_usage`) on purpose: a monotone accumulator like
    /// `tool_call_count`, independent of any one reading.
    #[serde(skip_serializing_if = "u64_is_zero", default)]
    pub tokens_used: u64,
    /// The most recent Usage reading (see [`UsageObservation`]).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_usage: Option<UsageObservation>,
}

impl AgentSlot {
    /// Whether its exit walk has had [`EXIT_GRACE_WINDOW`](reducer::EXIT_GRACE_WINDOW)
    /// to reach the door, so its slot may go.
    pub(crate) fn walked_out(&self, now: SystemTime) -> bool {
        self.exiting_at
            .is_some_and(|t| correlation::elapsed_past(now, t, reducer::EXIT_GRACE_WINDOW))
    }
}

fn u64_is_zero(v: &u64) -> bool {
    *v == 0
}

/// A RAW effort string + WHEN it was last observed — the freshness the scene
/// layer's effort TTL reads (see `AgentSlot::effort`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EffortObservation {
    /// The RAW effort string as observed on the wire (uninterpreted).
    #[serde(with = "arc_str_serde")]
    pub value: Arc<str>,
    /// When `value` was last observed — the effort's freshness clock.
    pub seen_at: SystemTime,
}

impl EffortObservation {
    /// Bundle a raw effort string with the time it was observed.
    pub fn new(value: Arc<str>, seen_at: SystemTime) -> Self {
        Self { value, seen_at }
    }
}

/// The most recent `AgentEvent::Usage` reading — its SIZE and its apply time
/// in ONE struct, so a half-stamped reading (a delta with no time, a time with
/// no delta) is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct UsageObservation {
    /// Fresh tokens in this one reading (new input + cache writes + output).
    pub delta: u64,
    /// When this reading was applied — the falling-sheet window anchor.
    pub seen_at: SystemTime,
}

impl UsageObservation {
    /// Bundle a fresh-token `delta` with the time it was applied.
    pub fn new(delta: u64, seen_at: SystemTime) -> Self {
        Self { delta, seen_at }
    }
}

/// What every agent did on one local day, counted as it happened while pixtuoid
/// ran: the increases of [`AgentSlot::tokens_used`] and
/// [`AgentSlot::tool_call_count`], never a replayed transcript's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Today {
    /// The date the count is for, on the counting reducer's own calendar.
    pub day: NaiveDate,
    /// Fresh tokens, the [`AgentSlot::tokens_used`] measure.
    pub tokens: u64,
    /// Tool calls, the only number a CLI with no [`Usage`](crate::AgentEvent::Usage) has.
    pub tool_calls: u64,
}

impl Today {
    /// A count for `day`.
    pub fn new(day: NaiveDate, tokens: u64, tool_calls: u64) -> Self {
        Self {
            day,
            tokens,
            tool_calls,
        }
    }
}

/// Liveness of a daemon-style source (the OpenClaw gateway), driving the
/// wandering lobster mascot. PER-INSTANCE (see [`DaemonInstanceId`]) — one
/// gateway going Down says nothing about its siblings. `Down` is distinct from
/// *absent* (no roster entry): absent = never observed / plugin not loaded;
/// `Down` = this instance was seen and then died.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonState {
    /// Alive with no turn in flight — the mascot ambles.
    Idle,
    /// ≥1 turn in flight (projected from `DaemonPresence::in_flight_turns`).
    Running,
    /// Gateway is UP but every turn fails on its model backend (auth revoked /
    /// provider down) — alive-but-broken, NOT idle. Self-heals on the next
    /// successful turn, a new turn start, or a gateway restart.
    Degraded,
    /// Seen and then died — the mascot walks out (distinct from *absent*).
    Down,
}

/// The two ORTHOGONAL liveness axes a daemon mascot actually STORES. The
/// remaining render distinction — Idle vs Running — is deliberately NOT a field
/// here: it is a pure function of [`DaemonPresence::in_flight_turns`], projected
/// by [`DaemonPresence::display_state`], so "running" can never drift from the turn
/// set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonLiveness {
    /// The gateway is alive.
    Up {
        /// Alive-but-broken: every model turn is failing, so the mascot renders
        /// distressed.
        degraded: bool,
    },
    /// The gateway was seen and then died (the mascot walks out). Distinct from
    /// *absent* (no map entry = never configured / plugin not loaded).
    Down,
}

impl DaemonLiveness {
    /// The healthy alive state (`Up { degraded: false }`).
    pub const UP: DaemonLiveness = DaemonLiveness::Up { degraded: false };
}

/// One daemon INSTANCE's stable identity — the inner key of
/// [`SceneState::daemons`], so N concurrently-running instances of ONE daemon
/// source each earn their own mascot instead of collapsing onto the source name.
///
/// OPAQUE to the shared daemon layer BY DESIGN: only a source's own wire decoder
/// mints one, so "what makes two instances different" stays source knowledge.
/// STABLE across a restart of the same logical instance; the PROCESS incarnation
/// is separate state ([`DaemonPresence::current_pid`]), which is what makes a
/// stale exit receipt for the old process a no-op instead of a kill of its
/// replacement.
///
/// Deserialization routes through [`DaemonInstanceId::new`] via `try_from`
/// rather than the derive: a derived impl would reconstruct the blank id that
/// `new` exists to refuse.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct DaemonInstanceId(String);

impl TryFrom<String> for DaemonInstanceId {
    type Error = &'static str;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::new(raw).ok_or("a daemon instance id must not be blank")
    }
}

impl DaemonInstanceId {
    /// Mint an instance id, refusing an empty/whitespace-only one: a blank key
    /// IS the source-wide bucket this type exists to remove.
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        (!raw.trim().is_empty()).then_some(Self(raw))
    }

    /// The id of a daemon listening on `port`; a port always renders digits, so
    /// there is no blank case to refuse.
    pub fn from_port(port: std::num::NonZeroU16) -> Self {
        Self(port.to_string())
    }

    /// The id as its wire/display string (an OpenClaw gateway port, `"18789"`).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DaemonInstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-daemon-INSTANCE presence for the gateway mascot, carried on `SceneState`
/// so the serializable scene snapshot the renderer reads holds the mascot's
/// state + concurrency (bubble) intensity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonPresence {
    /// The stored liveness axes; the 4-way render state is PROJECTED via
    /// [`display_state`](Self::display_state), never stored.
    pub liveness: DaemonLiveness,
    /// Concurrent sessions the gateway is multiplexing (bubble intensity).
    pub active_sessions: u32,
    /// Last time ANY presence event arrived — drives the running→idle decay and
    /// the presence-TTL stale-down sweep, and anchors the leave animation
    /// (under `Down` this is the moment the gateway died).
    pub last_seen: SystemTime,
    /// When the gateway first appeared (absent/Down → up) — the enter-animation
    /// anchor and the steady wander clock.
    pub entered_at: SystemTime,
    /// In-flight turns (running iff non-empty), each keyed by its correlation key
    /// and stamped with its LAST observation. The per-turn stamp is what makes
    /// the running-decay honest: the daemon-wide `last_seen` is refreshed by ANY
    /// event, so on a gateway still serving other traffic a turn whose
    /// `agent_end` was dropped would never age out and the mascot would latch
    /// Running forever.
    ///
    /// NOT serialized: transient process state a restart resets, so a restored
    /// dump can't strand a perpetual Running.
    #[serde(skip)]
    pub in_flight_turns: HashMap<TurnKey, SystemTime>,
    /// The gateway pid currently armed for `ExitWatch` (None until first seen).
    pub current_pid: Option<i32>,
}

impl DaemonPresence {
    /// The 4-way render vocabulary ([`DaemonState`]) projected from the stored
    /// axes — the SINGLE place the `Degraded > Running > Idle` priority is encoded.
    /// `degraded` is checked BEFORE the turn set, so a degraded gateway renders
    /// Degraded even with turns still in flight.
    pub fn display_state(&self) -> DaemonState {
        match self.liveness {
            DaemonLiveness::Down => DaemonState::Down,
            DaemonLiveness::Up { degraded: true } => DaemonState::Degraded,
            DaemonLiveness::Up { degraded: false } => {
                if self.in_flight_turns.is_empty() {
                    DaemonState::Idle
                } else {
                    DaemonState::Running
                }
            }
        }
    }

    /// Whether the mascot renders as Running (alive, not degraded, ≥1 turn in flight).
    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.display_state() == DaemonState::Running
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// The whole-office snapshot the reducer maintains and the render layer reads:
/// live agent slots, per-floor desk capacities, and the daemon mascots.
pub struct SceneState {
    /// Live agent slots, keyed by `AgentId`.
    pub agents: BTreeMap<AgentId, AgentSlot>,
    /// Desk capacity per floor, indexed by floor (`0..MAX_FLOORS`). Private,
    /// so a home desk past its floor's capacity can't outlive
    /// [`Self::set_capacities`].
    floor_capacities: [usize; MAX_FLOORS],
    /// Daemon-style sources rendered as wandering mascots, keyed source →
    /// instance so instance A's stop/expiry can't touch instance B. Empty for
    /// an all-agent scene.
    ///
    /// PRIVATE with READ-ONLY accessors on purpose: mutation must stay inside
    /// the daemon layer's apply/sweep/mark entry points, or
    /// [`RemoteOffice`](crate::remote::RemoteOffice)'s copy of a server's roster,
    /// so nothing else can invent presence.
    #[serde(default)]
    pub(crate) daemons: DaemonRoster,
    /// The day's count, from a [`Reducer`](reducer::Reducer) built
    /// [`counting`](reducer::Reducer::counting); `None` while its day has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today: Option<Today>,
}

impl SceneState {
    /// Every daemon mascot as `(source, instance, presence)`, in deterministic
    /// (source, instance) order.
    pub fn daemons(&self) -> impl Iterator<Item = (&str, &DaemonInstanceId, &DaemonPresence)> + '_ {
        self.daemons.iter()
    }

    /// One exact daemon instance's presence, if present.
    pub fn daemon(&self, source: &str, instance: &DaemonInstanceId) -> Option<&DaemonPresence> {
        self.daemons.get(source, instance)
    }

    /// Copy another scene's whole daemon roster over this one's — the per-floor
    /// projection's one mutation (daemons are office-global, projected onto the
    /// ground floor). `#[doc(hidden)]`: workspace-internal, not stable API.
    #[doc(hidden)]
    pub fn clone_daemons_from(&mut self, other: &SceneState) {
        self.daemons.clone_from(&other.daemons);
    }

    /// Place one exact daemon instance's presence verbatim. `#[doc(hidden)]`: the
    /// workspace-internal FIXTURE seam for a presence with a chosen `entered_at`
    /// / turn set, which `apply_presence` can only stamp as "now". PRODUCTION
    /// mutation goes through `daemon::apply_presence` and the sweeps.
    #[doc(hidden)]
    pub fn insert_daemon(
        &mut self,
        source: &str,
        instance: DaemonInstanceId,
        presence: DaemonPresence,
    ) {
        self.daemons.insert(source, instance, presence);
    }
}

/// The `source → instance → presence` roster behind [`SceneState::daemons`], a
/// named type so the nesting lives in ONE place and the mutation ops stay
/// `pub(crate)` to the daemon layer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct DaemonRoster(BTreeMap<String, BTreeMap<DaemonInstanceId, DaemonPresence>>);

impl DaemonRoster {
    fn iter(&self) -> impl Iterator<Item = (&str, &DaemonInstanceId, &DaemonPresence)> + '_ {
        self.0.iter().flat_map(|(source, instances)| {
            instances
                .iter()
                .map(move |(instance, presence)| (source.as_str(), instance, presence))
        })
    }

    fn get(&self, source: &str, instance: &DaemonInstanceId) -> Option<&DaemonPresence> {
        self.0.get(source)?.get(instance)
    }

    pub(crate) fn insert(
        &mut self,
        source: &str,
        instance: DaemonInstanceId,
        presence: DaemonPresence,
    ) {
        self.0
            .entry(source.to_string())
            .or_default()
            .insert(instance, presence);
    }

    pub(crate) fn get_mut(
        &mut self,
        source: &str,
        instance: &DaemonInstanceId,
    ) -> Option<&mut DaemonPresence> {
        self.0.get_mut(source)?.get_mut(instance)
    }

    pub(crate) fn get_or_insert_with(
        &mut self,
        source: &str,
        instance: &DaemonInstanceId,
        make: impl FnOnce() -> DaemonPresence,
    ) -> &mut DaemonPresence {
        self.0
            .entry(source.to_string())
            .or_default()
            .entry(instance.clone())
            .or_insert_with(make)
    }

    pub(crate) fn instances_of_mut(
        &mut self,
        source: &str,
    ) -> impl Iterator<Item = (&DaemonInstanceId, &mut DaemonPresence)> + use<'_> {
        self.0
            .get_mut(source)
            .into_iter()
            .flat_map(|m| m.iter_mut())
    }

    pub(crate) fn remove_instances(&mut self, source: &str, doomed: &[DaemonInstanceId]) {
        let Some(instances) = self.0.get_mut(source) else {
            return;
        };
        for id in doomed {
            instances.remove(id);
        }
        if instances.is_empty() {
            self.0.remove(source);
        }
    }
}

impl Default for SceneState {
    fn default() -> Self {
        Self::new([0; MAX_FLOORS])
    }
}

impl SceneState {
    /// An empty scene with the given per-floor desk capacities.
    pub fn new(floor_capacities: [usize; MAX_FLOORS]) -> Self {
        Self {
            agents: BTreeMap::new(),
            floor_capacities,
            daemons: DaemonRoster::default(),
            today: None,
        }
    }

    /// A scene with the same desk capacity on every floor.
    pub fn uniform(cap: usize) -> Self {
        Self::new([cap; MAX_FLOORS])
    }

    /// Desk capacity per floor, indexed by floor.
    pub fn floor_capacities(&self) -> &[usize; MAX_FLOORS] {
        &self.floor_capacities
    }

    /// Total desk count across all floors (sum of `floor_capacities`).
    pub fn total_capacity(&self) -> usize {
        self.floor_capacities.iter().sum()
    }

    /// Take a new layout. An agent keeps its home desk while its floor still
    /// has that desk; whoever loses one joins those waiting for one.
    pub fn set_capacities(&mut self, capacities: [usize; MAX_FLOORS]) {
        self.floor_capacities = capacities;
        for slot in self.agents.values_mut() {
            slot.home_desk = (slot.home_desk)
                .filter(|desk| (capacities.get(desk.floor)).is_some_and(|&cap| desk.index.0 < cap));
        }
        self.assign_waiting(|_| true);
    }

    /// Give each agent waiting for a desk the first free one: those staying
    /// before those walking out, then oldest first, as the reducer seats
    /// arrivals. A desk is free while no slot that `holds` sits at it.
    pub(crate) fn assign_waiting(&mut self, holds: impl Fn(&AgentSlot) -> bool) {
        let taken: BTreeSet<FloorDesk> = (self.agents.values())
            .filter(|slot| holds(slot))
            .filter_map(|slot| slot.home_desk)
            .collect();
        let capacities = self.floor_capacities;
        let mut free = (0..MAX_FLOORS)
            .flat_map(|floor| (0..capacities[floor]).map(move |index| FloorDesk::new(floor, index)))
            .filter(|desk| !taken.contains(desk));
        let mut waiting: Vec<&mut AgentSlot> = (self.agents.values_mut())
            .filter(|slot| slot.home_desk.is_none())
            .collect();
        waiting.sort_by_key(|slot| (slot.exiting_at.is_some(), slot.created_at, slot.agent_id));
        for slot in waiting {
            slot.home_desk = free.next();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_used_is_omitted_only_when_it_is_actually_zero() {
        let id = AgentId::from_transcript_path("/p/tok.jsonl");
        let mut slot = make_slot(id, 0);

        let zero = serde_json::to_string(&slot).expect("serialize");
        assert!(
            !zero.contains("tokens_used"),
            "a zero accumulator stays off the wire, got {zero}"
        );

        slot.tokens_used = 7;
        let seven = serde_json::to_string(&slot).expect("serialize");
        assert!(
            seven.contains(r#""tokens_used":7"#),
            "a NONZERO accumulator must serialize, got {seven}"
        );
        let back: AgentSlot = serde_json::from_str(&seven).expect("deserialize");
        assert_eq!(back.tokens_used, 7);
    }

    fn make_slot(id: AgentId, desk: usize) -> AgentSlot {
        let now = SystemTime::now();
        AgentSlot {
            agent_id: id,
            source: Arc::from("cc"),
            session_id: Arc::from("s0"),
            cwd: Arc::from(Path::new("/repo")),
            label: "a0".into(),
            state: ActivityState::Idle,
            state_started_at: now,
            created_at: now,
            arrived_at: Some(now),
            last_event_at: now,
            exiting_at: None,
            home_desk: Some(FloorDesk::new(0, desk)),
            tool_call_count: 0,
            running_ms: 0,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        }
    }

    #[test]
    fn scene_state_json_round_trips_losslessly() {
        // The tree has no PartialEq (deliberate), so round-trip stability is
        // asserted via canonical-JSON equality.
        let mut s = SceneState::uniform(8);

        let a = AgentId::from_transcript_path("/p/a.jsonl");
        let mut slot_a = make_slot(a, 0);
        slot_a.state = ActivityState::Running {
            item: Some(ToolUse {
                tool_use_id: Some(Arc::from("tuid-1")),
                detail: Some(Arc::from("Read · src/main.rs")),
                kind: ToolKind::Read,
            }),
        };
        s.agents.insert(a, slot_a);

        let b = AgentId::from_transcript_path("/p/b.jsonl");
        let mut slot_b = make_slot(b, 1);
        slot_b.state = ActivityState::RequiresAction {
            reason: Some(Arc::from("permission: Bash")),
        };
        slot_b.parent_id = Some(a);
        s.agents.insert(b, slot_b);

        // Idle is a unit variant today; pinning it catches a future Idle field
        // reshaping the wire form from `"Idle"` to `{"Idle": {..}}`.
        let c = AgentId::from_transcript_path("/p/c.jsonl");
        s.agents.insert(c, make_slot(c, 2));

        let json = serde_json::to_string(&s).expect("serialize");
        let back: SceneState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            json,
            serde_json::to_string(&back).expect("re-serialize"),
            "round-trip must be byte-stable"
        );
        assert_eq!(back.agents.len(), 3);
        assert!(matches!(
            back.agents[&a].state,
            ActivityState::Running { .. }
        ));
        assert_eq!(back.agents[&c].state, ActivityState::Idle);
        assert_eq!(&*back.agents[&a].cwd, Path::new("/repo"));
        assert_eq!(back.agents[&b].parent_id, Some(a));
    }

    #[test]
    fn daemon_presence_round_trips_and_skips_in_flight_keys() {
        let p = DaemonPresence {
            liveness: DaemonLiveness::UP,
            active_sessions: 3,
            last_seen: SystemTime::now(),
            entered_at: SystemTime::now(),
            in_flight_turns: ["run-1", "run-2"]
                .into_iter()
                .map(|k| (TurnKey::Id(k.into()), SystemTime::now()))
                .collect(),
            current_pid: Some(4242),
        };
        assert_eq!(
            p.display_state(),
            DaemonState::Running,
            "a non-empty turn set reads Running before serialization"
        );
        let json = serde_json::to_string(&p).expect("serialize");
        assert!(
            !json.contains("run-1"),
            "in_flight_turns must be skipped on the wire: {json}"
        );
        let back: DaemonPresence = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.liveness, DaemonLiveness::UP);
        assert_eq!(
            back.display_state(),
            DaemonState::Idle,
            "skipped turn set restores empty ⇒ Idle, never a stranded Running"
        );
        assert_eq!(back.active_sessions, 3);
        assert_eq!(back.current_pid, Some(4242));
        assert!(
            back.in_flight_turns.is_empty(),
            "skipped field restores empty"
        );

        let mut q = back;
        for liveness in [
            DaemonLiveness::UP,
            DaemonLiveness::Up { degraded: true },
            DaemonLiveness::Down,
        ] {
            q.liveness = liveness;
            let j = serde_json::to_string(&q).unwrap();
            assert_eq!(
                serde_json::from_str::<DaemonPresence>(&j).unwrap().liveness,
                liveness
            );
        }
    }

    #[test]
    fn scene_state_daemons_round_trips() {
        let mut s = SceneState::uniform(8);
        let inst = DaemonInstanceId::new("18789").expect("non-empty");
        s.daemons
            .get_or_insert_with("openclaw", &inst, || DaemonPresence {
                liveness: DaemonLiveness::UP,
                active_sessions: 0,
                last_seen: SystemTime::now(),
                entered_at: SystemTime::now(),
                in_flight_turns: Default::default(),
                current_pid: None,
            })
            .current_pid = Some(900);
        let json = serde_json::to_string(&s).expect("serialize");
        let back: SceneState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            json,
            serde_json::to_string(&back).expect("re-serialize"),
            "round-trip must be byte-stable"
        );
        let p = back
            .daemon("openclaw", &inst)
            .expect("instance round-trips");
        assert_eq!(p.liveness, DaemonLiveness::UP);
        assert_eq!(p.current_pid, Some(900));
    }

    #[test]
    fn daemon_instance_id_refuses_a_blank_identity() {
        assert!(DaemonInstanceId::new("").is_none());
        assert!(DaemonInstanceId::new("   ").is_none());
        assert_eq!(
            DaemonInstanceId::new("18789").map(|i| i.as_str().to_string()),
            Some("18789".to_string())
        );
    }

    #[test]
    fn a_blank_daemon_instance_id_cannot_be_deserialized_back_in() {
        for blank in ["\"\"", "\"   \"", "\"\\t\""] {
            assert!(
                serde_json::from_str::<DaemonInstanceId>(blank).is_err(),
                "a blank id must not deserialize: {blank}"
            );
        }
        let id = DaemonInstanceId::new("18789").expect("non-blank");
        let json = serde_json::to_string(&id).expect("serializes");
        assert_eq!(json, "\"18789\"");
        assert_eq!(
            serde_json::from_str::<DaemonInstanceId>(&json).expect("round-trips"),
            id
        );
    }

    #[test]
    fn daemon_instance_id_displays_exactly_its_str() {
        for raw in ["18789", "19789", " 18789 "] {
            let id = DaemonInstanceId::new(raw).expect("non-blank");
            assert_eq!(
                id.to_string(),
                id.as_str(),
                "Display and as_str must not diverge"
            );
            assert_eq!(format!("{id}"), raw, "and neither may reformat the raw id");
        }
    }

    fn presence_at(liveness: DaemonLiveness) -> DaemonPresence {
        DaemonPresence {
            liveness,
            active_sessions: 0,
            last_seen: SystemTime::UNIX_EPOCH,
            entered_at: SystemTime::UNIX_EPOCH,
            in_flight_turns: Default::default(),
            current_pid: None,
        }
    }

    #[test]
    fn display_state_derives_running_from_the_turn_set_not_a_stored_flag() {
        let mut p = presence_at(DaemonLiveness::Up { degraded: false });
        assert_eq!(p.display_state(), DaemonState::Idle);
        assert!(!p.is_running());
        p.in_flight_turns
            .insert(TurnKey::Id("r".into()), SystemTime::UNIX_EPOCH);
        assert_eq!(p.display_state(), DaemonState::Running);
        assert!(p.is_running());
        p.in_flight_turns.clear();
        assert_eq!(p.display_state(), DaemonState::Idle, "drained ⇒ Idle");
    }

    #[test]
    fn display_state_degraded_wins_over_running_with_a_turn_still_in_flight() {
        let mut p = presence_at(DaemonLiveness::Up { degraded: true });
        p.in_flight_turns
            .insert(TurnKey::Id("still-running".into()), SystemTime::UNIX_EPOCH);
        assert_eq!(p.display_state(), DaemonState::Degraded);
        assert!(!p.is_running(), "a degraded daemon never reads as Running");
    }

    #[test]
    fn display_state_down_wins_over_a_stray_turn() {
        let mut p = presence_at(DaemonLiveness::Down);
        p.in_flight_turns
            .insert(TurnKey::Id("stray".into()), SystemTime::UNIX_EPOCH);
        assert_eq!(p.display_state(), DaemonState::Down);
        assert!(!p.is_running());
    }

    /// A scene of `capacities` holding `desks`, each slot one second younger
    /// than the last.
    fn seated(
        capacities: [usize; MAX_FLOORS],
        desks: &[Option<FloorDesk>],
    ) -> (SceneState, Vec<AgentId>) {
        let mut s = SceneState::new(capacities);
        let ids: Vec<AgentId> = (0..desks.len())
            .map(|i| AgentId::from_transcript_path(&format!("/p/{i}.jsonl")))
            .collect();
        for (i, (&id, &at)) in ids.iter().zip(desks).enumerate() {
            let mut slot = make_slot(id, 0);
            slot.home_desk = at;
            slot.created_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(i as u64);
            s.agents.insert(id, slot);
        }
        (s, ids)
    }

    #[test]
    fn a_waiter_takes_the_first_free_desk_past_an_empty_floor() {
        let (mut s, ids) = seated(
            [1, 0, 2, 0, 0, 0, 0, 0, 0, 0],
            &[Some(FloorDesk::new(0, 0)), None, None, None],
        );
        s.assign_waiting(|_| true);
        let desks: Vec<_> = ids.iter().map(|id| s.agents[id].home_desk).collect();
        assert_eq!(
            desks,
            [
                Some(FloorDesk::new(0, 0)),
                Some(FloorDesk::new(2, 0)),
                Some(FloorDesk::new(2, 1)),
                None
            ]
        );
    }

    #[test]
    fn a_shrink_moves_no_one_on_another_floor() {
        let all = [
            Some(FloorDesk::new(0, 0)),
            Some(FloorDesk::new(0, 1)),
            Some(FloorDesk::new(1, 0)),
            Some(FloorDesk::new(1, 1)),
        ];
        let (mut s, ids) = seated([2, 2, 0, 0, 0, 0, 0, 0, 0, 0], &all);
        s.set_capacities([1, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
        let desks: Vec<_> = ids.iter().map(|id| s.agents[id].home_desk).collect();
        assert_eq!(
            desks,
            [
                Some(FloorDesk::new(0, 0)),
                None,
                Some(FloorDesk::new(1, 0)),
                Some(FloorDesk::new(1, 1))
            ]
        );
        s.set_capacities([2, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
        let desks: Vec<_> = ids.iter().map(|id| s.agents[id].home_desk).collect();
        assert_eq!(desks, all);
    }

    #[test]
    fn a_freed_desk_goes_to_the_oldest_staying_waiter() {
        let (mut s, ids) = seated([1, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[None, None, None]);
        s.agents.get_mut(&ids[0]).unwrap().exiting_at = Some(SystemTime::UNIX_EPOCH);
        s.assign_waiting(|_| true);
        let desks: Vec<_> = ids.iter().map(|id| s.agents[id].home_desk).collect();
        assert_eq!(desks, [None, Some(FloorDesk::new(0, 0)), None]);
    }

    #[test]
    fn total_capacity_sums_all_floors() {
        let s = SceneState::new([4, 8, 6, 4, 2, 0, 0, 0, 0, 0]);
        assert_eq!(s.total_capacity(), 24);

        let u = SceneState::uniform(8);
        assert_eq!(u.total_capacity(), 80);
    }
}
