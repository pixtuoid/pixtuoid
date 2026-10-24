//! ONE row per agent CLI. A column must BOTH be picked by NAME by a generic
//! caller AND be depended on by production, not merely reported (`ActivityRecency`,
//! `should_seed_at_eof` fail that half). `home_env` is the deliberate exception:
//! its value is not the datum but the QUESTION a struct literal forces, after a
//! checklist bullet let three sources ship an unverified resolver (#880/#343/#342/#195).

use crate::source::decoder::DecodeResult as Result;
use serde_json::{Map, Value};

use crate::source::decoder::{
    CwdExtractor, IdDeriver, LineDecoder, ParentExtractor, PathFilter, accept_all_paths,
    default_id_from_path, extract_top_level_cwd,
};
use crate::source::{
    AgentEvent, antigravity, claude_code, codewhale, codex, copilot, cursor, dsh, grok, hermes,
    kimi, omp, openclaw, opencode, reasonix,
};

/// The wire NAME of the per-call tool id in this source's hook envelope, read by
/// the shared arms. It is a registry row and not a per-source copy of those arms
/// because reading the wrong name is SILENT — the field is optional, so a
/// mis-spelling is indistinguishable from an absent id, and every kimi tool call
/// decoded to `None` for the whole source's life.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolIdKey {
    /// `tool_use_id` — Claude Code's spelling, and every CC-shaped envelope's
    /// but Kimi's.
    ToolUse,
    /// `tool_call_id` — Kimi's (capture-verified: `fixtures/kimi/tool-run` never
    /// carries `tool_use_id`).
    ToolCall,
}

impl ToolIdKey {
    /// The JSON key to read the per-call id from.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::ToolUse => "tool_use_id",
            Self::ToolCall => "tool_call_id",
        }
    }
}

/// The JSON object a hook or presence decoder reads, so none re-checks its shape.
pub type HookObject = Map<String, Value>;

/// How a source's hook payloads decode. The source's own decoder runs FIRST,
/// before any shared field requirement, so an alien envelope (no `session_id` at
/// all) can still decode; TYPED by whether it may decline, so a
/// [`Self::ClaimsAll`] row has no `Option` to get wrong and no shared-arm setting
/// it would never read.
#[derive(Debug, Clone, Copy)]
pub enum HookDecoding {
    /// The shared CC-shaped arms, keyed on `session_id`, which `extend` EXTENDS:
    /// `Ok(Some(events))` short-circuits, `Ok(None)` DECLINES and falls through
    /// to the shared arms, `Err` propagates.
    Shared {
        /// The per-call tool id's wire name.
        tool_id_key: ToolIdKey,
        extend: fn(&HookObject) -> Result<Option<Vec<AgentEvent>>>,
    },
    /// CLAIMS every event: an alien-envelope source (no shared
    /// `hook_event_name`/`session_id` for the shared arms to key on) whose
    /// decoder handles EVERYTHING and constructs its own AgentIds. It can NOT
    /// decline, so a payload never silently falls through to the shared arms.
    ClaimsAll(fn(&HookObject) -> Result<Vec<AgentEvent>>),
}

/// Reducer-facing capability flags — stable facts about the source's wire
/// protocol, NOT policy names, so a future CLI picks values truthfully and the
/// policy falls out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceCaps {
    /// Does a CLEAN exit leave any end signal at all (a SessionEnd hook and/or
    /// a JSONL end marker — best-effort counts; "none of any kind" is the bar
    /// for `false`)? When false, the stale-sweep is the ONLY reaper a closed
    /// session ever gets.
    pub(crate) has_exit_signal: bool,
    /// Does a live-but-swept session WALK BACK IN on the user's next prompt (a
    /// `UserPromptSubmit`-class event re-emitting `SessionStart`)? The safety
    /// precondition for the short idle reaper: its only false positive (a live
    /// session idle past the window) must self-heal.
    pub(crate) resurrects_on_prompt: bool,
    /// Are subagent delegations invisible on this source's event stream (a
    /// window in which the PARENT's own stream goes quiet)? When true, a Delegating slot's
    /// `last_event_at` freezes for the whole delegation, so the reducer gives it
    /// the Waiting-class stale window instead of sweeping mid-delegation.
    pub(crate) delegations_are_hook_silent: bool,
    /// What a subagent dispatch's end says about its child.
    pub(crate) child_end_inference: ChildEndInference,
    /// Does a subagent's hook arrive under its parent's key, naming no child?
    /// Then while the parent has a Task in flight its hook activity is the
    /// child's, and the reducer drops it.
    pub(crate) child_hooks_unnamed: bool,
}

/// What a subagent dispatch's end says about its child; where it says the
/// child returned, the end walks the parent's whole subtree out (b1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildEndInference {
    /// Nothing: a dispatch can return while its child runs, and each child
    /// turn ends by its own event.
    None,
    /// A tracked dispatch's end means its child returned: the dispatch blocks
    /// until then.
    TaskDrain,
    /// As `TaskDrain`, and the end may carry no id: no end event names the
    /// child.
    AnyTaskEnd,
    /// Nothing, and the child is a session that outlives its turns: only its
    /// deletion ends it.
    Durable,
}

impl ChildEndInference {
    /// Whether a dispatch's end means its child returned (b1); `tracked` when
    /// the end names its dispatch.
    pub(crate) fn dispatch_end_ends_child(self, tracked: bool) -> bool {
        match self {
            Self::TaskDrain => tracked,
            Self::AnyTaskEnd => true,
            Self::None | Self::Durable => false,
        }
    }
}

impl SourceCaps {
    /// All-false caps for a `Daemon` source: it creates no `AgentSlot`s to reap.
    pub(crate) const INERT_DAEMON: SourceCaps = SourceCaps {
        has_exit_signal: false,
        resurrects_on_prompt: false,
        delegations_are_hook_silent: false,
        child_end_inference: ChildEndInference::None,
        child_hooks_unnamed: false,
    };

    /// The short-idle-reaper policy, derived: only safe when the sweep is the
    /// sole reaper (`!has_exit_signal`) AND the false positive self-heals
    /// (`resurrects_on_prompt`).
    pub(crate) fn short_idle_reap(&self) -> bool {
        !self.has_exit_signal && self.resurrects_on_prompt
    }
}

/// One agent CLI's cross-source facts: `const` data with fn pointers.
#[derive(Debug)]
pub struct SourceDescriptor {
    /// Stable lowercase id — MUST equal the module's `SOURCE_NAME`.
    pub name: &'static str,
    /// Exactly 2 chars; applied at `SessionStart` and reinforced idempotently by
    /// the JSONL label derivers.
    pub label_prefix: &'static str,
    /// The CLI version this build's decoder + fixtures were last verified
    /// against: the release, prerelease included (`0.2.0-rc.2`), never a
    /// build id.
    /// `"unknown"`, NOT `""`, where we have no fixed anchor — `pixtuoid doctor`
    /// only flags SKEW when [`parse_version`] reads one, with this row's
    /// [`Self::version_hyphen`].
    /// Pinned to the highest recorded capture's `version` banner by
    /// `a_recorded_capture_anchors_its_sources_verified_version`, so a
    /// re-record bumps it.
    pub verified_version: &'static str,
    /// argv to probe the installed CLI version. `None` = no stable CLI binary;
    /// `doctor` runs it best-effort and degrades to "version: unknown".
    pub version_probe: Option<&'static [&'static str]>,
    /// What a `-` after the numbers of this CLI's version starts.
    pub version_hyphen: Hyphen,
    /// The CLI-specific env var relocating the root
    /// [`crate::source::resolved_source_root`] reports. Compiler-forced, which
    /// is the recurrence gate for the #880 class; pinned truthful by
    /// `every_declared_home_env_actually_moves_that_sources_root`.
    ///
    /// **`None` does NOT mean "no override exists".** A hook-only source's root
    /// is its installer's config path, binary-side and out of core's reach —
    /// `OPENCODE_CONFIG_DIR` and `REASONIX_HOME` are declared there. Where a CLI
    /// has several (omp), this names the most direct one.
    pub home_env: Option<&'static str>,
    /// What KIND of source this is. Consumers read through the accessors so the
    /// enum shape stays an internal detail.
    pub(crate) kind: SourceKind,
}

