//! The interactive driver's decisions, over the screen a pty renders.
//!
//! Some wire exists only in a CLI's TUI: Claude Code's headless mode refuses a
//! tool without firing `PermissionRequest`. The `tui_drive` example runs the CLI under a pty and
//! asks [`Driver::step`] what to do with the screen it now shows; everything
//! that differs between CLIs is its [`Tui`] row, never code here. A new CLI's
//! edge case is that row plus a test over a screen its run recorded
//! (`screens/`).

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long an answered dialog may stay on screen before its keys are sent
/// again: a key can be dropped.
const GATE_RETRY: Duration = Duration::from_secs(1);
/// How long the composer may still hold the prompt after an Enter before it is
/// sent again: an Enter during startup is swallowed.
const ENTER_RETRY: Duration = Duration::from_secs(1);
/// How long the screen must stay unchanged after the turn before the driver
/// quits, short of the idle notifiers a CLI fires at a waiting composer.
const SETTLE: Duration = Duration::from_secs(3);
/// How long an unchanged screen may stand before a turn that never finished is
/// given up: an unrecognised dialog stalls a run, and its key is never guessed.
const STALL: Duration = Duration::from_secs(20);

/// `tui_drive`'s exit when it gave up on a run it could not finish.
pub const GAVE_UP: i32 = 3;
/// `tui_drive`'s exit when it gave up on a submitted turn that raised no gate
/// it knows to refuse: a verdict on the CLI, not a run that could not happen.
pub(crate) const UNREFUSED: i32 = 4;

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
    /// The keys that refuse it.
    pub(crate) deny: &'static [Key],
    /// How long it must stand before it is answered: a key it ignores is lost.
    pub(crate) dwell: Duration,
}

/// Where a CLI's composer is on screen.
#[derive(Debug)]
pub(crate) struct Composer {
    /// What its input line starts with, at the line's first column. A
    /// dialog's highlighted choice and the conversation's echo of a prompt
    /// can start with it too.
    pub(crate) marker: &'static str,
    pub(crate) above: Above,
}

/// What a composer draws directly above its input line, which tells the
/// input line from the other marked lines.
#[derive(Debug)]
pub(crate) enum Above {
    /// A rule of `glyph`, between `corners` when the composer is boxed; no
    /// other marked line sits under it.
    Rule {
        glyph: char,
        corners: Option<(char, char)>,
    },
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
    /// Every dialog the turn may raise, refused. One the table doesn't name
    /// stalls the run.
    pub(crate) gates: &'static [Gate],
    /// What ends the session the way a user would, so its end hook fires.
    pub(crate) quit: &'static [Key],
    pub(crate) renders_under_conpty: bool,
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

/// A dialog on screen: its row in the gate table, when it appeared, and when it
/// was last answered.
#[derive(Debug, Clone, Copy)]
struct Open {
    gate: usize,
    since: Instant,
    answered: Option<Instant>,
}

/// The state machine one `Permission` run steps through: the turn's dialog is
/// refused, and the turn is over once it was.
#[derive(Debug)]
pub struct Driver<'a> {
    tui: &'a Tui,
    prompt: String,
    phase: Phase,
    screen: String,
    changed_at: Instant,
    open: Option<Open>,
    refused: bool,
}

/// How many leading characters of the prompt identify it in the composer: a
/// long prompt wraps, and its first line is what the composer keeps.
const PROMPT_HEAD: usize = 16;

impl<'a> Driver<'a> {
    /// A run through `tui`, typing `prompt`.
    pub fn new(tui: &'a Tui, prompt: String, now: Instant) -> Self {
        Self {
            tui,
            prompt,
            phase: Phase::Boot,
            screen: String::new(),
            changed_at: now,
            open: None,
            refused: false,
        }
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
        if let Some(action) = self.gate(now) {
            return action;
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
            Phase::Submitted { enter_at } if self.composer_holds_prompt() => {
                if now.duration_since(enter_at) < ENTER_RETRY {
                    return Action::Wait;
                }
                self.phase = Phase::Submitted { enter_at: now };
                Action::Keys(&[Key::Enter])
            }
            Phase::Submitted { .. }
                if self.refused && now.duration_since(self.changed_at) >= SETTLE =>
            {
                self.phase = Phase::Quit;
                Action::Quit(self.tui.quit)
            }
            _ => Action::Wait,
        }
    }

    /// What `tui_drive` exits with if it gives up now.
    pub fn gave_up(&self) -> i32 {
        match self.phase {
            Phase::Submitted { .. } if !self.refused => UNREFUSED,
            _ => GAVE_UP,
        }
    }

    /// A dialog on screen: refused after it has stood its dwell, and again each
    /// [`GATE_RETRY`] it outlives the refusal, once the prompt is submitted;
    /// before that, it stalls the run like any dialog nobody planned for. While
    /// one is open nothing else is sent, so no Enter can land on it and pick
    /// its first choice.
    fn gate(&mut self, now: Instant) -> Option<Action> {
        let Some(index) = self
            .tui
            .gates
            .iter()
            .position(|g| self.screen.contains(g.shows))
        else {
            self.open = None;
            return None;
        };
        let gate = &self.tui.gates[index];
        if !matches!(self.phase, Phase::Submitted { .. }) {
            return Some(Action::Wait);
        }
        let mut open = match self.open {
            Some(open) if open.gate == index => open,
            _ => Open {
                gate: index,
                since: now,
                answered: None,
            },
        };
        let due = match open.answered {
            Some(at) => now.duration_since(at) >= GATE_RETRY,
            None => now.duration_since(open.since) >= gate.dwell,
        };
        if due {
            open.answered = Some(now);
        }
        self.open = Some(open);
        if !due {
            return Some(Action::Wait);
        }
        self.refused = true;
        Some(Action::Keys(gate.deny))
    }

