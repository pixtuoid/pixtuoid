//! Decor vocabulary used by `SceneLayout` — the enums describing every piece of
//! furniture and waypoint kind in the office, plus THE table giving each its
//! geometry. Kept separate so a new sprite kind doesn't churn the layout math.

use pixtuoid_core::sprite::format::Piece;

use super::{DESK_FOOT_H, DESK_H, DESK_W, OBSTACLE_PAD_PX, Pivot, Point, Size, TIGHT_PAD_PX};

/// Wander destinations the Idle state machine can pick — each kind controls the
/// pose + sprite an arriving agent takes. Plants/lamps are decor, not waypoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaypointKind {
    /// Top-of-cubicle viewing couch facing the city windows.
    Couch,
    /// Pantry counter — kitchen + coffee.
    Pantry,
    /// Aisle phone booth — agent stands at the door (private call).
    PhoneBooth,
    /// Aisle standing desk (alternate workstation).
    StandingDesk,
    /// Corridor vending machine — agent stands in front to grab a drink.
    VendingMachine,
    /// Corridor printer — agent stands in front while "printing."
    Printer,
    /// Meeting-room sofa seat, facing the table. Multiple seats per sofa.
    MeetingSofa,
    /// Meeting-room spot beside the table, facing it.
    MeetingChair,
    /// Kitchen-island spot at the island edge (coffee-and-chat).
    Island,
    /// Snack shelf — agent stands in front browsing the shelves.
    SnackShelf,
}

/// Per-spot idle dwell window. `range_ms == 0` is the DECOR sentinel (not a
/// wander destination).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DwellWindow {
    /// Baseline dwell time at the spot, in milliseconds.
    pub(crate) base_ms: u64,
    /// Extra randomized dwell added on top of `base_ms`, in milliseconds
    /// (`0` marks the [`Self::DECOR`] non-destination sentinel).
    pub(crate) range_ms: u64,
}
impl DwellWindow {
    /// The decor sentinel — scenery, not a wander destination.
    pub(crate) const DECOR: DwellWindow = DwellWindow {
        base_ms: 0,
        range_ms: 0,
    };
}

/// Plant GROUND footprint — the one geometry VALUE shared by the ficus + tall
/// plant rows in [`furniture_def`]: a shallow POT strip the mask south-anchors to
/// the sprite's base, with the leafy canopy overhanging it (invariant #6). Read
/// only THROUGH the table (`furniture_def(_).footprint`), never directly.
pub(crate) const PLANT_FOOTPRINT: Size = Size { w: 6, h: 3 };

/// Which sides an agent may approach a piece of furniture from, in the
/// CANONICAL frame (furniture facing South, toward the viewer). `Self::allows`
/// rotates this to the live `facing`, so one stored set works for
/// variable-facing furniture. **To add/remove an entry side, flip one bool.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApproachSides {
    /// Approachable from the north (−y) in the canonical frame?
    pub(crate) n: bool,
    /// Approachable from the south (+y, the canonical front)?
    pub(crate) s: bool,
    /// Approachable from the east (+x)?
    pub(crate) e: bool,
    /// Approachable from the west (−x)?
    pub(crate) w: bool,
}

impl ApproachSides {
    /// 360° — approachable from every open side (pantry counter).
    pub(crate) const ALL: Self = Self {
        n: true,
        s: true,
        e: true,
        w: true,
    };

    /// This canonical (facing-South) set rotated to the live `facing`.
    pub(crate) fn rotated(self, facing: Facing) -> Self {
        let s = self;
        match facing {
            Facing::South => s,
            Facing::North => Self {
                n: s.s,
                s: s.n,
                e: s.w,
                w: s.e,
            },
            Facing::East => Self {
                n: s.e,
                s: s.w,
                e: s.s,
                w: s.n,
            },
            Facing::West => Self {
                n: s.w,
                s: s.e,
                e: s.n,
                w: s.s,
            },
        }
    }

    /// Is the absolute unit dir `(dx, dy)` (north = (0,−1), south = (0,1),
    /// east = (1,0), west = (−1,0)) an allowed approach under the live `facing`?
    pub(crate) fn allows(self, facing: Facing, dir: (i32, i32)) -> bool {
        let r = self.rotated(facing);
        match dir {
            (0, -1) => r.n,
            (0, 1) => r.s,
            (1, 0) => r.e,
            (-1, 0) => r.w,
            _ => false,
        }
    }
}

/// Approach sides for the home desk. Canonical: exclude the south front (the
/// monitor faces the viewer; the agent sits behind it), so reachable from N/E/W.
pub(crate) const DESK_APPROACH: ApproachSides = ApproachSides {
    n: true,
    s: false,
    e: true,
    w: true,
};

/// Definition record for a furniture kind — the single source of truth for its
/// ground shape, occupancy semantics, and dwell. Reshaping a piece of furniture is
/// editing ONE row of [`furniture_def`]; the walkable mask, stand-point, hit-test
/// hitbox and render depth baseline all DERIVE from these fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FurnitureDef {
    /// Mask pad (px) around the blocked ground, every side. Whatever measures that
    /// ground reads this row's pad, never a sibling's.
    pub(crate) pad: u16,
    /// Ground footprint `(w, h)` the walkable mask stamps (top-down z=0 rect), or
    /// `None` for slots that add no obstacle of their own (MeetingSofa/MeetingChair
    /// sit on furniture stamped elsewhere). NB: `Pantry` is also `None` because its
    /// footprint is runtime-sized — `obstacle_footprint` special-cases it, the one
    /// kind whose shape isn't a static literal.
    pub(crate) footprint: Option<Size>,
    /// Visual sprite size `(w, h)` in buffer px — the SECOND geometry axis, kept
    /// distinct from `footprint` (the top-down ground rule, invariant #6): a sprite
    /// legitimately overhangs its ground base, and conflating the two is the
    /// canopy-over-aisle bug this split prevents. Render centering + the z-sort
    /// south row derive from this; the mask derives from `footprint`.
    pub visual: Size,
    /// The agent occupies `pos` DIRECTLY (sprite renders ON the furniture), so
    /// `stand_point` passes `pos` through unchanged instead of resolving a walkable
    /// cell beside the furniture. NOT "a human can sit here", and NOT capacity: a
    /// phone booth renders stand-beside yet holds exactly one caller, so capacity
    /// lives on the separate [`exclusive`](Self::exclusive) field. This is the set
    /// `seated_foot_cell` switches on (its `unreachable!` arm keeps the two in step).
    pub(crate) occupies_pos: bool,
    /// A single-occupancy DESTINATION: at most one agent is assigned here at a
    /// time. SUPERSET of `occupies_pos` — every seat, PLUS the enclosed
    /// stand-beside singles (`PhoneBooth`, `StandingDesk`) that render at a SIDE
    /// cell yet still hold exactly one person. Queue spots (pantry / vending /
    /// printer / snack shelf) are NOT exclusive — agents share and step aside.
    pub(crate) exclusive: bool,
    /// Per-spot idle dwell window. `range_ms == 0` (the `DECOR` rows) marks a kind
    /// that is NOT a wander destination; `dwell_ms` guards with
    /// `% range_ms.max(1)`, so a zero range is safe. Do not "fix" a decor row to a
    /// non-zero range — that silently turns it into a wander destination.
    pub(crate) dwell: DwellWindow,
    /// Canonical (facing-South) sides an agent may approach from. Obstacle
    /// furniture against walls keeps `ALL` (walls already constrain the open side);
    /// seats use "front + sides, no back" so a walker never paths in through the
    /// sofa back.
    pub(crate) approach: ApproachSides,
    /// Where `footprint` sits inside the VISUAL box horizontally. Every current row
    /// is `Center`; the field exists so a future sideways-overhanging piece declares
    /// `Start`/`End` instead of needing a new stamp path.
    pub(crate) ground_x: GroundAlign,
    /// Where `footprint` sits inside the VISUAL box vertically: `End` for the
    /// overhang canopy/panel/column pieces AND the desk (invariant #6, the
    /// walk-behind shape), `Center` for the meeting sofa body + floor lamp.
    /// Resolves to a pixel offset from `visual − footprint` at stamp time.
    pub(crate) ground_y: GroundAlign,
}

