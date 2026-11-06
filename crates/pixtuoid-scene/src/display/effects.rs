//! The [effects](crate::effects) riding on the display list's figures, drawn on the
//! figure's own art grid: a look authored at [`LOOK_DENSITY`] where the figure
//! is drawn there with a head to set it on or beside, the base art's z and requires-action
//! mark beside its head, else its [shared look](crate::effects::look), a
//! layout cell per art cell block. A look stays whole down to [`LOOK_SOLID`].

use crate::sprite::format::Density;
use crate::sprite::{HeadView, Rgb};

use crate::display::Span;
use crate::display::pen::Pen;
use crate::effects::look::{
    Inks, REQUIRES_ACTION_MARK_1X, SLEEP_Z_1X, SLEEP_Z_MAX_RISE, plot_effect, sleep_z_fade,
    walking_dust_foot,
};
use crate::effects::{Cap, Effect, EffectKind};
use crate::layout::chrome::{Glyph, REQUIRES_ACTION_MARK};

/// The density this module's own looks are drawn at.
const LOOK_DENSITY: Density = Density::new(4).expect("nonzero");

/// A point on the art grid, which a look may hang past the buffer's top or
/// west edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ArtPoint {
    pub(crate) x: i32,
    pub(crate) y: i32,
}

/// A figure's head on its art grid, which its riders set their looks by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Head {
    /// The base art's: its frame's top row over the head mark's column.
    Base(ArtPoint),
    /// A dressed frame's: its crest, the top of its hair over the head mark's
    /// column, and its brow.
    Dressed { crest: ArtPoint, brow: Brow },
}

/// A dressed head's brow on the art grid: its mark, the columns it is drawn
/// across there ([`Dress::brow`](crate::character::Dress::brow)) and the view
/// it is seen from, which a cap there takes its shape from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Brow {
    pub(crate) at: ArtPoint,
    pub(crate) reach: (i32, i32),
    pub(crate) view: HeadView,
}

/// One effect on the figure it rides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Riding {
    pub(crate) effect: Effect,
    /// The figure's head. `None` where its frame marks none to float a look
    /// beside.
    pub(crate) head: Option<Head>,
    /// The figure's own grid.
    pub(crate) pen: Pen,
    /// The theme's colours its look is drawn in.
    pub(crate) inks: Inks,
}

impl Riding {
    /// The logical cells it paints, sorted with the `carrier` figure it rides
    /// on; `None` when it paints nothing this frame.
    pub(crate) fn span(&self, carrier: Span) -> Option<Span> {
        let d = i32::from(self.pen.density().get());
        let mut cells: Option<(i32, i32, i32, i32)> = None;
        self.look(&mut |at, size, _, _| {
            let (x1, y1) = (at.x + size - 1, at.y + size - 1);
            cells = Some(cells.map_or((at.x, at.y, x1, y1), |(a, b, c, e)| {
                (a.min(at.x), b.min(at.y), c.max(x1), e.max(y1))
            }));
        });
        let (x0, y0, x1, y1) = cells?;
        let cell = |a: i32| u16::try_from(a.max(0).div_euclid(d)).unwrap_or(u16::MAX);
        let span = Span {
            x0: cell(x0),
            x1: cell(x1),
            y0: cell(y0),
            y1: cell(y1),
            depth: carrier.depth,
            layer: carrier.layer,
        };
        Some(span)
    }

    /// Its look, as squares of art pixels: each top-left, side, colour and the
    /// share of it covered.
    pub(crate) fn look(&self, emit: &mut impl FnMut(ArtPoint, i32, Rgb, f32)) {
        let (e, inks) = (self.effect, &self.inks);
        let d = self.pen.density();
        match self.head {
            Some(Head::Dressed { crest, brow }) if d == LOOK_DENSITY => {
                return dense_look(e, (crest, brow), inks, emit);
            }
            Some(Head::Base(head)) if d == Density::ONE && base_look(e, head, inks, emit) => return,
            _ => {}
        }
        let d = i32::from(d.get());
        plot_effect(&e, inks, &mut |x, y, c, alpha| {
            let at = ArtPoint {
                x: i32::from(x) * d,
                y: i32::from(y) * d,
            };
            emit(at, d, c, (alpha / LOOK_SOLID).min(1.0));
        });
    }
}

