//! Pure mixing math — target-chasing gain ramps plus the typing/raindrop
//! schedulers. No devices, no clocks: time arrives as `dt`/`now_s` parameters.

use pixtuoid_lofi::dsp::NoiseStream;

use crate::audio::StemLevels;

/// The looped stems the sink plays. `typing` is NOT here — it is a scheduled
/// one-shot voice (`TypingScheduler`), not a loop; nor is the music, which
/// streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoopStem {
    Rain,
}

impl LoopStem {
    pub const ALL: [LoopStem; 1] = [LoopStem::Rain];

    fn level_of(self, s: &StemLevels) -> f32 {
        match self {
            LoopStem::Rain => s.rain,
        }
    }
}

/// Full-scale gain travel per second — a weather change crossfades rather
/// than stepping.
pub const RAMP_PER_S: f32 = 0.5;

/// How long the music's gain takes to travel full scale (0..1), so a mute
/// fades it without a click.
const MUSIC_FADE_S: f32 = 0.5;

/// The foley's trim under the volume curve: the office's sounds sit UNDER the
/// user's real work audio, staged alike with lofi's music.
const BUS_TRIM: f32 = pixtuoid_lofi::Performance::MASTER_GAIN as f32;

/// Per-stem gain ramps chasing the scene's target levels, and the music's
/// gain. Mute ramps to silence through the same slew (no click).
#[derive(Debug)]
pub(crate) struct Mixer {
    current: [f32; LoopStem::ALL.len()],
    music: f32,
    target: StemLevels,
    muted: bool,
    master: f32,
}

/// `cur` moved toward `goal` by at most `max_delta`: a slew, never an
/// overshoot.
fn slew(cur: f32, goal: f32, max_delta: f32) -> f32 {
    if (goal - cur).abs() <= max_delta {
        goal
    } else if goal > cur {
        cur + max_delta
    } else {
        cur - max_delta
    }
}

impl Mixer {
    pub(crate) fn new(master: f32) -> Self {
        Self {
            current: [0.0; LoopStem::ALL.len()],
            music: 0.0,
            target: StemLevels::default(),
            muted: false,
            master,
        }
    }

    pub(crate) fn set_target(&mut self, stems: StemLevels) {
        self.target = stems;
    }

    /// Live master-volume update (the +/- keys) — targets rescale next
    /// step, riding the same slew as any level change (no zipper).
    pub(crate) fn set_master(&mut self, master: f32) {
        self.master = master.clamp(0.0, 1.0);
    }

    pub(crate) fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// The USER volume (0..1) mapped to amplitude: a squared perceptual curve
    /// (loudness is logarithmic). The ONE volume→amplitude site; the footer
    /// keeps showing the user's linear percent.
    fn volume_curve(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.master * self.master
        }
    }

    /// The scalar every one-shot's gain multiplies through — mute silences them
    /// instantly (one-shots are transient; no ramp needed).
    pub(crate) fn one_shot_gain(&self) -> f32 {
        self.volume_curve() * BUS_TRIM
    }

    /// Advance every gain toward its target; returns each loop's
    /// `(stem, gain)` and the music's gain for the sink. lofi masters the
    /// music "at the listener's volume, with no further trim"
    /// (`Performance::render`), so its goal is the volume curve alone.
    pub(crate) fn step(&mut self, dt_s: f32) -> ([(LoopStem, f32); LoopStem::ALL.len()], f32) {
        let foley = self.volume_curve() * BUS_TRIM;
        let mut out = [(LoopStem::Rain, 0.0f32); LoopStem::ALL.len()];
        for (i, stem) in LoopStem::ALL.into_iter().enumerate() {
            let goal = stem.level_of(&self.target) * foley;
            self.current[i] = slew(self.current[i], goal, RAMP_PER_S * dt_s);
            out[i] = (stem, self.current[i]);
        }
        self.music = slew(self.music, self.volume_curve(), dt_s / MUSIC_FADE_S);
        (out, self.music)
    }
}

