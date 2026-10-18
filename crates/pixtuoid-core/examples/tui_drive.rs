//! Drive a CLI's interactive TUI under a pty for one hermetic scenario: the
//! command `just hermetic` hands the recorder in place of the CLI's headless
//! invocation. What to send comes from `harness::hermetic::tui::Driver`, reading
//! the profile's `Tui` row; this shell only moves bytes and renders the screen.
//!
//! ```text
//! tui_drive <source> <scenario> <transcript> <prompt> -- <program> [args...]
//! ```
//!
//! Exits with the CLI's status; [`GAVE_UP`] when the driver gave up or the
//! [`BUDGET`] ran out, and [`USAGE_ERROR`] on a bad command line.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use pixtuoid_core::harness::hermetic::tui::{Action, Driver, Key, Tui};
use pixtuoid_core::harness::hermetic::{self, Drive, Scenario};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

/// How long one run may take, end to end, before the CLI is killed: well under
/// the hermetic job's timeout, which every scenario of every CLI shares.
const BUDGET: Duration = Duration::from_secs(120);
/// How long a CLI may take to exit after its quit keys.
const QUIT_GRACE: Duration = Duration::from_secs(10);
/// How often the screen is re-judged when no output arrives.
const TICK: Duration = Duration::from_millis(100);
/// The pause between two writes, so a TUI reads keys one at a time, never as
/// a paste.
const KEY_GAP: Duration = Duration::from_millis(80);
/// A size every CLI lays out in: a 0x0 pty renders nothing at all.
const SIZE: PtySize = PtySize {
    rows: 40,
    cols: 120,
    pixel_width: 0,
    pixel_height: 0,
};
/// The terminal's cursor-position query. ConPTY is created inheriting the
/// cursor and may hang until the host answers it.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";

const GAVE_UP: i32 = 3;
const USAGE_ERROR: i32 = 2;
const USAGE: &str =
    "usage: tui_drive <source> <scenario> <transcript> <prompt> -- <program> [args...]";

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let Some((source, scenario, transcript, prompt, command)) = parse(&args) else {
        eprintln!("{USAGE}");
        std::process::exit(USAGE_ERROR);
    };
    let code = match drive(&source, &scenario, Path::new(&transcript), prompt, &command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tui_drive: {e}");
            GAVE_UP
        }
    };
    std::process::exit(code);
}

type Parsed = (String, String, OsString, String, Vec<OsString>);

fn parse(args: &[OsString]) -> Option<Parsed> {
    let split = args.iter().position(|a| a == "--")?;
    let [source, scenario, transcript, prompt] = &args[..split] else {
        return None;
    };
    let command = args[split + 1..].to_vec();
    if command.is_empty() {
        return None;
    }
    Some((
        source.to_str()?.to_owned(),
        scenario.to_str()?.to_owned(),
        transcript.clone(),
        prompt.to_str()?.to_owned(),
        command,
    ))
}

fn tui_scenario(
    source: &str,
    scenario: &str,
) -> std::io::Result<(&'static Tui, &'static Scenario)> {
    let p = hermetic::profile(source)
        .ok_or_else(|| std::io::Error::other(format!("{source}: not a profiled source")))?;
    let tui = p
        .tui
        .as_ref()
        .ok_or_else(|| std::io::Error::other(format!("{source}: no Tui profile")))?;
    let found = p
        .scenarios
        .iter()
        .find(|s| s.drive == Drive::Tui && s.shape.name() == scenario)
        .ok_or_else(|| std::io::Error::other(format!("{source}: no TUI scenario {scenario}")))?;
    Ok((tui, found))
}

/// The CLI's pty command: exactly this process's environment. portable-pty's
/// own base environment merges the Windows registry's over it, which would let
/// a key set with `setx` past the tier's sandbox.
fn command(argv: &[OsString], env: impl Iterator<Item = (OsString, OsString)>) -> CommandBuilder {
    let mut cmd = CommandBuilder::from_argv(argv.to_vec());
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    cmd
}

