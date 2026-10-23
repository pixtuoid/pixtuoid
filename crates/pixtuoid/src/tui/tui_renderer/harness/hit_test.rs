use super::*;
use crate::tui::geometry::CellArea;
use crate::tui::hit_test::SceneHit;
use crate::tui::renderer::{DrawCtx, draw_scene};
use pixtuoid_scene::display::HoverTarget;
use pixtuoid_scene::floor::PerFloor;

#[test]
fn furniture_hit_test_resolves_against_rendered_layout() {
    let scene = scene_with(vec![idle("/hit/0.jsonl", 0, t0())], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, pack(), t0()).unwrap();
    let layout = r.cached_layout().expect("layout");
    let desk = layout.home_desks[0];
    let hit = crate::tui::hit_test::hit_test_furniture(
        layout,
        CellArea::half_block(desk.x + 4, CellArea::row_of(desk.y) + 1),
    );
    assert_eq!(
        hit,
        Some("Desk"),
        "a desk pixel should hit the Desk furniture in the cached layout"
    );
}

#[test]
fn coffee_machine_hit_test_resolves_on_pantry() {
    use pixtuoid_scene::layout::WaypointKind;
    let scene = scene_with(vec![idle("/cm/0.jsonl", 0, t0())], 16);
    let mut r = build(140, 48, vec![]);
    r.render(&scene, pack(), t0()).unwrap();
    let layout = r.cached_layout().expect("layout");
    let pantry = layout
        .waypoints
        .iter()
        .find(|w| w.kind == WaypointKind::Pantry)
        .expect("a 140×48 office must lay out a pantry");
    let cx = pantry.pos.x;
    let cy = CellArea::row_of(pantry.pos.y);
    let mut found = false;
    for dx in -14i32..=14 {
        for dy in -4i32..=4 {
            let mx = (i32::from(cx) + dx).max(0) as u16;
            let my = (i32::from(cy) + dy).max(0) as u16;
            if crate::tui::hit_test::hit_test_coffee_machine(layout, CellArea::half_block(mx, my)) {
                found = true;
            }
        }
    }
    assert!(
        found,
        "the coffee machine should be hit-testable somewhere on the pantry counter"
    );
}

#[test]
fn pet_hit_test_resolves_at_pet_position() {
    let scene = scene_with(vec![active("/ph/0.jsonl", 0, "Edit", t0())], 16);
    let mut r = build(120, 44, vec![PetKind::Cat]);
    r.render(&scene, pack(), t0()).unwrap();
    let PetHover { centre: pos, .. } = r.drawn_pet().expect("pet placed");
    assert!(
        matches!(
            r.scene_hit_at(pos.x, CellArea::row_of(pos.y)),
            Some(SceneHit::Figure(HoverTarget::Pet(_)))
        ),
        "clicking the pet's own position should hit it"
    );
}

#[test]
fn hovering_an_agent_marks_its_label() {
    let mut s = idle("/hov/0.jsonl", 0, t0() - Duration::from_secs(300));
    s.label = "HOVERME".into();
    let id = s.agent_id;
    let scene = scene_with(vec![s], 16);
    let mut r = build(140, 48, vec![]);
    r.render(&scene, pack(), t0()).unwrap();
    hover_agent(&mut r, id);
    r.render(&scene, pack(), t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("\u{25b8}HOVERME") || text.contains("\u{25b8}"),
        "hovering an agent should add the ▸ marker to its label; frame:\n{text}"
    );
}