/// The transcript half of an `Agent` row. Bundling the fns makes the
/// all-or-nothing pairing structural: a row is either transcript-bearing (every
/// fn) or hook-only (`transcript: None`), never half-populated.
#[derive(Debug)]
pub struct Transcript {
    /// JSONL line decoder.
    pub(crate) line_decoder: LineDecoder,
    /// How this source's transcript PATH becomes the session id its
    /// `SessionStart` is keyed on. Read by the JSONL watcher AND by the offline
    /// `harness::Drive` — ONE derivation, so a driven transcript keys exactly as
    /// production does. `default_id_from_path` is the path-keyed default.
    pub(crate) id_from_path: IdDeriver,
    /// WHICH `.jsonl` files under this source's root are its transcripts. Read
    /// by the JSONL watcher AND by any offline driver that WALKS a tree — a
    /// census over the unfiltered set counts files production never reads.
    /// `accept_all_paths` is the admit-everything default.
    #[cfg_attr(
        not(any(feature = "native", feature = "harness")),
        expect(dead_code, reason = "only a tree walk reads it")
    )]
    pub(crate) path_filter: PathFilter,
    /// First-sight cwd extractor for the walker's transcript head scan. The
    /// walker dispatches by the SCANNED source, so one source's shape is never
    /// tried against another's transcript.
    pub(crate) cwd_extractor: CwdExtractor,
    /// First-sight parent extractor for the same head scan, which stops at the
    /// cwd's line: a parent must sit on or before it.
    #[cfg_attr(
        not(any(feature = "native", feature = "harness")),
        expect(dead_code, reason = "only a tree walk reads it")
    )]
    pub(crate) parent_extractor: Option<ParentExtractor>,
}

/// Where this source's OS pid comes from — a DATA-only capability: this const
/// table compiles to wasm, so a native-only probe FN POINTER can never live here
/// (the probes stay in the BINARY's `focus::resolve_pid`). ONE source of truth
/// for the focus stamp gate, the hook exit-watch binding and the doctor report
/// bucketing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusChannel {
    /// The shim RESOLVES the CLI's pid into `_pid` by walking past the runner's
    /// interposed shell (`pixtuoid-hook`'s `cli_pid`) — a `cmd /C` on Windows, a
    /// `$SHELL -c` wrapper on Unix, which need NOT exec-replace itself with the
    /// shim (#896). How far to trust the resolved pid is the [`Corroboration`].
    ShimStamp(Corroboration),
    /// The source's own runtime stamps `_pid` — cross-platform, and survives
    /// even where the shim sends nothing.
    PluginStamp,
    /// The source's runtime stamps a background server's `_pid`, which every
    /// session shares and no window owns: the exit watch binds it, a focus
    /// click no-ops.
    ServerStamp,
    /// Pid resolves through a recycle-guarded liveness probe at CLICK time; hook
    /// stamps are never trusted (the shim resolves the nearest non-shell
    /// ancestor, not necessarily the CLI). The probe fn itself lives in the binary.
    TranscriptProbe,
    /// No pid channel — a focus click silently no-ops (the ONE failure rule).
    Unsupported,
}

/// How many sightings a shim-resolved pid needs before the exit watch acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corroboration {
    /// A wrapper the walk does not recognise can sit between the CLI and our
    /// hook command (Cursor `eval`s it mid-script, #896), so the resolved pid may
    /// not be the CLI: `HookPidWatch` waits for a second consecutive sighting.
    Twice,
    /// The runner reaches our hook command only through a shell the walk
    /// recognises, so the first sighting is the CLI. The second can never come
    /// from a turn that ends the process (its parent is gone before the shim walks).
    OnceViaShell,
}

impl FocusChannel {
    /// Whether a hook-envelope `_pid` stamp is this session's own focusable
    /// process.
    #[cfg(feature = "native")]
    pub(crate) fn stamps_focus_pid(self) -> bool {
        matches!(self, FocusChannel::ShimStamp(_) | FocusChannel::PluginStamp)
    }
}

/// The two source classes, type-isolated: the registry-driven demux and the
/// `daemon_sources()` sweep loop dispatch on this, so a 2nd daemon needs no
/// `handle_conn` edit and no new reducer arm.
#[derive(Debug)]
pub(crate) enum SourceKind {
    /// Produces `AgentEvent`s → `SceneState::agents` → a desk sprite.
    Agent {
        /// `None` = a HOOK-ONLY agent: the fixture harness then accepts a
        /// transcript-less, hook-payloads-only scenario for it.
        transcript: Option<Transcript>,
        /// `None` = a TRANSCRIPT-ONLY agent with no hook transport (no install
        /// target, the shim never fires for it). A non-daemon row must carry a
        /// transcript OR a hook — both `None` would decode nothing.
        hook: Option<HookDecoding>,
        caps: SourceCaps,
        /// The focus-jump pid channel. Lives INSIDE `Agent` so a daemon can't
        /// carry one — a mascot isn't clickable.
        focus: FocusChannel,
    },
    /// Produces `DaemonPresenceUpdate`s → `SceneState::daemons` → a wandering
    /// mascot. It emits ZERO `AgentEvent`s: the `HookRouter` demux routes its
    /// payloads to the sibling channel via `presence_decoder`.
    Daemon {
        #[cfg_attr(
            not(any(feature = "native", feature = "harness")),
            expect(dead_code, reason = "only the hook demux and the recorder read it")
        )]
        presence_decoder: PresenceDecoder,
    },
}

impl SourceDescriptor {
    /// A `Daemon`-kind row renders a mascot, not a desk sprite.
    pub fn is_daemon(&self) -> bool {
        matches!(self.kind, SourceKind::Daemon { .. })
    }

    fn transcript(&self) -> Option<&Transcript> {
        match &self.kind {
            SourceKind::Agent { transcript, .. } => transcript.as_ref(),
            SourceKind::Daemon { .. } => None,
        }
    }

    /// The JSONL line decoder (`None` for a hook-only agent AND every daemon).
    pub fn line_decoder(&self) -> Option<LineDecoder> {
        self.transcript().map(|t| t.line_decoder)
    }

    /// The first-sight cwd extractor (`None` for a hook-only agent or a daemon).
    pub(crate) fn cwd_extractor(&self) -> Option<CwdExtractor> {
        self.transcript().map(|t| t.cwd_extractor)
    }

    /// The first-sight parent extractor (`None` without one).
    #[cfg(any(feature = "native", feature = "harness"))]
    pub(crate) fn parent_extractor(&self) -> Option<ParentExtractor> {
        self.transcript().and_then(|t| t.parent_extractor)
    }

    /// The transcript path→session-id derivation (`None` without a transcript).
    pub(crate) fn id_deriver(&self) -> Option<IdDeriver> {
        self.transcript().map(|t| t.id_from_path)
    }

    /// Which `.jsonl` files are this source's transcripts (`None` without a
    /// transcript tree to walk).
    #[cfg(any(feature = "native", feature = "harness"))]
    pub(crate) fn path_filter(&self) -> Option<PathFilter> {
        self.transcript().map(|t| t.path_filter)
    }

    /// The focus-jump pid channel (`Unsupported` for a daemon).
    pub fn focus_channel(&self) -> FocusChannel {
        match &self.kind {
            SourceKind::Agent { focus, .. } => *focus,
            SourceKind::Daemon { .. } => FocusChannel::Unsupported,
        }
    }

