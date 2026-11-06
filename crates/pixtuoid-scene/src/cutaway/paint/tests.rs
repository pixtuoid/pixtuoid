use super::*;
use crate::anim::Motion;
use crate::atmosphere::Moment;
use crate::display::compose::tests::{
    base_size, empty_frame, kind_name, list_at, lively_office, many_layouts, queued, quiet_board,
    showing, sit_down, sit_down_as, sit_down_in,
};
use crate::display::compose::{compose_at, ground_shadow, push_windows};
use crate::display::pen::test_density;
use crate::display::{Piece, Span, fingerprint};
use crate::glass_weather::GlassWeather;
use crate::layout::{FixtureKind, SceneLayout};
use crate::theme::Theme;

/// `theme`'s rug tones, as a covering carries them.
fn rug_tones(theme: &Theme) -> [crate::sprite::Rgb; 3] {
    let f = &theme.furniture;
    [f.rug_trim, f.rug_accent, f.rug_field]
}
use crate::pack::{DeskProp, NORTH_SOFA_SEAT_ROWS, test_office};

/// Relighting recolors the screen KEYS and nothing else — not even a pixel
/// of another key the same colour as the glass — so the glow is exactly the
/// screen the art drew, at whatever density.
#[test]
fn a_lit_screen_relights_only_the_screen_keys() {
    let glass = crate::pack::SCREEN_GLASS_KEY;
    let text = crate::pack::SCREEN_TEXT_KEY;
    let pack = crate::pack::load_filled_pack(
        &format!(
            "[palette]\n\
                 \"{glass}\"=\"#1c2a36\"\n\"{text}\"=\"#34424e\"\n\
                 \"D\"=\"#8b5a2b\"\n\"x\"=\"#1c2a36\"\n\
                 \".\"=\"transparent\"\n\
                 [animations.desk]\nframes=[\"desk.sprite\"]\nframe_ms=100\n"
        ),
        &[("desk.sprite", &format!("@frame 0\n{glass} D x . {text}"))],
    )
    .expect("pack builds");
    let anim = pack.piece(PackPiece::Desk);
    let glow = crate::sprite::Rgb {
        r: 40,
        g: 180,
        b: 220,
    };
    let lit = relight_screen(anim.recolorable_at(0), glow);
    let original = anim.first().as_slice();
    assert_eq!(
        lit.as_slice(),
        &[
            Some(glow.ramp(SCREEN_GLASS_LEVEL)),
            original[1],
            original[2],
            None,
            Some(glow.ramp(SCREEN_TEXT_LEVEL)),
        ]
    );
    assert_eq!(
        original[2], original[0],
        "the fixture's `x` shares the glass colour"
    );
}

/// Each prop stands with its foot on the cell its desk art marks for it, at
/// every density and either facing, the cup on the cell its steam rises
/// from, and a sheet one sheet's fall short of landing hangs that many rows
/// over the tower.
#[test]
fn the_desk_props_stand_on_their_marks() {
    use crate::layout::Facing;
    for facing in [Facing::North, Facing::South] {
        let theme = crate::theme::theme_by_name("normal").expect("theme");
        let (layout, pack, frames, _) = sit_down(facing, 2);
        let mut frame = frames.last().expect("a seated frame").clone();
        for d in &mut frame.desks {
            d.cup = Some(crate::sim::Cup::Steaming);
            d.token_tier = crate::token_meter::MAX_TIER;
            d.sheet_fall = Some(1);
        }
        for s in [1, pack.max_density_variant().get()] {
            let scale = RenderScale::new(s).expect("nonzero");
            let office = Office {
                layout: &layout,
                pack: &pack,
                theme,
                scale,
            };
            let list = list_at(&frame, office, 12);
            let pieces = list.pieces();
            let mut stood = 0;
            for (i, p) in pieces.iter().enumerate() {
                let PieceKind::Desk { at, desk, .. } = p.kind else {
                    continue;
                };
                let art = desk.piece().name();
                let (desk_art, _, blit_at) = pack.desk(desk).at(scale);
                let k = blit_at.get();
                let mark = |prop: DeskProp| {
                    let (mx, my) = desk_art.props[prop];
                    (
                        scale.to_buffer(p.span.x0) + mx * k,
                        scale.to_buffer(p.span.y0) + my * k,
                    )
                };
                // This desk's props follow it in the list: cup, tower, sheet.
                let props: Vec<StoodProp> = pieces[i + 1..]
                    .iter()
                    .map_while(|q| match q.kind {
                        PieceKind::DeskProp(prop) => Some(prop),
                        _ => None,
                    })
                    .collect();
                let [cup, tower, sheet] = props[..] else {
                    panic!("desk at {at:?} stood {props:?}");
                };
                // A prop stands with its west edge on its mark's cell, or, on
                // a desk that mirrors its props, turned, its east edge.
                let mirrored = desk.mirrors_props();
                let turn = if mirrored {
                    Flip::Horizontal
                } else {
                    Flip::None
                };
                let foot = |prop: StoodProp| {
                    let f = crate::pack::densest_frame(&pack, prop.sprite, prop.frame, scale);
                    let b = f.blit_at.get();
                    let east = prop.at.0 + f.frame.width() * b;
                    let edge = if mirrored { east } else { prop.at.0 };
                    (edge, prop.at.1 + (f.frame.height() - 1) * b, b)
                };
                let edge_of = |mx: u16| if mirrored { mx + k } else { mx };
                for (prop, which) in [(cup, DeskProp::Cup), (tower, DeskProp::Tower)] {
                    let name = which.mark();
                    let (x, y, b) = foot(prop);
                    let (mx, my) = mark(which);
                    assert_eq!(
                        (x, y),
                        (edge_of(mx), my + k - b),
                        "at scale {s}, the {art} {name} is off its mark"
                    );
                    assert_eq!(prop.flip, turn, "at scale {s}, the {art} {name} turned");
                }
                // The steam rises from `desk_cup_at`, in either look.
                let facing = if desk == Desk::North {
                    Facing::North
                } else {
                    Facing::South
                };
                let steam = crate::sim::desk_cup_at(&pack, at, facing).expect("a cup cell");
                assert_eq!(
                    (cup.at.0 / s, cup.at.1 / s),
                    (steam.x, steam.y),
                    "at scale {s}, the {art} cup is off the cell its steam rises from"
                );
                let rest = crate::token_meter::SHEET_FALL_PX - 1;
                assert_eq!(
                    (foot(sheet).0, sheet.at.1),
                    (edge_of(mark(DeskProp::Tower).0), tower.at.1 - rest * s),
                    "at scale {s}, the {art} sheet hangs off its fall"
                );
                stood += 1;
            }
            assert!(stood > 0, "no desk stood its props");
        }
    }
}

/// No prop covers the monitor in the cutaway, in either facing, at every
/// density and tier, cup or none: its cells paint as the bare desk's.
#[test]
fn no_prop_covers_the_monitor_in_the_cutaway() {
    use crate::layout::Facing;
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    for facing in [Facing::North, Facing::South] {
        let (layout, pack, frames, _) = sit_down(facing, 2);
        let seated = frames.last().expect("a seated frame");
        for s in [1, pack.max_density_variant().get()] {
            let scale = RenderScale::new(s).expect("nonzero");
            let office = Office {
                layout: &layout,
                pack: &pack,
                theme,
                scale,
            };
            let render = |cup, tier| {
                let mut frame = seated.clone();
                for d in &mut frame.desks {
                    (d.cup, d.token_tier, d.sheet_fall) = (cup, tier, None);
                }
                let list = list_at(&frame, office, 12);
                let mut buf = RgbBuffer::filled(
                    scale.to_buffer(layout.buf_w),
                    scale.to_buffer(layout.buf_h),
                    theme.surface.bg_fallback,
                );
                paint(&list, &mut CutawayCache::default(), &mut buf);
                let monitors: Vec<(u16, u16)> = list
                    .pieces()
                    .iter()
                    .filter_map(|p| match p.kind {
                        PieceKind::Desk { desk, .. } => Some((desk.piece(), p.span)),
                        _ => None,
                    })
                    .flat_map(|(art, span)| {
                        let desk = crate::pack::densest_frame(&pack, art, 0, scale);
                        let (w, b) = (usize::from(desk.frame.width()), desk.blit_at.get());
                        let (x0, y0) = (scale.to_buffer(span.x0), scale.to_buffer(span.y0));
                        crate::pack::drawn_in(&desk, &crate::pack::MONITOR_KEYS)
                            .into_iter()
                            .enumerate()
                            .filter(|&(_, m)| m)
                            .map(move |(i, _)| (x0 + (i % w) as u16 * b, y0 + (i / w) as u16 * b))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                (buf, monitors)
            };
            let (bare, monitors) = render(None, 0);
            assert!(!monitors.is_empty(), "the desks draw monitors");
            for tier in 0..=crate::token_meter::MAX_TIER {
                for cup in [None, Some(crate::sim::Cup::Cold)] {
                    let (buf, _) = render(cup, tier);
                    for &(x, y) in &monitors {
                        assert_eq!(
                            buf.get(x, y),
                            bare.get(x, y),
                            "{facing:?} at scale {s}, tier {tier}, cup {cup:?}: a prop covers the monitor at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }
}

/// Pins the screen keys ([`SCREEN_GLASS_KEY`](crate::pack::SCREEN_GLASS_KEY),
/// [`SCREEN_TEXT_KEY`](crate::pack::SCREEN_TEXT_KEY)) to the bundled
/// art: the raised desk a back-turned sitter works at must light its glass
/// and its text at every density the pack draws it, or a key names nothing
/// and that screen never lights.
#[test]
fn the_bundled_back_turned_desk_draws_its_screen_in_the_screen_keys() {
    let pack = test_office();
    let art = PackPiece::DeskNorth;
    let sentinel = crate::sprite::Rgb {
        r: 255,
        g: 0,
        b: 255,
    };
    let glass_and_text = [SCREEN_GLASS_LEVEL, SCREEN_TEXT_LEVEL];
    let mut drawn = 0;
    for (name, anim) in std::iter::once((art.name().to_owned(), pack.piece(art))).chain(
        pack.inner()
            .variants_of(art)
            .iter()
            .map(|(d, s)| (format!("{}@{d}x", art.name()), s)),
    ) {
        let lit = relight_screen(anim.recolorable_at(0), sentinel);
        for level in glass_and_text {
            assert!(
                lit.as_slice().contains(&Some(sentinel.ramp(level))),
                "{name} has no pixel lit at level {level}"
            );
        }
        drawn += 1;
    }
    assert!(
        drawn > 1,
        "the bundled pack ships a density variant of {}",
        art.name()
    );
}

/// A base desk's derived face is its bottom row's material, and the bundled
/// legs tie their shadow against their dark inner side: the tie breaks
/// west-first, onto the shadow wood. An art edit that flips it recolours
/// every base-art face in the cutaway and nothing else notices.
#[test]
fn a_bundled_base_desks_face_is_its_shadow_wood() {
    let pack = test_office();
    let shadow = pack
        .palette()
        .get('d')
        .flatten()
        .expect("`d` is the desk's opaque shadow wood");
    for piece in [PackPiece::Desk, PackPiece::DeskNorth] {
        let f = pack.piece(piece).first();
        assert_eq!(
            dominant_opaque_row(f, f.height() - 1),
            Some(shadow),
            "{}'s derived face",
            piece.name()
        );
    }
}

/// The seat side is the LAYOUT's to decide, and both profiles read it.
#[test]
fn both_profiles_seat_an_occupant_on_the_side_the_layout_chose() {
    use crate::layout::{CHARACTER_SPRITE_W, Facing};
    let desk = crate::layout::Point { x: 40, y: 30 };
    let near = crate::sim::seated_top_left(
        desk,
        CHARACTER_SPRITE_W,
        Facing::North,
        crate::layout::BARE_OFFICE,
    );
    let far = crate::sim::seated_top_left(
        desk,
        CHARACTER_SPRITE_W,
        Facing::South,
        crate::layout::BARE_OFFICE,
    );
    assert_eq!(
        near.y, desk.y,
        "a back-turned occupant's shared top-left lands on desk.y"
    );
    assert!(
        far.y < near.y,
        "a viewer-facing occupant sits BEHIND the desk, a back-turned one in \
             front: far {far:?}, near {near:?}"
    );
    assert_eq!(far.x, near.x, "the seat side never moves the centring");
}

/// A run's spans step on one grid, wide characters included, on a plate
/// or off one: the span after `text` starts
/// [`advance`](crate::display::text::advance)`(text)` on.
#[test]
fn a_runs_spans_step_on_one_grid_on_a_plate_or_off_one() {
    use crate::display::text::advance;
    use crate::display::{Align, TextRole, TextSpan};
    use crate::sprite::Rgb;
    let pen =
        Pen::new(RenderScale::new(4).expect("nonzero"), test_density(4)).expect("4 divides 4");
    let (first, second) = ("I日b", "I");
    let (a, b) = (Rgb { r: 255, g: 0, b: 0 }, Rgb { r: 0, g: 255, b: 0 });
    for (align, plate, role) in [
        (Align::Left, None, TextRole::Board),
        (
            Align::Centre,
            Some(Rgb { r: 1, g: 1, b: 1 }),
            TextRole::Indicator,
        ),
    ] {
        let run = TextRun {
            at: Point { x: 8, y: 0 },
            align,
            within: crate::layout::BARE_OFFICE,
            spans: vec![TextSpan::new(first, a), TextSpan::new(second, b)],
            plate,
            ring: None,
            role,
        };
        let mut buf = RgbBuffer::filled(96, 16, Rgb { r: 0, g: 0, b: 0 });
        paint_run(&run, (&test_office(), pen), &mut buf);
        // An `I`'s top bar spans its whole cell, so its first ink is its span's start.
        let left =
            |ink| (0..buf.width()).find(|&x| (0..buf.height()).any(|y| buf.get(x, y) == ink));
        let (la, lb) = (
            left(a).expect("the first span"),
            left(b).expect("the second span"),
        );
        assert_eq!(lb - la, advance(first).0, "{align:?}");
    }
}

/// The layout leaves walkable rows between the wall band and `top_margin`.
#[test]
fn the_wall_band_stops_where_the_layout_says_the_ground_begins() {
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    let band_h = layout.wall_band_h();
    assert!(band_h > 0, "a laid-out office has a wall band");
    assert!(
        band_h < layout.top_margin,
        "the band must end ABOVE top_margin, leaving walkable rows: \
             band {band_h}, top_margin {}",
        layout.top_margin
    );
}

/// The ground is tiled on its own grid, from the wall's foot: a seam sits
/// under the tile beside it, on the lit ground and the dark alike.
#[test]
fn the_ground_is_tiled_from_the_wall_foot() {
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 110), None, 0).expect("lays out");
    assert_ne!(
        layout.wall_band_h() % GROUND_TILE,
        0,
        "a wall foot off the buffer's own grid, or one anchored at row 0 passes too"
    );
    let s = 8;
    let (pen, buf) = carpet(&layout, s, 4);
    let luma = |x: u16, y: u16| buf.get(x, y).lightness();
    let k = s / 4;
    let tile = pen.art(GROUND_TILE).0 * k;
    let ground_top = layout.wall_band_h() * s;
    let seam = tile * 3;
    let mid = seam + tile / 2;
    assert!(
        luma(mid, ground_top) < luma(mid, ground_top + k),
        "the first seam runs along the wall's foot, one art pixel deep"
    );
    for y in [ground_top + tile + tile / 2, buf.height() - tile / 2] {
        assert!(
            luma(seam, y) < luma(seam + tile / 2, y),
            "row {y}: the seam at column {seam} must sit under its tile"
        );
    }
}

/// At 1x a seam every tile would be a quarter of the ground, so there are
/// none: the lit zone is one flat tone.
#[test]
fn a_1x_ground_has_no_seams() {
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    let (_, buf) = carpet(&layout, 1, 1);
    let y = layout.wall_band_h() + 1;
    assert!(
        (0..buf.width()).all(|x| buf.get(x, y) == crate::theme::NORMAL.surface.carpet_light),
        "row {y} of the lit zone is unbroken"
    );
}

/// The carpet alone at scale `s`, drawn from art at density `d`, in the
/// normal theme.
fn carpet(layout: &SceneLayout, s: u16, d: u16) -> (Pen, RgbBuffer) {
    let scale = RenderScale::new(s).expect("nonzero");
    let pen = Pen::new(scale, test_density(d)).expect("d divides s");
    let mut buf = RgbBuffer::filled(
        scale.to_buffer(layout.buf_w),
        scale.to_buffer(layout.buf_h),
        crate::sprite::Rgb { r: 0, g: 0, b: 0 },
    );
    paint_carpet(
        &Backdrop::of(layout, &crate::theme::NORMAL),
        Carpet::plain(&crate::theme::NORMAL),
        pen,
        &mut buf,
    );
    (pen, buf)
}

/// Every rug in the office lies on the ground, the lounge's among them.
#[test]
fn every_rug_lies_on_the_ground() {
    let pack = test_office();
    let theme = &crate::theme::NORMAL;
    let scale = RenderScale::from(pack.max_density_variant());
    let f = &theme.furniture;
    let (mut trios, mut lounges) = (0, 0);
    for (w, h) in [(160, 96), (200, 120), (240, 144), (480, 270)] {
        for seed in 0..3 {
            let layout = SceneLayout::compute(crate::layout::Viewport::full(w, h), None, seed)
                .expect("lays out");
            trios += layout
                .meeting_rooms
                .iter()
                .filter(|r| r.trio.is_some())
                .count();
            lounges += usize::from(layout.lounge.is_some());
            let mut buf = RgbBuffer::filled(
                scale.to_buffer(layout.buf_w),
                scale.to_buffer(layout.buf_h),
                crate::sprite::Rgb { r: 0, g: 0, b: 0 },
            );
            paint_backdrop(
                &Backdrop::of(&layout, theme),
                Carpet::plain(theme),
                scale,
                Pen::for_pack(scale, &pack),
                &mut buf,
            );
            let rugs = layout.fixtures().filter_map(|f| {
                matches!(
                    f.kind,
                    FixtureKind::MeetingRug { .. } | FixtureKind::LoungeRug
                )
                .then_some(f.visual)
            });
            for rug in rugs {
                let c = buf.get(
                    scale.to_buffer(rug.x + rug.width / 2),
                    scale.to_buffer(rug.y + rug.height / 2),
                );
                assert!(
                    c == f.rug_field || c == f.rug_field.ramp(RUG_MOTIF_LEVEL),
                    "{w}x{h} seed {seed}: the rug at {rug:?} is woven, not floor: {c:?}"
                );
            }
        }
    }
    assert!(
        trios > 0 && lounges > 0,
        "the sweep lays meeting rooms and a lounge"
    );
}

/// At 1x the cutaway's rug is the classic painter's, cell for cell, the
/// lounge rug's short side included.
#[test]
fn a_1x_rug_is_the_classic_rug() {
    let theme = &crate::theme::NORMAL;
    let pen =
        Pen::new(RenderScale::new(1).expect("nonzero"), test_density(1)).expect("1 divides 1");
    for (width, height) in [(18, 24), (22, 7), (3, 3)] {
        let rug = crate::layout::Bounds {
            x: 2,
            y: 2,
            width,
            height,
        };
        let blank = || RgbBuffer::filled(32, 32, crate::sprite::Rgb { r: 0, g: 0, b: 0 });
        let (mut cutaway, mut classic) = (blank(), blank());
        paint_rug(rug, rug_tones(theme), pen, &mut cutaway);
        crate::pixel_painter::paint_area_rug(&mut classic, rug, theme);
        assert!(
            cutaway.as_slice() == classic.as_slice(),
            "the {width}x{height} rug"
        );
    }
}

/// A 12x12 floor, its west half one tone and its east half another, under
/// `shadows` at noon, drawn at scale `s` from art at density `d`.
fn shadowed(s: u16, d: u16, shadows: &[crate::ground::Contact]) -> RgbBuffer {
    let scale = RenderScale::new(s).expect("nonzero");
    let pen = Pen::new(scale, test_density(d)).expect("d divides s");
    let mut buf = RgbBuffer::filled(scale.to_buffer(12), scale.to_buffer(12), WEST);
    for y in 0..buf.height() {
        for x in buf.width() / 2..buf.width() {
            buf.put(x, y, EAST);
        }
    }
    paint_ground_shadows(
        shadows.iter().copied(),
        crate::ground::shadow_strength(NOON_DARKNESS),
        pen,
        &mut buf,
    );
    buf
}

const WEST: crate::sprite::Rgb = crate::sprite::Rgb {
    r: 150,
    g: 110,
    b: 72,
};
const EAST: crate::sprite::Rgb = crate::sprite::Rgb {
    r: 70,
    g: 90,
    b: 140,
};
/// A shadow centred on the seam at `(6, 6)`.
fn seam_shadow() -> crate::ground::Contact {
    crate::ground::Contact::under(2, 8, 6)
}

/// A shadow short of one whole stop still darkens the ground: the falloff
/// rounds to its nearest stop, where flooring would drop it.
#[test]
fn a_shadow_short_of_one_stop_still_darkens_the_ground() {
    let pen = Pen::new(RenderScale::ONE, test_density(1)).expect("d divides s");
    let mut buf = RgbBuffer::filled(12, 12, WEST);
    paint_ground_shadows(
        std::iter::once(seam_shadow()),
        0.6 / SHADOW_STOPS_PER_STRENGTH,
        pen,
        &mut buf,
    );
    assert!(buf.as_slice().iter().any(|&p| p != WEST));
}

/// A shadow is the ground it falls on, darker toward its centre: whole ramp
/// stops of that floor's own colour, never a colour of its own.
#[test]
fn a_shadow_steps_the_ground_it_falls_on_darker_toward_its_centre() {
    let buf = shadowed(8, 4, &[seam_shadow()]);
    let deepest = deepest_shadow_stop();
    let (cx, cy) = (6 * 8, 6 * 8);
    assert_eq!(buf.get(cx - 2, cy), WEST.ramp(-deepest), "west of the seam");
    assert_eq!(
        buf.get(cx + 2, cy),
        EAST.ramp(-deepest),
        "east of it, its own tone"
    );
    assert_eq!(buf.get(0, 0), WEST, "nothing past the rim");
    let stepped = |c, ground: crate::sprite::Rgb| (0..=deepest).any(|k| c == ground.ramp(-k));
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            let ground = if x < buf.width() / 2 { WEST } else { EAST };
            assert!(
                stepped(buf.get(x, y), ground),
                "({x}, {y}) is its ground, stepped"
            );
        }
    }
}

/// Two shadows over one spot are as deep as the deeper, not their sum, in
/// either order.
#[test]
fn overlapping_shadows_take_the_deeper_not_the_sum() {
    let (narrow, wide) = (
        crate::ground::Contact::under(4, 3, 6),
        crate::ground::Contact::under(1, 10, 7),
    );
    let (a, b) = (shadowed(8, 4, &[narrow]), shadowed(8, 4, &[wide]));
    let (ab, ba) = (
        shadowed(8, 4, &[narrow, wide]),
        shadowed(8, 4, &[wide, narrow]),
    );
    assert!(ab.as_slice() == ba.as_slice(), "order-free");
    let lum = crate::sprite::Rgb::lightness;
    let mut mixed = false;
    for ((&both, &a), &b) in ab.as_slice().iter().zip(a.as_slice()).zip(b.as_slice()) {
        let deeper = if lum(a) <= lum(b) { a } else { b };
        assert_eq!(both, deeper, "the deeper of the two, not their sum");
        mixed |= a != b && both == a;
    }
    assert!(mixed, "each shadow wins somewhere");
}

/// Every step of a shadow covers whole art pixels.
#[test]
fn a_shadow_lands_on_the_art_grid() {
    let buf = shadowed(8, 4, &[seam_shadow()]);
    let k = 8 / 4;
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            assert_eq!(buf.get(x, y), buf.get(x - x % k, y - y % k), "({x}, {y})");
        }
    }
}

/// Every piece's shadow falls inside its [`Piece::reach`], so a repaint of
/// the pieces whose reach meets a damaged rect leaves no stale shadow.
#[test]
fn a_shadow_falls_inside_its_pieces_reach() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::South, 2);
    let scale = RenderScale::from(pack.max_density_variant());
    let ground = crate::sprite::Rgb {
        r: 150,
        g: 110,
        b: 72,
    };
    let list = compose_at(
        frames.last().expect("a frame"),
        Office {
            layout: &layout,
            pack: &pack,
            theme,
            scale,
        },
        &Moment::resolve(
            crate::sky::Sky::clock(std::time::UNIX_EPOCH),
            theme,
            0.0,
            Motion::Full.timing(std::time::UNIX_EPOCH),
        ),
        crate::floor::FloorMeta::ground(),
        quiet_board(),
        (
            &mut crate::display::compose::LightCache::default(),
            &mut crate::outside::OutsideCache::default(),
        ),
    );
    let mut cast = 0;
    for piece in list.pieces() {
        let Some(shadow) = piece.shadow else { continue };
        cast += 1;
        let mut buf = RgbBuffer::filled(
            scale.to_buffer(layout.buf_w),
            scale.to_buffer(layout.buf_h),
            ground,
        );
        paint_ground_shadows(
            std::iter::once(shadow),
            crate::ground::shadow_strength(NOON_DARKNESS),
            Pen::for_pack(scale, &pack),
            &mut buf,
        );
        let reach = piece.reach();
        for (i, &c) in buf.as_slice().iter().enumerate() {
            if c != ground {
                let w = usize::from(buf.width());
                let (x, y) = (scale.logical((i % w) as u16), scale.logical((i / w) as u16));
                assert!(
                    (reach.x0..=reach.x1).contains(&x) && (reach.y0..=reach.y1).contains(&y),
                    "{:?}'s shadow reaches ({x}, {y}) past {reach:?}",
                    kind_name(&piece.kind)
                );
            }
        }
    }
    assert!(cast > 0, "the office casts shadows");
}

