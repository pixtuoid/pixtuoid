//! The plan a capture runs from: everything the recorder (`capture_fixture`)
//! needs to run one scenario, resolved on the host before the recorder starts.
//!
//! The hermetic runner writes it into the run dir and the
//! recorder reads it back; an operator's `just capture-fixture` line builds the
//! same type in memory. So the recorder has one
//! input, and a word the host left unexpanded cannot be spelled in it: [`Resolved`]
//! refuses a placeholder, and [`Word::Session`] is the one hole, filled after the
//! first turn.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use strum::VariantArray;
use vec1::Vec1;

use crate::harness::provenance::NonEmpty;

/// A run dir's plan, beside its [`CAPTURE_RECORD`](crate::harness::CAPTURE_RECORD).
pub(crate) const CAPTURE_PLAN: &str = "capture-plan.json";

/// One prompt for every source, so captures stay comparable: reading the SAME
/// file twice around a list forces both shapes the composed fixtures got wrong —
/// tools that interleave, and a tool id that repeats.
pub const DEFAULT_PROMPT: &str =
    "Read NOTE.txt, then list this directory, then read NOTE.txt again.";
/// The operator's prompt for a scenario [`DEFAULT_PROMPT`] cannot reach (a
/// permission gate needs a tool the CLI refuses to run unasked).
pub(crate) const PROMPT_ENV: &str = "CAPTURE_PROMPT";
/// A directory the operator's capture copies into its workspace first.
pub(crate) const SEED_ENV: &str = "CAPTURE_SEED";
/// Where macOS keeps a home's login keychain: the one part of the operator's
/// home a billed capture's sandbox links, so a CLI that keeps its login there
/// authenticates. cursor-agent 2026.10.22 runs `/usr/bin/security
/// find-generic-password`, naming no keychain (its bundle's `index.js:414`,
/// module `../keychain/dist/index.js`; security(1)). That the search list
/// then follows `$HOME` is observed, not documented: `cursor-agent status`
/// logs in through this link alone.
pub const KEYCHAINS: &str = "Library/Keychains";

/// A word a profile, script or config spells for the host to fill in. Each
/// spelling here is the one every expansion site and every check reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::VariantArray)]
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
    /// The same, with `/` separators, which a JSON string holds unescaped: how
    /// Claude Code keys its projects on Windows (`Xz` in the 2.1.302 bundle).
    WorkspaceSlashed,
    /// The operator's user name.
    User,
    /// The note a tool reads.
    Note,
    /// A reply the user cancels while it streams.
    Stream,
    /// The platform's shell tool.
    ShellTool,
}

