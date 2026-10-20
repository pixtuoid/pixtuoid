//! The hermetic tier's per-CLI facts: how each real CLI is installed, isolated,
//! and driven against a scripted model (aimock), with a daemon's live tier
//! beside it, and which hook events each scenario must fire. The registry already owns the binary (`version_probe`),
//! the home variable (`home_env`) and the pinned version (`verified_version`);
//! a profile holds only what it doesn't.

use std::borrow::Cow;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::CaptureRecord;
use crate::source::registry;

pub mod bump;
pub mod digest;
pub mod dropped;
mod literals;
pub mod pin;
pub mod tui;
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
/// What a script or a [`ConfigFile`] spells the workspace the recorder runs the
/// CLI in as, with its OS's own separators.
pub const WORKSPACE_PLACEHOLDER: &str = "{workspace}";
/// What a [`ConfigFile`] spells that workspace as with `/` separators, which a
/// JSON string holds unescaped: how Claude Code keys its projects on Windows
/// (`Xz` in the 2.1.302 bundle).
pub const WORKSPACE_SLASHED_PLACEHOLDER: &str = "{workspace_slashed}";
/// What a [`Profile::invocation`] or [`Profile::resume`] spells its turn's
/// prompt as.
pub const PROMPT_PLACEHOLDER: &str = "{prompt}";
/// What a [`Profile::resume`] spells the session its turn resumes as.
pub const SESSION_PLACEHOLDER: &str = "{session}";
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
    /// A prebuilt binary attached to a `v<version>` GitHub release.
    GithubRelease {
        /// Its repository's `owner/name` as GitHub spells it now: an old name
        /// redirects only until it is reused
        /// (docs.github.com/en/repositories/creating-and-managing-repositories/renaming-a-repository).
        repo: &'static str,
        /// Each platform's binary, as `(OS, ARCH, asset)` in
        /// [`std::env::consts`]' spelling.
        assets: &'static [(&'static str, &'static str, &'static str)],
        /// The release's file that must vouch for the download
        /// ([`digest::vouches`]).
        sums: &'static str,
    },
    /// The `v<version>` tag of the repository it names (spelled as
    /// [`GithubRelease`](Self::GithubRelease)'s `repo`), synced from its
    /// committed `uv.lock`.
    UvLocked(&'static str),
    /// The build an updater's per-platform manifest (`{"version", "url",
    /// "sha512"}`) names: the current one live, a pinned one from the lock
    /// `--record` writes ([`Profile::manifest_lock`]).
    UpdaterManifest {
        /// The manifest's URL, with `{platform}` filled for the host.
        manifest: &'static str,
        /// Each platform's name, as `(OS, ARCH, platform)` in
        /// [`std::env::consts`]' spelling.
        platforms: &'static [(&'static str, &'static str, &'static str)],
        /// The binary a `.tar.gz` build carries; any other build is the binary.
        member: &'static str,
    },
}

/// What a scenario's script drives a CLI through. Shared by every CLI, so one CLI
/// cannot be held to a weaker bar than another, save the gaps a profile's
/// `result_unseen` and its source's `ChildEndInference` owe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A turn that runs one tool: the session registers in the workspace, a
    /// tool starts with a detail naming it, and it ends.
    ToolRun,
    /// A turn whose tool call needs an approval the run cannot give: the slot
    /// waits on it, then the refused call ends, or its turn where the call
    /// never started.
    Permission,
    /// A turn that hands a file read to a child session: the parent delegates,
    /// the child registers as its child and reads, and both end.
    Subagent,
    /// A turn the model answers with an error a CLI does not retry (a 400): the
    /// session registers, and its turn ends failed.
    ModelError,
    /// A turn whose child's model errs: the parent delegates, the child
    /// registers as its child and ends, and the parent's turn, or its session, ends.
    ChildError,
    /// A command that starts the CLI's next session in place: the session it
    /// switches away from ends, and a root session succeeds it.
    Succession,
}

impl Shape {
    /// Whether a CLI that exited with `code` ran this shape's turn as scripted:
    /// one scripted to fail may exit non-zero, but a signal ends no turn.
    pub(crate) const fn exits_as_scripted(self, code: Option<i32>) -> bool {
        match code {
            Some(0) => true,
            Some(_) => match self {
                Self::ModelError => true,
                Self::ToolRun
                | Self::Permission
                | Self::Subagent
                | Self::ChildError
                | Self::Succession => false,
            },
            None => false,
        }
    }

    /// Whether its turn asks the scripted model anything: a succession's
    /// command runs in the CLI alone.
    pub(crate) const fn asks_the_model(self) -> bool {
        match self {
            Self::ToolRun
            | Self::Permission
            | Self::Subagent
            | Self::ModelError
            | Self::ChildError => true,
            Self::Succession => false,
        }
    }
}

/// Which of a CLI's surfaces a scenario's turn is driven through.
#[derive(Debug, Clone, Copy)]
pub enum Surface {
    /// The profile's headless invocation, with the prompt in its argv.
    Headless,
    /// Its interactive TUI under a pty, for a turn only the TUI asks.
    Tui {
        /// The CLI's interactive facts.
        row: &'static tui::Tui,
        /// How the run answers the turn's dialog.
        answer: tui::Answer,
    },
}

/// A CLI's scripted turn, and the turn resuming it where [`Turn::resume`]
/// names one.
#[derive(Debug)]
pub struct Scenario {
    /// Its name, unique in its profile: its script's file stem, its run
    /// directory, and what `just hermetic <source> <name>` selects.
    pub name: &'static str,
    /// What the run must look like.
    pub shape: Shape,
    /// The surface the turn is driven through.
    pub surface: Surface,
    /// The hook events this run must fire, as `Profile::event_name` reads them.
    pub expects: &'static [&'static str],
    /// The committed recording `--record` rewrites, under `tests/sources`: the
    /// tier's own, beside any capture still carrying a field the profile owes.
    pub fixture: &'static str,
    /// What its turn asks of the CLI beyond the profile's invocation.
    pub turn: Turn,
}

/// A scenario's own part of its turn.
#[derive(Debug)]
pub struct Turn {
    /// Appended to the profile's headless invocation.
    pub args: &'static [&'static str],
    /// Its prompt, in place of the recorder's shared one.
    pub prompt: Option<&'static str>,
    /// The prompt of a second headless turn, resuming this one's session in
    /// the same sandbox and workspace once its CLI exited.
    pub resume: Option<&'static str>,
}

impl Turn {
    /// The profile's invocation as it stands.
    pub const PROFILE: Self = Self {
        args: &[],
        prompt: None,
        resume: None,
    };
}

