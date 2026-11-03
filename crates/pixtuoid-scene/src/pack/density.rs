//! The density-variant picker: [`densest`] chooses which art a painter at a
//! render scale draws a piece from — the base art, or a `<name>@<N>x` variant
//! of it — and how much upscale is left to blit it at.

use std::collections::BTreeMap;
use std::num::NonZeroU16;

use crate::pack::OfficeArt;
use crate::pack::Piece;
use crate::sprite::format::Density;
use crate::sprite::{Frame, RecolorableFrame, Sprite};

use crate::render_scale::RenderScale;

/// One frame of art as a renderer at some [`RenderScale`] should draw it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DenseFrame<'a> {
    /// The art in the pack's own colours.
    pub(crate) frame: &'a Frame,
    /// The same art as palette indices, for a per-agent recolor.
    pub(crate) recolorable: RecolorableFrame<'a>,
    /// The piece's size in layout units: its base frame's, since a variant
    /// redraws that frame's box on a finer grid.
    pub(crate) logical: (u16, u16),
    /// The grid the art is authored on: 1 for the base, `N` for `<name>@<N>x`.
    pub(crate) density: Density,
    /// The factor still left to blit at: the scale divided by `density`.
    pub(crate) blit_at: NonZeroU16,
    /// Where the frame's head is, if the art marks it: where a hairstyle
    /// dresses it ([`crate::character::dress_for`]).
    pub(crate) head: Option<crate::sprite::HeadMark>,
}

/// The densest of `variants` whose density divides `scale`, else `base`, with
/// the density it is drawn at and the upscale left to blit it at.
///
/// A variant is the same PIECE on an N-times grid — the base's logical box
/// drawn finer — so it is taken at any scale its density divides: 4x art at
/// 8x blits at 2 rather than being discarded for not matching. A piece with
/// no variant at a scale renders its base, so richer art lands one piece at a
/// time rather than as a flag day.
pub(crate) fn densest<'a, T>(
    base: &'a T,
    variants: &'a BTreeMap<Density, T>,
    scale: RenderScale,
) -> (&'a T, Density, NonZeroU16) {
    let s = scale.get();
    variants
        .iter()
        .rev()
        .filter(|(d, _)| s.is_multiple_of(d.get()))
        .find_map(|(&d, art)| Some((art, d, NonZeroU16::new(s / d.get())?)))
        .unwrap_or((base, Density::ONE, scale.factor()))
}

/// Frame `frame_idx` of `piece`, [wrapped](Sprite::wrap) as the classic
/// painter wraps it, from its [`densest`] art at `scale`.
pub(crate) fn densest_frame(
    pack: &OfficeArt,
    piece: Piece,
    frame_idx: usize,
    scale: RenderScale,
) -> DenseFrame<'_> {
    let base = pack.piece(piece);
    let (art, density, blit_at) = densest(base, pack.inner().variants_of(piece), scale);
    DenseFrame::of(base, art, frame_idx, (density, blit_at))
}

