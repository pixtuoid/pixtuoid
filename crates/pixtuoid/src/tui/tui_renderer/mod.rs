//! `TuiRenderer` — the half-block terminal painter; its inherent `render` is
//! the production flush entry point.

use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Result;
#[cfg(test)]
use pixtuoid_core::AgentId;
use pixtuoid_core::sprite::RgbBuffer;
use pixtuoid_core::state::SceneState;
use pixtuoid_scene::pack::OfficeArt;

use ratatui::Terminal;
use ratatui::backend::Backend;

use ratatui::layout::Rect;

use crate::tui::renderer::PetState;
use pixtuoid_scene::floor::{
    FloorInputs, FloorMeta, FloorTransition, OfficeSession, PetInputs, project_floor_scene,
};
use pixtuoid_scene::layout::{SceneLayout, Size};
use pixtuoid_scene::look::{FrameNote, Look, Picture, Place, RenderInputs};
use pixtuoid_scene::screen::Screen;

#[derive(Debug, Default)]
struct PopupState {
    /// When the last visible↔hidden edge happened — the animation clock.
    started_at: Option<SystemTime>,
    /// Scale captured at that edge so an interrupted animation continues from its
    /// current position instead of snapping back to the start/end.
    scale_at_edge: f32,
    /// Scale computed during the most recent `render()`; the mouse handler reads
    /// this instead of recomputing with a fresh `SystemTime`, so click geometry
    /// stays in sync with what was painted.
    last_scale: f32,
}

#[derive(Debug)]
pub struct TuiRenderer<B: Backend<Error: Send + Sync + 'static>> {
    pub terminal: Terminal<B>,
    /// The pack every floor's raster draws with, which each frame's must be.
    pack: Arc<OfficeArt>,
    /// The frames' times and janks.
    jank: crate::jank::Jank,
    /// What holds each frame's output and presents it whole; `None` writes
    /// straight to the backend.
    frame_out: Option<crate::tui::FrameOut>,
    /// Every floor, the office-wide state they share (a cup of coffee
    /// survives a floor switch), which floor shows and the slide to another,
    /// and the floor a pointer grips: the floating window's own.
    session: OfficeSession,
    /// [`Self::office_extent`] after the last frame.
    last_extent: Option<(u16, u16)>,
    mouse_pos: Option<(u16, u16)>,
    /// The left button's gesture over the office.
    pointer: pixtuoid_scene::interact::Pointer,
    /// Live walkable/approach/route debug layer toggle (`w`); not persisted.
    debug_walkable: bool,
    chrome: Chrome,
    /// What the terminal shows, the half-blocks or the cutaway's tiles, and
    /// what a pointer finds there: one screen, so one flash hold.
    screen: Screen<ratatui::layout::Size, crate::tui::geometry::SceneGeometry>,
    /// The cutaway, painted as terminal images in place of the half-blocks.
    cutaway: Option<crate::tui::cutaway::TileCutaway>,
}

/// Everything a frame shows besides the floors: kept apart from `session` so a
/// frame borrows it while the session renders ([`Chrome::office_world`]).
#[derive(Debug)]
struct Chrome {
    theme: &'static pixtuoid_scene::theme::Theme,
    active_pet: Option<PetState>,
    pets: Vec<pixtuoid_scene::pet::Pet>,
    popup: PopupState,
    /// The panels the loop computed, which the frame paints: their
    /// `version_popup.open` is `popup`'s target, its scale animated there.
    frames: crate::panels::ui_state::RenderFrames,
    /// Ambient-audio gateway; inert unless installed.
    audio: crate::audio::AudioHandle,
    /// Transient +/- volume readout (percent); `None` past [`crate::audio::VOLUME_FLASH_MS`].
    volume_flash: Option<u8>,
    weather: pixtuoid_scene::sky::WeatherPolicy,
    motion: pixtuoid_scene::anim::Motion,
}

impl PopupState {
    fn scale(&self, open: bool, now: SystemTime) -> f32 {
        use pixtuoid_scene::anim::{Easing, eased_progress};
        const VERSION_POPUP_GROW_MS: u32 = 200;
        const VERSION_POPUP_SHRINK_MS: u32 = 120;
        match (open, self.started_at) {
            (true, Some(start)) => {
                let progress =
                    eased_progress(start, VERSION_POPUP_GROW_MS, Easing::EaseOutCubic, now);
                self.scale_at_edge + (1.0 - self.scale_at_edge) * progress
            }
            (false, Some(start)) => {
                let progress =
                    eased_progress(start, VERSION_POPUP_SHRINK_MS, Easing::EaseInQuad, now);
                self.scale_at_edge * (1.0 - progress)
            }
            (true, None) => 1.0,
            (false, None) => 0.0,
        }
    }
}

