use super::*;

use crate::localclock::{at_hour_min, on_day};
use std::time::Duration;

/// The level [`strike_phase_at`] lights the sky with at `beat`, 0 with no strike.
fn flash_level_at(beat: crate::anim::Beat, policy: WeatherPolicy) -> f32 {
    strike_phase_at(beat, policy).map_or(0.0, StrikePhase::level)
}

#[test]
fn the_clock_picks_every_weather_within_a_week() {
    use std::collections::HashSet;
    const WEEK_MS: u64 = 7 * 24 * crate::anim::HOUR_MS;
    let start = 1_700_000_000_000 / WEATHER_CYCLE_MS;
    let seen: HashSet<Weather> = (start..start + WEEK_MS / WEATHER_CYCLE_MS)
        .map(slot_weather)
        .collect();
    for w in Weather::ALL {
        assert!(
            seen.contains(&w),
            "the clock never picked {w:?} in a week of slots"
        );
    }
}

#[test]
fn weather_name_round_trips_for_every_variant() {
    for w in Weather::ALL {
        assert_eq!(Weather::from_name(w.name()), Some(w), "{w:?} round-trips");
    }
    assert_eq!(Weather::from_name("  SNOW "), Some(Weather::Snow));
    assert_eq!(Weather::from_name("drizzle"), None);
}

#[test]
fn body_is_sun_by_day_moon_by_night_never_both() {
    for slot in 0..48u32 {
        let (h, m) = (slot / 2, (slot % 2) * 30);
        let s = at_hour_min(h, m);
        let e = *Sky::clock(s).body();
        match e.kind {
            BodyKind::Sun => assert!(
                (5.0..20.0).contains(&(h as f32 + m as f32 / 60.0)),
                "sun only during the daylight ramp, got {h}:{m:02}"
            ),
            BodyKind::Moon => assert!(
                !(5.0..20.0).contains(&(h as f32 + m as f32 / 60.0)),
                "moon only when the sun is down, got {h}:{m:02}"
            ),
        }
    }
}

#[test]
fn sun_altitude_peaks_near_midday_and_bottoms_at_the_horizon() {
    let noon = Sky::clock(at_hour_min(12, 30)).body().altitude;
    let dawn = Sky::clock(at_hour_min(6, 30)).body().altitude;
    let dusk = Sky::clock(at_hour_min(18, 0)).body().altitude;
    assert!(noon > 0.8, "midday sun rides high: {noon}");
    // The two thresholds differ because 06:30 and 18:00 sit unequally far from
    // [`SUN_RISE_H`] and [`SUN_SET_H`].
    assert!(
        dawn < 0.4 && dusk < 0.5,
        "dawn/dusk sit low: {dawn} / {dusk}"
    );
}

#[test]
fn warmth_is_high_low_on_the_horizon_and_neutral_at_apex() {
    assert!(
        Sky::clock(at_hour_min(6, 30)).body().warmth > 0.6,
        "low sun is warm/red"
    );
    assert!(
        Sky::clock(at_hour_min(12, 30)).body().warmth < 0.3,
        "apex sun is neutral"
    );
}

#[test]
fn azimuth_advances_from_dawn_to_dusk() {
    let a = Sky::clock(at_hour_min(7, 0)).body().azimuth;
    let b = Sky::clock(at_hour_min(12, 0)).body().azimuth;
    let c = Sky::clock(at_hour_min(18, 0)).body().azimuth;
    assert!(
        a < b && b < c,
        "azimuth grows through the day: {a} < {b} < {c}"
    );
}

#[test]
fn moon_luminance_tracks_phase() {
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    let (mut lo_lum, mut hi_lum) = (0.0, 0.0);
    for day in 1..=30u32 {
        let s = on_day(day, 2);
        let frac = moon_phase_at(s);
        let lum = Sky::clock(s).body().lum;
        if frac < lo {
            lo = frac;
            lo_lum = lum;
        }
        if frac > hi {
            hi = frac;
            hi_lum = lum;
        }
    }
    assert!(
        hi_lum > lo_lum,
        "fuller moon lights brighter ({hi_lum} vs {lo_lum})"
    );
}

/// `Clock` is the clock's weather at every instant, and `Forced` holds its
/// weather, pure, whatever the clock, through its transitions included.
#[test]
fn a_policy_picks_the_clock_or_holds_its_weather() {
    const STRIDE_MS: usize = 7_000;
    for s in (0..40 * WEATHER_CYCLE_MS).step_by(STRIDE_MS) {
        let now = at_ms(1_700_000_000_000 + s);
        assert_eq!(
            Sky::at(crate::anim::Motion::Full.timing(now), WeatherPolicy::Clock).weather(),
            clock_weather(now),
            "{s}"
        );
        for w in Weather::ALL {
            assert_eq!(
                Sky::at(
                    crate::anim::Motion::Full.timing(now),
                    WeatherPolicy::Forced(w)
                )
                .weather(),
                WeatherMix::pure(w)
            );
        }
    }
    assert_eq!(WeatherPolicy::default(), WeatherPolicy::Clock);
    assert_eq!(WeatherPolicy::from_name(None), Ok(WeatherPolicy::Clock));
    assert_eq!(
        WeatherPolicy::from_name(Some(" STORM ")),
        Ok(WeatherPolicy::Forced(Weather::Storm))
    );
    assert_eq!(
        WeatherPolicy::from_name(Some("stormy")),
        Err(weather_names())
    );
}

