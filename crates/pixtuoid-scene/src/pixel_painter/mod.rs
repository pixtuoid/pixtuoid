//! Pure-pixel paint pass — no ratatui types, no terminal I/O.
//!
//! The classic's paint of a [`SimFrame`] the sim already stepped, which
//! [`look::render`](crate::look::render) calls.

use std::collections::HashMap;

use crate::pack::OfficeArt;
use pixtuoid_core::sprite::format::Density;
use pixtuoid_core::sprite::{Rgb, RgbBuffer};
use pixtuoid_core::{AgentSlot, SceneState};

use crate::display::{Badge, Hover, HoverTarget, Hovers, TextRun};
#[cfg(test)]
use crate::floor::VacancyDim;
use crate::frame_cache::FrameCache;
use crate::layout::{Depth, Facing, FixtureKind, Pivot, SceneLayout, StationArt};
use crate::sim::pack_frame_size;
use crate::walk::WalkState;

/// What [`paint_frame`] drew that the caller points at or badges.
#[derive(Debug, Default)]
pub(crate) struct Drawn {
    /// Each drawn agent's badge, in paint order.
    pub(crate) badges: Vec<Badge>,
    /// Each chitchat bubble, over its speaker's badge.
    pub(crate) bubbles: Vec<TextRun>,
    pub(crate) hovers: Hovers,
}

/// The classic's raster state for one floor, kept across frames.
#[derive(Debug)]
pub(crate) struct ClassicCaches {
    pub(crate) sprites: FrameCache,
    pub(crate) base_fill: BaseFillCache,
    pub(crate) shadows: crate::ground::DepthsCache,
}

impl ClassicCaches {
    pub(crate) fn new() -> Self {
        Self {
            sprites: FrameCache::new(),
            base_fill: BaseFillCache::new(),
            shadows: crate::ground::DepthsCache::default(),
        }
    }
}

mod ambient;
mod background;
mod debug_overlay;
pub(crate) mod drawable;
pub(crate) mod effects;
mod furniture;
pub(crate) mod palette;

/// Applies the hour's object terms to every pixel painted since `since`.
///
/// The branch-free XOR-OR reduction replaces a hard-to-predict per-pixel
/// branch with one predictable branch per `WASH_SCAN_CHUNK` on the clean
/// path, and the wash itself runs through a lazily-built [`palette::RgbLut`]
/// over the chunks that differ. Byte-identical to per-pixel [`wash_object`]
/// on the diff set.
fn wash_since(buf: &mut RgbBuffer, since: &RgbBuffer, wash: [(Rgb, f32); 2]) {
    const WASH_SCAN_CHUNK: usize = 64;
    let cur = buf.as_mut_slice();
    let old = since.as_slice();
    debug_assert_eq!(cur.len(), old.len());
    let mut lut: Option<palette::RgbLut> = None;
    for (cur_c, old_c) in cur
        .chunks_mut(WASH_SCAN_CHUNK)
        .zip(old.chunks(WASH_SCAN_CHUNK))
    {
        let differs = cur_c.iter().zip(old_c).fold(0u8, |acc, (a, b)| {
            acc | (a.r ^ b.r) | (a.g ^ b.g) | (a.b ^ b.b)
        });
        if differs != 0 {
            let lut =
                lut.get_or_insert_with(|| palette::RgbLut::tabulate(|c| wash_object(c, wash)));
            for (p, o) in cur_c.iter_mut().zip(old_c) {
                if *p != *o {
                    *p = lut.apply(*p);
                }
            }
        }
    }
}

/// Composes the hour's two object terms in the floor overlays' own order.
fn wash_object(painted: Rgb, wash: [(Rgb, f32); 2]) -> Rgb {
    wash.into_iter().fold(painted, |c, (tint, a)| {
        if a > 0.0 {
            palette::blend_rgb(c, tint, a)
        } else {
            c
        }
    })
}
use crate::sim::{SimFrame, desk_occupant};
pub(crate) use background::BaseFillCache;
#[cfg(test)]
pub(crate) use furniture::paint_area_rug;

