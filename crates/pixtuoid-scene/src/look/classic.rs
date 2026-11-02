//! The classic at a scale: its frame of one pixel a layout unit blown up, its
//! world text baked in the cutaway's pixel font and plates where its look asks,
//! and marked dirty only where it differs from the frame before.

use crate::cutaway::canvas::{Dirty, MAX_PASSES, passes};
use crate::display::pen::Pen;
use crate::display::{HostText, TextRole, TextRun};
use crate::layout::Bounds;
use crate::look::{Dim, WorldText};
use crate::pack::OfficeArt;
use crate::render_scale::RenderScale;
use crate::sprite::{Rgb, RgbBuffer};

/// Layout units a side of the squares a frame is compared to the last in: a
/// walker's step dirties a few, and a row of them compares as slices.
const TILE: u16 = 4;

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

    /// Draw `unit`, the classic's frame, at `scale`, with `text` baked over it
    /// where `world` says; `fresh` where the frame before showed another look,
    /// which the frame then repaints whole.
    pub(super) fn frame(
        &mut self,
        unit: &RgbBuffer,
        text: HostText<'_>,
        (scale, world): (RenderScale, WorldText),
        pack: &OfficeArt,
        fresh: bool,
    ) -> Dirty {
        std::mem::swap(&mut self.buf, &mut self.was);
        blow_up(unit, scale, &mut self.buf);
        self.star = None;
        if world == WorldText::Baked {
            let pen = Pen::for_pack(scale, pack);
            for run in runs(text) {
                if run.role == TextRole::Star {
                    self.star = Some(crate::display::compose::run_box(&run, pen));
                }
                crate::cutaway::paint::paint_run(&run, (pack, pen), &mut self.buf);
            }
        }
        let reshaped =
            (self.was.width(), self.was.height()) != (self.buf.width(), self.buf.height());
        if fresh || reshaped {
            return Dirty::All;
        }
        Dirty::within(passes(&changed(&self.was, &self.buf, scale), MAX_PASSES))
    }
}

const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

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

/// The [`TILE`]-unit squares, in buffer pixels, where `now` differs from
/// `was`, two frames `scale` buffer pixels a unit of one size.
fn changed(was: &RgbBuffer, now: &RgbBuffer, scale: RenderScale) -> Vec<Bounds> {
    let side = TILE.saturating_mul(scale.get());
    let (w, h) = (now.width(), now.height());
    fn row(buf: &RgbBuffer, y: u16, x: u16, end: u16) -> &[Rgb] {
        let at = usize::from(y) * usize::from(buf.width());
        &buf.as_slice()[at + usize::from(x)..at + usize::from(end)]
    }
    let mut tiles = Vec::new();
    for top in (0..h).step_by(usize::from(side)) {
        let bottom = top.saturating_add(side).min(h);
        for left in (0..w).step_by(usize::from(side)) {
            let right = left.saturating_add(side).min(w);
            if (top..bottom).any(|y| row(was, y, left, right) != row(now, y, left, right)) {
                tiles.push(Bounds {
                    x: left,
                    y: top,
                    width: right - left,
                    height: bottom - top,
                });
            }
        }
    }
    tiles
}

/// `text`'s runs in its paint order, in the dim it is set under.
fn runs(text: HostText<'_>) -> impl Iterator<Item = TextRun> + '_ {
    text.badges
        .iter()
        .map(crate::display::Badge::run)
        .chain(text.bubbles.iter().cloned())
        .chain(text.signs.iter().cloned())
        .map(move |run| dimmed(run, text.dim))
}

/// `run` under `dim`.
fn dimmed(mut run: TextRun, dim: Dim) -> TextRun {
    for span in &mut run.spans {
        span.ink = dim.of(span.ink);
    }
    run.plate = run.plate.map(|rgb| dim.of(rgb));
    run.strip = run.strip.map(|rgb| dim.of(rgb));
    run
}
