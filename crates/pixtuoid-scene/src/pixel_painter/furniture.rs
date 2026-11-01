//! Furniture the pixel painter stamps procedurally, without pack art.

use crate::sprite::{Rgb, RgbBuffer};

use crate::anim::FULL_TICK_MS;
use crate::layout::{Bounds, COAT_HOOK_DX, COAT_RACK_BASE_DY, COAT_W};
use crate::pack::COOLER_WATER;

/// Bordered area rug filling `rug` — the meeting and lounge rugs and both
/// pantry mats.
pub(crate) fn paint_area_rug(
    buf: &mut RgbBuffer,
    rug: crate::layout::Bounds,
    theme: &crate::theme::Theme,
) {
    let f = &theme.furniture;
    for dy in 0..rug.height {
        for dx in 0..rug.width {
            let (x, y) = (rug.x.saturating_add(dx), rug.y.saturating_add(dy));
            if x >= buf.width() || y >= buf.height() {
                continue;
            }
            let on_border = dx == 0 || dx + 1 == rug.width || dy == 0 || dy + 1 == rug.height;
            let on_inner_border = dx == 1 || dx + 2 == rug.width || dy == 1 || dy + 2 == rug.height;
            let color = if on_border {
                f.rug_trim
            } else if on_inner_border {
                f.rug_accent
            } else {
                f.rug_field
            };
            buf.put(x, y, color);
        }
    }
}

/// Lounge side table next to the viewing couch, opposite the floor lamp, with a
/// magazine stack on top so the silhouette reads as "side table with a book".
pub(super) fn paint_side_table(buf: &mut RgbBuffer, cx: u16, cy: u16, theme: &crate::theme::Theme) {
    let top = theme.furniture.wood_top;
    let trim = theme.furniture.wood_trim;
    let mag = theme.furniture.magazine;
    let mag_trim = theme.furniture.magazine_trim;
    // Dims come from the one furniture table, which the hover box reads too.
    let size = crate::layout::furniture_def(crate::layout::Furniture::LoungeSideTable).visual;
    let (w, h) = (i32::from(size.w), i32::from(size.h));
    for dy in 0..h {
        for dx in 0..w {
            let px = i32::from(cx) - w / 2 + dx;
            let py = i32::from(cy) - h / 2 + dy;
            if px < 0 || py < 0 || px >= i32::from(buf.width()) || py >= i32::from(buf.height()) {
                continue;
            }
            let on_bottom = dy == h - 1;
            buf.put(px as u16, py as u16, if on_bottom { trim } else { top });
        }
    }
    let mag_pixels: &[((i32, i32), Rgb)] = &[
        ((-1, -1), mag),
        ((0, -1), mag),
        ((1, -1), mag),
        ((-1, 0), mag_trim),
        ((0, 0), mag_trim),
        ((1, 0), mag_trim),
    ];
    for ((dx, dy), c) in mag_pixels {
        let px = i32::from(cx) + dx;
        let py = i32::from(cy) + dy;
        if px >= 0 && py >= 0 && (px as u16) < buf.width() && (py as u16) < buf.height() {
            buf.put(px as u16, py as u16, *c);
        }
    }
}

