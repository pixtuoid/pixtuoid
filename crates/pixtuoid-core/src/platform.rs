//! Cross-platform home-dir resolution, one resolver per runtime rule a CLI uses.
//!
//! On native Windows `HOME` is normally unset, and when Git Bash *does* export
//! one it is a POSIX-form path (`/c/Users/me`) that native Rust code must not
//! join onto — so no resolver here reads it on Windows, or the watcher watches a
//! path no session ever writes to. On Unix, `HOME` stays authoritative.
//!
//! Every path env filter here is TRIM-based — empty or whitespace-only counts as
//! unset, so `XDG_CONFIG_HOME="   "` can't be unset for the app config but set
//! for the CLI config-dir resolution.

use std::ffi::OsStr;
use std::path::PathBuf;

/// A PATH-valued env var, read as BYTES. `env::var` returns `Err` on a value
/// that is not UTF-8 — and a filesystem path is not required to be, on any
/// Unix — so reading a path override with it DROPS a legal value and sends the
/// resolver silently to a fallback location: the user's override is ignored and
/// the office comes up empty (the #880/#343/#342/#195 failure shape, reached
/// through the encoding rather than the precedence). `None` for unset, empty,
/// or whitespace-only — a blank value is never a valid home/config dir.
pub fn path_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !is_blank(v))
        .map(PathBuf::from)
}

/// [`path_env`]'s TRIMMING twin — hermes mirrors Python's
/// `os.environ.get(K, "").strip()`, so `" /srv/hm "` IS `/srv/hm` upstream. A
/// value that is not UTF-8 cannot be trimmed as text and is taken VERBATIM:
/// preserving a legal path beats stripping hypothetical padding from one.
pub fn path_env_trimmed(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).and_then(trimmed_path)
}

/// [`path_env_trimmed`] for a value already read.
pub(crate) fn trimmed_path(raw: std::ffi::OsString) -> Option<PathBuf> {
    match raw.to_str() {
        Some(s) => (!s.trim().is_empty()).then(|| PathBuf::from(s.trim())),
        None => Some(PathBuf::from(raw)),
    }
}

/// A TEXT-valued env var, `None` when unset, EMPTY or not UTF-8 — never trimmed,
/// as `NO_COLOR`'s "present and not an empty string" needs (a location is
/// [`path_env`]; `env::var` is banned by `clippy.toml`).
pub fn text_env(name: &str) -> Option<String> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())?
        .into_string()
        .ok()
}
/// Whitespace test that never rejects a non-UTF-8 value: the lossy form is used
/// ONLY for the emptiness question, never as the value, and its replacement
/// chars are not whitespace — so ill-formed bytes read as present, not blank.
fn is_blank(v: &OsStr) -> bool {
    v.to_string_lossy().trim().is_empty()
}

/// [`user_home_opt`], else [`or_host_fallback`]'s root.
pub(crate) fn user_home() -> PathBuf {
    or_host_fallback(user_home_opt())
}

/// `home`, else the host's temp root.
pub(crate) fn or_host_fallback(home: Option<PathBuf>) -> PathBuf {
    resolve_host_fallback(cfg!(windows), home, std::env::temp_dir())
}

/// `~` as a CLI on Node's `os.homedir` or Rust's `std::env::home_dir` resolves
/// it, with no host-level fallback: `None` when nothing resolves, so a caller
/// can supply its own. It is std's own: `USERPROFILE`, else the profile dir
/// `GetUserProfileDirectoryW` reports, on Windows; `HOME`, else the passwd
/// database, elsewhere (std 1.99.1 `sys/paths/windows.rs:174-179`,
/// `sys/paths/unix.rs:430-484`), the reads libuv's `uv_os_homedir` makes too
/// (node v26.13.0 `deps/uv/src/win/util.c:970-990`,
/// `deps/uv/src/unix/core.c:1265-1281`).
pub fn user_home_opt() -> Option<PathBuf> {
    std::env::home_dir()
}