    /// The hook-decoding spec (`None` for a daemon and for a transcript-only
    /// agent). `decode_hook_payload` already defaults a missing spec.
    pub fn hook(&self) -> Option<&HookDecoding> {
        match &self.kind {
            SourceKind::Agent { hook, .. } => hook.as_ref(),
            SourceKind::Daemon { .. } => None,
        }
    }

    /// Reducer capability flags — an INERT all-false default for a daemon.
    pub(crate) fn caps(&self) -> SourceCaps {
        match &self.kind {
            SourceKind::Agent { caps, .. } => *caps,
            SourceKind::Daemon { .. } => SourceCaps::INERT_DAEMON,
        }
    }

    /// The daemon's presence decoder (`None` for an agent source).
    #[cfg(any(feature = "native", feature = "harness"))]
    pub(crate) fn presence_decoder(&self) -> Option<PresenceDecoder> {
        match &self.kind {
            SourceKind::Daemon { presence_decoder } => Some(*presence_decoder),
            SourceKind::Agent { .. } => None,
        }
    }
}

pub const REGISTRY: &[SourceDescriptor] = &[
    CLAUDE_CODE,
    CODEX,
    ANTIGRAVITY,
    DSH,
    REASONIX,
    CODEWHALE,
    OPENCODE,
    COPILOT,
    CURSOR,
    HERMES,
    OMP,
    OPENCLAW,
    GROK,
    KIMI,
];

/// Linear scan — a handful of entries; a map would cost more ceremony than it
/// saves.
pub fn descriptor_for(name: &str) -> Option<&'static SourceDescriptor> {
    REGISTRY.iter().find(|d| d.name == name)
}

/// What a `-` after a version's numbers starts, which only the CLI knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hyphen {
    /// A semver prerelease, ordered below its release (dsh's `0.2.0-rc.2`).
    Prerelease,
    /// A build id, which no comparison reads (cursor's `2026.09.18-9a7762b`).
    Build,
}

/// The version in a CLI's `--version` banner, or in a `verified_version`: the
/// first token, split on whitespace and `/`, shaped
/// `v?MAJOR.MINOR[.PATCH][-SUFFIX]`, its `-` read as `hyphen` says.
/// One grammar for every CLI, so a banner whose wording changed between
/// releases still reads, and a parenthesized date or sha (`(2026.8.31)`,
/// `(e178c39)`) never does. A component's leading zeros drop (CalVer
/// `2026.09.18`), and a dot no digit follows ends the version (copilot's
/// `1.0.62.`). Compare two with `cmp_precedence`, which skips build ids.
pub fn parse_version(banner: &str, hyphen: Hyphen) -> Option<semver::Version> {
    banner
        .split(|c: char| c.is_whitespace() || c == '/')
        .find_map(|token| version_token(token, hyphen))
}

fn version_token(token: &str, hyphen: Hyphen) -> Option<semver::Version> {
    let token = token.strip_prefix(['v', 'V']).unwrap_or(token);
    let bytes = token.as_bytes();
    let mut end = 0;
    while end < bytes.len()
        && (bytes[end].is_ascii_digit()
            || (bytes[end] == b'.' && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)))
    {
        end += 1;
    }
    let mut parts = token[..end].split('.').map(str::parse::<u64>);
    let major = parts.next()?.ok()?;
    let minor = parts.next()?.ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    if parts.next().is_some() {
        return None;
    }
    let mut version = semver::Version::new(major, minor, patch);
    let rest = &token[end..];
    if let Some(suffix) = rest.strip_prefix('-').map(identifiers) {
        match hyphen {
            Hyphen::Prerelease => version.pre = semver::Prerelease::new(suffix).ok()?,
            Hyphen::Build => version.build = semver::BuildMetadata::new(suffix).ok()?,
        }
    }
    Some(version)
}

/// The dot-separated identifiers `s` starts with: what a `-` suffix runs to
/// before punctuation that ends the token.
fn identifiers(s: &str) -> &str {
    let run = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        .map_or(s, |end| &s[..end]);
    run.trim_end_matches(['.', '-'])
}

/// Every registered source's stable id, in `REGISTRY` order — THE roster
/// authority.
pub fn registered_source_names() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|d| d.name)
}

/// The first-sight cwd extractor for the source being scanned: the walker calls
/// this with the source it is scanning, so one source's head shape is never
/// tried against another's transcript — a foreign-shaped line (a codex-style
/// `payload.cwd` inside a CC transcript) would otherwise be a cross-source false
/// positive on the identity-bearing cwd. Falls back to the shared top-level
/// `cwd` shape for a source with no row.
pub fn cwd_extractor_for(source: &str) -> CwdExtractor {
    descriptor_for(source)
        .and_then(|d| d.cwd_extractor())
        .unwrap_or(extract_top_level_cwd)
}

/// `source`'s first-sight parent extractor, if its transcripts name a parent.
#[cfg(any(feature = "native", feature = "harness"))]
pub(crate) fn parent_extractor_for(source: &str) -> Option<ParentExtractor> {
    descriptor_for(source).and_then(|d| d.parent_extractor())
}

/// How this source keys a transcript path onto a session id. THE single
/// derivation: [`crate::source::jsonl::JsonlWatcher::new`] defaults to it and
/// the offline `harness::Drive` seeds with it, so a driven transcript lands on
/// the SAME `AgentId` the watcher would have registered — key it any other way
/// and every JSONL event is a silent no-op against an unknown id. Falls back to
/// the path-keyed default for an unregistered source name.
pub fn id_deriver_for(source: &str) -> IdDeriver {
    descriptor_for(source)
        .and_then(|d| d.id_deriver())
        .unwrap_or(default_id_from_path)
}

/// Which `.jsonl` files under this source's root ARE its transcripts. Same
/// two-reader shape as [`id_deriver_for`]: an offline driver that walks a tree
/// must select the SAME files or its census counts what production never reads.
/// Admits everything for an unregistered source name.
#[cfg(any(feature = "native", feature = "harness"))]
pub(crate) fn path_filter_for(source: &str) -> PathFilter {
    descriptor_for(source)
        .and_then(|d| d.path_filter())
        .unwrap_or(accept_all_paths)
}

/// A daemon source's wire decoder: its envelope → the sending INSTANCE's
/// identity plus its presence deltas. Handed to the `HookRouter` demux, which
/// therefore never learns what makes two instances of a daemon different.
pub(crate) type PresenceDecoder = fn(&HookObject) -> Result<crate::source::daemon::DecodedPresence>;

/// The presence decoder for a daemon source, or `None` for an agent source.
#[cfg(any(feature = "native", feature = "harness"))]
pub(crate) fn presence_decoder_for(source: &str) -> Option<PresenceDecoder> {
    descriptor_for(source).and_then(|d| d.presence_decoder())
}

