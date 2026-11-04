//! Windows' own mDNS responder, through dnsapi.dll's
//! [`DnsServiceRegister`](https://learn.microsoft.com/en-us/windows/win32/api/windns/nf-windns-dnsserviceregister)
//! (Windows 10 on), which names no way a registration is lost, so `announce`
//! holds it until dropped.

use std::ffi::c_void;
use std::ptr;

use anyhow::{Context, Result, bail};
use pixtuoid_core::remote::{SERVICE_TYPE, SPKI_KEY};
use windows_sys::Win32::Foundation::DNS_REQUEST_PENDING;
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
    let registered = register(instance, port, spki)?;
    std::future::pending::<()>().await;
    drop(registered);
    Ok(())
}

fn register(instance: &str, port: u16, spki: &str) -> Result<Registered> {
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
    let registered = Registered(Box::new(DNS_SERVICE_REGISTER_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1,
        InterfaceIndex: 0,
        pServiceInstance: built,
        pRegisterCompletionCallback: Some(registered),
        pQueryContext: ptr::null_mut(),
        hCredentials: ptr::null_mut(),
        unicastEnabled: 0,
    }));
    // SAFETY: the request and its instance outlive the registration: `Registered`
    // leaks them past its deregistration.
    let status = unsafe { DnsServiceRegister(&raw const *registered.0, ptr::null_mut()) };
    if status != DNS_REQUEST_PENDING.cast_unsigned() {
        std::mem::forget(registered);
        // SAFETY: the instance came from DnsServiceConstructInstance, and a
        // refused request never reads it again.
        unsafe { DnsServiceFreeInstance(built) };
        bail!("Windows refused the service (status {status})");
    }
    Ok(registered)
}

/// A registration, deregistered on drop: Learn's Remarks, "To deregister the
/// service, call DnsServiceDeRegister".
struct Registered(Box<DNS_SERVICE_REGISTER_REQUEST>);

// SAFETY: the request is read by dnsapi through the pointer each call is handed
// and by this owner's drop, so moving the box between threads moves nothing it
// points to.
unsafe impl Send for Registered {}

impl Drop for Registered {
    fn drop(&mut self) {
        self.0.pRegisterCompletionCallback = Some(deregistered);
        // SAFETY: the request registered the service; deregistering completes
        // later, so the request is leaked rather than freed.
        unsafe { DnsServiceDeRegister(&raw const *self.0, ptr::null_mut()) };
        Box::leak(std::mem::take(&mut self.0));
    }
}

/// `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// The responder's word on the registration. "If not nullptr, then you are
/// responsible for freeing the data using DnsServiceFreeInstance."
unsafe extern "system" fn registered(
    status: u32,
    _context: *const c_void,
    instance: *const DNS_SERVICE_INSTANCE,
) {
    if status == 0 {
        tracing::info!("announced on the local network");
    } else {
        tracing::warn!(status, "Windows dropped the service");
    }
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
