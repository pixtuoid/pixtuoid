//! Zone-based scene layout for the top-down office — primitive geometry
//! only, no terminal deps.
//!
//! Splits a buf-pixel rectangle into quadrants (meeting / pantry /
//! cubicles / lounge), then computes per-agent home desks, named lounge
//! waypoints, decor positions, and a per-pixel walkability mask.

mod approach;
mod coarse;
mod compute;
mod decor;
mod mask;
mod placement;
mod reach;
mod rooms;
pub(crate) mod roster;
mod windows;

// The deep interface is `SceneLayout::{stand_point,approach_point}`; these free
// fns stay for this crate's own synthetic-mask unit tests.
pub(crate) use approach::{approach_point, first_reachable_on_side, stand_point};
pub(crate) use compute::PANTRY_COUNTER_LARGE_W;
pub use compute::min_layout_size;
pub(crate) use decor::{
    DwellWindow, SEAT_RENDER_Y_OFF, WALKING_Y_OFF, desk_furniture_def, desk_walk_anchor_facing,
    seated_foot_cell,
};
pub use decor::{
    Facing, Furniture, FurnitureDef, PlantKind, PodDecor, WallDecor, WaypointKind, furniture_def,
};
pub(crate) use decor::{repels_plants, seated_sort_row};
pub(crate) use placement::{Pivot, anchored_top_left, sort_row_at};
pub(crate) use reach::ReachSet;
pub(crate) use rooms::meeting::{COAT_HOOK_DX, COAT_RACK_BASE_DY, COAT_W, coat_rack_rect_at};
pub(crate) use rooms::pantry::{
    COMPACT_COUNTER, LARGE_COUNTER, PANTRY_COUNTER_ANIMS, pantry_counter_anim,
};
pub(crate) use rooms::walls::Doorway;
pub(crate) use rooms::walls::{WALL_THICK_H, WALL_THICK_V, WallPiece};
pub use rooms::{MeetingRoom, MeetingTrio, PantryRoom};
pub use roster::Station;
pub(crate) use roster::StationArt;
pub(crate) use roster::{
    CLOCK, Depth, Fixture, NEON_PANEL, NEON_PANEL_BORDER, Tie, desk_chair_fixtures,
    desk_chair_sort_row, desk_chair_top_left, desk_fixtures, pod_decor_fixtures,
};
pub use roster::{
    FixtureKind, NEON_PANEL_INNER_H, NEON_PANEL_INNER_W, NEON_PANEL_INNER_X, NEON_PANEL_INNER_Y,
    NEON_PANEL_W,
};
#[cfg(test)]
pub(crate) use roster::{NEON_PANEL_H, coffee_machine_cols};
// Painter tests tile walls no `SceneLayout` has.
pub(crate) use windows::{
    NEON_DOOR_WALL_W, WINDOW_TOP, WindowBay, door_x, glass_rows, wall_trim_row, window_frame,
    window_posts, window_rows, window_run,
};
#[cfg(test)]
pub(crate) use windows::{WINDOW_W, window_bays, window_slots};
// `crate::pathfind`'s A* and `reach`'s BFS both ride these ONE definitions.
pub(crate) use coarse::{
    COARSE_CELL_SIZE, CoarseGrid, cell_anchor, cell_center, cell_walkable, snap, snap_where,
};

use pixtuoid_core::state::FloorLocalDeskIndex;
use pixtuoid_core::walkable::WalkableMask;

/// Primitive rectangle — same shape as `ratatui::layout::Rect` so the binary
/// converts field-by-field without core paying for the ratatui dep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bounds {
    /// Left edge x, in buffer pixels.
    pub x: u16,
    /// Top edge y, in buffer pixels.
    pub y: u16,
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
}

impl Bounds {
    /// Whether the two half-open boxes share a pixel. A zero-sized box shares
    /// none.
    pub(crate) fn overlaps(self, other: Bounds) -> bool {
        // u32: a box's exclusive end can lie one past `u16::MAX`.
        let end = |at: u16, len: u16| u32::from(at) + u32::from(len);
        self.width > 0
            && self.height > 0
            && other.width > 0
            && other.height > 0
            && u32::from(self.x) < end(other.x, other.width)
            && u32::from(other.x) < end(self.x, self.width)
            && u32::from(self.y) < end(other.y, other.height)
            && u32::from(other.y) < end(self.y, self.height)
    }

