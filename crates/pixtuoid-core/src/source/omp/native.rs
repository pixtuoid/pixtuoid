use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use super::{
    SOURCE_NAME, decode_omp_line, omp_derive_label, omp_head_title, omp_id_from_path,
    omp_profile_sessions_dirs, omp_sessions_dir,
};
use crate::source::decoder::parsed_tail_lines;
use crate::source::jsonl::{DEFAULT_POLL_INTERVAL, JsonlWatcher, ProbeSnapshot};
use crate::source::{Source, TaggedSender};

/// The profile-roots lister [`OmpSource::run`] re-invokes on its rescan tick,
/// injectable so tests can grow the set without touching process env.
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
    /// Re-lists the profile roots each rescan tick (a profile created mid-run
    /// gains a watcher without a restart).
    #[doc(hidden)]
    pub rescan: OmpProfileEnumerate,
    /// How often [`Self::rescan`] runs; production stays on the watcher poll
    /// authority, tests shrink it.
    #[doc(hidden)]
    pub rescan_interval: Duration,
    /// Roots here came from the resolver (never user-supplied), so every
    /// watcher gets the fd probe without the replay-excluding layout gate.
    /// Only [`Self::default_paths`] sets it.
    first_party_roots: bool,
}

impl std::fmt::Debug for OmpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OmpSource")
            .field("sessions_root", &self.sessions_root)
            .field("profile_sessions_roots", &self.profile_sessions_roots)
            .field("rescan_interval", &self.rescan_interval)
            .field("first_party_roots", &self.first_party_roots)
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
            first_party_roots: true,
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
            first_party_roots: false,
        }
    }
}

