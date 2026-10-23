//! The meeting room aggregate: bounds + the sofa/table trio.

use super::walls::WALL_H;
use crate::layout::{Bounds, Furniture, Pivot, Point, anchored_top_left, furniture_def, pct};

/// One meeting room's furniture trio. The fixed-size array encodes the
/// invariant that a fitted room produces exactly 2 sofas + 1 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeetingTrio {
    /// The two sofa centres: `[0]` north, `[1]` south (pixel-space).
    pub(crate) sofas: [Point; 2],
    /// The table centre, midway between the two sofas (pixel-space).
    pub table: Point,
}

/// A meeting rug's width.
const MEETING_RUG_W: u16 = 18;
/// How far a meeting rug runs past the two sofa centres, both ends together.
const MEETING_RUG_OVERHANG: u16 = 8;

impl MeetingTrio {
    /// The rug under this trio's table, reaching from sofa to sofa but no
    /// further than the table's distance to `ground_end`, the row the ground ends
    /// at (plus the same overhang).
    pub(crate) fn rug(&self, ground_end: u16) -> Bounds {
        let [north, south] = self.sofas;
        let h = south
            .y
            .saturating_sub(north.y)
            .saturating_add(MEETING_RUG_OVERHANG)
            // A trio by the south edge shortens its rug, still centred on the
            // table.
            .min(
                ground_end
                    .saturating_sub(self.table.y)
                    .saturating_add(MEETING_RUG_OVERHANG),
            );
        let tl = anchored_top_left(Pivot::Center, self.table, MEETING_RUG_W, h);
        Bounds {
            x: tl.x,
            y: tl.y,
            width: MEETING_RUG_W,
            height: h,
        }
    }
}

/// A meeting room: its bounds plus the trio it hosts. Its index in
/// `SceneLayout::meeting_rooms` IS the `room_id` every waypoint and painter
/// joins on, so the Vec must never be compacted or reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeetingRoom {
    /// The room's interior rectangle (buffer pixels).
    pub bounds: Bounds,
    /// The sofa/table trio, or `None` when the room is too small to fit it
    /// (bare floor — the room still exists to hold its `room_id`).
    pub trio: Option<MeetingTrio>,
}

/// Horizontal offset of each head-of-table chair from the table centre —
/// mirrored ±: west chair at `table.x − DX` (faces East), east at `+DX`.
pub(crate) const MEETING_CHAIR_TABLE_DX: u16 = 9;

/// Columns from a coat rack's pole to a coat hook.
pub(crate) const COAT_HOOK_DX: u16 = 1;

/// A coat's width, hanging outward from its hook.
pub(crate) const COAT_W: u16 = 2;

/// Rows from a coat rack's pole top to its base: the row it y-sorts at.
pub(crate) const COAT_RACK_BASE_DY: u16 = 11;

/// The box a coat rack whose pole top is `pos` is drawn in: the one authority
/// for its painter, its y-sort row, its hover box and the chair-clearance gate.
pub(crate) fn coat_rack_rect_at(pos: Point) -> Bounds {
    let reach = COAT_HOOK_DX + COAT_W - 1;
    Bounds {
        x: pos.x.saturating_sub(reach),
        y: pos.y,
        width: 2 * reach + 1,
        height: COAT_RACK_BASE_DY + 1,
    }
}

impl MeetingRoom {
    /// The coat rack's pole top beside the corridor door (east wall,
    /// room-centre row) — or `None` when a fitted room is too narrow for the
    /// rack's coats to clear the east chair and its sitter.
    pub(crate) fn coat_rack_pos(&self) -> Option<Point> {
        /// Rows from the room-centre row down to the rack's base.
        const BASE_BELOW_MID: u16 = 3;
        let b = self.bounds;
        if b.width <= 20 {
            return None;
        }
        let pos = Point {
            x: b.x + b.width - 5,
            y: (b.y + b.height / 2 + BASE_BELOW_MID).checked_sub(COAT_RACK_BASE_DY)?,
        };
        if let Some(t) = &self.trio {
            // The seated sprite shares the chair body's east edge, so the
            // body reach IS the sitter's reach.
            let chair_east_reach = t.table.x
                + MEETING_CHAIR_TABLE_DX
                + furniture_def(Furniture::MeetingChair).visual.w / 2;
            let rack_west_reach = coat_rack_rect_at(pos).x;
            // Drop only on true overlap: a rack and chair that sit exactly
            // adjacent keep it.
            if rack_west_reach <= chair_east_reach {
                return None;
            }
        }
        Some(pos)
    }

    /// Minimum room height that fits the sofa/table trio — the fit gate AND the
    /// floor of `compute_with_seed`'s meeting/pantry split negotiation. Prices
    /// the TABLE between the sofas, not just the two sofa bodies: with both sofa
    /// clamps bound the centred table's ground would otherwise clip both.
    /// Derived from the furniture rows the mask stamps, so a sprite that grows
    /// can't leave a too-short room passing.
    pub(crate) fn trio_fit_h() -> u16 {
        let sofa = furniture_def(Furniture::MeetingSofaBody);
        let table_fp_h = furniture_def(Furniture::MeetingTable)
            .footprint
            .map_or(0, |s| s.h);
        sofa.visual.h * 2 + sofa.footprint.map_or(0, |s| s.h) + table_fp_h
    }

