use crate::character::test_support::make_slot;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use pixtuoid_core::state::{ActivityState, GlobalDeskIndex};
use pixtuoid_core::{AgentId, AgentSlot, SceneState};

use super::*;
use crate::floor::{FloorMeta, PerOffice, PetInputs};
use crate::layout::Size;

const SIZE: Size = Size { w: 192, h: 160 };
const VIEWPORT: Viewport = Viewport::full(SIZE.w, SIZE.h);

fn theme() -> &'static Theme {
    crate::theme::theme_by_name("normal").expect("normal theme")
}

/// Agents entering (the door's walk) and long idle (wander trips, coffee among them).
fn office(t0: SystemTime) -> SceneState {
    let mut scene = SceneState::uniform(8);
    for i in 0..6 {
        let id = AgentId::from_transcript_path(&format!("/look/{i}.jsonl"));
        let started = if i < 2 {
            t0
        } else {
            t0 - Duration::from_secs(600)
        };
        scene.agents.insert(
            id,
            AgentSlot {
                session_id: std::sync::Arc::from(format!("s{i}").as_str()),
                cwd: std::sync::Arc::from(Path::new("/repo")),
                label: format!("a{i}").into(),
                state_started_at: started,
                created_at: started,
                last_event_at: started,
                desk_index: GlobalDeskIndex(i),
                ..make_slot(id, ActivityState::Idle)
            },
        );
    }
    scene
}

fn inputs(scene: &SceneState, now: SystemTime) -> RenderInputs<'_> {
    RenderInputs {
        world: FloorInputs {
            scene,
            now,
            floor: FloorMeta::ground(),
            pets: PetInputs::default(),
        },
        theme: theme(),
        viewport: VIEWPORT,
        place: Place::default(),
        debug_walkable: false,
        dim: crate::look::Dim::NONE,
    }
}

