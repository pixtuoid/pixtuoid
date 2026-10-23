//! Stateless easing curves for animations, and the wall-clock ms helpers every
//! layer shares.
//!
//! Wall-clock `SystemTime`, not `Instant`: matches the rest of the animation
//! state (FloorTransition, VacancyDim, PoseHistory) — serializable, no
//! out-of-process consumer today.

use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    Linear,
    EaseOutCubic,
    EaseInOutCubic,
    EaseInQuad,
    /// GLSL's `smoothstep` Hermite, `t²(3 − 2t)`
    /// (<https://registry.khronos.org/OpenGL-Refpages/gl4/html/smoothstep.xhtml>):
    /// flat at both ends, so what it moves leaves and lands without a jolt.
    Smoothstep,
}

impl Easing {
    /// Apply the easing curve to a normalized `t ∈ [0.0, 1.0]`.
    pub(crate) fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
            Easing::EaseInOutCubic => {
                if t < 0.5 {
                    4.0 * t.powi(3)
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
            Easing::EaseInQuad => t * t,
            Easing::Smoothstep => t * t * (3.0 - 2.0 * t),
        }
    }
}

/// Milliseconds elapsed from `since` to `now`, saturating to `0` on a backward
/// clock (`now < since`). Sites that deliberately SKIP or return `None` on a
/// backward clock mean something different — don't migrate those here.
pub(crate) fn elapsed_ms(now: SystemTime, since: SystemTime) -> u64 {
    now.duration_since(since)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

/// One hour in ms — the grid gen-media's fixed clocks sit on (`--now-hour`).
pub(crate) const HOUR_MS: u64 = 3_600_000;

/// Milliseconds since the Unix epoch for `now` (0 if the clock is before it).
/// It lives HERE, below both the model and the render layer, so a model module
/// never imports the renderer for it.
pub(crate) fn epoch_ms(now: SystemTime) -> u64 {
    elapsed_ms(now, SystemTime::UNIX_EPOCH)
}

/// How much of the office's ambient life moves: the loops that only make it
/// look alive — a flicker, a twinkle, an idle wander — never what an agent is
/// doing. Each tier steps those loops on its own clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Motion {
    /// Every loop, stepped each [`FULL_TICK_MS`].
    #[default]
    Full,
    /// Every loop at a quarter of Full's pace: each `CALM_TICK_MS` it steps
    /// one Full tick, so a loop plays Full's very sequence, slower, and none
    /// aliases on the slower repaint.
    Calm,
    /// No loop moves and no lightning flashes: each holds its first frame.
    Still,
}

/// How often a [`Motion::Full`] loop steps.
pub const FULL_TICK_MS: u64 = 125;
/// How often a [`Motion::Calm`] loop steps.
pub(crate) const CALM_TICK_MS: u64 = 500;
/// The rate the TUI and the floating window repaint an office with agents in
/// it, on every tier: the sampling rate the walks' strides are sized for
/// (`a_walking_person_never_slides`).
pub const PAINT_FPS: u32 = 30;
/// One [`PAINT_FPS`] frame, in whole ms.
pub const PAINT_FRAME_MS: u64 = 1000 / PAINT_FPS as u64;

/// The least any phase of a flash — a strike's level, a starved neon's catch,
/// the dark between — lasts in loop time: the project's own floor, beside
/// [WCAG 2.3.1]'s three flashes in any one second.
///
/// [WCAG 2.3.1]: https://www.w3.org/TR/WCAG22/#three-flashes-or-below-threshold
pub const PHOTOSENSITIVE_PHASE_MIN_MS: u64 = 100;

/// The most flashes [WCAG 2.3.1] allows in any one second.
///
/// [WCAG 2.3.1]: https://www.w3.org/TR/WCAG22/#three-flashes-or-below-threshold
#[cfg(test)]
pub(crate) const PHOTOSENSITIVE_FLASHES_PER_SECOND: usize = 3;

/// The least change of a level, as a share of full, that counts toward a
/// flash: [WCAG]'s general flash threshold.
///
/// [WCAG]: https://www.w3.org/TR/WCAG22/#dfn-general-flash-and-red-flash-thresholds
#[cfg(test)]
const FLASH_MIN_CHANGE: f32 = 0.1;