use crate::atmosphere::Moment;
use crate::lighting::{DeskLights, LightInputs, Lights};
use background::{
    paint_ground_and_walls, paint_ground_wash, paint_light, paint_shadows, paint_windows,
};
use drawable::{Drawable, DrawableKind, Layer, enqueue_room_walls, paint_drawable};

/// The paint pass's borrow set — everything `paint_frame` may touch. The only
/// `&mut`s are the pixel buffer and the paint-local caches; the sim stores are
/// absent BY TYPE (`walks` is an immutable view, read by the debug route
/// overlay), so painting cannot move the world.
pub(crate) struct PaintCtx<'a> {
    scene: &'a SceneState,
    layout: &'a SceneLayout,
    pack: &'a OfficeArt,
    /// Event ages and the wall clock, and the beat every ambient loop reads —
    /// every sky fact reads [`Self::sky`].
    timing: crate::anim::Timing,
    /// The sky on `timing`, sampled once for the whole pass.
    sky: crate::sky::Sky,
    /// The sky the windows look out on where a test parts it from the room's
    /// [`Self::sky`]; `None` for the room's.
    window_sky: Option<crate::sky::Sky>,
    buf: &'a mut RgbBuffer,
    cache: &'a mut FrameCache,
    base_fill: &'a mut background::BaseFillCache,
    shadows: &'a mut crate::ground::DepthsCache,
    theme: &'a crate::theme::Theme,
    floor: crate::floor::FloorMeta,
    walks: &'a HashMap<pixtuoid_core::AgentId, WalkState>,
    debug_walkable: bool,
    /// The office's outside, kept across frames.
    outside: &'a mut crate::outside::OutsideCache,
}

impl<'a> PaintCtx<'a> {
    /// The classic pass over `world` on `layout`, painting into `buf` with the
    /// floor's `caches` and the office's `outside`.
    pub(crate) fn classic(
        (world, pack): (crate::floor::FloorInputs<'a>, &'a OfficeArt),
        layout: &'a SceneLayout,
        theme: &'a crate::theme::Theme,
        (caches, outside): (&'a mut ClassicCaches, &'a mut crate::outside::OutsideCache),
        buf: &'a mut RgbBuffer,
        walks: &'a HashMap<pixtuoid_core::AgentId, WalkState>,
        debug_walkable: bool,
    ) -> Self {
        let timing = world.floor.motion.timing(world.now);
        Self {
            scene: world.scene,
            layout,
            pack,
            timing,
            sky: crate::sky::Sky::at(timing, world.floor.weather),
            window_sky: None,
            buf,
            cache: &mut caches.sprites,
            base_fill: &mut caches.base_fill,
            shadows: &mut caches.shadows,
            theme,
            floor: world.floor,
            walks,
            debug_walkable,
            outside,
        }
    }

    /// What of `frame` flashes under the sky this pass paints it in.
    pub(crate) fn flash(&self, frame: &SimFrame) -> crate::screen::FlashPhase {
        crate::screen::FlashPhase::of(&self.sky, frame)
    }

    /// The subset of the pass a [`Drawable`] paints with.
    fn drawable_ctx(&mut self) -> drawable::DrawableCtx<'_> {
        drawable::DrawableCtx {
            buf: &mut *self.buf,
            pack: self.pack,
            cache: &mut *self.cache,
            timing: self.timing,
            theme: self.theme,
        }
    }
}

