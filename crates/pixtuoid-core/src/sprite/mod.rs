use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use palette::color_difference::EuclideanDistance;
use palette::convert::FromColorUnclamped;
use palette::{Clamp, FromColor, IsWithinBounds, LinSrgb, Mix, Oklab, Oklch, Srgb};

use crate::grid::Grid;

/// Compositing a `Frame` onto an `RgbBuffer`, skipping transparent pixels.
pub mod blit;
pub mod error;
/// Sprite-pack file format: `pack.toml` + `.sprite` parsing and in-memory pack building.
pub mod format;

/// An opaque 24-bit color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red channel, 0–255.
    pub r: u8,
    /// Green channel, 0–255.
    pub g: u8,
    /// Blue channel, 0–255.
    pub b: u8,
}

/// The share of the distance left to white or black one ramp level covers:
/// small, so a material picks its ramp's contrast by how many levels it spans.
const RAMP_LIGHTNESS_STEP: f32 = 0.1;

/// How far one ramp level pulls a color toward the warm or cool hue, in OKLab chroma.
const RAMP_HUE_PULL: f32 = 0.006;

/// The OKLCH hue highlights pull toward: amber, a warm key light.
const RAMP_WARM_HUE_DEG: f32 = 80.0;

/// The OKLCH hue shadows pull toward: blue-violet, a cool ambient fill.
const RAMP_COOL_HUE_DEG: f32 = 280.0;

/// One just-noticeable ΔE-OK, the farthest a clipped color may land from its
/// target: CSS Color 4's <https://www.w3.org/TR/css-color-4/#GMA-Binary-local-MINDE>.
const GAMUT_JND: f32 = 0.02;

/// Where that search stops, in chroma and in ΔE-OK short of [`GAMUT_JND`].
const GAMUT_EPSILON: f32 = 0.0001;

/// Bounds [`RAMP_MEMO`] for a process that ramps ever new colours.
const RAMP_MEMO_CAP: usize = 1 << 12;

thread_local! {
    /// Ramp levels already walked: a painter re-resolves palette ramps per frame.
    static RAMP_MEMO: std::cell::RefCell<HashMap<(Rgb, i8), Rgb>> = Default::default();
}

impl Rgb {
    const WHITE: Rgb = Rgb {
        r: u8::MAX,
        g: u8::MAX,
        b: u8::MAX,
    };
    const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

    /// This color `level` steps along a hue-shifted ramp: lighter and warmer
    /// above zero, darker and cooler below, itself at zero.
    ///
    /// Stepped in OKLab, where equal lightness steps look equal whatever the
    /// hue. A level covers a share of the distance left to white or black, at
    /// least a `GAMUT_JND` where that fits: a fixed step would merge a dark
    /// base's deeper shadows into one black. A step past the gamut gives up
    /// chroma before that lightness, so a highlight pales.
    ///
    /// The warm and cool shift is a pull in OKLab's a/b plane toward a fixed
    /// hue: a hue rotation flips direction at the hue opposite its target and
    /// has nothing to rotate on a grey, where the pull warms its lights.
    #[must_use]
    pub fn ramp(self, level: i8) -> Rgb {
        if level == 0 {
            return self;
        }
        RAMP_MEMO.with_borrow_mut(|memo| {
            if memo.len() >= RAMP_MEMO_CAP {
                memo.clear();
            }
            *memo.entry((self, level)).or_insert_with(|| {
                self.ramp_run(level > 0)
                    .take(usize::from(level.unsigned_abs()))
                    .fold(self, |_, step| step)
            })
        })
    }

    fn ramp_run(self, lit: bool) -> impl Iterator<Item = Rgb> {
        let base = self.to_oklab();
        (1..=u8::MAX).scan((self, base.l), move |(prev, aim), steps| {
            (*prev, *aim) = Rgb::ramp_step(base, steps, lit, *prev, *aim);
            Some(*prev)
        })
    }

    /// Level `steps` past `prev` (aimed at `prev_aim`), and the lightness it aims at.
    fn ramp_step(base: Oklab, steps: u8, lit: bool, prev: Rgb, prev_aim: f32) -> (Rgb, f32) {
        let keep = (1.0 - RAMP_LIGHTNESS_STEP).powi(i32::from(steps));
        let (own, hue, toward) = if lit {
            (1.0 - (1.0 - base.l) * keep, RAMP_WARM_HUE_DEG, 1.0)
        } else {
            (base.l * keep, RAMP_COOL_HUE_DEG, -1.0)
        };
        let (sin, cos) = hue.to_radians().sin_cos();
        let pull = RAMP_HUE_PULL * f32::from(steps);
        let (a, b) = (base.a + pull * cos, base.b + pull * sin);
        let room = if lit { 1.0 - prev_aim } else { prev_aim };
        // One share held back keeps the last declarable level off white or black.
        let shares = format::MAX_RAMP_LEVEL.unsigned_abs().saturating_sub(steps) + 2;
        let gap = GAMUT_JND.min(room / f32::from(shares));
        let aim = if lit {
            own.max(prev_aim + gap)
        } else {
            own.min(prev_aim - gap)
        };
        // Rounding to 8 bits can still land on `prev`: push on past it.
        let below = prev.lightness();
        let (mut l, mut nudge) = (aim, GAMUT_EPSILON);
        loop {
            let c = Rgb::from_oklab_within(Oklab::new(l, a, b), gap / 2.0);
            let past = if lit {
                c.lightness() > below
            } else {
                c.lightness() < below
            };
            if past || (lit && l >= 1.0) || (!lit && l <= 0.0) {
                return (c, aim);
            }
            l += toward * nudge;
            nudge *= 2.0;
        }
    }

