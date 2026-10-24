//! Local wall-clock instants for tests, benches and the snapshot examples.
//!
//! The sky model decodes `now` back through `chrono::Local`, so a test building
//! its instants as epoch offsets is really asking for whatever local hours the
//! runner's `$TZ` gives — how `the_foreground_layer_is_lit_by_the_clock` passed
//! in CI (UTC) and failed on a clean checkout in UTC-6.
//!
//! Anything whose ASSERTION depends on the hour builds its instants here; a test
//! needing only "some fixed moment" is TZ-independent already and keeps a plain
//! epoch offset.

use std::time::SystemTime;

use chrono::TimeZone;

/// Day 0 for every helper. January, so the sun arc is the same short winter one
/// in each call.
const BASE: (i32, u32, u32) = (2026, 1, 1);

/// `day` counts FORWARD from the base date by calendar arithmetic, not as a
/// day-of-month: a search that walks days to find a weather or moon phase runs
/// past 31, and `with_ymd_and_hms(.., 1, 40, ..)` is an invalid date that panics
/// in exactly the zones whose day-hash sequence needs the extra days.
fn try_local(day: u32, h: u32, m: u32) -> Option<SystemTime> {
    let date = chrono::NaiveDate::from_ymd_opt(BASE.0, BASE.1, BASE.2)?
        .checked_add_days(chrono::Days::new(u64::from(day)))?;
    resolve(date, h, m)
}

#[expect(
    clippy::disallowed_methods,
    reason = "the one sanctioned TimeZone construction clippy.toml bans elsewhere"
)]
fn resolve(date: chrono::NaiveDate, h: u32, m: u32) -> Option<SystemTime> {
    let local = chrono::Local.from_local_datetime(&date.and_hms_opt(h, m, 0)?);
    local.single().map(Into::into)
}

fn local(day: u32, h: u32, m: u32) -> SystemTime {
    try_local(day, h, m).expect("a fixed January local time is valid and unambiguous in every zone")
}

/// Local `h:m` on `day`'s own local date, not the reference day: a run on
/// today's sky. `None` where that local time doesn't exist.
pub(crate) fn on_date_of(day: SystemTime, h: u32, m: u32) -> Option<SystemTime> {
    resolve(
        chrono::DateTime::<chrono::Local>::from(day).date_naive(),
        h,
        m,
    )
}

/// Local `h:00` on the reference day.
pub fn at_hour(h: u32) -> SystemTime {
    local(0, h, 0)
}

/// Local `h:m` on the reference day.
pub fn at_hour_min(h: u32, m: u32) -> SystemTime {
    local(0, h, m)
}

/// [`try_on_day`] where that local time exists.
#[cfg(test)]
pub(crate) fn on_day(day: u32, h: u32) -> SystemTime {
    local(day, h, 0)
}

/// Local `h:00`, `day` days after the base date — weather and moon phase vary by
/// day at a fixed hour, so a search over days reaches different sky states. Any
/// `day` is valid, including past the end of the month; `None` where the date is
/// past chrono's calendar, `h` is no hour, or that local time is skipped or
/// ambiguous.
pub fn try_on_day(day: u32, h: u32) -> Option<SystemTime> {
    try_local(day, h, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the module: the same call must name the same LOCAL
    /// hour whatever `$TZ` is, which a raw epoch offset does not.
    #[test]
    fn every_helper_lands_on_the_local_hour_it_names() {
        use chrono::{Datelike, Timelike};
        for h in [0, 2, 5, 12, 17, 23] {
            let t: chrono::DateTime<chrono::Local> = at_hour(h).into();
            assert_eq!(t.hour(), h, "at_hour({h}) landed on {}", t.hour());
        }
        let t: chrono::DateTime<chrono::Local> = at_hour_min(7, 30).into();
        assert_eq!((t.hour(), t.minute()), (7, 30));
        let t: chrono::DateTime<chrono::Local> = on_day(8, 2).into();
        assert_eq!(
            (t.day(), t.hour()),
            (9, 2),
            "on_day counts days FORWARD from the base"
        );
        // Past the end of January: the form this replaced panicked here.
        let t: chrono::DateTime<chrono::Local> = on_day(45, 7).into();
        assert_eq!((t.month(), t.day(), t.hour()), (2, 15, 7));
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
}