/// The windows stand where the layout tiles them, as the classic painter's
/// do, and show [`Outside::through`](crate::outside::Outside::through) art
/// pixel for art pixel, the one city over the sky, their joinery in the
/// theme's frame.
#[test]
fn the_windows_look_out_on_the_one_city_where_the_layout_tiles_them() {
    let pack = test_office();
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    let scale = RenderScale::from(pack.max_density_variant());
    let pen = Pen::for_pack(scale, &pack);
    let d = pen.density();
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale,
    };
    let rows = crate::layout::window_rows(layout.wall_band_h());
    let k = scale.get() / d.get();
    // Noon, the sun at dusk, a full moon up, a new moon down.
    for (day, hour) in [(1, 12), (2, 18), (2, 22), (17, 0)] {
        let now = crate::localclock::on_day(day, hour);
        // Clear, so no weather lies over the city and the sky.
        let moment = moment_at(crate::sky::Weather::Clear, now);
        let mut buf = RgbBuffer::filled(
            scale.to_buffer(layout.buf_w),
            scale.to_buffer(layout.buf_h),
            theme.surface.bg_fallback,
        );
        paint_backdrop(
            &Backdrop::of(&layout, theme),
            Carpet::plain(theme),
            scale,
            pen,
            &mut buf,
        );
        let mut order = Vec::new();
        push_windows(
            office,
            &moment,
            &GlassWeather::of(&moment),
            &mut order,
            &mut crate::outside::OutsideCache::default(),
        );
        let mut again = Vec::new();
        push_windows(
            office,
            &moment,
            &GlassWeather::of(&moment),
            &mut again,
            &mut crate::outside::OutsideCache::default(),
        );
        let prints = |o: &[(Span, PieceKind)]| -> Vec<u64> {
            o.iter().map(|(_, kind)| fingerprint(kind)).collect()
        };
        assert_eq!(prints(&order), prints(&again), "one moment, one view");
        assert_eq!(
            order.len(),
            layout.window_bays().count(),
            "a piece a window"
        );
        for (span, kind) in &order {
            let PieceKind::Window { view, frame } = kind else {
                panic!("only windows: {kind:?}");
            };
            assert_eq!(span.depth, 0, "a window sorts at the very back");
            paint_window(view, *frame, pen, &mut buf);
        }
        let density = pen.density();
        let wall = crate::outside::Wall::of(&layout);
        let sky = crate::celestial::SkyView::of(&moment.outlook(theme), &wall, theme);
        let run = crate::layout::window_run(wall.span.clone());
        let outside = crate::outside::Outside::of(
            &crate::outside::OutsideKey::of(&moment, wall, density, GlassWeather::of(&moment)),
            &pack,
            theme,
            &mut crate::clouds::CloudCache::default(),
        );
        let city = crate::skyline::CityStrip::draw(
            &pack,
            (
                run.end - run.start,
                crate::layout::glass_rows(rows.end - rows.start),
            ),
            &moment.outlook(theme),
            theme,
            density,
        );
        let at = |(ax, ay): (u16, u16)| buf.get(ax * k, ay * k);
        let (mut glass, mut buildings) = (0, 0);
        for (bay, view) in outside.views() {
            let here = format!("{day}/{hour}h bay {}", bay.idx);
            for cell in view.joinery() {
                assert_eq!(
                    at(cell),
                    theme.surface.window_frame,
                    "frame {here} {cell:?}"
                );
            }
            let open = sky.window(bay, rows.clone(), d, |_| None);
            let town = sky.window(bay, rows.clone(), d, city.front(run.start, d));
            for (((cell, c), (_, sky)), (_, town)) in
                view.cells().zip(open.cells()).zip(town.cells())
            {
                glass += 1;
                assert_eq!(at(cell), c, "glass {here} {cell:?}");
                // the city stands in front of the clouds
                if town != sky {
                    buildings += 1;
                    assert_eq!(c, town, "city {here} {cell:?}");
                }
            }
        }
        assert!(glass > 0 && buildings > 0, "windows, and a city in them");
    }
}

#[test]
fn the_wall_between_two_windows_is_one_frame_post() {
    let pack = test_office();
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    let scale = RenderScale::from(pack.max_density_variant());
    let pen = Pen::for_pack(scale, &pack);
    let mut buf = RgbBuffer::filled(
        scale.to_buffer(layout.buf_w),
        scale.to_buffer(layout.buf_h),
        theme.surface.bg_fallback,
    );
    paint_backdrop(
        &Backdrop::of(&layout, theme),
        Carpet::plain(theme),
        scale,
        pen,
        &mut buf,
    );
    let k = scale.get() / pen.density().get();
    let rows = crate::layout::window_rows(layout.wall_band_h());
    let mut posts = 0;
    for post in crate::layout::window_posts(layout.wall_span()) {
        posts += 1;
        for x in post.clone() {
            for y in rows.clone() {
                let (ax, ay) = (pen.art(x).0, pen.art(y).0);
                assert_eq!(
                    buf.get(ax * k, ay * k),
                    theme.surface.window_frame,
                    "post {post:?} at ({x}, {y})"
                );
            }
        }
    }
    assert!(posts > 0, "this wall has posts");
}

/// At the art's density the requires-action mark and a z float clear of their
/// figure's badge however far the z has risen, the z only climbs and drifts
/// away, and the dust lies along the stepping foot's row.
#[test]
fn the_dense_looks_keep_their_places() {
    looks_keep_their_places(test_office().max_density_variant().get());
}

/// [`the_dense_looks_keep_their_places`] on the base art, which draws its own
/// z and mark beside the head.
#[test]
fn the_base_looks_keep_their_places() {
    looks_keep_their_places(1);
}

fn looks_keep_their_places(s: u16) {
    use crate::effects::EffectKind as K;
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::South, 2);
    let scale = RenderScale::new(s).expect("nonzero");
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale,
    };
    let mut frame = frames.last().expect("a seated frame").clone();
    for c in &mut frame.characters {
        c.effects = every_effect(c.top_left);
    }
    let list = list_at(&frame, office, 12);
    let badge = list
        .pieces()
        .iter()
        .find(|p| matches!(p.kind, PieceKind::Text { .. }))
        .expect("the badge")
        .span;
    let rider = |kind| {
        list.pieces()
            .iter()
            .find_map(|p| match p.kind {
                PieceKind::Effect(r) if r.effect.kind == kind => Some((r, p.span)),
                _ => None,
            })
            .expect("the rider")
    };
    let clear = |s: Span| s.x1 < badge.x0 || badge.x1 < s.x0 || s.y1 < badge.y0 || badge.y1 < s.y0;
    let (_, mark) = rider(K::RequiresActionMark);
    assert!(
        clear(mark),
        "at scale {s} the mark {mark:?} lands on the badge {badge:?}"
    );
    let (z, _) = rider(K::SleepZ);
    let mut last: Option<Span> = None;
    for phase in (0..crate::effects::SLEEP_Z_RISE_MS).step_by(100) {
        let r = crate::display::effects::Riding {
            effect: crate::effects::Effect { phase, ..z.effect },
            ..z
        };
        let Some(s) = r.span(Span::new(0, 0, 1, 1, 0)) else {
            continue;
        };
        assert!(
            clear(s),
            "at scale {} the z at {phase} ms lands on the badge {badge:?}",
            scale.get()
        );
        if let Some(l) = last {
            assert!(
                s.y0 <= l.y0 && s.x0 >= l.x0,
                "the z sank or drifted back at {phase} ms"
            );
        }
        last = Some(s);
    }
    let (dust, span) = rider(K::WalkingDust);
    let foot = crate::effects::look::walking_dust_foot(dust.effect.at, dust.effect.phase);
    assert!(
        (span.x0..=span.x1).contains(&foot.x) && (span.y0..=span.y1).contains(&foot.y),
        "the dust {span:?} is off the foot {foot:?}"
    );
}

/// A cache shared across frames paints each as a fresh cache would, each
/// step changing one of the backdrop's inputs — the layout's backdrop,
/// the weather's carpet, the scale: the key holds them all.
#[test]
fn a_shared_backdrop_cache_paints_every_frame_as_a_fresh_one() {
    use crate::sky::{Sky, Weather};
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let now = crate::localclock::at_hour(12);
    let layouts = [0, 1].map(|seed| {
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, seed).expect("lays out")
    });
    assert_ne!(
        Backdrop::of(&layouts[0], theme),
        Backdrop::of(&layouts[1], theme),
        "the layouts' backdrops differ"
    );
    let mut shared = CutawayCache::default();
    let mut fresh_frames = Vec::new();
    for s in [1, pack.max_density_variant().get()] {
        let scale = RenderScale::new(s).expect("nonzero");
        for (seed, weather) in [
            (0, Weather::Clear),
            (1, Weather::Clear),
            (1, Weather::Snow),
            (0, Weather::Snow),
        ] {
            let layout = &layouts[seed];
            let office = Office {
                layout,
                pack: &pack,
                theme,
                scale,
            };
            let list = compose_at(
                &empty_frame(layout),
                office,
                &Moment::resolve(
                    Sky::at_with(now, weather),
                    theme,
                    0.0,
                    Motion::Full.timing(now),
                ),
                crate::floor::FloorMeta::ground(),
                quiet_board(),
                (
                    &mut crate::display::compose::LightCache::default(),
                    &mut crate::outside::OutsideCache::default(),
                ),
            );
            let blank = || {
                RgbBuffer::filled(
                    scale.to_buffer(layout.buf_w),
                    scale.to_buffer(layout.buf_h),
                    theme.surface.bg_fallback,
                )
            };
            let (mut fresh, mut stamped) = (blank(), blank());
            paint(&list, &mut CutawayCache::default(), &mut fresh);
            paint(&list, &mut shared, &mut stamped);
            assert!(
                fresh.as_slice() == stamped.as_slice(),
                "scale {s}, layout {seed}, {weather:?}"
            );
            fresh_frames.push(fresh);
        }
    }
    assert!(
        fresh_frames
            .windows(2)
            .all(|w| w[0].as_slice() != w[1].as_slice()),
        "each step changes the frame"
    );
}

/// A storm's strike lifts the whole frame by whole ramp steps, what glows
/// of its own too, and its window glass further, the bolt's. That only a
/// storm strikes is the model's: `a_strike_flashes_at_its_bucket_offset_and_ends_with_the_flash`.
#[test]
fn a_strike_lifts_the_room_and_its_glass_most() {
    use crate::sky::{Sky, Weather};
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    let frame = empty_frame(&layout);
    let now = crate::localclock::at_hour(23);
    let lift = crate::display::light::FLASH_MAX_STEPS as i8;
    for s in [1, pack.max_density_variant().get()] {
        let scale = RenderScale::new(s).expect("nonzero");
        let office = Office {
            layout: &layout,
            pack: &pack,
            theme,
            scale,
        };
        let drawn = |weather, strike| {
            let sky = Sky::at_with(now, weather).with_strike(strike);
            let list = compose_at(
                &frame,
                office,
                &Moment::resolve(sky, theme, 0.0, Motion::Full.timing(now)),
                crate::floor::FloorMeta::ground(),
                quiet_board(),
                (
                    &mut crate::display::compose::LightCache::default(),
                    &mut crate::outside::OutsideCache::default(),
                ),
            );
            let mut buf = RgbBuffer::filled(
                scale.to_buffer(layout.buf_w),
                scale.to_buffer(layout.buf_h),
                theme.surface.bg_fallback,
            );
            paint(&list, &mut CutawayCache::default(), &mut buf);
            let glass: Vec<Span> = list
                .pieces()
                .iter()
                .filter(|p| matches!(p.kind, PieceKind::Window { .. }))
                .map(|p| p.span)
                .collect();
            (buf, glass)
        };
        let (calm, glass) = drawn(Weather::Storm, None);
        let (strike, _) = drawn(Weather::Storm, Some(crate::sky::StrikePhase::Primary));
        let in_glass = |x: u16, y: u16| {
            let (lx, ly) = (x / s, y / s);
            glass
                .iter()
                .any(|g| (g.x0..=g.x1).contains(&lx) && (g.y0..=g.y1).contains(&ly))
        };
        let mut bolted = 0;
        for y in 0..calm.height() {
            for x in 0..calm.width() {
                let (c, f) = (calm.get(x, y), strike.get(x, y));
                if in_glass(x, y) {
                    bolted += usize::from(f.lightness() > c.ramp(lift).lightness());
                } else {
                    assert_eq!(f, c.ramp(lift), "at scale {s}, ({x}, {y}) took no strike");
                }
            }
        }
        assert!(
            bolted > 0,
            "at scale {s} the bolt lit no glass past the room"
        );
    }
}

