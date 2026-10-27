//! The office's foley — keystrokes, the door chime, the printer, the vending
//! machine and the rain. Office props and weather, so they sit beside the bank
//! that pools them rather than in the music synth: they change when the office
//! does. Pure buffer generators (mono f32 at [`SAMPLE_RATE`]), run once at
//! startup; every constant is an auditioned, ratified value. Tuned with
//! [procedural-lofi's analyzers](https://github.com/pixtuoid/lofi/tree/363576492e238a3a2df41af352e170475928457d/.claude/skills/procedural-lofi/scripts).

use pixtuoid_lofi::dsp::{Band, NoiseStream, bandpass, lowpass, shaped_noise_loop};

use super::SAMPLE_RATE;

const SR: f32 = SAMPLE_RATE as f32;

/// The rain wash's seamless loop. A power of two: `shaped_noise_loop` shapes in
/// the FFT domain, which is circular, so the block loops click-free.
const RAIN_LOOP_SAMPLES: usize = 1 << 19;

// Copied, not shared: the music voices live in `pixtuoid-lofi`, and
// exporting five small helpers for one consumer would make them its semver
// surface. Each copy shapes only its own module's sounds.
fn n_samples(dur_s: f32) -> usize {
    (dur_s * SR).round() as usize
}

fn env_ar(n: usize, attack_s: f32, release_s: f32) -> impl Fn(usize) -> f32 {
    let na = n_samples(attack_s).min(n).max(1);
    let nr = n_samples(release_s).min(n).max(1);
    move |i| {
        let a = (i as f32 / na as f32).min(1.0);
        let r = ((n - i) as f32 / nr as f32).min(1.0);
        a.min(r)
    }
}

fn place(buf: &mut [f32], snippet: &[f32], at_s: f32, gain: f32) {
    let start = (at_s * SR) as usize;
    for (i, &s) in snippet.iter().enumerate() {
        if let Some(slot) = buf.get_mut(start + i) {
            *slot += s * gain;
        }
    }
}

fn normalize(buf: &mut [f32], peak: f32) {
    let m = buf.iter().fold(0.0f32, |a, &v| a.max(v.abs())).max(1e-9);
    buf.iter_mut().for_each(|v| *v *= peak / m);
}

const MIDI_A4: f32 = 440.0;
fn midi_freq(m: f32) -> f32 {
    MIDI_A4 * 2f32.powf((m - 69.0) / 12.0)
}

/// One mechanical-keyboard stroke — the BRIGHT office clack, most of its energy
/// in 1-4kHz. The ASMR-lore deep thock measured OPPOSITE to the owner's
/// reference, so don't retune this darker.
pub(crate) fn keystroke(rng: &mut NoiseStream) -> Vec<f32> {
    let d = 0.05;
    let n = n_samples(d);
    let f_lo = 1250.0 + 450.0 * rng.unit();
    let raw: Vec<f32> = (0..n).map(|_| rng.norm()).collect();
    let click = bandpass(&raw, f_lo, f_lo + 2100.0);
    let f_b = 1000.0 + 400.0 * rng.unit();
    let raw_b: Vec<f32> = (0..n).map(|_| rng.norm()).collect();
    let body = bandpass(&raw_b, f_b, f_b + 900.0);
    let raw_s: Vec<f32> = (0..n).map(|_| rng.norm()).collect();
    let spice = bandpass(&raw_s, 4200.0, 7500.0);

    let up_at = 0.055 + 0.03 * rng.unit();
    let du = 0.02;
    let nu = n_samples(du);
    let raw_u: Vec<f32> = (0..nu).map(|_| rng.norm()).collect();
    let up = bandpass(&raw_u, 2000.0, 4500.0);

    let total = n_samples(up_at) + nu + 8;
    let mut buf = vec![0.0f32; total];
    let layers = click.iter().zip(&body).zip(&spice);
    for (i, (b, ((click, body), spice))) in buf.iter_mut().zip(layers).enumerate() {
        let t = i as f32 / SR;
        *b = click * (-t * 330.0).exp()
            + body * (-t * 280.0).exp() * 1.1
            + spice * (-t * 500.0).exp() * 0.18;
    }
    let up_scaled: Vec<f32> = up
        .iter()
        .enumerate()
        .map(|(i, &v)| v * (-(i as f32 / SR) * 520.0).exp())
        .collect();
    place(&mut buf, &up_scaled, up_at, 0.35);
    normalize(&mut buf, 0.8);
    buf
}

