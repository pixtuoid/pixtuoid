//! The frame-pacing bench's way into the real TUI painter: a renderer over
//! ratatui's own crossterm encoder and, for an image protocol, the real tile
//! sink, both writing into a [`Wire`] that counts bytes and the time spent
//! writing them. Mechanism for `examples/pacing.rs`, not a contract.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use pixtuoid_core::state::SceneState;
use pixtuoid_scene::pack::OfficeArt;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::layout::{Position, Rect, Size};

use crate::graphics::{CellSize, ImageProtocol, cutaway_fit};
use crate::tui::tui_renderer::TuiRenderer;

/// How the bench's office reaches the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// The classic, in half-block cells.
    HalfBlock,
    /// The cutaway over the kitty graphics protocol.
    Kitty,
    /// The cutaway over SIXEL.
    Sixel,
    /// The cutaway over iTerm2's inline images.
    Iterm2,
}

impl Protocol {
    /// Every way the bench's office reaches a terminal, in the report's order.
    pub const ALL: [Protocol; 4] = [
        Protocol::HalfBlock,
        Protocol::Kitty,
        Protocol::Sixel,
        Protocol::Iterm2,
    ];

    /// The image protocol the cutaway rides; `None` for the half-blocks.
    fn image(self) -> Option<ImageProtocol> {
        match self {
            Protocol::HalfBlock => None,
            Protocol::Kitty => Some(ImageProtocol::Kitty),
            Protocol::Sixel => Some(ImageProtocol::Sixel),
            Protocol::Iterm2 => Some(ImageProtocol::Iterm2),
        }
    }

    /// Its name in the report: the image protocol's own, or `half-block`.
    pub fn name(self) -> &'static str {
        self.image().map_or("half-block", ImageProtocol::name)
    }
}

/// What a [`Wire`] has carried: bytes, and the time spent writing them.
#[derive(Debug, Default)]
struct Carried {
    bytes: AtomicU64,
    write_ns: AtomicU64,
    #[cfg(test)]
    writes: AtomicU64,
}

/// A writer that discards what it is given, counting its bytes and timing each
/// write and flush on the monotonic clock. Clones share one count.
#[derive(Debug, Clone, Default)]
pub struct Wire(Arc<Carried>);

impl Wire {
    /// The writes since the last take, zeroing them: a frame presented whole
    /// is one.
    #[cfg(test)]
    fn take_writes(&self) -> u64 {
        self.0.writes.swap(0, Ordering::Relaxed)
    }

    /// Bytes and write nanoseconds since the last take, zeroing both.
    pub fn take(&self) -> (u64, u64) {
        (
            self.0.bytes.swap(0, Ordering::Relaxed),
            self.0.write_ns.swap(0, Ordering::Relaxed),
        )
    }

    fn timed<T>(&self, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let out = f();
        let ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.0.write_ns.fetch_add(ns, Ordering::Relaxed);
        out
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.timed(|| {
            #[cfg(test)]
            self.0.writes.fetch_add(1, Ordering::Relaxed);
            self.0.bytes.fetch_add(buf.len() as u64, Ordering::Relaxed);
            std::hint::black_box(buf);
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.timed(|| ());
        Ok(())
    }
}

/// A terminal `size` cells big whose cells are `cell` pixels, encoding through
/// ratatui's crossterm backend into a [`Wire`]: nothing asks a real terminal.
#[derive(Debug)]
pub struct PacedBackend {
    inner: CrosstermBackend<crate::tui::FrameOut>,
    size: Size,
    cell: CellSize,
    cursor: Position,
}

impl PacedBackend {
    /// Resize the terminal to `cols`×`rows`, as a window drag does.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.size = Size::new(cols, rows);
    }
}

impl Backend for PacedBackend {
    type Error = std::io::Error;
    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }
    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> std::io::Result<Position> {
        Ok(self.cursor)
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
        let position = position.into();
        self.cursor = position;
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> std::io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> std::io::Result<Size> {
        Ok(self.size)
    }
    fn window_size(&mut self) -> std::io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size::new(
                self.size.width * self.cell.w,
                self.size.height * self.cell.h,
            ),
        })
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

