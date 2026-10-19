//! The pixtuoid program: `main.rs` only calls [`run`] and reports its error
//! through [`fatal_error_text`]. What else is public is for the examples and
//! the integration tests, which are separate crates.

// A print macro panics when its reader leaves (`| head`): CLI and headless
// output goes through a `CliOut`, a stderr notice through a `let _ = writeln!`.
#![cfg_attr(not(test), warn(clippy::print_stdout, clippy::print_stderr))]
// AGENTS.md's Errors rule, as a lint: no `unwrap()` outside tests.
#![cfg_attr(not(test), warn(clippy::unwrap_used))]

mod app;
pub use app::run;
pub(crate) mod audio;
pub(crate) mod cli;
pub(crate) mod config;
pub(crate) mod doctor;

#[cfg(feature = "upstream")]
mod drift_watch;
pub(crate) mod floating;
pub(crate) mod focus;
pub(crate) mod graphics;
pub(crate) use graphics::GraphicsMode;
pub(crate) mod install;
pub(crate) mod jank;
pub(crate) mod pacing;
pub(crate) mod panels;
pub(crate) mod run_log;
pub(crate) mod runtime;
pub(crate) mod sources;
pub(crate) mod term;
pub(crate) mod tui;
pub(crate) mod version;

/// What the examples, the integration tests and the benches reach: each is a
/// separate crate, and this is the only path they have into the lib, so a
/// `pub` item anywhere else that none of them names is one `unreachable_pub`
/// reports. Not an API: it moves with them.
#[doc(hidden)]
pub mod dev {
    pub use crate::audio::demote_to_utility;
    pub use crate::config::{FLOATING_DEFAULT_H, FLOATING_DEFAULT_W};
    pub use crate::doctor::footer_warning;
    /// The upstream drift watch, apart from [`crate::run`], the program.
    #[cfg(feature = "upstream")]
    pub mod drift_watch {
        pub use crate::drift_watch::{Report, run};
    }
    pub use crate::floating::compose::{Change, XrgbSurface, composite, composition};
    pub use crate::floating::geometry::{Zoom, window_geometry};
    pub use crate::floating::offscreen::{OfficeRenderer, Pressing, WindowFrame};
    pub use crate::floating::overlays::{OverlayLayers, Overlays, footer_budget, panel_preview};
    pub use crate::pacing::{Protocol, cutaway_office, forget_frame, renderer, warm};
    pub use crate::panels::OverlayFrame;
    pub use crate::panels::connection::{ConnectionFrame, DaemonRollup, LiveFacet, LiveInfo};
    pub use crate::panels::dashboard::{DashboardFolds, DashboardFrame, build_dashboard_rows};
    pub use crate::panels::ui_state::{RenderFrames, VersionPopupFrame, popup_whole};
    pub use crate::panels::welcome::{OnboardingFrame, WelcomeRow, dim_opening};
    pub use crate::panels::widgets::{footer_context, grid_of};
    pub use crate::sources::{ConnState, ConnectionRow};
    pub use crate::tui::renderer::{DrawCtx, PetState, draw_scene, scene_buf_size};
    pub use crate::tui::tui_renderer::TuiRenderer;
}

/// Strip control characters (Cc) and bidi controls from an untrusted string
/// before it reaches a terminal: such a value can carry control bytes that
/// reposition the cursor or inject escapes, or reorder the text shown. One
/// chokepoint, so the policy can't drift across its call sites.
///
/// The core crate strips with `pixtuoid_core::source::decoder::display_safe`, a
/// per-crate copy of this predicate pinned to it by
/// `the_bidi_table_matches_pixtuoid_cores_display_safe`.
pub(crate) fn strip_control_chars(s: &str) -> String {
    s.chars().filter(|c| !is_terminal_unsafe(*c)).collect()
}

/// A character that can drive a terminal or reorder the text it shows.
pub(crate) fn is_terminal_unsafe(c: char) -> bool {
    c.is_control() || is_bidi_control(c)
}

