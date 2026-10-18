//! The `native`-only runtime half of the Codex source: the liveness probe
//! (open-rollout FD binding) + `CodexSource` and its `JsonlWatcher` wiring.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{SOURCE_NAME, codex_home, codex_id_from_path, decode_codex_line};
use crate::source::jsonl::{ChildEndUnclaims, JsonlWatcher, ProbeSnapshot};
use crate::source::{Source, TaggedSender};

/// Codex writes no session-end marker — defer to the mtime window + stale-sweep.
fn codex_session_ended(_tail: &[u8]) -> bool {
    false
}

/// Codex's liveness probe: the rollout UUIDs (in `codex_id_from_path` id-space,
/// so they join the watcher's first-sight gate directly) of every rollout under
/// `sessions_root` held OPEN by a running `codex` process, plus the owning pid
/// per id.
///
/// Codex has no session registry (unlike CC's `sessions/<pid>.json`), but a live
/// `codex` process holds its rollout file open in append mode for the whole
/// session, so an open rollout fd IS the first-party liveness signal: pid → open
/// fd → rollout path → UUID.
pub(crate) fn live_codex_rollout_ids(sessions_root: &Path) -> Option<ProbeSnapshot> {
    // `codex_id_from_path` is the SAME fn the registry row installs, so probe
    // ids and first-sight-gate ids cannot drift.
    ProbeSnapshot::from_open_fds(
        sessions_root,
        &["codex"],
        is_rollout_filename,
        codex_id_from_path,
    )
}

fn is_rollout_filename(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("rollout-"))
}

/// Attach the probe ONLY for codex's first-party layout: the standard
/// `~/.codex/sessions` shape, or the resolved `codex_home()/sessions` for THIS
/// environment (a `CODEX_HOME` user's real rollout root — rejecting it would
/// silently drop the whole liveness ladder for a supported config). A root
/// pointed elsewhere (a test's temp dir) must keep the pure-mtime first-sight
/// gate, or a rollout there vouched for by a coincidentally-running codex would
/// resurrect as live.
fn codex_probe_root(sessions_root: &Path) -> Option<PathBuf> {
    codex_probe_root_resolved(sessions_root, &codex_home())
}

/// The injectable core of [`codex_probe_root`]; `home` is the resolved codex
/// home for this environment.
fn codex_probe_root_resolved(sessions_root: &Path, home: &Path) -> Option<PathBuf> {
    if sessions_root.file_name().and_then(|n| n.to_str()) != Some("sessions") {
        return None;
    }
    let parent = sessions_root.parent();
    let parent_is_codex =
        parent.and_then(|p| p.file_name()).and_then(|n| n.to_str()) == Some(".codex");
    // A parent that IS the resolved codex home is first-party even when not
    // named `.codex` — the CODEX_HOME case.
    let parent_is_resolved_home = parent.is_some_and(|p| p == home);
    if !parent_is_codex && !parent_is_resolved_home {
        return None;
    }
    // Not canonicalized here: the dir may not exist yet at wiring time.
    // `live_codex_rollout_ids` canonicalizes per probe call, which also picks up
    // a root created after startup.
    Some(sessions_root.to_path_buf())
}

/// Source that watches the Codex session transcript directory.
#[derive(Debug)]
pub struct CodexSource {
    /// The watched Codex `sessions` rollout root (`~/.codex/sessions`).
    pub sessions_root: PathBuf,
    /// The child-end un-claim side-channel (#246) — Codex is consumer-only:
    /// this watcher releases an ended child's rollout claim so a multi-turn
    /// child's turn-N+1 append re-registers. `None` disables it.
    pub child_end_unclaims: Option<ChildEndUnclaims>,
}

impl CodexSource {
    /// Construct pointed at the default Codex `sessions` rollout root.
    pub fn default_paths() -> Self {
        Self {
            sessions_root: codex_home().join("sessions"),
            child_end_unclaims: None,
        }
    }
}

