//! The sky as a MODEL: the weather, the sun and the moon, lightning timing, and
//! the light they let into the office.
//!
//! Painters read it through one `Sky` sampled per frame, so no two of them can
//! disagree about the hour, the weather or whether lightning is striking.
//! Nothing here knows a theme or a pixel.

use std::time::{Duration, SystemTime};

use crate::anim::Motion;

#[cfg(test)]
mod tests;

/// The weather outside the office's windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weather {
    /// A clear sky.
    Clear,
    /// Rain.
    Rain,
    /// Rain with lightning.
    Storm,
    /// Snow.
    Snow,
    /// Fog.
    Fog,
    /// Cloud cover.
    Overcast,
    /// Wind-driven rain.
    Windy,
    /// Smog.
    Smog,
}

impl Weather {
    /// All variants, in canonical order. The site's gallery manifest
    /// (site/src/weather.json) mirrors it; `weather_gallery_manifest_matches_the_weather_enum`
    /// fails on any add/rename here until the manifest (+ gen-media art) follows.
    pub(crate) const ALL: [Weather; 8] = [
        Weather::Clear,
        Weather::Rain,
        Weather::Storm,
        Weather::Snow,
        Weather::Fog,
        Weather::Overcast,
        Weather::Windy,
        Weather::Smog,
    ];

    /// Lowercase CLI name (`Weather::Rain` → `"rain"`).
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Weather::Clear => "clear",
            Weather::Rain => "rain",
            Weather::Storm => "storm",
            Weather::Snow => "snow",
            Weather::Fog => "fog",
            Weather::Overcast => "overcast",
            Weather::Windy => "windy",
            Weather::Smog => "smog",
        }
    }

    /// Whether something falls: rain or snow.
    pub(crate) const fn falls(self) -> bool {
        matches!(
            self,
            Weather::Rain | Weather::Storm | Weather::Windy | Weather::Snow
        )
    }

    /// Parse a CLI name (case-insensitive) back to a variant.
    pub(crate) fn from_name(s: &str) -> Option<Weather> {
        let s = s.trim().to_ascii_lowercase();
        Weather::ALL.into_iter().find(|w| w.name() == s)
    }
}

/// Which weather a frame shows: an input of every frame, so two offices
/// rendered side by side each show their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WeatherPolicy {
    /// The clock's pick, one per `WEATHER_CYCLE_MS` slot, each slot's last
    /// `TRANSITION_MS` easing into the next one's.
    #[default]
    Clock,
    /// This weather, whatever the clock, never mixed.
    Forced(Weather),
}

impl WeatherPolicy {
    /// The policy a CLI or page names: a [`Weather`]'s name, case-insensitive,
    /// or `None` for the clock.
    ///
    /// # Errors
    ///
    /// If `name` is `Some` but no [`Weather`]'s name; the `Err` carries them all.
    pub fn from_name(name: Option<&str>) -> Result<Self, Vec<&'static str>> {
        match name {
            None => Ok(Self::Clock),
            Some(s) => Weather::from_name(s)
                .map(Self::Forced)
                .ok_or_else(weather_names),
        }
    }

    /// The weather at `now` under this policy.
    fn weather_at(self, now: SystemTime) -> WeatherMix {
        match self {
            Self::Clock => clock_weather(now),
            Self::Forced(w) => WeatherMix::pure(w),
        }
    }
    /// [`weather_at`](Self::weather_at) `ms` after the Unix epoch.
    pub(crate) fn weather_at_ms(self, ms: u64) -> WeatherMix {
        self.weather_at(SystemTime::UNIX_EPOCH + Duration::from_millis(ms))
    }
}

/// How long one weather holds before the next slot picks again.
pub(crate) const WEATHER_CYCLE_MS: u64 = 600_000;
/// How much of a slot's end the next slot's weather takes to come in, so every
/// slot opens on its own weather, pure.
pub(crate) const TRANSITION_MS: u64 = 120_000;
const _: () = assert!(TRANSITION_MS < WEATHER_CYCLE_MS);

/// What a transition moves, each across its own [`Stage`] of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Element {
    Cloud,
    /// What falls.
    Precipitation,
    /// Whether a storm's strikes fire.
    Lightning,
}

impl Element {
    #[cfg(test)]
    pub(crate) const ALL: [Element; 3] =
        [Element::Cloud, Element::Precipitation, Element::Lightning];
}