/// The PAINT half of the frame: blit the world the sim already advanced. Every
/// positional/lifecycle decision was made in `sim_step` — this pass only
/// resolves presentation (theme colors, sprite pixels) and composites.
pub(crate) fn paint_frame(ctx: &mut PaintCtx<'_>, frame: &SimFrame) -> Drawn {
    let agents: &[AgentSlot] = &frame.agents;
    let buf_h = ctx.layout.buf_h;

    let moment = Moment::resolve(ctx.sky, ctx.theme, ctx.floor.altitude, ctx.timing);
    let look = &moment.look;
    let lights = Lights::of(
        ctx.layout,
        look,
        &LightInputs {
            agents,
            seated: &frame.seated_agents,
            floor_idx: ctx.floor.floor_idx,
            indoor_scale: frame.indoor_scale,
            neon: frame.neon,
            beat: ctx.timing.beat,
            bulbs: crate::lighting::DeskBulbs::of(ctx.pack),
        },
    );
    let top_wall_h = ctx.layout.wall_band_h();
    paint_ground_and_walls(ctx.base_fill, ctx.buf, top_wall_h, &moment, ctx.theme);
    let window_moment = ctx
        .window_sky
        .map(|sky| Moment::resolve(sky, ctx.theme, ctx.floor.altitude, ctx.timing));
    paint_windows(
        ctx.buf,
        top_wall_h,
        ctx.layout.window_bays(),
        window_moment.as_ref().unwrap_or(&moment),
        ctx.pack,
        ctx.theme,
        ctx.outside,
    );
    for spill in &lights.spills {
        paint_light(ctx.buf, spill, ctx.theme.lighting.sun_spill);
    }

    // An empty ground reads dark because its artificial lights go out with
    // `indoor_scale`, not because the GROUND takes a second darkening of its own.
    paint_ground_wash(ctx.buf, top_wall_h, buf_h, look.ground_wash);
    if let Some(lamp) = &lights.floor_lamp {
        paint_light(ctx.buf, lamp, ctx.theme.lighting.floor_lamp_halo);
    }

    let neon = crate::floor::neon_look(frame.neon, ctx.theme);
    let Furnishings {
        backdrop,
        sorted: mut drawables,
    } = queue_fixtures(ctx, frame, &lights.desks, neon);
    paint_backdrop(ctx, &backdrop, Wash::Spared);
    // The rest overwrite the floor, so the overlays above cannot reach them, and
    // they paint before the drawable snapshot, so that pass cannot either. Hence
    // their own wash, which also keeps the EMITTERS painted above (the floor-lamp
    // halo) out of it.
    let pre_floor_fixtures = ctx.buf.clone();
    paint_backdrop(ctx, &backdrop, Wash::Washed);
    wash_since(ctx.buf, &pre_floor_fixtures, look.object_wash);

    let shadow_strength = crate::ground::shadow_strength(look.darkness);
    let contacts = ctx.layout.fixtures().filter_map(|f| f.contact()).collect();
    paint_shadows(
        ctx.buf,
        ctx.shadows.cells(contacts, Density::ONE),
        shadow_strength,
        ctx.theme.office.shadow,
    );

    ambient::paint_ceiling_halos(ctx.buf, ctx.theme, &lights.monitor_halos);

    // Every entity gets a `sort_row` — its floor-touching row — so sorting
    // ascending and painting in order puts things closer to the camera in
    // front: the painter's algorithm on a top-down 2D scene.
    if let Some(pet) = &frame.pet {
        enqueue_pet(ctx.pack, pet, &mut drawables);
    }
    enqueue_gateway_mascots(ctx.pack, &frame.mascots, &mut drawables);
    enqueue_characters(ctx.pack, ctx.theme, frame, &mut drawables);
    enqueue_room_walls(ctx.layout, &mut drawables);
    drawable::sort_drawables(&mut drawables);
    let mut drawn = Drawn::default();
    let namesakes = crate::badge::Namesakes::of(ctx.scene.agents.values());
    // A per-pixel diff finds EXACTLY what the foreground wrote. AFTER
    // `paint_shadows`/`paint_ceiling_halos`: both already carry the hour, so folding
    // them in here would apply it twice.
    let pre_foreground = ctx.buf.clone();
    for d in drawables {
        paint_drawable(&d.kind, &mut ctx.drawable_ctx());
        let Some(hover) = d.hover else { continue };
        // Badged where hoverable: a badge, like a hover, needs its frame drawn.
        if let DrawableKind::Character {
            agent,
            label_anchor,
            ..
        } = d.kind
            && let Some(agent) = ctx.scene.agents.get(&agent.agent_id)
        {
            drawn
                .badges
                .push(Badge::new(label_anchor, agent, &namesakes, ctx.theme));
        }
        drawn.hovers.push(hover);
    }
    drawn.bubbles = frame
        .chitchat_bubbles
        .iter()
        .filter_map(|bubble| {
            let badge = drawn.badges.iter().find(|b| b.agent == bubble.speaker)?;
            Some(TextRun::bubble(bubble, badge.at, ctx.theme))
        })
        .collect();
    // The floor's day/night wash, over the foreground: the overlays above run
    // before any drawable exists, so nothing painted carries a time-of-day term.
    wash_since(ctx.buf, &pre_foreground, look.object_wash);

    // LATE, so the light lands ON the shelf and the clock instead of hiding
    // behind them; after the wash, since the sign is an emitter and its light
    // isn't dimmed with the room it falls on.
    paint_light(ctx.buf, &lights.neon, neon.halo);

    // LAST, so a Storm strike briefly flares the whole interior (floor, walls,
    // furniture, characters), not just the window strip.
    background::paint_lightning_flash(ctx.buf, &ctx.sky);

    if ctx.debug_walkable {
        debug_overlay::paint(ctx.buf, ctx.layout, ctx.scene, ctx.walks);
    }

    drawn
}