/// A sleeping head's z, rising off the side of the head and drifting away.
const SLEEP_Z: Glyph = Glyph::new(&[
    "######", //
    "....##", //
    "...##.", //
    "..##..", //
    ".##...", //
    "######", //
]);

/// Art columns from the head mark to the west edge of what floats beside the
/// head: clear of the hair, and of the badge centred over it.
const BESIDE_DX: i32 = 9;
/// Art rows from the hair's top down to the bottom of what floats beside it.
const BESIDE_DY: i32 = 5;
/// The opacity down to which a look stays whole: a dither through a glyph a
/// stroke or two wide breaks it, so it dissolves only on its way out.
const LOOK_SOLID: f32 = 0.3;
/// Art rows a rising z drifts one column away from the head.
const SLEEP_Z_DRIFT: i32 = 4;

/// The walking dust's speckle, kicked up round a foot's sole.
const DUST: Glyph = Glyph::new(&[
    "..#.#..", //
    ".#.#.#.", //
    "#.#.#.#", //
]);

/// Columns from the base art's head to the west edge of what floats beside it:
/// past the head, and the badge centred over it.
const BESIDE_DX_1X: i32 = 5;
/// Rows from the head's top down to the bottom of what floats beside it.
const BESIDE_DY_1X: i32 = 1;

/// `e`'s look on the base art, from its rider's `head`, where the base art
/// has a look of its own: whether it drew one.
fn base_look(
    e: Effect,
    head: ArtPoint,
    inks: &Inks,
    emit: &mut impl FnMut(ArtPoint, i32, Rgb, f32),
) -> bool {
    match e.kind {
        EffectKind::SleepZ => {
            if let Some((alpha, t)) = sleep_z_fade(e.phase) {
                let rise = (t * f32::from(SLEEP_Z_MAX_RISE)) as i32;
                let top = head.y + BESIDE_DY_1X - i32::from(SLEEP_Z_1X.height()) - rise;
                let coverage = (alpha / LOOK_SOLID).min(1.0);
                stamp(
                    SLEEP_Z_1X,
                    head.x + BESIDE_DX_1X,
                    top,
                    emit,
                    (inks.sleep_z, coverage),
                );
            }
            true
        }
        EffectKind::RequiresActionMark => {
            let top = head.y + BESIDE_DY_1X - i32::from(REQUIRES_ACTION_MARK_1X.height());
            stamp(
                REQUIRES_ACTION_MARK_1X,
                head.x + BESIDE_DX_1X,
                top,
                emit,
                (inks.requires_action, 1.0),
            );
            true
        }
        _ => false,
    }
}

/// `e`'s look at [`LOOK_DENSITY`], from its rider's crest, `head`, and its `brow`.
fn dense_look(
    e: Effect,
    (head, brow): (ArtPoint, Brow),
    inks: &Inks,
    emit: &mut impl FnMut(ArtPoint, i32, Rgb, f32),
) {
    let d = i32::from(LOOK_DENSITY.get());
    match e.kind {
        EffectKind::SleepZ => {
            let Some((alpha, t)) = sleep_z_fade(e.phase) else {
                return;
            };
            let rise = (t * f32::from(SLEEP_Z_MAX_RISE) * d as f32) as i32;
            let x = head.x + BESIDE_DX + rise / SLEEP_Z_DRIFT;
            let top = head.y + BESIDE_DY - i32::from(SLEEP_Z.height()) - rise;
            let coverage = (alpha / LOOK_SOLID).min(1.0);
            stamp(SLEEP_Z, x, top, emit, (inks.sleep_z, coverage));
        }
        EffectKind::RequiresActionMark => {
            let top = head.y + BESIDE_DY - i32::from(REQUIRES_ACTION_MARK.height());
            stamp(
                REQUIRES_ACTION_MARK,
                head.x + BESIDE_DX,
                top,
                emit,
                (inks.requires_action, 1.0),
            );
        }
        EffectKind::WalkingDust => {
            let foot = walking_dust_foot(e.at, e.phase);
            let x = i32::from(foot.x) * d + d / 2 - i32::from(DUST.width()) / 2;
            let y = i32::from(foot.y) * d - 1;
            stamp(DUST, x, y, emit, (inks.dust, 1.0));
        }
        EffectKind::SubagentCap(cap) => cap_look(cap, brow, emit),
        // A creature's riders have no head, so `look` plots them as the classic
        // does; steam isn't drawn here.
        EffectKind::PetHeart | EffectKind::SteamPuff | EffectKind::MascotBubble => {}
    }
}

