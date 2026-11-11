//! Pivot conventions: WHERE a furniture/decor box sits relative to its layout
//! `pos`. Shared by the walkable mask (ground footprint rect) and the renderer
//! (sprite blit origin + y-sort row), so the three representations of the same
//! fact cannot drift.
//!
//! The pivot is a property of the PLACEMENT ROLE, not the furniture — so the
//! call site passes it explicitly rather than reading it off `FurnitureDef`.
//! `Furniture::Whiteboard` proves why: it is `Center` as pod-aisle decor but
//! `TopLeft` as a wall-hung board. One geometry row, two placement conventions —
//! a single per-furniture pivot field could not represent both.

use super::{Bounds, Point, Size};

/// How a `(w, h)` box is positioned relative to its `pos`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pivot {
    /// `pos` is the box CENTER. Top-left = `pos - size/2`; the y-sort row is the
    /// box's south (front) edge. Most furniture.
    Center,
    /// `pos` is the box's NW corner. Top-left = `pos`; y-sort row = `pos.y+h-1`.
    /// Wall-hung decor and the home desk.
    TopLeft,
}

/// Top-left corner of a `(w, h)` box anchored at `pos`. Used for BOTH the mask
/// footprint rect (pass the footprint size) and the sprite blit origin (pass the
/// visual size), so blocked ground and painted sprite can't diverge.
pub(crate) fn anchored_top_left(pivot: Pivot, pos: Point, w: u16, h: u16) -> Point {
    match pivot {
        Pivot::Center => Point {
            x: pos.x.saturating_sub(w / 2),
            y: pos.y.saturating_sub(h / 2),
        },
        Pivot::TopLeft => pos,
    }
}

/// The `size` box whose top-left is `tl`.
pub(super) fn boxed(tl: Point, size: Size) -> Bounds {
    Bounds {
        x: tl.x,
        y: tl.y,
        width: size.w,
        height: size.h,
    }
}

/// A centre-pinned `size` box at `pos`.
pub(super) fn centred(pos: Point, size: Size) -> Bounds {
    boxed(anchored_top_left(Pivot::Center, pos, size.w, size.h), size)
}

/// [`Bounds::overlaps`] for two `(top_left, size)` rects.
pub(super) fn rects_overlap((a, a_size): (Point, Size), (b, b_size): (Point, Size)) -> bool {
    boxed(a, a_size).overlaps(boxed(b, b_size))
}

/// Does `probe` overlap `obstacle` grown by `clearance` px on every side? Callers
/// pass a Center-anchored `obstacle` top-left (via [`anchored_top_left`]) so the
/// grown rect matches the sprite.
pub(super) fn overlaps_within_clearance(
    probe: (Point, Size),
    obstacle: (Point, Size),
    clearance: u16,
) -> bool {
    let (tl, sz) = obstacle;
    let grown_tl = Point {
        x: tl.x.saturating_sub(clearance),
        y: tl.y.saturating_sub(clearance),
    };
    let grown = Size {
        w: sz.w + 2 * clearance,
        h: sz.h + 2 * clearance,
    };
    rects_overlap(probe, (grown_tl, grown))
}

/// The sort row of a sprite of height `h` placed at `pos`: its south
/// (front) base row. Derived from [`anchored_top_left`] so it can NEVER drift
/// from where the sprite actually blits (`origin.y + h - 1`).
pub(crate) fn sort_row_at(pivot: Pivot, pos: Point, h: u16) -> u16 {
    anchored_top_left(pivot, pos, 0, h)
        .y
        .saturating_add(h.saturating_sub(1))
}

/// The centre `y` an [`Pivot::Center`] sprite `h` tall takes to stand on row
/// `base`: [`sort_row_at`]'s inverse.
pub(super) fn centre_y_standing_on(base: u16, h: u16) -> u16 {
    (base + 1).saturating_sub(h - h / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sprite_centred_to_stand_on_a_row_sorts_on_it() {
        for h in 1u16..24 {
            let base = 40;
            let pos = Point {
                x: 50,
                y: centre_y_standing_on(base, h),
            };
            assert_eq!(sort_row_at(Pivot::Center, pos, h), base, "h={h}");
        }
    }

    #[test]
    fn sort_row_at_is_the_sprite_south_row_for_every_pivot() {
        let pos = Point { x: 50, y: 40 };
        for &p in &[Pivot::Center, Pivot::TopLeft] {
            for h in 1u16..24 {
                let tl = anchored_top_left(p, pos, 8, h);
                assert_eq!(
                    sort_row_at(p, pos, h),
                    tl.y + h - 1,
                    "{p:?} h={h}: sort row must equal the box south row"
                );
            }
        }
    }

    #[test]
    fn center_sort_row_is_half_of_h_less_one_below_pos() {
        let pos = Point { x: 30, y: 25 };
        for h in 1u16..24 {
            assert_eq!(
                sort_row_at(Pivot::Center, pos, h),
                pos.y + (h - 1) / 2,
                "h={h}"
            );
        }
    }

    #[test]
    fn topleft_origin_is_pos() {
        let pos = Point { x: 7, y: 9 };
        assert_eq!(anchored_top_left(Pivot::TopLeft, pos, 14, 11), pos);
        assert_eq!(sort_row_at(Pivot::TopLeft, pos, 11), pos.y + 10);
    }

    #[test]
    fn center_origin_is_pos_minus_half() {
        let pos = Point { x: 40, y: 30 };
        assert_eq!(
            anchored_top_left(Pivot::Center, pos, 8, 6),
            Point { x: 36, y: 27 }
        );
    }
}
