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
    // `/proc/<pid>/stat` field 22 is starttime, but the comm field (2) can
    // contain spaces/parens — so count from after the LAST ')', where starttime
    // is the 20th token.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(19)?.parse().ok()
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
}
