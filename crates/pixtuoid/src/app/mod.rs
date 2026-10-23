//! The program behind `main`: parse the command line, set up the terminal,
//! logging and crash hook, and dispatch the subcommand. The binary is only
//! `main.rs`'s call into [`run`] and its error report
//! (<https://doc.rust-lang.org/book/ch12-03-improving-error-handling-and-modularity.html>).

mod crash;
pub(crate) mod logging;
mod sources_cli;

use std::io::Write;

use crate::cli::{Cli, Cmd, SourceArgs, SourcesAction};
use crate::{config, doctor, floating, install, runtime, sources};
use anyhow::Result;
use clap::Parser;

/// Run the command line's subcommand.
///
/// # Errors
///
/// The subcommand's failure, which `main` reports through
/// [`crate::fatal_error_text`].
pub fn run() -> Result<()> {
    crash::install_crash_hook();
    let (log_level, cli_theme, cmd) = Cli::parse().cmd_or_default();

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
    // explanation rather than render block-soup. crossterm needs the
    // $CLICOLOR_FORCE override applied explicitly — it ignores the var on its own.
    // Runs before the truecolor probe, so a dumb terminal never gets DECRQSS.
    if is_run_tui {
        use crate::term::ColorPreflight;
        match crate::term::color_preflight(
            pixtuoid_core::platform::text_env("NO_COLOR").as_deref(),
            pixtuoid_core::platform::text_env("CLICOLOR_FORCE").as_deref(),
            pixtuoid_core::platform::text_env("TERM").as_deref(),
        ) {
            ColorPreflight::Proceed => {}
            ColorPreflight::ForceColor => crossterm::style::force_color_output(true),
            ColorPreflight::RefuseNoColor => {
                let _ = writeln!(
                    std::io::stderr(),
                    "pixtuoid: $NO_COLOR is set, so color output is disabled — the \
                     pixel-art office is 24-bit color with no legible monochrome mode \
                     and would render as unreadable blocks. Unset NO_COLOR (or set \
                     CLICOLOR_FORCE=1 to override) to run it, or use \
                     `pixtuoid run --headless` for a text summary."
                );
                return Ok(());
            }
            ColorPreflight::RefuseDumbTerm => {
                let _ = writeln!(
                    std::io::stderr(),
                    "pixtuoid: $TERM=dumb — this terminal can't render the pixel-art \
                     office (no cursor addressing or color). Use a graphical terminal \
                     (Windows Terminal, iTerm2, Ghostty, Alacritty, kitty, WezTerm), \
                     or `pixtuoid run --headless` for a text summary."
                );
                return Ok(());
            }
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
    ) && crate::term::query_truecolor(crate::term::TRUECOLOR_PROBE_TIMEOUT).warrants_warning()
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
    let tui_active = matches!(&cmd, Cmd::Run { headless, .. } if !*headless)
        || matches!(&cmd, Cmd::Floating { .. });
    let drift = logging::init(tui_active, log_level);

    match cmd {
        Cmd::Run {
            source,
            max_desks: cli_max_desks,
            headless,
            look,
        } => {
            let rc = build_run_config(
                cli_theme.as_deref(),
                source,
                cli_max_desks,
                headless,
                look,
                drift,
            )?;
            // After every Drop the run held: the unwind is done, so the signal can now
            // end the process the way it would have.
            if let Some(signal) = runtime::run(rc)? {
                signal.reraise();
            }
            Ok(())
        }
        Cmd::Floating { source } => {
            // No desk cap: floating seeds its capacity from the window.
            let rc = build_run_config(
                cli_theme.as_deref(),
                source,
                None,
                false,
                crate::cli::LookArgs::default(),
                drift,
            )?;
            if let Some(signal) = floating::run(rc)? {
                signal.reraise();
            }
            Ok(())
        }
        Cmd::Doctor { look, protocol } => {
            let report = doctor::run(
                &crate::run_log::LogLocation::from_env(),
                crate::graphics::LookChoice::new(look, protocol),
            )?;
            write!(crate::cli_stdout(), "{report}")?;
            Ok(())
        }
        Cmd::Sources { action: None, json } => sources_cli::run_sources_list(json),
        Cmd::Sources {
            action: Some(SourcesAction::Set { ids }),
            json,
        } => sources_cli::run_sources_set(&ids, json),
        Cmd::Connect { ids, json } => sources_cli::run_change(&ids, json, |c, i| {
            sources::connect(c, i).map(|_| sources::ChangeOutcome::Connected)
        }),
        Cmd::Disconnect { ids, json } => {
            sources_cli::run_change(&ids, json, |c, i| match sources::disconnect(c, i)? {
                sources::DisconnectOutcome::Uninstalled(r)
                    if let Some(cli) = r.plugin_left_registered =>
                {
                    let _ = writeln!(
                        std::io::stderr(),
                        "{i}: {}",
                        sources::plugin_left_registered(cli)
                    );
                    Ok(sources::ChangeOutcome::Disconnected)
                }
                _ => Ok(sources::ChangeOutcome::Disconnected),
            })
        }
        Cmd::Setup { yes } => sources_cli::run_setup(yes),
        // Packaging interfaces: stdout carries ONLY the generated artifact (the
        // tracing subscriber writes to stderr) so the homebrew
        // `generate_completions_from_executable` / `man` capture stays clean.
        Cmd::Completions { shell } => {
            use clap::CommandFactory;
            // Into a buffer first: clap_complete panics on a failed write.
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "pixtuoid", &mut script);
            crate::cli_stdout().write_all(&script)?;
            Ok(())
        }
        Cmd::Man => {
            use clap::CommandFactory;
            clap_mangen::Man::new(Cli::command()).render(&mut crate::cli_stdout())?;
            Ok(())
        }
    }
}

