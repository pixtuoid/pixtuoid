//! The room's own lights, pixel-free: where each emitter stands, what shape it
//! lights, and how strongly this frame. Each painter picks the colours from its
//! theme and paints the shapes its own way; neither decides where a light falls
//! or how bright it is.

use std::collections::HashMap;

use pixtuoid_core::state::{ActivityState, FloorLocalDeskIndex};
use pixtuoid_core::{AgentSlot, ToolKind};

use crate::atmosphere::SkyTones;
use crate::floor::NeonLevels;
use crate::layout::{Facing, HomeDesk, Point, SceneLayout};

/// The floor lamp's level at full dark, before the room's own level.
const FLOOR_LAMP_GAIN: f32 = 0.55;
/// A room-corner fixture, so much wider than a desk lamp's pool.
const FLOOR_LAMP_RADIUS: u16 = 11;

/// Its diameter stays under the desk's width: a larger pool washes the
/// neighbouring workstations.
const DESK_LAMP_RADIUS: u16 = 5;
const _: () = assert!(2 * DESK_LAMP_RADIUS < crate::layout::desk_furniture_def().visual.w);
/// The desk lamp's pool at full dark, as a share of the lamp's own level.
const DESK_LAMP_MAX: f32 = 0.42;
/// The standby screen's ceiling. At parity with [`DESK_LAMP_MAX`] the lamp
/// pool washes out the back-turned desk's east half, its lamp's side.
pub(crate) const SCREEN_IDLE_MAX: f32 = 0.55;
/// Where each facing's desk lamp bulb hangs from its desk's point, as the 1x
/// art the pack draws there marks it ([`crate::pack::OfficeArt::bulb_offset`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeskBulbs {
    facing_viewer: (u16, i16),
    back_turned: (u16, i16),
}

impl DeskBulbs {
    /// `pack`'s.
    pub(crate) fn of(pack: &crate::pack::OfficeArt) -> Self {
        use crate::pack::Desk;
        Self {
            facing_viewer: pack.bulb_offset(Desk::South),
            back_turned: pack.bulb_offset(Desk::North),
        }
    }

    /// A desk facing `facing`'s.
    pub(crate) fn at(self, facing: Facing) -> (u16, i16) {
        match facing {
            Facing::North => self.back_turned,
            _ => self.facing_viewer,
        }
    }
}
const _: () = assert!(DESK_LAMP_MAX < SCREEN_IDLE_MAX);

/// A monitor halo's level.
const MONITOR_HALO_INTENSITY: f32 = 0.8;
/// The share of it the halo's centre gets.
const MONITOR_HALO_SHARE: f32 = 0.4;
/// The halo's column offset from the desk: the middle of the screen's glass.
const MONITOR_HALO_DX: u16 = u16::midpoint(
    *crate::layout::SCREEN_GLASS_COLS.start(),
    *crate::layout::SCREEN_GLASS_COLS.end(),
);
/// The halo's footprint.
const MONITOR_HALO_W: u16 = 5;
const MONITOR_HALO_H: u16 = 2;
/// The Manhattan distance from the halo's centre to its far corners, where it
/// has faded out.
const MONITOR_HALO_REACH: f32 = (MONITOR_HALO_W / 2 + MONITOR_HALO_H - 1) as f32;

/// How far the neon halo reaches past its panel.
pub(crate) const NEON_HALO_RADIUS: u16 = 7;
/// Halo strength at the tube, full power, for the brand and the alert hue.
const NEON_HALO_BRAND: f32 = 0.44;
const NEON_HALO_ALERT: f32 = 0.58;
/// The slow brand breath: period and trough. The alert breath is faster and
/// deeper — urgency without a strobe.
const NEON_BREATH_MS: u64 = 6_000;
const NEON_BREATH_FLOOR: f32 = 0.85;
const NEON_ALERT_BREATH_MS: u64 = 3_000;
const NEON_ALERT_BREATH_FLOOR: f32 = 0.62;
/// Daylight washes a neon out: the halo keeps this share at noon, all of it at night.
const NEON_DAYLIGHT_MIN: f32 = 0.5;

/// How far below its window the sun's spill reaches, in rows.
pub(crate) const SPILL_DEPTH: u16 = 12;
/// The spill's strength at the sill, as a share of the sunlight.
const SPILL_SILL: f32 = 0.32;
/// The most the spill widens past the window on either side.
const SPILL_MAX_WIDEN: u16 = 3;