#[test]
fn storm_transmits_less_than_rain_overall() {
    let s = transmission(Weather::Storm);
    let r = transmission(Weather::Rain);
    assert!(
        s.direct <= r.direct && s.diffuse < r.diffuse,
        "storm steady-state darker than rain: {s:?} vs {r:?}"
    );
}

#[test]
fn clear_beams_hard_overcast_kills_the_beam() {
    assert!(
        transmission(Weather::Clear).direct > 0.9,
        "clear = hard beam"
    );
    for w in [Weather::Overcast, Weather::Rain, Weather::Storm] {
        assert_eq!(
            transmission(w).direct,
            0.0,
            "{w:?} scatters the beam to nothing"
        );
    }
}

#[test]
fn fog_is_a_luminous_diffuse_whiteout() {
    let f = transmission(Weather::Fog);
    assert!(
        f.diffuse >= transmission(Weather::Overcast).diffuse,
        "fog is a bright veil"
    );
    assert!(f.direct < 0.2, "fog is near-shadowless");
    assert!(f.disc < 0.2, "the disc is lost in fog");
}

#[test]
fn disc_visibility_is_clear_then_hazy_then_gone() {
    assert!(transmission(Weather::Clear).disc > 0.9);
    assert!(
        (0.0..0.6).contains(&transmission(Weather::Smog).disc),
        "haze half-hides the disc"
    );
    assert!(
        transmission(Weather::Overcast).disc < 0.1,
        "overcast hides the disc"
    );
}

#[test]
fn windy_near_full_beam() {
    assert!(
        transmission(Weather::Windy).direct > 0.5,
        "windy keeps a strong beam"
    );
}

#[test]
fn haze_and_snow_keep_a_faint_but_nonzero_beam() {
    for w in [Weather::Snow, Weather::Fog, Weather::Smog] {
        let d = transmission(w).direct;
        assert!(
            0.0 < d && d < 0.5,
            "{w:?} should keep a faint but nonzero beam: {d}"
        );
    }
}

#[test]
fn storm_diffuse_dimmer_than_overcast() {
    assert!(
        transmission(Weather::Storm).diffuse < transmission(Weather::Overcast).diffuse,
        "storm diffuse should be dimmer than overcast"
    );
}

#[test]
fn night_floor_varies_by_weather() {
    assert!(
        city_bounce(Weather::Snow) >= city_bounce(Weather::Clear),
        "snow albedo should bounce at least as much as a clear night"
    );
    assert!(
        city_bounce(Weather::Clear) > city_bounce(Weather::Overcast),
        "clear night should out-glow overcast"
    );
    assert!(
        city_bounce(Weather::Storm) < city_bounce(Weather::Overcast),
        "storm should swallow more glow than overcast"
    );
    assert!(
        city_bounce(Weather::Storm) < city_bounce(Weather::Clear),
        "storm should swallow more glow than a clear night"
    );
    for w in Weather::ALL {
        assert!(
            city_bounce(w) > 0.0,
            "{w:?} must keep a nonzero city-bounce floor (never pitch black)"
        );
    }
}

#[test]
fn lightning_envelope_is_a_two_pulse_then_dark() {
    let mid = |phase: u64| phase * STRIKE_PHASE_MS + STRIKE_PHASE_MS / 2;
    assert_eq!(lightning_envelope(0), 1.0, "primary strike");
    assert!(
        lightning_envelope(mid(1)) < lightning_envelope(mid(0)),
        "dim between flickers"
    );
    assert!(
        lightning_envelope(mid(2)) > lightning_envelope(mid(1)),
        "after-flash rebrightens"
    );
    assert_eq!(lightning_envelope(STRIKE_MS), 0.0, "flash is over");
    assert_eq!(lightning_envelope(5000), 0.0, "dark between strikes");
}

/// A strike stays inside the photosensitive-safe envelope: at most four
/// phases, each at least [`PHOTOSENSITIVE_PHASE_MIN_MS`], and at most
/// [`PHOTOSENSITIVE_FLASHES_PER_SECOND`] flashes in any second.
///
/// [`PHOTOSENSITIVE_PHASE_MIN_MS`]: crate::anim::PHOTOSENSITIVE_PHASE_MIN_MS
/// [`PHOTOSENSITIVE_FLASHES_PER_SECOND`]: crate::anim::PHOTOSENSITIVE_FLASHES_PER_SECOND
#[test]
fn a_strike_keeps_the_photosensitive_flash_bounds() {
    let levels: Vec<f32> = (0..=STRIKE_MS).map(lightning_envelope).collect();
    let mut phases: Vec<(f32, u64)> = Vec::new();
    for &l in &levels {
        match phases.last_mut() {
            Some((level, len)) if *level == l => *len += 1,
            _ => phases.push((l, 1)),
        }
    }
    let lit = &phases[..phases.len() - 1];
    assert!(lit.len() <= 4, "{lit:?}");
    assert!(
        lit.iter()
            .all(|&(_, len)| len >= crate::anim::PHOTOSENSITIVE_PHASE_MIN_MS),
        "{lit:?}"
    );
    let flashes = crate::anim::most_flashes_in_a_second(
        std::iter::once((0, 0.0)).chain((0..=STRIKE_MS).map(|ms| (ms, lightning_envelope(ms)))),
    );
    assert!(
        flashes <= crate::anim::PHOTOSENSITIVE_FLASHES_PER_SECOND,
        "{phases:?}"
    );
}