    /// The least box holding both, its ends taken in u32 as [`Self::overlaps`]
    /// takes them, and its size clamped to `u16` (two boxes on one buffer
    /// never need more).
    pub(crate) fn union(self, other: Bounds) -> Bounds {
        let end = |at: u16, len: u16| u32::from(at) + u32::from(len);
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        let span = |from: u16, to: u32| u16::try_from(to - u32::from(from)).unwrap_or(u16::MAX);
        Bounds {
            x,
            y,
            width: span(x, end(self.x, self.width).max(end(other.x, other.width))),
            height: span(y, end(self.y, self.height).max(end(other.y, other.height))),
        }
    }

    /// Whether the two half-open boxes share a column, whatever their rows.
    pub(crate) fn shares_columns(self, other: Bounds) -> bool {
        Bounds {
            y: 0,
            height: 1,
            ..self
        }
        .overlaps(Bounds {
            y: 0,
            height: 1,
            ..other
        })
    }

    /// The box grown `dx` columns on each side, its west edge clamped at
    /// column 0 and its east edge kept.
    pub(crate) fn widened(self, dx: u16) -> Bounds {
        let x = self.x.saturating_sub(dx);
        Bounds {
            x,
            width: self.x + self.width + dx - x,
            ..self
        }
    }
}

/// A position in buffer-pixel space (screen-space: east = +x, south = +y,
/// north = −y = the buffer top).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Point {
    /// Pixel column (east-positive).
    pub x: u16,
    /// Pixel row (south-positive; north is the buffer top).
    pub y: u16,
}

/// A width×height extent in pixels — named axes so a (w,h) tuple can't be
/// silently transposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    /// Width in pixels.
    pub w: u16,
    /// Height in pixels.
    pub h: u16,
}

/// An interior room wall's straight run, both ends inclusive and in order. It
/// names its axis because a one-cell run, the post a door flush with its run's
/// end leaves, could not say it with its ends alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WallSegment {
    /// An E-W run.
    Horizontal {
        /// Its row.
        y: u16,
        /// Its west end.
        x0: u16,
        /// Its east end.
        x1: u16,
    },
    /// A N-S run.
    Vertical {
        /// Its column.
        x: u16,
        /// Its north end.
        y0: u16,
        /// Its south end.
        y1: u16,
    },
}

/// A placed plant: its kind paired with its centre position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlantItem {
    /// Which plant species/sprite.
    pub kind: PlantKind,
    /// Centre position in buffer pixels.
    pub pos: Point,
}

/// A placed wall decoration: its kind paired with its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallDecorItem {
    /// Which wall decoration.
    pub kind: WallDecor,
    /// Position in buffer pixels.
    pub pos: Point,
}

/// A placed aisle/pod decoration: its kind paired with its centre position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PodDecorItem {
    /// Which aisle/pod decoration.
    pub kind: PodDecor,
    /// Centre position in buffer pixels.
    pub pos: Point,
}

/// A named stop an agent can walk to and occupy — a lounge seat, appliance,
/// or meeting-room slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waypoint {
    /// Where the occupant stands or sits (pixel-space).
    pub pos: Point,
    /// What kind of stop this is (seat, appliance, meeting slot, …).
    pub kind: WaypointKind,
    /// Direction the occupant faces here — `South` for the facing-neutral
    /// single-point waypoints, toward the table for meeting-room slots.
    pub facing: Facing,
    /// Meeting-room id this slot belongs to (`Some` for `MeetingSofa` /
    /// `MeetingChair`). Slots sharing a `room_id` form one group-chitchat venue.
    pub room_id: Option<usize>,
}

