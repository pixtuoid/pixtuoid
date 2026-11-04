//! A painter's [`Screen`]: the picture its last write put there, which a
//! pointer hits, and the hold every write goes through so each flash phase
//! stays on screen at least [`PHOTOSENSITIVE_PHASE_MIN_MS`]. The hold runs on
//! the painter's own monotonic clock, stamped when a write finishes, never on
//! a scene clock: a paused or stepped one can neither shorten a phase nor
//! wedge a repaint.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::anim::PHOTOSENSITIVE_PHASE_MIN_MS;

/// What of a floor's frame flashes: which of a strike's levels lights it,
/// whether a starved neon tube is catching, and the phase each of the sky's
/// fireworks bursts is in. Two frames flash alike iff equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FlashPhase {
    // The sky's flash level's bits: a strike's levels are exact constants.
    strike: u32,
    stutter: bool,
    bursts: [Option<crate::sky::fireworks::BurstPhase>; crate::sky::fireworks::LANES],
}

impl FlashPhase {
    /// The phase of `frame` composed under `sky`.
    pub(crate) fn of(sky: &crate::sky::Sky, frame: &crate::sim::SimFrame) -> Self {
        Self {
            strike: sky.flash().to_bits(),
            stutter: frame.neon_stutter,
            bursts: sky.bursts().map(|b| b.map(|b| b.phase)),
        }
    }
}

/// The phases a picture shows: a floor slide's two floors', or one floor's.
/// Only a session makes one that flashes, of its picture, so no painter's
/// hold can key on half a slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flashes([FlashPhase; 2]);

impl Flashes {
    /// One floor's picture.
    pub(crate) fn of_floor(phase: FlashPhase) -> Self {
        Self([phase; 2])
    }

    /// A slide's picture, `leaving` composed with `arriving`.
    pub(crate) fn of_slide(leaving: FlashPhase, arriving: FlashPhase) -> Self {
        Self([leaving, arriving])
    }
}

/// A painter's screen clock: monotonic time since an origin of its own.
pub type ScreenClock = Arc<dyn Fn() -> Duration + Send + Sync>;

/// The process's monotonic clock.
#[cfg(not(target_arch = "wasm32"))]
pub fn monotonic() -> ScreenClock {
    let origin = std::time::Instant::now();
    Arc::new(move || origin.elapsed())
}

/// The [`Flashes`] a painter's screen shows, on a screen of shape `S`, and when
/// they finished reaching it on the painter's [`ScreenClock`]: a write showing
/// others goes out the moment its frame is painted, but not before those on
/// screen have shown [`PHOTOSENSITIVE_PHASE_MIN_MS`]. A held frame is still
/// painted; only its write waits. A screen of a new shape shows nothing to
/// hold, so its write goes out whatever it shows.
pub(crate) struct FlashHold<S> {
    shown: Option<(Flashes, S, Duration)>,
    /// A write under way, what it shows and on what shape: shown only once
    /// it [`landed`](Self::landed).
    staged: Option<(Flashes, S)>,
    clock: ScreenClock,
}

impl<S: std::fmt::Debug> std::fmt::Debug for FlashHold<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlashHold")
            .field("shown", &self.shown)
            .field("staged", &self.staged)
            .finish_non_exhaustive()
    }
}

impl<S: Copy + PartialEq> FlashHold<S> {
    /// A hold on `clock`.
    pub(crate) fn on(clock: ScreenClock) -> Self {
        Self {
            shown: None,
            staged: None,
            clock,
        }
    }

    /// Whether a write showing `phases` to a screen shaped `shape` waits, now.
    /// A clock run backward reads as elapsed, so the screen never freezes.
    pub(crate) fn holds(&self, phases: Flashes, shape: S) -> bool {
        let now = (self.clock)();
        self.shown.is_some_and(|(on, on_shape, since)| {
            on != phases
                && on_shape == shape
                && now
                    .checked_sub(since)
                    .is_some_and(|d| d < Duration::from_millis(PHOTOSENSITIVE_PHASE_MIN_MS))
        })
    }

