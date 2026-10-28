use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::AgentId;
use crate::state::TurnKey;

/// Backpressure bound for the workspace-wide `(Transport, AgentEvent)` event
/// channel — the ONE place this capacity is defined. The runtime reducer feed
/// and the hook tee both size their channels from this, so the tee adds a
/// stage rather than a different backpressure policy.
pub const EVENT_CHANNEL_CAPACITY: usize = 256;

/// The transcript watcher's poll backstop — also the cadence profile rescans
/// ride, so "how often we look at the filesystem" has one authority. Ungated,
/// because it also bounds how far a transcript lags its write for the state
/// layer.
pub(crate) const DEFAULT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Which transport produced an event — used by the reducer for hook-wins
/// dedup. Lives on the source side because every `Source` implementor must
/// tag its own events; the reducer is downstream.
///
/// `#[non_exhaustive]`: keeps adding a transport a non-breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Transport {
    /// Arrived over the hook socket/named pipe — live; wins hook-vs-JSONL dedup.
    Hook,
    /// Read from a transcript JSONL file (may be a historical replay).
    Jsonl,
}

/// Structured tool detail, so the reducer can pattern-match (instead of
/// string-scanning) on semantic categories like Task-delegation.
///
/// `#[non_exhaustive]`: keeps adding a tool category non-breaking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ToolDetail {
    /// A subagent dispatch: the parent shows Delegating until its
    /// `ToolUseEnd`.
    Task,
    /// Any other tool.
    Generic {
        /// The user-facing tool label (e.g. `"Bash: ls"`).
        display: String,
    },
}

impl ToolDetail {
    /// The user-facing label for this tool detail.
    pub fn display(&self) -> &str {
        match self {
            ToolDetail::Task => "Delegating",
            ToolDetail::Generic { display } => display,
        }
    }
    /// Whether this is the `Task` delegation category.
    pub fn is_task(&self) -> bool {
        matches!(self, ToolDetail::Task)
    }
}

/// Test-ergonomic conversion by tool NAME: `"Agent"` (CC's dispatch tool) maps
/// to `Task`, so a test written as `Some("Agent".into())` exercises the real
/// `is_task()` path instead of silently falling to `Generic`. Kept in lockstep
/// with `decoder::make_tool_detail`'s known-name set — a test-only alias for a
/// name production no longer recognizes would give false confidence.
impl From<&str> for ToolDetail {
    fn from(s: &str) -> Self {
        if s == "Agent" {
            ToolDetail::Task
        } else {
            ToolDetail::Generic {
                display: s.to_string(),
            }
        }
    }
}

