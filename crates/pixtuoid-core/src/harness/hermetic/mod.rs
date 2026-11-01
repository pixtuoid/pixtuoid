//! The hermetic tier's per-CLI facts: how each real CLI is installed, isolated,
//! and driven against a scripted model (aimock), with a daemon's live tier
//! beside it, and which hook events each scenario must fire. The registry already owns the binary (`version_probe`),
//! the home variable (`home_env`) and the pinned version (`verified_version`);
//! a profile holds only what it doesn't.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::host::Hive;
use crate::harness::plan::Placeholder;
use crate::harness::{CaptureRecord, ProcessExit};
use crate::source::registry;

pub mod bump;
pub mod digest;
pub mod dropped;
pub mod issues;
mod literals;
pub mod pin;
mod profiles;
pub mod tui;
pub mod verify;

/// The key a profile hands its CLI for its scripted model: aimock accepts
/// any, and a real provider must refuse it.
pub const DUMMY_KEY: &str = "hermetic-dummy-key";

/// The first line of [`Placeholder::Stream`]'s reply: on screen, the TUI
/// driver's cue that the turn is mid-stream.
pub(crate) const STREAM_CUE: &str = "HERMETIC-STREAM";
/// How many numbered lines follow [`STREAM_CUE`]: enough that, at its
/// script's pace, the reply outlasts the driver's [`tui::BUDGET`]
/// (`a_cancel_script_streams_its_reply_past_the_drivers_budget`), so an
/// uncancelled turn never ends inside a run.
const STREAM_LINES: usize = 600;

/// [`Placeholder::Stream`]'s reply.
fn stream_reply() -> String {
    std::iter::once(STREAM_CUE.to_owned())
        .chain((1..=STREAM_LINES).map(|n| n.to_string()))
        .collect::<Vec<_>>()
        .join("\n")
}

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

/// How long a hermetic turn may run before the recorder kills it: well past
/// the slowest turn a leg has run.
pub const TURN_BUDGET: PerOs<Duration> = PerOs {
    unix: Duration::from_secs(240),
    windows: Duration::from_secs(480),
};

/// Where a CLI reads policy or config that no sandbox variable redirects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostConfig {
    /// A path; a Unix one may spell [`Placeholder::User`], a Windows one start
    /// with a placeholder [`crate::harness::host::known_folders_expanded`] resolves.
    Path(&'static str),
    /// A Windows registry key.
    Registry {
        /// The hive it lives under.
        hive: Hive,
        /// The key below the hive.
        sub: &'static str,
    },
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
        /// Each platform's executable or `.tar.gz` containing one named as
        /// the asset without that suffix, as `(OS, ARCH, asset)` in
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

impl Package {
    /// Where its releases are published.
    pub const fn feed(&self) -> Feed {
        match *self {
            Self::Npm(package) => Feed::Npm(package),
            Self::GithubRelease { repo, .. } | Self::UvLocked(repo) => Feed::GithubLatest(repo),
            Self::UpdaterManifest {
                manifest,
                platforms,
                ..
            } => Feed::UpdaterManifest {
                manifest,
                platforms,
            },
        }
    }
}

/// Where a CLI's releases are published, as its latest one is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feed {
    /// An npm package's `latest` dist-tag.
    Npm(&'static str),
    /// A GitHub repository's latest release, spelled as [`Package::GithubRelease`]'s `repo`.
    GithubLatest(&'static str),
    /// The build an updater's manifest names now, as [`Package::UpdaterManifest`]
    /// spells it.
    UpdaterManifest {
        /// The manifest's URL, with `{platform}` filled for the host.
        manifest: &'static str,
        /// Each platform's name, as `(OS, ARCH, platform)`.
        platforms: &'static [(&'static str, &'static str, &'static str)],
    },
    /// The build the script users pipe to a shell downloads ([`installer_build`]).
    Installer {
        /// The script's URL.
        script: &'static str,
        /// What its download URL holds right before the build.
        build_at: &'static str,
    },
}

/// The build an installer `script` downloads: the path segment after each
/// `build_at` in it, which must all agree.
///
/// # Errors
///
/// When it names no build, or two.
pub fn installer_build<'a>(script: &'a str, build_at: &str) -> Result<&'a str, String> {
    let builds: std::collections::BTreeSet<&str> = script
        .match_indices(build_at)
        .map(|(at, _)| {
            let rest = &script[at + build_at.len()..];
            let end = rest
                .find(|c: char| c == '/' || c == '"' || c == '\'' || c.is_whitespace())
                .unwrap_or(rest.len());
            &rest[..end]
        })
        .collect();
    match builds.into_iter().collect::<Vec<_>>()[..] {
        [build] if !build.is_empty() => Ok(build),
        [] | [_] => Err(format!("the script names no build after {build_at}")),
        ref builds => Err(format!(
            "the script names {} builds after {build_at}",
            builds.len()
        )),
    }
}

