use super::*;

#[test]
fn walkable_debug_toggle_tints_blocked_pixels_and_is_reversible() {
    let scene = scene_with(vec![idle("/t/0.jsonl", 0, t0())], 16);
    let mut r = build(120, 60, vec![]);
    let now = t0();
    r.render(&scene, now).unwrap();
    let before = r.buf().expect("a frame").clone();

    // A known-blocked pixel from the live mask, below the busy top wall band.
    let layout = r.cached_layout().expect("layout").clone();
    let (bx, by) = (0..layout.buf_h)
        .flat_map(|y| (0..layout.buf_w).map(move |x| (x, y)))
        .find(|&(x, y)| y > layout.top_margin + 4 && !layout.is_walkable(x, y))
        .expect("some blocked cell below the wall band");

    r.set_debug_walkable(true);
    r.render(&scene, now).unwrap();
    let on = r.buf().expect("a frame").clone();
    // A warm cell's red channel barely rises while green/blue drop, so measure
    // DISTANCE to the blocked tint (220,60,60) rather than the red channel.
    let to_red = |c: pixtuoid_core::sprite::Rgb| {
        (i32::from(c.r) - 220).abs() + (i32::from(c.g) - 60).abs() + (i32::from(c.b) - 60).abs()
    };
    assert!(
        to_red(on.get(bx, by)) < to_red(before.get(bx, by)),
        "debug overlay must tint a blocked cell toward red (was {:?}, now {:?})",
        before.get(bx, by),
        on.get(bx, by),
    );
    let on_diff = region_diff(&before, &on, 0, 0, before.width(), before.height());
    assert!(
        on_diff > 1_000,
        "the debug layer must visibly change the frame"
    );

    r.set_debug_walkable(false);
    r.render(&scene, now).unwrap();
    let off_diff = region_diff(
        &before,
        r.buf().expect("a frame"),
        0,
        0,
        before.width(),
        before.height(),
    );
    assert!(
        off_diff < 200,
        "toggling the debug layer off must restore the scene (diff={off_diff})"
    );
}

/// 80 columns is the classic default and `min_terminal_size().1` is the tightest HEIGHT
/// that lays out the office at all — so it is also the tightest the popup must render
/// whole on: title and link both reach the frame. Not the tightest width: the panel
/// saturates at `VERSION_POPUP_W`, narrower wraps by design.
#[test]
fn the_version_popup_renders_whole_on_a_classic_terminal() {
    let version = env!("CARGO_PKG_VERSION");
    let rows = crate::tui::renderer::min_terminal_size().1;
    let mut r = build(80, rows, vec![]);
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
    // Past the 200ms entrance ease, so the panel is at full scale.
    let now = t0() + Duration::from_millis(250);
    r.render(&scene_with(vec![], 16), now).unwrap();
    let text = frame_text(r.frame_buffer());

    assert!(
        text.contains(&format!("v{version}")),
        "the title names the version:\n{text}"
    );
    assert!(
        text.contains("Release notes"),
        "the link is the CTA and must reach the frame in full:\n{text}"
    );
}

/// The body row is the destination a mouse-less terminal copies, and a
/// `Paragraph` clips a too-wide line silently — so the tail is what disappears,
/// on the composed frame no string-equality test can see. Swept from the
/// narrowest office the terminal lays out upward.
#[test]
fn the_popup_body_reaches_the_frame_at_every_office_width() {
    let version = env!("CARGO_PKG_VERSION");
    let (min_cols, rows) = crate::tui::renderer::min_terminal_size();
    for cols in [min_cols, min_cols + 8, 80, 120] {
        let mut r = build(cols, rows, vec![]);
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
        let now = t0() + Duration::from_millis(250);
        r.render(&scene_with(vec![], 16), now).unwrap();
        let text = frame_text(r.frame_buffer());
        assert!(
            text.contains(
                pixtuoid_scene::hit::REPO_URL
                    .strip_prefix("https://")
                    .expect("an https repository")
            ),
            "the body row's address is clipped at {cols} cols:\n{text}"
        );
        // At the popup's own measure the whole URL must land, version included.
        if cols >= 80 {
            assert!(
                text.contains(&format!("releases/tag/v{version}")),
                "the URL's tail is clipped at {cols} cols:\n{text}"
            );
        }
    }
}

