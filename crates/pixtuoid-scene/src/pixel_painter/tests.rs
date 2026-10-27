use super::background::paint_corridor_runner;
use super::drawable::paint_character_at;
use super::*;
use crate::anim::{Motion, epoch_ms};
use crate::character::test_support::{color_of, make_slot, make_slot_cwd};
use crate::character::{HAIR_KEY, PANTS_KEY, SHIRT_KEY, SKIN_KEY, work_glow_tint};
use crate::cutaway::wall::paint_wall;
#[cfg(feature = "cutaway-assets")]
use crate::display::pen::test_density;
use crate::floor::{FloorInputs, PetInputs};
use crate::layout::CHARACTER_SPRITE_W;
use crate::layout::{HomeDesk, Point, Size, sort_row_at};
use crate::pack::desk_art_top;
use crate::physics::LegProgress;
use crate::pose;
use crate::sim::anchors::{
    back_couch_top_left, compute_door_frame_idx, seated_top_left, walking_top_left,
    waypoint_top_left,
};
use crate::sim::seat::{Seat, settle_seat};
use crate::sim::{CharacterGlow, CharacterPlacement, SimInputs, SimStores, sim_step};
use pixtuoid_core::sprite::Frame;
use pixtuoid_core::sprite::format::Piece;
use pixtuoid_core::state::{
    ActivityState, FloorLocalDeskIndex, GlobalDeskIndex, ToolKind, ToolUse,
};
use std::sync::Arc;
use std::time::SystemTime;
use strum::VariantArray as _;

/// Paint all of `piece` in one call, which the classic's bands add up to.
fn paint_whole_wall(
    buf: &mut RgbBuffer,
    theme: &crate::theme::Theme,
    piece: crate::layout::WallPiece,
) {
    let (at, size) = piece.visual();
    paint_wall(
        buf,
        crate::glass::WallTrim::of(theme),
        piece,
        at.y..at.y + size.h,
        crate::display::pen::Pen::UNIT,
    );
}

#[test]
fn v_door_jambs_sit_flush_on_both_cut_ends() {
    // The glass painters are endpoint-INCLUSIVE, so each jamb must COVER its
    // cut end, or a 1px glass sliver survives between post and opening.
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let floor = Rgb {
        r: 150,
        g: 110,
        b: 72,
    };
    let mut buf = RgbBuffer::filled(20, 60, floor);
    paint_whole_wall(
        &mut buf,
        theme,
        crate::layout::WallPiece::Vertical {
            x: 5,
            y_top: 10,
            north: 10,
            y_bot: 24,
            south: 24,
            jamb_north: false,
            jamb_south: true,
        },
    );
    paint_whole_wall(
        &mut buf,
        theme,
        crate::layout::WallPiece::Vertical {
            x: 5,
            y_top: 38,
            north: 38,
            y_bot: 52,
            south: 52,
            jamb_north: true,
            jamb_south: false,
        },
    );
    let dark = theme.office.room_wall_trim_dark;
    for y in [23, 24, 38, 39] {
        assert_eq!(
            buf.get(5, y),
            dark,
            "row {y} must be jamb (posts cover BOTH inclusive cut ends)"
        );
    }
    for y in 25..38 {
        assert_eq!(buf.get(5, y), floor, "row {y} is the OPENING — untouched");
    }
}

#[test]
fn h_door_jambs_sit_flush_on_both_cut_ends() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let floor = Rgb {
        r: 150,
        g: 110,
        b: 72,
    };
    let mut buf = RgbBuffer::filled(60, 30, floor);
    let y_face = 20;
    for (x0, x1, jamb_west, jamb_east) in [(5, 19, false, true), (33, 47, true, false)] {
        paint_whole_wall(
            &mut buf,
            theme,
            crate::layout::WallPiece::Horizontal {
                x0,
                x1,
                y_face,
                jamb_west,
                jamb_east,
            },
        );
    }
    let dark = theme.office.room_wall_trim_dark;
    for x in [18, 19, 33, 34] {
        assert_eq!(
            buf.get(x, y_face),
            dark,
            "column {x} must be jamb (posts cover BOTH inclusive cut ends)"
        );
    }
    for x in 20..33 {
        assert_eq!(
            buf.get(x, y_face),
            floor,
            "column {x} is the OPENING — untouched"
        );
    }
}

#[test]
fn h_wall_jamb_flags_join_on_the_doorway_cut_ends() {
    use crate::layout::TEST_DEFAULT_DESKS;
    let l = SceneLayout::compute(215, 98, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let dw = l
        .doorways
        .iter()
        .find(|d| d.start.y == d.end.y)
        .expect("the meeting-pantry 60% door");
    let mut drawables = Vec::new();
    enqueue_room_walls(&l, &mut drawables);
    let walls: Vec<_> = drawables
        .iter()
        .filter_map(|d| match d.kind {
            DrawableKind::RoomWall {
                piece:
                    crate::layout::WallPiece::Horizontal {
                        x0,
                        x1,
                        jamb_west,
                        jamb_east,
                        ..
                    },
                ..
            } => Some((x0, x1, jamb_west, jamb_east)),
            _ => None,
        })
        .collect();
    let left = walls
        .iter()
        .find(|(_, x1, ..)| *x1 == dw.start.x)
        .expect("segment left of the door");
    assert!(
        left.3 && !left.2,
        "left segment: jamb on its RIGHT end only"
    );
    let right = walls
        .iter()
        .find(|(x0, ..)| *x0 == dw.end.x)
        .expect("segment right of the door");
    assert!(
        right.2 && !right.3,
        "right segment: jamb on its LEFT end only"
    );
}

#[test]
fn v_wall_jamb_flags_and_south_anchor_on_the_doorway_cut_ends() {
    use crate::layout::TEST_DEFAULT_DESKS;
    let l = SceneLayout::compute(215, 98, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let dw = l
        .doorways
        .iter()
        .find(|d| d.start.x == d.end.x)
        .expect("the meeting room's centered vertical door");
    let mut drawables = Vec::new();
    enqueue_room_walls(&l, &mut drawables);
    // Each wall's southmost band: the one that sorts on the wall's own end.
    let walls: Vec<_> = drawables
        .iter()
        .filter_map(|d| match d.kind {
            DrawableKind::RoomWall {
                piece:
                    crate::layout::WallPiece::Vertical {
                        x,
                        y_top,
                        y_bot,
                        jamb_north,
                        jamb_south,
                        ..
                    },
                ref rows,
            } if x == dw.start.x && rows.end == y_bot + 1 => {
                Some((d.sort_row, y_top, y_bot, jamb_north, jamb_south))
            }
            _ => None,
        })
        .collect();
    let top = walls
        .iter()
        .find(|(_, _, y_bot, ..)| *y_bot == dw.start.y)
        .expect("segment north of the door");
    assert_eq!(
        top.0, top.2,
        "the door-terminus (top) segment y-sorts at its south base"
    );
    assert!(top.4 && !top.3, "top segment: jamb on its SOUTH end only");
    let bottom = walls
        .iter()
        .find(|(_, y_top, ..)| *y_top == dw.end.y)
        .expect("segment south of the door");
    assert!(
        bottom.3 && !bottom.4,
        "bottom segment: jamb on its NORTH end only"
    );
}

#[test]
fn glass_wall_h_back_cap_composites_over_a_character_behind_it() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let y_top = 20u16;
    // The northmost row a routed walker's feet can reach; closer rows sit inside
    // the blocked band no walker ever occupies.
    let cap_row = y_top - (crate::layout::OBSTACLE_PAD_PX + 1);
    let character = Rgb {
        r: 220,
        g: 40,
        b: 40,
    };
    let mut buf = RgbBuffer::filled(
        48,
        48,
        Rgb {
            r: 150,
            g: 110,
            b: 72,
        },
    );
    for x in 4..20 {
        buf.put(x, cap_row, character);
    }
    paint_whole_wall(
        &mut buf,
        theme,
        crate::layout::WallPiece::Horizontal {
            x0: 0,
            x1: 47,
            y_face: y_top,
            jamb_west: false,
            jamb_east: false,
        },
    );
    let after = buf.get(8, cap_row);
    assert_ne!(after, character, "glass must composite over the character");
    assert!(
        after.r > after.g.max(after.b),
        "the pane is see-through: the character still reads red: {after:?}"
    );
}

#[test]
fn glass_wall_v_composites_over_a_character_behind_its_north_cap() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (x_left, y_top, y_bot) = (10u16, 20u16, 40u16);
    // The outer columns are frame (rim, post), so probe a pane cell.
    let probe_col = x_left + 1;
    let probe_row = y_top + 1;
    let character = Rgb {
        r: 220,
        g: 40,
        b: 40,
    };
    let mut buf = RgbBuffer::filled(
        48,
        48,
        Rgb {
            r: 150,
            g: 110,
            b: 72,
        },
    );
    buf.put(probe_col, probe_row, character);
    paint_whole_wall(
        &mut buf,
        theme,
        crate::layout::WallPiece::Vertical {
            x: x_left,
            y_top,
            y_bot,
            north: y_top,
            south: y_bot,
            jamb_north: false,
            jamb_south: false,
        },
    );
    let after = buf.get(probe_col, probe_row);
    assert_ne!(after, character, "glass must composite over the character");
    assert!(
        after.r > after.g.max(after.b),
        "the pane is see-through: the character still reads red: {after:?}"
    );
}

#[test]
fn seat_view_maps_facing_to_sprite_and_flip() {
    use crate::layout::{Facing, WaypointKind};
    assert_eq!(
        Seat::at_waypoint(WaypointKind::Couch, Point { x: 40, y: 30 }, Facing::North)
            .sprite_for(Piece::Seated),
        (Piece::BackCouch, false),
        "couch's seated facing is North (window) → back_couch, same path as the sofa"
    );
    assert_eq!(
        Seat::at_waypoint(
            WaypointKind::MeetingSofa,
            Point { x: 40, y: 30 },
            Facing::North
        )
        .sprite_for(Piece::Seated),
        (Piece::BackCouch, false)
    );
    assert_eq!(
        Seat::at_waypoint(
            WaypointKind::MeetingSofa,
            Point { x: 40, y: 30 },
            Facing::South
        )
        .sprite_for(Piece::Seated),
        (Piece::Seated, false)
    );
    assert_eq!(
        Seat::at_waypoint(
            WaypointKind::MeetingChair,
            Point { x: 40, y: 30 },
            Facing::East
        )
        .sprite_for(Piece::Seated),
        (Piece::SideSeated, false)
    );
    assert_eq!(
        Seat::at_waypoint(
            WaypointKind::MeetingChair,
            Point { x: 40, y: 30 },
            Facing::West
        )
        .sprite_for(Piece::Seated),
        (Piece::SideSeated, true)
    );
}

#[test]
fn a_back_turned_desk_shows_the_pose_s_own_back_view() {
    use crate::layout::{Facing, Point};
    let desk = Point { x: 40, y: 30 };
    let back = Seat::at_desk(desk, Facing::North);
    let front = Seat::at_desk(desk, Facing::South);
    for (base, want) in [
        (Piece::Seated, Piece::SeatedBack),
        (Piece::Typing, Piece::TypingBack),
        // A pose with no back view of its own falls back to the still one
        // rather than showing a face at the window.
        (Piece::SeatedSleeping, Piece::SeatedBack),
    ] {
        assert_eq!(back.sprite_for(base), (want, false), "{} back", base.name());
        assert_eq!(
            front.sprite_for(base),
            (base, false),
            "{} stays itself when the sitter faces the camera",
            base.name()
        );
    }
}

/// `piece`'s base art and each density variant, named as the manifest keys them.
#[cfg(feature = "cutaway-assets")]
fn arts_of(
    pack: &crate::pack::OfficeArt,
    piece: Piece,
) -> Vec<(String, &pixtuoid_core::sprite::Sprite)> {
    std::iter::once((piece.name().to_owned(), pack.piece(piece)))
        .chain(
            pack.variants_of(piece)
                .iter()
                .map(|(d, s)| (format!("{}@{d}x", piece.name()), s)),
        )
        .collect()
}

/// Every look of frame `i` of `anim` a viewer could see under `overrides`:
/// recolored and, where the art marks its head, dressed in each of the pack's
/// hairstyles.
#[cfg(feature = "cutaway-assets")]
fn looks(
    pack: &crate::pack::OfficeArt,
    anim: &pixtuoid_core::sprite::Sprite,
    i: usize,
    overrides: &[(char, pixtuoid_core::sprite::Pixel)],
) -> Vec<pixtuoid_core::sprite::Frame> {
    let bare = anim.recolorable_at(i).recolored(overrides);
    let line = pack.character_outline();
    match anim.head(i) {
        None => vec![bare],
        Some(head) => pack
            .hairstyles()
            .map(|s| {
                let dress = crate::character::Dress::of(&bare, head, Some(s));
                crate::character::dress(&bare, &dress, Some(s), overrides, line)
            })
            .collect(),
    }
}

/// Whether recoloring `key` changes some pixel of every look of frame `i`.
#[cfg(feature = "cutaway-assets")]
fn recolors(
    pack: &crate::pack::OfficeArt,
    anim: &pixtuoid_core::sprite::Sprite,
    i: usize,
    key: char,
) -> bool {
    let sentinel = Some(Rgb { r: 1, g: 2, b: 3 });
    looks(pack, anim, i, &[])
        .iter()
        .zip(looks(pack, anim, i, &[(key, sentinel)]))
        .all(|(own, recolored)| own.as_slice() != recolored.as_slice())
}

/// A key no character frame draws recolors nothing, so every agent would wear
/// the pack's own color there: `standing` shows all four, at every density,
/// dressed in whichever hairstyle.
#[test]
#[cfg(feature = "cutaway-assets")]
fn the_bundled_pack_draws_every_key_an_agent_recolors() {
    let pack = crate::pack::test_office();
    let mut drawn = 0;
    for (name, standing) in arts_of(&pack, Piece::Standing) {
        for key in [SHIRT_KEY, HAIR_KEY, SKIN_KEY, PANTS_KEY] {
            assert!(
                recolors(&pack, standing, 0, key),
                "no {name} pixel is drawn in {key:?}"
            );
        }
        drawn += 1;
    }
    assert!(
        drawn > 1,
        "the bundled pack ships a density variant of standing"
    );
}

/// Hair and shirt show in every pose, even face-down asleep, and they are how a
/// viewer tells two agents apart: every frame, base or `@Nx` variant, draws
/// them, dressed in whichever hairstyle, in a key the agent's recolor reaches
/// (the key itself or one of its `[ramps]` shades), or it shows every agent in
/// the pack's own colours. Skin and pants may be out of sight.
#[test]
#[cfg(feature = "cutaway-assets")]
fn every_character_frame_at_every_density_recolors_hair_and_shirt() {
    let pack = crate::pack::test_office();
    let mut variants = 0;
    for &base in Piece::VARIANTS
        .iter()
        .filter(|p| p.kind() == pixtuoid_core::sprite::format::PieceKind::Character)
    {
        for (name, anim) in arts_of(&pack, base) {
            variants += usize::from(name.contains('@'));
            for i in 0..anim.frames().len() {
                for key in [HAIR_KEY, SHIRT_KEY] {
                    assert!(
                        recolors(&pack, anim, i, key),
                        "{name} frame {i} draws nothing the {key:?} recolor reaches"
                    );
                }
            }
        }
    }
    assert!(variants > 0, "the bundled pack ships character variants");
}

/// A person drawn from `@Nx` art is the SAME person: the variant is picked at
/// the render scale and recolored through the same per-agent palette, so the
/// dense shirt is the classic profile's shirt colour.
#[test]
fn character_frame_takes_a_density_variant_recolored_like_the_base() {
    let one = format!("@frame 0\n{SHIRT_KEY}");
    let two = format!("@frame 0\n{SHIRT_KEY} {SHIRT_KEY}\n{SHIRT_KEY} {SHIRT_KEY}");
    let pack = crate::pack::test_office_with(
        &format!(
            "[palette]\n\
             \"{SHIRT_KEY}\"=\"#0a141e\"\n\"{HAIR_KEY}\"=\"#28323c\"\n\
             \"{SKIN_KEY}\"=\"#46505a\"\n\"{PANTS_KEY}\"=\"#646e78\"\n\
             [animations.typing]\nframes=[\"one.sprite\", \"one.sprite\"]\nframe_ms=100\n\
             [animations.\"typing@2x\"]\nframes=[\"two.sprite\", \"two.sprite\"]\nframe_ms=100\n"
        ),
        &[("one.sprite", one.as_str()), ("two.sprite", two.as_str())],
    );
    let slot = make_slot(
        pixtuoid_core::AgentId::from_transcript_path("/dense.jsonl"),
        ActivityState::Idle,
    );
    let mut cache = crate::frame_cache::FrameCache::new();
    let now = SystemTime::UNIX_EPOCH;
    let scale = crate::render_scale::RenderScale::new(4).expect("nonzero");

    let dense = crate::character::character_frame(
        crate::character::SpritePose {
            anim_name: Piece::Typing,
            frame_idx: 0,
            flip_x: false,
            glow_tint: None,
        },
        &slot,
        &pack,
        scale,
        &mut cache,
        now,
    );
    let got = (dense.frame.width(), dense.blit_at.get());
    let dense_shirt = dense.frame.get(0, 0).copied().flatten();
    assert_eq!(got, (2, 2));

    let classic = crate::character::character_frame(
        crate::character::SpritePose {
            anim_name: Piece::Typing,
            frame_idx: 0,
            flip_x: false,
            glow_tint: None,
        },
        &slot,
        &pack,
        crate::render_scale::RenderScale::ONE,
        &mut cache,
        now,
    );
    // Same agent, animation and frame through one cache: only the density key
    // keeps the classic request from being served the dense recolor.
    assert_eq!(classic.frame.width(), 1);
    assert_eq!(dense_shirt, classic.frame.get(0, 0).copied().flatten());
    assert_ne!(
        dense_shirt,
        pack.palette().get(SHIRT_KEY).flatten(),
        "the shirt took the agent's outfit, not the pack default"
    );
}

