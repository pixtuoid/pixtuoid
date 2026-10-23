//! The clouds the windows show: masses of lobes over a flat base, a deck of
//! them far to near, lit by the sky body, drifting on the beat, and a storm's
//! bolt dropping from a base. They draw on a window's glass between the sky
//! and the city ([`Outside::through`](crate::outside::Outside::through)), so
//! the city stands in front and the joinery is never theirs.
//!
//! Every length is in layout units: x from the window run's west end, y from
//! the glass's top.

use pixtuoid_core::sprite::format::Density;
use std::ops::Range;

use pixtuoid_core::sprite::Rgb;

use crate::atmosphere::Moment;
use crate::outside::{Cell, WindowView};

use crate::sky::{Element, Sky, StrikePhase, Weather};

/// A deterministic stream of `0..1` draws: [`crate::splitmix_draw`]'s.
struct Rng {
    seed: u64,
    n: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { seed, n: 0 }
    }

    fn u(&mut self) -> f32 {
        self.n += 1;
        let draw = crate::splitmix_draw(self.seed, self.n);
        (draw >> (u64::BITS - f32::MANTISSA_DIGITS)) as f32 / (1u64 << f32::MANTISSA_DIGITS) as f32
    }

    fn between(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.u()
    }
}

/// `0..1` value noise along `x`, one lattice point per unit, smoothstepped.
fn noise(seed: u64, x: f32) -> f32 {
    let i = x.floor();
    let f = crate::anim::Easing::Smoothstep.apply(x - i);
    let at = |k: f32| (crate::splitmix_draw(seed, k as i64 as u64) % 1000) as f32 / 1000.0;
    at(i) + (at(i + 1.0) - at(i)) * f
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Lobe {
    x: f32,
    y: f32,
    r: f32,
}

/// Which band of its ramp a cloud cell is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Band {
    Lit,
    Body,
    Shade,
}

/// How deep in the deck a mass hangs: a far one leans most to the sky behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Far,
    Mid,
    Near,
}

impl Layer {
    /// How much a mass in it leans to its row's sky.
    fn sky_mix(self) -> f32 {
        match self {
            Layer::Far => 0.55,
            Layer::Mid => 0.25,
            Layer::Near => 0.0,
        }
    }

    /// Its drift, in units per second of loop time: the far deck slowest.
    fn pace(self) -> f32 {
        match self {
            Layer::Far => 0.05,
            Layer::Mid => 0.08,
            Layer::Near => 0.12,
        }
    }
}

/// One mass: its lobes over a flat base, at a weather's full share.
#[derive(Debug, Clone, PartialEq)]
struct Mass {
    layer: Layer,
    lobes: Vec<Lobe>,
    base: f32,
    /// The weather it belongs to, whose tones it takes.
    weather: Weather,
    /// Its seed for the base's waver.
    seed: u64,
    /// Its place in its weather's deck.
    id: usize,
    /// Its west extent at full share, undrifted: what its own noise keys on,
    /// so its waver and shading ride with it as it drifts.
    anchor: f32,
    /// How far east it has drifted this frame, in units.
    off: f32,
    /// Its deck's widest mass's width, which its wrap clears ([`drifted_west`]).
    widest: f32,
}

/// A mass as its deck draws it, before the deck is whole: what it takes to
/// place it there ([`Mass::deck`]).
struct Shape {
    layer: Layer,
    lobes: Vec<Lobe>,
    base: f32,
    seed: u64,
}

/// The west and east extent of `lobes`, in units.
fn extent(lobes: &[Lobe]) -> (f32, f32) {
    lobes.iter().fold((f32::MAX, f32::MIN), |(w, e), l| {
        (w.min(l.x - l.r), e.max(l.x + l.r))
    })
}

impl Mass {
    /// `weather`'s deck of `shapes`, each knowing its place in it, its anchor
    /// and the deck's widest width, undrifted.
    fn deck(weather: Weather, shapes: Vec<Shape>) -> Vec<Mass> {
        let widest = shapes
            .iter()
            .map(|s| {
                let (west, east) = extent(&s.lobes);
                east - west
            })
            .fold(0.0, f32::max);
        shapes
            .into_iter()
            .enumerate()
            .map(|(id, s)| Mass {
                anchor: extent(&s.lobes).0,
                layer: s.layer,
                lobes: s.lobes,
                base: s.base,
                weather,
                seed: s.seed,
                id,
                off: 0.0,
                widest,
            })
            .collect()
    }

    /// Its west and east extent, in units.
    fn extent(&self) -> (f32, f32) {
        extent(&self.lobes)
    }

    /// Its top row, in units.
    fn top(&self) -> f32 {
        self.lobes
            .iter()
            .map(|l| l.y - l.r)
            .fold(f32::MAX, f32::min)
    }

    /// The flat base at `x`, wavering a little by column.
    fn base_at(&self, x: f32) -> f32 {
        let local = (x - self.anchor) / BASE_WAVER_PERIOD;
        self.base + BASE_WAVER * noise(self.seed, local) - BASE_WAVER / 2.0
    }

    /// How far east it has drifted `secs` into the beat over a run `span` wide.
    fn drift_at(&self, secs: f64, span: f32) -> f32 {
        let travelled = secs * f64::from(self.layer.pace());
        drifted_west(self.anchor, travelled, span, self.widest) - self.anchor
    }

    /// The mass `share` of the way grown from its base, drifted `off` east.
    fn grown(&self, share: f32, off: f32) -> Mass {
        let s = share.clamp(0.0, 1.0);
        Mass {
            lobes: self
                .lobes
                .iter()
                .map(|l| Lobe {
                    x: l.x,
                    y: self.base - (self.base - l.y) * s,
                    r: l.r * s,
                })
                .collect(),
            off,
            ..self.clone()
        }
    }

    /// Whether any lobe has grown in at all.
    fn shows(&self) -> bool {
        self.lobes.iter().any(|l| l.r > 0.0)
    }

    /// In a lobe, in a skirt under one, or in a short crease between two in
    /// one column; never under the base.
    fn solid(&self, x: f32, y: f32) -> bool {
        if y > self.base_at(x) {
            return false;
        }
        let lobes = &self.lobes;
        if lobes
            .iter()
            .any(|l| (x - l.x).powi(2) + (y - l.y).powi(2) <= l.r * l.r)
        {
            return true;
        }
        // a skirt down to the base, narrowing slowly: the base is flat, its
        // ends slant (no bowl, no wall)
        if lobes
            .iter()
            .any(|l| y >= l.y && (x - l.x).abs() <= l.r * (1.0 - SKIRT * (y - l.y) / l.r))
        {
            return true;
        }
        let (mut above, mut below) = (false, false);
        for l in lobes {
            if (x - l.x).abs() < l.r {
                let half = (l.r * l.r - (x - l.x).powi(2)).sqrt();
                above |= y - CREASE < l.y + half && l.y + half < y;
                below |= y < l.y - half && l.y - half < y + CREASE;
            }
        }
        above && below
    }

    /// Solid, with any slit narrower than [`SLIT`] closed: a hairline gap
    /// through a mass reads as a wisp of smoke.
    fn inside(&self, x: f32, y: f32) -> bool {
        self.solid(x, y) || (self.solid(x - SLIT, y) && self.solid(x + SLIT, y))
    }

    /// The mass as a surface: each lobe a dome, joined by a soft max, so the
    /// creases between lobes are valleys, not cracks.
    fn height(&self, x: f32, y: f32) -> f32 {
        let acc: f32 = self
            .lobes
            .iter()
            .filter_map(|l| {
                let h = l.r * l.r - (x - l.x).powi(2) - (y - l.y).powi(2);
                (h > 0.0).then(|| {
                    let e = DOME_EASE * l.r;
                    (SMOOTH * ((h + e * e).sqrt() - e)).exp()
                })
            })
            .sum();
        if acc > 0.0 { acc.ln() / SMOOTH } else { 0.0 }
    }
}

const BASE_WAVER: f32 = 1.2;
const BASE_WAVER_PERIOD: f32 = 5.0;
/// How fast a skirt narrows per lobe radius down.
const SKIRT: f32 = 0.3;
/// The longest gap between two lobes in one column that fills as a crease.
const CREASE: f32 = 1.5;
/// Within one mass, a gap this narrow is a notch, not sky.
const SLIT: f32 = 2.0;
/// A lobe this big carries smaller bumps on its upper rim.
const BUMP_FROM: f32 = 3.5;
/// A dome eased at its rim, as a share of its radius: a finite slope where a
/// bump meets its lobe.
const DOME_EASE: f32 = 0.3;
/// The soft max's sharpness: lower melts lobes together.
const SMOOTH: f32 = 3.0;
/// The surface normal's depth: lower rounds the lobes.
const RELIEF: f32 = 0.5;
/// The closing's reach: a notch, tooth or slit narrower fills.
const CLOSE: f32 = 1.0;

/// A cumulus's crown: the chance of a second crown lobe, each lobe's radius
/// and its sway aside as shares of the mass's width, and the share of its
/// radius its centre sits below the top.
const CROWN_SECOND: f32 = 0.5;
const CROWN_R: (f32, f32) = (0.26, 0.36);
const CROWN_SWAY: (f32, f32) = (-0.18, 0.18);
const CROWN_SIT: f32 = 0.9;
/// Its flanks, each side: up to this many more lobes than one, each lobe's
/// radius and reach out as shares of the width, and the share of its radius
/// its centre sinks past the base, which cuts it flat.
const FLANK_MORE: f32 = 2.0;
const FLANK_R: (f32, f32) = (0.14, 0.24);
const FLANK_REACH: (f32, f32) = (0.22, 0.46);
const FLANK_SINK: (f32, f32) = (0.4, 0.8);
/// Its skirt along the base: one lobe per this many units of width (at
/// least [`SKIRT_LOBES_MIN`]), each lobe's radius a share of the width,
/// spread over this share of it from its west edge's share, jittered by this
/// share of a slot, and sat this share of its radius above the base.
const SKIRT_PITCH: f32 = 3.0;
const SKIRT_LOBES_MIN: usize = 2;
const SKIRT_R: (f32, f32) = (0.12, 0.2);
const SKIRT_FROM: f32 = 0.45;
const SKIRT_SPAN: f32 = 0.9;
const SKIRT_JITTER: f32 = 0.6;
const SKIRT_SIT: f32 = 0.6;

/// A heap: one or two big top lobes, smaller side lobes, a filled base.
fn cumulus(r: &mut Rng, cx: f32, base: f32, width: f32, height: f32) -> Vec<Lobe> {
    let mut lobes = Vec::new();
    for _ in 0..1 + usize::from(r.u() < CROWN_SECOND) {
        let rad = width * r.between(CROWN_R.0, CROWN_R.1);
        lobes.push(Lobe {
            x: cx + width * r.between(CROWN_SWAY.0, CROWN_SWAY.1),
            y: base - height + rad * CROWN_SIT,
            r: rad,
        });
    }
    for side in [-1.0, 1.0] {
        for _ in 0..1 + (r.u() * FLANK_MORE) as usize {
            let rad = width * r.between(FLANK_R.0, FLANK_R.1);
            let x = cx + side * width * r.between(FLANK_REACH.0, FLANK_REACH.1);
            lobes.push(Lobe {
                x,
                y: base - rad * r.between(FLANK_SINK.0, FLANK_SINK.1),
                r: rad,
            });
        }
    }
    let n = ((width / SKIRT_PITCH) as usize).max(SKIRT_LOBES_MIN);
    for i in 0..n {
        let rad = width * r.between(SKIRT_R.0, SKIRT_R.1);
        let x = cx - width * SKIRT_FROM
            + width * SKIRT_SPAN * (i as f32 + r.u() * SKIRT_JITTER) / n as f32;
        lobes.push(Lobe {
            x,
            y: base - rad * SKIRT_SIT,
            r: rad,
        });
    }
    lobes
}