    /// Place the sofa/table trio inside `bounds` (the caller gates on
    /// `room_fits_furniture` first).
    ///
    /// Sofas sit SYMMETRICALLY about the room mid-line (20%/80%) so each gets
    /// equal front clearance to the centred table, which follows to the sofa
    /// midpoint. `dense` picks the north-sofa floor: a NON-dense room (room 0)
    /// sits above the wall band's walkable carpet apron, so its sofa may tuck to
    /// `sofa_h/2`; the DENSE room (room 1) sits under the glass divider (which
    /// stamps `WALL_THICK_H` rows into its top), so its sofa needs a full
    /// `sofa_h` for its ground to clear the wall. Room 0 always has a room
    /// stacked below, whose wall's cap rises into its south rows, so its south
    /// sofa ends above the cap, clear of the doorway cut there.
    pub(crate) fn place_trio(bounds: Bounds, dense: bool) -> MeetingTrio {
        let sofa_h = furniture_def(Furniture::MeetingSofaBody).visual.h;
        let north_floor = if dense { sofa_h } else { sofa_h / 2 };
        let south_floor = if dense {
            sofa_h
        } else {
            WALL_H.cap + sofa_h.div_ceil(2)
        };
        let cx = bounds.x + bounds.width / 2;
        let north_y = (bounds.y + pct(bounds.height, 20)).max(bounds.y + north_floor);
        let south_y = (bounds.y + pct(bounds.height, 80))
            .min(bounds.y + bounds.height.saturating_sub(south_floor));
        MeetingTrio {
            sofas: [Point { x: cx, y: north_y }, Point { x: cx, y: south_y }],
            table: Point {
                x: cx,
                y: u16::midpoint(north_y, south_y),
            },
        }
    }

    /// The entrance doormat's sprite box (bordered rug on the cubicle side, one
    /// clear column east of the room's east wall) — `None` on a room too narrow.
    /// The roster's mat also gives way to what stands over it.
    pub(crate) fn doormat_rect(&self) -> Option<Bounds> {
        let b = self.bounds;
        // Lazy `.then`: `b.height / 2 - 2` must not run for a sub-gate room.
        (b.width > 10).then(|| Bounds {
            x: b.x + b.width + 1,
            y: b.y + b.height / 2 - 2,
            width: 4,
            height: 5,
        })
    }

    /// The east edge past which the wall-band bookshelf must drain to clear the
    /// sofa's padded ground — read from the REAL placed sofa, NOT reconstructed
    /// from `bounds`, so a sofa resize can't desync it from `mask`'s
    /// Center-anchored sofa stamp. `None` for a bare room.
    pub(crate) fn sofa_east_drain_edge(&self) -> Option<u16> {
        self.trio.and_then(|t| {
            furniture_def(Furniture::MeetingSofaBody)
                .east_of_padded_ground(Pivot::Center, t.sofas[0])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dual-meeting floors (160x120, 192x158, 240x160) stack a room by the
    /// south edge, whose last rows the corridor shares.
    #[test]
    fn every_meeting_rug_ends_before_the_runner() {
        let mut south_rooms = 0;
        for (w, h) in [
            (96u16, 60u16),
            (160, 96),
            (160, 120),
            (192, 158),
            (240, 144),
            (240, 160),
            (320, 180),
        ] {
            for seed in 0..8 {
                let Some(layout) = crate::layout::SceneLayout::compute_with_seed(w, h, None, seed)
                else {
                    continue;
                };
                let ground_end = layout.corridor.y;
                for f in layout.fixtures() {
                    let crate::layout::FixtureKind::MeetingRug { room } = f.kind else {
                        continue;
                    };
                    let room = layout.meeting_rooms[room].bounds;
                    south_rooms += usize::from(room.y + room.height > ground_end);
                    let rug = f.visual;
                    assert!(
                        rug.y + rug.height <= ground_end,
                        "{w}x{h} seed {seed}: {rug:?} past {ground_end}"
                    );
                }
            }
        }
        assert!(south_rooms > 0, "no meeting room reaches the corridor");
    }

    #[test]
    fn a_meeting_rug_reaches_no_further_than_the_table_is_from_the_floor_end() {
        let x = 40;
        let trio = MeetingTrio {
            sofas: [Point { x, y: 50 }, Point { x, y: 90 }],
            table: Point { x, y: 70 },
        };
        let ground_end = 96;
        assert_eq!(
            trio.rug(ground_end).height,
            ground_end - trio.table.y + MEETING_RUG_OVERHANG,
            "held to the table's distance from the ground's end"
        );
        assert_eq!(
            trio.rug(200).height,
            90 - 50 + MEETING_RUG_OVERHANG,
            "sofa to sofa where the floor allows"
        );
        let edge = trio.table.y + MEETING_RUG_OVERHANG / 2;
        assert_eq!(
            trio.rug(edge).height,
            edge - trio.table.y + MEETING_RUG_OVERHANG,
            "nearer the edge than the overhang"
        );
    }
}
