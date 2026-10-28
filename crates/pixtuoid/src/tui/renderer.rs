//! Terminal-coupled rendering: the `draw_scene` orchestrator and the half-block
//! flush of the `RgbBuffer` that `look::render` produces.

use std::time::SystemTime;

use anyhow::Result;
use pixtuoid_core::SceneState;
use pixtuoid_core::sprite::RgbBuffer;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::style::Color;

use pixtuoid_scene::display::HoverTarget;
use pixtuoid_scene::display::cells::CellMap;
use pixtuoid_scene::floor::{FloorInputs, OfficeStores, PerFloor};
use pixtuoid_scene::footer::{FooterContext, FooterInputs};
use pixtuoid_scene::layout::Size;
use pixtuoid_scene::look::{FloorPicture, Hits, Look, Place, RenderInputs};

pub(crate) use crate::panels::widgets::{TooltipAt, paint_tooltip};
pub(super) use crate::panels::widgets::{paint_footer, paint_host_text, star_area};
pub(crate) use crate::panels::{FOOTER_ROWS, scene_rect};
use crate::tui::geometry::SceneGeometry;
pub(crate) use crate::tui::hit_test::SceneHit;

pub use pixtuoid_scene::pet::PetState;

#[derive(Debug)]
pub struct DrawCtx<'a> {
    pub world: FloorInputs<'a>,
    /// The floor drawn: its sim stores and raster.
    pub floor: &'a mut PerFloor,
    /// The office stores its frame steps and draws with.
    pub office: OfficeStores<'a>,
    pub mouse_pos: Option<(u16, u16)>,
    /// Walkable/approach/route debug layer toggle (`w`) — transient, never
    /// persisted to config.
    pub debug_walkable: bool,
    pub theme: &'static pixtuoid_scene::theme::Theme,
    /// From [`footer_context`](crate::panels::widgets::footer_context).
    pub footer: FooterContext<'a>,
    /// The panels open over the frame.
    pub overlays: crate::panels::OverlayFrame<'a>,
}

impl<'a> DrawCtx<'a> {
    /// An offscreen still of one floor, every input and overlay off; the office-wide
    /// tallies and gateway come from `scene`, as the live renderer's do.
    #[doc(hidden)]
    pub fn offscreen(
        floor: &'a mut PerFloor,
        office: OfficeStores<'a>,
        theme: &'static pixtuoid_scene::theme::Theme,
        scene: &'a SceneState,
        now: SystemTime,
        meta: pixtuoid_scene::floor::FloorMeta,
    ) -> Self {
        Self {
            world: FloorInputs {
                scene,
                now,
                floor: meta,
                pets: Default::default(),
            },
            floor,
            office,
            mouse_pos: None,
            debug_walkable: false,
            theme,
            footer: crate::panels::widgets::footer_context(scene, None, false, None, None),
            overlays: crate::panels::OverlayFrame::closed(),
        }
    }
}

/// Minimum drawable scene size (cells), the bound [`scene_too_small`] gates on.
pub(crate) const MIN_SCENE_WIDTH: u16 = 20;
pub(crate) const MIN_SCENE_HEIGHT: u16 = 12;

/// Whether `scene` is too small to render the world, for a footer-only draw.
pub(crate) fn scene_too_small(scene: Rect) -> bool {
    scene.width < MIN_SCENE_WIDTH || scene.height < MIN_SCENE_HEIGHT
}

/// The pixel buffer a `cols`×`rows` terminal's scene paints, two half-block pixels a row.
#[doc(hidden)]
pub fn scene_buf_size(cols: u16, rows: u16) -> (u16, u16) {
    let scene = scene_rect(Rect::new(0, 0, cols, rows));
    (scene.width, scene.height.saturating_mul(2))
}