/// How a transition changes what falls, which orders its elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Course {
    /// From a dry sky into precipitation.
    Wetting,
    /// From other precipitation into a storm.
    Storming,
    /// Neither: dry to dry, or one precipitation to another.
    Shifting,
}

impl Course {
    /// The course `from → to` runs, and whether it runs it backward (drying,
    /// calming).
    fn of(from: Weather, to: Weather) -> (Course, bool) {
        match (from.falls(), to.falls()) {
            (false, true) => (Course::Wetting, false),
            (true, false) => (Course::Wetting, true),
            _ if to == Weather::Storm => (Course::Storming, false),
            _ if from == Weather::Storm => (Course::Storming, true),
            _ => (Course::Shifting, false),
        }
    }

    /// Its [`Stages`], run forward.
    fn stages(self) -> &'static Stages {
        match self {
            Course::Wetting => &WETTING,
            Course::Storming => &STORMING,
            Course::Shifting => &SHIFTING,
        }
    }
}

/// How far from either end of a transition precipitation settles, as a share
/// of it: a fall landing in the next slot shows by that slot's weather, so one
/// no longer than this began under it too.
const FALL_SETTLE: f32 = 0.1;
/// How far from either end of a transition lightning settles, as a share of it.
const LIGHTNING_SETTLE: f32 = 0.1;

/// A `(start, end)` span of a transition, as shares of it.
type Stage = (f32, f32);

/// One [`Course`]'s [`Stage`] per [`Element`].
struct Stages {
    cloud: Stage,
    precipitation: Stage,
    lightning: Stage,
}

impl Stages {
    fn of(&self, element: Element) -> Stage {
        match element {
            Element::Cloud => self.cloud,
            Element::Precipitation => self.precipitation,
            Element::Lightning => self.lightning,
        }
    }
}

/// The cloud gathers, then the rain falls, a storm's lightning last.
const WETTING: Stages = Stages {
    cloud: (0.0, 0.5),
    precipitation: (0.5, 1.0 - FALL_SETTLE),
    lightning: (0.7, 1.0 - LIGHTNING_SETTLE),
};

/// The rain heavies, then the lightning starts.
const STORMING: Stages = Stages {
    cloud: (0.0, 1.0),
    precipitation: (FALL_SETTLE, 0.5),
    lightning: (0.5, 1.0 - LIGHTNING_SETTLE),
};

/// Everything together.
const SHIFTING: Stages = Stages {
    cloud: (0.0, 1.0),
    precipitation: (FALL_SETTLE, 1.0 - FALL_SETTLE),
    lightning: (LIGHTNING_SETTLE, 1.0 - LIGHTNING_SETTLE),
};

/// The weather a frame shows: one weather, or, mid-transition, the next
/// slot's coming in over it, each [`Element`] eased across its own [`Stage`]
/// by [`Easing::Smoothstep`](crate::anim::Easing::Smoothstep).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WeatherMix {
    from: Weather,
    to: Weather,
    /// How far through the transition, `0..1`, linear in time; 0 when
    /// `from == to`.
    progress: f32,
}

impl WeatherMix {
    pub(crate) const fn pure(w: Weather) -> Self {
        Self {
            from: w,
            to: w,
            progress: 0.0,
        }
    }

    /// `from` giving way to `to`, `progress` of the way through the transition.
    pub(crate) fn toward(from: Weather, to: Weather, progress: f32) -> Self {
        if from == to {
            return Self::pure(from);
        }
        Self { from, to, progress }
    }

    /// The weather going and the weather coming; one weather twice when pure.
    pub(crate) fn ends(self) -> [Weather; 2] {
        [self.from, self.to]
    }

    /// Whether one weather holds, none coming in.
    pub(crate) fn is_pure(self) -> bool {
        self.from == self.to
    }

    /// `element`'s share of the incoming weather, `0..=1`.
    pub(crate) fn incoming(self, element: Element) -> f32 {
        if self.is_pure() {
            return 0.0;
        }
        let (course, backward) = Course::of(self.from, self.to);
        let (start, end) = course.stages().of(element);
        // Run backward, a stage mirrors, so rain stops before the sky clears
        // and lightning before the rain eases.
        let (start, end) = if backward {
            (1.0 - end, 1.0 - start)
        } else {
            (start, end)
        };
        crate::anim::Easing::Smoothstep.apply((self.progress - start) / (end - start))
    }

