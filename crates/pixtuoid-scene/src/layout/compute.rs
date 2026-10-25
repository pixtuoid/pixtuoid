//! Layout computation helpers for `SceneLayout`.

use super::decor::{DESK_GROUND_H, DESK_GROUND_W};
use super::mask;
use super::*;

/// Counter width that marks the LARGE (detailed kitchen) pantry sprite;
/// consumers test `>= PANTRY_COUNTER_LARGE_W` rather than the bare literal.
pub(crate) const PANTRY_COUNTER_LARGE_W: u16 = 32;

/// Horizontal seat offsets for a 3-across sofa, relative to the middle-seat
/// anchor — shared by the lounge couch and the meeting sofas.
const SEAT_DX: [i16; 3] = [-6, 0, 6];

/// A band this wide has room for flanking greenery: the lounge pot's west edge,
/// plus breathing room.
pub(super) const ROOMY_BAND_MIN_W: u16 = 60;

/// Air kept between a scatter plant's sprite box and any obstacle waypoint's
/// visual box — 1px apart in the same column reads as one totem.
pub(super) const PLANT_OBSTACLE_CLEARANCE_PX: u16 = 3;

/// Gap kept between the fish tank's east edge and the elevator door column so
/// the spawn threshold never routes around furniture.
pub(super) const FISH_TANK_ELEVATOR_CLEARANCE: u16 = 2;

fn couch_pos(cubicle_band: &Bounds, top_margin: u16, west_clear_x: u16) -> Point {
    const COUCH_X_PCT: u16 = 35;
    const COUCH_BELOW_TOP_MARGIN: u16 = 3;
    // `west_clear_x` is the divider wall's east edge — the westmost seat's ground must
    // stay east of it (== band start with no wall, so the clamp is a no-op).
    let seat_reach =
        (-SEAT_DX[0]) as u16 + furniture_def(Furniture::Couch).footprint.map_or(0, |f| f.w) / 2;
    // Its side table stands west of it ([`LoungeFlanks`]).
    let couch_west_reach = seat_reach.max(LoungeFlanks::west_reach());
    Point {
        x: (cubicle_band.x + pct(cubicle_band.width, COUCH_X_PCT))
            .max(west_clear_x + couch_west_reach),
        y: top_margin + COUCH_BELOW_TOP_MARGIN,
    }
}

/// The band width that seats ONE desk: the two terms `compute_pod_desks`' x-clamp
/// compares — the first pod's aisle half, plus the desk's blocked GROUND width (side
/// cabinets included, so NOT `DESK_W`).
pub(super) const DESK_BAND_MIN_W: u16 = INTER_POD_AISLE_X / 2 + DESK_GROUND_W;

/// The Y twin. The y-clamp adds a third term, `couch_to_desk_extra`, which is 0
/// everywhere below `COUCH_GAP_GROWTH_BASE_H` — pinned by the const assert there.
pub(super) const DESK_BAND_MIN_H: u16 = INTER_POD_AISLE_Y / 2 + DESK_GROUND_H;

/// The 1px column between the left rooms and the cubicle band — `right_x` steps
/// over it and `band_w` subtracts it, so it is one const, not two literals.
const MID_DIVIDER_W: u16 = 1;

/// The cubicle band's WIDTH for a buffer — everything east of the left column and its
/// divider. THE formula `compute_with_seed` and `min_layout_w` both step from.
pub(super) const fn band_w(buf_w: u16, mid_x_pct: u16) -> u16 {
    buf_w.saturating_sub(pct(buf_w, mid_x_pct) + MID_DIVIDER_W)
}

/// The band's HEIGHT — everything below the wall band, less the appliance aisle. THE
/// formula `compute_with_seed` and `min_layout_h` both step from, and it SATURATES: a
/// sub-`top_margin` buffer collapses to 0 rather than wrapping to 58,974 and an office
/// of garbage desks. Only observable below the size gate, so no test can hold it — the
/// gate and the use site together are what keep it unobservable.
pub(super) const fn band_h(buf_h: u16) -> u16 {
    let usable = buf_h.saturating_sub(top_margin(buf_h));
    usable.saturating_sub(cubicle_aisle_h(usable))
}

/// `n` percent of `v`, never under `min`.
const fn pct_at_least(v: u16, n: u16, min: u16) -> u16 {
    let share = pct(v, n);
    if share > min { share } else { min }
}

const TOP_MARGIN_PCT: u16 = 30;

/// The north wall band's depth — [`TOP_MARGIN_PCT`] percent of the buffer, never
/// under [`MIN_TOP_MARGIN`].
pub(super) const fn top_margin(buf_h: u16) -> u16 {
    pct_at_least(buf_h, TOP_MARGIN_PCT, MIN_TOP_MARGIN)
}

const fn cubicle_aisle_h(usable_h: u16) -> u16 {
    const CUBICLE_AISLE_PCT: u16 = 10;
    pct_at_least(usable_h, CUBICLE_AISLE_PCT, MIN_CUBICLE_AISLE_H)
}

/// The appliance aisle south of the pods never shrinks below this.
const MIN_CUBICLE_AISLE_H: u16 = 8;

/// The smallest buffer `compute_with_seed` lays out; below either it returns `None`
/// ("terminal too small"). BOTH axes are SOLVED against the band, not the buffer, and
/// the width against the north wall too: no narrower wall hangs the door clear of
/// the neon.
pub(super) const MIN_LAYOUT_W: u16 = min_layout_w();
const _: () = assert!(MIN_LAYOUT_W >= super::NEON_DOOR_WALL_W);
// The shortest wall band's windows stand as tall as the door, so every office
// that lays out has one.
const _: () = assert!(
    super::wall_trim_row(super::wall_band_h(super::MIN_TOP_MARGIN)) - super::WINDOW_TOP
        >= super::ELEVATOR_H
);
pub(super) const MIN_LAYOUT_H: u16 = min_layout_h();

/// The widest left column any variant takes — the band gets the rest, so this
/// variant is the one that prices the width floor for every seed.
const fn widest_mid_x_pct() -> u16 {
    let mut widest = 0;
    let mut i = 0;
    while i < FloorVariant::ALL.len() {
        let pct = FloorVariant::ALL[i].mid_x_pct();
        if pct > widest {
            widest = pct;
        }
        i += 1;
    }
    widest
}

const fn min_layout_w() -> u16 {
    let mut w = if DESK_BAND_MIN_W > super::NEON_DOOR_WALL_W {
        DESK_BAND_MIN_W
    } else {
        super::NEON_DOOR_WALL_W
    };
    while band_w(w, widest_mid_x_pct()) < DESK_BAND_MIN_W {
        w += 1;
    }
    w
}

const fn min_layout_h() -> u16 {
    let mut h = DESK_BAND_MIN_H;
    while band_h(h) < DESK_BAND_MIN_H {
        h += 1;
    }
    h
}

/// The smallest buffer that lays out, for a painter that has to TELL the user
/// why it is not drawing an office. Buffer units — each painter owns its own
/// cell aspect, so only it can turn this into rows and columns.
pub fn min_layout_size() -> Size {
    Size {
        w: MIN_LAYOUT_W,
        h: MIN_LAYOUT_H,
    }
}

/// `DESK_BAND_MIN_H` omits the Y clamp's third term because it is 0 at the floor;
/// that holds only while the growth base stays above it.
const _: () = assert!(COUCH_GAP_GROWTH_BASE_H > MIN_LAYOUT_H);

const fn couch_to_desk_extra(buf_h: u16) -> u16 {
    const BUF_ROWS_PER_GAP_ROW: u16 = 20;
    buf_h.saturating_sub(COUCH_GAP_GROWTH_BASE_H) / BUF_ROWS_PER_GAP_ROW
}

/// Buffer height at which the couch-to-desk gap starts growing. It equalled the
/// old hand-written `MIN_LAYOUT_H` by coincidence, not by derivation — re-tying it
/// to the now-lower floor would widen the gap on most offices tall enough to render
/// today, so it keeps its own name and its own number.
const COUCH_GAP_GROWTH_BASE_H: u16 = 60;

/// A meeting room narrower than this can't host the sofa body with enough
/// walkable margin for the coarse router ([`COARSE_CELL_SIZE`]) to reach the
/// seats buried in it —
/// find_path returns None and an idle agent sent there TELEPORTS. Below it the
/// room degrades to bare floor.
const MEETING_FURNITURE_MIN_W: u16 = 30;

/// Whether a meeting room's bounds can host its sofa/table trio — wide enough
/// for the sofa body + router margin AND tall enough for the trio.
fn room_fits_furniture(mr: &Bounds) -> bool {
    mr.width >= MEETING_FURNITURE_MIN_W && mr.height >= MeetingRoom::trio_fit_h()
}

/// The floor's zones, fixed before the free-standing furniture, plants and decor
/// are placed in them: the side rooms west of the divider, and the cubicle band
/// with its aisle east of it.
struct FloorPlan {
    buf_w: u16,
    buf_h: u16,
    top_margin: u16,
    usable_h: u16,
    /// The west bound lounge furniture and the whiteboard must clear: the divider
    /// wall's east edge, or the band start when no meeting room encloses one.
    lounge_west_clear: u16,
    cubicle_aisle: Bounds,
    pod_grid: PodGrid,
    pantry: Option<Bounds>,
    pantry_counter_size: Size,
    /// Indexed by room id; room 0 is the top room.
    meeting_rooms: Vec<MeetingRoom>,
}

