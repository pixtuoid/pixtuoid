//! The SIM half of the frame — advance the world, produce no pixels.
//!
//! `sim_step` mutates the `SimStores` and returns an immutable [`SimFrame`];
//! the paint pass consumes `&SimFrame` and writes only what `PaintCtx` lends it
//! mutably. The paint-local caches it borrows are deliberately NOT sim stores:
//! flushing them changes no behavior, only repaint cost. Headless consumers drive
//! `floor::FloorSession::step` to observe poses/positions without buying a
//! pixel pass.

use pixtuoid_core::sprite::format::{Piece, Walk};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::{Duration, SystemTime};

use crate::pack::OfficeArt;
use pixtuoid_core::id::normalize_path_key;
use pixtuoid_core::source::daemon::DaemonInstanceKey;
use pixtuoid_core::state::{ActivityState, DaemonLiveness, DaemonState, FloorLocalDeskIndex};
use pixtuoid_core::walkable::OccupancyOverlay;
use pixtuoid_core::{AgentId, AgentSlot, SceneState};

use crate::anim::{Beat, Timing};
use crate::chitchat::{self, ActiveChitchat, ChitchatBubble, VenueKey};
use crate::creatures::{
    CreatureKey, CreatureWalk, GatewayCard, Ground, Roam, Stance, gateway_mascot_def,
    mascot_elevator, mascot_enter_delay, mascot_seed, naps_here,
};
use crate::effects::{self, Effect};
use crate::floor::{CoffeeState, FloorInputs, FloorMeta, PetInputs, VacancyDim};
use crate::layout::{Facing, Pivot, Point, SceneLayout, Size, WALKING_Y_OFF};
use crate::pathfind::Router;
use crate::pet::PetKind;
use crate::pose::{self, Pose, PoseHistory};
use crate::walk::WalkState;

use crate::layout::CHARACTER_SPRITE_W;
use crate::pack::{desk_art, desk_art_top};
use anchors::{
    badge_anchor, on_canvas, walking_top_left, waypoint_rank_offset_x, waypoint_top_left,
    with_breath,
};
use seat::{Seat, settle_seat};

pub(crate) mod anchors;
pub(crate) mod seat;

#[doc(hidden)]
pub use anchors::seated_top_left;

/// The mutable world state one `sim_step` advances.
pub(crate) struct SimStores<'a> {
    pub router: &'a mut dyn Router,
    pub overlay: &'a mut OccupancyOverlay,
    pub history: &'a mut PoseHistory,
    pub walks: &'a mut HashMap<AgentId, WalkState>,
    pub vacancy_dim: &'a mut VacancyDim,
    pub neon: &'a mut crate::floor::NeonState,
    pub chitchat: &'a mut HashMap<VenueKey, ActiveChitchat>,
    pub creatures: &'a mut HashMap<CreatureKey, CreatureWalk>,
    /// What a pointer holds on this floor, and set down since its last step.
    pub grip: &'a mut crate::interact::Grip,
}

/// A theme-free glow decision for a character sprite. Sim decides WHETHER a
/// glow applies; paint maps it to a `Theme` color — colors are presentation
/// and must not leak into the sim layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CharacterGlow {
    /// No glow.
    None,
    /// `SeatedThinking` — paint uses the theme's default tool-glow color.
    Thinking,
    /// `SeatedTyping` — paint resolves the per-tool tint (`tool_glow_tint`).
    Tool,
}

/// One character's fully resolved placement for this tick — everything the
/// paint pass needs to blit the sprite, with no sim access and no colors.
#[derive(Debug, Clone)]
pub struct CharacterPlacement {
    /// Index into [`SimFrame::agents`] for this character.
    pub agent_idx: usize,
    /// The row it sorts on (breath-independent).
    pub(crate) sort_row: u16,
    /// The sprite animation to blit (e.g. `"seated"`, `"walking"`).
    pub anim_name: Piece,
    /// The frame within `anim_name` to draw this tick.
    pub frame_idx: usize,
    /// Top-left screen position to blit the sprite at.
    pub top_left: Point,
    /// Where its name badge hangs: the top-centre of the frame `top_left` was
    /// fitted for, without the breath, held clear of the art of the desk it sits
    /// at.
    pub(crate) label_anchor: Point,
    /// Whether to mirror the sprite horizontally.
    pub(crate) flip_x: bool,
    /// The glow decision for this character (paint maps it to a color).
    pub(crate) glow: CharacterGlow,
    /// What rides on it this tick, in paint order.
    pub(crate) effects: Vec<Effect>,
    /// Whether `top_left` takes the idle breath: a figure at rest does, a walker's
    /// stride is its own motion.
    pub(crate) breathes: bool,
    /// The home desk this placement is SEATED AT, in logical units — `None` for
    /// anyone not sitting at one (walking, at a waypoint, standing). Carried
    /// because `top_left` is already PROJECTED and cannot yield it back.
    pub(crate) seat_desk: Option<crate::layout::HomeDesk>,
    /// Whether this figure sits on furniture — a desk's chair, a couch, a sofa
    /// or a meeting chair — which grounds it in place of a shadow.
    pub(crate) seated: bool,
}

/// The row a figure in a pointer's hand sorts on: in front of the whole room.
const IN_HAND_ROW: u16 = u16::MAX;

/// The row a creature centred at `pos` sorts on, its frame `h` tall since the
/// anims differ in height: its south row, or [`IN_HAND_ROW`] in hand.
fn creature_row(pos: Point, h: u16, in_hand: bool) -> u16 {
    if in_hand {
        IN_HAND_ROW
    } else {
        crate::layout::sort_row_at(Pivot::Center, pos, h)
    }
}

