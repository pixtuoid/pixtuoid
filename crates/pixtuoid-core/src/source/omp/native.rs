use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use notify::RecursiveMode;

use super::{
    SOURCE_NAME, decode_omp_line, omp_derive_label, omp_head_title, omp_id_from_path,
    omp_profile_sessions_dirs, omp_profiles_dir, omp_sessions_dir,
};
use crate::source::decoder::parsed_tail_lines;
use crate::source::jsonl::{
    DEFAULT_POLL_INTERVAL, JsonlWatcher, Nudge, ProbeSnapshot, RootWatch, Wake, absolute_root,
};
use crate::source::{Source, TaggedSender};

/// The profile-roots lister [`OmpSource::run`] re-invokes on each new profile
/// and backstop tick, injectable so tests can grow the set without touching
/// process env.
pub(crate) type OmpProfileEnumerate = Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync>;

/// omp appends a [`super::CUSTOM`] entry whose `customType` is
/// [`super::SESSION_EXIT`] on every clean teardown, so a transcript that already
/// ended carries that marker — the first-sight gate uses it to avoid
/// resurrecting a finished session. Structural parse only: tool
/// arguments/results are persisted verbatim in the same file, so a substring
/// scan would let CONTENT (a grep for the marker) end a live session — the CC
/// sharp edge.
fn omp_session_ended(tail: &[u8]) -> bool {
    parsed_tail_lines(tail).any(|v| {
        v.get("type").and_then(|t| t.as_str()) == Some(super::CUSTOM)
            && v.get("customType").and_then(|c| c.as_str()) == Some(super::SESSION_EXIT)
    })
}

/// Source that watches the omp sessions directory recursively — the primary
/// (env-selected) root plus every other profile's, since each profile keeps
/// its own sessions tree and a session under any of them is a sprite.
pub struct OmpSource {
    /// The watched primary omp `sessions` root.
    pub(crate) sessions_root: PathBuf,
    /// The OTHER profiles' sessions roots, one watcher each. Filled by
    /// [`Self::default_paths`]' enumeration.
    pub profile_sessions_roots: Vec<PathBuf>,
    /// Re-lists the profile roots (a profile created mid-run gains a watcher
    /// without a restart).
    #[doc(hidden)]
    pub rescan: OmpProfileEnumerate,
    /// The period of [`Self::rescan`]'s backstop; production stays on the
    /// watcher poll authority.
    #[doc(hidden)]
    pub rescan_interval: Duration,
    /// The dir whose new entry, a new profile, runs [`Self::rescan`] at once.
    #[doc(hidden)]
    pub profiles_dir: Option<PathBuf>,
}

impl std::fmt::Debug for OmpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OmpSource")
            .field("sessions_root", &self.sessions_root)
            .field("profile_sessions_roots", &self.profile_sessions_roots)
            .field("rescan_interval", &self.rescan_interval)
            .field("profiles_dir", &self.profiles_dir)
            .finish_non_exhaustive()
    }
}

impl OmpSource {
    /// Construct pointed at the default omp `sessions` root, plus every other
    /// profile's.
    pub fn default_paths() -> Self {
        Self {
            profile_sessions_roots: omp_profile_sessions_dirs(),
            rescan: Arc::new(omp_profile_sessions_dirs),
            rescan_interval: DEFAULT_POLL_INTERVAL,
            sessions_root: omp_sessions_dir(),
            profiles_dir: omp_profiles_dir(),
        }
    }

    /// A source over one root with no profile set — the test/replay shape.
    #[doc(hidden)]
    pub fn single_root(sessions_root: PathBuf) -> Self {
        Self {
            sessions_root,
            profile_sessions_roots: Vec::new(),
            rescan: Arc::new(Vec::new),
            rescan_interval: DEFAULT_POLL_INTERVAL,
            profiles_dir: None,
        }
    }
}

/// One watcher over `root`, unless one was ever started: `started` keeps every
/// root ever spawned, so a failed one is not respawn-thrashed by the rescan.
fn spawn_watcher(
    set: &mut tokio::task::JoinSet<Result<()>>,
    started: &mut HashSet<PathBuf>,
    root: PathBuf,
    tx: TaggedSender,
) {
    if !started.insert(root.clone()) {
        return;
    }
    let watcher = JsonlWatcher::new(
        root,
        SOURCE_NAME.to_string(),
        decode_omp_line,
        omp_session_ended,
    )
    .with_label_deriver(omp_derive_label)
    .with_head_label(omp_head_title)
    .with_root_probe(live_omp_session_ids);
    set.spawn(watcher.run(tx));
}

