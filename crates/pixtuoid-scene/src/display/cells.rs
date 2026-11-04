//! Screen text: a grid of terminal cells, the one form the footer, tooltips
//! and panels take before a painter draws them, as terminal cells or in the
//! pixel font ([`paint_grid`](crate::cutaway::paint_grid)). Its cells are the
//! ones ratatui's buffer writes ([`text::cells`](super::text::cells)), so
//! both draw the same layout.

use crate::sprite::Rgb;

use super::text::clusters;

/// How far a card's drop shadow darkens what is under it, toward black. One
/// uniform factor is the owner's preference, not an unfinished gradient.
pub const CARD_SHADOW: f32 = 0.42;

/// A rectangle of cells: its top-left and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// A screen cell's size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellPx {
    pub w: u16,
    pub h: u16,
}

/// A terminal's cell grid over the office: which cell shows a logical point,
/// and which logical units a cell shows. The classic's half-block is one
/// such grid and a cutaway's cells another, so world text lands by one rule
/// on both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellMap {
    /// Real pixels per logical unit.
    pub scale: u16,
    /// A cell's size in those pixels.
    pub cell: CellPx,
}

impl CellMap {
    /// The classic's: a cell one logical column wide and
    /// [`CELL_ROWS`](crate::layout::CELL_ROWS) tall.
    pub const HALF_BLOCK: Self = Self {
        scale: 1,
        cell: CellPx {
            w: 1,
            h: crate::layout::CELL_ROWS,
        },
    };

    /// The cell `(col, row)` a line of world text lands in whose classic
    /// cell lies over logical point `p`: the one under that cell's centre.
    /// World text is laid out on the classic's grid, and a cell holding a
    /// fraction of a unit either way rounds it by its middle, never its edge.
    pub(crate) fn cell_of(self, p: crate::layout::Point) -> (u16, u16) {
        let rows = crate::layout::CELL_ROWS;
        // Doubled, so the centre, half a column in and half a cell row down,
        // stays whole; u32, as a far point's pixel offset passes `u16::MAX`.
        let cell = |twice: u32, px: u16| {
            u16::try_from(twice * u32::from(self.scale) / (2 * u32::from(px.max(1))))
                .unwrap_or(u16::MAX)
        };
        (
            cell(2 * u32::from(p.x) + 1, self.cell.w),
            cell(
                2 * u32::from(p.y / rows * rows) + u32::from(rows),
                self.cell.h,
            ),
        )
    }

    /// The columns and the rows of the cells that show any of `b`.
    pub(crate) fn cells_showing(self, b: crate::layout::Bounds) -> [std::ops::Range<u16>; 2] {
        let cells = |from: u16, len: u16, px: u16| {
            let cell = |units: u16, round: fn(u32, u32) -> u32| {
                u16::try_from(round(
                    u32::from(units) * u32::from(self.scale),
                    u32::from(px.max(1)),
                ))
                .unwrap_or(u16::MAX)
            };
            cell(from, |a, b| a / b)..cell(from.saturating_add(len), u32::div_ceil)
        };
        [
            cells(b.x, b.width, self.cell.w),
            cells(b.y, b.height, self.cell.h),
        ]
    }

    /// The logical units cell `(col, row)` shows any part of; past the far
    /// edge, units nothing paints.
    pub fn area(self, col: u16, row: u16) -> crate::layout::Bounds {
        let units = |i: u16, px: u16| {
            let first = u32::from(i) * u32::from(px);
            let unit = |p: u32| u16::try_from(p / u32::from(self.scale.max(1))).unwrap_or(u16::MAX);
            let (a, b) = (unit(first), unit((first + u32::from(px)).saturating_sub(1)));
            (a, b - a + 1)
        };
        let ((x, width), (y, height)) = (units(col, self.cell.w), units(row, self.cell.h));
        crate::layout::Bounds {
            x,
            y,
            width,
            height,
        }
    }

    /// The logical units the cells of `r` show any part of.
    pub fn area_of(self, r: CellRect) -> crate::layout::Bounds {
        let (first, last) = (
            self.area(r.x, r.y),
            self.area(
                r.x.saturating_add(r.w.max(1) - 1),
                r.y.saturating_add(r.h.max(1) - 1),
            ),
        );
        crate::layout::Bounds {
            width: last.x - first.x + last.width,
            height: last.y - first.y + last.height,
            ..first
        }
    }
}

/// One cell of screen text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridCell {
    /// The grapheme cluster it shows; empty for the cells after a wide one's
    /// first, which it covers.
    pub symbol: String,
    /// Its ink; `None` is the painter's own text colour.
    pub fg: Option<Rgb>,
    /// Its fill; `None` leaves what is under it.
    pub bg: Option<Rgb>,
    pub bold: bool,
}

impl GridCell {
    const BLANK: &str = " ";

    fn blank() -> Self {
        Self {
            symbol: Self::BLANK.to_owned(),
            fg: None,
            bg: None,
            bold: false,
        }
    }
}

/// A `w`×`h` grid of [`GridCell`]s, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellGrid {
    w: u16,
    h: u16,
    cells: Vec<GridCell>,
}

impl CellGrid {
    /// A `w`×`h` grid of blank cells with no ink or fill.
    pub fn new(w: u16, h: u16) -> Self {
        Self {
            w,
            h,
            cells: vec![GridCell::blank(); usize::from(w) * usize::from(h)],
        }
    }

