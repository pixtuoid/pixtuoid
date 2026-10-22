//! The seam between LAYOUT space and BUFFER space.
//!
//! Were every layout coordinate a buffer pixel, the office's SIZE and its
//! RESOLUTION would be one axis: doubling the buffer would not draw the same
//! room sharper, it would build a room with several times the desks, and
//! "render the same office with more detail" would be unexpressible. Pinned by
//! `floor_capacity_is_invariant_under_render_scale` and its negative control
//! `without_the_seam_a_bigger_buffer_builds_a_bigger_office`.
//!
//! [`RenderScale`] splits the axis. Layout keeps computing in logical units, so
//! a floor's capacity, desk assignment and walkable mask are untouched; the
//! painter multiplies by the scale on its way to pixels. A richer visual
//! profile then buys detail per object rather than more objects.

use std::num::NonZeroU16;

use pixtuoid_core::sprite::format::Density;

/// How many buffer pixels one layout unit paints as.
///
/// `ONE` is the classic path: layout units ARE buffer pixels, byte-identical to
/// the pre-seam behaviour. A larger scale paints the same office into a bigger
/// buffer, which is the only thing that makes richer art expressible without
/// silently changing floor capacity or desk assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderScale(NonZeroU16);

impl From<Density> for RenderScale {
    /// The scale art drawn at `density` lands at as it is.
    fn from(density: Density) -> Self {
        Self(density.as_nonzero())
    }
}

impl RenderScale {
    /// The classic path — one layout unit per buffer pixel.
    pub(crate) const ONE: Self = Self(NonZeroU16::MIN);

    /// A scale of `n` buffer pixels per layout unit, or `None` for zero
    /// (a zero scale would divide the office away).
    pub fn new(n: u16) -> Option<Self> {
        NonZeroU16::new(n).map(Self)
    }

    /// The multiple of `density` nearest the surface's `natural` scale,
    /// measured by RATIO, or `None` when none lies within the ratio bound
    /// `FIT_MAX_RATIO_SQUARED` sets.
    ///
    /// Only at a multiple of a density does art drawn at it land: at a scale
    /// the density does not divide, those variants fall back to coarser art or
    /// the upscaled base. The multiple may lie ABOVE `natural`, so a painter
    /// must size the office from its surface's pixels ([`RenderScale::logical`]):
    /// rounding then changes how much office fits, never whether the art is
    /// resampled. Ratio, not difference, is the measure because framing is
    /// perceived multiplicatively: at a natural 12, 8 and 16 are equally far by
    /// difference, but 16 (×4/3) is nearer than 8 (×3/2).
    ///
    /// The rule meets HERE, in the engine, because the densities are the
    /// PACK's ([`Pack::density_variants`](pixtuoid_core::sprite::format::Pack::density_variants))
    /// and every painter that picks a scale
    /// needs it; a surface only contributes `natural`.
    ///
    /// A density of 1 — a pack with no variants — makes every scale a
    /// multiple, so this is exactly [`RenderScale::new`] there.
    pub(crate) fn fit(natural: u16, density: Density) -> Option<Self> {
        let nearest = Self::nearest_multiple(natural, density);
        // u64: a square of a u16-range value times the bound overflows u32.
        let (n, m) = (u64::from(natural), u64::from(nearest.get()));
        let (lo, hi) = (m.min(n), m.max(n));
        (hi * hi <= FIT_MAX_RATIO_SQUARED * lo * lo).then_some(Self(nearest))
    }

    /// The multiple of `density` nearest `natural` by ratio, the density
    /// itself at the least: the multiplier `k` is 0 only for a `natural` under
    /// the density, where the multiple above, the density, is the nearer, so
    /// the last line is `k.max(1)` as a `NonZeroU16`.
    fn nearest_multiple(natural: u16, density: Density) -> NonZeroU16 {
        let d = density.as_nonzero();
        let below = natural / d;
        // A multiple past `u16::MAX` is no scale at all, so it is no candidate.
        let above = below
            .checked_add(1)
            .filter(|k| k.checked_mul(d.get()).is_some());
        let (n, step) = (u64::from(natural), u64::from(d.get()));
        let k = match above {
            // `n/below < above/n` ⇔ `n² < below·above`, compared exactly in
            // integers; adjacent multiples `kd` and `(k+1)d` never tie, since
            // that needs `k(k+1)` to be a square.
            Some(a) if n * n >= u64::from(below) * step * u64::from(a) * step => a,
            _ => below,
        };
        d.saturating_mul(NonZeroU16::MIN.saturating_add(k.saturating_sub(1)))
    }

