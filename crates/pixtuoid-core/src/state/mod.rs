use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::id::AgentId;

mod correlation;
mod fsm;
/// The event coordinator: [`reducer::Reducer`] folds `AgentEvent`s into `SceneState`.
pub mod reducer;
mod requests;
mod scope;

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

/// Global desk index — the reducer's allocation space across ALL floors.
///
/// NOT a valid index into a single floor's `SceneLayout::home_desks`; convert
/// through `SceneState::floor_local_desk` (the one legal bridge) first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GlobalDeskIndex(pub usize);

/// Floor-local desk index — indexes a single floor's
/// `SceneLayout::home_desks` (see `SceneLayout::home_desk`).
///
/// Deliberately NOT `Serialize` (its twin `GlobalDeskIndex` is): a transient
/// bridge value, never a stored `SceneState` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FloorLocalDeskIndex(pub usize);

impl GlobalDeskIndex {
    /// The floor-local view of this index **within a single-floor scene**,
    /// where the global space coincides with the floor-0 local space so the
    /// cast is the identity by construction. For a multi-floor scene go through
    /// `SceneState::floor_local_desk` — the arithmetic bridge — instead.
    pub fn single_floor_local(self) -> FloorLocalDeskIndex {
        FloorLocalDeskIndex(self.0)
    }
}

/// Semantic category of the tool an `Active` slot is running, derived ONCE at
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
    /// Anything else, including a detail-less Active.
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
    /// No tool running.
    Idle,
    /// A tool call is in flight.
    Active {
        /// The in-flight tool call's id (pairs Start↔End).
        #[serde(with = "opt_arc_str_serde")]
        tool_use_id: Option<Arc<str>>,
        /// The tool's display detail, when known.
        #[serde(with = "opt_arc_str_serde")]
        detail: Option<Arc<str>>,
        /// The bucketed tool kind (drives the per-tool glow/tally).
        kind: ToolKind,
    },
    /// Blocked on a permission/input prompt.
    Waiting {
        /// Why the agent is waiting (the prompt reason).
        #[serde(with = "arc_str_serde")]
        reason: Arc<str>,
    },
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
    /// Current activity — Idle, Active (running a tool), or Waiting.
    pub state: ActivityState,
    /// Wall-clock time the current `state` was entered (reset on every state
    /// change).
    pub state_started_at: SystemTime,
    /// Wall-clock time of the most recent event (any type) from this agent —
    /// the stale-agent sweep's primary liveness signal.
    pub last_event_at: SystemTime,
    /// Wall-clock time the slot was first created — the one-shot entry
    /// animation's anchor, unaffected by later state transitions.
    pub created_at: SystemTime,
    /// Set when `SessionEnd` arrived but the slot is held alive long enough for
    /// the exit animation to play.
    pub exiting_at: Option<SystemTime>,
    /// GLOBAL desk index (assigned once at `SessionStart`, never mutated).
    pub desk_index: GlobalDeskIndex,
    /// Floor assigned at desk allocation time. Immutable for the agent's
    /// lifetime so capacity growth never silently migrates agents between
    /// floors.
    pub floor_idx: usize,
    /// Monotonic count of tool calls this agent has made this session.
    pub tool_call_count: u32,
    /// Cumulative milliseconds spent in the `Active` state.
    pub active_ms: u64,
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
    /// last-seen-wins, so a mid-session `/model` switch tracks. Interpretation
    /// (the burn-tier tables) lives in the scene layer; this stays
    /// uninterpreted wire truth.
    #[serde(
        with = "opt_arc_str_serde",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub model: Option<Arc<str>>,
    /// The RAW effort observation last seen. The scene layer treats the value
    /// as live only within its TTL — no sighting means the boost decays, which
    /// is honest (an idle agent isn't burning).
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

fn u64_is_zero(v: &u64) -> bool {
    *v == 0
}

/// A RAW effort string + WHEN it was last observed — the freshness the scene
/// layer's burn-tier TTL reads (see `AgentSlot::effort`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EffortObservation {
    /// The RAW effort string as observed on the wire (uninterpreted).
    #[serde(with = "arc_str_serde")]
    pub value: Arc<str>,
    /// When `value` was last observed — the burn-tier freshness clock.
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

