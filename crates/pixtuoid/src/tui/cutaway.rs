//! The cutaway as terminal images: a frame's changed tiles are encoded in the
//! plan's protocol, kitty's written before ratatui's flush and SIXEL's and
//! iTerm2's after it, once the frame's text cells are known.
//!
//! - kitty shows every tile through Unicode placeholders, which are ordinary
//!   text, so a modal or tooltip drawn after them takes their cells as any
//!   text would (<https://sw.kovidgoyal.net/kitty/graphics-protocol/>,
//!   "Unicode placeholders").
//! - SIXEL and iTerm2 draw pixels at the cursor. Their cells are left out of
//!   ratatui's diff, and a tile under text is sent without its text cells:
//!   text and image never share a cell.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime};

use pixtuoid_core::sprite::format::Density;
use pixtuoid_core::sprite::{Rgb, RgbBuffer};
use pixtuoid_scene::cutaway::canvas::Dirty;
use pixtuoid_scene::layout::Size;
use pixtuoid_scene::look::Picture;
use pixtuoid_scene::screen::Admit;
use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::layout::{Position, Rect};

use crate::graphics::tiles::{Carve, Changed, Tile, Tiles};
use crate::graphics::{CellSize, ImageProtocol, cutaway_fit, iterm2, kitty, sixel};
use crate::jank::FrameSend;
use crate::tui::geometry::SceneGeometry;
use pixtuoid_scene::render_scale::PixelFit;

/// Where the transmits go: the terminal ratatui's backend also writes to.
pub(crate) type Sink = Box<dyn Write + Send>;

/// What an image cell holds before the frame's text is drawn: a cell still
/// equal to it afterwards is image, any other is text.
const SENTINEL: &str = "\u{F8FF}";

/// What a frame's tiles are cut for: the scene's cells, each `cell` big, and
/// the office fitted over them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fitted {
    pub(crate) scene: Rect,
    pub(crate) cell: CellSize,
    pub(crate) fit: PixelFit,
}

impl Fitted {
    /// The look the office renders in: the cutaway, its world text left to
    /// the terminal, which sets it in its own font.
    pub(crate) fn look(self) -> pixtuoid_scene::look::Look {
        self.fit.look(pixtuoid_scene::look::WorldText::Host)
    }

    /// The image's cells, its top-left on the scene's.
    pub(crate) fn geometry(self) -> SceneGeometry {
        SceneGeometry::Cutaway {
            origin: self.scene.as_position(),
            cell: self.cell,
            scale: self.fit.scale(),
        }
    }
}

/// The cutaway's tiles, and what of them the terminal holds.
pub(crate) struct TileCutaway {
    /// The plan's cell.
    planned: CellSize,
    /// The window's cell on the first frame that read one.
    first_window: Option<CellSize>,
    density: Density,
    route: crate::graphics::Route,
    /// [`kitty::process_base`].
    base: u32,
    /// `None` while classic paints.
    fitted: Option<Fitted>,
    tiles: Tiles,
    /// What the tiles are cut from when sent: the last frame, or a slide's
    /// two composed.
    image: RgbBuffer,
    /// The last frame or slide the screen held back: [`Self::image`] lags
    /// it whatever the next frame's damage.
    image_behind: bool,
    /// What the last image showed: a floor's raster diffs against its own
    /// last frame, so any other image before it — a slide, another floor —
    /// leaves the tiles differing anywhere.
    shown: Option<Shown>,
    /// The image's top-left cell.
    origin: Position,
    out: Sink,
    /// This frame's write, once queued; its tiles carved around the frame's
    /// text once that is drawn.
    pending: Option<Pending>,
    /// Set once SIXEL or iTerm2 pixels are written, for the unwind
    /// ([`crate::graphics::grid_unwind`]).
    in_grid: &'static AtomicBool,
    /// When the last transmits were written, for the protocol's cadence.
    sent_at: Option<SystemTime>,
    /// What the last frame's transmits did, for its jank report.
    last: FrameSend,
    /// The cores a frame's encode splits across: the platform's, or a test's
    /// (`split_across`).
    cores: usize,
}

