//! The weather on the windows' glass: the veil over the view and the rain or
//! snow running down the panes, drawn onto a [`WindowView`] on a grid of `d`
//! cells to the layout unit, so each painter draws one design at its own
//! density.

use crate::sprite::Rgb;
use crate::sprite::format::Density;

use crate::anim::Beat;
use crate::atmosphere::Moment;
use crate::dither::Dithered;
use crate::layout::Size;
use crate::outside::WindowView;
use crate::sky::{Element, Weather, WeatherMix, WeatherPolicy};

/// One frame's weather on every window: [`GlassWeather::of`] once per frame.
/// Its fields are its whole key: two equal keys place equal marks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GlassWeather {
    /// [`SkyTones::glass_veil`](crate::atmosphere::SkyTones::glass_veil).
    veil: Dithered<Option<(Rgb, f32)>>,
    falling: Falling,
    /// The clock the marks move by.
    beat: Beat,
}

/// What decides which weather's marks fall.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Falling {
    /// At rest, where loop time stands still: the sky's mix of this instant,
    /// the one beat its marks read.
    AtRest(WeatherMix),
    /// Off rest: the weather at each mark's own loop time, so a frame paints
    /// the same whatever this instant's mix.
    Moving(WeatherPolicy),
}

impl Falling {
    /// On `beat`, under `policy`, with `now` the mix of this instant.
    fn on(beat: Beat, policy: WeatherPolicy, now: impl FnOnce() -> WeatherMix) -> Self {
        if beat.is_rest() {
            Self::AtRest(now())
        } else {
            Self::Moving(policy)
        }
    }
}

/// One weather's falling particles.
struct Fall {
    count: u64,
    seed_mult: u64,
    sx_mult: u64,
    speed_base: u64,
    speed_span: u64,
    colour: Rgb,
    shape: Shape,
}

#[derive(Clone, Copy)]
enum Shape {
    /// A vertical streak `len_base + seed % len_mod` units long, its alpha
    /// falling from `alpha_base` by `alpha_falloff` over its length; `drift`
    /// leans it one cell east per two down (the wind).
    Streak {
        len_base: u16,
        len_mod: u64,
        alpha_base: f32,
        alpha_falloff: f32,
        drift: bool,
    },
    /// An opaque flake wiggling one unit east and back every [`WIGGLE_MS`].
    Flake,
}

const STREAK_COLOUR: Rgb = Rgb {
    r: 210,
    g: 220,
    b: 240,
};

const RAIN: Fall = Fall {
    count: 4,
    seed_mult: 7,
    sx_mult: crate::GOLDEN_GAMMA_32 as u64,
    speed_base: 60,
    speed_span: 50,
    colour: STREAK_COLOUR,
    shape: Shape::Streak {
        len_base: 3,
        len_mod: 2,
        alpha_base: 0.35,
        alpha_falloff: 0.15,
        drift: false,
    },
};

const STORM: Fall = Fall {
    count: 6,
    speed_base: 40,
    speed_span: 40,
    colour: Rgb {
        r: 210,
        g: 220,
        b: 245,
    },
    shape: Shape::Streak {
        len_base: 4,
        len_mod: 3,
        alpha_base: 0.6,
        alpha_falloff: 0.3,
        drift: false,
    },
    ..RAIN
};

const WINDY: Fall = Fall {
    count: 5,
    speed_base: 50,
    speed_span: 40,
    shape: Shape::Streak {
        len_base: 3,
        len_mod: 2,
        alpha_base: 0.35,
        alpha_falloff: 0.15,
        drift: true,
    },
    ..RAIN
};

const SNOW: Fall = Fall {
    count: 3,
    seed_mult: 11,
    sx_mult: 0x517c_c1b7,
    speed_base: 150,
    speed_span: 100,
    colour: Rgb {
        r: 240,
        g: 240,
        b: 250,
    },
    shape: Shape::Flake,
};

/// How long a flake holds each side of its wiggle.
const WIGGLE_MS: u64 = 400;

