//! The per-frame audio engine — the ONE place the office's sound is advanced
//! each tick, so the native rodio gateway and the wasm WebAudio painter run
//! byte-identical mixing/scheduling logic. It owns ONLY per-tick STATE; the
//! bank's build stays caller-side.
//!
//! Time is a PARAMETER: `tick` takes `dt` and never reads a clock.

use pixtuoid_lofi::dsp::NoiseStream;

use super::bank::{
    DROP_GAIN, DROP_POOL, KEYSTROKE_GAIN, KEYSTROKE_POOL, ONE_SHOT_GAIN, OneShotPool,
};
use super::mixer::{DropScheduler, LoopStem, Mixer, TypingScheduler};
use super::{AudioFrame, Controls, DROP_SEED, OneShot, PICK_SEED, StemLevels, TYPING_SEED};

/// dt ceiling (s): a bigger inter-tick gap (a starved native thread, a
/// backgrounded wasm tab, a GC pause) is clamped so one tick can neither snap
/// a ramp nor burst-replay the schedulers. BOTH shells clamp to it.
pub const MAX_DT_S: f32 = 0.10;

/// One one-shot to spawn this tick: a fresh source from `(pool, index)` at
/// `gain`, already master-scaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayCmd {
    pub pool: OneShotPool,
    pub index: usize,
    pub gain: f32,
}

/// The music this tick: what lofi is asked, and the mixer's ramped gain to
/// play it at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MusicCmd {
    pub controls: Controls,
    pub gain: f32,
}

/// What one [`AudioEngine::tick`] produces — the backend-agnostic result each
/// shell flushes its own way.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickCommands {
    /// Target gain per LOOP stem, in `LoopStem::ALL` order.
    pub gains: [f32; LoopStem::ALL.len()],
    /// One-shots to fire this tick.
    pub plays: Vec<PlayCmd>,
    /// The music, `None` until the first frame: a stream that starts sooner
    /// plays a day song at night.
    pub music: Option<MusicCmd>,
}

/// The per-tick audio state both backends share. Constructed once, then `tick`
/// per frame.
#[derive(Debug)]
pub struct AudioEngine {
    mixer: Mixer,
    typing: TypingScheduler,
    drops: DropScheduler,
    /// One-shot variant picker — its draw order is part of the ratified sound.
    pick: NoiseStream,
    /// The last frame's target levels — held across a timeout tick (`None`).
    wanted: StemLevels,
    /// The last frame's music, held the same way.
    music: Option<Controls>,
    /// Monotonic scheduler clock (s), advanced by the passed (clamped) dt.
    sched_s: f64,
}

impl AudioEngine {
    /// A fresh engine at `master` volume.
    pub fn new(master: f32) -> Self {
        Self {
            mixer: Mixer::new(master),
            typing: TypingScheduler::new(TYPING_SEED),
            drops: DropScheduler::new(DROP_SEED),
            pick: NoiseStream::new(PICK_SEED),
            wanted: StemLevels::default(),
            music: None,
            sched_s: 0.0,
        }
    }

    /// Live mute (native `m` key). No-op on wasm (mute = JS suspends the context).
    pub fn set_muted(&mut self, muted: bool) {
        self.mixer.set_muted(muted);
    }

    /// Live master volume (native `+/-`). No-op on wasm (fixed 1.0 bus).
    pub fn set_master(&mut self, master: f32) {
        self.mixer.set_master(master);
    }

