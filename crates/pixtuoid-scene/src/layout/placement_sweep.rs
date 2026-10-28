//! The generative placement-invariant sweep over a sizes × seeds grid.
//!
//! The routability guards live HERE, on the same [`SWEEP_SIZES`] axis as the
//! placement ones, so the two can't disagree — they did, and the narrow band
//! was swept for placement but never for routability.

use super::decor::DESK_GROUND_H;
use super::mask::pantry_ground_rect;
use super::placement::rects_overlap;
use super::*;

/// The sweep's size axis: the forced-single-pod widths (floored by
/// `MIN_LAYOUT_W`), the SHORT band the derived height floor opened (46-58
/// units = 24-30 rows, which the old hand-written floor refused sight-unseen), the
/// decor-vs-wall and Y-overflow corners, the golden and live wasm heroes' units,
/// and a spread wide enough that the appliance kinds appear.
const SWEEP_SIZES: &[(u16, u16)] = &[
    (super::compute::MIN_LAYOUT_W, super::compute::MIN_LAYOUT_H),
    (super::compute::MIN_LAYOUT_W, 60),
    (super::compute::MIN_LAYOUT_W + 2, 100),
    (super::compute::MIN_LAYOUT_W, 120),
    (64, 48),
    (80, 46),
    (96, 52),
    (120, 46),
    (160, 50),
    (200, 46),
    (128, 56),
    (super::compute::MIN_LAYOUT_W + 1, 120),
    (super::compute::MIN_LAYOUT_W + 3, 70),
    (super::compute::MIN_LAYOUT_W + 4, 160),
    (59, 148),
    (64, 130),
    (96, 60),
    (96, 70),
    (96, 100),
    (96, 115),
    // The only size here with a partial BOTTOM row and a partial COLUMN beside it — a
    // `compute_pod_desks` branch no other size reaches.
    (117, 146),
    (120, 96),
    (128, 80),
    (128, 100),
    (150, 68),
    (160, 120),
    (192, 158),
    (200, 116),
    (231, 130),
    (240, 160),
    (320, 180),
];

/// Seeds swept per size. 0..12 reaches every `FloorVariant` through its
/// hash, pinned by `the_sweep_reaches_every_floor_variant`.
pub(super) const SWEEP_SEEDS: std::ops::Range<u64> = 0..12;

/// Run `f` over `SWEEP_SIZES` × `seeds` at production fill (`max_desks: None`,
/// the densest desk grid — the strictest placement case). A `None` layout is
/// asserted to be a legitimate refusal, never silently skipped.
fn sweep_over(
    seeds: impl Iterator<Item = u64> + Clone,
    mut f: impl FnMut(u16, u16, u64, &SceneLayout),
) {
    for &(w, h) in SWEEP_SIZES {
        for seed in seeds.clone() {
            match SceneLayout::compute_with_seed(w, h, None, seed) {
                Some(l) => {
                    // The dual of the None arm below: that one prices a refusal, this
                    // one prices an acceptance.
                    assert!(
                        !l.home_desks.is_empty(),
                        "{w}x{h} seed {seed}: a layout above the advertised minimum with \
                         no desk to seat anyone"
                    );
                    f(w, h, seed, &l);
                }
                // Every entry is meant to sit above both floors, so a refusal means a
                // floor rose past a hand-pinned size and this axis lost it SILENTLY.
                None => panic!(
                    "{w}x{h} seed {seed}: a SWEEP_SIZES entry fell under the floor \
                     ({}x{}) — the sweep lost it",
                    super::compute::MIN_LAYOUT_W,
                    super::compute::MIN_LAYOUT_H
                ),
            }
        }
    }
}

fn sweep(f: impl FnMut(u16, u16, u64, &SceneLayout)) {
    sweep_over(SWEEP_SEEDS, f);
}

/// The PRODUCTION axis: `SWEEP_SIZES` × `floor::floor_seed(0..MAX_FLOORS)`,
/// the seeds a running app actually lays out. Disjoint from [`SWEEP_SEEDS`] but
/// for seed 0, so a guard swept on `SWEEP_SEEDS` alone never visits a floor a
/// user sees.
fn sweep_production_floors(f: impl FnMut(u16, u16, u64, &SceneLayout)) {
    sweep_over(
        (0..crate::floor::MAX_FLOORS).map(crate::floor::floor_seed),
        f,
    );
}

/// Which Bounds a piece's rect must stay inside — one honest container per
/// piece, NOT one rule for all.
#[derive(Clone, Copy, Debug)]
enum Container {
    Band,
    Aisle,
    MeetingRoom(usize),
    Pantry,
    /// The carpet apron at the wall base — the straddling wall decor's ground
    /// strip.
    WallApron,
    WallBand,
}

struct Piece {
    label: String,
    /// `None` = the piece stamps no obstacle of its own (wall-hung decor).
    ground: Option<(Point, Size)>,
    visual: (Point, Size),
    /// For `Pivot::Center` pieces: the unclamped center + visual size, to catch
    /// a west/north spill that `anchored_top_left`'s `saturating_sub` silently
    /// clamps to 0 (a centered piece "fits" iff `pos >= visual/2` per axis).
    center_fit: Option<(Point, Size)>,
    container: Container,
    /// Also require the VISUAL box inside the container: the pod-decor sites
    /// SKIP a slot whose whole sprite wouldn't fit the band, and that skip is
    /// part of the contract, not just the ground.
    visual_in_container: bool,
    /// Pieces sharing a group id are ONE physical cluster, exempt from the
    /// pairwise-overlap invariant WITHIN the group. The harness's one exemption
    /// with no compile tooth: a NEW id needs a WHY at the declaration naming the
    /// authored composition AND a golden pinning the cluster's internal
    /// geometry — never a way to silence a real overlap finding.
    overlap_group: Option<u8>,
}

impl Piece {
    fn table(
        label: String,
        pivot: Pivot,
        pos: Point,
        kind: Furniture,
        container: Container,
        overlap_group: Option<u8>,
    ) -> Piece {
        let def = furniture_def(kind);
        let vis_tl = anchored_top_left(pivot, pos, def.visual.w, def.visual.h);
        Piece {
            label,
            ground: def.ground_rect(pivot, pos),
            visual: (vis_tl, def.visual),
            center_fit: matches!(pivot, Pivot::Center).then_some((pos, def.visual)),
            container,
            visual_in_container: false,
            overlap_group,
        }
    }
}

