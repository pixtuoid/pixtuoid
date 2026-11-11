//! Kernel process-start MARKERS — the identity half of pid-recycle guards.
//!
//! [`pid_start_marker`] returns an opaque per-OS value that is stable for a
//! process's whole life and different for a recycled pid: macOS epoch seconds,
//! Linux clock ticks since boot (read RAW — equality needs no
//! boot-time/ticks-per-sec conversion), Windows the creation `FILETIME`. The
//! units DIFFER per OS: compare two markers from the SAME machine for equality,
//! never across hosts and never as wall-clock. `None` on any failure (EPERM,
//! unsupported OS, and on unix a gone pid) — callers treat that as "no identity
//! check available", never an error. A `Some` is NOT proof of life on Windows.

/// Opaque start marker for `pid`, or `None` when unreadable/unsupported.
pub fn pid_start_marker(pid: i32) -> Option<u64> {
    imp(pid)
}

#[cfg(target_os = "macos")]
fn imp(pid: i32) -> Option<u64> {
    // SAFETY: all-zero bytes are a valid value for this repr(C) plain-old-data
    // struct (integers + byte arrays only).
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is exactly `size` bytes of a repr(C) struct matching
    // the macOS SDK's proc_bsdinfo layout (proc_info.h, ABI-stable since
    // 10.5), so the kernel fills only memory we own. PROC_PIDTBSDINFO returns
    // the full struct or <= 0 on failure.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut std::ffi::c_void,
            size,
        )
    };
    if n != size {
        return None;
    }
    Some(info.pbi_start_tvsec)
}

