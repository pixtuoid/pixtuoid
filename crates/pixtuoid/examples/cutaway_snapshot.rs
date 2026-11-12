//! Render one frame of the CUTAWAY profile to a PNG, through the real seam: the
//! real sim produces a `SimFrame`, the real layout is computed at LOGICAL size,
//! and `look::render` paints it into a buffer sized in pixels at `--scale`.
//!
//! Usage: `cargo run --release --example cutaway_snapshot -- <out.png> [OPTIONS]`

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use image::{Rgb as ImgRgb, RgbImage};
use pixtuoid_core::source::claude_code;
use pixtuoid_core::state::{ActivityState, SceneState, ToolKind, ToolUse};
use pixtuoid_core::{AgentId, AgentSlot, FloorDesk};
use pixtuoid_scene::floor::{Conditions, FloorInputs, PerFloor, PerOffice, project_floor_scene};
use pixtuoid_scene::hit::HoverTarget;
use pixtuoid_scene::layout::Viewport;
use pixtuoid_scene::look::{Look, RenderInputs};
use pixtuoid_scene::render_scale::RenderScale;
use pixtuoid_scene::theme::theme_by_name;

/// Default logical office size — the SAME extent whatever `--scale` is, which
/// is the property the whole seam exists for: more pixels, not more desks.
/// `--logical WxH` overrides it; a bigger office is how the size-gated pieces
/// (corridor appliances) get placed at all.
const DEFAULT_LOGICAL: (u16, u16) = (160, 96);

/// The agents drawn unless `--agents` names another count.
const DEFAULT_AGENTS: usize = 10;
const _: () = assert!(
    pixtuoid_scene::DEMO_TEAMS < DEFAULT_AGENTS,
    "the default cast must share a working directory"
);

