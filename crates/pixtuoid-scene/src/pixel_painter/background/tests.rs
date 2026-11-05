use super::*;
use crate::anim::Motion;
use crate::layout::{WINDOW_W, window_bays, window_run};
use crate::lighting::SPILL_DEPTH;
use crate::pack::test_office;
use crate::sky::{Weather, hour_is_day};
use std::time::SystemTime;

/// A wall `w` wide with its band `top_wall_h` tall, and no door.
fn bare_wall(w: u16, top_wall_h: u16) -> crate::outside::Wall {
    crate::outside::Wall {
        span: 0..w,
        band_h: top_wall_h,
        bays: window_bays(0..w, 0..0).collect(),
    }
}

/// The ground, the wall band `top_wall_h` tall and every window a wall as wide
/// as `buf` shows, at `moment`.
fn paint_band(buf: &mut RgbBuffer, top_wall_h: u16, moment: &Moment, theme: &crate::theme::Theme) {
    let wall = bare_wall(buf.width(), top_wall_h);
    paint_ground_and_walls(&mut BaseFillCache::new(), buf, &wall, moment, theme);
    paint_windows(
        buf,
        wall,
        moment,
        &test_office(),
        theme,
        &mut crate::outside::OutsideCache::default(),
    );
}

#[test]
fn lightning_flash_lights_the_room_mid_strike_only() {
    let now = SystemTime::UNIX_EPOCH;
    let mk = || {
        RgbBuffer::filled(
            8,
            4,
            Rgb {
                r: 10,
                g: 10,
                b: 12,
            },
        )
    };
    let quiet_fill = Rgb {
        r: 10,
        g: 10,
        b: 12,
    };

    let mut b = mk();
    paint_lightning_flash(
        &mut b,
        &Sky::at_with(now, Weather::Storm).with_strike(Some(crate::sky::StrikePhase::Primary)),
    );
    assert!(b.get(0, 0).r > 10, "storm strike should brighten the room");

    let mut b = mk();
    paint_lightning_flash(&mut b, &Sky::at_with(now, Weather::Storm).with_strike(None));
    assert_eq!(b.get(0, 0), quiet_fill, "no flash between strikes");
}
#[test]
fn storm_window_bolt_brightens_glass_during_the_flash() {
    let now = SystemTime::UNIX_EPOCH;
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (buf_w, top_wall_h) = (60, 30);
    let render_lum = |strike| -> u64 {
        let sky = Sky::at_with(now, Weather::Storm).with_strike(strike);
        let mut buf = RgbBuffer::filled(buf_w, 40, Rgb { r: 8, g: 8, b: 10 });
        paint_band(
            &mut buf,
            top_wall_h,
            &Moment::resolve(sky, theme, 0.0, Motion::Full.timing(now)),
            theme,
        );
        window_rows(top_wall_h)
            .flat_map(|y| (0..buf_w).map(move |x| (x, y)))
            .map(|(x, y)| {
                let p = buf.get(x, y);
                u64::from(p.r) + u64::from(p.g) + u64::from(p.b)
            })
            .sum()
    };
    let flashing = render_lum(Some(crate::sky::StrikePhase::Primary));
    let quiet = render_lum(None);
    assert!(
        flashing > quiet,
        "the on-glass bolt must brighten the storm glass during the flash \
         (flash={flashing}, quiet={quiet})"
    );
}

/// Mid-change, the carpet and the glass's veil each take the incoming
/// weather's look on the pixels the dither gives it, keyed on the buffer, and
/// the going weather's on the rest.
#[test]
fn a_change_dithers_the_carpet_and_the_veil_on_the_buffer() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let now = crate::localclock::at_hour(12);
    // Partway through a change of cloud, at a share whose dither, unlike
    // half's checkerboard, moves with any shift of its key.
    let moment = || {
        let mix = crate::sky::WeatherMix::toward(Weather::Clear, Weather::Fog, 0.4);
        let sky = Sky::at_with(now, Weather::Clear).with_weather(mix);
        Moment::resolve(sky, theme, 0.0, Motion::Full.timing(now))
    };
    let black = Rgb { r: 0, g: 0, b: 0 };
    let (buf_w, buf_h, top_wall_h) = (64u16, 48u16, 18u16);
    // Every pixel `paint` draws under `dither` is its incoming end's where the
    // dither takes it, else its going end's; and each end shows somewhere.
    let check =
        |what: &str, takes: &dyn Fn(u16, u16) -> bool, [going, coming, mid]: [RgbBuffer; 3]| {
            let mut took = [0; 2];
            for y in 0..mid.height() {
                for x in 0..mid.width() {
                    if going.get(x, y) == coming.get(x, y) {
                        continue;
                    }
                    let t = takes(x, y);
                    let want = if t { coming.get(x, y) } else { going.get(x, y) };
                    assert_eq!(mid.get(x, y), want, "{what} at ({x}, {y})");
                    took[usize::from(t)] += 1;
                }
            }
            assert!(took[0] > 0 && took[1] > 0, "{what}: {took:?}");
        };

    let tint = moment().look.ground_tint_color();
    let carpet = |tint: Dithered<Rgb>| {
        let mut m = moment();
        m.look = m.look.with_ground_tint_color(tint);
        let mut buf = RgbBuffer::filled(buf_w, buf_h, black);
        paint_ground_and_walls(
            &mut BaseFillCache::new(),
            &mut buf,
            &bare_wall(buf_w, top_wall_h),
            &m,
            theme,
        );
        buf
    };
    let [from, to] = tint.ends();
    check(
        "carpet",
        &|x, y| tint.takes_to(x, y),
        [Dithered::solid(from), Dithered::solid(to), tint].map(carpet),
    );

    let veil = moment().look.glass_veil;
    assert!(
        window_bays(0..buf_w, 0..0)
            .any(|b| b.glass_box(window_rows(top_wall_h)).x % crate::dither::PERIOD != 0),
        "every glass on the dither's phase: a key from its corner would pass"
    );
    let window = |veil| {
        let mut m = moment();
        m.look.glass_veil = veil;
        let mut buf = RgbBuffer::filled(buf_w, buf_h, black);
        paint_band(&mut buf, top_wall_h, &m, theme);
        buf
    };
    let [clear, fog] = veil.ends();
    check(
        "veil",
        &|x, y| veil.takes_to(x, y),
        [Dithered::solid(clear), Dithered::solid(fog), veil].map(window),
    );
}

