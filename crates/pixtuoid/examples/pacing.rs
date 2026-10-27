//! Frame pacing: drives the real TUI painter (`TuiRenderer` over ratatui's
//! crossterm encoder, or the cutaway's tile sink per image protocol) through a
//! fixed scenario and reports what a viewer feels: how long each frame takes,
//! split into compose / rasterize / encode / write, how many overrun the paint
//! interval, how even the interval between frames is, and how many bytes each
//! frame writes. Advisory, local: `just bench-pacing`.
//!
//! The scene advances a VIRTUAL clock one paint interval per frame, so every
//! case paints the same frames, and each frame waits for its tick on the wall
//! clock as the TUI's does (`Pace::Paced`): the CPU idles between frames
//! and each wakes on a cooled core, which a frame run back to back never
//! sees. Each case's `hot p99` runs the same frames back to back
//! (`Pace::Hot`). The interval columns are computed from the measured
//! render times under each loop model, not observed; the one `real-clock`
//! case runs the production loop's shape on the wall clock to check that
//! model once.
//!
//! `pacing next-storm` and `pacing dusk` print the Unix second the clock's
//! next transition into a storm and today's nightfall start, and `pacing
//! terminal [4|16]` the cells and cell pixels that give the owner's office at
//! that scale: what `just pace-check` runs on.
//!
//! `pacing hitch <frames.jsonl>` instead logs every frame of each terminal ×
//! clock run with what made it slow: each stage's time, the Dirty kind, tiles
//! and bytes sent, the canvas epoch field that changed, a cloud-raster miss,
//! the sky's weather and strike. Its runs: the owner's 16x kitty on the real
//! clock (`HITCH_REAL_SECS`, default 600), then per terminal a clear, an
//! overcast and a stormy noon, dusk a second a frame, the first clock-weather
//! transition, and a storm whose every frame paints and sends whole, with and
//! without lofi's music rendering beside it. `HITCH_ONLY=a|b` keeps the runs
//! whose terminal or name holds every part;
//! every run is paced, but back to back under `HITCH_HOT`.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use pixtuoid::dev::{Protocol, renderer};
use pixtuoid_core::state::{ActivityState, SceneState, ToolKind, ToolUse};
use pixtuoid_core::{AgentId, AgentSlot, GlobalDeskIndex};
use pixtuoid_scene::anim::{Motion, PAINT_FPS};
use pixtuoid_scene::look::spans;
use pixtuoid_scene::pet::{Pet, PetKind};
use pixtuoid_scene::sky::{Weather, WeatherPolicy, first_strike_after};
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

/// The scenario's length.
const SCENARIO: Duration = Duration::from_secs(10);
/// Agents in the office: more than one floor seats, so a slide has a floor.
const AGENTS: usize = 12;
/// Desks per floor.
const FLOOR_DESKS: usize = 8;
/// When the strike lands, from the scenario's start.
const STRIKE_AT: Duration = Duration::from_secs(2);
/// When the view slides to the second floor.
const SLIDE_AT: Duration = Duration::from_secs(4);
/// When the terminal shrinks, and by how many cells each way.
const RESIZE_AT: Duration = Duration::from_secs(7);
const RESIZE_BY: (u16, u16) = (12, 4);
/// The terminal every case starts in.
const TERMINAL: (u16, u16) = (160, 50);
/// The half-block's cell, and the cutaway's: the cutaway always renders at the
/// pack's densest art, so the cell picks its upscale (none, then 2x).
const HALF_BLOCK_CELL: (u16, u16) = (8, 16);
const CUTAWAY_CELLS: [(u16, u16); 2] = [(4, 8), (8, 16)];

/// Span time per name, summed since the last take.
#[derive(Clone, Default)]
struct SpanClock(Arc<Mutex<HashMap<&'static str, Duration>>>);

impl SpanClock {
    fn take(&self) -> HashMap<&'static str, Duration> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// When the span was entered, kept in its extensions.
struct Entered(Instant);

impl<S> tracing_subscriber::Layer<S> for SpanClock
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &tracing::span::Id, ctx: LayerContext<'_, S>) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().replace(Entered(Instant::now()));
        }
    }

    fn on_exit(&self, id: &tracing::span::Id, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let Some(Entered(at)) = span.extensions_mut().remove::<Entered>() else {
            return;
        };
        *self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(span.name())
            .or_default() += at.elapsed();
    }
}

