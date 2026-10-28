use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame as GifFrame, Rgba, RgbaImage};
use pixtuoid::dev::{DrawCtx, draw_scene};
use pixtuoid_core::SceneState;
use pixtuoid_scene::cutaway::{Canvas, CellPx, GridInk, paint_grid};
use pixtuoid_scene::floor::{FloorMeta, FloorSession, PerFloor};
use pixtuoid_scene::pack::OfficeArt;
use pixtuoid_scene::theme::Theme;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::cutaway::{CutawayOffice, CutawaySession};
use crate::{CELL_H, CELL_W, SnapshotArgs, due_navigations};

/// Print a connectedness report for the walkable mask: a BFS from the door threshold,
/// reachable vs total walkable pixels. If the two differ, the mask has an isolated
/// region and A* falls back to a straight line when crossing into it — the root cause
/// of any character teleport the user sees.
///
/// `floor_seed` MUST be the one the frame beside it was rendered with: the layout variants
/// have different obstacle placements, so a report computed for another
/// variant is a tick about an office nobody looked at.
pub(crate) fn print_walkability_report(
    term: &Terminal<TestBackend>,
    floor_seed: u64,
) -> Result<()> {
    use pixtuoid_scene::layout::SceneLayout;

    let size = term.size()?;
    let (buf_w, buf_h) = pixtuoid::dev::scene_buf_size(size.width, size.height);
    // `None` = the SAME fill the renderer's draw_scene passes — the overlay
    // must mirror the real layout exactly (desks stamp the walkable mask).
    let Some(layout) = SceneLayout::compute_with_seed(buf_w, buf_h, None, floor_seed) else {
        println!("(debug_walkable) layout too small to compute");
        return Ok(());
    };

    let reach_mask = compute_reachable(&layout);
    let w = layout.buf_w as usize;
    let h = layout.buf_h as usize;
    let mut reachable = 0usize;
    let mut walkable_total = 0usize;
    let mut sample_disconnects: Vec<(u16, u16)> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if layout.is_walkable(x as u16, y as u16) {
                walkable_total += 1;
                if reach_mask[y * w + x] {
                    reachable += 1;
                } else if sample_disconnects.len() < 10 {
                    sample_disconnects.push((x as u16, y as u16));
                }
            }
        }
    }
    let disconnected = walkable_total.saturating_sub(reachable);
    println!(
        "--- walkability report (floor seed {floor_seed}) ---\n\
        total walkable pixels   : {walkable_total}\n\
        reachable from threshold: {reachable}\n\
        disconnected pixels     : {disconnected}{}",
        if disconnected == 0 {
            "  ✓ all open areas connected"
        } else {
            "  ⚠ disconnected components present"
        }
    );
    if !sample_disconnects.is_empty() {
        print!("sample disconnected   : ");
        for (i, (x, y)) in sample_disconnects.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!("({x},{y})");
        }
        println!();
    }
    Ok(())
}

fn compute_reachable(layout: &pixtuoid_scene::layout::SceneLayout) -> Vec<bool> {
    use std::collections::VecDeque;
    let w = layout.buf_w as usize;
    let h = layout.buf_h as usize;
    let mut visited = vec![false; w * h];
    let start = layout.door_threshold;
    if !layout.is_walkable(start.x, start.y) {
        return visited;
    }
    let (sx, sy) = (start.x as usize, start.y as usize);
    visited[sy * w + sx] = true;
    let mut queue: VecDeque<(usize, usize)> = VecDeque::new();
    queue.push_back((sx, sy));
    while let Some((x, y)) = queue.pop_front() {
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 {
                continue;
            }
            let (nx, ny) = (nx as usize, ny as usize);
            if nx >= w || ny >= h || visited[ny * w + nx] {
                continue;
            }
            if !layout.is_walkable(nx as u16, ny as u16) {
                continue;
            }
            visited[ny * w + nx] = true;
            queue.push_back((nx, ny));
        }
    }
    visited
}

