//! Paints the room's lights ([`crate::lighting`]) and the shadows, the corridor
//! runner's texture, the neon sign's panel, and the wall clock.

use std::time::SystemTime;

use pixtuoid_core::sprite::{Rgb, RgbBuffer};

use crate::dither::FALLOFF_TONES;
use crate::floor::NeonLook;
use crate::layout::roster::RUNNER_LATTICE_STRIDE;
use crate::lighting::Emitter;
use crate::pixel_painter::palette::blend_rgb;
use crate::sky::clock_reading;
use crate::sky::octant_offset;
use crate::theme::Theme;

/// The composite every light shares: [`blend_tone`] over the caller-clipped
/// `xs` × `ys` at each pixel's `t(x, y)`; `None` leaves the pixel alone. A
/// light owns its falloff SHAPE and its clip, never the blend.
fn blend_falloff(
    buf: &mut RgbBuffer,
    xs: std::ops::Range<u16>,
    ys: std::ops::Range<u16>,
    color: Rgb,
    peak: f32,
    t: impl Fn(u16, u16) -> Option<f32>,
) {
    for y in ys {
        for x in xs.clone() {
            if let Some(t) = t(x, y) {
                blend_tone(buf, x, y, color, t, peak);
            }
        }
    }
}

/// Blend `color` over the pixel at `(x, y)` by `level`, stepped to one of
/// [`FALLOFF_TONES`] tones of `peak`.
fn blend_tone(buf: &mut RgbBuffer, x: u16, y: u16, color: Rgb, level: f32, peak: f32) {
    let tone = crate::dither::stepped(level, peak, FALLOFF_TONES, x, y);
    if tone > 0.0 {
        let cur = buf.get(x, y);
        buf.put(x, y, blend_rgb(cur, color, tone));
    }
}

/// The floor's shadows ([`Depths`](crate::ground::Depths) at one cell a
/// pixel) in `color`, `strength` deep at a shadow's centre.
pub(in crate::pixel_painter) fn paint_shadows(
    buf: &mut RgbBuffer,
    cells: &[(u16, u16, f32)],
    strength: f32,
    color: Rgb,
) {
    for &(x, y, d) in cells {
        if x < buf.width() && y < buf.height() {
            blend_tone(buf, x, y, color, d * strength, strength);
        }
    }
}

/// Blend `emitter`'s light in `color` over what is already painted, at the
/// level the model gives each cell, the window glass included.
pub(in crate::pixel_painter) fn paint_light(buf: &mut RgbBuffer, emitter: &Emitter, color: Rgb) {
    if emitter.strength <= 0.0 {
        return;
    }
    let ((x0, y0), (x1, y1)) = emitter.bounds();
    let (xs, ys) = (x0..x1.min(buf.width()), y0..y1.min(buf.height()));
    blend_falloff(buf, xs, ys, color, emitter.peak(), |x, y| {
        emitter.level_at(x, y)
    });
}

/// Neon sign panel — a flat dark interior inside a lit tube, painted in the wall
/// band. The text overlay is each painter's own pass on top.
pub(in crate::pixel_painter) fn paint_neon_panel(
    buf: &mut RgbBuffer,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    look: &NeonLook,
) {
    // The SAME const the board-text interior derives from (`NEON_PANEL_INNER_*`),
    // so the cells left dark == the cells the text may fill; they can't drift.
    let b = crate::layout::NEON_PANEL_BORDER;
    for dy in 0..h {
        for dx in 0..w {
            let on_border = dx < b || dx >= w - b || dy < b || dy >= h - b;
            let color = if on_border { look.tube } else { look.interior };
            buf.put_checked(x + dx, y + dy, color);
        }
    }
}

