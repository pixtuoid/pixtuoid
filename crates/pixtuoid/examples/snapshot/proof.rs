//! `--proof`: the §3 split-screen causal-proof renderer. ONE committed CC session
//! (`proof-session/`) drives BOTH sides of every frame: the left panel types the session,
//! the right side is the REAL draw_scene pass replaying the SAME decoded
//! AgentEvent stream through the real Reducer — the two sides structurally cannot
//! desync. scripts/gen-media.py (kind:"proof") encodes the frames.

use anyhow::{Context as _, Result, anyhow};
use image::{Rgba, RgbaImage};
use pixtuoid::dev::{DrawCtx, draw_scene};
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::source::claude_code::{
    SOURCE_NAME, cc_derive_label, cc_id_from_path, decode_cc_line,
};
use pixtuoid_core::{AgentId, Reducer, SceneState, Transport};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::collections::VecDeque;
use std::fs;
use std::path::Path;

use crate::encode::{FrameSink, ImageCanvas, Timeline, cells_to_rgba, fill_rect, terminal_cell};
use crate::{CELL_H, CELL_W};
use pixtuoid_scene::cutaway::{GridInk, paint_grid};
use pixtuoid_scene::display::cells::CellGrid;
use pixtuoid_scene::display::text::cells;

// Geometry (px); every canvas dim must stay even so yuv420p never crops.
// PANEL_W targets the pinned mock's ~44/56 typed-panel/office split against the
// reference render (--cols 120 --rows 52 -> office_w = 960px).
const PANEL_W: u32 = 760;
const TALL_PANEL_H: u32 = 400;
const HEADER_H: u32 = 32;
const PAD: u32 = 16;
const LINE_H: u32 = 28;
const TYPE_CPS: u64 = 30;
const PREAMBLE_MS: u64 = 6000; // "$ claude" + session start precede the session's first line

// Every callout holds at most this long from its OWN at_ms, independent of when
// the next annotated line appears — without a ceiling, the first one held for the
// whole gap to the next annotation (it predates the preamble that pads every
// later beat). Tuned to sit just above the other transitions' natural gaps, so
// only the outlier is shortened.
const ANNOTATION_MAX_HOLD_MS: u64 = 4200;

const CODA_LINE_H: u32 = 18;
const CODA_PAD: u32 = 10;
// The "captured"/"happened in a terminal" framing (here + the panel title below)
// is DELIBERATE: the timeline is authored — the statusline-ticker disjointness pin
// REQUIRES authored strings — and the load-bearing half of the claim is engine
// truth (the right pane replays through the real decode_cc_line → Reducer).
const CODA_TEXT: &str = "the left pane happened in a terminal. the right pane is the same \
event stream, drawn by the same engine -- nothing is mocked.";

// Burned panel palette — theme-independent (the office side carries the theme).
const PANEL_BG: Rgba<u8> = Rgba([13, 15, 19, 255]);
const CHROME_BG: Rgba<u8> = Rgba([24, 27, 33, 255]);
const EDGE: Rgba<u8> = Rgba([70, 74, 84, 255]);
const INK: Rgba<u8> = Rgba([214, 214, 208, 255]);
const PROMPT: Rgba<u8> = Rgba([139, 196, 138, 255]);
// coral — the pinned connector/annotation color, sampled from the approved mock.
const ANNOT: Rgba<u8> = Rgba([224, 122, 85, 255]);
const CODA_BG: Rgba<u8> = Rgba([10, 9, 8, 255]);
const CODA_INK: Rgba<u8> = Rgba([150, 145, 135, 255]);

pub(crate) enum ProofLayout {
    Wide,
    Tall,
}

pub(crate) struct PanelLine {
    pub(crate) at_ms: u64,
    pub(crate) text: String,
    pub(crate) prompt: bool,
    /// Burned office-side callout, lit while this line is the newest annotated one.
    pub(crate) annotation: Option<&'static str>,
}

pub(crate) struct ProofScript {
    pub(crate) events: Vec<(u64, AgentEvent)>,
    pub(crate) lines: Vec<PanelLine>,
    /// From the session's first timestamp — the panel titles itself as a
    /// past-tense archive, not the live ticker.
    pub(crate) capture_date: String,
}

