use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Result;
use notify::{Config, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::source::TaggedSender;
use crate::source::exit_watch::ExitWatch;

mod health;
mod liveness;
#[cfg(test)]
mod tests;
mod unclaim;
mod walk;

#[cfg(test)]
pub(crate) use crate::source::decoder::is_subagent_path;
pub use liveness::LivenessProbe;
pub use liveness::ProbeSnapshot;
pub use unclaim::ChildEndUnclaims;

pub(crate) use health::FailureLatch;
use liveness::{
    NEGATIVE_VOUCH_MIN_SPAN, ProbeLadder, emit_proof_of_life, emit_session_exit,
    refresh_probe_snapshot,
};
use unclaim::drain_child_end_unclaims;
use walk::{scan_root, walk_jsonl};

pub use crate::source::decoder::LineDecoder;

pub use crate::source::decoder::PathFilter;
pub use crate::source::decoder::{IdDeriver, TailActivity};

/// Derives an agent's display label from its transcript `(path, source, cwd)`.
/// The default is the source-prefixed cwd basename (`cx·dotfiles`); **CC**
/// overrides it with `cc_derive_label` and **omp** with `omp_derive_label`.
pub type LabelDeriver = fn(&Path, &str, &Path) -> String;

fn default_prefixed_label(_path: &Path, source: &str, cwd: &Path) -> String {
    crate::source::decoder::derive_prefixed_label(source, cwd)
}

/// Predicate on a transcript's raw bytes: `true` when they carry a session-end
/// marker, so the first-sight gate never seeds an already-ended transcript.
pub type SessionEndChecker = fn(&[u8]) -> bool;

/// Reads a transcript tail into a [`TailActivity`] verdict. Only **CC**
/// supplies one (see `claude_code::cc_activity_recency` for the write that made
/// mtime lie); every other source keeps the mtime proxy until such a write is
/// OBSERVED on its wire — supplying one is a per-source wire fact, not a policy.
pub(crate) type ActivityRecency = fn(&[u8]) -> TailActivity;

fn no_activity_recency(_tail: &[u8]) -> TailActivity {
    TailActivity::Unknown
}

/// Derives a first-sight cwd from the transcript PATH when the content
/// head-scan yields none (default: never). **grok** overrides it: its
/// transcript lines carry NO cwd anywhere — the cwd lives in the URL-encoded
/// group-dir name — so without this every grok registration would start
/// empty-cwd and ride the reducer's unknown-cwd reap.
pub(crate) type CwdDeriver = fn(&Path) -> Option<PathBuf>;

fn no_cwd_from_path(_p: &Path) -> Option<PathBuf> {
    None
}

/// Pulls a display label out of ONE decoded head line, for a source that names
/// its session in the transcript HEAD rather than only in the tail stream.
/// `None` on a source's decoder row means it has no such name — which is
/// load-bearing, not merely absent: it lets the head scan stop at the first
/// `cwd` instead of reading on for a label that can never arrive. **omp** is
/// the one override; see its `omp_head_title`.
pub(crate) type HeadLabel = fn(&serde_json::Value) -> Option<String>;

#[derive(Clone, Copy)]
struct SourceDecoders {
    decode_line: LineDecoder,
    derive_label: LabelDeriver,
    check_ended: SessionEndChecker,
    activity_recency: ActivityRecency,
    id_derive: folded::FoldedDeriver,
    path_filter: PathFilter,
    cwd_derive: CwdDeriver,
    head_label: Option<HeadLabel>,
}

/// The `IdDeriver` behind its only legal caller.
///
/// Every id in the watcher must be derived from a path folded through
/// [`walk::id_path`], or the consumer computes it in a different id-space than
/// the producer. The fn pointer lives in a module the sibling
/// `walk`/`liveness`/`unclaim` modules are not inside, so an un-folded
/// derivation is a compile error rather than a review catch.
mod folded {
    use std::path::Path;

    #[derive(Debug, Clone, Copy)]
    pub(super) struct FoldedDeriver(super::IdDeriver);

    impl FoldedDeriver {
        pub(super) fn new(f: super::IdDeriver) -> Self {
            Self(f)
        }

        /// The id for `path`, folded. THE only way to reach the deriver.
        pub(super) fn id_for(&self, path: &Path) -> String {
            (self.0)(&super::walk::id_path(path))
        }
    }
}

/// What the watcher's first sight of a transcript still holds. Absent = never
/// registered, or fully retired by an exit un-claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sight {
    /// Registered: appends decode without re-registering.
    Claimed,
    /// Its session ended: a DECODED terminator, an oversized span whose tail
    /// its [`SessionEndChecker`] matches, or a child-end un-claim: the path
    /// stays KNOWN, so [`revouch_gated_files`](liveness::revouch_gated_files)
    /// won't replay it however live the probe says the (still-open) rollout is,
    /// and only an append that shows activity re-registers it.
    Ended,
}

