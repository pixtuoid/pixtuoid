use super::*;

use crate::panels::connection::ConnectionFrame;

#[test]
fn too_small_terminal_returns_no_layout_no_panic() {
    let scene = scene_with(vec![idle("/sm/0.jsonl", 0, t0())], 16);
    let mut r = build(15, 8, vec![]); // under the `MIN_SCENE_*` gate
    r.render(&scene, t0()).expect("render must not panic");
    assert!(
        r.cached_layout().is_none(),
        "a too-small terminal yields no layout"
    );
}

/// A refusal has to SAY so — before #908 a sub-floor terminal got a footer over a
/// blank office, indistinguishable from a crash. The fixture is DERIVED: pinning a
/// size here is how the old one rotted into a size that renders fine.
#[test]
fn a_terminal_under_the_layout_minimum_says_why_it_is_not_drawing() {
    let scene = scene_with(vec![idle("/sm/0.jsonl", 0, t0())], 16);
    for (cols, rows) in [too_small_terminal(), (15, 8)] {
        let mut r = build(cols, rows, vec![]);
        r.render(&scene, t0()).expect("render");
        assert!(r.cached_layout().is_none(), "{cols}x{rows} must refuse");
        let text = frame_text(r.frame_buffer());
        assert!(
            text.contains("too small"),
            "{cols}x{rows}: the refusal must be visible, saw:\n{text}"
        );
        // The wide form names both sizes; a terminal too narrow to hold that
        // sentence still gets the requirement.
        assert!(
            text.contains(&format!("this is {cols}x{rows}")) || text.contains("min"),
            "{cols}x{rows}: and must name what it needs"
        );
    }
}