/// Every registered daemon source paired with its presence-decay profile, so
/// each daemon decays on its own TTL with no hardcoded name.
pub fn daemon_sources() -> impl Iterator<Item = (&'static str, crate::source::daemon::PresenceTtl)>
{
    REGISTRY
        .iter()
        .filter(|d| d.is_daemon())
        .map(|d| (d.name, crate::source::daemon::PresenceTtl::DEFAULT))
}

const CLAUDE_CODE: SourceDescriptor = SourceDescriptor {
    name: claude_code::SOURCE_NAME,
    label_prefix: "cc",
    verified_version: "2.1.309",
    version_probe: Some(&["claude", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: Some("CLAUDE_CONFIG_DIR"),
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: claude_code::decode_cc_line,
            // The filename stem IS the session UUID the hook keys on.
            id_from_path: claude_code::cc_id_from_path,
            // The workflow orchestrator's foreign-schema `journal.jsonl` lives
            // in the SAME projects tree.
            path_filter: claude_code::admits_transcript,
            cwd_extractor: extract_top_level_cwd,
            parent_extractor: None,
        }),
        hook: Some(HookDecoding::Shared {
            tool_id_key: ToolIdKey::ToolUse,
            // They swap the event's SUBJECT (child AgentId ≠ session AgentId), which
            // the shared arms can't express; Stop is a fleet subagent's ONLY end (#241).
            extend: claude_code::decode_cc_hook_custom,
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
        },
        focus: FocusChannel::TranscriptProbe,
    },
};

const CODEX: SourceDescriptor = SourceDescriptor {
    name: codex::SOURCE_NAME,
    label_prefix: "cx",
    verified_version: "0.164.0",
    version_probe: Some(&["codex", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: Some("CODEX_HOME"),
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: codex::decode_codex_line,
            // The rollout filename's trailing UUID (the stem also has a timestamp).
            id_from_path: codex::codex_id_from_path,
            path_filter: accept_all_paths,
            cwd_extractor: codex::extract_codex_cwd,
            parent_extractor: Some(codex::extract_codex_parent),
        }),
        hook: Some(HookDecoding::Shared {
            tool_id_key: ToolIdKey::ToolUse,
            // A child's events change their SUBJECT, and a root's `spawn_agent`
            // is a Task: neither is expressible in the shared arms.
            extend: codex::decode_codex_hook_custom,
        }),
        caps: SourceCaps {
            // Still false after #710 registered Codex's SessionEnd hook: it is
            // teardown-only, so flipping this regresses abrupt exits to a 30-min reap.
            has_exit_signal: false,
            resurrects_on_prompt: true,
            delegations_are_hook_silent: false,
            // `spawn_agent` returns at launch, and a child's SubagentStop ends
            // it (`codex-rs/core/src/hook_runtime.rs:401-437` at rust-v0.163.0).
            child_end_inference: ChildEndInference::None,
            child_hooks_unnamed: false,
        },
        focus: FocusChannel::TranscriptProbe,
    },
};

const ANTIGRAVITY: SourceDescriptor = SourceDescriptor {
    name: antigravity::SOURCE_NAME,
    label_prefix: "ag",
    verified_version: "1.4.2",
    version_probe: Some(&["agy", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: antigravity::decode_ag_line,
            id_from_path: antigravity::ag_id_from_path,
            path_filter: antigravity::admits_transcript,
            // AG step lines carry no cwd field at all; the cwd rides the
            // `SessionStart` hook.
            cwd_extractor: extract_top_level_cwd,
            parent_extractor: None,
        }),
        hook: Some(HookDecoding::ClaimsAll(antigravity::decode_ag_hook_payload)),
        // Both false is VERIFIED: no end marker (`status` is per-step, always DONE)
        // and no end hook, and no prompt-time hook re-registers a swept session.
        // Flipping `resurrects_on_prompt` for the 5-min reaper strands a live idle
        // session. An exit rides the shim-stamped pid.
        caps: SourceCaps {
            has_exit_signal: false,
            resurrects_on_prompt: false,
            delegations_are_hook_silent: false,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
        },
        focus: FocusChannel::ShimStamp(Corroboration::Twice),
    },
};

/// HOOK-ONLY: Reasonix v2 session files are full-rewritten per turn
/// (untailable) and its hook envelope is ALIEN (camelCase, `event`
/// discriminator, `sessionId` the identity), so the custom decoder claims
/// every event.
const REASONIX: SourceDescriptor = SourceDescriptor {
    name: reasonix::SOURCE_NAME,
    label_prefix: "rx",
    verified_version: "2.44.0",
    version_probe: Some(&["reasonix", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll(reasonix::decode_rx_hook_payload)),
        caps: SourceCaps {
            // SessionEnd fires on clean exit — best-effort counts.
            has_exit_signal: true,
            // UserPromptSubmit re-emits SessionStart, so a swept-but-live
            // session walks back in on the next prompt.
            resurrects_on_prompt: true,
            // A subagent's hooks carry its own session id, the parent's
            // suffixed with its role (`internal/ext/hook/runner.go:57-66` at
            // v2.39.0), so the parent's slot emits NOTHING until the dispatch
            // tool's PostToolUse.
            delegations_are_hook_silent: true,
            // A child's session fires no end; a skill tool's child is keyed
            // by a call id its parent's tool hooks don't carry
            // (`internal/assembly/boot/delegation.go:127-133`,
            // `internal/ext/hook/hook.go:827-847` at v2.39.0).
            child_end_inference: ChildEndInference::AnyTaskEnd,
            child_hooks_unnamed: false,
        },
        focus: FocusChannel::ShimStamp(Corroboration::Twice),
    },
};

/// HOOK-ONLY: DeepSeek Harness persists sessions as zstd-compressed
/// concatenated frames (`session.jsonl.zstd` — not line-readable, format v0
/// with no migration promise), so the only plane is the pixtuoid cordis
/// plugin (`pixtuoid/src/install/dsh_plugin.mjs`) mounted via the home-level
/// `$DSH_HOME/cordis.patch.yml`, claiming every payload here (the mount and
/// never-block stories live on the template and its tests). One dsh process
/// hosts many sessions (the `web` profile is a server), the opencode pid
/// model.
const DSH: SourceDescriptor = SourceDescriptor {
    name: dsh::SOURCE_NAME,
    label_prefix: "ds",
    verified_version: "0.2.0-rc.2",
    version_probe: Some(&["dsh", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    // `DSH_HOME` is honored INSTALLER-side (the plugin file + patch row live
    // under it); hook-only, so core resolves no root to relocate.
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll(dsh::decode_dsh_payload)),
        caps: SourceCaps {
            // `agent/disposed` + the plugin's own effect-disposer sweep fire
            // on clean quit AND signal shutdown (dsh runs disposers there
            // too); SIGKILL and an uncaught crash (upstream installs no
            // `uncaughtException` handler, so no disposer runs) fall to
            // `HookPidWatch` via the stamped pid.
            has_exit_signal: true,
            // Sessions are persistent; a `--resume` boots a NEW dsh whose
            // plugin emits a fresh `session_start` on the same id — the
            // SessionStart arm's ordinary re-registration, not this flag.
            resurrects_on_prompt: false,
            // A delegation freezes the PARENT's stream either way: the
            // local child's events all carry its OWN session id (the
            // delegation capture), and a remote provider
            // (`dsh-subagent-claude-code`/`-codex`/`-acp`) publishes no
            // local child at all. `true` can only over-retain a dead slot.
            delegations_are_hook_silent: true,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
        },
        // The plugin runs in-process (a cordis plugin in the launcher's one
        // root context; no worker mode exists for plugins), so its stamped
        // `process.pid` is dsh's own.
        focus: FocusChannel::PluginStamp,
    },
};

/// HOOK-ONLY: CodeWhale has NO tailable transcript (`rollout_path` is an unused
/// `state.db` column; saved sessions are full-snapshot rewrites; headless
/// `codewhale exec` runs hooks-off — only the TUI fires hooks). Its hook
/// envelope is ALIEN (snake_case `event` discriminator, identity via
/// `DEEPSEEK_*` env vars), so the custom decoder claims every event — keyed on
/// cwd, because `session_id` is inconsistent across events.
const CODEWHALE: SourceDescriptor = SourceDescriptor {
    name: codewhale::SOURCE_NAME,
    label_prefix: "cw",
    verified_version: "0.10.1",
    version_probe: Some(&["codewhale", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll(codewhale::decode_cw_hook_payload)),
        caps: SourceCaps {
            // session_end fires on a clean TUI quit — best-effort counts.
            has_exit_signal: true,
            // message_submit re-emits SessionStart, so a swept-but-live session
            // walks back in on the next prompt.
            resurrects_on_prompt: true,
            // The recorded dispatch fires nothing of the child's own between
            // `subagent_spawn` and `subagent_complete`.
            delegations_are_hook_silent: true,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
        },
        focus: FocusChannel::ShimStamp(Corroboration::Twice),
    },
};

