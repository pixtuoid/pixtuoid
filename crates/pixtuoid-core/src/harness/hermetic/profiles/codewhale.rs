//! codewhale's cites are `codewhale-hq/Codewhale` at `v0.10.2`.

use super::event_field;
use crate::harness::hermetic::{
    ConfigFile, HostConfig, Package, PerOs, Profile, Scenario, Shape, Surface, Turn, tui,
};

/// Its TUI, the one surface that fires a session's lifecycle: `exec --hooks`
/// fires `tool_call_before` and `shell_env` alone (`docs/HOOKS.md:33-44`).
const TUI: tui::Tui = tui::Tui {
    args: &[],
    composer: tui::Composer {
        marker: "│❯",
        above: tui::Above::Rule {
            glyph: '─',
            corners: Some(('╭', '╮')),
        },
    },
    gates: &[tui::Gate {
        // `crates/tui/src/tui/widgets/approval_band_legacy.rs:753-756`.
        shows: "Do you want to proceed?",
        // "Don't allow" (`crates/tui/src/tui/approval/view.rs:399-408`).
        deny: &[tui::Key::Text("n")],
        // Its event loop fires `waiting_for_user` on its next pass
        // (`crates/tui/src/tui/ui/event_loop.rs:2087`): answered on sight, the
        // wait resolves unobserved (measured).
        dwell: std::time::Duration::from_secs(1),
    }],
    // A graceful shutdown, which fires `session_end` (`docs/HOOKS.md:236`,
    // `crates/tui/src/commands/groups/core/exit.rs:9-14`).
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    renders_under_conpty: true,
};

pub(crate) const PROFILE: Profile = Profile {
    source: "codewhale",
    // What its installer fetches and checks against the release's sums
    // (`web/public/install.sh:197-207`, `:222-224`, `:237`).
    package: Package::GithubRelease {
        repo: "codewhale-hq/Codewhale",
        assets: &[
            ("macos", "aarch64", "codewhale-macos-arm64"),
            ("macos", "x86_64", "codewhale-macos-x64"),
            ("linux", "aarch64", "codewhale-linux-arm64"),
            ("linux", "x86_64", "codewhale-linux-x64"),
            ("windows", "aarch64", "codewhale-windows-arm64.exe"),
            ("windows", "x86_64", "codewhale-windows-x64.exe"),
        ],
        sums: "codewhale-artifacts-sha256.txt",
    },
    invocation: &["codewhale"],
    second_turn: None,
    // `docs/CONFIGURATION.md:923`, `:1465`.
    env: &[
        ("CODEWHALE_NO_UPDATE_CHECK", "1"),
        ("CODEWHALE_TELEMETRY", "0"),
    ],
    homes: &[],
    // Windows reads both under the home (`crates/tui/src/config/paths.rs:161-193`).
    system_config: PerOs {
        unix: &[
            HostConfig::Path("/etc/deepseek/managed_config.toml"),
            HostConfig::Path("/etc/deepseek/requirements.toml"),
        ],
        windows: &[],
    },
    // Instructions stop at the git root (`crates/tui/src/project_context.rs:888-955`),
    // which the recorder's `git init` makes the workspace.
    ancestor_config: &[],
    // A loopback endpoint needs no key (`docs/CONFIGURATION.md:457-458`, `:584-590`).
    config: &[ConfigFile {
        home: "HOME",
        path: ".codewhale/config.toml",
        template: r#"provider = "aimock"

[providers.aimock]
kind = "openai-compatible"
base_url = "http://127.0.0.1:{port}/v1"
model = "hermetic"
"#,
    }],
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Tui {
                row: &TUI,
                answer: tui::Answer::Nothing,
            },
            expects: &[
                "session_start",
                "message_submit",
                "tool_call_before",
                "tool_call_after",
                "session_idle",
                "session_end",
            ],
            fixture: "fixtures/codewhale/tool-run-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "permission",
            shape: Shape::Permission,
            surface: Surface::Tui {
                row: &TUI,
                answer: tui::Answer::Deny,
            },
            expects: &[
                "session_start",
                "message_submit",
                "tool_call_before",
                "waiting_for_user",
                "tool_call_after",
                "session_idle",
                "session_end",
            ],
            fixture: "fixtures/codewhale/permission-hermetic",
            turn: Turn::PROFILE,
        },
        // Not a `Subagent`: a child's own tools fire no hook
        // (`docs/HOOKS.md:26`), so no run shows one on the child.
        Scenario {
            name: "child-error",
            shape: Shape::ChildError,
            surface: Surface::Tui {
                row: &TUI,
                answer: tui::Answer::Nothing,
            },
            expects: &[
                "session_start",
                "message_submit",
                "tool_call_before",
                "tool_call_after",
                "subagent_spawn",
                "subagent_complete",
                "session_idle",
                "session_end",
            ],
            fixture: "codewhale/fixtures/child-error-hermetic",
            turn: Turn::PROFILE,
        },
    ],
    script_vars: &[],
    not_yet: &[],
    event_name: event_field,
    not_yet_fields: &[],
    not_yet_literals: &[],
    children_inherit_cwd: false,
    result_unseen: false,
    live: None,
    daemon: None,
};