/// A cap's crown, top row first: each row's half-width as a share of the head
/// it covers, a dome flattened on top as a ball cap's is.
const CAP_CROWN: [f32; 8] = [0.47, 0.7, 0.82, 0.9, 0.95, 0.98, 0.99, 1.0];
/// A cap seen from above, row by row as [`CAP_CROWN`] is: a disc.
const CAP_DISC: [f32; 8] = [0.54, 0.82, 0.95, 1.0, 1.0, 0.95, 0.82, 0.54];
/// Rows from an upright cap's crown down to its brim: the visor at the front,
/// the crown carried on over the strap at the back and over the peak in
/// profile.
const CAP_BAND: usize = 3;
/// The visor seen from the front, row by row: its share of the head and the
/// ramp step it is drawn in, its seam, its lit top and its dark lip.
const CAP_VISOR: [(f32, i8); CAP_BAND] = [(1.0, CAP_UNDER), (0.96, 1), (0.82, CAP_UNDER)];
/// The peak in profile, row by row down to the brim: the columns it juts
/// past the crown and its ramp step, its lit top over its underside.
const CAP_PEAK: [(i32, i8); CAP_BAND - 1] = [(4, 1), (6, CAP_UNDER)];
/// The tab under a cap seen from above, row by row from the disc's last: its
/// share of the head, the peak glimpsed past the crown.
const CAP_TAB: [f32; 2] = [0.55, 0.4];
/// The share of the head the seam across a cap seen from above spans.
const CAP_CROSS: f32 = 0.92;
/// The ramp step of a cap's seams and its shaded east edge.
const CAP_SEAM: i8 = -1;
/// The ramp step of what lies under a cap's crown, its brim, strap and tab,
/// and of its button seen from above.
const CAP_UNDER: i8 = -2;
/// Columns a cap is lit along its west edge.
const CAP_LIT: i32 = 3;
/// Columns a cap is shaded along its east edge.
const CAP_SHADED: i32 = 2;

