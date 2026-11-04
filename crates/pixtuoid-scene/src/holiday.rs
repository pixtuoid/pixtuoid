//! The holidays the office dresses for, on the viewer's local date: each
//! swaps pieces for its dressings (`Piece::dresses`) on their canvas, so the
//! layout, its footprints and its reachability never see a holiday.

use std::time::SystemTime;

use chrono::{Datelike, NaiveDate};

use crate::pack::Piece;

/// A holiday the office dresses for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holiday {
    /// All of December.
    Christmas,
}

impl Holiday {
    /// Every holiday.
    const ALL: [Holiday; 1] = [Holiday::Christmas];

    /// The holiday local `date` falls in, if any.
    fn on(date: NaiveDate) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.holds(date))
    }

    fn holds(self, date: NaiveDate) -> bool {
        match self {
            Holiday::Christmas => date.month() == 12,
        }
    }

    /// The dressings it puts on.
    fn wears(self) -> &'static [Piece] {
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

/// What the office wears in one frame: a holiday's dressings, or by default
/// its own art.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dressing(Option<Holiday>);

impl Dressing {
    /// The office's dressing at `now`.
    pub(crate) fn at(now: SystemTime) -> Self {
        Self(Holiday::on(crate::localclock::local_time(now).date()))
    }

    /// Each holiday's dressing.
    #[cfg(test)]
    pub(crate) fn of_each_holiday() -> impl Iterator<Item = Self> {
        Holiday::ALL.into_iter().map(|h| Self(Some(h)))
    }

    /// The instant the tests read Christmas at: Dec 25, local noon.
    #[cfg(test)]
    pub(crate) fn christmas_noon() -> SystemTime {
        crate::localclock::try_on_day(CHRISTMAS_DAY, NOON).expect("Dec 25, noon, exists")
    }

    /// What `piece` is drawn as.
    pub(crate) fn piece(self, piece: Piece) -> Piece {
        self.0
            .and_then(|h| {
                h.wears()
                    .iter()
                    .copied()
                    .find(|d| d.dresses() == Some(piece))
            })
            .unwrap_or(piece)
    }
}

/// Dec 25's day after `localclock`'s base, Jan 1.
#[cfg(test)]
const CHRISTMAS_DAY: u32 = 358;
#[cfg(test)]
const NOON: u32 = 12;

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

    /// A holiday draws each piece one of its dressings dresses as that
    /// dressing, and every other piece as itself; no holiday dresses none.
    #[test]
    fn a_holiday_dresses_its_pieces_and_leaves_the_rest() {
        let christmas = Dressing(Some(Holiday::Christmas));
        for &worn in Holiday::Christmas.wears() {
            let piece = worn.dresses().expect("a holiday wears dressings");
            assert_eq!(christmas.piece(piece), worn);
            assert_eq!(Dressing::default().piece(piece), piece);
        }
        assert_eq!(christmas.piece(Piece::Desk), Piece::Desk);
    }

    /// Every dressing a holiday wears that loops changes frames no faster
    /// than the flash floor: each frame holds
    /// [`PHOTOSENSITIVE_PHASE_MIN_MS`](crate::anim::PHOTOSENSITIVE_PHASE_MIN_MS),
    /// and a second of its loop, each frame change a full swing, stays within
    /// WCAG's three flashes.
    #[test]
    fn every_looping_dressing_flashes_within_the_photosensitive_floor() {
        use crate::anim::{
            PHOTOSENSITIVE_FLASHES_PER_SECOND, PHOTOSENSITIVE_PHASE_MIN_MS,
            most_flashes_in_a_second,
        };
        const SAMPLED_MS: u64 = 3000;
        let pack = crate::pack::test_office();
        for &worn in Holiday::ALL.iter().flat_map(|h| h.wears()) {
            let anim = pack.piece(worn);
            let (frames, step) = (anim.frames().len() as u64, u64::from(anim.frame_ms()));
            if frames < 2 {
                continue;
            }
            assert!(step >= PHOTOSENSITIVE_PHASE_MIN_MS, "{}", worn.name());
            let swings = (0..=SAMPLED_MS / step).map(|i| (i * step, (i % 2) as f32));
            assert!(
                most_flashes_in_a_second(swings) <= PHOTOSENSITIVE_FLASHES_PER_SECOND,
                "{}",
                worn.name()
            );
        }
    }

    /// The office reads the date off the viewer's wall clock.
    #[test]
    fn the_dressing_follows_the_local_date() {
        use chrono::Datelike;
        let noon = crate::localclock::local_time(Dressing::christmas_noon());
        assert_eq!((noon.month(), noon.day()), (12, 25));
        assert_eq!(
            Dressing::at(Dressing::christmas_noon()),
            Dressing(Some(Holiday::Christmas))
        );
        assert_eq!(
            Dressing::at(crate::localclock::at_hour(12)),
            Dressing::default()
        );
    }
}
