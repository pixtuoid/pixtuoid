//! Y-sorted drawable enum (painter's algorithm).
//!
//! Every mid-ground entity carries a `sort_row` = the y-pixel row where it
//! touches the floor (front-facing bottom edge for items with thickness).
//! Drawables sort ascending by `sort_row` and then [`Layer`], so larger
//! `sort_row` = closer to camera = paints last. A backdrop fixture is a
//! [`DrawableKind`] the background pass paints flat instead.

use std::time::SystemTime;

use crate::pack::OfficeArt;
use pixtuoid_core::sprite::blit::blit_frame;
use pixtuoid_core::sprite::format::Piece;
use pixtuoid_core::sprite::{Frame, Rgb, RgbBuffer};

use crate::sim::DeskProps;
use pixtuoid_core::AgentSlot;

use super::background::{paint_clock, paint_corridor_runner, paint_neon_panel};
use super::effects::{paint_effects, paint_screen_glow, paint_screen_idle};
use super::furniture::{
    paint_area_rug, paint_coat_rack, paint_doormat, paint_fish_tank, paint_kitchen_island,
    paint_meeting_chair, paint_notice_board, paint_side_table, paint_trash_bin, paint_water_cooler,
};
use crate::character::{CharacterFrame, SpritePose, character_frame};
pub(super) use crate::display::Layer;
use crate::effects::{Effect, STEAM_PUFFS};
use crate::frame_cache::FrameCache;
use crate::layout::{Pivot, Point, SceneLayout, Size, anchored_top_left};
use crate::pack::{Desk, DeskProp, desk_art, desk_art_top};
use crate::render_scale::RenderScale;

/// Coffee-steam plume column offset from the pantry sprite CENTER (`pos.x`), per
/// size — hand-tuned to the sprite art so the steam sits within the coffee
/// machine ([`SceneLayout::coffee_machine`](crate::layout::SceneLayout::coffee_machine)).
const PANTRY_STEAM_DX_LARGE: i16 = -2;
const PANTRY_STEAM_DX_SMALL: i16 = 1;

/// The steam offset for `anim`, a [`pantry_counter_anim`](crate::layout::pantry_counter_anim) pick.
fn pantry_steam_dx(anim: Piece) -> i16 {
    let [_, large] = crate::layout::PANTRY_COUNTER_ANIMS;
    if anim == large {
        PANTRY_STEAM_DX_LARGE
    } else {
        PANTRY_STEAM_DX_SMALL
    }
}

/// Where the steam rises from a pantry counter centred at `pos` drawn as
/// `anim`.
pub(super) fn pantry_steam_at(pos: Point, anim: Piece) -> Point {
    Point {
        x: (i32::from(pos.x) + i32::from(pantry_steam_dx(anim))).max(0) as u16,
        y: pos.y.saturating_sub(2),
    }
}

pub(super) struct Drawable<'a> {
    pub(super) sort_row: u16,
    pub(super) layer: Layer,
    /// Set only on a figure.
    pub(super) hover: Option<crate::display::Hover>,
    pub(super) kind: DrawableKind<'a>,
}

/// Sorts `drawables` into paint order. Stable, so drawables tied on row and
/// layer keep their queue order: the roster's among fixtures.
pub(super) fn sort_drawables(drawables: &mut [Drawable<'_>]) {
    drawables.sort_by_key(|d| (d.sort_row, d.layer));
}

pub(super) enum DrawableKind<'a> {
    /// Whole cubicle as one z-unit, so it paints atomically at the desk's
    /// bottom-edge row.
    DeskCubicle {
        desk: Point,
        /// Which way this desk seats its occupant; picks the art ([`Desk::facing`]).
        facing: crate::layout::Facing,
        screen_glow: Option<Rgb>,
        lights: crate::lighting::DeskLights,
        props: DeskProps,
    },
    Character {
        agent: &'a AgentSlot,
        pose: SpritePose,
        top_left: Point,
        label_anchor: Point,
        effects: &'a [Effect],
    },
    FilingCabinet {
        pos: Point,
    },
    /// Office chair, sorted at its occupant's row in [`Layer::Over`], so it
    /// paints over them.
    DeskChair {
        pos: Point,
    },
    /// Pantry counter, with coffee steam attached so the steam rides above it
    /// in z-order. `anim` is [`pantry_counter_anim`](crate::layout::pantry_counter_anim)'s pick.
    WaypointPantry {
        pos: Point,
        anim: Piece,
        steam: [Effect; STEAM_PUFFS],
    },
    MeetingSofa {
        pos: Point,
        /// Flip the art top-to-bottom. `meeting_sofa` draws its backrest in its
        /// top rows, so a mirrored sofa's back is its SOUTH edge and it seats
        /// people facing north: the south sofa of a meeting trio, and the
        /// lounge couch facing the windows.
        mirrored: bool,
    },
    MeetingTable {
        pos: Point,
    },
    /// A rug or mat filling its box, under the furniture that stands on it.
    AreaRug(crate::layout::Bounds),
    /// Lounge side table (wood + magazine), centred at `pos`.
    LoungeSideTable {
        pos: Point,
    },
    /// Kitchen-island body, centred at `pos`.
    KitchenIsland {
        pos: Point,
    },
    /// Snack shelf, centred at `pos` — the tall shelf overhangs its shallow
    /// 2-row base (walk-behind class).
    SnackShelf {
        pos: Point,
    },
    Plant {
        kind: crate::layout::PlantKind,
        pos: Point,
    },
    PodDecorItem {
        kind: crate::layout::PodDecor,
        pos: Point,
    },
    FloorLamp {
        pos: Point,
    },
    Door {
        pos: Point,
        /// Frame index into the `door` animation: 0 = closed, 1 = half-open,
        /// 2 = fully open.
        frame_idx: usize,
    },
    WallDecor {
        kind: crate::layout::WallDecor,
        pos: Point,
    },
    /// A corridor appliance: its pack art, centred at `pos`.
    Appliance {
        pos: Point,
        sprite: Piece,
        /// An agent stands here this frame: the art plays its busy loop.
        busy: bool,
    },
    Pet {
        pos: Point,
        flip: bool,
        anim_name: Piece,
        frame_idx: usize,
        effects: &'a [Effect],
    },
    /// The gateway lobster mascot — a presence-gated wandering creature, NOT an
    /// agent (lives in `daemons`, not `scene.agents`); y-sorted at its south row
    /// like a pet.
    GatewayMascot {
        pos: Point,
        anim_name: Piece,
        frame_idx: usize,
        effects: &'a [Effect],
        /// Gateway up but model-broken → render the lobster sickly red.
        degraded: bool,
    },
    /// One [sort band](crate::layout::WallPiece::sort_bands) of a glass room
    /// wall, so it composites over a character standing behind it.
    RoomWall {
        piece: crate::layout::WallPiece,
        rows: std::ops::Range<u16>,
    },
    /// Meeting-room coat rack, y-sorted at its base row (the bottom of
    /// `coat_rack_rect_at` its pole top). `pos` is the pole top.
    CoatRack {
        pos: Point,
    },
    /// Lounge aquarium, y-sorted at its cabinet's south row. `pos` is the sprite
    /// CENTER (matches the mask stamp's `Pivot::Center`).
    FishTank {
        pos: Point,
    },
    /// Head-of-table meeting chair, sorted at its sitter's row in
    /// [`Layer::Under`], so the sitter paints over it.
    MeetingChair {
        pos: Point,
        back_west: bool,
    },
    WaterCooler(crate::layout::Bounds),
    TrashBin(crate::layout::Bounds),
    Doormat(crate::layout::Bounds),
    NoticeBoard(crate::layout::Bounds),
    Runner(crate::layout::Bounds),
    /// The neon sign's panel in this frame's colours.
    NeonSign {
        panel: crate::layout::Bounds,
        look: crate::floor::NeonLook,
    },
    Clock {
        pos: Point,
    },
}