/// The office pet this tick.
#[derive(Debug, Clone)]
pub(crate) struct PetPlacement {
    /// Which pet.
    pub(crate) kind: PetKind,
    /// Its centre, in layout units, fitted so its frame lands on the canvas.
    pub(crate) pos: Point,
    /// The row it sorts on, both painters' ([`creature_row`]).
    pub(crate) sort_row: u16,
    /// Whether to mirror the sprite horizontally.
    pub(crate) flip: bool,
    /// The sprite animation to draw.
    pub(crate) anim_name: Piece,
    /// The frame within `anim_name`.
    pub(crate) frame_idx: usize,
    /// What rides on it this tick, in paint order.
    pub(crate) effects: Vec<Effect>,
}

impl PetPlacement {
    /// Who its hover names.
    pub(crate) fn target(&self) -> crate::display::HoverTarget {
        crate::display::HoverTarget::Pet(crate::display::PetHover {
            kind: self.kind,
            centre: self.pos,
            anim: self.anim_name,
        })
    }
}

/// One gateway mascot this tick.
#[derive(Debug, Clone)]
pub(crate) struct MascotPlacement {
    /// Its centre, in layout units, fitted so its frame lands on the canvas.
    pub(crate) pos: Point,
    /// The row it sorts on, both painters' ([`creature_row`]).
    pub(crate) sort_row: u16,
    /// The sprite animation to draw.
    pub(crate) anim_name: Piece,
    /// The frame within `anim_name`.
    pub(crate) frame_idx: usize,
    /// Which gateway instance it is.
    pub(crate) key: DaemonInstanceKey,
    /// Its gateway's backend fails every run ([`GatewayCard::degraded`]), so
    /// it greys.
    pub(crate) degraded: bool,
    /// Its gateway is in the roster; one walking out past that has no
    /// [`GatewayCard`], so it hovers as nothing.
    pub(crate) on_roster: bool,
    /// What rides on it this tick: a bubble per run in flight.
    pub(crate) effects: Vec<Effect>,
}

impl MascotPlacement {
    /// Who its hover names, while its gateway is on the roster.
    pub(crate) fn target(&self) -> Option<crate::display::HoverTarget> {
        self.on_roster
            .then(|| crate::display::HoverTarget::Mascot(self.key.clone()))
    }
}

/// A coffee on a desk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cup {
    /// Fetched within [`CoffeeState::STEAM_WINDOW_SECS`].
    Steaming,
    /// Past that window, still on the desk.
    Cold,
}

/// One home desk's live props this tick.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DeskProps {
    /// The coffee its occupant fetched, while it sits on the desk.
    pub(crate) cup: Option<Cup>,
    /// 0 = no tower (the plain desk), else the reams up to
    /// [`MAX_TIER`](crate::token_meter::MAX_TIER).
    pub(crate) token_tier: u8,
    /// A falling sheet's distance fallen, in layout units, when a big usage
    /// reading is mid-drop, else `None`.
    pub(crate) sheet_fall: Option<u16>,
    /// What rides on it this tick: a steaming cup's steam.
    pub(crate) effects: Vec<Effect>,
    /// Which glass column its lit screen's scanline is on, from the glass's
    /// west edge ([`scanline_col`]).
    pub(crate) scanline: u16,
}

/// How long a screen's scanline holds each glass column: one Full beat, so it
/// never skips one.
const SCANLINE_STEP_MS: u64 = crate::anim::FULL_TICK_MS;

/// The glass column a desk at column `desk_x`'s scanline is on at `beat`: the
/// line sweeps east and wraps, each desk a column on from its west neighbour's.
pub(crate) fn scanline_col(desk_x: u16, beat: Beat) -> u16 {
    let glass = crate::layout::SCREEN_GLASS_COLS;
    let glass_w = u64::from(glass.end() - glass.start() + 1);
    let phase = beat.ms() / SCANLINE_STEP_MS + u64::from(desk_x);
    // Below `glass_w`, a u16.
    (phase % glass_w) as u16
}

/// The immutable outcome of one `sim_step`: the world advanced, observed.
/// Paint consumes it by `&` — rendering the same frame twice is byte-identical
/// and cannot move the sim. Owned data, so the stores are free again the moment
/// `sim_step` returns.
#[derive(Debug, Clone)]
pub struct SimFrame {
    /// The tick's agent snapshot — placements index into it, paint borrows
    /// from it.
    pub agents: Vec<AgentSlot>,
    /// The authoritative routed pose per home-desk agent this tick (`None` =
    /// no renderable pose). Unread by paint BY DESIGN;
    /// `floor::FloorSession::step` is the lib-side consumer.
    pub(crate) poses: HashMap<AgentId, Option<Pose>>,
    /// Per-desk "occupant is actually seated right now" (drives screen glow +
    /// ceiling halos; exiting agents absent by construction).
    pub(crate) seated_agents: HashMap<FloorLocalDeskIndex, bool>,
    /// Fully resolved character sprites for this tick, in agent order.
    pub characters: Vec<CharacterPlacement>,
    /// Smoothed indoor-lighting level from `VacancyDim::tick`.
    pub(crate) indoor_scale: f32,
    /// The neon sign's light from `NeonState::tick`.
    pub(crate) neon: crate::floor::NeonLevels,
    /// Whether that light is a starved tube's catch.
    pub(crate) neon_stutter: bool,
    /// Active speech bubbles after this tick's venue update.
    pub(crate) chitchat_bubbles: Vec<ChitchatBubble>,
    /// Agents observed walking back with coffee this tick — the caller
    /// persists them into its `CoffeeState`.
    pub(crate) new_coffee_carriers: Vec<AgentId>,
    /// Waypoint indices with an occupant this tick — drives the appliance
    /// feedback animations.
    pub(crate) occupied_waypoints: std::collections::HashSet<usize>,
    /// The office pet this tick; `None` on a floor without one, or where
    /// [`FloorInputs::pets`] brings none.
    pub(crate) pet: Option<PetPlacement>,
    /// Each gateway mascot present this tick.
    pub(crate) mascots: Vec<MascotPlacement>,
    /// Each home desk's live props, index-parallel to
    /// [`home_desks`](crate::layout::SceneLayout::home_desks); read through
    /// [`Self::desk`].
    pub(crate) desks: Vec<DeskProps>,
    /// The elevator art's frame, shut to open.
    pub(crate) door_frame: usize,
}

