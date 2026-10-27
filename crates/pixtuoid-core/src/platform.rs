//! Cross-platform home-dir resolution.
//!
//! On native Windows `HOME` is normally unset, and when Git Bash *does* export
//! one it is a POSIX-form path (`/c/Users/me`) that native Rust code must not
//! join onto — so `USERPROFILE` must win on Windows, or the watcher watches a
//! path no session ever writes to. On Unix, `HOME` stays authoritative.
//!
//! Every path env filter here is TRIM-based — empty or whitespace-only counts as
//! unset, so `XDG_CONFIG_HOME="   "` can't be unset for the app config but set
//! for the CLI config-dir resolution.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

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

/// USERPROFILE-first on Windows, HOME on Unix. See module doc for WHY.
pub(crate) fn user_home() -> PathBuf {
    resolve_home(
        cfg!(windows),
        path_env("USERPROFILE"),
        path_env("HOME"),
        std::env::temp_dir(),
    )
}

/// `Option` variant of `user_home`: the SAME USERPROFILE-vs-HOME rule, but
/// with no host-level fallback — `None` when nothing is set, so a caller can
/// supply its own.
pub fn user_home_opt() -> Option<PathBuf> {
    resolve_user_home_opt(cfg!(windows), path_env("USERPROFILE"), path_env("HOME"))
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

/// `HOME`-FIRST home resolution, then `USERPROFILE` on Windows — the OPPOSITE
/// of pixtuoid's generic `user_home`, and load-bearing: a Windows user who
/// exports `HOME` (Git Bash / MSYS2 / Cygwin) has these CLIs read their config
/// under `%HOME%\…`, so writing hooks to `%USERPROFILE%\…` would land them where
/// the CLI never loads them — installed, but no sprite. `None` when nothing
/// resolves.
///
/// Source-verified HOME-first CLIs (the only consumers):
/// - **CodeWhale** — `paths::user_home` = `$HOME ?? $USERPROFILE ??
///   HOMEDRIVE+HOMEPATH ?? dirs::home_dir()`. We mirror the first two; the
///   Windows-pair rung is unmirrored but fails loud with an error, never
///   silently to a wrong dir.
/// - **OpenClaw** — `infra/home-dir.ts::resolveRawOsHomeDir` = `$HOME ??
///   $USERPROFILE ?? os.homedir()`.
///
/// Every OTHER CLI uses its language stdlib (all `USERPROFILE`-first/only on
/// Windows), so they correctly use the generic `user_home`, NOT this.
pub fn home_first_dir() -> Option<PathBuf> {
    resolve_home_first(cfg!(windows), path_env("HOME"), path_env("USERPROFILE"))
}

/// Pure precedence core (`HOME`-first, then `USERPROFILE` on Windows), separated
/// so the Windows arm is unit-testable on any host. Unix with no `HOME` →
/// `None`: we deliberately don't reach for `dirs::home_dir`'s getpwuid fallback.
fn resolve_home_first(
    windows: bool,
    home: Option<PathBuf>,
    userprofile: Option<PathBuf>,
) -> Option<PathBuf> {
    home.or(if windows { userprofile } else { None })
}

/// Pure mapping of Go's `os.UserConfigDir()` for the platforms we ship, with
/// the OS and env values injected so every arm (incl. macOS) is unit-testable
/// on any host — the runtime `cfg!(target_os)` if-else couldn't test its
/// non-host arms. Pass `std::env::consts::OS` for `os`; `home` is the
/// already-resolved user home used for the relative fallbacks.
///
/// `appdata`/`xdg` must ALREADY be blank-filtered — [`path_env`] owns that, so a
/// blank `PathBuf` handed in here is taken as a real path.
pub fn resolve_user_config_dir(
    os: &str,
    appdata: Option<PathBuf>,
    xdg: Option<PathBuf>,
    home: &Path,
) -> PathBuf {
    match os {
        "macos" => home.join("Library/Application Support"),
        "windows" => appdata.unwrap_or_else(|| home.join("AppData/Roaming")),
        _ => xdg.unwrap_or_else(|| home.join(".config")),
    }
}

/// Pure resolution core, separated so the Windows branch is unit-testable
/// on any platform. Layers the host-level fallback over the shared `Option`
/// precedence, so the USERPROFILE-vs-HOME rule lives in ONE place.
fn resolve_home(
    windows: bool,
    userprofile: Option<PathBuf>,
    home: Option<PathBuf>,
    temp_dir: PathBuf,
) -> PathBuf {
    resolve_user_home_opt(windows, userprofile, home).unwrap_or_else(|| {
        if windows {
            temp_dir
        } else {
            PathBuf::from("/tmp")
        }
    })
}

/// The single USERPROFILE-vs-HOME precedence, in its purest form: USERPROFILE
/// then HOME on Windows, HOME only on Unix, with empty strings treated as
/// unset. Both `resolve_home` and [`user_home_opt`] derive from this — pure, so
/// the Windows arm is unit-testable on any host.
pub(crate) fn resolve_user_home_opt(
    windows: bool,
    userprofile: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if windows {
        // USERPROFILE is effectively always set on Windows; a lone HOME here
        // was set deliberately (MSYS users exporting a real Windows path).
        return userprofile.or(home);
    }
    home
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
    fn windows_prefers_userprofile_over_home() {
        let got = resolve_home(true, s(r"C:\Users\me"), s("/c/Users/me"), t());
        assert_eq!(got, PathBuf::from(r"C:\Users\me"));
    }

    #[test]
    fn windows_falls_back_to_home_then_tempdir() {
        assert_eq!(
            resolve_home(true, None, s("/c/Users/me"), t()),
            PathBuf::from("/c/Users/me")
        );
        assert_eq!(resolve_home(true, None, None, t()), t());
    }

    #[test]
    fn unix_home_stays_authoritative() {
        assert_eq!(
            resolve_home(false, s(r"C:\ignored"), s("/Users/me"), t()),
            PathBuf::from("/Users/me")
        );
        assert_eq!(resolve_home(false, None, None, t()), PathBuf::from("/tmp"));
    }

    #[test]
    fn user_home_opt_has_no_host_fallback() {
        assert_eq!(resolve_user_home_opt(true, None, None), None);
        assert_eq!(resolve_user_home_opt(false, None, None), None);
    }

    #[test]
    fn user_config_dir_macos_is_application_support() {
        assert_eq!(
            resolve_user_config_dir(
                "macos",
                Some(r"C:\ignored".into()),
                Some("/ignored".into()),
                Path::new("/Users/me")
            ),
            PathBuf::from("/Users/me/Library/Application Support")
        );
    }

    #[test]
    fn user_config_dir_windows_prefers_appdata_then_roaming_fallback() {
        assert_eq!(
            resolve_user_config_dir(
                "windows",
                s(r"C:\Users\ada\AppData\Roaming"),
                None,
                Path::new(r"C:\Users\ada")
            ),
            PathBuf::from(r"C:\Users\ada\AppData\Roaming")
        );
        assert_eq!(
            resolve_user_config_dir("windows", None, None, Path::new(r"C:\Users\ada")),
            PathBuf::from(r"C:\Users\ada").join("AppData/Roaming")
        );
    }

    #[test]
    fn user_config_dir_linux_prefers_xdg_then_dot_config() {
        assert_eq!(
            resolve_user_config_dir("linux", None, s("/xdg/cfg"), Path::new("/home/u")),
            PathBuf::from("/xdg/cfg")
        );
        assert_eq!(
            resolve_user_config_dir("linux", None, None, Path::new("/home/u")),
            PathBuf::from("/home/u/.config")
        );
        assert_eq!(
            resolve_user_config_dir("freebsd", None, s("/xdg/cfg"), Path::new("/home/u")),
            PathBuf::from("/xdg/cfg")
        );
    }

    #[test]
    fn home_first_is_home_then_userprofile_on_windows() {
        assert_eq!(
            resolve_home_first(true, s(r"C:\Users\me"), s(r"C:\Users\other")),
            s(r"C:\Users\me")
        );
        assert_eq!(
            resolve_home_first(true, None, s(r"C:\Users\me")),
            s(r"C:\Users\me")
        );
        assert_eq!(resolve_home_first(true, None, None), None);
    }

    #[test]
    fn home_first_is_home_only_on_unix() {
        assert_eq!(
            resolve_home_first(false, s("/Users/me"), s(r"C:\ignored")),
            s("/Users/me")
        );
        assert_eq!(resolve_home_first(false, None, s(r"C:\ignored")), None);
    }
}
