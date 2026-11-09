//! What the window loop does around each redraw and at exit, apart from winit
//! and the GPU, so a test drives it with fakes: `FloatingApp` cannot be built
//! without a live event loop.

use crate::jank::Jank;

/// What one redraw touches.
pub(super) trait Redraw {
    fn jank(&mut self) -> &mut Jank;
    /// Whether a frame painted now would be seen
    /// ([`FrameClock::paints`](super::cadence::FrameClock::paints)).
    fn paints(&self) -> bool;
    /// Paint one frame, recording it with [`Jank::record`] once it has tried to
    /// present.
    fn paint(&mut self);
    fn recursor(&mut self);
}

/// One redraw: the frame, unless it would not be seen, then the cursor it
/// leaves under the pointer. Every way `paint` returns before it records, or
/// is skipped, skips `record`, the only thing that spends a scheduled
/// redraw's due, so it is spent here.
pub(super) fn redraw(app: &mut impl Redraw) {
    if app.paints() {
        app.paint();
    }
    app.jank().spend_due();
    app.recursor();
}

/// What the loop's end saves and releases.
pub(super) trait Teardown {
    /// Save what outlives the run: the window's geometry and zoom, read
    /// while the window is still held, and the frames' last pacing summary.
    fn persist(&mut self);
    /// Take the window off screen now. Dropping it does not: winit queues the
    /// destroy for an event loop that never runs again.
    fn hide_window(&mut self);
    /// Release the window and the surface drawn onto it.
    fn release_surface(&mut self);
    /// Flush audio state and join the device thread, which can wait out a
    /// synthesis build ([`crate::audio::AudioHandle::shutdown`]).
    fn stop_audio(&mut self);
}

/// The loop's end: the window is hidden before the audio join, so a quit does
/// not leave it up, frozen, while the device thread is waited out. Residual:
/// where winit cannot hide a window (Wayland,
/// [`winit::window::Window::set_visible`]) it stays until the process exits,
/// and macOS has closed it already.
pub(super) fn tear_down(parts: &mut impl Teardown) {
    parts.persist();
    parts.hide_window();
    parts.release_surface();
    parts.stop_audio();
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_scene::anim::PAINT_FRAME;
    use std::time::{Duration, Instant};

    struct Fake {
        jank: Jank,
        log: Vec<&'static str>,
        presents: bool,
        in_sight: bool,
        /// When a recorded frame showed.
        shown_at: Instant,
    }

    impl Redraw for Fake {
        fn jank(&mut self) -> &mut Jank {
            &mut self.jank
        }
        fn paints(&self) -> bool {
            self.in_sight
        }
        fn paint(&mut self) {
            self.log.push("paint");
            if self.presents {
                self.jank.record(
                    Duration::from_millis(1),
                    Duration::ZERO,
                    None,
                    None,
                    self.shown_at,
                );
            }
        }
        fn recursor(&mut self) {
            self.log.push("recursor");
        }
    }

    /// A scheduled frame `paint` skipped spends its due: the redraw after it,
    /// which no schedule asked for, is not booked as scheduled or late.
    #[test]
    fn a_skipped_scheduled_frame_is_not_booked_against_the_next_redraw() {
        let t0 = Instant::now();
        let logged = crate::test_capture::capture(|| {
            let mut app = Fake {
                jank: Jank::new(),
                log: Vec::new(),
                presents: false,
                in_sight: true,
                shown_at: t0 + 2 * PAINT_FRAME,
            };
            app.jank.due_at(t0);
            redraw(&mut app);
            assert_eq!(app.log, ["paint", "recursor"]);
            app.presents = true;
            redraw(&mut app);
            app.jank.finish(t0);
        });
        let line = logged
            .lines()
            .find(|l| l.contains("frame pacing"))
            .unwrap_or_default();
        assert!(line.contains("scheduled=0 "), "{line}");
        assert!(line.contains("hitch_ms=0.0 "), "{line}");
    }

    /// A frame that presents spends its due itself, so it still counts.
    #[test]
    fn a_presented_scheduled_frame_still_counts() {
        let logged = crate::test_capture::capture(|| {
            let mut app = Fake {
                jank: Jank::new(),
                log: Vec::new(),
                presents: true,
                in_sight: true,
                shown_at: Instant::now(),
            };
            app.jank.due_at(Instant::now());
            redraw(&mut app);
            app.jank.finish(Instant::now());
        });
        assert!(logged.contains("scheduled=1 "), "{logged}");
    }

    /// A redraw asked for while the window is out of sight paints nothing,
    /// and spends the due of a frame scheduled before it went.
    #[test]
    fn an_out_of_sight_redraw_paints_nothing_and_spends_its_due() {
        let t0 = Instant::now();
        let logged = crate::test_capture::capture(|| {
            let mut app = Fake {
                jank: Jank::new(),
                log: Vec::new(),
                presents: true,
                in_sight: false,
                shown_at: t0 + 2 * PAINT_FRAME,
            };
            app.jank.due_at(t0);
            redraw(&mut app);
            assert_eq!(app.log, ["recursor"]);
            app.in_sight = true;
            redraw(&mut app);
            app.jank.finish(t0);
        });
        let line = logged
            .lines()
            .find(|l| l.contains("frame pacing"))
            .unwrap_or_default();
        assert!(line.contains("scheduled=0 "), "{line}");
    }

    #[derive(Default)]
    struct Parts {
        log: Vec<&'static str>,
    }

    impl Teardown for Parts {
        fn persist(&mut self) {
            self.log.push("persisted");
        }
        fn hide_window(&mut self) {
            self.log.push("window hidden");
        }
        fn release_surface(&mut self) {
            self.log.push("surface released");
        }
        fn stop_audio(&mut self) {
            self.log.push("audio join");
        }
    }

    /// The geometry is saved while the window is still held, and the window
    /// is hidden before the audio join starts.
    #[test]
    fn the_run_persists_before_the_window_goes_and_hides_it_before_the_audio_join() {
        let mut parts = Parts::default();
        tear_down(&mut parts);
        assert_eq!(
            parts.log,
            [
                "persisted",
                "window hidden",
                "surface released",
                "audio join"
            ]
        );
    }
}