impl SimFrame {
    /// Desk `i`'s props this tick.
    pub(crate) fn desk(&self, i: FloorLocalDeskIndex) -> DeskProps {
        self.desks[i.0].clone()
    }
}

/// What one `sim_step` reads, besides the stores it advances.
pub(crate) struct SimInputs<'a> {
    pub(crate) world: FloorInputs<'a>,
    pub(crate) pack: &'a OfficeArt,
    pub(crate) layout: &'a SceneLayout,
    /// Carrier → fetch time of each desk cup.
    pub(crate) coffee: &'a HashMap<AgentId, SystemTime>,
    /// [`FloorCtx::door_anim_max_ms`](crate::floor::FloorCtx::door_anim_max_ms).
    pub(crate) door_anim_max_ms: u64,
}

/// Advance the world one tick WITHOUT painting, into the [`SimFrame`] every
/// painter reads.
pub(crate) fn sim_step(stores: &mut SimStores<'_>, inputs: SimInputs<'_>) -> SimFrame {
    let SimInputs {
        world:
            FloorInputs {
                scene,
                now,
                floor,
                pets,
            },
        pack,
        layout,
        coffee,
        door_anim_max_ms,
    } = inputs;
    let timing = floor.motion.timing(now);
    let beat = timing.beat;
    let agents: Vec<AgentSlot> = scene.agents.values().cloned().collect();
    take_grip(stores, &agents, layout, now);

    let indoor_scale = stores.vacancy_dim.tick(scene.agents.is_empty(), now);
    let neon = stores.neon.tick(
        crate::neon_sign::OfficeMood::of(crate::tally::scene_stats(scene)),
        stores.vacancy_dim.dimmed(),
        timing,
    );

    // Per-frame occupancy from STATIONARY agent positions only, BEFORE the
    // routed pose pass (which routes Walking poses against THIS overlay).
    // Walkers are deliberately excluded: their position interpolates every
    // frame, which would change the overlay signature every frame, wipe the
    // path cache, recompute A*, and snap walkers to new path segments (the
    // visible "flash"). Sitters at desks are already covered by the static desk
    // mask, so only waypoint visitors — stable across frames — contribute.
    stores.overlay.clear();
    // Only a wanderer stands at a waypoint, and at rest none wanders.
    let wanderers = if beat.is_rest() { &[][..] } else { &agents[..] };
    for agent in wanderers {
        // A lifted one stands nowhere its timeline says.
        if stores
            .walks
            .get(&agent.agent_id)
            .is_some_and(|w| w.lifted.is_some())
        {
            continue;
        }
        let Some(origin) = layout.home_desk(agent.desk_index.single_floor_local()) else {
            continue;
        };
        let Some(pose) = pose::derive(agent, now, layout) else {
            continue;
        };
        if let Pose::AtWaypoint { wp, .. } = pose
            && let Some(w) = layout.waypoints.get(wp)
        {
            // Reserve the cell the agent actually stands on, NOT the
            // blocked furniture center — else another agent's A* routes
            // straight through the stander.
            let stand = layout.stand_point(w.kind, w.pos, origin.at, w.facing);
            stores.overlay.add(
                stand.x.saturating_sub(CHARACTER_SPRITE_W / 2),
                stand.y.saturating_sub(WALKING_Y_OFF / 2),
                CHARACTER_SPRITE_W,
                WALKING_Y_OFF,
            );
        }
    }

    // The AUTHORITATIVE pose derivation, ONCE per frame: it runs the
    // advance_wander / walk_path / history side effects, and placement
    // resolution below looks the result up instead of re-deriving (a second
    // derive would double the A*). The `exiting_at` filter is INTENTIONALLY
    // absent — an exiting agent's pose is still needed to place its character.
    let poses: HashMap<AgentId, Option<Pose>> = agents
        .iter()
        .filter(|a| {
            layout
                .home_desk(a.desk_index.single_floor_local())
                .is_some()
        })
        .map(|a| {
            let p = pose::derive_with_routing(
                a,
                now,
                layout,
                &mut pose::RouteCtx {
                    router: &mut *stores.router,
                    overlay: &*stores.overlay,
                    history: &mut *stores.history,
                    walks: &mut *stores.walks,
                    wanders: !beat.is_rest(),
                },
            );
            (a.agent_id, p)
        })
        .collect();

    // Derived from the cached poses so the desk-cubicle screen glow and the
    // ceiling halos share one gate.
    let seated_agents: HashMap<FloorLocalDeskIndex, bool> = agents
        .iter()
        .filter(|a| {
            layout
                .home_desk(a.desk_index.single_floor_local())
                .is_some()
                && a.exiting_at.is_none()
        })
        .map(|a| {
            let seated = matches!(
                poses.get(&a.agent_id),
                Some(Some(Pose::SeatedTyping | Pose::SeatedThinking))
            );
            (a.desk_index.single_floor_local(), seated)
        })
        .collect();

    let (characters, waypoint_visitors, new_coffee_carriers, occupied_waypoints) =
        resolve_characters(&agents, &poses, layout, pack, coffee, timing);

    let chitchat_bubbles =
        chitchat::update_and_collect(stores.chitchat, floor.floor_idx, &waypoint_visitors, now);
    let mut ground = Ground {
        layout,
        router: &mut *stores.router,
        overlay: &*stores.overlay,
    };
    let pet = pet_placement(
        &agents,
        pack,
        pets,
        floor,
        timing,
        &mut ground,
        stores.creatures,
    );
    let mascots = mascot_placements(scene, pack, timing, &mut ground, stores.creatures);
    let desks = desk_props(&agents, layout, pack, coffee, timing);

    let door_frame = anchors::compute_door_frame_idx(&agents, now, door_anim_max_ms);
    SimFrame {
        agents,
        poses,
        seated_agents,
        characters,
        indoor_scale,
        neon,
        neon_stutter: stores.neon.stutters(),
        chitchat_bubbles,
        new_coffee_carriers,
        occupied_waypoints,
        pet,
        mascots,
        desks,
        door_frame,
    }
}