impl FloorPlan {
    /// The zones a `buf_w`×`buf_h` floor of `floor_seed`'s variant divides into.
    fn new(buf_w: u16, buf_h: u16, floor_seed: u64) -> Self {
        let top_margin = top_margin(buf_h);
        let usable_h = buf_h - top_margin;

        let variant = FloorVariant::from_seed(floor_seed);
        let has_meeting = variant.has_meeting();
        let has_dual_meeting = variant == FloorVariant::Dense && usable_h >= MIN_DUAL_MEETING_H;
        let geom = FloorGeometry {
            variant,
            has_dual_meeting,
        };
        let has_pantry = geom.has_pantry();
        let mid_x = pct(buf_w, geom.mid_x_pct());

        const COUNTER_SIDE_CLEARANCE: u16 = 2;
        // Large counter + a routing clearance each side, else the compact fallback.
        // Width-only, so the size is known before the split prices the pantry against it.
        let pantry_counter_size: Size =
            if has_pantry && mid_x >= PANTRY_COUNTER_LARGE_W + 2 * COUNTER_SIDE_CLEARANCE {
                super::LARGE_COUNTER
            } else {
                super::COMPACT_COUNTER
            };

        // CONTENT-FIT, donating the surplus below ALL-OR-NOTHING: a partial donation would
        // cram the trio to its fit gate to buy rows the island still couldn't use.
        let trio_fit_h = MeetingRoom::trio_fit_h();
        let pantry_content_h = PantryRoom::content_fit_h(pantry_counter_size);
        let half_split = usable_h / 2;
        let donated = usable_h.saturating_sub(pantry_content_h);
        let meeting_h = if (trio_fit_h..half_split).contains(&donated) {
            donated
        } else {
            half_split
        };
        let mid_y_split = if has_meeting && !has_dual_meeting {
            top_margin + meeting_h
        } else {
            top_margin + half_split
        };

        // The second room only ever joins a first, so room 0 is always the top one.
        debug_assert!(!has_dual_meeting || has_meeting);
        let meeting_room = if has_meeting {
            debug_assert!(
                has_pantry || has_dual_meeting,
                "meeting implies pantry-or-dual per the variant table"
            );
            Some(Bounds {
                x: 0,
                y: top_margin,
                width: mid_x,
                height: mid_y_split - top_margin,
            })
        } else {
            None
        };
        let meeting_room_2 = if has_dual_meeting {
            Some(Bounds {
                x: 0,
                y: mid_y_split,
                width: mid_x,
                height: usable_h - usable_h / 2,
            })
        } else {
            None
        };
        let pantry_room = if has_pantry {
            Some(Bounds {
                x: 0,
                y: if has_meeting { mid_y_split } else { top_margin },
                width: mid_x,
                height: if has_meeting {
                    usable_h - (mid_y_split - top_margin)
                } else {
                    usable_h
                },
            })
        } else {
            None
        };

        let right_x = mid_x + MID_DIVIDER_W;
        let right_w = band_w(buf_w, geom.mid_x_pct());
        let lounge_west_clear = if has_meeting {
            mid_x + super::WALL_THICK_V
        } else {
            right_x
        };
        let cubicle_aisle_h = cubicle_aisle_h(usable_h);
        // NOT `usable_h - cubicle_aisle_h`, though the locals are right here — see
        // `band_h` for what its saturation buys below the gate.
        let cubicle_h = band_h(buf_h);
        let cubicle_band = Bounds {
            x: right_x,
            y: top_margin,
            width: right_w,
            height: cubicle_h,
        };
        let cubicle_aisle = Bounds {
            x: right_x,
            y: top_margin + cubicle_h,
            width: right_w,
            height: cubicle_aisle_h,
        };

        let pod_w = POD_SIDE * DESK_W + (POD_SIDE - 1) * INTRA_POD_GAP_X;
        let pod_h = POD_SIDE * DESK_H + (POD_SIDE - 1) * INTRA_POD_GAP_Y;
        let pod_stride_x = pod_w + INTER_POD_AISLE_X;
        let pod_stride_y = pod_h + INTER_POD_AISLE_Y;
        let couch_to_desk_extra = couch_to_desk_extra(buf_h);
        let pod_cols = ((right_w.saturating_sub(INTER_POD_AISLE_X / 2)) / pod_stride_x).max(1);
        let pod_rows = ((cubicle_h.saturating_sub(couch_to_desk_extra) + INTER_POD_AISLE_Y)
            / pod_stride_y)
            .max(1);
        let pod_grid = PodGrid {
            band: cubicle_band,
            cols: pod_cols,
            rows: pod_rows,
            stride_x: pod_stride_x,
            stride_y: pod_stride_y,
            couch_to_desk_extra,
        };

        // A room too small for its trio still occupies its slot with `trio: None`, so
        // bounds and furniture can't mis-join.
        let mut meeting_rooms: Vec<MeetingRoom> = Vec::new();
        for (room_idx, room) in [meeting_room, meeting_room_2].into_iter().enumerate() {
            let Some(mr) = room else { continue };
            let trio = room_fits_furniture(&mr).then(|| MeetingRoom::place_trio(mr, room_idx != 0));
            meeting_rooms.push(MeetingRoom { bounds: mr, trio });
        }
        Self {
            buf_w,
            buf_h,
            top_margin,
            usable_h,
            lounge_west_clear,
            cubicle_aisle,
            pod_grid,
            pantry: pantry_room,
            pantry_counter_size,
            meeting_rooms,
        }
    }

    /// Room 0, the top meeting room.
    fn first_meeting_room(&self) -> Option<&MeetingRoom> {
        self.meeting_rooms.first()
    }
}

// Every variant stands a side room west of the divider, so the whiteboard, which
// hangs by it, needs no gate of its own.
const _: () = {
    let mut i = 0;
    while i < FloorVariant::ALL.len() {
        let v = FloorVariant::ALL[i];
        assert!(v.has_meeting() || v.has_pantry_base());
        i += 1;
    }
};

