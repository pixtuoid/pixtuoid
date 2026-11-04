//! A fireworks night's bursts over the city: each lane of buckets fires one
//! burst at a hashed time, place and hue, as lightning strikes, though outside
//! the night's finale only the lanes [`ALL_NIGHT_LANE_STRIDE`] picks fire, so
//! a frame's bursts are a function of its beat alone and every painter shows
//! the same.

use crate::anim::{Beat, FlashSchedule};

/// How many bursts the sky holds at once at most, each on its own lane of
/// buckets: all of them in a night's finale.
pub(crate) const LANES: usize = 6;
/// Of every this many lanes, one fires all night: the rest fire only in its
/// finale.
const ALL_NIGHT_LANE_STRIDE: usize = 2;
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
/// How far in from either end of the bays' span a burst's heart stands, as
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

/// One burst in the sky: its heart as shares of the bays' span and of the
/// open sky's height, its phase and its hue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Burst {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) phase: BurstPhase,
    pub(crate) hue: Hue,
}

/// The bursts in the sky on `beat` of a night lit in `hues`, every lane firing
/// a burst that starts while `finale` holds of its wall ms; none at rest.
pub(crate) fn bursts_at(
    beat: Beat,
    hues: &[Hue],
    finale: impl Fn(u64) -> bool,
) -> [Option<Burst>; LANES] {
    std::array::from_fn(|lane| {
        if beat.is_rest() {
            return None;
        }
        let all_night = lane.is_multiple_of(ALL_NIGHT_LANE_STRIDE);
        let lane = lane as u64;
        let shift = lane * BURSTS.period_ms / LANES as u64;
        let ms = beat.ms().checked_sub(shift)?;
        let bucket = BURSTS.bucket(ms);
        let mut draws = crate::Rng::new(bucket.wrapping_mul(LANES as u64).wrapping_add(lane));
        let start = BURSTS.start(bucket, draws.draw());
        if !all_night && !finale(beat.wall_ms(start + shift)) {
            return None;
        }
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
                .map(|ms| bursts_at(Beat::at_ms(ms), &HUES, |_| true)[lane].map(|b| b.phase))
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

    /// Outside a finale one lane in every [`ALL_NIGHT_LANE_STRIDE`] fires,
    /// and in one every lane.
    #[test]
    fn a_finale_fires_every_lane_and_the_rest_of_the_night_some() {
        let fired = |finale: bool| {
            let mut lanes = [false; LANES];
            for ms in (0..20 * BURSTS.period_ms).step_by(BURSTS.phase_ms as usize) {
                for (lane, b) in bursts_at(Beat::at_ms(ms), &HUES, |_| finale)
                    .iter()
                    .enumerate()
                {
                    lanes[lane] |= b.is_some();
                }
            }
            lanes
        };
        assert_eq!(fired(true), [true; LANES]);
        assert_eq!(
            fired(false),
            std::array::from_fn(|l| l % ALL_NIGHT_LANE_STRIDE == 0)
        );
    }

    /// A lane that fires only in a finale asks about its burst's start, on
    /// every moving tier: each of its runs is whole, its kindling inside the
    /// finale, so no burst is cut in or out mid-run at the finale's edges.
    #[test]
    fn a_finale_lane_fires_whole_bursts_that_start_inside_it() {
        use crate::anim::Motion;
        const SPAN_MS: u64 = 30 * BURSTS.period_ms;
        let open = 10 * BURSTS.period_ms..20 * BURSTS.period_ms;
        let at = |wall| std::time::UNIX_EPOCH + std::time::Duration::from_millis(wall);
        for motion in [Motion::Full, Motion::Calm] {
            let pace = motion.pace().expect("a moving tier");
            for lane in (0..LANES).filter(|l| !l.is_multiple_of(ALL_NIGHT_LANE_STRIDE)) {
                let samples: Vec<_> = (0..SPAN_MS * pace)
                    .step_by(BURSTS.phase_ms as usize)
                    .map(|wall| {
                        (
                            wall,
                            bursts_at(motion.beat(at(wall)), &HUES, |w| open.contains(&w))[lane],
                        )
                    })
                    .collect();
                let runs: Vec<_> = samples
                    .chunk_by(|a, b| a.1.is_some() == b.1.is_some())
                    .collect();
                let fired: Vec<_> = runs.iter().filter(|r| r[0].1.is_some()).collect();
                assert!(!fired.is_empty(), "{motion:?} lane {lane}");
                for run in fired {
                    let phases: Vec<_> = run.iter().map(|(_, b)| b.map(|b| b.phase)).collect();
                    let mut distinct = phases.clone();
                    distinct.dedup();
                    assert_eq!(
                        distinct,
                        BURST.map(Some),
                        "{motion:?} lane {lane}: a whole run"
                    );
                    assert!(
                        open.contains(&run[0].0),
                        "{motion:?} lane {lane}: kindled inside"
                    );
                }
            }
        }
    }

    /// Nothing fires at rest.
    #[test]
    fn nothing_fires_at_rest() {
        let still = crate::anim::Motion::Still
            .beat(std::time::UNIX_EPOCH + std::time::Duration::from_secs(60));
        assert_eq!(bursts_at(still, &HUES, |_| true), [None; LANES]);
    }

    /// A burst's heart stands within the bays' span's margins and the open sky's
    /// band, in one of the night's hues.
    #[test]
    fn a_burst_opens_over_the_city_in_a_hue_of_the_night() {
        const BUCKETS: u64 = 200;
        for ms in (0..BUCKETS * BURSTS.period_ms).step_by(BURSTS.phase_ms as usize) {
            for burst in bursts_at(Beat::at_ms(ms), &HUES, |_| true)
                .into_iter()
                .flatten()
            {
                assert!((X_MARGIN..=1.0 - X_MARGIN).contains(&burst.x), "{burst:?}");
                assert!((Y_BAND.0..=Y_BAND.1).contains(&burst.y), "{burst:?}");
                assert!(HUES.contains(&burst.hue), "{burst:?}");
            }
        }
    }

    /// Each lane starts its bursts at every legal start alike, as
    /// `bursts_at` places them: the latest as surely as the earliest.
    #[test]
    fn a_lane_starts_its_bursts_at_every_legal_offset() {
        const BUCKETS: u64 = 400;
        for lane in 0..LANES {
            let shift = lane as u64 * BURSTS.period_ms / LANES as u64;
            let mut seen = std::collections::BTreeSet::new();
            let mut was = None;
            for ms in (shift..shift + BUCKETS * BURSTS.period_ms).step_by(BURSTS.phase_ms as usize)
            {
                let phase = bursts_at(Beat::at_ms(ms), &HUES, |_| true)[lane].map(|b| b.phase);
                if phase == Some(BurstPhase::Kindle) && was != phase {
                    seen.insert((ms - shift) % BURSTS.period_ms);
                }
                was = phase;
            }
            let want: std::collections::BTreeSet<u64> =
                (0..BURSTS.starts()).map(|i| i * BURSTS.phase_ms).collect();
            assert_eq!(seen, want, "lane {lane}");
        }
    }

    /// Each lane draws its own bursts: no lane opens its bursts where
    /// another's open, bucket for bucket.
    #[test]
    fn each_lane_draws_its_own_bursts() {
        const BURSTS_A_LANE: usize = 5;
        let places: std::collections::HashSet<Vec<(u32, u32)>> = (0..LANES)
            .map(|lane| {
                (0..(BURSTS_A_LANE as u64 + 1) * BURSTS.period_ms)
                    .step_by(BURSTS.phase_ms as usize)
                    .filter_map(|ms| bursts_at(Beat::at_ms(ms), &HUES, |_| true)[lane])
                    .filter(|b| b.phase == BurstPhase::Kindle)
                    .map(|b| (b.x.to_bits(), b.y.to_bits()))
                    .take(BURSTS_A_LANE)
                    .collect()
            })
            .collect();
        assert_eq!(places.len(), LANES, "{places:?}");
    }
}