impl FurnitureDef {
    /// The blocked ground rect for an EXPLICIT footprint, placed with THIS def's
    /// visual box + ground aligns — so the runtime-footprint path (a waypoint's
    /// `approach::obstacle_footprint`) shares the def's alignment with the table
    /// path and no call site re-threads `visual`/`ground_x`/`ground_y`.
    pub(super) fn ground_rect_of(&self, pivot: Pivot, pos: Point, fp: Size) -> (Point, Size) {
        super::mask::ground_rect(pivot, pos, fp, self.visual, self.ground_x, self.ground_y)
    }

    /// The blocked ground rect from this def's OWN table footprint, or `None` when
    /// the piece has no ground footprint (wall-hung decor, runtime-sized pantry
    /// counter). THE concentrator the mask stamp / collision checks / placement
    /// sweep all read.
    pub(super) fn ground_rect(&self, pivot: Pivot, pos: Point) -> Option<(Point, Size)> {
        self.footprint.map(|fp| self.ground_rect_of(pivot, pos, fp))
    }

    /// The first column the mask leaves walkable east of this piece's ground (its
    /// [`Self::pad`] included), or `None` for a piece with no table footprint, which
    /// the mask doesn't block.
    pub(super) fn east_of_padded_ground(&self, pivot: Pivot, pos: Point) -> Option<u16> {
        self.ground_rect(pivot, pos)
            .map(|(tl, sz)| tl.x + sz.w + self.pad)
    }

    /// First clear pixel beyond a footprint half-extent: the one past the
    /// [`Self::pad`] band the mask stamps around the furniture.
    pub(super) const fn stand_clearance(&self) -> u16 {
        self.pad + 1
    }
}

/// Canonical seat approach: front + sides, exclude the back. Rotates with facing
/// so a north- or south-facing sofa each exclude their own back — for the couch,
/// seated facing North at the window, that resolves to {N, E, W}. A seat walled
/// in on ALL of those is un-sittable: `approach_point` returns the `pos` sentinel
/// and the wander SKIPS it, never entering over the backrest.
const SEAT_APPROACH: ApproachSides = ApproachSides {
    n: false,
    s: true,
    e: true,
    w: true,
};

/// Canonical bar-slot approach: behind + sides, never across the FRONT — the
/// mirror of [`SEAT_APPROACH`]. A bartender slot sits INSIDE the island body, so a
/// front (south, canonical) approach would glide visibly THROUGH the counter's
/// face; behind + lateral glides stay behind the countertop for the whole settle.
const BAR_APPROACH: ApproachSides = ApproachSides {
    n: true,
    s: false,
    e: true,
    w: true,
};

impl WaypointKind {
    /// Every variant, for exhaustive invariant tests. Order is not load-bearing.
    pub const ALL: &'static [WaypointKind] = &[
        WaypointKind::Couch,
        WaypointKind::Pantry,
        WaypointKind::PhoneBooth,
        WaypointKind::StandingDesk,
        WaypointKind::VendingMachine,
        WaypointKind::Printer,
        WaypointKind::MeetingSofa,
        WaypointKind::MeetingChair,
        WaypointKind::Island,
        WaypointKind::SnackShelf,
    ];

    /// This waypoint's geometry kind in the unified [`Furniture`] table. The
    /// waypoint enum carries only ROLE — a wander destination.
    pub(crate) const fn furniture(self) -> Furniture {
        match self {
            WaypointKind::Couch => Furniture::Couch,
            WaypointKind::Pantry => Furniture::Pantry,
            WaypointKind::PhoneBooth => Furniture::PhoneBooth,
            WaypointKind::StandingDesk => Furniture::StandingDesk,
            WaypointKind::VendingMachine => Furniture::VendingMachine,
            WaypointKind::Printer => Furniture::Printer,
            WaypointKind::MeetingSofa => Furniture::MeetingSofa,
            WaypointKind::MeetingChair => Furniture::MeetingChair,
            WaypointKind::Island => Furniture::IslandStand,
            WaypointKind::SnackShelf => Furniture::SnackShelf,
        }
    }
}