/// Blit `frame` centred on `pos` by [`anchored_top_left`], as its hover is
/// placed.
///
/// Layout units ARE buffer pixels in this pass, so the centring is plain
/// integer arithmetic. If a scaled classic painter is ever built, halve the
/// sprite in LOGICAL space and convert AFTERWARDS — halving an already-scaled
/// width drifts odd-width art half a logical unit off the footprint its mask
/// stamped, and no scale-1 test can see it.
fn blit_centered(frame: &Frame, pos: Point, buf: &mut RgbBuffer) {
    let at = anchored_top_left(Pivot::Center, pos, frame.width(), frame.height());
    blit_frame(frame, at.x, at.y, buf);
}

/// Look up `anim_name`, take its FIRST frame, and [`blit_centered`] it on `pos`.
fn blit_centered_first_frame(pack: &OfficeArt, anim_name: Piece, pos: Point, buf: &mut RgbBuffer) {
    blit_centered(pack.piece(anim_name).first(), pos, buf);
}

/// The subset of `PaintCtx` a [`Drawable`] arm paints with.
pub(super) struct DrawableCtx<'a> {
    pub buf: &'a mut RgbBuffer,
    pub pack: &'a OfficeArt,
    pub cache: &'a mut FrameCache,
    pub timing: crate::anim::Timing,
    pub theme: &'a crate::theme::Theme,
}

