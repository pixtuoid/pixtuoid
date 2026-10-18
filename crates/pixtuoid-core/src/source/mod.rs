use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::AgentId;

/// Backpressure bound for the workspace-wide `(Transport, AgentEvent)` event
/// channel — the ONE place this capacity is defined. The runtime reducer feed
/// and the hook tee both size their channels from this, so the tee adds a
/// stage rather than a different backpressure policy.
pub const EVENT_CHANNEL_CAPACITY: usize = 256;

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
    /// A subagent dispatch. The reducer suppresses hook-sourced Activity
    /// events for the parent until the matching `ActivityEnd` arrives.
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
    /// A tool call started — the slot goes Active.
    ActivityStart {
        /// The acting agent.
        agent_id: AgentId,
        /// The tool call's id, used to pair with its `ActivityEnd` (hook-wins dedup).
        tool_use_id: Option<String>,
        /// Structured tool detail (e.g. a subagent-dispatch `Task`), when known.
        detail: Option<ToolDetail>,
    },
    /// A tool call finished — arms the debounced return to Idle.
    ActivityEnd {
        /// The acting agent.
        agent_id: AgentId,
        /// The completing tool call's id (pairs with its `ActivityStart`).
        tool_use_id: Option<String>,
    },
    /// The agent is blocked on a permission/input prompt — the slot goes Waiting.
    Waiting {
        /// The waiting agent.
        agent_id: AgentId,
        /// Why it is waiting (the prompt/notification reason).
        reason: String,
        /// The gated tool call's id, when the wire names one (omp
        /// `tool_approval_requested`). Binds the wait to that call so the
        /// approval resume/denial resolves it regardless of which transport's
        /// events arrive first; `None` keeps the reducer's slot-state
        /// inference. Skipped when serialized so None-emitting sources keep
        /// their recorded goldens byte-stable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
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
        /// reducer's lifecycle record keys on the stamp: `true` starts the
        /// child's gate on a late/reordered parented re-registration, and a
        /// `false` from a hook makes only a hook start revive the root for the
        /// straggler window.
        as_child: bool,
    },
    /// Emitted by a watcher once per liveness-probe refresh for EVERY session
    /// id the probe currently vouches for. The reducer ONLY refreshes a
    /// sweep-exemption timestamp for an existing, non-exiting slot — it must
    /// never create a slot, never touch activity state, and never refresh
    /// `last_event_at` (the Active→Idle debounce and the label/back-fill logic
    /// stay driven by real events).
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

    /// The agent id this event concerns (every variant carries one).
    pub fn agent_id(&self) -> AgentId {
        match self {
            AgentEvent::SessionStart { agent_id, .. } => *agent_id,
            AgentEvent::ActivityStart { agent_id, .. } => *agent_id,
            AgentEvent::ActivityEnd { agent_id, .. } => *agent_id,
            AgentEvent::Waiting { agent_id, .. } => *agent_id,
            AgentEvent::Rename { agent_id, .. } => *agent_id,
            AgentEvent::SessionEnd { agent_id, .. } => *agent_id,
            AgentEvent::ProofOfLife { agent_id, .. } => *agent_id,
            AgentEvent::Identity { agent_id, .. } => *agent_id,
            AgentEvent::ModelInfo { agent_id, .. } => *agent_id,
            AgentEvent::Usage { agent_id, .. } => *agent_id,
        }
    }

    /// Whether this event shows its session working now: the creation
    /// polarity's evidence for a slot no start announced. A start carries its
    /// own authority, and an end, a model, usage or title line shows nothing,
    /// nor does [`ProofOfLife`](Self::ProofOfLife), which vouches a visible
    /// slot past the sweeps and must never create one.
    pub(crate) fn shows_activity(&self) -> bool {
        match self {
            AgentEvent::ActivityStart { .. }
            | AgentEvent::ActivityEnd { .. }
            | AgentEvent::Waiting { .. } => true,
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
/// override with no arm here fails
/// `every_declared_home_env_actually_moves_that_sources_root`.
/// Internal cross-crate helper, not a stable API.
#[doc(hidden)]
#[cfg(feature = "native")]
pub fn resolved_source_root(name: &str) -> Option<std::path::PathBuf> {
    Some(match name {
        "claude-code" => claude_code::ClaudeCodeSource::default_paths().projects_root,
        "codex" => codex::CodexSource::default_paths().sessions_root,
        "copilot" => copilot::CopilotSource::default_paths().sessions_root,
        "grok" => grok::GrokSource::default_paths().sessions_root,
        "omp" => omp::OmpSource::default_paths().sessions_root,
        "antigravity" => antigravity::AntigravitySource::default_paths().brain_root,
        "hermes" => hermes::hermes_home()?,
        _ => return None,
    })
}

/// Focus-jump pid point-queries for the transcript family — the ONE public
/// seam the binary's `focus` module consumes. A point query against the live
/// registry, never a transcript scan; it rides the recycle-guarded probe, the
/// reason transcript-family pids are NEVER taken from the shim parent. A
/// non-unix build resolves nothing (focus silently no-ops).
#[cfg(feature = "native")]
pub fn cc_pid_for_session(projects_root: &std::path::Path, session_id: &str) -> Option<i32> {
    let sessions_dir = cc_probe::cc_sessions_dir(projects_root)?;
    cc_probe::live_cc_session_ids(&sessions_dir)?
        .pid_of
        .get(session_id)
        .copied()
}

/// The CC sessions-registry dir the pid queries consult — the SAME
/// standard-layout gate the probe applies (a `--projects-root /tmp/fixture`
/// replay yields `None`). Exposed so `doctor` can report the focus channel's
/// on-disk state without re-deriving the sibling layout.
#[cfg(feature = "native")]
pub fn cc_registry_dir(projects_root: &std::path::Path) -> Option<std::path::PathBuf> {
    cc_probe::cc_sessions_dir(projects_root)
}

/// Codex twin of [`cc_pid_for_session`], keyed by the rollout UUID (the
/// slot's `session_id`) — NOT the rollout path, which comes back
/// kernel-canonicalized from the fd probe and is deliberately not matched on.
#[cfg(feature = "native")]
pub fn codex_pid_for_session(sessions_root: &std::path::Path, uuid: &str) -> Option<i32> {
    codex::live_codex_rollout_ids(sessions_root)?
        .pid_of
        .get(uuid)
        .copied()
}

/// grok twin, keyed by the session id (== the transcript's parent-dir name)
/// against grok's own `active_sessions.json` registry under `grok_root`
/// (= `grok_home()`, the registry file's parent).
#[cfg(feature = "native")]
pub fn grok_pid_for_session(grok_root: &std::path::Path, session_id: &str) -> Option<i32> {
    grok::live_grok_session_ids(grok_root)?
        .pid_of
        .get(session_id)
        .copied()
}

/// omp twin, keyed by the `omp_id_from_path` stem CHAIN (the slot's
/// `session_id`, so a nested subagent resolves its OWN pid — which is the
/// parent's, since omp subagents run in-process and inherit the fd).
/// `sessions_root` is `omp_sessions_dir()`; the probe applies its own
/// first-party-layout gate, so a replay root resolves nothing.
#[cfg(feature = "native")]
pub fn omp_pid_for_session(sessions_root: &std::path::Path, session_id: &str) -> Option<i32> {
    omp::live_omp_session_ids_for_focus(sessions_root)?
        .pid_of
        .get(session_id)
        .copied()
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
#[cfg(any(feature = "native", feature = "harness"))]
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
// `doc(hidden)` as `registry` is: read only by pixtuoid's `drift_watch`.
#[cfg(any(test, feature = "upstream"))]
#[doc(hidden)]
pub mod upstream;

#[cfg(all(test, unix, feature = "native"))]
mod focus_pid_tests {
    use super::*;

    #[test]
    fn cc_pid_for_session_hits_misses_and_tolerates_garbage() {
        let home = tempfile::tempdir().unwrap();
        let projects = home.path().join("projects");
        let sessions = home.path().join("sessions");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        // Our own pid is alive by construction.
        std::fs::write(
            sessions.join("self.json"),
            serde_json::json!({
                "pid": std::process::id(),
                "sessionId": "focus-sess",
                "startedAt": cc_probe::pid_start_time_secs(std::process::id() as i32).unwrap_or(0) * 1000,
                "status": "idle"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(sessions.join("junk.json"), "not json {{{").unwrap();

        assert_eq!(
            cc_pid_for_session(&projects, "focus-sess"),
            Some(std::process::id() as i32),
            "hit: the session's live registry pid"
        );
        assert_eq!(
            cc_pid_for_session(&projects, "unknown-sess"),
            None,
            "miss: unknown session resolves nothing"
        );
        // A NON-standard projects root (file_name != "projects") derives no
        // registry — the custom --projects-root replay case resolves nothing.
        assert_eq!(cc_pid_for_session(home.path(), "focus-sess"), None);
    }

    #[test]
    fn codex_pid_for_session_misses_on_unknown_uuid() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(codex_pid_for_session(root.path(), "0000-none"), None);
    }

    #[test]
    fn cc_registry_dir_derives_the_sibling_only_for_the_standard_layout() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            cc_registry_dir(&home.path().join("projects")),
            Some(home.path().join("sessions")),
            "the standard layout derives the SIBLING sessions dir"
        );
        // A custom --projects-root replay (file_name != "projects") derives
        // nothing, so those runs keep the pure-mtime gate.
        assert_eq!(cc_registry_dir(&home.path().join("fixture")), None);
    }

    #[test]
    fn grok_pid_for_session_binds_a_live_registry_pid_and_misses_otherwise() {
        let root = tempfile::tempdir().unwrap();
        let me = std::process::id();
        // No `opened_at`: a fixed stamp would predate this process's kernel
        // start and the recycle guard would (correctly) drop the entry, so the
        // binder is exercised on the pid-alive-only path.
        std::fs::write(
            root.path().join("active_sessions.json"),
            format!(r#"[{{"session_id":"focus-grok","pid":{me},"cwd":"/r/a"}}]"#),
        )
        .unwrap();

        assert_eq!(
            grok_pid_for_session(root.path(), "focus-grok"),
            Some(me as i32),
            "hit: the session's own live registry pid"
        );
        assert_eq!(grok_pid_for_session(root.path(), "unknown-sess"), None);
    }

    /// Like the Codex twin this is miss-only: a HIT needs an fd held by a
    /// process the probe recognizes (`bun`/`omp`), which a test binary is not,
    /// so this is a WIRING smoke test — both roots miss for different reasons
    /// and it cannot tell them apart. The layout gate itself has teeth in
    /// `omp::native`'s `probe_root_requires_first_party_layout`.
    #[test]
    fn omp_pid_for_session_gates_on_the_first_party_layout() {
        let root = tempfile::tempdir().unwrap();
        // An arbitrary replay root: rejected before the probe even runs.
        assert_eq!(omp_pid_for_session(root.path(), "any-session"), None);
        // First-party shape: passes the gate, so the probe itself runs.
        let sessions = root.path().join(".omp").join("agent").join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        assert_eq!(omp_pid_for_session(&sessions, "any-session"), None);
    }
}