/// What an emitter is, for the painter choosing its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmitterKind {
    FloorLamp,
    DeskLamp,
    /// Over a monitor lit by a call of this tool, and tinted by it.
    MonitorHalo(ToolKind),
    NeonGlow,
    WindowSpill,
}

/// The shape an emitter lights and how its light falls off across it, in
/// layout units. Each shape gives its brightest cell a fixed share of the
/// emitter's level: all of it, unless the shape says otherwise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Light {
    /// A disc falling off linearly from `centre` to `radius`; its centre gets
    /// `share` of the level.
    Halo {
        centre: Point,
        radius: u16,
        share: f32,
    },
    /// Around a `w`×`h` panel at `at`: the square of how much of `reach` is left
    /// past its edge, and nothing inside the panel.
    Glow {
        at: Point,
        w: u16,
        h: u16,
        reach: u16,
    },
    /// Below a `w`-wide window at `x`, from row `top` down [`SPILL_DEPTH`]
    /// rows: fading linearly, widening by a column every two rows, and leaning
    /// `slant` columns per row.
    Spill {
        x: u16,
        w: u16,
        top: u16,
        slant: f32,
    },
    /// A [`MONITOR_HALO_W`]-wide, [`MONITOR_HALO_H`]-tall patch whose bottom
    /// row holds `centre`, falling off by Manhattan distance from it; the
    /// centre gets [`MONITOR_HALO_SHARE`] of the level.
    Patch { centre: Point },
}

/// One light in the room this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Emitter {
    pub(crate) kind: EmitterKind,
    pub(crate) light: Light,
    /// How lit it is; its [`Light`] sets what share of this its brightest cell
    /// gets.
    pub(crate) strength: f32,
}

impl Emitter {
    /// The first column and row it can light, and the ones just past the last:
    /// [`Self::level_at`] is `None` everywhere else.
    pub(crate) fn bounds(&self) -> ((u16, u16), (u16, u16)) {
        match self.light {
            Light::Halo { centre, radius, .. } => (
                (
                    centre.x.saturating_sub(radius),
                    centre.y.saturating_sub(radius),
                ),
                (centre.x + radius, centre.y + radius),
            ),
            Light::Glow { at, w, h, reach } => (
                (at.x.saturating_sub(reach), at.y.saturating_sub(reach)),
                (at.x + w + reach, at.y + h + reach),
            ),
            Light::Spill { x, w, top, slant } => {
                let (mut min_x, mut max_x) = (i32::MAX, i32::MIN);
                for row in spill_rows(x, w, slant) {
                    min_x = min_x.min(row.start);
                    max_x = max_x.max(row.end);
                }
                let clip = |x: i32| x.clamp(0, i32::from(u16::MAX)) as u16;
                ((clip(min_x), top), (clip(max_x), top + SPILL_DEPTH))
            }
            Light::Patch { centre } => {
                let x0 = centre.x.saturating_sub(MONITOR_HALO_W / 2);
                (
                    (x0, centre.y.saturating_sub(MONITOR_HALO_H - 1)),
                    (x0 + MONITOR_HALO_W, centre.y + 1),
                )
            }
        }
    }

    /// The strength its brightest cell gets: the top of the ramp a painter
    /// steps [`Self::level_at`] down.
    pub(crate) fn peak(&self) -> f32 {
        match self.light {
            Light::Halo { share, .. } => self.strength * share,
            // Nothing lights inside the panel: its brightest cells stand one
            // out from its edge.
            Light::Glow { reach, .. } => {
                let near = 1.0 - 1.0 / f32::from(reach);
                self.strength * near * near
            }
            Light::Patch { .. } => self.strength * MONITOR_HALO_SHARE,
            Light::Spill { .. } => self.strength,
        }
    }

    /// The blend strength it lights cell `(x, y)` with, or `None` where it
    /// doesn't reach.
    pub(crate) fn level_at(&self, x: u16, y: u16) -> Option<f32> {
        self.level_at_f(f32::from(x), f32::from(y))
    }

