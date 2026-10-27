//! The CC `sessions/<pid>.json` registry probe behind
//! `claude_code::live_cc_session_ids`.

use std::path::{Path, PathBuf};

use crate::source::jsonl::ProbeSnapshot;

/// CC's first-party live-process registry: `<claude_home>/sessions/<pid>.json`,
/// one tiny undocumented JSON file per running CC process (`{pid, sessionId,
/// cwd, status, startedAt, procStart, ...}`). Returns the session UUIDs (+
/// owning pid each) of entries whose pid is still ALIVE — a registry file can
/// outlive a crashed CC, so each entry is verified with kill(pid, 0).
///
/// Failure is explicit (#223): an UNREADABLE-or-MISSING registry dir is
/// `None` — its absence is ambiguous (older CC without the registry, a
/// permissions problem), so the watcher changes nothing. An EMPTY but readable
/// dir is `Some(empty)`: a healthy "no CC running" observation the negative
/// vouch may act on.
///
/// PID-reuse guard (#220): kill(0) only proves SOME process owns the pid. When
/// the kernel can report the process start time, it must agree with the
/// entry's `startedAt` within `PID_START_TOLERANCE_SECS` or the pid was
/// recycled and the entry is skipped; a kernel that can't falls back to
/// pid-alive-only.
pub fn live_cc_session_ids(sessions_dir: &Path) -> Option<ProbeSnapshot> {
    #[cfg(unix)]
    {
        // session_id → winning entry. The fold (not direct inserts) exists for
        // the duplicate-id case (#252): binding id→pid by unspecified read_dir
        // order flaps across refreshes, churning the exit-watch rebind, and the
        // losing pid dying first emits a spurious SessionEnd for the live one.
        let mut winners: std::collections::HashMap<String, RegistryEntry> =
            std::collections::HashMap::new();
        let Ok(entries) = std::fs::read_dir(sessions_dir) else {
            tracing::debug!(
                path = ?sessions_dir,
                "CC session registry unreadable or missing; probe pass failed"
            );
            return None;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // Regular files only, decided WITHOUT following symlinks (so a
            // symlink-to-FIFO is rejected too): reading a writer-less FIFO
            // named `x.json` blocks forever, silently killing the watcher task.
            if !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            // An unreadable file is usually a CC exiting and removing its own
            // entry mid-scan — not format drift; skip silently.
            let Ok(bytes) = crate::source::read_bounded_bytes(&path, MAX_REGISTRY_ENTRY_BYTES)
            else {
                continue;
            };
            let reg = match parse_registry_entry(&bytes) {
                RegistryParse::Entry(reg) => reg,
                RegistryParse::Skip => continue,
                RegistryParse::ShapeDrift(key) => {
                    // Warn ONCE per process run, never per scan pass:
                    // the registry is the one undocumented upstream surface
                    // with no fetchable text to drift-diff (#247), so a silent
                    // key rename would degrade the probe to mtime-only gating
                    // with zero signal.
                    static SHAPE_DRIFT_WARNED: std::sync::Once = std::sync::Once::new();
                    SHAPE_DRIFT_WARNED.call_once(|| {
                        crate::source::drift::shape_drift(
                            crate::source::claude_code::SOURCE_NAME,
                            &format!(
                                "sessions-registry entry {} parses as JSON but `{key}` is \
                                 missing or mistyped — the registry shape changed upstream; \
                                 mid-attach liveness degraded to mtime gating",
                                path.display()
                            ),
                        );
                    });
                    continue;
                }
            };
            if !pid_alive(reg.pid) {
                continue;
            }
            if let Some(actual_secs) = pid_start_time_secs(reg.pid)
                && (reg.started_at_ms / 1000).abs_diff(actual_secs) > PID_START_TOLERANCE_SECS
            {
                tracing::debug!(
                    pid = reg.pid,
                    claimed_secs = reg.started_at_ms / 1000,
                    actual_secs,
                    "pid recycled — registry startedAt does not match process start; skipping"
                );
                continue;
            }
            match winners.entry(reg.session_id.clone()) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(reg);
                }
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    // Real registries are one-file-per-pid with unique ids — a
                    // duplicate is upstream junk worth ONE breadcrumb per
                    // process run, not a per-scan log storm.
                    static DUPLICATE_ID_WARNED: std::sync::Once = std::sync::Once::new();
                    DUPLICATE_ID_WARNED.call_once(|| {
                        tracing::warn!(
                            session_id = ?reg.session_id,
                            pids = ?(slot.get().pid, reg.pid),
                            "two live CC registry entries claim the same sessionId — \
                             keeping a deterministic winner"
                        );
                    });
                    if prefer_candidate(slot.get(), &reg) {
                        slot.insert(reg);
                    }
                }
            }
        }
        let mut live = ProbeSnapshot::default();
        for (session_id, entry) in winners {
            live.pid_of.insert(session_id, entry.pid);
        }
        Some(live)
    }
    #[cfg(not(unix))]
    {
        // Windows has no kill(0); pid liveness needs OpenProcess + exit-code
        // semantics unvalidated against CC-on-Windows. Explicit failure keeps
        // today's pure-mtime admission and never arms the negative vouch.
        let _ = sessions_dir;
        None
    }
}