/// The cutaway draws a person from the variant its scale lands, laid out in
/// LOGICAL units whatever density the art came from: a variant that is exactly
/// its base upscaled renders the same pixels, shadow and badge as the base
/// blitted at the scale, and one drawn differently renders differently.
#[test]
fn a_person_from_a_faithful_variant_renders_as_their_upscaled_base() {
    use crate::layout::{CHARACTER_SPRITE_H, CHARACTER_SPRITE_W};
    use crate::render_scale::RenderScale;
    const DENSITY: u16 = 2;
    let (scene, layout, _, now0, bundled) = sim_rig();
    let coffee = HashMap::new();
    let mut owned = OwnedSimStores::new();
    let frame = sim_step(
        &mut owned.stores(),
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now: now0 + std::time::Duration::from_millis(250),
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &bundled,
            layout: &layout,
            coffee: &coffee,
            door_anim_max_ms: 0,
        },
    );
    let anim = frame
        .characters
        .first()
        .expect("the agent is on screen")
        .anim_name;

    // Two frames of a shirt block topped with hair (frame 0) then skin
    // (frame 1): recolorable, and whole-pixel so an upscale by `DENSITY` is the
    // variant exactly.
    let rows = |w: u16, h: u16, top: char| -> String {
        (0..h)
            .map(|y| {
                let key = if y < h / 3 { top } else { SHIRT_KEY };
                vec![key.to_string(); usize::from(w)].join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (w, h) = (CHARACTER_SPRITE_W, CHARACTER_SPRITE_H);
    let (dw, dh) = (w * DENSITY, h * DENSITY);
    // `variant_top`: the keys the variant's head rows use per frame — the
    // base's for a faithful upscale — or no variant at all.
    let pack = |variant_top: Option<(char, char)>| {
        let (g0, g1) = variant_top.unwrap_or((HAIR_KEY, SKIN_KEY));
        let variant = if variant_top.is_some() {
            format!(
                "[animations.\"{}@{DENSITY}x\"]\nframes=[\"g0.sprite\", \"g1.sprite\"]\nframe_ms=100\n",
                anim.name()
            )
        } else {
            String::new()
        };
        let (anchor_toml, anchor_art) = grid_anchor(DENSITY, SHIRT_KEY);
        let stride = anim.walk().map_or(String::new(), |walk| {
            format!("stride={}\n", bundled.stride(walk))
        });
        let toml = format!(
            "[palette]\n\
             \"{SHIRT_KEY}\"=\"#0a141e\"\n\"{HAIR_KEY}\"=\"#28323c\"\n\
             \"{SKIN_KEY}\"=\"#46505a\"\n\"{PANTS_KEY}\"=\"#646e78\"\n\
             [animations.{}]\nframes=[\"f0.sprite\", \"f1.sprite\"]\nframe_ms=100\n{stride}{variant}{anchor_toml}",
            anim.name()
        );
        let art = [
            ("f0.sprite", format!("@frame 0\n{}", rows(w, h, HAIR_KEY))),
            ("f1.sprite", format!("@frame 0\n{}", rows(w, h, SKIN_KEY))),
            ("g0.sprite", format!("@frame 0\n{}", rows(dw, dh, g0))),
            ("g1.sprite", format!("@frame 0\n{}", rows(dw, dh, g1))),
        ];
        let art: Vec<(&str, &str)> = art
            .iter()
            .chain(&anchor_art)
            .map(|(n, t)| (*n, t.as_str()))
            .collect();
        crate::pack::test_office_with(&toml, &art)
    };

    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let scale = RenderScale::new(DENSITY).expect("nonzero");
    let render = |pack: &OfficeArt| {
        let mut buf = RgbBuffer::filled(
            scale.to_buffer(layout.buf_w),
            scale.to_buffer(layout.buf_h),
            theme.surface.bg_fallback,
        );
        let mut cache = crate::cutaway::paint::CutawayCache::default();
        let office = crate::display::Office {
            layout: &layout,
            pack,
            theme,
            scale,
        };
        let ground = crate::floor::FloorMeta::ground();
        crate::cutaway::paint::render_cutaway(
            &frame,
            office,
            crate::display::compose::tests::showing(ground, now0),
            (&mut cache, &mut crate::outside::OutsideCache::default()),
            &mut buf,
        );
        let list = crate::display::compose(
            &frame,
            office,
            crate::display::compose::tests::showing(ground, now0),
            (
                &mut crate::display::compose::LightCache::default(),
                &mut crate::outside::OutsideCache::default(),
            ),
        );
        let anchors: Vec<_> = list.badges().map(|run| run.at).collect();
        (buf.as_slice().to_vec(), anchors)
    };

    let (base_px, base_badges) = render(&pack(None));
    let (dense_px, dense_badges) = render(&pack(Some((HAIR_KEY, SKIN_KEY))));
    assert!(
        !base_badges.is_empty(),
        "the person must be drawn to be compared"
    );
    assert_eq!(
        dense_badges, base_badges,
        "the badge moved with the art's density"
    );
    assert!(
        dense_px == base_px,
        "the person's pixels moved with the art's density"
    );
    let (other_px, _) = render(&pack(Some((PANTS_KEY, PANTS_KEY))));
    assert!(
        other_px != base_px,
        "the cutaway did not draw from the variant"
    );
}

/// The TOML and art of a pet shipped at `density` too, drawn in palette `key`:
/// both packs of a with-and-without-variant comparison carry it, so both paint
/// the room on one art grid ([`Pen::for_pack`](crate::display::pen::Pen::for_pack))
/// and differ only in the piece compared.
fn grid_anchor(density: u16, key: char) -> (String, [(&'static str, String); 2]) {
    let square =
        |n: u16| vec![vec![key.to_string(); usize::from(n)].join(" "); usize::from(n)].join("\n");
    (
        format!(
            "[animations.cat_sit]\nframes=[\"c.sprite\"]\nframe_ms=100\n\
             [animations.\"cat_sit@{density}x\"]\nframes=[\"c2.sprite\"]\nframe_ms=100\n"
        ),
        [
            ("c.sprite", format!("@frame 0\n{}", square(1))),
            ("c2.sprite", format!("@frame 0\n{}", square(density))),
        ],
    )
}

/// `rows`, a desk's cell keys, as a desk the office can draw at block size `d`:
/// the lamp's bulb in its top-left block, the cup and the tower marked.
fn marked_desk_art(rows: &str, d: u16) -> String {
    let lit = |y: usize, line: &str| -> String {
        line.split(' ')
            .enumerate()
            .map(|(x, key)| {
                if y < usize::from(d) && x < usize::from(d) {
                    crate::pack::DESK_BULB_KEY.to_string()
                } else {
                    key.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let body: Vec<_> = rows.lines().enumerate().map(|(y, l)| lit(y, l)).collect();
    format!(
        "@frame 0\n@mark cup 0 0\n@mark tower {d} 0\n{}",
        body.join("\n")
    )
}

/// The tables that draw both desks from `f.sprite`, and from `g.sprite` at
/// `density` when `variant`, and nothing on or before them, as the desk-foot
/// tests compare bare desks.
fn bare_desks_toml(density: u16, variant: bool) -> String {
    let mut toml = String::new();
    for desk in ["desk", "desk_north"] {
        toml += &format!("[animations.{desk}]\nframes=[\"f.sprite\"]\nframe_ms=100\n");
        if variant {
            toml += &format!(
                "[animations.\"{desk}@{density}x\"]\nframes=[\"g.sprite\"]\nframe_ms=100\n"
            );
        }
    }
    let bare = [
        Piece::DeskFront,
        Piece::DeskChair,
        Piece::DeskCup,
        Piece::TokenTower,
    ]
    .into_iter()
    .chain([Piece::TokenSheet]);
    for piece in bare {
        toml += &format!(
            "[animations.{}]\nframes=[\"t.sprite\"]\nframe_ms=100\n",
            piece.name()
        );
    }
    for piece in Piece::VARIANTS
        .iter()
        .filter(|p| p.kind() == pixtuoid_core::sprite::format::PieceKind::Character)
    {
        toml += &format!(
            "[animations.{}]\nframes=[{}]\nframe_ms=100\n{}",
            piece.name(),
            vec!["\"t.sprite\""; piece.min_frames()].join(", "),
            piece.walk().map_or("", |_| "stride=1\n"),
        );
    }
    toml
}

/// A night the desk-foot tests render at: no sun spills through the windows,
/// so with the room's lights off ([`unlit_room`]) only its darkness reaches the
/// floor, which [`assert_variant_desk_foot`](crate::cutaway::paint::assert_variant_desk_foot)
/// accounts for.
fn desk_foot_hour() -> SystemTime {
    crate::localclock::at_hour(23)
}

/// `frame` with the room's own lights switched off.
fn unlit_room(frame: &SimFrame) -> SimFrame {
    SimFrame {
        indoor_scale: 0.0,
        ..frame.clone()
    }
}

/// A desk drawn from a variant that is exactly its base upscaled lands where the
/// base does — the cutaway places it by its LOGICAL size — and, being cutaway
/// art with its own front, gets no derived face
/// ([`assert_variant_desk_foot`](crate::cutaway::paint::assert_variant_desk_foot));
/// a variant drawn differently renders differently. The lit screen's pin is
/// `a_lit_desk_variant_lands_its_screen_where_the_base_does`.
#[test]
fn a_desk_variant_lands_where_the_base_does_and_draws_its_own_front() {
    use crate::render_scale::RenderScale;
    const DENSITY: u16 = 2;
    let (scene, layout, _, now0, bundled) = sim_rig();
    let coffee = HashMap::new();
    let mut owned = OwnedSimStores::new();
    let frame = sim_step(
        &mut owned.stores(),
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now: now0 + std::time::Duration::from_secs(40),
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &bundled,
            layout: &layout,
            coffee: &coffee,
            door_anim_max_ms: 0,
        },
    );

    // A desk top over a darker front row, whole-pixel so an upscale by
    // `DENSITY` is the variant exactly.
    let rows = |w: u16, h: u16, top: char| -> String {
        (0..h)
            .map(|y| {
                let key = if y + h / 3 < h { top } else { 'E' };
                vec![key.to_string(); usize::from(w)].join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (w, h) = (6, 3);
    let pack = |variant_top: Option<char>| {
        let (anchor_toml, anchor_art) = grid_anchor(DENSITY, 'D');
        let toml = format!(
            "[palette]\n\
             \"D\"=\"#6a4a2a\"\n\"E\"=\"#3a2a1a\"\n\"F\"=\"#aa2222\"\n\
             {}{anchor_toml}",
            bare_desks_toml(DENSITY, variant_top.is_some())
        );
        let art = [
            ("f.sprite", marked_desk_art(&rows(w, h, 'D'), 1)),
            (
                "g.sprite",
                marked_desk_art(
                    &rows(w * DENSITY, h * DENSITY, variant_top.unwrap_or('D')),
                    DENSITY,
                ),
            ),
            ("t.sprite", "@frame 0\n.".to_owned()),
        ];
        let art: Vec<(&str, &str)> = art
            .iter()
            .chain(&anchor_art)
            .map(|(n, t)| (*n, t.as_str()))
            .collect();
        crate::pack::test_office_with(&toml, &art)
    };

    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let scale = RenderScale::new(DENSITY).expect("nonzero");
    let render = |pack: &OfficeArt| {
        let mut buf = RgbBuffer::filled(
            scale.to_buffer(layout.buf_w),
            scale.to_buffer(layout.buf_h),
            theme.surface.bg_fallback,
        );
        let mut cache = crate::cutaway::paint::CutawayCache::default();
        crate::cutaway::paint::render_cutaway(
            &unlit_room(&frame),
            crate::display::Office {
                layout: &layout,
                pack,
                theme,
                scale,
            },
            crate::display::compose::tests::showing(
                crate::floor::FloorMeta::ground(),
                desk_foot_hour(),
            ),
            (&mut cache, &mut crate::outside::OutsideCache::default()),
            &mut buf,
        );
        buf.as_slice().to_vec()
    };

    let base_px = render(&pack(None));
    crate::cutaway::paint::assert_variant_desk_foot(
        &render(&pack(Some('D'))),
        &base_px,
        &layout,
        &pack(None),
        theme,
        scale,
        desk_foot_hour(),
    );
    assert!(
        render(&pack(Some('F'))) != base_px,
        "the cutaway did not draw the desk from the variant"
    );
}

/// A lit screen is the desk's own screen keys relit, so a desk drawn from a
/// variant that is exactly its base upscaled lights the same pixels as the
/// base, and draws its own front
/// ([`assert_variant_desk_foot`](crate::cutaway::paint::assert_variant_desk_foot)).
#[test]
fn a_lit_desk_variant_lands_its_screen_where_the_base_does() {
    use crate::layout::Facing;
    use crate::render_scale::RenderScale;
    use std::time::Duration;
    const DENSITY: u16 = 2;
    let (mut scene, layout, id, now0, bundled) = sim_rig();
    let north = layout
        .home_desks
        .iter()
        .position(|d| d.facing == Facing::North)
        .expect("the office has a back-turned desk");
    let slot = scene.agents.get_mut(&id).expect("the rig's agent");
    slot.desk_index = GlobalDeskIndex(north);
    slot.state = ActivityState::Running {
        item: Some(ToolUse {
            tool_use_id: None,
            detail: None,
            kind: ToolKind::Edit,
        }),
    };
    let coffee = HashMap::new();
    let mut owned = OwnedSimStores::new();
    // Stepped, not jumped: the sim walks the agent to their seat.
    let frame = (1..=1200u64)
        .map(|n| {
            sim_step(
                &mut owned.stores(),
                SimInputs {
                    world: FloorInputs {
                        scene: &scene,
                        now: now0 + Duration::from_millis(100 * n),
                        floor: crate::floor::FloorMeta::ground(),
                        pets: PetInputs::default(),
                    },
                    pack: &bundled,
                    layout: &layout,
                    coffee: &coffee,
                    door_anim_max_ms: 0,
                },
            )
        })
        .find(|f| {
            f.seated_agents
                .get(&FloorLocalDeskIndex(north))
                .copied()
                .unwrap_or(false)
        })
        .expect("the typing agent sits at their desk");
    let unlit = sim_step(
        &mut OwnedSimStores::new().stores(),
        SimInputs {
            world: FloorInputs {
                scene: &SceneState::uniform(16),
                now: now0,
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &bundled,
            layout: &layout,
            coffee: &coffee,
            door_anim_max_ms: 0,
        },
    );

    // A desk top, its screen glass, and a darker front row, whole-pixel so an
    // upscale by `DENSITY` is the variant exactly.
    let glass_key = crate::pack::SCREEN_GLASS_KEY;
    let rows = |w: u16, h: u16, glass: char| -> String {
        (0..h)
            .map(|y| {
                let key = match y * 3 / h {
                    0 => 'D',
                    1 => glass,
                    _ => 'E',
                };
                vec![key.to_string(); usize::from(w)].join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (w, h) = (6, 3);
    let pack = |variant_glass: Option<char>| {
        let (anchor_toml, anchor_art) = grid_anchor(DENSITY, 'D');
        let toml = format!(
            "[palette]\n\
             \"D\"=\"#6a4a2a\"\n\"E\"=\"#3a2a1a\"\n\"{glass_key}\"=\"#1c2a36\"\n\
             {}{anchor_toml}",
            bare_desks_toml(DENSITY, variant_glass.is_some())
        );
        let art = [
            ("f.sprite", marked_desk_art(&rows(w, h, glass_key), 1)),
            (
                "g.sprite",
                marked_desk_art(
                    &rows(w * DENSITY, h * DENSITY, variant_glass.unwrap_or(glass_key)),
                    DENSITY,
                ),
            ),
            ("t.sprite", "@frame 0\n.".to_owned()),
        ];
        let art: Vec<(&str, &str)> = art
            .iter()
            .chain(&anchor_art)
            .map(|(n, t)| (*n, t.as_str()))
            .collect();
        crate::pack::test_office_with(&toml, &art)
    };

    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let scale = RenderScale::new(DENSITY).expect("nonzero");
    let render = |frame: &SimFrame, pack: &OfficeArt| {
        let mut buf = RgbBuffer::filled(
            scale.to_buffer(layout.buf_w),
            scale.to_buffer(layout.buf_h),
            theme.surface.bg_fallback,
        );
        let mut cache = crate::cutaway::paint::CutawayCache::default();
        crate::cutaway::paint::render_cutaway(
            &unlit_room(frame),
            crate::display::Office {
                layout: &layout,
                pack,
                theme,
                scale,
            },
            crate::display::compose::tests::showing(
                crate::floor::FloorMeta::ground(),
                desk_foot_hour(),
            ),
            (&mut cache, &mut crate::outside::OutsideCache::default()),
            &mut buf,
        );
        buf.as_slice().to_vec()
    };

    let base_px = render(&frame, &pack(None));
    assert!(
        base_px != render(&unlit, &pack(None)),
        "the typing agent's screen must be lit, or this pins nothing"
    );
    crate::cutaway::paint::assert_variant_desk_foot(
        &render(&frame, &pack(Some(glass_key))),
        &base_px,
        &layout,
        &pack(None),
        theme,
        scale,
        desk_foot_hour(),
    );
    assert!(
        render(&frame, &pack(Some('D'))) != base_px,
        "the cutaway did not draw the lit desk from the variant"
    );
}

/// Pins [`crate::lighting::desk_screen_glow`], the one screen rule both profiles light
/// by: a seated occupant's tool, on a north-facing desk only.
#[test]
fn a_desk_screen_glows_only_for_a_seated_tool_user_facing_north() {
    use crate::layout::Facing;
    let id = pixtuoid_core::AgentId::from_transcript_path("/t.jsonl");
    let editing = make_slot(
        id,
        ActivityState::Running {
            item: Some(ToolUse {
                tool_use_id: None,
                detail: None,
                kind: ToolKind::Edit,
            }),
        },
    );
    let idle = make_slot(id, ActivityState::Idle);
    let theme = &crate::theme::NORMAL;
    let tool = work_glow_tint(&editing, &theme.tool_glow);
    assert!(tool.is_some(), "the fixture's occupant is using a tool");
    let glow = |occupant, facing, seated| {
        crate::lighting::desk_screen_glow(occupant, facing, seated, theme)
    };
    assert_eq!(glow(Some(&editing), Facing::North, true), tool);
    assert_eq!(
        glow(Some(&editing), Facing::South, true),
        None,
        "a viewer-facing desk shows its monitor's back"
    );
    assert_eq!(
        glow(Some(&editing), Facing::North, false),
        None,
        "not seated"
    );
    assert_eq!(glow(Some(&idle), Facing::North, true), None, "no tool");
    assert_eq!(glow(None, Facing::North, true), None, "nobody");
    let between_items = make_slot(id, ActivityState::Running { item: None });
    assert_eq!(
        glow(Some(&between_items), Facing::North, true),
        Some(theme.tool_glow.default),
        "an open turn works its screen, untinted"
    );
}

fn drawable(sort_row: u16) -> Drawable<'static> {
    Drawable {
        sort_row,
        layer: Layer::Under,
        hover: None,
        kind: DrawableKind::MeetingTable {
            pos: Point { x: 0, y: 0 },
        },
    }
}

#[test]
fn drawables_sort_ascending_by_sort_row() {
    let mut v = [drawable(30), drawable(10), drawable(20)];
    drawable::sort_drawables(&mut v);
    let ys: Vec<u16> = v.iter().map(|d| d.sort_row).collect();
    assert_eq!(ys, [10, 20, 30]);
}

#[test]
fn drawables_sort_is_stable_on_ties() {
    let mut v = [
        Drawable {
            sort_row: 10,
            layer: Layer::Under,
            hover: None,
            kind: DrawableKind::MeetingTable {
                pos: Point { x: 1, y: 0 },
            },
        },
        Drawable {
            sort_row: 10,
            layer: Layer::Under,
            hover: None,
            kind: DrawableKind::MeetingTable {
                pos: Point { x: 2, y: 0 },
            },
        },
        Drawable {
            sort_row: 10,
            layer: Layer::Under,
            hover: None,
            kind: DrawableKind::MeetingTable {
                pos: Point { x: 3, y: 0 },
            },
        },
    ];
    drawable::sort_drawables(&mut v);
    let xs: Vec<u16> = v
        .iter()
        .map(|d| match &d.kind {
            DrawableKind::MeetingTable { pos } => pos.x,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(xs, [1, 2, 3]);
}

/// Which of a fixture and a figure tied on a row paints on top is the layer's
/// call, not the queue's: queued in the reverse of paint order, they still
/// paint fixture under, figure, fixture over.
#[test]
fn a_tied_row_paints_by_layer_whatever_the_queue_order() {
    let tied = |layer, x| Drawable {
        sort_row: 10,
        layer,
        hover: None,
        kind: DrawableKind::MeetingTable {
            pos: Point { x, y: 0 },
        },
    };
    let mut v = [
        tied(Layer::Over, 3),
        tied(Layer::Character, 2),
        tied(Layer::Creature, 4),
        tied(Layer::Under, 1),
    ];
    drawable::sort_drawables(&mut v);
    let layers: Vec<Layer> = v.iter().map(|d| d.layer).collect();
    assert_eq!(
        layers,
        [Layer::Under, Layer::Creature, Layer::Character, Layer::Over]
    );
}

/// The layer only breaks a tie: a row north still paints first.
#[test]
fn a_row_north_paints_first_whatever_its_layer() {
    let at = |sort_row, layer, x| Drawable {
        sort_row,
        layer,
        hover: None,
        kind: DrawableKind::MeetingTable {
            pos: Point { x, y: 0 },
        },
    };
    let mut v = [at(10, Layer::Under, 2), at(5, Layer::Over, 1)];
    drawable::sort_drawables(&mut v);
    let rows: Vec<u16> = v.iter().map(|d| d.sort_row).collect();
    assert_eq!(rows, [5, 10]);
}

#[test]
fn pet_sort_row_tracks_the_selected_anim_sprite_height() {
    let pack = crate::pack::test_office();
    let pos = Point { x: 40, y: 30 };
    let anim_h = |walk: Piece| pack.piece(walk).first().height();
    for &kind in crate::pet::PetKind::ALL {
        let sleep_h = anim_h(kind.sleep_anim());
        let sleep = sort_row_at(Pivot::Center, pos, sleep_h);
        let walk = sort_row_at(Pivot::Center, pos, anim_h(kind.walk_anim().piece()));
        let sit = sort_row_at(Pivot::Center, pos, anim_h(kind.sit_anim()));
        assert!(
            sleep <= walk && sleep <= sit,
            "{kind:?}: shorter sleep sprite must not sort south of walk/sit \
             (sleep={sleep}, walk={walk}, sit={sit})",
        );
        assert_eq!(
            sleep,
            pos.y + (sleep_h - 1) / 2,
            "{kind:?}: sleep pet must land on its sprite's south row",
        );
    }
}

/// The chair, its occupant and the walk that ends there all derive from ONE
/// point, the way a meeting seat derives from `wp.pos` — so the seeded nudge
/// cannot slide any one of them off the others.
#[test]
fn a_seeded_seat_moves_the_chair_the_sitter_and_the_walk_together() {
    use crate::layout::Facing;
    let mut offsets = std::collections::BTreeSet::new();
    for x in (20..120).step_by(7) {
        for y in (20..90).step_by(5) {
            let desk = Point { x, y };
            for facing in [Facing::South, Facing::North] {
                let centre = crate::layout::desk_walk_anchor_facing(desk, facing);
                // the sitter and the chair both centre on it
                for w in [CHARACTER_SPRITE_W, 10] {
                    assert_eq!(
                        seated_top_left(desk, w, facing).x,
                        centre.x - w / 2,
                        "{desk:?} {facing:?} w={w}: sprite must centre on the seat"
                    );
                }
                let base = desk.x + crate::layout::desk_furniture_def().visual.w / 2;
                offsets.insert(i32::from(centre.x) - i32::from(base));
            }
        }
    }
    assert!(
        offsets.iter().all(|o| o.abs() <= 2),
        "the nudge must stay within ±2 of the midline, saw {offsets:?}"
    );
    assert!(
        offsets.len() >= 3,
        "the seed must actually vary the offset, saw {offsets:?}"
    );
}

#[test]
fn desk_walk_anchor_settles_exactly_on_the_seat() {
    for desk in [
        Point { x: 40, y: 30 },
        Point { x: 100, y: 60 },
        Point { x: 7, y: 5 }, // near-origin: saturating_sub edge
    ] {
        for w in [CHARACTER_SPRITE_W, 10] {
            // Only X has teeth — on Y both facings reduce to the same `saturating_sub`.
            // Y drift: `a_back_turned_seat_puts_the_occupant_past_the_desk_body`.
            for facing in [crate::layout::Facing::South, crate::layout::Facing::North] {
                assert_eq!(
                    walking_top_left(crate::layout::desk_walk_anchor_facing(desk, facing), w),
                    seated_top_left(desk, w, facing),
                    "walking_top_left(desk_walk_anchor_facing({desk:?}, {facing:?}), {w}) \
                     must equal seated_top_left",
                );
            }
        }
    }
}

#[test]
fn seated_foot_cell_settles_exactly_on_the_render_top_left() {
    use crate::layout::{Furniture, seated_foot_cell};
    for pos in [
        Point { x: 40, y: 30 },
        Point { x: 100, y: 60 },
        Point { x: 6, y: 8 }, // near-origin: saturating_sub edge
    ] {
        for w in [CHARACTER_SPRITE_W, 10] {
            for f in [Furniture::Couch, Furniture::MeetingSofa] {
                let s = seated_foot_cell(f, pos).expect("occupies_pos seat");
                assert_eq!(
                    walking_top_left(s, w),
                    back_couch_top_left(pos, w),
                    "{f:?}: walking_top_left(S={s:?}) must equal back_couch_top_left(pos={pos:?}) w={w}",
                );
            }
            let s = seated_foot_cell(Furniture::MeetingChair, pos).expect("occupies_pos seat");
            assert_eq!(
                walking_top_left(s, w),
                back_couch_top_left(pos, w),
                "MeetingChair: walking_top_left(S={s:?}) must equal back_couch_top_left(pos={pos:?}) w={w}",
            );
            let sd = seated_foot_cell(Furniture::Desk, pos).expect("desk is occupies_pos");
            assert_eq!(
                walking_top_left(sd, w),
                seated_top_left(pos, w, crate::layout::Facing::South),
                "Desk: walking_top_left(seated_foot_cell)={:?} must equal seated_top_left",
                walking_top_left(sd, w),
            );
        }
        assert_eq!(seated_foot_cell(Furniture::Pantry, pos), None);
        assert_eq!(seated_foot_cell(Furniture::VendingMachine, pos), None);
    }
}

#[test]
fn settle_view_matches_the_seated_view_for_every_seat() {
    use crate::layout::{Facing, TEST_DEFAULT_DESKS, WaypointKind};
    let l = SceneLayout::compute(192, 158, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let seats: Vec<_> = l
        .waypoints
        .iter()
        .filter(|w| crate::layout::seated_foot_cell(w.kind.furniture(), w.pos).is_some())
        .collect();
    assert!(
        seats.iter().any(
            |w| matches!(w.kind, WaypointKind::Couch | WaypointKind::MeetingSofa)
                && w.facing == Facing::North
        ),
        "this layout size must have a window-facing (North) seat to exercise the fix"
    );
    for w in &seats {
        let foot = crate::layout::seated_foot_cell(w.kind.furniture(), w.pos)
            .expect("seat occupies_pos → has a settle foot cell");
        let seat = Seat::at_waypoint(w.kind, w.pos, w.facing);
        assert_eq!(
            settle_seat(foot, &l),
            Some(seat),
            "settle onto {:?}@{:?} must resolve to that seat",
            w.kind,
            w.pos
        );
        assert!(
            matches!(
                w.kind,
                WaypointKind::Couch
                    | WaypointKind::MeetingSofa
                    | WaypointKind::MeetingChair
                    | WaypointKind::Island
            ),
            "seat kind {:?} has a settle foot-cell but is not explicitly handled \
             in Seat::view — add an arm there",
            w.kind
        );
        let seated_is_back = seat.sprite_for(Piece::Seated).0 == Piece::BackCouch;
        let (settle_is_back, _) = seat.settle_walk();
        assert_eq!(
            seated_is_back, settle_is_back,
            "{:?}: seated render and sit-down settle must share orientation",
            w.kind
        );
        if foot != w.pos {
            assert_eq!(
                settle_seat(w.pos, &l),
                None,
                "seat centre {:?} is not a settle foot cell",
                w.pos
            );
        }
    }
}

#[test]
fn island_settle_z_stays_behind_the_countertop() {
    use crate::layout::{Furniture, Pivot, TEST_DEFAULT_DESKS, WaypointKind};
    let mut exercised = false;
    for seed in 0..5u64 {
        let Some(l) = SceneLayout::compute_with_seed(240, 160, Some(TEST_DEFAULT_DESKS), seed)
        else {
            continue;
        };
        let Some(island) = l.pantry.and_then(|p| p.kitchen_island) else {
            continue;
        };
        exercised = true;
        let island_sort_row = crate::layout::sort_row_at(
            Pivot::Center,
            island,
            crate::layout::furniture_def(Furniture::KitchenIsland)
                .visual
                .h,
        );
        for wp in l
            .waypoints
            .iter()
            .filter(|w| matches!(w.kind, WaypointKind::Island))
        {
            let z = settle_seat(wp.pos, &l)
                .expect("island stand foot-cell == pos, so it settles")
                .sort_row();
            assert_eq!(
                z, wp.pos.y,
                "island stand glide z must be the plain feet row (the settled \
                 AtWaypoint key), not a Side-style +3"
            );
            assert!(
                z < island_sort_row,
                "stand z {z} must sort BEHIND the island's south-row key {island_sort_row}"
            );
        }
    }
    assert!(exercised, "no seed hosted the island — test lost its teeth");
}

#[test]
fn settle_seat_recognizes_the_home_desk() {
    use crate::layout::TEST_DEFAULT_DESKS;
    use crate::layout::{Furniture, desk_walk_anchor_facing};
    let l = SceneLayout::compute(192, 158, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let desk = *l.home_desks.first().expect("at least one home desk");
    let chair = desk_walk_anchor_facing(desk.at, desk.facing);
    // Pinning `Front` unconditionally would assert the pre-facing world.
    let want = Seat::at_desk(desk.at, desk.facing);
    assert_eq!(
        settle_seat(chair, &l),
        Some(want),
        "the desk chair {chair:?} must settle as that seat"
    );
    assert_eq!(
        want.sort_row(),
        chair.y,
        "and sort on its own chair row, not a couch-style pos+2"
    );
    // `seated_foot_cell` is facing-BLIND — it takes a kind and a position, so its
    // desk arm can only answer for the viewer-facing seat.
    assert_eq!(
        crate::layout::seated_foot_cell(Furniture::Desk, desk.at),
        Some(crate::layout::desk_walk_anchor_facing(
            desk.at,
            crate::layout::Facing::South
        ))
    );
    assert_eq!(
        settle_seat(desk.at, &l),
        None,
        "the desk corner is not the chair"
    );
}

#[test]
fn desk_sitter_feet_row_is_its_sort_row_on_the_documented_side_of_the_desk() {
    use crate::layout::{Facing, desk_furniture_def};
    for desk in [Point { x: 40, y: 30 }, Point { x: 100, y: 60 }] {
        for facing in [Facing::North, Facing::South] {
            for w in [CHARACTER_SPRITE_W, 10] {
                let seat = Seat::at_desk(desk, facing);
                assert_eq!(
                    seat.render_top_left(w).y + crate::layout::WALKING_Y_OFF,
                    seat.sort_row(),
                    "{facing:?}: the sitter's feet row must BE the row they sort on"
                );
                let desk_z = desk.y + desk_furniture_def().visual.h;
                let sitter_sort_row = seat.sort_row();
                assert_eq!(
                    sitter_sort_row < desk_z,
                    facing == Facing::South,
                    "{facing:?}: a viewer-facing sitter sorts BEHIND their desk \
                     ({sitter_sort_row} < {desk_z}), a back-turned one in FRONT"
                );
            }
        }
    }
}

#[test]
fn sit_arc_sort_row_is_stable_and_on_the_right_side_of_its_furniture() {
    use crate::layout::{
        Facing, Furniture, Pivot, TEST_DEFAULT_DESKS, WaypointKind, furniture_def, sort_row_at,
    };
    let l = SceneLayout::compute(192, 158, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let mut saw_back = false;
    for w in l
        .waypoints
        .iter()
        .filter(|w| crate::layout::seated_foot_cell(w.kind.furniture(), w.pos).is_some())
    {
        let z = Seat::at_waypoint(w.kind, w.pos, w.facing).sort_row();

        // Independent oracle: the pre-lift partition, keyed on kind alone.
        let historical = match w.kind {
            // The chair shares the front seat's top-left and bottom row by design.
            WaypointKind::Couch | WaypointKind::MeetingSofa | WaypointKind::MeetingChair => {
                back_couch_top_left(w.pos, CHARACTER_SPRITE_W).y + 9
            }
            // The AtWaypoint default.
            WaypointKind::Island => waypoint_top_left(w.pos, CHARACTER_SPRITE_W).y + 12,
            _ => unreachable!("{:?} has a foot cell but no oracle arm — add one", w.kind),
        };
        assert_eq!(
            z, historical,
            "{:?}@{:?}: seat sort row {z} must equal the historical AtWaypoint key {historical}",
            w.kind, w.pos
        );

        match w.kind {
            WaypointKind::Couch => {
                let couch_sort_row = sort_row_at(
                    Pivot::Center,
                    w.pos,
                    furniture_def(Furniture::Couch).visual.h,
                );
                assert!(
                    z < couch_sort_row,
                    "couch sitter z {z} must be BEHIND the couch back {couch_sort_row}"
                );
                saw_back = true;
            }
            // The sofa ties its sitters; its roster `Tie` decides who covers whom
            // (`a_seat_ties_its_sitter_and_hides_them_only_from_behind`).
            WaypointKind::MeetingSofa => {
                assert_eq!(
                    z,
                    crate::layout::seated_sort_row(w.pos),
                    "sofa sitter z {z} must tie the sofa"
                );
                saw_back |= w.facing == Facing::North;
            }
            WaypointKind::MeetingChair => {
                assert!(
                    z > w.pos.y + 1,
                    "chair sitter z {z} must clear the chair body at pos.y+1"
                );
            }
            _ => {}
        }
    }
    assert!(
        saw_back,
        "layout must contain a back-view seat to exercise the flicker fix"
    );
}

/// The geometry table's desk height must match the ART's, or the desk's sort row
/// lands on a south row the sprite does not reach. The `- 1`: `desk` blits at `desk.y - 1`
/// (top row is the north-overhanging bezel), so it covers `height - 1` from `desk.y`.
#[test]
fn desk_sort_row_is_the_visual_south() {
    let pack = crate::pack::test_office();
    let art = pack.piece(Piece::Desk).first();
    assert_eq!(
        crate::layout::desk_furniture_def().visual.h,
        art.height() - 1,
        "the desk's visual height must equal the rows its sprite covers from \
         desk.y down (sprite {} rows, blitted one above desk.y)",
        art.height()
    );
}

/// The painter centres a pack sprite by the ART's size, while its sort row,
/// ground strip and the binary's hover box place it by the size the layout
/// reads — a def's `.visual`, the elevator's, a counter's — so the two must
/// agree for the bundled pack. The desk is the exception: its box starts under
/// the bezel row it blits above `desk.y` (`desk_sort_row_is_the_visual_south`).
#[test]
fn every_hover_size_is_its_painted_sprite_size() {
    use crate::layout::{
        COMPACT_COUNTER, ELEVATOR_H, ELEVATOR_W, Furniture, LARGE_COUNTER, PlantKind, PodDecor,
        Size, WallDecor, furniture_def,
    };
    let def = |f: Furniture, sprite: Piece| (format!("{f:?}"), furniture_def(f).visual, sprite);
    let mut pieces: Vec<(String, Size, Piece)> = vec![
        def(Furniture::MeetingSofaBody, Piece::MeetingSofa),
        def(Furniture::MeetingSofaBody, Piece::MeetingSofaNorth),
        def(Furniture::SnackShelf, Piece::SnackShelf),
        def(Furniture::FloorLamp, Piece::FloorLamp),
        def(Furniture::VendingMachine, Piece::VendingMachine),
        def(Furniture::Printer, Piece::Printer),
        def(Furniture::MeetingTable, Piece::MeetingTable),
        def(Furniture::DeskChair, Piece::DeskChair),
        def(Furniture::FilingCabinet, Piece::FilingCabinet),
        (
            "ELEVATOR".into(),
            Size {
                w: ELEVATOR_W,
                h: ELEVATOR_H,
            },
            Piece::Door,
        ),
        (
            "LARGE_COUNTER".into(),
            LARGE_COUNTER,
            crate::layout::pantry_counter_anim(LARGE_COUNTER.w),
        ),
        (
            "COMPACT_COUNTER".into(),
            COMPACT_COUNTER,
            crate::layout::pantry_counter_anim(COMPACT_COUNTER.w),
        ),
    ];
    pieces.extend(PlantKind::ALL.iter().map(|k| def(k.furniture(), k.piece())));
    pieces.extend(WallDecor::ALL.iter().map(|k| def(k.furniture(), k.piece())));
    pieces.extend(PodDecor::ALL.iter().map(|k| def(k.furniture(), k.piece())));

    let pack = crate::pack::test_office();
    for (name, size, sprite) in pieces {
        for (i, art) in pack.piece(sprite).frames().iter().enumerate() {
            assert_eq!(
                (size.w, size.h),
                (art.width(), art.height()),
                "{name}'s size must be {}'s painted size (frame {i})",
                sprite.name()
            );
        }
    }
}

/// A frame with nobody in it, for `layout`.
fn empty_frame(layout: &SceneLayout) -> SimFrame {
    SimFrame {
        agents: Vec::new(),
        poses: HashMap::new(),
        seated_agents: HashMap::new(),
        characters: Vec::new(),
        indoor_scale: 0.0,
        neon: crate::floor::NeonLevels::EMPTY,
        neon_stutter: false,
        chitchat_bubbles: Vec::new(),
        new_coffee_carriers: Vec::new(),
        occupied_waypoints: Default::default(),
        pet: None,
        mascots: Vec::new(),
        desks: vec![Default::default(); layout.home_desks.len()],
        door_frame: 0,
    }
}

/// The classic's queue of `layout`'s fixtures on `frame`.
fn queued(layout: &SceneLayout, frame: &SimFrame) -> Furnishings<'static> {
    let pack = crate::pack::test_office();
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (scene, walks) = (SceneState::uniform(16), HashMap::new());
    let now = SystemTime::UNIX_EPOCH;
    let mut buf = RgbBuffer::filled(layout.buf_w, layout.buf_h, Rgb { r: 0, g: 0, b: 0 });
    let (mut cache, mut base_fill) = (FrameCache::new(), BaseFillCache::new());
    let ctx = PaintCtx {
        scene: &scene,
        layout,
        pack: &pack,
        timing: Motion::Full.timing(now),
        sky: crate::sky::Sky::clock(now),
        window_sky: None,
        buf: &mut buf,
        cache: &mut cache,
        base_fill: &mut base_fill,
        shadows: &mut crate::ground::DepthsCache::default(),
        theme,
        floor: crate::floor::FloorMeta::ground(),
        walks: &walks,
        debug_walkable: false,
        outside: &mut crate::outside::OutsideCache::default(),
    };
    let lights = crate::lighting::Lights::of(
        layout,
        &crate::atmosphere::SkyTones::resolve(&ctx.sky, theme),
        &crate::lighting::LightInputs {
            agents: &frame.agents,
            seated: &frame.seated_agents,
            floor_idx: 0,
            indoor_scale: frame.indoor_scale,
            neon: frame.neon,
            beat: ctx.timing.beat,
            bulbs: crate::lighting::DeskBulbs::of(ctx.pack),
        },
    );
    queue_fixtures(
        &ctx,
        frame,
        &lights.desks,
        crate::floor::neon_look(frame.neon, theme),
    )
}

fn paints_as(kind: crate::layout::FixtureKind) -> &'static str {
    use crate::layout::{FixtureKind, Station};
    match kind {
        FixtureKind::Desk(_) => "desk",
        FixtureKind::FilingCabinet(_) => "cabinet",
        FixtureKind::DeskChair(_) => "desk chair",
        FixtureKind::Station { station, .. } => match station {
            Station::PantryCounter => "pantry counter",
            Station::VendingMachine | Station::Printer => "appliance",
            Station::SnackShelf => "snack shelf",
        },
        FixtureKind::Plant { .. } => "plant",
        FixtureKind::Pod { .. } => "pod",
        FixtureKind::Wall { .. } => "wall decor",
        FixtureKind::MeetingRug { .. }
        | FixtureKind::LoungeRug
        | FixtureKind::PantryMat
        | FixtureKind::IslandMat => "rug",
        FixtureKind::MeetingSofa { .. } | FixtureKind::LoungeCouch => "sofa",
        FixtureKind::MeetingTable { .. } => "table",
        FixtureKind::MeetingChair { .. } => "meeting chair",
        FixtureKind::CoatRack { .. } => "coat rack",
        FixtureKind::Doormat { .. } => "doormat",
        FixtureKind::NoticeBoard { .. } => "notice board",
        FixtureKind::SideTable => "side table",
        FixtureKind::FloorLamp => "floor lamp",
        FixtureKind::FishTank => "fish tank",
        FixtureKind::KitchenIsland => "kitchen island",
        FixtureKind::WaterCooler => "water cooler",
        FixtureKind::TrashBin => "trash bin",
        FixtureKind::Door => "door",
        FixtureKind::Runner => "runner",
        FixtureKind::NeonSign => "neon sign",
        FixtureKind::Clock => "clock",
    }
}

/// What a queued fixture drawable is, in [`paints_as`]'s words.
fn drawn_as(kind: &DrawableKind<'_>) -> &'static str {
    match kind {
        DrawableKind::DeskCubicle { .. } => "desk",
        DrawableKind::FilingCabinet { .. } => "cabinet",
        DrawableKind::DeskChair { .. } => "desk chair",
        DrawableKind::WaypointPantry { .. } => "pantry counter",
        DrawableKind::Appliance { .. } => "appliance",
        DrawableKind::SnackShelf { .. } => "snack shelf",
        DrawableKind::Plant { .. } => "plant",
        DrawableKind::PodDecorItem { .. } => "pod",
        DrawableKind::WallDecor { .. } => "wall decor",
        DrawableKind::AreaRug(_) => "rug",
        DrawableKind::MeetingSofa { .. } => "sofa",
        DrawableKind::MeetingTable { .. } => "table",
        DrawableKind::MeetingChair { .. } => "meeting chair",
        DrawableKind::CoatRack { .. } => "coat rack",
        DrawableKind::Doormat(_) => "doormat",
        DrawableKind::NoticeBoard(_) => "notice board",
        DrawableKind::LoungeSideTable { .. } => "side table",
        DrawableKind::FloorLamp { .. } => "floor lamp",
        DrawableKind::FishTank { .. } => "fish tank",
        DrawableKind::KitchenIsland { .. } => "kitchen island",
        DrawableKind::WaterCooler(_) => "water cooler",
        DrawableKind::TrashBin(_) => "trash bin",
        DrawableKind::Door { .. } => "door",
        DrawableKind::Runner(_) => "runner",
        DrawableKind::NeonSign { .. } => "neon sign",
        DrawableKind::Clock { .. } => "clock",
        DrawableKind::Character { .. }
        | DrawableKind::Pet { .. }
        | DrawableKind::GatewayMascot { .. }
        | DrawableKind::RoomWall { .. } => panic!("queue_fixtures queues only fixtures"),
    }
}

/// The classic queues every fixture the roster yields, as what it is, at the
/// roster's depth and in roster order: a fixture it drops, or draws as another,
/// fails here.
#[test]
fn the_classic_queues_every_fixture_the_roster_yields() {
    use crate::layout::{Depth, Tie};
    for layout in crate::layout::roster::tests::offices() {
        let q = queued(&layout, &empty_frame(&layout));
        let got: Vec<(Option<(u16, Layer)>, &str)> = q
            .backdrop
            .iter()
            .map(|(_, k)| (None, drawn_as(k)))
            .chain(
                q.sorted
                    .iter()
                    .map(|d| (Some((d.sort_row, d.layer)), drawn_as(&d.kind))),
            )
            .collect();
        let want: Vec<(Option<(u16, Layer)>, &str)> = layout
            .fixtures()
            .map(|f| {
                let depth = match f.depth {
                    Depth::Backdrop => None,
                    Depth::Sorted {
                        row,
                        tie: Tie::FigureOver,
                    } => Some((row, Layer::Under)),
                    Depth::Sorted {
                        row,
                        tie: Tie::FixtureOver,
                    } => Some((row, Layer::Over)),
                };
                (depth, paints_as(f.kind))
            })
            .collect();
        assert_eq!(got, want, "{}x{}", layout.buf_w, layout.buf_h);
    }
}

/// The self-lit wall fixtures are the only ones spared the hour's wash; the
/// runner, which once stayed daylight-tan in a dimmed office, is washed.
#[test]
fn only_the_neon_sign_and_the_clock_are_spared_the_wash() {
    use crate::layout::FixtureKind;
    for layout in crate::layout::roster::tests::offices() {
        for f in layout.fixtures() {
            let spared = matches!(f.kind, FixtureKind::NeonSign | FixtureKind::Clock);
            assert_eq!(wash_of(f.kind) == Wash::Spared, spared, "{:?}", f.kind);
        }
    }
    assert_eq!(wash_of(FixtureKind::Runner), Wash::Washed);
}

/// The background pass paints every spared fixture before every washed one,
/// so the roster must list them that way or the pass paints out of its order.
#[test]
fn the_roster_lists_spared_backdrop_before_washed() {
    use crate::layout::Depth;
    for layout in crate::layout::roster::tests::offices() {
        let washes: Vec<Wash> = layout
            .fixtures()
            .filter(|f| f.depth == Depth::Backdrop)
            .map(|f| wash_of(f.kind))
            .collect();
        assert!(
            washes
                .windows(2)
                .all(|w| !(w[0] == Wash::Washed && w[1] == Wash::Spared)),
            "{washes:?}"
        );
    }
}

/// Only the background pass can spare a fixture the hour's wash: the
/// foreground one reaches everything the y-sort paints.
#[test]
fn only_a_backdrop_fixture_is_spared_the_wash() {
    use crate::layout::Depth;
    let mut spared = 0;
    for layout in crate::layout::roster::tests::offices() {
        for f in layout
            .fixtures()
            .filter(|f| wash_of(f.kind) == Wash::Spared)
        {
            assert_eq!(f.depth, Depth::Backdrop, "{:?}", f.kind);
            spared += 1;
        }
    }
    assert!(spared > 0, "the sweep spares some fixture");
}

/// A seat sorts at its sitter's own row, so its roster tie alone decides which
/// paints on top: the sitter on a front sofa or a meeting chair, and a desk
/// chair or a sofa we see the back of over its sitter.
#[test]
fn a_seat_ties_its_sitter_and_hides_them_only_from_behind() {
    use crate::layout::{Depth, Facing, FixtureKind, Tie, WaypointKind};
    let (mut fronts, mut backs, mut chairs, mut meeting_chairs) = (0, 0, 0, 0);
    for layout in crate::layout::roster::tests::offices() {
        let fixtures: Vec<_> = layout.fixtures().collect();
        for w in layout
            .waypoints
            .iter()
            .filter(|w| w.kind == WaypointKind::MeetingSofa)
        {
            let back = w.facing == Facing::North;
            let sofa = fixtures
                .iter()
                .find(|f| {
                    matches!(f.kind, FixtureKind::MeetingSofa { room, faces_away, .. }
                        if Some(room) == w.room_id && faces_away == back)
                })
                .expect("its sofa");
            let tie = if back {
                backs += 1;
                Tie::FixtureOver
            } else {
                fronts += 1;
                Tie::FigureOver
            };
            let row = Seat::at_waypoint(w.kind, w.pos, w.facing).sort_row();
            assert_eq!(sofa.depth, Depth::Sorted { row, tie }, "{w:?}");
        }
        for w in layout
            .waypoints
            .iter()
            .filter(|w| w.kind == WaypointKind::MeetingChair)
        {
            let chair = fixtures
                .iter()
                .find(|f| f.at == w.pos && matches!(f.kind, FixtureKind::MeetingChair { .. }))
                .expect("its chair");
            meeting_chairs += 1;
            let row = Seat::at_waypoint(w.kind, w.pos, w.facing).sort_row();
            assert_eq!(
                chair.depth,
                Depth::Sorted {
                    row,
                    tie: Tie::FigureOver
                },
                "{w:?}"
            );
        }
        for f in &fixtures {
            if let FixtureKind::DeskChair(i) = f.kind {
                chairs += 1;
                let desk = layout.home_desks[i.0];
                let row = Seat::at_desk(desk.at, desk.facing).sort_row();
                assert_eq!(
                    f.depth,
                    Depth::Sorted {
                        row,
                        tie: Tie::FixtureOver
                    }
                );
            }
        }
    }
    assert!(
        fronts > 0 && backs > 0 && chairs > 0 && meeting_chairs > 0,
        "the sweep seats every case"
    );
}

#[test]
fn every_pod_occludes_via_overhang() {
    use crate::layout::{PodDecor, Size, furniture_def};
    assert_eq!(
        PodDecor::ALL.len(),
        5,
        "PodDecor variant added/removed — update ALL (and this count)"
    );
    for &kind in PodDecor::ALL {
        let def = furniture_def(kind.furniture());
        assert!(
            def.visual.h > 0,
            "{kind:?}: pod decor needs a non-zero visual height for the z-sort"
        );
        let Size { h: fh, .. } = def.footprint.expect("aisle pod has a ground footprint");
        assert!(
            def.visual.h > fh,
            "{kind:?}: aisle pod must overhang its footprint to occlude (visual.h {} > footprint.h {fh})",
            def.visual.h
        );
    }
}

fn entry_slot(created_at_ms_ago: u64, now: SystemTime) -> AgentSlot {
    let id = pixtuoid_core::AgentId::from_transcript_path("/door.jsonl");
    let mut s = make_slot(id, ActivityState::Idle);
    s.created_at = now - std::time::Duration::from_millis(created_at_ms_ago);
    s
}

fn exit_slot(exit_ms_ago: u64, now: SystemTime) -> AgentSlot {
    let id = pixtuoid_core::AgentId::from_transcript_path("/exit.jsonl");
    let mut s = make_slot(id, ActivityState::Idle);
    s.created_at = now - std::time::Duration::from_secs(300);
    s.exiting_at = Some(now - std::time::Duration::from_millis(exit_ms_ago));
    s
}

#[test]
fn door_frame_closed_when_no_agents() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    assert_eq!(compute_door_frame_idx(&[], now, 0), 0);
}

#[test]
fn door_frame_just_spawned_is_half_open() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    // 50 ms into the `DOOR_TRANSITION_MS` opening ramp — first half = frame 1.
    let slot = entry_slot(50, now);
    assert_eq!(compute_door_frame_idx(&[slot], now, 0), 1);
}

#[test]
fn door_frame_after_opening_ramp_is_fully_open() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    // 150 ms (still inside opening ramp but past midpoint) → frame 2.
    let s1 = entry_slot(150, now);
    assert_eq!(compute_door_frame_idx(&[s1], now, 0), 2);
    // 2 s into the `ENTRY_ANIMATION_MS` window → fully open.
    let s2 = entry_slot(2_000, now);
    assert_eq!(compute_door_frame_idx(&[s2], now, 0), 2);
}

#[test]
fn door_frame_closing_then_closed_at_end_of_entry() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    // 150 ms left in the entry window → closing ramp first half → frame 1.
    let mid_close = entry_slot(pose::ENTRY_ANIMATION_MS - 150, now);
    assert_eq!(compute_door_frame_idx(&[mid_close], now, 0), 1);
    // 50 ms left → closing ramp final half → frame 0 (closed).
    let near_end = entry_slot(pose::ENTRY_ANIMATION_MS - 50, now);
    assert_eq!(compute_door_frame_idx(&[near_end], now, 0), 0);
}

#[test]
fn door_frame_expired_entry_contributes_nothing() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    // Older than the entry window → no contribution.
    let old = entry_slot(pose::ENTRY_ANIMATION_MS + 1, now);
    assert_eq!(compute_door_frame_idx(&[old], now, 0), 0);
}

#[test]
fn door_frame_is_fully_open_mid_exit() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let exiting = exit_slot(2_000, now);
    assert_eq!(compute_door_frame_idx(&[exiting], now, 0), 2);
}

#[test]
fn door_frame_takes_max_across_agents() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let opening = entry_slot(50, now);
    let open = entry_slot(2_000, now);
    assert_eq!(compute_door_frame_idx(&[opening, open], now, 0), 2);
}

#[test]
fn door_frame_uses_physics_window_when_nonzero() {
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    // A short physics window replaces ENTRY_ANIMATION_MS as the total.
    let short_window_ms: u64 = 2_500;
    // elapsed 3000 > total 2500 → remaining 0 → closed.
    let slot = entry_slot(3_000, now);
    let frame = compute_door_frame_idx(&[slot], now, short_window_ms);
    assert_eq!(
        frame, 0,
        "with short physics window elapsed>total should yield closed door, got frame {frame}"
    );

    // 500 ms into the 2500 ms window → still mid-flight.
    let slot_mid = entry_slot(500, now);
    let frame_mid = compute_door_frame_idx(&[slot_mid], now, short_window_ms);
    assert_eq!(
        frame_mid, 2,
        "500ms into 2500ms window should be fully open, got frame {frame_mid}"
    );
}

#[test]
fn waypoint_rank_offset_x_decollision_table() {
    use crate::layout::WaypointKind;
    use crate::sim::anchors::waypoint_rank_offset_x;
    assert_eq!(waypoint_rank_offset_x(WaypointKind::Couch, 0), 0);
    assert_eq!(waypoint_rank_offset_x(WaypointKind::Pantry, 0), 0);
    assert_eq!(waypoint_rank_offset_x(WaypointKind::Pantry, 1), 9);
    assert_eq!(waypoint_rank_offset_x(WaypointKind::Pantry, 2), -9);
    assert_eq!(
        waypoint_rank_offset_x(WaypointKind::Pantry, 5),
        0,
        "rank >2 collapses to 0"
    );
}

#[test]
fn no_exclusive_waypoint_kind_ever_steps_aside() {
    use crate::layout::{WaypointKind, furniture_def};
    use crate::sim::anchors::waypoint_rank_offset_x;
    let mut exclusive = 0;
    let (mut saw_booth, mut saw_shareable_steps) = (false, false);
    for &kind in WaypointKind::ALL {
        if furniture_def(kind.furniture()).exclusive {
            exclusive += 1;
            if matches!(kind, WaypointKind::PhoneBooth) {
                saw_booth = true;
            }
            for rank in 0..4 {
                assert_eq!(
                    waypoint_rank_offset_x(kind, rank),
                    0,
                    "{kind:?} is exclusive — rank {rank} must not slide it off the spot"
                );
            }
        } else if waypoint_rank_offset_x(kind, 1) != 0 {
            saw_shareable_steps = true;
        }
    }
    assert!(
        exclusive >= 6,
        "expected couch/sofa/chair/island + booth + standing desk, got {exclusive}"
    );
    assert!(saw_booth, "phone booth must be an exclusive spot");
    assert!(
        saw_shareable_steps,
        "a shareable queue spot (pantry/vending/printer/snack) must still step aside"
    );
}

#[test]
fn degraded_pixel_desaturates_reddens_and_dims() {
    // Expected value hand-traced through the three blend stages: desaturate,
    // red tint, dim.
    assert_eq!(
        palette::degraded_pixel(Rgb {
            r: 255,
            g: 255,
            b: 255
        }),
        Rgb {
            r: 171,
            g: 130,
            b: 130
        },
    );
    let out = palette::degraded_pixel(Rgb { r: 0, g: 255, b: 0 });
    assert!(
        out.r > out.b,
        "red bias must lift r above b for a pure-green input: {out:?}"
    );
    assert!(
        out.r > 0,
        "the red bias must raise r above the input's 0: {out:?}"
    );
    assert!(
        out.g < 255 && out.r < 255 && out.b < 255,
        "every channel dimmed below its bright max: {out:?}"
    );
}

#[test]
fn degraded_frame_transforms_opaque_pixels_and_preserves_transparency_and_dims() {
    let frame = Frame::from_pixels(
        2,
        1,
        vec![
            Some(Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            None,
        ],
    );
    let out = palette::degraded_frame(&frame);
    assert_eq!(out.width(), 2);
    assert_eq!(out.height(), 1);
    assert_eq!(
        out.as_slice()[0],
        Some(palette::degraded_pixel(Rgb {
            r: 255,
            g: 255,
            b: 255
        }))
    );
    assert_eq!(
        out.as_slice()[1],
        None,
        "transparent pixel must stay transparent"
    );
}

#[test]
fn obstacle_kinds_render_upright_and_unflipped() {
    use crate::layout::{Facing, WaypointKind};
    assert_eq!(
        Seat::at_waypoint(WaypointKind::Pantry, Point { x: 40, y: 30 }, Facing::South)
            .sprite_for(Piece::Seated),
        (Piece::HoldingCoffee, false),
        "the pantry is the one stand-beside spot with art of its own"
    );
    for kind in [
        WaypointKind::PhoneBooth,
        WaypointKind::StandingDesk,
        WaypointKind::VendingMachine,
        WaypointKind::Printer,
    ] {
        assert_eq!(
            Seat::at_waypoint(kind, Point { x: 40, y: 30 }, Facing::South)
                .sprite_for(Piece::Seated),
            (Piece::Standing, false),
            "{kind:?} must render as the upright default",
        );
    }
}

#[test]
fn top_tier_slot_paints_ember_hair_and_a_flame_crown() {
    use pixtuoid_core::state::EffortObservation;
    use std::time::Duration;
    let pack = crate::pack::test_office();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let black = Rgb { r: 0, g: 0, b: 0 };
    let top_left = Point { x: 8, y: 8 };
    let mut slot = make_slot(
        pixtuoid_core::AgentId::from_parts("claude-code", "ses_burn"),
        ActivityState::Idle,
    );

    let render = |slot: &pixtuoid_core::AgentSlot| {
        let mut buf = RgbBuffer::filled(32, 32, black);
        let drawn = paint_character_at(
            &mut buf,
            crate::character::SpritePose {
                anim_name: Piece::Seated,
                frame_idx: 0,
                flip_x: false,
                glow_tint: None,
            },
            top_left,
            slot,
            &pack,
            &mut FrameCache::new(),
            now,
        );
        let fx = crate::sim::character_effects(
            slot,
            top_left,
            drawn.w,
            crate::sim::Cues::default(),
            Motion::Full.timing(now),
        );
        super::effects::paint_effects(
            &mut buf,
            &fx,
            crate::theme::theme_by_name("normal").expect("normal theme"),
        );
        buf
    };
    let has = |buf: &RgbBuffer, c: Rgb| {
        (0..buf.height()).any(|y| (0..buf.width()).any(|x| buf.get(x, y) == c))
    };
    const EMBER: Rgb = crate::effects::look::FLAME_DEEP;
    const TIP: Rgb = crate::effects::look::FLAME_TIP;

    let plain = render(&slot);
    assert!(
        !has(&plain, EMBER) && !has(&plain, TIP),
        "Normal must not burn"
    );

    slot.model = Some("claude-fable-5".into());
    let ember = render(&slot);
    assert!(has(&ember, EMBER), "Premium recolors the hair to ember");
    assert!(!has(&ember, TIP), "Premium must not flame");
    assert_ne!(plain.as_slice(), ember.as_slice());

    slot.effort = Some(EffortObservation::new("ultra".into(), now));
    let burning = render(&slot);
    assert!(has(&burning, TIP), "Top paints flame tips");
    let above = (0..top_left.y).any(|y| (0..32).any(|x| burning.get(x, y) != black));
    assert!(above, "the crown must rise above the sprite's top row");

    slot.effort = Some(EffortObservation::new(
        "ultra".into(),
        now - Duration::from_secs(crate::burn::EFFORT_TTL_SECS + 1),
    ));
    let decayed = render(&slot);
    assert!(!has(&decayed, TIP), "stale effort must decay the flame");
    assert!(has(&decayed, EMBER), "…back to ember hair");
}

/// The sim crowns a Top-burning agent's placement on its post-breath top-left,
/// centred on its pack frame; a Premium one burns no crown.
#[test]
fn a_top_burning_placement_carries_its_crown_on_its_top_left() {
    use crate::effects::EffectKind;
    use crate::pose::Pose;
    use pixtuoid_core::state::EffortObservation;
    let (mut scene, layout, id, now0, pack) = sim_rig();
    // A breathing instant, where the post-breath top-left is off the fit.
    let now = (0..u64::from(u16::MAX))
        .map(|ms| now0 + std::time::Duration::from_millis(ms))
        .find(|&t| {
            crate::sim::anchors::with_breath(Point { x: 0, y: 1 }, id, Motion::Full.beat(t)).y == 0
        })
        .expect("the breath rises within a cycle");
    let slot = scene.agents.get_mut(&id).expect("the rig's agent");
    slot.model = Some("claude-fable-5".into());
    let crowns = |scene: &SceneState| {
        let agents: Vec<AgentSlot> = scene.agents.values().cloned().collect();
        let poses = HashMap::from([(id, Some(Pose::SeatedTyping))]);
        let (placements, ..) = crate::sim::resolve_characters(
            &agents,
            &poses,
            &layout,
            &pack,
            &HashMap::new(),
            Motion::Full.timing(now),
        );
        let [p] = <[_; 1]>::try_from(placements).expect("one agent, one placement");
        let crowns: Vec<Point> = p
            .effects
            .iter()
            .filter(|e| e.kind == EffectKind::FlameCrown)
            .map(|e| e.at)
            .collect();
        (p, crowns)
    };
    assert!(crowns(&scene).1.is_empty(), "Premium must not flame");

    let slot = scene.agents.get_mut(&id).expect("the rig's agent");
    slot.effort = Some(EffortObservation::new("ultra".into(), now));
    let (p, crowns) = crowns(&scene);
    let w = crate::sim::pack_frame_size(&pack, p.anim_name, p.frame_idx).w;
    assert_eq!(
        crowns,
        [Point {
            x: p.top_left.x + w / 2,
            y: p.top_left.y
        }]
    );
}

#[test]
fn glass_wall_h_clamps_below_buffer_bottom() {
    // `y_face` at the buffer's last row runs the face past it, so the pen's
    // clip drops the rows below.
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let bh = 16u16;
    let mut buf = RgbBuffer::filled(40, bh, Rgb { r: 0, g: 0, b: 0 });
    paint_whole_wall(
        &mut buf,
        theme,
        crate::layout::WallPiece::Horizontal {
            x0: 0,
            x1: 39,
            y_face: bh - 1,
            jamb_west: false,
            jamb_east: false,
        },
    );
    let mut painted = false;
    for y in 0..bh {
        for x in 0..40u16 {
            if buf.get(x, y) != (Rgb { r: 0, g: 0, b: 0 }) {
                painted = true;
            }
        }
    }
    assert!(painted, "in-bounds glass rows should still paint");
}

#[test]
fn glass_wall_v_clamps_past_right_edge() {
    // `x` at the last column runs the strip past the width, so the pen's clip
    // drops the columns beyond. Must not panic.
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let bw = 12u16;
    let mut buf = RgbBuffer::filled(bw, 40, Rgb { r: 0, g: 0, b: 0 });
    paint_whole_wall(
        &mut buf,
        theme,
        crate::layout::WallPiece::Vertical {
            x: bw - 1,
            y_top: 5,
            north: 5,
            y_bot: 20,
            south: 20,
            jamb_north: false,
            jamb_south: false,
        },
    );
    let mut painted = false;
    for y in 5..21u16 {
        if buf.get(bw - 1, y) != (Rgb { r: 0, g: 0, b: 0 }) {
            painted = true;
        }
    }
    assert!(painted, "the in-bounds glass column should paint");
}

#[test]
fn pet_hearts_skip_dead_and_faded_hearts() {
    let bg = Rgb { r: 0, g: 0, b: 0 };
    let cat_pos = Point { x: 20, y: 20 };
    let painted_count = |elapsed_ms: u64| -> usize {
        let mut buf = RgbBuffer::filled(40, 40, bg);
        let hearts: Vec<_> = crate::effects::pet_hearts(cat_pos, elapsed_ms).collect();
        super::effects::paint_effects(
            &mut buf,
            &hearts,
            crate::theme::theme_by_name("normal").expect("normal theme"),
        );
        (0..40u16)
            .flat_map(|y| (0..40u16).map(move |x| (x, y)))
            .filter(|&(x, y)| buf.get(x, y) != bg)
            .count()
    };
    assert_eq!(
        painted_count(2_100),
        0,
        "all hearts past their life → none paint"
    );
    assert!(painted_count(0) > 0, "first heart paints at t=0");
    let faded = painted_count(1_500);
    assert!(
        faded <= painted_count(300),
        "the faded heart drops out (alpha<0.05)"
    );
}

#[test]
fn the_notice_board_paints_its_box() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let bg = Rgb { r: 9, g: 9, b: 9 };
    let big = crate::layout::Bounds {
        x: 4,
        y: 4,
        width: 40,
        height: 40,
    };
    let mut buf = RgbBuffer::filled(120, 80, bg);
    super::furniture::paint_notice_board(&mut buf, big, theme);
    let painted = (0..80u16)
        .flat_map(|y| (0..120u16).map(move |x| (x, y)))
        .any(|(x, y)| buf.get(x, y) != bg);
    assert!(painted, "the notice board must paint its box");
}

#[test]
fn furniture_painters_fill_exactly_their_rect_authority() {
    use super::furniture::{paint_doormat, paint_trash_bin, paint_water_cooler};
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let bg = Rgb { r: 1, g: 2, b: 3 };
    let big = crate::layout::Bounds {
        x: 4,
        y: 4,
        width: 44,
        height: 44,
    };
    let pantry = crate::layout::PantryRoom {
        bounds: big,
        counter_size: crate::layout::COMPACT_COUNTER,
        kitchen_island: None,
    };
    let meeting = crate::layout::MeetingRoom {
        bounds: big,
        trio: None,
    };
    let painted_bbox = |f: &dyn Fn(&mut RgbBuffer)| -> Option<crate::layout::Bounds> {
        let mut buf = RgbBuffer::filled(120, 80, bg);
        f(&mut buf);
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (u16::MAX, u16::MAX, 0u16, 0u16);
        let mut any = false;
        for y in 0..buf.height() {
            for x in 0..buf.width() {
                if buf.get(x, y) != bg {
                    any = true;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        any.then(|| crate::layout::Bounds {
            x: min_x,
            y: min_y,
            width: max_x - min_x + 1,
            height: max_y - min_y + 1,
        })
    };
    let bin = pantry.trash_bin_rect().expect("fits");
    let mat = meeting.doormat_rect().expect("fits");
    let cooler = pantry.water_cooler_rect().expect("fits");
    assert_eq!(
        painted_bbox(&|b| paint_trash_bin(b, bin)),
        Some(bin),
        "trash bin paints exactly its rect",
    );
    assert_eq!(
        painted_bbox(&|b| paint_doormat(b, mat, theme)),
        Some(mat),
        "doormat paints exactly its rect",
    );
    assert_eq!(
        painted_bbox(&|b| paint_water_cooler(
            b,
            cooler,
            Motion::Full.beat(std::time::SystemTime::UNIX_EPOCH),
            theme
        )),
        Some(cooler),
        "water cooler paints exactly its rect (glug bubble stays inside)",
    );
}

#[test]
fn furniture_corner_clip_does_not_panic() {
    use super::furniture::{paint_area_rug, paint_side_table};
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    // Centre each piece near the (0,0) corner so part of the sprite has a
    // negative px/py, exercising the `< 0` / out-of-range `continue` clamps.
    let mut buf = RgbBuffer::filled(40, 40, Rgb { r: 0, g: 0, b: 0 });
    // A rug's box can't reach west of the buffer, only past its far corner.
    let rug = crate::layout::Bounds {
        x: 35,
        y: 36,
        width: 10,
        height: 8,
    };
    paint_area_rug(&mut buf, rug, theme);
    paint_side_table(&mut buf, 1, 1, theme);
    super::furniture::paint_kitchen_island(&mut buf, 1, 1, theme);
    // No panic reaching here is the assertion (negative coords are clipped).
}

#[test]
fn weather_gallery_manifest_matches_the_weather_enum() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../site/src/weather.json");
    let json = match std::fs::read_to_string(path) {
        Ok(s) => s,
        // crates.io-packaged test runs don't ship the repo's site/ tree.
        Err(_) => {
            eprintln!("skipping: {path} not present (packaged build)");
            return;
        }
    };
    let manifest: Vec<serde_json::Value> =
        serde_json::from_str(&json).expect("weather.json parses");
    let ids: Vec<&str> = manifest
        .iter()
        .map(|w| {
            w["id"]
                .as_str()
                .expect("weather.json entry has a string id")
        })
        .collect();
    assert_eq!(
        ids,
        crate::sky::weather_names(),
        "site/src/weather.json ids must match Weather::ALL names in order — \
         update the manifest + run `just gen-media` when the enum changes"
    );
}

#[test]
fn cwd_backfill_invalidates_cached_outfit_frames() {
    let pack = crate::pack::test_office();
    let unknown = make_slot_cwd("/p/heal.jsonl", "", true);
    // Pick a cwd whose Team-Palette outfit differs from the id-seeded fallback,
    // or the assertion has no teeth.
    let healed = (0..64)
        .map(|i| make_slot_cwd("/p/heal.jsonl", &format!("/repo/team{i}"), false))
        .find(|h| color_of(h, SHIRT_KEY) != color_of(&unknown, SHIRT_KEY))
        .expect("some cwd lands on a different outfit than the fallback");

    let top_left = Point { x: 2, y: 2 };
    let black = Rgb { r: 0, g: 0, b: 0 };
    let mut cache = FrameCache::new();
    let mut before = RgbBuffer::filled(24, 24, black);
    paint_character_at(
        &mut before,
        crate::character::SpritePose {
            anim_name: Piece::Seated,
            frame_idx: 0,
            flip_x: false,
            glow_tint: None,
        },
        top_left,
        &unknown,
        &pack,
        &mut cache,
        SystemTime::UNIX_EPOCH,
    );

    let mut after = RgbBuffer::filled(24, 24, black);
    paint_character_at(
        &mut after,
        crate::character::SpritePose {
            anim_name: Piece::Seated,
            frame_idx: 0,
            flip_x: false,
            glow_tint: None,
        },
        top_left,
        &healed,
        &pack,
        &mut cache,
        SystemTime::UNIX_EPOCH,
    );

    let mut fresh = RgbBuffer::filled(24, 24, black);
    paint_character_at(
        &mut fresh,
        crate::character::SpritePose {
            anim_name: Piece::Seated,
            frame_idx: 0,
            flip_x: false,
            glow_tint: None,
        },
        top_left,
        &healed,
        &pack,
        &mut FrameCache::new(),
        SystemTime::UNIX_EPOCH,
    );

    assert_ne!(
        before.as_slice(),
        after.as_slice(),
        "the healed cwd must change the painted outfit"
    );
    assert_eq!(
        after.as_slice(),
        fresh.as_slice(),
        "the healed repaint must match a fresh render, not the stale cached outfit"
    );
}

struct OwnedSimStores {
    route: pose::RouteRig<crate::pathfind::AStarRouter>,
    vacancy_dim: VacancyDim,
    neon: crate::floor::NeonState,
    chitchat: std::collections::HashMap<crate::chitchat::VenueKey, crate::chitchat::ActiveChitchat>,
    creatures:
        std::collections::HashMap<crate::creatures::CreatureKey, crate::creatures::CreatureWalk>,
    grip: crate::interact::Grip,
}

impl OwnedSimStores {
    fn new() -> Self {
        Self {
            route: pose::RouteRig::new(crate::pathfind::AStarRouter::new()),
            vacancy_dim: VacancyDim::new(),
            neon: crate::floor::NeonState::new(),
            chitchat: std::collections::HashMap::new(),
            creatures: std::collections::HashMap::new(),
            grip: crate::interact::Grip::default(),
        }
    }

    fn stores(&mut self) -> SimStores<'_> {
        SimStores {
            router: &mut self.route.router,
            overlay: &mut self.route.overlay,
            history: &mut self.route.history,
            walks: &mut self.route.walks,
            vacancy_dim: &mut self.vacancy_dim,
            neon: &mut self.neon,
            chitchat: &mut self.chitchat,
            creatures: &mut self.creatures,
            grip: &mut self.grip,
        }
    }
}

fn sim_rig() -> (
    SceneState,
    SceneLayout,
    pixtuoid_core::AgentId,
    SystemTime,
    OfficeArt,
) {
    let pack = crate::pack::test_office();
    let layout = SceneLayout::compute_with_seed(160, 96, None, 0).expect("160x96 lays out");
    let now0 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let id = pixtuoid_core::AgentId::from_transcript_path("/p/sim-seam.jsonl");
    let mut slot = make_slot(id, ActivityState::Idle);
    slot.created_at = now0;
    slot.state_started_at = now0;
    slot.last_event_at = now0;
    let mut scene = SceneState::uniform(16);
    scene.agents.insert(id, slot);
    (scene, layout, id, now0, pack)
}

/// `sim_step` must hand the sign the room's VERDICT, not its smoothed level: the
/// level trails the verdict by the length of its ease, so a level-based join
/// starves the sign in the gap after a populated room's last agent walks out.
#[test]
fn sim_step_keeps_the_sign_lit_through_a_gap_in_a_room_that_once_dimmed() {
    use std::time::Duration;
    let (populated, layout, _, now0, pack) = sim_rig();
    let empty = SceneState::uniform(16);
    let coffee = std::collections::HashMap::new();
    let mut owned = OwnedSimStores::new();
    let frame = Duration::from_millis(crate::anim::PAINT_FRAME_MS);
    let mut now = now0;
    let mut run = |scene: &SceneState, ms: u64| {
        let mut last = None;
        for _ in 0..ms / frame.as_millis() as u64 {
            now += frame;
            last = Some(
                sim_step(
                    &mut owned.stores(),
                    SimInputs {
                        world: FloorInputs {
                            scene,
                            now,
                            floor: crate::floor::FloorMeta::ground(),
                            pets: PetInputs::default(),
                        },
                        pack: &pack,
                        layout: &layout,
                        coffee: &coffee,
                        door_anim_max_ms: 0,
                    },
                )
                .neon,
            );
        }
        last.expect("at least one frame")
    };
    let debounce = crate::floor::VacancyDim::EMPTY_DEBOUNCE_MS;
    assert_eq!(run(&empty, debounce * 3), crate::floor::NeonLevels::EMPTY);
    run(&populated, 60_000);
    // A gap between transcripts: the tally is empty, the room's debounce is not up.
    let gap = run(&empty, debounce / 2);
    assert_eq!(
        gap,
        crate::floor::NeonLevels::CALM,
        "a lit room keeps its sign"
    );
}

/// The cutaway reads `seat_desk` for its chair and its suppressed contact
/// shadow; the classic painter never reads it. So flipping an arm to `None`
/// breaks the second profile while every classic assertion stays green.
///
/// Driven through the real `sim_step` rather than by constructing a placement,
/// so it pins what the sim DECIDES, not what a fixture was handed.
#[test]
fn seat_desk_is_set_exactly_when_the_sim_seats_someone_at_a_desk() {
    use crate::pose::Pose;
    use std::time::Duration;
    let (scene, layout, id, now0, pack) = sim_rig();
    let coffee = HashMap::new();
    let mut owned = OwnedSimStores::new();
    let mut stores = owned.stores();

    // Far enough past the entry walk that the agent has arrived and sat down.
    let mut seated_seen = false;
    let mut walking_seen = false;
    for ms in [50u64, 250, 1_000, 4_000, 12_000, 40_000] {
        let f = sim_step(
            &mut stores,
            SimInputs {
                world: FloorInputs {
                    scene: &scene,
                    now: now0 + Duration::from_millis(ms),
                    floor: crate::floor::FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                pack: &pack,
                layout: &layout,
                coffee: &coffee,
                door_anim_max_ms: 0,
            },
        );
        let Some(c) = f.characters.first() else {
            continue;
        };
        match f.poses.get(&id) {
            Some(Some(Pose::Walking { .. })) => {
                walking_seen = true;
                assert_eq!(
                    c.seat_desk, None,
                    "a WALKING agent is not seated at a desk (t={ms}ms)"
                );
            }
            Some(Some(Pose::SeatedIdle | Pose::SeatedTyping)) => {
                seated_seen = true;
                let desk = c.seat_desk.expect("a seated agent carries its desk");
                assert!(
                    layout.home_desks.contains(&desk),
                    "the carried desk must be one the layout placed (t={ms}ms)"
                );
            }
            _ => {}
        }
    }
    assert!(walking_seen, "the sweep never observed a walking pose");
    assert!(seated_seen, "the sweep never observed a seated pose");
}

/// A desk's props are the sim's call, from its occupant, so no painter
/// re-decides the cup or the tower.
#[test]
fn sim_step_decides_each_desks_props_from_its_occupant() {
    use std::time::Duration;
    let (mut scene, layout, id, now0, pack) = sim_rig();
    scene
        .agents
        .get_mut(&id)
        .expect("the rig's agent")
        .tokens_used = crate::token_meter::TIER_BASE_TOKENS;
    scene
        .agents
        .get_mut(&id)
        .expect("the rig's agent")
        .last_usage = Some(pixtuoid_core::state::UsageObservation::new(
        crate::token_meter::SHEET_MIN_DELTA_TOKENS,
        now0,
    ));
    let desk = scene.agents[&id].desk_index.single_floor_local().0;
    let coffee = HashMap::from([(id, now0)]);
    let desks_at = |now| {
        let mut owned = OwnedSimStores::new();
        sim_step(
            &mut owned.stores(),
            SimInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: crate::floor::FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                pack: &pack,
                layout: &layout,
                coffee: &coffee,
                door_anim_max_ms: 0,
            },
        )
        .desks
    };
    let fresh = desks_at(now0);
    assert_eq!(fresh.len(), layout.home_desks.len());
    assert_eq!(fresh[desk].cup, Some(crate::sim::Cup::Steaming));
    assert_eq!(fresh[desk].token_tier, 1);
    assert_eq!(
        fresh[desk].sheet_fall,
        Some(0),
        "a big reading drops a sheet"
    );
    for (i, props) in fresh.iter().enumerate() {
        assert_eq!(
            props.scanline,
            crate::sim::scanline_col(layout.home_desks[i].at.x, Motion::Full.beat(now0)),
            "desk {i}'s scanline is its own column's"
        );
        if i != desk {
            assert_eq!(
                *props,
                crate::sim::DeskProps {
                    scanline: props.scanline,
                    ..Default::default()
                },
                "desk {i} has no occupant"
            );
        }
    }
    let cold = desks_at(now0 + Duration::from_secs(crate::floor::CoffeeState::STEAM_WINDOW_SECS));
    assert_eq!(
        cold[desk].cup,
        Some(crate::sim::Cup::Cold),
        "the cup stays after it stops steaming"
    );
    assert_eq!(cold[desk].sheet_fall, None, "and the sheet has landed");
}

#[test]
fn sim_step_roams_the_pet_and_holds_a_petted_one_where_it_stands() {
    let (scene, layout, _, now0, pack) = sim_rig();
    let coffee = HashMap::new();
    let pet = crate::pet::Pet::defaulted(crate::pet::PetKind::Cat);
    let floor = crate::floor::FloorMeta::ground();
    let step = |pets| {
        let mut owned = OwnedSimStores::new();
        sim_step(
            &mut owned.stores(),
            SimInputs {
                world: FloorInputs {
                    scene: &scene,
                    now: now0,
                    floor,
                    pets,
                },
                pack: &pack,
                layout: &layout,
                coffee: &coffee,
                door_anim_max_ms: 0,
            },
        )
        .pet
    };
    assert!(
        step(PetInputs::default()).is_none(),
        "no pet on a floor without one"
    );
    let roaming = step(PetInputs {
        pet: Some(&pet),
        petting: None,
    })
    .expect("the cat roams");
    let hearts = |p: &crate::sim::PetPlacement| -> Vec<u64> {
        p.effects
            .iter()
            .filter(|e| e.kind == crate::effects::EffectKind::PetHeart)
            .map(|e| e.phase)
            .collect()
    };
    assert!(
        hearts(&roaming).is_empty(),
        "a roaming cat is not being petted"
    );

    let petting = crate::pet::PetState {
        petted_at: now0,
        kind: pet.kind,
        floor_idx: floor.floor_idx,
    };
    let held = step(PetInputs {
        pet: Some(&pet),
        petting: Some(&petting),
    })
    .expect("the petted cat");
    assert_eq!(
        held.pos, roaming.pos,
        "held where it stands, not where the click landed"
    );
    assert_eq!(held.anim_name, pet.kind.sit_anim());
    assert_eq!(
        hearts(&held),
        [0],
        "petted just now: its first heart leaves"
    );

    let upstairs = crate::pet::PetState {
        floor_idx: floor.floor_idx + 1,
        ..petting
    };
    let elsewhere = step(PetInputs {
        pet: Some(&pet),
        petting: Some(&upstairs),
    })
    .expect("the cat roams");
    assert!(
        hearts(&elsewhere).is_empty(),
        "a petting on another floor leaves this one roaming"
    );
    let a_dog = crate::pet::PetState {
        kind: crate::pet::PetKind::Dog,
        ..petting
    };
    let other_kind = step(PetInputs {
        pet: Some(&pet),
        petting: Some(&a_dog),
    })
    .expect("the cat roams");
    assert!(
        hearts(&other_kind).is_empty(),
        "petting another kind of pet leaves the cat roaming"
    );
}

/// A mascot hovers as its gateway's instance, and its sprite greys exactly
/// when its gateway is degraded.
#[test]
fn a_mascot_hovers_as_its_instance_and_greys_when_degraded() {
    let pack = crate::pack::test_office();
    let def = crate::creatures::gateway_mascot_def(pixtuoid_core::source::openclaw::SOURCE_NAME)
        .expect("openclaw has a mascot");
    for degraded in [false, true] {
        let mascot = crate::sim::MascotPlacement {
            pos: Point { x: 60, y: 60 },
            sort_row: 60,
            anim_name: def.walk.piece(),
            frame_idx: 0,
            key: crate::creatures::openclaw_key("18789"),
            degraded,
            on_roster: true,
            effects: Vec::new(),
        };
        let mut drawables = Vec::new();
        enqueue_gateway_mascots(&pack, std::slice::from_ref(&mascot), &mut drawables);
        let [
            Drawable {
                hover: Some(hover),
                kind:
                    DrawableKind::GatewayMascot {
                        degraded: drawn, ..
                    },
                ..
            },
        ] = drawables.as_slice()
        else {
            panic!("one drawn mascot");
        };
        assert_eq!(
            hover.target,
            HoverTarget::Mascot(mascot.key.clone()),
            "the hover"
        );
        assert_eq!(*drawn, degraded, "the sprite");
    }
}

#[test]
fn sim_step_walks_a_mascot_in_for_each_gateway_present() {
    use pixtuoid_core::source::daemon::{DaemonInstanceKey, DaemonPresenceUpdate, apply_presence};
    use pixtuoid_core::state::DaemonInstanceId;
    use std::time::Duration;
    let (mut scene, layout, _, now0, pack) = sim_rig();
    let coffee = HashMap::new();
    let key = DaemonInstanceKey::new(
        pixtuoid_core::source::openclaw::SOURCE_NAME,
        DaemonInstanceId::new("18789".to_string()).expect("id"),
    );
    apply_presence(
        &mut scene,
        &key,
        DaemonPresenceUpdate::GatewayUp { pid: Some(7) },
        now0,
    );
    let mut owned = OwnedSimStores::new();
    let frame = sim_step(
        &mut owned.stores(),
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now: now0 + Duration::from_secs(6),
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &pack,
            layout: &layout,
            coffee: &coffee,
            door_anim_max_ms: 0,
        },
    );
    let [mascot] = frame.mascots.as_slice() else {
        panic!("one gateway, one mascot: {:?}", frame.mascots);
    };
    let card = crate::creatures::GatewayCard::of(&scene, &mascot.key).expect("its card");
    assert_eq!(card.name, "OpenClaw");
    assert_eq!(card.instance, None, "a lone instance needs no port");
}

/// A drawn mascot hovers sized by the frame it blitted.
#[test]
fn a_mascot_hovers_sized_by_its_frame() {
    use pixtuoid_core::source::daemon::{DaemonInstanceKey, DaemonPresenceUpdate, apply_presence};
    use pixtuoid_core::state::DaemonInstanceId;
    use std::time::Duration;
    let (mut scene, layout, _, now0, pack) = sim_rig();
    let key = DaemonInstanceKey::new(
        pixtuoid_core::source::openclaw::SOURCE_NAME,
        DaemonInstanceId::new("18789".to_string()).expect("id"),
    );
    apply_presence(
        &mut scene,
        &key,
        DaemonPresenceUpdate::GatewayUp { pid: Some(7) },
        now0,
    );
    let now = now0 + Duration::from_secs(6);
    let mut owned = OwnedSimStores::new();
    let frame = sim_step(
        &mut owned.stores(),
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now,
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &pack,
            layout: &layout,
            coffee: &HashMap::new(),
            door_anim_max_ms: 0,
        },
    );
    let [drawn] = frame.mascots.as_slice() else {
        panic!("one gateway, one mascot: {:?}", frame.mascots);
    };
    let art = {
        let f = pack.piece(drawn.anim_name).frame_at(drawn.frame_idx);
        (f.width(), f.height())
    };

    let hovers = paint_drawn(&owned, &scene, &layout, &pack, now, &frame).hovers;
    let listed: Vec<_> = hovers
        .listed()
        .iter()
        .filter(|h| matches!(h.target, HoverTarget::Mascot(_)))
        .map(|h| (h.target.clone(), (h.at.width, h.at.height)))
        .collect();
    assert_eq!(listed, vec![(HoverTarget::Mascot(key), art)]);
}

#[test]
fn sim_step_fits_every_mascot_frame_on_the_canvas() {
    use pixtuoid_core::source::daemon::{DaemonInstanceKey, DaemonPresenceUpdate, apply_presence};
    use pixtuoid_core::state::DaemonInstanceId;
    use std::time::Duration;
    let (mut scene, layout, _, now0, pack) = sim_rig();
    let coffee = HashMap::new();
    for port in 0..32 {
        let key = DaemonInstanceKey::new(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            DaemonInstanceId::new(format!("{}", 18789 + port)).expect("id"),
        );
        apply_presence(
            &mut scene,
            &key,
            DaemonPresenceUpdate::GatewayUp { pid: Some(7) },
            now0,
        );
    }
    let mut owned = OwnedSimStores::new();
    let mut edge = false;
    for s in 0..600 {
        let frame = sim_step(
            &mut owned.stores(),
            SimInputs {
                world: FloorInputs {
                    scene: &scene,
                    now: now0 + Duration::from_secs(s),
                    floor: crate::floor::FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                pack: &pack,
                layout: &layout,
                coffee: &coffee,
                door_anim_max_ms: 0,
            },
        );
        for m in &frame.mascots {
            let size = crate::sim::pack_frame_size(&pack, m.anim_name, m.frame_idx);
            let (Some(x0), Some(y0)) = (
                m.pos.x.checked_sub(size.w / 2),
                m.pos.y.checked_sub(size.h / 2),
            ) else {
                panic!("{m:?} overruns the canvas's west or north edge");
            };
            assert!(
                x0 + size.w <= layout.buf_w && y0 + size.h <= layout.buf_h,
                "{m:?} overruns the canvas"
            );
            edge |= x0 == 0 || y0 == 0;
        }
    }
    assert!(edge, "the sweep never walked a mascot to the canvas edge");
}

#[test]
fn sim_step_advances_walks_without_painting() {
    use crate::pose::Pose;
    use std::time::Duration;
    let (scene, layout, id, now0, pack) = sim_rig();
    let coffee = HashMap::new();

    let mut owned = OwnedSimStores::new();
    let mut stores = owned.stores();

    let travelled = |f: &SimFrame| match f.poses.get(&id) {
        Some(Some(Pose::Walking { segment, .. })) => segment.travelled(),
        other => panic!("expected an entry walk pose, got {other:?}"),
    };
    let f1 = sim_step(
        &mut stores,
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now: now0 + Duration::from_millis(50),
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &pack,
            layout: &layout,
            coffee: &coffee,
            door_anim_max_ms: 0,
        },
    );
    let f2 = sim_step(
        &mut stores,
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now: now0 + Duration::from_millis(250),
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &pack,
            layout: &layout,
            coffee: &coffee,
            door_anim_max_ms: 0,
        },
    );
    assert!(
        travelled(&f2) > travelled(&f1),
        "entry walk must progress between ticks: {} -> {}",
        travelled(&f1),
        travelled(&f2)
    );
    assert!(
        f2.characters
            .iter()
            .any(|c| c.anim_name.name().starts_with("walking")),
        "the tick's placements carry the walking sprite"
    );
    let _ = stores;
    assert!(
        owned
            .route
            .walks
            .get(&id)
            .is_some_and(|walk| walk.entry.is_some()),
        "sim_step snapshotted the entry walk profile into the walks map"
    );
}

/// RequiresAction has NO pose of its own: the agent stays SEATED and the bubble carries
/// the state. So the cue must ride the agent's STATE, not the pose — and must
/// read the same on a back-turned desk, which has no requires-action art and needs none.
#[test]
fn a_requires_action_agent_stays_seated_and_gets_its_bubble_whichever_way_the_desk_faces() {
    let (mut scene, layout, id, now0, pack) = sim_rig();
    let coffee = HashMap::new();
    let mut seen = std::collections::BTreeSet::new();
    for (i, &HomeDesk { facing, .. }) in layout.home_desks.iter().enumerate() {
        let slot = scene.agents.get_mut(&id).expect("the rig's agent");
        slot.desk_index = GlobalDeskIndex(i);
        slot.state = ActivityState::RequiresAction {
            reason: Some("perm".into()),
        };
        // Past the entry walk, or the agent is still coming in the door.
        let now = now0 + std::time::Duration::from_millis(crate::pose::ENTRY_ANIMATION_MS + 5_000);
        slot.created_at = now0;
        slot.state_started_at = now0;
        let mut owned = OwnedSimStores::new();
        let f = sim_step(
            &mut owned.stores(),
            SimInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: crate::floor::FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                pack: &pack,
                layout: &layout,
                coffee: &coffee,
                door_anim_max_ms: 0,
            },
        );
        let p = f
            .characters
            .iter()
            .find(|p| p.seat_desk.is_some())
            .expect("a requires-action agent is SEATED at its desk");
        let carries = |kind| p.effects.iter().any(|e| e.kind == kind);
        assert!(
            carries(crate::effects::EffectKind::RequiresActionMark),
            "{facing:?} desk {i}: a requires-action agent must carry the bubble"
        );
        // RequiresAction rides the SeatedIdle pose, whose default sprite is the
        // sleeping one — a waiter drawn asleep inverts the state's meaning.
        assert!(
            !carries(crate::effects::EffectKind::SleepZ)
                && !p.anim_name.name().contains("sleeping"),
            "{facing:?} desk {i}: a requires-action agent must be AWAKE, saw {} with {:?}",
            p.anim_name.name(),
            p.effects
        );
        seen.insert(format!("{facing:?}"));
    }
    assert_eq!(
        seen.len(),
        2,
        "the sweep must cover BOTH facings, saw {seen:?}"
    );
}

/// An agent off the layout's desks draws nothing, so hover never names it; the
/// rest are listed in paint order.
#[test]
fn the_hover_list_omits_the_undrawn_and_follows_sort_drawables() {
    use std::time::Duration;
    let (mut scene, layout, _, now0, pack) = sim_rig();
    scene.agents.clear();
    let slot = |path: &str, desk: usize, created: SystemTime| {
        let mut s = make_slot(
            pixtuoid_core::AgentId::from_transcript_path(path),
            ActivityState::Idle,
        );
        s.desk_index = GlobalDeskIndex(desk);
        (s.created_at, s.state_started_at, s.last_event_at) = (created, created, created);
        s
    };
    let off = slot("/hover/off.jsonl", layout.home_desks.len(), now0);
    let lead = slot("/hover/lead.jsonl", 0, now0);
    let trail = slot("/hover/trail.jsonl", 1, now0 + Duration::from_millis(150));
    for s in [&off, &lead, &trail] {
        scene.agents.insert(s.agent_id, s.clone());
    }
    let now = now0 + Duration::from_millis(400);
    let mut owned = OwnedSimStores::new();
    let frame = sim_step(
        &mut owned.stores(),
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now,
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &pack,
            layout: &layout,
            coffee: &HashMap::new(),
            door_anim_max_ms: 0,
        },
    );
    let mut buf = RgbBuffer::filled(layout.buf_w, layout.buf_h, Rgb { r: 0, g: 0, b: 0 });
    let hover = paint_frame(
        &mut PaintCtx {
            scene: &scene,
            layout: &layout,
            pack: &pack,
            timing: Motion::Full.timing(now),
            sky: crate::sky::Sky::clock(now),
            window_sky: None,
            buf: &mut buf,
            cache: &mut FrameCache::new(),
            base_fill: &mut BaseFillCache::new(),
            shadows: &mut crate::ground::DepthsCache::default(),
            theme: crate::theme::theme_by_name("normal").expect("normal theme"),
            floor: crate::floor::FloorMeta::ground(),
            walks: &owned.route.walks,
            debug_walkable: false,
            outside: &mut crate::outside::OutsideCache::default(),
        },
        &frame,
    );

    let queued: Vec<_> = frame
        .characters
        .iter()
        .map(|c| (c.sort_row, frame.agents[c.agent_idx].agent_id))
        .collect();
    let mut sorted = queued.clone();
    // Every character is a `Layer::Character`, so `sort_drawables` orders them by row alone.
    sorted.sort_by_key(|&(row, _)| row);
    assert_ne!(sorted, queued, "premise: paint order is not the queue's");
    let listed = badged_ids(&hover);
    assert_eq!(listed, sorted.iter().map(|&(_, id)| id).collect::<Vec<_>>());
    let hovered: Vec<_> = hover
        .hovers
        .listed()
        .iter()
        .map(|h| h.target.clone())
        .collect();
    assert_eq!(
        hovered,
        sorted
            .iter()
            .map(|&(_, id)| HoverTarget::Agent(id))
            .collect::<Vec<_>>()
    );
    assert!(!listed.contains(&off.agent_id));
    let [a, b] = [lead.agent_id, trail.agent_id].map(|id| {
        hover
            .hovers
            .listed()
            .iter()
            .find(|h| h.target == HoverTarget::Agent(id))
            .expect("drawn")
            .at
    });
    assert!(
        a.overlaps(b),
        "premise: the two arrivals overlap: {a:?} {b:?}"
    );
}

/// Each figure on a walk to a desk hovers on exactly the box the classic
/// blits its frame in; the cutaway's at 1x is the same box, and at its densest
/// stands on the same feet, raised by the hair it draws.
#[test]
fn a_figure_hovers_on_the_box_it_is_drawn_in_in_both_looks() {
    let (layout, pack, frames, _) =
        crate::display::compose::tests::sit_down(crate::layout::Facing::North, 2);
    let theme = crate::theme::theme_by_name("normal").expect("normal theme");
    let (owned, scene) = (OwnedSimStores::new(), SceneState::uniform(16));
    let now = SystemTime::UNIX_EPOCH;
    let mut scratch = RgbBuffer::filled(layout.buf_w, layout.buf_h, Rgb { r: 0, g: 0, b: 0 });
    for frame in &frames {
        let blitted: Vec<_> = frame
            .characters
            .iter()
            .map(|c| {
                let agent = &frame.agents[c.agent_idx];
                let pose = crate::character::SpritePose::of(c, agent, theme);
                let size = paint_character_at(
                    &mut scratch,
                    pose,
                    c.top_left,
                    agent,
                    &pack,
                    &mut FrameCache::new(),
                    now,
                );
                Hover::figure(
                    Pivot::TopLeft,
                    c.top_left,
                    size,
                    HoverTarget::Agent(agent.agent_id),
                )
            })
            .collect();
        let drawn = paint_drawn(&owned, &scene, &layout, &pack, now, frame);
        assert_eq!(drawn.hovers.listed(), blitted);
        let cutaway = |scale| {
            let office = crate::display::Office {
                layout: &layout,
                pack: &pack,
                theme,
                scale,
            };
            crate::display::compose::tests::list_at(frame, office, 12)
                .hovers()
                .clone()
        };
        let scale = crate::render_scale::RenderScale::new;
        assert_eq!(
            cutaway(scale(1).expect("nonzero")).listed(),
            blitted,
            "the cutaway at 1x"
        );
        let densest = cutaway(scale(pack.max_density_variant().get()).expect("nonzero"));
        assert_eq!(densest.listed().len(), blitted.len());
        for (dense, flat) in densest.listed().iter().zip(&blitted) {
            let (d, f) = (dense.at, flat.at);
            assert_eq!(dense.target, flat.target);
            assert_eq!(
                (d.x, d.width, d.y + d.height),
                (f.x, f.width, f.y + f.height),
                "the same columns and feet"
            );
            assert!(d.y <= f.y, "{d:?} sinks below {f:?}");
        }
    }
}

/// Both looks hang each badge from the one anchor: at 1x the cutaway's badge
/// runs are the classic's, on every frame of a walk to a desk.
#[test]
fn both_looks_hang_a_badge_from_the_one_anchor() {
    let (layout, pack, frames, _) =
        crate::display::compose::tests::sit_down(crate::layout::Facing::North, 2);
    let theme = crate::theme::theme_by_name("normal").expect("normal theme");
    let (owned, now) = (OwnedSimStores::new(), SystemTime::UNIX_EPOCH);
    for frame in &frames {
        let mut scene = SceneState::uniform(16);
        for agent in &frame.agents {
            scene.agents.insert(agent.agent_id, agent.clone());
        }
        let classic: Vec<_> = paint_drawn(&owned, &scene, &layout, &pack, now, frame)
            .badges
            .iter()
            .map(crate::display::Badge::run)
            .collect();
        let office = crate::display::Office {
            layout: &layout,
            pack: &pack,
            theme,
            scale: crate::render_scale::RenderScale::ONE,
        };
        let list = crate::display::compose::tests::list_at(frame, office, 12);
        let cutaway: Vec<_> = list.badges().cloned().collect();
        assert!(!classic.is_empty(), "premise: the walker wears a badge");
        assert_eq!(cutaway, classic);
    }
}

/// A chitchat bubble hangs over its speaker's badge, and both looks draw the
/// same one.
#[test]
fn a_bubble_hangs_over_its_speakers_badge_in_both_looks() {
    use crate::display::{TextRole, TextRun};
    let (layout, pack, frames, _) =
        crate::display::compose::tests::sit_down(crate::layout::Facing::North, 2);
    let theme = crate::theme::theme_by_name("normal").expect("normal theme");
    let mut frame = frames.last().expect("a seated frame").clone();
    let speaker = frame.agents[0].agent_id;
    frame.chitchat_bubbles = vec![crate::chitchat::ChitchatBubble {
        text: "LGTM!",
        speaker,
    }];
    let mut scene = SceneState::uniform(16);
    for agent in &frame.agents {
        scene.agents.insert(agent.agent_id, agent.clone());
    }
    let now = SystemTime::UNIX_EPOCH;
    let classic = paint_drawn(&OwnedSimStores::new(), &scene, &layout, &pack, now, &frame);
    let find = |texts: &[TextRun], role| texts.iter().find(|run| run.role == role).cloned();
    let badge = classic
        .badges
        .iter()
        .find(|badge| badge.agent == speaker)
        .expect("the speaker's badge");
    let bubble = find(&classic.bubbles, TextRole::Bubble(speaker)).expect("the classic's bubble");
    assert_eq!(bubble.text(), "LGTM!");
    assert_eq!(bubble.at.x, badge.at.x, "centred over the badge");
    assert_eq!(
        badge.at.y - bubble.at.y,
        2 * crate::layout::CELL_ROWS,
        "two cell rows up, a row clear of the badge"
    );
    let office = crate::display::Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale: crate::render_scale::RenderScale::ONE,
    };
    let cutaway: Vec<_> = crate::display::compose::tests::list_at(&frame, office, 12)
        .texts()
        .cloned()
        .collect();
    assert_eq!(
        find(&cutaway, TextRole::Bubble(speaker)),
        Some(bubble),
        "the same bubble"
    );
}

/// A pet and an agent sharing a sort row paint the agent on top in both looks,
/// so a pointer over both names the agent in both.
#[test]
fn a_pet_tied_with_an_agent_sorts_under_them_in_both_looks() {
    use crate::display::{HoverTarget, PieceKind};
    use crate::layout::{Bounds, Pivot, Point};
    use pixtuoid_core::sprite::format::Piece as PackPiece;
    let (layout, pack, frames, _) =
        crate::display::compose::tests::sit_down(crate::layout::Facing::South, 2);
    let theme = crate::theme::theme_by_name("normal").expect("normal theme");
    let mut frame = frames.last().expect("a seated frame").clone();
    let office = crate::display::Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale: crate::render_scale::RenderScale::ONE,
    };
    let (body, row) = crate::display::compose::tests::list_at(&frame, office, 12)
        .pieces()
        .iter()
        .find_map(|p| match p.kind {
            PieceKind::Character { body, .. } => Some((body, p.span.depth)),
            _ => None,
        })
        .expect("the sitter");
    let anim_name = PackPiece::CatWalk;
    let h = crate::pack::densest_frame(&pack, anim_name, 0, crate::render_scale::RenderScale::ONE)
        .logical
        .1;
    let x = u16::midpoint(body.x0, body.x1);
    let pos = (0..layout.buf_h)
        .map(|y| Point { x, y })
        .find(|&pos| sort_row_at(Pivot::Center, pos, h) == row)
        .expect("a pet position standing on the sitter's row");
    let cat = crate::sim::PetPlacement {
        kind: crate::pet::PetKind::Cat,
        pos,
        sort_row: row,
        flip: false,
        anim_name,
        frame_idx: 0,
        effects: crate::effects::pet_hearts(pos, 300).collect(),
    };
    let pet = cat.target();
    frame.pet = Some(cat);
    let agent = HoverTarget::Agent(frame.agents[0].agent_id);
    let cell = Bounds {
        x: pos.x,
        y: pos.y,
        width: 1,
        height: 1,
    };
    let mut scene = SceneState::uniform(16);
    for a in &frame.agents {
        scene.agents.insert(a.agent_id, a.clone());
    }
    let classic = paint_drawn(
        &OwnedSimStores::new(),
        &scene,
        &layout,
        &pack,
        SystemTime::UNIX_EPOCH,
        &frame,
    )
    .hovers;
    let cutaway = crate::display::compose::tests::list_at(&frame, office, 12);
    for (look, hovers) in [("classic", &classic), ("cutaway", cutaway.hovers())] {
        let listed: Vec<_> = hovers.listed().iter().map(|h| &h.target).collect();
        assert!(
            listed.contains(&&pet) && listed.contains(&&agent),
            "{look}: premise: both are hoverable"
        );
        assert_eq!(
            hovers.at(cell),
            Some(&agent),
            "{look} paints the agent last"
        );
    }
    // What a creature wears sorts with it, under the person.
    let pieces = cutaway.pieces();
    let character = pieces
        .iter()
        .position(|p| matches!(p.kind, PieceKind::Character { .. }))
        .expect("the sitter's piece");
    let hearts: Vec<usize> = pieces
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p.kind, PieceKind::Effect(_)) && p.span.layer == Layer::Creature)
        .map(|(i, _)| i)
        .collect();
    assert!(!hearts.is_empty(), "premise: the cat wears hearts");
    assert!(
        hearts.iter().all(|&i| i < character),
        "the cat's hearts sort under the sitter"
    );
}

/// The classic's enqueue fns give the layers, so people stay over creatures
/// whatever order the queue was built in.
#[test]
fn the_classic_layers_creatures_under_characters_in_any_queue_order() {
    use crate::layout::Point;
    use pixtuoid_core::sprite::format::Piece as PackPiece;
    let (layout, pack, frames, _) =
        crate::display::compose::tests::sit_down(crate::layout::Facing::South, 2);
    let theme = crate::theme::theme_by_name("normal").expect("normal theme");
    let mut frame = frames.last().expect("a seated frame").clone();
    let row = frame.characters[0].sort_row;
    let pos = Point {
        x: layout.buf_w / 2,
        y: layout.buf_h / 2,
    };
    frame.pet = Some(crate::sim::PetPlacement {
        kind: crate::pet::PetKind::Cat,
        pos,
        sort_row: row,
        flip: false,
        anim_name: PackPiece::CatWalk,
        frame_idx: 0,
        effects: Vec::new(),
    });
    frame.mascots = vec![crate::sim::MascotPlacement {
        pos,
        sort_row: row,
        anim_name: PackPiece::LobsterWalk,
        frame_idx: 0,
        key: crate::creatures::openclaw_key("18789"),
        degraded: false,
        on_roster: true,
        effects: Vec::new(),
    }];
    let mut queue = Vec::new();
    enqueue_characters(&pack, theme, &frame, &mut queue);
    enqueue_gateway_mascots(&pack, &frame.mascots, &mut queue);
    enqueue_pet(&pack, frame.pet.as_ref().expect("the cat"), &mut queue);
    drawable::sort_drawables(&mut queue);
    assert_eq!(
        queue
            .iter()
            .map(|d| (d.layer, matches!(d.kind, DrawableKind::Character { .. })))
            .collect::<Vec<_>>(),
        [
            (Layer::Creature, false),
            (Layer::Creature, false),
            (Layer::Character, true)
        ]
    );
}

/// Two gateways of one source are two hovers, each naming its own instance,
/// in paint order.
#[test]
fn each_mascot_hovers_as_its_own_instance() {
    use pixtuoid_core::source::daemon::{DaemonPresenceUpdate, apply_presence};
    let (mut scene, layout, _, now0, pack) = sim_rig();
    for port in ["18789", "18790"] {
        apply_presence(
            &mut scene,
            &crate::creatures::openclaw_key(port),
            DaemonPresenceUpdate::GatewayUp { pid: Some(7) },
            now0,
        );
    }
    let now = now0 + std::time::Duration::from_secs(6);
    let mut owned = OwnedSimStores::new();
    let frame = sim_step(
        &mut owned.stores(),
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now,
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &pack,
            layout: &layout,
            coffee: &HashMap::new(),
            door_anim_max_ms: 0,
        },
    );
    let mut painted: Vec<_> = frame.mascots.iter().collect();
    painted.sort_by_key(|m| {
        let h = crate::sim::pack_frame_size(&pack, m.anim_name, m.frame_idx).h;
        sort_row_at(Pivot::Center, m.pos, h)
    });
    let expected: Vec<_> = painted
        .iter()
        .map(|m| HoverTarget::Mascot(m.key.clone()))
        .collect();
    assert_eq!(expected.len(), 2, "premise: both gateways walk in");
    let hovers = paint_drawn(&owned, &scene, &layout, &pack, now, &frame).hovers;
    let listed: Vec<_> = hovers
        .listed()
        .iter()
        .map(|h| h.target.clone())
        .filter(|t| matches!(t, HoverTarget::Mascot(_)))
        .collect();
    assert_eq!(listed, expected);
}

#[test]
fn paint_frame_is_pure_and_byte_identical() {
    use std::time::Duration;
    let (scene, layout, id, now0, pack) = sim_rig();
    let _ = id;
    let coffee = HashMap::new();

    let mut owned = OwnedSimStores::new();
    let now = now0 + Duration::from_millis(120);
    let frame = sim_step(
        &mut owned.stores(),
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now,
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &pack,
            layout: &layout,
            coffee: &coffee,
            door_anim_max_ms: 0,
        },
    );

    let light_before = owned.vacancy_dim.level();
    let walks_before = format!("{:?}", owned.route.walks);
    let history_before = format!("{:?}", owned.route.history);
    let chitchat_before = owned.chitchat.len();

    let theme = crate::theme::theme_by_name("normal").expect("normal theme");
    let black = Rgb { r: 0, g: 0, b: 0 };
    let mut cache = FrameCache::new();
    let mut base_fill = BaseFillCache::new();
    let mut buf1 = RgbBuffer::filled(layout.buf_w, layout.buf_h, black);
    let mut buf2 = RgbBuffer::filled(layout.buf_w, layout.buf_h, black);
    for buf in [&mut buf1, &mut buf2] {
        paint_frame(
            &mut PaintCtx {
                scene: &scene,
                layout: &layout,
                pack: &pack,
                timing: Motion::Full.timing(now),
                sky: crate::sky::Sky::clock(now),
                window_sky: None,
                buf,
                cache: &mut cache,
                base_fill: &mut base_fill,
                shadows: &mut crate::ground::DepthsCache::default(),
                theme,
                floor: crate::floor::FloorMeta::ground(),
                walks: &owned.route.walks,
                debug_walkable: false,
                outside: &mut crate::outside::OutsideCache::default(),
            },
            &frame,
        );
    }

    assert_eq!(
        buf1.as_slice(),
        buf2.as_slice(),
        "painting the same SimFrame twice must be byte-identical"
    );
    assert!(
        buf1.as_slice().iter().any(|p| *p != black),
        "the paint pass actually painted the office"
    );
    assert_eq!(
        owned.vacancy_dim.level(),
        light_before,
        "paint must not tick lighting"
    );
    assert_eq!(
        format!("{:?}", owned.route.walks),
        walks_before,
        "paint must not move walk state"
    );
    assert_eq!(
        format!("{:?}", owned.route.history),
        history_before,
        "paint must not record pose history"
    );
    assert_eq!(
        owned.chitchat.len(),
        chitchat_before,
        "paint must not start/expire chitchat"
    );
}

