//! The classic at a scale: its frame of one pixel a layout unit blown up, its
//! world text baked in the cutaway's pixel font and plates where its look asks,
//! and marked dirty only where it differs from the frame before.

use crate::composite::BLACK;
use crate::display::pen::Pen;
use crate::display::{HostText, TextRole, TextRun};
use crate::layout::Bounds;
use crate::look::Dirty;
use crate::look::WorldText;
use crate::pack::OfficeArt;
use crate::render_scale::RenderScale;
use crate::sprite::{Rgb, RgbBuffer};

/// Layout units a side of the squares a frame is compared to the last in: a
/// walker's step dirties a few, and a row of them compares as slices.
pub(super) const SQUARE: u16 = 4;

/// The scaled classic's state across frames.
#[derive(Debug)]
pub(super) struct Scaled {
    pub(super) buf: RgbBuffer,
    /// The frame before, which `buf` is compared to.
    was: RgbBuffer,
    /// The board's star, where its text is baked.
    pub(super) star: Option<Bounds>,
}

impl Scaled {
    pub(super) fn new() -> Self {
        Self {
            buf: RgbBuffer::filled(0, 0, BLACK),
            was: RgbBuffer::filled(0, 0, BLACK),
            star: None,
        }
    }

    /// Draw `unit`, the classic's undimmed frame, at `scale`, with `host`'s
    /// runs baked over it where `text` says, then dim it all as `host` is
    /// dimmed; `fresh` where the frame before showed another look, which the
    /// frame then repaints whole.
    pub(super) fn frame(
        &mut self,
        unit: &RgbBuffer,
        host: HostText<'_>,
        (scale, text): (RenderScale, WorldText),
        pack: &OfficeArt,
        fresh: bool,
    ) -> Dirty {
        std::mem::swap(&mut self.buf, &mut self.was);
        blow_up(unit, scale, &mut self.buf);
        self.star = None;
        if text == WorldText::Baked {
            let pen = Pen::for_pack(scale, pack);
            for run in runs(host, pen) {
                if run.role == TextRole::Star {
                    self.star = Some(crate::display::compose::run_box(&run, pen));
                }
                crate::cutaway::paint::paint_run(&run, (pack, pen), &mut self.buf);
            }
        }
        host.dim.apply(&mut self.buf);
        let reshaped =
            (self.was.width(), self.was.height()) != (self.buf.width(), self.buf.height());
        if fresh || reshaped {
            return Dirty::All;
        }
        changed(&self.was, &self.buf, scale)
    }
}

/// `unit` at `scale` buffer pixels a pixel, into `out`.
fn blow_up(unit: &RgbBuffer, scale: RenderScale, out: &mut RgbBuffer) {
    let s = usize::from(scale.get());
    let (w, h) = (unit.width(), unit.height());
    out.resize_fill(
        w.saturating_mul(scale.get()),
        h.saturating_mul(scale.get()),
        BLACK,
    );
    let row_len = usize::from(w) * s;
    let dst = out.as_mut_slice();
    for (y, src_row) in unit.as_slice().chunks(usize::from(w).max(1)).enumerate() {
        let first = y * s * row_len;
        for (x, &px) in src_row.iter().enumerate() {
            dst[first + x * s..first + (x + 1) * s].fill(px);
        }
        for k in 1..s {
            dst.copy_within(first..first + row_len, first + k * row_len);
        }
    }
}

/// Where `now` differs from `was`, two frames `scale` buffer pixels a unit of
/// one size: each row of [`SQUARE`]-unit squares' rects of changed ones, or
/// the whole frame where most squares changed, which a rect a square would
/// cost a host more than.
pub(super) fn changed(was: &RgbBuffer, now: &RgbBuffer, scale: RenderScale) -> Dirty {
    let side = SQUARE.saturating_mul(scale.get());
    let (w, h) = (now.width(), now.height());
    let (mut rects, mut dirty_squares, mut squares) = (Vec::new(), 0usize, 0usize);
    for top in (0..h).step_by(usize::from(side)) {
        let bottom = top.saturating_add(side).min(h);
        let mut rect: Option<Bounds> = None;
        for left in (0..w).step_by(usize::from(side)) {
            let right = left.saturating_add(side).min(w);
            squares += 1;
            if (top..bottom).any(|y| row(was, y, left..right) != row(now, y, left..right)) {
                dirty_squares += 1;
                let square = Bounds {
                    x: left,
                    y: top,
                    width: right - left,
                    height: bottom - top,
                };
                rect = Some(rect.map_or(square, |r| r.union(square)));
            } else if let Some(r) = rect.take() {
                rects.push(r);
            }
        }
        rects.extend(rect);
    }
    if dirty_squares * 2 > squares {
        Dirty::All
    } else {
        Dirty::within(rects)
    }
}

/// Row `y`'s pixels across `xs`.
fn row(buf: &RgbBuffer, y: u16, xs: std::ops::Range<u16>) -> &[Rgb] {
    let at = usize::from(y) * usize::from(buf.width());
    &buf.as_slice()[at + usize::from(xs.start)..at + usize::from(xs.end)]
}

/// `host`'s runs in its paint order, of its signs those the pixel font sets
/// on `pen`'s grid.
fn runs(host: HostText<'_>, pen: Pen) -> impl Iterator<Item = TextRun> + '_ {
    let signs = crate::display::compose::set_signs(host.signs, pen);
    host.badges
        .iter()
        .map(crate::display::Badge::run)
        .chain(host.bubbles.iter().cloned())
        .chain(signs.into_iter().map(|(run, _)| run.clone()))
}