#[derive(Clone, Copy)]
struct WatchCtx<'a> {
    source: &'a Arc<str>,
    cursors: &'a Arc<Mutex<HashMap<PathBuf, u64>>>,
    /// First-sight claims, by path.
    seen: &'a Arc<Mutex<HashMap<PathBuf, Sight>>>,
    tx: &'a TaggedSender,
    /// Recency window for the first-sight gate (an older file is seeded at EOF
    /// without a SessionStart). One window for the whole watch, so every path
    /// that can first-see a file gates identically (#85).
    window: Duration,
    /// Most recent liveness-probe snapshot (session ids in `IdDeriver` space),
    /// refreshed once per scan pass; notify-driven single-file walks reuse it —
    /// staleness is fine because the probe is ADDITIVE-ONLY (it can only admit,
    /// never gate). `emit_session_exit` is a second writer: it purges a
    /// confirmed-dead id so a probe-failure pass can't re-admit it.
    live: &'a Arc<Mutex<HashSet<String>>>,
}

struct ScanState {
    /// The probe ladder: the negative-vouch hysteresis (a probe-missing id must
    /// stay missing for `NEGATIVE_VOUCH_MIN_SPAN` before its exit fires, so a
    /// probe blip can't end a live session) plus the `pid → ids` bindings the
    /// instant-exit arm joins on.
    ladder: ProbeLadder,
    root_health: FailureLatch,
}

impl ScanState {
    fn new(negative_vouch_min_span: Duration) -> Self {
        Self {
            ladder: ProbeLadder::new(negative_vouch_min_span),
            root_health: FailureLatch::default(),
        }
    }
}

/// Tails a source's transcript directory, decoding each `.jsonl` append into
/// `AgentEvent`s. Built with [`JsonlWatcher::new`] plus the `with_*` builders;
/// [`JsonlWatcher::run`] drives the watch loop.
pub struct JsonlWatcher {
    root: PathBuf,
    initial_window: Duration,
    source_name: String,
    decode_line: LineDecoder,
    derive_label: LabelDeriver,
    check_session_ended: SessionEndChecker,
    activity_recency: ActivityRecency,
    id_derive: folded::FoldedDeriver,
    path_filter: PathFilter,
    cwd_derive: CwdDeriver,
    head_label: Option<HeadLabel>,
    liveness_probe: Option<LivenessProbe>,
    poll_interval: Duration,
    negative_vouch_min_span: Duration,
    child_end_unclaims: Option<ChildEndUnclaims>,
}

impl std::fmt::Debug for JsonlWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlWatcher")
            .field("root", &self.root)
            .field("source_name", &self.source_name)
            .field("initial_window", &self.initial_window)
            .field("poll_interval", &self.poll_interval)
            .field("liveness_probe", &self.liveness_probe.is_some())
            .finish_non_exhaustive()
    }
}

const DEFAULT_INITIAL_WINDOW: Duration = Duration::from_secs(3600);
/// The watcher's poll backstop — also the cadence profile rescans ride, so
/// "how often we look at the filesystem" has one authority.
pub(crate) const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);
/// How long after the initial seed the watcher rescans.
pub(super) const RESCAN_DELAY: Duration = Duration::from_millis(250);

/// Test-only seam: forces every `JsonlWatcher` in this process onto a polling
/// backend (`notify::PollWatcher`) at `interval`, instead of the native
/// FSEvents/inotify watcher. Set once — later calls are ignored. Integration
/// tests use it because a real FSEvents stream costs tens of seconds of
/// setup/teardown per `TempDir` on macOS.
#[doc(hidden)]
pub fn force_polling_backend_for_tests(interval: Duration) {
    let _ = TEST_POLL_OVERRIDE.set(interval);
}

static TEST_POLL_OVERRIDE: OnceLock<Duration> = OnceLock::new();

