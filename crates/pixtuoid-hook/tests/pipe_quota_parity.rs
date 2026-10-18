//! Pins the shim's stdin-cap arithmetic AGAINST the daemon's Windows
//! named-pipe in-buffer quota, cross-crate: the shim caps stdin so a stamped
//! wire line always fits `IN_BUFFER_SIZE` and its sync write can't stall on
//! quota (stall → the `WRITE_TIMEOUT` watchdog → the event is silently dropped).
//!
//! The daemon's source can't be `#[path]`-included here — windows.rs needs
//! tokio/windows-sys and the shim must stay dependency-free — so this pins the
//! DEFINITION LINES textually instead. Deliberately fail-loud on a
//! rename/move too: a drift guard that silently stops guarding is worse than
//! one that asks to be re-pointed.
//!
//! Workspace-only (reads a sibling crate's source): excluded from the
//! published tarball via this crate's Cargo.toml `exclude`.

const SHIM_MAIN: &str = include_str!("../src/main.rs");
const DAEMON_WINDOWS: &str = include_str!("../../pixtuoid-core/src/source/hook/windows.rs");
const DAEMON_HOOK: &str = include_str!("../../pixtuoid-core/src/source/hook/mod.rs");

#[test]
fn shim_stdin_cap_and_daemon_pipe_quota_stay_in_lockstep() {
    assert!(
        SHIM_MAIN.contains("const STAMP_HEADROOM: u64 = 256;"),
        "shim STAMP_HEADROOM definition changed/moved — re-check the daemon's \
         IN_BUFFER_SIZE (pixtuoid-core source/hook/windows.rs) still covers \
         STDIN_CAP + STAMP_HEADROOM, then update this pin"
    );
    assert!(
        SHIM_MAIN.contains("const STDIN_CAP: u64 = (1 << 20) - STAMP_HEADROOM;"),
        "shim STDIN_CAP definition changed/moved — re-check it still equals \
         the daemon's IN_BUFFER_SIZE minus STAMP_HEADROOM, then update this pin"
    );
    assert!(
        DAEMON_WINDOWS.contains("const IN_BUFFER_SIZE: u32 = 1 << 20;"),
        "daemon IN_BUFFER_SIZE definition changed/moved — re-check the shim's \
         STDIN_CAP + STAMP_HEADROOM still equals it, then update this pin"
    );
    assert_eq!(
        const_product(DAEMON_HOOK, "const MAX_CONN_BYTES: u64 = "),
        2 << 20,
        "daemon MAX_CONN_BYTES must stay twice STDIN_CAP + STAMP_HEADROOM"
    );
}

/// The value of a `<prefix> a * b * …;` definition line whose factors are
/// integer literals.
fn const_product(src: &str, prefix: &str) -> u64 {
    let line = src
        .lines()
        .find_map(|l| {
            let l = l.trim();
            l.strip_prefix("pub(crate) ")
                .unwrap_or(l)
                .strip_prefix(prefix)
        })
        .unwrap_or_else(|| panic!("`{prefix}` definition changed/moved — re-point this pin"));
    line.trim_end_matches(';')
        .split('*')
        .map(|f| f.trim().parse::<u64>().expect("an integer-literal factor"))
        .product()
}
