//! Multi-floor office partitioning: the floor arithmetic, the per-floor
//! rendering context ([`FloorCtx`]), the stepped frame every look draws
//! (`step_floor`), the per-floor fade states ([`VacancyDim`], the neon
//! sign's), and the per-office [`CoffeeState`] bookkeeping.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::time::SystemTime;

use crate::pack::OfficeArt;
use crate::physics::{WalkProfile, walk_arrived};
use pixtuoid_core::AgentId;
use pixtuoid_core::sprite::Rgb;
use pixtuoid_core::state::{AgentSlot, FloorLocalDeskIndex, GlobalDeskIndex, SceneState};
use pixtuoid_core::walkable::OccupancyOverlay;

use crate::audio::{AudioCueTracker, AudioFrame};
use crate::chitchat::{ActiveChitchat, VenueKey};
use crate::composite::{BLACK, WHITE, blend_rgb};
use crate::layout::Size;
use crate::pathfind::{AStarRouter, Router};
use crate::pet::{Pet, PetState};
use crate::pose::PoseHistory;
use crate::sim::{SimFrame, SimInputs, SimStores, sim_step};
use crate::theme::Theme;
use crate::walk::WalkState;

mod office;
use office::FloorView;
#[doc(hidden)]
pub use office::{Feed, FloorNav, OfficeSession};

pub use pixtuoid_core::state::MAX_FLOORS;

/// Derive a floor's layout seed from its index — the ONE definition every call
/// site shares, so a floor's look + capacity can't drift between paths.
pub fn floor_seed(floor_idx: usize) -> u64 {
    (floor_idx as u64).wrapping_mul(crate::GOLDEN_GAMMA)
}

/// How many home desks a floor of buffer size `buf_w × buf_h` with `floor_seed`
/// fits. Returns `0` when the buffer is too small for even one cubicle.
pub fn floor_capacity(buf_w: u16, buf_h: u16, floor_seed: u64) -> usize {
    crate::layout::SceneLayout::compute_with_seed(buf_w, buf_h, None, floor_seed)
        .map(|l| l.home_desks.len())
        .unwrap_or(0)
}

/// How many home desks a floor fits when `buf_w × buf_h` PIXELS are painted at
/// `scale` — i.e. the capacity of the logical office those pixels cover.
///
/// [`floor_capacity`] is the `RenderScale::ONE` case. The two are separate
/// functions rather than one defaulted parameter because every existing caller
/// means "buffer pixels are layout units" and must keep meaning it; a painter
/// that adopts a scale opts in at its own call site.
///
/// `#[cfg(test)]` because no painter has opted in yet: this is the vehicle for
/// `render_scale`'s scale-invariance proof, not shipped behaviour. Un-gate it
/// the day a painter passes a scale — the proof is what makes that safe, and
/// leaving it compiled-but-uncalled would have been the same unreached-surface
/// claim the seam itself was flagged for.
#[cfg(test)]
pub(crate) fn floor_capacity_scaled(
    buf_w: u16,
    buf_h: u16,
    scale: crate::render_scale::RenderScale,
    floor_seed: u64,
) -> usize {
    floor_capacity(scale.logical(buf_w), scale.logical(buf_h), floor_seed)
}

/// Per-floor identity + look: index, altitude, and derived layout seed.
#[derive(Debug, Clone, Copy)]
pub struct FloorMeta {
    /// Zero-based floor index.
    pub floor_idx: usize,
    /// How many floors the building has.
    pub floors: usize,
    /// Height fraction: 0.0 (ground) → 1.0 (top floor); drives skyline depth in the windows.
    pub altitude: f32,
    /// This floor's layout seed (`floor_seed(floor_idx)`).
    pub floor_seed: u64,
    /// Which weather its windows show, and its rain sounds.
    pub weather: crate::sky::WeatherPolicy,
    /// How much of its ambient life moves.
    pub motion: crate::anim::Motion,
}

impl FloorMeta {
    /// Metadata for floor `floor_idx` of `total_floors` — altitude spreads 0.0 (ground) → 1.0 (top).
    pub fn for_floor(floor_idx: usize, total_floors: usize) -> Self {
        let altitude = if total_floors <= 1 {
            0.0
        } else {
            floor_idx as f32 / (total_floors - 1) as f32
        };
        // Indoor lighting is deliberately uniform across floors — `altitude`
        // drives only skyline depth in the windows, never a lighting offset.
        Self {
            floor_idx,
            floors: total_floors,
            altitude,
            floor_seed: floor_seed(floor_idx),
            weather: crate::sky::WeatherPolicy::Clock,
            motion: crate::anim::Motion::Full,
        }
    }

    /// This floor under `weather`.
    #[must_use]
    pub fn with_weather(self, weather: crate::sky::WeatherPolicy) -> Self {
        Self { weather, ..self }
    }

    /// This floor moving as `motion` says.
    #[must_use]
    pub fn with_motion(self, motion: crate::anim::Motion) -> Self {
        Self { motion, ..self }
    }

    /// The lone floor of a single-floor office (index 0, altitude 0.0).
    pub fn ground() -> Self {
        Self::for_floor(0, 1)
    }
}