/// Every geometry-bearing furniture/decor KIND. This is the unification axis: the
/// role enums ([`WaypointKind`] = wander destination, [`PodDecor`] = aisle filler,
/// [`PlantKind`], [`WallDecor`]) each `.furniture()`-map onto these, so an item's
/// shape is defined exactly once no matter how many roles reference it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Furniture {
    /// The lounge / cubicle-top viewing couch (3 seats).
    Couch,
    /// The pantry counter (kitchen + coffee).
    Pantry,
    /// An aisle phone booth. Ground contact is the door/base; the column
    /// overhangs north (invariant #6) and hides a walker behind it by its own
    /// y-sort, while `stand_point` parks the USER clear of the full `visual`.
    PhoneBooth,
    /// An aisle standing desk (alternate workstation). Ground contact is the
    /// legs/base; the desktop overhangs north and occludes a walker behind it.
    StandingDesk,
    /// A corridor vending machine. Ground contact is its plinth; the cabinet
    /// overhangs north (invariant #6), like [`Furniture::PhoneBooth`].
    VendingMachine,
    /// A corridor floor-standing printer, grounded like
    /// [`Furniture::VendingMachine`].
    Printer,
    /// A meeting-room sofa SEAT (its body is [`Furniture::MeetingSofaBody`]).
    MeetingSofa,
    /// A meeting-room spot beside the table. Carries NO footprint deliberately:
    /// it has no furniture body to sit inside (unlike the island's in-body
    /// slots), so its cell stays walkable — blocking it would ripple
    /// mask/approach for a 7x7 body walkers can at worst visually clip.
    MeetingChair,
    /// A potted ficus (6px pot, overhanging canopy).
    PlantFicus,
    /// A tall potted plant (shares the ficus pot footprint, taller sprite).
    PlantTall,
    /// A small flowering plant — de-shared from `PLANT_FOOTPRINT` to a 2px
    /// terracotta pot at the sprite's south, the bloom overhanging it.
    PlantFlower,
    /// A low succulent — a 3px pot at the sprite's south, leaves overhanging it.
    PlantSucculent,
    /// A rolling whiteboard (aisle filler or wall decor). An ELEVATED obstacle:
    /// only the wheels/stand touch the floor, so the mask SOUTH-anchors the
    /// ground strip — a centered stamp lifts the block off the wheels.
    Whiteboard,
    /// A wall-mounted TV. Ground contact is the wide base; the monitor + mount
    /// column overhang north and occlude a walker behind the stand.
    Tv,
    /// A bookshelf. Its base dips below the window band into the room, so it
    /// needs a ground footprint or a walker clips through it; the shelves above
    /// overhang that base and sit in the already-blocked band.
    Bookshelf,
    /// A cork bulletin board. Truly wall-HUNG: no part touches the floor, so it
    /// carries no footprint and only `visual` is read.
    BulletinBoard,
    /// An exit sign (wall-hung, like [`Furniture::BulletinBoard`]).
    ExitSign,
    /// A meeting-room presentation screen on a soundbar base (bookshelf-class):
    /// the floor base blocks, the panel above overhangs it inside the
    /// already-blocked window band.
    MeetingScreen,
    /// The meeting-sofa BODY — the obstacle the mask stamps once per room (its
    /// seats are the [`Furniture::MeetingSofa`] rows). Its strip is CENTERED, not
    /// south-anchored: seat settle clearance and narrowest-room connectivity are
    /// both tuned to it sitting on the sofa pos.
    MeetingSofaBody,
    /// The meeting-room table body — `footprint == visual`, so the mask blocks
    /// exactly what is drawn.
    MeetingTable,
    /// The lounge floor lamp, whose footprint width is the base disc only, not
    /// the pole and its empty margins. CENTERED, and far taller than the 1px
    /// disc on purpose: from a centered stamp the tall footprint is what REACHES
    /// down to the disc at the sprite south, where `End` — or a shorter one —
    /// lifts the block off it.
    FloorLamp,
    /// The lounge side table (wood surface + magazine).
    LoungeSideTable,
    /// Kitchen-island BODY (the big center counter) — the obstacle the mask
    /// stamps once; the `IslandStand` rows are the stand slots around it. Its
    /// top rows are countertop OVERHANGING the south-anchored base (invariant #6).
    KitchenIsland,
    /// A standing slot at the kitchen island's edge (no obstacle of its own).
    /// Two shapes share it: the E/W FLANKS, pre-positioned CLEAR of the body's
    /// padded footprint, and the BARTENDER pair whose `pos` is INSIDE the body —
    /// a blocked pos is fine for `occupies_pos`, since A\* routes to a
    /// `BAR_APPROACH` cell and the settle glide bridges the rest.
    IslandStand,
    /// Snack shelf against the pantry's west wall — an approachable obstacle
    /// (vending-machine class): tall shelf sprite, shallow walk-behind base.
    /// `stand_point` delegates to `approach_point`, which finds the open side
    /// by reachability.
    SnackShelf,
    /// The agent's OWNED home workstation. Not a [`WaypointKind`] (per-agent,
    /// forced-seat when Active, never a wander destination) but a first-class
    /// geometry row, so desk and couch share ONE table and the same
    /// `seated_foot_cell` + approach/settle path. Its footprint is stamped
    /// TOP-LEFT in `mask.rs`, not centered — the full sprite width (the side
    /// cabinets touch the floor) × the shallow front-contact depth — and its
    /// `dwell` is the SEATED window (`pose::seated_dwell_ms`).
    Desk,
    /// Aquarium on a low cabinet against the north wall band. Pure decor: only
    /// the cabinet base blocks, the glass tank above it is visual overhang, and
    /// idle fish animate in the paint pass.
    FishTank,
    /// A home desk's task chair, seen from behind. Decor: its occupant's seat
    /// is the desk's, so it stamps nothing of its own.
    DeskChair,
    /// The filing cabinet beside a home desk. Decor, placed off its desk, so it
    /// stamps no ground of its own.
    FilingCabinet,
}

impl Furniture {
    /// Every variant — the iteration handle for the exhaustive row-invariant test.
    /// A new variant fails the `ALL.len()` count assert until it is listed here,
    /// so no row can slip in unverified.
    #[cfg(test)]
    pub(crate) const ALL: &'static [Furniture] = &[
        Furniture::Couch,
        Furniture::Pantry,
        Furniture::PhoneBooth,
        Furniture::StandingDesk,
        Furniture::VendingMachine,
        Furniture::Printer,
        Furniture::MeetingSofa,
        Furniture::MeetingChair,
        Furniture::PlantFicus,
        Furniture::PlantTall,
        Furniture::PlantFlower,
        Furniture::PlantSucculent,
        Furniture::Whiteboard,
        Furniture::Tv,
        Furniture::Bookshelf,
        Furniture::BulletinBoard,
        Furniture::ExitSign,
        Furniture::MeetingScreen,
        Furniture::MeetingSofaBody,
        Furniture::MeetingTable,
        Furniture::FloorLamp,
        Furniture::LoungeSideTable,
        Furniture::KitchenIsland,
        Furniture::IslandStand,
        Furniture::SnackShelf,
        Furniture::Desk,
        Furniture::FishTank,
        Furniture::DeskChair,
        Furniture::FilingCabinet,
    ];
}