pub(super) fn compute_with_seed(
    buf_w: u16,
    buf_h: u16,
    max_desks: Option<usize>,
    floor_seed: u64,
) -> Option<SceneLayout> {
    if buf_w < MIN_LAYOUT_W || buf_h < MIN_LAYOUT_H {
        return None;
    }

    let plan = FloorPlan::new(buf_w, buf_h, floor_seed);

    let pod_desks = compute_pod_desks(max_desks, plan.pod_grid);
    let desk_points: Vec<Point> = pod_desks.iter().map(|d| d.at).collect();

    let desk_art: Vec<(Point, Size)> = desk_fixtures(desk_points.iter().copied(), plan.buf_h)
        .chain(desk_chair_fixtures(&pod_desks))
        .map(|f| art_rect(f.visual))
        .collect();
    let pod_decor = compute_pod_decor(plan.pod_grid, floor_seed, &desk_art);

    // Dense's inter-meeting wall is deliberately solid (#557 door policy).
    let (room_walls, doorways) =
        super::rooms::walls::derive_room_walls(&plan.meeting_rooms, plan.pantry);

    // Elevator door — in the window wall's last slot; above the lounge gate so
    // that gate can check couch↔door clearance.
    let top_wall_h = super::wall_band_h(plan.top_margin);
    let door = Point {
        x: super::door_x(buf_w),
        // Its bottom row is the band's trim row, so it stands on the floor
        // line instead of floating mid-wall.
        y: super::wall_trim_row(top_wall_h) + 1 - ELEVATOR_H,
    };
    /// How far SOUTH of the floor line the elevator spawn sits, so a character entering
    /// stands on open floor, not on the wall apron the straddling wall decor stamps into.
    const DOOR_THRESHOLD_CLEARANCE_PX: u16 = 4;
    let door_threshold = Point {
        x: door.x + ELEVATOR_W / 2,
        y: plan.top_margin + DOOR_THRESHOLD_CLEARANCE_PX,
    };

    let Point {
        x: couch_x,
        y: couch_y,
    } = couch_pos(&plan.pod_grid.band, plan.top_margin, plan.lounge_west_clear);
    // #566: every lounge piece's padded ground ends west of the threshold column.
    let flanks = LoungeFlanks::of(couch_x);
    let lounge_fits = lounge_east_ground(Point {
        x: couch_x,
        y: couch_y,
    })
    .is_none_or(|east| east <= door_threshold.x);
    let lounge = lounge_fits.then(|| {
        place_lounge(
            Point {
                x: couch_x,
                y: couch_y,
            },
            door,
        )
    });

    let mut waypoints = compute_waypoints(
        &plan,
        &pod_decor,
        lounge.map(|l| l.couch_center),
        &desk_points,
    );

    const BAND_INSET: u16 = 4;
    // NOT the pantry (a plant + pad blocks the only bridge to the cubicle area), NOT the
    // cubicle top strip (the narrow wall-to-couch gap), NOT a meeting interior (seals the
    // door).
    let mut plant_candidates: Vec<PlantItem> = vec![
        PlantItem {
            kind: PlantKind::Flower,
            pos: Point {
                // Its pot clear of the divider, which a doorway may cut down to here.
                x: (plan.pod_grid.band.x + BAND_INSET).max(
                    plan.lounge_west_clear
                        + furniture_def(PlantKind::Flower.furniture()).visual.w / 2,
                ),
                // In the corridor beside its appliances: the band's bottom row is a desk
                // row on a packed floor.
                y: corridor_centre_y(
                    &plan.cubicle_aisle,
                    furniture_def(PlantKind::Flower.furniture()).visual.h,
                ),
            },
        },
        PlantItem {
            kind: PlantKind::Succulent,
            pos: Point {
                x: plan.pod_grid.band.x + plan.pod_grid.band.width.saturating_sub(BAND_INSET),
                y: corridor_centre_y(
                    &plan.cubicle_aisle,
                    furniture_def(PlantKind::Succulent.furniture()).visual.h,
                ),
            },
        },
    ]
    .into_iter()
    // West wall only — clear of the east-wall door and the central sofa/table column;
    // the size gate keeps plant + pad from squeezing the strip below routable width.
    .chain(
        plan.first_meeting_room()
            .map(|r| r.bounds)
            .into_iter()
            .flat_map(|mr| {
                const MIN_PLANTED_SIDE: u16 = 30;
                const WEST_INSET: u16 = 5;
                const TALL_NORTH_INSET: u16 = 6;
                const FLOWER_SOUTH_INSET: u16 = 7;
                if mr.width < MIN_PLANTED_SIDE || mr.height < MIN_PLANTED_SIDE {
                    Vec::new()
                } else {
                    vec![
                        PlantItem {
                            kind: PlantKind::Tall,
                            pos: Point {
                                x: mr.x + WEST_INSET,
                                y: mr.y + TALL_NORTH_INSET,
                            },
                        },
                        PlantItem {
                            kind: PlantKind::Flower,
                            pos: Point {
                                x: mr.x + WEST_INSET,
                                y: mr.y + mr.height.saturating_sub(FLOWER_SOUTH_INSET),
                            },
                        },
                    ]
                }
            }),
    )
    .collect();

    // Two Ficus spots — greeting plant west of the elevator, and the lounge's west flank.
    // On a narrower band each seals a top-strip pocket, hence the ROOMY gate.
    if plan.pod_grid.band.width >= ROOMY_BAND_MIN_W {
        const FICUS_WEST_OF_DOOR: u16 = 5;
        const FICUS_BELOW_TOP_MARGIN: u16 = 5;
        plant_candidates.push(PlantItem {
            kind: PlantKind::Ficus,
            pos: Point {
                x: door.x.saturating_sub(FICUS_WEST_OF_DOOR),
                y: plan.top_margin + FICUS_BELOW_TOP_MARGIN,
            },
        });
        if lounge.is_some() {
            // A plant's clearance west of the side table, and its ground east
            // of the divider, or no Ficus.
            let ficus = furniture_def(PlantKind::Ficus.furniture());
            let ficus_half_w = ficus.footprint.map_or(0, |f| f.w) / 2;
            let x = flanks
                .west()
                .saturating_sub(PLANT_OBSTACLE_CLEARANCE_PX + ficus.visual.w - ficus.visual.w / 2);
            if x >= plan.lounge_west_clear + ficus_half_w {
                plant_candidates.push(PlantItem {
                    kind: PlantKind::Ficus,
                    pos: Point { x, y: couch_y },
                });
            }
        }
    }

    let mut wall_decor = place_wall_decor(&plan, door, &desk_art, &pod_decor);

    // The island pushes its 4 slots BEFORE the shelf's — the push order the goldens pin.
    let kitchen_island = plan.pantry.and_then(|pr| {
        super::rooms::pantry::place_kitchen_island(pr, plan.pantry_counter_size, &mut waypoints)
    });
    if let Some(pr) = plan.pantry {
        super::rooms::pantry::place_snack_shelf(pr, plan.pantry_counter_size, &mut waypoints);
    }

    let corridor = Bounds {
        x: 0,
        y: plan.cubicle_aisle.y,
        width: buf_w,
        height: plan.cubicle_aisle.height,
    };

    // Settle only now — AFTER every waypoint exists; filtering at the candidate site
    // checked a subset of the final set.
    let singleton_rects = plant_obstacle_rects(
        lounge.and_then(|l| l.fish_tank),
        lounge.map(|l| l.floor_lamp),
        lounge.map(|l| l.side_table),
        kitchen_island,
        &plan.meeting_rooms,
    );
    // Folded, not mapped: the corridor's two pots slide toward each other, so each must
    // also clear the ones already placed.
    let mut plants: Vec<PlantItem> = Vec::new();
    for p in plant_candidates {
        let mut obstacles = singleton_rects.clone();
        obstacles.extend(plants.iter().map(|q| {
            let v = furniture_def(q.kind.furniture()).visual;
            (anchored_top_left(Pivot::Center, q.pos, v.w, v.h), v)
        }));
        if let Some(settled) = settle_plant(
            p,
            &desk_art,
            &waypoints,
            &obstacles,
            &plan.pod_grid.band,
            floor_seed,
        ) {
            plants.push(settled);
        }
    }

    let wall_pieces = rooms::walls::wall_pieces(&room_walls, &doorways, plan.top_margin);
    let build_mask = |plants: &[PlantItem], wall_decor: &[WallDecorItem]| {
        mask::build_walkable_mask(&mask::MaskObstacles {
            buf_w,
            buf_h,
            top_margin: plan.top_margin,
            home_desks: &desk_points,
            meeting_rooms: &plan.meeting_rooms,
            kitchen_island,
            waypoints: &waypoints,
            plants,
            floor_lamp: lounge.map(|l| l.floor_lamp),
            lounge_side_table: lounge.map(|l| l.side_table),
            fish_tank: lounge.and_then(|l| l.fish_tank),
            wall_decor,
            pod_decor: &pod_decor,
            wall_pieces: &wall_pieces,
            pantry_counter_size: plan.pantry_counter_size,
        })
    };
    // The door, where agents enter, so always in the main component.
    let conn_seed = door_threshold;

    // ROUTER granularity, not just the pixel flood's — a ≤3 px channel is
    // pixel-connected and coarse-IMPASSABLE (#566).
    // The router's reach of a connected `mask`; `None` while it is severed.
    let connected = |mask: &WalkableMask| -> Option<ReachSet> {
        if !unreachable_walkable_cells(mask, conn_seed).is_empty() {
            return None;
        }
        let reach = ReachSet::from_mask(mask, conn_seed);
        // South is where the demotion pass below retreats, so a reachable south seat
        // proves no decor arrangement strands a desk.
        let stranded = desk_points.iter().any(|&d| {
            let chair = desk_walk_anchor_facing(d, crate::layout::Facing::South);
            approach_point(
                Furniture::Desk,
                chair,
                Facing::South,
                plan.pantry_counter_size,
                mask,
                chair,
                &reach,
            ) == chair
        });
        (!stranded).then_some(reach)
    };

    let mut walkable = build_mask(&plants, &wall_decor);
    // Connectivity guard (#566): a decorative plant may NEVER disconnect the office. The
    // flood runs on EVERY compute — the check IS the guard, a net for ANY sealing decor.
    let mut reach = connected(&walkable);
    if reach.is_none() {
        // The pocket cells sit ACROSS the drain from the seal-causing plant, so target
        // by "settled into the aisle", not "borders the pocket".
        plants.retain(|p| !plant_ground_in_bounds(p, &plan.cubicle_aisle));
        walkable = build_mask(&plants, &wall_decor);
        reach = connected(&walkable);
        // Next rung — only a wall decor that TOUCHES THE FLOOR can seal a lane, so drop
        // those (by footprint, not by kind) before the drastic clear-all-plants.
        if reach.is_none() {
            wall_decor.retain(|d| !d.kind.stands_on_floor());
            walkable = build_mask(&plants, &wall_decor);
            reach = connected(&walkable);
        }
        if reach.is_none() {
            plants.clear();
            walkable = build_mask(&plants, &wall_decor);
            reach = connected(&walkable);
        }
    }
    let reachable = reach.expect(
        "#566 connectivity guard: a pocket (or a coarse-unroutable home desk) survived \
         dropping every scatter plant AND every floor-standing wall decor — a new NON-decor \
         seal cause needs its own fix",
    );

    // A narrow band can wall off a back-turned desk's SOUTH front — demote, don't drop.
    let home_desks: Vec<HomeDesk> = pod_desks
        .iter()
        .map(|&HomeDesk { at, facing }| {
            let facing = if facing == Facing::North && {
                // `approach_point` returns the probed cell itself as its "no side" sentinel.
                let chair = desk_walk_anchor_facing(at, facing);
                approach_point(
                    Furniture::Desk,
                    chair,
                    facing,
                    plan.pantry_counter_size,
                    &walkable,
                    chair,
                    &reachable,
                ) == chair
            } {
                Facing::South
            } else {
                facing
            };
            HomeDesk { at, facing }
        })
        .collect();

    Some(SceneLayout {
        buf_w,
        buf_h,
        cubicle_band: plan.pod_grid.band,
        cubicle_aisle: plan.cubicle_aisle,
        home_desks,
        waypoints,
        plants,
        wall_decor,
        pod_decor,
        lounge,
        door,
        door_threshold,
        meeting_rooms: plan.meeting_rooms,
        pantry: plan.pantry.map(|bounds| PantryRoom {
            bounds,
            counter_size: plan.pantry_counter_size,
            kitchen_island,
        }),
        room_walls,
        doorways,
        wall_pieces,
        top_margin: plan.top_margin,
        corridor,
        walkable,
        reachable,
    })
}

/// The wall-band decorations, each TOP-LEFT-anchored so its bottom row lands on the last
/// wall-band row however tall the band grows; the whiteboard stands on the floor instead. The screen hugs room 0's WEST
/// corner and the bookshelf spreads EAST — LOAD-BEARING, not taste: the carpet apron
/// between the two grounds must drain south AROUND the tucked sofa. Any item whose
/// clamped slot would pierce the divider/elevator drops, reopening the lane.
fn place_wall_decor(
    plan: &FloorPlan,
    door: Point,
    desk_art: &[(Point, Size)],
    pod_decor: &[PodDecorItem],
) -> Vec<WallDecorItem> {
    let FloorPlan {
        buf_w,
        top_margin,
        usable_h,
        pod_grid,
        ..
    } = *plan;
    let meeting_room = plan.first_meeting_room();
    let bookshelf = furniture_def(WallDecor::Bookshelf.furniture()).visual;
    let screen = furniture_def(WallDecor::MeetingScreen.furniture()).visual;
    // A room narrower than the screen would hang it ACROSS the east wall — dropping it
    // is the same degradation as the bare meeting room.
    let meeting_screen_x = meeting_room.and_then(|mr| {
        let sx = mr.bounds.x + 1;
        (sx + screen.w < mr.bounds.x + mr.bounds.width).then_some(sx)
    });
    let bookshelf_x = bookshelf_x(buf_w, screen.w, bookshelf.w, meeting_screen_x, meeting_room);
    // WEST of the vertical divider too: on narrow trio rooms the drain clamp can push it
    // onto the wall's top segment, where it pierces the glass. Dropping reopens the apron.
    let bookshelf_east_limit = meeting_room
        .map_or(u16::MAX, |mr| mr.bounds.x + mr.bounds.width)
        .min(door.x);
    let mut wall_decor = Vec::new();
    if bookshelf_x + bookshelf.w < bookshelf_east_limit {
        wall_decor.push(WallDecorItem {
            kind: WallDecor::Bookshelf,
            pos: Point {
                x: bookshelf_x,
                y: top_margin.saturating_sub(bookshelf.h),
            },
        });
    }
    wall_decor.extend(super::exit_sign_pos(door).map(|pos| WallDecorItem {
        kind: WallDecor::ExitSign,
        pos,
    }));
    const WHITEBOARD_HINT_DIVISOR: u16 = 3;
    // The hint is not a slot: unsnapped it drops the board on a desk row
    // or in the intra-pod gap, where the wheel strip plugs the pod's own west lane.
    let wb_def = furniture_def(WallDecor::Whiteboard.furniture());
    let hint = Point {
        x: plan.lounge_west_clear,
        y: top_margin + usable_h / WHITEBOARD_HINT_DIVISOR,
    };
    let pod_pieces: Vec<Fixture> = pod_decor_fixtures(pod_decor).collect();
    // A pod's own whiteboard in the same columns would read as the board's twin.
    let twin_columns: Vec<(u16, u16)> = pod_pieces
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                FixtureKind::Pod {
                    kind: PodDecor::Whiteboard,
                    ..
                }
            )
        })
        .map(|f| (f.visual.x, f.visual.x + f.visual.width))
        .collect();
    let band_east = pod_grid.band.x + pod_grid.band.width;
    // The westmost spot in the aisle east of the divider that hides no desk, chair,
    // cabinet or pod decor behind the board, or no board.
    let snapped = wb_def
        .ground_rect(Pivot::TopLeft, hint)
        .and_then(|(ground, size)| {
            let y = pod_grid
                .snap_inter_pod_ground_y(ground.y, size.h)?
                .saturating_sub(ground.y - hint.y);
            (hint.x..=band_east.saturating_sub(wb_def.visual.w))
                .map(|x| Point { x, y })
                .find(|&at| {
                    let east = at.x + wb_def.visual.w;
                    twin_columns.iter().all(|&(w, e)| e <= at.x || east <= w)
                        && pod_pieces
                            .iter()
                            .map(|f| art_rect(f.visual))
                            .chain(desk_art.iter().copied())
                            .all(|r| !super::placement::rects_overlap((at, wb_def.visual), r))
                })
        });
    if let Some(pos) = snapped {
        wall_decor.push(WallDecorItem {
            kind: WallDecor::Whiteboard,
            pos,
        });
    }
    if let (Some(_), Some(sx)) = (meeting_room, meeting_screen_x) {
        wall_decor.push(WallDecorItem {
            kind: WallDecor::MeetingScreen,
            pos: Point {
                x: sx,
                y: top_margin.saturating_sub(screen.h),
            },
        });
    }
    // A band too short to hang a piece under the neon sign drops it.
    wall_decor.retain(|d| {
        let v = furniture_def(d.kind.furniture()).visual;
        !NEON_PANEL.overlaps(Bounds {
            x: d.pos.x,
            y: d.pos.y,
            width: v.w,
            height: v.h,
        })
    });
    wall_decor
}