    /// Buffer pixels per layout unit.
    pub fn get(self) -> u16 {
        self.0.get()
    }

    /// The factor as a [`NonZeroU16`], for the core blitter.
    ///
    /// `pixtuoid-core` owns the pixel primitives but sits BELOW this crate in
    /// the DAG, so it cannot name `RenderScale`; it takes the bare non-zero
    /// factor, which this module and the density-variant picker make. Core gets
    /// "repeat each pixel N times", the layout↔buffer meaning stays here.
    pub(crate) fn factor(self) -> NonZeroU16 {
        self.0
    }

    /// The logical extent a buffer of `buf_px` covers. Truncating is deliberate:
    /// a buffer that is not a whole multiple of the scale leaves a sub-unit
    /// remainder that no layout unit could occupy anyway.
    pub fn logical(self, buf_px: u16) -> u16 {
        buf_px / self.0.get()
    }

    /// The buffer pixel a layout unit paints at. Saturates rather than wrapping
    /// so an oversized layout clips at the buffer edge (`blit_frame` already
    /// discards out-of-bounds pixels) instead of aliasing to the top-left.
    pub fn to_buffer(self, logical: u16) -> u16 {
        logical.saturating_mul(self.0.get())
    }
}

/// The square of how far, as a ratio, [`RenderScale::fit`] may move a scale
/// off its natural value — squared so the comparison stays in integers.
///
/// It only bites where `natural` is below the density, whose own value is then
/// the one candidate (above it, the nearest multiple always lies within √2): a
/// ratio of √2 is a factor of 2 in AREA, so past it the office would keep
/// under half the logical area the surface's natural scale gives it, and the
/// classic profile draws more office than that.
const FIT_MAX_RATIO_SQUARED: u64 = 2;

impl Default for RenderScale {
    fn default() -> Self {
        Self::ONE
    }
}

/// A cutaway on a surface of real pixels, whatever the painter: the office
/// renders at the pack's densest art and the image is that render upscaled a
/// whole number of times to [`Self::scale`], so every art pixel lands as an
/// equal square (integer scaling, as pixel-art engines draw:
/// <https://docs.godotengine.org/en/stable/tutorials/rendering/multiple_resolutions.html>),
/// pixel-identical to a render at the scale itself (pinned by
/// `the_cutaway_paints_whole_art_pixels`). The office takes the surface's
/// shape; the sub-unit remainder is the painter's margin.
///
/// The densest art, not whichever density the surface lands nearest: the one
/// render draws every piece at one density, and a scale only a coarser density
/// divides would draw the densest art from coarser stand-ins.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFit {
    scale: RenderScale,
    density: Density,
    logical: crate::layout::Size,
}

impl PixelFit {
    /// The surface's `natural` real pixels per unit fitted to `density` (the
    /// pack's [`max_density_variant`](crate::pack::OfficeArt::max_density_variant))
    /// over a surface `px` big; `None` when [`RenderScale::fit`] finds no
    /// scale.
    pub fn new(natural: u16, density: Density, px: crate::layout::Size) -> Option<Self> {
        RenderScale::fit(natural, density).map(|scale| Self::at(scale, density, px))
    }

    /// [`Self::new`] for a surface that has no other look to fall back to: a
    /// `natural` below `density` takes the density itself, so the office
    /// shrinks rather than its art coarsening.
    pub fn at_least_density(natural: u16, density: Density, px: crate::layout::Size) -> Self {
        let scale = RenderScale::nearest_multiple(natural.max(density.get()), density);
        Self::at(RenderScale(scale), density, px)
    }

    fn at(scale: RenderScale, density: Density, px: crate::layout::Size) -> Self {
        Self {
            scale,
            density,
            logical: crate::layout::Size { w: 0, h: 0 },
        }
        .over(px)
    }

    /// This fit over a surface `px` big: the scale stays, and the office takes
    /// as many units as the surface holds on each axis.
    #[must_use]
    pub fn over(self, px: crate::layout::Size) -> Self {
        Self {
            logical: crate::layout::Size {
                w: self.scale.logical(px.w),
                h: self.scale.logical(px.h),
            },
            ..self
        }
    }