/// Enumerate EVERY placed piece of a layout, with rects from the SAME
/// `mask::ground_rect` / `pantry_ground_rect` the walkable mask stamps — the
/// sweep can never drift from the collision truth. It maps the roster
/// exhaustively, so a new fixture kind fails compilation here until it is
/// swept, or skipped with the WHY beside it.
fn pieces(l: &SceneLayout) -> Vec<Piece> {
    let mut out = Vec::new();
    for f in l.fixtures() {
        match f.kind {
            FixtureKind::Desk(i) => out.push(Piece::table(
                format!("desk[{}]", i.0),
                Pivot::TopLeft,
                l.home_desks[i.0].at,
                Furniture::Desk,
                Container::Band,
                None,
            )),
            FixtureKind::Pod { item, kind } => {
                let mut piece = Piece::table(
                    format!("pod_decor[{item}] {kind:?}"),
                    Pivot::Center,
                    l.pod_decor[item].pos,
                    kind.furniture(),
                    Container::Band,
                    None,
                );
                piece.visual_in_container = true;
                out.push(piece);
            }
            // Per-ITEM container, picked by POSITION: a plant that `settle_plant`
            // moved beside a corner appliance adopts the blocker's AISLE row.
            FixtureKind::Plant { item, kind } => {
                let pos = l.plants[item].pos;
                let in_meeting = l
                    .meeting_room_bounds(0)
                    .is_some_and(|mr| contains_point(mr, pos));
                let container = if in_meeting {
                    Container::MeetingRoom(0)
                } else if contains_point(l.cubicle_aisle, pos) {
                    Container::Aisle
                } else {
                    Container::Band
                };
                out.push(Piece::table(
                    format!("plant[{item}] {kind:?}"),
                    Pivot::Center,
                    pos,
                    kind.furniture(),
                    container,
                    None,
                ));
            }
            FixtureKind::Wall { item, kind } => {
                let container = match kind {
                    // Free-standing floor furniture despite living in the
                    // wall_decor vec: the container is keyed on the KIND.
                    WallDecor::Whiteboard => Container::Band,
                    // Straddlers: tall sprite on the wall, shallow ground strip
                    // on the carpet apron at the wall base.
                    WallDecor::Bookshelf | WallDecor::MeetingScreen => Container::WallApron,
                    WallDecor::ExitSign | WallDecor::BulletinBoard => Container::WallBand,
                };
                out.push(Piece::table(
                    format!("wall_decor[{item}] {kind:?}"),
                    Pivot::TopLeft,
                    l.wall_decor[item].pos,
                    kind.furniture(),
                    container,
                    None,
                ));
            }
            FixtureKind::Station { waypoint, station } => {
                let wp = l.waypoints[waypoint];
                let label = format!("waypoint[{waypoint}] {station:?}");
                out.push(match station {
                    // Runtime-sized via `pantry_ground_rect` — the table row is
                    // empty ON PURPOSE.
                    Station::PantryCounter => {
                        let counter = l.pantry_counter_size();
                        Piece {
                            label,
                            ground: Some(pantry_ground_rect(wp.pos, counter)),
                            visual: (
                                anchored_top_left(Pivot::Center, wp.pos, counter.w, counter.h),
                                counter,
                            ),
                            center_fit: Some((wp.pos, counter)),
                            container: Container::Pantry,
                            visual_in_container: false,
                            overlap_group: None,
                        }
                    }
                    Station::VendingMachine | Station::Printer => Piece::table(
                        label,
                        Pivot::Center,
                        wp.pos,
                        wp.kind.furniture(),
                        Container::Aisle,
                        None,
                    ),
                    Station::SnackShelf => Piece::table(
                        label,
                        Pivot::Center,
                        wp.pos,
                        wp.kind.furniture(),
                        Container::Pantry,
                        None,
                    ),
                });
            }
            // Each seat stamps its own body and their union IS the couch's
            // blocked ground, so model the seats — the one sprite under-models it.
            FixtureKind::LoungeCouch => {
                for (i, wp) in l.waypoints.iter().enumerate() {
                    if wp.kind == WaypointKind::Couch {
                        out.push(Piece::table(
                            format!("waypoint[{i}] Couch seat"),
                            Pivot::Center,
                            wp.pos,
                            Furniture::Couch,
                            Container::Band,
                            Some(LOUNGE_GROUP),
                        ));
                    }
                }
            }
            FixtureKind::MeetingSofa { room, seat, .. } => {
                if let Some(trio) = l.meeting_rooms[room].trio {
                    out.push(Piece::table(
                        format!("meeting[{room}].sofa[{seat}]"),
                        Pivot::Center,
                        trio.sofas[seat],
                        Furniture::MeetingSofaBody,
                        Container::MeetingRoom(room),
                        None,
                    ));
                }
            }
            FixtureKind::MeetingTable { room } => {
                if let Some(trio) = l.meeting_rooms[room].trio {
                    out.push(Piece::table(
                        format!("meeting[{room}].table"),
                        Pivot::Center,
                        trio.table,
                        Furniture::MeetingTable,
                        Container::MeetingRoom(room),
                        None,
                    ));
                }
            }
            FixtureKind::FloorLamp => {
                out.extend(
                    l.floor_lamp()
                        .map(|p| lounge_piece("floor_lamp", p, Furniture::FloorLamp)),
                );
            }
            FixtureKind::SideTable => out.extend(
                l.lounge_side_table()
                    .map(|p| lounge_piece("lounge_side_table", p, Furniture::LoungeSideTable)),
            ),
            FixtureKind::FishTank => {
                out.extend(
                    l.fish_tank()
                        .map(|p| lounge_piece("fish_tank", p, Furniture::FishTank)),
                );
            }
            FixtureKind::KitchenIsland => {
                if let Some(island) = l.pantry.and_then(|p| p.kitchen_island) {
                    out.push(Piece::table(
                        "kitchen_island".into(),
                        Pivot::Center,
                        island,
                        Furniture::KitchenIsland,
                        Container::Pantry,
                        None,
                    ));
                }
            }
            // No obstacle of its own; containment is the pos-in-room check in
            // `every_meeting_slot_sits_in_its_room`.
            FixtureKind::MeetingChair { .. } => {}
            // Stamp no ground: they ride their desk, placed off it by a fixed
            // offset.
            FixtureKind::FilingCabinet(_) | FixtureKind::DeskChair(_) => {}
            // Flat on the floor: nothing to overlap.
            FixtureKind::MeetingRug { .. }
            | FixtureKind::LoungeRug
            | FixtureKind::Doormat { .. }
            | FixtureKind::PantryMat
            | FixtureKind::IslandMat
            | FixtureKind::Runner => {}
            // Placed by their room's own rect rules, stamping no ground.
            FixtureKind::CoatRack { .. }
            | FixtureKind::NoticeBoard { .. }
            | FixtureKind::WaterCooler
            | FixtureKind::TrashBin => {}
            // Architecture, not furniture: the door PUNCHES walkability through
            // the band, and its threshold is a walkable POINT the connectivity
            // guards assert.
            FixtureKind::Door => {}
            // On the wall band, above every floor.
            FixtureKind::NeonSign | FixtureKind::Clock => {}
        }
    }
    out
}

/// ONE authored cluster: the table tucks against the couch's west armrest and
/// the lamp hugs its east side BY DESIGN, so they share an overlap group and
/// the goldens — not the overlap invariant — pin their internal geometry.
const LOUNGE_GROUP: u8 = 2;

fn lounge_piece(label: &str, pos: Point, row: Furniture) -> Piece {
    Piece::table(
        label.into(),
        Pivot::Center,
        pos,
        row,
        Container::Band,
        Some(LOUNGE_GROUP),
    )
}

fn contains_point(b: Bounds, p: Point) -> bool {
    p.x >= b.x && p.x < b.x + b.width && p.y >= b.y && p.y < b.y + b.height
}

fn rect_in_bounds(tl: Point, sz: Size, b: Bounds) -> bool {
    tl.x >= b.x && tl.y >= b.y && tl.x + sz.w <= b.x + b.width && tl.y + sz.h <= b.y + b.height
}

/// Resolve a piece's container to concrete Bounds. `None` = the container
/// doesn't exist for this layout, itself a failure: a piece can't be placed in
/// a room the floor doesn't have.
fn container_bounds(l: &SceneLayout, c: Container) -> Option<Bounds> {
    match c {
        Container::Band => Some(l.cubicle_band),
        Container::Aisle => Some(l.cubicle_aisle),
        Container::MeetingRoom(i) => l.meeting_room_bounds(i),
        Container::Pantry => l.pantry.map(|p| p.bounds),
        Container::WallApron => Some(Bounds {
            x: 0,
            y: l.wall_band_h(),
            width: l.buf_w,
            height: l.top_margin - l.wall_band_h(),
        }),
        Container::WallBand => Some(Bounds {
            x: 0,
            y: 0,
            width: l.buf_w,
            height: l.top_margin,
        }),
    }
}

/// Cap on the reported violations: the invariants collect across the WHOLE
/// sweep and fail once, because a fail-fast assert reports only the first cell
/// and hides the pattern (one bug and a systemic clamp miss look identical).
const MAX_REPORTED: usize = 25;