/// A daemon's own live tier, driven against this build.
#[derive(Debug)]
pub struct Live {
    /// The tier, from the repository root. It takes its ports as arguments,
    /// and exits [`LIVE_PREREQUISITE`] when something it needs is missing.
    pub script: &'static str,
    /// How many free ports the orchestrator passes it.
    pub ports: usize,
}

impl Live {
    /// Why a leg on `windows` doesn't run it, if it doesn't.
    pub fn not_run_on(&self, windows: bool) -> Option<&'static str> {
        windows.then_some("its tier waits on a Unix socket")
    }
}

/// A server a profile's CLI talks to, run inside each scenario's recorded run.
/// It must stop when its stdin closes, the one stop the recorder has on every
/// OS: one that outlives it is killed, and its capture fails.
#[derive(Debug)]
pub struct Daemon {
    /// Its start, after the CLI's binary.
    pub start: &'static [&'static str],
    /// What exits 0 once it serves, after the CLI's binary.
    pub ready: &'static [&'static str],
    /// Environment it alone gets, beside the profile's.
    pub env: &'static [(&'static str, &'static str)],
}

/// The port a [`Daemon`] listens on, picked free by the orchestrator.
pub const DAEMON_PORT_PLACEHOLDER: &str = "{daemon_port}";

/// A live tier's exit when a prerequisite is missing, which no run of the build
/// under test caused.
pub const LIVE_PREREQUISITE: i32 = 2;

/// What a live tier reads as the build under test.
pub const LIVE_PIXTUOID_ENV: &str = "PIXTUOID_BIN";

/// A config file the tier writes into a sandbox home BEFORE `pixtuoid connect`,
/// which may merge its own into the same file (codex's `config.toml`).
#[derive(Debug)]
pub struct ConfigFile {
    /// The environment variable naming the home the file lives under.
    pub home: &'static str,
    /// The file's path under that home.
    pub path: &'static str,
    /// Its contents; `{port}`, `{key}`, [`DAEMON_PORT_PLACEHOLDER`],
    /// [`WORKSPACE_PLACEHOLDER`] and [`WORKSPACE_SLASHED_PLACEHOLDER`] expand.
    pub template: &'static str,
}

/// One CLI's hermetic facts.
#[derive(Debug)]
pub struct Profile {
    /// The registered source id.
    pub source: &'static str,
    /// How users install it.
    pub package: Package,
    /// The headless invocation: [`PROMPT_PLACEHOLDER`] expands to the
    /// scenario's prompt in the runner, else to the recorder's shared one.
    pub invocation: &'static [&'static str],
    /// The headless invocation that resumes a session, as [`Turn::resume`] runs
    /// it: the runner expands [`PROMPT_PLACEHOLDER`], the recorder
    /// [`SESSION_PLACEHOLDER`].
    pub resume: Option<&'static [&'static str]>,
    /// Environment for the CLI: endpoint, dummy key and opt-outs; `{port}`,
    /// `{key}` and [`DAEMON_PORT_PLACEHOLDER`] expand.
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
    /// Hooks its install forwards that no scenario expects yet. Shrink-only: a
    /// run that fires one is Stale (`verify::verify_source`).
    pub not_yet: &'static [&'static str],
    /// The hook event a captured payload is, as its install names it.
    pub(crate) event_name: fn(&Value) -> Option<Cow<'_, str>>,
    /// Fields a decoder reads in a committed capture that no scenario carries
    /// yet, as `(kind, path)` the field census names them. Shrink-only: a run
    /// that carries one is Stale.
    pub(crate) not_yet_fields: &'static [(&'static str, &'static str)],
    /// Literals a decoder reads that no scenario fixture of this profile holds
    /// yet and no other owed list names ([`literals`]). Shrink-only: a fixture
    /// that holds one, or a run that would keep one once stripped, leaves it.
    pub(crate) not_yet_literals: &'static [&'static str],
    /// Whether a child may name no cwd of its own, running where its parent
    /// does.
    pub(crate) children_inherit_cwd: bool,
    /// Whether aimock never sees this CLI's tool results, so its tool-run reply
    /// waits on the turn rather than the note. Shrink-only: a run in which
    /// aimock saw one is Stale.
    pub(crate) result_unseen: bool,
    /// Its live tier, run beside `scenarios`.
    pub live: Option<Live>,
    /// The server its CLI talks to.
    pub daemon: Option<Daemon>,
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

    /// Its scenario named `name`.
    pub fn scenario(&self, name: &str) -> Option<&'static Scenario> {
        self.scenarios.iter().find(|s| s.name == name)
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
/// The pty log of a [`Surface::Tui`] run: the CLI's raw output and the
/// driver's every action.
pub const TUI_LOG: &str = "tui.log";
/// The file beside a run's report counting its New findings, which
/// `hermetic.yml` reads to upload a run that passed with one.
pub const NEW_TYPES: &str = "new-types.json";

/// The name an install lifecycle run is selected and reported by.
pub const LIFECYCLE: &str = "lifecycle";
/// An install lifecycle's [`StepRecord`]s, beside its silent turn's capture.
pub const LIFECYCLE_RECORD: &str = "lifecycle.json";

/// One `pixtuoid <command> <source> --json` an install lifecycle runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// Install the hooks, or install them again.
    Connect,
    /// Uninstall them.
    Disconnect,
}

impl Step {
    /// Its subcommand.
    pub fn command(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Disconnect => "disconnect",
        }
    }

    /// The `outcome` its row must report, a token of the `--json` contract
    /// (`integrations/raycast/contract/outcome-row.schema.json`).
    pub fn outcome(self) -> &'static str {
        match self {
            Self::Connect => "connected",
            Self::Disconnect => "disconnected",
        }
    }
}

/// An install lifecycle, in order, after a scenario's own connect and turn: a
/// repeat connect (reasonix's `--replace`), a disconnect, a connect over its
/// residue, and the disconnect a last turn runs under, which must reach the tap
/// with no hook.
pub const STEPS: &[Step] = &[
    Step::Connect,
    Step::Disconnect,
    Step::Connect,
    Step::Disconnect,
];

/// What one [`Step`] reported, written by the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    /// The step it ran.
    pub step: Step,
    /// Its row's `outcome` for the source; `None` when it printed none.
    pub outcome: Option<String>,
    /// Its row's `message`, else its stderr.
    pub message: Option<String>,
    /// After a connect, what `sources --json` finds wrong with the install:
    /// its `health`, or no connected row; `None` for a sound one.
    pub health: Option<String>,
}

/// What one run was, written by the orchestrator beside the run's raw capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// The registered source id.
    pub source: String,
    /// The scenario's name.
    pub scenario: String,
    /// The recorder's record of the CLI's run.
    pub capture: CaptureRecord,
    /// The aimock version that played the model.
    pub aimock: String,
    /// Requests aimock answered, every GET aside.
    pub requests: usize,
    /// Of those, the requests no script line matched.
    pub unmatched: usize,
    /// Of those, the requests carrying a tool result as aimock read them.
    pub results_seen: usize,
}