#[test]
fn corridor_runner_weaves_sparse_diamonds_without_inner_edge_rows() {
    // Taste pin: stride-10 lattice, border rows only.
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let floor = Rgb {
        r: 150,
        g: 110,
        b: 72,
    };
    let mut buf = RgbBuffer::filled(60, 24, floor);
    let rect = crate::layout::Bounds {
        x: 0,
        y: 4,
        width: 60,
        height: 12,
    };
    paint_corridor_runner(&mut buf, rect, theme);
    let base = theme.office.runner_base;
    let stripe = theme.office.runner_stripe;
    let edge = theme.office.runner_edge;
    assert_eq!(buf.get(0, 4), edge, "border row stays");
    assert_eq!(
        buf.get(2, 5),
        base,
        "inner-edge row (dx=2,dy=1) must be base"
    );
    assert_eq!(
        buf.get(2, 8),
        base,
        "old stride-6 lattice point must be base"
    );
    assert_eq!(buf.get(7, 7), stripe, "(dx+dy)=10 lands on the new lattice");
}

#[test]
fn pantry_doorway_gets_a_centered_entry_mat() {
    // Taste pin: an entry mat centered under the pantry's north doorway, one
    // clear row off the wall face.
    use crate::layout::{TEST_DEFAULT_DESKS, WALL_THICK_H};
    let l = SceneLayout::compute(192, 160, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let p = l.pantry.expect("pantry");
    let dw = l
        .doorways
        .iter()
        .find(|d| d.start.y == d.end.y && d.start.y == p.bounds.y)
        .expect("the pantry north door");
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let floor = Rgb {
        r: 150,
        g: 110,
        b: 72,
    };
    let mut buf = RgbBuffer::filled(192, 160, floor);
    furniture::paint_area_rug(&mut buf, l.pantry_entry_mat().expect("the mat"), theme);
    let cx = u16::midpoint(dw.start.x, dw.end.x);
    let mat_cy = dw.start.y + WALL_THICK_H + 3;
    assert_ne!(buf.get(cx, mat_cy), floor, "mat center row painted");
    assert_ne!(buf.get(cx - 7, mat_cy), floor, "mat spans west of center");
    assert_ne!(buf.get(cx + 7, mat_cy), floor, "mat spans east of center");
    assert_eq!(buf.get(cx - 9, mat_cy), floor, "floor beyond the west edge");
    assert_eq!(buf.get(cx + 9, mat_cy), floor, "floor beyond the east edge");
    assert_eq!(
        buf.get(cx, dw.start.y + WALL_THICK_H),
        floor,
        "one clear row between wall face and mat"
    );
}

#[test]
fn kitchen_island_sits_on_a_bar_mat() {
    // Taste pin: a thin bordered mat under the island whose south sliver peeks
    // out in front of the bar.
    use crate::layout::TEST_DEFAULT_DESKS;
    let l = SceneLayout::compute(192, 160, Some(TEST_DEFAULT_DESKS)).expect("fits");
    let isl = l
        .pantry
        .and_then(|p| p.kitchen_island)
        .expect("island at this size");
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let floor = Rgb {
        r: 150,
        g: 110,
        b: 72,
    };
    let mut buf = RgbBuffer::filled(192, 160, floor);
    furniture::paint_area_rug(&mut buf, l.island_bar_mat().expect("the mat"), theme);
    assert_ne!(
        buf.get(isl.x, isl.y + 4),
        floor,
        "mat painted under the island front"
    );
    assert_eq!(
        buf.get(isl.x + 14, isl.y + 4),
        floor,
        "floor beyond the east edge"
    );
    assert_eq!(
        buf.get(isl.x - 14, isl.y + 4),
        floor,
        "floor beyond the west edge"
    );
    let before = buf.get(isl.x, isl.y);
    furniture::paint_kitchen_island(&mut buf, isl.x, isl.y, theme);
    assert_ne!(
        buf.get(isl.x, isl.y),
        before,
        "island body must cover the mat center"
    );
}

#[test]
fn pantry_mats_stay_inside_the_pantry_bounds() {
    use crate::layout::TEST_DEFAULT_DESKS;
    // 120x160 is the narrow-pantry case where the entry mat box reaches the
    // water-cooler column (the paint-order catch).
    for (w, h) in [(192u16, 160u16), (240, 160), (160, 120), (120, 160)] {
        let Some(l) = SceneLayout::compute(w, h, Some(TEST_DEFAULT_DESKS)) else {
            continue;
        };
        let Some(p) = l.pantry else { continue };
        let floor = Rgb {
            r: 150,
            g: 110,
            b: 72,
        };
        let theme = crate::theme::theme_by_name("normal").expect("theme");
        let mut buf = RgbBuffer::filled(w, h, floor);
        for mat in [l.pantry_entry_mat(), l.island_bar_mat()]
            .into_iter()
            .flatten()
        {
            furniture::paint_area_rug(&mut buf, mat, theme);
        }
        let b = p.bounds;
        for y in 0..h {
            for x in 0..w {
                let inside = x >= b.x && x < b.x + b.width && y >= b.y && y < b.y + b.height;
                if !inside {
                    assert_eq!(
                        buf.get(x, y),
                        floor,
                        "{w}x{h}: mat pixel escaped the pantry at ({x},{y})"
                    );
                }
            }
        }
    }
}

#[test]
fn fish_tank_paints_water_fish_and_cabinet_from_the_furniture_row() {
    use crate::layout::{Furniture, furniture_def};
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let floor = Rgb {
        r: 150,
        g: 110,
        b: 72,
    };
    let mut buf = RgbBuffer::filled(60, 40, floor);
    let pos = Point { x: 30, y: 20 };
    let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_234_567);
    furniture::paint_fish_tank(&mut buf, pos, Motion::Full.beat(now), theme);
    let def = furniture_def(Furniture::FishTank);
    let (x0, y0) = (pos.x - def.visual.w / 2, pos.y - def.visual.h / 2);
    let fc = &theme.furniture;
    assert_eq!(
        buf.get(x0, y0),
        theme.office.room_wall_trim_dark,
        "lid row is the shared dark frame"
    );
    assert_eq!(
        buf.get(x0 + 7, y0 + 2),
        fc.tank_water,
        "water body fills the glass"
    );
    assert_eq!(
        buf.get(x0 + 7, y0 + 1),
        fc.tank_water_line,
        "lit surface row under the lid"
    );
    let lane =
        |dy: u16, color: Rgb| (1..def.visual.w - 1).any(|dx| buf.get(x0 + dx, y0 + dy) == color);
    assert!(lane(3, fc.tank_fish), "a fish patrols the upper lane");
    assert!(
        lane(5, fc.tank_fish_alt),
        "the alt fish patrols the lower lane"
    );
    assert!(
        (2..8).any(|dy| buf.get(x0 + 2, y0 + dy) == fc.tank_plant),
        "plant sprig rises from the gravel"
    );
    assert_eq!(
        buf.get(x0 + 3, y0 + 9),
        fc.wood_top,
        "cabinet row reuses the wood family"
    );
}

