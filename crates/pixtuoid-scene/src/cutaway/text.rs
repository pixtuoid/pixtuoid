//! The cutaway's pixel font: text painted on the art grid, one glyph pixel per
//! art pixel, never anti-aliased, in the cells [`display::text`](crate::display::text)
//! lays it out by.
//!
//! Box-drawing lines and block elements are [`ruled`], and every character
//! Fusion Pixel 8px draws as a terminal's cells comes from it
//! (`scripts/gen-fonts.py`); a symbol is an icon, not a glyph
//! ([`paint_icon`]). [`grid`](super::grid) paints screen text in it too.

use pixtuoid_core::sprite::{Rgb, RgbBuffer};

use crate::display::pen::{ArtPx, ArtRect, Pen};
use crate::display::text::{
    ACCENT_ROWS, ADVANCE, CAP_H, GLYPH_W, LINE_H, cells, clusters, columns,
};

/// A glyph: each line row's ink, the high bit its leftmost pixel.
pub(crate) type Rows = [u8; LINE_H as usize];
pub(crate) const LEFTMOST_PIXEL: u8 = 1 << (u8::BITS - 1);

/// Fusion Pixel 8px, written by `scripts/gen-fonts.py` (its format);
/// license and sources in `fonts/`.
#[cfg(feature = "pixel-font")]
static FUSION: &[u8] = include_bytes!("../../fonts/fusion-pixel-8px.bin");
#[cfg(not(feature = "pixel-font"))]
static FUSION: &[u8] = &[];

/// Fusion Pixel's code points, ascending, and their glyphs; `None` when it
/// was generated for another [`LINE_H`].
fn fusion_font() -> Option<(&'static [[u8; 2]], &'static [Rows])> {
    let (&[rows, n_lo, n_hi], rest) = FUSION.split_first_chunk::<3>()?;
    if u16::from(rows) != LINE_H {
        return None;
    }
    let n = usize::from(u16::from_le_bytes([n_lo, n_hi]));
    let (points, glyphs) = rest.split_at_checked(n * size_of::<u16>())?;
    Some((points.as_chunks().0, glyphs.as_chunks().0))
}

/// `c`'s glyph in Fusion Pixel, if it draws one in the cells a terminal
/// gives it.
fn fusion(c: char) -> Option<Rows> {
    let (points, glyphs) = fusion_font()?;
    let point = u16::try_from(u32::from(c)).ok()?;
    let i = points
        .binary_search_by_key(&point, |b| u16::from_le_bytes(*b))
        .ok()?;
    glyphs.get(i).copied()
}

/// `c`'s glyph, `None` when the face lacks it.
pub(crate) fn glyph(c: char) -> Option<Rows> {
    ruled(c).or_else(|| fusion(c)).or_else(|| by_hand(c))
}

/// `c`'s glyph where Fusion Pixel 8px has none that fits its cell: the narrow
/// glyph it reads as where [`GLYPH_W`] columns can't tell them apart (`“`→`"`,
/// `—`→`-`), else one drawn here.
fn by_hand(c: char) -> Option<Rows> {
    match c {
        '\u{201c}' | '\u{201d}' | '\u{2033}' => return fusion('"'),
        '\u{2010}' | '\u{2011}' | '\u{2014}' | '\u{2015}' => return fusion('-'),
        '\u{2022}' => return fusion('\u{b7}'),
        _ => {}
    }
    // A glyph's columns, leftmost the high bit, from the line's top row.
    let art: [u8; LINE_H as usize] = match c {
        // The font's `’` turned over.
        '\u{2018}' => [0, 0b010, 0b100, 0b110, 0, 0, 0, 0],
        // Three dots take five columns: two keep the glyph's gap.
        '\u{2026}' => [0, 0, 0, 0, 0, 0, 0b101, 0],
        '\u{3a3}' => [0, 0b111, 0b100, 0b010, 0b010, 0b100, 0b111, 0],
        '\u{d8}' => [0, 0b011, 0b101, 0b101, 0b101, 0b101, 0b110, 0],
        '\u{e6}' => [0, 0, 0, 0b011, 0b111, 0b110, 0b011, 0],
        '\u{f8}' => [0, 0, 0, 0b011, 0b101, 0b101, 0b110, 0],
        _ => return None,
    };
    Some(art.map(|ink| ink << (u8::BITS - u32::from(GLYPH_W))))
}

