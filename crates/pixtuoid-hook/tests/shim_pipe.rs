//! The named-pipe twin of `shim.rs`, compiled only on Windows: mirror here any
//! behavior pinned there. Nothing enforces the parity, and the rosters differ.
#![cfg(windows)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::net::windows::named_pipe::{ClientOptions, PipeMode, ServerOptions};

fn shim_bin() -> &'static str {
    env!("CARGO_BIN_EXE_pixtuoid-hook")
}

fn run_shim(pipe_name: &str, stdin_json: &str) -> (std::process::ExitStatus, Duration) {
    run_shim_args(pipe_name, &[], stdin_json)
}

fn run_shim_args(
    pipe_name: &str,
    args: &[&str],
    stdin_json: &str,
) -> (std::process::ExitStatus, Duration) {
    let (out, elapsed) = run_shim_output(pipe_name, args, stdin_json);
    (out.status, elapsed)
}

fn run_shim_output(
    pipe_name: &str,
    args: &[&str],
    stdin_json: &str,
) -> (std::process::Output, Duration) {
    let started = Instant::now();
    let mut child = Command::new(shim_bin())
        .env("PIXTUOID_SOCKET", pipe_name)
        .env_remove("PIXTUOID_SOURCE")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shim");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait shim");
    (out, started.elapsed())
}