#[test]
fn click_hit_test_follows_a_walking_sprite() {
    let id = pixtuoid_core::AgentId::from_transcript_path("/w/0.jsonl");
    let mut s = idle("/w/0.jsonl", 0, t0() - Duration::from_secs(300));
    let scene = scene_with(vec![s.clone()], 16);
    let mut r = build(192, 80, vec![]);
    r.render(&scene, pack(), t0()).unwrap();
    let layout = r.cached_layout().expect("layout");
    let seat = pixtuoid_scene::sim::seated_top_left(
        layout.home_desks[0],
        pixtuoid_scene::layout::CHARACTER_SPRITE_W,
        layout.desk_facing(pixtuoid_core::state::FloorLocalDeskIndex(0)),
    );
    let (dx, dy) = (
        seat.x + pixtuoid_scene::layout::CHARACTER_SPRITE_W / 2,
        CellArea::row_of(seat.y + pixtuoid_scene::layout::CHARACTER_SPRITE_H / 2),
    );
    assert_eq!(r.hit_test_agent_at(dx, dy), Some(id));

    s.exiting_at = Some(t0());
    let scene = scene_with(vec![s], 16);
    // Mid-exit-walk, inside EXIT_GRACE_WINDOW — off the desk box, not yet GC'd.
    let walk_now = t0() + Duration::from_millis(1500);
    r.render(&scene, pack(), walk_now).unwrap();
    let drawn = drawn(&r, &scene, id, walk_now).top_left;
    assert_eq!(
        r.hit_test_agent_at(drawn.x, CellArea::row_of(drawn.y)),
        Some(id)
    );
    assert_eq!(
        r.hit_test_agent_at(dx, dy),
        None,
        "the sprite walked off its desk"
    );
}

/// A sprite's top-left and its frame's size, in logical pixels.
#[derive(Debug, Clone, Copy)]
struct Sprite {
    top_left: pixtuoid_scene::layout::Point,
    w: u16,
    h: u16,
}

/// Where the painter blits `id`'s sprite at `now`, sized by the pack's frame.
fn drawn(r: &TuiRenderer<TestBackend>, scene: &SceneState, id: AgentId, now: SystemTime) -> Sprite {
    let layout = r.cached_layout().expect("rendered layout");
    let stepped = pixtuoid_scene::floor::FloorSession::new(std::sync::Arc::new(pack().clone()))
        .step(
            pixtuoid_scene::floor::FloorInputs {
                scene,
                pack: pack(),
                now,
                floor: pixtuoid_scene::floor::FloorMeta::for_floor(0, 1),
                pets: pixtuoid_scene::floor::PetInputs::default(),
            },
            pixtuoid_scene::layout::Size {
                w: layout.buf_w,
                h: layout.buf_h,
            },
        )
        .expect("steppable floor");
    let frame = &stepped.frame;
    let c = frame
        .characters
        .iter()
        .find(|c| frame.agents[c.agent_idx].agent_id == id)
        .expect("the agent is drawn");
    let art = pack().piece(c.anim_name).frame_at(c.frame_idx);
    let art = (art.width(), art.height());
    Sprite {
        top_left: c.top_left,
        w: art.0,
        h: art.1,
    }
}

/// Whether the half-block cell `(col, row)` shows a pixel of `sprite`.
fn cell_shows(sprite: Sprite, col: u16, row: u16) -> bool {
    CellArea::half_block(col, row).overlaps(sprite.top_left, sprite.w, sprite.h)
}

/// Cells swept past each edge of the sprite, so the sweep sees its misses too.
const SWEEP_MARGIN: u16 = 2;

/// Probe offsets across `breath_offset_y`'s `CYCLE_MS` cycle, spaced under its
/// half, so one lands in the bobbed half whatever the agent's phase.
const BREATH_PROBES_MS: [u64; 10] = [
    0, 500, 1_000, 1_500, 2_000, 2_500, 3_000, 3_500, 4_000, 4_500,
];

