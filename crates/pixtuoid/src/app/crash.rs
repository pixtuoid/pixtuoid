//! Crash reporting: the panic hook that restores the terminal, appends a
//! timestamped backtrace to [`crash_log_path`], and prints a pre-filled GitHub
//! issue URL.

use std::path::PathBuf;

pub(crate) fn install_crash_hook() {
    std::panic::set_hook(Box::new(|info| {
        // stdout is the stream `setup_terminal` ENTERED the alt screen on, so the
        // restore must go there — on stderr, `pixtuoid run 2>/dev/null` strands it.
        let _ = crate::tui::unwind_terminal_modes(
            &mut std::io::stdout(),
            crossterm::terminal::disable_raw_mode,
        );

        let version = env!("CARGO_PKG_VERSION");
        let crash_path = crash_log_path();
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        let panic_msg = extract_panic_message(info);
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();

        let bt = std::backtrace::Backtrace::force_capture();
        let bt_str = bt.to_string();

        let mut report = String::new();
        report.push_str(&format!("pixtuoid v{version} crashed at {timestamp}\n"));
        report.push_str(&format!("{panic_msg}\n  at {location}\n\n"));
        report.push_str(&bt_str);
        report.push('\n');

        // Owner-only: a backtrace carries the same project paths and agent ids
        // the runtime log does.
        if let Ok(mut f) =
            crate::run_log::open_private_append(&crash_path, std::fs::OpenOptions::new())
        {
            use std::io::Write;
            let _ = f.write_all(report.as_bytes());
        }

        let issue_url = build_issue_url(version, &panic_msg, &location, &bt_str, &crash_path);

        // The payload and the path are text we don't control, so stripped
        // before they reach the terminal; the URL is percent-encoded already.
        let (msg, path) = (
            crate::strip_lines(&panic_msg, "\n  "),
            crate::display_path(&crash_path),
        );
        let notice = format!(
            "\n\x1b[1;31mpixtuoid v{version} crashed — sorry about that.\x1b[0m\n\n\
             \x20 \x1b[2m{msg}\x1b[0m\n\
             \x20 \x1b[2mat {location}\x1b[0m\n\n\
             \x20 \x1b[1mHelp fix it\x1b[0m — open this link to file a pre-filled bug report\n\
             \x20 (panic + backtrace already included, no typing needed):\n\n\
             \x20 \x1b[4m{issue_url}\x1b[0m\n\n\
             \x20 Full backtrace saved to \x1b[2m{path}\x1b[0m\n\
             \x20 \x1b[2m(attach if the reviewer asks — the link above only carries a truncated trace)\x1b[0m\n\n"
        );
        // Not `eprintln!`: a panic inside the hook aborts the process.
        let _ = std::io::Write::write_all(&mut std::io::stderr(), notice.as_bytes());
    }));
}

fn extract_panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        return (*s).to_string();
    }
    if let Some(s) = info.payload().downcast_ref::<String>() {
        return s.clone();
    }
    "unknown panic".to_string()
}

fn build_issue_url(
    version: &str,
    panic_msg: &str,
    location: &str,
    backtrace: &str,
    crash_path: &std::path::Path,
) -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    const PANIC_TITLE_MAX_LEN: usize = 80;
    let title_msg = if panic_msg.len() > PANIC_TITLE_MAX_LEN {
        let cut = truncate_to_char_boundary(panic_msg, PANIC_TITLE_MAX_LEN);
        format!("{}…", &panic_msg[..cut])
    } else {
        panic_msg.to_string()
    };
    let title = format!("Crash: {title_msg}");

    // Truncated so the URL stays under GitHub's limit, pinned by
    // `build_issue_url_truncates_long_backtrace`.
    const MAX_BT: usize = 1500;
    let bt_body = if backtrace.len() > MAX_BT {
        let cut = truncate_to_char_boundary(backtrace, MAX_BT);
        format!(
            "{}\n\n... truncated — see {} for full trace",
            &backtrace[..cut],
            crash_path.display()
        )
    } else {
        backtrace.to_string()
    };

    let body = format!(
        "## Environment\n\
         - **Version:** {version}\n\
         - **OS:** {os}/{arch}\n\n\
         ## Panic\n\
         ```\n{panic_msg}\n  at {location}\n```\n\n\
         ## Backtrace\n\
         ```\n{bt_body}\n```\n"
    );

    format!(
        "{}/issues/new?title={}&body={}",
        pixtuoid_scene::hit::REPO_URL,
        percent_encode(&title),
        percent_encode(&body),
    )
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn truncate_to_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