/// [`mark`] writes nothing outside the piece's span, at every scale: a lit
/// and a dark screen, a desk lamp's bulb, the wall decor's, a floor lamp's
/// and an open elevator's.
#[test]
fn a_piece_marks_its_glow_only_inside_its_span() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (seated, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
    let lively = lively_office();
    let open = SimFrame {
        door_frame: 2,
        ..empty_frame(&lively)
    };
    let mut marked = std::collections::BTreeSet::new();
    for (layout, frame) in [
        (&seated, frames.last().expect("a seated frame")),
        (&seated, &frames[0]),
        (&lively, &open),
    ] {
        for s in [1, 3, pack.max_density_variant().get()] {
            let scale = RenderScale::new(s).expect("nonzero");
            let office = Office {
                layout,
                pack: &pack,
                theme,
                scale,
            };
            let list = list_at(frame, office, 23);
            let (w, h) = (scale.to_buffer(layout.buf_w), scale.to_buffer(layout.buf_h));
            let mut art = ArtCache::default();
            for p in list.pieces() {
                let mut marks = RgbBuffer::filled(w, h, NO_MARK);
                if mark(&p.kind, (&pack, scale), &mut art, &mut marks) {
                    marked.insert(kind_name(&p.kind));
                }
                let stray = marks.as_slice().iter().enumerate().find(|&(i, &c)| {
                    let (x, y) = (
                        scale.logical((i % usize::from(w)) as u16),
                        scale.logical((i / usize::from(w)) as u16),
                    );
                    c != NO_MARK
                        && !((p.span.x0..=p.span.x1).contains(&x)
                            && (p.span.y0..=p.span.y1).contains(&y))
                });
                assert_eq!(
                    stray.map(|(i, _)| i),
                    None,
                    "{} at scale {s} marked outside {:?}",
                    kind_name(&p.kind),
                    p.span
                );
            }
        }
    }
    let want: &[&str] = &["desk", "door", "hung decor", "prop"];
    assert_eq!(
        marked.into_iter().collect::<Vec<_>>(),
        want,
        "a kind that marks went unchecked"
    );
}

/// `list` painted as by day, over its backdrop and shadows, with each
/// pixel's glow: what the net pass starts from.
fn by_day(
    list: &DisplayList<'_>,
    layout: &SceneLayout,
) -> (RgbBuffer, crate::cutaway::light::Emission) {
    let mut buf = RgbBuffer::filled(
        list.scale().to_buffer(layout.buf_w),
        list.scale().to_buffer(layout.buf_h),
        list.backdrop().tones.bg,
    );
    let pen = Pen::for_pack(list.scale(), list.pack());
    paint_backdrop(list.backdrop(), list.carpet(), list.scale(), pen, &mut buf);
    paint_ground_shadows(
        list.pieces().iter().filter_map(|p| p.shadow),
        crate::ground::shadow_strength(list.ambient().darkness()),
        pen,
        &mut buf,
    );
    let mut cache = CutawayCache::default();
    let emission = paint_pieces(list, &mut cache, &mut buf);
    (buf, emission)
}

/// At night, a pixel no light reaches is its daylight colour stepped down
/// by the room once: a wall's glass over the ground included, which recolours
/// the ground it lies on and so must not darken it a second time. A pixel a
/// light reaches is lifted back toward daylight, never past.
#[test]
fn the_night_room_is_its_daylight_stepped_down_once() {
    use crate::cutaway::light::Glow;
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
    let frame = frames.last().expect("a seated frame");
    let scale = RenderScale::from(pack.max_density_variant());
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale,
    };
    let list = list_at(frame, office, 23);
    assert!(list.ambient().darkness() > 0.0, "23:00 is dark");
    let (day, emission) = by_day(&list, &layout);
    let mut cache = CutawayCache::default();
    let mut night = RgbBuffer::filled(day.width(), day.height(), theme.surface.bg_fallback);
    paint_backdrop(
        &Backdrop::of(&layout, theme),
        list.carpet(),
        scale,
        Pen::for_pack(scale, &pack),
        &mut night,
    );
    paint_list(&list, &mut cache, &mut night);
    let lights: Vec<&crate::display::light::LightView> =
        list.lights().iter().map(|l| &*l.view).collect();
    let k = scale.get() / Pen::for_pack(scale, &pack).density().get();
    let walls: Vec<Span> = list
        .pieces()
        .iter()
        .filter(|p| matches!(p.kind, PieceKind::WallSeg { .. }))
        .map(|p| p.span)
        .collect();
    let (mut unlit, mut under_walls) = (0, 0);
    for y in 0..day.height() {
        for x in 0..day.width() {
            if emission.get(x, y) != Glow::Lit {
                continue;
            }
            let lift = lights
                .iter()
                .map(|l| l.lift_at(x / k, y / k))
                .max()
                .unwrap_or(0);
            if lift == 0 {
                assert_eq!(
                    night.get(x, y),
                    list.ambient().on(day.get(x, y)),
                    "({x}, {y}) is not its daylight a room's steps down"
                );
                unlit += 1;
                let (lx, ly) = (scale.logical(x), scale.logical(y));
                under_walls += usize::from(
                    walls
                        .iter()
                        .any(|w| (w.x0..=w.x1).contains(&lx) && (w.y0..=w.y1).contains(&ly)),
                );
            }
            assert!(
                f32::from(lift) <= list.ambient().darkness() * 4.0,
                "({x}, {y}) is lifted {lift} steps past the night"
            );
        }
    }
    assert!(
        unlit > 0 && under_walls > 0,
        "no unlit wall glass was checked"
    );
}

/// The hour reaches the room: over the whole frame, the night room is
/// darker than the noon one.
#[test]
fn the_night_room_is_darker_than_the_noon_room() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
    let frame = frames.last().expect("a seated frame");
    let scale = RenderScale::from(pack.max_density_variant());
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale,
    };
    let luma = |hour: u32| -> u64 {
        let mut buf = RgbBuffer::filled(
            scale.to_buffer(layout.buf_w),
            scale.to_buffer(layout.buf_h),
            theme.surface.bg_fallback,
        );
        let mut cache = CutawayCache::default();
        render_cutaway(
            frame,
            office,
            showing(
                crate::floor::FloorMeta::ground(),
                crate::localclock::at_hour(hour),
            ),
            (&mut cache, &mut crate::outside::OutsideCache::default()),
            &mut buf,
        );
        buf.as_slice()
            .iter()
            .map(|c| u64::from(c.r) + u64::from(c.g) + u64::from(c.b))
            .sum()
    };
    assert!(luma(23) < luma(12), "midnight lights the room like noon");
}

/// Record each of `list`'s pieces `keep` selects by (scale, span,
/// fingerprint), asserting one that recurs paints the pixels it painted
/// before; returns how many recurred.
fn same_fingerprint_same_pixels(
    painted: &mut std::collections::HashMap<(u16, Span, u64), u64>,
    list: &DisplayList<'_>,
    layout: &SceneLayout,
    keep: impl Fn(&Piece) -> bool,
) -> usize {
    let mut repeats = 0;
    for piece in list.pieces().iter().filter(|p| keep(p)) {
        let pixels = painted_alone(
            &piece.kind,
            layout,
            list.pack(),
            list.recolours(),
            list.scale(),
        );
        let s = list.scale().get();
        match painted.entry((s, piece.span, piece.fingerprint)) {
            std::collections::hash_map::Entry::Occupied(seen) => {
                assert_eq!(
                    *seen.get(),
                    pixels,
                    "{:?} at scale {s} shares {:?}'s fingerprint but paints differently",
                    piece.kind,
                    piece.span
                );
                repeats += 1;
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(pixels);
            }
        }
    }
    repeats
}

/// The two fills a piece is painted over to tell what it writes.
const UNDER: [crate::sprite::Rgb; 2] = [
    crate::sprite::Rgb { r: 0, g: 0, b: 0 },
    crate::sprite::Rgb {
        r: 255,
        g: 255,
        b: 255,
    },
];

/// `kind` painted alone over each of two opposite fills: where the two
/// buffers agree, the piece wrote the pixel.
pub(crate) fn painted_over_two_fills(
    kind: &PieceKind,
    layout: &SceneLayout,
    pack: &OfficeArt,
    recolours: &Recolours,
    scale: RenderScale,
) -> [RgbBuffer; 2] {
    let (w, h) = (scale.to_buffer(layout.buf_w), scale.to_buffer(layout.buf_h));
    UNDER.map(|fill| {
        let mut buf = RgbBuffer::filled(w, h, fill);
        paint_piece(
            kind,
            pack,
            recolours,
            scale,
            &mut CutawayCache::default(),
            &mut buf,
        );
        buf
    })
}

/// A hash of the pixels `kind` writes, painted alone.
fn painted_alone(
    kind: &PieceKind,
    layout: &SceneLayout,
    pack: &OfficeArt,
    recolours: &Recolours,
    scale: RenderScale,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    for buf in painted_over_two_fills(kind, layout, pack, recolours, scale) {
        buf.as_slice().hash(&mut hasher);
    }
    hasher.finish()
}

/// The first logical pixel `kind` writes outside `span`, painted alone.
fn stray_pixel(
    kind: &PieceKind,
    span: Span,
    layout: &SceneLayout,
    pack: &OfficeArt,
    theme: &Theme,
    scale: RenderScale,
) -> Option<(u16, u16)> {
    let painted = painted_over_two_fills(kind, layout, pack, &Recolours::of(theme), scale);
    stray_pixel_of(kind, span, scale.to_buffer(layout.buf_w), scale, &painted)
}

/// [`stray_pixel`] over `kind`'s [`painted_over_two_fills`], `w` wide.
fn stray_pixel_of(
    kind: &PieceKind,
    span: Span,
    w: u16,
    scale: RenderScale,
    [a, b]: &[RgbBuffer; 2],
) -> Option<(u16, u16)> {
    // A pixel that is neither the same over both fills nor left alone is a
    // recolouring of what lay under it.
    let recoloured = a
        .as_slice()
        .iter()
        .zip(b.as_slice())
        .any(|(pa, pb)| pa != pb && [*pa, *pb] != UNDER);
    assert!(
        !recoloured || kind.reads_under(),
        "{kind:?} recolours what lies under it, so it must say it reads under"
    );
    a.as_slice()
        .iter()
        .zip(b.as_slice())
        .enumerate()
        .filter(|(_, (pa, pb))| {
            if kind.reads_under() {
                [**pa, **pb] != UNDER
            } else {
                pa == pb
            }
        })
        .map(|(i, _)| {
            let (x, y) = (i % usize::from(w), i / usize::from(w));
            (scale.logical(x as u16), scale.logical(y as u16))
        })
        .find(|&(x, y)| !((span.x0..=span.x1).contains(&x) && (span.y0..=span.y1).contains(&y)))
}

#[test]
fn the_walls_contact_row_is_the_ground_a_shade_down() {
    let pack = test_office();
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let scale = RenderScale::from(pack.max_density_variant());
    let pen = Pen::for_pack(scale, &pack);
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(240, 144), None, 0).expect("lays out");
    let blank = || {
        RgbBuffer::filled(
            scale.to_buffer(layout.buf_w),
            scale.to_buffer(layout.buf_h),
            UNDER[0],
        )
    };
    let (mut ground, mut laid) = (blank(), blank());
    paint_carpet(
        &Backdrop::of(&layout, theme),
        Carpet::plain(theme),
        pen,
        &mut ground,
    );
    paint_backdrop(
        &Backdrop::of(&layout, theme),
        Carpet::plain(theme),
        scale,
        pen,
        &mut laid,
    );
    let row = scale.to_buffer(layout.wall_band_h());
    for x in 0..ground.width() {
        assert_eq!(
            laid.get(x, row),
            ground
                .get(x, row)
                .ramp(crate::cutaway::shade::RAMP_SHADE_LEVEL),
            "column {x}"
        );
    }
}

#[test]
fn a_rug_is_mirrored_about_its_centre_column() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let scale = RenderScale::from(pack.max_density_variant());
    let pen = Pen::for_pack(scale, &pack);
    let rug = crate::layout::Bounds {
        x: 4,
        y: 3,
        width: 18,
        height: 11,
    };
    let (w, h) = (
        scale.to_buffer(rug.x * 2 + rug.width),
        scale.to_buffer(rug.y * 2 + rug.height),
    );
    let mut buf = RgbBuffer::filled(w, h, UNDER[0]);
    paint_rug(rug, rug_tones(theme), pen, &mut buf);
    for y in 0..h {
        for x in 0..w {
            assert_eq!(buf.get(x, y), buf.get(w - 1 - x, y), "({x}, {y})");
        }
    }
}

fn lowest_painted_row(
    kind: &PieceKind,
    w: u16,
    scale: RenderScale,
    [a, b]: &[RgbBuffer; 2],
) -> Option<u16> {
    let w = usize::from(w);
    a.as_slice()
        .iter()
        .zip(b.as_slice())
        .enumerate()
        .filter(|(_, (pa, pb))| {
            if kind.reads_under() {
                [**pa, **pb] != UNDER
            } else {
                pa == pb
            }
        })
        .map(|(i, _)| scale.logical((i / w) as u16))
        .max()
}

/// The table's span ends on the last row it paints, at the base density
/// (with the face derived under it) and at the densest (whose art draws its
/// own front): a span reaching past it would sort the table and cast its
/// shadow rows below where it stands.
#[test]
fn the_tables_span_ends_where_it_paints() {
    let pack = test_office();
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    for s in [1, pack.max_density_variant().get()] {
        let scale = RenderScale::new(s).expect("nonzero");
        let order = queued(&layout, &pack, scale, &[], |k| {
            matches!(k, FixtureKind::MeetingTable { .. })
        });
        let (span, kind) = order
            .iter()
            .find(|(_, k)| matches!(k, PieceKind::Table { .. }))
            .expect("a meeting trio");
        let [a, b] = painted_over_two_fills(kind, &layout, &pack, &Recolours::of(theme), scale);
        let bottom = (0..a.height())
            .rev()
            .find(|&y| (0..a.width()).any(|x| a.get(x, y) != UNDER[0] || b.get(x, y) != UNDER[1]))
            .expect("the table paints");
        assert_eq!(
            span.y1,
            bottom / s,
            "scale {s}: the span's south row is not the table's painted bottom"
        );
    }
}

/// What the canvas relies on for what moves: a moving piece that keeps its
/// span and fingerprint paints what it painted, across the clock, the
/// elevator and the sign; and each shows more than one fingerprint over
/// them, so a kind that stopped hashing what moves it fails here.
#[test]
fn a_moving_pieces_fingerprint_moves_with_what_it_shows() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let layout = lively_office();
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale: RenderScale::from(pack.max_density_variant()),
    };
    let moving = |p: &Piece| {
        matches!(
            p.kind,
            PieceKind::Animated { .. }
                | PieceKind::Door { .. }
                | PieceKind::Neon { .. }
                | PieceKind::Clock { .. }
        )
    };
    let mut painted = std::collections::HashMap::new();
    let mut seen: std::collections::HashMap<&str, std::collections::HashSet<u64>> =
        std::collections::HashMap::new();
    let base = crate::localclock::at_hour_min(12, 0);
    let moments = [
        base,
        base + std::time::Duration::from_millis(450),
        base + std::time::Duration::from_millis(900),
        crate::localclock::at_hour_min(12, 15),
        crate::localclock::at_hour_min(3, 40),
    ];
    let levels = [
        crate::floor::NeonLevels::CALM,
        crate::floor::NeonLevels::ALERT,
        crate::floor::NeonLevels::VACANT,
    ];
    for now in moments {
        for door_frame in 0..3 {
            for neon in levels {
                let frame = SimFrame {
                    door_frame,
                    neon,
                    ..empty_frame(&layout)
                };
                let list = list_now(&frame, office, now);
                same_fingerprint_same_pixels(&mut painted, &list, &layout, moving);
                for p in list.pieces().iter().filter(|p| moving(p)) {
                    seen.entry(kind_name(&p.kind))
                        .or_default()
                        .insert(p.fingerprint);
                }
            }
        }
    }
    for kind in ["animated", "door", "neon", "clock"] {
        let n = seen.get(kind).map_or(0, |s| s.len());
        assert!(
            n > 1,
            "the {kind} showed one fingerprint across what moves it"
        );
    }
}

/// A light of its own keeps its colour through the night: the sign's tube,
/// a floor lamp's bulb, and the ceiling of an open elevator's car, while
/// the room around them darkens.
#[test]
fn what_glows_of_its_own_keeps_its_colour_at_night() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let layout = lively_office();
    let scale = RenderScale::from(pack.max_density_variant());
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale,
    };
    let frame = SimFrame {
        door_frame: 2,
        neon: crate::floor::NeonLevels::ALERT,
        ..empty_frame(&layout)
    };
    let list = list_at(&frame, office, 23);
    let pen = Pen::for_pack(scale, &pack);
    let blank = || {
        RgbBuffer::filled(
            scale.to_buffer(layout.buf_w),
            scale.to_buffer(layout.buf_h),
            crate::sprite::Rgb { r: 0, g: 0, b: 0 },
        )
    };
    let (mut night, mut raw) = (blank(), blank());
    let mut cache = CutawayCache::default();
    paint_backdrop(
        &Backdrop::of(&layout, theme),
        Carpet::plain(theme),
        scale,
        pen,
        &mut night,
    );
    paint_list(&list, &mut cache, &mut night);
    paint_backdrop(
        &Backdrop::of(&layout, theme),
        Carpet::plain(theme),
        scale,
        pen,
        &mut raw,
    );
    for p in list.pieces() {
        paint_piece(
            &p.kind,
            &pack,
            list.recolours(),
            scale,
            &mut cache,
            &mut raw,
        );
    }
    assert_ne!(
        night.as_slice(),
        raw.as_slice(),
        "the night room kept its daylight"
    );
    // Every buffer pixel of each bulb-key art pixel `art` draws placed so.
    let bulbs = |placed: Placed, art: Art| -> Vec<(u16, u16)> {
        let dense = crate::pack::densest_frame(&pack, art.sprite, art.frame, scale);
        let lit = drawn_in(&dense, &[crate::pack::DESK_BULB_KEY]);
        let (x0, y0) = placed.top_left(dense.logical, scale);
        let w = usize::from(dense.frame.width());
        let k = dense.blit_at.get();
        lit.iter()
            .enumerate()
            .filter(|&(_, &b)| b)
            .map(|(i, _)| (x0 + (i % w) as u16 * k, y0 + (i / w) as u16 * k))
            .collect()
    };
    let (mut signs, mut lamps, mut cars) = (0, 0, 0);
    for p in list.pieces() {
        let cells = match p.kind {
            PieceKind::Neon { at, .. } => {
                signs += 1;
                let (x0, y0) = (scale.to_buffer(at.x), scale.to_buffer(at.y));
                let (x1, y1) = (
                    scale.to_buffer(at.x + at.width),
                    scale.to_buffer(at.y + at.height),
                );
                (y0..y1)
                    .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                    .collect()
            }
            PieceKind::Prop { at, art } if art.sprite == PackPiece::FloorLamp => {
                let cells = bulbs(Placed::Centred(at), art);
                lamps += cells.len();
                cells
            }
            PieceKind::Door { at, frame } => {
                let art = Art {
                    sprite: PackPiece::Door,
                    frame,
                    flip: Flip::None,
                };
                let cells = bulbs(Placed::TopLeft(at), art);
                cars += cells.len();
                cells
            }
            _ => Vec::new(),
        };
        for (x, y) in cells {
            assert_eq!(
                night.get(x, y),
                raw.get(x, y),
                "{} dimmed at ({x}, {y})",
                kind_name(&p.kind)
            );
        }
    }
    assert!(
        signs == 1 && lamps > 0 && cars > 0,
        "sign {signs}, lamp {lamps}, car {cars}"
    );
}