#[test]
fn onboarding_overlay_renders_roster_and_hint() {
    use crate::panels::welcome::{OnboardingFrame, WelcomeRow};
    let scene = scene_with(vec![idle("/onboard/0.jsonl", 0, t0())], 16);
    let mut r = build(100, 40, vec![]);
    // A large elapsed so the typewriter and every staggered row are fully revealed.
    r.set_frames(
        RenderFrames {
            onboarding: OnboardingFrame {
                open: true,
                rows: vec![
                    WelcomeRow {
                        source_id: "codex",
                        label_prefix: "cx",
                        display_name: "Codex".into(),
                        checked: true,
                    },
                    WelcomeRow {
                        source_id: "claude-code",
                        label_prefix: "cc",
                        display_name: "Claude Code".into(),
                        checked: false,
                    },
                ],
                selected: 0,
                elapsed_ms: 100_000,
                dim: pixtuoid_scene::look::Dim::from_brightness(0.4),
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("Welcome to pixtuoid"),
        "onboarding title; frame:\n{text}"
    );
    assert!(text.contains("Codex"), "checked roster row; frame:\n{text}");
    assert!(
        text.contains("Claude Code"),
        "unchecked roster row; frame:\n{text}"
    );
    assert!(
        text.contains("space toggle") && text.contains("esc skip"),
        "key hint shown once rows are in; frame:\n{text}"
    );
    assert!(
        text.contains("press m anytime"),
        "the one-line audio offer rides the hints (#633); frame:\n{text}"
    );
}

#[test]
fn onboarding_dims_the_office_buffer() {
    use crate::panels::welcome::{OnboardingFrame, WelcomeRow};
    let scene = scene_with(vec![idle("/dim/0.jsonl", 0, t0())], 16);

    let mut base = build(100, 40, vec![]);
    base.render(&scene, t0()).unwrap();
    let bright = avg_lum(
        base.buf().expect("a frame"),
        0,
        0,
        base.buf().expect("a frame").width(),
        base.buf().expect("a frame").height(),
    );

    // The card paints on the cell layer, not the buffer, so this measures the
    // office pixel buffer only.
    let mut dimmed = build(100, 40, vec![]);
    dimmed.set_frames(
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
    dimmed.render(&scene, t0()).unwrap();
    let dim = avg_lum(
        dimmed.buf().expect("a frame"),
        0,
        0,
        dimmed.buf().expect("a frame").width(),
        dimmed.buf().expect("a frame").height(),
    );

    assert!(
        dim < bright * 0.6,
        "onboarding should dim the office buffer: dim={dim} vs bright={bright}"
    );
}

/// The onboarding's backdrop dims the world text the terminal sets over the
/// half-blocks, as it dims the office under it.
#[test]
fn onboarding_dims_the_world_text_over_the_half_blocks() {
    use crate::panels::welcome::OnboardingFrame;
    let scene = scene_with(vec![idle("/dim/0.jsonl", 0, t0())], 16);
    let dim = pixtuoid_scene::look::Dim::from_brightness(0.4);
    let mut bright = build(100, 40, vec![]);
    bright.render(&scene, t0()).unwrap();
    let mut dimmed = build(100, 40, vec![]);
    dimmed.set_frames(
        RenderFrames {
            onboarding: OnboardingFrame {
                dim,
                ..OnboardingFrame::default()
            },
            ..RenderFrames::default()
        },
        t0(),
    );
    dimmed.render(&scene, t0()).unwrap();
    assert_world_text_dims(bright.frame_buffer(), dimmed.frame_buffer(), dim);
}

#[test]
fn onboarding_dims_the_composed_slide_on_the_transition_path() {
    use crate::panels::welcome::{OnboardingFrame, WelcomeRow};
    let scene = two_floor_scene();
    let now = t0();
    let mid = now + Duration::from_millis(200);

    let mut bright_r = build(100, 40, vec![]);
    bright_r.render(&scene, now).unwrap();
    bright_r.navigate_floor(1, now);
    bright_r.render(&scene, mid).unwrap();
    assert!(bright_r.transition().is_some(), "baseline still mid-slide");
    let bright_slide = bright_r.session.picture().expect("the composed slide");
    let bb = bright_slide.pixels();
    let bright = avg_lum(bb, 0, 0, bb.width(), bb.height());

    let mut dim_r = build(100, 40, vec![]);
    dim_r.render(&scene, now).unwrap();
    dim_r.navigate_floor(1, now);
    dim_r.set_frames(
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
        now,
    );
    dim_r.render(&scene, mid).unwrap();
    assert!(dim_r.transition().is_some(), "dimmed still mid-slide");
    let dim_slide = dim_r.session.picture().expect("the composed slide");
    let db = dim_slide.pixels();
    let dim = avg_lum(db, 0, 0, db.width(), db.height());

    assert!(
        dim < bright * 0.6,
        "the transition path must dim the composed slide: dim={dim} vs bright={bright}"
    );
}

#[test]
fn coffee_machine_tooltip_on_hover() {
    let scene = scene_with(vec![idle("/tt/c.jsonl", 0, t0())], 16);
    let mut r = build(140, 48, vec![]);
    r.render(&scene, t0()).unwrap();
    let layout = r.cached_layout().expect("layout");
    let mut hover = None;
    'scan: for my in 0..48u16 {
        for mx in 0..140u16 {
            if crate::tui::hit_test::hit_test_coffee_machine(
                layout,
                crate::tui::geometry::CellArea::half_block(mx, my),
            ) {
                hover = Some((mx, my));
                break 'scan;
            }
        }
    }
    let hover = hover.expect("coffee machine should be hit-testable");
    r.set_mouse_pos(Some(hover));
    r.render(&scene, t0()).unwrap();
    assert!(
        frame_text(r.frame_buffer()).contains("Ivan"),
        "hovering the coffee machine shows the Buy-Ivan-a-coffee tooltip"
    );
}

#[test]
fn furniture_tooltip_on_hover_over_empty_desk() {
    // Agent on desk 0; hover an EMPTY desk so furniture (not agent) tooltip wins.
    let scene = scene_with(vec![idle("/tt/f.jsonl", 0, t0())], 16);
    let mut r = build(140, 48, vec![]);
    r.render(&scene, t0()).unwrap();
    let layout = r.cached_layout().expect("layout");
    if layout.home_desks.len() < 2 {
        return;
    }
    let d1 = layout.home_desks[1];
    r.set_mouse_pos(Some((d1.at.x + 4, d1.at.y / 2 + 1)));
    r.render(&scene, t0()).unwrap();
    assert!(
        frame_text(r.frame_buffer()).contains("Desk"),
        "hovering an empty desk shows the Desk furniture tooltip"
    );
}

#[test]
fn pet_tooltip_on_hover() {
    let scene = scene_with(vec![running("/tt/p.jsonl", 0, "Edit", t0())], 16);
    let mut r = build(140, 48, vec![PetKind::Cat]);
    r.render(&scene, t0()).unwrap();
    let PetHover { centre: pos, .. } = r.drawn_pet().expect("cat placed");
    r.set_mouse_pos(Some((pos.x, pos.y / 2)));
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("Pet me!"),
        "hovering the cat, sitting at home, shows its tooltip"
    );
}

/// Renders `r` until its pet walks, where its tooltip names it: the pet and
/// that frame's time.
fn until_the_pet_walks(
    r: &mut TuiRenderer<TestBackend>,
    scene: &SceneState,
) -> (PetHover, SystemTime) {
    (0..2 * pixtuoid_scene::PET_LONGEST_REST_MS / 250)
        .map(|step| t0() + Duration::from_millis(step * 250))
        .find_map(|now| {
            r.render(scene, now).unwrap();
            r.drawn_pet()
                .filter(|p| p.anim == p.kind.walk_anim().piece())
                .map(|p| (p, now))
        })
        .expect("the pet sets off")
}

#[test]
fn pet_tooltip_shows_custom_name() {
    let scene = scene_with(vec![running("/tt/cn.jsonl", 0, "Edit", t0())], 16);
    let cat = pixtuoid_scene::pet::Pet {
        kind: PetKind::Cat,
        name: "Luna".to_string(),
    };
    let mut r = build_pets(140, 48, vec![cat]);
    let (PetHover { centre: pos, .. }, now) = until_the_pet_walks(&mut r, &scene);
    r.set_mouse_pos(Some((pos.x, pos.y / 2)));
    r.render(&scene, now).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("Luna"),
        "hovering the cat shows its custom name; got:\n{text}"
    );
    assert!(
        !text.contains("Office Cat"),
        "custom name replaces the default, not appended"
    );
}