/// What a scenario's script drives a CLI through. Shared by every CLI, so one CLI
/// cannot be held to a weaker bar than another, save the gaps a profile's
/// `result_unseen` and its source's `ChildEndInference` owe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A turn that runs one tool: the session registers in the workspace, a
    /// tool starts with a detail naming it, and it ends.
    ToolRun,
    /// A turn whose tool call waits on an approval: the slot waits on it, then
    /// the gated call ends, or its turn where the call never started.
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
    /// A turn the user cancels while its reply streams: the session
    /// registers, and its turn ends.
    Cancel,
}

impl Shape {
    /// Whether a CLI whose turn's process exited as `exit` ran this shape's
    /// turn as scripted: one scripted to fail may exit non-zero, but neither a
    /// signal nor the budget ends a turn.
    pub(crate) const fn exits_as_scripted(self, exit: ProcessExit) -> bool {
        match exit {
            ProcessExit::Exited(0) => true,
            ProcessExit::Exited(_) => match self {
                Self::ModelError => true,
                Self::ToolRun
                | Self::Permission
                | Self::Subagent
                | Self::ChildError
                | Self::Succession
                | Self::Cancel => false,
            },
            ProcessExit::Signalled | ProcessExit::OutOfTime => false,
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
            | Self::ChildError
            | Self::Cancel => true,
            Self::Succession => false,
        }
    }

    /// How a TUI run answers its turn.
    pub(crate) const fn answer(self) -> tui::Answer {
        match self {
            Self::Permission => tui::Answer::Gate,
            Self::Cancel => tui::Answer::Cancel,
            Self::ToolRun
            | Self::Subagent
            | Self::ModelError
            | Self::ChildError
            | Self::Succession => tui::Answer::Nothing,
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
    },
}

/// A CLI's scripted turn, and its second where [`Turn::second`] names one.
#[derive(Debug)]
pub struct Scenario {
    /// Its name, unique in its profile: its script's file stem, its run
    /// directory, its recording's directory ([`Scenario::fixture`]), and what
    /// `just hermetic <source> <name>` selects.
    pub name: &'static str,
    /// What the run must look like.
    pub shape: Shape,
    /// The surface the turn is driven through.
    pub surface: Surface,
    /// The hook events this run must fire, as `Profile::event_name` reads them.
    pub expects: &'static [&'static str],
    /// What its turn asks of the CLI beyond the profile's invocation.
    pub turn: Turn,
}

/// A scenario's own part of its turn.
#[derive(Debug)]
pub struct Turn {
    /// Appended to the profile's headless invocation.
    pub args: &'static [&'static str],
    /// Its prompt, in place of [`crate::harness::plan::DEFAULT_PROMPT`].
    pub prompt: Option<&'static str>,
    /// A second turn on this one's session: on a TUI its prompt, typed once
    /// the first turn settles; headless, [`Profile::second_turn`]'s words once
    /// its CLI exited.
    pub second: Option<Second>,
    /// A directory beside its script copied into the workspace before the
    /// turn: project config this scenario alone runs under.
    pub(crate) seed: Option<&'static str>,
}

/// What a [`Turn::second`] asks of [`Profile::second_turn`]'s words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Second {
    /// A turn with this prompt, which the words spell as [`Placeholder::Prompt`].
    Prompt(&'static str),
    /// No prompt: words that act on the session, as deleting it does.
    Bare,
}

impl Turn {
    /// The profile's invocation as it stands.
    pub(crate) const PROFILE: Self = Self {
        args: &[],
        prompt: None,
        second: None,
        seed: None,
    };
}

/// A daemon's own live tier, driven against this build.
#[derive(Debug)]
pub struct Live {
    /// The tier, from the repository root. It takes its ports as arguments.
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
    /// Its start, after the CLI's binary; [`Placeholder::Port`], [`Placeholder::Key`] and
    /// [`Placeholder::DaemonPort`] expand in the runner, here and in [`Self::ready`].
    pub start: &'static [&'static str],
    /// What exits 0 once it serves, after the CLI's binary.
    pub ready: &'static [&'static str],
    /// Environment it alone gets, beside the profile's.
    pub env: &'static [(&'static str, &'static str)],
}

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
    /// Its contents; [`Placeholder::Port`], [`Placeholder::Key`], [`Placeholder::DaemonPort`],
    /// [`Placeholder::Workspace`] and [`Placeholder::WorkspaceSlashed`] expand.
    pub template: &'static str,
}