impl Chrome {
    /// The panels open over a frame at `popup_scale`, the help listing the
    /// terminal's own shortcuts.
    fn overlays(
        &self,
        popup_scale: f32,
        offers_walkable_debug: bool,
    ) -> crate::panels::OverlayFrame<'_> {
        let host_keys = if offers_walkable_debug {
            crate::tui::WALKABLE_SHORTCUTS
        } else {
            &[]
        };
        self.frames.overlays(popup_scale, host_keys)
    }

    /// Floor `floor` of `session`'s office in `scene`, whose projection is
    /// `floor_scene`: its inputs as [`OfficeSession::floor_world`] gives
    /// every painter's floors.
    fn world<'a>(
        &'a self,
        session: &OfficeSession,
        floor_scene: &'a SceneState,
        pack: &'a OfficeArt,
        now: SystemTime,
        floor: usize,
    ) -> FloorInputs<'a> {
        session.floor_world(
            self.office_world(floor_scene, pack, now),
            floor_scene,
            floor,
            &self.pets,
        )
    }

    /// The office's inputs over `scene`, as [`OfficeSession::render`] takes
    /// them: its weather and motion, and the petting the session keeps to
    /// its own floor.
    fn office_world<'a>(
        &'a self,
        scene: &'a SceneState,
        pack: &'a OfficeArt,
        now: SystemTime,
    ) -> FloorInputs<'a> {
        FloorInputs {
            scene,
            pack,
            now,
            floor: FloorMeta::ground()
                .with_weather(self.weather)
                .with_motion(self.motion),
            pets: PetInputs {
                pet: None,
                petting: self.active_pet.as_ref(),
            },
        }
    }

    /// The footer's context over the FULL `scene`, for the floor `session`'s
    /// footer speaks for.
    fn footer<'a>(
        &'a self,
        session: &OfficeSession,
        scene: &SceneState,
    ) -> pixtuoid_scene::footer::FooterContext<'a> {
        crate::panels::widgets::footer_context(
            scene,
            session.footer_floor(scene),
            self.audio.is_audible(),
            self.volume_flash,
            self.frames.source_warning.as_deref(),
        )
    }
}

