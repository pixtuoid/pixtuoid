//! Request-based room walls: each room DECLARES the edges it needs enclosed
//! (horizontal/vertical runs only) plus the doors it wants in them; the
//! resolver merges duplicate requests, unions their door requests, trims
//! vertical runs below crossing horizontal wall bodies, and cuts the gaps.
//! Walls are therefore a FUNCTION of the room set, never parallel geometry
//! derived from the same scalars the rooms were.
//!
//! Door policy is the ROOMS' (owner call): a meeting room opens a centered door
//! in its east (corridor) wall; the pantry opens the meeting↔pantry door at 60%
//! of the shared wall; two stacked meeting rooms declare NO door on their shared
//! wall — each already has its own corridor door, so connectivity holds.
//!
//! The mask and both painters read the one [`WallPiece`] built per wall, so the
//! ground they block and the glass they draw meet the same joints. The glass's
//! look is `crate::glass`, and nothing here touches a pixel.

use crate::layout::decor::GroundAlign;
use crate::layout::mask::ground_rect;
use crate::layout::{Bounds, MeetingRoom, Pivot, Point, Size, WallSegment, pct};
use std::ops::Range;

/// Walkable footprint (and render face height) of a horizontal (E-W) interior
/// wall, in px. The glass face is drawn from it, so the face and the blocked
/// ground can never drift apart.
pub(crate) const WALL_THICK_H: u16 = 6;
/// Thickness of a vertical (N-S) interior wall, in px — its blocked footprint
/// width AND its drawn width. They are EQUAL by design: seen edge-on, the width
/// you draw IS the wall's real floor thickness, so a walker collides with what
/// they see. Do NOT reintroduce a thinner footprint plus a symmetric routing
/// pad — that decoupling drifted into feet-in-wall on the east and phantom
/// blocked floor on the west. The coarse-router clearance is now the X-only
/// `mask::WALL_ROUTING_MARGIN_X`, stamped at mask time.
pub(crate) const WALL_THICK_V: u16 = 4;

/// North-end walk-behind overhang for a FREE vertical terminus (a segment whose
/// north end is NOT on a joint — e.g. the run below a door): the top rows of the
/// glass are visual-only, so a character parked behind the wall's top cap is
/// occluded by the y-sorted glass. Sized to the E-W wall's cap, deep enough to
/// hide a walker's feet, so the walk-behind reads as depth, not clipping.
pub(crate) const WALL_TOP_OVERHANG_PX: u16 = WALL_THICK_H;

/// A linear wall's geometry policy — the wall analog of a `FurnitureDef` row.
/// Its length is per-SEGMENT so it can't be a `Furniture` enum row, but its
/// blocked-area logic is identical: `footprint ⊆ visual`, the north `cap`
/// visual-only, south-anchored, stamped through the SAME `ground_rect`. A door
/// gap is therefore just the ABSENCE of a segment.
#[derive(Clone, Copy)]
pub(crate) struct WallDef {
    pub(crate) thickness: u16,
    /// Visual-only overhang toward the far (north) side: `footprint = visual −
    /// cap`. A BAND-connected N-S top overrides it to 0.
    pub(crate) cap: u16,
}

pub(crate) const WALL_H: WallDef = WallDef {
    thickness: WALL_THICK_H,
    cap: WALL_THICK_H,
};
pub(crate) const WALL_V: WallDef = WallDef {
    thickness: WALL_THICK_V,
    cap: WALL_TOP_OVERHANG_PX,
};

/// How far a door's jamb runs along its wall: a solid post that reads as one
/// without eating into the opening.
pub(crate) const DOOR_JAMB: u16 = 2;

/// How far BELOW a horizontal wall's row a vertical segment's north end may sit
/// and still bridge UP to it — slack absorbing the off-by-one in the
/// `~WALL_THICK_H` offset `derive_room_walls` applies. Named ONCE so the stitch
/// and the placement sweep's bridge re-derivation can't drift apart.
pub(crate) const WALL_BRIDGE_SLACK_PX: u16 = 2;

/// The horizontal-wall rows that CROSS a vertical run at column `x`: the joints
/// [`stitch_vertical_wall`] may bridge it to. Today the office is single-column
/// so the x-filter is a no-op; without it a multi-column layout would stitch a
/// wall onto an E-W wall in another column.
pub(crate) fn crossing_h_rows(x: u16, room_walls: &[WallSegment]) -> Vec<u16> {
    room_walls
        .iter()
        .filter_map(|w| match *w {
            WallSegment::Horizontal { y, x0, x1 } if (x0..=x1).contains(&x) => Some(y),
            _ => None,
        })
        .collect()
}

