//! What the crate's Windows code shares: wide strings, known folders, and a
//! security descriptor built from SDDL for each object created under it (the
//! hook's pipe, the capture rig's private directory).

use std::ffi::OsStr;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_INFORMATION_CLASS,
    TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::OpenProcessToken;

/// `s` as the NUL-terminated wide string a Win32 call takes.
pub(crate) fn nul_terminated(s: &OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(Some(0)).collect()
}

/// `process`'s token information of `class`, `u64`-backed so a SID pointer in it
/// is aligned. The buffer owns that SID.
///
/// # Safety
///
/// `process` is a valid process handle for the call's duration.
pub(crate) unsafe fn token_info(
    process: HANDLE,
    class: TOKEN_INFORMATION_CLASS,
) -> Option<Vec<u64>> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is forwarded from this fn's contract; `token` is a live out-param.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    // Size probe (returns 0 + sets `len`), then the real read.
    let mut len: u32 = 0;
    // SAFETY: `token` is open; a null buffer of length 0 is the documented size probe.
    unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut len) };
    let blob = if len == 0 {
        None
    } else {
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `token` is open and `buf` spans at least `len` bytes for the call.
        let read =
            unsafe { GetTokenInformation(token, class, buf.as_mut_ptr().cast(), len, &mut len) };
        if read == 0 { None } else { Some(buf) }
    };
    // SAFETY: `token` is open, and this is its only close.
    unsafe { CloseHandle(token) };
    blob
}

/// The wide string a Win32 call wrote at `p`, up to its NUL.
///
/// # Safety
///
/// `p` points to a live NUL-terminated UTF-16 string.
pub(crate) unsafe fn wide_at(p: *const u16) -> std::ffi::OsString {
    use std::os::windows::ffi::OsStringExt;
    // SAFETY: the caller's contract: every unit up to the NUL is readable.
    let units =
        unsafe { std::slice::from_raw_parts(p, (0..).take_while(|&i| *p.add(i) != 0).count()) };
    std::ffi::OsString::from_wide(units)
}

/// A known folder's path, which "does not include a trailing backslash"
/// (learn.microsoft.com, SHGetKnownFolderPath).
///
/// # Errors
///
/// If the shell has no path for it.
pub(crate) fn known_folder(id: &windows_sys::core::GUID) -> std::io::Result<std::path::PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;

    let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `id` is a live GUID, a null token asks for the current user, and
    // `raw` is a live out-pointer.
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut raw) };
    // SAFETY: on success `raw` is the NUL-terminated path the call allocated.
    let path =
        (hr >= 0 && !raw.is_null()).then(|| std::path::PathBuf::from(unsafe { wide_at(raw) }));
    // SAFETY: the caller frees `raw` "whether SHGetKnownFolderPath succeeds or
    // not"; freeing null is a no-op.
    unsafe { CoTaskMemFree(raw.cast()) };
    path.ok_or_else(|| std::io::Error::from_raw_os_error(hr))
}

/// Held alive for as long as a create call may borrow its attributes.
pub(crate) struct SecurityDescriptor {
    psd: PSECURITY_DESCRIPTOR,
    attrs: SECURITY_ATTRIBUTES,
}

impl std::fmt::Debug for SecurityDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityDescriptor")
            .field("psd", &self.psd)
            .finish_non_exhaustive()
    }
}

// SAFETY: the descriptor is immutable after creation (the Win32 calls only
// read through these pointers) and freed exactly once in Drop; none of the
// APIs involved carry thread affinity, so moving the owner across threads
// (tokio::spawn of the listener task) is sound.
unsafe impl Send for SecurityDescriptor {}

impl SecurityDescriptor {
    /// The descriptor `sddl` spells.
    pub(crate) fn from_sddl(sddl: &str) -> std::io::Result<Self> {
        let wide = nul_terminated(OsStr::new(sddl));
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated and outlives the call, psd is a live
        // out-pointer, and the size out-param is documented optional (null allowed).
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            psd,
            attrs: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: psd,
                bInheritHandle: 0,
            },
        })
    }

    /// A create call's `lpSecurityAttributes`, valid while `self` lives; the
    /// kernel copies the descriptor during the call.
    pub(crate) fn attributes(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attrs
    }

    /// This descriptor's DACL as SDDL, rendered by the same converter that
    /// renders one read back off an object.
    #[cfg(test)]
    pub(crate) fn dacl(&self) -> String {
        test_support::dacl_of(self.psd)
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: psd was LocalAlloc'd by the SDDL conversion (documented
        // contract: caller frees with LocalFree) and is freed exactly once
        // here; no other reads can follow Drop.
        unsafe {
            LocalFree(self.psd);
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
        SE_OBJECT_TYPE,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    /// The DACL the kernel holds for `handle`, as SDDL.
    pub(crate) fn held_dacl(handle: HANDLE, kind: SE_OBJECT_TYPE) -> String {
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a live handle; every out-pointer this call doesn't fill is
        // null, as documented optional, and `psd` is a live out-pointer.
        let rc = unsafe {
            GetSecurityInfo(
                handle,
                kind,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut psd,
            )
        };
        assert_eq!(rc, 0, "GetSecurityInfo");
        let sddl = dacl_of(psd);
        // SAFETY: LocalAlloc'd by GetSecurityInfo, freed once here.
        unsafe { LocalFree(psd) };
        sddl
    }

    pub(super) fn dacl_of(psd: PSECURITY_DESCRIPTOR) -> String {
        let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: `psd` is a live descriptor; both out-pointers are live.
        let ok = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                psd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(
            ok, 0,
            "ConvertSecurityDescriptorToStringSecurityDescriptorW"
        );
        // SAFETY: `text` is the NUL-terminated string the conversion allocated.
        let sddl = unsafe { super::wide_at(text) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: LocalAlloc'd by the conversion, freed once here.
        unsafe { LocalFree(text.cast()) };
        sddl
    }
}
