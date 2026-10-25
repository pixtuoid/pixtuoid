//! The cutaway image cut into a grid of cell-aligned tiles, and which of them a
//! frame changed: an encoder re-sends only those.
use std::collections::BTreeSet;
use std::hash::{BuildHasher, Hash, Hasher};
use std::ops::Range;

use crossterm::{Command, cursor::MoveTo};
use pixtuoid_core::sprite::RgbBuffer;
use pixtuoid_scene::cutaway::canvas::Dirty;
use pixtuoid_scene::layout::Bounds;
use ratatui::layout::Position;

use super::{CellSize, ImageProtocol, PixelFit, TileShape};

/// One tile, in cells from the image's top-left cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tile {
    /// Row-major, stable while the buffer keeps its size.
    pub(crate) index: u32,
    /// Its left cell.
    pub(crate) col: u16,
    /// Its top cell.
    pub(crate) row: u16,
    /// Cells across: fewer than the shape's in the right-hand column.
    pub(crate) cols: u16,
    /// Cells down: fewer than the shape's in the bottom row.
    pub(crate) rows: u16,
}

impl Tile {
    /// The escape that moves the cursor to this tile's top-left cell, the
    /// image's own being `origin`: where a SIXEL or iTerm2 image is drawn.
    pub(crate) fn cursor_to(self, origin: Position) -> String {
        let mut out = String::new();
        // Writing to a `String` cannot fail.
        let _ = MoveTo(
            origin.x.saturating_add(self.col),
            origin.y.saturating_add(self.row),
        )
        .write_ansi(&mut out);
        out
    }

    /// Its cell `(col, row)`'s bit in a [`Carve`], counted from its top-left.
    pub(crate) fn bit(self, col: u16, row: u16) -> Carve {
        1 << (u32::from(row) * u32::from(self.cols) + u32::from(col))
    }

    /// What of it the image draws when the cells `carve` marks hold text:
    /// each run of image cells along a row, a run alike on the rows under it
    /// merged into one, so an uncarved tile is itself.
    pub(crate) fn pieces(self, carve: Carve) -> Vec<Tile> {
        let mut done = Vec::new();
        // The pieces that reach the last row, which this one may extend.
        let mut open: Vec<Tile> = Vec::new();
        for row in 0..self.rows {
            let mut next = Vec::new();
            let mut col = 0;
            while col < self.cols {
                if carve & self.bit(col, row) != 0 {
                    col += 1;
                    continue;
                }
                let start = col;
                while col < self.cols && carve & self.bit(col, row) == 0 {
                    col += 1;
                }
                let (at, cols) = (self.col + start, col - start);
                match open.iter().position(|p| p.col == at && p.cols == cols) {
                    Some(i) => {
                        let mut piece = open.swap_remove(i);
                        piece.rows += 1;
                        next.push(piece);
                    }
                    None => next.push(Tile {
                        col: at,
                        row: self.row + row,
                        cols,
                        rows: 1,
                        ..self
                    }),
                }
            }
            done.append(&mut open);
            open = next;
        }
        done.append(&mut open);
        done
    }
}

/// A tile's cells that hold text, one bit each ([`Tile::bit`]). Only SIXEL's
/// and iTerm2's tiles are carved, and they keep the protocol's [`TileShape`]
/// (`a_carved_tile_fits_its_bits`).
pub(crate) type Carve = u64;

/// A tile's pixels as the terminal shows them: upscaled, in whole cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TileImage {
    /// The tile it is.
    pub(crate) tile: Tile,
    /// Width in pixels.
    pub(crate) width: u32,
    /// Height in pixels.
    pub(crate) height: u32,
    /// Row-major 8-bit RGB.
    pub(crate) rgb: Vec<u8>,
}

/// A tile whose pixels, or the text cells carved out of it, differ from what
/// it was last [`sent`](Tiles::sent) with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Changed {
    /// The tile.
    pub(crate) tile: Tile,
    /// The cells left to text, which its image leaves alone.
    pub(crate) carve: Carve,
    hash: u64,
    /// The buffer size the tile was cut from.
    size: (u16, u16),
}

/// What a tile was last sent with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sent {
    hash: u64,
    carve: Carve,
}