/// `s` stripped line by line, the lines rejoined with `sep`, so a multi-line
/// message keeps its shape.
pub(crate) fn strip_lines(s: &str, sep: &str) -> String {
    s.lines()
        .map(strip_control_chars)
        .collect::<Vec<_>>()
        .join(sep)
}

/// A path as a terminal shows it, stripped. A path from config, env or a
/// hand-editable hook command is stripped where it enters the text that
/// quotes it; per-output-site stripping already missed the `doctor` stdout
/// path once.
pub(crate) fn display_path(p: &std::path::Path) -> String {
    strip_control_chars(&p.display().to_string())
}

/// A CLI command's stdout. A reader that leaves early (`| head`) is not a
/// failure: a write into a broken pipe succeeds, marks the stream
/// [`closed`](Self::closed) and drops every later write, so the command still
/// completes its side effects and exits with its own verdict, where `println!`
/// would panic. Any other write error still fails.
#[derive(Debug)]
pub(crate) struct CliOut<W> {
    inner: W,
    closed: bool,
}

impl<W: std::io::Write> CliOut<W> {
    /// `inner`, with a broken pipe treated as the reader leaving.
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            closed: false,
        }
    }

    /// Whether the reader has gone away.
    pub(crate) fn closed(&self) -> bool {
        self.closed
    }

    fn absorb_broken_pipe<T>(
        &mut self,
        result: std::io::Result<T>,
        on_close: T,
    ) -> std::io::Result<T> {
        match result {
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                self.closed = true;
                Ok(on_close)
            }
            other => other,
        }
    }
}

impl<W: std::io::Write> std::io::Write for CliOut<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.closed {
            return Ok(buf.len());
        }
        let result = self.inner.write(buf);
        self.absorb_broken_pipe(result, buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.closed {
            return Ok(());
        }
        let result = self.inner.flush();
        self.absorb_broken_pipe(result, ())
    }
}

/// Stdout for a CLI command's output; see [`CliOut`].
pub(crate) fn cli_stdout() -> CliOut<std::io::Stdout> {
    CliOut::new(std::io::stdout())
}

/// A fatal error's chain as `main` returning `Err` would print it, stripped line
/// by line so the chain keeps its shape. Mechanism for `main`, not contract.
#[doc(hidden)]
pub fn fatal_error_text(e: &anyhow::Error) -> String {
    format!("Error: {}", strip_lines(&format!("{e:?}"), "\n"))
}

/// Run `cmd` and collect its output, or kill and reap it at `timeout`:
/// [`Command::output`](std::process::Command::output) has no timeout. `None`
/// on a spawn or wait error too.
///
/// The caller pipes the streams it reads (a spawned child inherits by default)
/// and expects little output: nothing drains the pipes until the child exits,
/// so a child that fills one blocks and runs into the timeout.
pub(crate) fn output_within(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> Option<std::process::Output> {
    use std::time::{Duration, Instant};
    const POLL_INTERVAL: Duration = Duration::from_millis(20);
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(_) => return None,
        }
    }
    child.wait_with_output().ok()
}

/// The one URL launcher both painters call: `open::that` blocks on Linux until the
/// browser exits (`xdg-open`), freezing frames, input and the quit arm on the event
/// loop. A failed launch only logs: the click does nothing on screen.
pub(crate) fn open_url(url: &str) {
    if let Err(e) = open::that_detached(url) {
        tracing::warn!(error = %e, url = ?url, "opening a url failed");
    }
}

/// The Unicode Bidi_Control characters. `char::is_control` covers only category
/// Cc; these are Cf and slip through — yet they REORDER displayed text in a
/// terminal (the "Trojan Source" class, CVE-2021-42574).
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'                    // ALM
            | '\u{200E}'..='\u{200F}' // LRM, RLM
            | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
            | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
    )
}