/// The lounge vignette placed as one unit. Couch + floor lamp + side table
/// share one fit gate (hence non-optional here); the aquarium
/// carries an EXTRA east-clearance gate against the elevator door, so it
/// stays `Option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lounge {
    /// Centre of the 3-seat couch sprite; the rug and side table are placed
    /// relative to it.
    pub couch_center: Point,
    /// Floor lamp, just east of the couch.
    pub floor_lamp: Point,
    /// Side table on the couch's opposite (west) flank.
    pub side_table: Point,
    /// Aquarium centre, east of the lamp against the north wall band — `None`
    /// when the elevator-door east clearance fails.
    pub fish_tank: Option<Point>,
}

/// The lounge rug's size.
const LOUNGE_RUG: Size = Size { w: 22, h: 7 };
/// How far south of the couch centre the lounge rug's centre sits, so it reaches
/// out in front of the couch.
const LOUNGE_RUG_DY: u16 = 3;

impl Lounge {
    /// The rug the couch stands on, ending by `ground_end`, the row the desks
    /// south of it start at.
    pub(crate) fn rug(&self, ground_end: u16) -> Bounds {
        let centre = Point {
            x: self.couch_center.x,
            y: self.couch_center.y + LOUNGE_RUG_DY,
        };
        let tl = anchored_top_left(Pivot::Center, centre, LOUNGE_RUG.w, LOUNGE_RUG.h);
        Bounds {
            x: tl.x,
            y: tl.y.min(ground_end.saturating_sub(LOUNGE_RUG.h)),
            width: LOUNGE_RUG.w,
            height: LOUNGE_RUG.h,
        }
    }
}

/// An agent's home desk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HomeDesk {
    /// Its top-left, where the pod grid places it.
    pub at: Point,
    /// Which way its occupant faces — the ONE authority painters and the
    /// approach/walk geometry share, so a seat is never drawn off its routed side.
    pub facing: Facing,
}

/// The computed office geometry for one floor — quadrant bounds, per-agent
/// desks, waypoints, decor, walls, and the walkability mask. Built once per
/// `(buf_w, buf_h, max_desks)`.
#[derive(Debug, Clone)]
pub struct SceneLayout {
    /// Buffer width in pixels this layout was computed for.
    pub buf_w: u16,
    /// Buffer height in pixels this layout was computed for.
    pub buf_h: u16,
    /// The desk-pod quadrant — the bounds enclosing the cubicle grid.
    pub cubicle_band: Bounds,
    /// The cubicle-band-width horizontal aisle at the bottom of the desk pods —
    /// the appliance-placement region (vending/printer). Keep it distinct from
    /// `corridor`: same y/height, different x-extent.
    pub cubicle_aisle: Bounds,
    /// Per-agent home desks, indexed floor-locally (read via
    /// [`Self::home_desk`]).
    pub home_desks: Vec<HomeDesk>,
    /// Named stops (lounge seats, appliances, meeting slots) agents walk to.
    pub waypoints: Vec<Waypoint>,
    /// Placed potted plants.
    pub plants: Vec<PlantItem>,
    /// Decorations mounted on the walls (whiteboards, TVs, exit signs).
    pub wall_decor: Vec<WallDecorItem>,
    /// Decor items placed in the aisles between 2×2 desk pods.
    pub pod_decor: Vec<PodDecorItem>,
    /// The lounge vignette as ONE unit — `None` when it doesn't fit.
    pub lounge: Option<Lounge>,
    /// The office entry door's top-left cell, in the window wall's last slot.
    pub door: Point,
    /// The walkable cell just inside the door — the entry/exit waypoint.
    pub door_threshold: Point,
    /// Meeting rooms in floor order — the index IS the `room_id` every
    /// waypoint and painter joins on.
    pub meeting_rooms: Vec<MeetingRoom>,
    /// The pantry aggregate (bounds + counter size + island) — `None`
    /// on floors without a pantry (Dense dual-meeting).
    pub pantry: Option<PantryRoom>,
    /// Interior room-wall segments the painter draws and the mask blocks.
    pub room_walls: Vec<WallSegment>,
    /// The openings the wall resolver cut into `room_walls` — the painter
    /// draws door frames from these instead of re-inferring gaps from
    /// segment adjacency.
    pub(crate) doorways: Vec<Doorway>,
    /// `room_walls` as the mask stamps them and the painters draw them, their
    /// doorways framed: built once, here.
    pub(crate) wall_pieces: Vec<WallPiece>,
    /// Top offset in px reserved above the floor for the north wall+window
    /// band (and its carpet apron).
    pub top_margin: u16,
    /// The full-width horizontal corridor below the desk pods — the A\* router's
    /// preferred zone and the pet/mascot path.
    pub(crate) corridor: Bounds,
    /// Per-pixel walkability mask — the ground footprint every obstacle stamps,
    /// the surface routing runs over.
    pub walkable: WalkableMask,
    /// Coarse-cell reachable component (the walkable area an agent can A\*-route
    /// to) — consumed by `approach_point` to prefer a *reachable* approach side
    /// over a merely-walkable-but-walled-off one.
    pub(crate) reachable: ReachSet,
}

