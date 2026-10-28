//! `--cutaway`: the office a graphics terminal of [`terminal_cell`]s shows,
//! in place of the classic frame's office, above its footer and under its panels. Its world text is the scene's pixel font
//! (the floating window's), where the terminal sets it in its own.

use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context as _, Result};
use pixtuoid::dev::ImageCells;
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
    /// The cells its last frame shows under that frame's text; all of them
    /// where none was drawn over it.
    under: Option<ImageCells>,
}

/// What a cutaway steps, on the inputs a classic frame of the same office
/// steps on.
pub(crate) trait CutawaySession {
    /// Step and draw one frame of `inputs` in `look`.
    fn draw(&mut self, look: Look, inputs: RenderInputs<'_>);

    /// The last frame's pixels; `None` before the first, and when it could not
    /// lay out.
    fn pixels(&self) -> Option<&RgbBuffer>;
}

/// [`crate::encode::draw_frame`]'s one floor, under the `FloorMeta` it is
/// handed (`--floor-seed`).
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
    /// The cutaway [`crate::encode::draw_frame`] draws on a `cols`×`rows`
    /// terminal.
    pub(crate) fn of_draw_frame(pack: &Arc<OfficeArt>, cols: u16, rows: u16) -> Result<Self> {
        Self::fitted(FloorSession::new(Arc::clone(pack)), pack, cols, rows)
    }

