//! The hermetic tier's per-CLI facts: how each real CLI is installed, isolated,
//! pointed at a scripted model (aimock) and driven, and which hook events each
//! scenario must fire. The registry already owns the binary (`version_probe`),
//! the home variable (`home_env`) and the pinned version (`verified_version`);
//! a profile holds only what it doesn't.

use std::borrow::Cow;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::CaptureRecord;
use crate::source::registry;

pub mod bump;
pub mod pin;
pub mod verify;

/// The key a profile hands its CLI for its scripted model: aimock accepts
/// any, and a real provider must refuse it.
pub const DUMMY_KEY: &str = "hermetic-dummy-key";

/// The scripted model's `/v1` base, as a profile's env spells it.
const AIMOCK_V1: &str = "http://127.0.0.1:{port}/v1";

/// What a Unix [`Profile::system_config`] path spells the operator's user name as.
pub const USER_PLACEHOLDER: &str = "{user}";
/// What a script spells [`CAPTURE_NOTE`](crate::harness::CAPTURE_NOTE) as.
pub(crate) const NOTE_PLACEHOLDER: &str = "{note}";
/// What a script spells the workspace the recorder runs the CLI in as.
pub(crate) const WORKSPACE_PLACEHOLDER: &str = "{workspace}";
/// What a script spells the platform's shell tool as, a profile's
/// [`Profile::script_vars`] naming it.
pub(crate) const SHELL_TOOL_PLACEHOLDER: &str = "{shell_tool}";

/// A fact that differs between Unix and Windows. Both arms are plain data, so
/// a test on any host reads both, as `platform::resolve_user_home_opt` takes
/// its `windows` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerOs<T> {
    /// macOS and Linux.
    pub unix: T,
    /// Windows.
    pub windows: T,
}

impl<T> PerOs<T> {
    /// The arm for the platform this was built for.
    pub const fn here(&self) -> &T {
        if cfg!(windows) {
            &self.windows
        } else {
            &self.unix
        }
    }
}

/// Where a CLI reads policy or config that no sandbox variable redirects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostConfig {
    /// A path; a Unix one may spell [`USER_PLACEHOLDER`], a Windows one start
    /// with a placeholder [`crate::harness::host::known_folders_expanded`] resolves.
    Path(&'static str),
    /// A Windows registry key, `HKLM\…` or `HKCU\…`.
    Registry(&'static str),
}

/// How users install the CLI, so the tier runs the artifact they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    /// An npm package; the tier installs `<name>@<version>` into its own prefix.
    Npm(&'static str),
}

/// What a scenario's script drives a CLI through. Shared by every CLI, so one CLI
/// cannot be held to a weaker bar than another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A turn that reads a file: the session registers in the workspace, a tool
    /// starts with a detail naming it, and it ends.
    ToolRun,
    /// A turn whose tool call needs an approval the run cannot give: the slot
    /// waits on it, then the refused tool ends.
    Permission,
    /// A turn that hands a file read to a child session: the parent delegates,
    /// the child registers as its child and reads, and both end.
    Subagent,
}

impl Shape {
    /// The scenario's name: its script's file stem and its run directory.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ToolRun => "tool-run",
            Self::Permission => "permission",
            Self::Subagent => "subagent",
        }
    }
}

/// One scripted turn of a CLI.
#[derive(Debug)]
pub struct Scenario {
    /// What the run must look like.
    pub shape: Shape,
    /// The hook events this run must fire, as `Profile::event_name` reads them.
    pub expects: &'static [&'static str],
    /// The committed recording `--record` rewrites, under `tests/sources`: the
    /// tier's own, beside any capture still carrying a field the profile owes.
    pub fixture: &'static str,
}

/// A config file the tier writes into a sandbox home BEFORE `pixtuoid connect`,
/// which may merge its own into the same file (codex's `config.toml`).
#[derive(Debug)]
pub struct ConfigFile {
    /// The environment variable naming the home the file lives under.
    pub home: &'static str,
    /// The file's path under that home.
    pub path: &'static str,
    /// Its contents; `{port}` and `{key}` expand.
    pub template: &'static str,
}