/// Spreads each particle's speed over its fall's span.
const SPEED_MULT: u64 = 0x4f6c_dd1d;

fn fall(w: Weather) -> Option<&'static Fall> {
    match w {
        Weather::Rain => Some(&RAIN),
        Weather::Storm => Some(&STORM),
        Weather::Windy => Some(&WINDY),
        Weather::Snow => Some(&SNOW),
        Weather::Clear | Weather::Fog | Weather::Overcast | Weather::Smog => None,
    }
}

/// One cell a particle covers on a painter's grid, from the glass's top-left.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mark {
    x: u16,
    y: u16,
    colour: Rgb,
    ink: Ink,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Ink {
    Solid,
    /// A streak's cell: `level` of its `peak` alpha, drawn in
    /// [`FALLOFF_TONES`](crate::dither::FALLOFF_TONES) flat tones.
    Fade {
        level: f32,
        peak: f32,
    },
}

impl Mark {
    /// The mark over `under`, at `at` on the painter's grid, which the falloff
    /// dithers by.
    fn over(self, under: Rgb, at: (u16, u16)) -> Rgb {
        match self.ink {
            Ink::Solid => self.colour,
            Ink::Fade { level, peak } => crate::composite::blend_rgb(
                under,
                self.colour,
                crate::dither::stepped(level, peak, crate::dither::FALLOFF_TONES, at.0, at.1),
            ),
        }
    }
}

/// One particle's way down the glass, at a tick.
struct Descent {
    seed: u64,
    /// Its column, in units.
    sx: u16,
    /// Its row, in units.
    sy: u16,
    /// Its current fall down the glass.
    this: Span,
    /// The fall after it.
    next: Span,
}

/// One fall down the glass: the ticks, in loop ms, it begins and lands.
#[derive(Clone, Copy)]
struct Span {
    began: u64,
    lands: u64,
}

impl Descent {
    /// The `i`th particle of `fall` on the glass of the window `idx`th in the
    /// run, `glass` units big, at `tick`.
    fn of(fall: &Fall, idx: u16, i: u64, glass: Size, tick: u64) -> Self {
        let seed = u64::from(idx) * fall.seed_mult + i;
        let gh = u64::from(glass.h);
        let speed = fall.speed_base + seed.wrapping_mul(SPEED_MULT) % fall.speed_span;
        let offset = seed.wrapping_mul(u64::from(crate::MURMUR3_FMIX32_M1)) % gh;
        let fallen = tick / speed + offset;
        let top = fallen - fallen % gh;
        // The tick by which `units` had fallen.
        let by = |units: u64| units.saturating_sub(offset) * speed;
        Self {
            seed,
            sx: (seed.wrapping_mul(fall.sx_mult) % u64::from(glass.w)) as u16,
            // Below `gh`, a u16.
            sy: (fallen % gh) as u16,
            this: Span {
                began: by(top),
                lands: by(top + gh),
            },
            next: Span {
                began: by(top + gh),
                lands: by(top + 2 * gh),
            },
        }
    }
}

/// A particle on the glass at a tick, and whether its fall and the next show.
struct Particle {
    fall: &'static Fall,
    descent: Descent,
    shows: bool,
    next_shows: bool,
}

impl Particle {
    /// Its cells at `tick` on `glass`, on a grid `d` cells to the unit.
    fn draw(&self, glass: Size, d: u16, tick: u64, marks: &mut Vec<Mark>) {
        let Descent { seed, sx, sy, .. } = self.descent;
        let (cols, rows) = (glass.w * d, glass.h * d);
        let colour = self.fall.colour;
        match self.fall.shape {
            Shape::Streak {
                len_base,
                len_mod,
                alpha_base,
                alpha_falloff,
                drift,
            } => {
                let len = (len_base + (seed % len_mod) as u16) * d;
                marks.extend((0..len).filter_map(|k| {
                    let y = sy * d + k;
                    // Past the glass's foot, the cell is the next fall's,
                    // coming in at the top.
                    let shows = if y < rows {
                        self.shows
                    } else {
                        self.next_shows
                    };
                    shows.then(|| Mark {
                        x: (sx * d + d / 2 + if drift { k / 2 } else { 0 }) % cols,
                        y: y % rows,
                        colour,
                        ink: Ink::Fade {
                            level: alpha_base - (f32::from(k) / f32::from(len)) * alpha_falloff,
                            peak: alpha_base,
                        },
                    })
                }));
            }
            Shape::Flake => {
                if !self.shows {
                    return;
                }
                let wiggle = u16::from(!(tick / WIGGLE_MS + seed).is_multiple_of(2));
                let side = (d / 2).max(1);
                let x0 = (sx + wiggle) % glass.w * d + (d - side) / 2;
                let y0 = sy * d + (d - side) / 2;
                marks.extend((0..side * side).map(|k| Mark {
                    x: x0 + k % side,
                    y: y0 + k / side,
                    colour,
                    ink: Ink::Solid,
                }));
            }
        }
    }
}