    /// Its floor's stores, to seed as the classic floor is seeded.
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
        Ok(Self {
            session,
            fit,
            under: None,
        })
    }

    /// Step and draw `world`'s office, dimmed as `under`'s backdrop is;
    /// [`Self::paint`] shows it on `under`'s image cells (every scene cell
    /// where `None`).
    pub(crate) fn render(
        &mut self,
        world: FloorInputs<'_>,
        theme: &'static Theme,
        under: Option<ImageCells>,
    ) -> Result<()> {
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
                dim: under.as_ref().map_or(Dim::NONE, ImageCells::dim),
            },
        );
        self.under = under;
        self.session
            .pixels()
            .map(drop)
            .context("the cutaway does not lay out")
    }

    /// The last frame over `img`'s scene cells, which start at its top-left,
    /// on those its frame's text left to it.
    pub(crate) fn paint(&self, img: &mut image::RgbaImage) {
        let Some(buf) = self.session.pixels() else {
            return;
        };
        let w = u32::from(buf.width());
        let up = u32::from(self.fit.upscale());
        let cell = terminal_cell();
        let shows = |x: u32, y: u32| {
            self.under.as_ref().is_none_or(|under| {
                let col = u16::try_from(x / u32::from(cell.w)).unwrap_or(u16::MAX);
                let row = u16::try_from(y / u32::from(cell.h)).unwrap_or(u16::MAX);
                under.shows(col, row)
            })
        };
        let mut canvas = ImageCanvas(img);
        for (i, &px) in buf.as_slice().iter().enumerate() {
            let (x, y) = (i as u32 % w * up, i as u32 / w * up);
            for (dx, dy) in (0..up).flat_map(|dy| (0..up).map(move |dx| (dx, dy))) {
                let (x, y) = (x + dx, y + dy);
                if shows(x, y) {
                    canvas.set(x as i32, y as i32, px);
                }
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
        AnimJob, FrameSink, Timeline, cells_to_rgba, compose, draw_frame, save_renderer_animation,
    };
    use crate::{CELL_H, CELL_W, scenes::sample_scene};
    use image::{Rgba, RgbaImage};
    use pixtuoid::dev::{DrawCtx, RenderFrames, scene_buf_size};
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

    /// A [`COLS`]×[`ROWS`] still of `scene` under `frames`' panels through
    /// [`draw_frame`]: the classic, the cutaway, and the cutaway drawn.
    fn stills(
        scene: &SceneState,
        frames: &RenderFrames,
    ) -> (RgbaImage, RgbaImage, CutawayOffice<FloorSession>) {
        let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack().unwrap());
        let mut floor = PerFloor::new(Arc::clone(&pack));
        let mut office = PerOffice::new();
        let mut ctx = DrawCtx {
            overlays: frames.overlays(0.0, &[]),
            ..DrawCtx::offscreen(
                &mut floor,
                office.stores(),
                &NORMAL,
                scene,
                epoch(),
                FloorMeta::ground(),
            )
        };
        let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
        draw_frame(&mut term, &mut ctx, None).unwrap();
        let cells = term.backend().buffer();
        let classic = cells_to_rgba(cells, cells.area);
        let mut cutaway = CutawayOffice::of_draw_frame(&pack, COLS, ROWS).unwrap();
        let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
        draw_frame(&mut term, &mut ctx, Some(&mut cutaway)).unwrap();
        let cells = term.backend().buffer();
        let composed = compose(cells, cells.area, Some(&cutaway));
        (classic, composed, cutaway)
    }

    /// The cutaway lands on the scene's cells and nowhere else: the footer
    /// under it stays the classic frame's, pixel for pixel.
    #[test]
    fn the_cutaway_takes_the_office_and_keeps_the_footer() {
        let scene = sample_scene(epoch(), 12, 6);
        let (classic, cutaway, _) = stills(&scene, &RenderFrames::default());
        assert_cutaway_over(&cutaway, &classic);
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
        let mut lit = CutawayOffice::of_draw_frame(&pack, COLS, ROWS).unwrap();
        let mut settled = CutawayOffice::of_draw_frame(&pack, COLS, ROWS).unwrap();
        let mut classic = PerFloor::new(Arc::clone(&pack));
        crate::settle_empty(&mut classic, Some(&mut settled));
        let level = |floor: &PerFloor| floor.ctx.vacancy_dim.level();
        assert!(level(&classic) < 1.0, "the classic floor stayed lit");
        assert_eq!(level(settled.session.floor()), level(&classic));

        let brightness = |cutaway: &mut CutawayOffice<FloorSession>| -> u64 {
            cutaway.render(world, &NORMAL, None).unwrap();
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
        let mut cutaway = CutawayOffice::of_draw_frame(&pack, COLS, ROWS).unwrap();
        let world = FloorInputs {
            scene: &scene,
            now: epoch(),
            floor: FloorMeta::ground(),
            pets: Default::default(),
        };
        cutaway.render(world, &NORMAL, None).unwrap();
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

    /// `--cutaway --dashboard`: the panel's cells are the classic frame's, as
    /// the TUI draws a panel over its image, and the cutaway shows around them.
    #[test]
    fn a_panel_keeps_its_cells_over_the_cutaway() {
        use pixtuoid::dev::{DashboardFolds, DashboardFrame, build_dashboard_rows};
        let scene = sample_scene(epoch(), 12, 6);
        let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
        let frames = RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                selected: rows.first().map(|r| r.agent_id),
                rows,
                scroll: 0,
            },
            ..Default::default()
        };
        let (classic, composed, cutaway) = stills(&scene, &frames);
        assert_cutaway_over(&composed, &classic);
        let image = cutaway.under.as_ref().expect("text over the cutaway");

        let scene_rows = footer_top() / CELL_H;
        let panel: Vec<_> = (0..scene_rows)
            .flat_map(|row| (0..u32::from(COLS)).map(move |col| (col, row)))
            .filter(|&(col, row)| !image.shows(col as u16, row as u16))
            .collect();
        assert!(!panel.is_empty(), "no cell is the panel's");
        let block = |img: &RgbaImage, (col, row): (u32, u32)| {
            band(img, row * CELL_H..(row + 1) * CELL_H)
                .chunks(img.width() as usize)
                .flat_map(|line| {
                    line[(col * CELL_W) as usize..((col + 1) * CELL_W) as usize].to_vec()
                })
                .collect::<Vec<_>>()
        };
        for &cell in &panel {
            assert_eq!(
                block(&composed, cell),
                block(&classic, cell),
                "panel cell {cell:?}"
            );
        }
    }

    /// `--cutaway --onboarding`: the backdrop dims the cutaway around the
    /// welcome card, as the TUI dims its own.
    #[test]
    fn the_onboarding_dims_the_cutaway_around_its_card() {
        use pixtuoid::dev::{OnboardingFrame, dim_opening};
        let scene = sample_scene(epoch(), 12, 6);
        let elapsed_ms = 100_000;
        let onboarding = RenderFrames {
            onboarding: OnboardingFrame {
                open: true,
                rows: Vec::new(),
                selected: 0,
                elapsed_ms,
                dim: dim_opening(elapsed_ms),
            },
            ..Default::default()
        };
        let (_, lit, _) = stills(&scene, &RenderFrames::default());
        let (_, dimmed, cutaway) = stills(&scene, &onboarding);
        let image = cutaway.under.as_ref().expect("text over the cutaway");
        let brightness = |img: &RgbaImage| -> u64 {
            (0..footer_top())
                .flat_map(|y| (0..img.width()).map(move |x| (x, y)))
                .filter(|&(x, y)| image.shows((x / CELL_W) as u16, (y / CELL_H) as u16))
                .map(|(x, y)| {
                    img.get_pixel(x, y).0[..3]
                        .iter()
                        .map(|&c| u64::from(c))
                        .sum::<u64>()
                })
                .sum()
        };
        assert!(
            brightness(&dimmed) < brightness(&lit),
            "the backdrop left the cutaway lit"
        );
    }
}
