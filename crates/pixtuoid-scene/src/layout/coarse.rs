//! Shared coarse routing-grid primitives — the ONE definition of the cell
//! coarsening the A\* router (`crate::pathfind`) and the reachability BFS
//! (`super::reach`) both ride. Sharing the cell size, walkability threshold,
//! neighbour rule and snap is what makes "reachable here" (`ReachSet`) agree
//! with "routable here" (A\*).

use super::{Bounds, Point};
use crate::grid::Grid;
use crate::walkable::WalkableMask;

/// Coarse-cell edge in px. Smaller = more accurate paths, more work per query.
/// `pathfind::CELL_SIZE` re-exports this value.
pub(crate) const COARSE_CELL_SIZE: u16 = 4;

/// Min pixels of a coarse cell's [`walk_piece`] for the cell to count as
/// walkable: low enough that the grid squeezes through the corridors the
/// meeting-room interior needs after furniture padding (tighter made the
/// meeting room unreachable), high enough not to graze furniture edges.
const COARSE_CELL_WALKABLE_MIN: u16 = 8;

/// The area the coarse grid tiles over an office's area `inner` whose floor
/// starts at row `top_margin`: `inner` from its first row on `top_margin`'s
/// pitch, so the cells stand where the floor does, the apron under the wall
/// band still among them.
pub(crate) fn routed_area(inner: Bounds, top_margin: u16) -> Bounds {
    let y = inner.y + (top_margin - inner.y) % COARSE_CELL_SIZE;
    Bounds {
        y,
        height: inner.bottom() - y,
        ..inner
    }
}

/// The coarse grid's size in cells: the whole cells across `mask`'s routed area.
pub(crate) fn cell_dims(mask: &WalkableMask) -> (u16, u16) {
    let routed = mask.routed();
    (
        routed.width / COARSE_CELL_SIZE,
        routed.height / COARSE_CELL_SIZE,
    )
}

/// The pixel `mask`'s coarse grid tiles from: its routed area's north-west
/// corner.
pub(crate) fn grid_origin(mask: &WalkableMask) -> Point {
    let routed = mask.routed();
    Point {
        x: routed.x,
        y: routed.y,
    }
}

/// The coarse cell holding pixel `p` on a grid tiled from `origin`, or `None`
/// west or north of it.
pub(crate) fn cell_holding(origin: Point, p: Point) -> Option<(u16, u16)> {
    Some((
        p.x.checked_sub(origin.x)? / COARSE_CELL_SIZE,
        p.y.checked_sub(origin.y)? / COARSE_CELL_SIZE,
    ))
}

/// The cell of `mask`'s coarse grid a snap toward pixel `p` starts from: the
/// one [`cell_holding`] it, its column or row clamped onto the grid's west or
/// north edge, and past its east or south edge an index off the grid, which
/// [`snap_where`] searches back from.
pub(crate) fn snap_cell(mask: &WalkableMask, p: Point) -> (u16, u16) {
    let origin = grid_origin(mask);
    let onto = Point {
        x: p.x.max(origin.x),
        y: p.y.max(origin.y),
    };
    // Clamped onto the grid's corner or past it, so never west or north of it.
    cell_holding(origin, onto).unwrap_or_default()
}

/// The north-west pixel of `mask`'s coarse cell `(cx, cy)`.
fn cell_corner(mask: &WalkableMask, cx: u16, cy: u16) -> Point {
    let origin = grid_origin(mask);
    Point {
        x: origin.x + cx * COARSE_CELL_SIZE,
        y: origin.y + cy * COARSE_CELL_SIZE,
    }
}

/// The centre pixel of `mask`'s coarse cell `(cx, cy)`.
pub(crate) fn cell_center(mask: &WalkableMask, cx: u16, cy: u16) -> Point {
    let corner = cell_corner(mask, cx, cy);
    Point {
        x: corner.x + COARSE_CELL_SIZE / 2,
        y: corner.y + COARSE_CELL_SIZE / 2,
    }
}

/// The pixel a route turns on in coarse cell `(cx, cy)`: its centre, or the
/// [`walk_piece`] pixel nearest it when the centre is off it. A cell counts as
/// walkable at half open, so the half holding its centre can be a wall's, and a
/// walker turning on the centre would stand in it.
pub(crate) fn cell_anchor(mask: &WalkableMask, cx: u16, cy: u16) -> Point {
    let centre = cell_center(mask, cx, cy);
    let piece = mask.coarse().cell((cx, cy)).piece;
    cell_pixels(mask, cx, cy)
        .filter(|&(bit, _)| piece & bit != 0)
        .map(|(_, p)| p)
        .min_by_key(|p| (p.x.abs_diff(centre.x) + p.y.abs_diff(centre.y), p.y, p.x))
        .unwrap_or(centre)
}