/// A cumulonimbus: its heap's height and its anvil's row as shares of the
/// glass, how near the anvil its column stops, how far the column narrows
/// at the top, each column lobe's radius and sway as shares of the width and
/// its step up as a share of its radius.
const HEAP_H: f32 = 0.22;
const ANVIL_Y: f32 = 0.10;
const COLUMN_STOP: f32 = 1.5;
const COLUMN_TAPER: f32 = 0.45;
const COLUMN_R: (f32, f32) = (0.28, 0.36);
const COLUMN_SWAY: (f32, f32) = (-0.12, 0.12);
const COLUMN_STEP: (f32, f32) = (0.7, 1.1);
/// Its anvil: this many lobes, each lobe's radius a share of the width,
/// spread from this share of it west across this share, ragged by this much.
const ANVIL_LOBES: usize = 6;
const ANVIL_R: (f32, f32) = (0.12, 0.2);
const ANVIL_FROM: f32 = 0.95;
const ANVIL_SPAN: f32 = 1.9;
const ANVIL_RAGGED: (f32, f32) = (-0.4, 0.6);

/// A cumulonimbus: a broad heap, a column narrowing as it climbs, and an anvil
/// spread flat across its top at `anvil_y`.
fn tower(r: &mut Rng, cx: f32, base: f32, width: f32, glass_h: f32) -> Vec<Lobe> {
    let mut lobes = cumulus(r, cx, base, width, glass_h * HEAP_H);
    let anvil_y = glass_h * ANVIL_Y;
    let mut y = base - glass_h * HEAP_H;
    let mut climb = 0.0;
    while y > anvil_y + COLUMN_STOP {
        let taper = 1.0 - COLUMN_TAPER * climb / (base - anvil_y).max(1.0);
        // a column as broad as the heap: a thin one reads as smoke
        let rad = width * taper * r.between(COLUMN_R.0, COLUMN_R.1);
        let side = r.between(COLUMN_SWAY.0, COLUMN_SWAY.1) * width * taper;
        lobes.push(Lobe {
            x: cx + side,
            y,
            r: rad,
        });
        let step = rad * r.between(COLUMN_STEP.0, COLUMN_STEP.1);
        y -= step;
        climb += step;
    }
    for i in 0..ANVIL_LOBES {
        // the anvil: flat on top, ragged underneath
        let rad = width * r.between(ANVIL_R.0, ANVIL_R.1);
        lobes.push(Lobe {
            x: cx + width * (-ANVIL_FROM + ANVIL_SPAN * (i as f32 + r.u()) / ANVIL_LOBES as f32),
            y: anvil_y + r.between(ANVIL_RAGGED.0, ANVIL_RAGGED.1),
            r: rad,
        });
    }
    lobes
}

/// A rim bump: the chance of a third, the arc it sits on in half-turns, its
/// radius as a share of its lobe's, and how far its radius sinks it in.
const BUMP_THIRD: f32 = 0.5;
const BUMP_ARC: (f32, f32) = (1.1, 1.9);
const BUMP_R: (f32, f32) = (0.3, 0.45);
const BUMP_INSET: f32 = 0.4;

/// Every big lobe's arc broken by two or three smaller bumps on its upper rim,
/// so no silhouette is one clean circle.
fn cauliflower(lobes: Vec<Lobe>, seed: u64) -> Vec<Lobe> {
    let mut r = Rng::new(seed);
    let mut out = lobes.clone();
    for l in lobes.iter().filter(|l| l.r >= BUMP_FROM) {
        for _ in 0..2 + usize::from(r.u() < BUMP_THIRD) {
            // the upper half, y down
            let a = std::f32::consts::PI * r.between(BUMP_ARC.0, BUMP_ARC.1);
            let b = l.r * r.between(BUMP_R.0, BUMP_R.1);
            out.push(Lobe {
                x: l.x + a.cos() * (l.r - b * BUMP_INSET),
                y: l.y + a.sin() * (l.r - b * BUMP_INSET),
                r: b,
            });
        }
    }
    out
}

/// The seeds of the decks' draws and of the city glow's, the shade climb's
/// and the flash rings' noise: each its own stream.
const DECK_SEED: u64 = 0x0c10_0d5e;
const GLOW_SEED: u64 = 0x617;
const SHADE_SEED: u64 = 0x1b5b;
const RING_SEED: u64 = 0xf1a5;

/// The deck a weather hangs over a run `span` units wide and glass `glass_h`
/// tall, far first: every mass at its full share, undrifted.
fn deck(weather: Weather, span: f32, glass_h: f32) -> Vec<Mass> {
    let mut r = Rng::new(DECK_SEED ^ weather as u64);
    let mut out = Vec::new();
    let mut row = |r: &mut Rng,
                   layer: Layer,
                   n: usize,
                   base_h: f32,
                   (w0, w1): (f32, f32),
                   (h0, h1): (f32, f32),
                   jitter: f32| {
        let mut xs: Vec<f32> = (0..n)
            .map(|i| span * (i as f32 + r.u() * jitter + (1.0 - jitter) / 2.0) / n as f32)
            .collect();
        xs.sort_by(f32::total_cmp);
        for x in xs {
            let width = r.between(w0, w1);
            let base = glass_h * base_h + r.between(-2.0, 2.0);
            let height = r.between(h0, h1);
            let seed = (r.u() * 1e6) as u64;
            out.push(Shape {
                layer,
                lobes: cauliflower(cumulus(r, x, base, width, height), seed),
                base,
                seed,
            });
        }
    };
    match deck_of(weather) {
        DeckKind::Scattered => {
            row(&mut r, Layer::Far, 2, 0.20, (5.0, 8.0), (2.5, 3.5), 0.6);
            row(&mut r, Layer::Near, 2, 0.36, (11.0, 16.0), (5.0, 7.0), 0.6);
        }
        DeckKind::Cover => {
            row(&mut r, Layer::Far, 4, 0.16, (10.0, 18.0), (3.0, 4.5), 0.9);
            row(&mut r, Layer::Mid, 4, 0.26, (12.0, 18.0), (4.0, 6.0), 0.6);
            row(&mut r, Layer::Near, 3, 0.32, (14.0, 20.0), (5.0, 7.0), 0.6);
        }
        DeckKind::Rain => {
            row(&mut r, Layer::Far, 4, 0.20, (12.0, 20.0), (3.5, 5.5), 0.9);
            row(&mut r, Layer::Mid, 5, 0.30, (14.0, 20.0), (5.0, 7.0), 0.6);
            row(&mut r, Layer::Near, 3, 0.38, (16.0, 22.0), (6.0, 8.0), 0.6);
        }
        DeckKind::Storm => {
            row(&mut r, Layer::Far, 4, 0.22, (12.0, 20.0), (4.0, 6.0), 0.9);
            row(&mut r, Layer::Mid, 4, 0.32, (14.0, 20.0), (5.0, 7.0), 0.6);
            let x = span * r.between(0.35, 0.65);
            let base = glass_h * 0.34;
            let width = r.between(20.0, 26.0);
            let seed = (r.u() * 1e6) as u64;
            out.push(Shape {
                layer: Layer::Near,
                lobes: cauliflower(tower(&mut r, x, base, width, glass_h), seed),
                base,
                seed,
            });
        }
    }
    Mass::deck(weather, out)
}

/// The kind of deck a weather hangs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeckKind {
    Scattered,
    Cover,
    Rain,
    Storm,
}

fn deck_of(weather: Weather) -> DeckKind {
    match weather {
        Weather::Clear | Weather::Windy => DeckKind::Scattered,
        Weather::Overcast | Weather::Snow | Weather::Fog | Weather::Smog => DeckKind::Cover,
        Weather::Rain => DeckKind::Rain,
        Weather::Storm => DeckKind::Storm,
    }
}

/// How far a weather leans its clouds to their storm grey.
fn heaviness(weather: Weather) -> f32 {
    match weather {
        Weather::Clear => 0.0,
        Weather::Windy => 0.1,
        Weather::Snow => 0.3,
        Weather::Overcast => 0.35,
        Weather::Fog => 0.4,
        Weather::Smog => 0.45,
        Weather::Rain => 0.6,
        Weather::Storm => 0.85,
    }
}

/// How the sky body lights the clouds at one time of day.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lighting {
    /// The light's lean on the glass, x east and y down.
    dir: (f32, f32),
    /// Its height: a low sun or moon catches only the edges facing it.
    z: f32,
    /// How much of the lit band a low body keeps to the rim.
    rim: f32,
    /// How deep the base's own shadow is.
    occlude: f32,
    /// Light, body and shade.
    tones: [Rgb; 3],
}

const DAY: Lighting = Lighting {
    dir: (-0.35, -1.0),
    z: 0.8,
    rim: 0.0,
    occlude: 0.5,
    tones: [
        Rgb {
            r: 255,
            g: 251,
            b: 242,
        },
        Rgb {
            r: 231,
            g: 235,
            b: 243,
        },
        Rgb {
            r: 176,
            g: 189,
            b: 214,
        },
    ],
};
const SUNSET: Lighting = Lighting {
    dir: (-1.0, 0.15),
    z: 0.35,
    rim: 0.22,
    occlude: 0.15,
    tones: [
        Rgb {
            r: 255,
            g: 194,
            b: 146,
        },
        Rgb {
            r: 222,
            g: 168,
            b: 168,
        },
        Rgb {
            r: 122,
            g: 100,
            b: 146,
        },
    ],
};
const NIGHT: Lighting = Lighting {
    dir: (0.55, -1.0),
    z: 0.3,
    rim: 0.1,
    occlude: 0.5,
    tones: [
        Rgb {
            r: 178,
            g: 188,
            b: 214,
        },
        Rgb {
            r: 66,
            g: 74,
            b: 98,
        },
        Rgb {
            r: 34,
            g: 38,
            b: 56,
        },
    ],
};
/// A heavy sky's tones, which each weather leans to by its [`heaviness`].
const STORM_GREY: [Rgb; 3] = [
    Rgb {
        r: 118,
        g: 122,
        b: 134,
    },
    Rgb {
        r: 84,
        g: 88,
        b: 102,
    },
    Rgb {
        r: 40,
        g: 42,
        b: 54,
    },
];
/// A storm's base, near black.
const STORM_BASE: Rgb = Rgb {
    r: 22,
    g: 20,
    b: 30,
};
/// How far a storm's shade sinks toward [`STORM_BASE`].
const STORM_BASE_SINK: f32 = 0.6;
/// A deck at least this heavy is a heavy one: its far masses fill in to their
/// body, and a night's light under it is diffuse.
const HEAVY_DECK: f32 = 0.6;
/// The heaviness past which a day's light under the deck is diffuse.
const DIFFUSE_DAY: f32 = 0.35;
/// The share of night past which the light is night's, and of day short of
/// which it is no longer day's.
const LIGHT_SPLIT: f32 = 0.5;
/// How much of a storm's precipitation hangs as virga, against rain's whole.
const STORM_VIRGA: f32 = 0.8;
/// The city's warm light on a night base: one warm-dark step, no more.
const CITY_GLOW: Rgb = Rgb {
    r: 104,
    g: 74,
    b: 62,
};
const CITY_GLOW_STEP: f32 = 0.16;
/// The glow takes a shade past this much night, in patches this many units
/// across, over this share of them.
const GLOW_NIGHT: f32 = 0.5;
const GLOW_GRAIN: f32 = 9.0;
const GLOW_SHARE: f32 = 0.5;
/// What a strike's light tints a deck toward, ring by ring.
const FLASH_TINT: Rgb = Rgb {
    r: 214,
    g: 220,
    b: 246,
};
const FLASH_RINGS: [f32; 3] = [0.0, 0.22, 0.42];
/// How much of its first ring a strike's faint edge lifts.
const DIM_SHARE: f32 = 0.6;
/// The bolt's near-white core.
const BOLT_CORE: Rgb = Rgb {
    r: 244,
    g: 244,
    b: 255,
};
/// At 1x one cell is a 4x's sixteen: a dimmer core keeps the bolt far away.
const BOLT_1X: f32 = 0.6;