/// The event vocabulary every source decodes into.
///
/// `#[non_exhaustive]`: keeps adding a variant non-breaking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AgentEvent {
    /// A session began — registers a new agent slot at the next free desk.
    SessionStart {
        /// The new session's agent id.
        agent_id: AgentId,
        /// The originating CLI (registry source name, e.g. `"claude-code"`).
        source: String,
        /// The CLI's own session identifier.
        session_id: String,
        /// The session's working directory (drives the desk label + team outfit).
        cwd: PathBuf,
        /// The parent agent when this is a subagent, else `None`.
        parent_id: Option<AgentId>,
    },
    /// A tool call started — the slot goes Running.
    ToolUseStart {
        /// The acting agent.
        agent_id: AgentId,
        /// The tool call's id, used to pair with its `ToolUseEnd` (hook-wins dedup).
        tool_use_id: Option<String>,
        /// Structured tool detail (e.g. a subagent-dispatch `Task`), when known.
        detail: Option<ToolDetail>,
    },
    /// A tool call finished.
    ToolUseEnd {
        /// The acting agent.
        agent_id: AgentId,
        /// The completing tool call's id (pairs with its `ToolUseStart`).
        tool_use_id: Option<String>,
    },
    /// A call about to run, named by its tool: a pending call a request can
    /// bind by that name, with no item of its own (codex's `PreToolUse`; CC's
    /// and kimi's beside their start).
    ToolUsePending {
        /// The acting agent.
        agent_id: AgentId,
        /// The call's id.
        tool_use_id: String,
        /// The tool's wire name, the key a request names it by; `None` when
        /// the wire carries none, which binds by no name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        /// The turn the call runs in, when the wire names it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<TurnKey>,
    },
    /// A turn started, from the agent's ordered bracket stream.
    TurnStart {
        /// The agent whose turn started.
        agent_id: AgentId,
        /// The turn.
        turn: TurnKey,
    },
    /// The agent's foreground work ended: its turn, or a wait outside one
    /// (codewhale `session_idle`). It names no call.
    TurnEnd {
        /// The agent whose work ended.
        agent_id: AgentId,
        /// The turn, from a bracket stream; `None` from a source whose turn
        /// end names none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<TurnKey>,
    },
    /// The agent is blocked on a permission/input prompt — the slot goes RequiresAction.
    UserActionRequest {
        /// The agent requiring action.
        agent_id: AgentId,
        /// Why it requires action (the prompt/notification reason); `None` when
        /// the wire carries none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The gated call's id, when the wire names one (omp
        /// `tool_approval_requested`, kimi `tool_call_id`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        /// The gated tool's wire name, when the wire names one: the request
        /// holds that tool's pending calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        /// The turn the request blocks, when the wire names it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<TurnKey>,
    },
    /// A signal that answers the agent's standing request, if one stands; it
    /// ends no item.
    UserActionResolved {
        /// The agent whose request this answers, if one stands.
        agent_id: AgentId,
        /// What answered it.
        by: Resolution,
    },
    /// Late-discovered display name (e.g. CC subagent `attributionAgent`).
    /// Reducer overrides the slot label; noop if the slot doesn't exist.
    Rename {
        /// The agent to relabel.
        agent_id: AgentId,
        /// The new display label.
        label: String,
    },
    /// A session ended — marks the slot exiting (GC'd after the grace window).
    SessionEnd {
        /// The ending agent.
        agent_id: AgentId,
        /// True ONLY when this end's SUBJECT is a CHILD agent ending *as a
        /// child*. Source-trait CONTRACT: only subagent-END decoders may stamp
        /// `true`; every other constructor stamps `false` on a root end. The
        /// reducer's lifecycle record keys on the stamp: `true`, or any end of a
        /// child it already knows, starts the child's gate on a late transcript
        /// re-registration, and any other `false` from a hook makes only a hook
        /// start revive the root for the straggler window.
        as_child: bool,
    },
    /// Emitted by a watcher once per liveness-probe refresh for EVERY session
    /// id the probe currently vouches for. The reducer ONLY refreshes a
    /// sweep-exemption timestamp for an existing, non-exiting slot — it must
    /// never create a slot, never touch activity state, and never refresh
    /// `last_event_at` (the label/back-fill logic stays driven by real events).
    ProofOfLife {
        /// The vouched-live agent whose sweep-exemption to refresh.
        agent_id: AgentId,
    },
    /// Identity context a hook decoder attaches IMMEDIATELY AHEAD of a
    /// tool/permission activity event: hook payloads carry source/session_id/
    /// cwd that the activity variants don't, so without this a proof-of-life
    /// registration for an unknown id starts BLANK until the next real
    /// `SessionStart` — for a hook-only source, the whole rest of the turn.
    /// The reducer registers-or-back-fills from it on the Hook transport ONLY
    /// (a JSONL `Identity` is a structural no-op — transcript lines can be
    /// historical replays and must never synthesize).
    Identity {
        /// The agent this identity context describes.
        agent_id: AgentId,
        /// The originating CLI (registry source name).
        source: String,
        /// The CLI's own session identifier.
        session_id: String,
        /// `None` when the payload carries no usable cwd — the registration is
        /// then label-ordinal but still reap-exempt.
        cwd: Option<PathBuf>,
        /// The agent process's pid (+ recycle marker), from the shim/plugin
        /// `_pid` stamp — the focus-jump channel for hook-only sources.
        /// Transcript-family sources resolve pid via their liveness probes and
        /// always carry `None` here. serde-skipped so the conformance/scene
        /// goldens don't churn on `None`.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pid: Option<PidIdentity>,
    },
    /// An LLM model/effort OBSERVATION from a source's wire. Both fields are
    /// RAW wire strings (the house RAW-store/interpret-at-paint posture — the
    /// burn-tier tables in `pixtuoid-scene` do the reading). The reducer
    /// updates an EXISTING slot only (unknown id = no-op — a model line never
    /// registers a session) and dedups per field; decoders may emit per
    /// sighting.
    ModelInfo {
        /// The agent this observation is attributed to.
        agent_id: AgentId,
        /// `Some` = a model observation (e.g. `"claude-fable-5"`).
        model: Option<String>,
        /// `Some` = an effort observation (Codex `"xhigh"` verbatim; CC's
        /// synthesized `"ultra"`/`"ultrathink"` marker labels).
        effort: Option<String>,
    },
    /// A FRESH-token spend observation from a source's wire — cache READS are
    /// re-served context, not new spend, and are excluded. Each event is that
    /// reading's DELTA; the reducer accumulates onto an EXISTING slot only
    /// (unknown id = no-op — usage never registers a session). JSONL-only (no
    /// hook carries usage), so it never enters hook-wins dedup.
    Usage {
        /// The agent whose token spend this reading records.
        agent_id: AgentId,
        /// Fresh tokens this reading: new input + cache writes + output.
        fresh_tokens: u64,
    },
}