/// What `cluster`'s `n` cells show, each glyph with the cells it takes. When
/// its characters' own cells add up to `n`, each draws in its own: a letter
/// under a combining accent, or a letter and a halfwidth sound mark. Otherwise,
/// as with a VS16 heart or a ZWJ sequence, it is one box `n` cells wide.
pub(crate) fn glyphs(cluster: &str, n: u16) -> impl Iterator<Item = (Rows, u16)> + '_ {
    let own = |c: char| cells(c.encode_utf8(&mut [0; 4]));
    let fits = cluster.chars().map(own).sum::<u16>() == n;
    let each = cluster
        .chars()
        .filter(move |_| fits)
        .map(move |c| (c, own(c)))
        .filter(|&(_, k)| k > 0)
        .map(move |(c, k)| (glyph(c).unwrap_or_else(|| tofu(k)), k));
    each.chain((!fits).then(|| (tofu(n), n)))
}

/// What a character neither font draws is: a solid box `n` cells wide and a
/// capital high, so a run never collapses.
fn tofu(n: u16) -> Rows {
    let ink = !u8::MAX
        .checked_shr(u32::from(columns(n).0.saturating_sub(1)))
        .unwrap_or(0);
    let mut rows = Rows::default();
    let capitals = rows.iter_mut().skip(usize::from(ACCENT_ROWS));
    for row in capitals.take(usize::from(CAP_H)) {
        *row = ink;
    }
    rows
}

/// The art row a box-drawing line runs along: the capitals' middle.
const RULE_ROW: u16 = ACCENT_ROWS + CAP_H / 2;
/// The art column a box-drawing line runs down: the glyph's middle.
const RULE_COL: u16 = GLYPH_W / 2;