/// A full storm on every tier, sampled at a live painter's rate across many
/// strikes: never more than [`PHOTOSENSITIVE_FLASHES_PER_SECOND`] flashes in
/// any second of wall time.
///
/// [`PHOTOSENSITIVE_FLASHES_PER_SECOND`]: crate::anim::PHOTOSENSITIVE_FLASHES_PER_SECOND
#[test]
fn a_storm_flashes_at_most_three_times_a_second_on_every_tier() {
    use crate::anim::{Motion, PAINT_FRAME_MS, PHOTOSENSITIVE_FLASHES_PER_SECOND};
    const BUCKETS: u64 = 40;
    let frame_ms = PAINT_FRAME_MS;
    let storm = WeatherPolicy::Forced(Weather::Storm);
    for motion in Motion::ALL {
        let span = BUCKETS * LIGHTNING_PERIOD_MS * motion.pace().unwrap_or(1);
        let samples = (0..span).step_by(frame_ms as usize).map(|ms| {
            let wall = std::time::UNIX_EPOCH + Duration::from_millis(ms);
            (ms, flash_level_at(motion.beat(wall), storm))
        });
        let most = crate::anim::most_flashes_in_a_second(samples);
        assert!(
            most <= PHOTOSENSITIVE_FLASHES_PER_SECOND,
            "{motion:?}: {most}"
        );
        assert!(
            motion == Motion::Still || most > 0,
            "{motion:?} never struck"
        );
    }
}

/// Two strikes never fall within a second of each other, so their flashes
/// never add up past three a second.
#[test]
fn strikes_are_at_least_a_second_apart() {
    const SECOND_MS: u64 = 1000;
    for bucket in 0..100_000u64 {
        let end = bucket * LIGHTNING_PERIOD_MS + strike_offset(bucket) + STRIKE_MS;
        let next = (bucket + 1) * LIGHTNING_PERIOD_MS + strike_offset(bucket + 1);
        assert!(next - end >= SECOND_MS, "bucket {bucket}");
    }
}

#[test]
fn lightning_strikes_are_jittered_not_metronomic() {
    let offsets: Vec<u64> = (0..24u64).map(strike_offset).collect();
    let distinct = offsets
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert!(
        distinct > 12,
        "strike offsets should vary across buckets, got {offsets:?}"
    );
    assert!(offsets.iter().all(|&o| o < LIGHTNING_PERIOD_MS - STRIKE_MS));
}

/// At rest no strike flashes, for the photosensitive.
#[test]
fn no_strike_flashes_at_rest() {
    for bucket in 0..24u64 {
        let strike = std::time::UNIX_EPOCH
            + std::time::Duration::from_millis(
                bucket * LIGHTNING_PERIOD_MS + strike_offset(bucket),
            );
        let sky = Sky::at(
            crate::anim::Motion::Still.timing(strike),
            WeatherPolicy::Forced(Weather::Storm),
        );
        assert_eq!(sky.flash(), 0.0, "bucket {bucket}");
    }
}

/// The one pin on the clock-to-flash path through [`Sky::at`], and on the
/// flash being the storm's alone; painter tests inject the strike with
/// [`Sky::with_strike`].
#[test]
fn a_strike_flashes_at_its_bucket_offset_and_ends_with_the_flash() {
    for bucket in 0..24u64 {
        let off = strike_offset(bucket);
        let at = |ms: u64| {
            std::time::UNIX_EPOCH
                + std::time::Duration::from_millis(bucket * LIGHTNING_PERIOD_MS + ms)
        };
        let storm = |ms| Sky::at_with(at(ms), Weather::Storm).flash();
        assert_eq!(storm(off), lightning_envelope(0), "bucket {bucket}");
        assert_eq!(storm(off + STRIKE_MS), 0.0, "bucket {bucket}");
        if off > 0 {
            assert_eq!(storm(off - 1), 0.0, "bucket {bucket}");
        }
        for w in Weather::ALL.into_iter().filter(|&w| w != Weather::Storm) {
            assert_eq!(Sky::at_with(at(off), w).flash(), 0.0, "{w:?} never strikes");
        }
    }
}

#[test]
fn night_exterior_tracks_weather_at_a_fixed_phase() {
    // One instant, so one moon phase: only the weather varies.
    let night = on_day(1, 2);
    let clear = Sky::at_with(night, Weather::Clear).light().exterior;
    let storm = Sky::at_with(night, Weather::Storm).light().exterior;
    assert!(
        clear > storm,
        "clear night brighter than storm night at equal phase: {clear} vs {storm}"
    );
    assert!(storm > 0.0, "storm night keeps some city glow: {storm}");
    let noon = Sky::at_with(at_hour_min(12, 0), Weather::Clear)
        .light()
        .exterior;
    assert!(noon > 0.9, "clear noon ~fully lit: {noon}");
}