/// What a [`AgentEvent::UserActionRequest`] names on the wire, each `None`
/// where it names nothing.
///
/// `#[non_exhaustive]`: keeps adding a key non-breaking.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RequestKeys {
    /// The gated call's id.
    pub tool_use_id: Option<String>,
    /// The gated tool's wire name.
    pub tool_name: Option<String>,
    /// The turn the request blocks.
    pub turn: Option<TurnKey>,
}

/// What answered an agent's request: one call, or the whole turn it blocked.
///
/// `#[non_exhaustive]`: keeps adding a resolution non-breaking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Resolution {
    /// The call ran (codex's `PostToolUse`).
    Call(String),
    /// The turn stopped with nothing left pending (codex's `Stop`).
    Turn(TurnKey),
}

/// A cached agent pid PLUS the kernel start marker read when it was stamped
/// ([`pid_start_marker`]) — together they name ONE process incarnation, so a
/// focus click can refuse a RECYCLED pid: re-read the marker, and a mismatch
/// (or a dead pid) means this is not the process the hook came from.
/// `started: None` = no marker was readable at stamp time (non-unix daemon,
/// EPERM); the click-time guard then skips the identity check, so a markerless
/// cache retains a recycled-pid residual until the stale sweep.
///
/// `non_exhaustive`: cross-crate construction goes through
/// [`PidIdentity::new`], so a future identity component is a non-breaking add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct PidIdentity {
    /// The agent CLI's OS pid (`pid_t`; matches `DaemonPresence.current_pid`).
    pub pid: i32,
    /// Opaque per-OS start marker — equality-only, see [`pid_start_marker`].
    pub started: Option<u64>,
}

impl PidIdentity {
    /// Bundle a pid with its start marker (`None` where the OS can't provide one).
    pub fn new(pid: i32, started: Option<u64>) -> Self {
        Self { pid, started }
    }
}

impl AgentEvent {
    /// A hook `Identity` event with `pid: None` — the pid is stamped LATER by
    /// the daemon's `_pid` peek (`patch_identity_pids`), never at decode. THE
    /// constructor every source's custom decoder mints its `Identity` through,
    /// so a future field addition lands in ONE place.
    pub(crate) fn identity(
        agent_id: AgentId,
        source: impl Into<String>,
        session_id: impl Into<String>,
        cwd: Option<PathBuf>,
    ) -> Self {
        AgentEvent::Identity {
            agent_id,
            source: source.into(),
            session_id: session_id.into(),
            cwd,
            pid: None,
        }
    }