/// The most flashes in any one second of `samples`, each `(wall ms, level)`
/// in time order with the level a share of full: a flash is a pair of
/// opposing changes, each at least [`FLASH_MIN_CHANGE`], and a run of changes
/// one way is one change.
#[cfg(test)]
pub(crate) fn most_flashes_in_a_second(samples: impl IntoIterator<Item = (u64, f32)>) -> usize {
    const SECOND_MS: u64 = 1000;
    let mut samples = samples.into_iter();
    let Some((_, mut settled)) = samples.next() else {
        return 0;
    };
    let mut turns: Vec<(u64, bool)> = Vec::new();
    for (ms, level) in samples {
        if (level - settled).abs() < FLASH_MIN_CHANGE {
            continue;
        }
        let up = level > settled;
        settled = level;
        if turns.last().is_none_or(|&(_, was_up)| was_up != up) {
            turns.push((ms, up));
        }
    }
    let mut first = 0;
    let mut most = 0;
    for last in 0..turns.len() {
        while turns[last].0 - turns[first].0 >= SECOND_MS {
            first += 1;
        }
        most = most.max((last - first + 1).div_ceil(2));
    }
    most
}

impl Motion {
    /// Every tier.
    pub(crate) const ALL: [Motion; 3] = [Motion::Full, Motion::Calm, Motion::Still];

    /// Wall-clock ms per ms of loop time; `None` at rest.
    pub(crate) const fn pace(self) -> Option<u64> {
        match self {
            Self::Full => Some(1),
            Self::Calm => Some(CALM_TICK_MS / FULL_TICK_MS),
            Self::Still => None,
        }
    }

    /// The ambient clock at `now`, a function of `now` alone so a frame is
    /// too: Full's is the instant floored to its tick, Calm's that many
    /// repaints of Full ticks.
    pub(crate) fn beat(self, now: SystemTime) -> Beat {
        match self.pace() {
            Some(pace) => Beat::looping(loop_time(now, pace), pace),
            None => Beat {
                loop_ms: None,
                pace: 1,
            },
        }
    }

    /// `now` and its [`beat`](Self::beat).
    pub(crate) fn timing(self, now: SystemTime) -> Timing {
        Timing {
            now,
            beat: self.beat(now),
        }
    }

    /// How long after `wall` this tier's [`beat`](Self::beat) next turns, as
    /// [`loop_time`] steps; `None` at rest, which never turns.
    pub fn until_next_beat(self, wall: SystemTime) -> Option<std::time::Duration> {
        let period = FULL_TICK_MS * self.pace()?;
        let turn = (epoch_ms(wall) / period + 1) * period;
        let since = wall
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        Some(std::time::Duration::from_millis(turn).saturating_sub(since))
    }
}

/// Loop time at `wall` for a tier `pace` wall ms to the loop ms: whole Full
/// ticks, one each `pace` of them.
fn loop_time(wall: SystemTime, pace: u64) -> u64 {
    epoch_ms(wall) / (FULL_TICK_MS * pace) * FULL_TICK_MS
}

/// A painter's `[p]ause`: while paused, every read of the clock gives the
/// instant the pause began, so the whole office holds still and resumes from
/// where it stood.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PauseClock {
    paused: bool,
    frozen: Option<SystemTime>,
}

impl PauseClock {
    /// Pause, or resume.
    pub fn toggle(&mut self) {
        self.paused = !self.paused;
    }

    /// Whether the clock is paused.
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// The frame's time at `wall`: `wall` itself, or the instant the pause
    /// began.
    pub fn now(&mut self, wall: SystemTime) -> SystemTime {
        if self.paused {
            *self.frozen.get_or_insert(wall)
        } else {
            self.frozen = None;
            wall
        }
    }
}

/// An instant, and the [`Beat`] its ambient loops read at it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    pub(crate) now: SystemTime,
    pub(crate) beat: Beat,
}

impl Timing {
    /// The timing `by` later on the same tier.
    pub(crate) fn later(self, by: std::time::Duration) -> Self {
        let now = self.now + by;
        Self {
            now,
            beat: match self.beat.loop_ms {
                Some(_) => Beat::looping(self.beat.loop_at(now), self.beat.pace),
                None => self.beat,
            },
        }
    }
}

/// The clock every ambient loop reads instead of the wall clock, so two
/// instants on one beat paint the same frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Beat {
    /// Loop time, in ms; `None` at rest.
    loop_ms: Option<u64>,
    /// Wall-clock ms per ms of loop time.
    pace: u64,
}