/// At night a window's sky keeps its own light, never darkened with the
/// room, while the sign's glow still lifts the glass beside it.
#[test]
fn a_window_pane_keeps_its_sky_and_takes_the_signs_glow() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let layout = lively_office();
    let scale = RenderScale::from(pack.max_density_variant());
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale,
    };
    let frame = SimFrame {
        neon: crate::floor::NeonLevels::ALERT,
        ..empty_frame(&layout)
    };
    let list = list_at(&frame, office, 23);
    let pen = Pen::for_pack(scale, &pack);
    let blank = || {
        RgbBuffer::filled(
            scale.to_buffer(layout.buf_w),
            scale.to_buffer(layout.buf_h),
            crate::sprite::Rgb { r: 0, g: 0, b: 0 },
        )
    };
    let mut cache = CutawayCache::default();
    let painted = |keep: &dyn Fn(&PieceKind) -> bool, cache: &mut _| {
        let mut buf = blank();
        paint_backdrop(
            &Backdrop::of(&layout, theme),
            Carpet::plain(theme),
            scale,
            pen,
            &mut buf,
        );
        for p in list.pieces().iter().filter(|p| keep(&p.kind)) {
            paint_piece(&p.kind, &pack, list.recolours(), scale, cache, &mut buf);
        }
        buf
    };
    let backdrop = painted(&|_| false, &mut cache);
    let glass = painted(&|k| matches!(k, PieceKind::Window { .. }), &mut cache);
    let all = painted(&|_| true, &mut cache);
    let mut night = blank();
    paint_backdrop(
        &Backdrop::of(&layout, theme),
        Carpet::plain(theme),
        scale,
        pen,
        &mut night,
    );
    paint_list(&list, &mut cache, &mut night);
    let luma = crate::sprite::Rgb::lightness;
    let (mut kept, mut lifted) = (0, 0);
    for y in 0..night.height() {
        for x in 0..night.width() {
            let s = scale.get();
            let pane = layout.glass_at(x / s, y / s)
                && glass.get(x, y) != backdrop.get(x, y)
                && all.get(x, y) == glass.get(x, y);
            if !pane {
                continue;
            }
            let (lit, sky) = (night.get(x, y), all.get(x, y));
            assert!(
                luma(lit) >= luma(sky),
                "the night darkened a pane at ({x}, {y})"
            );
            kept += usize::from(lit == sky);
            lifted += usize::from(luma(lit) > luma(sky));
        }
    }
    assert!(kept > 0 && lifted > 0, "panes kept {kept}, lifted {lifted}");
}

/// The clock's hands stay inside the face its dial art draws, at every
/// reading of the day.
#[test]
fn the_clocks_hands_stay_on_its_face() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let scale = RenderScale::from(pack.max_density_variant());
    let at = Point { x: 2, y: 2 };
    let dial = crate::pack::densest_frame(&pack, PackPiece::WallClock, 0, scale);
    let face = drawn_in(&dial, &[crate::pack::CLOCK_FACE_KEY]);
    let (w, k) = (usize::from(dial.frame.width()), dial.blit_at.get());
    let on_face = |x: u16, y: u16| {
        let (ax, ay) = (
            usize::from((x - scale.to_buffer(at.x)) / k),
            usize::from((y - scale.to_buffer(at.y)) / k),
        );
        face.get(ay * w + ax).copied().unwrap_or(false)
    };
    let blank = || {
        RgbBuffer::filled(
            scale.to_buffer(12),
            scale.to_buffer(12),
            crate::sprite::Rgb { r: 0, g: 0, b: 0 },
        )
    };
    let mut bare = blank();
    blit_frame_scaled(
        &dial.recolorable.recolored(&Recolours::of(theme).art),
        scale.to_buffer(at.x),
        scale.to_buffer(at.y),
        dial.blit_at,
        &mut bare,
    );
    let mut hands = 0;
    for minutes in (0..12 * 60).step_by(7) {
        let reading = crate::sky::ClockReading {
            hour: minutes / 60,
            minute: minutes % 60,
        };
        let mut buf = blank();
        paint_clock(
            at,
            reading,
            theme.office.clock_hand,
            &pack,
            &Recolours::of(theme).art,
            scale,
            &mut buf,
        );
        for y in 0..buf.height() {
            for x in 0..buf.width() {
                if buf.get(x, y) != bare.get(x, y) {
                    hands += 1;
                    assert!(
                        on_face(x, y),
                        "{reading:?}: a hand leaves the face at ({x}, {y})"
                    );
                }
            }
        }
    }
    assert!(hands > 0, "no hand was drawn");
}

/// Every fixture the roster yields is drawn: queued as a piece of the list,
/// or laid by the backdrop as a covering, over the ground. Every kind the
/// roster has is met on some office.
#[test]
fn the_cutaway_draws_every_fixture_the_roster_yields() {
    let pack = test_office();
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let scale = RenderScale::from(pack.max_density_variant());
    let pen = Pen::for_pack(scale, &pack);
    let mut met = std::collections::BTreeSet::new();
    for layout in many_layouts() {
        let blank = || {
            RgbBuffer::filled(
                scale.to_buffer(layout.buf_w),
                scale.to_buffer(layout.buf_h),
                crate::sprite::Rgb { r: 0, g: 0, b: 0 },
            )
        };
        let (mut bare, mut laid) = (blank(), blank());
        paint_carpet(
            &Backdrop::of(&layout, theme),
            Carpet::plain(theme),
            pen,
            &mut bare,
        );
        paint_backdrop(
            &Backdrop::of(&layout, theme),
            Carpet::plain(theme),
            scale,
            pen,
            &mut laid,
        );
        for fixture in layout.fixtures() {
            met.insert(crate::layout::roster::tests::kind_key(fixture.kind));
            if Covering::of(fixture.kind, theme).is_some() {
                let b = fixture.visual;
                let (x0, y0) = (scale.to_buffer(b.x), scale.to_buffer(b.y));
                let (x1, y1) = (
                    scale.to_buffer(b.x + b.width),
                    scale.to_buffer(b.y + b.height),
                );
                let covered = (y0..y1.min(bare.height()))
                    .flat_map(|y| (x0..x1.min(bare.width())).map(move |x| (x, y)))
                    .any(|(x, y)| bare.get(x, y) != laid.get(x, y));
                assert!(covered, "{:?} lays nothing at {b:?}", fixture.kind);
            } else {
                let pieces = queued(&layout, &pack, scale, &[], |k| k == fixture.kind);
                assert!(!pieces.is_empty(), "{:?} queues no piece", fixture.kind);
            }
        }
    }
    assert_eq!(
        met,
        crate::layout::roster::tests::every_kind_key(),
        "a fixture kind was never met"
    );
}

/// The cutaway grounds exactly the fixtures the roster says stand
/// ([`Fixture::contact`](crate::layout::Fixture::contact)): the two
/// painters cast from one answer.
#[test]
fn the_cutaway_grounds_what_the_roster_says_stands() {
    let pack = test_office();
    let scale = RenderScale::from(pack.max_density_variant());
    for layout in many_layouts() {
        for fixture in layout.fixtures() {
            let grounded = Covering::of(fixture.kind, &crate::theme::NORMAL).is_none()
                && queued(&layout, &pack, scale, &[], |k| k == fixture.kind)
                    .iter()
                    .any(|(span, kind)| ground_shadow(*span, kind, &pack).is_some());
            assert_eq!(
                grounded,
                fixture.contact().is_some(),
                "{:?} on {}x{}",
                fixture.kind,
                layout.buf_w,
                layout.buf_h
            );
        }
    }
}

/// Splitting the back-view sofa loses and moves nothing: its bands paint
/// the art whole.
#[test]
fn a_sofas_bands_paint_its_art_whole() {
    let pack = test_office();
    let sofa = crate::layout::Point { x: 20, y: 10 };
    let (w, h) = base_size(&pack, PackPiece::MeetingSofaNorth);
    for s in [1, pack.max_density_variant().get()] {
        let scale = RenderScale::new(s).expect("nonzero");
        let ground = crate::sprite::Rgb { r: 1, g: 2, b: 3 };
        let blank = || RgbBuffer::filled(scale.to_buffer(w + 40), scale.to_buffer(h + 20), ground);
        let (mut whole, mut split) = (blank(), blank());
        paint_art(
            sofa,
            Art::still(PackPiece::MeetingSofaNorth),
            &pack,
            &Recolours::of(&crate::theme::NORMAL).art,
            scale,
            &mut whole,
        );
        let top = NORTH_SOFA_SEAT_ROWS;
        for rows in [(0, top), (top, h)] {
            paint_prop_band(
                sofa,
                PackPiece::MeetingSofaNorth,
                rows,
                &pack,
                scale,
                &mut split,
            );
        }
        assert!(
            whole.as_slice() == split.as_slice(),
            "scale {s}: the bands differ from the art"
        );
    }
}

/// [`face_rows`]' rule, through the real paint.
#[test]
fn only_the_top_down_base_desk_gets_a_derived_front_face() {
    let pack = test_office();
    let ground = crate::sprite::Rgb { r: 1, g: 2, b: 3 };
    let at = crate::layout::Point { x: 1, y: 1 };
    let (bw, bh) = base_size(&pack, PackPiece::Desk);
    let span = desk_span(&pack, Desk::South, at, RenderScale::ONE);
    for (s, face) in [(1, true), (8, false)] {
        let scale = RenderScale::new(s).expect("nonzero");
        let mut buf = RgbBuffer::filled(
            scale.to_buffer(bw + 2 * at.x),
            scale.to_buffer(span.y0 + bh + desk_front_h() + 1),
            ground,
        );
        paint_desk(
            at,
            Desk::South,
            Screen::Off,
            (&pack, scale),
            &mut ArtCache::default(),
            &mut buf,
        );
        let (x, below) = (
            scale.to_buffer(at.x + bw / 2),
            scale.to_buffer(span.y0 + bh),
        );
        assert_eq!(
            buf.get(x, below) != ground,
            face,
            "scale {s}: the row under the art is {}",
            if face {
                "the derived face"
            } else {
                "bare ground"
            }
        );
    }
}

/// Every static piece's painter draws the densest variant its scale lands,
/// not the base block-scaled: a prop (the sofa among them), wall decor and
/// the task chair.
#[test]
fn static_pieces_draw_their_density_variant() {
    let pack = crate::pack::test_office_with(
        "[palette]\n\"A\"=\"#010203\"\n\"B\"=\"#a0b0c0\"\n\
             [animations.plant]\nframes=[\"a.sprite\"]\nframe_ms=100\n\
             [animations.\"plant@2x\"]\nframes=[\"b.sprite\"]\nframe_ms=100\n\
             [animations.whiteboard]\nframes=[\"a.sprite\"]\nframe_ms=100\n\
             [animations.\"whiteboard@2x\"]\nframes=[\"b.sprite\"]\nframe_ms=100\n\
             [animations.desk_chair]\nframes=[\"a.sprite\"]\nframe_ms=100\n\
             [animations.\"desk_chair@2x\"]\nframes=[\"b.sprite\"]\nframe_ms=100\n",
        &[
            ("a.sprite", "@frame 0\nA"),
            ("b.sprite", "@frame 0\nB B\nB B"),
        ],
    );
    let variant = crate::sprite::Rgb {
        r: 0xa0,
        g: 0xb0,
        b: 0xc0,
    };
    let scale = RenderScale::new(2).expect("nonzero");
    let at = crate::layout::Point { x: 3, y: 3 };
    let blank = || RgbBuffer::filled(16, 16, crate::sprite::Rgb { r: 0, g: 0, b: 0 });
    let drawn = |buf: &RgbBuffer| buf.get(scale.to_buffer(at.x), scale.to_buffer(at.y));

    let mut buf = blank();
    let theme = &crate::theme::NORMAL;
    paint_art(
        at,
        Art::still(PackPiece::Plant),
        &pack,
        &Recolours::of(theme).art,
        scale,
        &mut buf,
    );
    assert_eq!(drawn(&buf), variant, "prop");
    let mut buf = blank();
    let flipped = Art {
        flip: Flip::Horizontal,
        ..Art::still(PackPiece::Plant)
    };
    paint_art(
        at,
        flipped,
        &pack,
        &Recolours::of(theme).art,
        scale,
        &mut buf,
    );
    assert_eq!(drawn(&buf), variant, "mirrored prop");
    let mut buf = blank();
    paint_wall_decor(at, PackPiece::Whiteboard, &pack, scale, &mut buf);
    assert_eq!(drawn(&buf), variant, "wall decor");
    let mut buf = blank();
    paint_chair(at, &pack, scale, &mut buf);
    assert_eq!(drawn(&buf), variant, "chair");
}

/// `frame` painted through the real cutaway at render scale `s`.
fn render_at(
    frame: &SimFrame,
    layout: &SceneLayout,
    pack: &OfficeArt,
    theme: &Theme,
    s: u16,
) -> RgbBuffer {
    let scale = RenderScale::new(s).expect("nonzero");
    let mut buf = RgbBuffer::filled(
        scale.to_buffer(layout.buf_w),
        scale.to_buffer(layout.buf_h),
        theme.surface.bg_fallback,
    );
    let mut cache = CutawayCache::default();
    render_cutaway(
        frame,
        Office {
            layout,
            pack,
            theme,
            scale,
        },
        showing(
            crate::floor::FloorMeta::ground(),
            std::time::SystemTime::UNIX_EPOCH,
        ),
        (&mut cache, &mut crate::outside::OutsideCache::default()),
        &mut buf,
    );
    buf
}

/// The cutaway paints on ONE grid, the art pixel: at a render scale `k`
/// times the art's density, the frame is the density render upscaled `k`
/// times, which also makes every aligned `k`x`k` block one colour. A painter
/// sizing a thin feature in buffer pixels, or a band edge that rounds at
/// buffer resolution, breaks it.
///
/// Every theme over a walk and a sit; the office, there to gate in a pantry
/// and a meeting room, under one theme, since no painter branches on it.
#[test]
fn the_cutaway_paints_whole_art_pixels() {
    use crate::floor::{FloorMeta, FloorSession};
    let pack = test_office();
    let d = pack.max_density_variant().get();
    let (walk_layout, _, frames, _) = sit_down(crate::layout::Facing::North, 2);
    let walking = &frames[frames.len() / 2];
    let seated = frames.last().expect("a seated frame");
    let office = FloorSession::new(std::sync::Arc::new(pack.clone()))
        .step(
            crate::floor::FloorInputs {
                scene: &pixtuoid_core::SceneState::uniform(16),
                now: std::time::SystemTime::UNIX_EPOCH,
                floor: FloorMeta::ground(),
                pets: crate::floor::PetInputs::default(),
            },
            crate::layout::Viewport::full(240, 144),
        )
        .expect("lays out");
    assert!(
        office.layout.pantry.is_some() && !office.layout.meeting_rooms.is_empty(),
        "the office must gate in the rooms it is here to cover"
    );
    let cases = [
        ("walking", &walk_layout, walking),
        ("seated", &walk_layout, seated),
        ("office", &office.layout, &office.frame),
    ];
    for theme in crate::theme::ALL_THEMES {
        for (name, layout, frame) in cases {
            if name == "office" && theme.name != crate::theme::ALL_THEMES[0].name {
                continue;
            }
            let at_d = render_at(frame, layout, &pack, theme, d);
            for k in [2u16, 3] {
                let at_s = render_at(frame, layout, &pack, theme, d * k);
                let off_upscale = (0..at_s.height())
                    .flat_map(|y| (0..at_s.width()).map(move |x| (x, y)))
                    .find(|&(x, y)| at_s.get(x, y) != at_d.get(x / k, y / k));
                if let Some(at) = off_upscale {
                    // The first split art pixel, if any, points at the
                    // painter: one sizing a feature in buffer pixels.
                    let split = (0..at_s.height())
                        .step_by(usize::from(k))
                        .flat_map(|y| {
                            (0..at_s.width())
                                .step_by(usize::from(k))
                                .map(move |x| (x, y))
                        })
                        .find(|&(x, y)| {
                            let c = at_s.get(x, y);
                            (0..k).any(|dy| (0..k).any(|dx| at_s.get(x + dx, y + dy) != c))
                        });
                    panic!(
                        "{} {name} at k={k}: the frame is not the density render upscaled \
                             (first at {at:?}; first split {k}x{k} art pixel: {split:?})",
                        theme.name
                    );
                }
            }
        }
    }
}

/// A back-turned desk's own art is taller only ABOVE the desk: it sorts on
/// the same base row as the plain desk, so swapping the art moves no depth.
#[test]
fn a_back_turned_desk_grows_upward_and_keeps_its_base_row() {
    let pack = test_office();
    let desk = crate::layout::Point { x: 20, y: 30 };
    let plain = desk_span(&pack, Desk::South, desk, RenderScale::ONE);
    let raised = desk_span(&pack, Desk::North, desk, RenderScale::ONE);
    assert_eq!(raised.depth, plain.depth);
    let ((_, plain_h), (_, north_h)) = (
        base_size(&pack, Desk::South.piece()),
        base_size(&pack, Desk::North.piece()),
    );
    assert!(
        north_h > plain_h,
        "the back-turned desk's monitor stands above the base desk"
    );
    assert_eq!(plain.y0 - raised.y0, north_h - plain_h);
}