/// `c`'s glyph when it is a box-drawing line or a block element: computed, not
/// taken from a font, to fill the whole cell so neighbours join. The idea is
/// WezTerm's for the same blocks (wezterm/wezterm `docs/config/lua/config/custom_block_glyphs.md`:
/// "its own idea of what the glyphs … should be"); the geometry is ours.
pub(crate) fn ruled(c: char) -> Option<Rows> {
    const FULL: std::ops::Range<u16> = 0..ADVANCE;
    const TALL: std::ops::Range<u16> = 0..LINE_H;
    // Unicode's partial blocks come in eighths of the cell.
    const EIGHTHS: u16 = 8;
    let (half_w, half_h) = (ADVANCE / 2, LINE_H / 2);
    let mut rows = Rows::default();
    let mut ink =
        |xs: std::ops::Range<u16>, ys: std::ops::Range<u16>, on: &dyn Fn(u16, u16) -> bool| {
            for y in ys {
                for x in xs.clone() {
                    if on(x, y)
                        && let Some(row) = rows.get_mut(usize::from(y))
                    {
                        *row |= LEFTMOST_PIXEL >> x;
                    }
                }
            }
        };
    let solid = &|_, _| true;
    let code = u32::from(c);
    match c {
        // Light lines and rounded corners, as the arms they reach: left,
        // right, up, down.
        '\u{2500}'..='\u{257f}' => {
            let [left, right, up, down] = match c {
                '\u{2500}' => [true, true, false, false],
                '\u{2502}' => [false, false, true, true],
                '\u{250c}' | '\u{256d}' => [false, true, false, true],
                '\u{2510}' | '\u{256e}' => [true, false, false, true],
                '\u{2514}' | '\u{2570}' => [false, true, true, false],
                '\u{2518}' | '\u{256f}' => [true, false, true, false],
                '\u{251c}' => [false, true, true, true],
                '\u{2524}' => [true, false, true, true],
                '\u{252c}' => [true, true, false, true],
                '\u{2534}' => [true, true, true, false],
                '\u{253c}' => [true, true, true, true],
                '\u{2574}' => [true, false, false, false],
                '\u{2575}' => [false, false, true, false],
                '\u{2576}' => [false, true, false, false],
                '\u{2577}' => [false, false, false, true],
                _ => return None,
            };
            let row = RULE_ROW..RULE_ROW + 1;
            let col = RULE_COL..RULE_COL + 1;
            if left {
                ink(0..RULE_COL + 1, row.clone(), solid);
            }
            if right {
                ink(RULE_COL..ADVANCE, row, solid);
            }
            if up {
                ink(col.clone(), 0..RULE_ROW + 1, solid);
            }
            if down {
                ink(col, RULE_ROW..LINE_H, solid);
            }
        }
        '\u{2580}' => ink(FULL, 0..half_h, solid),
        // Lower one to eight eighths.
        '\u{2581}'..='\u{2588}' => {
            let eighths = u16::try_from(code - u32::from('\u{2580}')).ok()?;
            ink(FULL, LINE_H - eighths * LINE_H / EIGHTHS..LINE_H, solid);
        }
        // Left seven eighths down to one, at least a pixel.
        '\u{2589}'..='\u{258f}' => {
            let eighths = EIGHTHS - u16::try_from(code - u32::from('\u{2588}')).ok()?;
            ink(0..(eighths * ADVANCE / EIGHTHS).max(1), TALL, solid);
        }
        '\u{2590}' => ink(half_w..ADVANCE, TALL, solid),
        '\u{2591}' => ink(FULL, TALL, &|x, y| x % 2 == 0 && y % 2 == 0),
        '\u{2592}' => ink(FULL, TALL, &|x, y| (x + y) % 2 == 0),
        '\u{2593}' => ink(FULL, TALL, &|x, y| x % 2 == 0 || y % 2 == 0),
        '\u{2594}' => ink(FULL, 0..1, solid),
        '\u{2595}' => ink(ADVANCE - 1..ADVANCE, TALL, solid),
        // Quadrants, as which of upper-left, upper-right, lower-left and
        // lower-right they fill.
        '\u{2596}'..='\u{259f}' => {
            let [ul, ur, ll, lr] = match c {
                '\u{2596}' => [false, false, true, false],
                '\u{2597}' => [false, false, false, true],
                '\u{2598}' => [true, false, false, false],
                '\u{2599}' => [true, false, true, true],
                '\u{259a}' => [true, false, false, true],
                '\u{259b}' => [true, true, true, false],
                '\u{259c}' => [true, true, false, true],
                '\u{259d}' => [false, true, false, false],
                '\u{259e}' => [false, true, true, false],
                _ => [false, true, true, true],
            };
            let (left, right) = (0..half_w, half_w..ADVANCE);
            let (top, bottom) = (0..half_h, half_h..LINE_H);
            for (on, xs, ys) in [
                (ul, left.clone(), top.clone()),
                (ur, right.clone(), top),
                (ll, left, bottom.clone()),
                (lr, right, bottom),
            ] {
                if on {
                    ink(xs, ys, solid);
                }
            }
        }
        _ => return None,
    }
    Some(rows)
}

/// Paint `text` in `ink` from its top-left `(x, y)`, clipped to the buffer.
pub(crate) fn paint(pen: Pen, buf: &mut RgbBuffer, (x, y): (ArtPx, ArtPx), text: &str, ink: Rgb) {
    let mut left = x.0;
    for (rows, n) in clusters(text).flat_map(|(cluster, n)| glyphs(cluster, n)) {
        for (dy, mut bits) in (0u16..).zip(rows) {
            let mut dx = 0;
            while bits != 0 {
                if bits & LEFTMOST_PIXEL != 0 {
                    let at = ArtRect {
                        x: ArtPx(left.saturating_add(dx)),
                        y: ArtPx(y.0.saturating_add(dy)),
                        w: ArtPx(1),
                        h: ArtPx(1),
                    };
                    pen.fill(buf, at, ink);
                }
                bits <<= 1;
                dx += 1;
            }
        }
        left = left.saturating_add(columns(n).0);
    }
}