/// Integer percentage of `v`, floor semantics. Computed in u32: a bare
/// `buf_h * 30` overflows u16 once `buf_h > 2184`.
pub(crate) const fn pct(v: u16, n: u16) -> u16 {
    ((v as u32 * n as u32) / 100) as u16
}

/// The wider mask pad (px), around the solid pieces characters route AROUND rather
/// than scrape along.
pub(crate) const OBSTACLE_PAD_PX: u16 = 2;

/// The SMALLER mask pad of a piece that needs no routing buffer: a seat or appliance,
/// which paints over a visitor behind it; wall decor (an [`OBSTACLE_PAD_PX`] band would
/// inflate its blocked rect back to the full sprite width); a plant in a tight aisle.
const TIGHT_PAD_PX: u16 = 1;

/// The north wall+window band's visual bottom sits this many px ABOVE
/// `top_margin`; the rows in between render as carpet apron, not wall, so the
/// mask blocks only down to the band bottom. The mask and the visual read it
/// through [`wall_band_h`], or they drift.
pub(crate) const WALL_BAND_TO_TOP_MARGIN: u16 = 4;

/// The visible north wall band's height in px for a `top_margin`.
pub(crate) const fn wall_band_h(top_margin: u16) -> u16 {
    top_margin - WALL_BAND_TO_TOP_MARGIN
}

/// How many pixels of the pantry counter actually sit on the floor: only the
/// southern base contacts the ground, the receding cabinet tops + backsplash
/// are overhang (invariant #6), so a character routed behind the counter is
/// occluded by its own y-sorted sprite.
pub(crate) const PANTRY_FOOTPRINT_DEPTH: u16 = 3;

/// The desk BODY size in SLOT units — the grid-pitch pricing. SLOT ≠ GROUND:
/// the desk's blocked GROUND is the full sprite width (`decor::DESK_GROUND_W`,
/// side cabinets included) and the overhang rides the aisle, so every band-EDGE
/// clamp reads `DESK_GROUND_W`, not `DESK_W` (the #549 2px-overflow drift).
pub(crate) const DESK_W: u16 = 10;
/// Glass columns offset from the desk sprite's left edge.
pub(crate) const SCREEN_GLASS_COLS: std::ops::RangeInclusive<u16> = 4..=9;
/// Rows of desk SURFACE below `desk.y`; both desk sprites are cut to it.
pub(crate) const DESK_SURFACE_ROWS: u16 = 5;
pub(crate) const DESK_FRONT_ROWS: u16 = 1;
pub(crate) const DESK_LEG_ROWS: u16 = 2;

/// Desk body height in SLOT units — the N-S pod pitch; the blocked ground is
/// only `DESK_FOOT_H` deep.
pub(crate) const DESK_H: u16 = 6;

