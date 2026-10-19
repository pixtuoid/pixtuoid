//! A `WebAudioDriver` that runs the SAME `pixtuoid_scene::audio` mixer the
//! native rodio thread runs, but instead of writing to a device it RECORDS the
//! sink commands each tick for the site's WebAudio glue to flush.
//!
//! Time is a PARAMETER (`now_ms` from JS): the driver never reads a clock, and
//! dt is CLAMPED — a backgrounded tab whose `now_ms` jumps seconds neither
//! ramp-snaps a gain nor replays a keystroke backlog.

use std::sync::Arc;

use pixtuoid_scene::audio::bank::{AssetBank, DROP_POOL, KEYSTROKE_POOL, Synthesis};
use pixtuoid_scene::audio::mixer::LoopStem;
use pixtuoid_scene::audio::{
    AudioEngine, AudioFrame, Controls, MAX_DT_S, OneShotPool, TickCommands,
};

#[derive(Debug)]
pub(crate) struct WebAudioDriver {
    synthesis: Synthesis,
    bank: Option<AssetBank>,
    rain: Option<Arc<Vec<f32>>>,

    engine: AudioEngine,
    last_ms: Option<f64>,
}

impl WebAudioDriver {
    /// Master is fixed 1.0: the web has no volume UI — mute = JS suspending the
    /// AudioContext, so the mixer always runs unmuted.
    pub(crate) fn new() -> Self {
        Self {
            synthesis: Synthesis::new(),
            bank: None,
            rain: None,
            engine: AudioEngine::new(1.0),
            last_ms: None,
        }
    }

    /// A driver assembled from worker-synthesized pieces, ready immediately.
    pub(crate) fn adopted(bank: AssetBank, rain: Arc<Vec<f32>>) -> Self {
        Self {
            bank: Some(bank),
            rain: Some(rain),
            ..Self::new()
        }
    }

    /// Build ONE synthesis piece (bank → rain, the [`Synthesis`] order),
    /// returning the pieces REMAINING. JS pumps this off `setTimeout(0)` so the
    /// main thread never blocks on the whole synthesis.
    pub(crate) fn warmup_step(&mut self) -> u32 {
        if self.bank.is_none() {
            self.bank = Some(self.synthesis.assets());
        } else if self.rain.is_none() {
            self.rain = Some(self.synthesis.rain());
        }
        u32::from(self.bank.is_none()) + u32::from(self.rain.is_none())
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.bank.is_some() && self.rain.is_some()
    }

    /// `now_ms` is the site's PAUSE-SHIFTED clock, so pause freezes the sound
    /// coherently.
    pub(crate) fn tick(&mut self, now_ms: f64, frame: AudioFrame) -> TickCommands {
        let dt = match self.last_ms {
            Some(prev) => (((now_ms - prev) / 1000.0) as f32).clamp(0.0, MAX_DT_S),
            None => 0.0,
        };
        self.last_ms = Some(now_ms);
        self.engine.tick(dt, Some(frame))
    }

    /// JS reads this zero-copy, so it must re-read after any wasm call that
    /// may `memory.grow`.
    pub(crate) fn loop_buffer(&self, idx: usize) -> &[f32] {
        match LoopStem::ALL.get(idx) {
            Some(LoopStem::Rain) => self.rain.as_deref().map(Vec::as_slice).unwrap_or(&[]),
            None => &[],
        }
    }

    /// MUST return empty for any `index` past the pool's end: the JS discovery
    /// loop grows until the first empty slot, so an unbounded non-empty would
    /// spin the browser's main thread forever.
    pub(crate) fn oneshot_buffer(&self, pool: OneShotPool, index: usize) -> &[f32] {
        let Some(bank) = &self.bank else {
            return &[];
        };
        bank.get(pool, index).map_or(&[], |a| a.as_slice())
    }
}

