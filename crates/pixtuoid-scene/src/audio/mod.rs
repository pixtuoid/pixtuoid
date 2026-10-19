//! Backend-agnostic ambient-audio MODEL: the scene emits semantic stem levels,
//! one-shot events and what lofi's music is asked, and each painter's audio
//! system renders them its own way.
//! NO audio dependencies live in this crate — the crate boundary is the
//! compiler tooth, exactly like the terminal/window ban.

// The office's synthesis — pooled foley, the bank that orders every build,
// runtime mixer/schedulers. Shared so the native device gateway and the wasm
// WebAudio painter build the SAME sample buffers.
#[doc(hidden)]
pub mod bank;
#[doc(hidden)]
pub mod engine;
pub(crate) mod foley;
#[doc(hidden)]
pub mod mixer;

// The shared per-tick engine surface — both audio painters build on these, so
// they can't drift.
pub use bank::OneShotPool;
pub use engine::{AudioEngine, MAX_DT_S, MusicCmd, PlayCmd, TickCommands};
// The music, so a painter never names lofi.
pub use pixtuoid_lofi::{Controls, Frame, Performance};

use crate::tally::StateCounts;

// `AudioEngine::new`'s seeds for the per-tick keystroke / rain-drop schedulers
// and their picker.
pub(crate) const TYPING_SEED: u64 = 0xBEEF;
pub(crate) const DROP_SEED: u64 = 0xFACE;
pub(crate) const PICK_SEED: u64 = 0xDEAD;

/// The rate every synthesized buffer is tagged with; each sink resamples to its
/// device's or AudioContext's own rate.
pub const SAMPLE_RATE: u32 = pixtuoid_lofi::dsp::SAMPLE_RATE;

/// The peak each sink's limiter holds its summed bus under, after EBU R 128
/// (<https://tech.ebu.ch/docs/r/r128.pdf>): "the True Peak Level of a programme
/// shall not exceed −1 dBTP". Both sinks hold it on SAMPLE peaks, since a true
/// peak needs oversampling, so a peak between two samples can still pass it.
pub const CEILING_DBFS: f32 = -1.0;

/// [`CEILING_DBFS`] as a linear amplitude: what each sink's clamp holds.
#[must_use]
pub fn ceiling_amplitude() -> f32 {
    10f32.powf(CEILING_DBFS / 20.0)
}

/// How fast each sink's limiter pulls its gain down: whatever rises faster
/// reaches the clamp behind it, which hard-clips it.
pub const LIMITER_ATTACK: std::time::Duration = std::time::Duration::from_millis(5);

/// How fast each sink's limiter lets its gain back up, so one loud hit never
/// ducks the next.
pub const LIMITER_RELEASE: std::time::Duration = std::time::Duration::from_millis(100);

/// Active-agent count at which the office reads BUSY (full band, dense typing).
/// 1..BUSY_ACTIVE_MIN is the moderate anchor tier; 0 is empty.
const BUSY_ACTIVE_MIN: usize = 3;

/// The rain stem's gain at full precipitation — tuned well under the music,
/// or the broadband rain masks the mix. The drop one-shots derive from
/// this via `DROP_GAIN × wanted.rain`.
const RAIN_GAIN: f32 = 0.30;

/// How busy the office is: what picks the typing level and the music's
/// intensity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Empty,
    Moderate,
    Busy,
}

impl Tier {
    /// How much typing this tier holds: the scale on
    /// [`mixer::TypingScheduler`]'s burst rate.
    const fn typing_level(self) -> f32 {
        match self {
            Tier::Empty => 0.0,
            Tier::Moderate => 0.50,
            Tier::Busy => 0.80,
        }
    }

    /// lofi's intensity at this tier: 0 is its empty room, 1 its busy one,
    /// and its default the Moderate anchor.
    fn intensity(self) -> f32 {
        match self {
            Tier::Empty => 0.0,
            Tier::Moderate => Controls::default().intensity,
            Tier::Busy => 1.0,
        }
    }
}

/// Target mix levels (0..=1) for the foley's stems, derived once per frame. `typing`
/// is a PROCEDURAL stem: the consumer owns burst scheduling; the scene only says
/// how much typing the office holds.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StemLevels {
    pub rain: f32,
    pub typing: f32,
}

/// A fire-once audio event, emitted on state EDGES by the cue tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneShot {
    DoorChime,
    PrinterWhir,
    VendingDrop,
}

/// One frame of audio intent: target stem levels, the events that fired, and
/// what the music is asked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioFrame {
    pub stems: StemLevels,
    pub events: Vec<OneShot>,
    pub music: Controls,
}