/// Kitchen island — the pantry's counter-height centre piece; ALL dims read from
/// the FurnitureDef row. The mask blocks only the south-anchored base
/// (invariant #6).
pub(super) fn paint_kitchen_island(
    buf: &mut RgbBuffer,
    cx: u16,
    cy: u16,
    theme: &crate::theme::Theme,
) {
    let top = theme.furniture.wood_top;
    let body = theme.furniture.wood_trim;
    let shade = theme.furniture.chair_trim;
    let accents = theme.appliance.vending_drinks;
    let vis = crate::layout::furniture_def(crate::layout::Furniture::KitchenIsland).visual;
    let (w, h) = (i32::from(vis.w), i32::from(vis.h));
    for dy in 0..h {
        for dx in 0..w {
            let on_corner = (dx == 0 || dx == w - 1) && (dy == 0 || dy == h - 1);
            if on_corner {
                continue;
            }
            let px = i32::from(cx) - w / 2 + dx;
            let py = i32::from(cy) - h / 2 + dy;
            if px < 0 || py < 0 || px >= i32::from(buf.width()) || py >= i32::from(buf.height()) {
                continue;
            }
            // Rows 2+ inset 1px per side so the countertop reads as overhanging.
            if dy >= 2 && (dx == 0 || dx == w - 1) {
                continue;
            }
            let color = if dy < 2 {
                top
            } else if dy == h - 1 {
                shade
            } else {
                body
            };
            buf.put(px as u16, py as u16, color);
        }
    }
    // Front detail: door seams + handles so the body reads as cabinetry, not a slab.
    let putxy = |buf: &mut RgbBuffer, dx: i32, dy: i32, c: Rgb| {
        let px = i32::from(cx) - w / 2 + dx;
        let py = i32::from(cy) - h / 2 + dy;
        if px >= 0 && py >= 0 && (px as u16) < buf.width() && (py as u16) < buf.height() {
            buf.put(px as u16, py as u16, c);
        }
    };
    for dy in 2..(h - 1) {
        putxy(buf, w / 2, dy, shade); // centre seam splits two doors
    }
    putxy(buf, w / 2 - 2, 3, shade); // left door handle
    putxy(buf, w / 2 + 2, 3, shade); // right door handle
    putxy(buf, 3, 0, accents[0]);
    putxy(buf, 4, 0, accents[1]);
    // One mug — a THIRD accent so it can't blend into the fruit pair (the
    // vending panel color is theme-dependent and collided in default).
    putxy(buf, w - 5, 0, accents[2]);
}

/// Notice board filling `board`, the box
/// [`SceneLayout::notice_board_rect`](crate::layout::SceneLayout::notice_board_rect)
/// places and gates.
pub(super) fn paint_notice_board(buf: &mut RgbBuffer, board: Bounds, theme: &crate::theme::Theme) {
    let wall_color = theme.office.room_wall_trim_dark;
    let accent = theme.furniture.rug_accent;
    for dy in 0..board.height {
        for dx in 0..board.width {
            let px = board.x + dx;
            let py = board.y + dy;
            if px < buf.width() && py < buf.height() {
                let on_edge = dx == 0 || dx == board.width - 1 || dy == 0 || dy == board.height - 1;
                buf.put(px, py, if on_edge { wall_color } else { accent });
            }
        }
    }
}

/// Small doormat filling `mat`, its fixture's box.
pub(super) fn paint_doormat(buf: &mut RgbBuffer, mat: Bounds, theme: &crate::theme::Theme) {
    let mat_color = theme.furniture.rug_trim;
    let mat_accent = theme.furniture.rug_field;
    for dy in 0..mat.height {
        for dx in 0..mat.width {
            let px = mat.x + dx;
            let py = mat.y + dy;
            if px < buf.width() && py < buf.height() {
                let on_border = dx == 0 || dx == mat.width - 1 || dy == 0 || dy == mat.height - 1;
                buf.put(px, py, if on_border { mat_color } else { mat_accent });
            }
        }
    }
}

/// Water cooler filling `cooler`, the box
/// [`PantryRoom::water_cooler_rect`](crate::layout::PantryRoom::water_cooler_rect)
/// places and gates.
pub(super) fn paint_water_cooler(
    buf: &mut RgbBuffer,
    cooler: Bounds,
    beat: crate::anim::Beat,
    theme: &crate::theme::Theme,
) {
    let cooler_body = theme.office.building_light;
    let cooler_water = COOLER_WATER;
    const BOTTLE_ROWS: u16 = 3;
    let (wx, wy) = (cooler.x, cooler.y);
    for dy in 0..cooler.height {
        for dx in 0..cooler.width {
            let px = wx + dx;
            let py = wy + dy;
            if px < buf.width() && py < buf.height() {
                let color = if dy < BOTTLE_ROWS {
                    cooler_water
                } else {
                    cooler_body
                };
                buf.put(px, py, color);
            }
        }
    }
    // Ambient glug: a bubble climbs the bottle each cycle. Reusing
    // tank_water_line also keeps it off the mascot harness's bubble sentinel.
    const GLUG_STEP_MS: u64 = 3 * FULL_TICK_MS;
    const GLUG_CYCLE_MS: u64 = 5 * GLUG_STEP_MS;
    let phase = (beat.ms() % GLUG_CYCLE_MS) / GLUG_STEP_MS;
    if phase < 2 {
        let (bx, by) = (wx + 1, wy + 1 - phase as u16);
        if bx < buf.width() && by < buf.height() {
            buf.put(bx, by, theme.furniture.tank_water_line);
        }
    }
}