fn crash_log_path() -> PathBuf {
    // Empty or RELATIVE XDG_STATE_HOME = unset (XDG spec): an unfiltered "" yields
    // root `/pixtuoid/...`, a relative one lands CWD-relative.
    if let Some(state) = crate::install::nonempty_abs_env("XDG_STATE_HOME") {
        return state.join("pixtuoid").join("crash.log");
    }
    if let Some(home) = pixtuoid_core::platform::user_home_opt() {
        return home.join(".cache").join("pixtuoid").join("crash.log");
    }
    std::env::temp_dir().join("pixtuoid-crash.log")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// `name`'s path as libtest's `--exact` reads it: the module path less the
    /// crate, so a re-exec still finds the test after this module moves.
    #[cfg(unix)]
    fn this_test(name: &str) -> String {
        let module = module_path!()
            .split_once("::")
            .map_or(module_path!(), |(_, m)| m);
        format!("{module}::{name}")
    }

    /// Unix-only, and so is every test that reads it: crossterm dispatches on a
    /// PROCESS-GLOBAL ansi-support flag rather than on the writer, and under
    /// `windows-test` that flag is false — the sequences go to the real console,
    /// so no writer, in-memory or piped, ever sees a byte to assert on.
    #[cfg(unix)]
    const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l";

    /// The panic hook is process-global and writes to a real fd, so the only way
    /// to observe which stream it chose is from outside: re-exec this test binary
    /// with the child marker, let a real panic fire the hook, read the pipes apart.
    #[cfg(unix)]
    #[test]
    fn the_hook_restores_on_stdout_and_keeps_the_report_on_stderr() {
        const CHILD: &str = "PIXTUOID_CRASH_HOOK_STREAM_CHILD";
        if std::env::var_os(CHILD).is_some() {
            install_crash_hook();
            panic!("deliberate panic: the crash-hook stream probe");
        }
        // Isolated XDG_STATE_HOME so the child's crash.log never lands in the
        // developer's real ~/.cache.
        let state = tempfile::tempdir().unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &this_test("the_hook_restores_on_stdout_and_keeps_the_report_on_stderr"),
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("XDG_STATE_HOME", state.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stdout.contains(LEAVE_ALT_SCREEN),
            "the alt screen is ENTERED on stdout (tui::setup_terminal), so the panic \
             restore must go there — on stderr, `pixtuoid run 2>/dev/null` strands it.\n\
             stdout: {stdout:?}"
        );
        assert!(
            !stderr.contains(LEAVE_ALT_SCREEN),
            "stderr is the human-readable channel, not the terminal-mode one: {stderr:?}"
        );
        assert!(
            stderr.contains("crashed"),
            "the crash report itself still belongs on stderr: {stderr:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_hook_strips_the_payload_and_survives_a_broken_stderr() {
        const CHILD: &str = "PIXTUOID_CRASH_HOOK_PAYLOAD_CHILD";
        const PAYLOAD: &str = "\u{1b}]0;pwned\u{7}\u{202e}";
        /// libtest's exit status for a run with a failed test.
        const LIBTEST_FAILED: i32 = 101;
        if std::env::var_os(CHILD).is_some() {
            install_crash_hook();
            panic!("deliberate panic {PAYLOAD}\nsecond line");
        }
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join(format!("state{PAYLOAD}"));
        let child = || {
            let mut c = std::process::Command::new(std::env::current_exe().unwrap());
            c.args([
                "--exact",
                &this_test("the_hook_strips_the_payload_and_survives_a_broken_stderr"),
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("XDG_STATE_HOME", &state);
            c
        };

        let out = child().output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("deliberate panic ]0;pwned\n  second line"),
            "{stderr:?}"
        );
        assert!(
            stderr.contains("state]0;pwned/pixtuoid/crash.log"),
            "{stderr:?}"
        );
        assert!(
            !stderr.contains(PAYLOAD) && !stderr.contains('\u{202e}'),
            "{stderr:?}"
        );

        // A reader-gone pipe, not a closed fd: std swallows EBADF on stderr.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let status = child()
            .stdout(std::process::Stdio::null())
            .stderr(writer)
            .status()
            .unwrap();
        assert_eq!(
            status.code(),
            Some(LIBTEST_FAILED),
            "a failed write in the hook is a second panic, and that aborts: {status:?}"
        );
    }

    #[test]
    fn truncate_ascii() {
        assert_eq!(truncate_to_char_boundary("hello world", 5), 5);
        assert_eq!(
            &"hello world"[..truncate_to_char_boundary("hello world", 5)],
            "hello"
        );
    }

    #[test]
    fn truncate_multibyte_boundary() {
        let s = "café";
        assert_eq!(s.len(), 5);
        let cut = truncate_to_char_boundary(s, 4);
        assert_eq!(cut, 3);
        assert_eq!(&s[..cut], "caf");
    }

    #[test]
    fn truncate_beyond_length() {
        assert_eq!(truncate_to_char_boundary("short", 100), 5);
    }

    #[test]
    fn percent_encode_ascii() {
        assert_eq!(percent_encode("hello"), "hello");
        assert_eq!(percent_encode("a b"), "a%20b");
    }

    #[test]
    fn percent_encode_special_chars() {
        assert_eq!(percent_encode("#&="), "%23%26%3D");
        assert_eq!(percent_encode("a\nb"), "a%0Ab");
    }

    #[test]
    fn build_issue_url_starts_with_github() {
        let url = build_issue_url(
            "0.4.0",
            "test panic",
            "file.rs:1:1",
            "bt",
            Path::new("/tmp/x"),
        );
        assert!(url.starts_with(&format!("{}/issues/new?", pixtuoid_scene::hit::REPO_URL)));
        assert!(url.contains("title="));
        assert!(url.contains("body="));
    }

    #[test]
    fn build_issue_url_truncates_long_backtrace() {
        /// GitHub's limit on a URL, in bytes.
        const GITHUB_URL_LIMIT: usize = 8191;
        // A newline percent-encodes to three bytes, so untruncated this trace
        // alone is three times the limit.
        let long_bt = "\n".repeat(GITHUB_URL_LIMIT);
        let url = build_issue_url("0.4.0", "msg", "loc", &long_bt, Path::new("/tmp/x"));
        assert!(url.len() < GITHUB_URL_LIMIT, "{}", url.len());
        assert!(url.contains("truncated"));
    }

    #[test]
    fn crash_log_path_rejects_a_relative_xdg_state_home() {
        for rel in ["", "   ", "rel/state", "~/state"] {
            temp_env::with_var("XDG_STATE_HOME", Some(rel), || {
                let home =
                    pixtuoid_core::platform::user_home_opt().expect("a home dir in the test env");
                assert_eq!(
                    crash_log_path(),
                    home.join(".cache").join("pixtuoid").join("crash.log"),
                    "relative XDG_STATE_HOME {rel:?} must fall back to ~/.cache"
                );
            });
        }
        let abs = if cfg!(windows) { "C:/state" } else { "/state" };
        temp_env::with_var("XDG_STATE_HOME", Some(abs), || {
            assert_eq!(
                crash_log_path(),
                PathBuf::from(format!("{abs}/pixtuoid/crash.log"))
            );
        });
    }
}
