//! The runtime log on disk, which the TUI and the floating window write and
//! `doctor`, `sources` and the Sources panel read: where it lives, a run's
//! own file and its tidy-up, and the bounded read.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

fn runs_under(base: &Path) -> LogLocation {
    LogLocation::Runs(base.join("pixtuoid").join("logs"))
}

/// How long a run's log outlives its last write before a later run removes
/// it: WezTerm's week (`env-bootstrap/src/ringlog.rs` `prune_old_logs`), so
/// what `doctor` reports is recent.
const RUN_LOG_RETAIN: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A run's sink, which a live run's [`prune_runs`] must not remove: std shares
/// delete by default on Windows (`sys/fs/windows.rs` `share_mode`).
fn live_run_options() -> OpenOptions {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let mut opts = OpenOptions::new();
        opts.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
        opts
    }
    #[cfg(not(windows))]
    OpenOptions::new()
}

/// A run's file: its start in UTC, then its pid, so runs started in one
/// second never share one and name order is start order.
fn run_file_name(start: SystemTime, tag: impl std::fmt::Display) -> String {
    let start: chrono::DateTime<chrono::Utc> = start.into();
    format!("{}-{tag}.{}", start.format("%Y%m%dT%H%M%SZ"), RUN_LOG_EXT)
}

/// Remove the runs in `dir` last written over [`RUN_LOG_RETAIN`] before `now`,
/// except one whose run still holds it open: a run quiet for a week keeps
/// logging to its file.
fn prune_runs(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != RUN_LOG_EXT) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|written| {
                now.duration_since(written)
                    .is_ok_and(|age| age > RUN_LOG_RETAIN)
            });
        // Windows refuses the remove itself.
        #[cfg(unix)]
        let live = || {
            std::fs::File::open(&path)
                .is_ok_and(|f| matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
        };
        #[cfg(not(unix))]
        let live = || false;
        if stale && !live() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Open a diagnostic sink for append, creating it — and any directory it needs —
/// OWNER-ONLY on Unix, the ONE opener the runtime log and the crash log share:
/// these are the most revealing artifacts pixtuoid creates (warn-floor records
/// carry full transcript paths and `AgentId`s that for some sources ARE the
/// workspace cwd; `--log-level debug` dumps the agent's own shell commands and
/// edited file paths).
///
/// The mode binds AT CREATION — race-free, no window in which a co-located user
/// can open the sink. An already-existing DIRECTORY keeps its mode: silently
/// re-moding a user's `~/.cache` is not ours.
///
/// # Errors
///
/// If the directory can't be created or the file can't be opened.
pub(crate) fn open_private_append(
    path: &Path,
    mut opts: OpenOptions,
) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(parent)?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(parent)?;
    }
    opts.create(true).append(true);
    crate::install::owner_only_create(&mut opts);
    opts.open(path)
}

/// One-deep rotation at startup of the file `$PIXTUOID_LOG` names (log →
/// log.old) keeps the last two generations. Accepted edge: with several
/// instances sharing that file, one instance's startup rotation renames it out
/// from under a running sibling (its fd follows; a later rotation strands it on
/// an unlinked inode).
fn rotate_if_large(path: &Path) {
    let too_large = std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_ROTATE_BYTES);
    if too_large {
        // APPEND ".old" rather than with_extension: a custom $PIXTUOID_LOG like
        // app.log must rotate to app.log.old (not clobber a sibling app.old), and a
        // path already ending in .old must not rename onto itself. OsString
        // concatenation, not format!/display(): display() is lossy on non-UTF-8
        // paths, and a U+FFFD-mangled target would silently break the rotation.
        let mut old = path.as_os_str().to_os_string();
        old.push(".old");
        let _ = std::fs::rename(path, &old);
    }
}

/// Read the warn-floor log for a drift scan, separating "there is no log yet" from "the log
/// could not be read" — the latter gets a warning line. A missing log is the ordinary
/// no-TUI-run-yet state and genuinely means "no drift recorded"; every other error class
/// leaves the counts UNKNOWN, and folding those into the same silent empty string made
/// `doctor` positively assert `✓ no decode drift` off an input it never read.
///
/// The warning is [`crate::strip_control_chars`]-ed where it is MINTED, for the
/// reason `crate::display_path` gives: the path comes from
/// `PIXTUOID_LOG`/`XDG_STATE_HOME`.
pub(crate) fn read_log(path: &std::path::Path) -> (String, Option<String>) {
    read_log_tail(path, u64::MAX)
}

