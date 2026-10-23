//! Frame-time telemetry, after Android's JankStats
//! (<https://developer.android.com/topic/performance/jankstats>): a frame past
//! the loop's scheduled interval is a debug `frame slow`, one past twice it a
//! debug `frame jank`, each with what it was drawn under; once a minute, and
//! at exit, a `frame pacing` summary gives the spread, at warn when a frame
//! janked.

use std::time::{Duration, Instant};

use pixtuoid_scene::anim::PAINT_FRAME_MS;
use pixtuoid_scene::look::FrameNote;

/// The target every report logs under, whatever module this is:
/// `scripts/pace-check.py` filters its debug frames by it
/// (`pace_check_filters_the_reports_target`).
const TARGET: &str = "pixtuoid::jank";

/// How often the spread is reported.
const WINDOW: Duration = Duration::from_secs(60);
/// A window's frames twice over, room for a loop that runs hot: past it the
/// oldest are overwritten.
const RING: usize = 2 * (WINDOW.as_millis() / PAINT_FRAME_MS as u128) as usize;

/// What of the scene a frame repainted, as its report names it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Painted {
    /// Nothing: the frame was the last one.
    #[default]
    Unchanged,
    /// Only inside dirty rects.
    Rects,
    /// Whole.
    All,
    /// The half-blocks, which have no transmits to say.
    Classic,
}

impl Painted {
    fn name(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Rects => "rects",
            Self::All => "all",
            Self::Classic => "classic",
        }
    }
}

impl From<&pixtuoid_scene::cutaway::canvas::Dirty> for Painted {
    fn from(dirty: &pixtuoid_scene::cutaway::canvas::Dirty) -> Self {
        use pixtuoid_scene::cutaway::canvas::Dirty;
        match dirty {
            Dirty::All => Self::All,
            Dirty::Rects(_) => Self::Rects,
            Dirty::Unchanged => Self::Unchanged,
        }
    }
}

/// What a frame's image transmits did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FrameSend {
    /// What of the scene it repainted.
    pub(crate) dirty: Painted,
    /// Sent whole for a floor or slide the terminal had not shown, whatever
    /// the scene repainted.
    pub(crate) fresh: bool,
    /// The tiles whose pixels changed.
    pub(crate) changed: usize,
    /// The tiles written.
    pub(crate) sent: usize,
    /// Their bytes.
    pub(crate) bytes: u64,
    /// Cutting and encoding them.
    pub(crate) encode: Duration,
}

/// One over-interval frame's report at `level`, with what drew it.
macro_rules! report {
    ($level:ident, $message:literal, $total:expr, $present:expr, $send:expr, $note:expr) => {{
        let (total, present, send, note): (Duration, Duration, FrameSend, Option<FrameNote>) =
            ($total, $present, $send, $note);
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let (from, to, share) = note.map_or((None, None, 0.0), |n| {
            (Some(n.weather.0), Some(n.weather.1), n.weather.2)
        });
        tracing::$level!(
            target: TARGET,
            total = ms(total),
            produce = ms(total.saturating_sub(send.encode + present)),
            encode = ms(send.encode),
            present = ms(present),
            dirty = send.dirty.name(),
            fresh = send.fresh,
            changed = send.changed,
            sent = send.sent,
            bytes = send.bytes,
            repaint = ?note.and_then(|n| n.repaint),
            weather_from = ?from,
            weather_to = ?to,
            weather_share = share,
            strike = note.is_some_and(|n| n.strike),
            $message
        );
    }};
}

/// The frame times of the current window, and its janks.
#[derive(Debug)]
pub(crate) struct Jank {
    micros: [u32; RING],
    len: usize,
    next: usize,
    janks: u32,
    /// Frames past their interval.
    over: u32,
    /// How late frames showed past their interval this window
    /// ([`Self::due_at`]).
    hitch: Duration,
    /// The frames this window that a schedule asked for.
    scheduled: u32,
    /// When the schedule asked for the redraw now pending.
    due: Option<Instant>,
    /// When this window began: `None` until the first frame starts it.
    since: Option<Instant>,
    painter: Painter,
    /// The interval the loop schedules frames at.
    interval: Duration,
}

/// What draws the frames a summary spreads: one matrix cell of a run.
#[derive(Debug, Clone, Default)]
pub(crate) struct Painter {
    /// The image protocol, or `classic` for the half-blocks.
    pub(crate) look: &'static str,
    /// Buffer pixels per layout unit; 1 for the half-blocks.
    pub(crate) scale: u16,
    /// Inside tmux.
    pub(crate) tmux: bool,
    /// `$TERM_PROGRAM`, the terminal's own name for itself.
    pub(crate) terminal: Option<String>,
    /// Each frame goes out inside a synchronized update.
    pub(crate) sync: bool,
}