/// Live wall clock — a 7x7 face whose hands quantize to 8 directions and are
/// drawn as multi-pixel rays so they read clearly at this size.
pub(in crate::pixel_painter) fn paint_clock(
    buf: &mut RgbBuffer,
    x: u16,
    y: u16,
    now: SystemTime,
    theme: &Theme,
) {
    let rim = theme.office.clock_rim;
    let face = theme.office.clock_face;
    let hand_color = theme.office.clock_hand;
    let hand_min = hand_color;

    // The disc — `R` rim, `F` face, `.` transparent — typed to the clock's
    // layout size, which places it.
    const W: usize = crate::layout::CLOCK.w as usize;
    const H: usize = crate::layout::CLOCK.h as usize;
    let rows: [&[u8; W]; H] = [
        b"..RRR..", b".RFFFR.", b"RFFFFFR", b"RFFFFFR", b"RFFFFFR", b".RFFFR.", b"..RRR..",
    ];
    for (dy, row) in rows.iter().enumerate() {
        for (dx, ch) in row.iter().enumerate() {
            let c = match ch {
                b'R' => rim,
                b'F' => face,
                _ => continue,
            };
            let px = x + dx as u16;
            let py = y + dy as u16;
            if px < buf.width() && py < buf.height() {
                buf.put(px, py, c);
            }
        }
    }

    let (hour_turns, min_turns) = clock_reading(now).turns();

    let put = |buf: &mut RgbBuffer, ox: i32, oy: i32, color: Rgb| {
        let px = i32::from(x) + 3 + ox;
        let py = i32::from(y) + 3 + oy;
        if px >= 0 && py >= 0 && (px as u16) < buf.width() && (py as u16) < buf.height() {
            buf.put(px as u16, py as u16, color);
        }
    };

    put(buf, 0, 0, hand_color);

    let (hdx, hdy) = octant_offset(hour_turns);
    put(buf, hdx, hdy, hand_color);

    // The 7x7 disc has 3-px face at cardinals but only 1-px face at diagonals, so
    // a length-2 diagonal minute hand would overwrite the rim and gap the border.
    let (mdx, mdy) = octant_offset(min_turns);
    let max_step = if mdx != 0 && mdy != 0 { 1 } else { 2 };
    for step in 1..=max_step {
        put(buf, mdx * step, mdy * step, hand_min);
    }
}