/// Greedy word-wrap of `text` to fit within `max_width` px, measured via
/// `width_fn`. A single over-long word is kept whole rather than looping forever;
/// never returns an empty vec.
fn wrap_text(text: &str, max_width: i32, width_fn: impl Fn(&str) -> i32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split(' ') {
        let candidate = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if cur.is_empty() || width_fn(&candidate) <= max_width {
            cur = candidate;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(text.to_string());
    }
    lines
}

/// `s`'s width in pixels in [`terminal_cell`]s.
fn text_width(s: &str) -> i32 {
    i32::from(cells(s)) * i32::from(terminal_cell().w)
}

/// Draw `s` in the window's face, top-left at `(x, top_y)`, over what is
/// there, with an optional one-pixel `halo` under each glyph. Returns its
/// width so the typing-cursor block needn't recompute it.
fn draw_text(
    img: &mut RgbaImage,
    s: &str,
    (x, top_y): (i32, i32),
    color: Rgba<u8>,
    halo: Option<Rgba<u8>>,
) -> i32 {
    let rgb = |c: Rgba<u8>| pixtuoid_core::sprite::Rgb {
        r: c[0],
        g: c[1],
        b: c[2],
    };
    let mut grid = CellGrid::new(cells(s), 1);
    grid.put((0, 0), s, None, false);
    paint_grid(
        &mut ImageCanvas(img),
        &grid,
        ((x, top_y), terminal_cell()),
        crate::icons(),
        GridInk {
            text: rgb(color),
            halo: halo.map(rgb),
            shadow: None,
        },
    );
    text_width(s)
}

fn coda_lines(canvas_w: u32) -> Vec<String> {
    let floor = text_width("M");
    let max_w = (canvas_w as i32 - 2 * CODA_PAD as i32).max(floor);
    wrap_text(CODA_TEXT, max_w, text_width)
}

fn coda_height(canvas_w: u32) -> u32 {
    let n = coda_lines(canvas_w).len() as u32;
    2 * CODA_PAD + n * CODA_LINE_H
}

pub(crate) fn canvas_dims(layout: &ProofLayout, office_w: u32, office_h: u32) -> (u32, u32) {
    match layout {
        ProofLayout::Wide => {
            let w = PANEL_W + office_w;
            (w, HEADER_H + office_h + coda_height(w))
        }
        ProofLayout::Tall => {
            let h = HEADER_H + TALL_PANEL_H + HEADER_H + office_h;
            (office_w, h + coda_height(office_w))
        }
    }
}

pub(crate) fn revealed_chars(at_ms: u64, elapsed_ms: u64, len: usize) -> usize {
    if elapsed_ms < at_ms {
        return 0;
    }
    (((elapsed_ms - at_ms) * TYPE_CPS) / 1000).min(len as u64) as usize
}

/// The newest annotated line already on screen — its connector + callout are lit
/// for at most `ANNOTATION_MAX_HOLD_MS` past its OWN `at_ms`, UNLESS it's the last
/// annotated line in the script, which holds indefinitely: the cap exists to stop
/// a callout over-holding while it waits on a FUTURE beat, and the final beat has
/// none, so capping it would just go quiet through the clip's idle tail. An
/// expired callout returns `None` rather than resurrecting an OLDER one.
pub(crate) fn active_annotation(lines: &[PanelLine], elapsed_ms: u64) -> Option<usize> {
    let (i, line) = lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| l.annotation.is_some() && l.at_ms <= elapsed_ms)?;
    let is_final_annotation = lines[i + 1..].iter().all(|l| l.annotation.is_none());
    (is_final_annotation || elapsed_ms - line.at_ms <= ANNOTATION_MAX_HOLD_MS).then_some(i)
}

fn ts_ms(v: &serde_json::Value) -> Result<i64> {
    let ts = v
        .get("timestamp")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("session line missing timestamp"))?;
    Ok(chrono::DateTime::parse_from_rfc3339(ts)
        .with_context(|| format!("bad session timestamp {ts:?}"))?
        .timestamp_millis())
}

