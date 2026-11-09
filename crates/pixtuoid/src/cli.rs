use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use pixtuoid_scene::look::LookKind;

#[derive(Debug, Parser)]
#[command(
    name = "pixtuoid",
    version,
    about = "Terminal pixel-art office for AI coding agents"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,

    /// Log verbosity.
    ///
    /// The TUI and the floating window always log warn+ to a file per run (in
    /// ~/.cache/pixtuoid/logs/ or $XDG_STATE_HOME/pixtuoid/logs/, or the one file
    /// $PIXTUOID_LOG names); debug/trace raise the file's verbosity. Everything
    /// else logs to stderr at this level. ($RUST_LOG remains the escape hatch for
    /// full directive syntax.)
    #[arg(long, global = true, value_enum, default_value = "info")]
    pub log_level: LogLevel,
}

/// The flags every painter takes, flattened into `run` and `floating`.
#[derive(Debug, Default, clap::Args)]
pub(crate) struct PaintArgs {
    // A `String`, not a typed enum: `config::resolve_theme` is the one place a
    // theme name is validated.
    #[arg(long, help = theme_help())]
    pub(crate) theme: Option<String>,
}

/// `--theme`'s help text, DERIVED from the `ALL_THEMES` registry rather than
/// hand-copied: this clap tree is what `--help`, the generated shell completions
/// AND the packaged man page all render from.
fn theme_help() -> String {
    let names: Vec<&str> = pixtuoid_scene::theme::ALL_THEMES
        .iter()
        .map(|t| t.name)
        .collect();
    format!("Color theme: {}.", names.join(", "))
}

/// `run`'s look flags: `--look` is resolved over the `look` config key by
/// [`crate::config::resolve_look`].
#[derive(Debug, Default, Clone, Copy, clap::Args)]
pub(crate) struct LookArgs {
    /// Which office to paint: the cutaway or the classic.
    ///
    /// The cutaway is drawn in terminal graphics (the protocols `--protocol`
    /// lists), the classic in half-blocks. Overrides the `look` config key; the
    /// default is `auto`. `pixtuoid doctor` shows what this terminal supports.
    #[arg(long, value_enum)]
    pub(crate) look: Option<crate::graphics::LookMode>,
    /// Draw the cutaway in this protocol whatever protocol the terminal reports.
    ///
    /// It implies `--look cutaway`.
    #[arg(long, value_enum, conflicts_with = "look")]
    pub(crate) protocol: Option<crate::graphics::ImageProtocol>,
}

/// The source-input flags shared VERBATIM by `run`, `floating` and `serve` (the
/// commands that wire sources → reducer → scene), flattened into each so they
/// can't drift.
#[derive(Debug, clap::Args)]
pub(crate) struct SourceArgs {
    /// Listen for agent hooks on this socket instead of the default.
    // Empty/whitespace is rejected: an explicit override deserves a loud
    // answer, and bind("") would strand a relative `.lock` in the CWD.
    #[arg(long, value_parser = parse_nonempty_path, value_hint = clap::ValueHint::FilePath)]
    pub socket: Option<PathBuf>,
}

/// The source ids `connect`, `disconnect` and `sources set` act on.
#[derive(Debug, clap::Args)]
pub(crate) struct SourceIdArgs {
    /// Source ids, e.g. `claude-code codex`.
    #[arg(required = true)]
    pub(crate) ids: Vec<String>,
}

