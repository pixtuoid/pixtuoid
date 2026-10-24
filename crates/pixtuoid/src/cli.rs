use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "pixtuoid",
    version,
    about = "Terminal pixel-art office for AI coding agents"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,

    /// Log verbosity. The TUI always logs warn+ to a file per run
    /// (in ~/.cache/pixtuoid/logs/ or $XDG_STATE_HOME/pixtuoid/logs/, or
    /// the one file $PIXTUOID_LOG names); debug/trace raise the file's
    /// verbosity. Non-TUI commands log to stderr at this level.
    /// ($RUST_LOG remains the escape hatch for full directive syntax.)
    #[arg(long, global = true, value_enum, default_value = "info")]
    pub log_level: LogLevel,

    #[arg(long, global = true, help = theme_help())]
    pub theme: Option<String>,
}

/// `--theme`'s help text, DERIVED from the `ALL_THEMES` registry rather than
/// hand-copied: this clap tree is what `--help`, the generated shell completions
/// AND the packaged man page all render from. The flag stays an untyped
/// `Option<String>` on purpose — `config::resolve_theme` is the ONE place a theme
/// name is validated.
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
    /// Which office to paint: the cutaway, drawn in terminal graphics
    /// (kitty/iTerm2/SIXEL), or the classic half-blocks. Overrides the `look`
    /// config key; the default is `auto`. `pixtuoid doctor` shows what this
    /// terminal supports.
    #[arg(long, value_enum)]
    pub(crate) look: Option<crate::graphics::LookMode>,
    /// Draw the cutaway in this protocol whatever the terminal answers; it
    /// implies `--look cutaway`.
    #[arg(long, value_enum, conflicts_with = "look")]
    pub(crate) protocol: Option<crate::graphics::ImageProtocol>,
}