    /// Each weather the frame shows in `element`, with its share; the shares
    /// sum to 1.
    pub(crate) fn parts(self, element: Element) -> impl Iterator<Item = (Weather, f32)> {
        let incoming = self.incoming(element);
        [(self.from, 1.0 - incoming), (self.to, incoming)]
            .into_iter()
            .filter(|&(_, share)| share > 0.0)
    }

    /// How much of `element` is `w`'s, `0..=1`.
    pub(crate) fn share(self, element: Element, w: Weather) -> f32 {
        self.parts(element)
            .filter(|&(part, _)| part == w)
            .map(|(_, share)| share)
            .sum()
    }

    /// A per-weather preset value, lerped by `element`'s shares: a pure
    /// weather's exactly.
    pub(crate) fn lerp(self, element: Element, preset: impl Fn(Weather) -> f32) -> f32 {
        self.parts(element)
            .map(|(w, share)| preset(w) * share)
            .sum()
    }
}

/// The clock's weather at `now`: its slot's pick, with the next slot's coming
/// in over the slot's last [`TRANSITION_MS`].
fn clock_weather(now: SystemTime) -> WeatherMix {
    let ms = crate::anim::epoch_ms(now);
    let (slot, into) = (ms / WEATHER_CYCLE_MS, ms % WEATHER_CYCLE_MS);
    match into.checked_sub(WEATHER_CYCLE_MS - TRANSITION_MS) {
        None => WeatherMix::pure(slot_weather(slot)),
        Some(since) => WeatherMix::toward(
            slot_weather(slot),
            slot_weather(slot + 1),
            since as f32 / TRANSITION_MS as f32,
        ),
    }
}

/// The weather [`WEATHER_CYCLE_MS`] slot `slot` picks: one hashed draw.
fn slot_weather(slot: u64) -> Weather {
    match crate::splitmix_draw(slot, 1) % 15 {
        0..=5 => Weather::Clear,
        6..=7 => Weather::Rain,
        8 => Weather::Storm,
        9 => Weather::Snow,
        10 => Weather::Fog,
        11..=12 => Weather::Overcast,
        13 => Weather::Windy,
        _ => Weather::Smog,
    }
}

// Weights folding the two transmission channels into one interior illuminance,
// calibrated so a CLEAR noon lands at full brightness
// (`K_BEAM + transmission(Clear).diffuse · K_FILL ≈ 1`).
const K_BEAM: f32 = 0.70;
const K_FILL: f32 = 0.55;

/// City-light bounce reaching the interior at night — a small, weather-keyed
/// FLOOR so the room is never pitch black even at a new moon. Snow albedo bounces
/// the most; a storm swallows it. Independent of the moon's (date-varying) phase,
/// so the night weather-ordering is phase-stable.
fn city_bounce(w: Weather) -> f32 {
    // Magnitudes stay low enough that a moonlit night's floor can never
    // out-light a stormy solar noon (`solar_noon_outshines_the_brightest_night`).
    let v = match w {
        Weather::Snow => 0.08,
        Weather::Clear => 0.055,
        Weather::Windy => 0.05,
        Weather::Fog => 0.045,
        Weather::Smog => 0.045,
        Weather::Overcast => 0.035,
        Weather::Rain => 0.03,
        Weather::Storm => 0.015,
    };
    debug_assert!(
        (0.0..=1.0).contains(&v),
        "city_bounce out of range: {w:?} -> {v}"
    );
    v
}

/// How hard `w` rains, 0 dry .. 1 storm: precipitation you can HEAR, so snow
/// and fog are 0.
fn rain_level(w: Weather) -> f32 {
    // The gap to Storm is an audible "getting heavier", not a new mix profile.
    const RAIN_LEVEL: f32 = 0.6;
    match w {
        Weather::Storm => 1.0,
        Weather::Rain => RAIN_LEVEL,
        _ => 0.0,
    }
}

/// How much of the sky body's light the weather lets through to the interior:
/// a hard directional beam, a flat diffuse fill, and the disc's own visibility.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Transmission {
    pub(crate) direct: f32,
    pub(crate) diffuse: f32,
    pub(crate) disc: f32,
}

