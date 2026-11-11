//! Kernel process-start times — the identity half of pid-recycle guards.
//!
//! [`pid_start_marker`] returns an opaque per-OS value that is stable for a
//! process's whole life and different for a recycled pid: macOS epoch seconds,
//! Linux clock ticks since boot (read RAW — equality needs no
//! boot-time/ticks-per-sec conversion), Windows the creation `FILETIME`. The
//! units DIFFER per OS: compare two markers from the SAME machine for equality,
//! never across hosts and never as wall-clock outside `pid_start`, which knows
//! each OS's units. `None` on any failure (EPERM,
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

/// A process's start: its [`pid_start_marker`], and the same instant in epoch
/// seconds for a guard that checks a CLI's own wall-clock stamp.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Start {
    pub(crate) marker: u64,
    pub(crate) secs: u64,
}

/// `pid`'s [`Start`], `None` on the same terms as [`pid_start_marker`].
#[cfg(unix)]
pub(crate) fn pid_start(pid: i32) -> Option<Start> {
    let marker = pid_start_marker(pid)?;
    #[cfg(target_os = "linux")]
    let secs = boot_secs()? + marker.checked_div(rustix::param::clock_ticks_per_second())?;
    #[cfg(not(target_os = "linux"))]
    let secs = marker;
    Some(Start { marker, secs })
}

/// The boot in epoch seconds by the clock now. Realtime − Boottime is the
/// `offs_real − offs_boot` that `getboottime64` takes
/// (kernel/time/timekeeping.c), so a realtime step moves it, while the
/// marker, ticks after boot, stays.
#[cfg(target_os = "linux")]
fn boot_secs() -> Option<u64> {
    use rustix::time::{ClockId, clock_gettime};
    let read = |clock| {
        let now = clock_gettime(clock);
        Some(std::time::Duration::new(
            now.tv_sec.try_into().ok()?,
            now.tv_nsec.try_into().ok()?,
        ))
    };
    boot_from(read(ClockId::Realtime)?, read(ClockId::Boottime)?)
}

/// The boot from the two clocks read together.
#[cfg(any(test, target_os = "linux"))]
fn boot_from(realtime: std::time::Duration, boottime: std::time::Duration) -> Option<u64> {
    // Whole clocks, subtracted before the floor: each clock floored first
    // would flip the result by a second as their nanoseconds cross.
    Some(realtime.checked_sub(boottime)?.as_secs())
}

/// The pids a registry probe has judged by their epoch start, each known by
/// its marker after: a realtime step moves Linux's epoch start, never the
/// marker, so a session judged once keeps its vouch across one.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct StartPins(std::sync::Mutex<Pinned>);

/// `(pid, claim) → marker`, so two entries naming one pid are judged apart.
#[cfg(unix)]
type Pinned = std::collections::BTreeMap<(i32, u64), u64>;

#[cfg(unix)]
impl StartPins {
    pub(crate) const fn new() -> Self {
        Self(std::sync::Mutex::new(Pinned::new()))
    }

    /// Whether the process at `pid` is the one whose start `claim` names:
    /// judged by `fits` over its epoch start until that passes, then by its
    /// marker alone. With no `start`, kill(0) alone decides.
    pub(crate) fn admits(
        &self,
        pid: i32,
        claim: u64,
        start: Option<Start>,
        fits: impl FnOnce(u64) -> bool,
    ) -> bool {
        let Some(start) = start else { return true };
        let mut pins = self.lock();
        if let Some(&marker) = pins.get(&(pid, claim)) {
            return marker == start.marker;
        }
        let fits = fits(start.secs);
        if fits {
            pins.insert((pid, claim), start.marker);
        }
        fits
    }

