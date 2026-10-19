//! The music: lofi's [`Performance`] rendered ahead on the audio thread into a
//! wait-free ring, which the device callback reads at the mixer's gain.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use pixtuoid_scene::audio::{Frame, MUSIC_AHEAD, MusicCmd, Performance, SAMPLE_RATE};

/// The ring's depth: [`MUSIC_AHEAD`] in frames.
fn ahead_frames() -> usize {
    (MUSIC_AHEAD.as_secs_f64() * f64::from(SAMPLE_RATE)).ceil() as usize
}

/// The stream's two ends: the audio thread's [`MusicProducer`] and the
/// device's [`MusicReader`]. `seed` picks the run of songs.
pub(crate) fn stream(seed: u64) -> (MusicProducer, MusicReader) {
    let (frames, ring) = rtrb::RingBuffer::new(ahead_frames());
    let gain = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let underruns = Arc::new(AtomicU64::new(0));
    (
        MusicProducer {
            seed,
            performance: None,
            frames,
            gain: Arc::clone(&gain),
            underruns: Arc::clone(&underruns),
            seen_underruns: 0,
            fed: false,
        },
        MusicReader {
            frames: ring,
            gain,
            underruns,
        },
    )
}

/// The audio thread's end: renders ahead and sets the gain.
pub(crate) struct MusicProducer {
    seed: u64,
    /// `None` until the scene's first frame picks the first song.
    performance: Option<Performance>,
    frames: rtrb::Producer<Frame>,
    gain: Arc<AtomicU32>,
    underruns: Arc<AtomicU64>,
    seen_underruns: u64,
    /// Whether the last tick topped the ring up, so an underrun since was one
    /// the listener heard.
    fed: bool,
}

impl MusicProducer {
    /// One audio tick on the engine's `music`; returns the frames the device
    /// played as silence since the last tick while the music should have
    /// played. A zero gain renders nothing, so muted costs nothing, and the
    /// stream picks up where it was on unmute, since lofi reads no clock.
    pub(crate) fn tick(&mut self, music: Option<MusicCmd>) -> u64 {
        let underruns = self.underruns.load(Relaxed);
        let heard = if self.fed {
            underruns - self.seen_underruns
        } else {
            0
        };
        self.seen_underruns = underruns;
        self.fed = false;
        let Some(MusicCmd { controls, gain }) = music else {
            return heard;
        };
        self.gain.store(gain.to_bits(), Relaxed);
        let performance = self
            .performance
            .get_or_insert_with(|| Performance::new(self.seed, controls));
        performance.set_controls(controls);
        if gain <= 0.0 {
            return heard;
        }
        let free = self.frames.slots();
        let was_empty = free == self.frames.buffer().capacity();
        if let Ok(mut chunk) = self.frames.write_chunk(free) {
            let (first, second) = chunk.as_mut_slices();
            performance.render(first);
            performance.render(second);
            chunk.commit_all();
        }
        if was_empty {
            // What the device played while an empty ring filled was the
            // silence before the music, never a dropout.
            self.seen_underruns = self.underruns.load(Relaxed);
        }
        self.fed = true;
        heard
    }
}

/// The device's end: one frame per call, wait-free and allocation-free.
pub(crate) struct MusicReader {
    frames: rtrb::Consumer<Frame>,
    gain: Arc<AtomicU32>,
    underruns: Arc<AtomicU64>,
}

impl MusicReader {
    /// The next frame at the mixer's gain, or silence when the ring is empty.
    pub(crate) fn next_frame(&mut self) -> Frame {
        match self.frames.pop() {
            Ok(frame) => {
                let gain = f32::from_bits(self.gain.load(Relaxed));
                frame.map(|s| s * gain)
            }
            Err(_) => {
                self.underruns.fetch_add(1, Relaxed);
                Frame::default()
            }
        }
    }

    /// Frames rendered and not yet read.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.frames.slots()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_scene::audio::Controls;

    const SEED: u64 = 7;

    fn night() -> Controls {
        Controls::default().with_daylight(0.0)
    }

    fn at(gain: f32) -> Option<MusicCmd> {
        Some(MusicCmd {
            controls: night(),
            gain,
        })
    }

