use super::*;

#[test]
fn offscreen_floor_freezes_and_resyncs_on_return() {
    let theme = pixtuoid_scene::theme::ALL_THEMES[0];
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    // A filler on floor 1 so `num_floors` == 2; the floor-0 agent is long-idle ⇒ wanders.
    let cap = 16;
    let mut scene = SceneState::uniform(cap);
    let a = AgentId::from_transcript_path("/h/floor0.jsonl");
    let b = AgentId::from_transcript_path("/h/floor1.jsonl");
    scene
        .agents
        .insert(a, slot(a, 0, 0, t0 - Duration::from_secs(120)));
    scene.agents.insert(b, slot(b, 1, cap, t0));

    let term = Terminal::new(TestBackend::new(100, 40)).expect("test backend");
    let mut r = TuiRenderer::new(term, theme, vec![], pack_arc());

    // Warm up floor 0 so agent A's WalkState initialises and wanders.
    let mut now = t0;
    for _ in 0..10 {
        r.render(&scene, now).expect("render");
        now += Duration::from_millis(PAINT_FRAME_MS);
    }
    assert_eq!(r.current_floor(), 0);
    assert!(
        r.floor_walks(0).and_then(|m| m.get(&a)).is_some(),
        "floor-0 agent should have a WalkState after warm-up"
    );

    r.navigate_floor(1, now);
    render_until_settled(&mut r, &scene, &mut now, 1);

    let frozen_at = r
        .floor_walks(0)
        .and_then(|m| m.get(&a))
        .map(|walk| walk.wander().last_advanced_at())
        .expect("floor-0 walks present");

    // ~30 s on floor 1.
    for _ in 0..900 {
        now += Duration::from_millis(PAINT_FRAME_MS);
        r.render(&scene, now).expect("render");
    }
    let still_frozen = r
        .floor_walks(0)
        .and_then(|m| m.get(&a))
        .map(|walk| walk.wander().last_advanced_at())
        .expect("floor-0 walks present");
    assert_eq!(
        frozen_at, still_frozen,
        "off-screen floor 0 walks must stay frozen while floor 1 is visible"
    );

    let back_at = now;
    r.navigate_floor(0, now);
    render_until_settled(&mut r, &scene, &mut now, 0);

    let walk = r
        .floor_walks(0)
        .and_then(|m| m.get(&a))
        .expect("floor-0 walks present");
    assert!(
        walk.wander().phase_started_at() >= back_at,
        "floor-0 agent must resync its wander clock on return (got an anchor before the switch-back ⇒ replay)"
    );
}

#[test]
fn floor_transition_completes_and_lands() {
    let scene = two_floor_scene();
    let mut r = build(100, 40, vec![]);
    let mut now = t0();
    r.render(&scene, now).unwrap();
    assert_eq!(r.current_floor(), 0);

    r.navigate_floor(1, now);
    assert!(
        r.transition().is_some(),
        "navigation should begin a transition"
    );

    now += Duration::from_millis(450);
    r.render(&scene, now).unwrap();
    assert!(r.transition().is_some(), "still transitioning mid-slide");
    assert!(
        r.cached_layout().is_none(),
        "layout is cleared during a transition"
    );

    now += Duration::from_millis(600); // past `FloorTransition::duration_ms`
    r.render(&scene, now).unwrap();
    assert!(r.transition().is_none(), "transition complete");
    assert_eq!(r.current_floor(), 1, "landed on the target floor");
    assert!(
        r.cached_layout().is_some(),
        "layout recomputed after landing"
    );
}

#[test]
fn navigation_blocked_during_active_transition() {
    let cap = 16;
    let scene = scene_with(
        vec![
            idle("/b/0.jsonl", 0, t0()),
            slot(AgentId::from_transcript_path("/b/1.jsonl"), 1, cap, t0()),
            slot(
                AgentId::from_transcript_path("/b/2.jsonl"),
                2,
                2 * cap,
                t0(),
            ),
        ],
        cap,
    );
    let mut r = build(100, 40, vec![]);
    let now = t0();
    r.render(&scene, now).unwrap();
    r.navigate_floor(1, now);
    r.navigate_floor(2, now); // must be ignored — a transition is in flight
    assert_eq!(
        r.transition().map(|t| t.to_floor),
        Some(1),
        "a second navigate during a transition is a no-op"
    );
}

#[test]
fn transition_cancelled_when_target_floor_disappears() {
    let cap = 16;
    let f1 = slot(AgentId::from_transcript_path("/c/1.jsonl"), 1, cap, t0());
    let mut scene = scene_with(vec![idle("/c/0.jsonl", 0, t0()), f1.clone()], cap);
    let mut r = build(100, 40, vec![]);
    let mut now = t0();
    r.render(&scene, now).unwrap();
    r.navigate_floor(1, now);
    assert!(r.transition().is_some());

    scene.agents.remove(&f1.agent_id);
    now += Duration::from_millis(100);
    r.render(&scene, now).unwrap();
    assert!(
        r.transition().is_none(),
        "transition to a vanished floor must cancel (no infinite slide)"
    );
    assert_eq!(r.current_floor(), 0);
}