/// The precipitation share at which the `i`th of `count` particles falls: the
/// middle of its even slice of the share, so a share shows its own count.
fn threshold(i: u64, count: u64) -> f32 {
    (i as f32 + 0.5) / count as f32
}

impl GlassWeather {
    pub(crate) fn of(moment: &Moment) -> Self {
        Self {
            veil: moment.look.glass_veil,
            falling: Falling::on(moment.timing.beat, moment.sky.policy(), || {
                moment.sky.weather()
            }),
            beat: moment.timing.beat,
        }
    }

    /// The weather at loop time `loop_ms`; at rest, where loop time stands
    /// still, the sky's.
    fn weather_on_beat(&self, loop_ms: u64) -> WeatherMix {
        match self.falling {
            Falling::AtRest(mix) => mix,
            Falling::Moving(policy) => policy.weather_at_ms(self.beat.wall_ms(loop_ms)),
        }
    }

    /// Whether the `i`th of `w`'s `count` shows for the whole of its fall
    /// `span`, so it never appears or vanishes partway down: by the weather
    /// the fall began in, or by the one it lands in once that has settled, so
    /// a slot opens with only its own weather on the glass.
    fn shows(&self, w: Weather, i: u64, count: u64, span: Span) -> bool {
        let lands = self.weather_on_beat(span.lands);
        let decides = if lands.is_pure() {
            lands
        } else {
            self.weather_on_beat(span.began)
        };
        decides.share(Element::Precipitation, w) >= threshold(i, count)
    }

    /// Every falling weather's particles on the glass of the window `idx`th
    /// in the run, `glass` units big, each named by its weather and index.
    fn particles(&self, idx: u16, glass: Size) -> impl Iterator<Item = ((Weather, u64), Particle)> {
        let tick = self.beat.ms();
        Weather::ALL
            .into_iter()
            .filter_map(|w| fall(w).map(|f| (w, f)))
            .flat_map(move |(w, fall)| {
                (0..fall.count).map(move |i| {
                    let descent = Descent::of(fall, idx, i, glass, tick);
                    let particle = Particle {
                        fall,
                        shows: self.shows(w, i, fall.count, descent.this),
                        next_shows: self.shows(w, i, fall.count, descent.next),
                        descent,
                    };
                    ((w, i), particle)
                })
            })
    }

    /// This weather on `view`'s glass: the veil, then the marks over it, so
    /// rain still reads through the murk.
    pub(crate) fn paint(&self, view: &mut WindowView) {
        // A clear pane takes no veil, so it skips the pass.
        if self.veil.as_solid() != Some(None) {
            view.paint(|cell, c| {
                self.veil
                    .at(cell.at.0, cell.at.1)
                    .map_or(c, |(v, a)| crate::composite::blend_rgb(c, v, a))
            });
        }
        for m in self.marks(view.idx(), view.glass_size(), view.d()) {
            view.paint_glass_at((m.x, m.y), |cell, under| m.over(under, cell.at));
        }
    }