#[test]
fn interior_brightness_is_altitude_coupled() {
    let noon = Sky::at_with(at_hour_min(12, 0), Weather::Storm)
        .light()
        .exterior;
    let dusk = Sky::at_with(at_hour_min(18, 0), Weather::Storm)
        .light()
        .exterior;
    assert!(
        noon > dusk,
        "a stormy noon out-lights a stormy dusk: {noon} vs {dusk}"
    );
}

#[test]
fn solar_noon_outshines_the_brightest_night() {
    // Every weather at the FULLEST moon: its `city_bounce` floor plus peak lunar
    // illumination.
    let full_moon_day = (1..=31u32)
        .max_by(|&a, &b| {
            moon_phase_at(on_day(a, 2))
                .partial_cmp(&moon_phase_at(on_day(b, 2)))
                .expect("moon_phase is never NaN")
        })
        .expect("January has days");
    // The night's brightest ten-minute mark, since the moon's apex moves with
    // its age.
    let night = (0..9 * 6u64)
        .map(|m| on_day(full_moon_day, 20) + std::time::Duration::from_secs(m * 10 * 60));

    let storm_noon = Sky::at_with(at_hour_min(12, 0), Weather::Storm)
        .light()
        .exterior;
    for w in Weather::ALL {
        let full_moon = night
            .clone()
            .map(|t| Sky::at_with(t, w).light().exterior)
            .fold(0.0_f32, f32::max);
        assert!(
            storm_noon > full_moon,
            "a stormy solar noon must outshine a {w:?} full-moon midnight: \
             storm_noon={storm_noon} vs {full_moon}"
        );
    }
}

/// `y-mo-d h:mi` UTC.
fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> SystemTime {
    let secs = chrono::NaiveDate::from_ymd_opt(y, mo, d)
        .and_then(|date| date.and_hms_opt(h, mi, 0))
        .expect("a valid instant")
        .and_utc()
        .timestamp();
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(secs).expect("after 1970"))
}

/// How far a true lunation strays from the mean one: under a day.
const MAX_MISS_DAYS: f32 = 1.0;

/// 2026's full moons, `(month, day, hour, minute)` UTC, from NASA GSFC's "Phases
/// of the Moon 2001 to 2100"
/// (<https://eclipse.gsfc.nasa.gov/phase/phases2001.html>, Universal Time).
const FULL_MOONS_2026: [(u32, u32, u32, u32); 13] = [
    (1, 3, 10, 3),
    (2, 1, 22, 9),
    (3, 3, 11, 38),
    (4, 2, 2, 12),
    (5, 1, 17, 23),
    (5, 31, 8, 45),
    (6, 29, 23, 57),
    (7, 29, 14, 36),
    (8, 28, 4, 18),
    (9, 26, 16, 49),
    (10, 26, 4, 12),
    (11, 24, 14, 53),
    (12, 24, 1, 28),
];

/// The phase lands on every 2026 new and full moon in the same NASA table as
/// [`FULL_MOONS_2026`]; the tolerance is the illuminated fraction a
/// [`MAX_MISS_DAYS`] miss leaves.
#[test]
fn the_moon_phase_lands_on_published_lunations() {
    let tolerance = (1.0 - (std::f32::consts::TAU * MAX_MISS_DAYS / SYNODIC_DAYS).cos()) / 2.0;
    let new_moons = [
        (1, 18, 19, 52),
        (2, 17, 12, 1),
        (3, 19, 1, 23),
        (4, 17, 11, 52),
        (5, 16, 20, 1),
        (6, 15, 2, 54),
        (7, 14, 9, 43),
        (8, 12, 17, 37),
        (9, 11, 3, 27),
        (10, 10, 15, 50),
        (11, 9, 7, 2),
        (12, 9, 0, 52),
    ];
    for (mo, d, h, mi) in new_moons {
        let lit = moon_phase_at(utc(2026, mo, d, h, mi));
        assert!(lit < tolerance, "new moon 2026-{mo}-{d} {h}:{mi} lit {lit}");
    }
    for (mo, d, h, mi) in FULL_MOONS_2026 {
        let lit = moon_phase_at(utc(2026, mo, d, h, mi));
        assert!(
            lit > 1.0 - tolerance,
            "full moon 2026-{mo}-{d} {h}:{mi} lit {lit}"
        );
    }
}

/// [`NEW_MOON_EPOCH_UNIX_DAYS`]'s literal and the instant its doc names are one
/// fact: a date that drifts from its number is how the old epoch went wrong.
#[test]
fn the_new_moon_epoch_is_the_instant_its_doc_names() {
    let named = utc(2019, 11, 27, 2, 56)
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs_f64()
        / 86_400.0;
    // A few minutes: above the literal's f32 step, below any mistyped date.
    assert!(
        (f64::from(NEW_MOON_EPOCH_UNIX_DAYS) - named).abs() < 0.005,
        "{NEW_MOON_EPOCH_UNIX_DAYS} is not 2019-11-27 02:56 ({named})"
    );
}