pub(crate) fn transmission(w: Weather) -> Transmission {
    // Storm < Rain in BOTH transmission channels (denser cloud), and
    // Overcast/Rain/Storm share one near-zero disc (below `MIN_DISC_VIS`) so a
    // thicker cloud never shows MORE of the disc than a thinner one.
    let (direct, diffuse, disc) = match w {
        Weather::Clear => (1.00, 0.55, 1.00),
        Weather::Windy => (0.90, 0.55, 0.95),
        Weather::Snow => (0.25, 0.70, 0.30),
        Weather::Smog => (0.30, 0.45, 0.45),
        Weather::Fog => (0.05, 0.75, 0.10),
        Weather::Overcast => (0.00, 0.50, 0.05),
        Weather::Rain => (0.00, 0.40, 0.05),
        Weather::Storm => (0.00, 0.28, 0.05),
    };
    debug_assert!(
        [direct, diffuse, disc]
            .iter()
            .all(|c| (0.0..=1.0).contains(c)),
        "Transmission channels must be 0..=1: {w:?} -> ({direct}, {diffuse}, {disc})"
    );
    Transmission {
        direct,
        diffuse,
        disc,
    }
}

/// Which body the sky shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyKind {
    Sun,
    Moon,
}

/// The sky body — sun by day, moon by night (at its rim, lighting
/// nothing, while it is below the horizon). Luminance + warmth follow altitude
/// (low body = longer air path = dimmer + warmer). The ONE source the interior
/// light, the disc and the spill derive from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SkyBody {
    pub(crate) kind: BodyKind,
    /// 0 horizon .. 1 apex.
    pub(crate) altitude: f32,
    /// Progress along this body's arc: 0 as it rises .. 1 as it sets.
    pub(crate) azimuth: f32,
    /// 0 neutral (apex) .. 1 warm/red (horizon).
    pub(crate) warmth: f32,
    /// 0..1 luminance reaching the atmosphere.
    pub(crate) lum: f32,
}

// The sun rides the arc over its up-span; the moon's span moves with its age
// (`moon_arc`), and a full moon's is the night.
const SUN_RISE_H: f32 = 5.0;
const SUN_SET_H: f32 = 20.0;
/// How long a moon stays up: the night's length, so a full moon rises at dusk
/// and sets at dawn.
const MOON_UP_H: f32 = SUN_RISE_H + 24.0 - SUN_SET_H;
/// How long, after dusk and before dawn, the night takes to come in and to go:
/// the moon's light, the city's glow and the stars ride [`nightfall`] up and
/// down across it rather than switching with the body.
const NIGHTFALL_H: f32 = 0.75;
/// Moon luminance at a full phase — low enough that a full-moon midnight (plus
/// the `city_bounce` floor) still stays dimmer than a stormy solar noon; see
/// `solar_noon_outshines_the_brightest_night`.
const MOON_PEAK_LUM: f32 = 0.12;
/// The mean synodic month: NASA GSFC, "Phases of the Moon 2001 to 2100"
/// (<https://eclipse.gsfc.nasa.gov/phase/phases2001.html>).
const SYNODIC_DAYS: f32 = 29.530_588;
/// The MEAN new moon of 2019-11-27 02:56 TT, in unix days (Meeus, *Astronomical
/// Algorithms*, eq. 49.1, k = 246): a true new moon would carry its own
/// lunation's offset from the mean into every phase.
const NEW_MOON_EPOCH_UNIX_DAYS: f32 = 18_227.123;

fn arc_progress(h: f32, rise: f32, set: f32) -> f32 {
    ((h - rise) / (set - rise)).clamp(0.0, 1.0)
}

/// Whether the sky shows the SUN (not the moon) at hour-of-day `h` (0..24) — the
/// ONE definition of the day/night boundary, so the sky body and any external
/// consumer can't drift from a second hardcoded copy.
pub fn hour_is_day(h: f32) -> bool {
    (SUN_RISE_H..SUN_SET_H).contains(&h)
}

/// The weather names [`WeatherPolicy::from_name`] accepts, canonical order.
pub(crate) fn weather_names() -> Vec<&'static str> {
    Weather::ALL.iter().map(|w| w.name()).collect()
}

/// How hard it is raining at `now` under `policy` (0.0 dry … 1.0 storm; snow
/// and fog are 0.0): the audio model's weather feed.
pub(crate) fn rain_at(now: SystemTime, policy: WeatherPolicy) -> f32 {
    Sky::at(crate::anim::Motion::Full.timing(now), policy).rain()
}

/// What a wall clock reads at `now`, local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ClockReading {
    /// On a twelve-hour dial.
    pub(crate) hour: u32,
    pub(crate) minute: u32,
}