/// Lift `held` where `look` draws it on `inputs`' viewport and carry it to the
/// middle of each canvas edge and to each corner, handing `check` each gesture
/// with the frame it renders and the raster that drew it. Each carry must move
/// the figure off where it rested, so a lift that grips nothing fails here
/// instead of leaving `check` nothing to test.
fn carry_to_each_edge(
    (floor, office): (&mut PerFloor, &mut PerOffice),
    (look, inputs): (Look, RenderInputs<'_>),
    held: crate::interact::Figure,
    mut check: impl FnMut(&crate::interact::Gesture, &Rendered, &Raster),
) {
    use crate::interact::{Figure, Gesture};
    use crate::layout::Point;
    let drawn = |rendered: &Rendered| {
        rendered
            .hits
            .figures()
            .find(|(target, _)| Figure::of(target) == held)
            .map(|(_, at)| at)
            .expect("the held figure is drawn")
    };
    let rest = drawn(&render(floor, office.stores(), look, inputs).expect("lays out"));
    let Size { w, h } = inputs.viewport.size;
    let edges = [
        (0, 0),
        (w / 2, 0),
        (w - 1, 0),
        (w - 1, h / 2),
        (w - 1, h - 1),
        (w / 2, h - 1),
        (0, h - 1),
        (0, h / 2),
    ];
    let lift = Gesture::Lift {
        figure: held.clone(),
        at: Point {
            x: rest.x + rest.width / 2,
            y: rest.y + rest.height / 2,
        },
    };
    let gestures = std::iter::once(lift).chain(edges.map(|(x, y)| Gesture::Carry(Point { x, y })));
    for gesture in gestures {
        floor.grip(&gesture);
        let rendered = render(floor, office.stores(), look, inputs).expect("lays out");
        if matches!(gesture, Gesture::Carry(_)) {
            assert_ne!(
                drawn(&rendered),
                rest,
                "{look:?} {gesture:?}: {held:?} never left where it rested"
            );
        }
        check(&gesture, &rendered, &floor.raster);
    }
}

/// No figure is drawn under an inset: in either look, frame after frame of
/// agents walking in and wandering off beside a cat and a gateway's mascot,
/// and with each of the three carried to each edge and corner of the canvas,
/// every box [`Hits::figures`] reports lies inside the insets.
#[test]
fn no_figure_is_drawn_under_an_inset() {
    use crate::hit::HoverTarget;
    use crate::interact::Figure;
    use crate::layout::Insets;
    use pixtuoid_core::source::daemon::{DaemonInstanceKey, DaemonPresenceUpdate, apply_presence};
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut scene = office(t0);
    let gateway = DaemonInstanceKey::new(
        pixtuoid_core::source::openclaw::SOURCE_NAME,
        pixtuoid_core::state::DaemonInstanceId::new("18789".to_string()).expect("id"),
    );
    apply_presence(
        &mut scene,
        &gateway,
        DaemonPresenceUpdate::GatewayUp { pid: Some(7) },
        t0 - Duration::from_secs(3_600),
    );
    let cat = crate::pet::Pet::defaulted(crate::pet::PetKind::Cat);
    let viewport = Viewport {
        size: SIZE,
        insets: Insets {
            top: 9,
            right: 7,
            bottom: 11,
            left: 5,
        },
    };
    let at = |now| {
        let bare = inputs(&scene, now);
        RenderInputs {
            world: FloorInputs {
                pets: PetInputs {
                    pet: Some(&cat),
                    petting: None,
                },
                ..bare.world
            },
            viewport,
            place: Place {
                gateway: crate::tally::office_gateway(&scene),
                floor: None,
            },
            ..bare
        }
    };
    let inner = viewport.inner().expect("an office inside the insets");
    let inside = |b: Bounds| {
        b.x >= inner.x
            && b.y >= inner.y
            && b.x + b.width <= inner.x + inner.width
            && b.y + b.height <= inner.y + inner.height
    };
    let looks = [
        Look::HALF_BLOCKS,
        Look::Cutaway {
            scale: RenderScale::new(2).expect("nonzero"),
            text: WorldText::Baked,
        },
    ];
    for look in looks {
        let (mut floor, mut office) = (
            PerFloor::new(Arc::new(crate::pack::test_office())),
            PerOffice::new(),
        );
        let mut last: Vec<(HoverTarget, Bounds)> = Vec::new();
        let (mut walking_frames, mut pet_frames, mut mascot_frames) = (0, 0, 0);
        for step in 0..600u64 {
            let now = t0 + Duration::from_millis(step * 250);
            let rendered = render(&mut floor, office.stores(), look, at(now)).expect("lays out");
            let figures: Vec<_> = rendered
                .hits
                .figures()
                .map(|(target, at)| (target.clone(), at))
                .collect();
            for (target, at) in &figures {
                assert!(
                    inside(*at),
                    "{look:?} step {step}: {target:?} drawn at {at:?}, under an inset of {inner:?}"
                );
            }
            walking_frames += usize::from(
                figures
                    .iter()
                    .any(|f| last.iter().any(|l| l.0 == f.0 && l.1 != f.1)),
            );
            pet_frames += usize::from(figures.iter().any(|f| matches!(f.0, HoverTarget::Pet(_))));
            mascot_frames += usize::from(
                figures
                    .iter()
                    .any(|f| matches!(f.0, HoverTarget::Mascot(_))),
            );
            last = figures;
        }
        assert!(
            walking_frames > 0 && pet_frames > 0 && mascot_frames > 0,
            "{look:?}: {walking_frames} walking, {pet_frames} pet and {mascot_frames} mascot frames, \
             so the insets went untested"
        );
        let agent = *scene.agents.keys().next().expect("an agent");
        let now = t0 + Duration::from_secs(600);
        for held in [
            Figure::Agent(agent),
            Figure::Pet(cat.kind),
            Figure::Mascot(gateway.clone()),
        ] {
            carry_to_each_edge(
                (&mut floor, &mut office),
                (look, at(now)),
                held,
                |gesture, rendered, _| {
                    for (target, at) in rendered.hits.figures() {
                        assert!(
                            inside(at),
                            "{look:?} {gesture:?}: {target:?} drawn at {at:?}, under an inset of {inner:?}"
                        );
                    }
                },
            );
        }
    }
}

/// No world text is set under an inset: under every inset of the sweep, in
/// either look, with an agent carried to each edge and corner of the canvas,
/// each badge, the bubble it hangs and each sign bakes only inside the insets,
/// and the host sets each in cells that show what lies inside them.
#[test]
fn no_text_is_set_under_an_inset() {
    use crate::display::TextRun;
    use crate::display::cells::CellMap;
    const UNDER: crate::sprite::Rgb = crate::sprite::Rgb { r: 1, g: 2, b: 3 };
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let held = *scene.agents.keys().next().expect("an agent");
    let scale = RenderScale::new(2).expect("nonzero");
    let k = scale.get();
    let (mut badged, mut runs_checked) = (0, 0);
    for insets in crate::layout::SWEEP_INSETS {
        let viewport = crate::layout::inset_viewport(SIZE.w, SIZE.h, insets);
        let (size, inner) = (viewport.size, viewport.inner().expect("an office inside"));
        // Past a side no inset covers lies the host's own margin, never an inset.
        let open = Bounds {
            width: if insets.right == 0 {
                u16::MAX - inner.x
            } else {
                inner.width
            },
            height: if insets.bottom == 0 {
                u16::MAX - inner.y
            } else {
                inner.height
            },
            ..inner
        };
        let inside = |x: u16, y: u16| {
            (inner.x * k..(inner.x + inner.width) * k).contains(&x)
                && (inner.y * k..(inner.y + inner.height) * k).contains(&y)
        };
        let looks = [
            Look::Classic {
                scale,
                text: WorldText::Host,
            },
            Look::Cutaway {
                scale,
                text: WorldText::Host,
            },
        ];
        for look in looks {
            let pack = Arc::new(crate::pack::test_office());
            let (mut floor, mut office) = (PerFloor::new(Arc::clone(&pack)), PerOffice::new());
            let pen = crate::display::pen::Pen::for_pack(scale, &pack);
            let now = t0 + Duration::from_secs(600);
            carry_to_each_edge(
                (&mut floor, &mut office),
                (
                    look,
                    RenderInputs {
                        viewport,
                        ..inputs(&scene, now)
                    },
                ),
                crate::interact::Figure::Agent(held),
                |gesture, rendered, raster| {
                    let text = rendered
                        .picture(raster)
                        .and_then(|p| p.text)
                        .expect("the host sets the text");
                    badged += usize::from(text.badges.iter().any(|b| {
                        b.agent == held && b.at.y < inner.y + crate::display::text::LABEL_GAP
                    }));
                    let bubbles = text.badges.iter().map(|badge| {
                        let bubble = crate::chitchat::ChitchatBubble {
                            text: "a quip as long as any",
                            speaker: badge.agent,
                        };
                        TextRun::bubble(&bubble, badge, theme())
                    });
                    let runs: Vec<TextRun> = text
                        .badges
                        .iter()
                        .map(crate::display::Badge::run)
                        .chain(bubbles)
                        .chain(text.bubbles.iter().cloned())
                        .chain(text.signs.iter().cloned())
                        .collect();
                    for run in &runs {
                        let mut buf =
                            crate::sprite::RgbBuffer::filled(size.w * k, size.h * k, UNDER);
                        crate::cutaway::paint::paint_run(run, (&pack, pen), &mut buf);
                        let under = (0..buf.height())
                            .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
                            .find(|&(x, y)| !inside(x, y) && buf.get(x, y) != UNDER);
                        assert_eq!(
                            under,
                            None,
                            "{insets:?} {look:?} {gesture:?}: {:?} {:?} bakes under an inset of {inner:?}",
                            run.role,
                            run.text()
                        );
                        let at = run.line_at(
                            crate::display::text::width(&run.text()).0,
                            CellMap::HALF_BLOCK,
                        );
                        for (col, row) in (at.x..at.x + at.w)
                            .flat_map(|c| (at.y..at.y + at.h).map(move |r| (c, r)))
                        {
                            assert!(
                                CellMap::HALF_BLOCK.area(col, row).overlaps(open),
                                "{insets:?} {look:?} {gesture:?}: {:?} {:?} sets cell ({col}, {row}) under an inset of {inner:?}",
                                run.role,
                                run.text()
                            );
                        }
                        runs_checked += 1;
                    }
                },
            );
        }
    }
    assert!(
        badged > 0,
        "the held agent's badge never reached the top inset, so the clamp went untested"
    );
    assert!(runs_checked > 0, "no text was set");
}

/// The entry runs the sim's epilogue every frame it steps: a carrier's cup is
/// stamped with the frame that saw it, and the door's clamp is the frame's own.
#[test]
fn the_entry_runs_the_epilogue_every_frame() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let (mut floor, mut office) = (
        PerFloor::new(Arc::new(crate::pack::test_office())),
        PerOffice::new(),
    );
    let mut coffee_seen = false;
    let mut door_seen = false;
    for step in 0..1_200u64 {
        let now = t0 + Duration::from_millis(step * 500);
        let before = office.coffee.map().clone();
        render(
            &mut floor,
            office.stores(),
            Look::HALF_BLOCKS,
            inputs(&scene, now),
        )
        .expect("lays out");
        for (id, at) in office.coffee.map() {
            if !before.contains_key(id) {
                assert_eq!(*at, now, "step {step}: a cup stamped off its frame");
                coffee_seen = true;
            }
        }
        let clamp = floor.ctx.door_anim_max_ms;
        floor.ctx.recompute_door_anim_max_ms(now);
        assert_eq!(
            clamp, floor.ctx.door_anim_max_ms,
            "step {step}: a stale door clamp"
        );
        door_seen |= clamp > 0;
        if coffee_seen && door_seen {
            break;
        }
    }
    assert!(
        door_seen,
        "no frame walked the door, so its clamp went untested"
    );
    assert!(
        coffee_seen,
        "no carrier fetched coffee, so its stamp went untested"
    );
}

