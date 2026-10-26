//! The tail of transcripts their writer holds open. macOS's FSEvents reports a
//! file's writes only at its close (xnu `bsd/vfs/vfs_vnops.c:887-893` at
//! `f6217f89`), so a source whose writer keeps its transcript open (one with an
//! open-fd liveness probe) gets a kqueue knote per followed transcript, each
//! write becoming a walk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The followed set's moves after a healthy probe. The probe is the only
/// per-file sign that a writer is gone: kqueue has no close note (xnu
/// `bsd/sys/event.h:242-252` at `f6217f89`).
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Plan {
    /// Followed, and no live writer holds it.
    pub(super) release: Vec<PathBuf>,
    /// Known, held by a live writer, and not yet followed.
    pub(super) follow: Vec<PathBuf>,
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn plan<'a>(
    followed: &[PathBuf],
    known: impl IntoIterator<Item = &'a PathBuf>,
    live: &HashSet<String>,
    id_for: impl Fn(&Path) -> String,
) -> Plan {
    let release = followed
        .iter()
        .filter(|path| !live.contains(&id_for(path)))
        .cloned()
        .collect();
    let follow = known
        .into_iter()
        .filter(|path| !followed.contains(path) && live.contains(&id_for(path)))
        .cloned()
        .collect();
    Plan { release, follow }
}

#[cfg(target_os = "macos")]
pub(crate) use imp::Tail;

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};

    use notify::EventHandler;
    use notify::event::{DataChange, EventKind, ModifyKind};
    use rustix::event::kqueue::VnodeEvents;
    use tracing::warn;

    use super::super::health::FailureLatch;
    use crate::source::kqueue::{VnodeNote, VnodeWatch, Vnodes};

    const FOLLOWED: VnodeEvents = VnodeEvents::WRITE
        .union(VnodeEvents::DELETE)
        .union(VnodeEvents::RENAME)
        .union(VnodeEvents::REVOKE);

    pub(crate) struct Tail {
        watch: VnodeWatch,
        follow_health: Mutex<FailureLatch>,
    }

    fn written(path: &Path) -> notify::Event {
        notify::Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
            .add_path(path.to_path_buf())
    }

    impl Tail {
        /// `handler` is the root watch's own, so a write takes the path an
        /// FSEvents event takes.
        pub(crate) fn spawn(mut handler: impl EventHandler) -> Option<Self> {
            let sink = move |vnodes: &Vnodes, note: VnodeNote<'_>| match note {
                VnodeNote::Changed(path, flags) => {
                    if flags
                        .intersects(VnodeEvents::DELETE | VnodeEvents::RENAME | VnodeEvents::REVOKE)
                    {
                        // The followed inode left the path; a file renamed over
                        // it is the one to follow now.
                        vnodes.remove(path);
                        let _ = vnodes.add(path, FOLLOWED);
                    }
                    handler.handle_event(Ok(written(path)));
                }
                VnodeNote::Failed(error) => {
                    warn!(error = %error, "transcript tail stopped; held-open appends wait for the poll");
                }
            };
            match VnodeWatch::spawn("pixtuoid-tail", sink) {
                Ok(watch) => Some(Self {
                    watch,
                    follow_health: Mutex::default(),
                }),
                Err(error) => {
                    warn!(error = %error, "transcript tail unavailable; held-open appends wait for the poll");
                    None
                }
            }
        }

        /// Whether this call started following `path`, so its writes before
        /// the knote are the caller's to read.
        pub(crate) fn follow(&self, path: &Path) -> bool {
            let mut health = self
                .follow_health
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match self.watch.vnodes().add(path, FOLLOWED) {
                Ok(started) => {
                    health.on_success();
                    started
                }
                // The walk that finds it gone retires it.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => {
                    if health.on_failure() {
                        warn!(path = ?path, error = %error, "cannot follow a held-open transcript; its appends wait for the poll");
                    }
                    false
                }
            }
        }

        /// Applies [`super::plan`]; returns the paths it started following,
        /// whose writes before the knote are the caller's to read.
        pub(crate) fn reconcile<'a>(
            &self,
            known: impl IntoIterator<Item = &'a PathBuf>,
            live: &HashSet<String>,
            id_for: impl Fn(&Path) -> String,
        ) -> Vec<PathBuf> {
            let vnodes = self.watch.vnodes();
            let plan = super::plan(&vnodes.paths(), known, live, id_for);
            for path in &plan.release {
                vnodes.remove(path);
            }
            plan.follow
                .into_iter()
                .filter(|path| self.follow(path))
                .collect()
        }

        #[cfg(test)]
        pub(crate) fn follows(&self, path: &Path) -> bool {
            self.watch.vnodes().paths().iter().any(|p| p == path)
        }
    }
}

/// No tail off macOS: inotify reports each write to a held-open file (Linux
/// `IN_MODIFY`), and on Windows the open-fd probe a release rides is a no-op.
#[cfg(not(target_os = "macos"))]
pub(crate) enum Tail {}

#[cfg(not(target_os = "macos"))]
impl Tail {
    pub(crate) fn spawn(_handler: impl notify::EventHandler) -> Option<Self> {
        None
    }

    pub(crate) fn follow(&self, _path: &Path) -> bool {
        match *self {}
    }

