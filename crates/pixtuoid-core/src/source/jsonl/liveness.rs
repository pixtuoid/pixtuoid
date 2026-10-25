use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::debug;

use crate::AgentId;
use crate::source::exit_watch::ExitWatch;
use crate::source::{AgentEvent, TaggedSender, Transport, fd_probe};

use super::walk::{check_session_ended, park_if_truncated_below_cursor, walk_jsonl};
use super::{SourceDecoders, WatchCtx};

/// One healthy liveness-probe observation: which agent processes are verified
/// alive RIGHT NOW, and which OS pid owns each. The live-id set IS `pid_of`'s
/// key set; a separate `ids` set would only make "id without pid" representable.
#[derive(Debug, Clone, Default)]
pub struct ProbeSnapshot {
    /// id → owning OS pid, for the exit watch. Many ids may share one pid — one
    /// codex process holds every rollout it has open.
    pub pid_of: HashMap<String, i32>,
}

impl ProbeSnapshot {
    /// The ids this probe saw alive.
    #[cfg(test)]
    pub(crate) fn ids(&self) -> impl Iterator<Item = &String> {
        self.pid_of.keys()
    }
    /// Was `id` alive at probe time?
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.pid_of.contains_key(id)
    }
    /// Did the probe see nothing alive? Distinct from a FAILED probe, which is `None`.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.pid_of.is_empty()
    }

    /// Bind `id` to its owning `pid`. A contested id (two live processes holding
    /// one file open — a resume overlap) resolves to the LARGER pid: arbitrary
    /// but stable, never proc-enumeration order (#252).
    pub(crate) fn bind_pid(&mut self, id: String, pid: i32) {
        let bound = self.pid_of.entry(id).or_insert(pid);
        if pid > *bound {
            *bound = pid;
        }
    }

    /// Build a snapshot from the regular files that live `comm_names`-named
    /// processes hold open under `root` — the skeleton the open-FD liveness
    /// sources share, under `LivenessProbe`'s explicit-failure contract: only a
    /// failed proc-table enumeration is `None`, the state that changes nothing
    /// (#223); an un-canonicalizable `root` (the source never ran) is `Some(empty)`.
    pub(crate) fn from_open_fds(
        root: &Path,
        comm_names: &[&str],
        recognize: impl Fn(&Path) -> bool,
        id_derive: super::IdDeriver,
    ) -> Option<Self> {
        Self::from_open_fds_with(
            root,
            comm_names,
            recognize,
            id_derive,
            fd_probe::pids_by_name,
            fd_probe::open_vnode_paths,
        )
    }

    /// [`from_open_fds`](Self::from_open_fds) with the two proc-table calls
    /// INJECTED, so the shell's own decisions — canonicalize-or-`Some(empty)`,
    /// enumeration-failure-is-`None` (#223), and the pid→path fan-out — are
    /// reachable from a test without any FFI.
    pub(crate) fn from_open_fds_with(
        root: &Path,
        comm_names: &[&str],
        recognize: impl Fn(&Path) -> bool,
        id_derive: super::IdDeriver,
        pids_by_name: impl Fn(&str) -> Option<Vec<i32>>,
        open_vnode_paths: impl Fn(i32) -> Vec<PathBuf>,
    ) -> Option<Self> {
        // Kernel-reported fd paths are fully resolved (/tmp → /private/tmp on
        // macOS), so an under-root compare against a raw root misses everything.
        let Ok(canonical) = root.canonicalize() else {
            debug!(
                root = ?root,
                "fd probe: root not canonicalizable; nothing alive there"
            );
            return Some(Self::default());
        };
        let mut pids = Vec::new();
        for name in comm_names {
            pids.extend(pids_by_name(name)?);
        }
        let pairs = pids.into_iter().flat_map(|pid| {
            open_vnode_paths(pid)
                .into_iter()
                .map(move |path| (pid, path))
        });
        Some(Self::from_open_fd_pairs(
            &canonical, pairs, recognize, id_derive,
        ))
    }

    /// The PURE join half of [`Self::from_open_fds`], drivable with synthetic
    /// `(pid, path)` pairs.
    pub(crate) fn from_open_fd_pairs(
        root: &Path,
        pairs: impl Iterator<Item = (i32, PathBuf)>,
        recognize: impl Fn(&Path) -> bool,
        id_derive: super::IdDeriver,
    ) -> Self {
        let mut snap = Self::default();
        for (pid, path) in pairs {
            if !path.starts_with(root) {
                continue;
            }
            if recognize(&path) {
                // Folded at the seam (`walk::id_path`), NOT in each source's
                // deriver — one id-space no new fd-probe source can forget.
                let id = (id_derive)(&super::walk::id_path(&path));
                debug!(
                    pid,
                    path = ?path,
                    session_id = ?id,
                    "fd probe: pid holds a transcript open"
                );
                snap.bind_pid(id, pid);
            }
        }
        snap
    }
}