impl<'a> DenseFrame<'a> {
    /// Frame `idx` of `art`, which redraws `base` at `density`: a variant has
    /// its base's frame count, so `base` wraps the index for both.
    pub(crate) fn of(
        base: &Sprite,
        art: &'a Sprite,
        idx: usize,
        (density, blit_at): (Density, NonZeroU16),
    ) -> Self {
        let idx = base.wrap(idx);
        let logical = base.frame_at(idx);
        DenseFrame {
            frame: art.frame_at(idx),
            recolorable: art.recolorable_at(idx),
            logical: (logical.width(), logical.height()),
            density,
            blit_at,
            head: art.head(idx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `typing` has a variant at 2x, `holding_coffee` at both 2x and 4x; `desk`
    /// has none.
    fn variant_pack() -> OfficeArt {
        crate::pack::test_office_with(
            "[palette]\n\"A\"=\"#010203\"\n\"B\"=\"#040506\"\n\
             [animations.typing]\nframes=[\"a1.sprite\", \"b1.sprite\"]\nframe_ms=100\n\
             [animations.\"typing@2x\"]\nframes=[\"a2.sprite\", \"b2.sprite\"]\nframe_ms=100\n\
             [animations.holding_coffee]\nframes=[\"a1.sprite\"]\nframe_ms=100\n\
             [animations.\"holding_coffee@2x\"]\nframes=[\"a2.sprite\"]\nframe_ms=100\n\
             [animations.\"holding_coffee@4x\"]\nframes=[\"a4.sprite\"]\nframe_ms=100\n",
            &[
                ("a1.sprite", "@frame 0\nA"),
                ("b1.sprite", "@frame 0\nB"),
                ("a2.sprite", "@frame 0\nA A\nA A"),
                ("b2.sprite", "@frame 0\nB B\nB B"),
                ("a4.sprite", "@frame 0\nA A A A\nA A A A\nA A A A\nA A A A"),
            ],
        )
    }

    /// (width, density, blit_at) of `name`'s frame `idx` at scale `s`.
    fn at(pack: &OfficeArt, name: Piece, idx: usize, s: u16) -> (u16, u16, u16) {
        let d = densest_frame(pack, name, idx, RenderScale::new(s).expect("nonzero"));
        (d.frame.width(), d.density.get(), d.blit_at.get())
    }

    #[test]
    fn the_densest_dividing_variant_wins_and_blits_the_remainder() {
        let pack = variant_pack();
        assert_eq!(
            at(&pack, Piece::Typing, 0, 4),
            (2, 2, 2),
            "no 4x, so 4 falls to 2"
        );
        assert_eq!(at(&pack, Piece::Typing, 0, 2), (2, 2, 1));
        assert_eq!(
            at(&pack, Piece::Typing, 0, 3),
            (1, 1, 3),
            "2 does not divide 3"
        );
        assert_eq!(at(&pack, Piece::Typing, 0, 1), (1, 1, 1));
    }

    /// Two variants that both divide the scale: the DENSER one wins, and
    /// blits whatever upscale is left.
    #[test]
    fn the_denser_of_two_dividing_variants_wins() {
        let pack = variant_pack();
        assert_eq!(at(&pack, Piece::HoldingCoffee, 0, 4), (4, 4, 1));
        assert_eq!(at(&pack, Piece::HoldingCoffee, 0, 8), (4, 4, 2));
        assert_eq!(
            at(&pack, Piece::HoldingCoffee, 0, 6),
            (2, 2, 3),
            "4 does not divide 6"
        );
    }

    /// The layout size is the base's at every density.
    #[test]
    fn a_variant_keeps_its_bases_logical_size() {
        let pack = variant_pack();
        for s in [1, 2, 4, 8] {
            let d = densest_frame(
                &pack,
                Piece::HoldingCoffee,
                0,
                RenderScale::new(s).expect("nonzero"),
            );
            assert_eq!(d.logical, (1, 1), "scale {s}");
        }
    }

    /// A variant is indexed frame for frame, so frame 1 of the base must come
    /// out as frame 1 of the variant, not whatever the variant starts on.
    #[test]
    fn a_variant_is_read_at_the_same_frame_as_its_base() {
        let pack = variant_pack();
        let b = crate::sprite::Rgb { r: 4, g: 5, b: 6 };
        let d = densest_frame(
            &pack,
            Piece::Typing,
            1,
            RenderScale::new(2).expect("nonzero"),
        );
        assert_eq!(d.density.get(), 2);
        assert_eq!(d.frame.get(0, 0).copied().flatten(), Some(b));
        assert_eq!(
            d.recolorable.recolored(&[]).get(0, 0).copied().flatten(),
            Some(b)
        );
    }

    /// Past the end reads the first frame, like the classic painter.
    #[test]
    fn a_frame_past_the_end_reads_the_first() {
        let pack = variant_pack();
        let a = crate::sprite::Rgb { r: 1, g: 2, b: 3 };
        let d = densest_frame(
            &pack,
            Piece::Typing,
            9,
            RenderScale::new(2).expect("nonzero"),
        );
        assert_eq!(
            (d.density.get(), d.frame.get(0, 0).copied().flatten()),
            (2, Some(a))
        );
    }

    /// The flip side of one piece at a time: a piece with no variant draws its
    /// base while another's lands at the same scale, or adding one `@Nx` sprite
    /// would be a flag day for all of them.
    #[test]
    fn a_piece_with_no_variant_draws_its_base_while_anothers_lands() {
        let pack = variant_pack();
        assert_eq!(at(&pack, Piece::HoldingCoffee, 0, 4), (4, 4, 1));
        let desk_w = pack.piece(Piece::Desk).first().width();
        assert_eq!(at(&pack, Piece::Desk, 0, 4), (desk_w, 1, 4));
    }

    /// The same frames in the same order, at the same density, as a painter
    /// at that scale draws.
    #[test]
    fn frames_at_hands_out_what_a_painter_draws() {
        use strum::VariantArray as _;
        for pack in [variant_pack(), crate::pack::test_office()] {
            let max = pack.max_density_variant().get();
            // past the densest, so a variant blits a remainder and a non-divisor falls back
            for s in 1..=2 * max {
                let scale = RenderScale::new(s).expect("nonzero");
                for &piece in Piece::VARIANTS {
                    let (frames, density) = pack.frames_at(piece, scale);
                    assert_eq!(
                        frames.len(),
                        pack.piece(piece).frames().len(),
                        "{piece:?} at {s}"
                    );
                    for (i, f) in frames.enumerate() {
                        let painted = densest_frame(&pack, piece, i, scale);
                        assert!(std::ptr::eq(f, painted.frame), "{piece:?} {i} at {s}");
                        assert_eq!(density, painted.density, "{piece:?} at {s}");
                    }
                }
            }
        }
    }
}