/// A queued write: the tiles it sends.
struct Pending {
    tiles: Vec<Changed>,
    /// Whether the protocol's cadence lets it send now.
    due: bool,
}

impl std::fmt::Debug for TileCutaway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileCutaway")
            .field("planned", &self.planned)
            .field("density", &self.density)
            .field("route", &self.route)
            .field("origin", &self.origin)
            .field("pending", &self.pending.as_ref().map(|p| p.tiles.len()))
            .finish_non_exhaustive()
    }
}

/// What a [`TileCutaway`]'s image last showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Floor(usize),
    Slide,
}

impl TileCutaway {
    /// [`crate::graphics::Plan::Cutaway`]'s parts, transmitting into `out`.
    pub(crate) fn new(
        fit: PixelFit,
        cell: CellSize,
        route: crate::graphics::Route,
        out: Sink,
    ) -> Self {
        Self {
            planned: cell,
            first_window: None,
            density: fit.density(),
            route,
            base: kitty::process_base(),
            fitted: None,
            tiles: Tiles::new(route.protocol(), cell, fit),
            image: RgbBuffer::filled(0, 0, Rgb { r: 0, g: 0, b: 0 }),
            image_behind: false,
            shown: None,
            origin: Position::ORIGIN,
            out,
            pending: None,
            in_grid: &crate::graphics::IN_GRID,
            sent_at: None,
            last: FrameSend::default(),
            cores: pixtuoid_scene::par::cores(),
        }
    }

    /// Fit the office over `scene`'s cells under a window whose cell reads
    /// `window`; `None` while the cell has no [`cutaway_fit`], when classic paints.
    /// Every frame fits, a refused one too: any change re-sends every tile,
    /// since a resize clears the screen ratatui redraws, a new cell cuts a new
    /// grid, and classic paints over the image.
    pub(crate) fn fit_to(&mut self, scene: Rect, window: Option<CellSize>) -> Option<Fitted> {
        let cell = self.cell_under(window);
        let Some(fit) = cutaway_fit(cell, scene.as_size(), self.density) else {
            self.fitted = None;
            // The classic paints this frame: it transmits nothing.
            self.last = FrameSend {
                dirty: crate::jank::Painted::Classic,
                ..FrameSend::default()
            };
            return None;
        };
        let fitted = Fitted { scene, cell, fit };
        if self.fitted != Some(fitted) {
            self.tiles = Tiles::new(self.route.protocol(), cell, fit);
        }
        self.fitted = Some(fitted);
        Some(fitted)
    }

    /// The plan's cell, the terminal's answer where it gave one (as
    /// [`crate::graphics::probe`] ranks them), until a font zoom moves the
    /// window's cell from what it read on the first frame.
    fn cell_under(&mut self, window: Option<CellSize>) -> CellSize {
        if self.first_window.is_none() {
            self.first_window = window;
        }
        match window {
            Some(cell) if Some(cell) != self.first_window => cell,
            _ => self.planned,
        }
    }

    /// The office's logical extent as last fitted; `None` while classic
    /// paints.
    pub(crate) fn office(&self) -> Option<Size> {
        self.fitted.map(|f| f.fit.logical())
    }