/// The tile grid over one image and the hash each tile was last sent with.
#[derive(Debug)]
pub(crate) struct Tiles {
    protocol: ImageProtocol,
    /// The tile it sends in at `size`: [`Self::fitted`].
    shape: TileShape,
    cell: CellSize,
    upscale: u32,
    /// The buffer size `sent` describes.
    size: (u16, u16),
    /// Per tile, what it was last sent with; `None` until then.
    sent: Vec<Option<Sent>>,
    /// The tiles a frame reached that may still differ from what was sent: a
    /// frame whose bytes never reached the terminal leaves its tiles here.
    owed: BTreeSet<u32>,
}

impl Tiles {
    /// The grid `protocol` re-sends in, over an image `fit` lays on `cell`.
    pub(crate) fn new(protocol: ImageProtocol, cell: CellSize, fit: PixelFit) -> Self {
        Self {
            protocol,
            shape: protocol.tile(),
            cell,
            upscale: u32::from(fit.upscale()),
            size: (0, 0),
            sent: Vec::new(),
            owed: BTreeSet::new(),
        }
    }

    /// The tiles whose pixels differ from what was last [`sent`](Self::sent)
    /// for them, among those `dirty` and every frame since reached.
    pub(crate) fn changed(&mut self, buf: &RgbBuffer, dirty: &Dirty) -> Vec<Changed> {
        let size = (buf.width(), buf.height());
        if size != self.size {
            self.size = size;
            self.shape = self.fitted();
            let (across, down) = self.across_down();
            self.sent = vec![None; (across * down) as usize];
            self.owed = (0..self.sent.len() as u32).collect();
        }
        self.owe(dirty);
        let mut owed = std::mem::take(&mut self.owed);
        let mut changed = Vec::new();
        owed.retain(|&index| {
            let tile = self.tile(index);
            let hash = self.hash(buf, tile);
            let sent = self.sent[index as usize];
            let differs = sent.map(|s| s.hash) != Some(hash);
            if differs {
                changed.push(Changed {
                    tile,
                    carve: sent.map_or(0, |s| s.carve),
                    hash,
                    size,
                });
            }
            differs
        });
        self.owed = owed;
        changed
    }

    /// Owe the tiles `dirty` reaches, for a frame painted but not sent.
    pub(crate) fn owe(&mut self, dirty: &Dirty) {
        match dirty {
            Dirty::All => self.owed.extend(0..self.sent.len() as u32),
            Dirty::Rects(rects) => {
                for &r in rects.as_slice() {
                    self.owed.extend(self.reached(r));
                }
            }
            Dirty::Unchanged => {}
        }
    }

    /// Record `tiles` as on the terminal, once their bytes are written.
    pub(crate) fn sent(&mut self, tiles: &[Changed]) {
        for c in tiles.iter().filter(|c| c.size == self.size) {
            self.sent[c.tile.index as usize] = Some(Sent {
                hash: c.hash,
                carve: c.carve,
            });
            self.owed.remove(&c.tile.index);
        }
    }

    /// `changed`, each carved as `carves` (by tile index) says, then every
    /// other tile last sent with another carve than its own now: text that
    /// left a cell leaves it to the image again.
    pub(crate) fn carved(&self, changed: &[Changed], carves: &[Carve]) -> Vec<Changed> {
        let carve = |index: u32| carves[index as usize];
        let recarved = self.sent.iter().zip(0u32..).filter_map(|(sent, index)| {
            let sent = (*sent)?;
            let moved = sent.carve != carve(index);
            (moved && !changed.iter().any(|c| c.tile.index == index)).then(|| Changed {
                tile: self.tile(index),
                carve: carve(index),
                hash: sent.hash,
                size: self.size,
            })
        });
        changed
            .iter()
            .map(|&c| Changed {
                carve: carve(c.tile.index),
                ..c
            })
            .chain(recarved)
            .collect()
    }