#[test]
fn short_buffer_clamps_spill_and_window_without_panic() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let top_wall_h = 18u16;
    // buf_h sits just above top_wall_h so the window glass and the spill
    // ([`SPILL_DEPTH`] rows below the wall band) both straddle the bottom edge.
    let buf_h = top_wall_h + 2;
    let buf_w = 60u16;
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(12 * 3600);
    let mut buf = RgbBuffer::filled(buf_w, buf_h, Rgb { r: 5, g: 5, b: 5 });
    paint_band(
        &mut buf,
        top_wall_h,
        &Moment::resolve(Sky::clock(now), theme, 0.0, Motion::Full.timing(now)),
        theme,
    );
    let spill = crate::lighting::Emitter {
        light: crate::lighting::Light::Spill {
            x: 0,
            w: WINDOW_W,
            top: top_wall_h,
            slant: 0.0,
        },
        ..spill(0, 0.0)
    };
    paint_light(&mut buf, &spill, theme.lighting.sun_spill);
    // Reaching here without a panic IS the primary assertion — `RgbBuffer::put`
    // has no bounds guard.
    assert_ne!(
        buf.get(0, 0),
        Rgb { r: 5, g: 5, b: 5 },
        "the wall band should still paint in the in-bounds rows"
    );
    assert_ne!(
        buf.get(1, buf_h - 1),
        Rgb { r: 5, g: 5, b: 5 },
        "the spill should still paint its in-bounds rows"
    );
}

/// Render a full office wall through [`paint_band`] at a
/// forced January `day` + local `hour` + weather.
fn render_office_on(
    day: u32,
    hour: u32,
    weather: Weather,
    buf_w: u16,
    top_wall_h: u16,
) -> RgbBuffer {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    render_office_themed(day, hour, weather, theme, buf_w, top_wall_h)
}

/// [`render_office_on`] with the theme as a parameter — the weather/light
/// invariants hold per THEME (each ships its own night-sky + glass colours), so
/// their pins sweep `ALL_THEMES` rather than trusting `normal` to be worst-case.
fn render_office_themed(
    day: u32,
    hour: u32,
    weather: Weather,
    theme: &'static crate::theme::Theme,
    buf_w: u16,
    top_wall_h: u16,
) -> RgbBuffer {
    let now = crate::localclock::on_day(day, hour);
    let buf_h = top_wall_h + 4;
    let mut buf = RgbBuffer::filled(buf_w, buf_h, Rgb { r: 4, g: 4, b: 6 });
    paint_band(
        &mut buf,
        top_wall_h,
        &Moment::resolve(
            // The weather's steady light: a strike landing on `now` is the
            // instant's luck, and its flash is the strike tests'.
            Sky::at_with(now, weather).with_strike(None),
            theme,
            0.0,
            Motion::Full.timing(now),
        ),
        theme,
    );
    buf
}

/// `render_office_on` pinned to January 1st, for the hour/weather-only tests.
fn render_office_at(hour: u32, weather: Weather, buf_w: u16, top_wall_h: u16) -> RgbBuffer {
    render_office_on(1, hour, weather, buf_w, top_wall_h)
}

/// Count "warm bright" pixels (the sun disc's signature) in the sky-only top
/// third of the window band. Restricted to the top third so it can never pick up
/// the SKYLINE's own lit city-window dots, which live in the glass's bottom half
/// regardless of time of day and would false-positive as a "disc".
fn count_warm_bright(buf: &RgbBuffer, top_wall_h: u16) -> usize {
    (1..(top_wall_h / 3).max(2))
        .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let p = buf.get(x, y);
            p.r > 200 && p.r > p.b.saturating_add(40)
        })
        .count()
}

/// Count "cool bright" pixels (the moon disc's signature) in the same sky-only
/// region. `moon_core` sits closer to neutral white than the warm `sun_core`, so
/// the blue-over-red margin is smaller than `count_warm_bright`'s — still well
/// clear of the base night-sky gradient, whose blue never approaches 200.
fn count_cool_bright(buf: &RgbBuffer, top_wall_h: u16) -> usize {
    (1..(top_wall_h / 3).max(2))
        .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let p = buf.get(x, y);
            p.b > 200 && p.b > p.r.saturating_add(10)
        })
        .count()
}

/// Count STAR pixels in the same sky-only top-third band: bright cells with
/// no bright neighbour. Only a `STAR_COLOR` blend lifts a lone cell this
/// bright; a moonlit cloud's edge is bright too, but never alone.
fn count_stars(buf: &RgbBuffer, top_wall_h: u16) -> usize {
    let bright = |x: u16, y: u16| {
        let p = buf.get(x, y);
        p.r > 90 && p.g > 90 && p.b > 90
    };
    (1..(top_wall_h / 3).max(2))
        .flat_map(|y| (1..buf.width() - 1).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            bright(x, y)
                && !bright(x - 1, y)
                && !bright(x + 1, y)
                && !bright(x, y - 1)
                && !bright(x, y + 1)
        })
        .count()
}