    /// A turn end naming no turn: THE constructor every decoder whose wire
    /// names none mints one through, so a field it gains lands here and in
    /// [`Self::keyed_turn_end`], never at a decoder.
    pub(crate) fn turn_end(agent_id: AgentId) -> Self {
        AgentEvent::TurnEnd {
            agent_id,
            turn: None,
        }
    }

    /// A bracket stream's turn end, naming its turn.
    pub(crate) fn keyed_turn_end(agent_id: AgentId, turn: TurnKey) -> Self {
        AgentEvent::TurnEnd {
            agent_id,
            turn: Some(turn),
        }
    }

    /// A request for the user's action. THE constructor every site mints one
    /// through, so a field it gains lands in ONE place: a key in
    /// [`RequestKeys`], which a keyless site leaves at its default.
    pub fn user_action_request(
        agent_id: AgentId,
        reason: Option<String>,
        keys: RequestKeys,
    ) -> Self {
        let RequestKeys {
            tool_use_id,
            tool_name,
            turn,
        } = keys;
        AgentEvent::UserActionRequest {
            agent_id,
            reason,
            tool_use_id,
            tool_name,
            turn,
        }
    }

    /// The agent id this event concerns (every variant carries one).
    pub fn agent_id(&self) -> AgentId {
        match self {
            AgentEvent::SessionStart { agent_id, .. } => *agent_id,
            AgentEvent::ToolUseStart { agent_id, .. } => *agent_id,
            AgentEvent::ToolUseEnd { agent_id, .. } => *agent_id,
            AgentEvent::ToolUsePending { agent_id, .. } => *agent_id,
            AgentEvent::TurnStart { agent_id, .. } => *agent_id,
            AgentEvent::TurnEnd { agent_id, .. } => *agent_id,
            AgentEvent::UserActionRequest { agent_id, .. } => *agent_id,
            AgentEvent::UserActionResolved { agent_id, .. } => *agent_id,
            AgentEvent::Rename { agent_id, .. } => *agent_id,
            AgentEvent::SessionEnd { agent_id, .. } => *agent_id,
            AgentEvent::ProofOfLife { agent_id, .. } => *agent_id,
            AgentEvent::Identity { agent_id, .. } => *agent_id,
            AgentEvent::ModelInfo { agent_id, .. } => *agent_id,
            AgentEvent::Usage { agent_id, .. } => *agent_id,
        }
    }

    /// Whether this event shows its session working now: the creation
    /// polarity's evidence for a slot no start announced. A session start
    /// carries its own authority, and a session end, a model, usage or title
    /// line shows nothing, nor does [`ProofOfLife`](Self::ProofOfLife), which
    /// vouches a visible slot past the sweeps and must never create one.
    pub(crate) fn shows_activity(&self) -> bool {
        match self {
            AgentEvent::ToolUseStart { .. }
            | AgentEvent::ToolUseEnd { .. }
            | AgentEvent::ToolUsePending { .. }
            | AgentEvent::TurnStart { .. }
            | AgentEvent::TurnEnd { .. }
            | AgentEvent::UserActionRequest { .. }
            | AgentEvent::UserActionResolved { .. } => true,
            AgentEvent::SessionStart { .. }
            | AgentEvent::Rename { .. }
            | AgentEvent::SessionEnd { .. }
            | AgentEvent::ProofOfLife { .. }
            | AgentEvent::Identity { .. }
            | AgentEvent::ModelInfo { .. }
            | AgentEvent::Usage { .. } => false,
        }
    }

    /// Whether this event is its session's work, which keeps the session and
    /// its ancestors past the stale sweep.
    pub(crate) fn refreshes_lineage(&self) -> bool {
        match self {
            AgentEvent::ToolUseStart { .. }
            | AgentEvent::ToolUseEnd { .. }
            | AgentEvent::ToolUsePending { .. }
            | AgentEvent::TurnStart { .. }
            | AgentEvent::TurnEnd { .. }
            | AgentEvent::UserActionRequest { .. }
            | AgentEvent::UserActionResolved { .. } => true,
            AgentEvent::SessionStart { .. }
            | AgentEvent::Rename { .. }
            | AgentEvent::SessionEnd { .. }
            | AgentEvent::ProofOfLife { .. }
            | AgentEvent::Identity { .. }
            | AgentEvent::ModelInfo { .. }
            | AgentEvent::Usage { .. } => false,
        }
    }
}

