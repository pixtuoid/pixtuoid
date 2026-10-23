//! The furniture roster: every physical thing the office stands on its floor
//! or hangs on its walls, placed and depth-keyed once. The hover hit-test, the
//! visual-clearance probe and the placement sweep map over
//! [`SceneLayout::fixtures`].

use super::placement::{boxed, centred};
use super::{
    Bounds, Facing, Furniture, Lounge, MeetingRoom, MeetingTrio, PantryRoom, Pivot, PlantItem,
    PlantKind, PodDecor, PodDecorItem, Point, SceneLayout, Size, WallDecor, WallDecorItem,
    WaypointKind, WindowBay, coat_rack_rect_at, furniture_def, sort_row_at,
};
use pixtuoid_core::state::FloorLocalDeskIndex;

/// One placed fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fixture {
    /// What it is, carrying the ids a painter joins live state on.
    pub(crate) kind: FixtureKind,
    /// The point the layout placed it at, which its art is pinned to: a
    /// centre-pinned piece's centre, the coat rack's pole top, else the top-left
    /// of [`visual`](Self::visual).
    pub(crate) at: Point,
    /// The box its art covers, in logical px.
    pub(crate) visual: Bounds,
    /// Where it sorts among everything else painted.
    pub(crate) depth: Depth,
}

impl Fixture {
    /// The top-left of the box its art covers.
    pub(crate) fn top_left(&self) -> Point {
        top_left(self.visual)
    }

    /// The shadow it casts where its art's box meets the floor, or `None` for a
    /// fixture that lies flat on the floor or hangs on a wall.
    pub(crate) fn contact(&self) -> Option<crate::ground::Contact> {
        let stands = match self.kind {
            FixtureKind::Wall { kind, .. } => kind.stands_on_floor(),
            FixtureKind::Desk(_)
            | FixtureKind::FilingCabinet(_)
            | FixtureKind::DeskChair(_)
            | FixtureKind::Station { .. }
            | FixtureKind::Plant { .. }
            | FixtureKind::Pod { .. }
            | FixtureKind::MeetingSofa { .. }
            | FixtureKind::MeetingTable { .. }
            | FixtureKind::MeetingChair { .. }
            | FixtureKind::CoatRack { .. }
            | FixtureKind::LoungeCouch
            | FixtureKind::SideTable
            | FixtureKind::FloorLamp
            | FixtureKind::FishTank
            | FixtureKind::KitchenIsland
            | FixtureKind::WaterCooler
            | FixtureKind::TrashBin => true,
            FixtureKind::MeetingRug { .. }
            | FixtureKind::Doormat { .. }
            | FixtureKind::NoticeBoard { .. }
            | FixtureKind::LoungeRug
            | FixtureKind::PantryMat
            | FixtureKind::IslandMat
            | FixtureKind::Door
            | FixtureKind::Runner
            | FixtureKind::NeonSign
            | FixtureKind::Clock => false,
        };
        let v = self.visual;
        stands.then(|| crate::ground::Contact::under(v.x, v.width, v.y + v.height))
    }
}

/// Where a fixture sorts. The derived order is the paint order: every
/// [`Depth::Backdrop`] fixture paints under every [`Depth::Sorted`] one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Depth {
    /// Painted flat under the sorted scene, in roster order: the mats, the
    /// runner, and wall fixtures nothing stands behind.
    Backdrop,
    /// Painted in the y-sort at `row`, a [`sort_row_at`] value. At an equal row,
    /// `tie` orders it against a figure and against a fixture of the other tie;
    /// roster order orders it against a fixture of the same tie.
    Sorted {
        row: u16,
        /// Which paints on top where a figure sorts at `row` too.
        tie: Tie,
    },
}

impl Depth {
    /// Sorted at `row`, under a figure tied with it.
    const fn sorted(row: u16) -> Self {
        Depth::Sorted {
            row,
            tie: Tie::FigureOver,
        }
    }
}

/// Which paints on top where a fixture and a figure (a character, a pet, a
/// mascot) sort at the same row. The derived order is the paint order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Tie {
    /// The figure, which sits on the fixture or stands in front of it.
    FigureOver,
    /// The fixture: a seat we see the back of hides whoever sits in it.
    FixtureOver,
}

/// The waypoint kinds that are furniture in their own right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Station {
    /// The pantry counter.
    PantryCounter,
    /// A corridor vending machine.
    VendingMachine,
    /// A corridor printer.
    Printer,
    /// The pantry's snack shelf.
    SnackShelf,
}