/// The bookshelf's wall slot — flush east of the meeting screen, spread further past
/// the sofa's drain edge, or back to flush when that spread would pierce the divider.
fn bookshelf_x(
    buf_w: u16,
    screen_w: u16,
    bookshelf_w: u16,
    meeting_screen_x: Option<u16>,
    meeting_room: Option<&MeetingRoom>,
) -> u16 {
    const BOOKSHELF_X_PCT: u16 = 18;
    let x = pct(buf_w, BOOKSHELF_X_PCT);
    match (meeting_screen_x, meeting_room) {
        (Some(sx), Some(mr)) => {
            // The ONE flush slot: screen east edge + both grounds' wall-decor pads, so the two
            // pads merge with no strandable apron cell. Every arm below derives from it.
            let flush_east = sx
                + screen_w
                + furniture_def(Furniture::MeetingScreen).pad
                + furniture_def(Furniture::Bookshelf).pad;
            // The drain term applies only where room 0 HOSTS its trio: with no sofa,
            // pushing the shelf east hangs it over a desk pod's pad, sealing the gap.
            if let Some(sofa_pad_east) = mr.sofa_east_drain_edge() {
                /// Past the sofa's drain edge by the shelf's own ground pad, then a ≥2-px
                /// walkable channel plus slack.
                const BOOKSHELF_DRAIN_GAP: u16 = furniture_def(Furniture::Bookshelf).pad + 4;
                let spread = x.max(flush_east).max(sofa_pad_east + BOOKSHELF_DRAIN_GAP);
                if spread + bookshelf_w < mr.bounds.x + mr.bounds.width {
                    spread
                } else {
                    // Falls back to FLUSH, NOT the `BOOKSHELF_X_PCT` anchor, which at these widths
                    // opens a strandable gap OVER the sofa pad.
                    flush_east
                }
            } else {
                x.max(flush_east)
            }
        }
        _ => x,
    }
}

/// The column past the farthest-east lounge piece's padded ground, for a couch at
/// `couch`.
fn lounge_east_ground(couch: Point) -> Option<u16> {
    couch_east_ground(couch.x, couch.y).max(LoungeFlanks::of(couch.x).east_ground())
}

/// The column past the couch's east seat's padded ground.
fn couch_east_ground(couch_x: u16, couch_y: u16) -> Option<u16> {
    furniture_def(Furniture::Couch).east_of_padded_ground(
        Pivot::Center,
        Point {
            x: couch_x + SEAT_DX[SEAT_DX.len() - 1] as u16,
            y: couch_y,
        },
    )
}

/// Clear columns between the lounge couch's drawn box and each flank.
const LOUNGE_FLANK_GAP: u16 = 1;

/// The columns the lounge couch's flanks stand on, [`LOUNGE_FLANK_GAP`] off its
/// drawn box: the box is wider than its seats, so a flank inside it paints
/// over an arm.
struct LoungeFlanks {
    side_table_x: u16,
    lamp_x: u16,
}

impl LoungeFlanks {
    fn of(couch_x: u16) -> Self {
        let couch_w = furniture_def(Furniture::MeetingSofaBody).visual.w;
        let couch_west = couch_x.saturating_sub(couch_w / 2);
        let side_w = furniture_def(Furniture::LoungeSideTable).visual.w;
        let lamp_w = furniture_def(Furniture::FloorLamp).visual.w;
        Self {
            side_table_x: couch_west.saturating_sub(LOUNGE_FLANK_GAP + side_w - side_w / 2),
            lamp_x: couch_west + couch_w + LOUNGE_FLANK_GAP + lamp_w / 2,
        }
    }

    /// Columns from the couch's centre west to the side table's west edge.
    fn west_reach() -> u16 {
        let couch_w = furniture_def(Furniture::MeetingSofaBody).visual.w;
        let side_w = furniture_def(Furniture::LoungeSideTable).visual.w;
        couch_w / 2 + LOUNGE_FLANK_GAP + side_w
    }

    /// The side table's west column.
    fn west(&self) -> u16 {
        self.side_table_x
            .saturating_sub(furniture_def(Furniture::LoungeSideTable).visual.w / 2)
    }

    /// The column past the floor lamp's padded ground.
    fn east_ground(&self) -> Option<u16> {
        // The east edge doesn't depend on y.
        let lamp = Point {
            x: self.lamp_x,
            y: 0,
        };
        furniture_def(Furniture::FloorLamp).east_of_padded_ground(Pivot::Center, lamp)
    }
}

/// The lounge vignette around the viewing couch — floor lamp, side table, aquarium.
/// The lamp sits just east so its halo bathes the seating area at night; the side table
/// takes the OPPOSITE (west) flank ([`LoungeFlanks`]). The
/// aquarium carries an EXTRA gate the other two don't: it must stay clear of the
/// elevator `door` column so the spawn threshold never routes around it.
fn place_lounge(couch: Point, door: Point) -> Lounge {
    /// Rows from the couch's centre down to the lamp's base: the art grows
    /// north from it (invariant #6).
    const LAMP_BASE_DY: u16 = 6;
    let flanks = LoungeFlanks::of(couch.x);
    let floor_lamp = Point {
        x: flanks.lamp_x,
        y: super::placement::centre_y_standing_on(
            couch.y + LAMP_BASE_DY,
            furniture_def(Furniture::FloorLamp).visual.h,
        ),
    };
    let side_table = Point {
        x: flanks.side_table_x,
        y: super::placement::centre_y_standing_on(
            super::placement::sort_row_at(
                Pivot::Center,
                couch,
                furniture_def(Furniture::MeetingSofaBody).visual.h,
            ),
            furniture_def(Furniture::LoungeSideTable).visual.h,
        ),
    };
    let fish_tank = {
        let def = furniture_def(Furniture::FishTank);
        let half_w = def.visual.w / 2;
        // One clear floor column of breathing room past the lamp shade's east edge; a
        // center-pinned east edge is (w-1)/2 past the anchor.
        const LAMP_TANK_GAP: u16 = 2;
        const TANK_ABOVE_COUCH: u16 = 4;
        let lamp_east = floor_lamp.x + (furniture_def(Furniture::FloorLamp).visual.w - 1) / 2;
        let cx = lamp_east + LAMP_TANK_GAP + half_w;
        (cx + half_w + FISH_TANK_ELEVATOR_CLEARANCE <= door.x).then_some(Point {
            x: cx,
            y: couch.y.saturating_sub(TANK_ABOVE_COUCH),
        })
    };
    Lounge {
        couch_center: couch,
        floor_lamp,
        side_table,
        fish_tank,
    }
}

/// THE non-waypoint obstacle census a scatter plant must clear — one derivation shared
/// by the production settle path and the placement-sweep backstop, so the two can't
/// drift. EVERY singleton is passed EXPLICITLY (omitting one shipped interpenetration
/// bugs) and included IFF its kind [`repels_plants`]; waypoint obstacles are
/// `first_blocking_waypoint`'s job.
pub(super) fn plant_obstacle_rects(
    fish_tank: Option<Point>,
    floor_lamp: Option<Point>,
    lounge_side_table: Option<Point>,
    kitchen_island: Option<Point>,
    meeting_rooms: &[MeetingRoom],
) -> Vec<(Point, Size)> {
    let boxed = |kind: Furniture, pos: Point| -> Option<(Point, Size)> {
        repels_plants(kind).then(|| {
            let v = furniture_def(kind).visual;
            (anchored_top_left(Pivot::Center, pos, v.w, v.h), v)
        })
    };
    [
        (Furniture::FishTank, fish_tank),
        (Furniture::FloorLamp, floor_lamp),
        (Furniture::LoungeSideTable, lounge_side_table),
        (Furniture::KitchenIsland, kitchen_island),
    ]
    .into_iter()
    .filter_map(|(kind, pos)| pos.and_then(|p| boxed(kind, p)))
    .chain(meeting_rooms.iter().flat_map(|room| {
        room.trio.iter().flat_map(|trio| {
            [
                (Furniture::MeetingSofaBody, trio.sofas[0]),
                (Furniture::MeetingSofaBody, trio.sofas[1]),
                (Furniture::MeetingTable, trio.table),
            ]
            .into_iter()
            .filter_map(|(kind, pos)| boxed(kind, pos))
        })
    }))
    .collect()
}

/// Settle a scatter-plant candidate: keep its authored spot when clear, else slide 1px
/// at a time toward the band's horizontal centre (bounded) until the desk-art and
/// clearance rules pass. SLIDING, not deleting — yield-by-deletion stripped the greenery.
fn settle_plant(
    p: PlantItem,
    desk_art: &[(Point, Size)],
    waypoints: &[Waypoint],
    singletons: &[(Point, Size)],
    band: &Bounds,
    floor_seed: u64,
) -> Option<PlantItem> {
    // Clears any single corner appliance without wandering out of the authored corner
    // region.
    const MAX_PLANT_NUDGE_PX: u16 = 12;
    /// How far into the nudge budget the seeded first try may reach — the budget and its
    /// inward direction are the ladder's own, so no new clearance rule and no new container.
    const PLANT_SCATTER_PX: u16 = 4;
    // The claim under test — a scattered pot only stands where a DISPLACED one
    // already could — holds only while the scatter fits inside the dodge budget.
    const _: () = assert!(PLANT_SCATTER_PX <= MAX_PLANT_NUDGE_PX);
    let dir: i16 = if p.pos.x < band.x + band.width / 2 {
        1
    } else {
        -1
    };
    let clear = |cand: Point| plant_spot_clear(p.kind, cand, desk_art, waypoints, singletons);
    let slide = |step: u16| Point {
        x: p.pos.x.saturating_add_signed(dir * step as i16),
        y: p.pos.y,
    };
    let seeded = pixtuoid_core::id::splitmix64(floor_seed ^ super::decor::point_seed(p.pos))
        % (u64::from(PLANT_SCATTER_PX) + 1);
    let first = slide(seeded as u16);
    if clear(first) {
        return Some(PlantItem {
            kind: p.kind,
            pos: first,
        });
    }
    // Beside the blocking obstacle, past its clearance.
    let pv = furniture_def(p.kind.furniture()).visual;
    if let Some(w) = first_blocking_waypoint(p.kind, p.pos, waypoints) {
        let wdef = furniture_def(w.kind.furniture());
        let m = PLANT_OBSTACLE_CLEARANCE_PX;
        // Derive the spot from the inflated box's EDGES: width-sum arithmetic
        // truncates w/2 on odd widths and landed 1px inside the box.
        let infl_left = w.pos.x.saturating_sub(wdef.visual.w / 2 + m);
        let infl_right = infl_left + wdef.visual.w + 2 * m;
        let cand_x = if dir < 0 {
            infl_left.saturating_sub(pv.w - pv.w / 2)
        } else {
            infl_right + pv.w / 2
        };
        let cand = Point {
            x: cand_x,
            y: p.pos.y,
        };
        if clear(cand) {
            return Some(PlantItem {
                kind: p.kind,
                pos: cand,
            });
        }
    }
    (0..=MAX_PLANT_NUDGE_PX).find_map(|step| {
        let cand = slide(step);
        clear(cand).then_some(PlantItem {
            kind: p.kind,
            pos: cand,
        })
    })
}