const OPENCODE: SourceDescriptor = SourceDescriptor {
    name: opencode::SOURCE_NAME,
    label_prefix: "oc",
    verified_version: "2.1.4",
    version_probe: Some(&["opencode", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll(opencode::decode_oc_hook_payload)),
        caps: SourceCaps {
            // A TUI quit ends nothing: the session lives on in the server.
            has_exit_signal: false,
            // The next prompt's `session.execution.started` decodes to
            // `SessionStart`.
            resurrects_on_prompt: true,
            // A `subagent` child is parented by its `session.created`'s `parentID`,
            // so its events keep the parent fresh (`scope::refresh_lineage`).
            delegations_are_hook_silent: false,
            // A `subagent` call can return while its child runs on
            // (`packages/core/src/tool/plugin/subagent.ts:229-243` at v2.0.32),
            // and a session ends only by `session.deleted`.
            child_end_inference: ChildEndInference::Durable,
            child_hooks_unnamed: false,
        },
        // The plugin runs in the background server every TUI attaches to
        // (`packages/client/src/service-contender.ts:19-20`), not in a TUI.
        focus: FocusChannel::ServerStamp,
    },
};

/// The DAEMON row: OpenClaw is one always-on gateway, not a per-session coding
/// agent — its backend `claude-cli` sessions already show up as `cc·`, so it
/// renders ONE presence-gated wandering lobster mascot instead.
const OPENCLAW: SourceDescriptor = SourceDescriptor {
    name: openclaw::SOURCE_NAME,
    label_prefix: "ok",
    verified_version: "2026.10.6",
    version_probe: Some(&["openclaw", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Daemon {
        presence_decoder: openclaw::decode_openclaw_hook_payload,
    },
};

/// GitHub Copilot CLI (`@github/copilot`). TRANSCRIPT-ONLY: the whole lifecycle
/// is persisted to `<copilot_home>/session-state/<id>/events.jsonl`, so it needs
/// no hook install target and no custom hook decoder. Sub-agents interleave in
/// the root file, keyed on the envelope `agentId`.
const COPILOT: SourceDescriptor = SourceDescriptor {
    name: copilot::SOURCE_NAME,
    label_prefix: "cp",
    verified_version: "1.0.98",
    version_probe: Some(&["copilot", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: Some("COPILOT_HOME"),
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: copilot::decode_copilot_line,
            id_from_path: copilot::copilot_id_from_path,
            path_filter: accept_all_paths,
            cwd_extractor: copilot::extract_copilot_cwd,
            parent_extractor: None,
        }),
        hook: None,
        caps: SourceCaps {
            // `session.shutdown` is a real persisted exit marker.
            has_exit_signal: true,
            // sessionId is constant across `--resume`, so a swept session does
            // not silently walk back in.
            resurrects_on_prompt: false,
            // The `task` dispatch emits `tool.execution_start`/`complete` AND
            // explicit `subagent.started`/`completed` events.
            delegations_are_hook_silent: false,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
        },
        focus: FocusChannel::Unsupported,
    },
};

/// HOOK-ONLY: Cursor CLI (`cursor-agent`) has no passively-observable
/// transcript — its stream-json NDJSON is per-invocation stdout and its on-disk
/// sessions are SQLite. The reachable seam is Cursor Hooks
/// (`~/.cursor/hooks.json`), whose envelope reuses CC's `hook_event_name` field
/// NAME with camelCase values, so the custom decoder claims every event and keys
/// on `session_id`. Subagents render FLAT: children run as independent sessions
/// with no parent-link on the wire.
const CURSOR: SourceDescriptor = SourceDescriptor {
    name: cursor::SOURCE_NAME,
    label_prefix: "cu",
    verified_version: "2026.09.18",
    version_probe: Some(&["cursor-agent", "--version"]),
    version_hyphen: Hyphen::Build,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll(cursor::decode_cursor_hook_payload)),
        caps: SourceCaps {
            // `sessionEnd` fires on clean completion. An abrupt exit rides the
            // shim-stamped pid, which reaches the CLI only because the ancestor walk
            // skips Cursor's wrapper shell (#896).
            has_exit_signal: true,
            // Each `cursor-agent` invocation is a NEW session_id, so a swept
            // session never walks back in.
            resurrects_on_prompt: false,
            // A `Task` dispatch gets NO `postToolUse`, and its children are separate
            // unlinkable sessions.
            delegations_are_hook_silent: true,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
        },
        focus: FocusChannel::ShimStamp(Corroboration::Twice),
    },
};

/// HOOK-ONLY: Hermes Agent (Nous Research) has no tailable transcript; the
/// reachable seam is Hermes shell hooks (the `config.yaml` under
/// `hermes::hermes_home`, which is NOT `~/.hermes` on Windows). The envelope
/// reuses CC's `hook_event_name` field NAME with snake_case values, so the
/// custom decoder claims every event and keys on `session_id` — not the
/// workspace, which would merge a user's concurrent instances.
const HERMES: SourceDescriptor = SourceDescriptor {
    name: hermes::SOURCE_NAME,
    label_prefix: "hm",
    verified_version: "0.22.2",
    version_probe: Some(&["hermes", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: Some("HERMES_HOME"),
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll(hermes::decode_hermes_hook_payload)),
        caps: SourceCaps {
            // `on_session_finalize`, fired by upstream's atexit `_run_cleanup` with
            // `reason="shutdown"`. NOT `on_session_end`, which despite the name is a
            // TURN boundary (`source/hermes.rs`).
            has_exit_signal: true,
            resurrects_on_prompt: false,
            // No subagent nesting on the wire — sessions render flat.
            delegations_are_hook_silent: false,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
        },
        focus: FocusChannel::ShimStamp(Corroboration::Twice),
    },
};

/// Grok Build (`grok`, xai-org/grok-build). TRANSCRIPT-BEARING **with** a hook
/// target. The decoder reads the camelCase envelope (`hookEventName`, `toolUseId`)
/// — the only spelling pre-1.0 sent, hence the claims-all custom decoder; 1.0.x
/// mirrors every key in CC's snake_case as well (`grok/tool-run-recorded`).
/// Coalescing rests on `sessionId == the transcript's parent-dir name` (upstream
/// joins the id into `session_dir`), mirrored by `grok_id_from_path`. Transcript
/// = `{grok_home}/sessions/<enc-cwd>/<session-id>/updates.jsonl`, append-only.
const GROK: SourceDescriptor = SourceDescriptor {
    name: grok::SOURCE_NAME,
    label_prefix: "gk",
    verified_version: "1.0.62",
    version_probe: Some(&["grok", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: Some("GROK_HOME"),
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: grok::decode_grok_line,
            id_from_path: grok::grok_id_from_path,
            // Session dirs carry rewrite-on-resume siblings (chat_history /
            // rewind_points / events.jsonl) that must never be tailed.
            path_filter: grok::is_updates_jsonl,
            // Always-None: grok lines carry no cwd — the URL-encoded group dir
            // does, applied via the watcher's cwd deriver.
            cwd_extractor: grok::extract_grok_cwd,
            parent_extractor: None,
        }),
        hook: Some(HookDecoding::ClaimsAll(grok::decode_grok_hook_payload)),
        caps: SourceCaps {
            // `session_end` does NOT fire on a plain TUI quit (the loop breaks without
            // draining the session actor), so the DOMINANT exit is signal-less; the
            // authority is the liveness probe over grok's `active_sessions.json`.
            has_exit_signal: false,
            // Every prompt fires `user_prompt_submit` (→ SessionStart), so a swept
            // LIVE session walks back in — which is what makes the short idle reap
            // safe for grok's untracked headless one-shots.
            resurrects_on_prompt: true,
            // A BLOCKING spawn's Task detail gets its post_tool_use at
            // completion, and a background spawn never mints a Task at all.
            delegations_are_hook_silent: false,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
        },
        // `active_sessions.json` maps session_id → pid with an `opened_at` recycle
        // guard; a shim stamp would shadow it with a walk carrying no guard (#527).
        focus: FocusChannel::TranscriptProbe,
    },
};