/// Map the sim's resolved [`crate::sim::CharacterPlacement`]s 1:1 onto y-sorted
/// drawables. The ONLY paint-side work is presentation — resolving the
/// theme-free [`CharacterGlow`](crate::sim::CharacterGlow) to a `Theme` color.
fn enqueue_characters<'a>(
    pack: &OfficeArt,
    theme: &crate::theme::Theme,
    frame: &'a SimFrame,
    drawables: &mut Vec<Drawable<'a>>,
) {
    for p in &frame.characters {
        let agent = &frame.agents[p.agent_idx];
        let pose = crate::character::SpritePose::of(p, agent, theme);
        drawables.push(Drawable {
            sort_row: p.sort_row,
            layer: Layer::Character,
            hover: Some(Hover::figure(
                Pivot::TopLeft,
                p.top_left,
                pack_frame_size(pack, pose.anim_name, pose.frame_idx),
                HoverTarget::Agent(agent.agent_id),
            )),
            kind: DrawableKind::Character {
                agent,
                pose,
                top_left: p.top_left,
                label_anchor: p.label_anchor,
                effects: &p.effects,
            },
        });
    }
}

/// The office pet, sorted on its [`sort_row`](crate::sim::PetPlacement::sort_row).
fn enqueue_pet<'a>(
    pack: &OfficeArt,
    pet: &'a crate::sim::PetPlacement,
    drawables: &mut Vec<Drawable<'a>>,
) {
    let pos = pet.pos;
    let size = pack_frame_size(pack, pet.anim_name, pet.frame_idx);
    drawables.push(Drawable {
        sort_row: pet.sort_row,
        layer: Layer::Creature,
        hover: Some(Hover::figure(Pivot::Center, pos, size, pet.target())),
        kind: DrawableKind::Pet {
            pos,
            flip: pet.flip,
            anim_name: pet.anim_name,
            frame_idx: pet.frame_idx,
            effects: &pet.effects,
        },
    });
}

fn enqueue_gateway_mascots<'a>(
    pack: &OfficeArt,
    mascots: &'a [crate::sim::MascotPlacement],
    drawables: &mut Vec<Drawable<'a>>,
) {
    for m in mascots {
        drawables.push(Drawable {
            sort_row: m.sort_row,
            layer: Layer::Creature,
            hover: m.target().map(|target| {
                Hover::figure(
                    Pivot::Center,
                    m.pos,
                    pack_frame_size(pack, m.anim_name, m.frame_idx),
                    target,
                )
            }),
            kind: DrawableKind::GatewayMascot {
                pos: m.pos,
                anim_name: m.anim_name,
                frame_idx: m.frame_idx,
                effects: &m.effects,
                degraded: m.degraded,
            },
        });
    }
}

