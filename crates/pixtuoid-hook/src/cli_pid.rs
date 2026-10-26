//! The spawning agent CLI's pid, for the hook envelope's `_pid`.
//!
//! A runner that interposes a shell makes the raw parent a corpse-to-be: a
//! `cmd.exe /C` on Windows (#528), and on Unix any wrapper with work left after
//! ours, which therefore cannot exec-replace itself with us — Cursor `eval`s
//! the hook mid-script and then dumps shell state (#896), so stamping it would
//! aim focus-jump at a corpse. (Ending a SESSION on a bad pid is separately
//! guarded — see `HookPidWatch`.)
//!
//! Only the row READ is per-OS, hand-rolled rather than `sysinfo` because this
//! runs on every tool call of every agent inside the shim's send bound.
#![cfg_attr(
    all(not(any(windows, target_os = "macos", target_os = "linux")), not(test)),
    expect(
        dead_code,
        reason = "without a row reader the parent pid is stamped as-is, so the walk is unused"
    )
)]

#[derive(Clone)]
struct ProcRow {
    parent: u32,
    /// The image NAME with no directory (Toolhelp32's `szExeFile`, Unix `comm`).
    exe: String,
}

/// By NAME because a process table carries nothing structural: on Windows the
/// ancestor's command line needs `NtQueryInformationProcess`, and "created
/// within N ms of us" skips the CLI itself at session_start on every OS. Bare STEMS, since
/// the same shell is `bash` under a Unix `comm` and `bash.exe` in a Toolhelp32
/// snapshot (Git-Bash/MSYS2 put the latter on Windows). `busybox` is here
/// because Alpine's `/bin/sh` IS busybox and `comm` reports the real image.
const INTERPOSER_SHELLS: &[&str] = &[
    "cmd",
    "powershell",
    "pwsh",
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "busybox",
];

/// Terminator for a cyclic/corrupt snapshot, not a tuning knob (a real chain
/// interposes one shell, two with a `.cmd` wrapper).
const MAX_HOPS: usize = 8;

fn is_interposer(exe: &str) -> bool {
    let stem = exe
        .rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("exe"))
        .map_or(exe, |(stem, _)| stem);
    INTERPOSER_SHELLS
        .iter()
        .any(|shell| stem.eq_ignore_ascii_case(shell))
}

/// First ancestor of `start` that isn't an interposed shell. A parent of `0` or
/// `1` is the reaper, nobody's agent CLI, so it is no answer. An exited
/// parent is absent from `row_of`; a RECYCLED one is present and
/// indistinguishable.
fn first_cli_ancestor(start: u32, row_of: impl Fn(u32) -> Option<ProcRow>) -> Option<u32> {
    let mut pid = start;
    for _ in 0..MAX_HOPS {
        let parent = row_of(pid)?.parent;
        if parent <= 1 {
            return None;
        }
        if !is_interposer(&row_of(parent)?.exe) {
            return Some(parent);
        }
        pid = parent;
    }
    None
}

/// The walk's own slice of the send bound. `arm_watchdog` is already running and
/// hard-exits at the FULL bound, so an unbudgeted walk costs the ENVELOPE, not
/// just `_pid` — and a process table can stall on either OS (an AV/EDR filter
/// over Toolhelp32; a throttled or contended procfs).
const WALK_BUDGET: std::time::Duration =
    std::time::Duration::from_millis(crate::transport::WRITE_TIMEOUT_MS / 4);

/// The CLI's pid, or `None` where this OS gives no trustworthy answer IN TIME —
/// the walk runs off-thread so a stalled table is abandoned, not waited on.
pub(crate) fn cli_pid() -> Option<u32> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .spawn(move || {
            let _ = tx.send(walk_now());
        })
        .ok()?;
    rx.recv_timeout(WALK_BUDGET).ok().flatten()
}

#[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
fn walk_now() -> Option<u32> {
    first_cli_ancestor(std::process::id(), proc_row)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn walk_now() -> Option<u32> {
    // Safety: getppid takes no args and is infallible.
    u32::try_from(unsafe { libc::getppid() }).ok()
}

#[cfg(target_os = "linux")]
fn proc_row(pid: u32) -> Option<ProcRow> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// `/proc/<pid>/stat`: `comm` is field 2, parenthesized and free to contain both
/// spaces and `)`, so it ends at the LAST one; `ppid` is field 4. Split from the
/// READ, and compiled everywhere, so a hostile row is table-testable off Linux —
/// this is the crate's only parser over bytes we do not control.
#[cfg(any(target_os = "linux", test))]
fn parse_stat(stat: &str) -> Option<ProcRow> {
    let close = stat.rfind(')')?;
    let exe = stat.get(stat.find('(')? + 1..close)?.to_string();
    let parent = stat
        .get(close + 1..)?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(ProcRow { parent, exe })
}

/// `proc_pidinfo` reports how many bytes it wrote, and a dead pid writes none —
/// a short answer is the liveness check, not the return code's sign.
#[cfg(target_os = "macos")]
fn proc_row(pid: u32) -> Option<ProcRow> {
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdshortinfo>()).ok()?;
    // SAFETY: `proc_bsdshortinfo` is a repr(C) struct of integers and byte arrays,
    // for which all-zero is a valid value.
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    // Safety: the flavor matches the out-param type, and the kernel writes at
    // most `size` bytes into the owned `info`.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::c_int::try_from(pid).ok()?,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    Some(ProcRow {
        parent: info.pbsi_ppid,
        exe: comm_name(&info.pbsi_comm),
    })
}