/// The session's date (`YYYY-MM-DD`), from its first line's timestamp.
fn capture_date_str(v: &serde_json::Value) -> Result<String> {
    let ts = v
        .get("timestamp")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("session line missing timestamp"))?;
    Ok(chrono::DateTime::parse_from_rfc3339(ts)
        .with_context(|| format!("bad session timestamp {ts:?}"))?
        .format("%Y-%m-%d")
        .to_string())
}

/// First human-meaningful arg of a tool_use input, for the panel line.
fn tool_arg(input: Option<&serde_json::Value>) -> String {
    let Some(obj) = input.and_then(|i| i.as_object()) else {
        return String::new();
    };
    for key in ["file_path", "command", "pattern", "path"] {
        if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

pub(crate) fn build_script(session: &Path) -> Result<ProofScript> {
    let raw = fs::read_to_string(session)
        .with_context(|| format!("read proof session {}", session.display()))?;
    let stem = cc_id_from_path(session);
    anyhow::ensure!(!stem.is_empty(), "session path has no filename stem");
    let agent_id = AgentId::from_parts(SOURCE_NAME, &stem);
    let path_str = session.to_string_lossy().into_owned();

    let parsed: Vec<serde_json::Value> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .context("proof session is not valid JSONL")?;
    let first = parsed
        .first()
        .ok_or_else(|| anyhow!("empty proof session"))?;
    let t0 = ts_ms(first)? - PREAMBLE_MS as i64;
    let capture_date = capture_date_str(first)?;
    let cwd = first
        .get("cwd")
        .and_then(|s| s.as_str())
        .unwrap_or("/")
        .to_string();

    let mut events: Vec<(u64, AgentEvent)> = Vec::new();
    let mut lines: Vec<PanelLine> = Vec::new();
    lines.push(PanelLine {
        at_ms: 0,
        text: "$ claude".into(),
        prompt: true,
        annotation: None,
    });
    lines.push(PanelLine {
        at_ms: 800,
        text: "* session started".into(),
        prompt: false,
        annotation: Some("a sprite walks in"),
    });
    // Registration is the WATCHER's job in production, not the decoder's, so the
    // render synthesizes it once.
    events.push((
        800,
        AgentEvent::SessionStart {
            agent_id,
            source: SOURCE_NAME.to_string(),
            session_id: stem.clone(),
            cwd: cwd.clone().into(),
            parent_id: None,
        },
    ));
    events.push((
        800,
        AgentEvent::Rename {
            agent_id,
            label: cc_derive_label(session, SOURCE_NAME, Path::new(&cwd)),
        },
    ));

    let mut tool_idx = 0usize;
    let mut last_ms = 800u64;
    for v in &parsed {
        let rel = (ts_ms(v)? - t0).max(0) as u64;
        last_ms = last_ms.max(rel);
        let ty = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
        let content = v.get("message").and_then(|m| m.get("content"));
        if ty == "user"
            && let Some(text) = content.and_then(|c| c.as_str())
        {
            lines.push(PanelLine {
                at_ms: rel,
                text: format!("> {text}"),
                prompt: true,
                annotation: None,
            });
        }
        if ty == "assistant"
            && let Some(blocks) = content.and_then(|c| c.as_array())
        {
            for b in blocks {
                if b.get("type").and_then(|s| s.as_str()) == Some("tool_use") {
                    let name = b.get("name").and_then(|s| s.as_str()).unwrap_or("?");
                    let annotation = Some(match tool_idx {
                        0 => "-- that's you",
                        1 => "monitor flips",
                        _ => "monitor flips again",
                    });
                    tool_idx += 1;
                    lines.push(PanelLine {
                        at_ms: rel,
                        text: format!("[{name}] {}", tool_arg(b.get("input"))),
                        prompt: false,
                        annotation,
                    });
                }
            }
        }
        for ev in decode_cc_line(&path_str, SOURCE_NAME, v.clone()) {
            events.push((rel, ev));
        }
    }
    lines.push(PanelLine {
        at_ms: last_ms + 600,
        text: "ok - done".into(),
        prompt: false,
        annotation: Some("back to idle"),
    });
    events.sort_by_key(|(at, _)| *at);
    Ok(ProofScript {
        events,
        lines,
        capture_date,
    })
}

fn put(img: &mut RgbaImage, x: i32, y: i32, c: Rgba<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, c);
    }
}

