//! The program behind `main`: parse the command line, set up the terminal,
//! logging and crash hook, and dispatch the subcommand. The binary is only
//! `main.rs`'s call into [`run`] and its error report
//! (<https://doc.rust-lang.org/book/ch12-03-improving-error-handling-and-modularity.html>).

mod crash;
pub(crate) mod logging;
mod sources_cli;

use std::io::Write;

use crate::cli::{Cli, Cmd, JsonArgs, SourceArgs, SourceIdArgs, SourcesAction};
use crate::{config, doctor, floating, install, runtime, sources};
use anyhow::Result;
use clap::Parser;

/// Run the command line's subcommand.
///
/// # Errors
///
/// The subcommand's failure, which `main` reports through
/// [`crate::report_fatal`].
pub fn run() -> Result<()> {
    crash::install_crash_hook();
    let (log_level, cmd) = Cli::parse().cmd_or_default();

    // Only the terminal `run` TUI needs the terminal's color: `floating` paints
    // real RGB through its GPU surface, and every other command is plain text.
    let is_run_tui = matches!(
        &cmd,
        Cmd::Run {
            headless: false,
            ..
        }
    );

    // The office has no legible monochrome fallback, so refuse the canvas with an
    // explanation rather than render block-soup. Runs before the truecolor
    // probe, so a dumb terminal never gets DECRQSS.
    if is_run_tui {
        use crate::term::ColorPreflight;
        match crate::term::ColorEnv::read().preflight {
            ColorPreflight::Proceed => {}
            ColorPreflight::RefuseNoColor => anyhow::bail!(
                "$NO_COLOR is set, so color output is disabled — the pixel-art office \
                 is 24-bit color with no legible monochrome mode and would render as \
                 unreadable blocks. Unset NO_COLOR to run it, or use `pixtuoid run \
                 --headless` for a text summary."
            ),
            ColorPreflight::RefuseDumbTerm => anyhow::bail!(
                "$TERM=dumb — this terminal can't render the pixel-art office (no cursor \
                 addressing or color). Use a graphical terminal (Windows Terminal, \
                 iTerm2, Ghostty, Alacritty, kitty, WezTerm), or `pixtuoid run \
                 --headless` for a text summary."
            ),
        }
    }

    // Rather than guess truecolor from a $TERM allowlist, ASK the terminal
    // (DECRQSS) when $COLORTERM hasn't declared it, and only WARN — never gate on
    // Unix (Windows hard-gates VT separately in `tui::mod`).
    // $PIXTUOID_NO_TRUECOLOR_WARN is the escape hatch for a terminal we can't
    // auto-detect. The query runs only inside `warn_zone`, so a healthy truecolor
    // session pays nothing.
    if crate::term::warn_zone(
        is_run_tui,
        std::io::IsTerminal::is_terminal(&std::io::stderr()),
        pixtuoid_core::platform::text_env("COLORTERM").as_deref(),
        pixtuoid_core::platform::text_env("PIXTUOID_NO_TRUECOLOR_WARN").as_deref(),
    ) && crate::term::query_truecolor().warrants_warning()
    {
        let _ = writeln!(
            std::io::stderr(),
            "⚠ pixtuoid: your terminal didn't confirm truecolor support — the \
             pixel-art office renders in 24-bit color and may look wrong. Use a \
             truecolor terminal (Windows Terminal, iTerm2, Ghostty, Alacritty, kitty, \
             WezTerm), run `pixtuoid doctor` to check, or set \
             PIXTUOID_NO_TRUECOLOR_WARN=1 to silence."
        );
    }
    let log_level: &'static str = log_level.as_str();
    let file_log = is_run_tui || matches!(&cmd, Cmd::Floating { .. });
    let (drift, log_flush) = logging::init(file_log, log_level);
    let ran = dispatch(cmd, drift);
    // The log file's copy of `main`'s stderr report, which no one reads once a
    // detached `floating`'s launcher lets go; a stderr log would show it twice.
    if file_log && let Err(e) = &ran {
        tracing::error!(error = ?e, "pixtuoid exited with an error");
    }
    // A quit by key, or an error, can leave a panel job running on its own
    // thread, whose CLI nothing holds to its limit once this process is gone.
    crate::install::io::kill_running();
    // A killed CLI's change still restores what it wrote, and queued saves are written.
    crate::within(
        "off-loop-threads-end",
        crate::EXIT_WAIT,
        crate::panels::join_off_loop_threads,
    );
    // Before the re-raise, which ends the process with the queue unwritten.
    drop(log_flush);
    // After every Drop the run held: the unwind is done, so the signal can now
    // end the process the way it would have.
    if let Some(signal) = ran? {
        signal.reraise();
    }
    Ok(())
}

