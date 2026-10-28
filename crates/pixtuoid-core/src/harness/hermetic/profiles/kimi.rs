//! kimi's cites are `MoonshotAI/kimi-code` at `@moonshot-ai/kimi-code@2.2.0`.

use super::{AIMOCK_V1, hook_event_name};
use crate::harness::hermetic::{Package, PerOs, Profile, Scenario, Shape, Surface, Turn, tui};
use crate::harness::plan::Placeholder;

/// kimi's TUI. Always Ask, its default mode, asks before a command
/// (`docs/en/guides/interaction.md:67`); `-p` never asks (`docs/en/reference/kimi-command.md:43`).
const TUI: tui::Tui = tui::Tui {
    args: &[],
    composer: tui::Composer {
        marker: " │ >",
        border: tui::Border::Rule {
            glyph: '─',
            corners: Some(('╭', '╮')),
        },
    },
    gates: &[tui::Gate {
        shows: "Run this command?",
        // Esc rejects the call outright
        // (`apps/kimi-code/src/tui/components/dialogs/approval-panel.ts:262-268`).
        keys: &[tui::Key::Esc],
        dwell: std::time::Duration::ZERO,
    }],
    cancel: &[],
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    conpty: tui::ConPty::Renders,
};

pub(crate) const PROFILE: Profile = Profile {
    source: crate::source::kimi::SOURCE_NAME,
    package: Package::Npm("@moonshot-ai/kimi-code"),
    plugin_types: &[],
    invocation: &["kimi", "-p", Placeholder::Prompt.as_str()],
    second_turn: None,
    // `KIMI_MODEL_NAME` builds a provider from the environment
    // (`docs/en/configuration/env-vars.md:106-124`); the telemetry and
    // update-check switches (`:27`, `:185`).
    env: &[
        ("KIMI_MODEL_NAME", "kimi-k2"),
        ("KIMI_MODEL_BASE_URL", AIMOCK_V1),
        ("KIMI_MODEL_API_KEY", "{key}"),
        ("KIMI_DISABLE_TELEMETRY", "1"),
        ("KIMI_CODE_NO_AUTO_UPDATE", "1"),
        // The TUI otherwise asks to trust the folder at boot (`:168`).
        ("KIMI_CODE_TRUST_WORKSPACE", "1"),
    ],
    homes: &[],
    system_config: PerOs {
        unix: &[],
        windows: &[],
    },
    // AGENTS.md and project skills stop at the git work tree
    // (`packages/agent-core-v2/src/agent/profile/context.ts:173`,
    // `packages/agent-core-v2/src/features/skill/catalog/skillRoots.ts:73`),
    // which the recorder's `git init` makes the workspace.
    ancestor_config: &[],
    config: &[],
    // A reply waits `latency`: a turn's end kills a still-running
    // fire-and-forget `PostToolUse`/`PostToolUseFailure` hook
    // (`packages/agent-core-v2/src/features/externalHooks/agent/agentExternalHooksService.ts:318-333`,
    // `human/agent/machine.ts:757-760`, `features/externalHooks/internal/runHook.ts:120-123`).
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &["SessionStart", "PreToolUse", "PostToolUse", "Stop"],
            fixture: "fixtures/kimi/tool-run-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "permission",
            shape: Shape::Permission,
            surface: Surface::Tui { row: &TUI },
            expects: &[
                "SessionStart",
                "PreToolUse",
                "PermissionRequest",
                "PostToolUseFailure",
                "Stop",
                "SessionEnd",
            ],
            fixture: "fixtures/kimi/permission-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "model-error",
            shape: Shape::ModelError,
            surface: Surface::Headless,
            expects: &["SessionStart", "StopFailure"],
            fixture: "fixtures/kimi/model-error-hermetic",
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
    server_stop: None,
    turn_exits: &[],
};
