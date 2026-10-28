//! Process-level contracts of the REAL `pixtuoid` binary — anything that only
//! holds once clap, config resolution, and the runtime are wired together, which
//! no in-process test can reach. Keep new tests to that bar rather than letting
//! this become a general binary-test dumping ground.
//!
//! Determinism: the goldens are a function of the REGISTRY, not of what's
//! installed on the test machine — SO LONG AS the environment is fully isolated,
//! which is why every arm clears the env and points HOME at an empty tempdir.
//! Every row is then `connected: false` (no hooks installed, no flag set), so
//! `cli_present` is the only field that varies,
//! and it splits on registry shape: a target-bearing source probes absent in the
//! empty HOME → `false`, a no-target one has no target to probe → `true`.
//! Unix-only: the Windows home-var isolation differs and can't be verified here.
#![cfg(unix)]

mod common;

#[test]
fn sources_json_lists_every_source_in_an_isolated_home() {
    let home = tempfile::tempdir().expect("tempdir");

    let output = common::isolated(&["sources", "--json"], home.path())
        .output()
        .expect("run pixtuoid sources --json");

    assert!(
        output.status.success(),
        "sources --json exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    // A `.json` golden compares structurally (key-order-insensitive), so a serde
    // field reorder doesn't churn it; update with `SNAPSHOTS=overwrite`.
    let golden = snapbox::Data::read_from(
        std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/snapshots/cli/sources.json"
        )),
        None,
    );
    snapbox::assert_data_eq!(stdout, golden.raw());
}

/// The `--json` DELIVERY contract, not just its row shape: a FAILING
/// `connect`/`disconnect` still prints the `OutcomeRow` array to STDOUT and
/// exits NON-ZERO, so a `$?`-checking caller (Raycast recovers the rows from a
/// rejected `execFile` via `stdout.startsWith("[")`) gets BOTH the per-source
/// detail and a real error signal. Invisible to the row-shape schema goldens.
#[test]
fn a_failing_connect_emits_the_outcome_rows_and_exits_nonzero() {
    let home = tempfile::tempdir().expect("tempdir");
    // Block claude-code's hook install deterministically: its plugin dir as a
    // regular FILE makes writing the hooks file error, while the pixtuoid config
    // beside it still writes — so connect reaches the install step, fails it,
    // and rolls the flag back.
    let pixtuoid_config = home.path().join(".config").join("pixtuoid");
    std::fs::create_dir_all(&pixtuoid_config).expect("pixtuoid config dir");
    std::fs::write(pixtuoid_config.join("claude-plugin"), b"not a directory")
        .expect("seed the plugin dir as a file");

    let output = common::isolated(&["connect", "claude-code", "--json"], home.path())
        .output()
        .expect("run pixtuoid connect --json");

    assert!(
        !output.status.success(),
        "a failing connect must exit non-zero (the $?-checking caller's signal); stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("failing connect must still print the OutcomeRow array to stdout: {e}: {stdout:?}")
    });
    assert_eq!(
        rows.len(),
        1,
        "exactly one OutcomeRow per requested id: {rows:?}"
    );
    assert_eq!(
        rows[0]["id"], "claude-code",
        "the row names the requested id"
    );
    assert_eq!(
        rows[0]["outcome"], "failed",
        "a blocked install surfaces as `failed`, never a clean success: {rows:?}"
    );
}

/// The gate's own announcement, the constant message `runtime/gate.rs` logs.
/// Paired by literal because an integration test cannot see the binary's
/// internals; the negative arm below is what fails if the two drift apart.
const GATE_DROP_MSG: &str = "dropping events: source not connected";

/// Everything a caller needs to tell "the gate kept the scene empty" apart from
/// "the replay never happened" — an assertion of ABSENCE is only evidence when
/// the thing that should have produced presence demonstrably reached the code.
struct Replay {
    out: String,
    /// The child's stderr, which carries the gate's announcement. It must not
    /// share `out`'s file: two handles on one `NamedTempFile` keep independent
    /// offsets and overwrite each other.
    err: String,
    /// Whether the fixture is readable under the WATCHED sessions root.
    fixture_landed: bool,
}

