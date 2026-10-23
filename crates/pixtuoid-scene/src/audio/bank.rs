//! The pre-rendered sample banks — the ONE place the office's sounds are
//! synthesized into buffers, for the native rodio gateway and the wasm WebAudio
//! painter alike: the same synthesis, not the same bits, as [`f32::sin`]'s and
//! [`f32::exp`]'s precision varies by platform. Pure: no device deps.
//!
//! The `rng` DRAW ORDER is the sound — [`Synthesis`] draws the one-shot pools,
//! then rain, from ONE stream in the ratified order, so every buffer matches the
//! LISTEN-ratified renders. Don't reorder its calls.

use std::sync::Arc;

use pixtuoid_lofi::dsp::NoiseStream;

use super::{OneShot, foley};

/// Per-key / per-drop variant pool sizes: playback picks randomly so
/// typing/rain never sound repeated, while runtime stays synthesis-free.
pub const KEYSTROKE_POOL: usize = 16;
pub const DROP_POOL: usize = 12;

/// One-shot playback gains relative to master — loudness-matched unit levels.
pub(crate) const KEYSTROKE_GAIN: f32 = 0.35;
pub(crate) const ONE_SHOT_GAIN: f32 = 0.5;
/// Foreground raindrops sit well ABOVE the wash per the reference; the bed
/// peaks under 1.0, so drops ride at the rain level itself.
pub(crate) const DROP_GAIN: f32 = 0.9;

/// Which one-shot pool a play draws from — the ONE vocabulary both backends
/// share: the native gateway resolves it to a buffer via [`AssetBank::sample`],
/// the wasm painter sends `(wire, index)` to JS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneShotPool {
    Keystroke,
    Drop,
    DoorChime,
    PrinterWhir,
    VendingDrop,
}

impl OneShotPool {
    /// Which appliance-cue pool a scene [`OneShot`] plays from.
    pub(crate) fn from_event(event: OneShot) -> Self {
        match event {
            OneShot::DoorChime => OneShotPool::DoorChime,
            OneShot::PrinterWhir => OneShotPool::PrinterWhir,
            OneShot::VendingDrop => OneShotPool::VendingDrop,
        }
    }

    /// Stable index for the wasm JSON wire (JS maps it back to its buffer bank).
    pub fn wire(self) -> u8 {
        match self {
            OneShotPool::Keystroke => 0,
            OneShotPool::Drop => 1,
            OneShotPool::DoorChime => 2,
            OneShotPool::PrinterWhir => 3,
            OneShotPool::VendingDrop => 4,
        }
    }

    /// Every pool in wire order — a new pool is declared in exactly one place.
    pub const ALL: [OneShotPool; 5] = [
        OneShotPool::Keystroke,
        OneShotPool::Drop,
        OneShotPool::DoorChime,
        OneShotPool::PrinterWhir,
        OneShotPool::VendingDrop,
    ];

    /// The inverse of [`OneShotPool::wire`] (`None` for an unknown wire).
    pub fn from_wire(wire: u8) -> Option<Self> {
        OneShotPool::ALL.into_iter().find(|p| p.wire() == wire)
    }
}

/// The ONE-SHOT pools a player keeps for its whole life. The rain bed is NOT
/// retained here — `RodioSink` copies it into its own `SamplesBuffer`, so
/// holding the Arc would double the bed RAM.
#[derive(Debug)]
pub struct AssetBank {
    keystrokes: [Arc<Vec<f32>>; KEYSTROKE_POOL],
    drops: [Arc<Vec<f32>>; DROP_POOL],
    door_chime: Arc<Vec<f32>>,
    printer_whir: Arc<Vec<f32>>,
    vending_drop: Arc<Vec<f32>>,
}