#[test]
fn disc_appears_low_in_the_sky_at_a_low_sun_hour() {
    // 07:00: the sun sits low, under the HORIZON_FRAC/ARC_RISE_FRAC clip
    // threshold, so its disc lands inside the glass rather than off the top.
    let buf_w = 96u16;
    let top_wall_h = 40u16;
    let clear = render_office_at(7, Weather::Clear, buf_w, top_wall_h);
    let overcast = render_office_at(7, Weather::Overcast, buf_w, top_wall_h);
    let clear_n = count_warm_bright(&clear, top_wall_h);
    let overcast_n = count_warm_bright(&overcast, top_wall_h);
    assert!(
        clear_n >= 3,
        "a warm disc should show at a low clear sun hour, got {clear_n} bright px"
    );
    assert!(
        clear_n > overcast_n,
        "overcast (atmo disc visibility below MIN_DISC_VIS) should hide the \
         disc clear shows: clear={clear_n} overcast={overcast_n}"
    );
}

#[test]
fn rain_hides_the_disc_like_overcast() {
    let buf_w = 96u16;
    let top_wall_h = 40u16;
    let clear = render_office_at(7, Weather::Clear, buf_w, top_wall_h);
    let rain = render_office_at(7, Weather::Rain, buf_w, top_wall_h);
    let overcast = render_office_at(7, Weather::Overcast, buf_w, top_wall_h);
    let clear_n = count_warm_bright(&clear, top_wall_h);
    let rain_n = count_warm_bright(&rain, top_wall_h);
    let overcast_n = count_warm_bright(&overcast, top_wall_h);
    assert!(
        clear_n >= 3,
        "clear should show a disc at a low sun hour, got {clear_n}"
    );
    assert_eq!(
        rain_n, 0,
        "rain should hide the disc entirely, like overcast, got {rain_n}"
    );
    assert_eq!(
        overcast_n, 0,
        "overcast should hide the disc entirely, got {overcast_n}"
    );
}

#[test]
fn thick_cloud_hides_the_disc_uniformly() {
    let min_disc_vis = crate::celestial::MIN_DISC_VIS;
    let overcast = crate::sky::transmission(Weather::Overcast).disc;
    let rain = crate::sky::transmission(Weather::Rain).disc;
    let storm = crate::sky::transmission(Weather::Storm).disc;
    assert!(
        overcast >= rain && rain >= storm,
        "disc visibility must not increase as cloud thickens: \
         overcast={overcast} rain={rain} storm={storm}"
    );
    assert!(
        overcast < min_disc_vis && rain < min_disc_vis && storm < min_disc_vis,
        "overcast/rain/storm should all hide the disc (below MIN_DISC_VIS={min_disc_vis}): \
         overcast={overcast} rain={rain} storm={storm}"
    );
}

#[test]
fn disc_clips_above_the_glass_at_the_arc_apex() {
    // `top_wall_h` is CONSTANT across both renders so the only difference is the
    // sun's altitude: at the apex `Disc::of`'s `cy` bracket goes negative
    // whatever the wall height, so the apex ALWAYS clips by construction.
    let buf_w = 96u16;
    let top_wall_h = 40u16;
    let low = render_office_at(7, Weather::Clear, buf_w, top_wall_h);
    let apex = render_office_at(12, Weather::Clear, buf_w, top_wall_h);
    let low_n = count_warm_bright(&low, top_wall_h);
    let apex_n = count_warm_bright(&apex, top_wall_h);
    assert!(low_n >= 3, "low sun should show a disc: {low_n}");
    assert_eq!(
        apex_n, 0,
        "the apex disc must clip entirely above the glass: {apex_n}"
    );
}

#[test]
fn short_window_apex_does_not_panic() {
    // top_wall_h=10 leaves a short window while the apex disc's `cy` is solidly
    // negative.
    let _ = render_office_at(12, Weather::Clear, 96, 10);
}

#[test]
fn disc_lands_in_a_window_never_on_the_wall_margin() {
    // The disc can legitimately hide behind an inter-window pillar at some hours,
    // so it is NOT visible at every hour — hence the two-part sweep: it must
    // appear inside a real window at least once, and NEVER paint past the last
    // painted window (the wall margin, which is the bug this guards).
    let top_wall_h = 40u16;
    for buf_w in [76u16, 96, 120, 150, 192, 220, 300] {
        let last_right = window_run(0..buf_w).end;
        let mut seen_in_a_window = false;
        for h in [5u32, 6, 7, 17, 18, 19] {
            let buf = render_office_at(h, Weather::Clear, buf_w, top_wall_h);
            for y in 1..(top_wall_h / 3).max(2) {
                for x in 0..buf.width() {
                    let p = buf.get(x, y);
                    if p.r > 240 && i16::from(p.r) - i16::from(p.b) > 40 {
                        assert!(
                            x < last_right,
                            "buf_w={buf_w} h={h}: disc pixel at x={x} is past the \
                             last window (wall margin; last right edge {last_right})"
                        );
                        seen_in_a_window = true;
                    }
                }
            }
        }
        assert!(
            seen_in_a_window,
            "buf_w={buf_w}: the disc never appeared in a window across the low-sun sweep"
        );
    }
}

#[test]
fn disc_sweeps_across_a_single_window_buffer() {
    // buf_w=64 paints EXACTLY one window (too narrow for a second pane) — the
    // degenerate case where a center-to-center azimuth mapping has zero span and
    // freezes `cx` on the mullion.
    let buf_w = 64u16;
    let top_wall_h = 40u16;
    let morning = render_office_at(7, Weather::Clear, buf_w, top_wall_h);
    let evening = render_office_at(18, Weather::Clear, buf_w, top_wall_h);
    let warm_center_x = |buf: &RgbBuffer| -> f32 {
        let mut sum = 0u32;
        let mut count = 0u32;
        for y in 1..(top_wall_h / 3).max(2) {
            for x in 0..buf.width() {
                let p = buf.get(x, y);
                if p.r > 200 && p.r > p.b.saturating_add(40) {
                    sum += u32::from(x);
                    count += 1;
                }
            }
        }
        assert!(count > 0, "expected a warm disc to render in this buffer");
        sum as f32 / count as f32
    };
    let morning_x = warm_center_x(&morning);
    let evening_x = warm_center_x(&evening);
    assert!(
        (morning_x - evening_x).abs() > 1.0,
        "the disc must sweep across a single-window buffer, not freeze on \
         the mullion: morning_x={morning_x} evening_x={evening_x}"
    );
}

