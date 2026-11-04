//! mDNSResponder's `DNSServiceRegister` ([dns_sd.h](https://github.com/apple-oss-distributions/mDNSResponder/blob/mDNSResponder-2881.120.11/mDNSShared/dns_sd.h)),
//! in libSystem, which std already links. Its replies arrive on a socket this
//! future reads, since "the daemon may terminate its connection with a client
//! that does not process the daemon's responses".

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::ptr;

use anyhow::{Context, Result, bail};
use pixtuoid_core::remote::SERVICE_TYPE;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

type ServiceRef = *mut c_void;
type Flags = u32;
type ErrorType = i32;
type RegisterReply = unsafe extern "C" fn(
    ServiceRef,
    Flags,
    ErrorType,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);

/// `kDNSServiceErr_NoError`.
const NO_ERROR: ErrorType = 0;

/// `kDNSServiceFlagsAdd`: the reply announces the name rather than its loss.
const FLAGS_ADD: Flags = 0x2;

unsafe extern "C" {
    fn DNSServiceRegister(
        sd_ref: *mut ServiceRef,
        flags: Flags,
        interface_index: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        host: *const c_char,
        port: u16,
        txt_len: u16,
        txt_record: *const c_void,
        callback: Option<RegisterReply>,
        context: *mut c_void,
    ) -> ErrorType;
    fn DNSServiceRefSockFD(sd_ref: ServiceRef) -> i32;
    fn DNSServiceProcessResult(sd_ref: ServiceRef) -> ErrorType;
    fn DNSServiceRefDeallocate(sd_ref: ServiceRef);
}

/// Announce `instance` on `port`, with the pin `spki`, until the responder lets the
/// service go. The responder renames the instance on a conflict, which no flag
/// here turns off.
pub(super) async fn announce(instance: &str, port: u16, spki: &str) -> Result<()> {
    register(instance, port, spki)?.run().await
}

/// The registration, with its socket on the runtime's poller.
fn register(instance: &str, port: u16, spki: &str) -> Result<Driver> {
    let name = CString::new(instance).context("an instance name with a NUL")?;
    let regtype = CString::new(SERVICE_TYPE)?;
    let record = txt_record(&super::txt(spki))?;
    let record_len = u16::try_from(record.len())?;
    let replied = Box::new(Cell::new(NO_ERROR));
    let mut sd_ref: ServiceRef = ptr::null_mut();
    // SAFETY: every pointer lives through the call, which copies what it reads
    // ("the DNSServiceRegister call copies the txtRecord data"), but `replied`,
    // which `Driver` keeps past the ref; `registered` has the reply's C
    // signature. Port is in network byte order, as it asks.
    let error = unsafe {
        DNSServiceRegister(
            &raw mut sd_ref,
            0,
            0,
            name.as_ptr(),
            regtype.as_ptr(),
            ptr::null(),
            ptr::null(),
            port.to_be(),
            record_len,
            record.as_ptr().cast(),
            Some(registered),
            ptr::from_ref(&*replied).cast_mut().cast(),
        )
    };
    held(error)?;
    let service = Service(sd_ref);
    // SAFETY: the ref was just initialized.
    let fd = unsafe { DNSServiceRefSockFD(service.0) };
    if fd < 0 {
        bail!("mDNSResponder gave the service no socket");
    }
    // SAFETY: the ref keeps the socket open until it is deallocated, which
    // `Driver`'s field order puts after the `AsyncFd`'s drop.
    let socket = unsafe { AsyncFd::register_with_interest(Socket(fd), Interest::READABLE) }
        .map_err(|error| error.into_parts().1)?;
    Ok(Driver {
        socket,
        service,
        replied,
    })
}

/// Whether `error`, a call's or a reply's, leaves the service held.
fn held(error: ErrorType) -> Result<()> {
    if error == NO_ERROR {
        Ok(())
    } else {
        bail!("mDNSResponder refused the service (error {error})")
    }
}

/// A TXT record of one string: a length byte, then the string.
fn txt_record(txt: &str) -> Result<Vec<u8>> {
    let len = u8::try_from(txt.len()).context("a TXT string past 255 bytes")?;
    Ok([&[len], txt.as_bytes()].concat())
}

/// The registration and its socket, the socket first so that it leaves the
/// runtime's poller before the ref that closes it: "it should be removed BEFORE
/// DNSServiceRefDeallocate() is called".
struct Driver {
    socket: AsyncFd<Socket>,
    service: Service,
    /// The last reply's error, which the socket may outlive; last, since the
    /// ref's replies write it.
    replied: Box<Cell<ErrorType>>,
}