fn assert_no_violations(what: &str, violations: Vec<String>) {
    assert!(
        violations.is_empty(),
        "{} {what} violations across the sweep (first {}):\n{}",
        violations.len(),
        violations.len().min(MAX_REPORTED),
        violations
            .iter()
            .take(MAX_REPORTED)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn every_piece_stays_inside_the_buffer() {
    let mut v = Vec::new();
    sweep(|w, h, seed, l| {
        let buffer = Bounds {
            x: 0,
            y: 0,
            width: l.buf_w,
            height: l.buf_h,
        };
        for p in pieces(l) {
            for (what, rect) in [("ground", p.ground), ("visual", Some(p.visual))] {
                if let Some((tl, sz)) = rect
                    && !rect_in_bounds(tl, sz, buffer)
                {
                    v.push(format!(
                        "{w}x{h} seed {seed}: {} {what} {tl:?}+{sz:?} leaves the buffer",
                        p.label
                    ));
                }
            }
            if let Some((pos, vis)) = p.center_fit
                && (pos.x < vis.w / 2 || pos.y < vis.h / 2)
            {
                v.push(format!(
                    "{w}x{h} seed {seed}: {} centered at {pos:?} spills its {vis:?} \
                         visual west/north (silently clamped by saturating_sub)",
                    p.label
                ));
            }
        }
    });
    assert_no_violations("buffer-containment", v);
}

#[test]
fn every_piece_ground_stays_in_its_container() {
    let mut v = Vec::new();
    sweep(|w, h, seed, l| {
        for p in pieces(l) {
            let Some(b) = container_bounds(l, p.container) else {
                if p.ground.is_some() {
                    v.push(format!(
                        "{w}x{h} seed {seed}: {} placed but its container {:?} doesn't exist",
                        p.label, p.container
                    ));
                }
                continue;
            };
            if let Some((tl, sz)) = p.ground
                && !rect_in_bounds(tl, sz, b)
            {
                v.push(format!(
                    "{w}x{h} seed {seed}: {} ground {tl:?}+{sz:?} leaves its {:?} {b:?}",
                    p.label, p.container
                ));
            }
            if p.visual_in_container {
                let (tl, sz) = p.visual;
                if !rect_in_bounds(tl, sz, b) {
                    v.push(format!(
                        "{w}x{h} seed {seed}: {} visual {tl:?}+{sz:?} leaves its {:?} {b:?}",
                        p.label, p.container
                    ));
                }
            }
        }
    });
    assert_no_violations("container", v);
}

#[test]
fn every_piece_ground_is_blocked_in_the_mask() {
    let mut v = Vec::new();
    sweep(|w, h, seed, l| {
        for p in pieces(l) {
            let Some((tl, sz)) = p.ground else { continue };
            let (cx, cy) = (tl.x + sz.w / 2, tl.y + sz.h / 2);
            if l.walkable.is_walkable(cx, cy) {
                v.push(format!(
                    "{w}x{h} seed {seed}: {} ground centre ({cx},{cy}) is WALKABLE —                      missing mask stamp",
                    p.label
                ));
            }
        }
    });
    assert_no_violations("mask-parity", v);
}

#[test]
fn every_rug_lies_whole_on_the_floor() {
    let mut v = Vec::new();
    sweep(|w, h, seed, l| {
        let rugs = l.fixtures().filter_map(|f| {
            matches!(
                f.kind,
                FixtureKind::MeetingRug { .. } | FixtureKind::LoungeRug
            )
            .then_some(f.visual)
        });
        for rug in rugs {
            let on_floor = rug.x > 0
                && rug.y >= l.wall_band_h()
                && rug.x + rug.width <= l.buf_w
                && rug.y + rug.height <= l.buf_h;
            if !on_floor {
                v.push(format!("{w}x{h} seed {seed}: rug {rug:?} leaves the floor"));
            }
        }
    });
    assert_no_violations("rug-on-floor", v);
}

/// Only the BLOCKED grounds: sprite overhangs may overlap freely — that is
/// occlusion, not placement.
#[test]
fn no_two_furniture_grounds_overlap() {
    let mut v = Vec::new();
    sweep(|w, h, seed, l| {
        let ps: Vec<Piece> = pieces(l)
            .into_iter()
            .filter(|p| p.ground.is_some())
            .collect();
        for i in 0..ps.len() {
            for j in i + 1..ps.len() {
                let (a, b) = (&ps[i], &ps[j]);
                if a.overlap_group.is_some() && a.overlap_group == b.overlap_group {
                    continue;
                }
                if rects_overlap(a.ground.unwrap(), b.ground.unwrap()) {
                    v.push(format!(
                        "{w}x{h} seed {seed}: {} {:?} overlaps {} {:?}",
                        a.label,
                        a.ground.unwrap(),
                        b.label,
                        b.ground.unwrap()
                    ));
                }
            }
        }
    });
    assert_no_violations("furniture-overlap", v);
}

/// UNPADDED grounds only — a padded rect legitimately touches a wall, because
/// the pad is routing slack.
#[test]
fn no_furniture_ground_overlaps_a_wall() {
    let mut v = Vec::new();
    sweep(|w, h, seed, l| {
        let walls: Vec<(Point, Size)> = l.wall_pieces.iter().map(|p| p.footprint()).collect();
        for p in pieces(l) {
            let Some(g) = p.ground else { continue };
            for &wrect in &walls {
                if rects_overlap(g, wrect) {
                    v.push(format!(
                        "{w}x{h} seed {seed}: {} ground {g:?} overlaps wall {wrect:?}",
                        p.label
                    ));
                }
            }
        }
    });
    assert_no_violations("wall-overlap", v);
}

/// Half-width, in px, of the box probed around each wall-footprint corner for
/// route endpoints: wide enough that a pair straddles the corner along both of
/// its arms, where a diagonal coarse step cuts it.
const CORNER_PROBE_RADIUS: i32 = 6;

/// Probe spacing inside that box. Not a divisor of `COARSE_CELL_SIZE`, so the
/// probes land on every offset within a routing cell.
const CORNER_PROBE_STEP: usize = 3;

/// The open pixels, on walkable routing cells, probed around each corner of
/// each wall footprint: one list per corner.
fn wall_corner_probes(l: &SceneLayout) -> Vec<Vec<Point>> {
    let mut corners = Vec::new();
    for piece in &l.wall_pieces {
        let (at, sz) = piece.footprint();
        let (x0, y0) = (i32::from(at.x) - 1, i32::from(at.y) - 1);
        let (x1, y1) = (i32::from(at.x + sz.w), i32::from(at.y + sz.h));
        for (cx, cy) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
            let span = (-CORNER_PROBE_RADIUS..=CORNER_PROBE_RADIUS).step_by(CORNER_PROBE_STEP);
            let probes = span
                .clone()
                .flat_map(|dy| span.clone().map(move |dx| (cx + dx, cy + dy)))
                .filter_map(|(x, y)| {
                    Some(Point {
                        x: u16::try_from(x).ok()?,
                        y: u16::try_from(y).ok()?,
                    })
                })
                .filter(|&p| {
                    l.walkable.is_walkable(p.x, p.y)
                        && crate::pathfind::point_in_walkable_cell(&l.walkable, p)
                        && wall_at(l, p).is_none()
                })
                .collect();
            corners.push(probes);
        }
    }
    corners
}

/// The wall piece whose footprint holds `p`, if any.
fn wall_at(l: &SceneLayout, p: Point) -> Option<WallPiece> {
    l.wall_pieces.iter().copied().find(|w| {
        let (at, sz) = w.footprint();
        (at.x..at.x + sz.w).contains(&p.x) && (at.y..at.y + sz.h).contains(&p.y)
    })
}

/// The first pixel a walker passes on `path` that stands inside a wall, with
/// that wall.
fn route_through_wall(l: &SceneLayout, path: &[Point]) -> Option<(Point, WallPiece)> {
    path.windows(2)
        .flat_map(|segment| crate::physics::segment_pixels(segment[0], segment[1]))
        .find_map(|p| wall_at(l, p).map(|w| (p, w)))
}

/// Every route between two open pixels around one wall corner that passes
/// through a wall, described.
fn corner_routes_through_walls(w: u16, h: u16, seed: u64, l: &SceneLayout) -> Vec<String> {
    use crate::pathfind::find_path;
    let overlay = pixtuoid_core::walkable::OccupancyOverlay::new();
    let mut v = Vec::new();
    for probes in wall_corner_probes(l) {
        for (i, &from) in probes.iter().enumerate() {
            for &to in &probes[i + 1..] {
                let Some(path) = find_path(&l.walkable, &overlay, None, from, to) else {
                    continue;
                };
                if let Some((p, wall)) = route_through_wall(l, &path) {
                    v.push(format!(
                        "{w}x{h} seed {seed}: {from:?}->{to:?} passes {p:?} inside {:?}",
                        wall.footprint()
                    ));
                }
            }
        }
    }
    v
}

/// No route between two open pixels around a wall's corner passes through a
/// wall. The production legs carry the same assert in
/// [`assert_home_desk_approaches_are_routable`] and
/// `every_wander_destination_is_routable_from_its_desk`.
#[test]
fn no_route_around_a_wall_corner_cuts_through_it() {
    let mut v = Vec::new();
    let mut check =
        |w, h, seed, l: &SceneLayout| v.extend(corner_routes_through_walls(w, h, seed, l));
    sweep(&mut check);
    sweep_production_floors(&mut check);
    assert_no_violations("route-through-wall", v);
}

/// The short floors, where the wall band leaves the door's stubs and posts
/// beside the routing cells the band's one pod row narrows: every height from
/// the floor to [`WALL_SCAN_TOP_H`] across the widths, on the floors a user
/// sees. The discrete [`SWEEP_SIZES`] grid reaches this band at one width.
#[test]
#[cfg_attr(
    coverage,
    ignore = "an exhaustive sweep outlasts its budget instrumented; the plain suite asserts it on every PR"
)]
fn no_route_on_a_short_floor_passes_through_a_wall() {
    let floors: Vec<(u16, u16, u64)> = (super::compute::MIN_LAYOUT_H..=WALL_SCAN_TOP_H)
        .flat_map(|h| {
            (super::compute::MIN_LAYOUT_W..=WALL_SCAN_RIGHT_W)
                .step_by(WALL_SCAN_W_STEP)
                .flat_map(move |w| {
                    (0..crate::floor::MAX_FLOORS).map(move |f| (w, h, crate::floor::floor_seed(f)))
                })
        })
        .collect();
    let check = |&(w, h, seed): &(u16, u16, u64)| {
        let Some(l) = SceneLayout::compute_with_seed(w, h, None, seed) else {
            panic!("{w}x{h} seed {seed}: refused above the floor");
        };
        let mut v = corner_routes_through_walls(w, h, seed, &l);
        v.extend(wander_legs(&l).into_iter().filter_map(|leg| {
            let (p, wall) = route_through_wall(&l, leg.path.as_ref()?)?;
            Some(format!(
                "{w}x{h} seed {seed}: {leg} passes {p:?} inside {:?}",
                wall.footprint()
            ))
        }));
        v
    };
    // the floors are independent: across the cores nextest reserves for it
    // (`threads-required` in .config/nextest.toml)
    let v: Vec<String> = crate::par::map(&floors, crate::par::cores(), check)
        .into_iter()
        .flatten()
        .collect();
    assert_no_violations("short-floor-route-through-wall", v);
}

