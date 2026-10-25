//! A painter's multi-floor office: every floor's view, the office-wide state
//! they share, which floor shows, and the slide between two — the half of a
//! floor switch no painter's own medium owns. [`FloorSession`] is its
//! one-floor case.
//!
//! [`FloorSession`]: super::FloorSession

use std::collections::HashSet;
use std::sync::Arc;
use std::time::SystemTime;

use crate::pack::OfficeArt;
use pixtuoid_core::SceneState;
use pixtuoid_core::sprite::{Rgb, RgbBuffer};

use super::{
    AudioFrame, FloorInputs, FloorMeta, FloorTransition, PerFloor, PerOffice, waypoint_kind_of,
};
use crate::footer::FooterFloor;

/// One floor of a painter's office: its raster and sim stores, and what its
/// last frame laid out, occupied and flashed.
#[derive(Debug)]
pub(crate) struct FloorView {
    pub(super) floor: PerFloor,
    /// What the last frame answers a pointer with, its layout among it.
    last_hits: Option<crate::look::Hits>,
    /// REPLACED each frame, never extended: the cue tracker fires on edges, so
    /// an accumulating set would re-report stale waypoints forever.
    pub(super) last_occupied: HashSet<usize>,
    last_flash: crate::screen::FlashPhase,
    /// Where the last frame may differ from the one before it.
    last_dirty: crate::look::Dirty,
}

impl FloorView {
    pub(super) fn new(pack: Arc<OfficeArt>) -> Self {
        Self {
            floor: PerFloor::new(pack),
            last_hits: None,
            last_occupied: HashSet::new(),
            last_flash: crate::screen::FlashPhase::default(),
            last_dirty: crate::look::Dirty::All,
        }
    }

    /// Draw `inputs` in `look` beside the office-wide `office`, eviction
    /// aside; a size that can't lay out keeps nothing of the last.
    pub(super) fn render(
        &mut self,
        office: &mut PerOffice,
        look: crate::look::Look,
        inputs: crate::look::RenderInputs<'_>,
    ) {
        match crate::look::render(&mut self.floor, office.stores(), look, inputs) {
            Some(frame) => {
                self.last_hits = Some(frame.hits);
                self.last_occupied = frame.occupied_waypoints;
                self.last_flash = frame.flash;
                self.last_dirty = frame.dirty;
            }
            None => self.forget(),
        }
    }

    /// Keep nothing of the last frame: one that drew no office.
    fn forget(&mut self) {
        self.last_hits = None;
        self.last_occupied.clear();
        self.last_flash = crate::screen::FlashPhase::default();
        self.last_dirty = crate::look::Dirty::All;
    }

    /// The last frame, as floor `floor` of its office, differing anywhere
    /// from the frame before it when `fresh`; `None` when it drew no office.
    pub(super) fn picture(
        &self,
        floor: usize,
        fresh: bool,
    ) -> Option<crate::look::FloorPicture<'_>> {
        let dirty = if fresh {
            &crate::look::Dirty::All
        } else {
            &self.last_dirty
        };
        self.floor
            .raster
            .floor_picture(floor, self.last_hits.as_ref()?, self.last_flash, dirty)
    }

    /// One frame of audio intent from this floor's last frame, fed from its
    /// OWN occupancy and layout so a painter can't hand a mismatched pair.
    pub(super) fn audio_frame(
        &self,
        office: &mut PerOffice,
        scene: &SceneState,
        floor: FloorMeta,
        now: SystemTime,
    ) -> AudioFrame {
        let layout = self.last_hits.as_ref().map(|hits| &**hits.layout());
        office.audio.frame(
            scene,
            &self.last_occupied,
            |idx| waypoint_kind_of(layout, idx),
            floor,
            now,
        )
    }

    pub(super) fn flash(&self) -> crate::screen::FlashPhase {
        self.last_flash
    }

    pub(super) fn buf(&self) -> Option<&RgbBuffer> {
        self.floor.raster.pixels()
    }

    pub(super) fn moves_off_beat(&self) -> bool {
        self.floor.ctx.moves_off_beat()
    }
}

/// What a frame of an office feeds besides its picture
/// ([`OfficeSession::picture`]), whether or not a screen shows that.
#[doc(hidden)]
#[must_use]
#[derive(Debug)]
pub struct Feed {
    /// Its audio intent.
    pub audio: AudioFrame,
    /// What the floor showing was drawn under, for a painter's jank report.
    #[doc(hidden)]
    pub note: Option<crate::look::FrameNote>,
}