impl ClockReading {
    /// The hour and the minute hands, as turns from twelve o'clock.
    pub(crate) fn turns(self) -> (f32, f32) {
        let (hour, minute) = (self.hour as f32, self.minute as f32);
        ((hour + minute / 60.0) / 12.0, minute / 60.0)
    }
}

/// Its own decode, not [`local_hour_frac`]: the hands need the raw
/// `hour % 12` and `minute`.
pub(crate) fn clock_reading(now: SystemTime) -> ClockReading {
    let unix_now = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let local = chrono::DateTime::<chrono::Local>::from(std::time::UNIX_EPOCH + unix_now);
    use chrono::Timelike;
    ClockReading {
        hour: local.hour() % 12,
        minute: local.minute(),
    }
}

/// Quantize a fractional turn (0.0..1.0, 0.0 = north) to one of 8 octant
/// (dx, dy) unit offsets.
pub(crate) fn octant_offset(turn: f32) -> (i32, i32) {
    // rem_euclid(8) maps every i32 (incl. a NaN turn's 0 cast) into 0..=7, so
    // the table is total — a match would need a dead wildcard arm.
    const OCTANTS: [(i32, i32); 8] = [
        (0, -1),
        (1, -1),
        (1, 0),
        (1, 1),
        (0, 1),
        (-1, 1),
        (-1, 0),
        (-1, -1),
    ];
    let oct = ((turn * 8.0).round() as i32).rem_euclid(8);
    OCTANTS[oct as usize]
}

/// Fractional local hour (`hour + minute/60`, in `0.0..24.0`) for `now` — the
/// sky's clock decode.
pub(crate) fn local_hour_frac(now: SystemTime) -> f32 {
    use chrono::Timelike;
    let unix_now = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let local = chrono::DateTime::<chrono::Local>::from(std::time::UNIX_EPOCH + unix_now);
    local.hour() as f32 + local.minute() as f32 / 60.0
}

/// When nightfall begins on `day`'s local date: where `just pace-check --run
/// dusk` starts its clock. `None` where that local time doesn't exist.
#[doc(hidden)]
pub fn nightfall_on(day: SystemTime) -> Option<SystemTime> {
    let minutes = (SUN_SET_H * 60.0).round() as u32;
    crate::localclock::on_date_of(day, minutes / 60, minutes % 60)
}

/// How far into the night hour `h` is, 0..=1: zero by day, rising over
/// [`NIGHTFALL_H`] after dusk and falling over it before dawn.
fn nightfall(h: f32) -> f32 {
    if hour_is_day(h) {
        return 0.0;
    }
    let since_dusk = (h - SUN_SET_H).rem_euclid(24.0);
    let until_dawn = (SUN_RISE_H - h).rem_euclid(24.0);
    (since_dusk.min(until_dawn) / NIGHTFALL_H).min(1.0)
}

/// The moon's progress along its arc at hour `h`, 0 as it rises .. 1 as it
/// sets, or `None` while it is down. A full moon rises at dusk; each day of
/// its `age` puts the rise a synodic share of the day later.
fn moon_arc(h: f32, age: f32) -> Option<f32> {
    let rise = SUN_SET_H + 24.0 * (age / SYNODIC_DAYS - 0.5);
    let t = (h - rise).rem_euclid(24.0) / MOON_UP_H;
    (t <= 1.0).then_some(t)
}

/// The body the sky shows at local hour `h`, `nightfall` into the night.
fn body_at(h: f32, nightfall: f32, moon_phase: f32, moon_age: f32) -> SkyBody {
    if hour_is_day(h) {
        let t = arc_progress(h, SUN_RISE_H, SUN_SET_H);
        let altitude = (std::f32::consts::PI * t).sin();
        return SkyBody {
            kind: BodyKind::Sun,
            altitude,
            azimuth: t,
            warmth: (1.0 - altitude).clamp(0.0, 1.0),
            lum: altitude,
        };
    }
    let t = moon_arc(h, moon_age).unwrap_or(0.0);
    let altitude = (std::f32::consts::PI * t).sin();
    SkyBody {
        kind: BodyKind::Moon,
        altitude,
        azimuth: t,
        warmth: (1.0 - altitude).clamp(0.0, 1.0),
        lum: MOON_PEAK_LUM * altitude * moon_phase * nightfall,
    }
}