/// Paint the footer-only frame shown when the terminal is too small to render the
/// office — the SHARED body of every too-small gate, so a hit paints one clean
/// footer-only frame instead of leaving a stale, clipped, footer-less one frozen.
///
/// It paints the modal overlays too: every modal's key handler stays live at any
/// size, so suppressing the overlay here made `?`/`s`/`Tab` toggle something
/// invisible on any terminal below the office layout's minimum — and first run
/// opens the onboarding modal there.
pub(crate) fn draw_footer_only_frame<B: Backend<Error: Send + Sync + 'static>>(
    term: &mut Terminal<B>,
    footer: &FooterInputs<'_>,
    theme: &pixtuoid_scene::theme::Theme,
    overlays: &crate::panels::OverlayFrame<'_>,
    now: SystemTime,
) -> Result<()> {
    term.draw(|f| {
        let actual = f.area();
        paint_footer(f, footer, actual, theme);
        crate::panels::paint_overlays(f, overlays, now, actual, theme);
        // LAST: a modal centres on the same rows, and first run opens one here —
        // so painting the notice first left the black screen unexplained in the
        // one case it exists for.
        paint_too_small_notice(f, actual, theme);
    })?;
    Ok(())
}

/// `min_layout_size` in the terminal's own units — rows carry TWO buffer pixels
/// (half-block), the footer takes its own, and the painter's own floor can outrank
/// the layout's. The notice states it and the harness derives its fixtures from it;
/// [`scene_buf_size`] is its inverse.
pub(crate) fn min_terminal_size() -> (u16, u16) {
    let min = pixtuoid_scene::layout::min_layout_size();
    (min.w.max(MIN_SCENE_WIDTH), advertised_rows(min.h))
}

/// Rows for a `layout_h` buffer. The `.max` sits INSIDE the footer add because
/// `MIN_SCENE_HEIGHT` gates `scene_rect.height`, which is already footer-less —
/// outside it, the branch where the painter floor binds under-advertises by a row
/// and the notice names a size that still shows the notice.
const fn advertised_rows(layout_h: u16) -> u16 {
    let scene_rows = layout_h.div_ceil(2);
    let floored = if scene_rows > MIN_SCENE_HEIGHT {
        scene_rows
    } else {
        MIN_SCENE_HEIGHT
    };
    floored + FOOTER_ROWS
}

#[cfg(test)]
mod min_size_tests {
    use super::*;

    /// The painter-floor branch is dormant at today's layout floor, so pin it
    /// directly: whatever it advertises must leave `MIN_SCENE_HEIGHT` scene rows
    /// AFTER the footer, or `draw_scene`'s own gate refuses the advertised size.
    #[test]
    fn the_advertised_rows_clear_the_painters_gate_on_both_branches() {
        for layout_h in [1u16, 2, 10, 24, 25, 90, 400] {
            let rows = advertised_rows(layout_h);
            assert!(
                rows.saturating_sub(FOOTER_ROWS) >= MIN_SCENE_HEIGHT,
                "layout_h {layout_h} advertises {rows} rows, leaving {} scene rows",
                rows.saturating_sub(FOOTER_ROWS)
            );
            assert!(
                rows.saturating_sub(FOOTER_ROWS) * 2 >= layout_h,
                "layout_h {layout_h} advertises {rows} rows, whose buffer is under it"
            );
        }
    }
}

/// Say WHY there is no office. Every caller of `draw_footer_only_frame` is
/// a refusal — the scene rect is under the painter's own floor, `frame_layout`
/// declined, or a floor transition hit the same gate — and a silent refusal reads
/// as a crash on the small terminal a first-time user is most likely to be at.
fn paint_too_small_notice(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    theme: &pixtuoid_scene::theme::Theme,
) {
    let (need_cols, need_rows) = min_terminal_size();
    // Widest form that FITS: a terminal narrow enough to trigger this is often
    // too narrow to hold the sentence explaining it, and skipping the line then
    // leaves exactly the blank screen this exists to prevent.
    let long = format!(
        "needs {need_cols}x{need_rows}, this is {}x{}",
        area.width, area.height
    );
    let short = format!("{need_cols}x{need_rows} min");
    let lines: Vec<String> = if long.chars().count() as u16 <= area.width {
        vec!["terminal too small".to_string(), long]
    } else {
        vec!["too small".to_string(), short]
    };
    let top = area.height.saturating_sub(FOOTER_ROWS) / 2;
    for (i, text) in lines.iter().enumerate() {
        let w = text.chars().count() as u16;
        if w > area.width || top + i as u16 >= area.height {
            continue;
        }
        f.render_widget(
            ratatui::widgets::Paragraph::new(text.as_str()).style(
                ratatui::style::Style::default().fg(Color::Rgb(
                    theme.ui.tooltip_dim.r,
                    theme.ui.tooltip_dim.g,
                    theme.ui.tooltip_dim.b,
                )),
            ),
            Rect {
                x: (area.width - w) / 2,
                y: top + i as u16,
                width: w,
                height: 1,
            },
        );
    }
}

