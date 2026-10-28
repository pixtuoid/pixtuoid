//! The room's light as the display list carries it: how dark the room is
//! ([`Ambient`]), how far lightning lifts it ([`Flash`]), and each of its
//! lights resolved on the art grid ([`LightView`]). The rasterizer applies
//! them.

use pixtuoid_core::sprite::Rgb;

use crate::display::Span;
use crate::display::pen::{ArtPx, ArtRect, Pen, layout_point};
use crate::lighting::{Emitter, EmitterKind};
use crate::theme::Theme;

/// Ramp steps the room drops by at full dark.
pub(crate) const AMBIENT_MAX_STEPS: u8 = 4;

/// Ramp steps a light lifts per unit of its level: a desk lamp at full dark then
/// lifts the desk under its bulb most of the way back to its daylight tone, so
/// the night room reads as lamp-lit pools in the dark, not as day with stains.
const LIFT_STOPS_PER_LEVEL: f32 = 7.0;

/// The most a light lifts the daylit room: one step shows a lamp is on at noon,
/// and a second would bleach the floor under it.
const DAYLIGHT_LIFT: u8 = 1;

/// How dark the room is: the sky's darkness in whole steps, so a frame's tone
/// changes a handful of times a day rather than every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct Ambient(u8);

impl Ambient {
    /// The room at full dark.
    #[cfg(test)]
    pub(crate) const NIGHT: Self = Self(AMBIENT_MAX_STEPS);

    /// The room under `look`.
    pub(crate) fn of(look: &crate::atmosphere::SkyTones) -> Self {
        let steps = (look.darkness.clamp(0.0, 1.0) * f32::from(AMBIENT_MAX_STEPS)).round();
        Self(steps as u8)
    }

    /// The darkness it stands for, stepped as the room is, so what else the
    /// hour darkens (the shadows) moves with the room and not between its steps.
    pub(crate) fn darkness(self) -> f32 {
        f32::from(self.0) / f32::from(AMBIENT_MAX_STEPS)
    }

    /// `c` in a room this dark, unlit.
    pub(crate) fn on(self, c: Rgb) -> Rgb {
        c.ramp(-(self.0 as i8))
    }

    /// The ramp steps it darkens by.
    pub(crate) fn steps(self) -> u8 {
        self.0
    }

    /// The most a light may lift a pixel in a room this dark: back to its
    /// daylight tone and never past it, or [`DAYLIGHT_LIFT`] by day.
    pub(crate) fn ceiling(self) -> u8 {
        if self.0 == 0 { DAYLIGHT_LIFT } else { self.0 }
    }
}

/// How far a storm's lightning lifts the whole room this frame, in ramp steps,
/// at [`Sky::flash`](crate::sky::Sky::flash): a strike's phases step it, never
/// a blend toward white. Outside a storm it lifts nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct Flash(u8);

/// The steps a strike's brightest phase lifts the room.
pub(crate) const FLASH_MAX_STEPS: u8 = 2;
/// The steps it lifts the window glass, where the bolt is.
const BOLT_MAX_STEPS: u8 = 4;

impl Flash {
    /// The room's lift under `sky`.
    pub(crate) fn of(sky: &crate::sky::Sky) -> Self {
        Self(flash_steps(sky, FLASH_MAX_STEPS))
    }

    /// `c` lifted by it.
    pub(crate) fn on(self, c: Rgb) -> Rgb {
        if self.0 == 0 { c } else { c.ramp(self.0 as i8) }
    }

    /// The ramp steps it lifts by.
    pub(crate) fn steps(self) -> u8 {
        self.0
    }
}

/// The steps the bolt lifts the window glass under `sky`, before the room's
/// [`Flash`] lifts it with everything else: up to [`BOLT_MAX_STEPS`] +
/// [`FLASH_MAX_STEPS`] at a strike's peak.
pub(crate) fn bolt_steps(sky: &crate::sky::Sky) -> u8 {
    flash_steps(sky, BOLT_MAX_STEPS)
}

/// `sky`'s flash level in whole steps up to `max`, by
/// [`Sky::flash`](crate::sky::Sky::flash).
fn flash_steps(sky: &crate::sky::Sky, max: u8) -> u8 {
    (sky.flash().clamp(0.0, 1.0) * f32::from(max)).round() as u8
}

