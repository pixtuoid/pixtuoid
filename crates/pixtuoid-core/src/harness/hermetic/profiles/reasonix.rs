//! reasonix's cites are `esengine/DeepSeek-Reasonix` at `v2.44.0`.

use super::event_field;
use crate::harness::hermetic::{
    ConfigFile, Package, PerOs, Profile, Scenario, Shape, Surface, Turn, tui,
};
use crate::harness::plan::Placeholder;

/// reasonix's TUI. A named mode skips the folder-trust question, whose yes
/// would run commands unasked
/// (`internal/frontend/cli/tui_posture.go:32-42`, `:69`).
const TUI: tui::Tui = tui::Tui {
    args: &["--permission-mode", "ask"],
    composer: tui::Composer {
        marker: " ❯",
        border: tui::Border::Rule {
            glyph: '─',
            corners: None,
        },
    },
    gates: &[tui::Gate {
        shows: "Permission required",
        // "n/Esc deny" (`internal/base/i18n/messages_en.go:164`).
        keys: &[tui::Key::Esc],
        dwell: std::time::Duration::ZERO,
    }],
    cancel: &[],
    // `/exit` is its alias (`internal/frontend/cli/slash_registry.go:69`).
    quit: &[tui::Key::Text("/quit"), tui::Key::Enter],
    conpty: tui::ConPty::Renders,
};

pub(crate) const PROFILE: Profile = Profile {
    source: crate::source::reasonix::SOURCE_NAME,
    package: Package::Npm("reasonix"),
    plugin_types: &[],
    // A non-read-only tool is denied headless; this allows the ones the
    // spawns need (`docs/CLI.md:502-503`, `:517-519`).
    invocation: &[
        "reasonix",
        "--allowed-tools",
        "task,explore",
        "-p",
        Placeholder::Prompt.as_str(),
    ],
    second_turn: None,
    // Off already for a non-interactive run, and pinned off against a
    // changed default (`internal/platform/telemetry/policy.go:44`).
    env: &[("REASONIX_TELEMETRY", "off")],
    homes: &["REASONIX_HOME"],
    system_config: PerOs {
        unix: &[],
        windows: &[],
    },
    // Instructions stop at the nearest git root
    // (`internal/state/instruction/resolver.go:80-84`), which the
    // recorder's `git init` makes the workspace; `./reasonix.toml` is the
    // workspace's own (`internal/contract/config/paths.go:639-649`).
    ancestor_config: &[],
    // A loopback `base_url` runs with no key
    // (`internal/contract/config/config.go:1651-1684`).
    config: &[ConfigFile {
        home: "REASONIX_HOME",
        path: "config.toml",
        // A role with the executor's own model builds no planner
        // (`internal/assembly/boot/roles.go:56`), so each has a model of
        // its own for a script to match on.
        template: r#"default_model = "aimock"

[agent]
planner_model = "planner"
guardian_model = "guardian"

[[providers]]
name = "aimock"
kind = "openai"
base_url = "http://127.0.0.1:{port}/v1"
model = "deepseek-v4-flash"

[[providers]]
name = "planner"
kind = "openai"
base_url = "http://127.0.0.1:{port}/v1"
model = "planner-model"

[[providers]]
name = "guardian"
kind = "openai"
base_url = "http://127.0.0.1:{port}/v1"
model = "guardian-model"
"#,
    }],
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
                "SessionEnd",
            ],
            fixture: "fixtures/reasonix/tool-run-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "subagent",
            shape: Shape::Subagent,
            surface: Surface::Headless,
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "SubagentStart",
                "PostToolUse",
                "SubagentStop",
                "Stop",
                "SessionEnd",
            ],
            fixture: "reasonix/fixtures/subagent-hermetic",
            turn: Turn::PROFILE,
        },
        // A skill tool's child fires no SubagentStart or SubagentStop.
        Scenario {
            name: "explore",
            shape: Shape::Subagent,
            surface: Surface::Headless,
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
                "SessionEnd",
            ],
            fixture: "reasonix/fixtures/explore-hermetic",
            turn: Turn::PROFILE,
        },
        // The marker routes the turn through the planner
        // (`internal/session/control/planner_gate.go:91-92`).
        Scenario {
            name: "planner",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
                "SessionEnd",
            ],
            fixture: "fixtures/reasonix/planner-hermetic",
            turn: Turn {
                prompt: Some("[Reasonix: plan-first] Read NOTE.txt."),
                ..Turn::PROFILE
            },
        },
        Scenario {
            name: "bash-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
                "SessionEnd",
            ],
            fixture: "fixtures/reasonix/bash-run-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "permission",
            shape: Shape::Permission,
            surface: Surface::Tui { row: &TUI },
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                // The guardian's read: the refused bash fires no PreToolUse.
                "PreToolUse",
                "PostToolUse",
                "PermissionRequest",
                "Notification",
                "Stop",
                "SessionEnd",
            ],
            fixture: "fixtures/reasonix/permission-hermetic",
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
