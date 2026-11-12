//! The flags the snapshot examples share, so each reads a clock or a size the
//! same way. Mechanism for `examples/`, not a contract.

use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

/// `--now-sec`'s bound: a second past it is the next hour's.
const HOUR_SECS: u64 = chrono::TimeDelta::hours(1).num_seconds().unsigned_abs();

/// The instant a snapshot draws: an hour, and a day and a second read only
/// beside it.
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct ClockArgs {
    /// Override local hour-of-day (0–23) used by time-of-day lighting.
    #[arg(long)]
    now_hour: Option<u32>,

    /// Override the local day (1 = `pixtuoid_scene::localclock`'s day 0) used by time-of-day.
    #[arg(long, default_value_t = 1, requires = "now_hour", value_parser = clap::value_parser!(u32).range(1..))]
    now_day: u32,

    /// Seconds past `--now-hour`, under an hour, fine enough to sample a
    /// weather transition.
    #[arg(long, default_value_t = 0, requires = "now_hour", value_parser = clap::value_parser!(u64).range(0..HOUR_SECS))]
    now_sec: u64,
}

impl ClockArgs {
    /// The local instant the flags name; `None` without `--now-hour`, where
    /// each snapshot keeps its own clock.
    ///
    /// # Errors
    ///
    /// When the day and hour name no one local time.
    pub fn instant(&self) -> Result<Option<SystemTime>> {
        let Some(h) = self.now_hour else {
            return Ok(None);
        };
        let at = pixtuoid_scene::localclock::try_on_day(self.now_day - 1, h)
            .with_context(|| format!("invalid --now-day/--now-hour {}:{h}", self.now_day))?;
        Ok(Some(at + Duration::from_secs(self.now_sec)))
    }
}

/// A `WxH` size: the cutaway's `--logical`, the floating snapshot's window.
///
/// # Errors
///
/// When `s` is not two `u16`s joined by `x`.
pub fn parse_size(s: &str) -> Result<(u16, u16), String> {
    let side = |n: &str| n.parse::<u16>().map_err(|e| format!("{n:?}: {e}"));
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| format!("{s:?}: want WxH"))?;
    Ok((side(w)?, side(h)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Flags {
        #[command(flatten)]
        clock: ClockArgs,
    }

    fn clock(flags: &[&str]) -> Result<ClockArgs, clap::Error> {
        Flags::try_parse_from([&["snapshot"], flags].concat()).map(|f| f.clock)
    }

    /// The day and the second are read only beside an hour, so either alone
    /// would draw the snapshot's own clock, unasked.
    #[test]
    fn the_day_and_the_second_need_an_hour_and_stay_in_range() {
        let parses = |flags: &[&str]| clock(flags).is_ok();
        let at_22 = |flag: &str, value: &str| parses(&["--now-hour", "22", flag, value]);
        assert!(parses(&[]));
        assert!(at_22("--now-day", "1"));
        assert!(!at_22("--now-day", "0"));
        assert!(!parses(&["--now-day", "5"]));
        assert!(at_22("--now-sec", &(HOUR_SECS - 1).to_string()));
        assert!(!at_22("--now-sec", &HOUR_SECS.to_string()));
        assert!(!parses(&["--now-sec", "30"]));
    }

    /// Day 1 is the clock's day 0, and the second lands past the hour.
    #[test]
    fn the_instant_is_the_hour_on_the_day_plus_the_second() {
        let flags = ["--now-hour", "22", "--now-day", "2", "--now-sec", "30"];
        let at = clock(&flags).unwrap().instant().unwrap();
        let hour = pixtuoid_scene::localclock::try_on_day(1, 22).unwrap();
        assert_eq!(at, Some(hour + Duration::from_secs(30)));
        assert_eq!(clock(&[]).unwrap().instant().unwrap(), None);
    }

    #[test]
    fn a_size_is_w_x_h() {
        assert_eq!(parse_size("960x640"), Ok((960, 640)));
        for bad in ["960", "x640", "960x", "-1x2", "960x640x2"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
    }
}