/// `Child::drop` neither kills nor waits, so any panic between spawn and the
/// explicit kill would orphan a `run --headless` loop — which exits only on
/// a quit signal or when its reader leaves — reparented to init and writing to an already-deleted temp file.
struct Reaped(std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run a headless pixtuoid against an isolated everything and drip a real Codex
/// rollout into its sessions root. The ids `pixtuoid connect` installs first are
/// the ONLY difference between the two arms below, so any behavioural difference
/// is attributable to them alone — which is why the log level is fixed here rather
/// than varied per arm.
fn headless_replay(connect: &[&str], budget: std::time::Duration) -> Replay {
    use std::io::Write;

    // Read the fixture BEFORE spawning, so a read panic stays on the near side
    // of the kill.
    let capture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../pixtuoid-core/tests/sources/codex/fixtures/permission-hermetic");
    let fixture = pixtuoid_core::harness::transcript_in("codex", &capture)
        .unwrap_or_else(|found| panic!("one codex rollout, found {found:?}"));
    let body = std::fs::read_to_string(&fixture).expect("committed permission-hermetic fixture");

    let home = tempfile::tempdir().expect("home");
    let cfg = tempfile::tempdir().expect("config");
    let sessions = home.path().join(".codex/sessions");
    std::fs::create_dir_all(&sessions).expect("sessions");
    std::fs::create_dir_all(home.path().join(".claude/projects")).expect("projects");
    let out = tempfile::NamedTempFile::new().expect("stdout file");
    let err = tempfile::NamedTempFile::new().expect("stderr file");

    for id in connect {
        // The rollout rides the transcript, so the shim the hooks name never runs.
        let done = common::isolated(&["connect", id], home.path())
            .env("XDG_CONFIG_HOME", cfg.path())
            .env("PIXTUOID_HOOK", "/usr/bin/true")
            .output()
            .expect("run pixtuoid connect");
        assert!(
            done.status.success(),
            "connect {id}: {}{}",
            String::from_utf8_lossy(&done.stdout),
            String::from_utf8_lossy(&done.stderr)
        );
    }

    // Isolate the SOCKET too, and not merely for hygiene: on the default socket
    // a live CC session's hook traffic on the developer's machine lands in this
    // run's scene, and the negative arm would see a sprite unrelated to Codex.
    let sock = home.path().join("hook.sock");

    // `debug` is required, not incidental: the gate's announcement is the only
    // observable proof of WHY a scene stayed empty.
    let child = common::isolated(&["run", "--headless"], home.path())
        .args(["--log-level", "debug"])
        .env("XDG_CONFIG_HOME", cfg.path())
        .env("PIXTUOID_SOCKET", &sock)
        .stdout(out.reopen().expect("stdout handle"))
        .stderr(err.reopen().expect("stderr handle"))
        .spawn()
        .expect("spawn pixtuoid run --headless");
    let mut child = Reaped(child);

    // Startup slack for the watcher to bind, not a path selector: the rollout is
    // picked up whether it arrives as an append or is already present.
    std::thread::sleep(std::time::Duration::from_millis(200));

    let landed =
        sessions.join("rollout-2026-01-01T00-00-00-0a0a0a0a-0b0b-0c0c-0d0d-0e0e0e0e0e0e.jsonl");
    let mut f = std::fs::File::create(&landed).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    f.sync_all().unwrap();
    drop(f);

    // Poll until the run has DECIDED, so both arms exit on a POSITIVE signal
    // rather than waiting out a fixed timeout to conclude an absence. `if let
    // Ok` rather than `unwrap_or_default`: a torn read mid-write must not blank
    // an already-good buffer.
    let deadline = std::time::Instant::now() + budget;
    let (mut seen_out, mut seen_err) = (String::new(), String::new());
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(s) = std::fs::read_to_string(out.path()) {
            seen_out = s;
        }
        if let Ok(s) = std::fs::read_to_string(err.path()) {
            seen_err = s;
        }
        // Each arm waits for ALL the evidence it will assert: the gate announces
        // the drop before the summary loop has printed its first line, so
        // breaking on the announcement alone returns an empty stdout and reds
        // the very assertion it was meant to satisfy.
        let rendered = seen_out.contains("cx·");
        let gated = seen_err.contains(GATE_DROP_MSG) && seen_out.contains("agents=[");
        if rendered || gated {
            break;
        }
    }

    let _ = child.0.kill();
    let _ = child.0.wait();
    Replay {
        out: seen_out,
        err: seen_err,
        fixture_landed: landed.metadata().is_ok_and(|m| m.len() > 0),
    }
}