/// One CLI's hermetic facts.
#[derive(Debug)]
pub struct Profile {
    /// The registered source id.
    pub source: &'static str,
    /// How users install it.
    pub package: Package,
    /// The npm packages publishing the API its plugin is type-checked against,
    /// which `just plugin-types` installs at its registry row's
    /// `verified_version`.
    pub plugin_types: &'static [&'static str],
    /// The headless invocation: [`Placeholder::Prompt`] expands to the
    /// scenario's prompt in the runner, else to [`crate::harness::plan::DEFAULT_PROMPT`], and
    /// [`Placeholder::Port`], [`Placeholder::Key`] and [`Placeholder::DaemonPort`] too.
    pub invocation: &'static [&'static str],
    /// The headless invocation of a second turn, as [`Turn::second`] runs it:
    /// the runner expands [`Placeholder::Port`], [`Placeholder::Key`], [`Placeholder::DaemonPort`] and a
    /// [`Second::Prompt`]'s [`Placeholder::Prompt`], the recorder only [`Placeholder::Session`].
    pub second_turn: Option<&'static [&'static str]>,
    /// Environment for the CLI: endpoint, dummy key and opt-outs; [`Placeholder::Port`],
    /// [`Placeholder::Key`] and [`Placeholder::DaemonPort`] expand.
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
    /// The CLI's own command that stops the server it starts for its turns,
    /// run after each scenario's: their sessions end inside the run, and no
    /// process outlives it.
    pub server_stop: Option<&'static [&'static str]>,
    /// Every in-process exit a turn takes, for a source whose registry row
    /// brackets its turns; empty for any other.
    pub(crate) turn_exits: &'static [TurnExit],
}

/// An in-process exit of a turn, and the scenario recording it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TurnExit {
    /// The `type` of the line that closes the turn.
    pub(crate) exit: &'static str,
    /// A field that line carries on this exit alone.
    pub(crate) with: Option<&'static str>,
    /// The scenario of its profile that records it.
    pub(crate) scenario: &'static str,
}