async fn rescan_into(
    rescan: &OmpProfileEnumerate,
    set: &mut tokio::task::JoinSet<Result<()>>,
    started: &mut HashSet<PathBuf>,
    tx: &TaggedSender,
) {
    // Off the worker: the enumeration does blocking fs reads
    // (the liveness probes set the precedent).
    let listed = tokio::task::spawn_blocking({
        let rescan = rescan.clone();
        move || rescan()
    })
    .await;
    for root in listed.unwrap_or_default() {
        spawn_watcher(set, started, root, tx.clone());
    }
}

/// The watch on `dir` that nudges for each new entry, a new profile.
async fn profiles_watch(dir: PathBuf, nudge: Nudge) -> RootWatch {
    let events = {
        let nudge = nudge.clone();
        move |res: notify::Result<notify::Event>| nudge.on_event(&res)
    };
    let mut watch = RootWatch::new(
        absolute_root(dir),
        RecursiveMode::NonRecursive,
        events,
        nudge,
    );
    watch.reconcile_logged(Wake::Poll).await;
    watch
}

impl Source for OmpSource {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        let Self {
            sessions_root,
            profile_sessions_roots,
            rescan,
            rescan_interval,
            profiles_dir,
        } = *self;
        let mut set = tokio::task::JoinSet::new();
        let mut started = HashSet::new();
        let mut last_error = None;
        for root in std::iter::once(sessions_root).chain(profile_sessions_roots) {
            spawn_watcher(&mut set, &mut started, root, tx.clone());
        }
        let (nudge, mut nudges) = Nudge::channel();
        let mut profiles = match profiles_dir {
            Some(dir) => Some(profiles_watch(dir, nudge).await),
            None => None,
        };
        // An interval, not a per-iteration sleep, which every nudge would reset.
        let mut backstop = tokio::time::interval(rescan_interval);
        backstop.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Its first tick completes immediately; the roots above are the boot listing.
        backstop.tick().await;
        loop {
            tokio::select! {
                Some(()) = nudges.recv() => {
                    if let Some(watch) = profiles.as_mut() {
                        watch.reconcile_logged(Wake::Nudge).await;
                        // An unwatched profiles dir, absent or failing, reports no profile.
                        if watch.is_attached() {
                            rescan_into(&rescan, &mut set, &mut started, &tx).await;
                        }
                    }
                }
                _ = backstop.tick() => {
                    if let Some(watch) = profiles.as_mut() {
                        watch.reconcile_logged(Wake::Poll).await;
                    }
                    rescan_into(&rescan, &mut set, &mut started, &tx).await;
                }
                Some(finished) = set.join_next() => {
                    // Log-and-continue per root, but REMEMBER the failure: a
                    // source watching nothing must reach the deaths surface
                    // the user sees (#157), not only the log.
                    match finished {
                        Ok(Err(e)) => {
                            tracing::warn!(source = SOURCE_NAME, error = %e, "omp watcher exited");
                            last_error = Some(e);
                        }
                        Err(join) => {
                            tracing::warn!(source = SOURCE_NAME, error = %join, "omp watcher task died");
                            last_error = Some(join.into());
                        }
                        Ok(Ok(())) => {}
                    }
                    if set.is_empty() {
                        return match last_error {
                            Some(e) => Err(e),
                            None => Ok(()),
                        };
                    }
                }
            }
        }
    }
}

/// omp's liveness probe: the session ids (in the `omp_id_from_path` id-space,
/// so they join the watcher's first-sight gate directly) of every transcript
/// under `sessions_root` held OPEN by a running omp process, plus the owning
/// pid per id.
///
/// omp keeps a for-lifetime append fd on its session file, so an open
/// transcript fd IS the first-party liveness signal. BOTH process names are
/// probed: a packaged omp (Homebrew ships a Bun-compiled single binary, which
/// reports `omp`) and a `bun run` checkout, whose kernel-truncated name is
/// `bun`. The fd MODE is not checked, so a long-lived bun tool reading an OLD
/// transcript vouches for it: an accepted intermittent resurrection, reaped
/// again once the fd closes.
pub(crate) fn live_omp_session_ids(sessions_root: &Path) -> Option<ProbeSnapshot> {
    ProbeSnapshot::from_open_fds(
        sessions_root,
        &["bun", "omp"],
        omp_recognize,
        omp_id_from_path,
    )
}