/// Oh My Pi (`omp`, omp.sh). HYBRID: the durable authority is the transcript
/// at `<omp_sessions_dir>/<encoded-cwd>/<ts>_<uuid>.jsonl`; the bridge
/// extension (`pixtuoid/src/install/omp_extension.ts`, auto-discovered from the agent
/// dir's `extensions/`) forwards what the transcript can never carry —
/// pre-persist presence, empty-session shutdown, the approval wait, and the
/// process's own `_pid` (#951). Both transports key on the sessionFile stem chain
/// ([`omp::omp_id_from_path`]), so they mint ONE AgentId per session — the
/// grok same-key pattern. Under `omp --no-extensions`, or with the bridge
/// broken or absent, the transcript path IS the old transcript-only source.
/// Subagents persist as SEPARATE nested files (`<parent-stem>/<taskId>.jsonl`),
/// parent-linked by path.
const OMP: SourceDescriptor = SourceDescriptor {
    name: omp::SOURCE_NAME,
    label_prefix: "om",
    verified_version: "18.12.1",
    version_probe: Some(&["omp", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: Some("PI_CODING_AGENT_DIR"),
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: omp::decode_omp_line,
            // The nested stem CHAIN, so a subagent task id can't collide across
            // sessions.
            id_from_path: omp::omp_id_from_path,
            path_filter: accept_all_paths,
            cwd_extractor: extract_top_level_cwd,
            parent_extractor: None,
        }),
        hook: Some(HookDecoding::ClaimsAll(omp::decode_omp_hook_payload)),
        caps: SourceCaps {
            // The `session_exit` entry is appended + flushed on every clean
            // teardown incl. SIGINT/SIGTERM (and the bridge fires
            // `session_shutdown` even for empty sessions the entry skips).
            // SIGKILL: with the bridge, `HookPidWatch` ends the stamped pid's
            // sessions promptly; without it, the stale-sweep.
            has_exit_signal: true,
            // The header `SessionStart` decodes once per transcript life. A
            // `--resume` fires the bridge's `session_start` on the SAME id
            // (`omp/fixtures/resume-recorded`), so an ended
            // session CAN walk back in — through the SessionStart arm's
            // ordinary parentless re-registration, not this prompt-resurrect
            // flag.
            resurrects_on_prompt: false,
            // The `task` dispatch emits a toolCall/toolResult pair on the parent
            // AND the child persists its own parent-linked transcript.
            delegations_are_hook_silent: false,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
        },
        // The bridge extension runs IN-PROCESS (upstream: extensions share
        // the runtime), so its stamped `process.pid` IS omp's own —
        // PluginStamp trust. The router stamps the kernel start marker; the
        // click-time compare refuses a recycled pid (#527). The flip also
        // puts omp on `HookPidWatch` (prompt exits) and makes a bridge-only,
        // never-persisted session focusable from birth. Stamp-less shapes —
        // `--no-extensions`, a session predating the bridge install — still
        // focus through the append-fd probe: `resolve_pid` dispatches by
        // source name and reads no channel.
        focus: FocusChannel::PluginStamp,
    },
};

