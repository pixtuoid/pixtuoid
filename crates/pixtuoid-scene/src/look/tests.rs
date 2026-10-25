use crate::character::test_support::make_slot;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use pixtuoid_core::state::{ActivityState, GlobalDeskIndex};
use pixtuoid_core::{AgentId, AgentSlot, SceneState};

use super::*;
use crate::floor::{FloorMeta, PerOffice, PetInputs};

const SIZE: Size = Size { w: 192, h: 160 };

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
        size: SIZE,
        place: Place::default(),
        debug_walkable: false,
        dim: crate::look::Dim::NONE,
    }
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
            Look::Classic,
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
        frame(&mut floor, Look::Classic, 0),
        (Dirty::All, classic_size)
    );
    assert_eq!(frame(&mut floor, cutaway, 1), (Dirty::All, cutaway_size));
    assert_ne!(
        frame(&mut floor, cutaway, 1).0,
        Dirty::All,
        "an unchanged frame"
    );
    assert_eq!(
        frame(&mut floor, Look::Classic, 2),
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
        Look::Classic,
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
        Look::Classic,
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
        size: Size { w: 1, h: 1 },
        ..inputs(&scene, t0)
    };
    assert!(render(&mut floor, office.stores(), Look::Classic, refused).is_none());
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
                size: (w, 32),
                bays: crate::layout::window_slots(w).collect(),
            },
            pixtuoid_core::sprite::format::Density::ONE,
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
    let px = pixtuoid_core::sprite::Rgb {
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
