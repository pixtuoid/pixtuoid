//! openclaw's cites are `openclaw/openclaw` at `v2026.10.1`.

use super::type_field;
use crate::harness::hermetic::{
    ConfigFile, Daemon, Live, Package, PerOs, Profile, Scenario, Shape, Surface, Turn,
};
use crate::harness::plan::Placeholder;

pub(crate) const PROFILE: Profile = Profile {
    source: "openclaw",
    package: Package::Npm("openclaw"),
    invocation: &[
        "openclaw",
        "agent",
        "--session-id",
        "hermetic",
        "-m",
        Placeholder::Prompt.as_str(),
    ],
    second_turn: None,
    // Its automatic update checks, and the daily request each sends
    // (`docs/install/updating/automatic-updates.md:160-162`,
    // `docs/gateway/telemetry.md:11`).
    env: &[("OPENCLAW_NO_AUTO_UPDATE", "1")],
    homes: &[],
    system_config: PerOs {
        unix: &[],
        windows: &[],
    },
    ancestor_config: &[],
    // A local gateway on the run's daemon port, which every command dials
    // (`docs/help/faq/gateway-ports-and-remote-mode.md:16`) and `gateway run`
    // refuses without `gateway.mode`; a provider on the scripted model
    // (`docs/gateway/config-tools/custom-providers.md`), its model the agents' default, and no separate utility model to title
    // the session (`docs/gateway/config-agents/models.md:63-66`). The agent works
    // in the run's workspace, not its state dir's, and writes no bootstrap
    // files there (`docs/gateway/config-agents/workspace-and-bootstrap.md:12-22`, `:81-83`).
    config: &[ConfigFile {
        home: "HOME",
        path: ".openclaw/openclaw.json",
        template: r#"{
  "gateway": { "mode": "local", "bind": "loopback", "port": {daemon_port} },
  "models": { "providers": { "hermetic": {
    "baseUrl": "http://127.0.0.1:{port}/v1",
    "api": "openai-completions",
    "apiKey": "{key}",
    "models": [{ "id": "hermetic", "name": "hermetic" }]
  } } },
  "agents": { "defaults": {
    "model": "hermetic/hermetic",
    "utilityModel": "",
    "workspace": "{workspace_slashed}",
    "skipBootstrap": true
  } }
}
"#,
    }],
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[
                "gateway_start",
                "session_start",
                "before_agent_run",
                "agent_end",
                "gateway_stop",
                "session_end",
            ],
            fixture: "fixtures/openclaw/tool-run-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "model-error",
            shape: Shape::ModelError,
            surface: Surface::Headless,
            expects: &[
                "gateway_start",
                "session_start",
                "before_agent_run",
                "agent_end",
                "gateway_stop",
                "session_end",
            ],
            fixture: "fixtures/openclaw/model-error-hermetic",
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
    live: Some(Live {
        script: "scripts/lib/tier-openclaw-multi.sh",
        // The script's own default count.
        ports: 4,
    }),
    daemon: Some(Daemon {
        start: &["gateway", "run"],
        ready: &["gateway", "health"],
        // Its gateway stops gracefully on its stdin's EOF, the recorder's
        // stop (`src/cli/gateway-cli/host-lifeline.ts:4-14`, `:40-47`,
        // `src/cli/gateway-cli/run-loop.ts:1214-1217`,
        // `docs/platforms/mac/bundled-gateway.md:46-49`).
        env: &[("OPENCLAW_GATEWAY_HOST_LIFELINE", "stdin")],
    }),
    server_stop: None,
};
