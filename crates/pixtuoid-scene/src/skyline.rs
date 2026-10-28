//! The city seen through the office's windows: which buildings stand where
//! along the window run, far to near, which of their windows burn, each depth's
//! colours under the sky, and the city drawn onto an art grid of any density.
//! The classic painter lays that grid on its buffer at 1x, so one city is what
//! every window shows.
//!
//! Everything is laid out in logical units from the run's west end and the
//! glass's top, and every choice is a hash of where it is, so a wider window
//! shows more of the same city rather than another one. How tall the city
//! stands is a share of the glass, plane by plane, so a short window crops its
//! towers and a tall one shows them whole over more sky.

use std::ops::RangeInclusive;

use crate::pack::OfficeArt;
use pixtuoid_core::sprite::format::{Building, CityMaterials, CityPlane, Density, Material};
use pixtuoid_core::sprite::{Frame, Pixel, Rgb};

use crate::atmosphere::SkyTones;
use crate::layout::pct;
use crate::outside::Cell;
use crate::theme::Theme;

/// A depth of the city.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Plane {
    /// Plain blocks on the horizon: the painter's own, behind every building a
    /// pack draws.
    Far,
    /// The pack's middle distance.
    Mid,
    /// The pack's nearest buildings, standing tallest: a short window shows
    /// only their tops.
    Near,
}

impl Plane {
    /// Far to near: the order the planes are drawn in.
    pub(crate) const ALL: [Plane; 3] = [Plane::Far, Plane::Mid, Plane::Near];

    /// Its place in [`Plane::ALL`].
    pub(crate) fn index(self) -> usize {
        self as usize
    }

    fn depth(self) -> &'static Depth {
        match self {
            Plane::Far => &FAR,
            Plane::Mid => &MID,
            Plane::Near => &NEAR,
        }
    }

    /// The pack's plane of the same depth; the far plane is the painter's own.
    fn pack_plane(self) -> Option<CityPlane> {
        match self {
            Plane::Far => None,
            Plane::Mid => Some(CityPlane::Mid),
            Plane::Near => Some(CityPlane::Near),
        }
    }

    /// The rows its buildings may rise over the sill behind glass `glass_h`
    /// tall, seen from `altitude`: a taller building shows only its top. The
    /// higher the office, the lower the nearer planes stand, while the horizon
    /// holds.
    fn band(self, glass_h: u16, altitude: f32) -> u16 {
        let depth = self.depth();
        let kept = 1.0 - altitude.clamp(0.0, 1.0) * f32::from(depth.altitude_pct) / 100.0;
        (f32::from(pct(glass_h, depth.band_pct)) * kept) as u16
    }
}

/// How a depth looks: distance reads as value, the far plane lifted and hazed
/// toward the horizon, the near one darkest.
struct Depth {
    /// Tells this plane's hashes apart from the others'.
    salt: u32,
    /// How many ramp stops its materials sit over the hour's building tone.
    lift: i8,
    /// How far everything in it fades toward the horizon: aerial perspective.
    haze: f32,
    /// The percent of its windows lit at full dark.
    lit_night_pct: u32,
    /// The most of the glass, in percent, its buildings rise over the sill:
    /// the nearer, the taller, so depth never reads inverted.
    band_pct: u16,
    /// The percent of its band a view from the top floor loses: the nearer,
    /// the more it drops away below the office.
    altitude_pct: u16,
    /// The gap after each of its buildings, in logical units: the nearer, the
    /// wider, so the planes behind show between its towers.
    gap: RangeInclusive<u16>,
}

const FAR: Depth = Depth {
    salt: 3,
    lift: 2,
    haze: 0.5,
    lit_night_pct: 40,
    band_pct: 30,
    altitude_pct: 0,
    gap: 0..=1,
};
const MID: Depth = Depth {
    salt: 41,
    lift: 1,
    haze: 0.25,
    lit_night_pct: 50,
    band_pct: 50,
    altitude_pct: 20,
    gap: 0..=2,
};
const NEAR: Depth = Depth {
    salt: 97,
    lift: 0,
    haze: 0.0,
    lit_night_pct: 55,
    band_pct: 65,
    altitude_pct: 45,
    gap: 2..=7,
};

/// A plain block's width, in logical units.
const BLOCK_W: RangeInclusive<u16> = 2..=5;
/// A plain block's height, in percent of its plane's band.
const BLOCK_H_PCT: RangeInclusive<u16> = 40..=100;
/// One more than the most logical units west of the run each plane's first
/// building may start, so the planes' edges do not line up at the run's west
/// end.
const WEST_JITTER: u32 = 4;

/// The percent of windows lit at noon: some offices keep their lights on by day.
const LIT_DAY_PCT: u32 = 4;
/// How strongly a lit window glows at noon, where daylight washes it out.
const LIT_DAY_GLOW: f32 = 0.25;
/// How far a dark window's glass sits from the theme's dark window toward its
/// building's tone.
const DARK_GLASS_TONE: f32 = 0.5;
/// The resolution a window's place in the lit order is drawn at.
const PER_MILLE: u32 = 1000;
/// A lit window's turn, in milliseconds: on each, it may go dark for the whole
/// turn.
const BLINK_CYCLE_MS: RangeInclusive<u64> = 6_000..=14_000;
/// One lit window in this many goes dark on a given turn.
const BLINK_OFF_IN: u32 = 12;

