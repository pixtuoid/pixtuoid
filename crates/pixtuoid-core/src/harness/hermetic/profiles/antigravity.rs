//! agy is closed source: its cites are its installer,
//! antigravity.google/cli/install.sh, and the agy 1.4.8 binary.

use crate::harness::plan::Placeholder;
use std::borrow::Cow;

use serde_json::Value;

use crate::harness::hermetic::{
    ConfigFile, Package, PerOs, Profile, Scenario, Shape, Surface, Turn,
};

fn shim_stamped_event(v: &Value) -> Option<Cow<'_, str>> {
    v.get(crate::source::decoder::EVENT_STAMP)?
        .as_str()
        .map(Cow::Borrowed)
}

pub(crate) const PROFILE: Profile = Profile {
    source: crate::source::antigravity::SOURCE_NAME,
    package: Package::UpdaterManifest {
        manifest: "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/{platform}.json",
        // The installer's `${os}_${arch}`, glibc's on Linux (`install.sh:69-99`).
        platforms: &[
            ("macos", "aarch64", "darwin_arm64"),
            ("macos", "x86_64", "darwin_amd64"),
            ("linux", "aarch64", "linux_arm64"),
            ("linux", "x86_64", "linux_amd64"),
            ("windows", "aarch64", "windows_arm64"),
            ("windows", "x86_64", "windows_amd64"),
        ],
        member: "antigravity",
    },
    invocation: &["agy", "-p", Placeholder::Prompt.as_str()],
    second_turn: None,
    // The Gemini API route at a custom endpoint, and Go's proxy variables
    // (pkg.go.dev/net/http#ProxyFromEnvironment) sending all but the mock
    // to a closed port: an unproxied run updated its own binary.
    env: &[
        ("GEMINI_API_KEY", "{key}"),
        ("GOOGLE_GEMINI_BASE_URL", "http://127.0.0.1:{port}"),
        ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ("HTTP_PROXY", "http://127.0.0.1:9"),
        ("NO_PROXY", "127.0.0.1,localhost"),
    ],
    homes: &[],
    system_config: PerOs {
        unix: &[],
        windows: &[],
    },
    // An AGENTS.md, GEMINI.md or .agents/AGENTS.md above the git root
    // reached no request in a run, and the recorder's `git init` makes the
    // workspace that root.
    ancestor_config: &[],
    // `agy changelog`, 1.1.13: the key route reads `modelProvider`.
    config: &[ConfigFile {
        home: "HOME",
        path: ".gemini/antigravity-cli/settings.json",
        template: "{\"modelProvider\": \"gemini\"}\n",
    }],
    scenarios: &[
        Scenario {
            name: "tool-run",
            shape: Shape::ToolRun,
            surface: Surface::Headless,
            expects: &["SessionStart", "PreToolUse", "PostToolUse", "Stop"],
            fixture: "fixtures/antigravity/tool-run-hermetic",
            turn: Turn::PROFILE,
        },
        // `-p` answers the question itself: "Auto-answering ask_question at
        // step %d with skipped=true" (the 1.4.8 binary's `ask_question_manager.go`).
        Scenario {
            name: "ask-question",
            shape: Shape::Permission,
            surface: Surface::Headless,
            expects: &["SessionStart", "PreToolUse", "PostToolUse", "Stop"],
            fixture: "fixtures/antigravity/ask-question-hermetic",
            turn: Turn::PROFILE,
        },
    ],
    script_vars: &[],
    not_yet: &[],
    event_name: shim_stamped_event,
    not_yet_fields: &[],
    not_yet_literals: &[],
    children_inherit_cwd: false,
    // agy returns a result under role `model`, and aimock's Gemini route
    // reads one only under `user` (`@copilotkit/aimock` `dist/gemini.js:55-71`).
    result_unseen: true,
    live: None,
    daemon: None,
    server_stop: None,
    turn_exits: &[],
};