#[test]
fn meeting_chairs_paint_with_backrests_toward_the_table_ends() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let floor = Rgb {
        r: 150,
        g: 110,
        b: 72,
    };
    let mut buf = RgbBuffer::filled(40, 20, floor);
    let pos = Point { x: 20, y: 10 };
    furniture::paint_meeting_chair(&mut buf, pos, true, theme);
    let fc = &theme.furniture;
    assert_eq!(
        buf.get(pos.x - 3, pos.y),
        fc.chair_trim,
        "west backrest bar"
    );
    assert_eq!(
        buf.get(pos.x, pos.y),
        furniture::MEETING_FABRIC,
        "cushion wears the sofa fabric"
    );
    let mut buf2 = RgbBuffer::filled(40, 20, floor);
    furniture::paint_meeting_chair(&mut buf2, pos, false, theme);
    assert_eq!(
        buf2.get(pos.x + 3, pos.y),
        fc.chair_trim,
        "east backrest bar"
    );
    assert_eq!(
        buf2.get(pos.x - 3, pos.y),
        floor,
        "no bar on the table side"
    );
}

#[test]
fn a_coat_rack_fills_exactly_its_bounds() {
    let bg = Rgb { r: 1, g: 2, b: 3 };
    let mut buf = RgbBuffer::filled(32, 16, bg);
    let pos = Point { x: 10, y: 2 };
    furniture::paint_coat_rack(&mut buf, pos, &crate::theme::NORMAL);
    let painted: Vec<(u16, u16)> = (0..16)
        .flat_map(|y| (0..32).map(move |x| (x, y)))
        .filter(|&(x, y)| buf.get(x, y) != bg)
        .collect();
    let b = crate::layout::coat_rack_rect_at(pos);
    let (x0, x1) = (
        painted.iter().map(|p| p.0).min(),
        painted.iter().map(|p| p.0).max(),
    );
    let (y0, y1) = (
        painted.iter().map(|p| p.1).min(),
        painted.iter().map(|p| p.1).max(),
    );
    assert_eq!(
        (x0, y0, x1, y1),
        (
            Some(b.x),
            Some(b.y),
            Some(b.x + b.width - 1),
            Some(b.y + b.height - 1)
        )
    );
}

