//! The [effects](crate::effects) riding on the display list's figures, drawn on the
//! figure's own art grid: a look authored at [`LOOK_DENSITY`] where the figure
//! is drawn there with a head to stand it on, the base art's z and requires-action
//! mark beside its head, else its [shared look](crate::effects::look), a
//! layout cell per art cell block. A look stays whole down to [`LOOK_SOLID`].

use pixtuoid_core::sprite::Rgb;
use pixtuoid_core::sprite::format::Density;

use crate::display::Span;
use crate::display::pen::Pen;
use crate::effects::look::{
    FLAME_CORE, FLAME_DEEP, FLAME_MID, FLAME_TIP, Inks, REQUIRES_ACTION_MARK_1X, SLEEP_Z_1X,
    SLEEP_Z_MAX_RISE, plot_effect, sleep_z_fade, walking_dust_foot,
};
use crate::effects::{Effect, EffectKind};

/// The density this module's own looks are drawn at.
const LOOK_DENSITY: Density = Density::new(4).expect("nonzero");

/// A point on the art grid, which a look may hang past the buffer's top or
/// west edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ArtPoint {
    pub(crate) x: i32,
    pub(crate) y: i32,
}

/// One effect on the figure it rides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Riding {
    pub(crate) effect: Effect,
    /// The figure's head on the art grid: the top of its hair over the head
    /// mark's column. `None` where its frame has no head to stand a look on.
    pub(crate) head: Option<ArtPoint>,
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
            Some(head) if d == LOOK_DENSITY => return dense_look(e, head, inks, emit),
            Some(head) if d == Density::ONE && base_look(e, head, inks, emit) => return,
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
const SLEEP_Z: &[&str] = &[
    "######", //
    "....##", //
    "...##.", //
    "..##..", //
    ".##...", //
    "######", //
];

/// The requires-action mark, beside the head.
const REQUIRES_ACTION_MARK: &[&str] = &[
    ".####.", //
    "##..##", //
    "##..##", //
    "...##.", //
    "..##..", //
    "..##..", //
    "......", //
    "..##..", //
    "..##..", //
];

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

/// The flame crown's two frames, centred over the head mark, their bottom row
/// on the hair's top: `D`eep, `M`id, `T`ip, `C`ore.
const FLAME_CROWN: [&[&str]; 2] = [
    &[
        "......M........", //
        "......DM....M..", //
        "..M..DMD...DM..", //
        "..DM.DTMD..DMD.", //
        ".DMD.MTTMD.MTD.", //
        ".DTMDMTCTMDTTMD", //
        "DMTTMTCCCTMTCTD", //
        "DTCCTCCCCCTCCMD", //
        "DMTTCCTCCTCTTMD", //
    ],
    &[
        "........M......", //
        "...M...MD......", //
        "...MD..DMD..M..", //
        "..DMD.DMTD..MD.", //
        ".DMTD.MTTMD.DMD", //
        "DMTTMDTCTMDMTD.", //
        "DTCTMTCCCTMTTMD", //
        "DMCCTCCCCCTCCTD", //
        "DMTTCTCCTCCTTMD", //
    ],
];

/// The walking dust's speckle, kicked up round a foot's sole.
const DUST: &[&str] = &[
    "..#.#..", //
    ".#.#.#.", //
    "#.#.#.#", //
];

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
                let top = head.y + BESIDE_DY_1X - glyph_h(SLEEP_Z_1X) - rise;
                let coverage = (alpha / LOOK_SOLID).min(1.0);
                stamp(SLEEP_Z_1X, head.x + BESIDE_DX_1X, top, emit, |_| {
                    Some((inks.sleep_z, coverage))
                });
            }
            true
        }
        EffectKind::RequiresActionMark => {
            let top = head.y + BESIDE_DY_1X - glyph_h(REQUIRES_ACTION_MARK_1X);
            stamp(
                REQUIRES_ACTION_MARK_1X,
                head.x + BESIDE_DX_1X,
                top,
                emit,
                |_| Some((inks.requires_action, 1.0)),
            );
            true
        }
        _ => false,
    }
}