impl Placeholder {
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
            Self::Stream => "{stream}",
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
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Resolved {
    type Error = PlanError;

    fn try_from(word: String) -> Result<Self, PlanError> {
        match Placeholder::VARIANTS
            .iter()
            .find(|p| word.contains(p.as_str()))
        {
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

impl TryFrom<String> for Word {
    type Error = PlanError;

    fn try_from(word: String) -> Result<Self, PlanError> {
        if word == Placeholder::Session.as_str() {
            Ok(Self::Session)
        } else {
            Resolved::try_from(word).map(Self::Lit)
        }
    }
}

/// A command line: a program, then its arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Argv<T>(Vec1<T>);

impl<T> Argv<T> {
    /// The words, in order.
    pub fn words(&self) -> &[T] {
        self.0.as_slice()
    }
}

impl<T> TryFrom<Vec<T>> for Argv<T> {
    type Error = PlanError;

    fn try_from(words: Vec<T>) -> Result<Self, PlanError> {
        Vec1::try_from_vec(words)
            .map(Self)
            .map_err(|_| PlanError::new("a command line with no program"))
    }
}

impl Argv<Resolved> {
    /// `words`, each finished.
    ///
    /// # Errors
    ///
    /// A word spells a placeholder, or there is none.
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Sink {
    /// The raw capture, unstripped, with no provenance: drift evidence, never
    /// committed.
    Raw {
        /// The directory it goes in.
        out: AbsPath,
        /// A second turn, run once the first's CLI exited.
        second_turn: Option<Argv<Word>>,
    },
    /// The committed fixture the scenario names, from one turn.
    Committed {
        /// The operator's argv, space-joined, with [`Placeholder::Prompt`]
        /// unexpanded: what the provenance records, never run.
        command: NonEmpty,
        /// The operator's prompt, when it replaced [`DEFAULT_PROMPT`].
        prompt: Option<NonEmpty>,
    },
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
    /// A server run around the CLI.
    pub daemon: Option<DaemonPlan>,
    /// The first turn.
    pub first_turn: Argv<Resolved>,
    /// What stops the server the CLI started for its turns, run after them.
    pub server_stop: Option<Argv<Resolved>>,
    /// The operator a billed capture runs for: none for the hermetic tier's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<Operator>,
}

/// The operator a billed capture runs for, read in their own env before the
/// recorder moves every home into its sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operator {
    /// Their identity, which no capture may hold.
    pub identity: BTreeSet<String>,
    /// Their [`KEYCHAINS`], which the sandbox's home links.
    pub keychains: Option<AbsPath>,
}

impl Operator {
    /// The operator whose identity is `identity` and whose home, if any, is
    /// `home`: its [`KEYCHAINS`] only on macOS, the one OS that keeps them.
    pub fn new(identity: BTreeSet<String>, home: Option<&str>) -> Self {
        Self {
            identity,
            keychains: home
                .filter(|_| cfg!(target_os = "macos"))
                .and_then(|home| AbsPath::try_from(Path::new(home).join(KEYCHAINS)).ok()),
        }
    }
}

impl CapturePlan {
    /// Whether every home the CLI resolves points into [`CapturePlan::within`],
    /// so every transcript born there is the run's.
    pub fn sandboxed(&self) -> bool {
        self.within.is_some()
    }

    /// Whether the CLI talks to the hermetic tier's scripted model, never an
    /// operator's account: its logs hold no credential, and its turn has the
    /// budget measured on such turns.
    pub fn scripted(&self) -> bool {
        self.sandboxed() && self.operator.is_none()
    }

    /// The second turn, for a raw capture that has one.
    pub fn second_turn(&self) -> Option<&Argv<Word>> {
        match &self.sink {
            Sink::Raw { second_turn, .. } => second_turn.as_ref(),
            Sink::Committed { .. } => None,
        }
    }

    /// Write it into `dir`, as the file the recorder is started on.
    ///
    /// # Errors
    ///
    /// The file cannot be written.
    pub fn write_to(&self, dir: &Path) -> std::io::Result<PathBuf> {
        let file = dir.join(CAPTURE_PLAN);
        std::fs::write(&file, serde_json::to_string_pretty(self)?)?;
        Ok(file)
    }

    /// The plan `file` holds.
    ///
    /// # Errors
    ///
    /// The file cannot be read or is not a plan.
    pub fn read(file: &Path) -> std::io::Result<Self> {
        super::read_json(file)
    }
}

/// What one run of the recorder asks of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Strip every committed capture in place.
    StripCorpus,
    /// Write a hermetic run into the fixture its scenario names.
    Promote(PathBuf),
    /// Create a source's transcript root inside `within`, with no CLI run: a
    /// user who ran the CLI before has it.
    PrepareRoot {
        /// The source whose root it is.
        source: String,
        /// The sandbox the root must lie in.
        within: AbsPath,
    },
    /// Run the capture a runner wrote.
    Plan(PathBuf),
    /// Run a capture an operator spelled.
    Capture(Box<CapturePlan>),
}

impl Invocation {
    /// What an operator gives a capture to keep it raw, as drift evidence.
    pub(crate) const OUT: &'static str = "--out";
    /// Strips every committed capture.
    pub(crate) const STRIP_CORPUS: &'static str = "--strip-corpus";
    /// Runs the plan in the file that follows.
    pub(crate) const PLAN: &'static str = "--plan";
    /// Promotes the run dir that follows.
    pub(crate) const PROMOTE: &'static str = "promote";
    /// Prepares a source's root inside the [`Invocation::WITHIN`] dir that follows.
    pub(crate) const PREPARE_ROOT: &'static str = "prepare-root";
    /// The sandbox a prepared root must lie in.
    pub(crate) const WITHIN: &'static str = "--within";

    /// The line that runs the plan in `file`.
    pub fn plan_args(file: &Path) -> Vec<OsString> {
        vec![Self::PLAN.into(), file.into()]
    }

    /// The line that promotes the run in `run`.
    pub fn promote_args(run: &Path) -> Vec<OsString> {
        vec![Self::PROMOTE.into(), run.into()]
    }

    /// The line that creates `source`'s transcript root inside `within`.
    pub fn prepare_root_args(within: &Path, source: &str) -> Vec<OsString> {
        vec![
            Self::PREPARE_ROOT.into(),
            Self::WITHIN.into(),
            within.into(),
            source.into(),
        ]
    }

    /// What the recorder prints for a line that asks none of these.
    pub fn usage() -> String {
        format!(
            "usage: capture_fixture [{out} <dir>] <source-id> <scenario> <cmd...>   ('{prompt}' expands)\n       \
             capture_fixture {strip}   (strip every committed capture in place)\n       \
             capture_fixture {promote} <run>   (write a hermetic run into its fixture)\n       \
             capture_fixture {prepare} {within} <dir> <source-id>   (create its root; no CLI)\n       \
             capture_fixture {plan} <file>   (run a plan the hermetic tier wrote)",
            out = Self::OUT,
            prompt = Placeholder::Prompt.as_str(),
            strip = Self::STRIP_CORPUS,
            promote = Self::PROMOTE,
            prepare = Self::PREPARE_ROOT,
            within = Self::WITHIN,
            plan = Self::PLAN,
        )
    }

    /// What `args` (after the program) ask, with the process's own env, which is
    /// the operator's.
    ///
    /// # Errors
    ///
    /// The line asks for none of these, or its operands are wrong.
    pub fn parse_process(args: &[String]) -> Result<Self, PlanError> {
        Self::parse(args, &crate::platform::text_env)
    }

    /// What `args` (after the program) ask. `env` is the operator's, read once
    /// here and nowhere after: a text variable, unset when absent.
    ///
    /// # Errors
    ///
    /// The line asks for none of these, or its operands are wrong.
    pub(crate) fn parse(
        args: &[String],
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, PlanError> {
        let usage = |flag: &str| {
            PlanError::new(if args.len() < 2 {
                format!("{flag} takes a value")
            } else {
                format!("{flag} takes one value, not {}", args.len() - 1)
            })
        };
        match args.first().map(String::as_str) {
            Some(Self::STRIP_CORPUS) if args.len() == 1 => Ok(Self::StripCorpus),
            Some(Self::PLAN) => match args {
                [_, file] => Ok(Self::Plan(PathBuf::from(file))),
                _ => Err(usage(Self::PLAN)),
            },
            Some(Self::PROMOTE) => match args {
                [_, run] => Ok(Self::Promote(PathBuf::from(run))),
                _ => Err(usage(Self::PROMOTE)),
            },
            Some(Self::PREPARE_ROOT) => match args {
                [_, flag, within, source] if flag == Self::WITHIN => Ok(Self::PrepareRoot {
                    source: source.clone(),
                    within: AbsPath::try_from(PathBuf::from(within))?,
                }),
                _ => Err(PlanError::new(format!(
                    "{} takes {} <dir> <source> and nothing else",
                    Self::PREPARE_ROOT,
                    Self::WITHIN
                ))),
            },
            _ => CapturePlan::from_operator(args, env).map(|p| Self::Capture(Box::new(p))),
        }
    }
}

impl CapturePlan {
    /// The capture an operator's line asks: an optional [`Invocation::OUT`] and
    /// its dir, then `<source> <scenario> <cmd...>`, [`Placeholder::Prompt`] in
    /// the command filled from [`PROMPT_ENV`] or [`DEFAULT_PROMPT`]. It runs in memory: only the runner's plan is kept, as
    /// the record of its run.
    ///
    /// # Errors
    ///
    /// The line is short, a word spells a placeholder the host cannot fill, or the [`Invocation::OUT`] dir is unusable.
    pub(crate) fn from_operator(
        args: &[String],
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, PlanError> {
        let (out, rest) = match args {
            [flag, out, rest @ ..] if flag == Invocation::OUT => (
                Some(AbsPath::try_from(std::path::absolute(out).map_err(
                    |e| PlanError::new(format!("{} {out}: {e}", Invocation::OUT)),
                )?)?),
                rest,
            ),
            _ => (None, args),
        };
        let [source, scenario, command @ ..] = rest else {
            return Err(PlanError::new(SHORT));
        };
        if command.is_empty() {
            return Err(PlanError::new(SHORT));
        }
        // Set-but-blank is unset: a blank prompt would expand [`Placeholder::Prompt`] to nothing.
        let text = |key: &str| env(key).and_then(|v| NonEmpty::try_from(v).ok());
        let prompt = text(PROMPT_ENV);
        let words = prompt.as_ref().map_or(DEFAULT_PROMPT, NonEmpty::as_str);
        let first_turn = Argv::resolved(
            command
                .iter()
                .map(|w| w.replace(Placeholder::Prompt.as_str(), words)),
        )?;
        let sink = match out {
            Some(out) => Sink::Raw {
                out,
                second_turn: None,
            },
            None => Sink::Committed {
                command: NonEmpty::try_from(command.join(" ")).map_err(PlanError::new)?,
                prompt,
            },
        };
        Ok(Self {
            source: source.clone(),
            scenario: scenario.clone(),
            sink,
            tee: None,
            within: None,
            workspace: None,
            cli_version: None,
            cli_env: BTreeMap::new(),
            seed: text(SEED_ENV),
            daemon: None,
            first_turn,
            server_stop: None,
            operator: None,
        })
    }
}

const SHORT: &str = "a capture needs <source> <scenario> <cmd...>";

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
                second_turn: None,
            },
            tee: None,
            within: None,
            workspace: None,
            cli_version: None,
            cli_env: BTreeMap::new(),
            seed: None,
            daemon: None,
            first_turn: Argv::resolved(["kimi", "-p", "go"]).expect("resolved"),
            server_stop: None,
            operator: None,
        }
    }

    #[test]
    fn every_placeholder_is_spelled_once_and_braced() {
        let spelled: std::collections::BTreeSet<_> =
            Placeholder::VARIANTS.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            spelled.len(),
            Placeholder::VARIANTS.len(),
            "two share a spelling"
        );
        for s in spelled {
            assert!(s.starts_with('{') && s.ends_with('}') && s.len() > 2, "{s}");
        }
    }

