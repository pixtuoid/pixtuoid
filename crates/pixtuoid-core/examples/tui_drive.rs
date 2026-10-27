//! Drive a CLI's interactive TUI under a pty for one hermetic scenario: the
//! command `just hermetic` hands the recorder in place of the CLI's headless
//! invocation. What to send comes from `harness::hermetic::tui::Driver`, reading
//! the profile's `Tui` row; this shell only moves bytes and renders the screen.
//!
//! ```text
//! tui_drive <source> <scenario> <log> <prompt> -- <program> [args...]
//! ```
//!
//! Exits with the CLI's status; when the driver gave up or the [`BUDGET`] ran
//! out, with what [`Driver::gave_up`] says, and why at [`GAVE_UP_NOTE`]; and
//! `USAGE_ERROR` on a bad command line.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use pixtuoid_core::harness::hermetic::tui::{
    Action, Answer, BUDGET, CommandLine, Driver, GAVE_UP, Key, Tui,
};
use pixtuoid_core::harness::hermetic::{self, Surface};
use pixtuoid_core::harness::{DRIVER_DIR_ENV, GAVE_UP_NOTE};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

/// How long a CLI may take to exit after its quit keys.
const QUIT_GRACE: Duration = Duration::from_secs(10);
/// How often the screen is re-judged when no output arrives.
const TICK: Duration = Duration::from_millis(100);
/// The pause between two writes, so a TUI reads keys one at a time, never as
/// a paste: past codewhale's `PASTE_ENTER_SUPPRESS_WINDOW`, in which an Enter
/// after typed text is a newline (`crates/tui/src/tui/paste_burst.rs:7` at v0.10.2).
const KEY_GAP: Duration = Duration::from_millis(200);
/// A size every CLI lays out in: a 0x0 pty renders nothing at all.
const SIZE: PtySize = PtySize {
    rows: 40,
    cols: 120,
    pixel_width: 0,
    pixel_height: 0,
};
/// The terminal's cursor-position query. portable-pty creates the ConPTY
/// inheriting the cursor (`src/win/psuedocon.rs:87` at 0.9.0), and its host
/// must then answer that query or may hang
/// (learn.microsoft.com/en-us/windows/console/createpseudoconsole).
const CURSOR_QUERY: &[u8] = b"\x1b[6n";
/// The terminal the CLI is told it runs in: what the screen parser emulates,
/// whatever the host's own is.
const TERM: &str = "xterm-256color";

const USAGE_ERROR: i32 = 2;
const USAGE: &str = "usage: tui_drive <source> <scenario> <log> <prompt> -- <program> [args...]";

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let Some(line) = CommandLine::parse(&args) else {
        eprintln!("{USAGE}");
        std::process::exit(USAGE_ERROR);
    };
    let code = drive(line, BUDGET).unwrap_or_else(|e| {
        let why = format!("tui_drive: {e}");
        eprintln!("{why}");
        say_why(&why);
        GAVE_UP
    });
    std::process::exit(code);
}

fn tui_of(source: &str, scenario: &str) -> std::io::Result<(&'static Tui, Answer)> {
    hermetic::profile(source)
        .ok_or_else(|| std::io::Error::other(format!("{source}: not a profiled source")))?
        .scenario(scenario)
        .and_then(|s| match s.surface {
            Surface::Tui { row } => Some((row, s.shape.answer())),
            Surface::Headless => None,
        })
        .ok_or_else(|| std::io::Error::other(format!("{source}: no TUI scenario {scenario}")))
}

/// The CLI's pty command: exactly this process's environment, and [`TERM`].
/// portable-pty's own base environment merges the Windows registry's over it
/// (`src/cmdbuilder.rs:74-190` at 0.9.0), which would let a key set with
/// `setx` past the tier's sandbox.
fn command(
    argv: &[OsString],
    env: impl Iterator<Item = (OsString, OsString)>,
) -> std::io::Result<CommandBuilder> {
    let mut cmd = CommandBuilder::from_argv(argv.to_vec());
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.env("TERM", TERM);
    cmd.cwd(std::env::current_dir()?);
    Ok(cmd)
}