/// `~` as a Go CLI resolves it, Go's `os.UserHomeDir`: `USERPROFILE` on
/// Windows, `HOME` elsewhere, and no OS fallback, so `None` where Go errors
/// (golang/go `src/os/file.go:605-623` @ be57ebb0f3).
pub fn go_user_home_dir() -> Option<PathBuf> {
    path_env(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
}

/// `~` as a CLI on the `dirs` crate resolves it (dirs 6.0.0 @ 00511bdc25,
/// dirs-sys 0.5.0 @ 8bcd4aa2c3): on Windows `FOLDERID_Profile`, which no
/// variable moves (`src/win.rs:5`, `src/lib.rs:172-173`); elsewhere a copy of
/// std's `home_dir` (`src/lib.rs:33-37`), so [`user_home_opt`].
pub fn dirs_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    return crate::win32::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Profile).ok();
    #[cfg(not(windows))]
    user_home_opt()
}

/// Hand back a CLI's env home override unchanged, warning when it is RELATIVE.
///
/// Each process resolves it against its OWN cwd — one string, two directories,
/// and the failure is otherwise mute: an empty office and no error (#880).
pub(crate) fn warn_if_relative_override(var: &str, dir: PathBuf) -> PathBuf {
    if !dir.is_absolute() {
        // Undeduped: this resolves a handful of times per run (source
        // construction + install), never per event.
        tracing::warn!(
            var = %var,
            dir = ?dir,
            "env home override is a RELATIVE path; it resolves against each \
             process's own cwd, so pixtuoid and the CLI may disagree on where \
             this points — set an absolute path",
        );
    }
    dir
}

