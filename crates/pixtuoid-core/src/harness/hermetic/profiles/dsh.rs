//! dsh's cites are `deepseek-ai/deepseek-harness` at `dsh-v0.2.1-rc.2`.

use crate::harness::plan::Placeholder;
use std::borrow::Cow;

use serde_json::Value;

use crate::harness::hermetic::{Package, PerOs, Profile, Scenario, Shape, Surface, Turn};

/// The event dsh's plugin forwards as each payload `type` (`dsh_plugin.mjs`).
fn event(v: &Value) -> Option<Cow<'_, str>> {
    Some(Cow::Borrowed(match v.get("type")?.as_str()? {
        "session_start" => "agent/created",
        "session_end" => "agent/disposed",
        "model" => "request/header",
        "usage" => "assistant/message",
        "tool_call" => "tool/call",
        "tool_result" => "tool/result",
        "approval_asked" => "approval/asked",
        "approval_decided" => "approval/decided",
        _ => return None,
    }))
}

pub(crate) const PROFILE: Profile = Profile {
    source: "dsh",
    package: Package::Npm("@deepseek-ai/dsh"),
    invocation: &["dsh", "--profile", "headless", Placeholder::Prompt.as_str()],
    second_turn: None,
    // The Messages endpoint and key come from the launch environment
    // (`packages/llm/llm-deepseek/src/config.ts:300`,
    // `packages/llm/llm-deepseek-api-key/src/index.ts:20-35`); the feedback
    // upload's switch (`packages/bundle/base/cordis.patch.yml:207-209`).
    env: &[
        ("DEEPSEEK_BASE_URL", "http://127.0.0.1:{port}"),
        ("DEEPSEEK_API_KEY", "{key}"),
        ("DSH_TELEMETRY_DISABLED", "1"),
    ],
    homes: &["DSH_HOME"],
    system_config: PerOs {
        unix: &[],
        windows: &[],
    },
    // AGENTS.md and CLAUDE.md stop at the nearest `.git`
    // (`packages/context/agent-instructions/README.md:38`), which the
    // recorder's `git init` makes the workspace.
    ancestor_config: &[],
    config: &[],
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[
                "agent/created",
                "request/header",
                "assistant/message",
                "tool/call",
                "tool/result",
                "agent/disposed",
            ],
            fixture: "fixtures/dsh/tool-run-recorded",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "permission",
            shape: Shape::Permission,
            surface: Surface::Headless,
            expects: &[
                "agent/created",
                "request/header",
                "assistant/message",
                "tool/call",
                "approval/asked",
                "approval/decided",
                "tool/result",
                "agent/disposed",
            ],
            fixture: "fixtures/dsh/permission-recorded",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "subagent",
            shape: Shape::Subagent,
            surface: Surface::Headless,
            expects: &[
                "agent/created",
                "request/header",
                "assistant/message",
                "tool/call",
                "tool/result",
                "agent/disposed",
            ],
            fixture: "delegation/fixtures/dsh",
            turn: Turn::PROFILE,
        },
    ],
    // The shell tool win32 mounts in bash's place, which alone can
    // escalate there.
    script_vars: &[(
        Placeholder::ShellTool.as_str(),
        PerOs {
            unix: "bash",
            windows: "pwsh",
        },
    )],
    not_yet: &[],
    event_name: event,
    not_yet_fields: &[],
    not_yet_literals: &[],
    children_inherit_cwd: false,
    result_unseen: false,
    live: None,
    daemon: None,
    server_stop: None,
};