/// Stitch a vertical (N-S) wall segment's raw `[seg_top, seg_bot]` to its joints:
///   • Top: a segment starting at `top_margin` is raised to the north window
///     band so no floor shows between window and wall (and A* can't thread the
///     top); one sitting just below a horizontal wall is bridged up to meet it.
///   • Bottom: where the vertical meets a horizontal wall, extend it down by the
///     horizontal's thickness to fill the inside corner, else its east columns
///     leave an L-notch — a walkable bite out of the divider.
/// A caller detects a stitched (jointed) top as `y_top != seg_top`: exactly when
/// the walk-behind cap must be DROPPED (no free floor above to stand behind).
pub(crate) fn stitch_vertical_wall(
    seg_top: u16,
    seg_bot: u16,
    top_margin: u16,
    top_wall_h: u16,
    h_rows: &[u16],
) -> (u16, u16) {
    let y_top = if seg_top == top_margin {
        top_wall_h
    } else if let Some(&hr) = h_rows
        .iter()
        .find(|&&hr| hr < seg_top && seg_top - hr <= WALL_THICK_H + WALL_BRIDGE_SLACK_PX)
    {
        hr
    } else {
        seg_top
    };
    let y_bot = if h_rows.contains(&seg_bot) {
        seg_bot + (WALL_THICK_H - 1)
    } else {
        seg_bot
    };
    (y_top, y_bot)
}

/// One room wall as every painter draws it: where its glass stands, which of
/// its ends a doorway frames, and the bands it sorts in. Built once from
/// [`SceneLayout::room_walls`](crate::layout::SceneLayout::room_walls) and
/// [`SceneLayout::doorways`](crate::layout::SceneLayout::doorways), so no painter
/// re-derives a room's perimeter, closes a doorway, or stands a wall the
/// layout never cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WallPiece {
    /// An E-W wall seen face-on: its face rows start at `y_face`, its
    /// visual-only cap rises [`WALL_H`]'s `cap` rows above them, and it sorts
    /// on its face's last row, so a figure standing north of it paints behind
    /// the glass.
    Horizontal {
        x0: u16,
        x1: u16,
        y_face: u16,
        jamb_west: bool,
        jamb_east: bool,
    },
    /// A N-S wall seen edge-on, [`WALL_THICK_V`] wide, its glass stitched to
    /// its joints `[y_top, y_bot]`. No band of it sorts south of its RAW south
    /// end, `south`: a stitch that runs down into a crossing E-W wall must not
    /// carry the sort row with it, or the edge-on glass paints over that wall.
    Vertical {
        x: u16,
        y_top: u16,
        y_bot: u16,
        /// Its raw north end: the footprint keeps its cap only where the
        /// stitch left `y_top` there.
        north: u16,
        south: u16,
        jamb_north: bool,
        jamb_south: bool,
    },
}

impl WallPiece {
    /// The box its glass fills: an E-W wall's face and the cap above it, or a
    /// N-S wall's stitched run.
    pub(crate) fn visual(self) -> (Point, Size) {
        match self {
            WallPiece::Horizontal { x0, x1, y_face, .. } => (
                Point {
                    x: x0,
                    y: y_face.saturating_sub(WALL_H.cap),
                },
                Size {
                    w: x1 - x0 + 1,
                    h: WALL_H.cap + WALL_H.thickness,
                },
            ),
            WallPiece::Vertical {
                x, y_top, y_bot, ..
            } => (
                Point { x, y: y_top },
                Size {
                    w: WALL_V.thickness,
                    h: y_bot - y_top + 1,
                },
            ),
        }
    }

    /// The stretch of its run, in units from its visual's start, clear of the
    /// walls it joins: a stitched end runs into a crossing wall's box.
    pub(crate) fn clear_run(self) -> Range<u16> {
        match self {
            WallPiece::Horizontal { .. } => 0..self.visual().1.w,
            WallPiece::Vertical {
                y_top,
                y_bot,
                north,
                south,
                ..
            } => {
                let end = if y_bot > south {
                    south.saturating_sub(WALL_H.cap)
                } else {
                    y_bot + 1
                };
                north - y_top..end.max(north) - y_top
            }
        }
    }