/// An aviation obstruction light's red, the same under every theme.
const BEACON: Rgb = Rgb {
    r: 255,
    g: 36,
    b: 28,
};
/// One of the tallest towers in this many carries a light.
const BEACON_IN: u32 = 2;
/// How many Full beats a light holds on, then off, so it flips on one.
const BEACON_HALF_BEATS: u64 = 10;
/// [`BEACON_HALF_BEATS`] in ms.
const BEACON_HALF_MS: u64 = BEACON_HALF_BEATS * crate::anim::FULL_TICK_MS;
/// Tells the lights' hashes apart from the planes'.
const BEACON_SALT: u32 = 0xB1EC;
/// A separate salt for the phase: carriers all share one [`BEACON_SALT`]
/// hash residue, so phasing off it would blink every light in unison.
const BEACON_PHASE_SALT: u32 = 0x9A5E;

/// Whether `stand` carries a light: a near building the pack draws as tall
/// as its `tallest`, a few of them. The band crops every near tower to one
/// line, so only the art's own height tells the tallest apart.
fn carries_beacon(plane: Plane, stand: &Stand<'_>, tallest: u16) -> bool {
    plane == Plane::Near
        && matches!(stand, Stand::Kit { building, .. } if building.size().1 >= tallest)
        && hash(stand.hash() ^ BEACON_SALT).is_multiple_of(BEACON_IN)
}

/// The tallest building `pack` stands in the near plane.
fn tallest_near(pack: &OfficeArt) -> u16 {
    pack.buildings()
        .filter(|b| b.stands_in(CityPlane::Near))
        .map(|b| b.size().1)
        .max()
        .unwrap_or(u16::MAX)
}

/// Whether the light on a stand hashed `hash` shines on `beat`: on and off a
/// [`BEACON_HALF_MS`] each, out of step with its neighbours'.
fn beacon_on(hash: u32, beat: crate::anim::Beat) -> bool {
    (beat.ms() / BEACON_HALF_MS + u64::from(self::hash(hash ^ BEACON_PHASE_SALT))).is_multiple_of(2)
}

/// The art pixels of a light `side` across on the tip of the building
/// `frame` draws from `origin`, `grow` art pixels to its pixel: its topmost
/// pixel nearest its middle, a spire's tip. Only the building's own pixels:
/// on a mast thinner than the light it runs down the mast, never out over
/// the sky or another building.
fn beacon_cells(frame: &Frame, origin: (i32, i32), grow: u16, side: u16) -> Vec<(i32, i32)> {
    let drawn = |fx: u16, fy: u16| frame.get(fx, fy).copied().flatten().is_some();
    let Some((fx, fy)) = (0..frame.height()).find_map(|fy| {
        (0..frame.width())
            .filter(|&fx| drawn(fx, fy))
            .min_by_key(|&fx| fx.abs_diff(frame.width() / 2))
            .map(|fx| (fx, fy))
    }) else {
        return Vec::new();
    };
    let (ax, ay) = (fx * grow, fy * grow);
    (0..side)
        .flat_map(|dy| (0..side).map(move |dx| (ax + dx, ay + dy)))
        .filter(|&(x, y)| drawn(x / grow, y / grow))
        .map(|(x, y)| (origin.0 + i32::from(x), origin.1 + i32::from(y)))
        .collect()
}

/// The rows at the top of glass `glass_h` tall that no building reaches: the
/// sky the sun, the moon and the stars always have.
pub(crate) fn clear_sky_rows(glass_h: u16) -> u16 {
    glass_h
        - Plane::ALL
            .into_iter()
            .map(|p| p.band(glass_h, 0.0))
            .fold(0, u16::max)
}

/// One building standing in a plane, its top-left in logical units from the
/// run's west end and the glass's top.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Stand<'p> {
    /// A building the pack draws.
    Kit {
        building: &'p Building,
        x: i32,
        top: i32,
        hash: u32,
    },
    /// A plain block the painter draws, its windows where [`block_window`]
    /// puts them.
    Block { x: i32, w: u16, top: i32, hash: u32 },
}

impl Stand<'_> {
    /// The hash every choice about it keys off.
    pub(crate) fn hash(&self) -> u32 {
        match *self {
            Stand::Kit { hash, .. } | Stand::Block { hash, .. } => hash,
        }
    }
}

/// A window of a [`Stand::Block`] at `(x, y)` art pixels from its top-left:
/// every other row and column, the lit-dot grid a city's windows are read by.
pub(crate) fn block_window(x: u16, y: u16) -> bool {
    x % 2 == 1 && y % 2 == 1
}

/// The city behind one window run.
pub(crate) struct Skyline<'p> {
    stands: Vec<(Plane, Stand<'p>)>,
}