/// One frame's measurements.
#[derive(Clone, Copy, Default)]
struct Frame {
    total: Duration,
    compose: Duration,
    rasterize: Duration,
    encode: Duration,
    write: Duration,
    bytes: u64,
}

struct Case {
    name: String,
    protocol: Protocol,
    cell: (u16, u16),
    motion: Motion,
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for motion in [Motion::Full, Motion::Calm] {
        out.push(Case {
            name: format!("half-block classic {motion:?}"),
            protocol: Protocol::HalfBlock,
            cell: HALF_BLOCK_CELL,
            motion,
        });
        for protocol in Protocol::ALL
            .into_iter()
            .filter(|&p| p != Protocol::HalfBlock)
        {
            for cell in CUTAWAY_CELLS {
                out.push(Case {
                    name: format!(
                        "{} cutaway cell {}x{} {motion:?}",
                        protocol.name(),
                        cell.0,
                        cell.1
                    ),
                    protocol,
                    cell,
                    motion,
                });
            }
        }
    }
    out
}

/// The office at `start`: agents walking in through the scenario, some long
/// at their desks (and so wandering), the rest working.
fn office(start: SystemTime) -> SceneState {
    let mut scene = SceneState::uniform(FLOOR_DESKS);
    let long_ago = start - Duration::from_secs(600);
    for i in 0..AGENTS {
        let id = AgentId::from_transcript_path(&format!("/pacing/a{i}.jsonl"));
        let (state, started) = match i % 3 {
            0 => (
                ActivityState::Running {
                    item: Some(ToolUse {
                        tool_use_id: Some(format!("tu_{i}").into()),
                        detail: Some("Edit: src/lib.rs".into()),
                        kind: ToolKind::Edit,
                    }),
                },
                long_ago,
            ),
            1 => (ActivityState::Idle, long_ago),
            // Arriving one by one across the scenario: entry walks.
            _ => (
                ActivityState::Idle,
                start + SCENARIO.mul_f64(i as f64 / AGENTS as f64),
            ),
        };
        scene.agents.insert(
            id,
            AgentSlot {
                agent_id: id,
                source: Arc::from("claude-code"),
                session_id: Arc::from(format!("pacing-{i:04x}").as_str()),
                cwd: Arc::from(Path::new("/pacing")),
                label: format!("a{i}").into(),
                state,
                state_started_at: started,
                created_at: started,
                last_event_at: started,
                exiting_at: None,
                desk_index: GlobalDeskIndex(i),
                floor_idx: scene.floor_of(GlobalDeskIndex(i)),
                tool_call_count: 0,
                running_ms: 0,
                unknown_cwd: false,
                parent_id: None,
                model: None,
                pid: None,
                effort: None,
                tokens_used: 0,
                last_usage: None,
            },
        );
    }
    scene
}

/// The scenario's start for `motion`: a strike lands [`STRIKE_AT`] in.
fn start_for(motion: Motion) -> Result<SystemTime> {
    let base = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let strike = first_strike_after(base + STRIKE_AT, motion).context("a moving tier strikes")?;
    Ok(strike - STRIKE_AT)
}

/// Run one case on the virtual clock: every frame's measurements.
fn run(
    case: &Case,
    clock: &SpanClock,
    pack: &Arc<pixtuoid_scene::pack::OfficeArt>,
    pace: Pace,
) -> Result<Vec<Frame>> {
    let tick = Duration::from_secs(1) / PAINT_FPS;
    let pets = vec![Pet::defaulted(PetKind::Cat), Pet::defaulted(PetKind::Dog)];
    let (mut r, wire) = renderer(
        case.protocol,
        TERMINAL.0,
        TERMINAL.1,
        case.cell,
        pets,
        Arc::clone(pack),
    )?;
    r.set_weather(WeatherPolicy::Forced(Weather::Storm));
    r.set_motion(case.motion);
    let start = start_for(case.motion)?;
    let scene = office(start);
    // By frame index: `tick` is a truncated third of a second, so no multiple
    // of it lands exactly on a whole second.
    let frame_at = |d: Duration| d.as_nanos() / tick.as_nanos();
    let frames = frame_at(SCENARIO);
    let (mut slid, mut resized) = (false, false);
    let mut out = Vec::new();
    let pacer = Pacer::new(pace, tick);
    for n in 0..frames {
        let now = start + tick * u32::try_from(n)?;
        pacer.wait();
        if n == frame_at(SLIDE_AT) {
            r.navigate_floor(1, now);
            slid = r.transition().is_some();
        }
        if n == frame_at(RESIZE_AT) {
            r.terminal
                .backend_mut()
                .resize(TERMINAL.0 - RESIZE_BY.0, TERMINAL.1 - RESIZE_BY.1);
            resized = true;
        }
        wire.take();
        clock.take();
        let begun = Instant::now();
        r.render(&scene, now)?;
        let total = begun.elapsed();
        let (bytes, write_ns) = wire.take();
        let spans = clock.take();
        let compose = spans.get(spans::COMPOSE).copied().unwrap_or_default();
        let rasterize = spans.get(spans::RASTERIZE).copied().unwrap_or_default();
        let write = Duration::from_nanos(write_ns);
        out.push(Frame {
            total,
            compose,
            rasterize,
            encode: total.saturating_sub(compose + rasterize + write),
            write,
            bytes,
        });
    }
    anyhow::ensure!(
        slid && resized,
        "{}: the slide or the resize never ran",
        case.name
    );
    Ok(out)
}