/// A floor that switches looks repaints the whole frame on each switch and keeps
/// each look's raster, so switching back rebuilds nothing.
#[test]
fn a_floor_switching_looks_repaints_whole_and_keeps_each_raster() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let (mut floor, mut office) = (
        PerFloor::new(Arc::new(crate::pack::test_office())),
        PerOffice::new(),
    );
    let scale = RenderScale::new(2).expect("nonzero");
    let cutaway = Look::Cutaway {
        scale,
        text: WorldText::Baked,
    };
    let mut frame = |floor: &mut PerFloor, look, ms: u64| {
        let r = render(
            floor,
            office.stores(),
            look,
            inputs(&scene, t0 + Duration::from_millis(ms)),
        )
        .expect("lays out");
        let px = floor.raster.pixels().expect("drawn");
        (r.dirty, (px.width(), px.height()))
    };
    let classic_size = (SIZE.w, SIZE.h);
    let cutaway_size = (scale.to_buffer(SIZE.w), scale.to_buffer(SIZE.h));
    assert_eq!(
        frame(&mut floor, Look::HALF_BLOCKS, 0),
        (Dirty::All, classic_size)
    );
    assert_eq!(frame(&mut floor, cutaway, 1), (Dirty::All, cutaway_size));
    assert_ne!(
        frame(&mut floor, cutaway, 1).0,
        Dirty::All,
        "an unchanged frame"
    );
    assert_eq!(
        frame(&mut floor, Look::HALF_BLOCKS, 2),
        (Dirty::All, classic_size)
    );
    let canvas = floor.raster.cutaway.as_ref().map(std::ptr::from_ref);
    assert_eq!(frame(&mut floor, cutaway, 3), (Dirty::All, cutaway_size));
    assert_eq!(
        floor.raster.cutaway.as_ref().map(std::ptr::from_ref),
        canvas,
        "switching back rebuilt the cutaway's canvas"
    );
}