    /// Show `picture` as `fitted`, as the terminal's screen `admit`ted it:
    /// queue the tiles it changed once the protocol's cadence allows, at once
    /// for a changed flash phase; until then they stay owed. A held picture
    /// is not sent, so the terminal keeps the last.
    pub(crate) fn paint(
        &mut self,
        fitted: Fitted,
        picture: &Picture<'_>,
        admit: Admit,
        now: SystemTime,
    ) {
        let image = match picture {
            Picture::Floor(floor) => Shown::Floor(floor.floor),
            Picture::Slide(_) => Shown::Slide,
        };
        // A slide's composed image is new every frame.
        let fresh = image == Shown::Slide || self.shown != Some(image);
        self.shown = Some(image);
        let dirty = if fresh {
            Dirty::All
        } else {
            picture.dirty().clone()
        };
        if admit == Admit::Held {
            self.tiles.owe(&dirty);
            self.image_behind = true;
            self.pending = None;
            return;
        }
        if dirty != Dirty::Unchanged || self.image_behind {
            self.image.clone_from(picture.pixels());
            self.image_behind = false;
        }
        let changed = admit == Admit::Changed;
        self.queue(&dirty, fresh, changed, now, fitted.scene.as_position());
    }

    /// Queue the tiles of [`Self::image`] that differ from what was sent,
    /// among those `dirty` reaches, once the cadence allows: at once for a
    /// `new_phase`, so it shows as long as it lasts. `fresh` is reported as
    /// [`FrameSend::fresh`].
    fn queue(
        &mut self,
        dirty: &Dirty,
        fresh: bool,
        new_phase: bool,
        now: SystemTime,
        origin: Position,
    ) {
        self.origin = origin;
        let changed =
            tracing::trace_span!("tiles.diff").in_scope(|| self.tiles.changed(&self.image, dirty));
        self.last = FrameSend {
            dirty: dirty.into(),
            fresh,
            changed: changed.len(),
            ..FrameSend::default()
        };
        tracing::trace!(
            dirty = ?self.last.dirty,
            rects = match dirty {
                Dirty::Rects(r) => r.as_slice().len(),
                _ => 0,
            },
            rect_px = match dirty {
                Dirty::Rects(r) => r
                    .as_slice()
                    .iter()
                    .map(|b| u64::from(b.width) * u64::from(b.height))
                    .sum::<u64>(),
                _ => 0,
            },
            changed = changed.len(),
            "tiles.stage"
        );
        let due = new_phase
            || self.sent_at.is_none_or(|at| {
                now.duration_since(at)
                    .map_or(true, |since| since >= self.route.protocol().cadence())
            });
        self.pending = Some(Pending {
            tiles: if due { changed } else { Vec::new() },
            due,
        });
    }

    /// Send kitty's tiles before ratatui's flush, so the placeholders it
    /// writes find their images there.
    pub(crate) fn before_flush(&mut self, now: SystemTime) -> std::io::Result<()> {
        if self.route.protocol() == ImageProtocol::Kitty {
            self.send(now)?;
        }
        Ok(())
    }

    /// Send SIXEL's or iTerm2's tiles after ratatui's flush, each carved
    /// around the text cells `carves` marks: only then are they known. A tile
    /// whose text moved is sent again, to give the cells text left back to
    /// the image.
    pub(crate) fn after_flush(&mut self, carves: &[Carve], now: SystemTime) -> std::io::Result<()> {
        if self.route.protocol() == ImageProtocol::Kitty {
            return Ok(());
        }
        if let Some(pending) = self.pending.as_mut().filter(|p| p.due) {
            pending.tiles = self.tiles.carved(&pending.tiles, carves);
        }
        self.send(now)
    }

    /// Encode and write the queued tiles, to land with the frame, counting them
    /// as shown: a frame that fails to land ends the run.
    fn send(&mut self, now: SystemTime) -> std::io::Result<()> {
        let Some(Pending { tiles: send, .. }) = self.pending.take() else {
            return Ok(());
        };
        if send.is_empty() {
            return Ok(());
        }
        match self.route.protocol() {
            ImageProtocol::Kitty => {
                if let Some(last) = send
                    .iter()
                    .filter_map(|c| kitty::image_id(self.base, c.tile))
                    .max()
                {
                    kitty::on_screen(self.route.tmux(), last);
                }
            }
            ImageProtocol::Sixel | ImageProtocol::Iterm2 => {
                self.in_grid.store(true, Ordering::Relaxed)
            }
        }
        let begun = Instant::now();
        let encoded =
            tracing::trace_span!("tiles.encode").in_scope(|| self.encoder().encode_all(&send));
        self.last.encode = begun.elapsed();
        let mut sent = Vec::with_capacity(send.len());
        for (c, bytes) in send.into_iter().zip(encoded) {
            if let Some(bytes) = bytes {
                tracing::trace_span!("tile.write").in_scope(|| self.out.write_all(&bytes))?;
                self.last.bytes += bytes.len() as u64;
                sent.push(c);
            }
        }
        self.out.flush()?;
        self.last.sent = sent.len();
        self.sent_at = Some(now);
        self.tiles.sent(&sent);
        Ok(())
    }

