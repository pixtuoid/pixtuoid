use super::*;

#[test]
fn theme_switch_recolors_floor() {
    let scene = scene_with(vec![idle("/t/0.jsonl", 0, t0())], 16);
    let mut r = build(100, 40, vec![]);
    let now = t0();
    r.render(&scene, now).unwrap();
    let before = r.buf().expect("a frame").clone();
    r.set_theme(dark_theme());
    r.render(&scene, now).unwrap();
    let d = region_diff(
        &before,
        r.buf().expect("a frame"),
        0,
        0,
        before.width(),
        before.height(),
    );
    assert!(
        d > 5_000,
        "switching to a different theme must recolor the floor (diff={d})"
    );
}

/// A typing sitter's glow is the one theme input to its cached sprite
/// ([`FrameKey::glow_tint`](pixtuoid_scene::frame_cache::FrameKey)), so a
/// renderer that rendered under the old theme paints the new one
/// pixel-for-pixel as a fresh one would, with no flush on a switch.
#[test]
fn theme_switch_paints_as_a_fresh_renderer_would() {
    let started = t0() - Duration::from_secs(300);
    let scene = scene_with(vec![running("/t/fresh.jsonl", 0, "Edit x", started)], 16);
    let now = t0();
    let mut switched = build(100, 40, vec![]);
    switched.render(&scene, now).unwrap();
    switched.set_theme(dark_theme());
    switched.render(&scene, now).unwrap();
    let mut fresh = build(100, 40, vec![]);
    fresh.set_theme(dark_theme());
    fresh.render(&scene, now).unwrap();
    assert_eq!(
        switched.buf().expect("a frame").as_slice(),
        fresh.buf().expect("a frame").as_slice(),
        "a stale cached sprite survived the theme switch"
    );
}

/// Asserts the lit state, not the frame: the level dims only the room's
/// artificial lights, under 1% of the frame mean.
#[test]
fn occupied_floor_stays_lit() {
    use pixtuoid_scene::floor::VacancyDim;
    let scene = scene_with(vec![running("/lit/0.jsonl", 0, "Edit x", t0())], 16);
    let mut r = build(100, 40, vec![]);
    for now in [
        t0(),
        t0() + Duration::from_millis(VacancyDim::EMPTY_DEBOUNCE_MS),
    ] {
        r.render(&scene, now).unwrap();
    }
    assert_eq!(
        r.session.floor(0).expect("a floor").ctx.vacancy_dim.level(),
        1.0
    );
}

/// Each phase of a strike stays on screen at least the photosensitive floor
/// in the half-blocks, at each frame grid: from the flush that first shows it
/// to the one that replaces it. A strike lifts the whole room, so a flush that
/// changes most of the scene's cells is a change of phase.
#[test]
fn each_strike_phase_holds_the_floor_on_screen_in_half_blocks() {
    use crate::test_flash::{assert_each_phase_holds_the_floor, frame_grid, lead, storm_strike};
    let strike = storm_strike();
    let tick = crate::tui::frame_tick();
    for (frame, offset) in frame_grid(tick) {
        let (cols, rows) = crate::tui::renderer::min_terminal_size();
        let (mut r, screen, _) = half_blocks_on_screen(cols, rows);
        r.set_weather(strike.weather);
        r.set_motion(pixtuoid_scene::anim::Motion::Full);
        let scene = scene_with(vec![idle("/s/0.jsonl", 0, t0())], 16);
        let now = strike.start - lead(frame) + offset;
        screen.at(now);
        r.render(&scene, now).expect("render");
        let cells = crate::tui::renderer::scene_rect(r.frame_buffer().area);
        let mut shown = r.frame_buffer().clone();
        let mut changed = Vec::new();
        for now in crate::test_flash::frames_after(now, frame, strike.end + lead(frame)) {
            screen.at(now);
            r.render(&scene, now).expect("render");
            let differ = cells
                .positions()
                .filter(|&p| r.frame_buffer()[p] != shown[p])
                .count();
            if 2 * differ > cells.area() as usize {
                changed.push(now);
            }
            shown = r.frame_buffer().clone();
        }
        let at = format!("a frame each {frame:?} from +{offset:?}");
        assert_each_phase_holds_the_floor(&changed, strike.changes.len(), &at);
    }
}

/// A frame whose strike phase would replace one shown under the floor is not
/// flushed, so the terminal and its hit targets stay the last frame's, alone
/// or sliding; the frame the floor later is.
#[test]
fn a_held_frame_leaves_the_terminal_and_its_hit_targets_alone() {
    use crate::test_flash::{held_frames, storm_strike};
    let strike = storm_strike();
    let [dark, late, held, shown] = held_frames(&strike);
    for sliding in [false, true] {
        let (cols, rows) = crate::tui::renderer::min_terminal_size();
        let (mut r, screen, _) = half_blocks_on_screen(cols, rows);
        r.set_weather(strike.weather);
        r.set_motion(pixtuoid_scene::anim::Motion::Full);
        let scene = two_floor_scene();
        if sliding {
            r.navigate_floor(1, dark);
        }
        for at in [dark, late] {
            screen.at(at);
            r.render(&scene, at).expect("render");
        }
        let before = r.frame_buffer().clone();
        let layout = r.cached_layout().map(|l| l as *const SceneLayout);
        screen.at(held);
        r.render(&scene, held).expect("render");
        assert_eq!(*r.frame_buffer(), before, "sliding {sliding}: held");
        assert_eq!(r.cached_layout().map(|l| l as *const SceneLayout), layout);
        screen.at(shown);
        r.render(&scene, shown).expect("render");
        assert_ne!(*r.frame_buffer(), before, "sliding {sliding}: shown");
        assert_eq!(r.transition().is_some(), sliding);
    }
}

