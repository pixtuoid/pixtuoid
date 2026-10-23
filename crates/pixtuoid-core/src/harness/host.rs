//! What the capture rig asks of the host: a known folder as the shell resolves
//! it, a directory no other local user can write into, whether this process
//! owns a path, a lock on a file it owns, a removal that waits out a process
//! holding the tree, and whether a registry key exists.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(windows)]
use crate::win32::nul_terminated;

/// A Windows known folder, resolved by the shell: a variable the sandbox sets
/// cannot move it, but an environment cleared of the host's leaves it unresolved.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KnownFolder {
    /// `FOLDERID_ProgramData`.
    ProgramData,
    /// `FOLDERID_Profile`: `~` as the `dirs` crate resolves it on Windows,
    /// whatever `USERPROFILE` says (dirs `src/win.rs:5` @00511bdc25, through
    /// dirs-sys `src/lib.rs:172-173` @8bcd4aa2c3).
    Profile,
}

#[cfg(any(windows, test))]
impl KnownFolder {
    /// Every known folder a
    /// [`system_config`](crate::harness::hermetic::Profile::system_config) row
    /// may name.
    pub(crate) const ALL: [Self; 2] = [Self::ProgramData, Self::Profile];

    /// How a [`system_config`](crate::harness::hermetic::Profile::system_config)
    /// row spells it.
    pub(crate) const fn placeholder(self) -> &'static str {
        match self {
            Self::ProgramData => "{FOLDERID_ProgramData}",
            Self::Profile => "{FOLDERID_Profile}",
        }
    }
}

/// `folder`'s path, which "does not include a trailing backslash"
/// (learn.microsoft.com, SHGetKnownFolderPath).
///
/// # Errors
///
/// If the shell has no path for it.
#[cfg(windows)]
pub(crate) fn known_folder(folder: KnownFolder) -> std::io::Result<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Profile, FOLDERID_ProgramData, SHGetKnownFolderPath,
    };

    let id = match folder {
        KnownFolder::ProgramData => &FOLDERID_ProgramData,
        KnownFolder::Profile => &FOLDERID_Profile,
    };
    let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `id` is a live GUID, a null token asks for the current user, and
    // `raw` is a live out-pointer.
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut raw) };
    // SAFETY: on success `raw` is the NUL-terminated path the call allocated.
    let path =
        (hr >= 0 && !raw.is_null()).then(|| PathBuf::from(unsafe { crate::win32::wide_at(raw) }));
    // SAFETY: the caller frees `raw` "whether SHGetKnownFolderPath succeeds or
    // not"; freeing null is a no-op.
    unsafe { CoTaskMemFree(raw.cast()) };
    path.ok_or_else(|| std::io::Error::from_raw_os_error(hr))
}

/// `path` with a leading `{FOLDERID_*}` placeholder resolved, or `None` when
/// the shell has no such folder; any other path, and every path off Windows,
/// as written.
pub fn known_folders_expanded(path: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        for folder in KnownFolder::ALL {
            if let Some(rest) = path.strip_prefix(folder.placeholder()) {
                return Some(
                    known_folder(folder)
                        .ok()?
                        .join(rest.trim_start_matches('\\')),
                );
            }
        }
    }
    Some(PathBuf::from(path))
}

/// Owner, SYSTEM and Administrators in full, Users read and execute, nothing
/// inherited: a parent like ProgramData would otherwise pass every local user
/// its create right. Users must keep read: on windows-latest, codex's
/// unelevated sandbox token could not read the workspace without it.
#[cfg(windows)]
const PRIVATE_DIR: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)(A;OICI;FRFX;;;BU)";

/// Create `path`, one level, where no other local user can write, as one
/// call: refuses one that already exists.
///
/// # Errors
///
/// `AlreadyExists` for an existing entry, or the create's own failure.
pub fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

        let sd = crate::win32::SecurityDescriptor::from_sddl(PRIVATE_DIR)?;
        let wide = nul_terminated(path.as_os_str());
        // SAFETY: `wide` is NUL-terminated, and `sd` outlives the call, which
        // copies the descriptor onto the new directory.
        if unsafe { CreateDirectoryW(wide.as_ptr(), sd.attributes()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }
}

/// Whether this process owns the entry at `path`, a link judged as itself and
/// never by its target: a planted link to a directory of ours is still not
/// ours. False whenever that cannot be read.
pub fn owned_by_us(path: &Path) -> bool {
    #[cfg(windows)]
    {
        windows_owned(path).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path)
            .map(|m| m.uid() == rustix::process::getuid().as_raw())
            .unwrap_or(false)
    }
}