const _: () = assert!(
    DESK_H + 2 == DESK_SURFACE_ROWS + DESK_FRONT_ROWS + DESK_LEG_ROWS,
    "the desk's VISUAL height (DESK_H + 2) must equal the rows its art draws: \
     surface + front + legs"
);
/// The desk's ground-CONTACT depth (rows) — only the front edge / legs touch
/// the floor; the surface + monitor OVERHANG north (`ground_y: End`), so a
/// walker passes BEHIND the monitor and is occluded by the desk's own y-sort
/// (invariant #6). Distinct from `DESK_H`, which prices the slot.
pub(crate) const DESK_FOOT_H: u16 = 2;
/// Default character sprite width (px) — the ONE authority every
/// out-of-pixel_painter consumer centers on. Sprite BLIT sites still
/// pass the pack's REAL `frame.width`; this is the width-unknown fallback.
/// Lives in `layout` so `layout::decor` can read it without a module cycle.
pub const CHARACTER_SPRITE_W: u16 = 8;
/// Default character sprite height (px) — [`CHARACTER_SPRITE_W`]'s twin: the
/// fallback where the pack's real frame isn't threaded. The pose offsets
/// are a SEPARATE vertical-anchor concern.
pub const CHARACTER_SPRITE_H: u16 = 12;
/// Elevator-door sprite width in buffer px, read by the layout, the wall's
/// window cut-out and the hover box; `every_hover_size_is_its_painted_sprite_size`
/// pins it to the door sprite.
pub const ELEVATOR_W: u16 = 16;
/// Elevator-door sprite height in buffer px — which sets the door's sort row.
pub const ELEVATOR_H: u16 = 14;

/// The buffer rows a half-block terminal cell shows.
pub const CELL_ROWS: u16 = 2;

/// The rows over a door whose top row is `door_y` that the terminal's floor
/// indicator writes its text across: the whole cell above the door's.
pub(crate) fn floor_indicator_rows(door_y: u16) -> std::ops::Range<u16> {
    let top = (door_y / CELL_ROWS).saturating_sub(1) * CELL_ROWS;
    top..top + CELL_ROWS
}

/// What the floor indicator says on `floor`, every painter's: its number
/// between the arrows up and down, each hollow where no floor is that way.
pub fn floor_indicator(floor: crate::floor::FloorMeta) -> [crate::display::Content; 3] {
    use crate::display::{Content, Icon};
    let above = floor.floor_idx + 1 < floor.floors;
    let below = floor.floor_idx > 0;
    [
        Content::Icon(if above { Icon::Up } else { Icon::NoUp }),
        Content::Text(format!(" F{} ", floor.floor_idx + 1)),
        Content::Icon(if below { Icon::Down } else { Icon::NoDown }),
    ]
}

/// Where the exit sign hangs over a door at `door`: centred above its floor
/// indicator, or `None` where that would climb above the windows' head.
pub(crate) fn exit_sign_pos(door: Point) -> Option<Point> {
    let sign = furniture_def(WallDecor::ExitSign.furniture()).visual;
    let y = floor_indicator_rows(door.y).start.checked_sub(sign.h)?;
    (y >= WINDOW_TOP).then_some(Point {
        x: door.x + (ELEVATOR_W - sign.w) / 2,
        y,
    })
}
/// NOT a cap — production layouts fill the buffer's physical space
/// (`max_desks: None`). This is the stable "one classic office worth of desks"
/// reference, and the `snapshot` example that renders the docs/CI media
/// baselines pins its scene to it.
pub const CLASSIC_OFFICE_DESKS: usize = 16;
/// Test-facing alias for [`CLASSIC_OFFICE_DESKS`] — the named default
/// deterministic tests/snapshots pass as `Some(TEST_DEFAULT_DESKS)`.
#[cfg(test)]
pub(crate) const TEST_DEFAULT_DESKS: usize = CLASSIC_OFFICE_DESKS;
/// The desk-grid vertical gap unit (px) — the lounge-waypoint bound in the layout
/// tests reads it; the pod grid itself prices its own spacing.
#[cfg(test)]
pub(crate) const DESK_GAP_Y: u16 = 14;
/// Floor (px) for the layout's `top_margin` — `compute::top_margin`'s
/// percent-of-buffer derivation never shrinks the north wall band below this.
pub(crate) const MIN_TOP_MARGIN: u16 = 20;
const MIN_DUAL_MEETING_H: u16 = 80;