/// The production loop's shape on the wall clock: render, then wait a paint
/// interval for input that never comes. Each frame's interval, observed.
fn real_clock(
    pack: &Arc<pixtuoid_scene::pack::OfficeArt>,
) -> Result<(Vec<Duration>, Vec<Duration>, &'static str)> {
    let tick = Duration::from_secs(1) / PAINT_FPS;
    let (mut r, _wire) = renderer(
        Protocol::HalfBlock,
        TERMINAL.0,
        TERMINAL.1,
        HALF_BLOCK_CELL,
        vec![Pet::defaulted(PetKind::Cat)],
        Arc::clone(pack),
    )?;
    r.set_weather(WeatherPolicy::Forced(Weather::Storm));
    r.set_motion(Motion::Full);
    let start = start_for(Motion::Full)?;
    let scene = office(start);
    let clock = Instant::now();
    let (mut renders, mut intervals) = (Vec::new(), Vec::new());
    let mut last = None;
    // Probed once: a failed poll per frame would add its syscall to every
    // observed interval.
    let polls = crossterm::event::poll(Duration::ZERO).is_ok();
    while clock.elapsed() < SCENARIO {
        let begun = Instant::now();
        if let Some(prev) = last.replace(begun) {
            intervals.push(begun - prev);
        }
        r.render(&scene, start + clock.elapsed())?;
        renders.push(begun.elapsed());
        // The loop's own wait is crossterm's input poll; a CI runner has no
        // terminal to poll, so it sleeps.
        if !polls {
            std::thread::sleep(tick);
        } else if crossterm::event::poll(tick)? {
            crossterm::event::read()?;
        }
    }
    Ok((renders, intervals, if polls { "poll" } else { "sleep" }))
}

/// How a run spaces its frames on the wall clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pace {
    /// Each frame on its own tick, as the TUI's frame interval runs them
    /// (`tokio::time::interval`, `MissedTickBehavior::Skip`).
    Paced,
    /// Back to back, which keeps the CPU hot as no running TUI does.
    Hot,
}

/// Holds each frame of a run to its tick under a [`Pace`].
struct Pacer {
    pace: Pace,
    tick: Duration,
    origin: Instant,
}

impl Pacer {
    fn new(pace: Pace, tick: Duration) -> Self {
        Self {
            pace,
            tick,
            origin: Instant::now(),
        }
    }

    /// Wait for the next tick not yet past, as `Skip` does after an overrun.
    fn wait(&self) {
        if self.pace == Pace::Hot {
            return;
        }
        let since = self.origin.elapsed();
        let ticks = since.as_nanos().div_ceil(self.tick.as_nanos());
        let at = self
            .tick
            .saturating_mul(u32::try_from(ticks).unwrap_or(u32::MAX));
        std::thread::sleep(at.saturating_sub(since));
    }
}