/// Whether the hour's object wash reaches a fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wash {
    /// It lights itself, so the wash would dim a light source.
    Spared,
    /// It is lit by the room, and dims with it.
    Washed,
}

/// A fixture's [`Wash`]. Only the background pass can spare a fixture: the
/// foreground wash reaches everything the y-sort paints.
fn wash_of(kind: FixtureKind) -> Wash {
    match kind {
        FixtureKind::NeonSign | FixtureKind::Clock => Wash::Spared,
        FixtureKind::Desk(_)
        | FixtureKind::FilingCabinet(_)
        | FixtureKind::DeskChair(_)
        | FixtureKind::Station { .. }
        | FixtureKind::Plant { .. }
        | FixtureKind::Pod { .. }
        | FixtureKind::Wall { .. }
        | FixtureKind::MeetingRug { .. }
        | FixtureKind::MeetingSofa { .. }
        | FixtureKind::MeetingTable { .. }
        | FixtureKind::MeetingChair { .. }
        | FixtureKind::CoatRack { .. }
        | FixtureKind::Doormat { .. }
        | FixtureKind::NoticeBoard { .. }
        | FixtureKind::LoungeRug
        | FixtureKind::LoungeCouch
        | FixtureKind::SideTable
        | FixtureKind::FloorLamp
        | FixtureKind::FishTank
        | FixtureKind::KitchenIsland
        | FixtureKind::PantryMat
        | FixtureKind::IslandMat
        | FixtureKind::WaterCooler
        | FixtureKind::TrashBin
        | FixtureKind::Door
        | FixtureKind::Runner => Wash::Washed,
    }
}

/// The roster, as the classic paints it.
struct Furnishings<'a> {
    /// The [`Depth::Backdrop`] fixtures in roster order, each with its [`Wash`].
    backdrop: Vec<(Wash, DrawableKind<'a>)>,
    /// The rest, queued for the y-sort in roster order.
    sorted: Vec<Drawable<'a>>,
}