impl JsonlWatcher {
    /// A watcher over `root` for `source`, decoding each transcript line with
    /// `decode_line` and gating ended sessions with `check_session_ended`. The
    /// id derivation comes from `source`'s own registry row
    /// ([`crate::source::registry::id_deriver_for`]), so there is no per-source
    /// `run()` wiring to forget and the offline `harness::Drive` reads the same
    /// row.
    pub fn new(
        root: PathBuf,
        source: String,
        decode_line: LineDecoder,
        check_session_ended: SessionEndChecker,
    ) -> Self {
        Self {
            id_derive: folded::FoldedDeriver::new(crate::source::registry::id_deriver_for(&source)),
            path_filter: crate::source::registry::path_filter_for(&source),
            root,
            initial_window: DEFAULT_INITIAL_WINDOW,
            source_name: source,
            decode_line,
            derive_label: default_prefixed_label,
            check_session_ended,
            activity_recency: no_activity_recency,
            cwd_derive: no_cwd_from_path,
            head_label: None,
            liveness_probe: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            negative_vouch_min_span: NEGATIVE_VOUCH_MIN_SPAN,
            child_end_unclaims: None,
        }
    }

    /// Override the recency window a first-sight transcript must fall within to
    /// seed at EOF (default `DEFAULT_INITIAL_WINDOW`).
    pub fn with_initial_window(mut self, window: Duration) -> Self {
        self.initial_window = window;
        self
    }

    /// Supply this source's [`ActivityRecency`] — what the first-sight gate
    /// measures the initial window against, in place of the file mtime.
    pub(crate) fn with_activity_recency(mut self, recency: ActivityRecency) -> Self {
        self.activity_recency = recency;
        self
    }

    /// Test-only seam: shrinks the `scan_root` poll backstop so the poll arm is
    /// testable without waiting a [`DEFAULT_POLL_INTERVAL`] per tick.
    /// Production never calls this.
    #[doc(hidden)]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Test-only seam: shrinks the [`NEGATIVE_VOUCH_MIN_SPAN`] confirmation
    /// window so the negative-vouch exit path is testable. Production never
    /// calls this.
    #[doc(hidden)]
    pub fn with_negative_vouch_min_span(mut self, span: Duration) -> Self {
        self.negative_vouch_min_span = span;
        self
    }

    /// Override the display-label derivation (default: the source-prefixed cwd
    /// basename via [`LabelDeriver`]). Needed by the sources whose transcript
    /// PATH names the agent better than its cwd does — **CC** (subagents) and
    /// **omp** (subagents, named by the task they were dispatched under).
    pub fn with_label_deriver(mut self, derive_label: LabelDeriver) -> Self {
        self.derive_label = derive_label;
        self
    }

    /// Derive a first-sight cwd from the transcript PATH when the content
    /// head-scan yields none (default: never). See [`CwdDeriver`].
    pub(crate) fn with_cwd_deriver(mut self, cwd_derive: CwdDeriver) -> Self {
        self.cwd_derive = cwd_derive;
        self
    }

    /// Name the session from ONE decoded head line, ahead of the cwd-basename
    /// deriver (default: never). See [`HeadLabel`].
    pub(crate) fn with_head_label(mut self, head_label: HeadLabel) -> Self {
        self.head_label = Some(head_label);
        self
    }

    /// Override the [`PathFilter`] the source's registry row supplies; no
    /// in-tree source needs it.
    pub fn with_path_filter(mut self, path_filter: PathFilter) -> Self {
        self.path_filter = path_filter;
        self
    }

    /// Attach a liveness probe (default: none) so the watcher gates first-sight
    /// seeding on a live-session check and drives ongoing liveness rather than
    /// transcript content.
    pub fn with_liveness_probe(mut self, probe: LivenessProbe) -> Self {
        self.liveness_probe = Some(probe);
        self
    }

    /// Attach the child-end un-claim side-channel (see [`ChildEndUnclaims`]).
    /// The watcher becomes the CONSUMER: on each pass it drains the handle's
    /// ids that match its own claimed transcripts and ends those claims
    /// ([`Sight::Ended`]).
    #[doc(hidden)]
    pub fn with_child_end_unclaims(mut self, unclaims: ChildEndUnclaims) -> Self {
        self.child_end_unclaims = Some(unclaims);
        self
    }