/// The source-input flags shared VERBATIM by `run` and `floating` (the two
/// commands that wire sources → reducer → renderer), flattened into both so
/// they can't drift.
#[derive(Debug, clap::Args)]
pub(crate) struct SourceArgs {
    /// Empty/whitespace is rejected: an explicit override deserves a loud
    /// answer, and bind("") would strand a relative `.lock` in the CWD.
    #[arg(long, value_parser = parse_nonempty_path, value_hint = clap::ValueHint::FilePath)]
    pub socket: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Cmd {
    /// Run the TUI (default if no subcommand given).
    Run {
        #[command(flatten)]
        source: SourceArgs,
        /// Cap desks per floor, ≥ 1 (auto-computed from terminal size if
        /// unset). 0 is rejected: it would permanently zero every floor's
        /// capacity and silently drop every agent (the boot atomics only
        /// grow via fetch_max, gated on capacity > 0).
        #[arg(long, hide = true, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..))]
        max_desks: Option<usize>,
        /// Skip the TUI entirely — useful for CI / scripting.
        /// Prints a one-line `agents=[label@desk:state, ...]` summary every
        /// 200ms when it changes.
        #[arg(long, default_value_t = false)]
        headless: bool,
        #[command(flatten)]
        look: LookArgs,
    },
    /// Render the live office in a frameless, always-on-top desktop window
    /// (no TUI). Shares the same source flags as `run`.
    Floating {
        #[command(flatten)]
        source: SourceArgs,
    },
    /// Diagnose source health: connection, hooks, and decode drift recorded in
    /// the log. Read-only.
    Doctor {
        /// The look the `graphics:` row plans, as `run --look` takes it.
        /// Defaults to `cutaway` whatever the `look` config key says: the row
        /// reports what this terminal can do.
        #[arg(long, value_enum, default_value_t = crate::graphics::LookMode::Cutaway)]
        look: crate::graphics::LookMode,
        /// The protocol the row plans the cutaway in, as `run --protocol`
        /// takes it.
        #[arg(long, value_enum, conflicts_with = "look")]
        protocol: Option<crate::graphics::ImageProtocol>,
    },
    /// List agent sources + their connection state, or apply a set. The
    /// scriptable twin of the in-TUI Sources panel (Raycast / automation).
    Sources {
        #[command(subcommand)]
        action: Option<SourcesAction>,
        /// Emit machine-readable JSON instead of a table. `global` so it's
        /// honored both before and after `set` (`sources --json set …` ==
        /// `sources set … --json`) — the natural Raycast/script form.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Connect one or more sources: install their hooks + persist the choice.
    /// A running office reflects it on its next launch.
    Connect {
        /// Source ids, e.g. `claude-code codex`.
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Disconnect one or more sources: remove their hooks + persist the choice.
    Disconnect {
        /// Source ids, e.g. `codex cursor`.
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// First-run setup: detect installed agent CLIs and connect them — the headless
    /// twin of the in-TUI onboarding (for Raycast / CI / scripting). Without
    /// `--yes` it's a DRY RUN that only prints what it would connect (writing to
    /// another tool's config is opt-in).
    Setup {
        /// Apply: actually connect the detected CLIs (install their hooks). Without
        /// it, `setup` only previews the detected set.
        #[arg(long)]
        yes: bool,
    },
    /// Print a shell completion script to stdout, e.g.
    /// `pixtuoid completions zsh > ~/.zfunc/_pixtuoid`. Package managers install
    /// these automatically; this command lets `cargo install` / `npm` users do it
    /// themselves for any shell.
    // Maintainer note stays a `//`: a `///` here would be SHIPPED PRODUCT COPY
    // (clap renders it as this subcommand's long help). homebrew-core's `install`
    // block calls `generate_completions_from_executable(bin/"pixtuoid",
    // "completions")`, so renaming this subcommand breaks their BUILD. Same for
    // `Man`.
    Completions {
        /// Target shell.
        shell: clap_complete::Shell,
    },
    /// Print the roff man page to stdout (`pixtuoid man > pixtuoid.1`). A
    /// packaging interface — generated from the same clap definitions as `--help`.
    // Also a `//`: `hide = true` drops this from the PARENT listing, not from
    // `pixtuoid man --help`. homebrew-core's `install` captures it via
    // `Utils.safe_popen_read` — see the contract note on `Completions`.
    #[command(hide = true)]
    Man,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SourcesAction {
    /// Make the connected set EXACTLY these ids (declarative — everything else
    /// disconnects). Backs the Raycast multi-select checkbox-form. `--json` is
    /// the global flag on `sources` (works before or after `set`).
    Set {
        #[arg(required = true)]
        ids: Vec<String>,
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
    pub(crate) fn cmd_or_default(self) -> (LogLevel, Option<String>, Cmd) {
        let level = self.log_level;
        let theme = self.theme;
        let cmd = self.cmd.unwrap_or(Cmd::Run {
            source: SourceArgs { socket: None },
            max_desks: None,
            headless: false,
            look: LookArgs::default(),
        });
        (level, theme, cmd)
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
            (LookMode::Cutaway, None),
            "absent is what this terminal can do"
        );
        assert_eq!(
            doctor(&["pixtuoid", "doctor", "--look", "classic"]),
            (LookMode::Classic, None)
        );
        assert_eq!(
            doctor(&["pixtuoid", "doctor", "--protocol", "sixel"]),
            (LookMode::Cutaway, Some(ImageProtocol::Sixel))
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
            ("cutaway", LookMode::Cutaway),
            ("classic", LookMode::Classic),
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
            theme: None,
        };
        let (level, theme, cmd) = cli.cmd_or_default();
        assert_eq!(level, LogLevel::Info);
        assert!(theme.is_none());
        assert!(matches!(
            cmd,
            Cmd::Run {
                headless: false,
                max_desks: None,
                look: LookArgs {
                    look: None,
                    protocol: None
                },
                ..
            }
        ));
    }

    #[test]
    fn cmd_or_default_preserves_explicit_subcommand() {
        let cli = Cli {
            cmd: Some(Cmd::Doctor {
                look: crate::graphics::LookMode::Classic,
                protocol: None,
            }),
            log_level: LogLevel::Debug,
            theme: Some("cyberpunk".into()),
        };
        let (level, theme, cmd) = cli.cmd_or_default();
        assert_eq!(level, LogLevel::Debug);
        assert_eq!(theme.as_deref(), Some("cyberpunk"));
        assert!(matches!(cmd, Cmd::Doctor { .. }));
    }

    #[test]
    fn floating_subcommand_parses_with_shared_run_flags() {
        let cli = Cli::try_parse_from(["pixtuoid", "floating", "--socket", "/tmp/p.sock"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Floating {
                source: SourceArgs { socket: Some(_) },
            })
        ));
    }

    #[test]
    fn sources_set_parses_its_ids() {
        let cli = Cli::try_parse_from(["pixtuoid", "sources", "set", "codex", "cursor"]).unwrap();
        match cli.cmd {
            Some(Cmd::Sources {
                action: Some(SourcesAction::Set { ids }),
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
                        json: true
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
        assert!(matches!(d.cmd, Some(Cmd::Disconnect { json: true, .. })));
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