/// Liveness of a daemon-style source (the OpenClaw gateway), driving the
/// wandering lobster mascot. PER-INSTANCE (see [`DaemonInstanceId`]) — one
/// gateway going Down says nothing about its siblings. `Down` is distinct from
/// *absent* (no roster entry): absent = never observed / plugin not loaded;
/// `Down` = this instance was seen and then died.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonState {
    /// Alive with no run in flight — the mascot ambles.
    Idle,
    /// ≥1 run in flight (projected from `DaemonPresence::in_flight_runs`).
    Busy,
    /// Gateway is UP but every run fails on its model backend (auth revoked /
    /// provider down) — alive-but-broken, NOT idle. Self-heals on the next
    /// successful run, a new run start, or a gateway restart.
    Degraded,
    /// Seen and then died — the mascot walks out (distinct from *absent*).
    Down,
}

/// The two ORTHOGONAL liveness axes a daemon mascot actually STORES. The
/// remaining render distinction — Idle vs Busy — is deliberately NOT a field
/// here: it is a pure function of [`DaemonPresence::in_flight_runs`], projected
/// by [`DaemonPresence::display_state`], so "busy" can never drift from the run
/// set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonLiveness {
    /// The gateway is alive.
    Up {
        /// Alive-but-broken: every model run is failing, so the mascot renders
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
    /// Last time ANY presence event arrived — drives the busy→idle decay and
    /// the presence-TTL stale-down sweep, and anchors the leave animation
    /// (under `Down` this is the moment the gateway died).
    pub last_seen: SystemTime,
    /// When the gateway first appeared (absent/Down → up) — the enter-animation
    /// anchor and the steady wander clock.
    pub entered_at: SystemTime,
    /// In-flight runs (busy iff non-empty), each keyed by its correlation key
    /// and stamped with its LAST observation. The per-run stamp is what makes
    /// the busy-decay honest: the daemon-wide `last_seen` is refreshed by ANY
    /// event, so on a gateway still serving other traffic a run whose
    /// `agent_end` was dropped would never age out and the mascot would latch
    /// Busy forever.
    ///
    /// NOT serialized: transient process state a restart resets, so a restored
    /// dump can't strand a perpetual Busy.
    #[serde(skip)]
    pub in_flight_runs: BTreeMap<String, SystemTime>,
    /// The gateway pid currently armed for `ExitWatch` (None until first seen).
    pub current_pid: Option<i32>,
}

impl DaemonPresence {
    /// The 4-way render vocabulary ([`DaemonState`]) projected from the stored
    /// axes — the SINGLE place the `Degraded > Busy > Idle` priority is encoded.
    /// `degraded` is checked BEFORE the run set, so a degraded gateway renders
    /// Degraded even with runs still in flight.
    pub fn display_state(&self) -> DaemonState {
        match self.liveness {
            DaemonLiveness::Down => DaemonState::Down,
            DaemonLiveness::Up { degraded: true } => DaemonState::Degraded,
            DaemonLiveness::Up { degraded: false } => {
                if self.in_flight_runs.is_empty() {
                    DaemonState::Idle
                } else {
                    DaemonState::Busy
                }
            }
        }
    }

    /// Whether the mascot renders as Busy (alive, not degraded, ≥1 run in flight).
    #[cfg(test)]
    pub(crate) fn is_busy(&self) -> bool {
        self.display_state() == DaemonState::Busy
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// The whole-office snapshot the reducer maintains and the render layer reads:
/// live agent slots, per-floor desk capacities, and the daemon mascots.
pub struct SceneState {
    /// Live agent slots, keyed by `AgentId`.
    pub agents: BTreeMap<AgentId, AgentSlot>,
    /// Desk capacity per floor, indexed by floor (`0..MAX_FLOORS`).
    pub floor_capacities: [usize; MAX_FLOORS],
    /// Daemon-style sources rendered as wandering mascots, keyed source →
    /// instance so instance A's stop/expiry can't touch instance B. Empty for
    /// an all-agent scene.
    ///
    /// PRIVATE with READ-ONLY accessors on purpose: mutation must stay inside
    /// the daemon layer's apply/sweep/mark entry points, so nothing else can
    /// invent presence.
    #[serde(default)]
    pub(crate) daemons: DaemonRoster,
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
    /// / run set, which `apply_presence` can only stamp as "now". PRODUCTION
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
        }
    }

