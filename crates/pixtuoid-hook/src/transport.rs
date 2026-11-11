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

/// Write `line` to `peer`, reached at `endpoint`, unless that is
/// [our own rendezvous](crate::paths::is_our_rendezvous) and `is_us` can't prove
/// the peer runs as us: a squatter there would else receive the payload. `is_us`
/// is a parameter because a single-user host can stage no peer of another user.
fn deliver<P: std::io::Write>(
    mut peer: P,
    endpoint: &std::path::Path,
    line: &[u8],
    is_us: impl FnOnce(&P) -> bool,
) {
    if crate::paths::is_our_rendezvous(endpoint) && !is_us(&peer) {
        return;
    }
    let _ = peer.write_all(line);
}

#[cfg(unix)]
pub(crate) fn send_line(_bound: &TimeBound, endpoint: &std::path::Path, line: &[u8]) {
    // `UnixStream::connect` has no timeout knob, and a backlog-saturated
    // listener parks it indefinitely — past the budget `set_write_timeout` only
    // enforces AFTER a successful connect (#167) — the caller's `TimeBound`
    // covers it.
    if let Ok(s) = std::os::unix::net::UnixStream::connect(endpoint) {
        let _ = s.set_write_timeout(Some(WRITE_TIMEOUT));
        deliver(s, endpoint, line, peer_is_us);
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
    // Safety: `cred` and `len` are live locals the kernel writes; it checks `fd` itself.
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
    // Safety: the out-params are live locals the kernel writes; it checks `fd` itself.
    let rc = unsafe { libc::getpeereid(fd, &mut euid, &mut egid) };
    (rc == 0).then_some(euid)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::peer_is_us;
    #[cfg(unix)]
    use std::os::unix::net::{UnixListener, UnixStream};

    /// Our `/tmp` fallback rendezvous, which the shim resolves with neither
    /// `PIXTUOID_SOCKET` nor `XDG_RUNTIME_DIR`.
    #[cfg(unix)]
    fn ours() -> std::path::PathBuf {
        // Safety: getuid is always safe on Unix.
        let uid = unsafe { libc::getuid() };
        format!("/tmp/pixtuoid-{uid}/pixtuoid.sock").into()
    }

    #[cfg(windows)]
    fn ours() -> std::path::PathBuf {
        crate::paths::default_windows_pipe_name().into()
    }

    #[test]
    fn our_own_rendezvous_gets_the_line_only_when_its_peer_is_proven_us() {
        let elsewhere = std::path::PathBuf::from("/explicit/path.sock");
        for (endpoint, proven, delivered) in [
            (ours(), true, true),
            (ours(), false, false),
            (elsewhere, false, true),
        ] {
            let mut got = Vec::new();
            super::deliver(&mut got, &endpoint, b"line\n", |_| proven);
            assert_eq!(
                got == b"line\n",
                delivered,
                "{endpoint:?}, proven us: {proven}"
            );
        }
    }

    /// The only peer of another uid a single-user host can stage is none at all.
    #[cfg(unix)]
    #[test]
    fn an_unconnected_socket_is_not_us() {
        use std::os::fd::FromRawFd;
        // Safety: socket(2) takes no pointers; a non-negative fd is ours to own.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0, "a socket");
        // Safety: `fd` is a fresh socket nothing else owns.
        let unconnected = unsafe { UnixStream::from_raw_fd(fd) };
        assert!(!peer_is_us(&unconnected));
    }

    /// A failed read is no uid: the out-param starts at uid 0, so a read taken without
    /// its result would prove root a peer it never saw.
    #[cfg(unix)]
    #[test]
    fn a_failed_read_names_no_peer_uid() {
        use std::os::fd::AsRawFd;
        let file = tempfile::tempfile().expect("a file");
        assert_eq!(super::peer_uid(file.as_raw_fd()), None, "no socket");
        assert_eq!(super::peer_uid(-1), None, "no fd");
    }

    #[cfg(unix)]
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
    // Named pipes have no SO_SNDTIMEO equivalent for sync writes, so the timeout
    // invariant is enforced solely by the watchdog's hard exit — hence the
    // `TimeBound` parameter: the ERROR_PIPE_BUSY retry below is unbounded without
    // it.
    const ERROR_PIPE_BUSY: i32 = windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32;
    const PIPE_BUSY_RETRY_BACKOFF_MS: u64 = 10;
    loop {
        match open_pipe(endpoint) {
            Ok(f) => return deliver(f, endpoint, line, peer::server_is_us),
            // Retry until the watchdog fires (all server instances mid-handshake).
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                std::thread::sleep(Duration::from_millis(PIPE_BUSY_RETRY_BACKOFF_MS));
            }
            Err(_) => return,
        }
    }
}