/// Every fixture [`SceneLayout::fixtures`](crate::layout::SceneLayout::fixtures)
/// yields, joined to this frame's live state on the ids its kind carries.
fn queue_fixtures<'a>(
    ctx: &PaintCtx<'_>,
    frame: &SimFrame,
    desk_lights: &[DeskLights],
    neon: crate::floor::NeonLook,
) -> Furnishings<'a> {
    let layout = ctx.layout;
    let mut out = Furnishings {
        backdrop: Vec::new(),
        sorted: Vec::new(),
    };
    for f in layout.fixtures() {
        let kind = match f.kind {
            FixtureKind::Desk(i) => {
                let facing = layout.home_desks[i.0].facing;
                DrawableKind::DeskCubicle {
                    desk: f.top_left(),
                    facing,
                    screen_glow: crate::lighting::desk_screen_glow(
                        desk_occupant(&frame.agents, i),
                        facing,
                        frame.seated_agents.get(&i).copied().unwrap_or(false),
                        ctx.theme,
                    ),
                    lights: desk_lights[i.0],
                    props: frame.desk(i),
                }
            }
            FixtureKind::FilingCabinet(_) => DrawableKind::FilingCabinet { pos: f.top_left() },
            FixtureKind::DeskChair(_) => DrawableKind::DeskChair { pos: f.top_left() },
            FixtureKind::Station { waypoint, station } => {
                let appliance = |sprite| DrawableKind::Appliance {
                    pos: f.at,
                    sprite,
                    busy: frame.occupied_waypoints.contains(&waypoint),
                };
                match station.art() {
                    StationArt::PantryCounter => {
                        let anim = crate::layout::pantry_counter_anim(f.visual.width);
                        DrawableKind::WaypointPantry {
                            pos: f.at,
                            anim,
                            // A fixture, which always steams: nothing for the sim to decide.
                            steam: crate::effects::steam(
                                drawable::pantry_steam_at(f.at, anim),
                                ctx.timing.beat,
                            ),
                        }
                    }
                    StationArt::Appliance(sprite) => appliance(sprite),
                    StationArt::SnackShelf => DrawableKind::SnackShelf { pos: f.at },
                }
            }
            FixtureKind::Plant { kind, .. } => DrawableKind::Plant { kind, pos: f.at },
            FixtureKind::Pod { kind, .. } => DrawableKind::PodDecorItem { kind, pos: f.at },
            FixtureKind::Wall { kind, .. } => DrawableKind::WallDecor {
                kind,
                pos: f.top_left(),
            },
            FixtureKind::MeetingRug { .. }
            | FixtureKind::LoungeRug
            | FixtureKind::PantryMat
            | FixtureKind::IslandMat => DrawableKind::AreaRug(f.visual),
            FixtureKind::MeetingSofa { faces_away, .. } => DrawableKind::MeetingSofa {
                pos: f.at,
                mirrored: faces_away,
            },
            FixtureKind::MeetingTable { .. } => DrawableKind::MeetingTable { pos: f.at },
            FixtureKind::MeetingChair { facing, .. } => DrawableKind::MeetingChair {
                pos: f.at,
                // The backrest rides the side AWAY from the table: a chair
                // FACING East sits west of the table, bar on its west.
                back_west: facing == Facing::East,
            },
            FixtureKind::CoatRack { .. } => DrawableKind::CoatRack { pos: f.at },
            FixtureKind::Doormat { .. } => DrawableKind::Doormat(f.visual),
            FixtureKind::NoticeBoard { .. } => DrawableKind::NoticeBoard(f.visual),
            // The lounge couch IS the meeting sofa's sprite, mirrored.
            FixtureKind::LoungeCouch => DrawableKind::MeetingSofa {
                pos: f.at,
                mirrored: true,
            },
            FixtureKind::SideTable => DrawableKind::LoungeSideTable { pos: f.at },
            FixtureKind::FloorLamp => DrawableKind::FloorLamp { pos: f.at },
            FixtureKind::FishTank => DrawableKind::FishTank { pos: f.at },
            FixtureKind::KitchenIsland => DrawableKind::KitchenIsland { pos: f.at },
            FixtureKind::WaterCooler => DrawableKind::WaterCooler(f.visual),
            FixtureKind::TrashBin => DrawableKind::TrashBin(f.visual),
            FixtureKind::Door => DrawableKind::Door {
                pos: f.top_left(),
                frame_idx: frame.door_frame,
            },
            FixtureKind::Runner => DrawableKind::Runner(f.visual),
            FixtureKind::NeonSign => DrawableKind::NeonSign {
                panel: f.visual,
                look: neon,
            },
            FixtureKind::Clock => DrawableKind::Clock { pos: f.top_left() },
        };
        match f.depth {
            Depth::Backdrop => out.backdrop.push((wash_of(f.kind), kind)),
            Depth::Sorted { row, tie } => out.sorted.push(Drawable {
                sort_row: row,
                layer: tie.into(),
                hover: None,
                kind,
            }),
        }
    }
    out
}

/// Paint the backdrop fixtures whose [`Wash`] is `pass`, in roster order.
fn paint_backdrop(ctx: &mut PaintCtx<'_>, backdrop: &[(Wash, DrawableKind<'_>)], pass: Wash) {
    for (_, kind) in backdrop.iter().filter(|(wash, _)| *wash == pass) {
        paint_drawable(kind, &mut ctx.drawable_ctx());
    }
}

#[cfg(test)]
pub(crate) mod tests;