/// Optional first-party liveness probe: the session ids — in the source's
/// `IdDeriver` id-space — of agent processes known to be ALIVE right now.
/// ADDITIVE-ONLY for admission: membership bypasses the first-sight
/// recency/ended gate. Failure is EXPLICIT: `None` means the probe itself FAILED
/// and callers must change NOTHING; `Some` with an empty snapshot means it ran
/// fine and nothing is alive — the distinction `ProbeLadder`'s negative vouch
/// (#223) rests on. `Arc<dyn Fn>` rather than a fn pointer because the real
/// probe captures its registry dir.
pub type LivenessProbe = Arc<dyn Fn() -> Option<ProbeSnapshot> + Send + Sync>;

/// Negative vouch (#223): a previously-vouched id must be MISSING from two
/// healthy probe snapshots at least this far apart before its exit is
/// confirmed, immune to Codex's brief drop-and-reopen fd gap on a write failure.
pub(super) const NEGATIVE_VOUCH_MIN_SPAN: Duration = Duration::from_secs(60);

/// Whether the liveness probe vouches for this transcript. A vouched-for file is
/// a RUNNING agent however old its mtime, so the first-sight gate must not hide
/// it. Subagent transcripts can never match — their stems are agent ids, not
/// session UUIDs.
pub(super) async fn probe_admits(
    path: &Path,
    decoders: SourceDecoders,
    ctx: &WatchCtx<'_>,
) -> bool {
    let live = ctx.live.lock().await;
    !live.is_empty() && live.contains(&decoders.id_derive.id_for(path))
}

/// The probe is ONGOING liveness, not just admission: emit a `ProofOfLife` per
/// vouched id after each refresh so the reducer can hold its sweep exemption
/// while the process lives; when the live signal disappears the emissions stop
/// and the exemption ages out.
pub(super) async fn emit_proof_of_life(
    live: &Arc<Mutex<HashSet<String>>>,
    source: &Arc<str>,
    tx: &TaggedSender,
) {
    // Snapshot before sending: holding the lock across `tx.send` would block
    // probe refreshes on a slow consumer for no reason.
    let ids: Vec<AgentId> = live
        .lock()
        .await
        .iter()
        .map(|sid| AgentId::from_parts(source, sid))
        .collect();
    for agent_id in ids {
        let _ = tx
            .send((Transport::Jsonl, AgentEvent::ProofOfLife { agent_id }))
            .await;
    }
}

/// #223: the probe ladder's DERIVED liveness state. A session id the probe
/// previously vouched for that DISAPPEARS from a healthy snapshot is a
/// high-confidence exit — the registry entry was removed / the rollout fd
/// closed, signals only the OWNING process can produce — so the watcher can emit
/// the `SessionEnd` the CLI never writes instead of waiting out the reducer's
/// stale sweep (`stale_threshold`). Confirmation needs the id missing from two healthy observations
/// at least `min_span` apart; a probe FAILURE is never an observation.
///
/// A pure failure detector: [`fold`](ProbeLadder::fold) RETURNS the effects to
/// apply rather than emitting them, so it is unit-testable with synthetic time
/// and zero mocks.
pub(super) struct ProbeLadder {
    min_span: Duration,
    /// Ids vouched by an earlier healthy snapshot. An id stays "previously
    /// vouched" while its miss window runs, so the second observation can
    /// confirm it.
    prev_vouched: HashSet<String>,
    /// id → when a healthy snapshot FIRST came back without it. `Instant`
    /// (monotonic): a wall-clock jump must not fake a `min_span`.
    miss_since: HashMap<String, std::time::Instant>,
    /// pid → the session ids a healthy snapshot bound to it, ADDITIVE per
    /// snapshot: an id leaves via `pid_died` or a confirmed exit, never by
    /// snapshot omission — the vouch ladder owns "gone" semantics. Bound under
    /// at most ONE pid (`fold`'s migration maintains this).
    pid_bindings: HashMap<i32, HashSet<String>>,
}

/// What one [`ProbeLadder::fold`] decided the imperative shell must DO.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ProbeOutcome {
    pub exits: Vec<String>,
    pub newly_watched: Vec<i32>,
}