impl Lighting {
    /// Day, sunset and night mixed by their weights.
    fn mixed(day: f32, sunset: f32, night: f32) -> Lighting {
        let total = (day + sunset + night).max(f32::EPSILON);
        let (sw, nw) = (sunset / total, night / total);
        let lerp = |d: f32, s: f32, n: f32| d + (s - d) * sw + (n - d) * nw;
        let tone = |i: usize| {
            let ds = DAY.tones[i].mix(SUNSET.tones[i], sw / (1.0 - nw).max(f32::EPSILON));
            ds.mix(NIGHT.tones[i], nw)
        };
        Lighting {
            dir: (
                lerp(DAY.dir.0, SUNSET.dir.0, NIGHT.dir.0),
                lerp(DAY.dir.1, SUNSET.dir.1, NIGHT.dir.1),
            ),
            z: lerp(DAY.z, SUNSET.z, NIGHT.z),
            rim: lerp(DAY.rim, SUNSET.rim, NIGHT.rim),
            occlude: lerp(DAY.occlude, SUNSET.occlude, NIGHT.occlude),
            tones: [tone(0), tone(1), tone(2)],
        }
    }
}

/// A strike: where its light gathers, and its bolt.
#[derive(Debug, Clone, PartialEq)]
struct Strike {
    /// The flash's centre, in units.
    at: (f32, f32),
    radius: f32,
    /// The mass it lights from inside, or `None` for a bolt's base.
    intra: Option<usize>,
    /// The bolt's polylines, trunk first, in units.
    bolt: Vec<Vec<(f32, f32)>>,
    /// How far up its ramp of rings the flash reaches this frame.
    step: u8,
    /// A strike's faint edge: only its brightest ring lifts, and less.
    dim: bool,
}

/// One mass's bands on a grid `d` cells to the unit, in its own undrifted
/// frame: what a frame translates by its drift and composites, and what the
/// office's [`CloudCache`] keeps.
// Eq: an `Arc` of an `Eq` type compares equal by pointer first, and the
// cache hands one mass's raster out as one `Arc`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MassRaster {
    /// Its first column and row, in grid cells from the run's west end and
    /// the glass's top, undrifted.
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    bands: Vec<Option<Band>>,
}

impl MassRaster {
    #[cfg(test)]
    fn at(&self, col: i32, row: i32) -> Option<Band> {
        let (x, y) = (
            usize::try_from(col - self.x0).ok()?,
            usize::try_from(row - self.y0).ok()?,
        );
        (x < self.w && y < self.h)
            .then(|| self.bands[y * self.w + x])
            .flatten()
    }

    /// Lays its bands, as mass `m`'s, over `px`, a grid `cols` wide whose
    /// first column is its own column `west`, covering what is there.
    fn lay(&self, m: usize, west: i32, px: &mut [Option<(Band, usize)>], cols: usize) {
        let rows = px.len() / cols.max(1);
        // its cells `0..n` that land in `0..end` when shifted by `from`
        let clip = |from: i32, n: usize, end: usize| {
            let (from, n, end) = (i64::from(from), n as i64, end as i64);
            let (lo, hi) = ((-from).clamp(0, n), (end - from).clamp(0, n));
            lo as usize..hi.max(lo) as usize
        };
        let (xs, ys) = (
            clip(self.x0 - west, self.w, cols),
            clip(self.y0, self.h, rows),
        );
        if xs.is_empty() {
            return;
        }
        for y in ys {
            let row = (self.y0 + y as i32) as usize * cols;
            let src = &self.bands[y * self.w..][xs.clone()];
            let col0 = (self.x0 - west + xs.start as i32) as usize;
            for (dst, band) in px[row + col0..].iter_mut().zip(src) {
                if let Some(b) = band {
                    *dst = Some((*b, m));
                }
            }
        }
    }
}

/// What a [`MassRaster`] is drawn from: one mass of one weather's deck over
/// one office's glass, at one density, grown to one share, under one light.
/// Every input it reads is here, quantized, so a cached raster is the one an
/// uncached frame would draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RasterKey {
    weather: Weather,
    id: usize,
    span: u16,
    glass_h: u16,
    d: Density,
    share: u8,
    light: LightKey,
}

/// The light a mass's bands are cut under, in [`LIGHT_STEPS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LightKey {
    night: u8,
    sunset: u8,
    heaviness: u8,
    storm: u8,
}

/// Each light input's steps: the deck's bands change with the hour and the
/// weather at most this often.
const LIGHT_STEPS: f32 = 16.0;
/// A weather's share's steps: a mass grows in or out in this many.
const SHARE_STEPS: f32 = 32.0;

fn quantize(v: f32, steps: f32) -> u8 {
    (v.clamp(0.0, 1.0) * steps).round() as u8
}

fn dequantize(k: u8, steps: f32) -> f32 {
    f32::from(k) / steps
}

/// How far ahead [`Clouds::draw_ahead`] looks for a step: a step's every mass,
/// [`DRAWS_AHEAD`] a frame, is drawn before it lands.
const AHEAD: std::time::Duration = std::time::Duration::from_secs(1);
/// The masses a frame draws ahead at most: a transition's share and light
/// steps can land a second apart, each a deck's masses.
const DRAWS_AHEAD: usize = 2;

/// The office's mass rasters, the most recently used first and at most
/// `CloudCache::CAPACITY` of them: drawing a mass's bands is most of a
/// cloudy frame's cost, and a drifting mass's bands don't change.
#[derive(Debug)]
pub(crate) struct CloudCache {
    entries: lru::LruCache<RasterKey, std::sync::Arc<MassRaster>>,
    /// The next frame draws all its [`AHEAD`] needs, not [`DRAWS_AHEAD`]:
    /// a boot frame, which no frame before it drew ahead for.
    warming: bool,
}

// `LruCache` has no `Default`: its capacity is given.
impl Default for CloudCache {
    fn default() -> Self {
        Self {
            entries: lru::LruCache::new(Self::CAPACITY),
            warming: false,
        }
    }
}

impl CloudCache {
    /// Room for a transition's two decks on both looks' grids, and every
    /// mass drawn [`AHEAD`] of them (`the_cache_holds_a_transition_in_both_looks`):
    /// one office draws one wall per look.
    const CAPACITY: std::num::NonZeroUsize = std::num::NonZeroUsize::new(256).expect("nonzero");

    /// Have the next frame draw every mass its [`AHEAD`] needs.
    pub(crate) fn warm(&mut self) {
        self.warming = true;
    }

    fn holds(&self, key: RasterKey) -> bool {
        self.entries.contains(&key)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    fn get_or_draw(
        &mut self,
        key: RasterKey,
        draw: impl FnOnce() -> MassRaster,
    ) -> std::sync::Arc<MassRaster> {
        let raster = self.entries.get_or_insert(key, || {
            std::sync::Arc::new(tracing::trace_span!("clouds.draw").in_scope(draw))
        });
        std::sync::Arc::clone(raster)
    }
}

/// What the windows' clouds are this frame.
#[derive(PartialEq)]
pub(crate) struct Clouds {
    masses: Vec<Mass>,
    /// Each mass's bands, by index.
    rasters: Vec<std::sync::Arc<MassRaster>>,
    /// The grid the rasters are drawn on, cells to the unit.
    d: Density,
    lighting: Lighting,
    /// Each weather's tones under this light.
    tones: Vec<(Weather, [Rgb; 3])>,
    diffuse: bool,
    heaviness: f32,
    /// The storm's share: its base shades deeper.
    storm: f32,
    night: f32,
    rain: f32,
    strike: Option<Strike>,
}

impl Clouds {
    /// What every mass of `sky`'s clouds is drawn under, quantized: two skies
    /// with one draw the same bands.
    fn keyed(sky: &Sky) -> (LightKey, Vec<(Weather, u8)>) {
        let weather = sky.weather();
        let body = sky.body();
        let night = match body.kind {
            crate::sky::BodyKind::Sun => 1.0 - ease(body.altitude / FULL_DAY_ALTITUDE),
            crate::sky::BodyKind::Moon => 1.0,
        };
        let light = LightKey {
            night: quantize(night, LIGHT_STEPS),
            sunset: quantize(crate::atmosphere::golden_hour(sky), LIGHT_STEPS),
            heaviness: quantize(weather.lerp(Element::Cloud, heaviness), LIGHT_STEPS),
            storm: quantize(weather.share(Element::Cloud, Weather::Storm), LIGHT_STEPS),
        };
        let shares = weather
            .parts(Element::Cloud)
            .map(|(w, share)| (w, quantize(ease(share), SHARE_STEPS)))
            .collect();
        (light, shares)
    }

    /// The clouds of `sky`, `secs` into their drift, their masses not yet
    /// drawn, and each mass with the key its bands are drawn under.
    fn plan(
        sky: &Sky,
        secs: f64,
        (span, glass_h): (u16, u16),
        d: Density,
    ) -> (Self, Vec<(Mass, RasterKey)>) {
        let (span_f, glass_h_f) = (f32::from(span), f32::from(glass_h));
        let weather = sky.weather();
        let (light, shares) = Self::keyed(sky);
        let night = dequantize(light.night, LIGHT_STEPS);
        let sunset = dequantize(light.sunset, LIGHT_STEPS);
        let day = (1.0 - night - sunset).max(0.0);
        let lighting = Lighting::mixed(day, sunset, night);
        let heavy = dequantize(light.heaviness, LIGHT_STEPS);
        let tones = weather
            .parts(Element::Cloud)
            .map(|(w, _)| {
                let k = heaviness(w);
                let mut t = [0, 1, 2].map(|i| lighting.tones[i].mix(STORM_GREY[i], k));
                if w == Weather::Storm {
                    t[2] = t[2].mix(STORM_BASE, STORM_BASE_SINK);
                }
                (w, t)
            })
            .collect();
        let stepped = |w| {
            dequantize(
                quantize(weather.share(Element::Precipitation, w), SHARE_STEPS),
                SHARE_STEPS,
            )
        };
        let clouds = Self {
            masses: Vec::new(),
            rasters: Vec::new(),
            d,
            lighting,
            tones,
            diffuse: heavy
                >= if night > LIGHT_SPLIT {
                    HEAVY_DECK
                } else {
                    DIFFUSE_DAY
                }
                && day < LIGHT_SPLIT,
            heaviness: heavy,
            storm: dequantize(light.storm, LIGHT_STEPS),
            night,
            // Each share in the deck's own steps, so its shafts step with its masses.
            rain: stepped(Weather::Rain) + STORM_VIRGA * stepped(Weather::Storm),
            strike: None,
        };
        let mut planned = Vec::new();
        for (w, share) in shares {
            for m in deck(w, span_f, glass_h_f) {
                let mass = m.grown(dequantize(share, SHARE_STEPS), m.drift_at(secs, span_f));
                let key = RasterKey {
                    weather: w,
                    id: m.id,
                    span,
                    glass_h,
                    d,
                    share,
                    light,
                };
                planned.push((mass, key));
            }
        }
        (clouds, planned)
    }