/// One light as the display list carries it, resolved when the list is built: how many
/// steps it lifts each art pixel of its box, row by row from the top-left, the
/// colour it tints them toward, and the room it lights, which bounds the lift.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct LightView {
    x: u16,
    y: u16,
    w: u16,
    lift: Vec<u8>,
    tint: Option<Rgb>,
    ambient: Ambient,
    /// Where two lights lift a pixel alike, the lower rank lights it: a total
    /// order on what the light is — its kind, where it stands and its tint —
    /// so the list's order cannot decide.
    rank: (u8, u16, u16, (u8, u8, u8)),
}

/// All [`LightView::of`] reads: two equal ones resolve equal views.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ViewInputs {
    pub(crate) emitter: Emitter,
    pub(crate) tint: Option<Rgb>,
    pub(crate) ambient: Ambient,
    pub(crate) pen: Pen,
    /// The office's `buf_w`×`buf_h`, which clips it.
    pub(crate) size: (u16, u16),
}

impl LightView {
    /// `emitter` on `pen`'s grid under `ambient`, clipped to the office's
    /// `size`, with its [`Span`]; `None` where it lifts no pixel a whole step,
    /// since a light with no solid band is only its seam's speckle.
    pub(crate) fn of(
        &ViewInputs {
            ref emitter,
            tint,
            ambient,
            pen,
            size: (buf_w, buf_h),
        }: &ViewInputs,
    ) -> Option<(Span, Self)> {
        let ((x0, y0), (x1, y1)) = emitter.bounds();
        let (x1, y1) = (x1.min(buf_w), y1.min(buf_h));
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let (ax0, ay0) = (pen.art(x0).0, pen.art(y0).0);
        let (ax1, ay1) = (pen.art(x1).0, pen.art(y1).0);
        let d = pen.density();
        let mut solid = false;
        let lift: Vec<u8> = (ay0..ay1)
            .flat_map(|ay| (ax0..ax1).map(move |ax| (ax, ay)))
            .map(|(ax, ay)| {
                let Some(level) =
                    emitter.level_at_f(layout_point(ArtPx(ax), d), layout_point(ArtPx(ay), d))
                else {
                    return 0;
                };
                let stops = level * LIFT_STOPS_PER_LEVEL;
                solid |= stops >= 1.0;
                // Floored, not rounded: the lift constants and the `solid` test
                // are tuned in whole steps.
                crate::dither::step(stops, ax, ay).min(ambient.ceiling())
            })
            .collect();
        solid.then(|| {
            (
                Span::new(x0, y0, x1 - x0, y1 - y0, 0),
                Self {
                    x: ax0,
                    y: ay0,
                    w: ax1 - ax0,
                    lift,
                    tint,
                    ambient,
                    rank: (
                        rank_of(emitter.kind),
                        x0,
                        y0,
                        tint.map_or((0, 0, 0), |t| (t.r, t.g, t.b)),
                    ),
                },
            )
        })
    }

    /// Its box on the art grid.
    pub(crate) fn rect(&self) -> ArtRect {
        let h = (self.lift.len() / usize::from(self.w.max(1))) as u16;
        ArtRect {
            x: ArtPx(self.x),
            y: ArtPx(self.y),
            w: ArtPx(self.w),
            h: ArtPx(h),
        }
    }

    /// Whether it is a `kind`'s light.
    #[cfg(test)]
    pub(crate) fn is(&self, kind: EmitterKind) -> bool {
        self.rank.0 == rank_of(kind)
    }

    /// The colour it tints toward.
    pub(crate) fn tint(&self) -> Option<Rgb> {
        self.tint
    }

    /// The order it lights a tie in: the lower rank wins.
    pub(crate) fn rank(&self) -> (u8, u16, u16, (u8, u8, u8)) {
        self.rank
    }