/// Whether a scatter plant must keep clearance from this kind when it appears as
/// a NON-waypoint singleton — the plant-obstacle census's per-kind authority,
/// which `plant_obstacle_rects` filters every singleton by. An EXHAUSTIVE match,
/// no `_`: curating this set by OMISSION let bodies ship with no repulsion and
/// plants grew through them, so a new [`Furniture`] kind must declare a stance.
pub(crate) const fn repels_plants(kind: Furniture) -> bool {
    match kind {
        // Solid non-waypoint singleton BODIES a scatter plant routes around.
        Furniture::FishTank
        | Furniture::MeetingSofaBody
        | Furniture::MeetingTable
        | Furniture::KitchenIsland
        | Furniture::FloorLamp
        | Furniture::LoungeSideTable => true,
        // Never a non-waypoint singleton in the plant census — waypoint furniture
        // is repelled via `first_blocking_waypoint` instead.
        Furniture::Couch
        | Furniture::Pantry
        | Furniture::PhoneBooth
        | Furniture::StandingDesk
        | Furniture::VendingMachine
        | Furniture::Printer
        | Furniture::MeetingSofa
        | Furniture::MeetingChair
        | Furniture::PlantFicus
        | Furniture::PlantTall
        | Furniture::PlantFlower
        | Furniture::PlantSucculent
        | Furniture::Whiteboard
        | Furniture::Tv
        | Furniture::Bookshelf
        | Furniture::BulletinBoard
        | Furniture::ExitSign
        | Furniture::MeetingScreen
        | Furniture::IslandStand
        | Furniture::SnackShelf
        | Furniture::Desk
        | Furniture::DeskChair
        | Furniture::FilingCabinet => false,
    }
}