/// Door chime — a DESCENDING ding-dong (E5 → C5), warm harmonic bells.
pub(crate) fn door_chime() -> Vec<f32> {
    let mut buf = vec![0.0f32; n_samples(2.0)];
    let tau = std::f32::consts::TAU;
    for &(at, m, g) in &[(0.0f32, 76.0f32, 0.8f32), (0.42, 72.0, 1.0)] {
        let d = 1.5;
        let nn = n_samples(d);
        let f = midi_freq(m);
        let note: Vec<f32> = (0..nn)
            .map(|i| {
                let t = i as f32 / SR;
                (tau * f * t).sin() * (-t * 2.6).exp()
                    + 0.35 * (tau * 2.0 * f * t).sin() * (-t * 5.0).exp()
                    + 0.10 * (tau * 3.0 * f * t).sin() * (-t * 9.0).exp()
            })
            .collect();
        place(&mut buf, &note, at, g * 0.7);
    }
    let mut out = lowpass(&buf, 4000.0);
    normalize(&mut out, 0.55);
    out
}

pub(crate) fn printer_whir(rng: &mut NoiseStream) -> Vec<f32> {
    let dur = 1.5;
    let n = n_samples(dur);
    let mut buf = vec![0.0f32; n];
    let env = env_ar(n, 0.12, 0.3);
    let env_tex = env_ar(n, 0.15, 0.35);
    let raw: Vec<f32> = (0..n).map(|_| rng.norm()).collect();
    let texture = bandpass(&raw, 400.0, 2600.0);
    let tau = std::f32::consts::TAU;
    let mut phase = 0.0f32;
    for (i, (b, texture)) in buf.iter_mut().zip(&texture).enumerate() {
        let t = i as f32 / SR;
        let f =
            80.0 + 50.0 * (t / 0.25).clamp(0.0, 1.0) - 30.0 * ((t - 1.15) / 0.35).clamp(0.0, 1.0);
        phase += tau * f / SR;
        let motor = phase.sin() + 0.45 * (2.0 * phase).sin() + 0.2 * (3.0 * phase).sin();
        *b = motor * env(i) * 0.5 + texture * env_tex(i) * 0.16;
    }
    let mut at = 0.28f32;
    while at < 1.05 {
        let dn = n_samples(0.014);
        let raw_t: Vec<f32> = (0..dn).map(|_| rng.norm()).collect();
        let tick: Vec<f32> = bandpass(&raw_t, 1500.0, 4200.0)
            .iter()
            .enumerate()
            .map(|(i, &v)| v * (-(i as f32 / SR) * 260.0).exp())
            .collect();
        place(&mut buf, &tick, at, 0.5 + 0.2 * rng.unit());
        at += 0.09 + 0.02 * rng.unit();
    }
    let sw_n = n_samples(0.5);
    let raw_s: Vec<f32> = (0..sw_n).map(|_| rng.norm()).collect();
    let sw: Vec<f32> = bandpass(&raw_s, 900.0, 3000.0)
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let w = (std::f32::consts::PI * i as f32 / sw_n as f32).sin();
            v * w * w * 0.35
        })
        .collect();
    place(&mut buf, &sw, 0.55, 1.0);
    let mut out = lowpass(&buf, 5000.0);
    normalize(&mut out, 0.55);
    out
}

/// Vending machine: mechanism click → beat → the can DROPS.
pub(crate) fn vending_drop(rng: &mut NoiseStream) -> Vec<f32> {
    let mut buf = vec![0.0f32; n_samples(0.7)];
    let cn = n_samples(0.03);
    let raw_c: Vec<f32> = (0..cn).map(|_| rng.norm()).collect();
    let click: Vec<f32> = bandpass(&raw_c, 800.0, 2000.0)
        .iter()
        .enumerate()
        .map(|(i, &v)| v * (-(i as f32 / SR) * 200.0).exp())
        .collect();
    place(&mut buf, &click, 0.0, 0.5);
    let tau = std::f32::consts::TAU;
    let thud = |rng: &mut NoiseStream| -> Vec<f32> {
        let dn = n_samples(0.12);
        let raw: Vec<f32> = (0..dn).map(|_| rng.norm()).collect();
        let rattle = bandpass(&raw, 300.0, 900.0);
        rattle
            .iter()
            .enumerate()
            .map(|(i, rattle)| {
                let t = i as f32 / SR;
                (tau * 170.0 * t).sin() * (-t * 70.0).exp() + rattle * (-t * 150.0).exp() * 0.4
            })
            .collect()
    };
    let b0 = thud(rng);
    let b1 = thud(rng);
    let b2 = thud(rng);
    place(&mut buf, &b0, 0.18, 1.0);
    place(&mut buf, &b1, 0.27, 0.5);
    place(&mut buf, &b2, 0.33, 0.25);
    let mut out = lowpass(&buf, 3500.0);
    normalize(&mut out, 0.55);
    out
}

