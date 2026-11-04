//! Windows' own mDNS responder, through dnsapi.dll's
//! [`DnsServiceRegister`](https://learn.microsoft.com/en-us/windows/win32/api/windns/nf-windns-dnsserviceregister)
//! (Windows 10 on), whose [callback](https://learn.microsoft.com/en-us/windows/win32/api/windns/nc-windns-dns_service_register_complete)
//! reports "the status of the registration" and which names no way a
//! registered service is lost, so `announce` holds one until dropped.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr;

use anyhow::{Context, Result, anyhow, bail};
use pixtuoid_core::remote::{SERVICE_TYPE, SPKI_KEY};
use tokio::sync::watch;
use windows_sys::Win32::Foundation::{DNS_REQUEST_PENDING, ERROR_SUCCESS};
use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_QUERY_REQUEST_VERSION1, DNS_SERVICE_INSTANCE, DNS_SERVICE_REGISTER_REQUEST,
    DnsServiceConstructInstance, DnsServiceDeRegister, DnsServiceFreeInstance, DnsServiceRegister,
};

/// Announce `instance` on `port`, pinned by `spki`, as `<instance>.<type>.local`
/// on `<host>.local`, until dropped. The forms follow the DNS_SERVICE_INSTANCE
/// page and Microsoft's [mDNS sample](https://github.com/microsoft/Xbox-GDK-Samples/blob/3acf5ffbe5cdb0d42db773a46bc083cc8b481233/Samples/Live/mDNS/mDNS.cpp),
/// which appends ".local" to the DNS host name: ".local must be appended in
/// order for mDNS to work".
pub(super) async fn announce(instance: &str, port: u16, spki: &str) -> Result<()> {
    let (registration, mut completed) = register(instance, port, spki)?;
    held(*completed.wait_for(|&status| status != PENDING).await?)?;
    tracing::info!("announced on the local network");
    std::future::pending::<()>().await;
    drop(registration);
    Ok(())
}

/// The status dnsapi gives a registration still in progress.
const PENDING: u32 = DNS_REQUEST_PENDING.cast_unsigned();

/// A registration's status, [`PENDING`] until its callback reports one.
type Completion = watch::Sender<u32>;

/// Whether the callback's `status` leaves the service held.
fn held(status: u32) -> Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(refused(status))
    }
}

fn refused(status: u32) -> anyhow::Error {
    anyhow!("Windows refused the service (status {status})")
}

fn register(instance: &str, port: u16, spki: &str) -> Result<(Registered, watch::Receiver<u32>)> {
    let host = whoami::hostname().context("no host name to announce")?;
    let service = wide(&format!("{instance}.{SERVICE_TYPE}.local"));
    let host = wide(&format!("{host}.local"));
    let (key, value) = (wide(SPKI_KEY), wide(spki));
    let keys = [key.as_ptr()];
    let values = [value.as_ptr()];
    // SAFETY: NUL-terminated wide strings and arrays as long as the count, live
    // through the call; the instance it returns is its own, freed by
    // DnsServiceFreeInstance. The port goes in host order, as the sample passes
    // it; Learn names no order.
    let built = unsafe {
        DnsServiceConstructInstance(
            service.as_ptr(),
            host.as_ptr(),
            ptr::null(),
            ptr::null(),
            port,
            0,
            0,
            u32::try_from(keys.len())?,
            keys.as_ptr(),
            values.as_ptr(),
        )
    };
    if built.is_null() {
        bail!("Windows built no service instance");
    }
    let (completion, completed) = watch::channel(PENDING);
    let completion = Box::new(completion);
    let request = Box::new(DNS_SERVICE_REGISTER_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1,
        InterfaceIndex: 0,
        pServiceInstance: built,
        pRegisterCompletionCallback: Some(registered),
        pQueryContext: ptr::from_ref::<Completion>(&completion).cast_mut().cast(),
        hCredentials: ptr::null_mut(),
        unicastEnabled: 0,
    });
    // SAFETY: the request, its instance and its context outlive the
    // registration: none is freed once it succeeds.
    let status = unsafe { DnsServiceRegister(&raw const *request, ptr::null_mut()) };
    if status != PENDING {
        // SAFETY: the instance came from DnsServiceConstructInstance, and a
        // refused request never reads it, or its context, again.
        unsafe { DnsServiceFreeInstance(built) };
        return Err(refused(status));
    }
    Box::leak(completion);
    Ok((Registered(ManuallyDrop::new(request)), completed))
}

/// A registration, deregistered on drop: Learn's Remarks, "To deregister the
/// service, call DnsServiceDeRegister". Its request, like the [`Completion`] its
/// context points at, is never freed: dnsapi reads both until a deregistration
/// completes, which a drop cannot wait for.
struct Registered(ManuallyDrop<Box<DNS_SERVICE_REGISTER_REQUEST>>);

// SAFETY: the request is read by dnsapi through the pointer each call is handed
// and by this owner's drop, so moving the box between threads moves nothing it
// points to.
unsafe impl Send for Registered {}

impl Drop for Registered {
    fn drop(&mut self) {
        self.0.pRegisterCompletionCallback = Some(deregistered);
        // SAFETY: the request registered the service.
        unsafe { DnsServiceDeRegister(&raw const **self.0, ptr::null_mut()) };
    }
}

/// `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// The responder's word on the registration, sent to `announce`. "If not
/// nullptr, then you are responsible for freeing the data using
/// DnsServiceFreeInstance."
unsafe extern "system" fn registered(
    status: u32,
    context: *const c_void,
    instance: *const DNS_SERVICE_INSTANCE,
) {
    // SAFETY: the context is the request's own `Completion`, never freed once
    // the registration succeeds.
    unsafe { &*context.cast::<Completion>() }.send_replace(status);
    free(instance);
}

/// The responder's word on the deregistration, the instance freed as above.
unsafe extern "system" fn deregistered(
    _status: u32,
    _context: *const c_void,
    instance: *const DNS_SERVICE_INSTANCE,
) {
    free(instance);
}

fn free(instance: *const DNS_SERVICE_INSTANCE) {
    if !instance.is_null() {
        // SAFETY: the callback hands this instance to the callee to free.
        unsafe { DnsServiceFreeInstance(instance) };
    }
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::Foundation::ERROR_TIMEOUT;

    use super::*;

    /// The status the callback reports reaches `announce`, which waits on
    /// nothing else, and only success holds the service.
    #[test]
    fn a_registration_status_reaches_its_waiter_and_only_success_holds() {
        let (completion, completed) = watch::channel(PENDING);
        // SAFETY: a live `Completion`, and no instance to free.
        unsafe {
            registered(
                ERROR_TIMEOUT,
                ptr::from_ref(&completion).cast(),
                ptr::null(),
            )
        };
        assert_eq!(*completed.borrow(), ERROR_TIMEOUT);
        assert!(held(ERROR_SUCCESS).is_ok());
        assert_eq!(
            held(ERROR_TIMEOUT).expect_err("refused").to_string(),
            format!("Windows refused the service (status {ERROR_TIMEOUT})")
        );
    }
}
