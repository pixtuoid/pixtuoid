//! omp's cites are `can1357/oh-my-pi` at `v18.12.1`, `src/` its
//! `packages/coding-agent/src/`.

use super::type_field;
use crate::harness::hermetic::{
    ConfigFile, Package, PerOs, Profile, Scenario, Shape, Surface, Turn, tui,
};
use crate::harness::plan::Placeholder;

const TUI: tui::Tui = tui::Tui {
    args: &[],
    composer: tui::Composer {
        marker: "╰─",
        border: tui::Border::Nothing,
    },
    gates: &[],
    cancel: &[],
    // `src/slash-commands/builtin-lifecycle.ts:835-839`.
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    conpty: tui::ConPty::Renders,
};

pub(crate) const PROFILE: Profile = Profile {
    source: "omp",
    // The release's own binaries: the npm package runs only under Bun (its
    // `engines`), and `scripts/install.sh` fetches from build.stencil.so
    // and runs `omp --version` under the operator's home (`:21`, `:331`).
    package: Package::GithubRelease {
        repo: "can1357/oh-my-pi",
        // `scripts/ci-build-upload.ts:39-48`, glibc's on Linux.
        assets: &[
            ("macos", "aarch64", "omp-darwin-arm64"),
            ("macos", "x86_64", "omp-darwin-x64"),
            ("linux", "aarch64", "omp-linux-arm64"),
            ("linux", "x86_64", "omp-linux-x64"),
            ("windows", "aarch64", "omp-windows-arm64.exe"),
            ("windows", "x86_64", "omp-windows-x64.exe"),
        ],
        sums: "SHA256SUMS.txt",
    },
    invocation: &[
        "omp",
        "-p",
        "--model",
        "aimock/gpt-4.1",
        Placeholder::Prompt.as_str(),
    ],
    second_turn: None,
    // The auto-QA report posts to qa.omp.sh (`src/tools/settings.ts:962-978`).
    env: &[("PI_AUTO_QA", "0")],
    homes: &[],
    system_config: PerOs {
        unix: &[],
        windows: &[],
    },
    // Context files stop at the git root when the workspace is outside the
    // home (`src/discovery/helpers.ts:721-735`), and the recorder's
    // `git init` makes the workspace that root; Cline's rules don't stop
    // (`src/discovery/cline.ts:19-35`).
    ancestor_config: &[".clinerules"],
    config: &[
        ConfigFile {
            home: "PI_CODING_AGENT_DIR",
            path: "models.yml",
            template: r#"providers:
  aimock:
    baseUrl: http://127.0.0.1:{port}/v1
    apiKey: "{key}"
    api: openai-completions
    models:
      - id: gpt-4.1
        reasoning: true
"#,
        },
        // A headless prompt is refused, and `always-ask` prompts for every
        // tool but a read (`docs/approval-mode.md:18-24`), so only the spawn
        // is allowed. `-p` exits once the turn ends, killing a background
        // child, so the spawn blocks (`src/task/settings.ts:251`). The TUI
        // would open on a setup wizard and fetch the latest release
        // (`src/main.ts:240-254`, `:668-679`).
        ConfigFile {
            home: "PI_CODING_AGENT_DIR",
            path: "config.yml",
            template: r#"tools:
  approvalMode: always-ask
  approval:
    task: allow
async:
  enabled: false
startup:
  setupWizard: false
  checkUpdate: false
"#,
        },
    ],
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &["session_start", "session_shutdown"],
            fixture: "omp/fixtures/tool-run-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "permission",
            shape: Shape::Permission,
            surface: Surface::Headless,
            expects: &[
                "session_start",
                "tool_approval_requested",
                "tool_approval_resolved",
                "session_shutdown",
            ],
            fixture: "omp/fixtures/permission-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "subagent",
            shape: Shape::Subagent,
            surface: Surface::Headless,
            expects: &["session_start", "session_shutdown"],
            fixture: "delegation/fixtures/omp",
            turn: Turn::PROFILE,
        },
        // `/new` is a TUI builtin, which `-p` never runs
        // (`src/slash-commands/builtin-lifecycle.ts:159-167`,
        // `src/session/agent-session.ts:7574-7596`); its switch names the
        // session it leaves (`:10250-10255`).
        Scenario {
            name: "new-session",
            shape: Shape::Succession,
            surface: Surface::Tui { row: &TUI },
            expects: &["session_start", "session_switch", "session_shutdown"],
            fixture: "omp/fixtures/new-session-hermetic",
            turn: Turn {
                prompt: Some("/new"),
                ..Turn::PROFILE
            },
        },
    ],
    script_vars: &[],
    not_yet: &["session_branch"],
    event_name: type_field,
    not_yet_fields: &[
        // Only a UI approves a prompt (`src/extensibility/extensions/wrapper.ts:386-398`).
        ("hook:tool_approval_resolved", "/approved"),
        // Only an interactive run titles a session (`src/main.ts:2258`).
        ("tx:title", "/title"),
        ("tx:title_change", "/title"),
        ("tx:title_change", "/type"),
        // An `ask` round, which no shape scripts.
        (
            "tx:message",
            "/message/content/*/arguments/questions/*/question",
        ),
    ],
    not_yet_literals: &["ask"],
    children_inherit_cwd: false,
    result_unseen: false,
    live: None,
    daemon: None,
    server_stop: None,
    turn_exits: &[],
};
