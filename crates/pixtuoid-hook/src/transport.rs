//! Best-effort one-line delivery to the daemon. Contract on every path
//! (invariant: never block CC): all failures return silently (caller exits 0)
//! and the entire send is bounded by ~`WRITE_TIMEOUT` on both platforms.

use std::time::Duration;

/// The send bound in millis — the authoritative number, so a derived budget
/// (`cli_pid::WALK_BUDGET`) divides an integer instead of an `Option<Duration>`.
pub(crate) const WRITE_TIMEOUT_MS: u64 = 200;
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_millis(WRITE_TIMEOUT_MS);

/// Proof the time bound is armed: [`send_line`] takes one, so an unbounded send
/// is a compile error rather than a rule to remember.
pub(crate) struct TimeBound(());

/// Arm the shim's hard time bound — exit(0)-on-timeout IS the contract.
/// `Builder::spawn` so thread exhaustion yields `None` rather than a panic.
///
/// Armed before the shim's own OS work, not just the send (#882). Reading stdin
/// stays OUTSIDE it: that is the CLI writing to us, and a large slow payload
/// should arrive rather than be cut into a dropped event.
pub(crate) fn arm_watchdog() -> Option<TimeBound> {
    std::thread::Builder::new()
        .spawn(|| {
            std::thread::sleep(WRITE_TIMEOUT);
            std::process::exit(0);
        })
        .ok()
        .map(|_| TimeBound(()))
}

#[cfg(unix)]
pub(crate) fn send_line(_bound: &TimeBound, endpoint: &std::path::Path, line: &[u8]) {
    use std::io::Write;
    // `UnixStream::connect` has no timeout knob, and a backlog-saturated
    // listener parks it indefinitely — past the budget `set_write_timeout` only
    // enforces AFTER a successful connect (#167) — the caller's `TimeBound`
    // covers it.
    if let Ok(mut s) = std::os::unix::net::UnixStream::connect(endpoint) {
        // For the `/tmp/pixtuoid-{uid}/` fallback we own, verify the connected
        // PEER is us BEFORE writing (#485) — on the connected fd, atomic w.r.t.
        // the connection, closing a TOCTOU a pre-connect path stat can't. Scoped
        // to the fallback: an XDG or explicit PIXTUOID_SOCKET endpoint is the
        // user's own trust decision (it may point at a cross-uid system daemon).
        if crate::paths::owned_tmp_socket_dir(endpoint).is_some() && !peer_is_us(&s) {
            return;
        }
        let _ = s.set_write_timeout(Some(WRITE_TIMEOUT));
        let _ = s.write_all(line);
    }
}

/// True iff the connected peer's effective uid is OURS, validated on the live fd
/// (atomic w.r.t. the connection — no TOCTOU). Fails CLOSED when the peer uid
/// can't be read: the syscall doesn't fail on a healthy connected `AF_UNIX`
/// stream, so a failure means we cannot prove the peer is us.
#[cfg(unix)]
fn peer_is_us(stream: &std::os::unix::net::UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;
    // Safety: getuid is always safe on Unix.
    peer_uid(stream.as_raw_fd()) == Some(unsafe { libc::getuid() })
}

/// The connected peer's uid, or `None` if it can't be read. Linux exposes it via
/// `SO_PEERCRED`; macOS/BSD via `getpeereid`, which `libc` doesn't declare on
/// Linux — hence the split.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(fd: std::os::unix::io::RawFd) -> Option<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // Safety: `fd` is a live connected socket; the kernel writes `cred`/`len`.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(cred).cast(),
            &mut len,
        )
    };
    (rc == 0).then_some(cred.uid)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn peer_uid(fd: std::os::unix::io::RawFd) -> Option<u32> {
    let mut euid: libc::uid_t = 0;
    let mut egid: libc::gid_t = 0;
    // Safety: `fd` is a live connected socket; the kernel writes the out-params.
    let rc = unsafe { libc::getpeereid(fd, &mut euid, &mut egid) };
    (rc == 0).then_some(euid)
}

#[cfg(all(unix, test))]
mod tests {
    use super::peer_is_us;
    use std::os::unix::net::{UnixListener, UnixStream};

    #[test]
    fn peer_is_us_for_a_self_connection() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let sock = tmp.path().join("s.sock");
        let listener = UnixListener::bind(&sock).expect("bind");
        let client = UnixStream::connect(&sock).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        assert!(peer_is_us(&client), "our own connection's peer is us");
        assert!(peer_is_us(&server), "the accepted side's peer is us too");
    }
}

#[cfg(windows)]
pub(crate) fn send_line(_bound: &TimeBound, endpoint: &std::path::Path, line: &[u8]) {
    use std::io::Write;
    // Named pipes have no SO_SNDTIMEO equivalent for sync writes, so the timeout
    // invariant is enforced solely by the watchdog's hard exit — hence the
    // `TimeBound` parameter: the ERROR_PIPE_BUSY retry below is unbounded without
    // it.
    const ERROR_PIPE_BUSY: i32 = windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32;
    const PIPE_BUSY_RETRY_BACKOFF_MS: u64 = 10;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(endpoint)
        {
            Ok(mut f) => {
                // #495: before writing, verify the pipe SERVER runs as US — a
                // co-located user who squatted our predictable default pipe (so
                // our daemon's create failed → SocketBusy) would else
                // receive the payload. Scoped to our default rendezvous; an
                // explicit PIXTUOID_SOCKET pipe is the user's trust call.
                if endpoint == std::path::Path::new(&crate::paths::default_windows_pipe_name())
                    && !peer::server_is_us(&f)
                {
                    return;
                }
                let _ = f.write_all(line);
                return;
            }
            // Retry until the watchdog fires (all server instances mid-handshake).
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                std::thread::sleep(Duration::from_millis(PIPE_BUSY_RETRY_BACKOFF_MS));
            }
            Err(_) => return,
        }
    }
}

