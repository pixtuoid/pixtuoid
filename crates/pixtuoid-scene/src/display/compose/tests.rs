use super::*;
use crate::anim::Motion;
use crate::character::test_support::make_slot;
use crate::layout::Station;
use crate::pack::test_office;

/// The wall board of an empty office, which no clock moves: for frames
/// whose board a test does not read.
pub(crate) fn quiet_board() -> &'static crate::neon_sign::BoardModel {
    static BOARD: std::sync::LazyLock<crate::neon_sign::BoardModel> =
        std::sync::LazyLock::new(|| {
            crate::neon_sign::build_board(
                crate::tally::StateCounts::default(),
                0,
                None,
                None,
                crate::anim::Motion::Full,
                std::time::UNIX_EPOCH,
            )
        });
    &BOARD
}

/// `floor` at `now`, under the [`quiet_board`].
pub(crate) fn showing(
    floor: crate::floor::FloorMeta,
    now: std::time::SystemTime,
) -> Showing<'static> {
    Showing {
        floor,
        now,
        board: quiet_board(),
        dim: crate::look::Dim::NONE,
    }
}

/// A piece's base row — the ordering key — through the SAME `piece_span`
/// the display list builds with. Width does not affect the base row, so the
/// call sites stay focused on depth.
fn sort_row(pivot: crate::layout::Pivot, pos: crate::layout::Point, h: u16, below: u16) -> u16 {
    piece_span(pivot, pos, 1, h, below).depth
}

/// The depth sort reserves no face under a density variant, which draws its
/// own front, so someone on the first row south of the art stands in FRONT
/// of the desk, not behind a face that is never drawn.
#[test]
#[cfg(feature = "cutaway-assets")]
fn someone_just_south_of_a_variant_desk_sorts_in_front_of_it() {
    let pack = test_office();
    let scale = RenderScale::from(pack.max_density_variant());
    let desk = crate::layout::Point { x: 0, y: 20 };
    let art = PackPiece::Desk;
    let desk_box = desk_span(&pack, Desk::South, desk, scale);
    let (_, art_h) = art_size(&pack, art);
    // The first row south of the ART, measured from its placement.
    let feet = crate::pack::desk_art_top(&pack, desk.y, art_h) + art_h;
    let (w, h) = base_size(&pack, PackPiece::Standing);
    let person = piece_span(
        crate::layout::Pivot::TopLeft,
        crate::layout::Point {
            x: desk.x,
            y: feet + 1 - h,
        },
        w,
        h,
        0,
    );
    assert_eq!(
        depth_sort(vec![(person, "person"), (desk_box, "desk")]),
        ["desk", "person"],
        "the person just south of the art must paint over the desk"
    );
}

/// A back-turned sitter's depth box, built the way `push_characters` builds
/// it, at the sort row the sim seats an occupant at (their seat's walk anchor).
fn seated_back_span(pack: &OfficeArt, desk: crate::layout::Point) -> Span {
    use crate::layout::Facing;
    let (w, h) = base_size(pack, PackPiece::SeatedBack);
    let chair = chair_span(pack, Facing::North, desk).map(|(s, _)| s);
    occupant_span(
        piece_span(crate::layout::Pivot::TopLeft, near_seat(desk), w, h, 0),
        crate::layout::desk_walk_anchor_facing(desk, Facing::North).y,
        chair,
    )
}

/// The other half: someone on the FAR side is occluded BY the desk, which is
/// what gives the office depth rather than a flat plan.
#[test]
fn a_character_north_of_the_desk_sorts_behind_it() {
    let pack = test_office();
    let desk = crate::layout::Point { x: 0, y: 20 };
    let (_, body_h) = base_size(&pack, PackPiece::Standing);

    let desk_z = desk_span(&pack, Desk::South, desk, RenderScale::ONE).depth;
    // Standing at the desk's north approach, feet on the row just north of
    // its anchor.
    let behind_z = sort_row(
        crate::layout::Pivot::TopLeft,
        crate::layout::Point {
            x: desk.x,
            y: desk.y - body_h,
        },
        body_h,
        0,
    );
    assert!(behind_z < desk_z, "desk {desk_z}, walker {behind_z}");
}

/// A centre-anchored prop standing in the aisle SOUTH of a desk must paint
/// in front of that desk and behind its occupant. Keyed on its own middle
/// row, a tall plant between the two would paint over the occupant while
/// standing behind them.
#[test]
fn an_aisle_prop_sorts_between_the_desk_and_its_occupant() {
    let pack = test_office();
    let desk = crate::layout::Point { x: 0, y: 10 };
    let desk_box = desk_span(&pack, Desk::North, desk, RenderScale::ONE);
    let seated = seated_back_span(&pack, desk);
    let (plant_w, plant_h) = base_size(&pack, PackPiece::Plant);
    // A plant whose BASE sits just south of the desk's front face.
    let plant_base = desk_box.depth + 1;
    let plant_centre = crate::layout::Point {
        x: plant_w / 2,
        y: plant_base + plant_h / 2 - plant_h + 1,
    };
    let plant = piece_span(
        crate::layout::Pivot::Center,
        plant_centre,
        plant_w,
        plant_h,
        0,
    );
    assert!(
        desk_box.depth < plant.depth && plant.depth < seated.depth,
        "the fixture must separate all three by depth, or a tie-break decides: \
         desk {desk_box:?}, plant {plant:?}, seated {seated:?}"
    );
    // Pushed in REVERSE, so no tie-break by push order can produce the answer.
    let drawn = crate::display::depth_sort(vec![
        (seated, "seated"),
        (plant, "plant"),
        (desk_box, "desk"),
    ]);
    assert_eq!(drawn, vec!["desk", "plant", "seated"]);
}

/// A back-turned sitter and their chair are ONE piece, so nothing can sort
/// between them: the occupied desk pushes no chair of its own, and the
/// sitter's box covers the chair's at either phase of the breathing bob.
#[test]
fn a_back_turned_sitter_carries_their_own_chair() {
    let pack = test_office();
    let layout = SceneLayout::compute_with_seed(160, 96, None, 0).expect("lays out");
    let north: Vec<crate::layout::Point> = layout
        .home_desks
        .iter()
        .filter(|d| d.facing == crate::layout::Facing::North)
        .map(|d| d.at)
        .collect();
    assert!(!north.is_empty(), "the office has back-turned desks");

    let chairs = queued(&layout, &pack, RenderScale::ONE, &north[..1], |k| {
        matches!(k, FixtureKind::DeskChair(_))
    });
    assert_eq!(
        chairs.len(),
        north.len() - 1,
        "the occupied desk's chair rides its sitter"
    );

    let desk = north[0];
    let (chair, _) = chair_span(&pack, crate::layout::Facing::North, desk).expect("a north chair");
    for anim in [PackPiece::TypingBack, PackPiece::SeatedBack] {
        let (w, h) = base_size(&pack, anim);
        for bob in [0, 1] {
            let seat = near_seat(desk);
            let body = piece_span(
                crate::layout::Pivot::TopLeft,
                crate::layout::Point {
                    x: seat.x,
                    y: seat.y + bob,
                },
                w,
                h,
                0,
            );
            let depth =
                crate::layout::desk_walk_anchor_facing(desk, crate::layout::Facing::North).y;
            let piece = occupant_span(body, depth, Some(chair));
            assert!(
                piece.depth >= chair.depth
                    && piece.depth >= depth
                    && piece.x0 <= chair.x0.min(body.x0)
                    && piece.x1 >= chair.x1.max(body.x1)
                    && piece.y0 <= chair.y0.min(body.y0)
                    && piece.y1 >= chair.y1.max(body.y1),
                "{}, bob {bob}: {piece:?} must cover {body:?} and {chair:?}",
                anim.name()
            );
        }
    }
    assert!(
        chair_span(&pack, crate::layout::Facing::South, desk).is_none(),
        "a viewer-facing occupant sits in front of their own chair"
    );

    // A chair wider than its sitter is still inside their piece: a passer-by
    // overlapping only the chair's columns sorts against the sitter too.
    let piece = occupant_span(
        Span::new(10, 10, 4, 6, 0),
        16,
        Some(Span::new(8, 13, 8, 4, 0)),
    );
    assert_eq!((piece.x0, piece.x1), (8, 15));
}