    /// The solid posts where a doorway cuts its ends. Each covers its end's own
    /// row or column, since the glass is endpoint-inclusive and a post short of
    /// it would leave a sliver of glass between post and opening; none is wider
    /// than the wall, which a door flush with its run's end leaves one cell long.
    pub(crate) fn jambs(self) -> impl Iterator<Item = (Point, Size)> {
        let (at, size) = self.visual();
        let (start, end, far, post) = match self {
            WallPiece::Horizontal {
                jamb_west,
                jamb_east,
                ..
            } => (
                jamb_west,
                jamb_east,
                Point {
                    x: at.x + size.w - DOOR_JAMB.min(size.w),
                    y: at.y,
                },
                Size {
                    w: DOOR_JAMB.min(size.w),
                    h: size.h,
                },
            ),
            WallPiece::Vertical {
                jamb_north,
                jamb_south,
                ..
            } => (
                jamb_north,
                jamb_south,
                Point {
                    x: at.x,
                    y: at.y + size.h - DOOR_JAMB.min(size.h),
                },
                Size {
                    w: size.w,
                    h: DOOR_JAMB.min(size.h),
                },
            ),
        };
        start
            .then_some((at, post))
            .into_iter()
            .chain(end.then_some((far, post)))
    }

    /// The rect it blocks in the walkable mask. A vertical wall keeps its
    /// visual-only cap only at a FREE north end: a top that the stitch raised to
    /// a joint has no free floor behind it, so a cap there would leave a walkable
    /// notch between the two walls' footprints, a hole through the divider.
    pub(crate) fn footprint(self) -> (Point, Size) {
        let (at, visual) = self.visual();
        let (align_x, cap) = match self {
            WallPiece::Horizontal { .. } => (GroundAlign::Start, WALL_H.cap),
            WallPiece::Vertical { y_top, north, .. } => (
                GroundAlign::Start,
                if y_top == north {
                    // Never eat the whole segment: a short run below a door keeps
                    // at least `WALL_THICK_V` rows blocked, so it stays a divider
                    // and not a second opening.
                    WALL_V.cap.min(visual.h.saturating_sub(WALL_THICK_V))
                } else {
                    0
                },
            ),
        };
        let fp = Size {
            w: visual.w,
            h: visual.h.saturating_sub(cap),
        };
        ground_rect(Pivot::TopLeft, at, fp, visual, align_x, GroundAlign::End)
    }

    /// The bands both painters sort it in among the office's pieces, as
    /// `(rows, depth)`: a horizontal wall whole, on its face's last row; a
    /// vertical one cut into runs of at most [`SORT_BAND_ROWS`], each on its own
    /// last row but never south of `south`. A long wall sorted whole on its
    /// south end paints over a figure beside its northern stretch.
    pub(crate) fn sort_bands(self) -> impl Iterator<Item = (Range<u16>, u16)> {
        let (at, size) = self.visual();
        let end = at.y + size.h;
        let (step, sort) = match self {
            WallPiece::Horizontal { y_face, .. } => (size.h, y_face + (WALL_THICK_H - 1)),
            WallPiece::Vertical { south, .. } => (SORT_BAND_ROWS, south),
        };
        (at.y..end).step_by(usize::from(step)).map(move |y| {
            let bottom = (y + step).min(end);
            (y..bottom, (bottom - 1).min(sort))
        })
    }
}

/// Rows of a vertical wall per [sort band](WallPiece::sort_bands). A band must
/// be no taller than the SHORTEST thing that can pass in front of it: one
/// spanning both sides of a figure has no correct position. This leaves
/// headroom under the bundled cast's height for a shorter pack, at a piece
/// count the draw lists absorb easily.
const SORT_BAND_ROWS: u16 = 4;