impl<B: Backend<Error: Send + Sync + 'static>> TuiRenderer<B> {
    pub fn new(
        terminal: Terminal<B>,
        theme: &'static pixtuoid_scene::theme::Theme,
        pets: Vec<pixtuoid_scene::pet::Pet>,
        pack: Arc<OfficeArt>,
    ) -> Self {
        Self {
            terminal,
            session: OfficeSession::new(Arc::clone(&pack)),
            pack,
            jank: crate::jank::Jank::new(),
            frame_out: None,
            last_extent: None,
            mouse_pos: None,
            pointer: pixtuoid_scene::interact::Pointer::default(),
            debug_walkable: false,
            chrome: Chrome {
                theme,
                active_pet: None,
                pets,
                popup: PopupState::default(),
                frames: crate::panels::ui_state::RenderFrames::default(),
                audio: crate::audio::AudioHandle::disabled(),
                volume_flash: None,
                weather: pixtuoid_scene::sky::WeatherPolicy::Clock,
                motion: pixtuoid_scene::anim::Motion::Full,
            },
            screen: Screen::on(pixtuoid_scene::screen::monotonic()),
            cutaway: None,
        }
    }

    /// Paint `cutaway` from the next frame on.
    pub(crate) fn set_cutaway(&mut self, cutaway: crate::tui::cutaway::TileCutaway) {
        self.cutaway = Some(cutaway);
    }

    /// Whether the last office frame drawn was the half-blocks of a debug
    /// build, which draw the walkable debug layer where the cutaway never does:
    /// `w` is listed and read only then, including while a cutaway too big for
    /// the cell falls back to them. A footer-only frame draws no office, so it
    /// keeps the answer of the one before it.
    pub(crate) fn offers_walkable_debug(&self) -> bool {
        cfg!(debug_assertions)
            && self
                .session
                .floor(self.session.nav().current())
                .is_some_and(|f| f.raster.shown() == Some(Look::Classic))
    }

    /// Draw the first frame of `scene` unseen, in the look the first frame
    /// shown will take, so the caches it fills (the cloud rasters above all)
    /// are warm when it is shown; what it shows is unchanged.
    pub(crate) fn warm(&mut self, scene: &SceneState, pack: &OfficeArt, now: SystemTime) {
        let Ok(size) = self.terminal.size() else {
            return;
        };
        let full = Rect::new(0, 0, size.width, size.height);
        let scene_area = crate::tui::renderer::scene_rect(full);
        if crate::tui::renderer::scene_too_small(scene_area) {
            return;
        }
        let (buf_w, buf_h) = crate::tui::renderer::scene_buf_size(size.width, size.height);
        let classic = (Look::Classic, Size { w: buf_w, h: buf_h });
        let look = self.cutaway_look(scene_area).unwrap_or(classic);
        self.session.office_mut().raster.warm();
        let _ = self.render_office(scene, pack, now, look.0, look.1, false);
    }

    /// One frame of `scene` drawn through the session in `look` at `size`:
    /// the floor showing, or a slide's two floors composed, its picture the
    /// session's. What it was drawn under.
    fn render_office(
        &mut self,
        scene: &SceneState,
        pack: &OfficeArt,
        now: SystemTime,
        look: Look,
        size: Size,
        debug_walkable: bool,
    ) -> Option<FrameNote> {
        let frame = self.session.render(
            look,
            RenderInputs {
                world: self.chrome.office_world(scene, pack, now),
                theme: self.chrome.theme,
                size,
                place: Place {
                    gateway: pixtuoid_scene::tally::office_gateway(scene),
                    floor: None,
                },
                debug_walkable,
                dim: self.chrome.frames.onboarding.dim,
            },
            &self.chrome.pets,
            self.chrome.theme.surface.bg_fallback,
        );
        // Fed as it is composed, before anything can fail, muted or not:
        // `AudioObserver::frame`'s every-world-frame contract.
        self.chrome.audio.frame(frame.audio);
        frame.note
    }

    /// The cutaway's look and office extent over `scene_area`, as its next
    /// frame fits them; `None` while classic paints.
    fn cutaway_look(&mut self, scene_area: Rect) -> Option<(Look, Size)> {
        let window = self
            .terminal
            .backend_mut()
            .window_size()
            .ok()
            .and_then(crate::graphics::CellSize::of_window);
        let fitted = self.cutaway.as_mut()?.fit_to(scene_area, window)?;
        Some((fitted.look(), fitted.fit.logical()))
    }

    /// Report the frames since the last pacing summary, at exit.
    pub(crate) fn finish_pacing(&self) {
        self.jank.finish(std::time::Instant::now());
    }

    /// The interval the loop now schedules frames at: what a frame's pacing
    /// is judged against.
    pub(crate) fn scheduled_every(&mut self, interval: std::time::Duration) {
        self.jank.scheduled_every(interval);
    }

    /// The next frame is the one the loop's clock scheduled for `at`
    /// ([`crate::jank::Jank::due_at`]).
    pub(crate) fn due_at(&mut self, at: std::time::Instant) {
        self.jank.due_at(at);
    }

    /// Hold each frame's output in `out` and present it whole. For the terminal
    /// painter, `out` is the handle its backend writes through
    /// ([`Self::restore_terminal`] abandons it).
    pub(crate) fn present_through(&mut self, out: crate::tui::FrameOut) {
        self.frame_out = Some(out);
    }

    /// Name what draws the frames, for their pacing summaries.
    pub(crate) fn painted_by(&mut self, painter: crate::jank::Painter) {
        self.jank.painted_by(painter);
    }

    /// Paint and send the next frame whole, for the pacing bench's worst case.
    pub(crate) fn forget_frame(&mut self) {
        for floor in self.session.floors_mut() {
            floor.raster.forget_shown();
        }
        if let Some(cutaway) = &mut self.cutaway {
            cutaway.forget();
        }
    }

    /// Clear the terminal and repaint every cell and every image.
    pub(crate) fn redraw(&mut self) -> Result<()> {
        if let Some(cutaway) = &mut self.cutaway {
            cutaway.forget();
        }
        self.terminal.clear()?;
        Ok(())
    }

    /// The logical extent the current floor last laid out on: what a resize
    /// changes, and a slide does not.
    fn office_extent(&self) -> (u16, u16) {
        if let Some(office) = self.cutaway.as_ref().and_then(|c| c.office()) {
            return (office.w, office.h);
        }
        self.buf().map_or((0, 0), |b| (b.width(), b.height()))
    }

    /// A frame laid out at a new extent — a resize — lands any slide.
    fn follow_resize(&mut self) {
        let extent = self.office_extent();
        if self
            .last_extent
            .replace(extent)
            .is_some_and(|was| was != extent)
        {
            self.cancel_transition();
        }
    }

    pub(crate) fn set_audio(&mut self, audio: crate::audio::AudioHandle) {
        self.chrome.audio = audio;
    }

    pub(crate) fn set_volume_flash(&mut self, flash: Option<u8>) {
        self.chrome.volume_flash = flash;
    }

    pub(crate) fn debug_walkable(&self) -> bool {
        self.debug_walkable
    }

    pub(crate) fn set_debug_walkable(&mut self, v: bool) {
        self.debug_walkable = v;
    }

    pub fn current_floor(&self) -> usize {
        self.session.nav().current()
    }

    #[cfg(test)]
    pub(crate) fn floor_history(&self, floor: usize) -> Option<&pixtuoid_scene::pose::PoseHistory> {
        self.session.floor(floor).map(|f| &f.ctx.history)
    }

    #[cfg(test)]
    pub(crate) fn floor_walks(
        &self,
        floor: usize,
    ) -> Option<&std::collections::HashMap<pixtuoid_core::AgentId, pixtuoid_scene::walk::WalkState>>
    {
        self.session.floor(floor).map(|f| &f.ctx.walks)
    }

    #[cfg(test)]
    pub(crate) fn floor_buf(&self, floor: usize) -> Option<&RgbBuffer> {
        self.session.floor(floor)?.raster.pixels()
    }

    /// Seed coffee-carrier state directly: the production path needs a full pantry
    /// wander trip, so this injects the end state to exercise steam rendering.
    #[cfg(test)]
    pub(crate) fn inject_coffee(&mut self, id: AgentId, fetched_at: SystemTime) {
        self.session.office_mut().coffee.insert(id, fetched_at);
    }

    /// The layout of the frame the terminal shows.
    pub(crate) fn cached_layout(&self) -> Option<&SceneLayout> {
        Some(self.screen.shown()?.hits()?.layout())
    }

    /// The pixels cell `(col, row)` shows of the floor the terminal shows;
    /// none in a slide, which shows no one floor where it stands.
    pub(crate) fn scene_area_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<crate::tui::geometry::CellArea> {
        let shown = self.screen.shown()?;
        shown.hits().and(shown.geometry.area_at(col, row))
    }

    /// A left press at cell `(col, row)` at `now`: on what the terminal
    /// shows there, a click or a drag follows.
    pub(crate) fn press(&mut self, col: u16, row: u16, now: std::time::SystemTime) {
        let Some(cell) = self.scene_area_at(col, row) else {
            // As any press does: a carry whose release never came ends here.
            if let Some(ended) = self.pointer.cancel() {
                self.grip(&ended);
            }
            return;
        };
        let hit = self
            .screen
            .shown()
            .and_then(|shown| shown.hit_at(cell.bounds()));
        let bounds = cell.bounds();
        // A cell is the terminal's one step: any move to another lifts.
        let slop = pixtuoid_scene::interact::Slop {
            x: bounds.width,
            y: bounds.height,
        };
        let down = self.pointer.down(
            hit,
            centre(bounds),
            slop,
            self.chrome.active_pet.as_ref(),
            now,
        );
        if let Some(ended) = &down.ended {
            self.grip(ended);
        }
    }

    /// The pointer dragged to cell `(col, row)`: a figure lifted follows it.
    pub(crate) fn drag(&mut self, col: u16, row: u16) {
        let Some(cell) = self.scene_area_at(col, row) else {
            return;
        };
        if let Some(gesture) = self.pointer.moved(centre(cell.bounds())) {
            self.grip(&gesture);
        }
    }

    /// The press released at cell `(col, row)`: the click's action, the
    /// caller's to carry out; a figure carried is set down.
    pub(crate) fn release(&mut self, col: u16, row: u16) -> Option<pixtuoid_scene::hit::HitAction> {
        let at = self
            .scene_area_at(col, row)
            .map(|cell| centre(cell.bounds()));
        match self.pointer.up(at)? {
            pixtuoid_scene::interact::Gesture::Click(action) => Some(action),
            gesture => {
                self.grip(&gesture);
                None
            }
        }
    }

    /// Where a tooltip follows the pointer: nowhere while it carries a
    /// figure, which has none.
    fn tooltip_pos(&self) -> Option<(u16, u16)> {
        self.mouse_pos.filter(|_| !self.pointer.carrying())
    }

    fn grip(&mut self, gesture: &pixtuoid_scene::interact::Gesture) {
        self.session.grip(gesture);
    }

    /// What cell `(col, row)` shows the pointer in the frame the terminal
    /// shows.
    #[cfg(test)]
    pub(crate) fn scene_hit_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<crate::tui::hit_test::SceneHit<'_>> {
        let shown = self.screen.shown()?;
        shown.hit_at(shown.geometry.area_at(col, row)?.bounds())
    }

    /// The agent topmost at cell `(col, row)` in the frame the terminal
    /// shows.
    #[cfg(test)]
    pub(crate) fn hit_test_agent_at(&self, col: u16, row: u16) -> Option<pixtuoid_core::AgentId> {
        match self.scene_hit_at(col, row)? {
            crate::tui::hit_test::SceneHit::Figure(
                pixtuoid_scene::display::HoverTarget::Agent(id),
            ) => Some(*id),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn current_floor_seed(&self) -> u64 {
        FloorMeta::for_floor(self.session.nav().current(), self.session.n_floors()).floor_seed
    }

    pub fn transition(&self) -> Option<&FloorTransition> {
        self.session.nav().transition()
    }

    pub fn navigate_floor(&mut self, target: usize, now: SystemTime) {
        self.session.navigate(target, now);
    }

    pub fn cancel_transition(&mut self) {
        self.session.cancel_slide();
    }

    pub(crate) fn set_mouse_pos(&mut self, pos: Option<(u16, u16)>) {
        self.mouse_pos = pos;
    }

    /// The current floor's last frame, `None` before its first.
    pub fn buf(&self) -> Option<&RgbBuffer> {
        self.session
            .floor(self.session.nav().current())?
            .raster
            .pixels()
    }

    pub(crate) fn set_theme(&mut self, theme: &'static pixtuoid_scene::theme::Theme) {
        self.chrome.theme = theme;
    }

    /// Which weather every floor shows from the next frame on.
    pub fn set_weather(&mut self, weather: pixtuoid_scene::sky::WeatherPolicy) {
        self.chrome.weather = weather;
    }

    /// How every floor moves from the next frame on.
    pub fn set_motion(&mut self, motion: pixtuoid_scene::anim::Motion) {
        self.chrome.motion = motion;
    }

    /// The loop's panels for the next frames, the version popup animating
    /// toward theirs.
    pub fn set_frames(&mut self, frames: crate::panels::ui_state::RenderFrames, now: SystemTime) {
        if frames.version_popup.open != self.chrome.frames.version_popup.open {
            self.chrome.popup.scale_at_edge = self.version_popup_scale(now);
            self.chrome.popup.started_at = Some(now);
        }
        self.chrome.frames = frames;
    }

    pub fn version_popup_scale(&self, now: SystemTime) -> f32 {
        self.chrome
            .popup
            .scale(self.chrome.frames.version_popup.open, now)
    }

    pub(crate) fn last_popup_scale(&self) -> f32 {
        self.chrome.popup.last_scale
    }

    pub fn set_active_pet(&mut self, pet: Option<PetState>) {
        self.chrome.active_pet = pet;
    }

    #[cfg(test)]
    pub(crate) fn active_pet_ref(&self) -> Option<&PetState> {
        self.chrome.active_pet.as_ref()
    }

    /// The pet the frame the terminal shows drew, found as the pointer finds
    /// it: on some logical pixel it is the topmost hover.
    #[cfg(test)]
    pub(crate) fn drawn_pet(&self) -> Option<pixtuoid_scene::display::PetHover> {
        let hits = self.screen.shown()?.hits()?;
        let layout = hits.layout();
        (0..layout.buf_h)
            .flat_map(|y| (0..layout.buf_w).map(move |x| (x, y)))
            .find_map(|(x, y)| {
                let pixel = pixtuoid_scene::layout::Bounds {
                    x,
                    y,
                    width: 1,
                    height: 1,
                };
                match hits.hovers().at(pixel)? {
                    pixtuoid_scene::display::HoverTarget::Pet(pet) => Some(*pet),
                    _ => None,
                }
            })
    }

    /// Drop the agents gone from `scene`: [`OfficeSession::evict_missing`],
    /// which every frame's `prepare` runs.
    #[cfg(test)]
    pub(crate) fn evict_missing(&mut self, scene: &SceneState) {
        self.session.evict_missing(scene);
    }

    #[cfg(test)]
    pub(crate) fn coffee_contains(&self, id: AgentId) -> bool {
        self.session.office().coffee.map().contains_key(&id)
    }

    /// The slide the session just composed, as classic half-blocks, under the
    /// destination's footer.
    fn flush_classic_slide(&mut self, scene: &SceneState, now: SystemTime) -> Result<()> {
        use crate::panels::paint_overlays;
        use crate::tui::renderer::{flush_buffer_to_term, paint_footer, scene_rect};
        let term_size = self.terminal.size()?;
        let footer_scene = self.session.footer_scene(scene);
        let footer = pixtuoid_scene::footer::FooterInputs::new(
            &footer_scene,
            self.chrome.footer(&self.session, scene),
        );
        let popup_scale = self.version_popup_scale(now);
        let overlays = self
            .chrome
            .overlays(popup_scale, self.offers_walkable_debug());
        let theme = self.chrome.theme;
        let Some(picture) = self.session.picture() else {
            return Ok(());
        };
        let geometry = crate::tui::renderer::classic_geometry(term_size);
        if !self
            .screen
            .admit(&picture, term_size, geometry, None)
            .written()
        {
            return Ok(());
        }
        let slide = picture.pixels();
        self.terminal.draw(|f| {
            let full = f.area();
            paint_footer(f, &footer, full, theme);
            flush_buffer_to_term(f, slide, scene_rect(full));
            paint_overlays(f, &overlays, now, full, theme);
        })?;
        self.chrome.popup.last_scale = popup_scale;
        Ok(())
    }

    /// The footer-only frame of a terminal too small for the office; a slide
    /// it would hide ends at once rather than run its course unseen. Its
    /// audio is the empty floor's unless the office `stepped` for it.
    fn draw_too_small(
        &mut self,
        scene: &SceneState,
        now: SystemTime,
        stepped: bool,
    ) -> Result<Option<FrameNote>> {
        self.session.prepare(scene, now);
        self.cancel_transition();
        let footer_scene = self.session.footer_scene(scene);
        let footer = pixtuoid_scene::footer::FooterInputs::new(
            &footer_scene,
            self.chrome.footer(&self.session, scene),
        );
        let popup_scale = self.version_popup_scale(now);
        let offers_walkable_debug = self.offers_walkable_debug();
        let flushed = crate::tui::renderer::draw_footer_only_frame(
            &mut self.terminal,
            &footer,
            self.chrome.theme,
            &self.chrome.overlays(popup_scale, offers_walkable_debug),
            now,
        );
        if !stepped {
            let office_floor = self.chrome.office_world(scene, &self.pack, now).floor;
            let audio = self.session.drew_no_office(scene, office_floor, now);
            self.chrome.audio.frame(audio);
        }
        self.screen.blank(self.terminal.size()?);
        self.chrome.popup.last_scale = popup_scale;
        self.rest_floor(now);
        flushed.map(|()| None)
    }

    /// A refused frame steps nothing, but the door's clamp still keeps time, as
    /// `step_floor` keeps it for a stepped one.
    fn rest_floor(&mut self, now: SystemTime) {
        if let Some((floor, _)) = self.session.floor_mut(self.session.nav().current()) {
            floor.ctx.recompute_door_anim_max_ms(now);
        }
    }
}

impl<B: Backend<Error: Send + Sync + 'static>> TuiRenderer<B> {
    /// Draw one frame of `scene`, then follow a terminal resize.
    ///
    /// # Errors
    ///
    /// If querying the terminal size, drawing the frame to the backend, or
    /// writing the held frame fails.
    pub fn render(&mut self, scene: &SceneState, pack: &OfficeArt, now: SystemTime) -> Result<()> {
        let begun = std::time::Instant::now();
        if let Some(out) = &self.frame_out {
            out.begin();
        }
        let drawn = self.draw_frame(scene, pack, now);
        self.follow_resize();
        // Presented even when the draw failed, so nothing stays held.
        let presenting = std::time::Instant::now();
        let presented = self
            .frame_out
            .as_ref()
            .map_or(Ok(()), crate::tui::FrameOut::present);
        let present = presenting.elapsed();
        presented?;
        self.screen.landed();
        let note = drawn?;
        let send = self
            .cutaway
            .as_ref()
            .map(crate::tui::cutaway::TileCutaway::last_send);
        self.jank.record(
            begun.elapsed(),
            present,
            send,
            note,
            std::time::Instant::now(),
        );
        Ok(())
    }

    /// One frame of `scene`, whichever path draws it: what it was drawn under.
    fn draw_frame(
        &mut self,
        scene: &SceneState,
        pack: &OfficeArt,
        now: SystemTime,
    ) -> Result<Option<FrameNote>> {
        if self
            .chrome
            .active_pet
            .as_ref()
            .is_some_and(|p| !p.is_active(now))
        {
            self.chrome.active_pet = None;
        }

        if let Some(mut cutaway) = self.cutaway.take() {
            cutaway.begin_frame();
            let size = self.terminal.size()?;
            let scene_area =
                crate::tui::renderer::scene_rect(Rect::new(0, 0, size.width, size.height));
            let window = self
                .terminal
                .backend_mut()
                .window_size()
                .ok()
                .and_then(crate::graphics::CellSize::of_window);
            let drawn = cutaway
                .fit_to(scene_area, window)
                .map(|fitted| self.render_cutaway(&mut cutaway, fitted, scene, pack, now));
            self.cutaway = Some(cutaway);
            if let Some(drawn) = drawn {
                return drawn;
            }
        }

        let term_size = self.terminal.size()?;
        let full_rect = Rect::new(0, 0, term_size.width, term_size.height);
        if crate::tui::renderer::scene_too_small(crate::tui::renderer::scene_rect(full_rect)) {
            return self.draw_too_small(scene, now, false);
        }
        let (buf_w, buf_h) =
            crate::tui::renderer::scene_buf_size(full_rect.width, full_rect.height);
        let drawn = self.render_office(
            scene,
            pack,
            now,
            Look::Classic,
            Size { w: buf_w, h: buf_h },
            self.debug_walkable,
        );
        if self.session.nav().transition().is_some() {
            return self.flush_classic_slide(scene, now).map(|()| drawn);
        }
        let Some(picture) = self.session.picture() else {
            return self.draw_too_small(scene, now, true);
        };
        let Picture::Floor(floor) = &picture else {
            return self.draw_too_small(scene, now, true);
        };
        let current = self.session.nav().current();
        let floor_scene = project_floor_scene(scene, current);
        let footer_scene = self.session.footer_scene(scene);
        let footer = pixtuoid_scene::footer::FooterInputs::new(
            &footer_scene,
            self.chrome.footer(&self.session, scene),
        );
        let popup_scale = self.version_popup_scale(now);
        let mouse_pos = self.tooltip_pos();
        let world = self
            .chrome
            .world(&self.session, &floor_scene, pack, now, current);
        let overlays = self
            .chrome
            .overlays(popup_scale, self.offers_walkable_debug());
        let (geometry, star) = crate::tui::renderer::classic_placement(term_size, floor);
        if !self
            .screen
            .admit(&picture, term_size, geometry, star)
            .written()
        {
            // The terminal still shows the last frame, and its hit targets.
            return Ok(drawn);
        }
        let frame = crate::tui::renderer::ClassicFrame {
            footer: &footer,
            overlays: &overlays,
            theme: self.chrome.theme,
            world: &world,
            mouse_pos,
        };
        crate::tui::renderer::flush_classic(&mut self.terminal, &frame, floor)?;
        // The popup's click rect derives from the terminal bounds — NOT the
        // office layout — so the painted scale IS the clickable one on both
        // draw paths.
        self.chrome.popup.last_scale = popup_scale;
        Ok(drawn)
    }
}

