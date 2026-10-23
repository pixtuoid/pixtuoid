//! Screen text in pixels: a [`CellGrid`] painted cell by cell in a [`Face`],
//! for the painters a terminal draws no text for (the floating window).
//!
//! Two faces of Fusion Pixel (`scripts/gen-fonts.py`, licenses in
//! `fonts/`): [`Face::World`], 8px, the cutaway's own (`text.rs`), whose cell
//! is one layout column of the art, and [`Face::Screen`], 12px, which reads at
//! a screen's size. A symbol is an [`Icon`]: screen text writes its terminal
//! glyph, which the screen face draws as the icon's art from the pack.

use crate::pack::OfficeArt;
use pixtuoid_core::sprite::{Frame, Rgb, RgbBuffer};

use super::text::{self, LEFTMOST_PIXEL as WORLD_LEFTMOST, Rows};
use crate::display::Icon;
use crate::display::cells::{CellGrid, CellPx};
use crate::display::text::{ADVANCE, LINE_H, clusters};

/// A glyph in either face: rows of ink from the cell's top, the high bit its
/// leftmost pixel, [`SCREEN_LINE_H`] rows deep.
type Bitmap = [u16; SCREEN_LINE_H as usize];
const LEFTMOST: u16 = 1 << (u16::BITS - 1);

/// The screen face's cell, in its pixels.
const SCREEN_CELL_W: u16 = 6;
const SCREEN_LINE_H: u16 = 12;
/// Its capitals' top row and height.
const SCREEN_CAP_TOP: u16 = 2;
const SCREEN_CAP_H: u16 = 8;

/// The screen face, written by `scripts/gen-fonts.py`: a byte, the
/// rows per glyph; a u16 LE count n; n u16 LE code points, ascending; n
/// glyphs of u16 LE rows.
#[cfg(feature = "cutaway-assets")]
static SCREEN: &[u8] = include_bytes!("../../fonts/screen.bin");
#[cfg(not(feature = "cutaway-assets"))]
static SCREEN: &[u8] = &[];

/// Which glyphs a grid draws in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    /// Fusion Pixel 8px, the cutaway's own, in a cell one layout column of
    /// the art wide.
    World,
    /// Fusion Pixel 12px, for text that reads at a screen's size.
    Screen,
}

impl Face {
    /// Its cell, one glyph pixel to a pixel.
    fn unit(self) -> CellPx {
        match self {
            Self::World => CellPx {
                w: ADVANCE,
                h: LINE_H,
            },
            Self::Screen => CellPx {
                w: SCREEN_CELL_W,
                h: SCREEN_LINE_H,
            },
        }
    }

    /// The cell that draws a glyph pixel `scale` pixels square.
    pub fn cell(self, scale: u16) -> CellPx {
        let unit = self.unit();
        CellPx {
            w: unit.w * scale,
            h: unit.h * scale,
        }
    }

    /// The whole-number scale its glyphs draw at to fit `cell`, at least 1:
    /// pixel art scales by whole numbers only (Godot, "Multiple
    /// resolutions": stretch scale mode `integer`).
    pub(crate) fn scale(self, cell: CellPx) -> u16 {
        let unit = self.unit();
        (cell.w / unit.w).min(cell.h / unit.h).max(1)
    }

    /// The scale its glyphs draw at in a cell `w` pixels wide, at least 1.
    pub(crate) fn scale_across(self, w: u16) -> u16 {
        (w / self.unit().w).max(1)
    }

    /// The cell the window's footer and tooltips draw in over `fit`, in
    /// [`Face::Screen`]: a glyph pixel the badges' (a layout unit holds a
    /// world cell).
    pub fn chrome(fit: crate::render_scale::PixelFit) -> CellPx {
        Self::Screen.cell(Self::World.scale_across(fit.scale().get()))
    }

    /// Whether it draws `c`, rather than tofu.
    pub fn draws(self, c: char, pack: &OfficeArt) -> bool {
        self.glyph(c).is_some() || self.icon(c.encode_utf8(&mut [0; 4]), pack).is_some()
    }