/// A filled disc of radius `r` centred on `(cx, cy)`.
fn dot(img: &mut RgbaImage, (cx, cy): (i32, i32), r: i32, c: Rgba<u8>) {
    for y in -r..=r {
        for x in -r..=r {
            if x * x + y * y <= r * r {
                put(img, cx + x, cy + y, c);
            }
        }
    }
}

/// The top of a [`terminal_cell`] centred in a band `h` tall from `y`.
fn centred_in(y: u32, h: u32) -> i32 {
    (y + (h - u32::from(terminal_cell().h)) / 2) as i32
}

fn dashed_h(img: &mut RgbaImage, x0: i32, x1: i32, y: i32, c: Rgba<u8>) {
    for x in x0..x1 {
        if (x - x0) / 4 % 2 == 0 {
            put(img, x, y, c);
            put(img, x, y + 1, c);
        }
    }
}

// Mac-style traffic-light dots — the pinned mock's terminal chrome carries them
// so the left panel reads as a window, not a bare text box.
const DOT_RED: Rgba<u8> = Rgba([255, 95, 86, 255]);
const DOT_YELLOW: Rgba<u8> = Rgba([255, 189, 46, 255]);
const DOT_GREEN: Rgba<u8> = Rgba([39, 201, 63, 255]);
const CHROME_DOT_R: i32 = 4;
/// The annotation's anchor dot at the desk.
const ANNOT_DOT_R: i32 = 4;
// Centre to centre: a pixel's gap between the dots.
const DOT_PITCH: i32 = 2 * CHROME_DOT_R + 2;
const DOT_GAP_AFTER: i32 = 6;

/// `is_panel` gates the traffic-light dots — only the left panel is a typed
/// terminal window.
fn chrome(img: &mut RgbaImage, x: u32, y: u32, w: u32, title: &str, is_panel: bool) {
    fill_rect(img, x, y, w, HEADER_H, CHROME_BG);
    fill_rect(img, x, y + HEADER_H - 1, w, 1, EDGE);
    let top = centred_in(y, HEADER_H);
    if is_panel {
        let cy = (y + HEADER_H / 2) as i32;
        let mut cx = x as i32 + PAD as i32 + CHROME_DOT_R;
        for c in [DOT_RED, DOT_YELLOW, DOT_GREEN] {
            dot(img, (cx, cy), CHROME_DOT_R, c);
            cx += DOT_PITCH;
        }
        let title_x = cx - CHROME_DOT_R + DOT_GAP_AFTER;
        draw_text(img, title, (title_x, top), INK, None);
    } else {
        draw_text(img, title, ((x + PAD) as i32, top), INK, None);
    }
}

fn panel_body(
    img: &mut RgbaImage,
    origin: (u32, u32),
    size: (u32, u32),
    script: &ProofScript,
    elapsed_ms: u64,
) {
    fill_rect(img, origin.0, origin.1, size.0, size.1, PANEL_BG);
    let floor = text_width("M");
    let max_w = (size.0 as i32 - 2 * PAD as i32).max(floor);
    let mut row = 0u32;
    for line in &script.lines {
        let total_len = line.text.chars().count();
        let shown = revealed_chars(line.at_ms, elapsed_ms, total_len);
        if shown == 0 && line.at_ms > elapsed_ms {
            continue;
        }
        // Wrapped purely at render time: the typewriter reveal walks the FLAT
        // string's character stream, so a long line pushes later lines down as
        // more of it becomes visible, like a real terminal.
        let wrapped = wrap_text(&line.text, max_w, text_width);
        let color = if line.prompt { PROMPT } else { INK };
        let mut remaining = shown;
        for sub in &wrapped {
            if remaining == 0 {
                break;
            }
            let sub_len = sub.chars().count();
            let take = remaining.min(sub_len);
            let y = origin.1 + PAD + row * LINE_H;
            if y + LINE_H > origin.1 + size.1 {
                return; // panel full — the timeline is authored to fit; guard anyway
            }
            let visible: String = sub.chars().take(take).collect();
            let top = centred_in(y, LINE_H);
            let advance = draw_text(img, &visible, ((origin.0 + PAD) as i32, top), color, None);
            if take < sub_len {
                let cx = origin.0 as i32 + PAD as i32 + advance;
                let cell = terminal_cell();
                fill_rect(
                    img,
                    cx.max(0) as u32,
                    top as u32,
                    u32::from(cell.w),
                    u32::from(cell.h),
                    INK,
                );
            }
            row += 1;
            remaining -= take;
        }
    }
}

