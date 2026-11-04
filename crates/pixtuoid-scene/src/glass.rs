//! The glass room partition's look, pixel-free: what the glass does to
//! each cell of a [`WallPiece`]'s box. Both painters
//! walk their own grid over that box and ask it, so the glass is one design
//! drawn at two densities.
//!
//! The panes are see-through: a pane cell is what lies behind it, stepped up
//! its own ramp ([`Rgb::ramp`]), so a figure behind the glass keeps its hues
//! and a cell that was one colour stays one. Only the frame — the rim, posts
//! and sill — and a face-on pane's glint are colours of their own, drawn from
//! the theme's trim.

use crate::sprite::Rgb;
use crate::sprite::format::Density;

use crate::dither::Stepped;
use crate::layout::WallPiece;
use crate::theme::Theme;

/// Mullion (partition post) spacing along a run, in logical units: a post
/// about this often reads as panelled partitions instead of one unbroken sheet.
const MULLION_STRIDE: u16 = 10;

/// How far into a pane, in logical units along plus across, its glint runs:
/// the diagonal shine a pane catches in its top corner.
const GLINT_AT: u16 = 5;
/// How many cells a glint's second stroke runs beyond its first.
const GLINT_GAP: u16 = 2;

/// Ramp stops a pane lifts what is behind it: the haze that makes it glass.
const PANE_LIFT: i8 = 3;

/// What the theme's trim is lifted by, per channel, for the frame's rim.
const RIM_LIFT: [u8; 3] = [125, 135, 124];
/// What the theme's trim is lifted by, per channel, for the frame's posts and
/// sill.
const POST_LIFT: [u8; 3] = [18, 52, 86];

/// The theme's trim a room wall is drawn in, resolved where the wall is
/// queued, so a painter reads no theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct WallTrim {
    /// What the glass's frame is lifted from.
    pub(crate) light: Rgb,
    /// The jamb posts where a doorway cuts the wall.
    pub(crate) dark: Rgb,
}

impl WallTrim {
    /// `theme`'s room-wall trim.
    pub(crate) fn of(theme: &Theme) -> Self {
        Self {
            light: theme.office.room_wall_trim_light,
            dark: theme.office.room_wall_trim_dark,
        }
    }
}

/// How a partition is seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// An E-W run, showing its face.
    Face,
    /// A N-S run, seen edge-on through more glass, so its haze sits a stop
    /// denser.
    EdgeOn,
}

/// One partition's glass on one painter's grid.
pub(crate) struct Glass {
    rim: Rgb,
    post: Rgb,
    pane: Stepped,
    view: View,
    depth: u16,
    per_unit: Density,
    /// Where each mullion stands along the run, in cells, west or north first:
    /// spread evenly over its [`clear_run`](WallPiece::clear_run), so none
    /// doubles a jamb or a joint's frame.
    posts: Vec<u16>,
}

impl Glass {
    /// `piece`'s glass on a grid of `per_unit` cells to a logical unit.
    pub(crate) fn of(trim: WallTrim, piece: WallPiece, per_unit: Density) -> Self {
        let (_, size) = piece.visual();
        let (view, depth) = match piece {
            WallPiece::Horizontal { .. } => (View::Face, size.h),
            WallPiece::Vertical { .. } => (View::EdgeOn, size.w),
        };
        let clear = piece.clear_run();
        let len = clear.end - clear.start;
        let panes = ((len + MULLION_STRIDE / 2) / MULLION_STRIDE).max(1);
        let posts = (1..panes)
            .map(|k| (clear.start + len * k / panes) * per_unit.get())
            .collect();
        let trim = trim.light;
        let lift = |[r, g, b]: [u8; 3]| Rgb {
            r: trim.r.saturating_add(r),
            g: trim.g.saturating_add(g),
            b: trim.b.saturating_add(b),
        };
        let denser = match view {
            View::Face => 0,
            View::EdgeOn => 1,
        };
        Self {
            rim: lift(RIM_LIFT),
            post: lift(POST_LIFT),
            pane: Stepped::new(PANE_LIFT + denser),
            view,
            depth: depth.saturating_mul(per_unit.get()),
            per_unit,
            posts,
        }
    }

