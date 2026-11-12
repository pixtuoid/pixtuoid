//! Render ONE frame of the `pixtuoid floating` office to a PNG — visual verification for
//! the floating window. It drives the SAME `window_geometry`, `OfficeRenderer`,
//! overlay layers and `composition` the live window presents, drawn by the CPU
//! compositor the window's GPU frames are held to.
//!
//! Usage: `cargo run --release --example floating_snapshot -- <out.png> [WxH] [OPTIONS]`,
//! e.g. `... -- /tmp/f.png --agents 6` (`config::FLOATING_DEFAULT_{W,H}` × `RETINA_SCALE_FACTOR`),
//! `... -- /tmp/f.png 960x640`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use image::{Rgb as ImgRgb, RgbImage};
use pixtuoid::dev::{OfficeRenderer, WindowFrame, XrgbSurface, window_geometry};
use pixtuoid_core::state::{ActivityState, SceneState, ToolKind, ToolUse};
use pixtuoid_core::{AgentId, AgentSlot, FloorDesk};
use pixtuoid_scene::cutaway::chrome_cell;
use pixtuoid_scene::floor::{Conditions, FloorInputs, PetInputs};
use pixtuoid_scene::theme::theme_by_name;

/// The two `cc` labels are a DELIBERATE collision, so the snapshot exercises the
/// `·xxxx` suffix as well as every label tone.
fn populate_demo_agents(scene: &mut SceneState, now: SystemTime, n: usize) {
    let archetypes: [(&str, ActivityState); 6] = [
        (
            "claude-code",
            ActivityState::Running {
                item: Some(ToolUse {
                    tool_use_id: Some("tu_a".into()),
                    detail: Some("Write: src/foo.rs".into()),
                    kind: ToolKind::Edit,
                }),
            },
        ),
        ("codex", ActivityState::Idle),
        (
            "cc",
            ActivityState::RequiresAction {
                reason: Some("permission?".into()),
            },
        ),
        (
            "cc",
            ActivityState::Running {
                item: Some(ToolUse {
                    tool_use_id: Some("tu_d".into()),
                    detail: Some("Bash: cargo test".into()),
                    kind: ToolKind::Bash,
                }),
            },
        ),
        ("reasonix", ActivityState::Idle),
        (
            "opencode",
            ActivityState::Running {
                item: Some(ToolUse {
                    tool_use_id: Some("tu_e".into()),
                    detail: Some("Grep: TODO".into()),
                    kind: ToolKind::Search,
                }),
            },
        ),
    ];
    // Back-dated past the entry animation with a recent `last_event_at`, so every agent is
    // SEATED and spread out rather than clustered walking in from the elevator, which
    // overlaps their badges.
    let seated_since = now.checked_sub(Duration::from_secs(120)).unwrap_or(now);
    let recent = now.checked_sub(Duration::from_secs(3)).unwrap_or(now);
    for i in 0..n {
        let (label, state) = &archetypes[i % archetypes.len()];
        let key = format!("{label}-{i}");
        let id = AgentId::from_transcript_path(&format!("/demo/{key}.jsonl"));
        scene.agents.insert(
            id,
            AgentSlot {
                agent_id: id,
                source: Arc::from("claude-code"),
                session_id: Arc::from(format!("demo-{key}-{i:04x}").as_str()),
                cwd: Arc::from(PathBuf::from("/demo").as_path()),
                label: (*label).into(),
                state: state.clone(),
                state_started_at: seated_since,
                created_at: seated_since,
                arrived_at: Some(seated_since),
                last_event_at: recent,
                exiting_at: None,
                desk: Some(FloorDesk::new(0, i)),
                tool_call_count: 0,
                running_ms: 0,
                unknown_cwd: false,
                parent_id: None,
                pid: None,
                model: None,
                effort: None,
                tokens_used: 0,
                last_usage: None,
            },
        );
    }
}

/// A window point, in px.
type Point = (f64, f64);

