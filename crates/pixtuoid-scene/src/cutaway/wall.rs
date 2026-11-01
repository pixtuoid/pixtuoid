//! The room walls: each of the layout's
//! [`wall_pieces`](crate::layout::SceneLayout::wall_pieces), sorted in its
//! [bands](WallPiece::sort_bands) and painted through the [`Glass`] by
//! both painters, each on its own [`Pen`].

use std::ops::Range;

use crate::sprite::RgbBuffer;

use crate::display::pen::{ArtRect, Pen};
use crate::glass::{Glass, WallTrim};
use crate::layout::WallPiece;

/// Paint the logical `rows` of one room wall: its glass over what is already
/// drawn behind them, then the jamb posts where a doorway cuts it. The glass is
/// laid from the wall's own top, so its rhythm runs on across the bands.
pub(crate) fn paint_wall(
    buf: &mut RgbBuffer,
    trim: WallTrim,
    piece: WallPiece,
    rows: Range<u16>,
    pen: Pen,
) {
    let (at, size) = piece.visual();
    let mut glass = Glass::of(trim, piece, pen.density());
    let band = ArtRect {
        x: pen.art(at.x),
        y: pen.art(rows.start),
        w: pen.art(size.w),
        h: pen.art(rows.end - rows.start),
    };
    let top = pen.art(rows.start - at.y).0;
    pen.recolour(buf, band, |dx, dy, under| glass.over(under, dx, top + dy));
    for (post, s) in piece.jambs() {
        let (from, to) = (post.y.max(rows.start), (post.y + s.h).min(rows.end));
        if from < to {
            let r = ArtRect {
                x: pen.art(post.x),
                y: pen.art(from),
                w: pen.art(s.w),
                h: pen.art(to - from),
            };
            pen.fill(buf, r, trim.dark);
        }
    }
}