/// The first obstacle waypoint whose clearance box the plant's authored spot
/// violates — the thing `settle_plant` steps around.
fn first_blocking_waypoint(
    kind: PlantKind,
    pos: Point,
    waypoints: &[Waypoint],
) -> Option<&Waypoint> {
    let pv = furniture_def(kind.furniture()).visual;
    let plant_tl = anchored_top_left(Pivot::Center, pos, pv.w, pv.h);
    waypoints.iter().find(|w| {
        let wdef = furniture_def(w.kind.furniture());
        if wdef.footprint.is_none() {
            return false;
        }
        let wp_tl = anchored_top_left(Pivot::Center, w.pos, wdef.visual.w, wdef.visual.h);
        super::placement::overlaps_within_clearance(
            (plant_tl, pv),
            (wp_tl, wdef.visual),
            PLANT_OBSTACLE_CLEARANCE_PX,
        )
    })
}

/// Both placement rules for one plant spot: the sprite box never overlaps a
/// rect in `desk_art`, and keeps PLANT_OBSTACLE_CLEARANCE_PX of air from every
/// obstacle waypoint's box.
fn plant_spot_clear(
    kind: PlantKind,
    pos: Point,
    desk_art: &[(Point, Size)],
    waypoints: &[Waypoint],
    singletons: &[(Point, Size)],
) -> bool {
    let pv = furniture_def(kind.furniture()).visual;
    let plant_tl = anchored_top_left(Pivot::Center, pos, pv.w, pv.h);
    if desk_art
        .iter()
        .any(|&r| super::placement::rects_overlap((plant_tl, pv), r))
    {
        return false;
    }
    // Fixed singletons get the same inflated-clearance rule as waypoints.
    if singletons.iter().any(|&(tl, sz)| {
        super::placement::overlaps_within_clearance(
            (plant_tl, pv),
            (tl, sz),
            PLANT_OBSTACLE_CLEARANCE_PX,
        )
    }) {
        return false;
    }
    first_blocking_waypoint(kind, pos, waypoints).is_none()
}

/// Walkable cells NOT reachable from `seed` by 4-connected flood (a sealed
/// pocket). Empty when the office is one region OR when `seed` itself is blocked
/// — a bad seed can't judge connectivity, so the guard stays a no-op rather than
/// falsely condemning every cell.
pub(super) fn unreachable_walkable_cells(mask: &WalkableMask, seed: Point) -> Vec<Point> {
    let (w, h) = (mask.width(), mask.height());
    if !mask.is_walkable(seed.x, seed.y) {
        return Vec::new();
    }
    let idx = |x: u16, y: u16| y as usize * w as usize + x as usize;
    let mut seen = vec![false; w as usize * h as usize];
    let mut stack = vec![seed];
    seen[idx(seed.x, seed.y)] = true;
    while let Some(p) = stack.pop() {
        for (dx, dy) in [(0i32, -1i32), (0, 1), (-1, 0), (1, 0)] {
            let (nx, ny) = (i32::from(p.x) + dx, i32::from(p.y) + dy);
            if nx < 0 || ny < 0 || nx >= i32::from(w) || ny >= i32::from(h) {
                continue;
            }
            let (nx, ny) = (nx as u16, ny as u16);
            if !seen[idx(nx, ny)] && mask.is_walkable(nx, ny) {
                seen[idx(nx, ny)] = true;
                stack.push(Point { x: nx, y: ny });
            }
        }
    }
    (0..h)
        .flat_map(|y| (0..w).map(move |x| Point { x, y }))
        .filter(|p| mask.is_walkable(p.x, p.y) && !seen[idx(p.x, p.y)])
        .collect()
}

/// Does scatter plant `p`'s ground rect fall inside `b` (the cubicle aisle)? A
/// plant `settle_plant` relocated onto an obstacle's aisle row lands here and can
/// plug the drain — THE seal-causer selector for the #566 connectivity guard.
fn plant_ground_in_bounds(p: &PlantItem, b: &Bounds) -> bool {
    let def = furniture_def(p.kind.furniture());
    let Some(ground) = def.ground_rect(Pivot::Center, p.pos) else {
        return false;
    };
    super::placement::rects_overlap(
        ground,
        (
            Point { x: b.x, y: b.y },
            Size {
                w: b.width,
                h: b.height,
            },
        ),
    )
}

/// The [`POD_SIDE`]-square pod lattice over the cubicle band.
#[derive(Clone, Copy)]
pub(super) struct PodGrid {
    band: Bounds,
    cols: u16,
    rows: u16,
    stride_x: u16,
    stride_y: u16,
    couch_to_desk_extra: u16,
}

impl PodGrid {
    /// NW origin (top-left of the first desk) of pod `(pod_c, pod_r)` within the
    /// cubicle band.
    fn pod_origin(self, pod_c: u16, pod_r: u16) -> (u16, u16) {
        let x = self.band.x + INTER_POD_AISLE_X / 2 + pod_c * self.stride_x;
        let y =
            self.band.y + INTER_POD_AISLE_Y / 2 + self.couch_to_desk_extra + pod_r * self.stride_y;
        (x, y)
    }

    /// The full-width y-bands BETWEEN consecutive pod rows, north-to-south — the only
    /// floor a free-standing piece may stand on. NOT "every pod-free strip": the north
    /// margin and south remainder hold the lounge and the door's approach.
    fn inter_pod_y_bands(self) -> Vec<(u16, u16)> {
        // NOT `stride_y - INTER_POD_AISLE_Y`: that prices the SLOT, and a desk's blocked
        // ground runs to `DESK_GROUND_H` below its corner, so the overhang rows look free.
        let pod_h = (POD_SIDE - 1) * (DESK_H + INTRA_POD_GAP_Y) + DESK_GROUND_H;
        (0..self.rows.saturating_sub(1))
            .map(|pod_r| {
                let (_, top) = self.pod_origin(0, pod_r);
                (top + pod_h, top + self.stride_y)
            })
            .filter(|&(start, end)| end > start)
            .collect()
    }

    /// Top row for a ground strip `h` px tall, centred in the inter-pod aisle nearest
    /// `desired` — `None` when no aisle can hold it. Centred, not flush: flush against a
    /// pod the strip piles all its clearance on one side and can seal the lane.
    fn snap_inter_pod_ground_y(self, desired: u16, h: u16) -> Option<u16> {
        let distance_to = |(start, end): (u16, u16)| (start + (end - start) / 2).abs_diff(desired);
        let (start, end) = self
            .inter_pod_y_bands()
            .into_iter()
            .filter(|&(start, end)| end - start >= h)
            .min_by_key(|&band| distance_to(band))?;
        Some(start + (end - start - h) / 2)
    }
}

/// The hand-authored floor geometries. `floor_seed` selects one via
/// [`Self::from_seed`]; floors past [`Self::COUNT`] repeat the same looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FloorVariant {
    /// Meeting + pantry, vertical wall between them and the cubicle area,
    /// horizontal wall between meeting/pantry.
    Standard,
    /// Pantry only, no vertical wall (open kitchen corner, the counter is the
    /// divider). No meeting room.
    OpenPlan,
    /// Two meeting rooms (top + bottom), no pantry — a horizontal wall separates
    /// them, each gets a door. Degrades to Standard when too short for two rooms.
    Dense,
    /// Larger meeting + pantry (like Standard but a wider left column).
    Senior,
    /// Pantry only, no vertical wall (open break area).
    Lounge,
}

impl FloorVariant {
    /// THE roster: the floor derivations sweep it, `from_seed` indexes it, `COUNT` is
    /// its length. A variant missing here is unreachable — rustc's `dead_code` reds on
    /// the never-constructed arm, NOT `the_sweep_reaches_every_floor_variant`, which
    /// catches the other direction. `has_meeting` / `has_pantry_base` are `matches!`
    /// lists; the `const _` above `compute_with_seed` reds a variant in neither.
    pub(super) const ALL: [Self; 5] = [
        FloorVariant::Standard,
        FloorVariant::OpenPlan,
        FloorVariant::Dense,
        FloorVariant::Senior,
        FloorVariant::Lounge,
    ];
    const COUNT: u64 = Self::ALL.len() as u64;
    /// Searched, not Fibonacci-derived: it maps floors `0..COUNT` to distinct
    /// variants, which `floor_variant_hash_gives_unique_layouts_per_floor` pins.
    const HASH_MULT: u64 = 0x4737_8190_96da_1dad;

    fn from_seed(floor_seed: u64) -> Self {
        Self::ALL[(floor_seed.wrapping_mul(Self::HASH_MULT) % Self::COUNT) as usize]
    }

    /// Whether this variant encloses a meeting room (== the vertical-wall presence).
    const fn has_meeting(self) -> bool {
        matches!(
            self,
            FloorVariant::Standard | FloorVariant::Dense | FloorVariant::Senior
        )
    }

    /// Pantry presence BEFORE the Dense-degrade fixup (Dense has none until it
    /// degrades on a short terminal).
    const fn has_pantry_base(self) -> bool {
        matches!(
            self,
            FloorVariant::Standard
                | FloorVariant::OpenPlan
                | FloorVariant::Senior
                | FloorVariant::Lounge
        )
    }

    /// Left-column split as a percent of buffer width, BEFORE the Dense-degrade.
    const fn mid_x_pct(self) -> u16 {
        match self {
            FloorVariant::Standard => 28,
            FloorVariant::OpenPlan => 18,
            FloorVariant::Dense => 22,
            FloorVariant::Senior => 35,
            FloorVariant::Lounge => 22,
        }
    }
}