/// A desk's shadow centres on the row just under the one it sorts on: the
/// row it meets the ground on. Pinned exactly, as the ordering tests compare
/// depths by inequality, which a one-row shift passes.
#[test]
fn a_desks_shadow_centres_on_the_row_under_where_it_sorts() {
    let pack = test_office();
    let desk = crate::layout::Point { x: 20, y: 30 };
    for which in [Desk::North, Desk::South] {
        let art = which.piece().name();
        for s in [1, pack.max_density_variant().get()] {
            let scale = RenderScale::new(s).expect("nonzero");
            let span = desk_span(&pack, which, desk, scale);
            let kind = PieceKind::Desk {
                at: desk,
                desk: which,
                screen: Screen::Off,
            };
            let shadow = ground_shadow(span, &kind, &pack).expect("a desk casts a shadow");
            let ((_, top), (_, past)) = shadow.bounds();
            assert_eq!(
                u16::midpoint(top, past),
                span.depth + 1,
                "{art} at scale {s}"
            );
        }
    }
}

/// A lit screen's scanline lights its glass on the column the model names
/// and nowhere else, at every density the desk art is drawn at: the glass's
/// columns split evenly among the classic's glass columns.
#[test]
fn a_lit_screens_scanline_is_on_the_models_column() {
    let pack = test_office();
    let art = Desk::North.piece();
    let glow = crate::sprite::Rgb {
        r: 40,
        g: 180,
        b: 220,
    };
    let line = crate::effects::look::scanline_color(glow);
    let cols = crate::layout::SCREEN_GLASS_COLS;
    let n = cols.end() - cols.start() + 1;
    for s in [1, pack.max_density_variant().get()] {
        let scale = RenderScale::new(s).expect("nonzero");
        let desk = crate::pack::densest_frame(&pack, art, 0, scale);
        let w = usize::from(desk.frame.width());
        let glass: Vec<u16> = drawn_in(
            &desk,
            &[crate::pack::SCREEN_GLASS_KEY, crate::pack::SCREEN_TEXT_KEY],
        )
        .iter()
        .enumerate()
        .filter(|&(_, &g)| g)
        .map(|(i, _)| (i % w) as u16)
        .collect();
        let x0 = *glass.iter().min().expect("the art draws glass");
        let band = (glass.iter().max().expect("glass") - x0 + 1) / n;
        for scan in 0..n {
            let lit = Screen::Lit { glow, scan }
                .on(desk.recolorable)
                .expect("a lit screen");
            let lined: std::collections::BTreeSet<u16> = (0..lit.height())
                .flat_map(|y| (0..lit.width()).map(move |x| (x, y)))
                .filter(|&(x, y)| lit.get(x, y).copied().flatten() == Some(line))
                .map(|(x, _)| x)
                .collect();
            assert_eq!(
                lined,
                (x0 + scan * band..x0 + (scan + 1) * band).collect(),
                "at scale {s}, scan {scan} lit other columns"
            );
        }
    }
}

/// A back-turned sitter's badge clears the raised monitor behind their head:
/// its whole plate lands above the desk art's top.
#[test]
fn a_back_turned_sitters_badge_clears_their_raised_monitor() {
    let (layout, pack, frames, desk) = sit_down(crate::layout::Facing::North, 0);
    let seated = frames.last().expect("a seated frame");
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme: crate::theme::theme_by_name("normal").expect("theme"),
        scale: RenderScale::new(4).expect("nonzero"),
    };
    let list = compose(
        seated,
        office,
        showing(
            crate::floor::FloorMeta::ground(),
            std::time::SystemTime::UNIX_EPOCH,
        ),
        (
            &mut crate::display::compose::LightCache::default(),
            &mut crate::outside::OutsideCache::default(),
        ),
    );
    let plate = list
        .pieces()
        .iter()
        .find(|p| matches!(p.kind, PieceKind::Text { .. }))
        .expect("the sitter has a badge")
        .span;
    let top = desk_span(&pack, Desk::North, desk, RenderScale::ONE).y0;
    assert!(plate.y1 < top, "{plate:?} reaches the monitor top at {top}");
}

/// A solid casts its shadow under its south edge; a prop's upper band does
/// not.
#[test]
fn what_meets_the_ground_casts_a_shadow() {
    let pack = test_office();
    let span = Span::new(10, 10, 8, 12, 0);
    let chair = PieceKind::Chair {
        at: crate::layout::Point { x: 10, y: 10 },
    };
    assert_eq!(
        ground_shadow(span, &chair, &pack),
        Some(crate::ground::Contact::under(10, 8, span.y1 + 1)),
        "centred under it, on the ground row under its south edge"
    );
    let (_, h) = art_size(&pack, PackPiece::MeetingSofaNorth);
    let band = |rows| PieceKind::PropBand {
        at: crate::layout::Point { x: 10, y: 10 },
        sprite: PackPiece::MeetingSofaNorth,
        rows,
    };
    assert!(
        ground_shadow(span, &band((0, NORTH_SOFA_SEAT_ROWS)), &pack).is_none(),
        "the seat band"
    );
    assert!(
        ground_shadow(span, &band((NORTH_SOFA_SEAT_ROWS, h)), &pack).is_some(),
        "the foot band"
    );
}

/// The cutaway holds to
/// [`assert_the_outside_reaches_only_the_glass`](crate::outside::tests::assert_the_outside_reaches_only_the_glass)
/// at every density the pack draws, its other pieces being what else paints.
#[test]
fn the_outside_reaches_only_the_glass() {
    use crate::floor::{FloorMeta, FloorSession};
    use crate::outside::tests::{
        UNPAINTED, assert_the_outside_reaches_only_the_glass, painted_alone,
    };
    let pack = test_office();
    let theme = &crate::theme::NORMAL;
    let office = FloorSession::new(std::sync::Arc::new(pack.clone()))
        .step(
            crate::floor::FloorInputs {
                scene: &pixtuoid_core::SceneState::uniform(16),
                now: std::time::SystemTime::UNIX_EPOCH,
                floor: FloorMeta::ground(),
                pets: crate::floor::PetInputs::default(),
            },
            crate::layout::Viewport::full(240, 144),
        )
        .expect("lays out");
    let layout = &office.layout;
    let mut scales = vec![1];
    scales.extend(pack.inner().density_variants().iter().map(|d| d.get()));
    for s in scales {
        let scale = RenderScale::new(s).expect("nonzero");
        let at = Office {
            layout,
            pack: &pack,
            theme,
            scale,
        };
        let (w, h) = (scale.to_buffer(layout.buf_w), scale.to_buffer(layout.buf_h));
        let painted = |list: &DisplayList<'_>| {
            let mut buf = RgbBuffer::filled(w, h, UNPAINTED);
            paint(list, &mut CutawayCache::default(), &mut buf);
            buf
        };
        let moment = |weathered: &crate::outside::tests::Weathered, sky| {
            Moment::resolve(sky, theme, 0.0, Motion::Full.timing(weathered.now))
        };
        let list = |shown: &Moment| {
            crate::display::compose::compose_at(
                &office.frame,
                at,
                shown,
                FloorMeta::ground(),
                quiet_board(),
                (
                    &mut crate::display::compose::LightCache::default(),
                    &mut crate::outside::OutsideCache::default(),
                ),
            )
        };
        let frames = |weathered: &crate::outside::tests::Weathered| {
            let shown = moment(weathered, weathered.sky);
            let mut bare = list(&shown);
            let plain = moment(weathered, weathered.bare);
            let mut views = Vec::new();
            push_windows(
                at,
                &plain,
                &GlassWeather::of(&plain),
                &mut views,
                &mut crate::outside::OutsideCache::default(),
            );
            for piece in bare.pieces_mut() {
                if let PieceKind::Window { .. } = piece.kind {
                    let i = views
                        .iter()
                        .position(|(span, _)| *span == piece.span)
                        .expect("one view a window");
                    piece.kind = views.swap_remove(i).1;
                    piece.fingerprint = fingerprint(&piece.kind);
                }
            }
            [painted(&list(&shown)), painted(&bare)]
        };
        // What every piece but a window paints, a glass wall aside (it
        // shows what lies behind it).
        let covered = |weathered: &crate::outside::tests::Weathered| {
            let list = list(&moment(weathered, weathered.sky));
            let mut cache = CutawayCache::default();
            painted_alone(
                (w, h),
                list.pieces().iter().filter(|piece| {
                    !matches!(piece.kind, PieceKind::Window { .. }) && !piece.kind.reads_under()
                }),
                |piece| {
                    (
                        scale.to_buffer(piece.span.x0)..scale.to_buffer(piece.span.x1 + 1),
                        scale.to_buffer(piece.span.y0)..scale.to_buffer(piece.span.y1 + 1),
                    )
                },
                |piece, over| {
                    paint_piece(
                        &piece.kind,
                        &pack,
                        list.recolours(),
                        scale,
                        &mut cache,
                        over,
                    )
                },
            )
        };
        let hung_over_glass = assert_the_outside_reaches_only_the_glass(
            &format!("cutaway at {s}x"),
            (w, h),
            |x, y| layout.glass_at(x / s, y / s),
            frames,
            covered,
        );
        assert!(
            !hung_over_glass.is_empty(),
            "at {s}x nothing hangs over a window, so nothing was compared there"
        );
    }
}

/// A window is drawn before every piece over it: first at depth 0, ahead
/// of the sign and the clock that sort there too, by push order.
#[test]
fn a_window_is_drawn_before_whatever_hangs_over_it() {
    use crate::floor::{FloorMeta, FloorSession};
    let pack = test_office();
    let office = FloorSession::new(std::sync::Arc::new(pack.clone()))
        .step(
            crate::floor::FloorInputs {
                scene: &pixtuoid_core::SceneState::uniform(16),
                now: std::time::SystemTime::UNIX_EPOCH,
                floor: FloorMeta::ground(),
                pets: crate::floor::PetInputs::default(),
            },
            crate::layout::Viewport::full(240, 144),
        )
        .expect("lays out");
    let at = Office {
        layout: &office.layout,
        pack: &pack,
        theme: &crate::theme::NORMAL,
        scale: RenderScale::from(pack.max_density_variant()),
    };
    let list = list_at(&office.frame, at, 12);
    let is_window = |p: &Piece| matches!(p.kind, PieceKind::Window { .. });
    let mut over = 0;
    for (i, window) in list
        .pieces()
        .iter()
        .enumerate()
        .filter(|(_, p)| is_window(p))
    {
        let span = window.span;
        let area = Bounds {
            x: span.x0,
            y: span.y0,
            width: span.x1 - span.x0 + 1,
            height: span.y1 - span.y0 + 1,
        };
        for (j, piece) in list.pieces().iter().enumerate() {
            if is_window(piece) || !piece.span.meets(area) {
                continue;
            }
            assert!(
                j > i,
                "{:?} drawn before the window at {span:?}",
                piece.kind
            );
            over += 1;
        }
    }
    assert!(
        over > 0,
        "nothing hangs over a window, so nothing was ordered"
    );
}

/// The glass `push_windows` queues for `layout` at `moment` under
/// `weather`, at the densest scale.
fn glass_views(layout: &SceneLayout, moment: &Moment, weather: &GlassWeather) -> Vec<WindowView> {
    let densest = test_office().max_density_variant().get();
    let scale = RenderScale::new(densest).expect("nonzero");
    glass_views_at(scale, layout, moment, weather)
}

/// [`glass_views`] at `scale`.
fn glass_views_at(
    scale: RenderScale,
    layout: &SceneLayout,
    moment: &Moment,
    weather: &GlassWeather,
) -> Vec<WindowView> {
    let pack = test_office();
    let mut order = Vec::new();
    push_windows(
        Office {
            layout,
            pack: &pack,
            theme: &crate::theme::NORMAL,
            scale,
        },
        moment,
        weather,
        &mut order,
        &mut crate::outside::OutsideCache::default(),
    );
    order
        .into_iter()
        .map(|(_, kind)| match kind {
            PieceKind::Window { view, .. } => std::sync::Arc::unwrap_or_clone(view),
            other => panic!("only windows: {other:?}"),
        })
        .collect()
}

fn moment_at(w: crate::sky::Weather, now: std::time::SystemTime) -> Moment {
    Moment::resolve(
        crate::sky::Sky::at_with(now, w),
        &crate::theme::NORMAL,
        0.0,
        Motion::Full.timing(now),
    )
}

/// Against the same sky under a clear model, a weather changes only glass
/// pixels, and every one that veils or falls changes some.
#[test]
fn the_weather_shows_on_the_glass_and_only_there() {
    use crate::sky::Weather;
    for (w, h) in [(160, 96), (240, 135)] {
        let layout =
            SceneLayout::compute(crate::layout::Viewport::full(w, h), None, 0).expect("lays out");
        for hour in [12, 0] {
            let now = crate::localclock::at_hour(hour);
            let clear = GlassWeather::of(&moment_at(Weather::Clear, now));
            for weather in Weather::ALL {
                let moment = moment_at(weather, now);
                let shown = glass_views(&layout, &moment, &GlassWeather::of(&moment));
                let crisp = glass_views(&layout, &moment, &clear);
                let mut changed = 0;
                for (a, b) in shown.iter().zip(&crisp) {
                    assert!(a.joinery().eq(b.joinery()), "{weather:?} on a frame");
                    changed += a.cells().zip(b.cells()).filter(|(a, b)| a != b).count();
                }
                let shows = weather != Weather::Clear;
                assert_eq!(changed > 0, shows, "{weather:?} at {w}x{h} {hour}h");
            }
        }
    }
}

/// The canvas repaints a window only when its fingerprint moves: over one
/// sky, the glass weather's tick moves it where something falls and
/// nowhere else, and one key always gives one fingerprint.
#[test]
fn a_window_s_fingerprint_moves_with_the_weather_tick_where_it_falls() {
    use crate::sky::Weather;
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    let at = |w, ms| {
        moment_at(
            w,
            crate::localclock::at_hour(12) + std::time::Duration::from_millis(ms),
        )
    };
    for w in Weather::ALL {
        let moment = at(w, 5_000);
        let prints = |ms| -> Vec<u64> {
            glass_views(&layout, &moment, &GlassWeather::of(&at(w, ms)))
                .into_iter()
                .map(|view| {
                    fingerprint(&PieceKind::Window {
                        view: std::sync::Arc::new(view),
                        frame: crate::theme::NORMAL.surface.window_frame,
                    })
                })
                .collect()
        };
        assert_eq!(prints(5_000), prints(5_000), "{w:?}: one key");
        assert_eq!(prints(5_000) != prints(5_600), w.falls(), "{w:?}");
    }
}

/// Pins [`Span`]'s bounds contract for every piece kind and prop builder. A
/// pixel counts as WRITTEN where two paints over different fills agree, so
/// no colour is assumed to be one the paint never uses.
#[test]
fn every_piece_paints_only_inside_its_span() {
    use crate::floor::{FloorMeta, FloorSession};
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (mut kinds, mut props) = (
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    );
    // One span and fingerprint always paint the same pixels at one scale
    // (`one_span_and_fingerprint_always_paint_the_same_pixels`), so a piece
    // checked once on one pack and canvas is checked for every frame.
    let mut checked = std::collections::HashSet::new();
    // keyed by grid and light, so one cache serves every pack and canvas
    let mut clouds = crate::outside::OutsideCache::default();
    let mut check = |pack_label: &str,
                     pack: &OfficeArt,
                     frame: &SimFrame,
                     layout: &SceneLayout,
                     only_people: bool| {
        for s in [1, 3, pack.max_density_variant().get()] {
            let scale = RenderScale::new(s).expect("nonzero");
            let office = Office {
                layout,
                pack,
                theme,
                scale,
            };
            let moment = Moment::resolve(
                crate::sky::Sky::clock(std::time::UNIX_EPOCH),
                theme,
                0.0,
                Motion::Full.timing(std::time::UNIX_EPOCH),
            );
            let list = compose_at(
                frame,
                office,
                &moment,
                crate::floor::FloorMeta::ground(),
                quiet_board(),
                (
                    &mut crate::display::compose::LightCache::default(),
                    &mut clouds,
                ),
            );
            for &Piece { span, ref kind, .. } in list.pieces() {
                if only_people
                    && !matches!(
                        kind,
                        PieceKind::Character { .. } | PieceKind::Effect(_) | PieceKind::Text { .. }
                    )
                {
                    continue;
                }
                kinds.insert(kind_name(kind));
                if let PieceKind::Prop { art, .. } | PieceKind::Animated { art, .. } = kind {
                    props.insert(art.sprite);
                }
                let canvas = (layout.buf_w, layout.buf_h);
                if !checked.insert((pack_label.to_owned(), s, canvas, span, fingerprint(kind))) {
                    continue;
                }
                let painted =
                    painted_over_two_fills(kind, layout, pack, &Recolours::of(theme), scale);
                let w = scale.to_buffer(layout.buf_w);
                assert_eq!(
                    stray_pixel_of(kind, span, w, scale, &painted),
                    None,
                    "{kind:?} at scale {s} wrote a logical pixel outside {span:?}"
                );
                // At the densities the cutaway draws at.
                if s % pack.max_density_variant().get() == 0
                    && ground_shadow(span, kind, pack).is_some()
                {
                    assert_eq!(
                        lowest_painted_row(kind, w, scale, &painted),
                        Some(span.y1),
                        "{kind:?} at scale {s} is grounded on a row it doesn't reach: {span:?}"
                    );
                }
            }
        }
    };
    // Every step of a walk to each desk facing and the sit, for the mover...
    for facing in [crate::layout::Facing::North, crate::layout::Facing::South] {
        let (layout, pack, frames, _) = sit_down(facing, 2);
        for frame in &frames {
            check("default", &pack, frame, &layout, true);
        }
        // ...the office around them once, a lit screen and a carried chair
        // included...
        check(
            "default",
            &pack,
            frames.last().expect("a seated frame"),
            &layout,
            false,
        );
    }
    // ...the mover in every style the pack draws, so a box that forgot the
    // rows a style's hair rises by shows...
    let styles: std::collections::BTreeSet<_> = test_office()
        .inner()
        .hairstyles()
        .map(|s| s.name().to_owned())
        .collect();
    let mut worn = std::collections::BTreeSet::new();
    let base = test_office();
    for i in 0..1000 {
        let pack = &base;
        let scale = RenderScale::from(pack.max_density_variant());
        let dense = crate::pack::densest_frame(pack, PackPiece::Walking, 0, scale);
        let id = pixtuoid_core::AgentId::from_transcript_path(&format!("/style/{i}.jsonl"));
        let style = crate::character::dress_for(pack, id, dense.frame, dense.head, dense.density)
            .and_then(|d| d.style);
        let Some(style) = style.filter(|s| !worn.contains(s)) else {
            continue;
        };
        worn.insert(style);
        let (layout, pack, frames, _) =
            sit_down_as(base.clone(), crate::layout::Facing::South, 0, id);
        for frame in &frames {
            check("default", &pack, frame, &layout, true);
        }
        if worn == styles {
            break;
        }
    }
    assert_eq!(worn, styles, "the walks wore every style");
    // ...every effect a figure carries, on each step of a walk and the sit...
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::South, 2);
    for mut frame in frames {
        for c in &mut frame.characters {
            c.effects = every_effect(c.top_left);
        }
        check("default", &pack, &frame, &layout, true);
    }
    // ...every desk prop, each tower tier with a sheet mid-fall, both ways
    // a desk faces...
    for facing in [crate::layout::Facing::North, crate::layout::Facing::South] {
        let (layout, pack, frames, _) = sit_down(facing, 2);
        let mut frame = frames.last().expect("a seated frame").clone();
        for (i, d) in frame.desks.iter_mut().enumerate() {
            d.cup = Some(crate::sim::Cup::Steaming);
            d.token_tier = (i % usize::from(crate::token_meter::MAX_TIER + 1)) as u8;
            d.sheet_fall = Some(1);
        }
        check("default", &pack, &frame, &layout, false);
    }
    // ...a walk whose frames differ in size, so a span sized from the wrong
    // frame shows...
    const LONG_STRIDE: &str = "\
    @frame 0
    . n H H H H n .
    n H H H H H H n
    H H S S S S H H
    H S e S S e S H
    . S S S m S S .
    . n S S S S n .
    . B B B B B B .
    B B B B B B B B
    S B B B B B B S
    . P P P P P P .
    . P P P P P P .
    . P . . . . P P
    . P . . . . . P
    ";
    let uneven = OfficeArt::parse(crate::pack::test_pack_with(&[(
        "walking_1.sprite",
        LONG_STRIDE,
    )]))
    .expect("the uneven pack parses");
    let (layout, uneven, frames, _) = sit_down_in(uneven, crate::layout::Facing::South, 0);
    for frame in &frames {
        check("uneven", &uneven, frame, &layout, true);
    }
    // ...and offices whose sizes gate in the pieces 160x96 lacks, empty.
    let pack = test_office();
    for (w, h) in [(240u16, 144u16), (100, 60)] {
        let stepped = FloorSession::new(std::sync::Arc::new(pack.clone()))
            .step(
                crate::floor::FloorInputs {
                    scene: &pixtuoid_core::SceneState::uniform(16),
                    now: std::time::SystemTime::UNIX_EPOCH,
                    floor: FloorMeta::ground(),
                    pets: crate::floor::PetInputs::default(),
                },
                crate::layout::Viewport::full(w, h),
            )
            .expect("lays out");
        check("default", &pack, &stepped.frame, &stepped.layout, false);
    }
    assert_eq!(
        kinds.into_iter().collect::<Vec<_>>(),
        [
            "animated",
            "chair",
            "character",
            "clock",
            "desk",
            "desk front",
            "desk prop",
            "door",
            "effect",
            "hung decor",
            "neon",
            "prop",
            "prop band",
            "table",
            "text",
            "wall",
            "window"
        ],
        "a piece kind went untested"
    );
    // Every fixture drawn from art must have been reached, the pantry
    // counter at both its sizes.
    for sprite in crate::layout::PANTRY_COUNTER_ANIMS.into_iter().chain([
        PackPiece::MeetingSofa,
        PackPiece::Plant,
        PackPiece::FilingCabinet,
        PackPiece::MeetingChair,
        PackPiece::CoatRack,
        PackPiece::SideTable,
        PackPiece::FloorLamp,
        PackPiece::KitchenIsland,
        PackPiece::PantryBin,
        PackPiece::FishTank,
        PackPiece::WaterCooler,
        PackPiece::VendingMachine,
        PackPiece::Printer,
    ]) {
        assert!(
            props.contains(&sprite),
            "no {} prop was painted: {props:?}",
            sprite.name()
        );
    }
}