/// A coarse cell's pixels, one bit each, row-major.
type CellBits = u16;
const _: () = assert!(COARSE_CELL_SIZE * COARSE_CELL_SIZE <= CellBits::BITS as u16);

/// Each pixel of `mask`'s coarse cell `(cx, cy)` with its [`CellBits`] bit.
fn cell_pixels(
    mask: &WalkableMask,
    cx: u16,
    cy: u16,
) -> impl Iterator<Item = (CellBits, Point)> + use<> {
    let Point { x: x0, y: y0 } = cell_corner(mask, cx, cy);
    (0..COARSE_CELL_SIZE).flat_map(move |dy| {
        (0..COARSE_CELL_SIZE).map(move |dx| {
            (
                1 << (dy * COARSE_CELL_SIZE + dx),
                Point {
                    x: x0 + dx,
                    y: y0 + dy,
                },
            )
        })
    })
}

/// The bits of a cell's west column.
const WEST_COLUMN: CellBits = {
    let mut bits = 0;
    let mut row = 0;
    while row < COARSE_CELL_SIZE {
        bits |= 1 << (row * COARSE_CELL_SIZE);
        row += 1;
    }
    bits
};
/// The bits of a cell's east column.
const EAST_COLUMN: CellBits = WEST_COLUMN << (COARSE_CELL_SIZE - 1);

/// Where a walker stands in coarse cell `(cx, cy)`: the largest 4-connected
/// piece of its walkable pixels, the first in row order among equals. A wall's
/// corner can split a cell's open pixels into pieces touching only diagonally,
/// and a walker crossing between them cuts that corner.
fn walk_piece(mask: &WalkableMask, cx: u16, cy: u16) -> CellBits {
    let open_bits = cell_pixels(mask, cx, cy)
        .filter(|&(_, p)| mask.is_walkable(p.x, p.y))
        .fold(0, |bits, (bit, _)| bits | bit);
    let mut unseen = open_bits;
    let mut largest: CellBits = 0;
    while unseen != 0 {
        let mut piece = unseen.isolate_lowest_one();
        loop {
            let grown = (piece
                | ((piece << 1) & !WEST_COLUMN)
                | ((piece >> 1) & !EAST_COLUMN)
                | (piece << COARSE_CELL_SIZE)
                | (piece >> COARSE_CELL_SIZE))
                & open_bits;
            if grown == piece {
                break;
            }
            piece = grown;
        }
        if piece.count_ones() > largest.count_ones() {
            largest = piece;
        }
        unseen &= !piece;
    }
    largest
}

/// Is coarse cell `(cx, cy)` walkable — ≥ `COARSE_CELL_WALKABLE_MIN` pixels
/// of [`walk_piece`]?
pub(crate) fn cell_walkable(mask: &WalkableMask, cx: u16, cy: u16) -> bool {
    piece_walkable(mask.coarse().cell((cx, cy)).piece)
}

/// Is a cell whose [`walk_piece`] is `piece` walkable?
fn piece_walkable(piece: CellBits) -> bool {
    piece.count_ones() >= u32::from(COARSE_CELL_WALKABLE_MIN)
}

/// Can a walker cross from coarse cell `a` into the orthogonally adjacent `b`
/// — does some pixel of `a`'s [`walk_piece`] on their shared edge face one of
/// `b`'s? Both cells can be half open with the open halves on opposite sides,
/// meeting only at a pixel corner a straight leg between them cuts.
fn crossable((a, a_piece): ((u16, u16), CellBits), (b, b_piece): ((u16, u16), CellBits)) -> bool {
    let edge = |from: u16, to: u16| {
        let near = from * COARSE_CELL_SIZE;
        if to > from {
            (near + COARSE_CELL_SIZE - 1, near + COARSE_CELL_SIZE)
        } else {
            (near, near - 1)
        }
    };
    let on_piece = |cell: (u16, u16), bits: CellBits| {
        move |x: u16, y: u16| {
            let (dx, dy) = (x - cell.0 * COARSE_CELL_SIZE, y - cell.1 * COARSE_CELL_SIZE);
            bits & (1 << (dy * COARSE_CELL_SIZE + dx)) != 0
        }
    };
    let (on_a, on_b) = (on_piece(a, a_piece), on_piece(b, b_piece));
    if a.1 == b.1 {
        let (xa, xb) = edge(a.0, b.0);
        let y0 = a.1 * COARSE_CELL_SIZE;
        (0..COARSE_CELL_SIZE).any(|d| on_a(xa, y0 + d) && on_b(xb, y0 + d))
    } else {
        let (ya, yb) = edge(a.1, b.1);
        let x0 = a.0 * COARSE_CELL_SIZE;
        (0..COARSE_CELL_SIZE).any(|d| on_a(x0 + d, ya) && on_b(x0 + d, yb))
    }
}