/// The resolved floor geometry: the `variant` plus the ONE size-dependent bit,
/// `has_dual_meeting` (a Dense floor tall enough for two meeting rooms). The
/// `has_pantry` / `mid_x_pct` accessors fold in the Dense-degrade (a too-short
/// Dense floor gains a pantry and widens to the Standard column).
#[derive(Clone, Copy)]
pub(super) struct FloorGeometry {
    variant: FloorVariant,
    has_dual_meeting: bool,
}

impl FloorGeometry {
    /// Resolved pantry presence AFTER the Dense-degrade: a Dense floor too short
    /// for two rooms gains a pantry (Standard geometry).
    fn has_pantry(self) -> bool {
        if self.variant == FloorVariant::Dense && !self.has_dual_meeting {
            true
        } else {
            self.variant.has_pantry_base()
        }
    }
    /// Resolved mid-column percent AFTER the Dense-degrade — reads the Standard
    /// row rather than repeating its percent, so retuning that row can't leave a
    /// degraded Dense floor on the old column.
    fn mid_x_pct(self) -> u16 {
        if self.variant == FloorVariant::Dense && !self.has_dual_meeting {
            FloorVariant::Standard.mid_x_pct()
        } else {
            self.variant.mid_x_pct()
        }
    }
}

/// Which way a desk on pod row `r` seats its occupant — a pod's two rows face EACH
/// OTHER across the inner gap. A partial bottom row is the next pod's row 0.
fn pod_row_facing(r: u16) -> Facing {
    if r == 0 { Facing::South } else { Facing::North }
}

/// Pod-grid desk placement: full pods, partial columns at right edge,
/// partial row at bottom edge.
pub(super) fn compute_pod_desks(max_desks: Option<usize>, grid: PodGrid) -> Vec<HomeDesk> {
    let cubicle_band = &grid.band;
    let PodGrid {
        cols: pod_cols,
        rows: pod_rows,
        ..
    } = grid;
    // The hint is bounded by physical capacity: `Vec::with_capacity(usize::MAX)` aborts.
    let n = max_desks.unwrap_or(usize::MAX);
    let grid_desk_cap =
        (pod_cols as usize) * (pod_rows as usize) * (POD_SIDE as usize) * (POD_SIDE as usize);
    let mut home_desks = Vec::with_capacity(n.min(grid_desk_cap.max(1)));
    // Ground, not slot, on BOTH axes: a walk-behind desk's blocked ground runs
    // DESK_GROUND_H/W past its Point, and the slot dims let it spill past the band.
    let desk_y_max =
        (cubicle_band.y + cubicle_band.height).saturating_sub(super::decor::DESK_GROUND_H);
    // `pod_cols` floors at 1, so a narrow band's forced pod puts its 2nd desk column
    // past the edge — an invisible desk whose walk anchor sits outside the mask.
    let desk_x_max =
        (cubicle_band.x + cubicle_band.width).saturating_sub(super::decor::DESK_GROUND_W);
    let push_desk = |desks: &mut Vec<HomeDesk>, x: u16, y: u16, facing: Facing| -> bool {
        if desks.len() >= n || y > desk_y_max || x > desk_x_max {
            return desks.len() >= n;
        }
        desks.push(HomeDesk {
            at: Point { x, y },
            facing,
        });
        false
    };

    'outer: for pod_r in 0..pod_rows {
        for pod_c in 0..pod_cols {
            let (pod_origin_x, pod_origin_y) = grid.pod_origin(pod_c, pod_r);
            for r in 0..POD_SIDE {
                for c in 0..POD_SIDE {
                    let full = push_desk(
                        &mut home_desks,
                        pod_origin_x + c * (DESK_W + INTRA_POD_GAP_X),
                        pod_origin_y + r * (DESK_H + INTRA_POD_GAP_Y),
                        pod_row_facing(r),
                    );
                    if full {
                        break 'outer;
                    }
                }
            }
        }
    }

    // Partial right-edge columns CONTINUE the pod lattice, so spacing never jumps.
    let partial_col_x = |i: u16| -> u16 {
        let (x, _) = grid.pod_origin(pod_cols + i / POD_SIDE, 0);
        x + (i % POD_SIDE) * (DESK_W + INTRA_POD_GAP_X)
    };
    // POD_SIDE: a further column would need a residual wider than the stride already consumed.
    let partial_col_count = (0..POD_SIDE)
        .take_while(|&i| partial_col_x(i) <= desk_x_max)
        .count() as u16;
    let partial_col_at_right = partial_col_count > 0;
    if partial_col_at_right {
        'partial_x: for pod_r in 0..pod_rows {
            let (_, pod_origin_y) = grid.pod_origin(0, pod_r);
            for r in 0..POD_SIDE {
                for i in 0..partial_col_count {
                    let full = push_desk(
                        &mut home_desks,
                        partial_col_x(i),
                        pod_origin_y + r * (DESK_H + INTRA_POD_GAP_Y),
                        pod_row_facing(r),
                    );
                    if full {
                        break 'partial_x;
                    }
                }
            }
        }
    }

    // The Y twin: the row IS the first row of the (pod_rows)-th pod, so the rhythm holds.
    let (_, partial_y) = grid.pod_origin(0, pod_rows);
    let partial_row_at_bottom = partial_y <= desk_y_max;
    if partial_row_at_bottom {
        'partial_y: for pod_c in 0..pod_cols {
            let (pod_origin_x, _) = grid.pod_origin(pod_c, 0);
            for c in 0..POD_SIDE {
                let full = push_desk(
                    &mut home_desks,
                    pod_origin_x + c * (DESK_W + INTRA_POD_GAP_X),
                    partial_y,
                    pod_row_facing(0),
                );
                if full {
                    break 'partial_y;
                }
            }
        }
        for i in 0..partial_col_count {
            let full = push_desk(
                &mut home_desks,
                partial_col_x(i),
                partial_y,
                pod_row_facing(0),
            );
            if full {
                break;
            }
        }
    }

    home_desks
}

/// The kind for aisle slot `slot_idx`. Each pass deals a FRESH permutation of
/// `PodDecor::ALL` — a fixed cyclic order lets one slot's kind tell you the next —
/// rotated past a collision with the previous pass's last kind, because two identical
/// pieces in neighbouring aisles is the failure a user sees first. Path-dependent, so
/// passes are re-walked, not cached; pinned by `no_two_adjacent_aisle_slots_share_a_kind`.
pub(super) fn decor_for_slot(floor_seed: u64, slot_idx: usize) -> PodDecor {
    // Below two kinds the adjacency rule is unhonourable and `rotate_left(1)` a no-op.
    const _: () = assert!(PodDecor::ALL.len() >= 2);
    // `a_wide_floors_decor_order_is_not_one_fixed_cycle` needs more distinct arrangements
    // than the roster has kinds, and can only observe `MAX_FLOORS` of them.
    const _: () = assert!(PodDecor::ALL.len() < pixtuoid_core::state::MAX_FLOORS);
    let n = PodDecor::ALL.len();
    let shuffled = |pass: u64| {
        let mut bag = PodDecor::ALL.to_vec();
        let mut z = floor_seed ^ pass;
        for i in (1..n).rev() {
            z = pixtuoid_core::id::splitmix64(z);
            bag.swap(i, (z % (i as u64 + 1)) as usize);
        }
        bag
    };
    let mut bag = shuffled(0);
    for pass in 1..=(slot_idx / n) as u64 {
        let prev_tail = bag[n - 1];
        bag = shuffled(pass);
        if bag[0] == prev_tail {
            bag.rotate_left(1);
        }
    }
    bag[slot_idx % n]
}

/// Rows from an aisle slot's centre down to the row every piece in it stands
/// on, so a taller piece grows north (invariant #6) rather than out of its aisle.
const POD_DECOR_BASE_DY: u16 = 4;

/// `b` as the `(top-left, size)` rect [`super::placement::rects_overlap`] takes.
fn art_rect(b: Bounds) -> (Point, Size) {
    (
        Point { x: b.x, y: b.y },
        Size {
            w: b.width,
            h: b.height,
        },
    )
}

/// Decor items placed in aisles between desk pods, clear of every rect in `desk_art`.
pub(super) fn compute_pod_decor(
    grid: PodGrid,
    floor_seed: u64,
    desk_art: &[(Point, Size)],
) -> Vec<PodDecorItem> {
    let cubicle_band = &grid.band;
    let PodGrid {
        cols: pod_cols,
        rows: pod_rows,
        stride_x: pod_stride_x,
        stride_y: pod_stride_y,
        ..
    } = grid;
    let pod_w = pod_stride_x - INTER_POD_AISLE_X;
    let pod_h = pod_stride_y - INTER_POD_AISLE_Y;
    let mut pod_decor: Vec<PodDecorItem> = Vec::new();
    let mut slot_idx: usize = 0;
    // Mirror of push_desk's x clamp: a slot centre past the band edge would promote a
    // PhoneBooth/StandingDesk to a waypoint, sending idle agents to invisible furniture.
    let band_right = cubicle_band.x + cubicle_band.width;
    // Vertical twin: the LAST POD ROW's slot centre can sit close enough to the bottom
    // that a tall centred visual crosses into cubicle_aisle and blocks its cells.
    let band_bottom = cubicle_band.y + cubicle_band.height;
    let mut push_slot = |pod_decor: &mut Vec<PodDecorItem>, x: u16, slot_y: u16, across: Point| {
        let kind = decor_for_slot(floor_seed, slot_idx);
        // The cycle advances even when the slot drops, so survivors keep the kinds
        // they'd have on a wider floor.
        slot_idx += 1;
        let vis = furniture_def(kind.furniture()).visual;
        let centre = Point {
            x,
            y: super::placement::centre_y_standing_on(slot_y + POD_DECOR_BASE_DY, vis.h),
        };
        // Slides by `across` steps over its aisle, never along it into a crossing, to the
        // nearest spot the desks' art leaves room for; art wider than the aisle drops it.
        let reach = across.x * INTER_POD_AISLE_X / 2 + across.y * INTER_POD_AISLE_Y / 2;
        let Some(Point { x, y }) = std::iter::once(0)
            .chain((1..=reach as i16).flat_map(|d| [d, -d]))
            .map(|d| Point {
                x: centre.x.saturating_add_signed(d * across.x as i16),
                y: centre.y.saturating_add_signed(d * across.y as i16),
            })
            .find(|&at| {
                let art = (anchored_top_left(Pivot::Center, at, vis.w, vis.h), vis);
                desk_art
                    .iter()
                    .all(|&r| !super::placement::rects_overlap(art, r))
            })
        else {
            return;
        };
        // Same centred-blit math the painter uses (pos − h/2 .. pos − h/2 + h).
        if x.saturating_sub(vis.w / 2) + vis.w > band_right
            || y.saturating_sub(vis.h / 2) + vis.h > band_bottom
        {
            return;
        }
        pod_decor.push(PodDecorItem {
            kind,
            pos: Point { x, y },
        });
    };
    for pod_r in 0..pod_rows {
        for pod_c in 0..pod_cols.saturating_sub(1) {
            let (pod_origin_x, pod_origin_y) = grid.pod_origin(pod_c, pod_r);
            let aisle_cx = pod_origin_x + pod_w + INTER_POD_AISLE_X / 2;
            let aisle_cy = pod_origin_y + pod_h / 2;
            push_slot(&mut pod_decor, aisle_cx, aisle_cy, Point { x: 1, y: 0 });
        }
    }
    for pod_r in 0..pod_rows.saturating_sub(1) {
        for pod_c in 0..pod_cols {
            let (pod_origin_x, pod_origin_y) = grid.pod_origin(pod_c, pod_r);
            let aisle_cx = pod_origin_x + pod_w / 2;
            let aisle_cy = pod_origin_y + pod_h + INTER_POD_AISLE_Y / 2;
            push_slot(&mut pod_decor, aisle_cx, aisle_cy, Point { x: 0, y: 1 });
        }
    }
    pod_decor
}