impl<'p> Skyline<'p> {
    /// `pack`'s city across a run of glass `run_w` wide and `glass_h` tall, seen
    /// from `altitude` (0 at the ground floor, 1 at the top). A plane the pack
    /// stands no building in gets plain blocks, so every depth reads.
    pub(crate) fn of(pack: &'p OfficeArt, run_w: u16, glass_h: u16, altitude: f32) -> Self {
        let kits = Plane::ALL.map(|plane| -> Vec<&'p Building> {
            plane.pack_plane().map_or_else(Vec::new, |p| {
                pack.buildings().filter(|b| b.stands_in(p)).collect()
            })
        });
        // Near to far, each plane's band is held to the tallest the plane in
        // front of it stands, so no plane rises above the one in front.
        let mut bands = [0; Plane::ALL.len()];
        let mut in_front = u16::MAX;
        for plane in Plane::ALL.into_iter().rev() {
            let band = plane.band(glass_h, altitude).min(in_front);
            bands[plane.index()] = band;
            in_front = kits[plane.index()]
                .iter()
                .map(|b| b.size().1)
                .max()
                .map_or(band, |tallest| tallest.min(band));
        }
        let mut stands = Vec::new();
        for plane in Plane::ALL {
            let band = bands[plane.index()];
            if band == 0 {
                continue;
            }
            let depth = plane.depth();
            let kit = &kits[plane.index()];
            let mut x = -((hash(depth.salt) % WEST_JITTER) as i32);
            let (mut slot, mut last) = (0u32, None);
            while x < i32::from(run_w) {
                let n = hash(depth.salt.wrapping_add(slot.wrapping_mul(131)));
                let (stand, w) = if kit.is_empty() {
                    let w = pick_in(&BLOCK_W, n);
                    let h = pct(band, pick_in(&BLOCK_H_PCT, n ^ 0x5A5A)).max(1);
                    let top = i32::from(glass_h) - i32::from(h);
                    (Stand::Block { x, w, top, hash: n }, w)
                } else {
                    let mut pick = n as usize % kit.len();
                    if kit.len() > 1 && last == Some(pick) {
                        pick = (pick + 1) % kit.len();
                    }
                    last = Some(pick);
                    let building = kit[pick];
                    let (w, h) = building.size();
                    let top = i32::from(glass_h) - i32::from(h.min(band));
                    (
                        Stand::Kit {
                            building,
                            x,
                            top,
                            hash: n,
                        },
                        w,
                    )
                };
                stands.push((plane, stand));
                x += i32::from(w) + i32::from(pick_in(&depth.gap, n ^ 0x7777));
                slot += 1;
            }
        }
        Self { stands }
    }

    /// Every building, far to near.
    pub(crate) fn stands(&self) -> impl Iterator<Item = (Plane, Stand<'p>)> + '_ {
        self.stands.iter().copied()
    }
}

/// Whether window `index` of a stand hashed `hash` burns under `darkness`, and
/// if so a seed for which of the theme's lit hues it burns in. The share of lit
/// windows rises from [`LIT_DAY_PCT`] at noon to the plane's own by night, each
/// window keeping its own place in that order, so lights come on one by one as
/// it darkens; a lit window now and then goes dark for a turn.
pub(crate) fn lit(
    plane: Plane,
    hash: u32,
    index: usize,
    darkness: f32,
    beat: crate::anim::Beat,
) -> Option<u32> {
    let depth = plane.depth();
    let r = self::hash(hash ^ depth.salt ^ (index as u32).wrapping_mul(0x2545_F491));
    let night = depth.lit_night_pct.saturating_sub(LIT_DAY_PCT) as f32;
    let share_pct = LIT_DAY_PCT as f32 + night * darkness.clamp(0.0, 1.0);
    if (r % PER_MILLE) as f32 >= share_pct * (PER_MILLE / 100) as f32 {
        return None;
    }
    let span = BLINK_CYCLE_MS.end() - BLINK_CYCLE_MS.start() + 1;
    let turn = beat.ms() / (BLINK_CYCLE_MS.start() + u64::from(r) % span);
    if self::hash(r ^ turn as u32).is_multiple_of(BLINK_OFF_IN) {
        return None;
    }
    Some(r / PER_MILLE)
}

/// A plane's colour for each [`Material`] under this hour's sky, its lit
/// windows' hues.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlaneColours {
    facade: Rgb,
    shade: Rgb,
    roof: Rgb,
    glass: Rgb,
    mullion: Rgb,
    detail: Rgb,
    sign: Rgb,
    lit: [Rgb; 3],
    /// A tower's aviation light, glowing as its lit windows do.
    beacon: Rgb,
}