/// Run `cmd`: apart from [`run`] so every failure in it, a `?` included,
/// reaches [`run`]'s log. The quit signal that ended a `run` (the TUI or
/// headless), `floating` or `serve` command is [`run`]'s to re-raise.
fn dispatch(cmd: Cmd, drift: doctor::DriftSeen) -> Result<Option<runtime::QuitSignal>> {
    let quit = match cmd {
        Cmd::Run {
            source,
            max_desks: cli_max_desks,
            headless,
            look,
            paint,
        } => {
            let rc = build_run_config(paint, source, cli_max_desks, headless, look, drift)?;
            runtime::run(rc)?
        }
        Cmd::Floating {
            source,
            paint,
            notify_ready,
        } => {
            // No desk cap: floating seeds its capacity from the window.
            let rc = build_run_config(
                paint,
                source,
                None,
                false,
                crate::cli::LookArgs::default(),
                drift,
            )?;
            floating::run(rc, notify_ready)?
        }
        #[cfg(feature = "serve")]
        Cmd::Serve { source, port } => {
            // Headless's boot: no painter.
            let rc = build_run_config(
                crate::cli::PaintArgs::default(),
                source,
                None,
                true,
                crate::cli::LookArgs::default(),
                drift,
            )?;
            Some(crate::serve::run(rc, port)?)
        }
        #[cfg(feature = "serve")]
        Cmd::Pair { host, port, reset } => {
            crate::serve::pair(host, port, reset)?;
            None
        }
        // homebrew-core contract: it exits 0 whatever it reports, and the
        // formula's `test do` reads its output only on that exit.
        Cmd::Doctor { look, protocol } => {
            let report = doctor::run(
                &crate::run_log::LogLocation::from_env(),
                crate::graphics::LookChoice::new(look, protocol),
            )?;
            write!(crate::cli_stdout(), "{report}")?;
            None
        }
        Cmd::Sources {
            action: None,
            output: JsonArgs { json },
        } => {
            sources_cli::run_sources_list(json)?;
            None
        }
        Cmd::Sources {
            action:
                Some(SourcesAction::Set {
                    sources: SourceIdArgs { ids },
                }),
            output: JsonArgs { json },
        } => {
            sources_cli::run_sources_set(&ids, json)?;
            None
        }
        Cmd::Connect {
            sources: SourceIdArgs { ids },
            output: JsonArgs { json },
        } => {
            sources_cli::run_change(&ids, json, sources::connect)?;
            None
        }
        Cmd::Disconnect {
            sources: SourceIdArgs { ids },
            output: JsonArgs { json },
        } => {
            sources_cli::run_change(&ids, json, sources::disconnect)?;
            None
        }
        Cmd::Setup { yes } => {
            sources_cli::run_setup(yes)?;
            None
        }
        // Packaging interfaces: stdout carries ONLY the generated artifact (the
        // tracing subscriber writes to stderr) so the homebrew
        // `generate_completions_from_executable` / `man` capture stays clean.
        Cmd::Completions { shell } => {
            use clap::CommandFactory;
            // Into a buffer first: clap_complete panics on a failed write.
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "pixtuoid", &mut script);
            crate::cli_stdout().write_all(&script)?;
            None
        }
        Cmd::Man => {
            crate::cli::render_man(&mut crate::cli_stdout())?;
            None
        }
    };
    Ok(quit)
}