/// Per-floor rendering state — each floor owns its stores, so floors are
/// fully independent.
#[derive(Debug)]
pub struct FloorCtx {
    /// This floor's A\* pathfinder.
    pub(crate) router: AStarRouter,
    /// Per-tick walkable-cell occupancy (routing steers around occupied cells).
    pub(crate) overlay: OccupancyOverlay,
    /// Per-agent pose history for the routed pose derivation.
    pub history: PoseHistory,
    /// This floor's indoor-lighting fade state.
    pub vacancy_dim: VacancyDim,
    /// This floor's neon-sign fade state.
    pub(crate) neon: NeonState,
    /// Per-agent walk state.
    pub walks: HashMap<AgentId, WalkState>,
    /// The pet's and the gateway mascots' walks.
    pub(crate) creatures: HashMap<crate::creatures::CreatureKey, crate::creatures::CreatureWalk>,
    /// What a pointer holds on this floor, and set down since its last step.
    pub(crate) grip: crate::interact::Grip,
    /// Longest in-flight entry- or exit-walk `duration_ms + pause_ms` on this
    /// floor (ms) — drives the door-open cosmetic without a hardcoded window.
    pub door_anim_max_ms: u64,
    /// Whether the last stepped frame changes between beats: see
    /// [`OfficeSession::moves_off_beat`].
    off_beat: bool,
    /// Memo of the last per-frame layout, keyed by the ONLY inputs
    /// `SceneLayout::compute_with_seed` reads on the frame path. Rebuilding it every
    /// frame re-allocs + re-stamps the walkable mask and re-runs the coarse BFS
    /// — the dominant fixed per-frame CPU, quadratic in buffer area.
    layout_memo: Option<((u16, u16, u64), Arc<crate::layout::SceneLayout>)>,
}

impl Default for FloorCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl FloorCtx {
    /// Fresh per-floor state.
    pub(crate) fn new() -> Self {
        Self {
            router: AStarRouter::new(),
            overlay: OccupancyOverlay::new(),
            history: PoseHistory::new(),
            vacancy_dim: VacancyDim::new(),
            neon: NeonState::new(),
            walks: HashMap::new(),
            creatures: HashMap::new(),
            grip: crate::interact::Grip::default(),
            door_anim_max_ms: 0,
            off_beat: false,
            layout_memo: None,
        }
    }

    /// [`OfficeSession::moves_off_beat`] for this floor.
    pub(crate) fn moves_off_beat(&self) -> bool {
        self.off_beat
    }

    /// This floor's stores for one `sim_step`, beside the office's `chitchat`.
    pub(crate) fn sim_stores<'a>(
        &'a mut self,
        chitchat: &'a mut HashMap<VenueKey, ActiveChitchat>,
    ) -> SimStores<'a> {
        SimStores {
            router: &mut self.router,
            overlay: &mut self.overlay,
            history: &mut self.history,
            walks: &mut self.walks,
            vacancy_dim: &mut self.vacancy_dim,
            neon: &mut self.neon,
            chitchat,
            creatures: &mut self.creatures,
            grip: &mut self.grip,
        }
    }

    /// The per-frame layout — memoized `compute_with_seed(w, h, None, seed)` +
    /// the router corridor re-point, the ONE frame prologue every painter rides.
    /// Returns a cheap `Arc` handle so callers can hold it across later
    /// `&mut self` uses without re-cloning the whole `SceneLayout` every frame. A
    /// too-small buffer returns `None` without poisoning the memo.
    pub(crate) fn frame_layout(
        &mut self,
        buf_w: u16,
        buf_h: u16,
        floor_seed: u64,
    ) -> Option<Arc<crate::layout::SceneLayout>> {
        let key = (buf_w, buf_h, floor_seed);
        let layout = match &self.layout_memo {
            Some((k, l)) if *k == key => Arc::clone(l),
            _ => {
                let l = Arc::new(crate::layout::SceneLayout::compute_with_seed(
                    buf_w, buf_h, None, floor_seed,
                )?);
                self.layout_memo = Some((key, Arc::clone(&l)));
                l
            }
        };
        self.router.set_preferred_zone(Some(layout.corridor));
        Some(layout)
    }

    /// Drop per-agent render state for agents no longer in `scene`. Load-bearing
    /// wherever agent ids can RECUR (the web hero's looped script): a returning
    /// id would find its previous life's entry/exit legs (they gate on
    /// `is_none()`) and teleport in instead of walking.
    pub(crate) fn evict_missing(&mut self, scene: &SceneState) {
        self.history.evict_missing(scene);
        self.walks.retain(|id, _| scene.agents.contains_key(id));
    }

    /// Recompute `door_anim_max_ms`: the max `duration_ms + pause_ms` over the
    /// **in-flight** entry/exit profiles only. An ARRIVED profile is excluded
    /// because `WalkState` keeps an agent's `entry` profile for its whole
    /// lifetime — without the gate the door would stay "open" for as long as the
    /// agent lives rather than just while they walk through it.
    pub fn recompute_door_anim_max_ms(&mut self, now: SystemTime) {
        let in_flight = |started_at: SystemTime, p: &WalkProfile| -> u64 {
            let elapsed = crate::anim::elapsed_ms(now, started_at);
            if walk_arrived(p, elapsed) {
                0
            } else {
                p.duration_ms + p.pause_ms
            }
        };
        self.door_anim_max_ms = self.walks.values().fold(0u64, |acc, walk| {
            let entry = walk
                .entry
                .as_ref()
                .map_or(0, |l| in_flight(l.started_at, &l.profile));
            let exit = walk
                .exit
                .as_ref()
                .map_or(0, |leg| in_flight(leg.started_at, &leg.profile));
            acc.max(entry).max(exit)
        });
    }
}