/// [`read_log`] of the last `max` bytes of `path`, from the first whole line
/// in them.
fn read_log_tail(path: &std::path::Path, max: u64) -> (String, Option<String>) {
    let tail = || -> std::io::Result<String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(path)?;
        // One byte before the cut, so a cut on a line start keeps that line.
        let skip = file.metadata()?.len().saturating_sub(max).saturating_sub(1);
        file.seek(SeekFrom::Start(skip))?;
        let mut bytes = Vec::new();
        file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
        // Past `max`: the byte before the cut was read, so the run is cut.
        if bytes.len() as u64 > max {
            let line = bytes
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |nl| nl + 1);
            bytes.drain(..line);
        }
        String::from_utf8(bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    };
    match tail() {
        Ok(s) => (s, None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), None),
        Err(e) => (
            String::new(),
            Some(crate::strip_control_chars(&format!(
                "log unreadable: {} ({e}) — the decode-drift counts are not meaningful",
                path.display()
            ))),
        ),
    }
}

/// Where the runtime log lives: the one file `$PIXTUOID_LOG` names, or a
/// directory holding a file per run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LogLocation {
    File(std::path::PathBuf),
    Runs(std::path::PathBuf),
}

/// The extension of a run's file in a [`LogLocation::Runs`] directory.
const RUN_LOG_EXT: &str = "log";

/// The size past which the file `$PIXTUOID_LOG` names rotates at startup.
const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// The most one [`LogLocation::read`] takes of the retained runs, newest first: what
/// the named file holds at most across its two generations, so a week of
/// verbose runs costs a reader no more than it did.
const LOG_READ_BYTES: u64 = 2 * LOG_ROTATE_BYTES;

impl LogLocation {
    /// Where this process's log goes: `$PIXTUOID_LOG`'s file, else a runs
    /// directory in the state, cache or temp directory.
    pub(crate) fn from_env() -> Self {
        if let Some(p) = pixtuoid_core::platform::path_env("PIXTUOID_LOG") {
            return LogLocation::File(p);
        }
        if let Some(state) = crate::install::nonempty_abs_env("XDG_STATE_HOME") {
            return runs_under(&state);
        }
        if let Some(home) = pixtuoid_core::platform::user_home_opt() {
            return runs_under(&home.join(".cache"));
        }
        // No home dir at all: the log must exist somewhere — it is the only runtime
        // diagnostics channel.
        runs_under(&std::env::temp_dir())
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        match self {
            Self::File(p) | Self::Runs(p) => p,
        }
    }

    /// Open this run's sink here, or the path that failed and why: the named
    /// file after its size rotation, or a fresh file of its own in the runs
    /// directory, which, once held, old runs are pruned from.
    ///
    /// # Errors
    ///
    /// The sink's path and the error, if it can't be opened.
    pub(crate) fn open_sink(
        &self,
        now: SystemTime,
    ) -> Result<std::fs::File, (PathBuf, std::io::Error)> {
        let (path, opts) = match self {
            LogLocation::File(path) => {
                rotate_if_large(path);
                (path.clone(), OpenOptions::new())
            }
            LogLocation::Runs(dir) => (
                dir.join(run_file_name(now, std::process::id())),
                live_run_options(),
            ),
        };
        let sink = open_private_append(&path, opts).map_err(|e| (path, e))?;
        if let LogLocation::Runs(dir) = self {
            // Held for the run's life, it marks the file live to [`prune_runs`] on
            // Unix, where removing an open file succeeds.
            #[cfg(unix)]
            let _ = sink.try_lock();
            prune_runs(dir, now);
        }
        Ok(sink)
    }

    /// The log here, its tail within the read budget; of a runs directory,
    /// the tail across its runs, oldest run first.
    pub(crate) fn read(&self) -> (String, Option<String>) {
        match self {
            LogLocation::File(path) => read_log(path),
            LogLocation::Runs(dir) => read_runs(dir, LOG_READ_BYTES),
        }
    }
}