/// Test-only `tracing` capture for asserting on what reaches the log sink.
#[cfg(test)]
pub(crate) mod test_capture {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    pub(crate) struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl MakeWriter<'_> for Buf {
        type Writer = Buf;
        fn make_writer(&self) -> Buf {
            self.clone()
        }
    }

    /// Capture in the line format the bin's `logging::init` writes to its log
    /// file (no ANSI), so an assertion is validated against the REAL format, not
    /// an assumed one.
    pub(crate) fn capture(f: impl FnOnce()) -> String {
        capture_with(crate::app::logging::fmt_layer().with_ansi(false), f)
    }

    /// What `layer` writes of what `f` logs.
    pub(crate) fn capture_with<N, E>(
        layer: tracing_subscriber::fmt::Layer<tracing_subscriber::Registry, N, E>,
        f: impl FnOnce(),
    ) -> String
    where
        N: for<'w> tracing_subscriber::fmt::FormatFields<'w> + Send + Sync + 'static,
        E: tracing_subscriber::fmt::FormatEvent<tracing_subscriber::Registry, N>
            + Send
            + Sync
            + 'static,
    {
        use tracing_subscriber::layer::SubscriberExt;
        let buf = Buf::default();
        let sub = tracing_subscriber::registry().with(layer.with_writer(buf.clone()));
        tracing::subscriber::with_default(sub, f);
        let bytes = buf.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }
}

#[cfg(test)]
pub(crate) mod test_io {
    /// Fails ONE write, the `fail_at`th (0-based), with `kind`, and accepts
    /// every other: a stream that recovers is what shows a caller stopped
    /// writing rather than kept hitting the error.
    pub(crate) struct FailOnce {
        pub(crate) fail_at: usize,
        pub(crate) kind: std::io::ErrorKind,
        pub(crate) calls: usize,
        pub(crate) written: Vec<u8>,
    }

    impl FailOnce {
        pub(crate) fn new(fail_at: usize, kind: std::io::ErrorKind) -> Self {
            Self {
                fail_at,
                kind,
                calls: 0,
                written: Vec::new(),
            }
        }
    }

    impl std::io::Write for FailOnce {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls - 1 == self.fail_at {
                return Err(self.kind.into());
            }
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}

/// What every painter's photosensitive tests drive and measure: a strike,
/// the frames it is drawn on, and what reached the screen.
#[cfg(test)]
pub(crate) mod test_flash {
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, SystemTime};

    use pixtuoid_core::state::{MAX_FLOORS, SceneState};
    use pixtuoid_scene::anim::{FULL_TICK_MS, Motion, PHOTOSENSITIVE_PHASE_MIN_MS};
    use pixtuoid_scene::floor::{FloorInputs, FloorMeta, FloorSession, PetInputs};
    use pixtuoid_scene::look::{Look, Place, RenderInputs};
    use pixtuoid_scene::pack::OfficeArt;
    use pixtuoid_scene::screen::Flashes;
    use pixtuoid_scene::sky::{Weather, WeatherPolicy};

    /// How long a probe looks for a flash before the test gives up on it.
    const MINUTE_MS: u64 = 60_000;