    /// Real pixels per logical unit.
    pub fn scale(self) -> RenderScale {
        self.scale
    }

    /// The density the office renders at before the upscale.
    pub fn density(self) -> Density {
        self.density
    }

    /// The look a floor renders in under this fit: the cutaway at
    /// [`Self::density`], its world text set by `text`.
    pub fn look(self, text: crate::look::WorldText) -> crate::look::Look {
        crate::look::Look::Cutaway {
            scale: RenderScale::from(self.density),
            text,
        }
    }

    /// The whole factor the density render is upscaled by.
    pub fn upscale(self) -> u16 {
        self.scale.get() / self.density.get()
    }

    /// The office's extent in logical units.
    pub fn logical(self) -> crate::layout::Size {
        self.logical
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::pen::test_density;
    use crate::floor::{floor_capacity, floor_capacity_scaled, floor_seed};

    fn fit(natural: u16, density: u16) -> Option<u16> {
        RenderScale::fit(natural, test_density(density)).map(RenderScale::get)
    }

    /// A surface with no other look renders at the nearest multiple of the
    /// density to its natural, the density at the least, for every natural:
    /// [`RenderScale::fit`] of a natural at least the density never refuses.
    #[test]
    fn a_fit_at_least_the_density_always_lands() {
        let px = crate::layout::Size { w: 640, h: 480 };
        for d in 1..=8 {
            let density = test_density(d);
            for natural in (0..=u16::MAX).step_by(7).chain([u16::MAX]) {
                let got = PixelFit::at_least_density(natural, density, px)
                    .scale()
                    .get();
                assert_eq!(
                    Some(got),
                    fit(natural.max(d), d),
                    "natural {natural}, density {d}"
                );
            }
            // The nearest multiple by ratio: at 4, 9 → 8 (9/8 < 12/9) and 10 → 12.
            if d == 4 {
                let at = |n| PixelFit::at_least_density(n, density, px).scale().get();
                assert_eq!([at(5), at(6), at(9), at(10)], [4, 8, 8, 12]);
            }
            // Below the density the office shrinks; the art keeps its density.
            let below = PixelFit::at_least_density(d - 1, density, px);
            assert_eq!(
                (below.scale().get(), below.upscale(), below.logical()),
                (
                    d,
                    1,
                    crate::layout::Size {
                        w: px.w / d,
                        h: px.h / d
                    }
                ),
                "density {d}"
            );
        }
    }

    /// At a density of 8, natural scale by natural scale: every pick is a
    /// multiple of 8, rounding goes whichever way is nearer by ratio, and a
    /// natural scale too small to reach 8 within the bound gets no scale.
    #[test]
    fn a_scale_snaps_to_the_multiple_of_the_densest_art_nearest_by_ratio() {
        let want: [(u16, Option<u16>); 12] = [
            (5, None),
            (6, Some(8)),
            (8, Some(8)),
            (11, Some(8)),
            (12, Some(16)),
            (15, Some(16)),
            (17, Some(16)),
            (19, Some(16)),
            (20, Some(24)),
            (27, Some(24)),
            (28, Some(32)),
            (41, Some(40)),
        ];
        for (natural, scale) in want {
            assert_eq!(fit(natural, 8), scale, "natural {natural}");
        }
    }

    /// The properties the table samples, over natural scales to 256 and every
    /// density a pack may claim (core's `MAX_DENSITY_VARIANT`).
    #[test]
    fn every_pick_is_a_multiple_within_the_ratio_bound_and_the_nearest_one() {
        for d in 1..=64u16 {
            for n in 0..=256u16 {
                let Some(s) = fit(n, d) else {
                    // Only a scale with no multiple of `d` in reach goes without.
                    assert!(
                        u32::from(d) * u32::from(d) > 2 * u32::from(n) * u32::from(n),
                        "natural {n} at density {d} has a multiple in reach"
                    );
                    continue;
                };
                assert_eq!(s % d, 0, "natural {n} at density {d} picked {s}");
                let ratio = |a: u16| f64::from(a.max(n)) / f64::from(a.min(n));
                assert!(ratio(s) <= std::f64::consts::SQRT_2, "{n}@{d} → {s}");
                for other in [s.saturating_sub(d), s + d].into_iter().filter(|&o| o > 0) {
                    assert!(
                        ratio(s) < ratio(other),
                        "{n}@{d}: {other} is nearer than {s}"
                    );
                }
            }
        }
    }

    /// The far end of the range: the multiple above `u16::MAX` is no scale, so
    /// the one below it wins, and the arithmetic never overflows on the way.
    #[test]
    fn the_largest_natural_scales_pick_a_representable_multiple() {
        assert_eq!(fit(u16::MAX, 8), Some(u16::MAX / 8 * 8));
        assert_eq!(fit(u16::MAX, 1), Some(u16::MAX));
        assert_eq!(fit(u16::MAX - 3, 16), Some((u16::MAX - 3) / 16 * 16));
    }

    /// A pack with no variants must be untouched: every scale is a multiple of
    /// 1, so the natural scale comes back as it went in.
    #[test]
    fn a_pack_without_variants_keeps_the_natural_scale() {
        for n in 1..=64 {
            assert_eq!(fit(n, 1), Some(n));
        }
        assert_eq!(fit(0, 1), None, "zero pixels is still no scale");
    }

    #[test]
    fn zero_is_refused_because_it_would_divide_the_office_away() {
        assert!(RenderScale::new(0).is_none());
        assert_eq!(RenderScale::ONE.get(), 1);
    }

    #[test]
    fn logical_and_buffer_round_trip_on_whole_multiples() {
        let s = RenderScale::new(4).expect("4 is nonzero");
        assert_eq!(s.to_buffer(160), 640);
        assert_eq!(s.logical(640), 160);
        // A partial unit at the edge belongs to no layout unit.
        assert_eq!(s.logical(643), 160);
    }

    /// The seam reaches the core blitter unchanged — a scale that says "4" must
    /// expand a sprite by 4, or layout and paint would disagree about what a
    /// unit is while every test above still passed.
    #[test]
    fn the_factor_drives_the_core_blitter_at_the_same_scale() {
        use pixtuoid_core::sprite::blit::blit_frame_scaled;
        use pixtuoid_core::sprite::{Frame, Rgb, RgbBuffer};

        let red = Rgb { r: 255, g: 0, b: 0 };
        let bg = Rgb { r: 0, g: 0, b: 0 };
        let s = RenderScale::new(4).expect("nonzero");
        let f = Frame::from_pixels(1, 1, vec![Some(red)]);
        let mut buf = RgbBuffer::filled(8, 8, bg);
        blit_frame_scaled(&f, 0, 0, s.factor(), &mut buf);

        // One source pixel covers exactly s.get() x s.get() buffer pixels.
        let last = s.get() - 1;
        assert_eq!(buf.get(last, last), red, "the block reaches its far corner");
        assert_eq!(buf.get(s.get(), 0), bg, "and stops there");
    }

    #[test]
    fn to_buffer_saturates_instead_of_wrapping() {
        let s = RenderScale::new(8).expect("8 is nonzero");
        assert_eq!(s.to_buffer(u16::MAX), u16::MAX);
    }

    /// The invariant the whole seam exists for: raising render fidelity must
    /// not change the office.
    #[test]
    fn floor_capacity_is_invariant_under_render_scale() {
        let seed = floor_seed(0);
        let base = floor_capacity_scaled(192, 160, RenderScale::ONE, seed);
        assert!(base > 0, "the 1x baseline must lay out at all");

        for n in [2u16, 4, 8] {
            let s = RenderScale::new(n).expect("nonzero");
            let got = floor_capacity_scaled(s.to_buffer(192), s.to_buffer(160), s, seed);
            assert_eq!(got, base, "render scale {n} changed the desk count");
        }
    }

    /// Negative control for the test above — it must be able to FAIL. Without
    /// the seam the same buffers yield a wildly bigger office, which is exactly
    /// the regression the invariant pins.
    #[test]
    fn without_the_seam_a_bigger_buffer_builds_a_bigger_office() {
        let seed = floor_seed(0);
        let base = floor_capacity(192, 160, seed);
        let unscaled = floor_capacity(192 * 8, 160 * 8, seed);
        assert!(
            unscaled > base * 8,
            "expected the un-seamed call to balloon the desk count \
             (base {base}, 8x buffer {unscaled}) — if this ever stops holding, \
             the invariant test above has lost its teeth"
        );
    }
}