/// Deterministic winner between two LIVE entries claiming one sessionId
/// (#252): read_dir order is unspecified and the binding must not flap.
/// Newest `startedAt` wins, and a tie falls to the larger pid.
#[cfg(unix)]
fn prefer_candidate(incumbent: &RegistryEntry, candidate: &RegistryEntry) -> bool {
    match candidate.started_at_ms.cmp(&incumbent.started_at_ms) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => candidate.pid > incumbent.pid,
    }
}

/// Bounds ONE registry entry — real entries are <1 KiB, and the bound keeps
/// junk in the registry dir from ballooning a per-scan read (truncated bytes
/// just fail the JSON parse and are skipped silently).
#[cfg(unix)]
const MAX_REGISTRY_ENTRY_BYTES: u64 = 64 * 1024;

/// One parsed registry entry — the fields the liveness join needs.
#[cfg(unix)]
#[derive(Debug)]
struct RegistryEntry {
    pid: i32,
    session_id: String,
    /// `startedAt` — ms-epoch CC stamps at startup.
    started_at_ms: u64,
}

/// One registry-file parse outcome — the seam the #247 drift warn keys on.
/// Routing rule for future keys: a REQUIRED consumed key goes to
/// `ShapeDrift("<key>")`; an additive key stays `Option` on the entry and its
/// absence is never a parse fail nor drift.
#[cfg(unix)]
#[derive(Debug)]
enum RegistryParse {
    Entry(RegistryEntry),
    /// Skip silently: not JSON at all (a half-written file mid-write) or a
    /// value-level corruption (pid <= 0, empty sessionId) — the keys are
    /// shaped right, so it is not format drift. A wholesale format replacement
    /// lands here too, deliberately: warning on it would let one startup
    /// transient consume the once-per-run breadcrumb a key rename deserves.
    Skip,
    /// Parses as JSON but a consumed key is missing or mistyped — the
    /// undocumented upstream shape changed (#247); the consumer warns once.
    ShapeDrift(&'static str),
}

/// Extract `{pid, sessionId, startedAt}` from one registry file.
/// `serde_json::Value` on purpose — the format is undocumented, so we read
/// only the fields the join needs and tolerate everything else changing.
#[cfg(unix)]
fn parse_registry_entry(bytes: &[u8]) -> RegistryParse {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return RegistryParse::Skip;
    };
    let Some(pid) = v.get("pid").and_then(|p| p.as_i64()) else {
        return RegistryParse::ShapeDrift("pid");
    };
    // The strictly-positive narrowing every JSON pid ingress shares (rationale
    // on `checked_pid`): a corrupt entry must not probe our own group as alive.
    let Some(pid) = crate::source::decoder::checked_pid(pid) else {
        return RegistryParse::Skip;
    };
    let Some(session_id) = v.get("sessionId").and_then(|s| s.as_str()) else {
        return RegistryParse::ShapeDrift("sessionId");
    };
    if session_id.is_empty() {
        return RegistryParse::Skip;
    }
    let Some(started_at_ms) = v.get("startedAt").and_then(|s| s.as_u64()) else {
        return RegistryParse::ShapeDrift("startedAt");
    };
    RegistryParse::Entry(RegistryEntry {
        pid,
        session_id: session_id.to_string(),
        started_at_ms,
    })
}

