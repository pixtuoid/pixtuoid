//! The plan a capture runs from: everything the recorder (`capture_fixture`)
//! needs to run one scenario, resolved on the host before the recorder starts.
//!
//! The hermetic runner writes it into the run dir ([`CAPTURE_PLAN`]) and the
//! recorder reads it back; an operator's `just capture-fixture` line builds the
//! same type in memory ([`CapturePlan::from_operator`]). So the recorder has one
//! input, and a word the host left unexpanded cannot be spelled in it: [`Resolved`]
//! refuses a placeholder, and [`Word::Session`] is the one hole, filled after the
//! first turn.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::harness::provenance::NonEmpty;

/// A run dir's plan, beside its [`CAPTURE_RECORD`](crate::harness::CAPTURE_RECORD).
pub const CAPTURE_PLAN: &str = "capture-plan.json";

/// One prompt for every source, so captures stay comparable: reading the SAME
/// file twice around a list forces both shapes the composed fixtures got wrong —
/// tools that interleave, and a tool id that repeats. [`PROMPT_ENV`] overrides
/// it for a scenario the shared one cannot reach (a permission gate needs a tool
/// the CLI refuses to run unasked).
pub const DEFAULT_PROMPT: &str =
    "Read NOTE.txt, then list this directory, then read NOTE.txt again.";
/// The operator's prompt for a scenario [`DEFAULT_PROMPT`] cannot reach.
pub const PROMPT_ENV: &str = "CAPTURE_PROMPT";
/// A directory the operator's capture copies into its workspace first.
pub const SEED_ENV: &str = "CAPTURE_SEED";

/// A word a profile, script or config spells for the host to fill in. Each
/// spelling here is the one every expansion site and every check reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder {
    /// A turn's prompt.
    Prompt,
    /// The session a second turn resumes: known only once the first turn ran.
    Session,
    /// The scripted model's port.
    Port,
    /// The scripted model's dummy key.
    Key,
    /// A daemon's port.
    DaemonPort,
    /// The workspace the CLI runs in, with its OS's separators.
    Workspace,
    /// The same, with `/` separators.
    WorkspaceSlashed,
    /// The operator's user name.
    User,
    /// The note a tool reads.
    Note,
    /// The platform's shell tool.
    ShellTool,
}

impl Placeholder {
    /// Every placeholder.
    pub const ALL: [Self; 10] = [
        Self::Prompt,
        Self::Session,
        Self::Port,
        Self::Key,
        Self::DaemonPort,
        Self::Workspace,
        Self::WorkspaceSlashed,
        Self::User,
        Self::Note,
        Self::ShellTool,
    ];

    /// How profiles, scripts and configs spell it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "{prompt}",
            Self::Session => "{session}",
            Self::Port => "{port}",
            Self::Key => "{key}",
            Self::DaemonPort => "{daemon_port}",
            Self::Workspace => "{workspace}",
            Self::WorkspaceSlashed => "{workspace_slashed}",
            Self::User => "{user}",
            Self::Note => "{note}",
            Self::ShellTool => "{shell_tool}",
        }
    }
}

/// Why a plan is not one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PlanError(String);

impl PlanError {
    fn new(why: impl Into<String>) -> Self {
        Self(why.into())
    }
}

/// A command word the host finished: no [`Placeholder`] is left in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct Resolved(String);

impl Resolved {
    /// The word.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Resolved {
    type Error = PlanError;

    fn try_from(word: String) -> Result<Self, PlanError> {
        match Placeholder::ALL.iter().find(|p| word.contains(p.as_str())) {
            Some(left) => Err(PlanError::new(format!(
                "{word:?} still spells {}: the host expands it before the plan is written",
                left.as_str()
            ))),
            None => Ok(Self(word)),
        }
    }
}

impl TryFrom<&str> for Resolved {
    type Error = PlanError;

    fn try_from(word: &str) -> Result<Self, PlanError> {
        word.to_owned().try_into()
    }
}

/// A second turn's word: finished, or the session the first turn's wire named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Word {
    /// A word the host finished.
    Lit(Resolved),
    /// [`Placeholder::Session`], which the recorder fills.
    Session,
}

impl Word {
    /// The word, with `session` where the session goes.
    pub fn filled(&self, session: &str) -> String {
        match self {
            Self::Lit(word) => word.as_str().to_owned(),
            Self::Session => session.to_owned(),
        }
    }
}

/// A command line: a program, then its arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Argv<T>(Vec<T>);

impl<T> Argv<T> {
    /// The words, in order.
    pub fn words(&self) -> &[T] {
        &self.0
    }
}

