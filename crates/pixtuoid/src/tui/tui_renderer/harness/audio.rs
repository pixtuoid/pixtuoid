//! Floor-scoped audio wiring, pinned through the PRODUCTION render path: the
//! renderer must feed the audio thread ONLY the floor being viewed.

use super::*;
use crate::audio::{AudioHandle, drain_frames};

fn running_on(path: &str, floor_idx: usize, desk: usize) -> AgentSlot {
    let mut s = slot(AgentId::from_transcript_path(path), floor_idx, desk, t0());
    s.state = ActivityState::Running {
        tool_use_id: Some(Arc::from("t")),
        detail: Some(Arc::from("Edit")),
        kind: ToolKind::from_display("Edit"),
    };
    s
}

#[test]
fn audio_stems_and_music_count_only_the_viewed_floor() {
    // 1 running on floor 0 vs 3 on floor 1: the tiers differ exactly there, so a
    // global count (4) reads BUSY where the viewed floor reads MODERATE.
    let cap = 16;
    let scene = scene_with(
        vec![
            running_on("/a/f0.jsonl", 0, 0),
            running_on("/a/f1a.jsonl", 1, cap),
            running_on("/a/f1b.jsonl", 1, cap + 1),
            running_on("/a/f1c.jsonl", 1, cap + 2),
        ],
        cap,
    );
    let mut r = build(80, 40, vec![]);
    let (handle, rx) = AudioHandle::test_pair();
    r.set_audio(handle);
    r.render(&scene, t0()).expect("render");
    let frames = drain_frames(&rx);
    assert!(!frames.is_empty(), "an enabled handle receives frames");
    let frame = frames.last().unwrap();
    let viewed = pixtuoid_scene::tally::StateCounts {
        running: 1,
        requires_action: 0,
        idle: 0,
        exiting: 0,
        total: 1,
    };
    let moderate = pixtuoid_scene::audio::stem_levels(&viewed, 0.0);
    assert_eq!(
        frame.stems.typing, moderate.typing,
        "typing level must reflect the VIEWED floor's 1 running, not all 4"
    );
    assert_eq!(
        frame.music.intensity,
        pixtuoid_scene::audio::music_controls(&viewed, 0.0, 0.0).intensity,
        "the music's intensity must reflect the VIEWED floor's 1 running, not all 4"
    );
}

#[test]
fn door_chime_fires_only_for_viewed_floor_arrivals() {
    let cap = 16;
    let mut agents = vec![running_on("/d/f0.jsonl", 0, 0)];
    let scene = scene_with(agents.clone(), cap);
    let mut r = build(80, 40, vec![]);
    let (handle, rx) = AudioHandle::test_pair();
    r.set_audio(handle);
    let mut now = t0();
    r.render(&scene, now).expect("prime render");
    drain_frames(&rx);

    agents.push(running_on("/d/f1-new.jsonl", 1, cap));
    let scene = scene_with(agents.clone(), cap);
    now += std::time::Duration::from_millis(PAINT_FRAME_MS);
    r.render(&scene, now).expect("render");
    let off_floor: Vec<_> = drain_frames(&rx)
        .into_iter()
        .flat_map(|f| f.events)
        .collect();
    assert!(
        off_floor.is_empty(),
        "a floor-1 walk-in must not chime while viewing floor 0: {off_floor:?}"
    );

    agents.push(running_on("/d/f0-new.jsonl", 0, 1));
    let scene = scene_with(agents, cap);
    now += std::time::Duration::from_millis(PAINT_FRAME_MS);
    r.render(&scene, now).expect("render");
    let on_floor: Vec<_> = drain_frames(&rx)
        .into_iter()
        .flat_map(|f| f.events)
        .collect();
    assert!(
        on_floor.contains(&pixtuoid_scene::audio::OneShot::DoorChime),
        "a floor-0 walk-in must chime while viewing floor 0: {on_floor:?}"
    );
}

#[test]
fn floor_switch_reprimes_without_a_chime_volley() {
    let cap = 16;
    let scene = scene_with(
        vec![
            running_on("/s/f0.jsonl", 0, 0),
            running_on("/s/f1a.jsonl", 1, cap),
            running_on("/s/f1b.jsonl", 1, cap + 1),
        ],
        cap,
    );
    let mut r = build(80, 40, vec![]);
    let (handle, rx) = AudioHandle::test_pair();
    r.set_audio(handle);
    let mut now = t0();
    r.render(&scene, now).expect("prime on floor 0");
    drain_frames(&rx);

    r.navigate_floor(1, now);
    render_until_settled(&mut r, &scene, &mut now, 1);
    let after_switch: Vec<_> = drain_frames(&rx)
        .into_iter()
        .flat_map(|f| f.events)
        .collect();
    assert!(
        after_switch.is_empty(),
        "arriving on floor 1 must not chime its existing agents: {after_switch:?}"
    );

    let scene = scene_with(
        vec![
            running_on("/s/f0.jsonl", 0, 0),
            running_on("/s/f1a.jsonl", 1, cap),
            running_on("/s/f1b.jsonl", 1, cap + 1),
            running_on("/s/f1-new.jsonl", 1, cap + 2),
        ],
        cap,
    );
    now += std::time::Duration::from_millis(PAINT_FRAME_MS);
    r.render(&scene, now).expect("render");
    let arrivals: Vec<_> = drain_frames(&rx)
        .into_iter()
        .flat_map(|f| f.events)
        .collect();
    assert!(
        arrivals.contains(&pixtuoid_scene::audio::OneShot::DoorChime),
        "a real floor-1 arrival chimes after the re-prime: {arrivals:?}"
    );
}

