#![cfg(unix)]
//! Integration tests for the hook shim BINARY's I/O contract (invariant #5:
//! "always exit 0, never block CC"). `shim_pipe.rs` is the Windows twin —
//! mirror there any behavior pinned here.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_pixtuoid-hook");

/// A short, unique socket path under /tmp — a deep tempdir would blow the
/// ~104-byte `sun_path` limit.
/// A test socket path that removes itself on drop. Cleanup used to be one
/// `remove_file` per test to remember, and 3 of 7 didn't — so every run leaked
/// a `/tmp` socket, forever.
struct SockPath(std::path::PathBuf);

impl std::ops::Deref for SockPath {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}
impl AsRef<std::path::Path> for SockPath {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}
impl AsRef<std::ffi::OsStr> for SockPath {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}
impl Drop for SockPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn sock_path(tag: &str) -> SockPath {
    let p = std::path::PathBuf::from(format!(
        "/tmp/pixtuoid-hook-it-{}-{tag}.sock",
        std::process::id()
    ));
    // Also up-front: a previous run that was KILLED never dropped its guard.
    let _ = std::fs::remove_file(&p);
    SockPath(p)
}

/// Generic over the arg type so the non-UTF-8-argv test can pass raw `OsStr`
/// bytes.
fn run_shim_inner<S: AsRef<std::ffi::OsStr>>(
    socket: &std::path::Path,
    source: Option<&str>,
    args: &[S],
    stdin: &[u8],
) -> std::process::Output {
    let mut cmd = Command::new(BIN);
    cmd.env("PIXTUOID_SOCKET", socket)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match source {
        Some(s) => {
            cmd.env("PIXTUOID_SOURCE", s);
        }
        None => {
            cmd.env_remove("PIXTUOID_SOURCE");
        }
    }
    let mut child = cmd.spawn().expect("spawn shim");
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(stdin)
        .expect("write stdin");
    // stdin dropped here → EOF, so the shim's read_to_string returns.
    child.wait_with_output().expect("wait shim")
}

fn run_shim(
    socket: &std::path::Path,
    source: Option<&str>,
    stdin: &[u8],
) -> std::process::ExitStatus {
    run_shim_inner::<&str>(socket, source, &[], stdin).status
}

fn run_shim_args(
    socket: &std::path::Path,
    args: &[&str],
    stdin: &[u8],
) -> std::process::ExitStatus {
    run_shim_inner(socket, None, args, stdin).status
}

fn recv_delivered_json(listener: &UnixListener) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = loop {
        match listener.accept() {
            Ok((s, _)) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "shim never delivered to the socket"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("accept: {e}"),
        }
    };
    // accept() inherited the listener's non-blocking mode — restore blocking.
    stream.set_nonblocking(false).unwrap();
    let mut got = String::new();
    stream
        .read_to_string(&mut got)
        .expect("read delivered line");
    let line = got.lines().next().expect("at least one line");
    serde_json::from_str(line).expect("delivered line is valid JSON")
}

#[test]
fn delivers_one_json_line_to_listener_and_exits_zero() {
    let path = sock_path("deliver");
    let listener = UnixListener::bind(&path).expect("bind listener");
    listener.set_nonblocking(true).unwrap();

    let status = run_shim(
        &path,
        Some("codex"),
        br#"{"hook_event_name":"Stop","session_id":"abc"}"#,
    );
    assert!(status.success(), "shim must exit 0; got {status:?}");

    let v = recv_delivered_json(&listener);
    assert_eq!(v["hook_event_name"], "Stop");
    assert_eq!(v["session_id"], "abc", "original payload preserved");
    assert_eq!(v["_pixtuoid_source"], "codex", "shim stamps the CLI source");
    assert!(v.get("_shim_ts_ms").is_some(), "shim stamps a timestamp");
}

#[test]
fn argv_source_flag_stamps_source_without_env() {
    let path = sock_path("argvsrc");
    let listener = UnixListener::bind(&path).expect("bind listener");
    listener.set_nonblocking(true).unwrap();

    let status = run_shim_args(
        &path,
        &["--source", "codex"],
        br#"{"hook_event_name":"Stop","session_id":"abc"}"#,
    );
    assert!(status.success(), "shim must exit 0; got {status:?}");

    let v = recv_delivered_json(&listener);
    assert_eq!(
        v["_pixtuoid_source"], "codex",
        "the --source flag must stamp the CLI source with no env set"
    );
    assert_eq!(v["session_id"], "abc", "original payload preserved");
}