impl PlaneColours {
    /// `plane`'s colours: the hour's building tone lifted by the plane's depth
    /// and hazed toward the horizon.
    pub(crate) fn of(plane: Plane, look: &SkyTones, theme: &Theme) -> Self {
        let depth = plane.depth();
        let o = &theme.office;
        let haze = |c: Rgb| c.mix(look.glass_horizon, depth.haze);
        let tone = o
            .building_light
            .mix(o.building_dark, look.darkness)
            .ramp(depth.lift);
        let glass = o.city_dark_window.mix(tone, DARK_GLASS_TONE);
        let glow = look.darkness.max(LIT_DAY_GLOW);
        PlaneColours {
            facade: haze(tone),
            shade: haze(tone.ramp(-1)),
            roof: haze(tone.ramp(1)),
            glass: haze(glass),
            mullion: haze(tone.ramp(-2)),
            detail: haze(tone.ramp(-1)),
            sign: haze(tone.mix(theme.lighting.twilight_a, look.darkness)),
            lit: o.city_lit_windows.map(|c| haze(glass.mix(c, glow))),
            beacon: haze(glass.mix(BEACON, glow)),
        }
    }

    /// The colour `material` is drawn in.
    pub(crate) fn material(&self, material: Material) -> Rgb {
        match material {
            Material::Facade => self.facade,
            Material::Shade => self.shade,
            Material::Roof => self.roof,
            Material::Glass => self.glass,
            Material::Mullion => self.mullion,
            Material::Detail => self.detail,
            Material::Sign => self.sign,
        }
    }

    /// The hue a window [`lit`] with `seed` burns in.
    fn lit(&self, seed: u32) -> Rgb {
        self.lit[seed as usize % self.lit.len()]
    }

    /// A window's colour: lit in its hue, or dark glass.
    fn window(&self, lit: Option<u32>) -> Rgb {
        lit.map_or(self.glass, |seed| self.lit(seed))
    }

    /// The palette overrides that draw a building's `[city]` keys in these
    /// colours.
    fn overrides(&self, materials: &CityMaterials) -> Vec<(char, Pixel)> {
        Material::ALL
            .into_iter()
            .map(|m| (materials.key(m), Some(self.material(m))))
            .collect()
    }
}

/// The city behind a window run, drawn onto an art grid of `density` art
/// pixels to the logical unit, `None` where the sky shows.
#[derive(PartialEq)]
pub(crate) struct CityStrip {
    w: u16,
    h: u16,
    px: Vec<Option<Rgb>>,
}

impl CityStrip {
    /// `pack`'s city across a run `run_w` wide behind glass `glass_h` tall, at
    /// `moment`, at `density`. A building the pack draws no art for at
    /// `density` is its base, each pixel grown to fill its cell.
    pub(crate) fn draw(
        pack: &OfficeArt,
        (run_w, glass_h): (u16, u16),
        outlook: &crate::atmosphere::Outlook,
        theme: &Theme,
        density: Density,
    ) -> Self {
        let (look, altitude, beat) = (outlook.look(), outlook.altitude(), outlook.beat());
        let d = density.get();
        let mut strip = CityStrip {
            w: run_w.saturating_mul(d),
            h: glass_h.saturating_mul(d),
            px: vec![
                None;
                usize::from(run_w.saturating_mul(d)) * usize::from(glass_h.saturating_mul(d))
            ],
        };
        let colours = Plane::ALL.map(|p| PlaneColours::of(p, look, theme));
        let mut recoloured: Vec<(&str, Plane, Frame)> = Vec::new();
        let tallest = tallest_near(pack);
        // Every light's art pixels: drawn once every tower stands, so none
        // stands over one.
        let mut beacons: Vec<(i32, i32)> = Vec::new();
        for (plane, stand) in Skyline::of(pack, run_w, glass_h, altitude).stands() {
            let c = &colours[plane.index()];
            let window = |i: usize| c.window(lit(plane, stand.hash(), i, look.darkness, beat));
            let beacon = carries_beacon(plane, &stand, tallest) && beacon_on(stand.hash(), beat);
            match stand {
                Stand::Block { x, w, top, .. } => {
                    let (x0, y0) = (x * i32::from(d), top * i32::from(d));
                    let (aw, ah) = (w.saturating_mul(d), glass_h.saturating_mul(d));
                    for ay in 0..(i32::from(ah) - y0).max(0) {
                        let ay = u16::try_from(ay).unwrap_or(u16::MAX);
                        for ax in 0..aw {
                            let colour = if block_window(ax, ay) {
                                window(usize::from(ay) * usize::from(aw) + usize::from(ax))
                            } else {
                                c.material(Material::Facade)
                            };
                            strip.put(x0 + i32::from(ax), y0 + i32::from(ay), colour);
                        }
                    }
                }
                Stand::Kit {
                    building, x, top, ..
                } => {
                    let materials = pack.city_materials();
                    let (art, grow) = building
                        .variant(density)
                        .map_or((building.base(), d), |a| (a, 1));
                    let frame = match recoloured
                        .iter()
                        .position(|(n, p, _)| *n == building.name() && *p == plane)
                    {
                        Some(i) => &recoloured[i].2,
                        None => {
                            let f = art
                                .sprite()
                                .recolorable_at(0)
                                .recolored(&c.overrides(materials));
                            recoloured.push((building.name(), plane, f));
                            &recoloured[recoloured.len() - 1].2
                        }
                    };
                    let (x0, y0) = (x * i32::from(d), top * i32::from(d));
                    let cell = |fx: u16, fy: u16| {
                        (
                            x0 + i32::from(fx) * i32::from(grow),
                            y0 + i32::from(fy) * i32::from(grow),
                        )
                    };
                    for fy in 0..frame.height() {
                        for fx in 0..frame.width() {
                            if let Some(colour) = frame.get(fx, fy).copied().flatten() {
                                strip.fill(cell(fx, fy), grow, colour);
                            }
                        }
                    }
                    for (i, pane) in art.windows().iter().enumerate() {
                        let colour = window(i);
                        for &(wx, wy) in pane {
                            strip.fill(cell(wx, wy), grow, colour);
                        }
                    }
                    // A cell of the art it stands in, so a base grown to the
                    // density grows its light with it; on finer art, half a
                    // unit across.
                    let side = if grow > 1 { grow } else { (d / 2).max(1) };
                    if beacon {
                        beacons.extend(beacon_cells(frame, (x0, y0), grow, side));
                    }
                }
            }
        }
        let beacon = colours[Plane::Near.index()].beacon;
        for (x, y) in beacons {
            strip.put(x, y, beacon);
        }
        strip
    }