/// The moon waxes from new to full and wanes after, on the First and Last
/// Quarter columns of NASA GSFC's "Phases of the Moon 2001 to 2100"
/// (<https://eclipse.gsfc.nasa.gov/phase/phases2001.html>, Universal Time).
#[test]
fn the_moon_waxes_to_full_and_wanes_after() {
    for (mo, d, h, mi) in [(1, 26, 4, 47), (7, 21, 11, 6)] {
        assert!(
            Sky::clock(utc(2026, mo, d, h, mi)).moon_waxing(),
            "first quarter 2026-{mo}-{d} {h}:{mi} waxes"
        );
    }
    for (mo, d, h, mi) in [(1, 10, 15, 48), (8, 6, 2, 21)] {
        assert!(
            !Sky::clock(utc(2026, mo, d, h, mi)).moon_waxing(),
            "last quarter 2026-{mo}-{d} {h}:{mi} wanes"
        );
    }
    // The flip itself sits within a miss of every full moon, not just somewhere
    // between the quarters.
    let miss = std::time::Duration::from_secs_f32(MAX_MISS_DAYS * 86_400.0);
    for (mo, d, h, mi) in FULL_MOONS_2026 {
        let full = utc(2026, mo, d, h, mi);
        assert!(
            Sky::clock(full - miss).moon_waxing(),
            "waxes a miss before full 2026-{mo}-{d}"
        );
        assert!(
            !Sky::clock(full + miss).moon_waxing(),
            "wanes a miss after full 2026-{mo}-{d}"
        );
    }
}

/// A full moon is up from dusk to dawn, a first quarter sets before midnight,
/// and a new moon is down all night.
#[test]
fn the_moon_keeps_its_phases_hours() {
    let up = |h: f32, age: f32| moon_arc(h, age).is_some();
    let full = SYNODIC_DAYS / 2.0;
    for h in [20.5, 23.0, 2.0, 4.5] {
        assert!(up(h, full), "a full moon is up at {h}");
    }
    let first_quarter = SYNODIC_DAYS / 4.0;
    assert!(
        up(22.5, first_quarter),
        "a first quarter is still up late evening"
    );
    assert!(
        !up(23.5, first_quarter),
        "a first quarter has set by midnight"
    );
    for h in [20.5, 23.0, 2.0, 4.5] {
        assert!(!up(h, 0.0), "a new moon is down at {h}");
    }
}

/// Night comes in over [`NIGHTFALL_H`] after dusk and goes over it before dawn.
#[test]
fn nightfall_ramps_across_dusk_and_dawn() {
    assert_eq!(nightfall(12.0), 0.0);
    assert_eq!(nightfall(SUN_SET_H), 0.0);
    assert!((0.0..1.0).contains(&nightfall(SUN_SET_H + NIGHTFALL_H / 2.0)));
    assert_eq!(nightfall(0.0), 1.0);
    assert!((0.0..1.0).contains(&nightfall(SUN_RISE_H - NIGHTFALL_H / 2.0)));
}

/// The moon's light and the sky's other night terms ride one `nightfall`: a
/// night whose moon is already up through the dusk ramp.
#[test]
fn the_moons_light_rides_the_skys_nightfall() {
    let ramp_lit = |m: u64| {
        let s = Sky::at_with(
            on_day(1, 20) + std::time::Duration::from_secs(m * 60),
            Weather::Clear,
        );
        s.body().altitude > 0.0 && s.nightfall() < 1.0
    };
    assert!((0..45).any(ramp_lit), "the moon is up while night comes in");
    for m in 0..9 * 60u64 {
        let t = on_day(1, 20) + std::time::Duration::from_secs(m * 60);
        let s = Sky::at_with(t, Weather::Clear);
        let e = s.body();
        assert_eq!(
            e.lum,
            MOON_PEAK_LUM * e.altitude * s.moon_phase() * s.nightfall(),
            "minute {m}"
        );
    }
}

fn at_ms(ms: u64) -> SystemTime {
    std::time::UNIX_EPOCH + Duration::from_millis(ms)
}

const HOLD_MS: u64 = WEATHER_CYCLE_MS - TRANSITION_MS;

/// Every ordered pair of different weathers.
fn changes() -> impl Iterator<Item = (Weather, Weather)> {
    Weather::ALL
        .into_iter()
        .flat_map(|a| Weather::ALL.into_iter().map(move |b| (a, b)))
        .filter(|(a, b)| a != b)
}

/// Progress through a transition, `0..1`, in `SAMPLES` even steps.
fn progress() -> impl Iterator<Item = f32> {
    const SAMPLES: u32 = 1000;
    (0..SAMPLES).map(|k| k as f32 / SAMPLES as f32)
}

/// `element`'s `(start, end)` stage for `from → to`, read off the incoming
/// share: where it first leaves 0 and first reaches 1.
fn observed_stage(from: Weather, to: Weather, element: Element) -> (f32, f32) {
    let incoming = |p| WeatherMix::toward(from, to, p).incoming(element);
    let start = progress()
        .take_while(|&p| incoming(p) == 0.0)
        .last()
        .unwrap_or(0.0);
    let end = progress().find(|&p| incoming(p) >= 1.0).unwrap_or(1.0);
    (start, end)
}

/// Every slot opens on its own weather, pure, and so does every local `hh:00`
/// the committed media render at.
#[test]
fn every_slot_opens_on_its_own_pure_weather() {
    for slot in 0..10_000u64 {
        assert_eq!(
            clock_weather(at_ms(slot * WEATHER_CYCLE_MS)),
            WeatherMix::pure(slot_weather(slot)),
            "slot {slot}"
        );
    }
    for day in 0..40 {
        for h in 0..24 {
            let [from, to] = Sky::clock(on_day(day, h)).weather().ends();
            assert_eq!(from, to, "day {day} {h}:00");
        }
    }
}

