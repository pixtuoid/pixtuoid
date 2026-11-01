//! Ordered dither: how a gradient is drawn on a pixel grid, the one threshold
//! matrix both painters step by, never a soft blend. A ramp of several tones is
//! flat bands with a dithered seam ([`step`]); a gradient between two is
//! dithered across its span ([`takes_next`]).

use crate::sprite::Rgb;

/// The 4x4 ordered (Bayer) threshold matrix.
///
/// Its evenly-spread levels are why a dither reads as a smooth ramp rather than
/// as noise or as banding: the classic pixel-art answer.
const BAYER_4X4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
/// How many threshold levels [`BAYER_4X4`] spreads.
const BAYER_LEVELS: u32 = 16;
/// The columns and rows after which an ordered dither repeats.
pub(crate) const PERIOD: u16 = BAYER_4X4.len() as u16;

/// Whether the pixel at `(x, y)` takes the next tone of an ordered dither
/// covering `coverage` of its area.
pub(crate) fn takes_next(x: u16, y: u16, coverage: f32) -> bool {
    below(x, y, level(coverage))
}

/// `coverage` as one of the levels [`BAYER_4X4`] draws.
fn level(coverage: f32) -> u8 {
    (coverage.clamp(0.0, 1.0) * BAYER_LEVELS as f32) as u8
}

fn below(x: u16, y: u16, level: u8) -> bool {
    BAYER_4X4[usize::from(y % PERIOD)][usize::from(x % PERIOD)] < level
}

/// A value carried from `from` to `to` by an ordered dither: the pixel at
/// `(x, y)` takes `to` where [`takes_next`] would at the share, so a change
/// draws as a growing share of pixels, never as a blend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Dithered<T> {
    from: T,
    to: T,
    /// The share of pixels on `to`, in [`BAYER_4X4`]'s levels: all a dither
    /// can draw, so equal levels draw equal pixels.
    level: u8,
}

impl<T: Copy> Dithered<T> {
    /// `value` on every pixel.
    #[cfg(test)]
    pub(crate) fn solid(value: T) -> Self {
        Self {
            from: value,
            to: value,
            level: 0,
        }
    }

    /// `to` on `share` of the pixels, `from` on the rest.
    pub(crate) fn new(from: T, to: T, share: f32) -> Self {
        Self {
            from,
            to,
            level: level(share),
        }
    }

    /// Whether the pixel at `(x, y)` takes `to`.
    pub(crate) fn takes_to(self, x: u16, y: u16) -> bool {
        below(x, y, self.level)
    }

    /// The value the pixel at `(x, y)` takes.
    pub(crate) fn at(self, x: u16, y: u16) -> T {
        if self.takes_to(x, y) {
            self.to
        } else {
            self.from
        }
    }

    /// The value going and the value coming.
    pub(crate) fn ends(self) -> [T; 2] {
        [self.from, self.to]
    }

    /// The value every pixel takes, or `None` while the dither splits them.
    pub(crate) fn as_solid(self) -> Option<T> {
        match self.level {
            0 => Some(self.from),
            l if u32::from(l) >= BAYER_LEVELS => Some(self.to),
            _ => None,
        }
    }

    /// Both values through `f`, at the same share.
    pub(crate) fn map<U>(self, f: impl Fn(T) -> U) -> Dithered<U> {
        Dithered {
            from: f(self.from),
            to: f(self.to),
            level: self.level,
        }
    }
}

/// How many flat tones a falloff steps through from its peak down, the seam
/// between each pair dithered.
pub(crate) const FALLOFF_TONES: u8 = 3;
const _: () = assert!(FALLOFF_TONES > 0);

/// The share of each band, at its top, that dithers into the next: a whole band
/// of dither reads as grain.
pub(crate) const SEAM: f32 = 0.3;

/// The whole tone a ramp `stops` along paints at `(x, y)`: solid through each
/// band, dithered into the next only across its top [`SEAM`].
pub(crate) fn step(stops: f32, x: u16, y: u16) -> u8 {
    let whole = stops.max(0.0).floor();
    let into_seam = (stops - whole - (1.0 - SEAM)) / SEAM;
    whole as u8 + u8::from(into_seam > 0.0 && takes_next(x, y, into_seam))
}

/// [`step`], rounding to the nearer tone: the [`SEAM`] sits just below the
/// midpoint between two, so a falloff keeps the reach its level gives it, where
/// flooring would shave its faint outer ring off.
pub(crate) fn nearest(stops: f32, x: u16, y: u16) -> u8 {
    step(stops + 0.5, x, y)
}