    #[test]
    fn a_word_spelling_a_placeholder_is_not_resolved() {
        for p in Placeholder::VARIANTS {
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
        let mut json = serde_json::to_value(plan()).expect("json");
        json["first_turn"] = serde_json::json!(["kimi", "{session}"]);
        assert!(serde_json::from_value::<CapturePlan>(json).is_err());
        let mut p = plan();
        let Sink::Raw { second_turn, .. } = &mut p.sink else {
            panic!("a raw capture");
        };
        *second_turn = Some(turn.clone());
        assert_eq!(p.second_turn(), Some(&turn));
    }

    #[test]
    fn a_word_is_the_session_only_when_it_is_exactly_the_placeholder() {
        assert_eq!(
            Word::try_from(Placeholder::Session.as_str().to_owned()),
            Ok(Word::Session)
        );
        assert!(Word::try_from("--resume={session}".to_owned()).is_err());
        assert_eq!(
            Word::try_from("kimi".to_owned()),
            Ok(Word::Lit(Resolved::try_from("kimi").expect("resolved")))
        );
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
    fn a_field_the_sink_does_not_have_is_refused() {
        let mut json = serde_json::to_value(plan()).expect("json");
        json["sink"]["raw"]["a_flag_from_another_build"] = serde_json::json!(true);
        assert!(serde_json::from_value::<CapturePlan>(json).is_err());
    }

    #[test]
    fn a_field_the_plan_does_not_have_is_refused() {
        let mut json = serde_json::to_value(plan()).expect("json");
        json["a_flag_from_another_build"] = serde_json::json!(true);
        assert!(serde_json::from_value::<CapturePlan>(json).is_err());
    }

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn an_operators_line_fills_the_prompt_and_records_what_they_typed() {
        let plan = CapturePlan::from_operator(
            &args(&["kimi", "tool-run", "kimi", "-p", "{prompt}"]),
            &env_of(&[]),
        )
        .expect("plan");
        assert_eq!(plan.first_turn.strings(), ["kimi", "-p", DEFAULT_PROMPT]);
        let Sink::Committed { command, prompt } = &plan.sink else {
            panic!("a committed capture");
        };
        assert_eq!(command.as_str(), "kimi -p {prompt}");
        assert_eq!(prompt, &None);
    }

    #[test]
    fn a_flag_after_the_positionals_is_the_clis() {
        let plan = CapturePlan::from_operator(
            &args(&["kimi", "tool-run", "kimi", "--out", "x"]),
            &env_of(&[]),
        )
        .expect("plan");
        assert!(matches!(plan.sink, Sink::Committed { .. }));
        assert_eq!(plan.first_turn.strings(), ["kimi", "--out", "x"]);
    }

    #[test]
    fn an_operators_keychains_are_their_homes_on_macos_alone() {
        let home = std::env::temp_dir().join("ada");
        let operator = Operator::new(BTreeSet::new(), home.to_str());
        assert_eq!(
            operator.keychains.as_ref().map(AbsPath::as_path),
            cfg!(target_os = "macos")
                .then(|| home.join(KEYCHAINS))
                .as_deref()
        );
        assert_eq!(Operator::new(BTreeSet::new(), None).keychains, None);
    }

    #[test]
    fn only_a_sandboxed_run_no_operator_reaches_is_scripted() {
        let sandboxed = CapturePlan {
            within: Some(abs(&std::env::temp_dir().join("sandbox").to_string_lossy())),
            ..plan()
        };
        assert!(sandboxed.scripted());
        let billed = CapturePlan {
            operator: Some(Operator::new(BTreeSet::new(), None)),
            ..sandboxed.clone()
        };
        assert!(billed.sandboxed() && !billed.scripted());
        assert!(!plan().scripted(), "a home outside any sandbox");
    }

    #[test]
    fn an_operators_prompt_and_seed_come_from_their_env_and_a_blank_one_is_unset() {
        let line = args(&["kimi", "gate", "kimi", "-p", "{prompt}"]);
        let plan = CapturePlan::from_operator(
            &line,
            &env_of(&[(PROMPT_ENV, "rm NOTE.txt"), (SEED_ENV, "/seed")]),
        )
        .expect("plan");
        assert_eq!(plan.first_turn.strings(), ["kimi", "-p", "rm NOTE.txt"]);
        let Sink::Committed { prompt, .. } = &plan.sink else {
            panic!("a committed capture");
        };
        assert_eq!(prompt.as_ref().map(NonEmpty::as_str), Some("rm NOTE.txt"));
        assert_eq!(plan.seed.as_ref().map(NonEmpty::as_str), Some("/seed"));
        let blank =
            CapturePlan::from_operator(&line, &env_of(&[(PROMPT_ENV, "  "), (SEED_ENV, "")]))
                .expect("plan");
        assert_eq!(blank.first_turn.strings()[2], DEFAULT_PROMPT);
        let Sink::Committed { prompt, .. } = &blank.sink else {
            panic!("a committed capture");
        };
        assert_eq!((prompt, blank.seed), (&None, None));
    }

    #[test]
    fn an_operators_out_makes_a_raw_capture_in_an_absolute_dir() {
        let plan = CapturePlan::from_operator(
            &args(&["--out", "evidence", "kimi", "tool-run", "kimi"]),
            &env_of(&[]),
        )
        .expect("plan");
        let Sink::Raw { out, .. } = plan.sink else {
            panic!("a raw capture");
        };
        assert!(out.as_path().is_absolute() && out.as_path().ends_with("evidence"));
    }

    #[test]
    fn an_operators_line_with_a_placeholder_the_host_cannot_fill_is_refused() {
        let line = args(&["kimi", "tool-run", "kimi", "--port", "{port}"]);
        assert!(CapturePlan::from_operator(&line, &env_of(&[])).is_err());
    }

    #[test]
    fn an_operators_line_needs_a_source_a_scenario_and_a_command() {
        for short in [
            &[][..],
            &["kimi"],
            &["kimi", "tool-run"],
            &["--out", "d", "kimi", "x"],
        ] {
            assert!(
                CapturePlan::from_operator(&args(short), &env_of(&[])).is_err(),
                "{short:?}"
            );
        }
    }

    #[test]
    fn the_other_invocations_take_exactly_their_operands() {
        let parse = |w: &[&str]| Invocation::parse(&args(w), &env_of(&[]));
        assert_eq!(parse(&["--strip-corpus"]), Ok(Invocation::StripCorpus));
        assert_eq!(
            parse(&["--plan", "p.json"]),
            Ok(Invocation::Plan("p.json".into()))
        );
        assert_eq!(
            parse(&["promote", "run"]),
            Ok(Invocation::Promote("run".into()))
        );
        let root = std::env::temp_dir();
        let within = root.to_string_lossy();
        assert_eq!(
            parse(&["prepare-root", "--within", &within, "codex"]),
            Ok(Invocation::PrepareRoot {
                source: "codex".into(),
                within: AbsPath::try_from(root.clone()).expect("abs"),
            })
        );
        for bad in [
            &["--strip-corpus", "x"][..],
            &["--plan"],
            &["promote"],
            &["promote", "a", "b"],
            &["prepare-root", "codex"],
            &["prepare-root", "--within", "rel", "codex"],
            &["prepare-root", "--within", &within, "codex", "x"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_missing_operand_and_an_extra_one_are_told_apart() {
        let why = |w: &[&str]| {
            Invocation::parse(&args(w), &env_of(&[]))
                .expect_err("refused")
                .to_string()
        };
        for flag in ["--plan", "promote"] {
            assert_eq!(why(&[flag]), format!("{flag} takes a value"));
            assert_eq!(
                why(&[flag, "a", "b"]),
                format!("{flag} takes one value, not 2")
            );
        }
    }

    #[test]
    fn the_runners_lines_parse_back_to_what_built_them() {
        let (dir, source) = (std::env::temp_dir(), "codex");
        let line = |argv: Vec<OsString>| {
            argv.into_iter()
                .map(|w| w.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let parse = |argv: Vec<OsString>| Invocation::parse(&line(argv), &env_of(&[]));
        assert_eq!(
            parse(Invocation::plan_args(&dir)),
            Ok(Invocation::Plan(dir.clone()))
        );
        assert_eq!(
            parse(Invocation::promote_args(&dir)),
            Ok(Invocation::Promote(dir.clone()))
        );
        assert_eq!(
            parse(Invocation::prepare_root_args(&dir, source)),
            Ok(Invocation::PrepareRoot {
                source: source.into(),
                within: AbsPath::try_from(dir).expect("absolute"),
            })
        );
    }

    #[test]
    fn the_usage_names_every_spelling_the_parse_takes() {
        let usage = Invocation::usage();
        for word in [
            Invocation::OUT,
            Invocation::STRIP_CORPUS,
            Invocation::PLAN,
            Invocation::PROMOTE,
            Invocation::PREPARE_ROOT,
            Invocation::WITHIN,
            Placeholder::Prompt.as_str(),
        ] {
            assert!(usage.contains(word), "{word} in {usage}");
        }
    }

    /// The recorder takes its inputs from the plan, so a direct read of the
    /// process env or of `platform` in `capture_fixture.rs` is a second channel,
    /// bar two that read an env that is not an input: the scrub of the CLI's
    /// inherited agent env, and the operator's own identity, kept out of the
    /// bytes it writes. Every `std::env` item is a read but the ones that name
    /// no variable.
    #[test]
    fn the_recorder_reads_no_env_but_the_scrub_and_the_operators_identity() {
        use syn::visit::Visit;
        const NO_VARIABLE: [&str; 7] = [
            "args",
            "args_os",
            "current_dir",
            "current_exe",
            "join_paths",
            "split_paths",
            "EXE_SUFFIX",
        ];
        const EXEMPT: [(&str, &str); 3] = [
            ("wired", "vars_os"),
            ("identity_needles", "text_env"),
            ("operator", "text_env"),
        ];
        /// The names a `use` brings in from `std::env` or `platform`, which a bare
        /// call then reads the env by.
        #[derive(Default)]
        struct Imports(std::collections::BTreeSet<String>);
        impl Imports {
            fn tree(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>) {
                let reads_env = |prefix: &[String], last: &str| {
                    prefix.iter().any(|n| n == "platform" || n == "env")
                        && !NO_VARIABLE.contains(&last)
                };
                match tree {
                    syn::UseTree::Path(p) => {
                        prefix.push(p.ident.to_string());
                        self.tree(&p.tree, prefix);
                        prefix.pop();
                    }
                    syn::UseTree::Name(n) if reads_env(prefix, &n.ident.to_string()) => {
                        self.0.insert(n.ident.to_string());
                    }
                    syn::UseTree::Rename(r) if reads_env(prefix, &r.ident.to_string()) => {
                        self.0.insert(r.rename.to_string());
                    }
                    syn::UseTree::Group(g) => {
                        g.items.iter().for_each(|t| self.tree(t, prefix));
                    }
                    _ => {}
                }
            }
        }
        impl<'ast> Visit<'ast> for Imports {
            fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
                self.tree(&u.tree, &mut Vec::new());
            }
        }
        struct Reads {
            imported: std::collections::BTreeSet<String>,
            in_fn: Vec<String>,
            found: Vec<String>,
        }
        impl Reads {
            fn note(&mut self, path: &syn::Path) {
                let names: Vec<String> =
                    path.segments.iter().map(|s| s.ident.to_string()).collect();
                let Some(last) = names.last() else {
                    return;
                };
                let reads = names.iter().any(|n| n == "platform")
                    || (names.iter().any(|n| n == "env") && !NO_VARIABLE.contains(&last.as_str()));
                let exempt = self
                    .in_fn
                    .last()
                    .is_some_and(|f| EXEMPT.contains(&(f.as_str(), last.as_str())));
                if reads && !exempt {
                    self.found.push(names.join("::"));
                }
            }
        }
        impl<'ast> Visit<'ast> for Reads {
            fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
                if m.ident != "tests" {
                    syn::visit::visit_item_mod(self, m);
                }
            }
            fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
                self.in_fn.push(f.sig.ident.to_string());
                syn::visit::visit_item_fn(self, f);
                self.in_fn.pop();
            }
            fn visit_expr_path(&mut self, e: &'ast syn::ExprPath) {
                if e.path.segments.len() > 1 {
                    self.note(&e.path);
                }
                syn::visit::visit_expr_path(self, e);
            }
            fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
                // A call through a name `use` brought in: `text_env(..)`.
                if let syn::Expr::Path(p) = &*c.func
                    && p.path.segments.len() == 1
                {
                    let name = p.path.segments[0].ident.to_string();
                    let exempt = self
                        .in_fn
                        .last()
                        .is_some_and(|f| EXEMPT.contains(&(f.as_str(), name.as_str())));
                    if self.imported.contains(&name) && !exempt {
                        self.found.push(name);
                    }
                }
                syn::visit::visit_expr_call(self, c);
            }
            fn visit_macro(&mut self, m: &'ast syn::Macro) {
                // `format!`-like bodies are expressions; one that is not is no env read.
                if let Ok(args) = m.parse_body_with(
                    syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
                ) {
                    args.iter().for_each(|e| self.visit_expr(e));
                }
            }
        }
        let file =
            syn::parse_file(include_str!("../../examples/capture_fixture.rs")).expect("parses");
        let mut imports = Imports::default();
        imports.visit_file(&file);
        let mut reads = Reads {
            imported: imports.0,
            in_fn: Vec::new(),
            found: Vec::new(),
        };
        reads.visit_file(&file);
        assert!(
            reads.found.is_empty(),
            "the recorder reads env: {:?}",
            reads.found
        );
    }
}