/// The size of `anim`'s frame `frame_idx`, [wrapped](pixtuoid_core::sprite::Sprite::wrap).
pub(crate) fn pack_frame_size(pack: &OfficeArt, anim: Piece, frame_idx: usize) -> Size {
    let f = pack.piece(anim).frame_at(frame_idx);
    Size {
        w: f.width(),
        h: f.height(),
    }
}

/// Carry out the floor's grip before anything steps: each figure set down
/// lands, in the order set down, a creature to rest and an agent to walk
/// home; the one held follows the pointer. A hold stays for the next step
/// until its figure is gone or on its way out.
fn take_grip(
    stores: &mut SimStores<'_>,
    agents: &[AgentSlot],
    layout: &SceneLayout,
    now: SystemTime,
) {
    use crate::interact::{Figure, Grip};
    let Grip { dropped, held } = std::mem::take(&mut *stores.grip);
    // Whether `figure` stays in hand: held at `at` while it can be, else set
    // down there.
    let mut grip = |figure: &Figure, at: Point, held: bool| {
        let creature = match figure {
            Figure::Agent(id) => {
                let here = agents
                    .iter()
                    .any(|a| a.agent_id == *id && a.exiting_at.is_none());
                return match stores.walks.get_mut(id) {
                    Some(walk) if here && held => {
                        walk.carry(at);
                        true
                    }
                    // One walking out leaves the hand where the grip has it;
                    // one lifted on its way out was never in it.
                    Some(walk) if here || walk.carried() => {
                        walk.set_down(at);
                        false
                    }
                    // One gone mid-carry has nothing to walk home.
                    _ => false,
                };
            }
            Figure::Pet(kind) => CreatureKey::Pet(*kind),
            Figure::Mascot(key) => CreatureKey::Mascot(key.clone()),
        };
        let Some(walk) = stores.creatures.get_mut(&creature) else {
            return false;
        };
        if held && !walk.leaving() {
            walk.carry(at);
            true
        } else {
            walk.set_down(at, layout, now);
            false
        }
    };
    for (figure, at) in &dropped {
        grip(figure, *at, false);
    }
    stores.grip.held = held.filter(|(figure, at)| grip(figure, *at, true));
}

/// The floor's pet this tick, walking the people's walker: a pet being
/// petted holds still where it stands, and a resting one naps beside an idle
/// desk.
fn pet_placement(
    agents: &[AgentSlot],
    pack: &OfficeArt,
    pets: PetInputs<'_>,
    floor: FloorMeta,
    timing: Timing,
    ground: &mut Ground<'_>,
    creatures: &mut HashMap<CreatureKey, CreatureWalk>,
) -> Option<PetPlacement> {
    let Timing { now, beat } = timing;
    let layout = ground.layout;
    let kind = pets.pet.map(|p| p.kind);
    creatures.retain(|key, _| !matches!(key, CreatureKey::Pet(k) if Some(*k) != kind));
    let kind = kind?;
    let walk_anim = kind.walk_anim();
    let stride = pack.stride(walk_anim);
    let walk = creatures
        .entry(CreatureKey::Pet(kind))
        .or_insert_with(|| CreatureWalk::at_home(layout, floor.floor_seed, now));
    let petted = pets
        .petting
        .filter(|p| p.is_active(now) && p.kind == kind && p.floor_idx == floor.floor_idx);
    if petted.is_some() {
        walk.hold(now);
    }
    let Stance { at, walking } = walk.step(Roam::pet(stride), ground, timing)?;
    // a petted pet sits for it, even held mid-leg
    let (anim_name, frame_idx, flip) = match walking.filter(|_| petted.is_none()) {
        Some(segment) => (
            walk_anim.piece(),
            pose::walk_frame(segment.travelled(), pack.piece(walk_anim.piece()), stride),
            // its walk faces east
            segment.to().x < segment.from().x,
        ),
        None => {
            let all_idle = agents
                .iter()
                .all(|a| matches!(a.state, ActivityState::Idle));
            let anim = if petted.is_none()
                && (all_idle || (kind.sleeps_near_idle() && naps_here(layout, agents, at)))
            {
                kind.sleep_anim()
            } else {
                kind.sit_anim()
            };
            (
                anim,
                crate::pack::looping_frame_index(pack.piece(anim), beat),
                false,
            )
        }
    };
    let size = pack_frame_size(pack, anim_name, frame_idx);
    let pos = on_canvas(layout, Pivot::Center, at, size);
    Some(PetPlacement {
        kind,
        pos,
        sort_row: creature_row(pos, size.h, walk.carried()),
        flip,
        anim_name,
        frame_idx,
        effects: pet_effects(
            kind,
            pos,
            anim_name,
            petted.map(|p| p.elapsed_ms(now)),
            beat,
        ),
    })
}

/// Staggers the pet's sleep z off every sleeper's at its desk.
const PET_SLEEP_Z_SEED: u64 = 0xCAFE;