    /// The text after the composer's marker on its input line.
    fn composer(&self) -> Option<&str> {
        let composer = &self.tui.composer;
        let lines: Vec<&str> = self.screen.lines().collect();
        match composer.above {
            Above::Rule { glyph, corners } => lines.windows(2).rev().find_map(|pair| {
                is_rule(pair[0], glyph, corners)
                    .then(|| pair[1].strip_prefix(composer.marker))
                    .flatten()
            }),
            Above::Nothing => {
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
    /// The scenario's shape name.
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
    use super::super::{Surface, profile};
    use super::*;

    const BOOT: &str = include_str!("screens/claude-code-boot.txt");
    const READY: &str = include_str!("screens/claude-code-ready.txt");
    const GATE: &str = include_str!("screens/claude-code-gate.txt");
    const DONE: &str = include_str!("screens/claude-code-done.txt");
    const PROMPT: &str = "Read NOTE.txt, then list this directory, then read NOTE.txt again.";
    const CODEX_BOOT: &str = include_str!("screens/codex-boot.txt");
    const CODEX_GATE: &str = include_str!("screens/codex-gate.txt");

    fn row(source: &str) -> &'static Tui {
        profile(source)
            .expect("profiled")
            .scenarios
            .iter()
            .find_map(|s| match s.surface {
                Surface::Tui(tui) => Some(tui),
                Surface::Headless => None,
            })
            .expect("drives a TUI scenario")
    }

    fn claude_code() -> &'static Tui {
        row("claude-code")
    }

    fn dwell() -> Duration {
        claude_code().gates[0].dwell
    }

    /// The ready screen with `text` in its composer, which Claude Code opens
    /// with a no-break space.
    fn typed(text: &str) -> String {
        let screen = READY.replacen("\n❯\u{a0} \n", &format!("\n❯\u{a0}{text}\n"), 1);
        assert_ne!(screen, READY, "the ready screen's composer line moved");
        screen
    }

    fn driver(t0: Instant) -> Driver<'static> {
        Driver::new(claude_code(), PROMPT.to_owned(), t0)
    }

    /// Steps a booted driver to a submitted prompt.
    fn submitted(d: &mut Driver<'_>, t0: Instant) {
        assert_eq!(d.step(BOOT, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(d.step(&typed(PROMPT), t0), Action::Keys(&[Key::Enter]));
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
            d.step(&typed(PROMPT), late),
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
    fn the_turns_dialog_is_refused_after_its_dwell_and_again_while_it_stands() {
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
            d.step(GATE, answered + GATE_RETRY / 2),
            Action::Wait,
            "the answer is given time to land"
        );
        assert_eq!(
            d.step(GATE, answered + GATE_RETRY),
            Action::Keys(&[Key::Esc]),
            "a dropped key is sent again"
        );
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
    fn a_give_up_after_the_prompt_with_no_gate_refused_is_unrefused() {
        let t0 = Instant::now();
        let mut d = driver(t0);
        assert_eq!(d.gave_up(), GAVE_UP, "the CLI never booted");
        submitted(&mut d, t0);
        d.step(DONE, t0);
        assert_eq!(
            d.step(DONE, t0 + STALL),
            Action::Fail("the screen stood still with the turn unfinished")
        );
        assert_eq!(d.gave_up(), UNREFUSED, "the turn ran with no gate");
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
        let mut d = Driver::new(row("codex"), PROMPT.to_owned(), t0);
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
    fn each_row_takes_the_prompt_and_refuses_the_gate_its_run_recorded() {
        let recorded = [
            ("claude-code", BOOT, GATE),
            ("codex", CODEX_BOOT, CODEX_GATE),
            (
                "kimi",
                include_str!("screens/kimi-boot.txt"),
                include_str!("screens/kimi-gate.txt"),
            ),
            (
                "grok",
                include_str!("screens/grok-boot.txt"),
                include_str!("screens/grok-gate.txt"),
            ),
            (
                "reasonix",
                include_str!("screens/reasonix-boot.txt"),
                include_str!("screens/reasonix-gate.txt"),
            ),
            (
                "hermes",
                include_str!("screens/hermes-boot.txt"),
                include_str!("screens/hermes-gate.txt"),
            ),
        ];
        let rows: Vec<&str> = super::super::PROFILES
            .iter()
            .filter(|p| {
                p.scenarios
                    .iter()
                    .any(|s| matches!(s.surface, Surface::Tui(_)))
            })
            .map(|p| p.source)
            .collect();
        assert_eq!(recorded.map(|(source, ..)| source).to_vec(), rows);
        for (source, boot, gate) in recorded {
            let tui = row(source);
            let t0 = Instant::now();
            let mut d = Driver::new(tui, PROMPT.to_owned(), t0);
            assert_eq!(
                d.step(boot, t0),
                Action::Type(PROMPT.to_owned()),
                "{source}"
            );
            assert_eq!(d.step(boot, t0), Action::Keys(&[Key::Enter]), "{source}");
            let [refused] = tui.gates else {
                panic!("{source}: one gate per row");
            };
            let answers: Vec<Action> = [t0, t0 + refused.dwell]
                .map(|t| d.step(gate, t))
                .into_iter()
                .filter(|a| *a != Action::Wait)
                .collect();
            assert_eq!(answers, [Action::Keys(refused.deny)], "{source}");
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