pub(crate) fn compose_frame(
    layout: &ProofLayout,
    office: &RgbaImage,
    script: &ProofScript,
    elapsed_ms: u64,
    desk_px: (u32, u32),
) -> RgbaImage {
    let (ow, oh) = (office.width(), office.height());
    let (w, h) = canvas_dims(layout, ow, oh);
    let mut img = RgbaImage::from_pixel(w, h, PANEL_BG);
    let panel_title = format!("~ captured claude code session · {}", script.capture_date);
    let (panel_origin, panel_size, office_origin) = match layout {
        ProofLayout::Wide => {
            chrome(&mut img, 0, 0, PANEL_W, &panel_title, true);
            chrome(&mut img, PANEL_W, 0, ow, "pixtuoid", false);
            ((0, HEADER_H), (PANEL_W, oh), (PANEL_W, HEADER_H))
        }
        ProofLayout::Tall => {
            chrome(&mut img, 0, 0, ow, &panel_title, true);
            chrome(&mut img, 0, HEADER_H + TALL_PANEL_H, ow, "pixtuoid", false);
            (
                (0, HEADER_H),
                (ow, TALL_PANEL_H),
                (0, HEADER_H + TALL_PANEL_H + HEADER_H),
            )
        }
    };
    panel_body(&mut img, panel_origin, panel_size, script, elapsed_ms);
    image::imageops::overlay(
        &mut img,
        office,
        i64::from(office_origin.0),
        i64::from(office_origin.1),
    );
    // Divider between the halves; the coda strip, drawn last, trims its own bottom
    // slice back off.
    match layout {
        ProofLayout::Wide => fill_rect(&mut img, PANEL_W - 1, 0, 2, HEADER_H + oh, EDGE),
        ProofLayout::Tall => fill_rect(&mut img, 0, HEADER_H + TALL_PANEL_H, w, 1, EDGE),
    }

    // Anchored to the ACTUAL working sprite's desk — no hand-placed coordinates.
    if let Some(i) = active_annotation(&script.lines, elapsed_ms)
        && let Some(label) = script.lines[i].annotation
    {
        let desk = (
            (office_origin.0 + desk_px.0) as i32,
            (office_origin.1 + desk_px.1) as i32,
        );
        // Clears the halo `paint_ceiling_halos` burns over a lit monitor, with a
        // margin, so the connector/dot never sits inside the glow.
        const GLOW_CLEARANCE: i32 = 24;
        let anchor_y = desk.1 - GLOW_CLEARANCE;
        let left_edge = match layout {
            ProofLayout::Wide => {
                dashed_h(
                    &mut img,
                    (PANEL_W - PAD) as i32,
                    desk.0 - 10,
                    anchor_y,
                    ANNOT,
                );
                (PANEL_W + PAD) as i32
            }
            // No cross-panel connector line — the panel sits above, not beside.
            ProofLayout::Tall => PAD as i32,
        };
        let label_x = (desk.0 - text_width(label) - 16).max(left_edge);
        // Its halo keeps it legible over the office it is drawn on.
        let shade = Some(Rgba([0, 0, 0, 255]));
        draw_text(&mut img, label, (label_x, anchor_y - 22), ANNOT, shade);
        dot(&mut img, (desk.0 - 6, anchor_y), ANNOT_DOT_R, ANNOT);
    }

    let ch = coda_height(w);
    let coda_y0 = h - ch;
    fill_rect(&mut img, 0, coda_y0, w, ch, CODA_BG);
    fill_rect(&mut img, 0, coda_y0, w, 1, EDGE);
    for (i, cline) in coda_lines(w).iter().enumerate() {
        let x = ((w as i32 - text_width(cline)) / 2).max(0);
        let y = (coda_y0 + CODA_PAD) as i32 + i as i32 * CODA_LINE_H as i32;
        draw_text(&mut img, cline, (x, y), CODA_INK, None);
    }
    img
}