/// A standing chair sorts on the classic painter's own chair key.
#[test]
fn a_chair_sorts_on_the_classic_chair_key() {
    let pack = test_office();
    let desk = crate::layout::Point { x: 20, y: 30 };
    let (span, _) = chair_span(&pack, crate::layout::Facing::North, desk)
        .expect("a back-turned desk stands a chair");
    assert_eq!(
        span.depth,
        crate::layout::desk_chair_sort_row(desk, crate::layout::Facing::North)
    );
}

/// A walker sorts on the sim's sort row for them, every step.
#[test]
fn a_walker_sorts_on_the_sims_sort_row() {
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::South, 0);
    let mut walked = 0;
    for frame in &frames {
        let Some(c) = frame.characters.first().filter(|c| c.seat_desk.is_none()) else {
            continue;
        };
        let mut order = Vec::new();
        push_characters(
            frame,
            Office {
                layout: &layout,
                pack: &pack,
                theme: &crate::theme::NORMAL,
                scale: RenderScale::ONE,
            },
            std::time::UNIX_EPOCH,
            (&mut order, &mut Vec::new()),
        );
        let (span, _) = order.first().expect("the walker is drawn");
        assert_eq!(span.depth, c.sort_row);
        walked += 1;
    }
    assert!(walked > 1, "the fixture never walked, so this pins nothing");
}

/// A chair that rises above its sitter's head still lies inside their
/// piece, which sorts on the later of the two depths.
#[test]
fn an_occupant_span_bounds_a_chair_taller_than_its_sitter() {
    let body = Span::new(10, 20, 8, 12, 0);
    let chair = Span::new(9, 15, 10, 20, 0).with_depth(40);
    let piece = occupant_span(body, 31, Some(chair));
    assert_eq!(
        (piece.x0, piece.x1, piece.y0, piece.y1, piece.depth),
        (9, 18, 15, 34, 40)
    );
}

/// The bundled office with one editing agent homed at its first desk facing
/// `facing`, observed through the real sim every tick of their walk there:
/// the frames up to the first where they sit, then `seated_ticks` more, and
/// that desk.
pub(crate) fn sit_down(
    facing: crate::layout::Facing,
    seated_ticks: usize,
) -> (SceneLayout, OfficeArt, Vec<SimFrame>, crate::layout::Point) {
    sit_down_in(test_office(), facing, seated_ticks)
}

/// [`sit_down`] with `pack` drawing the office.
pub(crate) fn sit_down_in(
    pack: OfficeArt,
    facing: crate::layout::Facing,
    seated_ticks: usize,
) -> (SceneLayout, OfficeArt, Vec<SimFrame>, crate::layout::Point) {
    let id = pixtuoid_core::AgentId::from_transcript_path("/cutaway/sit.jsonl");
    sit_down_as(pack, facing, seated_ticks, id)
}

/// [`sit_down_in`] with `id` doing the walking.
pub(crate) fn sit_down_as(
    pack: OfficeArt,
    facing: crate::layout::Facing,
    seated_ticks: usize,
    id: pixtuoid_core::AgentId,
) -> (SceneLayout, OfficeArt, Vec<SimFrame>, crate::layout::Point) {
    use crate::floor::{FloorMeta, FloorSession};
    use pixtuoid_core::state::{ActivityState, FloorLocalDeskIndex, ToolKind};
    use std::time::{Duration, SystemTime};
    const LOGICAL: (u16, u16) = (160, 96);
    let meta = FloorMeta::ground();
    let layout = SceneLayout::compute_with_seed(LOGICAL.0, LOGICAL.1, None, meta.floor_seed)
        .expect("lays out");
    let home = layout
        .home_desks
        .iter()
        .position(|d| d.facing == facing)
        .expect("the office has a desk facing that way");
    let now0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut scene = pixtuoid_core::SceneState::uniform(16);
    scene.agents.insert(
        id,
        pixtuoid_core::AgentSlot {
            cwd: std::sync::Arc::from(std::path::Path::new("/w/x")),
            state_started_at: now0,
            created_at: now0,
            last_event_at: now0,
            desk_index: pixtuoid_core::GlobalDeskIndex(home),
            ..make_slot(
                id,
                ActivityState::Running {
                    tool_use_id: None,
                    detail: None,
                    kind: ToolKind::Edit,
                },
            )
        },
    );
    let mut session = FloorSession::new(std::sync::Arc::new(pack.clone()));
    let mut frames = Vec::new();
    let mut seated_at = None;
    for n in 1..=1200u64 {
        let frame = session
            .step(
                crate::floor::FloorInputs {
                    scene: &scene,
                    now: now0 + Duration::from_millis(100 * n),
                    floor: meta,
                    pets: crate::floor::PetInputs::default(),
                },
                crate::layout::Size {
                    w: LOGICAL.0,
                    h: LOGICAL.1,
                },
            )
            .expect("lays out")
            .frame;
        if seated_at.is_none()
            && frame
                .seated_agents
                .get(&FloorLocalDeskIndex(home))
                .copied()
                .unwrap_or(false)
        {
            seated_at = Some(frames.len());
        }
        frames.push(frame);
        if seated_at.is_some_and(|at| frames.len() > at + seated_ticks) {
            let desk = layout.home_desks[home].at;
            return (layout, pack, frames, desk);
        }
    }
    panic!("the agent never sat at their desk");
}

/// A seated sitter carries their desk's chair.
#[test]
fn a_seated_sitter_carries_their_chair() {
    let (layout, pack, frames, desk) = sit_down(crate::layout::Facing::North, 0);
    let seated = frames.last().expect("a seated frame");
    let mut order = Vec::new();
    assert_eq!(
        push_characters(
            seated,
            Office {
                layout: &layout,
                pack: &pack,
                theme: &crate::theme::NORMAL,
                scale: RenderScale::ONE
            },
            std::time::UNIX_EPOCH,
            (&mut order, &mut Vec::new())
        ),
        vec![desk]
    );
}