/// THE furniture table — one row per [`Furniture`] kind, the **single** source of
/// truth for ground shape, sprite size, occupancy, dwell and approach. Every
/// geometric dependent derives from a row; never re-type these numbers elsewhere.
pub const fn furniture_def(kind: Furniture) -> FurnitureDef {
    // Not a wander destination: no dwell, and the approach set is unused (decor
    // never runs stand_point). `ground_y: End` suits the `..DECOR` overhang rows.
    const DECOR: FurnitureDef = FurnitureDef {
        pad: TIGHT_PAD_PX,
        footprint: None,
        visual: Size { w: 0, h: 0 },
        occupies_pos: false,
        exclusive: false,
        dwell: DwellWindow::DECOR,
        approach: ApproachSides::ALL,
        ground_x: GroundAlign::Center,
        ground_y: GroundAlign::End,
    };
    match kind {
        Furniture::Couch => FurnitureDef {
            pad: TIGHT_PAD_PX,
            footprint: Some(Size { w: 8, h: 7 }),
            // One seat's box: the lounge couch's three seats overlap across
            // the `MeetingSofaBody` sprite it blits.
            visual: Size { w: 8, h: 7 },
            occupies_pos: true,
            exclusive: true,
            dwell: DwellWindow {
                base_ms: 20_000,
                range_ms: 20_000,
            },
            approach: SEAT_APPROACH,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::Center,
        },
        Furniture::Pantry => FurnitureDef {
            pad: TIGHT_PAD_PX,
            footprint: None,
            visual: Size { w: 0, h: 0 }, // runtime-sized: `PantryRoom::counter_size`
            occupies_pos: false,
            exclusive: false,
            dwell: DwellWindow {
                base_ms: 10_000,
                range_ms: 8_000,
            },
            approach: ApproachSides::ALL,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::Center,
        },
        Furniture::PhoneBooth => FurnitureDef {
            pad: TIGHT_PAD_PX,
            footprint: Some(Size { w: 7, h: 3 }),
            visual: Size { w: 7, h: 15 },
            occupies_pos: false,
            exclusive: true,
            dwell: DwellWindow {
                base_ms: 8_000,
                range_ms: 22_000,
            },
            approach: ApproachSides::ALL,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::End,
        },
        Furniture::StandingDesk => FurnitureDef {
            pad: TIGHT_PAD_PX,
            footprint: Some(Size { w: 8, h: 3 }),
            visual: Size { w: 8, h: 8 },
            occupies_pos: false,
            exclusive: true,
            dwell: DwellWindow {
                base_ms: 8_000,
                range_ms: 22_000,
            },
            approach: ApproachSides::ALL,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::End,
        },
        Furniture::VendingMachine => FurnitureDef {
            pad: TIGHT_PAD_PX,
            footprint: Some(Size { w: 7, h: 3 }),
            visual: Size { w: 7, h: 13 },
            occupies_pos: false,
            exclusive: false,
            dwell: DwellWindow {
                base_ms: 4_000,
                range_ms: 4_000,
            },
            approach: ApproachSides::ALL,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::End,
        },
        Furniture::Printer => FurnitureDef {
            pad: TIGHT_PAD_PX,
            footprint: Some(Size { w: 7, h: 3 }),
            visual: Size { w: 7, h: 8 },
            occupies_pos: false,
            exclusive: false,
            dwell: DwellWindow {
                base_ms: 4_000,
                range_ms: 4_000,
            },
            approach: ApproachSides::ALL,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::End,
        },
        Furniture::MeetingSofa => FurnitureDef {
            pad: TIGHT_PAD_PX,
            footprint: None,
            visual: Size { w: 0, h: 0 },
            occupies_pos: true,
            exclusive: true,
            dwell: DwellWindow {
                base_ms: 20_000,
                range_ms: 20_000,
            },
            approach: SEAT_APPROACH,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::Center,
        },
        Furniture::MeetingChair => FurnitureDef {
            pad: TIGHT_PAD_PX,
            footprint: None,
            visual: Size { w: 7, h: 7 },
            occupies_pos: true,
            exclusive: true,
            dwell: DwellWindow {
                base_ms: 20_000,
                range_ms: 20_000,
            },
            approach: SEAT_APPROACH,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::Center,
        },
        Furniture::PlantFicus => FurnitureDef {
            footprint: Some(PLANT_FOOTPRINT),
            visual: Size { w: 6, h: 7 },
            ..DECOR
        },
        Furniture::PlantTall => FurnitureDef {
            footprint: Some(PLANT_FOOTPRINT),
            visual: Size { w: 6, h: 10 },
            ..DECOR
        },
        Furniture::PlantFlower => FurnitureDef {
            footprint: Some(Size { w: 2, h: 2 }),
            visual: Size { w: 6, h: 6 },
            ..DECOR
        },
        Furniture::PlantSucculent => FurnitureDef {
            footprint: Some(Size { w: 3, h: 2 }),
            visual: Size { w: 5, h: 4 },
            ..DECOR
        },
        Furniture::Whiteboard => FurnitureDef {
            footprint: Some(Size { w: 10, h: 3 }),
            visual: Size { w: 14, h: 11 },
            ..DECOR
        },
        Furniture::Tv => FurnitureDef {
            footprint: Some(Size { w: 6, h: 2 }),
            visual: Size { w: 10, h: 10 },
            ..DECOR
        },
        Furniture::Bookshelf => FurnitureDef {
            footprint: Some(Size { w: 8, h: 3 }),
            visual: Size { w: 8, h: 12 },
            ..DECOR
        },
        Furniture::BulletinBoard => FurnitureDef {
            visual: Size { w: 10, h: 6 },
            ..DECOR
        },
        Furniture::ExitSign => FurnitureDef {
            visual: Size { w: 5, h: 3 },
            ..DECOR
        },
        Furniture::DeskChair => FurnitureDef {
            visual: Size { w: 8, h: 5 },
            ..DECOR
        },
        Furniture::FilingCabinet => FurnitureDef {
            visual: Size { w: 4, h: 6 },
            ..DECOR
        },
        Furniture::MeetingScreen => FurnitureDef {
            footprint: Some(Size { w: 14, h: 3 }),
            visual: Size { w: 14, h: 12 },
            ..DECOR
        },
        Furniture::MeetingSofaBody => {
            const VISUAL: Size = Size { w: 20, h: 7 };
            const PAD: u16 = OBSTACLE_PAD_PX;
            FurnitureDef {
                footprint: Some(Size {
                    // Narrower than the sprite ON PURPOSE: a full-width footprint plus
                    // pad disconnects the narrowest meeting room.
                    w: VISUAL.w - 2 * PAD,
                    h: VISUAL.h - 2 * PAD,
                }),
                visual: VISUAL,
                pad: PAD,
                ground_y: GroundAlign::Center,
                ..DECOR
            }
        }
        Furniture::MeetingTable => FurnitureDef {
            footprint: Some(Size { w: 11, h: 5 }),
            visual: Size { w: 11, h: 5 },
            pad: OBSTACLE_PAD_PX,
            ..DECOR
        },
        Furniture::KitchenIsland => FurnitureDef {
            footprint: Some(Size { w: 20, h: 5 }),
            visual: Size { w: 20, h: 7 },
            pad: OBSTACLE_PAD_PX,
            ..DECOR
        },
        Furniture::IslandStand => FurnitureDef {
            pad: TIGHT_PAD_PX,
            footprint: None,
            visual: Size { w: 0, h: 0 },
            occupies_pos: true,
            exclusive: true,
            dwell: DwellWindow {
                base_ms: 9_000,
                range_ms: 9_000,
            },
            approach: BAR_APPROACH,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::Center,
        },
        Furniture::SnackShelf => FurnitureDef {
            pad: TIGHT_PAD_PX,
            footprint: Some(Size { w: 7, h: 2 }),
            visual: Size { w: 7, h: 10 },
            occupies_pos: false,
            exclusive: false,
            dwell: DwellWindow {
                base_ms: 5_000,
                range_ms: 5_000,
            },
            approach: ApproachSides::ALL,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::End,
        },
        Furniture::FloorLamp => FurnitureDef {
            footprint: Some(Size { w: 2, h: 9 }),
            visual: Size { w: 4, h: 12 },
            ground_y: GroundAlign::Center,
            ..DECOR
        },
        Furniture::LoungeSideTable => FurnitureDef {
            footprint: Some(Size { w: 7, h: 4 }),
            visual: Size { w: 7, h: 4 },
            ..DECOR
        },
        Furniture::FishTank => FurnitureDef {
            footprint: Some(Size { w: 14, h: 3 }),
            visual: Size { w: 14, h: 11 },
            ..DECOR
        },
        Furniture::Desk => FurnitureDef {
            pad: OBSTACLE_PAD_PX,
            footprint: Some(Size {
                w: DESK_W + 4,
                h: DESK_FOOT_H,
            }),
            visual: Size {
                w: DESK_W + 4,
                h: DESK_H + 2,
            },
            occupies_pos: true,
            exclusive: true,
            dwell: DwellWindow {
                base_ms: 15_000,
                range_ms: 15_000,
            },
            approach: DESK_APPROACH,
            ground_x: GroundAlign::Center,
            ground_y: GroundAlign::End,
        },
    }
}

/// Where a footprint sits inside its VISUAL box on ONE axis. Each variant declares
/// INTENT and resolves its offset from `visual − footprint` at stamp time, so it
/// can NEVER drift when a sprite is resized — the point of this type over a `dx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroundAlign {
    /// Flush to the box's LOW edge — North (y) / West (x): offset 0.
    Start,
    /// Centered ON the sprite center (== the placement `pos` for a `Center` pivot);
    /// [`Self::offset`] carries the parity rule that makes it exact.
    Center,
    /// Flush to the box's HIGH edge — South (y) / East (x). THE walk-behind shape
    /// (invariant #6): a walker parks behind the overhang, occluded by the y-sort.
    End,
}

impl GroundAlign {
    /// The pixel offset from the visual box's low edge for a `footprint`-long span
    /// inside a `visual`-long box. `Center` is `floor(v/2) − floor(f/2)`, NOT
    /// `floor((v−f)/2)` — the two differ by 1px at opposite parity, which lifts the
    /// floor lamp's block off its disc. Needs `footprint <= visual`, which
    /// `furniture_def_invariants_hold_for_every_row` pins for every row.
    pub(crate) const fn offset(self, visual: u16, footprint: u16) -> u16 {
        match self {
            GroundAlign::Start => 0,
            GroundAlign::Center => visual / 2 - footprint / 2,
            GroundAlign::End => visual - footprint,
        }
    }
}

/// The desk's blocked-GROUND width — the full sprite width (side cabinets
/// included), read from the ONE table row so the pod-grid's band-EDGE clamps
/// (`compute.rs`) price the honest ground, not the `DESK_W` SLOT width. The two
/// diverge by the side-cabinet overhang that rides the aisle, and a clamp on
/// `DESK_W` lets a desk's ground poke past the buffer edge.
pub(crate) const DESK_GROUND_W: u16 = match desk_furniture_def().footprint {
    Some(s) => s.w,
    None => panic!("Desk must carry a static footprint"),
};

