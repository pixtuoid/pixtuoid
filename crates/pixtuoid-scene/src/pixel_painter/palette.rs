//! The color math the classic painter's passes share.

pub(super) use crate::composite::{BLACK, WHITE, blend_rgb};
use pixtuoid_core::sprite::{Frame, Rgb, RgbBuffer};

/// Map one mascot pixel to its "degraded" look: a gateway that is UP but whose
/// model backend fails every run must read as UNWELL.
pub(super) fn degraded_pixel(c: Rgb) -> Rgb {
    // By eye: unwell, but not so grey that the dull-red bias below stops showing.
    const SATURATION_DRAIN: f32 = 0.55;
    let lum = (f32::from(c.r) * 0.30 + f32::from(c.g) * 0.59 + f32::from(c.b) * 0.11) as u8;
    let gray = Rgb {
        r: lum,
        g: lum,
        b: lum,
    };
    let desat = blend_rgb(c, gray, SATURATION_DRAIN);
    let sick = Rgb {
        r: 150,
        g: 40,
        b: 40,
    };
    let tinted = blend_rgb(desat, sick, 0.45);
    blend_rgb(
        tinted, BLACK, 0.18, // dim: the mascot looks drained
    )
}

/// A degraded copy of a mascot frame — every opaque pixel through
/// [`degraded_pixel`], transparency preserved.
pub(crate) fn degraded_frame(frame: &Frame) -> Frame {
    let pixels = frame
        .as_slice()
        .iter()
        .map(|&p| p.map(degraded_pixel))
        .collect();
    Frame::from_pixels(frame.width(), frame.height(), pixels)
}

/// A pixel transform tabulated over the diagonal greys — byte-identical to
/// calling `f` per pixel, but three L1 loads instead of the f32 chain, ONLY
/// for channel-separable `f` (every constant-tint [`blend`](crate::composite::blend) chain is; a
/// transform where one output channel reads another input channel tabulates
/// wrong). Amortizes when a pass touches ≫256 pixels.
pub(super) struct RgbLut {
    r: [u8; 256],
    g: [u8; 256],
    b: [u8; 256],
}

impl RgbLut {
    pub(super) fn tabulate(f: impl Fn(Rgb) -> Rgb) -> Self {
        let mut lut = RgbLut {
            r: [0; 256],
            g: [0; 256],
            b: [0; 256],
        };
        for i in 0..256 {
            let v = i as u8;
            let o = f(Rgb { r: v, g: v, b: v });
            lut.r[i] = o.r;
            lut.g[i] = o.g;
            lut.b[i] = o.b;
        }
        lut
    }

    #[inline]
    pub(super) fn apply(&self, c: Rgb) -> Rgb {
        Rgb {
            r: self.r[c.r as usize],
            g: self.g[c.g as usize],
            b: self.b[c.b as usize],
        }
    }
}

/// Composite `tint` over the existing buffer pixel at `(x, y)` by `t` — the
/// haze / overlay primitive.
pub(super) fn blend_over(buf: &RgbBuffer, x: u16, y: u16, tint: Rgb, t: f32) -> Rgb {
    blend_rgb(buf.get(x, y), tint, t)
}

/// Composite `tint` over the buffer pixel at `(x, y)` by `t` AND write it back.
/// Clips like [`RgbBuffer::put_checked`]: a no-op outside the buffer. Use
/// [`blend_over`] instead when the blended color feeds a further composite
/// rather than landing straight back on the buffer.
pub(super) fn blend_pixel(buf: &mut RgbBuffer, x: u16, y: u16, tint: Rgb, t: f32) {
    if x < buf.width() && y < buf.height() {
        let blended = blend_over(buf, x, y, tint, t);
        buf.put(x, y, blended);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::pen::test_density;
    use crate::pack::{appliance_overrides, fixture_overrides};
    use pixtuoid_core::sprite::format::Piece;

    #[test]
    fn blend_pixel_composites_in_bounds_and_noops_out_of_bounds() {
        let base = Rgb {
            r: 100,
            g: 100,
            b: 100,
        };
        let tint = Rgb { r: 0, g: 0, b: 0 };
        let mut buf = RgbBuffer::filled(2, 2, base);
        blend_pixel(&mut buf, 1, 1, tint, 0.5);
        assert_eq!(buf.get(1, 1), blend_rgb(base, tint, 0.5));
        assert_eq!(buf.get(0, 0), base, "neighbor untouched");
        blend_pixel(&mut buf, 2, 0, tint, 0.5);
        blend_pixel(&mut buf, 0, 2, tint, 0.5);
        assert_eq!(buf.get(0, 0), base);
        assert_eq!(
            buf.get(1, 1),
            blend_rgb(base, tint, 0.5),
            "in-bounds pixel unchanged"
        );
    }

    /// The appliance art's keys are the bundled pack's, and the pack's colour
    /// for each is the normal theme's, as `pack.toml` says: a painter that does
    /// not recolour still shows the normal office.
    #[test]
    fn the_packs_appliance_keys_are_the_normal_themes_colours() {
        let pack = crate::pack::test_office();
        let normal = crate::theme::theme_by_name("normal").expect("theme");
        for (key, pixel) in appliance_overrides(&normal.appliance) {
            assert_eq!(pack.palette().get(key), Some(pixel), "key {key:?}");
        }
    }

    /// The pack's own colours for [`fixture_overrides`]' keys are the normal
    /// theme's.
    #[test]
    fn the_packs_fixture_keys_are_the_normal_themes_colours() {
        let pack = crate::pack::test_office();
        let normal = crate::theme::theme_by_name("normal").expect("theme");
        for (key, pixel) in fixture_overrides(normal) {
            assert_eq!(pack.palette().get(key), Some(pixel), "key {key:?}");
        }
    }

    /// A theme's fixtures take its colours.
    #[test]
    fn a_recolour_rethemes_the_fixture_art() {
        let pack = crate::pack::test_office();
        let art = pack.variants_of(Piece::FishTank)[&test_density(4)].recolorable_at(0);
        let water = pack.palette().get('Д').flatten().expect("the water");
        let plain = art.recolored(&[]);
        let (x, y) = (0..plain.height())
            .flat_map(|y| (0..plain.width()).map(move |x| (x, y)))
            .find(|&(x, y)| plain.get(x, y) == Some(&Some(water)))
            .expect("the art draws its water");
        let [a, b] = ["normal", "cyberpunk"].map(|name| {
            let theme = crate::theme::theme_by_name(name).expect("theme");
            art.recolored(&fixture_overrides(theme))
        });
        assert_ne!(a.get(x, y), b.get(x, y), "the water kept the pack's colour");
    }

    /// A recolour re-derives the shading: one shaded cell of the vending art
    /// is a different colour in two themes whose appliance bodies differ.
    #[test]
    fn a_recolour_reshades_the_appliance_art() {
        let pack = crate::pack::test_office();
        let art = pack.variants_of(Piece::VendingMachine)[&test_density(4)].recolorable_at(0);
        let [a, b] = ["normal", "cyberpunk"].map(|name| {
            let theme = crate::theme::theme_by_name(name).expect("theme");
            art.recolored(&appliance_overrides(&theme.appliance))
        });
        let shade = pack.palette().get('ъ').flatten().expect("the body's shade");
        let plain = art.recolored(&[]);
        let (x, y) = (0..plain.height())
            .flat_map(|y| (0..plain.width()).map(move |x| (x, y)))
            .find(|&(x, y)| plain.get(x, y) == Some(&Some(shade)))
            .expect("the art draws the body's shade");
        assert_ne!(a.get(x, y), b.get(x, y), "the shade kept the pack's colour");
    }
}