#[test]
fn moon_disc_shows_at_night() {
    let buf_w = 96u16;
    let top_wall_h = 40u16;
    let (day, hour) = (1..=31u32)
        .filter(|&d| Sky::clock(crate::localclock::on_day(d, 0)).moon_phase() > 0.9)
        .find_map(|d| low_moon(d, buf_w, top_wall_h).map(|(h, _)| (d, h)))
        .expect("a near-full moon shows low some January night");
    let clear = render_office_on(day, hour, Weather::Clear, buf_w, top_wall_h);
    let overcast = render_office_on(day, hour, Weather::Overcast, buf_w, top_wall_h);
    let clear_n = count_cool_bright(&clear, top_wall_h);
    let overcast_n = count_cool_bright(&overcast, top_wall_h);
    assert!(
        clear_n >= 3,
        "a cool moon disc should show at a clear night hour, got {clear_n} bright px"
    );
    assert!(
        clear_n > overcast_n,
        "overcast should hide the moon disc clear shows: \
         clear={clear_n} overcast={overcast_n}"
    );
}

#[test]
fn stars_appear_on_a_clear_night_and_vanish_under_overcast() {
    // A moonless small hour, and one under a moon high enough to clip above the
    // glass: either way the bright things in the upper sky band are stars.
    let buf_w = 96u16;
    let top_wall_h = 40u16;
    let night = |want: fn(f32) -> bool| {
        (1..=31u32)
            .flat_map(|d| [0, 1, 2, 3, 22, 23].map(|h| (d, h)))
            .find(|&(d, h)| {
                let sky = Sky::at_with(crate::localclock::on_day(d, h), Weather::Clear);
                sky.nightfall() >= 1.0 && want(sky.body().altitude)
            })
    };
    let moonless = night(|alt| alt <= 0.0).expect("a moonless deep-night hour in January");
    let high_moon = night(|alt| alt > 0.95).expect("a high-moon deep-night hour in January");
    for (day, hour) in [moonless, high_moon] {
        let clear = render_office_on(day, hour, Weather::Clear, buf_w, top_wall_h);
        let overcast = render_office_on(day, hour, Weather::Overcast, buf_w, top_wall_h);
        let clear_n = count_stars(&clear, top_wall_h);
        let overcast_n = count_stars(&overcast, top_wall_h);
        assert!(
            clear_n >= 3,
            "day {day} {hour}:00: a clear night should show some stars, got {clear_n}"
        );
        assert!(
            clear_n > overcast_n,
            "day {day} {hour}:00: overcast should hide the stars a clear sky shows: \
             clear={clear_n} overcast={overcast_n}"
        );
    }
}

#[test]
fn disc_never_bleeds_across_a_window_pillar() {
    // A disc whose `cx` lands near an inter-window gap is wide enough (radius +
    // glow) to reach the glass on BOTH sides of the solid wall pillar — the
    // sun/moon showing THROUGH a wall. A wide buffer has many internal gaps;
    // sweeping the low-sun hours makes `cx` pass over one.
    let buf_w = 280u16;
    let top_wall_h = 40u16;
    let bays: Vec<_> = window_bays(0..buf_w, 0..0).collect();
    for h in [5u32, 6, 7, 17, 18, 19] {
        let buf = render_office_at(h, Weather::Clear, buf_w, top_wall_h);
        let mut wins = std::collections::HashSet::new();
        // Top third only, so the skyline's lit city dots can't pose as disc pixels.
        for y in 1..(top_wall_h / 3).max(2) {
            for x in 0..buf.width() {
                let p = buf.get(x, y);
                if !(p.r > 240 && i16::from(p.r) - i16::from(p.b) > 40) {
                    continue;
                }
                if let Some(b) = bays.iter().find(|b| b.span().contains(&x)) {
                    wins.insert(b.idx);
                }
            }
        }
        assert!(
            wins.len() <= 1,
            "at {h}:00 the disc lit {} windows {:?} — it bled across a wall pillar",
            wins.len(),
            wins
        );
    }
}

/// The first whole hour of `day`'s night its moon stands low in the glass,
/// fully faded in under a clear sky, with that disc: low, since a high moon
/// clips above the glass like a midday sun. Every pixel inside it is then
/// EXACTLY `moon_core` or EXACTLY `MOON_SHADOW`.
fn low_moon(day: u32, buf_w: u16, top_wall_h: u16) -> Option<(u32, crate::celestial::Disc)> {
    (0..24u32).find_map(|h| {
        let sky = Sky::at_with(crate::localclock::on_day(day, h), Weather::Clear);
        let e = sky.body();
        let low = e.kind == crate::sky::BodyKind::Moon && (0.2..0.5).contains(&e.altitude);
        if !low || sky.nightfall() < 1.0 {
            return None;
        }
        crate::celestial::Disc::of(&sky, 0..buf_w, top_wall_h).map(|d| (h, d))
    })
}