/// The gentle-rain octave-band envelope, measured from the owner's chosen
/// reference recording.
const GENTLE_RAIN_BANDS: [Band; 9] = [
    Band::new(20.0, 60.0, 0.015),
    Band::new(60.0, 125.0, 0.035),
    Band::new(125.0, 250.0, 0.069),
    Band::new(250.0, 500.0, 0.143),
    Band::new(500.0, 1000.0, 0.244),
    Band::new(1000.0, 2000.0, 0.253),
    Band::new(2000.0, 4000.0, 0.102),
    Band::new(4000.0, 8000.0, 0.089),
    Band::new(8000.0, 16000.0, 0.040),
];

/// The rain WASH only — audible foreground drops are scattered at runtime from
/// `rain_drop`'s pool, so rain never repeats.
pub(crate) fn rain_bed(rng: &mut NoiseStream) -> Vec<f32> {
    shaped_noise_loop(RAIN_LOOP_SAMPLES, &GENTLE_RAIN_BANDS, rng)
}

/// One audible raindrop for the runtime scatter pool: dull plop on wood/soil,
/// water plip, or bright ping on metal/glass.
pub(crate) fn rain_drop(rng: &mut NoiseStream) -> Vec<f32> {
    let d = 0.10;
    let n = n_samples(d);
    let kind = rng.unit();
    let (f0, decay, spl_gain, spl_lo, spl_hi) = if kind < 0.20 {
        (320.0 + 300.0 * rng.unit(), 55.0, 0.12, 900.0, 2200.0)
    } else if kind < 0.75 {
        (700.0 + 700.0 * rng.unit(), 62.0, 0.25, 1200.0, 4000.0)
    } else {
        (1800.0 + 1200.0 * rng.unit(), 80.0, 0.15, 2500.0, 6000.0)
    };
    let raw: Vec<f32> = (0..n).map(|_| rng.norm()).collect();
    let splash = bandpass(&raw, spl_lo, spl_hi);
    let tau = std::f32::consts::TAU;
    let mut phase = 0.0f32;
    let mut buf: Vec<f32> = splash
        .iter()
        .enumerate()
        .map(|(i, splash)| {
            let t = i as f32 / SR;
            let f = f0 * (1.0 + 0.12 * t / d); // the rising Minnaert chirp
            phase += tau * f / SR;
            phase.sin() * (-t * decay).exp() + splash * (-t * 180.0).exp() * spl_gain
        })
        .collect();
    normalize(&mut buf, 1.0);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_lofi::dsp::{band_energy_share, centroid_hz};

    #[test]
    fn one_shot_synth_spectral_sanity() {
        let mut rng = NoiseStream::new(42);
        let key = keystroke(&mut rng);
        let c_key = centroid_hz(&key);
        assert!(
            (1800.0..=3200.0).contains(&c_key),
            "keystroke centroid {c_key} outside the bright-clack band"
        );

        let chime = door_chime();
        let c_chime = centroid_hz(&chime);
        assert!(
            c_chime < 700.0,
            "door chime centroid {c_chime} must stay the warm low bell (~556Hz ratified)"
        );
    }

    #[test]
    fn rain_bed_matches_the_gentle_reference_envelope() {
        let mut rng = NoiseStream::new(3);
        let bed = rain_bed(&mut rng);
        for band in GENTLE_RAIN_BANDS {
            let (lo, hi) = (band.lo_hz(), band.hi_hz());
            let share = band_energy_share(&bed, lo, hi);
            assert!(
                (share - band.share()).abs() < 0.04,
                "band {lo}-{hi}: {share:.3} vs target {:.3}",
                band.share()
            );
        }
    }

    #[test]
    fn rain_bed_is_one_rain_loop() {
        assert_eq!(
            rain_bed(&mut NoiseStream::new(1)).len(),
            1 << 19,
            "the ratified rain loop: a new length is a new sound"
        );
    }

    #[test]
    fn every_foley_buffer_is_finite_and_peak_bounded() {
        let mut rng = NoiseStream::new(9);
        for (name, buf) in [
            ("keystroke", keystroke(&mut rng)),
            ("chime", door_chime()),
            ("printer", printer_whir(&mut rng)),
            ("vending", vending_drop(&mut rng)),
            ("drop", rain_drop(&mut rng)),
        ] {
            assert!(!buf.is_empty(), "{name} empty");
            assert!(
                buf.iter().all(|v| v.is_finite() && v.abs() <= 1.0),
                "{name} has NaN/over-peak samples"
            );
        }
    }
}