fn drive(
    source: &str,
    scenario: &str,
    transcript: &Path,
    prompt: String,
    argv: &[OsString],
) -> std::io::Result<i32> {
    let (tui, scenario) = tui_scenario(source, scenario)?;
    let mut log = std::fs::File::create(transcript)?;
    let pair = NativePtySystem::default()
        .openpty(SIZE)
        .map_err(std::io::Error::other)?;
    let mut child = pair
        .slave
        .spawn_command(command(argv, std::env::vars_os()))
        .map_err(std::io::Error::other)?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(std::io::Error::other)?;
    let mut writer = pair.master.take_writer().map_err(std::io::Error::other)?;

    // Output drains on its own thread, apart from the close: a ConPTY before
    // Windows 11 24H2 can block its close until the output is read.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let started = Instant::now();
    let mut screen = vt100::Parser::new(SIZE.rows, SIZE.cols, 0);
    let mut driver = Driver::new(tui, scenario.shape, prompt, started);
    let mut quit_at: Option<Instant> = None;
    loop {
        if let Some(status) = child.try_wait()? {
            note(&mut log, &format!("exited: {status:?}"));
            return Ok(i32::try_from(status.exit_code()).unwrap_or(GAVE_UP));
        }
        let now = Instant::now();
        let out_of_time = now.duration_since(started) >= BUDGET
            || quit_at.is_some_and(|at| now.duration_since(at) >= QUIT_GRACE);
        if out_of_time {
            note(&mut log, "out of time: killing the CLI");
            child.kill()?;
            return Ok(GAVE_UP);
        }
        if let Ok(chunk) = rx.recv_timeout(TICK) {
            log.write_all(&chunk)?;
            screen.process(&chunk);
            if chunk.windows(CURSOR_QUERY.len()).any(|w| w == CURSOR_QUERY) {
                let (row, col) = screen.screen().cursor_position();
                send(
                    &mut writer,
                    format!("\x1b[{};{}R", row + 1, col + 1).as_bytes(),
                )?;
            }
        }
        match driver.step(&screen.screen().contents(), Instant::now()) {
            Action::Wait => {}
            Action::Type(text) => {
                note(&mut log, "type the prompt");
                send(&mut writer, text.as_bytes())?;
            }
            Action::Keys(keys) => {
                note(&mut log, &format!("keys {keys:?}"));
                send_keys(&mut writer, keys)?;
            }
            Action::Quit(keys) => {
                note(&mut log, &format!("quit {keys:?}"));
                send_keys(&mut writer, keys)?;
                quit_at = Some(Instant::now());
            }
            Action::Fail(why) => {
                note(
                    &mut log,
                    &format!(
                        "gave up: {why}\n--- screen ---\n{}",
                        screen.screen().contents()
                    ),
                );
                child.kill()?;
                return Ok(GAVE_UP);
            }
        }
    }
}

fn send(writer: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(bytes)?;
    writer.flush()?;
    std::thread::sleep(KEY_GAP);
    Ok(())
}

fn send_keys(writer: &mut impl Write, keys: &[Key]) -> std::io::Result<()> {
    keys.iter().try_for_each(|k| send(writer, k.bytes()))
}

/// The driver's own lines in the transcript, between the CLI's raw output.
fn note(log: &mut std::fs::File, line: &str) {
    let _ = writeln!(log, "\n[tui_drive] {line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_gets_exactly_the_drivers_environment() {
        let given = [("ONLY", "this"), ("PATH", "/sandbox/bin")];
        let cmd = command(
            &["cli".into()],
            given.iter().map(|(k, v)| ((*k).into(), (*v).into())),
        );
        let got: std::collections::BTreeMap<&str, &str> = cmd.iter_full_env_as_str().collect();
        assert_eq!(
            got,
            given.into_iter().collect(),
            "portable-pty's base environment reached the CLI past the sandbox"
        );
    }

    #[test]
    fn the_command_line_needs_four_operands_and_a_program() {
        let args = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        let (source, scenario, _, prompt, command) = parse(&args(&[
            "claude-code",
            "permission",
            "t.log",
            "hi",
            "--",
            "claude",
            "-x",
        ]))
        .unwrap();
        assert_eq!(
            (source.as_str(), scenario.as_str(), prompt.as_str()),
            ("claude-code", "permission", "hi")
        );
        assert_eq!(command, args(&["claude", "-x"]));
        assert!(parse(&args(&["claude-code", "permission", "t.log", "hi", "--"])).is_none());
        assert!(parse(&args(&["claude-code", "permission", "hi", "--", "claude"])).is_none());
    }
}