/// The tooltip for what `hit` names on `world`'s frame, whichever painter
/// drew it.
pub(crate) fn paint_scene_tooltip(
    f: &mut ratatui::Frame<'_>,
    hit: &SceneHit<'_>,
    world: &FloorInputs<'_>,
    at: TooltipAt,
    theme: &pixtuoid_scene::theme::Theme,
) {
    if let Some(tip) = pixtuoid_scene::tooltip::for_hit(*hit, world) {
        paint_tooltip(f, &tip, at, theme);
    }
}

/// Draw one classic-look frame (scene, footer, overlays): what it answers a
/// pointer with, `None` for a footer-only frame.
///
/// # Errors
///
/// If querying the terminal size or drawing the frame to the backend fails.
pub fn draw_scene<B: Backend<Error: Send + Sync + 'static>>(
    term: &mut Terminal<B>,
    ctx: &mut DrawCtx<'_>,
) -> Result<Option<Hits>> {
    let term_size = term.size()?;
    let full_rect = Rect {
        x: 0,
        y: 0,
        width: term_size.width,
        height: term_size.height,
    };
    let scene_rect = scene_rect(full_rect);
    let theme = ctx.theme;
    let world = ctx.world;
    let FloorInputs { scene, now, .. } = world;
    let footer = FooterInputs::new(scene, ctx.footer);
    let overlays = ctx.overlays;

    if scene_too_small(scene_rect) {
        draw_footer_only_frame(term, &footer, theme, &overlays, now)?;
        return Ok(None);
    }

    let (buf_w, buf_h) = scene_buf_size(full_rect.width, full_rect.height);
    let rendered = pixtuoid_scene::look::render(
        ctx.floor,
        ctx.office.reborrow(),
        Look::Classic,
        RenderInputs {
            world,
            theme,
            size: Size { w: buf_w, h: buf_h },
            place: Place {
                gateway: footer.context.gateway,
                floor: footer.context.floor,
            },
            debug_walkable: ctx.debug_walkable,
            dim: overlays.onboarding.dim,
        },
    );
    let Some(picture) = rendered.as_ref().and_then(|r| r.picture(&ctx.floor.raster)) else {
        draw_footer_only_frame(term, &footer, theme, &overlays, now)?;
        return Ok(None);
    };
    let frame = ClassicFrame {
        footer: &footer,
        overlays: &overlays,
        theme,
        world: &world,
        mouse_pos: ctx.mouse_pos,
    };
    flush_classic(term, &frame, &picture)?;
    Ok(Some(picture.hits.clone()))
}

/// A cutaway frame's cells, in the one order every painter of it draws them:
/// the footer, `under` (the image's cells placed, then the text the host sets
/// over them), the panels, and what `carve` makes of the image's cells after.
pub(crate) fn paint_over_image<C>(
    f: &mut ratatui::Frame<'_>,
    footer: &FooterInputs<'_>,
    overlays: &crate::panels::OverlayFrame<'_>,
    theme: &pixtuoid_scene::theme::Theme,
    now: SystemTime,
    under: impl FnOnce(&mut ratatui::Frame<'_>, Rect),
    carve: impl FnOnce(&mut ratatui::buffer::Buffer, Rect) -> C,
) -> C {
    let full = f.area();
    let scene = scene_rect(full);
    paint_footer(f, footer, full, theme);
    under(f, scene);
    crate::panels::paint_overlays(f, overlays, now, full, theme);
    carve(f.buffer_mut(), scene)
}

/// [`draw_scene`]'s frame over an image of the office: [`paint_over_image`]
/// with every scene cell the image's, and what of them stays image.
///
/// # Errors
///
/// If querying the terminal size or drawing the frame to the backend fails.
#[doc(hidden)]
pub fn draw_over_image<B: Backend<Error: Send + Sync + 'static>>(
    term: &mut Terminal<B>,
    ctx: &DrawCtx<'_>,
) -> Result<crate::tui::cutaway::ImageCells> {
    use crate::tui::cutaway::ImageCells;
    let theme = ctx.theme;
    let FloorInputs { scene, now, .. } = ctx.world;
    let footer = FooterInputs::new(scene, ctx.footer);
    let overlays = ctx.overlays;
    let size = term.size()?;
    if scene_too_small(scene_rect(Rect::new(0, 0, size.width, size.height))) {
        draw_footer_only_frame(term, &footer, theme, &overlays, now)?;
        return Ok(ImageCells::none());
    }
    let mut image = ImageCells::none();
    term.draw(|f| {
        image = paint_over_image(
            f,
            &footer,
            &overlays,
            theme,
            now,
            |f, scene| ImageCells::place(f.buffer_mut(), scene),
            |buf, scene| ImageCells::carve(buf, scene, overlays.onboarding.dim),
        );
    })?;
    Ok(image)
}

