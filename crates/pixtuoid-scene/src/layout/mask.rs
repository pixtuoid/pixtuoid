//! Walkable-mask construction: stamps every obstacle into a `WalkableMask` so
//! A* knows where characters can route.

use super::decor::{FurnitureDef, GroundAlign};
use super::{
    Furniture, MeetingRoom, PANTRY_FOOTPRINT_DEPTH, Pivot, PlantItem, PodDecorItem, Point, Size,
    WALL_THICK_V, WallDecorItem, Waypoint, WaypointKind, anchored_top_left, furniture_def,
};
use pixtuoid_core::walkable::WalkableMask;

/// Stamp a furniture footprint as a collision rect DECLARED relative to its
/// VISUAL box. The visual top-left comes from `pivot` via `anchored_top_left`
/// (the SAME origin the renderer blits from, so blocked ground and painted
/// sprite can't drift); the footprint is offset inside it by the row's
/// [`GroundAlign`] per axis. A footprint-less piece (wall-hung decor) stamps
/// nothing; runtime-footprint pieces build their rect and call `mark_blocked`
/// directly. The pad is the row's [`FurnitureDef::pad`].
fn stamp_ground(mask: &mut WalkableMask, def: &FurnitureDef, pivot: Pivot, pos: Point) {
    if let Some((tl, sz)) = def.ground_rect(pivot, pos) {
        mask.mark_blocked(tl.x, tl.y, sz.w, sz.h, def.pad);
    }
}

/// The ONE ground-geometry formula: the blocked rect (top-left + size) of a
/// piece's footprint, declared relative to its VISUAL box. Shared by the mask
/// AND the placement sweep's containment/overlap invariants, so the sweep can't
/// grow a second copy of the offset math. The pad is deliberately NOT part of the
/// rect: it is routing slack, not the object; `mark_blocked` adds it.
pub(super) fn ground_rect(
    pivot: Pivot,
    pos: Point,
    fp: Size,
    visual: Size,
    ground_x: GroundAlign,
    ground_y: GroundAlign,
) -> (Point, Size) {
    let vis_tl = anchored_top_left(pivot, pos, visual.w, visual.h);
    let left = vis_tl.x + ground_x.offset(visual.w, fp.w);
    let top = vis_tl.y + ground_y.offset(visual.h, fp.h);
    (Point { x: left, y: top }, fp)
}

/// The pantry counter's blocked-ground rect — the RUNTIME-sized twin of
/// [`ground_rect`] (the counter's `FurnitureDef` row is `footprint: None`; its
/// real size arrives per-layout as `pantry_counter_size`). A shallow
/// `PANTRY_FOOTPRINT_DEPTH` strip anchored to the sprite base — the walk-behind
/// shape. Shared by the mask stamp AND the placement sweep so it can't fork.
pub(super) fn pantry_ground_rect(pos: Point, counter: Size) -> (Point, Size) {
    let depth = PANTRY_FOOTPRINT_DEPTH.min(counter.h);
    ground_rect(
        Pivot::Center,
        pos,
        Size {
            w: counter.w,
            h: depth,
        },
        counter,
        GroundAlign::Center,
        GroundAlign::End,
    )
}

use super::rooms::walls::WallPiece;

/// WEST-only routing clearance stamped onto a vertical wall's footprint (NOT
/// part of the physical footprint — the placement sweep reads the un-margined
/// rect). The coarse 4×4 router (`pathfind::cell_walkable`) can't see a barrier
/// thinner than a cell: a bare `WALL_THICK_V` at a 2-mod-4 offset splits into
/// two cells at exactly the walkable threshold, so A* threads through. Stamped
/// toward the WEST (room side) so the east, band-facing mask edge stays flush
/// with the visual.
const WALL_ROUTING_MARGIN_X: u16 = 1;