/// How a painter draws a [`Station`]: the one mapping both painters read.
#[derive(Debug, Clone, Copy)]
pub(crate) enum StationArt {
    /// The pantry counter, sized by its fixture.
    PantryCounter,
    SnackShelf,
    /// A corridor appliance, drawn from this piece.
    Appliance(pixtuoid_core::sprite::format::Piece),
}

impl Station {
    pub(crate) fn art(self) -> StationArt {
        use pixtuoid_core::sprite::format::Piece;
        match self {
            Station::PantryCounter => StationArt::PantryCounter,
            Station::SnackShelf => StationArt::SnackShelf,
            Station::VendingMachine => StationArt::Appliance(Piece::VendingMachine),
            Station::Printer => StationArt::Appliance(Piece::Printer),
        }
    }
}

/// Every kind of fixture. Consumers match it exhaustively, so a new kind is a
/// compile error in each until it decides how to hover and clear it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixtureKind {
    /// A home desk.
    Desk(FloorLocalDeskIndex),
    /// The filing cabinet beside a home desk.
    FilingCabinet(FloorLocalDeskIndex),
    /// The task chair of a desk whose occupant sits with their back to us.
    DeskChair(FloorLocalDeskIndex),
    /// A waypoint that is furniture itself.
    Station {
        /// Its index in [`SceneLayout::waypoints`].
        waypoint: usize,
        /// Which station.
        station: Station,
    },
    /// A free-standing plant.
    Plant {
        /// Its index in [`SceneLayout::plants`].
        item: usize,
        /// Which plant.
        kind: PlantKind,
    },
    /// Decor in the aisles between desk pods.
    Pod {
        /// Its index in [`SceneLayout::pod_decor`].
        item: usize,
        /// Which decor.
        kind: PodDecor,
    },
    /// Decor standing against or hung on a wall.
    Wall {
        /// Its index in [`SceneLayout::wall_decor`].
        item: usize,
        /// Which decor.
        kind: WallDecor,
    },
    /// The rug under a meeting room's trio.
    MeetingRug {
        /// Its room.
        room: usize,
    },
    /// A meeting-room sofa.
    MeetingSofa {
        /// Its room.
        room: usize,
        /// `0` north, `1` south.
        seat: usize,
        /// South of the table: we see its back.
        faces_away: bool,
    },
    /// A meeting room's table.
    MeetingTable {
        /// Its room.
        room: usize,
    },
    /// A head-of-table meeting chair.
    MeetingChair {
        /// Its index in [`SceneLayout::waypoints`].
        waypoint: usize,
        /// Which way its sitter faces.
        facing: Facing,
    },
    /// A meeting room's coat rack.
    CoatRack {
        /// Its room.
        room: usize,
    },
    /// The doormat outside a meeting room.
    Doormat {
        /// Its room.
        room: usize,
    },
    /// A meeting room's notice board.
    NoticeBoard {
        /// Its room.
        room: usize,
    },
    /// The rug the lounge couch stands on.
    LoungeRug,
    /// The lounge couch.
    LoungeCouch,
    /// The lounge side table.
    SideTable,
    /// The lounge floor lamp.
    FloorLamp,
    /// The lounge aquarium.
    FishTank,
    /// The pantry's kitchen island.
    KitchenIsland,
    /// The mat inside the pantry's north doorway.
    PantryMat,
    /// The bar mat under the kitchen island's serving front.
    IslandMat,
    /// The pantry's water cooler.
    WaterCooler,
    /// The pantry's trash bin.
    TrashBin,
    /// The elevator door.
    Door,
    /// The corridor's runner carpet.
    Runner,
    /// The neon sign the wall board shows its text on.
    NeonSign,
    /// The wall clock.
    Clock,
}