    /// The clouds of `outlook` over a window run `span` units wide and glass
    /// `glass_h` tall, drawn on a grid `d` to the unit; `cache` keeps the
    /// masses' bands across frames, and an empty one draws them afresh.
    pub(crate) fn of(
        outlook: &crate::atmosphere::Outlook,
        (span, glass_h): (u16, u16),
        d: Density,
        panes: &[Range<u16>],
        cache: &mut CloudCache,
    ) -> Self {
        let (span_f, glass_h_f) = (f32::from(span), f32::from(glass_h));
        // f64: an epoch-scale beat in f32 steps in minutes, freezing then jumping the deck
        let secs = outlook.beat().ms() as f64 / 1000.0;
        let (mut clouds, planned) = Self::plan(outlook.sky(), secs, (span, glass_h), d);
        let mut drawn: Vec<_> = planned
            .into_iter()
            .map(|(mass, key)| {
                let raster = cache.get_or_draw(key, || clouds.draw(&mass, glass_h_f));
                (mass, raster)
            })
            .collect();
        // far to near, each mass beside its own bands: the nearest wins
        drawn.sort_by_key(|(m, _)| m.layer);
        (clouds.masses, clouds.rasters) = drawn.into_iter().unzip();
        if let Some(phase) = outlook.sky().strike() {
            let beat = outlook.beat();
            clouds.strike = clouds.strike_at(
                crate::sky::strike_bucket(beat),
                phase,
                (span_f, glass_h_f),
                crate::sky::strike_start_ms(beat) as f64 / 1000.0,
                panes,
            );
        }
        clouds
    }

    /// Draw up to [`DRAWS_AHEAD`] of the masses the [`AHEAD`] of `moment`
    /// needs and `cache` lacks, the soonest needed first: each quantized
    /// step's bands drawn a few a frame before it lands, not all in the frame
    /// it does. Its budget is a frame's, so every painted frame calls it.
    pub(crate) fn draw_ahead(
        moment: &Moment,
        size: (u16, u16),
        d: Density,
        cache: &mut CloudCache,
    ) {
        let _ahead = tracing::trace_span!("clouds.ahead").entered();
        let frame = std::time::Duration::from_millis(crate::anim::PAINT_FRAME_MS);
        let mut budget = if std::mem::take(&mut cache.warming) {
            usize::MAX
        } else {
            DRAWS_AHEAD
        };
        let mut was = Self::keyed(&moment.sky);
        for k in 1..=AHEAD.as_millis() / frame.as_millis() {
            let sky = Sky::at(moment.timing.later(frame * k as u32), moment.sky.policy());
            let keyed = Self::keyed(&sky);
            if keyed == was {
                continue;
            }
            was = keyed;
            let (next, planned) = Self::plan(&sky, 0.0, size, d);
            for (mass, key) in planned {
                if budget == 0 {
                    return;
                }
                if !cache.holds(key) {
                    cache.get_or_draw(key, || next.draw(&mass, f32::from(size.1)));
                    budget -= 1;
                }
            }
        }
    }

    /// Mass `m`'s bands over glass `glass_h` tall on its grid, in its own
    /// undrifted frame, closed: a notch narrower than [`CLOSE`] fills.
    fn draw(&self, m: &Mass, glass_h: f32) -> MassRaster {
        let df = f32::from(self.d.get());
        let k = ((CLOSE * df).round() as i32).max(1);
        let (west, east) = m.extent();
        let x0 = ((west - SLIT) * df).floor() as i32 - k;
        let x1 = ((east + SLIT) * df).ceil() as i32 + k;
        let y0 = (m.top().min(0.0) * df).floor() as i32 - k;
        let y1 = (glass_h * df).ceil() as i32;
        let (w, h) = (
            usize::try_from(x1 - x0).unwrap_or(0),
            usize::try_from(y1 - y0).unwrap_or(0),
        );
        let unit = |i: usize| {
            (
                ((x0 + (i % w.max(1)) as i32) as f32 + 0.5) / df,
                ((y0 + (i / w.max(1)) as i32) as f32 + 0.5) / df,
            )
        };
        let raw: Vec<bool> = (0..w * h)
            .map(|i| {
                let (x, y) = unit(i);
                m.shows() && m.inside(x, y)
            })
            .collect();
        let k = k as usize;
        let solid = erode(&dilate(&raw, w, h, k), w, h, k);
        let bands = (0..w * h)
            .map(|i| {
                solid[i].then(|| {
                    let (x, y) = unit(i);
                    let band = self.band(m, x, y);
                    if m.layer == Layer::Far && self.heaviness >= HEAVY_DECK {
                        band.max(Band::Body)
                    } else {
                        band
                    }
                })
            })
            .collect();
        MassRaster {
            x0,
            y0,
            w,
            h,
            bands,
        }
    }

    /// Each mass's drift this frame, in whole cells of its grid.
    #[cfg(test)]
    fn cell_drifts(&self) -> Vec<i32> {
        self.masses.iter().map(|m| self.cell_drift(m)).collect()
    }

    /// `m`'s drift this frame in whole cells of its grid: where it is drawn.
    fn cell_drift(&self, m: &Mass) -> i32 {
        (m.off * f32::from(self.d.get())).round() as i32
    }

    /// The nearest mass's band at each cell of a grid `cols` by `rows` whose
    /// first column is the run's `west`, each mass at its drift, and which
    /// mass: the far laid first, so the nearest covers.
    fn bands(&self, west: i32, (cols, rows): (usize, usize)) -> Vec<Option<(Band, usize)>> {
        let mut px = vec![None; cols * rows];
        for (m, (mass, raster)) in self.masses.iter().zip(&self.rasters).enumerate() {
            raster.lay(m, west - self.cell_drift(mass), &mut px, cols);
        }
        px
    }

    /// The nearest mass's band at `(col, row)` cells from the run's west end
    /// and the glass's top, each mass drifted by its `drift`, and which mass.
    #[cfg(test)]
    fn band_at(&self, drift: &[i32], col: i32, row: i32) -> Option<(Band, usize)> {
        (0..self.masses.len())
            .rev()
            .find_map(|m| self.rasters[m].at(col - drift[m], row).map(|b| (b, m)))
    }

    /// The nearest mass with a lobe within [`BOLT_LOBE_SHARE`] of its radius over
    /// `x`, each mass drifted by its `offs`.
    fn nearest_over(&self, x: f32, offs: &[f32]) -> Option<usize> {
        (0..self.masses.len()).rev().find(|&i| {
            self.masses[i]
                .lobes
                .iter()
                .any(|l| l.r > 0.0 && (x - offs[i] - l.x).abs() < l.r * BOLT_LOBE_SHARE)
        })
    }

    /// Bucket `bucket`'s strike in `phase` over a run `(span, glass_h)`, its
    /// cloud the one over a column of one of `panes` as the masses stood
    /// `start` seconds into the beat, when it struck: so neither the drift
    /// nor the joinery hides it partway through.
    fn strike_at(
        &self,
        bucket: u64,
        phase: StrikePhase,
        (span, glass_h): (f32, f32),
        start: f64,
        panes: &[Range<u16>],
    ) -> Option<Strike> {
        let mut r = Rng::new(bucket.wrapping_mul(31).wrapping_add(7));
        let pane = panes.get((r.u() * panes.len() as f32) as usize)?;
        let (west, east) = (f32::from(pane.start), f32::from(pane.end));
        let x = r.between(west + BOLT_PANE_MARGIN, (east - BOLT_PANE_MARGIN).max(west));
        let offs: Vec<f32> = self
            .masses
            .iter()
            .map(|m| m.drift_at(start, span))
            .collect();
        let i = self.nearest_over(x, &offs)?;
        let base = self.masses[i].base_at(x - offs[i]);
        let intra = r.u() < INTRA_SHARE;
        let step = match phase {
            StrikePhase::Primary => 2,
            StrikePhase::After => 1,
            StrikePhase::Dim => 0,
        };
        Some(Strike {
            at: (x, base - if intra { INTRA_DEPTH } else { BOLT_FLASH_DEPTH }),
            radius: if intra {
                INTRA_RADIUS
            } else {
                BOLT_FLASH_RADIUS
            },
            intra: intra.then_some(i),
            bolt: if intra {
                Vec::new()
            } else {
                bolt(&mut r, (x, base), span, glass_h)
            },
            step,
            dim: phase == StrikePhase::Dim,
        })
    }

    /// The clouds on `view`'s glass, its run starting `run_x0` units in,
    /// behind every cell `front` stands something in.
    pub(crate) fn paint(
        &self,
        view: &mut WindowView,
        run_x0: u16,
        front: impl Fn(Cell) -> Option<Rgb>,
    ) {
        if self.masses.is_empty() {
            return;
        }
        let d = self.d.get();
        debug_assert_eq!(view.d().get(), d, "the clouds were drawn for another grid");
        let df = f32::from(d);
        let unit = |cell: Cell| {
            (
                (f32::from(cell.at.0) + 0.5) / df - f32::from(run_x0),
                (f32::from(cell.glass_offset.1) + 0.5) / df,
            )
        };
        let size = view.glass_size();
        let (cols, rows) = (usize::from(size.w * d), usize::from(size.h * d));
        if cols == 0 || rows == 0 {
            return;
        }
        // the glass's west edge's column on the run's grid
        let west = i32::from(view.glass_origin().0) - i32::from(run_x0 * d);
        let mut px = self.bands(west, (cols, rows));
        if d == 1 {
            despeckle(&mut px, cols, rows);
        } else {
            close_thin_runs(&mut px, cols, rows, usize::from(d));
        }
        // only a row a mass covers mixes its sky in
        let clouded: Vec<bool> = px
            .chunks(cols)
            .map(|row| row.iter().any(Option::is_some))
            .collect();
        let mut row_sky = vec![(0u32, [0u32; 3]); rows];
        view.paint(|cell, c| {
            let gy = usize::from(cell.glass_offset.1);
            // The window resolved no sky behind `front`, so its cells are
            // neither averaged into a row's sky nor clouded.
            if !clouded[gy] || front(cell).is_some() {
                return c;
            }
            let acc = &mut row_sky[gy];
            acc.0 += 1;
            acc.1[0] += u32::from(c.r);
            acc.1[1] += u32::from(c.g);
            acc.1[2] += u32::from(c.b);
            c
        });
        let row_sky: Vec<Option<Rgb>> = row_sky
            .iter()
            .map(|&(n, s)| {
                (n > 0).then(|| Rgb {
                    r: (s[0] / n) as u8,
                    g: (s[1] / n) as u8,
                    b: (s[2] / n) as u8,
                })
            })
            .collect();
        let bolt = self.bolt_cells(run_x0);
        // no rain to shaft and no bolt: a cell no mass covers keeps its sky
        let bare = self.rain <= 0.0 && bolt.is_empty();
        // a run of cells mixes one tone into one row's sky: mix it once
        let mut last_mix: Option<((Rgb, Rgb, Layer), Rgb)> = None;
        view.paint(|cell, c| {
            let (gx, gy) = (
                usize::from(cell.glass_offset.0),
                usize::from(cell.glass_offset.1),
            );
            let here = px[gy * cols + gx];
            if (bare && here.is_none()) || front(cell).is_some() {
                return c;
            }
            let (x, y) = unit(cell);
            let mut c = match here {
                Some((band, m)) => {
                    let mass = &self.masses[m];
                    let mut tone = self.tone(mass.weather, band);
                    if band == Band::Shade
                        && self.night > GLOW_NIGHT
                        && noise(GLOW_SEED, x / GLOW_GRAIN) > GLOW_SHARE
                    {
                        tone = tone.mix(CITY_GLOW, CITY_GLOW_STEP);
                    }
                    if let Some(lift) = self.flash_lift(m, x, y) {
                        tone = tone.mix(FLASH_TINT, lift);
                    }
                    let sky = row_sky[gy].unwrap_or(c);
                    let key = (tone, sky, mass.layer);
                    match last_mix {
                        Some((k, mixed)) if k == key => mixed,
                        _ => {
                            let mixed = tone.mix(sky, mass.layer.sky_mix());
                            last_mix = Some((key, mixed));
                            mixed
                        }
                    }
                }
                None => self.virga(cell, (x, y), c),
            };
            if bolt.contains(&(cell.at.0, cell.glass_offset.1)) {
                c = c.mix(BOLT_CORE, if d > 1 { 1.0 } else { BOLT_1X });
            }
            c
        });
    }