    /// The art of the icon whose terminal glyph `symbol` is, where this face
    /// draws one: only the screen face reads symbols as icons, the world
    /// face's text naming its icons ([`Content::Icon`](crate::display::Content::Icon)).
    fn icon<'p>(
        self,
        symbol: &str,
        pack: &'p OfficeArt,
    ) -> Option<&'p pixtuoid_core::sprite::Sprite> {
        if self != Self::Screen {
            return None;
        }
        <Icon as strum::VariantArray>::VARIANTS
            .iter()
            .filter(|i| i.terminal() == symbol)
            .find_map(|&i| pack.icon(i).screen())
    }

    fn glyph(self, c: char) -> Option<Bitmap> {
        match self {
            Self::World => text::glyph(c).map(widen),
            Self::Screen => text::ruled(c).map(widen).or_else(|| screen_font(c)),
        }
    }

    /// A box `n` cells wide and a capital high, for a character it lacks.
    fn tofu(self, n: u16) -> Bitmap {
        let (top, h) = match self {
            Self::World => (
                crate::display::text::ACCENT_ROWS,
                crate::display::text::CAP_H,
            ),
            Self::Screen => (SCREEN_CAP_TOP, SCREEN_CAP_H),
        };
        let w = self.unit().w * n - 1;
        let ink = !u16::MAX.checked_shr(u32::from(w)).unwrap_or(0);
        let mut rows = Bitmap::default();
        for row in rows.iter_mut().skip(usize::from(top)).take(usize::from(h)) {
            *row = ink;
        }
        rows
    }
}

/// The world face's rows, in a [`Bitmap`].
fn widen(rows: Rows) -> Bitmap {
    let mut out = Bitmap::default();
    for (o, r) in out.iter_mut().zip(rows) {
        *o = u16::from(r) << (u16::BITS - u8::BITS);
    }
    debug_assert_eq!(u16::from(WORLD_LEFTMOST) << 8, LEFTMOST);
    out
}

/// `c`'s glyph in the generated screen face, if it has one.
fn screen_font(c: char) -> Option<Bitmap> {
    let (&rows, rest) = SCREEN.split_first()?;
    if u16::from(rows) != SCREEN_LINE_H {
        return None;
    }
    let (n, rest) = rest.split_first_chunk::<2>()?;
    let n = usize::from(u16::from_le_bytes(*n));
    let (points, glyphs) = rest.split_at_checked(n * size_of::<u16>())?;
    let point = u16::try_from(u32::from(c)).ok()?;
    let i = points
        .as_chunks::<2>()
        .0
        .binary_search_by_key(&point, |b| u16::from_le_bytes(*b))
        .ok()?;
    let stride = usize::from(SCREEN_LINE_H) * size_of::<u16>();
    let glyph = glyphs.get(i * stride..(i + 1) * stride)?;
    let mut out = Bitmap::default();
    for (o, b) in out.iter_mut().zip(glyph.as_chunks::<2>().0) {
        *o = u16::from_le_bytes(*b);
    }
    Some(out)
}

/// The pixels a grid of screen text paints into; a read or write off it is
/// a no-op.
pub trait Canvas {
    fn pixel(&self, x: i32, y: i32) -> Option<Rgb>;
    fn set(&mut self, x: i32, y: i32, rgb: Rgb);
    /// Darken `(x, y)` toward black by `factor`, as a card's drop shadow
    /// does: a canvas with nothing under a pixel overrides it.
    fn shade(&mut self, x: i32, y: i32, factor: f32) {
        if let Some(under) = self.pixel(x, y) {
            self.set(x, y, crate::look::Dim::from_brightness(factor).of(under));
        }
    }
}

impl Canvas for RgbBuffer {
    fn pixel(&self, x: i32, y: i32) -> Option<Rgb> {
        let (x, y) = (u16::try_from(x).ok()?, u16::try_from(y).ok()?);
        (x < self.width() && y < self.height()).then(|| self.get(x, y))
    }

    fn set(&mut self, x: i32, y: i32, rgb: Rgb) {
        if let (Ok(x), Ok(y)) = (u16::try_from(x), u16::try_from(y)) {
            self.put_checked(x, y, rgb);
        }
    }
}