/// Cross-frame coffee bookkeeping: ONE map — an agent holds a desk cup iff its
/// id is a key, and the value is WHEN it was fetched (drives the steam window).
/// Deliberately a single map, not a `HashSet` + `HashMap` pair: cup-without-stamp
/// and stamp-without-cup are unrepresentable instead of merely maintained. One
/// per OFFICE, not per floor — an agent's cup survives floor navigation.
#[derive(Debug, Default)]
pub struct CoffeeState(HashMap<AgentId, SystemTime>);

impl CoffeeState {
    /// Desk-cup steam window (secs) — ONE source of truth for the sim's
    /// desk-cup steam gate and [`record`](CoffeeState::record)'s
    /// refetch-refresh.
    pub(crate) const STEAM_WINDOW_SECS: u64 = 120;

    /// The map view the sim borrows: key = carrier, value = fetch time.
    pub fn map(&self) -> &HashMap<AgentId, SystemTime> {
        &self.0
    }

    /// Force a carrier with a chosen fetch stamp (overwrites) — a seeding seam;
    /// production detection goes through `record`.
    pub fn insert(&mut self, id: AgentId, fetched_at: SystemTime) {
        self.0.insert(id, fetched_at);
    }

    /// Drop coffee state for agents no longer in `scene` — the cup leaves with
    /// the agent.
    pub(crate) fn evict_missing(&mut self, scene: &SceneState) {
        self.0.retain(|id, _| scene.agents.contains_key(id));
    }

    /// Persist newly detected coffee carriers. A carrier re-reported WITHIN the
    /// steam window keeps its stamp — carriers re-report every frame of a
    /// walk-back, and a re-render must not restart an old cup's steam.
    pub(crate) fn record(&mut self, carriers: impl IntoIterator<Item = AgentId>, now: SystemTime) {
        for id in carriers {
            match self.0.entry(id) {
                Entry::Occupied(mut e) => {
                    // Backward clock (duration_since err) reads as not-expired:
                    // keep the old stamp rather than restamping on a clock step.
                    let expired = now
                        .duration_since(*e.get())
                        .is_ok_and(|d| d.as_secs() >= Self::STEAM_WINDOW_SECS);
                    if expired {
                        e.insert(now);
                    }
                }
                Entry::Vacant(v) => {
                    v.insert(now);
                }
            }
        }
    }
}

/// The per-frame EPILOGUE: stamp this frame's new coffee carriers and refresh
/// the door-cosmetic clamp.
fn frame_epilogue(
    fctx: &mut FloorCtx,
    coffee: &mut CoffeeState,
    carriers: impl IntoIterator<Item = pixtuoid_core::AgentId>,
    now: SystemTime,
) {
    coffee.record(carriers, now);
    fctx.recompute_door_anim_max_ms(now);
}

/// A floor's pet and the live interaction with it.
#[derive(Debug, Clone, Copy, Default)]
pub struct PetInputs<'a> {
    /// This floor's configured pet; `None` when no pets are configured or none
    /// maps to this floor seed.
    pub pet: Option<&'a Pet>,
    /// The last petting, if any; honoured only while it plays, for this
    /// floor's pet.
    pub petting: Option<&'a PetState>,
}

/// What one floor shows at one instant — the inputs the frame, the paint pass
/// and the sim share.
#[derive(Debug, Clone, Copy)]
pub struct FloorInputs<'a> {
    /// The scene to render (the full live scene, or a projected single-floor one).
    pub scene: &'a SceneState,
    /// This frame's time — a parameter; the engine never reads the clock (wasm).
    pub now: SystemTime,
    /// This floor's index, altitude, and layout seed.
    pub floor: FloorMeta,
    /// This floor's pet and the live interaction with it.
    pub pets: PetInputs<'a>,
}

/// One floor, one tick, stepped rather than painted: the world advanced, and the
/// layout it advanced on. A second profile paints THIS layout — laying the office
/// out again beside the sim is how a painter ends up drawing one office while the
/// sim walked another.
#[derive(Debug)]
pub struct SteppedFloor {
    /// The layout the sim stepped on.
    pub layout: Arc<crate::layout::SceneLayout>,
    /// The world, advanced one tick.
    pub frame: SimFrame,
}

/// The per-FLOOR half of a painter's persistent session state: the sim
/// stores ([`FloorCtx`]) plus what the floor is drawn into.
#[derive(Debug)]
pub struct PerFloor {
    /// This floor's sim stores.
    pub ctx: FloorCtx,
    /// What this floor is drawn into, in each look.
    pub raster: crate::look::Raster,
}

impl PerFloor {
    /// Hand this floor a pointer's lift, carry or drop; its next step carries
    /// it out. A click is the painter's ([`SceneHit::action`](crate::hit::SceneHit::action)).
    pub(crate) fn grip(&mut self, gesture: &crate::interact::Gesture) {
        self.ctx.grip.push(gesture);
    }

    /// Fresh floor stores, drawn with `pack`.
    pub fn new(pack: Arc<OfficeArt>) -> Self {
        Self {
            ctx: FloorCtx::new(),
            raster: crate::look::Raster::new(pack),
        }
    }

    /// The per-floor half of the dual per-agent eviction protocol. Run with the
    /// FULL live scene.
    pub(crate) fn evict_missing(&mut self, scene: &SceneState) {
        self.ctx.evict_missing(scene);
        self.raster.evict_missing(scene);
    }
}