#[test]
fn pet_tooltip_falls_back_to_default_name_when_not_configured() {
    let scene = scene_with(vec![running("/tt/fb.jsonl", 0, "Edit", t0())], 16);
    let mut r = build(140, 48, vec![PetKind::Cat]);
    let (PetHover { centre: pos, .. }, now) = until_the_pet_walks(&mut r, &scene);
    r.set_mouse_pos(Some((pos.x, pos.y / 2)));
    r.render(&scene, now).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("Office Cat"),
        "an unconfigured cat falls back to the default name; got:\n{text}"
    );
}

#[test]
fn hovered_running_agent_tooltip_shows_state_and_detail() {
    // running() sets last_event_at = started; created >5s ago, past the tooltip's
    // freshness floor, so the running-% meter shows; running_ms>0 makes it non-zero.
    let mut a = running(
        "/ttA/0.jsonl",
        0,
        "Edit src/lib.rs",
        t0() - Duration::from_secs(600),
    );
    a.running_ms = 120_000; // 120s running over a 600s session ⇒ 20%
    let id = a.agent_id;
    let scene = scene_with(vec![a], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, id);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(text.contains("Running"), "running state word: {text}");
    assert!(text.contains("Edit"), "tool name on the state line: {text}");
    assert!(text.contains("src/lib.rs"), "detail line args: {text}");
    assert!(text.contains("20%"), "exact running percent: {text}");
    assert!(
        text.contains("\u{25ae}\u{25af}\u{25af}\u{25af}\u{25af}"),
        "meter fill (1 filled ▮ + 4 empty ▯): {text}"
    );
}