#[test]
fn floor_buffers_grow_on_overflow() {
    let cap = 16;
    let mut r = build(100, 40, vec![]);
    let now = t0();
    let one = scene_with(vec![idle("/g/0.jsonl", 0, t0())], cap);
    r.render(&one, now).unwrap();
    assert!(r.session.floor(1).is_none(), "only one floor allocated");

    let two = scene_with(
        vec![
            idle("/g/0.jsonl", 0, t0()),
            slot(AgentId::from_transcript_path("/g/1.jsonl"), 1, cap, t0()),
        ],
        cap,
    );
    r.render(&two, now).unwrap();
    assert!(
        r.session.floor(1).is_some(),
        "floor-1 state allocated after overflow"
    );
}

#[test]
fn per_floor_layout_seeds_differ() {
    let scene = two_floor_scene();
    let mut r = build(100, 40, vec![]);
    let mut now = t0();
    r.render(&scene, now).unwrap();
    let seed0 = r.current_floor_seed();
    r.navigate_floor(1, now);
    render_until_settled(&mut r, &scene, &mut now, 1);
    assert_ne!(
        seed0,
        r.current_floor_seed(),
        "each floor must use a distinct layout seed"
    );
}

#[test]
fn transition_at_narrow_terminal_paints_no_agents_no_panic() {
    let cap = 16;
    // A floor-1 occupant so num_floors==2 and navigate_floor(1) has a destination.
    let scene = scene_with(
        vec![
            idle("/narrow/0.jsonl", 0, t0() - Duration::from_secs(120)),
            slot(
                AgentId::from_transcript_path("/narrow/1.jsonl"),
                1,
                cap,
                t0(),
            ),
        ],
        cap,
    );
    // 30 cols: scene_rect 30×39 passes the `MIN_SCENE_*` transition gate; buf_w=30
    // fails compute_with_seed's office minimum.
    let mut r = TuiRenderer::new(
        Terminal::new(TestBackend::new(30, 40)).expect("test backend"),
        normal_theme(),
        vec![],
        pack_arc(),
    );
    let mut now = t0();
    r.render(&scene, now).expect("render at 30 cols");
    r.navigate_floor(1, now);
    assert!(r.transition().is_some(), "navigation begins a transition");
    now += Duration::from_millis(PAINT_FRAME_MS);
    r.render(&scene, now)
        .expect("transition render at a narrow terminal must not panic");
    assert!(
        r.transition().is_some(),
        "the slide is still active this frame"
    );

    let bg = normal_theme().surface.bg_fallback;
    let from = r.floor_buf(0).expect("floor-0 buffer allocated");
    let non_bg = (0..from.height())
        .flat_map(|y| (0..from.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| from.get(x, y) != bg)
        .count();
    assert_eq!(
        non_bg, 0,
        "compute failed at 30 cols ⇒ no scene/agents painted, buffer stays bg-fallback ({non_bg} stray pixels)"
    );
    assert!(
        !r.coffee_contains(AgentId::from_transcript_path("/narrow/0.jsonl")),
        "a skipped transition floor records no coffee carriers"
    );
}

#[test]
fn footer_shows_source_death_warning() {
    let scene = scene_with(vec![idle("/f/0.jsonl", 0, t0())], 16);
    let mut r = build(140, 44, vec![]);
    let frames = RenderFrames {
        source_warning: Some(
            "claude-code source died — its agents are frozen; restart pixtuoid (see log)".into(),
        ),
        ..Default::default()
    };
    r.set_frames(frames, t0());
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("source died") && text.contains("restart pixtuoid"),
        "the footer must surface a dead source (#157); footer row:\n{}",
        text.lines().last().unwrap_or("")
    );
    r.set_frames(RenderFrames::default(), t0());
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        !text.contains("source died"),
        "footer returns to stats when no source is dead"
    );
}

#[test]
fn source_death_warning_survives_floor_transition() {
    let scene = two_floor_scene();
    let mut r = build(120, 44, vec![]);
    let mut now = t0();
    r.render(&scene, now).unwrap();
    r.set_frames(
        RenderFrames {
            source_warning: Some(
                "claude-code source died — its agents are frozen; restart pixtuoid (see log)"
                    .into(),
            ),
            ..Default::default()
        },
        now,
    );
    r.navigate_floor(1, now);
    now += Duration::from_millis(200); // mid-transition
    r.render(&scene, now).unwrap();
    assert!(r.transition().is_some(), "still mid-transition");
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("source died"),
        "the warning must not vanish during the floor slide"
    );
}