    /// Whether a write showing `phases` on a screen shaped `shape` changes
    /// the screen's: it goes out at once, whatever the painter's cadence. A
    /// new shape is a cleared screen, which shows nothing.
    pub(crate) fn changes(&self, phases: Flashes, shape: S) -> bool {
        self.shown
            .is_none_or(|(on, on_shape, _)| on != phases || on_shape != shape)
    }

    /// A write showing `phases` to a screen shaped `shape` is under way.
    pub(crate) fn stage(&mut self, phases: Flashes, shape: S) {
        self.staged = Some((phases, shape));
    }

    /// The staged write finished reaching the screen just now.
    pub(crate) fn landed(&mut self) {
        if let Some((phases, shape)) = self.staged.take() {
            self.shown(phases, shape);
        }
    }

    /// The screen was cleared: no phase shows on it to hold.
    pub(crate) fn wiped(&mut self) {
        self.shown = None;
        self.staged = None;
    }

    /// A write showing `phases` finished reaching a screen shaped `shape` just
    /// now.
    fn shown(&mut self, phases: Flashes, shape: S) {
        match &mut self.shown {
            Some((on, on_shape, _)) if *on == phases => *on_shape = shape,
            _ => self.shown = Some((phases, shape, (self.clock)())),
        }
    }
}

/// What a [`Screen`] does with a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    /// Kept back by the hold: the screen keeps the last.
    Held,
    /// Written, flashing as the screen already does.
    Same,
    /// Written, flashing otherwise: at once, whatever the painter's cadence.
    Changed,
}

impl Admit {
    /// Whether the picture is written.
    pub fn written(self) -> bool {
        self != Self::Held
    }
}

/// A painter's screen of shape `S`: the hold its writes go through, and what
/// the picture on it answers a pointer with, through the painter's geometry
/// `G` (its cells, its window pixels). A held write leaves the screen showing
/// the last, and a pointer hitting it.
#[derive(Debug)]
pub struct Screen<S, G> {
    hold: FlashHold<S>,
    /// The write under way, shown once it [`landed`](Self::landed).
    staged: Option<Shown<G>>,
    shown: Option<Shown<G>>,
}

/// The picture a [`Screen`] shows, as its painter's geometry maps onto it.
#[derive(Debug)]
pub struct Shown<G> {
    geometry: G,
    /// `None` for a slide, which shows no one floor's figures where they stand.
    hits: Option<crate::look::Hits>,
    /// The board's star where the painter set the text itself.
    host_star: Option<crate::layout::Bounds>,
}

impl<G> Shown<G> {
    /// How a pointer maps onto it.
    pub fn geometry(&self) -> &G {
        &self.geometry
    }

    /// What it shows a pointer over `area`, in layout units: nothing on a
    /// slide.
    pub fn hit_at(&self, area: crate::layout::Bounds) -> Option<crate::hit::SceneHit<'_>> {
        self.hits.as_ref()?.at(area, self.host_star)
    }

    /// What it answers a pointer with; `None` for a slide.
    pub fn hits(&self) -> Option<&crate::look::Hits> {
        self.hits.as_ref()
    }
}

impl<S: Copy + PartialEq, G> Screen<S, G> {
    /// A blank screen whose hold runs on `clock`.
    pub fn on(clock: ScreenClock) -> Self {
        Self {
            hold: FlashHold::on(clock),
            staged: None,
            shown: None,
        }
    }

    /// Whether `picture` is written to the screen, shaped `shape`, now. A
    /// written one is staged, mapped by `geometry`, its board's star at
    /// `host_star` where the painter set the text itself.
    pub fn admit(
        &mut self,
        picture: &crate::look::Picture<'_>,
        shape: S,
        geometry: G,
        host_star: Option<crate::layout::Bounds>,
    ) -> Admit {
        let flashes = picture.flashes();
        if self.hold.holds(flashes, shape) {
            return Admit::Held;
        }
        let admit = if self.hold.changes(flashes, shape) {
            Admit::Changed
        } else {
            Admit::Same
        };
        self.hold.stage(flashes, shape);
        self.staged = Some(Shown {
            geometry,
            hits: picture.hits().cloned(),
            host_star,
        });
        admit
    }