/// `e`'s look at [`LOOK_DENSITY`], from its rider's `head`.
fn dense_look(
    e: Effect,
    head: ArtPoint,
    inks: &Inks,
    emit: &mut impl FnMut(ArtPoint, i32, Rgb, f32),
) {
    let d = i32::from(LOOK_DENSITY.get());
    let centred = |glyph: &[&str]| head.x - glyph_w(glyph) / 2;
    match e.kind {
        EffectKind::SleepZ => {
            let Some((alpha, t)) = sleep_z_fade(e.phase) else {
                return;
            };
            let rise = (t * f32::from(SLEEP_Z_MAX_RISE) * d as f32) as i32;
            let x = head.x + BESIDE_DX + rise / SLEEP_Z_DRIFT;
            let top = head.y + BESIDE_DY - glyph_h(SLEEP_Z) - rise;
            let coverage = (alpha / LOOK_SOLID).min(1.0);
            stamp(SLEEP_Z, x, top, emit, |_| Some((inks.sleep_z, coverage)));
        }
        EffectKind::RequiresActionMark => {
            let top = head.y + BESIDE_DY - glyph_h(REQUIRES_ACTION_MARK);
            stamp(REQUIRES_ACTION_MARK, head.x + BESIDE_DX, top, emit, |_| {
                Some((inks.requires_action, 1.0))
            });
        }
        EffectKind::FlameCrown => {
            let glyph = FLAME_CROWN[usize::from(e.phase == 1)];
            let top = head.y - glyph_h(glyph) + 1;
            stamp(glyph, centred(glyph), top, emit, |ch| {
                let c = match ch {
                    'D' => FLAME_DEEP,
                    'M' => FLAME_MID,
                    'T' => FLAME_TIP,
                    _ => FLAME_CORE,
                };
                Some((c, 1.0))
            });
        }
        EffectKind::WalkingDust => {
            let foot = walking_dust_foot(e.at, e.phase);
            let x = i32::from(foot.x) * d + d / 2 - glyph_w(DUST) / 2;
            let y = i32::from(foot.y) * d - 1;
            stamp(DUST, x, y, emit, |_| Some((inks.dust, 1.0)));
        }
        // A creature's riders have no head, so `look` plots them as the classic
        // does; steam isn't drawn here.
        EffectKind::PetHeart | EffectKind::SteamPuff | EffectKind::MascotBubble => {}
    }
}

fn glyph_w(glyph: &[&str]) -> i32 {
    glyph.first().map_or(0, |row| row.len() as i32)
}

fn glyph_h(glyph: &[&str]) -> i32 {
    glyph.len() as i32
}

/// Emit each of `glyph`'s inked cells from `(x, y)`, coloured by `ink`.
fn stamp(
    glyph: &[&str],
    x: i32,
    y: i32,
    emit: &mut impl FnMut(ArtPoint, i32, Rgb, f32),
    ink: impl Fn(char) -> Option<(Rgb, f32)>,
) {
    for (dx, dy, ch) in crate::effects::look::inked(glyph) {
        if let Some((c, coverage)) = ink(ch) {
            let at = ArtPoint {
                x: x + i32::from(dx),
                y: y + i32::from(dy),
            };
            emit(at, 1, c, coverage);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A look is centred and stacked by its first row's width and its row
    /// count, so a ragged row would sit it off its mark.
    #[test]
    fn every_look_is_a_rectangle() {
        for glyph in [
            SLEEP_Z,
            REQUIRES_ACTION_MARK,
            SLEEP_Z_1X,
            REQUIRES_ACTION_MARK_1X,
            DUST,
            FLAME_CROWN[0],
            FLAME_CROWN[1],
        ] {
            assert!(
                glyph.iter().all(|row| row.len() == glyph[0].len()),
                "{glyph:?}"
            );
        }
        for frame in FLAME_CROWN {
            assert!(
                frame
                    .iter()
                    .flat_map(|row| row.chars())
                    .all(|c| ".DMTC".contains(c)),
                "{frame:?} inks a colour the crown has no name for"
            );
        }
    }
}
