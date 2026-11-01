//! Painting on the [`Pen`]'s art grid.

use std::ops::Range;

use crate::sprite::{Rgb, RgbBuffer};

use crate::cutaway::shade::fill;
use crate::display::pen::{ArtPx, ArtRect, BufferPx, Pen};
use crate::dither::Stepped;

impl Pen {
    /// The art columns and rows over `buf`'s writable part: all of it, or a
    /// repaint's clip. A painter that walks art pixels walks only these.
    pub(crate) fn writable_art(self, buf: &RgbBuffer) -> (Range<u16>, Range<u16>) {
        let (xs, ys) = buf.writable();
        (
            self.art_of_buffer(BufferPx(xs.start)).0..self.art_covering(BufferPx(xs.end)).0,
            self.art_of_buffer(BufferPx(ys.start)).0..self.art_covering(BufferPx(ys.end)).0,
        )
    }

    /// Copy `from`'s art pixel onto `buf` wherever `take` holds for it.
    pub(crate) fn take_where(
        self,
        buf: &mut RgbBuffer,
        from: &RgbBuffer,
        take: impl Fn(ArtPx, ArtPx) -> bool,
    ) {
        let (xs, ys) = buf.writable();
        for y in ys.start..ys.end.min(from.height()) {
            for x in xs.start..xs.end.min(from.width()) {
                if take(
                    self.art_of_buffer(BufferPx(x)),
                    self.art_of_buffer(BufferPx(y)),
                ) {
                    buf.put(x, y, from.get(x, y));
                }
            }
        }
    }

    /// Paint `r` solid, clipped to the buffer.
    pub(crate) fn fill(self, buf: &mut RgbBuffer, r: ArtRect, c: Rgb) {
        fill(
            buf,
            self.buffer(r.x).0,
            self.buffer(r.y).0,
            self.buffer(r.w).0,
            self.buffer(r.h).0,
            c,
        );
    }

    /// [`shade`](Self::shade) the lines of a `pitch` grid laid from `r`'s top
    /// left corner, within `r`: each art pixel once, so a crossing is no darker
    /// than the lines through it.
    pub(crate) fn shade_grid(self, buf: &mut RgbBuffer, r: ArtRect, pitch: ArtPx, level: i8) {
        let p = pitch.0.max(1);
        let (x1, y1) = (r.x.0.saturating_add(r.w.0), r.y.0.saturating_add(r.h.0));
        let mut stepped = Stepped::new(level);
        for y in (r.y.0..y1).step_by(usize::from(p)) {
            let row = ArtRect {
                y: ArtPx(y),
                h: ArtPx(1),
                ..r
            };
            self.shade(buf, row, &mut stepped);
            // The columns run from under this row to the next one.
            let run = ArtPx((p - 1).min(y1 - y - 1));
            for x in (r.x.0..x1).step_by(usize::from(p)) {
                let col = ArtRect {
                    x: ArtPx(x),
                    y: ArtPx(y + 1),
                    w: ArtPx(1),
                    h: run,
                };
                self.shade(buf, col, &mut stepped);
            }
        }
    }

    /// Step every pixel of `r` `stepped`'s level stops along its own ramp
    /// ([`Rgb::ramp`](crate::sprite::Rgb::ramp)), clipped to the
    /// buffer: a tone relative to what is already painted there, not a colour
    /// of its own. An art pixel that was one colour stays one.
    fn shade(self, buf: &mut RgbBuffer, r: ArtRect, stepped: &mut Stepped) {
        let (xs, ys) = buf.writable();
        let (x0, y0) = (self.buffer(r.x).0, self.buffer(r.y).0);
        let x1 = x0.saturating_add(self.buffer(r.w).0).min(xs.end);
        let y1 = y0.saturating_add(self.buffer(r.h).0).min(ys.end);
        for y in y0.max(ys.start)..y1 {
            for x in x0.max(xs.start)..x1 {
                let c = stepped.of(buf.get(x, y));
                buf.put(x, y, c);
            }
        }
    }

    /// Recolour each art pixel of `r` from what lies there, clipped to the
    /// buffer: `f` gets the pixel's offset in `r` and its colour, and what it
    /// returns covers the whole art pixel.
    pub(crate) fn recolour(
        self,
        buf: &mut RgbBuffer,
        r: ArtRect,
        mut f: impl FnMut(u16, u16, Rgb) -> Rgb,
    ) {
        let (art_xs, art_ys) = self.writable_art(buf);
        let offsets = |from: u16, len: u16, on: &Range<u16>| {
            on.start.saturating_sub(from).min(len)..on.end.saturating_sub(from).min(len)
        };
        for dy in offsets(r.y.0, r.h.0, &art_ys) {
            for dx in offsets(r.x.0, r.w.0, &art_xs) {
                let at = ArtRect {
                    x: ArtPx(r.x.0 + dx),
                    y: ArtPx(r.y.0 + dy),
                    w: ArtPx(1),
                    h: ArtPx(1),
                };
                let (x, y) = (self.buffer(at.x).0, self.buffer(at.y).0);
                if x < buf.width() && y < buf.height() {
                    let c = f(dx, dy, buf.get(x, y));
                    self.fill(buf, at, c);
                }
            }
        }
    }

