//! The north wall's floor-to-ceiling windows: where each one stands, in the
//! layout's own units, so every painter tiles the same windows and the elevator
//! door stands in the last one's slot, on their bottom line.

use std::ops::Range;

use super::{Bounds, ELEVATOR_H, ELEVATOR_W, NEON_PANEL, SceneLayout, Size};

/// A window's narrowest width, frame included: the wall left over widens
/// them all, so the skyline detail reads the same on every terminal.
pub(crate) const WINDOW_W: u16 = 22;
/// The frame post between two window slots.
const POST_W: u16 = 3;
/// The first window's left edge; the neon sign hangs in front of it.
const FIRST_WINDOW_X: u16 = 3;
/// The door's inset from its slot's west edge, centring it in the slot.
const DOOR_INSET: u16 = (WINDOW_W - ELEVATOR_W) / 2;
/// The wall east of the door's slot.
const WINDOW_EDGE_MARGIN: u16 = 2;
/// How far west of the wall's east end the door's slot starts.
const DOOR_SLOT_EAST: u16 = WINDOW_W + WINDOW_EDGE_MARGIN;
/// The narrowest wall whose door's slot stands clear of the neon.
pub(crate) const NEON_DOOR_WALL_W: u16 = NEON_PANEL.x + NEON_PANEL.width + DOOR_SLOT_EAST;
/// The windows' top frame row; one wall row stands above it.
pub(crate) const WINDOW_TOP: u16 = 1;
/// How thick a window's outer frame is, on every side.
const FRAME_W: u16 = 1;

/// One window on the north wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct WindowBay {
    /// The window's left edge.
    pub(crate) x: u16,
    /// The window's width, frame included.
    pub(crate) w: u16,
    /// The window's place in the run, counted from the west.
    pub(crate) idx: u16,
}

impl WindowBay {
    /// The window's middle column, its centre mullion ([`window_frame`]).
    pub(crate) fn center_x(self) -> u16 {
        self.x + self.w / 2
    }

    /// The columns the window spans, frame included.
    pub(crate) fn span(self) -> Range<u16> {
        self.x..self.x + self.w
    }

    /// The glass columns of its two panes, west then east of the centre
    /// mullion.
    pub(crate) fn panes(self) -> [Range<u16>; 2] {
        let mullion = self.center_x();
        [
            self.x + FRAME_W..mullion,
            mullion + 1..self.span().end - FRAME_W,
        ]
    }

    /// The glass inside its outer frame over the window `rows`, its mullion
    /// and transom included.
    pub(crate) fn glass_box(self, rows: Range<u16>) -> Bounds {
        Bounds {
            x: self.x + FRAME_W,
            y: rows.start + FRAME_W,
            width: self.w.saturating_sub(2 * FRAME_W),
            height: glass_rows(rows.end.saturating_sub(rows.start)),
        }
    }
}

/// The door's slot's left edge on a wall `buf_w` wide: flush with the east
/// margin.
fn door_slot_x(buf_w: u16) -> Option<u16> {
    buf_w.checked_sub(DOOR_SLOT_EAST)
}

/// Every window slot a wall `buf_w` wide fits, left to right, ending in the
/// door's, [`WINDOW_W`] wide: as many windows as fit west of it with a
/// [`POST_W`] post east of each, widened evenly by the wall left over, the
/// easternmost taking the columns that don't divide.
pub(crate) fn window_slots(buf_w: u16) -> impl Iterator<Item = WindowBay> {
    let door = door_slot_x(buf_w).filter(|&x| x >= FIRST_WINDOW_X);
    let span = door.map_or(0, |x| x - FIRST_WINDOW_X);
    let windows = span / (WINDOW_W + POST_W);
    let spare = span - windows * (WINDOW_W + POST_W);
    let narrow = windows - spare % windows.max(1);
    let mut x = FIRST_WINDOW_X;
    (0..windows)
        .map(move |idx| {
            let w = WINDOW_W + spare / windows + u16::from(idx >= narrow);
            let bay = WindowBay { x, w, idx };
            x += w + POST_W;
            bay
        })
        .chain(door.map(|x| WindowBay {
            x,
            w: WINDOW_W,
            idx: windows,
        }))
}

/// The windows a wall `buf_w` wide shows, left to right: every slot less those
/// `door` overlaps, whose glass would otherwise show through the elevator's
/// frame.
pub(crate) fn window_bays(buf_w: u16, door: Range<u16>) -> impl Iterator<Item = WindowBay> {
    window_slots(buf_w).filter(move |b| !(b.x < door.end && b.span().end > door.start))
}