/// Tolerance for the `startedAt` ↔ kernel-start-time identity check. CC writes
/// `startedAt` ≈ 1.2s after `pbi_start_tvsec` (Node boot + module load before
/// the stamp), so 10s is generous margin while far below any plausible
/// pid-recycling interval.
#[cfg(unix)]
pub(crate) const PID_START_TOLERANCE_SECS: u64 = 10;

/// Kernel-reported process start time in epoch seconds — the identity half of
/// the PID-reuse guard. macOS: `proc_pidinfo(PROC_PIDTBSDINFO)` →
/// `pbi_start_tvsec`. Linux's `/proc/<pid>/stat` field 22 is clock ticks since
/// BOOT, so the identity check is macOS-only for now and Linux returns `None`
/// (pid-alive-only). `None` on failure — never an error: the check is additive.
#[cfg(target_os = "macos")]
pub(crate) fn pid_start_time_secs(pid: i32) -> Option<u64> {
    super::proc_start::pid_start_marker(pid)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn pid_start_time_secs(_pid: i32) -> Option<u64> {
    // The Linux MARKER is ticks-since-boot — NOT epoch — so this wall-clock
    // comparison must not ride it; epoch conversion stays deferred (#220).
    None
}

/// `kill(pid, 0)` liveness: Ok = alive and signalable; EPERM = alive but owned
/// by another user; anything else = no such process.
#[cfg(unix)]
pub(crate) fn pid_alive(pid: i32) -> bool {
    let Some(pid) = rustix::process::Pid::from_raw(pid) else {
        return false;
    };
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(e) => e == rustix::io::Errno::PERM,
    }
}

/// Liveness by a handle that a zero-timeout wait finds unsignalled; a pid no
/// handle opens for reads as gone.
#[cfg(all(windows, feature = "harness"))]
pub(crate) fn pid_alive(pid: i32) -> bool {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{FALSE, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    let Ok(id) = u32::try_from(pid) else {
        return false;
    };
    // SAFETY: by-value arguments; the null check screens the handle.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, FALSE, id) };
    if handle.is_null() {
        return false;
    }
    // SAFETY: a fresh handle nothing else owns.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    // SAFETY: the handle is open with SYNCHRONIZE, and a zero timeout only polls.
    unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) == WAIT_TIMEOUT }
}

/// The sessions registry is a SIBLING of the projects root. Derived only when
/// the parent layout matches (the root's file_name is literally `projects`) —
/// a root pointed elsewhere (a test's temp dir) has a parent that could hold an
/// unrelated `sessions/`, so it gets no probe and keeps the pure-mtime gate.
pub(crate) fn cc_sessions_dir(projects_root: &Path) -> Option<PathBuf> {
    if projects_root.file_name().and_then(|n| n.to_str()) != Some("projects") {
        return None;
    }
    projects_root
        .parent()
        // A bare relative `projects` has an EMPTY parent — not a claude home.
        .filter(|home| !home.as_os_str().is_empty())
        .map(|home| home.join("sessions"))
}

#[cfg(test)]
mod liveness_tests {
    use super::*;
    #[cfg(unix)]
    use std::collections::HashSet;

    // Gated with its only callers: on Windows this would be dead code.
    #[cfg(unix)]
    fn write_entry(dir: &Path, name: &str, pid: i64, session_id: &str) {
        // The pid's real start, so the PID-reuse guard vouches for a live one.
        let started = i32::try_from(pid)
            .ok()
            .and_then(pid_start_time_secs)
            .unwrap_or(0);
        write_entry_started(dir, name, pid, session_id, started * 1000);
    }

