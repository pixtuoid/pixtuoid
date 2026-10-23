//! Jcode's hook environment and named provider config at `1jehuang/jcode` v0.99.0.

use std::borrow::Cow;

use crate::harness::hermetic::{
    ConfigFile, Package, PerOs, Profile, Scenario, Shape, Surface, Turn,
};
use crate::harness::plan::Placeholder;

pub(crate) const PROFILE: Profile = Profile {
    source: "jcode",
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
    invocation: &[
        "jcode",
        "--no-update",
        "--no-selfdev",
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
            "session_start",
            "session_end",
            "turn_start",
            "turn_end",
            "pre_tool",
            "post_tool",
        ],
        fixture: "fixtures/jcode/tool-run-hermetic",
        turn: Turn::PROFILE,
    }],
    script_vars: &[],
    not_yet: &[],
    event_name: |v| v.get("event")?.as_str().map(Cow::Borrowed),
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
    fn headless_run_is_isolated_from_user_server_and_background_work() {
        assert!(PROFILE.invocation.contains(&"--no-update"));
        assert!(PROFILE.invocation.contains(&"--no-selfdev"));
        assert!(
            PROFILE
                .invocation
                .windows(2)
                .any(|args| args == ["--socket", "jcode.sock"])
        );
        assert!(PROFILE.env.contains(&("JCODE_RUN_MCP", "0")));
        assert!(PROFILE.env.contains(&("JCODE_RUN_AUTO_POKE", "0")));
        assert!(PROFILE.homes.contains(&"JCODE_HOME"));
        assert!(PROFILE.not_yet.is_empty());
    }
}