/// Where the half-blocks lie under the cells of a terminal `size` big.
pub(crate) fn classic_geometry(size: ratatui::layout::Size) -> SceneGeometry {
    SceneGeometry::half_block(scene_rect(Rect::new(0, 0, size.width, size.height)))
}

/// [`classic_geometry`], and the cells of the star `picture`'s host text
/// sets there.
pub(crate) fn classic_placement(
    size: ratatui::layout::Size,
    picture: &FloorPicture<'_>,
) -> (SceneGeometry, Option<pixtuoid_scene::layout::Bounds>) {
    let geometry = classic_geometry(size);
    let star = picture
        .text
        .and_then(|text| star_area(text.signs, geometry.map()));
    (geometry, star)
}

/// What a classic frame flushes beside the floor's own drawing: whichever
/// painter rendered it.
pub(crate) struct ClassicFrame<'f> {
    pub(crate) footer: &'f FooterInputs<'f>,
    pub(crate) overlays: &'f crate::panels::OverlayFrame<'f>,
    pub(crate) theme: &'static pixtuoid_scene::theme::Theme,
    /// The floor's inputs, which a tooltip reads.
    pub(crate) world: &'f FloorInputs<'f>,
    pub(crate) mouse_pos: Option<(u16, u16)>,
}

/// Flush the classic `picture` to `term` with `frame`'s footer, text and
/// overlays.
///
/// # Errors
///
/// If querying the terminal size or drawing to the backend fails.
pub(crate) fn flush_classic<B: Backend<Error: Send + Sync + 'static>>(
    term: &mut Terminal<B>,
    frame: &ClassicFrame<'_>,
    picture: &FloorPicture<'_>,
) -> Result<()> {
    let &ClassicFrame {
        footer,
        overlays,
        theme,
        world,
        mouse_pos,
    } = frame;
    let now = world.now;
    let pixels = picture.pixels;
    let (geometry, star) = classic_placement(term.size()?, picture);
    let hit =
        mouse_pos.and_then(|(mx, my)| picture.hits.at(geometry.area_at(mx, my)?.bounds(), star));
    let hovered = match hit {
        Some(SceneHit::Figure(HoverTarget::Agent(id))) => Some(*id),
        _ => None,
    };

    term.draw(|f| {
        // Re-derive rects from the actual frame buffer to guard against
        // terminal resize between term.size() and term.draw().
        let actual_full = f.area();
        let actual_scene = crate::tui::renderer::scene_rect(actual_full);
        paint_footer(f, footer, actual_full, theme);
        flush_buffer_to_term(f, pixels, actual_scene);
        if let Some(text) = picture.text {
            paint_host_text(f, text, (actual_scene, CellMap::HALF_BLOCK), hovered);
        }
        let at = mouse_pos.map(|(mx, my)| TooltipAt {
            mx,
            my,
            scene_rect: actual_scene,
        });
        if let (Some(hit), Some(at)) = (&hit, at) {
            paint_scene_tooltip(f, hit, world, at, theme);
        }
        crate::panels::paint_overlays(f, overlays, now, actual_full, theme);
    })?;
    Ok(())
}