/// Whether desk `desk`'s chair draws over `frame`'s one person — riding their
/// piece once they sit, or as its own piece before — or `None` where the two
/// do not overlap, so their order shows nothing.
fn chair_over_person(
    frame: &SimFrame,
    layout: &SceneLayout,
    pack: &OfficeArt,
    desk: crate::layout::Point,
) -> Option<bool> {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let order = collect_pieces(
        frame,
        Office {
            layout,
            pack,
            theme,
            scale: RenderScale::ONE,
        },
        &Moment::resolve(
            crate::sky::Sky::clock(std::time::UNIX_EPOCH),
            theme,
            0.0,
            Motion::Full.timing(std::time::UNIX_EPOCH),
        ),
        &mut crate::outside::OutsideCache::default(),
    )
    .0;
    let (person, person_span) = order
        .iter()
        .enumerate()
        .find_map(|(i, (s, k))| matches!(k, PieceKind::Character { .. }).then_some((i, *s)))?;
    if let (_, PieceKind::Character { chair: Some(_), .. }) = &order[person] {
        return Some(true);
    }
    let (at, chair_span) =
        crate::layout::desk_chair_top_left(desk, crate::layout::Facing::North)
            .zip(chair_span(pack, crate::layout::Facing::North, desk).map(|(s, _)| s))?;
    let chair = order
        .iter()
        .position(|(_, k)| matches!(k, PieceKind::Chair { at: a } if *a == at))?;
    let overlap = person_span.x0 <= chair_span.x1
        && chair_span.x0 <= person_span.x1
        && person_span.y0 <= chair_span.y1
        && chair_span.y0 <= person_span.y1;
    if !overlap {
        return None;
    }
    let drawn = crate::display::depth_sort(
        order
            .iter()
            .enumerate()
            .map(|(i, (s, _))| (*s, i))
            .collect(),
    );
    let pos = |i: usize| drawn.iter().position(|&j| j == i);
    Some(pos(chair)? > pos(person)?)
}

/// The chair draws over its occupant through the settle arc — every frame
/// the sim keys them at their seat — as in the classic painter: keyed on its
/// own box, it would sort behind them until they sat and jump in front the
/// frame they did.
#[test]
fn a_chair_keeps_its_order_to_its_sitter_through_the_settle() {
    use crate::layout::Facing;
    let (layout, pack, frames, desk) = sit_down(Facing::North, 0);
    let seat_row = crate::layout::desk_chair_sort_row(desk, Facing::North);
    let orders: Vec<(usize, bool)> = frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.characters.first().is_some_and(|c| c.sort_row == seat_row))
        .filter_map(|(n, f)| chair_over_person(f, &layout, &pack, desk).map(|o| (n, o)))
        .collect();
    assert!(
        orders.len() > 1,
        "the sitter was never keyed at their seat before sitting"
    );
    assert!(
        orders.iter().all(|&(_, over)| over),
        "the chair flipped under its sitter at frames {:?}",
        orders
            .iter()
            .filter(|(_, o)| !o)
            .map(|(n, _)| n)
            .collect::<Vec<_>>()
    );
}

/// A person sorts on the sim's own key, so a viewer-facing sitter's depth
/// holds while their breath moves their drawn box.
#[test]
fn a_sitters_depth_holds_through_their_breath() {
    let (layout, pack, frames, desk) = sit_down(crate::layout::Facing::South, 60);
    let (mut depths, mut tops) = (
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    );
    for frame in frames.iter().filter(|f| {
        f.characters
            .first()
            .is_some_and(|c| c.seat_desk.is_some_and(|d| d.at == desk))
    }) {
        let mut order = Vec::new();
        push_characters(
            frame,
            Office {
                layout: &layout,
                pack: &pack,
                theme: &crate::theme::NORMAL,
                scale: RenderScale::ONE,
            },
            std::time::UNIX_EPOCH,
            (&mut order, &mut Vec::new()),
        );
        let (span, _) = order.first().expect("the sitter is drawn");
        depths.insert(span.depth);
        tops.insert(span.y0);
    }
    assert!(
        tops.len() > 1,
        "the sitter never breathed, so this pins nothing: {tops:?}"
    );
    assert_eq!(
        depths.len(),
        1,
        "their depth moved with their breath: {depths:?}"
    );
}

/// A badge `text` hanging from `anchor`.
fn badge_at(anchor: crate::layout::Point) -> TextRun {
    TextRun {
        at: anchor,
        align: Align::Over,
        spans: vec![crate::display::TextSpan::new(
            "cc",
            pixtuoid_core::sprite::Rgb { r: 9, g: 9, b: 9 },
        )],
        plate: None,
        strip: None,
        role: crate::display::TextRole::Badge(pixtuoid_core::AgentId::from_transcript_path(
            "/badge.jsonl",
        )),
    }
}

/// The row a badge's plate ends above.
fn plate_bottom(anchor: crate::layout::Point) -> u16 {
    let plate = badge_plate(&badge_at(anchor), Pen::UNIT);
    plate.y.0 + plate.h.0
}

/// A badge centres over the sprite, [`LABEL_GAP`] rows clear of its head.
#[test]
fn a_badge_sits_above_the_head_and_centred_on_the_sprite() {
    let at = crate::layout::Point { x: 10, y: 20 };
    let size = crate::layout::Size { w: 8, h: 12 };
    let anchor = crate::sim::anchors::badge_anchor(at, size, None);
    assert_eq!(anchor.x, at.x + 4, "centred on the sprite");
    assert_eq!(at.y - plate_bottom(anchor), LABEL_GAP, "clear of the head");
}

/// The floor indicator's plate stays in the cell the classic writes it
/// across, its text whole, at every density the pack draws: a row lower
/// and it covers the top of the elevator door, over everything.
#[test]
#[cfg(feature = "cutaway-assets")]
fn the_floor_indicator_stays_in_its_cell() {
    use crate::display::text::LINE_H;
    let pack = crate::pack::test_office();
    let door = SceneLayout::compute_with_seed(160, 96, None, 0)
        .expect("lays out")
        .door;
    let rows = crate::layout::floor_indicator_rows(door.y);
    let densities = pack.density_variants();
    assert!(!densities.is_empty(), "the pack draws a density");
    for d in densities {
        let pen = Pen::new(RenderScale::from(*d), *d).expect("d divides itself");
        for (floor, floors) in [(0, 1), (11, 12), (98, 99)] {
            let floor = crate::floor::FloorMeta::for_floor(floor, floors);
            let run = TextRun::indicator(door, floor, &crate::theme::NORMAL);
            let plate = run_rect(&run, pen);
            let span = topmost_span(plate, pen);
            assert!(
                rows.contains(&span.y0) && rows.contains(&span.y1),
                "{d:?} floor {}: rows {}..={} outside {rows:?}",
                floor.floor_idx,
                span.y0,
                span.y1
            );
            assert!(plate.h.0 >= LINE_H, "{d:?}: the text fits");
        }
    }
}

/// The board's star ends at the sign's interior's right edge at every scale
/// the pack draws, measured on the art grid, not in text cells.
#[test]
fn the_star_sits_flush_with_the_interior_at_every_scale() {
    use crate::layout::{NEON_PANEL_INNER_W, NEON_PANEL_INNER_X};
    let pack = test_office();
    for s in [1, pack.max_density_variant().get()] {
        let pen = Pen::for_pack(RenderScale::new(s).expect("nonzero"), &pack);
        let star = quiet_board()
            .runs(&crate::theme::NORMAL)
            .into_iter()
            .find(|run| run.role == crate::display::TextRole::Star)
            .expect("the star");
        assert_eq!(
            run_rect(&star, pen).x.0 + crate::display::text::advance(&star.text()).0,
            pen.art(NEON_PANEL_INNER_X + NEON_PANEL_INNER_W).0,
            "at scale {s}"
        );
    }
}

