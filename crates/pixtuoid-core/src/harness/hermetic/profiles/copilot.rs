//! copilot is closed source: its cites are docs.github.com,
//! `github/copilot-cli` and the `@github/copilot-<platform>@1.0.98` binaries.

use super::AIMOCK_V1;
use crate::harness::hermetic::{
    HostConfig, Package, PerOs, Profile, Scenario, Second, Shape, Surface, Turn,
};
use crate::harness::host::Hive;
use crate::harness::plan::Placeholder;

pub(crate) const PROFILE: Profile = Profile {
    source: "copilot",
    package: Package::Npm("@github/copilot"),
    invocation: &["copilot", "-p", Placeholder::Prompt.as_str()],
    // `-r, --resume [<value>]`: "optionally specify existing session ID"
    // (`copilot --help` at 1.0.98).
    second_turn: Some(&[
        "copilot",
        "--resume",
        Placeholder::Session.as_str(),
        "-p",
        Placeholder::Prompt.as_str(),
    ]),
    // The BYOK provider, with GitHub's servers kept out
    // (`/en/copilot/how-tos/copilot-cli/customize-copilot/use-byok-models`).
    env: &[
        ("COPILOT_OFFLINE", "true"),
        ("COPILOT_PROVIDER_BASE_URL", AIMOCK_V1),
        ("COPILOT_PROVIDER_API_KEY", "{key}"),
        ("COPILOT_MODEL", "gpt-4.1"),
    ],
    homes: &[],
    // Managed settings and the policy directory, and on Windows the
    // registry policy beside them: each a string of its 1.0.98 binary.
    system_config: PerOs {
        unix: &[
            HostConfig::Path("/Library/Application Support/GitHubCopilot/managed-settings.json"),
            HostConfig::Path("/etc/github-copilot/managed-settings.json"),
            HostConfig::Path("/etc/github-copilot/policy.d"),
        ],
        windows: &[
            HostConfig::Registry {
                hive: Hive::Hklm,
                sub: r"SOFTWARE\Policies\GitHubCopilot",
            },
            HostConfig::Registry {
                hive: Hive::Hklm,
                sub: r"Software\Policies\GitHub\Copilot",
            },
            HostConfig::Path(r"{FOLDERID_ProgramData}\GitHub\Copilot\policy.d"),
        ],
    },
    // Instructions stop at the repository root
    // (`/en/copilot/how-tos/copilot-cli/customize-copilot/add-custom-instructions`),
    // which the recorder's `git init` makes the workspace.
    ancestor_config: &[],
    config: &[],
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[],
            fixture: "fixtures/copilot/tool-run-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "resume",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[],
            fixture: "fixtures/copilot/resume-hermetic",
            turn: Turn {
                args: &[],
                prompt: None,
                second: Some(Second::Prompt("Say done.")),
            },
        },
        Scenario {
            name: "autopilot",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &[],
            fixture: "fixtures/copilot/autopilot-hermetic",
            // `--autopilot`: "Start in autopilot mode" (`copilot --help` at
            // 1.0.98), which offers the `task_complete` tool plain `-p` does not.
            turn: Turn {
                args: &["--autopilot"],
                ..Turn::PROFILE
            },
        },
        Scenario {
            name: "permission",
            shape: Shape::Permission,
            surface: Surface::Headless,
            expects: &[],
            fixture: "fixtures/copilot/permission-hermetic",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "subagent",
            shape: Shape::Subagent,
            surface: Surface::Headless,
            expects: &[],
            fixture: "delegation/fixtures/copilot",
            turn: Turn::PROFILE,
        },
        Scenario {
            name: "child-error",
            shape: Shape::ChildError,
            surface: Surface::Headless,
            expects: &[],
            fixture: "copilot/fixtures/child-error-hermetic",
            turn: Turn::PROFILE,
        },
    ],
    // The shell tool is `bash` or `powershell`
    // (`/en/copilot/reference/copilot-cli-reference/cli-command-reference`),
    // `powershell` on Windows (`github/copilot-cli#4594`, `#4719`).
    script_vars: &[(
        Placeholder::ShellTool.as_str(),
        PerOs {
            unix: "bash",
            windows: "powershell",
        },
    )],
    not_yet: &[],
    event_name: |_| None,
    // GitHub's billing response carries them (1.0.98's bundled
    // `cli-main.js` reads `copilotUsage.tokenDetails`); a BYOK run has none.
    not_yet_fields: &[
        (
            "tx:session.shutdown",
            "/data/tokenDetails/cache_write/tokenCount",
        ),
        ("tx:session.shutdown", "/data/tokenDetails/input/tokenCount"),
        (
            "tx:session.shutdown",
            "/data/tokenDetails/output/tokenCount",
        ),
    ],
    not_yet_literals: &[],
    // A `subagent.started` child runs in-process, in the parent's transcript,
    // and no event of it names a cwd.
    children_inherit_cwd: true,
    result_unseen: false,
    live: None,
    daemon: None,
};
