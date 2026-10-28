//! Renders the TUI off-screen via ratatui's TestBackend and rasterizes every cell
//! into a `CELL_W`×`CELL_H` px tile — of a PNG, an animated GIF (`--gif`) or
//! `--proof` frame sequences — so the visual output is verifiable without a real
//! terminal.

mod cutaway;
mod encode;
mod proof;

/// The bundled pack the cell text draws its icons from, read once.
fn icons() -> &'static pixtuoid_scene::pack::OfficeArt {
    static PACK: std::sync::LazyLock<pixtuoid_scene::pack::OfficeArt> =
        std::sync::LazyLock::new(|| load_bundled_pack().expect("the bundled pack loads"));
    &PACK
}
mod scenes;

use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context as _, Result};
use clap::Parser;
use pixtuoid::dev::{DrawCtx, draw_scene};
use pixtuoid_core::SceneState;
use pixtuoid_scene::pack::load_bundled_pack;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::cutaway::CutawayOffice;
use crate::encode::{
    AnimJob, Timeline, centered_crop, compute_crop_rect, print_walkability_report, save_animation,
    save_backend_as_png, save_renderer_animation,
};
use crate::scenes::{
    GatewayState, anim_scene, capture_live_scene, dashboard_scene, inject_openclaw_presence,
    meeting_scene, sample_scene,
};

const COLS: u16 = 192;
const ROWS: u16 = 80;
const CELL_W: u32 = 8;
const CELL_H: u32 = 16;

#[derive(Debug, Parser)]
#[command(about = "Render the TUI off-screen to a PNG, GIF or proof frames for verification")]
#[command(group(clap::ArgGroup::new("animation").args(["gif", "anim", "proof"]).multiple(true)))]
struct SnapshotArgs {
    /// Output PNG path.
    #[arg(default_value = "snapshot.png")]
    out: PathBuf,

    /// Capture real CC events by watching --projects-root for --listen-secs.
    #[arg(long)]
    live: bool,

    /// CC project root to watch (only with --live).
    #[arg(long, default_value_t = default_projects_root())]
    projects_root: String,

    /// How many seconds to listen for events (only with --live).
    #[arg(long, default_value_t = 5)]
    listen_secs: u64,

    /// After rendering the scene normally, overlay every non-walkable
    /// pixel in semi-transparent red so the restricted zones are visible.
    #[arg(long)]
    debug_walkable: bool,

    /// Override the snapshot terminal width (cells).
    #[arg(long)]
    cols: Option<u16>,

    /// Override the snapshot terminal height (cells).
    #[arg(long)]
    rows: Option<u16>,

    /// Cap on home desks per floor for the sample scene. Agents past
    /// this count overflow to additional floors.
    #[arg(long, default_value_t = 12)]
    max_desks: usize,

    /// Number of agents in the sample scene.
    #[arg(long, default_value_t = 12)]
    agents: usize,

    /// Output an animated GIF instead of a static PNG.
    #[arg(long)]
    gif: bool,

