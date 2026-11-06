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

/// Motion is time-driven, so a populated office must repaint continuously.
const ACTIVE_FPS: u32 = pixtuoid_scene::anim::PAINT_FPS;

/// One frame of a populated office.
pub(super) fn frame() -> Duration {
    Duration::from_secs(1) / ACTIVE_FPS
}

pub(crate) struct FrameClock {
    next: Instant,
}

impl FrameClock {
    /// Armed to paint immediately, so the first `about_to_wait` after window
    /// creation still draws.
    pub(crate) fn new(now: Instant) -> Self {
        Self { next: now }
    }

    /// One `about_to_wait` pass at `now`, the office next needing a paint
    /// `until` later ([`OfficeSession::until_next_paint`], `None` each
    /// [`frame`]): `(paint, deadline)` — the instant the frame to redraw NOW
    /// was due, if one is, and the instant the loop should wait until.
    ///
    /// [`OfficeSession::until_next_paint`]: pixtuoid_scene::floor::OfficeSession::until_next_paint
    pub(crate) fn poll(
        &mut self,
        now: Instant,
        until: Option<Duration>,
    ) -> (Option<Instant>, Instant) {
        let wait = until.unwrap_or_else(frame);
        if now >= self.next {
            let due = self.next;
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

    /// How far past the first pass `clock` paints over `span`, polled every
    /// `step`, the office next needing a paint `until` later.
    fn paints_over(until: Option<Duration>, span: Duration, step: Duration) -> Vec<Duration> {
        let t0 = Instant::now();
        let mut clock = FrameClock::new(t0);
        let mut painted = Vec::new();
        let mut elapsed = Duration::ZERO;
        while elapsed <= span {
            if clock.poll(t0 + elapsed, until).0.is_some() {
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
        assert_eq!(clock.poll(t0, None), (Some(t0), t0 + frame()));
    }

    #[test]
    fn an_active_office_paints_at_active_fps_not_once_per_event_loop_iteration() {
        let painted = paints_over(None, Duration::from_secs(1), Duration::from_millis(1)).len();
        // A band, not equality: 1ms poll quantization can slip a tick by a ms.
        assert!(
            (ACTIVE_FPS as usize - 2..=ACTIVE_FPS as usize + 1).contains(&painted),
            "an active office must paint ~ACTIVE_FPS times per second, not once per \
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
            t0 + soon + frame(),
            "an agent arriving mid-rest must pull the deadline in to the active \
             cadence, not sit out the rest of the second"
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
            Some(deadline),
            "due when armed"
        );
    }
}
