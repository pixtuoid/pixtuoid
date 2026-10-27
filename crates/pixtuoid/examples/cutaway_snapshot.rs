//! Render one frame of the CUTAWAY profile to a PNG, through the real seam: the
//! real sim produces a `SimFrame`, the real layout is computed at LOGICAL size,
//! and `render_cutaway` paints it into a buffer sized in pixels at `--scale`.
//!
//! Usage:
//!   cargo run --release --example cutaway_snapshot -- <out.png> [--scale N]
//!       [--agents N] [--theme T] [--logical WxH] [--now-hour H] [--floor I/N]
//!       [--weather W] [--now-day D] [--now-sec S] [--flame I] [--repos a,b,...]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow};
use image::{Rgb as ImgRgb, RgbImage};
use pixtuoid_core::state::{ActivityState, SceneState, ToolKind};
use pixtuoid_core::{AgentId, AgentSlot, GlobalDeskIndex};
use pixtuoid_scene::floor::{FloorMeta, FloorSession};
use pixtuoid_scene::layout::Size;
use pixtuoid_scene::look::{Look, Place, RenderInputs};
use pixtuoid_scene::render_scale::RenderScale;
use pixtuoid_scene::theme::theme_by_name;

/// Default logical office size — the SAME extent whatever `--scale` is, which
/// is the property the whole seam exists for: more pixels, not more desks.
/// `--logical WxH` overrides it; a bigger office is how the size-gated pieces
/// (corridor appliances) get placed at all.
const DEFAULT_LOGICAL: (u16, u16) = (160, 96);

/// The repos (cwd basenames) the fixture cycles through, unless `--repos`
/// names others. Fewer than the desk count on purpose: two agents sharing a
/// repo share an outfit, which is the grouping Team Palette exists to show.
const REPOS: &[&str] = &["pixtuoid", "site", "raycast", "notes"];