/// Resolve an occupied-waypoint index to its [`WaypointKind`](crate::layout::WaypointKind)
/// against `layout` — the ONE authored form of the audio cue tracker's kind lookup.
pub(crate) fn waypoint_kind_of(
    layout: Option<&crate::layout::SceneLayout>,
    idx: usize,
) -> Option<crate::layout::WaypointKind> {
    layout.and_then(|l| l.waypoints.get(idx)).map(|w| w.kind)
}

/// Wraps the pure `crate::audio` model into the ONE per-frame [`AudioFrame`]
/// composition every painter shares. Holds the [`AudioCueTracker`] plus the
/// floor it is primed for, so a floor switch reprimes silently.
#[derive(Debug, Default)]
pub(crate) struct AudioObserver {
    cues: AudioCueTracker,
    primed_floor: Option<usize>,
}

impl AudioObserver {
    /// Compose one frame of audio intent for the `floor` being VIEWED, advancing
    /// the cross-frame cue edges. Call it EVERY world-frame regardless of mute
    /// (the painter gates only DELIVERY): a muted stretch keeps
    /// `seen_agents`/`occupied` warm, so re-enabling never fires a
    /// door/appliance volley for what arrived while silent.
    pub(crate) fn frame(
        &mut self,
        scene: &SceneState,
        occupied: &std::collections::HashSet<usize>,
        waypoint_kind: impl Fn(usize) -> Option<crate::layout::WaypointKind>,
        floor: FloorMeta,
        now: SystemTime,
    ) -> AudioFrame {
        let floor_idx = floor.floor_idx;
        // Reprime on floor switch: a fresh tracker primes silently next observe,
        // so riding to a new floor never fires a cue volley for agents /
        // appliances already there.
        if self.primed_floor != Some(floor_idx) {
            self.cues = AudioCueTracker::new();
            self.primed_floor = Some(floor_idx);
        }
        // You hear the floor you're LOOKING AT — but rain stays global, since
        // it's weather, not agent activity.
        let counts = crate::tally::per_floor_counts(scene)[floor_idx.min(MAX_FLOORS - 1)];
        let sky = crate::sky::Sky::at(crate::anim::Motion::Full.timing(now), floor.weather);
        let rain = sky.rain();
        let floor_ids = scene
            .agents
            .iter()
            .filter(|(_, slot)| slot.floor_idx == floor_idx)
            .map(|(id, _)| id);
        let events = self.cues.observe(floor_ids, occupied, waypoint_kind);
        AudioFrame {
            stems: crate::audio::stem_levels(&counts, rain),
            events,
            music: crate::audio::music_controls(&counts, sky.nightfall(), rain),
        }
    }

    /// The floor this observer's cue tracker is currently primed for.
    #[cfg(test)]
    pub(crate) fn primed_floor(&self) -> Option<usize> {
        self.primed_floor
    }
}

/// The per-OFFICE half: cross-frame state that survives floor navigation — ONE
/// per painter surface, shared across every floor.
#[derive(Debug, Default)]
pub struct PerOffice {
    /// Every agent's desk cup + fetch time — survives floor navigation.
    pub coffee: CoffeeState,
    /// In-progress speech bubbles keyed by venue (the `VenueKey` carries `floor_idx`).
    pub(crate) chitchat: HashMap<VenueKey, ActiveChitchat>,
    /// The office-wide [`AudioObserver`] — one cue tracker + reprime latch,
    /// shared across floors.
    pub(crate) audio: AudioObserver,
    /// The office's raster state, shared by every floor's.
    pub raster: crate::look::OfficeRaster,
}

/// The office stores one floor's frame steps and draws with.
#[derive(Debug)]
pub struct OfficeStores<'a> {
    /// See [`PerOffice::coffee`].
    pub(crate) coffee: &'a mut CoffeeState,
    /// See [`PerOffice::chitchat`].
    pub(crate) chitchat: &'a mut HashMap<VenueKey, ActiveChitchat>,
    /// See [`PerOffice::raster`].
    pub(crate) raster: &'a mut crate::look::OfficeRaster,
}

impl OfficeStores<'_> {
    /// The same stores for one more frame.
    pub fn reborrow(&mut self) -> OfficeStores<'_> {
        OfficeStores {
            coffee: self.coffee,
            chitchat: self.chitchat,
            raster: self.raster,
        }
    }
}

impl PerOffice {
    /// Empty office state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every store a floor's frame uses.
    pub fn stores(&mut self) -> OfficeStores<'_> {
        OfficeStores {
            coffee: &mut self.coffee,
            chitchat: &mut self.chitchat,
            raster: &mut self.raster,
        }
    }

    /// The office half of the dual eviction: the coffee, and the cutaway's
    /// recoloured figures. `chitchat` is deliberately untouched —
    /// conversations self-expire inside `chitchat::update_and_collect`, so
    /// there is no per-agent entry to leak.
    pub(crate) fn evict_missing(&mut self, scene: &SceneState) {
        self.coffee.evict_missing(scene);
        self.raster.evict_missing(scene);
    }
}

/// The OWNED single-floor painter session: one floor's view + one
/// [`PerOffice`] plus the dual `evict_missing` protocol behind one type, so a
/// painter can't hand-roll (and silently skip) the eviction — a skipped
/// eviction leaks per-agent state or teleports a recurring agent. An office
/// of several floors is an [`OfficeSession`].
#[derive(Debug)]
pub struct FloorSession {
    /// This session's single floor — its sim stores + raster, and its last frame.
    view: FloorView,
    /// The office-wide cross-frame state (coffee, chitchat, audio) shared across floors.
    pub office: PerOffice,
}