/// The desk's blocked-GROUND SOUTH edge from its NW corner — the Y twin of
/// [`DESK_GROUND_W`], deliberately NOT the footprint HEIGHT: `ground_y: End`
/// anchors the shallow `DESK_FOOT_H` strip to the sprite BASE, so its south edge
/// sits the full VISUAL height below the corner. `compute.rs`'s `desk_y_max`
/// clamps on THIS so a bottom-row desk's ground can't spill into the aisle.
pub(crate) const DESK_GROUND_H: u16 = match desk_furniture_def().footprint {
    Some(fp) => {
        let def = desk_furniture_def();
        def.ground_y.offset(def.visual.h, fp.h) + fp.h
    }
    None => panic!("Desk must carry a static footprint"),
};

/// The **home desk** descriptor — sugar over the [`Furniture::Desk`] table row,
/// kept because the desk is per-agent rather than a `WaypointKind`.
pub(crate) const fn desk_furniture_def() -> FurnitureDef {
    furniture_def(Furniture::Desk)
}

/// Vertical offset baked into the walking / waypoint sprite top-left
/// (`p.y - WALKING_Y_OFF`) — the standing sprite height, owned here so
/// [`seated_foot_cell`] and the top-left invert each other by construction.
pub(crate) const WALKING_Y_OFF: u16 = 12;
/// Vertical offset of the back-view seat sprite top-left (`pos.y - SEAT_RENDER_Y_OFF`).
/// The seat's settle cell is `WALKING_Y_OFF - SEAT_RENDER_Y_OFF` px south of `pos`,
/// where `walking_top_left` lands exactly on `back_couch_top_left`.
pub(crate) const SEAT_RENDER_Y_OFF: u16 = 7;

/// How far south of their seat a sitter on seated furniture (couch, sofa,
/// meeting chair) sorts.
const SEATED_Z_OFF: u16 = 2;

/// The depth a sitter on seated furniture at `seat` sorts at, which the
/// furniture under them keys its own depth from.
pub(crate) fn seated_sort_row(seat: Point) -> u16 {
    seat.y + SEATED_Z_OFF
}

/// Y offset from a home desk's top-left to the agent's WALK anchor — how far south
/// of the desk origin a FAR seat sits (a near one takes `DESK_WALK_Y_OFF_BACK`).
/// The no-arrival-pop identity holds structurally now that sprite and walk both
/// route through `Seat::render_top_left`, and
/// `desk_walk_anchor_settles_exactly_on_the_seat` still pins it.
pub(crate) const DESK_WALK_Y_OFF: u16 = 4;

/// A point packed into one hash input, so a per-spot seed is derived the same
/// way everywhere it is needed. The PACKING is load-bearing, not an
/// implementation detail: every committed still was rendered against it, and
/// swapping the halves re-seeds every seat and pot while every property test
/// still passes — pinned by `point_seed_packing_is_frozen`.
pub(super) fn point_seed(p: Point) -> u64 {
    (u64::from(p.x) << 32) | u64::from(p.y)
}

/// Half the seeded nudge's span. The chair is `CHARACTER_SPRITE_W` on a
/// `visual.w` desk, so centring leaves 3 px a side and ±2 keeps a pixel of desk
/// under each edge.
const SEAT_NUDGE_PX: i32 = 2;

/// The seat CENTRE's x — the ONE value the chair, its occupant and the walk that
/// ends there all derive from, the way a meeting seat derives from `wp.pos`. It
/// centres on the PAINTED desk, whose sprite is wider than `DESK_W`, and carries
/// a per-desk nudge so a pod does not read as a stamped grid.
fn seat_center_x(desk: Point) -> u16 {
    let base = i32::from(desk.x) + i32::from(desk_furniture_def().visual.w) / 2;
    let seed = point_seed(desk);
    let span = SEAT_NUDGE_PX * 2 + 1;
    let nudge = (pixtuoid_core::id::splitmix64(seed) % span as u64) as i32 - SEAT_NUDGE_PX;
    base.saturating_add(nudge).max(0) as u16
}

pub(crate) const DESK_WALK_Y_OFF_BACK: u16 = WALKING_Y_OFF;

// Below `WALKING_Y_OFF`, `seated_top_left`'s `saturating_sub` clamps and the sitter's sprite lands off its chair.
const _: () = assert!(DESK_WALK_Y_OFF_BACK >= WALKING_Y_OFF);

/// Where an agent walks to/from for its home `desk` (`East`/`West` never occur, and take the `South` arrangement).
pub(crate) fn desk_walk_anchor_facing(desk: Point, facing: Facing) -> Point {
    let y_off = match facing {
        Facing::North => DESK_WALK_Y_OFF_BACK,
        Facing::South | Facing::East | Facing::West => DESK_WALK_Y_OFF,
    };
    Point {
        x: seat_center_x(desk),
        y: desk.y + y_off,
    }
}

/// The cell where a seated agent's WALK visually ends so the seated sprite renders
/// with no arrival jump — the inverse of the render top-left under
/// [`WALKING_Y_OFF`], solving `walking_top_left(S) == render_top_left(pos)`. `Some`
/// for every `occupies_pos` furniture; `None` for obstacles, whose sprite renders
/// AT the approach cell. The post-A\* settle walks `approach_point → S`, and a
/// blocked `S` (meeting sofa, desk) makes that leg the "sit down" motion rather
/// than pathfinding. The `Desk` arm hardcodes `Facing::South` — a future caller
/// must pass the facing.
pub(crate) fn seated_foot_cell(kind: Furniture, pos: Point) -> Option<Point> {
    if !furniture_def(kind).occupies_pos {
        return None;
    }
    Some(match kind {
        Furniture::Couch | Furniture::MeetingSofa | Furniture::MeetingChair => Point {
            x: pos.x,
            y: pos.y + (WALKING_Y_OFF - SEAT_RENDER_Y_OFF),
        },
        Furniture::IslandStand => pos,
        Furniture::Desk => desk_walk_anchor_facing(pos, crate::layout::Facing::South),
        _ => unreachable!(
            "{kind:?} sets occupies_pos but lacks a seated_foot_cell arm — settling \
             silently would put its occupant on the blocked furniture centre"
        ),
    })
}