    /// Draw the office as the cutaway the TUI fits to this terminal of
    /// `CELL_W`×`CELL_H` cells, in place of the classic office, above its footer.
    // Each draws over the classic's office cells, which the cutaway covers, or
    // crops a frame the cutaway paints from its top-left.
    #[arg(long, conflicts_with_all = [
        "dashboard", "connection", "onboarding", "help_open", "theme_picker", "popup",
        "hover", "debug_walkable", "crop_agent", "crop_furniture", "crop_mascot",
    ])]
    cutaway: bool,

    /// GIF duration in seconds (only with --gif, which --anim implies).
    #[arg(long, default_value_t = 5)]
    gif_duration: u64,

    /// GIF frame rate (only with --gif, which --anim implies).
    #[arg(long, default_value_t = 10)]
    gif_fps: u64,

    /// Color theme name.
    #[arg(long, default_value = "normal")]
    theme: String,

    /// Floor seed — hashed to one of the floor layout variants.
    #[arg(long, default_value_t = 0)]
    floor_seed: u64,

    /// Schedule floor navigations inside a --gif capture: repeatable
    /// `--navigate-at <sec>:<floor>` (0-based floor). A navigation during another
    /// one's slide is dropped.
    #[arg(
        long = "navigate-at",
        value_name = "SEC:FLOOR",
        requires = "gif",
        conflicts_with = "anim"
    )]
    navigate_at: Vec<String>,

    /// Render an empty office (no agents).
    #[arg(long)]
    empty: bool,

    /// Inject an OpenClaw gateway presence (the wandering lobster mascot) in the
    /// given state.
    #[arg(long, value_enum)]
    openclaw: Option<GatewayState>,

    /// Gateway PORTS to stage for `--openclaw`, comma-separated (default: one, the
    /// upstream default port).
    // The multi-instance render is the one thing no gate can check, so N gateways
    // must be renderable to a PNG a human can look at.
    #[arg(long, value_delimiter = ',')]
    openclaw_ports: Vec<String>,

    /// Override local hour-of-day (0–23) used by time-of-day lighting.
    #[arg(long)]
    now_hour: Option<u32>,

    /// Override the local day (1 = `pixtuoid_scene::localclock`'s base date) used by time-of-day.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    now_day: u32,

    /// Seconds past `--now-hour` (0–3599), fine enough to sample a weather
    /// transition.
    #[arg(long, default_value_t = 0, requires = "now_hour", value_parser = clap::value_parser!(u64).range(0..3600))]
    now_sec: u64,

    /// Force a specific weather, bypassing the clock-based 10-minute cycle.
    /// One of: clear | rain | storm | snow | fog | overcast | windy | smog.
    #[arg(long)]
    weather: Option<String>,

    /// Force the `?` keyboard help overlay open (for screenshots).
    #[arg(long)]
    help_open: bool,

    /// Force the source-death footer warning with the given source
    /// name (for screenshots), e.g. --source-warning claude-code.
    #[arg(long)]
    source_warning: Option<String>,

    /// Force the decode-drift footer nudge for the given comma-separated
    /// source label-prefixes (for screenshots), e.g. --drift-warning cc,cx.
    /// Lower priority than --source-warning (source-death preempts it).
    #[arg(long)]
    drift_warning: Option<String>,

    /// Force the theme picker open at the given row index (for screenshots).
    #[arg(long)]
    theme_picker: Option<usize>,

    /// Force the version popup fully visible (for screenshots).
    #[arg(long)]
    popup: bool,

    /// Force a hover tooltip at terminal cell "x,y" (for screenshots / shadow
    /// visual checks), e.g. --hover 30,40.
    #[arg(long, value_name = "X,Y")]
    hover: Option<String>,

    /// Add a wandering office pet to a renderer-driven --gif capture
    /// (cat | dog).
    #[arg(long, value_name = "KIND", requires = "gif", conflicts_with = "anim")]
    pets: Option<String>,

    /// Render with the agent-dashboard popup open over a representative mixed
    /// cc/cx/rx scene.
    #[arg(long, conflicts_with_all = ["anim", "gif", "live", "empty", "pets"])]
    dashboard: bool,

    /// Render with the Sources panel open over a representative mixed fixture
    /// (per-CLI hook state + live connection).
    #[arg(long, conflicts_with_all = ["anim", "gif", "live", "empty", "pets", "dashboard"])]
    connection: bool,

    /// Render with the first-run onboarding "move-in" overlay open (fully revealed)
    /// over a representative roster.
    #[arg(long, conflicts_with_all = ["anim", "gif", "live", "empty", "pets", "dashboard", "connection"])]
    onboarding: bool,

    /// Animation-verification mode: render ONE agent walking to + settling at a
    /// chosen furniture. One of: couch | sofa | chair | pantry |
    /// printer | vending | island | snackshelf | desk. Forces `--gif`; the
    /// agent is back-dated so its walk-out starts at frame 0.
    #[arg(long)]
    anim: Option<String>,

    /// Override the `--anim` pre-roll skip (ms). The default skips to the
    /// walk-out (settle/sit follow); set a LARGER value to start the capture at
    /// a later phase — e.g. desk_dwell + walk + sit_dwell to capture the LEAVE
    /// (walk-back).
    #[arg(long, requires = "anim", conflicts_with_all = ["pets", "navigate_at"])]
    anim_skip_ms: Option<u64>,

    /// Stage N agents (2–3) converging on ONE meeting room in a `--gif`
    /// capture. Composes with `--agents`: the staged agents take desk indices
    /// 0..N and the normal sample archetypes fill desks N..--agents so the floor
    /// stays alive. Auto-computes a pre-roll so the encoded clip starts just
    /// before the first agent rises; `--warmup-secs` overrides it.
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u8).range(2..=3),
        requires = "gif",
        conflicts_with_all = ["anim", "dashboard", "empty", "live", "pets", "navigate_at"]
    )]
    meeting: Option<u8>,

    /// Pre-roll a `--gif` capture: advance the simulated clock through the
    /// real per-frame render (walk state advances) WITHOUT encoding frames
    /// for the first N seconds, so the clip starts mid-action. Overrides the
    /// `--meeting` auto-computed warmup. (`--anim` has its own pre-roll knob,
    /// `--anim-skip-ms`.)
    #[arg(
        long,
        value_name = "SECS",
        requires = "gif",
        conflicts_with_all = ["anim", "pets", "navigate_at"]
    )]
    warmup_secs: Option<f64>,

    /// Restrict `--anim sofa`/`couch`/`chair` to a seat with a given SEATED
    /// facing: `north` (back-view, `back_couch` sprite — sofa occludes the lower
    /// body) or `south` (front-view, `seated` sprite). Also picks WHICH home desk
    /// `--anim desk` stages, so the back-turned seat is reachable at all.
    #[arg(long)]
    anim_facing: Option<String>,

    /// Crop the generated PNG (and text preview) to the fixed crop window
    /// centered on the agent with this session id, or the one agent with this
    /// label. Static-PNG path only.
    // The --gif/--anim paths return before the crop is computed, so clap rejects
    // the combination instead of silently ignoring the flag.
    #[arg(long, conflicts_with_all = ["crop_furniture", "gif", "anim"])]
    crop_agent: Option<String>,

    /// Crop the generated PNG (and text preview) to the fixed crop window
    /// centered on a furniture piece. Static-PNG path only.
    /// One of: pantry | couch | vending | printer | meeting | sofa | chair |
    /// island | snackshelf | desk.
    #[arg(long, conflicts_with_all = ["gif", "anim"])]
    crop_furniture: Option<String>,

    /// Give the agent with this session id, or the one agent with this label,
    /// a Top burn tier (claude-fable-5 at fresh ultra effort → ember hair + flame crown).
    #[arg(long)]
    flame: Option<String>,

    /// Crop the generated PNG to the fixed crop window centered on the gateway
    /// lobster mascot `--openclaw` stages. Static-PNG path only.
    // Visibility is still checked at runtime (the mascot may be off-canvas).
    #[arg(
        long,
        requires = "openclaw",
        conflicts_with_all = ["crop_agent", "crop_furniture", "gif", "anim"]
    )]
    crop_mascot: bool,

    /// Render the §3 split-screen proof replay from a CC session transcript:
    /// left = typed transcript, right = the real reducer+renderer
    /// replaying the same decoded events; annotations burned in. Emits PNG
    /// frame sequences to --frames-dir/{wide,tall}/.
    #[arg(long, value_name = "SESSION", value_hint = clap::ValueHint::FilePath,
          conflicts_with_all = ["gif", "anim", "meeting", "pets", "navigate_at",
          "dashboard", "connection", "onboarding", "empty", "live",
          "crop_agent", "crop_furniture", "crop_mascot", "debug_walkable"])]
    proof: Option<std::path::PathBuf>,

    /// Output directory for an animation's lossless PNG frames (`f0001.png`, …):
    /// --gif/--anim write them here INSTEAD of the GIF at OUT; --proof writes its
    /// two sequences into wide/ + tall/ inside it.
    #[arg(long, value_hint = clap::ValueHint::DirPath, requires = "animation")]
    frames_dir: Option<std::path::PathBuf>,

    /// --proof frame rate.
    #[arg(long, default_value_t = 12, requires = "proof")]
    proof_fps: u64,

    /// --proof clip length in seconds (the idle tail past the last event included).
    #[arg(long, default_value_t = 26, requires = "proof")]
    proof_secs: u64,
}