impl<B: Backend<Error: Send + Sync + 'static>> TuiRenderer<B> {
    /// [`Self::render`] under the cutaway: the session's frame (the floor
    /// showing, or a slide's two floors composed with their text baked) as
    /// the image in place of the half-blocks, and the floor showing's badges,
    /// bubbles, wall board and floor indicator over it as terminal text, as
    /// the footer, tooltips and modals are.
    fn render_cutaway(
        &mut self,
        cutaway: &mut crate::tui::cutaway::TileCutaway,
        fitted: crate::tui::cutaway::Fitted,
        scene: &SceneState,
        pack: &OfficeArt,
        now: SystemTime,
    ) -> Result<Option<FrameNote>> {
        use crate::panels::paint_overlays;
        use crate::panels::widgets::{paint_host_text, star_area};
        use crate::tui::hit_test::SceneHit;
        use crate::tui::renderer::{TooltipAt, paint_footer, scene_rect};
        use pixtuoid_scene::display::HoverTarget;
        if crate::tui::renderer::scene_too_small(fitted.scene) {
            return self.draw_too_small(scene, now, false);
        }
        let drawn =
            self.render_office(scene, pack, now, fitted.look(), fitted.fit.logical(), false);
        let Some(picture) = self.session.picture() else {
            return self.draw_too_small(scene, now, true);
        };
        let term_size = self.terminal.size()?;
        let current = self.session.nav().current();
        let floor_scene = project_floor_scene(scene, current);
        let footer_scene = self.session.footer_scene(scene);
        let footer = pixtuoid_scene::footer::FooterInputs::new(
            &footer_scene,
            self.chrome.footer(&self.session, scene),
        );
        let popup_scale = self.version_popup_scale(now);
        let theme = self.chrome.theme;
        let mouse_pos = self.tooltip_pos();
        let world = self
            .chrome
            .world(&self.session, &floor_scene, pack, now, current);
        let overlays = self
            .chrome
            .overlays(popup_scale, self.offers_walkable_debug());
        let geometry = fitted.geometry();
        let floor = match &picture {
            Picture::Floor(floor) => Some(floor),
            Picture::Slide(_) => None,
        };
        let text = floor.and_then(|floor| floor.text);
        let star = text.and_then(|text| star_area(text.signs, geometry.map()));
        let admit = self.screen.admit(&picture, term_size, geometry, star);
        cutaway.paint(fitted, &picture, admit, now);
        if !admit.written() {
            return Ok(drawn);
        }
        let mouse = floor.zip(mouse_pos).and_then(|(floor, (mx, my))| {
            let hit = floor.hits.at(geometry.area_at(mx, my)?.bounds(), star)?;
            Some((mx, my, hit))
        });
        let hovered = match mouse {
            Some((_, _, SceneHit::Figure(HoverTarget::Agent(id)))) => Some(*id),
            _ => None,
        };
        let tooltip = mouse.and_then(|(mx, my, hit)| {
            pixtuoid_scene::tooltip::for_hit(hit, &world).map(|tip| (mx, my, tip))
        });
        cutaway.before_flush(now)?;
        let mut carves = Vec::new();
        self.terminal.draw(|f| {
            let full = f.area();
            let scene_area = scene_rect(full);
            paint_footer(f, &footer, full, theme);
            cutaway.place(f.buffer_mut(), scene_area);
            if let Some(text) = text {
                paint_host_text(f, text, (scene_area, geometry.map()), hovered);
            }
            if let Some((mx, my, tip)) = &tooltip {
                let at = TooltipAt {
                    mx: *mx,
                    my: *my,
                    scene_rect: scene_area,
                };
                crate::tui::renderer::paint_tooltip(f, tip, at, theme);
            }
            paint_overlays(f, &overlays, now, full, theme);
            carves = cutaway.carve(f.buffer_mut(), scene_area);
        })?;
        cutaway.after_flush(&carves, now)?;
        self.chrome.popup.last_scale = popup_scale;
        Ok(drawn)
    }
}