/// Cross-frame cue state: diffs identity/occupancy sets frame-to-frame and
/// emits each [`OneShot`] exactly once on the EDGE. The FIRST observe only
/// primes — attaching to a full office must not fire a door-chime volley.
#[derive(Debug, Default)]
pub(crate) struct AudioCueTracker {
    primed: bool,
    seen_agents: std::collections::HashSet<pixtuoid_core::AgentId>,
    occupied: std::collections::HashSet<usize>,
}

impl AudioCueTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed one frame's observations; returns the events that fired on this
    /// frame's edges. `waypoint_kind` resolves an occupied-waypoint index to its
    /// kind so the tracker never holds a `SceneLayout` borrow and tests need no
    /// layout at all. Purely EDGE-triggered — it takes no clock, so a caller
    /// can't read it as time-dependent.
    pub(crate) fn observe<'a>(
        &mut self,
        agent_ids: impl IntoIterator<Item = &'a pixtuoid_core::AgentId>,
        occupied_waypoints: &std::collections::HashSet<usize>,
        waypoint_kind: impl Fn(usize) -> Option<crate::layout::WaypointKind>,
    ) -> Vec<OneShot> {
        use crate::layout::WaypointKind;

        let ids: std::collections::HashSet<pixtuoid_core::AgentId> =
            agent_ids.into_iter().cloned().collect();

        if !self.primed {
            self.primed = true;
            self.seen_agents = ids;
            self.occupied = occupied_waypoints.clone();
            return Vec::new();
        }

        let mut events = Vec::new();

        // Capped at ONE per frame — a workflow fleet arriving together is one
        // door moment, not a chime chord.
        if ids.difference(&self.seen_agents).next().is_some() {
            events.push(OneShot::DoorChime);
        }
        self.seen_agents = ids;

        // A waypoint BECOMING occupied is the moment the matching feedback
        // animation starts — `sim::resolve_characters` keys its anims on this same set.
        for &idx in occupied_waypoints.difference(&self.occupied) {
            match waypoint_kind(idx) {
                Some(WaypointKind::Printer) => events.push(OneShot::PrinterWhir),
                Some(WaypointKind::VendingMachine) => events.push(OneShot::VendingDrop),
                _ => {}
            }
        }
        self.occupied = occupied_waypoints.clone();

        events
    }
}

/// How busy `counts` make the office.
fn tier(counts: &StateCounts) -> Tier {
    if counts.active >= BUSY_ACTIVE_MIN {
        Tier::Busy
    } else if counts.active >= 1 {
        Tier::Moderate
    } else {
        Tier::Empty
    }
}

/// Map the office's activity + weather onto target stem levels, with rain
/// scaling on the precipitation scalar (0 clear … 1 storm).
pub fn stem_levels(counts: &StateCounts, precipitation: f32) -> StemLevels {
    let t = tier(counts);
    StemLevels {
        rain: RAIN_GAIN * precipitation.clamp(0.0, 1.0),
        typing: t.typing_level(),
    }
}

/// How far ahead of the listener each painter renders the music: as long as
/// its producer may go unscheduled before the music drops out. Nothing waits
/// on it, since the gain sits after it and the controls latch at a bar.
pub const MUSIC_AHEAD: std::time::Duration = std::time::Duration::from_secs(1);

/// What the music is asked under `counts`, the sun's `nightfall` (0 by day …
/// 1 by night) and `rain` (0 dry … 1 storm). The sun's alone: a
/// weather-dimmed light would flip a storm's songs to the night family.
pub fn music_controls(counts: &StateCounts, nightfall: f32, rain: f32) -> Controls {
    Controls::default()
        .with_daylight(1.0 - nightfall)
        .with_intensity(tier(counts).intensity())
        .with_rain(rain)
}