/// `buf`'s pixels into `scene_rect`'s cells, two rows a half-block.
pub(crate) fn flush_buffer_to_term(f: &mut ratatui::Frame<'_>, buf: &RgbBuffer, scene_rect: Rect) {
    let term_buf = f.buffer_mut();
    let term_area = term_buf.area;
    let w = buf.width() as usize;
    let cell_rows = usize::from(buf.height() / 2).min(usize::from(scene_rect.height));
    for cy in 0..cell_rows {
        for cx in 0..(buf.width() as usize) {
            let x = scene_rect.x + cx as u16;
            let y = scene_rect.y + cy as u16;
            if x >= scene_rect.x + scene_rect.width {
                continue;
            }
            if x >= term_area.width || y >= term_area.height {
                continue;
            }
            let py_top = cy * 2;
            let py_bot = cy * 2 + 1;
            let fg = buf.as_slice()[py_top * w + cx];
            let bg = buf.as_slice()[py_bot * w + cx];
            set_half_block(&mut term_buf[(x, y)], fg, bg);
        }
    }
}

/// Show `top` over `bottom` in `cell`: the half-block every flush of the
/// office paints with.
fn set_half_block(
    cell: &mut ratatui::buffer::Cell,
    top: pixtuoid_core::sprite::Rgb,
    bottom: pixtuoid_core::sprite::Rgb,
) {
    cell.set_symbol("\u{2580}");
    cell.fg = Color::Rgb(top.r, top.g, top.b);
    cell.bg = Color::Rgb(bottom.r, bottom.g, bottom.b);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::clip_widget_rect;
    use crate::tui::geometry::CellArea;
    use std::sync::Arc;

    #[test]
    fn an_offscreen_still_shows_its_scenes_gateway() {
        let t0 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let mut scene = SceneState::uniform(16);
        scene.insert_daemon(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            pixtuoid_core::state::DaemonInstanceId::new("18789").expect("non-empty"),
            pixtuoid_core::state::DaemonPresence {
                liveness: pixtuoid_core::state::DaemonLiveness::UP,
                active_sessions: 0,
                last_seen: t0,
                entered_at: t0,
                in_flight_turns: Default::default(),
                current_pid: Some(1),
            },
        );
        let mut floor = pixtuoid_scene::floor::PerFloor::new(Arc::new(
            pixtuoid_scene::pack::load_bundled_pack().expect("pack"),
        ));
        let mut office = pixtuoid_scene::floor::PerOffice::new();
        let ctx = DrawCtx::offscreen(
            &mut floor,
            office.stores(),
            &pixtuoid_scene::theme::NORMAL,
            &scene,
            t0,
            pixtuoid_scene::floor::FloorMeta::ground(),
        );
        assert!(ctx.footer.gateway.is_some());
    }

    #[test]
    fn clip_widget_rect_fully_inside() {
        let r = Rect {
            x: 2,
            y: 2,
            width: 4,
            height: 4,
        };
        let b = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        assert_eq!(clip_widget_rect(r, b), Some(r));
    }

    #[test]
    fn clip_widget_rect_fully_outside_right() {
        let r = Rect {
            x: 80,
            y: 0,
            width: 10,
            height: 5,
        };
        let b = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        assert_eq!(clip_widget_rect(r, b), None);
    }

    #[test]
    fn clip_widget_rect_partially_overflows_right() {
        let r = Rect {
            x: 75,
            y: 0,
            width: 10,
            height: 5,
        };
        let b = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let clipped = clip_widget_rect(r, b).unwrap();
        assert_eq!(clipped.x, 75);
        assert_eq!(clipped.width, 5);
    }

    #[test]
    fn clip_widget_rect_zero_size_returns_none() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 5,
        };
        let b = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        assert_eq!(clip_widget_rect(r, b), None);
    }

    // A zero-HEIGHT rect sits STRICTLY inside both entry guards, so only the final
    // collapse guard (`bot <= y`) can reject it — unlike the zero-WIDTH rect above,
    // which exits early.
    #[test]
    fn clip_widget_rect_zero_height_inside_bounds_returns_none() {
        let zero_h = Rect {
            x: 2,
            y: 2,
            width: 4,
            height: 0,
        };
        let b = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        assert_eq!(clip_widget_rect(zero_h, b), None);

        let inside = Rect {
            x: 2,
            y: 2,
            width: 4,
            height: 3,
        };
        assert_eq!(clip_widget_rect(inside, b), Some(inside));
    }

    fn rgb(r: u8, g: u8, b: u8) -> pixtuoid_core::sprite::Rgb {
        pixtuoid_core::sprite::Rgb { r, g, b }
    }

    const HALF_BLOCK: &str = "\u{2580}";

    /// The flush and [`CellArea::half_block`] are two copies of one mapping:
    /// the pixels a cell's half-blocks carry are exactly the ones its area
    /// covers.
    #[test]
    fn a_cells_area_is_the_pixels_its_half_blocks_carry() {
        use pixtuoid_scene::layout::Point;
        let (w, h) = (4u16, 6u16);
        let mut buf = RgbBuffer::filled(w, h, rgb(0, 0, 0));
        for (i, px) in buf.as_mut_slice().iter_mut().enumerate() {
            *px = rgb((i % usize::from(w)) as u8, (i / usize::from(w)) as u8, 1);
        }
        let mut term =
            Terminal::new(ratatui::backend::TestBackend::new(w, h / 2)).expect("test backend");
        let rect = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h / 2,
        };
        term.draw(|f| flush_buffer_to_term(f, &buf, rect))
            .expect("draw");
        let shows = |area: CellArea, x: u16, y: u16| area.overlaps(Point { x, y }, 1, 1);
        for row in 0..h / 2 {
            for col in 0..w {
                let cell = term.backend().buffer().cell((col, row)).expect("cell");
                let area = CellArea::half_block(col, row);
                for color in [cell.fg, cell.bg] {
                    let Color::Rgb(x, y, _) = color else {
                        panic!("cell ({col},{row}) carries {color:?}");
                    };
                    assert!(
                        shows(area, x.into(), y.into()),
                        "cell ({col},{row}) carries ({x},{y})"
                    );
                }
                let covered = (0..h)
                    .flat_map(|y| (0..w).map(move |x| (x, y)))
                    .filter(|&(x, y)| shows(area, x, y))
                    .count();
                assert_eq!(covered, 2, "cell ({col},{row}) covers its two half-blocks");
            }
        }
    }

    #[test]
    fn flush_skips_columns_past_scene_right_edge() {
        let mut term =
            Terminal::new(ratatui::backend::TestBackend::new(10, 6)).expect("test backend");
        // buf is WIDER (6) than the scene rect (width 4); cell_rows = 4/2 = 2.
        let buf = RgbBuffer::filled(6, 4, rgb(10, 20, 30));
        let rect = Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 2,
        };
        term.draw(|f| flush_buffer_to_term(f, &buf, rect))
            .expect("draw");
        let term_buf = term.backend().buffer();
        for x in 0..4u16 {
            assert_eq!(
                term_buf.cell((x, 0)).unwrap().symbol(),
                HALF_BLOCK,
                "column {x} is inside the rect and must be painted"
            );
        }
        for x in 4..6u16 {
            assert_ne!(
                term_buf.cell((x, 0)).unwrap().symbol(),
                HALF_BLOCK,
                "column {x} is past the rect's right edge and must be skipped"
            );
        }
    }

    #[test]
    fn flush_skips_cells_past_terminal_bounds() {
        let mut term =
            Terminal::new(ratatui::backend::TestBackend::new(4, 3)).expect("test backend");
        // buf 8x8 (cell_rows = 4) flushed into a rect that EXCEEDS the 4x3 backend.
        let buf = RgbBuffer::filled(8, 8, rgb(40, 50, 60));
        let rect = Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 6,
        };
        term.draw(|f| flush_buffer_to_term(f, &buf, rect))
            .expect("draw must not panic on an oversized rect");
        let term_buf = term.backend().buffer();
        for y in 0..3u16 {
            for x in 0..4u16 {
                assert_eq!(
                    term_buf.cell((x, y)).unwrap().symbol(),
                    HALF_BLOCK,
                    "in-bounds cell ({x},{y}) must be painted"
                );
            }
        }
    }
}
