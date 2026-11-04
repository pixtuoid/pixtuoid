//! The holidays the office keeps, on the viewer's local date: each draws its
//! stand-ins on the canvas of the plants they stand in for, so the layout, its
//! footprints and its reachability never see a holiday.

use std::time::SystemTime;

use chrono::{Datelike, Month, NaiveDate, NaiveDateTime, TimeDelta};
use strum::VariantArray;

use crate::layout::PlantKind;
use crate::pack::Piece;
use crate::sprite::Rgb;

/// How far past midnight a holiday's night runs: New Year's countdown, and
/// the fireworks after it, belong to the eve they began on.
const NIGHT_RUNS_PAST_MIDNIGHT: TimeDelta = TimeDelta::minutes(30);

/// The holiday day local `local` belongs to: its date, until its night ends.
fn festive_day(local: NaiveDateTime) -> NaiveDate {
    (local - NIGHT_RUNS_PAST_MIDNIGHT).date()
}

/// The hues New Year's fireworks burst in: gold, red, green, blue, violet.
const NEW_YEAR_HUES: [Rgb; 5] = [
    Rgb {
        r: 255,
        g: 214,
        b: 102,
    },
    Rgb {
        r: 255,
        g: 96,
        b: 96,
    },
    Rgb {
        r: 112,
        g: 232,
        b: 136,
    },
    Rgb {
        r: 112,
        g: 176,
        b: 255,
    },
    Rgb {
        r: 214,
        g: 144,
        b: 255,
    },
];

/// The hues of the fireworks lit at `now`: after dusk on a holiday's eve, its
/// night running past midnight; `None` on any other night or by day.
pub(crate) fn fireworks_at(now: std::time::SystemTime) -> Option<&'static [Rgb]> {
    let local = crate::localclock::local_time(now);
    let day = festive_day(local);
    let hues = Holiday::on(day)?.fireworks(day)?;
    (local.date() != day || local.time() >= crate::sky::DUSK).then_some(hues)
}

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

    /// The hues of its fireworks, if `day` is the eve it lights them on.
    fn fireworks(self, day: NaiveDate) -> Option<&'static [Rgb]> {
        match self {
            Holiday::Christmas => {
                let years_last = day.succ_opt().is_none_or(|next| next.year() != day.year());
                years_last.then_some(NEW_YEAR_HUES.as_slice())
            }
        }
    }

    /// The stand-ins it draws.
    fn stand_ins(self) -> &'static [StandIn] {
        match self {
            Holiday::Christmas => &CHRISTMAS_STAND_INS,
        }
    }
}

/// A holiday's piece drawn instead of a plant, on that plant's canvas at
/// every density, so the plant's footprint and box hold for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StandIn {
    /// The piece drawn.
    pub(crate) art: Piece,
    /// The plant it is drawn instead of: a plant's piece is drawn only
    /// through [`Occasion::art`], where another, a whiteboard's, is drawn by
    /// a wall's decor too.
    pub(crate) plant: PlantKind,
}

const CHRISTMAS_STAND_INS: [StandIn; 4] = [
    StandIn {
        art: Piece::ChristmasTree,
        plant: PlantKind::Tall,
    },
    StandIn {
        art: Piece::ChristmasTreeShort,
        plant: PlantKind::Ficus,
    },
    StandIn {
        art: Piece::Poinsettia,
        plant: PlantKind::Flower,
    },
    StandIn {
        art: Piece::Presents,
        plant: PlantKind::Succulent,
    },
];

/// Every holiday's stand-ins, for the checks every one is held to.
#[cfg(test)]
pub(crate) fn every_stand_in() -> impl Iterator<Item = StandIn> {
    Holiday::VARIANTS
        .iter()
        .flat_map(|h| h.stand_ins())
        .copied()
}

/// The holiday a frame's office keeps, by default none.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Occasion(Option<Holiday>);

