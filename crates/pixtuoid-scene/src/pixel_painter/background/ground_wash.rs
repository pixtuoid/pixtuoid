//! The classic's ground wash: [`SkyTones::ground_wash`]'s blends laid over the ground
//! band.

use crate::sprite::{Rgb, RgbBuffer};

#[cfg(doc)]
use crate::atmosphere::SkyTones;
use crate::pixel_painter::palette::{RgbLut, blend_rgb};

/// Lay each of `wash`'s `(tint, strength)` blends over the ground band
/// `top_y..bottom_y`, in order.
pub(in crate::pixel_painter) fn paint_ground_wash(
    buf: &mut RgbBuffer,
    top_y: u16,
    bottom_y: u16,
    wash: [(Rgb, f32); 2],
) {
    for (tint, s) in wash {
        blend_ground_band(buf, top_y, bottom_y, tint, s);
    }
}

/// Blend `tint` over every ground pixel in the band `top_y..bottom_y` at
/// strength `s`. `s <= 0.0` early-returns, skipping the whole pass every clear
/// frame. Tint and strength are constant across the band, so the blend runs
/// through an [`RgbLut`] — byte-identical to per-pixel [`blend_rgb`] (#900).
fn blend_ground_band(buf: &mut RgbBuffer, top_y: u16, bottom_y: u16, tint: Rgb, s: f32) {
    if s <= 0.0 {
        return;
    }
    let w = buf.width() as usize;
    let start = (top_y.min(buf.height()) as usize) * w;
    let end = (bottom_y.min(buf.height()) as usize) * w;
    if start >= end {
        return;
    }
    let lut = RgbLut::tabulate(|c| blend_rgb(c, tint, s));
    for px in &mut buf.as_mut_slice()[start..end] {
        *px = lut.apply(*px);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pixel_painter::palette::{BLACK, WHITE};

    #[test]
    fn blend_ground_band_tints_only_the_band_and_noops_at_zero() {
        let base = Rgb {
            r: 100,
            g: 100,
            b: 100,
        };
        let tint = Rgb { r: 0, g: 0, b: 0 };
        let mut buf = RgbBuffer::filled(3, 4, base);
        blend_ground_band(&mut buf, 1, 3, tint, 0.0);
        for y in 0..4 {
            for x in 0..3 {
                assert_eq!(buf.get(x, y), base, "s=0 leaves ({x},{y}) untouched");
            }
        }
        blend_ground_band(&mut buf, 1, 3, tint, 0.5);
        let blended = blend_rgb(base, tint, 0.5);
        for x in 0..3 {
            assert_eq!(buf.get(x, 0), base, "row above the band untouched");
            assert_eq!(buf.get(x, 1), blended);
            assert_eq!(buf.get(x, 2), blended);
            assert_eq!(buf.get(x, 3), base, "bottom_y is exclusive");
        }
    }

    #[test]
    fn blend_ground_band_matches_the_per_pixel_blend_reference() {
        let mut lcg = 0x9E3779B9u32;
        let mut next = || {
            lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
            Rgb {
                r: (lcg >> 24) as u8,
                g: (lcg >> 16) as u8,
                b: (lcg >> 8) as u8,
            }
        };
        let tints = [
            Rgb {
                r: 255,
                g: 244,
                b: 214,
            },
            Rgb {
                r: 24,
                g: 32,
                b: 64,
            },
            Rgb { r: 0, g: 0, b: 0 },
        ];
        for tint in tints {
            for s in [0.001f32, 0.22, 0.45, 0.999, 1.0] {
                let (w, h) = (67u16, 11u16);
                let mut buf = RgbBuffer::filled(w, h, Rgb { r: 0, g: 0, b: 0 });
                for y in 0..h {
                    for x in 0..w {
                        buf.put(x, y, next());
                    }
                }
                let mut expected = buf.clone();
                for y in 2..9u16 {
                    for x in 0..w {
                        expected.put(x, y, blend_rgb(expected.get(x, y), tint, s));
                    }
                }
                blend_ground_band(&mut buf, 2, 9, tint, s);
                for y in 0..h {
                    for x in 0..w {
                        assert_eq!(
                            buf.get(x, y),
                            expected.get(x, y),
                            "({x},{y}) diverged from per-pixel blend_rgb at tint {tint:?} s {s}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn blend_ground_band_clamps_degenerate_bounds() {
        let tint = Rgb {
            r: 200,
            g: 180,
            b: 120,
        };
        let mut buf = RgbBuffer::filled(
            6,
            5,
            Rgb {
                r: 40,
                g: 50,
                b: 60,
            },
        );
        let before = buf.clone();
        blend_ground_band(&mut buf, 4, 2, tint, 0.5);
        assert_eq!(
            buf.as_slice(),
            before.as_slice(),
            "top >= bottom is a no-op"
        );
        blend_ground_band(&mut buf, 9, 12, tint, 0.5);
        assert_eq!(
            buf.as_slice(),
            before.as_slice(),
            "a band entirely past the bottom edge is a no-op"
        );
        blend_ground_band(&mut buf, 3, 12, tint, 0.5);
        let mut expected = before.clone();
        for y in 3..5u16 {
            for x in 0..6u16 {
                expected.put(x, y, blend_rgb(expected.get(x, y), tint, 0.5));
            }
        }
        assert_eq!(
            buf.as_slice(),
            expected.as_slice(),
            "bottom_y clamps to the buffer height"
        );
    }

    #[test]
    fn a_ground_wash_lays_its_blends_in_order() {
        let base = Rgb {
            r: 100,
            g: 100,
            b: 100,
        };
        let (dark, light) = (BLACK, WHITE);
        let paint = |wash| {
            let mut buf = RgbBuffer::filled(3, 4, base);
            paint_ground_wash(&mut buf, 1, 3, wash);
            buf
        };
        let dim_then_lift = paint([(dark, 0.5), (light, 0.5)]);
        let mut expected = RgbBuffer::filled(3, 4, base);
        blend_ground_band(&mut expected, 1, 3, dark, 0.5);
        blend_ground_band(&mut expected, 1, 3, light, 0.5);
        assert_eq!(dim_then_lift.as_slice(), expected.as_slice());
        assert_ne!(
            dim_then_lift.as_slice(),
            paint([(light, 0.5), (dark, 0.5)]).as_slice(),
            "the order is part of the wash"
        );
    }
}
