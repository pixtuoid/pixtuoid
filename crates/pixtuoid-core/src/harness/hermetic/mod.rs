//! The hermetic tier's per-CLI facts: how each real CLI is installed, isolated,
//! and driven against a scripted model (aimock), with a daemon's live tier
//! beside it, and which hook events each scenario must fire. The registry already owns the binary (`version_probe`),
//! the home variable (`home_env`) and the pinned version (`verified_version`);
//! a profile holds only what it doesn't.

use std::borrow::Cow;
use std::path::PathBuf;
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
                | Self::Succession => false,
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

/// A CLI's scripted turn, and its second where [`Turn::second`] names one.
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
    /// Its prompt, in place of [`crate::harness::plan::DEFAULT_PROMPT`].
    pub prompt: Option<&'static str>,
    /// A second headless turn on this one's session, in the same sandbox and
    /// workspace once its CLI exited: [`Profile::second_turn`]'s words.
    pub second: Option<Second>,
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
];

/// Every registered source without a profile, and why.
pub const NOT_HERMETIC: &[(&str, Absence)] = &[
    // Owner, 2026-10-13: no custom model endpoint, so no upstream detection.
    ("cursor", Absence::NoModelEndpoint),
];

/// The profile for `source`, if it has one.
pub fn profile(source: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.source == source)
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

    /// The scenario its install lifecycle turns under its last uninstall on a
    /// leg on `windows`.
    pub fn lifecycle_turn(&self, windows: bool) -> Option<&'static Scenario> {
        lifecycle_turn_of(self.scenarios, windows)
    }
}

/// The first of `scenarios` a leg on `windows` runs and no dialog stops, since
/// a TUI's gate waits on a hook.
fn lifecycle_turn_of(scenarios: &[Scenario], windows: bool) -> Option<&Scenario> {
    scenarios.iter().find(|s| {
        s.not_run_on(windows).is_none()
            && match s.surface {
                Surface::Headless => true,
                Surface::Tui { answer, .. } => answer == tui::Answer::Nothing,
            }
    })
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
    fn a_tui_turn_takes_no_args_and_has_no_second_turn() {
        for p in PROFILES {
            for s in p.scenarios {
                assert!(
                    matches!(s.surface, Surface::Headless)
                        || (s.turn.args.is_empty() && s.turn.second.is_none()),
                    "{}/{}: a TUI turn takes no args and has no second turn",
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
    fn a_lifecycle_turns_a_scenario_its_leg_runs_and_no_dialog_stops() {
        let tui = |source: &str, name: &str, answer| match profile(source)
            .and_then(|p| p.scenario(name))
            .expect("profiled")
            .surface
        {
            Surface::Tui { row, .. } => Surface::Tui { row, answer },
            Surface::Headless => panic!("{source}/{name} drives no TUI"),
        };
        let scenario = |name, surface| Scenario {
            name,
            shape: Shape::ToolRun,
            surface,
            expects: &[],
            fixture: name,
            turn: Turn::PROFILE,
        };
        let scenarios = [
            scenario("gated", tui("claude-code", "permission", tui::Answer::Deny)),
            scenario("unpainted", tui("grok", "permission", tui::Answer::Nothing)),
            scenario("ungated", tui("omp", "new-session", tui::Answer::Nothing)),
            scenario("headless", Surface::Headless),
        ];
        let picked = |s: &[Scenario], windows| lifecycle_turn_of(s, windows).map(|s| s.name);
        assert_eq!(picked(&scenarios[..1], false), None);
        assert_eq!(picked(&scenarios[..2], false), Some("unpainted"));
        assert_eq!(
            picked(&scenarios[..2], true),
            None,
            "ConPTY paints it nothing"
        );
        assert_eq!(picked(&scenarios[..3], true), Some("ungated"));
        assert_eq!(picked(&scenarios[3..], true), Some("headless"));
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
                let Some(second) = s.turn.second else {
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
}
