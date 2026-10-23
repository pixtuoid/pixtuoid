//! The sun, the moon and the stars behind the windows: where the disc stands,
//! and what a pane's glass shows at a point, as a few tones resolved once a
//! frame and picked per pixel by ordered dither.

use pixtuoid_core::sprite::Rgb;
use pixtuoid_core::sprite::format::Density;

use crate::composite::{blend, blend_rgb};
use crate::dither::FALLOFF_TONES;
use crate::layout::{WindowBay, glass_rows, window_run};
use crate::outside::WindowView;
use crate::sky::{BodyKind, Sky};
use crate::theme::Theme;

/// One frame's disc (sun by day, moon by night), arcing across the window
/// wall. `cx` is an ABSOLUTE buffer x, not a per-window offset: one disc across
/// the whole wall, shown by the one pane it stands over ([`Disc::hosted_by`]).
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Disc {
    pub(crate) cx: f32,
    pub(crate) cy: f32,
    pub(crate) r: f32,
    vis: f32,
    /// Illuminated fraction (0 new..1 full) — `1.0` for the sun; for the moon it
    /// drives the elliptical terminator.
    lit_frac: f32,
    /// The lit limb is on the right, as a northern-hemisphere sky shows a waxing
    /// moon; `false` puts it on the left ([`Sky::moon_waxing`]).
    lit_right: bool,
    body: BodyKind,
}

/// What of the disc lands at a point of glass.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DiscPart {
    Lit,
    /// The moon's dark limb, past the terminator.
    Dark,
    /// Its halo, `1` at the rim fading to `0` at [`GLOW_PX`] past it.
    Halo(f32),
}

const DISC_RADIUS_PX: f32 = 5.0;
const GLOW_PX: f32 = 3.0;
const GLOW_ALPHA: f32 = 0.55;
/// The moon's dark (un-illuminated) limb, near the night sky's own base colour
/// so the shadowed side recedes into the backdrop instead of reading as a
/// hard-edged bite out of the disc.
pub(crate) const MOON_SHADOW: Rgb = Rgb {
    r: 30,
    g: 34,
    b: 52,
};
// "Real low window": the horizon sits low in the band and the apex climbs off
// the glass entirely rather than tracking the full window height.
const HORIZON_FRAC: f32 = 0.55;
const ARC_RISE_FRAC: f32 = 0.80;
/// Below this visibility (cloud transmission, the moon's nightfall and the
/// horizon fade combined), the disc is not drawn at all.
pub(crate) const MIN_DISC_VIS: f32 = 0.08;
/// The altitude a rising body's disc takes to fade in, and a setting one's to
/// fade out.
const HORIZON_FADE: f32 = 0.1;

impl Disc {
    /// This frame's disc over a wall band `top_wall_h` tall, or `None` while its
    /// body is below the horizon or its visibility (cloud, nightfall, horizon
    /// fade) is under [`MIN_DISC_VIS`].
    pub(crate) fn of(sky: &Sky, buf_w: u16, top_wall_h: u16) -> Option<Self> {
        let e = sky.body();
        if e.altitude <= 0.0 {
            return None;
        }
        let night_gate = match e.kind {
            // A sun is only ever up by day, so the night has nothing to take.
            BodyKind::Sun => 1.0,
            BodyKind::Moon => sky.nightfall(),
        };
        let vis = sky.transmission().disc * night_gate * (e.altitude / HORIZON_FADE).min(1.0);
        if vis < MIN_DISC_VIS {
            return None;
        }
        // Sweep across [first pane, last pane] inset by the radius. NOT the pane
        // CENTERS (bit-identical to the mullion columns, so the span bisected the
        // disc at its most visible low-altitude moment and froze `cx` there on a
        // single-window buffer) and NOT a linear `buf_w - WINDOW_W` bound (that
        // only coincidentally lands inside a window).
        let run = window_run(buf_w);
        let span_left = f32::from(run.start) + DISC_RADIUS_PX;
        let span_right = (f32::from(run.end) - DISC_RADIUS_PX).max(span_left);
        let cx = span_left + e.azimuth * (span_right - span_left);
        let horizon_y = f32::from(top_wall_h) * HORIZON_FRAC;
        let cy = horizon_y - e.altitude * (f32::from(top_wall_h) * ARC_RISE_FRAC);
        let (lit_frac, lit_right) = match e.kind {
            BodyKind::Sun => (1.0, true),
            BodyKind::Moon => (sky.moon_phase(), sky.moon_waxing()),
        };
        Some(Self {
            cx,
            cy,
            r: DISC_RADIUS_PX,
            vis,
            lit_frac,
            lit_right,
            body: e.kind,
        })
    }