/// A cutaway that leaves its world text to the host hands over the
/// classic's: every agent's badge, the board and the floor indicator. One
/// that bakes it hands over nothing. That it then paints none is
/// `leaving_text_to_the_host_keeps_no_text_piece`'s.
#[test]
fn a_cutaway_leaving_its_text_to_the_host_hands_over_the_classics() {
    let pack = Arc::new(crate::pack::test_office());
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let mut office = PerOffice::new();
    let mut frame = |text| {
        let mut floor = PerFloor::new(Arc::clone(&pack));
        let look = Look::Cutaway {
            scale: RenderScale::new(4).expect("nonzero"),
            text,
        };
        render(&mut floor, office.stores(), look, inputs(&scene, t0)).expect("lays out");
        let pixels = floor.raster.pixels().expect("drawn").clone();
        (floor, pixels)
    };
    let (baked, baked_px) = frame(WorldText::Baked);
    let (host, host_px) = frame(WorldText::Host);
    assert!(baked.raster.host_text().is_none());
    assert!(host.raster.star().is_none(), "the host sets the star");
    assert_ne!(
        host_px.as_slice(),
        baked_px.as_slice(),
        "the text left the image"
    );
    let handed = host.raster.host_text().expect("handed over");
    let mut classic = PerFloor::new(Arc::clone(&pack));
    render(
        &mut classic,
        office.stores(),
        Look::HALF_BLOCKS,
        inputs(&scene, t0),
    )
    .expect("lays out");
    let classic = classic.raster.host_text().expect("shown");
    // Each look's badges come in its own paint order.
    let agents = |w: crate::display::HostText<'_>| -> HashSet<AgentId> {
        w.badges.iter().map(|b| b.agent).collect()
    };
    assert!(!handed.badges.is_empty());
    assert_eq!(agents(handed), agents(classic));
    assert_eq!(handed.signs, classic.signs);
}