/// A terminal resized under a hold gets its frame at once, alone or sliding:
/// a screen of a new shape shows nothing to hold.
#[test]
fn a_resized_terminal_is_never_held_in_half_blocks() {
    use crate::test_flash::{held_frames, storm_strike};
    let strike = storm_strike();
    let [dark, late, held, _] = held_frames(&strike);
    for sliding in [false, true] {
        let (cols, rows) = crate::tui::renderer::min_terminal_size();
        let (mut r, screen, _) = half_blocks_on_screen(cols, rows);
        r.set_weather(strike.weather);
        r.set_motion(pixtuoid_scene::anim::Motion::Full);
        let scene = two_floor_scene();
        if sliding {
            r.navigate_floor(1, dark);
        }
        for at in [dark, late] {
            screen.at(at);
            r.render(&scene, at).expect("render");
        }
        let resized = ratatui::layout::Rect::new(0, 0, cols + 8, rows + 4);
        r.terminal
            .backend_mut()
            .resize(resized.width, resized.height);
        let mut stale = r.frame_buffer().clone();
        stale.resize(resized);
        screen.at(held);
        r.render(&scene, held).expect("render");
        assert_eq!(r.frame_buffer().area, resized);
        assert_ne!(*r.frame_buffer(), stale, "sliding {sliding}: drawn");
    }
}

/// A phase holds the floor from when its flush lands, not from when its frame
/// began: after a slow flush, the next phase waits for the floor to pass on
/// the screen clock, though its frame's own clock says it has.
#[test]
fn a_slow_flushs_phase_holds_the_floor_from_when_it_lands() {
    use crate::test_flash::{SLOW, storm_strike};
    let floor = Duration::from_millis(pixtuoid_scene::anim::PHOTOSENSITIVE_PHASE_MIN_MS);
    let strike = storm_strike();
    let [first, second] = [strike.changes[0], strike.changes[1]];
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let (mut r, screen, wire) = half_blocks_on_screen(cols, rows);
    r.set_weather(strike.weather);
    r.set_motion(pixtuoid_scene::anim::Motion::Full);
    let scene = scene_with(vec![idle("/s/0.jsonl", 0, t0())], 16);
    screen.at(first - 2 * floor);
    r.render(&scene, first - 2 * floor).expect("render");
    *wire.slow.lock().expect("lock") = Some(Latency {
        screen: screen.clone(),
        by: SLOW,
    });
    screen.at(first);
    r.render(&scene, first).expect("render");
    let landed = screen.now();
    *wire.slow.lock().expect("lock") = None;
    let before = r.frame_buffer().clone();
    assert!(
        second.duration_since(first).expect("in order") >= floor,
        "the frame clock says the floor has passed"
    );
    screen.at(second);
    r.render(&scene, second).expect("render");
    assert_eq!(
        *r.frame_buffer(),
        before,
        "held until the slow flush's phase shows the floor"
    );
    let shown = std::time::UNIX_EPOCH + landed + floor;
    screen.at(shown);
    r.render(&scene, shown).expect("render");
    assert_ne!(*r.frame_buffer(), before, "shown once it has");
}

/// A slide cancelled before the next frame (a resize lands it) still reports
/// what its last frame flashes, which its pixels still show: a starved tube's
/// catch on the floor it leaves.
#[test]
fn a_cancelled_slide_reports_what_its_last_frame_flashes() {
    use crate::test_flash::starved_stutter;
    let stutter = starved_stutter();
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let (mut r, screen, _) = half_blocks_on_screen(cols, rows);
    r.set_weather(stutter.weather);
    r.set_motion(pixtuoid_scene::anim::Motion::Full);
    let cap = 16;
    let upstairs = slot(AgentId::from_transcript_path("/c/1.jsonl"), 1, cap, t0());
    let scene = scene_with(vec![upstairs], cap);
    let catch = stutter.changes[0];
    // A tick before: a starved tube catches only across a step no longer than
    // its shortest catch.
    let before = catch - Duration::from_millis(pixtuoid_scene::anim::FULL_TICK_MS);
    for at in stutter.setup.into_iter().chain([before]) {
        screen.at(at);
        r.render(&scene, at).expect("render");
    }
    r.navigate_floor(1, before);
    screen.at(catch);
    r.render(&scene, catch).expect("render");
    let flashes = |r: &TuiRenderer<_>| r.session.picture().map(|p| p.flashes()).unwrap_or_default();
    let caught = flashes(&r);
    assert_ne!(
        caught,
        pixtuoid_scene::screen::Flashes::default(),
        "the slide's frame catches"
    );
    r.cancel_transition();
    assert_eq!(flashes(&r), caught);
}