/// A renderer drawing the office over `protocol` into a `cols`×`rows`
/// terminal of `cell_px` cells, with `pets`, and the wire every byte it
/// writes crosses.
///
/// # Errors
///
/// The renderer's, when its terminal can't be built over the backend.
pub fn renderer(
    protocol: Protocol,
    cols: u16,
    rows: u16,
    cell_px: (u16, u16),
    pets: Vec<pixtuoid_scene::pet::Pet>,
    pack: Arc<OfficeArt>,
) -> Result<(TuiRenderer<PacedBackend>, Wire)> {
    let wire = Wire::default();
    // As the TUI does: each frame's text and images held and written whole.
    let out = crate::tui::FrameOut::new(wire.clone(), false);
    let cell = CellSize {
        w: cell_px.0,
        h: cell_px.1,
    };
    let backend = PacedBackend {
        inner: CrosstermBackend::new(out.clone()),
        size: Size::new(cols, rows),
        cell,
        cursor: Position::ORIGIN,
    };
    let mut r = TuiRenderer::new(
        Terminal::new(backend)?,
        &pixtuoid_scene::theme::NORMAL,
        pets,
        Arc::clone(&pack),
    );
    if let Some(image) = protocol.image() {
        let area = crate::tui::renderer::scene_rect(Rect::new(0, 0, cols, rows)).as_size();
        let fit = cutaway_fit(cell, area, pack.max_density_variant())
            .context("the cell is too small for the cutaway")?;
        r.set_cutaway(crate::tui::cutaway::TileCutaway::new(
            fit,
            cell,
            crate::graphics::Route::direct(image, false),
            Box::new(out.clone()),
        ));
    }
    r.present_through(out);
    Ok((r, wire))
}

/// The cutaway's office in logical units, and its scale, on a `cols`×`rows`
/// terminal of `cell_px` cells: `None` where the cell fits no cutaway.
pub fn cutaway_office(
    cols: u16,
    rows: u16,
    cell_px: (u16, u16),
    pack: &OfficeArt,
) -> Option<(u16, u16, u16)> {
    let cell = CellSize {
        w: cell_px.0,
        h: cell_px.1,
    };
    let area = crate::tui::renderer::scene_rect(Rect::new(0, 0, cols, rows)).as_size();
    let fit = cutaway_fit(cell, area, pack.max_density_variant())?;
    Some((fit.logical().w, fit.logical().h, fit.scale().get()))
}

/// Warm `r`'s caches on `scene` at `now`, as the TUI does at boot.
pub fn warm(r: &mut TuiRenderer<PacedBackend>, scene: &SceneState, now: std::time::SystemTime) {
    r.warm(scene, now);
}

/// Paint and send `r`'s next frame whole.
pub fn forget_frame(r: &mut TuiRenderer<PacedBackend>) {
    r.forget_frame();
}

#[cfg(test)]
mod tests {
    use super::{ImageProtocol, Protocol};

    /// A whole frame through the real painter, its kitty tiles and its text,
    /// reaches the terminal in one write.
    #[test]
    fn a_rendered_frame_is_one_write() {
        let pack =
            std::sync::Arc::new(pixtuoid_scene::pack::load_bundled_pack().expect("the pack"));
        let (mut r, wire) =
            super::renderer(Protocol::Kitty, 120, 40, (8, 16), vec![], pack).expect("a cutaway");
        let scene = pixtuoid_core::SceneState::uniform(8);
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        wire.take_writes();
        r.render(&scene, now).expect("render");
        let (bytes, _) = wire.take();
        assert!(bytes > 100_000, "a whole frame of tiles: {bytes}");
        assert_eq!(wire.take_writes(), 1);
    }

    /// The bench measures every protocol the cutaway speaks: a new
    /// `ImageProtocol` fails to compile here until it has a `Protocol`, whose
    /// `ALL` entry, beside the enum, is by hand.
    #[test]
    fn all_lists_every_protocol_the_cutaway_speaks() {
        let of = |image: ImageProtocol| match image {
            ImageProtocol::Kitty => Protocol::Kitty,
            ImageProtocol::Sixel => Protocol::Sixel,
            ImageProtocol::Iterm2 => Protocol::Iterm2,
        };
        for protocol in Protocol::ALL {
            if let Some(image) = protocol.image() {
                assert_eq!(of(image), protocol);
            }
        }
    }
}