/// HOOK-ONLY: Kimi Code CLI (`kimi`, MoonshotAI) DOES persist a transcript
/// (`<KIMI_CODE_HOME>/sessions/.../wire.jsonl`), but that format is EXPLICITLY
/// unstable (a versioned `metadata` envelope, a `wire/migration/` module,
/// undocumented op `type` strings), so pixtuoid does NOT watch it. Its hooks are
/// the stable surface and the envelope is CLAUDE-CODE-SHAPED, so it rides the
/// SHARED arms keyed on `session_id`; the custom `Extend` decoder claims ONLY
/// Kimi's `PostToolUseFailure`/`StopFailure` variants and declines the rest.
const KIMI: SourceDescriptor = SourceDescriptor {
    name: kimi::SOURCE_NAME,
    label_prefix: "km",
    verified_version: "2.3.1",
    version_probe: Some(&["kimi", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::Shared {
            // The ONE source that does not spell the per-call id `tool_use_id`.
            tool_id_key: ToolIdKey::ToolCall,
            extend: kimi::decode_kimi_hook_custom,
        }),
        caps: SourceCaps {
            // `SessionEnd` fires on exit — best-effort counts; abrupt exits fall
            // to the stale-sweep.
            has_exit_signal: true,
            // SessionStart is the registration carrier, not UserPromptSubmit, so
            // a swept session does not walk back in.
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
        },
        // kimi-code 2.2.0 spawns every hook with `shell: true`, which Node runs as
        // `/bin/sh -c` on Unix and `%ComSpec%` on Windows
        // (`packages/agent-core-v2/src/features/externalHooks/internal/runHook.ts:66-67`,
        // https://nodejs.org/api/child_process.html): re-check both if either changes.
        focus: FocusChannel::ShimStamp(Corroboration::OnceViaShell),
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_banner_reads_as_its_first_version_shaped_token() {
        let version = |s: &str| parse_version(s, Hyphen::Prerelease).map(|v| v.to_string());
        for (banner, want) in [
            // Real banners, from the captures' provenance.
            ("2.1.278 (Claude Code)", Some("2.1.278")),
            ("codex-cli 0.155.1", Some("0.155.1")),
            ("0.135", Some("0.135.0")),
            ("omp/18.2.6", Some("18.2.6")),
            (
                "Hermes Agent v0.22.2 (2026.9.24) · stable · local d923a5eb487a",
                Some("0.22.2"),
            ),
            ("grok 1.0.25 (f7e67d6988e2) [stable]", Some("1.0.25")),
            ("0.2.0-rc.2", Some("0.2.0-rc.2")),
            // copilot's, which no capture records: a dot no digit follows ends it.
            ("GitHub Copilot CLI 1.0.62.", Some("1.0.62")),
            // The FIRST version-shaped token, not the last or the highest.
            ("tool 1.2.3 built with 4.5.6", Some("1.2.3")),
            // A parenthesized date or sha is never the version, nor a bare major,
            // nor a fourth component semver has no place for.
            ("(2026.8.31)", None),
            ("2026", None),
            ("2.1.233.4", None),
            ("no version here", None),
            // A major no u64 holds is skipped for the next version-shaped token.
            ("99999999999999999999999.0 v1.2.3", Some("1.2.3")),
            // Punctuation closing the suffix is not an identifier.
            ("1.0.0-rc.", Some("1.0.0-rc")),
        ] {
            assert_eq!(version(banner).as_deref(), want, "{banner:?}");
        }
    }

    #[test]
    fn a_build_hyphen_reads_as_build_metadata_whatever_the_id() {
        for (banner, build) in [
            ("2026.09.18-9a7762b", "9a7762b"),
            ("2026.09.18-1234567", "1234567"),
            ("2026.09.18-0123456", "0123456"),
        ] {
            let v = parse_version(banner, Hyphen::Build).expect(banner);
            assert_eq!(
                (v.to_string(), v.build.as_str()),
                (format!("2026.9.18+{build}"), build)
            );
            assert!(v.pre.is_empty(), "{banner:?}: a build id is no prerelease");
        }
    }

    #[test]
    fn daemon_sources_yields_every_daemon_row() {
        let daemons: Vec<&'static str> = daemon_sources()
            .map(|(name, ttl)| {
                assert_eq!(
                    ttl.presence_ttl_ms,
                    crate::source::daemon::PresenceTtl::DEFAULT.presence_ttl_ms,
                    "{name} must carry the default decay profile"
                );
                name
            })
            .collect();
        assert_eq!(daemons, vec![crate::source::openclaw::SOURCE_NAME]);
    }

    #[test]
    fn every_descriptor_has_a_verified_version() {
        for d in REGISTRY {
            assert!(
                !d.verified_version.is_empty(),
                "source {:?} verified_version is empty — use \"unknown\", not \"\"",
                d.name
            );
        }
    }

    #[test]
    fn all_label_prefixes_are_unique() {
        use std::collections::HashSet;
        let set: HashSet<&str> = REGISTRY.iter().map(|d| d.label_prefix).collect();
        assert_eq!(
            set.len(),
            REGISTRY.len(),
            "duplicate label_prefix across sources — two CLIs would share one sprite prefix"
        );
    }

    #[test]
    fn descriptor_names_match_module_source_name_consts() {
        assert_eq!(CLAUDE_CODE.name, claude_code::SOURCE_NAME);
        assert_eq!(CODEX.name, codex::SOURCE_NAME);
        assert_eq!(ANTIGRAVITY.name, antigravity::SOURCE_NAME);
        assert_eq!(REASONIX.name, reasonix::SOURCE_NAME);
        assert_eq!(CODEWHALE.name, codewhale::SOURCE_NAME);
        assert_eq!(OPENCODE.name, opencode::SOURCE_NAME);
        assert_eq!(COPILOT.name, copilot::SOURCE_NAME);
        assert_eq!(CURSOR.name, cursor::SOURCE_NAME);
        assert_eq!(HERMES.name, hermes::SOURCE_NAME);
        assert_eq!(OMP.name, omp::SOURCE_NAME);
        assert_eq!(OPENCLAW.name, openclaw::SOURCE_NAME);
        assert_eq!(GROK.name, grok::SOURCE_NAME);
        assert_eq!(KIMI.name, kimi::SOURCE_NAME);
        assert_eq!(DSH.name, dsh::SOURCE_NAME);
        assert_eq!(REGISTRY.len(), 14, "new row? add its name-pin assert above");
    }

    #[test]
    fn registered_source_names_are_unique() {
        let names: Vec<&str> = registered_source_names().collect();
        let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(
            names.len(),
            unique.len(),
            "duplicate source name in REGISTRY: {names:?}"
        );
    }

    #[test]
    fn descriptor_for_resolves_known_and_rejects_unknown() {
        assert_eq!(descriptor_for("codex").unwrap().label_prefix, "cx");
        assert!(descriptor_for("not-a-source").is_none());
    }

    #[test]
    fn id_deriver_for_returns_each_sources_own_transcript_key() {
        use std::path::Path;
        let cases: &[(&str, &str, &str)] = &[
            (
                claude_code::SOURCE_NAME,
                "/h/.claude/projects/-h-p/01000000-0000-7000-8000-0000000000cc.jsonl",
                "01000000-0000-7000-8000-0000000000cc",
            ),
            (
                codex::SOURCE_NAME,
                "/h/.codex/sessions/2026/05/29/rollout-2026-05-29T22-36-52-019e7762-9ded-7e33-be41-946ecf105bf4.jsonl",
                "019e7762-9ded-7e33-be41-946ecf105bf4",
            ),
            (
                copilot::SOURCE_NAME,
                "/h/.copilot/session-state/019e7762-9ded-7e33-be41-946ecf105bf4/events.jsonl",
                "019e7762-9ded-7e33-be41-946ecf105bf4",
            ),
            (
                grok::SOURCE_NAME,
                "/h/.grok/sessions/enc-cwd/019e7762-9ded-7e33-be41-946ecf105bf4/updates.jsonl",
                "019e7762-9ded-7e33-be41-946ecf105bf4",
            ),
        ];
        for (source, path, want) in cases {
            assert_eq!(
                id_deriver_for(source)(Path::new(path)),
                *want,
                "{source}: the registry must derive this transcript's own session key"
            );
        }

        let ag = "/h/.gemini/antigravity-cli/brain/x/.system_generated/logs/transcript.jsonl";
        assert_eq!(id_deriver_for(antigravity::SOURCE_NAME)(Path::new(ag)), "x");
        assert_eq!(
            id_deriver_for("not-a-source")(Path::new(ag)),
            crate::id::normalize_path_key(ag),
        );
    }

    #[cfg(any(feature = "native", feature = "harness"))]
    #[test]
    fn path_filter_for_rejects_each_sources_own_foreign_siblings() {
        use std::path::Path;
        let reject: &[(&str, &str)] = &[
            (
                claude_code::SOURCE_NAME,
                "/h/.claude/projects/-h-p/uuid/subagents/workflows/wf_1/journal.jsonl",
            ),
            (
                antigravity::SOURCE_NAME,
                "/h/.gemini/antigravity-cli/brain/c1/.system_generated/logs/transcript_full.jsonl",
            ),
            (
                grok::SOURCE_NAME,
                "/h/.grok/sessions/%2Fr/s1/chat_history.jsonl",
            ),
            (grok::SOURCE_NAME, "/h/.grok/sessions/%2Fr/s1/events.jsonl"),
        ];
        for (source, path) in reject {
            assert!(
                !path_filter_for(source)(Path::new(path)),
                "{source} must not walk {path}"
            );
        }

        let admit: &[(&str, &str)] = &[
            (
                claude_code::SOURCE_NAME,
                "/h/.claude/projects/-h-p/uuid/subagents/workflows/wf_1/agent-xyz.jsonl",
            ),
            (
                antigravity::SOURCE_NAME,
                "/h/.gemini/antigravity-cli/brain/c1/.system_generated/logs/transcript.jsonl",
            ),
            (grok::SOURCE_NAME, "/h/.grok/sessions/%2Fr/s1/updates.jsonl"),
            (
                codex::SOURCE_NAME,
                "/h/.codex/sessions/2026/05/29/rollout-x.jsonl",
            ),
            (omp::SOURCE_NAME, "/h/.omp/agent/sessions/e/2026_0199.jsonl"),
            (
                copilot::SOURCE_NAME,
                "/h/.copilot/session-state/019e/events.jsonl",
            ),
            ("not-a-source", "/anything.jsonl"),
        ];
        for (source, path) in admit {
            assert!(
                path_filter_for(source)(Path::new(path)),
                "{source} must walk {path}"
            );
        }
    }

    #[test]
    fn id_deriver_for_omp_keys_the_nested_stem_chain() {
        use std::path::Path;
        let child =
            Path::new("/h/.omp/agent/sessions/enc-cwd/2026-07-10T08-00-00-000Z_0199/Alpha.jsonl");
        assert_eq!(
            id_deriver_for(omp::SOURCE_NAME)(child),
            omp::omp_id_from_path(child),
        );
        assert!(
            id_deriver_for(omp::SOURCE_NAME)(child).contains("Alpha"),
            "the omp key must carry the subagent stem"
        );
    }

    #[test]
    fn cwd_extractor_for_dispatches_per_source_shapes() {
        use std::path::PathBuf;
        let top = serde_json::json!({ "cwd": "/repo/top" });
        let codex_shaped =
            serde_json::json!({ "type": "session_meta", "payload": { "cwd": "/repo/cx" } });
        let copilot_shaped = serde_json::json!({ "type": "session.start", "data": { "context": { "cwd": "/repo/cp" } } });

        let cc = cwd_extractor_for(claude_code::SOURCE_NAME);
        assert_eq!(cc(&top), Some(PathBuf::from("/repo/top")));
        assert_eq!(cc(&codex_shaped), None, "CC must ignore the codex shape");
        assert_eq!(
            cc(&copilot_shaped),
            None,
            "CC must ignore the copilot shape"
        );

        let cx = cwd_extractor_for(codex::SOURCE_NAME);
        assert_eq!(cx(&codex_shaped), Some(PathBuf::from("/repo/cx")));
        assert_eq!(cx(&top), None, "codex must ignore a top-level cwd");

        let cp = cwd_extractor_for(copilot::SOURCE_NAME);
        assert_eq!(cp(&copilot_shaped), Some(PathBuf::from("/repo/cp")));
        assert_eq!(
            cp(&codex_shaped),
            None,
            "copilot must ignore the codex shape"
        );

        let unknown = cwd_extractor_for("not-a-source");
        assert_eq!(
            unknown(&top),
            Some(PathBuf::from("/repo/top")),
            "unregistered sources keep the shared top-level default"
        );
    }

    #[test]
    fn short_idle_reap_fires_for_codex_grok_and_opencode_only() {
        for d in REGISTRY {
            assert_eq!(
                d.caps().short_idle_reap(),
                [codex::SOURCE_NAME, grok::SOURCE_NAME, opencode::SOURCE_NAME].contains(&d.name),
                "short_idle_reap mismatch for {:?}",
                d.name
            );
        }
    }

    #[test]
    fn daemon_accessors_are_inert() {
        let d = descriptor_for(openclaw::SOURCE_NAME).unwrap();
        assert!(d.is_daemon());
        assert!(d.line_decoder().is_none(), "a daemon has no JSONL watcher");
        assert!(
            d.hook().is_none(),
            "a daemon never reaches the shared agent arms"
        );
        assert!(
            !d.caps().short_idle_reap(),
            "INERT caps — a daemon reaps no AgentSlot"
        );
        assert!(
            d.presence_decoder().is_some(),
            "a daemon MUST carry a presence decoder"
        );
    }

    #[test]
    fn agent_has_a_decode_path_and_no_presence_decoder() {
        for d in REGISTRY.iter().filter(|d| !d.is_daemon()) {
            assert!(
                d.line_decoder().is_some() || d.hook().is_some(),
                "agent {:?} has no decode path (neither a transcript nor a hook)",
                d.name
            );
            assert!(
                d.presence_decoder().is_none(),
                "agent {:?} must have no presence decoder",
                d.name
            );
        }
    }

    #[test]
    fn every_daemon_source_has_a_presence_decoder_and_no_line_decoder() {
        for d in REGISTRY.iter().filter(|d| d.is_daemon()) {
            assert!(
                d.presence_decoder().is_some(),
                "daemon {:?} needs a presence decoder",
                d.name
            );
            assert!(
                d.line_decoder().is_none(),
                "daemon {:?} must not also be a transcript source",
                d.name
            );
        }
    }

    use proptest::prelude::*;

    /// A depth-capped arbitrary `serde_json::Value` (leaf | array | object) plus
    /// a "JSON re-encoded as a string" arm, so a string can itself carry nested
    /// JSON — the shape codex's `function_call.arguments` reparse consumes. The
    /// bounds stay shallow because the decoders do flat `.get("a").get("b")`
    /// chains, never deep recursion.
    fn arb_json() -> impl Strategy<Value = serde_json::Value> {
        use serde_json::Value;
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            any::<f64>()
                .prop_filter("finite", |x| x.is_finite())
                .prop_map(Value::from),
            ".*".prop_map(Value::from),
        ];
        leaf.prop_recursive(4, 64, 8, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
                proptest::collection::hash_map(".*", inner.clone(), 0..8)
                    .prop_map(|m| Value::Object(m.into_iter().collect())),
                inner.prop_map(|v| Value::String(v.to_string())),
            ]
        })
    }

    proptest! {
        // An `Err` is fine: the watcher logs-and-continues on a malformed line, so
        // the property is only that it does not PANIC. Iterated from REGISTRY, so a
        // new source is covered with no test edit.
        #[test]
        fn every_line_decoder_never_panics_on_arbitrary_json(v in arb_json()) {
            for d in REGISTRY {
                if let Some(decode) = d.line_decoder() {
                    let _ = decode("/fixture/session.jsonl", d.name, v.clone());
                }
                if let Some(extract) = d.cwd_extractor() {
                    let _ = extract(&v);
                }
            }
        }

        #[test]
        fn every_hook_and_presence_decoder_never_panics(v in arb_json()) {
            for d in REGISTRY {
                match (d.hook(), v.as_object()) {
                    (Some(HookDecoding::Shared { extend, .. }), Some(obj)) => {
                        let _ = extend(obj);
                    }
                    (Some(HookDecoding::ClaimsAll(f)), Some(obj)) => {
                        let _ = f(obj);
                    }
                    _ => {}
                }
                if let (Some(presence), Some(obj)) = (d.presence_decoder(), v.as_object()) {
                    let _ = presence(obj);
                }
            }
            let _ = crate::source::decoder::decode_hook_payload(v.clone());
        }
    }

    /// A declared var is only worth a column if it REACHES the root production
    /// watches — a source could read it in the installer and not the watcher.
    #[cfg(feature = "native")]
    #[test]
    fn every_declared_home_env_actually_moves_that_sources_root() {
        let declared: Vec<_> = REGISTRY
            .iter()
            .filter_map(|d| d.home_env.map(|v| (d.name, v)))
            .collect();
        assert!(
            declared.len() >= 6,
            "a floor, so an emptied column can't make this pass vacuously: {declared:?}"
        );

        // A named profile derives its own agent dir and IGNORES the override
        // (`dirs.ts`), so an exported OMP_PROFILE/PI_PROFILE reds this gate blaming
        // the resolver. Scrub them.
        let failures = temp_env::with_vars_unset(["OMP_PROFILE", "PI_PROFILE"], || {
            // Collect rather than assert in-loop, so one run names every source
            // that fails.
            let mut failures: Vec<String> = Vec::new();
            for (name, var) in declared {
                // A REAL dir: upstream `find_codex_home` gates `CODEX_HOME` on the path
                // existing, so a bare string silently falls back.
                let tmp = tempfile::tempdir().expect("tempdir");
                let root_env = tmp.path().to_path_buf();
                let resolved = temp_env::with_var(var, Some(&root_env), || {
                    crate::source::resolved_source_root(name)
                });

                match resolved {
                    None => failures.push(format!(
                        "{name} declares home_env={var} but `resolved_source_root` has no arm \
                     for it — add one, or the declaration is unproven"
                    )),
                    Some(r) if !r.starts_with(&root_env) => failures.push(format!(
                        "{name}: ${var}={} did not reach the resolved root {} — the override is \
                     declared but does not actually relocate anything",
                        root_env.display(),
                        r.display(),
                    )),
                    Some(_) => {}
                }
            }
            failures
        });

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Negative control: without an override, no root may land under the temp
    /// dir — else `starts_with` above could pass for an unrelated reason.
    #[cfg(feature = "native")]
    #[test]
    fn a_source_root_does_not_wander_into_an_unset_overrides_directory() {
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
            let tmp = tempfile::tempdir().expect("tempdir");
            for (name, root) in [
                (
                    "claude-code",
                    crate::source::resolved_source_root("claude-code").unwrap(),
                ),
                (
                    "copilot",
                    crate::source::resolved_source_root("copilot").unwrap(),
                ),
            ] {
                assert!(
                    !root.starts_with(tmp.path()),
                    "{name}: resolved {} under a directory nothing pointed at",
                    root.display()
                );
            }
        });
    }
}