impl Jank {
    pub(crate) fn new() -> Self {
        Self {
            micros: [0; RING],
            len: 0,
            next: 0,
            janks: 0,
            over: 0,
            hitch: Duration::ZERO,
            scheduled: 0,
            due: None,
            since: None,
            painter: Painter::default(),
            interval: Duration::from_millis(PAINT_FRAME_MS),
        }
    }

    /// The interval frames are scheduled at from now on, which a frame is
    /// slow past and janks at twice.
    pub(crate) fn scheduled_every(&mut self, interval: Duration) {
        self.interval = interval;
    }

    /// The redraw that follows is the one the schedule asked for at `at`: if it
    /// records, it counts toward the hitch time by how far it showed past its interval,
    /// when the frame after it is due. That is the hitch duration, "the
    /// difference between the actual frame lifetime and the expected frame
    /// lifetime", each from the frame's begin
    /// (<https://developer.apple.com/documentation/xcode/understanding-hitches-in-your-app>).
    /// A redraw that ends without recording spends the due
    /// ([`Self::spend_due`]); one no schedule asked for has none, so it can't
    /// count.
    pub(crate) fn due_at(&mut self, at: Instant) {
        self.due = Some(at);
    }

    /// Spend a due that [`Self::record`] did not take, when the redraw ended
    /// without one (nothing to show, or nothing changed).
    pub(crate) fn spend_due(&mut self) {
        self.due = None;
    }

    /// Name what draws the frames from now on.
    pub(crate) fn painted_by(&mut self, painter: Painter) {
        self.painter = painter;
    }

    /// Count a frame that took `total`, `present` of it writing the held frame
    /// out, reported past its interval and twice
    /// past it; the window's spread once `now` closes it. The per-frame
    /// reports are debug, so a slow terminal can't grow the log a line a
    /// frame; a window that janked warns once, in its summary.
    pub(crate) fn record(
        &mut self,
        total: Duration,
        present: Duration,
        send: Option<FrameSend>,
        note: Option<FrameNote>,
        now: Instant,
    ) {
        if let Some(due) = self.due.take() {
            self.hitch += now.saturating_duration_since(due + self.interval);
            self.scheduled += 1;
        }
        self.micros[self.next] = u32::try_from(total.as_micros()).unwrap_or(u32::MAX);
        self.next = (self.next + 1) % RING;
        self.len = (self.len + 1).min(RING);
        if total > self.interval {
            self.over += 1;
            let send = send.unwrap_or(FrameSend {
                dirty: Painted::Classic,
                ..FrameSend::default()
            });
            if total > 2 * self.interval {
                self.janks += 1;
                report!(debug, "frame jank", total, present, send, note);
            } else {
                report!(debug, "frame slow", total, present, send, note);
            }
        }
        let since = *self
            .since
            .get_or_insert_with(|| now.checked_sub(total).unwrap_or(now));
        if now.duration_since(since) >= WINDOW {
            self.summarize(now);
            self.len = 0;
            self.next = 0;
            self.janks = 0;
            self.over = 0;
            self.hitch = Duration::ZERO;
            self.scheduled = 0;
            self.since = Some(now);
        }
    }

    /// Report the window so far, at exit `now`: a run shorter than [`WINDOW`]
    /// still has its summary.
    pub(crate) fn finish(&self, now: Instant) {
        if self.len > 0 {
            self.summarize(now);
        }
    }

    /// The window that `now` closes; `span_ms` is its real time, which a rate
    /// is taken over.
    fn summarize(&self, now: Instant) {
        let mut sorted = self.micros;
        let window = &mut sorted[..self.len];
        window.sort_unstable();
        let (Some(&max), Some(since)) = (window.last(), self.since) else {
            return;
        };
        let at = |q: usize| window[window.len() * q / 100];
        let ms = |us: u32| f64::from(us) / 1000.0;
        let (p50, p99, max) = (ms(at(50)), ms(at(99)), ms(max));
        let frames = self.len;
        let janks = self.janks;
        let over = self.over;
        let hitch_ms = self.hitch.as_secs_f64() * 1000.0;
        let scheduled = self.scheduled;
        let interval_ms = self.interval.as_secs_f64() * 1000.0;
        let span_ms = now.saturating_duration_since(since).as_secs_f64() * 1000.0;
        let Painter {
            look,
            scale,
            tmux,
            terminal,
            sync,
        } = &self.painter;
        if janks > 0 {
            tracing::warn!(target: TARGET, look, scale, tmux, terminal = ?terminal, sync, frames, over, janks, scheduled, hitch_ms, interval_ms, span_ms, p50, p99, max, "frame pacing");
        } else {
            tracing::info!(target: TARGET, look, scale, tmux, terminal = ?terminal, sync, frames, over, janks, scheduled, hitch_ms, interval_ms, span_ms, p50, p99, max, "frame pacing");
        }
    }
}

