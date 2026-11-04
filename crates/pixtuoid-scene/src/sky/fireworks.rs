//! A fireworks night's bursts over the city: each lane of buckets fires one
//! burst at a hashed time, place and hue, as lightning strikes, so a frame's
//! bursts are a function of its beat alone and every painter shows the same.

use crate::anim::{Beat, FlashSchedule};

/// How many bursts the sky holds at once, each on its own lane of buckets.
pub(crate) const LANES: usize = 3;
/// A burst's phases in order: its sparks fly out, then fade where they stop.
const BURST: [BurstPhase; 5] = [
    BurstPhase::Kindle,
    BurstPhase::Spread,
    BurstPhase::Bloom,
    BurstPhase::Drift,
    BurstPhase::Ember,
];
/// When each lane's bursts run: each phase a whole Full beat, so the beat
/// neither skips one nor stretches it, and a gap of dark after each burst.
const BURSTS: FlashSchedule = FlashSchedule {
    period_ms: 1500,
    phase_ms: crate::anim::FULL_TICK_MS,
    phases: BURST.len() as u64,
    gap_ms: 500,
}
.floored();
// The lanes' buckets start a share of a bucket apart, on a phase boundary.
const _: () = assert!(
    BURSTS
        .period_ms
        .is_multiple_of(BURSTS.phase_ms * LANES as u64)
);
/// How far in from either end of the wall's glass a burst's heart stands, as
/// a share of it, so a burst opens over the city rather than at its edge.
const X_MARGIN: f32 = 0.1;
/// The band of the open sky a burst's heart stands in, as shares of it from
/// the glass's top.
const Y_BAND: (f32, f32) = (0.35, 0.65);

/// A hue a burst's sparks burn in; the painter gives each its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hue {
    Gold,
    Red,
    Green,
    Blue,
    Violet,
}

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

    /// How much of its hue its sparks lay over the sky.
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

/// One burst in the sky: its heart as shares of the wall's glass and of the
/// open sky's height, its phase and its hue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Burst {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) phase: BurstPhase,
    pub(crate) hue: Hue,
}

/// The bursts in the sky on `beat` of a night lit in `hues`; none at rest.
pub(crate) fn bursts_at(beat: Beat, hues: &[Hue]) -> [Option<Burst>; LANES] {
    std::array::from_fn(|lane| {
        if beat.is_rest() {
            return None;
        }
        let lane = lane as u64;
        let ms = beat
            .ms()
            .checked_sub(lane * BURSTS.period_ms / LANES as u64)?;
        let bucket = BURSTS.bucket(ms);
        let mut draws = crate::Rng::new(bucket.wrapping_mul(LANES as u64).wrapping_add(lane));
        let start = BURSTS.start(bucket, draws.draw());
        let phase = *BURST.get(BURSTS.phase(ms, start)?)?;
        Some(Burst {
            x: draws.between(X_MARGIN, 1.0 - X_MARGIN),
            y: draws.between(Y_BAND.0, Y_BAND.1),
            phase,
            hue: *hues.get(crate::spread(draws.draw(), hues.len()))?,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUES: [Hue; 2] = [Hue::Gold, Hue::Blue];

    /// Each burst runs its phases in order, one a beat, and a lane's next
    /// burst waits out the gap.
    #[test]
    fn a_lane_fires_its_phases_in_order_then_rests() {
        const BUCKETS: u64 = 20;
        let gap_beats = usize::try_from(BURSTS.gap_ms / BURSTS.phase_ms).expect("a count");
        for lane in 0..LANES {
            let phases: Vec<_> = (0..BUCKETS * BURSTS.period_ms)
                .step_by(BURSTS.phase_ms as usize)
                .map(|ms| bursts_at(Beat::at_ms(ms), &HUES)[lane].map(|b| b.phase))
                .collect();
            let mut runs: Vec<&[Option<BurstPhase>]> =
                phases.chunk_by(|a, b| a.is_some() == b.is_some()).collect();
            // The sampling's ends cut short the runs under way there.
            runs.pop();
            let fired: Vec<_> = runs.iter().filter(|r| r[0].is_some()).collect();
            assert!(
                fired.len() as u64 >= BUCKETS - 2,
                "lane {lane} fired {}",
                fired.len()
            );
            for run in &fired {
                assert_eq!(**run, BURST.map(Some), "lane {lane}");
            }
            for dark in runs.iter().skip(1).filter(|r| r[0].is_none()) {
                assert!(
                    dark.len() >= gap_beats,
                    "lane {lane}: a gap of {}",
                    dark.len()
                );
            }
        }
    }

    /// Nothing fires at rest.
    #[test]
    fn nothing_fires_at_rest() {
        let still = crate::anim::Motion::Still
            .beat(std::time::UNIX_EPOCH + std::time::Duration::from_secs(60));
        assert_eq!(bursts_at(still, &HUES), [None; LANES]);
    }

    /// A burst's heart stands within the glass's margins and the open sky's
    /// band, in one of the night's hues.
    #[test]
    fn a_burst_opens_over_the_city_in_a_hue_of_the_night() {
        const BUCKETS: u64 = 200;
        for ms in (0..BUCKETS * BURSTS.period_ms).step_by(BURSTS.phase_ms as usize) {
            for burst in bursts_at(Beat::at_ms(ms), &HUES).into_iter().flatten() {
                assert!((X_MARGIN..=1.0 - X_MARGIN).contains(&burst.x), "{burst:?}");
                assert!((Y_BAND.0..=Y_BAND.1).contains(&burst.y), "{burst:?}");
                assert!(HUES.contains(&burst.hue), "{burst:?}");
            }
        }
    }

    /// Each of a lane's buckets starts its burst at any of its legal starts
    /// alike: the latest as often as the earliest.
    #[test]
    fn a_lane_starts_its_bursts_at_every_legal_offset() {
        let starts = (BURSTS.period_ms - BURSTS.run_ms() - BURSTS.gap_ms) / BURSTS.phase_ms + 1;
        let mut seen = std::collections::BTreeSet::new();
        for bucket in 0..1000 {
            let mut draws = crate::Rng::new(bucket * LANES as u64);
            seen.insert(BURSTS.start(bucket, draws.draw()) - bucket * BURSTS.period_ms);
        }
        let want: std::collections::BTreeSet<u64> =
            (0..starts).map(|i| i * BURSTS.phase_ms).collect();
        assert_eq!(seen, want);
    }
}