pub(crate) fn compute_crop_rect(
    args: &SnapshotArgs,
    scene: &SceneState,
    history: &pixtuoid_scene::pose::PoseHistory,
    cols: u16,
    rows: u16,
    now: SystemTime,
) -> Result<Option<ratatui::layout::Rect>> {
    // Fail loudly, as an unknown --theme/--weather does: a typo'd crop target
    // silently writing the full uncropped PNG defeats the point of the flag.
    let target_pixel: pixtuoid_scene::layout::Point = if let Some(ref agent_label) = args.crop_agent
    {
        let slot = crate::scenes::agent_named(scene, "--crop-agent", agent_label)?;
        // `PoseHistory` records only waypoint and walking poses, so every
        // desk-seated agent — the majority, and every seated pose — has no entry.
        // Fall back to the seat the painter would draw them on rather than widen
        // a sim store for a dev tool (#909).
        match history.recent(slot.agent_id, u64::MAX, now) {
            Some(p) => p,
            None => {
                let (buf_w, buf_h) = pixtuoid::dev::scene_buf_size(cols, rows);
                // The agent's OWN floor: `desk_index` is global, and a scene with
                // more agents than `--max-desks` puts them on floor 1+, whose
                // geometry and seed both differ from floor 0's.
                let layout = pixtuoid_scene::layout::SceneLayout::compute_with_seed(
                    buf_w,
                    buf_h,
                    Some(
                        scene.floor_capacities
                            [slot.floor_idx.min(pixtuoid_core::state::MAX_FLOORS - 1)],
                    ),
                    pixtuoid_scene::floor::floor_seed(slot.floor_idx),
                )
                .ok_or_else(|| anyhow::anyhow!("scene too small to compute a layout"))?;
                let idx = scene.floor_local_desk(slot.desk_index);
                let desk = layout.home_desk(idx).ok_or_else(|| {
                    anyhow::anyhow!("agent {agent_label:?} is neither placed nor at a home desk")
                })?;
                pixtuoid_scene::sim::seated_top_left(
                    desk.at,
                    pixtuoid_scene::layout::CHARACTER_SPRITE_W,
                    desk.facing,
                )
            }
        }
    } else if let Some(ref furniture_str) = args.crop_furniture {
        let (buf_w, buf_h) = pixtuoid::dev::scene_buf_size(cols, rows);
        let layout = pixtuoid_scene::layout::SceneLayout::compute_with_seed(
            buf_w,
            buf_h,
            Some(scene.floor_capacities[0]),
            args.floor_seed,
        )
        .ok_or_else(|| anyhow::anyhow!("scene too small to compute a layout"))?;
        let found = match furniture_str.to_lowercase().as_str() {
            "desk" => layout.home_desks.first().map(|d| d.at),
            name => {
                let kind = crate::scenes::waypoint_target(name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown --crop-furniture {name:?}; valid: pantry | couch | vending | printer | meeting | sofa | chair | island | snackshelf | desk"
                    )
                })?;
                layout
                    .waypoints
                    .iter()
                    .find(|w| w.kind == kind)
                    .map(|w| w.pos)
            }
        };
        found.ok_or_else(|| {
            anyhow::anyhow!("no {furniture_str:?} waypoint in this layout (terminal too small?)")
        })?
    } else {
        return Ok(None);
    };

    Ok(Some(centered_crop(target_pixel, cols, rows)))
}

/// The `--crop-*` window, in cells.
const CROP_WINDOW: ratatui::layout::Size = ratatui::layout::Size::new(40, 24);

/// A [`CROP_WINDOW`] centered on `target`, a half-block buffer pixel, clamped
/// to stay inside the cols x rows terminal (shrinks only when the terminal
/// itself is smaller).
pub(crate) fn centered_crop(
    target: pixtuoid_scene::layout::Point,
    cols: u16,
    rows: u16,
) -> ratatui::layout::Rect {
    let (cell_x, cell_y) = (target.x, target.y / pixtuoid_scene::layout::CELL_ROWS);
    let crop_w = CROP_WINDOW.width.min(cols);
    let crop_h = CROP_WINDOW.height.min(rows);

    let crop_x = cell_x
        .saturating_sub(crop_w / 2)
        .min(cols.saturating_sub(crop_w));
    let crop_y = cell_y
        .saturating_sub(crop_h / 2)
        .min(rows.saturating_sub(crop_h));

    ratatui::layout::Rect {
        x: crop_x,
        y: crop_y,
        width: crop_w,
        height: crop_h,
    }
}

/// [`compose`]'s frame of `area` to the PNG at `path`.
pub(crate) fn save_backend_as_png(
    term: &Terminal<TestBackend>,
    path: &Path,
    area: ratatui::layout::Rect,
    cutaway: Option<&CutawayOffice<FloorSession>>,
) -> Result<()> {
    let frame = compose(term.backend().buffer(), area, cutaway);
    // Every pixel is opaque, so the PNG carries no alpha.
    image::DynamicImage::from(frame).into_rgb8().save(path)?;
    Ok(())
}