#[test]
fn a_non_utf8_workspace_keys_the_session_not_the_fallback() {
    use std::os::unix::ffi::OsStrExt;
    let path = sock_path("cwlossy");
    let listener = UnixListener::bind(&path).expect("bind listener");
    listener.set_nonblocking(true).unwrap();

    let status = Command::new(BIN)
        .env("PIXTUOID_SOCKET", &path)
        .env("PIXTUOID_SOURCE", "codewhale")
        .env(
            "DEEPSEEK_WORKSPACE",
            std::ffi::OsStr::from_bytes(b"/ws/\xff"),
        )
        .args(["--event", "session_start"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run shim");
    assert!(status.success(), "shim must exit 0; got {status:?}");
    assert_eq!(recv_delivered_json(&listener)["cwd"], "/ws/\u{FFFD}");
}

#[test]
fn codewhale_event_mode_builds_envelope_from_env_and_ignores_stdin() {
    // Env-mode MUST NOT read stdin: CodeWhale leaves the child's stdin = the
    // TUI terminal, which NEVER reaches EOF, and `tool_call_before` runs
    // SYNCHRONOUSLY — a blind read_to_string would freeze the user's tool call.
    // So the fixture holds the child's stdin pipe OPEN (never write, never
    // close) and bounds the wait: a re-added blocking read hangs and trips it,
    // where an EOF-able pipe would pass.
    let path = sock_path("cwevent");
    let listener = UnixListener::bind(&path).expect("bind listener");
    listener.set_nonblocking(true).unwrap();

    let mut cmd = Command::new(BIN);
    cmd.env("PIXTUOID_SOCKET", &path)
        .env("PIXTUOID_SOURCE", "codewhale")
        .env("DEEPSEEK_WORKSPACE", "/repo/myproj")
        .env("DEEPSEEK_TOOL_NAME", "exec_shell")
        .env("DEEPSEEK_TOOL_ARGS", r#"{"command":"ls -la"}"#)
        .args(["--event", "tool_call_before"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn shim");
    let stdin = child.stdin.take().expect("stdin piped");

    let deadline = Instant::now() + Duration::from_millis(2000);
    let status = loop {
        if let Some(s) = child.try_wait().expect("try_wait") {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "env-mode shim did not exit within 2s — it must NOT read stdin (the inherited, never-EOF TUI terminal would block it; tool_call_before runs synchronously)"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    assert!(
        status.success(),
        "env-mode shim must exit 0; got {status:?}"
    );

    let v = recv_delivered_json(&listener);
    assert_eq!(v["event"], "tool_call_before");
    assert_eq!(
        v["cwd"], "/repo/myproj",
        "AgentId key folded from DEEPSEEK_WORKSPACE, not stdin"
    );
    assert_eq!(v["tool"], "exec_shell");
    assert_eq!(v["tool_args"], r#"{"command":"ls -la"}"#);
    assert_eq!(v["_pixtuoid_source"], "codewhale", "source stamped");
    assert!(v.get("_shim_ts_ms").is_some(), "timestamp stamped");
}

#[test]
fn missing_socket_exits_zero_without_blocking() {
    let path = sock_path("nosock");
    let start = Instant::now();
    let out = run_shim_inner::<&str>(&path, None, &[], br#"{"hook_event_name":"Stop"}"#);
    assert!(
        out.status.success(),
        "must exit 0 even with no listener; got {:?}",
        out.status
    );
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "must stay silent: stdout {:?}, stderr {:?}",
        out.stdout,
        out.stderr
    );
    // A missing socket makes `connect()` return ConnectionRefused in
    // microseconds, so this guards a regression that adds a blocking
    // retry/backoff on connect failure — don't delete it. The bound is the
    // watchdog's `transport::WRITE_TIMEOUT` plus spawn/exec jitter: it measures a CHILD PROCESS's
    // whole spawn+exit wall-clock, which is load-sensitive (#161 flaked at 1s
    // under the fully-parallel suite), hence .config/nextest.toml's
    // threads-required override giving this test the machine to itself.
    assert!(
        start.elapsed() < Duration::from_millis(1500),
        "shim must not block when the socket is absent"
    );
}

#[test]
fn stalled_listener_shim_exits_zero_within_watchdog_bound() {
    // A wedged accept loop with a saturated backlog is the one Unix path where
    // `connect()` itself can park forever (#167). Kernel-dependent: Linux
    // BLOCKS the shim's connect (the load-bearing arm — the watchdog must shoot
    // the process at `WRITE_TIMEOUT`), macOS fails fast with ECONNREFUSED.
    let path = sock_path("stall");
    let listener = UnixListener::bind(&path).expect("bind listener");

    // 160 fillers oversaturate the accept backlog (std binds with backlog 128);
    // each parks to hold its connection — or its blocked connect — open.
    let fillers: Vec<_> = (0..160)
        .map(|_| {
            let p = path.to_path_buf();
            std::thread::spawn(move || {
                let _conn = std::os::unix::net::UnixStream::connect(&p);
                std::thread::park();
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(100));

    let start = Instant::now();
    let status = run_shim(&path, None, br#"{"hook_event_name":"Stop"}"#);
    assert!(
        status.success(),
        "stalled listener must still exit 0; got {status:?}"
    );
    // The watchdog bound is `transport::WRITE_TIMEOUT`; the rest is spawn-jitter headroom.
    assert!(
        start.elapsed() < Duration::from_millis(1500),
        "watchdog must bound the connect phase; took {:?}",
        start.elapsed()
    );

    drop(listener);
    drop(fillers);
}

#[test]
fn non_utf8_argv_exits_zero() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    // Non-UTF-8 bytes are legal in Unix argv, and `std::env::args()` PANICS
    // while collecting such an argument — exit 101 + stderr, breaching
    // invariant #5's silent exit-0 contract.
    let path = sock_path("nonutf8");
    let status = run_shim_inner(
        &path,
        None,
        &[
            OsStr::from_bytes(b"--source"),
            OsStr::from_bytes(b"\xff\xfe not utf8"),
        ],
        br#"{"hook_event_name":"Stop"}"#,
    )
    .status;
    assert!(
        status.success(),
        "non-UTF-8 argv must not panic the shim; got {status:?}"
    );
}

#[test]
fn malformed_stdin_exits_zero() {
    let path = sock_path("garbage");
    let status = run_shim(&path, None, b"this is not json at all {{{");
    assert!(
        status.success(),
        "malformed stdin must still exit 0; got {status:?}"
    );
}
