//! The `native`-only runtime half of the Claude Code source:
//! `ClaudeCodeSource` (the pure `~/.claude/projects` JSONL watcher), the
//! shim↔daemon socket-path anchor, and the liveness-probe re-export.

use std::path::PathBuf;

use anyhow::Result;

use super::{SOURCE_NAME, cc_derive_label, cc_session_ended, claude_config_dir, decode_cc_line};
use crate::source::cc_probe::cc_sessions_dir;
pub use crate::source::cc_probe::live_cc_session_ids;
use crate::source::jsonl::{ChildEndUnclaims, JsonlWatcher};
use crate::source::{Source, TaggedSender};

/// A pure `~/.claude/projects` transcript watcher: no socket bind, no
/// presence/pid plumbing. Every source's hooks ride the one socket owned by
/// [`crate::source::hook::HookRouter`].
#[derive(Debug)]
pub struct ClaudeCodeSource {
    /// The watched `~/.claude/projects` transcript root.
    pub projects_root: PathBuf,
    /// The #246 child-end un-claim side-channel — CONSUMER only (releases CC
    /// child-transcript claims on the rare blocked-stop continuation; the
    /// PRODUCER tee lives on the `HookRouter`). `None` disables it.
    pub child_end_unclaims: Option<ChildEndUnclaims>,
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
            // `owned_socket_dir()` that guard compares against, so the two
            // cannot drift apart.
            crate::source::hook::owned_socket_dir().join(crate::source::hook::SOCKET_FILE_NAME)
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

    /// Construct pointed at the default `~/.claude/projects` root.
    pub fn default_paths() -> Self {
        let projects_root = claude_config_dir()
            .unwrap_or_else(|| crate::platform::user_home().join(".claude"))
            .join("projects");
        Self {
            projects_root,
            child_end_unclaims: None,
        }
    }
}

/// THE CC watcher wiring — the per-source decoder/deriver set, in one place.
/// Production, the out-of-crate integration harness (`tests/watcher/`) and the
/// `--live` snapshot example all need the SAME chain, and hand-mirroring it
/// drifted three times (#203, #632, the activity clock), each time silently
/// exercising a configuration production does not run. Callers add only what is
/// genuinely theirs.
pub fn cc_watcher(projects_root: std::path::PathBuf) -> JsonlWatcher {
    JsonlWatcher::new(
        projects_root,
        SOURCE_NAME.to_string(),
        decode_cc_line,
        cc_session_ended,
    )
    .with_label_deriver(cc_derive_label)
    .with_activity_recency(super::cc_activity_recency)
}

impl Source for ClaudeCodeSource {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        let mut watcher = cc_watcher(self.projects_root.clone());
        if let Some(sessions_dir) = cc_sessions_dir(&self.projects_root) {
            watcher = watcher.with_liveness_probe(std::sync::Arc::new(move || {
                live_cc_session_ids(&sessions_dir)
            }));
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
        let fallback_suffix = PathBuf::from(".claude").join("projects");

        temp_env::with_var_unset("CLAUDE_CONFIG_DIR", || {
            let unset_paths = ClaudeCodeSource::default_paths();
            assert!(
                unset_paths.projects_root.ends_with(&fallback_suffix),
                "projects_root must end with .claude/projects, got {:?}",
                unset_paths.projects_root
            );
        });

        let custom_dir = std::env::temp_dir().join("pixtuoid-claude-config-dir");
        temp_env::with_var("CLAUDE_CONFIG_DIR", Some(&custom_dir), || {
            assert_eq!(
                ClaudeCodeSource::default_paths().projects_root,
                custom_dir.join("projects")
            );
        });

        temp_env::with_var("CLAUDE_CONFIG_DIR", Some(""), || {
            let empty_paths = ClaudeCodeSource::default_paths();
            assert!(
                empty_paths.projects_root.ends_with(&fallback_suffix),
                "empty CLAUDE_CONFIG_DIR must fall back to .claude/projects, got {:?}",
                empty_paths.projects_root
            );
        });
    }
}
