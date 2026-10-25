//! The seat model. [`Seat`] (kind × cell × facing) is the single source of
//! truth for an occupant of ANY seat — waypoint couch, sofa, meeting chair,
//! island stool, or a home desk: sprite + flip, render top-left, sort row and
//! sit-down glide all derive from it. [`SeatView`] is the LOOK it resolves to,
//! not the authority.

use super::*;
use pixtuoid_core::sprite::format::Piece;

use crate::sim::anchors::{back_couch_top_left, waypoint_top_left};

/// The still back view — also the fallback for a pose that has none of its own,
/// so a back-turned sitter keeps their back to the camera whenever the pack has
/// this one.
const SEATED_BACK: Piece = Piece::SeatedBack;

/// A table, not `format!("{base}_back")`, because sprite names are `&'static str`.
const SEATED_BACK_VIEWS: &[(Piece, Piece)] = &[
    (Piece::Seated, SEATED_BACK),
    (Piece::Typing, Piece::TypingBack),
];

/// Which VIEW of a character a seat shows. ONLY the look: what they sit ON
/// decides the art and the geometry ([`Seat`]), not this.
///
/// The ONE source BOTH the seated render and the sit-down WALK glide derive
/// from. Deriving the glide facing from the travel direction instead is the
/// recurring "sit facing the wrong way then snap" bug: a window-facing (`North`)
/// seat is approached from the north but its foot-cell is pinned SOUTH, so the
/// settle travels south, renders a FRONT walk, and the agent sits facing the
/// camera for the whole settle before snapping to `back_couch`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SeatView {
    /// Faces the camera (south).
    Front,
    /// Faces away — the window / the back wall.
    Back,
    /// Faces sideways; `flip` mirrors east↔west.
    Side { flip: bool },
}

/// What an agent is sitting ON. The home desk is not a `WaypointKind` — it is
/// one agent's own seat, not a shared destination — so the two meet here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SeatKind {
    Waypoint(crate::layout::WaypointKind),
    HomeDesk,
}

/// A seat: WHAT you sit on, WHERE, and which way you LOOK. Everything a sitter
/// needs — sprite, flip, render top-left, sort row, sit-down glide — derives from
/// these three, so a couch, a meeting chair and a home desk are one thing.
///
/// Extend [`view`](Self::view), [`sprite_for`](Self::sprite_for) and
/// [`seated_furniture`](Self::seated_furniture) to add a seatable furniture —
/// each lists its kinds EXPLICITLY, so a new `WaypointKind` is a compile error
/// there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Seat {
    kind: SeatKind,
    /// The cell the SPRITE renders on — a waypoint's resolved stand cell, a
    /// desk's walk anchor. NOT the settle foot-cell `settle_seat` matches on,
    /// which for a couch/sofa/chair is `WALKING_Y_OFF - SEAT_RENDER_Y_OFF`
    /// further south; constructing a `Seat` from that shifts top-left and sort row.
    pos: Point,
    /// Which way the SITTER looks, decoupled from the side they approached from.
    facing: crate::layout::Facing,
}

/// The depth the sitters of a meeting sofa at `sofa` sort at: their seat's own
/// [`sort_row`](Seat::sort_row). The cutaway keys the sofa by it, so a front sofa,
/// queued before its sitters, ties them and they sit on it.
pub(crate) fn sofa_sitter_sort_row(sofa: Point) -> u16 {
    Seat::at_waypoint(
        crate::layout::WaypointKind::MeetingSofa,
        sofa,
        crate::layout::Facing::South,
    )
    .sort_row()
}

impl Seat {
    pub(crate) fn at_waypoint(
        kind: crate::layout::WaypointKind,
        pos: Point,
        facing: crate::layout::Facing,
    ) -> Self {
        Seat {
            kind: SeatKind::Waypoint(kind),
            pos,
            facing,
        }
    }

    /// The seat at a home `desk`. Its cell is the desk's walk anchor — the ONE
    /// value the chair sprite, its occupant and the walk that ends there share.
    pub(crate) fn at_desk(desk: Point, facing: crate::layout::Facing) -> Self {
        Seat {
            kind: SeatKind::HomeDesk,
            pos: crate::layout::desk_walk_anchor_facing(desk, facing),
            facing,
        }
    }

    /// Whether this seat's occupant sits at SEAT height. Read by both geometry
    /// arms so they cannot disagree — a seat-height top-left paired with the
    /// feet's sort row orders a sitter through their own furniture. The meeting
    /// chair qualifies because its profile sprite shares the front `seated`
    /// sprite's bottom-row geometry.
    pub(crate) fn seated_furniture(self) -> bool {
        use crate::layout::WaypointKind;
        match self.kind {
            SeatKind::Waypoint(
                WaypointKind::Couch | WaypointKind::MeetingSofa | WaypointKind::MeetingChair,
            ) => true,
            SeatKind::HomeDesk
            | SeatKind::Waypoint(
                WaypointKind::Pantry
                | WaypointKind::PhoneBooth
                | WaypointKind::StandingDesk
                | WaypointKind::VendingMachine
                | WaypointKind::Printer
                | WaypointKind::Island
                | WaypointKind::SnackShelf,
            ) => false,
        }
    }