/// `cap` with its brim on `brow`, as wide as the head is drawn there: its
/// crown lit on top and west and shaded east, ringed in its line as a dressed
/// figure is, and turned with its figure.
fn cap_look(cap: Cap, brow: Brow, emit: &mut impl FnMut(ArtPoint, i32, Rgb, f32)) {
    let (west, east) = brow.reach;
    let span = |s: f32| {
        let side = |reach: i32| (reach as f32 * s).round() as i32;
        -side(west)..=side(east)
    };
    let ink = |level: i8| cap.ink.ramp(level);
    let (crown, under): (&[f32], usize) = match brow.view {
        HeadView::Front => (&CAP_CROWN, 0),
        HeadView::Back => (&CAP_CROWN, CAP_BAND - 1),
        HeadView::Side => (&CAP_CROWN, CAP_BAND),
        HeadView::Crown => (&CAP_DISC, 0),
    };
    let mut px = std::collections::BTreeMap::new();
    let rows = crown.iter().map(|&s| span(s));
    for (row, cols) in (0..).zip(rows.chain(std::iter::repeat_n(-west..=east, under))) {
        let (lo, hi) = (*cols.start(), *cols.end());
        for dx in cols {
            let level = if row == 0 || dx < lo + CAP_LIT {
                1
            } else if dx > hi - CAP_SHADED {
                CAP_SEAM
            } else {
                0
            };
            px.insert((dx, row), ink(level));
        }
    }
    let mut band = |row: i32, cols: std::ops::RangeInclusive<i32>, c: Rgb| {
        for dx in cols {
            px.insert((dx, row), c);
        }
    };
    let (centre, depth) = ((east - west + 1).div_euclid(2), crown.len() as i32);
    match brow.view {
        HeadView::Front => {
            (1..depth).for_each(|row| band(row, centre..=centre, ink(CAP_SEAM)));
            band(-1, centre - 1..=centre, ink(CAP_SEAM));
            for (row, (s, level)) in (depth..).zip(CAP_VISOR) {
                band(row, span(s), ink(level));
            }
        }
        HeadView::Back => {
            let strap = depth + under as i32;
            (1..strap - 1).for_each(|row| band(row, centre..=centre, ink(CAP_SEAM)));
            band(-1, centre - 1..=centre, ink(CAP_SEAM));
            band(strap - 1, centre - 1..=centre, cap.line);
            band(strap, centre - 2..=centre + 1, ink(CAP_UNDER));
        }
        HeadView::Side => {
            band(-1, centre - 1..=centre, ink(CAP_SEAM));
            let peak = depth + (CAP_BAND - CAP_PEAK.len()) as i32;
            for (row, (jut, level)) in (peak..).zip(CAP_PEAK) {
                band(row, east + 1..=east + jut, ink(level));
            }
        }
        HeadView::Crown => {
            let tab = depth - 1;
            (1..tab).for_each(|row| band(row, centre..=centre, ink(CAP_SEAM)));
            let cross = span(CAP_CROSS);
            band(
                depth / 2,
                cross.start() + 1..=cross.end() - 1,
                ink(CAP_SEAM),
            );
            for row in depth / 2 - 1..=depth / 2 {
                band(row, centre - 1..=centre, ink(CAP_UNDER));
            }
            for (row, s) in (tab..).zip(CAP_TAB) {
                band(row, span(s), ink(CAP_UNDER));
            }
        }
    }
    let bounds = px
        .keys()
        .fold(None, |b: Option<(i32, i32, i32, i32)>, &(dx, row)| {
            Some(b.map_or((dx, dx, row, row), |(x0, x1, top, bottom)| {
                (x0.min(dx), x1.max(dx), top.min(row), bottom.max(row))
            }))
        });
    let Some((x0, x1, top, bottom)) = bounds else {
        return;
    };
    let sign = if cap.flip { -1 } else { 1 };
    let at = |(dx, row): (i32, i32)| ArtPoint {
        x: brow.at.x + sign * dx,
        y: brow.at.y - bottom + row,
    };
    let around = (top - 1..=bottom + 1).flat_map(|row| (x0 - 1..=x1 + 1).map(move |dx| (dx, row)));
    for cell in crate::character::outline(around, |dx, row| px.contains_key(&(dx, row))) {
        emit(at(cell), 1, cap.line, 1.0);
    }
    for (&cell, &c) in &px {
        emit(at(cell), 1, c, 1.0);
    }
}

/// Emit each of `glyph`'s inked cells from `(x, y)` in colour `c` at `coverage`.
fn stamp(
    glyph: Glyph,
    x: i32,
    y: i32,
    emit: &mut impl FnMut(ArtPoint, i32, Rgb, f32),
    (c, coverage): (Rgb, f32),
) {
    for (dx, dy) in glyph.inked() {
        let at = ArtPoint {
            x: x + i32::from(dx),
            y: y + i32::from(dy),
        };
        emit(at, 1, c, coverage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_scale::RenderScale;

    /// The densest art's cap takes its shape from its own frame's view, even
    /// where the base art's, which the effect carries for the base look,
    /// differs: a profile's peak juts past the brow, a front's does not.
    #[test]
    fn a_dense_cap_takes_its_own_frames_view() {
        let theme = crate::theme::theme_by_name("normal").expect("theme");
        let line = Rgb { r: 1, g: 1, b: 1 };
        let at = ArtPoint { x: 40, y: 40 };
        let reach = (10, 10);
        let east_edge = |view: HeadView| {
            let cap = Cap {
                ink: Rgb {
                    r: 0xec,
                    g: 0xec,
                    b: 0xec,
                },
                line,
                view: HeadView::Front,
                flip: false,
            };
            let riding = Riding {
                effect: crate::effects::subagent_cap(crate::layout::Point { x: 10, y: 10 }, cap),
                head: Some(Head::Dressed {
                    crest: at,
                    brow: Brow { at, reach, view },
                }),
                pen: Pen::new(RenderScale::new(4).expect("nonzero"), LOOK_DENSITY)
                    .expect("a density divides itself"),
                inks: Inks::of(theme),
            };
            let mut east = i32::MIN;
            riding.look(&mut |p, _, c, _| {
                if c != line {
                    east = east.max(p.x);
                }
            });
            east
        };
        assert_eq!(east_edge(HeadView::Front), at.x + reach.1);
        assert!(
            east_edge(HeadView::Side) > at.x + reach.1,
            "no peak in profile"
        );
    }
}
