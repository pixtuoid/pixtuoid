//! The floating window's PAINT CADENCE, kept pure so the animation throttle is
//! unit-testable while `window.rs` stays winit glue.
//!
//! The subtlety this module exists for: `ApplicationHandler::about_to_wait` runs
//! on EVERY event-loop iteration, so an unconditional `Window::request_redraw()`
//! there leaves a redraw pending whenever the loop reaches its wait — the
//! `ControlFlow::WaitUntil` deadline set beside it then never sleeps, and the
//! window renders + presents back-to-back at 100% of a CPU core. Gating the
//! redraw REQUEST on a deadline, not just arming the wait, is what makes the
//! cadence below take effect.

use std::time::{Duration, Instant};

use pixtuoid_scene::anim::PAINT_FRAME;
use pixtuoid_scene::floor::REST_TICK;

/// What one `about_to_wait` pass is to do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Due {
    /// Paint the frame that fell due at this instant.
    Paint(Instant),
    /// Feed the audio without painting: the window is out of sight.
    Hear,
}

pub(crate) struct FrameClock {
    next: Instant,
    /// Whether the frame last due was held back by the flash hold.
    retry: bool,
    /// Whether the window is out of sight ([`winit::event::WindowEvent::Occluded`]):
    /// nothing paints, and the audio still hears the office each
    /// [`REST_TICK`], as a resting office's paints feed it.
    occluded: bool,
}