/// At every scale, no sign run overprints another on its line: where the
/// pixel font is too wide for the sign's interior (the base art's grid), the
/// star yields to the brand rather than writing over it.
#[test]
fn no_run_overprints_another_on_its_line() {
    let pack = test_office();
    let layout = SceneLayout::compute_with_seed(240, 144, None, 0).expect("lays out");
    let d = pack.max_density_variant().get();
    for s in [1, d, 2 * d] {
        let scale = RenderScale::new(s).expect("nonzero");
        let office = Office {
            layout: &layout,
            pack: &pack,
            theme: &crate::theme::NORMAL,
            scale,
        };
        let pen = Pen::for_pack(scale, &pack);
        let rects: Vec<(crate::display::TextRole, u16, ArtRect)> = signs(
            office,
            &TextRun::signs(
                quiet_board(),
                layout.door,
                crate::floor::FloorMeta::ground(),
                &crate::theme::NORMAL,
            ),
        )
        .into_iter()
        .filter_map(|(_, kind)| match kind {
            PieceKind::Text { run } => Some((run.role, run.at.y, run_rect(&run, pen))),
            _ => None,
        })
        .collect();
        assert!(
            rects
                .iter()
                .any(|(r, ..)| *r == crate::display::TextRole::Brand),
            "at scale {s} the brand is drawn"
        );
        // Without the density art every scale is the base art's grid.
        assert_eq!(
            rects
                .iter()
                .any(|(r, ..)| *r == crate::display::TextRole::Star),
            cfg!(feature = "cutaway-assets") && s.is_multiple_of(d),
            "at scale {s} the star is drawn exactly where the pack's density divides it"
        );
        for (i, (a, ya, ra)) in rects.iter().enumerate() {
            for (b, yb, rb) in &rects[i + 1..] {
                assert!(
                    ya != yb || !meets(*ra, *rb),
                    "at scale {s} {a:?} {ra:?} overprints {b:?} {rb:?}"
                );
            }
        }
    }
}

/// At the pack's 4x art the board writes inside the neon sign's dark
/// interior, as the classic's terminal board does, however full its lines.
#[test]
#[cfg(feature = "cutaway-assets")]
fn the_board_writes_inside_the_signs_interior() {
    use crate::layout::{
        NEON_PANEL_INNER_H, NEON_PANEL_INNER_W, NEON_PANEL_INNER_X, NEON_PANEL_INNER_Y,
    };
    let pack = test_office();
    let scale = RenderScale::from(pack.max_density_variant());
    let counts = crate::tally::StateCounts {
        requires_action: 12,
        running: 34,
        idle: 56,
        exiting: 0,
        total: 102,
    };
    let gateway = Some(pixtuoid_core::state::DaemonState::Degraded);
    for ms in (0..16_000).step_by(100) {
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms);
        let board = crate::neon_sign::build_board(
            counts,
            99 * 3_600,
            Some((12, 12)),
            gateway,
            crate::anim::Motion::Full,
            now,
        );
        let pen = Pen::for_pack(scale, &pack);
        for run in board.runs(&crate::theme::NORMAL) {
            let span = topmost_span(run_rect(&run, pen), pen);
            assert!(
                span.x0 >= NEON_PANEL_INNER_X
                    && span.x1 < NEON_PANEL_INNER_X + NEON_PANEL_INNER_W
                    && span.y0 >= NEON_PANEL_INNER_Y
                    && span.y1 < NEON_PANEL_INNER_Y + NEON_PANEL_INNER_H,
                "{:?} {span:?} at +{ms}ms",
                run.role
            );
        }
    }
}

/// A ceiling ABOVE the head lifts the badge clear of it; one below the head
/// changes nothing.
#[test]
fn a_badge_clears_a_ceiling_above_the_head() {
    use crate::sim::anchors::badge_anchor;
    let at = crate::layout::Point { x: 10, y: 20 };
    let size = crate::layout::Size { w: 8, h: 12 };
    let free = badge_anchor(at, size, None);
    let raised = badge_anchor(at, size, Some(at.y - 4));
    assert_eq!(
        plate_bottom(raised),
        at.y - 4 - LABEL_GAP,
        "the badge clears the monitor top by the same gap it clears a head by"
    );
    assert_eq!(raised.x, free.x);
    assert_eq!(badge_anchor(at, size, Some(at.y + 4)), free);
}

/// A figure standing casts its own shadow; sitting, the chair they carry
/// casts one under itself, so a chair's shadow stays put as they sit down.
#[test]
fn a_sitters_chair_casts_the_shadow_they_do_not() {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
    let (mut standing, mut sitting) = (false, false);
    for frame in &frames {
        for (span, kind) in collect_pieces(
            frame,
            Office {
                layout: &layout,
                pack: &pack,
                theme,
                scale: RenderScale::ONE,
            },
            &Moment::resolve(
                crate::sky::Sky::clock(std::time::UNIX_EPOCH),
                theme,
                0.0,
                Motion::Full.timing(std::time::UNIX_EPOCH),
            ),
            &mut crate::outside::OutsideCache::default(),
        )
        .0
        {
            let PieceKind::Character {
                ref figure,
                body,
                chair,
                ..
            } = kind
            else {
                continue;
            };
            let cast = ground_shadow(span, &kind, &pack).expect("every figure grounds");
            if figure.shadow {
                standing = true;
                assert_eq!(
                    cast,
                    crate::ground::Contact::under(body.x0, body.x1 - body.x0 + 1, body.y1 + 1)
                );
            } else {
                sitting = true;
                let at = chair.expect("a desk sitter carries their chair");
                let (w, h) = art_size(&pack, PackPiece::DeskChair);
                let empty = piece_span(crate::layout::Pivot::TopLeft, at, w, h, 0);
                assert_eq!(
                    Some(cast),
                    ground_shadow(empty, &PieceKind::Chair { at }, &pack),
                    "the chair casts the shadow it cast empty"
                );
            }
        }
    }
    assert!(standing && sitting, "the walk in and the sit");
}