#[test]
fn crescent_moon_leaves_the_dark_limb_unlit() {
    let buf_w = 96u16;
    let top_wall_h = 40u16;
    let shown = |pick: fn(f32) -> bool| {
        (1..=31u32)
            .filter(|&d| pick(Sky::clock(crate::localclock::on_day(d, 0)).moon_phase()))
            .find_map(|d| low_moon(d, buf_w, top_wall_h).map(|(h, geom)| (d, h, geom)))
    };
    let crescent = shown(|p| p < 0.35).expect("a crescent shows low some January night");
    let full = shown(|p| p > 0.9).expect("a near-full moon shows low some January night");

    let count_dark_and_bright =
        |(day, hour, geom): (u32, u32, crate::celestial::Disc)| -> (usize, usize) {
            let buf = render_office_on(day, hour, Weather::Clear, buf_w, top_wall_h);
            let r = geom.r.ceil() as i32;
            let (cx, cy) = (geom.cx.round() as i32, geom.cy.round() as i32);
            let mut dark = 0usize;
            let mut bright = 0usize;
            for py in (cy - r)..=(cy + r) {
                for px in (cx - r)..=(cx + r) {
                    if px < 0 || py < 0 || px as u16 >= buf.width() || py as u16 >= buf.height() {
                        continue;
                    }
                    let dx = px as f32 - geom.cx;
                    let dy = py as f32 - geom.cy;
                    if dx * dx + dy * dy > geom.r * geom.r {
                        continue; // outside the disc proper
                    }
                    let p = buf.get(px as u16, py as u16);
                    if p == crate::celestial::MOON_SHADOW {
                        dark += 1;
                    } else if p.b > 200 && p.b > p.r.saturating_add(10) {
                        bright += 1;
                    }
                }
            }
            (dark, bright)
        };

    let (crescent_dark, crescent_bright) = count_dark_and_bright(crescent);
    let (full_dark, full_bright) = count_dark_and_bright(full);

    assert!(
        crescent_bright >= 2,
        "the crescent should still show a lit sliver, got {crescent_bright}"
    );
    assert!(
        crescent_dark >= 2,
        "the crescent should leave a dark limb unlit, got {crescent_dark}"
    );
    assert!(
        full_bright >= 2,
        "a near-full moon should be lit, got {full_bright}"
    );
    assert!(
        crescent_dark > full_dark,
        "a crescent should have strictly MORE dark-within-disc pixels than \
         a near-full moon: crescent={crescent_dark} full={full_dark}"
    );
    assert!(
        crescent_dark >= full_dark + 10,
        "assert a real margin, not a hair's-breadth win: \
         crescent={crescent_dark} full={full_dark}"
    );
}

/// A waxing moon is lit on its right limb and a waning one on its left, as a
/// northern-hemisphere sky shows them: 2026's first quarter falls on Jan 26 and
/// its last on Jan 10 ([`Sky::moon_waxing`]'s test cites the table).
#[test]
fn a_waning_moon_lights_its_left_limb() {
    let buf_w = 96u16;
    let top_wall_h = 40u16;
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    // Lit disc pixels left and right of the disc's centre column.
    let lit_sides = |day: u32| -> (usize, usize) {
        let (hour, geom) = low_moon(day, buf_w, top_wall_h).expect("the quarter moon shows low");
        let buf = render_office_on(day, hour, Weather::Clear, buf_w, top_wall_h);
        let r = geom.r.ceil() as i32;
        let (cx, cy) = (geom.cx.round() as i32, geom.cy.round() as i32);
        let (mut left, mut right) = (0usize, 0usize);
        for py in (cy - r)..=(cy + r) {
            for px in (cx - r)..=(cx + r) {
                let dx = px as f32 - geom.cx;
                let dy = py as f32 - geom.cy;
                if dx * dx + dy * dy > geom.r * geom.r || px < 0 || py < 0 {
                    continue;
                }
                if buf.get(px as u16, py as u16) == theme.lighting.moon_core {
                    if dx < 0.0 {
                        left += 1;
                    } else if dx > 0.0 {
                        right += 1;
                    }
                }
            }
        }
        (left, right)
    };
    // `on_day` counts from Jan 1, so day 9 is Jan 10 and day 25 is Jan 26.
    let (waning_left, waning_right) = lit_sides(9);
    assert!(
        waning_left > waning_right,
        "last quarter lights the left limb: left={waning_left} right={waning_right}"
    );
    let (waxing_left, waxing_right) = lit_sides(25);
    assert!(
        waxing_right > waxing_left,
        "first quarter lights the right limb: left={waxing_left} right={waxing_right}"
    );
}

#[test]
fn moon_glow_dims_at_new_moon() {
    let buf_w = 96u16;
    let top_wall_h = 40u16;
    let (mut new_moon_day, mut new_moon_frac) = (1u32, f32::MAX);
    let (mut full_moon_day, mut full_moon_frac) = (1u32, f32::MIN);
    for day in 1..=31u32 {
        let frac = Sky::clock(crate::localclock::on_day(day, 21)).moon_phase();
        if frac < new_moon_frac {
            new_moon_frac = frac;
            new_moon_day = day;
        }
        if frac > full_moon_frac {
            full_moon_frac = frac;
            full_moon_day = day;
        }
    }

    // A softer bar than `count_cool_bright`'s core threshold, so it catches the
    // halo blend rather than requiring a fully-opaque core hit.
    let count_glow_ring = |buf: &RgbBuffer| -> usize {
        (1..(top_wall_h / 3).max(2))
            .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let p = buf.get(x, y);
                p.b > 90 && p.b > p.r.saturating_add(5)
            })
            .count()
    };

    let new_moon_buf = render_office_on(new_moon_day, 21, Weather::Clear, buf_w, top_wall_h);
    let full_moon_buf = render_office_on(full_moon_day, 21, Weather::Clear, buf_w, top_wall_h);
    let new_moon_glow = count_glow_ring(&new_moon_buf);
    let full_moon_glow = count_glow_ring(&full_moon_buf);
    assert!(
        new_moon_glow < full_moon_glow,
        "a new moon's glow ring (phase={new_moon_frac}) should show fewer/dimmer \
         cool pixels than a full moon's (phase={full_moon_frac}): \
         new={new_moon_glow} full={full_moon_glow}"
    );
}