/// The cutaway's recoloured figures leave with their agents: a long run that
/// sees sessions come and go holds none of the gone ones' art.
#[test]
fn the_cutaways_figures_leave_with_their_agents() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let (mut floor, mut office) = (
        PerFloor::new(Arc::new(crate::pack::test_office())),
        PerOffice::new(),
    );
    let cutaway = Look::Cutaway {
        scale: RenderScale::new(4).expect("nonzero"),
        text: WorldText::Baked,
    };
    render(&mut floor, office.stores(), cutaway, inputs(&scene, t0)).expect("lays out");
    assert!(
        office.raster.cutaway.figures_len() > 0,
        "premise: the frame drew its agents"
    );
    office.evict_missing(&SceneState::new([8; pixtuoid_core::state::MAX_FLOORS]));
    assert_eq!(office.raster.cutaway.figures_len(), 0);
}

/// A classic frame hands its badges, its bubbles and its signs over apart,
/// and a refused frame leaves them empty.
#[test]
fn a_classic_frame_hands_over_its_badges_and_signs_apart() {
    use crate::display::TextRole;
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let (mut floor, mut office) = (
        PerFloor::new(Arc::new(crate::pack::test_office())),
        PerOffice::new(),
    );
    render(
        &mut floor,
        office.stores(),
        Look::HALF_BLOCKS,
        inputs(&scene, t0),
    )
    .expect("lays out");
    let text = floor.raster.host_text().expect("shown");
    assert!(!text.badges.is_empty(), "the office badges its agents");
    let signs: Vec<TextRole> = text.signs.iter().map(|r| r.role).collect();
    assert!(
        signs.contains(&TextRole::Star) && signs.contains(&TextRole::Indicator),
        "{signs:?}"
    );
    assert!(
        !signs.iter().any(|r| matches!(r, TextRole::Badge(_))),
        "{signs:?}"
    );
    let refused = RenderInputs {
        viewport: Viewport::full(1, 1),
        ..inputs(&scene, t0)
    };
    assert!(render(&mut floor, office.stores(), Look::HALF_BLOCKS, refused).is_none());
    let text = floor.raster.host_text().expect("shown");
    assert!(text.badges.is_empty() && text.bubbles.is_empty() && text.signs.is_empty());
}

/// A frame of another pack or theme empties the office's caches first, and
/// one of the same keeps them.
#[test]
fn a_new_pack_or_theme_empties_the_office_caches() {
    use crate::outside::{OutsideCache, Wall};
    let normal = &crate::theme::NORMAL;
    let cyberpunk = crate::theme::theme_by_name("cyberpunk").expect("a registry theme");
    let one = Arc::new(crate::pack::test_office());
    // the curtain wall drops out of the near plane
    let other = Arc::new(
        OfficeArt::parse(crate::pack::test_pack_declaring(
            "planes = [\"mid\", \"near\"]",
            "planes = [\"mid\"]",
        ))
        .expect("the test pack parses"),
    );
    let now = crate::localclock::at_hour(12);
    let views = |outside: &mut OutsideCache, pack: &OfficeArt, theme| {
        let moment = crate::atmosphere::Moment::resolve(
            crate::sky::Sky::at_with(now, crate::sky::Weather::Overcast),
            theme,
            0.0,
            crate::anim::Motion::Full.timing(now),
        );
        let w = crate::layout::WINDOW_W * 3;
        outside.views(
            &moment,
            pack,
            theme,
            Wall {
                span: 0..w,
                band_h: 32,
                bays: crate::layout::window_slots(0..w).collect(),
            },
            crate::sprite::format::Density::ONE,
            crate::glass_weather::GlassWeather::of(&moment),
        )[0]
        .1
        .clone()
    };
    let mut raster = OfficeRaster::default();
    raster.serve(&one, normal);
    let first = views(&mut raster.outside, &one, normal);
    raster.serve(&one, normal);
    let kept = views(&mut raster.outside, &one, normal);
    assert!(
        Arc::ptr_eq(&first, &kept),
        "one pack in one theme lost its views"
    );
    raster.serve(&one, cyberpunk);
    let themed = views(&mut raster.outside, &one, cyberpunk);
    assert!(!Arc::ptr_eq(&kept, &themed), "a new theme kept the views");
    raster.serve(&other, cyberpunk);
    let packed = views(&mut raster.outside, &other, cyberpunk);
    assert!(!Arc::ptr_eq(&themed, &packed), "a new pack kept the views");
}

