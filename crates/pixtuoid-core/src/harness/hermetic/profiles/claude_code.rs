use super::hook_event_name;
use crate::harness::hermetic::{
    ConfigFile, HostConfig, Package, PerOs, Profile, Scenario, Second, Shape, Surface, Turn, tui,
};
use crate::harness::plan::Placeholder;

/// Claude Code's TUI. Manual mode asks before a non-read-only Bash call
/// (code.claude.com/docs/en/permission-modes).
const TUI: tui::Tui = tui::Tui {
    args: &["--permission-mode", "manual"],
    composer: tui::Composer {
        marker: "❯",
        above: tui::Above::Rule {
            glyph: '─',
            corners: None,
        },
    },
    gates: &[tui::Gate {
        shows: "Do you want to proceed?",
        deny: &[tui::Key::Esc],
        // Past the 6 s an unanswered dialog waits before it fires `Notification`
        // (`qYt=6000` in `eTe`, the 2.1.302 bundle), and so past the 150 ms it
        // ignores keys for (`AM=150`).
        dwell: std::time::Duration::from_secs(7),
    }],
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    renders_under_conpty: true,
};

pub(crate) const PROFILE: Profile = Profile {
    source: "claude-code",
    package: Package::Npm("@anthropic-ai/claude-code"),
    invocation: &["claude", "-p", Placeholder::Prompt.as_str()],
    // `-r, --resume [value]`: "Resume a conversation by session ID"
    // (`claude --help` at 2.1.302).
    second_turn: Some(&[
        "claude",
        "-p",
        Placeholder::Prompt.as_str(),
        "--resume",
        Placeholder::Session.as_str(),
    ]),
    env: &[
        ("ANTHROPIC_BASE_URL", "http://127.0.0.1:{port}"),
        // A bearer token, not `ANTHROPIC_API_KEY`: the TUI asks to approve an API
        // key once (code.claude.com/docs/en/env-vars), a dialog no run should answer.
        ("ANTHROPIC_AUTH_TOKEN", "{key}"),
        // Auto-updates, telemetry, error reporting and the fast-mode probe
        // (code.claude.com/docs/en/env-vars); the plugin marketplace's
        // auto-install has its own switch.
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
        ("CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL", "1"),
    ],
    homes: &[],
    // Managed settings, managed MCP servers and a managed CLAUDE.md, and
    // on Windows the registry policy beside them
    // (code.claude.com/docs/en/managed-settings).
    system_config: PerOs {
        unix: &[
            HostConfig::Path("/Library/Application Support/ClaudeCode"),
            HostConfig::Path("/etc/claude-code"),
        ],
        windows: &[
            HostConfig::Path(r"C:\Program Files\ClaudeCode"),
            HostConfig::Registry(r"HKLM\SOFTWARE\Policies\ClaudeCode"),
            HostConfig::Registry(r"HKCU\SOFTWARE\Policies\ClaudeCode"),
        ],
    },
    // Memory files up to `/`, and AGENTS.md where none is found
    // (code.claude.com/docs/en/memory).
    ancestor_config: &["CLAUDE.md", "CLAUDE.local.md", ".claude", "AGENTS.md"],
    // The TUI's first launch: onboarding, then a trust dialog per folder.
    config: &[ConfigFile {
        home: "CLAUDE_CONFIG_DIR",
        path: ".claude.json",
        template: r#"{"hasCompletedOnboarding":true,"projects":{"{workspace_slashed}":{"hasTrustDialogAccepted":true}}}"#,
    }],
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &["SessionStart", "PreToolUse", "PostToolUse", "SessionEnd"],
            fixture: "fixtures/claude-code/tool-run-hermetic",
            turn: Turn::PROFILE,
        },
        // Ultracode on enters, a prompt naming ultrathink adds its effort,
        // and the resumed turn, ultracode off, exits (`GQr`, `jQr` in the
        // 2.1.302 bundle).
        Scenario {
            name: "ultra-effort",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &["SessionStart", "PreToolUse", "PostToolUse", "SessionEnd"],
            fixture: "fixtures/claude-code/ultra-effort-hermetic",
            turn: Turn {
                args: &["--effort", "ultracode"],
                prompt: Some("ultrathink: read NOTE.txt."),
                second: Some(Second::Prompt("Say done.")),
            },
        },
        Scenario {
            name: "subagent",
            shape: Shape::Subagent,
            surface: Surface::Headless,
            expects: &[
                "SessionStart",
                "PreToolUse",
                "PostToolUse",
                "SubagentStart",
                "SubagentStop",
                "SessionEnd",
            ],
            fixture: "claude/fixtures/subagent-hermetic",
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
                "PreToolUse",
                "PermissionRequest",
                "Notification",
                "SessionEnd",
            ],
            fixture: "fixtures/claude-code/permission-hermetic",
            turn: Turn::PROFILE,
        },
    ],
    script_vars: &[],
    not_yet: &[],
    event_name: hook_event_name,
    not_yet_fields: &[
        // aimock's Messages usage is input and output tokens alone
        // (`CopilotKit/aimock` `src/messages.ts:413-421` at v1.45.1).
        ("tx:assistant", "/message/usage/cache_creation_input_tokens"),
    ],
    not_yet_literals: &[],
    children_inherit_cwd: false,
    result_unseen: false,
    live: None,
    daemon: None,
};