/// What rides on a `kind` pet at `pos` drawn as `anim_name`: hearts while it
/// is being petted, `petted_ms` in, else a z while it sleeps.
pub(crate) fn pet_effects(
    kind: PetKind,
    pos: Point,
    anim_name: Piece,
    petted_ms: Option<u64>,
    beat: Beat,
) -> Vec<Effect> {
    match petted_ms {
        Some(ms) => effects::pet_hearts(pos, ms).collect(),
        None if anim_name == kind.sleep_anim() => effects::sleep_z(pos, PET_SLEEP_Z_SEED, beat)
            .into_iter()
            .collect(),
        None => Vec::new(),
    }
}

/// One gateway mascot to draw this frame, and what its card shows.
struct DrawnMascot {
    key: DaemonInstanceKey,
    stance: Stance,
    /// [`GatewayCard::degraded`]; false once its gateway left the roster.
    degraded: bool,
    /// [`MascotPlacement::on_roster`].
    on_roster: bool,
    /// Its gateway's runs in flight.
    runs: u32,
    /// [`CreatureWalk::carried`].
    carried: bool,
}

/// Every gateway mascot in the scene's daemon roster, walking the people's
/// walker, plus each one still walking out after its gateway left the roster.
/// The runtime keeps the roster honest, so "entry present" tracks "connected +
/// alive", not merely "a hook arrived"; only the ground floor carries it, so
/// each mascot shows once.
fn mascot_placements(
    scene: &SceneState,
    pack: &OfficeArt,
    timing: Timing,
    ground: &mut Ground<'_>,
    creatures: &mut HashMap<CreatureKey, CreatureWalk>,
) -> Vec<MascotPlacement> {
    let Timing { now, beat } = timing;
    let Some(elevator) = mascot_elevator(ground.layout) else {
        creatures.retain(|key, _| matches!(key, CreatureKey::Pet(_)));
        return Vec::new();
    };
    let mut drawn: Vec<DrawnMascot> = Vec::new();
    for (source, instance, presence) in scene.daemons() {
        let Some(def) = gateway_mascot_def(source) else {
            continue;
        };
        let state = presence.display_state();
        let roam = Roam::mascot(pack.stride(def.walk), state);
        let key = DaemonInstanceKey::new(source, instance.clone());
        let creature = CreatureKey::Mascot(key.clone());
        if presence.liveness != DaemonLiveness::Down
            && creatures.get(&creature).is_some_and(CreatureWalk::leaving)
        {
            // back up: it walks in again
            creatures.remove(&creature);
        }
        let walk = match creatures.entry(creature) {
            Entry::Occupied(e) => e.into_mut(),
            // one never seen alive was never in the room
            Entry::Vacant(_) if presence.liveness == DaemonLiveness::Down => continue,
            Entry::Vacant(e) => {
                let seed = mascot_seed(source, instance);
                let enters_at =
                    presence.entered_at + Duration::from_millis(mascot_enter_delay(seed));
                if now < enters_at {
                    continue;
                }
                e.insert(CreatureWalk::arriving(
                    elevator, seed, roam, ground, enters_at, now,
                ))
            }
        };
        if presence.liveness == DaemonLiveness::Down && !walk.leaving() {
            walk.leave(elevator, roam, ground, now);
        }
        if let Some(stance) = walk.step(roam, ground, timing) {
            drawn.push(DrawnMascot {
                degraded: GatewayCard::of(scene, &key).is_some_and(|card| card.degraded),
                on_roster: true,
                key,
                stance,
                runs: presence.in_flight_runs.len() as u32,
                carried: walk.carried(),
            });
        }
    }
    // One whose gateway left the roster walks out, on past the entry; in key
    // order, so the draw order holds frame to frame.
    let mut orphans: Vec<DaemonInstanceKey> = creatures
        .keys()
        .filter_map(|key| match key {
            CreatureKey::Mascot(k) if scene.daemon(k.source(), k.instance()).is_none() => {
                Some(k.clone())
            }
            CreatureKey::Mascot(_) | CreatureKey::Pet(_) => None,
        })
        .collect();
    orphans.sort();
    for key in orphans {
        let creature = CreatureKey::Mascot(key.clone());
        let (Some(def), Some(walk)) = (
            gateway_mascot_def(key.source()),
            creatures.get_mut(&creature),
        ) else {
            creatures.remove(&creature);
            continue;
        };
        let roam = Roam::mascot(pack.stride(def.walk), DaemonState::Down);
        if !walk.leaving() {
            walk.leave(elevator, roam, ground, now);
        }
        if let Some(stance) = walk.step(roam, ground, timing) {
            drawn.push(DrawnMascot {
                key,
                stance,
                degraded: false,
                on_roster: false,
                runs: 0,
                carried: walk.carried(),
            });
        }
    }
    creatures.retain(|key, walk| !matches!(key, CreatureKey::Mascot(_)) || !walk.gone());
    drawn
        .into_iter()
        .filter_map(
            |DrawnMascot {
                 key,
                 stance: Stance { at, walking },
                 degraded,
                 on_roster,
                 runs,
                 carried,
             }| {
                let def = gateway_mascot_def(key.source())?;
                let (anim_name, frame_idx) = match walking {
                    Some(segment) => (
                        def.walk.piece(),
                        pose::walk_frame(
                            segment.travelled(),
                            pack.piece(def.walk.piece()),
                            pack.stride(def.walk),
                        ),
                    ),
                    None => (
                        def.rest,
                        crate::pack::looping_frame_index(pack.piece(def.rest), beat),
                    ),
                };
                let size = pack_frame_size(pack, anim_name, frame_idx);
                let pos = on_canvas(ground.layout, Pivot::Center, at, size);
                Some(MascotPlacement {
                    pos,
                    sort_row: creature_row(pos, size.h, carried),
                    anim_name,
                    frame_idx,
                    key,
                    degraded,
                    on_roster,
                    // The busy tell keys on in-flight RUNS, not the (persistent,
                    // single-user) session count, which sticks at 1 at rest.
                    effects: if runs > 0 {
                        effects::mascot_bubbles(pos, size.h, runs, beat).collect()
                    } else {
                        Vec::new()
                    },
                })
            },
        )
        .collect()
}