impl std::fmt::Display for TurnExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}`", self.exit)?;
        match self.with {
            Some(field) => write!(f, " with `{field}`"),
            None => Ok(()),
        }
    }
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
            (Placeholder::Workspace.as_str(), workspace.to_owned()),
            (
                Placeholder::Note.as_str(),
                crate::harness::CAPTURE_NOTE.to_owned(),
            ),
            (Placeholder::Stream.as_str(), stream_reply()),
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
    pub(crate) fn outcome(self) -> &'static str {
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

/// A registered source without a profile: why, and where its releases are
/// published, which the nightly reads so a capture only a billed run refreshes
/// is not left behind.
#[derive(Debug)]
pub struct Unprofiled {
    /// The registered source id.
    pub source: &'static str,
    /// Why it has no profile.
    pub absence: Absence,
    /// Where its releases are published.
    pub feed: Feed,
}

/// Why a registered source has no profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Absence {
    /// Its model endpoint cannot be redirected, so no scripted turn can run.
    NoModelEndpoint,
}

/// The profiled CLIs.
pub const PROFILES: &[Profile] = &[
    profiles::claude_code::PROFILE,
    profiles::codex::PROFILE,
    profiles::opencode::PROFILE,
    profiles::dsh::PROFILE,
    profiles::kimi::PROFILE,
    profiles::grok::PROFILE,
    profiles::reasonix::PROFILE,
    profiles::copilot::PROFILE,
    profiles::omp::PROFILE,
    profiles::hermes::PROFILE,
    profiles::antigravity::PROFILE,
    profiles::openclaw::PROFILE,
    profiles::codewhale::PROFILE,
    profiles::jcode::PROFILE,
];

/// Every registered source without a profile.
pub const NOT_HERMETIC: &[Unprofiled] = &[Unprofiled {
    source: crate::source::cursor::SOURCE_NAME,
    // Owner, 2026-10-13: no custom model endpoint.
    absence: Absence::NoModelEndpoint,
    // `DOWNLOAD_URL="https://downloads.cursor.com/lab/<build>/${OS}/${ARCH}/…"`
    // (the script at 2026.10.22-27dcf5f, `:85`).
    feed: Feed::Installer {
        script: "https://cursor.com/install",
        build_at: "https://downloads.cursor.com/lab/",
    },
}];

/// The profile for `source`, if it has one.
pub fn profile(source: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.source == source)
}

/// Every home variable `source`'s CLI reads that a sandbox points into its
/// own directories: its profile's ([`Profile::home_vars`]), else its registry
/// row's.
pub fn home_vars(source: &str) -> Vec<&'static str> {
    match profile(source) {
        Some(p) => p.home_vars().collect(),
        None => crate::source::registry::descriptor_for(source)
            .and_then(|d| d.home_env)
            .into_iter()
            .collect(),
    }
}

/// The matrix's `os` axis, as `hermetic.yml` names it.
pub(crate) const OSES: &[&str] = &["macos", "windows"];

/// The sources each of an OS's `hermetic.yml` legs runs, as `--shard <n>`
/// selects the `n`th: whole sources, split for about equal run time.
pub const SHARDS: &[&[&str]] = &[
    &[
        "openclaw",
        "codex",
        "omp",
        "dsh",
        "antigravity",
        "codewhale",
        "jcode",
    ],
    &[
        "grok",
        "copilot",
        "hermes",
        "opencode",
        "claude-code",
        "reasonix",
        "kimi",
    ],
];

/// One leg of `hermetic.yml`'s matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Leg {
    pub(crate) os: &'static str,
    /// Counting from 1, as `--shard` does.
    pub(crate) shard: usize,
}

impl std::fmt::Display for Leg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}, shard {}", self.os, self.shard)
    }
}

/// Every leg, each of [`OSES`] by each of [`SHARDS`].
pub(crate) fn legs() -> impl Iterator<Item = Leg> {
    OSES.iter()
        .flat_map(|&os| (1..=SHARDS.len()).map(move |shard| Leg { os, shard }))
}

/// The shard that runs `source`, counting from 1, if one does.
pub(crate) fn shard_of(source: &str) -> Option<usize> {
    SHARDS
        .iter()
        .position(|s| s.contains(&source))
        .map(|i| i + 1)
}

/// The scripted model's npm project, whose lockfile every run installs.
pub fn aimock_dir() -> PathBuf {
    crate::harness::sources_root().join("hermetic/aimock")
}

/// Where `source`'s profile keeps its scenario scripts, their seeds and its
/// manifest lock: its home's `hermetic/`.
fn scripts_dir(source: &str) -> PathBuf {
    crate::harness::source_home(source).join("hermetic")
}

impl Profile {
    /// The pinned build's per-platform manifests, beside the profile's scripts,
    /// for a [`Package::UpdaterManifest`] profile.
    pub fn manifest_lock(&self) -> Option<PathBuf> {
        matches!(self.package, Package::UpdaterManifest { .. })
            .then(|| scripts_dir(self.source).join("manifest.lock.json"))
    }

    /// The scenario its install lifecycle turns under its last uninstall on a
    /// leg on `windows`.
    pub fn lifecycle_turn(&self, windows: bool) -> Option<&'static Scenario> {
        lifecycle_turn_of(self.scenarios, windows)
    }
}

/// The first of `scenarios` a leg on `windows` runs whose TUI turn, if it has
/// one, the driver need not answer: a gate waits on a hook, and a cancel's
/// reply outlasts the run ([`STREAM_LINES`]).
fn lifecycle_turn_of(scenarios: &[Scenario], windows: bool) -> Option<&Scenario> {
    scenarios.iter().find(|s| {
        s.not_run_on(windows).is_none()
            && match s.surface {
                Surface::Headless => true,
                Surface::Tui { .. } => s.answer() == tui::Answer::Nothing,
            }
    })
}

impl Scenario {
    /// The committed recording `--record` rewrites, under `tests/sources`:
    /// `<module>/fixtures/<name>-hermetic`, so renaming a scenario moves it.
    pub fn fixture(&self, source: &str) -> PathBuf {
        crate::harness::fixtures_dir(Path::new(""), source).join(format!("{}-hermetic", self.name))
    }

    /// `source`'s script for this scenario.
    pub fn script(&self, source: &str) -> PathBuf {
        scripts_dir(source).join(format!("{}.json", self.name))
    }

    /// `source`'s seed for this scenario, if its turn names one.
    pub fn seed(&self, source: &str) -> Option<PathBuf> {
        self.turn.seed.map(|dir| scripts_dir(source).join(dir))
    }

    /// Why a leg on `windows` doesn't run it, if it doesn't.
    pub fn not_run_on(&self, windows: bool) -> Option<&'static str> {
        match self.surface {
            Surface::Tui { row, .. } if windows => row.conpty.not_run(),
            Surface::Tui { .. } | Surface::Headless => None,
        }
    }

    /// Its second turn, where a second CLI process runs it: a TUI types its
    /// own into the same session.
    pub fn headless_second(&self) -> Option<Second> {
        match self.surface {
            Surface::Headless => self.turn.second,
            Surface::Tui { .. } => None,
        }
    }

    /// How its TUI run answers its turn: as its shape says, but nothing where
    /// its row lists no gate, as a seeded hook then decides every prompt.
    pub(crate) fn answer(&self) -> tui::Answer {
        match (self.shape.answer(), self.surface) {
            (tui::Answer::Gate, Surface::Tui { row }) if row.gates.is_empty() => {
                tui::Answer::Nothing
            }
            (answer, _) => answer,
        }
    }

    /// The driver of its TUI run, typing `prompt` and then its second turn's;
    /// `None` for a headless one.
    pub fn tui_driver(
        &self,
        prompt: String,
        now: std::time::Instant,
    ) -> Option<tui::Driver<'static>> {
        let Surface::Tui { row } = self.surface else {
            return None;
        };
        let driver = tui::Driver::new(row, self.answer(), prompt, now);
        Some(match self.turn.second {
            Some(Second::Prompt(next)) => driver.then(next),
            Some(Second::Bare) | None => driver,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;
    use crate::harness::provenance::Provenance;

    #[test]
    fn hermetic_yml_reads_the_verdicts_and_names_the_upload_of_every_leg() {
        let yml = std::fs::read_to_string(bump::repo_root().join(".github/workflows/hermetic.yml"))
            .expect("hermetic.yml");
        assert!(
            yml.contains(&format!("runs/{}", issues::VERDICTS)),
            "the file the tier writes"
        );
        let new = serde_json::to_value(verify::Label::New).expect("a label");
        let verdicts = serde_json::to_value(issues::Verdicts {
            os: "macos".into(),
            shard: Some(1),
            runs: vec![issues::RunVerdict {
                source: "codex".into(),
                key: "codex/tool-run".into(),
                version: None,
                label: verify::Label::New,
                findings: Vec::new(),
            }],
        })
        .expect("verdicts");
        assert_eq!(verdicts["runs"][0]["label"], new);
        assert!(
            yml.contains(&format!(".runs | any(.label == {new})")),
            "a run that passed with a New finding uploads only as its verdicts read"
        );
        for leg in legs() {
            assert_eq!(
                yml.matches(&format!("{}:", issues::runs_url_output(leg)))
                    .count(),
                2,
                "{leg}'s uploaded runs, as the job's output and the workflow's"
            );
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn the_matrix_names_this_os_as_its_legs_report_it() {
        let os = std::env::consts::OS;
        assert!(
            OSES.contains(&os),
            "a leg reports {os:?}, which no leg of {OSES:?} is"
        );
    }

    #[test]
    fn every_registered_source_is_profiled_or_says_why_not_and_never_both() {
        let registered: BTreeSet<&str> = registry::registered_source_names().collect();
        let profiled: BTreeSet<&str> = PROFILES.iter().map(|p| p.source).collect();
        let absent: BTreeSet<&str> = NOT_HERMETIC.iter().map(|u| u.source).collect();
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
    fn an_installer_names_its_build_once_or_says_why_not() {
        // cursor's script as fetched at 2026.10.22-27dcf5f, its download line.
        let script = r#"DOWNLOAD_URL="https://downloads.cursor.com/lab/2026.10.22-27dcf5f/${OS}/${ARCH}/agent-cli-package.tar.gz""#;
        let [
            Unprofiled {
                feed: Feed::Installer { build_at, .. },
                ..
            },
        ] = NOT_HERMETIC
        else {
            panic!("cursor's feed is its installer");
        };
        assert_eq!(installer_build(script, build_at), Ok("2026.10.22-27dcf5f"));
        assert!(installer_build("echo no url", build_at).is_err());
        assert!(
            installer_build(&format!("{build_at}/x"), build_at).is_err(),
            "an empty build"
        );
        let two = format!("{script}\n{}", script.replace("10.22", "10.23"));
        assert!(installer_build(&two, build_at).is_err(), "two builds");
        let twice = format!("{script}\n{script}");
        assert_eq!(installer_build(&twice, build_at), Ok("2026.10.22-27dcf5f"));
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
                if let Some(seed) = s.seed(p.source) {
                    assert!(seed.is_dir(), "{}: no seed at {}", p.source, seed.display());
                }
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
    fn a_live_tier_takes_the_ports_and_the_build_it_is_given() {
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
    fn a_tui_turn_takes_no_args_and_a_second_turn_only_as_a_prompt_it_types() {
        for p in PROFILES {
            for s in p.scenarios {
                assert!(
                    matches!(s.surface, Surface::Headless)
                        || (s.turn.args.is_empty()
                            && matches!(s.turn.second, None | Some(Second::Prompt(_)))),
                    "{}/{}: a TUI turn takes no args, and types its second turn's prompt",
                    p.source,
                    s.name
                );
            }
        }
    }

    /// What the runner's source selection, the lifecycle's turn and the bump
    /// plan take for granted.
    #[test]
    fn no_leg_skips_a_whole_source_or_its_lifecycle() {
        for p in PROFILES {
            for windows in [false, true] {
                assert!(
                    p.lifecycle_turn(windows).is_some(),
                    "{}: a leg (windows: {windows}) runs no scenario its lifecycle can turn",
                    p.source
                );
            }
        }
    }

    #[test]
    fn a_lifecycle_turns_a_scenario_its_leg_runs_and_the_driver_need_not_answer() {
        let tui = |source: &str, name: &str| {
            let surface = profile(source)
                .and_then(|p| p.scenario(name))
                .expect("profiled")
                .surface;
            assert!(
                matches!(surface, Surface::Tui { .. }),
                "{source}/{name} drives no TUI"
            );
            surface
        };
        let scenario = |name, shape, surface| Scenario {
            name,
            shape,
            surface,
            expects: &[],
            turn: Turn::PROFILE,
        };
        let scenarios = [
            scenario("gated", Shape::Permission, tui("claude-code", "permission")),
            scenario("cancelled", Shape::Cancel, tui("codex", "cancel")),
            scenario("unpainted", Shape::ToolRun, tui("grok", "permission")),
            scenario("ungated", Shape::ToolRun, tui("omp", "new-session")),
            scenario("headless", Shape::ToolRun, Surface::Headless),
        ];
        let picked = |s: &[Scenario], windows| lifecycle_turn_of(s, windows).map(|s| s.name);
        assert_eq!(picked(&scenarios[..2], false), None);
        assert_eq!(picked(&scenarios[..3], false), Some("unpainted"));
        assert_eq!(
            picked(&scenarios[..3], true),
            None,
            "ConPTY paints it nothing"
        );
        assert_eq!(picked(&scenarios[..4], true), Some("ungated"));
        assert_eq!(picked(&scenarios[4..], true), Some("headless"));
    }

    #[test]
    fn a_cancel_runs_on_a_tui_whose_row_names_its_cancel_keys() {
        // A headless run has no way to cancel its turn.
        for p in PROFILES {
            for s in p.scenarios.iter().filter(|s| s.shape == Shape::Cancel) {
                let Surface::Tui { row } = s.surface else {
                    panic!("{}/{} runs headless", p.source, s.name);
                };
                assert!(
                    !row.cancel.is_empty(),
                    "{}: its row names no cancel keys",
                    p.source
                );
            }
        }
    }

    #[test]
    fn a_cancel_script_streams_its_reply_past_the_drivers_budget() {
        // A reply that ends inside the run ends its turn uncancelled, and that
        // end passes for the cancel's.
        let reply = stream_reply();
        assert!(reply.starts_with(&format!("{STREAM_CUE}\n")));
        for p in PROFILES {
            for s in p.scenarios.iter().filter(|s| s.shape == Shape::Cancel) {
                let script: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(s.script(p.source)).expect("the script"),
                )
                .expect("json");
                let streams: Vec<&Value> = script["fixtures"]
                    .as_array()
                    .expect("fixtures")
                    .iter()
                    .filter(|f| f["response"]["content"] == Placeholder::Stream.as_str())
                    .collect();
                let [stream] = streams[..] else {
                    panic!("{}: {} replies stream", p.source, streams.len());
                };
                // Any other key may pace or cut the stream (aimock's
                // `dist/sse-writer.js` `calculateDelay`, `dist/interruption.js`
                // at v1.47.0).
                for key in stream.as_object().expect("a reply").keys() {
                    assert!(
                        ["match", "response", "latency", "chunkSize"].contains(&key.as_str()),
                        "{}: {key}",
                        p.source
                    );
                }
                let per_chunk = |key: &str| {
                    stream[key]
                        .as_u64()
                        .unwrap_or_else(|| panic!("{}: no {key}", p.source))
                };
                let chunks = u64::try_from(reply.len())
                    .expect("a short reply")
                    .div_ceil(per_chunk("chunkSize"));
                assert!(
                    Duration::from_millis(chunks * per_chunk("latency")) > tui::BUDGET,
                    "{}: its reply ends inside the driver's budget",
                    p.source
                );
            }
        }
    }

    #[test]
    fn a_second_turns_words_name_its_session_and_a_prompt_only_when_it_has_one() {
        for p in PROFILES {
            if let Some(words) = p.second_turn {
                assert!(
                    words.contains(&Placeholder::Session.as_str()),
                    "{}",
                    p.source
                );
            }
            for s in p.scenarios {
                let Some(second) = s.headless_second() else {
                    continue;
                };
                let words = p
                    .second_turn
                    .unwrap_or_else(|| panic!("{}: no second turn", p.source));
                assert_eq!(
                    words.contains(&Placeholder::Prompt.as_str()),
                    matches!(second, Second::Prompt(_)),
                    "{}/{}: its words spell a prompt exactly when it has one",
                    p.source,
                    s.name
                );
            }
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
    fn a_server_stop_runs_the_registered_binary() {
        for p in PROFILES {
            if let Some(words) = p.server_stop {
                assert_eq!(
                    words.first(),
                    p.invocation.first(),
                    "{}: the server stop runs a binary the registry does not name",
                    p.source
                );
            }
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
                let fixture = s.fixture(p.source);
                assert!(
                    fixtures.insert(fixture.clone()),
                    "{fixture:?} recorded twice"
                );
                let path = crate::harness::sources_root()
                    .join(&fixture)
                    .join(crate::harness::PROVENANCE);
                let prov = Provenance::read(&path).unwrap_or_else(|e| panic!("{}: {e}", p.source));
                assert!(
                    prov.recorded().aimock.is_some(),
                    "{0}: {1:?} holds no recording of the tier; `just hermetic --record {0}` writes one",
                    p.source,
                    fixture
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
                    !path
                        .replace(Placeholder::User.as_str(), "")
                        .contains(['{', '}']),
                    "{}: {path} spells a placeholder other than {}",
                    p.source,
                    Placeholder::User.as_str()
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
                    HostConfig::Registry { .. } => {}
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
                    .any(|f| f["match"]["toolResultContains"] == Placeholder::Note.as_str());
                if p.result_unseen {
                    assert!(!waits, "{}: a wait on a result aimock never sees", p.source);
                } else if s.name == "tool-run" {
                    // Only the note-reading scenario has a note to wait on.
                    assert!(
                        waits,
                        "{}: no reply waits for {}",
                        p.source,
                        Placeholder::Note.as_str()
                    );
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
    fn a_permission_script_calls_no_tool_after_its_gated_call() {
        // A retried tool raises a second dialog in its turn, which the driver never answers.
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
                        "{}: a reply to the gated call calls a tool again",
                        p.source
                    );
                }
            }
        }
    }

    /// Whether a scripted reply calls a tool whose command is `rm` then the
    /// note's name: in one of a call's top-level string arguments, or in a
    /// string literal of a custom call's source (a code-mode cell's).
    fn script_deletes_the_note(script: &serde_json::Value) -> bool {
        let deletes = |command: &str| {
            let words: Vec<&str> = command.split_whitespace().collect();
            words.first() == Some(&"rm") && words[1..].contains(&crate::harness::NOTE_FILE)
        };
        let calls = |key: &'static str| {
            script["fixtures"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(move |reply| reply["response"][key].as_array().into_iter().flatten())
        };
        let arguments = calls("toolCalls")
            .filter_map(|call| call["arguments"].as_object())
            .flat_map(|arguments| arguments.values())
            .filter_map(serde_json::Value::as_str);
        let literals = calls("customToolCalls")
            .filter_map(|call| call["input"].as_str())
            .flat_map(|source| source.split('"').skip(1).step_by(2));
        arguments.chain(literals).any(deletes)
    }

    /// Whether a scripted reply waits on the note's text, which only the
    /// gated call's result carries once the run approved it.
    fn script_waits_on_the_note(script: &serde_json::Value) -> bool {
        script["fixtures"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|f| f["match"]["toolResultContains"] == Placeholder::Note.as_str())
    }

    #[test]
    fn a_permission_script_gates_a_call_that_deletes_the_note_or_waits_on_it() {
        // The recorder's `note_survived` is only evidence of a refusal if the
        // refused call would have removed the note, and an approval's is the
        // note in the gated call's result, which no refused call returns. A
        // question's gate calls nothing that could, so those rows are held to
        // the note alone.
        const QUESTIONS: &[&str] = &["ask-user", "ask-question", "ask"];
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
                    script_deletes_the_note(&script) || script_waits_on_the_note(&script),
                    "{}/{}: no scripted tool call deletes the note, and no reply waits on it",
                    p.source,
                    s.name
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
        let cell = |source: &str| {
            script(serde_json::json!({ "response": { "customToolCalls": [
                { "name": "exec", "input": source }
            ] } }))
        };
        assert!(script_deletes_the_note(&cell(
            r#"await tools.exec_command({ cmd: "rm NOTE.txt" });"#
        )));
        assert!(
            !script_deletes_the_note(&cell("// rm NOTE.txt\ntext(1);")),
            "a cell's comment is not a call"
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
                // codewhale hands its parent a child's failure in a user-role
                // message (`crates/tui/src/runtime_handoff.rs:3-4`, `:556-567` at v0.10.2).
                let waits = |f: &Value| {
                    ["toolResultContains", "userMessage"].iter().any(|k| {
                        f["match"][k]
                            .as_str()
                            .is_some_and(|t| errors.iter().any(|e| e.contains(t)))
                    })
                };
                assert!(
                    replies.iter().any(waits),
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

    /// Every event a committed capture in `dir` of `source` decodes to.
    fn captured_events(source: &str, dir: &Path) -> Vec<crate::AgentEvent> {
        let mut events = Vec::new();
        for file in crate::harness::jsonl_files_under(dir) {
            let bytes = std::fs::read(&file).expect("a committed capture reads");
            let driven = if crate::harness::reads::is_hook_envelope(&file) {
                crate::harness::Drive::hooks().body(&bytes)
            } else if let Some(drive) = crate::harness::Drive::transcript_at(source, &file) {
                drive.seeded().body(&bytes)
            } else {
                continue;
            };
            events.extend(driven.events);
        }
        events
    }

    /// The exit of `exits` capture `line` takes: its `type`, in a rollout's
    /// `payload` or else the line's own, is the exit's, and a field it carries
    /// picks the exit naming it over the bare type's.
    fn exit_of<'a>(exits: &'a [TurnExit], line: &Value) -> Option<&'a TurnExit> {
        let typed = line
            .get("payload")
            .filter(|p| p.get("type").is_some())
            .unwrap_or(line);
        exits
            .iter()
            .filter(|e| typed.get("type").and_then(Value::as_str) == Some(e.exit))
            .filter(|e| {
                e.with
                    .is_none_or(|f| typed.get(f).is_some_and(|v| !v.is_null()))
            })
            .max_by_key(|e| e.with.is_some())
    }

    /// Every JSON line of the committed capture in `dir`.
    fn captured_lines(dir: &Path) -> Vec<Value> {
        crate::harness::jsonl_files_under(dir)
            .iter()
            .flat_map(|file| {
                crate::harness::captures::json_lines(file).expect("a committed capture reads")
            })
            .collect()
    }

    /// `SourceCaps::brackets_turns`' conformance: a source brackets its turns
    /// exactly where its profile names a recording of every exit a turn takes,
    /// each recording closing every turn it opens through its own exit alone,
    /// and no capture of any other source decodes a turn's start.
    #[test]
    fn a_source_brackets_turns_only_where_every_exit_is_recorded() {
        for source in registry::registered_source_names() {
            let brackets = registry::descriptor_for(source)
                .expect("registered")
                .caps()
                .brackets_turns;
            let exits = profile(source).map_or(&[][..], |p| p.turn_exits);
            assert_eq!(
                brackets,
                !exits.is_empty(),
                "{source}: it brackets its turns only with every exit recorded"
            );
            for exit in exits {
                let scenario = profile(source)
                    .and_then(|p| p.scenario(exit.scenario))
                    .unwrap_or_else(|| panic!("{source}: no scenario {}", exit.scenario));
                let dir = crate::harness::sources_root().join(scenario.fixture(source));
                let events = captured_events(source, &dir);
                assert!(
                    events
                        .iter()
                        .any(|e| matches!(e, crate::AgentEvent::TurnStart { .. })),
                    "{source}/{}: its recording of {exit} opens no turn",
                    exit.scenario,
                );
                let open = verify::unclosed_turns(&events);
                assert!(
                    open.is_empty(),
                    "{source}/{}: its recording of {exit} leaves {open:?} open",
                    exit.scenario,
                );
                let taken: BTreeSet<&TurnExit> = captured_lines(&dir)
                    .iter()
                    .filter_map(|line| exit_of(exits, line))
                    .collect();
                assert_eq!(
                    taken,
                    BTreeSet::from([exit]),
                    "{source}/{}: its recording takes another exit than {exit}",
                    exit.scenario,
                );
            }
        }
        for capture in crate::harness::captures(&crate::harness::sources_root()) {
            let brackets =
                registry::descriptor_for(&capture.source).is_some_and(|d| d.caps().brackets_turns);
            if brackets {
                continue;
            }
            let starts = captured_events(&capture.source, &capture.dir)
                .iter()
                .any(|e| matches!(e, crate::AgentEvent::TurnStart { .. }));
            assert!(
                !starts,
                "{}: {} opens a turn, but its source does not bracket its turns",
                capture.source,
                capture.dir.display()
            );
        }
    }
}