    /// Advance one tick by `dt` seconds. `frame` is the scene's audio intent —
    /// `None` on a native `recv_timeout`, where the ramp and schedulers still
    /// advance from the held state.
    pub fn tick(&mut self, dt: f32, frame: Option<AudioFrame>) -> TickCommands {
        self.sched_s += f64::from(dt);

        let events: Vec<OneShot> = if let Some(f) = frame {
            self.wanted = f.stems;
            self.music = Some(f.music);
            f.events
        } else {
            Vec::new()
        };

        self.mixer.set_target(self.wanted);
        let (gain_pairs, music_gain) = self.mixer.step(dt);

        let mut gains = [0.0f32; LoopStem::ALL.len()];
        for (i, (_, g)) in gain_pairs.iter().enumerate() {
            gains[i] = *g;
        }

        // Muted rides through `one_shot_gain` = 0 — plays still enqueue (the sink
        // drops them) so the schedulers' clocks never desync on mute.
        let mut plays = Vec::new();
        let os_gain = self.mixer.one_shot_gain();
        for event in events {
            plays.push(PlayCmd {
                pool: OneShotPool::from_event(event),
                index: 0,
                gain: ONE_SHOT_GAIN * os_gain,
            });
        }
        for _ in 0..self.typing.tick(self.sched_s, self.wanted.typing) {
            let index = (self.pick.unit() * KEYSTROKE_POOL as f32) as usize % KEYSTROKE_POOL;
            plays.push(PlayCmd {
                pool: OneShotPool::Keystroke,
                index,
                gain: KEYSTROKE_GAIN * os_gain,
            });
        }
        for _ in 0..self.drops.tick(self.sched_s, self.wanted.rain) {
            let index = (self.pick.unit() * DROP_POOL as f32) as usize % DROP_POOL;
            plays.push(PlayCmd {
                pool: OneShotPool::Drop,
                index,
                gain: DROP_GAIN * self.wanted.rain * os_gain,
            });
        }

        TickCommands {
            gains,
            plays,
            music: self.music.map(|controls| MusicCmd {
                controls,
                gain: music_gain,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl TickCommands {
        /// `stem`'s gain in [`TickCommands::gains`].
        fn gain(&self, stem: LoopStem) -> f32 {
            LoopStem::ALL
                .iter()
                .zip(self.gains)
                .find_map(|(s, g)| (*s == stem).then_some(g))
                .expect("every stem has a gain")
        }
    }

    fn running_frame() -> AudioFrame {
        AudioFrame {
            stems: StemLevels {
                rain: 0.5,
                typing: 0.8,
            },
            events: Vec::new(),
            music: Controls::default(),
        }
    }

    fn settle(engine: &mut AudioEngine, ticks: usize) {
        for _ in 0..ticks {
            engine.tick(0.05, Some(running_frame()));
        }
    }

    #[test]
    fn ramps_loop_gains_and_fires_typing_for_a_running_office() {
        let mut e = AudioEngine::new(1.0);
        let mut typed = 0;
        let mut last_rain = 0.0;
        for _ in 0..200 {
            let cmd = e.tick(0.05, Some(running_frame()));
            last_rain = cmd.gain(LoopStem::Rain);
            typed += cmd
                .plays
                .iter()
                .filter(|p| p.pool == OneShotPool::Keystroke)
                .count();
        }
        assert!(last_rain > 0.0, "rain ramped up");
        assert!(typed > 0, "the typing scheduler fired keystrokes");
    }

    #[test]
    fn mute_zeroes_every_loop_gain_the_music_and_one_shot() {
        let mut e = AudioEngine::new(1.0);
        settle(&mut e, 200);
        e.set_muted(true);
        settle(&mut e, 200);
        let cmd = e.tick(0.05, Some(running_frame()));
        assert!(cmd.gains.iter().all(|g| *g == 0.0), "muted loops fall to 0");
        assert_eq!(cmd.music.map(|m| m.gain), Some(0.0), "the music falls to 0");
        assert!(
            cmd.plays.iter().all(|p| p.gain == 0.0),
            "muted one-shots ride at gain 0 (sink drops them)"
        );
    }

    #[test]
    fn master_zero_silences_the_bus_and_raising_it_ramps_back() {
        let mut e = AudioEngine::new(0.0);
        settle(&mut e, 200);
        let cmd = e.tick(0.05, Some(running_frame()));
        assert!(cmd.gains.iter().all(|g| *g == 0.0), "master 0 = silent");
        e.set_master(1.0);
        let mut rain = 0.0;
        for _ in 0..200 {
            rain = e.tick(0.05, Some(running_frame())).gain(LoopStem::Rain);
        }
        assert!(rain > 0.0, "master up ramps the mix back");
    }

    #[test]
    fn a_timeout_tick_advances_without_consuming_levels_or_events() {
        let mut e = AudioEngine::new(1.0);
        settle(&mut e, 100);
        let cmd = e.tick(0.05, None);
        assert!(
            cmd.plays.iter().all(|p| !matches!(
                p.pool,
                OneShotPool::DoorChime | OneShotPool::PrinterWhir | OneShotPool::VendingDrop
            )),
            "a None tick fires no scene one-shots"
        );
        assert!(
            cmd.gain(LoopStem::Rain) > 0.0,
            "the held rain keeps ramping on a None tick"
        );
    }

    #[test]
    fn the_music_waits_for_the_first_frame_then_holds_its_controls() {
        let mut e = AudioEngine::new(1.0);
        assert_eq!(e.tick(0.05, None).music, None, "no frame, no song");
        let night = Controls::default().with_daylight(0.0);
        let mut frame = running_frame();
        frame.music = night;
        let first = e
            .tick(0.05, Some(frame))
            .music
            .expect("a frame starts the song");
        assert_eq!(first.controls, night);
        assert!(first.gain > 0.0, "the music ramps in with the first tick");
        assert_eq!(
            e.tick(0.05, None).music.map(|m| m.controls),
            Some(night),
            "a timeout tick holds the last frame's music"
        );
    }

    #[test]
    fn scene_events_become_one_shot_plays() {
        let mut e = AudioEngine::new(1.0);
        settle(&mut e, 20);
        let mut frame = running_frame();
        frame.events = vec![OneShot::DoorChime, OneShot::VendingDrop];
        let cmd = e.tick(0.05, Some(frame));
        assert!(cmd.plays.iter().any(|p| p.pool == OneShotPool::DoorChime));
        assert!(cmd.plays.iter().any(|p| p.pool == OneShotPool::VendingDrop));
    }

    #[test]
    fn the_dt_clamp_ceiling_stays_a_slew_while_an_unclamped_gap_would_snap() {
        let one_tick = |dt: f32| {
            AudioEngine::new(1.0)
                .tick(dt, Some(running_frame()))
                .gain(LoopStem::Rain)
        };
        let mut settled = AudioEngine::new(1.0);
        let mut full = 0.0;
        for _ in 0..500 {
            full = settled
                .tick(0.05, Some(running_frame()))
                .gain(LoopStem::Rain);
        }
        assert!(
            one_tick(MAX_DT_S) < full * 0.5,
            "one clamped-ceiling tick is a slew, well under the settled target {full}"
        );
        assert!(
            (one_tick(2.0) - full).abs() < 1e-6,
            "an unclamped multi-second gap snaps straight to the target"
        );
    }
}