    fn put(&mut self, x: i32, y: i32, colour: Rgb) {
        if let (Ok(x), Ok(y)) = (u16::try_from(x), u16::try_from(y))
            && x < self.w
            && y < self.h
        {
            self.px[usize::from(y) * usize::from(self.w) + usize::from(x)] = Some(colour);
        }
    }

    /// A `size`-square cell from `(x, y)`.
    fn fill(&mut self, (x, y): (i32, i32), size: u16, colour: Rgb) {
        for dy in 0..i32::from(size) {
            for dx in 0..i32::from(size) {
                self.put(x + dx, y + dy, colour);
            }
        }
    }

    /// What of this city stands before the sky at a window cell on a grid `d`
    /// cells to the unit, the run's west end at column `run_x0`.
    pub(crate) fn front(&self, run_x0: u16, d: Density) -> impl Fn(Cell) -> Option<Rgb> + '_ {
        let x0 = run_x0.saturating_mul(d.get());
        move |cell| self.at(cell.at.0.wrapping_sub(x0), cell.glass_offset.1)
    }

    /// What stands at `(x, y)` art pixels from the run's west end and the
    /// glass's top, or `None` where the sky shows.
    fn at(&self, x: u16, y: u16) -> Option<Rgb> {
        (x < self.w && y < self.h)
            .then(|| self.px[usize::from(y) * usize::from(self.w) + usize::from(x)])
            .flatten()
    }
}

/// A value in `range` chosen by `n`.
fn pick_in(range: &RangeInclusive<u16>, n: u32) -> u16 {
    let span = u32::from(range.end() - range.start()) + 1;
    range.start() + (hash(n) % span) as u16
}