#[test]
fn a_breathing_sitter_is_hit_at_its_drawn_cells_not_its_seat_top_left() {
    let (cols, rows) = (140, 48);
    let mut s = active("/breath/0.jsonl", 0, "Edit", t0() - Duration::from_secs(60));
    s.label = "BREATH".into();
    let id = s.agent_id;
    let scene = scene_with(vec![s], 16);
    let mut r = build(cols, rows, vec![]);
    r.render(&scene, pack(), t0()).unwrap();
    let layout = r.cached_layout().expect("layout").clone();
    let seat = pixtuoid_scene::sim::seated_top_left(
        layout.home_desks[0],
        pixtuoid_scene::layout::CHARACTER_SPRITE_W,
        layout.desk_facing(pixtuoid_core::state::FloorLocalDeskIndex(0)),
    );
    let (now, drawn) = BREATH_PROBES_MS
        .into_iter()
        .map(|ms| t0() + Duration::from_millis(ms))
        .map(|now| (now, drawn(&r, &scene, id, now)))
        .find(|&(_, drawn)| drawn.top_left != seat)
        .expect("within one breath cycle the sitter bobs off its seat top-left");
    r.render(&scene, pack(), now).unwrap();
    let seated = Sprite {
        top_left: seat,
        ..drawn
    };

    let mut moved = None;
    for row in CellArea::row_of(seat.y).saturating_sub(SWEEP_MARGIN)
        ..=CellArea::row_of(seat.y + drawn.h) + SWEEP_MARGIN
    {
        for col in seat.x.saturating_sub(SWEEP_MARGIN)..seat.x + drawn.w + SWEEP_MARGIN {
            let shows = cell_shows(drawn, col, row);
            assert_eq!(
                r.hit_test_agent_at(col, row) == Some(id),
                shows,
                "cell ({col},{row}) against the sprite drawn at {drawn:?}"
            );
            if shows != cell_shows(seated, col, row) {
                moved = Some(((col, row), shows));
            }
        }
    }
    let ((col, row), shows) = moved.expect("the bob moves the sprite across a cell edge");
    r.set_mouse_pos(Some((col, row)));
    r.render(&scene, pack(), now).unwrap();
    assert_eq!(
        frame_text(r.frame_buffer()).contains('\u{25b8}'),
        shows,
        "hovering ({col},{row}) marks the label iff the cell shows the sprite"
    );
}

#[test]
fn overlapping_agents_hit_the_one_painted_on_top() {
    let (cols, rows) = (140, 48);
    // Two arrivals of one instant walk out of the elevator on one spot; a
    // second cwd dresses the second in another outfit, so the pixels tell
    // them apart.
    let mut a = active("/overlap/a.jsonl", 0, "Edit", t0());
    a.label = "ALPHA".into();
    let mut b = active("/overlap/b.jsonl", 1, "Edit", t0());
    b.label = "BRAVO".into();
    b.cwd = Arc::from(Path::new("/elsewhere"));
    let now = t0() + Duration::from_millis(400);
    let render = |agents: Vec<AgentSlot>| {
        let scene = scene_with(agents, 16);
        let mut r = build(cols, rows, vec![]);
        r.render(&scene, pack(), now).unwrap();
        (r, scene)
    };
    let (mut both, scene) = render(vec![a.clone(), b.clone()]);
    let (solo_a, _) = render(vec![a.clone()]);
    let (solo_b, _) = render(vec![b.clone()]);
    let px = |r: &TuiRenderer<TestBackend>, x, y| r.floor_buf(0).expect("floor buf").get(x, y);
    let (drawn_a, drawn_b) = (
        drawn(&both, &scene, a.agent_id, now),
        drawn(&both, &scene, b.agent_id, now),
    );
    // Where the two sprites cover each other the frame shows only the top one.
    let (x0, y0) = (
        drawn_a.top_left.x.max(drawn_b.top_left.x),
        drawn_a.top_left.y.max(drawn_b.top_left.y),
    );
    let (x1, y1) = (
        (drawn_a.top_left.x + drawn_a.w).min(drawn_b.top_left.x + drawn_b.w),
        (drawn_a.top_left.y + drawn_a.h).min(drawn_b.top_left.y + drawn_b.h),
    );
    let (x, y, top) = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .find_map(|(x, y)| {
            let (pa, pb) = (px(&solo_a, x, y), px(&solo_b, x, y));
            (pa != pb).then(|| {
                let shown = px(&both, x, y);
                let top = if shown == pa {
                    a.agent_id
                } else {
                    assert_eq!(shown, pb, "({x},{y}) shows neither agent");
                    b.agent_id
                };
                (x, y, top)
            })
        })
        .expect("the two arrivals overlap in differently dressed pixels");
    assert_ne!(
        top,
        a.agent_id.min(b.agent_id),
        "premise: the agent on top is not the first by AgentId"
    );
    assert_eq!(both.hit_test_agent_at(x, CellArea::row_of(y)), Some(top));
    both.set_mouse_pos(Some((x, CellArea::row_of(y))));
    both.render(&scene, pack(), now).unwrap();
    let hovered = scene.agents[&top].label.clone();
    assert!(
        frame_text(both.frame_buffer()).contains(&format!("\u{25b8}{hovered}")),
        "hover marks the label of the agent on top"
    );
}