/// Whether a corridor appliance centred at `pos` keeps its art off every home
/// desk's workstation — the desk and its filing cabinet, and its sitter, since
/// a south-row sitter hangs into the aisle, over the art's top. Both facings,
/// since a narrow band demotes a back-turned desk after this runs.
fn clears_the_workstations(kind: Furniture, pos: Point, home_desks: &[Point], buf_h: u16) -> bool {
    let art = corridor_art(kind, pos);
    super::roster::desk_fixtures(home_desks.iter().copied(), buf_h).all(|f| !f.visual.overlaps(art))
        && home_desks.iter().all(|&desk| {
            [Facing::North, Facing::South].into_iter().all(|facing| {
                let foot = desk_walk_anchor_facing(desk, facing);
                let sitter = Bounds {
                    x: foot.x.saturating_sub(CHARACTER_SPRITE_W / 2),
                    y: foot.y.saturating_sub(WALKING_Y_OFF),
                    width: CHARACTER_SPRITE_W,
                    height: CHARACTER_SPRITE_H,
                };
                !art.overlaps(sitter)
            })
        })
}

/// The art box of a corridor appliance of `kind` centred at `pos`.
fn corridor_art(kind: Furniture, pos: Point) -> Bounds {
    let art = furniture_def(kind).visual;
    let at = anchored_top_left(Pivot::Center, pos, art.w, art.h);
    Bounds {
        x: at.x,
        y: at.y,
        width: art.w,
        height: art.h,
    }
}

/// Where the corridor's vending machine stands: slid east from the band's
/// `west` edge, a pod's `stride` at most, off a workstation standing over it;
/// `None` in an aisle too small for it, so nothing keeps off a machine that
/// isn't there.
fn vending_spot(
    aisle: &Bounds,
    west: u16,
    stride: u16,
    home_desks: &[Point],
    buf_h: u16,
) -> Option<Point> {
    let fits = aisle.height >= VENDING_MIN_AISLE_H && aisle.width > VENDING_MIN_AISLE_W;
    let art = furniture_def(Furniture::VendingMachine).visual;
    (0..=stride)
        .filter(|_| fits)
        .map(|dx| Point {
            x: west + VENDING_WEST_GAP + art.w / 2 + dx,
            y: corridor_centre_y(aisle, art.h),
        })
        .find(|&p| clears_the_workstations(Furniture::VendingMachine, p, home_desks, buf_h))
}

pub(super) const VENDING_MIN_AISLE_H: u16 = 10;
pub(super) const VENDING_MIN_AISLE_W: u16 = 30;
pub(super) const PRINTER_MIN_AISLE_H: u16 = 9;
pub(super) const PRINTER_MIN_AISLE_W: u16 = 40;
/// Columns from the band's west edge to the vending machine's, clear of a
/// vertical wall's foot there.
const VENDING_WEST_GAP: u16 = 3;

/// The centre `y` of a corridor piece `h` tall: its base one row off the aisle's
/// south edge, its art overhanging north (invariant #6).
fn corridor_centre_y(aisle: &Bounds, h: u16) -> u16 {
    super::placement::centre_y_standing_on((aisle.y + aisle.height).saturating_sub(2), h)
}

/// Waypoints: couch, pantry, pod-decor-promoted (PhoneBooth/StandingDesk), corridor
/// appliances (VendingMachine/Printer).
fn compute_waypoints(
    plan: &FloorPlan,
    pod_decor: &[PodDecorItem],
    couch: Option<Point>,
    home_desks: &[Point],
) -> Vec<Waypoint> {
    let FloorPlan {
        buf_h,
        pantry: pantry_room,
        pantry_counter_size,
        ref pod_grid,
        ref cubicle_aisle,
        ref meeting_rooms,
        ..
    } = *plan;
    let right_x = pod_grid.band.x;
    let right_w = pod_grid.band.width;
    // room_id stays None: lounge grouping is keyed at the chitchat venue layer, not here.
    let mut waypoints: Vec<Waypoint> = couch
        .into_iter()
        .flat_map(|couch| {
            SEAT_DX.into_iter().map(move |dx| Waypoint {
                pos: Point {
                    x: couch.x.saturating_add_signed(dx),
                    y: couch.y,
                },
                kind: WaypointKind::Couch,
                // SEATED facing: the sitter looks NORTH at the window.
                facing: Facing::North,
                room_id: None,
            })
        })
        .collect();
    if let Some(pos) =
        pantry_room.and_then(|pr| PantryRoom::counter_center(pr, pantry_counter_size))
    {
        waypoints.push(Waypoint {
            pos,
            kind: WaypointKind::Pantry,
            facing: Facing::South,
            room_id: None,
        });
    }
    for &PodDecorItem { kind, pos } in pod_decor {
        if let Some(wp_kind) = kind.waypoint() {
            waypoints.push(Waypoint {
                pos,
                kind: wp_kind,
                facing: Facing::South,
                room_id: None,
            });
        }
    }

    let vending = vending_spot(cubicle_aisle, right_x, pod_grid.stride_x, home_desks, buf_h);
    if let Some(vending) = vending {
        waypoints.push(Waypoint {
            pos: vending,
            kind: WaypointKind::VendingMachine,
            facing: Facing::South,
            room_id: None,
        });
    }
    const PRINTER_CORNER_INSET: u16 = 10;
    // Slid west from its corner, a pod's stride at most, into the gap between
    // two pods' seats when a south-row sitter stands over it, and off the
    // vending machine's columns.
    let printer = (0..=pod_grid.stride_x)
        .map(|dx| Point {
            x: (right_x + right_w).saturating_sub(PRINTER_CORNER_INSET + dx),
            y: corridor_centre_y(cubicle_aisle, furniture_def(Furniture::Printer).visual.h),
        })
        .find(|&p| {
            clears_the_workstations(Furniture::Printer, p, home_desks, buf_h)
                && vending.is_none_or(|v| {
                    !corridor_art(Furniture::Printer, p)
                        .shares_columns(corridor_art(Furniture::VendingMachine, v))
                })
        });
    if let Some(printer) = printer
        && cubicle_aisle.height >= PRINTER_MIN_AISLE_H
        && cubicle_aisle.width > PRINTER_MIN_AISLE_W
    {
        waypoints.push(Waypoint {
            pos: printer,
            kind: WaypointKind::Printer,
            facing: Facing::South,
            room_id: None,
        });
    }

    // `room_id` is the room's TRUE index — a trio-less room keeps its slot, so it never shifts.
    for (room_id, room) in meeting_rooms.iter().enumerate() {
        let Some(trio) = room.trio else { continue };
        let table = trio.table;
        for sofa in trio.sofas {
            let facing = if sofa.y < table.y {
                Facing::South
            } else {
                Facing::North
            };
            for dx in SEAT_DX {
                waypoints.push(Waypoint {
                    pos: Point {
                        x: sofa.x.saturating_add_signed(dx),
                        y: sofa.y,
                    },
                    kind: WaypointKind::MeetingSofa,
                    facing,
                    room_id: Some(room_id),
                });
            }
        }
        // The offsets must MIRROR: the table blocks a span symmetric about t.x, and an asymmetric
        // pair puts one chair closer to the wood, swallowing the rug border its twin shows.
        let chair_dx = super::rooms::meeting::MEETING_CHAIR_TABLE_DX as i16;
        for (dx, facing) in [(-chair_dx, Facing::East), (chair_dx, Facing::West)] {
            waypoints.push(Waypoint {
                pos: Point {
                    x: table.x.saturating_add_signed(dx),
                    y: table.y,
                },
                kind: WaypointKind::MeetingChair,
                facing,
                room_id: Some(room_id),
            });
        }
    }

    debug_assert!(
        waypoints.iter().all(|w| {
            matches!(
                w.kind,
                WaypointKind::MeetingSofa | WaypointKind::MeetingChair
            ) == w.room_id.is_some()
        }),
        "room_id must be Some exactly for meeting-slot waypoints"
    );

    waypoints
}

#[cfg(test)]
mod tests {
    use super::{FloorGeometry, FloorVariant};

    /// The lounge is kept exactly while its padded ground ends at or west of the
    /// door threshold's column: the scan finds the width where the east edge meets
    /// the column, which is kept, and the width below it, which is dropped.
    #[test]
    fn the_lounge_is_kept_iff_its_east_ground_reaches_no_further_than_the_threshold() {
        use super::{FloorPlan, couch_pos, lounge_east_ground};
        use crate::layout::SceneLayout;
        use crate::layout::placement_sweep::{NARROW_BAND, SWEEP_SEEDS};
        let h = 160;
        for seed in SWEEP_SEEDS {
            let scan: Vec<(u16, bool, Option<u16>, u16)> = NARROW_BAND
                .map(|w| {
                    let l = SceneLayout::compute_with_seed(w, h, None, seed).expect("lays out");
                    let plan = FloorPlan::new(w, h, seed);
                    let couch =
                        couch_pos(&plan.pod_grid.band, plan.top_margin, plan.lounge_west_clear);
                    (
                        w,
                        l.lounge.is_some(),
                        lounge_east_ground(couch),
                        l.door_threshold.x,
                    )
                })
                .collect();
            for &(w, kept, east, threshold) in &scan {
                assert_eq!(
                    kept,
                    east.is_none_or(|e| e <= threshold),
                    "{w}x{h} seed {seed}: kept {kept}, east ground {east:?}, threshold {threshold}"
                );
            }
            let exact = scan
                .iter()
                .find(|&&(_, kept, east, threshold)| kept && east == Some(threshold))
                .unwrap_or_else(|| panic!("seed {seed}: no width meets the threshold exactly"));
            let below = scan
                .iter()
                .find(|&&(w, ..)| w + 1 == exact.0)
                .unwrap_or_else(|| {
                    panic!(
                        "seed {seed}: the width below {} falls outside the band",
                        exact.0
                    )
                });
            assert!(
                !below.1 && below.2.is_some_and(|e| e > below.3),
                "seed {seed}: the width below the exact fit must drop the lounge for \
                 crossing the threshold: {below:?}"
            );
        }
    }

