//! mDNSResponder's `DNSServiceRegister` ([dns_sd.h](https://github.com/apple-oss-distributions/mDNSResponder/blob/mDNSResponder-2881.120.11/mDNSShared/dns_sd.h)),
//! in libSystem, which std already links. Its replies arrive on a socket that a
//! task reads, since "the daemon may terminate its connection with a client that
//! does not process the daemon's responses".

use std::ffi::{CStr, CString, c_char, c_void};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::ptr;

use anyhow::{Context, Result, bail};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;

use super::SERVICE;

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

/// The service, until dropped: the task holding it then deallocates its ref,
/// which "will be deregistered".
#[derive(Debug)]
pub(super) struct Registration {
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Register `instance` on `port` with the one TXT string `txt`. The responder
/// renames the instance on a conflict, which no flag here turns off.
pub(super) async fn register(instance: &str, port: u16, txt: &str) -> Result<Registration> {
    let name = CString::new(instance).context("an instance name with a NUL")?;
    let regtype = CString::new(SERVICE)?;
    let record = txt_record(txt)?;
    let record_len = u16::try_from(record.len())?;
    let mut sd_ref: ServiceRef = ptr::null_mut();
    // SAFETY: every pointer lives through the call, which copies what it reads
    // ("the DNSServiceRegister call copies the txtRecord data"), and `registered`
    // has the reply's C signature. Port is in network byte order, as it asks.
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
            ptr::null_mut(),
        )
    };
    if error != NO_ERROR {
        bail!("mDNSResponder refused the service (error {error})");
    }
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
    let driver = Driver { socket, service };
    let (stop, stopped) = oneshot::channel();
    tokio::spawn(drive(driver, stopped));
    Ok(Registration { stop: Some(stop) })
}

/// A TXT record of one string: a length byte, then the string.
fn txt_record(txt: &str) -> Result<Vec<u8>> {
    let len = u8::try_from(txt.len()).context("a TXT string past 255 bytes")?;
    Ok([&[len], txt.as_bytes()].concat())
}

/// Hand each reply on the socket to `registered`, until `stopped` or the
/// responder goes away.
async fn drive(driver: Driver, mut stopped: oneshot::Receiver<()>) {
    loop {
        let mut ready = tokio::select! {
            _ = &mut stopped => return,
            ready = driver.socket.readable() => match ready {
                Ok(ready) => ready,
                Err(error) => {
                    tracing::warn!(error = ?error, "mDNSResponder's socket failed");
                    return;
                }
            },
        };
        match ready.try_io(|socket| socket.get_ref().reply_waits()) {
            Err(_no_reply) => continue,
            Ok(Err(error)) => {
                tracing::warn!(error = ?error, "mDNSResponder closed the service's socket");
                return;
            }
            Ok(Ok(())) => {}
        }
        // SAFETY: a reply waits, so this returns without blocking, and only this
        // task touches the ref.
        let error = unsafe { DNSServiceProcessResult(driver.service.0) };
        if error != NO_ERROR {
            tracing::warn!(error, "mDNSResponder's reply failed");
            return;
        }
    }
}

/// What `drive` owns, the socket first so that it leaves the runtime's poller
/// before the ref that closes it: "it should be removed BEFORE
/// DNSServiceRefDeallocate() is called".
struct Driver {
    socket: AsyncFd<Socket>,
    service: Service,
}

/// The registration's ref, deallocated on drop.
struct Service(ServiceRef);

// SAFETY: dns_sd.h "does no locking internally" and leaves serializing a ref's
// use to its client; only the one `drive` task ever uses this one.
unsafe impl Send for Service {}

impl Drop for Service {
    fn drop(&mut self) {
        // SAFETY: the ref is live, and nothing processes it once its task ends.
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

/// The responder's word on the service: announced, renamed on a conflict, or
/// lost.
unsafe extern "C" fn registered(
    _sd_ref: ServiceRef,
    flags: Flags,
    error: ErrorType,
    name: *const c_char,
    _regtype: *const c_char,
    _domain: *const c_char,
    _context: *mut c_void,
) {
    if error != NO_ERROR {
        tracing::warn!(error, "mDNSResponder dropped the service");
        return;
    }
    // SAFETY: on success the reply names the instance, NUL-terminated and live
    // through the call.
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    if flags & FLAGS_ADD == 0 {
        tracing::warn!(name = ?name, "mDNSResponder withdrew the service");
    } else {
        tracing::info!(name = ?name, "announced on the local network");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_txt_record_is_its_string_behind_a_length_byte() {
        assert_eq!(txt_record("spki=AB").expect("short"), b"\x07spki=AB");
        assert!(txt_record(&"A".repeat(256)).is_err());
    }
}