/// Trash bin filling `bin`, the box
/// [`PantryRoom::trash_bin_rect`](crate::layout::PantryRoom::trash_bin_rect)
/// places and gates. Its colours are intentionally un-themed neutral greys (a
/// semantic object, like the water bottle's blue), so it takes no theme.
pub(super) fn paint_trash_bin(buf: &mut RgbBuffer, bin: Bounds) {
    let (tx, ty) = (bin.x, bin.y);
    let bin_outer = Rgb {
        r: 70,
        g: 70,
        b: 78,
    };
    let bin_rim = Rgb {
        r: 100,
        g: 100,
        b: 108,
    };
    let bag_liner = Rgb {
        r: 200,
        g: 200,
        b: 210,
    };
    let bag_fill = Rgb {
        r: 160,
        g: 160,
        b: 170,
    };
    // Loop bounds derive from the rect so the painted box can't drift from the
    // hover box.
    for dy in 0..bin.height {
        for dx in 0..bin.width {
            let px = tx + dx;
            let py = ty + dy;
            if px < buf.width() && py < buf.height() {
                let on_edge = dx == 0 || dx == bin.width - 1;
                let color = if dy == 0 {
                    // Rim row.
                    if on_edge { bin_rim } else { bag_liner }
                } else if dy == 1 {
                    // Bag-liner peek.
                    if on_edge { bin_outer } else { bag_fill }
                } else {
                    // Body.
                    bin_outer
                };
                buf.put(px, py, color);
            }
        }
    }
}

/// Aquarium on a low cabinet: theme water behind a shared-dark frame, two fish
/// patrolling opposite lanes on the anim clock, a rising bubble and a plant sprig.
/// Geometry derives from the `FishTank` furniture row.
pub(super) fn paint_fish_tank(
    buf: &mut RgbBuffer,
    pos: crate::layout::Point,
    beat: crate::anim::Beat,
    theme: &crate::theme::Theme,
) {
    use crate::layout::{Furniture, furniture_def};
    let def = furniture_def(Furniture::FishTank);
    let (w, h) = (def.visual.w, def.visual.h);
    let x0 = pos.x.saturating_sub(w / 2);
    let y0 = pos.y.saturating_sub(h / 2);
    let frame = theme.office.room_wall_trim_dark;
    let fc = &theme.furniture;
    let mut put = |dx: u16, dy: u16, c: Rgb| {
        let (px, py) = (x0 + dx, y0 + dy);
        if px < buf.width() && py < buf.height() {
            buf.put(px, py, c);
        }
    };
    for dx in 0..w {
        put(dx, 0, frame);
        put(dx, h - 3, frame);
    }
    for dy in 1..=(h - 4) {
        put(0, dy, frame);
        put(w - 1, dy, frame);
        for dx in 1..w - 1 {
            let c = if dy == 1 {
                fc.tank_water_line
            } else if dy == h - 4 {
                if dx % 2 == 0 {
                    fc.wood_trim
                } else {
                    fc.wood_top
                }
            } else {
                fc.tank_water
            };
            put(dx, dy, c);
        }
    }
    for dx in 0..w {
        put(
            dx,
            h - 2,
            if dx == w / 2 {
                fc.wood_trim
            } else {
                fc.wood_top
            },
        );
        put(dx, h - 1, fc.wood_trim);
    }
    // Fish patrol: a triangle wave over the interior span, one lane each. Distinct
    // periods (and a phase offset) keep the pair from mirroring in lockstep.
    let t = beat.ms();
    const FISH_STEP_MS: u64 = 3 * FULL_TICK_MS;
    const FISH_ALT_STEP_MS: u64 = 4 * FULL_TICK_MS;
    const FISH_ALT_PHASE_STEPS: u64 = 7;
    const BUBBLE_RISE_STEP_MS: u64 = 2 * FULL_TICK_MS;
    let span = u64::from(w - 5);
    let mut fish = |lane_dy: u16, color: Rgb, step_ms: u64, phase: u64| {
        let cycle = span * 2;
        let step = ((t / step_ms) + phase) % cycle;
        let start = if step < span {
            1 + step as u16
        } else {
            1 + (cycle - step) as u16
        };
        for dx in start..start + 3 {
            put(dx, lane_dy, color);
        }
    };
    fish(3, fc.tank_fish, FISH_STEP_MS, 0);
    fish(5, fc.tank_fish_alt, FISH_ALT_STEP_MS, FISH_ALT_PHASE_STEPS);
    let bubble_dy = (h - 5) - ((t / BUBBLE_RISE_STEP_MS) % (u64::from(h) - 6)) as u16;
    put(w - 3, bubble_dy, fc.tank_water_line);
    // Plant sprig last so the fish swim behind it.
    put(2, 5, fc.tank_plant);
    put(2, 6, fc.tank_plant);
    put(2, 7, fc.tank_plant);
    put(3, 6, fc.tank_plant);
}