/// Why a registered source has no profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Absence {
    /// Its model endpoint cannot be redirected, so no scripted turn can run.
    NoModelEndpoint,
    /// Its headless run fires no session lifecycle, so no headless shape can
    /// pass.
    NoHeadlessLifecycle,
}

fn hook_event_name(v: &Value) -> Option<Cow<'_, str>> {
    v.get("hook_event_name")?.as_str().map(Cow::Borrowed)
}

fn shim_stamped_event(v: &Value) -> Option<Cow<'_, str>> {
    v.get(crate::source::decoder::EVENT_STAMP)?
        .as_str()
        .map(Cow::Borrowed)
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
/// spells each key in snake_case
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

/// Claude Code's TUI. Manual mode asks before a non-read-only Bash call
/// (code.claude.com/docs/en/permission-modes).
const CLAUDE_CODE_TUI: tui::Tui = tui::Tui {
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

/// grok's TUI. Its default mode asks before a shell command off the read-only
/// list (`crates/codegen/xai-grok-pager/docs/user-guide/22-permissions-and-safety.md:140-175`).
const GROK_TUI: tui::Tui = tui::Tui {
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

/// hermes's classic REPL, where a dangerous terminal command waits on the
/// user.
const HERMES_TUI: tui::Tui = tui::Tui {
    args: &["--accept-hooks", "--cli", "chat"],
    composer: tui::Composer {
        marker: "❯",
        above: tui::Above::Rule {
            glyph: '─',
            corners: None,
        },
    },
    gates: &[tui::Gate {
        shows: "Dangerous Command",
        // A digit picks its choice, "4. Deny" here
        // (`hermes_cli/cli_tui_mixin.py:1301-1307`, `:2066-2068`).
        deny: &[tui::Key::Text("4")],
        dwell: std::time::Duration::ZERO,
    }],
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    renders_under_conpty: true,
};

/// codex's TUI, its cites at `rust-v0.163.0`. On-request lets the model ask
/// (`codex-rs/protocol/src/protocol.rs:1099-1102`), and the script's call asks
/// to run unsandboxed (`core/src/tools/handlers/shell_spec.rs:239-243`).
const CODEX_TUI: tui::Tui = tui::Tui {
    args: &[
        "--dangerously-bypass-hook-trust",
        "--ask-for-approval",
        "on-request",
    ],
    composer: tui::Composer {
        marker: "›",
        above: tui::Above::Nothing,
    },
    gates: &[tui::Gate {
        shows: "Would you like to run the following command?",
        // "No, and tell Codex what to do differently", which aborts the turn
        // (`tui/src/bottom_pane/approval_overlay.rs:911-915`).
        deny: &[tui::Key::Esc],
        // It loses a key sent the moment it shows (measured; the cause in
        // codex is unverified).
        dwell: std::time::Duration::from_millis(500),
    }],
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    renders_under_conpty: true,
};

/// kimi's TUI, its cites at `@moonshot-ai/kimi-code@2.2.0`. Always Ask, its
/// default mode, asks before a command (`docs/en/guides/interaction.md:67`);
/// `-p` never asks (`docs/en/reference/kimi-command.md:43`).
const KIMI_TUI: tui::Tui = tui::Tui {
    args: &[],
    composer: tui::Composer {
        marker: " │ >",
        above: tui::Above::Rule {
            glyph: '─',
            corners: Some(('╭', '╮')),
        },
    },
    gates: &[tui::Gate {
        shows: "Run this command?",
        // Esc rejects the call outright
        // (`apps/kimi-code/src/tui/components/dialogs/approval-panel.ts:262-268`).
        deny: &[tui::Key::Esc],
        dwell: std::time::Duration::ZERO,
    }],
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    renders_under_conpty: true,
};

/// reasonix's TUI, its cites at `v2.44.0`. A named mode skips the folder-trust
/// question, whose yes would run commands unasked
/// (`internal/frontend/cli/tui_posture.go:32-42`, `:69`).
const REASONIX_TUI: tui::Tui = tui::Tui {
    args: &["--permission-mode", "ask"],
    composer: tui::Composer {
        marker: " ❯",
        above: tui::Above::Rule {
            glyph: '─',
            corners: None,
        },
    },
    gates: &[tui::Gate {
        shows: "Permission required",
        // "n/Esc deny" (`internal/base/i18n/messages_en.go:164`).
        deny: &[tui::Key::Esc],
        dwell: std::time::Duration::ZERO,
    }],
    // `/exit` is its alias (`internal/frontend/cli/slash_registry.go:69`).
    quit: &[tui::Key::Text("/quit"), tui::Key::Enter],
    renders_under_conpty: true,
};

/// omp's TUI, cited as its profile is.
const OMP_TUI: tui::Tui = tui::Tui {
    args: &[],
    composer: tui::Composer {
        marker: "╰─",
        above: tui::Above::Nothing,
    },
    gates: &[],
    // `src/slash-commands/builtin-lifecycle.ts:835-839`.
    quit: &[tui::Key::Text("/exit"), tui::Key::Enter],
    renders_under_conpty: true,
};

/// The profiled CLIs.
pub const PROFILES: &[Profile] = &[
    Profile {
        source: "claude-code",
        package: Package::Npm("@anthropic-ai/claude-code"),
        invocation: &["claude", "-p", PROMPT_PLACEHOLDER],
        // `-r, --resume [value]`: "Resume a conversation by session ID"
        // (`claude --help` at 2.1.302).
        resume: Some(&[
            "claude",
            "-p",
            PROMPT_PLACEHOLDER,
            "--resume",
            SESSION_PLACEHOLDER,
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
                    resume: Some("Say done."),
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
                    row: &CLAUDE_CODE_TUI,
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
            PROMPT_PLACEHOLDER,
        ],
        resume: None,
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
            template: r#"# Cites at codex `rust-v0.163.0`.
model = "gpt-5.5"
model_provider = "aimock"
model_reasoning_effort = "high"
# A trust decision would make the sandbox workspace-write
# (`codex-rs/config/src/config_toml.rs:855-861`).
sandbox_mode = "read-only"

[model_providers.aimock]
name = "aimock"
base_url = "http://127.0.0.1:{port}/v1"
env_key = "HERMETIC_API_KEY"
wire_api = "responses"

[windows]
sandbox = "unelevated"

# The TUI asks to trust a folder no entry names, matched on the cwd's own
# text (`config/src/loader/mod.rs:1416-1437`).
[projects.'{workspace}']
trust_level = "trusted"

# The TUI offers the catalog's upgrade from this model unless it was seen
# (`tui/src/app/startup_prompts.rs:165-168`, `models-manager/models.json:1401`).
[notice.model_migrations]
"gpt-5.5" = "gpt-6-sol"
"#,
        }],
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
                fixture: "fixtures/codex/tool-run-hermetic",
                turn: Turn::PROFILE,
            },
            Scenario {
                name: "custom-tool",
                shape: Shape::ToolRun,
                surface: Surface::Headless,
                // The read-only sandbox rejects the patch, so no PostToolUse
                // fires and the transcript's `custom_tool_call_output` ends it.
                expects: &[
                    "SessionStart",
                    "UserPromptSubmit",
                    "PreToolUse",
                    "Stop",
                    "SessionEnd",
                ],
                fixture: "fixtures/codex/custom-tool-hermetic",
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
                    "PostToolUse",
                    "SubagentStart",
                    "SubagentStop",
                    "Stop",
                    "SessionEnd",
                ],
                fixture: "codex/fixtures/subagent-hermetic",
                turn: Turn::PROFILE,
            },
            Scenario {
                name: "permission",
                shape: Shape::Permission,
                surface: Surface::Tui {
                    row: &CODEX_TUI,
                    answer: tui::Answer::Deny,
                },
                expects: &[
                    "SessionStart",
                    "UserPromptSubmit",
                    "PreToolUse",
                    "PermissionRequest",
                    "SessionEnd",
                ],
                fixture: "fixtures/codex/permission-hermetic",
                turn: Turn::PROFILE,
            },
        ],
        script_vars: &[],
        not_yet: &[],
        event_name: hook_event_name,
        // aimock's Responses usage has no token details (`CopilotKit/aimock`
        // `src/responses.ts:543-556` at v1.45.1).
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
        not_yet_literals: &[],
        children_inherit_cwd: false,
        result_unseen: false,
        live: None,
        daemon: None,
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
            PROMPT_PLACEHOLDER,
        ],
        resume: None,
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
        // A shell command asks, which `opencode run` rejects
        // (`services/www/src/docs/content/permissions.mdx:9-20`,
        // `packages/cli/src/run/noninteractive.ts:158-181`): the permission turn.
        config: &[ConfigFile {
            home: "OPENCODE_CONFIG_DIR",
            path: "opencode.jsonc",
            template: r#"{
  "providers": {
    "openai": {
      "settings": { "baseURL": "http://127.0.0.1:{port}/v1", "apiKey": "{key}" },
      "models": { "gpt-5.2": {} }
    }
  },
  "permissions": [{ "action": "shell", "resource": "*", "effect": "ask" }]
}
"#,
        }],
        scenarios: &[
            Scenario {
                name: "tool-run",
                shape: Shape::ToolRun,
                surface: Surface::Headless,
                expects: &[
                    "session.created",
                    "session.execution.started",
                    "session.tool.input.started",
                    "session.tool.success",
                    "session.execution.succeeded",
                ],
                fixture: "fixtures/opencode/tool-run-hermetic",
                turn: Turn::PROFILE,
            },
            Scenario {
                name: "subagent",
                shape: Shape::Subagent,
                surface: Surface::Headless,
                expects: &[
                    "session.created",
                    "session.execution.started",
                    "session.tool.input.started",
                    "session.tool.success",
                    "session.execution.succeeded",
                ],
                fixture: "opencode/fixtures/subagent-hermetic",
                turn: Turn::PROFILE,
            },
            Scenario {
                name: "model-error",
                shape: Shape::ModelError,
                surface: Surface::Headless,
                expects: &[
                    "session.created",
                    "session.execution.started",
                    "session.execution.failed",
                ],
                fixture: "fixtures/opencode/model-error-hermetic",
                turn: Turn::PROFILE,
            },
            Scenario {
                name: "permission",
                shape: Shape::Permission,
                surface: Surface::Headless,
                expects: &[
                    "session.created",
                    "session.execution.started",
                    "session.tool.input.started",
                    "permission.asked",
                    "session.tool.failed",
                    "session.execution.succeeded",
                ],
                fixture: "fixtures/opencode/permission-hermetic",
                turn: Turn::PROFILE,
            },
        ],
        script_vars: &[],
        not_yet: &["session.deleted", "session.execution.interrupted"],
        event_name: type_field,
        not_yet_fields: &[],
        not_yet_literals: &[],
        children_inherit_cwd: false,
        result_unseen: false,
        live: None,
        daemon: None,
    },
    // dsh's cites are `deepseek-ai/deepseek-harness` at `dsh-v0.2.0-rc.2`.
    Profile {
        source: "dsh",
        package: Package::Npm("@deepseek-ai/dsh"),
        invocation: &["dsh", "--profile", "headless", PROMPT_PLACEHOLDER],
        resume: None,
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
            SHELL_TOOL_PLACEHOLDER,
            PerOs {
                unix: "bash",
                windows: "pwsh",
            },
        )],
        not_yet: &[],
        event_name: dsh_event,
        not_yet_fields: &[],
        not_yet_literals: &[],
        children_inherit_cwd: false,
        result_unseen: false,
        live: None,
        daemon: None,
    },
    // kimi's cites are `MoonshotAI/kimi-code` at `@moonshot-ai/kimi-code@2.2.0`.
    Profile {
        source: "kimi",
        package: Package::Npm("@moonshot-ai/kimi-code"),
        invocation: &["kimi", "-p", PROMPT_PLACEHOLDER],
        resume: None,
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
                surface: Surface::Tui {
                    row: &KIMI_TUI,
                    answer: tui::Answer::Deny,
                },
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
    },
    // grok's cites are `xai-org/grok-build` at `37949780c1`, the monorepo sync
    // nearest 1.0.25 (the repo tags no release), unless a cite names its own ref.
    Profile {
        source: "grok",
        package: Package::Npm("@xai-official/grok"),
        // Headless automation runs always-approve
        // (`crates/codegen/xai-grok-pager/docs/user-guide/22-permissions-and-safety.md:76-94`),
        // which a blocking spawn needs: `-p` cancels every prompt
        // (`crates/codegen/xai-grok-pager/src/headless.rs:1882-1901`).
        invocation: &["grok", "--always-approve", "-p", PROMPT_PLACEHOLDER],
        resume: None,
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
                    row: &GROK_TUI,
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
        event_name: grok_event,
        not_yet_fields: &[],
        not_yet_literals: &[],
        children_inherit_cwd: false,
        result_unseen: false,
        live: None,
        daemon: None,
    },
    // reasonix's cites are `esengine/DeepSeek-Reasonix` at `v2.44.0`.
    Profile {
        source: "reasonix",
        package: Package::Npm("reasonix"),
        // A non-read-only tool is denied headless; this allows the ones the
        // spawns need (`docs/CLI.md:502-503`, `:517-519`).
        invocation: &[
            "reasonix",
            "--allowed-tools",
            "task,explore",
            "-p",
            PROMPT_PLACEHOLDER,
        ],
        resume: None,
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
            // A role with the executor's own model builds no planner
            // (`internal/assembly/boot/roles.go:56`), so each has a model of
            // its own for a script to match on.
            template: r#"default_model = "aimock"

[agent]
planner_model = "planner"
guardian_model = "guardian"

[[providers]]
name = "aimock"
kind = "openai"
base_url = "http://127.0.0.1:{port}/v1"
model = "deepseek-v4-flash"

[[providers]]
name = "planner"
kind = "openai"
base_url = "http://127.0.0.1:{port}/v1"
model = "planner-model"

[[providers]]
name = "guardian"
kind = "openai"
base_url = "http://127.0.0.1:{port}/v1"
model = "guardian-model"
"#,
        }],
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
                fixture: "fixtures/reasonix/tool-run-hermetic",
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
                fixture: "reasonix/fixtures/subagent-hermetic",
                turn: Turn::PROFILE,
            },
            // A skill tool's child fires no SubagentStart or SubagentStop.
            Scenario {
                name: "explore",
                shape: Shape::Subagent,
                surface: Surface::Headless,
                expects: &[
                    "SessionStart",
                    "UserPromptSubmit",
                    "PreToolUse",
                    "PostToolUse",
                    "Stop",
                    "SessionEnd",
                ],
                fixture: "reasonix/fixtures/explore-hermetic",
                turn: Turn::PROFILE,
            },
            // The marker routes the turn through the planner
            // (`internal/session/control/planner_gate.go:91-92`).
            Scenario {
                name: "planner",
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
                fixture: "fixtures/reasonix/planner-hermetic",
                turn: Turn {
                    args: &[],
                    prompt: Some("[Reasonix: plan-first] Read NOTE.txt."),
                    resume: None,
                },
            },
            Scenario {
                name: "bash-run",
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
                fixture: "fixtures/reasonix/bash-run-hermetic",
                turn: Turn::PROFILE,
            },
            Scenario {
                name: "permission",
                shape: Shape::Permission,
                surface: Surface::Tui {
                    row: &REASONIX_TUI,
                    answer: tui::Answer::Deny,
                },
                expects: &[
                    "SessionStart",
                    "UserPromptSubmit",
                    // The guardian's read: the refused bash fires no PreToolUse.
                    "PreToolUse",
                    "PostToolUse",
                    "PermissionRequest",
                    "Notification",
                    "Stop",
                    "SessionEnd",
                ],
                fixture: "fixtures/reasonix/permission-hermetic",
                turn: Turn::PROFILE,
            },
        ],
        script_vars: &[],
        not_yet: &[],
        event_name: event_field,
        not_yet_fields: &[],
        not_yet_literals: &[],
        children_inherit_cwd: false,
        result_unseen: false,
        live: None,
        daemon: None,
    },
    // copilot is closed source: its cites are docs.github.com,
    // `github/copilot-cli` and the `@github/copilot-<platform>@1.0.98` binaries.
    Profile {
        source: "copilot",
        package: Package::Npm("@github/copilot"),
        invocation: &["copilot", "-p", PROMPT_PLACEHOLDER],
        // `-r, --resume [<value>]`: "optionally specify existing session ID"
        // (`copilot --help` at 1.0.98).
        resume: Some(&[
            "copilot",
            "--resume",
            SESSION_PLACEHOLDER,
            "-p",
            PROMPT_PLACEHOLDER,
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
                HostConfig::Path(
                    "/Library/Application Support/GitHubCopilot/managed-settings.json",
                ),
                HostConfig::Path("/etc/github-copilot/managed-settings.json"),
                HostConfig::Path("/etc/github-copilot/policy.d"),
            ],
            windows: &[
                HostConfig::Registry(r"HKLM\SOFTWARE\Policies\GitHubCopilot"),
                HostConfig::Registry(r"HKLM\Software\Policies\GitHub\Copilot"),
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
                    resume: Some("Say done."),
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
            SHELL_TOOL_PLACEHOLDER,
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
    },
    // omp's cites are `can1357/oh-my-pi` at `v18.12.1`, `src/` its
    // `packages/coding-agent/src/`.
    Profile {
        source: "omp",
        // The release's own binaries: the npm package runs only under Bun (its
        // `engines`), and `scripts/install.sh` fetches from build.stencil.so
        // and runs `omp --version` under the operator's home (`:21`, `:331`).
        package: Package::GithubRelease {
            repo: "can1357/oh-my-pi",
            // `scripts/ci-build-upload.ts:39-48`, glibc's on Linux.
            assets: &[
                ("macos", "aarch64", "omp-darwin-arm64"),
                ("macos", "x86_64", "omp-darwin-x64"),
                ("linux", "aarch64", "omp-linux-arm64"),
                ("linux", "x86_64", "omp-linux-x64"),
                ("windows", "aarch64", "omp-windows-arm64.exe"),
                ("windows", "x86_64", "omp-windows-x64.exe"),
            ],
            sums: "SHA256SUMS.txt",
        },
        invocation: &["omp", "-p", "--model", "aimock/gpt-4.1", PROMPT_PLACEHOLDER],
        resume: None,
        // The auto-QA report posts to qa.omp.sh (`src/tools/settings.ts:962-978`).
        env: &[("PI_AUTO_QA", "0")],
        homes: &[],
        system_config: PerOs {
            unix: &[],
            windows: &[],
        },
        // Context files stop at the git root when the workspace is outside the
        // home (`src/discovery/helpers.ts:721-735`), and the recorder's
        // `git init` makes the workspace that root; Cline's rules don't stop
        // (`src/discovery/cline.ts:19-35`).
        ancestor_config: &[".clinerules"],
        config: &[
            ConfigFile {
                home: "PI_CODING_AGENT_DIR",
                path: "models.yml",
                template: r#"providers:
  aimock:
    baseUrl: http://127.0.0.1:{port}/v1
    apiKey: "{key}"
    api: openai-completions
    models:
      - id: gpt-4.1
        reasoning: true
"#,
            },
            // A headless prompt is refused, and `always-ask` prompts for every
            // tool but a read (`docs/approval-mode.md:18-24`), so only the spawn
            // is allowed. `-p` exits once the turn ends, killing a background
            // child, so the spawn blocks (`src/task/settings.ts:251`). The TUI
            // would open on a setup wizard and fetch the latest release
            // (`src/main.ts:240-254`, `:668-679`).
            ConfigFile {
                home: "PI_CODING_AGENT_DIR",
                path: "config.yml",
                template: r#"tools:
  approvalMode: always-ask
  approval:
    task: allow
async:
  enabled: false
startup:
  setupWizard: false
  checkUpdate: false
"#,
            },
        ],
        scenarios: &[
            Scenario {
                name: "tool-run",
                shape: Shape::ToolRun,
                surface: Surface::Headless,
                expects: &["session_start", "session_shutdown"],
                fixture: "omp/fixtures/tool-run-hermetic",
                turn: Turn::PROFILE,
            },
            Scenario {
                name: "permission",
                shape: Shape::Permission,
                surface: Surface::Headless,
                expects: &[
                    "session_start",
                    "tool_approval_requested",
                    "tool_approval_resolved",
                    "session_shutdown",
                ],
                fixture: "omp/fixtures/permission-hermetic",
                turn: Turn::PROFILE,
            },
            Scenario {
                name: "subagent",
                shape: Shape::Subagent,
                surface: Surface::Headless,
                expects: &["session_start", "session_shutdown"],
                fixture: "delegation/fixtures/omp",
                turn: Turn::PROFILE,
            },
            // `/new` is a TUI builtin, which `-p` never runs
            // (`src/slash-commands/builtin-lifecycle.ts:159-167`,
            // `src/session/agent-session.ts:7574-7596`); its switch names the
            // session it leaves (`:10250-10255`).
            Scenario {
                name: "new-session",
                shape: Shape::Succession,
                surface: Surface::Tui {
                    row: &OMP_TUI,
                    answer: tui::Answer::Nothing,
                },
                expects: &["session_start", "session_switch", "session_shutdown"],
                fixture: "omp/fixtures/new-session-hermetic",
                turn: Turn {
                    args: &[],
                    prompt: Some("/new"),
                    resume: None,
                },
            },
        ],
        script_vars: &[],
        not_yet: &["session_branch"],
        event_name: type_field,
        not_yet_fields: &[
            // Only a UI approves a prompt (`src/extensibility/extensions/wrapper.ts:386-398`).
            ("hook:tool_approval_resolved", "/approved"),
            // Only an interactive run titles a session (`src/main.ts:2258`).
            ("tx:title", "/title"),
            ("tx:title_change", "/title"),
            ("tx:title_change", "/type"),
            // An `ask` round, which no shape scripts.
            (
                "tx:message",
                "/message/content/*/arguments/questions/*/question",
            ),
        ],
        not_yet_literals: &["ask"],
        children_inherit_cwd: false,
        result_unseen: false,
        live: None,
        daemon: None,
    },
    // hermes's cites are `NousResearch/hermes-agent` at `v0.22.2`.
    Profile {
        source: "hermes",
        // Its build refuses a wheel (`setup.py:37`), so no index carries a
        // current release; a source checkout tracks the latest `vX.Y.Z` tag
        // (`website/docs/getting-started/updating.md:33-36`). Its installer
        // would rewrite the operator's shell rc files and run `sudo -n -v`
        // (`scripts/install.sh:851-853`, `:882-908`).
        package: Package::UvLocked("NousResearch/hermes-agent"),
        // `-z` is one turn that approves every tool (`hermes_cli/oneshot.py:275-277`),
        // and a hook runs only once accepted.
        invocation: &["hermes", "--accept-hooks", "-z", PROMPT_PLACEHOLDER],
        resume: None,
        env: &[],
        // The managed scope, which otherwise is `/etc/hermes`
        // (`hermes_cli/managed_scope.py:31`, `:45-58`).
        homes: &["HERMES_MANAGED_DIR"],
        // Read there whatever the managed scope is (`hermes_cli/image_provenance.py:16`).
        system_config: PerOs {
            unix: &[HostConfig::Path("/etc/hermes")],
            windows: &[],
        },
        // Context files stop at the git root (`agent/prompt_builder.py:141`,
        // `:1722-1732`), which the recorder's `git init` makes the workspace.
        ancestor_config: &[],
        // `manual` approvals ask the user where the default asks an LLM guardian
        // (`tests/e2e/core/security/test_approval_boundary.py:17`), and an answered
        // shared-metrics offer, one at upstream's `OFFER_VERSION`, never opens before
        // a chat (`hermes_cli/observability/shared_metrics_consent.py:55`, `:58-72`).
        config: &[ConfigFile {
            home: "HERMES_HOME",
            path: "config.yaml",
            template: r#"model:
  default: hermetic
  provider: custom
  base_url: http://127.0.0.1:{port}/v1
  api_key: "{key}"
approvals:
  mode: manual
telemetry:
  shared_metrics:
    enabled: false
    offer_version: 2
"#,
        }],
        scenarios: &[
            Scenario {
                name: "tool-run",
                shape: Shape::ToolRun,
                surface: Surface::Headless,
                expects: &[
                    "on_session_start",
                    "pre_tool_call",
                    "post_tool_call",
                    "on_session_end",
                    "on_session_finalize",
                ],
                fixture: "fixtures/hermes/tool-run-hermetic",
                turn: Turn::PROFILE,
            },
            Scenario {
                name: "permission",
                shape: Shape::Permission,
                surface: Surface::Tui {
                    row: &HERMES_TUI,
                    answer: tui::Answer::Deny,
                },
                expects: &[
                    "on_session_start",
                    "pre_tool_call",
                    "pre_approval_request",
                    "post_tool_call",
                    "on_session_end",
                    "on_session_finalize",
                ],
                fixture: "fixtures/hermes/permission-hermetic",
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
    },
    // agy is closed source: its cites are its installer,
    // antigravity.google/cli/install.sh, and the agy 1.4.2 binary.
    Profile {
        source: "antigravity",
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
        invocation: &["agy", "-p", PROMPT_PLACEHOLDER],
        resume: None,
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
            // step %d with skipped=true" (`ask_question_manager.go:30`).
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
    },
    // openclaw's cites are `openclaw/openclaw` at `v2026.10.1`.
    Profile {
        source: "openclaw",
        package: Package::Npm("openclaw"),
        invocation: &[
            "openclaw",
            "agent",
            "--session-id",
            "hermetic",
            "-m",
            PROMPT_PLACEHOLDER,
        ],
        resume: None,
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
    },
];

/// Every registered source without a profile, and why.
pub const NOT_HERMETIC: &[(&str, Absence)] = &[
    // Owner, 2026-10-13: no custom model endpoint, so no upstream detection.
    ("cursor", Absence::NoModelEndpoint),
    // Owner, 2026-10-18: `exec --hooks` fires only `tool_call_before` and
    // `shell_env` (codewhale-hq/Codewhale `docs/HOOKS.md:33-42` at v0.10.1).
    ("codewhale", Absence::NoHeadlessLifecycle),
];

/// The profile for `source`, if it has one.
pub fn profile(source: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.source == source)
}

/// The sources each of an OS's `hermetic.yml` legs runs, as `--shard <n>`
/// selects the `n`th: whole sources, split for about equal run time.
pub const SHARDS: &[&[&str]] = &[
    &[
        "openclaw",
        "codex",
        "kimi",
        "grok",
        "claude-code",
        "opencode",
    ],
    &["hermes", "dsh", "copilot", "omp", "reasonix", "antigravity"],
];

/// Where each profile's scenario scripts live, as `<source>/<name>.json`, and
/// the scripted model's npm lockfile, under `aimock/`.
pub fn scripts_dir() -> PathBuf {
    crate::harness::sources_root().join("hermetic")
}

impl Profile {
    /// The pinned build's per-platform manifests, beside the profile's scripts,
    /// for a [`Package::UpdaterManifest`] profile.
    pub fn manifest_lock(&self) -> Option<PathBuf> {
        matches!(self.package, Package::UpdaterManifest { .. })
            .then(|| scripts_dir().join(self.source).join("manifest.lock.json"))
    }
}

impl Scenario {
    /// `source`'s script for this scenario.
    pub fn script(&self, source: &str) -> PathBuf {
        scripts_dir()
            .join(source)
            .join(format!("{}.json", self.name))
    }

    /// Why a leg on `windows` doesn't run it, if it doesn't.
    pub fn not_run_on(&self, windows: bool) -> Option<&'static str> {
        match self.surface {
            Surface::Tui { row, .. } if windows && !row.renders_under_conpty => {
                Some("its TUI paints nothing under ConPTY")
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;
    use crate::harness::provenance::Provenance;

    #[test]
    fn hermetic_yml_reads_the_new_types_the_tier_writes() {
        let yml = std::fs::read_to_string(bump::repo_root().join(".github/workflows/hermetic.yml"))
            .expect("hermetic.yml");
        assert!(
            yml.contains(&format!("runs/{NEW_TYPES}")),
            "a run that passed with a New finding uploads only as this file says"
        );
    }

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
    fn every_scenario_has_its_script_and_a_name_no_other_in_its_profile_has() {
        for p in PROFILES {
            let mut names = BTreeSet::new();
            for s in p.scenarios {
                assert!(names.insert(s.name), "{}: {} twice", p.source, s.name);
                let script = s.script(p.source);
                assert!(
                    script.is_file(),
                    "{}: no script at {}",
                    p.source,
                    script.display()
                );
                let heard = p.descriptor().hook().is_some() || p.descriptor().is_daemon();
                assert_eq!(
                    s.expects.is_empty(),
                    !heard,
                    "{}: a scenario expects hooks if and only if pixtuoid hears the CLI's",
                    p.source
                );
            }
            assert!(
                !p.scenarios.is_empty(),
                "{}: a profile runs scenarios",
                p.source
            );
            assert!(
                !p.descriptor().is_daemon()
                    || p.scenarios
                        .iter()
                        .all(|s| verify::settled(s.shape).is_some()),
                "{}: a daemon's turn is judged on its presence, which shows only some shapes",
                p.source
            );
        }
    }

    #[test]
    fn a_live_tier_takes_the_ports_the_build_and_the_exit_it_is_given() {
        for p in PROFILES {
            let Some(live) = &p.live else {
                continue;
            };
            let script = std::fs::read_to_string(super::bump::repo_root().join(live.script))
                .expect("script");
            assert!(
                script.contains(&format!("${{{LIVE_PIXTUOID_ENV}:-")),
                "{}: the script ignores the build under test",
                p.source
            );
            assert!(
                script.contains(&format!("exit {LIVE_PREREQUISITE}")),
                "{}: no prerequisite exit",
                p.source
            );
            let default = script
                .lines()
                .find_map(|l| l.split_once("|| PORTS=(").map(|(_, ports)| ports))
                .expect("a default port list");
            assert_eq!(
                default.trim_end_matches(')').split_whitespace().count(),
                live.ports,
                "{}: the orchestrator passes as many ports as the script defaults to",
                p.source
            );
        }
    }

    #[test]
    fn a_tui_turn_takes_no_args_and_resumes_nothing() {
        for p in PROFILES {
            for s in p.scenarios {
                assert!(
                    matches!(s.surface, Surface::Headless)
                        || (s.turn.args.is_empty() && s.turn.resume.is_none()),
                    "{}/{}: a TUI turn takes no args and resumes nothing",
                    p.source,
                    s.name
                );
            }
        }
    }

    /// What the runner's source selection, the lifecycle's turn and the bump
    /// plan take for granted.
    #[test]
    fn no_leg_skips_a_whole_source() {
        for p in PROFILES {
            assert!(
                p.scenarios
                    .iter()
                    .any(|s| matches!(s.surface, Surface::Headless)),
                "{}: no Headless scenario, which every leg runs and the lifecycle turns",
                p.source
            );
            for windows in [false, true] {
                assert!(
                    p.scenarios.iter().any(|s| s.not_run_on(windows).is_none()),
                    "{}: a leg (windows: {windows}) runs none of its scenarios",
                    p.source
                );
            }
        }
    }

    #[test]
    fn a_tui_turn_answers_a_gate_exactly_when_its_shape_is_permission() {
        // A turn waiting on a dialog it never raises stalls, and the verdict
        // blames its CLI.
        for p in PROFILES {
            for s in p.scenarios {
                let Surface::Tui { answer, .. } = s.surface else {
                    continue;
                };
                assert_eq!(
                    answer != tui::Answer::Nothing,
                    s.shape == Shape::Permission,
                    "{}/{}",
                    p.source,
                    s.name
                );
            }
        }
    }

    #[test]
    fn a_resumed_scenarios_profile_names_how_its_session_resumes() {
        for p in PROFILES {
            let resumes = p.scenarios.iter().any(|s| s.turn.resume.is_some());
            if let Some(words) = p.resume {
                assert!(words.contains(&SESSION_PLACEHOLDER), "{}", p.source);
                assert!(words.contains(&PROMPT_PLACEHOLDER), "{}", p.source);
            }
            assert!(!resumes || p.resume.is_some(), "{}: no resume", p.source);
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
    fn the_shards_run_every_profile_once() {
        let mut sharded: Vec<&str> = SHARDS.iter().flat_map(|s| s.iter().copied()).collect();
        let mut profiled: Vec<&str> = PROFILES.iter().map(|p| p.source).collect();
        sharded.sort_unstable();
        profiled.sort_unstable();
        assert_eq!(sharded, profiled);
    }

    #[test]
    fn every_scenario_records_into_a_fixture_of_its_own() {
        let mut fixtures = BTreeSet::new();
        for p in PROFILES {
            for s in p.scenarios {
                assert!(fixtures.insert(s.fixture), "{} recorded twice", s.fixture);
                let path = crate::harness::sources_root()
                    .join(s.fixture)
                    .join(crate::harness::PROVENANCE);
                let prov = Provenance::read(&path).unwrap_or_else(|e| panic!("{}: {e}", p.source));
                assert!(
                    prov.recorded().is_some_and(|r| r.aimock.is_some()),
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
    fn no_tool_run_replies_to_any_result_and_tool_run_waits_on_its_note_unless_unseen() {
        // A reply to any tool result lets a run whose tool failed pass.
        for p in PROFILES {
            for s in p.scenarios.iter().filter(|s| s.shape == Shape::ToolRun) {
                let script: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(s.script(p.source)).expect("the script"),
                )
                .expect("json");
                let replies = script["fixtures"].as_array().expect("fixtures");
                let waits = replies
                    .iter()
                    .any(|f| f["match"]["toolResultContains"] == NOTE_PLACEHOLDER);
                if p.result_unseen {
                    assert!(!waits, "{}: a wait on a result aimock never sees", p.source);
                } else if s.name == "tool-run" {
                    // Only the note-reading scenario has a note to wait on.
                    assert!(waits, "{}: no reply waits for {NOTE_PLACEHOLDER}", p.source);
                }
                assert!(
                    !replies.iter().any(|f| f["match"]["hasToolResult"] == true),
                    "{}: a reply takes any tool result",
                    p.source
                );
            }
        }
    }

    #[test]
    fn a_permission_script_calls_no_tool_after_its_refusal() {
        // A retried tool raises a second dialog, which the TUI driver never refuses.
        for p in PROFILES {
            for s in p.scenarios.iter().filter(|s| s.shape == Shape::Permission) {
                let script: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(s.script(p.source)).expect("the script"),
                )
                .expect("json");
                for reply in script["fixtures"].as_array().expect("fixtures") {
                    let after_a_result = reply["match"]["hasToolResult"] == true
                        || reply["match"]["toolResultContains"].is_string();
                    assert!(
                        !(after_a_result && reply["response"]["toolCalls"].is_array()),
                        "{}: a reply to the refused call calls a tool again",
                        p.source
                    );
                }
            }
        }
    }

    /// Whether a scripted reply calls a tool whose command is `rm` then the
    /// note's name, in one of its top-level string arguments (every committed script's shape).
    fn script_deletes_the_note(script: &serde_json::Value) -> bool {
        let deletes = |command: &str| {
            let words: Vec<&str> = command.split_whitespace().collect();
            words.first() == Some(&"rm") && words[1..].contains(&crate::harness::NOTE_FILE)
        };
        script["fixtures"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|reply| {
                reply["response"]["toolCalls"]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .filter_map(|call| call["arguments"].as_object())
            .flat_map(|arguments| arguments.values())
            .filter_map(serde_json::Value::as_str)
            .any(deletes)
    }

    #[test]
    fn a_permission_script_gates_a_call_that_deletes_the_note() {
        // The recorder's `note_survived` is only evidence of a refusal if the
        // refused call would have removed the note. A question's gate calls
        // nothing that could, so those rows are held to the note alone.
        const QUESTIONS: &[&str] = &["ask-user", "ask-question"];
        for p in PROFILES {
            for s in p
                .scenarios
                .iter()
                .filter(|s| s.shape == Shape::Permission && !QUESTIONS.contains(&s.name))
            {
                let script: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(s.script(p.source)).expect("the script"),
                )
                .expect("json");
                assert!(
                    script_deletes_the_note(&script),
                    "{}: no scripted tool call deletes the note",
                    p.source
                );
            }
        }
    }

    #[test]
    fn only_a_scripted_tool_call_counts_as_deleting_the_note() {
        let script = |reply: serde_json::Value| serde_json::json!({ "fixtures": [reply] });
        let call = |command: &str| {
            script(serde_json::json!({ "response": { "toolCalls": [
                { "name": "t", "arguments": { "command": command } }
            ] } }))
        };
        assert!(script_deletes_the_note(&call("rm NOTE.txt")));
        assert!(script_deletes_the_note(&call("rm -rf NOTE.txt")));
        for not in [
            "cat NOTE.txt",
            "NOTE.txt rm",
            "rm NOTE.txt.bak",
            "echo rm NOTE.txt",
        ] {
            assert!(!script_deletes_the_note(&call(not)), "{not}");
        }
        assert!(
            !script_deletes_the_note(&script(
                serde_json::json!({ "response": { "content": "I will rm NOTE.txt" } })
            )),
            "a reply's prose is not a call"
        );
    }

    #[test]
    fn a_model_error_script_errs_and_calls_no_tool() {
        // A reply that answers the turn lets a sound run pass as a failed one.
        for p in PROFILES {
            for s in p.scenarios.iter().filter(|s| s.shape == Shape::ModelError) {
                let script: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(s.script(p.source)).expect("the script"),
                )
                .expect("json");
                let replies = script["fixtures"].as_array().expect("fixtures");
                assert!(
                    replies.iter().any(|f| f["response"]["status"]
                        .as_u64()
                        .is_some_and(|status| status >= 400)
                        && f["response"]["error"].is_object()),
                    "{}: no reply is an error",
                    p.source
                );
                assert!(
                    !replies
                        .iter()
                        .any(|f| f["response"]["toolCalls"].is_array()),
                    "{}: a reply calls a tool",
                    p.source
                );
            }
        }
    }

    #[test]
    fn a_shape_asks_the_model_nothing_exactly_when_its_script_has_no_reply() {
        // A reply to a turn that asks nothing lets a run whose calls left the
        // sandbox complete with no request.
        for p in PROFILES {
            for s in p.scenarios {
                let script: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(s.script(p.source)).expect("the script"),
                )
                .expect("json");
                let replies = script["fixtures"].as_array().expect("fixtures");
                assert_eq!(
                    replies.is_empty(),
                    !s.shape.asks_the_model(),
                    "{}/{}",
                    p.source,
                    s.name
                );
            }
        }
    }

    #[test]
    fn a_child_error_script_errs_the_childs_request_and_answers_only_its_error() {
        // A reply to any result lets a run whose child succeeded pass as failed.
        for p in PROFILES {
            for s in p.scenarios.iter().filter(|s| s.shape == Shape::ChildError) {
                let script: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(s.script(p.source)).expect("the script"),
                )
                .expect("json");
                let replies = script["fixtures"].as_array().expect("fixtures");
                let errors: Vec<&str> = replies
                    .iter()
                    .filter(|f| {
                        f["match"]["userMessage"].is_string()
                            && f["response"]["status"]
                                .as_u64()
                                .is_some_and(|status| status >= 400)
                    })
                    .filter_map(|f| f["response"]["error"]["message"].as_str())
                    .collect();
                assert!(
                    !errors.is_empty(),
                    "{}: the child's request never errs",
                    p.source
                );
                assert!(
                    replies.iter().any(|f| f["match"]["toolResultContains"]
                        .as_str()
                        .is_some_and(|t| errors.iter().any(|e| e.contains(t)))),
                    "{}: no reply waits for the child's error",
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
