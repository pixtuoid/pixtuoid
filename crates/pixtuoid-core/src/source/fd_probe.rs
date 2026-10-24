//! Process → open-file-descriptor enumeration, the OS half of the Codex
//! liveness probe. A live `codex` process holds its rollout `.jsonl` open in
//! append mode for the whole session (upstream `RolloutRecorder` owns the
//! handle), so "which rollout files does a running `codex` hold open" is a
//! first-party liveness signal.
//!
//! Contract: never panic, never block. Every failure path (process exited
//! mid-probe, EPERM, short read) skips the entry or returns empty; the probe
//! runs inside the watcher task on every scan pass.

use std::path::PathBuf;

/// PIDs of running processes whose (kernel-truncated) name equals `name`
/// exactly. Both kernels truncate the comparand at well under 32 bytes, so only
/// short names like `codex` can ever match.
///
/// `None` = the proc-table enumeration ITSELF failed — the caller must treat the
/// whole probe pass as failed (#223: an empty result from a broken enumeration
/// must not read as "every process exited"). `Some(vec![])` = the enumeration
/// ran fine and nothing matched.
pub(crate) fn pids_by_name(name: &str) -> Option<Vec<i32>> {
    imp::pids_by_name(name)
}

/// Filesystem paths of the regular-file (vnode) descriptors `pid` holds open.
/// A pid that exited or is unreadable simply contributes nothing.
pub(crate) fn open_vnode_paths(pid: i32) -> Vec<PathBuf> {
    imp::open_vnode_paths(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::path::PathBuf;

    // libc lacks ONLY the per-fd vnode-path flavor — the constant and the two
    // structs below, from <sys/proc_info.h> (ABI-stable since 10.5).
    const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

    #[repr(C)]
    struct proc_fileinfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: i64,
        fi_type: i32,
        fi_guardflags: u32,
    }

    #[repr(C)]
    struct vnode_fdinfowithpath {
        pfi: proc_fileinfo,
        pvip: libc::vnode_info_path,
    }

    pub(super) fn pids_by_name(name: &str) -> Option<Vec<i32>> {
        // SAFETY: a null buffer is the sizing call — xnu's `proc_listpids`
        // returns a count without writing (`bsd/kern/proc_info.c`).
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            tracing::debug!(
                ret = count,
                "proc_listallpids sizing failed; probe pass failed"
            );
            return None;
        }
        // Slack for processes spawned between the sizing call and the fill.
        const PID_LIST_SLACK: usize = 32;
        let cap = count as usize + PID_LIST_SLACK;
        let mut pids = vec![0 as libc::pid_t; cap];
        // SAFETY: `pids` owns exactly `cap` pid_t elements and `buffersize` is
        // exactly the matching byte length, so the kernel cannot write past
        // the allocation.
        let filled = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr() as *mut c_void,
                (cap * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
            )
        };
        if filled <= 0 {
            tracing::debug!(
                ret = filled,
                "proc_listallpids fill failed; probe pass failed"
            );
            return None;
        }
        pids.truncate(usize::min(filled as usize, cap));
        pids.retain(|&pid| pid > 0 && process_name(pid).as_deref() == Some(name));
        Some(pids)
    }

    fn process_name(pid: libc::pid_t) -> Option<String> {
        let mut buf = [0u8; 64];
        // SAFETY: buffer length passed matches the allocation; proc_name
        // writes at most `buffersize` bytes and returns the length written
        // (<= 0 on failure — pid gone or unreadable, skipped by the caller).
        let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
        if n <= 0 || n as usize > buf.len() {
            return None;
        }
        Some(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
    }

    pub(super) fn open_vnode_paths(pid: i32) -> Vec<PathBuf> {
        let fdinfo_size = std::mem::size_of::<libc::proc_fdinfo>();
        // SAFETY: null-buffer sizing call — returns the byte size of the fd
        // list, writes nothing.
        let bytes =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if bytes <= 0 {
            // Dead pid / EPERM — expected during any probe pass; skip quietly.
            return Vec::new();
        }
        // Slack for fds opened between the sizing call and the fill.
        let cap = bytes as usize / fdinfo_size + 8;
        let mut fds = vec![
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0,
            };
            cap
        ];
        // SAFETY: `fds` owns exactly `cap` proc_fdinfo entries and
        // `buffersize` is the matching byte length; proc_fdinfo is repr(C)
        // plain-old-data matching the SDK layout.
        let filled_bytes = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr() as *mut c_void,
                (cap * fdinfo_size) as libc::c_int,
            )
        };
        if filled_bytes <= 0 {
            return Vec::new();
        }
        fds.truncate(usize::min(filled_bytes as usize / fdinfo_size, cap));

        let mut out = Vec::new();
        for fd in &fds {
            if fd.proc_fdtype != libc::PROX_FDTYPE_VNODE as u32 {
                continue;
            }
            // SAFETY: all-zero bytes are a valid value for this repr(C)
            // plain-old-data struct (integers + byte arrays only).
            let mut info: vnode_fdinfowithpath = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<vnode_fdinfowithpath>() as libc::c_int;
            // SAFETY: the buffer is exactly `size` bytes of a repr(C) struct
            // matching the macOS SDK's vnode_fdinfowithpath layout (proc_info.h,
            // ABI-stable since 10.5), so the kernel fills only memory we own.
            let n = unsafe {
                libc::proc_pidfdinfo(
                    pid,
                    fd.proc_fd,
                    PROC_PIDFDVNODEPATHINFO,
                    &mut info as *mut _ as *mut c_void,
                    size,
                )
            };
            if n != size {
                // fd closed between the list and this query (TOCTOU), or the
                // vnode has no resolvable path — skip this fd.
                continue;
            }
            // vip_path is [[c_char; 32]; 32] (libc's old-rustc stand-in for a
            // flat 1024 array), hence the flatten.
            let path_bytes: Vec<u8> = info
                .pvip
                .vip_path
                .iter()
                .flatten()
                .map(|&c| c as u8)
                .take_while(|&b| b != 0)
                .collect();
            if path_bytes.is_empty() {
                continue;
            }
            out.push(PathBuf::from(
                String::from_utf8_lossy(&path_bytes).into_owned(),
            ));
        }
        out
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    pub(super) fn pids_by_name(name: &str) -> Option<Vec<i32>> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            tracing::debug!("/proc unreadable; probe pass failed");
            return None;
        };
        let mut pids = Vec::new();
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            else {
                continue;
            };
            // A process exiting mid-scan makes the read fail — skip it.
            let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
                continue;
            };
            // `pid > 0` for parity with the macOS arm: a 0 leaking downstream
            // would collide with the kqueue wake slot's ident-0 there, so keep
            // both arms structurally incapable of emitting it.
            if pid > 0 && comm.trim_end_matches('\n') == name {
                pids.push(pid);
            }
        }
        Some(pids)
    }

    /// Absoluteness IS the vnode test here: `/proc/<pid>/fd` resolves non-file
    /// descriptors to synthetic NON-absolute targets (`socket:[inode]`,
    /// `pipe:[inode]`, `anon_inode:[eventfd]`), so the `is_absolute` filter is
    /// what makes this arm match the macOS arm's `PROX_FDTYPE_VNODE`.
    ///
    /// A deleted file's link ends with `" (deleted)"`, which simply never
    /// matches a rollout path — no stripping needed.
    pub(super) fn open_vnode_paths(pid: i32) -> Vec<PathBuf> {
        // Dead pid / EPERM → read_dir fails → empty, by design.
        let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| std::fs::read_link(e.path()).ok())
            .filter(|p| p.is_absolute())
            .collect()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use std::path::PathBuf;

    // No validated fd-enumeration path on this platform. Some(empty) rather
    // than None: nothing is ever vouched here, so the pure-mtime gate applies
    // AND the negative vouch can never fire.
    pub(super) fn pids_by_name(_name: &str) -> Option<Vec<i32>> {
        Some(Vec::new())
    }

    pub(super) fn open_vnode_paths(_pid: i32) -> Vec<PathBuf> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn open_vnode_paths_sees_a_file_this_process_holds_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held-open.txt");
        std::fs::write(&path, b"x").unwrap();
        let handle = std::fs::File::open(&path).unwrap();
        // Kernel-reported fd paths are fully resolved; compare canonical.
        let canonical = path.canonicalize().unwrap();
        let pid = std::process::id() as i32;
        let open = open_vnode_paths(pid);
        drop(handle);
        assert!(
            open.contains(&canonical),
            "expected {canonical:?} among this process's open vnodes, got {open:?}"
        );
    }

    #[test]
    fn open_vnode_paths_for_dead_pid_is_empty_not_panic() {
        // Far above any real pid range on macOS/Linux.
        assert!(open_vnode_paths(999_999_999).is_empty());
    }

    /// The macOS/Linux parity pin: macOS filtered `PROX_FDTYPE_VNODE` while
    /// Linux returned every `/proc/<pid>/fd` link, `socket:[inode]` included.
    /// A caller needing a socket's OWNER needs a socket-aware probe instead
    /// (macOS `PROC_PIDFDSOCKETINFO`; on Linux a `/proc/net/unix` inode join —
    /// NOT `stat`'s `st_ino`, the path's filesystem inode rather than the
    /// sockfs inode `socket:[N]` names). grok's #638 leader vouch was this
    /// crate's only such caller and was deleted rather than repaired (#826);
    /// don't add a socket arm here on spec.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn open_vnode_paths_never_reports_a_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("leader.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let canonical = sock.canonicalize().unwrap();
        let open = open_vnode_paths(std::process::id() as i32);
        drop(listener);
        assert!(
            !open.contains(&canonical),
            "a unix socket is not a vnode path; got {open:?}"
        );
    }

    #[test]
    fn pids_by_name_for_nonexistent_process_is_some_empty() {
        // Longer than any kernel-truncated process name, so it can never match.
        assert_eq!(
            pids_by_name("definitely-not-a-process-7q3z9"),
            Some(Vec::new())
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn pids_by_name_finds_a_spawned_child() {
        let mut child = crate::test_child::sleeper();
        let pid = child.id() as i32;
        // A just-spawned child isn't always named in the proc-table at the first
        // probe — poll rather than assert once.
        let found = (0..50).any(|_| {
            if pids_by_name("sleep")
                .expect("a healthy system's proc-table enumeration must succeed")
                .contains(&pid)
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            false
        });
        let _ = child.kill();
        let _ = child.wait();
        assert!(found, "expected spawned sleep (pid {pid}) in pids_by_name");
    }
}