    /// A scene with the same desk capacity on every floor.
    pub fn uniform(cap: usize) -> Self {
        Self::new([cap; MAX_FLOORS])
    }

    /// Total desk count across all floors (sum of `floor_capacities`).
    pub fn total_capacity(&self) -> usize {
        self.floor_capacities.iter().sum()
    }

    fn cumulative_offsets(&self) -> [usize; MAX_FLOORS] {
        let mut offsets = [0usize; MAX_FLOORS];
        for i in 1..MAX_FLOORS {
            offsets[i] = offsets[i - 1] + self.floor_capacities[i - 1];
        }
        offsets
    }

    fn floor_of_with_offsets(
        &self,
        desk_index: GlobalDeskIndex,
        offsets: &[usize; MAX_FLOORS],
    ) -> usize {
        for i in (0..MAX_FLOORS).rev() {
            if self.floor_capacities[i] > 0 && desk_index.0 >= offsets[i] {
                return i;
            }
        }
        0
    }

    /// Which floor does `desk_index` belong to? Recomputed from CURRENT
    /// capacities, while `AgentSlot.floor_idx` freezes at allocation — so after
    /// growth `pixtuoid_scene::tally::per_floor_counts` still counts an agent
    /// that `pixtuoid_scene::floor::build_floor_scene` skips. The skip is
    /// deliberate — desk 0 has a real occupant — and is pinned by
    /// `build_floor_scene_skips_agent_below_grown_offset`.
    pub fn floor_of(&self, desk_index: GlobalDeskIndex) -> usize {
        self.floor_of_with_offsets(desk_index, &self.cumulative_offsets())
    }

    /// Local desk offset within the floor — THE bridge from the global
    /// allocation space to a floor's `home_desks` index space.
    pub fn floor_local_desk(&self, desk_index: GlobalDeskIndex) -> FloorLocalDeskIndex {
        let offsets = self.cumulative_offsets();
        let floor = self.floor_of_with_offsets(desk_index, &offsets);
        FloorLocalDeskIndex(desk_index.0 - offsets[floor])
    }

    /// Global desk index range `[lo, hi)` for a given floor, clamping
    /// `floor_idx` to `MAX_FLOORS - 1`. Raw `usize` rather than the newtype:
    /// a `Range` of newtypes has no `Step` impl.
    pub fn floor_range(&self, floor_idx: usize) -> std::ops::Range<usize> {
        let idx = floor_idx.min(MAX_FLOORS - 1);
        let offsets = self.cumulative_offsets();
        let lo = offsets[idx];
        let hi = lo + self.floor_capacities[idx];
        lo..hi
    }

    /// Lowest free desk index, or `None` if all desks are occupied.
    pub fn next_free_desk(&self) -> Option<GlobalDeskIndex> {
        let occupied: std::collections::BTreeSet<GlobalDeskIndex> =
            self.agents.values().map(|a| a.desk_index).collect();
        (0..self.total_capacity())
            .map(GlobalDeskIndex)
            .find(|i| !occupied.contains(i))
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

    fn make_slot(id: AgentId, desk_index: usize) -> AgentSlot {
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
            last_event_at: now,
            exiting_at: None,
            desk_index: GlobalDeskIndex(desk_index),
            floor_idx: 0,
            tool_call_count: 0,
            active_ms: 0,
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
        slot_a.state = ActivityState::Active {
            tool_use_id: Some(Arc::from("tuid-1")),
            detail: Some(Arc::from("Read · src/main.rs")),
            kind: ToolKind::Read,
        };
        s.agents.insert(a, slot_a);

        let b = AgentId::from_transcript_path("/p/b.jsonl");
        let mut slot_b = make_slot(b, 1);
        slot_b.state = ActivityState::Waiting {
            reason: Arc::from("permission: Bash"),
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
            ActivityState::Active { .. }
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
            in_flight_runs: ["run-1", "run-2"]
                .into_iter()
                .map(|k| (k.to_string(), SystemTime::now()))
                .collect(),
            current_pid: Some(4242),
        };
        assert_eq!(
            p.display_state(),
            DaemonState::Busy,
            "a non-empty run set reads Busy before serialization"
        );
        let json = serde_json::to_string(&p).expect("serialize");
        assert!(
            !json.contains("run-1"),
            "in_flight_runs must be skipped on the wire: {json}"
        );
        let back: DaemonPresence = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.liveness, DaemonLiveness::UP);
        assert_eq!(
            back.display_state(),
            DaemonState::Idle,
            "skipped run set restores empty ⇒ Idle, never a stranded Busy"
        );
        assert_eq!(back.active_sessions, 3);
        assert_eq!(back.current_pid, Some(4242));
        assert!(
            back.in_flight_runs.is_empty(),
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
                in_flight_runs: Default::default(),
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
            in_flight_runs: Default::default(),
            current_pid: None,
        }
    }