/// Head-of-table meeting chair centered on its MeetingChair waypoint. The
/// backrest bar rides the OUTER side (`back_west`), so it carries the sitter's
/// orientation even when the chair is empty.
pub(super) fn paint_meeting_chair(
    buf: &mut RgbBuffer,
    pos: crate::layout::Point,
    back_west: bool,
    theme: &crate::theme::Theme,
) {
    let fc = &theme.furniture;
    let chair = crate::layout::furniture_def(crate::layout::Furniture::MeetingChair).visual;
    let (x0, y0) = (
        pos.x.saturating_sub(chair.w / 2),
        pos.y.saturating_sub(chair.h / 2),
    );
    let mut put = |dx: u16, dy: u16, c: Rgb| {
        let (px, py) = (x0 + dx, y0 + dy);
        if px < buf.width() && py < buf.height() {
            buf.put(px, py, c);
        }
    };
    let back_dx = if back_west { 0 } else { chair.w - 1 };
    for dy in 0..5u16 {
        put(back_dx, dy, fc.chair_trim);
    }
    for dy in 1..5u16 {
        for dx in 1..chair.w - 1 {
            let c = if dy == 1 {
                MEETING_FABRIC_LIT
            } else {
                MEETING_FABRIC
            };
            put(dx, dy, c);
        }
    }
    for dx in [1u16, chair.w - 2] {
        put(dx, 5, fc.chair_trim);
        put(dx, 6, fc.chair_trim);
    }
}

/// The meeting chairs upholster in the SAME fabric as the sofas they flank — and
/// the sofa is a SPRITE (un-themed), so the chair cannot read the value from
/// `Theme`. Deliberate second copies of the pack palette entries, pinned by
/// `meeting_chair_fabric_matches_the_sofa_sprite_palette`.
pub(super) const MEETING_FABRIC: Rgb = Rgb {
    r: 0x4f,
    g: 0x6d,
    b: 0x77,
};
pub(super) const MEETING_FABRIC_LIT: Rgb = Rgb {
    r: 0x6a,
    g: 0x8e,
    b: 0x98,
};

/// Meeting-room coat rack centred on `pos`, the pole top, filling
/// `coat_rack_rect_at(pos)`.
pub(super) fn paint_coat_rack(
    buf: &mut RgbBuffer,
    pos: crate::layout::Point,
    theme: &crate::theme::Theme,
) {
    let (cx, cy) = (pos.x, pos.y);
    let pole = theme.furniture.wood_trim;
    let base = theme.furniture.wood_top;
    let coats = theme.appliance.coats;
    for dy in 0..=COAT_RACK_BASE_DY {
        let py = cy + dy;
        if py < buf.height() && cx < buf.width() {
            buf.put(cx, py, pole);
        }
    }
    let by = cy + COAT_RACK_BASE_DY;
    for dx in 0..3u16 {
        let px = cx.saturating_sub(1) + dx;
        if px < buf.width() && by < buf.height() {
            buf.put(px, by, base);
        }
    }
    // Coat blobs on alternating hooks.
    for (i, &coat_color) in coats.iter().enumerate() {
        let hook_y = cy + 1 + (i as u16) * 2;
        let left = i % 2 == 0;
        let x0 = if left {
            cx.wrapping_sub(COAT_HOOK_DX + COAT_W - 1)
        } else {
            cx + COAT_HOOK_DX
        };
        for dy in 0..2u16 {
            for dx in 0..COAT_W {
                let px = x0.wrapping_add(dx);
                let py = hook_y + dy;
                if px < buf.width() && py < buf.height() {
                    buf.put(px, py, coat_color);
                }
            }
        }
    }
}
