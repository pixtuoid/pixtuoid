//! The interactive driver's decisions, over the screen a pty renders.
//!
//! Some wire exists only in a CLI's TUI: Claude Code's headless mode refuses a
//! tool without firing `PermissionRequest`. The `tui_drive` example runs the CLI under a pty and
//! asks [`Driver::step`] what to do with the screen it now shows; everything
//! that differs between CLIs is its [`Tui`] row, never code here. A new CLI's
//! edge case is that row plus a test over a screen its run recorded
//! (`tests/sources/<module>/screens/`).

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long the composer may still hold the prompt after an Enter before it is
/// sent again: an Enter during startup is swallowed.
const ENTER_RETRY: Duration = Duration::from_secs(1);
/// How long the screen must stay unchanged after the turn before the driver
/// quits, short of the idle notifiers a CLI fires at an idle composer.
const SETTLE: Duration = Duration::from_secs(3);
/// How long an unchanged screen may stand before a turn that never finished is
/// given up: an unrecognised dialog stalls a run, and its key is never guessed.
const STALL: Duration = Duration::from_secs(20);
/// How long one run may take, end to end, before `tui_drive` kills the CLI:
/// inside the recorder's [`super::TURN_BUDGET`], so the driver gives up first
/// and logs the screen it stood at.
pub const BUDGET: Duration = Duration::from_secs(120);

/// `tui_drive`'s exit when it gave up on a run it could not finish.
pub const GAVE_UP: i32 = 3;
/// `tui_drive`'s exit when it gave up on a submitted turn that never showed
/// what its [`Answer`] answers: a verdict on the CLI, not a run that could not
/// happen.
pub(crate) const UNANSWERED: i32 = 4;

/// A key the driver writes to the pty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Submits the composer or confirms a highlighted choice.
    Enter,
    /// Dismisses a dialog.
    Esc,
    /// Literal text, a slash command for one.
    Text(&'static str),
}

impl Key {
    /// The bytes a terminal sends for it.
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Enter => b"\r",
            Self::Esc => b"\x1b",
            Self::Text(text) => text.as_bytes(),
        }
    }
}

/// One of a CLI's confirmation dialogs.
#[derive(Debug)]
pub(crate) struct Gate {
    /// Text on screen only while the dialog is open.
    pub(crate) shows: &'static str,
    /// The keys the run answers it with: a refusal, unless its row is one
    /// that approves.
    pub(crate) keys: &'static [Key],
    /// How long it must stand before it is answered: a key it ignores is lost.
    pub(crate) dwell: Duration,
}

/// How a scenario's run answers its turn, and so when the turn is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    /// Answer its dialog with its gate's `keys`; the turn is over once
    /// answered and the screen settles.
    Gate,
    /// Cancel it with its row's `cancel` keys once the screen shows its reply
    /// streaming; the turn is over once cancelled and the screen settles.
    Cancel,
    /// Nothing: the turn raises no dialog, and is over once the screen settles
    /// after the prompt.
    Nothing,
}

impl Answer {
    /// What the screen must show before this answer is given, if anything.
    pub(crate) const fn awaits(self) -> Option<&'static str> {
        match self {
            Self::Gate => Some("a gate the driver knows to answer"),
            Self::Cancel => Some("its reply streaming for the driver to cancel"),
            Self::Nothing => None,
        }
    }
}

/// Where a CLI's composer is on screen.
#[derive(Debug)]
pub(crate) struct Composer {
    /// What its input line starts with, at the line's first column, or at any
    /// indent under [`Border::Footed`]. A dialog's highlighted choice and the
    /// conversation's echo of a prompt can start with it too.
    pub(crate) marker: &'static str,
    pub(crate) border: Border,
}

/// What a composer draws around its input line, which tells the input line
/// from the other marked lines.
#[derive(Debug)]
pub(crate) enum Border {
    /// A rule of `glyph` above it, between `corners` when the composer is
    /// boxed; no other marked line sits under it.
    Rule {
        glyph: char,
        corners: Option<(char, char)>,
    },
    /// A rule of `glyph` above it, which may carry a title, and another under
    /// it: the input line is the one under the screen's next-to-last rule,
    /// since a pair of them also borders the conversation's echo of a prompt.
    Rules { glyph: char },
    /// A box closed by a bottom edge opening with `edge`, the input line
    /// `rows` rows above the screen's last such edge, at any indent: the
    /// conversation's echo of a prompt wears the same marker, in a box with no
    /// edge.
    Footed { edge: char, rows: usize },
    /// Nothing: the input line is the screen's last marked line, which only a
    /// dialog drawn below it displaces, its numbered choice then the last.
    Nothing,
}

/// A CLI's interactive facts, one row per CLI.
#[derive(Debug)]
pub struct Tui {
    /// The interactive launch's arguments after the CLI's binary, mode flags
    /// included; no prompt.
    pub(crate) args: &'static [&'static str],
    pub(crate) composer: Composer,
    /// Every dialog the turn may raise; one per turn, since its script calls no
    /// tool after the gate's answer (`a_permission_script_calls_no_tool_after_its_gated_call`).
    /// One the table doesn't name stalls the run.
    pub(crate) gates: &'static [Gate],
    /// What cancels a running turn the way a user would, for a run that
    /// answers [`Answer::Cancel`]; empty for a CLI no scenario cancels.
    pub(crate) cancel: &'static [Key],
    /// What ends the session the way a user would, so its end hook fires.
    pub(crate) quit: &'static [Key],
    pub(crate) conpty: ConPty,
}

/// What a Windows leg has seen the TUI do under ConPTY.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ConPty {
    /// It renders, and a Windows leg drives it.
    Renders,
    /// It paints nothing, so no Windows leg drives it.
    PaintsNothing,
    /// No Windows leg has driven it yet, so none does until one shows that
    /// it renders.
    Unverified,
}