    /// Where it lifts most, in art pixels: the middle of its brightest.
    #[cfg(test)]
    pub(crate) fn peak(&self) -> (f32, f32) {
        let top = self.lift.iter().copied().max().unwrap_or(0);
        let w = usize::from(self.w.max(1));
        let (mut n, mut sx, mut sy) = (0.0, 0.0, 0.0);
        for (i, _) in self.lift.iter().enumerate().filter(|&(_, &l)| l == top) {
            n += 1.0;
            sx += (i % w) as f32;
            sy += (i / w) as f32;
        }
        (f32::from(self.x) + sx / n, f32::from(self.y) + sy / n)
    }

    /// Its lift at art pixel `(x, y)`, 0 outside its box.
    pub(crate) fn lift_at(&self, x: u16, y: u16) -> u8 {
        let (dx, dy) = (x.wrapping_sub(self.x), y.wrapping_sub(self.y));
        if dx >= self.w {
            return 0;
        }
        self.lift
            .get(usize::from(dy) * usize::from(self.w) + usize::from(dx))
            .copied()
            .unwrap_or(0)
    }
}

/// An emitter kind's place in the light order ([`LightView::rank`]).
fn rank_of(kind: EmitterKind) -> u8 {
    match kind {
        EmitterKind::WindowSpill => 0,
        EmitterKind::FloorLamp => 1,
        EmitterKind::DeskLamp => 2,
        EmitterKind::MonitorHalo(_) => 3,
        EmitterKind::NeonGlow => 4,
    }
}

/// The colour a light of `kind` tints toward, or `None` for one that tints
/// nothing.
pub(crate) fn tint_of(
    kind: EmitterKind,
    theme: &Theme,
    neon: crate::floor::NeonLevels,
) -> Option<Rgb> {
    let lighting = &theme.lighting;
    match kind {
        EmitterKind::FloorLamp => Some(lighting.floor_lamp_halo),
        EmitterKind::DeskLamp => Some(lighting.desk_lamp),
        EmitterKind::WindowSpill => Some(lighting.sun_spill),
        EmitterKind::NeonGlow => Some(crate::floor::neon_look(neon, theme).halo),
        // Dark themes only, as in the classic's
        // `pixel_painter::ambient::paint_ceiling_halos`: on a light one it reads as grime.
        EmitterKind::MonitorHalo(work) => {
            (theme.kind == crate::theme::ThemeKind::Dark).then(|| theme.tool_glow.for_work(work))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::display::pen::test_density;
    use crate::layout::Point;
    use crate::lighting::Light;
    use crate::render_scale::RenderScale;

    pub(crate) fn lamp(strength: f32, at: Point) -> Emitter {
        Emitter {
            kind: EmitterKind::DeskLamp,
            light: Light::Halo {
                centre: at,
                radius: 5,
                share: 1.0,
            },
            strength,
        }
    }

    pub(crate) fn pen() -> Pen {
        Pen::new(RenderScale::new(4).expect("nonzero"), test_density(4)).expect("4 divides 4")
    }

    fn inputs(emitter: Emitter, tint: Option<Rgb>, ambient: Ambient) -> ViewInputs {
        ViewInputs {
            emitter,
            tint,
            ambient,
            pen: pen(),
            size: (40, 16),
        }
    }

    pub(crate) fn view(e: &Emitter, tint: Option<Rgb>, ambient: Ambient) -> LightView {
        LightView::of(&inputs(*e, tint, ambient))
            .expect("it lifts")
            .1
    }

    /// A light takes the night room back to its daylight tone at most, and the
    /// daylit room [`DAYLIGHT_LIFT`] past it.
    #[test]
    fn a_light_lifts_the_night_room_to_daylight_and_the_day_room_a_step() {
        let night = Ambient::NIGHT;
        let bright = lamp(1.0, Point { x: 10, y: 8 });
        let at_night = view(&bright, None, night);
        assert_eq!(at_night.lift.iter().max(), Some(&night.0));
        let by_day = view(&bright, None, Ambient(0));
        assert_eq!(by_day.lift.iter().max(), Some(&DAYLIGHT_LIFT));
    }

    /// A light too faint for a solid band is left out: all it would show is its
    /// seam's speckle.
    #[test]
    fn a_light_with_no_solid_band_is_left_out() {
        let faint = lamp(0.1, Point { x: 10, y: 8 });
        assert!(LightView::of(&inputs(faint, None, Ambient(0))).is_none());
    }
}