#[test]
fn meeting_chair_fabric_matches_the_sofa_sprite_palette() {
    let pack = crate::pack::test_office();
    let c = pack.palette().get('C').flatten().expect("couch fabric key");
    let g = pack
        .palette()
        .get('G')
        .flatten()
        .expect("cushion highlight key");
    assert_eq!(furniture::MEETING_FABRIC, c, "chair fabric == sofa 'C'");
    assert_eq!(
        furniture::MEETING_FABRIC_LIT,
        g,
        "chair highlight == sofa 'G'"
    );
}

#[test]
fn chair_sitter_bottom_row_lands_on_its_sort_row_overlapping_the_chair_body() {
    use crate::layout::{Facing, Point, SEAT_RENDER_Y_OFF, WaypointKind};
    let pack = crate::pack::test_office();
    let pos = Point { x: 40, y: 30 };
    let seat = Seat::at_waypoint(WaypointKind::MeetingChair, pos, Facing::West);
    let (anim, _) = seat.sprite_for(Piece::Seated);
    let seated_h = pack.piece(anim).first().height();
    let top = pos.y - SEAT_RENDER_Y_OFF;
    let bottom = top + seated_h - 1;
    assert_eq!(
        bottom,
        seat.sort_row(),
        "the chair sprite's bottom row must land on its seat sort row"
    );
    let chair = crate::layout::furniture_def(crate::layout::Furniture::MeetingChair).visual;
    let chair_top = pos.y - chair.h / 2;
    assert!(
        bottom > chair_top,
        "sitter bottom ({bottom}) must overlap the chair body (top {chair_top})"
    );
}