/// The tallest floor [`no_route_on_a_short_floor_passes_through_a_wall`]
/// scans: the band where a door's post and stubs crossed routes reached 63.
const WALL_SCAN_TOP_H: u16 = 64;
/// The widest floor that scan lays out.
const WALL_SCAN_RIGHT_W: u16 = 240;
/// That scan's width step, prime to the routing cell so each width shifts the
/// walls against the grid.
const WALL_SCAN_W_STEP: usize = 23;

/// The door threshold is walkable AND every walkable pixel is reachable from it
/// (4-connected), through the PRODUCTION `unreachable_walkable_cells` so the
/// guard and its test can't drift. The threshold-walkable assert is SEPARATE and
/// FIRST: `unreachable_walkable_cells` returns empty on a BLOCKED seed, so
/// without it a sealed threshold passes vacuously.
fn assert_walkable_connected(w: u16, h: u16, seed: u64, l: &SceneLayout) {
    let start = l.door_threshold;
    assert!(
        l.walkable.is_walkable(start.x, start.y),
        "{w}x{h} seed {seed}: door threshold {start:?} is not walkable"
    );
    let pocket = super::compute::unreachable_walkable_cells(&l.walkable, start);
    assert!(
        pocket.is_empty(),
        "{w}x{h} seed {seed}: {} walkable px unreachable from the door (a sealed \
         pocket), e.g. {:?}",
        pocket.len(),
        pocket.first()
    );
}

#[test]
fn walkable_is_one_connected_region() {
    sweep(assert_walkable_connected);
}

/// Deliberately a one-sided bound on `top_margin`, not `assert_eq!` against the
/// spawn formula, which would pin nothing. Walkability is NOT the discriminator
/// and a routability sweep is blind here: `find_path` and `ReachSet::from_mask`
/// both SNAP a displaced seed back into the component, so every routing assert
/// still passes with the spawn north of the floor line — only this one fails.
fn assert_spawn_stands_on_open_floor(w: u16, h: u16, seed: u64, l: &SceneLayout) {
    let dt = l.door_threshold;
    assert!(
        dt.y >= l.top_margin,
        "{w}x{h} seed {seed}: spawn {dt:?} sits on the wall apron (rows {}..{}) \
         instead of open floor — an entering character starts inside the strip \
         the straddling wall decor stamps its ground into",
        l.wall_band_h(),
        l.top_margin
    );
}

#[test]
fn the_spawn_threshold_stands_on_the_floor_not_the_wall_apron() {
    sweep(assert_spawn_stands_on_open_floor);
    sweep_production_floors(assert_spawn_stands_on_open_floor);
}

/// Routes from `pose::desk_leg_endpoint(desk, l).0`, where the production
/// wander-out leg starts — NOT from the door. Routing from the door made a desk
/// an origin-free bystander, so one stranded on the severed side of a coarse cut
/// never appeared as a route START and the whole class passed through; an
/// unroutable destination degrades the leg to a straight line through furniture.
#[test]
fn every_wander_destination_is_routable_from_its_desk() {
    sweep(|w, h, seed, l| {
        for leg in wander_legs(l) {
            let Some(path) = &leg.path else {
                panic!("{w}x{h} seed {seed}: {leg} is unroutable");
            };
            if let Some((p, wall)) = route_through_wall(l, path) {
                panic!(
                    "{w}x{h} seed {seed}: {leg} passes {p:?} inside {:?}",
                    wall.footprint()
                );
            }
        }
    });
}

/// A production wander-out leg, named by the desk and waypoint it serves.
struct WanderLeg {
    desk: Point,
    kind: super::WaypointKind,
    origin: Point,
    approach: Point,
    /// `None` when unroutable.
    path: Option<Vec<Point>>,
}

impl std::fmt::Display for WanderLeg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            desk,
            kind,
            origin,
            approach,
            ..
        } = self;
        write!(
            f,
            "desk {desk:?}'s leg {origin:?}->{kind:?} approach {approach:?}"
        )
    }
}

/// Each production wander-out leg, from a desk's leg origin to a waypoint's
/// approach.
fn wander_legs(l: &SceneLayout) -> Vec<WanderLeg> {
    use crate::pathfind::find_path;
    let overlay = pixtuoid_core::walkable::OccupancyOverlay::new();
    // Deduped: the desk loop otherwise re-routes one `(origin, approach)` pair
    // once per desk sharing that approach cell.
    let mut seen: std::collections::HashSet<(Point, Point)> = std::collections::HashSet::new();
    let mut legs = Vec::new();
    for &desk in &l.home_desks {
        let (origin, _) = crate::pose::desk_leg_endpoint(desk, l);
        for wp in &l.waypoints {
            let a = super::approach_point(
                wp.kind.furniture(),
                wp.pos,
                wp.facing,
                l.pantry_counter_size(),
                &l.walkable,
                desk.at,
                &l.reachable,
            );
            // A `wp.pos` sentinel is the documented "no valid approach"
            // answer — `resolve_wander_target` ambles that cycle instead.
            if a == wp.pos || !seen.insert((origin, a)) {
                continue;
            }
            legs.push(WanderLeg {
                desk: desk.at,
                kind: wp.kind,
                origin,
                approach: a,
                path: find_path(&l.walkable, &overlay, None, origin, a),
            });
        }
    }
    legs
}

