//! Alpha compositing, painter-neutral: one colour over another by coverage, per
//! sRGB channel, as the web composites. A gradient between two hues whose middle
//! must not sag is [`Rgb::mix`] instead.

use crate::sprite::Rgb;

/// The blend anchors: a lit fixture is its tint pushed toward [`WHITE`], an unlit
/// one toward [`BLACK`] — one pair, so the lamps, the screens and the neon tube
/// cannot disagree on what "white" is.
pub(crate) const WHITE: Rgb = Rgb {
    r: 255,
    g: 255,
    b: 255,
};
pub(crate) const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

/// Channel `b` over `a` at coverage `t`: cheap per pixel, and channel-separable,
/// so a constant-tint pass can tabulate it.
pub(crate) fn blend(a: u8, b: u8, t: f32) -> u8 {
    (f32::from(a) * (1.0 - t) + f32::from(b) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// [`blend`] on each channel, `b` over `a` at one coverage `t`.
pub(crate) fn blend_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    Rgb {
        r: blend(a.r, b.r, t),
        g: blend(a.g, b.g, t),
        b: blend(a.b, b.b, t),
    }
}