    #[cfg(unix)]
    fn write_entry_started(dir: &Path, name: &str, pid: i64, session_id: &str, started_ms: u64) {
        std::fs::write(
            dir.join(name),
            serde_json::json!({
                "pid": pid, "sessionId": session_id, "startedAt": started_ms, "status": "idle"
            })
            .to_string(),
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn nonpositive_registry_pid_is_dropped_via_checked_pid() {
        for bad in [0i64, -1, -12345] {
            let bytes =
                serde_json::json!({ "pid": bad, "sessionId": "s", "status": "idle" }).to_string();
            assert!(
                matches!(parse_registry_entry(bytes.as_bytes()), RegistryParse::Skip),
                "pid {bad} must be dropped"
            );
        }
        let ok =
            serde_json::json!({ "pid": 4321, "sessionId": "s", "startedAt": 1, "status": "idle" })
                .to_string();
        assert!(matches!(
            parse_registry_entry(ok.as_bytes()),
            RegistryParse::Entry(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn keeps_entry_whose_pid_is_alive_and_binds_its_pid() {
        let dir = tempfile::tempdir().unwrap();
        write_entry(
            dir.path(),
            "self.json",
            i64::from(std::process::id()),
            "alive-session",
        );
        let live = live_cc_session_ids(dir.path()).expect("readable dir is a healthy probe");
        assert!(
            live.contains("alive-session"),
            "an entry with a live pid must be kept, got {live:?}"
        );
        assert_eq!(
            live.pid_of.get("alive-session"),
            Some(&(std::process::id() as i32)),
            "the vouched id must bind to the registry entry's pid"
        );
    }

    #[cfg(unix)]
    #[test]
    fn drops_entry_whose_pid_is_dead() {
        // Spawn-and-reap a real child: its pid is dead once wait() returns.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = i64::from(child.id());
        child.wait().unwrap();

        let dir = tempfile::tempdir().unwrap();
        write_entry(dir.path(), "dead.json", dead_pid, "dead-session");
        write_entry(
            dir.path(),
            "alive.json",
            i64::from(std::process::id()),
            "alive-session",
        );
        let live = live_cc_session_ids(dir.path()).expect("readable dir is a healthy probe");
        assert!(
            !live.contains("dead-session"),
            "a crashed CC's leftover registry file must not count as live, got {live:?}"
        );
        assert!(
            live.contains("alive-session"),
            "the live sibling must survive the dead entry, got {live:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn malformed_and_incomplete_entries_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let own_pid = i64::from(std::process::id());
        std::fs::write(dir.path().join("garbage.json"), "not json {{{").unwrap();
        std::fs::write(
            dir.path().join("nosid.json"),
            serde_json::json!({ "pid": own_pid }).to_string(),
        )
        .unwrap();
        write_entry(dir.path(), "pid0.json", 0, "group-session");
        std::fs::write(
            dir.path().join("strpid.json"),
            serde_json::json!({ "pid": own_pid.to_string(), "sessionId": "str-pid" }).to_string(),
        )
        .unwrap();
        write_entry(dir.path(), "notes.txt", own_pid, "txt-session");
        write_entry(dir.path(), "valid.json", own_pid, "valid-session");

        let live = live_cc_session_ids(dir.path()).expect("readable dir is a healthy probe");
        assert_eq!(
            live.ids().cloned().collect::<HashSet<_>>(),
            HashSet::from(["valid-session".to_string()]),
            "only the well-formed live entry may survive"
        );
    }

    #[cfg(unix)]
    #[test]
    fn oversized_junk_json_entry_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("huge.json"), vec![b'x'; 256 * 1024]).unwrap();
        write_entry(
            dir.path(),
            "valid.json",
            i64::from(std::process::id()),
            "valid-session",
        );
        let live = live_cc_session_ids(dir.path()).expect("readable dir is a healthy probe");
        assert_eq!(
            live.ids().cloned().collect::<HashSet<_>>(),
            HashSet::from(["valid-session".to_string()]),
            "an oversized junk entry must be ignored, not break the probe"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fifo_named_json_does_not_hang_the_probe() {
        // Reading a writer-less FIFO blocks forever, so the file_type() filter
        // must reject it before any read.
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo.json");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: mkfifo only reads the NUL-terminated path; 0o600 owner-only.
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
        write_entry(
            dir.path(),
            "valid.json",
            i64::from(std::process::id()),
            "valid-session",
        );
        let live = live_cc_session_ids(dir.path()).expect("readable dir is a healthy probe");
        assert_eq!(
            live.ids().cloned().collect::<HashSet<_>>(),
            HashSet::from(["valid-session".to_string()]),
            "a FIFO entry must be skipped; the live sibling must survive"
        );
    }

    #[test]
    fn missing_dir_is_probe_failure_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(live_cc_session_ids(&missing).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn prefer_candidate_orders_by_started_at_then_pid() {
        let e = |started_at_ms: u64, pid: i32| RegistryEntry {
            pid,
            session_id: "s".to_string(),
            started_at_ms,
        };
        assert!(prefer_candidate(&e(1_000, 99), &e(2_000, 1)));
        assert!(!prefer_candidate(&e(2_000, 1), &e(1_000, 99)));
        assert!(prefer_candidate(&e(1_000, 5), &e(1_000, 9)));
        assert!(!prefer_candidate(&e(1_000, 9), &e(1_000, 5)));
        assert!(!prefer_candidate(&e(1_000, 7), &e(1_000, 7)));
    }

    #[cfg(target_os = "macos")]
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
            "pbi_start_tvsec must be epoch seconds in (2014, now], got {start}"
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn pid_start_time_is_deliberately_unavailable_off_macos() {
        assert_eq!(pid_start_time_secs(std::process::id() as i32), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn started_at_exactly_at_tolerance_still_vouches() {
        let own_pid = std::process::id() as i32;
        let actual = pid_start_time_secs(own_pid).expect("own pid must be introspectable");
        let dir = tempfile::tempdir().unwrap();
        let entry = |name: &str, claimed_secs: u64, sid: &str| {
            std::fs::write(
                dir.path().join(name),
                serde_json::json!({
                    "pid": own_pid,
                    "sessionId": sid,
                    "startedAt": claimed_secs * 1000,
                })
                .to_string(),
            )
            .unwrap();
        };
        entry(
            "boundary.json",
            actual + PID_START_TOLERANCE_SECS,
            "boundary-session",
        );
        entry(
            "recycled.json",
            actual + PID_START_TOLERANCE_SECS + 1,
            "recycled-session",
        );
        let live = live_cc_session_ids(dir.path()).expect("readable dir is a healthy probe");
        assert!(
            live.contains("boundary-session"),
            "a claim exactly at the tolerance must still vouch, got {live:?}"
        );
        assert!(
            !live.contains("recycled-session"),
            "one second past the tolerance is a recycled pid, got {live:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn multi_kib_entry_still_parses_within_the_read_cap() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("padded.json"),
            serde_json::json!({
                "pid": i64::from(std::process::id()),
                "sessionId": "padded-session",
                "startedAt": pid_start_time_secs(std::process::id() as i32).unwrap_or(0) * 1000,
                "future_upstream_key": "y".repeat(8 * 1024),
            })
            .to_string(),
        )
        .unwrap();
        let live = live_cc_session_ids(dir.path()).expect("readable dir is a healthy probe");
        assert!(
            live.contains("padded-session"),
            "an 8 KiB entry sits well under the 64 KiB cap, got {live:?}"
        );
    }

    #[test]
    fn unreadable_dir_is_probe_failure_none() {
        // A FILE where the dir should be makes read_dir fail deterministically.
        let dir = tempfile::tempdir().unwrap();
        let file_not_dir = dir.path().join("sessions");
        std::fs::write(&file_not_dir, b"not a dir").unwrap();
        assert!(live_cc_session_ids(&file_not_dir).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn empty_readable_dir_is_some_empty() {
        let dir = tempfile::tempdir().unwrap();
        let snap = live_cc_session_ids(dir.path()).expect("empty readable dir is healthy");
        assert!(snap.is_empty());
        assert!(snap.pid_of.is_empty());
    }

    #[cfg(unix)]
    fn expect_entry(bytes: &[u8]) -> RegistryEntry {
        match parse_registry_entry(bytes) {
            RegistryParse::Entry(e) => e,
            other => panic!("expected a parsed entry, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn parse_registry_entry_requires_started_at() {
        let with = serde_json::json!({
            "pid": 64924, "sessionId": "s", "startedAt": 1_781_109_422_174_u64
        })
        .to_string();
        let entry = expect_entry(with.as_bytes());
        assert_eq!(entry.started_at_ms, 1_781_109_422_174);

        for bad in [
            serde_json::json!({ "pid": 64924, "sessionId": "s" }),
            serde_json::json!({ "pid": 64924, "sessionId": "s", "startedAt": "soon" }),
        ] {
            assert!(
                matches!(
                    parse_registry_entry(bad.to_string().as_bytes()),
                    RegistryParse::ShapeDrift("startedAt")
                ),
                "{bad}"
            );
        }
    }

    /// Pin of the CURRENT live `<claude_home>/sessions/<pid>.json` shape —
    /// synthesized values, but the same keys as a real entry. The format is
    /// undocumented with no upstream text to diff, so this fixture is the
    /// shape's only regression detector.
    #[cfg(unix)]
    fn live_shape_entry(pid: i64, session_id: &str) -> String {
        serde_json::json!({
            "pid": pid,
            "sessionId": session_id,
            "startedAt": 1_781_109_422_174_u64,
            "procStart": 1_781_109_420_000_u64,
            "cwd": "/Users/someone/project",
            "version": "2.1.0",
            "peerProtocol": 1,
            "kind": "repl",
            "entrypoint": "cli",
            "status": "idle",
            "updatedAt": 1_781_109_500_000_u64,
            "name": "project",
            "bridgeSessionId": "00000000-0000-7000-8000-00000000bb1d"
        })
        .to_string()
    }

    #[cfg(unix)]
    #[test]
    fn registry_shape_pin_parses_the_current_live_format() {
        let entry = expect_entry(live_shape_entry(64924, "pinned-session").as_bytes());
        assert_eq!(entry.pid, 64924);
        assert_eq!(entry.session_id, "pinned-session");
        assert_eq!(entry.started_at_ms, 1_781_109_422_174);
    }

    #[cfg(unix)]
    #[test]
    fn renamed_or_mistyped_required_key_is_shape_drift() {
        let renamed_sid = serde_json::json!({ "pid": 64924, "session_id": "s" }).to_string();
        assert!(matches!(
            parse_registry_entry(renamed_sid.as_bytes()),
            RegistryParse::ShapeDrift("sessionId")
        ));
        let nonstring_sid = serde_json::json!({ "pid": 64924, "sessionId": 7 }).to_string();
        assert!(matches!(
            parse_registry_entry(nonstring_sid.as_bytes()),
            RegistryParse::ShapeDrift("sessionId")
        ));
        let renamed_pid = serde_json::json!({ "processId": 64924, "sessionId": "s" }).to_string();
        assert!(matches!(
            parse_registry_entry(renamed_pid.as_bytes()),
            RegistryParse::ShapeDrift("pid")
        ));
        let string_pid = serde_json::json!({ "pid": "64924", "sessionId": "s" }).to_string();
        assert!(matches!(
            parse_registry_entry(string_pid.as_bytes()),
            RegistryParse::ShapeDrift("pid")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn non_json_and_corrupt_values_are_silent_skips_not_drift() {
        assert!(matches!(
            parse_registry_entry(b"{\"pid\": 64924, \"sessionI"),
            RegistryParse::Skip
        ));
        let pid_zero = serde_json::json!({ "pid": 0, "sessionId": "s" }).to_string();
        assert!(matches!(
            parse_registry_entry(pid_zero.as_bytes()),
            RegistryParse::Skip
        ));
        let empty_sid = serde_json::json!({ "pid": 64924, "sessionId": "" }).to_string();
        assert!(matches!(
            parse_registry_entry(empty_sid.as_bytes()),
            RegistryParse::Skip
        ));
    }

    #[cfg(unix)]
    #[test]
    fn shape_drifted_entry_yields_no_vouch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("drifted.json"),
            serde_json::json!({
                "pid": std::process::id(), "session_id": "drifted-session"
            })
            .to_string(),
        )
        .unwrap();
        write_entry(
            dir.path(),
            "valid.json",
            i64::from(std::process::id()),
            "valid-session",
        );
        let live = live_cc_session_ids(dir.path()).expect("readable dir is a healthy probe");
        assert_eq!(
            live.ids().cloned().collect::<HashSet<_>>(),
            HashSet::from(["valid-session".to_string()]),
            "a shape-drifted entry must not vouch; the well-formed sibling survives"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pid_start_time_secs_reports_a_fresh_child_as_just_started() {
        let mut child = crate::test_child::sleeper();
        let started = pid_start_time_secs(child.id() as i32);
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let _ = child.kill();
        let _ = child.wait();
        let started = started.expect("proc_pidinfo must report a live child's start time");
        assert!(
            started.abs_diff(now_secs) <= 5,
            "child start time {started} should be within 5s of now {now_secs}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pid_start_time_secs_none_for_dead_pid() {
        assert_eq!(pid_start_time_secs(999_999_999), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn recycled_pid_entry_is_dropped_and_matching_one_kept() {
        let own_pid = std::process::id() as i32;
        let own_start = pid_start_time_secs(own_pid).expect("own start time");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("recycled.json"),
            serde_json::json!({
                "pid": own_pid,
                "sessionId": "recycled-session",
                "startedAt": (own_start - 3600) * 1000
            })
            .to_string(),
        )
        .unwrap();
        // CC stamps startedAt ~1.2s after process start — inside the tolerance.
        std::fs::write(
            dir.path().join("genuine.json"),
            serde_json::json!({
                "pid": own_pid,
                "sessionId": "genuine-session",
                "startedAt": own_start * 1000 + 1200
            })
            .to_string(),
        )
        .unwrap();
        let live = live_cc_session_ids(dir.path()).expect("readable dir is a healthy probe");
        assert_eq!(
            live.ids().cloned().collect::<HashSet<_>>(),
            HashSet::from(["genuine-session".to_string()]),
            "a recycled-pid entry must be dropped; the identity-matching one kept"
        );
    }

    #[cfg(unix)]
    fn entry(pid: i32, started_at_ms: u64) -> RegistryEntry {
        RegistryEntry {
            pid,
            session_id: "dup".into(),
            started_at_ms,
        }
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_winner_rule_is_symmetric_and_total() {
        // Every pair is asserted in BOTH presentation orders: the symmetry IS
        // the scan-order independence.
        assert!(prefer_candidate(&entry(1, 100), &entry(2, 200)));
        assert!(!prefer_candidate(&entry(2, 200), &entry(1, 100)));
        assert!(prefer_candidate(&entry(1, 100), &entry(2, 100)));
        assert!(!prefer_candidate(&entry(2, 100), &entry(1, 100)));
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_session_id_binds_a_stable_pid_regardless_of_scan_order() {
        // The same pair is presented under file names sorting in OPPOSITE
        // orders; whatever order read_dir yields, the binding must come out
        // identical. One shared startedAt, so the pid tiebreak decides.
        let mut child_a = crate::test_child::sleeper();
        let mut child_b = crate::test_child::sleeper();
        let (pid_a, pid_b) = (i64::from(child_a.id()), i64::from(child_b.id()));
        let expected = pid_a.max(pid_b) as i32;
        // Both children start within the PID-reuse tolerance of this.
        let started = pid_start_time_secs(child_a.id() as i32).unwrap_or(0) * 1000;

        // Probe inside the loop, assert only after the children are reaped —
        // a panicking assert here would leak two `sleep 30`s.
        let mut snapshots = Vec::new();
        for (first, second) in [(pid_a, pid_b), (pid_b, pid_a)] {
            let dir = tempfile::tempdir().unwrap();
            write_entry_started(dir.path(), "aaa.json", first, "dup", started);
            write_entry_started(dir.path(), "zzz.json", second, "dup", started);
            snapshots.push(live_cc_session_ids(dir.path()));
        }

        let _ = child_a.kill();
        let _ = child_a.wait();
        let _ = child_b.kill();
        let _ = child_b.wait();

        for snapshot in snapshots {
            let live = snapshot.expect("readable dir is a healthy probe");
            assert_eq!(
                live.ids().cloned().collect::<HashSet<_>>(),
                HashSet::from(["dup".to_string()]),
                "a duplicate id must yield ONE vouch, not two"
            );
            assert_eq!(
                live.pid_of.get("dup"),
                Some(&expected),
                "the winning pid must be the deterministic tiebreak winner in both name orders"
            );
        }
    }

    #[test]
    fn sessions_dir_derives_only_from_the_standard_layout() {
        assert_eq!(
            cc_sessions_dir(Path::new("/home/u/.claude/projects")),
            Some(PathBuf::from("/home/u/.claude/sessions"))
        );
        assert_eq!(cc_sessions_dir(Path::new("/tmp/fixture")), None);
        assert_eq!(cc_sessions_dir(Path::new("projects")), None);
    }
}