/// [`Dim::NONE`] leaves a frame as drawn; a dim darkens each pixel a frame
/// may write, inside a clip where the buffer has one. A brightness past
/// either end clamps, and a NaN leaves the frame as drawn.
#[test]
fn a_dim_darkens_each_pixel_a_frame_may_write() {
    let px = crate::sprite::Rgb {
        r: 201,
        g: 103,
        b: 7,
    };
    let mut buf = RgbBuffer::filled(3, 2, px);
    Dim::NONE.apply(&mut buf);
    assert!(buf.as_slice().iter().all(|p| *p == px));
    let half = Dim::from_brightness(0.5);
    buf.with_clip((0..1, 0..2), |buf| half.apply(buf));
    assert_eq!(buf.get(0, 1), half.of(px));
    assert_ne!(buf.get(0, 1), px);
    assert_eq!(buf.get(1, 1), px, "outside the clip");
    assert_eq!(Dim::from_brightness(f32::NAN), Dim::NONE);
    assert_eq!(Dim::from_brightness(1.5), Dim::NONE);
    assert_eq!(Dim::from_brightness(-1.0).of(px), Rgb { r: 0, g: 0, b: 0 });
}

#[test]
fn each_look_kind_has_its_own_id_and_every_look_has_a_kind() {
    let ids: HashSet<_> = LookKind::ALL.iter().map(|k| k.name()).collect();
    assert_eq!(ids.len(), LookKind::ALL.len());
    let shown: HashSet<_> = LookKind::ALL.iter().map(|k| k.display_name()).collect();
    assert_eq!(shown.len(), LookKind::ALL.len());
    assert_eq!(Look::HALF_BLOCKS.kind(), LookKind::Classic);
    let cutaway = Look::Cutaway {
        scale: RenderScale::ONE,
        text: WorldText::Baked,
    };
    assert_eq!(cutaway.kind(), LookKind::Cutaway);
}

/// The bundled pack, whose art a pixel painter's classic is drawn at the
/// density of.
fn bundled() -> (Arc<crate::pack::OfficeArt>, RenderScale) {
    let pack = Arc::new(crate::pack::test_office());
    let scale = RenderScale::from(pack.max_density_variant());
    assert!(scale != RenderScale::ONE, "the bundled pack draws variants");
    (pack, scale)
}

fn scaled(scale: RenderScale, text: WorldText) -> Look {
    Look::Classic { scale, text }
}

/// A screen that takes only each frame's dirt shows every frame whole.
#[test]
fn a_scaled_classic_shown_under_its_dirt_is_each_whole_frame() {
    let (pack, scale) = bundled();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let (mut floor, mut office) = (PerFloor::new(pack), PerOffice::new());
    let mut screen: Option<RgbBuffer> = None;
    let mut partial = 0;
    for step in 0..300u64 {
        let now = t0 + Duration::from_millis(step * 100);
        let look = scaled(scale, WorldText::Baked);
        let rendered =
            render(&mut floor, office.stores(), look, inputs(&scene, now)).expect("lays out");
        let pixels = floor.raster.pixels().expect("a frame");
        match (&rendered.dirty, &mut screen) {
            (Dirty::Rects(rects), Some(shown)) => {
                partial += 1;
                for b in rects.as_slice() {
                    for y in b.y..b.y + b.height {
                        for x in b.x..b.x + b.width {
                            shown.put(x, y, pixels.get(x, y));
                        }
                    }
                }
            }
            (Dirty::Unchanged, Some(_)) => {}
            _ => screen = Some(pixels.clone()),
        }
        let shown = screen.as_ref().expect("a frame shown");
        assert!(
            shown.as_slice() == pixels.as_slice(),
            "step {step}: the screen shows other than the frame ({:?})",
            rendered.dirty
        );
    }
    assert!(partial > 0, "some frame repaints in part");
}

/// The first frame in another look repaints whole, its own look's change of
/// scale or text included.
#[test]
fn a_change_of_look_repaints_the_scaled_classic_whole() {
    let (pack, scale) = bundled();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let (mut floor, mut office) = (PerFloor::new(pack), PerOffice::new());
    let double = RenderScale::new(scale.get() * 2).expect("a scale");
    let mut dirty = |look: Look| {
        render(&mut floor, office.stores(), look, inputs(&scene, t0))
            .expect("lays out")
            .dirty
    };
    let baked = scaled(scale, WorldText::Baked);
    assert_eq!(dirty(baked), Dirty::All, "the first frame");
    assert_ne!(dirty(baked), Dirty::All, "the same look");
    for other in [
        Look::HALF_BLOCKS,
        Look::Cutaway {
            scale,
            text: WorldText::Baked,
        },
        scaled(double, WorldText::Baked),
        scaled(scale, WorldText::Host),
    ] {
        dirty(other);
        assert_eq!(dirty(baked), Dirty::All, "back from {other:?}");
    }
}