/// Dispatch one Drawable's paint; character-attached effects paint inline so
/// they ride along with the character in z-order.
pub(super) fn paint_drawable(kind: &DrawableKind<'_>, c: &mut DrawableCtx<'_>) {
    let buf = &mut *c.buf;
    let cache = &mut *c.cache;
    let (pack, theme) = (c.pack, c.theme);
    let crate::anim::Timing { now, beat } = c.timing;
    match kind {
        DrawableKind::DeskCubicle {
            desk,
            facing,
            screen_glow,
            lights,
            props,
        } => {
            let art = desk_art(pack, *facing);
            // The effects address the monitor by the sprite's OWN row numbering, so this is
            // the blit origin; passing `sprite_top + DESK_BEZEL_RAISE` caps every glow with a bar.
            let sprite_top = desk_art_top(pack, desk.y, art.height());
            blit_frame(art, desk.x, sprite_top, buf);
            paint_desk_props(buf, (*desk, *facing), props, pack, theme);
            if let Some(front) = Desk::facing(*facing).front() {
                blit_frame(pack.piece(front).first(), desk.x, sprite_top, buf);
            }
            // The desk's light falls on the whole group, its front and props
            // included, as the cutaway lights every piece after painting it.
            paint_desk_lamp_pool(buf, lights, theme);
            paint_screen_idle(
                buf,
                desk.x,
                sprite_top,
                theme.effects.monitor_idle,
                lights.screen_idle,
            );
            if let Some(tint) = screen_glow {
                paint_screen_glow(buf, desk.x, sprite_top, props.scanline, *tint, theme);
            }
        }
        DrawableKind::Character {
            agent,
            pose,
            top_left,
            effects,
            ..
        } => {
            paint_effects(buf, effects.iter().filter(|e| e.kind.beneath()), theme);
            paint_character_at(buf, *pose, *top_left, agent, pack, cache, now);
            paint_effects(buf, effects.iter().filter(|e| !e.kind.beneath()), theme);
        }
        DrawableKind::FilingCabinet { pos } => {
            blit_frame(pack.piece(Piece::FilingCabinet).first(), pos.x, pos.y, buf);
        }
        DrawableKind::DeskChair { pos } => paint_chair_back(buf, *pos, pack),
        DrawableKind::WaypointPantry { pos, anim, steam } => {
            // A character behind the counter is occluded by the counter's own
            // sprite (it y-sorts at the south base, and the mask south-anchors a
            // shallow strip there) — no synthetic cap needed.
            blit_centered_first_frame(pack, *anim, *pos, buf);
            paint_effects(buf, steam, theme);
        }
        DrawableKind::MeetingSofa { pos, mirrored } => {
            let f = pack.piece(Piece::MeetingSofa).first();
            if *mirrored {
                blit_centered(&f.mirror_vertical(), *pos, buf);
            } else {
                blit_centered(f, *pos, buf);
            }
        }
        DrawableKind::MeetingTable { pos } => {
            blit_centered_first_frame(pack, Piece::MeetingTable, *pos, buf);
        }
        DrawableKind::AreaRug(rug) => paint_area_rug(buf, *rug, theme),
        DrawableKind::LoungeSideTable { pos } => {
            paint_side_table(buf, pos.x, pos.y, theme);
        }
        DrawableKind::KitchenIsland { pos } => {
            paint_kitchen_island(buf, pos.x, pos.y, theme);
        }
        DrawableKind::SnackShelf { pos } => {
            blit_centered_first_frame(pack, Piece::SnackShelf, *pos, buf);
        }
        DrawableKind::Plant { kind, pos } => {
            // Occlusion is the sprite's own job: the foliage overhangs north of
            // the mask's shallow south-anchored pot strip, so it hides a walker
            // parked behind it. No synthetic back-cap.
            blit_centered_first_frame(pack, kind.piece(), *pos, buf);
        }
        DrawableKind::PodDecorItem { kind, pos } => {
            blit_centered_first_frame(pack, kind.piece(), *pos, buf);
        }
        DrawableKind::FloorLamp { pos } => {
            blit_centered_first_frame(pack, Piece::FloorLamp, *pos, buf);
        }
        DrawableKind::Door { pos, frame_idx } => {
            blit_frame(
                pack.piece(Piece::Door).frame_at(*frame_idx),
                pos.x,
                pos.y,
                buf,
            );
        }
        DrawableKind::WallDecor { kind, pos } => {
            blit_frame(pack.piece(kind.piece()).first(), pos.x, pos.y, buf);
        }
        DrawableKind::Appliance { pos, sprite, busy } => {
            let anim = pack.piece(*sprite);
            let art = anim.recolorable_at(crate::pack::appliance_frame_index(anim, *busy, beat));
            let themed = art.recolored(&crate::pack::appliance_overrides(&theme.appliance));
            blit_centered(&themed, *pos, buf);
        }
        DrawableKind::Pet {
            pos,
            flip,
            anim_name,
            frame_idx,
            effects,
        } => {
            let frame = pack.piece(*anim_name).frame_at(*frame_idx);
            // Declared out here so the flipped path's temporary outlives the `if`.
            let mirrored;
            let final_frame = if *flip {
                mirrored = frame.mirror_horizontal();
                &mirrored
            } else {
                frame
            };
            blit_centered(final_frame, *pos, buf);
            paint_effects(buf, *effects, theme);
        }
        DrawableKind::GatewayMascot {
            pos,
            anim_name,
            frame_idx,
            effects,
            degraded,
        } => {
            let frame = pack.piece(*anim_name).frame_at(*frame_idx);
            if *degraded {
                blit_centered(&super::palette::degraded_frame(frame), *pos, buf);
            } else {
                blit_centered(frame, *pos, buf);
            }
            paint_effects(buf, *effects, theme);
        }
        DrawableKind::RoomWall { piece, rows } => {
            crate::cutaway::wall::paint_wall(
                buf,
                crate::glass::WallTrim::of(theme),
                *piece,
                rows.clone(),
                crate::display::pen::Pen::UNIT,
            );
        }
        DrawableKind::FishTank { pos } => {
            paint_fish_tank(buf, *pos, beat, theme);
        }
        DrawableKind::MeetingChair { pos, back_west } => {
            paint_meeting_chair(buf, *pos, *back_west, theme);
        }
        DrawableKind::CoatRack { pos } => {
            paint_coat_rack(buf, *pos, theme);
        }
        DrawableKind::WaterCooler(cooler) => paint_water_cooler(buf, *cooler, beat, theme),
        DrawableKind::TrashBin(bin) => paint_trash_bin(buf, *bin),
        DrawableKind::Doormat(mat) => paint_doormat(buf, *mat, theme),
        DrawableKind::NoticeBoard(board) => paint_notice_board(buf, *board, theme),
        DrawableKind::Runner(runner) => paint_corridor_runner(buf, *runner, theme),
        DrawableKind::NeonSign { panel, look } => {
            paint_neon_panel(buf, panel.x, panel.y, panel.width, panel.height, look);
        }
        DrawableKind::Clock { pos } => paint_clock(buf, pos.x, pos.y, now, theme),
    }
}

/// The props on the 1x desk at `desk` facing its facing, each from the pack in
/// the theme's cup and paper at the desk art's mark for it: the cup, the
/// steam riding on it, then the token tower at its tier with the sheet falling
/// onto it.
fn paint_desk_props(
    buf: &mut RgbBuffer,
    (desk, facing): (Point, crate::layout::Facing),
    props: &DeskProps,
    pack: &OfficeArt,
    theme: &crate::theme::Theme,
) {
    let overrides = crate::pack::desk_prop_overrides(theme);
    // Frame `frame` of `sprite` stood on `at`, turned as its desk turns it; its
    // top row.
    let stand = |buf: &mut RgbBuffer, sprite: Piece, frame: usize, at: crate::pack::PropMark| {
        let f = crate::pack::densest_frame(pack, sprite, frame, RenderScale::ONE);
        let x = at.left(f.frame.width())?;
        let y = at.at.y.checked_sub(f.frame.height())?;
        let art = f.recolorable.recolored(&overrides);
        let art = if at.mirrored {
            art.mirror_horizontal()
        } else {
            art
        };
        blit_frame(&art, x, y, buf);
        Some(y)
    };
    let mark = |prop| crate::pack::desk_mark(pack, desk, facing, prop);
    if props.cup.is_some() {
        stand(buf, Piece::DeskCup, 0, mark(DeskProp::Cup));
    }
    paint_effects(buf, &props.effects, theme);
    let Some(tier) = usize::from(props.token_tier).checked_sub(1) else {
        return;
    };
    let at = mark(DeskProp::Tower);
    let Some(top) = stand(buf, Piece::TokenTower, tier, at) else {
        return;
    };
    // The sheet lands as the pile's next sheet: at its full fall it is gone,
    // and one still above the canvas's top is not drawn.
    if let Some(foot) = props
        .sheet_fall
        .and_then(|fallen| crate::token_meter::SHEET_FALL_PX.checked_sub(fallen))
        .filter(|&rest| rest > 0)
        .and_then(|rest| top.checked_sub(rest - 1))
    {
        let at = crate::pack::PropMark {
            at: Point {
                x: at.at.x,
                y: foot,
            },
            ..at
        };
        stand(buf, Piece::TokenSheet, 0, at);
    }
}

