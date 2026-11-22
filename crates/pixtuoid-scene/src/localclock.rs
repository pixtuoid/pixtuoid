//! The office's one bridge between an instant and the viewer's local time:
//! `local_time` reads `now` for every reader of the local hour or date, and
//! the helpers build instants from local times for tests, benches and the
//! snapshot examples.
//!
//! Anything whose ASSERTION depends on the local hour or date builds its
//! instants here; a unit test needing only "some fixed moment" keeps an epoch offset.

use std::sync::OnceLock;
use std::time::SystemTime;

use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone};

/// The viewer's offset at an instant, where a host reads it: see
/// [`read_viewer_offset_with`].
static VIEWER_OFFSET: OnceLock<fn(Timestamp) -> Offset> = OnceLock::new();

/// Reads the viewer's offset at each instant through `offset_at` in place of
/// `TimeZone::system()`, for a host whose platform jiff finds no zone on
/// without a bundled tz database: a browser, whose own clock knows its offset
/// (jiff 1.0.2 `PLATFORM.md`, "IANA Time Zone Database"). The first host's
/// reading stands.
pub fn read_viewer_offset_with(offset_at: fn(Timestamp) -> Offset) {
    let _ = VIEWER_OFFSET.set(offset_at);
}

/// The zone the office reads `instant` in: the viewer's.
fn zone_at(instant: Timestamp) -> TimeZone {
    VIEWER_OFFSET
        .get()
        .map_or_else(host_zone, |offset_at| TimeZone::fixed(offset_at(instant)))
}

#[cfg(not(test))]
#[expect(
    clippy::disallowed_methods,
    reason = "the office's reader of the viewer's zone"
)]
fn host_zone() -> TimeZone {
    TimeZone::system()
}

/// This crate's unit tests read one fixed zone, never the runner's. Its
/// offset is neither UTC nor a whole hour, so epoch and local hours never
/// coincide.
#[cfg(test)]
fn host_zone() -> TimeZone {
    let offset = jiff::SignedDuration::from_hours(5) + jiff::SignedDuration::from_mins(45);
    TimeZone::fixed(Offset::try_from(offset).expect("the offset is within a day of UTC"))
}

/// `now` in the viewer's local time.
pub(crate) fn local_time(now: SystemTime) -> jiff::civil::DateTime {
    let instant = Timestamp::try_from(now).expect("a clock reading is inside jiff's calendar");
    zone_at(instant).to_datetime(instant)
}

/// Day 0 for every helper, so day 1 of `scripts/media.json`'s renders: a day no
/// holiday touches, so a test meets only the holidays it names. A holiday that
/// reaches it fails `day_zero_is_an_ordinary_day`, so day 0, and every render
/// counted from it, moves only by an edit here.
const DAY_ZERO: jiff::civil::Date = jiff::civil::date(2026, 1, 2);

/// `day` counts FORWARD from day 0 by calendar arithmetic, not as a
/// day-of-month: a search that walks days to find a weather or moon phase runs
/// past 31, and `with_ymd_and_hms(.., 1, 40, ..)` is an invalid date that panics
/// in exactly the zones whose day-hash sequence needs the extra days.
fn try_local(day: u32, h: u32, m: u32) -> Option<SystemTime> {
    let date = DAY_ZERO
        .checked_add(jiff::Span::new().try_days(i64::from(day)).ok()?)
        .ok()?;
    local_instant(date, h, m)
}

/// The instant `date` reads `h:m` in the viewer's local time; `None` where
/// that local time is skipped or ambiguous.
pub(crate) fn local_instant(date: jiff::civil::Date, h: u32, m: u32) -> Option<SystemTime> {
    let time = jiff::civil::Time::new(i8::try_from(h).ok()?, i8::try_from(m).ok()?, 0, 0).ok()?;
    let local = date.to_datetime(time);
    let in_zone = |zone: TimeZone| zone.to_ambiguous_timestamp(local).unambiguous().ok();
    // A host's offset is per instant, so it is read where the local time read
    // as UTC lands, then again where that lands: once is an offset's width off
    // across a change.
    let first = in_zone(zone_at(in_zone(TimeZone::UTC)?))?;
    SystemTime::try_from(in_zone(zone_at(first))?).ok()
}