/// A dressed figure carried to the top edge of the area inside a top inset, at
/// the pack's densest art, paints none of its hair over the inset, and the box
/// [`Hits::figures`](crate::look::Hits::figures) reports stops at its edge.
#[test]
fn a_dressed_figure_at_a_top_insets_edge_keeps_its_hair_off_it() {
    let base = test_office();
    let scale = RenderScale::from(base.max_density_variant());
    let walk = crate::pack::densest_frame(&base, PackPiece::Walking, 0, scale);
    let id = (0..1000)
        .map(|i| pixtuoid_core::AgentId::from_transcript_path(&format!("/style/{i}.jsonl")))
        .find(|&id| {
            crate::character::dress_for(&base, id, walk.frame, walk.head, walk.density)
                .is_some_and(|d| d.rise() > 0)
        })
        .expect("a style whose hair rises over the frame");
    let (_, pack, frames, _) = sit_down_as(base.clone(), crate::layout::Facing::South, 0, id);
    let insets = crate::layout::Insets {
        top: 9,
        ..Default::default()
    };
    let layout = SceneLayout::compute(
        crate::layout::inset_viewport(160, 96, insets),
        None,
        crate::floor::FloorMeta::ground().floor_seed,
    )
    .expect("lays out");
    let mut frame = frames.first().expect("a walking frame").clone();
    for c in &mut frame.characters {
        c.top_left.y = layout.inner.y;
    }
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let moment = Moment::resolve(
        crate::sky::Sky::clock(std::time::UNIX_EPOCH),
        theme,
        0.0,
        Motion::Full.timing(std::time::UNIX_EPOCH),
    );
    let list = compose_at(
        &frame,
        Office {
            layout: &layout,
            pack: &pack,
            theme,
            scale,
        },
        &moment,
        crate::floor::FloorMeta::ground(),
        quiet_board(),
        (
            &mut crate::display::compose::LightCache::default(),
            &mut crate::outside::OutsideCache::default(),
        ),
    );
    let figure = list
        .pieces()
        .iter()
        .find(|p| matches!(p.kind, PieceKind::Character { .. }))
        .expect("the figure is drawn");
    let [a, b] = painted_over_two_fills(&figure.kind, &layout, &pack, &Recolours::of(theme), scale);
    let w = usize::from(scale.to_buffer(layout.buf_w));
    let over = usize::from(scale.to_buffer(layout.inner.y)) * w;
    let written_over = a.as_slice()[..over]
        .iter()
        .zip(&b.as_slice()[..over])
        .position(|(pa, pb)| pa == pb)
        .map(|i| (i % w, i / w));
    assert_eq!(written_over, None, "a pixel painted over the inset");
    let boxes: Vec<_> = list.hovers().listed().iter().map(|h| h.at).collect();
    assert_eq!(
        boxes.iter().map(|b| b.y).min(),
        Some(layout.inner.y),
        "the boxes stop at the inset's edge: {boxes:?}"
    );
}

/// A cat and two gateways of one source, the second degraded and in hand,
/// so it sorts last on a row no feet stand on.
fn creatures() -> (SceneLayout, SimFrame) {
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    let mut frame = empty_frame(&layout);
    let (cat, lobster) = (Point { x: 40, y: 70 }, Point { x: 110, y: 70 });
    let row = |at, piece, frame| {
        let h = crate::sim::pack_frame_size(&test_office(), piece, frame).h;
        crate::layout::sort_row_at(crate::layout::Pivot::Center, at, h)
    };
    frame.pet = Some(crate::sim::PetPlacement {
        kind: crate::pet::PetKind::Cat,
        pos: cat,
        sort_row: row(cat, PackPiece::CatWalk, 1),
        flip: true,
        anim_name: PackPiece::CatWalk,
        frame_idx: 1,
        effects: crate::effects::pet_hearts(cat, 300).collect(),
    });
    frame.mascots = vec![crate::sim::MascotPlacement {
        pos: lobster,
        sort_row: row(lobster, PackPiece::LobsterWalk, 0),
        anim_name: PackPiece::LobsterWalk,
        frame_idx: 0,
        key: crate::creatures::openclaw_key("18789"),
        degraded: false,
        on_roster: true,
        effects: crate::effects::mascot_bubbles(
            lobster,
            12,
            2,
            Motion::Full.beat(std::time::UNIX_EPOCH),
        )
        .collect(),
    }];
    let sick = Point { x: 110, y: 84 };
    frame.mascots.push(crate::sim::MascotPlacement {
        pos: sick,
        sort_row: u16::MAX,
        key: crate::creatures::openclaw_key("18790"),
        degraded: true,
        effects: Vec::new(),
        ..frame.mascots[0].clone()
    });
    (layout, frame)
}

/// The pet and each gateway hover on their art as themselves, in paint
/// order.
#[test]
fn the_cutaway_lists_the_pet_and_each_mascot_as_hovers() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let (layout, frame) = creatures();
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale: RenderScale::ONE,
    };
    let list = list_at(&frame, office, 12);
    let creatures: Vec<_> = list
        .pieces()
        .iter()
        .filter_map(|p| match &p.kind {
            PieceKind::Creature { who, .. } => who.clone().map(|target| crate::display::Hover {
                at: p.span.bounds(),
                target,
            }),
            _ => None,
        })
        .collect();
    let expected: Vec<_> = std::iter::once(frame.pet.as_ref().expect("the cat").target())
        .chain(
            frame
                .mascots
                .iter()
                .filter_map(crate::sim::MascotPlacement::target),
        )
        .collect();
    assert_eq!(
        creatures
            .iter()
            .map(|h| h.target.clone())
            .collect::<Vec<_>>(),
        expected,
        "premise: the cat, then each gateway"
    );
    assert_eq!(list.hovers().listed(), creatures);

    // one walking out past the roster still paints, and hovers as nothing
    let mut frame = frame;
    frame.mascots[0].on_roster = false;
    let left = frame.mascots[0].key.clone();
    let list = list_at(&frame, office, 12);
    assert_eq!(
        list.pieces()
            .iter()
            .filter(|p| matches!(p.kind, PieceKind::Creature { .. }))
            .count(),
        creatures.len()
    );
    assert!(
        !list
            .hovers()
            .listed()
            .iter()
            .any(|h| h.target == crate::hit::HoverTarget::Mascot(left.clone()))
    );
}

/// A pet whose feet sort south of an agent's paints over them, so where
/// the two overlap it is the hover.
#[test]
fn a_pet_painted_over_an_agent_is_the_hover() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::South, 2);
    let mut frame = frames.last().expect("a seated frame").clone();
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale: RenderScale::ONE,
    };
    let body = list_at(&frame, office, 12)
        .pieces()
        .iter()
        .find_map(|p| match p.kind {
            PieceKind::Character { body, .. } => Some(body),
            _ => None,
        })
        .expect("the sitter");
    // centred a row below the sitter's, so its feet sort south of theirs
    let at = Point {
        x: u16::midpoint(body.x0, body.x1),
        y: body.y1 + 1,
    };
    let cat = crate::sim::PetPlacement {
        kind: crate::pet::PetKind::Cat,
        pos: at,
        sort_row: crate::layout::sort_row_at(
            crate::layout::Pivot::Center,
            at,
            crate::sim::pack_frame_size(&pack, PackPiece::CatWalk, 0).h,
        ),
        flip: false,
        anim_name: PackPiece::CatWalk,
        frame_idx: 0,
        effects: Vec::new(),
    };
    let target = cat.target();
    frame.pet = Some(cat);
    let cell = Bounds {
        x: at.x,
        y: body.y1,
        width: 1,
        height: 1,
    };
    assert_eq!(list_at(&frame, office, 12).hovers().at(cell), Some(&target));
}

/// What rides on a figure never covers it from the pointer: every cell of
/// an effect over its body names the figure.
#[test]
fn an_effect_never_blocks_its_figure() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::South, 2);
    let mut frame = frames.last().expect("a seated frame").clone();
    for c in &mut frame.characters {
        c.effects = every_effect(c.top_left);
    }
    let id = crate::hit::HoverTarget::Agent(frame.agents[frame.characters[0].agent_idx].agent_id);
    let mut over = 0;
    for s in [1, pack.max_density_variant().get()] {
        let office = Office {
            layout: &layout,
            pack: &pack,
            theme,
            scale: RenderScale::new(s).expect("nonzero"),
        };
        let list = list_at(&frame, office, 12);
        let body = list
            .pieces()
            .iter()
            .find_map(|p| match p.kind {
                PieceKind::Character { body, .. } => Some(body),
                _ => None,
            })
            .expect("the sitter");
        for p in list.pieces() {
            let PieceKind::Effect(riding) = p.kind else {
                continue;
            };
            let e = p.span;
            for y in e.y0.max(body.y0)..=e.y1.min(body.y1) {
                for x in e.x0.max(body.x0)..=e.x1.min(body.x1) {
                    over += 1;
                    let cell = Bounds {
                        x,
                        y,
                        width: 1,
                        height: 1,
                    };
                    assert_eq!(
                        list.hovers().at(cell),
                        Some(&id),
                        "at scale {s}, {:?} at {cell:?}",
                        riding.effect.kind
                    );
                }
            }
        }
    }
    assert!(over > 0, "premise: an effect lies over the body");
}

/// The pet and the gateway mascots stand as figures: each paints only
/// inside its span at every density, sorts on the row the sim gave it,
/// faces as the sim turns it, grounds its shadow, and has what rides on it
/// straight after it; a degraded gateway's art is greyed.
#[test]
fn creatures_stand_as_figures_with_their_riders_after_them() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let (layout, frame) = creatures();
    let lobster = frame.mascots[0].pos;
    let riders = [
        frame.pet.as_ref().map_or(0, |p| p.effects.len()),
        frame.mascots[0].effects.len(),
        0,
    ];
    let rows = [
        frame.pet.as_ref().map(|p| p.sort_row),
        Some(frame.mascots[0].sort_row),
        Some(frame.mascots[1].sort_row),
    ];
    for s in [1, pack.max_density_variant().get()] {
        let scale = RenderScale::new(s).expect("nonzero");
        let office = Office {
            layout: &layout,
            pack: &pack,
            theme,
            scale,
        };
        let list = list_at(&frame, office, 12);
        let pieces = list.pieces();
        let creatures: Vec<usize> = pieces
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p.kind, PieceKind::Creature { .. }))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(creatures.len(), 3, "at scale {s}, the pet and the mascots");
        // The pet faces west; a mascot never turns.
        let facing = [Flip::Horizontal, Flip::None, Flip::None];
        let sickly = [false, false, true];
        for ((((&i, ridden), flip), sick), row) in creatures
            .iter()
            .zip(riders)
            .zip(facing)
            .zip(sickly)
            .zip(rows)
        {
            let p = &pieces[i];
            let PieceKind::Creature {
                at, art, degraded, ..
            } = p.kind
            else {
                unreachable!("filtered to creatures");
            };
            assert_eq!(
                art.flip,
                flip,
                "at scale {s} {} faces wrong",
                art.sprite.name()
            );
            assert_eq!(
                degraded, sick,
                "at scale {s} the gateway at {at:?} misreads its state"
            );
            assert!(
                p.shadow.is_some(),
                "at scale {s} {} casts no shadow",
                art.sprite.name()
            );
            assert_eq!(
                Some(p.span.depth),
                row,
                "at scale {s} {at:?} sorts off its row"
            );
            assert_eq!(
                stray_pixel(&p.kind, p.span, &layout, &pack, theme, scale),
                None,
                "at scale {s} {:?} painted outside {:?}",
                p.kind,
                p.span
            );
            let after = pieces[i + 1..]
                .iter()
                .take_while(|q| matches!(q.kind, PieceKind::Effect(_)))
                .count();
            assert_eq!(
                after,
                ridden,
                "at scale {s} {} lost its riders",
                art.sprite.name()
            );
        }
        let lobster_kind = |degraded| PieceKind::Creature {
            at: lobster,
            art: Art::still(PackPiece::LobsterRest),
            degraded,
            who: frame.mascots[0].target(),
        };
        let recolours = Recolours::of(theme);
        assert_ne!(
            painted_alone(&lobster_kind(true), &layout, &pack, &recolours, scale),
            painted_alone(&lobster_kind(false), &layout, &pack, &recolours, scale),
            "at scale {s} a degraded gateway looks as a healthy one"
        );
    }
}

/// The carpet takes the weather's tint: the list a rainy hour builds lays
/// the theme's carpet drawn toward the rain's tint, which no clear hour's
/// ground matches.
#[test]
fn the_ground_takes_the_weathers_tint() {
    use crate::sky::{Sky, Weather};
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    let frame = empty_frame(&layout);
    let now = crate::localclock::at_hour(12);
    let ground_in = |w: Weather| {
        let sky = Sky::at_with(now, w);
        let carpet = crate::atmosphere::SkyTones::resolve(&sky, theme).carpet(theme);
        let list = compose_at(
            &frame,
            Office {
                layout: &layout,
                pack: &pack,
                theme,
                scale: RenderScale::ONE,
            },
            &Moment::resolve(sky, theme, 0.0, Motion::Full.timing(now)),
            crate::floor::FloorMeta::ground(),
            quiet_board(),
            (
                &mut crate::display::compose::LightCache::default(),
                &mut crate::outside::OutsideCache::default(),
            ),
        );
        (list.carpet(), carpet)
    };
    let (rain, model) = ground_in(Weather::Rain);
    assert_eq!(rain, model, "the list lays the model's carpet");
    assert_ne!(rain, ground_in(Weather::Clear).0);
}

/// Mid-change, the ground and the glass's veil each take the incoming
/// weather's look on the art pixels the dither gives it, keyed on the art
/// grid, and the going weather's on the rest.
#[test]
fn a_change_dithers_the_ground_and_the_veil_on_the_art_grid() {
    use crate::sky::{Sky, Weather, WeatherMix};
    use crate::sprite::Rgb;
    let theme = &crate::theme::NORMAL;
    let pack = test_office();
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    let now = crate::localclock::at_hour(12);
    // Partway through a change of cloud, at a share whose dither, unlike
    // half's checkerboard, moves with any shift of its key.
    let mix = WeatherMix::toward(Weather::Clear, Weather::Fog, 0.4);
    let moment = || {
        let sky = Sky::at_with(now, Weather::Clear).with_weather(mix);
        Moment::resolve(sky, theme, 0.0, Motion::Full.timing(now))
    };
    // Tallies a pixel the two ends draw apart: `mid` must be the incoming
    // end's where the dither `takes` it, else the going end's.
    let tally = |took: &mut [usize; 2], what: &str, takes: bool, [going, coming, mid]: [Rgb; 3]| {
        if going != coming {
            assert_eq!(mid, if takes { coming } else { going }, "{what}");
            took[usize::from(takes)] += 1;
        }
    };

    // Two buffer pixels to the art pixel, so a key off the art grid shows.
    let d = pack.max_density_variant().get();
    let pen = Pen::for_pack(RenderScale::new(2 * d).expect("nonzero"), &pack);
    let carpet = moment().look.carpet(theme);
    let [from, to] = carpet.ends();
    let lay = |paint: &dyn Fn(&mut RgbBuffer)| {
        let mut buf = RgbBuffer::filled(
            pen.buffer(pen.art(layout.buf_w)).0,
            pen.buffer(pen.art(layout.buf_h)).0,
            Rgb { r: 0, g: 0, b: 0 },
        );
        paint(&mut buf);
        buf
    };
    let [going, coming, mid] = [
        lay(&|buf| paint_carpet_tones(&Backdrop::of(&layout, theme), from, pen, buf)),
        lay(&|buf| paint_carpet_tones(&Backdrop::of(&layout, theme), to, pen, buf)),
        lay(&|buf| paint_carpet(&Backdrop::of(&layout, theme), carpet, pen, buf)),
    ];
    let mut took = [0; 2];
    for y in 0..mid.height() {
        for x in 0..mid.width() {
            let px = [&going, &coming, &mid].map(|b| b.get(x, y));
            let what = format!("ground at ({x}, {y})");
            let (ax, ay) = (
                pen.art_of_buffer(BufferPx(x)),
                pen.art_of_buffer(BufferPx(y)),
            );
            tally(&mut took, &what, carpet.takes_to(ax.0, ay.0), px);
        }
    }
    assert!(took[0] > 0 && took[1] > 0, "ground: {took:?}");

    let veil = moment().look.glass_veil;
    let [clear, fog] = veil.ends();
    let look = |veil| {
        let mut m = moment();
        m.look.glass_veil = veil;
        // One art pixel to the unit, so a glass's corner sits off the
        // dither's period and a key from it would show.
        glass_views_at(RenderScale::ONE, &layout, &m, &GlassWeather::of(&m))
    };
    let [going, coming, mid] = [Dithered::solid(clear), Dithered::solid(fog), veil].map(look);
    let rows = crate::layout::window_rows(layout.wall_band_h());
    assert!(
        layout
            .window_bays()
            .any(|b| b.glass_box(rows.clone()).x % crate::dither::PERIOD != 0),
        "every glass on the dither's phase: a key from its corner would pass"
    );
    let mut took = [0; 2];
    for ((g, c), view) in going.iter().zip(&coming).zip(&mid) {
        for (((at, g), (_, c)), (_, m)) in g.cells().zip(c.cells()).zip(view.cells()) {
            let what = format!("veil at {at:?}");
            tally(&mut took, &what, veil.takes_to(at.0, at.1), [g, c, m]);
        }
    }
    assert!(took[0] > 0 && took[1] > 0, "veil: {took:?}");
}