/// `icon`'s art from `pack` in the cell whose top-left is `(x, y)`, its
/// [`ICON_INK_KEY`](crate::pack::ICON_INK_KEY) pixels in `ink`.
pub(crate) fn paint_icon(
    pen: Pen,
    buf: &mut RgbBuffer,
    (x, y): (ArtPx, ArtPx),
    (pack, icon): (&crate::pack::OfficeArt, crate::display::Icon),
    ink: Rgb,
) {
    let frame = pack
        .icon(icon)
        .recolorable_at(0)
        .recolored(&[(crate::pack::ICON_INK_KEY, Some(ink))]);
    for dy in 0..frame.height() {
        for dx in 0..frame.width() {
            if let Some(&Some(rgb)) = frame.get(dx, dy) {
                let at = ArtRect {
                    x: ArtPx(x.0.saturating_add(dx)),
                    y: ArtPx(y.0.saturating_add(dy)),
                    w: ArtPx(1),
                    h: ArtPx(1),
                };
                pen.fill(buf, at, rgb);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::pen::test_density;
    use crate::display::text::{advance, width};

    /// Every character the wall board and the floor indicator write as text:
    /// each mood over two flap cycles, each gateway state, many floors.
    #[cfg(feature = "pixel-font")]
    fn signs() -> std::collections::BTreeSet<char> {
        use crate::anim::Motion;
        use crate::neon_sign::build_board;
        use crate::tally::StateCounts;
        use pixtuoid_core::state::DaemonState;
        let written = |content: &crate::display::Content| match content {
            crate::display::Content::Text(text) => text.clone(),
            crate::display::Content::Icon(_) => String::new(),
        };
        let mut text: String =
            crate::layout::floor_indicator(crate::floor::FloorMeta::for_floor(11, 12))
                .iter()
                .map(written)
                .collect();
        let moods = [
            StateCounts::default(),
            StateCounts {
                idle: 2,
                total: 2,
                ..StateCounts::default()
            },
            StateCounts {
                running: 1,
                total: 1,
                ..StateCounts::default()
            },
            StateCounts {
                running: 3,
                total: 3,
                ..StateCounts::default()
            },
            StateCounts {
                requires_action: 1,
                total: 1,
                ..StateCounts::default()
            },
            StateCounts {
                requires_action: 2,
                running: 3,
                idle: 4,
                total: 9,
                ..StateCounts::default()
            },
        ];
        let gateways = [
            None,
            Some(DaemonState::Idle),
            Some(DaemonState::Running),
            Some(DaemonState::Degraded),
            Some(DaemonState::Down),
        ];
        for (counts, gateway) in moods.into_iter().zip(gateways.into_iter().cycle()) {
            for ms in (0..32_000).step_by(20) {
                let now = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms);
                let board = build_board(counts, 3_700, Some((2, 3)), gateway, Motion::Full, now);
                for seg in std::iter::once(&board.brand)
                    .chain(&board.star)
                    .chain(&board.mood)
                    .chain(&board.context)
                {
                    text.push_str(&written(&seg.content));
                }
            }
        }
        text.chars().collect()
    }

    /// The font draws everything the signs write: no sign shows a tofu box.
    #[cfg(feature = "pixel-font")]
    #[test]
    fn the_font_draws_every_character_the_signs_write() {
        let missing: Vec<char> = signs()
            .into_iter()
            .filter(|&c| glyph(c).is_none())
            .collect();
        assert_eq!(missing, []);
    }

    /// Lines join their neighbours: a rule runs through every column of its
    /// cell, a stem down every row, so a frame drawn of them is unbroken.
    #[test]
    fn box_drawing_lines_join_across_cells() {
        let across = ink("\u{2500}\u{2500}");
        for x in 0..columns(2).0 {
            assert!(across.contains(&(x, RULE_ROW)), "a gap at {x}");
        }
        let down = ink("\u{2502}");
        for y in 0..LINE_H {
            assert!(down.contains(&(RULE_COL, y)), "a gap at {y}");
        }
        let corner = ink("\u{2514}");
        assert!(corner.contains(&(RULE_COL, 0)) && corner.contains(&(ADVANCE - 1, RULE_ROW)));
        assert!(
            !corner.contains(&(0, RULE_ROW)),
            "a └ reaches no further left"
        );
    }

    /// No glyph inks past the width a run is measured by: a rule that joins
    /// its neighbour reaches its cell's gap, and the width counts it.
    #[test]
    fn no_glyph_inks_past_its_width() {
        for c in '\u{2500}'..='\u{259f}' {
            let filled = crate::display::text::fills_cell(c);
            assert_eq!(
                filled,
                ruled(c).is_some(),
                "{c:?}: measured as filling its cell"
            );
        }
        let ruled_chars = ('\u{2500}'..='\u{259f}').filter(|&c| ruled(c).is_some());
        let by_hand = "\u{2018}\u{201c}\u{2014}\u{2022}\u{2026}\u{3a3}\u{d8}\u{e6}\u{f8}".chars();
        for c in ruled_chars.chain(['I', '\u{25cf}']).chain(by_hand) {
            let text = c.to_string();
            let reach = ink(&text).iter().map(|&(x, _)| x + 1).max().unwrap_or(0);
            assert!(reach <= width(&text).0, "{c:?} inks to {reach}");
        }
        assert_eq!(
            width("\u{2500}\u{2500}"),
            columns(2),
            "a rule ends at its cell's edge"
        );
    }

    /// The first and last rows `c`'s glyph inks.
    #[cfg(feature = "pixel-font")]
    fn ink_rows(c: char) -> (Option<usize>, Option<usize>) {
        let inked: Vec<usize> = (0..)
            .zip(glyph(c).expect("drawn"))
            .filter(|&(_, ink)| ink != 0)
            .map(|(row, _)| row)
            .collect();
        (inked.first().copied(), inked.last().copied())
    }

    /// [`ACCENT_ROWS`] and [`CAP_H`] are Fusion Pixel's own: its capitals
    /// span the rows they name.
    #[cfg(feature = "pixel-font")]
    #[test]
    fn the_capitals_span_the_rows_the_metrics_name() {
        let top = usize::from(ACCENT_ROWS);
        let capitals = (Some(top), Some(top + usize::from(CAP_H) - 1));
        for c in "EHIT".chars() {
            assert_eq!(ink_rows(c), capitals, "{c:?}");
        }
    }

    /// A glyph drawn by hand stands on Fusion Pixel's own rows: a capital
    /// spans its `E`'s, a lowercase letter its `o`'s.
    #[cfg(feature = "pixel-font")]
    #[test]
    fn hand_drawn_glyphs_stand_on_the_fonts_rows() {
        for c in ['\u{3a3}', '\u{d8}'] {
            assert_eq!(ink_rows(c), ink_rows('E'), "{c:?}");
        }
        for c in ['\u{e6}', '\u{f8}'] {
            assert_eq!(ink_rows(c), ink_rows('o'), "{c:?}");
        }
    }

    /// Block elements fill their share of the whole cell: a full block every
    /// pixel, the eighths one row more each from the bottom up.
    #[test]
    fn block_elements_fill_their_share_of_the_cell() {
        let all: std::collections::BTreeSet<(u16, u16)> = (0..ADVANCE)
            .flat_map(|x| (0..LINE_H).map(move |y| (x, y)))
            .collect();
        assert_eq!(ink("\u{2588}"), all);
        for (eighths, c) in (1..=8u16).zip('\u{2581}'..='\u{2588}') {
            let rows: std::collections::BTreeSet<u16> = ink(c.encode_utf8(&mut [0; 4]))
                .into_iter()
                .map(|(_, y)| y)
                .collect();
            assert_eq!(rows, (LINE_H - eighths..LINE_H).collect(), "{c:?}");
        }
        assert_eq!(ink("\u{2580}").len() + ink("\u{2584}").len(), all.len());
        assert_eq!(ink("\u{258c}").len() + ink("\u{2590}").len(), all.len());
    }

    /// One glyph pixel is one art pixel, whatever the scale: `k` buffer
    /// pixels square, all ink.
    #[test]
    fn a_glyph_pixel_is_one_art_pixel() {
        let bg = Rgb { r: 0, g: 0, b: 0 };
        let fg = Rgb { r: 9, g: 9, b: 9 };
        let unit = ink("H");
        for (s, d) in [(4u16, 4u16), (8, 4)] {
            let pen = Pen::new(
                crate::render_scale::RenderScale::new(s).expect("s"),
                test_density(d),
            )
            .expect("d divides s");
            let k = s / d;
            let side = 1 + LINE_H;
            let mut buf = RgbBuffer::filled(side * k, side * k, bg);
            paint(pen, &mut buf, (ArtPx(1), ArtPx(1)), "H", fg);
            for y in 0..side * k {
                for x in 0..side * k {
                    let art = (x / k).checked_sub(1).zip((y / k).checked_sub(1));
                    let want = if art.is_some_and(|p| unit.contains(&p)) {
                        fg
                    } else {
                        bg
                    };
                    assert_eq!(buf.get(x, y), want, "scale {s} at ({x}, {y})");
                }
            }
        }
    }

    /// Where `text` painted from the origin leaves ink, as `(x, y)` art pixels.
    fn ink(text: &str) -> std::collections::BTreeSet<(u16, u16)> {
        let (bg, fg) = (Rgb { r: 0, g: 0, b: 0 }, Rgb { r: 9, g: 9, b: 9 });
        let w = advance(text).0.max(1);
        let mut buf = RgbBuffer::filled(w, LINE_H, bg);
        paint(Pen::UNIT, &mut buf, (ArtPx(0), ArtPx(0)), text, fg);
        (0..w)
            .flat_map(|x| (0..LINE_H).map(move |y| (x, y)))
            .filter(|&(x, y)| buf.get(x, y) == fg)
            .collect()
    }

    /// Project names in CJK and accented Latin draw real glyphs,
    /// each as wide as its [`cells`](crate::display::text::cells).
    #[cfg(feature = "pixel-font")]
    #[test]
    fn names_beyond_ascii_draw_glyphs_as_wide_as_their_cells() {
        for (name, n) in [
            ("日本語", 6),
            ("项目", 4),
            ("café-api", 8),
            ("ñandú", 5),
            ("한글", 4),
        ] {
            assert_eq!(cells(name), n, "{name}");
            for c in name.chars() {
                assert!(glyph(c).is_some(), "{c:?} of {name} is tofu");
            }
        }
        let wide = ink("日");
        assert!(
            wide.iter()
                .any(|&(x, _)| x >= crate::display::text::ADVANCE),
            "a CJK glyph spans its second cell: {wide:?}"
        );
        assert_eq!(advance("日I"), columns(3));
        let after = ink("日I");
        let top = ink("I").iter().map(|&(_, y)| y).min().expect("an I draws");
        assert!(
            after.contains(&(columns(2).0, top)),
            "the I's top bar opens its third cell"
        );
    }

    /// A grapheme cluster paints inside the cells it takes, and the run after
    /// it starts there: a VS16 heart and a ZWJ sequence each take two.
    #[test]
    fn a_cluster_paints_inside_its_own_cells() {
        for cluster in ["\u{2764}\u{fe0f}", "\u{1f469}\u{200d}\u{1f4bb}"] {
            assert_eq!(cells(cluster), 2, "{cluster:?}");
            let alone = ink(cluster);
            assert!(
                !alone.is_empty() && alone.iter().all(|&(x, _)| x < columns(2).0),
                "{cluster:?} inks {alone:?}"
            );
            let after: std::collections::BTreeSet<_> = ink(&format!("{cluster}I"))
                .into_iter()
                .filter(|&(x, _)| x >= columns(2).0)
                .map(|(x, y)| (x - columns(2).0, y))
                .collect();
            assert_eq!(after, ink("I"), "{cluster:?}: the I opens the third cell");
        }
    }

    /// A halfwidth sound mark takes a cell of its own (ratatui-core's
    /// `count_halfwidth_sound_marks`), so `ｶﾞ` draws the `ｶ` in its first
    /// cell and the mark in its second, as each draws alone.
    #[test]
    fn a_halfwidth_sound_mark_draws_in_its_own_cell() {
        for (base, mark) in [('\u{ff76}', '\u{ff9e}'), ('\u{ff8a}', '\u{ff9f}')] {
            let cluster = format!("{base}{mark}");
            assert_eq!(cells(&cluster), 2, "{cluster:?}");
            let want: std::collections::BTreeSet<_> = ink(&base.to_string())
                .into_iter()
                .chain(
                    ink(&mark.to_string())
                        .into_iter()
                        .map(|(x, y)| (x + columns(1).0, y)),
                )
                .collect();
            assert_eq!(ink(&cluster), want, "{cluster:?}");
        }
    }

    /// A character the face lacks is a solid box,
    /// capital-high and its cells wide, so a run never collapses.
    #[test]
    fn a_character_no_font_draws_is_a_box_its_cells_wide() {
        // Thai, and CJK Extension A: not in Fusion Pixel's subset.
        for (c, n) in [('\u{0e01}', 1), ('\u{3400}', 2)] {
            assert_eq!(glyph(c), None, "{c:?}");
            assert_eq!(cells(c.encode_utf8(&mut [0; 4])), n, "{c:?}");
            let text = c.to_string();
            let want: std::collections::BTreeSet<_> = (0..width(&text).0)
                .flat_map(|x| (ACCENT_ROWS..ACCENT_ROWS + CAP_H).map(move |y| (x, y)))
                .collect();
            assert_eq!(ink(&text), want, "{c:?}");
        }
    }

    /// Without `pixel-font` a character only Fusion Pixel draws is the
    /// box.
    #[test]
    fn fusion_pixel_ships_with_pixel_font() {
        assert_eq!(fusion('e').is_some(), cfg!(feature = "pixel-font"));
    }

    /// Fusion Pixel's CJK reaches the descender row its Latin does.
    #[cfg(feature = "pixel-font")]
    #[test]
    fn cjk_reaches_the_descender_row() {
        let bottom = |c| glyph(c).and_then(|g| g.iter().rposition(|&row| row != 0));
        assert_eq!(
            bottom('\u{65e5}'),
            bottom('g'),
            "CJK reaches the descender row"
        );
    }

    /// Fusion Pixel's glyphs are well formed, and every one fits the cells its
    /// character takes.
    #[cfg(feature = "pixel-font")]
    #[test]
    fn every_fusion_glyph_fits_its_characters_cells() {
        let (points, glyphs) = fusion_font().expect("the header matches LINE_H");
        assert_eq!(points.len(), glyphs.len());
        let points: Vec<u16> = points.iter().map(|&b| u16::from_le_bytes(b)).collect();
        assert!(
            points.is_sorted_by(|a, b| a < b),
            "binary search needs them ascending"
        );
        let misfits: Vec<(char, u16)> = points
            .iter()
            .zip(glyphs)
            .filter_map(|(&point, rows)| {
                let c = char::from_u32(u32::from(point))?;
                let n = cells(c.encode_utf8(&mut [0; 4]));
                (!fits(rows, n)).then_some((c, n))
            })
            .collect();
        assert_eq!(misfits, []);
    }

    /// Whether `rows` fit `n` cells: ink no further right than their advance.
    /// The gap column may take ink, as Fusion Pixel's widest glyphs (`–`,
    /// `ȵ`) do: a legible glyph touching its neighbour beats a tofu box.
    fn fits(rows: &Rows, n: u16) -> bool {
        let ink = rows
            .iter()
            .map(|row| u8::BITS - row.trailing_zeros())
            .max()
            .unwrap_or(0);
        n > 0 && ink <= u32::from(columns(n).0)
    }

    /// [`fits`] refuses ink past one cell's advance and takes a glyph that
    /// fills it, at one cell and at two (where a row can hold no more).
    #[test]
    fn a_glyph_past_its_advance_does_not_fit() {
        let wide = |bits| {
            let mut rows = Rows::default();
            rows[usize::from(ACCENT_ROWS)] = bits;
            rows
        };
        assert!(!fits(&wide(0b1111_1000), 1));
        assert!(!fits(&wide(u8::MAX), 1));
        assert!(fits(&wide(0b1111_0000), 1));
        assert!(fits(&wide(u8::MAX), 2));
        assert!(!fits(&wide(0b1000_0000), 0));
    }
}