/// One CLI's hermetic facts.
#[derive(Debug)]
pub struct Profile {
    /// The registered source id.
    pub source: &'static str,
    /// How users install it.
    pub package: Package,
    /// The headless invocation; `{prompt}` expands (the recorder's own rule).
    pub invocation: &'static [&'static str],
    /// Environment for the CLI: endpoint, dummy key and opt-outs; `{port}` and
    /// `{key}` expand.
    pub env: &'static [(&'static str, &'static str)],
    /// Homes the registry row does not name, each pointed into the sandbox.
    pub(crate) homes: &'static [&'static str],
    /// Fixed places the CLI reads policy or config from, which no home
    /// redirects: a run on a machine that has one is not hermetic. A macOS
    /// managed-preferences domain whose plist path upstream does not fix is not
    /// checked.
    pub system_config: PerOs<&'static [HostConfig]>,
    /// Names the CLI reads from the workspace's ancestor directories, which
    /// sit outside the sandbox: a run under one that has any is not hermetic.
    pub ancestor_config: &'static [&'static str],
    /// Config written before `connect`.
    pub config: &'static [ConfigFile],
    /// The scripted turns.
    pub scenarios: &'static [Scenario],
    /// Placeholders the scenario scripts spell, each the platform's value.
    pub(crate) script_vars: &'static [(&'static str, PerOs<&'static str>)],
    /// Registered hooks no scenario expects yet. Shrink-only: a run that fires
    /// one is Stale (`verify::verify_source`).
    pub not_yet: &'static [&'static str],
    /// The hook event a captured payload is, in `registers()` terms.
    pub(crate) event_name: fn(&Value) -> Option<Cow<'_, str>>,
    /// Fields a decoder reads in a committed capture that no scenario carries
    /// yet, as `(kind, path)` the field census names them. Shrink-only: a run
    /// that carries one is Stale.
    pub(crate) not_yet_fields: &'static [(&'static str, &'static str)],
}

impl Profile {
    /// Its registry row.
    ///
    /// # Panics
    ///
    /// If the profile names an unregistered source, which the roster test refuses.
    pub fn descriptor(&self) -> &'static registry::SourceDescriptor {
        registry::descriptor_for(self.source).expect("a profile names a registered source")
    }

    /// Every home variable the CLI reads that the tier points into its own
    /// directories: the registry row's and the profile's.
    pub fn home_vars(&self) -> impl Iterator<Item = &'static str> {
        self.descriptor()
            .home_env
            .into_iter()
            .chain(self.homes.iter().copied())
    }

    /// Every placeholder this profile's scripts may spell, with its value on
    /// this platform: `workspace` as the CLI reports its cwd.
    pub fn script_values(&self, workspace: &str) -> Vec<(&'static str, String)> {
        [
            (WORKSPACE_PLACEHOLDER, workspace.to_owned()),
            (NOTE_PLACEHOLDER, crate::harness::CAPTURE_NOTE.to_owned()),
        ]
        .into_iter()
        .chain(
            self.script_vars
                .iter()
                .map(|(k, v)| (*k, (*v.here()).to_owned())),
        )
        .collect()
    }
}

/// A run directory's manifest, beside the raw capture.
pub const MANIFEST: &str = "run.json";
/// The headless pixtuoid's output (scene lines and its log) for a run.
pub const SCENE_LOG: &str = "scene.log";
/// The provenance key naming the aimock a recording ran against, which only
/// the tier's own recordings carry.
pub const PROVENANCE_AIMOCK: &str = "aimock";

/// What one run was, written by the orchestrator beside the run's raw capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// The registered source id.
    pub source: String,
    /// The scenario's shape name.
    pub scenario: String,
    /// The recorder's record of the CLI's run.
    pub capture: CaptureRecord,
    /// The aimock version that played the model.
    pub aimock: String,
    /// Requests aimock answered.
    pub requests: usize,
    /// Requests no script fixture matched.
    pub unmatched: usize,
}

/// Why a registered source has no profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Absence {
    /// Its model endpoint cannot be redirected, so no scripted turn can run.
    NoModelEndpoint,
    /// A profile is owed. Shrink-only.
    AwaitingProfile,
}

fn hook_event_name(v: &Value) -> Option<Cow<'_, str>> {
    v.get("hook_event_name")?.as_str().map(Cow::Borrowed)
}