/// The steps out of a coarse cell, the four orthogonal ones first, in the
/// order [`CoarseGraph::neighbors`] yields them: A\*'s heap breaks its ties by
/// push order, so this order is part of every route.
const STEPS: [(i16, i16); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (1, -1),
    (-1, 1),
    (-1, -1),
];
/// How many of [`STEPS`] are orthogonal.
const ORTHOGONAL_STEPS: usize = 4;

/// A set of [`STEPS`], bit `i` for `STEPS[i]`.
type StepSet = u8;
const _: () = assert!(STEPS.len() <= StepSet::BITS as usize);

/// The bit of `StepSet` for step `d`, one of [`STEPS`].
fn step_bit(d: (i16, i16)) -> StepSet {
    STEPS.iter().position(|&s| s == d).map_or(0, |i| 1 << i)
}

fn steps_where(steps: &[(i16, i16)], can: impl Fn((i16, i16)) -> bool) -> StepSet {
    steps
        .iter()
        .filter(|&&d| can(d))
        .fold(0, |set, &d| set | step_bit(d))
}

/// `cell` moved by `d`, or `None` west or north of the grid.
fn offset(cell: (u16, u16), (dx, dy): (i16, i16)) -> Option<(u16, u16)> {
    Some((
        cell.0.checked_add_signed(dx)?,
        cell.1.checked_add_signed(dy)?,
    ))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CoarseCell {
    /// Its [`walk_piece`].
    piece: CellBits,
    steps: StepSet,
}

/// The coarse grid over a [`WalkableMask`]: each cell's [`walk_piece`] and the
/// steps out of it, worked out once when the mask freezes. The A\* expansion
/// and the reach BFS both step through [`CoarseGraph::neighbors`], so
/// "reachable" and "routable" share ONE neighbour rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CoarseGraph {
    cells: Grid<CoarseCell>,
}

impl CoarseGraph {
    /// `mask`'s graph, worked out from its pixels alone.
    pub(crate) fn new(mask: &WalkableMask) -> Self {
        let (w, h) = cell_dims(mask);
        let pieces = Grid::from_fn(w, h, |cx, cy| walk_piece(mask, cx, cy));
        let piece = |c: (u16, u16)| pieces.get_or(c.0, c.1, 0);
        // An orthogonal step needs the neighbour walkable and `crossable`.
        let orthogonal = |from: (u16, u16), d| {
            offset(from, d).is_some_and(|to| {
                piece_walkable(piece(to)) && crossable((from, piece(from)), (to, piece(to)))
            })
        };
        let orthogonals = Grid::from_fn(w, h, |cx, cy| {
            steps_where(&STEPS[..ORTHOGONAL_STEPS], |d| orthogonal((cx, cy), d))
        });
        // A diagonal step needs BOTH orthogonal cells it squeezes between
        // steppable on the way: a walker's straight leg between two diagonal
        // cells crosses their shared corner, so a blocked orthogonal cell puts
        // that corner inside a wall.
        let stepped = |from: (u16, u16), d| {
            (orthogonals.get_or(from.0, from.1, 0) & step_bit(d) != 0)
                .then(|| offset(from, d))
                .flatten()
        };
        let diagonal = |c: (u16, u16), (dx, dy)| {
            let via = |first, then| stepped(c, first).and_then(|on| stepped(on, then));
            via((dx, 0), (0, dy)).is_some() && via((0, dy), (dx, 0)).is_some()
        };
        CoarseGraph {
            cells: Grid::from_fn(w, h, |cx, cy| CoarseCell {
                piece: piece((cx, cy)),
                steps: orthogonals.get_or(cx, cy, 0)
                    | steps_where(&STEPS[ORTHOGONAL_STEPS..], |d| diagonal((cx, cy), d)),
            }),
        }
    }

    /// Coarse cell `(cx, cy)`, or one with no piece and no steps off the grid.
    fn cell(&self, (cx, cy): (u16, u16)) -> CoarseCell {
        self.cells.get_or(cx, cy, CoarseCell::default())
    }

    /// The 8-neighbours of `cell` a walker can step to, each flagged `true`
    /// when the step is diagonal.
    pub(crate) fn neighbors(
        &self,
        cell: (u16, u16),
    ) -> impl Iterator<Item = ((u16, u16), bool)> + use<> {
        let steps = self.cell(cell).steps;
        STEPS
            .into_iter()
            .enumerate()
            .filter(move |&(i, _)| steps & (1 << i) != 0)
            .filter_map(move |(i, d)| Some((offset(cell, d)?, i >= ORTHOGONAL_STEPS)))
    }
}