    /// [`Self::level_at`] at any point, in layout units: a painter whose grid is
    /// finer than a layout cell samples each of its pixels where it lies, so a
    /// light's falloff steps with the art rather than in blocks a cell wide. A
    /// spill's rows and a patch's footprint stay whole cells ([`unit_holding`]), the
    /// shapes they are.
    pub(crate) fn level_at_f(&self, x: f32, y: f32) -> Option<f32> {
        let strength = self.strength;
        match self.light {
            Light::Halo {
                centre,
                radius,
                share,
            } => {
                let peak = strength * share;
                let r2max = f32::from(radius) * f32::from(radius);
                let dx = x - f32::from(centre.x);
                let dy = y - f32::from(centre.y);
                let r2 = dx * dx + dy * dy;
                (r2 <= r2max).then(|| (1.0 - (r2 / r2max).sqrt()) * peak)
            }
            Light::Glow { at, w, h, reach } => {
                let (left, right) = (f32::from(at.x), f32::from(at.x + w - 1));
                let (top, bottom) = (f32::from(at.y), f32::from(at.y + h - 1));
                let dx = (left - x).max(x - right).max(0.0);
                let dy = (top - y).max(y - bottom).max(0.0);
                let left_of_reach = 1.0 - (dx * dx + dy * dy).sqrt() / f32::from(reach);
                let outside = dx > 0.0 || dy > 0.0;
                (outside && left_of_reach > 0.0).then_some(strength * left_of_reach * left_of_reach)
            }
            Light::Spill {
                x: wx,
                w,
                top,
                slant,
            } => {
                let below = unit_holding(y) - i32::from(top);
                let dy = u16::try_from(below).ok().filter(|&dy| dy < SPILL_DEPTH)?;
                let row = spill_rows(wx, w, slant).nth(usize::from(dy))?;
                row.contains(&unit_holding(x))
                    .then(|| strength * (1.0 - f32::from(dy) / f32::from(SPILL_DEPTH)))
            }
            Light::Patch { centre } => {
                let ((x0, y0), (x1, y1)) = self.bounds();
                if !(i32::from(x0)..i32::from(x1)).contains(&unit_holding(x))
                    || !(i32::from(y0)..i32::from(y1)).contains(&unit_holding(y))
                {
                    return None;
                }
                let dx = x - f32::from(x0);
                // Under the centre's row but within its cell: as lit as the row.
                let dy = (f32::from(centre.y) - y).max(0.0);
                let dist = ((dx - f32::from(MONITOR_HALO_W / 2)).abs() + dy) / MONITOR_HALO_REACH;
                Some((strength * (1.0 - dist).max(0.0) * MONITOR_HALO_SHARE).clamp(0.0, 1.0))
            }
        }
    }
}

/// The layout unit a point of [`Emitter::level_at_f`] falls in: a unit spans
/// `[u - 0.5, u + 0.5)` around its integer, as
/// [`layout_point`](crate::display::pen::layout_point) places art pixels.
fn unit_holding(v: f32) -> i32 {
    (v + 0.5).floor() as i32
}

/// The columns each row of a spill spans, top row first, unclipped: the
/// window's own columns leaning `slant` per row and widening by one every two
/// rows up to [`SPILL_MAX_WIDEN`].
fn spill_rows(x: u16, w: u16, slant: f32) -> impl Iterator<Item = std::ops::Range<i32>> {
    (0..SPILL_DEPTH).map(move |dy| {
        let widen = i32::from((dy / 2).min(SPILL_MAX_WIDEN));
        let shift = (slant * f32::from(dy)).round() as i32;
        let base_x = i32::from(x) + shift;
        base_x - widen..base_x + i32::from(w) + widen
    })
}

/// One home desk's lights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DeskLights {
    /// The lamp's light, which it throws whichever way the desk faces.
    pub(crate) lamp: Emitter,
    /// How strongly its standby screen glows; zero where the desk shows the
    /// viewer the monitor's back.
    pub(crate) screen_idle: f32,
}

/// Every light in the room this frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Lights {
    pub(crate) floor_lamp: Option<Emitter>,
    /// Index-parallel to [`home_desks`](crate::layout::SceneLayout::home_desks).
    pub(crate) desks: Vec<DeskLights>,
    /// Over each [`lit_screen`].
    pub(crate) monitor_halos: Vec<Emitter>,
    /// The neon sign's halo on the wall around it.
    pub(crate) neon: Emitter,
    /// The sun through each window, left to right; none while it is down.
    pub(crate) spills: Vec<Emitter>,
}