impl ProbeLadder {
    pub(super) fn new(min_span: Duration) -> Self {
        Self {
            min_span,
            prev_vouched: HashSet::new(),
            miss_since: HashMap::new(),
            pid_bindings: HashMap::new(),
        }
    }

    /// Fold one HEALTHY snapshot: advance the negative-vouch miss windows and
    /// the pid bindings, returning the effects for the shell to emit + watch.
    /// Never called on a probe FAILURE — the shell forwards only `Some`
    /// snapshots.
    pub(super) fn fold(&mut self, snap: &ProbeSnapshot, now: std::time::Instant) -> ProbeOutcome {
        self.miss_since.retain(|id, _| !snap.contains(id));
        let missing: Vec<String> = self
            .prev_vouched
            .iter()
            .filter(|id| !snap.contains(id))
            .cloned()
            .collect();
        let mut exits = Vec::new();
        for id in missing {
            match self.miss_since.get(&id) {
                Some(first_miss) if now.duration_since(*first_miss) >= self.min_span => {
                    debug!(
                        session_id = ?id,
                        "negative vouch confirmed: probe stopped vouching; emitting SessionEnd"
                    );
                    self.forget(&id);
                    self.unbind(&id);
                    exits.push(id);
                }
                Some(_) => {}
                None => {
                    self.miss_since.insert(id, now);
                }
            }
        }
        // Ids whose miss window still runs must STAY vouched, or no later
        // snapshot can confirm them.
        self.prev_vouched = snap.pid_of.keys().cloned().collect();
        self.prev_vouched.extend(self.miss_since.keys().cloned());

        let mut newly_watched = Vec::new();
        for (id, pid) in &snap.pid_of {
            // find-then-compare (not `any(p != pid && contains)`): the first
            // holder is the only holder, so there is no conjunction to mutate.
            let bound_elsewhere = self
                .pid_bindings
                .iter()
                .find(|(_, ids)| ids.contains(id))
                .is_some_and(|(p, _)| p != pid);
            if bound_elsewhere {
                // A rebind (a codex `resume` of one rollout in a second process)
                // MIGRATES the id — the old pid's death would else instant-exit it.
                self.unbind(id);
            }
            let newly_seen = !self.pid_bindings.contains_key(pid);
            self.pid_bindings
                .entry(*pid)
                .or_default()
                .insert(id.clone());
            if newly_seen {
                newly_watched.push(*pid);
            }
        }
        ProbeOutcome {
            exits,
            newly_watched,
        }
    }

    /// The instant-exit rung: the watched OS process `pid` died. Remove its
    /// whole binding and `forget` every id it held, so the slower negative-vouch
    /// rung can't re-confirm an exit this rung is about to emit.
    pub(super) fn pid_died(&mut self, pid: i32) -> Vec<String> {
        let ids: Vec<String> = self
            .pid_bindings
            .remove(&pid)
            .into_iter()
            .flatten()
            .collect();
        for id in &ids {
            self.forget(id);
        }
        ids
    }

    /// Disarm the negative-vouch ledger for `id` WITHOUT confirming anything —
    /// a later healthy snapshot must not open/age a miss window toward
    /// re-confirming an exit a faster rung already emitted. Always paired with
    /// `unbind`, its bindings inverse.
    fn forget(&mut self, id: &str) {
        self.prev_vouched.remove(id);
        self.miss_since.remove(id);
    }

    /// Remove one session id from every pid's binding set, dropping pids whose
    /// set empties. The bindings inverse of `forget` — a confirmed exit needs
    /// both, because a codex-style process owns many rollouts and may outlive
    /// this session, so its eventual OS exit must not re-emit a SessionEnd for
    /// an already-confirmed id.
    fn unbind(&mut self, id: &str) {
        self.pid_bindings.retain(|_, ids| {
            ids.remove(id);
            !ids.is_empty()
        });
    }
}

