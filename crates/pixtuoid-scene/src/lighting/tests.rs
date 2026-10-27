use crate::anim::Beat;
use std::collections::HashMap;

use super::*;
use crate::display::pen::{ArtPx, layout_point, test_density};
use crate::layout::WINDOW_W;
use crate::sky::{Sky, Weather};

/// A desk away from every edge.
const DESK: Point = Point { x: 40, y: 30 };

fn bulbs() -> DeskBulbs {
    DeskBulbs::of(&crate::pack::test_office())
}

/// [`desk_lights`] of [`DESK`] facing `facing`, its bulb the bundled pack's.
fn lights(facing: crate::layout::Facing, darkness: f32, indoor: f32) -> DeskLights {
    desk_lights(DESK, facing, bulbs().at(facing), darkness, indoor)
}

/// Each facing lights the bulb its own desk art draws, the East and West
/// facings included.
#[test]
fn every_facing_lights_its_own_desks_bulb() {
    use crate::layout::Facing;
    let pack = crate::pack::test_office();
    for facing in [Facing::North, Facing::South, Facing::East, Facing::West] {
        assert_eq!(
            DeskBulbs::of(&pack).at(facing),
            pack.bulb_offset(crate::pack::Desk::facing(facing)),
            "{facing:?}"
        );
    }
}

/// A bulb drawn above the desk's own row still lights its lamp, centred on it.
#[test]
fn a_bulb_above_the_desk_row_still_lights() {
    use crate::layout::Facing;
    // the bulb moved to the art's top row, a row above the desk's point
    let raised: &'static str = include_str!("../../sprites/default/desk.sprite")
        .replacen(" 9 ", " D ", 1)
        .replacen("\n. . .", "\n9 . .", 1)
        .leak();
    let pack =
        crate::pack::OfficeArt::parse(crate::pack::test_pack_with(&[("desk.sprite", raised)]))
            .expect("the raised bulb is still a bulb");
    let bulb = DeskBulbs::of(&pack).at(Facing::South);
    let (_, dy) = bulb;
    assert!(dy < 0, "it stands above the desk's row: {dy}");
    let lights = desk_lights(DESK, Facing::South, bulb, 1.0, 1.0);
    assert!(lights.lamp.strength > 0.0);
    let Light::Halo { centre, .. } = lights.lamp.light else {
        panic!("a desk lamp throws a halo");
    };
    assert!(centre.y < DESK.y, "the pool centres on the raised bulb");
}

/// A WALL-CLOCK-scale epoch — the magnitude [`neon_breath`]'s integer modulo
/// exists for.
const WALL_CLOCK_MS: u64 = 1_767_000_000_000;

fn neon_at(levels: NeonLevels, darkness: f32) -> f32 {
    neon_halo_strength(levels, Beat::at_ms(WALL_CLOCK_MS), darkness)
}

/// The lights of a `w`×`h` office with nobody in it, at `hour` under a clear sky.
fn lights_at(w: u16, h: u16, hour: u32) -> (SceneLayout, Lights) {
    let layout = SceneLayout::compute(w, h, Some(crate::layout::TEST_DEFAULT_DESKS)).expect("fits");
    let sky = Sky::at_with(crate::localclock::at_hour(hour), Weather::Clear);
    let look = SkyTones::resolve(&sky, &crate::theme::NORMAL);
    let lights = Lights::of(
        &layout,
        &look,
        &LightInputs {
            agents: &[],
            seated: &HashMap::new(),
            floor_idx: 0,
            indoor_scale: 1.0,
            neon: NeonLevels::RUNNING,
            beat: Beat::at_ms(0),
            bulbs: crate::lighting::DeskBulbs::of(&crate::pack::test_office()),
        },
    );
    (layout, lights)
}

#[test]
fn neon_breath_advances_at_wall_clock_scale_and_stays_in_band() {
    let a = neon_breath(WALL_CLOCK_MS, NEON_BREATH_MS, NEON_BREATH_FLOOR);
    let b = neon_breath(WALL_CLOCK_MS + 33, NEON_BREATH_MS, NEON_BREATH_FLOOR);
    assert_ne!(a, b, "the breath must move across a 33ms frame");
    for ms in (0..NEON_BREATH_MS).step_by(97) {
        let v = neon_breath(WALL_CLOCK_MS + ms, NEON_BREATH_MS, NEON_BREATH_FLOOR);
        assert!((NEON_BREATH_FLOOR..=1.0).contains(&v), "{v} at +{ms}ms");
    }
}