/// Days into the mean lunation at `now`: 0 at new moon, half a synodic month at
/// full.
fn moon_age_at(now: SystemTime) -> f32 {
    let unix_days = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f32() / 86_400.0)
        .unwrap_or(0.0);
    (unix_days - NEW_MOON_EPOCH_UNIX_DAYS).rem_euclid(SYNODIC_DAYS)
}

/// Illuminated fraction of the moon (0 new .. 1 full), from the synodic month.
fn moon_phase_at(now: SystemTime) -> f32 {
    (1.0 - (std::f32::consts::TAU * moon_age_at(now) / SYNODIC_DAYS).cos()) / 2.0
}

/// Lightning cadence: one strike per bucket this long, at a hashed offset
/// ([`strike_offset`]) — a much faster cadence reads as a hyperactive storm.
const LIGHTNING_PERIOD_MS: u64 = 15000;
// A slot's start is a bucket's start on every moving tier's loop clock, so no
// strike runs into the next slot's weather.
const _: () = {
    let mut i = 0;
    while i < Motion::ALL.len() {
        if let Some(pace) = Motion::ALL[i].pace() {
            assert!(WEATHER_CYCLE_MS.is_multiple_of(crate::anim::FULL_TICK_MS * pace));
            assert!((WEATHER_CYCLE_MS / pace).is_multiple_of(LIGHTNING_PERIOD_MS));
        }
        i += 1;
    }
};
/// How long each of [`STRIKE`]'s phases holds: a whole Full beat, so the beat
/// neither skips one nor stretches it.
const STRIKE_PHASE_MS: u64 = crate::anim::FULL_TICK_MS;
const _: () = assert!(
    STRIKE_PHASE_MS >= crate::anim::PHOTOSENSITIVE_PHASE_MIN_MS
        && STRIKE_GAP_MS >= crate::anim::PHOTOSENSITIVE_PHASE_MIN_MS
);
/// A strike's phases in order, each held [`STRIKE_PHASE_MS`]: the primary
/// strike, a brief dim, an after-flash, so it reads as a flicker rather than a
/// single blink.
const STRIKE: [StrikePhase; 3] = [StrikePhase::Primary, StrikePhase::Dim, StrikePhase::After];

/// One phase of a strike's flicker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrikePhase {
    Primary,
    Dim,
    After,
}

impl StrikePhase {
    /// The flash it lights the sky with.
    pub(crate) const fn level(self) -> f32 {
        match self {
            StrikePhase::Primary => 1.0,
            StrikePhase::Dim => 0.15,
            StrikePhase::After => 0.55,
        }
    }
}
/// How long one strike's [`STRIKE`] phases last together.
const STRIKE_MS: u64 = STRIKE.len() as u64 * STRIKE_PHASE_MS;
/// The least dark time between one strike's end and the next's start, so two
/// strikes never put more than three flashes in a second.
const STRIKE_GAP_MS: u64 = 1000;

/// The phase of a strike `since_strike_ms` after it began: [`STRIKE`]'s in
/// turn, then none.
fn strike_phase(since_strike_ms: u64) -> Option<StrikePhase> {
    usize::try_from(since_strike_ms / STRIKE_PHASE_MS)
        .ok()
        .and_then(|i| STRIKE.get(i).copied())
}

/// Intensity envelope (0..1) of a lightning flash given ms since the strike
/// began: its phases' levels in turn, then 0.
#[cfg(test)]
fn lightning_envelope(since_strike_ms: u64) -> f32 {
    strike_phase(since_strike_ms).map_or(0.0, StrikePhase::level)
}

/// Per-bucket strike offset (ms into the bucket) so strikes don't fire on a
/// fixed metronome. Each `LIGHTNING_PERIOD_MS`-long bucket hashes to its own
/// offset, leaving the flash and [`STRIKE_GAP_MS`] after it inside the
/// bucket, so the next bucket's strike is never too close. It lands on a
/// phase boundary, so each phase holds whole beats.
fn strike_offset(bucket: u64) -> u64 {
    let off = crate::splitmix_draw(bucket, 1) % (LIGHTNING_PERIOD_MS - STRIKE_MS - STRIKE_GAP_MS);
    off / STRIKE_PHASE_MS * STRIKE_PHASE_MS
}

/// Whether `bucket`'s strike fires under a sky `storm` of storm, `0..=1`: a
/// fixed draw per bucket against the share, so a storm coming in only adds
/// strikes and never moves one.
fn strikes(bucket: u64, storm: f32) -> bool {
    let draw = crate::splitmix_draw(bucket, 2) >> (u64::BITS - f32::MANTISSA_DIGITS);
    (draw as f32) < storm * (1u64 << f32::MANTISSA_DIGITS) as f32
}