/// Mean channel value over every PAINTED window pane's glass interior. The
/// day-over-night invariant is asserted on THIS, not on
/// [`SkyTones::darkness`]: the weather veils are painted onto the glass
/// AFTER the light model resolved the sky, so a `darkness`-only assertion is
/// structurally blind to them.
fn glass_mean_luminance(buf: &RgbBuffer, top_wall_h: u16) -> f32 {
    let rows = window_rows(top_wall_h);
    let mut sum = 0.0f64;
    let mut n = 0u32;
    for w in window_bays(0..buf.width(), 0..0) {
        for y in (rows.start + 1)..rows.end.saturating_sub(1) {
            for x in (w.x + 1)..w.span().end.saturating_sub(1) {
                if x < buf.width() && y < buf.height() {
                    let p = buf.get(x, y);
                    sum += f64::from(p.r) + f64::from(p.g) + f64::from(p.b);
                    n += 3;
                }
            }
        }
    }
    assert!(n > 0, "the rig must sample real glass");
    (sum / f64::from(n)) as f32
}

/// The brightest night January 2026 can offer.
fn fullest_moon_day() -> u32 {
    (1..=31u32)
        .max_by(|&a, &b| {
            let phase = |d: u32| Sky::clock(crate::localclock::on_day(d, 0)).moon_phase();
            phase(a)
                .partial_cmp(&phase(b))
                .expect("moon_phase is never NaN")
        })
        .expect("January has days")
}

/// The pane-luminance rig every weather/time invariant below shares: the glass
/// mean for one theme × hour × weather, at the year's brightest night.
fn pane(theme: &'static crate::theme::Theme, hour: u32, w: Weather) -> f32 {
    const BUF_W: u16 = 120;
    const TOP_WALL_H: u16 = 26;
    glass_mean_luminance(
        &render_office_themed(fullest_moon_day(), hour, w, theme, BUF_W, TOP_WALL_H),
        TOP_WALL_H,
    )
}

/// Local midnight. It is NOT the brightest RENDERED
/// night pane (the pre-dawn twilight tint reads brighter on every theme), which
/// is why the two ordering pins below sweep [`night_hours`] instead of sampling
/// this one.
const NIGHT_HOUR: u32 = 0;
/// Solar noon — the daytime reference the pane orderings measure against.
const NOON_HOUR: u32 = 12;

/// Every whole hour at which the sky shows the MOON, straight off
/// [`hour_is_day`] — the ONE day/night boundary, so this sweep can't drift
/// from a second hand-written hour list.
fn night_hours() -> impl Iterator<Item = u32> {
    (0..24u32).filter(|h| !hour_is_day(*h as f32))
}

/// The most of its OWN solar-noon brightness a pane may still show at any night
/// hour. A bare `night < noon` has NO teeth against the veil defect: an
/// absolute-grey veil leaves each weather's night pane just barely under its own
/// noon pane, so the ordering holds while the rendered day/night cycle has
/// collapsed to a few percent. This floor sits in the gap between the veiled and
/// unveiled weather populations.
const MAX_NIGHT_PANE_FRACTION: f32 = 0.75;

// The day/night CONTRAST the light model produces has to survive the weather
// VEIL the painter lays over the glass afterwards — the veils were absolute
// daylight-grey constants with no time input, so a night-lit room sat behind
// daylight-white windows.
#[test]
fn no_weather_flattens_the_glass_day_night_contrast() {
    for theme in crate::theme::ALL_THEMES {
        for w in Weather::ALL {
            let noon = pane(theme, NOON_HOUR, w);
            for hour in night_hours() {
                let night = pane(theme, hour, w);
                assert!(
                    night <= noon * MAX_NIGHT_PANE_FRACTION,
                    "{}/{:?}: the {hour:02}:00 pane must stay under {MAX_NIGHT_PANE_FRACTION} \
                     of its own solar-noon pane (night={night:.1} noon={noon:.1} ratio={:.3})",
                    theme.name,
                    w,
                    night / noon
                );
            }
        }
    }
}

// The cross-weather half of the same defect. The reference is CLEAR noon, not
// the dimmest noon: how bright a snowy/stormy noon pane renders is a
// theme-palette choice, so "the dimmest noon of any weather" is not a property
// of the light model and is deliberately not asserted.
#[test]
fn no_night_pane_outshines_the_clear_solar_noon_pane() {
    for theme in crate::theme::ALL_THEMES {
        let clear_noon = pane(theme, NOON_HOUR, Weather::Clear);
        for w in Weather::ALL {
            for hour in night_hours() {
                let night = pane(theme, hour, w);
                assert!(
                    night < clear_noon,
                    "{}/{:?}: the {hour:02}:00 pane ({night:.1}) must stay below the \
                     clear solar-noon pane ({clear_noon:.1})",
                    theme.name,
                    w
                );
            }
        }
    }
}

// The counter-pin: night-adapting the veil must not ERASE it. A veil scaled to
// zero at night, or deleted outright, passes the two tests above and fails this.
#[test]
fn fog_still_glows_over_the_midnight_sky() {
    const FOG_NIGHT_GLOW_MIN: f32 = 1.25;
    for theme in crate::theme::ALL_THEMES {
        let clear = pane(theme, NIGHT_HOUR, Weather::Clear);
        let fog = pane(theme, NIGHT_HOUR, Weather::Fog);
        assert!(
            fog > clear * FOG_NIGHT_GLOW_MIN,
            "{}: fog must still read as a lit murk at midnight (fog={fog:.1} \
             vs clear={clear:.1})",
            theme.name
        );
        let smog = pane(theme, NIGHT_HOUR, Weather::Smog);
        assert!(
            smog > clear,
            "{}: smog must still veil the midnight sky (smog={smog:.1} vs clear={clear:.1})",
            theme.name
        );
    }
}