/// How [`paint_grid`] inks what a grid leaves to the painter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridInk {
    /// The text colour of a cell with no ink of its own.
    pub text: Rgb,
    /// A one-pixel drop shadow under every glyph, for text over the office.
    pub halo: Option<Rgb>,
    /// How far a card's drop shadow darkens what is under it, a cell right
    /// and half a cell down, where the terminal's half-block shadow lands;
    /// `None` casts none.
    pub shadow: Option<f32>,
}

/// Paint `grid` in `face` with its top-left at pixel `at`, each cell `cell`
/// big, its icons in `pack`'s art: every cell's fill, then each glyph centred
/// at `Face::scale`, or
/// stretched to the whole cell for box-drawing lines and block elements so
/// they join; a bold one struck twice a pixel apart, as a terminal without a
/// bold face does (xterm(1): "the bold font will be produced by
/// overstriking").
pub fn paint_grid(
    canvas: &mut impl Canvas,
    grid: &CellGrid,
    (at, cell): ((i32, i32), CellPx),
    (face, pack): (Face, &OfficeArt),
    ink: GridInk,
) {
    let (cw, ch) = (i32::from(cell.w), i32::from(cell.h));
    if let Some(factor) = ink.shadow {
        let (w, h) = (i32::from(grid.width()) * cw, i32::from(grid.height()) * ch);
        for y in at.1 + ch / 2..at.1 + ch / 2 + h {
            for x in at.0 + cw..at.0 + cw + w {
                canvas.shade(x, y, factor);
            }
        }
    }
    let cells = || {
        (0..grid.height()).flat_map(move |gy| {
            (0..grid.width()).filter_map(move |gx| {
                let c = grid.get(gx, gy)?;
                Some((c, (at.0 + i32::from(gx) * cw, at.1 + i32::from(gy) * ch)))
            })
        })
    };
    // Every fill before any glyph: a wide glyph, a halo and a bold strike
    // reach into the next cell, whose fill would cover them.
    for (c, (x, y)) in cells() {
        if let Some(bg) = c.bg {
            for py in y..y + ch {
                for px in x..x + cw {
                    canvas.set(px, py, bg);
                }
            }
        }
    }
    for (c, (x, y)) in cells() {
        let strikes: &[i32] = if c.bold { &[0, 1] } else { &[0] };
        if let Some(halo) = ink.halo {
            for &dx in strikes {
                let at = ((x + dx + 1, y + 1), cell);
                paint_cell_glyph(canvas, &c.symbol, at, (face, pack), Stroke::Shadow(halo));
            }
        }
        for &dx in strikes {
            let fg = c.fg.unwrap_or(ink.text);
            let at = ((x + dx, y), cell);
            paint_cell_glyph(canvas, &c.symbol, at, (face, pack), Stroke::Ink(fg));
        }
    }
}

/// How a glyph is struck.
#[derive(Clone, Copy)]
enum Stroke {
    /// In this ink: a glyph's ink, an icon's ink key.
    Ink(Rgb),
    /// Whole in this colour, as the shadow under it.
    Shadow(Rgb),
}

impl Stroke {
    fn rgb(self) -> Rgb {
        match self {
            Self::Ink(rgb) | Self::Shadow(rgb) => rgb,
        }
    }
}