/// The pipe's client end at `SecurityIdentification`: once a server has read our
/// line it impersonates at `SecurityImpersonation` by default
/// (<https://learn.microsoft.com/en-us/windows/win32/ipc/impersonating-a-named-pipe-client>),
/// and an explicit `PIXTUOID_SOCKET` pipe never passes [`peer::server_is_us`]; at
/// this level a server can identify us, never act as us.
#[cfg(windows)]
fn open_pipe(endpoint: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .security_qos_flags(windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION)
        .open(endpoint)
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

// No in-process tests for `send_line` ON PURPOSE: its `TimeBound` stands for
// `arm_watchdog`'s watchdog, which exit(0)s the process a `WRITE_TIMEOUT` later and
// would kill sibling tests under `cargo test`'s shared-process runner. Its coverage
// lives at the child-process level (tests/shim.rs and its Windows twin
// tests/shim_pipe.rs), where exit-is-the-contract is observable, not fatal.

#[cfg(all(windows, test))]
mod win_peer_tests {
    #[tokio::test]
    async fn server_is_us_for_a_self_hosted_pipe() {
        // A unique name per run so parallel test processes don't collide.
        let name = format!(r"\\.\pipe\pixtuoid-peer-selftest-{}", std::process::id());
        let _server = tokio::net::windows::named_pipe::ServerOptions::new()
            .create(&name)
            .expect("create self-hosted pipe server");
        let client = super::open_pipe(std::path::Path::new(&name)).expect("open client end");
        assert!(
            super::peer::server_is_us(&client),
            "our own process's pipe server must verify as us"
        );
    }

    /// The only server of another user a single-user host can stage is none at all.
    #[test]
    fn a_handle_with_no_pipe_server_is_not_us() {
        let exe = std::env::current_exe().expect("our own path");
        let not_a_pipe = std::fs::File::open(exe).expect("our own image opens");
        assert!(!super::peer::server_is_us(&not_a_pipe));
    }

    /// What a pipe server gets once it has read the shim's line, read the way a
    /// hostile one would: impersonate, then inspect the thread token's level.
    #[tokio::test]
    async fn the_pipe_server_can_identify_the_shim_but_never_act_as_it() {
        use std::io::Write;
        use std::os::windows::io::AsRawHandle;

        use tokio::io::AsyncReadExt;
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{
            GetTokenInformation, RevertToSelf, SECURITY_IMPERSONATION_LEVEL,
            SecurityIdentification, TOKEN_QUERY, TokenImpersonationLevel,
        };
        use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
        use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

        let name = format!(r"\\.\pipe\pixtuoid-qos-selftest-{}", std::process::id());
        let mut server = tokio::net::windows::named_pipe::ServerOptions::new()
            .create(&name)
            .expect("create self-hosted pipe server");
        let mut client = super::open_pipe(std::path::Path::new(&name)).expect("open client end");
        server.connect().await.expect("server sees the client");
        client.write_all(b"\n").expect("client writes");
        let mut byte = [0u8; 1];
        server.read_exact(&mut byte).await.expect("server reads");

        let mut level: SECURITY_IMPERSONATION_LEVEL = 0;
        // SAFETY: `server` is a live connected pipe handle for the whole block;
        // `token` is closed only when OpenThreadToken opened it; `level` outlives
        // the read and is exactly the length passed; RevertToSelf ends the
        // impersonation on this thread before any assert can unwind.
        let (impersonated, read) = unsafe {
            let impersonated = ImpersonateNamedPipeClient(server.as_raw_handle() as HANDLE) != 0;
            let mut token: HANDLE = std::ptr::null_mut();
            let opened = impersonated
                && OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) != 0;
            let mut len = 0u32;
            let read = opened
                && GetTokenInformation(
                    token,
                    TokenImpersonationLevel,
                    (&raw mut level).cast(),
                    size_of::<SECURITY_IMPERSONATION_LEVEL>() as u32,
                    &mut len,
                ) != 0;
            if opened {
                CloseHandle(token);
            }
            RevertToSelf();
            (impersonated, read)
        };
        assert!(
            impersonated && read,
            "the server reads the client's token level"
        );
        assert_eq!(level, SecurityIdentification);
    }
}