/// The seed a music stream starting at `now` plays: whole seconds since the
/// UNIX epoch, so each launch plays its own run of songs. Pre-epoch clocks
/// read as 0.
pub fn music_seed(now: std::time::SystemTime) -> u64 {
    now.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(active: usize) -> StateCounts {
        StateCounts {
            active,
            waiting: 0,
            idle: 0,
            exiting: 0,
            total: active,
        }
    }

    #[test]
    fn stem_levels_typing_scales_with_active_agents() {
        assert_eq!(stem_levels(&counts(0), 0.0).typing, 0.0);
        assert_eq!(
            stem_levels(&counts(1), 0.0).typing,
            Tier::Moderate.typing_level()
        );
        assert_eq!(
            stem_levels(&counts(BUSY_ACTIVE_MIN - 1), 0.0).typing,
            Tier::Moderate.typing_level()
        );
        assert_eq!(
            stem_levels(&counts(BUSY_ACTIVE_MIN), 0.0).typing,
            Tier::Busy.typing_level()
        );
    }

    #[test]
    fn stem_levels_rain_tracks_precipitation() {
        assert_eq!(stem_levels(&counts(0), 0.0).rain, 0.0);
        assert_eq!(stem_levels(&counts(0), 1.0).rain, RAIN_GAIN);
        let half = stem_levels(&counts(0), 0.5).rain;
        assert!((half - RAIN_GAIN * 0.5).abs() < 1e-6);
        assert_eq!(stem_levels(&counts(0), -1.0).rain, 0.0);
        assert_eq!(stem_levels(&counts(0), 2.0).rain, RAIN_GAIN);
    }

    use crate::layout::WaypointKind;
    use pixtuoid_core::AgentId;
    use std::collections::HashSet;

    fn aid(n: usize) -> AgentId {
        AgentId::from_parts("test", &n.to_string())
    }

    /// A fixed waypoint-kind table: 5 = printer, 7 = vending, else couch.
    fn kinds(idx: usize) -> Option<WaypointKind> {
        match idx {
            5 => Some(WaypointKind::Printer),
            7 => Some(WaypointKind::VendingMachine),
            _ => Some(WaypointKind::Couch),
        }
    }

    #[test]
    fn tracker_primes_silently_then_chimes_once_per_new_agent_wave() {
        let mut tr = AudioCueTracker::new();
        let none = HashSet::new();
        // priming frame: an already-full office fires NOTHING (mid-attach)
        assert!(tr.observe(&[aid(1)], &none, kinds).is_empty());
        assert_eq!(
            tr.observe(&[aid(1), aid(2)], &none, kinds),
            vec![OneShot::DoorChime]
        );
        assert!(tr.observe(&[aid(1), aid(2)], &none, kinds).is_empty());
        assert_eq!(
            tr.observe(&[aid(1), aid(2), aid(3), aid(4), aid(5)], &none, kinds),
            vec![OneShot::DoorChime]
        );
        assert!(tr.observe(&[aid(1)], &none, kinds).is_empty());
        assert_eq!(
            tr.observe(&[aid(1), aid(2)], &none, kinds),
            vec![OneShot::DoorChime]
        );
    }

    #[test]
    fn tracker_emits_printer_whir_exactly_once_per_animation() {
        let mut tr = AudioCueTracker::new();
        let ids = [aid(1)];
        tr.observe(&ids, &HashSet::new(), kinds); // prime
        let at_printer: HashSet<usize> = [5].into();
        assert_eq!(
            tr.observe(&ids, &at_printer, kinds),
            vec![OneShot::PrinterWhir]
        );
        assert!(tr.observe(&ids, &at_printer, kinds).is_empty());
        assert!(tr.observe(&ids, &at_printer, kinds).is_empty());
        assert!(tr.observe(&ids, &HashSet::new(), kinds).is_empty());
        assert_eq!(
            tr.observe(&ids, &at_printer, kinds),
            vec![OneShot::PrinterWhir]
        );
    }

    #[test]
    fn tracker_maps_vending_and_ignores_non_appliance_waypoints() {
        let mut tr = AudioCueTracker::new();
        let ids = [aid(1)];
        tr.observe(&ids, &HashSet::new(), kinds); // prime
        let occupied: HashSet<usize> = [2, 7].into();
        assert_eq!(
            tr.observe(&ids, &occupied, kinds),
            vec![OneShot::VendingDrop]
        );
    }

    #[test]
    fn waiting_and_idle_agents_do_not_raise_the_tier() {
        let c = StateCounts {
            active: 0,
            waiting: 5,
            idle: 3,
            exiting: 1,
            total: 9,
        };
        assert_eq!(music_controls(&c, 0.0, 0.0).intensity, 0.0);
    }

    #[test]
    fn each_tier_asks_lofi_for_its_intensity_anchor() {
        let at = |active| music_controls(&counts(active), 0.0, 0.0).intensity;
        assert_eq!(at(0), 0.0, "an empty office is lofi's empty room");
        assert_eq!(
            at(1),
            Controls::default().intensity,
            "Moderate is lofi's anchor"
        );
        assert_eq!(at(BUSY_ACTIVE_MIN - 1), Controls::default().intensity);
        assert_eq!(
            at(BUSY_ACTIVE_MIN),
            1.0,
            "a busy office is lofi's busy room"
        );
    }

    #[test]
    fn music_controls_pass_the_sky_through() {
        for v in [0.0, 0.25, 1.0] {
            let c = music_controls(&counts(1), v, v);
            assert_eq!((c.daylight, c.rain), (1.0 - v, v));
        }
    }

    #[test]
    fn music_seed_is_whole_seconds_since_the_epoch() {
        use std::time::{Duration, UNIX_EPOCH};
        assert_eq!(
            music_seed(UNIX_EPOCH + Duration::from_millis(1_234_999)),
            1_234
        );
        assert_eq!(music_seed(UNIX_EPOCH - Duration::from_secs(10)), 0);
    }
}