/// A slot holds its weather until its last [`TRANSITION_MS`], then runs
/// toward the next slot's linearly in time, landing on its pure weather.
#[test]
fn a_slot_holds_then_runs_into_the_next_slots_weather() {
    const SLOTS: u64 = 500;
    const STRIDE_MS: usize = 997;
    let mut changed = 0;
    for slot in 0..SLOTS {
        let (here, next) = (slot_weather(slot), slot_weather(slot + 1));
        for into in (0..WEATHER_CYCLE_MS).step_by(STRIDE_MS) {
            let mix = WeatherPolicy::Clock.weather_at_ms(slot * WEATHER_CYCLE_MS + into);
            let want = match into.checked_sub(HOLD_MS) {
                None => WeatherMix::pure(here),
                Some(since) => WeatherMix::toward(here, next, since as f32 / TRANSITION_MS as f32),
            };
            assert_eq!(mix, want, "slot {slot} +{into}ms");
        }
        changed += usize::from(here != next);
    }
    assert!(changed > 0, "no slot changed weather in {SLOTS}");
}

/// Every element's share of the incoming weather starts at 0, never falls,
/// and reaches 1 by the transition's end, for every change.
#[test]
fn every_share_rises_from_nothing_to_whole() {
    for (from, to) in changes() {
        for element in Element::ALL {
            let incoming = |p| WeatherMix::toward(from, to, p).incoming(element);
            assert_eq!(incoming(0.0), 0.0, "{from:?} -> {to:?} {element:?}");
            assert!(
                incoming(1.0) == 1.0 && incoming(1.0 - 1e-6) > 1.0 - 1e-3,
                "{from:?} -> {to:?} {element:?}"
            );
            let mut prev = 0.0;
            for p in progress() {
                let now = incoming(p);
                assert!(now >= prev, "{from:?} -> {to:?} {element:?} fell at {p}");
                prev = now;
            }
        }
    }
}

/// Each element moves only inside its [`Course::stages`] stage, mirrored when
/// the course runs backward: still before it, whole after it.
#[test]
fn each_element_moves_only_inside_its_stage() {
    const EDGE: f32 = 2e-3;
    for (from, to) in changes() {
        let (course, backward) = Course::of(from, to);
        for element in Element::ALL {
            let (start, end) = course.stages().of(element);
            let (start, end) = if backward {
                (1.0 - end, 1.0 - start)
            } else {
                (start, end)
            };
            let (seen_start, seen_end) = observed_stage(from, to, element);
            assert!(
                (seen_start - start).abs() <= EDGE && (seen_end - end).abs() <= EDGE,
                "{from:?} -> {to:?} {element:?}: moved over {seen_start}..{seen_end}, \
                 not {start}..{end}"
            );
        }
    }
}

/// Rain gathers after the cloud and stops before the sky clears; a storm's
/// rain heavies before its lightning and its lightning stops before its rain
/// eases.
#[test]
fn the_elements_move_in_their_weathers_order() {
    let stage = |from, to, element| observed_stage(from, to, element);
    let before = |from, to, first, then| {
        let (_, first_end) = stage(from, to, first);
        let (then_start, _) = stage(from, to, then);
        assert!(
            first_end <= then_start,
            "{from:?} -> {to:?}: {first:?} ends at {first_end}, {then:?} starts at {then_start}"
        );
    };
    use Element::{Cloud, Lightning, Precipitation};
    use Weather::{Clear, Rain, Storm};
    before(Clear, Rain, Cloud, Precipitation);
    before(Rain, Clear, Precipitation, Cloud);
    before(Rain, Storm, Precipitation, Lightning);
    before(Storm, Rain, Lightning, Precipitation);
    let (rain_start, _) = stage(Clear, Storm, Precipitation);
    let (lightning_start, _) = stage(Clear, Storm, Lightning);
    assert!(
        rain_start < lightning_start,
        "a storm's lightning comes last"
    );
}

/// What falls settles [`FALL_SETTLE`] from either end of every change.
#[test]
fn precipitation_settles_clear_of_the_slot_boundary() {
    for (from, to) in changes() {
        let (start, end) = observed_stage(from, to, Element::Precipitation);
        assert!(
            start >= FALL_SETTLE - 1e-3 && end <= 1.0 - FALL_SETTLE + 1e-3,
            "{from:?} -> {to:?}: {start}..{end}"
        );
    }
}