    pub fn width(&self) -> u16 {
        self.w
    }

    pub fn height(&self) -> u16 {
        self.h
    }

    /// Its size, at the origin.
    pub fn rect(&self) -> CellRect {
        CellRect {
            w: self.w,
            h: self.h,
            ..CellRect::default()
        }
    }

    /// The cell at `(x, y)`, `None` off the grid.
    pub fn get(&self, x: u16, y: u16) -> Option<&GridCell> {
        (x < self.w && y < self.h)
            .then(|| {
                self.cells
                    .get(usize::from(y) * usize::from(self.w) + usize::from(x))
            })
            .flatten()
    }

    fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut GridCell> {
        (x < self.w && y < self.h)
            .then(|| {
                self.cells
                    .get_mut(usize::from(y) * usize::from(self.w) + usize::from(x))
            })
            .flatten()
    }

    /// Replace the cell at `(x, y)`; off the grid, nothing.
    pub fn set(&mut self, (x, y): (u16, u16), cell: GridCell) {
        if let Some(at) = self.get_mut(x, y) {
            *at = cell;
        }
    }

    /// Fill every cell with `bg`.
    pub(crate) fn fill(&mut self, bg: Rgb) {
        for cell in &mut self.cells {
            cell.bg = Some(bg);
        }
    }

    /// Write `text` from `(x, y)` in `fg`, a cluster a cell (or two, wide),
    /// as ratatui's buffer writes it; a cluster that would cross the right
    /// edge is dropped with the rest. Returns the column after it.
    pub fn put(&mut self, (x, y): (u16, u16), text: &str, fg: Option<Rgb>, bold: bool) -> u16 {
        let mut col = x;
        for (cluster, n) in clusters(text) {
            if col.saturating_add(n) > self.w {
                break;
            }
            for i in 0..n {
                if let Some(cell) = self.get_mut(col + i, y) {
                    cell.symbol = if i == 0 {
                        cluster.to_owned()
                    } else {
                        String::new()
                    };
                    cell.fg = fg;
                    cell.bold = bold;
                }
            }
            col += n;
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INK: Rgb = Rgb { r: 9, g: 8, b: 7 };

    fn symbols(grid: &CellGrid, y: u16) -> Vec<String> {
        (0..grid.width())
            .map(|x| grid.get(x, y).map(|c| c.symbol.clone()).unwrap_or_default())
            .collect()
    }

    /// A point's classic cell lands where its centre shows, on the
    /// half-block, where that is its own cell, and on grids whose cells hold
    /// a fractional number of units either way.
    #[test]
    fn a_classic_cell_lands_under_its_centre() {
        let pixel = |w, h, scale| CellMap {
            scale,
            cell: CellPx { w, h },
        };
        for map in [
            CellMap::HALF_BLOCK,
            pixel(14, 34, 16),
            pixel(8, 18, 4),
            pixel(10, 20, 4),
        ] {
            for (x, y) in (0..60).flat_map(|y| (0..60).map(move |x| (x, y))) {
                let (col, row) = map.cell_of(crate::layout::Point { x, y });
                // The centre's real pixel, doubled: half a column in, half a
                // cell row down.
                let rows = crate::layout::CELL_ROWS;
                let twice = |units: u16| u32::from(units) * u32::from(map.scale);
                let (cx, cy) = (twice(2 * x + 1), twice(2 * (y / rows * rows) + rows));
                let (w, h) = (2 * u32::from(map.cell.w), 2 * u32::from(map.cell.h));
                assert!(
                    (u32::from(col) * w..(u32::from(col) + 1) * w).contains(&cx)
                        && (u32::from(row) * h..(u32::from(row) + 1) * h).contains(&cy),
                    "{map:?}: ({x}, {y}) in cell ({col}, {row})"
                );
                if map == CellMap::HALF_BLOCK {
                    assert_eq!((col, row), (x, y / rows), "the classic's own cell");
                }
            }
        }
        assert_eq!(
            CellMap::HALF_BLOCK.area(3, 4),
            crate::layout::Bounds {
                x: 3,
                y: 8,
                width: 1,
                height: 2
            }
        );
    }

    #[test]
    fn a_wide_cluster_takes_two_cells_the_second_empty() {
        let mut grid = CellGrid::new(5, 1);
        assert_eq!(grid.put((0, 0), "a\u{65e5}b", Some(INK), false), 4);
        assert_eq!(symbols(&grid, 0), ["a", "\u{65e5}", "", "b", " "]);
        assert_eq!(grid.get(2, 0).and_then(|c| c.fg), Some(INK));
    }

    #[test]
    fn a_cluster_crossing_the_right_edge_is_dropped() {
        let mut grid = CellGrid::new(3, 1);
        assert_eq!(grid.put((1, 0), "a\u{65e5}", None, false), 2);
        assert_eq!(symbols(&grid, 0), [" ", "a", " "]);
    }

    #[test]
    fn writes_off_the_grid_land_nowhere() {
        let mut grid = CellGrid::new(2, 1);
        grid.put((0, 3), "ab", None, false);
        assert_eq!(grid, CellGrid::new(2, 1));
        assert!(grid.get(2, 0).is_none() && grid.get(0, 1).is_none());
    }
}