#[cfg(target_os = "linux")]
fn imp(pid: i32) -> Option<u64> {
    starttime_ticks(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

#[cfg(windows)]
fn imp(pid: i32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let pid = u32::try_from(pid).ok()?;
    // SAFETY: all arguments by-value; the null check screens the handle.
    // LIMITED is the lesser right GetProcessTimes accepts.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if handle.is_null() {
        return None;
    }
    // SAFETY: all-zero bytes are a valid FILETIME (two u32s); the kernel fills
    // exactly the four we own, through a live handle.
    let mut times: [FILETIME; 4] = unsafe { std::mem::zeroed() };
    let (created, rest) = times.split_at_mut(1);
    // SAFETY: `handle` is live until the CloseHandle below; all four out-params
    // are ours.
    let ok = unsafe {
        GetProcessTimes(
            handle,
            &mut created[0],
            &mut rest[0],
            &mut rest[1],
            &mut rest[2],
        )
    };
    // SAFETY: closing the handle OpenProcess just returned, exactly once.
    unsafe { CloseHandle(handle) };
    (ok != 0)
        .then(|| (u64::from(created[0].dwHighDateTime) << 32) | u64::from(created[0].dwLowDateTime))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn imp(_pid: i32) -> Option<u64> {
    None
}

/// When `pid` started, in epoch seconds, for a guard that checks a CLI's own
/// wall-clock stamp. `None` on the same terms as [`pid_start_marker`].
#[cfg(unix)]
pub(crate) fn pid_start_time_secs(pid: i32) -> Option<u64> {
    let start = pid_start_marker(pid)?;
    // Linux's marker is ticks after a boot that `btime` dates by the clock
    // now, so a clock step since the start moves the result; macOS's is
    // `pbi_start_tvsec`, epoch seconds already.
    #[cfg(target_os = "linux")]
    let start = boot_time_secs(&std::fs::read_to_string("/proc/stat").ok()?)?
        + start.checked_div(rustix::param::clock_ticks_per_second())?;
    Some(start)
}

/// `starttime`, field 22 of a `/proc/<pid>/stat`
/// ([proc_pid_stat(5)](https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html)).
#[cfg(any(test, target_os = "linux"))]
fn starttime_ticks(stat: &str) -> Option<u64> {
    // The comm field (2) can contain spaces/parens — so count from after the
    // LAST ')', where starttime is the 20th token.
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(19)?.parse().ok()
}

/// The `btime` line of `/proc/stat`
/// ([proc_stat(5)](https://man7.org/linux/man-pages/man5/proc_stat.5.html)).
#[cfg(any(test, target_os = "linux"))]
fn boot_time_secs(stat: &str) -> Option<u64> {
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .parse()
        .ok()
}

/// Every host runs these: the text is Linux's, the parsing isn't.
#[cfg(test)]
mod procfs_tests {
    use super::*;

    #[test]
    fn boot_time_is_the_btime_line() {
        let stat = "cpu  10132153 290696 3084719 46828483 16683 0 25195 0 0 0\n\
                    cpu0 1393280 32966 572056 13343292 6130 0 17875 0 0 0\n\
                    intr 1462898 0 0 0\n\
                    ctxt 115315\n\
                    btime 769041601\n\
                    processes 86031\n\
                    procs_running 6\n";
        assert_eq!(boot_time_secs(stat), Some(769_041_601));
        assert_eq!(boot_time_secs("cpu  1 2 3\nctxt 115315\n"), None);
    }

    /// The comm `(my (odd) cmd)` holds spaces and both parens: a split at the
    /// first `)` or on whitespace alone lands on a field other than 22.
    #[test]
    fn starttime_is_field_22_past_a_comm_with_spaces_and_parens() {
        let stat = "4242 (my (odd) cmd) S 1 4242 4242 0 -1 4194560 1234 0 0 0 10 5 0 0 \
                    20 0 1 0 98765 12345678 456 18446744073709551615 1 1 0 0 0 0 0";
        assert_eq!(starttime_ticks(stat), Some(98_765));
        assert_eq!(starttime_ticks("4242 (cut"), None);
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux", windows)))]
mod tests {
    use super::*;
    use crate::test_child::sleeper;

    #[test]
    fn marker_is_stable_for_a_live_process() {
        let mut child = sleeper();
        let pid = child.id() as i32;
        let first = pid_start_marker(pid).expect("a live child has a marker");
        let second = pid_start_marker(pid).expect("still alive");
        assert_eq!(first, second, "the marker never changes for one process");
        child.kill().expect("kill the child");
        child.wait().expect("reap the child");
    }

    /// Unix-only: a Windows handle stays valid after termination and `Child`
    /// holds one, so this would assert std's handle lifetime, not the kernel's.
    #[cfg(unix)]
    #[test]
    fn marker_is_none_after_the_process_dies() {
        let mut child = sleeper();
        let pid = child.id() as i32;
        assert!(pid_start_marker(pid).is_some(), "alive before the kill");
        child.kill().expect("kill the child");
        child.wait().expect("reap so the pid leaves the table");
        assert_eq!(pid_start_marker(pid), None);
    }

    /// The teeth: every other Windows assertion survives an `imp` returning a
    /// constant. Windows-only — macOS's epoch-SECONDS marker would collide for
    /// a child spawned inside the same test.
    #[cfg(windows)]
    #[test]
    fn two_live_processes_have_different_markers() {
        let mut child = sleeper();
        let mine = pid_start_marker(std::process::id() as i32).expect("our own marker");
        let theirs = pid_start_marker(child.id() as i32).expect("the child's marker");
        child.kill().expect("kill the child");
        child.wait().expect("reap the child");
        assert_ne!(mine, theirs, "a later-started process reads differently");
    }

    #[test]
    fn garbage_pid_is_none() {
        assert_eq!(pid_start_marker(-1), None);
        assert_eq!(pid_start_marker(i32::MAX), None);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn pid_start_time_is_plausible_epoch_seconds_for_own_process() {
        let start =
            pid_start_time_secs(std::process::id() as i32).expect("own pid must be introspectable");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            start > 1_400_000_000 && start <= now + 1,
            "the start must be epoch seconds in (2014, now], got {start}"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn pid_start_time_secs_reports_a_fresh_child_as_just_started() {
        let mut child = sleeper();
        let started = pid_start_time_secs(child.id() as i32);
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let _ = child.kill();
        let _ = child.wait();
        let started = started.expect("the kernel must report a live child's start time");
        assert!(
            started.abs_diff(now_secs) <= 5,
            "child start time {started} should be within 5s of now {now_secs}"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn pid_start_time_secs_none_for_dead_pid() {
        assert_eq!(pid_start_time_secs(999_999_999), None);
    }
}
