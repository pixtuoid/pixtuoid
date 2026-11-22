//! A host's offset stands in for jiff's system zone. Its own test binary: the
//! reading a host sets is the process's, so it would reach every other test.

use std::time::SystemTime;

use jiff::Timestamp;
use jiff::civil::date;
use jiff::tz::Offset;
use pixtuoid_scene::localclock::read_viewer_offset_with;
use pixtuoid_scene::sky::nightfall_on;
use pixtuoid_scene::tally::local_day;

fn stamp(s: &str) -> Timestamp {
    s.parse().expect("an RFC 3339 stamp")
}

fn at(s: &str) -> SystemTime {
    SystemTime::try_from(stamp(s)).expect("a stamp inside the system clock's range")
}

/// A zone east of UTC that moves from `+12:00` to `+13:00` at
/// 2026-09-26T14:00Z, so a local time read as UTC lands past a change the true
/// instant precedes.
fn springs_forward_east_of_utc(instant: Timestamp) -> Offset {
    let hours = if instant < stamp("2026-09-26T14:00:00Z") {
        12
    } else {
        13
    };
    Offset::from_hours(hours).expect("a real offset")
}

#[test]
fn the_office_reads_the_hosts_offset_at_each_instant_both_ways() {
    read_viewer_offset_with(springs_forward_east_of_utc);
    assert_eq!(local_day(at("2026-09-26T11:30:00Z")), date(2026, 9, 26));
    assert_eq!(local_day(at("2026-09-27T11:30:00Z")), date(2026, 9, 28));
    // Dusk on the evening before the change is in that evening's +12:00,
    // though 20:00 read as UTC lands after the change.
    assert_eq!(
        nightfall_on(at("2026-09-26T00:00:00Z")),
        Some(at("2026-09-26T08:00:00Z"))
    );
}