/// Every room wall of `room_walls` as a [`WallPiece`], its jambs where
/// `doorways` cut its ends.
pub(crate) fn wall_pieces(
    room_walls: &[WallSegment],
    doorways: &[Doorway],
    top_margin: u16,
) -> Vec<WallPiece> {
    let top_wall_h = crate::layout::wall_band_h(top_margin);
    room_walls
        .iter()
        .map(|&seg| match seg {
            WallSegment::Horizontal { y, x0, x1 } => {
                let cut = |x: u16, at_end: bool| {
                    doorways.iter().any(|d| {
                        d.start.y == y
                            && d.end.y == y
                            && if at_end { d.start.x == x } else { d.end.x == x }
                    })
                };
                WallPiece::Horizontal {
                    x0,
                    x1,
                    y_face: y,
                    jamb_west: cut(x0, false),
                    jamb_east: cut(x1, true),
                }
            }
            WallSegment::Vertical {
                x,
                y0: north,
                y1: south,
            } => {
                let h_rows = crossing_h_rows(x, room_walls);
                let (y_top, y_bot) =
                    stitch_vertical_wall(north, south, top_margin, top_wall_h, &h_rows);
                // Jambs sit on the RAW cut ends: a door cut is never a stitch
                // joint, so they equal the stitched ends wherever one is framed.
                let cut = |y: u16, at_end: bool| {
                    doorways.iter().any(|d| {
                        d.start.x == x
                            && d.end.x == x
                            && if at_end { d.start.y == y } else { d.end.y == y }
                    })
                };
                WallPiece::Vertical {
                    x,
                    y_top,
                    y_bot,
                    north,
                    south,
                    jamb_north: cut(north, false),
                    jamb_south: cut(south, true),
                }
            }
        })
        .collect()
}

/// An opening the resolver CUT into a wall run. The resolver is the one place
/// that knows every door, so it hands the openings to the renderer instead of
/// the painter re-inferring them from segment adjacency. Axis is implicit:
/// `start.x == end.x` ⇒ a vertical wall's doorway (the span is in y), else
/// horizontal (span in x).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Doorway {
    /// One endpoint of the opening (pixel-space).
    pub(crate) start: Point,
    /// The other endpoint (pixel-space).
    pub(crate) end: Point,
}

/// Doorway width in ABSOLUTE pixels — NOT a percentage, which shrinks to nothing
/// on a small terminal and disconnects the room. The cuts are endpoint-inclusive,
/// so the opening is one narrower.
const DOOR_GAP: u16 = 14;

/// Where along its wall run a door sits.
enum DoorAt {
    Centered,
    /// `pct(run length, p)` from the run's start.
    Pct(u16),
}

/// One straight enclosure run a room asks for. Axis-aligned only — the office
/// has no diagonal walls (owner-stated simplification).
enum Run {
    V { x: u16, y0: u16, y1: u16 },
    H { y: u16, x0: u16, x1: u16 },
}

struct WallRequest {
    run: Run,
    doors: Vec<DoorAt>,
}

/// Derive every interior wall from the rooms themselves. `pantry` is only the
/// pantry's BOUNDS: the wall pass runs before the island is placed, so the full
/// `PantryRoom` doesn't exist yet.
pub(crate) fn derive_room_walls(
    meeting_rooms: &[MeetingRoom],
    pantry: Option<Bounds>,
) -> (Vec<WallSegment>, Vec<Doorway>) {
    let mut requests: Vec<WallRequest> = Vec::new();

    for (i, room) in meeting_rooms.iter().enumerate() {
        let b = room.bounds;
        requests.push(WallRequest {
            run: Run::V {
                x: b.x + b.width,
                y0: b.y,
                y1: b.y + b.height,
            },
            doors: vec![DoorAt::Centered],
        });
        let south = Run::H {
            y: b.y + b.height,
            x0: b.x,
            x1: b.x + b.width,
        };
        let below_meeting = meeting_rooms
            .get(i + 1)
            .is_some_and(|r| stacked(b, r.bounds));
        let below_pantry = pantry.is_some_and(|p| stacked(b, p));
        if below_meeting || below_pantry {
            requests.push(WallRequest {
                run: south,
                doors: vec![],
            });
        }
        if i > 0 && stacked(meeting_rooms[i - 1].bounds, b) {
            requests.push(WallRequest {
                run: Run::H {
                    y: b.y,
                    x0: b.x,
                    x1: b.x + b.width,
                },
                doors: vec![], // meeting↔meeting: solid (no door)
            });
        }
    }
    if let Some(p) = pantry {
        let above_meeting = meeting_rooms.iter().any(|r| stacked(r.bounds, p));
        if above_meeting {
            requests.push(WallRequest {
                run: Run::H {
                    y: p.y,
                    x0: p.x,
                    x1: p.x + p.width,
                },
                doors: vec![DoorAt::Pct(60)],
            });
        }
        // No east wall request AT ALL — "the counter is the boundary" is the
        // pantry's honest shape.
    }

    resolve(requests)
}