/// Which floor an office shows, and the slide to another: what every painter's
/// floor keys and frame loop share. Navigation never starts a slide during
/// one, and a slide lands on its destination however it ends.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct FloorNav {
    current: usize,
    transition: Option<FloorTransition>,
}

impl FloorNav {
    /// The floor showing, or the one a slide leaves.
    pub fn current(&self) -> usize {
        self.current
    }

    /// The one floor on screen: none during a slide.
    pub(crate) fn showing(&self) -> Option<usize> {
        self.transition.is_none().then_some(self.current)
    }

    /// The slide under way, if any.
    pub fn transition(&self) -> Option<&FloorTransition> {
        self.transition.as_ref()
    }

    /// Slide to `target`, unless it is the floor showing or a slide is under
    /// way; whether a slide began.
    pub(crate) fn navigate(&mut self, target: usize, now: SystemTime) -> bool {
        if target == self.current || self.transition.is_some() {
            return false;
        }
        self.transition = Some(FloorTransition::new(self.current, target, now));
        true
    }

    /// End a slide at once on its destination, of `n_floors`: a cancel (a
    /// resize) must not silently revert a navigation.
    pub(crate) fn cancel(&mut self, n_floors: usize) {
        if let Some(tr) = self.transition.take() {
            self.current = tr.to_floor.min(n_floors - 1);
        }
    }

    /// The navigation over `n_floors` at `now`: a slide to or from a floor
    /// gone is dropped, a finished one lands, and the floor showing stays in
    /// the building.
    pub(crate) fn settle(&mut self, n_floors: usize, now: SystemTime) {
        self.transition
            .take_if(|tr| tr.from_floor >= n_floors || tr.to_floor >= n_floors);
        if let Some(tr) = self.transition.take_if(|tr| tr.is_done(now)) {
            self.current = tr.to_floor;
        }
        if self.current >= n_floors {
            self.current = n_floors - 1;
        }
    }
}

/// Floor `floor` of `n_floors` under `office`'s weather and motion.
fn floor_meta(office: FloorMeta, floor: usize, n_floors: usize) -> FloorMeta {
    FloorMeta::for_floor(floor, n_floors)
        .with_weather(office.weather)
        .with_motion(office.motion)
}

/// The pet of `pets` floor `floor` of `n_floors` draws: the one choice its
/// frames and its tooltip both read.
fn floor_pet(floor: usize, n_floors: usize, pets: &[crate::pet::Pet]) -> Option<&crate::pet::Pet> {
    crate::pet::select_pet_for_floor(FloorMeta::for_floor(floor, n_floors).floor_seed, pets)
}

/// `world` as floor `floor` of `n_floors` sees it, `floor_scene` its
/// projection: its index, altitude and pet (of `pets`) the office's, and a
/// petting playing only on its own floor while it lasts.
fn floor_world<'a>(
    world: FloorInputs<'a>,
    floor_scene: &'a SceneState,
    floor: usize,
    n_floors: usize,
    pets: &'a [crate::pet::Pet],
) -> FloorInputs<'a> {
    let now = world.now;
    FloorInputs {
        scene: floor_scene,
        floor: floor_meta(world.floor, floor, n_floors),
        pets: super::PetInputs {
            pet: floor_pet(floor, n_floors, pets),
            petting: world
                .pets
                .petting
                .filter(|p| p.floor_idx == floor && p.is_active(now)),
        },
        ..world
    }
}

/// The footer's floor breadcrumb for floor `current` of `n_floors`, among
/// `total_agents`; `None` in a one-floor office.
pub(crate) fn footer_floor(
    current: usize,
    n_floors: usize,
    total_agents: usize,
) -> Option<FooterFloor> {
    (n_floors > 1).then_some(FooterFloor {
        current: current + 1,
        total_floors: n_floors,
        total_agents,
    })
}

