use super::{SECOND_TURN, hook_event_name};
use crate::harness::hermetic::{
    ConfigFile, HostConfig, Package, PerOs, Profile, Scenario, Second, Shape, Surface, Turn,
    TurnExit, tui,
};
use crate::harness::plan::Placeholder;

/// codex's TUI flags, its cites at `rust-v0.163.0`. On-request lets the model
/// ask (`codex-rs/protocol/src/protocol.rs:1099-1102`), and the script's call
/// asks to run unsandboxed (`core/src/tools/handlers/shell_spec.rs:239-243`).
const TUI_ARGS: [&str; 3] = [
    "--dangerously-bypass-hook-trust",
    "--ask-for-approval",
    "on-request",
];

const TUI: tui::Tui = tui::Tui {
    args: &TUI_ARGS,
    composer: tui::Composer {
        marker: "›",
        border: tui::Border::Nothing,
    },
    gates: &[GATE],
    // `chat.interrupt_turn` (`tui/src/keymap.rs:1695` at `rust-v0.164.0`),
    // while a turn runs (`tui/src/bottom_pane/mod.rs:1822-1835`).
    cancel: &[tui::Key::Esc],
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    conpty: tui::ConPty::Renders,
};

/// The exec approval dialog, answered "No, and tell Codex what to do
/// differently", which aborts the turn
/// (`tui/src/bottom_pane/approval_overlay.rs:911-915`).
const GATE: tui::Gate = tui::Gate {
    shows: "Would you like to run the following command?",
    keys: &[tui::Key::Esc],
    // It loses a key sent the moment it shows (measured; the cause in
    // codex is unverified).
    dwell: std::time::Duration::from_millis(500),
};

/// [`TUI`] answering its dialog "Yes, proceed", its choices' keys at
/// `rust-v0.164.0` (`tui/src/bottom_pane/approval_overlay.rs:852-934`,
/// `tui/src/keymap.rs:1979-1983`).
const TUI_APPROVE: tui::Tui = tui::Tui {
    gates: &[tui::Gate {
        keys: &[tui::Key::Text("y")],
        ..GATE
    }],
    ..TUI
};

/// [`TUI`] where the seed's own PermissionRequest hook denies every prompt
/// before a dialog shows: hooks decide ahead of the user
/// (`core/src/tools/approvals.rs:483-505` at `rust-v0.164.0`).
const TUI_POLICY: tui::Tui = tui::Tui { gates: &[], ..TUI };

/// Code mode: the model's `exec` cell runs JavaScript whose tools are nested
/// calls (`code-mode-protocol/src/description.rs:23-48` at `rust-v0.164.0`),
/// off by default for the profile's model (`features/src/lib.rs:1198-1201`,
/// `core/src/tools/mod.rs:65-75`; `models-manager/models.json` gives
/// `gpt-5.5` no `tool_mode`).
const CODE_MODE: [&str; 2] = ["-c", "features.code_mode=true"];

/// [`TUI`] in code mode.
const TUI_CODE_MODE: tui::Tui = tui::Tui {
    // Destructured whole, so a flag added to either list fails to compile here.
    args: &{
        let [bypass, ask, policy] = TUI_ARGS;
        let [set, code_mode] = CODE_MODE;
        [bypass, ask, policy, set, code_mode]
    },
    ..TUI
};

/// [`TUI_CODE_MODE`] answering its dialog as [`TUI_APPROVE`] does.
const TUI_CODE_MODE_APPROVE: tui::Tui = tui::Tui {
    gates: TUI_APPROVE.gates,
    ..TUI_CODE_MODE
};