/// Paint the appliance `sprite` at `ms` past the epoch, `busy` or not.
fn appliance_at(sprite: Piece, busy: bool, ms: u64) -> RgbBuffer {
    let pack = crate::pack::test_office();
    let mut cache = FrameCache::new();
    let mut buf = RgbBuffer::filled(60, 40, Rgb { r: 1, g: 2, b: 3 });
    let d = Drawable {
        sort_row: 23,
        layer: Layer::Under,
        hover: None,
        kind: DrawableKind::Appliance {
            pos: Point { x: 30, y: 20 },
            sprite,
            busy,
        },
    };
    paint_drawable(
        &d.kind,
        &mut super::drawable::DrawableCtx {
            buf: &mut buf,
            pack: &pack,
            cache: &mut cache,
            timing: Motion::Full
                .timing(SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(ms)),
            theme: crate::theme::theme_by_name("normal").expect("theme"),
        },
    );
    buf
}

/// A busy appliance reads busy for most of its loop at every density: fewer
/// than half its busy frames may show it at rest.
#[test]
#[cfg(feature = "cutaway-assets")]
fn a_busy_loop_spends_most_of_its_frames_away_from_rest() {
    let pack = crate::pack::test_office();
    let four = test_density(4);
    for (name, art) in [
        ("vending_machine", pack.piece(Piece::VendingMachine)),
        ("printer", pack.piece(Piece::Printer)),
        (
            "vending_machine@4x",
            &pack.variants_of(Piece::VendingMachine)[&four],
        ),
        ("printer@4x", &pack.variants_of(Piece::Printer)[&four]),
    ] {
        let frames = art.frames();
        let (rest, busy) = frames.split_first().expect("a rest frame");
        let at_rest = busy
            .iter()
            .filter(|f| f.as_slice() == rest.as_slice())
            .count();
        assert!(
            at_rest * 2 < busy.len(),
            "{name}: {at_rest} of {} busy frames show it at rest",
            busy.len()
        );
    }
}

/// An idle appliance holds still; a busy one plays its loop, and a vend drops
/// one of the machine's drinks where the idle machine shows none.
#[test]
fn a_busy_appliance_animates_and_an_idle_one_holds_still() {
    let drinks = crate::theme::theme_by_name("normal")
        .expect("theme")
        .appliance
        .vending_drinks;
    for sprite in [Piece::VendingMachine, Piece::Printer] {
        let rest = appliance_at(sprite, false, 0);
        let sweep = (0..40).map(|i| i * 150);
        assert!(
            sweep
                .clone()
                .all(|ms| appliance_at(sprite, false, ms).as_slice() == rest.as_slice()),
            "{}: an idle appliance moves",
            sprite.name()
        );
        assert!(
            sweep
                .map(|ms| appliance_at(sprite, true, ms))
                .any(|b| b.as_slice() != rest.as_slice()),
            "{}: a busy appliance never leaves its rest frame",
            sprite.name()
        );
    }
    let rest = appliance_at(Piece::VendingMachine, false, 0);
    let vend = appliance_at(Piece::VendingMachine, true, 1_200);
    let dropped = (0..rest.height())
        .flat_map(|y| (0..rest.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| rest.get(x, y) != vend.get(x, y))
        .any(|(x, y)| drinks.contains(&vend.get(x, y)));
    assert!(
        dropped,
        "mid-vend, a drink lands where the idle machine shows none"
    );
}

#[test]
fn water_cooler_glugs_a_rising_bubble() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let bg = Rgb { r: 1, g: 2, b: 3 };
    let pr = crate::layout::Bounds {
        x: 4,
        y: 4,
        width: 30,
        height: 40,
    };
    let pantry = crate::layout::PantryRoom {
        bounds: pr,
        counter_size: crate::layout::COMPACT_COUNTER,
        kitchen_island: None,
    };
    let render = |ms: u64| {
        let mut buf = RgbBuffer::filled(60, 60, bg);
        furniture::paint_water_cooler(
            &mut buf,
            pantry.water_cooler_rect().expect("fits"),
            Motion::Full.beat(SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(ms)),
            theme,
        );
        buf
    };
    let bubble = theme.furniture.tank_water_line;
    let cooler = pantry.water_cooler_rect().expect("fits");
    let (wx, wy) = (cooler.x, cooler.y);
    let a = render(100); // phase 0: bubble low
    let b = render(500); // phase 1: bubble high
    assert_eq!(
        a.get(wx + 1, wy + 1),
        bubble,
        "bubble starts low in the bottle"
    );
    assert_eq!(b.get(wx + 1, wy), bubble, "bubble rises a row");
    let c = render(1_500); // rest of the cycle: no bubble
    assert_ne!(c.get(wx + 1, wy), bubble);
    assert_ne!(c.get(wx + 1, wy + 1), bubble);
}

#[test]
fn sim_reports_occupied_waypoints_and_enqueue_marks_them_busy() {
    use std::time::Duration;
    let (scene, layout, _id, now0, pack) = sim_rig();
    let coffee = HashMap::new();
    let mut owned = OwnedSimStores::new();
    let mut stores = owned.stores();
    let mut pinned = false;
    for step in 0..240u64 {
        let now = now0 + Duration::from_secs(5 * step);
        let f = sim_step(
            &mut stores,
            SimInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: crate::floor::FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                pack: &pack,
                layout: &layout,
                coffee: &coffee,
                door_anim_max_ms: 0,
            },
        );
        let at_wp: Vec<usize> = f
            .poses
            .values()
            .filter_map(|p| match p {
                Some(crate::pose::Pose::AtWaypoint { wp, .. }) => Some(*wp),
                _ => None,
            })
            .collect();
        if !at_wp.is_empty() {
            for wp in at_wp {
                assert!(
                    f.occupied_waypoints.contains(&wp),
                    "AtWaypoint({wp}) must appear in occupied_waypoints"
                );
            }
            pinned = true;
            break;
        }
    }
    assert!(
        pinned,
        "the idle agent never reached a waypoint in 20 min of sim"
    );
    let layout =
        SceneLayout::compute(192, 160, Some(crate::layout::TEST_DEFAULT_DESKS)).expect("fits");
    let printer_idx = layout
        .waypoints
        .iter()
        .position(|w| w.kind == crate::layout::WaypointKind::Printer)
        .expect("the layout places a printer");
    let frame = SimFrame {
        occupied_waypoints: [printer_idx].into(),
        ..empty_frame(&layout)
    };
    let busy_flag = queued(&layout, &frame)
        .sorted
        .iter()
        .find_map(|d| match d.kind {
            DrawableKind::Appliance {
                sprite: Piece::Printer,
                busy,
                ..
            } => Some(busy),
            _ => None,
        })
        .expect("printer drawable enqueued");
    assert!(
        busy_flag,
        "occupied printer waypoint must enqueue busy=true"
    );
}

#[test]
fn no_two_agents_ever_occupy_the_same_exclusive_waypoint() {
    use crate::layout::{TEST_DEFAULT_DESKS, furniture_def};
    use crate::pose::Pose;
    use std::time::Duration;

    let pack = crate::pack::test_office();
    let layout =
        SceneLayout::compute_with_seed(192, 160, Some(TEST_DEFAULT_DESKS), 0).expect("fits");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut scene = SceneState::uniform(64);
    for i in 0..TEST_DEFAULT_DESKS {
        let id = pixtuoid_core::AgentId::from_transcript_path(&format!("/p/seat{i}.jsonl"));
        let mut slot = make_slot(id, ActivityState::Idle);
        // Stagger the idle starts so wander cycles desync instead of moving as
        // one lockstep wave.
        let started = now0 - Duration::from_secs(5 + (i as u64 * 11) % 80);
        slot.created_at = started;
        slot.state_started_at = started;
        slot.last_event_at = started;
        slot.desk_index = GlobalDeskIndex(i);
        scene.agents.insert(id, slot);
    }

    let coffee = HashMap::new();
    let mut owned = OwnedSimStores::new();
    let mut stores = owned.stores();
    let mut seat_visits = 0usize;
    for step in 0..3_600u64 {
        let now = now0 + Duration::from_millis(250 * step);
        let frame = sim_step(
            &mut stores,
            SimInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: crate::floor::FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                pack: &pack,
                layout: &layout,
                coffee: &coffee,
                door_anim_max_ms: 0,
            },
        );
        let mut occupants: HashMap<usize, usize> = HashMap::new();
        for pose in frame.poses.values().flatten() {
            let Pose::AtWaypoint { wp, kind } = pose else {
                continue;
            };
            if !furniture_def(kind.furniture()).exclusive {
                continue;
            }
            seat_visits += 1;
            let n = occupants.entry(*wp).or_insert(0);
            *n += 1;
            assert_eq!(
                *n, 1,
                "{kind:?} waypoint {wp} double-booked at step {step} — an exclusive spot is single-occupancy"
            );
        }
    }
    assert!(seat_visits > 100, "agents barely sat down ({seat_visits})");
}

/// The cutaway grounds a figure with a shadow unless `seated`, so a sofa
/// sitter flagged standing lays a shadow slab across the sofa's front.
#[test]
fn a_placement_is_seated_exactly_when_its_figure_sits_on_furniture() {
    use crate::layout::{TEST_DEFAULT_DESKS, WaypointKind};
    use crate::pose::Pose;
    use std::time::Duration;

    let pack = crate::pack::test_office();
    let layout =
        SceneLayout::compute_with_seed(192, 160, Some(TEST_DEFAULT_DESKS), 0).expect("fits");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut scene = SceneState::uniform(64);
    for i in 0..TEST_DEFAULT_DESKS {
        let id = pixtuoid_core::AgentId::from_transcript_path(&format!("/p/sit{i}.jsonl"));
        let mut slot = make_slot(id, ActivityState::Idle);
        let started = now0 - Duration::from_secs(5 + (i as u64 * 11) % 80);
        slot.created_at = started;
        slot.state_started_at = started;
        slot.last_event_at = started;
        slot.desk_index = GlobalDeskIndex(i);
        scene.agents.insert(id, slot);
    }
    let coffee = HashMap::new();
    let mut owned = OwnedSimStores::new();
    let mut stores = owned.stores();
    let (mut on_furniture, mut on_foot) = (0usize, 0usize);
    for step in 0..3_600u64 {
        let now = now0 + Duration::from_millis(250 * step);
        let frame = sim_step(
            &mut stores,
            SimInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: crate::floor::FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                pack: &pack,
                layout: &layout,
                coffee: &coffee,
                door_anim_max_ms: 0,
            },
        );
        for c in &frame.characters {
            let id = frame.agents[c.agent_idx].agent_id;
            let sits = match frame.poses.get(&id) {
                Some(Some(Pose::SeatedIdle | Pose::SeatedTyping)) => true,
                Some(Some(Pose::AtWaypoint { kind, .. })) => matches!(
                    kind,
                    WaypointKind::Couch | WaypointKind::MeetingSofa | WaypointKind::MeetingChair
                ),
                _ => false,
            };
            if sits && c.seat_desk.is_none() {
                on_furniture += 1;
            } else if !sits {
                on_foot += 1;
            }
            assert_eq!(c.seated, sits, "{:?} at step {step}", frame.poses.get(&id));
        }
    }
    assert!(
        on_furniture > 0,
        "nobody sat on a couch, sofa or meeting chair"
    );
    assert!(on_foot > 0, "nobody stood or walked");
}

#[test]
fn a_running_agent_releases_the_seat_it_snapped_back_from() {
    use crate::layout::{TEST_DEFAULT_DESKS, furniture_def};
    use crate::pose::Pose;
    use crate::walk::WanderKind;
    use std::time::Duration;

    let pack = crate::pack::test_office();
    let layout =
        SceneLayout::compute_with_seed(192, 160, Some(TEST_DEFAULT_DESKS), 0).expect("fits");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let id = pixtuoid_core::AgentId::from_transcript_path("/p/claim-release.jsonl");
    let mut slot = make_slot(id, ActivityState::Idle);
    slot.created_at = now0;
    slot.state_started_at = now0;
    slot.last_event_at = now0;
    let mut scene = SceneState::uniform(16);
    scene.agents.insert(id, slot);

    let coffee = HashMap::new();
    let mut owned = OwnedSimStores::new();
    let mut stores = owned.stores();

    let mut sat_at = None;
    let mut now = now0;
    for _ in 0..2_000 {
        now += Duration::from_millis(250);
        let frame = sim_step(
            &mut stores,
            SimInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: crate::floor::FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                pack: &pack,
                layout: &layout,
                coffee: &coffee,
                door_anim_max_ms: 0,
            },
        );
        if let Some(Pose::AtWaypoint { wp, kind }) = frame.poses.get(&id).copied().flatten()
            && furniture_def(kind.furniture()).occupies_pos
        {
            sat_at = Some(wp);
            break;
        }
    }
    let sat_at = sat_at.expect("agent never reached a seat");
    assert!(
        matches!(
            owned.route.walks[&id].wander.target.kind,
            WanderKind::Named { wp_idx, .. } if wp_idx == sat_at
        ),
        "the seated agent should hold its seat's claim"
    );

    scene.agents.get_mut(&id).expect("slot").state = ActivityState::Running {
        item: Some(ToolUse {
            tool_use_id: None,
            detail: None,
            kind: ToolKind::Other,
        }),
    };
    scene.agents.get_mut(&id).expect("slot").state_started_at = now;
    let mut stores = owned.stores();
    now += Duration::from_millis(250);
    sim_step(
        &mut stores,
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now,
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &pack,
            layout: &layout,
            coffee: &coffee,
            door_anim_max_ms: 0,
        },
    );

    assert!(
        matches!(
            owned.route.walks[&id].wander.target.kind,
            WanderKind::Aimless
        ),
        "an agent that left the wander machine must release its seat claim"
    );
}