/// How far down the floor being left and the one arriving sit, `t` through a
/// slide over a scene `h` units tall, a divider's gap between them.
fn slide_offsets(t: f32, going_down: bool, h: f32) -> (i32, i32) {
    // `t` applies to the total travel (screen height + divider gap) so the
    // easing covers the full distance including the gap.
    const FLOOR_SLIDE_DIVIDER_FRACTION: f32 = 5.0;
    let divider_h = h / FLOOR_SLIDE_DIVIDER_FRACTION;
    let total = h + divider_h;
    if going_down {
        // Higher floor: current slides DOWN, new enters from TOP
        let from_y = (t * total) as i32;
        let to_y = -(total - t * total) as i32;
        (from_y, to_y)
    } else {
        // Lower floor: current slides UP, new enters from BOTTOM
        let from_y = -(t * total) as i32;
        let to_y = (total - t * total) as i32;
        (from_y, to_y)
    }
}

/// `leaving` and `arriving` placed as [`slide_offsets`] places them, `t`
/// through a slide, into `into` (resized to `leaving`'s size), `gap` where
/// neither reaches.
fn compose_slide(
    into: &mut RgbBuffer,
    (leaving, arriving): (&RgbBuffer, &RgbBuffer),
    t: f32,
    going_down: bool,
    gap: Rgb,
) {
    let (w, h) = (leaving.width(), leaving.height());
    let offsets = slide_offsets(t, going_down, f32::from(h));
    into.resize_fill(w, h, gap);
    for (buf, dy) in [(leaving, offsets.0), (arriving, offsets.1)] {
        for y in 0..h {
            let src = i32::from(y) - dy;
            if let Ok(src) = u16::try_from(src)
                && src < buf.height()
            {
                for x in 0..w.min(buf.width()) {
                    into.put(x, y, buf.get(x, src));
                }
            }
        }
    }
}

/// A painter's office of up to [`MAX_FLOORS`](super::MAX_FLOORS) floors:
/// each floor's [`FloorView`], the office-wide state they share (a cup of
/// coffee survives a floor switch), the [`FloorNav`], and the slide's
/// composed frame.
#[doc(hidden)]
#[derive(Debug)]
pub struct OfficeSession {
    pack: Arc<OfficeArt>,
    /// Each floor's view, kept past the last frame's floor count so a floor
    /// that empties and fills again resumes its own.
    views: Vec<FloorView>,
    /// The floors the last frame's scene filled, at least one.
    n_floors: usize,
    office: PerOffice,
    nav: FloorNav,
    /// The last frame's slide, while one is under way: its composed pixels,
    /// and what they flash.
    slide: Option<(RgbBuffer, crate::screen::Flashes)>,
    /// The floor (or slide) the frame before showed, and whether the last
    /// frame showed another: a floor's own dirt is against ITS last frame,
    /// not the one on screen.
    shown: Option<usize>,
    shown_changed: bool,
    gripped: crate::interact::GripFloor,
}

impl OfficeSession {
    /// An empty office drawing with `pack`.
    pub fn new(pack: Arc<OfficeArt>) -> Self {
        Self {
            views: vec![FloorView::new(Arc::clone(&pack))],
            n_floors: 1,
            pack,
            office: PerOffice::default(),
            nav: FloorNav::default(),
            slide: None,
            shown: None,
            shown_changed: true,
            gripped: crate::interact::GripFloor::default(),
        }
    }

    /// Which floor shows, and the slide under way.
    pub fn nav(&self) -> &FloorNav {
        &self.nav
    }

    /// Slide to floor `target`: [`FloorNav::navigate`].
    pub fn navigate(&mut self, target: usize, now: SystemTime) -> bool {
        self.nav.navigate(target, now)
    }

    /// Hand a pointer's lift to the floor showing, and its carry and drop to
    /// the floor it lifted on, whatever shows since: [`PerFloor::grip`]. A
    /// lift during a slide, which shows no one floor, lifts nothing.
    pub fn grip(&mut self, gesture: &crate::interact::Gesture) {
        let floor = self.gripped.of(gesture, &self.nav);
        if let Some(view) = floor.and_then(|f| self.views.get_mut(f)) {
            view.floor.grip(gesture);
        }
    }

    /// The floors the last frame's scene filled.
    pub fn n_floors(&self) -> usize {
        self.n_floors
    }