/// ONE exit path for every watcher-synthesized session end, shared by the
/// negative-vouch and instant-exit arms so the two can't fork. Drain FIRST,
/// then un-claim (which lets a LATER append re-register via `emit_first_sight`):
/// an instant exit can beat a pre-death write's notify, and un-claiming with
/// bytes pending re-enters that chunk as a first-sight — a ghost, every rung disarmed.
pub(super) async fn emit_session_exit(id: &str, decoders: SourceDecoders, ctx: &WatchCtx<'_>) {
    let claimed: Vec<PathBuf> = {
        let seen = ctx.seen.lock().await;
        seen.keys()
            .filter(|p| decoders.id_derive.id_for(p) == id)
            .cloned()
            .collect()
    };
    for path in &claimed {
        // A truncated-below-cursor file must be parked at its new EOF, not
        // handed to the walk's truncation arm (cursor→0, whole-file replay).
        park_if_truncated_below_cursor(path, ctx).await;
        walk_jsonl(path, decoders, ctx).await;
    }
    let agent_id = AgentId::from_parts(ctx.source, id);
    let _ = ctx
        .tx
        .send((
            Transport::Jsonl,
            AgentEvent::SessionEnd {
                agent_id,
                as_child: false,
            },
        ))
        .await;
    {
        let mut seen = ctx.seen.lock().await;
        for path in &claimed {
            seen.remove(path);
        }
    }
    // Purge `live` too: only a HEALTHY refresh rewrites it, so a probe FAILURE
    // would keep vouching this id and replay it into a phantom SessionStart.
    ctx.live.lock().await.remove(id);
}

/// The imperative SHELL over [`ProbeLadder::fold`]: returns whether a healthy snapshot
/// landed, so the caller re-emits `ProofOfLife` only then. On a probe FAILURE (`None`) or no probe wired:
/// change NOTHING — `ctx.live` keeps the previous ids, the miss windows neither
/// advance nor confirm, no bindings move (the reducer's TTL absorbs the gap).
pub(super) async fn refresh_probe_snapshot(
    probe: Option<&LivenessProbe>,
    ladder: &mut ProbeLadder,
    exit_watch: Option<&ExitWatch>,
    decoders: SourceDecoders,
    ctx: &WatchCtx<'_>,
) -> bool {
    let Some(probe) = probe else {
        return false;
    };
    // `spawn_blocking`, not `block_in_place`: the probe is blocking std::fs and
    // libproc, and `block_in_place` panics on a current-thread runtime.
    let probe = Arc::clone(probe);
    let snap = match tokio::task::spawn_blocking(move || probe()).await {
        Ok(Some(snap)) => snap,
        Ok(None) => {
            debug!(
                "liveness probe failed; keeping the previous snapshot (failure changes nothing)"
            );
            return false;
        }
        Err(join_err) => {
            tracing::warn!(error = %join_err, "liveness probe task panicked; keeping the previous snapshot");
            return false;
        }
    };
    *ctx.live.lock().await = snap.pid_of.keys().cloned().collect();
    let outcome = ladder.fold(&snap, std::time::Instant::now());
    for id in &outcome.exits {
        emit_session_exit(id, decoders, ctx).await;
    }
    for pid in outcome.newly_watched {
        if let Some(watch) = exit_watch {
            // A pid whose kernel registration fails (EPERM) is not retried — the
            // slower rungs cover.
            watch.watch(pid);
        }
    }
    true
}

/// Re-ask the probe, on each SCAN pass, about every known-but-never-registered
/// file, resetting a vouched one's cursor to 0: the probe is consulted only in
/// `walk_jsonl`'s !known first-sight branch, so a TRANSIENT miss (registry file
/// mid-rewrite, a read race) would gate a live session PERMANENTLY — every later
/// pass exits at `cursor == file_len` and never asks again.
pub(super) async fn revouch_gated_files(decoders: SourceDecoders, ctx: &WatchCtx<'_>) {
    // Empty = no probe wired, or nothing live: a probe-less source then pays one
    // lock check per pass, not a metadata read per gated file.
    if ctx.live.lock().await.is_empty() {
        return;
    }
    let candidates: Vec<(PathBuf, u64)> = {
        let cursors = ctx.cursors.lock().await;
        cursors.iter().map(|(p, c)| (p.clone(), *c)).collect()
    };
    for (path, cursor) in candidates {
        // ANY entry skips: registered ones leave the candidate set (so this cannot
        // loop), and replaying a RELEASED, still-vouched child bursts stale activity.
        if ctx.seen.lock().await.contains_key(&path) {
            continue;
        }
        // Only a file parked exactly at EOF is stuck; a pending append revives it.
        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    // Prune, or a lost notify Remove leaves a forever-candidate.
                    ctx.cursors.lock().await.remove(&path);
                }
                continue;
            }
        };
        if meta.len() != cursor {
            continue;
        }
        if !probe_admits(&path, decoders, ctx).await {
            continue;
        }
        // Last, so the bounded tail read costs only the vouched candidates. An
        // ENDED one is refused here — resetting undoes the terminator gate.
        if check_session_ended(&path, decoders.check_ended).await {
            continue;
        }
        ctx.cursors.lock().await.insert(path, 0);
    }
}