#[test]
fn one_meeting_sofa_still_seats_three_agents_at_once() {
    use crate::layout::{TEST_DEFAULT_DESKS, WaypointKind, furniture_def};
    use crate::pose::Pose;
    use std::time::Duration;

    let pack = crate::pack::test_office();
    let layout =
        SceneLayout::compute_with_seed(192, 160, Some(TEST_DEFAULT_DESKS), 0).expect("fits");
    let sofa: Vec<usize> = {
        let mut out: Vec<usize> = vec![];
        for (i, w) in layout.waypoints.iter().enumerate() {
            if w.kind != WaypointKind::MeetingSofa {
                continue;
            }
            match out.first() {
                None => out.push(i),
                Some(&f) => {
                    if w.pos.y == layout.waypoints[f].pos.y
                        && w.room_id == layout.waypoints[f].room_id
                    {
                        out.push(i);
                    }
                }
            }
            if out.len() == 3 {
                break;
            }
        }
        out
    };
    assert_eq!(sofa.len(), 3, "expected a 3-seat sofa, got {sofa:?}");
    assert!(
        sofa.iter()
            .all(|&i| furniture_def(layout.waypoints[i].kind.furniture()).exclusive),
        "each sofa seat must be an exclusive waypoint"
    );

    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut scene = SceneState::uniform(64);
    for i in 0..TEST_DEFAULT_DESKS {
        let id = pixtuoid_core::AgentId::from_transcript_path(&format!("/p/sofa{i}.jsonl"));
        let mut slot = make_slot(id, ActivityState::Idle);
        let started = now0 - Duration::from_secs(5 + (i as u64 * 11) % 80);
        slot.created_at = started;
        slot.state_started_at = started;
        slot.last_event_at = started;
        slot.desk_index = GlobalDeskIndex(i);
        scene.agents.insert(id, slot);
    }

    let coffee = HashMap::new();
    let mut owned = OwnedSimStores::new();
    let mut stores = owned.stores();
    let mut max_on_sofa = 0usize;
    // A BUDGET, not part of the assertion: three-on-a-sofa is reached by random
    // wander, whose route rides live desk positions.
    for step in 0..60_000u64 {
        let now = now0 + Duration::from_millis(250 * step);
        let frame = sim_step(
            &mut stores,
            SimInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    floor: crate::floor::FloorMeta::ground(),
                    pets: PetInputs::default(),
                },
                pack: &pack,
                layout: &layout,
                coffee: &coffee,
                door_anim_max_ms: 0,
            },
        );
        let n = frame
            .poses
            .values()
            .flatten()
            .filter(|p| matches!(p, Pose::AtWaypoint { wp, .. } if sofa.contains(wp)))
            .count();
        max_on_sofa = max_on_sofa.max(n);
        if max_on_sofa >= 3 {
            break;
        }
    }
    assert_eq!(
        max_on_sofa, 3,
        "one sofa must seat 3 agents at once; peaked at {max_on_sofa}"
    );
}

#[test]
fn a_meeting_chair_sitter_is_drawn_on_the_seat_not_5px_high() {
    use crate::layout::{TEST_DEFAULT_DESKS, WaypointKind, stand_point};
    use crate::pose::Pose;
    use std::time::Duration;

    let pack = crate::pack::test_office();
    let layout =
        SceneLayout::compute_with_seed(192, 160, Some(TEST_DEFAULT_DESKS), 0).expect("fits");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut scene = SceneState::uniform(64);
    for i in 0..TEST_DEFAULT_DESKS {
        let id = pixtuoid_core::AgentId::from_transcript_path(&format!("/p/mc{i}.jsonl"));
        let mut slot = make_slot(id, ActivityState::Idle);
        let started = now0 - Duration::from_secs(5 + (i as u64 * 11) % 80);
        slot.created_at = started;
        slot.state_started_at = started;
        slot.last_event_at = started;
        slot.desk_index = GlobalDeskIndex(i);
        scene.agents.insert(id, slot);
    }
    let coffee = HashMap::new();
    let mut owned = OwnedSimStores::new();
    let mut checked = false;
    for step in 0..4_000u64 {
        let now = now0 + Duration::from_millis(250 * step);
        let frame = {
            let mut stores = owned.stores();
            sim_step(
                &mut stores,
                SimInputs {
                    world: FloorInputs {
                        scene: &scene,
                        now,
                        floor: crate::floor::FloorMeta::ground(),
                        pets: PetInputs::default(),
                    },
                    pack: &pack,
                    layout: &layout,
                    coffee: &coffee,
                    door_anim_max_ms: 0,
                },
            )
        };
        let mc = frame.poses.iter().find_map(|(id, p)| match p {
            Some(Pose::AtWaypoint {
                wp,
                kind: WaypointKind::MeetingChair,
            }) => Some((*id, *wp)),
            _ => None,
        });
        let Some((id, wp)) = mc else { continue };
        let agent = scene.agents.get(&id).unwrap();
        let desk = layout
            .home_desk(agent.desk_index.single_floor_local())
            .unwrap();
        let w = &layout.waypoints[wp];
        let stand = stand_point(
            w.kind,
            w.pos,
            layout.pantry_counter_size(),
            &layout.walkable,
            desk.at,
            w.facing,
            &layout.reachable,
        );
        let drawn = frame
            .characters
            .iter()
            .find(|c| frame.agents[c.agent_idx].agent_id == id)
            .map(|c| c.top_left)
            .expect("chair sitter is placed");
        let seat = back_couch_top_left(stand, CHARACTER_SPRITE_W);
        let walk = waypoint_top_left(stand, CHARACTER_SPRITE_W);
        assert!(
            (i32::from(drawn.y) - i32::from(seat.y)).abs() <= 1,
            "meeting-chair sitter y {} must track the seat top-left {} (±breath)",
            drawn.y,
            seat.y
        );
        assert!(
            (i32::from(drawn.y) - i32::from(walk.y)).abs() >= 4,
            "meeting-chair sitter must NOT sit on the 5px-high waypoint_top_left {} (the bug)",
            walk.y
        );
        checked = true;
        break;
    }
    assert!(checked, "no meeting-chair sitter appeared in 1000s of sim");
}

#[test]
fn render_top_left_matches_the_pre_lift_kind_partition() {
    use crate::layout::{Facing, Point, WaypointKind};
    let stand = Point { x: 80, y: 60 };
    let w = CHARACTER_SPRITE_W;
    for &kind in WaypointKind::ALL {
        // Independent oracle: the exact pre-lift partition, keyed on kind alone.
        let expected = match kind {
            WaypointKind::Couch | WaypointKind::MeetingSofa | WaypointKind::MeetingChair => {
                back_couch_top_left(stand, w)
            }
            _ => waypoint_top_left(stand, w),
        };
        for facing in [Facing::North, Facing::South, Facing::East, Facing::West] {
            assert_eq!(
                Seat::at_waypoint(kind, stand, facing).render_top_left(w),
                expected,
                "{kind:?}/{facing:?}: render top-left drifted from the pre-lift partition"
            );
        }
    }
}

#[test]
fn an_upright_occupant_sorts_on_the_row_their_sprite_bottoms_out_on() {
    use crate::layout::{Facing, Point, WaypointKind};
    let stand = Point { x: 80, y: 60 };
    let w = CHARACTER_SPRITE_W;
    for &kind in WaypointKind::ALL {
        if matches!(
            kind,
            WaypointKind::Couch | WaypointKind::MeetingSofa | WaypointKind::MeetingChair
        ) {
            continue;
        }
        let seat = Seat::at_waypoint(kind, stand, Facing::South);
        assert_eq!(
            seat.render_top_left(w).y + crate::layout::WALKING_Y_OFF,
            seat.sort_row(),
            "{kind:?}: an upright occupant's feet row must BE the row they sort on"
        );
        assert_eq!(
            seat.sort_row(),
            stand.y,
            "{kind:?}: and that row is the stand cell"
        );
    }
}

// Sweeps gateway ports × wander phases: the escape is destination-hash-driven,
// so no single port/instant demonstrates it.
#[test]
fn a_roaming_creature_is_never_sliced_by_the_canvas_edge() {
    use pixtuoid_core::source::daemon::{DaemonInstanceKey, DaemonPresenceUpdate, apply_presence};
    use pixtuoid_core::state::DaemonInstanceId;
    use std::time::Duration;

    let pack = crate::pack::test_office();
    let layout = SceneLayout::compute_with_seed(192, 128, None, 0).expect("layout");
    let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let src = pixtuoid_core::source::openclaw::SOURCE_NAME;
    let pet = crate::pet::Pet::defaulted(crate::pet::PetKind::Cat);

    let mut escapes: Vec<String> = Vec::new();
    let mut walking = 0;
    for port in 18900..18924u32 {
        // The pet's roam is keyed on the FLOOR seed, the mascot's on its instance
        // id — vary both, or the pet half of the sweep rides one trajectory.
        let floor = crate::floor::FloorMeta {
            floor_seed: u64::from(port),
            ..crate::floor::FloorMeta::ground()
        };
        let mut scene = SceneState::uniform(16);
        let key = DaemonInstanceKey::new(src, DaemonInstanceId::new(port.to_string()).expect("id"));
        apply_presence(
            &mut scene,
            &key,
            DaemonPresenceUpdate::GatewayUp { pid: Some(7) },
            boot,
        );
        // One office's stores across the sweep: the creatures walk on state
        // they keep. Past the enter stagger + walk-in, then across several roams,
        // so both the walking legs and the resting cells get sampled.
        let mut owned = OwnedSimStores::new();
        for step in 0..48u64 {
            let now = boot + Duration::from_millis(6_000 + step * 1_700);
            let frame = sim_step(
                &mut owned.stores(),
                SimInputs {
                    world: FloorInputs {
                        scene: &scene,
                        now,
                        floor,
                        pets: PetInputs {
                            pet: Some(&pet),
                            petting: None,
                        },
                    },
                    pack: &pack,
                    layout: &layout,
                    coffee: &HashMap::new(),
                    door_anim_max_ms: 0,
                },
            );
            walking += frame
                .mascots
                .iter()
                .filter(|m| m.anim_name.name().contains("walk"))
                .count()
                + usize::from(
                    frame
                        .pet
                        .as_ref()
                        .is_some_and(|p| p.anim_name.name().contains("walk")),
                );
            for m in &frame.mascots {
                let Size { w, h } = crate::sim::pack_frame_size(&pack, m.anim_name, m.frame_idx);
                if m.pos.x < w / 2
                    || m.pos.x + w.div_ceil(2) > layout.buf_w
                    || m.pos.y < h / 2
                    || m.pos.y + h.div_ceil(2) > layout.buf_h
                {
                    escapes.push(format!(
                        "mascot port {port} step {step} at {:?} ({w}x{h}) escapes {}x{}",
                        m.pos, layout.buf_w, layout.buf_h
                    ));
                }
            }
            if let Some(p) = &frame.pet {
                let f = pack.piece(p.anim_name).first();
                let (w, h) = (f.width(), f.height());
                if p.pos.x < w / 2
                    || p.pos.x + w.div_ceil(2) > layout.buf_w
                    || p.pos.y < h / 2
                    || p.pos.y + h.div_ceil(2) > layout.buf_h
                {
                    escapes.push(format!(
                        "pet step {step} at {:?} ({w}x{h}) escapes {}x{}",
                        p.pos, layout.buf_w, layout.buf_h
                    ));
                }
            }
        }
    }
    assert!(
        escapes.is_empty(),
        "every roamer must render whole inside the canvas: {escapes:#?}"
    );
    assert!(
        walking > 100,
        "the sweep must catch them roaming, saw {walking}"
    );
}

#[test]
fn keep_sprite_on_canvas_bounds_differ_by_pivot() {
    use crate::layout::{Pivot, Size};
    use crate::sim::anchors::keep_sprite_on_canvas;
    let buf = Size { w: 100, h: 80 };
    let size = Size { w: 8, h: 12 };
    let at = |p, x, y| keep_sprite_on_canvas(p, Point { x, y }, size, buf);

    // Centre-anchored: `pos` is the middle, so BOTH bounds inset by half.
    assert_eq!(at(Pivot::Center, 0, 0), Point { x: 4, y: 6 });
    assert_eq!(at(Pivot::Center, 99, 79), Point { x: 96, y: 74 });
    // Top-left-anchored: `u16` already floors the near edge, only the far one binds.
    assert_eq!(at(Pivot::TopLeft, 0, 0), Point { x: 0, y: 0 });
    assert_eq!(at(Pivot::TopLeft, 99, 79), Point { x: 92, y: 68 });

    // A canvas smaller than the sprite: `Center` falls back to its lower bound
    // rather than `clamp`'s inverted-range panic; `TopLeft` just floors at 0.
    let tiny = Size { w: 4, h: 4 };
    assert_eq!(
        keep_sprite_on_canvas(Pivot::Center, Point { x: 2, y: 2 }, size, tiny),
        Point { x: 4, y: 6 }
    );
    assert_eq!(
        keep_sprite_on_canvas(Pivot::TopLeft, Point { x: 2, y: 2 }, size, tiny),
        Point { x: 0, y: 0 }
    );
}

/// The canvas-edge fixtures: per agent, the earliest second its PURE pose is
/// aimless past the east rim, soonest first.
///
/// Probing `pose::derive` rather than the routed twin keeps the search cheap —
/// only the survivors are worth an A* run — but a cold routing store answers
/// `Walking`, so a caller must still drive its agent second by second up to the
/// returned target.
fn east_rim_targets(layout: &SceneLayout, now0: SystemTime) -> Vec<(u32, u64)> {
    use crate::pose::Pose;
    use std::time::Duration;
    let w = CHARACTER_SPRITE_W;
    let mut targets: Vec<(u32, u64)> = (0..48u32)
        .filter_map(|aid| {
            let mut slot = make_slot(edge_agent_id(aid), ActivityState::Idle);
            slot.created_at = now0;
            slot.state_started_at = now0;
            slot.last_event_at = now0;
            (1..900u64)
                .find(|&secs| {
                    matches!(
                        pose::derive(&slot, now0 + Duration::from_secs(secs), layout),
                        Some(Pose::AimlessAt { dest })
                            if waypoint_top_left(dest, w).x + w > layout.buf_w
                    )
                })
                .map(|secs| (aid, secs))
        })
        .collect();
    targets.sort_by_key(|&(_, secs)| secs);
    targets
}

fn edge_agent_id(aid: u32) -> pixtuoid_core::AgentId {
    pixtuoid_core::AgentId::from_transcript_path(&format!("/p/edge-{aid}.jsonl"))
}

/// The #916 case: `pick_aimless_dest` legally stands an agent in the outermost
/// walkable column, where `blit_frame` silently dropped the columns past the
/// buffer. Driven through the real `sim_step`, so deleting the guard in
/// `resolve_characters` reds this rather than only moving pixels.
#[test]
fn a_wandering_character_is_never_sliced_by_the_canvas_edge() {
    use crate::pose::Pose;
    use std::time::Duration;

    let pack = crate::pack::test_office();
    let layout = SceneLayout::compute_with_seed(112, 100, None, 0).expect("112x100 lays out");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let coffee = HashMap::new();
    let w = CHARACTER_SPRITE_W;

    // Three agents, not every target — the earliest arrivals cost the fewest steps.
    let mut hit = 0usize;
    for &(aid, target) in east_rim_targets(&layout, now0).iter().take(3) {
        let id = edge_agent_id(aid);
        let mut slot = make_slot(id, ActivityState::Idle);
        slot.created_at = now0;
        slot.state_started_at = now0;
        slot.last_event_at = now0;
        let mut scene = SceneState::uniform(16);
        scene.agents.insert(id, slot);
        let mut owned = OwnedSimStores::new();
        let mut stores = owned.stores();

        for secs in 1..=target {
            let now = now0 + Duration::from_secs(secs);
            let f = sim_step(
                &mut stores,
                SimInputs {
                    world: FloorInputs {
                        scene: &scene,
                        now,
                        floor: crate::floor::FloorMeta::ground(),
                        pets: PetInputs::default(),
                    },
                    pack: &pack,
                    layout: &layout,
                    coffee: &coffee,
                    door_anim_max_ms: 0,
                },
            );
            if let Some(Some(Pose::AimlessAt { dest })) = f.poses.get(&id)
                && waypoint_top_left(*dest, w).x + w > layout.buf_w
            {
                hit += 1;
            }
            for c in &f.characters {
                let fw = pack.piece(c.anim_name).first().width();
                assert!(
                    c.top_left.x + fw <= layout.buf_w,
                    "agent {aid} at {secs}s renders at {:?} ({fw} px wide), running {} px \
                     past the {} px canvas",
                    c.top_left,
                    c.top_left.x + fw - layout.buf_w,
                    layout.buf_w
                );
            }
        }
    }
    assert!(
        hit > 0,
        "no routed pose ever settled past the east rim — the guard went untested"
    );
}