#[test]
fn version_popup_active_during_floor_transition() {
    let scene = two_floor_scene();
    let mut r = build(120, 44, vec![]);
    let mut now = t0();
    r.render(&scene, now).unwrap();
    r.set_frames(
        RenderFrames {
            version_popup: crate::panels::ui_state::VersionPopupFrame {
                open: true,
                ..Default::default()
            },
            ..Default::default()
        },
        now,
    );
    r.navigate_floor(1, now);
    now += Duration::from_millis(200); // mid-transition
    r.render(&scene, now).unwrap();
    assert!(r.transition().is_some(), "still mid-transition");
    assert!(
        r.last_popup_scale() > 0.0,
        "version popup must keep animating through a floor transition"
    );
}

#[test]
fn help_overlay_renders_during_floor_transition() {
    let scene = two_floor_scene();
    let mut r = build(120, 44, vec![]);
    let mut now = t0();
    r.render(&scene, now).unwrap();
    r.set_frames(
        RenderFrames {
            help_open: true,
            ..Default::default()
        },
        now,
    );
    r.navigate_floor(1, now);
    now += Duration::from_millis(200);
    r.render(&scene, now).unwrap();
    assert!(r.transition().is_some());
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("Keyboard"),
        "help overlay must paint over a floor transition"
    );
}

#[test]
fn a_too_small_slide_footers_the_destination_floor() {
    let cap = 16;
    let scene = scene_with(
        vec![
            active("/d/0.jsonl", 0, "Grep x", t0()),
            slot(AgentId::from_transcript_path("/d/1.jsonl"), 1, cap, t0()),
        ],
        cap,
    );
    let mut r = build(120, crate::tui::renderer::MIN_SCENE_HEIGHT, vec![]);
    r.render(&scene, t0()).unwrap();
    r.navigate_floor(1, t0());
    r.render(&scene, t0() + Duration::from_millis(100)).unwrap();
    let text = frame_text(r.frame_buffer());
    let footer = text.lines().last().expect("a footer row");
    assert!(
        footer.contains(" 1/2 ") && footer.contains("F2/2"),
        "{footer}"
    );
    assert!(
        !footer.contains("Grep"),
        "floor 1 tallies floor 0's tool: {footer}"
    );
}

#[test]
fn transition_on_too_small_terminal_clears_state_and_lands() {
    // Under the `MIN_SCENE_*` gate: draw_too_small's bail.
    let scene = two_floor_scene();
    let mut r = build(18, 10, vec![PetKind::Cat]);
    let now = t0();
    r.render(&scene, now).unwrap();
    r.navigate_floor(1, now);
    r.render(&scene, now + Duration::from_millis(100))
        .expect("transition render on a tiny terminal must not panic");
    assert!(r.cached_layout().is_none());
    assert!(r.drawn_pet().is_none());
    assert_eq!(r.last_popup_scale(), 0.0);
    assert!(
        r.transition().is_none(),
        "the too-small gate should land (cancel) the stuck transition"
    );
    assert_eq!(r.current_floor(), 1, "landed on the destination floor");
}

#[test]
fn debug_walkable_getter_reflects_setter() {
    let mut r = build(100, 40, vec![]);
    assert!(!r.debug_walkable());
    r.set_debug_walkable(true);
    assert!(r.debug_walkable());
    r.set_debug_walkable(false);
    assert!(!r.debug_walkable());
}

#[test]
fn already_expired_active_pet_clears_on_render() {
    let scene = scene_with(vec![active("/exp/0.jsonl", 0, "Edit", t0())], 16);
    let mut r = build(100, 40, vec![PetKind::Cat]);
    r.set_active_pet(Some(PetState {
        petted_at: t0() - Duration::from_secs(3600), // long expired
        kind: PetKind::Cat,
        floor_idx: 0,
    }));
    r.render(&scene, t0()).unwrap();
    assert!(
        r.active_pet_ref().is_none(),
        "an already-expired pet state must be cleared on render"
    );
}

#[test]
fn current_floor_clamps_when_floor_count_drops() {
    let cap = 16;
    let two = two_floor_scene();
    let mut r = build(100, 40, vec![]);
    let mut now = t0();
    r.render(&two, now).unwrap();
    r.navigate_floor(1, now);
    render_until_settled(&mut r, &two, &mut now, 1);
    assert_eq!(r.current_floor(), 1);
    let one = scene_with(vec![idle("/clamp/0.jsonl", 0, t0())], cap);
    r.render(&one, now).unwrap();
    assert_eq!(
        r.current_floor(),
        0,
        "current_floor clamps when floors shrink"
    );
}