/// `symbol`'s glyph in `face` in the cell at `at`, an icon's in `pack`'s art.
fn paint_cell_glyph(
    canvas: &mut impl Canvas,
    symbol: &str,
    (at, cell): ((i32, i32), CellPx),
    (face, pack): (Face, &OfficeArt),
    stroke: Stroke,
) {
    let ink = stroke.rgb();
    let (cw, ch) = (i32::from(cell.w), i32::from(cell.h));
    let mut chars = symbol.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && let Some(rows) = text::ruled(c)
    {
        let (w, h) = (u32::from(cell.w), u32::from(cell.h));
        for py in 0..cell.h {
            let row = rows[(u32::from(py) * u32::from(LINE_H) / h) as usize];
            for px in 0..cell.w {
                let gx = u32::from(px) * u32::from(ADVANCE) / w;
                if row & (WORLD_LEFTMOST >> gx) != 0 {
                    canvas.set(at.0 + i32::from(px), at.1 + i32::from(py), ink);
                }
            }
        }
        return;
    }
    let unit = face.unit();
    let s = i32::from(face.scale(cell));
    let top = at.1 + (ch - i32::from(unit.h) * s) / 2;
    if let Some(art) = face.icon(symbol, pack).map(|a| a.recolorable_at(0)) {
        let frame = match stroke {
            Stroke::Ink(rgb) => art.recolored(&[(crate::pack::ICON_INK_KEY, Some(rgb))]),
            Stroke::Shadow(_) => art.recolored(&[]),
        };
        let n = crate::display::text::cells(symbol);
        let left = at.0 + (cw * i32::from(n) - i32::from(unit.w * n) * s) / 2;
        paint_art(canvas, &frame, (left, top), s, stroke);
        return;
    }
    let mut left = at.0;
    let glyphs = clusters(symbol).flat_map(|(cluster, n)| {
        text::glyphs_by(cluster, n, move |c| face.glyph(c), move |k| face.tofu(k))
    });
    for (rows, n) in glyphs {
        let span = cw * i32::from(n);
        let x0 = left + (span - i32::from(unit.w * n) * s) / 2;
        for (dy, mut bits) in (0i32..).zip(rows) {
            let mut dx = 0;
            while bits != 0 {
                if bits & LEFTMOST != 0 {
                    for py in 0..s {
                        for px in 0..s {
                            canvas.set(x0 + dx * s + px, top + dy * s + py, ink);
                        }
                    }
                }
                bits <<= 1;
                dx += 1;
            }
        }
        left += span;
    }
}