#[test]
fn hovered_burning_agent_tooltip_shows_model_and_fresh_effort() {
    use pixtuoid_core::state::EffortObservation;
    let mut a = running(
        "/burn/0.jsonl",
        0,
        "Read src/main.rs",
        t0() - Duration::from_secs(30),
    );
    a.source = std::sync::Arc::from("claude-code");
    a.model = Some("claude-fable-5".into());
    a.effort = Some(EffortObservation::new("ultra".into(), t0()));
    let id = a.agent_id;
    let plain = running(
        "/burn/1.jsonl",
        1,
        "Read src/lib.rs",
        t0() - Duration::from_secs(30),
    );
    let plain_id = plain.agent_id;
    let scene = scene_with(vec![a, plain], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, id);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("\u{2605} claude-fable-5 \u{b7} ultra"),
        "model + fresh effort row: {text}"
    );
    super::hover_agent(&mut r, plain_id);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    // (The wall board's own `★ Star` CTA is unrelated — assert the MODEL row.)
    assert!(
        !text.contains("\u{2605} claude-fable-5"),
        "no model row without an observation: {text}"
    );
}

#[test]
fn stale_effort_drops_off_the_dossier() {
    // An hour-old stamp is comfortably past the freshness window
    // (`pixtuoid_scene::burn::EFFORT_TTL_SECS`, crate-private).
    use pixtuoid_core::state::EffortObservation;
    let mut a = running(
        "/burn/2.jsonl",
        0,
        "Read src/main.rs",
        t0() - Duration::from_secs(30),
    );
    a.source = std::sync::Arc::from("claude-code");
    a.model = Some("claude-fable-5".into());
    a.effort = Some(EffortObservation::new(
        "ultra".into(),
        t0() - Duration::from_secs(3600),
    ));
    let id = a.agent_id;
    let scene = scene_with(vec![a], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, id);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("\u{2605} claude-fable-5"),
        "model row stays: {text}"
    );
    assert!(
        !text.contains("\u{b7} ultra"),
        "stale effort suffix must be suppressed: {text}"
    );
}