/// The COARSE twin of [`assert_walkable_connected`]: that one is a 4-connected
/// PIXEL flood, but the router runs on the 4×4 grid, so a ≤3px channel is
/// pixel-connected and coarse-IMPASSABLE — `desk_approach_cell` then returns its
/// no-valid-approach sentinel and every leg for the severed desks degrades to a
/// straight `door→chair` line through furniture.
fn assert_home_desk_approaches_are_routable(w: u16, h: u16, seed: u64, l: &SceneLayout) {
    // A free fn, not a closure: three sweeps share it — the placement seed axis,
    // the production floor seeds, and the step-1 `NARROW_BAND` width scan.
    use crate::pathfind::find_path;
    let overlay = pixtuoid_core::walkable::OccupancyOverlay::new();
    let door = l.door_threshold;
    for (i, &desk) in l.home_desks.iter().enumerate() {
        let approach = crate::pose::desk_approach_cell(desk, l).unwrap_or_else(|| {
            panic!(
                "{w}x{h} seed {seed}: home desk {i} at {desk:?} has NO reachable approach \
                 side — every leg to it falls back to a straight line through the desk"
            )
        });
        let path = find_path(&l.walkable, &overlay, None, door, approach).unwrap_or_else(|| {
            panic!(
                "{w}x{h} seed {seed}: home desk {i} at {desk:?} has approach {approach:?} \
                 unroutable from the door {door:?} — the coarse grid is severed"
            )
        });
        if let Some((p, wall)) = route_through_wall(l, &path) {
            panic!(
                "{w}x{h} seed {seed}: the leg {door:?}->{approach:?} passes {p:?} inside {:?}",
                wall.footprint()
            );
        }
    }
}

#[test]
fn every_home_desk_approach_is_routable_from_the_door() {
    sweep(assert_home_desk_approaches_are_routable);
    sweep_production_floors(assert_home_desk_approaches_are_routable);
}

/// A back-turned desk exists only to face a viewer-facing one across the pod's inner
/// gap. Stated as the PAIRING, not as `pod_row_facing`'s formula a test could only copy.
fn assert_back_turned_desks_face_a_partner(w: u16, h: u16, seed: u64, l: &SceneLayout) {
    let south: std::collections::HashSet<Point> = l
        .home_desks
        .iter()
        .filter(|d| d.facing == Facing::South)
        .map(|d| d.at)
        .collect();
    for &HomeDesk { at: d, facing } in &l.home_desks {
        if facing != Facing::North {
            continue;
        }
        let partner = Point {
            x: d.x,
            y: d.y.saturating_sub(DESK_H + INTRA_POD_GAP_Y),
        };
        assert!(
            south.contains(&partner),
            "{w}x{h} seed {seed}: back-turned desk {d:?} has no viewer-facing desk at \
             {partner:?} — it turns its back on empty floor instead of on a pod partner"
        );
    }
}

#[test]
fn back_turned_desks_face_a_partner_across_the_pod_gap() {
    sweep(assert_back_turned_desks_face_a_partner);
    sweep_production_floors(assert_back_turned_desks_face_a_partner);
}

/// A divider crossed by an E-W wall is TWO vertical segments meeting at the
/// cross wall; every corner row must be solid across the wall's whole width, or
/// a walker can stand IN the divider.
#[test]
fn no_walkable_hole_where_a_vertical_wall_meets_a_horizontal_one() {
    sweep(|w, h, seed, l| {
        let h_walls: Vec<_> = l
            .room_walls
            .iter()
            .filter_map(|s| match *s {
                WallSegment::Horizontal { y, x0, x1 } => Some((y, x0, x1)),
                WallSegment::Vertical { .. } => None,
            })
            .collect();
        let v_walls = l.room_walls.iter().filter_map(|s| match *s {
            WallSegment::Vertical { x, y0, .. } => Some((x, y0)),
            WallSegment::Horizontal { .. } => None,
        });
        for (vx, vtop) in v_walls {
            for &(hr, hx0, hx1) in &h_walls {
                // Only the crossing that actually trims this segment's north end.
                if hr < vtop
                    && vtop - hr <= super::WALL_THICK_H + super::rooms::walls::WALL_BRIDGE_SLACK_PX
                    && (hx0..=hx1).contains(&vx)
                {
                    for y in hr..vtop {
                        for dx in 0..super::WALL_THICK_V {
                            assert!(
                                !l.is_walkable(vx + dx, y),
                                "{w}x{h} seed {seed}: walkable HOLE at ({},{y}) in the \
                                 divider corner between H wall @{hr} and V wall @{vtop}",
                                vx + dx,
                            );
                        }
                    }
                }
            }
        }
    });
}

/// Widths inside the narrow-band DEGRADATION zone the discrete `SWEEP_SIZES`
/// grid structurally skips. Floored by `MIN_LAYOUT_W`; the upper bound is 76,
/// not 64, to cover the FULL single-pod-column window: the desk grid stays one
/// column through buf_w≈70 and only splits to two (two drains, robust) at ≈71,
/// and the discrete grid's nearest points either side are 64 and 80.
pub(super) const NARROW_BAND: std::ops::RangeInclusive<u16> = super::compute::MIN_LAYOUT_W..=76;

/// The Y twin of the width scan below: the derived height floor opened 45..59, where
/// `pod_rows` collapses to 1 and the appliance aisle sits at its 8px minimum. The
/// discrete grid gives that band 6 points; this walks it at step 1.
#[test]
fn short_band_connectivity_boundary_scan() {
    for h in super::compute::MIN_LAYOUT_H..60 {
        for &w in &[super::compute::MIN_LAYOUT_W, 80, 120, 200] {
            for seed in SWEEP_SEEDS {
                let Some(l) = SceneLayout::compute_with_seed(w, h, None, seed) else {
                    panic!("{w}x{h} seed {seed}: refused above the floor");
                };
                assert!(
                    !l.home_desks.is_empty(),
                    "{w}x{h} seed {seed}: lays out with no desk to seat anyone"
                );
                assert_walkable_connected(w, h, seed, &l);
                assert_home_desk_approaches_are_routable(w, h, seed, &l);
            }
        }
    }
}

/// Step-1 width sweep across the degradation band, running BOTH connectivity
/// predicates at that resolution — the discrete `SWEEP_SIZES` grid can't cover
/// every width, and a sealed pocket at a width it skips ships silently. Heights
/// span the tall floors where the aisle-seal manifests.
#[test]
fn narrow_band_connectivity_boundary_scan() {
    for w in NARROW_BAND {
        // 46 reaches the SHORT band, where the appliance aisle sits at its 8px minimum.
        for &h in &[46u16, 80, 100, 120, 148, 160] {
            for seed in SWEEP_SEEDS {
                let Some(l) = SceneLayout::compute_with_seed(w, h, None, seed) else {
                    // Every size here is above BOTH floors, so a refusal is a bug —
                    // and `sweep_over`'s None arm covers only the discrete grid.
                    panic!("{w}x{h} seed {seed}: refused above the floor");
                };
                {
                    // `assert_home_desk_approaches_are_routable` passes VACUOUSLY on an
                    // empty one, and this scan visits the widths between the grid points.
                    assert!(
                        !l.home_desks.is_empty(),
                        "{w}x{h} seed {seed}: lays out with no desk to seat anyone"
                    );
                    assert_walkable_connected(w, h, seed, &l);
                    assert_home_desk_approaches_are_routable(w, h, seed, &l);
                }
            }
        }
    }
}

/// Each pod's vertical extent from the desks ALONE, so the guards below test the
/// layout's output rather than the placement helper's own idea of where the pods are.
fn pod_y_extents(l: &SceneLayout) -> Vec<(u16, u16)> {
    let mut ys: Vec<u16> = l.home_desks.iter().map(|d| d.at.y).collect();
    ys.sort_unstable();
    ys.dedup();
    let mut pods = Vec::new();
    let mut i = 0;
    while i < ys.len() {
        let top = ys[i];
        if ys.get(i + 1) == Some(&(top + DESK_H + INTRA_POD_GAP_Y)) {
            pods.push((top, ys[i + 1] + DESK_GROUND_H));
            i += 2;
        } else {
            pods.push((top, top + DESK_GROUND_H));
            i += 1;
        }
    }
    pods
}