fn drive(line: CommandLine, budget: Duration) -> std::io::Result<i32> {
    let (tui, answer) = tui_of(&line.source, &line.scenario)?;
    let mut log = std::fs::File::create(&line.log)?;
    let pair = NativePtySystem::default()
        .openpty(SIZE)
        .map_err(std::io::Error::other)?;
    let mut child = pair
        .slave
        .spawn_command(command(&line.program, std::env::vars_os())?)
        .map_err(std::io::Error::other)?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(std::io::Error::other)?;
    let mut writer = pair.master.take_writer().map_err(std::io::Error::other)?;

    // Output drains on its own thread, apart from the close: before Windows 11
    // 24H2 a ConPTY's close waits until its output is read
    // (learn.microsoft.com/en-us/windows/console/closepseudoconsole).
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
    let mut screen = parser();
    let mut driver = Driver::new(tui, answer, line.prompt, started);
    let mut quit_at: Option<Instant> = None;
    loop {
        if let Some(status) = child.try_wait()? {
            // The CLI's last output may still be queued behind its exit.
            while let Ok(chunk) = rx.recv_timeout(TICK) {
                log.write_all(&chunk)?;
            }
            note(&mut log, &format!("exited: {status:?}"));
            return Ok(i32::try_from(status.exit_code()).unwrap_or(GAVE_UP));
        }
        let now = Instant::now();
        if now.duration_since(started) >= budget {
            let why = format!("the {budget:?} budget ran out");
            return give_up(&mut log, &mut child, &why, &screen, &driver);
        }
        if quit_at.is_some_and(|at| now.duration_since(at) >= QUIT_GRACE) {
            let why = format!("the CLI was still up {QUIT_GRACE:?} after its quit keys");
            return give_up(&mut log, &mut child, &why, &screen, &driver);
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
        let shown = shown(screen.screen());
        let action = driver.step(&shown, Instant::now());
        if action != Action::Wait {
            note(&mut log, &format!("screen:\n{shown}"));
        }
        match action {
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
            Action::Fail(why) => return give_up(&mut log, &mut child, why, &screen, &driver),
        }
    }
}

/// A screen the pty's size.
fn parser() -> vt100::Parser {
    let side = |n| NonZeroU16::new(n).expect("SIZE is never 0x0");
    vt100::Parser::new(side(SIZE.rows), side(SIZE.cols), 0)
}

/// The screen row by row: `contents()` joins a wrapped row onto the next
/// (`src/grid.rs:289-290` at vt100 0.18.1), and a ConPTY repaint wraps a
/// full-width rule into the composer's row (`hermetic.yml`'s Windows leg, run
/// 39172132633's `claude-code/permission/tui.log`).
fn shown(screen: &vt100::Screen) -> String {
    let rows: Vec<String> = screen.rows(0, SIZE.cols).collect();
    rows.join("\n").trim_end().to_owned()
}

/// Kill the CLI, logging why and the screen it stood at.
fn give_up(
    log: &mut std::fs::File,
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    why: &str,
    screen: &vt100::Parser,
    driver: &Driver<'_>,
) -> std::io::Result<i32> {
    note(
        log,
        &format!("gave up: {why}\n--- screen ---\n{}", shown(screen.screen())),
    );
    say_why(why);
    child.kill()?;
    Ok(driver.gave_up())
}

/// Tell the recorder that runs this driver why it gave up.
fn say_why(why: &str) {
    if let Some(dir) = std::env::var_os(DRIVER_DIR_ENV) {
        let _ = std::fs::write(Path::new(&dir).join(GAVE_UP_NOTE), why);
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

/// The driver's own lines in the log, between the CLI's raw output.
fn note(log: &mut std::fs::File, line: &str) {
    let _ = writeln!(log, "\n[tui_drive] {line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_composer_under_a_rule_the_cli_let_wrap_takes_the_prompt() {
        let mut screen = parser();
        let rule = "─".repeat(usize::from(SIZE.cols));
        screen.process(format!("{rule}❯ ").as_bytes());
        let (tui, answer) = tui_of("claude-code", "permission").expect("a TUI row");
        let now = Instant::now();
        let mut driver = Driver::new(tui, answer, "hi".to_owned(), now);
        assert_eq!(
            driver.step(&shown(screen.screen()), now),
            Action::Type("hi".to_owned())
        );
    }

    #[test]
    fn the_driver_gives_up_before_the_recorder_kills_it() {
        assert!(BUDGET + QUIT_GRACE <= *hermetic::TURN_BUDGET.here());
    }

    #[cfg(unix)]
    #[test]
    fn a_run_past_its_budget_gives_up_as_its_driver_says_and_tells_the_recorder_why() {
        let dir = tempfile::tempdir().expect("dir");
        let line = CommandLine {
            source: "claude-code".into(),
            scenario: "permission".into(),
            log: dir.path().join("tui.log"),
            prompt: "hi".into(),
            program: vec!["sh".into(), "-c".into(), "sleep 600".into()],
        };
        let (tui, answer) = tui_of(&line.source, &line.scenario).expect("a TUI row");
        let unprompted = Driver::new(tui, answer, line.prompt.clone(), Instant::now());
        let budget = Duration::from_millis(500);
        let code = temp_env::with_var(DRIVER_DIR_ENV, Some(dir.path()), || drive(line, budget))
            .expect("it drove");
        assert_eq!(code, unprompted.gave_up());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(GAVE_UP_NOTE)).expect("its note"),
            format!("the {budget:?} budget ran out")
        );
    }

    #[test]
    fn the_cli_gets_exactly_the_drivers_environment() {
        let given = [("ONLY", "this"), ("PATH", "/sandbox/bin"), ("TERM", "dumb")];
        let cmd = command(
            &["cli".into()],
            given.iter().map(|(k, v)| ((*k).into(), (*v).into())),
        )
        .expect("cwd");
        let got: std::collections::BTreeMap<&str, &str> = cmd.iter_full_env_as_str().collect();
        assert_eq!(
            got,
            [("ONLY", "this"), ("PATH", "/sandbox/bin"), ("TERM", TERM)]
                .into_iter()
                .collect(),
            "portable-pty's base environment reached the CLI past the sandbox"
        );
    }
}