fn default_projects_root() -> String {
    // Honor CLAUDE_CONFIG_DIR via the same resolver the runtime uses, rather
    // than re-hardcoding the ~/.claude shape.
    pixtuoid_core::source::claude_code::ClaudeCodeSource::default_paths()
        .projects_root
        .to_string_lossy()
        .into_owned()
}

fn parse_navigations(specs: &[String]) -> Result<Vec<(u64, usize)>> {
    specs
        .iter()
        .map(|s| {
            let (sec, floor) = s
                .split_once(':')
                .with_context(|| format!("--navigate-at '{s}': expected SEC:FLOOR"))?;
            let ms = (sec
                .parse::<f64>()
                .with_context(|| format!("--navigate-at '{s}': bad SEC"))?
                * 1000.0) as u64;
            let floor = floor
                .parse::<usize>()
                .with_context(|| format!("--navigate-at '{s}': bad FLOOR"))?;
            Ok((ms, floor))
        })
        .collect()
}

/// The centre of the pixels a pointer finds the first mascot on.
fn mascot_centre(hits: &pixtuoid_scene::look::Hits) -> Option<pixtuoid_scene::layout::Point> {
    use pixtuoid_scene::display::HoverTarget;
    use pixtuoid_scene::layout::{Bounds, Point};
    let layout = hits.layout();
    let at = |(x, y)| {
        hits.hovers().at(Bounds {
            x,
            y,
            width: 1,
            height: 1,
        })
    };
    let pixels = || (0..layout.buf_h).flat_map(|y| (0..layout.buf_w).map(move |x| (x, y)));
    let mascot = pixels().find_map(|p| at(p).filter(|t| matches!(t, HoverTarget::Mascot(_))))?;
    let (x0, y0, x1, y1) = pixels()
        .filter(|&p| at(p) == Some(mascot))
        .fold((u16::MAX, u16::MAX, 0, 0), |(x0, y0, x1, y1), (x, y)| {
            (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
        });
    Some(Point {
        x: x0.midpoint(x1),
        y: y0.midpoint(y1),
    })
}

fn main() -> Result<()> {
    let args = SnapshotArgs::parse();

    let weather = match pixtuoid_scene::sky::WeatherPolicy::from_name(args.weather.as_deref()) {
        Ok(w) => w,
        Err(valid) => anyhow::bail!(
            "unknown --weather {:?}; valid: {}",
            args.weather.unwrap_or_default(),
            valid.join(" | ")
        ),
    };

    let now = match args.now_hour {
        Some(h) => {
            pixtuoid_scene::localclock::try_on_day(args.now_day - 1, h)
                .with_context(|| format!("invalid --now-day/--now-hour {}:{h}", args.now_day))?
                + std::time::Duration::from_secs(args.now_sec)
        }
        None => SystemTime::now(),
    };
    let cols = args.cols.unwrap_or(COLS);
    let rows = args.rows.unwrap_or(ROWS);
    let mut skip_ms = 0u64;
    let scene = if let Some(target) = args.anim.as_deref() {
        let (s, skip) = anim_scene(
            now,
            target,
            cols,
            rows,
            args.floor_seed,
            args.anim_facing.as_deref(),
        );
        skip_ms = args.anim_skip_ms.unwrap_or(skip);
        eprintln!("ANIM pre-roll skip = {skip_ms}ms (default {skip}ms)");
        s
    } else if let Some(n) = args.meeting {
        let (s, warmup) = meeting_scene(
            now,
            n as usize,
            cols,
            rows,
            args.floor_seed,
            args.max_desks,
            args.agents,
        )?;
        skip_ms = warmup;
        eprintln!("MEETING auto warmup = {warmup}ms");
        s
    } else if args.empty {
        SceneState::uniform(args.max_desks)
    } else if args.live {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        rt.block_on(capture_live_scene(&args.projects_root, args.listen_secs))?
    } else if args.dashboard {
        dashboard_scene(now)
    } else {
        sample_scene(now, args.max_desks, args.agents)
    };
    if let Some(secs) = args.warmup_secs {
        skip_ms = (secs * 1000.0) as u64;
        eprintln!("WARMUP pre-roll = {skip_ms}ms (explicit --warmup-secs)");
    }
    let mut scene = scene;
    if let Some(key) = &args.flame {
        let id = scenes::agent_named(&scene, "--flame", key)?.agent_id;
        if let Some(a) = scene.agents.get_mut(&id) {
            a.model = Some("claude-fable-5".into());
            a.effort = Some(pixtuoid_core::state::EffortObservation::new(
                "ultra".into(),
                now,
            ));
        }
    }
    if let Some(state) = args.openclaw {
        inject_openclaw_presence(&mut scene, state, now, &args.openclaw_ports)?;
    }
    let backend = TestBackend::new(cols, rows);
    let mut term = Terminal::new(backend)?;
    let pack = std::sync::Arc::new(load_bundled_pack()?);
    let mut floor = pixtuoid_scene::floor::PerFloor::new(std::sync::Arc::clone(&pack));
    // A typo'd theme silently rendering NORMAL would put wrong-palette art into
    // the docs/site screenshot pipelines.
    let theme = pixtuoid_scene::theme::theme_by_name(&args.theme).ok_or_else(|| {
        let valid: Vec<&str> = pixtuoid_scene::theme::ALL_THEMES
            .iter()
            .map(|t| t.name)
            .collect();
        anyhow::anyhow!(
            "unknown --theme {:?}; valid: {}",
            args.theme,
            valid.join(" | ")
        )
    })?;

    if let Some(session) = args.proof.as_deref() {
        let frames_dir = args
            .frames_dir
            .as_deref()
            .context("--proof requires --frames-dir")?;
        proof::render_proof(&proof::ProofJob {
            session,
            frames_dir,
            cols,
            rows,
            timeline: Timeline {
                fps: args.proof_fps,
                secs: args.proof_secs,
                start: now,
            },
            max_desks: args.max_desks,
            theme,
            pack: &pack,
            weather,
            cutaway: args.cutaway,
        })?;
        println!("wrote proof frames → {}", frames_dir.display());
        return Ok(());
    }

    let navigations = parse_navigations(&args.navigate_at)?;
    let pet_vec: Vec<pixtuoid_scene::pet::Pet> = match args.pets.as_deref() {
        None => vec![],
        Some(kind_str) => {
            use pixtuoid_scene::pet::{Pet, PetKind};
            let kind = match kind_str {
                "cat" => PetKind::Cat,
                "dog" => PetKind::Dog,
                other => anyhow::bail!("unknown --pets {:?}; valid: cat | dog", other),
            };
            vec![Pet {
                kind,
                name: "Pixel".into(),
            }]
        }
    };

    if args.floor_seed != 0 && (!navigations.is_empty() || !pet_vec.is_empty()) {
        eprintln!(
            "--floor-seed is ignored on the renderer path (--navigate-at / --pets): \
             TuiRenderer derives per-floor seeds internally"
        );
    }
    let anim_job = AnimJob {
        path: &args.out,
        frames_dir: args.frames_dir.as_deref(),
        timeline: Timeline {
            fps: args.gif_fps,
            secs: args.gif_duration,
            start: now,
        },
        scene: &scene,
        pack: &pack,
        theme,
        weather,
    };
    let anim_dest = args.frames_dir.as_deref().unwrap_or(&args.out);
    if !navigations.is_empty() || !pet_vec.is_empty() {
        save_renderer_animation(&anim_job, term, &navigations, pet_vec, args.cutaway)?;
        println!("wrote {}", anim_dest.display());
        return Ok(());
    }

    let mut cutaway = args
        .cutaway
        .then(|| CutawayOffice::of_draw_scene(&pack, cols, rows))
        .transpose()?;
    let mut floor_meta = pixtuoid_scene::floor::FloorMeta::ground().with_weather(weather);
    floor_meta.floor_seed = args.floor_seed;
    if args.gif || args.anim.is_some() {
        save_animation(
            &anim_job,
            &mut term,
            &mut floor,
            floor_meta,
            skip_ms,
            args.debug_walkable,
            cutaway,
        )?;
        println!("wrote {}", anim_dest.display());
        return Ok(());
    }

    // Reuse the REAL formatters so the screenshot wording can't drift from
    // production.
    let deaths: Vec<_> = args
        .source_warning
        .as_deref()
        .map(|src| pixtuoid_core::source::manager::SourceDeath::new(src, "forced for screenshot"))
        .into_iter()
        .collect();
    let drifted: Vec<&str> = args
        .drift_warning
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let warning_text = pixtuoid::dev::footer_warning(&deaths, &drifted);
    let mut office = pixtuoid_scene::floor::PerOffice::new();
    if args.empty {
        settle_empty(&mut floor, cutaway.as_mut());
    }
    let (dash_rows, dash_selected) = if args.dashboard {
        let folds = pixtuoid::dev::DashboardFolds::default();
        let rows = pixtuoid::dev::build_dashboard_rows(&scene, &folds);
        let sel = rows.first().map(|r| r.agent_id);
        (rows, sel)
    } else {
        (Vec::new(), None)
    };

    // Deterministic — no FS probes — so the demo image is reproducible across
    // machines.
    let (connection_rows, connection_socket_line) = if args.connection {
        use pixtuoid::dev::{ConnState, ConnectionRow, DaemonRollup, LiveFacet, LiveInfo};
        use std::path::PathBuf;
        use std::time::Duration;
        let mk = |source_id, label_prefix, display_name, state, cfg: Option<&str>| ConnectionRow {
            source_id,
            label_prefix,
            display_name,
            state,
            config_path: cfg.map(PathBuf::from),
            target: None,
            health: None,
        };
        let agents = |n: usize, age_s: u64| LiveInfo {
            facet: LiveFacet::Agents {
                agents: n,
                last_event_age: Some(Duration::from_secs(age_s)),
            },
            dead: false,
        };
        let rows = vec![
            (
                mk(
                    "claude-code",
                    "cc",
                    "Claude Code",
                    ConnState::Connected,
                    Some("~/.claude/settings.json"),
                ),
                agents(2, 3),
            ),
            (
                mk(
                    "codex",
                    "cx",
                    "Codex",
                    ConnState::Disconnected,
                    Some("~/.codex/config.toml"),
                ),
                LiveInfo::default(),
            ),
            (
                mk(
                    "reasonix",
                    "rx",
                    "Reasonix",
                    ConnState::NoCli { connected: false },
                    None,
                ),
                LiveInfo::default(),
            ),
            (
                mk(
                    "codewhale",
                    "cw",
                    "CodeWhale",
                    ConnState::Connected,
                    Some("~/.codewhale/config.toml"),
                ),
                LiveInfo {
                    facet: LiveFacet::default(),
                    dead: true,
                },
            ),
            (
                mk(
                    "opencode",
                    "oc",
                    "opencode",
                    ConnState::Disconnected,
                    Some("~/.config/opencode/plugins/pixtuoid.ts"),
                ),
                LiveInfo::default(),
            ),
            (
                mk(
                    "antigravity",
                    "ag",
                    "Antigravity",
                    ConnState::Connected,
                    None,
                ),
                agents(1, 12),
            ),
            // A DAEMON row, so the captured panel shows the daemon LIVE cell next
            // to the agent rows' N-agents form.
            (
                mk(
                    "openclaw",
                    "ok",
                    "OpenClaw",
                    ConnState::Connected,
                    Some("~/.openclaw/openclaw.json"),
                ),
                LiveInfo {
                    facet: LiveFacet::Daemon(Some(DaemonRollup {
                        instances: std::num::NonZeroUsize::new(2).expect("two gateways"),
                        state: pixtuoid_core::state::DaemonState::Running,
                    })),
                    dead: false,
                },
            ),
        ];
        (rows, "socket  /run/user/501/pixtuoid.sock".to_string())
    } else {
        (Vec::new(), String::new())
    };
    let onboarding_frame = if args.onboarding {
        use pixtuoid::dev::WelcomeRow;
        let mk = |source_id, label_prefix, display_name: &str, checked| WelcomeRow {
            source_id,
            label_prefix,
            display_name: display_name.to_string(),
            checked,
        };
        pixtuoid::dev::OnboardingFrame {
            open: true,
            rows: vec![
                mk("claude-code", "cc", "Claude Code", true),
                mk("codex", "cx", "Codex", true),
                mk("cursor", "cu", "Cursor CLI", false),
            ],
            selected: 0,
            elapsed_ms: 100_000,
            dim: pixtuoid::dev::dim_opening(100_000),
        }
    } else {
        pixtuoid::dev::OnboardingFrame::default()
    };
    let dashboard_frame = pixtuoid::dev::DashboardFrame {
        open: args.dashboard,
        rows: dash_rows,
        selected: dash_selected,
        scroll: 0,
    };
    let connection_frame = pixtuoid::dev::ConnectionFrame {
        open: args.connection,
        rows: connection_rows,
        selected: 0,
        confirm: None,
        result: None,
        socket_line: connection_socket_line,
    };
    let frames = pixtuoid::dev::RenderFrames {
        theme_picker: args.theme_picker,
        version_popup: pixtuoid::dev::VersionPopupFrame {
            open: args.popup,
            upgraded: true,
            ..Default::default()
        },
        help_open: args.help_open,
        dashboard: dashboard_frame,
        connection: connection_frame,
        onboarding: onboarding_frame,
        ..Default::default()
    };
    let mut draw_ctx = DrawCtx {
        mouse_pos: args.hover.as_deref().and_then(|s| {
            let (x, y) = s.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        }),
        debug_walkable: args.debug_walkable,
        footer: pixtuoid::dev::footer_context(&scene, None, false, None, warning_text.as_deref()),
        overlays: frames.overlays(pixtuoid::dev::popup_whole(args.popup), &[]),
        ..DrawCtx::offscreen(&mut floor, office.stores(), theme, &scene, now, floor_meta)
    };
    let drawn = draw_scene(&mut term, &mut draw_ctx)?;
    if let Some(cutaway) = &mut cutaway {
        cutaway.render(draw_ctx.world, theme)?;
    }

    if args.debug_walkable {
        print_walkability_report(&term, args.floor_seed)?;
    }

    let crop_rect = if args.crop_mascot {
        // The mascot wanders to a time-derived cell, so we crop on the position
        // the renderer actually resolved, not a precomputed layout point.
        let at = drawn
            .as_ref()
            .and_then(mascot_centre)
            .context("--crop-mascot needs a visible mascot")?;
        Some(centered_crop(at, cols, rows))
    } else {
        compute_crop_rect(&args, &scene, &floor.ctx.history, cols, rows, now)?
    };
    let area = crop_rect.unwrap_or(ratatui::layout::Rect::new(0, 0, cols, rows));

    save_backend_as_png(&term, &args.out, area, cutaway.as_ref())?;
    println!("wrote {}", args.out.display());

    println!("\n--- text preview (symbols only) ---");
    let buf = term.backend().buffer();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            print!("{}", buf[(x, y)].symbol());
        }
        println!();
    }
    Ok(())
}