impl FloorSession {
    /// An empty session drawing with `pack` — fresh floor + office state,
    /// nothing laid out yet.
    pub fn new(pack: Arc<OfficeArt>) -> Self {
        Self {
            view: FloorView::new(pack),
            office: PerOffice::default(),
        }
    }

    /// This session's floor: its sim stores + raster.
    pub fn floor(&self) -> &PerFloor {
        &self.view.floor
    }

    /// [`Self::floor`], to seed or inspect its stores.
    #[cfg(test)]
    pub(crate) fn floor_mut(&mut self) -> &mut PerFloor {
        &mut self.view.floor
    }

    /// Drop per-agent state for agents no longer in `scene` — BOTH halves of the
    /// dual eviction. `scene` must be the FULL live scene: a PROJECTED
    /// per-floor one holds no other floor's agents, so evicting against it
    /// would wipe their state.
    pub(crate) fn evict_missing(&mut self, scene: &SceneState) {
        self.view.floor.evict_missing(scene);
        self.office.evict_missing(scene);
    }

    /// Render one frame in `look`: the dual eviction, then
    /// [`look::render`](crate::look::render); its picture is
    /// [`Self::picture`]. `scene` MUST be the full live scene — the session
    /// evicts against it.
    pub fn render(&mut self, look: crate::look::Look, inputs: crate::look::RenderInputs<'_>) {
        self.evict_missing(inputs.world.scene);
        self.view.render(&mut self.office, look, inputs);
    }

    /// The last frame [`Self::render`] drew; `None` before the first, and
    /// when it could not lay out.
    pub fn picture(&self) -> Option<crate::look::FloorPicture<'_>> {
        self.view.picture(0, false)
    }

    /// The [`wall_board`](crate::neon_sign::wall_board) of `scene`, a one-floor office.
    pub fn board(
        &self,
        scene: &SceneState,
        motion: crate::anim::Motion,
        now: SystemTime,
    ) -> crate::neon_sign::BoardModel {
        crate::neon_sign::wall_board(
            scene,
            crate::tally::office_gateway(scene),
            None,
            motion,
            now,
        )
    }

    /// [`OfficeSession::moves_off_beat`] for this session's one floor.
    #[cfg(test)]
    pub(crate) fn moves_off_beat(&self) -> bool {
        self.view.floor.ctx.moves_off_beat()
    }

    /// One frame of audio intent for THIS session's last render, fed from the
    /// session's OWN occupancy + layout so a painter can't hand a mismatched
    /// occupancy/kind pair. Call it EVERY frame regardless of mute (see
    /// `AudioObserver::frame`).
    pub fn audio_frame(
        &mut self,
        scene: &SceneState,
        floor: FloorMeta,
        now: SystemTime,
    ) -> AudioFrame {
        self.view.audio_frame(&mut self.office, scene, floor, now)
    }

    /// Advance the world one tick WITHOUT painting: the session's eviction, then
    /// `step_floor`. `size` is the layout's logical extent, whatever scale a
    /// painter draws it at. `None` when the size can't lay out.
    pub fn step(&mut self, world: FloorInputs<'_>, size: Size) -> Option<SteppedFloor> {
        self.evict_missing(world.scene);
        step_floor(
            &mut self.view.floor,
            &mut self.office.coffee,
            &mut self.office.chitchat,
            world,
            size,
        )
    }
}

/// [`FloorSession::step`] minus eviction, which a projected `world.scene` would turn on other floors.
pub(crate) fn step_floor(
    floor: &mut PerFloor,
    coffee: &mut CoffeeState,
    chitchat: &mut HashMap<VenueKey, ActiveChitchat>,
    world: FloorInputs<'_>,
    size: Size,
) -> Option<SteppedFloor> {
    let PerFloor { ctx: fctx, raster } = floor;
    let layout = fctx.frame_layout(size.w, size.h, world.floor.floor_seed)?;
    let door_anim_max_ms = fctx.door_anim_max_ms;
    let frame = sim_step(
        &mut fctx.sim_stores(chitchat),
        SimInputs {
            world,
            pack: raster.pack(),
            layout: &layout,
            coffee: coffee.map(),
            door_anim_max_ms,
        },
    );
    frame_epilogue(
        fctx,
        coffee,
        frame.new_coffee_carriers.iter().copied(),
        world.now,
    );
    fctx.off_beat = frame
        .poses
        .values()
        .flatten()
        .any(|pose| matches!(pose, crate::pose::Pose::Walking { .. }))
        || fctx.creatures.values().any(|w| w.walks_at(world.now))
        || fctx.neon.fading(world.now)
        || fctx.vacancy_dim.easing();
    Some(SteppedFloor { layout, frame })
}

/// Per-floor indoor-lighting fade state: an emptied floor holds full light for
/// `EMPTY_DEBOUNCE_MS` (so agents briefly disappearing between transcripts don't
/// flicker it) then eases toward `MIN_LEVEL`; repopulating snaps the target
/// straight back to 1.0.
#[derive(Debug)]
pub struct VacancyDim {
    level: f32,
    empty_since: Option<SystemTime>,
    last_update: Option<SystemTime>,
    dimmed: bool,
}

impl Default for VacancyDim {
    fn default() -> Self {
        Self::new()
    }
}