impl FixtureKind {
    /// What hovering it says, or `None` where a tooltip would be noise.
    pub(crate) fn hover_label(self) -> Option<&'static str> {
        Some(match self {
            FixtureKind::Desk(_) => "Desk",
            FixtureKind::FilingCabinet(_) => "Filing Cabinet",
            FixtureKind::DeskChair(_) => "Desk Chair",
            FixtureKind::Station { station, .. } => match station {
                Station::PantryCounter => "Pantry Counter",
                Station::VendingMachine => "Vending Machine",
                Station::Printer => "Printer",
                Station::SnackShelf => "Snack Shelf",
            },
            FixtureKind::Plant { kind, .. } => match kind {
                PlantKind::Ficus => "Ficus",
                PlantKind::Tall => "Tall Plant",
                PlantKind::Flower => "Flower Pot",
                PlantKind::Succulent => "Succulent",
            },
            FixtureKind::Pod { kind, .. } => match kind {
                PodDecor::PlantTall => "Tall Plant",
                PodDecor::Whiteboard => "Whiteboard",
                PodDecor::Tv => "TV Stand",
                PodDecor::PhoneBooth => "Phone Booth",
                PodDecor::StandingDesk => "Standing Desk",
            },
            FixtureKind::Wall { kind, .. } => match kind {
                WallDecor::Whiteboard => "Whiteboard",
                WallDecor::Bookshelf => "Bookshelf",
                WallDecor::BulletinBoard => "Bulletin Board",
                WallDecor::ExitSign => "Exit Sign",
                WallDecor::MeetingScreen => "Meeting Screen",
            },
            FixtureKind::MeetingRug { .. } | FixtureKind::LoungeRug => "Rug",
            FixtureKind::MeetingSofa { .. } => "Meeting Sofa",
            FixtureKind::MeetingTable { .. } => "Meeting Table",
            FixtureKind::MeetingChair { .. } => "Meeting Chair",
            FixtureKind::CoatRack { .. } => "Coat Rack",
            FixtureKind::Doormat { .. } | FixtureKind::PantryMat => "Doormat",
            FixtureKind::NoticeBoard { .. } => "Notice Board",
            FixtureKind::LoungeCouch => "Lounge Sofa",
            FixtureKind::SideTable => "Side Table",
            FixtureKind::FloorLamp => "Floor Lamp",
            FixtureKind::FishTank => "Fish Tank",
            FixtureKind::KitchenIsland => "Kitchen Island",
            FixtureKind::IslandMat => "Bar Mat",
            FixtureKind::WaterCooler => "Water Cooler",
            FixtureKind::TrashBin => "Trash Bin",
            FixtureKind::Door => "Elevator",
            FixtureKind::Clock => "Clock",
            // The whole corridor: a tooltip wherever the pointer crosses it.
            FixtureKind::Runner => return None,
            // The wall board's text and star link sit on it, and hover there
            // belongs to them.
            FixtureKind::NeonSign => return None,
        })
    }
}

/// The wall clock's face.
pub(crate) const CLOCK: Size = Size { w: 7, h: 7 };
/// The wall clock's top row.
const CLOCK_Y: u16 = 1;

/// The neon sign's outer box: its origin, then its size with the frame
/// included.
pub(crate) const NEON_PANEL_X: u16 = 1;
pub(crate) const NEON_PANEL_Y: u16 = 1;
/// The neon sign's outer width in pixels. A pixel column is a terminal cell
/// column in the half-block flush, so it is a cell width too.
pub const NEON_PANEL_W: u16 = 30;
pub(crate) const NEON_PANEL_H: u16 = 8;
/// The frame thickness `paint_neon_panel` lights on every side — it reads THIS,
/// so the interior derivations below match the pixels it leaves dark.
pub(crate) const NEON_PANEL_BORDER: u16 = 1;

/// The corridor runner's diamond lattice pitch, in logical px. Taste pin: a
/// tighter stride read as bathroom tiling rather than a woven runner at
/// half-block scale.
pub(crate) const RUNNER_LATTICE_STRIDE: i32 = 10;
/// The dark interior's left cell-origin — where board text starts. The board's
/// text pins to the interior, not the outer box, or the lit text overruns the
/// glowing frame by the border on each side.
pub const NEON_PANEL_INNER_X: u16 = NEON_PANEL_X + NEON_PANEL_BORDER;
/// The dark interior's cell WIDTH — the board's usable text width.
pub const NEON_PANEL_INNER_W: u16 = NEON_PANEL_W - 2 * NEON_PANEL_BORDER;
/// The dark interior's top pixel-origin — where the floating / wasm painters
/// anchor the board's first text row.
pub const NEON_PANEL_INNER_Y: u16 = NEON_PANEL_Y + NEON_PANEL_BORDER;
/// The dark interior's pixel HEIGHT.
pub const NEON_PANEL_INNER_H: u16 = NEON_PANEL_H - 2 * NEON_PANEL_BORDER;
// The interior must be a non-empty strict subset of the outer frame (catches a
// degenerate BORDER=0 / oversized-border config at compile time).
const _: () = assert!(NEON_PANEL_INNER_W > 0 && NEON_PANEL_INNER_W < NEON_PANEL_W);
const _: () = assert!(NEON_PANEL_INNER_H > 0 && NEON_PANEL_INNER_H < NEON_PANEL_H);

/// The neon sign's outer box.
pub(crate) const NEON_PANEL: Bounds = Bounds {
    x: NEON_PANEL_X,
    y: NEON_PANEL_Y,
    width: NEON_PANEL_W,
    height: NEON_PANEL_H,
};