#[tokio::test]
async fn delivers_one_json_line_to_pipe_listener_and_exits_zero() {
    let name = format!(r"\\.\pipe\pixtuoid-test-{}", std::process::id());
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .pipe_mode(PipeMode::Byte)
        .create(&name)
        .expect("create pipe");

    let name2 = name.clone();
    let shim = std::thread::spawn(move || {
        run_shim(&name2, r#"{"hook_event_name":"Stop","session_id":"s1"}"#)
    });

    server.connect().await.expect("connect");
    let mut got = Vec::new();
    server.read_to_end(&mut got).await.expect("read");
    let line = String::from_utf8(got).expect("utf8");
    assert!(line.ends_with('\n'), "newline-terminated: {line:?}");
    assert!(line.contains(r#""hook_event_name":"Stop""#));
    assert!(line.contains("_shim_ts_ms"), "shim enrichment present");

    let (status, _) = shim.join().expect("join");
    assert!(status.success(), "exit 0");
}

#[tokio::test]
async fn argv_source_flag_stamps_source_over_pipe_without_env() {
    // The Windows install form carries no PIXTUOID_SOURCE env — cmd.exe /C can't
    // express the env-prefix form.
    let name = format!(r"\\.\pipe\pixtuoid-test-argvsrc-{}", std::process::id());
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .pipe_mode(PipeMode::Byte)
        .create(&name)
        .expect("create pipe");

    let name2 = name.clone();
    let shim = std::thread::spawn(move || {
        run_shim_args(
            &name2,
            &["--source", "codex"],
            r#"{"hook_event_name":"Stop","session_id":"s1"}"#,
        )
    });

    server.connect().await.expect("connect");
    let mut got = Vec::new();
    server.read_to_end(&mut got).await.expect("read");
    let line = String::from_utf8(got).expect("utf8");
    assert!(
        line.contains(r#""_pixtuoid_source":"codex""#),
        "the --source flag must stamp the source with no env: {line:?}"
    );

    let (status, _) = shim.join().expect("join");
    assert!(status.success(), "exit 0");
}

#[tokio::test]
async fn codewhale_event_mode_builds_envelope_from_env_and_ignores_stdin() {
    // The child's stdin is HELD open (never written, never closed → no EOF) so a
    // re-added blind stdin read would BLOCK and trip the connect timeout below —
    // an EOF-able pipe would instead be drained instantly and prove nothing.
    let name = format!(r"\\.\pipe\pixtuoid-test-cwevent-{}", std::process::id());
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .pipe_mode(PipeMode::Byte)
        .create(&name)
        .expect("create pipe");

    let name2 = name.clone();
    let shim = std::thread::spawn(move || {
        let mut child = Command::new(shim_bin())
            .env("PIXTUOID_SOCKET", &name2)
            .env_remove("PIXTUOID_SOURCE")
            .env("DEEPSEEK_WORKSPACE", r"C:\repo\myproj")
            .env("DEEPSEEK_TOOL_NAME", "exec_shell")
            .env("DEEPSEEK_TOOL_ARGS", r#"{"command":"dir"}"#)
            .args(["--source", "codewhale", "--event", "tool_call_before"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn shim");
        // Dropped only after wait(), so stdin stays open for the child's lifetime.
        let stdin = child.stdin.take().expect("stdin");
        let status = child.wait().expect("wait shim");
        drop(stdin);
        status
    });

    // Keep this a bare statement, not a `let` binding — `connect()` yields `()`,
    // and binding a unit value trips clippy::let_unit_value under -D warnings.
    tokio::time::timeout(Duration::from_secs(3), server.connect())
        .await
        .expect("env-mode shim must connect within 3s — it must NOT block reading stdin")
        .expect("connect");
    let mut got = Vec::new();
    server.read_to_end(&mut got).await.expect("read");
    let line = String::from_utf8(got).expect("utf8");
    assert!(line.contains(r#""event":"tool_call_before""#), "{line:?}");
    assert!(
        line.contains(r#""cwd":"C:\\repo\\myproj""#),
        "AgentId key from DEEPSEEK_WORKSPACE, not stdin: {line:?}"
    );
    assert!(line.contains(r#""tool":"exec_shell""#), "{line:?}");
    assert!(
        line.contains(r#""_pixtuoid_source":"codewhale""#),
        "{line:?}"
    );

    let status = shim.join().expect("join");
    assert!(status.success(), "env-mode shim exits 0");
}

#[tokio::test]
async fn codex_cmd_c_invocation_of_hook_command_stamps_source() {
    // Reproduces codex's Windows hook spawn, which is the layer that defeats a
    // QUOTED path: codex's Command::arg escapes inner quotes and cmd.exe then
    // mangles it, so the installer must emit the BARE exec form. CARGO_BIN_EXE has
    // no spaces on CI, i.e. this pins the bare form's working case only.
    let name = format!(r"\\.\pipe\pixtuoid-test-cmdc-{}", std::process::id());
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .pipe_mode(PipeMode::Byte)
        .create(&name)
        .expect("create pipe");

    let exe = shim_bin();
    let command = format!("{exe} --source codex");
    let name2 = name.clone();
    let shim = std::thread::spawn(move || {
        let mut child = Command::new("cmd.exe")
            .arg("/C")
            .arg(&command)
            .env("PIXTUOID_SOCKET", &name2)
            .env_remove("PIXTUOID_SOURCE")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd.exe");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(br#"{"hook_event_name":"Stop","session_id":"s1"}"#)
            .expect("write stdin");
        child.wait().expect("wait cmd")
    });

    server.connect().await.expect("connect");
    let mut got = Vec::new();
    server.read_to_end(&mut got).await.expect("read");
    let line = String::from_utf8(got).expect("utf8");
    assert!(
        line.contains(r#""_pixtuoid_source":"codex""#),
        "the bare `<exe> --source codex` form must reach the shim through cmd.exe /C \
         and stamp the source: {line:?}"
    );
    // #528: the shim's parent here IS the transient `cmd /C`, so a raw ppid
    // names it and a correct skip names US, its grandparent.
    assert!(
        line.contains(&format!(r#""_pid":{}"#, std::process::id())),
        "the walk must skip the cmd.exe /C parent and stamp this process: {line:?}"
    );

    let status = shim.join().expect("join");
    assert!(status.success(), "cmd.exe wrapper exits 0");
}

#[test]
fn missing_pipe_exits_zero_without_blocking() {
    let (out, elapsed) = run_shim_output(r"\\.\pipe\pixtuoid-test-nonexistent", &[], "{}");
    assert!(out.status.success(), "exit 0 on missing daemon");
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "must stay silent: stdout {:?}, stderr {:?}",
        out.stdout,
        out.stderr
    );
    // NotFound is the fast path; 2s is generous CI margin but well under any hang.
    assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
}

#[tokio::test]
async fn stalled_daemon_shim_exits_zero_within_watchdog_bound() {
    let name = format!(r"\\.\pipe\pixtuoid-test-stall-{}", std::process::id());
    let _server = ServerOptions::new()
        .first_pipe_instance(true)
        .pipe_mode(PipeMode::Byte)
        .create(&name)
        .expect("create pipe");
    // Occupy the lone instance so the shim's open() keeps getting ERROR_PIPE_BUSY.
    let _client = ClientOptions::new().open(&name).expect("occupy instance");

    let (status, elapsed) = run_shim(&name, r#"{"hook_event_name":"Stop","session_id":"s2"}"#);
    assert!(status.success(), "watchdog exits 0");
    // The watchdog bound is `transport::WRITE_TIMEOUT`; 1500 leaves runner-jitter
    // headroom while still proving "never blocks the calling CLI".
    assert!(elapsed < Duration::from_millis(1500), "took {elapsed:?}");
}