#[test]
fn theme_picker_renders_during_floor_transition() {
    let scene = two_floor_scene();
    let mut r = build(140, 48, vec![]);
    let mut now = t0();
    r.render(&scene, now).unwrap();
    r.set_frames(
        RenderFrames {
            theme_picker: Some(0),
            ..Default::default()
        },
        now,
    );
    r.navigate_floor(1, now);
    now += Duration::from_millis(200);
    r.render(&scene, now).unwrap();
    assert!(r.transition().is_some(), "still mid-transition");
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("cyberpunk") || text.contains("normal"),
        "theme picker must paint over a floor transition; frame:\n{text}"
    );
}

/// The board's context row is wired in `draw_scene` from the floor breadcrumb
/// and the OFFICE-WIDE gateway: the projected floor scene carries daemons only
/// on floor 0, so a board built from it would drop the `⬢gw` chip upstairs.
#[test]
fn the_wall_board_upstairs_shows_the_breadcrumb_and_the_office_gateway() {
    let theme = pixtuoid_scene::theme::ALL_THEMES[0];
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let cap = 16;
    let mut scene = SceneState::uniform(cap);
    let a = AgentId::from_transcript_path("/h/board0.jsonl");
    let b = AgentId::from_transcript_path("/h/board1.jsonl");
    scene.agents.insert(a, slot(a, 0, 0, t0));
    scene.agents.insert(b, slot(b, 1, cap, t0));
    scene.insert_daemon(
        pixtuoid_core::source::openclaw::SOURCE_NAME,
        pixtuoid_core::state::DaemonInstanceId::new("18789").expect("non-empty"),
        pixtuoid_core::state::DaemonPresence {
            liveness: pixtuoid_core::state::DaemonLiveness::UP,
            active_sessions: 0,
            last_seen: t0,
            entered_at: t0,
            in_flight_turns: Default::default(),
            current_pid: Some(1),
        },
    );

    let term = Terminal::new(TestBackend::new(120, 44)).expect("test backend");
    let mut r = TuiRenderer::new(term, theme, vec![], pack_arc());
    let mut now = t0;
    r.render(&scene, now).expect("render");
    r.navigate_floor(1, now);
    render_until_settled(&mut r, &scene, &mut now, 1);

    // Above the footer row, which carries its own floor indicator.
    let text = frame_text(r.frame_buffer());
    let rows: Vec<&str> = text.lines().collect();
    let office = rows[..rows.len() - usize::from(crate::tui::renderer::FOOTER_ROWS)].join("\n");
    assert!(
        office.contains("F2/2"),
        "board breadcrumb names floor 2 of 2:\n{office}"
    );
    assert!(
        office.contains("\u{2b22}gw"),
        "board keeps the office-wide gateway chip upstairs:\n{office}"
    );
}

/// Every painter's board for a floor comes out of the one builder: a one-floor
/// office's TUI board is the floating window's, and an upper floor, whose
/// projection carries no daemon, still shows the office's gateway.
#[test]
fn every_painter_shows_the_one_board_of_a_floor() {
    use pixtuoid_scene::anim::Motion;
    use pixtuoid_scene::floor::project_floor_scene;
    let now = t0() + Duration::from_secs(90);
    let with_gateway = |mut scene: SceneState| {
        scene.insert_daemon(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            pixtuoid_core::state::DaemonInstanceId::new("18789").expect("non-empty"),
            pixtuoid_core::state::DaemonPresence {
                liveness: pixtuoid_core::state::DaemonLiveness::UP,
                active_sessions: 1,
                last_seen: now,
                entered_at: t0(),
                in_flight_turns: Default::default(),
                current_pid: Some(1),
            },
        );
        scene
    };
    let mut r = build(120, 40, vec![]);
    let mut tui = |scene: &SceneState, floor: usize| {
        r.session.prepare(scene, now);
        r.session.navigate(floor, now);
        r.session.cancel_slide();
        let drawn = project_floor_scene(scene, floor);
        let ctx = r.chrome.footer(&r.session, scene);
        pixtuoid_scene::neon_sign::wall_board(&drawn, ctx.gateway, ctx.floor, Motion::Full, now)
    };

    let one_floor = with_gateway(scene_with(vec![idle("/b/0.jsonl", 0, t0())], 16));
    let floating = pixtuoid_scene::neon_sign::wall_board(
        &one_floor,
        pixtuoid_scene::tally::office_gateway(&one_floor),
        None,
        Motion::Full,
        now,
    );
    assert_eq!(tui(&one_floor, 0), floating, "the floating window's place");

    let upper = tui(&with_gateway(two_floor_scene()), 1);
    let context: Vec<_> = upper.context.iter().map(|s| s.text().trim()).collect();
    assert!(context.contains(&"F2/2"), "{context:?}");
    assert!(
        context.iter().any(|t| t.contains("gw")),
        "floor 2 lost the office's gateway chip: {context:?}"
    );
}