#[test]
fn the_drawn_geometry_answers_every_cell_as_the_half_block_does() {
    let now = t0() + Duration::from_secs(20);
    let mut scene = scene_with(
        (0..6)
            .map(|i| active(&format!("/geo/{i}.jsonl"), i, "Edit", t0()))
            .collect(),
        16,
    );
    scene.insert_daemon(
        pixtuoid_core::source::openclaw::SOURCE_NAME,
        pixtuoid_core::state::DaemonInstanceId::new("18789").expect("non-empty"),
        pixtuoid_core::state::DaemonPresence {
            liveness: pixtuoid_core::state::DaemonLiveness::UP,
            active_sessions: 1,
            last_seen: now,
            entered_at: t0(),
            in_flight_runs: Default::default(),
            current_pid: Some(1),
        },
    );
    let cat = pixtuoid_scene::pet::Pet::defaulted(PetKind::Cat);
    for (cols, rows) in [(80, 30), (120, 52), (157, 41)] {
        let mut term = Terminal::new(TestBackend::new(cols, rows)).expect("test backend");
        let mut floor = PerFloor::new(pack_arc());
        let mut office = pixtuoid_scene::floor::PerOffice::new();
        let mut ctx = DrawCtx::offscreen(
            &mut floor,
            office.stores(),
            normal_theme(),
            &scene,
            pack(),
            now,
            FloorMeta::ground(),
        );
        ctx.world.pets.pet = Some(&cat);
        let drawn = draw_scene(&mut term, &mut ctx)
            .expect("draw")
            .expect("drawn");
        let geometry = crate::tui::renderer::classic_geometry(term.size().expect("a size"));
        let hits = |at: CellArea| drawn.at(at.bounds(), None);
        let mut seen = [false; 5];
        for (col, row) in (0..rows).flat_map(|row| (0..cols).map(move |col| (col, row))) {
            let half_block = hits(CellArea::half_block(col, row));
            assert_eq!(
                geometry.area_at(col, row).map(hits),
                Some(half_block),
                "{cols}x{rows} cell ({col},{row})"
            );
            for (seen, hit) in seen.iter_mut().zip([
                matches!(half_block, Some(SceneHit::Figure(HoverTarget::Agent(_)))),
                half_block == Some(SceneHit::Coffee),
                matches!(half_block, Some(SceneHit::Figure(HoverTarget::Pet(_)))),
                matches!(half_block, Some(SceneHit::Figure(HoverTarget::Mascot(_)))),
                matches!(half_block, Some(SceneHit::Furniture(_))),
            ]) {
                *seen |= hit;
            }
        }
        assert_eq!(
            seen, [true; 5],
            "{cols}x{rows}: every kind is hit somewhere"
        );
    }
}

/// A click on an agent focuses it and a click on the pet pets it, through the
/// mouse handler itself.
#[test]
fn a_click_focuses_an_agent_and_pets_the_pet() {
    a_click_acts_on_what_it_hits(&mut build(140, 48, vec![PetKind::Cat]));
}

