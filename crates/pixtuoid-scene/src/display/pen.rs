//! The art pixel, the grid every look places and paints on: sprites are
//! authored `d` art pixels per logical unit, and a render at scale `s` blits
//! each `s / d` buffer pixels square, so a frame mixes no pixel sizes. Painting
//! through a [`Pen`] holds that, because a pen can't address a buffer pixel.

use std::num::NonZeroU16;

use pixtuoid_core::sprite::format::Density;

use crate::pack::OfficeArt;

use crate::render_scale::RenderScale;

/// A length or coordinate on the art grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ArtPx(pub(crate) u16);

/// Where art pixel `a`'s centre lies on the layout's units at `d` art pixels to
/// the unit, with a unit's own centre on its integer: at one to the unit, the
/// unit itself, as the classic samples it. The continuous twin of
/// [`Pen::logical`].
pub(crate) fn layout_point(a: ArtPx, d: Density) -> f32 {
    (f32::from(a.0) + 0.5) / f32::from(d.get()) - 0.5
}

/// A density literal for a test.
#[cfg(test)]
pub(crate) const fn test_density(n: u16) -> Density {
    Density::new(n).expect("a test density is nonzero")
}

/// A length or coordinate in a render's buffer pixels, which a [`Pen`] turns
/// an [`ArtPx`] into and back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BufferPx(pub(crate) u16);

/// A rect on the art grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArtRect {
    pub(crate) x: ArtPx,
    pub(crate) y: ArtPx,
    pub(crate) w: ArtPx,
    pub(crate) h: ArtPx,
}

/// A render's art grid (the module doc): `k` buffer pixels make one
/// art pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Pen {
    d: Density,
    k: NonZeroU16,
}

impl Pen {
    /// The classic painter's grid: one buffer pixel per logical unit.
    pub(crate) const UNIT: Self = Self {
        d: Density::ONE,
        k: NonZeroU16::MIN,
    };

    /// The pen for art authored at density `d`, painted at `scale`; `None` when
    /// `d` does not divide it, since an art pixel would then straddle buffer
    /// pixels.
    pub(crate) fn new(scale: RenderScale, d: Density) -> Option<Self> {
        let s = scale.get();
        if !s.is_multiple_of(d.get()) {
            return None;
        }
        Some(Self {
            d,
            k: NonZeroU16::new(s / d.get())?,
        })
    }

    /// The pen for `pack` at `scale`: the densest of its variant densities that
    /// divides `scale`, else the base art's. [`densest_frame`](crate::pack::densest_frame)
    /// applies the same rule per piece, so the room shares every piece's grid
    /// only while the pack draws its variants at one density
    /// (`the_bundled_pack_draws_every_variant_at_one_density`).
    pub(crate) fn for_pack(scale: RenderScale, pack: &OfficeArt) -> Self {
        pack.density_variants()
            .iter()
            .find_map(|&d| Self::new(scale, d))
            .unwrap_or(Self {
                d: Density::ONE,
                k: scale.factor(),
            })
    }

    /// How many art pixels make one layout unit.
    pub(crate) fn density(self) -> Density {
        self.d
    }

    /// `logical` layout units, as art pixels: the one conversion from the
    /// layout's units onto the grid.
    pub(crate) fn art(self, logical: u16) -> ArtPx {
        ArtPx(logical.saturating_mul(self.d.get()))
    }

    /// The logical unit art pixel `a` lies in.
    pub(crate) fn logical(self, a: ArtPx) -> u16 {
        a.0 / self.d.get()
    }

    /// The art pixel buffer pixel `b` lies in.
    pub(crate) fn art_of_buffer(self, b: BufferPx) -> ArtPx {
        ArtPx(b.0 / self.k.get())
    }

    /// How many art columns it takes to cover `b` buffer pixels: the last one
    /// partly.
    pub(crate) fn art_covering(self, b: BufferPx) -> ArtPx {
        ArtPx(b.0.div_ceil(self.k.get()))
    }

    /// `a` art pixels, as buffer pixels.
    pub(crate) fn buffer(self, a: ArtPx) -> BufferPx {
        BufferPx(a.0.saturating_mul(self.k.get()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pen_reports_the_density_it_was_built_at() {
        let scale = RenderScale::new(4).expect("a scale");
        assert_eq!(
            Pen::new(scale, test_density(4)).map(Pen::density),
            Some(test_density(4))
        );
        assert_eq!(Pen::UNIT.density(), test_density(1));
    }

    #[test]
    fn the_bundled_pack_draws_every_variant_at_one_density() {
        let pack = crate::pack::test_office();
        assert_eq!(
            pack.density_variants().len(),
            1,
            "{:?}",
            pack.density_variants()
        );
    }

    fn pen(s: u16, d: u16) -> Pen {
        Pen::new(RenderScale::new(s).expect("nonzero"), test_density(d)).expect("d divides s")
    }

    #[test]
    fn a_pen_needs_its_density_to_divide_the_scale() {
        let s = RenderScale::new(8).expect("nonzero");
        assert_eq!(Pen::new(s, test_density(4)).map(|p| p.k.get()), Some(2));
        assert_eq!(
            Pen::new(s, test_density(3)),
            None,
            "an art pixel would straddle buffer pixels"
        );
    }

    /// Every buffer pixel an art pixel is painted on lies in it, the next one
    /// does not, and covering one more buffer pixel takes one more art column.
    #[test]
    fn a_buffer_pixel_lies_in_the_art_pixel_painted_on_it() {
        for (s, d) in [(1, 1), (8, 4), (12, 4)] {
            let pen = pen(s, d);
            let k = s / d;
            for a in 0..20 {
                let b = pen.buffer(ArtPx(a));
                for dk in 0..k {
                    assert_eq!(
                        pen.art_of_buffer(BufferPx(b.0 + dk)),
                        ArtPx(a),
                        "s {s} d {d}"
                    );
                }
                assert_eq!(
                    pen.art_of_buffer(BufferPx(b.0 + k)),
                    ArtPx(a + 1),
                    "s {s} d {d}"
                );
                assert_eq!(pen.art_covering(b), ArtPx(a), "s {s} d {d}");
                assert_eq!(
                    pen.art_covering(BufferPx(b.0 + 1)),
                    ArtPx(a + 1),
                    "s {s} d {d}"
                );
            }
        }
    }

    #[test]
    fn a_pens_density_is_the_densest_the_pack_draws_at_that_scale() {
        let pack = crate::pack::test_office();
        let d = pack.max_density_variant().get();
        let at = |s: u16| Pen::for_pack(RenderScale::new(s).expect("nonzero"), &pack);
        assert_eq!(at(d * 2), pen(d * 2, d), "the variant's grid");
        assert_eq!(at(1), pen(1, 1), "no variant fits: the base art's grid");
    }
}