/// Number of desks per side in a pod (`POD_SIDE * POD_SIDE` total).
pub(crate) const POD_SIDE: u16 = 2;
/// Gap between two desks inside the same pod — big enough that each desk
/// reads as its own workstation, not a merged blob.
pub(crate) const INTRA_POD_GAP_X: u16 = 12;
/// N-S gap between the two desks stacked in one pod (vertical counterpart to
/// [`INTRA_POD_GAP_X`]); sets the pod's inner height. Rows step by
/// `DESK_H + this`, and that STEP must stay EVEN or the pod's two rows land on
/// different half-block parities — so retuning either side needs both checked.
pub(crate) const INTRA_POD_GAP_Y: u16 = 6;
const _: () = assert!((DESK_H + INTRA_POD_GAP_Y).is_multiple_of(2));
/// Horizontal (E-W) gap between adjacent pod COLUMNS — wide enough to keep the
/// pod boundary visually distinct. Deliberately > the N-S gap: screens are
/// landscape, so spread wider horizontally and pack tighter vertically.
pub(crate) const INTER_POD_AISLE_X: u16 = 20;
/// Vertical (N-S) gap between adjacent pod ROWS. INTENTIONALLY < the E-W gap
/// (landscape screens — see `INTER_POD_AISLE_X`). Shrinking it breaks
/// `every_home_desk_has_a_reachable_approach_on_its_own_far_side`: the seat's
/// far-side approach cell collides with the desk in the row above.
pub(crate) const INTER_POD_AISLE_Y: u16 = 18;

impl SceneLayout {
    /// Returns `None` if the buffer is too small for even one cubicle plus the
    /// fixed lounge area — the caller paints a "terminal too small" message.
    /// Panics where [`Self::compute_with_seed`] does.
    pub fn compute(buf_w: u16, buf_h: u16, max_desks: Option<usize>) -> Option<Self> {
        Self::compute_with_seed(buf_w, buf_h, max_desks, 0)
    }

    /// `max_desks` caps the desk count: `None` fills the office to the buffer's
    /// physical capacity, `Some(n)` caps at `n` for deterministic
    /// tests/snapshots. The pod grid geometry is always the room's true
    /// capacity regardless of the cap.
    ///
    /// # Panics
    ///
    /// If the office is still sealed once the #566 guard has dropped every
    /// decor that can seal it: a layout bug no production floor seed is known to reach.
    pub fn compute_with_seed(
        buf_w: u16,
        buf_h: u16,
        max_desks: Option<usize>,
        floor_seed: u64,
    ) -> Option<Self> {
        compute::compute_with_seed(buf_w, buf_h, max_desks, floor_seed)
    }

    /// Is buffer pixel `(x, y)` walkable?
    pub fn is_walkable(&self, x: u16, y: u16) -> bool {
        self.walkable.is_walkable(x, y)
    }

    /// Typed accessor for a floor's home desk. `home_desks` is a
    /// FLOOR-LOCAL vector — index it through a `FloorLocalDeskIndex`, never
    /// with an `AgentSlot.desk_index` directly.
    pub fn home_desk(&self, i: FloorLocalDeskIndex) -> Option<HomeDesk> {
        self.home_desks.get(i.0).copied()
    }

    /// Is `p` clear of the furniture sprites that PAINT OVER it? Walkable is the
    /// GROUND rule (invariant #6), so the cell in front of a desk is legitimately
    /// walkable AND legitimately covered — fine to walk THROUGH, wrong to park in.
    pub(crate) fn is_visually_clear(&self, p: Point) -> bool {
        let inside =
            |b: Bounds| p.x >= b.x && p.x < b.x + b.width && p.y >= b.y && p.y < b.y + b.height;
        !self.fixtures().any(|f| {
            let covers = match f.kind {
                FixtureKind::Desk(_)
                | FixtureKind::Station { .. }
                | FixtureKind::Plant { .. }
                | FixtureKind::Pod { .. }
                // Not out as a class: the whiteboard is free-standing floor
                // furniture standing in an inter-pod aisle.
                | FixtureKind::Wall { .. }
                | FixtureKind::MeetingSofa { .. }
                | FixtureKind::MeetingTable { .. }
                | FixtureKind::MeetingChair { .. }
                | FixtureKind::LoungeCouch
                | FixtureKind::SideTable
                | FixtureKind::FloorLamp
                | FixtureKind::FishTank
                | FixtureKind::KitchenIsland => true,
                // Flat on the floor: whatever stands on them paints over them.
                FixtureKind::MeetingRug { .. }
                | FixtureKind::LoungeRug
                | FixtureKind::Doormat { .. }
                | FixtureKind::PantryMat
                | FixtureKind::IslandMat
                | FixtureKind::Runner => false,
                // Hung in the backdrop, under the whole sorted scene.
                FixtureKind::NoticeBoard { .. } | FixtureKind::NeonSign | FixtureKind::Clock => {
                    false
                }
                // Architecture, and the band it punches is not walkable.
                FixtureKind::Door => false,
                // They do paint over what stands behind them, but covering them
                // moves where the pet and the mascots rest: a look change, left
                // to its own decision.
                FixtureKind::CoatRack { .. }
                | FixtureKind::FilingCabinet(_)
                | FixtureKind::DeskChair(_)
                | FixtureKind::WaterCooler
                | FixtureKind::TrashBin => false,
            };
            covers && inside(f.visual)
        })
    }

