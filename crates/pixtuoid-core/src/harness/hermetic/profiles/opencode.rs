//! opencode's cites are at v2.0.28, the release that added the system
//! config below.

use super::type_field;
use crate::harness::hermetic::{
    ConfigFile, Daemon, HostConfig, Package, PerOs, Profile, Scenario, Second, Shape, Surface,
    Turn, TurnExit, tui,
};
use crate::harness::plan::Placeholder;

/// opencode's daemon, as its `--server` commands name it.
const SERVER: &str = "http://127.0.0.1:{daemon_port}";

/// opencode's TUI on the daemon, which it takes as `--server` as every
/// command does (`packages/cli/src/commands/commands.ts:15-24`, `:38-42` at
/// v2.1.4).
const TUI: tui::Tui = tui::Tui {
    args: &["--server", SERVER],
    composer: tui::Composer {
        marker: "┃",
        // The input line, a padding row, then the agent and model above the edge.
        border: tui::Border::Footed {
            edge: '╹', rows: 3
        },
    },
    gates: &[],
    // `session.interrupt` is Esc (`packages/tui/src/config/keybind.ts:128` at
    // v2.1.4), which interrupts only on its second press
    // (`packages/tui/src/component/prompt/index.tsx:516-527`).
    cancel: &[tui::Key::Esc, tui::Key::Esc],
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    conpty: tui::ConPty::Unverified,
};

