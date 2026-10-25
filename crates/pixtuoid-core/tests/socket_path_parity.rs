//! Pins the hook shim's socket path EQUAL to the daemon's, branch by branch.
//!
//! The shim (producer) and `ClaudeCodeSource` (consumer) each compute the
//! default socket path independently — they MUST agree or hook events silently
//! never arrive. Each crate unit-tests its own branches against the same
//! literals, but two parallel literal pins only hold if a reviewer notices the
//! sibling; this compares the two implementations DIRECTLY (#93).
//!
//! The shim source is included via `#[path]`, NOT a cargo dependency, because
//! the hook crate must stay free of pixtuoid-core (see `pixtuoid-hook/src/paths.rs`).

#[path = "../../pixtuoid-hook/src/paths.rs"]
mod hook_paths;

use std::path::PathBuf;

use pixtuoid_core::source::claude_code::ClaudeCodeSource;

fn both() -> (PathBuf, PathBuf) {
    (
        hook_paths::default_socket_path(),
        ClaudeCodeSource::default_socket_path(),
    )
}

#[cfg(unix)]
#[test]
fn shim_and_daemon_resolve_identical_socket_paths_in_all_three_branches() {
    temp_env::with_var("XDG_RUNTIME_DIR", Some("/run/user/7"), || {
        let (shim, daemon) =
            temp_env::with_var("PIXTUOID_SOCKET", Some("/explicit/parity.sock"), both);
        assert_eq!(shim, daemon, "PIXTUOID_SOCKET branch diverged");
        assert_eq!(shim, PathBuf::from("/explicit/parity.sock"));

        // Set-but-empty PIXTUOID_SOCKET = unset on BOTH sides.
        let (shim, daemon) = temp_env::with_var("PIXTUOID_SOCKET", Some(""), both);
        assert_eq!(shim, daemon, "empty PIXTUOID_SOCKET branch diverged");
        assert_eq!(shim, PathBuf::from("/run/user/7/pixtuoid.sock"));
        let (shim, daemon) = temp_env::with_var("PIXTUOID_SOCKET", Some("   "), both);
        assert_eq!(shim, daemon, "whitespace PIXTUOID_SOCKET branch diverged");
        assert_eq!(shim, PathBuf::from("/run/user/7/pixtuoid.sock"));

        let (shim, daemon) = temp_env::with_var_unset("PIXTUOID_SOCKET", both);
        assert_eq!(shim, daemon, "XDG_RUNTIME_DIR branch diverged");
        assert_eq!(shim, PathBuf::from("/run/user/7/pixtuoid.sock"));
    });

    temp_env::with_var_unset("PIXTUOID_SOCKET", || {
        // XDG_RUNTIME_DIR is absolute-only per spec, so an empty/relative value must be
        // ignored — never `/pixtuoid.sock` or a cwd-relative path.
        // Safety: getuid is always safe on Unix.
        let uid = unsafe { libc::getuid() };
        let tmp_fallback = PathBuf::from(format!("/tmp/pixtuoid-{uid}/pixtuoid.sock"));
        for invalid in ["", "   ", "relative/run"] {
            let (shim, daemon) = temp_env::with_var("XDG_RUNTIME_DIR", Some(invalid), both);
            assert_eq!(shim, daemon, "invalid XDG_RUNTIME_DIR {invalid:?} diverged");
            assert_eq!(
                shim, tmp_fallback,
                "invalid XDG_RUNTIME_DIR {invalid:?} must fall to the /tmp subdir"
            );
        }

        // The /tmp fallback is a per-user 0700 SUBDIR, not a flat squattable
        // `pixtuoid-{uid}.sock` (#485).
        let (shim, daemon) = temp_env::with_var_unset("XDG_RUNTIME_DIR", both);
        assert_eq!(shim, daemon, "/tmp-uid fallback branch diverged");
        assert_eq!(
            shim,
            PathBuf::from(format!("/tmp/pixtuoid-{uid}/pixtuoid.sock"))
        );
        // The shim's pre-connect ownership guard derives its owned-dir from that same
        // fallback endpoint.
        assert_eq!(
            hook_paths::owned_tmp_socket_dir(&shim),
            Some(PathBuf::from(format!("/tmp/pixtuoid-{uid}"))),
        );
    });
}

#[cfg(windows)]
#[test]
fn shim_and_daemon_resolve_identical_pipe_names_in_all_branches() {
    // PIXTUOID_SOCKET is a pipe name on Windows.
    let (shim, daemon) =
        temp_env::with_var("PIXTUOID_SOCKET", Some(r"\\.\pipe\parity-explicit"), both);
    assert_eq!(shim, daemon, "PIXTUOID_SOCKET branch diverged");
    assert_eq!(shim, PathBuf::from(r"\\.\pipe\parity-explicit"));

    temp_env::with_var("USERNAME", Some("parity"), || {
        // Set-but-empty PIXTUOID_SOCKET = unset on BOTH sides.
        let (shim, daemon) = temp_env::with_var("PIXTUOID_SOCKET", Some(""), both);
        assert_eq!(shim, daemon, "empty PIXTUOID_SOCKET branch diverged");
        assert_eq!(shim, PathBuf::from(r"\\.\pipe\pixtuoid-parity"));
        let (shim, daemon) = temp_env::with_var("PIXTUOID_SOCKET", Some("   "), both);
        assert_eq!(shim, daemon, "whitespace PIXTUOID_SOCKET branch diverged");
        assert_eq!(shim, PathBuf::from(r"\\.\pipe\pixtuoid-parity"));

        let (shim, daemon) = temp_env::with_var_unset("PIXTUOID_SOCKET", both);
        assert_eq!(shim, daemon, "USERNAME default branch diverged");
        assert_eq!(shim, PathBuf::from(r"\\.\pipe\pixtuoid-parity"));
    });

    temp_env::with_var_unset("PIXTUOID_SOCKET", || {
        // Backslashes are illegal in pipe names, and enterprise boxes set
        // USERNAME=DOMAIN\user — both sides must sanitize identically.
        let (shim, daemon) = temp_env::with_var("USERNAME", Some(r"CORP\alice"), both);
        assert_eq!(shim, daemon, "USERNAME sanitize branch diverged");
        assert_eq!(shim, PathBuf::from(r"\\.\pipe\pixtuoid-CORP-alice"));

        let (shim, daemon) = temp_env::with_var_unset("USERNAME", both);
        assert_eq!(shim, daemon, "USERNAME-absent fallback branch diverged");
        assert_eq!(shim, PathBuf::from(r"\\.\pipe\pixtuoid-default"));
        let (shim, daemon) = temp_env::with_var("USERNAME", Some(""), both);
        assert_eq!(shim, daemon, "empty USERNAME branch diverged");
        assert_eq!(shim, PathBuf::from(r"\\.\pipe\pixtuoid-default"));
    });
}