/// With its text the host's, the scaled classic is the half-blocks' frame,
/// each pixel a square of its scale.
#[test]
fn the_scaled_classic_is_the_half_blocks_blown_up() {
    let (pack, scale) = bundled();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let frame = |look: Look| {
        let (mut floor, mut office) = (PerFloor::new(Arc::clone(&pack)), PerOffice::new());
        render(&mut floor, office.stores(), look, inputs(&scene, t0)).expect("lays out");
        let text = floor.raster.host_text().map(|t| t.badges.len());
        (floor.raster.pixels().expect("a frame").clone(), text)
    };
    let (unit, unit_text) = frame(Look::HALF_BLOCKS);
    let (big, big_text) = frame(scaled(scale, WorldText::Host));
    let s = scale.get();
    assert_eq!(
        (big.width(), big.height()),
        (unit.width() * s, unit.height() * s)
    );
    for y in 0..big.height() {
        for x in 0..big.width() {
            assert_eq!(big.get(x, y), unit.get(x / s, y / s), "({x}, {y})");
        }
    }
    assert!(unit_text.is_some_and(|n| n > 0), "the scene has badges");
    assert_eq!(big_text, unit_text, "its text is still the host's");
}

/// Baked, the classic at a whole multiple of its density is its render at
/// the density upscaled, every pixel a square of the multiple: what a pixel
/// painter's fit shows.
#[test]
fn the_baked_classic_upscales_whole() {
    let (pack, density) = bundled();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let frame = |scale| {
        let (mut floor, mut office) = (PerFloor::new(Arc::clone(&pack)), PerOffice::new());
        let look = scaled(scale, WorldText::Baked);
        render(&mut floor, office.stores(), look, inputs(&scene, t0)).expect("lays out");
        floor.raster.pixels().expect("a frame").clone()
    };
    let k = 2;
    let (small, big) = (
        frame(density),
        frame(RenderScale::new(density.get() * k).expect("nonzero")),
    );
    assert_eq!(
        (big.width(), big.height()),
        (small.width() * k, small.height() * k)
    );
    for y in 0..big.height() {
        for x in 0..big.width() {
            assert_eq!(big.get(x, y), small.get(x / k, y / k), "({x}, {y})");
        }
    }
}

/// Baked, the classic's text is the cutaway's paint of its runs, in its
/// host text's order, over its art, and no host sets any.
#[test]
fn baked_classic_text_is_the_cutaways_paint_of_its_runs() {
    let (pack, scale) = bundled();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let floor = |text: WorldText| {
        let (mut floor, mut office) = (PerFloor::new(Arc::clone(&pack)), PerOffice::new());
        render(
            &mut floor,
            office.stores(),
            scaled(scale, text),
            inputs(&scene, t0),
        )
        .expect("lays out");
        floor
    };
    let (host, baked) = (floor(WorldText::Host), floor(WorldText::Baked));
    let text = host.raster.host_text().expect("the host's text");
    let mut expected = host.raster.pixels().expect("a frame").clone();
    let pen = crate::display::pen::Pen::for_pack(scale, &pack);
    let runs = text
        .badges
        .iter()
        .map(crate::display::Badge::run)
        .chain(text.bubbles.iter().cloned())
        .chain(text.signs.iter().cloned());
    for run in runs {
        crate::cutaway::paint::paint_run(&run, (&pack, pen), &mut expected);
    }
    assert!(
        baked.raster.host_text().is_none(),
        "no host sets baked text"
    );
    assert!(
        baked.raster.pixels().expect("a frame").as_slice() == expected.as_slice(),
        "the baked frame is the art with its runs painted"
    );
}