    /// Every tile of the grid [`changed`](Self::changed) last cut.
    pub(crate) fn all(&self) -> impl Iterator<Item = Tile> + '_ {
        (0..self.sent.len() as u32).map(|index| self.tile(index))
    }

    /// Owe every tile, as if the terminal had dropped them all.
    pub(crate) fn forget(&mut self) {
        self.sent.fill(None);
        self.owed.extend(0..self.sent.len() as u32);
    }

    /// `tile`'s pixels from `buf`, upscaled. A cell the image only partly
    /// covers is filled out with the image's edge pixels, so every tile is
    /// whole cells and no terminal resamples it.
    pub(crate) fn image(&self, buf: &RgbBuffer, tile: Tile) -> TileImage {
        let (cw, ch) = (u32::from(self.cell.w), u32::from(self.cell.h));
        let (width, height) = (u32::from(tile.cols) * cw, u32::from(tile.rows) * ch);
        let src = |px, edge| self.source_px(px, edge);
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            let by = src(u32::from(tile.row) * ch + y, buf.height());
            for x in 0..width {
                let p = buf.get(src(u32::from(tile.col) * cw + x, buf.width()), by);
                rgb.extend_from_slice(&[p.r, p.g, p.b]);
            }
        }
        TileImage {
            tile,
            width,
            height,
            rgb,
        }
    }

    /// The buffer pixel image pixel `px` upscales, clamped to a buffer `edge`
    /// pixels long.
    fn source_px(&self, px: u32, edge: u16) -> u16 {
        u16::try_from(px / self.upscale).map_or(edge - 1, |b| b.min(edge - 1))
    }

    /// One tile's extent in image pixels on each axis.
    fn tile_px(&self, shape: TileShape) -> (u32, u32) {
        (
            u32::from(shape.cols) * u32::from(self.cell.w),
            u32::from(shape.rows) * u32::from(self.cell.h),
        )
    }

    fn image_px(&self) -> (u32, u32) {
        (
            u32::from(self.size.0) * self.upscale,
            u32::from(self.size.1) * self.upscale,
        )
    }

    /// The protocol's tile, doubled a side until a whole frame's tiles fit
    /// its [image budget](ImageProtocol::image_budget) or one tile is the
    /// whole image: the finest diff the terminal keeps up with.
    fn fitted(&self) -> TileShape {
        let mut shape = self.protocol.tile();
        let Some(budget) = self.protocol.image_budget() else {
            return shape;
        };
        loop {
            let (across, down) = self.grid(shape);
            if across * down <= budget || (across == 1 && down == 1) {
                return shape;
            }
            shape = TileShape {
                cols: shape.cols.saturating_mul(2),
                rows: shape.rows.saturating_mul(2),
            };
        }
    }

    /// The tile it sends in now.
    #[cfg(test)]
    pub(crate) fn shape(&self) -> TileShape {
        self.shape
    }

    fn across_down(&self) -> (u32, u32) {
        self.grid(self.shape)
    }

    /// The tiles across and down the image at `size` in `shape`.
    fn grid(&self, shape: TileShape) -> (u32, u32) {
        let ((w, h), (tw, th)) = (self.image_px(), self.tile_px(shape));
        (w.div_ceil(tw), h.div_ceil(th))
    }

    fn tile(&self, index: u32) -> Tile {
        let (across, _) = self.across_down();
        let (iw, ih) = self.image_px();
        let (cells_w, cells_h) = (
            iw.div_ceil(u32::from(self.cell.w)),
            ih.div_ceil(u32::from(self.cell.h)),
        );
        let (col, row) = (
            index % across * u32::from(self.shape.cols),
            index / across * u32::from(self.shape.rows),
        );
        let cells = |n: u32| {
            u16::try_from(n).expect("the image is at most u16::MAX px a side (cutaway_fit)")
        };
        Tile {
            index,
            col: cells(col),
            row: cells(row),
            cols: cells(u32::from(self.shape.cols).min(cells_w - col)),
            rows: cells(u32::from(self.shape.rows).min(cells_h - row)),
        }
    }

    /// The tiles a buffer-pixel rect overlaps.
    fn reached(&self, r: Bounds) -> impl Iterator<Item = u32> + use<> {
        let (across, down) = self.across_down();
        let (tw, th) = self.tile_px(self.shape);
        let k = self.upscale;
        let span = |start: u16, len: u16, tile: u32, limit: u32| {
            let (a, b) = (u32::from(start), u32::from(start) + u32::from(len));
            if len == 0 {
                0..0
            } else {
                (a * k / tile).min(limit)..(b * k).div_ceil(tile).min(limit)
            }
        };
        let (xs, ys) = (
            span(r.x, r.width, tw, across),
            span(r.y, r.height, th, down),
        );
        ys.flat_map(move |ty| xs.clone().map(move |tx| ty * across + tx))
    }

    /// The buffer pixels `tile`'s upscaled pixels come from, on one axis.
    fn source(&self, cell0: u16, cells: u16, cell_px: u16, image_px: u32) -> Range<usize> {
        let a = u32::from(cell0) * u32::from(cell_px);
        let b = (a + u32::from(cells) * u32::from(cell_px)).min(image_px);
        (a / self.upscale) as usize..b.div_ceil(self.upscale) as usize
    }

    fn hash(&self, buf: &RgbBuffer, tile: Tile) -> u64 {
        let (iw, ih) = self.image_px();
        let xs = self.source(tile.col, tile.cols, self.cell.w, iw);
        let pixels = buf.as_slice();
        // A collision would hold a changed tile back: the display list's
        // fingerprints take the quality hasher for the same reason.
        let mut h = foldhash::quality::FixedState::with_seed(0).build_hasher();
        for y in self.source(tile.row, tile.rows, self.cell.h, ih) {
            let row = y * usize::from(buf.width());
            Hash::hash_slice(&pixels[row + xs.start..row + xs.end], &mut h);
        }
        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_core::sprite::Rgb;
    use pixtuoid_core::sprite::format::Density;
    use ratatui::layout::Size as TermSize;

    const AREA: TermSize = TermSize {
        width: 200,
        height: 100,
    };

    /// A grid whose upscaled pixels straddle cells: a 5-px cell fits scale 6
    /// on 2x art, so each art pixel is 3 image pixels.
    fn misaligned() -> Tiles {
        let cell = CellSize { w: 5, h: 10 };
        let fit = crate::graphics::cutaway_fit(cell, AREA, Density::new(2).expect("nonzero"))
            .expect("fits");
        assert_eq!(fit.upscale(), 3);
        Tiles::new(ImageProtocol::Kitty, cell, fit)
    }

    fn buffer(w: u16, h: u16) -> RgbBuffer {
        let px = (0..u32::from(w) * u32::from(h))
            .map(|i| Rgb {
                r: i as u8,
                g: (i >> 8) as u8,
                b: 7,
            })
            .collect();
        RgbBuffer::from_pixels(w, h, px)
    }

    fn rect(x: u16, y: u16, width: u16, height: u16) -> Dirty {
        Dirty::within(vec![Bounds {
            x,
            y,
            width,
            height,
        }])
    }

    fn poke(buf: &mut RgbBuffer, x: u16, y: u16) {
        let p = buf.get(x, y);
        buf.put(x, y, Rgb { r: !p.r, ..p });
    }

    /// A frame whose bytes all reach the terminal.
    fn emit(t: &mut Tiles, buf: &RgbBuffer, dirty: &Dirty) -> Vec<Tile> {
        let changed = t.changed(buf, dirty);
        t.sent(&changed);
        changed.iter().map(|c| c.tile).collect()
    }

    fn indices(tiles: &[Tile]) -> Vec<u32> {
        tiles.iter().map(|t| t.index).collect()
    }

    /// 13x7 art px upscaled 3x is 39x21 image px: 8x3 cells of 5x10, the
    /// last column and row only partly covered, in kitty's 4x2-cell tiles.
    #[test]
    fn the_grid_covers_the_image_in_whole_cells_with_short_edge_tiles() {
        let mut t = misaligned();
        let all = emit(&mut t, &buffer(13, 7), &Dirty::All);
        let shape = |t: &Tile| (t.index, t.col, t.row, t.cols, t.rows);
        assert_eq!(
            all.iter().map(shape).collect::<Vec<_>>(),
            [
                (0, 0, 0, 4, 2),
                (1, 4, 0, 4, 2),
                (2, 0, 2, 4, 1),
                (3, 4, 2, 4, 1)
            ]
        );
        let mut t = misaligned();
        let all = emit(&mut t, &buffer(14, 7), &Dirty::All);
        assert_eq!(
            all.iter().map(shape).collect::<Vec<_>>()[2],
            (2, 8, 0, 1, 2),
            "42 image px is 9 cells: a third, one-cell column"
        );
    }

    /// The owner's 17x41 cell over 4x art, upscaled 4x, as a 16x office.
    fn owner_kitty(protocol: ImageProtocol) -> Tiles {
        let cell = CellSize { w: 17, h: 41 };
        let fit = crate::graphics::cutaway_fit(cell, AREA, Density::new(4).expect("nonzero"))
            .expect("fits");
        Tiles::new(protocol, cell, fit)
    }

    /// A whole frame never holds more tiles than kitty's image budget, at
    /// any office size, while a small one keeps the protocol's own tile: the
    /// finest diff that fits.
    #[test]
    fn kittys_tiles_coarsen_only_past_its_image_budget() {
        let budget = ImageProtocol::Kitty.image_budget().expect("kitty has one") as usize;
        for (w, h) in [
            (40, 20),
            (200, 100),
            (400, 200),
            (700, 350),
            (1000, 500),
            (1500, 800),
        ] {
            let mut t = owner_kitty(ImageProtocol::Kitty);
            let sent = t.changed(&buffer(w, h), &Dirty::All).len();
            assert!(sent <= budget, "{w}x{h}: {sent} tiles");
            if t.shape != ImageProtocol::Kitty.tile() {
                let finer = TileShape {
                    cols: t.shape.cols / 2,
                    rows: t.shape.rows / 2,
                };
                let (across, down) = t.grid(finer);
                let over = (across * down) as usize;
                assert!(
                    over > budget,
                    "{w}x{h}: {finer:?} gave {over}, within budget"
                );
            }
        }
        let mut small = owner_kitty(ImageProtocol::Kitty);
        small.changed(&buffer(40, 20), &Dirty::All);
        assert_eq!(small.shape, ImageProtocol::Kitty.tile());
        let mut big = owner_kitty(ImageProtocol::Kitty);
        big.changed(&buffer(1000, 500), &Dirty::All);
        assert_ne!(big.shape, ImageProtocol::Kitty.tile());
    }

    /// SIXEL and iTerm2 have no image budget: their tile is the protocol's at
    /// any size.
    #[test]
    fn sixel_and_iterm2_keep_their_tile_at_any_size() {
        for p in [ImageProtocol::Sixel, ImageProtocol::Iterm2] {
            assert_eq!(p.image_budget(), None);
            let mut t = owner_kitty(p);
            t.changed(&buffer(1000, 500), &Dirty::All);
            assert_eq!(t.shape, p.tile());
        }
    }

    /// A carved tile's cells fit a [`Carve`]'s bits at any size, since the
    /// carved protocols keep their tile.
    #[test]
    fn a_carved_tile_fits_its_bits() {
        for p in [ImageProtocol::Sixel, ImageProtocol::Iterm2] {
            let TileShape { cols, rows } = p.tile();
            assert!(u32::from(cols) * u32::from(rows) <= Carve::BITS, "{p:?}");
        }
    }

    /// A tile with a badge's run carved out of its second row draws the row
    /// above whole, the run's left and right, and the two rows below as one
    /// image; an uncarved tile is itself, and an all-text one draws nothing.
    #[test]
    fn a_carved_tile_draws_the_runs_of_cells_text_leaves() {
        let tile = Tile {
            index: 7,
            col: 8,
            row: 4,
            cols: 8,
            rows: 4,
        };
        let piece = |col, row, cols, rows| Tile {
            index: 7,
            col,
            row,
            cols,
            rows,
        };
        let badge = (2..5).fold(0, |carve, col| carve | tile.bit(col, 1));
        let mut pieces = tile.pieces(badge);
        pieces.sort_by_key(|p| (p.row, p.col));
        assert_eq!(
            pieces,
            [
                piece(8, 4, 8, 1),
                piece(8, 5, 2, 1),
                piece(13, 5, 3, 1),
                piece(8, 6, 8, 2),
            ]
        );
        assert_eq!(tile.pieces(0), [tile]);
        assert_eq!(tile.pieces(Carve::MAX >> (Carve::BITS - 32)), []);
    }

    #[test]
    fn the_tile_shape_is_the_protocols() {
        assert_eq!(ImageProtocol::Kitty.tile(), TileShape { cols: 4, rows: 2 });
        for p in [ImageProtocol::Sixel, ImageProtocol::Iterm2] {
            assert_eq!(p.tile(), TileShape { cols: 8, rows: 4 });
        }
    }

    /// An idle office emits nothing: a frame the canvas did not paint names
    /// no tile, whatever the buffer holds, and a repaint of the same pixels
    /// re-sends none.
    #[test]
    fn an_unchanged_frame_emits_no_tile() {
        let mut t = misaligned();
        let mut buf = buffer(13, 7);
        assert_eq!(emit(&mut t, &buf, &Dirty::All).len(), 4);
        assert_eq!(emit(&mut t, &buf, &Dirty::All), []);
        assert_eq!(emit(&mut t, &buf, &rect(0, 0, 13, 7)), []);
        poke(&mut buf, 0, 0);
        assert_eq!(emit(&mut t, &buf, &Dirty::Unchanged), []);
    }

    /// A rect re-sends the tiles it overlaps once their pixels changed, and
    /// only those: art column 6 is image px 18..21, which straddles kitty's
    /// 20-px tile edge.
    #[test]
    fn a_dirty_rect_reaches_exactly_the_tiles_it_overlaps() {
        let mut t = misaligned();
        let mut buf = buffer(13, 7);
        emit(&mut t, &buf, &Dirty::All);
        poke(&mut buf, 6, 0);
        poke(&mut buf, 12, 6);
        assert_eq!(indices(&emit(&mut t, &buf, &rect(6, 0, 1, 1))), [0, 1]);
        assert_eq!(indices(&emit(&mut t, &buf, &rect(0, 0, 6, 7))), [0u32; 0]);
        assert_eq!(indices(&emit(&mut t, &buf, &rect(12, 6, 0, 0))), [0u32; 0]);
        assert_eq!(indices(&emit(&mut t, &buf, &rect(12, 6, 1, 1))), [3]);
    }

    /// A tile stays changed until its bytes are written, through idle frames
    /// that reach nothing.
    #[test]
    fn a_tile_stays_changed_until_it_is_sent() {
        let mut t = misaligned();
        let mut buf = buffer(13, 7);
        emit(&mut t, &buf, &Dirty::All);
        poke(&mut buf, 0, 0);
        let unsent = t.changed(&buf, &rect(0, 0, 1, 1));
        assert_eq!(unsent.len(), 1);
        assert_eq!(t.changed(&buf, &Dirty::Unchanged), unsent);
        t.sent(&unsent);
        assert_eq!(t.changed(&buf, &Dirty::Unchanged), []);
    }

    /// A buffer of a new size is a new grid: every tile is re-sent, even when
    /// the frame names only a rect, and the old grid's tiles no longer count.
    #[test]
    fn a_new_buffer_size_re_sends_every_tile() {
        let mut t = misaligned();
        let old = t.changed(&buffer(13, 7), &Dirty::All);
        let new = t.changed(&buffer(14, 7), &rect(0, 0, 1, 1));
        assert_eq!(new.len(), 6);
        t.sent(&old);
        assert_eq!(t.changed(&buffer(14, 7), &Dirty::Unchanged), new);
    }

    /// The hash reads art pixels, before the upscale, which is k² fewer: it is
    /// exact because a tile changes on screen precisely when an art pixel it
    /// samples does.
    #[test]
    fn hashing_before_the_upscale_misses_no_change_on_screen() {
        let base = buffer(13, 7);
        for (x, y) in (0..13).flat_map(|x| (0..7).map(move |y| (x, y))) {
            let (mut t, mut buf) = (misaligned(), base.clone());
            let before: Vec<TileImage> = emit(&mut t, &buf, &Dirty::All)
                .into_iter()
                .map(|tile| t.image(&buf, tile))
                .collect();
            poke(&mut buf, x, y);
            let on_screen: Vec<u32> = before
                .iter()
                .filter(|was| t.image(&buf, was.tile) != **was)
                .map(|was| was.tile.index)
                .collect();
            assert_eq!(
                indices(&emit(&mut t, &buf, &Dirty::All)),
                on_screen,
                "({x},{y})"
            );
        }
    }

    /// Each image pixel is the art pixel it upscales; past the image's edge,
    /// the edge pixel.
    #[test]
    fn a_tile_image_is_whole_cells_of_upscaled_art() {
        let (mut t, buf) = (misaligned(), buffer(13, 7));
        let corner = emit(&mut t, &buf, &Dirty::All)[3];
        let img = t.image(&buf, corner);
        assert_eq!((img.width, img.height), (20, 10));
        assert_eq!(img.rgb.len(), 20 * 10 * 3);
        let at = |x: u32, y: u32| {
            let i = ((y * img.width + x) * 3) as usize;
            Rgb {
                r: img.rgb[i],
                g: img.rgb[i + 1],
                b: img.rgb[i + 2],
            }
        };
        // Image px (20, 20) is art (6, 6).
        assert_eq!(at(0, 0), buf.get(6, 6));
        assert_eq!(at(1, 0), buf.get(7, 6));
        assert_eq!(at(18, 0), buf.get(12, 6), "image px 38, the last");
        assert_eq!(at(19, 9), buf.get(12, 6), "past the edge");
    }
}