/// A first run on a sub-floor terminal is the exact case the notice exists for, and
/// first run is also when the onboarding modal opens — over the same centred rows.
#[test]
fn the_too_small_notice_survives_an_open_onboarding_modal() {
    use crate::panels::welcome::{OnboardingFrame, WelcomeRow};
    let scene = scene_with(vec![idle("/sm/0.jsonl", 0, t0())], 16);
    let (cols, rows) = too_small_terminal();
    let mut r = build(cols, rows, vec![]);
    r.set_frames(
        RenderFrames {
            onboarding: OnboardingFrame {
                open: true,
                rows: vec![WelcomeRow {
                    source_id: "claude-code",
                    label_prefix: "cc",
                    display_name: "Claude Code".into(),
                    checked: true,
                }],
                selected: 0,
                elapsed_ms: 60_000,
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).expect("render");
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("too small"),
        "the modal must not bury the only message that explains the black screen:\n{text}"
    );
}

/// The size the notice NAMES has to be a size that works — a requirement nobody
/// re-derives is the half of the rule that rots.
#[test]
fn the_size_the_too_small_notice_names_is_one_that_seats_someone() {
    let scene = scene_with(vec![idle("/sm/0.jsonl", 0, t0())], 16);
    let (small_cols, small_rows) = too_small_terminal();
    let mut small = build(small_cols, small_rows, vec![]);
    small.render(&scene, t0()).expect("render");
    let text = frame_text(small.frame_buffer());
    let named = text
        .split_whitespace()
        .find_map(|w| w.trim_end_matches(',').split_once('x'))
        .and_then(|(c, r)| Some((c.parse::<u16>().ok()?, r.parse::<u16>().ok()?)))
        .expect("the notice must name a size");

    let mut at_min = build(named.0, named.1, vec![]);
    at_min.render(&scene, t0()).expect("render");
    let layout = at_min
        .cached_layout()
        .unwrap_or_else(|| panic!("the notice asks for {named:?}, which does not lay out"));
    assert!(
        !layout.home_desks.is_empty(),
        "the notice asks for {named:?}, which lays out an office with no desk to seat anyone"
    );
    for (cols, rows, axis) in [
        (named.0, named.1 - 1, "row"),
        (named.0 - 1, named.1, "column"),
    ] {
        let mut one_short = build(cols, rows, vec![]);
        one_short.render(&scene, t0()).expect("render");
        assert!(
            one_short.cached_layout().is_none(),
            "{named:?} minus a {axis} lays out, so the notice overstates"
        );
    }
}

/// A refused frame drew nothing, so the click handler must not hit-test the
/// last drawn frame's sprites; both refusal arms.
#[test]
fn shrinking_under_the_minimum_drops_the_last_frames_hit_targets() {
    let id = AgentId::from_transcript_path("/sm/0.jsonl");
    let scene = scene_with(vec![idle("/sm/0.jsonl", 0, t0())], 16);
    let (cols, rows) = (192, 80);
    // The scene-size refusal, then the layout-compute refusal (clears the scene
    // minimum, under `MIN_LAYOUT_W`).
    for (small_cols, small_rows) in [too_small_terminal(), (28, 40)] {
        let mut r = build(cols, rows, vec![PetKind::Cat]);
        r.render(&scene, t0()).expect("render");
        let cell = (0..cols)
            .flat_map(|c| (0..rows).map(move |row| (c, row)))
            .find(|&(c, row)| r.hit_test_agent_at(c, row) == Some(id))
            .expect("the drawn agent is hit-testable");
        assert!(r.drawn_pet().is_some(), "the pet is drawn");

        r.terminal.backend_mut().resize(small_cols, small_rows);
        r.render(&scene, t0()).expect("render");
        assert_eq!(
            r.hit_test_agent_at(cell.0, cell.1),
            None,
            "{small_cols}x{small_rows}"
        );
        assert!(r.drawn_pet().is_none(), "{small_cols}x{small_rows}");
        assert_eq!(
            r.scene_area_at(cell.0, cell.1),
            None,
            "{small_cols}x{small_rows}"
        );
    }
}

/// The other direction: a terminal that CAN lay out must never show the notice.
#[test]
fn a_terminal_that_fits_shows_no_too_small_notice() {
    let scene = scene_with(vec![idle("/ok/0.jsonl", 0, t0())], 16);
    let mut r = build(100, 40, vec![]);
    r.render(&scene, t0()).expect("render");
    assert!(r.cached_layout().is_some(), "100x40 must lay out");
    assert!(
        !frame_text(r.frame_buffer()).contains("terminal too small"),
        "a fitting terminal must not be told it is too small"
    );
}

#[test]
fn colliding_labels_with_multibyte_session_ids_do_not_panic() {
    let mut scene = SceneState::uniform(16);
    // In `/naïveté/app` the `ï` occupies bytes 3..5, so a `&session_id[..4]` slice
    // splits it; the shared label makes the disambiguation suffix fire.
    let mut mk = |id: &str, desk: usize| {
        let a = AgentId::from_transcript_path(id);
        let mut s = slot(a, 0, desk, t0());
        s.label = "rx\u{00b7}proj".into();
        s.session_id = Arc::from("/na\u{00ef}vet\u{00e9}/app");
        scene.agents.insert(a, s);
    };
    mk("/mb/0.jsonl", 0);
    mk("/mb/1.jsonl", 1);
    let mut r = build(120, 40, vec![]);
    r.render(&scene, t0())
        .expect("render must not panic on a multi-byte session_id");
}

#[test]
fn no_layout_frame_paints_the_popup_at_its_clickable_scale() {
    // 100x16 → scene_rect 100x15 passes render()'s `MIN_SCENE_*` gate, but buf_h=30 is
    // below compute_with_seed's office minimum → draw_scene paints the footer-only frame.
    let scene = scene_with(vec![idle("/nl/0.jsonl", 0, t0())], 16);
    let mut r = build(100, 16, vec![]);
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
    let t = t0() + Duration::from_millis(150); // mid-entrance ⇒ scale > 0
    let painted = r.version_popup_scale(t);
    assert!(painted > 0.0, "the popup is animating this frame");
    r.render(&scene, t).expect("render");
    assert!(
        r.cached_layout().is_none(),
        "no layout produced at this size"
    );
    assert_eq!(
        r.last_popup_scale(),
        painted,
        "the hit-box scale must equal the scale the painter used"
    );
    assert!(
        frame_text(r.frame_buffer()).contains("Enter to close"),
        "the popup must actually paint on the footer-only frame"
    );
}

#[test]
fn modal_overlays_still_paint_when_the_office_cannot_lay_out() {
    use crate::panels::welcome::{OnboardingFrame, WelcomeRow};
    let scene = scene_with(vec![idle("/tiny/0.jsonl", 0, t0())], 16);

    let (cols, rows) = too_small_terminal();
    let mut r = build(cols, rows, vec![]);
    r.set_frames(
        RenderFrames {
            help_open: true,
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).expect("render");
    assert!(
        r.cached_layout().is_none(),
        "{cols}x{rows} is below the office layout minimum — this IS the footer-only path"
    );
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("Keyboard"),
        "the help overlay must paint on the footer-only frame; frame was:\n{text}"
    );

    let mut r = build(cols, rows, vec![]);
    r.set_frames(
        RenderFrames {
            onboarding: OnboardingFrame {
                open: true,
                rows: vec![WelcomeRow {
                    source_id: "codex",
                    label_prefix: "cx",
                    display_name: "Codex".into(),
                    checked: true,
                }],
                selected: 0,
                elapsed_ms: 100_000,
                dim: pixtuoid_scene::look::Dim::from_brightness(0.4),
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).expect("render");
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("Welcome to pixtuoid"),
        "the onboarding overlay must paint on the footer-only frame; frame was:\n{text}"
    );

    let mut r = build(cols, rows, vec![]);
    r.set_frames(
        RenderFrames {
            connection: ConnectionFrame {
                open: true,
                socket_line: "socket  /tmp/p.sock".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).expect("render");
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("Sources"),
        "the Sources panel must paint on the footer-only frame; frame was:\n{text}"
    );
}

#[test]
fn modal_overlays_still_paint_during_a_slide_on_a_too_small_terminal() {
    let scene = two_floor_scene();
    // Under the gate on BOTH axes.
    let mut r = build(
        crate::tui::renderer::MIN_SCENE_WIDTH - 1,
        crate::tui::renderer::MIN_SCENE_HEIGHT - 1 + crate::tui::renderer::FOOTER_ROWS,
        vec![],
    );
    let now = t0();
    r.render(&scene, now).expect("render");
    r.set_frames(
        RenderFrames {
            help_open: true,
            version_popup: crate::panels::ui_state::VersionPopupFrame {
                open: true,
                ..Default::default()
            },
            ..Default::default()
        },
        now,
    );
    r.navigate_floor(1, now);

    let t = now + Duration::from_millis(100); // mid-slide, mid-entrance
    let painted = r.version_popup_scale(t);
    assert!(painted > 0.0, "the popup is animating this frame");
    r.render(&scene, t)
        .expect("transition render on a tiny terminal must not panic");

    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("Keyboard"),
        "the help overlay must paint on the slide's footer-only frame; \
         frame was:\n{text}"
    );
    assert_eq!(
        r.last_popup_scale(),
        painted,
        "the click hit-box must carry the scale this arm painted at"
    );
}

#[test]
fn departed_agent_walks_are_evicted_on_a_non_current_floor() {
    let cap = 16;
    let a = AgentId::from_transcript_path("/ev/floor0.jsonl");
    let b = AgentId::from_transcript_path("/ev/floor1.jsonl");
    // Long-idle so both wander and acquire a WalkState.
    let scene = scene_with(
        vec![
            slot(a, 0, 0, t0() - Duration::from_secs(120)),
            slot(b, 1, cap, t0() - Duration::from_secs(120)),
        ],
        cap,
    );
    let mut r = build(100, 40, vec![]);
    let mut now = t0();

    for _ in 0..10 {
        r.render(&scene, now).expect("render");
        now += Duration::from_millis(PAINT_FRAME_MS);
    }
    r.navigate_floor(1, now);
    render_until_settled(&mut r, &scene, &mut now, 1);
    for _ in 0..10 {
        r.render(&scene, now).expect("render");
        now += Duration::from_millis(PAINT_FRAME_MS);
    }
    assert!(
        r.floor_walks(1).and_then(|m| m.get(&b)).is_some(),
        "floor-1 agent B should have a WalkState after visiting floor 1"
    );

    r.navigate_floor(0, now);
    render_until_settled(&mut r, &scene, &mut now, 0);
    let scene_without_b = scene_with(vec![slot(a, 0, 0, t0() - Duration::from_secs(120))], cap);
    now += Duration::from_millis(PAINT_FRAME_MS);
    r.evict_missing(&scene_without_b);
    r.render(&scene_without_b, now).expect("render");

    assert_eq!(
        r.floor_walks(1).map(|m| m.contains_key(&b)),
        Some(false),
        "a departed agent's WalkState must be evicted even on a non-current floor"
    );
}

#[test]
fn evict_missing_drops_history_and_walks_on_every_floor() {
    let cap = 16;
    let a = AgentId::from_transcript_path("/ev2/floor0.jsonl");
    let b = AgentId::from_transcript_path("/ev2/floor1.jsonl");
    // Fresh agents: the entry walk populates BOTH history (per-frame walker
    // position records) and walks (entry profile) on their floors.
    let scene = scene_with(vec![slot(a, 0, 0, t0()), slot(b, 1, cap, t0())], cap);
    let mut r = build(100, 40, vec![]);
    let mut now = t0();

    for _ in 0..5 {
        now += Duration::from_millis(PAINT_FRAME_MS);
        r.render(&scene, now).expect("render");
    }
    r.navigate_floor(1, now);
    render_until_settled(&mut r, &scene, &mut now, 1);
    for _ in 0..5 {
        now += Duration::from_millis(PAINT_FRAME_MS);
        r.render(&scene, now).expect("render");
    }
    assert_eq!(
        r.floor_history(0).map(|h| h.contains(a)),
        Some(true),
        "floor-0 history should hold agent A after its entry frames"
    );
    assert_eq!(
        r.floor_history(1).map(|h| h.contains(b)),
        Some(true),
        "floor-1 history should hold agent B after its entry frames"
    );
    assert!(
        r.floor_walks(1).and_then(|m| m.get(&b)).is_some(),
        "floor-1 walks should hold agent B"
    );

    let empty = SceneState::uniform(cap);
    r.evict_missing(&empty);

    for floor in 0..2 {
        assert_eq!(
            r.floor_history(floor)
                .map(|h| h.contains(a) || h.contains(b)),
            Some(false),
            "departed agents' PoseHistory must be evicted on floor {floor}"
        );
        assert_eq!(
            r.floor_walks(floor)
                .map(|m| m.contains_key(&a) || m.contains_key(&b)),
            Some(false),
            "departed agents' WalkState must be evicted on floor {floor}"
        );
    }
}

#[test]
fn floor_transition_clears_stale_pet_position() {
    let cap = 16;
    let mut scene = SceneState::uniform(cap);
    let a = AgentId::from_transcript_path("/pettrans/f0.jsonl");
    let b = AgentId::from_transcript_path("/pettrans/f1.jsonl");
    scene.agents.insert(a, slot(a, 0, 0, t0()));
    scene.agents.insert(b, slot(b, 1, cap, t0())); // floor 1 ⇒ navigate_floor(1) valid

    let mut r = build(100, 40, vec![PetKind::Cat]);
    let mut now = t0();
    for _ in 0..3 {
        r.render(&scene, now).expect("render");
        now += Duration::from_millis(PAINT_FRAME_MS);
    }
    assert!(
        r.drawn_pet().is_some(),
        "a pet should be drawn on the normal floor-0 frame"
    );

    r.navigate_floor(1, now);
    r.render(&scene, now).expect("render"); // single in-flight transition frame
    assert!(
        r.drawn_pet().is_none(),
        "an in-flight floor transition must clear the stale pet position"
    );
}

#[test]
fn layout_compute_none_bails_to_footer_only() {
    let scene = scene_with(vec![idle("/lc/0.jsonl", 0, t0())], 16);
    // Clears `MIN_SCENE_WIDTH` but not the layout's `MIN_LAYOUT_W`: the second bail arm.
    let mut r = build(28, 40, vec![]);
    r.render(&scene, t0())
        .expect("render must not error on the compute-None bail");
    assert!(
        r.cached_layout().is_none(),
        "a layout that fails compute yields no cached layout"
    );
}

/// A refused frame steps nothing, but the door still closes on time: the
/// clamp is the refused frame's own, not the last drawn one's.
#[test]
fn a_refused_classic_frame_keeps_the_doors_clamp_on_time() {
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let mut r = build(cols, rows, vec![]);
    let scene = scene_with(vec![idle("/door/0.jsonl", 0, t0())], 16);
    r.render(&scene, t0()).expect("render");
    assert!(
        r.session.floor(0).expect("a floor").ctx.door_anim_max_ms > 0,
        "the entry walk holds the door"
    );
    let (small_cols, small_rows) = too_small_terminal();
    r.terminal.backend_mut().resize(small_cols, small_rows);
    r.render(&scene, t0() + Duration::from_secs(600))
        .expect("render");
    assert_eq!(
        r.session.floor(0).expect("a floor").ctx.door_anim_max_ms,
        0,
        "the walk long arrived"
    );
}

/// A frame the loop scheduled reaches the hitch count through the renderer:
/// shown a second after its due, it hitches; a redraw with no due doesn't.
#[test]
fn a_scheduled_frame_shown_late_hitches() {
    let scene = scene_with(vec![idle("/sm/0.jsonl", 0, t0())], 16);
    let summary = |late: Option<std::time::Duration>| {
        crate::test_capture::capture(|| {
            let mut r = build(120, 40, vec![]);
            if let Some(late) = late {
                r.due_at(std::time::Instant::now() - late);
            }
            r.render(&scene, t0()).expect("render");
            r.finish_pacing();
        })
    };
    assert!(summary(None).contains("scheduled=0 "));
    let late = summary(Some(std::time::Duration::from_secs(1)));
    assert!(late.contains("scheduled=1 "), "{late}");
    assert!(!late.contains("hitch_ms=0.0 "), "{late}");
}

/// A frame the terminal fails to take is the render's error, which ends the
/// run, never a stale screen shown as drawn.
#[test]
fn a_failed_present_ends_the_render() {
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let (mut r, _, wire) = half_blocks_on_screen(cols, rows);
    let scene = scene_with(vec![idle("/sm/0.jsonl", 0, t0())], 16);
    wire.fail.store(true, std::sync::atomic::Ordering::Relaxed);
    let e = r.render(&scene, t0()).expect_err("the present failed");
    assert_eq!(
        e.downcast_ref::<std::io::Error>().map(std::io::Error::kind),
        Some(std::io::ErrorKind::BrokenPipe)
    );
}