#[test]
fn neon_halo_drops_to_its_daylight_floor_and_a_calm_sign_glows_less_than_a_running_one() {
    let night = neon_at(NeonLevels::RUNNING, 1.0);
    let day = neon_at(NeonLevels::RUNNING, 0.0);
    assert!(
        (day - night * NEON_DAYLIGHT_MIN).abs() < 1e-6,
        "{day} vs {night}"
    );
    let calm = neon_at(NeonLevels::CALM, 1.0);
    assert!(calm > 0.0 && calm < night, "{calm} vs {night}");
}

#[test]
fn a_starved_tube_throws_no_halo_and_a_flash_does() {
    assert_eq!(neon_at(NeonLevels::EMPTY, 1.0), 0.0);
    assert!(neon_at(NeonLevels::FLASH, 1.0) > 0.0);
}

#[test]
fn the_neon_halo_throws_the_signs_own_levels() {
    let layout =
        SceneLayout::compute(192, 80, Some(crate::layout::TEST_DEFAULT_DESKS)).expect("fits");
    let sky = Sky::at_with(crate::localclock::at_hour(0), Weather::Clear);
    let look = SkyTones::resolve(&sky, &crate::theme::NORMAL);
    let beat = Beat::at_ms(WALL_CLOCK_MS);
    for levels in [NeonLevels::CALM, NeonLevels::ALERT, NeonLevels::EMPTY] {
        let lights = Lights::of(
            &layout,
            &look,
            &LightInputs {
                agents: &[],
                seated: &HashMap::new(),
                floor_idx: 0,
                indoor_scale: 1.0,
                neon: levels,
                beat,
                bulbs: bulbs(),
            },
        );
        assert_eq!(
            lights.neon.strength,
            neon_halo_strength(levels, beat, look.darkness),
            "{levels:?}"
        );
    }
}

#[test]
fn the_sun_spills_through_every_window_by_day_and_none_by_night() {
    let (layout, noon) = lights_at(192, 80, 12);
    assert_eq!(noon.spills.len(), layout.window_bays().count());
    assert!(noon.spills.iter().all(|s| s.strength > 0.0));
    let (_, midnight) = lights_at(192, 80, 0);
    assert!(midnight.spills.is_empty());
}

#[test]
fn a_spills_bounds_hold_every_row_whichever_way_it_leans() {
    for slant in [-0.7_f32, 0.0, 0.7] {
        let spill = Emitter {
            kind: EmitterKind::WindowSpill,
            light: Light::Spill {
                x: 40,
                w: WINDOW_W,
                top: 10,
                slant,
            },
            strength: 1.0,
        };
        let ((x0, y0), (x1, y1)) = spill.bounds();
        assert_eq!((y0, y1), (10, 10 + SPILL_DEPTH));
        let rows: Vec<_> = spill_rows(40, WINDOW_W, slant).collect();
        assert!(
            rows.iter()
                .all(|r| r.start >= i32::from(x0) && r.end <= i32::from(x1))
        );
        assert!(
            rows.iter().any(|r| r.start == i32::from(x0)),
            "{slant}: tight on the west"
        );
        assert!(
            rows.iter().any(|r| r.end == i32::from(x1)),
            "{slant}: tight on the east"
        );
    }
}