/// [`cells_to_rgba`], `cutaway`'s office over its scene cells where there is
/// one: the frame every still and `--gif` capture writes.
pub(crate) fn compose<S: CutawaySession>(
    term_buf: &ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    cutaway: Option<&CutawayOffice<S>>,
) -> RgbaImage {
    let mut img = cells_to_rgba(term_buf, area);
    if let Some(cutaway) = cutaway {
        cutaway.paint(&mut img);
    }
    img
}

/// `area`'s cells of `term_buf` from the image's origin, one [`terminal_cell`]
/// each, in the window's face: the ONE rasterizer. A cell with no colour of
/// its own shows [`TERMINAL_BG`] and takes [`TERMINAL_FG`].
pub(crate) fn cells_to_rgba(
    term_buf: &ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
) -> RgbaImage {
    let [r, g, b] = TERMINAL_BG;
    let mut rgba = RgbaImage::from_pixel(
        u32::from(area.width) * CELL_W,
        u32::from(area.height) * CELL_H,
        Rgba([r, g, b, 255]),
    );
    paint_grid(
        &mut ImageCanvas(&mut rgba),
        &pixtuoid::dev::grid_of(term_buf, area),
        ((0, 0), terminal_cell()),
        crate::icons(),
        GridInk {
            text: TERMINAL_FG,
            halo: None,
            shadow: None,
        },
    );
    rgba
}

/// The colours a cell the terminal colours itself (`Reset`) shows in.
const TERMINAL_BG: [u8; 3] = [20, 22, 28];
const TERMINAL_FG: pixtuoid_core::sprite::Rgb = pixtuoid_core::sprite::Rgb {
    r: 220,
    g: 220,
    b: 220,
};

/// One terminal cell of the frames, [`CELL_W`]×[`CELL_H`].
pub(crate) fn terminal_cell() -> CellPx {
    CellPx {
        w: u16::try_from(CELL_W).expect("a cell is small"),
        h: u16::try_from(CELL_H).expect("a cell is small"),
    }
}

/// A capture's frame clock — `secs` of frames at `fps` from `start` — shared by
/// the animation encoders and the proof frames.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeline {
    pub(crate) fps: u64,
    pub(crate) secs: u64,
    pub(crate) start: SystemTime,
}

impl Timeline {
    pub(crate) fn frame_count(&self) -> usize {
        (self.secs * self.fps) as usize
    }

    /// Frame `i`'s offset from `start`. Exact, not `i * frame_ms`: the truncated
    /// `frame_ms` accumulates, drifting every time-derived element off the wall
    /// clock by the last frame — and a late --navigate-at then never fires.
    ///
    /// This does NOT make the site's `loop`ed clip seam-free, and no timing choice
    /// can: each creature walk's destination is a seeded `walkable_target` draw
    /// numbered by its roam, so the roaming is aperiodic BY DESIGN and frame N is never frame 0
    /// however the duration is chosen. Closing it would mean a scripted
    /// (non-wandering) timeline for the demo — a media decision, not a rendering one.
    pub(crate) fn elapsed_ms(&self, i: usize) -> u64 {
        i as u64 * 1000 / self.fps.max(1)
    }

    pub(crate) fn now(&self, i: usize) -> SystemTime {
        self.start + Duration::from_millis(self.elapsed_ms(i))
    }
}

/// Where an animation's frames go.
pub(crate) enum FrameSink {
    Gif {
        encoder: GifEncoder<std::fs::File>,
        delay: Delay,
    },
    /// Lossless PNGs for a consumer that re-encodes (gen-media's clips and their
    /// posters): the GIF encoder NeuQuant-quantises every frame past 256 colours
    /// (`gif::Frame::from_rgba_speed`), and a re-encode of the GIF inherits that loss.
    Pngs { dir: PathBuf, written: usize },
}

impl FrameSink {
    pub(crate) fn open(gif_path: &Path, frames_dir: Option<&Path>, frame_ms: u64) -> Result<Self> {
        if let Some(dir) = frames_dir {
            return Self::pngs(dir);
        }
        let mut encoder = GifEncoder::new(std::fs::File::create(gif_path)?);
        encoder.set_repeat(Repeat::Infinite)?;
        Ok(Self::Gif {
            encoder,
            delay: Delay::from_numer_denom_ms(frame_ms as u32, 1),
        })
    }