/// `path`'s lock, held until the returned file drops. A fixed name in a shared
/// directory is pre-plantable, so a link there is never followed, nothing is
/// truncated, and a file another user owns is refused. On Unix a new one is
/// ours alone, since whoever can open a file can lock it and stall every
/// capture; Windows gives it the directory's inherited ACL.
///
/// # Errors
///
/// The open's or the lock's own failure, or `PermissionDenied` for a file
/// that is not ours.
pub fn hold_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        open.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let lock = open.open(path)?;
    if !owned_by_us(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} is not ours", path.display()),
        ));
    }
    lock.lock()?;
    Ok(lock)
}

/// How long a removal waits for another process to let go of its tree.
pub const RELEASE_PATIENCE: Duration = Duration::from_secs(60);
/// How often it asks meanwhile.
const RELEASE_POLL: Duration = Duration::from_millis(250);

/// The error std's Windows `remove_dir_all` gives up on after a few yields
/// (`library/std/src/sys/fs/windows/remove_dir_all.rs:152-170`, `:188-191`
/// at 1.98.1), while a previous run's process can hold the workspace for
/// seconds (codex/subagent in actions run 39128433306).
#[cfg(windows)]
const SHARING_VIOLATION: i32 = windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32;

/// `remove`, retried while another process holds what it removes, for up
/// to `patience`; a tree already gone counts as removed.
///
/// # Errors
///
/// `remove`'s last failure.
pub fn remove_once_released(
    patience: Duration,
    mut remove: impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let deadline = Instant::now() + patience;
    loop {
        match remove() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) if held(&e) && Instant::now() < deadline => std::thread::sleep(RELEASE_POLL),
            done => return done,
        }
    }
}

#[cfg(windows)]
fn held(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(SHARING_VIOLATION)
}

#[cfg(not(windows))]
fn held(_: &std::io::Error) -> bool {
    false
}

/// The entry's owner against the SID this token gives each object it creates,
/// which may be one of its groups rather than its user (learn.microsoft.com,
/// TOKEN_OWNER).
#[cfg(windows)]
fn windows_owned(path: &Path) -> Option<bool> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        TOKEN_OWNER, TOKEN_QUERY, TokenOwner,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let wide = nul_terminated(path.as_os_str());
    // SAFETY: `wide` is NUL-terminated; the handle is closed below. The
    // reparse-point flag opens a link itself, never its target.
    let file = unsafe {
        CreateFileW(
            wide.as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if file == INVALID_HANDLE_VALUE {
        return None;
    }
    let owned = (|| {
        let mut owner: PSID = std::ptr::null_mut();
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `file` was opened with READ_CONTROL; the out-pointers are
        // live, and those this call doesn't fill are null, as documented optional.
        let rc = unsafe {
            GetSecurityInfo(
                file,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut psd,
            )
        };
        if rc != 0 {
            return None;
        }
        let mut token = std::ptr::null_mut();
        // SAFETY: the pseudo-handle needs no close; `token` is a live out-pointer.
        let matches =
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
                None
            } else {
                let mut len = 0u32;
                // SAFETY: a null buffer of length 0 is the documented size probe.
                unsafe {
                    GetTokenInformation(token, TokenOwner, std::ptr::null_mut(), 0, &mut len);
                }
                // `u64`-backed so the TOKEN_OWNER's SID pointer is aligned.
                let mut buf = vec![0u64; (len as usize).div_ceil(8)];
                // SAFETY: `token` is open and `buf` spans at least `len` bytes.
                let read = unsafe {
                    GetTokenInformation(token, TokenOwner, buf.as_mut_ptr().cast(), len, &mut len)
                };
                let matches = (read != 0).then(|| {
                    // SAFETY: a successful read filled `buf` with a TOKEN_OWNER whose
                    // SID points into `buf`, which outlives this borrow.
                    let ours = unsafe { (*buf.as_ptr().cast::<TOKEN_OWNER>()).Owner };
                    // SAFETY: both SIDs are valid for the call.
                    unsafe { EqualSid(owner, ours) != 0 }
                });
                // SAFETY: opened above, closed once.
                unsafe { CloseHandle(token) };
                matches
            };
        // SAFETY: LocalAlloc'd by GetSecurityInfo; `owner` pointed into it and is
        // not read again.
        unsafe { LocalFree(psd) };
        matches
    })();
    // SAFETY: opened above, closed once.
    unsafe { CloseHandle(file) };
    owned
}