/// A pause freezes the frame clock inside a hold; the screen clock runs on,
/// so the hold runs out and the paused frame, with a theme change, is flushed.
#[test]
fn a_pause_inside_a_hold_never_wedges_the_half_blocks() {
    use crate::test_flash::{held_frames, storm_strike};
    let floor = Duration::from_millis(pixtuoid_scene::anim::PHOTOSENSITIVE_PHASE_MIN_MS);
    let strike = storm_strike();
    let [dark, late, paused, _] = held_frames(&strike);
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let (mut r, screen, _) = half_blocks_on_screen(cols, rows);
    r.set_weather(strike.weather);
    r.set_motion(pixtuoid_scene::anim::Motion::Full);
    let scene = scene_with(vec![idle("/s/0.jsonl", 0, t0())], 16);
    for at in [dark, late] {
        screen.at(at);
        r.render(&scene, at).expect("render");
    }
    let before = r.frame_buffer().clone();
    screen.at(paused);
    r.render(&scene, paused).expect("render");
    assert_eq!(*r.frame_buffer(), before, "held");
    r.set_theme(dark_theme());
    screen.at(late);
    screen.advance(floor);
    r.render(&scene, paused).expect("render");
    assert_ne!(
        *r.frame_buffer(),
        before,
        "the paused frame was flushed once the hold ran out"
    );
}

/// A starved neon's every catch, and every dark between, stays on screen at
/// least the photosensitive floor in the half-blocks, at each frame grid. The
/// cells by the tube's west side change only with it.
#[test]
fn each_stutter_phase_holds_the_floor_on_screen_in_half_blocks() {
    use crate::test_flash::{
        assert_each_phase_holds_the_floor, frame_grid, lead, neon_tube, starved_stutter,
    };
    let stutter = starved_stutter();
    let tick = crate::tui::frame_tick();
    let scene = scene_with(vec![], 16);
    for (frame, offset) in frame_grid(tick) {
        let (cols, rows) = crate::tui::renderer::min_terminal_size();
        let (mut r, screen, _) = half_blocks_on_screen(cols, rows);
        r.set_weather(stutter.weather);
        r.set_motion(pixtuoid_scene::anim::Motion::Full);
        for at in stutter.setup {
            screen.at(at);
            r.render(&scene, at).expect("render");
        }
        let cells = crate::tui::renderer::scene_rect(r.frame_buffer().area);
        let tube: Vec<ratatui::layout::Position> = cells
            .positions()
            .filter(|p| neon_tube(p.x - cells.x, 2 * (p.y - cells.y)))
            .collect();
        let snapshot = |r: &TuiRenderer<TestBackend>| -> Vec<ratatui::buffer::Cell> {
            tube.iter().map(|&p| r.frame_buffer()[p].clone()).collect()
        };
        let now = stutter.start - lead(frame) + offset;
        screen.at(now);
        r.render(&scene, now).expect("render");
        let mut shown = snapshot(&r);
        let mut changed = Vec::new();
        for now in crate::test_flash::frames_after(now, frame, stutter.end + lead(frame)) {
            screen.at(now);
            r.render(&scene, now).expect("render");
            let shot = snapshot(&r);
            if shot != shown {
                changed.push(now);
            }
            shown = shot;
        }
        let at = format!("a frame each {frame:?} from +{offset:?}");
        assert_each_phase_holds_the_floor(&changed, stutter.changes.len(), &at);
    }
}

#[test]
fn theme_picker_renders_theme_names() {
    let scene = scene_with(vec![idle("/tp/0.jsonl", 0, t0())], 16);
    let mut r = build(140, 48, vec![]);
    r.set_frames(
        RenderFrames {
            theme_picker: Some(0),
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("cyberpunk") || text.contains("normal"),
        "the theme picker lists theme names"
    );
}

#[test]
fn version_popup_paints_when_open() {
    let scene = scene_with(vec![idle("/vp/0.jsonl", 0, t0())], 16);
    let mut r = build(140, 48, vec![]);
    r.render(&scene, t0()).unwrap();
    let baseline = r.buf().expect("a frame").clone();
    // Render past the 200ms entrance animation so the popup is at full scale.
    r.set_frames(
        RenderFrames {
            version_popup: crate::panels::ui_state::VersionPopupFrame {
                open: true,
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    let t1 = t0() + Duration::from_millis(250);
    r.render(&scene, t1).unwrap();
    assert!(
        r.last_popup_scale() > 0.9,
        "popup should be near full scale"
    );
    let d = region_diff(
        &baseline,
        r.buf().expect("a frame"),
        0,
        0,
        baseline.width(),
        baseline.height(),
    );
    assert!(
        d > 1000,
        "an open version popup must paint over the scene (diff={d})"
    );
}