impl VacancyDim {
    /// Floor of the smoothed lit level — an empty floor dims to here, never to black.
    pub(crate) const MIN_LEVEL: f32 = 0.10;
    /// How long an emptied floor holds full light before it starts fading (ms).
    pub const EMPTY_DEBOUNCE_MS: u64 = 5_000;
    /// Time constant of the exponential lit-level ease (ms).
    pub(crate) const FADE_TAU_MS: u64 = 800;
    /// How near its target the ease lands on it: under one 8-bit channel step.
    const SETTLED: f32 = 1.0 / u8::MAX as f32;
    /// A fully-lit floor (level 1.0), no fade in progress.
    pub(crate) fn new() -> Self {
        Self {
            level: 1.0,
            empty_since: None,
            last_update: None,
            dimmed: false,
        }
    }

    /// Whether the empty-debounce has run out — the floor's VERDICT that it is
    /// really empty. Read this, never `level() < 1.0`: the level trails the
    /// verdict by the length of its ease.
    pub(crate) fn dimmed(&self) -> bool {
        self.dimmed
    }

    /// Current smoothed lit level in `[MIN_LEVEL, 1.0]`.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Whether the level has yet to land on its target.
    pub(crate) fn easing(&self) -> bool {
        let target = if self.dimmed { Self::MIN_LEVEL } else { 1.0 };
        self.level != target
    }

    /// Force the steady-state empty look, bypassing the debounce + ease — static
    /// snapshots want it, not frame-0 of the fade. The debounce is back-dated too,
    /// so the next tick still JUDGES the floor empty instead of re-arming it.
    pub fn snap_to_empty(&mut self) {
        self.level = Self::MIN_LEVEL;
        self.empty_since = Some(SystemTime::UNIX_EPOCH);
        self.dimmed = true;
    }

    /// Advance the fade one frame. Returns the new lit level in
    /// `[MIN_LEVEL, 1.0]`.
    pub(crate) fn tick(&mut self, empty: bool, now: SystemTime) -> f32 {
        let target = if empty {
            let since = *self.empty_since.get_or_insert(now);
            let elapsed = crate::anim::elapsed_ms(now, since);
            if elapsed >= Self::EMPTY_DEBOUNCE_MS {
                Self::MIN_LEVEL
            } else {
                1.0
            }
        } else {
            self.empty_since = None;
            1.0
        };
        self.dimmed = target < 1.0;

        let dt_ms = self
            .last_update
            .and_then(|prev| now.duration_since(prev).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last_update = Some(now);

        let alpha = 1.0 - (-(dt_ms as f32) / Self::FADE_TAU_MS as f32).exp();
        self.level += (target - self.level) * alpha.clamp(0.0, 1.0);
        // The ease ends rather than stepping a stray channel forever.
        if (target - self.level).abs() < Self::SETTLED {
            self.level = target;
        }
        self.level
    }
}

/// The neon sign's light for one frame — theme-free, like
/// [`crate::sim::CharacterGlow`]: the sim decides HOW LIT and how ALARMED
/// the sign is, paint maps that to colors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct NeonLevels {
    /// 0 = the brand hue, 1 = the "someone needs you" hue.
    pub(crate) alert: f32,
    /// How hard the tube is driven, 0..=1.
    pub(crate) power: f32,
}

impl NeonLevels {
    /// Someone waits on the user.
    pub(crate) const ALERT: Self = Self {
        alert: 1.0,
        power: 1.0,
    };
    /// Agents work, nobody waits.
    pub(crate) const RUNNING: Self = Self {
        alert: 0.0,
        power: 1.0,
    };
    /// Everyone present is idle.
    pub(crate) const CALM: Self = Self {
        alert: 0.0,
        power: 0.62,
    };
    /// Nobody home: the tube barely holds, and stutters ([`NeonState::tick`]).
    pub(crate) const EMPTY: Self = Self {
        alert: 0.0,
        power: 0.16,
    };
    /// A stutter's flash: the starved tube catching for an instant.
    pub(crate) const FLASH: Self = Self {
        alert: 0.0,
        power: 0.9,
    };

    fn lerp(self, to: Self, t: f32) -> Self {
        Self {
            alert: self.alert + (to.alert - self.alert) * t,
            power: self.power + (to.power - self.power) * t,
        }
    }
}

/// The neon sign's colors for one frame: a bright TUBE, a colored HALO that
/// spills onto the wall and whatever hangs there, and a faintly tinted interior.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct NeonLook {
    pub tube: Rgb,
    pub interior: Rgb,
    pub halo: Rgb,
}

/// A lit tube is its hue pushed this far toward white — the core of a real neon
/// reads near-white, the COLOR lives in the halo.
const NEON_TUBE_WHITEN: f32 = 0.38;
/// How much of the hue the dark interior picks up at full power.
const NEON_INTERIOR_TINT: f32 = 0.07;
/// Map the sim's theme-free `levels` to this frame's colors; how strongly the
/// halo throws them is the [`Lights`](crate::lighting::Lights)' call.
pub(crate) fn neon_look(levels: NeonLevels, theme: &Theme) -> NeonLook {
    let power = levels.power;
    let hue = theme.ui.neon_brand.mix(theme.ui.neon_alert, levels.alert);
    NeonLook {
        tube: blend_rgb(BLACK, blend_rgb(hue, WHITE, NEON_TUBE_WHITEN), power),
        interior: blend_rgb(theme.office.neon_panel_bg, hue, NEON_INTERIOR_TINT * power),
        halo: hue,
    }
}