impl AssetBank {
    pub(crate) fn build(rng: &mut NoiseStream) -> Self {
        Self {
            keystrokes: std::array::from_fn(|_| Arc::new(foley::keystroke(rng))),
            drops: std::array::from_fn(|_| Arc::new(foley::rain_drop(rng))),
            door_chime: Arc::new(foley::door_chime()),
            printer_whir: Arc::new(foley::printer_whir(rng)),
            vending_drop: Arc::new(foley::vending_drop(rng)),
        }
    }

    /// A bank of buffers built elsewhere (the wasm worker's handoff); `None`
    /// unless each pool holds its full size.
    pub fn adopt(
        keystrokes: Vec<Arc<Vec<f32>>>,
        drops: Vec<Arc<Vec<f32>>>,
        door_chime: Arc<Vec<f32>>,
        printer_whir: Arc<Vec<f32>>,
        vending_drop: Arc<Vec<f32>>,
    ) -> Option<Self> {
        Some(Self {
            keystrokes: keystrokes.try_into().ok()?,
            drops: drops.try_into().ok()?,
            door_chime,
            printer_whir,
            vending_drop,
        })
    }

    /// The `index`th buffer of `pool`, `None` past its end; an appliance pool
    /// holds one.
    pub fn get(&self, pool: OneShotPool, index: usize) -> Option<&Arc<Vec<f32>>> {
        match pool {
            OneShotPool::Keystroke => self.keystrokes.get(index),
            OneShotPool::Drop => self.drops.get(index),
            OneShotPool::DoorChime => (index == 0).then_some(&self.door_chime),
            OneShotPool::PrinterWhir => (index == 0).then_some(&self.printer_whir),
            OneShotPool::VendingDrop => (index == 0).then_some(&self.vending_drop),
        }
    }

    /// Resolve an engine-emitted `(pool, index)` play to its buffer, `index`
    /// taken modulo the pool's size.
    pub fn sample(&self, pool: OneShotPool, index: usize) -> Arc<Vec<f32>> {
        Arc::clone(match pool {
            OneShotPool::Keystroke => &self.keystrokes[index % KEYSTROKE_POOL],
            OneShotPool::Drop => &self.drops[index % DROP_POOL],
            OneShotPool::DoorChime => &self.door_chime,
            OneShotPool::PrinterWhir => &self.printer_whir,
            OneShotPool::VendingDrop => &self.vending_drop,
        })
    }
}

/// Seeds [`Synthesis`]'s one stream: the ratified renders were drawn from it,
/// so a new value is a new sound.
const BUILD_SEED: u64 = 0xC0FF_EE01;

/// The office's synthesis in its one ratified order: the one-shot pools, then
/// rain — one rng stream, so the order is the sound. Every painter builds
/// through here.
#[derive(Debug)]
pub struct Synthesis {
    rng: NoiseStream,
}

impl Default for Synthesis {
    fn default() -> Self {
        Self::new()
    }
}

impl Synthesis {
    pub fn new() -> Self {
        Self {
            rng: NoiseStream::new(BUILD_SEED),
        }
    }

    pub fn assets(&mut self) -> AssetBank {
        AssetBank::build(&mut self.rng)
    }

    pub fn rain(&mut self) -> Arc<Vec<f32>> {
        Arc::new(foley::rain_bed(&mut self.rng))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one ratified order, rebuilt by hand from the same functions in this
    /// process: pools, then rain, one stream.
    #[test]
    fn synthesis_draws_in_the_ratified_order() {
        let mut s = Synthesis::new();
        let assets = s.assets();
        let rain = s.rain();

        let mut rng = NoiseStream::new(BUILD_SEED);
        let want_assets = AssetBank::build(&mut rng);
        let want_rain = foley::rain_bed(&mut rng);

        for pool in OneShotPool::ALL {
            for i in 0..KEYSTROKE_POOL.max(DROP_POOL) {
                assert_eq!(
                    assets.get(pool, i),
                    want_assets.get(pool, i),
                    "{pool:?}[{i}]"
                );
            }
        }
        assert_eq!(*rain, want_rain);
    }
}