#[cfg(test)]
mod tests {
    /// The fluency gate reads the debug frames through its `RUST_LOG`, which
    /// must name the target they log under.
    #[test]
    fn pace_check_filters_the_reports_target() {
        let script = include_str!("../../../scripts/pace-check.py");
        assert!(
            script.contains(&format!("{}=debug", super::TARGET)),
            "scripts/pace-check.py's RUST_LOG must enable {}=debug",
            super::TARGET
        );
    }

    use super::*;

    fn slow() -> Duration {
        2 * Duration::from_millis(PAINT_FRAME_MS) + Duration::from_millis(1)
    }

    /// A frame over twice the interval is reported at debug, with its
    /// transmits; one within it is not.
    #[test]
    fn a_frame_over_twice_the_interval_is_reported() {
        let t0 = Instant::now();
        let logged = crate::test_capture::capture(|| {
            let mut jank = Jank::new();
            jank.record(
                Duration::from_millis(PAINT_FRAME_MS),
                Duration::ZERO,
                None,
                None,
                t0,
            );
            let send = FrameSend {
                dirty: Painted::All,
                fresh: true,
                changed: 1275,
                sent: 1275,
                bytes: 1_400_000,
                encode: Duration::from_millis(30),
            };
            jank.record(slow(), Duration::from_millis(7), Some(send), None, t0);
        });
        assert_eq!(logged.matches("frame jank").count(), 1, "{logged}");
        let line = logged
            .lines()
            .find(|l| l.contains("frame jank"))
            .unwrap_or_default();
        assert!(
            line.contains(" DEBUG "),
            "a frame's own report never reaches the warn log: {line}"
        );
        assert!(logged.contains("dirty=\"all\""), "{logged}");
        assert!(logged.contains("sent=1275"), "{logged}");
        assert!(logged.contains("fresh=true"), "{logged}");
        assert!(logged.contains("present=7"), "{logged}");
        let produce = slow() - Duration::from_millis(30 + 7);
        assert!(
            logged.contains(&format!("produce={}", produce.as_secs_f64() * 1000.0)),
            "the present is not the scene's: {logged}"
        );
    }

    /// A frame past its interval but short of twice it is a debug `frame
    /// slow`, not a jank, and counts as over; the interval is the schedule's.
    #[test]
    fn a_frame_past_its_scheduled_interval_is_slow() {
        let t0 = Instant::now();
        let logged = crate::test_capture::capture(|| {
            let mut jank = Jank::new();
            jank.scheduled_every(Duration::from_millis(125));
            jank.record(Duration::from_millis(100), Duration::ZERO, None, None, t0);
            jank.record(Duration::from_millis(150), Duration::ZERO, None, None, t0);
            jank.finish(t0);
        });
        assert_eq!(logged.matches("frame slow").count(), 1, "{logged}");
        assert_eq!(logged.matches("frame jank").count(), 0, "{logged}");
        assert!(logged.contains("over=1"), "{logged}");
        assert!(logged.contains("janks=0"), "{logged}");
    }