    /// Whether the pane over columns `x..x + w` shows the disc: only the pane
    /// its centre sits over. A disc near an inter-window gap is wide enough to
    /// reach both neighbours' glass, and would render twice across the pillar.
    pub(crate) fn hosted_by(&self, x: u16, w: u16) -> bool {
        self.cx >= f32::from(x) && self.cx < f32::from(x + w)
    }

    fn at(&self, x: f32, y: f32) -> Option<DiscPart> {
        let (dx, dy) = (x - self.cx, y - self.cy);
        let dist = (dx * dx + dy * dy).sqrt();
        if dist <= self.r {
            if self.lit_frac >= 1.0 {
                return Some(DiscPart::Lit);
            }
            let terminator_x =
                (1.0 - 2.0 * self.lit_frac) * (self.r * self.r - dy * dy).max(0.0).sqrt();
            let toward_lit_limb = if self.lit_right { dx } else { -dx };
            Some(if toward_lit_limb >= terminator_x {
                DiscPart::Lit
            } else {
                DiscPart::Dark
            })
        } else if dist <= self.r + GLOW_PX {
            Some(DiscPart::Halo(1.0 - (dist - self.r) / GLOW_PX))
        } else {
            None
        }
    }

    /// Its halo's strength at the rim. Scaling by `lit_frac` keeps a new moon's
    /// near-dark face from casting a full-bright halo.
    fn halo_peak(&self) -> f32 {
        self.vis * GLOW_ALPHA * self.lit_frac
    }
}

/// Roughly 1-in-`STAR_SPARSITY` sky pixels host a star — prime so the
/// hash-modulo grid can't line up into a visible lattice.
const STAR_SPARSITY: u64 = 47;
const STAR_COLOR: Rgb = Rgb {
    r: 255,
    g: 255,
    b: 255,
};
/// Cap on the star blend alpha — a faint glimmer, not a bright dot.
const STAR_ALPHA_MAX: f32 = 0.55;
/// Per-star twinkle cycle length range, in whole Full beats so a star turns on
/// one, hashed per position so the field doesn't blink in unison.
const STAR_TWINKLE_CYCLE_BASE_BEATS: u64 = 16;
const STAR_TWINKLE_CYCLE_SPAN_BEATS: u64 = 24;

/// Deterministic sparse star field, hashed on the ABSOLUTE buffer `(px, py)`
/// so it reads as one continuous sky rather than a per-window reseed.
fn star_exists(px: u16, py: u16) -> bool {
    let mut h = u64::from(px).wrapping_mul(crate::GOLDEN_GAMMA);
    h ^= u64::from(py).wrapping_mul(crate::MURMUR64A_M);
    h = (h ^ (h >> 17)).wrapping_mul(pixtuoid_core::id::SPLITMIX64_M2);
    h.is_multiple_of(STAR_SPARSITY)
}

/// The star at `(px, py)`'s own seed, which picks its cycle and its turns.
fn star_seed(px: u16, py: u16) -> u64 {
    u64::from(px).wrapping_mul(131) ^ u64::from(py).wrapping_mul(521)
}

/// How long a star seeded `seed` holds each turn.
fn star_twinkle_cycle_ms(seed: u64) -> u64 {
    (STAR_TWINKLE_CYCLE_BASE_BEATS + seed % STAR_TWINKLE_CYCLE_SPAN_BEATS)
        * crate::anim::FULL_TICK_MS
}

/// Per-star twinkle: a hashed per-star cycle length, rerolled on/off each cycle.
fn star_twinkle(px: u16, py: u16, beat: crate::anim::Beat) -> bool {
    let now_ms = beat.ms();
    let seed = star_seed(px, py);
    let cycle_ms = star_twinkle_cycle_ms(seed);
    let phase = now_ms / cycle_ms;
    let hash = seed.wrapping_add(phase).wrapping_mul(crate::GOLDEN_GAMMA);
    (hash % 10) < 7
}