/// The `p`th percentile of `xs`, nearest rank.
fn pct(xs: &[Duration], p: f64) -> Duration {
    let mut v = xs.to_vec();
    v.sort();
    let i = ((p / 100.0) * v.len() as f64).ceil() as usize;
    v.get(i.saturating_sub(1)).copied().unwrap_or_default()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// Each frame's interval under the production loop today: render, then a full
/// paint interval of polling.
fn sleep_loop(renders: &[Duration], tick: Duration) -> Vec<Duration> {
    renders.iter().map(|&r| r + tick).collect()
}

/// Each frame's interval under a deadline-paced loop: the next frame starts at
/// the previous deadline plus a paint interval, or as soon as the render ends
/// when it overran, re-anchoring there rather than bursting to catch up.
fn deadline_loop(renders: &[Duration], tick: Duration) -> Vec<Duration> {
    renders.iter().map(|&r| r.max(tick)).collect()
}

fn stats(xs: &[Duration]) -> serde_json::Value {
    serde_json::json!({
        "p50_ms": ms(pct(xs, 50.0)),
        "p95_ms": ms(pct(xs, 95.0)),
        "p99_ms": ms(pct(xs, 99.0)),
        "max_ms": ms(xs.iter().copied().max().unwrap_or_default()),
    })
}

/// What the hitch probe saw in one frame: each span's time and count, and
/// each event's last fields by message.
#[derive(Clone, Default)]
struct Probe(Arc<Mutex<Seen>>);

#[derive(Default)]
struct Seen {
    spans: HashMap<&'static str, (Duration, u32)>,
    events: HashMap<String, serde_json::Map<String, serde_json::Value>>,
}

impl Probe {
    fn take(&self) -> Seen {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

#[derive(Default)]
struct Fields {
    message: String,
    map: serde_json::Map<String, serde_json::Value>,
}

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let v = format!("{value:?}");
        if field.name() == "message" {
            self.message = v;
        } else {
            self.map.insert(field.name().into(), v.into());
        }
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.map.insert(field.name().into(), value.into());
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.map.insert(field.name().into(), value.into());
    }
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.map.insert(field.name().into(), value.into());
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.map.insert(field.name().into(), value.into());
    }
}

impl<S> tracing_subscriber::Layer<S> for Probe
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &tracing::span::Id, ctx: LayerContext<'_, S>) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().replace(Entered(Instant::now()));
        }
    }

    fn on_exit(&self, id: &tracing::span::Id, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let Some(Entered(at)) = span.extensions_mut().remove::<Entered>() else {
            return;
        };
        let mut seen = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let e = seen.spans.entry(span.name()).or_default();
        e.0 += at.elapsed();
        e.1 += 1;
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: LayerContext<'_, S>) {
        let mut f = Fields::default();
        event.record(&mut f);
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .events
            .insert(f.message, f.map);
    }
}

/// The owner's terminal: kitty in Ghostty, a 17x41 cell, a 214x125 office.
const OWNER_CELL: (u16, u16) = (17, 41);
const OWNER_OFFICE: (u16, u16) = (214, 125);
/// A cell whose natural scale is the art's own 4x, upscaled not at all.
const FOUR_X_CELL: (u16, u16) = (4, 8);

/// The smallest terminal whose office on `cell` holds the owner's: exactly
/// it at 16x, a row taller at 4x, whose cell halves the rows.
fn owner_terminal(pack: &pixtuoid_scene::pack::OfficeArt, cell: (u16, u16)) -> Result<(u16, u16)> {
    (1..=1000u16)
        .flat_map(|c| (1..=400u16).map(move |r| (c, r)))
        .find(|&(c, r)| {
            pixtuoid::dev::cutaway_office(c, r, cell, pack)
                .is_some_and(|(w, h, _)| w >= OWNER_OFFICE.0 && h >= OWNER_OFFICE.1)
        })
        .context("no terminal fits the owner's office")
}

struct HitchRun {
    name: &'static str,
    weather: WeatherPolicy,
    /// `None`: the wall clock, waiting out each paint interval.
    start: Option<SystemTime>,
    step: Duration,
    length: Duration,
    /// Paint and send every frame whole: the worst case.
    whole: bool,
    /// Render lofi's music on a thread throughout, back to back: more than
    /// the audio thread's render-ahead ever asks.
    music: bool,
    /// The ambient tier: the plan puts SIXEL and iTerm2 on Calm.
    motion: Motion,
}

/// A terminal the hitch runs draw on.
struct Term {
    name: &'static str,
    protocol: Protocol,
    cols: u16,
    rows: u16,
    cell: (u16, u16),
}