#[derive(Debug, Parser)]
struct FloatingArgs {
    out: PathBuf,
    #[arg(value_name = "WxH", value_parser = pixtuoid::dev::parse_size)]
    size: Option<(u16, u16)>,
    #[arg(long, default_value = "normal")]
    theme: String,
    #[arg(long, default_value_t = 0)]
    agents: usize,
    #[arg(long, value_name = "X,Y", value_parser = parse_point)]
    hover: Option<Point>,
    #[arg(long, value_name = "KIND", value_parser = pixtuoid::dev::pet_parser())]
    pet: Option<pixtuoid_scene::pet::PetKind>,
    /// Press at the first window point and carry what it lifts to the second,
    /// drawing it in hand.
    #[arg(long, value_name = "X0,Y0:X1,Y1", value_parser = parse_drag)]
    drag: Option<(Point, Point)>,
    /// Release the drag there and draw MS later.
    #[arg(long, value_name = "MS", requires = "drag")]
    drop_after: Option<u64>,
    /// Open that panel over the office, as its key does.
    #[arg(long, value_name = "NAME", value_parser = clap::builder::PossibleValuesParser::new(pixtuoid::dev::panel_names()))]
    panel: Option<String>,
    /// Zoom the office N density steps from the automatic scale, as the zoom
    /// keys do.
    #[arg(
        long,
        value_name = "N",
        default_value_t = 0,
        allow_negative_numbers = true
    )]
    zoom: i8,
}

fn parse_point(s: &str) -> Result<Point, String> {
    pixtuoid::dev::parse_pair(s, ',', "X,Y window px")
}

fn parse_drag(s: &str) -> Result<(Point, Point), String> {
    let (from, to) = s
        .split_once(':')
        .ok_or_else(|| format!("{s:?}: want X0,Y0:X1,Y1"))?;
    Ok((parse_point(from)?, parse_point(to)?))
}