impl Driver {
    /// Hand each reply to `registered` until the responder lets the service go;
    /// a refusal is the error.
    async fn run(self) -> Result<()> {
        loop {
            let mut ready = match self.socket.readable().await {
                Ok(ready) => ready,
                Err(error) => {
                    tracing::debug!(error = ?error, "mDNSResponder's socket failed");
                    return Ok(());
                }
            };
            match ready.try_io(|socket| socket.get_ref().reply_waits()) {
                Err(_no_reply) => continue,
                Ok(Err(error)) => {
                    tracing::debug!(error = ?error, "mDNSResponder closed the service's socket");
                    return Ok(());
                }
                Ok(Ok(())) => {}
            }
            // SAFETY: a reply waits, so this returns without blocking, and only
            // this future touches the ref.
            let error = unsafe { DNSServiceProcessResult(self.service.0) };
            if error != NO_ERROR {
                tracing::debug!(error, "mDNSResponder's reply failed");
                return Ok(());
            }
            held(self.replied.get())?;
        }
    }
}

/// A ref, deallocated on drop, which "will be deregistered".
struct Service(ServiceRef);

// SAFETY: dns_sd.h "does no locking internally" and leaves serializing a ref's
// use to its client; only the one future that owns this one ever uses it.
unsafe impl Send for Service {}

impl Drop for Service {
    fn drop(&mut self) {
        // SAFETY: the ref is live, and nothing processes it once its owner drops.
        unsafe { DNSServiceRefDeallocate(self.0) }
    }
}

/// The ref's socket, which the ref owns and closes.
struct Socket(RawFd);

