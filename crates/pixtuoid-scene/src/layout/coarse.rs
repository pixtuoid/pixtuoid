//! Shared coarse routing-grid primitives — the ONE definition of the cell
//! coarsening the A\* router (`crate::pathfind`) and the reachability BFS
//! (`super::reach`) both ride. Sharing the cell size, walkability threshold,
//! neighbour rule and snap is what makes "reachable here" (`ReachSet`) agree
//! with "routable here" (A\*).

use super::Point;
use crate::grid::Grid;
use crate::walkable::{OccupancyOverlay, WalkableMask};

/// Coarse-cell edge in px. Smaller = more accurate paths, more work per query.
/// `pathfind::CELL_SIZE` re-exports this value.
pub(crate) const COARSE_CELL_SIZE: u16 = 4;

/// Min pixels of a coarse cell's [`walk_piece`] for the cell to count as
/// walkable: low enough that the grid squeezes through the corridors the
/// meeting-room interior needs after furniture padding (tighter made the
/// meeting room unreachable), high enough not to graze furniture edges.
const COARSE_CELL_WALKABLE_MIN: u16 = 8;

/// The centre pixel of coarse cell `(cx, cy)`.
pub(crate) fn cell_center(cx: u16, cy: u16) -> Point {
    Point {
        x: cx * COARSE_CELL_SIZE + COARSE_CELL_SIZE / 2,
        y: cy * COARSE_CELL_SIZE + COARSE_CELL_SIZE / 2,
    }
}

/// The pixel a route turns on in coarse cell `(cx, cy)`: its centre, or the
/// [`walk_piece`] pixel nearest it when the centre is off it. A cell counts as
/// walkable at half open, so the half holding its centre can be a wall's, and a
/// walker turning on the centre would stand in it.
pub(crate) fn cell_anchor(mask: &WalkableMask, cx: u16, cy: u16) -> Point {
    let centre = cell_center(cx, cy);
    let piece = walk_piece(mask, &OccupancyOverlay::new(), cx, cy);
    cell_pixels(cx, cy)
        .filter(|&(bit, _)| piece & bit != 0)
        .map(|(_, p)| p)
        .min_by_key(|p| (p.x.abs_diff(centre.x) + p.y.abs_diff(centre.y), p.y, p.x))
        .unwrap_or(centre)
}

/// A coarse cell's pixels, one bit each, row-major.
type CellBits = u16;
const _: () = assert!(COARSE_CELL_SIZE * COARSE_CELL_SIZE <= CellBits::BITS as u16);