/// Resolve the shared [`runtime::RunConfig`] — the common prelude for `run`
/// (TUI), `floating` (window) and `serve`.
fn build_run_config(
    cli_paint: crate::cli::PaintArgs,
    source: SourceArgs,
    cli_max_desks: Option<usize>,
    headless: bool,
    cli_look: crate::cli::LookArgs,
    drift: crate::doctor::DriftSeen,
) -> Result<runtime::RunConfig> {
    let SourceArgs { socket } = source;
    let cfg_path = config::config_path();
    let mut cfg_warnings = Vec::new();
    let (cfg, load_degraded) = config::load_with_status(&cfg_path, &mut cfg_warnings);
    let theme = config::resolve_theme(&cfg, cli_paint.theme.as_deref(), &mut cfg_warnings)?;
    let desk_cap = config::resolve_desk_cap(&cfg, cli_max_desks, &mut cfg_warnings);
    let pets = config::resolve_pets(&cfg, &mut cfg_warnings);
    let look = crate::graphics::LookChoice::new(
        config::resolve_look(&cfg, cli_look.look, &mut cfg_warnings),
        cli_look.protocol,
    );
    let motion = config::resolve_motion(&cfg, &mut cfg_warnings);
    let connected = sources::connected(&cfg);
    let first_run = sources::is_first_run(&connected, load_degraded);
    if !headless {
        // Config problems must reach stderr BEFORE any alternate screen / window,
        // not just the log file. Headless already has a stderr tracing subscriber,
        // so re-printing there would duplicate.
        for w in &cfg_warnings {
            let _ = writeln!(std::io::stderr(), "⚠ pixtuoid: {w}");
        }
    }
    refresh_plugins(
        &connected,
        headless,
        sources::converge_owned,
        &mut std::io::stderr(),
    );
    if !headless {
        warn_broken_installs(&connected);
    }
    Ok(runtime::RunConfig {
        socket,
        desk_cap,
        headless,
        config_path: cfg_path,
        theme,
        pets,
        connected,
        log: crate::run_log::LogLocation::from_env(),
        drift,
        first_run,
        audio: config::resolve_audio(&cfg),
        look,
        motion,
        floating: config::resolve_floating(&cfg),
    })
}

/// Bring the connected sources' plugin files up to this build, then name on `out`
/// each one whose CLI now has a newer plugin to load ([`sources::converge_owned`]).
/// A headless run converges too, since its CLIs load the plugin, and leaves the
/// naming to the converge's own log line.
fn refresh_plugins(
    connected: &std::collections::HashSet<String>,
    headless: bool,
    converge: impl FnOnce(&std::collections::HashSet<String>) -> Vec<&'static install::target::Target>,
    out: &mut impl Write,
) {
    let refreshed = converge(connected);
    if headless {
        return;
    }
    for t in refreshed {
        let _ = writeln!(out, "{}", sources::refreshed_notice(t));
    }
}

/// Warn on stderr when a CONNECTED source's hooks are installed but structurally
/// BROKEN — it renders zero sprites with no other hint, so the passive user who
/// never opens the Sources panel still learns. Iterates TARGETS, not the source
/// registry: only an install-bearing source can be install-BROKEN.
fn warn_broken_installs(connected: &std::collections::HashSet<String>) {
    for &t in install::TARGETS {
        if !connected.contains(t.core_source) {
            continue;
        }
        if let Some(line) =
            broken_install_warning(t.core_source, &doctor::diagnose(t.core_source, "", None))
        {
            let _ = writeln!(std::io::stderr(), "{line}");
        }
    }
}

/// [`warn_broken_installs`]'s line for `source`, when its install is broken.
fn broken_install_warning(source: &str, diag: &doctor::SourceDiagnostics) -> Option<String> {
    diag.broken_summary().map(|broken| {
        format!(
            "⚠ pixtuoid: {source} {broken} — reconnect in the Sources panel (press {})",
            crate::panels::keys::SOURCES
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_install_warning_names_the_source_and_its_summary() {
        let broken = doctor::SourceDiagnostics {
            install: Some(install::verify::SchemaVerifyResult {
                issues: vec!["shim binary missing: /x".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            broken_install_warning("hermes", &broken).as_deref(),
            Some(
                "⚠ pixtuoid: hermes install broken: shim binary missing: /x — reconnect in the Sources panel (press s)"
            )
        );
        assert_eq!(
            broken_install_warning("hermes", &doctor::SourceDiagnostics::default()),
            None
        );
    }

    #[test]
    fn both_boot_modes_converge_and_only_an_interactive_one_prints() {
        for headless in [true, false] {
            let mut converged = false;
            let mut out = Vec::new();
            refresh_plugins(
                &std::collections::HashSet::new(),
                headless,
                |_| {
                    converged = true;
                    vec![&install::target::OPENCODE]
                },
                &mut out,
            );
            assert!(converged, "headless: {headless}");
            assert_eq!(out.is_empty(), headless, "headless: {headless}");
        }
    }
}