fn local(day: u32, h: u32, m: u32) -> SystemTime {
    try_local(day, h, m)
        .expect("a helper's time in the weeks after day 0 is valid and unambiguous in every zone")
}

/// Local `h:m` on `at`'s own local date, not day 0: a run on
/// today's sky. `None` where that local time doesn't exist.
pub(crate) fn on_date_of(at: SystemTime, h: u32, m: u32) -> Option<SystemTime> {
    local_instant(local_time(at).date(), h, m)
}

/// Local `h:00` on day 0.
pub fn at_hour(h: u32) -> SystemTime {
    local(0, h, 0)
}

/// Local `h:m` on day 0.
pub fn at_hour_min(h: u32, m: u32) -> SystemTime {
    local(0, h, m)
}

/// [`try_on_day`] where that local time exists.
#[cfg(test)]
pub(crate) fn on_day(day: u32, h: u32) -> SystemTime {
    local(day, h, 0)
}

/// Local `h:00`, `day` days after day 0 — weather and moon phase vary by
/// day at a fixed hour, so a search over days reaches different sky states. Any
/// `day` is valid, including past the end of the month; `None` where the date is
/// past jiff's calendar, `h` is no hour, or that local time is skipped or
/// ambiguous.
pub fn try_on_day(day: u32, h: u32) -> Option<SystemTime> {
    try_local(day, h, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the module: the same call names the same LOCAL hour
    /// the office then reads, which a raw epoch offset does not.
    #[test]
    fn every_helper_lands_on_the_local_hour_it_names() {
        for h in [0, 2, 5, 12, 17, 23] {
            let t = local_time(at_hour(h));
            assert_eq!(
                t.hour(),
                i8::try_from(h).expect("a test hour"),
                "at_hour({h}) landed on {}",
                t.hour()
            );
        }
        let t = local_time(at_hour_min(7, 30));
        assert_eq!((t.hour(), t.minute()), (7, 30));
        let t = local_time(on_day(8, 2));
        assert_eq!(
            (t.date(), t.hour()),
            (DAY_ZERO + jiff::Span::new().days(8), 2),
            "on_day counts days FORWARD from day 0"
        );
        // Past the end of day 0's month: the form this replaced panicked here.
        let t = local_time(on_day(45, 7));
        assert_eq!(
            (t.date(), t.hour()),
            (DAY_ZERO + jiff::Span::new().days(45), 7)
        );
        assert_ne!(t.month(), DAY_ZERO.month());
    }

    #[test]
    fn the_fallible_form_refuses_a_non_hour_or_day_and_agrees_otherwise() {
        assert_eq!(try_on_day(0, 24), None);
        assert_eq!(try_on_day(u32::MAX, 0), None);
        assert_eq!(try_on_day(45, 7), Some(on_day(45, 7)));
    }

    /// Twelve hours apart must stay twelve hours apart — the property the
    /// lighting suite's day-vs-night pair rests on.
    #[test]
    fn hours_are_ordered_and_spaced_as_named() {
        let (noon, night) = (at_hour(12), at_hour(0));
        assert_eq!(
            noon.duration_since(night).expect("noon is after midnight"),
            std::time::Duration::from_secs(12 * 3600)
        );
    }

    /// The unit tests read one fixed zone: the epoch reads `ZONE`'s offset on every runner.
    #[test]
    fn the_tests_read_one_fixed_zone() {
        let epoch = jiff::civil::date(1970, 1, 1);
        assert_eq!(
            local_time(SystemTime::UNIX_EPOCH),
            epoch.to_datetime(jiff::civil::time(5, 45, 0, 0))
        );
    }

    /// Day 0 is ordinary at every minute: no stand-in, no fireworks, no finale.
    #[test]
    fn day_zero_is_an_ordinary_day() {
        for minute in 0u32..24 * 60 {
            let (h, m) = (minute / 60, minute % 60);
            let t = at_hour_min(h, m);
            assert_eq!(
                crate::holiday::Occasion::at(t),
                crate::holiday::Occasion::default(),
                "{h}:{m:02}"
            );
            assert!(crate::holiday::fireworks_at(t).is_none(), "{h}:{m:02}");
            assert!(!crate::holiday::finale_at(t), "{h}:{m:02}");
        }
    }
}