/// The worker-prewarm handoff: buffers synthesized in a Web Worker's OWN wasm
/// instance are copied here piece-by-piece, because a postMessage transfer
/// can't cross wasm memories. A torn handoff (worker died mid-stream) yields
/// `None` so the click-time warmup runs instead.
#[derive(Debug, Default)]
pub(crate) struct Adoption {
    keystrokes: Vec<Arc<Vec<f32>>>,
    drops: Vec<Arc<Vec<f32>>>,
    door_chime: Option<Arc<Vec<f32>>>,
    printer_whir: Option<Arc<Vec<f32>>>,
    vending_drop: Option<Arc<Vec<f32>>>,
    rain: Option<Arc<Vec<f32>>>,
}

impl Adoption {
    /// `false` — overflow, duplicate, or empty — aborts the handoff.
    pub(crate) fn push_oneshot(&mut self, pool: OneShotPool, samples: &[f32]) -> bool {
        if samples.is_empty() {
            return false;
        }
        let arc = Arc::new(samples.to_vec());
        let slot = |o: &mut Option<Arc<Vec<f32>>>| {
            if o.is_some() {
                return false;
            }
            *o = Some(arc.clone());
            true
        };
        match pool {
            OneShotPool::Keystroke => {
                if self.keystrokes.len() >= KEYSTROKE_POOL {
                    return false;
                }
                self.keystrokes.push(arc.clone());
                true
            }
            OneShotPool::Drop => {
                if self.drops.len() >= DROP_POOL {
                    return false;
                }
                self.drops.push(arc.clone());
                true
            }
            OneShotPool::DoorChime => slot(&mut self.door_chime),
            OneShotPool::PrinterWhir => slot(&mut self.printer_whir),
            OneShotPool::VendingDrop => slot(&mut self.vending_drop),
        }
    }

    /// Loop stem `idx` in `LoopStem::ALL` order; a stem twice aborts.
    pub(crate) fn push_loop(&mut self, idx: usize, samples: &[f32]) -> bool {
        if samples.is_empty() {
            return false;
        }
        match LoopStem::ALL.get(idx) {
            Some(LoopStem::Rain) => {
                if self.rain.is_some() {
                    return false;
                }
                self.rain = Some(Arc::new(samples.to_vec()));
                true
            }
            None => false,
        }
    }

    pub(crate) fn finish(self) -> Option<WebAudioDriver> {
        let bank = AssetBank::adopt(
            self.keystrokes,
            self.drops,
            self.door_chime?,
            self.printer_whir?,
            self.vending_drop?,
        )?;
        Some(WebAudioDriver::adopted(bank, self.rain?))
    }
}

pub(crate) fn commands_json(cmd: &TickCommands) -> String {
    let mut out = String::from("{\"gains\":[");
    for (i, g) in cmd.gains.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&fmt_f32(*g));
    }
    out.push_str("],\"plays\":[");
    for (i, p) in cmd.plays.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "[{},{},{}]",
            p.pool.wire(),
            p.index,
            fmt_f32(p.gain)
        ));
    }
    out.push_str("],\"music\":");
    match cmd.music {
        Some(m) => {
            let [daylight, intensity, rain] = controls_wire(m.controls);
            out.push_str(&format!(
                "{{\"controls\":[{},{},{}],\"gain\":{}}}",
                fmt_f32(daylight),
                fmt_f32(intensity),
                fmt_f32(rain),
                fmt_f32(m.gain)
            ));
        }
        None => out.push_str("null"),
    }
    out.push('}');
    out
}

/// The music's controls as the wire carries them to the worker, which hands
/// them back to [`controls_from_wire`] unread.
fn controls_wire(c: Controls) -> [f32; 3] {
    [c.daylight, c.intensity, c.rain]
}

