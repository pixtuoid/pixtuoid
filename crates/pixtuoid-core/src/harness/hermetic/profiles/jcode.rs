//! Jcode's hook environment and named provider config at `1jehuang/jcode` v0.99.1.

use super::event_field;
use crate::harness::hermetic::{
    ConfigFile, Package, PerOs, Profile, Scenario, Shape, Surface, Turn,
};
use crate::harness::plan::Placeholder;

pub(crate) const PROFILE: Profile = Profile {
    source: crate::source::jcode::SOURCE_NAME,
    package: Package::GithubRelease {
        repo: "1jehuang/jcode",
        assets: &[
            ("macos", "aarch64", "jcode-macos-aarch64.tar.gz"),
            ("macos", "x86_64", "jcode-macos-x86_64.tar.gz"),
            ("linux", "aarch64", "jcode-linux-aarch64.tar.gz"),
            ("linux", "x86_64", "jcode-linux-x86_64.tar.gz"),
            ("windows", "aarch64", "jcode-windows-aarch64.exe"),
            ("windows", "x86_64", "jcode-windows-x86_64.exe"),
        ],
        sums: "SHA256SUMS",
    },
    plugin_types: &[],
    invocation: &[
        "jcode",
        "--no-update",
        "--no-selfdev",
        // A server of the run's own, never the user's.
        "--socket",
        "jcode.sock",
        "--provider-profile",
        "hermetic",
        "--tools",
        "read",
        "run",
        Placeholder::Prompt.as_str(),
    ],
    second_turn: None,
    // `jcode run` connects MCP and pokes open todos unless told not to
    // (`src/cli/commands.rs:2719`, `:2732`).
    env: &[
        ("JCODE_RUN_MCP", "0"),
        ("JCODE_RUN_AUTO_POKE", "0"),
        ("JCODE_NO_TELEMETRY", "1"),
        ("JCODE_NO_AUTO_UPDATE", "1"),
    ],
    homes: &["JCODE_HOME"],
    system_config: PerOs {
        unix: &[],
        windows: &[],
    },
    ancestor_config: &[],
    config: &[ConfigFile {
        home: "JCODE_HOME",
        path: "config.toml",
        template: r#"[providers.hermetic]
type = "openai-compatible"
base_url = "http://127.0.0.1:{port}/v1"
api_key = "{key}"
default_model = "hermetic"
model_catalog = false
[menubar]
enabled = false
"#,
    }],
    scenarios: &[Scenario {
        name: "tool-run",
        shape: Shape::ToolRun,
        surface: Surface::Headless,
        expects: &[
            "session_end",
            "turn_start",
            "turn_end",
            "pre_tool",
            "post_tool",
        ],
        turn: Turn::PROFILE,
    }],
    script_vars: &[],
    not_yet: &[],
    event_name: event_field,
    not_yet_fields: &[],
    not_yet_literals: &[],
    children_inherit_cwd: false,
    result_unseen: false,
    live: None,
    daemon: None,
    server_stop: None,
    turn_exits: &[],
};