fn assert_no_free_standing_piece_inside_a_pod(w: u16, h: u16, seed: u64, l: &SceneLayout) {
    let pods = pod_y_extents(l);
    for d in &l.wall_decor {
        let Some((g, gs)) = furniture_def(d.kind.furniture()).ground_rect(Pivot::TopLeft, d.pos)
        else {
            continue; // wall-hung: no ground contact, nothing to wedge into a pod
        };
        for &(top, bottom) in &pods {
            assert!(
                g.y >= bottom || g.y + gs.h <= top,
                "{w}x{h} seed {seed}: {:?}'s ground rows {}..{} fall inside the pod \
                 spanning {top}..{bottom} — free-standing furniture belongs in the \
                 aisles BETWEEN pods, never in a pod's own desk rows or intra-pod gap",
                d.kind,
                g.y,
                g.y + gs.h
            );
        }
    }
}

#[test]
fn free_standing_furniture_never_stands_inside_a_pod() {
    sweep(assert_no_free_standing_piece_inside_a_pod);
    sweep_production_floors(assert_no_free_standing_piece_inside_a_pod);
}

/// `snap_inter_pod_ground_y` answers `None` by design and the caller drops the board
/// with no trace, so a broken snap surfaces only as one fewer whiteboard. A lone pod
/// column is exempt, having no spot that hides nothing: see
/// `the_width_floor_stands_no_whiteboard`.
fn assert_the_whiteboard_lands_when_an_aisle_exists(w: u16, h: u16, seed: u64, l: &SceneLayout) {
    let has_side_rooms = !l.meeting_rooms.is_empty() || l.pantry.is_some();
    let mut desk_columns: Vec<u16> = l.home_desks.iter().map(|d| d.at.x).collect();
    desk_columns.sort_unstable();
    desk_columns.dedup();
    let lone_pod_column = desk_columns.len() <= usize::from(POD_SIDE);
    if !has_side_rooms || pod_y_extents(l).len() < 2 || lone_pod_column {
        return;
    }
    assert!(
        l.wall_decor
            .iter()
            .any(|d| matches!(d.kind, super::WallDecor::Whiteboard)),
        "{w}x{h} seed {seed}: {} pod rows leave an inter-pod aisle, but no whiteboard \
         was placed — the snap dropped it silently",
        pod_y_extents(l).len()
    );
}

#[test]
fn the_whiteboard_lands_whenever_an_inter_pod_aisle_exists() {
    sweep(assert_the_whiteboard_lands_when_an_aisle_exists);
    sweep_production_floors(assert_the_whiteboard_lands_when_an_aisle_exists);
}

fn assert_the_whiteboard_hides_no_desk_or_wall(w: u16, h: u16, seed: u64, l: &SceneLayout) {
    let Some(board) = l.fixtures().find(|f| {
        matches!(
            f.kind,
            FixtureKind::Wall {
                kind: super::WallDecor::Whiteboard,
                ..
            }
        )
    }) else {
        return;
    };
    let b = board.visual;
    let as_rect = |v: Bounds| {
        (
            Point { x: v.x, y: v.y },
            Size {
                w: v.width,
                h: v.height,
            },
        )
    };
    for f in l.fixtures().filter(|f| {
        matches!(
            f.kind,
            FixtureKind::Desk(_)
                | FixtureKind::DeskChair(_)
                | FixtureKind::FilingCabinet(_)
                | FixtureKind::Pod { .. }
        )
    }) {
        assert!(
            !rects_overlap(as_rect(b), as_rect(f.visual)),
            "{w}x{h} seed {seed}: the whiteboard {b:?} over {:?} {:?}",
            f.kind,
            f.visual
        );
    }
    for p in &l.wall_pieces {
        assert!(
            !rects_overlap(as_rect(b), p.visual()),
            "{w}x{h} seed {seed}: the whiteboard {b:?} over the wall {p:?}"
        );
    }
    for twin in l.fixtures().filter(|f| {
        matches!(
            f.kind,
            FixtureKind::Pod {
                kind: super::PodDecor::Whiteboard,
                ..
            }
        )
    }) {
        let t = twin.visual;
        assert!(
            t.x + t.width <= b.x || b.x + b.width <= t.x,
            "{w}x{h} seed {seed}: the whiteboard {b:?} in a pod whiteboard's columns {t:?}"
        );
    }
}

#[test]
fn the_whiteboard_hides_no_desk_and_stands_clear_of_the_walls_and_its_twin() {
    sweep(assert_the_whiteboard_hides_no_desk_or_wall);
    sweep_production_floors(assert_the_whiteboard_hides_no_desk_or_wall);
}

/// The width floor's lone pod column, where an unsnapped board once stood flush
/// against the divider with the desk column east of it and sealed the desks'
/// south: no spot there clears the column, so the floor stands no board.
#[test]
fn the_width_floor_stands_no_whiteboard() {
    let (w, h, seed) = (super::compute::MIN_LAYOUT_W, 120, 3);
    let l = SceneLayout::compute_with_seed(w, h, None, seed).expect("the width floor lays out");
    assert!(
        !l.wall_decor
            .iter()
            .any(|d| matches!(d.kind, super::WallDecor::Whiteboard)),
        "a lone pod column has no spot that hides nothing"
    );
    assert_eq!(
        l.plants.len(),
        2,
        "the two far-south plants survive — the guard spends nothing here"
    );
}

/// A machine is placed only where it clears every sitter, so a corner whose
/// seat check fails is dropped with no trace: every aisle that clears a
/// machine's gates must hold one.
fn assert_each_appliance_lands_where_its_aisle_fits(w: u16, h: u16, seed: u64, l: &SceneLayout) {
    use super::compute::{
        PRINTER_MIN_AISLE_H, PRINTER_MIN_AISLE_W, VENDING_MIN_AISLE_H, VENDING_MIN_AISLE_W,
    };
    let aisle = l.cubicle_aisle;
    for (kind, min_h, min_w) in [
        (
            WaypointKind::VendingMachine,
            VENDING_MIN_AISLE_H,
            VENDING_MIN_AISLE_W,
        ),
        (
            WaypointKind::Printer,
            PRINTER_MIN_AISLE_H,
            PRINTER_MIN_AISLE_W,
        ),
    ] {
        if aisle.height >= min_h && aisle.width > min_w {
            assert!(
                l.waypoints.iter().any(|wp| wp.kind == kind),
                "{w}x{h} seed {seed}: the aisle {aisle:?} fits a {kind:?}, but none stands"
            );
        }
    }
    let machines: Vec<Bounds> = l
        .fixtures()
        .filter(|f| {
            matches!(
                f.kind,
                FixtureKind::Station {
                    station: super::Station::VendingMachine | super::Station::Printer,
                    ..
                }
            )
        })
        .map(|f| f.visual)
        .collect();
    for m in &machines {
        assert!(
            aisle.x <= m.x && m.x + m.width <= aisle.x + aisle.width,
            "{w}x{h} seed {seed}: a machine {m:?} leaves the aisle {aisle:?}"
        );
    }
    if let [a, b] = machines[..] {
        assert!(
            !a.shares_columns(b),
            "{w}x{h} seed {seed}: the machines {a:?} and {b:?} share columns"
        );
    }
}

#[test]
fn each_appliance_lands_wherever_its_aisle_fits() {
    sweep(assert_each_appliance_lands_where_its_aisle_fits);
    sweep_production_floors(assert_each_appliance_lands_where_its_aisle_fits);
}

#[test]
fn desk_capacity_obeys_the_request_law() {
    // A sub-grid, because capacity re-computes the whole layout per n.
    for &(w, h) in &[(50u16, 80u16), (96, 100), (120, 96), (192, 158), (320, 180)] {
        for seed in 0..4u64 {
            let Some(full) = SceneLayout::compute_with_seed(w, h, None, seed) else {
                continue;
            };
            let cap = full.home_desks.len();
            for n in [1usize, cap.saturating_sub(1).max(1), cap, cap + 5] {
                let l = SceneLayout::compute_with_seed(w, h, Some(n), seed).expect("fits");
                assert_eq!(
                    l.home_desks.len(),
                    n.min(cap),
                    "{w}x{h} seed {seed}: Some({n}) must yield min({n}, cap={cap}) desks"
                );
            }
        }
    }
}