impl Beat {
    fn looping(loop_ms: u64, pace: u64) -> Self {
        Self {
            loop_ms: Some(loop_ms),
            pace,
        }
    }

    /// Milliseconds of loop time on the beat; 0 at rest, where every loop
    /// holds its first frame.
    pub(crate) fn ms(self) -> u64 {
        self.loop_ms.unwrap_or(0)
    }

    /// Whether nothing moves: a source with no frame to hold, a flash, shows
    /// none.
    pub(crate) fn is_rest(self) -> bool {
        self.loop_ms.is_none()
    }

    /// The loop time this beat's tier reads at `wall`: what [`ms`](Self::ms)
    /// is when `wall` is now, so an ambient walk times its start and its
    /// present on one clock.
    pub(crate) fn loop_at(self, wall: SystemTime) -> u64 {
        loop_time(wall, self.pace)
    }

    /// The wall-clock ms loop time `loop_ms` plays at, where what keeps real
    /// time — the weather — is read.
    pub(crate) fn wall_ms(self, loop_ms: u64) -> u64 {
        loop_ms.saturating_mul(self.pace)
    }

    /// A beat at exactly `ms`, off any tier's tick.
    #[cfg(test)]
    pub(crate) fn at_ms(ms: u64) -> Self {
        Self::looping(ms, 1)
    }
}