pub(crate) const PROFILE: Profile = Profile {
    source: crate::source::opencode::SOURCE_NAME,
    package: Package::Npm("@opencode/cli"),
    plugin_types: &["@opencode/plugin"],
    // On the daemon, whose plugin forwards its session's delete
    // (`packages/core/src/session.ts:378-388` at v2.1.0): a delete through
    // a private server of its own forwards none. Only an explicit `--model`
    // reaches the session's events (`packages/cli/src/run/run.ts:91`).
    invocation: &[
        "opencode",
        "run",
        "--server",
        SERVER,
        "--model",
        "openai/gpt-5.2",
        Placeholder::Prompt.as_str(),
    ],
    // `session delete` (`packages/cli/src/commands/commands.ts:437-443` at
    // v2.1.0).
    second_turn: Some(&[
        "opencode",
        "session",
        "delete",
        "--server",
        SERVER,
        Placeholder::Session.as_str(),
    ]),
    env: &[
        // The models.dev fetch and the update check
        // (`packages/cli/src/server-process.ts:113`, `services/updater.ts:534`).
        ("OPENCODE_DISABLE_MODELS_FETCH", "1"),
        ("OPENCODE_DISABLE_AUTOUPDATE", "1"),
        // The daemon's password, which each `--server` command sends and the
        // daemon keeps from its tools' env
        // (`packages/cli/src/services/server-connection.ts:24-29`,
        // `packages/cli/src/server-process.ts:86-89` at v2.1.0).
        ("OPENCODE_PASSWORD", "{key}"),
    ],
    // On Windows its system config is under `env.ProgramData`
    // (`packages/core/src/config/discovery.ts:27-31`), so a sandboxed
    // ProgramData holds none.
    homes: &["OPENCODE_CONFIG_DIR", "ProgramData"],
    // System config and managed preferences
    // (`packages/core/src/config/discovery.ts:27-38`).
    system_config: PerOs {
        unix: &[
            HostConfig::Path("/Library/Application Support/opencode"),
            HostConfig::Path("/etc/opencode"),
            HostConfig::Path("/Library/Managed Preferences/ai.opencode.managed.plist"),
            HostConfig::Path("/Library/Managed Preferences/{user}/ai.opencode.managed.plist"),
        ],
        windows: &[],
    },
    // Project config up to `/` (`packages/core/src/config/discovery.ts:12`,
    // `:80`); its AGENTS.md walk stops at the git worktree
    // (`config/plugin/instruction.ts:37`), the workspace the recorder inits.
    ancestor_config: &[
        ".claude",
        ".agents",
        ".opencode",
        "opencode.jsonc",
        "opencode.json",
    ],
    // A shell command asks, which `opencode run` rejects
    // (`services/www/src/docs/content/permissions.mdx:9-20`,
    // `packages/cli/src/run/noninteractive.ts:158-181`): the permission turn.
    // `model` is the one a TUI turn runs on, since it names none
    // (`packages/schema/src/config.ts:33-35` at v2.1.4).
    config: &[ConfigFile {
        home: "OPENCODE_CONFIG_DIR",
        path: "opencode.jsonc",
        template: r#"{
  "model": "openai/gpt-5.2",
  "providers": {
    "openai": {
      "settings": { "baseURL": "http://127.0.0.1:{port}/v1", "apiKey": "{key}" },
      "models": { "gpt-5.2": {} }
    }
  },
  "permissions": [{ "action": "shell", "resource": "*", "effect": "ask" }]
}
"#,
    }],
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[
                "session.execution.started",
                "session.tool.input.started",
                "session.tool.success",
                "session.execution.succeeded",
            ],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "subagent",
            shape: Shape::Subagent,
            surface: Surface::Headless,
            expects: &[
                "session.execution.started",
                "session.tool.input.started",
                "session.tool.success",
                "session.execution.succeeded",
            ],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "model-error",
            shape: Shape::ModelError,
            surface: Surface::Headless,
            expects: &["session.execution.started", "session.execution.failed"],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "permission",
            shape: Shape::Permission,
            surface: Surface::Headless,
            expects: &[
                "session.execution.started",
                "session.tool.input.started",
                "permission.asked",
                "session.tool.failed",
                "session.execution.succeeded",
            ],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "delete",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[
                "session.execution.started",
                "session.tool.input.started",
                "session.tool.success",
                "session.execution.succeeded",
                "session.deleted",
            ],
            turn: Turn {
                second: Some(Second::Bare),
                ..Turn::PROFILE
            },
        },
        Scenario {
            name: "cancel",
            shape: Shape::Cancel,
            surface: Surface::Tui { row: &TUI },
            expects: &["session.execution.started", "session.execution.interrupted"],
            turn: Turn::PROFILE,
        },
    ],
    script_vars: &[],
    not_yet: &[],
    event_name: type_field,
    not_yet_fields: &[],
    not_yet_literals: &[],
    children_inherit_cwd: false,
    result_unseen: false,
    live: None,
    // A server on the run's port whose stdin is its lease
    // (`packages/cli/src/server-process.ts:208-212`, `:231-244` at v2.1.0);
    // listing the sessions loads the workspace, and with it the plugin,
    // before the turn.
    daemon: Some(Daemon {
        start: &[
            "serve",
            "--stdio",
            "--port",
            Placeholder::DaemonPort.as_str(),
        ],
        ready: &["session", "list", "--server", SERVER],
        env: &[],
    }),
    server_stop: None,
    // A session's execution settles on every exit as one of three
    // (`packages/core/src/session/execution.ts:116-145` at v2.2.0).
    turn_exits: &[
        TurnExit {
            exit: "session.execution.succeeded",
            with: None,
            scenario: "tool-run",
        },
        TurnExit {
            exit: "session.execution.failed",
            with: None,
            scenario: "model-error",
        },
        TurnExit {
            exit: "session.execution.interrupted",
            with: None,
            scenario: "cancel",
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencodes_model_flag_names_a_model_its_config_declares_and_defaults_to() {
        let model = PROFILE
            .invocation
            .windows(2)
            .find_map(|w| (w[0] == "--model").then_some(w[1]))
            .expect("a --model");
        let (provider, id) = model.split_once('/').expect("provider/model");
        let config: serde_json::Value =
            serde_json::from_str(PROFILE.config[0].template).expect("plain JSON");
        assert!(
            config["providers"][provider]["models"].get(id).is_some(),
            "{model} is not under the config's providers.{provider}.models"
        );
        assert_eq!(config["model"], model, "a TUI turn runs on another model");
    }
}
