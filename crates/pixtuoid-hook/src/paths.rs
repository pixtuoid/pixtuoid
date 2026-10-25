//! The shim's socket-path resolution, in its own TEST-FREE file on purpose:
//! `pixtuoid-core/tests/socket_path_parity.rs` includes it via `#[path]` (source
//! inclusion, NOT a cargo dependency — the shim must stay free of pixtuoid-core
//! and vice versa) and pins it to the daemon's
//! `ClaudeCodeSource::default_socket_path`. Producer and consumer MUST agree or
//! hook events silently never arrive; if you move this file, fix that `#[path]`
//! rather than dropping the parity pin.

use std::path::PathBuf;

/// A PATH-valued env var read as BYTES. `env::var` errs on a value that is not
/// UTF-8 — legal for a Unix socket path — and would silently send the shim to a
/// DIFFERENT endpoint than the daemon bound, so no event ever arrives. The
/// shim cannot depend on `pixtuoid-core`, so this is a deliberate twin of
/// `platform::path_env`; `socket_path_parity.rs` pins the two resolvers equal.
fn path_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
}

pub(crate) fn default_socket_path() -> PathBuf {
    // Set-but-empty/whitespace = unset (the RUST_LOG policy): honored verbatim,
    // "" makes the daemon's bind fail fatally and the shim drop every event.
    if let Some(p) = path_env("PIXTUOID_SOCKET") {
        return p;
    }
    #[cfg(unix)]
    {
        // XDG spec: absolute-only. Empty → `/pixtuoid.sock` (fatal bind); relative →
        // shim/daemon cwd mis-rendezvous. Non-absolute is invalid → treated as unset.
        if let Some(dir) = path_env("XDG_RUNTIME_DIR")
            && dir.is_absolute()
        {
            return dir.join("pixtuoid.sock");
        }
        // No XDG_RUNTIME_DIR (macOS, bare Linux): a per-user subdir the daemon
        // creates 0700-owned-by-us, NOT a flat predictable /tmp name. A
        // co-located user could squat/lock a flat `pixtuoid-{uid}.sock` and
        // silently disable the hook plane (#485); a 0700 subdir they cannot
        // write into makes the daemon's bind fail loudly instead.
        // Safety: getuid is always safe on Unix.
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/pixtuoid-{uid}")).join("pixtuoid.sock")
    }
    #[cfg(windows)]
    {
        PathBuf::from(default_windows_pipe_name())
    }
}

/// The default hook pipe name `\\.\pipe\pixtuoid-{USERNAME}`, WITHOUT the
/// `PIXTUOID_SOCKET` override short-circuit. The security boundary is the
/// server-side DACL; the NAME is namespacing only. Backslashes are sanitized:
/// pipe names can't contain them, and enterprise boxes set USERNAME=DOMAIN\user.
/// The shim compares the resolved endpoint against this to SCOPE its #495
/// peer-cred check to our own predictable rendezvous — an explicit
/// `PIXTUOID_SOCKET` pipe stays the user's trust decision.
#[cfg(windows)]
pub(crate) fn default_windows_pipe_name() -> String {
    let user = std::env::var_os("USERNAME")
        .filter(|u| !u.is_empty())
        .and_then(|u| u.into_string().ok())
        .unwrap_or_else(|| "default".into())
        .replace('\\', "-");
    format!(r"\\.\pipe\pixtuoid-{user}")
}

/// The per-user tmp dir we OWN (`/tmp/pixtuoid-{uid}`) when `endpoint` is the
/// no-XDG `/tmp` FALLBACK — else `None`. PURE (no I/O), so it stays parity-safe.
/// SCOPES the shim's connected-peer-uid check (#485) to that fallback: the XDG /
/// explicit-override branches are systemd's / the user's trust decision, not
/// ours to police (an override may legitimately point at a cross-uid daemon).
#[cfg(unix)]
pub(crate) fn owned_tmp_socket_dir(endpoint: &std::path::Path) -> Option<PathBuf> {
    // Safety: getuid is always safe on Unix.
    let uid = unsafe { libc::getuid() };
    let owned = PathBuf::from(format!("/tmp/pixtuoid-{uid}"));
    (endpoint.parent() == Some(owned.as_path())).then_some(owned)
}