    #[test]
    fn display_state_derives_busy_from_the_run_set_not_a_stored_flag() {
        let mut p = presence_at(DaemonLiveness::Up { degraded: false });
        assert_eq!(p.display_state(), DaemonState::Idle);
        assert!(!p.is_busy());
        p.in_flight_runs.insert("r".into(), SystemTime::UNIX_EPOCH);
        assert_eq!(p.display_state(), DaemonState::Busy);
        assert!(p.is_busy());
        p.in_flight_runs.clear();
        assert_eq!(p.display_state(), DaemonState::Idle, "drained ⇒ Idle");
    }

    #[test]
    fn display_state_degraded_wins_over_busy_with_a_run_still_in_flight() {
        let mut p = presence_at(DaemonLiveness::Up { degraded: true });
        p.in_flight_runs
            .insert("still-running".into(), SystemTime::UNIX_EPOCH);
        assert_eq!(p.display_state(), DaemonState::Degraded);
        assert!(!p.is_busy(), "a degraded daemon never reads as Busy");
    }

    #[test]
    fn display_state_down_wins_over_a_stray_run_key() {
        let mut p = presence_at(DaemonLiveness::Down);
        p.in_flight_runs
            .insert("stray".into(), SystemTime::UNIX_EPOCH);
        assert_eq!(p.display_state(), DaemonState::Down);
        assert!(!p.is_busy());
    }

    #[test]
    fn next_free_desk_returns_none_when_full() {
        let mut s = SceneState::uniform(2);
        let total = s.total_capacity();
        for i in 0..total {
            let id = AgentId::from_transcript_path(&format!("p{i}"));
            s.agents.insert(id, make_slot(id, i));
        }
        assert_eq!(s.next_free_desk(), None);
    }

    #[test]
    fn next_free_desk_overflows_to_second_floor() {
        let mut s = SceneState::uniform(4);
        for i in 0..4 {
            let id = AgentId::from_transcript_path(&format!("f{i}"));
            s.agents.insert(id, make_slot(id, i));
        }
        assert_eq!(
            s.next_free_desk(),
            Some(GlobalDeskIndex(4)),
            "should overflow to desk 4 (floor 1)"
        );
    }

    #[test]
    fn floor_of_variable_capacities() {
        let s = SceneState::new([4, 8, 6, 4, 2, 0, 0, 0, 0, 0]);
        assert_eq!(s.floor_of(GlobalDeskIndex(0)), 0);
        assert_eq!(s.floor_of(GlobalDeskIndex(3)), 0);
        assert_eq!(s.floor_of(GlobalDeskIndex(4)), 1);
        assert_eq!(s.floor_of(GlobalDeskIndex(11)), 1);
        assert_eq!(s.floor_of(GlobalDeskIndex(12)), 2);
        assert_eq!(s.floor_of(GlobalDeskIndex(17)), 2);
        assert_eq!(s.floor_of(GlobalDeskIndex(18)), 3);
        assert_eq!(s.floor_of(GlobalDeskIndex(22)), 4);
        assert_eq!(s.floor_of(GlobalDeskIndex(23)), 4);
    }