    pub(crate) fn pngs(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self::Pngs {
            dir: dir.to_path_buf(),
            written: 0,
        })
    }

    /// Frame `n`'s file in a [`Self::Pngs`] `dir`, counted from 1: the name
    /// gen-media.py's `FRAME_PATTERN` matches.
    pub(crate) fn frame_path(dir: &Path, n: usize) -> PathBuf {
        dir.join(format!("f{n:04}.png"))
    }

    pub(crate) fn push(&mut self, rgba: RgbaImage) -> Result<()> {
        match self {
            Self::Gif { encoder, delay } => {
                encoder.encode_frame(GifFrame::from_parts(rgba, 0, 0, *delay))?;
            }
            Self::Pngs { dir, written } => {
                *written += 1;
                rgba.save(Self::frame_path(dir, *written))?;
            }
        }
        Ok(())
    }
}

/// One animation capture. `scene`, `pack` and `theme` are what each path's per-frame
/// render reads; [`AnimJob::encode`] itself only clocks and encodes.
pub(crate) struct AnimJob<'a> {
    pub(crate) path: &'a Path,
    pub(crate) frames_dir: Option<&'a Path>,
    pub(crate) timeline: Timeline,
    pub(crate) scene: &'a SceneState,
    pub(crate) pack: &'a std::sync::Arc<OfficeArt>,
    pub(crate) theme: &'static Theme,
    pub(crate) weather: pixtuoid_scene::sky::WeatherPolicy,
}

impl AnimJob<'_> {
    /// Call `render` once per frame on `state` — `skip_ms` of pre-roll first,
    /// rendered but not encoded — then encode the image `image` reads back.
    fn encode<S>(
        &self,
        skip_ms: u64,
        state: &mut S,
        mut render: impl FnMut(&mut S, SystemTime, u64) -> Result<()>,
        image: impl Fn(&S) -> RgbaImage,
    ) -> Result<()> {
        let Timeline { fps, secs, .. } = self.timeline;
        let frame_count = self.timeline.frame_count();
        let frame_ms = 1000 / fps.max(1);
        let skip_frames = (skip_ms / frame_ms.max(1)) as usize;

        let mut sink = FrameSink::open(self.path, self.frames_dir, frame_ms)?;
        for i in 0..(skip_frames + frame_count) {
            let elapsed_ms = self.timeline.elapsed_ms(i);
            render(state, self.timeline.now(i), elapsed_ms)?;
            if i < skip_frames {
                continue;
            }
            sink.push(image(state))?;
            let cap = i + 1 - skip_frames;
            if cap.is_multiple_of(fps as usize) {
                eprint!("\r  encoding: {}/{secs}s", cap / fps as usize);
            }
        }
        eprintln!("\r  encoded {frame_count} frames @ {fps}fps");
        Ok(())
    }
}

/// Drive the real TuiRenderer (slide transition, footer floor chip, pet motion) frame by
/// frame and encode its TestBackend cell buffer; with `cutaway`, a second
/// office navigated as the renderer's is painted over its office cells.
pub(crate) fn save_renderer_animation(
    job: &AnimJob,
    term: Terminal<TestBackend>,
    navigations: &[(u64, usize)],
    pets: Vec<pixtuoid_scene::pet::Pet>,
    cutaway: bool,
) -> Result<()> {
    let size = term.size()?;
    let cutaway = cutaway
        .then(|| CutawayOffice::of_renderer(job.pack, size.width, size.height, pets.clone()))
        .transpose()?;
    let mut r =
        pixtuoid::dev::TuiRenderer::new(term, job.theme, pets, std::sync::Arc::clone(job.pack));
    r.set_weather(job.weather);
    let mut fired = vec![false; navigations.len()];
    // 0, not the caller's skip_ms: clap keeps every pre-roll flag off this path
    // (`conflicts_with` on --navigate-at / --pets), and a pre-roll would shift the
    // --navigate-at schedule off the encoded clip's t=0.
    job.encode(
        0,
        &mut (r, cutaway),
        |(r, cutaway), now, elapsed_ms| {
            for floor in due_navigations(navigations, &mut fired, elapsed_ms) {
                r.navigate_floor(floor, now);
                if let Some(cutaway) = cutaway {
                    cutaway.navigate(floor, now);
                }
            }
            r.render(job.scene, now)?;
            if let Some(cutaway) = cutaway {
                cutaway.render(r.office_world(job.scene, now), job.theme)?;
            }
            Ok(())
        },
        |(r, cutaway)| {
            let cells = r.terminal.backend().buffer();
            compose(cells, cells.area, cutaway.as_ref())
        },
    )
}