    /// The visible top window-wall band height in px: [`wall_band_h`] of this
    /// layout's margin, for the render sites that only hold the layout.
    pub(crate) fn wall_band_h(&self) -> u16 {
        wall_band_h(self.top_margin)
    }

    /// The bounds of meeting room `room_id` — the id IS the
    /// [`Self::meeting_rooms`] index.
    pub fn meeting_room_bounds(&self, room_id: usize) -> Option<Bounds> {
        self.meeting_rooms.get(room_id).map(|r| r.bounds)
    }

    /// Couch sprite centre (middle of the 3 seats) — `Some` iff the lounge
    /// vignette fits.
    #[cfg(test)]
    pub(crate) fn couch_sprite_center(&self) -> Option<Point> {
        self.lounge.as_ref().map(|l| l.couch_center)
    }

    /// The lounge floor lamp — `Some` iff the vignette fits.
    pub(crate) fn floor_lamp(&self) -> Option<Point> {
        self.lounge.as_ref().map(|l| l.floor_lamp)
    }

    /// The lounge side table — `Some` iff the vignette fits.
    #[cfg(test)]
    pub(crate) fn lounge_side_table(&self) -> Option<Point> {
        self.lounge.as_ref().map(|l| l.side_table)
    }

    /// The aquarium centre — `Some` only when the vignette fits AND the
    /// east-clearance gate against the elevator door passes.
    #[cfg(test)]
    pub(crate) fn fish_tank(&self) -> Option<Point> {
        self.lounge.as_ref().and_then(|l| l.fish_tank)
    }

    /// The pantry counter's size, or the `COMPACT_COUNTER` fallback
    /// when no pantry exists — `approach_point`'s signature needs SOME
    /// size even on pantry-less floors, where it is never consulted.
    pub(crate) fn pantry_counter_size(&self) -> Size {
        self.pantry.map_or(COMPACT_COUNTER, |p| p.counter_size)
    }

    /// Where an agent's sprite RENDERS when it visits furniture `kind` at `pos`
    /// (the walk goal for an obstacle, the seat cell for a seat), on the side
    /// nearest `origin` facing `facing`.
    pub(crate) fn stand_point(
        &self,
        kind: WaypointKind,
        pos: Point,
        origin: Point,
        facing: Facing,
    ) -> Point {
        stand_point(
            kind,
            pos,
            self.pantry_counter_size(),
            &self.walkable,
            origin,
            facing,
            &self.reachable,
        )
    }

    /// A\*'s goal cell when an agent at `origin` visits furniture `kind` at
    /// `pos` facing `facing`. Callers MUST honor its `== pos` "no valid
    /// approach" sentinel — skip the furniture this cycle rather than route to it.
    pub(crate) fn approach_point(
        &self,
        kind: Furniture,
        pos: Point,
        origin: Point,
        facing: Facing,
    ) -> Point {
        approach_point(
            kind,
            pos,
            facing,
            self.pantry_counter_size(),
            &self.walkable,
            origin,
            &self.reachable,
        )
    }
}

#[cfg(test)]
mod placement_sweep;
#[cfg(test)]
mod tests;