/// Which way a waypoint occupant faces. Drives sprite choice (back vs front view)
/// and horizontal mirroring at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Facing {
    /// Facing north (−y, toward the far wall) — a back view.
    North,
    /// Facing south (+y, toward the viewer) — a front view.
    South,
    /// Facing east (+x, right).
    East,
    /// Facing west (−x, left).
    West,
}

/// Wall-mounted / wall-leaning furniture, painted as decor in the top wall area.
/// Not a wander destination — agents can't walk through their own cubicle row to
/// reach the back wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WallDecor {
    /// A bookshelf against the back wall.
    Bookshelf,
    /// A wall-mounted whiteboard.
    Whiteboard,
    /// A cork bulletin board.
    BulletinBoard,
    /// An exit sign.
    ExitSign,
    /// Wall-mounted meeting-room display.
    MeetingScreen,
}

impl WallDecor {
    /// Every variant.
    #[cfg(test)]
    pub(crate) const ALL: &'static [WallDecor] = &[
        WallDecor::Bookshelf,
        WallDecor::Whiteboard,
        WallDecor::BulletinBoard,
        WallDecor::ExitSign,
        WallDecor::MeetingScreen,
    ];

    /// Geometry kind in the unified [`Furniture`] table.
    pub(crate) const fn furniture(self) -> Furniture {
        match self {
            WallDecor::Whiteboard => Furniture::Whiteboard,
            WallDecor::Bookshelf => Furniture::Bookshelf,
            WallDecor::BulletinBoard => Furniture::BulletinBoard,
            WallDecor::ExitSign => Furniture::ExitSign,
            WallDecor::MeetingScreen => Furniture::MeetingScreen,
        }
    }

    /// Whether it stands on the floor rather than hanging on the wall: it has a
    /// footprint, which the mask stamps and a sealed lane can drop it for.
    pub(crate) fn stands_on_floor(self) -> bool {
        furniture_def(self.furniture()).footprint.is_some()
    }

    /// The piece it draws: here, so a new variant is a compile error HERE,
    /// not a forgotten call-site match arm.
    pub(crate) const fn piece(self) -> Piece {
        match self {
            WallDecor::Bookshelf => Piece::Bookshelf,
            WallDecor::Whiteboard => Piece::Whiteboard,
            WallDecor::BulletinBoard => Piece::BulletinBoard,
            WallDecor::ExitSign => Piece::ExitSign,
            WallDecor::MeetingScreen => Piece::MeetingScreen,
        }
    }
}

/// Variety of potted plants — each renders a different sprite, so the lounge
/// doesn't read as one ficus repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlantKind {
    /// A leafy ficus in a 6px pot.
    Ficus,
    /// A tall plant (shares the ficus pot footprint).
    Tall,
    /// A small flowering plant (2×2 pot).
    Flower,
    /// A low succulent (3×2 pot).
    Succulent,
}

impl PlantKind {
    /// Every variant.
    #[cfg(test)]
    pub(crate) const ALL: &'static [PlantKind] = &[
        PlantKind::Ficus,
        PlantKind::Tall,
        PlantKind::Flower,
        PlantKind::Succulent,
    ];

    /// Geometry kind in the unified [`Furniture`] table.
    pub(crate) const fn furniture(self) -> Furniture {
        match self {
            PlantKind::Ficus => Furniture::PlantFicus,
            PlantKind::Tall => Furniture::PlantTall,
            PlantKind::Flower => Furniture::PlantFlower,
            PlantKind::Succulent => Furniture::PlantSucculent,
        }
    }

    /// The piece it draws.
    pub(crate) const fn piece(self) -> Piece {
        match self {
            PlantKind::Ficus => Piece::Plant,
            PlantKind::Tall => Piece::PlantTall,
            PlantKind::Flower => Piece::PlantFlower,
            PlantKind::Succulent => Piece::PlantSucculent,
        }
    }
}

/// Decor placed in the aisles BETWEEN 2×2 desk pods. Each slot draws from a
/// per-floor shuffled bag (`compute::decor_for_slot`), so a floor sees every kind
/// before any repeats and no two neighbouring aisles share one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PodDecor {
    /// A tall plant filling the aisle.
    PlantTall,
    /// A rolling whiteboard in the aisle.
    Whiteboard,
    /// A wheeled TV cart.
    Tv,
    /// A phone booth.
    PhoneBooth,
    /// A standing desk.
    StandingDesk,
}

