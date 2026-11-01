use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};
use tokio::sync::Semaphore;
use tracing::warn;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, HANDLE};
use windows_sys::Win32::Security::{EqualSid, PSID, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::win32::{SecurityDescriptor, token_info};

use super::{CONN_TIMEOUT, MAX_CONCURRENT_CONNS};

/// Must cover the shim's whole stamped wire line — `STDIN_CAP` + the 256B
/// `STAMP_HEADROOM` in pixtuoid-hook are test-pinned to this 1MiB quota — so
/// the shim's sync write can't stall behind a busy daemon task.
const IN_BUFFER_SIZE: u32 = 1 << 20;

/// Owner-only, the named-pipe equivalent of the Unix socket's umask-0700,
/// closing the default DACL's Everyone-READ.
const OWNER_ONLY: &str = "D:P(A;;GA;;;OW)";

/// A hook pipe instance under `sd`. `first` claims `first_pipe_instance`: ONLY
/// the initial bind may, so a taken name surfaces as the typed `SocketBusy`.
/// The recreate + next-instance must NOT claim it — the in-flight instance
/// still holds it, and re-claiming fails ACCESS_DENIED.
fn create_pipe(
    sd: &SecurityDescriptor,
    name: &str,
    first: bool,
) -> std::io::Result<NamedPipeServer> {
    let attrs: *mut c_void = sd.attributes().cast_mut().cast();
    // SAFETY: `attrs` is the well-formed SECURITY_ATTRIBUTES `sd` owns, and the
    // borrow of `sd` outlives the call; the kernel copies the descriptor during
    // CreateNamedPipeW, so nothing borrows past it.
    unsafe { hook_pipe_options(first).create_with_security_attributes_raw(name, attrs) }
}

/// One accepted hook connection.
pub(super) type Conn = NamedPipeServer;

/// A client's end of a [`Conn`].
pub(super) type Client = NamedPipeClient;

/// How long a connect waits out a pipe whose every instance is mid-handshake.
pub(crate) const PIPE_BUSY_BOUND: Duration = Duration::from_secs(2);
pub(crate) const PIPE_BUSY_BACKOFF: Duration = Duration::from_millis(10);

/// Connect to the hook endpoint at `path`, as long as its server is this user:
/// the pipe namespace is machine-wide, so another user can squat the name and
/// feed the office invented agents.
pub(crate) async fn connect(path: &Path) -> std::io::Result<Client> {
    let started = std::time::Instant::now();
    let client = loop {
        match ClientOptions::new().open(path) {
            Ok(client) => break client,
            Err(e)
                if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                    && started.elapsed() < PIPE_BUSY_BOUND =>
            {
                tokio::time::sleep(PIPE_BUSY_BACKOFF).await;
            }
            Err(e) => return Err(e),
        }
    };
    if server_is_us(&client) {
        Ok(client)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "another user owns the hook endpoint",
        ))
    }
}

/// The `PSID` embedded in a `TOKEN_USER` blob. Valid only while `blob` lives.
///
/// SAFETY: `blob` is a `TOKEN_USER` written by `GetTokenInformation`, u64-aligned.
unsafe fn sid_of(blob: &[u64]) -> PSID {
    let user = blob.as_ptr().cast::<TOKEN_USER>();
    // SAFETY: forwarded from this fn's contract.
    unsafe { (*user).User.Sid }
}

/// True iff the pipe server behind `client` runs as our user. Fail-closed. The
/// shim's `transport::peer` is the twin: no dep edge joins the two crates.
fn server_is_us(client: &NamedPipeClient) -> bool {
    use std::os::windows::io::AsRawHandle;
    let handle = client.as_raw_handle() as HANDLE;
    // SAFETY: `handle` is a live connected pipe; `server` (from OpenProcess) is
    // closed exactly once.
    unsafe {
        let mut server_pid: u32 = 0;
        if GetNamedPipeServerProcessId(handle, &mut server_pid) == 0 {
            return false;
        }
        let server = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, server_pid);
        if server.is_null() {
            return false;
        }
        let theirs = token_info(server, TokenUser);
        CloseHandle(server);
        theirs.is_some_and(|theirs| is_us(&theirs))
    }
}

/// Whether `user`, a `TOKEN_USER` blob, is our own user.
fn is_us(user: &[u64]) -> bool {
    // SAFETY: the pseudo-handle needs no close, and the two SID buffers outlive
    // the EqualSid call.
    unsafe {
        token_info(GetCurrentProcess(), TokenUser)
            .is_some_and(|ours| EqualSid(sid_of(&ours), sid_of(user)) != 0)
    }
}

#[derive(Debug)]
pub(super) struct Listener {
    server: NamedPipeServer,
    name: String,
    /// Held for the daemon's lifetime, so every create site's attributes stay valid.
    sd: SecurityDescriptor,
}

fn hook_pipe_options(first: bool) -> ServerOptions {
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte)
        .in_buffer_size(IN_BUFFER_SIZE);
    opts
}