/// Snap coarse `cell` to the nearest walkable coarse cell of the
/// `cell_w × cell_h` grid within `max_radius` rings (Chebyshev), or `None` when
/// none is in range.
pub(crate) fn snap(
    mask: &WalkableMask,
    cell: (u16, u16),
    cell_w: u16,
    cell_h: u16,
    max_radius: u16,
) -> Option<(u16, u16)> {
    snap_where(cell, cell_w, cell_h, max_radius, |(x, y)| {
        cell_walkable(mask, x, y)
    })
}

/// [`snap`] to the nearest in-grid cell that `fits`.
pub(crate) fn snap_where(
    cell: (u16, u16),
    cell_w: u16,
    cell_h: u16,
    max_radius: u16,
    mut fits: impl FnMut((u16, u16)) -> bool,
) -> Option<(u16, u16)> {
    if cell.0 < cell_w && cell.1 < cell_h && fits(cell) {
        return Some(cell);
    }
    for r in 1..=max_radius {
        let r_i = i32::from(r);
        for dy in -r_i..=r_i {
            for dx in -r_i..=r_i {
                if dx.abs() != r_i && dy.abs() != r_i {
                    continue; // ring only
                }
                let nx = i32::from(cell.0) + dx;
                let ny = i32::from(cell.1) + dy;
                if nx < 0 || ny < 0 {
                    continue;
                }
                let (nx, ny) = (nx as u16, ny as u16);
                if nx >= cell_w || ny >= cell_h {
                    continue;
                }
                if fits((nx, ny)) {
                    return Some((nx, ny));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walkable::WalkableMaskBuilder;

    fn neighbours(mask: &WalkableMask, cell: (u16, u16)) -> Vec<(u16, u16)> {
        mask.coarse().neighbors(cell).map(|(c, _)| c).collect()
    }

    #[test]
    fn a_diagonal_step_past_a_blocked_orthogonal_cell_is_refused() {
        let mut m = WalkableMaskBuilder::new_open(32, 32);
        // Cell (2, 1) blocked: the step (1, 1) -> (2, 2) would cut its corner.
        m.mark_blocked(
            2 * COARSE_CELL_SIZE,
            COARSE_CELL_SIZE,
            COARSE_CELL_SIZE,
            COARSE_CELL_SIZE,
            0,
        );
        let m = m.freeze();
        let n = neighbours(&m, (1, 1));
        assert!(!n.contains(&(2, 2)) && !n.contains(&(2, 0)));
        assert!(
            n.contains(&(0, 2)) && n.contains(&(1, 2)),
            "the open side still steps"
        );
        let n = neighbours(&m, (2, 2));
        assert!(
            !n.contains(&(1, 1)) && !n.contains(&(3, 1)),
            "nor past a blocked vertical neighbour"
        );
    }

    #[test]
    fn half_open_cells_meeting_at_a_pixel_corner_are_not_crossable() {
        let mut m = WalkableMaskBuilder::new_open(32, 32);
        let s = COARSE_CELL_SIZE;
        // Cell (1, 1) open only in its north half, cell (2, 1) only in its south
        // half: both walkable, their open halves touching at one pixel corner.
        m.mark_blocked(s, s + s / 2, s, s / 2, 0);
        m.mark_blocked(2 * s, s, s, s / 2, 0);
        let m = m.freeze();
        assert!(cell_walkable(&m, 1, 1));
        assert!(cell_walkable(&m, 2, 1));
        assert!(!neighbours(&m, (1, 1)).contains(&(2, 1)));
        assert!(!neighbours(&m, (2, 1)).contains(&(1, 1)));
    }

    /// A wall's corner splits cell (1, 1)'s open pixels into a piece of six
    /// and one of two touching it only diagonally: enough open pixels, but
    /// no piece big enough to stand on.
    #[test]
    fn a_cell_counts_only_its_largest_walk_piece() {
        let s = COARSE_CELL_SIZE;
        let cornered = |joined: bool| {
            let mut m = WalkableMaskBuilder::new_open(32, 32);
            m.mark_blocked(s, s, s, s, 0);
            m.mark_walkable(s + 1, s, s - 1, s / 2);
            m.mark_walkable(s, s + s / 2, 1, s / 2);
            if joined {
                m.mark_walkable(s, s + 1, 1, 1);
            }
            m.freeze()
        };
        let m = cornered(false);
        let open = cell_pixels(&m, 1, 1)
            .filter(|&(_, p)| m.is_walkable(p.x, p.y))
            .count();
        assert_eq!(open, usize::from(COARSE_CELL_WALKABLE_MIN));
        assert!(!cell_walkable(&m, 1, 1));
        assert!(
            cell_walkable(&cornered(true), 1, 1),
            "joined into one piece, it stands"
        );
    }
}
