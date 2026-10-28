//! `--cutaway`: the office a graphics terminal of [`terminal_cell`]s shows,
//! in place of the classic frame's office, above its footer. Its world text is the scene's pixel font
//! (the floating window's), where the terminal sets it in its own.

use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context as _, Result};
use pixtuoid_core::sprite::RgbBuffer;
use pixtuoid_scene::cutaway::Canvas as _;
use pixtuoid_scene::floor::{FloorInputs, FloorSession, OfficeSession, PerFloor};
use pixtuoid_scene::layout::{Point, SceneLayout};
use pixtuoid_scene::look::{Dim, Look, Place, RenderInputs, WorldText};
use pixtuoid_scene::pack::OfficeArt;
use pixtuoid_scene::pet::Pet;
use pixtuoid_scene::render_scale::PixelFit;
use pixtuoid_scene::theme::Theme;

use crate::encode::{ImageCanvas, terminal_cell};

/// The TUI's cutaway on one terminal: a session of its own, stepped on the
/// inputs of the classic frame whose office it takes.
pub(crate) struct CutawayOffice<S> {
    session: S,
    fit: PixelFit,
}

/// What a cutaway steps: what the classic frame beside it steps, so both show
/// one office.
pub(crate) trait CutawaySession {
    /// Step and draw one frame of `inputs` in `look`.
    fn draw(&mut self, look: Look, inputs: RenderInputs<'_>);

    /// The last frame's pixels; `None` before the first, and when it could not
    /// lay out.
    fn pixels(&self) -> Option<&RgbBuffer>;
}

/// `draw_scene`'s one floor, under the `FloorMeta` it is handed (`--floor-seed`).
impl CutawaySession for FloorSession {
    fn draw(&mut self, look: Look, inputs: RenderInputs<'_>) {
        self.render(look, inputs);
    }

    fn pixels(&self) -> Option<&RgbBuffer> {
        self.picture().map(|picture| picture.pixels)
    }
}

/// A second office stepped beside a `TuiRenderer`'s: every floor its scene
/// fills, the pet each picks of `pets`, and the slide between two.
pub(crate) struct RendererOffice {
    session: OfficeSession,
    pets: Vec<Pet>,
}

impl CutawaySession for RendererOffice {
    fn draw(&mut self, look: Look, inputs: RenderInputs<'_>) {
        // Nothing here plays its audio or reports its pacing.
        let _ = self
            .session
            .render(look, inputs, &self.pets, inputs.theme.surface.bg_fallback);
    }

    fn pixels(&self) -> Option<&RgbBuffer> {
        self.session.picture().map(|picture| picture.pixels())
    }
}

impl CutawayOffice<FloorSession> {
    /// The cutaway of `draw_scene`'s floor on a `cols`×`rows` terminal.
    pub(crate) fn of_draw_scene(pack: &Arc<OfficeArt>, cols: u16, rows: u16) -> Result<Self> {
        Self::fitted(FloorSession::new(Arc::clone(pack)), pack, cols, rows)
    }

    /// Its floor's stores, to seed as `draw_scene`'s floor is seeded.
    pub(crate) fn floor_mut(&mut self) -> &mut PerFloor {
        self.session.floor_mut()
    }

    /// The layout its last frame was drawn on.
    pub(crate) fn layout(&self) -> Option<&SceneLayout> {
        self.session
            .picture()
            .map(|picture| &**picture.hits.layout())
    }
}

impl CutawayOffice<RendererOffice> {
    /// The cutaway of a `TuiRenderer` with `pets` on a `cols`×`rows` terminal.
    pub(crate) fn of_renderer(
        pack: &Arc<OfficeArt>,
        cols: u16,
        rows: u16,
        pets: Vec<Pet>,
    ) -> Result<Self> {
        let office = RendererOffice {
            session: OfficeSession::new(Arc::clone(pack)),
            pets,
        };
        Self::fitted(office, pack, cols, rows)
    }

    /// Slide to floor `target` at `now`, as the renderer beside it does.
    pub(crate) fn navigate(&mut self, target: usize, now: SystemTime) {
        self.session.session.navigate(target, now);
    }
}

impl<S: CutawaySession> CutawayOffice<S> {
    /// `session`, drawn as the cutaway the TUI fits to a `cols`×`rows` terminal.
    fn fitted(session: S, pack: &OfficeArt, cols: u16, rows: u16) -> Result<Self> {
        let cell = terminal_cell();
        let fit = pixtuoid::dev::cutaway_office(cols, rows, (cell.w, cell.h), pack)
            .with_context(|| format!("a {}x{} px cell fits no cutaway", cell.w, cell.h))?;
        Ok(Self { session, fit })
    }