    /// What encodes this frame's tiles, apart from the sink no thread shares.
    fn encoder(&self) -> Encoder<'_> {
        Encoder {
            tiles: &self.tiles,
            image: &self.image,
            route: self.route,
            base: self.base,
            origin: self.origin,
            threads: self.cores,
        }
    }

    /// A new frame, which has transmitted nothing until it paints.
    pub(crate) fn begin_frame(&mut self) {
        self.last = FrameSend::default();
    }

    /// Set `in_grid` where the unwind would read the process's own.
    #[cfg(test)]
    pub(crate) fn arming(mut self, in_grid: &'static AtomicBool) -> Self {
        self.in_grid = in_grid;
        self
    }

    /// Split a frame's encode across `cores` whatever the machine has.
    #[cfg(test)]
    pub(crate) fn split_across(&mut self, cores: usize) {
        self.cores = cores;
    }

    /// The tile the grid sends in now, its image budget applied.
    #[cfg(test)]
    pub(crate) fn tile_shape(&self) -> crate::graphics::TileShape {
        self.tiles.shape()
    }

    /// Show every tile in `scene`'s cells of `buf`, before the frame's text
    /// is drawn: kitty's placeholders, or for SIXEL and iTerm2 a
    /// [`SENTINEL`] ratatui's diff skips, so the flush never blanks the
    /// pixels.
    pub(crate) fn place(&self, buf: &mut Buffer, scene: Rect) {
        if self.route.protocol() == ImageProtocol::Kitty {
            let tiles = self
                .tiles
                .all()
                .filter_map(|tile| kitty::placeholders(kitty::image_id(self.base, tile)?, tile));
            for p in tiles.flatten() {
                if let Some(cell) = image_cell(buf, scene, p.col, p.row) {
                    cell.reset();
                    cell.set_symbol(&p.symbol).set_fg(p.fg);
                }
            }
            return;
        }
        for tile in self.tiles.all() {
            for (col, row) in cells(tile) {
                if let Some(cell) = image_cell(buf, scene, col, row) {
                    *cell = sentinel();
                }
            }
        }
    }

    /// Per tile, its cells text was drawn in since [`Self::place`], each
    /// handed back to ratatui's diff. Kitty's text needs no room made.
    pub(crate) fn carve(&self, buf: &mut Buffer, scene: Rect) -> Vec<Carve> {
        if self.route.protocol() == ImageProtocol::Kitty {
            return Vec::new();
        }
        let sentinel = sentinel();
        self.tiles
            .all()
            .map(|tile| {
                let mut carve = 0;
                for (col, row) in cells(tile) {
                    if let Some(cell) = image_cell(buf, scene, col, row)
                        && *cell != sentinel
                    {
                        cell.set_diff_option(CellDiffOption::None);
                        carve |= tile.bit(col - tile.col, row - tile.row);
                    }
                }
                carve
            })
            .collect()
    }

    /// What the last frame's transmits did.
    pub(crate) fn last_send(&self) -> FrameSend {
        self.last
    }

    /// Owe every tile again: the terminal may have dropped them.
    pub(crate) fn forget(&mut self) {
        self.tiles.forget();
    }
}