impl FrameClock {
    /// Armed to paint immediately, so the first `about_to_wait` after window
    /// creation still draws.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            next: now,
            retry: false,
            occluded: false,
        }
    }

    /// The window went out of sight, or came back at `now`, where it paints
    /// at once.
    pub(crate) fn occlude(&mut self, occluded: bool, now: Instant) {
        self.occluded = occluded;
        if !occluded {
            self.next = now;
        }
    }

    /// Whether a redraw may paint: not while the window is out of sight.
    pub(crate) fn paints(&self) -> bool {
        !self.occluded
    }

    /// The frame last due was held back: the next poll retries it a
    /// [`PAINT_FRAME`] on, not at the office's next wait, past which a one-beat
    /// phase is gone.
    pub(crate) fn retry(&mut self) {
        self.retry = true;
    }

    /// One `about_to_wait` pass at `now`, the office next needing a paint
    /// `until` later ([`OfficeSession::until_next_paint`], `None` each
    /// [`PAINT_FRAME`]): `(due, deadline)` — what to do NOW, if anything, and
    /// the instant the loop should wait until.
    ///
    /// [`OfficeSession::until_next_paint`]: pixtuoid_scene::floor::OfficeSession::until_next_paint
    pub(crate) fn poll(&mut self, now: Instant, until: Option<Duration>) -> (Option<Due>, Instant) {
        let retry = std::mem::take(&mut self.retry);
        let wait = if self.occluded {
            REST_TICK
        } else if retry {
            PAINT_FRAME
        } else {
            until.unwrap_or(PAINT_FRAME)
        };
        if now >= self.next {
            let due = if self.occluded {
                Due::Hear
            } else {
                Due::Paint(self.next)
            };
            self.next = now + wait;
            return (Some(due), self.next);
        }
        // A cadence SPEED-UP (an agent arriving mid-beat) must not sit out the
        // slow deadline already armed.
        self.next = self.next.min(now + wait);
        (None, self.next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_scene::anim::PAINT_FPS;

    /// How far past the first pass `clock` paints over `span`, polled every
    /// `step`, the office next needing a paint `until` later.
    fn paints_over(until: Option<Duration>, span: Duration, step: Duration) -> Vec<Duration> {
        let t0 = Instant::now();
        let mut clock = FrameClock::new(t0);
        let mut painted = Vec::new();
        let mut elapsed = Duration::ZERO;
        while elapsed <= span {
            if let (Some(Due::Paint(_)), _) = clock.poll(t0 + elapsed, until) {
                painted.push(elapsed);
            }
            elapsed += step;
        }
        painted
    }

    #[test]
    fn the_first_pass_paints_so_the_window_is_never_blank() {
        let t0 = Instant::now();
        let mut clock = FrameClock::new(t0);
        assert_eq!(
            clock.poll(t0, None),
            (Some(Due::Paint(t0)), t0 + PAINT_FRAME)
        );
    }

    #[test]
    fn a_moving_office_paints_at_paint_fps_not_once_per_event_loop_iteration() {
        let painted = paints_over(None, Duration::from_secs(1), Duration::from_millis(1)).len();
        // A band, not equality: 1ms poll quantization can slip a tick by a ms.
        assert!(
            (PAINT_FPS as usize - 2..=PAINT_FPS as usize + 1).contains(&painted),
            "a moving office must paint ~PAINT_FPS times per second, not once per \
             event-loop iteration (got {painted} over 1001 passes)"
        );
    }

    /// An office that names its next paint is painted exactly that long after
    /// each paint, a beat's wait and a rest tick's alike.
    #[test]
    fn a_resting_office_paints_when_it_says() {
        let span = Duration::from_secs(3);
        for wait in [Duration::from_millis(126), Duration::from_secs(1)] {
            let painted = paints_over(Some(wait), span, Duration::from_millis(1));
            let waits = (span.as_millis() / wait.as_millis()) as usize;
            assert_eq!(painted.len(), waits + 1, "{wait:?}");
            for pair in painted.windows(2) {
                assert_eq!(pair[1] - pair[0], wait, "{wait:?}");
            }
        }
    }

    #[test]
    fn an_idle_to_active_transition_does_not_wait_out_the_slow_deadline() {
        let t0 = Instant::now();
        let mut clock = FrameClock::new(t0);
        let rest = Duration::from_secs(1);
        assert!(clock.poll(t0, Some(rest)).0.is_some()); // a rest paint, armed a second out
        let soon = Duration::from_millis(10);
        let (paint, deadline) = clock.poll(t0 + soon, None);
        assert!(
            paint.is_none(),
            "10ms after a paint there is nothing to draw yet"
        );
        assert_eq!(
            deadline,
            t0 + soon + PAINT_FRAME,
            "an agent arriving mid-rest must pull the deadline in to the active \
             cadence, not sit out the rest of the second"
        );
    }

    /// A frame the flash hold kept back is retried a frame on, not at the
    /// office's next wait, and the wait after it is the office's again.
    #[test]
    fn a_held_frame_is_retried_a_frame_on() {
        let t0 = Instant::now();
        let rest = Duration::from_secs(1);
        let mut clock = FrameClock::new(t0);
        assert!(clock.poll(t0, Some(rest)).0.is_some());
        clock.retry();
        let soon = t0 + Duration::from_millis(1);
        assert_eq!(clock.poll(soon, Some(rest)), (None, soon + PAINT_FRAME));
        let retried = soon + PAINT_FRAME;
        assert_eq!(
            clock.poll(retried, Some(rest)),
            (Some(Due::Paint(retried)), retried + rest)
        );
    }

    #[test]
    fn the_returned_deadline_is_when_the_next_paint_becomes_due() {
        let t0 = Instant::now();
        let mut clock = FrameClock::new(t0);
        let (_, deadline) = clock.poll(t0, None);
        assert!(
            clock
                .poll(deadline - Duration::from_nanos(1), None)
                .0
                .is_none()
        );
        assert_eq!(
            clock.poll(deadline, None).0,
            Some(Due::Paint(deadline)),
            "due when armed"
        );
    }

    /// An out-of-sight window paints nothing, however busy its office, but
    /// hears it each rest tick, sleeping between; seen again, it paints at
    /// once.
    #[test]
    fn an_occluded_window_hears_each_rest_tick_and_paints_once_seen() {
        let t0 = Instant::now();
        let mut clock = FrameClock::new(t0);
        assert_eq!(clock.poll(t0, None).0, Some(Due::Paint(t0)));
        clock.occlude(true, t0);
        assert!(!clock.paints());
        let step = Duration::from_millis(1);
        let span = 3 * REST_TICK;
        let mut heard = Vec::new();
        let mut elapsed = step;
        while elapsed <= span {
            let now = t0 + elapsed;
            match clock.poll(now, None) {
                (Some(Due::Hear), deadline) => {
                    assert_eq!(deadline, now + REST_TICK, "it sleeps till the next hearing");
                    heard.push(elapsed);
                }
                (Some(paint), _) => panic!("an occluded window painted: {paint:?}"),
                (None, _) => {}
            }
            elapsed += step;
        }
        assert_eq!(heard.len(), 3, "{heard:?}");
        for pair in heard.windows(2) {
            assert_eq!(pair[1] - pair[0], REST_TICK);
        }
        let seen = t0 + span + step;
        clock.occlude(false, seen);
        assert!(clock.paints());
        assert_eq!(clock.poll(seen, None).0, Some(Due::Paint(seen)));
    }
}