    /// Step and draw `world`'s office.
    pub(crate) fn render(&mut self, world: FloorInputs<'_>, theme: &'static Theme) -> Result<()> {
        self.session.draw(
            self.fit.look(WorldText::Baked),
            RenderInputs {
                world,
                theme,
                size: self.fit.logical(),
                place: Place {
                    gateway: pixtuoid_scene::tally::office_gateway(world.scene),
                    floor: None,
                },
                debug_walkable: false,
                dim: Dim::NONE,
            },
        );
        self.session
            .pixels()
            .map(drop)
            .context("the cutaway does not lay out")
    }

    /// The last frame over `img`'s scene cells, which start at its top-left.
    pub(crate) fn paint(&self, img: &mut image::RgbaImage) {
        let Some(buf) = self.session.pixels() else {
            return;
        };
        let w = u32::from(buf.width());
        let up = u32::from(self.fit.upscale());
        let mut canvas = ImageCanvas(img);
        for (i, &px) in buf.as_slice().iter().enumerate() {
            let (x, y) = (i as u32 % w * up, i as u32 / w * up);
            for (dx, dy) in (0..up).flat_map(|dy| (0..up).map(move |dx| (dx, dy))) {
                canvas.set((x + dx) as i32, (y + dy) as i32, px);
            }
        }
    }

    /// Where [`Self::paint`] puts the layout's point `p`: its top-left pixel.
    pub(crate) fn frame_px(&self, p: Point) -> (u32, u32) {
        let scale = u32::from(self.fit.scale().get());
        (u32::from(p.x) * scale, u32::from(p.y) * scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{
        AnimJob, FrameSink, Timeline, cells_to_rgba, compose, save_renderer_animation,
    };
    use crate::{CELL_H, scenes::sample_scene};
    use image::{Rgba, RgbaImage};
    use pixtuoid::dev::{DrawCtx, draw_scene, scene_buf_size};
    use pixtuoid_core::SceneState;
    use pixtuoid_scene::floor::{FloorMeta, PerOffice};
    use pixtuoid_scene::pet::{Pet, PetKind};
    use pixtuoid_scene::theme::NORMAL;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const COLS: u16 = 96;
    const ROWS: u16 = 40;

    fn epoch() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    /// `img`'s pixel rows `rows`.
    fn band(img: &RgbaImage, rows: std::ops::Range<u32>) -> Vec<Rgba<u8>> {
        rows.flat_map(|y| (0..img.width()).map(move |x| *img.get_pixel(x, y)))
            .collect()
    }

    /// The first pixel row of a [`COLS`]×[`ROWS`] frame's footer.
    fn footer_top() -> u32 {
        u32::from(scene_buf_size(COLS, ROWS).1 / pixtuoid_scene::layout::CELL_ROWS) * CELL_H
    }

    /// `img`'s office, above its footer.
    fn office_band(img: &RgbaImage) -> Vec<Rgba<u8>> {
        band(img, 0..footer_top())
    }

    /// `composed` is the cutaway over `classic`: its footer `classic`'s, pixel
    /// for pixel, and most of its office another's.
    fn assert_cutaway_over(composed: &RgbaImage, classic: &RgbaImage) {
        assert_eq!(
            band(composed, footer_top()..composed.height()),
            band(classic, footer_top()..classic.height()),
            "the footer moved"
        );
        let ours = office_band(composed);
        let changed = ours
            .iter()
            .zip(office_band(classic))
            .filter(|(a, b)| **a != *b)
            .count();
        assert!(
            changed * 2 > ours.len(),
            "only {changed} of {} office pixels are the cutaway's",
            ours.len()
        );
    }

    /// The cutaway lands on the scene's cells and nowhere else: the footer
    /// under it stays the classic frame's, pixel for pixel.
    #[test]
    fn the_cutaway_takes_the_office_and_keeps_the_footer() {
        let now = epoch();
        let scene = sample_scene(now, 12, 6);
        let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack().unwrap());
        let meta = FloorMeta::ground();
        let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
        let mut floor = PerFloor::new(Arc::clone(&pack));
        let mut office = PerOffice::new();
        let mut ctx = DrawCtx::offscreen(&mut floor, office.stores(), &NORMAL, &scene, now, meta);
        draw_scene(&mut term, &mut ctx).unwrap();
        let cells = term.backend().buffer();
        let classic = cells_to_rgba(cells, cells.area);

        let mut cutaway = CutawayOffice::of_draw_scene(&pack, COLS, ROWS).unwrap();
        let world = FloorInputs {
            scene: &scene,
            now,
            floor: meta,
            pets: Default::default(),
        };
        cutaway.render(world, &NORMAL).unwrap();
        assert_cutaway_over(&compose(cells, cells.area, Some(&cutaway)), &classic);
    }

    /// The last frame `save_renderer_animation` writes of `scene` with `pets`
    /// and `navigations`, the cutaway over it when `cutaway`.
    fn renderer_frame(
        scene: &SceneState,
        cutaway: bool,
        pets: &[Pet],
        navigations: &[(u64, usize)],
    ) -> RgbaImage {
        let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack().unwrap());
        let frames = tempfile::tempdir().unwrap();
        // Its last frame is mid-slide from a navigation at 0.
        let timeline = Timeline {
            fps: 2,
            secs: 1,
            start: epoch(),
        };
        let job = AnimJob {
            path: frames.path(),
            frames_dir: Some(frames.path()),
            timeline,
            scene,
            pack: &pack,
            theme: &NORMAL,
            weather: Default::default(),
        };
        let term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
        save_renderer_animation(&job, term, navigations, pets.to_vec(), cutaway).unwrap();
        let last = FrameSink::frame_path(frames.path(), timeline.frame_count());
        image::open(last).unwrap().to_rgba8()
    }