/// The columns from the first slot's left edge to the last one's right, the
/// door's included; the first window alone when none fits.
pub(crate) fn window_run(buf_w: u16) -> Range<u16> {
    let mut slots = window_slots(buf_w);
    let first = slots
        .next()
        .map_or(FIRST_WINDOW_X..FIRST_WINDOW_X + WINDOW_W, WindowBay::span);
    let end = slots.last().map_or(first.end, |s| s.span().end);
    first.start..end
}

/// The frame posts between neighbouring window slots, left to right.
pub(crate) fn window_posts(buf_w: u16) -> impl Iterator<Item = Range<u16>> {
    window_slots(buf_w)
        .zip(window_slots(buf_w).skip(1))
        .map(|(west, east)| west.span().end..east.x)
}

/// The elevator door's left column on a wall `buf_w` wide: centred in the
/// door's slot, which stands even where no window run fits west of it.
pub(crate) fn door_x(buf_w: u16) -> u16 {
    door_slot_x(buf_w).expect("every wall that lays out holds the door's slot") + DOOR_INSET
}

/// The wall band's trim row, where the band meets the floor.
pub(crate) const fn wall_trim_row(band_h: u16) -> u16 {
    band_h.saturating_sub(1)
}

/// The rows a window spans on a wall band `band_h` tall, frame included: from
/// [`WINDOW_TOP`] down to the band's trim row, so a taller terminal gets taller
/// glass.
pub(crate) fn window_rows(band_h: u16) -> Range<u16> {
    WINDOW_TOP..wall_trim_row(band_h).max(WINDOW_TOP)
}

/// The glass rows of a window `window_h` tall: all but its top and bottom
/// frame rows.
pub(crate) fn glass_rows(window_h: u16) -> u16 {
    window_h.saturating_sub(2 * FRAME_W)
}

/// Where a window's transom crosses it, in percent of its height from the top.
const TRANSOM_PCT: u16 = 70;

/// Whether the cell `(dx, dy)` of a window `size` big is its frame — its
/// edge, its centre mullion or its transom — which no glass shows through.
pub(crate) fn window_frame(dx: u16, dy: u16, size: Size) -> bool {
    let Size { w, h } = size;
    dx < FRAME_W
        || dx + FRAME_W >= w
        || dy < FRAME_W
        || dy + FRAME_W >= h
        || dx == w / 2
        || dy == h * TRANSOM_PCT / 100
}

impl SceneLayout {
    /// The windows this office's north wall shows, left to right: every slot
    /// but the door's.
    pub(crate) fn window_bays(&self) -> impl Iterator<Item = WindowBay> + use<> {
        let door = self.door_rect();
        window_bays(self.buf_w, door.x..door.x + door.width)
    }

    /// The box the elevator door's art covers.
    pub(crate) fn door_rect(&self) -> Bounds {
        Bounds {
            x: self.door.x,
            y: self.door.y,
            width: ELEVATOR_W,
            height: ELEVATOR_H,
        }
    }