fn main() -> Result<()> {
    let FloatingArgs {
        out,
        size,
        theme: theme_name,
        agents: n_agents,
        hover,
        pet,
        drag,
        drop_after,
        panel,
        zoom,
    } = FloatingArgs::parse();
    const RETINA_SCALE_FACTOR: u32 = 2;
    let size = match size {
        Some(size) => size,
        None => (
            u16::try_from(pixtuoid::dev::FLOATING_DEFAULT_W * RETINA_SCALE_FACTOR)?,
            u16::try_from(pixtuoid::dev::FLOATING_DEFAULT_H * RETINA_SCALE_FACTOR)?,
        ),
    };
    let zoom = pixtuoid::dev::Zoom::new(zoom);

    let theme =
        theme_by_name(&theme_name).ok_or_else(|| anyhow!("unknown --theme {theme_name:?}"))?;
    let pack = std::sync::Arc::new(pixtuoid_scene::pack::load_bundled_pack()?);
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    let (win_w, win_h) = (u32::from(size.0), u32::from(size.1));
    let at = window_geometry(
        winit::dpi::PhysicalSize::new(win_w, win_h),
        pack.max_density_variant(),
        zoom,
    );
    let office = at.logical();
    let desks = pixtuoid::dev::floor_caps_for_buffer(office.w, office.h);
    let mut scene = SceneState::new(desks);
    populate_demo_agents(&mut scene, now, n_agents);
    // As the app takes a layout: an agent it has no desk for moves to a free one.
    scene.set_capacities(desks, now);
    let mut renderer = OfficeRenderer::new(std::sync::Arc::clone(&pack));
    renderer.set_pets(
        pet.map(pixtuoid_scene::pet::Pet::defaulted)
            .into_iter()
            .collect(),
    );
    // Shown as the window shows a frame, so a press finds what it shows.
    let render = |renderer: &mut OfficeRenderer, now| {
        let live = renderer.render_live(
            at,
            WindowFrame {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    conditions: Conditions::default(),
                    pets: PetInputs::default(),
                },
                theme,
                gateway: pixtuoid_scene::tally::office_gateway(&scene),
                dim: pixtuoid_scene::look::Dim::NONE,
            },
            (win_w, win_h),
        );
        if live {
            renderer.presented();
        }
    };
    render(&mut renderer, now);
    let mut now = now;
    if let Some((from, to)) = drag {
        let window = (win_w, win_h);
        renderer.press_at(
            from,
            window,
            pixtuoid::dev::Pressing {
                scale_factor: 1.0,
                now,
            },
        );
        if !renderer.pointer_moved(to) {
            return Err(anyhow!("--drag lifted nothing at {from:?}"));
        }
        now += Duration::from_millis(100);
        render(&mut renderer, now);
        if let Some(ms) = drop_after {
            renderer.release(to);
            // The drop lands on the frame after; the walk then runs `ms`.
            now += Duration::from_millis(100);
            render(&mut renderer, now);
            let step = Duration::from_millis(100);
            let end = now + Duration::from_millis(ms);
            while now < end {
                now = (now + step).min(end);
                render(&mut renderer, now);
            }
        }
    }
    let buf = renderer.buf().context("a frame")?;
    let (bw, bh) = (buf.width(), buf.height());
    let cell = chrome_cell(at);
    let next = pixtuoid::dev::Overlays {
        window: (win_w, win_h),
        // Audible so the ♩ suffix shows; no transient flash in a static snapshot.
        footer: renderer.footer(
            &scene,
            pixtuoid::dev::footer_budget(win_w as usize, cell),
            true,
            None,
            None,
        ),
        tooltip: hover.and_then(|cursor| {
            let tip = renderer.drawn_hit_at(cursor, at).and_then(|hit| {
                pixtuoid_scene::tooltip::for_hit(hit, &scene, now, PetInputs::default())
            })?;
            Some((tip, (cursor.0 as i32, cursor.1 as i32)))
        }),
        panels: panel.and_then(|name| {
            pixtuoid::dev::panel_preview(&name, &scene, theme, ((win_w, win_h), cell), now)
        }),
    };
    let mut overlays = pixtuoid::dev::OverlayLayers::default();
    overlays.update(next, at, (theme, &pack));
    let (ww, wh) = (win_w as usize, win_h as usize);
    let mut sb: Vec<u32> = vec![0; ww * wh];
    pixtuoid::dev::composite(
        &pixtuoid::dev::composition(
            buf,
            pixtuoid::dev::Change::All,
            at,
            (win_w, win_h),
            &overlays,
        ),
        &mut XrgbSurface::new(&mut sb, ww, wh).expect("sized to the window"),
    );

    let mut img = RgbImage::new(win_w, win_h);
    for wy in 0..win_h {
        for wx in 0..win_w {
            let px = sb[wy as usize * ww + wx as usize];
            img.put_pixel(
                wx,
                wy,
                ImgRgb([(px >> 16) as u8, (px >> 8) as u8, px as u8]),
            );
        }
    }
    img.save(&out)
        .with_context(|| format!("writing {}", out.display()))?;
    eprintln!(
        "wrote {} ({win_w}x{win_h}, office buffer {bw}x{bh} upscaled {}x, {n_agents} agents)",
        out.display(),
        at.upscale()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(flags: &[&str]) -> Result<FloatingArgs, clap::Error> {
        FloatingArgs::try_parse_from([&["floating_snapshot", "o.png"], flags].concat())
    }

    #[test]
    fn no_flags_parse_as_the_defaults() {
        let args = parse(&[]).unwrap();
        assert_eq!((args.size, args.agents, args.zoom), (None, 0, 0));
        assert_eq!((args.hover, args.drag, args.drop_after), (None, None, None));
        assert_eq!(
            (args.pet, args.panel, args.theme.as_str()),
            (None, None, "normal")
        );
    }

    /// A zoom out is a negative step, which clap would otherwise read as a flag.
    #[test]
    fn a_negative_zoom_parses() {
        assert_eq!(parse(&["--zoom", "-1"]).unwrap().zoom, -1);
    }

    /// A drop is read only after a drag, so one alone would draw no drop, unasked.
    #[test]
    fn a_drop_needs_a_drag_and_a_drag_two_points() {
        let dragged = parse(&["--drag", "1,2:3,4", "--drop-after", "300"]).unwrap();
        assert_eq!(dragged.drag, Some(((1.0, 2.0), (3.0, 4.0))));
        assert_eq!(dragged.drop_after, Some(300));
        assert!(parse(&["--drop-after", "300"]).is_err());
        assert!(parse(&["--drag", "1,2"]).is_err());
    }

    #[test]
    fn a_malformed_hover_point_is_refused() {
        assert_eq!(parse(&["--hover", "1,2"]).unwrap().hover, Some((1.0, 2.0)));
        for bad in ["1,y", "1;2"] {
            assert!(parse(&["--hover", bad]).is_err(), "{bad}");
        }
    }

    /// The names are the lib's, so clap refuses one it would draw no panel for.
    #[test]
    fn a_panel_must_be_one_the_lib_lists() {
        for name in pixtuoid::dev::panel_names() {
            assert_eq!(
                parse(&["--panel", name]).unwrap().panel.as_deref(),
                Some(name)
            );
        }
        assert!(parse(&["--panel", "bogus"]).is_err());
    }
}