/// Per-floor neon-sign fade state: a mood change eases the light over
/// [`NeonState::FADE_MS`] instead of snapping the hue. A sign with no recent light
/// to ease FROM — its first tick, or one nobody has drawn for longer than a fade
/// (a still's warm-up step, a floor switched back to) — snaps to the mood's own.
#[derive(Debug, Default)]
pub(crate) struct NeonState {
    fade: Option<NeonFade>,
    last_tick: Option<SystemTime>,
    /// The loop time the last tick read, which the stutter steps by.
    last_beat_ms: Option<u64>,
    /// Whether the last tick's light was a stutter's flash.
    stutter: bool,
}

#[derive(Debug)]
struct NeonFade {
    from: NeonLevels,
    to: NeonLevels,
    started_at: SystemTime,
}

impl NeonFade {
    fn at(&self, now: SystemTime) -> NeonLevels {
        let t = crate::anim::eased_progress(
            self.started_at,
            NeonState::FADE_MS,
            crate::anim::Easing::EaseInOutCubic,
            now,
        );
        // `to` EXACTLY at the end: `tick`'s stutter gate compares by equality.
        if t >= 1.0 {
            self.to
        } else {
            self.from.lerp(self.to, t)
        }
    }
}

// Every stutter bound on a whole Full tick, so the beat neither skips a flash
// nor stretches one; every flash, and the dark up to the next (the cycle's
// first, past its end), at least the photosensitive floor.
const _: () = {
    use crate::anim::{FULL_TICK_MS, PHOTOSENSITIVE_PHASE_MIN_MS};
    let flashes = NeonState::STUTTER_FLASHES_MS;
    let mut i = 0;
    while i < flashes.len() {
        let (start, end) = flashes[i];
        let next = if i + 1 < flashes.len() {
            flashes[i + 1].0
        } else {
            flashes[0].0 + NeonState::STUTTER_MS
        };
        assert!(start % FULL_TICK_MS == 0 && end % FULL_TICK_MS == 0);
        assert!(end - start >= PHOTOSENSITIVE_PHASE_MIN_MS);
        assert!(next - end >= PHOTOSENSITIVE_PHASE_MIN_MS);
        i += 1;
    }
};

impl NeonState {
    /// How long a mood change takes to cross over (ms).
    pub(crate) const FADE_MS: u32 = 1_600;
    /// A starved tube's stutter cycle (ms).
    const STUTTER_MS: u64 = 5_000;
    /// The flash windows inside one cycle, `[start, end)` ms, each on whole
    /// Full beats.
    const STUTTER_FLASHES_MS: [(u64, u64); 4] = [
        (1_875, 2_000),
        (2_125, 2_250),
        (2_375, 2_500),
        (4_125, 4_250),
    ];

    /// A sign that has not been lit yet.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether a mood change is still crossing over at `now`.
    pub(crate) fn fading(&self, now: SystemTime) -> bool {
        self.fade
            .as_ref()
            .is_some_and(|f| crate::anim::elapsed_ms(now, f.started_at) < u64::from(Self::FADE_MS))
    }

    fn stutter_flash(beat: crate::anim::Beat) -> bool {
        if beat.is_rest() {
            return false;
        }
        let t = beat.ms() % Self::STUTTER_MS;
        Self::STUTTER_FLASHES_MS
            .iter()
            .any(|&(start, end)| (start..end).contains(&t))
    }

    /// The longest loop-time step a flash can still be drawn across: the
    /// shortest flash. A painter stepping further, such as a still, would skip
    /// some flashes and hold others, so it gets the steady starved tube
    /// instead.
    fn shortest_flash_ms() -> u64 {
        Self::STUTTER_FLASHES_MS
            .iter()
            .map(|&(start, end)| end - start)
            .fold(u64::MAX, u64::min)
    }

    /// Advance to `mood` on `timing`; returns this frame's light. A count change
    /// inside one mood is not a change, and a reversal mid-fade restarts from the
    /// light it interrupted.
    ///
    /// `room_dimmed` is [`VacancyDim::dimmed`]: the sign only starves once the
    /// ROOM is judged empty, so that debounce is the one "is the office really
    /// empty" clock — a gap between transcripts, or the last agent still walking
    /// out of a lit room, can't drop the sign.
    pub(crate) fn tick(
        &mut self,
        mood: crate::neon_sign::OfficeMood,
        room_dimmed: bool,
        timing: crate::anim::Timing,
    ) -> NeonLevels {
        use crate::neon_sign::OfficeMood;
        let now = timing.now;
        let to = match mood {
            OfficeMood::Alert { .. } => NeonLevels::ALERT,
            OfficeMood::Running { .. } => NeonLevels::RUNNING,
            OfficeMood::Empty if room_dimmed => NeonLevels::EMPTY,
            OfficeMood::Calm | OfficeMood::Empty => NeonLevels::CALM,
        };
        let gap_ms = self
            .last_tick
            .map(|last| crate::anim::elapsed_ms(now, last));
        self.last_tick = Some(now);
        // Stepped in loop time, which `Motion::Calm` paces slower than the wall
        // clock, so Calm plays the stutter slower rather than never.
        let beat_ms = timing.beat.ms();
        let step_ms = self.last_beat_ms.map(|last| beat_ms.abs_diff(last));
        self.last_beat_ms = Some(beat_ms);
        let recent = gap_ms.is_some_and(|gap| gap <= u64::from(Self::FADE_MS));
        let current = match &self.fade {
            Some(fade) if recent && fade.to == to => fade.at(now),
            Some(fade) if recent => {
                let current = fade.at(now);
                self.fade = Some(NeonFade {
                    from: current,
                    to,
                    started_at: now,
                });
                current
            }
            _ => {
                self.fade = Some(NeonFade {
                    from: to,
                    to,
                    started_at: now,
                });
                to
            }
        };
        // Only a tube that has LANDED on starved stutters, not one coasting down.
        let drawable = step_ms.is_some_and(|step| step <= Self::shortest_flash_ms());
        self.stutter = current == NeonLevels::EMPTY && drawable && Self::stutter_flash(timing.beat);
        if self.stutter {
            NeonLevels::FLASH
        } else {
            current
        }
    }