impl<T> TryFrom<Vec<T>> for Argv<T> {
    type Error = PlanError;

    fn try_from(words: Vec<T>) -> Result<Self, PlanError> {
        if words.is_empty() {
            Err(PlanError::new("a command line with no program"))
        } else {
            Ok(Self(words))
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Argv<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::try_from(Vec::<T>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl Argv<Resolved> {
    /// `words`, each finished.
    pub fn resolved<S: AsRef<str>>(words: impl IntoIterator<Item = S>) -> Result<Self, PlanError> {
        words
            .into_iter()
            .map(|w| Resolved::try_from(w.as_ref()))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
    }

    /// The words as the strings a process is started from.
    pub fn strings(&self) -> Vec<String> {
        self.0.iter().map(|w| w.as_str().to_owned()).collect()
    }
}

/// A path a process outside the caller's directory can use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PathBuf")]
pub struct AbsPath(PathBuf);

impl AbsPath {
    /// The path.
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl TryFrom<PathBuf> for AbsPath {
    type Error = PlanError;

    fn try_from(path: PathBuf) -> Result<Self, PlanError> {
        if path.is_absolute() {
            Ok(Self(path))
        } else {
            Err(PlanError::new(format!(
                "{} is not absolute: the recorder runs elsewhere",
                path.display()
            )))
        }
    }
}

impl TryFrom<&Path> for AbsPath {
    type Error = PlanError;

    fn try_from(path: &Path) -> Result<Self, PlanError> {
        path.to_path_buf().try_into()
    }
}

/// Where a capture is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sink {
    /// The raw capture, unstripped, with no provenance: drift evidence, never
    /// committed.
    Raw {
        /// The directory it goes in.
        out: AbsPath,
    },
    /// The committed fixture the scenario names.
    Committed,
}

/// A server the CLI talks to, run around its run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonPlan {
    /// Its start.
    pub start: Argv<Resolved>,
    /// What exits 0 once it serves.
    pub ready: Argv<Resolved>,
    /// The environment its spawn alone gets.
    pub env: BTreeMap<String, String>,
}

/// One scenario's capture, as the recorder runs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePlan {
    /// The registered source id.
    pub source: String,
    /// The scenario's name.
    pub scenario: String,
    /// Where the capture goes.
    pub sink: Sink,
    /// Each payload, as received, is forwarded to this endpoint: a headless
    /// pixtuoid renders the same run the recorder records.
    pub tee: Option<AbsPath>,
    /// A sandbox the caller pointed every home into: a harvest root outside
    /// it is refused, since a resolver that slipped would harvest the
    /// operator's own transcripts.
    pub within: Option<AbsPath>,
    /// The workspace as the host resolved it: Windows resolves its known
    /// ProgramData only in an environment the sandbox's no longer holds.
    pub workspace: Option<AbsPath>,
    /// The CLI's `--version`, asked by a caller that launched it some way the
    /// recorder cannot repeat (an interpreter and an entry script).
    pub cli_version: Option<NonEmpty>,
    /// Set on the CLI after the agent-env scrub, which would strip a caller's
    /// own `CLAUDE_CODE_*` opt-outs with the agent's.
    pub cli_env: BTreeMap<String, String>,
    /// A directory copied into the workspace before the run.
    pub seed: Option<NonEmpty>,
    /// The operator's prompt, when it replaced [`DEFAULT_PROMPT`]: the record
    /// of it, since the words already hold it.
    pub prompt_override: Option<NonEmpty>,
    /// The command line as the operator typed it, `{prompt}` unexpanded: what a
    /// committed capture's provenance records. Never run.
    pub recorded_command: Option<NonEmpty>,
    /// A server run around the CLI.
    pub daemon: Option<DaemonPlan>,
    /// The first turn.
    pub first_turn: Argv<Resolved>,
    /// A second turn, run once the first's CLI exited.
    pub second_turn: Option<Argv<Word>>,
}

impl CapturePlan {
    /// Why this plan cannot run, across the fields no type holds apart.
    pub fn validate(&self) -> Result<(), PlanError> {
        if self.second_turn.is_some() && !matches!(self.sink, Sink::Raw { .. }) {
            return Err(PlanError::new(
                "a second turn runs only into a raw capture: a committed one records one turn",
            ));
        }
        Ok(())
    }

    /// Write it into `dir` as [`CAPTURE_PLAN`], once it validates.
    pub fn write_to(&self, dir: &Path) -> std::io::Result<PathBuf> {
        self.validate().map_err(std::io::Error::other)?;
        let file = dir.join(CAPTURE_PLAN);
        std::fs::write(&file, serde_json::to_string_pretty(self)?)?;
        Ok(file)
    }

    /// The plan `file` holds.
    pub fn read(file: &Path) -> std::io::Result<Self> {
        let plan: Self = serde_json::from_str(&std::fs::read_to_string(file)?)
            .map_err(|e| std::io::Error::other(format!("{}: {e}", file.display())))?;
        plan.validate().map_err(std::io::Error::other)?;
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(p: &str) -> AbsPath {
        AbsPath::try_from(Path::new(p)).expect("absolute")
    }

    fn plan() -> CapturePlan {
        CapturePlan {
            source: "kimi".into(),
            scenario: "tool-run".into(),
            sink: Sink::Raw {
                out: abs(&std::env::temp_dir().join("run").to_string_lossy()),
            },
            tee: None,
            within: None,
            workspace: None,
            cli_version: None,
            cli_env: BTreeMap::new(),
            seed: None,
            prompt_override: None,
            recorded_command: None,
            daemon: None,
            first_turn: Argv::resolved(["kimi", "-p", "go"]).expect("resolved"),
            second_turn: None,
        }
    }

    #[test]
    fn every_placeholder_is_spelled_once_and_braced() {
        let spelled: std::collections::BTreeSet<_> =
            Placeholder::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            spelled.len(),
            Placeholder::ALL.len(),
            "two share a spelling"
        );
        for s in spelled {
            assert!(s.starts_with('{') && s.ends_with('}') && s.len() > 2, "{s}");
        }
    }

    #[test]
    fn a_word_spelling_a_placeholder_is_not_resolved() {
        for p in Placeholder::ALL {
            let word = format!("--x={}", p.as_str());
            assert!(Resolved::try_from(word.as_str()).is_err(), "{word}");
            let json = serde_json::json!(word);
            assert!(serde_json::from_value::<Resolved>(json).is_err(), "{word}");
        }
        assert!(Resolved::try_from("{unrelated}").is_ok());
    }

    #[test]
    fn the_session_is_the_one_hole_and_only_a_second_turn_may_hold_it() {
        let turn: Argv<Word> =
            serde_json::from_str(r#"[{"lit":"kimi"},"session"]"#).expect("parse");
        let words: Vec<String> = turn.words().iter().map(|w| w.filled("s-1")).collect();
        assert_eq!(words, ["kimi", "s-1"]);
        let mut p = plan();
        let mut json = serde_json::to_value(&p).expect("json");
        json["first_turn"] = serde_json::json!(["kimi", "{session}"]);
        assert!(serde_json::from_value::<CapturePlan>(json).is_err());
        p.second_turn = Some(turn);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn a_literal_word_cannot_smuggle_the_session_in() {
        let lit = r#"[{"lit":"{session}"}]"#;
        assert!(serde_json::from_str::<Argv<Word>>(lit).is_err());
    }

    #[test]
    fn a_command_line_needs_a_program() {
        assert!(Argv::<Resolved>::resolved(Vec::<String>::new()).is_err());
        assert!(serde_json::from_str::<Argv<Resolved>>("[]").is_err());
    }

    #[test]
    fn a_relative_path_is_not_absolute() {
        assert!(AbsPath::try_from(Path::new("rel/dir")).is_err());
        assert!(serde_json::from_str::<AbsPath>(r#""rel/dir""#).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn a_verbatim_windows_path_round_trips() {
        let p = abs(r"\\?\C:\ProgramData\pixtuoid-run-x\proj");
        let back: AbsPath =
            serde_json::from_str(&serde_json::to_string(&p).expect("ser")).expect("de");
        assert_eq!(back, p);
    }

    #[test]
    fn a_plan_round_trips_through_its_file() {
        let dir = tempfile::tempdir().expect("dir");
        let mut p = plan();
        p.cli_env.insert("A".into(), "b".into());
        let file = p.write_to(dir.path()).expect("write");
        assert_eq!(file, dir.path().join(CAPTURE_PLAN));
        assert_eq!(CapturePlan::read(&file).expect("read"), p);
    }

    #[test]
    fn a_field_the_plan_does_not_have_is_refused() {
        let mut json = serde_json::to_value(plan()).expect("json");
        json["a_flag_from_another_build"] = serde_json::json!(true);
        assert!(serde_json::from_value::<CapturePlan>(json).is_err());
    }

    #[test]
    fn a_committed_capture_records_one_turn() {
        let mut p = plan();
        p.sink = Sink::Committed;
        p.second_turn = Some(Argv::try_from(vec![Word::Session]).expect("argv"));
        assert!(p.validate().is_err());
        assert!(
            p.write_to(tempfile::tempdir().expect("dir").path())
                .is_err()
        );
    }
}