    /// The color `t` of the way from this one to `other`, `t` clamped to
    /// `0..=1`.
    ///
    /// Interpolated in OKLab, the space [`Rgb::ramp`] steps in, so a mix's
    /// lightness moves evenly with `t` where an sRGB mix of two hues sags
    /// through a darker, muddier middle.
    #[must_use]
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        debug_assert!(!t.is_nan(), "a NaN t survives the clamp");
        Rgb::from_oklab_in_gamut(self.to_oklab().mix(other.to_oklab(), t))
    }

    /// How light this color looks, from black at 0 to white at 1: OKLab's
    /// lightness, the axis [`Rgb::ramp`] steps along and [`Rgb::mix`] runs
    /// evenly on.
    pub fn lightness(self) -> f32 {
        self.to_oklab().l
    }

    fn to_oklab(self) -> Oklab {
        Oklab::from_color(Srgb::new(self.r, self.g, self.b).into_format::<f32>())
    }

    /// `c` cut in chroma at its lightness and hue until clipping lands within
    /// [`GAMUT_JND`]: clipping alone turns hue, cutting alone jumps at a graze.
    fn from_oklab_in_gamut(c: Oklab) -> Rgb {
        Rgb::from_oklab_within(c, GAMUT_JND)
    }

    /// [`Rgb::from_oklab_in_gamut`], its clip also within `slack` of `c`'s lightness.
    fn from_oklab_within(c: Oklab, slack: f32) -> Rgb {
        if c.l >= 1.0 {
            return Rgb::WHITE;
        }
        if c.l <= 0.0 {
            return Rgb::BLACK;
        }
        let lin = |c: Oklch| LinSrgb::from_color_unclamped(c);
        let clip = |c: Oklch| {
            let clipped = lin(c).clamp();
            let landed = Oklab::from_color_unclamped(clipped);
            let miss = landed.distance(Oklab::from_color_unclamped(c));
            let near = miss < GAMUT_JND && (landed.l - c.l).abs() < slack;
            (clipped, miss, near)
        };
        let mut current = Oklch::from_color_unclamped(c);
        let mut clipped = lin(current);
        if !clipped.is_within_bounds() {
            let near;
            (clipped, _, near) = clip(current);
            if !near {
                let (mut min, mut max) = (0.0, current.chroma);
                let mut min_in_gamut = true;
                while max - min > GAMUT_EPSILON {
                    current.chroma = f32::midpoint(min, max);
                    if min_in_gamut && lin(current).is_within_bounds() {
                        min = current.chroma;
                        continue;
                    }
                    let (miss, near);
                    (clipped, miss, near) = clip(current);
                    if !near {
                        max = current.chroma;
                    } else if GAMUT_JND - miss < GAMUT_EPSILON {
                        break;
                    } else {
                        min_in_gamut = false;
                        min = current.chroma;
                    }
                }
            }
        }
        let out: Srgb<u8> = Srgb::<f32>::from_linear(clipped).into_format();
        Rgb {
            r: out.red,
            g: out.green,
            b: out.blue,
        }
    }
}

/// A single pixel: `Some(rgb)` or `None` (transparent).
pub type Pixel = Option<Rgb>;

/// How a palette key gets its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Color(Pixel),
    /// `level` steps along `of`'s ramp ([`Rgb::ramp`]).
    Ramp {
        of: char,
        level: i8,
    },
}

/// A palette of single-character keys, each with an index and a color: its
/// own, or a ramp step of another key's.
///
/// A pack holds its frames as these indices ([`Sprite::recolorable_at`]), so
/// replacing a key's color recolors exactly the pixels drawn with that key and
/// every ramp of it, even where another key has the same color.
#[derive(Debug, Clone, Default)]
pub struct Palette {
    /// In index order.
    entries: Vec<(char, Entry)>,
    index: HashMap<char, usize>,
}

impl Palette {
    /// A palette with no keys.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Map `key` to `pixel` (opaque `Some(rgb)` or transparent `None`). A key
    /// that was a ramp becomes this fixed color.
    pub(crate) fn insert(&mut self, key: char, pixel: Pixel) {
        self.set(key, Entry::Color(pixel));
    }

    /// Declare `key` as `level` steps along `of`'s ramp ([`Rgb::ramp`]). It has
    /// no color of its own, so replacing `of`'s color moves it too.
    ///
    /// It resolves only while `of` has a color of its own: a ramp of an
    /// undefined key or of another ramp is undefined, so resolution never
    /// chains or cycles.
    pub(crate) fn insert_ramp(&mut self, key: char, of: char, level: i8) {
        self.set(key, Entry::Ramp { of, level });
    }

    /// Replace `key`'s entry in place: frames hold its index, so a second slot
    /// for the key would leave them reading the old one.
    fn set(&mut self, key: char, entry: Entry) {
        match self.index.get(&key).and_then(|&i| self.entries.get_mut(i)) {
            Some(slot) => slot.1 = entry,
            None => {
                self.index.insert(key, self.entries.len());
                self.entries.push((key, entry));
            }
        }
    }

    /// Look up `key`: `None` if it is undefined, a ramp that does not resolve
    /// included (`insert_ramp`), else `Some(pixel)` (the
    /// pixel itself may be transparent).
    pub fn get(&self, key: char) -> Option<Pixel> {
        self.resolve(self.entry(key)?)
    }

    fn entry(&self, key: char) -> Option<Entry> {
        self.entries.get(self.index_of(key)?).map(|&(_, e)| e)
    }

    fn resolve(&self, entry: Entry) -> Option<Pixel> {
        match entry {
            Entry::Color(pixel) => Some(pixel),
            Entry::Ramp { of, level } => match self.entry(of)? {
                Entry::Color(pixel) => Some(pixel.map(|rgb| rgb.ramp(level))),
                Entry::Ramp { .. } => None,
            },
        }
    }

    fn index_of(&self, key: char) -> Option<usize> {
        self.index.get(&key).copied()
    }

    /// The index a frame stores for `key`, if the key resolves: decided from
    /// the entries alone, so parsing a pixel never derives a ramp's color.
    fn drawable_index(&self, key: char) -> Option<usize> {
        let index = self.index_of(key)?;
        match self.entry(key)? {
            Entry::Color(_) => Some(index),
            Entry::Ramp { of, .. } => matches!(self.entry(of)?, Entry::Color(_)).then_some(index),
        }
    }