/// A floor slide's frames feed the audio thread too, one a frame, as
/// `AudioObserver::frame`'s every-world-frame contract asks.
#[test]
fn every_frame_of_a_slide_feeds_the_audio_thread() {
    let cap = 16;
    let scene = scene_with(
        vec![
            running_on("/l/f0.jsonl", 0, 0),
            running_on("/l/f1.jsonl", 1, cap),
        ],
        cap,
    );
    let mut r = build(80, 40, vec![]);
    let (handle, rx) = AudioHandle::test_pair();
    r.set_audio(handle);
    let mut now = t0();
    r.render(&scene, now).expect("prime render");
    drain_frames(&rx);
    r.navigate_floor(1, now);
    let mut frames = 0;
    while r.transition().is_some() {
        r.render(&scene, now).expect("render");
        frames += 1;
        now += Duration::from_millis(PAINT_FRAME_MS);
    }
    assert!(frames > 1, "the slide ran over frames");
    assert_eq!(drain_frames(&rx).len(), frames);
}

#[test]
fn footer_note_glyph_tracks_effective_audibility() {
    let scene = scene_with(vec![running_on("/f/a.jsonl", 0, 0)], 16);

    let mut silent = build(80, 40, vec![]);
    silent.render(&scene, t0()).expect("render");
    assert!(
        !frame_text(silent.frame_buffer()).contains('\u{2669}'),
        "a disabled handle shows no note glyph"
    );

    let mut r = build(80, 40, vec![]);
    let (handle, _rx) = AudioHandle::test_pair();
    r.set_audio(handle.clone());
    r.render(&scene, t0()).expect("render");
    assert!(
        frame_text(r.frame_buffer()).contains('\u{2669}'),
        "enabled + unmuted shows ♩ in the footer"
    );

    handle.set_muted(true);
    r.render(&scene, t0()).expect("render");
    assert!(
        !frame_text(r.frame_buffer()).contains('\u{2669}'),
        "muting hides the glyph"
    );

    handle.set_muted(false);
    handle.set_volume(0.0);
    r.render(&scene, t0()).expect("render");
    assert!(
        !frame_text(r.frame_buffer()).contains('\u{2669}'),
        "0% volume shows no note glyph"
    );
    handle.set_volume(0.4);
    r.render(&scene, t0()).expect("render");
    assert!(
        frame_text(r.frame_buffer()).contains('\u{2669}'),
        "restoring volume restores the glyph"
    );
}

/// A forced weather reaches every consumer the renderer feeds: the viewed
/// frame, both halves of a floor slide, and the audio frame's rain and music.
#[test]
fn a_forced_weather_reaches_the_frame_both_slide_halves_and_the_rain() {
    use pixtuoid_scene::sky::{Weather, WeatherPolicy};
    let cap = 16;
    let scene = scene_with(
        vec![
            running_on("/w/f0.jsonl", 0, 0),
            running_on("/w/f1.jsonl", 1, cap),
        ],
        cap,
    );
    let under = |w| {
        let mut r = build(80, 40, vec![]);
        let (handle, rx) = AudioHandle::test_pair();
        r.set_audio(handle);
        r.set_weather(WeatherPolicy::Forced(w));
        r.render(&scene, t0()).expect("render");
        let audio = drain_frames(&rx).pop().expect("a frame");
        let rain = (audio.stems.rain, audio.music.rain);
        let still = r.buf().expect("a frame").as_slice().to_vec();
        r.navigate_floor(1, t0());
        r.render(&scene, t0() + Duration::from_millis(PAINT_FRAME_MS))
            .expect("render");
        assert!(r.transition().is_some(), "{w:?}: mid-slide");
        let halves = [0, 1].map(|f| {
            r.session
                .floor(f)
                .expect("a floor")
                .raster
                .pixels()
                .expect("a frame")
                .as_slice()
                .to_vec()
        });
        (rain, still, halves)
    };
    let (storm, clear) = (under(Weather::Storm), under(Weather::Clear));
    assert!(
        storm.0.0 > 0.0 && clear.0.0 == 0.0,
        "the rain follows the weather: {} vs {}",
        storm.0.0,
        clear.0.0
    );
    assert!(
        storm.0.1 > 0.0 && clear.0.1 == 0.0,
        "the music hears the weather's rain: {} vs {}",
        storm.0.1,
        clear.0.1
    );
    assert!(storm.1 != clear.1, "the viewed frame follows the weather");
    for f in 0..2 {
        assert!(
            storm.2[f] != clear.2[f],
            "slide half {f} follows the weather"
        );
    }
}