/// A frame of an office nobody is in, its room lights full and its sign
/// calm.
pub(crate) fn empty_frame(layout: &SceneLayout) -> SimFrame {
    SimFrame {
        agents: Vec::new(),
        poses: std::collections::HashMap::new(),
        seated_agents: std::collections::HashMap::new(),
        characters: Vec::new(),
        indoor_scale: 1.0,
        neon: crate::floor::NeonLevels::CALM,
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

/// The pieces the fixtures `keep` picks queue ([`push_fixture`]) in an
/// empty office.
pub(crate) fn queued(
    layout: &SceneLayout,
    pack: &OfficeArt,
    scale: RenderScale,
    carried: &[crate::layout::Point],
    keep: impl Fn(FixtureKind) -> bool,
) -> Vec<(Span, PieceKind)> {
    let timing = Motion::Full.timing(std::time::UNIX_EPOCH);
    queued_at(layout, pack, scale, carried, keep, timing)
}

/// [`queued`] on `timing`.
fn queued_at(
    layout: &SceneLayout,
    pack: &OfficeArt,
    scale: RenderScale,
    carried: &[crate::layout::Point],
    keep: impl Fn(FixtureKind) -> bool,
    timing: crate::anim::Timing,
) -> Vec<(Span, PieceKind)> {
    let theme = crate::theme::theme_by_name("normal").expect("theme");
    let frame = empty_frame(layout);
    let moment = Moment::resolve(crate::sky::Sky::clock(timing.now), theme, 0.0, timing);
    let inputs = ComposeInputs {
        frame: &frame,
        office: Office {
            layout,
            pack,
            theme,
            scale,
        },
        moment: &moment,
    };
    let mut order = Vec::new();
    for f in layout.fixtures().filter(|f| keep(f.kind)) {
        push_fixture(f, inputs, carried, &mut order);
    }
    order
}

fn near_seat(desk: crate::layout::Point) -> crate::layout::Point {
    crate::sim::seated_top_left(
        desk,
        crate::layout::CHARACTER_SPRITE_W,
        crate::layout::Facing::North,
    )
}

pub(crate) fn base_size(pack: &OfficeArt, piece: PackPiece) -> (u16, u16) {
    let f = pack.piece(piece).first();
    (f.width(), f.height())
}

/// A REAL office's display list, checked against every pairwise "must be
/// behind" fact its own geometry states.
#[test]
fn a_real_offices_display_list_satisfies_every_ordering_constraint() {
    let pack = test_office();
    for (w, h) in [(160u16, 96u16), (240, 144), (100, 60)] {
        let layout = SceneLayout::compute_with_seed(w, h, None, 0).expect("lays out");
        let mut order = queued(&layout, &pack, RenderScale::ONE, &[], |_| true);
        wall_segments(
            &layout,
            crate::glass::WallTrim::of(&crate::theme::NORMAL),
            &mut order,
        );
        assert!(order.len() > 10, "{w}x{h} produced a trivial list");

        let spans: Vec<Span> = order.iter().map(|(s, _)| *s).collect();
        let tagged: Vec<(Span, usize)> = spans.iter().copied().zip(0..).collect();
        let produced = crate::display::depth_sort(tagged);
        assert_eq!(produced.len(), spans.len(), "{w}x{h} dropped a piece");
        assert_eq!(
            crate::display::check_order(&spans, &produced),
            None,
            "{w}x{h}: the display list violates a constraint its geometry states"
        );
    }
}

/// `frame`'s list at local `hour`, under a clear sky.
pub(crate) fn list_at<'a>(frame: &SimFrame, office: Office<'a>, hour: u32) -> DisplayList<'a> {
    let now = crate::localclock::at_hour(hour);
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

pub(crate) fn kind_name(kind: &PieceKind) -> &'static str {
    match kind {
        PieceKind::WallSeg { .. } => "wall",
        PieceKind::Desk { .. } => "desk",
        PieceKind::DeskFront { .. } => "desk front",
        PieceKind::Chair { .. } => "chair",
        PieceKind::Prop { .. } => "prop",
        PieceKind::PropBand { .. } => "prop band",
        PieceKind::Table { .. } => "table",
        PieceKind::Animated { .. } => "animated",
        PieceKind::Door { .. } => "door",
        PieceKind::Neon { .. } => "neon",
        PieceKind::Clock { .. } => "clock",
        PieceKind::Character { .. } => "character",
        PieceKind::Window { .. } => "window",
        PieceKind::Hung { .. } => "hung decor",
        PieceKind::Effect(_) => "effect",
        PieceKind::Text { .. } => "text",
        PieceKind::DeskProp(_) => "desk prop",
        PieceKind::Creature { .. } => "creature",
    }
}

/// Splitting is what makes the office above orderable, so pin it directly:
/// no band of a N-S wall may be tall enough to span a figure. An E-W wall
/// is whole on one row, which a figure is wholly north or south of.
#[test]
fn no_wall_segment_is_taller_than_the_cast() {
    let pack = test_office();
    let (_, body_h) = base_size(&pack, PackPiece::Standing);
    let layout = SceneLayout::compute_with_seed(240, 144, None, 0).expect("lays out");
    let mut order: Vec<(Span, PieceKind)> = Vec::new();
    wall_segments(
        &layout,
        crate::glass::WallTrim::of(&crate::theme::NORMAL),
        &mut order,
    );
    order.retain(|(_, k)| {
        matches!(
            k,
            PieceKind::WallSeg {
                piece: crate::layout::WallPiece::Vertical { .. },
                ..
            }
        )
    });
    assert!(!order.is_empty(), "a laid-out office has N-S walls");
    for (span, _) in &order {
        let h = span.y1 - span.y0 + 1;
        assert!(
            h <= body_h,
            "a {h}-row wall segment can span the {body_h}-row cast, so one \
             segment would be both in front of and behind the same figure"
        );
    }
}

/// A back-view sofa sits its sitter between its two bands: the seat under
/// them, the backrest, nearest the viewer, over their lap.
#[test]
fn a_back_view_sofa_seats_its_sitter_between_its_seat_and_its_backrest() {
    let pack = test_office();
    let sofa = crate::layout::Point { x: 40, y: 30 };
    let mut order = Vec::new();
    push_sofa(&mut order, &pack, sofa, true, Tie::FixtureOver);
    let [
        (seat, PieceKind::PropBand { rows: under, .. }),
        (back, PieceKind::PropBand { rows: over, .. }),
    ] = order.as_slice()
    else {
        panic!("a back-view sofa is two bands: {order:?}");
    };
    let sitter = crate::sim::seat::sofa_sitter_sort_row(sofa);
    assert!(
        seat.depth == sitter && sitter < back.depth,
        "seat {} = sitter {sitter} < backrest {}",
        seat.depth,
        back.depth
    );
    let (_, h) = base_size(&pack, PackPiece::MeetingSofaNorth);
    assert_eq!((under.0, under.1, over.0, over.1), (0, over.0, under.1, h));
}