    /// The staged write finished reaching the screen just now.
    pub fn landed(&mut self) {
        self.hold.landed();
        if let Some(staged) = self.staged.take() {
            self.shown = Some(staged);
        }
    }

    /// A write showing no office to a screen shaped `shape` is under way:
    /// a pointer hits nothing now, and once it lands nothing flashes.
    pub fn blank(&mut self, shape: S) {
        self.hold.stage(Flashes::default(), shape);
        self.staged = None;
        self.shown = None;
    }

    /// The screen was cleared: it shows nothing, so the next write holds
    /// back for no flash and the pointer hits nothing until it lands.
    pub fn wiped(&mut self) {
        self.hold.wiped();
        self.staged = None;
        self.shown = None;
    }

    /// What the screen shows; `None` before its first write lands, and while
    /// blank.
    pub fn shown(&self) -> Option<&Shown<G>> {
        self.shown.as_ref()
    }
}

/// A [`ScreenClock`] a test moves by hand.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<Mutex<Duration>>);

impl ManualClock {
    /// The clock to hand a [`Screen`].
    pub fn clock(&self) -> ScreenClock {
        let manual = self.clone();
        Arc::new(move || manual.now())
    }

    /// Where it reads.
    pub fn now(&self) -> Duration {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Set to the frame instant `at`, so a test's frames and writes share one
    /// timeline.
    pub fn at(&self, at: SystemTime) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
    }

    /// Move on by `by`: a write that takes that long.
    pub fn advance(&self, by: Duration) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += by;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: Duration = Duration::from_millis(PHOTOSENSITIVE_PHASE_MIN_MS);
    const SHAPE: (u16, u16) = (80, 24);