fn type_field(v: &Value) -> Option<Cow<'_, str>> {
    v.get("type")?.as_str().map(Cow::Borrowed)
}

fn event_field(v: &Value) -> Option<Cow<'_, str>> {
    v.get("event")?.as_str().map(Cow::Borrowed)
}

/// The event dsh's plugin forwards as each payload `type` (`dsh_plugin.mjs`).
fn dsh_event(v: &Value) -> Option<Cow<'_, str>> {
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

/// The registration a grok envelope's `hookEventName` answers to: the wire
/// spells each key in snake_case, `SubagentEnd`'s as `SubagentStop`'s
/// (`crates/codegen/xai-grok-hooks/src/event.rs:78-180`).
fn grok_event(v: &Value) -> Option<Cow<'_, str>> {
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

/// The profiled CLIs.
pub const PROFILES: &[Profile] = &[
    Profile {
        source: "claude-code",
        package: Package::Npm("@anthropic-ai/claude-code"),
        invocation: &["claude", "-p", "{prompt}"],
        env: &[
            ("ANTHROPIC_BASE_URL", "http://127.0.0.1:{port}"),
            ("ANTHROPIC_API_KEY", "{key}"),
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
        config: &[],
        scenarios: &[Scenario {
            shape: Shape::ToolRun,
            expects: &["SessionStart", "PreToolUse", "PostToolUse", "SessionEnd"],
            fixture: "fixtures/claude-code/tool-run-hermetic",
        }],
        script_vars: &[],
        not_yet: &[
            "Notification",
            "PermissionRequest",
            "SubagentStart",
            "SubagentStop",
        ],
        event_name: hook_event_name,
        not_yet_fields: &[
            // A Bash call's; the scripted turn reads a file.
            ("hook:PreToolUse", "/tool_input/command"),
            ("tx:assistant", "/message/content/*/input/command"),
            // `claude -p` sends no model; the interactive TUI does
            // (fixtures/claude-code/permission-recorded).
            ("hook:SessionStart", "/model"),
            // A subagent transcript's (fixtures/claude-code/subagent-recorded).
            ("tx:assistant", "/attributionAgent"),
            // aimock's Messages usage is input and output tokens alone
            // (`CopilotKit/aimock` `src/messages.ts:405-414` at v1.43.0).
            ("tx:assistant", "/message/usage/cache_creation_input_tokens"),
        ],
    },
    Profile {
        source: "codex",
        package: Package::Npm("@openai/codex"),
        // A hook runs only once trusted, and trust is a hash of codex's own
        // normalized hook identity (`codex-rs/hooks/src/engine/discovery.rs:775`)
        // that seeding would have to re-implement; this flag is upstream's path
        // for "automation that already vets hook sources"
        // (`codex-rs/utils/cli/src/shared_options.rs:62-64`).
        invocation: &[
            "codex",
            "exec",
            "--dangerously-bypass-hook-trust",
            "{prompt}",
        ],
        env: &[("HERMETIC_API_KEY", "{key}")],
        homes: &[],
        // System config, requirements and managed config; `CODEX_HOME` never
        // reaches them (`codex-rs/config/src/loader/mod.rs:69`, `:102-107`,
        // `layer_io.rs:222-227`). On Windows they live in the known
        // ProgramData (`:72`, `:823-885`), and skills come from the known
        // profile, which no variable moves (`ext/skills/src/host_roots.rs:18-36`).
        system_config: PerOs {
            unix: &[HostConfig::Path("/etc/codex")],
            windows: &[
                HostConfig::Path(r"{FOLDERID_ProgramData}\OpenAI\Codex"),
                HostConfig::Path(r"{FOLDERID_Profile}\.agents\skills"),
            ],
        },
        // Its project config and AGENTS.md stop at the project root
        // (`codex-rs/config/src/loader/mod.rs:1662`, `core/src/agents_md.rs:8-12`),
        // which the recorder's `git init` makes the workspace.
        ancestor_config: &[],
        // `[windows]` is what codex's own setup writes for its non-admin
        // sandbox (`codex-rs/tui/src/config_update.rs:111-126`), without which
        // read-only exec is Forbidden (`core/src/exec_policy.rs:799-806`);
        // only Windows reads it (`sandboxing/src/manager.rs:67-81`).
        config: &[ConfigFile {
            home: "CODEX_HOME",
            path: "config.toml",
            template: r#"model = "gpt-5.5"
model_provider = "aimock"
model_reasoning_effort = "high"

[model_providers.aimock]
name = "aimock"
base_url = "http://127.0.0.1:{port}/v1"
env_key = "HERMETIC_API_KEY"
wire_api = "responses"

[windows]
sandbox = "unelevated"
"#,
        }],
        scenarios: &[Scenario {
            shape: Shape::ToolRun,
            expects: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
                "SessionEnd",
            ],
            fixture: "fixtures/codex/tool-run-hermetic",
        }],
        script_vars: &[],
        not_yet: &["SubagentStart", "SubagentStop", "PermissionRequest"],
        event_name: hook_event_name,
        // aimock's Responses usage has no token details (`CopilotKit/aimock`
        // `src/responses.ts:266-279` at v1.43.0).
        not_yet_fields: &[
            (
                "tx:event_msg",
                "/payload/info/last_token_usage/cached_input_tokens",
            ),
            (
                "tx:event_msg",
                "/payload/info/last_token_usage/reasoning_output_tokens",
            ),
        ],
    },
    // opencode's cites are at v2.0.28, the release that added the system
    // config below.
    Profile {
        source: "opencode",
        package: Package::Npm("@opencode/cli"),
        // A plain `run` attaches to the background service, whose env never
        // carries the run's PIXTUOID_SOCKET; a private server inherits it
        // (`packages/cli/src/services/standalone.ts:24-25`). Only an explicit
        // `--model` reaches the session's events (`packages/cli/src/run/run.ts:91`).
        invocation: &[
            "opencode",
            "run",
            "--standalone",
            "--model",
            "openai/gpt-5.2",
            "{prompt}",
        ],
        // The models.dev fetch and the update check
        // (`packages/cli/src/server-process.ts:113`, `services/updater.ts:534`).
        env: &[
            ("OPENCODE_DISABLE_MODELS_FETCH", "1"),
            ("OPENCODE_DISABLE_AUTOUPDATE", "1"),
        ],
        // On Windows its system config is under `env.ProgramData`
        // (`packages/core/src/config/discovery.ts:27-31`), so a sandboxed
        // ProgramData holds none.
        homes: &["OPENCODE_CONFIG_DIR", "ProgramData"],
        // System config and managed preferences
        // (`packages/core/src/config/discovery.ts:27-38`).
        system_config: PerOs {
            unix: &[
                HostConfig::Path("/Library/Application Support/opencode"),
                HostConfig::Path("/etc/opencode"),
                HostConfig::Path("/Library/Managed Preferences/ai.opencode.managed.plist"),
                HostConfig::Path("/Library/Managed Preferences/{user}/ai.opencode.managed.plist"),
            ],
            windows: &[],
        },
        // Project config up to `/` (`packages/core/src/config/discovery.ts:12`,
        // `:80`); its AGENTS.md walk stops at the git worktree
        // (`config/plugin/instruction.ts:37`), the workspace the recorder inits.
        ancestor_config: &[
            ".claude",
            ".agents",
            ".opencode",
            "opencode.jsonc",
            "opencode.json",
        ],
        config: &[ConfigFile {
            home: "OPENCODE_CONFIG_DIR",
            path: "opencode.jsonc",
            template: r#"{
  "providers": {
    "openai": {
      "settings": { "baseURL": "http://127.0.0.1:{port}/v1", "apiKey": "{key}" },
      "models": { "gpt-5.2": {} }
    }
  }
}
"#,
        }],
        scenarios: &[Scenario {
            shape: Shape::ToolRun,
            expects: &[
                "session.created",
                "session.execution.started",
                "session.tool.input.started",
                "session.tool.success",
                "session.execution.succeeded",
            ],
            fixture: "fixtures/opencode/tool-run-hermetic",
        }],
        script_vars: &[],
        not_yet: &[
            "session.deleted",
            "session.execution.failed",
            "session.execution.interrupted",
            "session.tool.failed",
            "permission.asked",
        ],
        event_name: type_field,
        // A child session's (tests/sources/opencode/fixtures).
        not_yet_fields: &[
            ("hook:session.created", "/data/parentID"),
            ("hook:session.execution.started", "/data/parentID"),
        ],
    },
    // dsh's cites are `deepseek-ai/deepseek-harness` at `dsh-v0.2.0-rc.2`.
    Profile {
        source: "dsh",
        package: Package::Npm("@deepseek-ai/dsh"),
        invocation: &["dsh", "--profile", "headless", "{prompt}"],
        // The Messages endpoint and key come from the launch environment
        // (`packages/llm/llm-deepseek/src/config.ts:290`,
        // `packages/llm/llm-deepseek-api-key/src/index.ts:20-35`); the feedback
        // upload's switch (`packages/bundle/base/cordis.patch.yml:193-195`).
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
        // (`packages/context/agent-instructions/README.md:36`), which the
        // recorder's `git init` makes the workspace.
        ancestor_config: &[],
        config: &[],
        scenarios: &[
            Scenario {
                shape: Shape::ToolRun,
                expects: &[
                    "agent/created",
                    "request/header",
                    "assistant/message",
                    "tool/call",
                    "tool/result",
                    "agent/disposed",
                ],
                fixture: "fixtures/dsh/tool-run-recorded",
            },
            Scenario {
                shape: Shape::Permission,
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
            },
            Scenario {
                shape: Shape::Subagent,
                expects: &[
                    "agent/created",
                    "request/header",
                    "assistant/message",
                    "tool/call",
                    "tool/result",
                    "agent/disposed",
                ],
                fixture: "delegation/fixtures/dsh",
            },
        ],
        // The shell tool win32 mounts in bash's place, which alone can
        // escalate there.
        script_vars: &[(
            SHELL_TOOL_PLACEHOLDER,
            PerOs {
                unix: "bash",
                windows: "pwsh",
            },
        )],
        not_yet: &[],
        event_name: dsh_event,
        not_yet_fields: &[],
    },
    // kimi's cites are `MoonshotAI/kimi-code` at `@moonshot-ai/kimi-code@2.0.1`.
    Profile {
        source: "kimi",
        package: Package::Npm("@moonshot-ai/kimi-code"),
        invocation: &["kimi", "-p", "{prompt}"],
        // `KIMI_MODEL_NAME` builds a provider from the environment
        // (`docs/en/configuration/env-vars.md:106-124`); the telemetry and
        // update-check switches (`:27`, `:183`).
        env: &[
            ("KIMI_MODEL_NAME", "kimi-k2"),
            ("KIMI_MODEL_BASE_URL", AIMOCK_V1),
            ("KIMI_MODEL_API_KEY", "{key}"),
            ("KIMI_DISABLE_TELEMETRY", "1"),
            ("KIMI_CODE_NO_AUTO_UPDATE", "1"),
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
        scenarios: &[Scenario {
            shape: Shape::ToolRun,
            expects: &["SessionStart", "PreToolUse", "PostToolUse", "Stop"],
            fixture: "fixtures/kimi/tool-run-hermetic",
        }],
        script_vars: &[],
        not_yet: &[
            "PostToolUseFailure",
            "PermissionRequest",
            "StopFailure",
            "SessionEnd",
        ],
        event_name: hook_event_name,
        // A Bash call's; the scripted turn reads a file.
        not_yet_fields: &[("hook:PreToolUse", "/tool_input/command")],
    },
    // grok's cites are `xai-org/grok-build` at `37949780c1`, the monorepo sync
    // nearest 1.0.25: the repo tags no release.
    Profile {
        source: "grok",
        package: Package::Npm("@xai-official/grok"),
        // Headless automation runs always-approve
        // (`crates/codegen/xai-grok-pager/docs/user-guide/22-permissions-and-safety.md:76-94`),
        // which a blocking spawn needs: `-p` cancels every prompt
        // (`crates/codegen/xai-grok-pager/src/headless.rs:1882-1901`).
        invocation: &["grok", "--always-approve", "-p", "{prompt}"],
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
                shape: Shape::ToolRun,
                expects: &[
                    "SessionStart",
                    "UserPromptSubmit",
                    "PreToolUse",
                    "PostToolUse",
                    "Stop",
                    "SessionEnd",
                ],
                fixture: "fixtures/grok/tool-run-hermetic",
            },
            Scenario {
                shape: Shape::Subagent,
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
            },
        ],
        script_vars: &[],
        not_yet: &[
            "PostToolUseFailure",
            "PermissionDenied",
            "Notification",
            "StopFailure",
            "StopCancelled",
            "SubagentEnd",
        ],
        event_name: grok_event,
        // A shell call's; the scripted turns read a file and spawn a child.
        not_yet_fields: &[("hook:PreToolUse", "/toolInput/command")],
    },
    // reasonix's cites are `esengine/DeepSeek-Reasonix` at `v2.39.0`.
    Profile {
        source: "reasonix",
        package: Package::Npm("reasonix"),
        // A non-read-only tool is denied headless; this allows the one a
        // blocking spawn needs (`docs/CLI.md:502-503`, `:517-519`).
        invocation: &["reasonix", "--allowed-tools", "task", "-p", "{prompt}"],
        // Off already for a non-interactive run, and pinned off against a
        // changed default (`internal/platform/telemetry/policy.go:44`).
        env: &[("REASONIX_TELEMETRY", "off")],
        homes: &["REASONIX_HOME"],
        system_config: PerOs {
            unix: &[],
            windows: &[],
        },
        // Instructions stop at the nearest git root
        // (`internal/state/instruction/resolver.go:80-84`), which the
        // recorder's `git init` makes the workspace; `./reasonix.toml` is the
        // workspace's own (`internal/contract/config/paths.go:639-649`).
        ancestor_config: &[],
        // A loopback `base_url` runs with no key
        // (`internal/contract/config/config.go:1651-1684`).
        config: &[ConfigFile {
            home: "REASONIX_HOME",
            path: "config.toml",
            template: r#"default_model = "aimock"

[[providers]]
name = "aimock"
kind = "openai"
base_url = "http://127.0.0.1:{port}/v1"
model = "deepseek-v4-flash"
"#,
        }],
        scenarios: &[
            Scenario {
                shape: Shape::ToolRun,
                expects: &[
                    "SessionStart",
                    "UserPromptSubmit",
                    "PreToolUse",
                    "PostToolUse",
                    "Stop",
                    "SessionEnd",
                ],
                fixture: "fixtures/reasonix/tool-run-hermetic",
            },
            Scenario {
                shape: Shape::Subagent,
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
                fixture: "reasonix/fixtures/subagent-hermetic",
            },
        ],
        script_vars: &[],
        not_yet: &["PermissionRequest", "Notification"],
        event_name: event_field,
        // A bash call's; the scripted turn reads a file.
        not_yet_fields: &[("hook:PreToolUse", "/toolArgs/command")],
    },
];