    fn view(self) -> SeatView {
        use crate::layout::{Facing, WaypointKind};
        match self.kind {
            // A seat with only two views: any facing but North reads as front.
            SeatKind::HomeDesk
            | SeatKind::Waypoint(WaypointKind::Couch | WaypointKind::MeetingSofa) => {
                match self.facing {
                    Facing::North => SeatView::Back,
                    _ => SeatView::Front,
                }
            }
            // The base `side_seated` sprite faces East (the west chair's view),
            // so the east chair mirrors. The island's stander flips on the
            // OPPOSITE facing; both directions are pinned, don't align them.
            SeatKind::Waypoint(WaypointKind::MeetingChair) => SeatView::Side {
                flip: matches!(self.facing, Facing::West),
            },
            SeatKind::Waypoint(WaypointKind::Island) => SeatView::Side {
                flip: matches!(self.facing, Facing::East),
            },
            // Not seats — the agent stands AT these.
            SeatKind::Waypoint(
                WaypointKind::Pantry
                | WaypointKind::PhoneBooth
                | WaypointKind::StandingDesk
                | WaypointKind::VendingMachine
                | WaypointKind::Printer
                | WaypointKind::SnackShelf,
            ) => SeatView::Front,
        }
    }

    /// Sprite + horizontal flip, where `base` is the occupant's own FRONT-view
    /// animation. A waypoint occupant always has the one (`seated`); a desk
    /// occupant's is their pose's (`typing`, `seated_sleeping`, …), and the view
    /// only decides which side of it shows. The furniture with art of its own —
    /// the couch's `back_couch`, the chair's profile, an upright stander —
    /// answers with that and ignores `base`; nobody types on a sofa.
    pub(crate) fn sprite_for(self, base: Piece) -> (Piece, bool) {
        use crate::layout::WaypointKind;
        let view = self.view();
        match self.kind {
            SeatKind::HomeDesk => match view {
                SeatView::Back => (
                    SEATED_BACK_VIEWS
                        .iter()
                        .find(|(front, _)| *front == base)
                        .map_or(SEATED_BACK, |&(_, back)| back),
                    false,
                ),
                _ => (base, false),
            },
            SeatKind::Waypoint(WaypointKind::Couch | WaypointKind::MeetingSofa) => match view {
                SeatView::Back => (Piece::BackCouch, false),
                _ => (base, false),
            },
            SeatKind::Waypoint(WaypointKind::MeetingChair) => (
                Piece::SideSeated,
                matches!(view, SeatView::Side { flip: true }),
            ),
            // You leave the counter holding what you came for.
            SeatKind::Waypoint(WaypointKind::Pantry) => (Piece::HoldingCoffee, false),
            // The island's bartender and the stand-beside appliances.
            SeatKind::Waypoint(
                WaypointKind::Island
                | WaypointKind::PhoneBooth
                | WaypointKind::StandingDesk
                | WaypointKind::VendingMachine
                | WaypointKind::Printer
                | WaypointKind::SnackShelf,
            ) => (
                Piece::Standing,
                matches!(view, SeatView::Side { flip: true }),
            ),
        }
    }

    /// `(going_back, flip)` for the sit-down WALK glide that settles onto this
    /// seat — the SAME orientation as [`sprite_for`](Self::sprite_for),
    /// overriding the travel-direction rule for this terminal segment.
    pub(crate) fn settle_walk(self) -> (bool, bool) {
        match self.view() {
            SeatView::Front => (false, false),
            SeatView::Back => (true, false),
            SeatView::Side { flip } => (false, flip),
        }
    }

    /// The sort row of this seat's occupant — used BOTH for the settled
    /// `AtWaypoint` render AND for the sit-down / stand-up WALK glide. Letting
    /// the glide keep its natural foot sort row instead makes it cross the
    /// furniture's own sort row on the way down: the agent pops in front of the sofa
    /// mid-glide, then jumps behind it.
    pub(crate) fn sort_row(self) -> u16 {
        if self.seated_furniture() {
            return crate::layout::seated_sort_row(self.pos);
        }
        // The plain feet row, which for the bartender sits INSIDE the island
        // body — so the whole arc stays behind the counter.
        self.pos.y
    }

    /// The sprite's top-left, from which `sim::resolve_characters` places the
    /// sprite and its badge.
    pub(crate) fn render_top_left(self, sprite_w: u16) -> Point {
        if self.seated_furniture() {
            back_couch_top_left(self.pos, sprite_w)
        } else {
            // The desk sitter keeps the UPRIGHT top-left: their seat cell IS the
            // walk anchor the arrival settles on, so sprite and walk share it.
            waypoint_top_left(self.pos, sprite_w)
        }
    }
}

/// The [`Seat`] whose settle foot-cell is `cell`, or `None` if `cell` is not
/// one. The caller passes the glide's `to` (settling ONTO a seat) and/or `from`
/// (rising OFF it) — either endpoint on a foot-cell means the agent is on the
/// sit arc and must render in the seat's view and sort row, not the
/// travel-direction / foot-position values.
///
/// Covers the home desk too: `layout.home_desks` are NOT waypoints, but the
/// chair is a settle target once the desk's arrival glides onto it.
pub(crate) fn settle_seat(cell: Point, layout: &SceneLayout) -> Option<Seat> {
    use crate::layout::seated_foot_cell;
    layout
        .waypoints
        .iter()
        .find_map(|w| {
            (seated_foot_cell(w.kind.furniture(), w.pos) == Some(cell))
                .then(|| Seat::at_waypoint(w.kind, w.pos, w.facing))
        })
        .or_else(|| {
            layout.home_desks.iter().find_map(|desk| {
                let seat = Seat::at_desk(desk.at, desk.facing);
                (seat.pos == cell).then_some(seat)
            })
        })
}