#[test]
fn a_monitor_halo_hangs_over_each_lit_screen_only() {
    let layout =
        SceneLayout::compute(192, 80, Some(crate::layout::TEST_DEFAULT_DESKS)).expect("fits");
    let facing = |i: usize| layout.home_desks[i].facing;
    let north: Vec<usize> = (0..layout.home_desks.len())
        .filter(|&i| facing(i) == Facing::North)
        .collect();
    let south = (0..layout.home_desks.len())
        .find(|&i| facing(i) == Facing::South)
        .expect("this layout seats both ways");
    let [lit, walking, idle, ..] = north[..] else {
        panic!("this layout needs three screens facing us: {north:?}");
    };
    let id = |p: &str| pixtuoid_core::AgentId::from_transcript_path(p);
    // No tool detail: a screen lights for any tool call.
    let running = pixtuoid_core::state::ActivityState::Running {
        item: Some(ToolUse {
            tool_use_id: None,
            detail: None,
            kind: pixtuoid_core::state::ToolKind::Edit,
        }),
    };
    let at_desk = |path: &str, desk: usize, state: pixtuoid_core::state::ActivityState| {
        let mut a = crate::character::test_support::make_slot(id(path), state);
        a.desk_index = pixtuoid_core::state::GlobalDeskIndex(desk);
        a
    };
    let agents = [
        at_desk("/lit.jsonl", lit, running.clone()),
        at_desk("/walking.jsonl", walking, running.clone()),
        at_desk(
            "/idle.jsonl",
            idle,
            pixtuoid_core::state::ActivityState::Idle,
        ),
        at_desk("/back.jsonl", south, running),
    ];
    let seated = HashMap::from([
        (FloorLocalDeskIndex(lit), true),
        (FloorLocalDeskIndex(walking), false),
        (FloorLocalDeskIndex(idle), true),
        (FloorLocalDeskIndex(south), true),
    ]);
    let sky = Sky::at_with(crate::localclock::at_hour(0), Weather::Clear);
    let lights = Lights::of(
        &layout,
        &SkyTones::resolve(&sky, &crate::theme::NORMAL),
        &LightInputs {
            agents: &agents,
            seated: &seated,
            floor_idx: 0,
            indoor_scale: 1.0,
            neon: NeonLevels::RUNNING,
            beat: Beat::at_ms(0),
            bulbs: crate::lighting::DeskBulbs::of(&crate::pack::test_office()),
        },
    );
    let kinds: Vec<_> = lights.monitor_halos.iter().map(|h| h.kind).collect();
    assert_eq!(
        kinds,
        [EmitterKind::MonitorHalo(ScreenWork::ToolUse(
            pixtuoid_core::state::ToolKind::Edit
        ))],
        "only the seated, mid-call agent at a screen facing us"
    );
    let desk = layout.home_desks[lit].at;
    assert_eq!(
        lights.monitor_halos[0].light,
        Light::Patch {
            centre: Point {
                x: desk.x + MONITOR_HALO_DX,
                y: desk.y - 1,
            }
        }
    );
}

#[test]
fn a_desk_lamp_is_lit_whichever_way_the_desk_seats_its_occupant() {
    use crate::layout::Facing;
    // A lamp is a FIXTURE on a wing (west, east on the back-turned desk), visible
    // from either side; the standby SCREEN is the one that gates on facing.
    for darkness in [0.0_f32, 0.5, 1.0] {
        let north = lights(Facing::North, darkness, 1.0);
        let south = lights(Facing::South, darkness, 1.0);
        assert_eq!(
            north.lamp.strength, south.lamp.strength,
            "the lamp may not depend on facing (darkness {darkness})"
        );
        assert!(
            south.screen_idle == 0.0 && north.screen_idle >= south.screen_idle,
            "only a back-turned desk shows its screen (darkness {darkness})"
        );
    }
    assert!(
        lights(Facing::South, 1.0, 1.0).lamp.strength > 0.0,
        "a viewer-facing desk must still light its lamp after dark"
    );
}

/// The two desk emitters (`lamp`, `screen_idle`); the floor lamp shares the
/// rule but not this pin. Dropping either factor makes that emitter's two
/// readings equal.
#[test]
fn an_emptied_floor_takes_both_desk_emitters_down_with_the_level() {
    use crate::layout::Facing;
    let min = crate::floor::VacancyDim::MIN_LEVEL;
    let lit = lights(Facing::North, 1.0, 1.0);
    let empty = lights(Facing::North, 1.0, min);
    for (what, lit, empty) in [
        ("lamp", lit.lamp.strength, empty.lamp.strength),
        ("screen_idle", lit.screen_idle, empty.screen_idle),
    ] {
        assert!(
            (empty - lit * min).abs() < f32::EPSILON,
            "an empty floor's {what} must scale with the level, got {empty} against {lit}"
        );
    }
}