/// Resolve the shared [`runtime::RunConfig`] — the common prelude for `run`
/// (TUI) and `floating` (window).
fn build_run_config(
    cli_theme: Option<&str>,
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
    let theme = config::resolve_theme(&cfg, cli_theme, &mut cfg_warnings)?;
    let desk_cap = config::resolve_desk_cap(&cfg, cli_max_desks, &mut cfg_warnings);
    let pets = config::resolve_pets(&cfg, &mut cfg_warnings);
    let look = crate::graphics::LookChoice::new(
        config::resolve_look(&cfg, cli_look.look, &mut cfg_warnings),
        cli_look.protocol,
    );
    let motion = config::resolve_motion(&cfg, &mut cfg_warnings);
    let adopted =
        adopt_unless_headless(headless, || sources::adopt_flagged_targets(&cfg_path, &cfg));
    let connected = sources::connected(&cfg);
    let first_run = sources::is_first_run(&connected, load_degraded);
    if !headless {
        // Config problems must reach stderr BEFORE any alternate screen / window,
        // not just the log file. Headless already has a stderr tracing subscriber,
        // so re-printing there would duplicate.
        for w in &cfg_warnings {
            let _ = writeln!(std::io::stderr(), "⚠ pixtuoid: {w}");
        }
        for (t, installed) in &adopted {
            let _ = writeln!(
                std::io::stderr(),
                "{}",
                sources::adopted_notice(t, installed)
            );
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
    })
}

/// Run `adopt` ([`sources::adopt_flagged_targets`]) on an interactive launch
/// only: adoption installs hooks, and a headless run's flags are no user's
/// consent.
fn adopt_unless_headless<T>(headless: bool, adopt: impl FnOnce() -> Vec<T>) -> Vec<T> {
    if headless { Vec::new() } else { adopt() }
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
        let diag = doctor::diagnose(t.core_source, "", None);
        if let Some(broken) = diag.broken_install() {
            let _ = writeln!(
                std::io::stderr(),
                "⚠ pixtuoid: {} hooks are installed but BROKEN: {} — \
                 reconnect in the Sources panel (press s)",
                t.core_source,
                broken.issues.join("; ")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_interactive_launch_adopts() {
        let ran = std::cell::Cell::new(0);
        let adopt = || {
            ran.set(ran.get() + 1);
            vec![()]
        };
        assert!(adopt_unless_headless(true, adopt).is_empty());
        assert_eq!(ran.get(), 0, "headless never adopts");
        assert_eq!(adopt_unless_headless(false, adopt).len(), 1);
        assert_eq!(ran.get(), 1);
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
