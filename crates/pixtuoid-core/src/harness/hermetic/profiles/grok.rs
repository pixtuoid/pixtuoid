//! grok's cites are `xai-org/grok-build` at `37949780c1`, the monorepo sync
//! nearest 1.0.25 (the repo tags no release), unless a cite names its own ref.

use crate::harness::plan::Placeholder;
use std::borrow::Cow;

use serde_json::Value;

use super::AIMOCK_V1;
use crate::harness::hermetic::{
    HostConfig, Package, PerOs, Profile, Scenario, Shape, Surface, Turn, tui,
};

/// grok's TUI. Its default mode asks before a shell command off the read-only
/// list (`crates/codegen/xai-grok-pager/docs/user-guide/22-permissions-and-safety.md:140-175`).
const TUI: tui::Tui = tui::Tui {
    args: &["--permission-mode", "default"],
    composer: tui::Composer {
        marker: "  │ ❯",
        above: tui::Above::Rule {
            glyph: '─',
            corners: Some(('╭', '╮')),
        },
    },
    gates: &[tui::Gate {
        shows: "No, reject (type to add feedback)",
        deny: &[tui::Key::Text("4")],
        dwell: std::time::Duration::ZERO,
    }],
    // `/exit` is `/quit`'s alias
    // (`crates/codegen/xai-grok-pager/docs/user-guide/04-slash-commands.md:71-73`).
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    // It clears the alternate screen and paints nothing more (`hermetic.yml`'s
    // Windows leg, run 39177894288's `grok/permission/tui.log`).
    renders_under_conpty: false,
};

/// The registration a grok envelope's `hookEventName` answers to: the wire
/// spells each key in snake_case
/// (`crates/codegen/xai-grok-hooks/src/event.rs:78-180`).
fn event(v: &Value) -> Option<Cow<'_, str>> {
    let wire = v.get("hookEventName")?.as_str()?;
    let key = wire
        .split('_')
        .flat_map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|c| c.to_ascii_uppercase())
                .into_iter()
                .chain(chars)
        })
        .collect();
    Some(Cow::Owned(key))
}

