//! Windows' own mDNS responder, through dnsapi.dll's
//! [`DnsServiceRegister`](https://learn.microsoft.com/en-us/windows/win32/api/windns/nf-windns-dnsserviceregister)
//! (Windows 10 on). "The registration is tied to the lifetime of the calling
//! process", so the process, not a drop, ends it.

use std::ffi::c_void;
use std::ptr;

use anyhow::{Context, Result, bail};
use windows_sys::Win32::Foundation::DNS_REQUEST_PENDING;
use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_QUERY_REQUEST_VERSION1, DNS_SERVICE_INSTANCE, DNS_SERVICE_REGISTER_REQUEST,
    DnsServiceConstructInstance, DnsServiceFreeInstance, DnsServiceRegister,
};

use super::SERVICE;

/// The service, for the rest of the process's life.
#[derive(Debug)]
pub(super) struct Registration;

/// Register `instance` on `port` with the one TXT string `txt`, as
/// `<instance>.<SERVICE>.local` on `<host>.local`, the forms the DNS_SERVICE_INSTANCE
/// page and Microsoft's mDNS sample use.
pub(super) async fn register(instance: &str, port: u16, txt: &str) -> Result<Registration> {
    let (key, value) = txt.split_once('=').context("a TXT string with no key")?;
    let host = whoami::hostname().context("no host name to announce")?;
    let service = wide(&format!("{instance}.{SERVICE}.local"));
    let host = wide(&format!("{host}.local"));
    let (key, value) = (wide(key), wide(value));
    let keys = [key.as_ptr()];
    let values = [value.as_ptr()];
    // SAFETY: NUL-terminated wide strings and one-entry arrays, live through the
    // call; the instance it returns is its own, freed by DnsServiceFreeInstance.
    // The port goes in host order, as Microsoft's sample passes it; Learn names
    // no order.
    let built = unsafe {
        DnsServiceConstructInstance(
            service.as_ptr(),
            host.as_ptr(),
            ptr::null(),
            ptr::null(),
            port,
            0,
            0,
            1,
            keys.as_ptr(),
            values.as_ptr(),
        )
    };
    if built.is_null() {
        bail!("Windows built no service instance");
    }
    let request = Box::new(DNS_SERVICE_REGISTER_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1,
        InterfaceIndex: 0,
        pServiceInstance: built,
        pRegisterCompletionCallback: Some(registered),
        pQueryContext: ptr::null_mut(),
        hCredentials: ptr::null_mut(),
        unicastEnabled: 0,
    });
    // SAFETY: the request and its instance live on: leaked below once the call
    // takes them, freed only if it doesn't.
    let status = unsafe { DnsServiceRegister(&raw const *request, ptr::null_mut()) };
    if status != DNS_REQUEST_PENDING.cast_unsigned() {
        // SAFETY: the instance came from DnsServiceConstructInstance, and a
        // refused request never reads it again.
        unsafe { DnsServiceFreeInstance(built) };
        bail!("Windows refused the service (status {status})");
    }
    Box::leak(request);
    Ok(Registration)
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
    if !instance.is_null() {
        // SAFETY: the callback hands this instance to the callee to free.
        unsafe { DnsServiceFreeInstance(instance) };
    }
}