    /// Where the strike's bolt starts, in units from the run's west end and
    /// the glass's top; `None` without one.
    #[cfg(test)]
    pub(crate) fn bolt_top(&self) -> Option<(f32, f32)> {
        self.strike.as_ref()?.bolt.first()?.first().copied()
    }

    /// The strike's bolt alone: its flash at the first ring, which lifts
    /// nothing; with `bolt` false, no strike at all.
    #[cfg(test)]
    pub(crate) fn only_bolt(&mut self, bolt: bool) {
        if bolt {
            if let Some(s) = &mut self.strike {
                s.step = 0;
            }
        } else {
            self.strike = None;
        }
    }

    fn tone(&self, weather: Weather, band: Band) -> Rgb {
        let t = self
            .tones
            .iter()
            .find(|(w, _)| *w == weather)
            .map_or(self.lighting.tones, |(_, t)| *t);
        t[band as usize]
    }

    /// The band the light leaves `(x, y)` of `m` in: the surface's normal
    /// against the light, the mass shading itself toward its base.
    fn band(&self, m: &Mass, x: f32, y: f32) -> Band {
        const E: f32 = 0.25;
        let hc = m.height(x, y);
        // a sample off every lobe (a closed slit, the fill) is level with
        // here: no cliff at its edge
        let at = |sx: f32, sy: f32| {
            let h = m.height(sx, sy);
            if h > 0.0 { h } else { hc }
        };
        let gx = (at(x + E, y) - at(x - E, y)) / (2.0 * E);
        let gy = (at(x, y + E) - at(x, y - E)) / (2.0 * E);
        let (nx, ny, nz) = (-gx, -gy, RELIEF);
        let nn = (nx * nx + ny * ny + nz * nz).sqrt();
        let Lighting {
            dir: (mut lx, mut ly),
            z: mut lz,
            rim,
            occlude,
            ..
        } = self.lighting;
        if self.diffuse {
            // a heavy deck hides the sky body: the light is diffuse, from above
            (lx, ly, lz) = (lx * DIFFUSE_SIDE, DIFFUSE_FROM.0, DIFFUSE_FROM.1);
        }
        let ln = (lx * lx + ly * ly + lz * lz).sqrt();
        let mut v = (nx * lx + ny * ly + nz * lz) / (nn * ln);
        let t = (m.base_at(x) - y) / (m.base - m.top()).max(1.0);
        // how far up the base's shadow climbs, by the mass's own column: two
        // octaves, never a ruled line
        let col = x - m.anchor;
        let climb = SHADE_CLIMB
            + SHADE_CLIMB_OCTAVES
                .iter()
                .map(|&(amp, freq, phase)| amp * noise(SHADE_SEED, col * freq + phase))
                .sum::<f32>();
        v -= (occlude + STORM_OCCLUDE * self.storm) * (1.0 - t / climb).max(0.0);
        let cuts = (
            LIT_CUT.0 + LIT_CUT.1 * self.heaviness + rim,
            BODY_CUT.0 + BODY_CUT.1 * self.heaviness,
        );
        let mut band = if v > cuts.0 {
            Band::Lit
        } else if v > cuts.1 {
            Band::Body
        } else {
            Band::Shade
        };
        if band == Band::Shade && t >= climb {
            // shade is the base's alone: a flank turned away stays body
            band = Band::Body;
        }
        if band == Band::Lit && rim > 0.01 && !self.diffuse {
            // a rim: only within its depth of the edge the light falls on
            let l = (lx * lx + ly * ly).sqrt().max(f32::EPSILON);
            if m.inside(x + lx / l * RIM_DEPTH, y + ly / l * RIM_DEPTH) {
                band = Band::Body;
            }
        }
        band
    }

    /// How far a strike lifts mass `m` at `(x, y)` toward its flash: by rings
    /// around its light, the rings' edges ragged.
    fn flash_lift(&self, m: usize, x: f32, y: f32) -> Option<f32> {
        let s = self.strike.as_ref()?;
        if s.intra.is_some_and(|i| i != m) {
            return None;
        }
        let ry = s.radius
            * if s.intra.is_some() {
                INTRA_ASPECT
            } else {
                BOLT_FLASH_ASPECT
            };
        let mut dist = ((x - s.at.0) / s.radius).hypot((y - s.at.1) / ry);
        let (fx, fy) = RING_GRAIN;
        dist += RING_RAGGED * (noise(RING_SEED, x * fx + y * fy) - 0.5);
        let ring = if dist < INNER_RING {
            2
        } else {
            u8::from(dist < 1.0)
        };
        let lift = if s.dim {
            if ring == 2 {
                FLASH_RINGS[1] * DIM_SHARE
            } else {
                0.0
            }
        } else {
            FLASH_RINGS[usize::from(ring.min(s.step))]
        };
        (lift > 0.0).then_some(lift)
    }

    /// Faint shafts of rain under the darkest masses, by the rain's share.
    fn virga(&self, cell: Cell, (x, y): (f32, f32), c: Rgb) -> Rgb {
        if self.rain <= 0.0 {
            return c;
        }
        let (gx, gy) = (u32::from(cell.at.0), u32::from(cell.at.1));
        // sparse, slanted streaks, half their cells; tested before the mass
        // search, which scans every lobe
        let streak = (gx + gy / 2) % (VIRGA_PITCH * u32::from(self.d.get())) == 0;
        if !(streak && gx % 2 == gy % 2) {
            return c;
        }
        let Some(m) = self.virga_mass(x, y) else {
            return c;
        };
        let fade = 1.0 - (y - m.base) / VIRGA_DEPTH;
        c.mix(
            self.tone(m.weather, Band::Shade),
            VIRGA_STRENGTH * self.rain.min(1.0) * fade,
        )
    }

    /// The mid or near mass whose virga falls at `(x, y)` units: under its
    /// base, within [`VIRGA_DEPTH`], near a lobe; the nearest such, at its
    /// drawn drift, as [`bands`](Self::bands) lays the masses.
    fn virga_mass(&self, x: f32, y: f32) -> Option<&Mass> {
        self.masses.iter().rev().find(|m| {
            let off = self.cell_drift(m) as f32 / f32::from(self.d.get());
            m.layer >= Layer::Mid
                && m.base < y
                && y <= m.base + VIRGA_DEPTH
                && m.lobes
                    .iter()
                    .any(|l| (x - off - l.x).abs() < l.r * VIRGA_LOBE_SHARE)
        })
    }