    /// The initial seed, the `RESCAN_DELAY` rescan and the `DEFAULT_POLL_INTERVAL` poll all run this SAME
    /// sequence; only the seed skips the un-claim drain (`drain = false` —
    /// nothing has been pushed at startup).
    async fn run_scan_pass(
        &self,
        ctx: &WatchCtx<'_>,
        scan_state: &mut ScanState,
        exit_watch: Option<&ExitWatch>,
        unclaims: Option<&ChildEndUnclaims>,
        decoders: SourceDecoders,
        drain: bool,
    ) {
        let healthy = refresh_probe_snapshot(
            self.liveness_probe.as_ref(),
            &mut scan_state.ladder,
            exit_watch,
            decoders,
            ctx,
        )
        .await;
        if drain {
            drain_child_end_unclaims(unclaims, decoders, ctx).await;
        }
        scan_root(&self.root, decoders, ctx, &mut scan_state.root_health).await;
        if healthy {
            emit_proof_of_life(ctx.live, ctx.source, ctx.tx).await;
        }
    }

    /// Consume the watcher and drive the watch loop — initial seed, a `RESCAN_DELAY`
    /// rescan, the `DEFAULT_POLL_INTERVAL` poll backstop, and notify events — feeding each decoded
    /// event to `tx`.
    ///
    /// # Errors
    ///
    /// If the file-watch backend cannot be created, or the watch root cannot be stat'ed or watched.
    pub async fn run(self, tx: TaggedSender) -> Result<()> {
        let cursors: Arc<Mutex<HashMap<PathBuf, u64>>> = Arc::new(Mutex::new(HashMap::new()));
        let seen_sessions: Arc<Mutex<HashMap<PathBuf, Sight>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let live: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let mut scan_state = ScanState::new(self.negative_vouch_min_span);

        // Instant exit: a probed watcher spawns ONE detached ExitWatch thread so a
        // bound OS process dying becomes a SessionEnd in milliseconds, ahead of the
        // negative vouch and the TTL/stale sweeps. Purely additive — spawn() is None
        // on unsupported platforms, and a dead thread just stops sending.
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel::<i32>();
        let exit_watch = if self.liveness_probe.is_some() {
            ExitWatch::spawn(exit_tx.clone())
        } else {
            None
        };
        // TRAP: the only long-lived sender is owned by the ExitWatch thread. With
        // no probe wired or a failed spawn, every sender would drop right here and
        // `exit_rx.recv()` would resolve `Ready(None)` on every select! pass — a
        // wasted poll on every loop iteration, forever. Park one clone so the arm
        // stays forever-pending in exactly those cases.
        let _exit_keepalive = exit_watch.is_none().then(|| exit_tx.clone());
        drop(exit_tx);

        // Bound on buffered notify PATHS (#585) so a reducer stall can't grow it
        // without limit; drop-on-Full is safe because the poll re-walks.
        const NOTIFY_PATH_CHANNEL_CAP: usize = 1024;
        let (notify_tx, mut notify_rx) =
            tokio::sync::mpsc::channel::<PathBuf>(NOTIFY_PATH_CHANNEL_CAP);
        let mut notify_health = FailureLatch::default();
        let mut notify_backpressure = FailureLatch::default();
        let event_handler = move |res: notify::Result<notify::Event>| match res {
            Ok(event) => {
                if notify_health.on_success() {
                    tracing::info!("file-watch backend is delivering events again");
                }
                for path in event.paths {
                    if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                        match notify_tx.try_send(path) {
                            Ok(()) => {
                                if notify_backpressure.on_success() {
                                    tracing::info!("notify path channel draining again");
                                }
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                if notify_backpressure.on_failure() {
                                    tracing::warn!(
                                        "notify path channel saturated under load — dropping paths; the 60s poll re-walks them"
                                    );
                                }
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
                        }
                    }
                }
            }
            Err(e) => {
                if notify_health.on_failure() {
                    warn!(
                        error = %e,
                        "file-watch backend error; events may have been lost — the poll \
                         backstop covers until it recovers"
                    );
                }
            }
        };
        let mut watcher: Box<dyn Watcher + Send> = match TEST_POLL_OVERRIDE.get().copied() {
            // `with_compare_contents` detects changes by hashing file contents, not
            // just mtime/size, so truncate-rewrites are caught reliably.
            Some(interval) => Box::new(PollWatcher::new(
                event_handler,
                Config::default()
                    .with_poll_interval(interval)
                    .with_compare_contents(true),
            )?),
            None => Box::new(RecommendedWatcher::new(event_handler, Config::default())?),
        };
        // The root is another CLI's, and auto-detect keys on its presence, so it
        // is never created: until it appears, the poll's scan (an absent root is
        // a healthy observation) stands in for the watch. One that can't even be
        // stat'ed is a watch failure (#157).
        let mut watching = tokio::fs::try_exists(&self.root).await?;
        if watching {
            watcher.watch(&self.root, RecursiveMode::Recursive)?;
        }

        let source_arc: Arc<str> = Arc::from(self.source_name.as_str());
        let unclaims = self.child_end_unclaims.clone();
        let decoders = SourceDecoders {
            decode_line: self.decode_line,
            derive_label: self.derive_label,
            check_ended: self.check_session_ended,
            activity_recency: self.activity_recency,
            id_derive: self.id_derive,
            path_filter: self.path_filter,
            cwd_derive: self.cwd_derive,
            head_label: self.head_label,
        };

        // The initial seed rides the same `scan_root` → `walk_jsonl` path every
        // later scan uses, so a file is gated identically (recency + session_end)
        // no matter which pass first sees it (#85).
        {
            let ctx = WatchCtx {
                source: &source_arc,
                cursors: &cursors,
                seen: &seen_sessions,
                tx: &tx,
                window: self.initial_window,
                live: &live,
            };
            self.run_scan_pass(
                &ctx,
                &mut scan_state,
                exit_watch.as_ref(),
                unclaims.as_ref(),
                decoders,
                false,
            )
            .await;
        }

        // Re-scan shortly after startup to catch files APFS read_dir missed during
        // the initial seed walk (metadata propagation race). walk_jsonl is
        // idempotent (cursor == file_len → no-op).
        let mut rescan_done = false;
        let rescan_delay = tokio::time::sleep(RESCAN_DELAY);
        tokio::pin!(rescan_delay);

        // An INTERVAL hoisted outside the loop, not a per-iteration sleep: a sleep
        // re-created per iteration resets its deadline on every notify event, so
        // sustained notify traffic starves scan_root indefinitely. Delay (not the
        // Burst default) so a long stall doesn't fire catch-up scans back-to-back.
        let mut poll = tokio::time::interval(self.poll_interval);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // An interval's first tick completes immediately; the seed already scanned.
        poll.tick().await;

        loop {
            let source_arc = source_arc.clone();
            let ctx = WatchCtx {
                source: &source_arc,
                cursors: &cursors,
                seen: &seen_sessions,
                tx: &tx,
                window: self.initial_window,
                live: &live,
            };
            tokio::select! {
                Some(path) = notify_rx.recv() => {
                    // Drain BEFORE the walk (not only on scan passes), so the
                    // un-claim lands on the first notify after the hook Stop instead
                    // of waiting out the 60s poll while turn-N+1 bytes stream past
                    // as unknown-id no-ops.
                    drain_child_end_unclaims(unclaims.as_ref(), decoders, &ctx).await;
                    walk_jsonl(&path, decoders, &ctx).await;
                }
                _ = &mut rescan_delay, if !rescan_done => {
                    rescan_done = true;
                    self.run_scan_pass(
                        &ctx, &mut scan_state,
                        exit_watch.as_ref(), unclaims.as_ref(), decoders, true,
                    ).await;
                }
                _ = poll.tick() => {
                    if !watching && tokio::fs::try_exists(&self.root).await.unwrap_or(false) {
                        watcher.watch(&self.root, RecursiveMode::Recursive)?;
                        watching = true;
                    }
                    self.run_scan_pass(
                        &ctx, &mut scan_state,
                        exit_watch.as_ref(), unclaims.as_ref(), decoders, true,
                    ).await;
                }
                Some(pid) = exit_rx.recv() => {
                    // `pid_died` translates through the pid→ids binding AND disarms
                    // the negative vouch for each id, so the slower rung can't
                    // re-confirm the exit we're about to emit.
                    for id in scan_state.ladder.pid_died(pid) {
                        debug!(pid, session_id = ?id, "instant exit: pid died; emitting SessionEnd");
                        emit_session_exit(&id, decoders, &ctx).await;
                    }
                }
            }
        }
    }
}
