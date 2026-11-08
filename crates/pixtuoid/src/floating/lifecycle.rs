//! What the window loop does around each redraw and at exit, apart from winit
//! and the GPU, so a test drives it with fakes: `FloatingApp` cannot be built
//! without a live event loop.

use crate::jank::Jank;

/// What one redraw touches.
pub(super) trait Redraw {
    fn jank(&mut self) -> &mut Jank;
    /// Paint one frame, recording it with [`Jank::record`] once it has tried to
    /// present.
    fn paint(&mut self);
    fn recursor(&mut self);
}

/// One redraw: the frame, then the cursor it leaves under the pointer. Every
/// way `paint` returns before it records skips `record`, the only thing that
/// spends a scheduled redraw's due, so it is spent here.
pub(super) fn redraw(app: &mut impl Redraw) {
    app.paint();
    app.jank().spend_due();
    app.recursor();
}

/// What the loop's end releases.
pub(super) trait Teardown {
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
        /// When a recorded frame showed.
        shown_at: Instant,
    }

    impl Redraw for Fake {
        fn jank(&mut self) -> &mut Jank {
            &mut self.jank
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
                shown_at: Instant::now(),
            };
            app.jank.due_at(Instant::now());
            redraw(&mut app);
            app.jank.finish(Instant::now());
        });
        assert!(logged.contains("scheduled=1 "), "{logged}");
    }

    #[derive(Default)]
    struct Parts {
        log: Vec<&'static str>,
    }

    impl Teardown for Parts {
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

    /// The window is hidden before the audio join starts.
    #[test]
    fn the_window_is_hidden_before_the_audio_join_begins() {
        let mut parts = Parts::default();
        tear_down(&mut parts);
        assert_eq!(
            parts.log,
            ["window hidden", "surface released", "audio join"]
        );
    }
}