fn populate(scene: &mut SceneState, now: SystemTime, n: usize, repos: &[String], floor: usize) {
    let seated = now.checked_sub(Duration::from_secs(120)).unwrap_or(now);
    let recent = now.checked_sub(Duration::from_secs(3)).unwrap_or(now);
    for i in 0..n {
        let id = AgentId::from_transcript_path(&format!("/cutaway/a{i}.jsonl"));
        let cwd = PathBuf::from("/w").join(&repos[i % repos.len()]);
        let state = match i % 4 {
            0 => ActivityState::Running {
                item: Some(ToolUse {
                    tool_use_id: Some(format!("tu_{i}").into()),
                    detail: Some("Edit: src/lib.rs".into()),
                    kind: ToolKind::Edit,
                }),
            },
            1 => ActivityState::Idle,
            2 => ActivityState::RequiresAction {
                reason: Some("permission?".into()),
            },
            _ => ActivityState::Running { item: None },
        };
        scene.agents.insert(
            id,
            AgentSlot {
                agent_id: id,
                source: Arc::from(claude_code::SOURCE_NAME),
                session_id: Arc::from(format!("cut-{i:04x}").as_str()),
                cwd: Arc::from(cwd.as_path()),
                // No transcript path: only a subagent's would name the label.
                label: claude_code::cc_derive_label(Path::new(""), claude_code::SOURCE_NAME, &cwd)
                    .as_str()
                    .into(),
                state,
                state_started_at: seated,
                created_at: seated,
                arrived_at: Some(seated),
                last_event_at: recent,
                exiting_at: None,
                desk: Some(FloorDesk::new(floor, i)),
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
}

#[derive(Debug, Parser)]
struct CutawayArgs {
    out: PathBuf,
    #[arg(long)]
    scale: Option<u16>,
    #[arg(long, default_value_t = DEFAULT_AGENTS)]
    agents: usize,
    #[arg(long, default_value = "tokyo-night")]
    theme: String,
    #[arg(long, value_name = "WxH", value_parser = parse_logical)]
    logical: Option<(u16, u16)>,
    #[arg(long)]
    now_hour: Option<u32>,
    /// 1 = `pixtuoid_scene::localclock`'s day 0, as the classic snapshot's `--now-day`.
    #[arg(long, default_value_t = 1, requires = "now_hour", value_parser = clap::value_parser!(u32).range(1..))]
    now_day: u32,
    /// Seconds past `--now-hour`, fine enough to sample a weather transition.
    #[arg(long, default_value_t = 0, requires = "now_hour", value_parser = clap::value_parser!(u64).range(0..3600))]
    now_sec: u64,
    #[arg(long)]
    weather: Option<String>,
    #[arg(long, value_delimiter = ',', default_values_t = pixtuoid_scene::DEMO_REPOS[..pixtuoid_scene::DEMO_TEAMS].iter().map(|r| r.to_string()))]
    repos: Vec<String>,
    #[arg(long, default_value_t = 0)]
    floor: usize,
}

fn parse_logical(s: &str) -> Result<(u16, u16), String> {
    let side = |n: &str| n.parse::<u16>().map_err(|e| format!("{n:?}: {e}"));
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| format!("{s:?}: want WxH"))?;
    Ok((side(w)?, side(h)?))
}

fn main() -> Result<()> {
    let CutawayArgs {
        out,
        scale: scale_n,
        agents,
        theme: theme_name,
        logical,
        now_hour,
        now_day,
        now_sec,
        weather,
        repos,
        floor,
    } = CutawayArgs::parse();
    let (lw, lh) = logical.unwrap_or(DEFAULT_LOGICAL);
    let theme =
        theme_by_name(&theme_name).ok_or_else(|| anyhow!("unknown theme {theme_name:?}"))?;
    let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack()?);
    // Defaults to the pack's densest art, the density it was drawn for.
    let scale_n = scale_n.unwrap_or_else(|| pack.max_density_variant().get());
    let scale = RenderScale::new(scale_n).ok_or_else(|| anyhow!("--scale must be nonzero"))?;
    // The sky otherwise cycles its weather with the clock, so an hour alone
    // does not say what the room looks like.
    let policy =
        pixtuoid_scene::sky::WeatherPolicy::from_name(weather.as_deref()).map_err(|valid| {
            anyhow!(
                "unknown --weather {weather:?}; valid: {}",
                valid.join(" | ")
            )
        })?;
    let now = match now_hour {
        Some(h) => pixtuoid_scene::localclock::try_on_day(now_day - 1, h)
            .with_context(|| format!("invalid --now-day/--now-hour {now_day}:{h}"))?,
        None => SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    };
    let now = now + Duration::from_secs(now_sec);
    let conditions = Conditions::default().with_weather(policy);

    let desks = pixtuoid::dev::floor_caps_for_buffer(lw, lh);
    let mut scene = SceneState::new(desks);
    populate(&mut scene, now, agents, &repos, floor);
    // As the app takes a layout: an agent it has no desk for moves to a free one.
    scene.set_capacities(desks, now);

    // The real sim, at LOGICAL size, drawn through the painters' one entry
    // from floor `--floor`'s projection, the building's top while `--agents`
    // fits one floor.
    let mut per_floor = PerFloor::new(Arc::clone(&pack));
    let mut office = PerOffice::new();
    let floor_scene = project_floor_scene(&scene, floor);
    let rendered = pixtuoid_scene::look::render(
        &mut per_floor,
        office.stores(),
        Look::Cutaway {
            scale,
            text: pixtuoid_scene::look::WorldText::Baked,
        },
        RenderInputs {
            world: FloorInputs {
                scene: &floor_scene,
                now,
                conditions,
                pets: pixtuoid_scene::floor::PetInputs::default(),
            },
            theme,
            viewport: Viewport::full(lw, lh),
            gateway: pixtuoid_scene::tally::office_gateway(&scene),
            debug_walkable: false,
            dim: pixtuoid_scene::look::Dim::NONE,
        },
    );
    let picture = rendered
        .as_ref()
        .and_then(|rendered| rendered.picture(&per_floor.raster))
        .ok_or_else(|| anyhow!("{lw}x{lh} does not lay out"))?;
    let (buf, layout) = (picture.pixels, picture.hits.layout());
    let (bw, bh) = (buf.width(), buf.height());

    let mut img = RgbImage::new(u32::from(bw), u32::from(bh));
    for (i, px) in buf.as_slice().iter().enumerate() {
        let (x, y) = (i as u32 % u32::from(bw), i as u32 / u32::from(bw));
        img.put_pixel(x, y, ImgRgb([px.r, px.g, px.b]));
    }
    img.save(&out)
        .with_context(|| format!("writing {}", out.display()))?;
    eprintln!(
        "wrote {} ({bw}x{bh} = {lw}x{lh} logical @{scale_n}x, \
         {} desks, {} agents)",
        out.display(),
        layout.home_desks.len(),
        picture
            .hits
            .figures()
            .filter(|(target, _)| matches!(target, HoverTarget::Agent(_)))
            .count()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The day is read only beside an hour, so a day alone would render the
    /// default instant's sky, unasked.
    #[test]
    fn now_day_refuses_zero_and_wants_now_hour() {
        let parses = |flags: &[&str]| {
            CutawayArgs::try_parse_from([&["cutaway", "o.png"], flags].concat()).is_ok()
        };
        assert!(parses(&["--now-hour", "22", "--now-day", "1"]));
        assert!(!parses(&["--now-hour", "22", "--now-day", "0"]));
        assert!(!parses(&["--now-day", "5"]));
    }
}