#[test]
fn base_fill_cache_hit_is_byte_identical_and_a_key_change_repaints() {
    let normal = crate::theme::theme_by_name("normal").expect("normal theme");
    let other = crate::theme::ALL_THEMES
        .iter()
        .find(|t| {
            t.surface.carpet_base != normal.surface.carpet_base
                || t.surface.wall != normal.surface.wall
        })
        .copied()
        .expect("a theme with a different carpet/wall exists");
    let now = crate::localclock::on_day(1, 12);
    let (buf_w, buf_h, top_wall_h) = (96u16, 64u16, 14u16);
    let paint = |base_fill: &mut BaseFillCache, theme: &'static crate::theme::Theme, weather| {
        let mut buf = RgbBuffer::filled(buf_w, buf_h, Rgb { r: 9, g: 9, b: 9 });
        paint_ground_and_walls(
            base_fill,
            &mut buf,
            &bare_wall(buf_w, top_wall_h),
            &Moment::resolve(
                Sky::at_with(now, weather),
                theme,
                0.0,
                Motion::Full.timing(now),
            ),
            theme,
        );
        buf
    };
    let mut shared = BaseFillCache::new();
    let first = paint(&mut shared, normal, Weather::Clear);
    let hit = paint(&mut shared, normal, Weather::Clear);
    assert_eq!(
        first.as_slice(),
        hit.as_slice(),
        "a cache HIT must be byte-identical to the fill it memoized"
    );
    let switched = paint(&mut shared, other, Weather::Clear);
    let fresh = paint(&mut BaseFillCache::new(), other, Weather::Clear);
    assert_eq!(
        switched.as_slice(),
        fresh.as_slice(),
        "a theme swap on a warm cache must repaint, not serve the stale fill"
    );
    let back = paint(&mut shared, normal, Weather::Clear);
    assert_eq!(
        first.as_slice(),
        back.as_slice(),
        "swapping back must re-derive the original fill"
    );

    // Weather leg: the tint changes the CARPET colours while the wall stays
    // put — the one key component nothing else covers.
    let clear = paint(&mut shared, normal, Weather::Clear);
    let rain_shared = paint(&mut shared, normal, Weather::Rain);
    let rain_fresh = paint(&mut BaseFillCache::new(), normal, Weather::Rain);
    assert_eq!(
        rain_shared.as_slice(),
        rain_fresh.as_slice(),
        "a weather-tint change on a warm cache must repaint the carpet, not serve the stale fill"
    );
    assert_ne!(
        clear.as_slice(),
        rain_shared.as_slice(),
        "clear vs rain must differ somewhere in the carpet (else this leg pins nothing)"
    );
}

#[test]
fn base_fill_cache_resize_on_a_warm_cache_recomputes() {
    let theme = crate::theme::theme_by_name("normal").expect("normal theme");
    let now = crate::localclock::on_day(1, 12);
    let paint_at = |base_fill: &mut BaseFillCache, w: u16, h: u16| {
        let mut buf = RgbBuffer::filled(w, h, Rgb { r: 9, g: 9, b: 9 });
        paint_ground_and_walls(
            base_fill,
            &mut buf,
            &bare_wall(w, 14),
            &Moment::resolve(Sky::clock(now), theme, 0.0, Motion::Full.timing(now)),
            theme,
        );
        buf
    };
    // The memo is single-slot, so each leg varies ONE key component against
    // the immediately preceding call — a co-varying sibling would mask the
    // component under test (a width-only change must repaint even when the
    // height alone would have missed the memo anyway).
    let mut warm = BaseFillCache::new();
    let big = paint_at(&mut warm, 96, 64);
    let narrow_shared = paint_at(&mut warm, 80, 64);
    let narrow_fresh = paint_at(&mut BaseFillCache::new(), 80, 64);
    assert_eq!(
        narrow_shared.as_slice(),
        narrow_fresh.as_slice(),
        "a width-only resize on a warm cache must recompute the fill, not serve (or panic on) the old size"
    );
    let short_shared = paint_at(&mut warm, 80, 48);
    let short_fresh = paint_at(&mut BaseFillCache::new(), 80, 48);
    assert_eq!(
        short_shared.as_slice(),
        short_fresh.as_slice(),
        "a height-only resize on a warm cache must recompute the fill"
    );
    let big_again = paint_at(&mut warm, 96, 64);
    assert_eq!(
        big.as_slice(),
        big_again.as_slice(),
        "growing back must re-derive the original fill"
    );
}

#[test]
fn lightning_flash_matches_the_per_pixel_blend_reference() {
    let sky = Sky::at_with(SystemTime::UNIX_EPOCH, Weather::Storm)
        .with_strike(Some(crate::sky::StrikePhase::Primary));
    let mut lcg = 0xC0FFEEu32;
    let mut next = || {
        lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
        Rgb {
            r: (lcg >> 24) as u8,
            g: (lcg >> 16) as u8,
            b: (lcg >> 8) as u8,
        }
    };
    let (w, h) = (37u16, 9u16);
    let mut buf = RgbBuffer::filled(w, h, Rgb { r: 0, g: 0, b: 0 });
    for y in 0..h {
        for x in 0..w {
            buf.put(x, y, next());
        }
    }
    let mut expected = buf.clone();
    let alpha = 0.20 * sky.flash();
    for y in 0..h {
        for x in 0..w {
            let c = expected.get(x, y);
            expected.put(
                x,
                y,
                blend_rgb(
                    c,
                    Rgb {
                        r: 255,
                        g: 255,
                        b: 255,
                    },
                    alpha,
                ),
            );
        }
    }
    paint_lightning_flash(&mut buf, &sky);
    for y in 0..h {
        for x in 0..w {
            assert_eq!(
                buf.get(x, y),
                expected.get(x, y),
                "({x},{y}) diverged from the per-pixel blend reference"
            );
        }
    }
}

