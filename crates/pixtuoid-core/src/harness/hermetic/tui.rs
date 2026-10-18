//! The interactive driver's decisions, over the screen a pty renders.
//!
//! Some wire exists only in a CLI's TUI: every headless mode auto-approves or
//! refuses without asking. The `tui_drive` example runs the CLI under a pty and
//! asks [`Driver::step`] what to do with the screen it now shows; everything
//! that differs between CLIs is a profile's [`Tui`] data, never code here.

use std::time::{Duration, Instant};

use super::{PerOs, Shape};

/// How long a dialog must stay on screen before it is answered: Claude Code
/// ignores keys for 150 ms after one opens and re-arms on each ignored key, and
/// the headless scene samples every `HEADLESS_SUMMARY_POLL_INTERVAL_MS`, which
/// must see the wait; its `permission_prompt` notifier fires about 6 s later.
pub const GATE_DWELL: Duration = Duration::from_millis(400);
/// How long an answered dialog may stay on screen before its keys are sent
/// again: a key can be dropped, and the dialog's notifier is still seconds off.
pub const GATE_RETRY: Duration = Duration::from_secs(1);
/// How long the composer may still hold the prompt after an Enter before it is
/// sent again: an Enter during startup is swallowed.
pub const ENTER_RETRY: Duration = Duration::from_secs(1);
/// How long the screen must stay unchanged after the turn before the driver
/// quits; well under the idle notifiers (Claude Code's is about 60 s).
pub const SETTLE: Duration = Duration::from_secs(3);
/// How long an unchanged screen may stand before a turn that never finished is
/// given up: an unrecognised dialog stalls a run, and its key is never guessed.
pub const STALL: Duration = Duration::from_secs(20);

/// A key the driver writes to the pty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Submits the composer or confirms a highlighted choice.
    Enter,
    /// Dismisses a dialog.
    Esc,
    /// Moves a menu's highlight one choice down.
    Down,
    /// Literal text, a slash command for one.
    Text(&'static str),
}

impl Key {
    /// The bytes a terminal sends for it.
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Enter => b"\r",
            Self::Esc => b"\x1b",
            Self::Down => b"\x1b[B",
            Self::Text(text) => text.as_bytes(),
        }
    }
}

/// One of a CLI's confirmation dialogs.
#[derive(Debug)]
pub struct Gate {
    /// Text on screen only while the dialog is open.
    pub shows: &'static str,
    /// The keys that answer it yes.
    pub accept: PerOs<&'static [Key]>,
    /// The keys that answer it no.
    pub deny: PerOs<&'static [Key]>,
}

/// A CLI's interactive facts: the data the driver reads, one row per CLI.
#[derive(Debug)]
pub struct Tui {
    /// The interactive command, mode flags included; no prompt.
    pub argv: &'static [&'static str],
    /// The start of the composer's input line. The LAST screen line carrying
    /// it is the composer: the transcript above echoes a submitted prompt.
    pub composer: &'static str,
    /// Every dialog a run may raise. One the table doesn't name stalls the run.
    pub gates: &'static [Gate],
    /// What ends the session the way a user would, so its end hook fires.
    pub quit: &'static [Key],
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

/// The state machine one run steps through.
#[derive(Debug)]
pub struct Driver<'a> {
    tui: &'a Tui,
    prompt: String,
    /// The answer the turn's own dialogs get; dialogs before the prompt is
    /// submitted are setup and are accepted.
    deny_the_turn: bool,
    /// Whether the turn is over only once one of its dialogs was answered.
    turn_needs_a_gate: bool,
    phase: Phase,
    screen: String,
    changed_at: Instant,
    /// The dialog on screen: its row in the table, when it appeared, and when
    /// it was last answered.
    open: Option<(usize, Instant, Option<Instant>)>,
    turn_gate_answered: bool,
}

/// How many leading characters of the prompt identify it in the composer: a
/// long prompt wraps, and its first line is what the composer keeps.
const PROMPT_HEAD: usize = 16;

