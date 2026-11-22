//! Test-only stand-in process, and how long a test waits for an exit, for the
//! unit tests that watch, probe or mark a live pid. `pub(crate)`, so integration
//! tests (`tests/`) can't reach it.

/// How long a test waits for an exit to surface: a killed [`sleeper`]'s, or the
/// exit watch's own thread's. A liveness bound, not a latency claim: wide enough
/// to absorb `cargo llvm-cov` instrumentation and CI scheduling jitter.
pub(crate) const EXIT_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// A child that outlives any test's assertions; the test kills and reaps it.
/// `ping` on Windows, which ships no `sleep`, whose `timeout` refuses a
/// redirected stdin and whose `waitfor` is not on every SKU.
pub(crate) fn sleeper() -> std::process::Child {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("ping");
        c.args(["-n", "61", "127.0.0.1"]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new("sleep");
        c.arg("60");
        c
    };
    cmd.stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn a sleeper")
}