fn hitch(path: &Path) -> Result<()> {
    let probe = Probe::default();
    let targets = tracing_subscriber::filter::Targets::new()
        .with_target("pixtuoid_scene", tracing::Level::TRACE)
        .with_target("pixtuoid", tracing::Level::TRACE);
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry()
            .with(probe.clone())
            .with(targets),
    )?;
    let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack()?);
    let (cols, rows) = owner_terminal(&pack, OWNER_CELL)?;
    anyhow::ensure!(
        pixtuoid::dev::cutaway_office(cols, rows, OWNER_CELL, &pack)
            .is_some_and(|(w, h, _)| (w, h) == OWNER_OFFICE),
        "the owner's terminal no longer gives the owner's office exactly: its runs stop being comparable"
    );
    let tick = Duration::from_secs(1) / PAINT_FPS;
    let real_secs = pixtuoid_core::platform::text_env("HITCH_REAL_SECS")
        .map(|s| s.parse().with_context(|| format!("HITCH_REAL_SECS={s:?}")))
        .transpose()?
        .unwrap_or(600);
    let only = pixtuoid_core::platform::text_env("HITCH_ONLY");
    let noon = pixtuoid_scene::localclock::at_hour(12);
    let dusk = pixtuoid_scene::localclock::at_hour_min(19, 55);
    let sweeps = [
        HitchRun {
            name: "noon clear",
            weather: WeatherPolicy::Forced(Weather::Clear),
            start: Some(noon),
            step: tick,
            length: Duration::from_secs(60),
            whole: false,
            music: false,
            motion: Motion::Full,
        },
        HitchRun {
            name: "noon overcast",
            weather: WeatherPolicy::Forced(Weather::Overcast),
            start: Some(noon),
            step: tick,
            length: Duration::from_secs(60),
            whole: false,
            music: false,
            motion: Motion::Full,
        },
        HitchRun {
            name: "noon storm",
            weather: WeatherPolicy::Forced(Weather::Storm),
            start: Some(noon),
            step: tick,
            length: Duration::from_secs(120),
            whole: false,
            music: false,
            motion: Motion::Full,
        },
        // Every beat at night changes the windows' sky and a light far apart:
        // the repaint that spans them is the SIXEL and iTerm2 rows' cost.
        HitchRun {
            name: "night on Calm, 33 ms",
            weather: WeatherPolicy::Forced(Weather::Clear),
            start: Some(pixtuoid_scene::localclock::at_hour_min(21, 30)),
            step: tick,
            length: Duration::from_secs(60),
            whole: false,
            music: false,
            motion: Motion::Calm,
        },
        HitchRun {
            name: "dusk ff 1s/frame",
            weather: WeatherPolicy::Forced(Weather::Clear),
            start: Some(dusk),
            step: Duration::from_secs(1),
            length: Duration::from_secs(3600),
            whole: false,
            music: false,
            motion: Motion::Full,
        },
        HitchRun {
            name: "a weather transition, 33 ms",
            weather: WeatherPolicy::Clock,
            start: Some(
                pixtuoid_scene::sky::first_transition_after(noon)
                    .context("the clock's weather never changes")?,
            ),
            step: tick,
            length: Duration::from_secs(120),
            whole: false,
            music: false,
            motion: Motion::Full,
        },
        HitchRun {
            name: "stress: storm, every frame whole",
            weather: WeatherPolicy::Forced(Weather::Storm),
            start: Some(noon),
            step: tick,
            length: Duration::from_secs(30),
            whole: true,
            music: false,
            motion: Motion::Full,
        },
        HitchRun {
            name: "stress: storm, every frame whole, music rendering",
            weather: WeatherPolicy::Forced(Weather::Storm),
            start: Some(noon),
            step: tick,
            length: Duration::from_secs(30),
            whole: true,
            music: true,
            motion: Motion::Full,
        },
    ];
    let real = HitchRun {
        name: "real clock 10 min, clock weather",
        weather: WeatherPolicy::Clock,
        start: None,
        step: tick,
        length: Duration::from_secs(real_secs),
        whole: false,
        music: false,
        motion: Motion::Full,
    };
    let owner = |protocol, name| Term {
        name,
        protocol,
        cols,
        rows,
        cell: OWNER_CELL,
    };
    let mid = |protocol, name| Term {
        name,
        protocol,
        cols: 160,
        rows: 45,
        cell: (9, 18),
    };
    let terms = [
        owner(Protocol::Kitty, "owner kitty 17x41"),
        owner(Protocol::Sixel, "owner sixel 17x41"),
        owner(Protocol::Iterm2, "owner iterm2 17x41"),
        owner(Protocol::HalfBlock, "owner half-block"),
        mid(Protocol::Kitty, "mid kitty 9x18"),
        mid(Protocol::Sixel, "mid sixel 9x18"),
        mid(Protocol::Iterm2, "mid iterm2 9x18"),
        mid(Protocol::HalfBlock, "mid half-block"),
    ];
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut stdout = std::io::stdout().lock();
    for t in &terms {
        let office_of = pixtuoid::dev::cutaway_office(t.cols, t.rows, t.cell, &pack);
        let _ = writeln!(
            stdout,
            "{}: {}x{} cells of {:?} px, cutaway office/scale {office_of:?}",
            t.name, t.cols, t.rows, t.cell
        );
    }
    let mut plan: Vec<(&Term, &HitchRun)> = terms
        .iter()
        .flat_map(|t| sweeps.iter().map(move |r| (t, r)))
        .collect();
    plan.insert(0, (&terms[0], &real));
    plan.retain(|(term, run)| {
        only.as_deref().is_none_or(|o| {
            o.split('|')
                .all(|part| term.name.contains(part) || run.name.contains(part))
        })
    });
    anyhow::ensure!(
        !plan.is_empty(),
        "HITCH_ONLY={:?} selects no run",
        only.as_deref().unwrap_or_default()
    );
    for (term, run) in plan {
        let pets = vec![Pet::defaulted(PetKind::Cat), Pet::defaulted(PetKind::Dog)];
        let (mut r, wire) = renderer(
            term.protocol,
            term.cols,
            term.rows,
            term.cell,
            pets,
            Arc::clone(&pack),
        )?;
        r.set_weather(run.weather);
        r.set_motion(run.motion);
        let music_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let music = run.music.then(|| {
            let stop = Arc::clone(&music_stop);
            std::thread::spawn(move || {
                use pixtuoid_scene::audio::{
                    Controls, Frame, MUSIC_AHEAD, Performance, SAMPLE_RATE,
                };
                // As the audio thread does.
                #[cfg(target_os = "macos")]
                pixtuoid::dev::promote_to_user_initiated();
                let mut performance = Performance::new(0, Controls::default());
                // A whole render-ahead at a time.
                let frames = MUSIC_AHEAD.as_secs_f64() * f64::from(SAMPLE_RATE);
                let mut chunk = vec![Frame::default(); frames as usize];
                let mut renders = Vec::new();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let at = Instant::now();
                    performance.render(&mut chunk);
                    renders.push(at.elapsed());
                }
                renders
            })
        });
        let wall = Instant::now();
        let start = run.start.unwrap_or_else(SystemTime::now);
        let scene = office(start);
        pixtuoid::dev::warm(&mut r, &scene, start);
        let mut totals = Vec::new();
        let mut n = 0u64;
        let pace = if pixtuoid_core::platform::text_env("HITCH_HOT").is_some() {
            Pace::Hot
        } else {
            Pace::Paced
        };
        let pacer = Pacer::new(pace, tick);
        loop {
            pacer.wait();
            let offset = match run.start {
                None => wall.elapsed(),
                Some(_) => run.step * u32::try_from(n)?,
            };
            if offset >= run.length {
                break;
            }
            let now = start + offset;
            if run.whole {
                pixtuoid::dev::forget_frame(&mut r);
            }
            wire.take();
            probe.take();
            let begun = Instant::now();
            r.render(&scene, now)?;
            let total = begun.elapsed();
            let (bytes, write_ns) = wire.take();
            let seen = probe.take();
            let span = |name: &str| seen.spans.get(name).copied().unwrap_or_default();
            let ms_of = |name: &str| ms(span(name).0);
            let known = [
                spans::COMPOSE,
                spans::RASTERIZE,
                "tiles.diff",
                "tiles.encode",
            ]
            .iter()
            .map(|n| span(n).0)
            .sum::<Duration>()
                + Duration::from_nanos(write_ns);
            let mut line = serde_json::json!({
                "term": term.name,
                "run": run.name,
                "n": n,
                "offset_s": offset.as_secs_f64(),
                "total_ms": ms(total),
                "sim_ms": ms_of(spans::COMPOSE),
                "list_ms": ms_of("canvas.compose"),
                "paint_ms": ms_of("canvas.paint"),
                "clouds_draw": { "ms": ms_of("clouds.draw"), "n": span("clouds.draw").1 },
                "clouds_ahead_ms": ms_of("clouds.ahead"),
                "diff_ms": ms_of("tiles.diff"),
                "cut_ms": ms_of("tile.cut"),
                "zlib_ms": ms_of("tile.zlib"),
                "base64_ms": ms_of("tile.base64"),
                "tiles_sent": span("tile.encode").1,
                "encode_ms": ms_of("tiles.encode"),
                "encode_cpu_ms": ms_of("tile.encode"),
                "tile_write_ms": ms_of("tile.write"),
                "rasterize_ms": ms_of(spans::RASTERIZE),
                "write_ms": ms(Duration::from_nanos(write_ns)),
                "other_ms": ms(total.saturating_sub(known)),
                "bytes": bytes,
            });
            if let Some(obj) = line.as_object_mut() {
                for (msg, key) in [
                    ("tiles.stage", "stage"),
                    ("canvas.epoch", "epoch"),
                    ("sky.at", "sky"),
                ] {
                    if let Some(f) = seen.events.get(msg) {
                        obj.insert(key.into(), serde_json::Value::Object(f.clone()));
                    }
                }
            }
            serde_json::to_writer(&mut out, &line)?;
            writeln!(out)?;
            totals.push(total);
            n += 1;
        }
        music_stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(renders) = music.and_then(|h| h.join().ok()) {
            let _ = writeln!(
                stdout,
                "  music alongside: {} renders of MUSIC_AHEAD, slowest {:.2} ms",
                renders.len(),
                ms(renders.iter().copied().max().unwrap_or_default())
            );
        }
        let over = totals.iter().filter(|&&t| t > tick).count();
        let _ = writeln!(
            stdout,
            "{:<20} {:<32} frames {:>6}  p50 {:>6.2}  p99 {:>6.2}  max {:>7.2} ms  over {:.1} ms: {}",
            term.name,
            run.name,
            totals.len(),
            ms(pct(&totals, 50.0)),
            ms(pct(&totals, 99.0)),
            ms(totals.iter().copied().max().unwrap_or_default()),
            ms(tick),
            over
        );
    }
    out.flush()?;
    let _ = writeln!(stdout, "frames: {}", path.display());
    Ok(())
}

fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("dusk") {
        let start = pixtuoid_scene::sky::nightfall_on(SystemTime::now())
            .context("no nightfall in today's local time")?;
        let secs = start.duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
        let _ = writeln!(std::io::stdout(), "{secs}");
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("terminal") {
        let pack = pixtuoid_scene::pack::load_bundled_pack()?;
        let cell = match std::env::args().nth(2).as_deref() {
            Some("4") => FOUR_X_CELL,
            _ => OWNER_CELL,
        };
        let (cols, rows) = owner_terminal(&pack, cell)?;
        let _ = writeln!(std::io::stdout(), "{cols} {rows} {} {}", cell.0, cell.1);
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("next-storm") {
        let start = pixtuoid_scene::sky::first_transition_into(SystemTime::now(), Weather::Storm)
            .context("no storm in the clock's weeks ahead")?;
        let secs = start.duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
        let _ = writeln!(std::io::stdout(), "{secs}");
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("hitch") {
        let path = PathBuf::from(
            std::env::args()
                .nth(2)
                .context("usage: pacing hitch <frames.jsonl>")?,
        );
        return hitch(&path);
    }
    let clock = SpanClock::default();
    let subscriber = tracing_subscriber::registry().with(clock.clone()).with(
        tracing_subscriber::filter::Targets::new()
            .with_target("pixtuoid_scene::look", tracing::Level::TRACE),
    );
    tracing::subscriber::set_global_default(subscriber)?;
    let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack()?);
    let tick = Duration::from_secs(1) / PAINT_FPS;
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(
        stdout,
        "paint interval {:.1} ms (1 s / PAINT_FPS); frames paced on its ticks, hot p99 back to back; intervals and jitter are COMPUTED from render times\n",
        ms(tick)
    );
    let _ = writeln!(
        stdout,
        "{:<38} {:>7} {:>7} {:>7} {:>7} {:>7} | {:>6} {:>6} {:>6} {:>6} | {:>5} | {:>13} {:>13} | {:>9} {:>9}",
        "case",
        "p50",
        "p95",
        "p99",
        "max",
        "hot p99",
        "comp",
        "rast",
        "enc",
        "write",
        "over%",
        "now p95/jit",
        "deadl p95/jit",
        "B p50",
        "B p95"
    );
    let mut report = Vec::new();
    for case in cases() {
        let frames = run(&case, &clock, &pack, Pace::Paced)?;
        let hot: Vec<Duration> = run(&case, &clock, &pack, Pace::Hot)?
            .iter()
            .map(|f| f.total)
            .collect();
        let col = |f: fn(&Frame) -> Duration| frames.iter().map(f).collect::<Vec<_>>();
        let total = col(|f| f.total);
        let over = total.iter().filter(|&&t| t > tick).count() as f64 * 100.0 / total.len() as f64;
        let now_loop = sleep_loop(&total, tick);
        let deadline = deadline_loop(&total, tick);
        let jitter = |xs: &[Duration]| pct(xs, 95.0).saturating_sub(pct(xs, 50.0));
        let mut bytes: Vec<u64> = frames.iter().map(|f| f.bytes).collect();
        bytes.sort_unstable();
        let byte_pct = |p: f64| {
            let i = ((p / 100.0) * bytes.len() as f64).ceil() as usize;
            bytes.get(i.saturating_sub(1)).copied().unwrap_or_default()
        };
        let _ = writeln!(
            stdout,
            "{:<38} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2} | {:>6.2} {:>6.2} {:>6.2} {:>6.2} | {:>5.1} | {:>6.1}/{:>6.2} {:>6.1}/{:>6.2} | {:>9} {:>9}",
            case.name,
            ms(pct(&total, 50.0)),
            ms(pct(&total, 95.0)),
            ms(pct(&total, 99.0)),
            ms(total.iter().copied().max().unwrap_or_default()),
            ms(pct(&hot, 99.0)),
            ms(pct(&col(|f| f.compose), 50.0)),
            ms(pct(&col(|f| f.rasterize), 50.0)),
            ms(pct(&col(|f| f.encode), 50.0)),
            ms(pct(&col(|f| f.write), 50.0)),
            over,
            ms(pct(&now_loop, 95.0)),
            ms(jitter(&now_loop)),
            ms(pct(&deadline, 95.0)),
            ms(jitter(&deadline)),
            byte_pct(50.0),
            byte_pct(95.0),
        );
        report.push(serde_json::json!({
            "case": case.name,
            "frames": frames.len(),
            "frame": stats(&total),
            "frame_hot": stats(&hot),
            "compose": stats(&col(|f| f.compose)),
            "rasterize": stats(&col(|f| f.rasterize)),
            "encode": stats(&col(|f| f.encode)),
            "write": stats(&col(|f| f.write)),
            "over_budget_pct": over,
            "interval_render_then_poll": stats(&now_loop),
            "jitter_render_then_poll_ms": ms(jitter(&now_loop)),
            "interval_deadline": stats(&deadline),
            "jitter_deadline_ms": ms(jitter(&deadline)),
            "bytes_p50": byte_pct(50.0),
            "bytes_p95": byte_pct(95.0),
        }));
    }
    let (renders, observed, waited) = real_clock(&pack)?;
    let modeled = sleep_loop(&renders, tick);
    let _ = writeln!(
        stdout,
        "\nreal-clock half-block Full (waits on {waited}): interval OBSERVED p50/p95/p99 {:.2}/{:.2}/{:.2} ms vs COMPUTED {:.2}/{:.2}/{:.2} ms",
        ms(pct(&observed, 50.0)),
        ms(pct(&observed, 95.0)),
        ms(pct(&observed, 99.0)),
        ms(pct(&modeled, 50.0)),
        ms(pct(&modeled, 95.0)),
        ms(pct(&modeled, 99.0)),
    );
    report.push(serde_json::json!({
        "case": "real-clock half-block Full",
        "waits_on": waited,
        "interval_observed": stats(&observed),
        "interval_computed": stats(&modeled),
    }));
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("usage: pacing <report.json>")?,
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "paint_interval_ms": ms(tick),
            "scenario_s": SCENARIO.as_secs_f64(),
            "intervals_are": "computed from render times, but for the real-clock case",
            "frames_are": "paced on the wall clock as the TUI's; frame_hot back to back",
            "cases": report,
        }))?,
    )?;
    let _ = writeln!(stdout, "\nreport: {}", path.display());
    Ok(())
}