/// One of every shape, lit.
fn every_shape() -> [Emitter; 4] {
    let lit = |kind, light| Emitter {
        kind,
        light,
        strength: 0.8,
    };
    [
        lit(
            EmitterKind::DeskLamp,
            Light::Halo {
                centre: Point { x: 30, y: 20 },
                radius: 5,
                share: 0.4,
            },
        ),
        lit(
            EmitterKind::NeonGlow,
            Light::Glow {
                at: Point { x: 25, y: 15 },
                w: 8,
                h: 4,
                reach: 6,
            },
        ),
        lit(
            EmitterKind::WindowSpill,
            Light::Spill {
                x: 26,
                w: WINDOW_W,
                top: 12,
                slant: 0.4,
            },
        ),
        lit(
            EmitterKind::MonitorHalo(ScreenWork::ToolUse(ToolKind::Read)),
            Light::Patch {
                centre: Point { x: 30, y: 20 },
            },
        ),
    ]
}

/// Whether a shape lights whole cells rather than a continuous falloff; no
/// wildcard, so a new shape fails to compile until it is classified.
fn is_whole_cell(light: &Light) -> bool {
    match light {
        Light::Spill { .. } | Light::Patch { .. } => true,
        Light::Halo { .. } | Light::Glow { .. } => false,
    }
}

#[test]
fn a_light_sampled_between_cells_stays_inside_its_bounds() {
    const D: u16 = 4;
    let at = |a: u16| layout_point(ArtPx(a), test_density(D));
    for e in every_shape() {
        let ((x0, y0), (x1, y1)) = e.bounds();
        // A whole-cell shape lights exactly its bounds' cells; a continuous
        // one's reach ends at its bound, so its far-edge pixels sit up to half
        // a cell past the last cell.
        let whole_cell = is_whole_cell(&e.light);
        for y in 0..48 * D {
            for x in 0..64 * D {
                let (fx, fy) = (at(x), at(y));
                if e.level_at_f(fx, fy).is_none() {
                    continue;
                }
                let inside = if whole_cell {
                    (i32::from(x0)..i32::from(x1)).contains(&unit_holding(fx))
                        && (i32::from(y0)..i32::from(y1)).contains(&unit_holding(fy))
                } else {
                    fx >= f32::from(x0)
                        && fx < f32::from(x1)
                        && fy >= f32::from(y0)
                        && fy < f32::from(y1)
                };
                assert!(
                    inside,
                    "{:?} lights ({fx}, {fy}) outside {:?}",
                    e.kind,
                    e.bounds()
                );
            }
        }
    }
}

/// A whole-cell shape sampled the way the display does ([`layout_point`])
/// lights exactly the art pixels of the cells it lights at density 1.
#[test]
fn a_whole_cell_light_lights_the_same_cells_at_any_density() {
    const D: u16 = 4;
    for e in every_shape() {
        if !is_whole_cell(&e.light) {
            continue;
        }
        for y in 0..48 * D {
            for x in 0..64 * D {
                let at = |a: u16| layout_point(ArtPx(a), test_density(D));
                assert_eq!(
                    e.level_at_f(at(x), at(y)).is_some(),
                    e.level_at(x / D, y / D).is_some(),
                    "{:?} art pixel ({x}, {y})",
                    e.kind
                );
            }
        }
    }
}

/// A painter steps a light's levels down from its peak, so the peak must be
/// the level its brightest cell actually gets — every shape.
#[test]
fn a_lights_peak_is_its_brightest_cell() {
    let at = |x, y| Point { x, y };
    let lights = [
        Light::Halo {
            centre: at(30, 20),
            radius: 11,
            share: 1.0,
        },
        Light::Halo {
            centre: at(30, 20),
            radius: 5,
            share: 0.42,
        },
        Light::Glow {
            at: at(20, 20),
            w: 30,
            h: 8,
            reach: NEON_HALO_RADIUS,
        },
        Light::Spill {
            x: 20,
            w: WINDOW_W,
            top: 10,
            slant: 0.3,
        },
        Light::Patch { centre: at(30, 20) },
    ];
    for light in lights {
        let e = Emitter {
            kind: EmitterKind::FloorLamp,
            light,
            strength: 0.6,
        };
        let ((x0, y0), (x1, y1)) = e.bounds();
        let brightest = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter_map(|(x, y)| e.level_at(x, y))
            .fold(0.0_f32, f32::max);
        assert!(
            (e.peak() - brightest).abs() < 1e-6,
            "{light:?}: peak {} vs brightest {brightest}",
            e.peak()
        );
    }
}