#[test]
fn the_exit_sentinel_never_renders_in_the_dossier() {
    use pixtuoid_core::source::claude_code::ULTRA_EXIT_LABEL;
    use pixtuoid_core::state::EffortObservation;
    let mut a = running(
        "/burn/3.jsonl",
        0,
        "Read src/main.rs",
        t0() - Duration::from_secs(30),
    );
    a.source = std::sync::Arc::from("claude-code");
    a.model = Some("claude-fable-5".into());
    a.effort = Some(EffortObservation::new(ULTRA_EXIT_LABEL.into(), t0()));
    let id = a.agent_id;
    let scene = scene_with(vec![a], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, id);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("\u{2605} claude-fable-5"),
        "model row stays: {text}"
    );
    assert!(
        !text.contains(ULTRA_EXIT_LABEL),
        "the internal sentinel must never render: {text}"
    );
}

#[test]
fn hovered_agent_tooltip_shows_source_badge() {
    let mut a = running(
        "/badge/0.jsonl",
        0,
        "Read src/main.rs",
        t0() - Duration::from_secs(30),
    );
    // The badge resolves the source id → `label_prefix` via the registry; use the
    // real id (the fixtures' shorthand "cc" is a prefix, not a registered id).
    a.source = std::sync::Arc::from("claude-code");
    let id = a.agent_id;
    let scene = scene_with(vec![a], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, id);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(text.contains("[cc]"), "source badge on the tooltip: {text}");
    // The fixtures' session_id is "s"; `disambig_suffix` is deterministic.
    let id4 = pixtuoid_scene::badge::disambig_suffix("s");
    assert!(
        text.contains(&format!("\u{b7}{id4}")),
        "id4 disambiguation suffix ·{id4}: {text}"
    );
}

#[test]
fn hovered_subagent_tooltip_shows_lineage() {
    let parent = running(
        "/lin/root.jsonl",
        0,
        "Read a",
        t0() - Duration::from_secs(60),
    );
    let parent_id = parent.agent_id;
    let mut child = running(
        "/lin/child.jsonl",
        1,
        "Edit b",
        t0() - Duration::from_secs(30),
    );
    child.label = "kid".into();
    child.parent_id = Some(parent_id);
    let child_id = child.agent_id;
    let scene = scene_with(vec![parent, child], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, child_id);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("\u{21b3} under"),
        "lineage line on the subagent: {text}"
    );
}

#[test]
fn hovered_requires_action_agent_without_a_reason_shows_the_bare_state() {
    let hovered = |reason: Option<&str>| {
        let mut a = idle("/ttB/0.jsonl", 0, t0() - Duration::from_secs(60));
        a.state = ActivityState::RequiresAction {
            reason: reason.map(Arc::from),
        };
        let id = a.agent_id;
        let scene = scene_with(vec![a], 16);
        let mut r = build(120, 44, vec![]);
        r.render(&scene, t0()).unwrap();
        super::hover_agent(&mut r, id);
        r.render(&scene, t0()).unwrap();
        frame_text(r.frame_buffer())
    };
    let bare = hovered(None);
    assert!(bare.contains("Requires action"), "state arm: {bare}");
    // The reason row is the `?`-flagged line: one more `?` than the bare card.
    assert_eq!(
        hovered(Some("x")).matches('?').count(),
        bare.matches('?').count() + 1,
        "no reason line: {bare}"
    );
}

#[test]
fn hovered_requires_action_agent_tooltip_shows_reason() {
    let mut a = idle("/ttW/0.jsonl", 0, t0() - Duration::from_secs(60));
    a.state = ActivityState::RequiresAction {
        reason: Some(Arc::from("permission to edit")),
    };
    let id = a.agent_id;
    let scene = scene_with(vec![a], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, id);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("Requires action"),
        "requires-action state arm: {text}"
    );
    assert!(
        text.contains("?permission"),
        "?-flagged reason line: {text}"
    );
}

#[test]
fn hovered_exiting_agent_tooltip_suppresses_meter() {
    // A walking-out agent keeps its retained Running payload (`mark_exiting`
    // doesn't reset `state`), but the dossier reads `◌ Exiting`.
    let mut a = running(
        "/exM/0.jsonl",
        0,
        "Edit src/lib.rs",
        t0() - Duration::from_secs(600),
    );
    a.running_ms = 120_000; // a 20% meter if it were NOT exiting
    a.exiting_at = Some(t0());
    let id = a.agent_id;
    let scene = scene_with(vec![a], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, id);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(text.contains("Exiting"), "exiting state word: {text}");
    assert!(
        !text.contains('%'),
        "no running-% meter on an exiting card: {text}"
    );
}