    /// Every index's pixel: what a frame's indices resolve through. An index
    /// that does not resolve is transparent, though no parsed frame holds one.
    fn resolved(&self) -> Vec<Pixel> {
        self.entries
            .iter()
            .map(|&(_, e)| self.resolve(e).flatten())
            .collect()
    }
}

/// A palette index as a frame stores it: the indexed-color convention, so a
/// palette addresses at most [`PALETTE_CAPACITY`] keys.
type PaletteIndex = u8;

/// The most keys a palette's frames can address.
const PALETTE_CAPACITY: usize = PaletteIndex::MAX as usize + 1;

/// A frame as palette indices: how a pack holds its art, so a recolor resolves
/// the same indices through a different palette.
#[derive(Debug, Clone)]
struct IndexedFrame(Grid<PaletteIndex>);

impl IndexedFrame {
    /// The frame in `pixels`' colors, one entry per palette index.
    fn resolve(&self, pixels: &[Pixel]) -> Frame {
        let data = self
            .0
            .as_slice()
            .iter()
            .map(|&i| pixels.get(usize::from(i)).copied().flatten())
            .collect();
        Frame::from_pixels(self.0.width(), self.0.height(), data)
    }
}

/// A sprite frame: a `width × height` row-major grid of `Pixel`s.
#[derive(Debug, Clone, Default)]
pub struct Frame(Grid<Pixel>);

impl std::ops::Deref for Frame {
    type Target = Grid<Pixel>;
    fn deref(&self) -> &Grid<Pixel> {
        &self.0
    }
}

impl std::ops::DerefMut for Frame {
    fn deref_mut(&mut self) -> &mut Grid<Pixel> {
        &mut self.0
    }
}

impl Frame {
    /// Build a frame from a row-major pixel `Vec` (length = `width * height`).
    pub fn from_pixels(width: u16, height: u16, pixels: Vec<Pixel>) -> Self {
        Frame(Grid::from_vec(width, height, pixels))
    }

    /// Reverse each row — turns a right-facing sprite into a left-facing one.
    #[must_use]
    pub fn mirror_horizontal(&self) -> Self {
        let w = self.width as usize;
        let h = self.height as usize;
        let src = self.as_slice();
        let mut pixels = Vec::with_capacity(src.len());
        for y in 0..h {
            let row_start = y * w;
            for x in (0..w).rev() {
                pixels.push(src[row_start + x]);
            }
        }
        Frame::from_pixels(self.width, self.height, pixels)
    }

    /// Flip rows top-to-bottom.
    #[must_use]
    pub fn mirror_vertical(&self) -> Self {
        let w = self.width as usize;
        let h = self.height as usize;
        let src = self.as_slice();
        let mut pixels = Vec::with_capacity(src.len());
        for y in (0..h).rev() {
            let row_start = y * w;
            for x in 0..w {
                pixels.push(src[row_start + x]);
            }
        }
        Frame::from_pixels(self.width, self.height, pixels)
    }
}

/// A named point on one frame, from a `.sprite` frame's `@mark <name> <x> <y>`:
/// where a painter lays something over the art, in the frame's own pixels.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Mark {
    name: String,
    x: u16,
    y: u16,
}

impl Mark {
    pub(crate) fn new(name: String, x: u16, y: u16) -> Self {
        Self { name, x, y }
    }

    /// Its name: `head.<view>` is where a hairstyle is laid ([`Sprite::head`]).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its column.
    pub fn x(&self) -> u16 {
        self.x
    }

    /// Its row.
    pub fn y(&self) -> u16 {
        self.y
    }
}

/// Which way a character frame's head faces the viewer: the view a hairstyle
/// draws its layers for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum HeadView {
    /// The face toward the viewer.
    Front,
    /// The back of the head toward the viewer.
    Back,
    /// In profile.
    Side,
    /// Seen from above, face down.
    Crown,
}

impl HeadView {
    /// Every view.
    pub(crate) const ALL: [HeadView; 4] = [Self::Front, Self::Back, Self::Side, Self::Crown];

    /// The name a head mark (`head.<name>`) and a `[hairstyles]` table call it by.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Front => "front",
            Self::Back => "back",
            Self::Side => "side",
            Self::Crown => "crown",
        }
    }

    /// The view called `name`, if there is one.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == name)
    }

    /// Its place in [`HeadView::ALL`].
    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// A frame's head: which way it faces and the point a hairstyle layer's own
/// head is laid on, read from the frame's one `head.<view>` [`Mark`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct HeadMark {
    /// Which way the head faces.
    pub view: HeadView,
    /// The mark's column.
    pub x: u16,
    /// The mark's row.
    pub(crate) y: u16,
}

impl HeadMark {
    /// The head `mark` places, where it is a `head.<view>` mark.
    pub(crate) fn of(mark: &Mark) -> Option<Self> {
        let view = HeadView::from_name(mark.name().strip_prefix(HEAD_MARK)?)?;
        Some(Self {
            view,
            x: mark.x,
            y: mark.y,
        })
    }
}

/// What a head mark's name starts with, before its view.
pub(crate) const HEAD_MARK: &str = "head.";

/// An animation: its frames in order, at least one, the palette indices they
/// were drawn with, each frame's marks, the per-frame hold time, and a walk's
/// stride.
#[derive(Debug, Clone)]
pub struct Sprite {
    /// The frames in their own palette's colors.
    frames: vec1::Vec1<Frame>,
    /// The same frames as indices into `palette`, for a recolor to resolve again.
    indexed: Vec<IndexedFrame>,
    /// Each frame's `@mark`s.
    marks: Vec<Vec<Mark>>,
    /// The palette `indexed` refers to: its pack's.
    palette: Arc<Palette>,
    frame_ms: u32,
    stride: Option<std::num::NonZeroU16>,
}

impl Sprite {
    fn new(
        marked: vec1::Vec1<(IndexedFrame, Vec<Mark>)>,
        palette: Arc<Palette>,
        frame_ms: u32,
        stride: Option<std::num::NonZeroU16>,
    ) -> Self {
        let pixels = palette.resolved();
        let frames = marked.mapped_ref(|(f, _)| f.resolve(&pixels));
        let (indexed, marks): (Vec<_>, Vec<_>) = marked.into_iter().unzip();
        Sprite {
            frames,
            indexed,
            marks,
            palette,
            frame_ms,
            stride,
        }
    }