/// Test-only access to the rendered ratatui frame, through a `TestBackend`
/// because only it exposes the post-draw cell buffer.
#[cfg(test)]
impl<B> TuiRenderer<B>
where
    B: Backend<Error: Send + Sync + 'static> + std::borrow::Borrow<ratatui::backend::TestBackend>,
{
    pub(crate) fn frame_buffer(&self) -> &ratatui::buffer::Buffer {
        self.terminal.backend().borrow().buffer()
    }
}

impl TuiRenderer<ratatui::backend::CrosstermBackend<crate::tui::FrameOut>> {
    /// [`crate::tui::teardown_terminal`], abandoning the writer this renderer
    /// presents through ([`Self::present_through`]) when the restore fails.
    ///
    /// # Errors
    ///
    /// If restoring the terminal modes or showing the cursor fails.
    pub(crate) fn restore_terminal(&mut self) -> anyhow::Result<()> {
        crate::tui::teardown_terminal(&mut self.terminal, self.frame_out.as_ref())
    }
}

#[cfg(test)]
mod harness;

/// The pixel a cell showing `bounds` points at: its middle.
fn centre(bounds: pixtuoid_scene::layout::Bounds) -> pixtuoid_scene::layout::Point {
    pixtuoid_scene::layout::Point {
        x: bounds.x + bounds.width / 2,
        y: bounds.y + bounds.height / 2,
    }
}