fn populate(scene: &mut SceneState, now: SystemTime, n: usize, repos: &[String]) {
    let seated = now.checked_sub(Duration::from_secs(120)).unwrap_or(now);
    let recent = now.checked_sub(Duration::from_secs(3)).unwrap_or(now);
    for i in 0..n {
        let id = AgentId::from_transcript_path(&format!("/cutaway/a{i}.jsonl"));
        let state = match i % 3 {
            0 => ActivityState::Active {
                tool_use_id: Some(format!("tu_{i}").into()),
                detail: Some("Edit: src/lib.rs".into()),
                kind: ToolKind::Edit,
            },
            1 => ActivityState::Idle,
            _ => ActivityState::Waiting {
                reason: "permission?".into(),
            },
        };
        scene.agents.insert(
            id,
            AgentSlot {
                agent_id: id,
                source: Arc::from("claude-code"),
                session_id: Arc::from(format!("cut-{i:04x}").as_str()),
                cwd: Arc::from(PathBuf::from(format!("/w/{}", repos[i % repos.len()])).as_path()),
                // The decoder's `cc·<cwd basename>`, so the badges show real text.
                label: format!("cc\u{b7}{}", repos[i % repos.len()]).into(),
                state,
                state_started_at: seated,
                created_at: seated,
                last_event_at: recent,
                exiting_at: None,
                desk_index: GlobalDeskIndex(i),
                floor_idx: scene.floor_of(GlobalDeskIndex(i)),
                tool_call_count: 0,
                active_ms: 0,
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

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .ok_or_else(|| anyhow!("usage: see the `//!` header of examples/cutaway_snapshot.rs"))?;

    let (mut scale_n, mut agents, mut theme_name) = (None, 10usize, "tokyo-night".to_string());
    let (mut now_hour, mut now_sec, mut floor) = (None::<u32>, 0u64, (0usize, 1usize));
    // 1 = the clock's base date, as the classic snapshot's `--now-day`.
    let mut now_day = 1u32;
    let mut weather = None::<String>;
    let mut flame = None::<usize>;
    let mut repos: Vec<String> = REPOS.iter().map(|&r| r.to_string()).collect();
    let (mut lw, mut lh) = DEFAULT_LOGICAL;
    let rest: Vec<String> = args.collect();
    let mut i = 0;
    while i < rest.len() {
        let val = |k: &str| -> Result<String> {
            rest.get(i + 1)
                .cloned()
                .ok_or_else(|| anyhow!("{k} needs a value"))
        };
        match rest[i].as_str() {
            "--scale" => scale_n = Some(val("--scale")?.parse().context("bad --scale")?),
            "--agents" => agents = val("--agents")?.parse().context("bad --agents")?,
            "--theme" => theme_name = val("--theme")?,
            "--logical" => {
                let v = val("--logical")?;
                let (w, h) = v
                    .split_once('x')
                    .ok_or_else(|| anyhow!("--logical wants WxH"))?;
                lw = w.parse().context("bad --logical width")?;
                lh = h.parse().context("bad --logical height")?;
            }
            "--now-hour" => now_hour = Some(val("--now-hour")?.parse().context("bad --now-hour")?),
            "--now-day" => now_day = val("--now-day")?.parse().context("bad --now-day")?,
            // Seconds past `--now-hour`, fine enough to sample a weather
            // transition.
            "--now-sec" => now_sec = val("--now-sec")?.parse().context("bad --now-sec")?,
            "--weather" => weather = Some(val("--weather")?),
            "--repos" => repos = val("--repos")?.split(',').map(str::to_string).collect(),
            // The `I`th agent burns at the Top tier, crowned in flame.
            "--flame" => flame = Some(val("--flame")?.parse().context("bad --flame")?),
            "--floor" => {
                let v = val("--floor")?;
                let (f, n) = v
                    .split_once('/')
                    .ok_or_else(|| anyhow!("--floor wants I/N"))?;
                floor = (
                    f.parse().context("bad --floor index")?,
                    n.parse().context("bad --floor count")?,
                );
            }
            other => return Err(anyhow!("unexpected arg: {other}")),
        }
        i += 2;
    }
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
        Some(h) => pixtuoid_scene::localclock::try_on_day(now_day.saturating_sub(1), h)
            .with_context(|| format!("invalid --now-day/--now-hour {now_day}:{h}"))?,
        None => SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    };
    if now_sec >= 3600 || (now_sec > 0 && now_hour.is_none()) {
        return Err(anyhow!("--now-sec wants 0..3600 and a --now-hour"));
    }
    let now = now + Duration::from_secs(now_sec);
    let meta = FloorMeta::for_floor(floor.0, floor.1).with_weather(policy);

    let mut scene = SceneState::uniform(64);
    populate(&mut scene, now, agents, &repos);
    if let Some(i) = flame {
        let id = AgentId::from_transcript_path(&format!("/cutaway/a{i}.jsonl"));
        let a = scene
            .agents
            .get_mut(&id)
            .ok_or_else(|| anyhow!("--flame {i}: only {agents} agents"))?;
        a.model = Some("claude-fable-5".into());
        a.effort = Some(pixtuoid_core::state::EffortObservation::new(
            "ultra".into(),
            now,
        ));
    }

    // The real sim, at LOGICAL size, drawn through the painters' one entry.
    let mut session = FloorSession::new(Arc::clone(&pack));
    session.render(
        Look::Cutaway {
            scale,
            text: pixtuoid_scene::look::WorldText::Baked,
        },
        RenderInputs {
            world: pixtuoid_scene::floor::FloorInputs {
                scene: &scene,
                now,
                floor: meta,
                pets: pixtuoid_scene::floor::PetInputs::default(),
            },
            theme,
            size: Size { w: lw, h: lh },
            place: Place {
                gateway: pixtuoid_scene::tally::office_gateway(&scene),
                floor: None,
            },
            debug_walkable: false,
            dim: pixtuoid_scene::look::Dim::NONE,
        },
    );
    let picture = session
        .picture()
        .ok_or_else(|| anyhow!("{lw}x{lh} does not lay out"))?;
    let (buf, layout) = (picture.pixels, picture.hits.layout());
    let (bw, bh) = (buf.width(), buf.height());

    let mut img = RgbImage::new(u32::from(bw), u32::from(bh));
    for (i, px) in buf.as_slice().iter().enumerate() {
        let (x, y) = (i as u32 % u32::from(bw), i as u32 / u32::from(bw));
        img.put_pixel(x, y, ImgRgb([px.r, px.g, px.b]));
    }
    img.save(&out).with_context(|| format!("writing {out}"))?;
    eprintln!(
        "wrote {out} ({bw}x{bh} = {lw}x{lh} logical @{scale_n}x, \
         {} desks, {} agents)",
        layout.home_desks.len(),
        scene.agents.len()
    );
    Ok(())
}
