//! The playback seam: `AudioSink` is the ONE device boundary, so everything above
//! it is device-free and tests (or CI runners with no sound card) ride
//! `NullSink`.

use std::sync::Arc;

use pixtuoid_scene::audio::mixer::LoopStem;
use pixtuoid_scene::audio::{CEILING_DBFS, LIMITER_ATTACK, LIMITER_RELEASE, ceiling_amplitude};
use rodio::source::{LimitSettings, Source};

pub(crate) trait AudioSink: Send {
    /// Start `stem` looping `samples` (mono f32 at [`pixtuoid_scene::audio::SAMPLE_RATE`]) at gain 0.
    fn start_loop(&mut self, stem: LoopStem, samples: Arc<Vec<f32>>);
    /// Set a looping stem's gain (0..=1, already master-scaled).
    fn set_loop_gain(&mut self, stem: LoopStem, gain: f32);
    /// Fire-and-forget: the one-shot is detached, never tracked.
    fn play_once(&mut self, samples: Arc<Vec<f32>>, gain: f32);
}

/// Records calls instead of making sound — the CI/test double.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct NullSink {
    pub(crate) loops_started: Vec<LoopStem>,
    pub(crate) one_shots: usize,
}

#[cfg(test)]
impl AudioSink for NullSink {
    fn start_loop(&mut self, stem: LoopStem, _samples: Arc<Vec<f32>>) {
        self.loops_started.push(stem);
    }
    fn set_loop_gain(&mut self, _stem: LoopStem, _gain: f32) {}
    fn play_once(&mut self, _samples: Arc<Vec<f32>>, gain: f32) {
        if gain > 0.0 {
            self.one_shots += 1;
        }
    }
}

/// A synthesized buffer (mono f32 at [`pixtuoid_scene::audio::SAMPLE_RATE`]) as
/// a rodio source.
pub(crate) fn mono(samples: &[f32]) -> rodio::buffer::SamplesBuffer {
    let channels = std::num::NonZero::new(1u16).expect("1 != 0");
    let rate =
        std::num::NonZero::new(pixtuoid_scene::audio::SAMPLE_RATE).expect("SAMPLE_RATE != 0");
    rodio::buffer::SamplesBuffer::new(channels, rate, samples)
}

/// `source` held under [`CEILING_DBFS`]: the one output stage of the device's
/// bus and the LISTEN render alike.
pub(crate) fn limited<S: Source>(source: S) -> impl Source {
    // The default's soft knee stays: a zero one divides by zero at the
    // threshold (`LimitBase::new`'s `inv_knee_8`), so this curve bends where
    // the web's hard-kneed one doesn't.
    let settings = LimitSettings::default()
        .with_threshold(CEILING_DBFS)
        .with_attack(LIMITER_ATTACK)
        .with_release(LIMITER_RELEASE);
    // A distortion of gain 1 is a bare clamp. It catches what overshoots the
    // limiter, which is feedforward with no lookahead (`rodio::source::limit`).
    source.limit(settings).distortion(1.0, ceiling_amplitude())
}

/// The bus every loop and one-shot sums into, at the device's format, and its
/// [`limited`] output for the device's mixer.
fn bus(
    channels: rodio::ChannelCount,
    rate: rodio::SampleRate,
) -> (rodio::mixer::Mixer, impl Source + Send + 'static) {
    let (mixer, out) = rodio::mixer::mixer(channels, rate);
    // An empty mixer ends (`rodio::mixer::mixer`), and the device's mixer
    // drops an ended source for good: silence holds it open.
    mixer.add(rodio::source::Zero::new(channels, rate));
    (mixer, limited(out))
}

/// The real device sink — rodio/cpal construction glue. Needs real audio hardware,
/// so it is codecov-excluded and has no unit tests.
#[cfg(feature = "audio")]
pub(crate) mod rodio_sink {
    use super::*;
    use std::collections::HashMap;

    pub(crate) struct RodioSink {
        // field order = drop order: players release before the device sink
        loops: HashMap<LoopStem, rodio::Player>,
        bus: rodio::mixer::Mixer,
        _stream: rodio::MixerDeviceSink,
    }

    impl RodioSink {
        /// `None` when no output device is available (headless boxes) —
        /// callers degrade to silence, never error the office.
        pub(crate) fn open() -> Option<Self> {
            // `open_default_sink` keeps rodio's full open FALLBACK (every other
            // non-"null" output device, each retried across its configs); a
            // hand-rolled `from_default_device().open_stream()` would silently drop
            // that `.or_else` and go silent on hardware the fallback recovers.
            // rodio's `tracing` feature routes a MID-SESSION stream error to
            // `tracing::error!` instead of an `eprintln!` that would corrupt the TUI
            // mid-altscreen.
            let opened = with_stderr_silenced(rodio::DeviceSinkBuilder::open_default_sink);
            match opened {
                Ok(mut stream) => {
                    stream.log_on_drop(false);
                    let config = stream.config();
                    let (bus, out) = bus(config.channel_count(), config.sample_rate());
                    stream.mixer().add(out);
                    Some(Self {
                        loops: HashMap::new(),
                        bus,
                        _stream: stream,
                    })
                }
                Err(e) => {
                    tracing::warn!(error = %e, "audio: no output device, running silent");
                    None
                }
            }
        }
    }