impl ConPty {
    /// Why a Windows leg doesn't drive it, if it doesn't.
    pub(crate) const fn not_run(self) -> Option<&'static str> {
        match self {
            Self::Renders => None,
            Self::PaintsNothing => Some("its TUI paints nothing under ConPTY"),
            Self::Unverified => Some("its TUI was never run under ConPTY"),
        }
    }
}

impl Tui {
    /// The interactive launch's arguments after the CLI's binary.
    pub fn args(&self) -> &'static [&'static str] {
        self.args
    }
}

/// What the driver does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Type the prompt; its Enter goes as a separate write, since a TUI reads
    /// text and newline sent together as a paste.
    Type(String),
    /// Send these keys.
    Keys(&'static [Key]),
    /// End the session.
    Quit(&'static [Key]),
    /// Nothing yet.
    Wait,
    /// Give up: the run cannot finish.
    Fail(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Boot,
    Typed,
    Submitted { enter_at: Instant },
    Quit,
}

/// What a run's answer waits for on screen, and how it is given.
#[derive(Debug, Clone, Copy)]
struct Cue {
    /// Text on screen while it stands.
    shows: &'static str,
    keys: &'static [Key],
    /// As [`Gate::dwell`].
    dwell: Duration,
}

/// A cue on screen, and when it appeared.
#[derive(Debug, Clone, Copy)]
struct Open {
    shows: &'static str,
    since: Instant,
}

/// The state machine one run steps through, its turn over once its `Answer`
/// says.
#[derive(Debug)]
pub struct Driver<'a> {
    tui: &'a Tui,
    answer: Answer,
    prompt: String,
    /// The prompt typed once the first turn settles, in the same session.
    then: Option<String>,
    phase: Phase,
    screen: String,
    changed_at: Instant,
    open: Option<Open>,
    answered: bool,
}

/// How many leading characters of the prompt identify it in the composer: a
/// long prompt wraps, and its first line is what the composer keeps.
const PROMPT_HEAD: usize = 16;

impl<'a> Driver<'a> {
    /// A run through `tui`, typing `prompt` and answering as `answer` says.
    pub(crate) fn new(tui: &'a Tui, answer: Answer, prompt: String, now: Instant) -> Self {
        Self {
            tui,
            answer,
            prompt,
            then: None,
            phase: Phase::Boot,
            screen: String::new(),
            changed_at: now,
            open: None,
            answered: false,
        }
    }

    /// The same run with a second turn, `prompt`, typed into its session.
    #[must_use]
    pub(crate) fn then(mut self, prompt: &str) -> Self {
        self.then = Some(prompt.to_owned());
        self
    }

    /// The next action for the screen as it now reads.
    pub fn step(&mut self, screen: &str, now: Instant) -> Action {
        if screen != self.screen {
            screen.clone_into(&mut self.screen);
            self.changed_at = now;
        }
        if self.phase == Phase::Quit {
            return Action::Wait;
        }
        if now.duration_since(self.changed_at) >= STALL {
            return Action::Fail("the screen stood still with the turn unfinished");
        }
        if let Some(action) = self.answer(now) {
            return action;
        }
        if self.dialog_open() {
            return Action::Wait;
        }
        match self.phase {
            Phase::Boot if self.composer().is_some() => {
                self.phase = Phase::Typed;
                Action::Type(self.prompt.clone())
            }
            Phase::Typed => {
                self.phase = Phase::Submitted { enter_at: now };
                Action::Keys(&[Key::Enter])
            }
            // A turn already answered was submitted: a prompt shown after its
            // answer is the conversation's echo.
            Phase::Submitted { enter_at } if !self.answered && self.composer_holds_prompt() => {
                if now.duration_since(enter_at) < ENTER_RETRY {
                    return Action::Wait;
                }
                self.phase = Phase::Submitted { enter_at: now };
                Action::Keys(&[Key::Enter])
            }
            // The screen must move after the Enter: one standing since before
            // it may hold a prompt the CLI dropped, its command never run. A
            // frame read in the Enter's own step predates the Enter.
            Phase::Submitted { enter_at }
                if !self.owes_answer()
                    && self.changed_at > enter_at
                    && now.duration_since(self.changed_at) >= SETTLE =>
            {
                if let Some(next) = self.then.take() {
                    self.prompt = next;
                    self.phase = Phase::Typed;
                    (self.answered, self.open) = (false, None);
                    return Action::Type(self.prompt.clone());
                }
                self.phase = Phase::Quit;
                Action::Quit(self.tui.quit)
            }
            _ => Action::Wait,
        }
    }

    /// What `tui_drive` exits with if it gives up now.
    pub fn gave_up(&self) -> i32 {
        match self.phase {
            Phase::Submitted { .. } if self.owes_answer() => UNANSWERED,
            _ => GAVE_UP,
        }
    }

    /// Whether the turn still waits on what its answer answers.
    fn owes_answer(&self) -> bool {
        self.answer.awaits().is_some() && !self.answered
    }

    /// Its answer's cue on screen, if one stands: for a gate the first dialog
    /// its row lists, for a cancel the reply's first line.
    fn cue(&self) -> Option<Cue> {
        match self.answer {
            Answer::Gate => self
                .tui
                .gates
                .iter()
                .find(|g| self.screen.contains(g.shows))
                .map(|g| Cue {
                    shows: g.shows,
                    keys: g.keys,
                    dwell: g.dwell,
                }),
            Answer::Cancel => self.screen.contains(super::STREAM_CUE).then_some(Cue {
                shows: super::STREAM_CUE,
                keys: self.tui.cancel,
                dwell: Duration::ZERO,
            }),
            Answer::Nothing => None,
        }
    }

    /// Its answer, once its cue has stood its dwell after the prompt was
    /// submitted; a cue before that stalls the run. Once per turn: a frame
    /// drawn before the answer landed still shows its cue, and a second key
    /// would land on the turn.
    fn answer(&mut self, now: Instant) -> Option<Action> {
        if self.answered {
            return None;
        }
        let Some(cue) = self.cue() else {
            self.open = None;
            return None;
        };
        if !matches!(self.phase, Phase::Submitted { .. }) {
            return Some(Action::Wait);
        }
        let open = match self.open {
            Some(open) if open.shows == cue.shows => open,
            _ => Open {
                shows: cue.shows,
                since: now,
            },
        };
        self.open = Some(open);
        if now.duration_since(open.since) < cue.dwell {
            return Some(Action::Wait);
        }
        self.answered = true;
        Some(Action::Keys(cue.keys))
    }

    /// Whether a dialog its row lists stands ([`Tui::gates`]): nothing else is
    /// sent then, so no Enter can land on it and pick its first choice, and
    /// one its answer leaves standing stalls the run like any dialog nobody
    /// planned for.
    fn dialog_open(&self) -> bool {
        self.tui.gates.iter().any(|g| self.screen.contains(g.shows))
    }

    /// The text after the composer's marker on its input line.
    fn composer(&self) -> Option<&str> {
        let composer = &self.tui.composer;
        let lines: Vec<&str> = self.screen.lines().collect();
        match composer.border {
            Border::Rule { glyph, corners } => lines.windows(2).rev().find_map(|pair| {
                is_rule(pair[0], glyph, corners)
                    .then(|| pair[1].strip_prefix(composer.marker))
                    .flatten()
            }),
            Border::Rules { glyph } => {
                let (top, _) = lines
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(|(_, l)| is_titled_rule(l, glyph))
                    .nth(1)?;
                lines.get(top + 1)?.strip_prefix(composer.marker)
            }
            Border::Footed { edge, rows } => {
                let bottom = lines
                    .iter()
                    .rposition(|l| l.trim_start().starts_with(edge))?;
                lines
                    .get(bottom.checked_sub(rows)?)?
                    .trim_start()
                    .strip_prefix(composer.marker)
            }
            Border::Nothing => {
                let typed = lines
                    .iter()
                    .rev()
                    .find_map(|l| l.strip_prefix(composer.marker))?;
                (!is_choice(typed)).then_some(typed)
            }
        }
    }

    fn composer_holds_prompt(&self) -> bool {
        let head: String = self.prompt.chars().take(PROMPT_HEAD).collect();
        self.composer()
            .is_some_and(|typed| typed.trim_start().starts_with(head.trim_end()))
    }
}

/// Whether `line` is a rule of `glyph`, between `corners` when it has them.
fn is_rule(line: &str, glyph: char, corners: Option<(char, char)>) -> bool {
    let mut line = line.trim_end();
    if let Some((left, right)) = corners {
        let Some(inner) = line
            .trim_start()
            .strip_prefix(left)
            .and_then(|l| l.strip_suffix(right))
        else {
            return false;
        };
        line = inner;
    }
    !line.is_empty() && line.chars().all(|c| c == glyph)
}

/// Whether `line` opens and closes with `glyph`, with a title or nothing more
/// between.
fn is_titled_rule(line: &str, glyph: char) -> bool {
    let line = line.trim_end();
    line.starts_with(glyph) && line.ends_with(glyph)
}

/// Whether the text after a marker is a dialog's numbered choice (`1. Yes`).
fn is_choice(typed: &str) -> bool {
    typed
        .trim_start()
        .split_once(". ")
        .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// `tui_drive`'s command line, which the recorder runs in place of the CLI.
#[derive(Debug, PartialEq, Eq)]
pub struct CommandLine {
    /// The profiled source.
    pub source: String,
    /// The scenario's name.
    pub scenario: String,
    /// Where the pty log goes.
    pub log: PathBuf,
    /// What the driver types.
    pub prompt: String,
    /// The CLI's launch and interactive arguments.
    pub program: Vec<OsString>,
}

/// Separates the driver's operands from the CLI's own command line.
const PROGRAM: &str = "--";

impl CommandLine {
    /// Its arguments, after `tui_drive`'s own path.
    pub fn to_args(&self) -> Vec<OsString> {
        [self.source.as_str(), self.scenario.as_str()]
            .map(OsString::from)
            .into_iter()
            .chain([self.log.clone().into_os_string()])
            .chain([OsString::from(&self.prompt), OsString::from(PROGRAM)])
            .chain(self.program.iter().cloned())
            .collect()
    }

    /// The command line `args` spells, or `None` when it spells none.
    pub fn parse(args: &[OsString]) -> Option<Self> {
        let split = args.iter().position(|a| a == PROGRAM)?;
        let [source, scenario, log, prompt] = &args[..split] else {
            return None;
        };
        let program = args[split + 1..].to_vec();
        if program.is_empty() {
            return None;
        }
        Some(Self {
            source: source.to_str()?.to_owned(),
            scenario: scenario.to_str()?.to_owned(),
            log: PathBuf::from(log),
            prompt: prompt.to_str()?.to_owned(),
            program,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Second, Surface, profile};
    use super::*;

    /// A screen a CLI's TUI run recorded, beside its captures.
    macro_rules! screen {
        ($module:literal, $name:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/sources/",
                $module,
                "/screens/",
                $name,
                ".txt"
            ))
        };
    }

    const BOOT: &str = screen!("claude_code", "boot");
    const READY: &str = screen!("claude_code", "ready");
    const GATE: &str = screen!("claude_code", "gate");
    const DONE: &str = screen!("claude_code", "done");
    /// What every recorded screen's run typed.
    const PROMPT: &str = crate::harness::plan::DEFAULT_PROMPT;
    const CODEX_BOOT: &str = screen!("codex", "boot");
    const CODEX_GATE: &str = screen!("codex", "gate");
    const CODEX_CANCEL: &str = screen!("codex", "cancel");
    const OPENCODE_BOOT: &str = screen!("opencode", "boot");
    const OPENCODE_CANCEL: &str = screen!("opencode", "cancel");
    const OMP_BOOT: &str = screen!("omp", "boot");
    const OMP_NEW: &str = screen!("omp", "new");
    const CODEWHALE_BOOT: &str = screen!("codewhale", "boot");
    /// Claude Code's blank input line, which it opens with a no-break space.
    const READY_INPUT: &str = "❯\u{a0} ";
    const OMP_INPUT: &str = "╰─";

    /// `source`'s TUI scenario `name`: its row, and how it answers its turn.
    fn tui_scenario(source: &str, name: &str) -> (&'static Tui, Answer) {
        let s = profile(source)
            .and_then(|p| p.scenario(name))
            .expect("profiled");
        match s.surface {
            Surface::Tui { row } => (row, s.answer()),
            Surface::Headless => panic!("{source}/{name} drives no TUI"),
        }
    }

    fn claude_code() -> &'static Tui {
        tui_scenario("claude-code", "permission").0
    }

    fn omp() -> (&'static Tui, Answer) {
        tui_scenario("omp", "new-session")
    }

    fn dwell() -> Duration {
        claude_code().gates[0].dwell
    }

    /// `screen` with [`PROMPT`] typed into `input`, its blank input line.
    fn typed(screen: &str, input: &str) -> String {
        let filled = format!("\n{}{PROMPT}\n", input.trim_end_matches(' '));
        let typed = screen.replacen(&format!("\n{input}\n"), &filled, 1);
        assert_ne!(typed, screen, "the screen's input line moved");
        typed
    }

    fn driver(t0: Instant) -> Driver<'static> {
        let (tui, answer) = tui_scenario("claude-code", "permission");
        Driver::new(tui, answer, PROMPT.to_owned(), t0)
    }

    /// Steps a booted driver to a submitted prompt.
    fn submitted(d: &mut Driver<'_>, t0: Instant) {
        assert_eq!(d.step(BOOT, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(
            d.step(&typed(READY, READY_INPUT), t0),
            Action::Keys(&[Key::Enter])
        );
    }

    #[test]
    fn the_prompt_is_typed_once_the_composer_shows_and_entered_separately() {
        let t0 = Instant::now();
        let mut d = driver(t0);
        assert_eq!(d.step("", t0), Action::Wait, "still painting");
        submitted(&mut d, t0);
    }

    #[test]
    fn enter_is_resent_only_while_the_composer_still_holds_the_prompt() {
        let t0 = Instant::now();
        let mut d = driver(t0);
        submitted(&mut d, t0);
        let late = t0 + ENTER_RETRY;
        assert_eq!(
            d.step(&typed(READY, READY_INPUT), late),
            Action::Keys(&[Key::Enter]),
            "a swallowed Enter"
        );
        assert_eq!(
            d.step(DONE, late + ENTER_RETRY),
            Action::Wait,
            "the conversation's echo of the prompt is not the composer"
        );
    }

    #[test]
    fn the_turns_dialog_is_refused_once_though_a_frame_drops_it() {
        let t0 = Instant::now();
        let mut d = driver(t0);
        submitted(&mut d, t0);
        let t1 = t0 + Duration::from_millis(10);
        assert_eq!(
            d.step(GATE, t1),
            Action::Wait,
            "inside its key-ignore window"
        );
        let answered = t1 + dwell();
        assert_eq!(d.step(GATE, answered), Action::Keys(&[Key::Esc]));
        assert_eq!(
            d.step(GATE, answered + SETTLE),
            Action::Wait,
            "a frame drawn before the refusal landed"
        );
        let dropped = answered + SETTLE;
        assert_eq!(d.step(DONE, dropped), Action::Wait);
        for back in [dropped, dropped + dwell()] {
            assert_eq!(
                d.step(GATE, back),
                Action::Wait,
                "a frame without the dialog and then one with it is no second dialog"
            );
        }
    }

    #[test]
    fn nothing_is_sent_while_a_dialog_is_open() {
        let t0 = Instant::now();
        let mut d = driver(t0);
        submitted(&mut d, t0);
        assert_eq!(
            d.step(GATE, t0 + ENTER_RETRY),
            Action::Wait,
            "an Enter here would pick the dialog's first choice, Yes"
        );
    }

    #[test]
    fn the_run_quits_only_after_its_dialog_was_refused_and_the_screen_settled() {
        let t0 = Instant::now();
        let mut d = driver(t0);
        submitted(&mut d, t0);
        let quiet = t0 + Duration::from_millis(1);
        assert_eq!(d.step(READY, quiet), Action::Wait);
        assert_eq!(
            d.step(READY, quiet + SETTLE),
            Action::Wait,
            "a quiet stretch before the dialog is not the turn's end"
        );
        let t1 = quiet + SETTLE + Duration::from_millis(1);
        d.step(GATE, t1);
        assert_eq!(d.step(GATE, t1 + dwell()), Action::Keys(&[Key::Esc]));
        let t2 = t1 + dwell() + Duration::from_millis(1);
        assert_eq!(d.step(DONE, t2), Action::Wait);
        assert_eq!(d.step(DONE, t2 + SETTLE), Action::Quit(claude_code().quit));
        assert_eq!(d.step(DONE, t2 + SETTLE * 2), Action::Wait, "quits once");
    }

    /// The echo of the prompt sits between two rules too, and a working turn's
    /// redirect line, which an Enter would interrupt, is no composer.
    #[test]
    fn hermes_enters_nothing_into_its_working_turn_nor_onto_its_echo() {
        let (tui, answer) = tui_scenario("hermes", "permission");
        let [gate] = tui.gates else {
            panic!("one gate");
        };
        let t0 = Instant::now();
        let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0);
        let boot = screen!("hermes", "boot");
        assert_eq!(d.step(boot, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(d.step(boot, t0), Action::Keys(&[Key::Enter]));
        let mut t = t0;
        let mut next = |screen| {
            t += ENTER_RETRY;
            d.step(screen, t)
        };
        assert_eq!(next(screen!("hermes", "turn")), Action::Wait);
        assert_eq!(next(screen!("hermes", "gate")), Action::Keys(gate.keys));
        assert_eq!(
            next(screen!("hermes", "echo")),
            Action::Wait,
            "a frame showing only the echo"
        );
        let done = screen!("hermes", "done");
        assert_eq!(next(done), Action::Wait, "the screen just changed");
        assert_eq!(d.step(done, t + SETTLE), Action::Quit(tui.quit));
        assert_eq!(
            d.composer(),
            Some(" Explain this error and how to fix it"),
            "the input line under the titled rule"
        );
    }

    #[test]
    fn a_turn_answering_nothing_quits_once_the_screen_settles_after_its_prompt() {
        let (tui, answer) = omp();
        assert_eq!(answer, Answer::Nothing);
        let t0 = Instant::now();
        let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0);
        assert_eq!(d.step("", t0), Action::Wait);
        assert_eq!(
            d.step("", t0 + SETTLE),
            Action::Wait,
            "a settled screen before the prompt is no turn's end"
        );
        let t1 = t0 + SETTLE;
        assert_eq!(d.step(OMP_BOOT, t1), Action::Type(PROMPT.to_owned()));
        let holding = typed(OMP_BOOT, OMP_INPUT);
        assert_eq!(d.step(&holding, t1), Action::Keys(&[Key::Enter]));
        assert_eq!(
            d.step(&holding, t1 + SETTLE),
            Action::Keys(&[Key::Enter]),
            "a composer still holding the prompt was never submitted"
        );
        let t2 = t1 + SETTLE + Duration::from_millis(1);
        assert_eq!(
            d.step(OMP_NEW, t2),
            Action::Wait,
            "the turn's screen just changed"
        );
        assert_eq!(d.step(OMP_NEW, t2 + SETTLE), Action::Quit(tui.quit));
    }

    #[test]
    fn a_second_prompt_goes_in_once_the_first_turn_settles_and_the_run_quits_after_its_own() {
        const SECOND: &str = "List this directory.";
        let (tui, answer) = omp();
        let t0 = Instant::now();
        let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0).then(SECOND);
        assert_eq!(d.step(OMP_BOOT, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(
            d.step(&typed(OMP_BOOT, OMP_INPUT), t0),
            Action::Keys(&[Key::Enter])
        );
        let t1 = t0 + Duration::from_millis(1);
        assert_eq!(d.step(OMP_NEW, t1), Action::Wait);
        assert_eq!(
            d.step(OMP_NEW, t1 + SETTLE),
            Action::Type(SECOND.to_owned())
        );
        assert_eq!(d.step(OMP_NEW, t1 + SETTLE), Action::Keys(&[Key::Enter]));
        assert_eq!(
            d.step(OMP_NEW, t1 + SETTLE * 2),
            Action::Wait,
            "a screen standing since before the second Enter is no second turn's end"
        );
        let t2 = t1 + SETTLE * 2 + Duration::from_millis(1);
        assert_eq!(d.step(OMP_BOOT, t2), Action::Wait);
        assert_eq!(d.step(OMP_BOOT, t2 + SETTLE), Action::Quit(tui.quit));
    }

    #[test]
    fn a_second_turn_answers_its_own_gate() {
        const SECOND: &str = "Second turn.";
        let (tui, answer) = tui_scenario("codex", "permission");
        let [gate] = tui.gates else {
            panic!("one gate");
        };
        let t0 = Instant::now();
        let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0).then(SECOND);
        assert_eq!(d.step(CODEX_BOOT, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(d.step(CODEX_BOOT, t0), Action::Keys(&[Key::Enter]));
        let mut t = t0;
        for turn in ["first", "second"] {
            t += Duration::from_millis(1);
            assert_eq!(
                d.step(CODEX_GATE, t),
                Action::Wait,
                "{turn}: inside its dwell"
            );
            t += gate.dwell;
            assert_eq!(d.step(CODEX_GATE, t), Action::Keys(gate.keys), "{turn}");
            t += Duration::from_millis(1);
            assert_eq!(
                d.step(CODEX_BOOT, t),
                Action::Wait,
                "{turn}: the screen moved"
            );
            t += SETTLE;
            if turn == "first" {
                assert_eq!(d.step(CODEX_BOOT, t), Action::Type(SECOND.to_owned()));
                assert_eq!(d.step(CODEX_BOOT, t), Action::Keys(&[Key::Enter]));
            }
        }
        assert_eq!(d.step(CODEX_BOOT, t), Action::Quit(tui.quit));
    }

    #[test]
    fn a_run_quits_unanswered_only_where_it_answers_nothing() {
        let (tui, _) = omp();
        for answer in [Answer::Gate, Answer::Cancel, Answer::Nothing] {
            let t0 = Instant::now();
            let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0);
            assert_eq!(d.step(OMP_BOOT, t0), Action::Type(PROMPT.to_owned()));
            assert_eq!(
                d.step(&typed(OMP_BOOT, OMP_INPUT), t0),
                Action::Keys(&[Key::Enter])
            );
            let moved = t0 + Duration::from_millis(1);
            assert_eq!(d.step(OMP_NEW, moved), Action::Wait);
            let end = match answer {
                Answer::Gate | Answer::Cancel => Action::Wait,
                Answer::Nothing => Action::Quit(tui.quit),
            };
            assert_eq!(d.step(OMP_NEW, moved + SETTLE), end, "{answer:?}");
        }
    }

    #[test]
    fn a_turn_answering_nothing_leaves_a_dialog_its_row_lists_unanswered() {
        let t0 = Instant::now();
        let mut d = Driver::new(claude_code(), Answer::Nothing, PROMPT.to_owned(), t0);
        submitted(&mut d, t0);
        let shown = t0 + Duration::from_millis(1);
        for t in [shown, shown + dwell(), shown + SETTLE] {
            assert_eq!(d.step(GATE, t), Action::Wait, "nothing answers it");
        }
        assert_eq!(
            d.step(GATE, shown + STALL),
            Action::Fail("the screen stood still with the turn unfinished")
        );
    }

    #[test]
    fn a_stalled_turn_answering_nothing_gives_up_as_a_run_that_could_not_happen() {
        let (tui, answer) = omp();
        let t0 = Instant::now();
        let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0);
        let holding = typed(OMP_BOOT, OMP_INPUT);
        assert_eq!(d.step(OMP_BOOT, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(d.step(&holding, t0), Action::Keys(&[Key::Enter]));
        assert_eq!(
            d.step(&holding, t0 + STALL),
            Action::Fail("the screen stood still with the turn unfinished")
        );
        assert_eq!(d.gave_up(), GAVE_UP, "no gate was owed");
    }

    #[test]
    fn a_turn_answering_nothing_ends_only_on_a_screen_that_moved_after_its_enter() {
        let (tui, answer) = omp();
        let t0 = Instant::now();
        let entered = t0 + Duration::from_millis(1);
        // The CLI dropped the typed prompt while booting: its boot frame stands.
        let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0);
        assert_eq!(d.step(OMP_BOOT, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(d.step(OMP_BOOT, entered), Action::Keys(&[Key::Enter]));
        assert_eq!(d.step(OMP_BOOT, entered + SETTLE), Action::Wait);
        assert_eq!(
            d.step(OMP_BOOT, t0 + STALL),
            Action::Fail("the screen stood still with the turn unfinished")
        );
        assert_eq!(d.gave_up(), GAVE_UP, "a run that could not happen");
        // A frame read in the step the Enter went out predates it.
        let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0);
        assert_eq!(d.step(OMP_BOOT, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(d.step("", entered), Action::Keys(&[Key::Enter]));
        assert_eq!(d.step("", entered + SETTLE), Action::Wait);
    }

    #[test]
    fn a_dialog_before_the_prompt_stalls_the_run_instead_of_being_answered() {
        let t0 = Instant::now();
        let unlisted = GATE.replace("Do you want to proceed?", "Use this API key?");
        for screen in [GATE, unlisted.as_str()] {
            let mut d = driver(t0);
            assert_eq!(d.step(screen, t0), Action::Wait);
            assert_eq!(
                d.step(screen, t0 + dwell()),
                Action::Wait,
                "its highlighted choice is not the composer, and nothing answers it"
            );
            assert_eq!(
                d.step(screen, t0 + STALL),
                Action::Fail("the screen stood still with the turn unfinished")
            );
        }
    }

    #[test]
    fn a_give_up_after_the_prompt_with_no_gate_refused_is_unanswered() {
        let t0 = Instant::now();
        let mut d = driver(t0);
        assert_eq!(d.gave_up(), GAVE_UP, "the CLI never booted");
        submitted(&mut d, t0);
        d.step(DONE, t0);
        assert_eq!(
            d.step(DONE, t0 + STALL),
            Action::Fail("the screen stood still with the turn unfinished")
        );
        assert_eq!(d.gave_up(), UNANSWERED, "the turn ran with no gate");
        let mut d = driver(t0);
        submitted(&mut d, t0);
        d.step(GATE, t0);
        assert_eq!(d.step(GATE, t0 + dwell()), Action::Keys(&[Key::Esc]));
        assert_eq!(d.gave_up(), GAVE_UP, "its gate was refused");
    }

    #[test]
    fn an_unlisted_dialog_never_reads_as_a_composer_with_nothing_above() {
        let unlisted = CODEX_GATE.replace(
            "Would you like to run the following command?",
            "Allow this tool?",
        );
        let t0 = Instant::now();
        let (tui, answer) = tui_scenario("codex", "permission");
        let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0);
        assert_eq!(
            d.step(&unlisted, t0),
            Action::Wait,
            "a numbered choice is no composer to type into"
        );
        assert_eq!(d.step(CODEX_BOOT, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(d.step(CODEX_BOOT, t0), Action::Keys(&[Key::Enter]));
        assert_eq!(
            d.step(&unlisted, t0 + ENTER_RETRY),
            Action::Wait,
            "an Enter here would pick the dialog's first choice, Yes"
        );
    }

    /// codex's cancel run, submitted, at its screen before the reply streamed.
    fn codex_cancelling(t0: Instant) -> (Driver<'static>, &'static Tui, String) {
        let (tui, answer) = tui_scenario("codex", "cancel");
        assert_eq!(answer, Answer::Cancel);
        let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0);
        assert_eq!(d.step(CODEX_BOOT, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(d.step(CODEX_BOOT, t0), Action::Keys(&[Key::Enter]));
        (d, tui, CODEX_CANCEL.replace(super::super::STREAM_CUE, ""))
    }

    #[test]
    fn a_turn_is_cancelled_once_its_reply_streams_and_quit_once_the_screen_settles() {
        let t0 = Instant::now();
        let (mut d, tui, before_reply) = codex_cancelling(t0);
        let t1 = t0 + Duration::from_millis(1);
        assert_eq!(d.step(&before_reply, t1), Action::Wait);
        assert_eq!(
            d.step(&before_reply, t1 + SETTLE),
            Action::Wait,
            "a quiet stretch before the reply streams is not the turn's end"
        );
        let t2 = t1 + SETTLE;
        assert_eq!(d.step(CODEX_CANCEL, t2), Action::Keys(tui.cancel));
        assert_eq!(
            d.step(CODEX_CANCEL, t2 + ENTER_RETRY),
            Action::Wait,
            "a frame drawn before the cancel landed"
        );
        assert_eq!(d.step(CODEX_CANCEL, t2 + SETTLE), Action::Quit(tui.quit));
        assert_eq!(d.gave_up(), GAVE_UP, "its turn was cancelled");
    }

    #[test]
    fn a_turn_whose_reply_never_streams_gives_up_unanswered() {
        let t0 = Instant::now();
        let (mut d, _, before_reply) = codex_cancelling(t0);
        d.step(&before_reply, t0);
        assert_eq!(
            d.step(&before_reply, t0 + STALL),
            Action::Fail("the screen stood still with the turn unfinished")
        );
        assert_eq!(d.gave_up(), UNANSWERED);
    }

    /// The boot screen's input line holds a placeholder; the session's holds
    /// nothing, under the prompt's echo, which wears the same bar.
    #[test]
    fn opencodes_input_line_sits_its_rows_above_the_edge_and_never_on_the_echo() {
        const PLACEHOLDER: &str = "Ask anything… \"What is the tech stack of this project?\"";
        let (tui, answer) = tui_scenario("opencode", "cancel");
        let t0 = Instant::now();
        let mut d = Driver::new(tui, answer, PROMPT.to_owned(), t0);
        assert_eq!(d.step(OPENCODE_BOOT, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(d.composer(), Some(format!("  {PLACEHOLDER}").as_str()));
        let typed = OPENCODE_BOOT.replacen(PLACEHOLDER, PROMPT, 1);
        assert_ne!(typed, OPENCODE_BOOT, "the placeholder moved");
        assert_eq!(d.step(&typed, t0), Action::Keys(&[Key::Enter]));
        assert_eq!(d.step(&typed, t0), Action::Wait);
        assert_eq!(
            d.step(&typed, t0 + ENTER_RETRY),
            Action::Keys(&[Key::Enter]),
            "a composer still holding the prompt was never submitted"
        );
        let session = OPENCODE_CANCEL.replace(super::super::STREAM_CUE, "");
        let t1 = t0 + ENTER_RETRY;
        assert_eq!(d.step(&session, t1), Action::Wait);
        assert_eq!(d.composer(), Some(""));
        assert_eq!(
            d.step(&session, t1 + ENTER_RETRY),
            Action::Wait,
            "the echo of the prompt is not the composer"
        );
    }

    #[test]
    fn a_rule_closes_on_its_rows_corners_and_nothing_else() {
        let plain = |l| is_rule(l, '─', None);
        let boxed = |l| is_rule(l, '─', Some(('╭', '╮')));
        assert!(plain("────"));
        assert!(!plain("a──b"));
        assert!(!plain("╭──╮"));
        assert!(boxed("  ╭────╮  "));
        assert!(!boxed("────"));
        assert!(!boxed("│ ❯ 1. Allow once │"));
        assert!(!boxed("╭─ title ─╮"));
        assert!(!plain("  "));
    }

    #[test]
    fn a_titled_rule_opens_and_closes_on_its_glyph() {
        let titled = |l| is_titled_rule(l, '─');
        assert!(titled("────"));
        assert!(titled("──── Hermetic ─  "));
        assert!(!titled("─ 4.6s · 12:48 PM"));
        assert!(!titled("❯ ──"));
        assert!(!titled(""));
    }

    /// Each row over its run's recorded boot screen and the screen its turn
    /// ended at: the gate it answered or the reply it cancelled, where it
    /// answers one.
    #[test]
    fn each_row_takes_the_prompt_and_ends_the_turn_its_run_recorded_as_it_answers() {
        const CODEX_TURN_ONE: &str = screen!("codex", "turn-one");
        const GROK_TURN_ONE: &str = screen!("grok", "turn-one");
        // Each run's boot screen, then the screen each of its turns ended at:
        // the gate it answered or the reply it cancelled, where it answers one.
        let recorded: [(&str, &str, &str, &[&str]); 21] = [
            ("claude-code", "permission", BOOT, &[GATE]),
            ("codex", "permission", CODEX_BOOT, &[CODEX_GATE]),
            ("codex", "cancel", CODEX_BOOT, &[CODEX_CANCEL]),
            (
                "codex",
                "two-turn",
                CODEX_BOOT,
                &[CODEX_TURN_ONE, screen!("codex", "turn-two")],
            ),
            (
                "codex",
                "refuse-then-prompt",
                CODEX_BOOT,
                &[
                    CODEX_GATE,
                    screen!("codex", "refused"),
                    screen!("codex", "gate-again"),
                    screen!("codex", "refused-again"),
                ],
            ),
            // The same dialog as the refused one's, answered otherwise.
            ("codex", "approve", CODEX_BOOT, &[CODEX_GATE]),
            (
                "codex",
                "policy-deny",
                CODEX_BOOT,
                &[screen!("codex", "policy-denied")],
            ),
            ("codex", "code-mode-gate", CODEX_BOOT, &[CODEX_GATE]),
            ("codex", "code-mode-approve", CODEX_BOOT, &[CODEX_GATE]),
            (
                "opencode",
                "cancel",
                screen!("opencode", "boot"),
                &[screen!("opencode", "cancel")],
            ),
            (
                "kimi",
                "permission",
                screen!("kimi", "boot"),
                &[screen!("kimi", "gate")],
            ),
            (
                "grok",
                "permission",
                screen!("grok", "boot"),
                &[screen!("grok", "gate")],
            ),
            (
                "grok",
                "two-turn",
                screen!("grok", "boot"),
                &[GROK_TURN_ONE, screen!("grok", "turn-two")],
            ),
            (
                "reasonix",
                "permission",
                screen!("reasonix", "boot"),
                &[screen!("reasonix", "gate")],
            ),
            (
                "omp",
                "approve",
                OMP_BOOT,
                &[screen!("omp", "approve-gate")],
            ),
            ("omp", "ask", OMP_BOOT, &[screen!("omp", "ask-gate")]),
            ("omp", "new-session", OMP_BOOT, &[OMP_NEW]),
            (
                "hermes",
                "permission",
                screen!("hermes", "boot"),
                &[screen!("hermes", "gate")],
            ),
            (
                "codewhale",
                "tool-run",
                CODEWHALE_BOOT,
                &[screen!("codewhale", "done")],
            ),
            (
                "codewhale",
                "permission",
                CODEWHALE_BOOT,
                &[screen!("codewhale", "gate")],
            ),
            (
                "codewhale",
                "child-error",
                CODEWHALE_BOOT,
                &[screen!("codewhale", "child-failed")],
            ),
        ];
        let scenarios: Vec<(&str, &str)> = super::super::PROFILES
            .iter()
            .flat_map(|p| {
                p.scenarios
                    .iter()
                    .filter(|s| matches!(s.surface, Surface::Tui { .. }))
                    .map(|s| (p.source, s.name))
            })
            .collect();
        assert_eq!(
            recorded
                .iter()
                .map(|(source, name, ..)| (*source, *name))
                .collect::<Vec<_>>(),
            scenarios
        );
        for (source, name, boot, ends) in recorded {
            let scenario = profile(source)
                .and_then(|p| p.scenario(name))
                .expect("profiled");
            let Surface::Tui { row: tui } = scenario.surface else {
                panic!("{source}/{name} drives no TUI");
            };
            let t0 = Instant::now();
            let mut d = scenario
                .tui_driver(PROMPT.to_owned(), t0)
                .expect("a TUI scenario's driver");
            assert_eq!(
                d.step(boot, t0),
                Action::Type(PROMPT.to_owned()),
                "{source}/{name}"
            );
            assert_eq!(
                d.step(boot, t0),
                Action::Keys(&[Key::Enter]),
                "{source}/{name}"
            );
            let moved = t0 + Duration::from_millis(1);
            let (first, then) = match scenario.answer() {
                Answer::Gate => {
                    let [gate] = tui.gates else {
                        panic!("{source}: one gate per row");
                    };
                    let (end, rest) = ends.split_first().expect("a gate");
                    let answers: Vec<Action> = [t0, t0 + gate.dwell]
                        .map(|t| d.step(end, t))
                        .into_iter()
                        .filter(|a| *a != Action::Wait)
                        .collect();
                    assert_eq!(answers, [Action::Keys(gate.keys)], "{source}/{name}");
                    let mut t = t0 + gate.dwell + SETTLE;
                    assert_eq!(
                        d.step(end, t),
                        Action::Wait,
                        "{source}/{name}: a repeat would land on the turn"
                    );
                    let Some(Second::Prompt(second)) = scenario.turn.second else {
                        assert!(rest.is_empty(), "{source}/{name}: one turn");
                        continue;
                    };
                    // The screen each gate left, and the second turn's gate.
                    let [after, again, last] = rest else {
                        panic!("{source}/{name}: two turns' gates and what each left");
                    };
                    t += Duration::from_millis(1);
                    assert_eq!(d.step(after, t), Action::Wait, "{source}/{name}");
                    t += SETTLE;
                    assert_eq!(d.step(after, t), Action::Type(second.to_owned()));
                    assert_eq!(d.step(after, t), Action::Keys(&[Key::Enter]));
                    t += Duration::from_millis(1);
                    assert_eq!(d.step(again, t), Action::Wait, "{source}/{name}: its dwell");
                    t += gate.dwell;
                    assert_eq!(d.step(again, t), Action::Keys(gate.keys), "{source}/{name}");
                    t += Duration::from_millis(1);
                    assert_eq!(d.step(last, t), Action::Wait, "{source}/{name}");
                    assert_eq!(
                        d.step(last, t + SETTLE),
                        Action::Quit(tui.quit),
                        "{source}/{name}"
                    );
                    continue;
                }
                Answer::Cancel => (Action::Keys(tui.cancel), ends),
                Answer::Nothing => (Action::Wait, ends),
            };
            let (end, later) = then.split_first().expect("a turn's end");
            assert_eq!(
                d.step(end, moved),
                first,
                "{source}/{name}: the screen moved"
            );
            let settled = moved + SETTLE;
            let Some(Second::Prompt(second)) = scenario.turn.second else {
                assert!(later.is_empty(), "{source}/{name}: one turn");
                assert_eq!(
                    d.step(end, settled),
                    Action::Quit(tui.quit),
                    "{source}/{name}"
                );
                continue;
            };
            let [last] = later else {
                panic!("{source}/{name}: two turns' ends");
            };
            assert_eq!(
                d.step(end, settled),
                Action::Type(second.to_owned()),
                "{source}/{name}"
            );
            assert_eq!(
                d.step(end, settled),
                Action::Keys(&[Key::Enter]),
                "{source}/{name}"
            );
            let moved = settled + Duration::from_millis(1);
            assert_eq!(d.step(last, moved), Action::Wait, "{source}/{name}");
            assert_eq!(
                d.step(last, moved + SETTLE),
                Action::Quit(tui.quit),
                "{source}/{name}"
            );
        }
    }

    #[test]
    fn the_command_line_round_trips_and_refuses_a_short_one() {
        let line = CommandLine {
            source: "claude-code".into(),
            scenario: "permission".into(),
            log: PathBuf::from("/runs/x/tui.log"),
            prompt: "hi".into(),
            program: vec!["/cli/claude".into(), "--permission-mode".into()],
        };
        assert_eq!(CommandLine::parse(&line.to_args()), Some(line));
        let args = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            CommandLine::parse(&args(&["s", "p", "t", "hi", "--"])),
            None
        );
        assert_eq!(
            CommandLine::parse(&args(&["s", "p", "hi", "--", "cli"])),
            None
        );
    }
}
