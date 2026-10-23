//! Pathfinding façade — `Router` trait + `AStarRouter` impl.
//!
//! `AStarRouter` runs A* on a coarsened cell grid whose primitives
//! (`cell_walkable`/`snap`/`CoarseGrid`/`CELL_SIZE`) are the SHARED
//! `layout::coarse` ones `layout::reach` also rides, so router reachability
//! can't drift from `ReachSet`. Routes are memoized per (from, to) and
//! auto-invalidated when the overlay signature changes, so per-frame agent
//! movement still routes around live agents.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, VecDeque};

use pixtuoid_core::grid::Grid;
use pixtuoid_core::walkable::{OccupancyOverlay, WalkableMask};

use crate::layout::{
    Bounds, COARSE_CELL_SIZE, CoarseGrid, Point, cell_anchor, cell_center, cell_walkable, snap,
    snap_where,
};
use crate::physics::{OCTILE_DIAGONAL_COST, OCTILE_STRAIGHT_COST, octile_cost};

/// Cell size in pixels — the coarse routing-grid edge, the SHARED
/// `layout::coarse` one, so router coarsening can't drift from reachability
/// coarsening.
pub(crate) const CELL_SIZE: u16 = COARSE_CELL_SIZE;

/// Abstract pathfinder — routes from `from` to `to` over the supplied mask +
/// overlay, returning a polyline (first = `from`, last = `to`, intermediate =
/// corners).
pub trait Router: std::fmt::Debug {
    /// Compute or look up the route.
    fn route(
        &mut self,
        mask: &WalkableMask,
        overlay: &OccupancyOverlay,
        from: Point,
        to: Point,
    ) -> Vec<Point>;

    /// Optional: bias the cost function toward a preferred zone (e.g. the office
    /// corridor) so paths hug the hallway instead of cutting across the cubicle
    /// floor. Default impl is a no-op.
    fn set_preferred_zone(&mut self, zone: Option<Bounds>) {
        let _ = zone;
    }
}

/// Path-cache entry cap, sized above the recurring (from, to) pairs of a fully
/// loaded floor. The key space is unbounded in steady state — aimless wander
/// mints a fresh destination every cycle and snap-back/exit legs route from live
/// interpolated origins — so an always-on office would accumulate keys forever.
/// Overflowing clears the whole map, which is safe: cornered in-flight legs are
/// frozen on `WalkState.walk_path` and never re-consult the router, and every
/// other evicted route just re-routes under the CURRENT overlay.
const PATH_CACHE_CAP: usize = 512;

/// A* router with an internal path cache.
#[derive(Debug, Default, Clone)]
pub struct AStarRouter {
    paths: HashMap<(Point, Point), Vec<Point>>,
    last_overlay_sig: u64,
    /// Cells inside this zone get a cost discount during A*. Changing it drops
    /// the cached paths — a different zone means a different optimal route.
    preferred_zone: Option<Bounds>,
}

impl AStarRouter {
    /// Construct an empty router — no cached paths, no preferred zone.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Number of cached `(from, to)` routes currently held.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the path cache holds no routes.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

impl Router for AStarRouter {
    fn route(
        &mut self,
        mask: &WalkableMask,
        overlay: &OccupancyOverlay,
        from: Point,
        to: Point,
    ) -> Vec<Point> {
        let overlay_sig = overlay.signature();
        // Invalidate only the entries that actually conflict with the new
        // overlay — paths in unaffected corridors stay cached.
        if overlay_sig != self.last_overlay_sig {
            self.paths.retain(|_, path| path_clear_under(path, overlay));
            self.last_overlay_sig = overlay_sig;
        }
        if let Some(p) = self.paths.get(&(from, to)) {
            return p.clone();
        }
        // Cache ONLY real routes. `path_clear_under` validates cached entries
        // against the OVERLAY only, never the static mask, so a straight
        // [from, to] fallback minted while a transient blocker severed the grid
        // would survive every retain() and serve a walk-through-walls line for
        // that key forever. Left uncached it re-routes next call.
        match find_path(mask, overlay, self.preferred_zone, from, to) {
            Some(path) => {
                self.paths.insert((from, to), path.clone());
                if self.paths.len() > PATH_CACHE_CAP {
                    self.paths.clear();
                }
                path
            }
            None => vec![from, to],
        }
    }

