use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
use tokio::sync::Semaphore;
use tracing::warn;
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;

use crate::win32::SecurityDescriptor;

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
            // is indistinguishable and also degrades to transcript-only;
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