/// The per-source RECOGNIZER: a held-open `.jsonl` under the root vouches. The
/// id-space fold is applied by `from_open_fds`, not here.
fn omp_recognize(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ended_marker_is_anchored_on_the_structural_fields() {
        assert!(omp_session_ended(
            br#"{"type":"custom","id":"a","parentId":null,"timestamp":"t","customType":"session_exit","data":{"reason":"exit command","kind":"normal","recordedAt":"t"}}"#
        ));
        assert!(!omp_session_ended(
            br#"{"type":"custom","customType":"tool_execution_start","data":{"toolCallId":"t1"}}"#
        ));
        // Marker bytes inside tool CONTENT must not end the session.
        assert!(!omp_session_ended(
            br#"{"type":"message","message":{"role":"toolResult","toolCallId":"t1","content":[{"type":"text","text":"grep hit: \"customType\":\"session_exit\""}]}}"#
        ));
        assert!(!omp_session_ended(br#"{"type":"session","cwd":"/p"}"#));
    }

    #[test]
    fn session_ended_matches_marker_after_a_partial_first_tail_line() {
        assert!(omp_session_ended(
            b"...tail-fragment\"}\n{\"type\":\"custom\",\"customType\":\"session_exit\",\"data\":{}}\n"
        ));
    }

    #[test]
    fn default_paths_points_at_the_agent_sessions_dir() {
        let src = OmpSource::default_paths();
        assert!(
            src.sessions_root.ends_with("sessions"),
            "got {:?}",
            src.sessions_root
        );
    }

    const STEM: &str = "2026-07-10T18-32-27-539Z_019f4d4d-6c93-7000-af7b-59b47b0e8111";

    fn snap_of(root: &Path, paths: Vec<PathBuf>) -> ProbeSnapshot {
        ProbeSnapshot::from_open_fd_pairs(
            root,
            paths.into_iter().map(|p| (42, p)),
            omp_recognize,
            omp_id_from_path,
        )
    }

    #[test]
    fn transcript_under_root_yields_its_chain_id_bound_to_its_pid() {
        let root = Path::new("/home/u/.omp/agent/sessions");
        let session = root.join(format!("-dev-proj/{STEM}.jsonl"));
        let child = root.join(format!("-dev-proj/{STEM}/Alpha.jsonl"));
        let got = snap_of(root, vec![session, child]);
        let mut ids: Vec<_> = got.ids().cloned().collect();
        ids.sort();
        // Expectations go through the SAME fold the probe applies — a raw-case
        // literal here fails ONLY on windows-test.
        let stem_key = crate::id::normalize_path_key(STEM);
        assert_eq!(
            ids,
            vec![
                stem_key.clone(),
                crate::id::normalize_path_key(&format!("{STEM}/Alpha"))
            ]
        );
        assert_eq!(got.pid_of.get(&stem_key), Some(&42));
    }

    #[test]
    fn paths_outside_root_and_non_jsonl_files_are_excluded() {
        let root = Path::new("/home/u/.omp/agent/sessions");
        let outside = PathBuf::from(format!("/tmp/elsewhere/{STEM}.jsonl"));
        let wrong_ext = root.join(format!("-dev-proj/{STEM}/notes.txt"));
        let no_ext = root.join("-dev-proj/README");
        let got = snap_of(root, vec![outside, wrong_ext, no_ext]);
        assert!(got.is_empty());
        assert!(got.pid_of.is_empty());
    }

    #[test]
    fn live_ids_for_missing_root_is_some_empty_not_a_failure() {
        // canonicalize() fails on a nonexistent dir, but an ABSENT root is not
        // a probe failure — `None` would freeze the negative-vouch ledger
        // forever on machines that never ran omp.
        let missing = Path::new("/definitely/not/a/real/.omp/agent/sessions");
        let snap = live_omp_session_ids(missing).expect("absent root is not a probe failure");
        assert!(snap.is_empty());
        assert!(snap.pid_of.is_empty());
    }

    #[test]
    fn live_ids_for_unrelated_root_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let snap =
            live_omp_session_ids(dir.path()).expect("a healthy system's enumeration must succeed");
        assert!(snap.is_empty());
    }
}