    /// The marks on the glass of the window `idx`th in the run, `glass` units
    /// big, on a grid `d` cells to the unit. A streak is one cell wide and
    /// `d` cells per unit long; a flake is a square half a unit across.
    fn marks(&self, idx: u16, glass: Size, d: Density) -> Vec<Mark> {
        let mut marks = Vec::new();
        if glass.w == 0 || glass.h == 0 {
            return marks;
        }
        for (_, particle) in self.particles(idx, glass) {
            particle.draw(glass, d.get(), self.beat.ms(), &mut marks);
        }
        marks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Motion;
    use crate::display::pen::test_density;
    use crate::localclock::at_hour;
    use crate::sky::Sky;
    use crate::sky::{TRANSITION_MS, WEATHER_CYCLE_MS};

    /// The windows in a run the tests sweep.
    const WINDOWS: u16 = 8;

    /// The glass `tick_ms` past noon on `motion`'s beat, under `w`.
    fn glass_weather(motion: Motion, w: Weather, tick_ms: u64) -> GlassWeather {
        let now = at_hour(12) + std::time::Duration::from_millis(tick_ms);
        GlassWeather::of(&Moment::resolve(
            Sky::at_with(now, w),
            &crate::theme::NORMAL,
            0.0,
            motion.timing(now),
        ))
    }

    /// The glass `ms` after the epoch on `motion`'s beat under `policy`.
    fn glass_at(motion: Motion, policy: WeatherPolicy, ms: u64) -> GlassWeather {
        let timing = motion.timing(std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms));
        GlassWeather::of(&Moment::resolve(
            Sky::at(timing, policy),
            &crate::theme::NORMAL,
            0.0,
            timing,
        ))
    }

    /// The clock's glass on `beat`, `wall_ms` after the epoch, without the sky
    /// a frame resolves: the sweeps below take hundreds of thousands.
    fn clock_glass(beat: Beat, wall_ms: u64) -> GlassWeather {
        GlassWeather {
            veil: Dithered::solid(None),
            falling: Falling::on(beat, WeatherPolicy::Clock, || {
                WeatherPolicy::Clock.weather_at_ms(wall_ms)
            }),
            beat,
        }
    }