/// The desk task chair's art — the ONE authority for its size, so the enqueue
/// site centres on what is actually drawn.
pub(super) fn desk_chair_frame(pack: &OfficeArt) -> &Frame {
    pack.piece(Piece::DeskChair).first()
}

/// Office-chair back, crossing a back-turned occupant's lower torso.
pub(super) fn paint_chair_back(buf: &mut RgbBuffer, top_left: Point, pack: &OfficeArt) {
    blit_frame(desk_chair_frame(pack), top_left.x, top_left.y, buf);
}

/// The task lamp's warm pool; the desk art draws the lamp itself.
pub(super) fn paint_desk_lamp_pool(
    buf: &mut RgbBuffer,
    lights: &crate::lighting::DeskLights,
    theme: &crate::theme::Theme,
) {
    if lights.lamp.strength <= 0.0 {
        return;
    }
    super::background::paint_light(buf, &lights.lamp, theme.lighting.desk_lamp);
}

/// Paint a character at a top-left with per-agent recolor, returning
/// the size of the frame it drew.
pub(crate) fn paint_character_at(
    buf: &mut RgbBuffer,
    pose: SpritePose,
    top_left: Point,
    agent: &AgentSlot,
    pack: &OfficeArt,
    cache: &mut FrameCache,
    now: SystemTime,
) -> Size {
    let CharacterFrame {
        frame: cached,
        blit_at: _,
        rise: _,
    } = character_frame(
        pose,
        agent,
        pack,
        crate::render_scale::RenderScale::ONE,
        cache,
        now,
    );
    let size = Size {
        w: cached.width(),
        h: cached.height(),
    };
    blit_frame(cached, top_left.x, top_left.y, buf);
    size
}