/// Mid-change, each parameter the sky hands on is its two presets' lerp by
/// its element's share, and at either end one preset's.
#[test]
fn a_change_lerps_the_skys_parameters_between_the_presets() {
    type Handed = fn(&Sky) -> f32;
    type Preset = fn(Weather) -> f32;
    const SAMPLES: u64 = 24;
    let params: [(&str, Element, Handed, Preset); 5] = [
        (
            "direct",
            Element::Cloud,
            |s| s.transmission().direct,
            |w| transmission(w).direct,
        ),
        (
            "diffuse",
            Element::Cloud,
            |s| s.transmission().diffuse,
            |w| transmission(w).diffuse,
        ),
        (
            "disc",
            Element::Cloud,
            |s| s.transmission().disc,
            |w| transmission(w).disc,
        ),
        ("rain", Element::Precipitation, Sky::rain, rain_level),
        (
            "city bounce",
            Element::Cloud,
            |s| s.light().exterior - s.light().interior,
            city_bounce,
        ),
    ];
    let (here, next) = (Weather::Clear, Weather::Storm);
    let sky_in = |slot: u64, ms| Sky::clock(at_ms(slot * WEATHER_CYCLE_MS + ms));
    // At full night the glass's light is the room's plus the whole city bounce.
    let slot = (0..)
        .find(|&s| {
            let night = |ms| sky_in(s, ms).nightfall() == 1.0;
            slot_weather(s) == here
                && slot_weather(s + 1) == next
                && night(0)
                && night(WEATHER_CYCLE_MS)
        })
        .expect("a night slot a storm comes in on");
    let sky = |ms| sky_in(slot, ms);
    let close = |got: f32, want: f32| (got - want).abs() < 1e-6;
    for (name, element, of, preset) in params {
        assert!(
            close(of(&sky(0)), preset(here)),
            "{name} opens on {here:?}'s"
        );
        assert!(
            close(of(&sky(WEATHER_CYCLE_MS)), preset(next)),
            "{name} lands on {next:?}'s"
        );
        for k in 0..SAMPLES {
            let s = sky(HOLD_MS + k * TRANSITION_MS / SAMPLES);
            let p = s.weather().share(element, next);
            let want = preset(here) + (preset(next) - preset(here)) * p;
            assert!(
                close(of(&s), want),
                "{name} at sample {k}: {} vs {want}",
                of(&s)
            );
        }
    }
}

/// A storm coming in or going out fires its share of the strikes, and a
/// stronger storm keeps every strike a weaker one fired.
#[test]
fn a_changing_storm_fires_its_share_of_the_strikes() {
    const BUCKETS: u64 = 10_000;
    const SHARES: u16 = 20;
    let share = |k: u16| f32::from(k) / f32::from(SHARES);
    for k in [0, SHARES / 4, SHARES / 2, 3 * SHARES / 4, SHARES] {
        let fired = (0..BUCKETS).filter(|&b| strikes(b, share(k))).count();
        let rate = fired as f32 / BUCKETS as f32;
        assert!(
            (rate - share(k)).abs() < 0.03,
            "share {}: fired {rate}",
            share(k)
        );
    }
    for bucket in 0..BUCKETS {
        for k in 0..SHARES {
            assert!(
                !strikes(bucket, share(k)) || strikes(bucket, share(k + 1)),
                "a stronger storm keeps bucket {bucket}'s strike"
            );
        }
    }
}

/// Each bucket with its strike's first loop ms.
fn strike_starts() -> impl Iterator<Item = (u64, u64)> {
    (0..200_000u64).map(|bucket| (bucket, bucket * LIGHTNING_PERIOD_MS + strike_offset(bucket)))
}

/// The wall-clock instant a loop clock of `pace` reads `loop_ms` at.
fn wall_at(pace: u64, loop_ms: u64) -> SystemTime {
    std::time::UNIX_EPOCH + Duration::from_millis(loop_ms * pace)
}

/// On no moving tier does a strike run across a slot's start, so a storm's
/// strike never lights the next slot's weather.
#[test]
fn no_strike_runs_into_the_next_slot() {
    let slot = |at: SystemTime| crate::anim::epoch_ms(at) / WEATHER_CYCLE_MS;
    for motion in Motion::ALL {
        let Some(pace) = motion.pace() else {
            continue;
        };
        let wall = |loop_ms| wall_at(pace, loop_ms);
        for (bucket, start) in strike_starts() {
            let last = wall(start + STRIKE_MS) - Duration::from_millis(1);
            assert_eq!(
                slot(wall(start)),
                slot(last),
                "{motion:?} bucket {bucket}'s strike runs into the next slot"
            );
        }
    }
}

/// On every moving tier a strike fires by the storm's share at its start, read
/// on the wall clock its loop time plays at, and then runs whole. On Calm a
/// bucket spans several of a transition's steps, so a step can fall mid-strike.
#[test]
fn every_strike_fires_by_its_start_and_runs_whole_on_every_tier() {
    let phase_starts = || (0..STRIKE_MS).step_by(STRIKE_PHASE_MS as usize);
    let whole: Vec<f32> = phase_starts().map(lightning_envelope).collect();
    let mut cut_by_a_later_read = 0;
    for motion in Motion::ALL {
        let Some(pace) = motion.pace() else {
            continue;
        };
        let wall = |loop_ms| wall_at(pace, loop_ms);
        let storm =
            |loop_ms| clock_weather(wall(loop_ms)).share(Element::Lightning, Weather::Storm);
        for (bucket, start) in strike_starts() {
            let fires = strikes(bucket, storm(start));
            let levels: Vec<f32> = phase_starts()
                .map(|ms| flash_level_at(motion.beat(wall(start + ms)), WeatherPolicy::Clock))
                .collect();
            let want = if fires {
                whole.clone()
            } else {
                vec![0.0; whole.len()]
            };
            assert_eq!(levels, want, "{motion:?} bucket {bucket}");
            cut_by_a_later_read +=
                usize::from(fires != strikes(bucket, storm(start + STRIKE_MS - 1)));
        }
    }
    assert!(
        cut_by_a_later_read > 0,
        "no strike's eligibility changes mid-strike, so none tells a start read from a later one"
    );
}