/// `pbsi_comm` is a fixed 16-byte field the kernel leaves UNTERMINATED at
/// exactly that length, so the array bound — not a NUL — ends the name.
#[cfg(target_os = "macos")]
fn comm_name(raw: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = raw
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(windows)]
fn walk_now() -> Option<u32> {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    // Safety: GetCurrentProcessId takes no args and cannot fail.
    let me = unsafe { GetCurrentProcessId() };
    let rows = process_snapshot();
    first_cli_ancestor(me, |pid| rows.get(&pid).cloned())
}

#[cfg(not(any(unix, windows)))]
fn walk_now() -> Option<u32> {
    None
}

/// Every live process, from ONE snapshot so the walk sees a consistent tree.
/// Empty or truncated on failure; the walk reads both as no answer.
///
/// A second Toolhelp32 reader on purpose — `focus::windows::OsProcessTable` is
/// the other, and the shim can't depend on that crate. Fix bugs in BOTH. Its
/// unix twins (`focus::{linux,macos}`) walk the opposite direction, past the
/// setuid-root `login`, so they do not share these readers' permissions.
#[cfg(windows)]
fn process_snapshot() -> std::collections::HashMap<u32, ProcRow> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut rows = std::collections::HashMap::new();
    // Safety: the entry is plain-old-data we own, sized as the API requires,
    // and the snapshot handle is closed on the one exit path below.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return rows;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                rows.insert(
                    entry.th32ProcessID,
                    ProcRow {
                        parent: entry.th32ParentProcessID,
                        exe: exe_name(&entry.szExeFile),
                    },
                );
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    rows
}