    /// The renderer path (`--pets`, `--navigate-at`) paints the cutaway too, and
    /// its office holds the renderer's pets and slides to the floor it does.
    #[test]
    fn the_renderer_path_paints_the_cutaway_with_its_pets_and_floors() {
        // Two floors.
        let scene = sample_scene(epoch(), 4, 6);
        let cat = [Pet::defaulted(PetKind::Cat)];
        let up = [(0, 1)];
        let frame = renderer_frame(&scene, true, &cat, &up);
        assert_cutaway_over(&frame, &renderer_frame(&scene, false, &cat, &up));
        assert!(
            office_band(&frame) != office_band(&renderer_frame(&scene, true, &[], &up)),
            "the cutaway drew no pet"
        );
        assert!(
            office_band(&frame) != office_band(&renderer_frame(&scene, true, &cat, &[])),
            "the cutaway stayed on its floor"
        );
    }

    /// `--empty` settles both looks' floors to the empty office's light: the
    /// cutaway's as dim as the classic's, and drawn darker than one left lit.
    #[test]
    fn an_empty_still_settles_the_cutaway_as_it_does_the_classic() {
        let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack().unwrap());
        let scene = SceneState::uniform(12);
        let world = FloorInputs {
            scene: &scene,
            now: epoch(),
            floor: FloorMeta::ground(),
            pets: Default::default(),
        };
        let blank = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, COLS, ROWS));
        let mut lit = CutawayOffice::of_draw_scene(&pack, COLS, ROWS).unwrap();
        let mut settled = CutawayOffice::of_draw_scene(&pack, COLS, ROWS).unwrap();
        let mut classic = PerFloor::new(Arc::clone(&pack));
        crate::settle_empty(&mut classic, Some(&mut settled));
        let level = |floor: &PerFloor| floor.ctx.vacancy_dim.level();
        assert!(level(&classic) < 1.0, "the classic floor stayed lit");
        assert_eq!(level(settled.session.floor()), level(&classic));

        let brightness = |cutaway: &mut CutawayOffice<FloorSession>| -> u64 {
            cutaway.render(world, &NORMAL).unwrap();
            let frame = compose(&blank, blank.area, Some(&*cutaway));
            office_band(&frame)
                .iter()
                .map(|px| px.0[..3].iter().map(|&c| u64::from(c)).sum::<u64>())
                .sum()
        };
        assert!(
            brightness(&mut settled) < brightness(&mut lit),
            "the settled cutaway is not darker"
        );
    }

    /// `frame_px` puts a point of the layout where `paint` puts its pixel.
    #[test]
    fn frame_px_lands_where_paint_puts_the_point() {
        let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack().unwrap());
        let scene = sample_scene(epoch(), 12, 6);
        let mut cutaway = CutawayOffice::of_draw_scene(&pack, COLS, ROWS).unwrap();
        let world = FloorInputs {
            scene: &scene,
            now: epoch(),
            floor: FloorMeta::ground(),
            pets: Default::default(),
        };
        cutaway.render(world, &NORMAL).unwrap();
        let blank = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, COLS, ROWS));
        let frame = compose(&blank, blank.area, Some(&cutaway));
        let density = cutaway.fit.density().get();
        let pixels = cutaway.session.pixels().unwrap();
        let logical = cutaway.layout().unwrap();
        let points = (0..logical.buf_h)
            .step_by(7)
            .flat_map(|y| (0..logical.buf_w).step_by(5).map(move |x| (x, y)));
        for (x, y) in points {
            let (fx, fy) = cutaway.frame_px(pixtuoid_scene::layout::Point { x, y });
            let want = pixels.get(x * density, y * density);
            assert_eq!(
                frame.get_pixel(fx, fy).0[..3],
                [want.r, want.g, want.b],
                "logical ({x}, {y})"
            );
        }
    }
}