/// One watcher over `root`. A first-party root (resolver-enumerated, never
/// user-supplied) gets the fd probe unconditionally; anything else goes
/// through [`omp_probe_root`]'s replay-excluding gate. `started` keeps every
/// root ever spawned, so a failed one is not respawn-thrashed by the rescan.
fn spawn_watcher(
    set: &mut tokio::task::JoinSet<Result<()>>,
    started: &mut HashSet<PathBuf>,
    root: PathBuf,
    first_party: bool,
    tx: TaggedSender,
) {
    let mut watcher = JsonlWatcher::new(
        root.clone(),
        SOURCE_NAME.to_string(),
        decode_omp_line,
        omp_session_ended,
    )
    .with_label_deriver(omp_derive_label)
    .with_head_label(omp_head_title);
    let probe_root = if first_party {
        Some(root.clone())
    } else {
        omp_probe_root(&root)
    };
    if let Some(probe_root) = probe_root {
        watcher = watcher.with_liveness_probe(Arc::new(move || live_omp_session_ids(&probe_root)));
    }
    started.insert(root);
    set.spawn(watcher.run(tx));
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
            first_party_roots,
        } = *self;
        let mut set = tokio::task::JoinSet::new();
        let mut started = HashSet::new();
        let mut last_error = None;
        spawn_watcher(
            &mut set,
            &mut started,
            sessions_root,
            first_party_roots,
            tx.clone(),
        );
        for root in profile_sessions_roots {
            if !started.contains(&root) {
                spawn_watcher(&mut set, &mut started, root, first_party_roots, tx.clone());
            }
        }
        loop {
            tokio::select! {
                _ = tokio::time::sleep(rescan_interval) => {
                    // Off the worker: the enumeration does blocking fs reads
                    // (the liveness probes set the precedent).
                    let listed = tokio::task::spawn_blocking({
                        let rescan = rescan.clone();
                        move || rescan()
                    })
                    .await;
                    for root in listed.unwrap_or_default() {
                        if !started.contains(&root) {
                            spawn_watcher(&mut set, &mut started, root, first_party_roots, tx.clone());
                        }
                    }
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
fn live_omp_session_ids(sessions_root: &Path) -> Option<ProbeSnapshot> {
    ProbeSnapshot::from_open_fds(
        sessions_root,
        &["bun", "omp"],
        omp_recognize,
        omp_id_from_path,
    )
}

/// The FOCUS-jump entry point: the same snapshot the liveness probe takes,
/// behind the same first-party-layout gate, so a root pointed elsewhere can
/// never resolve a pid for a transcript merely replayed there. Re-probed at CLICK
/// time rather than cached, which is what makes the pid recycle-safe: a dead
/// omp holds no fd, so its id simply drops out of the snapshot.
pub(crate) fn live_omp_session_ids_for_focus(sessions_root: &Path) -> Option<ProbeSnapshot> {
    live_omp_session_ids(&omp_probe_root(sessions_root)?)
}

/// The per-source RECOGNIZER: a held-open `.jsonl` under the root vouches. The
/// id-space fold is applied by `from_open_fds`, not here.
fn omp_recognize(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
}

/// Attach the probe ONLY for omp's first-party layout — the standard
/// `~/.omp/agent/sessions` shape, or whatever `omp_sessions_dir()` resolves for
/// THIS environment. A test/replay root pointed at an arbitrary dir must keep
/// the pure-mtime first-sight gate, or a replayed transcript vouched for by a
/// coincidentally running bun process resurrects as live.
fn omp_probe_root(sessions_root: &Path) -> Option<PathBuf> {
    omp_probe_root_resolved(sessions_root, &omp_sessions_dir())
}

/// The injectable core of [`omp_probe_root`]. `resolved_sessions` is the
/// SESSIONS root, not the agent dir: XDG flattens `agent/` away, so an XDG root
/// has no agent-dir parent and would silently lose the probe.
fn omp_probe_root_resolved(sessions_root: &Path, resolved_sessions: &Path) -> Option<PathBuf> {
    if sessions_root.file_name().and_then(|n| n.to_str()) != Some("sessions") {
        return None;
    }
    let parent = sessions_root.parent();
    let parent_is_dot_omp_agent = parent.is_some_and(|p| {
        p.file_name().and_then(|n| n.to_str()) == Some(super::AGENT_SUBDIR)
            && p.parent()
                .and_then(|g| g.file_name())
                .and_then(|n| n.to_str())
                == Some(".omp")
    });
    // Covers every relocating axis at once: this gate and `default_paths` call
    // the SAME `omp_sessions_dir()`, so they cannot disagree.
    let is_resolved_root = sessions_root == resolved_sessions;
    if !parent_is_dot_omp_agent && !is_resolved_root {
        return None;
    }
    // Not canonicalized here: the dir may not exist yet at wiring time (omp
    // never run). `live_omp_session_ids` canonicalizes per probe call, which
    // also picks up a root created after startup.
    Some(sessions_root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_root_accepts_the_dot_omp_agent_layout_and_nothing_that_merely_resembles_it() {
        let home = tempfile::tempdir().unwrap();
        // A resolved agent dir that is NOT the standard layout, so each case
        // below is decided by the `.omp/agent` check alone.
        let elsewhere = home.path().join("pi-coding-agent");

        let standard = home.path().join(".omp").join("agent").join("sessions");
        std::fs::create_dir_all(&standard).unwrap();
        assert_eq!(
            omp_probe_root_resolved(&standard, &elsewhere.join("sessions")),
            Some(standard.clone()),
            "the first-party `.omp/agent/sessions` layout attaches the probe"
        );

        // Parent named `agent` but the GRANDPARENT is not `.omp` — the halves
        // of the layout check must both hold, or a `~/work/agent/sessions`
        // replay would be vouched for by any running bun process.
        let impostor = home.path().join("work").join("agent").join("sessions");
        std::fs::create_dir_all(&impostor).unwrap();
        assert_eq!(
            omp_probe_root_resolved(&impostor, &elsewhere.join("sessions")),
            None
        );

        let wrong_parent = home.path().join(".omp").join("other").join("sessions");
        std::fs::create_dir_all(&wrong_parent).unwrap();
        assert_eq!(
            omp_probe_root_resolved(&wrong_parent, &elsewhere.join("sessions")),
            None
        );

        // The PI_CODING_AGENT_DIR case still attaches on the resolved dir.
        let resolved = elsewhere.join("sessions");
        std::fs::create_dir_all(&resolved).unwrap();
        assert_eq!(
            omp_probe_root_resolved(&resolved, &elsewhere.join("sessions")),
            Some(resolved)
        );
        assert_eq!(
            omp_probe_root_resolved(Path::new("/tmp/fixture"), &elsewhere.join("sessions")),
            None
        );
    }

    /// The XDG shape has no `agent/` parent to recognise, so it used to lose the
    /// probe and fall back to pure mtime.
    #[test]
    fn probe_root_attaches_for_the_xdg_flattened_layout() {
        let xdg = Path::new("/xdg/omp/sessions");
        assert_eq!(
            omp_probe_root_resolved(xdg, xdg),
            Some(xdg.to_path_buf()),
            "the flattened `$XDG_DATA_HOME/omp/sessions` root is first-party too"
        );
        // Still not a blanket accept: an arbitrary `.../sessions` that is NOT
        // what this environment resolves stays on the mtime gate.
        assert_eq!(
            omp_probe_root_resolved(Path::new("/srv/x/sessions"), xdg),
            None
        );
    }

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
    fn probe_root_requires_first_party_layout() {
        let resolved = Path::new("/home/u/.omp/agent/sessions");
        assert_eq!(
            omp_probe_root_resolved(Path::new("/home/u/.omp/agent/sessions"), resolved),
            Some(PathBuf::from("/home/u/.omp/agent/sessions"))
        );
        assert_eq!(
            omp_probe_root_resolved(Path::new("/tmp/fixture"), resolved),
            None
        );
        assert_eq!(
            omp_probe_root_resolved(Path::new("/srv/other/sessions"), resolved),
            None
        );
        assert_eq!(
            omp_probe_root_resolved(Path::new("sessions"), resolved),
            None
        );
    }

    #[test]
    fn probe_root_accepts_resolved_agent_dir_sessions_layout() {
        let agent_dir = tempfile::tempdir().unwrap();
        let sessions = agent_dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        assert_eq!(
            omp_probe_root_resolved(&sessions, &sessions),
            Some(sessions.clone())
        );
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