/// A figure's dust paints straight before them and their other riders
/// straight after, so nothing sorts between a person and what rides on
/// them.
#[test]
fn riders_paint_beside_their_figure() {
    use crate::effects::EffectKind as K;
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::South, 2);
    let scale = RenderScale::from(pack.max_density_variant());
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale,
    };
    for mut frame in frames {
        for c in &mut frame.characters {
            c.effects = every_effect(c.top_left);
        }
        let list = list_at(&frame, office, 12);
        let kinds: Vec<Option<K>> = list
            .pieces()
            .iter()
            .map(|p| match p.kind {
                PieceKind::Effect(r) => Some(r.effect.kind),
                _ => None,
            })
            .collect();
        let figure = list
            .pieces()
            .iter()
            .position(|p| matches!(p.kind, PieceKind::Character { .. }))
            .expect("the figure");
        assert_eq!(
            kinds[figure - 1..=figure + 2],
            [
                Some(K::WalkingDust),
                None,
                Some(K::SleepZ),
                Some(K::RequiresActionMark)
            ]
        );
    }
}

/// What the incremental canvas relies on: two pieces sharing a span and a
/// fingerprint paint the same pixels, so one may stand in for the other.
/// Walked over every tick of a walk to a desk and a sit, where the figure's
/// and the desk's fingerprints both change (asserted below). Every piece is
/// compared at scale 1; at the densest scale only figures, the kind that
/// changes tick to tick (the glass is walked in
/// `a_window_s_fingerprint_moves_with_the_moment_it_shows`).
#[test]
fn one_span_and_fingerprint_always_paint_the_same_pixels() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let now = std::time::SystemTime::UNIX_EPOCH;
    // Keyed by scale too: a fingerprint holds within one scale (`Piece`).
    let mut painted: std::collections::HashMap<(u16, Span, u64), u64> =
        std::collections::HashMap::new();
    let (mut repeats, mut figure_changes, mut desk_changes) = (0, 0, 0);
    for facing in [crate::layout::Facing::North, crate::layout::Facing::South] {
        let (layout, pack, frames, desk) = sit_down(facing, 2);
        for s in [1, pack.max_density_variant().get()] {
            let scale = RenderScale::new(s).expect("nonzero");
            let mut last: Option<(u64, u64)> = None;
            for frame in &frames {
                let list = compose_at(
                    frame,
                    Office {
                        layout: &layout,
                        pack: &pack,
                        theme,
                        scale,
                    },
                    &Moment::resolve(
                        crate::sky::Sky::clock(now),
                        theme,
                        0.0,
                        Motion::Full.timing(now),
                    ),
                    crate::floor::FloorMeta::ground(),
                    quiet_board(),
                    (
                        &mut crate::display::compose::LightCache::default(),
                        &mut crate::outside::OutsideCache::default(),
                    ),
                );
                repeats += same_fingerprint_same_pixels(&mut painted, &list, &layout, |p| {
                    s == 1 || matches!(p.kind, PieceKind::Character { .. })
                });
                let fp_of = |want: fn(&PieceKind, crate::layout::Point) -> bool| {
                    list.pieces()
                        .iter()
                        .find(|p| want(&p.kind, desk))
                        .map(|p| p.fingerprint)
                };
                let figure = fp_of(|k, _| matches!(k, PieceKind::Character { .. }));
                let desk_fp = fp_of(|k, d| matches!(k, PieceKind::Desk { at, .. } if *at == d));
                if let (Some((f0, d0)), Some(f1), Some(d1)) = (last, figure, desk_fp) {
                    figure_changes += usize::from(f0 != f1);
                    desk_changes += usize::from(d0 != d1);
                }
                last = figure.zip(desk_fp);
            }
        }
    }
    assert!(repeats > 0, "no piece recurred, so nothing was compared");
    assert!(figure_changes > 0, "the walker never changed fingerprint");
    assert!(
        desk_changes > 0,
        "the home desk's screen never changed fingerprint"
    );
}

/// The glass half of the property: a window keeps its span while the sky
/// and the city's lights move under it, so only the fingerprint can tell
/// two moments twelve hours apart, at the densest scale where the view is art pixels.
#[test]
fn a_window_s_fingerprint_moves_with_the_moment_it_shows() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::South, 2);
    let scale = RenderScale::from(pack.max_density_variant());
    let mut painted = std::collections::HashMap::new();
    let glass = |now: std::time::SystemTime, painted: &mut _| {
        let list = compose_at(
            &frames[0],
            Office {
                layout: &layout,
                pack: &pack,
                theme,
                scale,
            },
            &Moment::resolve(
                crate::sky::Sky::clock(now),
                theme,
                0.0,
                Motion::Full.timing(now),
            ),
            crate::floor::FloorMeta::ground(),
            quiet_board(),
            (
                &mut crate::display::compose::LightCache::default(),
                &mut crate::outside::OutsideCache::default(),
            ),
        );
        let is_window = |p: &Piece| matches!(p.kind, PieceKind::Window { .. });
        same_fingerprint_same_pixels(painted, &list, &layout, is_window);
        list.pieces()
            .iter()
            .filter(|p| is_window(p))
            .map(|p| (p.span, p.fingerprint))
            .collect::<Vec<_>>()
    };
    let noon = std::time::UNIX_EPOCH + std::time::Duration::from_secs(12 * 3600);
    let (a, b) = (
        glass(std::time::UNIX_EPOCH, &mut painted),
        glass(noon, &mut painted),
    );
    assert!(!a.is_empty(), "the office has windows");
    assert_eq!(a.len(), b.len(), "the same windows at every hour");
    for ((span_a, fp_a), (span_b, fp_b)) in a.iter().zip(&b) {
        assert_eq!(span_a, span_b, "a window never moves");
        assert_ne!(
            fp_a, fp_b,
            "a window at {span_a:?} looks the same twelve hours apart"
        );
    }
    assert_eq!(glass(noon, &mut painted), b, "one moment, one fingerprint");
}

/// `list`'s lights over a flat `under`, relit over `rect` alone by the
/// frame's own pass: every light that meets it, over an all-lit room.
fn lights_over(list: &DisplayList<'_>, layout: &SceneLayout, rect: Span) -> RgbBuffer {
    let (w, h) = (
        list.scale().to_buffer(layout.buf_w),
        list.scale().to_buffer(layout.buf_h),
    );
    let mut buf = RgbBuffer::filled(w, h, list.backdrop().tones.bg);
    let pen = Pen::for_pack(list.scale(), list.pack());
    let lights: Vec<&crate::display::light::LightView> =
        list.lights().iter().map(|l| &*l.view).collect();
    crate::cutaway::light::net_pass(
        ArtRect {
            x: pen.art(rect.x0),
            y: pen.art(rect.y0),
            w: pen.art(rect.x1 - rect.x0 + 1),
            h: pen.art(rect.y1 - rect.y0 + 1),
        },
        &lights,
        (list.ambient(), list.flash()),
        &crate::cutaway::light::Emission::new(w, h),
        pen,
        &mut crate::cutaway::light::NetMemo::default(),
        &mut buf,
    );
    buf
}

/// The light half of the fingerprint property, through the frame's own
/// pass: a light's span relit from the same lights over the same room paints
/// alike, so a canvas repainting a light's span on a change to any light
/// that meets it is complete. Over a walk at three hours, at scale 1 and
/// the densest, where some light changes its bands in place.
#[test]
fn one_set_of_lights_one_set_of_pixels() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
    let mut painted: std::collections::HashMap<(u16, Span, Vec<u64>), u64> =
        std::collections::HashMap::new();
    let mut seen: std::collections::HashMap<(u16, Span), std::collections::BTreeSet<u64>> =
        std::collections::HashMap::new();
    let mut repeats = 0;
    for s in [1, pack.max_density_variant().get()] {
        let scale = RenderScale::new(s).expect("nonzero");
        for hour in [18, 20, 23] {
            for frame in frames.iter().step_by(4) {
                let list = list_at(
                    frame,
                    Office {
                        layout: &layout,
                        pack: &pack,
                        theme,
                        scale,
                    },
                    hour,
                );
                for light in list.lights() {
                    seen.entry((s, light.span))
                        .or_default()
                        .insert(light.fingerprint);
                    let meeting: Vec<u64> = list
                        .lights()
                        .iter()
                        .filter(|l| {
                            l.span.x0 <= light.span.x1
                                && light.span.x0 <= l.span.x1
                                && l.span.y0 <= light.span.y1
                                && light.span.y0 <= l.span.y1
                        })
                        .map(|l| l.fingerprint)
                        .collect();
                    let pixels = {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::hash::DefaultHasher::new();
                        lights_over(&list, &layout, light.span)
                            .as_slice()
                            .hash(&mut h);
                        (list.ambient(), h.finish())
                    };
                    match painted.entry((s, light.span, meeting)) {
                        std::collections::hash_map::Entry::Occupied(e) => {
                            assert_eq!(
                                *e.get(),
                                pixels.1,
                                "one set of lights at {:?} painted two ways",
                                light.span
                            );
                            repeats += 1;
                        }
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert(pixels.1);
                        }
                    }
                }
            }
        }
    }
    assert!(repeats > 0, "no light recurred, so nothing was compared");
    assert!(
        seen.values().any(|fps| fps.len() > 1),
        "no light changed its bands in place"
    );
}

/// A light alone, through the frame's own pass over the whole room under a
/// sky that darkens nothing, changes no pixel outside its span, at every
/// scale.
#[test]
fn a_light_paints_only_inside_its_span() {
    use crate::cutaway::light::{Emission, NetMemo, net_pass};
    use crate::display::light::Ambient;
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
    let frame = frames.last().expect("a seated frame");
    let (mut lights, mut lit) = (0, 0);
    for s in [1, 3, pack.max_density_variant().get()] {
        let scale = RenderScale::new(s).expect("nonzero");
        let office = Office {
            layout: &layout,
            pack: &pack,
            theme,
            scale,
        };
        let list = list_at(frame, office, 23);
        let pen = Pen::for_pack(scale, &pack);
        let (w, h) = (scale.to_buffer(layout.buf_w), scale.to_buffer(layout.buf_h));
        let whole = ArtRect {
            x: ArtPx(0),
            y: ArtPx(0),
            w: pen.art(layout.buf_w),
            h: pen.art(layout.buf_h),
        };
        for light in list.lights() {
            lights += 1;
            let mut buf = RgbBuffer::filled(w, h, theme.surface.bg_fallback);
            net_pass(
                whole,
                &[&light.view],
                (Ambient::default(), crate::display::light::Flash::default()),
                &Emission::new(w, h),
                pen,
                &mut NetMemo::default(),
                &mut buf,
            );
            lit += buf
                .as_slice()
                .iter()
                .filter(|&&c| c != theme.surface.bg_fallback)
                .count();
            let stray = buf.as_slice().iter().enumerate().find(|&(i, &c)| {
                let (x, y) = (
                    scale.logical((i % usize::from(w)) as u16),
                    scale.logical((i / usize::from(w)) as u16),
                );
                c != theme.surface.bg_fallback
                    && !((light.span.x0..=light.span.x1).contains(&x)
                        && (light.span.y0..=light.span.y1).contains(&y))
            });
            assert_eq!(
                stray.map(|(i, _)| i),
                None,
                "a light at scale {s} wrote outside {:?}",
                light.span
            );
        }
    }
    assert!(lights > 0, "the night office has no lights");
    assert!(lit > 0, "no light lifted a pixel, so this pins nothing");
}

/// The cutaway's sign glows only where the sign is drawn, and its lamps
/// light the night room.
#[test]
fn the_cutaway_glows_only_around_its_sign() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
    let frame = frames.last().expect("a seated frame");
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale: RenderScale::from(pack.max_density_variant()),
    };
    let (mut lamps, mut glows) = (0, 0);
    for hour in [12, 18, 23] {
        let list = list_at(frame, office, hour);
        let signs = list
            .pieces()
            .iter()
            .filter(|p| matches!(p.kind, PieceKind::Neon { .. }))
            .count();
        for light in list.lights() {
            lamps += usize::from(light.view.is(crate::lighting::EmitterKind::DeskLamp));
            let glow = light.view.is(crate::lighting::EmitterKind::NeonGlow);
            assert!(!glow || signs == 1, "{hour}:00 glows with no sign");
            glows += usize::from(glow);
        }
    }
    assert!(glows > 0, "the sign never glows");
    assert!(
        lamps > 0,
        "nothing lights the night room, so this pins nothing"
    );
}

#[test]
fn the_neon_glow_takes_its_tubes_hue() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(160, 96), None, 0).expect("lays out");
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale: RenderScale::from(pack.max_density_variant()),
    };
    for neon in [
        crate::floor::NeonLevels::CALM,
        crate::floor::NeonLevels::ALERT,
        crate::floor::NeonLevels {
            alert: 0.5,
            power: 1.0,
        },
    ] {
        let frame = SimFrame {
            neon,
            ..empty_frame(&layout)
        };
        let list = list_at(&frame, office, 23);
        let hue = list.pieces().iter().find_map(|p| match p.kind {
            PieceKind::Neon { hue, .. } => Some(hue),
            _ => None,
        });
        let glow = list
            .lights()
            .iter()
            .find(|l| l.view.is(crate::lighting::EmitterKind::NeonGlow))
            .map(|l| l.view.tint());
        assert_eq!(glow, Some(hue), "{neon:?}");
    }
}

/// Every desk's lamp pools where its art hangs the bulb, whichever way the
/// desk faces and at every density its art is drawn at: the cutaway's art
/// stands it on the side the desk faces.
#[test]
fn a_desk_lamp_pools_under_its_painted_bulb() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let pack = test_office();
    let layout =
        SceneLayout::compute(crate::layout::Viewport::full(240, 144), None, 0).expect("lays out");
    for s in [1, pack.max_density_variant().get()] {
        let scale = RenderScale::new(s).expect("nonzero");
        let pen = Pen::for_pack(scale, &pack);
        let frame = empty_frame(&layout);
        let list = list_at(
            &frame,
            Office {
                layout: &layout,
                pack: &pack,
                theme,
                scale,
            },
            23,
        );
        // Each painted bulb's middle, in art pixels.
        let bulbs: Vec<(f32, f32)> = layout
            .home_desks
            .iter()
            .filter_map(|&crate::layout::HomeDesk { at, facing }| {
                let which = Desk::facing(facing);
                let span = desk_span(&pack, which, at, scale);
                let desk = crate::pack::densest_frame(&pack, which.piece(), 0, scale);
                let cells = drawn_in(&desk, &[crate::pack::DESK_BULB_KEY]);
                let w = usize::from(desk.frame.width());
                let hits: Vec<(f32, f32)> = cells
                    .iter()
                    .enumerate()
                    .filter(|&(_, &b)| b)
                    .map(|(i, _)| ((i % w) as f32, (i / w) as f32))
                    .collect();
                // Art pixels of this art, on the pen's grid.
                let k = f32::from(pen.density().get()) / f32::from(desk.density.get());
                (!hits.is_empty()).then(|| {
                    let n = hits.len() as f32;
                    (
                        f32::from(pen.art(span.x0).0)
                            + hits.iter().map(|h| h.0).sum::<f32>() / n * k,
                        f32::from(pen.art(span.y0).0)
                            + hits.iter().map(|h| h.1).sum::<f32>() / n * k,
                    )
                })
            })
            .collect();
        assert!(!bulbs.is_empty(), "the pack's desks draw no bulb");
        let lamps: Vec<(f32, f32)> = list
            .lights()
            .iter()
            .filter(|l| l.view.is(crate::lighting::EmitterKind::DeskLamp))
            .map(|l| l.view.peak())
            .collect();
        assert_eq!(lamps.len(), bulbs.len(), "one pool a bulb");
        let near = f32::from(pen.density().get());
        for bulb in &bulbs {
            assert!(
                lamps
                    .iter()
                    .any(|p| (p.0 - bulb.0).abs() <= near && (p.1 - bulb.1).abs() <= near),
                "no lamp pools within {near} art px of the bulb at {bulb:?}: {lamps:?}"
            );
        }
    }
}

#[test]
fn a_piece_takes_the_pixels_it_paints_in_the_colour_already_there() {
    use crate::cutaway::light::Glow;
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, desk) = sit_down(crate::layout::Facing::North, 2);
    let frame = frames.last().expect("a seated frame");
    let scale = RenderScale::from(pack.max_density_variant());
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale,
    };
    let mut list = list_at(frame, office, 12);
    let (painted, _) = by_day(&list, &layout);
    let pen = Pen::for_pack(scale, &pack);
    let at = |(x, y): (u16, u16)| (pen.buffer(ArtPx(x)).0, pen.buffer(ArtPx(y)).0);
    let bay = crate::layout::WindowBay {
        x: desk.x,
        w: 4,
        idx: 0,
    };
    let view = WindowView::new(bay, desk.y..desk.y + 4, pen.density(), |cell| {
        let (x, y) = at(cell.at);
        painted.get(x, y)
    });
    let ((bx, by), _) = view
        .cells()
        .map(|(cell, c)| (at(cell), c))
        .next()
        .expect("the pane has glass");
    list.pieces_mut().push(Piece {
        span: Span::new(desk.x, desk.y, 4, 4, 0),
        kind: PieceKind::Window {
            view: std::sync::Arc::new(view),
            frame: theme.surface.window_frame,
        },
        shadow: None,
        fingerprint: 0,
    });
    let (repainted, glow) = by_day(&list, &layout);
    assert_eq!(
        repainted.get(bx, by),
        painted.get(bx, by),
        "the pane repaints the desk's colour"
    );
    assert_eq!(glow.get(bx, by), Glow::Pane);
}