/// The placed-piece inventory the mask stamps — a NAMED input so the
/// interchangeable `Option<Point>` pieces can't be positionally swapped, and so
/// `build_walkable_mask` destructures it with NO `..` (a new field added here
/// must then be handled by the mask, not silently walked through).
pub(super) struct MaskObstacles<'a> {
    pub(super) buf_w: u16,
    pub(super) buf_h: u16,
    pub(super) top_margin: u16,
    pub(super) home_desks: &'a [Point],
    pub(super) meeting_rooms: &'a [MeetingRoom],
    pub(super) kitchen_island: Option<Point>,
    pub(super) waypoints: &'a [Waypoint],
    pub(super) plants: &'a [PlantItem],
    pub(super) floor_lamp: Option<Point>,
    pub(super) lounge_side_table: Option<Point>,
    pub(super) fish_tank: Option<Point>,
    pub(super) wall_decor: &'a [WallDecorItem],
    pub(super) pod_decor: &'a [PodDecorItem],
    pub(super) wall_pieces: &'a [WallPiece],
    pub(super) pantry_counter_size: Size,
}

#[cfg(test)]
impl MaskObstacles<'_> {
    /// An empty floor: a test names only the pieces it stamps, via `..bare(..)`.
    pub(super) fn bare(buf_w: u16, buf_h: u16) -> Self {
        Self {
            buf_w,
            buf_h,
            top_margin: super::WALL_BAND_TO_TOP_MARGIN,
            home_desks: &[],
            meeting_rooms: &[],
            kitchen_island: None,
            waypoints: &[],
            plants: &[],
            floor_lamp: None,
            lounge_side_table: None,
            fish_tank: None,
            wall_decor: &[],
            pod_decor: &[],
            wall_pieces: &[],
            pantry_counter_size: Size { w: 0, h: 0 },
        }
    }
}

/// The first column east of `from` on row `y` that the mask leaves walkable,
/// scanning from a blocked `from`.
#[cfg(test)]
pub(super) fn first_walkable_east(mask: &WalkableMask, from: u16, y: u16) -> u16 {
    (from..mask.width())
        .take_while(|&x| !mask.is_walkable(x, y))
        .last()
        .expect("the piece blocks its own column")
        + 1
}