/// [`a_click_focuses_an_agent_and_pets_the_pet`] on `r`, a renderer with a cat.
pub(super) fn a_click_acts_on_what_it_hits<B>(r: &mut TuiRenderer<B>)
where
    B: Backend<Error: Send + Sync + 'static> + std::borrow::Borrow<TestBackend>,
{
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let now = t0() + Duration::from_secs(20);
    let id = AgentId::from_transcript_path("/click/0.jsonl");
    let scene = scene_with(vec![active("/click/0.jsonl", 0, "Edit", t0())], 16);
    r.render(&scene, pack(), now).unwrap();
    let (_tx, scene_rx) = tokio::sync::watch::channel(Arc::new(scene));
    let mut ui = crate::panels::ui_state::UiState::new(
        normal_theme(),
        crate::panels::welcome::WelcomeUi::from_detected(&[]),
        std::path::PathBuf::from("/tmp/sock"),
        None,
        crate::doctor::DriftSeen::default(),
    );
    let cell = |r: &TuiRenderer<B>, hits: &dyn Fn(&SceneHit<'_>) -> bool| {
        r.frame_buffer()
            .area()
            .positions()
            .find(|p| r.scene_hit_at(p.x, p.y).is_some_and(|h| hits(&h)))
            .map(|p| (p.x, p.y))
    };
    let event = |kind, (column, row)| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    // A click is a press and its release; the release acts.
    let click = |r: &mut TuiRenderer<B>,
                 ui: &mut crate::panels::ui_state::UiState,
                 at,
                 focus: &mut dyn FnMut(&AgentSlot),
                 now| {
        crate::tui::handle_mouse_event(
            event(MouseEventKind::Down(MouseButton::Left), at),
            ui,
            r,
            &scene_rx,
            |_| panic!("a press acts on nothing"),
            now,
        );
        crate::tui::handle_mouse_event(
            event(MouseEventKind::Up(MouseButton::Left), at),
            ui,
            r,
            &scene_rx,
            |slot| focus(slot),
            now,
        );
    };
    let on_agent = cell(r, &|h| matches!(h, SceneHit::Figure(HoverTarget::Agent(_))))
        .expect("an agent's cell");
    let mut focused = None;
    click(
        r,
        &mut ui,
        on_agent,
        &mut |slot| focused = Some(slot.agent_id),
        now,
    );
    assert_eq!(focused, Some(id), "the click focuses the agent it hits");
    let on_pet =
        cell(r, &|h| matches!(h, SceneHit::Figure(HoverTarget::Pet(_)))).expect("the pet's cell");
    assert!(
        r.active_pet_ref().is_none(),
        "premise: nobody petted it yet"
    );
    let mut pet = |r: &mut TuiRenderer<B>, at| {
        click(
            r,
            &mut ui,
            on_pet,
            &mut |_| panic!("the pet is no agent"),
            at,
        );
        r.active_pet_ref().map(|p| p.petted_at)
    };
    assert_eq!(pet(r, now), Some(now), "the click pets the cat");
    assert_eq!(
        pet(r, now + Duration::from_millis(1)),
        Some(now),
        "a second click while it purrs pets nothing new"
    );
    // A press on the agent that drags lifts it: the next frame draws it
    // under the pointer, and a release clicks nothing but sets it down.
    let scene = scene_rx.borrow().clone();
    let away = (on_agent.0.saturating_sub(12), on_agent.1 + 4);
    let send = |r: &mut TuiRenderer<B>, ui: &mut crate::panels::ui_state::UiState, kind, at| {
        crate::tui::handle_mouse_event(
            event(kind, at),
            ui,
            r,
            &scene_rx,
            |_| panic!("a drag focuses nothing"),
            now,
        );
    };
    send(
        r,
        &mut ui,
        MouseEventKind::Down(MouseButton::Left),
        on_agent,
    );
    send(r, &mut ui, MouseEventKind::Drag(MouseButton::Left), away);
    r.render(&scene, pack(), now + Duration::from_millis(50))
        .unwrap();
    assert!(
        matches!(r.scene_hit_at(away.0, away.1), Some(SceneHit::Figure(HoverTarget::Agent(hit))) if *hit == id),
        "the lifted agent hangs under the pointer"
    );
    send(r, &mut ui, MouseEventKind::Up(MouseButton::Left), away);
    for tenth in 1..=300 {
        r.render(&scene, pack(), now + Duration::from_millis(100 * tenth))
            .unwrap();
    }
    assert!(
        matches!(r.scene_hit_at(on_agent.0, on_agent.1), Some(SceneHit::Figure(HoverTarget::Agent(hit))) if *hit == id),
        "set down, it walked back to its desk"
    );
    // A carry whose release never came (the button let go outside the
    // terminal) ends at the next press, even one where no office is drawn.
    send(
        r,
        &mut ui,
        MouseEventKind::Down(MouseButton::Left),
        on_agent,
    );
    send(r, &mut ui, MouseEventKind::Drag(MouseButton::Left), away);
    assert!(r.pointer.carrying(), "premise: the agent is lifted");
    r.screen.blank(r.terminal.size().expect("a size"));
    send(r, &mut ui, MouseEventKind::Down(MouseButton::Left), away);
    assert!(
        !r.pointer.carrying(),
        "a press over no drawn office still sets the lost carry down"
    );
}