/// Typing-burst scheduler driven by the scene's `typing` level: 0 = silence,
/// higher = more bursts.
#[derive(Debug)]
pub(crate) struct TypingScheduler {
    rng: NoiseStream,
    burst_left: u32,
    next_at_s: f64,
}

const BURSTS_PER_MIN_AT_FULL: f64 = 28.0;

impl TypingScheduler {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            rng: NoiseStream::new(seed),
            burst_left: 0,
            next_at_s: 0.0,
        }
    }

    /// Advance to `now_s`; returns how many keystrokes fire this tick.
    pub(crate) fn tick(&mut self, now_s: f64, level: f32) -> u32 {
        if level <= 0.0 {
            self.burst_left = 0;
            // hold the clock so a later level>0 doesn't replay a backlog
            self.next_at_s = now_s;
            return 0;
        }
        let mut fired = 0u32;
        while self.next_at_s <= now_s {
            if self.burst_left == 0 {
                let per_s = BURSTS_PER_MIN_AT_FULL / 60.0 * f64::from(level);
                let gap = (0.5 + f64::from(self.rng.unit())) / per_s.max(1e-6);
                self.burst_left = 8 + (self.rng.unit() * 14.0) as u32;
                self.next_at_s += gap;
            } else {
                fired += 1;
                self.burst_left -= 1;
                let mut gap = 0.066 + 0.030 * f64::from(self.rng.unit());
                if self.rng.unit() < 0.08 {
                    gap += 0.18;
                }
                self.next_at_s += gap;
            }
        }
        fired
    }
}

/// Runtime raindrop scatter — the bed loops, the drops never repeat.
#[derive(Debug)]
pub(crate) struct DropScheduler {
    rng: NoiseStream,
    next_at_s: f64,
}

const DROPS_PER_S: f64 = 0.9;