/// An empty office's still: `floor`, and `cutaway`'s where there is one, at
/// the light they fade to. A still has no time to animate the fade.
fn settle_empty(
    floor: &mut pixtuoid_scene::floor::PerFloor,
    cutaway: Option<&mut CutawayOffice<pixtuoid_scene::floor::FloorSession>>,
) {
    for floor in std::iter::once(floor).chain(cutaway.map(CutawayOffice::floor_mut)) {
        floor.ctx.vacancy_dim.snap_to_empty();
    }
}

/// Floors whose scheduled navigation comes due at `elapsed_ms`, firing each
/// schedule entry exactly once (marks `fired`).
fn due_navigations(
    navigations: &[(u64, usize)],
    fired: &mut [bool],
    elapsed_ms: u64,
) -> Vec<usize> {
    let mut due = Vec::new();
    for (n, &(at_ms, floor)) in navigations.iter().enumerate() {
        if !fired[n] && elapsed_ms >= at_ms {
            fired[n] = true;
            due.push(floor);
        }
    }
    due
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_navigations_happy_and_fractional() {
        assert_eq!(
            parse_navigations(&["3:1".to_string(), "2.5:0".to_string()]).unwrap(),
            vec![(3000, 1), (2500, 0)]
        );
    }

    #[test]
    fn parse_navigations_truncates_fractional_ms() {
        assert_eq!(
            parse_navigations(&["0.9999:0".to_string()]).unwrap(),
            vec![(999, 0)]
        );
    }

    #[test]
    fn parse_navigations_rejects_bad_input() {
        for bad in ["5-1", "5:x", "x:1", "", ":", "5:"] {
            assert!(
                parse_navigations(&[bad.to_string()]).is_err(),
                "accepted {bad:?}"
            );
        }
    }

    /// The committed multi-floor clip's clock: 10s at 15fps.
    fn clip_timeline() -> Timeline {
        Timeline {
            fps: 15,
            secs: 10,
            start: std::time::UNIX_EPOCH,
        }
    }

    #[test]
    fn due_navigations_fires_each_exactly_once_in_schedule_order() {
        let navs = vec![(7000u64, 0usize), (3000, 1)];
        let mut fired = vec![false; navs.len()];
        let mut hits: Vec<(usize, usize)> = Vec::new();
        let timeline = clip_timeline();
        for i in 0..timeline.frame_count() {
            for floor in due_navigations(&navs, &mut fired, timeline.elapsed_ms(i)) {
                hits.push((i, floor));
            }
        }
        assert_eq!(hits, vec![(45, 1), (105, 0)]);
    }

    #[test]
    fn due_navigations_late_schedule_still_fires_within_capture() {
        // With truncating per-frame accumulation (i * 66ms) a 9.9s navigation
        // never fired in a 10s/15fps capture.
        let navs = vec![(9900u64, 1usize)];
        let mut fired = vec![false; 1];
        let mut hit = None;
        let timeline = clip_timeline();
        for i in 0..timeline.frame_count() {
            if !due_navigations(&navs, &mut fired, timeline.elapsed_ms(i)).is_empty() {
                hit = Some(i);
            }
        }
        assert_eq!(hit, Some(149));
    }

    fn crop_args(extra: &[&str]) -> SnapshotArgs {
        SnapshotArgs::try_parse_from([&["snapshot"], extra].concat()).unwrap()
    }

    #[test]
    fn crop_rect_centers_on_the_pantry_waypoint() {
        let now = SystemTime::now();
        let scene = sample_scene(now, 12, 12);
        let history = pixtuoid_scene::pose::PoseHistory::new();
        let args = crop_args(&["--crop-furniture", "pantry"]);
        let rect = compute_crop_rect(&args, &scene, &history, 192, 64, now)
            .unwrap()
            .expect("pantry crop");
        let layout =
            pixtuoid_scene::layout::SceneLayout::compute_with_seed(192, 126, Some(12), 0).unwrap();
        let pantry = layout
            .waypoints
            .iter()
            .find(|w| w.kind == pixtuoid_scene::layout::WaypointKind::Pantry)
            .unwrap();
        let (cx, cy) = (
            pantry.pos.x,
            pantry.pos.y / pixtuoid_scene::layout::CELL_ROWS,
        );
        assert!(rect.x <= cx && cx < rect.x + rect.width, "x not in crop");
        assert!(rect.y <= cy && cy < rect.y + rect.height, "y not in crop");
    }

    /// The seated fallback resolves an agent on a floor ABOVE the ground one.
    /// `desk_index` is GLOBAL, and every floor gets its own geometry and seed, so
    /// reading floor 0's is how an agent at a real desk reports having none.
    #[test]
    fn crop_rect_seats_an_agent_that_lives_on_an_upper_floor() {
        let now = SystemTime::now();
        // More agents than one floor holds, so the tail lands on floor 1+.
        let scene = sample_scene(now, 4, 9);
        let upper = scene
            .agents
            .values()
            .find(|a| a.floor_idx > 0)
            .expect("a scene with 9 agents over 4 desks must reach an upper floor");
        let (floor, desk) = (upper.floor_idx, upper.desk_index.0);

        let history = pixtuoid_scene::pose::PoseHistory::new();
        let args = crop_args(&["--crop-agent", &upper.session_id]);
        let rect = compute_crop_rect(&args, &scene, &history, 192, 64, now)
            .unwrap_or_else(|e| panic!("floor {floor} agent at desk {desk} must crop: {e}"))
            .expect("a seated agent crops");
        assert!(
            rect.width > 0 && rect.height > 0,
            "empty crop for desk {desk}"
        );
    }

    #[test]
    fn crop_rect_without_flags_is_none() {
        let now = SystemTime::now();
        let scene = sample_scene(now, 12, 12);
        let history = pixtuoid_scene::pose::PoseHistory::new();
        let args = crop_args(&[]);
        assert!(
            compute_crop_rect(&args, &scene, &history, 192, 64, now)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn crop_rect_fails_loudly_on_typos_and_unknown_agents() {
        let now = SystemTime::now();
        let scene = sample_scene(now, 12, 12);
        let history = pixtuoid_scene::pose::PoseHistory::new();

        let typo = crop_args(&["--crop-furniture", "fridge"]);
        let err = compute_crop_rect(&typo, &scene, &history, 192, 64, now).unwrap_err();
        assert!(err.to_string().contains("valid: pantry"), "{err}");

        let ghost = crop_args(&["--crop-agent", "ghost"]);
        let err = compute_crop_rect(&ghost, &scene, &history, 192, 64, now).unwrap_err();
        assert!(err.to_string().contains("(demo-turn)"), "{err}");
    }

    #[test]
    fn crop_flags_conflict_with_gif_and_anim() {
        assert!(SnapshotArgs::try_parse_from(["snapshot", "--gif", "--crop-agent", "x"]).is_err());
        assert!(SnapshotArgs::try_parse_from(["snapshot", "--crop-mascot"]).is_err());
        assert!(
            SnapshotArgs::try_parse_from(["snapshot", "--crop-mascot", "--openclaw", "idle"])
                .is_ok()
        );
        assert!(
            SnapshotArgs::try_parse_from([
                "snapshot",
                "--anim",
                "couch",
                "--crop-furniture",
                "pantry"
            ])
            .is_err()
        );
    }

    #[test]
    fn meeting_flag_parses_caps_and_conflicts() {
        let ok = ["snapshot", "out.gif", "--gif", "--meeting", "3"];
        assert!(SnapshotArgs::try_parse_from(ok).is_ok());
        for bad in [
            vec!["snapshot", "--gif", "--meeting", "1"],
            vec!["snapshot", "--gif", "--meeting", "4"],
            vec!["snapshot", "--meeting", "3"],
            vec!["snapshot", "--gif", "--meeting", "3", "--anim", "sofa"],
            vec!["snapshot", "--gif", "--meeting", "3", "--pets", "cat"],
            vec![
                "snapshot",
                "--gif",
                "--meeting",
                "3",
                "--navigate-at",
                "3:1",
            ],
            vec!["snapshot", "--gif", "--meeting", "3", "--dashboard"],
            vec!["snapshot", "--gif", "--meeting", "3", "--empty"],
        ] {
            assert!(
                SnapshotArgs::try_parse_from(bad.clone()).is_err(),
                "accepted {bad:?}"
            );
        }
    }

    #[test]
    fn warmup_flag_requires_gif_and_conflicts_with_anim() {
        assert!(
            SnapshotArgs::try_parse_from(["snapshot", "--gif", "--warmup-secs", "13.5"]).is_ok()
        );
        assert!(SnapshotArgs::try_parse_from(["snapshot", "--warmup-secs", "5"]).is_err());
        assert!(
            SnapshotArgs::try_parse_from([
                "snapshot",
                "--gif",
                "--warmup-secs",
                "5",
                "--anim",
                "sofa"
            ])
            .is_err()
        );
    }

    /// The renderer path (`--pets` / `--navigate-at`) encodes from t=0, so clap
    /// refuses each pre-roll flag beside it rather than dropping it.
    #[test]
    fn pre_roll_flags_conflict_with_the_renderer_path() {
        let gif = &["snapshot", "--gif"][..];
        let pre_rolls = [
            &["--warmup-secs", "5"][..],
            &["--anim", "sofa"],
            &["--anim", "sofa", "--anim-skip-ms", "5"],
        ];
        let renderers = [&["--pets", "cat"][..], &["--navigate-at", "3:1"]];
        for alone in pre_rolls.iter().chain(&renderers) {
            let argv = [gif, alone].concat();
            assert!(
                SnapshotArgs::try_parse_from(&argv).is_ok(),
                "rejected {argv:?}"
            );
        }
        let skip_alone = &["--anim-skip-ms", "5"][..];
        let argv = [gif, skip_alone].concat();
        assert!(
            SnapshotArgs::try_parse_from(&argv).is_err(),
            "accepted {argv:?}"
        );
        for pre_roll in pre_rolls.into_iter().chain([skip_alone]) {
            for renderer in renderers {
                let argv = [gif, pre_roll, renderer].concat();
                assert!(
                    SnapshotArgs::try_parse_from(&argv).is_err(),
                    "accepted {argv:?}"
                );
            }
        }
    }

    #[test]
    fn frames_dir_needs_an_animation() {
        for ok in [
            vec!["snapshot", "--gif", "--frames-dir", "d"],
            vec!["snapshot", "--anim", "sofa", "--frames-dir", "d"],
            vec!["snapshot", "--proof", "f.jsonl", "--frames-dir", "d"],
        ] {
            assert!(
                SnapshotArgs::try_parse_from(ok.clone()).is_ok(),
                "rejected {ok:?}"
            );
        }
        assert_eq!(
            SnapshotArgs::try_parse_from(["snapshot", "--frames-dir", "d"])
                .unwrap_err()
                .kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn dashboard_flag_parses_and_conflicts_with_anim() {
        assert!(SnapshotArgs::try_parse_from(["snapshot", "out.png", "--dashboard"]).is_ok());
        assert!(
            SnapshotArgs::try_parse_from(["snapshot", "out.png", "--dashboard", "--anim", "desk"])
                .is_err()
        );
        assert!(
            SnapshotArgs::try_parse_from(["snapshot", "out.png", "--dashboard", "--gif"]).is_err()
        );
    }

    #[test]
    fn cutaway_conflicts_with_what_it_covers() {
        let parses = |argv: &[&str]| SnapshotArgs::try_parse_from(argv).is_ok();
        for flags in [
            &["--dashboard"][..],
            &["--connection"],
            &["--onboarding"],
            &["--help-open"],
            &["--theme-picker", "0"],
            &["--popup"],
            &["--hover", "1,1"],
            &["--debug-walkable"],
            &["--crop-agent", "x"],
            &["--crop-furniture", "pantry"],
            &["--crop-mascot", "--openclaw", "idle"],
        ] {
            let alone = [&["snapshot"][..], flags].concat();
            assert!(parses(&alone), "rejected {alone:?}");
            let with = [&alone[..], &["--cutaway"]].concat();
            assert!(!parses(&with), "accepted {with:?}");
        }
        for argv in [
            &["snapshot", "--cutaway"][..],
            &["snapshot", "--cutaway", "--gif"],
            &["snapshot", "--cutaway", "--gif", "--warmup-secs", "1"],
            &["snapshot", "--cutaway", "--gif", "--pets", "cat"],
            &["snapshot", "--cutaway", "--gif", "--navigate-at", "3:1"],
            &["snapshot", "--cutaway", "--empty"],
            &["snapshot", "--cutaway", "--proof", "session.jsonl"],
        ] {
            assert!(parses(argv), "rejected {argv:?}");
        }
    }
}