impl<'a> Driver<'a> {
    /// A run of `shape` through `tui`, typing `prompt`.
    pub fn new(tui: &'a Tui, shape: Shape, prompt: String, now: Instant) -> Self {
        let permission = shape == Shape::Permission;
        Self {
            tui,
            prompt,
            deny_the_turn: permission,
            turn_needs_a_gate: permission,
            phase: Phase::Boot,
            screen: String::new(),
            changed_at: now,
            open: None,
            turn_gate_answered: false,
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
                if (self.turn_gate_answered || !self.turn_needs_a_gate)
                    && now.duration_since(self.changed_at) >= SETTLE =>
            {
                self.phase = Phase::Quit;
                Action::Quit(self.tui.quit)
            }
            _ => Action::Wait,
        }
    }

    /// A dialog on screen: answered after it has stood [`GATE_DWELL`], and
    /// again each [`GATE_RETRY`] it outlives its answer. While one is open
    /// nothing else is sent, so no Enter can land on it and pick its first
    /// choice.
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
        let (since, answered) = match self.open {
            Some((open, since, answered)) if open == index => (since, answered),
            _ => (now, None),
        };
        self.open = Some((index, since, answered));
        let due = match answered {
            Some(at) => now.duration_since(at) >= GATE_RETRY,
            None => now.duration_since(since) >= GATE_DWELL,
        };
        if !due {
            return Some(Action::Wait);
        }
        self.open = Some((index, since, Some(now)));
        let gate = &self.tui.gates[index];
        let in_turn = matches!(self.phase, Phase::Submitted { .. });
        self.turn_gate_answered |= in_turn;
        let keys = if in_turn && self.deny_the_turn {
            gate.deny.here()
        } else {
            gate.accept.here()
        };
        Some(Action::Keys(keys))
    }

    fn composer(&self) -> Option<&str> {
        self.screen
            .lines()
            .rev()
            .find_map(|line| line.trim_start().strip_prefix(self.tui.composer))
    }

    fn composer_holds_prompt(&self) -> bool {
        let head: String = self.prompt.chars().take(PROMPT_HEAD).collect();
        self.composer()
            .is_some_and(|typed| typed.trim_start().starts_with(head.trim_end()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCEPT: &[Key] = &[Key::Enter];
    const DENY: &[Key] = &[Key::Esc];
    const QUIT: &[Key] = &[Key::Text("/exit"), Key::Enter];
    const TUI: Tui = Tui {
        argv: &["cli"],
        composer: "> ",
        gates: &[Gate {
            shows: "Do you want to proceed?",
            accept: PerOs {
                unix: ACCEPT,
                windows: ACCEPT,
            },
            deny: PerOs {
                unix: DENY,
                windows: DENY,
            },
        }],
        quit: QUIT,
    };
    const PROMPT: &str = "delete the scratch file";
    const READY: &str = "welcome\n> \nstatus";
    const TYPED: &str = "welcome\n> delete the scratch file\nstatus";
    const GATED: &str = "> delete the scratch file\nBash(rm x)\nDo you want to proceed?\n> ";
    const ECHOED: &str = "> delete the scratch file\nsure\n> \nstatus";

    fn driver(shape: Shape, t0: Instant) -> Driver<'static> {
        Driver::new(&TUI, shape, PROMPT.to_owned(), t0)
    }

    /// Steps a booted driver to a submitted prompt, returning the time it was.
    fn submitted(d: &mut Driver<'_>, t0: Instant) -> Instant {
        assert_eq!(d.step(READY, t0), Action::Type(PROMPT.to_owned()));
        assert_eq!(d.step(TYPED, t0), Action::Keys(&[Key::Enter]));
        t0
    }

    #[test]
    fn the_prompt_is_typed_once_the_composer_shows_and_entered_separately() {
        let t0 = Instant::now();
        let mut d = driver(Shape::Permission, t0);
        assert_eq!(d.step("loading", t0), Action::Wait);
        submitted(&mut d, t0);
    }

    #[test]
    fn enter_is_resent_only_while_the_composer_still_holds_the_prompt() {
        let t0 = Instant::now();
        let mut d = driver(Shape::Permission, t0);
        submitted(&mut d, t0);
        let late = t0 + ENTER_RETRY;
        assert_eq!(
            d.step(TYPED, late),
            Action::Keys(&[Key::Enter]),
            "swallowed Enter"
        );
        assert_eq!(
            d.step(ECHOED, late + ENTER_RETRY),
            Action::Wait,
            "the transcript's echo of the prompt is not the composer"
        );
    }

    #[test]
    fn a_turn_gate_is_denied_under_permission_after_it_has_stood() {
        let t0 = Instant::now();
        let mut d = driver(Shape::Permission, t0);
        submitted(&mut d, t0);
        let t1 = t0 + Duration::from_millis(10);
        assert_eq!(
            d.step(GATED, t1),
            Action::Wait,
            "inside its key-ignore window"
        );
        let answered = t1 + GATE_DWELL;
        assert_eq!(d.step(GATED, answered), Action::Keys(DENY));
        assert_eq!(
            d.step(GATED, answered + GATE_RETRY / 2),
            Action::Wait,
            "the answer is given time to land"
        );
        assert_eq!(
            d.step(GATED, answered + GATE_RETRY),
            Action::Keys(DENY),
            "a dropped key is sent again"
        );
    }

    #[test]
    fn a_dialog_before_the_prompt_is_setup_and_accepted() {
        let t0 = Instant::now();
        let mut d = driver(Shape::Permission, t0);
        let trust = "Do you want to proceed?\n1. Yes";
        assert_eq!(d.step(trust, t0), Action::Wait);
        assert_eq!(d.step(trust, t0 + GATE_DWELL), Action::Keys(ACCEPT));
    }

    #[test]
    fn nothing_is_sent_while_a_dialog_is_open() {
        let t0 = Instant::now();
        let mut d = driver(Shape::Permission, t0);
        submitted(&mut d, t0);
        let t1 = t0 + ENTER_RETRY;
        assert_eq!(
            d.step(GATED, t1),
            Action::Wait,
            "an Enter here would pick the gate's first choice"
        );
    }

    #[test]
    fn permission_quits_only_after_its_gate_was_answered_and_the_screen_settled() {
        let t0 = Instant::now();
        let mut d = driver(Shape::Permission, t0);
        submitted(&mut d, t0);
        let quiet = t0 + Duration::from_millis(1);
        assert_eq!(d.step(ECHOED, quiet), Action::Wait);
        assert_eq!(
            d.step(ECHOED, quiet + SETTLE),
            Action::Wait,
            "a quiet stretch before the gate is not the turn's end"
        );
        let t1 = quiet + SETTLE + Duration::from_millis(1);
        d.step(GATED, t1);
        assert_eq!(d.step(GATED, t1 + GATE_DWELL), Action::Keys(DENY));
        let t2 = t1 + GATE_DWELL + Duration::from_millis(1);
        assert_eq!(d.step(ECHOED, t2), Action::Wait);
        assert_eq!(d.step(ECHOED, t2 + SETTLE), Action::Quit(QUIT));
        assert_eq!(d.step(ECHOED, t2 + SETTLE * 2), Action::Wait, "quits once");
    }

    #[test]
    fn a_shape_without_a_gate_quits_once_the_submitted_turn_settles() {
        let t0 = Instant::now();
        let mut d = driver(Shape::ToolRun, t0);
        submitted(&mut d, t0);
        let t1 = t0 + Duration::from_millis(1);
        assert_eq!(d.step(ECHOED, t1), Action::Wait);
        assert_eq!(d.step(ECHOED, t1 + SETTLE), Action::Quit(QUIT));
    }

    #[test]
    fn an_unrecognised_dialog_stalls_the_run_instead_of_being_guessed() {
        let t0 = Instant::now();
        let mut d = driver(Shape::Permission, t0);
        let unknown = "Use this API key?\n❯ 1. No (recommended)";
        assert_eq!(d.step(unknown, t0), Action::Wait, "not the composer");
        assert_eq!(
            d.step(unknown, t0 + STALL),
            Action::Fail("the screen stood still with the turn unfinished")
        );
    }
}