    /// A frame hitches by how far it showed past its interval, and one
    /// shown within it never does, however late in it. Every 33 ms, due at 0,
    /// 33, 66 and 132 ms and shown at 5, 60, 110 and 133: only the third is
    /// past its 99 ms, by 11. A redraw with no due counts neither way.
    #[test]
    fn a_frame_hitches_by_how_far_it_showed_past_its_interval() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let logged = crate::test_capture::capture(|| {
            let mut jank = Jank::new();
            jank.scheduled_every(ms(33));
            for (due, at) in [(0, 5), (33, 60), (66, 110), (132, 133)] {
                jank.due_at(t0 + ms(due));
                jank.record(ms(1), Duration::ZERO, None, None, t0 + ms(at));
            }
            // A redraw no schedule asked for, however late.
            jank.record(ms(1), Duration::ZERO, None, None, t0 + ms(900));
            jank.finish(t0);
        });
        let line = logged
            .lines()
            .find(|l| l.contains("frame pacing"))
            .unwrap_or_default();
        let late = ms(11).as_secs_f64() * 1000.0;
        assert!(line.contains(&format!("hitch_ms={late:?} ")), "{line}");
        assert!(line.contains("scheduled=4 "), "{line}");
        let interval = ms(33).as_secs_f64() * 1000.0;
        assert!(
            line.contains(&format!("interval_ms={interval:?}")),
            "{line}"
        );
    }

    /// A window that closes reports its frames' spread and janks, then starts
    /// afresh.
    #[test]
    fn a_closed_window_reports_its_spread_and_starts_afresh() {
        let t0 = Instant::now();
        let logged = crate::test_capture::capture(|| {
            let mut jank = Jank::new();
            jank.painted_by(Painter {
                look: "kitty",
                scale: 16,
                tmux: true,
                terminal: Some("ghostty".into()),
                sync: true,
            });
            for _ in 0..99 {
                jank.record(Duration::from_millis(10), Duration::ZERO, None, None, t0);
            }
            jank.record(slow(), Duration::ZERO, None, None, t0 + WINDOW);
            jank.record(
                Duration::from_millis(10),
                Duration::ZERO,
                None,
                None,
                t0 + WINDOW,
            );
        });
        let summaries: Vec<&str> = logged
            .lines()
            .filter(|l| l.contains("frame pacing"))
            .collect();
        assert_eq!(summaries.len(), 1, "{logged}");
        let line = summaries[0];
        assert!(line.contains("frames=100"), "{line}");
        assert!(line.contains("janks=1"), "{line}");
        assert!(line.contains("over=1"), "{line}");
        assert!(line.contains("look=\"kitty\""), "{line}");
        assert!(line.contains("tmux=true"), "{line}");
        assert!(line.contains("sync=true"), "{line}");
        assert!(line.contains("p50=10"), "{line}");
        assert!(
            line.contains(" WARN "),
            "a window with a jank warns: {line}"
        );
    }

    /// A run that ends inside a window still reports it.
    #[test]
    fn an_unfinished_window_is_reported_at_exit() {
        let t0 = Instant::now();
        let logged = crate::test_capture::capture(|| {
            let mut jank = Jank::new();
            jank.record(Duration::from_millis(10), Duration::ZERO, None, None, t0);
            jank.finish(t0);
        });
        assert!(logged.contains("frame pacing"), "{logged}");
        assert!(logged.contains("frames=1"), "{logged}");
    }

    /// The first window's span starts at its first frame, not at the boot
    /// before it, which a rate over it would count as frames on time.
    #[test]
    fn the_first_span_starts_at_the_first_frame() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let logged = crate::test_capture::capture(|| {
            let mut jank = Jank::new();
            jank.record(ms(1), Duration::ZERO, None, None, t0 + ms(5000));
            jank.finish(t0 + ms(6000));
        });
        let span = ms(1001).as_secs_f64() * 1000.0;
        assert!(logged.contains(&format!("span_ms={span:?} ")), "{logged}");
    }

    /// A summary names the span it covers, what the fluency gate rates its
    /// hitch time over: a painter that records fewer frames than its interval
    /// fits, as the floating window's idle beats do, still covers its span.
    #[test]
    fn a_summary_reports_the_span_it_covers() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let logged = crate::test_capture::capture(|| {
            let mut jank = Jank::new();
            // A 1 ms frame shown at 1 ms: the window begins at `t0`.
            jank.record(ms(1), Duration::ZERO, None, None, t0 + ms(1));
            jank.record(ms(1), Duration::ZERO, None, None, t0 + ms(126));
            jank.record(ms(1), Duration::ZERO, None, None, t0 + WINDOW);
            jank.record(ms(1), Duration::ZERO, None, None, t0 + WINDOW + ms(250));
            jank.finish(t0 + WINDOW + ms(2500));
        });
        let spans: Vec<&str> = logged
            .lines()
            .filter(|l| l.contains("frame pacing"))
            .collect();
        let window = WINDOW.as_secs_f64() * 1000.0;
        let tail = ms(2500).as_secs_f64() * 1000.0;
        assert_eq!(spans.len(), 2, "{logged}");
        assert!(
            spans[0].contains(&format!("span_ms={window:?} ")),
            "{}",
            spans[0]
        );
        assert!(
            spans[1].contains(&format!("span_ms={tail:?} ")),
            "{}",
            spans[1]
        );
    }
}