/// Each pixel of coarse cell `(cx, cy)` with its [`CellBits`] bit.
fn cell_pixels(cx: u16, cy: u16) -> impl Iterator<Item = (CellBits, Point)> {
    let (x0, y0) = (cx * COARSE_CELL_SIZE, cy * COARSE_CELL_SIZE);
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
/// piece of its [`open`] pixels, the first in row order among equals. A wall's
/// corner can split a cell's open pixels into pieces touching only diagonally,
/// and a walker crossing between them cuts that corner.
fn walk_piece(mask: &WalkableMask, overlay: &OccupancyOverlay, cx: u16, cy: u16) -> CellBits {
    let open_bits = cell_pixels(cx, cy)
        .filter(|&(_, p)| open(mask, overlay, p.x, p.y))
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

/// Is pixel `(x, y)` open — walkable on the static `mask` and clear of the
/// per-frame `overlay`?
fn open(mask: &WalkableMask, overlay: &OccupancyOverlay, x: u16, y: u16) -> bool {
    mask.is_walkable(x, y) && (overlay.is_empty() || !overlay.blocks(x, y))
}

/// Is coarse cell `(cx, cy)` walkable — ≥ `COARSE_CELL_WALKABLE_MIN` pixels
/// of [`walk_piece`]? The reach BFS passes an EMPTY overlay (static geometry only);
/// the router passes the live occupancy overlay.
pub(crate) fn cell_walkable(
    mask: &WalkableMask,
    overlay: &OccupancyOverlay,
    cx: u16,
    cy: u16,
) -> bool {
    piece_walkable(walk_piece(mask, overlay, cx, cy))
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

/// A once-computed answer, or not yet asked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Memo {
    Unknown,
    No,
    Yes,
}

/// One search's view of the coarse grid over a `mask` and an `overlay`: each
/// cell's [`walk_piece`] and each edge's crossability is computed at most
/// once, then read by every step. The A\* expansion and the reach BFS both step
/// through [`CoarseGrid::neighbors`], so "reachable" and "routable" share ONE
/// neighbour rule.
pub(crate) struct CoarseGrid<'a> {
    mask: &'a WalkableMask,
    overlay: &'a OccupancyOverlay,
    pieces: Grid<Option<CellBits>>,
    /// Crossability from each cell to its east neighbour.
    east: Grid<Memo>,
    /// Crossability from each cell to its south neighbour.
    south: Grid<Memo>,
}

impl<'a> CoarseGrid<'a> {
    pub(crate) fn new(mask: &'a WalkableMask, overlay: &'a OccupancyOverlay) -> Self {
        let (w, h) = (
            mask.width() / COARSE_CELL_SIZE,
            mask.height() / COARSE_CELL_SIZE,
        );
        CoarseGrid {
            mask,
            overlay,
            pieces: Grid::filled(w, h, None),
            east: Grid::filled(w, h, Memo::Unknown),
            south: Grid::filled(w, h, Memo::Unknown),
        }
    }

    fn piece(&mut self, (cx, cy): (u16, u16)) -> CellBits {
        if let Some(piece) = self.pieces.get_or(cx, cy, None) {
            return piece;
        }
        let piece = walk_piece(self.mask, self.overlay, cx, cy);
        self.pieces.set(cx, cy, Some(piece));
        piece
    }

    fn crossable(&mut self, a: (u16, u16), b: (u16, u16)) -> bool {
        let (a_piece, b_piece) = (self.piece(a), self.piece(b));
        let (lo, edges) = match (a.1 == b.1, a < b) {
            (true, true) => (a, &mut self.east),
            (true, false) => (b, &mut self.east),
            (false, true) => (a, &mut self.south),
            (false, false) => (b, &mut self.south),
        };
        memo(edges, lo.0, lo.1, || crossable((a, a_piece), (b, b_piece)))
    }

    /// The 8-neighbours of `cell` a walker can step to, each flagged `true`
    /// when the step is diagonal. An orthogonal step needs the neighbour
    /// walkable and `crossable`. A diagonal step needs BOTH orthogonal cells it
    /// squeezes between steppable on the way: a walker's straight leg between
    /// two diagonal cells crosses their shared corner, so a blocked orthogonal
    /// cell puts that corner inside a wall.
    pub(crate) fn neighbors(
        &mut self,
        cell: (u16, u16),
    ) -> impl Iterator<Item = ((u16, u16), bool)> + use<> {
        let [e, w, s, n] = [(1, 0), (-1, 0), (0, 1), (0, -1)].map(|d| self.step(Some(cell), d));
        let mut diagonal = |a: Option<(u16, u16)>, b: Option<(u16, u16)>, (dx, dy)| {
            let via_a = self.step(a, (0, dy))?;
            self.step(b, (dx, 0)).and(Some(via_a))
        };
        [
            (e, false),
            (w, false),
            (s, false),
            (n, false),
            (diagonal(e, s, (1, 1)), true),
            (diagonal(e, n, (1, -1)), true),
            (diagonal(w, s, (-1, 1)), true),
            (diagonal(w, n, (-1, -1)), true),
        ]
        .into_iter()
        .filter_map(|(c, diag)| c.map(|c| (c, diag)))
    }

    /// The orthogonal neighbour `from + d`, when it is walkable and crossable.
    fn step(&mut self, from: Option<(u16, u16)>, (dx, dy): (i32, i32)) -> Option<(u16, u16)> {
        let from = from?;
        let to = (
            u16::try_from(i32::from(from.0) + dx).ok()?,
            u16::try_from(i32::from(from.1) + dy).ok()?,
        );
        let in_grid = to.0 < self.pieces.width() && to.1 < self.pieces.height();
        (in_grid && piece_walkable(self.piece(to)) && self.crossable(from, to)).then_some(to)
    }
}

/// `grid[x, y]`, computing it with `f` on first ask.
fn memo(grid: &mut Grid<Memo>, x: u16, y: u16, f: impl FnOnce() -> bool) -> bool {
    match grid.get_or(x, y, Memo::Unknown) {
        Memo::Yes => true,
        Memo::No => false,
        Memo::Unknown => {
            let v = f();
            grid.set(x, y, if v { Memo::Yes } else { Memo::No });
            v
        }
    }
}

/// Snap coarse `cell` to the nearest walkable coarse cell of the
/// `cell_w × cell_h` grid within `max_radius` rings (Chebyshev), or `None` when
/// none is in range.
pub(crate) fn snap(
    mask: &WalkableMask,
    overlay: &OccupancyOverlay,
    cell: (u16, u16),
    cell_w: u16,
    cell_h: u16,
    max_radius: u16,
) -> Option<(u16, u16)> {
    snap_where(cell, cell_w, cell_h, max_radius, |(x, y)| {
        cell_walkable(mask, overlay, x, y)
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

    fn neighbours(mask: &WalkableMask, cell: (u16, u16)) -> Vec<(u16, u16)> {
        let overlay = OccupancyOverlay::new();
        CoarseGrid::new(mask, &overlay)
            .neighbors(cell)
            .map(|(c, _)| c)
            .collect()
    }

    #[test]
    fn a_diagonal_step_past_a_blocked_orthogonal_cell_is_refused() {
        let mut m = WalkableMask::new_open(32, 32);
        // Cell (2, 1) blocked: the step (1, 1) -> (2, 2) would cut its corner.
        m.mark_blocked(
            2 * COARSE_CELL_SIZE,
            COARSE_CELL_SIZE,
            COARSE_CELL_SIZE,
            COARSE_CELL_SIZE,
            0,
        );
        let n = neighbours(&m, (1, 1));
        assert!(!n.contains(&(2, 2)) && !n.contains(&(2, 0)));
        assert!(
            n.contains(&(0, 2)) && n.contains(&(1, 2)),
            "the open side still steps"
        );
    }

    #[test]
    fn half_open_cells_meeting_at_a_pixel_corner_are_not_crossable() {
        let mut m = WalkableMask::new_open(32, 32);
        let s = COARSE_CELL_SIZE;
        // Cell (1, 1) open only in its north half, cell (2, 1) only in its south
        // half: both walkable, their open halves touching at one pixel corner.
        m.mark_blocked(s, s + s / 2, s, s / 2, 0);
        m.mark_blocked(2 * s, s, s, s / 2, 0);
        assert!(cell_walkable(&m, &OccupancyOverlay::new(), 1, 1));
        assert!(cell_walkable(&m, &OccupancyOverlay::new(), 2, 1));
        assert!(!neighbours(&m, (1, 1)).contains(&(2, 1)));
        assert!(!neighbours(&m, (2, 1)).contains(&(1, 1)));
    }

    /// A wall's corner splits cell (1, 1)'s open pixels into a piece of six
    /// and one of two touching it only diagonally: enough open pixels, but
    /// no piece big enough to stand on.
    #[test]
    fn a_cell_counts_only_its_largest_walk_piece() {
        let mut m = WalkableMask::new_open(32, 32);
        let s = COARSE_CELL_SIZE;
        m.mark_blocked(s, s, s, s, 0);
        m.mark_walkable(s + 1, s, s - 1, s / 2);
        m.mark_walkable(s, s + s / 2, 1, s / 2);
        let open = cell_pixels(1, 1)
            .filter(|&(_, p)| m.is_walkable(p.x, p.y))
            .count();
        assert_eq!(open, usize::from(COARSE_CELL_WALKABLE_MIN));
        assert!(!cell_walkable(&m, &OccupancyOverlay::new(), 1, 1));
        m.mark_walkable(s, s + 1, 1, 1);
        assert!(
            cell_walkable(&m, &OccupancyOverlay::new(), 1, 1),
            "joined into one piece, it stands"
        );
    }
}