/// `level` drawn at `(x, y)` as one of `tones` flat steps of `peak`, by
/// [`nearest`].
pub(crate) fn stepped(level: f32, peak: f32, tones: u8, x: u16, y: u16) -> f32 {
    if peak <= 0.0 || level <= 0.0 {
        return 0.0;
    }
    let tone = peak / f32::from(tones);
    f32::from(nearest(level / tone, x, y).min(tones)) * tone
}

/// Colours already stepped `level` stops: a shade crosses a handful of tones,
/// and scanning them per pixel is cheaper than [`Rgb::ramp`]'s hashed memo.
pub(crate) struct Stepped {
    level: i8,
    seen: Vec<(Rgb, Rgb)>,
}

impl Stepped {
    pub(crate) fn new(level: i8) -> Self {
        Self {
            level,
            seen: Vec::new(),
        }
    }

    pub(crate) fn of(&mut self, c: Rgb) -> Rgb {
        if let Some(&(_, stepped)) = self.seen.iter().find(|(from, _)| *from == c) {
            return stepped;
        }
        let stepped = c.ramp(self.level);
        self.seen.push((c, stepped));
        stepped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ramp_steps_through_its_tones_and_nothing_between() {
        let peak = 0.6;
        for y in 0..8 {
            for x in 0..8 {
                for i in 0..=20 {
                    let level = peak * i as f32 / 20.0;
                    let got = stepped(level, peak, 3, x, y);
                    let tone = got / (peak / 3.0);
                    assert!(
                        (tone - tone.round()).abs() < 1e-4,
                        "{level} at ({x},{y}) gave {got}, between tones"
                    );
                    assert!((0.0..=peak + 1e-6).contains(&got));
                }
            }
        }
    }

    /// Flat rings: a level near a tone paints it across the whole tile; only
    /// the seam below the midpoint mixes in the next.
    #[test]
    fn a_level_paints_its_nearer_tone_flat_and_dithers_only_the_seam() {
        let (peak, tones) = (0.9, 3);
        let tone = peak / f32::from(tones);
        let tile = |level: f32| -> Vec<f32> {
            (0..4u16)
                .flat_map(|y| (0..4u16).map(move |x| stepped(level, peak, tones, x, y)))
                .collect()
        };
        let seam_start = 1.5 - SEAM;
        assert!(
            tile(tone * (seam_start - 0.05)).iter().all(|&t| t == tone),
            "below the seam"
        );
        assert!(
            tile(tone * 1.55).iter().all(|&t| t == tone * 2.0),
            "past the midpoint"
        );
        let seam = tile(tone * (seam_start + SEAM / 2.0));
        let upper = seam.iter().filter(|&&t| t == tone * 2.0).count();
        assert_eq!(upper, 8, "mid-seam dithers the two tones half and half");
        assert!(seam.iter().all(|&t| t == tone || t == tone * 2.0));
    }

    /// A band is one tone below its [`SEAM`], mixed with the next across it.
    #[test]
    fn a_band_is_solid_but_for_its_seam() {
        for band in 0..4u8 {
            for tenth in 0..10 {
                let stops = f32::from(band) + tenth as f32 / 10.0;
                let steps: std::collections::BTreeSet<u8> = (0..8)
                    .flat_map(|y| (0..8).map(move |x| step(stops, x, y)))
                    .collect();
                if (tenth as f32 / 10.0) < 1.0 - SEAM {
                    assert_eq!(steps, [band].into(), "{stops} is dithered off its seam");
                } else {
                    assert!(
                        steps.is_subset(&[band, band + 1].into()),
                        "{stops}: {steps:?}"
                    );
                }
            }
        }
    }

    /// A level on a tone, however the ramp's division rounds it, paints that
    /// tone at every phase of the matrix.
    #[test]
    fn an_exact_tone_paints_flat_at_every_phase() {
        for (level, peak) in [(0.8 * 0.4 * (1.0 - 1.0 / 3.0), 0.8 * 0.4), (0.3, 0.9)] {
            let tones = 3;
            let want = stepped(level, peak, tones, 0, 0);
            for y in 0..4 {
                for x in 0..4 {
                    assert_eq!(
                        stepped(level, peak, tones, x, y),
                        want,
                        "{level} at ({x},{y})"
                    );
                }
            }
        }
    }
}