#[test]
fn hovered_exiting_agent_tooltip_suppresses_requires_action_reason() {
    let mut a = idle("/exW/0.jsonl", 0, t0() - Duration::from_secs(60));
    a.state = ActivityState::RequiresAction {
        reason: Some(Arc::from("permission to edit")),
    };
    a.exiting_at = Some(t0());
    let id = a.agent_id;
    let scene = scene_with(vec![a], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, id);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(text.contains("Exiting"), "exiting state word: {text}");
    assert!(
        !text.contains("?permission"),
        "no ?reason line on an exiting card: {text}"
    );
    assert!(
        !text.contains("Requires action"),
        "state word is Exiting, not RequiresAction: {text}"
    );
}

#[test]
fn exiting_agent_label_uses_exiting_color() {
    // Color is theme-internal, so this only asserts the exiting branch paints.
    let mut a = idle("/ttE/0.jsonl", 0, t0() - Duration::from_secs(10));
    a.label = "LEAVING".into();
    a.exiting_at = Some(t0());
    let scene = scene_with(vec![a], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0() + Duration::from_millis(100)).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(text.contains("LEAVING"), "exiting agent label: {text}");
}

#[test]
fn hovered_then_removed_agent_is_a_safe_noop() {
    let id = AgentId::from_transcript_path("/ttGone/0.jsonl");
    let scene = scene_with(vec![slot(id, 0, 0, t0())], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, id);
    let empty = SceneState::uniform(16);
    r.render(&empty, t0() + Duration::from_millis(PAINT_FRAME_MS))
        .expect("render must not panic when the hovered agent vanished");
}

/// A Still floor's wall board holds its flap: an instant where the same office
/// under Full rolls its board within one beat leaves the Still board as it was.
#[test]
fn a_still_floor_holds_its_wall_board_flap() {
    use pixtuoid_scene::anim::{FULL_TICK_MS, Motion};
    let scene = scene_with(vec![running("/t/0.jsonl", 0, "Edit a.rs", t0())], 16);
    let (cols, rows) = (
        pixtuoid_scene::layout::NEON_PANEL_W + 2,
        pixtuoid_scene::layout::NEON_PANEL_INNER_Y / 2 + 3,
    );
    let board_at = |motion, at| {
        let mut r = build(120, 40, vec![]);
        r.set_motion(motion);
        r.render(&scene, at).expect("render");
        let buf = r.frame_buffer();
        (0..rows)
            .flat_map(|y| (0..cols).map(move |x| (x, y)))
            .map(|cell| buf.cell(cell).map(|c| c.symbol().to_string()))
            .collect::<Vec<_>>()
    };
    let beat = Duration::from_millis(FULL_TICK_MS);
    // One flap cycle is two halves of whole beats; some beat in it rolls.
    let rolling = (0..64)
        .map(|n| t0() + beat * n)
        .find(|&at| board_at(Motion::Full, at) != board_at(Motion::Full, at + beat))
        .expect("a Full board rolls its flap within a cycle");
    assert_eq!(
        board_at(Motion::Still, rolling),
        board_at(Motion::Still, rolling + beat),
        "the Still board rolled"
    );
}

/// In the half-blocks `w` is listed in the help the frame draws and flips a
/// layer the frame shows.
#[cfg(debug_assertions)]
#[test]
fn the_classic_lists_w_and_its_key_tints_the_frame() {
    use crossterm::event::KeyCode;
    let scene = scene_with(vec![idle("/t/0.jsonl", 0, t0())], 16);
    let mut r = build(140, 60, vec![]);
    r.set_frames(
        RenderFrames {
            help_open: true,
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(text.contains("walkable"), "the help lists w: {text}");
    r.set_frames(RenderFrames::default(), t0());
    r.render(&scene, t0()).unwrap();
    let before = r.buf().expect("a frame").clone();
    assert!(crate::tui::toggle_walkable_debug_on_w(
        &mut r,
        KeyCode::Char('w'),
        &crate::panels::ModalState::closed()
    ));
    r.render(&scene, t0()).unwrap();
    let after = r.buf().expect("a frame");
    assert!(
        region_diff(&before, after, 0, 0, before.width(), before.height()) > 1_000,
        "the key must show the layer"
    );
}