impl DropScheduler {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            rng: NoiseStream::new(seed),
            next_at_s: 0.0,
        }
    }

    /// Returns how many drops fire this tick (0 while dry).
    pub(crate) fn tick(&mut self, now_s: f64, rain_level: f32) -> u32 {
        if rain_level <= 0.0 {
            self.next_at_s = now_s;
            return 0;
        }
        let mut fired = 0u32;
        while self.next_at_s <= now_s {
            fired += 1;
            let gap = (0.4 + 1.2 * f64::from(self.rng.unit())) / DROPS_PER_S;
            self.next_at_s += gap;
            if self.rng.unit() < 0.35 {
                fired += 1;
                self.next_at_s += 0.20 + 0.15 * f64::from(self.rng.unit());
            }
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(rain: f32, typing: f32) -> StemLevels {
        StemLevels { rain, typing }
    }

    #[test]
    fn mixer_ramps_toward_targets_without_overshoot() {
        let mut m = Mixer::new(1.0);
        m.set_target(levels(0.8, 0.0));
        let (g1, _) = m.step(0.1);
        let rain1 = g1.iter().find(|(s, _)| *s == LoopStem::Rain).unwrap().1;
        assert!((rain1 - 0.05).abs() < 1e-6, "ramp step {rain1}");
        for _ in 0..100 {
            m.step(0.1);
        }
        let (g, _) = m.step(0.1);
        let rain = g.iter().find(|(s, _)| *s == LoopStem::Rain).unwrap().1;
        assert_eq!(rain, 0.8 * BUS_TRIM, "converged without overshoot");
    }

    #[test]
    fn music_plays_at_the_volume_curve_and_foley_under_the_bus_trim() {
        let mut m = Mixer::new(0.5);
        m.set_target(levels(1.0, 0.0));
        for _ in 0..200 {
            m.step(0.1);
        }
        let (loops, music) = m.step(0.1);
        let curve = 0.5 * 0.5;
        assert_eq!(music, curve, "lofi masters the music");
        assert!((loops[0].1 - curve * BUS_TRIM).abs() < 1e-6);
        assert!((m.one_shot_gain() - curve * BUS_TRIM).abs() < 1e-6);
    }

    /// The loudest lane the stream replaced, Moderate's texture at 0.84 of
    /// the 0.35 bus trim it played under, faded at [`RAMP_PER_S`].
    #[test]
    fn a_mute_fades_the_music_no_slower_than_the_lanes_did() {
        let lanes_fade_s = 0.84 * 0.35 / RAMP_PER_S;
        let dt = 0.01;
        let mut m = Mixer::new(1.0);
        for _ in 0..200 {
            m.step(dt);
        }
        m.set_muted(true);
        let mut faded_s = 0.0;
        while m.step(dt).1 > 0.0 {
            faded_s += dt;
        }
        assert!(faded_s > 0.0, "a mute fades, never cuts");
        assert!(
            faded_s <= lanes_fade_s,
            "{faded_s} s against the lanes' {lanes_fade_s} s"
        );
    }

    #[test]
    fn mute_ramps_all_loops_to_zero_and_zeroes_one_shots() {
        let mut m = Mixer::new(1.0);
        m.set_target(levels(0.8, 0.5));
        for _ in 0..100 {
            m.step(0.1);
        }
        m.set_muted(true);
        assert_eq!(m.one_shot_gain(), 0.0, "one-shots cut instantly");
        for _ in 0..100 {
            m.step(0.1);
        }
        let (loops, music) = m.step(0.1);
        assert!(loops.iter().all(|(_, g)| *g == 0.0), "loops ramped out");
        assert_eq!(music, 0.0, "the music ramped out");
    }

    #[test]
    fn master_volume_scales_every_loop_target() {
        let mut m = Mixer::new(0.5);
        m.set_target(levels(0.8, 0.0));
        for _ in 0..200 {
            m.step(0.1);
        }
        let (g, _) = m.step(0.1);
        let rain = g.iter().find(|(s, _)| *s == LoopStem::Rain).unwrap().1;
        let want = 0.8 * 0.25 * BUS_TRIM;
        assert!(
            (rain - want).abs() < 1e-6,
            "curved master: {rain} vs {want}"
        );
    }

    #[test]
    fn volume_curve_makes_low_percents_near_silent() {
        let mut quiet = Mixer::new(0.05);
        quiet.set_target(levels(1.0, 0.0));
        for _ in 0..300 {
            quiet.step(0.1);
        }
        let (g, _) = quiet.step(0.1);
        let rain = g.iter().find(|(s, _)| *s == LoopStem::Rain).unwrap().1;
        assert!(rain < 0.001, "5% must be near-silent, got {rain}");

        let mut full = Mixer::new(1.0);
        full.set_target(levels(1.0, 0.0));
        for _ in 0..300 {
            full.step(0.1);
        }
        let (g, _) = full.step(0.1);
        let rain = g.iter().find(|(s, _)| *s == LoopStem::Rain).unwrap().1;
        assert!(
            (rain - BUS_TRIM).abs() < 1e-6,
            "100% = the trimmed bus: {rain}"
        );
    }

    #[test]
    fn typing_scheduler_is_silent_at_zero_and_types_at_level() {
        let mut t = TypingScheduler::new(1);
        let mut total = 0;
        for i in 0..600 {
            total += t.tick(f64::from(i) * 0.1, 0.0);
        }
        assert_eq!(total, 0, "level 0 must never type");
        let first = t.tick(60.05, 0.5);
        assert!(first <= 1, "no backlog replay, got {first}");
        let mut typed = 0;
        for i in 0..1200 {
            typed += t.tick(60.1 + f64::from(i) * 0.05, 0.5);
        }
        assert!(
            (60..=400).contains(&typed),
            "moderate typing rate out of band: {typed} keys/min"
        );
    }

    #[test]
    fn drop_scheduler_matches_the_reference_density_band() {
        let mut d = DropScheduler::new(2);
        let mut drops = 0;
        for i in 0..600 {
            drops += d.tick(f64::from(i) * 0.1, 0.55);
        }
        assert!(
            (30..=110).contains(&drops),
            "drop density out of band: {drops}/min"
        );
        assert_eq!(d.tick(61.0, 0.0), 0, "dry sky drops nothing");
    }
}