/// The flat bands the window sky steps through from zenith to horizon.
const SKY_BANDS: usize = 4;

/// A colour for each band of the sky, zenith first.
type Bands = [Rgb; SKY_BANDS];

/// Below this [`SkyTones::golden_hour`](crate::atmosphere::SkyTones::golden_hour) the
/// blaze is too faint to paint.
const BLAZE_MIN: f32 = 0.05;
/// How far a full golden hour pulls open sky toward [`BLAZE`].
const BLAZE_SHARE: f32 = 0.35;
/// The golden hour's colour, and how far each channel leans toward it: red
/// most, so the cast reads orange over any sky tone.
const BLAZE: Rgb = Rgb {
    r: 255,
    g: 160,
    b: 60,
};
const BLAZE_LEAN: [f32; 3] = [0.4, 0.25, 0.1];

/// The golden hour's warm cast over open sky, at one strength a frame.
#[derive(Clone, Copy, PartialEq)]
struct Blaze(f32);

impl Blaze {
    fn of(golden_hour: f32) -> Option<Self> {
        (golden_hour > BLAZE_MIN).then_some(Self(golden_hour * BLAZE_SHARE))
    }

    /// `cur` under the blaze.
    fn over(self, cur: Rgb) -> Rgb {
        let [r, g, b] = BLAZE_LEAN.map(|lean| self.0 * lean);
        Rgb {
            r: blend(cur.r, BLAZE.r, r),
            g: blend(cur.g, BLAZE.g, g),
            b: blend(cur.b, BLAZE.b, b),
        }
    }
}

/// The window sky one frame shows: its disc and stars, and every tone they
/// paint in, resolved once so a pixel only picks among them.
#[derive(PartialEq)]
pub(crate) struct SkyView {
    disc: Option<Disc>,
    stars: bool,
    beat: crate::anim::Beat,
    sky: Bands,
    star: Bands,
    lit: Bands,
    dark: Bands,
    halo: [[Rgb; FALLOFF_TONES as usize]; SKY_BANDS],
    blaze: Option<Blaze>,
}

impl SkyView {
    /// `outlook`'s sky behind a wall band `top_wall_h` tall, in `theme`.
    pub(crate) fn of(
        outlook: &crate::atmosphere::Outlook,
        buf_w: u16,
        top_wall_h: u16,
        theme: &Theme,
    ) -> Self {
        let look = outlook.look();
        let sky: Bands = std::array::from_fn(|k| {
            look.glass_zenith
                .mix(look.glass_horizon, k as f32 / (SKY_BANDS - 1) as f32)
        });
        let disc = Disc::of(outlook.sky(), buf_w, top_wall_h);
        let core = match disc.map_or(BodyKind::Sun, |d| d.body) {
            BodyKind::Sun => theme.lighting.sun_core,
            BodyKind::Moon => theme.lighting.moon_core,
        };
        let (vis, peak) = disc.map_or((0.0, 0.0), |d| (d.vis, d.halo_peak()));
        let over = |c: Rgb, alpha: f32| sky.map(|s| blend_rgb(s, c, alpha));
        let tone = |k: usize| peak * (k + 1) as f32 / f32::from(FALLOFF_TONES);
        Self {
            disc,
            stars: look.star_strength > 0.0,
            beat: outlook.beat(),
            sky,
            star: over(STAR_COLOR, look.star_strength * STAR_ALPHA_MAX),
            lit: over(core, vis),
            dark: over(MOON_SHADOW, vis),
            halo: sky.map(|s| std::array::from_fn(|k| blend_rgb(s, core, tone(k)))),
            blaze: Blaze::of(look.golden_hour),
        }
    }

    /// `bay`'s window over `rows`, on a grid of `d` cells to the unit, its
    /// glass showing this sky under the golden hour's cast wherever `front`
    /// stands nothing before it. A cell `front` fills never resolves the sky.
    pub(crate) fn window(
        &self,
        bay: WindowBay,
        rows: std::ops::Range<u16>,
        d: Density,
        front: impl Fn(crate::outside::Cell) -> Option<Rgb>,
    ) -> WindowView {
        let pane = self.pane(
            bay.x,
            bay.w,
            glass_rows(rows.end.saturating_sub(rows.start)),
            d,
        );
        WindowView::new(bay, rows, d, |cell| {
            front(cell).unwrap_or_else(|| {
                let open = pane.colour(cell.at, cell.glass_offset.1);
                self.blaze.map_or(open, |b| b.over(open))
            })
        })
    }