    /// The bundled pack, parsed once per test process.
    pub(crate) fn pack() -> &'static OfficeArt {
        pack_static()
    }

    /// [`pack`], shared: a floor's raster draws the pack its frames step.
    pub(crate) fn pack_arc() -> Arc<OfficeArt> {
        Arc::clone(pack_static())
    }

    fn pack_static() -> &'static Arc<OfficeArt> {
        static PACK: OnceLock<Arc<OfficeArt>> = OnceLock::new();
        PACK.get_or_init(|| {
            Arc::new(pixtuoid_scene::pack::load_bundled_pack().expect("bundled pack"))
        })
    }

    /// What an empty floor's frame flashes at `at`, rendered on `session` as a
    /// painter renders it.
    fn rendered_flash(session: &mut FloorSession, floor: FloorMeta, at: SystemTime) -> Flashes {
        let scene = SceneState::new([8; MAX_FLOORS]);
        session.render(
            Look::Classic,
            RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    pack: pack(),
                    now: at,
                    floor,
                    pets: PetInputs::default(),
                },
                theme: pixtuoid_scene::theme::ALL_THEMES[0],
                size: pixtuoid_scene::layout::min_layout_size(),
                place: Place::default(),
                debug_walkable: false,
                dim: pixtuoid_scene::look::Dim::NONE,
            },
        );
        session
            .picture()
            .map(|picture| picture.flashes)
            .unwrap_or_default()
    }

    /// A forced storm's first strike after local noon, on a Full floor.
    pub(crate) struct Strike {
        pub(crate) weather: WeatherPolicy,
        pub(crate) start: SystemTime,
        pub(crate) end: SystemTime,
        /// Where its phase changes, its start and its return to dark included.
        pub(crate) changes: Vec<SystemTime>,
    }

    pub(crate) fn storm_strike() -> Strike {
        let ms = Duration::from_millis;
        let weather = WeatherPolicy::Forced(Weather::Storm);
        let floor = FloorMeta::for_floor(0, 1)
            .with_weather(weather)
            .with_motion(Motion::Full);
        // A fresh floor each instant: a room never left to dim, so only the sky flashes.
        let phase = |at| rendered_flash(&mut FloorSession::new(pack_arc()), floor, at);
        let noon = pixtuoid_scene::localclock::at_hour_min(12, 0);
        let start = (0..MINUTE_MS)
            .step_by(FULL_TICK_MS as usize)
            .map(|n| noon + ms(n))
            .find(|&at| phase(at) != Flashes::default())
            .expect("a storm strikes within a minute");
        let mut changes = vec![start];
        let mut was = phase(start);
        let end = (1..MINUTE_MS)
            .map(|n| start + ms(n))
            .find(|&at| {
                let now = phase(at);
                if now != was {
                    changes.push(at);
                    was = now;
                }
                now == Flashes::default()
            })
            .expect("a strike ends within a minute");
        Strike {
            weather,
            start,
            end,
            changes,
        }
    }

    /// How late each painter's slow twin lands its frame: one value, so the
    /// half-block, cutaway and floating twins test one model.
    pub(crate) const SLOW: Duration = Duration::from_millis(60);

    /// Four frames over `strike`: one long in the dark; one late in its first
    /// phase; one at its second phase's start, inside the floor of the last,
    /// so held; and the first the floor after the last, so shown.
    pub(crate) fn held_frames(strike: &Strike) -> [SystemTime; 4] {
        let floor = Duration::from_millis(PHOTOSENSITIVE_PHASE_MIN_MS);
        let second = strike.changes[1];
        let late = second - floor / 4;
        [strike.start - 2 * floor, late, second, late + floor]
    }

    /// A starved neon's first burst of catches on an empty, clear, Full floor:
    /// a painter's two setup frames empty the room and land the tube, and the
    /// burst follows a second later.
    pub(crate) struct Stutter {
        pub(crate) weather: WeatherPolicy,
        pub(crate) setup: [SystemTime; 2],
        pub(crate) start: SystemTime,
        pub(crate) end: SystemTime,
        /// Where its catches and darks begin, the last return to dark included.
        pub(crate) changes: Vec<SystemTime>,
    }

    pub(crate) fn starved_stutter() -> Stutter {
        use pixtuoid_scene::floor::VacancyDim;
        const BURST_MS: u64 = 1_000;
        let ms = Duration::from_millis;
        let weather = WeatherPolicy::Forced(Weather::Clear);
        let floor = FloorMeta::for_floor(0, 1)
            .with_weather(weather)
            .with_motion(Motion::Full);
        let noon = pixtuoid_scene::localclock::at_hour_min(12, 0);
        let setup = [noon, noon + ms(VacancyDim::EMPTY_DEBOUNCE_MS)];
        let mut session = FloorSession::new(pack_arc());
        let mut phase = |at| rendered_flash(&mut session, floor, at);
        phase(setup[0]);
        let mut was = phase(setup[1]);
        let mut last = setup[1];
        let mut changed: Vec<SystemTime> = Vec::new();
        for at in (1..=MINUTE_MS / FULL_TICK_MS).map(|n| setup[1] + ms(n * FULL_TICK_MS)) {
            let now = phase(at);
            if now != was {
                // A burst opens after a second of dark, so none is caught mid-way.
                let opens = at.duration_since(last).expect("in order") >= ms(BURST_MS);
                if !changed.is_empty() || opens {
                    changed.push(at);
                }
                (was, last) = (now, at);
            }
            if changed
                .first()
                .is_some_and(|&first| at > first + ms(BURST_MS))
            {
                break;
            }
        }
        let start = *changed
            .first()
            .expect("a starved tube catches within a minute");
        changed.retain(|&at| at <= start + ms(BURST_MS));
        Stutter {
            weather,
            setup,
            start,
            end: *changed.last().expect("a burst"),
            changes: changed,
        }
    }

    /// The neon sign's tube on its west side and the wall it lights, in
    /// logical units: what a starved tube's catch changes and nothing else
    /// in an empty office does.
    pub(crate) fn neon_tube(x: u16, y: u16) -> bool {
        use pixtuoid_scene::layout::{NEON_PANEL_INNER_H, NEON_PANEL_INNER_X, NEON_PANEL_INNER_Y};
        x < NEON_PANEL_INNER_X && y < NEON_PANEL_INNER_Y + NEON_PANEL_INNER_H
    }

    /// The frame periods and first-frame offsets a painter whose frames come
    /// each `tick` is driven at: each tick or half again as long, landing on
    /// a phase's start or half a frame after.
    pub(crate) fn frame_grid(tick: Duration) -> impl Iterator<Item = (Duration, Duration)> {
        [tick, tick * 3 / 2]
            .into_iter()
            .flat_map(|frame| [(frame, Duration::ZERO), (frame, frame / 2)])
    }

    /// The instants a painter's frames land after `from`, each `frame` on,
    /// through the first at or past `until`. Each is `from` plus a multiple,
    /// never a running sum, so a clock coarser than `frame` (Windows'
    /// `SystemTime` counts 100 ns) truncates it once rather than drifting.
    pub(crate) fn frames_after(
        from: SystemTime,
        frame: Duration,
        until: SystemTime,
    ) -> impl Iterator<Item = SystemTime> {
        let span = until.duration_since(from).unwrap_or_default();
        let n = u32::try_from(span.as_nanos().div_ceil(frame.as_nanos())).unwrap_or(u32::MAX);
        (1..=n).map(move |k| from + frame * k)
    }

    /// How far before a phase a painter's frames start, on its frame grid:
    /// the dark before has shown the floor, as the gap between strikes sees
    /// to.
    pub(crate) fn lead(frame: Duration) -> Duration {
        let floor = Duration::from_millis(PHOTOSENSITIVE_PHASE_MIN_MS);
        frame * (floor.as_nanos().div_ceil(frame.as_nanos()) as u32 + 1)
    }

    /// `changed`, the instants a picture's flash changed on screen, are
    /// `changes` of them, and each phase between showed at least the floor.
    pub(crate) fn assert_each_phase_holds_the_floor(
        changed: &[SystemTime],
        changes: usize,
        at: &str,
    ) {
        assert_eq!(changed.len(), changes, "{at}: {changed:?}");
        for pair in changed.windows(2) {
            let shown = pair[1].duration_since(pair[0]).expect("in order");
            assert!(
                shown >= Duration::from_millis(PHOTOSENSITIVE_PHASE_MIN_MS),
                "{at}: a phase shown {shown:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_io::FailOnce;

    #[test]
    fn a_broken_pipe_closes_the_stream_and_drops_later_writes() {
        use std::io::Write;
        let mut out = CliOut::new(FailOnce::new(1, std::io::ErrorKind::BrokenPipe));
        writeln!(out, "first").expect("written");
        assert!(!out.closed());
        writeln!(out, "second").expect("a broken pipe is not an error");
        assert!(out.closed());
        writeln!(out, "third").expect("dropped");
        assert_eq!(out.inner.written, b"first\n");
    }

    #[test]
    fn any_other_write_error_still_fails() {
        use std::io::Write;
        let mut out = CliOut::new(FailOnce::new(0, std::io::ErrorKind::StorageFull));
        assert!(writeln!(out, "x").is_err());
        assert!(!out.closed());
    }

    #[test]
    fn display_path_strips_control_chars_from_a_hostile_path() {
        let hostile = std::path::Path::new("/x/\x1b]0;pwned\x07\x1b[31mhook");
        let got = display_path(hostile);
        assert!(!got.chars().any(|c| c.is_control()), "{got:?}");
        assert!(got.contains("hook") && got.contains("/x/"), "{got:?}");
    }

    #[test]
    fn a_fatal_error_keeps_its_chain_and_loses_its_controls() {
        let e = anyhow::anyhow!("bad key \u{1b}[31m\u{202e}")
            .context("failed to load sprite pack from \"/p\"");
        let text = fatal_error_text(&e);
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{202e}'),
            "{text:?}"
        );
        assert!(
            text.starts_with("Error: failed to load sprite pack"),
            "{text:?}"
        );
        assert!(text.contains("\nCaused by:\n"), "{text:?}");
    }

    #[test]
    fn strips_c0_and_c1_controls() {
        assert_eq!(strip_control_chars("a\x1b[31mb\x07c"), "a[31mbc");
        assert_eq!(strip_control_chars("x\u{0085}y"), "xy"); // C1 NEL
    }

    #[test]
    fn strips_trojan_source_bidi_controls() {
        assert_eq!(strip_control_chars("safe\u{202E}gpj.exe"), "safegpj.exe");
        for c in [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            assert_eq!(
                strip_control_chars(&format!("a{c}b")),
                "ab",
                "U+{:04X} not stripped",
                c as u32
            );
        }
    }

    #[test]
    fn keeps_ordinary_text_and_non_bidi_unicode() {
        let s = "hello wörld café 日本語 🦞";
        assert_eq!(strip_control_chars(s), s);
    }

    /// Round-trip one probe name through `pixtuoid-core`'s copy of the predicate,
    /// via `decode_hook_payload`'s unsupported-event error.
    fn core_display_safe(name: &str) -> String {
        let v = serde_json::json!({"hook_event_name": name, "session_id": "s"});
        let e = pixtuoid_core::source::decoder::decode_hook_payload(v)
            .expect_err("an unregistered hook_event_name must be refused");
        let pixtuoid_core::source::decoder::DecodeError::Unsupported { event, .. } = e else {
            panic!("an unregistered hook_event_name must be Unsupported, got {e:?}");
        };
        event
    }

    #[test]
    fn the_bidi_table_matches_pixtuoid_cores_display_safe() {
        // Two per-crate copies of one security table (core's is `pub(crate)`), so
        // pin them BEHAVIOURALLY: sweep every codepoint either side could
        // plausibly gain or lose — the Cc block and its boundaries, DEL/C1, and
        // the Cf neighbourhoods on both sides of each bidi range.
        let candidates = (0x00..=0x20u32)
            .chain(0x7E..=0xA1)
            .chain(0x061A..=0x061E)
            .chain(0x200B..=0x2010)
            .chain(0x2028..=0x2030)
            .chain(0x2065..=0x206F);
        for cp in candidates {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let probe = format!("PixtuoidParity{c}Probe");
            assert_eq!(
                strip_control_chars(&probe),
                core_display_safe(&probe),
                "U+{cp:04X}: the binary's strip_control_chars and pixtuoid-core's \
                 display_safe disagree — the two Cf/Cc tables have drifted apart",
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_outlives_its_timeout_is_killed_and_reaped() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let dir = tempfile::TempDir::new().expect("tempdir");
        let pid_file = dir.path().join("pid");
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("echo $$ > \"$0\"; exec sleep 30")
            .arg(&pid_file)
            .stdout(Stdio::piped());
        let started = Instant::now();
        assert!(output_within(&mut cmd, Duration::from_millis(200)).is_none());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .expect("the child wrote its pid")
            .trim()
            .parse()
            .expect("a pid");
        // ESRCH only once the child is reaped: a zombie still answers kill 0.
        // SAFETY: signal 0 sends nothing; it only checks the pid exists.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "the child {pid} was left running or unreaped");
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_exits_in_time_yields_its_output_and_status() {
        use std::process::{Command, Stdio};
        use std::time::Duration;
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo hi; exit 3"]).stdout(Stdio::piped());
        let out = output_within(&mut cmd, Duration::from_secs(5)).expect("exits in time");
        assert_eq!(out.stdout, b"hi\n");
        assert_eq!(out.status.code(), Some(3));
    }
}