    /// `under` seen through the cell `dx` across and `dy` down the wall's
    /// [`visual`](WallPiece::visual) box.
    pub(crate) fn over(&mut self, under: Rgb, dx: u16, dy: u16) -> Rgb {
        let (along, across) = match self.view {
            View::Face => (dx, dy),
            View::EdgeOn => (dy, dx),
        };
        let pane = self.posts.iter().rev().find(|&&p| p <= along);
        let mullion = pane == Some(&along);
        // Only a pane seen face-on catches one; edge-on, the glass shows only
        // its thickness.
        let glint = self.view == View::Face
            && (along - pane.unwrap_or(&0) + across)
                .checked_sub(GLINT_AT * self.per_unit.get())
                .is_some_and(|c| c == 0 || c == GLINT_GAP);
        if mullion || across + 1 == self.depth {
            self.post
        } else if across == 0 || glint {
            self.rim
        } else {
            self.pane.of(under)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEHIND: Rgb = Rgb {
        r: 220,
        g: 40,
        b: 40,
    };

    /// An E-W wall `units` long.
    const fn run(units: u16) -> WallPiece {
        WallPiece::Horizontal {
            x0: 0,
            x1: units - 1,
            y_face: 20,
            jamb_west: false,
            jamb_east: false,
        }
    }

    #[test]
    fn a_pane_is_what_is_behind_it_a_few_stops_lighter() {
        let mut glass = Glass::of(WallTrim::of(&crate::theme::NORMAL), run(40), Density::ONE);
        assert_eq!(glass.over(BEHIND, 3, 5), BEHIND.ramp(PANE_LIFT));
    }

    #[test]
    fn the_frame_is_the_themes_whatever_is_behind_it() {
        let mut glass = Glass::of(WallTrim::of(&crate::theme::NORMAL), run(40), Density::ONE);
        let other = Rgb {
            r: 10,
            g: 200,
            b: 90,
        };
        let depth = WallPiece::visual(run(40)).1.h;
        for (dx, dy) in [(3, 0), (MULLION_STRIDE, 5), (3, depth - 1)] {
            assert_eq!(
                glass.over(BEHIND, dx, dy),
                glass.over(other, dx, dy),
                "cell ({dx}, {dy}) is frame, not glass"
            );
        }
    }

    #[test]
    fn a_denser_grid_keeps_the_rhythm_in_logical_units() {
        let (mut one, mut four) = (
            Glass::of(WallTrim::of(&crate::theme::NORMAL), run(40), Density::ONE),
            Glass::of(
                WallTrim::of(&crate::theme::NORMAL),
                run(40),
                crate::display::pen::test_density(4),
            ),
        );
        // A row below the glints, whose strokes stay one cell wide.
        let row = 10;
        for x in 0..40 {
            assert_eq!(
                one.over(BEHIND, x, row),
                four.over(BEHIND, x * 4, row * 4),
                "cell {x} lands on the same part of the pane at 4x"
            );
        }
        assert_ne!(
            four.over(BEHIND, MULLION_STRIDE * 4, row * 4),
            four.over(BEHIND, MULLION_STRIDE * 4 + 1, row * 4),
            "a mullion is one cell wide at any density"
        );
    }

    #[test]
    fn a_face_on_pane_catches_a_glint_in_its_top_corner_at_any_density() {
        for per_unit in [1, 4] {
            let mut glass = Glass::of(
                WallTrim::of(&crate::theme::NORMAL),
                run(40),
                crate::display::pen::test_density(per_unit),
            );
            let (x, y) = (GLINT_AT * per_unit - per_unit, per_unit);
            assert_eq!(glass.over(BEHIND, x, y), glass.rim, "at {per_unit}x");
            assert_eq!(
                glass.over(BEHIND, x + 1, y),
                BEHIND.ramp(PANE_LIFT),
                "at {per_unit}x a stroke is one cell wide"
            );
        }
    }

    #[test]
    fn an_edge_on_run_catches_no_glint() {
        let piece = WallPiece::Vertical {
            x: 0,
            y_top: 0,
            y_bot: 39,
            north: 0,
            south: 39,
            jamb_north: false,
            jamb_south: false,
        };
        let mut glass = Glass::of(WallTrim::of(&crate::theme::NORMAL), piece, Density::ONE);
        let depth = piece.visual().1.w;
        for along in 0..40 {
            for across in 1..depth {
                assert_ne!(glass.over(BEHIND, across, along), glass.rim);
            }
        }
    }

    #[test]
    fn a_mullion_keeps_two_units_clear_of_every_jamb_and_joint() {
        const CLEAR: u16 = 2;
        let mut met = 0;
        for &(w, h) in crate::layout::roster::tests::CENSUS_SIZES {
            for seed in 0..12 {
                let l = crate::layout::SceneLayout::compute(
                    crate::layout::Viewport::full(w, h),
                    None,
                    seed,
                )
                .expect("lays out");
                for &piece in &l.wall_pieces {
                    let run = piece.clear_run();
                    for &post in
                        &Glass::of(WallTrim::of(&crate::theme::NORMAL), piece, Density::ONE).posts
                    {
                        met += 1;
                        assert!(
                            run.start + CLEAR <= post && post + CLEAR < run.end,
                            "{w}x{h} seed {seed}: {piece:?}'s post {post} in {run:?}"
                        );
                    }
                    // An E-W run's joints are its end units; a N-S run's stitched
                    // end runs into the crossing wall's box, which its clear run stops short of.
                    let WallPiece::Vertical { x, y_top, .. } = piece else {
                        continue;
                    };
                    let rows = y_top + run.start..y_top + run.end;
                    // A door's stub has no clear run, so no glass to meet anything.
                    if rows.is_empty() {
                        continue;
                    }
                    for other in l.wall_pieces.iter().filter(|&&p| p != piece) {
                        let (at, size) = other.visual();
                        assert!(
                            at.x + size.w <= x
                                || x + crate::layout::WALL_THICK_V <= at.x
                                || at.y + size.h <= rows.start
                                || rows.end <= at.y,
                            "{w}x{h} seed {seed}: {piece:?}'s clear rows {rows:?} meet {other:?}"
                        );
                    }
                }
            }
        }
        assert!(met > 0, "the sweep met a mullion");
    }
}