/// The figure half of the property, where it is hardest: a sitter keeps
/// their span while each thing their figure is painted from changes, so
/// only the fingerprint can tell the frames apart. Every variant is painted
/// at scale 1 and the densest, and any two sharing a span and fingerprint
/// must paint alike.
#[test]
fn a_sitters_fingerprint_moves_with_everything_their_figure_paints_from() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let now = std::time::SystemTime::UNIX_EPOCH;
    let mut painted = std::collections::HashMap::new();
    let (mut figures, mut built) = (std::collections::HashSet::new(), 0);
    for facing in [crate::layout::Facing::North, crate::layout::Facing::South] {
        let (layout, pack, frames, _) = sit_down(facing, 2);
        let seated = frames
            .iter()
            .rev()
            .find(|f| f.characters.first().is_some_and(|c| c.seat_desk.is_some()))
            .expect("the fixture sits down");
        let other = pixtuoid_core::AgentId::from_transcript_path("/cutaway/other.jsonl");
        let variants: [&FigureEdit; 6] = [
            &|_, _| {},
            &|c, a| {
                c.glow = crate::sim::CharacterGlow::Work;
                a.state = pixtuoid_core::state::ActivityState::Running {
                    item: Some(pixtuoid_core::state::ToolUse {
                        tool_use_id: None,
                        detail: None,
                        kind: pixtuoid_core::state::ToolKind::Read,
                    }),
                };
            },
            &|_, a| a.cwd = std::sync::Arc::from(std::path::Path::new("/w/other")),
            &|c, _| c.flip_x = !c.flip_x,
            &|c, _| {
                c.anim_name = PackPiece::SeatedSleeping;
                c.frame_idx = 0;
            },
            &move |_, a| a.agent_id = other,
        ];
        // A set: without the density art the densest scale is 1.
        let scales = std::collections::BTreeSet::from([1, pack.max_density_variant().get()]);
        for s in scales {
            let scale = RenderScale::new(s).expect("nonzero");
            for edit in variants {
                built += 1;
                let frame = varied(seated, edit);
                let list = compose_at(
                    &frame,
                    Office {
                        layout: &layout,
                        pack: &pack,
                        theme,
                        scale,
                    },
                    &Moment::resolve(
                        crate::sky::Sky::clock(now),
                        theme,
                        0.0,
                        Motion::Full.timing(now),
                    ),
                    crate::floor::FloorMeta::ground(),
                    quiet_board(),
                    (
                        &mut crate::display::compose::LightCache::default(),
                        &mut crate::outside::OutsideCache::default(),
                    ),
                );
                same_fingerprint_same_pixels(&mut painted, &list, &layout, |p| {
                    matches!(p.kind, PieceKind::Character { .. })
                });
                for p in list.pieces() {
                    if matches!(p.kind, PieceKind::Character { .. }) {
                        figures.insert((s, p.span, p.fingerprint));
                    }
                }
            }
        }
    }
    // One figure per facing, scale and variant, no two alike: had a variant
    // left the fingerprint unmoved, it would have been compared above.
    assert_eq!(
        figures.len(),
        built,
        "a variant did not move the fingerprint"
    );
}

/// An edit to one figure and its agent.
type FigureEdit = dyn Fn(&mut crate::sim::CharacterPlacement, &mut pixtuoid_core::AgentSlot);

/// `frame` with `edit` applied to its first figure and that figure's agent.
fn varied(frame: &SimFrame, edit: &FigureEdit) -> SimFrame {
    let mut varied = frame.clone();
    if let Some(c) = varied.characters.first_mut() {
        edit(c, &mut varied.agents[c.agent_idx]);
    }
    varied
}

/// Building one frame twice gives the same list, so a caller diffing two
/// frames sees only what moved.
#[test]
fn one_frame_builds_one_list() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 0);
    let frame = frames.last().expect("a seated frame");
    let now = std::time::SystemTime::UNIX_EPOCH;
    let summary = |list: &DisplayList| {
        list.pieces()
            .iter()
            .map(|p| (p.span, p.fingerprint))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        summary(&compose_at(
            frame,
            Office {
                layout: &layout,
                pack: &pack,
                theme,
                scale: RenderScale::ONE
            },
            &Moment::resolve(
                crate::sky::Sky::clock(now),
                theme,
                0.0,
                Motion::Full.timing(now)
            ),
            crate::floor::FloorMeta::ground(),
            quiet_board(),
            (
                &mut crate::display::compose::LightCache::default(),
                &mut crate::outside::OutsideCache::default()
            )
        )),
        summary(&compose_at(
            frame,
            Office {
                layout: &layout,
                pack: &pack,
                theme,
                scale: RenderScale::ONE
            },
            &Moment::resolve(
                crate::sky::Sky::clock(now),
                theme,
                0.0,
                Motion::Full.timing(now)
            ),
            crate::floor::FloorMeta::ground(),
            quiet_board(),
            (
                &mut crate::display::compose::LightCache::default(),
                &mut crate::outside::OutsideCache::default()
            )
        )),
    );
}

/// Each drawn agent has one hover box, inside the span of a piece that
/// paints, in draw order, and every badge belongs to one of them.
#[test]
fn each_drawn_agent_hovers_inside_its_piece() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
    for frame in &frames {
        let list = compose_at(
            frame,
            Office {
                layout: &layout,
                pack: &pack,
                theme,
                scale: RenderScale::ONE,
            },
            &Moment::resolve(
                crate::sky::Sky::clock(std::time::SystemTime::UNIX_EPOCH),
                theme,
                0.0,
                Motion::Full.timing(std::time::SystemTime::UNIX_EPOCH),
            ),
            crate::floor::FloorMeta::ground(),
            quiet_board(),
            (
                &mut crate::display::compose::LightCache::default(),
                &mut crate::outside::OutsideCache::default(),
            ),
        );
        let pieces: Vec<&Piece> = list
            .pieces()
            .iter()
            .filter(|p| matches!(p.kind, PieceKind::Character { .. }))
            .collect();
        let hovers: Vec<(pixtuoid_core::AgentId, Span)> = list
            .hovers()
            .listed()
            .iter()
            .map(|h| match h.target {
                crate::hit::HoverTarget::Agent(id) => {
                    let Bounds {
                        x,
                        y,
                        width,
                        height,
                    } = h.at;
                    (id, Span::new(x, y, width, height, 0))
                }
                ref other => panic!("{other:?} in an office of agents"),
            })
            .collect();
        assert_eq!(hovers.len(), pieces.len());
        assert_eq!(hovers.len(), frame.characters.len());
        for ((_, body), piece) in hovers.iter().zip(&pieces) {
            let span = piece.span;
            assert!(
                span.x0 <= body.x0
                    && body.x1 <= span.x1
                    && span.y0 <= body.y0
                    && body.y1 <= span.y1,
                "hover box {body:?} leaves its piece's span {span:?}"
            );
            let [a, b] = painted_over_two_fills(
                &piece.kind,
                &layout,
                &pack,
                &Recolours::of(theme),
                RenderScale::ONE,
            );
            assert!(
                a.as_slice()
                    .iter()
                    .zip(b.as_slice())
                    .any(|(pa, pb)| pa == pb),
                "hover box {body:?} belongs to a piece that paints nothing"
            );
        }
        // Each drawn agent's badge, known by whose it is.
        let badged: Vec<_> = list.badge_runs().map(|run| run.role).collect();
        let drawn: std::collections::HashSet<_> = hovers
            .iter()
            .map(|&(id, _)| crate::display::TextRole::Badge(id))
            .collect();
        assert_eq!(badged.len(), drawn.len());
        assert_eq!(
            badged.into_iter().collect::<std::collections::HashSet<_>>(),
            drawn
        );
    }
}

/// One of each effect a figure carries, riding on its `top_left`, each at a
/// step it shows at.
fn every_effect(top_left: crate::layout::Point) -> Vec<crate::effects::Effect> {
    let at = |ms| std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms);
    vec![
        crate::effects::walking_dust(top_left, 0),
        crate::effects::sleep_z(top_left, 0, Motion::Full.beat(at(500)))
            .expect("a z rising at 500 ms"),
        crate::effects::requires_action_mark(top_left),
    ]
}

/// THE property the whole mixed-density contract rests on: a density variant
/// changes how a piece is DRAWN, never how big it is. `densest_frame`'s
/// variant and base arms return different (frame, factor) pairs whose
/// PRODUCT has to agree with the logical size the desk's foot (its face) is
/// placed by, and getting it wrong is silent — the desk
/// still renders, with its foot a whole desk below the surface.
#[test]
fn the_drawn_size_is_the_same_whichever_density_the_art_came_from() {
    let pack = test_office();
    let (bw, bh) = base_size(&pack, PackPiece::Desk);
    for s in 1..=12u16 {
        let scale = RenderScale::new(s).expect("nonzero");
        let d = crate::pack::densest_frame(&pack, PackPiece::Desk, 0, scale);
        let drawn = (
            d.frame.width() * d.blit_at.get(),
            d.frame.height() * d.blit_at.get(),
        );
        assert_eq!(
            drawn,
            (scale.to_buffer(bw), scale.to_buffer(bh)),
            "scale {s} drew a different size than the base art implies"
        );
        assert_eq!(
            drawn,
            (scale.to_buffer(d.logical.0), scale.to_buffer(d.logical.1)),
            "scale {s} drew a different size than the foot is placed by"
        );
    }
}

/// `frame`'s list at `now`, under a clear sky.
fn list_now<'a>(
    frame: &SimFrame,
    office: Office<'a>,
    now: std::time::SystemTime,
) -> DisplayList<'a> {
    let sky = crate::sky::Sky::at_with(now, crate::sky::Weather::Clear);
    compose_at(
        frame,
        office,
        &Moment::resolve(sky, office.theme, 0.0, Motion::Full.timing(now)),
        crate::floor::FloorMeta::ground(),
        quiet_board(),
        (
            &mut crate::display::compose::LightCache::default(),
            &mut crate::outside::OutsideCache::default(),
        ),
    )
}

/// The split row is the art's at every density: the back-view sofa's
/// backrest starts on its lit ridge, just under the seam where the seat
/// meets it, so the seam paints under the sitter and the ridge over them.
#[test]
fn the_north_sofas_backrest_starts_on_its_lit_ridge() {
    let pack = test_office();
    let densities = std::iter::once(crate::sprite::format::Density::ONE)
        .chain(pack.inner().density_variants().iter().copied());
    for d in densities {
        let name = format!("{}@{d}x", PackPiece::MeetingSofaNorth.name());
        let art = if d == crate::sprite::format::Density::ONE {
            Some(pack.piece(PackPiece::MeetingSofaNorth))
        } else {
            pack.inner()
                .variants_of(PackPiece::MeetingSofaNorth)
                .get(&d)
        };
        let Some(f) = art.map(|a| a.first()) else {
            continue;
        };
        let (x, split) = (f.width() / 2, NORTH_SOFA_SEAT_ROWS * d.get());
        let luma = |y: u16| {
            let c = f
                .get(x, y)
                .copied()
                .flatten()
                .expect("the sofa is opaque at its centre");
            c.lightness()
        };
        assert!(
            luma(split) > luma(split - 1),
            "{name}: row {split} must be the ridge, lit over the seam above it"
        );
    }
}

/// A sitter in front of a glowing screen takes the room's light over it: the
/// last piece to paint a pixel says how it glows.
#[test]
fn a_sitter_over_a_screen_takes_the_rooms_light() {
    use crate::cutaway::light::Glow;
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
    let frame = frames.last().expect("a seated frame");
    let scale = RenderScale::from(pack.max_density_variant());
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme,
        scale,
    };
    let mut list = list_at(frame, office, 23);
    let (with, glow) = by_day(&list, &layout);
    list.pieces_mut()
        .retain(|p| !matches!(p.kind, PieceKind::Character { .. }));
    let (without, glow_without) = by_day(&list, &layout);
    let mut over = 0;
    for y in 0..with.height() {
        for x in 0..with.width() {
            if with.get(x, y) == without.get(x, y) {
                continue;
            }
            assert_eq!(glow.get(x, y), Glow::Lit, "({x}, {y}) of a sitter glows");
            over += usize::from(glow_without.get(x, y) != Glow::Lit);
        }
    }
    assert!(
        over > 0,
        "no sitter covers a screen, so nothing was compared"
    );
}

/// A full office on `viewport`: every desk seated at work, its room lit to
/// the full.
fn full_office(viewport: crate::layout::Viewport) -> (SceneLayout, SimFrame) {
    let layout = SceneLayout::compute(viewport, None, 0).expect("lays out");
    let mut frame = empty_frame(&layout);
    let running = pixtuoid_core::state::ActivityState::Running {
        item: Some(pixtuoid_core::state::ToolUse {
            tool_use_id: None,
            detail: None,
            kind: pixtuoid_core::state::ToolKind::Edit,
        }),
    };
    frame.agents = (0..layout.home_desks.len())
        .map(|i| {
            let id = pixtuoid_core::AgentId::from_transcript_path(&format!("/lit/{i}.jsonl"));
            pixtuoid_core::AgentSlot {
                desk_index: pixtuoid_core::state::GlobalDeskIndex(i),
                ..crate::character::test_support::make_slot(id, running.clone())
            }
        })
        .collect();
    frame.seated_agents = (0..layout.home_desks.len())
        .map(|i| (pixtuoid_core::state::FloorLocalDeskIndex(i), true))
        .collect();
    frame.neon = crate::floor::NeonLevels::RUNNING;
    (layout, frame)
}

/// `list` painted whole on a fresh canvas.
fn painted(list: &DisplayList<'_>, size: (u16, u16)) -> crate::sprite::RgbBuffer {
    let mut buf =
        crate::sprite::RgbBuffer::filled(size.0, size.1, crate::sprite::Rgb { r: 0, g: 0, b: 0 });
    paint(list, &mut CutawayCache::default(), &mut buf);
    buf
}

/// Light carries across an inset's edge with no seam: under every inset of the
/// sweep, wherever a light of a full office lifts the margin past the area
/// inside the insets, the frame lit by it differs there from the frame without
/// it — every light by night, and the sun's spills slanting low. No desk lamp,
/// monitor or floor lamp stands near enough an edge to reach past it.
#[test]
fn every_light_crossing_an_inset_edge_lifts_the_margin() {
    use crate::lighting::EmitterKind;
    let pack = test_office();
    let mut crossed: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for insets in crate::layout::SWEEP_INSETS {
        let viewport = crate::layout::inset_viewport(192, 160, insets);
        let (layout, frame) = full_office(viewport);
        let inner = layout.inner;
        let outside = |(x, y): (u16, u16)| {
            x < inner.x || y < inner.y || x >= inner.right() || y >= inner.bottom()
        };
        for hour in [0, 8, 17] {
            let floor = crate::floor::FloorMeta::ground().with_weather(
                crate::sky::WeatherPolicy::Forced(crate::sky::Weather::Clear),
            );
            let office = Office {
                layout: &layout,
                pack: &pack,
                theme: &crate::theme::NORMAL,
                scale: RenderScale::ONE,
            };
            let mut list = compose(
                &frame,
                office,
                showing(floor, crate::localclock::at_hour(hour)),
                (
                    &mut crate::display::compose::LightCache::default(),
                    &mut crate::outside::OutsideCache::default(),
                ),
            );
            let size = (layout.buf_w, layout.buf_h);
            let lit = painted(&list, size);
            for k in 0..list.lights().len() {
                // One logical unit an art pixel at `RenderScale::ONE`.
                let margin: Vec<(u16, u16)> = list.lights()[k]
                    .view
                    .lit()
                    .filter(|&p| outside(p))
                    .collect();
                if margin.is_empty() {
                    continue;
                }
                let kind = [
                    (EmitterKind::NeonGlow, "neon"),
                    (EmitterKind::WindowSpill, "spill"),
                    (EmitterKind::FloorLamp, "floor lamp"),
                    (EmitterKind::DeskLamp, "desk lamp"),
                    (
                        EmitterKind::MonitorHalo(crate::lighting::ScreenWork::Turn),
                        "monitor halo",
                    ),
                ]
                .into_iter()
                .find_map(|(kind, name)| list.lights()[k].view.is(kind).then_some(name))
                .expect("every light has a kind");
                crossed.insert(kind);
                let light = list.lights_mut().remove(k);
                let unlit = painted(&list, size);
                list.lights_mut().insert(k, light);
                assert!(
                    margin
                        .iter()
                        .any(|&(x, y)| lit.get(x, y) != unlit.get(x, y)),
                    "{insets:?} at {hour:02}h: the {kind} lifts nothing past {inner:?}"
                );
            }
        }
    }
    assert_eq!(
        crossed.into_iter().collect::<Vec<_>>(),
        ["neon", "spill"],
        "the lights that reach past an inset's edge, each tested"
    );
}

/// The dark carries past an inset's edge with no seam: at midnight, every
/// light off and the floor bare, a margin pixel is the dark of its daylight
/// tone exactly as one inside is.
#[test]
fn the_cutaways_dark_carries_past_the_insets() {
    let pack = test_office();
    let mut judged = 0;
    for insets in crate::layout::SWEEP_INSETS {
        let viewport = crate::layout::inset_viewport(192, 160, insets);
        let layout = SceneLayout::compute(viewport, None, 0).expect("lays out");
        let floor = crate::floor::FloorMeta::ground().with_weather(
            crate::sky::WeatherPolicy::Forced(crate::sky::Weather::Clear),
        );
        let office = Office {
            layout: &layout,
            pack: &pack,
            theme: &crate::theme::NORMAL,
            scale: RenderScale::ONE,
        };
        let mut list = compose(
            &empty_frame(&layout),
            office,
            showing(floor, crate::localclock::at_hour(0)),
            (
                &mut crate::display::compose::LightCache::default(),
                &mut crate::outside::OutsideCache::default(),
            ),
        );
        assert!(list.ambient().steps() > 0, "midnight darkens the room");
        list.lights_mut().clear();
        list.pieces_mut().clear();
        list.backdrop_mut().coverings.clear();
        let size = (layout.buf_w, layout.buf_h);
        let night = painted(&list, size);
        *list.ambient_mut() = crate::display::light::Ambient::default();
        let day = painted(&list, size);
        judged += crate::display::light::tests::assert_the_dark_carries_past_the_insets(
            &layout,
            (&day, &night),
            &format!("{insets:?}"),
        );
    }
    assert!(judged > 0, "no margin pixel lies below the window wall");
}