pub(crate) struct ProofJob<'a> {
    pub(crate) session: &'a Path,
    pub(crate) frames_dir: &'a Path,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) timeline: Timeline,
    pub(crate) max_desks: usize,
    pub(crate) theme: &'static pixtuoid_scene::theme::Theme,
    pub(crate) pack: &'a std::sync::Arc<pixtuoid_scene::pack::OfficeArt>,
    pub(crate) weather: pixtuoid_scene::sky::WeatherPolicy,
}

pub(crate) fn render_proof(job: &ProofJob) -> Result<()> {
    let script = build_script(job.session)?;
    let mut pending: VecDeque<(u64, AgentEvent)> = script.events.iter().cloned().collect();

    // Anchor the burned callout to home_desks[0] in the SAME layout draw_scene
    // computes.
    let (buf_w, buf_h) = pixtuoid::dev::scene_buf_size(job.cols, job.rows);
    let layout = pixtuoid_scene::layout::SceneLayout::compute_with_seed(
        buf_w,
        buf_h,
        Some(job.max_desks),
        0,
    )
    .ok_or_else(|| anyhow!("scene too small for a proof layout"))?;
    let desk = layout
        .home_desks
        .first()
        .copied()
        .ok_or_else(|| anyhow!("layout has no home desks"))?;
    let desk_px = (
        u32::from(desk.at.x) * CELL_W,
        (u32::from(desk.at.y) / 2) * CELL_H,
    );

    let backend = TestBackend::new(job.cols, job.rows);
    let mut term = Terminal::new(backend)?;
    let mut floor = pixtuoid_scene::floor::PerFloor::new(std::sync::Arc::clone(job.pack));
    let mut scene = SceneState::uniform(job.max_desks);
    let mut reducer = Reducer::new();
    let mut office = pixtuoid_scene::floor::PerOffice::new();

    let mut wide = FrameSink::pngs(&job.frames_dir.join("wide"))?;
    let mut tall = FrameSink::pngs(&job.frames_dir.join("tall"))?;

    let Timeline { fps, secs, .. } = job.timeline;
    let frames = job.timeline.frame_count();
    for i in 0..frames {
        let elapsed = job.timeline.elapsed_ms(i);
        let now = job.timeline.now(i);
        while pending.front().is_some_and(|(at, _)| *at <= elapsed) {
            if let Some((_, ev)) = pending.pop_front() {
                reducer.apply(&mut scene, ev, now, Transport::Jsonl);
            }
        }
        // `apply` only runs its expiry pass as a side effect of an incoming
        // event, so without this nothing would sweep or cascade once the
        // session's events are drained.
        reducer.tick(&mut scene, now);
        let mut draw_ctx = DrawCtx::offscreen(
            &mut floor,
            office.stores(),
            job.theme,
            &scene,
            now,
            pixtuoid_scene::floor::FloorMeta::ground().with_weather(job.weather),
        );
        draw_scene(&mut term, &mut draw_ctx)?;
        let cells = term.backend().buffer();
        let office = cells_to_rgba(cells, cells.area);
        for (kind, sink) in [
            (ProofLayout::Wide, &mut wide),
            (ProofLayout::Tall, &mut tall),
        ] {
            sink.push(compose_frame(&kind, &office, &script, elapsed, desk_px))?;
        }
        if (i + 1).is_multiple_of(fps as usize) {
            eprint!("\r  proof: {}/{secs}s", (i + 1) / fps as usize);
        }
    }
    eprintln!("\r  proof: {frames} frames x2 layouts @ {fps}fps");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/snapshot/proof-session/01000000-0000-7000-8000-0000000000f4.jsonl")
    }

    #[test]
    fn build_script_pins_the_session_beats() {
        let s = build_script(&session_path()).unwrap();
        assert_eq!(s.events.len(), 8);
        assert!(matches!(s.events[0].1, AgentEvent::SessionStart { .. }));
        assert!(matches!(s.events[1].1, AgentEvent::Rename { .. }));
        let starts = s
            .events
            .iter()
            .filter(|(_, e)| matches!(e, AgentEvent::ToolUseStart { .. }))
            .count();
        let ends = s
            .events
            .iter()
            .filter(|(_, e)| matches!(e, AgentEvent::ToolUseEnd { .. }))
            .count();
        assert_eq!((starts, ends), (3, 3));
        assert!(s.events.windows(2).all(|w| w[0].0 <= w[1].0));
        assert_eq!(s.lines.len(), 7);
        assert_eq!(s.lines[0].text, "$ claude");
        assert!(s.lines[2].prompt, "the user prompt renders as a prompt row");
        assert!(s.lines[6].text.contains("done"));
        assert_eq!(s.lines[2].at_ms, PREAMBLE_MS);
        assert_eq!(s.capture_date, "2026-06-30");
    }

    #[test]
    fn reveal_and_annotation_math() {
        assert_eq!(revealed_chars(1000, 999, 10), 0);
        assert_eq!(revealed_chars(1000, 1000, 10), 0);
        assert_eq!(revealed_chars(1000, 1100, 10), 3);
        assert_eq!(revealed_chars(1000, 9000, 10), 10);
        let lines = vec![
            PanelLine {
                at_ms: 0,
                text: "a".into(),
                prompt: false,
                annotation: Some("x"),
            },
            PanelLine {
                at_ms: 500,
                text: "b".into(),
                prompt: false,
                annotation: None,
            },
            PanelLine {
                at_ms: 900,
                text: "c".into(),
                prompt: false,
                annotation: Some("y"),
            },
            PanelLine {
                at_ms: 900 + ANNOTATION_MAX_HOLD_MS + 50_000,
                text: "d".into(),
                prompt: false,
                annotation: Some("z"),
            },
        ];
        assert_eq!(active_annotation(&lines, 100), Some(0));
        assert_eq!(active_annotation(&lines, 899), Some(0));
        assert_eq!(active_annotation(&lines, 900), Some(2));
        assert_eq!(
            active_annotation(&lines, 900 + ANNOTATION_MAX_HOLD_MS),
            Some(2)
        );
        assert_eq!(
            active_annotation(&lines, 900 + ANNOTATION_MAX_HOLD_MS + 1),
            None
        );
        let z_at = 900 + ANNOTATION_MAX_HOLD_MS + 50_000;
        assert_eq!(active_annotation(&lines, z_at), Some(3));
        assert_eq!(
            active_annotation(&lines, z_at + ANNOTATION_MAX_HOLD_MS + 1_000_000),
            Some(3)
        );
    }

    #[test]
    fn canvas_dims_are_even_and_stack_correctly() {
        let (ww, wh) = canvas_dims(&ProofLayout::Wide, 960, 832);
        assert_eq!((ww, wh), (1720, 902));
        let (tw, th) = canvas_dims(&ProofLayout::Tall, 960, 832);
        assert_eq!((tw, th), (960, 1352));
        for d in [ww, wh, tw, th] {
            assert_eq!(d % 2, 0, "yuv420p needs even dims");
        }
    }

    #[test]
    fn compose_frame_matches_canvas_dims() {
        let s = build_script(&session_path()).unwrap();
        let office = RgbaImage::new(960, 832);
        for layout in [ProofLayout::Wide, ProofLayout::Tall] {
            let (w, h) = canvas_dims(&layout, 960, 832);
            let f = compose_frame(&layout, &office, &s, 10_000, (400, 300));
            assert_eq!((f.width(), f.height()), (w, h));
        }
    }

    #[test]
    fn coda_fits_one_line_at_the_wide_reference_canvas() {
        let lines = coda_lines(1720);
        assert_eq!(lines, [CODA_TEXT]);
    }

    #[test]
    fn coda_wraps_a_narrow_canvas_without_dropping_words() {
        let narrow = coda_lines(500);
        assert!(narrow.len() > 1, "500px must force a wrap");
        assert_eq!(
            narrow.join(" "),
            CODA_TEXT,
            "wrapping must not drop or reorder words"
        );
    }

    #[test]
    fn wrap_text_never_produces_an_empty_line_list() {
        let unit_width = |s: &str| s.chars().count() as i32;
        assert_eq!(wrap_text("", 100, unit_width), vec![String::new()]);
        assert_eq!(wrap_text("hi", 1, unit_width), vec!["hi".to_string()]);
    }
}