#[test]
fn connected_codex_rollout_becomes_a_sprite() {
    let r = headless_replay(&["codex"], std::time::Duration::from_secs(20));
    assert!(
        r.fixture_landed,
        "the fixture must reach the watched sessions root\nstderr:\n{}",
        r.err
    );
    assert!(
        r.out.contains("cx·"),
        "a connected Codex rollout must render a cx· sprite\nstdout:\n{}\nstderr:\n{}",
        r.out,
        r.err
    );
    assert!(
        !r.err.contains(GATE_DROP_MSG),
        "a CONNECTED source must not be announced as gated\nstderr:\n{}",
        r.err
    );
}

/// The same rollout with codex unconnected renders nothing — and, crucially, for
/// the RIGHT reason. Asserting only the absence would be satisfied by any
/// breakage that stopped the rollout reaching the watcher at all, so what this
/// pins is the gate's own announcement plus the fixture-landed check.
#[test]
fn disconnected_codex_rollout_is_dropped_by_the_gate() {
    let r = headless_replay(&[], std::time::Duration::from_secs(20));
    assert!(
        r.fixture_landed,
        "the fixture must reach the watched sessions root\nstderr:\n{}",
        r.err
    );
    assert!(
        r.err.contains(GATE_DROP_MSG) && r.err.contains("codex"),
        "the gate must announce dropping codex's events\nstderr:\n{}",
        r.err
    );
    assert!(
        !r.out.contains("cx·"),
        "a DISCONNECTED Codex rollout must render no sprite\nstdout:\n{}",
        r.out
    );
    assert!(
        r.out.contains("agents=[]"),
        "the run must still be alive and reporting an empty scene\nstdout:\n{}",
        r.out
    );
}

/// A quit signal ends a headless run through its unwind (the loop logs its
/// shutdown) and then the kernel's own verdict: the process dies BY the signal,
/// so a parent shell reads an interrupt rather than a clean exit.
#[test]
fn a_quit_signal_unwinds_a_headless_run_then_ends_it_by_that_signal() {
    use std::os::unix::process::ExitStatusExt;

    let home = tempfile::tempdir().expect("home");
    let out = tempfile::NamedTempFile::new().expect("stdout file");
    let err = tempfile::NamedTempFile::new().expect("stderr file");
    let child = common::isolated(&["run", "--headless"], home.path())
        .args(["--log-level", "info"])
        .env("PIXTUOID_SOCKET", home.path().join("hook.sock"))
        .stdout(out.reopen().expect("stdout handle"))
        .stderr(err.reopen().expect("stderr handle"))
        .spawn()
        .expect("spawn pixtuoid run --headless");
    let mut child = Reaped(child);

    // The first summary line prints from inside the loop, past the arming.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !std::fs::read_to_string(out.path()).is_ok_and(|s| s.contains("agents=[")) {
        assert!(
            std::time::Instant::now() < deadline,
            "the loop never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // SAFETY: signalling a child this test spawned and still owns.
    unsafe {
        libc::kill(child.0.id().cast_signed(), libc::SIGTERM);
    }
    let status = child.0.wait().expect("wait");

    let stderr = std::fs::read_to_string(err.path()).unwrap_or_default();
    assert!(stderr.contains("shutting down"), "no unwind ran:\n{stderr}");
    assert_eq!(status.signal(), Some(libc::SIGTERM), "status {status:?}");
}