    /// Dither a full-width band from `light` at its top to `dark` at its
    /// bottom, `y1` exclusive; a band with no height paints nothing.
    ///
    /// One matrix cell is one art pixel on both axes, and the level steps once
    /// per art row: a cell or a step any finer would be a pixel smaller than the
    /// art's. The matrix is indexed by the ABSOLUTE art coordinate, not its
    /// offset in the band, so the pattern tiles across every band sharing the
    /// buffer with no seam at a band's boundary.
    pub(crate) fn dither_band(
        self,
        buf: &mut RgbBuffer,
        y0: ArtPx,
        y1: ArtPx,
        dark: Rgb,
        light: Rgb,
    ) {
        if y1 <= y0 {
            return;
        }
        let span = u32::from(y1.0 - y0.0);
        let (columns, art_ys) = self.writable_art(buf);
        let rows = y0.0.max(art_ys.start)..y1.0.min(art_ys.end);
        for y in rows {
            let through = f32::from(y - y0.0) / span as f32;
            for x in columns.clone() {
                let c = if crate::dither::takes_next(x, y, through) {
                    dark
                } else {
                    light
                };
                self.fill(
                    buf,
                    ArtRect {
                        x: ArtPx(x),
                        y: ArtPx(y),
                        w: ArtPx(1),
                        h: ArtPx(1),
                    },
                    c,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::pen::test_density;
    use crate::render_scale::RenderScale;

    const LIGHT: Rgb = Rgb {
        r: 200,
        g: 200,
        b: 200,
    };
    const DARK: Rgb = Rgb {
        r: 40,
        g: 40,
        b: 40,
    };
    const BG: Rgb = Rgb { r: 1, g: 2, b: 3 };

    fn pen(s: u16, d: u16) -> Pen {
        Pen::new(RenderScale::new(s).expect("nonzero"), test_density(d)).expect("d divides s")
    }

    #[test]
    fn a_dither_cell_is_one_art_pixel_on_both_axes() {
        for (s, d) in [(1u16, 1u16), (8, 4), (12, 4), (8, 8)] {
            let pen = pen(s, d);
            let k = s / d;
            let mut buf = RgbBuffer::filled(16 * k, 16 * k, BG);
            pen.dither_band(&mut buf, ArtPx(0), ArtPx(16), DARK, LIGHT);
            for y in (0..buf.height()).step_by(usize::from(k)) {
                for x in (0..buf.width()).step_by(usize::from(k)) {
                    let c = buf.get(x, y);
                    for (dx, dy) in (0..k).flat_map(|dy| (0..k).map(move |dx| (dx, dy))) {
                        assert_eq!(
                            buf.get(x + dx, y + dy),
                            c,
                            "s {s} d {d}: the art pixel at ({x}, {y}) is split"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_take_copies_whole_art_pixels_where_it_holds() {
        let take = |x: ArtPx, y: ArtPx| (x.0 + 2 * y.0).is_multiple_of(3);
        for (s, d) in [(1u16, 1u16), (8, 4), (12, 4)] {
            let pen = pen(s, d);
            let k = s / d;
            let mut buf = RgbBuffer::filled(9 * k, 9 * k, BG);
            pen.take_where(&mut buf, &RgbBuffer::filled(9 * k, 9 * k, LIGHT), take);
            for y in 0..buf.height() {
                for x in 0..buf.width() {
                    let want = if take(ArtPx(x / k), ArtPx(y / k)) {
                        LIGHT
                    } else {
                        BG
                    };
                    assert_eq!(buf.get(x, y), want, "s {s} d {d}: ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn a_dither_level_steps_per_art_row() {
        // Two renders of one band, one art grid apart in resolution, must agree
        // art pixel for art pixel: a level stepping per buffer row would put a
        // step mid-way through an art row at the coarser one.
        let (fine, coarse) = (pen(4, 4), pen(12, 4));
        let mut a = RgbBuffer::filled(12, 12, BG);
        let mut b = RgbBuffer::filled(36, 36, BG);
        fine.dither_band(&mut a, ArtPx(1), ArtPx(11), DARK, LIGHT);
        coarse.dither_band(&mut b, ArtPx(1), ArtPx(11), DARK, LIGHT);
        for y in 0..b.height() {
            for x in 0..b.width() {
                assert_eq!(b.get(x, y), a.get(x / 3, y / 3), "buffer pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn a_dither_band_runs_light_at_the_top_to_dark_at_the_bottom() {
        let pen = pen(1, 1);
        let mut buf = RgbBuffer::filled(16, 32, BG);
        pen.dither_band(&mut buf, ArtPx(0), ArtPx(32), DARK, LIGHT);

        let dark_in = |y0: u16, y1: u16| {
            (y0..y1)
                .flat_map(|y| (0..16).map(move |x| (x, y)))
                .filter(|&(x, y)| buf.get(x, y) == DARK)
                .count()
        };
        let (top, bottom) = (dark_in(0, 4), dark_in(28, 32));
        assert_eq!(top, 0, "the first rows are entirely the light tone");
        assert!(
            bottom > top,
            "the dark tone must dominate by the bottom (top {top}, bottom {bottom})"
        );
        // Monotone: each quarter is at least as dark as the one above it.
        let quarters: Vec<usize> = (0..4).map(|q| dark_in(q * 8, q * 8 + 8)).collect();
        assert!(
            quarters.windows(2).all(|w| w[1] >= w[0]),
            "the ramp must not reverse: {quarters:?}"
        );
    }

    #[test]
    fn a_dither_band_uses_only_its_two_tones() {
        let mut buf = RgbBuffer::filled(8, 8, BG);
        pen(1, 1).dither_band(&mut buf, ArtPx(0), ArtPx(8), DARK, LIGHT);
        assert!(buf.as_slice().iter().all(|&c| c == DARK || c == LIGHT));
    }

    #[test]
    fn an_inverted_or_empty_band_paints_nothing() {
        let mut buf = RgbBuffer::filled(4, 4, BG);
        pen(1, 1).dither_band(&mut buf, ArtPx(3), ArtPx(3), DARK, LIGHT);
        pen(1, 1).dither_band(&mut buf, ArtPx(3), ArtPx(1), DARK, LIGHT);
        assert!(buf.as_slice().iter().all(|&c| c == BG));
    }

    #[test]
    fn a_shade_steps_every_pixel_along_its_own_ramp() {
        let mut buf = RgbBuffer::filled(8, 4, BG);
        for x in 4..8 {
            for y in 0..4 {
                buf.put(x, y, LIGHT);
            }
        }
        let r = ArtRect {
            x: ArtPx(1),
            y: ArtPx(0),
            w: ArtPx(2),
            h: ArtPx(1),
        };
        pen(2, 1).shade(&mut buf, r, &mut Stepped::new(-2));
        assert_eq!(
            buf.get(2, 1),
            BG.ramp(-2),
            "the dark pixel, two stops darker"
        );
        assert_eq!(
            buf.get(5, 0),
            LIGHT.ramp(-2),
            "the light one, relative to itself"
        );
        assert_eq!(buf.get(1, 0), BG, "nothing west of the rect");
        assert_eq!(buf.get(2, 2), BG, "nothing south of it");
    }

    #[test]
    fn a_shade_past_the_edge_clips_rather_than_wrapping() {
        let mut buf = RgbBuffer::filled(8, 4, BG);
        let r = ArtRect {
            x: ArtPx(3),
            y: ArtPx(0),
            w: ArtPx(2),
            h: ArtPx(1),
        };
        pen(2, 1).shade(&mut buf, r, &mut Stepped::new(-2));
        assert_eq!(buf.get(7, 1), BG.ramp(-2), "the part on the buffer");
        assert_eq!(buf.get(0, 1), BG, "nothing wraps into the next row");
        assert_eq!(buf.get(1, 2), BG, "nor the one after");
    }

    #[test]
    fn a_grid_shades_each_line_pixel_once() {
        let mut buf = RgbBuffer::filled(8, 8, BG);
        let r = ArtRect {
            x: ArtPx(0),
            y: ArtPx(0),
            w: ArtPx(4),
            h: ArtPx(4),
        };
        pen(2, 1).shade_grid(&mut buf, r, ArtPx(2), -2);
        let line = BG.ramp(-2);
        assert_eq!(buf.get(0, 0), line, "a crossing, shaded once");
        assert_eq!(buf.get(4, 5), line, "a row");
        assert_eq!(buf.get(5, 3), line, "a column");
        assert_eq!(buf.get(3, 3), BG, "inside a cell");
    }

    #[test]
    fn a_recolour_covers_whole_art_pixels_from_their_offset() {
        let mut buf = RgbBuffer::filled(8, 4, BG);
        let r = ArtRect {
            x: ArtPx(1),
            y: ArtPx(0),
            w: ArtPx(4),
            h: ArtPx(1),
        };
        pen(2, 1).recolour(&mut buf, r, |dx, _, c| if dx == 1 { LIGHT } else { c });
        assert_eq!(buf.get(4, 1), LIGHT, "offset 1 is the art pixel at x 2");
        assert_eq!(buf.get(5, 0), LIGHT, "all of it");
        assert_eq!(buf.get(2, 0), BG, "offset 0 kept its colour");
    }

    #[test]
    fn a_fill_covers_whole_art_pixels_and_clips() {
        let mut buf = RgbBuffer::filled(8, 8, BG);
        let r = ArtRect {
            x: ArtPx(1),
            y: ArtPx(1),
            w: ArtPx(4),
            h: ArtPx(1),
        };
        pen(2, 1).fill(&mut buf, r, LIGHT);
        assert_eq!(buf.get(2, 2), LIGHT);
        assert_eq!(
            buf.get(7, 3),
            LIGHT,
            "the last art pixel, both buffer columns"
        );
        assert_eq!(buf.get(1, 2), BG, "nothing before the rect");
        assert_eq!(buf.get(2, 4), BG, "nothing below it");
    }
}