    fn set_preferred_zone(&mut self, zone: Option<Bounds>) {
        if self.preferred_zone != zone {
            self.paths.clear();
            self.preferred_zone = zone;
        }
    }
}

/// Is `path` still walkable under the current `overlay`? Samples each segment at
/// a small stride, so it tolerates a tiny overshoot (a 1-px clip into an obstacle
/// won't invalidate, but a real intersection at any corner will).
fn path_clear_under(path: &[Point], overlay: &OccupancyOverlay) -> bool {
    if overlay.is_empty() {
        return true;
    }
    for w in path.windows(2) {
        let (a, b) = (w[0], w[1]);
        let dx = i32::from(b.x) - i32::from(a.x);
        let dy = i32::from(b.y) - i32::from(a.y);
        let steps = dx.abs().max(dy.abs()).max(1) / 4;
        let n = steps.max(2);
        for i in 0..=n {
            let x = (i32::from(a.x) + dx * i / n) as u16;
            let y = (i32::from(a.y) + dy * i / n) as u16;
            if overlay.blocks(x, y) {
                return false;
            }
        }
    }
    true
}

#[derive(Eq, PartialEq)]
struct Node {
    f: u32,
    g: u32,
    cell: (u16, u16),
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        other.f.cmp(&self.f).then(other.g.cmp(&self.g))
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Ignores the preferred-zone discount, so a zone-biased route is not guaranteed shortest.
fn heuristic(a: (u16, u16), b: (u16, u16)) -> u32 {
    let dx = (i32::from(a.0) - i32::from(b.0)).unsigned_abs();
    let dy = (i32::from(a.1) - i32::from(b.1)).unsigned_abs();
    octile_cost(dx, dy)
}

/// Is the center of cell `(cx, cy)` inside `zone`?
fn cell_in_zone(zone: Option<Bounds>, cx: u16, cy: u16) -> bool {
    let Some(z) = zone else {
        return false;
    };
    let cp = cell_center(cx, cy);
    cp.x >= z.x && cp.x < z.x + z.width && cp.y >= z.y && cp.y < z.y + z.height
}

fn cell_of(p: Point) -> (u16, u16) {
    (p.x / CELL_SIZE, p.y / CELL_SIZE)
}

/// Coarse-grid dimensions, or `None` when either axis is 0 — a degenerate grid
/// the A* loop can't index.
fn grid_dims(mask: &WalkableMask) -> Option<(u16, u16)> {
    let cell_w = mask.width() / CELL_SIZE;
    let cell_h = mask.height() / CELL_SIZE;
    if cell_w == 0 || cell_h == 0 {
        return None;
    }
    Some((cell_w, cell_h))
}

/// Max rings the A\* start/goal snap probes for a walkable coarse cell.
const MAX_SNAP_RADIUS: u16 = 12;

/// Run A* on the layout's walkability mask + per-frame occupancy. Cells whose
/// center falls inside `preferred` get a step-cost discount, so paths hug that
/// zone even when an off-zone diagonal cut would be slightly shorter.
pub(crate) fn find_path(
    mask: &WalkableMask,
    overlay: &OccupancyOverlay,
    preferred: Option<Bounds>,
    from: Point,
    to: Point,
) -> Option<Vec<Point>> {
    let Some((cell_w, cell_h)) = grid_dims(mask) else {
        return Some(vec![from, to]);
    };

    // A step inside the preferred zone is discounted — a bias, not a hard constraint.
    const PREFERRED_ZONE_COST_NUM: u32 = 7;
    const PREFERRED_ZONE_COST_DEN: u32 = 10;

    let start = snap(
        mask,
        overlay,
        cell_of(from),
        cell_w,
        cell_h,
        MAX_SNAP_RADIUS,
    )?;
    let goal = snap(mask, overlay, cell_of(to), cell_w, cell_h, MAX_SNAP_RADIUS)?;

    if start == goal {
        return Some(reconstruct(
            mask,
            &Grid::filled(0, 0, None),
            start,
            from,
            to,
        ));
    }

    let mut coarse = CoarseGrid::new(mask, overlay);
    let mut open: BinaryHeap<Node> = BinaryHeap::new();
    // indexed by cell: a cell's lookups run on every expansion
    let mut came_from: Grid<Option<(u16, u16)>> = Grid::filled(cell_w, cell_h, None);
    let mut g_score: Grid<u32> = Grid::filled(cell_w, cell_h, u32::MAX);
    g_score.set(start.0, start.1, 0);
    open.push(Node {
        f: heuristic(start, goal),
        g: 0,
        cell: start,
    });

    while let Some(current) = open.pop() {
        if current.cell == goal {
            return Some(reconstruct(mask, &came_from, goal, from, to));
        }
        if current.g > g_score.get_or(current.cell.0, current.cell.1, u32::MAX) {
            continue;
        }
        for ((nx, ny), diagonal) in coarse.neighbors(current.cell) {
            let base_step = if diagonal {
                OCTILE_DIAGONAL_COST
            } else {
                OCTILE_STRAIGHT_COST
            };
            let step = if cell_in_zone(preferred, nx, ny) {
                base_step * PREFERRED_ZONE_COST_NUM / PREFERRED_ZONE_COST_DEN
            } else {
                base_step
            };
            let tentative = current.g + step;
            if tentative < g_score.get_or(nx, ny, u32::MAX) {
                came_from.set(nx, ny, Some(current.cell));
                g_score.set(nx, ny, tentative);
                open.push(Node {
                    f: tentative + heuristic((nx, ny), goal),
                    g: tentative,
                    cell: (nx, ny),
                });
            }
        }
    }
    None
}

/// Is the coarse routing cell containing `p` walkable (the SAME predicate A*
/// expands on)? This is the granularity the router actually guarantees: a
/// position can fail a per-pixel `is_walkable` — it's in the obstacle PAD band,
/// or a transient diagonal corner-graze — yet still be in a walkable routing
/// cell, exactly like every agent sprite.
pub fn point_in_walkable_cell(mask: &WalkableMask, p: Point) -> bool {
    let Some((cell_w, cell_h)) = grid_dims(mask) else {
        return false;
    };
    let (cx, cy) = cell_of(p);
    cx < cell_w && cy < cell_h && cell_walkable(mask, &OccupancyOverlay::new(), cx, cy)
}

/// Snap a pixel-space `Point` into the nearest walkable coarse CELL on
/// the STATIC mask. `None` when the grid is degenerate or no walkable cell exists
/// within `MAX_SNAP_RADIUS`. Distinct from `find_path`'s internal snapping, whose
/// `reconstruct` overwrites the polyline endpoints with the RAW `from`/`to` — a
/// caller that needs a guaranteed-walkable endpoint must re-anchor with this.
pub(crate) fn snap_point_to_walkable(mask: &WalkableMask, p: Point) -> Option<Point> {
    snap_point_where(mask, p, |_| true)
}

/// [`snap_point_to_walkable`] to the nearest walkable cell whose point `fits`.
pub(crate) fn snap_point_where(
    mask: &WalkableMask,
    p: Point,
    mut fits: impl FnMut(Point) -> bool,
) -> Option<Point> {
    let (cell_w, cell_h) = grid_dims(mask)?;
    let empty = OccupancyOverlay::new();
    let (cx, cy) = snap_where(cell_of(p), cell_w, cell_h, MAX_SNAP_RADIUS, |(cx, cy)| {
        cell_walkable(mask, &empty, cx, cy) && fits(cell_anchor(mask, cx, cy))
    })?;
    Some(cell_anchor(mask, cx, cy))
}

fn reconstruct(
    mask: &WalkableMask,
    came_from: &Grid<Option<(u16, u16)>>,
    end: (u16, u16),
    from: Point,
    to: Point,
) -> Vec<Point> {
    let mut cells = vec![end];
    let mut cur = end;
    while let Some(prev) = came_from.get_or(cur.0, cur.1, None) {
        cells.push(prev);
        cur = prev;
    }
    cells.reverse();
    let anchors: Vec<Point> = cells
        .iter()
        .map(|&(cx, cy)| cell_anchor(mask, cx, cy))
        .collect();
    // A raw endpoint stands in for its own cell's anchor only where the segment it
    // shortens stays on open floor: its cell can be up to half wall, and the
    // straight segment past it then crosses the wall.
    let mut pts = vec![from];
    match anchors[..] {
        [only] => {
            if !segment_clear(mask, from, to) {
                pts.push(only);
            }
        }
        [first, ref inner @ .., last] => {
            if !segment_clear(mask, from, inner.first().copied().unwrap_or(last)) {
                pts.push(first);
            }
            pts.extend_from_slice(inner);
            if pts.last().is_some_and(|&p| !segment_clear(mask, p, to)) {
                pts.push(last);
            }
        }
        [] => {}
    }
    pts.push(to);
    let mut turned = Vec::with_capacity(pts.len() * 2);
    for segment in pts.windows(2) {
        turned.push(segment[0]);
        turned.extend(segment_corners(mask, segment[0], segment[1]));
    }
    turned.push(to);
    simplify_polyline(mask, turned)
}

/// The corners a segment from `a` to `b` turns on to stay on open floor: none
/// when the straight segment does, an [`elbow`] when an L does, else a
/// [`pixel_detour`]. A segment neither clears stays straight.
fn segment_corners(mask: &WalkableMask, a: Point, b: Point) -> Vec<Point> {
    if segment_clear(mask, a, b) {
        return Vec::new();
    }
    elbow(mask, a, b)
        .map(|c| vec![c])
        .or_else(|| pixel_detour(mask, a, b))
        .unwrap_or_default()
}

/// How far past the box a segment spans [`pixel_detour`] searches: the gap
/// joining two half-open routing cells can lie beside either.
const DETOUR_MARGIN: u16 = CELL_SIZE;

/// The corners of a shortest walk from `a` to `b` over open pixels, stepping
/// orthogonally inside the box the two span grown by [`DETOUR_MARGIN`], pulled
/// straight wherever a segment stays on open floor; `None` when the box holds no
/// such walk. Two routing cells are walkable at half open, so the gap joining
/// them can be a pixel wide, where neither the straight segment nor an L fits.
fn pixel_detour(mask: &WalkableMask, a: Point, b: Point) -> Option<Vec<Point>> {
    let lo = |p: u16, q: u16| p.min(q).saturating_sub(DETOUR_MARGIN);
    let hi = |p: u16, q: u16, end: u16| {
        Some(
            p.max(q)
                .saturating_add(DETOUR_MARGIN)
                .min(end.checked_sub(1)?),
        )
    };
    let (x0, y0) = (lo(a.x, b.x), lo(a.y, b.y));
    let (x1, y1) = (hi(a.x, b.x, mask.width())?, hi(a.y, b.y, mask.height())?);
    let inside = |p: Point| (x0..=x1).contains(&p.x) && (y0..=y1).contains(&p.y);
    if !inside(a) || !inside(b) {
        return None;
    }
    let w = usize::from(x1 - x0) + 1;
    let at = |p: Point| usize::from(p.y - y0) * w + usize::from(p.x - x0);
    let mut came_from: Vec<Option<Point>> = vec![None; w * (usize::from(y1 - y0) + 1)];
    came_from[at(a)] = Some(a);
    let mut queue = VecDeque::from([a]);
    while let Some(p) = queue.pop_front() {
        if p == b {
            break;
        }
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let (Some(x), Some(y)) = (p.x.checked_add_signed(dx), p.y.checked_add_signed(dy))
            else {
                continue;
            };
            let q = Point { x, y };
            if inside(q) && came_from[at(q)].is_none() && (q == b || mask.is_walkable(x, y)) {
                came_from[at(q)] = Some(p);
                queue.push_back(q);
            }
        }
    }
    let mut walk = vec![b];
    while let Some(&p) = walk.last().filter(|&&p| p != a) {
        walk.push(came_from[at(p)]?);
    }
    walk.reverse();
    let mut corners = Vec::new();
    let mut i = 0;
    while i + 1 < walk.len() {
        i = (i + 1..walk.len())
            .rev()
            .find(|&j| segment_clear(mask, walk[i], walk[j]))
            .unwrap_or(i + 1);
        corners.extend(walk.get(i).filter(|_| i + 1 < walk.len()));
    }
    Some(corners)
}

/// The corner an axis-aligned L from `a` to `b` turns on, when the
/// straight segment crosses blocked floor and one of the two L-shaped ones doesn't.
/// Two adjacent routing cells are walkable at half open, so the straight segment
/// between their anchors can clip the corner of a wall standing in either.
fn elbow(mask: &WalkableMask, a: Point, b: Point) -> Option<Point> {
    if segment_clear(mask, a, b) {
        return None;
    }
    [Point { x: b.x, y: a.y }, Point { x: a.x, y: b.y }]
        .into_iter()
        .find(|&c| {
            mask.is_walkable(c.x, c.y) && segment_clear(mask, a, c) && segment_clear(mask, c, b)
        })
}

/// Does every pixel a walker passes strictly between `a` and `b` stand on the
/// static `mask`? The ends are exempt: a raw endpoint may sit in a routing pad.
fn segment_clear(mask: &WalkableMask, a: Point, b: Point) -> bool {
    crate::physics::segment_pixels(a, b)
        .filter(|&p| p != a && p != b)
        .all(|p| mask.is_walkable(p.x, p.y))
}

/// Drop each corner collinear with its neighbours, unless the merged segment would
/// cross blocked floor its two halves step around: a walk truncates toward its
/// start, so one long segment lands on different pixels than its halves.
fn simplify_polyline(mask: &WalkableMask, pts: Vec<Point>) -> Vec<Point> {
    if pts.len() < 3 {
        return pts;
    }
    let mut out: Vec<Point> = Vec::with_capacity(pts.len());
    out.push(pts[0]);
    for i in 1..pts.len() - 1 {
        let prev = out[out.len() - 1];
        let here = pts[i];
        let next = pts[i + 1];
        let dx_in = i32::from(here.x) - i32::from(prev.x);
        let dy_in = i32::from(here.y) - i32::from(prev.y);
        let dx_out = i32::from(next.x) - i32::from(here.x);
        let dy_out = i32::from(next.y) - i32::from(here.y);
        let collinear = dx_in * dy_out == dy_in * dx_out;
        let crossed_clean = segment_clear(mask, prev, here) && segment_clear(mask, here, next);
        if !collinear || (crossed_clean && !segment_clear(mask, prev, next)) {
            out.push(here);
        }
    }
    out.push(pts[pts.len() - 1]);
    out
}

#[cfg(test)]
mod tests;