    #[test]
    fn every_mark_lies_on_the_glass_at_every_density() {
        let glass = Size { w: 20, h: 13 };
        for w in Weather::ALL {
            for tick in [0, 37, 1234, 99_999] {
                for d in [1, 2, 3, 4] {
                    for m in glass_weather(Motion::Full, w, tick).marks(3, glass, test_density(d)) {
                        assert!(
                            m.x < glass.w * d && m.y < glass.h * d,
                            "{w:?} {tick} {d}: {m:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn only_falling_weather_marks_the_glass() {
        let glass = Size { w: 20, h: 13 };
        for motion in [Motion::Full, Motion::Calm, Motion::Still] {
            for w in Weather::ALL {
                assert_eq!(
                    !glass_weather(motion, w, 0)
                        .marks(0, glass, test_density(4))
                        .is_empty(),
                    w.falls(),
                    "{motion:?} {w:?}"
                );
            }
        }
    }

    /// Still freezes the marks, never the weather: the glass still shows the
    /// sky's rain or snow now, held in place, as dense as its share.
    #[test]
    fn at_rest_the_weather_shows_frozen() {
        const STEPS: u16 = 20;
        let glass = Size { w: 20, h: 13 };
        let now = at_hour(12);
        for w in [Weather::Rain, Weather::Snow] {
            let still =
                |tick| glass_weather(Motion::Still, w, tick).marks(0, glass, test_density(4));
            let full = still(0);
            assert!(!full.is_empty(), "{w:?} vanished at rest");
            assert_eq!(still(37), still(99_999), "{w:?} moved at rest");
            let coming_in = |progress| {
                let mix = WeatherMix::toward(Weather::Clear, w, progress);
                let sky = Sky::at_with(now, w).with_weather(mix);
                GlassWeather::of(&Moment::resolve(
                    sky,
                    &crate::theme::NORMAL,
                    0.0,
                    Motion::Still.timing(now),
                ))
                .marks(0, glass, test_density(4))
            };
            let shares: Vec<Vec<Mark>> = (0..=STEPS)
                .map(|k| coming_in(f32::from(k) / f32::from(STEPS)))
                .collect();
            assert!(shares[0].is_empty(), "{w:?} showed with no share");
            assert_eq!(shares[usize::from(STEPS)], full, "{w:?}");
            for pair in shares.windows(2) {
                assert!(pair[1].starts_with(&pair[0]), "{w:?} thinned as it came in");
            }
            assert!(
                shares.iter().any(|m| !m.is_empty() && m.len() < full.len()),
                "{w:?} never showed a part share"
            );
        }
    }

    /// The tick moves the marks, and is all that does within one weather.
    #[test]
    fn equal_keys_place_equal_marks_and_the_tick_moves_them() {
        let glass = Size { w: 20, h: 13 };
        for w in [Weather::Rain, Weather::Snow] {
            let marks =
                |tick| glass_weather(Motion::Full, w, tick).marks(1, glass, test_density(4));
            assert_eq!(marks(5_000), marks(5_000), "{w:?}");
            assert_ne!(marks(5_000), marks(6_000), "{w:?}");
        }
    }

    /// A streak is one cell wide at every density, so it stays a line, never
    /// a bar a unit wide.
    #[test]
    fn a_streak_is_one_cell_wide_and_a_unit_per_unit_long() {
        let glass = Size { w: 20, h: 13 };
        let rain = glass_weather(Motion::Full, Weather::Rain, 0);
        let (marks, one) = (
            rain.marks(0, glass, test_density(4)),
            rain.marks(0, glass, test_density(1)),
        );
        assert_eq!(marks.len(), one.len() * 4);
        let columns: std::collections::BTreeSet<u16> = marks.iter().map(|m| m.x).collect();
        assert!(columns.len() <= usize::try_from(RAIN.count).expect("small"));
    }

    /// The weathers that fall are the sky's: [`Weather::falls`] and [`fall`]
    /// name one set.
    #[test]
    fn the_sky_and_the_glass_agree_on_what_falls() {
        for w in Weather::ALL {
            assert_eq!(fall(w).is_some(), w.falls(), "{w:?}");
        }
    }

    /// Each particle on window `idx`'s `glass`, the tick its fall began, and
    /// whether it shows, under the clock at `tick` wall ms on `motion`'s beat.
    fn falls_at(
        motion: Motion,
        tick: u64,
        idx: u16,
        glass: Size,
    ) -> Vec<((Weather, u64), u64, bool)> {
        let beat = motion.beat(std::time::UNIX_EPOCH + std::time::Duration::from_millis(tick));
        clock_glass(beat, tick)
            .particles(idx, glass)
            .map(|(id, p)| (id, p.descent.this.began, p.shows))
            .collect()
    }

    /// The start, in ms, of each clock transition that changes what falls,
    /// with its two weathers.
    fn falling_changes() -> impl Iterator<Item = (u64, Weather, Weather)> {
        (0..).filter_map(move |slot: u64| {
            let start = (slot + 1) * WEATHER_CYCLE_MS - TRANSITION_MS;
            let [here, next] = WeatherPolicy::Clock.weather_at_ms(start + 1).ends();
            (here != next && (here.falls() || next.falls())).then_some((start, here, next))
        })
    }

    /// A particle shows or hides for a whole fall on every moving tier: none
    /// appears or vanishes partway down, through changes of every kind and the
    /// slot boundaries after them, on every window of a tall glass.
    #[test]
    fn no_particle_appears_or_vanishes_mid_fall() {
        const CHANGES: usize = 16;
        const STEP_MS: usize = 37;
        const MARGIN_MS: u64 = 15_000;
        let glass = Size { w: 20, h: 56 };
        let mut kinds = std::collections::HashSet::new();
        for (start, here, next) in falling_changes().take(CHANGES) {
            kinds.insert((here, next));
            let end = start + TRANSITION_MS + MARGIN_MS;
            for (motion, idx) in [Motion::Full, Motion::Calm]
                .into_iter()
                .flat_map(|m| (0..WINDOWS).map(move |i| (m, i)))
            {
                let mut prev = falls_at(motion, start - MARGIN_MS, idx, glass);
                for tick in (start - MARGIN_MS..end).step_by(STEP_MS).skip(1) {
                    let now = falls_at(motion, tick, idx, glass);
                    for ((id, began, was), (_, began_now, is)) in prev.iter().zip(&now) {
                        assert!(
                            began != began_now || was == is,
                            "{motion:?} {here:?} -> {next:?}: {id:?} on window {idx} \
                             {} mid-fall at {tick}",
                            if *is { "appeared" } else { "vanished" }
                        );
                    }
                    prev = now;
                }
            }
        }
        assert!(kinds.len() > 4, "too few kinds of change: {kinds:?}");
    }

    /// A streak slides on and off the glass and never pops: between two
    /// ticks a unit apart it gains or loses at most a unit's cells, through
    /// changes of every kind.
    #[test]
    fn a_streak_slides_on_and_off_the_glass() {
        const CHANGES: usize = 8;
        const MARGIN_MS: u64 = 15_000;
        const D: u16 = 4;
        let glass = Size { w: 20, h: 28 };
        let fastest = Weather::ALL
            .into_iter()
            .filter_map(fall)
            .map(|f| f.speed_base)
            .min()
            .expect("something falls");
        for (start, here, next) in falling_changes().take(CHANGES) {
            for idx in 0..WINDOWS {
                let cells = |tick: u64| -> Vec<usize> {
                    clock_glass(Beat::at_ms(tick), tick)
                        .particles(idx, glass)
                        .filter(|(_, p)| matches!(p.fall.shape, Shape::Streak { .. }))
                        .map(|(_, p)| {
                            let mut marks = Vec::new();
                            p.draw(glass, D, tick, &mut marks);
                            marks.len()
                        })
                        .collect()
                };
                let range = start - MARGIN_MS..start + TRANSITION_MS + MARGIN_MS;
                let mut prev = cells(range.start);
                for tick in range
                    .step_by(usize::try_from(fastest).expect("small"))
                    .skip(1)
                {
                    let now = cells(tick);
                    for (k, (was, is)) in prev.iter().zip(&now).enumerate() {
                        assert!(
                            was.abs_diff(*is) <= usize::from(D),
                            "{here:?} -> {next:?}: streak {k} on window {idx} went \
                             from {was} cells to {is} at {tick}"
                        );
                    }
                    prev = now;
                }
            }
        }
    }

    /// The tallest glass whose slowest fall, on the slowest moving tier, lands
    /// before the slot after the one it began in starts to change.
    fn tallest_glass() -> u16 {
        let hold_ms = WEATHER_CYCLE_MS - TRANSITION_MS;
        let pace = crate::anim::CALM_TICK_MS / crate::anim::FULL_TICK_MS;
        let slowest = Weather::ALL
            .into_iter()
            .filter_map(fall)
            .map(|f| f.speed_base + f.speed_span - 1)
            .max()
            .expect("something falls");
        u16::try_from((hold_ms - 1) / (slowest * pace)).expect("a glass")
    }

    /// Every slot opens with only its own weather on the glass, on every
    /// moving tier and every glass up to the tallest: the clock's marks are its
    /// weather's forced ones.
    #[test]
    fn every_slot_opens_with_only_its_own_weather_on_the_glass() {
        const CHANGES: usize = 32;
        for (start, _, next) in falling_changes().take(CHANGES) {
            let opens = start + TRANSITION_MS;
            for motion in [Motion::Full, Motion::Calm] {
                for h in [28, 56, tallest_glass()] {
                    let glass = Size { w: 20, h };
                    for idx in 0..WINDOWS {
                        let marks = |policy| {
                            glass_at(motion, policy, opens).marks(idx, glass, test_density(1))
                        };
                        assert!(
                            marks(WeatherPolicy::Clock) == marks(WeatherPolicy::Forced(next)),
                            "{motion:?} {next:?} at {opens} on window {idx}, glass {h} tall"
                        );
                    }
                }
            }
        }
    }

    /// Across a rain-to-storm change the storm's particles take over from the
    /// rain's on every moving tier: all rain as it opens, all storm as the
    /// next slot does, both between.
    #[test]
    fn a_change_trades_one_falls_particles_for_the_others() {
        let glass = Size { w: 20, h: 28 };
        let (start, ..) = falling_changes()
            .find(|&(_, here, next)| here == Weather::Rain && next == Weather::Storm)
            .expect("the clock turns rain to storm");
        let every = |f: &Fall| usize::from(WINDOWS) * usize::try_from(f.count).expect("small");
        let (rains, storms) = (every(&RAIN), every(&STORM));
        for motion in [Motion::Full, Motion::Calm] {
            let counts = |tick| {
                let shown = |w| {
                    (0..WINDOWS)
                        .flat_map(|idx| falls_at(motion, tick, idx, glass))
                        .filter(|&((pw, _), _, shows)| pw == w && shows)
                        .count()
                };
                (shown(Weather::Rain), shown(Weather::Storm))
            };
            assert_eq!(counts(start), (rains, 0), "{motion:?}");
            assert_eq!(counts(start + TRANSITION_MS), (0, storms), "{motion:?}");
            let mid = counts(start + TRANSITION_MS / 3);
            assert!(mid.0 > 0 && mid.1 > 0, "{motion:?}: {mid:?}");
        }
    }

    #[test]
    fn a_streak_steps_down_through_the_falloff_tones() {
        let Shape::Streak { alpha_base, .. } = STORM.shape else {
            panic!("storm streaks");
        };
        let black = Rgb { r: 0, g: 0, b: 0 };
        let tones: Vec<Rgb> = (1..=crate::dither::FALLOFF_TONES)
            .map(|k| {
                let alpha = alpha_base * f32::from(k) / f32::from(crate::dither::FALLOFF_TONES);
                crate::composite::blend_rgb(black, STORM.colour, alpha)
            })
            .chain([black])
            .collect();
        let marks = glass_weather(Motion::Full, Weather::Storm, 12_345).marks(
            0,
            Size { w: 18, h: 28 },
            test_density(1),
        );
        assert!(!marks.is_empty());
        for m in marks {
            let c = m.over(black, (m.x, m.y));
            assert!(tones.contains(&c), "{c:?} vs {tones:?}");
        }
    }

    /// The veil keys its dither on the painter's grid, as the room's other
    /// dithers do, never on the glass's corner: keyed there, its pattern would
    /// shift with each window's place on the grid.
    #[test]
    fn the_veil_dithers_on_the_painters_grid() {
        use crate::layout::{WINDOW_W, WindowBay, window_rows};
        use crate::outside::WindowView;
        const BARE: Rgb = Rgb { r: 0, g: 0, b: 0 };
        const VEIL: Rgb = Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        // A share whose dither, unlike half's checkerboard, moves with any
        // shift of its key.
        let veil = Dithered::new(None, Some((VEIL, 1.0)), 0.4);
        let weather = GlassWeather {
            veil,
            ..glass_weather(Motion::Full, Weather::Clear, 0)
        };
        let rows = window_rows(32);
        let bay = WindowBay {
            x: 2,
            w: WINDOW_W,
            idx: 0,
        };
        let corner = bay.glass_box(rows.clone());
        assert!(
            ![corner.x, corner.y]
                .iter()
                .any(|c| c.is_multiple_of(crate::dither::PERIOD)),
            "the glass on the dither's phase at d=1: a key from its corner would pass"
        );
        for d in [1, 4] {
            let mut view = WindowView::new(bay, rows.clone(), test_density(d), |_| BARE);
            weather.paint(&mut view);
            let mut took = [0; 2];
            for (at, c) in view.cells() {
                let takes = veil.takes_to(at.0, at.1);
                assert_eq!(c, if takes { VEIL } else { BARE }, "d={d} at {at:?}");
                took[usize::from(takes)] += 1;
            }
            assert!(took[0] > 0 && took[1] > 0, "d={d}: {took:?}");
        }
    }
}