/// The spill leans away from the sun. The disc (`Disc::of`) and the spill
/// (`Light::Spill`) each map the one azimuth to a side on their own, so this is
/// the only check that sees them disagree, read off the pixels each paints.
#[test]
fn the_spill_leans_away_from_the_disc() {
    const BUF_W: u16 = 192;
    const BUF_H: u16 = 80;
    const TOP_WALL_H: u16 = 30;
    const FILL: Rgb = Rgb {
        r: 20,
        g: 20,
        b: 24,
    };
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let mid = f32::from(BUF_W) / 2.0;
    // The mean x of the pixels a paint changed in rows `ys`, or `None` if none.
    let lit_x = |buf: &RgbBuffer, ys: std::ops::Range<u16>| {
        let xs: Vec<f32> = ys
            .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| buf.get(x, y) != FILL)
            .map(|(x, _)| f32::from(x))
            .collect();
        (!xs.is_empty()).then(|| xs.iter().sum::<f32>() / xs.len() as f32)
    };
    for hour in [6, 19] {
        let at = crate::localclock::at_hour(hour);
        let moment = crate::atmosphere::Moment::resolve(
            Sky::at_with(at, Weather::Clear),
            theme,
            0.0,
            Motion::Full.timing(at),
        );
        let (sky, look) = (&moment.sky, &moment.look);
        let disc = crate::celestial::Disc::of(sky, 0..BUF_W, TOP_WALL_H).expect("a clear low sun");
        let disc_side = (disc.cx - mid).signum();

        // One centred window, so the lean can run either way unclipped.
        let mut buf = RgbBuffer::filled(BUF_W, BUF_H, FILL);
        let window_x = (BUF_W - WINDOW_W) / 2;
        paint_light(
            &mut buf,
            &spill(window_x, look.spill_slant),
            theme.lighting.sun_spill,
        );
        // Halves, not single rows: the dither leaves a faint row with no lit cell.
        let half = SPILL_DEPTH / 2;
        let top = lit_x(&buf, 0..half).expect("the spill's upper half");
        let bottom = lit_x(&buf, half..SPILL_DEPTH).expect("the spill's lower half");
        assert_eq!(
            (bottom - top).signum(),
            -disc_side,
            "spill lean vs disc at {hour}:00"
        );
    }
}

/// A spill leaning off the canvas's left edge lights only the columns that land
/// on the canvas: the same clip the right edge applies at the buffer's width.
#[test]
fn a_spill_leaning_off_the_left_edge_is_clipped() {
    const FILL: Rgb = Rgb {
        r: 20,
        g: 20,
        b: 24,
    };
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let window_x = 1u16;
    let buf_w = window_x + WINDOW_W + SPILL_DEPTH;
    let mut buf = RgbBuffer::filled(buf_w, SPILL_DEPTH, FILL);
    // One column left per row: every row past the first leans off the edge.
    paint_light(&mut buf, &spill(window_x, -1.0), theme.lighting.sun_spill);
    for dy in 0..SPILL_DEPTH {
        let widen = i32::from((dy / 2).min(3));
        let left = i32::from(window_x) - i32::from(dy) - widen;
        let right = i32::from(window_x) - i32::from(dy) + i32::from(WINDOW_W) + widen;
        let lit: Vec<u16> = (0..buf_w).filter(|&x| buf.get(x, dy) != FILL).collect();
        let want: Vec<u16> = (0..buf_w)
            .filter(|&x| (left..right).contains(&i32::from(x)))
            .collect();
        // The sill row is at the spill's peak, so every cell of it lights; the
        // rows below dither out, but never past the clipped span.
        if dy == 0 {
            assert_eq!(lit, want, "row {dy}");
        } else {
            assert!(
                lit.iter().all(|x| want.contains(x)),
                "row {dy}: {lit:?} ⊄ {want:?}"
            );
        }
    }
}

/// The sun's spill below a window at `x`, from row 0, leaning `slant`.
fn spill(x: u16, slant: f32) -> crate::lighting::Emitter {
    crate::lighting::Emitter {
        kind: crate::lighting::EmitterKind::WindowSpill,
        light: crate::lighting::Light::Spill {
            x,
            w: WINDOW_W,
            top: 0,
            slant,
        },
        strength: 0.32,
    }
}

#[test]
fn the_wall_between_two_windows_is_one_frame_post() {
    let theme = crate::theme::theme_by_name("normal").expect("normal theme");
    let now = crate::localclock::on_day(1, 12);
    let (buf_w, buf_h, top_wall_h) = (160u16, 96u16, 24u16);
    let mut buf = RgbBuffer::filled(buf_w, buf_h, Rgb { r: 9, g: 9, b: 9 });
    paint_ground_and_walls(
        &mut BaseFillCache::new(),
        &mut buf,
        &bare_wall(buf_w, top_wall_h),
        &Moment::resolve(Sky::clock(now), theme, 0.0, Motion::Full.timing(now)),
        theme,
    );
    let mut posts = 0;
    for post in crate::layout::window_posts(0..buf_w) {
        posts += 1;
        for x in post.clone() {
            for y in window_rows(top_wall_h) {
                assert_eq!(
                    buf.get(x, y),
                    theme.surface.window_frame,
                    "post {post:?} at ({x}, {y})"
                );
            }
        }
    }
    assert!(posts > 0, "this wall has posts");
}

#[test]
fn the_classic_lays_the_carpet_the_model_tints() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (top_wall_h, buf_w, buf_h) = (18u16, 60u16, 60u16);
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(12 * 3600);
    let moment = Moment::resolve(
        Sky::at_with(now, crate::sky::Weather::Rain),
        theme,
        0.0,
        Motion::Full.timing(now),
    );
    let mut buf = RgbBuffer::filled(buf_w, buf_h, Rgb { r: 5, g: 5, b: 5 });
    paint_band(&mut buf, top_wall_h, &moment, theme);
    let carpet = moment
        .look
        .carpet(theme)
        .as_solid()
        .expect("a settled weather");
    let tones = [carpet.lit, carpet.base, carpet.dark];
    let floor_row = buf_h - 1;
    assert!(
        (0..buf_w).all(|x| tones.contains(&buf.get(x, floor_row))),
        "every carpet pixel is one of the model's three tones"
    );
}