/// Every registered source without a profile, and why.
pub const NOT_HERMETIC: &[(&str, Absence)] = &[
    // Owner, 2026-10-13: no custom model endpoint, so no upstream detection.
    ("cursor", Absence::NoModelEndpoint),
    ("antigravity", Absence::AwaitingProfile),
    ("codewhale", Absence::AwaitingProfile),
    ("openclaw", Absence::AwaitingProfile),
    ("copilot", Absence::AwaitingProfile),
    ("hermes", Absence::AwaitingProfile),
    ("omp", Absence::AwaitingProfile),
];

/// The profile for `source`, if it has one.
pub fn profile(source: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.source == source)
}

/// Where each profile's scenario scripts live, as `<source>/<shape>.json`, and
/// the scripted model's npm lockfile, under `aimock/`.
pub fn scripts_dir() -> PathBuf {
    crate::harness::sources_root().join("hermetic")
}

impl Scenario {
    /// `source`'s script for this scenario.
    pub fn script(&self, source: &str) -> PathBuf {
        scripts_dir()
            .join(source)
            .join(format!("{}.json", self.shape.name()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    #[test]
    fn every_registered_source_is_profiled_or_says_why_not_and_never_both() {
        let registered: BTreeSet<&str> = registry::registered_source_names().collect();
        let profiled: BTreeSet<&str> = PROFILES.iter().map(|p| p.source).collect();
        let absent: BTreeSet<&str> = NOT_HERMETIC.iter().map(|(s, _)| *s).collect();
        assert_eq!(profiled.len(), PROFILES.len(), "a source profiled twice");
        assert_eq!(absent.len(), NOT_HERMETIC.len(), "a source listed twice");
        assert!(
            profiled.is_disjoint(&absent),
            "profiled AND listed absent: {:?}",
            profiled.intersection(&absent).collect::<Vec<_>>()
        );
        let covered: BTreeSet<&str> = profiled.union(&absent).copied().collect();
        assert_eq!(
            covered, registered,
            "the tier's population is the roster: profile each source or say why not"
        );
    }

    #[test]
    fn every_scenario_has_its_script_and_one_scenario_per_shape() {
        for p in PROFILES {
            let mut shapes = BTreeSet::new();
            for s in p.scenarios {
                assert!(
                    shapes.insert(s.shape.name()),
                    "{}: {} twice",
                    p.source,
                    s.shape.name()
                );
                let script = s.script(p.source);
                assert!(
                    script.is_file(),
                    "{}: no script at {}",
                    p.source,
                    script.display()
                );
                assert!(
                    !s.expects.is_empty(),
                    "{}: a scenario expecting nothing",
                    p.source
                );
            }
            assert!(
                !p.scenarios.is_empty(),
                "{}: a profile with no scenario",
                p.source
            );
        }
    }

    #[test]
    fn every_profile_runs_its_registered_binary() {
        for p in PROFILES {
            let probe = p
                .descriptor()
                .version_probe
                .expect("a profiled CLI has a version probe");
            assert_eq!(
                p.invocation.first(),
                probe.first(),
                "{}: the invocation runs a binary the registry does not name",
                p.source
            );
        }
    }

    #[test]
    fn a_config_file_lives_under_a_home_the_sandbox_owns() {
        for p in PROFILES {
            for c in p.config {
                let owned = p.home_vars().any(|h| h == c.home);
                assert!(owned, "{}: {} is not a sandboxed home", p.source, c.home);
            }
        }
    }

    #[test]
    fn the_drift_watchs_npm_release_installs_the_profiles_package() {
        use crate::source::upstream::Release;
        for (source, repo) in [
            ("opencode", crate::source::opencode::REPO),
            ("dsh", crate::source::dsh::REPO),
            ("reasonix", crate::source::reasonix::REPO),
        ] {
            let Release::Npm { package, .. } = repo.release else {
                panic!("{source}'s watch reads its npm release");
            };
            let p = profile(source).expect("profiled");
            assert_eq!(p.package, Package::Npm(package), "{source}");
        }
    }

    #[test]
    fn every_scenario_records_into_a_fixture_of_its_own() {
        let mut fixtures = BTreeSet::new();
        for p in PROFILES {
            for s in p.scenarios {
                assert!(fixtures.insert(s.fixture), "{} recorded twice", s.fixture);
                let prov = crate::harness::sources_root()
                    .join(s.fixture)
                    .join(crate::harness::PROVENANCE);
                let prov: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(&prov)
                        .unwrap_or_else(|e| panic!("{}: {}: {e}", p.source, prov.display())),
                )
                .expect("provenance is json");
                assert!(
                    prov.get(PROVENANCE_AIMOCK).is_some(),
                    "{0}: {1} holds no recording of the tier; `just hermetic --record {0}` writes one",
                    p.source,
                    s.fixture
                );
            }
        }
    }

    #[test]
    fn every_not_yet_field_is_one_a_decoder_reads() {
        for p in PROFILES {
            let reads = verify::committed_reads(p).expect("the committed corpus reads");
            for (kind, path) in p.not_yet_fields {
                assert!(
                    reads.has(kind, path),
                    "{}: no committed capture shows a decoder reading {kind} {path}",
                    p.source
                );
            }
        }
    }

    /// Lexical, on both arms, so a Unix host catches a misspelled Windows row:
    /// a misspelled placeholder or hive is checked as a place that never exists.
    #[test]
    fn config_outside_the_sandbox_is_spelled_as_its_reader_expands_it() {
        use crate::harness::host::KnownFolder;
        for p in PROFILES {
            for row in p.system_config.unix {
                let HostConfig::Path(path) = row else {
                    panic!("{}: {row:?} on Unix", p.source);
                };
                assert!(path.starts_with('/'), "{}: {path}", p.source);
                assert!(
                    !path.replace(USER_PLACEHOLDER, "").contains(['{', '}']),
                    "{}: {path} spells a placeholder other than {USER_PLACEHOLDER}",
                    p.source
                );
            }
            for row in p.system_config.windows {
                match row {
                    HostConfig::Path(path) => {
                        let rest = KnownFolder::ALL
                            .iter()
                            .find_map(|f| path.strip_prefix(f.placeholder()))
                            .or_else(|| path.get(2..).filter(|_| path.as_bytes()[1] == b':'))
                            .unwrap_or_else(|| panic!("{}: {path} is not absolute", p.source));
                        assert!(
                            rest.starts_with('\\') && !rest.contains(['{', '}', '/']),
                            "{}: {path}",
                            p.source
                        );
                    }
                    HostConfig::Registry(key) => assert!(
                        ["HKLM\\", "HKCU\\"].iter().any(|h| key.starts_with(h)),
                        "{}: {key} names no hive",
                        p.source
                    ),
                }
            }
            for name in p.ancestor_config {
                assert_eq!(
                    Path::new(name).components().count(),
                    1,
                    "{}: {name} is not one name",
                    p.source
                );
            }
        }
    }

    /// A script placeholder no profile declares reaches the CLI as a literal;
    /// one declared and never spelled is dead.
    #[test]
    fn every_script_placeholder_is_declared_and_spelled() {
        for p in PROFILES {
            let mut spelled = BTreeSet::new();
            for s in p.scenarios {
                let text = std::fs::read_to_string(s.script(p.source)).expect("script");
                let mut rest = text.as_str();
                while let Some(open) = rest.find('{') {
                    let tail = &rest[open..];
                    let close = tail.find('}').expect("a closed brace");
                    let word = &tail[..=close];
                    if word[1..word.len() - 1]
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_')
                        && word.len() > 2
                    {
                        spelled.insert(word.to_owned());
                    }
                    rest = &tail[1..];
                }
            }
            let declared: BTreeSet<String> = p
                .script_values("")
                .into_iter()
                .map(|(k, _)| k.to_owned())
                .collect();
            let undeclared: Vec<_> = spelled.difference(&declared).collect();
            assert!(undeclared.is_empty(), "{}: {undeclared:?}", p.source);
            for (k, _) in p.script_vars {
                assert!(spelled.contains(*k), "{}: {k} is never spelled", p.source);
            }
        }
    }

    #[test]
    fn opencodes_model_flag_names_a_model_its_config_declares() {
        let p = profile("opencode").expect("opencode");
        let model = p
            .invocation
            .windows(2)
            .find_map(|w| (w[0] == "--model").then_some(w[1]))
            .expect("a --model");
        let (provider, id) = model.split_once('/').expect("provider/model");
        let config: Value = serde_json::from_str(p.config[0].template).expect("plain JSON");
        assert!(
            config["providers"][provider]["models"].get(id).is_some(),
            "{model} is not under the config's providers.{provider}.models"
        );
    }

    #[test]
    fn a_hook_both_expected_and_not_yet_is_a_stale_entry() {
        for p in PROFILES {
            let expected: BTreeSet<&str> = p
                .scenarios
                .iter()
                .flat_map(|s| s.expects.iter().copied())
                .collect();
            for n in p.not_yet {
                assert!(
                    !expected.contains(n),
                    "{}: {n} is fired by a scenario, so it leaves not_yet",
                    p.source
                );
            }
        }
    }

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
            assert_eq!(grok_event(&v).as_deref(), Some(key));
        }
        assert_eq!(grok_event(&serde_json::json!({})), None);
    }

    #[test]
    fn a_tool_run_replies_only_to_the_note_it_read() {
        // A reply to any tool result lets a run whose read failed pass.
        for p in PROFILES {
            for s in p.scenarios.iter().filter(|s| s.shape == Shape::ToolRun) {
                let script: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(s.script(p.source)).expect("the script"),
                )
                .expect("json");
                let replies = script["fixtures"].as_array().expect("fixtures");
                assert!(
                    replies
                        .iter()
                        .any(|f| f["match"]["toolResultContains"] == NOTE_PLACEHOLDER),
                    "{}: no reply waits for {NOTE_PLACEHOLDER}",
                    p.source
                );
                assert!(
                    !replies.iter().any(|f| f["match"]["hasToolResult"] == true),
                    "{}: a reply takes any tool result",
                    p.source
                );
            }
        }
    }
}