/// `below` sits directly under `above` (same column, touching edges).
fn stacked(above: Bounds, below: Bounds) -> bool {
    below.y == above.y + above.height && below.x == above.x && below.width == above.width
}

fn resolve(requests: Vec<WallRequest>) -> (Vec<WallSegment>, Vec<Doorway>) {
    // Merge duplicate collinear runs, unioning their doors. Runs that merely
    // TOUCH end-to-end stay SEPARATE so each keeps its own door (two stacked
    // meeting rooms' east walls touch at the split line but are two walls with
    // two corridor doors). Only same-span duplicates collapse.
    let mut merged: Vec<WallRequest> = Vec::new();
    'outer: for req in requests {
        for m in &mut merged {
            if same_run(&m.run, &req.run) {
                m.doors.extend(req.doors);
                continue 'outer;
            }
        }
        merged.push(req);
    }

    // Trim: a vertical run STARTING on a horizontal wall's line begins below
    // that wall's stamped body instead — starting inside it would double-stamp
    // and de-sync the renderer's stitch-up tolerance, which is defined AS
    // WALL_THICK_H.
    let h_runs: Vec<(u16, u16, u16)> = merged
        .iter()
        .filter_map(|r| match r.run {
            Run::H { y, x0, x1 } => Some((y, x0, x1)),
            Run::V { .. } => None,
        })
        .collect();
    for req in &mut merged {
        if let Run::V { x, y0, .. } = &mut req.run {
            // Same line AND the horizontal run actually reaches this column: a
            // coincidental same-y wall in another column must not trim.
            if h_runs
                .iter()
                .any(|&(y, x0, x1)| y == *y0 && (x0..=x1).contains(x))
            {
                *y0 += WALL_THICK_H;
            }
        }
    }

    // Vertical runs first — the render/mask order.
    let (vs, hs): (Vec<_>, Vec<_>) = merged
        .into_iter()
        .partition(|r| matches!(r.run, Run::V { .. }));
    let mut out = Vec::new();
    let mut doorways = Vec::new();
    for req in vs.into_iter().chain(hs) {
        emit(&req, &mut out, &mut doorways);
    }
    (out, doorways)
}

fn same_run(a: &Run, b: &Run) -> bool {
    match (a, b) {
        (
            Run::V { x, y0, y1 },
            Run::V {
                x: x2,
                y0: y02,
                y1: y12,
            },
        ) => x == x2 && y0 == y02 && y1 == y12,
        (
            Run::H { y, x0, x1 },
            Run::H {
                y: y2,
                x0: x02,
                x1: x12,
            },
        ) => y == y2 && x0 == x02 && x1 == x12,
        _ => false,
    }
}