    /// The couch↔door gate reads the column past the lamp's ground AS THE MASK
    /// STAMPS IT, so it drops the lounge exactly when the lamp would block the
    /// threshold.
    #[test]
    fn the_lamps_east_ground_is_the_column_past_its_stamped_ground() {
        use super::LoungeFlanks;
        use crate::layout::mask::{MaskObstacles, build_walkable_mask, first_walkable_east};
        let flanks = LoungeFlanks::of(60);
        let lamp = crate::layout::Point {
            x: flanks.lamp_x,
            y: 50,
        };
        let mask = build_walkable_mask(&MaskObstacles {
            top_margin: 20,
            floor_lamp: Some(lamp),
            ..MaskObstacles::bare(160, 96)
        });
        assert_eq!(
            flanks.east_ground(),
            Some(first_walkable_east(&mask, lamp.x, lamp.y))
        );
    }

    /// The same pin for the east seat: the gate's column is the first one the
    /// mask leaves walkable past the couch's seats.
    #[test]
    fn the_couchs_east_ground_is_the_column_past_its_stamped_seats() {
        use super::{SEAT_DX, couch_east_ground};
        use crate::layout::mask::{MaskObstacles, build_walkable_mask, first_walkable_east};
        use crate::layout::{Facing, Point, Waypoint, WaypointKind};
        let (couch_x, couch_y) = (60u16, 50u16);
        let seats: Vec<Waypoint> = SEAT_DX
            .into_iter()
            .map(|dx| Waypoint {
                pos: Point {
                    x: couch_x.saturating_add_signed(dx),
                    y: couch_y,
                },
                kind: WaypointKind::Couch,
                facing: Facing::North,
                room_id: None,
            })
            .collect();
        let mask = build_walkable_mask(&MaskObstacles {
            top_margin: 20,
            waypoints: &seats,
            ..MaskObstacles::bare(160, 96)
        });
        assert_eq!(
            couch_east_ground(couch_x, couch_y),
            Some(first_walkable_east(
                &mask,
                seats[seats.len() - 1].pos.x,
                couch_y
            ))
        );
    }

    /// An aisle too short or too narrow for a vending machine has none, so the
    /// printer keeps off nothing there.
    #[test]
    fn a_vending_machine_stands_only_where_its_aisle_fits() {
        use super::{VENDING_MIN_AISLE_H, VENDING_MIN_AISLE_W, vending_spot};
        use crate::layout::Bounds;
        let aisle = |height, width| Bounds {
            x: 0,
            y: 100,
            width,
            height,
        };
        let wide = VENDING_MIN_AISLE_W + 1;
        let spot = |a| vending_spot(&a, 0, 0, &[], u16::MAX);
        assert!(spot(aisle(VENDING_MIN_AISLE_H, wide)).is_some());
        assert_eq!(spot(aisle(VENDING_MIN_AISLE_H - 1, wide)), None);
        assert_eq!(spot(aisle(VENDING_MIN_AISLE_H, VENDING_MIN_AISLE_W)), None);
    }

    #[test]
    fn a_degraded_dense_floor_reads_the_standard_column_percent() {
        let degraded = FloorGeometry {
            variant: FloorVariant::Dense,
            has_dual_meeting: false,
        };
        assert_eq!(
            degraded.mid_x_pct(),
            FloorVariant::Standard.mid_x_pct(),
            "a too-short Dense floor degrades to the Standard geometry"
        );
        let dual = FloorGeometry {
            variant: FloorVariant::Dense,
            has_dual_meeting: true,
        };
        assert_eq!(
            dual.mid_x_pct(),
            FloorVariant::Dense.mid_x_pct(),
            "a Dense floor that KEEPS both meeting rooms keeps its own column"
        );
    }

    /// Neither floor carries a SAFETY MARGIN — one-directional on purpose. It catches a
    /// floor set too HIGH, which on the width axis nothing else can. Too LOW is
    /// `every_floor_variant_seats_a_desk…`'s job. Tautological
    /// against today's `while` loops — that IS the point: it fires when a number replaces it.
    #[test]
    fn neither_floor_carries_a_safety_margin() {
        let under = super::MIN_LAYOUT_W - 1;
        assert!(
            super::band_w(under, super::widest_mid_x_pct()) < super::DESK_BAND_MIN_W
                || under < super::super::NEON_DOOR_WALL_W,
            "the width floor is not tight: {under} px still clears the band and the neon"
        );
        assert!(
            super::band_h(super::MIN_LAYOUT_H - 1) < super::DESK_BAND_MIN_H,
            "the height floor is not tight: {} px still clears the band",
            super::MIN_LAYOUT_H - 1
        );
    }

    /// `DESK_BAND_MIN_W` must be where `compute_pod_desks` ITSELF stops fitting a desk —
    /// the width floor is derived from it, so a formula that merely looks right
    /// would advertise a minimum the placer disagrees with.
    #[test]
    fn min_band_w_is_the_placers_own_first_desk_boundary() {
        let pod_h =
            super::POD_SIDE * super::DESK_H + (super::POD_SIDE - 1) * super::INTRA_POD_GAP_Y;
        let grid = super::PodGrid {
            band: super::Bounds {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            // The forced single pod of a narrow band (`pod_cols`/`pod_rows` floor at 1).
            cols: 1,
            rows: 1,
            stride_x: super::POD_SIDE * super::DESK_W
                + (super::POD_SIDE - 1) * super::INTRA_POD_GAP_X
                + super::INTER_POD_AISLE_X,
            stride_y: pod_h + super::INTER_POD_AISLE_Y,
            couch_to_desk_extra: 0,
        };
        for (width, seats) in [
            (super::DESK_BAND_MIN_W, true),
            (super::DESK_BAND_MIN_W - 1, false),
        ] {
            let band = super::Bounds {
                x: 5,
                y: 5,
                width,
                height: 200,
            };
            let desks = super::compute_pod_desks(None, super::PodGrid { band, ..grid });
            assert_eq!(
                !desks.is_empty(),
                seats,
                "a {width}px band: expected seats={seats}, placed {} desk(s)",
                desks.len()
            );
        }
        // The Y twin, same clamp on the other axis — the height floor derives from it.
        for (height, seats) in [
            (super::DESK_BAND_MIN_H, true),
            (super::DESK_BAND_MIN_H - 1, false),
        ] {
            let band = super::Bounds {
                x: 5,
                y: 5,
                width: 200,
                height,
            };
            let desks = super::compute_pod_desks(None, super::PodGrid { band, ..grid });
            assert_eq!(
                !desks.is_empty(),
                seats,
                "a {height}px-tall band: expected seats={seats}, placed {} desk(s)",
                desks.len()
            );
        }
    }

    /// Cross-checked against `compute_pod_desks`' own positions, not the band formula a
    /// test could only copy.
    #[test]
    fn no_inter_pod_band_row_is_ground_a_desk_blocks() {
        let band = super::Bounds {
            x: 0,
            y: 0,
            width: 400,
            height: 400,
        };
        let pod_h =
            super::POD_SIDE * super::DESK_H + (super::POD_SIDE - 1) * super::INTRA_POD_GAP_Y;
        let grid = super::PodGrid {
            band,
            cols: 2,
            rows: 3,
            stride_x: super::POD_SIDE * super::DESK_W
                + (super::POD_SIDE - 1) * super::INTRA_POD_GAP_X
                + super::INTER_POD_AISLE_X,
            stride_y: pod_h + super::INTER_POD_AISLE_Y,
            couch_to_desk_extra: 0,
        };
        let desks = super::compute_pod_desks(None, grid);
        assert!(!desks.is_empty(), "the fixture grid must place desks");
        let bands = grid.inter_pod_y_bands();
        assert!(!bands.is_empty(), "a 3-row grid has bands between its rows");
        for &(start, end) in &bands {
            for d in &desks {
                let (g0, g1) = (d.at.y, d.at.y + super::decor::DESK_GROUND_H);
                assert!(
                    start >= g1 || end <= g0,
                    "band {start}..{end} overlaps desk {d:?}'s ground rows {g0}..{g1}"
                );
            }
        }
    }

    /// A slot slides its piece to the nearest spot clear of `desk_art`, and an aisle
    /// narrower than the piece drops the slot without skipping the next slot's kind.
    #[test]
    fn a_pod_slot_slides_clear_of_the_desk_art_or_drops() {
        use super::{
            Bounds, DESK_H, DESK_W, INTER_POD_AISLE_X, INTER_POD_AISLE_Y, INTRA_POD_GAP_X,
            INTRA_POD_GAP_Y, POD_SIDE, PodGrid, Point, Size, compute_pod_decor, furniture_def,
        };
        let stride_x = POD_SIDE * DESK_W + (POD_SIDE - 1) * INTRA_POD_GAP_X + INTER_POD_AISLE_X;
        let stride_y = POD_SIDE * DESK_H + (POD_SIDE - 1) * INTRA_POD_GAP_Y + INTER_POD_AISLE_Y;
        let grid = |cols: u16| PodGrid {
            band: Bounds {
                x: 0,
                y: 0,
                width: cols * stride_x,
                height: stride_y,
            },
            cols,
            rows: 1,
            stride_x,
            stride_y,
            couch_to_desk_extra: 0,
        };
        let wall = |x: u16, east: u16| {
            (
                Point { x, y: 0 },
                Size {
                    w: east - x,
                    h: stride_y,
                },
            )
        };
        let reach = INTER_POD_AISLE_X / 2;

        // Art covering the centred piece's west `n` columns: it lands exactly `n` east.
        let free = compute_pod_decor(grid(2), 0, &[]);
        let w = furniture_def(free[0].kind.furniture()).visual.w;
        let n = reach / 2;
        let art_west = free[0].pos.x - w / 2;
        let slid = compute_pod_decor(grid(2), 0, &[wall(0, art_west + n)]);
        assert_eq!(
            slid[0].pos,
            Point {
                x: free[0].pos.x + n,
                ..free[0].pos
            }
        );

        // A gap one column narrower than slot 0's piece over its whole reach: slot 0
        // drops, and slot 1 still deals its own kind.
        let free = compute_pod_decor(grid(3), 0, &[]);
        let w = furniture_def(free[0].kind.furniture()).visual.w;
        let art_west = free[0].pos.x - w / 2;
        let narrow = [
            wall(0, art_west),
            wall(art_west + w - 1, free[0].pos.x + reach + w),
        ];
        assert_eq!(compute_pod_decor(grid(3), 0, &narrow), free[1..]);
    }
}
