//! The flags the snapshot examples share, so each reads a clock, a size or a
//! pet the same way. Mechanism for `examples/`, not a contract.

use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use pixtuoid_scene::pet::PetKind;

/// `--now-sec`'s exclusive bound: from it on, a second is the next hour's.
const HOUR_SECS: u64 = chrono::TimeDelta::hours(1).num_seconds().unsigned_abs();

/// The instant a snapshot draws: an hour, and a day and a second read only
/// beside it.
#[derive(Debug, Clone, Copy, clap::Args)]
// Else clap makes the doc above the about of a command that flattens this one
// and has none of its own.
#[command(about = None, long_about = None)]
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

/// Two `T`s joined by `sep`, the shape `want` names: a [`parse_size`], the
/// floating snapshot's window points.
///
/// # Errors
///
/// When `s` has no `sep`, or a side is no `T`.
pub fn parse_pair<T>(s: &str, sep: char, want: &str) -> Result<(T, T), String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let side = |n: &str| n.parse::<T>().map_err(|e| format!("{n:?}: {e}"));
    let (a, b) = s
        .split_once(sep)
        .ok_or_else(|| format!("{s:?}: want {want}"))?;
    Ok((side(a)?, side(b)?))
}

/// A `WxH` size: the cutaway's `--logical`, the floating snapshot's window.
///
/// # Errors
///
/// When `s` is not two `u16`s joined by `x`.
pub fn parse_size(s: &str) -> Result<(u16, u16), String> {
    parse_pair(s, 'x', "WxH")
}

/// One of `all` by its `name`, which clap's help and error list as the
/// possible values: [`pet_parser`]'s, the classic snapshot's looks.
pub fn named_parser<T: Copy + Send + Sync + 'static>(
    all: &'static [T],
    name: fn(T) -> &'static str,
) -> impl clap::builder::TypedValueParser<Value = T> {
    use clap::builder::TypedValueParser as _;
    clap::builder::PossibleValuesParser::new(all.iter().map(|&value| name(value))).try_map(
        move |given| {
            (all.iter().copied())
                .find(|&value| name(value) == given)
                .ok_or("each possible value is one of them")
        },
    )
}

/// A pet kind by its [`PetKind::config_name`], for the classic's `--pets` and
/// the floating snapshot's `--pet`.
pub fn pet_parser() -> impl clap::builder::TypedValueParser<Value = PetKind> {
    named_parser(PetKind::ALL, PetKind::config_name)
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

    #[test]
    fn the_clock_lends_no_about() {
        use clap::CommandFactory;
        assert_eq!(Flags::command().get_about(), None);
    }

    /// Day 1 is the clock's day 0, and the second lands past the hour.
    #[test]
    fn the_instant_is_the_hour_on_the_day_plus_the_second() {
        let flags = ["--now-hour", "22", "--now-day", "2", "--now-sec", "30"];
        let at = clock(&flags).unwrap().instant().unwrap();
        let hour = pixtuoid_scene::localclock::try_on_day(1, 22).unwrap();
        assert_eq!(at, Some(hour + Duration::from_secs(30)));
    }

    /// Each snapshot then keeps its own clock.
    #[test]
    fn no_hour_names_no_instant() {
        assert_eq!(clock(&[]).unwrap().instant().unwrap(), None);
    }

    #[derive(Parser)]
    struct Pets {
        #[arg(long, value_parser = pet_parser())]
        pet: PetKind,
    }

    /// The scene owns the names; this pins only the glue to them.
    #[test]
    fn the_pet_parser_maps_a_name_and_lists_the_kinds_it_refuses_for() {
        let pet = |name: &str| Pets::try_parse_from(["snapshot", "--pet", name]).map(|p| p.pet);
        let dog = PetKind::Dog;
        assert_eq!(pet(dog.config_name()).unwrap(), dog);
        let refused = pet("bird").unwrap_err().to_string();
        for kind in PetKind::ALL {
            assert!(refused.contains(kind.config_name()), "{refused}");
        }
    }

    #[test]
    fn a_size_is_w_x_h() {
        assert_eq!(parse_size("960x640"), Ok((960, 640)));
        for bad in ["960", "x640", "960x", "-1x2", "960x640x2"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
    }
}