/// What the lights depend on this frame besides the layout and the sky's [`SkyTones`].
pub(crate) struct LightInputs<'a> {
    /// Every agent the scene holds; only this floor's light it.
    pub(crate) agents: &'a [AgentSlot],
    /// Whether each home desk's occupant sits there now.
    pub(crate) seated: &'a HashMap<FloorLocalDeskIndex, bool>,
    pub(crate) floor_idx: usize,
    /// The room's artificial-light level, which an emptied floor turns down.
    pub(crate) indoor_scale: f32,
    pub(crate) neon: NeonLevels,
    /// The neon halo's breath steps on it.
    pub(crate) beat: crate::anim::Beat,
    /// Where each desk's lamp hangs its bulb.
    pub(crate) bulbs: DeskBulbs,
}

impl Lights {
    /// The lights `layout` shows under `look`.
    pub(crate) fn of(layout: &SceneLayout, look: &SkyTones, inputs: &LightInputs<'_>) -> Self {
        let (darkness, indoor) = (look.darkness, inputs.indoor_scale);
        Self {
            floor_lamp: layout.floor_lamp_base().map(|centre| Emitter {
                kind: EmitterKind::FloorLamp,
                light: Light::Halo {
                    centre,
                    radius: FLOOR_LAMP_RADIUS,
                    share: 1.0,
                },
                strength: darkness * FLOOR_LAMP_GAIN * indoor,
            }),
            desks: layout
                .home_desks
                .iter()
                .map(|&HomeDesk { at, facing }| {
                    desk_lights(at, facing, inputs.bulbs.at(facing), darkness, indoor)
                })
                .collect(),
            monitor_halos: monitor_halos(layout, inputs),
            neon: Emitter {
                kind: EmitterKind::NeonGlow,
                light: Light::Glow {
                    at: Point {
                        x: crate::layout::NEON_PANEL.x,
                        y: crate::layout::NEON_PANEL.y,
                    },
                    w: crate::layout::NEON_PANEL.width,
                    h: crate::layout::NEON_PANEL.height,
                    reach: NEON_HALO_RADIUS,
                },
                strength: neon_halo_strength(inputs.neon, inputs.beat, darkness),
            },
            // `sunlight` already carries the weather, so heavy cloud dims the
            // spill with it.
            spills: if look.sunlight > 0.0 {
                let top = layout.wall_band_h();
                layout
                    .window_bays()
                    .map(|bay| Emitter {
                        kind: EmitterKind::WindowSpill,
                        light: Light::Spill {
                            x: bay.x,
                            w: bay.w,
                            top,
                            slant: look.spill_slant,
                        },
                        strength: SPILL_SILL * look.sunlight,
                    })
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}

/// The lights of a desk facing `facing` that hangs its lamp's `bulb`, scaled
/// by `darkness` — `1 − exterior`, so weather counts and not just the hour —
/// and by `indoor`, which is what an emptied floor switches off.
fn desk_lights(
    desk: Point,
    facing: Facing,
    bulb: (u16, i16),
    darkness: f32,
    indoor: f32,
) -> DeskLights {
    DeskLights::new(
        desk,
        bulb,
        darkness * indoor,
        screen_idle(facing, darkness, indoor),
    )
}

/// How strongly a desk's idle screen glows on standby, up to
/// [`SCREEN_IDLE_MAX`]: the dark and the room's own level both wake it, and
/// only a desk that shows the viewer its screen shows it.
pub(crate) fn screen_idle(facing: Facing, darkness: f32, indoor: f32) -> f32 {
    if facing == Facing::North {
        SCREEN_IDLE_MAX * darkness * indoor
    } else {
        0.0
    }
}

impl DeskLights {
    /// The lights of a desk at `desk` whose lamp hangs its bulb `bulb` from
    /// it: its lamp lit to `level`, its pool at most [`DESK_LAMP_MAX`], and
    /// its standby screen at `screen_idle`.
    pub(crate) fn new(desk: Point, bulb: (u16, i16), level: f32, screen_idle: f32) -> Self {
        let (dx, dy) = bulb;
        let bulb = Point {
            x: desk.x + dx,
            y: desk.y.saturating_add_signed(dy),
        };
        Self {
            lamp: Emitter {
                kind: EmitterKind::DeskLamp,
                light: Self::halo(bulb),
                strength: level,
            },
            screen_idle,
        }
    }

    /// These lights with the lamp's pool centred on `centre`, where the art the
    /// painter draws hangs its bulb.
    pub(crate) fn relit_at(self, centre: Point) -> Self {
        Self {
            lamp: Emitter {
                light: Self::halo(centre),
                ..self.lamp
            },
            ..self
        }
    }

    fn halo(centre: Point) -> Light {
        Light::Halo {
            centre,
            radius: DESK_LAMP_RADIUS,
            share: DESK_LAMP_MAX,
        }
    }
}

/// The tool lighting the screen of a desk facing `facing`, whose occupant
/// `agent` sits there right now (`seated`, so not mid-walk): `None` where the
/// screen is dark, or shows us the monitor's back — a glow there would be
/// light leaking out of a case. The one rule the screen glow and its halo
/// share.
pub(crate) fn lit_screen(agent: &AgentSlot, facing: Facing, seated: bool) -> Option<ToolKind> {
    match agent.state {
        ActivityState::Active { kind, .. } if seated && facing == Facing::North => Some(kind),
        _ => None,
    }
}

/// The glow of a desk's screen: its occupant's [`lit_screen`],
/// tinted by the tool. Both profiles light screens from this.
pub(crate) fn desk_screen_glow(
    occupant: Option<&AgentSlot>,
    facing: crate::layout::Facing,
    seated: bool,
    theme: &crate::theme::Theme,
) -> Option<pixtuoid_core::sprite::Rgb> {
    occupant
        .and_then(|a| crate::lighting::lit_screen(a, facing, seated))
        .map(|tool| theme.tool_glow.for_kind(tool))
}

/// One halo over each [`lit_screen`], on the row above its desk, clear of the
/// monitor.
fn monitor_halos(layout: &SceneLayout, inputs: &LightInputs<'_>) -> Vec<Emitter> {
    inputs
        .agents
        .iter()
        .filter(|agent| agent.exiting_at.is_none() && agent.floor_idx == inputs.floor_idx)
        .filter_map(|agent| {
            let local = agent.desk_index.single_floor_local();
            let seated = inputs.seated.get(&local).copied().unwrap_or(false);
            let desk = layout.home_desk(local)?;
            let tool = lit_screen(agent, desk.facing, seated)?;
            Some(Emitter {
                kind: EmitterKind::MonitorHalo(tool),
                light: Light::Patch {
                    centre: Point {
                        x: desk.at.x + MONITOR_HALO_DX,
                        y: desk.at.y.saturating_sub(1),
                    },
                },
                strength: MONITOR_HALO_INTENSITY,
            })
        })
        .collect()
}

/// A 0..1 sine breath with trough `floor`. The ms reduce by INTEGER modulo
/// before any float cast — at wall-clock magnitude an f32 of the raw ms cannot
/// tell two frames apart (`neon_breath_advances_at_wall_clock_scale_and_stays_in_band`).
fn neon_breath(elapsed_ms: u64, period_ms: u64, floor: f32) -> f32 {
    let phase = (elapsed_ms % period_ms) as f32 / period_ms as f32;
    floor + (1.0 - floor) * ((std::f32::consts::TAU * phase).sin() * 0.5 + 0.5)
}

/// The neon halo's strength at the tube: breathing, dimmed by daylight, and
/// none from a tube driven no harder than it is starved.
fn neon_halo_strength(levels: NeonLevels, beat: crate::anim::Beat, darkness: f32) -> f32 {
    let ms = beat.ms();
    let brand = NEON_HALO_BRAND * neon_breath(ms, NEON_BREATH_MS, NEON_BREATH_FLOOR);
    let alert = NEON_HALO_ALERT * neon_breath(ms, NEON_ALERT_BREATH_MS, NEON_ALERT_BREATH_FLOOR);
    let daylight = NEON_DAYLIGHT_MIN + (1.0 - NEON_DAYLIGHT_MIN) * darkness.clamp(0.0, 1.0);
    // A tube only throws light ABOVE its starved level — one darker than the wall
    // it hangs on has none to give.
    let starved = NeonLevels::EMPTY.power;
    let throw = ((levels.power - starved) / (1.0 - starved)).max(0.0);
    throw * (brand + (alert - brand) * levels.alert) * daylight
}

#[cfg(test)]
mod tests;