/// Cut the run's door gaps and push the wall left on each side of them.
fn emit(req: &WallRequest, out: &mut Vec<WallSegment>, doorways: &mut Vec<Doorway>) {
    let (start, end) = match req.run {
        Run::V { x: _, y0, y1 } => (y0, y1),
        Run::H { y: _, x0, x1 } => (x0, x1),
    };
    let len = end.saturating_sub(start);
    // Fail LOUD if a future policy unions a second door onto a shared run —
    // silently dropping a requested opening would read as a sealed room.
    debug_assert!(
        req.doors.len() <= 1,
        "multi-door runs are not implemented; a request was dropped"
    );
    let gap = req.doors.first().map(|at| {
        let center = match at {
            DoorAt::Centered => start + len / 2,
            DoorAt::Pct(p) => start + pct(len, *p),
        };
        // Clamped into the run, so a door as wide as its run leaves a one-cell
        // post at each end to frame it.
        (
            center.saturating_sub(DOOR_GAP / 2).max(start),
            (center + DOOR_GAP / 2).min(end),
        )
    });
    if let Some((gs, ge)) = gap {
        doorways.push(match req.run {
            Run::V { x, .. } => Doorway {
                start: Point { x, y: gs },
                end: Point { x, y: ge },
            },
            Run::H { y, .. } => Doorway {
                start: Point { x: gs, y },
                end: Point { x: ge, y },
            },
        });
    }
    let spans: Vec<(u16, u16)> = match gap {
        Some((gs, ge)) => vec![(start, gs), (ge, end)],
        None => vec![(start, end)],
    };
    for (s, e) in spans {
        debug_assert!(s <= e, "a wall run ends before it starts: {s}..={e}");
        out.push(match req.run {
            Run::V { x, .. } => WallSegment::Vertical { x, y0: s, y1: e },
            Run::H { y, .. } => WallSegment::Horizontal { y, x0: s, x1: e },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{MeetingTrio, WALL_BAND_TO_TOP_MARGIN};

    #[test]
    fn stitch_vertical_wall_connects_each_joint() {
        let top_margin = 48u16;
        let top_wall_h = top_margin - 4;
        let h_y = 90u16;
        let h_rows = [h_y];

        let (yt, _) = stitch_vertical_wall(top_margin, 70, top_margin, top_wall_h, &h_rows);
        assert_eq!(
            yt, top_wall_h,
            "top segment should connect up to the window band"
        );

        let (_, yb) = stitch_vertical_wall(60, h_y, top_margin, top_wall_h, &h_rows);
        assert_eq!(
            yb,
            h_y + (WALL_THICK_H - 1),
            "bottom should fill the corner"
        );

        let (yt2, _) = stitch_vertical_wall(h_y + 6, 120, top_margin, top_wall_h, &h_rows);
        assert_eq!(yt2, h_y, "lower segment should bridge up to the cross wall");

        let (yt3, yb3) = stitch_vertical_wall(h_y + 20, 130, top_margin, top_wall_h, &h_rows);
        assert_eq!(
            (yt3, yb3),
            (h_y + 20, 130),
            "distant segment must not bridge"
        );
        let (yt4, yb4) = stitch_vertical_wall(60, 80, top_margin, top_wall_h, &[]);
        assert_eq!((yt4, yb4), (60, 80), "no joints → unchanged");
    }

    #[test]
    fn vertical_wall_top_raise_lands_on_the_band_row() {
        let top_margin = 48u16;
        let tbm = WALL_BAND_TO_TOP_MARGIN;
        let top_wall_h = top_margin - tbm;
        let band_row = top_margin.saturating_sub(tbm);
        let (stitch_raise, _) = stitch_vertical_wall(top_margin, 90, top_margin, top_wall_h, &[]);
        assert_eq!(
            stitch_raise, band_row,
            "the shared stitch must raise a band-rooted vertical wall top to the band row"
        );
    }

    /// `seg`'s mask footprint beside `others`, the walls it can stitch to.
    fn footprint(seg: WallSegment, others: &[WallSegment], top_margin: u16) -> (Point, Size) {
        let walls: Vec<_> = others.iter().copied().chain([seg]).collect();
        wall_pieces(&walls, &[], top_margin)
            .last()
            .expect("seg's piece")
            .footprint()
    }

    #[test]
    fn vertical_wall_free_terminus_reserves_a_north_walk_behind_cap() {
        let top_margin = 20;
        let seg = WallSegment::Vertical {
            x: 56,
            y0: 60,
            y1: 100,
        };
        let (o, s) = footprint(seg, &[], top_margin);
        assert_eq!(o.x, 56, "west edge sits at the wall's x (no west bleed)");
        assert_eq!(s.w, WALL_THICK_V, "footprint width == the drawn width");
        assert_eq!(
            o.y,
            60 + WALL_TOP_OVERHANG_PX,
            "north cap trimmed (south-anchored)"
        );
        assert_eq!(
            s.h,
            (100 - 60 + 1) - WALL_TOP_OVERHANG_PX,
            "height == visual − cap"
        );
    }

    #[test]
    fn vertical_wall_on_the_window_band_is_full_height_and_plugged() {
        let top_margin = 20;
        let seg = WallSegment::Vertical {
            x: 56,
            y0: top_margin,
            y1: 80,
        };
        let (o, s) = footprint(seg, &[], top_margin);
        assert_eq!(o.x, 56);
        assert_eq!(s.w, WALL_THICK_V);
        assert_eq!(
            o.y,
            top_margin - WALL_BAND_TO_TOP_MARGIN,
            "plugged up to the band"
        );
        assert_eq!(
            s.h,
            80 - (top_margin - WALL_BAND_TO_TOP_MARGIN) + 1,
            "full height, no north trim"
        );
    }

    #[test]
    fn vertical_wall_below_a_crossing_wall_drops_its_north_cap() {
        let hwall = WallSegment::Horizontal {
            y: 50,
            x0: 40,
            x1: 56,
        };
        // Trimmed lower segment: starts WALL_THICK_H below the H wall's row.
        let vseg = WallSegment::Vertical {
            x: 56,
            y0: 50 + WALL_THICK_H,
            y1: 100,
        };
        let (capless, _) = footprint(vseg, &[hwall], 20);
        assert_eq!(
            capless.y, 50,
            "north end abuts the H wall ⇒ no cap, blocked top BRIDGED onto the H wall row"
        );
        let (capped, _) = footprint(vseg, &[], 20);
        assert_eq!(
            capped.y,
            50 + WALL_THICK_H + WALL_TOP_OVERHANG_PX,
            "a genuinely free north terminus still reserves the walk-behind cap"
        );
    }

    #[test]
    fn vertical_wall_meeting_a_horizontal_at_its_bottom_extends_to_fill_the_corner() {
        let hwall = WallSegment::Horizontal {
            y: 80,
            x0: 40,
            x1: 56,
        };
        let vseg = WallSegment::Vertical {
            x: 56,
            y0: 40,
            y1: 80,
        };
        // seg_bot (80) sits on the crossing H-wall row ⇒ the bottom stitch fires.
        let (o, s) = footprint(vseg, &[hwall], 20);
        assert_eq!(
            o.y + s.h - 1,
            80 + (WALL_THICK_H - 1),
            "south edge extends WALL_THICK_H-1 below seg_bot to fill the inside corner"
        );
        let (o2, s2) = footprint(vseg, &[], 20);
        assert_eq!(
            o2.y + s2.h - 1,
            80,
            "no crossing wall at the bottom ⇒ footprint ends at seg_bot (no extension)"
        );
    }

    #[test]
    fn horizontal_wall_rect_is_full_face_unchanged() {
        let seg = WallSegment::Horizontal {
            y: 50,
            x0: 20,
            x1: 60,
        };
        let (o, s) = footprint(seg, &[], 20);
        assert_eq!((o.x, o.y), (20, 50));
        assert_eq!((s.w, s.h), (60 - 20 + 1, WALL_THICK_H));
    }

    fn room(x: u16, y: u16, w: u16, h: u16) -> MeetingRoom {
        MeetingRoom {
            bounds: Bounds {
                x,
                y,
                width: w,
                height: h,
            },
            trio: None::<MeetingTrio>,
        }
    }

    /// The E-W runs of `walls`, as `(x0, x1)`.
    fn h_runs(walls: &[WallSegment]) -> Vec<(u16, u16)> {
        walls
            .iter()
            .filter_map(|w| match *w {
                WallSegment::Horizontal { x0, x1, .. } => Some((x0, x1)),
                WallSegment::Vertical { .. } => None,
            })
            .collect()
    }

    /// The N-S runs of `walls`, as `(y0, y1)`.
    fn v_runs(walls: &[WallSegment]) -> Vec<(u16, u16)> {
        walls
            .iter()
            .filter_map(|w| match *w {
                WallSegment::Vertical { y0, y1, .. } => Some((y0, y1)),
                WallSegment::Horizontal { .. } => None,
            })
            .collect()
    }

    #[test]
    fn dense_shared_wall_resolves_once_and_solid() {
        let rooms = [room(0, 20, 40, 30), room(0, 50, 40, 30)];
        let (walls, _) = derive_room_walls(&rooms, None);
        let h = h_runs(&walls);
        assert_eq!(h.len(), 1, "one horizontal wall, not two: {h:?}");
        assert_eq!(
            h[0],
            (0, 40),
            "solid across the full span — no inter-meeting door"
        );
    }

    #[test]
    fn pantry_door_survives_and_every_enclosed_room_has_a_door() {
        let rooms = [room(0, 20, 40, 30)];
        let pantry = Some(Bounds {
            x: 0,
            y: 50,
            width: 40,
            height: 30,
        });
        let (walls, doorways) = derive_room_walls(&rooms, pantry);
        let h = h_runs(&walls);
        assert_eq!(h.len(), 2, "the 60% door splits the shared wall: {h:?}");
        let gap = (h[0].1, h[1].0);
        let door_center = pct(40, 60);
        assert_eq!(
            gap,
            (door_center - DOOR_GAP / 2, door_center + DOOR_GAP / 2)
        );
        let v = v_runs(&walls);
        assert_eq!(v.len(), 2, "east wall split by the centered door");
        assert!(
            v[0].1 < v[1].0,
            "a real gap exists — the meeting room is never sealed"
        );
        assert_eq!(doorways.len(), 2, "one Doorway per cut opening");
        let v_door = doorways
            .iter()
            .find(|d| d.start.x == d.end.x)
            .expect("east door");
        assert_eq!((v_door.start.y, v_door.end.y), (v[0].1, v[1].0));
        let h_door = doorways
            .iter()
            .find(|d| d.start.y == d.end.y)
            .expect("60% door");
        assert_eq!((h_door.start.x, h_door.end.x), gap);
    }

    #[test]
    fn a_door_as_wide_as_its_run_leaves_a_post_at_each_end_on_its_axis() {
        let (walls, doorways) = derive_room_walls(&[room(0, 20, 22, DOOR_GAP - 1)], None);
        let end = 20 + DOOR_GAP - 1;
        assert_eq!(
            walls,
            [
                WallSegment::Vertical {
                    x: 22,
                    y0: 20,
                    y1: 20
                },
                WallSegment::Vertical {
                    x: 22,
                    y0: end,
                    y1: end
                },
            ]
        );
        assert_eq!(
            doorways,
            [Doorway {
                start: Point { x: 22, y: 20 },
                end: Point { x: 22, y: end },
            }]
        );
    }

    #[test]
    fn vertical_run_trims_below_crossing_horizontal_wall() {
        let rooms = [room(0, 20, 40, 30), room(0, 50, 40, 30)];
        let (walls, _) = derive_room_walls(&rooms, None);
        let v = v_runs(&walls);
        // room 0's pair spans [20, 50]; room 1's pair starts BELOW the wall.
        assert_eq!(v[0].0, 20);
        assert_eq!(v[1].1, 50);
        let trimmed_top = 50 + WALL_THICK_H;
        assert_eq!(v[2].0, trimmed_top, "trimmed below the shared wall");
        assert_eq!(v[3].1, 80);
        let c = trimmed_top + (80 - trimmed_top) / 2;
        assert_eq!(
            (v[2].1, v[3].0),
            (c - DOOR_GAP / 2, c + DOOR_GAP / 2),
            "door centers on the trimmed run (legacy v2_center)"
        );
    }

    #[test]
    fn open_plan_requests_nothing() {
        assert!(derive_room_walls(&[], None).0.is_empty());
        let (w, d) = derive_room_walls(
            &[],
            Some(Bounds {
                x: 0,
                y: 20,
                width: 40,
                height: 60,
            }),
        );
        assert!(w.is_empty() && d.is_empty());
    }

    #[test]
    fn a_walls_sort_bands_tile_its_glass_none_sorting_south_of_its_raw_end() {
        let mut met = 0;
        for &(w, h) in crate::layout::roster::tests::CENSUS_SIZES {
            for seed in 0..12 {
                let l = crate::layout::SceneLayout::compute_with_seed(w, h, None, seed)
                    .expect("lays out");
                for &piece in &l.wall_pieces {
                    met += 1;
                    let (at, size) = piece.visual();
                    let mut next = at.y;
                    for (rows, depth) in piece.sort_bands() {
                        let at = format!("{w}x{h} seed {seed}: {piece:?} {rows:?}");
                        assert_eq!(rows.start, next, "{at}: tiles");
                        assert!(rows.start < rows.end && depth < rows.end, "{at}");
                        match piece {
                            WallPiece::Horizontal { y_face, .. } => {
                                assert_eq!(rows.len(), usize::from(size.h), "{at}: whole");
                                assert_eq!(depth, y_face + WALL_THICK_H - 1, "{at}");
                            }
                            WallPiece::Vertical { south, .. } => {
                                assert!(rows.len() <= usize::from(SORT_BAND_ROWS), "{at}");
                                assert!(depth <= south, "{at}");
                            }
                        }
                        next = rows.end;
                    }
                    assert_eq!(
                        next,
                        at.y + size.h,
                        "{w}x{h} seed {seed}: {piece:?} covered"
                    );
                }
            }
        }
        assert!(met > 0, "the sweep met a wall");
    }

    #[test]
    fn a_stitched_walls_bands_below_its_raw_end_sort_on_it() {
        let piece = WallPiece::Vertical {
            x: 0,
            y_top: 0,
            y_bot: 15,
            north: 0,
            south: 10,
            jamb_north: false,
            jamb_south: false,
        };
        assert_eq!(
            piece.sort_bands().collect::<Vec<_>>(),
            [(0..4, 3), (4..8, 7), (8..12, 10), (12..16, 10)]
        );
    }
}