const NOTICE_BOARD: Size = Size { w: 8, h: 5 };
/// Rows from the board's foot up off the band's trim.
const NOTICE_BOARD_SILL: u16 = 2;
/// Clear columns the board keeps from its neighbours.
const NOTICE_BOARD_GAP: u16 = 1;

/// How far north of the couch centre the lounge rug sorts: under the couch,
/// which then sits on it.
const LOUNGE_RUG_Z_LEAD: u16 = 2;

fn top_left(b: Bounds) -> Point {
    Point { x: b.x, y: b.y }
}

/// Each home desk's filing cabinet, then the desk: the fixture authority the
/// floor whiteboard also clears, before the layout that rosters them exists.
pub(crate) fn desk_fixtures(
    home_desks: &[Point],
    buf_h: u16,
) -> impl Iterator<Item = Fixture> + '_ {
    let desk = furniture_def(Furniture::Desk).visual;
    home_desks.iter().enumerate().flat_map(move |(i, &at)| {
        let local = FloorLocalDeskIndex(i);
        let depth = Depth::sorted(at.y + desk.h);
        filing_cabinet_top_left(at, local, buf_h)
            .map(|tl| Fixture {
                kind: FixtureKind::FilingCabinet(local),
                at: tl,
                visual: boxed(tl, furniture_def(Furniture::FilingCabinet).visual),
                depth,
            })
            .into_iter()
            .chain(std::iter::once(Fixture {
                kind: FixtureKind::Desk(local),
                at,
                visual: boxed(at, desk),
                depth,
            }))
    })
}

/// Each home desk's task chair, where `facing` turns its desk north.
pub(crate) fn desk_chair_fixtures(
    home_desks: &[Point],
    facing: impl Fn(FloorLocalDeskIndex) -> Facing,
) -> impl Iterator<Item = Fixture> {
    home_desks.iter().enumerate().filter_map(move |(i, &at)| {
        let local = FloorLocalDeskIndex(i);
        let facing = facing(local);
        desk_chair_top_left(at, facing).map(|tl| Fixture {
            kind: FixtureKind::DeskChair(local),
            at: tl,
            visual: boxed(tl, furniture_def(Furniture::DeskChair).visual),
            depth: Depth::Sorted {
                row: desk_chair_sort_row(at, facing),
                tie: Tie::FixtureOver,
            },
        })
    })
}

/// Each pod decor item.
pub(crate) fn pod_decor_fixtures(pod_decor: &[PodDecorItem]) -> impl Iterator<Item = Fixture> + '_ {
    pod_decor
        .iter()
        .enumerate()
        .map(|(item, &PodDecorItem { kind, pos })| {
            centred_row(FixtureKind::Pod { item, kind }, pos, kind.furniture())
        })
}

/// A centre-pinned furniture row at `pos`, sorting at its south row.
fn centred_row(kind: FixtureKind, pos: Point, row: Furniture) -> Fixture {
    let size = furniture_def(row).visual;
    Fixture {
        kind,
        at: pos,
        visual: centred(pos, size),
        depth: Depth::sorted(sort_row_at(Pivot::Center, pos, size.h)),
    }
}