pub(super) fn build_walkable_mask(obs: &MaskObstacles) -> WalkableMask {
    let &MaskObstacles {
        buf_w,
        buf_h,
        top_margin,
        home_desks,
        meeting_rooms,
        kitchen_island,
        waypoints,
        plants,
        floor_lamp,
        lounge_side_table,
        fish_tank,
        wall_decor,
        pod_decor,
        wall_pieces,
        pantry_counter_size,
    } = obs;

    let mut mask = WalkableMask::new_open(buf_w, buf_h);

    // To the WALL VISUAL bottom, not the full top_margin: the rows between are
    // carpet apron, and blocking them pushed the walkable boundary south of the
    // visible wall base (invariant #6).
    mask.mark_blocked(0, 0, buf_w, super::wall_band_h(top_margin), 0);
    const BASEBOARD_H: u16 = 3;

    let baseboard_top = buf_h.saturating_sub(BASEBOARD_H);
    mask.mark_blocked(0, baseboard_top, buf_w, BASEBOARD_H, 0);

    // Both block their FULL visual footprint (invariant #6); only the router
    // clearance is asymmetric — horizontal faces already fill a routing cell top to
    // bottom, while vertical walls are thinner and take `WALL_ROUTING_MARGIN_X`
    // westward.
    for piece in wall_pieces {
        let (origin, size) = piece.footprint();
        let (west, east) = match *piece {
            WallPiece::Vertical { .. } => (WALL_ROUTING_MARGIN_X, 0),
            // A post a door leaves at a run's end is as thin to the router as a
            // bare vertical wall, so it widens to one, away from its door.
            WallPiece::Horizontal { jamb_west, .. } => {
                let short = WALL_THICK_V.saturating_sub(size.w);
                if jamb_west { (0, short) } else { (short, 0) }
            }
        };
        let x = origin.x.saturating_sub(west);
        mask.mark_blocked(x, origin.y, size.w + (origin.x - x) + east, size.h, 0);
    }

    for desk in home_desks {
        // WALK-BEHIND: the footprint is a shallow south strip at the sprite base, so
        // the monitor overhangs NORTH and a walker behind it is occluded by the desk's
        // own y-sorted sprite. Stamped TOP-LEFT — the desk pos IS its NW corner.
        let desk_def = super::decor::desk_furniture_def();
        stamp_ground(&mut mask, &desk_def, Pivot::TopLeft, *desk);
    }

    for trio in meeting_rooms.iter().filter_map(|r| r.trio.as_ref()) {
        // The sofa's pad is what gives vertical sit clearance — see its
        // `furniture_def` row.
        let sofa_def = furniture_def(Furniture::MeetingSofaBody);
        for sofa in trio.sofas {
            stamp_ground(&mut mask, &sofa_def, Pivot::Center, sofa);
        }
        let table_def = furniture_def(Furniture::MeetingTable);
        stamp_ground(&mut mask, &table_def, Pivot::Center, trio.table);
    }

    if let Some(island) = kitchen_island {
        // South-anchored base only; the countertop rows overhang (walk-behind).
        let def = furniture_def(Furniture::KitchenIsland);
        stamp_ground(&mut mask, &def, Pivot::Center, island);
    }

    for wp in waypoints {
        // `None` = meeting slots, which sit/stand on sofa/table furniture
        // already stamped above — no obstacle of their own.
        let Some(Size { w, h }) = super::approach::obstacle_footprint(wp.kind, pantry_counter_size)
        else {
            continue;
        };
        if matches!(wp.kind, WaypointKind::Pantry) {
            // Only the SOUTH base sits on the floor; cabinet tops and backsplash are
            // overhang (invariant #6). `stand_point` uses the FULL visual instead, so
            // the USER parks clear of the whole counter rather than inside it.
            let (tl, sz) = pantry_ground_rect(wp.pos, Size { w, h });
            mask.mark_blocked(tl.x, tl.y, sz.w, sz.h, furniture_def(Furniture::Pantry).pad);
            continue;
        }
        let def = furniture_def(wp.kind.furniture());
        let (tl, sz) = def.ground_rect_of(Pivot::Center, wp.pos, Size { w, h });
        mask.mark_blocked(tl.x, tl.y, sz.w, sz.h, def.pad);
    }

    for &PlantItem { kind, pos } in plants {
        let def = furniture_def(kind.furniture());
        stamp_ground(&mut mask, &def, Pivot::Center, pos);
    }

    if let Some(lamp) = floor_lamp {
        let def = furniture_def(Furniture::FloorLamp);
        stamp_ground(&mut mask, &def, Pivot::Center, lamp);
    }

    if let Some(t) = lounge_side_table {
        let def = furniture_def(Furniture::LoungeSideTable);
        stamp_ground(&mut mask, &def, Pivot::Center, t);
    }

    if let Some(t) = fish_tank {
        let def = furniture_def(Furniture::FishTank);
        stamp_ground(&mut mask, &def, Pivot::Center, t);
    }

    for &WallDecorItem { kind, pos } in wall_decor {
        let def = furniture_def(kind.furniture());
        stamp_ground(&mut mask, &def, Pivot::TopLeft, pos);
    }

    // PhoneBooth + StandingDesk double-block as waypoints too; `mark_blocked` is
    // idempotent.
    for &PodDecorItem { kind, pos } in pod_decor {
        let def = furniture_def(kind.furniture());
        stamp_ground(&mut mask, &def, Pivot::Center, pos);
    }

    mask
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{WALL_THICK_V, sort_row_at};

    #[test]
    fn vertical_wall_blocks_its_whole_visual_width_in_the_mask() {
        let l = crate::layout::SceneLayout::compute_with_seed(200, 130, Some(8), 0).unwrap();
        let piece = l
            .wall_pieces
            .iter()
            .find(|p| matches!(p, WallPiece::Vertical { .. }))
            .copied()
            .expect("a vertical wall");
        let (o, s) = piece.footprint();
        let y = o.y + s.h / 2; // deep in the wall body, clear of any north overhang
        for dx in 0..WALL_THICK_V {
            assert!(
                !l.is_walkable(o.x + dx, y),
                "visual column {} must be blocked (no feet-in-wall)",
                o.x + dx
            );
        }
    }

    #[test]
    fn overhang_footprint_south_anchored_leaves_the_overhang_walkable() {
        // The fixture is a phone booth: a 6-wide sprite 12 tall over a 3-row
        // ground base.
        let mut mask = WalkableMask::new_open(40, 40);
        let pos = Point { x: 20, y: 20 };
        let (tl, sz) = ground_rect(
            Pivot::Center,
            pos,
            Size { w: 6, h: 3 },
            Size { w: 6, h: 12 },
            GroundAlign::Center,
            GroundAlign::End,
        );
        mask.mark_blocked(tl.x, tl.y, sz.w, sz.h, 0);
        let south = sort_row_at(Pivot::Center, pos, 12);
        for dy in 0..3 {
            assert!(
                !mask.is_walkable(pos.x, south - dy),
                "base row {} must be blocked",
                south - dy
            );
        }
        assert!(
            mask.is_walkable(pos.x, south - 4),
            "overhang region north of the base must stay walkable (walker parks here)"
        );
        assert!(
            mask.is_walkable(pos.x, pos.y.saturating_sub(5)),
            "sprite-top region must stay walkable"
        );
    }

    #[test]
    fn wall_decor_whiteboard_footprint_centers_under_the_wider_sprite() {
        // TopLeft-anchored, and its ground footprint is the WHEEL SPAN at sprite
        // cols 2 and 11 — hence the probe offsets below.
        use crate::layout::WallDecor;
        let pos = Point { x: 40, y: 30 };
        let wall_decor = vec![WallDecorItem {
            kind: WallDecor::Whiteboard,
            pos,
        }];
        let mask = build_walkable_mask(&MaskObstacles {
            buf_w: 120,
            buf_h: 96,
            top_margin: 20,
            home_desks: &[],
            meeting_rooms: &[],
            kitchen_island: None,
            waypoints: &[],
            plants: &[],
            floor_lamp: None,
            lounge_side_table: None,
            fish_tank: None,
            wall_decor: &wall_decor,
            pod_decor: &[],
            wall_pieces: &[],
            pantry_counter_size: Size { w: 20, h: 8 },
        });
        let def = furniture_def(Furniture::Whiteboard);
        let sprite_h = def.visual.h;
        let base = pos.y + sprite_h - 1;
        assert!(
            !mask.is_walkable(pos.x + 2, base),
            "west wheel column must be blocked"
        );
        assert!(
            !mask.is_walkable(pos.x + 11, base),
            "east wheel column must be blocked"
        );
        assert!(
            mask.is_walkable(pos.x, base),
            "bare floor west of the wheels must stay walkable"
        );
        assert!(
            mask.is_walkable(pos.x + 13, base),
            "floor east of the wheels+pad must stay walkable"
        );
    }

    #[test]
    fn flat_box_footprint_is_centered_not_south_anchored() {
        // The fixture is a flat box: visual == footprint (vending/printer/table).
        let mut mask = WalkableMask::new_open(40, 40);
        let pos = Point { x: 20, y: 20 };
        let (tl, sz) = ground_rect(
            Pivot::Center,
            pos,
            Size { w: 4, h: 6 },
            Size { w: 4, h: 6 },
            GroundAlign::Center,
            GroundAlign::Center,
        );
        mask.mark_blocked(tl.x, tl.y, sz.w, sz.h, 0);
        assert!(
            !mask.is_walkable(pos.x, pos.y),
            "centered block: center blocked"
        );
        assert!(
            !mask.is_walkable(pos.x, pos.y - 2),
            "centered block: north-of-center blocked (not a south strip)"
        );
    }
}