/// A registry hive a CLI reads policy from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    /// `HKEY_LOCAL_MACHINE`.
    Hklm,
    /// `HKEY_CURRENT_USER`.
    Hkcu,
}

impl Hive {
    /// The hive's short name, the prefix a key is written under.
    #[cfg(windows)]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hklm => "HKLM",
            Self::Hkcu => "HKCU",
        }
    }
}

/// Whether `sub` (the 64-bit view) exists under `hive`. One the process may not
/// read counts as existing: a CLI under the same token could not read it
/// either, but the run would not be shown to be hermetic.
#[cfg(windows)]
pub fn registry_key_exists(hive: Hive, sub: &str) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegOpenKeyExW,
    };

    let hive = match hive {
        Hive::Hklm => HKEY_LOCAL_MACHINE,
        Hive::Hkcu => HKEY_CURRENT_USER,
    };
    let wide = nul_terminated(std::ffi::OsStr::new(sub));
    let mut opened: HKEY = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated and `opened` a live out-pointer.
    let rc = unsafe { RegOpenKeyExW(hive, wide.as_ptr(), 0, KEY_READ, &mut opened) };
    if rc == ERROR_SUCCESS {
        // SAFETY: opened above, closed once.
        unsafe { RegCloseKey(opened) };
    }
    rc != ERROR_FILE_NOT_FOUND
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_waits_for_its_holder_and_truncates_nothing() {
        let d = tempfile::tempdir().expect("tempdir");
        let path = d.path().join("the.lock");
        std::fs::write(&path, "kept").expect("write");
        let held = hold_lock(&path).expect("the lock");
        let other = std::fs::File::open(&path).expect("the lock file");
        assert!(other.try_lock().is_err(), "a second holder waits");
        drop(held);
        // Read before `other` locks: Windows refuses a read past any handle's lock.
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "kept");
        assert!(other.try_lock().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_new_lock_is_ours_alone() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().expect("tempdir");
        let path = d.path().join("the.lock");
        drop(hold_lock(&path).expect("the lock"));
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// `/dev/null` is no link and anyone may open it, so only the owner check
    /// refuses it.
    #[cfg(unix)]
    #[test]
    fn a_file_another_user_owns_is_refused() {
        if rustix::process::getuid().is_root() {
            return;
        }
        let refused = hold_lock(Path::new("/dev/null")).expect_err("root's");
        assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_link_is_never_followed() {
        let d = tempfile::tempdir().expect("tempdir");
        let victim = d.path().join("victim");
        std::fs::write(&victim, "kept").expect("write");
        let path = d.path().join("the.lock");
        std::os::unix::fs::symlink(&victim, &path).expect("symlink");
        assert!(hold_lock(&path).is_err());
        assert_eq!(std::fs::read_to_string(&victim).expect("read"), "kept");
    }

    /// Busy the way a held tree is on this platform.
    fn busy() -> std::io::Error {
        #[cfg(windows)]
        return std::io::Error::from_raw_os_error(SHARING_VIOLATION);
        #[cfg(not(windows))]
        std::io::Error::from(std::io::ErrorKind::ResourceBusy)
    }

    #[test]
    fn a_held_tree_is_retried_only_on_windows() {
        let mut calls = 0;
        let removed = remove_once_released(Duration::from_secs(5), || {
            calls += 1;
            if calls < 3 { Err(busy()) } else { Ok(()) }
        });
        if cfg!(windows) {
            assert!(removed.is_ok() && calls == 3, "{removed:?} after {calls}");
        } else {
            assert!(removed.is_err() && calls == 1, "{removed:?} after {calls}");
        }
    }

    #[test]
    fn a_tree_held_past_the_patience_fails() {
        let mut calls = 0;
        let removed = remove_once_released(Duration::ZERO, || {
            calls += 1;
            Err(busy())
        });
        assert!(removed.is_err() && calls == 1, "{removed:?} after {calls}");
    }

    #[cfg(windows)]
    #[test]
    fn a_tree_refused_for_another_reason_fails_at_once() {
        let mut calls = 0;
        let removed = remove_once_released(Duration::from_secs(5), || {
            calls += 1;
            Err(std::io::Error::from_raw_os_error(
                windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32,
            ))
        });
        assert!(removed.is_err() && calls == 1, "{removed:?} after {calls}");
    }

    #[test]
    fn an_absent_tree_counts_as_removed() {
        let mut calls = 0;
        let removed = remove_once_released(Duration::ZERO, || {
            calls += 1;
            Err(std::io::ErrorKind::NotFound.into())
        });
        assert!(removed.is_ok() && calls == 1, "{removed:?} after {calls}");
    }

    #[test]
    fn a_private_dir_is_made_once_and_never_reused() {
        let d = tempfile::tempdir().expect("tempdir");
        let dir = d.path().join("base");
        create_private_dir(&dir).expect("create");
        assert!(owned_by_us(&dir));
        let again = create_private_dir(&dir).expect_err("it exists");
        assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);
    }

    /// Judged on the link itself: our link to a directory that is not ours is
    /// ours, and following it would have read the target's owner instead.
    #[test]
    fn ownership_is_judged_on_the_entry_itself_never_its_target() {
        let d = tempfile::tempdir().expect("tempdir");
        assert!(owned_by_us(d.path()));
        assert!(!owned_by_us(&d.path().join("missing")));
        let link = d.path().join("link");
        #[cfg(unix)]
        let target = PathBuf::from("/usr");
        #[cfg(windows)]
        let target = system32();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        #[cfg(windows)]
        {
            // A junction: unlike a symlink, it needs no privilege.
            let made = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&link)
                .arg(&target)
                .status()
                .expect("mklink");
            assert!(made.success());
        }
        assert!(owned_by_us(&link), "the link is ours, its target is not");
    }

    #[cfg(unix)]
    #[test]
    fn a_private_dir_is_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().expect("tempdir");
        let dir = d.path().join("base");
        create_private_dir(&dir).expect("create");
        let mode = std::fs::metadata(&dir).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    /// The DACL read back off the directory: a create that dropped the
    /// descriptor would read ProgramData's inherited ACEs here.
    #[cfg(windows)]
    #[test]
    fn a_private_dir_holds_exactly_its_protected_dacl() {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

        let d = tempfile::tempdir().expect("tempdir");
        let dir = d.path().join("base");
        create_private_dir(&dir).expect("create");
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&dir)
            .expect("open the directory");
        let held = crate::win32::test_support::held_dacl(handle.as_raw_handle(), SE_FILE_OBJECT);
        let meant = crate::win32::SecurityDescriptor::from_sddl(PRIVATE_DIR)
            .expect("sddl")
            .dacl();
        assert_eq!(held, meant);
        assert!(held.starts_with("D:P"), "protected: {held}");
    }

    #[cfg(windows)]
    fn system32() -> PathBuf {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot");
        PathBuf::from(root).join("System32")
    }

    #[cfg(windows)]
    #[test]
    fn program_data_is_read_from_the_shell_not_its_variable() {
        let real = known_folder(KnownFolder::ProgramData).expect("ProgramData");
        let elsewhere = tempfile::tempdir().expect("tempdir");
        temp_env::with_var("ProgramData", Some(elsewhere.path()), || {
            assert_eq!(known_folder(KnownFolder::ProgramData).expect("again"), real);
        });
    }

    #[test]
    fn a_known_folder_placeholder_is_resolved_where_the_shell_puts_it() {
        #[cfg(windows)]
        for folder in KnownFolder::ALL {
            let row = format!(r"{}\pixtuoid-row", folder.placeholder());
            assert_eq!(
                known_folders_expanded(&row),
                Some(known_folder(folder).expect("folder").join("pixtuoid-row"))
            );
        }
        assert_eq!(
            known_folders_expanded("/etc/pixtuoid-row"),
            Some(PathBuf::from("/etc/pixtuoid-row")),
            "any other path as written"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_registry_key_exists_only_while_it_does() {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_VOLATILE, RegCloseKey, RegCreateKeyExW,
            RegDeleteKeyW,
        };

        let sub = format!(r"Software\pixtuoid-test-{}", std::process::id());
        assert!(!registry_key_exists(Hive::Hkcu, &sub));
        let wide = nul_terminated(std::ffi::OsStr::new(&sub));
        let mut made: HKEY = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; every other pointer is a live
        // out-pointer or documented optional null.
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_VOLATILE,
                KEY_WRITE,
                std::ptr::null(),
                &mut made,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, ERROR_SUCCESS);
        // SAFETY: created above, closed once.
        unsafe { RegCloseKey(made) };
        assert!(registry_key_exists(Hive::Hkcu, &sub));
        // SAFETY: `wide` is NUL-terminated.
        let deleted = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, wide.as_ptr()) };
        assert_eq!(deleted, ERROR_SUCCESS);
        assert!(!registry_key_exists(Hive::Hkcu, &sub));
        assert!(
            registry_key_exists(Hive::Hklm, r"SAM\SAM"),
            "SYSTEM's alone: unreadable here, so counted"
        );
    }
}