/// [`controls_wire`]'s inverse; `None` for anything but its three values.
pub(crate) fn controls_from_wire(wire: &[f32]) -> Option<Controls> {
    let &[daylight, intensity, rain] = wire else {
        return None;
    };
    Some(
        Controls::default()
            .with_daylight(daylight)
            .with_intensity(intensity)
            .with_rain(rain),
    )
}

/// Non-finite is impossible here (gains are bounded ramps, controls bounded
/// levels) but maps to 0 defensively, so the JSON stays valid.
fn fmt_f32(v: f32) -> String {
    if v.is_finite() {
        format!("{v:.5}")
    } else {
        "0".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_scene::audio::{PlayCmd, bank};

    /// The index of `stem` in a tick's `gains` and the loop wire.
    fn at(stem: LoopStem) -> usize {
        LoopStem::ALL
            .iter()
            .position(|s| *s == stem)
            .expect("every stem is in ALL")
    }

    /// A busy office in the rain: every loop stem and the typing above zero.
    fn busy_rainy() -> AudioFrame {
        AudioFrame {
            stems: pixtuoid_scene::audio::stem_levels(
                &pixtuoid_scene::tally::StateCounts {
                    active: 3,
                    waiting: 0,
                    idle: 0,
                    exiting: 0,
                    total: 3,
                },
                0.5,
            ),
            events: Vec::new(),
            music: Controls::default().with_daylight(0.0),
        }
    }

    #[test]
    fn warmup_draws_in_the_synthesis_order() {
        let mut d = WebAudioDriver::new();
        while d.warmup_step() > 0 {}
        assert!(d.is_ready());

        let mut want = bank::Synthesis::new();
        let assets = want.assets();
        let rain = want.rain();

        for pool in OneShotPool::ALL {
            let mut index = 0;
            while let Some(buf) = assets.get(pool, index) {
                assert_eq!(
                    d.oneshot_buffer(pool, index),
                    buf.as_slice(),
                    "{pool:?} {index}"
                );
                index += 1;
            }
        }
        assert_eq!(d.loop_buffer(at(LoopStem::Rain)), rain.as_slice());
    }

    #[test]
    fn every_oneshot_pool_has_a_finite_end_the_js_discovery_loop_can_find() {
        let mut d = WebAudioDriver::new();
        while d.warmup_step() > 0 {}
        for (pool, size) in ONESHOT_POOL_SIZES {
            for j in 0..size {
                assert!(
                    !d.oneshot_buffer(pool, j).is_empty(),
                    "{pool:?}[{j}] present"
                );
            }
            assert!(
                d.oneshot_buffer(pool, size).is_empty(),
                "{pool:?}[{size}] must be EMPTY — the discovery-loop terminator"
            );
        }
    }

    #[test]
    fn commands_json_is_parseable_and_pool_wire_round_trips() {
        let cmd = TickCommands {
            gains: [0.35; LoopStem::ALL.len()],
            plays: vec![
                PlayCmd {
                    pool: OneShotPool::Keystroke,
                    index: 7,
                    gain: 0.12,
                },
                PlayCmd {
                    pool: OneShotPool::PrinterWhir,
                    index: 0,
                    gain: 0.5,
                },
            ],
            music: Some(pixtuoid_scene::audio::MusicCmd {
                controls: Controls::default()
                    .with_daylight(0.25)
                    .with_intensity(1.0)
                    .with_rain(0.5),
                gain: 0.75,
            }),
        };
        let json = commands_json(&cmd);
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["music"]["gain"], 0.75);
        let wire: Vec<f32> =
            serde_json::from_value(v["music"]["controls"].clone()).expect("floats");
        assert_eq!(
            controls_from_wire(&wire),
            cmd.music.map(|m| m.controls),
            "the worker hands back what the wire carried"
        );
        assert_eq!(controls_from_wire(&wire[..2]), None);
        let silent: serde_json::Value =
            serde_json::from_str(&commands_json(&TickCommands::default())).expect("valid JSON");
        assert!(silent["music"].is_null(), "no frame, no song");
        assert_eq!(v["gains"].as_array().unwrap().len(), LoopStem::ALL.len());
        assert_eq!(v["gains"][at(LoopStem::Rain)].as_f64().unwrap(), 0.35);
        assert_eq!(v["plays"][0][0], 0, "keystroke wire = 0");
        assert_eq!(v["plays"][0][1], 7, "keystroke index");
        assert_eq!(v["plays"][1][0], 3, "printer wire = 3");
        for p in OneShotPool::ALL {
            assert_eq!(OneShotPool::from_wire(p.wire()), Some(p));
        }
        assert_eq!(OneShotPool::from_wire(9), None);
    }

    #[test]
    fn warmup_builds_the_bank_then_rain_then_is_ready() {
        let mut d = WebAudioDriver::new();
        assert!(!d.is_ready());
        assert_eq!(d.warmup_step(), 1);
        assert!(!d.oneshot_buffer(OneShotPool::DoorChime, 0).is_empty());
        assert!(!d.is_ready(), "not ready before the rain bed");
        assert_eq!(d.warmup_step(), 0);
        assert!(d.is_ready());
        assert!(
            !d.loop_buffer(at(LoopStem::Rain)).is_empty(),
            "rain bed ready"
        );
        assert_eq!(d.warmup_step(), 0, "warmup is idempotent once ready");
        for i in 0..bank::KEYSTROKE_POOL {
            assert!(!d.oneshot_buffer(OneShotPool::Keystroke, i).is_empty());
        }
    }

    #[test]
    fn tick_ramps_loop_gains_and_fires_scheduled_typing() {
        let mut d = WebAudioDriver::new();
        while d.warmup_step() > 0 {}
        // 200 ticks × 50ms = 10s, so the typing scheduler's first burst gap
        // (~2-3s at this rate) fires well inside the window.
        let mut now = 0.0;
        let mut typed = 0;
        let mut last_rain = 0.0;
        for _ in 0..200 {
            now += 50.0;
            let cmd = d.tick(now, busy_rainy());
            last_rain = cmd.gains[at(LoopStem::Rain)];
            typed += cmd
                .plays
                .iter()
                .filter(|p| p.pool == OneShotPool::Keystroke)
                .count();
        }
        assert!(last_rain > 0.0, "rain gain ramped up");
        assert_eq!(
            d.tick(now + 50.0, busy_rainy()).music.map(|m| m.controls),
            Some(busy_rainy().music),
            "the frame's music goes out with the tick"
        );
        assert!(
            typed > 0,
            "the typing scheduler fired keystrokes for a busy office"
        );
    }

    #[test]
    fn a_big_time_gap_does_not_snap_the_ramp_or_burst_typing() {
        let mut d = WebAudioDriver::new();
        while d.warmup_step() > 0 {}
        d.tick(0.0, busy_rainy());
        let cmd = d.tick(30_000.0, busy_rainy());
        assert!(
            cmd.gains[at(LoopStem::Rain)]
                <= pixtuoid_scene::audio::mixer::RAMP_PER_S * MAX_DT_S + 1e-4,
            "one clamped tick can't snap the rain gain to full"
        );
        assert!(
            cmd.plays
                .iter()
                .filter(|p| p.pool == OneShotPool::Keystroke)
                .count()
                < 50,
            "the scheduler clock advanced by the clamped dt, not 30s of backlog"
        );
    }

    fn adopt_all_of(src: &WebAudioDriver) -> Adoption {
        let mut ad = Adoption::default();
        for i in 0..LoopStem::ALL.len() {
            assert!(ad.push_loop(i, src.loop_buffer(i)), "loop {i}");
        }
        for (pool, size) in ONESHOT_POOL_SIZES {
            for j in 0..size {
                assert!(
                    ad.push_oneshot(pool, src.oneshot_buffer(pool, j)),
                    "{pool:?}[{j}]"
                );
            }
        }
        ad
    }

    const ONESHOT_POOL_SIZES: [(OneShotPool, usize); 5] = [
        (OneShotPool::Keystroke, KEYSTROKE_POOL),
        (OneShotPool::Drop, DROP_POOL),
        (OneShotPool::DoorChime, 1),
        (OneShotPool::PrinterWhir, 1),
        (OneShotPool::VendingDrop, 1),
    ];

    #[test]
    fn adoption_round_trips_a_warmed_driver_byte_for_byte() {
        let mut src = WebAudioDriver::new();
        while src.warmup_step() > 0 {}
        let dst = adopt_all_of(&src).finish().expect("complete handoff");
        assert!(dst.is_ready(), "adopted driver is ready with zero warmup");
        for i in 0..LoopStem::ALL.len() {
            assert_eq!(dst.loop_buffer(i), src.loop_buffer(i), "stem {i} bytes");
        }
        for (pool, size) in ONESHOT_POOL_SIZES {
            for j in 0..size {
                assert_eq!(dst.oneshot_buffer(pool, j), src.oneshot_buffer(pool, j));
            }
            assert!(
                dst.oneshot_buffer(pool, size).is_empty(),
                "{pool:?} keeps the discovery-loop terminator"
            );
        }
    }

    #[test]
    fn an_adopted_driver_ticks_like_a_warmed_one() {
        let mut src = WebAudioDriver::new();
        while src.warmup_step() > 0 {}
        let mut d = adopt_all_of(&src).finish().expect("handoff");
        assert!(d.is_ready(), "an adopted driver needs no warmup");
        // 200 ticks × 50 ms: the typing and the rain's drops schedule plays.
        let mut now = 0.0;
        let mut rain = 0.0;
        let mut plays = 0;
        for _ in 0..200 {
            now += 50.0;
            let cmd = d.tick(now, busy_rainy());
            rain = cmd.gains[at(LoopStem::Rain)];
            for p in &cmd.plays {
                assert_eq!(
                    d.oneshot_buffer(p.pool, p.index),
                    src.oneshot_buffer(p.pool, p.index),
                    "{:?}[{}] plays the adopted buffer",
                    p.pool,
                    p.index
                );
                plays += 1;
            }
        }
        assert!(rain > 0.0, "adopted mix ramps up");
        assert!(plays > 0, "premise: the engine scheduled plays");
    }

    #[test]
    fn a_torn_adoption_yields_none_and_bad_pushes_are_refused() {
        let mut src = WebAudioDriver::new();
        while src.warmup_step() > 0 {}
        let mut ad = adopt_all_of(&src);
        ad.rain = None;
        assert!(ad.finish().is_none(), "missing rain is a torn handoff");
        let rain = src.loop_buffer(at(LoopStem::Rain));
        let mut ad = Adoption::default();
        assert!(
            !ad.push_loop(LoopStem::ALL.len(), rain),
            "an unknown loop is refused"
        );
        assert!(
            !ad.push_loop(at(LoopStem::Rain), &[]),
            "empty buffers are refused"
        );
        let mut ad = adopt_all_of(&src);
        assert!(
            !ad.push_loop(at(LoopStem::Rain), rain),
            "a second rain bed is refused"
        );
        assert!(
            !ad.push_oneshot(
                OneShotPool::Keystroke,
                src.oneshot_buffer(OneShotPool::Keystroke, 0)
            ),
            "keystroke overflow refused"
        );
        assert!(
            !ad.push_oneshot(
                OneShotPool::DoorChime,
                src.oneshot_buffer(OneShotPool::DoorChime, 0)
            ),
            "duplicate cue refused"
        );
        let mut ad = Adoption::default();
        for i in 0..LoopStem::ALL.len() {
            ad.push_loop(i, src.loop_buffer(i));
        }
        assert!(ad.finish().is_none(), "missing one-shots is a torn handoff");
    }
}
