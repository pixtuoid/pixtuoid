//! A fireworks night's bursts over the city: each lane of buckets fires one
//! burst at a hashed time, place and hue, as lightning strikes, so a frame's
//! bursts are a function of its beat alone and every painter shows the same.

use crate::anim::{Beat, PHOTOSENSITIVE_PHASE_MIN_MS};
use crate::sprite::Rgb;

/// How many bursts the sky holds at once, each on its own lane of buckets.
pub(crate) const LANES: usize = 3;
/// One lane's bucket: a burst at a hashed offset in each.
const BURST_PERIOD_MS: u64 = 1500;
/// How long each of [`BURST`]'s phases holds: a whole Full beat, so the beat
/// neither skips one nor stretches it.
const BURST_PHASE_MS: u64 = crate::anim::FULL_TICK_MS;
/// A burst's phases in order: its sparks fly out, then fade where they stop.
const BURST: [BurstPhase; 5] = [
    BurstPhase::Kindle,
    BurstPhase::Spread,
    BurstPhase::Bloom,
    BurstPhase::Drift,
    BurstPhase::Ember,
];
/// How long one burst's phases last together.
const BURST_MS: u64 = BURST.len() as u64 * BURST_PHASE_MS;
/// The least dark time a lane keeps between its bursts.
const BURST_GAP_MS: u64 = 500;
const _: () = assert!(
    BURST_PHASE_MS >= PHOTOSENSITIVE_PHASE_MIN_MS
        && BURST_GAP_MS >= PHOTOSENSITIVE_PHASE_MIN_MS
        && BURST_MS + BURST_GAP_MS <= BURST_PERIOD_MS
        && BURST_PERIOD_MS.is_multiple_of(BURST_PHASE_MS * LANES as u64)
);
/// How far in from either end of the wall a burst's heart stands, as a share
/// of it, so a burst opens over the city rather than at its edge.
const X_MARGIN: f32 = 0.1;
/// The band of the open sky a burst's heart stands in, as shares of it from
/// the glass's top.
const Y_BAND: (f32, f32) = (0.35, 0.65);
/// A draw's resolution as a share: 0.000 to 0.999.
const SHARE_STEPS: u64 = 1000;

/// One phase of a burst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BurstPhase {
    Kindle,
    Spread,
    Bloom,
    Drift,
    Ember,
}

impl BurstPhase {
    /// How far its sparks stand from the burst's heart, in layout units.
    pub(crate) const fn radius(self) -> f32 {
        match self {
            BurstPhase::Kindle => 1.0,
            BurstPhase::Spread => 3.0,
            BurstPhase::Bloom => 5.0,
            BurstPhase::Drift => 6.0,
            BurstPhase::Ember => 6.5,
        }
    }

    /// How far its sparks have fallen below where they flew, in layout units.
    pub(crate) const fn sag(self) -> f32 {
        match self {
            BurstPhase::Kindle | BurstPhase::Spread | BurstPhase::Bloom => 0.0,
            BurstPhase::Drift => 0.5,
            BurstPhase::Ember => 1.5,
        }
    }

    /// How much of its hue its sparks lay over the sky; a spark's trail, back
    /// toward the heart, lays half as much.
    pub(crate) const fn strength(self) -> f32 {
        match self {
            BurstPhase::Kindle | BurstPhase::Spread | BurstPhase::Bloom => 1.0,
            BurstPhase::Drift => 0.6,
            BurstPhase::Ember => 0.3,
        }
    }

    /// Whether its heart still burns, where the burst opened.
    pub(crate) const fn heart_lit(self) -> bool {
        matches!(self, BurstPhase::Kindle)
    }
}

/// One burst in the sky: its heart as shares of the wall's width and of the
/// open sky's height, its phase and its hue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Burst {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) phase: BurstPhase,
    pub(crate) hue: Rgb,
}

/// The bursts in the sky on `beat` of a night lit in `hues`; none at rest.
pub(crate) fn bursts_at(beat: Beat, hues: &[Rgb]) -> [Option<Burst>; LANES] {
    std::array::from_fn(|lane| {
        if beat.is_rest() || hues.is_empty() {
            return None;
        }
        let lane = lane as u64;
        let ms = beat
            .ms()
            .checked_sub(lane * BURST_PERIOD_MS / LANES as u64)?;
        let bucket = ms / BURST_PERIOD_MS;
        let seed = bucket.wrapping_mul(LANES as u64).wrapping_add(lane);
        let since = ms.checked_sub(bucket * BURST_PERIOD_MS + offset(seed))?;
        let phase = *BURST.get(usize::try_from(since / BURST_PHASE_MS).ok()?)?;
        let share = |n| (crate::splitmix_draw(seed, n) % SHARE_STEPS) as f32 / SHARE_STEPS as f32;
        Some(Burst {
            x: X_MARGIN + share(1) * (1.0 - 2.0 * X_MARGIN),
            y: Y_BAND.0 + share(2) * (Y_BAND.1 - Y_BAND.0),
            phase,
            hue: hues[crate::spread(crate::splitmix_draw(seed, 3), hues.len())],
        })
    })
}