impl AsRawFd for Socket {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Socket {
    /// Whether a reply waits, left in place: `DNSServiceProcessResult` "will
    /// block until data does become available".
    fn reply_waits(&self) -> io::Result<()> {
        let mut byte = 0_u8;
        // SAFETY: a one-byte buffer the call may write; MSG_PEEK leaves the byte
        // for `DNSServiceProcessResult`.
        let read = unsafe {
            libc::recv(
                self.0,
                (&raw mut byte).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        match read {
            1.. => Ok(()),
            0 => Err(io::ErrorKind::UnexpectedEof.into()),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

/// The responder's word on the service, its error kept where `run` reads it:
/// announced, withdrawn to be renamed, or refused.
unsafe extern "C" fn registered(
    _sd_ref: ServiceRef,
    flags: Flags,
    error: ErrorType,
    name: *const c_char,
    _regtype: *const c_char,
    _domain: *const c_char,
    context: *mut c_void,
) {
    // SAFETY: the context is the `Driver`'s `replied`, which outlives its ref,
    // and only `run`'s `DNSServiceProcessResult` calls this, on `run`'s thread.
    unsafe { &*context.cast::<Cell<ErrorType>>() }.set(error);
    if error != NO_ERROR {
        return;
    }
    // SAFETY: on success the reply names the instance, NUL-terminated and live
    // through the call.
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    if flags & FLAGS_ADD == 0 {
        // A conflict's: the responder "renames and reregisters" the name, an Add
        // to follow (mDNSResponder-2881.120.11 `mDNSShared/uds_daemon.c:1081-1086`).
        tracing::info!(name = ?name, "mDNSResponder withdrew the name to rename it");
    } else {
        tracing::info!(name = ?name, "announced on the local network");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type ResolveReply = unsafe extern "C" fn(
        ServiceRef,
        Flags,
        u32,
        ErrorType,
        *const c_char,
        *const c_char,
        u16,
        u16,
        *const u8,
        *mut c_void,
    );

    unsafe extern "C" {
        fn DNSServiceResolve(
            sd_ref: *mut ServiceRef,
            flags: Flags,
            interface_index: u32,
            name: *const c_char,
            regtype: *const c_char,
            domain: *const c_char,
            callback: Option<ResolveReply>,
            context: *mut c_void,
        ) -> ErrorType;
    }

    /// The port, in host order, and TXT record a resolve found.
    type Found = Option<(u16, Vec<u8>)>;

    unsafe extern "C" fn resolved(
        _sd_ref: ServiceRef,
        _flags: Flags,
        _interface_index: u32,
        error: ErrorType,
        _fullname: *const c_char,
        _host: *const c_char,
        port: u16,
        txt_len: u16,
        txt: *const u8,
        context: *mut c_void,
    ) {
        if error != NO_ERROR {
            return;
        }
        // SAFETY: `context` is the `Found` `resolve` passed, live through the
        // blocking `DNSServiceProcessResult` that calls this, and the reply's TXT
        // is `txt_len` bytes live through the call.
        unsafe {
            *context.cast::<Found>() = Some((
                u16::from_be(port),
                std::slice::from_raw_parts(txt, usize::from(txt_len)).to_vec(),
            ));
        }
    }

    /// How long a resolve waits for its reply, which a host with no responder
    /// never sends.
    const RESOLVE_WITHIN_MS: i32 = 10_000;

    /// What the system resolves `instance` to, blocking up to
    /// [`RESOLVE_WITHIN_MS`] for it.
    fn resolve(instance: &str) -> Found {
        let name = CString::new(instance).expect("no NUL");
        let regtype = CString::new(SERVICE_TYPE).expect("no NUL");
        let mut found: Found = None;
        let mut sd_ref: ServiceRef = ptr::null_mut();
        // SAFETY: the strings and `found` live through the resolve, which ends
        // with the `Service` below, and `resolved` has the reply's C signature.
        let error = unsafe {
            DNSServiceResolve(
                &raw mut sd_ref,
                0,
                0,
                name.as_ptr(),
                regtype.as_ptr(),
                c"local.".as_ptr(),
                Some(resolved),
                (&raw mut found).cast(),
            )
        };
        assert_eq!(error, NO_ERROR);
        let service = Service(sd_ref);
        let mut reply = libc::pollfd {
            // SAFETY: the ref was just initialized.
            fd: unsafe { DNSServiceRefSockFD(service.0) },
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live `pollfd`.
        let ready = unsafe { libc::poll(&raw mut reply, 1, RESOLVE_WITHIN_MS) };
        assert_eq!(
            ready, 1,
            "{instance} did not resolve within {RESOLVE_WITHIN_MS} ms"
        );
        // SAFETY: a reply waits, so this returns without blocking, writing
        // `found` through `resolved`.
        assert_eq!(unsafe { DNSServiceProcessResult(service.0) }, NO_ERROR);
        drop(service);
        found
    }

    /// What a device reads off the network is the port given, in its order,
    /// and the pin's TXT string.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_announcement_resolves_to_its_port_and_pin() {
        let instance = format!("pixtuoid-test-{}", std::process::id());
        let port = 47_998;
        let announcing = tokio::spawn({
            let instance = instance.clone();
            async move { announce(&instance, port, "AB").await }
        });
        let found = tokio::task::spawn_blocking(move || resolve(&instance))
            .await
            .expect("it resolves");
        announcing.abort();
        let txt = txt_record(&super::super::txt("AB")).expect("short");
        assert_eq!(found, Some((port, txt)));
    }

    /// A withdrawal, then the renamed name's Add, leaves the service held; of
    /// the replies, only an error refuses it.
    #[test]
    fn only_a_replys_error_refuses_the_service() {
        // `kDNSServiceErr_ServiceNotRunning`, a daemon's crash.
        let not_running = -65_563;
        let replied = Cell::new(NO_ERROR);
        let context = ptr::from_ref(&replied).cast_mut().cast();
        let null = ptr::null();
        let reply = |flags, error, name: *const c_char| {
            // SAFETY: a live `Cell`, and a name, read only on success, live
            // through the call.
            unsafe { registered(ptr::null_mut(), flags, error, name, null, null, context) };
            held(replied.get())
        };
        assert!(reply(FLAGS_ADD, NO_ERROR, c"studio".as_ptr()).is_ok());
        assert!(reply(0, NO_ERROR, c"studio".as_ptr()).is_ok());
        assert!(reply(FLAGS_ADD, NO_ERROR, c"studio (2)".as_ptr()).is_ok());
        let refused = reply(FLAGS_ADD, not_running, null).expect_err("refused");
        assert!(
            refused.to_string().contains(&not_running.to_string()),
            "{refused}"
        );
    }

    #[test]
    fn a_txt_record_is_its_string_behind_a_length_byte() {
        assert_eq!(txt_record("spki=AB").expect("short"), b"\x07spki=AB");
        let max = usize::from(u8::MAX);
        assert_eq!(txt_record(&"A".repeat(max)).expect("fits").len(), max + 1);
        assert!(txt_record(&"A".repeat(max + 1)).is_err());
    }
}