    /// Forget each pin whose process `start` no longer finds at its pid, dead
    /// or recycled: a pin lives as long as its process, never as one scan's
    /// vouch, since a scan skips a live entry it reads half-written and the
    /// #252 tiebreak's loser.
    pub(crate) fn retain(&self, start: impl Fn(i32) -> Option<Start>) {
        self.lock()
            .retain(|&(pid, _), &mut marker| start(pid).is_some_and(|now| now.marker == marker));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Pinned> {
        // A panic mid-update leaves at most one pin unwritten, which only sends
        // its entry back to the wall-clock check.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
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

/// Fixtures stand in for the kernel, so these run off Linux too.
#[cfg(test)]
mod any_host_tests {
    use super::*;

    /// The comm `(my (odd) cmd)` holds spaces and both parens: a split at the
    /// first `)` or on whitespace alone lands on a field other than 22.
    #[test]
    fn starttime_is_field_22_past_a_comm_with_spaces_and_parens() {
        let stat = "4242 (my (odd) cmd) S 1 4242 4242 0 -1 4194560 1234 0 0 0 10 5 0 0 \
                    20 0 1 0 98765 12345678 456 18446744073709551615 1 1 0 0 0 0 0";
        assert_eq!(starttime_ticks(stat), Some(98_765));
        assert_eq!(starttime_ticks("4242 (cut"), None);
    }

    /// Realtime's 200 ms sit under boottime's 900 ms, so flooring each clock
    /// first reads the boot a second late.
    #[test]
    fn the_boot_floors_the_clocks_difference() {
        use std::time::Duration;
        let at = |secs, millis| Duration::from_secs(secs) + Duration::from_millis(millis);
        assert_eq!(
            boot_from(at(1_700_000_000, 200), at(100, 900)),
            Some(1_699_999_899)
        );
        assert_eq!(
            boot_from(at(100, 0), at(200, 0)),
            None,
            "boottime past realtime"
        );
    }

    /// A clock step moves the epoch start the kernel's marker doesn't: once a
    /// claim fits, the marker alone holds it, and a new marker is a new process.
    #[cfg(unix)]
    #[test]
    fn a_claim_judged_once_is_held_by_its_marker() {
        let pins = StartPins::new();
        let (claim, later) = (1_000, 1_060);
        let fits = |claim: u64| move |secs: u64| secs == claim;
        let at = |marker, secs| Some(Start { marker, secs });
        let step = 3_600;
        assert!(
            !pins.admits(7, claim, at(1, claim + 1), fits(claim)),
            "a misfit, by its secs"
        );
        assert!(
            !pins.admits(7, claim, at(1, claim + 1), fits(claim)),
            "a refusal pins nothing"
        );
        assert!(
            pins.admits(7, claim, at(1, claim), fits(claim)),
            "a fit pins the marker"
        );
        assert!(
            !pins.admits(7, later, at(1, claim + step), fits(later)),
            "a new claim on a pinned pid is judged afresh"
        );
        assert!(
            pins.admits(7, later, at(1, later), fits(later)),
            "one pid, a second claim"
        );
        assert!(
            pins.admits(7, claim, at(1, claim + step), fits(claim)),
            "a step moves secs"
        );
        assert!(
            pins.admits(7, later, at(1, later + step), fits(later)),
            "each claim its own pin"
        );
        assert!(
            !pins.admits(7, claim, at(2, claim), fits(claim)),
            "a new marker, a recycled pid"
        );
        assert!(
            pins.admits(7, claim, None, |_| false),
            "no start leaves kill(0) to decide"
        );
    }

    /// A pin lives as long as its process: the prune keeps it while the pid
    /// still runs the marker it pinned, and drops it once the pid is dead or
    /// recycled.
    #[cfg(unix)]
    #[test]
    fn a_pin_lives_as_long_as_its_process() {
        let pins = StartPins::new();
        let at = |marker, secs| Some(Start { marker, secs });
        for pid in [7, 8, 9] {
            assert!(pins.admits(pid, 1_000, at(1, 1_000), |_| true));
        }
        pins.retain(|pid| match pid {
            7 => at(1, 4_600),
            8 => None,
            _ => at(2, 1_000),
        });
        let pinned: Vec<i32> = pins.lock().keys().map(|&(pid, _)| pid).collect();
        assert_eq!(
            pinned,
            [7],
            "the live pid kept, the dead and the recycled dropped"
        );
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
        let start = pid_start(std::process::id() as i32)
            .expect("own pid must be introspectable")
            .secs;
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
    fn pid_start_reports_a_fresh_child_as_just_started() {
        let mut child = sleeper();
        let started = pid_start(child.id() as i32).map(|start| start.secs);
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
    fn pid_start_none_for_dead_pid() {
        assert_eq!(pid_start(999_999_999), None);
    }
}
