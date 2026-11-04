//! The holidays the office keeps, on the viewer's local date: each draws its
//! stand-ins (`Piece::stands_in_for`) on the canvas of the pieces they stand
//! in for, so the layout, its footprints and its reachability never see a
//! holiday.

use std::time::SystemTime;

use chrono::{Datelike, Month, NaiveDate};
use strum::VariantArray;

use crate::pack::Piece;

/// A holiday the office keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::VariantArray)]
enum Holiday {
    /// All of December.
    Christmas,
}

impl Holiday {
    /// The holiday local `date` falls in, if any.
    fn on(date: NaiveDate) -> Option<Self> {
        Self::VARIANTS.iter().copied().find(|h| h.holds(date))
    }

    fn holds(self, date: NaiveDate) -> bool {
        match self {
            Holiday::Christmas => date.month() == Month::December.number_from_month(),
        }
    }

    /// The stand-ins it draws.
    fn stand_ins(self) -> &'static [Piece] {
        match self {
            Holiday::Christmas => &[
                Piece::ChristmasTree,
                Piece::ChristmasTreeShort,
                Piece::Poinsettia,
                Piece::Presents,
            ],
        }
    }
}

/// The holiday a frame's office keeps, by default none.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Occasion(Option<Holiday>);

impl Occasion {
    /// The office's occasion at `now`.
    pub(crate) fn at(now: SystemTime) -> Self {
        Self(Holiday::on(crate::localclock::local_time(now).date()))
    }

    /// Each holiday's occasion.
    #[cfg(test)]
    pub(crate) fn of_each_holiday() -> impl Iterator<Item = Self> {
        Holiday::VARIANTS.iter().map(|&h| Self(Some(h)))
    }

    /// The instant the tests keep Christmas at: Dec 25, local noon.
    #[cfg(test)]
    pub(crate) fn christmas_noon() -> SystemTime {
        let day = NaiveDate::from_ymd_opt(2026, 12, 25).expect("a date");
        crate::localclock::local_instant(day, 12, 0).expect("local noon exists")
    }

    /// The art `piece` is drawn in: its holiday's stand-in for it, else itself.
    pub(crate) fn art(self, piece: Piece) -> Piece {
        self.0
            .and_then(|h| {
                h.stand_ins()
                    .iter()
                    .copied()
                    .find(|s| s.stands_in_for() == Some(piece))
            })
            .unwrap_or(piece)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(y: i32, m: u32, d: u32) -> Option<Holiday> {
        Holiday::on(NaiveDate::from_ymd_opt(y, m, d).expect("a date"))
    }

    /// Christmas holds every day of December, and neither day either side.
    #[test]
    fn christmas_holds_december_and_no_day_either_side() {
        assert_eq!(on(2026, 11, 30), None);
        assert_eq!(on(2026, 12, 1), Some(Holiday::Christmas));
        assert_eq!(on(2026, 12, 31), Some(Holiday::Christmas));
        assert_eq!(on(2027, 1, 1), None);
    }

    /// A holiday draws each piece one of its stand-ins stands in for as that
    /// stand-in, and every other piece as itself; with no holiday, every
    /// piece is drawn as itself.
    #[test]
    fn a_holiday_stands_in_for_its_pieces_and_leaves_the_rest() {
        let christmas = Occasion(Some(Holiday::Christmas));
        for &stand_in in Holiday::Christmas.stand_ins() {
            let piece = stand_in.stands_in_for().expect("a stand-in stands in");
            assert_eq!(christmas.art(piece), stand_in);
            assert_eq!(Occasion::default().art(piece), piece);
        }
        assert_eq!(christmas.art(Piece::Desk), Piece::Desk);
    }

    /// Every stand-in a holiday draws that loops changes frames no faster than
    /// the flash floor: each frame holds
    /// [`PHOTOSENSITIVE_PHASE_MIN_MS`](crate::anim::PHOTOSENSITIVE_PHASE_MIN_MS),
    /// and a second of its loop, each frame change a full swing, stays within
    /// WCAG's three flashes.
    #[test]
    fn every_looping_stand_in_flashes_within_the_photosensitive_floor() {
        use crate::anim::{
            PHOTOSENSITIVE_FLASHES_PER_SECOND, PHOTOSENSITIVE_PHASE_MIN_MS,
            most_flashes_in_a_second,
        };
        const SAMPLED_MS: u64 = 3000;
        let pack = crate::pack::test_office();
        for &stand_in in Holiday::VARIANTS.iter().flat_map(|h| h.stand_ins()) {
            let anim = pack.piece(stand_in);
            let (frames, step) = (anim.frames().len() as u64, u64::from(anim.frame_ms()));
            if frames < 2 {
                continue;
            }
            assert!(step >= PHOTOSENSITIVE_PHASE_MIN_MS, "{}", stand_in.name());
            let swings = (0..=SAMPLED_MS / step).map(|i| (i * step, (i % 2) as f32));
            assert!(
                most_flashes_in_a_second(swings) <= PHOTOSENSITIVE_FLASHES_PER_SECOND,
                "{}",
                stand_in.name()
            );
        }
    }

    /// Every looping stand-in is a string of lights: each of its frames, at
    /// every density, draws the bulb key the cutaway lights after dark.
    #[test]
    fn every_looping_stand_in_lights_up_after_dark() {
        let pack = crate::pack::test_office();
        for &stand_in in Holiday::VARIANTS.iter().flat_map(|h| h.stand_ins()) {
            let base = pack.piece(stand_in);
            if base.frames().len() < 2 {
                continue;
            }
            let densities = pack.inner().variants_of(stand_in).values();
            for sprite in std::iter::once(base).chain(densities) {
                for frame in 0..sprite.frames().len() {
                    let bulbs = sprite
                        .recolorable_at(frame)
                        .drawn_in(&[crate::pack::DESK_BULB_KEY]);
                    assert!(bulbs.contains(&true), "{} frame {frame}", stand_in.name());
                }
            }
        }
    }

    /// The occasion is the viewer's local date: it turns at local midnight
    /// either side of December, which, in the tests' +05:45 zone, is the other
    /// month in UTC.
    #[test]
    fn the_occasion_turns_at_local_midnight() {
        let at = |y, m, d, h, min| {
            let day = NaiveDate::from_ymd_opt(y, m, d).expect("a date");
            Occasion::at(crate::localclock::local_instant(day, h, min).expect("a local time"))
        };
        let christmas = Occasion(Some(Holiday::Christmas));
        assert_eq!(at(2026, 11, 30, 23, 59), Occasion::default());
        assert_eq!(at(2026, 12, 1, 0, 0), christmas);
        assert_eq!(at(2026, 12, 31, 23, 59), christmas);
        assert_eq!(at(2027, 1, 1, 0, 0), Occasion::default());
        assert_eq!(Occasion::at(Occasion::christmas_noon()), christmas);
    }
}
