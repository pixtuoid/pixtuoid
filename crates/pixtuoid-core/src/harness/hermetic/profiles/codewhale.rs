//! codewhale's cites are `codewhale-hq/Codewhale` at `v0.10.4`.

use super::event_field;
use crate::harness::hermetic::{
    ConfigFile, HostConfig, Package, PerOs, Profile, Scenario, Shape, Surface, Turn, tui,
};

/// Its TUI, the one surface that fires a session's lifecycle: `exec --hooks`
/// fires `tool_call_before` and `shell_env` alone (`docs/HOOKS.md:40-51`).
const TUI: tui::Tui = tui::Tui {
    args: &[],
    composer: tui::Composer {
        marker: "│❯",
        border: tui::Border::Rule {
            glyph: '─',
            corners: Some(('╭', '╮')),
        },
    },
    gates: &[tui::Gate {
        // The approval band's deny pill (`crates/tui/src/tui/widgets/mod.rs:2068-2074`).
        shows: "✕ Deny",
        // `n` denies (`crates/tui/src/tui/approval/view.rs:544-550`); the band's `[d]`
        // beside it toggles details (`:491-494`).
        keys: &[tui::Key::Text("n")],
        dwell: std::time::Duration::ZERO,
    }],
    cancel: &[],
    // A graceful shutdown, which fires `session_end` (`docs/HOOKS.md:243`,
    // `crates/tui/src/commands/groups/core/exit.rs:9-14`).
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    conpty: tui::ConPty::Renders,
};

pub(crate) const PROFILE: Profile = Profile {
    source: crate::source::codewhale::SOURCE_NAME,
    // What its installer fetches and checks against the release's sums
    // (`web/public/install.sh:239-249`, `:264-266`, `:428`).
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
    plugin_types: &[],
    invocation: &["codewhale"],
    second_turn: None,
    // `docs/CONFIGURATION.md:1037`, `:1598`.
    env: &[
        ("CODEWHALE_NO_UPDATE_CHECK", "1"),
        ("CODEWHALE_TELEMETRY", "0"),
    ],
    homes: &[],
    // Windows reads both under the home (`crates/runtime/src/config/paths.rs:156-188`).
    system_config: PerOs {
        unix: &[
            HostConfig::Path("/etc/deepseek/managed_config.toml"),
            HostConfig::Path("/etc/deepseek/requirements.toml"),
        ],
        windows: &[],
    },
    // Instructions stop at the git root (`crates/engine/src/project_context.rs:1504-1570`),
    // which the recorder's `git init` makes the workspace.
    ancestor_config: &[],
    // A loopback endpoint needs no key (`docs/CONFIGURATION.md:551-552`, `:678-684`).
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
            surface: Surface::Tui { row: &TUI },
            expects: &[
                "session_start",
                "message_submit",
                "tool_call_before",
                "tool_call_after",
                "session_idle",
                "session_end",
            ],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "permission",
            shape: Shape::Permission,
            surface: Surface::Tui { row: &TUI },
            expects: &[
                "session_start",
                "message_submit",
                "tool_call_before",
                "waiting_for_user",
                "tool_call_after",
                "session_idle",
                "session_end",
            ],
            turn: Turn::PROFILE,
        },
        // Not a `Subagent`: a child's own tools fire no hook
        // (`docs/HOOKS.md:33`), so no run shows one on the child.
        Scenario {
            name: "child-error",
            shape: Shape::ChildError,
            surface: Surface::Tui { row: &TUI },
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
    server_stop: None,
    turn_exits: &[],
};