    /// The cells the bolt's core covers on its grid, as each cell's grid
    /// column and glass row.
    fn bolt_cells(&self, run_x0: u16) -> std::collections::HashSet<(u16, u16)> {
        let mut out = std::collections::HashSet::new();
        let Some(s) = &self.strike else { return out };
        let d = self.d.get();
        let df = f32::from(d);
        for (li, line) in s.bolt.iter().enumerate() {
            // 1 cell wide on every grid; at 4x the trunk's top few segments 2
            let parts: Vec<(&[(f32, f32)], u16)> = if d > 1 && li == 0 {
                vec![
                    (&line[..line.len().min(4)], 2),
                    (&line[line.len().min(3)..], 1),
                ]
            } else {
                vec![(&line[..], 1)]
            };
            for (seg, width) in parts {
                for pair in seg.windows(2) {
                    let (ax, ay) = ((pair[0].0 + f32::from(run_x0)) * df, pair[0].1 * df);
                    let (bx, by) = ((pair[1].0 + f32::from(run_x0)) * df, pair[1].1 * df);
                    let n = (bx - ax).abs().max((by - ay).abs()) as u32 + 1;
                    for i in 0..=n {
                        let t = i as f32 / n as f32;
                        let (x, y) = (ax + (bx - ax) * t, ay + (by - ay) * t);
                        for o in 0..width {
                            out.insert((x as u16 + o, y as u16));
                        }
                    }
                }
            }
        }
        out
    }
}

/// How far in from the edge it faces a low body's rim light reaches, in units.
const RIM_DEPTH: f32 = 1.5;
/// How much deeper a storm's base shades.
const STORM_OCCLUDE: f32 = 0.4;
/// How far under a base its rain shafts fall.
const VIRGA_DEPTH: f32 = 4.5;
/// The units between two rain shafts.
const VIRGA_PITCH: u32 = 3;

/// How far out along a lobe's radius a strike may fall: under a mass's body,
/// where its drawn bottom reaches its base, never its fringe, which curls up.
const BOLT_LOBE_SHARE: f32 = 0.5;

/// How far in from a pane's edges, in units, a strike may fall.
const BOLT_PANE_MARGIN: f32 = 1.0;
/// The share of strikes that light their cloud from inside, with no bolt.
const INTRA_SHARE: f32 = 0.4;
/// How far over its cloud's base an intra-cloud flash centres, its radius,
/// and its rings' height to width.
const INTRA_DEPTH: f32 = 3.0;
const INTRA_RADIUS: f32 = 3.5;
const INTRA_ASPECT: f32 = 0.6;
/// The same of a bolt's flash, round the bolt's top.
const BOLT_FLASH_DEPTH: f32 = 0.8;
const BOLT_FLASH_RADIUS: f32 = 8.0;
const BOLT_FLASH_ASPECT: f32 = 0.5;
/// How ragged a flash's rings run, by noise of this grain across `(x, y)`.
const RING_RAGGED: f32 = 0.3;
const RING_GRAIN: (f32, f32) = (4.3, 7.1);
/// The brightest ring's reach, a share of the flash's radius.
const INNER_RING: f32 = 0.55;
/// A bolt's trunk: its length a share of the glass, each step down, each
/// sway aside, the share of sways that follow its lean, and the units it
/// keeps from the run's west and east ends.
const BOLT_DEPTH: (f32, f32) = (0.25, 0.40);
const BOLT_STEP: (f32, f32) = (0.7, 1.3);
const BOLT_SWAY: (f32, f32) = (0.15, 0.65);
const BOLT_WITH_LEAN: f32 = 0.7;
const BOLT_INSET: (f32, f32) = (1.5, 2.5);
/// Its fork: at least the first segments, up to the second more, each step
/// down and sway back against the lean.
const FORK_SEGMENTS: (usize, usize) = (2, 2);
const FORK_STEP: (f32, f32) = (0.6, 1.1);
const FORK_SWAY: (f32, f32) = (0.4, 0.9);
/// A heavy deck's diffuse light: its sideways share of the body's, and the
/// `(y, z)` it comes from.
const DIFFUSE_SIDE: f32 = 0.3;
const DIFFUSE_FROM: (f32, f32) = (-1.0, 0.8);
/// How far up a base its shade climbs, a share of the mass's height, with
/// two octaves `(amplitude, frequency, phase)` of the mass's own column.
const SHADE_CLIMB: f32 = 0.12;
const SHADE_CLIMB_OCTAVES: [(f32, f32, f32); 2] = [(0.22, 2.3, 40.0), (0.14, 6.1, 13.0)];
/// The light above which a cell is lit, and above which it is body: each at
/// no heaviness, and more per unit of it.
const LIT_CUT: (f32, f32) = (0.55, 0.12);
const BODY_CUT: (f32, f32) = (0.05, 0.1);
/// Virga falls under a lobe within this share of its radius, at most this
/// far toward the cloud's shade.
const VIRGA_LOBE_SHARE: f32 = 0.7;
const VIRGA_STRENGTH: f32 = 0.35;

/// The sun's altitude, from the horizon's 0, above which its clouds wear
/// their full day tones.
const FULL_DAY_ALTITUDE: f32 = 0.3;

/// Where a mass whose west edge began at `x0` has drifted after `travelled`
/// units east over a run `span` wide, its deck's widest mass `widest` wide: it
/// wraps a period the run plus two of the widest long, so it leaves the run's
/// east end whole before it comes back round its west.
fn drifted_west(x0: f32, travelled: f64, span: f32, widest: f32) -> f32 {
    let period = f64::from(span + 2.0 * widest);
    ((f64::from(x0 + widest) + travelled).rem_euclid(period) as f32) - widest
}

/// A share eased in and out: a mass forms slowly, then fills, then settles.
fn ease(share: f32) -> f32 {
    crate::anim::Easing::Smoothstep.apply(share)
}

/// A strike's bolt from `(x, base)`: a zig-zag trunk a quarter to two-fifths
/// of the glass long, and one fork.
fn bolt(r: &mut Rng, (mut x, base): (f32, f32), span: f32, glass_h: f32) -> Vec<Vec<(f32, f32)>> {
    let mut y = base;
    let depth = y + glass_h * r.between(BOLT_DEPTH.0, BOLT_DEPTH.1);
    let mut trunk = vec![(x, y)];
    let lean = if r.u() < 0.5 { 1.0 } else { -1.0 };
    while y < depth {
        y += r.between(BOLT_STEP.0, BOLT_STEP.1);
        let with = if r.u() < BOLT_WITH_LEAN { 1.0 } else { -1.0 };
        x += lean * r.between(BOLT_SWAY.0, BOLT_SWAY.1) * with;
        x = x.clamp(BOLT_INSET.0, span - BOLT_INSET.1);
        trunk.push((x, y));
    }
    let k = 1 + (r.u() * (trunk.len().saturating_sub(3)).max(1) as f32) as usize;
    let (mut fx, mut fy) = trunk[k.min(trunk.len() - 1)];
    let mut fork = vec![(fx, fy)];
    for _ in 0..FORK_SEGMENTS.0 + (r.u() * FORK_SEGMENTS.1 as f32) as usize {
        fy += r.between(FORK_STEP.0, FORK_STEP.1);
        fx = (fx - lean * r.between(FORK_SWAY.0, FORK_SWAY.1))
            .clamp(BOLT_INSET.0, span - BOLT_INSET.1);
        fork.push((fx, fy));
    }
    vec![trunk, fork]
}

/// A square grow by `k` cells.
fn dilate(m: &[bool], cols: usize, rows: usize, k: usize) -> Vec<bool> {
    morph(m, cols, rows, k, true)
}

/// A square shrink by `k` cells.
fn erode(m: &[bool], cols: usize, rows: usize, k: usize) -> Vec<bool> {
    morph(m, cols, rows, k, false)
}

fn morph(m: &[bool], cols: usize, rows: usize, k: usize, grow: bool) -> Vec<bool> {
    let pass = |src: &[bool], horizontal: bool| -> Vec<bool> {
        (0..rows * cols)
            .map(|i| {
                let (x, y) = (i % cols, i / cols);
                let (along, len) = if horizontal { (x, cols) } else { (y, rows) };
                let at = |j: usize| {
                    if horizontal {
                        y * cols + j
                    } else {
                        j * cols + x
                    }
                };
                let mut window =
                    (along.saturating_sub(k)..=(along + k).min(len - 1)).map(|j| src[at(j)]);
                if grow {
                    window.any(|b| b)
                } else {
                    window.all(|b| b)
                }
            })
            .collect()
    };
    pass(&pass(m, true), false)
}

/// At 1x a lone cell is noise, not form: one whose band none of its mass's
/// 4-neighbours share takes the band most of them have.
fn despeckle(px: &mut [Option<(Band, usize)>], cols: usize, rows: usize) {
    let snapshot = px.to_vec();
    for i in 0..px.len() {
        let Some((band, m)) = snapshot[i] else {
            continue;
        };
        let (x, y) = (i % cols, i / cols);
        // how many of its mass's 4-neighbours have each band
        let mut nb = [0usize; 3];
        [
            (x.wrapping_sub(1), y),
            (x + 1, y),
            (x, y.wrapping_sub(1)),
            (x, y + 1),
        ]
        .into_iter()
        .filter(|&(nx, ny)| nx < cols && ny < rows)
        .filter_map(|(nx, ny)| snapshot[ny * cols + nx])
        .filter(|&(_, nm)| nm == m)
        .for_each(|(b, _)| nb[b as usize] += 1);
        if nb.iter().sum::<usize>() >= 3 && nb[band as usize] == 0 {
            let most = [Band::Lit, Band::Body, Band::Shade]
                .into_iter()
                .max_by_key(|&b| nb[b as usize])
                .unwrap_or(band);
            px[i] = Some((most, m));
        }
    }
}

/// A run no wider than `thin` cells between two cells of one mass and band
/// takes that mass and band: a hairline crease reads as smoke or a crack.
fn close_thin_runs(px: &mut [Option<(Band, usize)>], cols: usize, rows: usize, thin: usize) {
    for horizontal in [true, false] {
        let (outer, inner) = if horizontal {
            (rows, cols)
        } else {
            (cols, rows)
        };
        let at = |o: usize, i: usize| {
            if horizontal {
                o * cols + i
            } else {
                i * cols + o
            }
        };
        for o in 0..outer {
            let mut i = 1;
            while i < inner {
                let prev = px[at(o, i - 1)];
                if prev.is_none() || px[at(o, i)] == prev {
                    i += 1;
                    continue;
                }
                let run = px[at(o, i)];
                let mut n = 0;
                while i + n < inner && px[at(o, i + n)] == run && n <= thin {
                    n += 1;
                }
                if n <= thin && i + n < inner && px[at(o, i + n)] == prev {
                    for j in 0..n {
                        px[at(o, i + j)] = prev;
                    }
                }
                i += n.max(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Motion;
    use crate::display::pen::test_density;
    use crate::sky::Sky;
    use std::time::Duration;

    const SPAN: u16 = 160;
    const GLASS_H: u16 = 20;
    /// One pane the run's width: a strike may fall anywhere over it.
    const WHOLE_RUN: Range<u16> = 0..SPAN;
    const RUN: &[Range<u16>] = std::slice::from_ref(&WHOLE_RUN);

    fn clouds(weather: Weather, hour: u32, strike: Option<StrikePhase>) -> Clouds {
        clouds_at(crate::localclock::at_hour(hour), weather, strike)
    }

    fn clouds_at(
        now: std::time::SystemTime,
        weather: Weather,
        strike: Option<StrikePhase>,
    ) -> Clouds {
        let sky = Sky::at_with(now, weather).with_strike(strike);
        let moment = Moment::resolve(sky, &crate::theme::NORMAL, 0.0, Motion::Full.timing(now));
        Clouds::of(
            &moment.outlook(&crate::theme::NORMAL),
            (SPAN, GLASS_H),
            test_density(1),
            RUN,
            &mut CloudCache::default(),
        )
    }

    /// The mass rasters `f` draws: the `clouds.draw` spans it opens.
    fn draws(f: impl FnOnce()) -> usize {
        struct Count(std::sync::atomic::AtomicUsize);
        impl tracing::Subscriber for Count {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                if span.metadata().name() == "clouds.draw" {
                    self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, _: &tracing::Event<'_>) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }
        let count = std::sync::Arc::new(Count(std::sync::atomic::AtomicUsize::new(0)));
        tracing::subscriber::with_default(std::sync::Arc::clone(&count), f);
        count.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A cache warmed before the first frame draws all of that frame's
    /// `AHEAD`, so a step landing within it draws nothing on demand.
    #[test]
    fn a_warmed_cache_draws_the_first_second_ahead() {
        use crate::sky::WeatherPolicy;
        let theme = &crate::theme::NORMAL;
        let frame = Duration::from_millis(crate::anim::PAINT_FRAME_MS);
        let start = crate::sky::first_transition_after(
            std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        )
        .expect("a transition");
        let moment_at = |n: u32| {
            let timing = Motion::Full.timing(start + frame * n);
            Moment::resolve(Sky::at(timing, WeatherPolicy::Clock), theme, 0.0, timing)
        };
        let steps = (1..AHEAD.as_millis() / frame.as_millis())
            .filter(|&n| {
                Clouds::keyed(&moment_at(n as u32).sky) != Clouds::keyed(&moment_at(0).sky)
            })
            .count();
        assert!(steps > 0, "a step lands within the first second");
        let mut cache = CloudCache::default();
        cache.warm();
        Clouds::of(
            &moment_at(0).outlook(theme),
            (SPAN, GLASS_H),
            test_density(4),
            RUN,
            &mut cache,
        );
        Clouds::draw_ahead(&moment_at(0), (SPAN, GLASS_H), test_density(4), &mut cache);
        for n in 1..(AHEAD.as_millis() / frame.as_millis()) as u32 {
            let moment = moment_at(n);
            let (_, planned) = Clouds::plan(&moment.sky, 0.0, (SPAN, GLASS_H), test_density(4));
            assert!(
                planned.iter().all(|&(_, key)| cache.holds(key)),
                "frame {n} drew on demand"
            );
            Clouds::of(
                &moment.outlook(theme),
                (SPAN, GLASS_H),
                test_density(4),
                RUN,
                &mut cache,
            );
            Clouds::draw_ahead(&moment, (SPAN, GLASS_H), test_density(4), &mut cache);
        }
    }

    /// A mass's bands are drawn in its own undrifted frame, so the key leaves
    /// the drift out and a step drawn ahead at drift 0 is the one the frame
    /// it lands in would draw.
    #[test]
    fn a_mass_draws_the_same_bands_at_any_drift() {
        for weather in Weather::ALL {
            let sky = Sky::at_with(crate::localclock::at_hour(12), weather);
            let (clouds, still) = Clouds::plan(&sky, 0.0, (SPAN, GLASS_H), test_density(4));
            let (_, drifted) = Clouds::plan(&sky, 4321.5, (SPAN, GLASS_H), test_density(4));
            assert_eq!(still.len(), drifted.len(), "{weather:?}");
            for ((a, key_a), (b, key_b)) in still.iter().zip(&drifted) {
                assert_eq!(key_a, key_b, "{weather:?}");
                let glass = f32::from(GLASS_H);
                assert_eq!(clouds.draw(a, glass), clouds.draw(b, glass), "{weather:?}");
            }
        }
    }

    /// A transition's two fullest decks, on the classic's grid and the
    /// cutaway's, and all a look draws ahead of them, fit the cache: a frame
    /// never evicts a mass it draws or one drawn for a step to come.
    #[test]
    fn the_cache_holds_a_transition_in_both_looks() {
        let fullest = Weather::ALL
            .map(|w| deck(w, f32::from(SPAN), f32::from(GLASS_H)).len())
            .into_iter()
            .max()
            .unwrap_or(0);
        let ahead = (AHEAD.as_millis() / u128::from(crate::anim::PAINT_FRAME_MS)) as usize;
        assert!(
            CloudCache::CAPACITY.get() >= 2 * (2 * fullest + ahead * DRAWS_AHEAD),
            "{fullest} masses a deck"
        );
    }

    /// At the paint rate through the office's
    /// [`OutsideCache`](crate::outside::OutsideCache), on Full and Calm,
    /// through a weather transition and across dusk, every frame within
    /// [`AHEAD`] of a step, past the first `lead` frames of warm-up, finds its
    /// every mass already drawn and draws at most [`DRAWS_AHEAD`] ahead: a
    /// frame that reuses the last one's views draws ahead all the same.
    #[test]
    fn every_step_is_drawn_ahead_of_it() {
        use crate::sky::WeatherPolicy;
        let theme = &crate::theme::NORMAL;
        let frame = Duration::from_millis(crate::anim::PAINT_FRAME_MS);
        let noon = crate::localclock::at_hour(12);
        let windows = [
            (
                crate::sky::first_transition_after(noon).expect("a transition"),
                WeatherPolicy::Clock,
                Duration::from_millis(crate::sky::TRANSITION_MS),
            ),
            (
                crate::localclock::at_hour_min(19, 30),
                WeatherPolicy::Forced(Weather::Clear),
                Duration::from_secs(300),
            ),
        ];
        // Each window opens `AHEAD` early: a step within `AHEAD` of the first
        // frame has had no frames to be drawn ahead in.
        let lead = (AHEAD.as_millis() / frame.as_millis()) as u32;
        let pack = crate::pack::test_office();
        let d = test_density(4);
        let wall = crate::outside::Wall {
            size: (crate::layout::WINDOW_W * 3, 32),
            bays: crate::layout::window_slots(crate::layout::WINDOW_W * 3).collect(),
        };
        let (run, glass_h) = wall.glass();
        for motion in [Motion::Full, Motion::Calm] {
            for (start, policy, length) in windows {
                let timing = |n: u32| motion.timing(start - AHEAD + frame * n);
                let planned =
                    |sky: &Sky| Clouds::plan(sky, 0.0, (run.end - run.start, glass_h), d).1;
                // Only a frame within `AHEAD` of a step, where the plan
                // changes, can draw ahead or on demand, so the rest are
                // found by the plan alone, which paints nothing, and skipped.
                let frames = lead + (length.as_millis() / frame.as_millis()) as u32;
                let mut near = vec![false; frames as usize];
                let mut last_plan = planned(&Sky::at(timing(0), policy));
                for n in 1..frames {
                    let plan = planned(&Sky::at(timing(n), policy));
                    if plan != last_plan {
                        // `lead` frames to warm the cache, then `AHEAD`.
                        near[n.saturating_sub(2 * lead) as usize..=n as usize].fill(true);
                    }
                    last_plan = plan;
                }
                let from = near
                    .iter()
                    .position(|&b| b)
                    .expect("the window crosses a step") as u32;
                let mut cache = crate::outside::OutsideCache::default();
                let mut ahead_drawn = 0;
                for n in (from..frames).filter(|&n| near[n as usize]) {
                    let timing = timing(n);
                    let moment = Moment::resolve(Sky::at(timing, policy), theme, 0.0, timing);
                    let missing = planned(&moment.sky)
                        .iter()
                        .filter(|&&(_, key)| !cache.clouds.holds(key))
                        .count();
                    let drawn = draws(|| {
                        cache.views(
                            &moment,
                            &pack,
                            theme,
                            crate::outside::Wall {
                                size: wall.size,
                                bays: wall.bays.clone(),
                            },
                            d,
                            crate::glass_weather::GlassWeather::of(&moment),
                        );
                    });
                    if n > from + lead {
                        assert_eq!(missing, 0, "{motion:?} {policy:?} frame {n} drew on demand");
                        assert!(
                            drawn <= DRAWS_AHEAD,
                            "{motion:?} {policy:?} frame {n}: {drawn}"
                        );
                        ahead_drawn += drawn;
                    }
                }
                assert!(ahead_drawn > 0, "{policy:?}: the window crossed no step");
            }
        }
    }

    /// Laying the masses far to near leaves each cell the band the nearest
    /// mass over it shows, wherever the glass's west edge falls on the run.
    #[test]
    fn the_laid_bands_are_the_nearest_masses() {
        for weather in Weather::ALL {
            for (d, hour) in [(1u16, 12), (4, 12), (1, 23), (4, 18)] {
                let now = crate::localclock::at_hour(hour);
                let sky = Sky::at_with(now, weather);
                let moment =
                    Moment::resolve(sky, &crate::theme::NORMAL, 0.0, Motion::Full.timing(now));
                let c = Clouds::of(
                    &moment.outlook(&crate::theme::NORMAL),
                    (SPAN, GLASS_H),
                    test_density(d),
                    RUN,
                    &mut CloudCache::default(),
                );
                let drift = c.cell_drifts();
                let (cols, rows) = (usize::from(SPAN * d) / 2, usize::from(GLASS_H * d));
                for west in [-3, 0, i32::from(SPAN * d) / 3, i32::from(SPAN * d) - 2] {
                    let laid = c.bands(west, (cols, rows));
                    for (i, &got) in laid.iter().enumerate() {
                        let (col, row) = (west + (i % cols) as i32, (i / cols) as i32);
                        assert_eq!(
                            got,
                            c.band_at(&drift, col, row),
                            "{weather:?} d {d} hour {hour} west {west}: cell ({col}, {row})"
                        );
                    }
                }
            }
        }
    }

    /// At a real clock's time each mass drifts its layer's pace every beat:
    /// no freeze, no jump.
    #[test]
    fn masses_drift_smoothly_at_a_real_clock() {
        let at = |ms: u64| {
            let now = crate::localclock::at_hour(12) + Duration::from_millis(ms);
            let sky = Sky::at_with(now, Weather::Overcast);
            let moment = Moment::resolve(sky, &crate::theme::NORMAL, 0.0, Motion::Full.timing(now));
            Clouds::of(
                &moment.outlook(&crate::theme::NORMAL),
                (SPAN, GLASS_H),
                test_density(1),
                RUN,
                &mut CloudCache::default(),
            )
            .masses
        };
        let beat = crate::anim::FULL_TICK_MS;
        let first = at(0);
        assert!(!first.is_empty());
        for k in 1..=8 {
            let (before, after) = (at((k - 1) * beat), at(k * beat));
            for (a, b) in before.iter().zip(&after) {
                assert_eq!(a.id, b.id);
                let moved = b.off - a.off;
                // a wrap moves it a whole period back, out of sight
                if moved.abs() >= f32::from(SPAN) {
                    continue;
                }
                let pace = a.layer.pace() * beat as f32 / 1000.0;
                assert!(
                    (moved - pace).abs() < pace / 4.0,
                    "beat {k}, mass {}: moved {moved}, its pace {pace}",
                    a.id
                );
            }
        }
    }

    /// A drifting mass wraps only out of sight, in every weather's real deck:
    /// it has left the run's east end whole, and comes back round wholly west
    /// of its west end.
    #[test]
    fn a_mass_wraps_only_out_of_sight() {
        let span = f32::from(SPAN);
        let mut wraps = 0;
        // a sample's step, in seconds: a mass comes back at most the drift of
        // one step onto the run, sliding in from its west end
        const STEP: f64 = 0.5;
        for weather in Weather::ALL {
            for m in deck(weather, span, f32::from(GLASS_H)) {
                let (west, east) = m.extent();
                let slide = m.layer.pace() * STEP as f32;
                let mut prev = m.drift_at(0.0, span);
                for step in 1..4_000 {
                    let off = m.drift_at(f64::from(step) * STEP, span);
                    if off < prev {
                        wraps += 1;
                        assert!(
                            west + prev >= span,
                            "{weather:?} mass {}: wrapped with its west at {}, still on the run",
                            m.id,
                            west + prev
                        );
                        assert!(
                            east + off <= slide,
                            "{weather:?} mass {}: came back with its east at {}, already on the run",
                            m.id,
                            east + off
                        );
                    }
                    prev = off;
                }
            }
        }
        assert!(wraps > 0, "the sample must wrap");
    }

    /// A mass grows from its base: the base stays put, and it fills in as its
    /// weather's share rises, never jumping.
    #[test]
    fn a_mass_grows_from_its_fixed_base() {
        let full = deck(Weather::Rain, f32::from(SPAN), f32::from(GLASS_H));
        for m in &full {
            let mut prev = 0;
            for k in 0..=20 {
                let g = m.grown(ease(k as f32 / 20.0), 0.0);
                assert_eq!(g.base, m.base);
                let cells = (0..SPAN)
                    .flat_map(|x| (0..GLASS_H).map(move |y| (x, y)))
                    .filter(|&(x, y)| {
                        g.lobes.iter().any(|l| l.r > 0.0)
                            && g.inside(f32::from(x) + 0.5, f32::from(y) + 0.5)
                    })
                    .count();
                assert!(
                    cells + 2 >= prev,
                    "share {k}/20 shrank the mass: {prev} → {cells}"
                );
                prev = cells;
            }
        }
    }

    /// A strike's bolt hangs off a drawn cloud: a mass draws the cell its
    /// top starts in or the one over it, at every density, partway through the
    /// strike, so the masses have drifted on since the instant it chose its
    /// cloud at.
    #[test]
    fn the_bolt_hangs_off_a_drawn_cloud() {
        for d in [1u16, 4] {
            let (mut bolts, mut lagged) = (0, 0);
            for k in 0..60u64 {
                let then = crate::localclock::at_hour(12) + Duration::from_secs(k * 15);
                let beat = Motion::Full.timing(then).beat;
                let start = crate::sky::strike_start_ms(beat) - beat.ms();
                // its last phase: the furthest the drift gets from the strike's start
                let now = then + Duration::from_millis(start + 2 * crate::anim::FULL_TICK_MS);
                let sky = Sky::at_with(now, Weather::Storm)
                    .with_strike(Some(crate::sky::StrikePhase::Primary));
                let moment =
                    Moment::resolve(sky, &crate::theme::NORMAL, 0.0, Motion::Full.timing(now));
                let c = Clouds::of(
                    &moment.outlook(&crate::theme::NORMAL),
                    (SPAN, GLASS_H),
                    test_density(d),
                    RUN,
                    &mut CloudCache::default(),
                );
                let Some(&(x, y)) = c
                    .strike
                    .as_ref()
                    .and_then(|s| s.bolt.first())
                    .and_then(|trunk| trunk.first())
                else {
                    continue;
                };
                let drift = c.cell_drifts();
                let struck_at = crate::sky::strike_start_ms(moment.timing.beat);
                lagged += usize::from(moment.timing.beat.ms() > struck_at);
                let df = f32::from(d);
                let (col, row) = ((x * df) as i32, (y * df) as i32);
                assert!(
                    (row - 1..=row).any(|r| c.band_at(&drift, col, r).is_some()),
                    "d {d}, strike {k}: the bolt's top at ({col}, {row}) hangs off no cloud"
                );
                bolts += 1;
            }
            assert!(bolts > 0, "d {d}: the sample must strike a bolt");
            assert_eq!(
                lagged, bolts,
                "d {d}: every bolt drawn after its strike began"
            );
        }
    }

    /// A strike's cloud, bolt and flash hold through all of its phases: they
    /// are chosen as the masses stood when it struck, not as they drift on.
    #[test]
    fn a_strike_holds_through_its_phases() {
        let mut strikes = 0;
        for k in 0..60u64 {
            let then = crate::localclock::at_hour(12) + Duration::from_secs(k * 15);
            let beat = Motion::Full.timing(then).beat;
            let start = crate::sky::strike_start_ms(beat) - beat.ms();
            let at = |phase: u64| {
                let now = then + Duration::from_millis(start + phase * crate::anim::FULL_TICK_MS);
                let sky = Sky::at_with(now, Weather::Storm)
                    .with_strike(Some(crate::sky::StrikePhase::Primary));
                let moment =
                    Moment::resolve(sky, &crate::theme::NORMAL, 0.0, Motion::Full.timing(now));
                Clouds::of(
                    &moment.outlook(&crate::theme::NORMAL),
                    (SPAN, GLASS_H),
                    test_density(1),
                    RUN,
                    &mut CloudCache::default(),
                )
                .strike
            };
            let first = at(0);
            strikes += usize::from(first.is_some());
            for phase in 1..3 {
                assert_eq!(at(phase), first, "strike {k}, phase {phase}");
            }
        }
        assert!(strikes > 0, "the sample must strike");
    }

    /// A strike falls over a pane's glass, clear of its frame, whichever pane
    /// its bucket picks.
    #[test]
    fn a_strike_falls_in_a_pane() {
        let c = clouds(Weather::Storm, 12, Some(StrikePhase::Primary));
        let panes = [3..12, 14..23, 40..50];
        let run = (f32::from(SPAN), f32::from(GLASS_H));
        let mut seen = std::collections::HashSet::new();
        for bucket in 0..200 {
            let Some(s) = c.strike_at(bucket, StrikePhase::Primary, run, 0.0, &panes) else {
                continue;
            };
            let pane = panes
                .iter()
                .position(|p| {
                    (f32::from(p.start) + BOLT_PANE_MARGIN..=f32::from(p.end) - BOLT_PANE_MARGIN)
                        .contains(&s.at.0)
                })
                .unwrap_or_else(|| panic!("bucket {bucket}: struck at {} in no pane", s.at.0));
            seen.insert(pane);
        }
        assert_eq!(seen.len(), panes.len(), "every pane takes strikes");
    }

    /// Rain hangs in shafts under a mid or near mass's base, fading with
    /// depth: none without rain, none under a far mass, none past
    /// [`VIRGA_DEPTH`].
    #[test]
    fn virga_falls_from_a_rained_on_mid_or_near_base() {
        const SKY: Rgb = Rgb {
            r: 90,
            g: 130,
            b: 200,
        };
        // How much the shafts shade the sky, by whole rows below the base of
        // the mass they hang from.
        let shaded = |c: &Clouds| {
            let mut by_depth = vec![0u32; VIRGA_DEPTH.ceil() as usize];
            for col in 0..SPAN {
                for row in 0..GLASS_H {
                    let (x, y) = (f32::from(col) + 0.5, f32::from(row) + 0.5);
                    let cell = Cell {
                        at: (col, row),
                        glass_offset: (col, row),
                    };
                    let out = c.virga(cell, (x, y), SKY);
                    let shade = u32::from(SKY.r.abs_diff(out.r))
                        + u32::from(SKY.g.abs_diff(out.g))
                        + u32::from(SKY.b.abs_diff(out.b));
                    if shade == 0 {
                        continue;
                    }
                    let m = c.virga_mass(x, y).unwrap_or_else(|| {
                        panic!("({col}, {row}) shaded under no mid or near base")
                    });
                    by_depth[(y - m.base) as usize] += shade;
                }
            }
            by_depth
        };
        let rain = clouds(Weather::Rain, 12, None);
        let fall = shaded(&rain);
        assert!(fall[0] > 0, "rain shades under the base: {fall:?}");
        assert!(
            fall[0] > fall[fall.len() - 1],
            "it fades with depth: {fall:?}"
        );

        let mut light = clouds(Weather::Rain, 12, None);
        light.rain /= 2.0;
        let half: u32 = shaded(&light).iter().sum();
        assert!(half < fall.iter().sum(), "lighter rain, fainter shafts");
        light.rain = 0.0;
        assert!(shaded(&light).iter().all(|&n| n == 0), "no rain, no shafts");

        let mut far = clouds(Weather::Rain, 12, None);
        far.masses.iter_mut().for_each(|m| m.layer = Layer::Far);
        assert!(
            shaded(&far).iter().all(|&n| n == 0),
            "none under a far mass"
        );
    }

    /// Where two masses hang over one shaft, it falls from the nearer, as
    /// `band_at` draws the nearer over the farther.
    #[test]
    fn virga_falls_from_the_nearer_of_two_masses() {
        let rain = clouds(Weather::Rain, 12, None);
        let mid = rain
            .masses
            .iter()
            .find(|m| m.layer == Layer::Mid)
            .expect("a rain deck has a mid mass")
            .clone();
        // the same mass nearer, in another weather's tones
        let near = Mass {
            layer: Layer::Near,
            weather: Weather::Storm,
            ..mid.clone()
        };
        assert_ne!(
            rain.tone(mid.weather, Band::Shade),
            rain.tone(near.weather, Band::Shade),
            "the two must tell apart"
        );
        let with = |masses: Vec<Mass>| {
            let mut c = clouds(Weather::Rain, 12, None);
            c.masses = masses;
            c
        };
        let (both, nearer) = (with(vec![mid.clone(), near.clone()]), with(vec![near]));
        let sky = Rgb {
            r: 90,
            g: 130,
            b: 200,
        };
        let mut shafts = 0;
        for col in 0..SPAN {
            for row in 0..GLASS_H {
                let cell = Cell {
                    at: (col, row),
                    glass_offset: (col, row),
                };
                let at = (f32::from(col) + 0.5, f32::from(row) + 0.5);
                let shaft = nearer.virga(cell, at, sky);
                shafts += usize::from(shaft != sky);
                assert_eq!(both.virga(cell, at, sky), shaft, "({col}, {row})");
            }
        }
        assert!(shafts > 0, "the sample must fall in shafts");
    }

    /// A rim lights only the edge its light falls on: no lit cell has its mass
    /// [`RIM_DEPTH`] on toward the light.
    #[test]
    fn a_rim_lights_only_the_edge_its_light_falls_on() {
        let mut rims = 0;
        for hour in 0..24 {
            let c = clouds(Weather::Clear, hour, None);
            if c.diffuse || c.lighting.rim <= 0.01 {
                continue;
            }
            let (lx, ly) = c.lighting.dir;
            let l = lx.hypot(ly).max(f32::EPSILON);
            for m in &c.masses {
                let (west, east) = m.extent();
                let (mut x, step) = (west, 0.25);
                while x <= east {
                    let mut y = m.top();
                    while y <= m.base {
                        if m.inside(x, y) && c.band(m, x, y) == Band::Lit {
                            rims += 1;
                            assert!(
                                !m.inside(x + lx / l * RIM_DEPTH, y + ly / l * RIM_DEPTH),
                                "{hour}h mass {}: ({x}, {y}) lit with its mass on toward the light",
                                m.id
                            );
                        }
                        y += step;
                    }
                    x += step;
                }
            }
        }
        assert!(rims > 0, "the sample must light a rim");
    }

    /// A far mass leans further to the sky behind it than a near one.
    #[test]
    fn a_far_mass_leans_to_the_sky() {
        let sky = Rgb {
            r: 90,
            g: 130,
            b: 200,
        };
        let c = clouds(Weather::Overcast, 12, None);
        for band in [Band::Lit, Band::Body, Band::Shade] {
            let tone = c.tone(Weather::Overcast, band);
            let off = |layer: Layer| {
                let t = tone.mix(sky, layer.sky_mix());
                (i32::from(t.r) - i32::from(sky.r)).abs()
                    + (i32::from(t.g) - i32::from(sky.g)).abs()
                    + (i32::from(t.b) - i32::from(sky.b)).abs()
            };
            assert!(
                off(Layer::Far) < off(Layer::Mid) && off(Layer::Mid) < off(Layer::Near),
                "{band:?}"
            );
        }
    }

    /// A strike lights the deck by the phase the sky's own envelope is in,
    /// walked through real strikes: one look through each phase, the primary
    /// and the dim apart, and nothing once it is over.
    #[test]
    fn a_strike_lights_the_deck_only_by_its_phase() {
        let tick = crate::anim::FULL_TICK_MS;
        let mut walked = 0;
        for k in 0..60u64 {
            let then = crate::localclock::at_hour(12) + Duration::from_secs(k * 15);
            let beat = Motion::Full.timing(then).beat;
            let start = then + Duration::from_millis(crate::sky::strike_start_ms(beat) - beat.ms());
            let at = |ms: u64| {
                let now = start + Duration::from_millis(ms);
                let sky = Sky::at_with(now, Weather::Storm);
                let moment =
                    Moment::resolve(sky, &crate::theme::NORMAL, 0.0, Motion::Full.timing(now));
                let c = Clouds::of(
                    &moment.outlook(&crate::theme::NORMAL),
                    (SPAN, GLASS_H),
                    test_density(1),
                    RUN,
                    &mut CloudCache::default(),
                );
                let lifts: Vec<Option<u32>> = (0..SPAN * 4)
                    .flat_map(|x| (0..GLASS_H * 4).map(move |y| (x, y)))
                    .flat_map(|(x, y)| {
                        let c = &c;
                        (0..c.masses.len()).map(move |m| {
                            c.flash_lift(m, f32::from(x) / 4.0, f32::from(y) / 4.0)
                                .map(f32::to_bits)
                        })
                    })
                    .collect();
                (sky.strike(), c.strike.is_some(), lifts)
            };
            let (phase, struck, _) = at(0);
            if phase.is_none() || !struck {
                continue;
            }
            walked += 1;
            let mut looks = Vec::new();
            for i in 0.. {
                let (phase, _, early) = at(i * tick);
                let Some(phase) = phase else {
                    assert!(
                        early.iter().all(Option::is_none),
                        "strike {k}: lit after its end"
                    );
                    break;
                };
                let (_, _, late) = at(i * tick + tick / 2);
                assert_eq!(
                    early, late,
                    "strike {k}: {phase:?} changed its look within itself"
                );
                looks.push((phase, early));
            }
            let look = |p| looks.iter().find(|(q, _)| *q == p).map(|(_, l)| l);
            assert_ne!(
                look(StrikePhase::Primary),
                look(StrikePhase::Dim),
                "strike {k}: the primary and the dim look alike"
            );
        }
        assert!(walked > 0, "the sample must strike a cloud");
    }

    /// Clouds drift on the beat: at rest they hold still.
    #[test]
    fn a_still_office_holds_its_clouds() {
        let at = |ms: u64| {
            let now = crate::localclock::at_hour(12) + Duration::from_millis(ms);
            let sky = Sky::at_with(now, Weather::Overcast);
            let moment =
                Moment::resolve(sky, &crate::theme::NORMAL, 0.0, Motion::Still.timing(now));
            Clouds::of(
                &moment.outlook(&crate::theme::NORMAL),
                (SPAN, GLASS_H),
                test_density(1),
                RUN,
                &mut CloudCache::default(),
            )
            .masses
        };
        assert_eq!(at(0), at(600_000));
    }
}