/// Each home desk's live props, from its occupant: the coffee it fetched and the
/// tokens it has spent.
fn desk_props(
    agents: &[AgentSlot],
    layout: &SceneLayout,
    pack: &OfficeArt,
    coffee: &HashMap<AgentId, SystemTime>,
    timing: Timing,
) -> Vec<DeskProps> {
    let Timing { now, beat } = timing;
    layout
        .home_desks
        .iter()
        .enumerate()
        .map(|(i, desk)| {
            let occupant = desk_occupant(agents, FloorLocalDeskIndex(i));
            let fetched_at = occupant.and_then(|a| coffee.get(&a.agent_id));
            let cup = fetched_at.map(|t| {
                let fresh = now
                    .duration_since(*t)
                    .is_ok_and(|d| d.as_secs() < CoffeeState::STEAM_WINDOW_SECS);
                if fresh { Cup::Steaming } else { Cup::Cold }
            });
            DeskProps {
                cup,
                token_tier: occupant.map_or(0, |a| crate::token_meter::token_tier(a.tokens_used)),
                sheet_fall: occupant.and_then(|a| crate::token_meter::sheet_fall_dist(a, now)),
                effects: cup_effects(desk_cup_at(pack, desk.at, desk.facing), cup, beat),
                scanline: scanline_col(desk.at.x, beat),
            }
        })
        .collect()
}

/// What rides on the cup standing at `cup_at`: steam while it is fresh.
pub(crate) fn cup_effects(cup_at: Option<Point>, cup: Option<Cup>, beat: Beat) -> Vec<Effect> {
    match (cup, cup_at) {
        (Some(Cup::Steaming), Some(at)) => effects::steam(at, beat).to_vec(),
        _ => Vec::new(),
    }
}

/// What a pose arm puts on its figure, before the fit settles where it stands.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Cues {
    /// Asleep: its z's stagger off this seed.
    pub(crate) sleep_seed: Option<u64>,
    /// Waiting on the human.
    pub(crate) waiting: bool,
    /// Walking, its weight on this foot ([`effects::planted_foot`]).
    pub(crate) planted_foot: Option<usize>,
}

/// What rides on `agent`, whose `w`-wide frame stands at `top_left` this
/// tick, in paint order: dust underfoot, then a burning head's crown, then a
/// sleeper's z or a waiter's mark.
pub(crate) fn character_effects(
    agent: &AgentSlot,
    top_left: Point,
    w: u16,
    cues: Cues,
    timing: Timing,
) -> Vec<Effect> {
    let Timing { now, beat } = timing;
    let mut out = Vec::new();
    out.extend(
        cues.planted_foot
            .map(|f| effects::walking_dust(top_left, f)),
    );
    if crate::burn::slot_burn_tier(agent, now) == crate::burn::BurnTier::Top {
        out.push(effects::flame_crown(top_left, w, beat));
    }
    out.extend(
        cues.sleep_seed
            .and_then(|seed| effects::sleep_z(top_left, seed, beat)),
    );
    if cues.waiting {
        out.push(effects::waiting_mark(top_left));
    }
    out
}

/// The upright seated pose. Each seated pose is a base
/// [`Seat::sprite_for`](seat::Seat::sprite_for) turns to its seat's view.
const SEATED: Piece = Piece::Seated;
/// The typing pose.
const TYPING: Piece = Piece::Typing;
/// The sleeping poses, picked by agent id so a row of sleepers isn't one pose.
const SLEEPING_POSES: [Piece; 2] = [Piece::SeatedSleeping, Piece::SeatedSleepingAlt];

/// A person's walks, the sim's pick by the leg: facing the camera, walking
/// away, carrying a coffee.
const WALK: Walk = Walk::Walking;
const WALK_BACK: Walk = Walk::WalkingBack;
const WALK_COFFEE: Walk = Walk::WalkingCoffee;
/// Every walk a person has.
#[cfg(test)]
pub(crate) const WALKS: [Walk; 3] = [WALK, WALK_BACK, WALK_COFFEE];