/// The newest `budget` bytes of the runs in `dir`, oldest run first: a run's
/// file is named for its start, so name order is run order.
fn read_runs(dir: &std::path::Path, budget: u64) -> (String, Option<String>) {
    let mut runs: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == RUN_LOG_EXT))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (String::new(), None),
        Err(e) => {
            return (
                String::new(),
                Some(crate::strip_control_chars(&format!(
                    "log unreadable: {} ({e}) — the decode-drift counts are not meaningful",
                    dir.display()
                ))),
            );
        }
    };
    runs.sort();
    let (mut read, mut warning, mut left) = (Vec::new(), None, budget);
    for run in runs.iter().rev() {
        if left == 0 {
            break;
        }
        let len = std::fs::metadata(run).map_or(0, |m| m.len());
        let (run_text, run_warning) = read_log_tail(run, left);
        left = left.saturating_sub(len);
        read.push(run_text);
        warning = warning.or(run_warning);
    }
    let mut text = String::new();
    for run_text in read.iter().rev() {
        text.push_str(run_text);
        // A run cut off mid-line must not join the next run's first line.
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
    }
    (text, warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode_of(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn a_fresh_diagnostic_sink_is_created_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("pixtuoid").join("log");
        drop(open_private_append(&path, OpenOptions::new()).expect("opens"));
        assert_eq!(mode_of(&path), 0o600, "the sink must not inherit the umask");
        assert_eq!(
            mode_of(&dir.path().join("state").join("pixtuoid")),
            0o700,
            "nor the directory it is created in"
        );
    }

    #[test]
    fn log_location_rejects_a_relative_xdg_state_home() {
        // Pins the CALL SITE, not just the primitive: a revert to plain
        // `path_env` here would leak a relative log path.
        temp_env::with_var_unset("PIXTUOID_LOG", || {
            let home =
                pixtuoid_core::platform::user_home_opt().expect("a home dir in the test env");
            let cache = LogLocation::Runs(home.join(".cache").join("pixtuoid").join("logs"));
            for rel in ["", "   ", "rel/state", "~/state"] {
                temp_env::with_var("XDG_STATE_HOME", Some(rel), || {
                    assert_eq!(
                        LogLocation::from_env(),
                        cache,
                        "relative XDG_STATE_HOME {rel:?} must fall back to ~/.cache"
                    );
                });
            }
            // A leading slash is not absolute on Windows, so pick per-platform. The
            // literal's `/` is fine: `PathBuf` equality compares components.
            let abs = if cfg!(windows) { "C:/state" } else { "/state" };
            temp_env::with_var("XDG_STATE_HOME", Some(abs), || {
                assert_eq!(
                    LogLocation::from_env(),
                    LogLocation::Runs(PathBuf::from(format!("{abs}/pixtuoid/logs")))
                );
            });
        });
    }

    #[test]
    fn rotate_if_large_rotates_once_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");

        std::fs::write(&log, b"recent").unwrap();
        rotate_if_large(&log);
        assert!(log.exists(), "under-cap log must not rotate");

        // Sparse via set_len — no real `LOG_ROTATE_BYTES` write.
        let f = std::fs::OpenOptions::new().write(true).open(&log).unwrap();
        f.set_len(LOG_ROTATE_BYTES + 1).unwrap();
        drop(f);
        rotate_if_large(&log);
        assert!(!log.exists(), "over-cap log rotates away");
        assert!(
            dir.path().join("log.old").exists(),
            "one prior generation is kept"
        );
    }

    #[test]
    fn rotate_if_large_appends_old_to_dotted_custom_paths() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let f = std::fs::File::create(&log).unwrap();
        f.set_len(LOG_ROTATE_BYTES + 1).unwrap();
        drop(f);
        rotate_if_large(&log);
        assert!(!log.exists());
        assert!(
            dir.path().join("app.log.old").exists(),
            ".old is appended, not substituted"
        );
    }

    /// A run's file in `dir`, last written `age` before `now`.
    fn aged(dir: &Path, name: &str, now: SystemTime, age: Duration) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "x\n").unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(now - age).unwrap();
        path
    }

    /// A run opens a file of its own, owner-only, named so name order is start
    /// order, and runs past [`RUN_LOG_RETAIN`] go, the new one, a recent one and
    /// a quiet live one staying.
    #[test]
    fn a_run_logs_to_its_own_file_and_tidies_the_runs_before_it() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("pixtuoid").join("logs");
        let now = SystemTime::now();
        std::fs::create_dir_all(&dir).unwrap();
        let aged = |name: &str, age: Duration| aged(&dir, name, now, age);
        let stale = aged(
            &run_file_name(now - RUN_LOG_RETAIN * 2, 1),
            RUN_LOG_RETAIN * 2,
        );
        let recent = aged(
            &run_file_name(now - RUN_LOG_RETAIN / 2, 2),
            RUN_LOG_RETAIN / 2,
        );
        let foreign = aged("notes.txt", RUN_LOG_RETAIN * 2);
        let quiet = aged(
            &run_file_name(now - RUN_LOG_RETAIN * 3, 3),
            RUN_LOG_RETAIN * 2,
        );
        // Its run, still open: the lock is the Unix mark, the share mode Windows'.
        let quiet_run = {
            let mut opts = OpenOptions::new();
            opts.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
                opts.share_mode(FILE_SHARE_READ);
            }
            opts.open(&quiet).unwrap()
        };
        #[cfg(unix)]
        quiet_run.try_lock().unwrap();
        let at = LogLocation::Runs(dir.clone());
        drop(at.open_sink(now).expect("opens"));
        let this_run = dir.join(run_file_name(now, std::process::id()));
        assert!(this_run.exists() && recent.exists() && foreign.exists());
        assert!(!stale.exists(), "a run past the retention goes");
        assert!(quiet.exists(), "a quiet run still running keeps its file");
        drop(quiet_run);
        #[cfg(unix)]
        assert_eq!(mode_of(&this_run), 0o600);

        let (text, warning) = at.read();
        assert_eq!(warning, None);
        let order: Vec<&str> = text.lines().collect();
        assert_eq!(order, ["x", "x"], "runs read oldest first");
    }

    /// The tidy-up test ages its runs off [`RUN_LOG_RETAIN`] itself, so only an
    /// absolute age pins the week its doc cites.
    #[test]
    fn a_run_is_kept_for_a_week() {
        const DAY: Duration = Duration::from_hours(24);
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let aged = |name: &str, age: Duration| aged(dir.path(), name, now, age);
        let six_days = aged(&run_file_name(now - DAY * 6, 1), DAY * 6);
        let eight_days = aged(&run_file_name(now - DAY * 8, 2), DAY * 8);
        prune_runs(dir.path(), now);
        assert!(six_days.exists(), "a run inside the week stays");
        assert!(!eight_days.exists(), "a run past the week goes");
    }

    /// No runs directory yet is a fresh install, read as an empty log.
    #[test]
    fn a_missing_runs_dir_reads_as_an_empty_log() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            read_runs(&root.path().join("logs"), LOG_READ_BYTES),
            (String::new(), None)
        );
    }

    /// A runs directory that can't be listed makes the drift counts
    /// meaningless, so it warns. Unix only, where opendir(3) documents ENOTDIR
    /// for a file.
    #[cfg(unix)]
    #[test]
    fn an_unlistable_runs_dir_warns() {
        let root = tempfile::tempdir().unwrap();
        let not_a_dir = root.path().join("file");
        std::fs::write(&not_a_dir, "x").unwrap();
        let (text, warning) = read_runs(&not_a_dir, LOG_READ_BYTES);
        assert_eq!(text, "");
        assert!(
            warning.is_some_and(|w| w.contains("log unreadable")),
            "an unlistable runs dir must not read as a clean log"
        );
    }

    /// A run that died mid-line still ends where it ended: the next run's
    /// first line starts a line of its own.
    #[test]
    fn a_run_cut_mid_line_does_not_join_the_next() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("1.log"), "a1\na2").unwrap();
        std::fs::write(dir.path().join("2.log"), "b1\n").unwrap();
        assert_eq!(read_runs(dir.path(), u64::MAX).0, "a1\na2\nb1\n");
    }

    #[test]
    fn run_file_names_sort_by_start() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_791_000_000);
        assert_eq!(run_file_name(t, 42), "20261003T040000Z-42.log");
        assert!(run_file_name(t, 99) < run_file_name(t + Duration::from_secs(1), 1));
    }

    /// A read takes the newest runs' bytes up to its budget, each cut run
    /// from its first whole line, and lays them oldest run first.
    #[test]
    fn a_runs_read_takes_the_newest_bytes_within_its_budget() {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in [
            ("1.log", "a1\na2\n"),
            ("2.log", "b1\nb2\n"),
            ("3.log", "c1\n"),
        ] {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        assert_eq!(read_runs(dir.path(), 7), ("b2\nc1\n".to_string(), None));
        assert_eq!(
            read_runs(dir.path(), 6),
            ("b2\nc1\n".to_string(), None),
            "a cut on a line start keeps that line"
        );
        assert_eq!(
            read_runs(dir.path(), 9),
            ("b1\nb2\nc1\n".to_string(), None),
            "a run the budget fits exactly is not cut"
        );
        assert_eq!(
            read_runs(dir.path(), u64::MAX).0,
            "a1\na2\nb1\nb2\nc1\n",
            "every run under a budget that holds them"
        );
    }
}