    pub(crate) fn reconcile<'a>(
        &self,
        _known: impl IntoIterator<Item = &'a PathBuf>,
        _live: &HashSet<String>,
        _id_for: impl Fn(&Path) -> String,
    ) -> Vec<PathBuf> {
        match *self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_for(path: &Path) -> String {
        path.file_stem().unwrap().to_string_lossy().into_owned()
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|n| PathBuf::from(format!("/r/{n}.jsonl")))
            .collect()
    }

    fn live(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    /// codex writes no session-end marker, so a session that lived and died
    /// between two probes is never vouched dead: only the probe frees it.
    #[test]
    fn a_followed_transcript_no_live_writer_holds_is_released() {
        let plan = plan(
            &paths(&["gone", "held"]),
            &paths(&["gone", "held"]),
            &live(&["held"]),
            id_for,
        );
        assert_eq!(plan.release, paths(&["gone"]));
        assert_eq!(plan.follow, Vec::<PathBuf>::new());
    }

    /// Whatever the claim says: a multi-turn codex child's claim ends at each
    /// turn while its parent still holds the rollout open.
    #[test]
    fn a_known_transcript_a_live_writer_holds_is_followed() {
        let plan = plan(&[], &paths(&["child"]), &live(&["child"]), id_for);
        assert_eq!(plan.follow, paths(&["child"]));
    }

    /// A boot knows every transcript in the initial window; only the held
    /// ones may cost an fd.
    #[test]
    fn only_what_a_live_writer_holds_is_followed() {
        let plan = plan(&[], &paths(&["old", "held"]), &live(&["held"]), id_for);
        assert_eq!(plan.follow, paths(&["held"]));
    }

    #[test]
    fn a_followed_live_transcript_is_left_alone() {
        let plan = plan(
            &paths(&["held"]),
            &paths(&["held"]),
            &live(&["held"]),
            id_for,
        );
        assert_eq!(plan, Plan::default());
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use std::fs::File;
        use std::io::Write;
        use std::sync::mpsc::{Receiver, channel};
        use std::time::Duration;

        use super::super::Tail;
        use super::*;

        const DEADLINE: Duration = Duration::from_secs(5);

        fn tail() -> (Tail, Receiver<PathBuf>) {
            let (tx, rx) = channel();
            let tail = Tail::spawn(move |res: notify::Result<notify::Event>| {
                for path in res.expect("a healthy tail reports no error").paths {
                    let _ = tx.send(path);
                }
            })
            .unwrap();
            (tail, rx)
        }

        fn held(path: &Path) -> File {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap()
        }

        #[test]
        fn follow_reports_each_held_open_append() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.jsonl");
            let mut file = held(&path);
            let (tail, reported) = tail();
            assert!(tail.follow(&path), "the first follow starts it");
            assert!(!tail.follow(&path), "a second follow is a no-op");
            file.write_all(b"{}\n").unwrap();
            assert_eq!(reported.recv_timeout(DEADLINE), Ok(path));
        }

        /// omp rewrites its transcript as temp + rename, and re-opens the new
        /// inode to keep appending.
        #[test]
        fn a_path_renamed_over_is_followed_again() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.jsonl");
            drop(held(&path));
            let (tail, reported) = tail();
            tail.follow(&path);
            let rewrite = dir.path().join("t.jsonl.tmp");
            std::fs::write(&rewrite, "{}\n").unwrap();
            std::fs::rename(&rewrite, &path).unwrap();
            assert_eq!(reported.recv_timeout(DEADLINE), Ok(path.clone()));
            held(&path).write_all(b"{}\n").unwrap();
            assert_eq!(
                reported.recv_timeout(DEADLINE),
                Ok(path),
                "an append to the new inode is reported"
            );
        }

        #[test]
        fn a_path_renamed_away_is_reported_and_dropped() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.jsonl");
            drop(held(&path));
            let (tail, reported) = tail();
            tail.follow(&path);
            std::fs::rename(&path, dir.path().join("t.jsonl.1")).unwrap();
            assert_eq!(reported.recv_timeout(DEADLINE), Ok(path.clone()));
            assert!(!tail.follows(&path));
        }

        #[test]
        fn a_deleted_path_is_reported_and_dropped() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.jsonl");
            drop(held(&path));
            let (tail, reported) = tail();
            tail.follow(&path);
            std::fs::remove_file(&path).unwrap();
            assert_eq!(reported.recv_timeout(DEADLINE), Ok(path.clone()));
            assert!(!tail.follows(&path));
        }

        #[test]
        fn reconcile_releases_the_unheld_and_follows_the_held() {
            let dir = tempfile::tempdir().unwrap();
            let gone = dir.path().join("gone.jsonl");
            let held_open = dir.path().join("held.jsonl");
            drop(held(&gone));
            drop(held(&held_open));
            let (tail, _reported) = tail();
            tail.follow(&gone);
            let known = [gone.clone(), held_open.clone()];
            let started = tail.reconcile(&known, &live(&["held"]), id_for);
            assert_eq!(started, vec![held_open.clone()]);
            assert!(!tail.follows(&gone));
            assert!(tail.follows(&held_open));
        }
    }
}