pub(crate) const PROFILE: Profile = Profile {
    source: crate::source::codex::SOURCE_NAME,
    package: Package::Npm("@openai/codex"),
    plugin_types: &[],
    // A hook runs only once trusted, and trust is a hash of codex's own
    // normalized hook identity (`codex-rs/hooks/src/engine/discovery.rs:775`)
    // that seeding would have to re-implement; this flag is upstream's path
    // for "automation that already vets hook sources"
    // (`codex-rs/utils/cli/src/shared_options.rs:62-64`).
    invocation: &[
        "codex",
        "exec",
        "--dangerously-bypass-hook-trust",
        Placeholder::Prompt.as_str(),
    ],
    second_turn: None,
    env: &[("HERMETIC_API_KEY", "{key}")],
    homes: &[],
    // System config, requirements and managed config; `CODEX_HOME` never
    // reaches them (`codex-rs/config/src/loader/mod.rs:69`, `:102-107`,
    // `layer_io.rs:222-227`). On Windows they live in the known
    // ProgramData (`:72`, `:823-885`), and skills come from the known
    // profile, which no variable moves (`ext/skills/src/host_roots.rs:18-36`).
    system_config: PerOs {
        unix: &[HostConfig::Path("/etc/codex")],
        windows: &[
            HostConfig::Path(r"{FOLDERID_ProgramData}\OpenAI\Codex"),
            HostConfig::Path(r"{FOLDERID_Profile}\.agents\skills"),
        ],
    },
    // Its project config and AGENTS.md stop at the project root
    // (`codex-rs/config/src/loader/mod.rs:1662`, `core/src/agents_md.rs:8-12`),
    // which the recorder's `git init` makes the workspace.
    ancestor_config: &[],
    // `[windows]` is what codex's own setup writes for its non-admin
    // sandbox (`codex-rs/tui/src/config_update.rs:111-126`), without which
    // read-only exec is Forbidden (`core/src/exec_policy.rs:799-806`);
    // only Windows reads it (`sandboxing/src/manager.rs:67-81`).
    config: &[ConfigFile {
        home: "CODEX_HOME",
        path: "config.toml",
        template: r#"# Cites at codex `rust-v0.163.0`.
model = "gpt-5.5"
model_provider = "aimock"
model_reasoning_effort = "high"
# A trust decision would make the sandbox workspace-write
# (`codex-rs/config/src/config_toml.rs:855-861`).
sandbox_mode = "read-only"

[model_providers.aimock]
name = "aimock"
base_url = "http://127.0.0.1:{port}/v1"
env_key = "HERMETIC_API_KEY"
wire_api = "responses"

[windows]
sandbox = "unelevated"

# The TUI asks to trust a folder no entry names, matched on the cwd's own
# text (`config/src/loader/mod.rs:1416-1437`).
[projects.'{workspace}']
trust_level = "trusted"

# The TUI offers the catalog's upgrade from this model unless it was seen
# (`tui/src/app/startup_prompts.rs:165-168`, `models-manager/models.json:1401`).
[notice.model_migrations]
"gpt-5.5" = "gpt-6-sol"
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
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "custom-tool",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            // The read-only sandbox rejects the patch, so no PostToolUse
            // fires and the transcript's `custom_tool_call_output` ends it.
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "Stop",
                "SessionEnd",
            ],
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
                "PostToolUse",
                "SubagentStart",
                "SubagentStop",
                "Stop",
                "SessionEnd",
            ],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "permission",
            shape: Shape::Permission,
            surface: Surface::Tui { row: &TUI },
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PermissionRequest",
                "SessionEnd",
            ],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "cancel",
            shape: Shape::Cancel,
            surface: Surface::Tui { row: &TUI },
            expects: &["SessionStart", "UserPromptSubmit", "SessionEnd"],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "model-error",
            shape: Shape::ModelError,
            surface: Surface::Headless,
            expects: &["SessionStart", "UserPromptSubmit", "SessionEnd"],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "two-turn",
            shape: Shape::ToolRun,
            surface: Surface::Tui { row: &TUI },
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
                "SessionEnd",
            ],
            turn: Turn {
                second: Some(Second::Prompt(SECOND_TURN)),
                ..Turn::PROFILE
            },
        },
        // A turn refused, then the next prompt's own call gated before the
        // refused turn's rollout lines are read: its lagging end must not
        // answer the new request.
        Scenario {
            name: "refuse-then-prompt",
            shape: Shape::Permission,
            surface: Surface::Tui { row: &TUI },
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PermissionRequest",
                "SessionEnd",
            ],
            turn: Turn {
                second: Some(Second::Prompt(SECOND_TURN)),
                ..Turn::PROFILE
            },
        },
        Scenario {
            name: "approve",
            shape: Shape::Permission,
            surface: Surface::Tui { row: &TUI_APPROVE },
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PermissionRequest",
                "PostToolUse",
                "Stop",
                "SessionEnd",
            ],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "code-mode-failure",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "Stop",
                "SessionEnd",
            ],
            turn: Turn {
                args: &CODE_MODE,
                ..Turn::PROFILE
            },
        },
        // The one way an exec is denied and its turn goes on: a policy hook's
        // deny (`protocol/src/approvals.rs:377-410` offers the user no such
        // choice), so its request clears through the rollout's output alone.
        Scenario {
            name: "policy-deny",
            shape: Shape::Permission,
            surface: Surface::Tui { row: &TUI_POLICY },
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PermissionRequest",
                "Stop",
                "SessionEnd",
            ],
            turn: Turn {
                seed: Some("policy-deny"),
                ..Turn::PROFILE
            },
        },
        // The dialog offers no "No, continue without running it" for an exec
        // (`protocol/src/approvals.rs:377-410`,
        // `core/src/tools/approvals.rs:710-726` at `rust-v0.164.0`), so a
        // gated nested call is refused the one way a user can: aborting.
        Scenario {
            name: "code-mode-gate",
            shape: Shape::Permission,
            surface: Surface::Tui {
                row: &TUI_CODE_MODE,
            },
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PermissionRequest",
                "SessionEnd",
            ],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "code-mode-approve",
            shape: Shape::Permission,
            surface: Surface::Tui {
                row: &TUI_CODE_MODE_APPROVE,
            },
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PermissionRequest",
                "PostToolUse",
                "Stop",
                "SessionEnd",
            ],
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "code-mode-yield",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"],
            turn: Turn {
                args: &CODE_MODE,
                ..Turn::PROFILE
            },
        },
        // The seed's own Stop hook blocks the turn's first stop, which goes on
        // (`core/src/session/turn.rs:722-759`, `hooks/src/events/stop.rs` at
        // `rust-v0.164.0`); codex reads a trusted project's `.codex/hooks.json`
        // (`config/src/loader/mod.rs:1838-1965`, `hooks/src/engine/discovery.rs:378-418`).
        Scenario {
            name: "stop-hook",
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
            turn: Turn {
                seed: Some("stop-hook"),
                ..Turn::PROFILE
            },
        },
    ],
    script_vars: &[],
    not_yet: &[],
    event_name: hook_event_name,
    not_yet_fields: &[],
    // A `write_stdin` approval, which no shape scripts.
    not_yet_literals: &["approval_id"],
    children_inherit_cwd: false,
    result_unseen: false,
    live: None,
    daemon: None,
    server_stop: None,
    // A finished task writes `task_complete`, its error inside when it
    // failed, and an aborted one `turn_aborted`
    // (`core/src/tasks/mod.rs:645-669`, `:831-870` at rust-v0.164.0).
    turn_exits: &[
        TurnExit {
            exit: "task_complete",
            with: None,
            scenario: "tool-run",
        },
        TurnExit {
            exit: "task_complete",
            with: Some("error"),
            scenario: "model-error",
        },
        TurnExit {
            exit: "turn_aborted",
            with: None,
            scenario: "cancel",
        },
    ],
};