/// The first instant from `after` at which a full storm lights a strike under
/// `motion`, or `None` at rest: where the frame-pacing bench places its window.
#[doc(hidden)]
pub fn first_strike_after(after: SystemTime, motion: Motion) -> Option<SystemTime> {
    let pace = motion.pace()?;
    let storm = WeatherPolicy::Forced(Weather::Storm);
    // Two periods: the strike of the bucket `after` falls in can be behind it,
    // and the next bucket's can land as late as its end.
    (0..=2 * LIGHTNING_PERIOD_MS * pace / crate::anim::FULL_TICK_MS)
        .map(|n| after + Duration::from_millis(n * crate::anim::FULL_TICK_MS))
        .find(|&t| strike_phase_at(motion.beat(t), storm).is_some())
}

/// The start of the first clock-weather transition from `after` that changes
/// the weather: where the frame-pacing bench places its window.
#[doc(hidden)]
pub fn first_transition_after(after: SystemTime) -> Option<SystemTime> {
    transition_after(after, |_| true)
}

/// [`first_transition_after`] into `into`: where `just pace-check` starts
/// its clock, so a run crosses a transition and then the weather after it.
#[doc(hidden)]
pub fn first_transition_into(after: SystemTime, into: Weather) -> Option<SystemTime> {
    transition_after(after, |to| to == into)
}

fn transition_after(after: SystemTime, wanted: impl Fn(Weather) -> bool) -> Option<SystemTime> {
    let ms = u64::try_from(
        after
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()?
            .as_millis(),
    )
    .ok()?;
    (ms / WEATHER_CYCLE_MS..)
        .take(TRANSITIONS_SEARCHED)
        .map(|slot| (slot + 1) * WEATHER_CYCLE_MS - TRANSITION_MS)
        .filter(|&start| start >= ms)
        .find(|&start| {
            let mid = WeatherPolicy::Clock.weather_at_ms(start + TRANSITION_MS / 2);
            mid.from != mid.to && wanted(mid.to)
        })
        .map(|start| SystemTime::UNIX_EPOCH + Duration::from_millis(start))
}

/// The slots a transition is searched over: weeks of weather.
const TRANSITIONS_SEARCHED: usize = 4096;

/// The lightning bucket `beat` falls in: what seeds its strike's look.
pub(crate) fn strike_bucket(beat: crate::anim::Beat) -> u64 {
    beat.ms() / LIGHTNING_PERIOD_MS
}

/// When the strike of the bucket `beat` falls in starts, in `beat`'s ms.
pub(crate) fn strike_start_ms(beat: crate::anim::Beat) -> u64 {
    let bucket = strike_bucket(beat);
    bucket * LIGHTNING_PERIOD_MS + strike_offset(bucket)
}

/// The phase of the strike `beat` falls in under `policy`, or `None` when not
/// mid-strike or at rest.
fn strike_phase_at(beat: crate::anim::Beat, policy: WeatherPolicy) -> Option<StrikePhase> {
    if beat.is_rest() {
        return None;
    }
    let elapsed_ms = beat.ms();
    let bucket = strike_bucket(beat);
    let strike_ms = strike_start_ms(beat);
    let since = elapsed_ms
        .checked_sub(strike_ms)
        .filter(|&since| since < STRIKE_MS)?;
    // The storm's share at the strike's start: the share moving mid-strike
    // would otherwise cut its phases short of `STRIKE_PHASE_MS`.
    let storm = policy
        .weather_at_ms(beat.wall_ms(strike_ms))
        .share(Element::Lightning, Weather::Storm);
    strikes(bucket, storm)
        .then(|| strike_phase(since))
        .flatten()
}

/// The light the sky lets into the office: the interior illuminance and what
/// the window glass shows outside.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InteriorLight {
    /// The sun's or moon's light reaching the interior through the atmosphere, 0..=1.
    pub(crate) interior: f32,
    /// The glass's daylight: the interior plus the night's city-light minimum, 0..=1.
    pub(crate) exterior: f32,
}

/// The sky at one instant, sampled once per frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Sky {
    policy: WeatherPolicy,
    weather: WeatherMix,
    body: SkyBody,
    moon_phase: f32,
    moon_waxing: bool,
    nightfall: f32,
    strike: Option<StrikePhase>,
}