/// The weather, transitions included, keeps the wall clock on every tier:
/// only the loops slow or rest.
#[test]
fn the_weather_keeps_real_time_on_every_tier() {
    use crate::anim::Motion;
    for ms in (0..24 * crate::anim::HOUR_MS).step_by(97_000) {
        let now = at_ms(1_700_000_000_000 + ms);
        let weather = |m: Motion| Sky::at(m.timing(now), WeatherPolicy::Clock).weather();
        assert_eq!(weather(Motion::Calm), weather(Motion::Full), "{ms}ms");
        assert_eq!(weather(Motion::Still), weather(Motion::Full), "{ms}ms");
    }
}

#[test]
fn rain_maps_audible_rain_under_its_policy() {
    let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10_000);
    let level = |w| Sky::at_with(t, w).rain();
    assert_eq!(level(Weather::Storm), 1.0, "storm is full rain");
    let rain = level(Weather::Rain);
    assert!(
        rain > 0.0 && rain < 1.0,
        "rain sits strictly between clear and storm, got {rain}"
    );
    for quiet in [
        Weather::Clear,
        Weather::Snow,
        Weather::Fog,
        Weather::Overcast,
        Weather::Windy,
        Weather::Smog,
    ] {
        assert_eq!(level(quiet), 0.0, "{quiet:?} must be silent");
    }
}

/// The music changes family where the visual night is half in: the moon,
/// city glow and stars ride `nightfall`, and lofi composes for the night
/// below [`Controls::DUSK`](pixtuoid_lofi::Controls::DUSK).
#[test]
fn the_musics_night_family_starts_at_the_nightfalls_midpoint() {
    const MINUTE_H: f32 = 1.0 / 60.0;
    let dusk = pixtuoid_lofi::Controls::DUSK;
    let office = crate::tally::StateCounts::default();
    let at =
        |h: f32| crate::audio::music_controls(&office, nightfall(h.rem_euclid(24.0)), 0.0).daylight;
    for mid in [
        SUN_SET_H + NIGHTFALL_H / 2.0,
        SUN_RISE_H - NIGHTFALL_H / 2.0,
    ] {
        let (before, after) = (at(mid - MINUTE_H), at(mid + MINUTE_H));
        let (day_side, night_side) = if mid > SUN_SET_H {
            (before, after)
        } else {
            (after, before)
        };
        assert!(day_side >= dusk, "{mid}h's day side reads {day_side}");
        assert!(night_side < dusk, "{mid}h's night side reads {night_side}");
    }
}

/// From any start, a full storm on a moving tier has a strike to find, and the
/// instant found flashes; at rest it has none.
#[test]
fn a_strike_follows_every_start_on_a_moving_tier() {
    use crate::anim::Motion;
    let base = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for motion in Motion::ALL {
        for start in (0..LIGHTNING_PERIOD_MS * 3).step_by(97) {
            let after = base + Duration::from_millis(start);
            let strike = first_strike_after(after, motion);
            if motion.pace().is_none() {
                assert_eq!(strike, None, "{motion:?}");
            } else {
                let storm = WeatherPolicy::Forced(Weather::Storm);
                assert!(
                    strike
                        .is_some_and(|t| t >= after && flash_level_at(motion.beat(t), storm) > 0.0),
                    "{motion:?} from {start} ms"
                );
            }
        }
    }
}

/// The transition found starts at or after where the search did, opens on
/// a pure weather, and is halfway into a different one at its middle.
#[test]
fn the_transition_found_changes_the_weather() {
    let base = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let ms = |t: std::time::SystemTime| {
        t.duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_millis() as u64
    };
    for k in 0..40u64 {
        let after = base + Duration::from_millis(k * 97_003);
        let start = first_transition_after(after).expect("a transition");
        assert!(start >= after, "{k}");
        let before = WeatherPolicy::Clock.weather_at_ms(ms(start) - 1);
        assert_eq!(before.from, before.to, "{k}: the slot ends pure");
        let mid = WeatherPolicy::Clock.weather_at_ms(ms(start) + TRANSITION_MS / 2);
        assert_ne!(mid.from, mid.to, "{k}: the weather changes");
        let storm = first_transition_into(after, Weather::Storm).expect("a storm comes");
        assert!(storm >= start, "{k}");
        let into = WeatherPolicy::Clock.weather_at_ms(ms(storm) + TRANSITION_MS / 2);
        assert_eq!(into.to, Weather::Storm, "{k}");
    }
}

/// Nightfall begins at the instant `nightfall_on` names: none a minute
/// before, some a minute after.
#[test]
fn nightfall_begins_when_nightfall_on_says() {
    let day = crate::localclock::at_hour(9);
    let start = nightfall_on(day).expect("a local evening");
    let minute = Duration::from_secs(60);
    assert_eq!(nightfall(local_hour_frac(start - minute)), 0.0);
    assert!(nightfall(local_hour_frac(start + minute)) > 0.0);
}