impl SceneLayout {
    /// Every fixture of this office, backdrop first and then in the order that
    /// breaks the y-sort's ties.
    ///
    /// The ONE destructure of the layout and its aggregates with no `..`: a new
    /// field is a compile error here until it is rostered or bound away with
    /// its reason, and every consumer then meets its kinds in an exhaustive
    /// match. Lazy, so a probe that stops at its first hit builds nothing.
    pub(crate) fn fixtures(&self) -> impl Iterator<Item = Fixture> + '_ {
        let SceneLayout {
            // Read through `clock_pos`.
            buf_w: _,
            buf_h,
            // Containers the pieces were placed in, not pieces.
            cubicle_band: _,
            cubicle_aisle: _,
            home_desks,
            // A desk attribute, read through `desk_facing`.
            desk_facings: _,
            waypoints,
            plants,
            wall_decor,
            pod_decor,
            lounge,
            // Read through `door_rect`.
            door: _,
            // A walkable point, not a thing.
            door_threshold: _,
            meeting_rooms,
            pantry,
            // Architecture, which the painters draw from `wall_pieces`; the
            // pantry mat reads `doorways` through `pantry_entry_mat`.
            room_walls: _,
            doorways: _,
            wall_pieces: _,
            // Wall-band geometry, not a piece.
            top_margin: _,
            corridor,
            // What the fixtures stamp, not fixtures.
            walkable: _,
            reachable: _,
        } = self;
        let rooms = meeting_rooms.iter().enumerate().map(|(room, r)| {
            // Its bounds place its pieces through the rect methods.
            let MeetingRoom { bounds: _, trio } = r;
            let trio = trio.map(|t| {
                let MeetingTrio { sofas, table } = t;
                (t, sofas, table)
            });
            (room, r, trio)
        });
        let (island, pantry_uprights) = match pantry {
            Some(
                p @ PantryRoom {
                    // Place the counter, the cooler and the bin through their
                    // rect methods.
                    bounds: _,
                    counter_size: _,
                    kitchen_island,
                },
            ) => (*kitchen_island, Some(p)),
            None => (None, None),
        };
        let (couch, lamp, side_table, tank) = match lounge {
            Some(Lounge {
                couch_center,
                floor_lamp,
                side_table,
                fish_tank,
            }) => (
                Some(*couch_center),
                Some(*floor_lamp),
                Some(*side_table),
                *fish_tank,
            ),
            None => (None, None, None, None),
        };
        let backdrop = |kind, visual| Fixture {
            kind,
            at: top_left(visual),
            visual,
            depth: Depth::Backdrop,
        };
        let upright = |kind, at, visual: Bounds| Fixture {
            kind,
            at,
            visual,
            depth: Depth::sorted(visual.y + visual.height - 1),
        };
        std::iter::once(backdrop(FixtureKind::NeonSign, NEON_PANEL))
            .chain(
                self.clock_pos()
                    .map(|at| backdrop(FixtureKind::Clock, boxed(at, CLOCK))),
            )
            .chain(std::iter::once(backdrop(FixtureKind::Runner, *corridor)))
            .chain(rooms.clone().filter_map(move |(room, r, _)| {
                self.doormat(r)
                    .map(|b| backdrop(FixtureKind::Doormat { room }, b))
            }))
            .chain(
                self.pantry_entry_mat()
                    .map(|b| backdrop(FixtureKind::PantryMat, b)),
            )
            .chain(
                self.island_bar_mat()
                    .map(|b| backdrop(FixtureKind::IslandMat, b)),
            )
            .chain(desk_fixtures(home_desks, *buf_h))
            .chain(rooms.clone().filter_map(move |(room, _, trio)| {
                // Off the corridor runner a south room's floor runs into.
                let rug = trio?.0.rug(corridor.y);
                Some(Fixture {
                    kind: FixtureKind::MeetingRug { room },
                    at: top_left(rug),
                    visual: rug,
                    depth: Depth::sorted(rug.y),
                })
            }))
            .chain(rooms.clone().flat_map(|(room, _, trio)| {
                trio.into_iter().flat_map(move |(_, sofas, table)| {
                    sofas.into_iter().enumerate().map(move |(seat, sofa)| {
                        let faces_away = sofa.y >= table.y;
                        Fixture {
                            kind: FixtureKind::MeetingSofa {
                                room,
                                seat,
                                faces_away,
                            },
                            at: sofa,
                            visual: centred(sofa, furniture_def(Furniture::MeetingSofaBody).visual),
                            depth: Depth::Sorted {
                                row: super::seated_sort_row(sofa),
                                tie: if faces_away {
                                    Tie::FixtureOver
                                } else {
                                    Tie::FigureOver
                                },
                            },
                        }
                    })
                })
            }))
            .chain(rooms.clone().filter_map(|(room, _, trio)| {
                let (_, _, table) = trio?;
                Some(centred_row(
                    FixtureKind::MeetingTable { room },
                    table,
                    Furniture::MeetingTable,
                ))
            }))
            .chain(
                island.map(|at| {
                    centred_row(FixtureKind::KitchenIsland, at, Furniture::KitchenIsland)
                }),
            )
            .chain(pantry_uprights.into_iter().flat_map(move |p| {
                p.water_cooler_rect()
                    .map(|b| upright(FixtureKind::WaterCooler, top_left(b), b))
                    .into_iter()
                    .chain(
                        p.trash_bin_rect()
                            .map(|b| upright(FixtureKind::TrashBin, top_left(b), b)),
                    )
            }))
            .chain(lounge.as_ref().map(|l| {
                let first_desk_row = home_desks.iter().map(|d| d.y).min();
                let rug = l.rug(first_desk_row.unwrap_or(*buf_h));
                Fixture {
                    kind: FixtureKind::LoungeRug,
                    at: top_left(rug),
                    visual: rug,
                    depth: Depth::sorted(l.couch_center.y.saturating_sub(LOUNGE_RUG_Z_LEAD)),
                }
            }))
            .chain(
                couch.map(|at| {
                    centred_row(FixtureKind::LoungeCouch, at, Furniture::MeetingSofaBody)
                }),
            )
            .chain(
                side_table
                    .map(|at| centred_row(FixtureKind::SideTable, at, Furniture::LoungeSideTable)),
            )
            .chain(
                waypoints
                    .iter()
                    .enumerate()
                    .filter_map(move |(waypoint, wp)| {
                        let station = match wp.kind {
                            WaypointKind::Pantry => Station::PantryCounter,
                            WaypointKind::VendingMachine => Station::VendingMachine,
                            WaypointKind::Printer => Station::Printer,
                            WaypointKind::SnackShelf => Station::SnackShelf,
                            // Seats on the lounge couch, a meeting sofa, a meeting chair
                            // (rostered below, after the lamp) or the island.
                            WaypointKind::Couch
                            | WaypointKind::MeetingSofa
                            | WaypointKind::MeetingChair
                            | WaypointKind::Island => return None,
                            // Promoted pod-decor slots: their decor is the fixture.
                            WaypointKind::PhoneBooth | WaypointKind::StandingDesk => return None,
                        };
                        let kind = FixtureKind::Station { waypoint, station };
                        Some(match station {
                            // Runtime-sized: the furniture row is empty on purpose.
                            Station::PantryCounter => {
                                upright(kind, wp.pos, pantry_uprights?.counter_rect()?)
                            }
                            Station::VendingMachine | Station::Printer | Station::SnackShelf => {
                                centred_row(kind, wp.pos, wp.kind.furniture())
                            }
                        })
                    }),
            )
            .chain(pod_decor_fixtures(pod_decor))
            .chain(
                plants
                    .iter()
                    .enumerate()
                    .map(|(item, &PlantItem { kind, pos })| {
                        centred_row(FixtureKind::Plant { item, kind }, pos, kind.furniture())
                    }),
            )
            .chain(lamp.map(|at| centred_row(FixtureKind::FloorLamp, at, Furniture::FloorLamp)))
            .chain(
                waypoints
                    .iter()
                    .enumerate()
                    .filter(|(_, wp)| wp.kind == WaypointKind::MeetingChair)
                    .map(|(waypoint, wp)| Fixture {
                        kind: FixtureKind::MeetingChair {
                            waypoint,
                            facing: wp.facing,
                        },
                        at: wp.pos,
                        visual: centred(wp.pos, furniture_def(Furniture::MeetingChair).visual),
                        // Its sitter's own row: they sit on it.
                        depth: Depth::sorted(super::seated_sort_row(wp.pos)),
                    }),
            )
            .chain(tank.map(|at| centred_row(FixtureKind::FishTank, at, Furniture::FishTank)))
            .chain(rooms.filter_map(move |(room, r, _)| {
                let pole = r.coat_rack_pos()?;
                Some(upright(
                    FixtureKind::CoatRack { room },
                    pole,
                    coat_rack_rect_at(pole),
                ))
            }))
            .chain(std::iter::once(self.door_rect()).map(|visual| Fixture {
                kind: FixtureKind::Door,
                at: top_left(visual),
                visual,
                depth: Depth::sorted(visual.y + visual.height),
            }))
            .chain(
                wall_decor
                    .iter()
                    .enumerate()
                    .map(|(item, &WallDecorItem { kind, pos })| {
                        let size = furniture_def(kind.furniture()).visual;
                        Fixture {
                            kind: FixtureKind::Wall { item, kind },
                            at: pos,
                            visual: boxed(pos, size),
                            depth: Depth::sorted(sort_row_at(Pivot::TopLeft, pos, size.h)),
                        }
                    }),
            )
            .chain((0..meeting_rooms.len()).filter_map(move |room| {
                self.notice_board_rect(room)
                    .map(|b| upright(FixtureKind::NoticeBoard { room }, top_left(b), b))
            }))
            .chain(desk_chair_fixtures(home_desks, |i| self.desk_facing(i)))
    }

    /// The fixture hovering `cell` points at: the topmost whose art covers any
    /// of it — the one painted last there, by depth and then roster order.
    pub fn fixture_at(&self, cell: Bounds) -> Option<FixtureKind> {
        self.fixtures()
            .enumerate()
            .filter(|(_, f)| cell.overlaps(f.visual))
            .max_by_key(|&(i, f)| (f.depth, i))
            .map(|(_, f)| f.kind)
    }

    /// Where meeting room `room` hangs its notice board: on the band, its north
    /// wall, within the window pane nearest the room's middle, at the free spot
    /// nearest the pane's centre — `None` for a room whose north wall is not
    /// the band, or with none free.
    pub(crate) fn notice_board_rect(&self, room: usize) -> Option<Bounds> {
        let b = self.meeting_rooms.get(room)?.bounds;
        if b.y > self.top_margin {
            return None;
        }
        let y = self
            .wall_band_h()
            .checked_sub(NOTICE_BOARD_SILL + NOTICE_BOARD.h)?;
        let taken: Vec<Bounds> = self
            .wall_decor
            .iter()
            .map(|d| boxed(d.pos, furniture_def(d.kind.furniture()).visual))
            .chain(
                self.plants
                    .iter()
                    .map(|p| centred(p.pos, furniture_def(p.kind.furniture()).visual)),
            )
            .chain([self.door_rect(), NEON_PANEL])
            .chain(self.clock_pos().map(|at| boxed(at, CLOCK)))
            .collect();
        let clear = |board: Bounds| {
            let spaced = board.widened(NOTICE_BOARD_GAP);
            taken.iter().all(|v| !spaced.overlaps(*v))
        };
        let (lo, hi) = (b.x + 1, (b.x + b.width).saturating_sub(1));
        let middle = b.x + b.width / 2;
        self.window_bays()
            .flat_map(WindowBay::panes)
            .filter_map(|pane| {
                // The free spot nearest the pane's centre the room's wall allows.
                let west = pane.start.max(lo);
                let east = pane.end.min(hi).checked_sub(NOTICE_BOARD.w)?;
                let centred = (pane.start + pane.end - NOTICE_BOARD.w) / 2;
                (west..=east)
                    .map(|x| boxed(Point { x, y }, NOTICE_BOARD))
                    .filter(|board| clear(*board))
                    .min_by_key(|board| board.x.abs_diff(centred))
            })
            .min_by_key(|board| (board.x + board.width / 2).abs_diff(middle))
    }

    /// The wall clock's top-left: centred on the window post nearest the
    /// wall's middle that the neon sign leaves clear, or `None` on a wall
    /// with none.
    pub(crate) fn clock_pos(&self) -> Option<Point> {
        let middle = self.buf_w / 2;
        super::window_posts(self.buf_w)
            .map(|post| u16::midpoint(post.start, post.end))
            .filter(|&centre| centre - CLOCK.w / 2 >= NEON_PANEL.x + NEON_PANEL.width)
            .min_by_key(|centre| centre.abs_diff(middle))
            .map(|centre| Point {
                x: centre - CLOCK.w / 2,
                y: CLOCK_Y,
            })
    }

    /// The mat outside meeting room `r`'s door, or `None` when the lounge's
    /// side table or a plant stands over it: half hidden, it reads as a stain.
    fn doormat(&self, r: &MeetingRoom) -> Option<Bounds> {
        let mat = r.doormat_rect()?;
        let side_table = self.lounge.as_ref().map(|l| {
            centred(
                l.side_table,
                furniture_def(Furniture::LoungeSideTable).visual,
            )
        });
        let plants = self
            .plants
            .iter()
            .map(|p| Some(centred(p.pos, furniture_def(p.kind.furniture()).visual)));
        std::iter::once(side_table)
            .chain(plants)
            .flatten()
            .all(|b| !b.overlaps(mat))
            .then_some(mat)
    }

    /// The mat inside the pantry's north doorway, one clear floor row south of
    /// the wall face, or `None` without a pantry or that doorway.
    pub(crate) fn pantry_entry_mat(&self) -> Option<Bounds> {
        const ENTRY_MAT: Size = Size { w: 16, h: 5 };
        let p = self.pantry?;
        let dw = self
            .doorways
            .iter()
            .find(|d| d.start.y == d.end.y && d.start.y == p.bounds.y)?;
        let mut mat = centred(
            Point {
                x: u16::midpoint(dw.start.x, dw.end.x),
                y: dw.start.y + super::WALL_THICK_H + 1 + ENTRY_MAT.h / 2,
            },
            ENTRY_MAT,
        );
        // Inside the room's columns, or none: past them it lies under the wall.
        let b = p.bounds;
        let last_x = (b.x + b.width)
            .checked_sub(mat.width)
            .filter(|&x| x >= b.x)?;
        mat.x = mat.x.clamp(b.x, last_x);
        // Shifted west off the cooler standing at the room's east edge.
        if let Some(cooler) = p.water_cooler_rect().filter(|c| c.overlaps(mat)) {
            mat.x = cooler
                .x
                .checked_sub(mat.width)
                .filter(|&x| x >= p.bounds.x)?;
        }
        // Gives way to whatever stands or lies over it: half hidden, it reads
        // as a stain.
        let island = p
            .kitchen_island
            .map(|at| centred(at, furniture_def(Furniture::KitchenIsland).visual));
        [
            island,
            p.counter_rect(),
            p.snack_shelf_rect(),
            p.trash_bin_rect(),
            Some(self.corridor),
        ]
        .into_iter()
        .flatten()
        .all(|b| !b.overlaps(mat))
        .then_some(mat)
    }

    /// The thin bar mat under the kitchen island: the island covers most of
    /// it, leaving a sliver along the bar's south serving front.
    pub(crate) fn island_bar_mat(&self) -> Option<Bounds> {
        const BAR_MAT: Size = Size { w: 26, h: 4 };
        // Drops the mat's centre from the island's to the seat row, so the
        // sliver clears the body's south edge (mock-verified).
        const BAR_MAT_Y_OFF: u16 = 4;
        let isl = self.pantry.and_then(|p| p.kitchen_island)?;
        Some(centred(
            Point {
                x: isl.x,
                y: isl.y + BAR_MAT_Y_OFF,
            },
            BAR_MAT,
        ))
    }

    /// The floor lamp's base — the row its sprite stands on, where its halo
    /// pools — or `None` without a lounge.
    pub(crate) fn floor_lamp_base(&self) -> Option<Point> {
        let lamp = self.floor_lamp()?;
        Some(Point {
            x: lamp.x,
            y: sort_row_at(
                Pivot::Center,
                lamp,
                furniture_def(Furniture::FloorLamp).visual.h,
            ),
        })
    }

    /// The coffee machine's box on the pantry counter, or `None` without one.
    pub fn coffee_machine(&self) -> Option<Bounds> {
        let counter = self.pantry?.counter_rect()?;
        let (x0, x1) = coffee_machine_cols(counter.width);
        Some(Bounds {
            x: counter.x + x0,
            y: counter.y,
            width: x1 - x0,
            height: counter.height,
        })
    }
}