/// Office corridor runner, painted along the cubicle_aisle band so the eye
/// traces a path connecting the door, meeting room, pantry, cubicles and lounge.
/// Just texture over the floor — walls and decor paint on top.
pub(in crate::pixel_painter) fn paint_corridor_runner(
    buf: &mut RgbBuffer,
    rect: crate::layout::Bounds,
    theme: &Theme,
) {
    let runner_base = theme.office.runner_base;
    let runner_stripe = theme.office.runner_stripe;
    let runner_edge = theme.office.runner_edge;
    let max_x = (rect.x + rect.width).min(buf.width());
    let max_y = (rect.y + rect.height).min(buf.height());
    for y in rect.y..max_y {
        for x in rect.x..max_x {
            let is_edge = y == rect.y || y + 1 == max_y;
            let dy = i32::from(y - rect.y);
            let dx = i32::from(x - rect.x);
            let diamond = ((dx + dy) % RUNNER_LATTICE_STRIDE == 0)
                || ((dx - dy).rem_euclid(RUNNER_LATTICE_STRIDE) == 0);
            let color = if is_edge {
                runner_edge
            } else if diamond {
                runner_stripe
            } else {
                runner_base
            };
            buf.put(x, y, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Beat;

    use crate::floor::{NeonLevels, neon_look};
    use crate::layout::{NEON_PANEL_BORDER, NEON_PANEL_H, NEON_PANEL_W, Point};
    use crate::lighting::{EmitterKind, Light, NEON_HALO_RADIUS};

    fn look(levels: NeonLevels) -> NeonLook {
        neon_look(levels, &crate::theme::NORMAL)
    }

    /// A neon halo around the `w`×`h` panel at `(x, y)`, thrown at `strength`.
    fn glow(x: u16, y: u16, w: u16, h: u16, strength: f32) -> Emitter {
        Emitter {
            kind: EmitterKind::NeonGlow,
            light: Light::Glow {
                at: Point { x, y },
                w,
                h,
                reach: NEON_HALO_RADIUS,
            },
            strength,
        }
    }

    const WALL: Rgb = Rgb {
        r: 90,
        g: 90,
        b: 90,
    };
    /// Room for the panel plus its whole halo on every side.
    const PANEL_AT: u16 = 12;

    /// A lit sign's halo strength at the tube.
    const LIT: f32 = 0.5;

    fn lit_wall(levels: NeonLevels) -> (RgbBuffer, NeonLook) {
        let side = PANEL_AT * 2 + NEON_PANEL_W;
        let mut buf = RgbBuffer::filled(side, side, WALL);
        let look = look(levels);
        paint_neon_panel(
            &mut buf,
            PANEL_AT,
            PANEL_AT,
            NEON_PANEL_W,
            NEON_PANEL_H,
            &look,
        );
        paint_light(
            &mut buf,
            &glow(PANEL_AT, PANEL_AT, NEON_PANEL_W, NEON_PANEL_H, LIT),
            look.halo,
        );
        (buf, look)
    }

    #[test]
    fn neon_hue_is_the_brand_until_someone_waits() {
        let theme = &crate::theme::NORMAL;
        assert_eq!(look(NeonLevels::RUNNING).halo, theme.ui.neon_brand);
        assert_eq!(look(NeonLevels::CALM).halo, theme.ui.neon_brand);
        assert_eq!(look(NeonLevels::ALERT).halo, theme.ui.neon_alert);
    }

    /// A terminal text cell shows only its BOTTOM pixel (ratatui keeps the old bg
    /// under a glyph), while an uncovered cell shows both — so the interior must
    /// be one flat color and the halo must stay out of it, or the rows band.
    #[test]
    fn the_interior_is_one_flat_color_the_halo_never_enters() {
        let (buf, look) = lit_wall(NeonLevels::ALERT);
        let b = NEON_PANEL_BORDER;
        for dy in 0..NEON_PANEL_H {
            for dx in 0..NEON_PANEL_W {
                let edge = dx < b || dx >= NEON_PANEL_W - b || dy < b || dy >= NEON_PANEL_H - b;
                let want = if edge { look.tube } else { look.interior };
                assert_eq!(buf.get(PANEL_AT + dx, PANEL_AT + dy), want, "({dx},{dy})");
            }
        }
    }

    #[test]
    fn the_halo_lights_the_wall_beside_the_tube_and_stops_at_its_radius() {
        let (buf, _) = lit_wall(NeonLevels::ALERT);
        let mid_y = PANEL_AT + NEON_PANEL_H / 2;
        let lit = |x: u16| buf.get(x, mid_y) != WALL;
        assert!(lit(PANEL_AT - 1), "the wall next to the tube is lit");
        assert!(!lit(PANEL_AT - NEON_HALO_RADIUS), "nothing AT the radius");
        // A 4x4 tile's worth of the reach, beside the tube and near its edge:
        // the dither lights fewer of the far cells.
        let lit_in = |x0: u16| {
            (0..4u16)
                .flat_map(|dx| (0..4u16).map(move |dy| (x0 - dx, mid_y - 2 + dy)))
                .filter(|&(x, y)| buf.get(x, y) != WALL)
                .count()
        };
        assert!(
            lit_in(PANEL_AT - 1) > lit_in(PANEL_AT - 3),
            "and the light thins out with distance"
        );
    }

    /// A falloff is a few flat tones, dithered, never a soft blend with a tone
    /// per pixel.
    #[test]
    fn a_halo_paints_no_more_tones_than_its_ramp_has() {
        let (buf, look) = lit_wall(NeonLevels::ALERT);
        let tones: std::collections::HashSet<Rgb> = buf
            .as_slice()
            .iter()
            .copied()
            .filter(|&c| c != WALL && c != look.tube && c != look.interior)
            .collect();
        // A literal, not FALLOFF_TONES: the rule is "a few", whatever the
        // constant grows to.
        assert!(
            (1..=4).contains(&tones.len()),
            "{} tones: {tones:?}",
            tones.len()
        );
    }

    #[test]
    fn an_unpowered_halo_leaves_the_wall_alone() {
        let mut buf = RgbBuffer::filled(60, 40, WALL);
        paint_light(
            &mut buf,
            &glow(PANEL_AT, PANEL_AT, NEON_PANEL_W, NEON_PANEL_H, 0.0),
            look(NeonLevels::EMPTY).halo,
        );
        assert!((0..40).all(|y| (0..60).all(|x| buf.get(x, y) == WALL)));
    }

    /// A shadow darkens the floor under its solid at strength, and paints
    /// nothing at none.
    #[test]
    fn a_shadow_paints_only_at_strength() {
        let shadow = crate::theme::NORMAL.office.shadow;
        let fill = Rgb {
            r: 30,
            g: 30,
            b: 30,
        };
        let cells: Vec<_> = crate::ground::Depths::of(
            std::iter::once(crate::ground::Contact::under(5, 10, 10)),
            pixtuoid_core::sprite::format::Density::ONE,
        )
        .expect("one contact")
        .cells()
        .collect();
        let mut buf = RgbBuffer::filled(20, 20, fill);
        paint_shadows(&mut buf, &cells, 0.0, shadow);
        assert!(
            buf.as_slice().iter().all(|&p| p == fill),
            "none at no strength"
        );
        paint_shadows(&mut buf, &cells, 0.9, shadow);
        assert_ne!(buf.get(10, 10), fill, "under the solid's middle");
    }

    /// A painter that lights a cell outside an emitter's bounds breaks every
    /// cache that repaints only what those bounds say changed.
    #[test]
    fn every_light_paints_only_inside_its_bounds() {
        let layout =
            crate::layout::SceneLayout::compute(192, 80, Some(crate::layout::TEST_DEFAULT_DESKS))
                .expect("fits");
        // 07:00 lights the lamps AND leans the sun through the windows.
        let sky =
            crate::sky::Sky::at_with(crate::localclock::at_hour(7), crate::sky::Weather::Clear);
        let lights = crate::lighting::Lights::of(
            &layout,
            &crate::atmosphere::SkyTones::resolve(&sky, &crate::theme::NORMAL),
            &crate::lighting::LightInputs {
                agents: &[],
                seated: &std::collections::HashMap::new(),
                floor_idx: 0,
                indoor_scale: 1.0,
                neon: NeonLevels::FLASH,
                beat: Beat::at_ms(0),
                bulbs: crate::lighting::DeskBulbs::of(&crate::pack::test_office()),
            },
        );
        let patch = Emitter {
            kind: EmitterKind::MonitorHalo(pixtuoid_core::state::ToolKind::Edit),
            light: Light::Patch {
                centre: Point { x: 60, y: 20 },
            },
            strength: 1.0,
        };
        let emitters: Vec<Emitter> = lights
            .floor_lamp
            .iter()
            .chain(lights.desks.iter().map(|d| &d.lamp))
            .chain(std::iter::once(&lights.neon))
            .chain(&lights.spills)
            .chain(std::iter::once(&patch))
            .copied()
            .collect();
        assert!(!lights.spills.is_empty() && lights.floor_lamp.is_some());
        let white = Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        for e in emitters {
            let e = Emitter { strength: 1.0, ..e };
            let mut buf = RgbBuffer::filled(layout.buf_w, layout.buf_h, WALL);
            paint_light(&mut buf, &e, white);
            let ((x0, y0), (x1, y1)) = e.bounds();
            let mut lit = 0;
            for y in 0..buf.height() {
                for x in 0..buf.width() {
                    if buf.get(x, y) != WALL {
                        lit += 1;
                        assert!(
                            (x0..x1).contains(&x) && (y0..y1).contains(&y),
                            "{e:?} lit ({x},{y}) outside {:?}",
                            e.bounds()
                        );
                    }
                }
            }
            assert!(lit > 0, "{e:?} lit nothing");
        }
    }

    // Off-edge must clip, not panic: the panel through `put_checked`, the glow
    // through its clamped ranges.
    #[test]
    fn neon_panel_off_edge_does_not_panic() {
        let mut buf = RgbBuffer::filled(10, 10, Rgb { r: 0, g: 0, b: 0 });
        // x=8, w=6 → px reaches 13 (>= width 10); y=8, h=5 → py reaches 12.
        let look = look(NeonLevels::ALERT);
        paint_neon_panel(&mut buf, 8, 8, 6, 5, &look);
        paint_light(&mut buf, &glow(8, 8, 6, 5, LIT), look.halo);
        assert_ne!(
            buf.get(8, 8),
            Rgb { r: 0, g: 0, b: 0 },
            "in-bounds frame paints"
        );
    }
}
