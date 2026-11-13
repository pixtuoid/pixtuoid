//! The `native`-only runtime half of the Claude Code source:
//! `ClaudeCodeSource` (the pure `~/.claude/projects` JSONL watcher), the
//! shim↔daemon socket-path anchor, and the liveness-probe re-export.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::{SOURCE_NAME, cc_derive_label, claude_config_dir};
use crate::platform::NO_HOME;
use crate::source::cc_probe::cc_sessions_dir;
pub use crate::source::cc_probe::live_cc_session_ids;
use crate::source::jsonl::JsonlWatcher;
use crate::source::registry::{FocusProbe, Native, NativeTranscript};
use crate::source::{Source, TaggedSender};

/// A pure `~/.claude/projects` transcript watcher: no socket bind, no
/// presence/pid plumbing. Every source's hooks ride the one socket owned by
/// [`crate::source::hook::HookRouter`].
#[derive(Debug)]
pub struct ClaudeCodeSource {
    /// The watched `~/.claude/projects` transcript root.
    pub projects_root: PathBuf,
}

impl ClaudeCodeSource {
    // Stays on `ClaudeCodeSource` even though the `HookRouter` binds the socket:
    // this is the shim↔daemon parity anchor, pinned by socket_path_parity.rs.
    /// Resolve the hook rendezvous path — a Unix socket (or Windows named pipe) the shim connects to and the daemon binds.
    pub fn default_socket_path() -> PathBuf {
        // Set-but-empty/whitespace = unset: fall through to the default rather
        // than bind it verbatim. Same shape as the shim's paths.rs.
        if let Some(p) = crate::platform::path_env(crate::source::hook::SOCKET_ENV) {
            return p;
        }
        #[cfg(unix)]
        {
            // XDG spec: absolute-only. Empty → `/pixtuoid.sock` (fatal bind); relative →
            // shim/daemon cwd mis-rendezvous → treated as unset. Parity with paths.rs.
            if let Some(dir) = crate::platform::path_env("XDG_RUNTIME_DIR")
                && dir.is_absolute()
            {
                return dir.join("pixtuoid.sock");
            }
            // No XDG_RUNTIME_DIR (macOS, bare Linux): a per-user SUBDIR the bind
            // creates 0700-owned-by-us, NOT a flat predictable
            // `pixtuoid-{uid}.sock` a co-located user could squat/lock to
            // silently kill the hook plane (#485). Built from the same
            // `owned_tmp_dir()` that guard compares against, so the two
            // cannot drift apart.
            crate::source::hook::owned_tmp_dir().join(crate::source::hook::SOCKET_FILE_NAME)
        }
        #[cfg(windows)]
        {
            // Mirrors pixtuoid-hook/src/paths.rs — parity-pinned by
            // tests/socket_path_parity.rs, not shared (no dep edge).
            let user = crate::platform::text_env("USERNAME")
                .unwrap_or_else(|| "default".into())
                .replace('\\', "-");
            PathBuf::from(format!(r"\\.\pipe\pixtuoid-{user}"))
        }
    }

    /// Construct pointed at the default `~/.claude/projects` root, if
    /// [`claude_config_dir`] resolves.
    pub fn default_paths() -> Option<Self> {
        Some(Self {
            projects_root: claude_config_dir()?.join("projects"),
        })
    }
}

impl Source for ClaudeCodeSource {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        let mut watcher = JsonlWatcher::new(self.projects_root.clone(), SOURCE_NAME.to_string())
            .context("no transcript row")?;
        if let Some(sessions_dir) = cc_sessions_dir(&self.projects_root) {
            watcher = watcher.with_liveness_probe(std::sync::Arc::new(move || {
                live_cc_session_ids(&sessions_dir)
            }));
        }
        watcher.run(tx).await
    }
}

pub(crate) const TRANSCRIPT: NativeTranscript = NativeTranscript {
    activity_recency: Some(super::cc_activity_recency),
    head_label: None,
    derive_label: Some(cc_derive_label),
    cwd_derive: None,
};

pub(crate) const NATIVE: Native = Native {
    root: Some(projects_root),
    focus_probe: Some(FocusProbe {
        paths: probe_paths,
        present: Path::is_dir,
        pid_for_session,
    }),
};

fn projects_root() -> Option<PathBuf> {
    ClaudeCodeSource::default_paths().map(|s| s.projects_root)
}

fn probe_paths() -> std::result::Result<Vec<PathBuf>, &'static str> {
    registry_dir(&projects_root().ok_or(NO_HOME)?)
        .map(|dir| vec![dir])
        .ok_or("non-standard projects root")
}