/// Where in its bucket lane seed `seed`'s burst starts: on a phase boundary,
/// leaving the burst and the gap after it inside the bucket.
fn offset(seed: u64) -> u64 {
    let off = crate::splitmix_draw(seed, 0) % (BURST_PERIOD_MS - BURST_MS - BURST_GAP_MS + 1);
    off / BURST_PHASE_MS * BURST_PHASE_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::{Motion, PHOTOSENSITIVE_FLASHES_PER_SECOND, most_flashes_in_a_second};

    const HUES: [Rgb; 2] = [Rgb { r: 1, g: 2, b: 3 }, Rgb { r: 4, g: 5, b: 6 }];

    /// Each burst runs its phases in order, one a beat, and a lane's next
    /// burst waits out the gap.
    #[test]
    fn a_lane_fires_its_phases_in_order_then_rests() {
        for lane in 0..LANES {
            let mut seen = Vec::new();
            for ms in (0..20 * BURST_PERIOD_MS).step_by(BURST_PHASE_MS as usize) {
                let phase = bursts_at(Beat::at_ms(ms), &HUES)[lane].map(|b| b.phase);
                if seen.last() != Some(&phase) {
                    seen.push(phase);
                }
            }
            // The sampling's end cuts short the burst under way there.
            if seen.last().is_some_and(Option::is_some) {
                seen.truncate(seen.iter().rposition(Option::is_none).map_or(0, |i| i + 1));
            }
            let fired: Vec<_> = seen
                .split(Option::is_none)
                .filter(|r| !r.is_empty())
                .collect();
            assert!(
                fired.len() >= 19,
                "lane {lane} fired {} bursts",
                fired.len()
            );
            for run in fired {
                assert_eq!(run, BURST.map(Some), "lane {lane}");
            }
        }
    }

    /// Nothing fires at rest, and a night with no hues fires nothing.
    #[test]
    fn nothing_fires_at_rest_or_without_hues() {
        let still = Motion::Still.beat(std::time::UNIX_EPOCH + std::time::Duration::from_secs(60));
        assert_eq!(bursts_at(still, &HUES), [None; LANES]);
        for ms in (0..BURST_PERIOD_MS).step_by(BURST_PHASE_MS as usize) {
            assert_eq!(bursts_at(Beat::at_ms(ms), &[]), [None; LANES]);
        }
    }

    /// A burst's heart stands within the wall's margins and the open sky's
    /// band, in one of the night's hues.
    #[test]
    fn a_burst_opens_over_the_city_in_a_hue_of_the_night() {
        for ms in (0..200 * BURST_PERIOD_MS).step_by(BURST_PHASE_MS as usize) {
            for burst in bursts_at(Beat::at_ms(ms), &HUES).into_iter().flatten() {
                assert!((X_MARGIN..=1.0 - X_MARGIN).contains(&burst.x), "{burst:?}");
                assert!((Y_BAND.0..=Y_BAND.1).contains(&burst.y), "{burst:?}");
                assert!(HUES.contains(&burst.hue), "{burst:?}");
            }
        }
    }

    /// On every moving tier, the sky's bursts never flash more than WCAG's
    /// three times in any second: each lit spark's strength is a level, and a
    /// burst's rise and fall one flash.
    #[test]
    fn the_bursts_flash_within_the_photosensitive_floor_on_every_tier() {
        const NIGHT_MS: u64 = 4 * 3_600_000;
        for motion in Motion::ALL {
            let samples = (0..NIGHT_MS).step_by(BURST_PHASE_MS as usize).map(|wall| {
                let beat =
                    motion.beat(std::time::UNIX_EPOCH + std::time::Duration::from_millis(wall));
                let lit = bursts_at(beat, &HUES)
                    .iter()
                    .flatten()
                    .map(|b| b.phase.strength())
                    .fold(0.0, f32::max);
                (wall, lit)
            });
            assert!(
                most_flashes_in_a_second(samples) <= PHOTOSENSITIVE_FLASHES_PER_SECOND,
                "{motion:?}"
            );
        }
    }
}