#[test]
fn every_kind_is_placed_somewhere_in_the_sweep() {
    use std::collections::BTreeSet;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    sweep(|_, _, _, l| {
        for wp in &l.waypoints {
            seen.insert(format!("wp:{:?}", wp.kind));
        }
        for pd in &l.pod_decor {
            seen.insert(format!("pod:{:?}", pd.kind));
        }
        for p in &l.plants {
            seen.insert(format!("plant:{:?}", p.kind));
        }
        for wd in &l.wall_decor {
            seen.insert(format!("wall:{:?}", wd.kind));
        }
        if l.floor_lamp().is_some() {
            seen.insert("floor_lamp".into());
        }
        if l.lounge_side_table().is_some() {
            seen.insert("lounge_side_table".into());
        }
        if l.fish_tank().is_some() {
            seen.insert("fish_tank".into());
        }
        if l.pantry.is_some_and(|p| p.kitchen_island.is_some()) {
            seen.insert("kitchen_island".into());
        }
        if l.couch_sprite_center().is_some() {
            seen.insert("couch".into());
        }
    });
    let mut missing: Vec<String> = Vec::new();
    for kind in WaypointKind::ALL {
        let k = format!("wp:{kind:?}");
        if !seen.contains(&k) {
            missing.push(k);
        }
    }
    for kind in PodDecor::ALL {
        let k = format!("pod:{kind:?}");
        if !seen.contains(&k) {
            missing.push(k);
        }
    }
    for kind in PlantKind::ALL {
        let k = format!("plant:{kind:?}");
        if !seen.contains(&k) {
            missing.push(k);
        }
    }
    // BulletinBoard: allowlisted — unplaced by design, registered for pack
    // authors, so it has no push site in compute.
    for kind in WallDecor::ALL
        .iter()
        .filter(|&&k| k != WallDecor::BulletinBoard)
    {
        let k = format!("wall:{kind:?}");
        if !seen.contains(&k) {
            missing.push(k);
        }
    }
    for fixed in [
        "floor_lamp",
        "lounge_side_table",
        "fish_tank",
        "kitchen_island",
        "couch",
    ] {
        if !seen.contains(fixed) {
            missing.push(fixed.into());
        }
    }
    assert!(
        missing.is_empty(),
        "kinds never placed across the whole sweep: {missing:?}"
    );
}

/// Seat waypoints carry no ground (the sofa body does), so their honest
/// containment is pos-in-room.
#[test]
fn every_meeting_slot_sits_in_its_room() {
    sweep(|w, h, seed, l| {
        for wp in &l.waypoints {
            let Some(room_id) = wp.room_id else { continue };
            let Some(b) = l.meeting_room_bounds(room_id) else {
                panic!(
                    "{w}x{h} seed {seed}: waypoint {:?} claims room {room_id} \
                     but that room has no bounds",
                    wp.kind
                );
            };
            assert!(
                contains_point(b, wp.pos),
                "{w}x{h} seed {seed}: {:?} slot {:?} sits outside its room {room_id} {b:?}",
                wp.kind,
                wp.pos
            );
        }
    });
}

/// Variant internals are private, so the shapes are compared observationally.
/// The signature needs `cubicle_band.x`: Senior differs from Standard, and
/// Lounge from OpenPlan, ONLY by the left-column percent — room presence alone
/// collapses the 5 variants to 3 shapes.
#[test]
fn the_sweep_reaches_every_floor_variant() {
    use std::collections::BTreeSet;
    let mut shapes: BTreeSet<(bool, bool, bool, u16)> = BTreeSet::new();
    for seed in SWEEP_SEEDS {
        let l = SceneLayout::compute_with_seed(240, 160, None, seed).expect("fits");
        shapes.insert((
            !l.meeting_rooms.is_empty(),
            l.pantry.is_some(),
            l.meeting_rooms.len() > 1,
            l.cubicle_band.x,
        ));
    }
    assert!(
        shapes.len() >= super::compute::FloorVariant::ALL.len(),
        "sweep seeds reach only {} of {} floor shapes: {shapes:?} — widen SWEEP_SEEDS",
        shapes.len(),
        super::compute::FloorVariant::ALL.len()
    );
}

/// Positions are arbitrary — only presence/absence in the census matters.
#[test]
fn plant_obstacle_census_honors_repels_plants() {
    let p = |x: u16, y: u16| Point { x, y };
    let rects = super::compute::plant_obstacle_rects(
        Some(p(10, 10)), // fish tank
        Some(p(50, 50)), // floor lamp
        Some(p(60, 60)), // side table
        Some(p(80, 40)), // kitchen island
        &[],             // no meeting rooms
    );
    assert_eq!(
        rects.len(),
        4,
        "every lounge and pantry singleton repels a plant"
    );
    assert!(
        super::compute::plant_obstacle_rects(None, None, None, None, &[]).is_empty(),
        "no singleton, no census"
    );
}

#[test]
fn scatter_plants_keep_obstacle_clearance_and_survive_by_sliding() {
    use super::compute::{PLANT_OBSTACLE_CLEARANCE_PX, ROOMY_BAND_MIN_W};
    let visual_tl = |pos: Point, v: Size| {
        (
            Point {
                x: pos.x.saturating_sub(v.w / 2),
                y: pos.y.saturating_sub(v.h / 2),
            },
            v,
        )
    };
    // The center-anchored predicate the WAYPOINT family needs; the fixed
    // non-waypoint singletons ride `plant_obstacle_rects` instead.
    let within_clearance = |plant_box: (Point, Size), obs_pos: Point, obs_v: Size| {
        let m = PLANT_OBSTACLE_CLEARANCE_PX;
        let (otl, osz) = visual_tl(obs_pos, obs_v);
        let inflated = (
            Point {
                x: otl.x.saturating_sub(m),
                y: otl.y.saturating_sub(m),
            },
            Size {
                w: osz.w + 2 * m,
                h: osz.h + 2 * m,
            },
        );
        super::placement::rects_overlap(plant_box, inflated)
    };
    let mut v = Vec::new();
    sweep(|w, h, seed, l| {
        for p in &l.plants {
            let pv = furniture_def(p.kind.furniture()).visual;
            let plant_box = visual_tl(p.pos, pv);
            // The SAME `plant_obstacle_rects` census the production settle path uses,
            // never a second copy: a census miss here IS a plant interpenetration.
            for (otl, osz) in super::compute::plant_obstacle_rects(
                l.fish_tank(),
                l.floor_lamp(),
                l.lounge_side_table(),
                l.pantry.and_then(|pr| pr.kitchen_island),
                &l.meeting_rooms,
            ) {
                if super::placement::overlaps_within_clearance(
                    plant_box,
                    (otl, osz),
                    PLANT_OBSTACLE_CLEARANCE_PX,
                ) {
                    v.push(format!(
                        "{w}x{h} seed {seed}: plant {:?}@{:?} within clearance of a repels-plants singleton @{:?}",
                        p.kind, p.pos, otl
                    ));
                }
            }
            for wp in &l.waypoints {
                let def = furniture_def(wp.kind.furniture());
                if def.footprint.is_none() {
                    continue;
                }
                if within_clearance(plant_box, wp.pos, def.visual) {
                    v.push(format!(
                        "{w}x{h} seed {seed}: plant {:?}@{:?} within clearance of {:?}@{:?}",
                        p.kind, p.pos, wp.kind, wp.pos
                    ));
                }
            }
        }
        // Pinned only where room exists — a narrower band leaves the slide nowhere
        // to land — so this guards an office-WIDE loss at flagship sizes only.
        if l.cubicle_band.width >= ROOMY_BAND_MIN_W {
            let has = |k: WaypointKind| l.waypoints.iter().any(|w| w.kind == k);
            let corridor_plant = |kind: PlantKind| {
                l.plants
                    .iter()
                    .any(|p| p.kind == kind && p.pos.y + 6 >= l.cubicle_aisle.y)
            };
            if has(WaypointKind::VendingMachine) && !corridor_plant(PlantKind::Flower) {
                v.push(format!(
                    "{w}x{h} seed {seed}: vending cost the corridor its Flower"
                ));
            }
            if has(WaypointKind::Printer) && !corridor_plant(PlantKind::Succulent) {
                v.push(format!(
                    "{w}x{h} seed {seed}: printer cost the corridor its Succulent"
                ));
            }
        }
    });
    assert!(
        v.is_empty(),
        "{} violations (first 6):\n{}",
        v.len(),
        v[..v.len().min(6)].join("\n")
    );
}