impl PodDecor {
    /// The randomly-picked pool. Every member's GROUND footprint has to fit the
    /// aisle width once its row's [`FurnitureDef::pad`] is added.
    pub(crate) const ALL: &'static [PodDecor] = &[
        PodDecor::PlantTall,
        PodDecor::Whiteboard,
        PodDecor::Tv,
        PodDecor::PhoneBooth,
        PodDecor::StandingDesk,
    ];

    /// Geometry kind in the unified [`Furniture`] table. PlantTall resolves to the
    /// SAME row as the free-standing `PlantKind::Tall`, and PhoneBooth/StandingDesk
    /// to the same rows as their `WaypointKind` twins, so nothing drifts.
    pub(crate) const fn furniture(self) -> Furniture {
        match self {
            PodDecor::PlantTall => Furniture::PlantTall,
            PodDecor::Whiteboard => Furniture::Whiteboard,
            PodDecor::Tv => Furniture::Tv,
            PodDecor::PhoneBooth => Furniture::PhoneBooth,
            PodDecor::StandingDesk => Furniture::StandingDesk,
        }
    }

    /// The waypoint this decor also is, or `None` for pure decor. Exhaustive (no
    /// `_`): a new `PodDecor` makes a deliberate wander decision here.
    pub(crate) const fn waypoint(self) -> Option<WaypointKind> {
        match self {
            PodDecor::PhoneBooth => Some(WaypointKind::PhoneBooth),
            PodDecor::StandingDesk => Some(WaypointKind::StandingDesk),
            PodDecor::PlantTall | PodDecor::Whiteboard | PodDecor::Tv => None,
        }
    }

    /// The piece it draws.
    pub(crate) const fn piece(self) -> Piece {
        match self {
            PodDecor::PlantTall => Piece::PlantTall,
            PodDecor::Whiteboard => Piece::Whiteboard,
            PodDecor::Tv => Piece::TvStand,
            PodDecor::PhoneBooth => Piece::PhoneBooth,
            PodDecor::StandingDesk => Piece::StandingDesk,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: (i32, i32) = (0, -1);
    const S: (i32, i32) = (0, 1);
    const E: (i32, i32) = (1, 0);
    const W: (i32, i32) = (-1, 0);

    #[test]
    fn repels_plants_is_exactly_the_solid_non_waypoint_bodies() {
        for k in [
            Furniture::FishTank,
            Furniture::MeetingSofaBody,
            Furniture::MeetingTable,
            Furniture::KitchenIsland,
            Furniture::FloorLamp,
            Furniture::LoungeSideTable,
        ] {
            assert!(
                repels_plants(k),
                "{k:?} is a solid body — must repel plants"
            );
        }
        assert_eq!(
            Furniture::ALL.iter().filter(|&&k| repels_plants(k)).count(),
            6,
            "exactly six kinds repel — a new `true` must be deliberate"
        );
    }

    fn allowed(sides: ApproachSides, facing: Facing) -> Vec<(i32, i32)> {
        [N, S, E, W]
            .into_iter()
            .filter(|&d| sides.allows(facing, d))
            .collect()
    }

    #[test]
    fn all_allows_every_side_for_any_facing() {
        for facing in [Facing::North, Facing::South, Facing::East, Facing::West] {
            assert_eq!(allowed(ApproachSides::ALL, facing), vec![N, S, E, W]);
        }
    }

    #[test]
    fn seat_facing_south_allows_front_and_sides_not_back() {
        assert_eq!(allowed(SEAT_APPROACH, Facing::South), vec![S, E, W]);
    }

    #[test]
    fn seat_facing_north_rotates_to_exclude_the_south_back() {
        assert_eq!(allowed(SEAT_APPROACH, Facing::North), vec![N, E, W]);
    }

    #[test]
    fn desk_excludes_its_south_front() {
        assert_eq!(allowed(DESK_APPROACH, Facing::South), vec![N, E, W]);
        let no_east = ApproachSides {
            e: false,
            ..DESK_APPROACH
        };
        assert_eq!(allowed(no_east, Facing::South), vec![N, W]);
    }

    #[test]
    fn rotation_is_a_bijection_on_sides() {
        for facing in [Facing::North, Facing::South, Facing::East, Facing::West] {
            for one in [N, S, E, W] {
                let sides = ApproachSides {
                    n: one == N,
                    s: one == S,
                    e: one == E,
                    w: one == W,
                };
                assert_eq!(
                    allowed(sides, facing).len(),
                    1,
                    "facing {facing:?}, side {one:?} must rotate to exactly one side",
                );
            }
        }
    }

    #[test]
    fn the_desk_row_uses_the_desk_approach_policy() {
        assert_eq!(
            furniture_def(Furniture::Desk).approach,
            DESK_APPROACH,
            "desk uses the editable DESK_APPROACH policy"
        );
    }

    #[test]
    fn ground_align_stays_inside_the_visual_and_follows_the_declared_intent() {
        for &kind in Furniture::ALL {
            let def = furniture_def(kind);
            let Some(fp) = def.footprint else {
                continue; // no static ground
            };
            let dx = def.ground_x.offset(def.visual.w, fp.w);
            let dy = def.ground_y.offset(def.visual.h, fp.h);
            assert!(
                dx + fp.w <= def.visual.w && dy + fp.h <= def.visual.h,
                "{kind:?}: blocked rect must not poke past the visual box"
            );
            let center_exception =
                matches!(kind, Furniture::MeetingSofaBody | Furniture::FloorLamp);
            if def.visual.h > fp.h && !center_exception {
                assert_eq!(
                    def.ground_y,
                    GroundAlign::End,
                    "{kind:?}: an overhang row must south-anchor (End) unless documented"
                );
            }
            assert_eq!(
                def.ground_x,
                GroundAlign::Center,
                "{kind:?}: first non-Center ground_x — update the ground_x field doc"
            );
        }
    }

    #[test]
    fn furniture_def_invariants_hold_for_every_row() {
        assert_eq!(
            Furniture::ALL.len(),
            29,
            "Furniture variant added/removed — update ALL (and this count)"
        );
        for &f in Furniture::ALL {
            let d = furniture_def(f);
            assert!(
                d.dwell == DwellWindow::DECOR || d.dwell.range_ms > 0,
                "{f:?}: half-broken dwell {:?}",
                d.dwell
            );
            let expect_occupies = matches!(
                f,
                Furniture::Couch
                    | Furniture::MeetingSofa
                    | Furniture::MeetingChair
                    | Furniture::IslandStand
                    | Furniture::Desk
            );
            assert_eq!(d.occupies_pos, expect_occupies, "{f:?}: occupies_pos");
            let expect_exclusive =
                expect_occupies || matches!(f, Furniture::PhoneBooth | Furniture::StandingDesk);
            assert_eq!(d.exclusive, expect_exclusive, "{f:?}: exclusive");
            assert!(
                !d.occupies_pos || d.exclusive,
                "{f:?}: occupies_pos implies exclusive"
            );
            if matches!(
                f,
                Furniture::MeetingSofa | Furniture::MeetingChair | Furniture::IslandStand
            ) {
                assert!(
                    d.footprint.is_none(),
                    "{f:?}: seat row must carry no footprint"
                );
            }
            if matches!(f, Furniture::PlantFicus | Furniture::PlantTall) {
                assert_eq!(
                    d.footprint,
                    Some(PLANT_FOOTPRINT),
                    "{f:?}: plant ground footprint"
                );
            }
            if matches!(f, Furniture::PlantFlower | Furniture::PlantSucculent) {
                assert!(
                    d.footprint.is_some_and(|s| s.w < PLANT_FOOTPRINT.w),
                    "{f:?}: de-shared plant must be narrower than PLANT_FOOTPRINT"
                );
            }
            if let Some(Size { w: fw, h: fh }) = d.footprint {
                assert!(
                    fw <= d.visual.w && fh <= d.visual.h,
                    "{f:?}: footprint {:?} exceeds visual {:?} (invariant #6)",
                    d.footprint,
                    d.visual
                );
            }
            // Not in `PodDecor::ALL`, so `every_pod_occludes_via_overhang` never sees
            // the de-shared pair: the `≤` above is their only other, too-weak check.
            if matches!(
                f,
                Furniture::PlantFicus
                    | Furniture::PlantTall
                    | Furniture::PlantFlower
                    | Furniture::PlantSucculent
            ) {
                let Size { h: fh, .. } = d.footprint.expect("plant has a pot footprint");
                assert!(
                    d.visual.h > fh,
                    "{f:?}: plant must overhang its pot to occlude (visual.h {} > footprint.h {fh})",
                    d.visual.h
                );
            }
        }
    }
}