/// A meeting room at its tightest still stands the table behind the
/// back-view sofa's seat: sorted on its own south edge, the seat tied the
/// table there, and the table, pushed after it, painted over its cushions.
#[test]
fn the_table_sorts_behind_the_back_view_sofas_seat_in_the_tightest_room() {
    let pack = test_office();
    let mut checked = 0;
    for (w, h) in [(110, 66), (130, 90)] {
        for seed in 0..4 {
            let Some(layout) = SceneLayout::compute_with_seed(w, h, None, seed) else {
                continue;
            };
            let order = queued(&layout, &pack, RenderScale::ONE, &[], |k| {
                matches!(
                    k,
                    FixtureKind::MeetingSofa { .. } | FixtureKind::MeetingTable { .. }
                )
            });
            let seats = order.iter().filter_map(|(s, k)| match k {
                PieceKind::PropBand { rows: (0, _), .. } => Some(s.depth),
                _ => None,
            });
            let tables = order.iter().filter_map(|(s, k)| match k {
                PieceKind::Table { .. } => Some(s.depth),
                _ => None,
            });
            for (seat, table) in seats.zip(tables) {
                assert!(table < seat, "{w}x{h}/{seed}: table {table}, seat {seat}");
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "the sizes lay out meeting trios");
}

/// An office with every moving fixture: an aquarium and a cooler that
/// loop, an elevator, the sign and the clock.
pub(crate) fn lively_office() -> SceneLayout {
    many_layouts()
        .find(|l| {
            let kinds: Vec<FixtureKind> = l.fixtures().map(|f| f.kind).collect();
            [
                FixtureKind::FishTank,
                FixtureKind::WaterCooler,
                FixtureKind::Door,
                FixtureKind::Clock,
            ]
            .iter()
            .all(|k| kinds.contains(k))
        })
        .expect("an office has a lounge aquarium, a pantry cooler, an elevator and a clock")
}

/// A desk sorts on the roster's row, the classic painter's: a figure one row
/// south of it draws over it, one row north draws under it.
#[test]
fn a_walker_just_south_of_a_desk_front_draws_over_it() {
    let pack = test_office();
    let layout = SceneLayout::compute_with_seed(160, 96, None, 0).expect("lays out");
    let desk = layout
        .fixtures()
        .find(|f| matches!(f.kind, FixtureKind::Desk(_)))
        .expect("a desk");
    let Depth::Sorted { row, .. } = desk.depth else {
        panic!("a desk sorts: {desk:?}");
    };
    let pieces = queued(&layout, &pack, RenderScale::ONE, &[], |k| k == desk.kind);
    let [(span, PieceKind::Desk { .. }), ref front @ ..] = pieces[..] else {
        panic!("the desk piece first");
    };
    // its front, if any, sorts as the desk does
    for (s, kind) in front {
        assert!(matches!(kind, PieceKind::DeskFront { .. }), "{kind:?}");
        assert_eq!(*s, span, "the front sorts with its desk");
    }
    assert_eq!(span.depth, row, "the desk sorts on the roster's row");
    let walker = |depth| Span::new(span.x0, span.y0, 4, 8, 0).with_depth(depth);
    for (depth, over) in [(row + 1, true), (row - 1, false)] {
        let drawn = depth_sort(vec![(span, "desk"), (walker(depth), "walker")]);
        assert_eq!(
            drawn[1] == "walker",
            over,
            "a walker sorted on {depth}, the desk on {row}"
        );
    }
}

/// A figure at a fixture piece's row paints over it, but for a desk chair
/// and a sofa seen from behind, which hide their sitters; a back-view
/// sofa's seat, which its sitter sits on, stays under.
#[test]
fn a_fixture_ties_a_figure_as_the_roster_says() {
    let pack = test_office();
    let scale = RenderScale::from(pack.max_density_variant());
    let mut over = std::collections::BTreeSet::new();
    for layout in many_layouts() {
        for fixture in layout.fixtures() {
            if fixture.depth == Depth::Backdrop {
                continue;
            }
            use FixtureKind as K;
            let hides_its_sitter = match fixture.kind {
                K::DeskChair(_) | K::LoungeCouch => true,
                K::MeetingSofa { faces_away, .. } => faces_away,
                K::Desk(_)
                | K::FilingCabinet(_)
                | K::Station { .. }
                | K::Plant { .. }
                | K::Pod { .. }
                | K::Wall { .. }
                | K::MeetingRug { .. }
                | K::MeetingTable { .. }
                | K::MeetingChair { .. }
                | K::CoatRack { .. }
                | K::Doormat { .. }
                | K::NoticeBoard { .. }
                | K::LoungeRug
                | K::SideTable
                | K::FloorLamp
                | K::FishTank
                | K::KitchenIsland
                | K::PantryMat
                | K::IslandMat
                | K::WaterCooler
                | K::TrashBin
                | K::Door
                | K::Runner
                | K::NeonSign
                | K::Clock => false,
            };
            for (span, kind) in queued(&layout, &pack, scale, &[], |k| k == fixture.kind) {
                let seat = matches!(kind, PieceKind::PropBand { rows: (0, _), .. });
                let body = Span::new(span.x0, span.y0, 1, 1, 0);
                let figure = occupant_span(body, span.depth, None);
                // The figure queued first, so push order alone would draw it under.
                let drawn = crate::display::depth_sort(vec![(figure, "figure"), (span, "fixture")]);
                let want = if hides_its_sitter && !seat {
                    over.insert(crate::layout::roster::tests::kind_key(fixture.kind));
                    ["figure", "fixture"]
                } else {
                    ["fixture", "figure"]
                };
                assert_eq!(drawn, want, "{:?}: {kind:?}", fixture.kind);
            }
        }
    }
    assert_eq!(
        over.len(),
        3,
        "the chair and both sofas hide a sitter: {over:?}"
    );
}

/// A glass wall band composites over whoever stands behind it at its row.
#[test]
fn a_wall_band_draws_over_a_figure_at_its_row() {
    let mut walls = 0;
    for layout in many_layouts() {
        let mut order = Vec::new();
        wall_segments(
            &layout,
            crate::glass::WallTrim::of(&crate::theme::NORMAL),
            &mut order,
        );
        for (span, _) in order {
            let figure = occupant_span(Span::new(span.x0, span.y0, 1, 1, 0), span.depth, None);
            let drawn = crate::display::depth_sort(vec![(span, "wall"), (figure, "figure")]);
            assert_eq!(drawn, ["figure", "wall"], "{span:?}");
            walls += 1;
        }
    }
    assert!(walls > 0, "no office had a wall");
}

/// The lounge couch faces the window, so the cutaway draws its back.
#[test]
fn the_lounge_couch_is_drawn_from_behind() {
    let pack = test_office();
    let scale = RenderScale::from(pack.max_density_variant());
    let layout = many_layouts()
        .find(|l| l.lounge.is_some())
        .expect("an office with a lounge");
    let pieces = queued(&layout, &pack, scale, &[], |k| {
        k == FixtureKind::LoungeCouch
    });
    assert!(
        !pieces.is_empty()
            && pieces.iter().all(|(_, k)| matches!(
                k,
                PieceKind::PropBand { sprite, .. } if *sprite == PackPiece::MeetingSofaNorth
            )),
        "{pieces:?}"
    );
}

/// The fish tank and the water cooler loop on the floor's beat: Full steps
/// them, Calm a [`CALM_TICK_MS`](crate::anim::CALM_TICK_MS) pace slower,
/// Still never.
#[test]
fn a_looping_fixture_plays_on_the_floors_beat() {
    use crate::anim::{CALM_TICK_MS, FULL_TICK_MS};
    let pack = test_office();
    let looping = |k| matches!(k, FixtureKind::FishTank | FixtureKind::WaterCooler);
    let layout = many_layouts()
        .find(|l| {
            [FixtureKind::FishTank, FixtureKind::WaterCooler]
                .iter()
                .all(|&k| l.fixtures().any(|f| f.kind == k))
        })
        .expect("a layout with a fish tank and a water cooler");
    let frame_ms = |sprite| u64::from(pack.piece(sprite).frame_ms());
    let (a, b) = (
        frame_ms(PackPiece::FishTank),
        frame_ms(PackPiece::WaterCooler),
    );
    let gcd = |mut x: u64, mut y: u64| {
        while y != 0 {
            (x, y) = (y, x % y);
        }
        x
    };
    // Whole Calm repaints of both loops' frame_ms, so every tier ends on a step.
    let span_ms = 2 * a / gcd(a, b) * b * (CALM_TICK_MS / FULL_TICK_MS);
    let steps = |motion: Motion| {
        let mut steps = std::collections::BTreeMap::<PackPiece, u64>::new();
        let mut last = std::collections::BTreeMap::new();
        for ms in (0..=span_ms).step_by(FULL_TICK_MS as usize) {
            let timing =
                motion.timing(std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms));
            for (_, kind) in queued_at(&layout, &pack, RenderScale::ONE, &[], looping, timing) {
                let PieceKind::Animated { art, .. } = kind else {
                    panic!("a looping fixture queues an animated piece: {kind:?}");
                };
                if last
                    .insert(art.sprite, art.frame)
                    .is_some_and(|was| was != art.frame)
                {
                    *steps.entry(art.sprite).or_default() += 1;
                }
            }
        }
        (steps, last)
    };
    let (full, _) = steps(Motion::Full);
    let (calm, _) = steps(Motion::Calm);
    let (still, held) = steps(Motion::Still);
    assert_eq!(full.len(), 2, "both loops step on Full: {full:?}");
    for (sprite, n) in &full {
        assert_eq!(
            calm.get(sprite).copied().unwrap_or(0) * (CALM_TICK_MS / FULL_TICK_MS),
            *n,
            "{} on Calm steps a quarter as often as on Full",
            sprite.name()
        );
    }
    assert!(still.is_empty(), "Still moves no loop: {still:?}");
    assert!(
        held.values().all(|&f| f == 0),
        "Still holds the first frame: {held:?}"
    );
}

/// Layouts across the sizes and seeds that place every kind of piece this
/// module draws: pods with booths and desks, meeting rooms, a pantry.
pub(crate) fn many_layouts() -> impl Iterator<Item = SceneLayout> {
    // The small sizes are an 80x24-class terminal's, where a door can run
    // flush with its wall's end.
    [
        (64, 50),
        (80, 46),
        (80, 48),
        (160, 96),
        (200, 120),
        (240, 144),
        (320, 180),
    ]
    .into_iter()
    .flat_map(|(w, h)| {
        (0..4).filter_map(move |seed| SceneLayout::compute_with_seed(w, h, None, seed))
    })
}

/// Pins [`push_sofa`]'s front view on its sitters' key, so they paint over
/// it.
#[test]
fn a_front_view_sofa_ties_its_sitters() {
    let pack = test_office();
    let sofa = crate::layout::Point { x: 40, y: 30 };
    let mut order = Vec::new();
    push_sofa(&mut order, &pack, sofa, false, Tie::FigureOver);
    let [
        (
            span,
            PieceKind::Prop {
                art: Art {
                    flip: Flip::None, ..
                },
                ..
            },
        ),
    ] = order.as_slice()
    else {
        panic!("a front sofa is one prop: {order:?}");
    };
    assert_eq!(span.depth, crate::sim::seat::sofa_sitter_sort_row(sofa));
}

/// No wall this module stands closes a doorway, and every doorway is framed
/// by two jambs.
#[test]
fn walls_leave_every_doorway_open_and_frame_it() {
    let mut checked = 0;
    for layout in many_layouts() {
        let mut order = Vec::new();
        wall_segments(
            &layout,
            crate::glass::WallTrim::of(&crate::theme::NORMAL),
            &mut order,
        );
        for d in &layout.doorways {
            let vertical = d.start.x == d.end.x;
            let (lo, hi) = if vertical {
                (d.start.y.min(d.end.y), d.start.y.max(d.end.y))
            } else {
                (d.start.x.min(d.end.x), d.start.x.max(d.end.x))
            };
            for v in lo + 1..hi {
                let (x, y) = if vertical {
                    (d.start.x, v)
                } else {
                    (v, d.start.y)
                };
                // Its own wall's pieces only: a crossing wall's glass rising
                // in front of the opening is occlusion, not a wall in it.
                let blocked = order.iter().any(|(s, k)| {
                    let own = match k {
                        PieceKind::WallSeg {
                            piece: crate::layout::WallPiece::Vertical { x: px, .. },
                            ..
                        } => vertical && *px == d.start.x,
                        PieceKind::WallSeg {
                            piece: crate::layout::WallPiece::Horizontal { y_face, .. },
                            ..
                        } => !vertical && *y_face == d.start.y,
                        _ => false,
                    };
                    own && (s.x0..=s.x1).contains(&x) && (s.y0..=s.y1).contains(&y)
                });
                assert!(
                    !blocked,
                    "a wall stands in the doorway at ({x}, {y}): {d:?}"
                );
            }
            checked += 1;
        }
        let jambs: std::collections::HashSet<_> = order
            .iter()
            .filter_map(|(_, k)| match k {
                PieceKind::WallSeg { piece, .. } => Some(*piece),
                _ => None,
            })
            .flat_map(|piece| {
                let (at, size) = piece.visual();
                piece.jambs().inspect(move |&(p, s)| {
                    assert!(
                        p.x >= at.x
                            && p.y >= at.y
                            && p.x + s.w <= at.x + size.w
                            && p.y + s.h <= at.y + size.h,
                        "a jamb {p:?}+{s:?} reaches past its wall {piece:?}"
                    );
                })
            })
            .collect();
        assert_eq!(
            jambs.len(),
            2 * layout.doorways.len(),
            "two jambs a doorway"
        );
    }
    assert!(checked > 0, "the layouts cut doorways");
}

/// The pantry counter stands where the layout stands it: on the Pantry
/// waypoint the mask blocks and the visitor faces.
#[test]
fn the_pantry_counter_stands_on_its_waypoint() {
    let pack = test_office();
    let mut checked = 0;
    for layout in many_layouts() {
        let Some(wp) = layout
            .waypoints
            .iter()
            .find(|wp| wp.kind == crate::layout::WaypointKind::Pantry)
        else {
            continue;
        };
        let order = queued(&layout, &pack, RenderScale::ONE, &[], |k| {
            matches!(
                k,
                FixtureKind::Station {
                    station: Station::PantryCounter,
                    ..
                }
            )
        });
        let [(_, PieceKind::Prop { at, .. })] = order.as_slice() else {
            panic!("one counter: {order:?}");
        };
        assert_eq!(*at, wp.pos);
        checked += 1;
    }
    assert!(checked > 0, "the layouts have pantries");
}

/// No piece is queued twice, and every pod decor piece once: a phone booth
/// or a standing desk is pod decor, and its waypoint is only where a
/// visitor stands.
#[test]
fn no_piece_is_queued_twice() {
    let pack = test_office();
    let mut booths = 0;
    for layout in many_layouts() {
        let order = queued(&layout, &pack, RenderScale::ONE, &[], |_| true);
        let mut seen = std::collections::HashSet::new();
        for (span, kind) in &order {
            assert!(
                seen.insert((span.x0, span.y0, span.x1, span.y1, fingerprint(kind))),
                "{} queued twice at {span:?}",
                kind_name(kind)
            );
        }
        for d in &layout.pod_decor {
            let queued = order
                .iter()
                .filter(|(_, k)| {
                    matches!(k, PieceKind::Prop { at, art }
                        if *at == d.pos && art.sprite == d.kind.piece())
                })
                .count();
            assert_eq!(queued, 1, "{:?} at {:?}", d.kind, d.pos);
            booths += usize::from(d.kind == crate::layout::PodDecor::PhoneBooth);
        }
    }
    assert!(booths > 0, "the layouts place a phone booth");
}

/// Wall decor that stands on the ground sorts among the ground's pieces, so a
/// figure north of a whiteboard between pods goes behind it; decor that
/// only hangs on the band is left to the backdrop.
#[test]
fn ground_standing_wall_decor_sorts_with_the_ground() {
    let pack = test_office();
    let mut standing = 0;
    for layout in many_layouts() {
        let order = queued(&layout, &pack, RenderScale::ONE, &[], |k| {
            matches!(k, FixtureKind::Wall { .. })
        });
        for item in &layout.wall_decor {
            let queued = order.iter().any(|(_, k)| {
                matches!(k, PieceKind::Prop { art, .. } if art.sprite == item.kind.piece())
            });
            assert_eq!(queued, item.kind.stands_on_floor(), "{:?}", item.kind);
            standing += usize::from(queued);
        }
    }
    assert!(standing > 0, "the layouts place floor-standing decor");
}

/// Both densities place a desk's lamp, cup and tower from one arrangement
/// (gen-art's `DESK_ARRANGEMENT`): each sits in the same column at 1x and at
/// the densest art, in either facing. The rows are each density's own.
#[test]
fn every_desk_follows_the_one_arrangement() {
    use crate::layout::Point;
    let pack = test_office();
    let dense = RenderScale::from(pack.max_density_variant());
    let desk = Point { x: 40, y: 30 };
    for which in [Desk::North, Desk::South] {
        let art = which.piece().name();
        let column = |scale: RenderScale, prop: DeskProp| {
            let (art, density, _) = pack.desk(which).at(scale);
            art.props[prop].0 / density.get()
        };
        for prop in [DeskProp::Cup, DeskProp::Tower] {
            assert_eq!(
                column(dense, prop),
                column(RenderScale::ONE, prop),
                "{art}'s {} stands in another column at {dense:?}",
                prop.mark()
            );
        }
        let bulb = |scale| bulb_at(desk, which, &pack, scale).x;
        assert_eq!(
            bulb(dense),
            bulb(RenderScale::ONE),
            "{art}'s lamp moved wings"
        );
    }
    // The back-turned desk is the viewer-facing one turned round: each of its
    // columns is the other's mirrored, its marks on their props' east cells.
    let cols = |which: Desk| {
        let art = pack.desk(which).base();
        let bulb = bulb_at(desk, which, &pack, RenderScale::ONE).x - desk.x;
        (
            art.sprite.first().width(),
            [
                art.props[DeskProp::Cup].0,
                art.props[DeskProp::Tower].0,
                bulb,
            ],
        )
    };
    let ((w, south), (_, north)) = (cols(Desk::South), cols(Desk::North));
    for (s, n) in south.into_iter().zip(north) {
        assert_eq!(
            s + n,
            w - 1,
            "the back-turned desk mirrors {south:?} as {north:?}"
        );
    }
}

/// A desk's cup stands on its sitter's side of the monitor: behind it as they
/// face the viewer, before it once they turn their back.
#[test]
fn the_cup_stands_on_the_sitters_side() {
    let pack = test_office();
    for scale in [
        RenderScale::ONE,
        RenderScale::from(pack.max_density_variant()),
    ] {
        for which in [Desk::North, Desk::South] {
            let (desk, density, blit_at) = pack.desk(which).at(scale);
            let f = crate::pack::DenseFrame::of(
                pack.piece(which.piece()),
                &desk.sprite,
                0,
                (density, blit_at),
            );
            let w = usize::from(f.frame.width());
            let monitor_foot = crate::pack::drawn_in(&f, &crate::pack::MONITOR_KEYS)
                .iter()
                .rposition(|&m| m)
                .map(|i| i / w)
                .expect("a monitor") as u16;
            let cup = desk.props[DeskProp::Cup].1;
            let behind = which == Desk::South;
            assert_eq!(
                cup < monitor_foot,
                behind,
                "{} at {scale:?}: its cup's foot row {cup}, the monitor's {monitor_foot}",
                which.piece().name()
            );
        }
    }
}

/// The classic lights each desk's lamp where its 1x art draws the bulb, in
/// either facing.
#[test]
fn the_classic_lamp_pool_centres_on_the_1x_bulb() {
    use crate::layout::{Facing, Point};
    let pack = test_office();
    let desk = Point { x: 40, y: 30 };
    for facing in [Facing::North, Facing::South] {
        let bulb = crate::lighting::DeskBulbs::of(&pack).at(facing);
        let lights = crate::lighting::DeskLights::new(desk, bulb, 1.0, 0.0);
        let crate::lighting::Light::Halo { centre, .. } = lights.lamp.light else {
            panic!("a desk lamp throws a halo");
        };
        assert_eq!(
            centre,
            bulb_at(desk, Desk::facing(facing), &pack, RenderScale::ONE),
            "{facing:?}'s pool is off its bulb"
        );
    }
}

/// A frame composed on the inputs the last one's windows and lights were
/// resolved from takes them from the memo; a beat later resolves the windows
/// afresh.
#[test]
fn a_frame_on_unchanged_inputs_reuses_the_last_frames_windows_and_lights() {
    use std::sync::Arc;
    let pack = test_office();
    let layout = SceneLayout::compute_with_seed(160, 96, None, 0).expect("lays out");
    let office = Office {
        layout: &layout,
        pack: &pack,
        theme: &crate::theme::NORMAL,
        scale: RenderScale::ONE,
    };
    let frame = empty_frame(&layout);
    let (mut lights, mut outside) = (
        LightCache::default(),
        crate::outside::OutsideCache::default(),
    );
    let mut views = |now| {
        let sky = crate::sky::Sky::at_with(now, crate::sky::Weather::Clear);
        let list = compose_at(
            &frame,
            office,
            &Moment::resolve(sky, office.theme, 0.0, Motion::Full.timing(now)),
            crate::floor::FloorMeta::ground(),
            quiet_board(),
            (&mut lights, &mut outside),
        );
        let windows: Vec<_> = list
            .pieces()
            .iter()
            .filter_map(|p| match &p.kind {
                PieceKind::Window { view, .. } => Some(Arc::clone(view)),
                _ => None,
            })
            .collect();
        let lights: Vec<_> = list.lights().iter().map(|l| Arc::clone(&l.view)).collect();
        (windows, lights)
    };
    fn same<T>(a: &[Arc<T>], b: &[Arc<T>]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| Arc::ptr_eq(a, b))
    }
    let night = crate::localclock::at_hour(23);
    let (windows, lights) = views(night);
    assert!(
        !windows.is_empty() && !lights.is_empty(),
        "the office has windows and lights"
    );
    let (again_windows, again_lights) = views(night);
    assert!(
        same(&windows, &again_windows),
        "the windows were resolved again"
    );
    assert!(
        same(&lights, &again_lights),
        "the lights were resolved again"
    );
    let (later, _) = views(night + std::time::Duration::from_secs(1));
    assert!(
        later
            .iter()
            .all(|v| windows.iter().all(|w| !Arc::ptr_eq(v, w))),
        "a beat later took the last beat's windows"
    );
}