pub(crate) const PROFILE: Profile = Profile {
    source: "grok",
    package: Package::Npm("@xai-official/grok"),
    // Headless automation runs always-approve
    // (`crates/codegen/xai-grok-pager/docs/user-guide/22-permissions-and-safety.md:76-94`),
    // which a blocking spawn needs: `-p` cancels every prompt
    // (`crates/codegen/xai-grok-pager/src/headless.rs:1882-1901`).
    invocation: &[
        "grok",
        "--always-approve",
        "-p",
        Placeholder::Prompt.as_str(),
    ],
    second_turn: None,
    // What upstream's own run against a mock server sets: every service
    // URL on it, and the telemetry, feedback, update and unscripted
    // side-request switches
    // (`crates/codegen/xai-grok-test-support/src/sandbox.rs:275-288`, `:329-348`).
    env: &[
        ("XAI_API_KEY", "{key}"),
        ("GROK_XAI_API_BASE_URL", AIMOCK_V1),
        ("GROK_CLI_CHAT_PROXY_BASE_URL", AIMOCK_V1),
        ("GROK_MODELS_BASE_URL", AIMOCK_V1),
        ("GROK_FEEDBACK_BASE_URL", AIMOCK_V1),
        ("GROK_TRACE_UPLOAD_URL", AIMOCK_V1),
        ("GROK_MANAGED_CONFIG_URL", AIMOCK_V1),
        ("GROK_CODE_WEB_URL", AIMOCK_V1),
        ("GROK_CONVERSATIONS_BASE_URL", AIMOCK_V1),
        ("GROK_TELEMETRY_ENABLED", "false"),
        ("GROK_TELEMETRY_MIXPANEL_ENABLED", "false"),
        ("GROK_TELEMETRY_MIXPANEL_TOKEN", ""),
        ("GROK_TELEMETRY_EVENTS_URL", ""),
        ("GROK_TELEMETRY_EVENTS_API_KEY", ""),
        ("GROK_TELEMETRY_TRACE_UPLOAD", "false"),
        ("GROK_FEEDBACK_ENABLED", "false"),
        ("GROK_TRACE_UPLOAD", "false"),
        ("GROK_INSTRUMENTATION", "disabled"),
        ("OTEL_SDK_DISABLED", "true"),
        ("DISABLE_TELEMETRY", "1"),
        ("GROK_DISABLE_AUTOUPDATER", "1"),
        ("GROK_PROMPT_SUGGESTIONS", "false"),
        ("GROK_TURN_SUMMARY", "0"),
    ],
    homes: &[],
    // Its system config, and the Claude Code managed settings it imports
    // (`crates/codegen/xai-grok-config/src/paths.rs:5-9`, `:23-29`): none
    // on Windows, where every home path is std's, which reads USERPROFILE
    // (`crates/codegen/xai-dirs/src/lib.rs:32-36`).
    system_config: PerOs {
        unix: &[
            HostConfig::Path("/etc/grok"),
            HostConfig::Path("/Library/Application Support/ClaudeCode/managed-settings.json"),
            HostConfig::Path("/etc/claude-code/managed-settings.json"),
        ],
        windows: &[],
    },
    // Instructions, rules and project config stop at the git work tree
    // (`crates/codegen/xai-grok-agent/src/repo.rs:23-52`), which the
    // recorder's `git init` makes the workspace.
    ancestor_config: &[],
    config: &[],
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
            fixture: "fixtures/grok/tool-run-hermetic",
            turn: Turn::PROFILE,
        },
        // A tool that fails to dispatch fires PostToolUseFailure, a built-in's
        // logical error PostToolUse, and `use_tool` on a tool no server holds
        // errs (at main 2bdd1d6a: `crates/codegen/xai-grok-pager/docs/user-guide/10-hooks.md:94-95`,
        // `crates/codegen/xai-grok-tools/src/registry/types.rs:1611-1615`,
        // `crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_calls.rs:1064-1074`, `:1148-1152`).
        Scenario {
            name: "tool-error",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUseFailure",
                "Stop",
                "SessionEnd",
            ],
            fixture: "fixtures/grok/tool-error-hermetic",
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
            fixture: "grok/fixtures/subagent-hermetic",
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
                "Notification",
                "PermissionDenied",
                "StopCancelled",
                "SessionEnd",
                "Stop",
            ],
            fixture: "fixtures/grok/permission-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "model-error",
            shape: Shape::ModelError,
            surface: Surface::Headless,
            expects: &["SessionStart", "UserPromptSubmit", "StopFailure"],
            fixture: "fixtures/grok/model-error-hermetic",
            turn: Turn::PROFILE,
        },
        // A question headless cannot answer: the shell fires the
        // `elicitation_dialog` notification, then headless cancels it at
        // once (at main 2bdd1d6a: `crates/codegen/xai-grok-shell/src/session/acp_session_impl/spawn.rs:2336-2341`,
        // `crates/codegen/xai-grok-pager/src/headless/ext_protocol.rs:27`).
        Scenario {
            name: "ask-user",
            shape: Shape::Permission,
            surface: Surface::Headless,
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "Notification",
                "PostToolUse",
                "Stop",
                "SessionEnd",
            ],
            fixture: "fixtures/grok/ask-user-hermetic",
            turn: Turn::PROFILE,
        },
    ],
    script_vars: &[],
    not_yet: &[],
    event_name: event,
    not_yet_fields: &[],
    not_yet_literals: &[],
    children_inherit_cwd: false,
    result_unseen: false,
    live: None,
    daemon: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grok_event_is_the_registration_key_its_wire_name_spells() {
        // A key `GROK_EVENTS` adds maps with no edit here, so a run that fires
        // it reads Stale.
        for (wire, key) in [
            ("session_start", "SessionStart"),
            ("post_tool_use_failure", "PostToolUseFailure"),
            ("a_new_hook", "ANewHook"),
        ] {
            let v = serde_json::json!({ "hookEventName": wire });
            assert_eq!(event(&v).as_deref(), Some(key));
        }
        assert_eq!(event(&serde_json::json!({})), None);
    }
}
