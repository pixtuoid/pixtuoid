//! Render ONE deterministic frame of the live-office hero to a PNG — the site
//! backdrop's poster and the VIBING-playground poster. It renders the ACTUAL
//! wasm hero (same `Office`, same layout, same looped script) so the
//! poster→canvas crossfade dissolves in place rather than reframing.
//!
//! Determinism: a FIXED `--t0-ms` (or `--hour`, which maps onto a FIXED
//! reference calendar date rather than "now") plus a fixed `--advance-ms` give
//! the same bytes every run. Run under TZ=UTC, which `scripts/gen-media.py`
//! pins process-wide.
//!
//! Driven by the `wasm-still` job kind in `scripts/media.json`; not part of the
//! shipped wasm artifact (an example, native-only).

use std::process::ExitCode;

use pixtuoid_web::Office;

const USAGE: &str = "usage: hero_still <out.png> [--width W] [--height H] \
(--t0-ms EPOCH_MS | --hour 0-23) [--advance-ms MS] [--weather NAME] [--seed N] \
(<out.png> and the flags may appear in any order)";

// Each takes one value token — skipped in pairs so the lone positional
// (<out.png>) may appear first or last interchangeably.
const FLAGS_WITH_VALUE: &[&str] = &[
    "--width",
    "--height",
    "--t0-ms",
    "--advance-ms",
    "--hour",
    "--weather",
    "--seed",
];

fn positional_out(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if FLAGS_WITH_VALUE.contains(&a) {
            i += 2;
        } else if !a.starts_with("--") {
            return Some(a);
        } else {
            i += 1;
        }
    }
    None
}

// The wasm hero's canvas buffer for a 16:9 viewport.
const DEFAULT_WIDTH: u32 = 231;
const DEFAULT_HEIGHT: u32 = 130;
// Far enough into the loop that the cast has walked in and seated, rather than
// an empty office at t0.
const DEFAULT_ADVANCE_MS: u64 = 100_000;
// The hero backdrop (OfficeBackdrop.astro) is seed 0, so this default keeps
// `hero-wide.png` byte-identical; `--seed` matches a DIFFERENT live canvas.
const DEFAULT_SEED: u32 = 0;

// Arbitrary but FIXED — never "today": only the hour-of-day drives the sky, and
// pinning the date keeps an `--hour` render byte-identical across machines.
const T0_REFERENCE_YMD: (i32, u32, u32) = (2024, 6, 1);

fn arg<T: std::str::FromStr>(args: &[String], name: &str) -> Option<T> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

/// `hour` (0-23) → epoch millis on the fixed reference date, at `:00:00` UTC.
/// The office's sky decodes `t0_ms` via `chrono::Local`, so this only lands on
/// the same hour because the gen pipeline pins the process to `TZ=UTC`.
fn hour_to_t0_ms(hour: u32) -> Option<f64> {
    let (y, m, d) = T0_REFERENCE_YMD;
    chrono::NaiveDate::from_ymd_opt(y, m, d)?
        .and_hms_opt(hour, 0, 0)
        .map(|dt| dt.and_utc().timestamp_millis() as f64)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(out) = positional_out(&args) else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let width: u32 = arg(&args, "--width").unwrap_or(DEFAULT_WIDTH);
    let height: u32 = arg(&args, "--height").unwrap_or(DEFAULT_HEIGHT);
    let advance_ms: u64 = arg(&args, "--advance-ms").unwrap_or(DEFAULT_ADVANCE_MS);
    let seed: u32 = arg(&args, "--seed").unwrap_or(DEFAULT_SEED);
    let weather: Option<String> = arg(&args, "--weather");

    let t0_ms: f64 = match (arg::<u64>(&args, "--t0-ms"), arg::<u32>(&args, "--hour")) {
        (Some(t0), _) => t0 as f64,
        (None, Some(hour)) => match hour_to_t0_ms(hour) {
            Some(t0) => t0,
            None => {
                eprintln!("--hour must be 0-23, got {hour}");
                return ExitCode::FAILURE;
            }
        },
        (None, None) => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let mut office = match Office::new(seed) {
        Ok(o) => o,
        Err(_) => {
            eprintln!("bundled sprite pack failed to parse (build bug)");
            return ExitCode::FAILURE;
        }
    };
    if let Some(name) = weather {
        office.set_weather(Some(name));
    }
    // The first step anchors the script epoch at t0 (only the at_ms=0 beat is
    // due); the second advances the loop so the beats actually fire.
    office.step(t0_ms, width, height);
    office.step(t0_ms + advance_ms as f64, width, height);

    let px = office.frame().to_vec();
    let Some(img) = image::RgbaImage::from_raw(width, height, px) else {
        eprintln!("frame length didn't match {width}x{height}*4");
        return ExitCode::FAILURE;
    };
    if let Err(e) = img.save(out) {
        eprintln!("write {out}: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