/// Resolve every character's placement for this tick from the routed poses
/// `sim_step` already derived. Returns the placements (paint maps them 1:1 to
/// drawables), the waypoint visitors (for the chitchat venues), the agents seen
/// carrying coffee, and the occupied waypoint indices.
pub(crate) fn resolve_characters(
    agents: &[AgentSlot],
    poses: &HashMap<AgentId, Option<Pose>>,
    layout: &SceneLayout,
    pack: &OfficeArt,
    coffee: &HashMap<AgentId, SystemTime>,
    timing: Timing,
) -> (
    Vec<CharacterPlacement>,
    Vec<chitchat::Visitor>,
    Vec<AgentId>,
    std::collections::HashSet<usize>,
) {
    let beat = timing.beat;
    let mut placements: Vec<(CharacterPlacement, Cues)> = Vec::new();
    let mut new_coffee_carriers: Vec<AgentId> = Vec::new();
    let mut wp_rank: HashMap<usize, usize> = HashMap::new();
    let mut waypoint_visitors: Vec<chitchat::Visitor> = Vec::new();
    for (agent_idx, agent) in agents.iter().enumerate() {
        let Some(desk) = layout.home_desk(agent.desk_index.single_floor_local()) else {
            continue;
        };
        let Some(p) = poses.get(&agent.agent_id).copied().flatten() else {
            continue;
        };
        let is_waiting = matches!(agent.state, ActivityState::Waiting { .. });
        let seated = |base: Piece, glow: CharacterGlow, sleep_seed: Option<u64>| {
            let seat = Seat::at_desk(desk.at, desk.facing);
            let top_left = seat.render_top_left(CHARACTER_SPRITE_W);
            let (anim_name, flip_x) = seat.sprite_for(base);
            let placement = CharacterPlacement {
                agent_idx,
                // Breath-independent sort row: the breath's 1 px rise must not flip
                // sort order against nearby desk decor frame-to-frame.
                sort_row: seat.sort_row(),
                anim_name,
                frame_idx: 0,
                top_left,
                label_anchor: top_left,
                flip_x,
                glow,
                effects: Vec::new(),
                // The one arm that IS seated at a desk — see the field's doc.
                seat_desk: Some(desk),
                seated: true,
                breathes: true,
            };
            let cues = Cues {
                sleep_seed,
                waiting: is_waiting,
                planted_foot: None,
            };
            (placement, cues)
        };
        match p {
            Pose::SeatedIdle if is_waiting => {
                // Waiting is the one state that WANTS the human — the `N wait`
                // counter's twin. Asleep-with-zzz reads as the opposite.
                placements.push(seated(SEATED, CharacterGlow::None, None));
            }
            Pose::SeatedIdle => {
                let sleep_variant =
                    SLEEPING_POSES[(agent.agent_id.raw() % SLEEPING_POSES.len() as u64) as usize];
                placements.push(seated(
                    sleep_variant,
                    CharacterGlow::None,
                    Some(agent.agent_id.raw()),
                ));
            }
            Pose::SeatedThinking => {
                placements.push(seated(SEATED, CharacterGlow::Thinking, None));
            }
            Pose::SeatedTyping => {
                let (mut placement, cues) = seated(TYPING, CharacterGlow::Tool, None);
                // the art the seat resolved to, a back view's own loop included
                placement.frame_idx =
                    pose::typing_frame(agent, beat, pack.piece(placement.anim_name));
                placements.push((placement, cues));
            }
            Pose::AtWaypoint { wp, kind } => {
                if let Some(wp_obj) = layout.waypoints.get(wp) {
                    let rank = *wp_rank.entry(wp).or_insert(0);
                    wp_rank.insert(wp, rank + 1);
                    let dx = waypoint_rank_offset_x(kind, rank);
                    let stand = layout.stand_point(wp_obj.kind, wp_obj.pos, desk.at, wp_obj.facing);
                    let seat = Seat::at_waypoint(kind, stand, wp_obj.facing);
                    let upright_top_left = seat.render_top_left(CHARACTER_SPRITE_W);
                    let (anim_name, flip_x) = seat.sprite_for(SEATED);
                    let top_left = Point {
                        x: upright_top_left.x.saturating_add_signed(dx),
                        y: upright_top_left.y,
                    };
                    if chitchat::supports_chitchat(kind) {
                        waypoint_visitors.push(chitchat::Visitor {
                            // The couch's seats collapse to ONE venue so it
                            // hosts a single group conversation; other
                            // waypoints key on their own index.
                            wp_idx: chitchat::venue_wp_idx(kind, wp, &layout.waypoints),
                            agent_id: agent.agent_id,
                            room_id: wp_obj.room_id,
                        });
                    }
                    placements.push((
                        CharacterPlacement {
                            agent_idx,
                            // The glide's own key, so nothing pops at the
                            // walk→seat seam.
                            sort_row: seat.sort_row(),
                            anim_name,
                            frame_idx: 0,
                            top_left,
                            label_anchor: top_left,
                            flip_x,
                            glow: CharacterGlow::None,
                            effects: Vec::new(),
                            seat_desk: None,
                            seated: seat.seated_furniture(),
                            breathes: true,
                        },
                        Cues::default(),
                    ));
                }
            }
            Pose::AimlessAt { dest: at } | Pose::Held { at } => {
                let held = matches!(p, Pose::Held { .. });
                let top_left = waypoint_top_left(at, CHARACTER_SPRITE_W);
                placements.push((
                    CharacterPlacement {
                        agent_idx,
                        sort_row: if held {
                            IN_HAND_ROW
                        } else {
                            top_left.y + WALKING_Y_OFF
                        },
                        anim_name: Piece::Standing,
                        frame_idx: 0,
                        top_left,
                        label_anchor: top_left,
                        flip_x: false,
                        glow: CharacterGlow::None,
                        effects: Vec::new(),
                        seat_desk: None,
                        seated: false,
                        breathes: !held,
                    },
                    Cues::default(),
                ));
            }
            Pose::Walking {
                segment,
                mut carrying_coffee,
            } => {
                let (from, to, travelled) = (segment.from(), segment.to(), segment.travelled());
                // Exit walks: core sets carrying_coffee=false (it holds no
                // render-side state), but the coffee map knows better.
                if agent.exiting_at.is_some() && coffee.contains_key(&agent.agent_id) {
                    carrying_coffee = true;
                }
                if carrying_coffee {
                    new_coffee_carriers.push(agent.agent_id);
                }
                let walker_top_left = walking_top_left(segment.at(), CHARACTER_SPRITE_W);
                let dx = i32::from(to.x) - i32::from(from.x);
                let dy = i32::from(to.y) - i32::from(from.y);
                // A glide on/off a seat (`to` is a foot-cell sitting down,
                // `from` rising) renders in the SEAT's view and at the SEAT's
                // sort row, NOT the travel direction's. Without it a window-facing
                // seat renders a FRONT walk and the agent sits facing the
                // camera until it snaps at AtWaypoint. Ordinary travel segments
                // keep the travel-direction facing and foot-position sort row.
                let settle = settle_seat(to, layout).or_else(|| settle_seat(from, layout));
                let (going_back, flip) = match settle {
                    Some(seat) => seat.settle_walk(),
                    None => (
                        dy.unsigned_abs() > dx.unsigned_abs() && dy < 0,
                        to.x < from.x,
                    ),
                };
                // walking_back always wins (no back-facing coffee sprite).
                let walk: Walk = if going_back {
                    WALK_BACK
                } else if carrying_coffee {
                    WALK_COFFEE
                } else {
                    WALK
                };
                let anim_name = walk.piece();
                let frame = pose::walk_frame(travelled, pack.piece(anim_name), pack.stride(walk));
                placements.push((
                    CharacterPlacement {
                        agent_idx,
                        sort_row: match settle {
                            Some(seat) => seat.sort_row(),
                            None => walker_top_left.y + WALKING_Y_OFF,
                        },
                        anim_name,
                        frame_idx: frame,
                        top_left: walker_top_left,
                        label_anchor: walker_top_left,
                        flip_x: flip,
                        glow: CharacterGlow::None,
                        effects: Vec::new(),
                        seat_desk: None,
                        seated: false,
                        breathes: false,
                    },
                    Cues {
                        planted_foot: Some(effects::planted_foot(
                            frame,
                            pack.piece(anim_name).frames().len(),
                            flip,
                        )),
                        ..Cues::default()
                    },
                ));
            }
        }
    }
    // ONE fit for every pose arm, on the frame each placement will blit, read by
    // both the sprite and its badge. The sort row keeps pre-fit geometry.
    for (p, cues) in &mut placements {
        let size = pack_frame_size(pack, p.anim_name, p.frame_idx);
        let fitted = on_canvas(layout, Pivot::TopLeft, p.top_left, size);
        // The painter's own desk art: whatever it raises behind the sitter's
        // head, the badge clears.
        let ceiling = p
            .seat_desk
            .map(|d| desk_art_top(pack, d.at.y, desk_art(pack, d.facing).height()));
        p.label_anchor = badge_anchor(fitted, size, ceiling);
        // Breath after the fit, so it never moves the badge.
        let agent = &agents[p.agent_idx];
        p.top_left = if p.breathes {
            with_breath(fitted, agent.agent_id, beat)
        } else {
            fitted
        };
        p.effects = character_effects(agent, p.top_left, size.w, *cues, timing);
    }

    // wp_rank's keys ARE this tick's occupied waypoints — every AtWaypoint
    // occupant registers a rank.
    (
        placements.into_iter().map(|(p, _)| p).collect(),
        waypoint_visitors,
        new_coffee_carriers,
        wp_rank.into_keys().collect(),
    )
}