    /// The pet of `pets` the floor showing picks, as its frames draw it.
    pub fn showing_pet<'p>(&self, pets: &'p [crate::pet::Pet]) -> Option<&'p crate::pet::Pet> {
        floor_pet(self.nav.current(), self.n_floors, pets)
    }

    /// Ready the office for a frame of `scene` (the FULL live scene) at
    /// `now`: every floor and the office drop the agents gone, a floor the
    /// scene fills gets its view, and the navigation settles.
    /// [`Self::render`] does this itself; a painter drawing the floors on its
    /// own calls it first.
    pub fn prepare(&mut self, scene: &SceneState, now: SystemTime) {
        self.evict_missing(scene);
        self.n_floors = super::num_floors(scene).clamp(1, super::MAX_FLOORS);
        while self.views.len() < self.n_floors {
            self.views.push(FloorView::new(Arc::clone(&self.pack)));
        }
        self.nav.settle(self.n_floors, now);
    }

    /// Drop the agents gone from `scene` from every floor (an agent's floor
    /// need not be the one showing) and from the office's coffee and
    /// chitchat: one seam, so no frame path skips either half.
    pub fn evict_missing(&mut self, scene: &SceneState) {
        for view in &mut self.views {
            view.floor.evict_missing(scene);
        }
        self.office.evict_missing(scene);
    }

    /// `world` as floor `floor` sees it, `floor_scene` its projection: the
    /// inputs [`Self::render`] draws each floor with, for a painter drawing
    /// it on its own.
    pub fn floor_world<'a>(
        &self,
        world: FloorInputs<'a>,
        floor_scene: &'a SceneState,
        floor: usize,
        pets: &'a [crate::pet::Pet],
    ) -> FloorInputs<'a> {
        floor_world(world, floor_scene, floor, self.n_floors, pets)
    }

    /// End a slide at once on its destination: [`FloorNav::cancel`].
    pub fn cancel_slide(&mut self) {
        self.nav.cancel(self.n_floors);
    }

    /// Floor `floor`'s stores and raster, once a frame has grown it.
    pub fn floor(&self, floor: usize) -> Option<&PerFloor> {
        self.views.get(floor).map(|view| &view.floor)
    }

    /// Floor `floor`, and the office-wide state a frame of it draws beside.
    pub fn floor_mut(&mut self, floor: usize) -> Option<(&mut PerFloor, &mut PerOffice)> {
        let view = self.views.get_mut(floor)?;
        Some((&mut view.floor, &mut self.office))
    }

    /// Every floor grown so far, for what reaches all of them at once (the
    /// pacing bench's whole repaint).
    pub fn floors_mut(&mut self) -> impl Iterator<Item = &mut PerFloor> {
        self.views.iter_mut().map(|view| &mut view.floor)
    }

    /// The office-wide state: coffee, chitchat, audio, the shared raster.
    pub fn office(&self) -> &PerOffice {
        &self.office
    }

    /// [`Self::office`], to step or record into.
    pub fn office_mut(&mut self) -> &mut PerOffice {
        &mut self.office
    }

    /// Render one frame of `inputs.world.scene` in `look`: the floor showing,
    /// or both floors of a slide composed into one, `gap` between them dimmed
    /// as the floors are and their world text baked whatever `look` says.
    /// `inputs.world.scene` is the FULL live scene: the office evicts against
    /// it and projects each floor out of it. `inputs.world.floor` carries the
    /// office's weather and motion, `inputs.place.gateway` its gateway; each
    /// floor's index, altitude, breadcrumb and pet (of `pets`) are the
    /// office's, and a petting plays only on its own floor. Its picture is
    /// [`Self::picture`].
    pub fn render(
        &mut self,
        look: crate::look::Look,
        inputs: crate::look::RenderInputs<'_>,
        pets: &[crate::pet::Pet],
        gap: Rgb,
    ) -> Feed {
        let scene = inputs.world.scene;
        let now = inputs.world.now;
        self.prepare(scene, now);
        let n_floors = self.n_floors;
        let total_agents = scene.agents.len();
        let draw = |views: &mut Vec<FloorView>, office: &mut PerOffice, floor: usize, look| {
            let floor_scene = super::project_floor_scene(scene, floor);
            let world = floor_world(inputs.world, &floor_scene, floor, n_floors, pets);
            views[floor].render(
                office,
                look,
                crate::look::RenderInputs {
                    world,
                    place: crate::look::Place {
                        floor: footer_floor(floor, n_floors, total_agents),
                        ..inputs.place
                    },
                    ..inputs
                },
            );
        };
        if let Some(tr) = self.nav.transition() {
            self.shown = None;
            // A slide has no one layout for a host to set text on.
            let look = match look {
                crate::look::Look::Cutaway { scale, .. } => crate::look::Look::Cutaway {
                    scale,
                    text: crate::look::WorldText::Baked,
                },
                classic @ crate::look::Look::Classic => classic,
            };
            let (from, to, t) = (tr.from_floor, tr.to_floor, tr.t(now));
            draw(&mut self.views, &mut self.office, from, look);
            draw(&mut self.views, &mut self.office, to, look);
            let (slide, flashes) = self
                .slide
                .get_or_insert_with(|| (RgbBuffer::filled(0, 0, gap), Default::default()));
            if let (Some(leaving), Some(arriving)) = (self.views[from].buf(), self.views[to].buf())
            {
                compose_slide(slide, (leaving, arriving), t, to > from, inputs.dim.of(gap));
                *flashes = crate::screen::Flashes::of_slide(
                    self.views[from].flash(),
                    self.views[to].flash(),
                );
            }
        } else {
            self.slide = None;
            let current = self.nav.current();
            self.shown_changed = self.shown.replace(current) != Some(current);
            draw(&mut self.views, &mut self.office, current, look);
        }
        let current = self.nav.current();
        Feed {
            audio: self.audio_frame(scene, inputs.world.floor, now),
            note: self.views[current].floor.raster.note(),
        }
    }

    /// The last frame [`Self::render`] drew: the slide's, or the floor
    /// showing's; `None` before the first, and when the floor showing could
    /// not lay out.
    pub fn picture(&self) -> Option<crate::look::Picture<'_>> {
        if let Some((pixels, flashes)) = &self.slide {
            return Some(crate::look::Picture::Slide(crate::look::SlidePicture {
                pixels,
                flashes: *flashes,
            }));
        }
        let current = self.nav.current();
        self.views[current]
            .picture(current, self.shown_changed)
            .map(crate::look::Picture::Floor)
    }

    /// A frame drew no office (a painter's too-small screen): the floor
    /// showing keeps nothing of its last. Its audio, the empty floor's, is
    /// the caller's to feed.
    #[must_use]
    pub fn drew_no_office(
        &mut self,
        scene: &SceneState,
        office_floor: FloorMeta,
        now: SystemTime,
    ) -> AudioFrame {
        let current = self.nav.current();
        self.views[current].forget();
        // The empty floor's by construction, whatever the view still holds.
        let floor = floor_meta(office_floor, current, self.n_floors);
        self.office
            .audio
            .frame(scene, &HashSet::new(), |_| None, floor, now)
    }

    /// The waypoints the floor showing's audio reads as occupied.
    #[cfg(test)]
    pub(super) fn heard_occupied(&self) -> &HashSet<usize> {
        &self.views[self.nav.current()].last_occupied
    }

    /// One frame of audio intent for the floor showing, in `scene` (the FULL
    /// live scene) under `office_floor`'s weather and motion.
    fn audio_frame(
        &mut self,
        scene: &SceneState,
        office_floor: FloorMeta,
        now: SystemTime,
    ) -> AudioFrame {
        // `nav` settles within `n_floors`, and a view is kept for each.
        let current = self.nav.current();
        let floor = floor_meta(office_floor, current, self.n_floors);
        self.views[current].audio_frame(&mut self.office, scene, floor, now)
    }

    /// The floor the footer speaks for: the one showing, or a slide's
    /// destination for the whole slide, so its count matches the breadcrumb.
    fn footer_floor_index(&self) -> usize {
        self.nav
            .transition()
            .map_or(self.nav.current(), |tr| tr.to_floor)
    }

    /// The footer's floor breadcrumb, among `scene`'s agents.
    pub fn footer_floor(&self, scene: &SceneState) -> Option<FooterFloor> {
        footer_floor(self.footer_floor_index(), self.n_floors, scene.agents.len())
    }

    /// The scene the footer counts and tallies: its floor's, as
    /// [`FooterInputs::new`](crate::footer::FooterInputs::new) asks.
    pub fn footer_scene(&self, scene: &SceneState) -> SceneState {
        super::project_floor_scene(scene, self.footer_floor_index())
    }

    /// Whether the floor showing's last frame changes between beats: someone
    /// walks, or the sign's or the room's light is mid-fade. Everything else a
    /// painter draws holds one frame per beat
    /// (`both_painters_paint_one_frame_per_beat`), so a painter may sleep to the
    /// next beat only while this is false.
    pub fn moves_off_beat(&self) -> bool {
        self.views[self.nav.current()].moves_off_beat()
    }
}