/// Queue every room wall's bands into the y-sort, emitted after the fixtures so
/// the glass also covers a chair tied with a band's row.
pub(super) fn enqueue_room_walls<'a>(layout: &'a SceneLayout, drawables: &mut Vec<Drawable<'a>>) {
    for &piece in &layout.wall_pieces {
        for (rows, depth) in piece.sort_bands() {
            drawables.push(Drawable {
                sort_row: depth,
                // A character tied with a band's row still paints behind the glass.
                layer: Layer::Over,
                hover: None,
                kind: DrawableKind::RoomWall { piece, rows },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Motion;
    use crate::layout::{DESK_W, Facing};
    use crate::pet::PetKind;
    use crate::sim::Cup;

    /// The 1x token tower's width and rows a tier, as its art draws them.
    const STACK_W: u16 = 3;
    const STACK_PX_PER_TIER: u16 = 2;

    /// Where the 1x south desk at `desk` stands its tower: its west column and
    /// its base row.
    fn tower_base(pack: &OfficeArt, desk: Point) -> Point {
        let foot = crate::pack::desk_mark(pack, desk, Facing::South, DeskProp::Tower).at;
        Point {
            x: foot.x,
            y: foot.y - 1,
        }
    }

    /// The 1x tower and sheet art: a frame per tier up to
    /// [`MAX_TIER`](crate::token_meter::MAX_TIER), each [`STACK_PX_PER_TIER`]
    /// rows a tier and [`STACK_W`] wide, the full tower a column wider for its
    /// teeter.
    #[test]
    fn the_1x_token_art_is_the_classics_stack() {
        let pack = crate::pack::test_office();
        let tower = pack.piece(Piece::TokenTower);
        let max = crate::token_meter::MAX_TIER;
        assert_eq!(tower.frames().len(), usize::from(max));
        for (tier, f) in (1..=max).zip(tower.frames()) {
            let teeter = u16::from(tier == max);
            assert_eq!(
                (f.width(), f.height()),
                (STACK_W + 1, u16::from(tier) * STACK_PX_PER_TIER),
                "tier {tier}"
            );
            let drawn = |x: u16, y: u16| f.get(x, y).copied().flatten().is_some();
            assert!(
                (0..f.height()).all(|y| {
                    let dx = if y == 0 { teeter } else { 0 };
                    (0..f.width()).all(|x| drawn(x, y) == (dx..dx + STACK_W).contains(&x))
                }),
                "tier {tier} is not {STACK_W} wide with its teeter"
            );
        }
        let sheet = pack.piece(Piece::TokenSheet).first();
        assert_eq!((sheet.width(), sheet.height()), (STACK_W, 1));
    }

    #[test]
    fn steam_anchor_sits_within_the_coffee_machine_columns() {
        let pack = crate::pack::test_office();
        let width = |piece: Piece| pack.piece(piece).first().width() as i16;
        // steam_x = pos.x + steam_dx; sprite_x = pos.x - cw/2 → sprite-local
        // steam col = steam_dx + cw/2.
        let large_w = crate::layout::PANTRY_COUNTER_LARGE_W;
        for counter_w in [large_w, large_w - 1] {
            let (lo, hi) = crate::layout::coffee_machine_cols(counter_w);
            let anim = crate::layout::pantry_counter_anim(counter_w);
            let steam_col = pantry_steam_dx(anim) + width(anim) / 2;
            assert!(
                steam_col >= lo as i16 && steam_col < hi as i16,
                "steam col {steam_col} must sit within the machine cols [{lo},{hi})"
            );
        }
    }

    fn test_pack() -> OfficeArt {
        crate::pack::test_office()
    }

    fn desk_cubicle_drawable(
        desk: Point,
        token_tier: u8,
        sheet_fall: Option<u16>,
    ) -> Drawable<'static> {
        Drawable {
            sort_row: desk.y
                + crate::layout::furniture_def(crate::layout::Furniture::Desk)
                    .visual
                    .h,
            layer: Layer::Under,
            hover: None,
            kind: DrawableKind::DeskCubicle {
                desk,
                facing: crate::layout::Facing::South,
                screen_glow: None,
                lights: crate::lighting::DeskLights::new(desk, (0, 0), 0.0, 0.0),
                props: DeskProps {
                    cup: None,
                    token_tier,
                    sheet_fall,
                    effects: Vec::new(),
                    scanline: 0,
                },
            },
        }
    }

    fn paper_pixel_count(buf: &RgbBuffer, th: &crate::theme::Theme) -> usize {
        let mut n = 0;
        for y in 0..buf.height() {
            for x in 0..buf.width() {
                let c = buf.get(x, y);
                if c == th.furniture.paper || c == th.furniture.paper_shade {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn only_a_steaming_cup_steams() {
        let pack = test_pack();
        let th = theme();
        let bg = Rgb { r: 1, g: 2, b: 3 };
        let desk = Point { x: 20, y: 30 };
        let render = |cup, ms| {
            let mut buf = RgbBuffer::filled(60, 60, bg);
            let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(ms);
            let at = crate::sim::desk_cup_at(&pack, desk, Facing::South);
            let props = DeskProps {
                cup,
                token_tier: 0,
                sheet_fall: None,
                effects: crate::sim::cup_effects(at, cup, Motion::Full.beat(now)),
                scanline: 0,
            };
            paint_desk_props(&mut buf, (desk, Facing::South), &props, &pack, th);
            buf.as_slice().iter().filter(|&&c| c != bg).count()
        };
        let instants = (0..20u64).map(|i| i * 97);
        assert!(
            instants.clone().all(|ms| render(Some(Cup::Cold), ms) == 4),
            "a cold cup paints its four cells and nothing above"
        );
        assert!(
            instants
                .clone()
                .any(|ms| render(Some(Cup::Steaming), ms) > 4),
            "a fresh cup steams"
        );
        assert!(instants.clone().all(|ms| render(None, ms) == 0));
    }

    #[test]
    fn tier_zero_desk_paints_no_paper() {
        let pack = test_pack();
        let mut cache = FrameCache::new();
        let th = theme();
        let mut buf = RgbBuffer::filled(120, 80, Rgb { r: 1, g: 2, b: 3 });
        let d = desk_cubicle_drawable(Point { x: 40, y: 30 }, 0, None);
        paint_drawable(
            &d.kind,
            &mut DrawableCtx {
                buf: &mut buf,
                pack: &pack,
                cache: &mut cache,
                timing: Motion::Full.timing(SystemTime::UNIX_EPOCH),
                theme: th,
            },
        );
        assert_eq!(paper_pixel_count(&buf, th), 0);
        // …including a mid-fall sheet: a big EARLY reading can clear the sheet
        // minimum before cumulative usage reaches T1.
        let mut cache = FrameCache::new();
        let mut buf = RgbBuffer::filled(120, 80, Rgb { r: 1, g: 2, b: 3 });
        let d = desk_cubicle_drawable(Point { x: 40, y: 30 }, 0, Some(2));
        paint_drawable(
            &d.kind,
            &mut DrawableCtx {
                buf: &mut buf,
                pack: &pack,
                cache: &mut cache,
                timing: Motion::Full.timing(SystemTime::UNIX_EPOCH),
                theme: th,
            },
        );
        assert_eq!(paper_pixel_count(&buf, th), 0);
    }

    /// No prop covers the monitor, in either facing, at any tier, cup or none:
    /// what stands behind it hides under the desk's front, what stands before it
    /// stands beside it, so its cells paint as the bare desk's.
    #[test]
    fn no_prop_covers_the_monitor_in_the_classic() {
        use crate::layout::Facing;
        let (pack, th) = (test_pack(), theme());
        let desk = Point { x: 40, y: 30 };
        for facing in [Facing::North, Facing::South] {
            let paint = |cup, token_tier| {
                let mut d = desk_cubicle_drawable(desk, token_tier, None);
                if let DrawableKind::DeskCubicle {
                    facing: f, props, ..
                } = &mut d.kind
                {
                    *f = facing;
                    props.cup = cup;
                }
                let mut buf = RgbBuffer::filled(120, 80, Rgb { r: 1, g: 2, b: 3 });
                paint_drawable(
                    &d.kind,
                    &mut DrawableCtx {
                        buf: &mut buf,
                        pack: &pack,
                        cache: &mut FrameCache::new(),
                        timing: Motion::Full.timing(SystemTime::UNIX_EPOCH),
                        theme: th,
                    },
                );
                buf
            };
            let name = Desk::facing(facing).piece().name();
            let art = crate::pack::densest_frame(
                &pack,
                Desk::facing(facing).piece(),
                0,
                RenderScale::ONE,
            );
            let w = usize::from(art.frame.width());
            let top = crate::pack::desk_art_top(&pack, desk.y, art.frame.height());
            let monitor: Vec<(u16, u16)> = crate::pack::drawn_in(&art, &crate::pack::MONITOR_KEYS)
                .iter()
                .enumerate()
                .filter(|&(_, &m)| m)
                .map(|(i, _)| (desk.x + (i % w) as u16, top + (i / w) as u16))
                .collect();
            assert!(!monitor.is_empty(), "{name} draws a monitor");
            let bare = paint(None, 0);
            for tier in 0..=crate::token_meter::MAX_TIER {
                for cup in [None, Some(crate::sim::Cup::Cold)] {
                    let buf = paint(cup, tier);
                    for &(x, y) in &monitor {
                        assert_eq!(
                            buf.get(x, y),
                            bare.get(x, y),
                            "{name}, tier {tier}, cup {cup:?}: a prop covers the monitor at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }

    /// A sheet that would fall from above the canvas's top is not drawn: a
    /// desk at the top of the buffer paints its tower as it would without one.
    #[test]
    fn a_sheet_above_the_canvas_is_not_drawn() {
        let (pack, th) = (test_pack(), theme());
        let paper = |sheet_fall| {
            let mut buf = RgbBuffer::filled(120, 80, Rgb { r: 1, g: 2, b: 3 });
            let d = desk_cubicle_drawable(Point { x: 40, y: 1 }, 1, sheet_fall);
            paint_drawable(
                &d.kind,
                &mut DrawableCtx {
                    buf: &mut buf,
                    pack: &pack,
                    cache: &mut FrameCache::new(),
                    timing: Motion::Full.timing(SystemTime::UNIX_EPOCH),
                    theme: th,
                },
            );
            paper_pixel_count(&buf, th)
        };
        assert_eq!(paper(Some(1)), paper(None));
    }

    /// The desk's lamp lights its front as it lights the desk: at night a
    /// monitor cell under the pool keeps its lit colour with the front drawn
    /// over it, as the desk's art alone, lit, shows it.
    #[test]
    fn the_lamp_pool_lights_the_desk_front() {
        use crate::layout::Facing;
        let (pack, th) = (test_pack(), theme());
        let desk = Point { x: 40, y: 30 };
        let bulb = crate::lighting::DeskBulbs::of(&pack).at(Facing::South);
        let lights = crate::lighting::DeskLights::new(desk, bulb, 1.0, 0.0);
        let mut d = desk_cubicle_drawable(desk, 0, None);
        if let DrawableKind::DeskCubicle { lights: l, .. } = &mut d.kind {
            *l = lights;
        }
        let fill = Rgb { r: 1, g: 2, b: 3 };
        let mut buf = RgbBuffer::filled(120, 80, fill);
        paint_drawable(
            &d.kind,
            &mut DrawableCtx {
                buf: &mut buf,
                pack: &pack,
                cache: &mut FrameCache::new(),
                timing: Motion::Full.timing(SystemTime::UNIX_EPOCH),
                theme: th,
            },
        );
        let art = crate::pack::densest_frame(&pack, Desk::South.piece(), 0, RenderScale::ONE);
        let top = crate::pack::desk_art_top(&pack, desk.y, art.frame.height());
        let mut lit = RgbBuffer::filled(120, 80, fill);
        blit_frame(art.frame, desk.x, top, &mut lit);
        paint_desk_lamp_pool(&mut lit, &lights, th);
        let front = pack.piece(Desk::South.front().expect("a front")).first();
        let mut pooled = 0;
        for y in 0..front.height() {
            for x in 0..front.width() {
                if front.get(x, y).copied().flatten().is_none() {
                    continue;
                }
                let (bx, by) = (desk.x + x, top + y);
                pooled +=
                    usize::from(art.frame.get(x, y).copied().flatten() != Some(lit.get(bx, by)));
                assert_eq!(
                    buf.get(bx, by),
                    lit.get(bx, by),
                    "the front at ({x}, {y}) lost its light"
                );
            }
        }
        assert!(pooled > 0, "the pool must reach the front");
    }

    #[test]
    fn token_stack_grows_two_px_per_tier_with_a_t3_teeter() {
        let pack = test_pack();
        let th = theme();
        let desk = Point { x: 40, y: 30 };
        let tower = tower_base(&pack, desk);
        let base_y = tower.y;
        let mut counts = Vec::new();
        for tier in 1..=3u8 {
            let mut cache = FrameCache::new();
            let mut buf = RgbBuffer::filled(120, 80, Rgb { r: 1, g: 2, b: 3 });
            let d = desk_cubicle_drawable(desk, tier, None);
            paint_drawable(
                &d.kind,
                &mut DrawableCtx {
                    buf: &mut buf,
                    pack: &pack,
                    cache: &mut cache,
                    timing: Motion::Full.timing(SystemTime::UNIX_EPOCH),
                    theme: th,
                },
            );
            counts.push(paper_pixel_count(&buf, th));
            for xoff in 0..STACK_W {
                assert_eq!(
                    buf.get(tower.x + xoff, base_y),
                    th.furniture.paper,
                    "tier {tier} base row col {xoff}"
                );
            }
            let top_y = base_y - (u16::from(tier) * STACK_PX_PER_TIER - 1);
            let above = buf.get(tower.x, top_y - 1);
            assert!(
                above != th.furniture.paper && above != th.furniture.paper_shade,
                "tier {tier} must top out at {top_y}"
            );
        }
        assert!(counts[0] < counts[1] && counts[1] < counts[2], "{counts:?}");
        let mut cache = FrameCache::new();
        let mut buf = RgbBuffer::filled(120, 80, Rgb { r: 1, g: 2, b: 3 });
        let d = desk_cubicle_drawable(desk, 3, None);
        paint_drawable(
            &d.kind,
            &mut DrawableCtx {
                buf: &mut buf,
                pack: &pack,
                cache: &mut cache,
                timing: Motion::Full.timing(SystemTime::UNIX_EPOCH),
                theme: th,
            },
        );
        let t3_top = base_y - (3 * STACK_PX_PER_TIER - 1);
        let overhang = buf.get(tower.x + STACK_W, t3_top);
        assert!(
            overhang == th.furniture.paper || overhang == th.furniture.paper_shade,
            "T3 top sheet must overhang 1px east, got {overhang:?}"
        );
    }

    #[test]
    fn falling_sheet_paints_above_the_stack_and_lands_silently() {
        let pack = test_pack();
        let th = theme();
        let desk = Point { x: 40, y: 30 };
        let tower = tower_base(&pack, desk);
        let base_y = tower.y;
        let stack_top = base_y - (STACK_PX_PER_TIER - 1);
        let mut cache = FrameCache::new();
        let mut buf = RgbBuffer::filled(120, 80, Rgb { r: 1, g: 2, b: 3 });
        let d = desk_cubicle_drawable(desk, 1, Some(2));
        paint_drawable(
            &d.kind,
            &mut DrawableCtx {
                buf: &mut buf,
                pack: &pack,
                cache: &mut cache,
                timing: Motion::Full.timing(SystemTime::UNIX_EPOCH),
                theme: th,
            },
        );
        let sy = stack_top - (crate::token_meter::SHEET_FALL_PX - 2);
        assert_eq!(buf.get(tower.x, sy), th.furniture.paper);
        let mut cache = FrameCache::new();
        let mut buf2 = RgbBuffer::filled(120, 80, Rgb { r: 1, g: 2, b: 3 });
        let d = desk_cubicle_drawable(desk, 1, Some(crate::token_meter::SHEET_FALL_PX));
        paint_drawable(
            &d.kind,
            &mut DrawableCtx {
                buf: &mut buf2,
                pack: &pack,
                cache: &mut cache,
                timing: Motion::Full.timing(SystemTime::UNIX_EPOCH),
                theme: th,
            },
        );
        for y in 0..stack_top {
            for xoff in 0..STACK_W {
                let c = buf2.get(tower.x + xoff, y);
                assert!(
                    c != th.furniture.paper && c != th.furniture.paper_shade,
                    "landed sheet must not linger at ({xoff},{y})"
                );
            }
        }
    }

    fn theme() -> &'static crate::theme::Theme {
        crate::theme::theme_by_name("normal").expect("theme")
    }

    #[test]
    fn a_desk_and_its_cabinet_paint_but_no_per_desk_bin() {
        let pack = test_pack();
        // Pin the ASSET removal, not just its pixels: with no `trash_bin` in
        // the pack, a re-added blit would be dead code.
        assert!(Piece::from_name("trash_bin").is_none());
        let mut cache = FrameCache::new();
        let now = SystemTime::UNIX_EPOCH;
        let layout = crate::layout::SceneLayout::compute(160, 120, None).expect("fits");
        let first = pixtuoid_core::state::FloorLocalDeskIndex(0);
        let desk = layout.home_desks[first.0].at;
        let cabinet = layout
            .fixtures()
            .find(|f| f.kind == crate::layout::FixtureKind::FilingCabinet(first))
            .expect("desk 0 stands a cabinet")
            .at;
        let cab = pack.piece(Piece::FilingCabinet).first();
        let bg = Rgb { r: 1, g: 2, b: 3 };
        let mut buf = RgbBuffer::filled(layout.buf_w, layout.buf_h, bg);
        for kind in [
            DrawableKind::FilingCabinet { pos: cabinet },
            DrawableKind::DeskCubicle {
                desk,
                facing: crate::layout::Facing::South,
                screen_glow: None,
                lights: crate::lighting::DeskLights::new(desk, (0, 0), 0.0, 0.0),
                props: DeskProps::default(),
            },
        ] {
            paint_drawable(
                &kind,
                &mut DrawableCtx {
                    buf: &mut buf,
                    pack: &pack,
                    cache: &mut cache,
                    timing: Motion::Full.timing(now),
                    theme: theme(),
                },
            );
        }
        let mut cab_painted = false;
        for dy in 0..cab.height() {
            for dx in 0..cab.width() {
                if buf.get(cabinet.x + dx, cabinet.y + dy) != bg {
                    cab_painted = true;
                }
            }
        }
        assert!(cab_painted, "filing cabinet should paint west of the desk");
        // The removed bin's chrome grey. The old bin cell may show the desk
        // sprite's own east columns, but never this.
        let bin_grey = Rgb {
            r: 0xa8,
            g: 0xa8,
            b: 0xb0,
        };
        for dy in 0..4u16 {
            for dx in 0..3u16 {
                assert_ne!(
                    buf.get(desk.x + DESK_W + dx, desk.y + 4 + dy),
                    bin_grey,
                    "no per-desk bin pixels at the old east-edge cell"
                );
            }
        }
    }

    #[test]
    fn meeting_sofa_mirrored_flips_vertically() {
        let pack = test_pack();
        let mut cache = FrameCache::new();
        let now = SystemTime::UNIX_EPOCH;
        let pos = Point { x: 30, y: 30 };
        let mut render = |mirrored: bool| {
            let mut buf = RgbBuffer::filled(80, 80, Rgb { r: 0, g: 0, b: 0 });
            let d = Drawable {
                sort_row: pos.y,
                layer: Layer::Under,
                hover: None,
                kind: DrawableKind::MeetingSofa { pos, mirrored },
            };
            paint_drawable(
                &d.kind,
                &mut DrawableCtx {
                    buf: &mut buf,
                    pack: &pack,
                    cache: &mut cache,
                    timing: Motion::Full.timing(now),
                    theme: theme(),
                },
            );
            buf
        };
        let plain = render(false);
        let flipped = render(true);
        let mut differs = false;
        for y in 0..80u16 {
            for x in 0..80u16 {
                if plain.get(x, y) != flipped.get(x, y) {
                    differs = true;
                }
            }
        }
        assert!(differs, "mirrored sofa must render distinct pixels");
    }

    #[test]
    fn pet_drawable_sleep_anim_paints_sleep_z() {
        let pack = test_pack();
        let mut cache = FrameCache::new();
        let now = SystemTime::UNIX_EPOCH;
        let pos = Point { x: 30, y: 40 };
        let mut render = |anim_name: Piece| {
            let mut buf = RgbBuffer::filled(60, 60, Rgb { r: 0, g: 0, b: 0 });
            let effects =
                crate::sim::pet_effects(PetKind::Cat, pos, anim_name, None, Motion::Full.beat(now));
            let d = Drawable {
                sort_row: pos.y,
                layer: Layer::Creature,
                hover: None,
                kind: DrawableKind::Pet {
                    pos,
                    flip: false,
                    anim_name,
                    frame_idx: 0,
                    effects: &effects,
                },
            };
            paint_drawable(
                &d.kind,
                &mut DrawableCtx {
                    buf: &mut buf,
                    pack: &pack,
                    cache: &mut cache,
                    timing: Motion::Full.timing(now),
                    theme: theme(),
                },
            );
            buf
        };
        let count_above = |buf: &RgbBuffer| {
            let mut n = 0u32;
            for y in 0..pos.y.saturating_sub(4) {
                for x in 0..60u16 {
                    if buf.get(x, y) != (Rgb { r: 0, g: 0, b: 0 }) {
                        n += 1;
                    }
                }
            }
            n
        };
        let sit = count_above(&render(PetKind::Cat.sit_anim()));
        let sleep = count_above(&render(PetKind::Cat.sleep_anim()));
        assert!(
            sleep > sit,
            "sleep anim must add floating z's above the pet (sleep={sleep}, sit={sit})"
        );
    }

    /// Paint the appliance `sprite` at rest, centred at `pos`, in `th`.
    fn appliance_at_rest(sprite: Piece, pos: Point, th: &crate::theme::Theme) -> RgbBuffer {
        let pack = test_pack();
        let mut cache = FrameCache::new();
        let mut buf = RgbBuffer::filled(80, 80, Rgb { r: 1, g: 2, b: 3 });
        let d = Drawable {
            sort_row: pos.y,
            layer: Layer::Under,
            hover: None,
            kind: DrawableKind::Appliance {
                pos,
                sprite,
                busy: false,
            },
        };
        paint_drawable(
            &d.kind,
            &mut DrawableCtx {
                buf: &mut buf,
                pack: &pack,
                cache: &mut cache,
                timing: Motion::Full.timing(SystemTime::UNIX_EPOCH),
                theme: th,
            },
        );
        buf
    }

    /// In every theme, each `appliance_overrides` role
    /// lands on the cells the 1x vending art draws in it.
    #[test]
    fn a_vending_machine_takes_its_themes_colours() {
        let pos = Point { x: 30, y: 30 };
        let vis = crate::layout::furniture_def(crate::layout::Furniture::VendingMachine).visual;
        let (vx, vy) = (pos.x - vis.w / 2, pos.y - vis.h / 2);
        for th in crate::theme::ALL_THEMES {
            let a = &th.appliance;
            let buf = appliance_at_rest(Piece::VendingMachine, pos, th);
            for ((dx, dy), want, role) in [
                ((0, 1), a.vending_panel, "the panel, under its lit top row"),
                ((1, 2), a.vending_drinks[0], "the first drink"),
                ((4, 2), a.vending_drinks[3], "the fourth drink"),
                ((5, 5), a.vending_trim, "the coin plate"),
                ((1, 10), a.vending_dark, "the pickup tray"),
                ((0, 2), a.vending_body, "the body"),
            ] {
                assert_eq!(buf.get(vx + dx, vy + dy), want, "{}: {role}", th.name);
            }
        }
    }

    /// See [`a_vending_machine_takes_its_themes_colours`].
    #[test]
    fn a_printer_takes_its_themes_colours() {
        let pos = Point { x: 30, y: 30 };
        let vis = crate::layout::furniture_def(crate::layout::Furniture::Printer).visual;
        let (px, py) = (pos.x - vis.w / 2, pos.y - vis.h / 2);
        for th in crate::theme::ALL_THEMES {
            let a = &th.appliance;
            let buf = appliance_at_rest(Piece::Printer, pos, th);
            for ((dx, dy), want, role) in [
                ((2, 1), a.printer_glass, "the scanner glass"),
                ((4, 0), a.printer_top, "the lid, east of its lit end"),
                ((2, 3), a.printer_paper, "the stack"),
                ((1, 2), a.printer_tray, "the output bay"),
                ((2, 4), a.printer_body, "the chassis"),
            ] {
                assert_eq!(buf.get(px + dx, py + dy), want, "{}: {role}", th.name);
            }
        }
    }

    #[test]
    fn blit_centered_lands_top_left_at_pos_minus_half_size() {
        // The 3×2 frame's ODD width pins the FLOOR division (3/2 == 1, not a
        // rounded 2) — a rounding change would move only this case, and the
        // "two renders differ" sofa/pet tests shift equally either way.
        let bg = Rgb { r: 0, g: 0, b: 0 };
        let marker = Rgb { r: 9, g: 8, b: 7 };
        let frame = Frame::from_pixels(3, 2, vec![Some(marker); 6]);
        let mut buf = RgbBuffer::filled(20, 20, bg);
        blit_centered(&frame, Point { x: 10, y: 10 }, &mut buf);
        assert_eq!(buf.get(9, 9), marker, "top-left lands at pos − size/2");
        assert_eq!(buf.get(11, 10), marker, "bottom-right at (9+2, 9+1)");
        assert_eq!(buf.get(8, 9), bg, "one column west of the frame stays bg");
        assert_eq!(buf.get(9, 8), bg, "one row north of the frame stays bg");
    }

    #[test]
    fn gateway_mascot_degraded_renders_distinct_pixels() {
        let pack = test_pack();
        let mut cache = FrameCache::new();
        let now = SystemTime::UNIX_EPOCH;
        let pos = Point { x: 30, y: 30 };
        let def =
            crate::creatures::gateway_mascot_def(pixtuoid_core::source::openclaw::SOURCE_NAME)
                .expect("openclaw mascot def");
        let black = Rgb { r: 0, g: 0, b: 0 };
        let mut render = |degraded: bool| {
            let mut buf = RgbBuffer::filled(80, 80, black);
            let d = Drawable {
                sort_row: pos.y,
                layer: Layer::Creature,
                hover: None,
                kind: DrawableKind::GatewayMascot {
                    pos,
                    anim_name: def.rest,
                    frame_idx: 0,
                    effects: &[],
                    degraded,
                },
            };
            paint_drawable(
                &d.kind,
                &mut DrawableCtx {
                    buf: &mut buf,
                    pack: &pack,
                    cache: &mut cache,
                    timing: Motion::Full.timing(now),
                    theme: theme(),
                },
            );
            buf
        };
        let plain = render(false);
        let degraded = render(true);
        let mut plain_painted = false;
        let mut differs = false;
        for y in 0..80u16 {
            for x in 0..80u16 {
                let pp = plain.get(x, y);
                if pp != black {
                    plain_painted = true;
                }
                if pp != degraded.get(x, y) {
                    differs = true;
                }
            }
        }
        assert!(plain_painted, "the plain lobster must actually render");
        assert!(
            differs,
            "the degraded lobster must render distinct (tinted) pixels vs the plain one"
        );
    }
}