/// The click handler acts on `scene_hit_at`, so wherever it names a figure or
/// the coffee machine, the tooltip there names the same thing.
#[test]
fn what_the_tooltip_names_is_what_a_click_acts_on() {
    let (cols, rows) = (140, 48);
    the_tooltip_names_what_a_click_acts_on(&mut build(cols, rows, vec![PetKind::Cat]), |a| {
        format!("\u{25b8}{}", a.label)
    });
}

/// [`what_the_tooltip_names_is_what_a_click_acts_on`] on `r`, a renderer with
/// a cat, where a hovered agent shows `agent_says`.
pub(super) fn the_tooltip_names_what_a_click_acts_on<B>(
    r: &mut TuiRenderer<B>,
    agent_says: impl Fn(&AgentSlot) -> String,
) where
    B: Backend<Error: Send + Sync + 'static> + std::borrow::Borrow<TestBackend>,
{
    let now = t0() + Duration::from_secs(20);
    let mut scene = scene_with(
        (0..3)
            .map(|i| {
                let mut s = active(&format!("/same/{i}.jsonl"), i, "Edit", t0());
                s.label = format!("AGENT{i}").into();
                s
            })
            .collect(),
        16,
    );
    for port in ["18789", "18790"] {
        scene.insert_daemon(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            pixtuoid_core::state::DaemonInstanceId::new(port).expect("non-empty"),
            pixtuoid_core::state::DaemonPresence {
                liveness: pixtuoid_core::state::DaemonLiveness::UP,
                active_sessions: 1,
                last_seen: now,
                entered_at: t0(),
                in_flight_runs: Default::default(),
                current_pid: Some(1),
            },
        );
    }
    r.render(&scene, pack(), now).unwrap();
    let (mut named, mut seen) = (Vec::new(), [false; 4]);
    for (col, row) in r.frame_buffer().area().positions().map(|p| (p.x, p.y)) {
        let (kind, says) = match r.scene_hit_at(col, row) {
            Some(SceneHit::Figure(HoverTarget::Agent(id))) => (0, agent_says(&scene.agents[id])),
            Some(SceneHit::Figure(HoverTarget::Pet(pet))) => (
                1,
                if pet.anim == PetKind::Cat.sleep_anim() {
                    "sleeping"
                } else if pet.anim == PetKind::Cat.sit_anim() {
                    "Pet me!"
                } else {
                    "Office Cat"
                }
                .to_string(),
            ),
            Some(SceneHit::Figure(HoverTarget::Mascot(key))) => {
                (2, format!("OpenClaw:{} gateway", key.instance().as_str()))
            }
            Some(SceneHit::Coffee) => (3, "Buy Ivan a coffee".to_string()),
            Some(SceneHit::Star | SceneHit::Furniture(_)) | None => continue,
        };
        seen[kind] = true;
        named.push(((col, row), says));
    }
    assert_eq!(
        seen, [true; 4],
        "premise: agents, the cat, a gateway and the coffee"
    );
    // A stride, not every cell: each probe is a full render.
    for ((col, row), says) in named.iter().step_by(3) {
        r.set_mouse_pos(Some((*col, *row)));
        r.render(&scene, pack(), now).unwrap();
        let text = frame_text(r.frame_buffer());
        assert!(
            text.contains(says.as_str()),
            "cell ({col},{row}) acts on {says:?}; frame:\n{text}"
        );
    }
}