impl Listener {
    pub(super) async fn bind(path: &Path) -> Result<Self> {
        let name = path.to_string_lossy().into_owned();
        let sd = SecurityDescriptor::from_sddl(OWNER_ONLY)
            .context("converting owner-only SDDL into a pipe security descriptor")?;
        // The server stays DUPLEX (tokio default): the shim's client opens
        // read+write, so an inbound-only pipe would reject it with
        // ACCESS_DENIED — a silent event drop.
        let server = match create_pipe(&sd, &name, true) {
            Ok(s) => s,
            // ERROR_ACCESS_DENIED is almost always another instance holding
            // first_pipe_instance on this name — the one recoverable bind
            // failure. A genuine ACL denial (restricted token / AppContainer)
            // is indistinguishable and ends in the router's failed subscribe;
            // accepted trade-off. Every other create error stays fatal.
            Err(e)
                if e.kind() == std::io::ErrorKind::PermissionDenied
                    || e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
            {
                return Err(anyhow::Error::new(super::SocketBusy {
                    path: path.to_path_buf(),
                }));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("creating hook pipe at {name}"));
            }
        };
        Ok(Self { server, name, sd })
    }

    /// Accept connections forever, each handed to `handle` under [`CONN_TIMEOUT`].
    pub(super) async fn serve<F, Fut>(mut self, handle: F) -> Result<()>
    where
        F: Fn(Conn) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNS));
        loop {
            let permit = match Arc::clone(&sem).acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    anyhow::bail!("hook pipe semaphore closed unexpectedly");
                }
            };
            if let Err(e) = self.server.connect().await {
                // A failed instance isn't guaranteed reusable — recreate it.
                warn!(error = %e, "hook pipe connect error; recreating instance");
                self.server = create_pipe(&self.sd, &self.name, false).with_context(|| {
                    format!("re-creating hook pipe after connect error at {}", self.name)
                })?;
                continue;
            }
            // Create the NEXT instance BEFORE handing this one off: in the gap
            // between handoff and re-create, clients get ERROR_PIPE_BUSY or
            // NotFound depending on timing.
            let next = create_pipe(&self.sd, &self.name, false)
                .with_context(|| format!("re-creating hook pipe at {}", self.name))?;
            let handled = handle(std::mem::replace(&mut self.server, next));
            tokio::spawn(async move {
                let _permit = permit;
                let _ = tokio::time::timeout(CONN_TIMEOUT, handled).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Security::Authorization::SE_KERNEL_OBJECT;

    use crate::win32::test_support::held_dacl;

    #[test]
    fn another_user_is_not_us() {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
        use windows_sys::Win32::Security::SID_AND_ATTRIBUTES;
        // The anonymous logon, which no test runner is.
        let wide = crate::win32::nul_terminated(std::ffi::OsStr::new("S-1-5-7"));
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; `sid` is a live out-param.
        let converted = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) };
        assert_ne!(converted, 0);
        let user = TOKEN_USER {
            User: SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: 0,
            },
        };
        let words = size_of::<TOKEN_USER>() / size_of::<u64>();
        // SAFETY: `user` is u64-aligned and spans `words` words.
        let blob = unsafe { std::slice::from_raw_parts((&raw const user).cast::<u64>(), words) };
        assert!(!is_us(blob));
        // SAFETY: LocalAlloc'd above, freed once.
        unsafe { LocalFree(sid) };
    }

    #[tokio::test]
    async fn our_own_pipe_server_verifies_as_us() {
        let name = format!(r"\\.\pipe\pixtuoid-peer-selftest-{}", std::process::id());
        let _server = ServerOptions::new().create(&name).unwrap();
        let client = ClientOptions::new().open(&name).unwrap();
        assert!(server_is_us(&client));
    }

    /// No API reads `PIPE_REJECT_REMOTE_CLIENTS` back off a pipe — neither
    /// [`GetNamedPipeInfo`](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-getnamedpipeinfo)
    /// nor [`GetNamedPipeHandleState`](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-getnamedpipehandlestatew)
    /// reports it — so the options are read where they are built.
    #[test]
    fn the_hook_pipe_rejects_remote_clients() {
        for first in [true, false] {
            let opts = format!("{:?}", hook_pipe_options(first));
            assert!(opts.contains("reject_remote_clients: true"), "{opts}");
        }
    }

    #[test]
    fn the_first_instance_is_claimed_only_when_asked() {
        for first in [true, false] {
            let opts = format!("{:?}", hook_pipe_options(first));
            assert!(
                opts.contains(&format!("first_pipe_instance: {first}")),
                "{opts}"
            );
        }
    }

    /// One protected ACE, for the owner alone, read back off the pipe `bind`
    /// creates: a bind that drops the descriptor reads the default DACL here.
    #[tokio::test]
    async fn the_hook_pipe_is_owner_only() {
        let name = format!(r"\\.\pipe\pixtuoid-sd-{}", std::process::id());
        let listener = Listener::bind(Path::new(&name)).await.expect("bind");
        let sddl = held_dacl(listener.server.as_raw_handle(), SE_KERNEL_OBJECT);
        let (flags, aces) = sddl.split_once('(').expect("an ACE");
        assert!(flags.starts_with("D:") && flags.contains('P'), "{sddl}");
        assert_eq!(aces.matches('(').count(), 0, "one ACE: {sddl}");
        assert!(
            aces.starts_with("A;;") && aces.ends_with(";;;OW)"),
            "{sddl}"
        );
    }
}