    /// Frame `idx`'s marks, in the order the file names them.
    pub fn marks(&self, idx: usize) -> &[Mark] {
        self.marks.get(idx).map_or(&[], Vec::as_slice)
    }

    /// Frame `idx`'s head: where a hairstyle dresses it.
    pub fn head(&self, idx: usize) -> Option<HeadMark> {
        self.marks(idx).iter().find_map(HeadMark::of)
    }

    /// Frame 0 and its offset when its own head mark is laid on `head`.
    pub fn laid_on(&self, head: HeadMark) -> Option<(&Frame, i32, i32)> {
        let mark = self.head(0)?;
        Some((
            self.first(),
            i32::from(head.x) - i32::from(mark.x),
            i32::from(head.y) - i32::from(mark.y),
        ))
    }

    /// The frames, played in order.
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// The first frame.
    pub fn first(&self) -> &Frame {
        self.frames.first()
    }

    /// `idx`, or `0` once it runs past the last frame: an animation with fewer
    /// frames than a shared cycle's index then shows its first rather than
    /// vanishing.
    pub fn wrap(&self, idx: usize) -> usize {
        if idx < self.frames.len() { idx } else { 0 }
    }

    /// Frame [`wrap`](Self::wrap)`(idx)`.
    pub fn frame_at(&self, idx: usize) -> &Frame {
        &self.frames[self.wrap(idx)]
    }

    /// Frame [`wrap`](Self::wrap)`(idx)` as the palette indices a recolor
    /// resolves.
    pub fn recolorable_at(&self, idx: usize) -> RecolorableFrame<'_> {
        RecolorableFrame {
            indexed: &self.indexed[self.wrap(idx)],
            palette: &self.palette,
        }
    }

    /// How long each frame holds before advancing, in milliseconds: a walk
    /// with a [`stride`](Self::stride) steps by distance instead.
    pub fn frame_ms(&self) -> u32 {
        self.frame_ms
    }

    /// The base-grid pixels a walker covers in one full cycle of its frames:
    /// they advance by distance travelled, so a planted foot stays planted at
    /// any speed. `None` where the pack declares none.
    pub fn stride(&self) -> Option<std::num::NonZeroU16> {
        self.stride
    }
}

/// One frame of a [`Sprite`] as palette indices, from [`Sprite::recolorable_at`].
#[derive(Debug, Clone, Copy)]
pub struct RecolorableFrame<'a> {
    indexed: &'a IndexedFrame,
    palette: &'a Palette,
}

impl RecolorableFrame<'_> {
    /// The frame with each `(key, pixel)` of `overrides` replacing that key's
    /// color, and so every ramp of it.
    ///
    /// An override replaces a color and never adds one: a key the palette
    /// lacks or holds transparent stays as it is, so a pack that leaves a part
    /// out (a character with no hair) keeps it out for every agent.
    pub fn recolored(&self, overrides: &[(char, Pixel)]) -> Frame {
        let mut palette = self.palette.clone();
        for &(key, pixel) in overrides {
            if let Some(Some(_)) = palette.get(key) {
                palette.insert(key, pixel);
            }
        }
        self.indexed.resolve(&palette.resolved())
    }

    /// Which of the frame's pixels, row by row, it draws in one of `keys`:
    /// those that go transparent when the keys are blanked.
    pub fn drawn_in(&self, keys: &[char]) -> Vec<bool> {
        let art = self.recolored(&[]);
        let blank = self.recolored(&keys.iter().map(|&k| (k, None)).collect::<Vec<_>>());
        let opaque = |f: &Frame, x, y| f.get(x, y).and_then(|p| *p).is_some();
        (0..art.height())
            .flat_map(|y| (0..art.width()).map(move |x| (x, y)))
            .map(|(x, y)| opaque(&art, x, y) && !opaque(&blank, x, y))
            .collect()
    }
}

/// A flat RGB buffer used as a blit target.
#[derive(Debug, Clone)]
pub struct RgbBuffer {
    pixels: Grid<Rgb>,
    writes: Option<Writes>,
    /// The columns and rows writes are kept to, if any: see
    /// [`with_clip`](Self::with_clip).
    clip: Option<(Range<u16>, Range<u16>)>,
}

/// A write epoch of the one buffer whose [`RgbBuffer::begin_writes`] minted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteEpoch(u32);

/// Each pixel's epoch of its last noted write.
#[derive(Debug, Clone)]
struct Writes {
    at: Vec<u32>,
    now: u32,
    tracking: bool,
}

impl Writes {
    fn note(&mut self, i: usize) {
        if self.tracking {
            self.at[i] = self.now;
        }
    }

    fn note_all(&mut self) {
        if self.tracking {
            self.at.fill(self.now);
        }
    }
}

impl std::ops::Deref for RgbBuffer {
    type Target = Grid<Rgb>;
    fn deref(&self) -> &Grid<Rgb> {
        &self.pixels
    }
}

impl RgbBuffer {
    /// A `width × height` buffer with every pixel set to `fill`.
    #[inline]
    pub fn filled(width: u16, height: u16, fill: Rgb) -> Self {
        RgbBuffer {
            pixels: Grid::filled(width, height, fill),
            writes: None,
            clip: None,
        }
    }

    /// Build from a row-major `Vec<Rgb>` (length = `width * height`).
    pub fn from_pixels(width: u16, height: u16, pixels: Vec<Rgb>) -> Self {
        RgbBuffer {
            pixels: Grid::from_vec(width, height, pixels),
            writes: None,
            clip: None,
        }
    }

    #[inline]
    fn raw_index(&self, x: u16, y: u16) -> usize {
        (y as usize) * (self.pixels.width as usize) + (x as usize)
    }