/// CC's sessions registry, the dir its pid lookup reads: the SAME
/// standard-layout gate the probe applies, so a projects root pointed
/// elsewhere yields `None`.
pub fn registry_dir(projects_root: &Path) -> Option<PathBuf> {
    cc_sessions_dir(projects_root)
}

fn pid_for_session(sessions_dir: &Path, session_id: &str) -> Option<i32> {
    live_cc_session_ids(sessions_dir)?.focus_pid(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn default_socket_path_env_precedence_and_default_paths() {
        temp_env::with_var("XDG_RUNTIME_DIR", Some("/run/user/1000"), || {
            temp_env::with_var("PIXTUOID_SOCKET", Some("/tmp/explicit.sock"), || {
                assert_eq!(
                    ClaudeCodeSource::default_socket_path(),
                    PathBuf::from("/tmp/explicit.sock")
                );
            });

            for blank in ["", "   "] {
                temp_env::with_var("PIXTUOID_SOCKET", Some(blank), || {
                    assert_eq!(
                        ClaudeCodeSource::default_socket_path(),
                        PathBuf::from("/run/user/1000/pixtuoid.sock")
                    );
                });
            }

            temp_env::with_var_unset("PIXTUOID_SOCKET", || {
                assert_eq!(
                    ClaudeCodeSource::default_socket_path(),
                    PathBuf::from("/run/user/1000/pixtuoid.sock")
                );
            });
        });

        let uid = rustix::process::getuid().as_raw();
        let tmp_fallback = PathBuf::from(format!("/tmp/pixtuoid-{uid}/pixtuoid.sock"));
        temp_env::with_var_unset("PIXTUOID_SOCKET", || {
            for invalid in ["", "   ", "relative/run"] {
                temp_env::with_var("XDG_RUNTIME_DIR", Some(invalid), || {
                    assert_eq!(ClaudeCodeSource::default_socket_path(), tmp_fallback);
                });
            }

            temp_env::with_var_unset("XDG_RUNTIME_DIR", || {
                assert_eq!(
                    ClaudeCodeSource::default_socket_path(),
                    PathBuf::from(format!("/tmp/pixtuoid-{uid}/pixtuoid.sock"))
                );
            });
        });
    }

    #[test]
    fn default_paths_projects_root_honors_claude_config_dir() {
        let home = tempfile::tempdir().unwrap();
        let fallback = Some(home.path().join(".claude").join("projects"));
        let projects_root = || ClaudeCodeSource::default_paths().map(|s| s.projects_root);
        let pinned = [
            ("HOME", Some(home.path().as_os_str())),
            ("USERPROFILE", Some(home.path().as_os_str())),
        ];

        temp_env::with_vars(pinned, || {
            temp_env::with_var_unset("CLAUDE_CONFIG_DIR", || {
                assert_eq!(projects_root(), fallback, "unset CLAUDE_CONFIG_DIR");
            });

            let custom_dir = std::env::temp_dir().join("pixtuoid-claude-config-dir");
            temp_env::with_var("CLAUDE_CONFIG_DIR", Some(&custom_dir), || {
                assert_eq!(projects_root(), Some(custom_dir.join("projects")));
            });

            temp_env::with_var("CLAUDE_CONFIG_DIR", Some(""), || {
                assert_eq!(projects_root(), fallback, "empty CLAUDE_CONFIG_DIR");
            });
        });
    }

    #[cfg(unix)]
    #[test]
    fn pid_for_session_hits_misses_and_tolerates_garbage() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        // Our own pid is alive by construction.
        std::fs::write(
            sessions.join("self.json"),
            serde_json::json!({
                "pid": std::process::id(),
                "sessionId": "focus-sess",
                "startedAt": crate::source::proc_start::pid_start(std::process::id() as i32).map_or(0, |start| start.secs) * 1000,
                "status": "idle"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(sessions.join("junk.json"), "not json {{{").unwrap();

        assert_eq!(
            pid_for_session(&sessions, "focus-sess"),
            Some(std::process::id() as i32),
            "hit: the session's live registry pid"
        );
        assert_eq!(
            pid_for_session(&sessions, "unknown-sess"),
            None,
            "miss: unknown session resolves nothing"
        );
    }

    #[test]
    fn registry_dir_derives_the_sibling_only_for_the_standard_layout() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            registry_dir(&home.path().join("projects")),
            Some(home.path().join("sessions")),
            "the standard layout derives the SIBLING sessions dir"
        );
        // A root pointed elsewhere (file_name != "projects") derives nothing,
        // so it keeps the pure-mtime gate.
        assert_eq!(registry_dir(&home.path().join("fixture")), None);
    }
}
