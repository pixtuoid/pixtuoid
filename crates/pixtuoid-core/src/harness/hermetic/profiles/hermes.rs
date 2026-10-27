//! hermes's cites are `NousResearch/hermes-agent` at `v0.22.2`.

use super::hook_event_name;
use crate::harness::hermetic::{
    ConfigFile, HostConfig, Package, PerOs, Profile, Scenario, Shape, Surface, Turn, tui,
};
use crate::harness::plan::Placeholder;

/// hermes's classic REPL, where a dangerous terminal command waits on the
/// user.
const TUI: tui::Tui = tui::Tui {
    args: &["--accept-hooks", "--cli", "chat"],
    composer: tui::Composer {
        marker: "❯",
        border: tui::Border::Rules { glyph: '─' },
    },
    gates: &[tui::Gate {
        shows: "Dangerous Command",
        // A digit picks its choice, "4. Deny" here
        // (`hermes_cli/cli_tui_mixin.py:1301-1307`, `:2066-2068`).
        deny: &[tui::Key::Text("4")],
        dwell: std::time::Duration::ZERO,
    }],
    cancel: &[],
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    conpty: tui::ConPty::Renders,
};

pub(crate) const PROFILE: Profile = Profile {
    source: "hermes",
    // Its build refuses a wheel (`setup.py:37`), so no index carries a
    // current release; a source checkout tracks the latest `vX.Y.Z` tag
    // (`website/docs/getting-started/updating.md:33-36`). Its installer
    // would rewrite the operator's shell rc files and run `sudo -n -v`
    // (`scripts/install.sh:851-853`, `:882-908`).
    package: Package::UvLocked("NousResearch/hermes-agent"),
    // `-z` is one turn that approves every tool (`hermes_cli/oneshot.py:275-277`),
    // and a hook runs only once accepted.
    invocation: &[
        "hermes",
        "--accept-hooks",
        "-z",
        Placeholder::Prompt.as_str(),
    ],
    second_turn: None,
    env: &[],
    // The managed scope, which otherwise is `/etc/hermes`
    // (`hermes_cli/managed_scope.py:31`, `:45-58`).
    homes: &["HERMES_MANAGED_DIR"],
    // Read there whatever the managed scope is (`hermes_cli/image_provenance.py:16`).
    system_config: PerOs {
        unix: &[HostConfig::Path("/etc/hermes")],
        windows: &[],
    },
    // Context files stop at the git root (`agent/prompt_builder.py:141`,
    // `:1722-1732`), which the recorder's `git init` makes the workspace.
    ancestor_config: &[],
    // `manual` approvals ask the user where the default asks an LLM guardian
    // (`tests/e2e/core/security/test_approval_boundary.py:17`), and an answered
    // shared-metrics offer, one at upstream's `OFFER_VERSION`, never opens before
    // a chat (`hermes_cli/observability/shared_metrics_consent.py:55`, `:58-72`).
    config: &[ConfigFile {
        home: "HERMES_HOME",
        path: "config.yaml",
        template: r#"model:
  default: hermetic
  provider: custom
  base_url: http://127.0.0.1:{port}/v1
  api_key: "{key}"
approvals:
  mode: manual
telemetry:
  shared_metrics:
    enabled: false
    offer_version: 2
"#,
    }],
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[
                "on_session_start",
                "pre_tool_call",
                "post_tool_call",
                "on_session_end",
                "on_session_finalize",
            ],
            fixture: "fixtures/hermes/tool-run-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "permission",
            shape: Shape::Permission,
            surface: Surface::Tui { row: &TUI },
            expects: &[
                "on_session_start",
                "pre_tool_call",
                "pre_approval_request",
                "post_tool_call",
                "on_session_end",
                "on_session_finalize",
            ],
            fixture: "fixtures/hermes/permission-hermetic",
            turn: Turn::PROFILE,
        },
    ],
    script_vars: &[],
    not_yet: &[],
    event_name: hook_event_name,
    not_yet_fields: &[],
    not_yet_literals: &[],
    children_inherit_cwd: false,
    result_unseen: false,
    live: None,
    daemon: None,
};