/// Where the cup stands on the 1x desk at `desk` facing `facing`: its top-left
/// cell, at the desk art's cup mark; `None` where the cup would hang off the
/// buffer.
pub(crate) fn desk_cup_at(pack: &OfficeArt, desk: Point, facing: Facing) -> Option<Point> {
    let mark = crate::pack::desk_mark(pack, desk, facing, crate::pack::DeskProp::Cup);
    let cup = pack_frame_size(pack, Piece::DeskCup, 0);
    Some(Point {
        x: mark.left(cup.w)?,
        y: mark.at.y.checked_sub(cup.h)?,
    })
}

/// The agent whose home desk is `local`, while they have not begun to leave.
pub(crate) fn desk_occupant(
    agents: &[AgentSlot],
    local: FloorLocalDeskIndex,
) -> Option<&AgentSlot> {
    agents
        .iter()
        .find(|a| a.desk_index.single_floor_local() == local && a.exiting_at.is_none())
}

/// Deterministic seed from a normalized cwd string: byte-fold, then the
/// splitmix64 finalizer. NOT `DefaultHasher`: its algorithm may change between
/// Rust releases, which would re-dress every agent on a toolchain bump.
fn cwd_outfit_seed(cwd_norm: &str) -> u64 {
    let folded = cwd_norm
        .bytes()
        .fold(0u64, |h, b| h.wrapping_mul(131).wrapping_add(u64::from(b)));
    pixtuoid_core::id::splitmix64(folded)
}

/// The outfit-determining seed for `agent`. Extracted so
/// `FrameCache::note_outfit_seed` watches the mid-lifetime cwd backfill through
/// the EXACT unknown-cwd fallback [`agent_overrides`](crate::character::agent_overrides)
/// uses; a second copy would drift.
pub(crate) fn outfit_seed_for(agent: &AgentSlot) -> u64 {
    if agent.unknown_cwd || agent.cwd.as_os_str().is_empty() {
        agent.agent_id.raw()
    } else {
        cwd_outfit_seed(&normalize_path_key(&agent.cwd.to_string_lossy()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The phase is epoch-based, so it outgrows a u16: the scanline must keep
    /// stepping one column per step rather than overflow or jump.
    #[test]
    fn the_scanline_keeps_stepping_past_a_u16_phase() {
        let col = |step: u64| scanline_col(0, Beat::at_ms(step * SCANLINE_STEP_MS + 1));
        let glass = crate::layout::SCREEN_GLASS_COLS;
        let glass_w = glass.end() - glass.start() + 1;
        let before = col(u64::from(u16::MAX));
        assert_eq!(col(u64::from(u16::MAX) + 1), (before + 1) % glass_w);
        let at = Beat::at_ms(7 * SCANLINE_STEP_MS);
        assert_eq!(
            scanline_col(3, at),
            (scanline_col(0, at) + 3) % glass_w,
            "each desk a column on from its west neighbour's"
        );
    }
}