/// `frame` with its top-left at `at`, each pixel `s` square: in its own
/// colours, or whole in a shadow's.
fn paint_art(canvas: &mut impl Canvas, frame: &Frame, at: (i32, i32), s: i32, stroke: Stroke) {
    for y in 0..frame.height() {
        for x in 0..frame.width() {
            let Some(Some(rgb)) = frame.get(x, y).copied() else {
                continue;
            };
            let rgb = match stroke {
                Stroke::Ink(_) => rgb,
                Stroke::Shadow(shadow) => shadow,
            };
            let (px, py) = (at.0 + i32::from(x) * s, at.1 + i32::from(y) * s);
            for dy in 0..s {
                for dx in 0..s {
                    canvas.set(px + dx, py + dy, rgb);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::cells::CellGrid;
    use std::collections::BTreeSet;

    const BG: Rgb = Rgb {
        r: 200,
        g: 200,
        b: 200,
    };
    const FG: Rgb = Rgb { r: 9, g: 9, b: 9 };
    const INK: GridInk = GridInk {
        text: FG,
        halo: None,
        shadow: None,
    };
    const VS_CODE: CellPx = CellPx { w: 8, h: 18 };

    fn painted(text: &str, bold: bool, cell: CellPx, ink: GridInk) -> RgbBuffer {
        let mut grid = CellGrid::new(crate::display::text::cells(text), 1);
        grid.put((0, 0), text, None, bold);
        let (w, h) = (grid.width() * cell.w * 2, cell.h * 2);
        let mut buf = RgbBuffer::filled(w, h, BG);
        let pack = crate::pack::test_office();
        paint_grid(&mut buf, &grid, ((0, 0), cell), (Face::World, &pack), ink);
        buf
    }

    fn inked(buf: &RgbBuffer, colour: Rgb) -> BTreeSet<(u16, u16)> {
        (0..buf.width())
            .flat_map(|x| (0..buf.height()).map(move |y| (x, y)))
            .filter(|&(x, y)| buf.get(x, y) == colour)
            .collect()
    }

    /// A glyph pixel is `scale` pixels square and the glyph is centred in
    /// its cell: VS Code's 8×18 cell draws at 2, a row of margin above.
    #[test]
    fn a_glyph_draws_whole_scaled_and_centred_in_its_cell() {
        assert_eq!(Face::World.scale(VS_CODE), 2);
        let unit = inked(&painted("H", false, Face::World.cell(1), INK), FG);
        let margin = (VS_CODE.h - LINE_H * 2) / 2;
        let want: BTreeSet<(u16, u16)> = unit
            .iter()
            .flat_map(|&(x, y)| (0..4).map(move |i| (x * 2 + i % 2, y * 2 + i / 2 + margin)))
            .collect();
        assert_eq!(inked(&painted("H", false, VS_CODE, INK), FG), want);
    }

    /// A box-drawing line runs the whole cell, however the cell divides,
    /// so a frame joins.
    #[test]
    fn a_rule_fills_its_cell_edge_to_edge() {
        let cell = CellPx { w: 9, h: 19 };
        let ink = inked(&painted("\u{2500}\u{2500}", false, cell, INK), FG);
        let row = ink.iter().map(|&(_, y)| y).min().expect("a rule draws");
        for x in 0..2 * cell.w {
            assert!(ink.contains(&(x, row)), "a gap at {x}");
        }
    }

    /// Bold strikes the glyph twice, a pixel apart.
    #[test]
    fn bold_strikes_twice_a_pixel_apart() {
        let plain = inked(&painted("I", false, VS_CODE, INK), FG);
        let bold = inked(&painted("I", true, VS_CODE, INK), FG);
        let struck: BTreeSet<_> = plain
            .iter()
            .flat_map(|&(x, y)| [(x, y), (x + 1, y)])
            .collect();
        assert_eq!(bold, struck);
    }

    /// Text over the office draws on a one-pixel shadow down and right.
    #[test]
    fn a_halo_lies_a_pixel_down_and_right_of_the_glyph() {
        let halo = Rgb { r: 1, g: 2, b: 3 };
        let buf = painted(
            "I",
            false,
            VS_CODE,
            GridInk {
                halo: Some(halo),
                ..INK
            },
        );
        let glyph = inked(&buf, FG);
        for (x, y) in inked(&buf, halo) {
            assert!(glyph.contains(&(x - 1, y - 1)), "({x}, {y}) shadows no ink");
        }
    }

    /// A wide glyph on a filled card keeps its right half: the second cell's
    /// fill lands before it.
    #[test]
    fn a_wide_glyph_on_a_card_keeps_its_second_cell() {
        let mut grid = CellGrid::new(2, 1);
        grid.fill(BG);
        grid.put((0, 0), "\u{65e5}", None, false);
        let cell = Face::Screen.cell(1);
        let mut buf = RgbBuffer::filled(2 * cell.w, cell.h, BG);
        let pack = crate::pack::test_office();
        paint_grid(&mut buf, &grid, ((0, 0), cell), (Face::Screen, &pack), INK);
        assert!(
            inked(&buf, FG).iter().any(|&(x, _)| x >= cell.w),
            "the second cell lost its ink"
        );
    }

    /// A card darkens what is a cell right and half a cell down of it,
    /// and its own fill covers the rest of the shadow.
    #[test]
    fn a_card_casts_its_shadow_a_cell_down_and_right() {
        let fill = Rgb { r: 50, g: 0, b: 0 };
        let mut grid = CellGrid::new(1, 1);
        grid.fill(fill);
        let mut buf = RgbBuffer::filled(3 * VS_CODE.w, 3 * VS_CODE.h, BG);
        let ink = GridInk {
            shadow: Some(0.5),
            ..INK
        };
        let pack = crate::pack::test_office();
        paint_grid(
            &mut buf,
            &grid,
            ((0, 0), VS_CODE),
            (Face::World, &pack),
            ink,
        );
        assert_eq!(buf.get(0, 0), fill);
        assert_eq!(
            buf.get(VS_CODE.w, VS_CODE.h / 2),
            crate::look::Dim::from_brightness(0.5).of(BG)
        );
        assert_eq!(buf.get(VS_CODE.w, VS_CODE.h / 2 - 1), BG);
        assert_eq!(buf.get(2 * VS_CODE.w, 2 * VS_CODE.h), BG);
    }

    /// A symbol draws as its icon in the screen face, in its text's ink and
    /// inside the cells a terminal gives it, though Fusion Pixel draws it
    /// full-width.
    #[test]
    fn a_symbol_draws_in_the_one_cell_a_terminal_gives_it() {
        let cell = Face::Screen.cell(2);
        let ink = inked(&painted_in("\u{25cf}", Face::Screen, cell), FG);
        assert!(!ink.is_empty());
        assert!(ink.iter().all(|&(x, _)| x < cell.w), "{ink:?}");
    }

    /// The screen face draws ASCII and CJK from Fusion Pixel 12px, each as
    /// wide as its cells.
    #[cfg(feature = "cutaway-assets")]
    #[test]
    fn the_screen_face_draws_latin_and_cjk_in_their_cells() {
        let pack = crate::pack::test_office();
        for c in ['M', 'g', '\u{5c0f}', '\u{660e}', '\u{d55c}', '\u{416}'] {
            assert!(Face::Screen.draws(c, &pack), "{c:?}");
        }
        let cell = Face::Screen.cell(1);
        let wide = inked(&painted_in("\u{5c0f}", Face::Screen, cell), FG);
        assert!(
            wide.iter().any(|&(x, _)| x >= cell.w),
            "a CJK glyph spans two cells"
        );
        assert!(wide.iter().all(|&(x, _)| x < 2 * cell.w));
    }

    /// Each icon's screen art is a screen cell wide less the gap per cell its
    /// terminal glyph takes, a line tall; and no two icons screen text reads
    /// by glyph share one, or the screen face could not tell them apart.
    #[test]
    fn every_screen_icon_fits_its_cells_and_owns_its_glyph() {
        let pack = crate::pack::test_office();
        let cell = Face::Screen.cell(1);
        let mut seen = std::collections::HashMap::new();
        for icon in <crate::display::Icon as strum::VariantArray>::VARIANTS {
            let Some(art) = pack.icon(*icon).screen().map(|s| s.first()) else {
                continue;
            };
            let n = crate::display::text::cells(icon.terminal());
            assert_eq!(
                (art.width(), art.height()),
                (cell.w * n - 1, cell.h),
                "{icon:?}"
            );
            if let Some(other) = seen.insert(icon.terminal(), icon) {
                panic!("{icon:?} and {other:?} both draw {:?}", icon.terminal());
            }
        }
    }

    fn painted_in(text: &str, face: Face, cell: CellPx) -> RgbBuffer {
        let mut grid = CellGrid::new(crate::display::text::cells(text), 1);
        grid.put((0, 0), text, None, false);
        let mut buf = RgbBuffer::filled(grid.width() * cell.w * 2, cell.h * 2, BG);
        let pack = crate::pack::test_office();
        paint_grid(&mut buf, &grid, ((0, 0), cell), (face, &pack), INK);
        buf
    }

    /// A card's shadow reaches the canvas through `shade`, over the card's
    /// extent shifted a cell right and half a cell down, so a canvas with
    /// nothing under it can still draw one.
    #[test]
    fn a_cards_shadow_shades_through_the_canvas() {
        struct Shades(Vec<(i32, i32)>);
        impl Canvas for Shades {
            fn pixel(&self, _: i32, _: i32) -> Option<Rgb> {
                None
            }
            fn set(&mut self, _: i32, _: i32, _: Rgb) {}
            fn shade(&mut self, x: i32, y: i32, _: f32) {
                self.0.push((x, y));
            }
        }
        let pack = crate::pack::test_office();
        let card = CellGrid::new(2, 1);
        let cell = Face::Screen.cell(1);
        let mut shades = Shades(Vec::new());
        let ink = GridInk {
            shadow: Some(crate::display::cells::CARD_SHADOW),
            ..INK
        };
        paint_grid(
            &mut shades,
            &card,
            ((0, 0), cell),
            (Face::Screen, &pack),
            ink,
        );
        let (cw, ch) = (i32::from(cell.w), i32::from(cell.h));
        assert_eq!(shades.0.len(), usize::try_from(2 * cw * ch).expect("small"));
        assert_eq!(shades.0[0], (cw, ch / 2), "a cell right, half a cell down");
    }

    /// The default `shade` darkens what is there, as the shadow always has.
    #[test]
    fn the_default_shade_darkens_what_is_under_it() {
        let mut buf = RgbBuffer::filled(
            1,
            1,
            Rgb {
                r: 100,
                g: 50,
                b: 10,
            },
        );
        buf.shade(0, 0, 0.5);
        assert_eq!(buf.get(0, 0), Rgb { r: 50, g: 25, b: 5 });
    }
}