/// A thread's least share of a frame's tiles: below it, its spawn outweighs
/// their encode, which a frame after the TUI's sleep runs on cooled cores
/// ([`pixtuoid_scene::par`]).
pub(crate) const TILES_PER_THREAD: usize = 2;

/// The threads a frame of `tiles` splits across on `cores`: one a
/// [`TILES_PER_THREAD`] share, never more than the cores.
fn threads_for(tiles: usize, cores: usize) -> usize {
    cores.min(tiles / TILES_PER_THREAD).max(1)
}

/// A frame's tiles to escapes, each independent of the others.
#[derive(Clone, Copy)]
struct Encoder<'a> {
    tiles: &'a Tiles,
    image: &'a RgbBuffer,
    route: crate::graphics::Route,
    base: u32,
    origin: Position,
    threads: usize,
}

impl Encoder<'_> {
    /// Each of `send`'s escapes, in its order, split across as many threads
    /// as it has [`TILES_PER_THREAD`] shares, up to the cores.
    fn encode_all(self, send: &[Changed]) -> Vec<Option<Vec<u8>>> {
        pixtuoid_scene::par::map(send, threads_for(send.len(), self.threads), |&c| {
            self.encode(c)
        })
    }

    /// `c`'s tile in the protocol's escape, carved: one image a
    /// [piece](Tile::pieces); `None` where it has none.
    fn encode(self, c: Changed) -> Option<Vec<u8>> {
        let _encode = tracing::trace_span!("tile.encode").entered();
        let cut =
            |tile| tracing::trace_span!("tile.cut").in_scope(|| self.tiles.image(self.image, tile));
        match self.route.protocol() {
            ImageProtocol::Kitty => kitty::image_id(self.base, c.tile).map(|id| {
                kitty::transmit(id, &cut(c.tile), self.route.tmux(), self.route.medium())
            }),
            ImageProtocol::Sixel => Some(
                c.tile
                    .pieces(c.carve)
                    .into_iter()
                    .flat_map(|piece| sixel::transmit(&cut(piece), self.origin))
                    .collect(),
            ),
            ImageProtocol::Iterm2 => c
                .tile
                .pieces(c.carve)
                .into_iter()
                .map(|piece| iterm2::transmit(&cut(piece), self.origin))
                .collect::<Result<Vec<_>, _>>()
                .map(|pieces| pieces.concat())
                .inspect_err(|e| tracing::warn!(error = %e, "iterm2 encode failed"))
                .ok(),
        }
    }
}

fn sentinel() -> Cell {
    let mut cell = Cell::new(SENTINEL);
    cell.set_diff_option(CellDiffOption::Skip);
    cell
}

/// `tile`'s cells, from the image's top-left.
fn cells(tile: Tile) -> impl Iterator<Item = (u16, u16)> {
    (tile.row..tile.row + tile.rows)
        .flat_map(move |row| (tile.col..tile.col + tile.cols).map(move |col| (col, row)))
}

/// The cell of `buf` showing image cell `(col, row)`, inside `scene`.
fn image_cell(buf: &mut Buffer, scene: Rect, col: u16, row: u16) -> Option<&mut Cell> {
    let at = Position {
        x: scene.x.saturating_add(col),
        y: scene.y.saturating_add(row),
    };
    scene.contains(at).then(|| buf.cell_mut(at)).flatten()
}

#[cfg(test)]
mod tests {
    use super::{TILES_PER_THREAD, threads_for};

    /// A frame of two shares splits across two cores, one share short of
    /// that stays on one thread, and one core never splits: a serial
    /// regression fails here, where an instruction count can't see it.
    #[test]
    fn a_frame_of_two_shares_splits_across_the_cores() {
        assert_eq!(threads_for(2 * TILES_PER_THREAD, 8), 2);
        assert_eq!(threads_for(2 * TILES_PER_THREAD - 1, 8), 1);
        assert_eq!(threads_for(100 * TILES_PER_THREAD, 8), 8);
        assert_eq!(threads_for(100 * TILES_PER_THREAD, 1), 1);
    }
}