    /// Whether the last [`tick`](Self::tick)'s light was a stutter's flash.
    pub(crate) fn stutters(&self) -> bool {
        self.stutter
    }
}

/// Animated floor-switch transition.
#[derive(Debug)]
pub struct FloorTransition {
    /// The floor being slid away FROM.
    pub(crate) from_floor: usize,
    /// The floor being slid TO.
    pub to_floor: usize,
    /// When the slide began.
    pub(crate) started_at: SystemTime,
    /// Slide duration (ms).
    pub duration_ms: u64,
}

const TRANSITION_DURATION_MS: u64 = 900;

impl FloorTransition {
    /// Start a slide from floor `from` to floor `to` at `now`.
    pub(crate) fn new(from: usize, to: usize, now: SystemTime) -> Self {
        Self {
            from_floor: from,
            to_floor: to,
            started_at: now,
            duration_ms: TRANSITION_DURATION_MS,
        }
    }

    /// Progress ratio 0.0 → 1.0 with ease-in-out curve.
    pub(crate) fn t(&self, now: SystemTime) -> f32 {
        crate::anim::eased_progress(
            self.started_at,
            self.duration_ms as u32,
            crate::anim::Easing::EaseInOutCubic,
            now,
        )
    }

    /// Whether the slide has finished (or a backward clock step past its duration ends it).
    pub(crate) fn is_done(&self, now: SystemTime) -> bool {
        // Backward-clock escape: `t` saturates to 0 while `now < started_at`, so
        // a wall-clock step back (NTP correction, suspend) would otherwise wedge
        // the renderer in the transition composite — no labels, tooltips,
        // chitchat, or hit-testing — until the clock re-passes started_at. A step
        // larger than the transition's own duration can't be render-loop jitter;
        // treat it as done. Smaller wobbles keep the saturate-to-0 convention
        // every other animation uses.
        if let Ok(behind) = self.started_at.duration_since(now)
            && behind.as_millis() as u64 > self.duration_ms
        {
            return true;
        }
        self.t(now) >= 1.0
    }
}

/// The floors `scene`'s agents sit on: one past the highest, at least one.
pub fn num_floors(scene: &SceneState) -> usize {
    scene
        .agents
        .values()
        .map(|a| a.floor_idx + 1)
        .max()
        .unwrap_or(1)
}

/// One agent projected onto a floor by [`build_floor_scene`]. The floor-local
/// offset rides a SEPARATE `desk` field rather than being written back into
/// `AgentSlot.desk_index`, which keeps that field's GLOBAL type honest until
/// [`project_floor_scene`] re-hosts the slot.
#[derive(Debug)]
pub(crate) struct ProjectedSlot {
    /// The projected agent — its `desk_index` still the ORIGINAL global allocation.
    pub(crate) slot: AgentSlot,
    /// The agent's desk remapped into this floor's local `[0..capacity)` space.
    pub(crate) desk: FloorLocalDeskIndex,
}

/// Extract agents belonging to `floor_idx`, pairing each with its desk remapped
/// into the floor's `[0..capacity)` LOCAL space so the layout engine sees a
/// self-contained floor. Uses the stored `floor_idx` on each slot so capacity
/// growth never migrates agents between floors.
pub(crate) fn build_floor_scene(scene: &SceneState, floor_idx: usize) -> Vec<ProjectedSlot> {
    let offset = scene.floor_range(floor_idx).start;
    scene
        .agents
        .values()
        .filter(|a| a.floor_idx == floor_idx)
        .filter_map(|a| {
            if a.desk_index.0 < offset {
                return None;
            }
            Some(ProjectedSlot {
                slot: a.clone(),
                desk: FloorLocalDeskIndex(a.desk_index.0 - offset),
            })
        })
        .collect()
}

/// Build a self-contained `SceneState` for one floor: a `uniform(cap)` scene, so
/// floor arithmetic stays self-consistent with the remapped desk indices in
/// `[0..cap)`. A floor past the building's is an empty one.
pub fn project_floor_scene(scene: &SceneState, floor_idx: usize) -> SceneState {
    let cap = scene.floor_capacities.get(floor_idx).copied().unwrap_or(0);
    let mut s = SceneState::uniform(cap);
    for p in build_floor_scene(scene, floor_idx) {
        let mut slot = p.slot;
        // The RE-HOST, not a space mix-up: this `uniform(cap)` single-floor
        // scene's global desk space coincides with its floor-0 local space by
        // construction, so the floor-local desk IS a genuinely valid
        // `GlobalDeskIndex` FOR THIS SMALLER SCENE.
        slot.desk_index = GlobalDeskIndex(p.desk.0);
        s.agents.insert(slot.agent_id, slot);
    }
    // Daemon presences are global, not per-desk — ground floor only, so the
    // mascot renders exactly once.
    if floor_idx == 0 {
        s.clone_daemons_from(scene);
    }
    s
}

#[cfg(test)]
mod tests;
