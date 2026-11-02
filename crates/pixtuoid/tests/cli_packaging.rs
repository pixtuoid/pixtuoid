//! Integration coverage for what only the REAL binary shows: the
//! `completions` / `man` packaging dispatch (the generation itself is unit-tested
//! in `cli.rs`) — that the SHELL arg reaches clap_complete and that stdout stays
//! the clean artifact channel homebrew-core captures — the fatal-error exit, the
//! clean exit when a printing command's reader leaves, and the two binaries
//! talking over a real endpoint.

use clap::ValueEnum;
use pixtuoid_core::source::hook::{self, EndpointStatus, HookRouter};

mod common;

/// Spawn the built binary with its log env cleared and a scratch state dir: the
/// binary HONORS a non-empty `$RUST_LOG`, so a test asserting a clean channel
/// must clear it rather than assume an inherited dev/CI verbosity is unset, and
/// a crash in a test must log to a scratch state dir, not the developer's real
/// crash.log.
fn run(args: &[&str]) -> std::process::Output {
    let state = tempfile::TempDir::new().expect("tempdir");
    std::process::Command::new(env!("CARGO_BIN_EXE_pixtuoid"))
        .args(args)
        .env_remove("RUST_LOG")
        .env_remove("PIXTUOID_LOG")
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("run pixtuoid")
}

/// Iterating `Shell::value_variants()` rather than a hardcoded subset means a
/// shell clap_complete adds later is covered automatically.
#[test]
fn completions_emit_a_clean_script_for_every_supported_shell() {
    let shells = clap_complete::Shell::value_variants();
    // Negative control: with 0 or 1 shell the per-shell loop AND the pairwise
    // distinctness tooth below would pass vacuously.
    assert!(
        shells.len() >= 2,
        "clap_complete exposes < 2 shells ({}) — the per-shell + distinctness coverage below is vacuous",
        shells.len()
    );

    let mut scripts = std::collections::HashSet::new();
    for shell in shells {
        let name = shell
            .to_possible_value()
            .expect("every Shell variant has a value name")
            .get_name()
            .to_owned();
        let out = run(&["completions", &name]);
        assert!(
            out.status.success(),
            "completions {name} exited non-zero: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stderr.is_empty(),
            "completions {name} wrote to stderr — the artifact channel must stay clean: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("pixtuoid"),
            "completions {name} script omits the binary name"
        );
        assert!(
            scripts.insert(out.stdout),
            "completions {name} produced a script identical to another shell's — the shell arg is ignored"
        );
    }
}

/// The homebrew `Utils.safe_popen_read` capture greps `^.TH`.
#[test]
fn man_emits_clean_roff_to_stdout() {
    let out = run(&["man"]);
    assert!(
        out.status.success(),
        "man exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "man wrote to stderr — the artifact channel must stay clean: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(".TH"),
        "man output is not roff (.TH header missing)"
    );
}

/// A theme name carrying an ESC and a bidi override: `run` refuses it with an
/// error that quotes it.
const HOSTILE_THEME: &str = "x\u{1b}[31m\u{202e}";

/// `run` refused for `HOSTILE_THEME`, its config read from an empty `config`
/// dir, so the developer's own config.toml plays no part.
fn refused_run(config: &std::path::Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_pixtuoid"));
    cmd.args(["--theme", HOSTILE_THEME, "run", "--headless"])
        .env_remove("RUST_LOG")
        .env_remove("PIXTUOID_LOG")
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_STATE_HOME", config);
    cmd
}

#[test]
fn a_fatal_error_reaches_stderr_stripped_and_exits_1() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out = refused_run(tmp.path()).output().expect("run pixtuoid");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("Error: "), "{stderr:?}");
    assert!(
        !stderr.contains('\u{1b}') && !stderr.contains('\u{202e}'),
        "{stderr:?}"
    );
}

/// A pipe whose reader is gone, not a closed fd: std swallows EBADF on
/// stdout/stderr, so only a write that fails reaches the panic this guards.
fn reader_gone() -> std::io::PipeWriter {
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    writer
}

#[test]
fn a_fatal_error_into_a_broken_pipe_still_exits_1() {
    // A regression crashes, and the crash hook must not log into the real
    // state dir.
    let state = tempfile::TempDir::new().expect("tempdir");
    let status = refused_run(state.path())
        .stderr(reader_gone())
        .status()
        .expect("run pixtuoid");
    assert_eq!(status.code(), Some(1), "{status:?}");
}