/// The baked classic sets the board's star where the cutaway does at its
/// scale, dropping it with the cutaway where a grid the pack's density does
/// not divide would write it over the brand, and a pointer finds it there;
/// the half-blocks leave its placing to their host.
#[test]
fn the_baked_classic_sets_the_star_the_cutaway_does() {
    let (pack, density) = bundled();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let star = |look: Look| {
        let (mut floor, mut office) = (PerFloor::new(Arc::clone(&pack)), PerOffice::new());
        let rendered =
            render(&mut floor, office.stores(), look, inputs(&scene, t0)).expect("lays out");
        let star = floor.raster.star();
        if let Some(star) = star {
            assert!(matches!(
                rendered.hits.at(star, None),
                Some(crate::hit::SceneHit::Star)
            ));
        }
        star
    };
    let coarse = RenderScale::new(2).expect("nonzero");
    for scale in [coarse, density] {
        assert_eq!(
            star(scaled(scale, WorldText::Baked)),
            star(Look::Cutaway {
                scale,
                text: WorldText::Baked,
            }),
            "{scale:?}"
        );
    }
    assert!(star(scaled(density, WorldText::Baked)).is_some());
    assert_eq!(star(scaled(coarse, WorldText::Baked)), None);
    assert_eq!(star(Look::HALF_BLOCKS), None);
}

/// A slide bakes every look's text but the half-blocks', which no glyph
/// fits.
#[test]
fn a_slide_bakes_every_look_but_the_half_blocks() {
    let (_, scale) = bundled();
    assert_eq!(Look::HALF_BLOCKS.baked(), Look::HALF_BLOCKS);
    for look in [
        scaled(scale, WorldText::Host),
        Look::Cutaway {
            scale,
            text: WorldText::Host,
        },
    ] {
        let baked = look.baked();
        assert_eq!(baked.kind(), look.kind());
        assert!(
            matches!(
                baked,
                Look::Classic {
                    text: WorldText::Baked,
                    ..
                } | Look::Cutaway {
                    text: WorldText::Baked,
                    ..
                }
            ),
            "{look:?} → {baked:?}"
        );
    }
}

/// Under a dim, the baked classic is its undimmed frame dimmed, its text's
/// icons with the rest, as the cutaway dims its own.
#[test]
fn a_dim_darkens_the_baked_classic_after_its_text() {
    let (pack, scale) = bundled();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let look = scaled(scale, WorldText::Baked);
    let dim = Dim::from_brightness(0.5);
    let frame = |dim: Dim| {
        let (mut floor, mut office) = (PerFloor::new(Arc::clone(&pack)), PerOffice::new());
        let inputs = RenderInputs {
            dim,
            ..inputs(&scene, t0)
        };
        render(&mut floor, office.stores(), look, inputs).expect("lays out");
        floor.raster.pixels().expect("a frame").clone()
    };
    let mut expected = frame(Dim::NONE);
    dim.apply(&mut expected);
    assert!(frame(dim).as_slice() == expected.as_slice());
}

/// A frame most of whose squares changed is reported changed whole, not as
/// a rect a square.
#[test]
fn a_scaled_classic_changed_all_over_repaints_whole() {
    let (pack, scale) = bundled();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let scene = office(t0);
    let (mut floor, mut office) = (PerFloor::new(pack), PerOffice::new());
    let look = scaled(scale, WorldText::Baked);
    let mut dirty = |dim: Dim| {
        let inputs = RenderInputs {
            dim,
            ..inputs(&scene, t0)
        };
        render(&mut floor, office.stores(), look, inputs)
            .expect("lays out")
            .dirty
    };
    dirty(Dim::NONE);
    assert_eq!(dirty(Dim::NONE), Dirty::Unchanged, "the same frame");
    assert_eq!(
        dirty(Dim::from_brightness(0.5)),
        Dirty::All,
        "every pixel darker"
    );
}

/// A frame is changed whole only past half its squares changed.
#[test]
fn a_frame_is_changed_whole_past_half_its_squares() {
    let side = usize::from(classic::SQUARE);
    let (cols, rows) = (4, 2);
    let size = |n: usize| u16::try_from(n * side).expect("fits");
    let was = RgbBuffer::filled(size(cols), size(rows), crate::composite::BLACK);
    let changed = |n: usize| {
        let mut now = was.clone();
        for i in 0..n {
            let at = |square: usize| u16::try_from(square * side).expect("fits");
            now.put(at(i % cols), at(i / cols), Rgb { r: 255, g: 0, b: 0 });
        }
        classic::changed(&was, &now, RenderScale::ONE)
    };
    let half = cols * rows / 2;
    assert!(matches!(changed(half), Dirty::Rects(_)), "half");
    assert_eq!(changed(half + 1), Dirty::All, "past half");
}