    /// Run `f` with fd 2 pointed at /dev/null (Unix): ALSA and friends print raw
    /// diagnostics to stderr during device open, and the lazy spawn happens
    /// MID-ALTSCREEN — one stray line corrupts the TUI.
    #[cfg(unix)]
    fn with_stderr_silenced<T>(f: impl FnOnce() -> T) -> T {
        // SAFETY: plain dup/dup2 fd shuffling; restored before returning.
        // A panic inside `f` would leak the redirect — acceptable for a
        // device-open that must not unwind (rodio returns Result).
        unsafe {
            let saved = libc::dup(2);
            if saved >= 0 {
                let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
                if devnull >= 0 {
                    libc::dup2(devnull, 2);
                    libc::close(devnull);
                }
            }
            let out = f();
            if saved >= 0 {
                libc::dup2(saved, 2);
                libc::close(saved);
            }
            out
        }
    }

    #[cfg(not(unix))]
    fn with_stderr_silenced<T>(f: impl FnOnce() -> T) -> T {
        f()
    }

    impl AudioSink for RodioSink {
        fn start_loop(&mut self, stem: LoopStem, samples: Arc<Vec<f32>>) {
            let player = rodio::Player::connect_new(&self.bus);
            player.set_volume(0.0);
            player.append(mono(&samples).repeat_infinite());
            self.loops.insert(stem, player);
        }

        fn set_loop_gain(&mut self, stem: LoopStem, gain: f32) {
            if let Some(player) = self.loops.get(&stem) {
                player.set_volume(gain);
            }
        }

        fn play_once(&mut self, samples: Arc<Vec<f32>>, gain: f32) {
            if gain <= 0.0 {
                return;
            }
            let player = rodio::Player::connect_new(&self.bus);
            player.set_volume(gain);
            player.append(mono(&samples));
            player.detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_scene::audio::SAMPLE_RATE;

    fn tone(amplitude: f32, hz: f32, secs: f32) -> Vec<f32> {
        let n = (secs * SAMPLE_RATE as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                // cos, not sin: the burst starts at its crest
                amplitude * (std::f32::consts::TAU * hz * t).cos()
            })
            .collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |a, &v| a.max(v.abs()))
    }

    #[test]
    fn a_burst_from_silence_never_passes_the_ceiling() {
        let mut input = vec![0.0; SAMPLE_RATE as usize / 2];
        input.extend(tone(2.0, 440.0, 1.0));
        let out: Vec<f32> = limited(mono(&input)).collect();
        assert!(
            peak(&out) <= ceiling_amplitude(),
            "the limiter's attack lets the burst's first samples through: {}",
            peak(&out)
        );
    }

    #[test]
    fn a_sustained_overload_comes_out_limited_not_clipped() {
        let input = tone(2.0, 110.0, 2.0);
        let out: Vec<f32> = limited(mono(&input)).collect();
        let attack = (LIMITER_ATTACK.as_secs_f32() * SAMPLE_RATE as f32) as usize;
        let settled = &out[10 * attack..];
        let at_clamp = settled
            .iter()
            .filter(|v| v.abs() >= ceiling_amplitude())
            .count();
        let share = at_clamp as f32 / settled.len() as f32;
        // Only the crests the release lets past between two peaks: a clamp
        // alone holds most of this tone's samples there.
        assert!(share < 0.15, "{share} of the samples sit at the clamp");
    }

    #[test]
    fn a_quiet_signal_passes_unchanged() {
        let input = tone(0.1, 440.0, 1.0);
        let out: Vec<f32> = limited(mono(&input)).collect();
        let worst = input
            .iter()
            .zip(&out)
            .fold(0.0f32, |a, (x, y)| a.max((x - y).abs()));
        assert!(worst < 1e-6, "a −20 dBFS tone moved by {worst}");
    }

    #[test]
    fn the_bus_limits_every_source_summed_into_it() {
        let channels = std::num::NonZero::new(2u16).expect("2 != 0");
        let rate = std::num::NonZero::new(48_000u32).expect("48 kHz != 0");
        let (bus, mut out) = bus(channels, rate);
        assert_eq!(
            out.next(),
            Some(0.0),
            "an empty bus must stay open, or the device mixer drops it before the first lane"
        );
        for _ in 0..3 {
            bus.add(mono(&tone(0.9, 110.0, 1.0)));
        }
        let mixed: Vec<f32> = out.take(rate.get() as usize).collect();
        assert!(peak(&mixed) <= ceiling_amplitude(), "{}", peak(&mixed));
        assert!(
            peak(&mixed) > ceiling_amplitude() / 2.0,
            "the summed sources never reached the output"
        );
    }
}