/// Pure core of [`or_host_fallback`], separated so the Windows arm is
/// unit-testable on any host.
fn resolve_host_fallback(windows: bool, home: Option<PathBuf>, temp_dir: PathBuf) -> PathBuf {
    home.unwrap_or_else(|| {
        if windows {
            temp_dir
        } else {
            PathBuf::from("/tmp")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<PathBuf> {
        Some(PathBuf::from(v))
    }

    /// Stand-in for the host temp dir the Windows fallback uses.
    fn t() -> PathBuf {
        PathBuf::from("T")
    }

    /// A filesystem path is not required to be UTF-8 on Unix, and `env::var`
    /// returns `Err(NotUnicode)` for one that isn't — DROPPING a legal override
    /// and sending the resolver to a fallback dir, silently. Pins the read at
    /// the boundary where that decision is made.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_home_is_honored_not_dropped() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        // 0xFF is never valid UTF-8, and is a legal byte in a Unix path.
        let bad = OsString::from_vec(b"/tmp/pixtuoid-caf\xFF".to_vec());
        let got = temp_env::with_vars(
            [("USERPROFILE", None), ("HOME", Some(bad.as_os_str()))],
            user_home_opt,
        );

        assert_eq!(
            got,
            Some(PathBuf::from(&bad)),
            "a non-UTF-8 HOME must be honored verbatim, not dropped to a fallback"
        );
    }

    /// The read boundary owns blank-filtering AND (for the trimming twin) the
    /// `.strip()` mirror hermes needs — the pure resolvers below assume values
    /// that already went through here.
    #[test]
    fn path_env_filters_blanks_and_the_trimmed_twin_strips() {
        const K: &str = "PIXTUOID_PATH_ENV_TEST";

        temp_env::with_var_unset(K, || assert_eq!(path_env(K), None, "unset"));
        for blank in ["", "   ", "\t \n"] {
            temp_env::with_var(K, Some(blank), || {
                assert_eq!(path_env(K), None, "{blank:?} counts as unset");
                assert_eq!(path_env_trimmed(K), None, "{blank:?} counts as unset");
            });
        }
        temp_env::with_var(K, Some(" /srv/hm "), || {
            assert_eq!(
                path_env(K),
                Some(PathBuf::from(" /srv/hm ")),
                "the plain read TESTS the padding, it does not strip it"
            );
            assert_eq!(
                path_env_trimmed(K),
                Some(PathBuf::from("/srv/hm")),
                "the trimming twin mirrors hermes's `os.environ.get(K, '').strip()`"
            );
        });
    }

    /// The whole point of reading as bytes: an ill-formed value survives BOTH
    /// readers rather than being dropped, and the trimming twin takes it
    /// verbatim because it cannot be trimmed as text.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_value_survives_both_readers() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        const K: &str = "PIXTUOID_PATH_ENV_BYTES_TEST";

        let bad = OsString::from_vec(b"/tmp/caf\xFF".to_vec());
        temp_env::with_var(K, Some(&bad), || {
            #[expect(clippy::disallowed_methods, reason = "self-test of clippy.toml's ban")]
            let dropped = std::env::var(K).is_err();
            assert!(dropped, "precondition: env::var is what DROPS this value");
            assert_eq!(text_env(K), None, "text_env drops it, as env::var does");
            assert_eq!(path_env(K), Some(PathBuf::from(&bad)));
            assert_eq!(path_env_trimmed(K), Some(PathBuf::from(&bad)));
        });
    }

    #[test]
    fn text_env_reads_an_empty_value_as_unset_and_never_trims() {
        const K: &str = "PIXTUOID_TEXT_ENV_TEST";
        temp_env::with_var(K, Some(""), || assert_eq!(text_env(K), None));
        for kept in [" ", " x "] {
            temp_env::with_var(K, Some(kept), || {
                assert_eq!(text_env(K).as_deref(), Some(kept));
            });
        }
    }

    #[test]
    fn a_relative_override_warns_and_an_absolute_one_stays_quiet() {
        let noisy = crate::test_capture::capture_logs(|| {
            warn_if_relative_override("CLAUDE_CONFIG_DIR", PathBuf::from("rel/dir"));
        });
        assert!(
            noisy.contains("CLAUDE_CONFIG_DIR") && noisy.contains("rel/dir"),
            "a relative override must name itself in the warn floor:\n{noisy}"
        );

        // Negative control: the same helper on an absolute path must be silent,
        // or the warn is noise rather than signal. The path is per-platform
        // because `/abs/dir` is NOT absolute on Windows — it is drive-relative,
        // so it resolves against the process's current DRIVE and the warn is
        // CORRECT there. Hardcoding the Unix form asserted the opposite.
        let absolute = if cfg!(windows) {
            r"C:\abs\dir"
        } else {
            "/abs/dir"
        };
        let quiet = crate::test_capture::capture_logs(|| {
            warn_if_relative_override("CLAUDE_CONFIG_DIR", PathBuf::from(absolute));
        });
        assert!(
            !quiet.contains("CLAUDE_CONFIG_DIR"),
            "an absolute override must not warn:\n{quiet}"
        );
    }

    #[test]
    fn warn_if_relative_override_is_pass_through() {
        for p in ["rel/dir", "/abs/dir", ""] {
            assert_eq!(
                warn_if_relative_override("X", PathBuf::from(p)),
                PathBuf::from(p),
                "the helper reports, it never rewrites"
            );
        }
    }

    #[test]
    fn the_generic_home_reads_its_platform_variable_and_never_a_git_bash_home() {
        let (own, other) = if cfg!(windows) {
            ("USERPROFILE", "HOME")
        } else {
            ("HOME", "USERPROFILE")
        };
        temp_env::with_vars([(own, Some("/own")), (other, Some("/other"))], || {
            assert_eq!(user_home_opt(), s("/own"));
        });
        // With `USERPROFILE` unset, std asks Windows for the profile dir.
        #[cfg(windows)]
        temp_env::with_vars(
            [("USERPROFILE", None), ("HOME", Some("/c/Users/me"))],
            || assert_ne!(user_home_opt(), s("/c/Users/me")),
        );
    }

    #[test]
    fn an_unresolved_home_falls_back_to_the_host_temp_root() {
        assert_eq!(resolve_host_fallback(true, None, t()), t());
        assert_eq!(
            resolve_host_fallback(false, None, t()),
            PathBuf::from("/tmp")
        );
        assert_eq!(
            resolve_host_fallback(true, s(r"C:\Users\me"), t()),
            PathBuf::from(r"C:\Users\me")
        );
    }

    #[test]
    fn go_user_home_dir_reads_its_platform_variable_alone() {
        let (own, other) = if cfg!(windows) {
            ("USERPROFILE", "HOME")
        } else {
            ("HOME", "USERPROFILE")
        };
        temp_env::with_vars([(own, None), (other, Some("/elsewhere"))], || {
            assert_eq!(go_user_home_dir(), None, "Go has no OS fallback");
        });
        temp_env::with_vars(
            [(own, Some("/home/u")), (other, Some("/elsewhere"))],
            || {
                assert_eq!(go_user_home_dir(), s("/home/u"));
            },
        );
    }
}