/// Every platform: these read no home, so they need no env isolation, and
/// Windows reports a gone reader through its own error code.
#[test]
fn artifact_commands_exit_0_when_their_reader_leaves() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    for args in [&["man"][..], &["completions", "bash"]] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_pixtuoid"))
            .args(args)
            .env_remove("RUST_LOG")
            .env_remove("PIXTUOID_LOG")
            .env("XDG_STATE_HOME", tmp.path())
            .stdout(reader_gone())
            .output()
            .expect("run pixtuoid");
        assert!(
            out.status.success(),
            "{args:?}: {:?}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn home_reading_commands_exit_0_when_their_reader_leaves() {
    let home = tempfile::TempDir::new().expect("tempdir");
    // Claude Code counts as detected by its `~/.claude` dir alone, and only a
    // detected CLI reaches `setup --yes`'s apply-loop rows.
    std::fs::create_dir_all(home.path().join(".claude")).expect("seed ~/.claude");
    for args in [
        &["sources", "--json"][..],
        &["sources"],
        &["setup"],
        &["doctor"],
        &["connect", "claude-code", "--json"],
        &["disconnect", "claude-code"],
        &["sources", "set", "claude-code"],
        &["setup", "--yes"],
    ] {
        let out = common::isolated(args, home.path())
            .stdout(reader_gone())
            .output()
            .expect("run pixtuoid");
        assert!(
            out.status.success(),
            "{args:?}: {:?}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `doctor` reads `run`'s `look` setting, so a typo in it is one of
/// doctor's config warnings, as it is `run`'s.
#[cfg(unix)]
#[test]
fn doctor_warns_on_an_unknown_look_setting() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let config = home.path().join(".config/pixtuoid");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(config.join("config.toml"), "look = \"clasic\"\n").expect("config");
    let out = common::isolated(&["doctor"], home.path())
        .output()
        .expect("run pixtuoid");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("unknown look \"clasic\" in config"),
        "{stdout}"
    );
}

/// The tracing sink's own report of a failed write is an `eprintln!` onto the
/// stream that failed; `PIXTUOID_LOG` naming a directory makes `sources` log a
/// warning on stderr.
#[cfg(unix)]
#[test]
fn a_tracing_warning_into_a_gone_stderr_exits_0() {
    let home = tempfile::TempDir::new().expect("tempdir");
    // The positive control: with stderr kept, the warning this test relies on
    // is there.
    let kept = common::isolated(&["sources"], home.path())
        .env("PIXTUOID_LOG", home.path())
        .output()
        .expect("run pixtuoid");
    let stderr = String::from_utf8_lossy(&kept.stderr);
    assert!(stderr.contains("log unreadable"), "{stderr}");

    let gone = reader_gone();
    let status = common::isolated(&["sources"], home.path())
        .env("PIXTUOID_LOG", home.path())
        .stdout(gone.try_clone().expect("clone pipe"))
        .stderr(gone)
        .status()
        .expect("run pixtuoid");
    assert_eq!(status.code(), Some(0), "{status:?}");
}

/// The shim a workspace `just test` builds beside the binary.
fn built_shim() -> std::path::PathBuf {
    let shim = std::path::Path::new(env!("CARGO_BIN_EXE_pixtuoid"))
        .with_file_name(format!("pixtuoid-hook{}", std::env::consts::EXE_SUFFIX));
    assert!(
        shim.exists(),
        "{} is not built: `-p pixtuoid` alone doesn't build it",
        shim.display()
    );
    shim
}

/// `pixtuoid doctor` with `home` as every home the environment can name.
fn doctor(home: &std::path::Path, endpoint: &std::path::Path) -> std::process::Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_pixtuoid"));
    // Cleared: an install that a CLI's own home variable leads to would stand in
    // for the built shim. Windows starts no process and finds no temp dir without
    // these.
    command.arg("doctor").env_clear();
    for kept in [
        "PATH",
        "SystemRoot",
        "TEMP",
        "TMP",
        "TMPDIR",
        "LLVM_PROFILE_FILE",
    ] {
        if let Some(value) = std::env::var_os(kept) {
            command.env(kept, value);
        }
    }
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_STATE_HOME", home)
        .env(hook::SOCKET_ENV, endpoint)
        .env("PIXTUOID_HOOK", built_shim())
        .output()
        .expect("run pixtuoid doctor")
}

/// By its path: on Windows Jcode's home ignores the environment, so an install
/// there can stand in for the built shim.
fn assert_the_built_shim_delivered(stdout: &str) {
    let shim = built_shim().display().to_string();
    assert!(
        stdout
            .lines()
            .any(|row| row.contains(&shim) && row.contains("delivered a test event")),
        "{stdout}"
    );
}

#[test]
fn doctor_proves_the_built_shim_delivers_with_no_office_open() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let out = doctor(home.path(), &hook::private_endpoint(home.path(), "none"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert_the_built_shim_delivered(&stdout);
    assert!(stdout.contains("no instance on"), "{stdout}");
}

// The whole chain: a shim's hook reaches a live owner, and doctor reads it there.
#[tokio::test(flavor = "multi_thread")]
async fn doctor_reads_what_the_built_shim_delivered_to_a_live_owner() {
    use std::io::Write;
    use std::process::Stdio;

    let home = tempfile::TempDir::new().expect("tempdir");
    let endpoint = hook::private_endpoint(home.path(), "live");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let owner = tokio::spawn(pixtuoid_core::Source::run(
        Box::new(HookRouter::new(endpoint.clone())),
        tx,
    ));
    while !matches!(
        hook::endpoint_status(&endpoint).await,
        EndpointStatus::Live(_)
    ) {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    let mut shim = std::process::Command::new(built_shim())
        .env(hook::SOCKET_ENV, &endpoint)
        .args(["--source", "claude-code"])
        .stdin(Stdio::piped())
        .spawn()
        .expect("run the shim");
    shim.stdin
        .take()
        .expect("piped")
        .write_all(br#"{"hook_event_name":"SessionStart","session_id":"s","cwd":""}"#)
        .expect("feed the shim");
    assert!(shim.wait().expect("the shim exits").success());
    rx.recv().await.expect("the hook reaches the owner");

    let (home_path, at) = (home.path().to_path_buf(), endpoint.clone());
    let out = tokio::task::spawn_blocking(move || doctor(&home_path, &at))
        .await
        .expect("doctor ran");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_the_built_shim_delivered(&stdout);
    assert!(stdout.contains("instance live on"), "{stdout}");
    assert!(stdout.contains("last hook"), "{stdout}");
    owner.abort();
}