    /// [`raw_index`](Self::raw_index) guarded by a debug-only bounds assert: a
    /// stray `x >= width` would silently read/write the WRONG row rather than
    /// fault. Unchecked in release (the hot path).
    #[inline]
    fn checked_index(&self, x: u16, y: u16) -> usize {
        debug_assert!(
            x < self.pixels.width && y < self.pixels.height,
            "RgbBuffer index out of bounds: ({x},{y}) in {}x{}",
            self.pixels.width,
            self.pixels.height
        );
        self.raw_index(x, y)
    }

    /// Read the `Rgb` at `(x, y)`. Debug-asserts the point is in bounds;
    /// unchecked in release (the hot blit path clips first).
    #[inline]
    pub fn get(&self, x: u16, y: u16) -> Rgb {
        self.pixels.as_slice()[self.checked_index(x, y)]
    }

    /// Write `rgb` at `(x, y)`, unless a [`with_clip`](Self::with_clip) keeps
    /// it out. Debug-asserts the point is in bounds; use
    /// [`put_checked`](Self::put_checked) when `(x, y)` may fall outside.
    #[inline]
    pub fn put(&mut self, x: u16, y: u16, rgb: Rgb) {
        if self.clipped(x, y) {
            return;
        }
        let i = self.checked_index(x, y);
        self.write(i, rgb);
    }

    /// Run `paint` with every write kept to `columns × rows` within any clip
    /// already set, and the clip as it was after: Skia's `save`, `clipRect`,
    /// `restore`. A repaint of part of the buffer runs the whole paint and
    /// lands only there; a bulk writer goes through
    /// [`writable_rows_mut`](Self::writable_rows_mut), never
    /// [`as_mut_slice`](Self::as_mut_slice). A painter's mechanism, not
    /// core's contract.
    #[doc(hidden)]
    pub fn with_clip<T>(
        &mut self,
        (columns, rows): (Range<u16>, Range<u16>),
        paint: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let (xs, ys) = self.writable();
        let clip = (
            columns.start.max(xs.start)..columns.end.min(xs.end),
            rows.start.max(ys.start)..rows.end.min(ys.end),
        );
        let was = self.clip.replace(clip);
        let painted = paint(self);
        self.clip = was;
        painted
    }

    /// Each writable row as `(y, its first writable column, its writable
    /// pixels)`: a bulk write that keeps to the clip by construction.
    #[doc(hidden)]
    pub fn writable_rows_mut(&mut self) -> impl Iterator<Item = (u16, u16, &mut [Rgb])> {
        let (xs, ys) = self.writable();
        if let Some(w) = &mut self.writes {
            w.note_all();
        }
        let width = usize::from(self.pixels.width).max(1);
        self.pixels
            .as_mut_slice()
            .chunks_mut(width)
            .zip(0u16..)
            .skip(usize::from(ys.start))
            .take(usize::from(ys.end.saturating_sub(ys.start)))
            .map(move |(row, y)| {
                let run = row
                    .get_mut(usize::from(xs.start)..usize::from(xs.end))
                    .unwrap_or_default();
                (y, xs.start, run)
            })
    }

    /// The columns and rows a write may land in: the clip, within the buffer.
    #[doc(hidden)]
    pub fn writable(&self) -> (Range<u16>, Range<u16>) {
        let (w, h) = (self.pixels.width, self.pixels.height);
        match &self.clip {
            Some((xs, ys)) => (
                xs.start.min(w)..xs.end.min(w),
                ys.start.min(h)..ys.end.min(h),
            ),
            None => (0..w, 0..h),
        }
    }

    #[inline]
    fn clipped(&self, x: u16, y: u16) -> bool {
        self.clip
            .as_ref()
            .is_some_and(|(xs, ys)| !xs.contains(&x) || !ys.contains(&y))
    }

    /// Bounds-checked write: a no-op when `(x, y)` falls outside the buffer or
    /// a [`with_clip`](Self::with_clip). The bounds check for per-pixel scatter
    /// (glyphs, particles) that can't pre-clip; the hot blit path clips its
    /// loop bounds to [`writable`](Self::writable) once and keeps
    /// [`put`](Self::put).
    #[inline]
    pub fn put_checked(&mut self, x: u16, y: u16, rgb: Rgb) {
        if x < self.pixels.width && y < self.pixels.height && !self.clipped(x, y) {
            let i = self.raw_index(x, y);
            self.write(i, rgb);
        }
    }

    /// Every pixel, row-major, to write in bulk. While
    /// [`begin_writes`](Self::begin_writes) tracks, the whole buffer counts as
    /// written this epoch, since a bulk write may touch any of it. Outside any
    /// [`with_clip`](Self::with_clip) only: it hands out the whole buffer, so a
    /// clipped bulk write goes through [`writable_rows_mut`](Self::writable_rows_mut).
    pub fn as_mut_slice(&mut self) -> &mut [Rgb] {
        debug_assert!(
            self.clip.is_none(),
            "a clipped bulk write goes through writable_rows_mut"
        );
        if let Some(w) = &mut self.writes {
            w.note_all();
        }
        self.pixels.as_mut_slice()
    }

    /// Resize to `width × height` with every pixel `fill`, which counts as
    /// writing every pixel.
    pub fn resize_fill(&mut self, width: u16, height: u16, fill: Rgb) {
        let reshaped = (width, height) != (self.pixels.width, self.pixels.height);
        self.pixels.resize_fill(width, height, fill);
        if let Some(w) = &mut self.writes {
            if reshaped {
                w.at.clear();
                w.at.resize(self.pixels.as_slice().len(), 0);
            }
            w.note_all();
        }
    }

    #[inline]
    fn write(&mut self, i: usize, rgb: Rgb) {
        self.pixels.as_mut_slice()[i] = rgb;
        if let Some(w) = &mut self.writes {
            w.note(i);
        }
    }

    /// Start a write epoch and return it, for [`written_in`](Self::written_in):
    /// unlike a diff, it sees a pixel written in the colour already there.
    pub fn begin_writes(&mut self) -> WriteEpoch {
        let n = self.pixels.as_slice().len();
        let w = self.writes.get_or_insert_with(|| Writes {
            at: vec![0; n],
            now: 0,
            tracking: false,
        });
        w.tracking = true;
        w.now = w.now.wrapping_add(1);
        if w.now == 0 {
            w.at.fill(0);
            w.now = 1;
        }
        WriteEpoch(w.now)
    }