/// Drive `draw_scene` over one floor, `cutaway`'s office over it where there
/// is one, and encode it; `skip_ms` (from --anim, --meeting or --warmup-secs)
/// starts the clip mid-action.
pub(crate) fn save_animation(
    job: &AnimJob,
    term: &mut Terminal<TestBackend>,
    floor: &mut PerFloor,
    floor_meta: FloorMeta,
    skip_ms: u64,
    debug_walkable: bool,
    cutaway: Option<CutawayOffice<FloorSession>>,
) -> Result<()> {
    let scene = job.scene;
    let mut office = pixtuoid_scene::floor::PerOffice::new();
    job.encode(
        skip_ms,
        &mut (term, cutaway),
        |(term, cutaway), now, _| {
            let mut draw_ctx = DrawCtx {
                debug_walkable,
                ..DrawCtx::offscreen(floor, office.stores(), job.theme, scene, now, floor_meta)
            };
            draw_scene(term, &mut draw_ctx)?;
            if let Some(cutaway) = cutaway {
                cutaway.render(draw_ctx.world, job.theme)?;
            }
            Ok(())
        },
        |(term, cutaway)| {
            let cells = term.backend().buffer();
            compose(cells, cells.area, cutaway.as_ref())
        },
    )
}

/// An `image` buffer as the scene's screen-text [`Canvas`]: the examples draw
/// their text through the window's sink and face.
pub(crate) struct ImageCanvas<'a, I>(pub(crate) &'a mut I);

impl<I: image::GenericImage> Canvas for ImageCanvas<'_, I>
where
    I::Pixel: image::Pixel<Subpixel = u8>,
{
    fn pixel(&self, x: i32, y: i32) -> Option<pixtuoid_core::sprite::Rgb> {
        use image::Pixel as _;
        let (x, y) = (u32::try_from(x).ok()?, u32::try_from(y).ok()?);
        (x < self.0.width() && y < self.0.height()).then(|| {
            let c = self.0.get_pixel(x, y).to_rgb();
            pixtuoid_core::sprite::Rgb {
                r: c[0],
                g: c[1],
                b: c[2],
            }
        })
    }

    fn set(&mut self, x: i32, y: i32, rgb: pixtuoid_core::sprite::Rgb) {
        use image::Pixel as _;
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return;
        };
        if x < self.0.width() && y < self.0.height() {
            let mut px = self.0.get_pixel(x, y);
            // Alpha, where the pixel has one, stays: every canvas here is opaque.
            px.channels_mut()[..3].copy_from_slice(&[rgb.r, rgb.g, rgb.b]);
            self.0.put_pixel(x, y, px);
        }
    }
}

/// Fill a rect, clipped to `img`.
pub(crate) fn fill_rect<I: image::GenericImage>(
    img: &mut I,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    px: I::Pixel,
) {
    let (img_w, img_h) = (img.width(), img.height());
    for j in 0..h {
        for i in 0..w {
            let (px_x, px_y) = (x + i, y + j);
            if px_x < img_w && px_y < img_h {
                img.put_pixel(px_x, px_y, px);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_scene::layout::Point;

    #[test]
    fn centered_crop_centers_in_the_open() {
        let r = centered_crop(Point { x: 96, y: 64 }, 192, 64);
        assert_eq!((r.x, r.y, r.width, r.height), (76, 20, 40, 24));
    }

    #[test]
    fn centered_crop_clamps_at_origin_and_far_edge() {
        let near_origin = centered_crop(Point { x: 2, y: 2 }, 192, 64);
        assert_eq!((near_origin.x, near_origin.y), (0, 0));
        let near_edge = centered_crop(Point { x: 191, y: 126 }, 192, 64);
        assert_eq!((near_edge.x, near_edge.y), (152, 40));
    }

    #[test]
    fn centered_crop_shrinks_to_a_small_terminal() {
        let r = centered_crop(Point { x: 10, y: 10 }, 30, 20);
        assert_eq!((r.x, r.y, r.width, r.height), (0, 0, 30, 20));
    }
}