/// The PRIMARY on-disk root a source resolves, through the SAME
/// `default_paths()` the driver calls — a second copy of the resolution is the
/// #880 defect one layer up. `None` = no such root (a daemon is port-keyed; a
/// hook-only CLI's config root lives with its install target). omp watches
/// further profile roots beyond this one (`omp_profile_sessions_dirs`).
///
/// LOCKSTEP with [`registry::SourceDescriptor::home_env`]: a row declaring an
/// override with no `root` fails
/// `every_declared_home_env_actually_moves_that_sources_root`.
/// Internal cross-crate helper, not a stable API.
#[doc(hidden)]
#[cfg(feature = "native")]
pub fn resolved_source_root(name: &str) -> Option<std::path::PathBuf> {
    registry::descriptor_for(name)?.native.root?()
}

/// Shared ACP (Agent Client Protocol) wire-vocabulary decode — reused by any
/// source whose transcript speaks ACP (grok today).
pub(crate) mod acp;
/// Antigravity (Google IDE CLI) transcript source: decoder + `Source` adapter.
pub mod antigravity;
// All the tokio/notify/libc FFI in the crate lives behind `native`, gated out
// of a wasm (`--no-default-features`) build; the per-source modules below stay
// compiled because their pure DECODERS feed the registry, with each one's
// runtime half in a `source/<cli>/native.rs` sub-module.
#[cfg(feature = "native")]
pub(crate) mod cc_probe;

/// Read a first-party file, bounded to `cap` bytes — the one spelling for every
/// probe/registry read in `source/`; the `.cwd` sidecar keeps its own
/// String-returning `grok::read_bounded`. These files are re-read on every probe
/// refresh, so an unbounded read lets a junk or runaway file balloon a per-scan
/// allocation; truncated bytes just fail the caller's parse. The CAP stays at
/// the call site, where its sizing rationale lives.
#[cfg(all(unix, feature = "native"))]
pub(crate) fn read_bounded_bytes(path: &std::path::Path, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(cap).read_to_end(&mut bytes)?;
    Ok(bytes)
}
pub(crate) mod admit;
/// Claude Code transcript source: line/hook decoders + the `Source` adapter.
pub mod claude_code;
pub mod codewhale;
pub mod codex;
pub mod copilot;
pub mod cursor;
/// The shared, daemon-agnostic presence layer (state machine + lifecycle for
/// every daemon-style source); per-daemon wire decode stays in its own module.
pub mod daemon;
pub mod decoder;
pub mod drift;
pub mod dsh;
#[cfg(feature = "native")]
pub(crate) mod exit_watch;
#[cfg(feature = "native")]
pub(crate) mod fd_probe;
pub mod grok;
pub mod hermes;
#[cfg(feature = "native")]
/// The shared hook socket listener + router every CLI's hook shim connects to.
pub mod hook;
#[cfg(feature = "native")]
/// The JSONL transcript watcher: tails per-session `.jsonl` files with the
/// first-sight gate + liveness probe ladder.
pub mod jsonl;
pub mod kimi;
#[cfg(all(target_os = "macos", feature = "native"))]
pub(crate) mod kqueue;
#[cfg(feature = "native")]
/// `SourceManager` — spawns sources and surfaces a fatal source exit as `SourceDeath`.
pub mod manager;
#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::{DynSource, Source, TaggedReceiver, TaggedSender};
pub mod omp;
pub mod openclaw;
pub mod opencode;
#[cfg(feature = "native")]
mod proc_start;
#[cfg(feature = "native")]
pub use proc_start::pid_start_marker;
pub mod reasonix;
// `doc(hidden)`: an internal fact table, `pub` ONLY so the integration-test
// crates can read it. Keeping it off the published API lets cargo-semver-checks
// accept descriptor/caps field changes without a breaking-version bump.
#[doc(hidden)]
pub mod registry;