/// `szExeFile` is a NUL-padded UTF-16 buffer; decode up to the first NUL.
#[cfg(windows)]
fn exe_name(raw: &[u16]) -> String {
    let units: Vec<u16> = raw.iter().copied().take_while(|&c| c != 0).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake process table as the walk consumes one: pid → (parent, exe).
    fn table(rows: &[(u32, u32, &str)]) -> impl Fn(u32) -> Option<ProcRow> + use<> {
        let rows: std::collections::HashMap<u32, ProcRow> = rows
            .iter()
            .map(|&(pid, parent, exe)| {
                (
                    pid,
                    ProcRow {
                        parent,
                        exe: exe.into(),
                    },
                )
            })
            .collect();
        move |pid| rows.get(&pid).cloned()
    }

    /// The only parser over bytes we do not control. A panic here drops the whole
    /// envelope, and the walk runs off-thread where a panic would be INVISIBLE to
    /// the suite — so the hostile rows are pinned directly rather than through
    /// `proc_row`'s read.
    #[test]
    fn a_hostile_stat_row_is_no_answer_not_a_panic() {
        for row in [
            "",
            ")",
            "(",
            ")(",                             // a '(' AFTER the last ')' — reversed range
            "1 (comm) S",                     // truncated before ppid
            "1 (comm S 2 3",                  // unclosed comm
            "1 comm) S 2 3",                  // unopened comm
            "1 () S 2 3",                     // empty comm
            "1 (a) b) S 2 3",                 // ')' inside comm
            "1 (x) S notanumber 3",           // non-numeric ppid
            "1 (x) S -5 3",                   // negative ppid
            "1 (x) S 99999999999999999999 3", // ppid overflows u32
            "\u{0}\u{0}\u{0}",
            "1 (🦀 crab) S 42 3",
        ] {
            let _ = parse_stat(row); // must not panic
        }
        assert_eq!(
            parse_stat("42 (zsh) S 7 42 42 0").map(|r| (r.parent, r.exe)),
            Some((7, "zsh".to_string())),
            "the well-formed row still parses"
        );
        assert_eq!(
            parse_stat("42 (weird ) name) S 7 42").map(|r| (r.parent, r.exe)),
            Some((7, "weird ) name".to_string())),
            "comm ends at the LAST ')', so a ')' inside the name survives"
        );
        assert!(parse_stat(")(").is_none(), "a reversed range is no answer");
    }

    #[test]
    fn a_direct_parent_that_is_not_a_shell_is_the_cli() {
        let rows = table(&[
            (300, 200, "pixtuoid-hook.exe"),
            (200, 100, "codex.exe"),
            (100, 50, "WindowsTerminal.exe"),
        ]);
        assert_eq!(first_cli_ancestor(300, rows), Some(200));
    }

    /// #896: the wrapper Cursor `eval`s the hook inside cannot exec us.
    #[test]
    fn the_transient_unix_wrapper_shell_is_skipped_for_the_cli_above_it() {
        for shell in ["zsh", "bash", "sh", "dash", "ksh", "fish"] {
            let rows = table(&[
                (300, 250, "pixtuoid-hook"),
                (250, 200, shell),
                (200, 100, "cursor-agent"),
                (100, 50, "zsh"),
            ]);
            assert_eq!(
                first_cli_ancestor(300, rows),
                Some(200),
                "{shell} wrapper must resolve to the CLI, not the wrapper"
            );
        }
    }

    /// Git-Bash/MSYS2 put a `.exe`-suffixed unix shell on Windows, and Alpine's
    /// `/bin/sh` reports its real image, `busybox`.
    #[test]
    fn suffixed_and_busybox_spellings_are_interposers_too() {
        for exe in ["bash.exe", "ZSH.EXE", "sh.exe", "busybox", "cmd.exe", "CMD"] {
            assert!(is_interposer(exe), "{exe} must read as an interposer");
        }
        for exe in [
            "cursor-agent",
            "node.exe",
            "claude",
            "codex.exe",
            "fisherman",
        ] {
            assert!(!is_interposer(exe), "{exe} is a CLI, not a shell");
        }
    }

    /// A `.cmd` wrapper stacks a second shell; casing is the filesystem's.
    #[test]
    fn every_interposer_spelling_and_a_stacked_pair_are_skipped() {
        let rows = table(&[
            (300, 260, "pixtuoid-hook.exe"),
            (260, 250, "CMD.EXE"),
            (250, 200, "PowerShell.exe"),
            (200, 100, "node.exe"),
            (100, 50, "WindowsTerminal.exe"),
        ]);
        assert_eq!(
            first_cli_ancestor(300, rows),
            Some(200),
            "node.exe is the CLI — an interpreter name must not read as a shell"
        );
    }

    /// An exited parent leaves no row — stamp nothing, not someone else's pid.
    #[test]
    fn a_parent_missing_from_the_snapshot_is_no_answer() {
        assert_eq!(
            first_cli_ancestor(300, table(&[(300, 250, "pixtuoid-hook.exe")])),
            None
        );
        assert_eq!(
            first_cli_ancestor(999, table(&[(300, 250, "pixtuoid-hook.exe")])),
            None,
            "our own row missing is no answer either"
        );
    }

    /// Reparented to the reaper: `1` (or Windows' `0`) is nobody's agent CLI.
    #[test]
    fn a_reaper_parent_is_no_answer() {
        for reaper in [0, 1] {
            assert_eq!(
                first_cli_ancestor(
                    300,
                    table(&[(300, reaper, "pixtuoid-hook"), (reaper, 0, "init")])
                ),
                None,
                "a parent of {reaper} must not be stamped as the CLI"
            );
        }
    }

    #[test]
    fn a_chain_of_nothing_but_shells_runs_out_rather_than_looping() {
        let all_shells: Vec<(u32, u32, &str)> = (2..MAX_HOPS as u32 + 4)
            .map(|i| (i, i + 1, "cmd.exe"))
            .collect();
        assert_eq!(first_cli_ancestor(2, table(&all_shells)), None);

        let cycle = [(300, 250, "cmd.exe"), (250, 300, "cmd.exe")];
        assert_eq!(first_cli_ancestor(300, table(&cycle)), None);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_live_row_reader_agrees_with_getppid() {
        let me = proc_row(std::process::id()).expect("our own row is readable");
        // Safety: getppid takes no args and is infallible.
        let ppid = u32::try_from(unsafe { libc::getppid() }).expect("ppid fits u32");
        assert_eq!(me.parent, ppid, "the reader's ppid must be the kernel's");
        assert!(!me.exe.is_empty(), "a live process has a name");

        let mut dead = std::process::Command::new("true")
            .spawn()
            .expect("spawn a child to reap");
        dead.wait().expect("reap it");
        assert!(
            proc_row(dead.id()).is_none(),
            "a reaped pid must read as no row, not a stale one"
        );
    }

    /// The real resolver against the real process table.
    #[test]
    fn the_live_resolver_names_a_real_ancestor() {
        let pid = cli_pid().expect("this process has a live ancestor");
        assert_ne!(pid, std::process::id(), "never our own pid");
        assert!(pid > 1, "never the reaper");
    }
}