/// Compute the eased progress of an animation `[0.0, 1.0]` given its
/// `started_at` wall-clock time, total `duration_ms`, and `easing` curve.
pub fn eased_progress(
    started_at: SystemTime,
    duration_ms: u32,
    easing: Easing,
    now: SystemTime,
) -> f32 {
    let elapsed = elapsed_ms(now, started_at) as f32;
    let raw = if duration_ms == 0 {
        1.0
    } else {
        (elapsed / duration_ms as f32).clamp(0.0, 1.0)
    };
    easing.apply(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// A tier's beat holds until the instant `until_next_beat` names, and
    /// turns at it.
    #[test]
    fn the_beat_turns_when_until_next_beat_says() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        for motion in [Motion::Full, Motion::Calm] {
            for off in [0, 1, 62, 124, 125, 499, 777].map(Duration::from_millis) {
                let wall = base + off;
                let until = motion.until_next_beat(wall).expect("a moving tier turns");
                let turn = wall + until;
                // A whole ms before: the beat floors whole ms, and a nanosecond
                // before rounds away where "the time is represented in 100
                // nanosecond intervals" (Windows,
                // doc.rust-lang.org/std/time/struct.SystemTime.html).
                assert_eq!(
                    motion.beat(wall),
                    motion.beat(turn - Duration::from_millis(1)),
                    "{motion:?} turned before {off:?} + {until:?}"
                );
                assert_ne!(
                    motion.beat(wall),
                    motion.beat(turn),
                    "{motion:?} held past {off:?} + {until:?}"
                );
            }
        }
        assert_eq!(Motion::Still.until_next_beat(base), None);
    }

    /// A timing taken later on a tier is the one the tier gives that instant.
    #[test]
    fn a_later_timing_is_the_tiers_own_then() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        for motion in Motion::ALL {
            for by in [1, PAINT_FRAME_MS, 499, 1000, 60_000].map(Duration::from_millis) {
                let later = motion.timing(base).later(by);
                let then = motion.timing(base + by);
                assert_eq!(
                    (later.now, later.beat),
                    (then.now, then.beat),
                    "{motion:?} {by:?}"
                );
            }
        }
    }

    /// `flashes` square flashes, one each `period_ms`: up at its start, down
    /// halfway through.
    fn square(flashes: u64, period_ms: u64) -> Vec<(u64, f32)> {
        std::iter::once((0, 0.0))
            .chain((0..flashes).flat_map(|k| {
                let start = 1 + k * period_ms;
                [(start, 1.0), (start + period_ms / 2, 0.0)]
            }))
            .collect()
    }

    /// WCAG's general flash: a rise and a fall of 10% of full or more.
    #[test]
    fn a_flash_is_a_rise_and_a_fall_of_a_tenth_or_more() {
        let pulse = |level: f32| most_flashes_in_a_second([(0, 0.0), (100, level), (200, 0.0)]);
        assert_eq!(pulse(0.10), 1);
        assert_eq!(pulse(0.09), 0);
    }

    /// Four flashes spread so no second holds more than three pass; the same
    /// four inside a second do not.
    #[test]
    fn flashes_are_counted_in_any_one_second() {
        assert_eq!(most_flashes_in_a_second(square(4, 350)), 3);
        assert_eq!(most_flashes_in_a_second(square(4, 200)), 4);
    }

    /// A slow ramp, each step under the threshold, is one change once it has
    /// moved past it, so a ramp up and back down is one flash.
    #[test]
    fn a_slow_ramp_is_one_change() {
        const STEPS: u64 = 10;
        let step = 2.0 * FLASH_MIN_CHANGE / STEPS as f32;
        let up = (0..=STEPS).map(|i| (i * 20, step * i as f32));
        let down = (1..=STEPS).map(|i| ((STEPS + i) * 20, step * (STEPS - i) as f32));
        assert_eq!(most_flashes_in_a_second(up.chain(down)), 1);
    }

    /// Changes one way in a row are one change: a rise in two steps and a
    /// fall in two is one flash.
    #[test]
    fn a_run_of_changes_one_way_is_one_change() {
        let stepped = [(0, 0.0), (20, 0.5), (40, 1.0), (60, 0.5), (80, 0.0)];
        assert_eq!(most_flashes_in_a_second(stepped), 1);
    }

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn at(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
    }

    #[test]
    fn each_moving_tier_steps_its_loops_once_a_tick() {
        const T0: u64 = 1_700_000_000_000;
        for (motion, tick) in [(Motion::Full, FULL_TICK_MS), (Motion::Calm, CALM_TICK_MS)] {
            let beat = |ms| motion.beat(at(T0 + ms));
            assert_eq!(beat(0), beat(tick - 1), "{motion:?} moved inside a tick");
            assert_ne!(beat(tick - 1), beat(tick), "{motion:?} held past its tick");
        }
    }

    /// Calm's repaint advances its loops by exactly one Full tick, so a loop
    /// whose frames hold whole Full ticks visits each in turn: a two-frame loop
    /// of any whole-tick period flips within that many repaints, where a plain
    /// 500 ms sample of a 250 ms loop would land on one frame forever.
    #[test]
    fn calm_plays_every_whole_tick_loop_without_aliasing() {
        const T0: u64 = 1_700_000_000_000;
        for ticks in 1..=10 {
            let period = ticks * FULL_TICK_MS;
            let frames: std::collections::HashSet<u64> = (0..=ticks)
                .map(|repaint| Motion::Calm.beat(at(T0 + repaint * CALM_TICK_MS)).ms() / period % 2)
                .collect();
            assert_eq!(frames.len(), 2, "a {period} ms two-frame loop aliased");
        }
        let step = |n: u64| Motion::Calm.beat(at(T0 + n * CALM_TICK_MS)).ms();
        assert_eq!(step(1) - step(0), FULL_TICK_MS, "one Full tick a repaint");
    }

    #[test]
    fn still_rests_at_every_instant() {
        let beat = |ms| Motion::Still.beat(at(ms));
        assert!(beat(0).is_rest());
        assert_eq!(beat(1_700_000_000_000), beat(1_700_003_600_000));
        assert_eq!(beat(0).ms(), 0, "a loop at rest holds its first frame");
    }

    #[test]
    fn ease_out_cubic_endpoints() {
        assert!(approx_eq(Easing::EaseOutCubic.apply(0.0), 0.0));
        assert!(approx_eq(Easing::EaseOutCubic.apply(1.0), 1.0));
        assert!(Easing::EaseOutCubic.apply(0.5) > 0.5);
    }

    #[test]
    fn ease_in_out_cubic_endpoints() {
        assert!(approx_eq(Easing::EaseInOutCubic.apply(0.0), 0.0));
        assert!(approx_eq(Easing::EaseInOutCubic.apply(1.0), 1.0));
        assert!(approx_eq(Easing::EaseInOutCubic.apply(0.5), 0.5));
    }

    #[test]
    fn ease_in_quad_endpoints() {
        assert!(approx_eq(Easing::EaseInQuad.apply(0.0), 0.0));
        assert!(approx_eq(Easing::EaseInQuad.apply(1.0), 1.0));
        assert!(approx_eq(Easing::EaseInQuad.apply(0.5), 0.25));
    }

    #[test]
    fn smoothstep_is_the_hermite_cubic() {
        assert_eq!(Easing::Smoothstep.apply(0.0), 0.0);
        assert_eq!(Easing::Smoothstep.apply(1.0), 1.0);
        // 3t² − 2t³, exact in f32; a linear curve gives 0.25.
        assert_eq!(Easing::Smoothstep.apply(0.25), 0.15625);
    }

    #[test]
    fn all_curves_are_monotone_non_decreasing() {
        for curve in [
            Easing::Linear,
            Easing::EaseOutCubic,
            Easing::EaseInOutCubic,
            Easing::EaseInQuad,
            Easing::Smoothstep,
        ] {
            let mut prev = -1.0_f32;
            for i in 0..=100 {
                let t = i as f32 / 100.0;
                let v = curve.apply(t);
                assert!(v >= prev, "{:?} not monotone at t={t}: {v} < {prev}", curve);
                prev = v;
            }
        }
    }

    #[test]
    fn out_of_range_inputs_clamp() {
        assert!(approx_eq(Easing::Linear.apply(-1.0), 0.0));
        assert!(approx_eq(Easing::Linear.apply(2.0), 1.0));
        assert!(approx_eq(Easing::EaseOutCubic.apply(-0.5), 0.0));
        assert!(approx_eq(Easing::EaseInOutCubic.apply(1.5), 1.0));
    }

    #[test]
    fn eased_progress_at_start_is_zero() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let p = eased_progress(start, 200, Easing::Linear, start);
        assert!(approx_eq(p, 0.0));
    }

    #[test]
    fn eased_progress_at_end_is_one() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let now = start + Duration::from_millis(200);
        let p = eased_progress(start, 200, Easing::Linear, now);
        assert!(approx_eq(p, 1.0));
    }

    #[test]
    fn eased_progress_past_end_clamps_to_one() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let now = start + Duration::from_secs(60);
        let p = eased_progress(start, 200, Easing::Linear, now);
        assert!(approx_eq(p, 1.0));
    }

    #[test]
    fn eased_progress_now_before_start_clamps_to_zero() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let now = start - Duration::from_secs(5);
        let p = eased_progress(start, 200, Easing::Linear, now);
        assert!(approx_eq(p, 0.0));
    }

    #[test]
    fn eased_progress_zero_duration_is_complete() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert!(approx_eq(
            eased_progress(start, 0, Easing::Linear, start),
            1.0
        ));
        assert!(approx_eq(
            eased_progress(start, 0, Easing::EaseOutCubic, start),
            1.0
        ));
    }

    #[test]
    fn elapsed_ms_counts_forward_and_saturates_backward() {
        let since = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(elapsed_ms(since + Duration::from_millis(250), since), 250);
        assert_eq!(elapsed_ms(since, since), 0);
        assert_eq!(elapsed_ms(since - Duration::from_secs(5), since), 0);
    }

    #[test]
    fn eased_progress_applies_curve() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let now = start + Duration::from_millis(100);
        let linear = eased_progress(start, 200, Easing::Linear, now);
        let eased = eased_progress(start, 200, Easing::EaseOutCubic, now);
        assert!(approx_eq(linear, 0.5));
        assert!(
            eased > 0.8,
            "expected ease-out to be past 80% at midpoint; got {eased}"
        );
    }

    /// The site's backdrop runs the wasm office on its own frame gate.
    #[test]
    fn the_site_backdrop_paints_at_paint_fps() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../site/src/components/OfficeBackdrop.astro"
        );
        // crates.io-packaged test runs don't ship the repo's site/ tree.
        let site = concat!(env!("CARGO_MANIFEST_DIR"), "/../../site");
        if !std::path::Path::new(site).is_dir() {
            return;
        }
        let astro =
            std::fs::read_to_string(path).expect("OfficeBackdrop.astro moved or was renamed");
        let frame_ms: u64 = astro
            .lines()
            .find_map(|l| l.trim().strip_prefix("const FRAME_MS = "))
            .and_then(|v| v.strip_suffix(';'))
            .expect("OfficeBackdrop.astro's `const FRAME_MS = <ms>;` moved or was renamed")
            .parse()
            .expect("FRAME_MS is a whole ms literal");
        assert_eq!(frame_ms, PAINT_FRAME_MS, "OfficeBackdrop.astro's FRAME_MS");
    }
}