    fn at(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000) + Duration::from_millis(ms)
    }

    /// Two pictures that flash apart.
    fn two_pictures() -> [Flashes; 2] {
        [1, 2].map(|strike| {
            Flashes::of_floor(FlashPhase {
                strike,
                ..FlashPhase::default()
            })
        })
    }

    /// A change waits out the floor on the hold's own clock from the instant
    /// the phases on screen showed, on both sides of it; the same phases never
    /// wait.
    #[test]
    fn a_hold_keeps_a_phase_the_floor_on_its_own_clock() {
        let [one, two] = two_pictures();
        let screen = ManualClock::default();
        let mut hold = FlashHold::on(screen.clock());
        screen.at(at(0));
        assert!(!hold.holds(one, SHAPE), "an empty screen holds nothing");
        assert!(hold.changes(one, SHAPE));
        hold.shown(one, SHAPE);
        screen.at(at(50));
        hold.shown(one, SHAPE);
        let floor = FLOOR.as_millis() as u64;
        screen.at(at(floor - 1));
        assert!(hold.holds(two, SHAPE), "timed from the first showing");
        assert!(!hold.holds(one, SHAPE), "the phases on screen never wait");
        assert!(!hold.changes(one, SHAPE) && hold.changes(two, SHAPE));
        screen.at(at(floor));
        assert!(!hold.holds(two, SHAPE));
        screen.at(at(0) - Duration::from_millis(1));
        assert!(!hold.holds(two, SHAPE), "a clock run backward");
    }

    /// A staged write counts from when it lands.
    #[test]
    fn a_write_counts_from_when_it_lands() {
        let [one, two] = two_pictures();
        let screen = ManualClock::default();
        let mut hold = FlashHold::on(screen.clock());
        screen.at(at(0));
        hold.stage(one, SHAPE);
        screen.at(at(50));
        hold.landed();
        let floor = FLOOR.as_millis() as u64;
        screen.at(at(floor));
        assert!(hold.holds(two, SHAPE), "timed from the landing");
        screen.at(at(50 + floor));
        assert!(!hold.holds(two, SHAPE));
    }

    /// A screen of a new shape (a resize, a font zoom) shows nothing to hold:
    /// its write goes out at once. Phases shown on it are timed from their
    /// first showing, on whatever shape.
    #[test]
    fn a_reshaped_screen_is_never_held() {
        let [one, two] = two_pictures();
        let screen = ManualClock::default();
        let mut hold = FlashHold::on(screen.clock());
        screen.at(at(0));
        hold.shown(one, SHAPE);
        screen.at(at(10));
        assert!(!hold.holds(two, (SHAPE.0 + 1, SHAPE.1)), "resized");
        assert!(
            hold.changes(one, (SHAPE.0 + 1, SHAPE.1)),
            "a cleared screen"
        );
        hold.shown(one, (SHAPE.0 + 1, SHAPE.1));
        assert!(
            hold.holds(two, (SHAPE.0 + 1, SHAPE.1)),
            "the new shape holds once shown"
        );
        screen.at(at(FLOOR.as_millis() as u64));
        assert!(
            !hold.holds(two, (SHAPE.0 + 1, SHAPE.1)),
            "timed from the phase's first showing, not the reshape"
        );
    }

    /// A cleared screen shows no phase to hold: its next write goes out at
    /// once, on the same shape inside the floor.
    #[test]
    fn a_wiped_screen_is_never_held() {
        let [one, two] = two_pictures();
        let screen = ManualClock::default();
        let mut hold = FlashHold::on(screen.clock());
        screen.at(at(0));
        hold.shown(one, SHAPE);
        hold.stage(one, SHAPE);
        screen.at(at(10));
        assert!(hold.holds(two, SHAPE), "premise: held");
        hold.wiped();
        assert!(!hold.holds(two, SHAPE));
        assert!(
            hold.changes(two, SHAPE) && hold.changes(one, SHAPE),
            "every phase is news"
        );
        hold.landed();
        assert!(!hold.holds(two, SHAPE), "the staged write was wiped too");
    }

    /// The phase is the composed frame's own: its sky's strike level, each a
    /// phase of its own and dark at none, its neon's catch, and each burst's
    /// phase.
    #[test]
    fn a_flash_phase_is_the_frames_strike_level_catch_and_bursts() {
        let now = at(0);
        let sky =
            |strike| crate::sky::Sky::at_with(now, crate::sky::Weather::Storm).with_strike(strike);
        let layout = crate::display::compose::tests::lively_office();
        let mut frame = crate::display::compose::tests::empty_frame(&layout);
        assert_eq!(FlashPhase::of(&sky(None), &frame), FlashPhase::default());
        let (dim, bright) = (
            FlashPhase::of(&sky(Some(crate::sky::StrikePhase::Dim)), &frame),
            FlashPhase::of(&sky(Some(crate::sky::StrikePhase::After)), &frame),
        );
        assert!(dim != bright && dim != FlashPhase::default());
        let burst = |phase| crate::sky::fireworks::Burst {
            x: 0.5,
            y: 0.5,
            phase,
            hue: crate::sky::fireworks::Hue::Gold,
        };
        let bursting = |phase| {
            let mut bursts = [None; crate::sky::fireworks::LANES];
            bursts[0] = Some(burst(phase));
            FlashPhase::of(&sky(None).with_bursts(bursts), &frame)
        };
        let (kindle, bloom) = (
            bursting(crate::sky::fireworks::BurstPhase::Kindle),
            bursting(crate::sky::fireworks::BurstPhase::Bloom),
        );
        assert!(kindle != bloom && kindle != FlashPhase::default());
        frame.neon_stutter = true;
        assert_ne!(FlashPhase::of(&sky(None), &frame), FlashPhase::default());
    }

    /// What a floor laid out on `layout` answers a pointer with.
    fn hits(layout: &Arc<crate::layout::SceneLayout>) -> crate::look::Hits {
        crate::look::Hits::new(Arc::default(), Arc::clone(layout), None, Default::default())
    }

    /// A floor's picture flashing `flashes`, answering a pointer with `hits`.
    fn floor_picture<'a>(
        pixels: &'a crate::sprite::RgbBuffer,
        hits: &'a crate::look::Hits,
        flashes: Flashes,
    ) -> crate::look::Picture<'a> {
        crate::look::Picture::Floor(crate::look::FloorPicture {
            floor: 0,
            pixels,
            dirty: &crate::look::Dirty::All,
            flashes,
            text: None,
            hits,
        })
    }

    /// Two layouts a test tells apart by identity.
    fn two_layouts() -> [Arc<crate::layout::SceneLayout>; 2] {
        [160, 161].map(|w| {
            Arc::new(
                crate::layout::SceneLayout::compute_with_seed(w, 96, None, 0).expect("lays out"),
            )
        })
    }

    /// The layout of the picture `screen` shows, if one does.
    fn shown_layout<S: Copy + PartialEq>(
        screen: &Screen<S, ()>,
    ) -> Option<Arc<crate::layout::SceneLayout>> {
        Some(Arc::clone(screen.shown()?.hits()?.layout()))
    }

    /// A pointer hits the picture on screen: not one held back, nor one still
    /// under way.
    #[test]
    fn a_pointer_hits_the_picture_on_screen() {
        let [one, two] = two_pictures();
        let [first, second] = two_layouts();
        let (first_hits, second_hits) = (hits(&first), hits(&second));
        let pixels = crate::sprite::RgbBuffer::filled(1, 1, crate::composite::BLACK);
        let screen_clock = ManualClock::default();
        let mut screen = Screen::on(screen_clock.clock());
        screen_clock.at(at(0));
        assert!(screen.shown().is_none(), "a blank screen shows nothing");
        let admit = screen.admit(&floor_picture(&pixels, &first_hits, one), SHAPE, (), None);
        assert_eq!(admit, Admit::Changed);
        assert!(
            screen.shown().is_none(),
            "a write under way shows nothing yet"
        );
        screen.landed();
        let is = |layout: &Arc<_>, screen: &Screen<_, ()>| {
            shown_layout(screen).is_some_and(|l| Arc::ptr_eq(&l, layout))
        };
        assert!(is(&first, &screen));
        let held = screen.admit(&floor_picture(&pixels, &second_hits, two), SHAPE, (), None);
        assert_eq!(held, Admit::Held);
        screen.landed();
        assert!(is(&first, &screen), "a held picture never shows");
        let same = screen.admit(&floor_picture(&pixels, &second_hits, one), SHAPE, (), None);
        assert_eq!(same, Admit::Same);
    }

    /// A wiped screen shows nothing for a pointer to hit, and its next write
    /// is news on the same shape inside the floor.
    #[test]
    fn a_wiped_screen_shows_nothing_and_takes_its_next_write_at_once() {
        let [one, two] = two_pictures();
        let [first, _] = two_layouts();
        let first_hits = hits(&first);
        let pixels = crate::sprite::RgbBuffer::filled(1, 1, crate::composite::BLACK);
        let screen_clock = ManualClock::default();
        let mut screen = Screen::on(screen_clock.clock());
        screen_clock.at(at(0));
        screen.admit(&floor_picture(&pixels, &first_hits, one), SHAPE, (), None);
        screen.landed();
        assert!(screen.shown().is_some(), "premise: shown");
        screen.wiped();
        assert!(screen.shown().is_none(), "the pointer hits nothing");
        screen_clock.at(at(10));
        let next = screen.admit(&floor_picture(&pixels, &first_hits, two), SHAPE, (), None);
        assert_eq!(next, Admit::Changed);
    }

    /// A slide on screen hits nothing, and a blank screen shows nothing.
    #[test]
    fn a_slide_and_a_blank_screen_hit_nothing() {
        let [one, _] = two_pictures();
        let [first, _] = two_layouts();
        let first_hits = hits(&first);
        let pixels = crate::sprite::RgbBuffer::filled(1, 1, crate::composite::BLACK);
        let screen_clock = ManualClock::default();
        let mut screen = Screen::on(screen_clock.clock());
        screen_clock.at(at(0));
        let _ = screen.admit(&floor_picture(&pixels, &first_hits, one), SHAPE, (), None);
        screen.landed();
        let slide = crate::look::Picture::Slide(crate::look::SlidePicture {
            pixels: &pixels,
            flashes: one,
        });
        assert!(screen.admit(&slide, SHAPE, (), None).written());
        screen.landed();
        let shown = screen.shown().expect("the slide shows");
        assert!(shown.hits().is_none());
        let whole = crate::layout::Bounds {
            x: 0,
            y: 0,
            width: 160,
            height: 96,
        };
        assert!(shown.hit_at(whole).is_none());
        screen.blank(SHAPE);
        assert!(screen.shown().is_none());
    }
}