    fn drain(reader: &mut MusicReader) -> Vec<Frame> {
        (0..reader.queued()).map(|_| reader.next_frame()).collect()
    }

    fn performed(controls: Controls, n: usize) -> Vec<Frame> {
        let mut frames = vec![Frame::default(); n];
        Performance::new(SEED, controls).render(&mut frames);
        frames
    }

    #[test]
    fn nothing_renders_before_the_first_frame() {
        let (mut producer, reader) = stream(SEED);
        producer.tick(None);
        assert_eq!(reader.queued(), 0);
    }

    /// lofi picks the first song from what it was asked before its first
    /// render, so the first frame's night plays a night song.
    #[test]
    fn the_first_frames_controls_pick_the_first_song() {
        let (mut producer, mut reader) = stream(SEED);
        producer.tick(at(1.0));
        let heard = drain(&mut reader);
        assert_eq!(heard.len(), ahead_frames(), "a full ring");
        assert_eq!(heard, performed(night(), heard.len()));
        assert_ne!(
            heard,
            performed(Controls::default(), heard.len()),
            "premise: a day start sounds different"
        );
    }

    #[test]
    fn each_tick_tops_the_ring_up_and_continues_the_stream() {
        let (mut producer, mut reader) = stream(SEED);
        producer.tick(at(1.0));
        let mut heard: Vec<Frame> = (0..1000).map(|_| reader.next_frame()).collect();
        producer.tick(at(1.0));
        assert_eq!(reader.queued(), ahead_frames());
        heard.extend(drain(&mut reader));
        assert_eq!(heard, performed(night(), heard.len()), "no gap, no repeat");
    }

    #[test]
    fn a_zero_gain_renders_nothing_and_the_stream_resumes_where_it_was() {
        let (mut producer, mut reader) = stream(SEED);
        producer.tick(at(1.0));
        let mut heard = drain(&mut reader);
        producer.tick(at(0.0));
        assert_eq!(reader.queued(), 0, "muted costs nothing");
        producer.tick(at(1.0));
        heard.extend(drain(&mut reader));
        assert_eq!(heard, performed(night(), heard.len()));
    }

    #[test]
    fn the_reader_plays_the_mixers_gain() {
        let (mut producer, mut reader) = stream(SEED);
        producer.tick(at(0.25));
        let heard = drain(&mut reader);
        let full = performed(night(), heard.len());
        assert!(full.iter().any(|f| f[0] != 0.0), "premise: the song sounds");
        for (h, f) in heard.iter().zip(&full) {
            assert_eq!(*h, f.map(|s| s * 0.25));
        }
    }

    #[test]
    fn only_a_dropout_while_the_music_plays_is_reported() {
        let (mut producer, mut reader) = stream(SEED);
        reader.next_frame();
        assert_eq!(producer.tick(at(1.0)), 0, "silence before the first fill");
        drain(&mut reader);
        reader.next_frame();
        assert_eq!(producer.tick(at(1.0)), 1, "a dropout after a fill");
        assert_eq!(producer.tick(at(0.0)), 0);
        drain(&mut reader);
        reader.next_frame();
        assert_eq!(producer.tick(at(1.0)), 0, "muted silence is no dropout");
    }

    #[test]
    fn an_empty_ring_plays_silence_and_counts_it() {
        let (_producer, mut reader) = stream(SEED);
        assert_eq!(reader.next_frame(), Frame::default());
        assert_eq!(reader.next_frame(), Frame::default());
        assert_eq!(reader.underruns.load(Relaxed), 2);
    }

    /// rodio keeps each frame's first sample on a mono device and discards the
    /// rest (<https://docs.rs/rodio/0.23.0/src/rodio/conversions/channels.rs.html#59-80>),
    /// so the music folds to mono losslessly only while lofi renders L == R.
    #[test]
    fn lofi_renders_both_channels_alike() {
        let frames = performed(Controls::default(), SAMPLE_RATE as usize);
        assert!(
            frames.iter().any(|f| f[0] != 0.0),
            "premise: the song sounds"
        );
        assert!(frames.iter().all(|[l, r]| l == r));
    }
}