impl Sky {
    pub(crate) fn at(timing: crate::anim::Timing, policy: WeatherPolicy) -> Self {
        let now = timing.now;
        let (moon_phase, moon_age) = (moon_phase_at(now), moon_age_at(now));
        let h = local_hour_frac(now);
        let nightfall = nightfall(h);
        let weather = policy.weather_at(now);
        let strike = strike_phase_at(timing.beat, policy);
        tracing::trace!(weather = ?weather, strike = ?strike, nightfall, "sky.at");
        Self {
            policy,
            weather,
            body: body_at(h, nightfall, moon_phase, moon_age),
            moon_phase,
            moon_waxing: moon_age < SYNODIC_DAYS / 2.0,
            nightfall,
            strike,
        }
    }

    /// The sky at `now` under `weather`, whatever the clock picks.
    #[cfg(test)]
    pub(crate) fn at_with(now: SystemTime, weather: Weather) -> Self {
        Self::at(Motion::Full.timing(now), WeatherPolicy::Forced(weather))
    }

    /// The sky at `now` under the clock's weather.
    #[cfg(test)]
    pub(crate) fn clock(now: SystemTime) -> Self {
        Self::at(Motion::Full.timing(now), WeatherPolicy::Clock)
    }

    /// This sky mid-`strike` — a painter test's strike without the clock
    /// arithmetic that places one.
    #[cfg(test)]
    pub(crate) fn with_strike(self, strike: Option<StrikePhase>) -> Self {
        Self { strike, ..self }
    }

    /// This sky under `weather` — a painter test's transition without the
    /// clock arithmetic that places one. Its [`policy`](Self::policy) stays
    /// as it was, so a `GlassWeather` built from it on a moving beat follows
    /// the policy, not `weather`.
    #[cfg(test)]
    pub(crate) fn with_weather(self, weather: WeatherMix) -> Self {
        Self { weather, ..self }
    }

    pub(crate) fn weather(&self) -> WeatherMix {
        self.weather
    }

    /// The policy this sky was sampled under: the weather at any other instant.
    pub(crate) fn policy(&self) -> WeatherPolicy {
        self.policy
    }

    pub(crate) fn body(&self) -> &SkyBody {
        &self.body
    }

    pub(crate) fn transmission(&self) -> Transmission {
        let channel = |of: fn(Transmission) -> f32| {
            self.weather.lerp(Element::Cloud, |w| of(transmission(w)))
        };
        Transmission {
            direct: channel(|t| t.direct),
            diffuse: channel(|t| t.diffuse),
            disc: channel(|t| t.disc),
        }
    }

    /// [`moon_phase_at`] at this instant.
    pub(crate) fn moon_phase(&self) -> f32 {
        self.moon_phase
    }

    /// Whether the moon is waxing (new to full) rather than waning at this
    /// instant: the side its lit limb faces.
    pub(crate) fn moon_waxing(&self) -> bool {
        self.moon_waxing
    }

    /// [`nightfall`] at this instant.
    pub(crate) fn nightfall(&self) -> f32 {
        self.nightfall
    }

    /// The lightning envelope at this instant: its strike's level, or 0.
    pub(crate) fn flash(&self) -> f32 {
        self.strike.map_or(0.0, StrikePhase::level)
    }

    /// The phase of the strike at this instant, if one is under way.
    pub(crate) fn strike(&self) -> Option<StrikePhase> {
        self.strike
    }

    /// [`rain_level`] under this sky's weather.
    pub(crate) fn rain(&self) -> f32 {
        self.weather.lerp(Element::Precipitation, rain_level)
    }

    pub(crate) fn light(&self) -> InteriorLight {
        let body = &self.body;
        let through = self.transmission();
        // The moon casts no USABLE direct beam — a moonlit night must never
        // out-light a cloudy solar noon, so the moon's illuminance is
        // diffuse-fill only.
        let direct = match body.kind {
            BodyKind::Sun => through.direct,
            BodyKind::Moon => 0.0,
        };
        let interior = (body.lum * (direct * K_BEAM + through.diffuse * K_FILL)).clamp(0.0, 1.0);
        let night_min = self.weather.lerp(Element::Cloud, city_bounce) * self.nightfall;
        InteriorLight {
            interior,
            exterior: (interior + night_min).min(1.0),
        }
    }
}