/// The Windows counterpart of the Unix `peer_is_us` check (#495): verify the
/// named-pipe SERVER runs as OUR user before the shim writes the hook payload.
/// Windows named pipes are a machine-global, unprivileged namespace, so a
/// co-located user can pre-create our predictable `\\.\pipe\pixtuoid-{USERNAME}`
/// and receive it. EVERYTHING here fails CLOSED (any FFI failure ⇒ `false` ⇒
/// drop) and never panics.
///
/// KNOWN SHARP EDGE — pid→token, not fd-atomic (don't "fix" it): resolving the
/// server via `GetNamedPipeServerProcessId` → `OpenProcess` carries an inherent
/// PID-reuse TOCTOU, but it is unexploitable — for the payload to leak the
/// squatter's server must stay ALIVE to receive the write, so its pid can't have
/// been recycled. The atomic alternative, `GetSecurityInfo(OWNER)`, would
/// false-negative an admin daemon (owner = Administrators, not the user).
#[cfg(windows)]
mod peer {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// The `TOKEN_USER` blob for `process`'s token, `u64`-backed so the embedded
    /// `PSID` pointer is properly aligned (a `Vec<u8>` would only guarantee
    /// align-1). The buffer OWNS the SID — keep it alive across `EqualSid`.
    ///
    /// SAFETY: `process` is a valid process handle for the call's duration.
    unsafe fn token_user_blob(process: HANDLE) -> Option<Vec<u64>> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `process` is forwarded from this fn's contract; `token` is a live out-param.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        // Size probe (returns 0 + sets `len`), then the real read.
        let mut len: u32 = 0;
        // SAFETY: `token` is open; a null buffer of length 0 is the documented size probe.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        let blob = if len == 0 {
            None
        } else {
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            // SAFETY: `token` is open and `buf` spans at least `len` bytes for the call.
            let read = unsafe {
                GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
            };
            if read == 0 { None } else { Some(buf) }
        };
        // SAFETY: `token` is open, and this is its only close.
        unsafe { CloseHandle(token) };
        blob
    }

    /// The `PSID` embedded in a `TOKEN_USER` blob. Valid only while `blob` lives.
    ///
    /// SAFETY: `blob` is a `TOKEN_USER` written by `GetTokenInformation`, u64-aligned.
    unsafe fn sid_of(blob: &[u64]) -> PSID {
        let user = blob.as_ptr().cast::<TOKEN_USER>();
        // SAFETY: forwarded from this fn's contract.
        unsafe { (*user).User.Sid }
    }

    /// True iff the pipe server behind `file` runs as our user. Fail-closed.
    pub(super) fn server_is_us(file: &std::fs::File) -> bool {
        let handle = file.as_raw_handle() as HANDLE;
        // SAFETY: `handle` is a live connected pipe; `server` (from OpenProcess)
        // is closed exactly once; the two SID buffers outlive the EqualSid call.
        unsafe {
            let mut server_pid: u32 = 0;
            if GetNamedPipeServerProcessId(handle, &mut server_pid) == 0 {
                return false;
            }
            // Our own user SID (GetCurrentProcess is a pseudo-handle — never closed).
            let Some(ours) = token_user_blob(GetCurrentProcess()) else {
                return false;
            };
            let server = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, server_pid);
            if server.is_null() {
                return false;
            }
            let theirs = token_user_blob(server);
            CloseHandle(server);
            let Some(theirs) = theirs else {
                return false;
            };
            EqualSid(sid_of(&ours), sid_of(&theirs)) != 0
        }
    }
}

// No in-process tests for `send_line` ON PURPOSE: it spawns a watchdog that
// exit(0)s the whole process a `WRITE_TIMEOUT` later, which would kill sibling tests under
// `cargo test`'s shared-process runner. Its coverage lives at the child-process
// level (tests/shim.rs and its Windows twin tests/shim_pipe.rs), where
// exit-is-the-contract is observable, not fatal.

#[cfg(all(windows, test))]
mod win_peer_tests {
    #[tokio::test]
    async fn server_is_us_for_a_self_hosted_pipe() {
        // A unique name per run so parallel test processes don't collide.
        let name = format!(r"\\.\pipe\pixtuoid-peer-selftest-{}", std::process::id());
        let _server = tokio::net::windows::named_pipe::ServerOptions::new()
            .create(&name)
            .expect("create self-hosted pipe server");
        // A blocking std client: the same handle type `send_line` writes over.
        let client = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .expect("open client end");
        assert!(
            super::peer::server_is_us(&client),
            "our own process's pipe server must verify as us"
        );
    }
}