/// A deterministic hash for the city: the same office always gets the same
/// skyline and the same lit windows, where a per-frame reshuffle would flicker.
fn hash(n: u32) -> u32 {
    let mut v = n.wrapping_mul(crate::GOLDEN_GAMMA_32);
    v ^= v >> 15;
    v = v.wrapping_mul(crate::MURMUR3_FMIX32_M1);
    v ^ (v >> 13)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atmosphere::Moment;
    use crate::display::pen::test_density;

    use crate::anim::{Beat, Motion};

    /// Every per-plane table is `Plane::ALL.map(..)` indexed by
    /// [`Plane::index`], so each plane in `ALL` sits at its own index.
    #[test]
    fn all_lists_every_plane_at_its_index() {
        for (i, plane) in Plane::ALL.into_iter().enumerate() {
            assert_eq!(plane.index(), i);
        }
    }

    fn pack() -> OfficeArt {
        crate::pack::test_office()
    }

    /// A window shows the strip read from the run's west end and the glass's
    /// top, at every density.
    #[test]
    fn a_window_shows_the_strip_from_the_runs_west_end() {
        const SKY: Rgb = Rgb { r: 1, g: 2, b: 3 };
        let now = crate::localclock::at_hour(12);
        let theme = &crate::theme::NORMAL;
        let moment = Moment::resolve(
            crate::sky::Sky::at_with(now, crate::sky::Weather::Clear),
            theme,
            0.0,
            Motion::Full.timing(now),
        );
        let (run_x0, glass_h) = (3, 30);
        let rows = 1..glass_h + 3;
        let bay = crate::layout::WindowBay {
            x: run_x0 + 25,
            w: crate::layout::WINDOW_W,
            idx: 1,
        };
        for d in [1, 4] {
            let density = test_density(d);
            let strip = CityStrip::draw(
                &pack(),
                (80, glass_h),
                &moment.outlook(theme),
                theme,
                density,
            );
            let front = strip.front(run_x0, density);
            let view = crate::outside::WindowView::new(bay, rows.clone(), density, |cell| {
                front(cell).unwrap_or(SKY)
            });
            let mut buildings = 0;
            for (at, c) in view.cells() {
                let (ax, ay) = (at.0 - bay.x * d, at.1 - rows.start * d);
                let stands = strip.at((bay.x - run_x0) * d + ax, ay - d);
                assert_eq!(c, stands.unwrap_or(SKY), "{d}: {at:?}");
                buildings += usize::from(stands.is_some());
            }
            assert!(buildings > 0, "{d}: the window shows a city");
        }
    }

    /// Where each stand stands, for comparing two cities.
    fn places(city: &Skyline<'_>) -> Vec<(Plane, i32, i32)> {
        city.stands()
            .map(|(p, s)| match s {
                Stand::Kit { x, top, .. } | Stand::Block { x, top, .. } => (p, x, top),
            })
            .collect()
    }

    #[test]
    fn the_city_is_drawn_far_to_near_across_the_whole_run() {
        let pack = pack();
        let city = Skyline::of(&pack, 120, 20, 0.0);
        let planes: Vec<_> = city.stands().map(|(p, _)| p).collect();
        assert!(planes.windows(2).all(|w| w[0] <= w[1]), "far to near");
        for plane in Plane::ALL {
            let reach = city
                .stands()
                .filter(|&(p, _)| p == plane)
                .map(|(_, s)| match s {
                    Stand::Kit { building, x, .. } => x + i32::from(building.size().0),
                    Stand::Block { x, w, .. } => x + i32::from(w),
                })
                .max()
                .expect("every plane stands something");
            let gap = i32::from(*plane.depth().gap.end());
            assert!(
                reach + gap >= 120,
                "{plane:?} reaches the run's east end, but for a gap"
            );
        }
    }

    #[test]
    fn a_wider_run_shows_more_of_the_same_city() {
        let pack = pack();
        let narrow = places(&Skyline::of(&pack, 60, 20, 0.0));
        let wide = places(&Skyline::of(&pack, 140, 20, 0.0));
        for stand in &narrow {
            assert!(wide.contains(stand), "{stand:?} stays put");
        }
    }

    #[test]
    fn a_building_stands_only_in_the_planes_it_names() {
        let pack = pack();
        for (plane, stand) in Skyline::of(&pack, 200, 20, 0.0).stands() {
            match (plane, stand) {
                (Plane::Far, Stand::Block { .. }) => {}
                (_, Stand::Kit { building, .. }) => {
                    let p = plane.pack_plane().expect("a pack plane");
                    assert!(building.stands_in(p), "{} in {plane:?}", building.name());
                }
                (p, s) => panic!("the bundled pack stands buildings in {p:?}: {s:?}"),
            }
        }
    }

    /// The tallest a stand rises over the sill.
    fn rise(glass_h: u16, stand: Stand<'_>) -> i32 {
        i32::from(glass_h)
            - match stand {
                Stand::Kit { top, .. } | Stand::Block { top, .. } => top,
            }
    }

    #[test]
    fn every_window_keeps_its_sky_and_shows_every_plane() {
        let pack = pack();
        for glass_h in [12, 16, 21, 38, 60] {
            for altitude in [0.0, 1.0] {
                assert!(
                    clear_sky_rows(glass_h) > 0,
                    "{glass_h}-row glass keeps some sky"
                );
                let city = Skyline::of(&pack, 120, glass_h, altitude);
                for plane in Plane::ALL {
                    let rises: Vec<_> = city
                        .stands()
                        .filter(|&(p, _)| p == plane)
                        .map(|(_, s)| rise(glass_h, s))
                        .collect();
                    assert!(
                        rises.iter().any(|&r| r > 0),
                        "{plane:?} shows behind {glass_h}-row glass at altitude {altitude}"
                    );
                    assert!(
                        rises
                            .iter()
                            .all(|&r| r <= i32::from(glass_h - clear_sky_rows(glass_h))),
                        "{plane:?} leaves the top {} rows to the sky",
                        clear_sky_rows(glass_h)
                    );
                }
            }
        }
    }

    #[test]
    fn from_the_ground_floor_the_nearer_planes_stand_taller() {
        let pack = pack();
        for glass_h in [12, 38, 60] {
            let city = Skyline::of(&pack, 200, glass_h, 0.0);
            let tallest = |plane| {
                city.stands()
                    .filter(|&(p, _)| p == plane)
                    .map(|(_, s)| rise(glass_h, s))
                    .max()
                    .expect("the plane stands something")
            };
            assert!(
                tallest(Plane::Far) <= tallest(Plane::Mid)
                    && tallest(Plane::Mid) <= tallest(Plane::Near),
                "behind {glass_h}-row glass"
            );
        }
    }

    #[test]
    fn a_higher_office_sees_the_near_city_lower_and_the_horizon_hold() {
        let pack = pack();
        let ground = Skyline::of(&pack, 80, 38, 0.0);
        let top = Skyline::of(&pack, 80, 38, 1.0);
        let mut lower = false;
        for ((plane, g), (_, t)) in ground.stands().zip(top.stands()) {
            let (g, t) = (rise(38, g), rise(38, t));
            match plane {
                Plane::Far => assert_eq!(t, g, "the horizon holds"),
                _ => {
                    assert!(t <= g, "{plane:?} never rises");
                    lower |= t < g;
                }
            }
        }
        assert!(lower, "the nearer planes drop away");
    }

    #[test]
    fn a_denser_strip_draws_the_denser_art_on_the_same_city() {
        let pack = pack();
        let theme = crate::theme::theme_by_name("normal").expect("theme");
        let sky = crate::sky::Sky::clock(std::time::UNIX_EPOCH);
        let strip = |d| {
            CityStrip::draw(
                &pack,
                (60, 20),
                &Moment::resolve(sky, theme, 0.0, Motion::Full.timing(std::time::UNIX_EPOCH))
                    .outlook(theme),
                theme,
                test_density(d),
            )
        };
        let (one, three, four) = (strip(1), strip(3), strip(4));
        assert_eq!((four.w, four.h), (one.w * 4, one.h * 4));
        // The near plane's cells: drawn last, so nothing else stands over them.
        let near: Vec<(u16, u16)> = Skyline::of(&pack, 60, 20, 0.0)
            .stands()
            .filter_map(|(plane, s)| match (plane, s) {
                (
                    Plane::Near,
                    Stand::Kit {
                        building, x, top, ..
                    },
                ) => Some((building, x, top)),
                _ => None,
            })
            .flat_map(|(b, x, top)| {
                let (w, h) = b.size();
                (0..w).flat_map(move |dx| {
                    (0..h).map(move |dy| (x + i32::from(dx), top + i32::from(dy)))
                })
            })
            .filter_map(|(x, y)| Some((u16::try_from(x).ok()?, u16::try_from(y).ok()?)))
            .filter(|&(x, y)| one.at(x, y).is_some())
            .collect();
        assert!(!near.is_empty(), "the near plane stands in the strip");
        let uniform = |s: &CityStrip, d: u16, (x, y): (u16, u16)| {
            (0..d).all(|dy| (0..d).all(|dx| s.at(x * d + dx, y * d + dy) == s.at(x * d, y * d)))
        };
        assert!(
            near.iter().any(|&c| !uniform(&four, 4, c)),
            "the buildings' 4x art draws detail finer than a logical cell"
        );
        assert!(
            near.iter().all(|&(x, y)| uniform(&three, 3, (x, y)) && three.at(x * 3, y * 3) == one.at(x, y)),
            "with no 3x art, a building is its base grown to the cell"
        );
        let stands_at = |s: &CityStrip, d: u16, x: u16| {
            (0..s.h).find(|&y| s.at(x * d, y).is_some()).map(|y| y / d)
        };
        let near_matches = (0..one.w)
            .filter(|&x| stands_at(&one, 1, x).is_some() == stands_at(&four, 4, x).is_some())
            .count();
        assert!(
            near_matches * 10 >= usize::from(one.w) * 9,
            "the same city stands in the same columns at either density"
        );
    }

    /// A light burns only on a tallest near tower that carries one, in its
    /// top rows, over pixels the tower already covers; half a turn later each
    /// has flipped, and the city stands where it stood.
    #[test]
    fn aviation_lights_sit_only_on_rooftops_and_blink() {
        let pack = pack();
        let theme = crate::theme::theme_by_name("normal").expect("theme");
        let (run_w, glass_h) = (400, 24);
        let night = crate::localclock::at_hour(23);
        for d in [1, 4] {
            let side = (d / 2).max(1);
            let strip = |now| {
                let moment = Moment::resolve(
                    crate::sky::Sky::at_with(now, crate::sky::Weather::Clear),
                    theme,
                    0.0,
                    Motion::Full.timing(now),
                );
                let near = PlaneColours::of(Plane::Near, &moment.look, theme);
                let s = CityStrip::draw(
                    &pack,
                    (run_w, glass_h),
                    &moment.outlook(theme),
                    theme,
                    test_density(d),
                );
                (s, near)
            };
            let half = std::time::Duration::from_millis(BEACON_HALF_MS);
            let ((a, near), (b, _)) = (strip(night), strip(night + half));
            let beacon = near.beacon;
            let lights = |s: &CityStrip| -> Vec<(u16, u16)> {
                (0..s.h)
                    .flat_map(|y| (0..s.w).map(move |x| (x, y)))
                    .filter(|&(x, y)| s.at(x, y) == Some(beacon))
                    .collect()
            };
            let (on_a, on_b) = (lights(&a), lights(&b));
            assert!(
                !on_a.is_empty() || !on_b.is_empty(),
                "{d}: the towers blink"
            );
            assert!(on_a.iter().all(|p| !on_b.contains(p)), "{d}: in turn");
            let shown = |s: &CityStrip| -> Vec<bool> { s.px.iter().map(Option::is_some).collect() };
            assert_eq!(shown(&a), shown(&b), "{d}: no light in the sky");
            let city = Skyline::of(&pack, run_w, glass_h, 0.0);
            let tallest = tallest_near(&pack);
            let materials = pack.city_materials();
            for &(x, y) in on_a.iter().chain(&on_b) {
                let (x, y) = (i32::from(x), i32::from(y));
                let on_a_roof = city.stands().any(|(plane, s)| {
                    let Stand::Kit {
                        building,
                        x: sx,
                        top,
                        ..
                    } = s
                    else {
                        return false;
                    };
                    let (art, grow) = building
                        .variant(test_density(d))
                        .map_or((building.base(), d), |a| (a, 1));
                    let frame = art
                        .sprite()
                        .recolorable_at(0)
                        .recolored(&near.overrides(materials));
                    // Its art's first drawn row: its roof, or its spire's tip.
                    let first = (0..frame.height())
                        .find(|&fy| {
                            (0..frame.width())
                                .any(|fx| frame.get(fx, fy).copied().flatten().is_some())
                        })
                        .expect("drawn");
                    let d = i32::from(d);
                    let roof = top * d + i32::from(first * grow);
                    carries_beacon(plane, &s, tallest)
                        && (sx * d..(sx + i32::from(building.size().0)) * d).contains(&x)
                        && (roof..roof + i32::from(side.max(grow))).contains(&y)
                });
                assert!(on_a_roof, "{d}: a light at ({x}, {y}) off any rooftop");
            }
        }
    }

    /// A light's square takes only its own tower's pixels: where it overlaps
    /// another building around a thin mast, a strip full of that building
    /// keeps it there. Only art finer than its base has such a mast.
    #[test]
    fn a_light_leaves_a_neighbours_pixels_alone() {
        let pack = pack();
        let theme = crate::theme::theme_by_name("normal").expect("theme");
        let materials = pack.city_materials();
        let near = PlaneColours::of(
            Plane::Near,
            &SkyTones::resolve(
                &crate::sky::Sky::at_with(std::time::UNIX_EPOCH, crate::sky::Weather::Clear),
                theme,
            ),
            theme,
        );
        let tallest = tallest_near(&pack);
        let neighbour = Rgb { r: 1, g: 2, b: 3 };
        let mut spared = 0;
        for tower in pack
            .buildings()
            .filter(|b| b.stands_in(CityPlane::Near) && b.size().1 >= tallest)
        {
            for d in 1..=4 {
                let (art, grow) = tower
                    .variant(test_density(d))
                    .map_or((tower.base(), d), |a| (a, 1));
                let frame = art
                    .sprite()
                    .recolorable_at(0)
                    .recolored(&near.overrides(materials));
                let side = if grow > 1 { grow } else { (d / 2).max(1) };
                let (w, h) = (frame.width() * grow + side, frame.height() * grow + side);
                let mut strip = CityStrip {
                    w,
                    h,
                    px: vec![Some(neighbour); usize::from(w) * usize::from(h)],
                };
                let cells = beacon_cells(&frame, (0, 0), grow, side);
                let &(tx, ty) = cells.first().expect("a lit tip");
                for &(x, y) in &cells {
                    strip.put(x, y, near.beacon);
                }
                for (x, y) in (0..side).flat_map(|dy| (0..side).map(move |dx| (dx, dy))) {
                    let (x, y) = (tx + i32::from(x), ty + i32::from(y));
                    let own = frame
                        .get(
                            u16::try_from(x).expect("in") / grow,
                            u16::try_from(y).expect("in") / grow,
                        )
                        .copied()
                        .flatten()
                        .is_some();
                    let want = if own { near.beacon } else { neighbour };
                    let got =
                        strip.at(u16::try_from(x).expect("in"), u16::try_from(y).expect("in"));
                    assert_eq!(got, Some(want), "{} at {d}: ({x}, {y})", tower.name());
                    spared += usize::from(!own);
                }
            }
        }
        assert!(spared > 0, "some light's square overlaps a neighbour");
    }

    /// The towers' lights blink out of step: at one instant some carriers
    /// are lit and others dark.
    #[test]
    fn aviation_lights_blink_out_of_step() {
        let pack = pack();
        let tallest = tallest_near(&pack);
        let carriers: Vec<u32> = Skyline::of(&pack, 1200, 24, 0.0)
            .stands()
            .filter(|(plane, s)| carries_beacon(*plane, s, tallest))
            .map(|(_, s)| s.hash())
            .collect();
        assert!(carriers.len() >= 2, "{} carriers", carriers.len());
        let now = Motion::Full.beat(crate::localclock::at_hour(23));
        let lit = carriers.iter().filter(|&&h| beacon_on(h, now)).count();
        assert!(
            0 < lit && lit < carriers.len(),
            "{lit} of {} lit at once",
            carriers.len()
        );
    }

    #[test]
    fn more_windows_burn_as_it_darkens() {
        let now = Beat::at_ms(0);
        let count = |d: f32| {
            (0..2000)
                .filter(|&i| lit(Plane::Near, 7, i, d, now).is_some())
                .count()
        };
        let (noon, dusk, night) = (count(0.0), count(0.5), count(1.0));
        assert!(noon < dusk && dusk < night, "{noon} < {dusk} < {night}");
        assert!(noon > 0, "some lights are on by day");
        assert!(night < 2000, "some windows stay dark all night");
        for i in 0..2000 {
            if lit(Plane::Near, 7, i, 0.3, now).is_some() {
                assert!(
                    lit(Plane::Near, 7, i, 0.9, now).is_some(),
                    "a light that is on stays on as it darkens"
                );
            }
        }
    }
}