/// `--json`, for the commands whose output a script reads.
#[derive(Debug, Clone, Copy, clap::Args)]
pub(crate) struct JsonArgs {
    /// Emit machine-readable JSON instead of text.
    // `global` so `sources` honors it both before and after `set` (`sources
    // --json set …` == `sources set … --json`), the natural Raycast/script form.
    #[arg(long, global = true)]
    pub(crate) json: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Cmd {
    /// Run the TUI (default if no subcommand given).
    Run {
        #[command(flatten)]
        source: SourceArgs,
        /// Cap desks per floor, ≥ 1 (auto-computed from terminal size if
        /// unset).
        // 0 is rejected: it would zero every floor's capacity, so every agent
        // would wait for a desk forever.
        #[arg(long, hide = true, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..))]
        max_desks: Option<usize>,
        /// Skip the TUI entirely — useful for CI / scripting.
        ///
        /// Prints a one-line `agents=[…] daemons=[…]` summary on each change
        /// of the scene.
        #[arg(long, default_value_t = false)]
        headless: bool,
        #[command(flatten)]
        look: LookArgs,
        #[command(flatten)]
        paint: PaintArgs,
    },
    /// Render the live office in a frameless, always-on-top desktop window.
    Floating {
        #[command(flatten)]
        source: SourceArgs,
        #[command(flatten)]
        paint: PaintArgs,
    },
    /// Serve the office to paired devices on your local network.
    ///
    /// It needs no TUI. Agent status, model, usage, repo names, which agent CLIs
    /// you have and how each is connected, and the machine's name and OS leave
    /// it, never paths, prompts, tool input or transcripts, and it announces the
    /// machine's name and its key's SPKI hash on the network. A paired device can
    /// also connect or disconnect those CLIs. Nothing listens on a network unless
    /// this runs.
    #[cfg(feature = "serve")]
    Serve {
        #[command(flatten)]
        source: SourceArgs,
        /// The TCP port to listen on.
        #[arg(long, default_value_t = crate::serve::DEFAULT_PORT)]
        port: u16,
    },
    /// Print a one-time QR code that pairs a device with `serve`, running or not.
    #[cfg(feature = "serve")]
    Pair {
        /// The hostname or IPv4 address a device reaches this machine at.
        ///
        /// Defaults to this Mac's `.local` name, elsewhere to the default route's
        /// local-network IPv4.
        #[arg(long)]
        host: Option<String>,
        /// The port `serve` listens on.
        #[arg(
            long,
            default_value_t = crate::serve::DEFAULT_PORT,
            value_parser = clap::value_parser!(u16).range(1..),
        )]
        port: u16,
        /// Forget every paired device instead, ending their streams; each must
        /// pair again.
        #[arg(long, conflicts_with_all = ["host", "port"])]
        reset: bool,
    },
    /// Diagnose source health: connection, hooks, and decode drift.
    ///
    /// It reads decode drift from the log. Changes no config.
    Doctor {
        /// The look the `graphics:` row plans, as `run --look` takes it.
        ///
        /// Defaults to `cutaway` whatever the `look` config key says: the row
        /// reports what this terminal can do.
        #[arg(long, value_enum, default_value_t = crate::graphics::LookMode::Named(LookKind::Cutaway))]
        look: crate::graphics::LookMode,
        /// The protocol the row plans the cutaway in, as `run --protocol`
        /// takes it.
        #[arg(long, value_enum, conflicts_with = "look")]
        protocol: Option<crate::graphics::ImageProtocol>,
    },
    /// List agent sources + their connection state, or apply a set.
    ///
    /// The scriptable twin of the in-TUI Sources panel.
    Sources {
        #[command(subcommand)]
        action: Option<SourcesAction>,
        #[command(flatten)]
        output: JsonArgs,
    },
    /// Connect one or more sources: install their hooks + persist the choice.
    ///
    /// A running office reflects it on its next launch.
    Connect {
        #[command(flatten)]
        sources: SourceIdArgs,
        #[command(flatten)]
        output: JsonArgs,
    },
    /// Disconnect one or more sources: remove their hooks + persist the choice.
    Disconnect {
        #[command(flatten)]
        sources: SourceIdArgs,
        #[command(flatten)]
        output: JsonArgs,
    },
    /// First-run setup: detect installed agent CLIs and connect them.
    ///
    /// The headless twin of the in-TUI onboarding. Without `--yes` it's a DRY RUN
    /// that only prints what it would connect.
    Setup {
        /// Apply: actually connect the detected CLIs (install their hooks).
        ///
        /// Without it, `setup` only previews the detected set.
        // Opt-in: applying writes to another tool's config.
        #[arg(long)]
        yes: bool,
    },
    /// Print a shell completion script to stdout.
    ///
    /// For example `pixtuoid completions zsh > ~/.zfunc/_pixtuoid`. Package
    /// managers install these automatically; this command lets `cargo install` /
    /// `npm` users do it themselves for any shell.
    // Maintainer note stays a `//`: a `///` here would be SHIPPED PRODUCT COPY
    // (clap renders it as this subcommand's long help). homebrew-core's `install`
    // block calls `generate_completions_from_executable(bin/"pixtuoid",
    // "completions")`, so renaming this subcommand breaks their BUILD. Same for
    // `Man`.
    Completions {
        /// Target shell.
        shell: clap_complete::Shell,
    },
    /// Print the roff man page to stdout.
    ///
    /// For example `pixtuoid man > pixtuoid.1`.
    // Also a `//`: `hide = true` drops this from the PARENT listing, not from
    // `pixtuoid man --help`. homebrew-core's `install` captures it via
    // `Utils.safe_popen_read` — see the contract note on `Completions`.
    #[command(hide = true)]
    Man,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SourcesAction {
    /// Make these ids the connected set: every other source disconnects.
    // Declarative: it backs the Raycast multi-select checkbox-form.
    Set {
        #[command(flatten)]
        sources: SourceIdArgs,
    },
}

