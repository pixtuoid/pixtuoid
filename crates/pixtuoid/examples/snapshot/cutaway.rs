//! `--cutaway`: the office a graphics terminal of [`terminal_cell`]s shows,
//! in place of the classic frame's office, above its footer. Its world text is the scene's pixel font
//! (the floating window's), where the terminal sets it in its own.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use pixtuoid_scene::cutaway::Canvas as _;
use pixtuoid_scene::floor::{FloorInputs, FloorSession};
use pixtuoid_scene::look::{Dim, Place, RenderInputs, WorldText};
use pixtuoid_scene::pack::OfficeArt;
use pixtuoid_scene::render_scale::PixelFit;
use pixtuoid_scene::theme::Theme;

use crate::encode::{ImageCanvas, terminal_cell};

/// The TUI's cutaway on one terminal: a floor session of its own, stepped on
/// the classic frame's inputs.
pub(crate) struct CutawayOffice {
    session: FloorSession,
    fit: PixelFit,
}

impl CutawayOffice {
    /// The cutaway the TUI fits to a `cols`×`rows` terminal.
    pub(crate) fn new(pack: &Arc<OfficeArt>, cols: u16, rows: u16) -> Result<Self> {
        let cell = terminal_cell();
        let fit = pixtuoid::dev::cutaway_office(cols, rows, (cell.w, cell.h), pack)
            .with_context(|| format!("a {}x{} px cell fits no cutaway", cell.w, cell.h))?;
        Ok(Self {
            session: FloorSession::new(Arc::clone(pack)),
            fit,
        })
    }

    /// Step and draw `world`'s office.
    pub(crate) fn render(&mut self, world: FloorInputs<'_>, theme: &'static Theme) -> Result<()> {
        self.session.render(
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
            .picture()
            .map(drop)
            .context("the cutaway does not lay out")
    }

    /// The last frame over `img`'s scene cells, which start at its top-left.
    pub(crate) fn paint<I: image::GenericImage>(&self, img: &mut I)
    where
        I::Pixel: image::Pixel<Subpixel = u8>,
    {
        let Some(picture) = self.session.picture() else {
            return;
        };
        let buf = picture.pixels;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::cells_to_rgba;
    use crate::{CELL_H, scenes::sample_scene};
    use image::RgbaImage;
    use pixtuoid::dev::{DrawCtx, draw_scene, scene_buf_size};
    use pixtuoid_scene::floor::{FloorMeta, PerFloor, PerOffice};
    use pixtuoid_scene::theme::NORMAL;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::{Duration, UNIX_EPOCH};

    /// The cutaway lands on the scene's cells and nowhere else: the footer
    /// under it stays the classic frame's, pixel for pixel.
    #[test]
    fn the_cutaway_takes_the_office_and_keeps_the_footer() {
        let (cols, rows) = (96, 40);
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let scene = sample_scene(now, 12, 6);
        let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack().unwrap());
        let meta = FloorMeta::ground();
        let mut term = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        let mut floor = PerFloor::new(Arc::clone(&pack));
        let mut office = PerOffice::new();
        let mut ctx = DrawCtx::offscreen(&mut floor, office.stores(), &NORMAL, &scene, now, meta);
        draw_scene(&mut term, &mut ctx).unwrap();
        let classic = cells_to_rgba(term.backend().buffer());

        let mut cutaway = CutawayOffice::new(&pack, cols, rows).unwrap();
        let world = FloorInputs {
            scene: &scene,
            now,
            floor: meta,
            pets: Default::default(),
        };
        cutaway.render(world, &NORMAL).unwrap();
        let mut composed = classic.clone();
        cutaway.paint(&mut composed);

        let footer_top = u32::from(scene_buf_size(cols, rows).1 / 2) * CELL_H;
        let band = |img: &RgbaImage, top: u32, bottom: u32| {
            (top..bottom)
                .flat_map(|y| (0..img.width()).map(move |x| *img.get_pixel(x, y)))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            band(&composed, footer_top, composed.height()),
            band(&classic, footer_top, classic.height()),
            "the footer moved"
        );
        let office = band(&composed, 0, footer_top);
        let changed = office
            .iter()
            .zip(band(&classic, 0, footer_top))
            .filter(|(a, b)| **a != *b)
            .count();
        assert!(
            changed * 2 > office.len(),
            "only {changed} of {} office pixels are the cutaway's",
            office.len()
        );
    }
}