/// `is_visually_clear`'s completeness against the already-compile-forced
/// enumeration. Its no-`..` destructure catches a new COLLECTION; this catches a
/// member of an existing one read with the wrong anchor, kind or size — the class
/// that shipped incomplete twice (lounge + the runtime-sized kinds, then the
/// free-standing whiteboard).
#[test]
fn every_placed_sprite_is_opaque_to_the_visual_clearance_predicate() {
    let mut checked = 0u32;
    sweep(|w, h, seed, l| {
        for p in pieces(l) {
            let (tl, sz) = p.visual;
            if sz.w == 0 || sz.h == 0 {
                continue; // runtime-sized: the table has no rect to centre on
            }
            let mid = Point {
                x: tl.x + sz.w / 2,
                y: tl.y + sz.h / 2,
            };
            assert!(
                !l.is_visually_clear(mid),
                "{w}x{h} seed {seed}: {} paints over {mid:?} but the predicate calls it clear",
                p.label
            );
            checked += 1;
        }
    });
    assert!(checked > 1000, "the sweep must reach pieces, saw {checked}");
}

/// The north wall band is a WALL PLANE, not floor — nothing in it is walkable,
/// the elevator included. Its doorway is a hole in the WALL, not in the ground:
/// `door_threshold` already stands clear to the south, so a cut here only ever
/// produced walkable wall, and `creatures::walkable_target` draws wander
/// destinations straight off this mask (#902 — a cat wandering up the channel).
#[test]
fn no_cell_of_the_north_wall_band_is_walkable() {
    let mut checked = 0usize;
    sweep(|w, h, seed, l| {
        let band = l.wall_band_h();
        assert!(band > 0, "{w}x{h} seed {seed}: no wall band to check");
        for y in 0..band {
            for x in 0..l.walkable.width() {
                assert!(
                    !l.walkable.is_walkable(x, y),
                    "{w}x{h} seed {seed}: ({x},{y}) is walkable inside the wall band \
                     (rows 0..{band}), door at {:?}",
                    l.door
                );
            }
        }
        checked += 1;
    });
    assert!(checked > 0, "the sweep produced no layouts");
}

/// A pot must not stand on the same pixel of every floor. `settle_plant` was
/// already authorised to slide `MAX_PLANT_NUDGE_PX` inward to dodge an
/// obstacle; the seeded first step spends part of that same budget on variety,
/// so this asks only that the budget is actually being spent.
#[test]
fn a_plants_spot_varies_by_floor() {
    use std::collections::HashSet;
    for &(w, h) in &[(96u16, 70u16), (160, 120), (192, 158)] {
        let mut seen: HashSet<Vec<(u16, u16)>> = HashSet::new();
        for f in 0..crate::floor::MAX_FLOORS {
            if let Some(l) = SceneLayout::compute_with_seed(w, h, None, crate::floor::floor_seed(f))
            {
                seen.insert(l.plants.iter().map(|p| (p.pos.x, p.pos.y)).collect());
            }
        }
        assert!(
            seen.len() > crate::floor::MAX_FLOORS / 2,
            "{w}x{h}: only {} distinct plant arrangements over {} floors — the \
             seeded step is not reaching the pots",
            seen.len(),
            crate::floor::MAX_FLOORS
        );
    }
}

/// Each PASS of the decor bag is a permutation: a floor sees every kind before
/// any repeats. That is the property a bare rotation was picked for, and the
/// reason a plain hash was rejected before it — a shuffled bag has to keep it.
#[test]
fn each_pass_of_the_decor_bag_is_a_permutation_of_the_roster() {
    use crate::layout::PodDecor;
    let n = PodDecor::ALL.len();
    let pass_at = |seed: u64, pass: usize| -> Vec<PodDecor> {
        (0..n)
            .map(|i| super::compute::decor_for_slot(seed, pass * n + i))
            .collect()
    };
    let mut saw_two_orders = false;
    for f in 0..crate::floor::MAX_FLOORS {
        let seed = crate::floor::floor_seed(f);
        for pass in 0..4 {
            let got = pass_at(seed, pass);
            for kind in PodDecor::ALL {
                assert_eq!(
                    got.iter().filter(|k| *k == kind).count(),
                    1,
                    "floor {f} pass {pass}: {kind:?} is not exactly once in {got:?}"
                );
            }
            saw_two_orders |= got != pass_at(seed, 0);
        }
    }
    assert!(
        saw_two_orders,
        "every pass has the SAME order — that is a rotation, not a bag, and a \
         floor wide enough for a second pass repeats it verbatim"
    );
}

/// Every pod-decor kind must be able to open a floor's deal — a one-slot floor
/// renders only whatever the bag deals first, so a kind that never leads is a
/// sprite that never appears there. The rotation this replaced stranded `Tv`
/// exactly that way on all ten production floors.
#[test]
fn every_pod_decor_kind_can_open_a_floor() {
    let mut seen: Vec<crate::layout::PodDecor> = (0..crate::floor::MAX_FLOORS)
        .map(|f| super::compute::decor_for_slot(crate::floor::floor_seed(f), 0))
        .collect();
    seen.sort_by_key(|k| format!("{k:?}"));
    seen.dedup();
    assert_eq!(
        seen.len(),
        crate::layout::PodDecor::ALL.len(),
        "only {seen:?} ever open a floor — a phase modulus that misses the \
         roster strands the rest"
    );
}

/// A wide floor's aisles must not read as one repeating cycle. A rotation can
/// only ever produce `PodDecor::ALL.len()` arrangements however many slots a
/// floor has, because one slot's kind fixes every later one.
#[test]
fn a_wide_floors_decor_order_is_not_one_fixed_cycle() {
    use std::collections::HashSet;
    for &(w, h) in &[(160u16, 120u16), (192, 158), (200, 116)] {
        let seen: HashSet<Vec<crate::layout::PodDecor>> = (0..crate::floor::MAX_FLOORS)
            .filter_map(|f| {
                SceneLayout::compute_with_seed(w, h, None, crate::floor::floor_seed(f))
                    .map(|l| l.pod_decor.iter().map(|d| d.kind).collect())
            })
            .collect();
        assert!(
            seen.len() > crate::layout::PodDecor::ALL.len(),
            "{w}x{h}: only {} distinct aisle arrangements over {} floors — no more \
             than a rotation of the roster would give",
            seen.len(),
            crate::floor::MAX_FLOORS
        );
    }
}

/// No two adjacent aisle slots share a kind. The rotation this bag replaced had
/// it by construction; a bag has to be told, because a fresh permutation may
/// open on the kind the last one closed with — two identical pieces in
/// neighbouring aisles is the failure a user sees first.
#[test]
fn no_two_adjacent_aisle_slots_share_a_kind() {
    // Enough passes that the pass BOUNDARY is exercised, not just one bag.
    let span = crate::layout::PodDecor::ALL.len() * 4;
    for f in 0..crate::floor::MAX_FLOORS {
        let seed = crate::floor::floor_seed(f);
        let kinds: Vec<_> = (0..span)
            .map(|i| super::compute::decor_for_slot(seed, i))
            .collect();
        for i in 1..kinds.len() {
            assert_ne!(
                kinds[i],
                kinds[i - 1],
                "floor {f}: slots {} and {i} are both {:?}",
                i - 1,
                kinds[i]
            );
        }
    }
}

/// The seed PACKING, frozen by value. Every other seat/plant test asserts a
/// property that survives any uniform seed — a swapped packing passes all of
/// them while moving every chair and pot, which no other gate notices.
#[test]
fn point_seed_packing_is_frozen() {
    assert_eq!(
        super::decor::point_seed(Point { x: 1, y: 2 }),
        (1u64 << 32) | 2,
        "x occupies the high word and y the low one"
    );
    assert_eq!(super::decor::point_seed(Point { x: 0, y: 0 }), 0);
    assert_ne!(
        super::decor::point_seed(Point { x: 3, y: 5 }),
        super::decor::point_seed(Point { x: 5, y: 3 }),
        "the packing must not be symmetric in x and y"
    );
}
