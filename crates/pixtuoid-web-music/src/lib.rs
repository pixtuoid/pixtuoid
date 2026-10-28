//! The site's lofi music, in a wasm of its own: only the music worker loads
//! it, once a visitor turns the ♩ on, so the office's wasm every visitor
//! downloads carries no synthesizer for it.

// AGENTS.md's Errors rule, as a lint: no `unwrap()` outside tests.
#![cfg_attr(not(test), warn(clippy::unwrap_used))]

use pixtuoid_scene::anim::from_epoch_ms;
use pixtuoid_scene::audio::{Frame, Performance, controls_from_wire, music_seed};
use wasm_bindgen::prelude::*;

/// The music worker's lofi `Performance`, rendered in the chunks the worklet
/// asks for.
#[wasm_bindgen]
#[derive(Debug)]
pub struct MusicStream {
    performance: Performance,
    frames: Vec<Frame>,
}

#[wasm_bindgen]
impl MusicStream {
    /// A stream starting at `now_ms` ([`from_epoch_ms`]'s milliseconds) under
    /// the office's first `controls`, a tick's `music.controls` on the
    /// [`controls_wire`](pixtuoid_scene::audio::controls_wire), which pick
    /// its first song.
    ///
    /// # Errors
    ///
    /// When `controls` isn't the wire's.
    #[wasm_bindgen(constructor)]
    pub fn new(now_ms: f64, controls: &[f32]) -> Result<MusicStream, JsError> {
        let controls =
            controls_from_wire(controls).ok_or_else(|| JsError::new("not the music's controls"))?;
        Ok(MusicStream {
            performance: Performance::new(music_seed(from_epoch_ms(now_ms)), controls),
            frames: Vec::new(),
        })
    }

    /// The office's latest `controls`, which the next bar latches; `false`
    /// when they aren't the wire's.
    pub fn set_controls(&mut self, controls: &[f32]) -> bool {
        controls_from_wire(controls)
            .map(|c| self.performance.set_controls(c))
            .is_some()
    }

    /// Render the next `frames` frames and return where their `2 × frames`
    /// interleaved samples sit, valid until the next wasm call.
    pub fn render(&mut self, frames: usize) -> *const f32 {
        self.frames.resize(frames, Frame::default());
        self.performance.render(&mut self.frames);
        self.frames.as_flattened().as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_scene::audio::{Controls, controls_wire};

    /// The worker's stream plays the song its first controls pick, chunk
    /// after chunk with no seam.
    #[test]
    fn a_music_stream_renders_the_performance_its_first_controls_pick() {
        let now_ms = 1_753_000_000_000.0;
        let night = Controls::default().with_daylight(0.0).with_intensity(1.0);
        let wire = controls_wire(night);
        let Ok(mut stream) = MusicStream::new(now_ms, &wire) else {
            panic!("the wire's controls start a stream");
        };
        assert!(stream.set_controls(&wire));
        assert!(!stream.set_controls(&wire[..1]), "a torn wire is refused");
        let mut heard = Vec::new();
        for n in [1000, 3000] {
            let ptr = stream.render(n);
            assert_eq!(ptr, stream.frames.as_flattened().as_ptr());
            heard.extend_from_slice(&stream.frames);
        }
        let seed = music_seed(from_epoch_ms(now_ms));
        let performed = |c| {
            let mut want = vec![Frame::default(); heard.len()];
            Performance::new(seed, c).render(&mut want);
            want
        };
        assert_eq!(heard, performed(night));
        assert_ne!(
            heard,
            performed(Controls::default()),
            "premise: a day start differs"
        );
    }
}