/// One tick, simulated then painted: the frame and the sprites paint drew.
fn sim_and_paint(
    owned: &mut OwnedSimStores,
    scene: &SceneState,
    layout: &SceneLayout,
    pack: &OfficeArt,
    now: SystemTime,
) -> (SimFrame, Vec<(Badged, crate::layout::Bounds)>) {
    let frame = sim_step(
        &mut owned.stores(),
        SimInputs {
            world: FloorInputs {
                scene,
                now,
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack,
            layout,
            coffee: &HashMap::new(),
            door_anim_max_ms: 0,
        },
    );
    let drawn = badged(&paint_drawn(owned, scene, layout, pack, now, &frame));
    (frame, drawn)
}

fn paint_drawn(
    owned: &OwnedSimStores,
    scene: &SceneState,
    layout: &SceneLayout,
    pack: &OfficeArt,
    now: SystemTime,
    frame: &SimFrame,
) -> Drawn {
    let mut buf = RgbBuffer::filled(layout.buf_w, layout.buf_h, Rgb { r: 0, g: 0, b: 0 });
    paint_frame(
        &mut PaintCtx {
            scene,
            layout,
            pack,
            timing: Motion::Full.timing(now),
            sky: crate::sky::Sky::clock(now),
            window_sky: None,
            buf: &mut buf,
            cache: &mut FrameCache::new(),
            base_fill: &mut BaseFillCache::new(),
            shadows: &mut crate::ground::DepthsCache::default(),
            theme: crate::theme::theme_by_name("normal").expect("normal theme"),
            floor: crate::floor::FloorMeta::ground(),
            walks: &owned.route.walks,
            debug_walkable: false,
            outside: &mut crate::outside::OutsideCache::default(),
        },
        frame,
    )
}

/// A drawn sprite's badge: whose, and where it hangs.
#[derive(Debug, Clone, Copy)]
struct Badged {
    agent_id: pixtuoid_core::AgentId,
    label_anchor: Point,
}

/// Who `drawn` badged, in paint order.
fn badged_ids(drawn: &Drawn) -> Vec<pixtuoid_core::AgentId> {
    drawn.badges.iter().map(|badge| badge.agent).collect()
}

/// Each badged sprite and the box it hovers on, which is the box it is drawn
/// in.
fn badged(drawn: &Drawn) -> Vec<(Badged, crate::layout::Bounds)> {
    drawn
        .badges
        .iter()
        .map(|badge| Badged {
            agent_id: badge.agent,
            label_anchor: badge.at,
        })
        .map(|f| {
            let hover = drawn
                .hovers
                .listed()
                .iter()
                .find(|h| h.target == HoverTarget::Agent(f.agent_id))
                .expect("a badged sprite hovers");
            (f, hover.at)
        })
        .collect()
}

/// Every drawn sprite's badge sits over its frame's top-centre: level with the
/// breath-free top, or higher only as far as the art of the desk it sits at.
fn assert_badges_top_their_frames(
    frame: &SimFrame,
    drawn: &[(Badged, crate::layout::Bounds)],
    pack: &OfficeArt,
) {
    // A breath lifts the drawn top by one pixel, never the badge.
    const BREATH: u16 = 1;
    assert!(!drawn.is_empty(), "premise: something is drawn");
    for (f, body) in drawn {
        let at = f.label_anchor;
        assert_eq!(
            at.x,
            body.x + body.width / 2,
            "{f:?}: off its frame's centre"
        );
        assert!(at.y <= body.y + BREATH, "{f:?}: under its frame's top");
        let c = frame
            .characters
            .iter()
            .find(|c| frame.agents[c.agent_idx].agent_id == f.agent_id)
            .expect("every drawn sprite has a placement");
        let desk_top = c
            .seat_desk
            .map(|d| desk_art_top(pack, d.at.y, crate::pack::desk_art(pack, d.facing).height()));
        match desk_top {
            Some(row) => assert!(
                at.y <= row && (at.y == row || at.y >= body.y),
                "{f:?}: a sitter's badge rises to its desk art's top {row} and no higher"
            ),
            None => assert!(at.y >= body.y, "{f:?}: floats above its frame"),
        }
    }
}

/// A requires-action agent at every desk, both facings, past the entry walk.
fn seated_at_every_desk() -> (SceneState, SceneLayout, SystemTime, OfficeArt) {
    let (mut scene, layout, _, now0, pack) = sim_rig();
    scene.agents.clear();
    for i in 0..layout.home_desks.len() {
        let mut s = make_slot(
            pixtuoid_core::AgentId::from_transcript_path(&format!("/badge/{i}.jsonl")),
            ActivityState::RequiresAction {
                reason: Some("perm".into()),
            },
        );
        s.desk_index = GlobalDeskIndex(i);
        (s.created_at, s.state_started_at, s.last_event_at) = (now0, now0, now0);
        scene.agents.insert(s.agent_id, s);
    }
    let now = now0 + std::time::Duration::from_millis(crate::pose::ENTRY_ANIMATION_MS + 5_000);
    (scene, layout, now, pack)
}

#[test]
fn every_seated_badge_tops_its_frame_and_clears_a_raised_monitor() {
    let (scene, layout, now, pack) = seated_at_every_desk();
    let (frame, drawn) = sim_and_paint(&mut OwnedSimStores::new(), &scene, &layout, &pack, now);
    assert_eq!(
        drawn.len(),
        scene.agents.len(),
        "premise: everyone is drawn"
    );
    assert_badges_top_their_frames(&frame, &drawn, &pack);
    assert!(
        drawn.iter().any(|(f, body)| f.label_anchor.y < body.y),
        "premise: some back-turned sitter's monitor rises over their head"
    );
}

/// The breath bobs the sprite, not its badge.
#[test]
fn a_breathing_sitter_s_badge_holds_still() {
    use std::time::Duration;
    let (mut scene, layout, now, pack) = seated_at_every_desk();
    let keep = *scene.agents.keys().next().expect("an agent");
    scene.agents.retain(|id, _| *id == keep);
    let mut owned = OwnedSimStores::new();
    let (mut tops, mut badges) = (
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    );
    // Long enough for a whole breath cycle.
    for step in 0..24u64 {
        let (_, drawn) = sim_and_paint(
            &mut owned,
            &scene,
            &layout,
            &pack,
            now + Duration::from_millis(250 * step),
        );
        let [(f, body)] = drawn[..] else {
            panic!("one agent, one sprite: {drawn:?}")
        };
        tops.insert(body.y);
        badges.insert((f.label_anchor.x, f.label_anchor.y));
    }
    assert_eq!(tops.len(), 2, "premise: the sprite breathes");
    assert_eq!(badges.len(), 1, "the badge bobbed with it: {badges:?}");
}

/// A typist keys on the art its seat resolves to: a back-turned desk plays
/// `typing_back`'s own loop, which a pack may time apart from `typing`'s.
#[test]
fn a_typist_keys_on_the_art_its_seat_resolves_to() {
    use crate::layout::Facing;
    use crate::pose::Pose;
    let (mut scene, layout, id, now0, _) = sim_rig();
    let pack = OfficeArt::parse(crate::pack::test_pack_declaring(
        "[animations.typing_back]\nframes   = [\"typing_back_0.sprite\", \"typing_back_1.sprite\"]\nframe_ms = 125",
        "[animations.typing_back]\nframes   = [\"typing_back_0.sprite\", \"typing_back_1.sprite\"]\nframe_ms = 250",
    ))
    .expect("the test pack parses");
    let back = layout
        .home_desks
        .iter()
        .position(|d| d.facing == Facing::North)
        .expect("a back-turned desk");
    scene.agents.get_mut(&id).expect("the agent").desk_index = GlobalDeskIndex(back);
    let agents: Vec<AgentSlot> = scene.agents.values().cloned().collect();
    let art = pack.piece(Piece::TypingBack);
    let ms = u64::from(art.frame_ms());
    let typing = pack.piece(Piece::Typing);
    assert_ne!(
        ms,
        u64::from(typing.frame_ms()),
        "premise: the two views keep their own time"
    );
    let frames = art.frames().len() as u64;
    let phase = epoch_ms(agents[0].state_started_at) / ms;
    let tick = crate::anim::FULL_TICK_MS;
    for step in 0..2 * frames * ms / tick {
        let timing = Motion::Full.timing(now0 + std::time::Duration::from_millis(step * tick));
        let poses = HashMap::from([(id, Some(Pose::SeatedTyping))]);
        let (placements, ..) = crate::sim::resolve_characters(
            &agents,
            &poses,
            &layout,
            &pack,
            &HashMap::new(),
            timing,
        );
        let [p] = <[_; 1]>::try_from(placements).expect("one agent, one placement");
        assert_eq!(p.anim_name, Piece::TypingBack);
        let want = ((timing.beat.ms() / ms + phase) % frames) as usize;
        assert_eq!(p.frame_idx, want, "at {} ms", step * tick);
    }
}

/// Breath rides `breathes` alone: at a breathing instant a walker's sprite stays
/// on its fit while a figure at rest rises off it.
#[test]
fn only_a_placement_that_breathes_takes_the_breath() {
    use crate::pose::Pose;
    let (scene, layout, id, now0, pack) = sim_rig();
    let agents: Vec<AgentSlot> = scene.agents.values().cloned().collect();
    let now = (0..u64::from(u16::MAX))
        .map(|ms| now0 + std::time::Duration::from_millis(ms))
        .find(|&t| {
            crate::sim::anchors::with_breath(Point { x: 0, y: 1 }, id, Motion::Full.beat(t)).y == 0
        })
        .expect("the breath rises within a cycle");
    let mid = Point {
        x: layout.buf_w / 2,
        y: layout.buf_h / 2,
    };
    let place = |pose| {
        let poses = HashMap::from([(id, Some(pose))]);
        let (placements, ..) = crate::sim::resolve_characters(
            &agents,
            &poses,
            &layout,
            &pack,
            &HashMap::new(),
            Motion::Full.timing(now),
        );
        let [p] = <[_; 1]>::try_from(placements).expect("one agent, one placement");
        p
    };
    let walker = place(
        crate::pose::LegPose {
            from: mid,
            to: mid,
            progress: LegProgress::ZERO,
            carrying_coffee: false,
        }
        .straight(),
    );
    let idler = place(Pose::AimlessAt { dest: mid });
    let sitter = place(Pose::SeatedTyping);
    assert!(!walker.breathes && idler.breathes && sitter.breathes);
    // Neither stands at a desk, so the badge row IS the fitted top.
    assert_eq!(
        walker.top_left.y, walker.label_anchor.y,
        "the walker breathed"
    );
    assert_eq!(
        idler.top_left.y + 1,
        idler.label_anchor.y,
        "the idler held its breath"
    );
}

/// Co-located visitors step aside, and each one's badge goes with them.
#[test]
fn co_located_visitors_badges_step_aside_with_their_sprites() {
    use crate::sim::anchors::waypoint_rank_offset_x;
    let (mut scene, layout, _, now, pack) = sim_rig();
    scene.agents.clear();
    let (wp, kind) = layout
        .waypoints
        .iter()
        .enumerate()
        .find(|(_, w)| waypoint_rank_offset_x(w.kind, 1) != 0)
        .map(|(i, w)| (i, w.kind))
        .expect("the layout has a shareable spot");
    for i in 0..3 {
        let mut s = make_slot(
            pixtuoid_core::AgentId::from_transcript_path(&format!("/queue/{i}.jsonl")),
            ActivityState::Idle,
        );
        s.desk_index = GlobalDeskIndex(i);
        scene.agents.insert(s.agent_id, s);
    }
    let mut owned = OwnedSimStores::new();
    let (mut frame, _) = sim_and_paint(&mut owned, &scene, &layout, &pack, now);
    let poses = scene
        .agents
        .keys()
        .map(|&id| (id, Some(crate::pose::Pose::AtWaypoint { wp, kind })))
        .collect();
    (frame.characters, ..) = crate::sim::resolve_characters(
        &frame.agents,
        &poses,
        &layout,
        &pack,
        &HashMap::new(),
        Motion::Full.timing(now),
    );
    let drawn = badged(&paint_drawn(&owned, &scene, &layout, &pack, now, &frame));
    assert_eq!(drawn.len(), 3, "premise: all three are drawn");
    assert_badges_top_their_frames(&frame, &drawn, &pack);
    let xs: std::collections::BTreeSet<_> = drawn.iter().map(|(f, _)| f.label_anchor.x).collect();
    assert_eq!(xs.len(), 3, "the badges stacked: {drawn:?}");
}

/// At the canvas rim the fit moves the sprite, and the badge moves with it.
#[test]
fn a_badge_follows_its_sprite_fitted_to_the_canvas_rim() {
    use crate::pose::Pose;
    use std::time::Duration;

    let pack = crate::pack::test_office();
    let layout = SceneLayout::compute_with_seed(112, 100, None, 0).expect("112x100 lays out");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let coffee = HashMap::new();
    let w = CHARACTER_SPRITE_W;

    let mut at_the_rim = 0usize;
    for &(aid, target) in east_rim_targets(&layout, now0).iter().take(3) {
        let id = edge_agent_id(aid);
        let mut slot = make_slot(id, ActivityState::Idle);
        slot.created_at = now0;
        slot.state_started_at = now0;
        slot.last_event_at = now0;
        let mut scene = SceneState::uniform(16);
        scene.agents.insert(id, slot);
        let mut owned = OwnedSimStores::new();

        for secs in 1..=target {
            let now = now0 + Duration::from_secs(secs);
            let frame = sim_step(
                &mut owned.stores(),
                SimInputs {
                    world: FloorInputs {
                        scene: &scene,
                        now,
                        floor: crate::floor::FloorMeta::ground(),
                        pets: PetInputs::default(),
                    },
                    pack: &pack,
                    layout: &layout,
                    coffee: &coffee,
                    door_anim_max_ms: 0,
                },
            );
            let Some(Some(Pose::AimlessAt { dest })) = frame.poses.get(&id) else {
                continue;
            };
            if waypoint_top_left(*dest, w).x + w <= layout.buf_w {
                continue;
            }
            let drawn = badged(&paint_drawn(&owned, &scene, &layout, &pack, now, &frame));
            assert_badges_top_their_frames(&frame, &drawn, &pack);
            at_the_rim += drawn
                .iter()
                .filter(|(_, body)| body.x + body.width == layout.buf_w)
                .count();
            break;
        }
    }
    assert!(at_the_rim > 0, "no sprite was fitted to the east rim");
}

#[test]
fn a_back_turned_seat_puts_the_occupant_past_the_desk_body() {
    use crate::layout::{Facing, Furniture};
    let desk_h = crate::layout::furniture_def(Furniture::Desk).visual.h;
    for desk in [Point { x: 40, y: 30 }, Point { x: 100, y: 60 }] {
        let far = seated_top_left(desk, CHARACTER_SPRITE_W, Facing::South);
        let near = seated_top_left(desk, CHARACTER_SPRITE_W, Facing::North);
        assert!(
            far.y < desk.y,
            "a viewer-facing occupant sits ABOVE the desk row: {far:?} vs {desk:?}"
        );
        assert!(
            near.y >= desk.y,
            "a back-turned occupant must reach the desk's own row, not hover above \
             it: {near:?} vs {desk:?}"
        );
        assert!(
            near.y < desk.y + desk_h,
            "…but still overlap the desk body rather than float below it: \
             {near:?} vs desk {desk:?} + visual h {desk_h}"
        );
    }
}

/// Asserted on the DRAWABLE list, not on pixels: a chair's rect also carries
/// the hour and the desk's lights, so contrasting a north desk's rect against a
/// south one measures more than the chair and can pass with no chair drawn.
#[test]
fn every_north_facing_desk_enqueues_a_chair_and_no_south_one_does() {
    for seed in 0..8u64 {
        let layout =
            SceneLayout::compute_with_seed(240, 160, Some(crate::layout::TEST_DEFAULT_DESKS), seed)
                .expect("240x160 lays out");
        let pack = crate::pack::test_office();
        let chair_w = super::drawable::desk_chair_frame(&pack).width();
        // Keyed on the FULL position: desks in one pod column share an x, so an
        // x-only key silently folds a wrongly-chaired south desk onto its
        // north neighbour and the assertion cannot see it.
        let seated: std::collections::BTreeSet<(u16, u16)> = queued(&layout, &empty_frame(&layout))
            .sorted
            .iter()
            .filter_map(|d| match d.kind {
                super::DrawableKind::DeskChair { pos, .. } => Some((pos.x, pos.y)),
                _ => None,
            })
            .collect();
        let want: std::collections::BTreeSet<(u16, u16)> = layout
            .home_desks
            .iter()
            .filter(|d| d.facing == crate::layout::Facing::North)
            .map(|&HomeDesk { at: d, .. }| {
                (
                    crate::sim::anchors::seated_top_left(d, chair_w, crate::layout::Facing::North)
                        .x,
                    d.y + 6,
                )
            })
            .collect();
        assert!(!want.is_empty(), "seed {seed}: fixture has no north desk");
        assert_eq!(
            seated, want,
            "seed {seed}: one chair per north desk, at the seat"
        );
    }
}

/// The painter half of the chair, on a BLANK buffer: no floor, no light,
/// no hour — so the only thing that can move these pixels is the chair itself.
#[test]
fn paint_chair_back_writes_its_mask_and_nothing_outside_it() {
    const BG: Rgb = Rgb { r: 1, g: 2, b: 3 };
    let mut buf = RgbBuffer::filled(64, 32, BG);
    let at = Point { x: 20, y: 10 };
    let pack = crate::pack::test_office();
    super::drawable::paint_chair_back(&mut buf, at, &pack);
    let painted: Vec<(u16, u16)> = (0..buf.height())
        .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| buf.get(x, y) != BG)
        .collect();
    assert!(!painted.is_empty(), "the chair must paint something");
    let (x0, x1) = (
        painted.iter().map(|p| p.0).min().unwrap(),
        painted.iter().map(|p| p.0).max().unwrap(),
    );
    let (y0, y1) = (
        painted.iter().map(|p| p.1).min().unwrap(),
        painted.iter().map(|p| p.1).max().unwrap(),
    );
    let w = super::drawable::desk_chair_frame(&pack).width();
    assert!(
        y0 == at.y && x0 >= at.x && x1 < at.x + w && y1 < at.y + 8,
        "the chair painted outside its own box: {:?}..{:?}",
        (x0, y0),
        (x1, y1)
    );
    // The back is inset on BOTH flanks so a seated occupant's shoulders stay visible;
    // widening it back to the full box is the silent regression this pins.
    assert!(
        x0 > at.x && x1 < at.x + w - 1,
        "the chair must leave both flank columns clear, spans {x0}..={x1} of {}..{}",
        at.x,
        at.x + w
    );
}

/// The crown's own pixels, on a BLANK buffer — the burn tier also tints the
/// sprite over this exact box, so no full-pass diff can isolate the crown.
#[test]
fn paint_flame_crown_draws_its_pattern() {
    const BG: Rgb = Rgb { r: 9, g: 9, b: 9 };
    let mut buf = RgbBuffer::filled(40, 40, BG);
    let top_left = Point { x: 12, y: 20 };
    const W: u16 = 8;
    super::effects::paint_effect(
        &mut buf,
        &crate::effects::flame_crown(top_left, W, Motion::Full.beat(SystemTime::UNIX_EPOCH)),
        crate::theme::theme_by_name("normal").expect("normal theme"),
    );
    let painted: Vec<(u16, u16)> = (0..buf.height())
        .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| buf.get(x, y) != BG)
        .collect();
    assert!(!painted.is_empty(), "the crown must paint");
    let cx = top_left.x + W / 2;
    for &(x, y) in &painted {
        assert!(
            (cx - 2..=cx + 1).contains(&x) && (top_left.y - 2..=top_left.y).contains(&y),
            "the crown painted outside its own box at ({x}, {y})"
        );
    }
}

/// The two sideways kinds mirror on OPPOSITE facings, and the shipped `standing`
/// sprite is not left-right symmetric (its mouth sits off-centre), so aligning
/// the two predicates turns every bartender around by a pixel — silently, since
/// no other test reads either flip.
#[test]
fn the_chair_and_the_island_mirror_on_opposite_facings() {
    use crate::layout::{Facing, Point, WaypointKind};
    let p = Point { x: 40, y: 30 };
    for (kind, flips_on) in [
        (WaypointKind::MeetingChair, Facing::West),
        (WaypointKind::Island, Facing::East),
    ] {
        for facing in [Facing::North, Facing::South, Facing::East, Facing::West] {
            let (_, flip) = Seat::at_waypoint(kind, p, facing).sprite_for(Piece::Seated);
            assert_eq!(
                flip,
                facing == flips_on,
                "{kind:?} must mirror on {flips_on:?} and nothing else, saw {facing:?} -> {flip}"
            );
        }
    }
}

#[test]
fn wash_since_washes_exactly_the_diff_set_and_matches_the_naive_reference() {
    let washes = [
        [
            (
                Rgb {
                    r: 255,
                    g: 200,
                    b: 150,
                },
                0.35f32,
            ),
            (
                Rgb {
                    r: 10,
                    g: 20,
                    b: 40,
                },
                0.15f32,
            ),
        ],
        [
            (Rgb { r: 0, g: 0, b: 0 }, 1.0f32),
            (
                Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                },
                0.001f32,
            ),
        ],
        [
            (
                Rgb {
                    r: 90,
                    g: 120,
                    b: 200,
                },
                0.0f32,
            ),
            (
                Rgb {
                    r: 200,
                    g: 90,
                    b: 30,
                },
                0.6f32,
            ),
        ],
    ];
    // Odd width forces a partial trailing chunk; 1-px and 0-px cases guard the
    // degenerate loops.
    for (wash, (w, h)) in washes
        .into_iter()
        .cycle()
        .zip([(67u16, 9u16), (192, 20), (1, 3), (0, 0)])
        .chain(washes.into_iter().map(|wa| (wa, (128, 16))))
    {
        let mut lcg = 0x2545F491u32;
        let mut next = || {
            lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
            Rgb {
                r: (lcg >> 24) as u8,
                g: (lcg >> 16) as u8,
                b: (lcg >> 8) as u8,
            }
        };
        let mut buf = RgbBuffer::filled(w, h, Rgb { r: 0, g: 0, b: 0 });
        for y in 0..h {
            for x in 0..w {
                buf.put(x, y, next());
            }
        }
        let since = buf.clone();
        if w > 0 && h > 0 {
            buf.put(0, 0, next());
            buf.put(w - 1, h - 1, next());
            let mid_y = h / 2;
            for x in 0..w.min(80) {
                buf.put(x, mid_y, next());
            }
            let lone = (w / 3, h - 1);
            buf.put(lone.0, lone.1, next());
            // A write of the identical value must stay invisible to the diff.
            let same = since.get(w / 2, 0);
            buf.put(w / 2, 0, same);
        }
        let mut expected = buf.clone();
        for y in 0..h {
            for x in 0..w {
                let painted = expected.get(x, y);
                if painted != since.get(x, y) {
                    expected.put(x, y, wash_object(painted, wash));
                }
            }
        }
        wash_since(&mut buf, &since, wash);
        for y in 0..h {
            for x in 0..w {
                assert_eq!(
                    buf.get(x, y),
                    expected.get(x, y),
                    "({x},{y}) diverged from the naive reference at {w}x{h}"
                );
            }
        }
    }
}

/// A pose is the placement's own frame, facing and resolved glow — the one
/// mapping both profiles draw through.
#[test]
fn a_pose_is_its_placements_frame_facing_and_glow() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let id = pixtuoid_core::AgentId::from_transcript_path("/pose.jsonl");
    let typing = make_slot(
        id,
        ActivityState::Running {
            item: Some(ToolUse {
                tool_use_id: None,
                detail: Some(Arc::from("Edit src/main.rs")),
                kind: ToolKind::Edit,
            }),
        },
    );
    let placement = |glow| CharacterPlacement {
        agent_idx: 0,
        sort_row: 0,
        anim_name: Piece::Typing,
        frame_idx: 3,
        top_left: Point { x: 0, y: 0 },
        label_anchor: Point { x: 0, y: 0 },
        flip_x: true,
        glow,
        effects: Vec::new(),
        breathes: true,
        seat_desk: None,
        seated: true,
    };
    let pose = crate::character::SpritePose::of(&placement(CharacterGlow::Work), &typing, theme);
    assert_eq!(
        (pose.anim_name, pose.frame_idx, pose.flip_x),
        (Piece::Typing, 3, true)
    );
    assert_eq!(pose.glow_tint, work_glow_tint(&typing, &theme.tool_glow));
    let between_items = make_slot(id, ActivityState::Running { item: None });
    let turn =
        crate::character::SpritePose::of(&placement(CharacterGlow::Work), &between_items, theme);
    assert_eq!(turn.glow_tint, Some(theme.tool_glow.default));
    let unlit = crate::character::SpritePose::of(&placement(CharacterGlow::None), &typing, theme);
    assert_eq!(unlit.glow_tint, None);
}

/// Unflipped, a character is drawn as its art faces; `flip_x` alone mirrors it.
#[test]
fn an_unflipped_character_faces_the_way_its_art_does() {
    let pack = crate::pack::test_office();
    let slot = make_slot(
        pixtuoid_core::AgentId::from_transcript_path("/face.jsonl"),
        ActivityState::Idle,
    );
    let mut cache = FrameCache::new();
    let opaque = |f: &Frame| -> Vec<bool> { f.as_slice().iter().map(Option::is_some).collect() };
    let art = pack.piece(Piece::SideSeated).first().clone();
    let drawn = crate::character::character_frame(
        crate::character::SpritePose {
            anim_name: Piece::SideSeated,
            frame_idx: 0,
            flip_x: false,
            glow_tint: None,
        },
        &slot,
        &pack,
        crate::render_scale::RenderScale::ONE,
        &mut cache,
        SystemTime::UNIX_EPOCH,
    )
    .frame
    .clone();
    assert_eq!(opaque(&drawn), opaque(&art));
    assert_ne!(
        opaque(&art),
        opaque(&art.mirror_horizontal()),
        "the side view must be asymmetric for this to see a flip"
    );
}

/// A facing flip mirrors the dressed frame; it never dresses a mirrored one. A
/// style's layers are drawn for the art as authored, so profile hair laid on a
/// flipped body would land on the face side.
#[test]
#[cfg(feature = "cutaway-assets")]
fn a_facing_flip_mirrors_the_dressed_frame() {
    let pack = crate::pack::test_office();
    let scale = crate::render_scale::RenderScale::from(pack.max_density_variant());
    let mut cache = crate::frame_cache::FrameCache::new();
    let now = SystemTime::UNIX_EPOCH;
    let mut asymmetric = 0;
    for i in 0..12 {
        let id = pixtuoid_core::AgentId::from_transcript_path(&format!("/flip/{i}.jsonl"));
        let slot = make_slot(id, ActivityState::Idle);
        let mut look = |flip| {
            crate::character::character_frame(
                crate::character::SpritePose {
                    anim_name: Piece::SideSeated,
                    frame_idx: 0,
                    flip_x: flip,
                    glow_tint: None,
                },
                &slot,
                &pack,
                scale,
                &mut cache,
                now,
            )
            .frame
            .clone()
        };
        let (east, west) = (look(false), look(true));
        let mirrored = east.mirror_horizontal();
        assert_eq!(west.as_slice(), mirrored.as_slice(), "agent {i}");
        asymmetric += usize::from(east.as_slice() != mirrored.as_slice());
    }
    assert!(asymmetric > 0, "a profile is not its own mirror");
}

/// The classic holds to
/// [`assert_the_outside_reaches_only_the_glass`](crate::outside::tests::assert_the_outside_reaches_only_the_glass),
/// its fixtures being what else paints.
#[test]
fn the_outside_reaches_only_the_glass() {
    use crate::outside::tests::{UNPAINTED, painted_alone};
    let (scene, _, _, now0, pack) = sim_rig();
    // Wide enough for meeting rooms, whose notice boards hang on the panes.
    let layout = SceneLayout::compute_with_seed(240, 144, None, 0).expect("240x144 lays out");
    let theme = crate::theme::theme_by_name("normal").expect("normal theme");
    let mut owned = OwnedSimStores::new();
    let frame = sim_step(
        &mut owned.stores(),
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now: now0,
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &pack,
            layout: &layout,
            coffee: &HashMap::new(),
            door_anim_max_ms: 0,
        },
    );
    let (w, h) = (layout.buf_w, layout.buf_h);
    let paint = |weathered: &crate::outside::tests::Weathered, window_sky| {
        let mut buf = RgbBuffer::filled(w, h, UNPAINTED);
        paint_frame(
            &mut PaintCtx {
                scene: &scene,
                layout: &layout,
                pack: &pack,
                timing: Motion::Full.timing(weathered.now),
                sky: weathered.sky,
                window_sky,
                buf: &mut buf,
                cache: &mut FrameCache::new(),
                base_fill: &mut BaseFillCache::new(),
                shadows: &mut crate::ground::DepthsCache::default(),
                theme,
                floor: crate::floor::FloorMeta::ground(),
                walks: &owned.route.walks,
                debug_walkable: false,
                outside: &mut crate::outside::OutsideCache::default(),
            },
            &frame,
        );
        buf
    };
    // Every pixel a fixture paints, each painted alone.
    let covered = |weathered: &crate::outside::tests::Weathered| {
        let timing = Motion::Full.timing(weathered.now);
        let mut scratch = RgbBuffer::filled(w, h, UNPAINTED);
        let fctx = PaintCtx {
            scene: &scene,
            layout: &layout,
            pack: &pack,
            timing,
            sky: weathered.sky,
            window_sky: None,
            buf: &mut scratch,
            cache: &mut FrameCache::new(),
            base_fill: &mut BaseFillCache::new(),
            shadows: &mut crate::ground::DepthsCache::default(),
            theme,
            floor: crate::floor::FloorMeta::ground(),
            walks: &owned.route.walks,
            debug_walkable: false,
            outside: &mut crate::outside::OutsideCache::default(),
        };
        let moment = Moment::resolve(weathered.sky, theme, 0.0, timing);
        let lights = Lights::of(
            &layout,
            &moment.look,
            &LightInputs {
                agents: &frame.agents,
                seated: &frame.seated_agents,
                floor_idx: 0,
                indoor_scale: frame.indoor_scale,
                neon: frame.neon,
                beat: timing.beat,
                bulbs: crate::lighting::DeskBulbs::of(&pack),
            },
        );
        let neon = crate::floor::neon_look(frame.neon, theme);
        let Furnishings { backdrop, sorted } = queue_fixtures(&fctx, &frame, &lights.desks, neon);
        let kinds = backdrop
            .iter()
            .map(|(_, k)| k)
            .chain(sorted.iter().map(|d| &d.kind));
        let mut cache = FrameCache::new();
        painted_alone(
            (w, h),
            kinds,
            |_| (0..w, 0..h),
            |kind, over| {
                paint_drawable(
                    kind,
                    &mut drawable::DrawableCtx {
                        buf: over,
                        pack: &pack,
                        cache: &mut cache,
                        timing,
                        theme,
                    },
                );
            },
        )
    };
    let hung_on_glass = crate::outside::tests::assert_the_outside_reaches_only_the_glass(
        "classic",
        (w, h),
        |x, y| layout.glass_at(x, y),
        |weathered| {
            [
                paint(weathered, None),
                paint(weathered, Some(weathered.bare)),
            ]
        },
        covered,
    );
    for kind in ["neon sign", "notice board"] {
        let over_glass = layout.fixtures().any(|f| {
            let named = match f.kind {
                FixtureKind::NeonSign => "neon sign",
                FixtureKind::NoticeBoard { .. } => "notice board",
                _ => return false,
            };
            named == kind
                && (f.visual.y..f.visual.y + f.visual.height).any(|y| {
                    (f.visual.x..f.visual.x + f.visual.width)
                        .any(|x| hung_on_glass.contains(&(x, y)))
                })
        });
        assert!(
            over_glass,
            "no {kind} hangs over a window, so none was compared there"
        );
    }
}

/// The neon halo lifts the window glass it falls on in the frame itself, as
/// the cutaway's glass takes the sign's glow: the frame lit by the tube differs
/// from the one with the tube off on glass the sign does not cover.
#[test]
fn the_neon_halo_lifts_the_window_glass_it_falls_on() {
    let (scene, _, _, now0, pack) = sim_rig();
    let layout = SceneLayout::compute(192, 160, Some(crate::layout::TEST_DEFAULT_DESKS))
        .expect("192x160 fits");
    let mut owned = OwnedSimStores::new();
    let frame = sim_step(
        &mut owned.stores(),
        SimInputs {
            world: FloorInputs {
                scene: &scene,
                now: now0,
                floor: crate::floor::FloorMeta::ground(),
                pets: PetInputs::default(),
            },
            pack: &pack,
            layout: &layout,
            coffee: &HashMap::new(),
            door_anim_max_ms: 0,
        },
    );
    let night = crate::localclock::at_hour(23);
    let paint = |neon| {
        let frame = SimFrame {
            neon,
            ..frame.clone()
        };
        let mut buf = RgbBuffer::filled(layout.buf_w, layout.buf_h, Rgb { r: 0, g: 0, b: 0 });
        paint_frame(
            &mut PaintCtx {
                scene: &scene,
                layout: &layout,
                pack: &pack,
                timing: Motion::Full.timing(night),
                sky: crate::sky::Sky::at_with(night, crate::sky::Weather::Clear),
                window_sky: None,
                buf: &mut buf,
                cache: &mut FrameCache::new(),
                base_fill: &mut BaseFillCache::new(),
                shadows: &mut crate::ground::DepthsCache::default(),
                theme: crate::theme::theme_by_name("normal").expect("normal theme"),
                floor: crate::floor::FloorMeta::ground(),
                walks: &owned.route.walks,
                debug_walkable: false,
                outside: &mut crate::outside::OutsideCache::default(),
            },
            &frame,
        );
        buf
    };
    let (lit, dark) = (
        paint(crate::floor::NeonLevels::FLASH),
        paint(crate::floor::NeonLevels::EMPTY),
    );
    let sign = layout
        .fixtures()
        .find(|f| f.kind == FixtureKind::NeonSign)
        .expect("the office hangs its sign")
        .visual;
    let under_sign = |x: u16, y: u16| {
        (sign.x..sign.x + sign.width).contains(&x) && (sign.y..sign.y + sign.height).contains(&y)
    };
    let lifted = (0..layout.buf_h)
        .flat_map(|y| (0..layout.buf_w).map(move |x| (x, y)))
        .filter(|&(x, y)| layout.glass_at(x, y) && !under_sign(x, y))
        .filter(|&(x, y)| lit.get(x, y) != dark.get(x, y))
        .count();
    assert!(lifted > 0, "the sign's glow reaches no window glass");
}