    /// Stop noting writes until the next [`begin_writes`](Self::begin_writes);
    /// epochs already noted still answer [`written_in`](Self::written_in).
    pub fn end_writes(&mut self) {
        if let Some(w) = &mut self.writes {
            w.tracking = false;
        }
    }

    /// Whether `(x, y)` was last written in `epoch` ([`begin_writes`](Self::begin_writes)).
    pub fn written_in(&self, x: u16, y: u16, epoch: WriteEpoch) -> bool {
        x < self.pixels.width
            && y < self.pixels.height
            && self
                .writes
                .as_ref()
                .is_some_and(|w| w.at.get(self.raw_index(x, y)) == Some(&epoch.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// The buffer's pixels as rows of 0/1 against `bg`.
    fn drawn(buf: &RgbBuffer, bg: Rgb) -> Vec<String> {
        (0..buf.height())
            .map(|y| {
                (0..buf.width())
                    .map(|x| if buf.get(x, y) == bg { '0' } else { '1' })
                    .collect()
            })
            .collect()
    }

    /// A clip within a clip keeps to both, and each closure leaves the clip as
    /// it found it; one wider than the buffer keeps to the buffer.
    #[test]
    fn a_clip_nests_by_intersection_and_restores() {
        let (bg, ink) = (rgb(0, 0, 0), rgb(9, 9, 9));
        let mut buf = RgbBuffer::filled(6, 4, bg);
        buf.with_clip((1..5, 0..9), |buf| {
            assert_eq!(
                buf.writable(),
                (1..5, 0..4),
                "the buffer bounds a wide clip"
            );
            buf.with_clip((3..9, 1..3), |buf| {
                assert_eq!(buf.writable(), (3..5, 1..3));
                for y in 0..4 {
                    for x in 0..6 {
                        buf.put(x, y, ink);
                        buf.put_checked(x + 1, y, ink);
                    }
                }
            });
            assert_eq!(buf.writable(), (1..5, 0..4), "the inner clip is lifted");
        });
        assert_eq!(buf.writable(), (0..6, 0..4), "and the outer one");
        assert_eq!(drawn(&buf, bg), ["000000", "000110", "000110", "000000"]);
    }

    /// A clipped bulk write sees exactly the clip's rows and columns, and an
    /// empty clip's rows no pixels.
    #[test]
    fn the_writable_rows_are_the_clips_and_no_more() {
        let (bg, ink) = (rgb(0, 0, 0), rgb(9, 9, 9));
        let mut buf = RgbBuffer::filled(5, 4, bg);
        buf.with_clip((1..3, 2..4), |buf| {
            let rows: Vec<(u16, u16, usize)> = buf
                .writable_rows_mut()
                .map(|(y, first, row)| {
                    row.fill(ink);
                    (y, first, row.len())
                })
                .collect();
            assert_eq!(rows, [(2, 1, 2), (3, 1, 2)]);
        });
        assert_eq!(drawn(&buf, bg), ["00000", "00000", "01100", "01100"]);
        buf.with_clip((3..3, 0..4), |buf| {
            let runs: Vec<usize> = buf
                .writable_rows_mut()
                .map(|(_, _, row)| row.len())
                .collect();
            assert_eq!(runs, [0; 4]);
        });
    }

    #[test]
    fn a_bulk_write_counts_as_writing_every_pixel() {
        let mut buf = RgbBuffer::filled(3, 2, rgb(0, 0, 0));
        let epoch = buf.begin_writes();
        assert!(!buf.written_in(1, 1, epoch));
        buf.as_mut_slice()[0] = rgb(9, 9, 9);
        assert!((0..2).all(|y| (0..3).all(|x| buf.written_in(x, y, epoch))));
        let epoch = buf.begin_writes();
        buf.resize_fill(4, 4, rgb(1, 1, 1));
        assert!(buf.written_in(3, 3, epoch), "a resize writes every pixel");
    }

    #[test]
    fn a_write_after_end_writes_leaves_the_last_epoch_standing() {
        let mut buf = RgbBuffer::filled(2, 1, rgb(0, 0, 0));
        let epoch = buf.begin_writes();
        buf.put(0, 0, rgb(9, 9, 9));
        buf.end_writes();
        buf.put(1, 0, rgb(9, 9, 9));
        assert!(buf.written_in(0, 0, epoch));
        assert!(
            !buf.written_in(1, 0, epoch),
            "a write after end_writes is not noted"
        );
    }

    #[test]
    fn a_reshape_forgets_epochs_noted_in_the_old_layout() {
        let mut buf = RgbBuffer::filled(4, 2, rgb(0, 0, 0));
        let epoch = buf.begin_writes();
        buf.put(0, 1, rgb(9, 9, 9));
        buf.end_writes();
        buf.resize_fill(2, 4, rgb(0, 0, 0));
        assert!(!(0..4).any(|y| (0..2).any(|x| buf.written_in(x, y, epoch))));
    }

    #[test]
    fn an_epoch_past_the_last_never_reads_an_unwritten_pixel_as_written() {
        let mut buf = RgbBuffer::filled(2, 1, rgb(0, 0, 0));
        buf.begin_writes();
        buf.put(0, 0, rgb(9, 9, 9));
        if let Some(w) = &mut buf.writes {
            w.now = u32::MAX;
        }
        let epoch = buf.begin_writes();
        assert!(!buf.written_in(0, 0, epoch));
        assert!(!buf.written_in(1, 0, epoch));
    }

    /// Mid grey, dark hair, skin, and red, blue and yellow, which a lit step
    /// pushes out of gamut; black and white are left out, having no darker or
    /// lighter.
    const RAMP_BASES: [Rgb; 6] = [
        rgb(128, 128, 128),
        rgb(42, 26, 14),
        rgb(232, 180, 138),
        rgb(255, 0, 0),
        rgb(0, 0, 255),
        rgb(255, 255, 0),
    ];

    fn shades(base: Rgb) -> Vec<Rgb> {
        let max = usize::from(format::MAX_RAMP_LEVEL.unsigned_abs());
        let mut shades: Vec<Rgb> = base.ramp_run(false).take(max).collect();
        shades.reverse();
        shades.push(base);
        shades.extend(base.ramp_run(true).take(max));
        shades
    }

    #[test]
    fn a_ramp_level_is_that_step_of_its_walk() {
        let max = format::MAX_RAMP_LEVEL;
        for base in RAMP_BASES {
            let shades = shades(base);
            for (n, &shade) in (-max..=max).zip(&shades) {
                assert_eq!(base.ramp(n), shade, "{base:?} level {n}");
            }
        }
    }

    /// Also catches a clipped out-of-gamut step ([`Rgb::from_oklab_in_gamut`]).
    #[test]
    fn every_ramp_level_a_pack_may_declare_is_lighter_than_the_one_below() {
        for base in RAMP_BASES {
            assert_eq!(base.ramp(0), base);
            let lightness: Vec<f32> = shades(base).iter().map(|c| c.lightness()).collect();
            assert!(
                lightness.windows(2).all(|w| w[0] < w[1]),
                "{base:?}: {lightness:?}"
            );
        }
    }

    #[test]
    fn every_ramp_steps_through_ever_lighter_distinct_colors() {
        let levels = (0..=u8::MAX).step_by(8).chain([u8::MAX]);
        for r in levels.clone() {
            for g in levels.clone() {
                for b in levels.clone() {
                    let base = rgb(r, g, b);
                    let shades = shades(base);
                    assert!(
                        shades.windows(2).all(|w| (w[0] == w[1]
                            && (w[0] == Rgb::WHITE || w[0] == Rgb::BLACK))
                            || (w[0] != w[1] && w[0].lightness() < w[1].lightness())),
                        "{base:?}: {shades:?}"
                    );
                }
            }
        }
    }

    /// A one-level nudge to a base moves no ramp step a JND further, past rounding.
    fn assert_ramp_moves_continuously(from: Rgb, to: Rgb) {
        let moved = |a: Rgb, b: Rgb| a.to_oklab().distance(b.to_oklab());
        for (a, b) in shades(from).into_iter().zip(shades(to)) {
            let rounding = [a.r.abs_diff(b.r), a.g.abs_diff(b.g), a.b.abs_diff(b.b)]
                .iter()
                .all(|&d| d <= 1);
            assert!(
                rounding || moved(a, b) - moved(from, to) < GAMUT_JND,
                "{from:?} -> {a:?} but {to:?} -> {b:?}"
            );
        }
    }

    #[test]
    fn a_ramp_step_has_no_cliff_where_its_chroma_path_grazes_the_gamut() {
        for b in 240..u8::MAX {
            assert_ramp_moves_continuously(rgb(11, 86, b), rgb(11, 86, b + 1));
        }
    }

    #[test]
    fn every_ramp_step_moves_continuously_with_its_base() {
        let levels = (0..u8::MAX).step_by(16);
        for r in levels.clone() {
            for g in levels.clone() {
                for b in levels.clone() {
                    let base = rgb(r, g, b);
                    for next in [rgb(r + 1, g, b), rgb(r, g + 1, b), rgb(r, g, b + 1)] {
                        assert_ramp_moves_continuously(base, next);
                    }
                }
            }
        }
    }

    #[test]
    fn a_ramp_step_grazing_the_gamut_by_float_noise_keeps_its_chroma() {
        assert_eq!(
            rgb(11, 86, 249).ramp(-3),
            rgb(0, 0, 207),
            "not (0, 39, 185), where a strict bounds check stops the chroma search"
        );
    }

    #[test]
    fn an_srgb_lattice_survives_an_oklab_round_trip() {
        let levels = (0..=255u8).step_by(15);
        for r in levels.clone() {
            for g in levels.clone() {
                for b in levels.clone() {
                    let c = rgb(r, g, b);
                    assert_eq!(Rgb::from_oklab_in_gamut(c.to_oklab()), c);
                }
            }
        }
    }

    #[test]
    fn a_write_is_noted_in_its_epoch_even_in_the_colour_already_there() {
        let grey = rgb(128, 128, 128);
        let mut buf = RgbBuffer::filled(4, 2, grey);
        let first = buf.begin_writes();
        buf.put(1, 0, grey);
        buf.put_checked(9, 9, grey);
        assert!(buf.written_in(1, 0, first));
        assert!(!buf.written_in(2, 0, first));
        assert!(!buf.written_in(9, 9, first));
        let second = buf.begin_writes();
        assert!(
            !buf.written_in(1, 0, second),
            "an epoch sees only its own writes"
        );
        buf.put(3, 1, grey);
        assert!(buf.written_in(3, 1, second));
    }

    #[test]
    fn a_mix_runs_from_one_color_to_the_other_evenly_in_lightness() {
        let (navy, amber) = (rgb(18, 26, 52), rgb(252, 215, 110));
        assert_eq!(navy.mix(amber, 0.0), navy);
        assert_eq!(navy.mix(amber, 1.0), amber);
        assert_eq!(navy.mix(amber, -1.0), navy, "t clamps below");
        assert_eq!(navy.mix(amber, 2.0), amber, "t clamps above");
        let l = |t: f32| navy.mix(amber, t).lightness();
        let (l0, l1) = (l(0.0), l(1.0));
        for t in [0.25, 0.5, 0.75] {
            let even = l0 + (l1 - l0) * t;
            assert!((l(t) - even).abs() < 0.01, "t={t}: {} vs {even}", l(t));
        }
    }

    #[test]
    fn lightness_runs_from_black_to_white_as_the_eye_sees_it() {
        assert!(rgb(0, 0, 0).lightness().abs() < 1e-4);
        assert!((rgb(255, 255, 255).lightness() - 1.0).abs() < 1e-4);
        assert!(
            rgb(0, 0, 255).lightness() < rgb(0, 160, 0).lightness(),
            "perceived, not a channel sum: pure blue is darker than a dimmer green"
        );
    }

    #[test]
    fn a_ramp_warms_its_lights_and_cools_its_shadows_even_on_a_grey() {
        let grey = rgb(128, 128, 128);
        let (lit, shaded) = (grey.ramp(1), grey.ramp(-1));
        assert!(lit.r > lit.b, "lit grey should lean warm: {lit:?}");
        assert!(
            shaded.b > shaded.r,
            "shaded grey should lean cool: {shaded:?}"
        );
    }

    /// Blue to green leaves the gamut mid-way, where a plain clip turns the hue.
    #[test]
    fn a_mix_that_leaves_the_gamut_keeps_its_hue() {
        let (blue, green) = (rgb(0, 0, 255), rgb(0, 255, 0));
        let (from, to) = (blue.to_oklab(), green.to_oklab());
        let off_hue = |c: Oklab, of: Oklab| {
            let lch = Oklch::from_color_unclamped(c);
            let on_hue = Oklch::new(lch.l, lch.chroma, Oklch::from_color_unclamped(of).hue);
            c.distance(Oklab::from_color_unclamped(on_hue))
        };
        for t in [0.2, 0.3] {
            let lerped = from.mix(to, t);
            let clipped =
                Oklab::from_color_unclamped(LinSrgb::from_color_unclamped(lerped).clamp());
            assert!(
                off_hue(clipped, lerped) >= GAMUT_JND,
                "t={t}: clipping must turn the hue, or this checks nothing"
            );
            let got = off_hue(blue.mix(green, t).to_oklab(), lerped);
            assert!(got < GAMUT_JND, "t={t}: {got} off the hue");
        }
    }

    #[test]
    fn palette_ramp_derives_from_its_base_and_follows_an_override() {
        let hair = rgb(40, 20, 10);
        let mut p = Palette::new();
        p.insert('H', Some(hair));
        p.insert_ramp('h', 'H', -1);
        assert_eq!(p.get('h'), Some(Some(hair.ramp(-1))));

        let blond = rgb(200, 160, 80);
        p.insert('H', Some(blond));
        assert_eq!(p.get('h'), Some(Some(blond.ramp(-1))));

        let picked = rgb(90, 40, 60);
        p.insert('h', Some(picked));
        assert_eq!(p.get('h'), Some(Some(picked)), "a hand-picked shade wins");
    }

    #[test]
    fn a_ramp_of_a_ramp_or_of_a_missing_key_is_undefined() {
        let mut p = Palette::new();
        p.insert('H', Some(rgb(40, 20, 10)));
        p.insert_ramp('h', 'H', -1);
        p.insert_ramp('x', 'h', -1);
        p.insert_ramp('y', 'Q', 1);
        assert_eq!(p.get('x'), None, "a ramp of a ramp");
        assert_eq!(p.get('y'), None, "a ramp of a missing key");
        assert_eq!(p.drawable_index('x'), None);
        assert_eq!(p.drawable_index('y'), None);
        assert_eq!(p.drawable_index('h'), Some(1));
    }

    #[test]
    fn palette_insert_replaces_a_keys_color_in_place() {
        let mut p = Palette::new();
        p.insert('B', Some(rgb(0, 0, 255)));
        p.insert('X', Some(rgb(9, 9, 9)));
        p.insert('B', Some(rgb(255, 0, 0)));
        assert_eq!(p.get('B'), Some(Some(rgb(255, 0, 0))));
        assert_eq!(p.index_of('B'), Some(0));
        assert_eq!(p.resolved().len(), 2);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "out of bounds")]
    fn rgbbuffer_get_out_of_bounds_panics_in_debug() {
        let b = RgbBuffer::filled(4, 4, Rgb { r: 0, g: 0, b: 0 });
        let _ = b.get(4, 0);
    }

    #[test]
    fn mirror_horizontal_reverses_each_row() {
        let f = Frame::from_pixels(
            3,
            2,
            vec![
                Some(Rgb { r: 1, g: 0, b: 0 }),
                None,
                Some(Rgb { r: 2, g: 0, b: 0 }),
                Some(Rgb { r: 3, g: 0, b: 0 }),
                Some(Rgb { r: 4, g: 0, b: 0 }),
                None,
            ],
        );
        let m = f.mirror_horizontal();
        assert_eq!(m.width, 3);
        assert_eq!(m.height, 2);
        assert_eq!(
            m.as_slice(),
            vec![
                Some(Rgb { r: 2, g: 0, b: 0 }),
                None,
                Some(Rgb { r: 1, g: 0, b: 0 }),
                None,
                Some(Rgb { r: 4, g: 0, b: 0 }),
                Some(Rgb { r: 3, g: 0, b: 0 }),
            ]
        );
    }

    #[test]
    fn rgb_buffer_put_get_roundtrip() {
        let mut b = RgbBuffer::filled(3, 2, Rgb { r: 0, g: 0, b: 0 });
        b.put(
            1,
            1,
            Rgb {
                r: 10,
                g: 20,
                b: 30,
            },
        );
        assert_eq!(
            b.get(1, 1),
            Rgb {
                r: 10,
                g: 20,
                b: 30
            }
        );
        assert_eq!(b.get(0, 0), Rgb { r: 0, g: 0, b: 0 });
    }

    #[test]
    fn put_checked_writes_in_bounds_and_noops_out_of_bounds() {
        let bg = Rgb { r: 0, g: 0, b: 0 };
        let fg = Rgb { r: 9, g: 8, b: 7 };
        let mut b = RgbBuffer::filled(3, 2, bg);
        b.put_checked(2, 1, fg);
        assert_eq!(b.get(2, 1), fg);
        for (x, y) in [(3, 0), (0, 2), (3, 2), (99, 99)] {
            b.put_checked(x, y, fg);
        }
        for y in 0..2 {
            for x in 0..3 {
                let want = if (x, y) == (2, 1) { fg } else { bg };
                assert_eq!(b.get(x, y), want, "cell ({x},{y})");
            }
        }
    }
}