impl Source for CodexSource {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        let mut watcher = JsonlWatcher::new(
            self.sessions_root.clone(),
            SOURCE_NAME.to_string(),
            decode_codex_line,
            codex_session_ended,
        );
        if let Some(root) = codex_probe_root(&self.sessions_root) {
            watcher = watcher
                .with_liveness_probe(std::sync::Arc::new(move || live_codex_rollout_ids(&root)));
        }
        if let Some(unclaims) = &self.child_end_unclaims {
            watcher = watcher.with_child_end_unclaims(unclaims.clone());
        }
        watcher.run(tx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_session_ended_is_always_false() {
        assert!(!codex_session_ended(b"anything"));
        assert!(!codex_session_ended(b""));
    }

    const UUID: &str = "019e7762-9ded-7e33-be41-946ecf105bf4";

    fn snap_of(root: &Path, paths: Vec<PathBuf>) -> ProbeSnapshot {
        ProbeSnapshot::from_open_fd_pairs(
            root,
            paths.into_iter().map(|p| (42, p)),
            is_rollout_filename,
            codex_id_from_path,
        )
    }

    #[test]
    fn rollout_under_root_yields_its_uuid_bound_to_its_pid() {
        let root = Path::new("/home/u/.codex/sessions");
        // Real layout nests YYYY/MM/DD below the root — the subtree must be
        // admitted, not only direct children.
        let nested = root.join(format!(
            "2026/06/10/rollout-2026-06-10T08-00-00-{UUID}.jsonl"
        ));
        let got = snap_of(root, vec![nested]);
        assert_eq!(
            got.ids().cloned().collect::<Vec<_>>(),
            vec![UUID.to_string()]
        );
        assert_eq!(got.pid_of.get(UUID), Some(&42));
    }

    #[test]
    fn shared_rollout_binds_the_larger_pid_regardless_of_enumeration_order() {
        // Two live processes holding ONE rollout: a resume overlap.
        let root = Path::new("/home/u/.codex/sessions");
        let path = root.join(format!(
            "2026/06/10/rollout-2026-06-10T08-00-00-{UUID}.jsonl"
        ));
        for pids in [[100, 200], [200, 100]] {
            let got = ProbeSnapshot::from_open_fd_pairs(
                root,
                pids.into_iter().map(|p| (p, path.clone())),
                is_rollout_filename,
                codex_id_from_path,
            );
            assert_eq!(
                got.ids().cloned().collect::<Vec<_>>(),
                vec![UUID.to_string()]
            );
            assert_eq!(
                got.pid_of.get(UUID),
                Some(&200),
                "the larger pid must win in both enumeration orders"
            );
        }
    }

    #[test]
    fn rollout_outside_root_is_excluded() {
        let root = Path::new("/home/u/.codex/sessions");
        let outside = PathBuf::from(format!("/tmp/elsewhere/rollout-1-{UUID}.jsonl"));
        let got = snap_of(root, vec![outside]);
        assert!(got.is_empty());
        assert!(got.pid_of.is_empty());
    }

    #[test]
    fn non_rollout_files_under_root_are_excluded() {
        let root = Path::new("/home/u/.codex/sessions");
        let wrong_stem = root.join("2026/06/10/history.jsonl");
        let wrong_ext = root.join(format!("2026/06/10/rollout-1-{UUID}.log"));
        let no_ext = root.join("2026/06/10/rollout-noext");
        assert!(snap_of(root, vec![wrong_stem, wrong_ext, no_ext]).is_empty());
    }

    #[test]
    fn probe_root_requires_dot_codex_sessions_layout() {
        assert_eq!(
            codex_probe_root(Path::new("/home/u/.codex/sessions")),
            Some(PathBuf::from("/home/u/.codex/sessions"))
        );
        assert_eq!(codex_probe_root(Path::new("/tmp/fixture")), None);
        assert_eq!(codex_probe_root(Path::new("sessions")), None);
    }

    #[test]
    fn probe_root_accepts_resolved_codex_home_sessions_layout() {
        // A CODEX_HOME-shaped layout: the resolved home is NOT named `.codex`.
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        assert_eq!(
            codex_probe_root_resolved(&sessions, home.path()),
            Some(sessions.clone())
        );
        assert_eq!(
            codex_probe_root_resolved(Path::new("/tmp/fixture"), home.path()),
            None
        );
        assert_eq!(
            codex_probe_root_resolved(Path::new("/srv/other/sessions"), home.path()),
            None
        );
    }

    #[test]
    fn live_ids_for_missing_root_is_some_empty_not_a_failure() {
        // Some(empty), not None: None would freeze the negative-vouch ledger
        // forever on machines where codex has never run (#223).
        let missing = Path::new("/definitely/not/a/real/.codex/sessions");
        let snap = live_codex_rollout_ids(missing).expect("absent root is not a probe failure");
        assert!(snap.is_empty());
        assert!(snap.pid_of.is_empty());
    }

    #[test]
    fn live_ids_for_unrelated_root_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let snap = live_codex_rollout_ids(dir.path())
            .expect("a healthy system's enumeration must succeed");
        assert!(snap.is_empty());
    }
}