    /// Whether cell `(x, y)` is a window's glass, where the outside shows
    /// rather than the room: inside a bay and off its [`window_frame`].
    #[cfg(test)]
    pub(crate) fn glass_at(&self, x: u16, y: u16) -> bool {
        let rows = window_rows(self.wall_band_h());
        let h = rows.end - rows.start;
        rows.contains(&y)
            && self.window_bays().any(|b| {
                b.span().contains(&x) && !window_frame(x - b.x, y - rows.start, Size { w: b.w, h })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_share_the_wall_west_of_the_door_slot_evenly() {
        let mut walls_with_a_window = 0;
        for buf_w in 0..400 {
            let slots: Vec<_> = window_slots(buf_w).collect();
            let Some((door, windows)) = slots.split_last() else {
                assert!(
                    door_slot_x(buf_w).is_none_or(|x| x < FIRST_WINDOW_X),
                    "{buf_w}"
                );
                continue;
            };
            assert_eq!(
                Some(door.x),
                door_slot_x(buf_w),
                "{buf_w}: the door's slot ends it"
            );
            assert_eq!(door.w, WINDOW_W, "{buf_w}");
            assert!(
                windows.first().is_none_or(|b| b.x == FIRST_WINDOW_X),
                "{buf_w}"
            );
            assert!(
                window_posts(buf_w).all(|p| p.len() == usize::from(POST_W)),
                "{buf_w}: thin posts"
            );
            for (k, s) in slots.iter().enumerate() {
                assert_eq!(usize::from(s.idx), k);
            }
            if let Some((min, max)) = windows
                .iter()
                .map(|b| b.w)
                .min()
                .zip(windows.iter().map(|b| b.w).max())
            {
                walls_with_a_window += 1;
                assert!(min >= WINDOW_W && max - min <= 1, "{buf_w}: {windows:?}");
            }
            assert_eq!(
                windows.len(),
                usize::from((door.x - FIRST_WINDOW_X) / (WINDOW_W + POST_W)),
                "{buf_w}: as many windows as fit west of the door's slot"
            );
            assert_eq!(window_run(buf_w), slots[0].x..door.span().end);
            assert_eq!(door_x(buf_w), door.x + DOOR_INSET);
        }
        assert!(walls_with_a_window > 0);
    }

    #[test]
    fn a_door_takes_exactly_the_window_it_overlaps() {
        let all: Vec<_> = window_bays(240, 0..0).collect();
        let doomed = all[2];
        let kept: Vec<_> = window_bays(240, doomed.span()).collect();
        assert_eq!(kept.len() + 1, all.len());
        assert!(kept.iter().all(|b| b.idx != doomed.idx));
    }

    #[test]
    fn a_post_fills_each_gap_between_window_slots() {
        let buf_w = 240;
        let slots: Vec<_> = window_slots(buf_w).collect();
        let posts: Vec<_> = window_posts(buf_w).collect();
        assert_eq!(posts.len() + 1, slots.len());
        for (post, pair) in posts.iter().zip(slots.windows(2)) {
            assert_eq!(*post, pair[0].span().end..pair[1].x);
        }
    }

    #[test]
    fn a_pane_is_the_glass_between_two_frame_columns() {
        for w in [WINDOW_W, WINDOW_W + 1] {
            let bay = WindowBay { x: 10, w, idx: 0 };
            let (h, glass_row) = (20, 5);
            let frame = |x: u16| window_frame(x - bay.x, glass_row, Size { w, h });
            for pane in bay.panes() {
                assert!(pane.clone().all(|x| !frame(x)), "{w}: {pane:?}");
                assert!(frame(pane.start - 1) && frame(pane.end), "{w}: {pane:?}");
            }
        }
    }

    #[test]
    fn a_wall_too_narrow_for_a_window_runs_the_first_window_alone() {
        assert_eq!(window_bays(WINDOW_W, 0..0).count(), 0);
        assert_eq!(
            window_run(WINDOW_W),
            FIRST_WINDOW_X..FIRST_WINDOW_X + WINDOW_W
        );
    }

    #[test]
    fn a_window_runs_from_its_top_row_to_the_trim() {
        let band_h = 30;
        assert_eq!(window_rows(band_h), WINDOW_TOP..wall_trim_row(band_h));
        assert!(
            window_rows(0).is_empty(),
            "a band with no room has no window"
        );
    }

    #[test]
    fn the_widest_wall_tiles_to_its_door_slot() {
        assert_eq!(
            window_slots(u16::MAX).last().map(|s| s.x),
            door_slot_x(u16::MAX)
        );
    }

    /// The door is placed from the window rows, and nothing else pins where it
    /// ends up.
    #[test]
    fn the_door_stands_on_the_wall_trim() {
        for (w, h) in [(192, 80), (140, 60), (250, 90)] {
            let layout = SceneLayout::compute(w, h, Some(4)).expect("layout fits");
            let door = layout.door;
            assert_eq!(
                door.y + super::super::ELEVATOR_H - 1,
                wall_trim_row(layout.wall_band_h()),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn glass_is_a_bays_cells_off_its_frame() {
        let layout =
            crate::layout::SceneLayout::compute(192, 160, Some(crate::layout::TEST_DEFAULT_DESKS))
                .expect("192x160 fits");
        let rows = window_rows(layout.wall_band_h());
        let h = rows.end - rows.start;
        for bay in layout.window_bays() {
            let size = Size { w: bay.w, h };
            for dy in 0..h {
                for dx in 0..bay.w {
                    assert_eq!(
                        layout.glass_at(bay.x + dx, rows.start + dy),
                        !window_frame(dx, dy, size),
                        "({dx}, {dy}) of bay {}",
                        bay.idx
                    );
                }
            }
            assert!(
                !layout.glass_at(bay.x + bay.w, rows.start + 2),
                "the post past bay {}",
                bay.idx
            );
            assert!(
                !layout.glass_at(bay.x + 2, rows.end),
                "the trim below bay {}",
                bay.idx
            );
        }
    }
}