impl Occasion {
    /// The office's occasion at `now`.
    pub(crate) fn at(now: SystemTime) -> Self {
        Self(Holiday::on(festive_day(crate::localclock::local_time(now))))
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
            .and_then(|h| h.stand_ins().iter().find(|s| s.plant.piece() == piece))
            .map_or(piece, |s| s.art)
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
        for &StandIn { art, plant } in Holiday::Christmas.stand_ins() {
            assert_eq!(christmas.art(plant.piece()), art);
            assert_eq!(Occasion::default().art(plant.piece()), plant.piece());
        }
        assert_eq!(christmas.art(Piece::Desk), Piece::Desk);
    }

    #[test]
    fn every_looping_stand_in_flashes_within_the_photosensitive_floor() {
        use crate::anim::{
            PHOTOSENSITIVE_FLASHES_PER_SECOND, PHOTOSENSITIVE_PHASE_MIN_MS,
            most_flashes_in_a_second,
        };
        const SAMPLED_MS: u64 = 3000;
        let pack = crate::pack::test_office();
        for stand_in in every_stand_in().map(|s| s.art) {
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

    /// Every looping stand-in is strings of lights twinkling in the bulb key
    /// the cutaway lights after dark: at every density, each frame lights
    /// bulbs the next does not.
    #[test]
    fn every_looping_stand_in_twinkles_in_the_bulb_key() {
        let pack = crate::pack::test_office();
        for stand_in in every_stand_in().map(|s| s.art) {
            let base = pack.piece(stand_in);
            if base.frames().len() < 2 {
                continue;
            }
            let densities = pack.inner().variants_of(stand_in).values();
            for sprite in std::iter::once(base).chain(densities) {
                let lit = |frame| {
                    sprite
                        .recolorable_at(frame)
                        .drawn_in(&[crate::pack::DESK_BULB_KEY])
                };
                let frames = sprite.frames().len();
                for frame in 0..frames {
                    let (now, next) = (lit(frame), lit((frame + 1) % frames));
                    let swapped = now.iter().zip(&next).any(|(&on, &then)| on && !then);
                    assert!(swapped, "{} frame {frame}", stand_in.name());
                }
            }
        }
    }

    /// The local instant `y-m-d h:min` on the viewer's wall clock.
    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> SystemTime {
        let day = NaiveDate::from_ymd_opt(y, m, d).expect("a date");
        crate::localclock::local_instant(day, h, min).expect("a local time")
    }

    /// The occasion is the viewer's local date, its night running past
    /// midnight: it turns half an hour after local midnight either side of
    /// December, which, in the justfile's `TEST_TZ`, is the other month in
    /// UTC.
    #[test]
    fn the_occasion_turns_as_the_night_ends() {
        let christmas = Occasion(Some(Holiday::Christmas));
        assert_eq!(Occasion::at(local(2026, 12, 1, 0, 29)), Occasion::default());
        assert_eq!(Occasion::at(local(2026, 12, 1, 0, 30)), christmas);
        assert_eq!(Occasion::at(local(2027, 1, 1, 0, 29)), christmas);
        assert_eq!(Occasion::at(local(2027, 1, 1, 0, 30)), Occasion::default());
        assert_eq!(Occasion::at(Occasion::christmas_noon()), christmas);
    }

    /// New Year's fireworks burst from local dusk on Dec 31 until the night
    /// ends past midnight, and on no other night.
    #[test]
    fn new_years_fireworks_light_the_eve_from_dusk_until_the_night_ends() {
        use chrono::Timelike;
        let dusk = crate::sky::DUSK;
        let lit = |now| fireworks_at(now).is_some();
        assert!(!lit(local(2026, 12, 31, dusk.hour() - 1, 59)));
        assert!(lit(local(2026, 12, 31, dusk.hour(), dusk.minute())));
        assert!(lit(local(2027, 1, 1, 0, 29)));
        assert!(!lit(local(2027, 1, 1, 0, 30)));
        assert!(!lit(local(2026, 12, 30, 23, 0)));
        assert!(!lit(local(2026, 12, 31, 0, 45)));
        assert_eq!(
            fireworks_at(local(2026, 12, 31, 23, 0)),
            Some(NEW_YEAR_HUES.as_slice())
        );
    }
}