/// The coffee machine's logical columns `[start, end)` in the large counter:
/// where the steam and the click target sit.
const PANTRY_COFFEE_COLS_LARGE: (u16, u16) = (11, 18);
/// The coffee machine's columns within the compact `pantry_small` sprite.
const PANTRY_COFFEE_COLS_SMALL: (u16, u16) = (9, 12);

/// The coffee machine's columns within a `counter_w`-wide counter's sprite.
pub(crate) fn coffee_machine_cols(counter_w: u16) -> (u16, u16) {
    if counter_w >= super::PANTRY_COUNTER_LARGE_W {
        PANTRY_COFFEE_COLS_LARGE
    } else {
        PANTRY_COFFEE_COLS_SMALL
    }
}

/// Whether desk `i` stands a filing cabinet beside it.
pub(crate) fn desk_has_cabinet(i: FloorLocalDeskIndex) -> bool {
    i.0.is_multiple_of(2)
}

/// Where desk `i`, at `desk`, stands its filing cabinet — one clear column west
/// of the desk — or `None` for a desk without one, or whose cabinet would run
/// off a `buf_h`-tall office's south edge.
fn filing_cabinet_top_left(desk: Point, i: FloorLocalDeskIndex, buf_h: u16) -> Option<Point> {
    let cab = furniture_def(Furniture::FilingCabinet).visual;
    (desk_has_cabinet(i) && desk.y + cab.h <= buf_h).then(|| Point {
        x: desk.x.saturating_sub(cab.w + 1),
        y: desk.y,
    })
}

/// Where the task chair stands at `desk`, or `None` for a desk that does not
/// face north: a viewer-facing occupant sits behind their desk, in front of
/// their chair.
pub(crate) fn desk_chair_top_left(desk: Point, facing: Facing) -> Option<Point> {
    /// The backrest crosses the occupant's lower torso deliberately — clearing
    /// the sprite would leave a detached slab at their feet.
    const CHAIR_BACK_TOP_DY: u16 = 6;
    (facing == Facing::North).then(|| Point {
        x: super::desk_walk_anchor_facing(desk, facing)
            .x
            .saturating_sub(furniture_def(Furniture::DeskChair).visual.w / 2),
        y: desk.y + CHAIR_BACK_TOP_DY,
    })
}

/// The depth a desk's task chair sorts at: its seat's own sort row, which the sim
/// gives the occupant arriving at, sitting in and leaving the seat alike. Its
/// [`Tie::FixtureOver`] therefore draws the chair over its occupant throughout —
/// no flip where the walk ends and the sit begins.
pub(crate) fn desk_chair_sort_row(desk: Point, facing: Facing) -> u16 {
    super::desk_walk_anchor_facing(desk, facing).y
}

#[cfg(test)]
pub(crate) mod tests;