    #[test]
    fn floor_local_desk_variable() {
        let s = SceneState::new([4, 8, 6, 4, 2, 0, 0, 0, 0, 0]);
        assert_eq!(
            s.floor_local_desk(GlobalDeskIndex(0)),
            FloorLocalDeskIndex(0)
        );
        assert_eq!(
            s.floor_local_desk(GlobalDeskIndex(3)),
            FloorLocalDeskIndex(3)
        );
        assert_eq!(
            s.floor_local_desk(GlobalDeskIndex(4)),
            FloorLocalDeskIndex(0)
        );
        assert_eq!(
            s.floor_local_desk(GlobalDeskIndex(11)),
            FloorLocalDeskIndex(7)
        );
        assert_eq!(
            s.floor_local_desk(GlobalDeskIndex(12)),
            FloorLocalDeskIndex(0)
        );
    }

    #[test]
    fn floor_range_variable() {
        let s = SceneState::new([4, 8, 6, 4, 2, 0, 0, 0, 0, 0]);
        assert_eq!(s.floor_range(0), 0..4);
        assert_eq!(s.floor_range(1), 4..12);
        assert_eq!(s.floor_range(2), 12..18);
        assert_eq!(s.floor_range(3), 18..22);
        assert_eq!(s.floor_range(4), 22..24);
    }

    #[test]
    fn total_capacity_sums_all_floors() {
        let s = SceneState::new([4, 8, 6, 4, 2, 0, 0, 0, 0, 0]);
        assert_eq!(s.total_capacity(), 24);

        let u = SceneState::uniform(8);
        assert_eq!(u.total_capacity(), 80);
    }

    #[test]
    fn zero_capacity_floor_skipped_by_next_free_desk() {
        let s = SceneState::new([4, 0, 6, 0, 2, 0, 0, 0, 0, 0]);
        assert_eq!(s.total_capacity(), 12);
        assert_eq!(s.floor_range(0), 0..4);
        assert_eq!(s.floor_range(1), 4..4);
        assert_eq!(s.floor_range(2), 4..10);
        assert_eq!(s.next_free_desk(), Some(GlobalDeskIndex(0)));
    }

    #[test]
    fn floor_of_skips_zero_capacity_floors() {
        let s = SceneState::new([4, 0, 6, 0, 2, 0, 0, 0, 0, 0]);
        assert_eq!(s.floor_of(GlobalDeskIndex(4)), 2);
        assert_eq!(
            s.floor_local_desk(GlobalDeskIndex(4)),
            FloorLocalDeskIndex(0)
        );
        assert_eq!(s.floor_of(GlobalDeskIndex(9)), 2);
        assert_eq!(s.floor_of(GlobalDeskIndex(10)), 4);
    }

    #[test]
    fn floor_of_leading_zero_capacity_floors() {
        let s = SceneState::new([0, 0, 6, 4, 2, 0, 0, 0, 0, 0]);
        assert_eq!(s.floor_of(GlobalDeskIndex(0)), 2);
        assert_eq!(s.floor_of(GlobalDeskIndex(5)), 2);
        assert_eq!(s.floor_of(GlobalDeskIndex(6)), 3);
    }

    #[test]
    fn floor_range_clamps_oob_index() {
        let s = SceneState::uniform(4);
        let last = s.floor_range(MAX_FLOORS - 1);
        let oob = s.floor_range(MAX_FLOORS + 10);
        assert_eq!(
            last, oob,
            "an OOB floor index clamps to the last floor's range"
        );
    }

    #[test]
    fn floor_local_desk_oob_lands_on_last_nonempty_floor() {
        let s = SceneState::new([4, 8, 6, 4, 2, 0, 0, 0, 0, 0]);
        let total = s.total_capacity();
        let oob = total + 76;
        let floor = s.floor_of(GlobalDeskIndex(oob));
        assert_eq!(floor, 4, "OOB desk lands on last nonempty floor");
        let local = s.floor_local_desk(GlobalDeskIndex(oob));
        assert_eq!(local, FloorLocalDeskIndex(oob - 22));
    }
}