/// `--log-level` values. A typed enum (not a free `String`) so a typo like
/// `dbug` is a hard clap parse error instead of silently parsing as an
/// `EnvFilter` TARGET directive (`dbug=trace`) that filters everything off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// The tracing level token every `EnvFilter` is built from.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

impl Cli {
    pub(crate) fn cmd_or_default(self) -> (LogLevel, Cmd) {
        let cmd = self.cmd.unwrap_or(Cmd::Run {
            source: SourceArgs { socket: None },
            max_desks: None,
            headless: false,
            look: LookArgs::default(),
            paint: PaintArgs::default(),
        });
        (self.log_level, cmd)
    }
}

fn parse_nonempty_path(s: &str) -> Result<PathBuf, String> {
    if s.trim().is_empty() {
        Err("must not be empty — pass a path, or drop the flag for the default".into())
    } else {
        Ok(PathBuf::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // It binds a free hook endpoint and leaves its lock, so "read-only" would lie.
    #[test]
    fn doctors_help_promises_only_what_it_keeps() {
        let command = <Cli as clap::CommandFactory>::command();
        let doctor = command.find_subcommand("doctor").expect("a subcommand");
        let about = doctor.get_long_about().or(doctor.get_about());
        let about = about.expect("documented").to_string();
        assert!(about.contains("Changes no config"), "{about}");
        assert!(!about.to_lowercase().contains("read-only"), "{about}");
    }

    /// `pair` finds a host on every OS it runs on, so `--host` is required on none.
    #[cfg(feature = "serve")]
    #[test]
    fn pairs_host_help_names_a_default_on_every_os() {
        let command = <Cli as clap::CommandFactory>::command();
        let pair = command.find_subcommand("pair").expect("a subcommand");
        let host = pair
            .get_arguments()
            .find(|arg| arg.get_id() == "host")
            .and_then(|arg| arg.get_long_help())
            .expect("documented")
            .to_string();
        assert!(host.contains("default route"), "{host}");
        assert!(!host.contains("required"), "{host}");
    }

    /// The flags have to REACH `doctor`, not merely parse: an accepted value
    /// that never arrives is the declared-but-unwired class.
    #[test]
    fn doctor_takes_look_and_protocol() {
        use crate::graphics::{ImageProtocol, LookMode};
        let doctor = |args: &[&str]| match Cli::try_parse_from(args).expect("parses").cmd {
            Some(Cmd::Doctor { look, protocol }) => (look, protocol),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            doctor(&["pixtuoid", "doctor"]),
            (LookMode::Named(LookKind::Cutaway), None),
            "absent is what this terminal can do"
        );
        assert_eq!(
            doctor(&["pixtuoid", "doctor", "--look", "classic"]),
            (LookMode::Named(LookKind::Classic), None)
        );
        assert_eq!(
            doctor(&["pixtuoid", "doctor", "--protocol", "sixel"]),
            (
                LookMode::Named(LookKind::Cutaway),
                Some(ImageProtocol::Sixel)
            )
        );
    }

    /// Absent stays `None` so the config key can speak; every look and every
    /// protocol is a value.
    #[test]
    fn run_takes_look_and_protocol_and_leaves_absence_to_the_config() {
        use crate::graphics::{ImageProtocol, LookMode};
        let look = |args: &[&str]| match Cli::try_parse_from(args).expect("parses").cmd {
            Some(Cmd::Run { look, .. }) => (look.look, look.protocol),
            other => panic!("{other:?}"),
        };
        assert_eq!(look(&["pixtuoid", "run"]), (None, None));
        for (value, mode) in [
            ("auto", LookMode::Auto),
            ("cutaway", LookMode::Named(LookKind::Cutaway)),
            ("classic", LookMode::Named(LookKind::Classic)),
        ] {
            assert_eq!(
                look(&["pixtuoid", "run", "--look", value]),
                (Some(mode), None)
            );
        }
        for (value, protocol) in [
            ("kitty", ImageProtocol::Kitty),
            ("sixel", ImageProtocol::Sixel),
            ("iterm2", ImageProtocol::Iterm2),
        ] {
            assert_eq!(
                look(&["pixtuoid", "run", "--protocol", value]),
                (None, Some(protocol))
            );
        }
    }

    /// `--protocol` implies the cutaway, so a `--look` beside it, classic
    /// above all, is refused.
    #[test]
    fn look_with_protocol_is_a_conflict() {
        for cmd in ["doctor", "run"] {
            let err =
                Cli::try_parse_from(["pixtuoid", cmd, "--look", "classic", "--protocol", "kitty"])
                    .unwrap_err();
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{cmd}"
            );
        }
    }

    #[test]
    fn look_and_protocol_typos_are_hard_parse_errors() {
        for cmd in ["doctor", "run"] {
            for (flag, typo) in [("--look", "off"), ("--protocol", "auto")] {
                let err = Cli::try_parse_from(["pixtuoid", cmd, flag, typo]).unwrap_err();
                assert_eq!(
                    err.kind(),
                    clap::error::ErrorKind::InvalidValue,
                    "{cmd} {flag}"
                );
            }
        }
    }

    #[test]
    fn empty_socket_is_a_hard_parse_error() {
        for v in ["", "   "] {
            let err = Cli::try_parse_from(["pixtuoid", "run", "--socket", v]).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        }
    }

    #[test]
    fn log_level_typo_is_a_hard_parse_error() {
        let err = Cli::try_parse_from(["pixtuoid", "--log-level", "dbug"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn max_desks_zero_is_a_hard_parse_error() {
        let err = Cli::try_parse_from(["pixtuoid", "run", "--max-desks", "0"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn log_level_valid_values_map_to_the_same_filter_tokens_as_before() {
        for (raw, lvl) in [
            ("error", LogLevel::Error),
            ("warn", LogLevel::Warn),
            ("info", LogLevel::Info),
            ("debug", LogLevel::Debug),
            ("trace", LogLevel::Trace),
        ] {
            let cli = Cli::try_parse_from(["pixtuoid", "--log-level", raw]).unwrap();
            assert_eq!(cli.log_level, lvl);
            assert_eq!(lvl.as_str(), raw, "filter token must be unchanged");
            assert!(
                tracing_subscriber::EnvFilter::try_new(lvl.as_str()).is_ok(),
                "{raw} must parse as an EnvFilter level"
            );
        }
    }

    #[test]
    fn max_desks_positive_parses() {
        let cli = Cli::try_parse_from(["pixtuoid", "run", "--max-desks", "4"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Run {
                max_desks: Some(4),
                ..
            })
        ));
    }

    #[test]
    fn cmd_or_default_returns_run_when_no_subcommand() {
        let cli = Cli {
            cmd: None,
            log_level: LogLevel::Info,
        };
        let (level, cmd) = cli.cmd_or_default();
        assert_eq!(level, LogLevel::Info);
        assert!(matches!(
            cmd,
            Cmd::Run {
                headless: false,
                max_desks: None,
                look: LookArgs {
                    look: None,
                    protocol: None
                },
                paint: PaintArgs { theme: None },
                ..
            }
        ));
    }

    #[test]
    fn cmd_or_default_preserves_explicit_subcommand() {
        let cli = Cli {
            cmd: Some(Cmd::Doctor {
                look: crate::graphics::LookMode::Named(LookKind::Classic),
                protocol: None,
            }),
            log_level: LogLevel::Debug,
        };
        let (level, cmd) = cli.cmd_or_default();
        assert_eq!(level, LogLevel::Debug);
        assert!(matches!(cmd, Cmd::Doctor { .. }));
    }

    #[test]
    fn floating_subcommand_parses_with_shared_run_flags() {
        let cli = Cli::try_parse_from(["pixtuoid", "floating", "--socket", "/tmp/p.sock"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Floating {
                source: SourceArgs { socket: Some(_) },
                ..
            })
        ));
    }

    #[cfg(feature = "serve")]
    #[test]
    fn pair_refuses_port_zero_and_a_reset_with_a_link_flag() {
        for (args, kind) in [
            (
                &["pixtuoid", "pair", "--port", "0"][..],
                clap::error::ErrorKind::ValueValidation,
            ),
            (
                &["pixtuoid", "pair", "--reset", "--host", "h"],
                clap::error::ErrorKind::ArgumentConflict,
            ),
            (
                &["pixtuoid", "pair", "--reset", "--port", "1"],
                clap::error::ErrorKind::ArgumentConflict,
            ),
        ] {
            let err = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(err.kind(), kind, "{args:?}");
        }
    }

    #[test]
    fn sources_set_parses_its_ids() {
        let cli = Cli::try_parse_from(["pixtuoid", "sources", "set", "codex", "cursor"]).unwrap();
        match cli.cmd {
            Some(Cmd::Sources {
                action:
                    Some(SourcesAction::Set {
                        sources: SourceIdArgs { ids },
                    }),
                ..
            }) => assert_eq!(ids, vec!["codex".to_string(), "cursor".to_string()]),
            other => panic!("expected sources set, got {other:?}"),
        }
    }

    #[test]
    fn sources_json_is_global_across_the_set_subcommand() {
        for args in [
            ["pixtuoid", "sources", "--json", "set", "codex"],
            ["pixtuoid", "sources", "set", "codex", "--json"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(
                matches!(
                    cli.cmd,
                    Some(Cmd::Sources {
                        action: Some(SourcesAction::Set { .. }),
                        output: JsonArgs { json: true },
                    })
                ),
                "args {args:?} must parse json=true"
            );
        }
    }

    #[test]
    fn connect_requires_at_least_one_id() {
        assert!(Cli::try_parse_from(["pixtuoid", "connect"]).is_err());
    }

    #[test]
    fn connect_and_disconnect_parse_multiple_ids() {
        let c = Cli::try_parse_from(["pixtuoid", "connect", "claude-code", "codex"]).unwrap();
        assert!(matches!(c.cmd, Some(Cmd::Connect { .. })));
        let d = Cli::try_parse_from(["pixtuoid", "disconnect", "codex", "--json"]).unwrap();
        assert!(matches!(
            d.cmd,
            Some(Cmd::Disconnect {
                output: JsonArgs { json: true },
                ..
            })
        ));
    }

    #[test]
    fn setup_defaults_to_dry_run_and_takes_yes() {
        let dry = Cli::try_parse_from(["pixtuoid", "setup"]).unwrap();
        assert!(matches!(dry.cmd, Some(Cmd::Setup { yes: false })));
        let apply = Cli::try_parse_from(["pixtuoid", "setup", "--yes"]).unwrap();
        assert!(matches!(apply.cmd, Some(Cmd::Setup { yes: true })));
    }

    #[test]
    fn theme_help_names_every_registered_theme() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let help = cmd
            .find_subcommand("run")
            .expect("a subcommand")
            .get_arguments()
            .find(|a| a.get_id() == "theme")
            .and_then(|a| a.get_help())
            .map(ToString::to_string)
            .expect("--theme carries help text");
        for t in pixtuoid_scene::theme::ALL_THEMES {
            assert!(
                help.contains(t.name),
                "--theme help omits the registered theme {:?} — it must read the \
                 ALL_THEMES authority, not a copy",
                t.name
            );
        }
    }

    /// `--theme` is taken only where a theme is painted, so any other command
    /// refuses it rather than dropping it.
    #[test]
    fn only_the_painters_take_theme() {
        const PAINTERS: [&str; 2] = ["run", "floating"];
        // Unbuilt: a build adds `help`, whose children reuse every command's name.
        let mut commands = vec![<Cli as clap::CommandFactory>::command()];
        while let Some(command) = commands.pop() {
            let name = command.get_name();
            let takes = command.get_arguments().any(|a| a.get_id() == "theme");
            assert_eq!(takes, PAINTERS.contains(&name), "{name}");
            commands.extend(command.get_subcommands().cloned());
        }
        for args in [
            &["pixtuoid", "doctor", "--theme", "normal"][..],
            &["pixtuoid", "--theme", "normal", "doctor"],
        ] {
            let err = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(
                (err.kind(), err.exit_code()),
                (clap::error::ErrorKind::UnknownArgument, 2),
                "{args:?}"
            );
        }
        // Reaching the painter, not merely parsing.
        for painter in PAINTERS {
            let cli = Cli::try_parse_from(["pixtuoid", painter, "--theme", "normal"]);
            match cli.expect("parses").cmd {
                Some(Cmd::Run { paint, .. } | Cmd::Floating { paint, .. }) => {
                    assert_eq!(paint.theme.as_deref(), Some("normal"), "{painter}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    /// Each command a `--help` lists gets one line, and each visible flag or
    /// positional a one-line summary: a doc's first paragraph, the rest being
    /// the long `--help`.
    #[test]
    fn every_help_summary_is_present_and_fits_one_line() {
        // clap's own help width where it can't read the terminal's
        // (`clap::Command::term_width`).
        const HELP_WIDTH: usize = 100;
        let mut root = <Cli as clap::CommandFactory>::command();
        root.build();
        let mut commands = vec![root];
        while let Some(mut command) = commands.pop() {
            let name = command.get_name().to_owned();
            for arg in command.get_arguments().filter(|a| !a.is_hide_set()) {
                let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
                assert!(
                    !help.is_empty() && help.chars().count() <= HELP_WIDTH,
                    "`{name}`'s {} summary: {help:?}",
                    arg.get_id()
                );
            }
            let help = command.render_long_help().to_string();
            let listing = help
                .split_once("Commands:\n")
                .and_then(|(_, rest)| rest.split("\n\n").next())
                .unwrap_or_default();
            for line in listing.lines() {
                assert!(
                    line.chars().count() <= HELP_WIDTH,
                    "`{name} --help` lists:\n{line}"
                );
            }
            commands.extend(command.get_subcommands().cloned());
        }
    }

    #[test]
    fn packaging_subcommand_help_carries_no_maintainer_only_prose() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        for name in ["completions", "man"] {
            let help = cmd
                .find_subcommand_mut(name)
                .expect("subcommand exists")
                .render_long_help()
                .to_string()
                .to_lowercase();
            for marker in [
                "homebrew",
                "autobump",
                "generate_completions_from_executable",
            ] {
                assert!(
                    !help.contains(marker),
                    "`pixtuoid {name} --help` leaks the maintainer note {marker:?} to end users"
                );
            }
        }
    }

    #[test]
    fn completions_and_man_generate_non_empty_and_cover_subcommands() {
        use clap::CommandFactory;

        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
        ] {
            let mut buf = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "pixtuoid", &mut buf);
            let script = String::from_utf8(buf).expect("completion script is utf-8");
            assert!(!script.is_empty(), "{shell:?} completion script is empty");
            assert!(
                script.contains("floating") && script.contains("doctor"),
                "{shell:?} completion script is missing a known subcommand"
            );
        }

        let mut man = Vec::new();
        clap_mangen::Man::new(Cli::command())
            .render(&mut man)
            .expect("man render");
        let man = String::from_utf8(man).expect("man page is utf-8");
        assert!(man.contains("pixtuoid"), "man page missing program name");
        assert!(
            man.contains("doctor"),
            "man page SUBCOMMANDS section is missing a known subcommand"
        );
    }
}