    /// One pane's glass, over columns `x..x + w` and `glass_h` rows tall, on a
    /// grid of `d` cells to the unit.
    fn pane(&self, x: u16, w: u16, glass_h: u16, d: Density) -> PaneSky<'_> {
        PaneSky {
            view: self,
            hosts_disc: self.disc.is_some_and(|d| d.hosted_by(x, w)),
            glass_h,
            clear_rows: crate::skyline::clear_sky_rows(glass_h),
            d,
        }
    }
}

/// One pane's share of a [`SkyView`]: whether it shows the disc, and how far
/// down its glass the open sky runs, where a star may shine.
struct PaneSky<'a> {
    view: &'a SkyView,
    hosts_disc: bool,
    glass_h: u16,
    clear_rows: u16,
    d: Density,
}

impl PaneSky<'_> {
    /// The colour of grid cell `g`, `glass_dy` cells down this pane's glass.
    /// A cell is sampled at its centre
    /// ([`layout_point`](crate::display::pen::layout_point)). A star is the one
    /// cell at its unit's centre.
    fn colour(&self, g: (u16, u16), glass_dy: u16) -> Rgb {
        let v = self.view;
        let d = self.d;
        let unit = |c: u16| crate::display::pen::layout_point(crate::display::pen::ArtPx(c), d);
        let (p, glass_dy) = ((unit(g.0), unit(g.1)), unit(glass_dy));
        let share = crate::atmosphere::sky_share(glass_dy, self.glass_h);
        let band = crate::dither::nearest(share * (SKY_BANDS - 1) as f32, g.0, g.1);
        let i = usize::from(band);
        match v
            .disc
            .filter(|_| self.hosts_disc)
            .and_then(|d| d.at(p.0, p.1))
        {
            Some(DiscPart::Lit) => return v.lit[i],
            Some(DiscPart::Dark) => return v.dark[i],
            Some(DiscPart::Halo(f)) => {
                // Flat, not seamed: at a pixel a ring, a seam's specks would
                // be all the ring there is.
                let k = (f * f32::from(FALLOFF_TONES)).round() as u8;
                if k > 0 {
                    return v.halo[i][usize::from(k) - 1];
                }
            }
            None => {}
        }
        let d = d.get();
        let (sx, sy) = (g.0 / d, g.1 / d);
        if v.stars
            && (g.0 % d, g.1 % d) == (d / 2, d / 2)
            && glass_dy < f32::from(self.clear_rows)
            && star_exists(sx, sy)
            && star_twinkle(sx, sy, v.beat)
        {
            return v.star[i];
        }
        v.sky[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atmosphere::Moment;
    use crate::display::pen::test_density;

    /// `colour` indexes its bands by the dithered stop count, which the horizon
    /// (the deepest share) must not carry past the last band at any phase.
    #[test]
    fn the_horizon_dithers_no_deeper_than_the_last_band() {
        for y in 0..crate::dither::PERIOD {
            for x in 0..crate::dither::PERIOD {
                let band = crate::dither::nearest((SKY_BANDS - 1) as f32, x, y);
                assert!(usize::from(band) < SKY_BANDS, "({x}, {y}) -> {band}");
            }
        }
    }

    /// A star turns only on a Full beat, and the field's cycles span every
    /// beat count from the base to the base plus the span.
    #[test]
    fn a_star_twinkles_only_on_beats_across_its_cycle_range() {
        use crate::anim::{Beat, FULL_TICK_MS};
        let mut cycles = std::collections::BTreeSet::new();
        for (px, py) in (0..48u16).flat_map(|x| (0..8u16).map(move |y| (x, y))) {
            let cycle_ms = star_twinkle_cycle_ms(star_seed(px, py));
            cycles.insert(cycle_ms / FULL_TICK_MS);
            let mut was = star_twinkle(px, py, Beat::at_ms(0));
            for ms in 1..3 * cycle_ms {
                let on = star_twinkle(px, py, Beat::at_ms(ms));
                assert!(
                    on == was || ms % FULL_TICK_MS == 0,
                    "({px},{py}) turned at {ms} ms, off the beat"
                );
                was = on;
            }
        }
        let range = STAR_TWINKLE_CYCLE_BASE_BEATS
            ..STAR_TWINKLE_CYCLE_BASE_BEATS + STAR_TWINKLE_CYCLE_SPAN_BEATS;
        assert_eq!(cycles, range.collect(), "the cycles in beats");
    }

    /// Either body's disc fades in as it rises and out as it sets, never
    /// popping a whole step between two minutes, in any weather.
    #[test]
    fn a_discs_visibility_never_pops_minute_by_minute() {
        use crate::sky::Weather;
        const MAX_STEP: f32 = 0.25;
        for weather in [Weather::Clear, Weather::Windy] {
            for day in 0..30u32 {
                let start = crate::localclock::on_day(day, 0);
                let vis = |m: u64| {
                    let s = crate::sky::Sky::at_with(
                        start + std::time::Duration::from_secs(m * 60),
                        weather,
                    );
                    Disc::of(&s, 96, 40).map_or(0.0, |d| d.vis)
                };
                let mut prev = vis(0);
                for m in 1..24 * 60 {
                    let next = vis(m);
                    assert!(
                        (next - prev).abs() <= MAX_STEP,
                        "{weather:?} day {day} {:02}:{:02}: {prev} -> {next}",
                        m / 60,
                        m % 60
                    );
                    prev = next;
                }
            }
        }
    }

    /// A sun disc grows with its altitude through the morning and shrinks back
    /// through the evening, and is absent at the horizon itself.
    #[test]
    fn a_suns_disc_fades_monotonically_toward_the_horizon() {
        use crate::sky::Weather;
        let vis = |h, m: u64| {
            let t = crate::localclock::on_day(0, h) + std::time::Duration::from_secs(m * 60);
            let s = crate::sky::Sky::at_with(t, Weather::Clear);
            Disc::of(&s, 96, 40).map_or(0.0, |d| d.vis)
        };
        assert_eq!(vis(5, 0), 0.0);
        assert_eq!(vis(20, 0), 0.0);
        let rising = vis(5, 10);
        assert!(
            rising > 0.0 && rising < 1.0,
            "ten minutes after sunrise the disc is partly faded: {rising}"
        );
        let morning: Vec<f32> = (0..=180).map(|m| vis(5, m)).collect();
        assert!(morning.is_sorted(), "{morning:?}");
        let evening: Vec<f32> = (0..=180).map(|m| vis(17, m)).collect();
        assert!(evening.iter().rev().is_sorted(), "{evening:?}");
    }

    /// A moon below the horizon shows no disc, and one up fades in with the
    /// night rather than switching on at dusk.
    #[test]
    fn a_moon_shows_only_when_up_and_fades_in_after_dusk() {
        use crate::sky::Weather;
        let sky = |d, h| crate::sky::Sky::at_with(crate::localclock::on_day(d, h), Weather::Clear);
        let disc = |s: &crate::sky::Sky| Disc::of(s, 96, 40);
        let new_moon = (1..=31u32)
            .find(|&d| sky(d, 23).moon_phase() < 0.05)
            .expect("a new moon in January");
        assert!(
            disc(&sky(new_moon, 23)).is_none(),
            "a new moon is down at 23:00"
        );
        let full = (1..=31u32)
            .find(|&d| sky(d, 23).moon_phase() > 0.95)
            .expect("a full moon in January");
        let dusk = crate::sky::Sky::at_with(
            crate::localclock::on_day(full, 20) + std::time::Duration::from_secs(20 * 60),
            Weather::Clear,
        );
        let (dusk, late) = (
            disc(&dusk).expect("a full moon is up just after dusk"),
            disc(&sky(full, 23)).expect("and late in the night"),
        );
        assert!(
            dusk.vis < late.vis,
            "{} at dusk vs {} at 23:00",
            dusk.vis,
            late.vis
        );
    }
    use crate::sky::Weather;

    fn view(hour: u32) -> SkyView {
        view_in(hour, Weather::Clear)
    }

    fn view_in(hour: u32, weather: Weather) -> SkyView {
        let now = crate::localclock::at_hour(hour);
        let theme = crate::theme::theme_by_name("normal").expect("theme");
        let moment = Moment::resolve(
            Sky::at_with(now, weather),
            theme,
            0.0,
            crate::anim::Motion::Full.timing(now),
        );
        SkyView::of(&moment.outlook(theme), 160, 40, theme)
    }

    /// The window sky lights its stars on a clear night and none under
    /// overcast: the decision itself, since a render's clouds would hide a
    /// star wrongly lit behind them.
    #[test]
    fn the_window_sky_lights_stars_only_when_the_night_is_clear() {
        assert!(view_in(2, Weather::Clear).stars, "a clear night");
        assert!(!view_in(2, Weather::Overcast).stars, "an overcast night");
        assert!(!view_in(12, Weather::Clear).stars, "a clear noon");
    }

    /// A window's glass is its pane's sky under the blaze, read from the
    /// glass's top, at every density.
    #[test]
    fn a_window_shows_its_pane_s_sky_under_the_blaze() {
        let v = view(19);
        let blaze = v.blaze.expect("a clear 19h casts the golden hour");
        let bay = WindowBay {
            x: 30,
            w: crate::layout::WINDOW_W,
            idx: 1,
        };
        let rows = 1..33;
        for d in [1, 4] {
            let pane = v.pane(
                bay.x,
                bay.w,
                glass_rows(rows.end - rows.start),
                test_density(d),
            );
            let window = v.window(bay, rows.clone(), test_density(d), |_| None);
            let mut cells = 0;
            for (at, c) in window.cells() {
                let ay = at.1 - rows.start * d;
                assert_eq!(c, blaze.over(pane.colour(at, ay - d)), "{d}: {at:?}");
                cells += 1;
            }
            assert!(cells > 0, "{d}: the window has glass");
        }
    }

    #[test]
    fn the_window_sky_paints_only_its_resolved_tones() {
        for hour in [6, 12, 19, 22] {
            let v = view(hour);
            let palette: Vec<Rgb> = [v.sky, v.star, v.lit, v.dark]
                .into_iter()
                .flatten()
                .chain(v.halo.into_iter().flatten())
                .collect();
            let pane = v.pane(0, 160, 30, test_density(1));
            for y in 0..30u16 {
                for x in 0..160u16 {
                    let c = pane.colour((x, y), y);
                    assert!(palette.contains(&c), "{hour}h ({x},{y}): {c:?}");
                }
            }
        }
    }

    /// A star is one cell at its unit's centre at any density: the same stars
    /// as at one cell to the unit, never a block.
    #[test]
    fn a_star_is_one_cell_at_any_density() {
        let v = view(2);
        let glass_h = 30;
        let star = |c: Rgb| v.star.contains(&c);
        let one = v.pane(0, 0, glass_h, test_density(1));
        let mut stars = 0;
        for d in [2, 4] {
            let dense = v.pane(0, 0, glass_h, test_density(d));
            for y in 0..glass_h {
                for x in 0..160u16 {
                    let cells = (0..d)
                        .flat_map(|j| (0..d).map(move |i| (x * d + i, y * d + j)))
                        .filter(|&(ax, ay)| star(dense.colour((ax, ay), ay)))
                        .count();
                    let shines = star(one.colour((x, y), y));
                    stars += usize::from(shines);
                    assert_eq!(cells, usize::from(shines), "{d}: ({x}, {y})");
                }
            }
        }
        assert!(stars > 0, "a clear night shows stars");
    }

    #[test]
    fn the_sky_is_flat_at_its_ends() {
        let v = view(12);
        let glass_h = 30;
        let pane = v.pane(0, 0, glass_h, test_density(1));
        let tile = |glass_dy: u16| -> Vec<Rgb> {
            (0..crate::dither::PERIOD)
                .flat_map(|y| (0..crate::dither::PERIOD).map(move |x| (x, y)))
                .map(|(x, y)| pane.colour((x, y), glass_dy))
                .collect()
        };
        assert!(tile(0).iter().all(|&c| c == v.sky[0]), "zenith");
        assert!(
            tile(glass_h - 1).iter().all(|&c| c == v.sky[SKY_BANDS - 1]),
            "horizon"
        );
    }
}
