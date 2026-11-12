//! macOS focus glue: `/bin/ps` for the ancestor walk + `NSRunningApplication` for
//! the focusable test and activation. Zero TCC permissions — `activate()` is plain
//! Cocoa, not an Apple Event.
//!
//! Glue that needs a real GUI session; the walk logic itself is tested in
//! `focus::tests` on mock tables.

use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication};

use super::ProcessTable;

pub(crate) struct OsProcessTable;

impl ProcessTable for OsProcessTable {
    fn ppid(&self, pid: i32) -> Option<i32> {
        // `ps` rather than proc_pidinfo FFI: the terminal chain runs through the
        // setuid-root `login` process and proc_pidinfo enforces same-user, so
        // EPERM there stopped the walk one hop short of the terminal app. ps
        // reads any pid via sysctl KERN_PROC; libc doesn't bind kinfo_proc, and a
        // hand-written 600-byte ABI struct for one field is a worse trade than one
        // short-lived child per hop on a click-driven path.
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    fn focusable(&self, pid: i32) -> bool {
        // A REGULAR activation policy = a real Dock app (the terminal);
        // shells/daemons have no NSRunningApplication or are Prohibited.
        // SAFETY: plain Cocoa class-method calls on valid arguments; objc2's
        // 0.2-generation bindings mark every msg-send unsafe.
        unsafe {
            NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                .is_some_and(|app| app.activationPolicy() == NSApplicationActivationPolicy::Regular)
        }
    }
}

/// Bring the app owning `pid` to the foreground. Returns whether macOS
/// accepted the request (a `false` is the caller's silent-no-op path).
pub(crate) fn activate_os(pid: i32) -> bool {
    // SAFETY: plain Cocoa calls on a valid pid; see `focusable`.
    unsafe {
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .map(|app| {
                #[expect(
                    deprecated,
                    reason = "deprecated on 14+ but still honored; the no-options activate() is cooperative and drops the request while the user interacts elsewhere"
                )]
                app.activateWithOptions(
                    objc2_app_kit::NSApplicationActivationOptions::NSApplicationActivateIgnoringOtherApps,
                )
            })
            .unwrap_or(false)
    }
}
