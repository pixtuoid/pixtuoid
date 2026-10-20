use super::hook_event_name;
use crate::harness::hermetic::{
    ConfigFile, HostConfig, PROMPT_PLACEHOLDER, Package, PerOs, Profile, Scenario, Shape, Surface,
    Turn, tui,
};

/// codex's TUI, its cites at `rust-v0.163.0`. On-request lets the model ask
/// (`codex-rs/protocol/src/protocol.rs:1099-1102`), and the script's call asks
/// to run unsandboxed (`core/src/tools/handlers/shell_spec.rs:239-243`).
const TUI: tui::Tui = tui::Tui {
    args: &[
        "--dangerously-bypass-hook-trust",
        "--ask-for-approval",
        "on-request",
    ],
    composer: tui::Composer {
        marker: "›",
        above: tui::Above::Nothing,
    },
    gates: &[tui::Gate {
        shows: "Would you like to run the following command?",
        // "No, and tell Codex what to do differently", which aborts the turn
        // (`tui/src/bottom_pane/approval_overlay.rs:911-915`).
        deny: &[tui::Key::Esc],
        // It loses a key sent the moment it shows (measured; the cause in
        // codex is unverified).
        dwell: std::time::Duration::from_millis(500),
    }],
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    renders_under_conpty: true,
};

pub(crate) const PROFILE: Profile = Profile {
    source: "codex",
    package: Package::Npm("@openai/codex"),
    // A hook runs only once trusted, and trust is a hash of codex's own
    // normalized hook identity (`codex-rs/hooks/src/engine/discovery.rs:775`)
    // that seeding would have to re-implement; this flag is upstream's path
    // for "automation that already vets hook sources"
    // (`codex-rs/utils/cli/src/shared_options.rs:62-64`).
    invocation: &[
        "codex",
        "exec",
        "--dangerously-bypass-hook-trust",
        PROMPT_PLACEHOLDER,
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
            fixture: "fixtures/codex/tool-run-hermetic",
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
            fixture: "fixtures/codex/custom-tool-hermetic",
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
            fixture: "codex/fixtures/subagent-hermetic",
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
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PermissionRequest",
                "SessionEnd",
            ],
            fixture: "fixtures/codex/permission-hermetic",
            turn: Turn::PROFILE,
        },
    ],
    script_vars: &[],
    not_yet: &[],
    event_name: hook_event_name,
    // aimock's Responses usage has no token details (`CopilotKit/aimock`
    // `src/responses.ts:543-556` at v1.45.1).
    not_yet_fields: &[
        (
            "tx:event_msg",
            "/payload/info/last_token_usage/cached_input_tokens",
        ),
        (
            "tx:event_msg",
            "/payload/info/last_token_usage/reasoning_output_tokens",
        ),
    ],
    not_yet_literals: &[],
    children_inherit_cwd: false,
    result_unseen: false,
    live: None,
    daemon: None,
};
