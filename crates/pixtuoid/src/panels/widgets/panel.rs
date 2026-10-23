//! The shared borderless modal frame for every popup. NO border — legibility
//! over the busy pixel office beats the outline.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{paint_card_backing, to_color};
use pixtuoid_scene::theme::Theme;

/// Uniform inner padding: the breathing room standing in for the removed border.
const PANEL_PAD_X: u16 = 2;
const PANEL_PAD_Y: u16 = 1;

/// Minimum renderable envelope: below this the panel paints nothing — nothing is
/// legible under 4×3, and `Clear::render` panics indexing past a narrower buffer.
const PANEL_MIN_W: u16 = 4;
const PANEL_MIN_H: u16 = 3;

/// Rows at the bottom of `bounds` a panel may never occupy: the footer, the one
/// persistent affordance (`[q]uit`, `[?]help`). Without it a panel taller than
/// the terminal clamps to `bounds.height` and paints straight over it. Reserved
/// HERE rather than by shrinking each caller's `bounds`, because centering in a
/// shorter box would also move every panel that already fits.
const RESERVED_FOOTER_ROWS: u16 = crate::panels::FOOTER_ROWS;

/// Inner content `Rect`: `outer` inset by `PANEL_PAD_*` with the title row (when
/// present) dropped; the raw area when `outer` is too small to inset. Both
/// `borderless_panel` (returns it) and [`PanelGeometry::inner`] (reads it) go
/// through here, so the painted rect and any geometry query cannot drift.
fn inner_rect(outer: Rect, has_title: bool) -> Rect {
    if outer.width <= PANEL_PAD_X * 2 || outer.height <= PANEL_PAD_Y * 2 {
        return outer;
    }
    let mut inner = Rect {
        x: outer.x + PANEL_PAD_X,
        y: outer.y + PANEL_PAD_Y,
        width: outer.width - PANEL_PAD_X * 2,
        height: outer.height - PANEL_PAD_Y * 2,
    };
    if has_title && inner.height >= 1 {
        inner.y += 1;
        inner.height -= 1;
    }
    inner
}

/// THE pure geometry authority for a centered borderless popup: the scaled,
/// bounds-clamped, guarded envelope + inner content rect + a content-cell →
/// screen-rect mapping. No `Frame`, so the paint/click lockstep is a pure
/// arithmetic test. Painter and click-target read the SAME value, so they cannot
/// drift.
pub(crate) struct PanelGeometry {
    outer: Rect,
    inner: Rect,
}

impl PanelGeometry {
    /// `content_rows` is the content BELOW the title; the title row is added here.
    /// `None` when the scaled envelope is under the minimum.
    pub(crate) fn compute(
        bounds: Rect,
        content_w: u16,
        content_rows: u16,
        title: Option<&str>,
        scale: f32,
    ) -> Option<Self> {
        let scale = scale.clamp(0.0, 1.0);
        let full_w = content_w.saturating_add(2 * PANEL_PAD_X).min(bounds.width);
        let full_h = content_rows
            .saturating_add(u16::from(title.is_some()))
            .saturating_add(2 * PANEL_PAD_Y)
            .min(bounds.height.saturating_sub(RESERVED_FOOTER_ROWS));
        let w = (f32::from(full_w) * scale).round() as u16;
        let h = (f32::from(full_h) * scale).round() as u16;
        if w < PANEL_MIN_W || h < PANEL_MIN_H {
            return None;
        }
        let outer = Rect {
            x: bounds.x + bounds.width.saturating_sub(w) / 2,
            y: bounds.y + bounds.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        Some(Self {
            outer,
            inner: inner_rect(outer, title.is_some()),
        })
    }

    pub(crate) fn outer(&self) -> Rect {
        self.outer
    }

    pub(crate) fn inner(&self) -> Rect {
        self.inner
    }

    /// Map a content cell — `row` below the title, `col`/`len` chars within
    /// `inner` — to a clipped screen `Rect`.
    pub(crate) fn cell_rect(&self, row: u16, col: u16, len: u16) -> Option<Rect> {
        let inner = self.inner;
        let x = inner.x + col;
        let y = inner.y + row;
        if x >= inner.right() || y >= inner.bottom() {
            return None;
        }
        let width = len.min(inner.right() - x);
        if width == 0 {
            return None;
        }
        Some(Rect {
            x,
            y,
            width,
            height: 1,
        })
    }
}

/// The inner content WIDTH alone — height-independent, so a width-dependent row
/// builder (word-wrap, marquee) can size before the row count is known. Pinned to
/// agree with `compute(..)`'s `inner().width` by a test.
fn panel_inner_width(bounds: Rect, content_w: u16, scale: f32) -> Option<u16> {
    let scale = scale.clamp(0.0, 1.0);
    let full_w = content_w.saturating_add(2 * PANEL_PAD_X).min(bounds.width);
    let w = (f32::from(full_w) * scale).round() as u16;
    if w < PANEL_MIN_W {
        return None;
    }
    Some(if w <= PANEL_PAD_X * 2 {
        w
    } else {
        w - PANEL_PAD_X * 2
    })
}

/// The visible slice of a scrollable list + the hidden-below count for the cue.
pub(crate) struct ListWindow {
    pub(crate) start: usize,
    pub(crate) count: usize,
    pub(crate) cue: Option<usize>,
}

/// Window a `list_len`-row list into a `viewport`-row region, following `selected`
/// from `scroll`. On overflow, reserve the last line for the `⋮ N more ▾` cue —
/// UNLESS the selection has reached the end (nothing below), then use the full
/// viewport and drop the cue.
pub(crate) fn window_range(
    list_len: usize,
    selected: Option<usize>,
    scroll: usize,
    viewport: usize,
) -> ListWindow {
    use crate::panels::dashboard::clamp_scroll_idx;
    let overflow = list_len > viewport;
    let reserved = if overflow {
        viewport.saturating_sub(1)
    } else {
        viewport
    };
    let probe = clamp_scroll_idx(selected, scroll, reserved);
    let show_cue = overflow && list_len > probe + reserved;
    let window = if show_cue { reserved } else { viewport };
    let start = clamp_scroll_idx(selected, scroll, window);
    ListWindow {
        start,
        count: list_len.saturating_sub(start).min(window),
        // guarded arithmetic → `.then(||)` (lazy), not `then_some` (eager).
        cue: show_cue.then(|| list_len.saturating_sub(start + window)),
    }
}

pub(crate) fn overflow_cue(hidden: usize) -> String {
    format!("  \u{22ee} {hidden} more \u{25be}")
}

/// Paint a borderless panel over `area` and return the content `Rect` (the padded
/// region, below the title row when one is drawn). No borders are ever drawn.
pub(crate) fn borderless_panel(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    title: Option<&str>,
    theme: &Theme,
) -> Rect {
    paint_card_backing(f, area, theme);
    let bg = Style::default().bg(to_color(theme.ui.tooltip_bg));
    // Too small to pad — hand back the raw area rather than underflow.
    if area.width <= PANEL_PAD_X * 2 || area.height <= PANEL_PAD_Y * 2 {
        return area;
    }
    // Past the early return the padded region is ≥1 row tall, so the 1-row title
    // always fits.
    if let Some(t) = title {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t.to_string(),
                Style::default()
                    .fg(to_color(theme.ui.neon_brand))
                    .add_modifier(Modifier::BOLD),
            )))
            .style(bg),
            Rect {
                x: area.x + PANEL_PAD_X,
                y: area.y + PANEL_PAD_Y,
                width: area.width - PANEL_PAD_X * 2,
                height: 1,
            },
        );
    }
    inner_rect(area, title.is_some())
}

/// How [`Panel::paint`] treats the windowed `list` band.
pub(crate) enum Overflow {
    /// Selection-follow window + cue. `cap` limits the visible list rows
    /// regardless of terminal height; `None` fills.
    Follow {
        selected: Option<usize>,
        scroll: usize,
        cap: Option<u16>,
    },
    /// Window from the top with a cue on overflow; no selection.
    CueOnly,
    /// Render the whole list as-is, sized to fit.
    None,
}

/// One list-bearing popup: callers hand PRE-STYLED lines. The three bands are
/// named fields because they share one type, so a positional swap would compile.
pub(crate) struct Panel<'a> {
    pub(crate) title: Option<&'a str>,
    /// Content width in cells, before padding.
    pub(crate) content_w: u16,
    pub(crate) above: Vec<Line<'static>>,
    /// The band windowed into the space between `above` and `below`.
    pub(crate) list: Vec<Line<'static>>,
    pub(crate) below: Vec<Line<'static>>,
    pub(crate) overflow: Overflow,
}

impl Panel<'_> {
    /// A panel never animates its size; only the version popup, which paints
    /// through [`PanelGeometry`] itself, does.
    const SCALE: f32 = 1.0;

    /// The inner content WIDTH of a panel `content_w` wide, before its bands
    /// exist — for a width-dependent row builder (word-wrap, marquee).
    pub(crate) fn inner_width(bounds: Rect, content_w: u16) -> Option<u16> {
        panel_inner_width(bounds, content_w, Self::SCALE)
    }

    /// Frame (backing, title), window the `list` band, and append the overflow
    /// cue. Auto-heights to the ACTUAL band lengths, so no caller-side row count
    /// can drift from the lines pushed.
    pub(crate) fn paint(self, f: &mut ratatui::Frame<'_>, bounds: Rect, theme: &Theme) {
        let Panel {
            title,
            content_w,
            above,
            list,
            below,
            overflow,
        } = self;
        let cap = match &overflow {
            Overflow::Follow { cap, .. } => *cap,
            Overflow::CueOnly | Overflow::None => None,
        };
        let list_size_rows = cap.map_or(list.len(), |c| list.len().min(c as usize));
        let content_rows = (above.len() + list_size_rows + below.len()) as u16;
        let Some(geom) =
            PanelGeometry::compute(bounds, content_w, content_rows, title, Self::SCALE)
        else {
            return;
        };
        let inner = borderless_panel(f, geom.outer(), title, theme);

        let viewport = (inner.height as usize).saturating_sub(above.len() + below.len());
        let win = match &overflow {
            Overflow::None => ListWindow {
                start: 0,
                count: list.len().min(viewport),
                cue: None,
            },
            Overflow::CueOnly => window_range(list.len(), Option::None, 0, viewport),
            Overflow::Follow {
                selected, scroll, ..
            } => window_range(list.len(), *selected, *scroll, viewport),
        };

        let mut lines: Vec<Line<'static>> = Vec::with_capacity(inner.height as usize);
        lines.extend(above);
        lines.extend(list.into_iter().skip(win.start).take(win.count));
        if let Some(hidden) = win.cue {
            lines.push(Line::from(Span::styled(
                overflow_cue(hidden),
                Style::default().fg(to_color(theme.ui.label_idle)),
            )));
        }
        lines.extend(below);
        f.render_widget(Paragraph::new(lines), inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_to_string(w: u16, h: u16, title: Option<&str>) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            borderless_panel(
                f,
                Rect::new(0, 0, w, h),
                title,
                &pixtuoid_scene::theme::NORMAL,
            );
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..h {
            for x in 0..w {
                if let Some(cell) = buf.cell((x, y)) {
                    s.push_str(cell.symbol());
                }
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn borderless_panel_has_no_border_glyphs_and_renders_title() {
        let s = render_to_string(40, 8, Some("Connection"));
        for g in ['╭', '╮', '╰', '╯', '│', '─', '┌', '┐', '└', '┘'] {
            assert!(!s.contains(g), "panel must be borderless, found {g:?}");
        }
        assert!(s.contains("Connection"), "title must render in the body");
    }

    #[test]
    fn borderless_panel_returns_padded_inner_below_the_title_row() {
        let mut term = Terminal::new(TestBackend::new(20, 6)).unwrap();
        let mut inner = Rect::default();
        term.draw(|f| {
            inner = borderless_panel(
                f,
                Rect::new(0, 0, 20, 6),
                Some("X"),
                &pixtuoid_scene::theme::NORMAL,
            );
        })
        .unwrap();
        assert_eq!(inner.x, PANEL_PAD_X);
        assert_eq!(
            inner.y,
            PANEL_PAD_Y + 1,
            "content starts below the title row"
        );
        assert_eq!(inner.width, 20 - PANEL_PAD_X * 2);
        assert_eq!(inner.height, 6 - PANEL_PAD_Y * 2 - 1);
        term.draw(|f| {
            inner = borderless_panel(
                f,
                Rect::new(0, 0, 20, 6),
                None,
                &pixtuoid_scene::theme::NORMAL,
            );
        })
        .unwrap();
        assert_eq!(inner.y, PANEL_PAD_Y);
        assert_eq!(inner.height, 6 - PANEL_PAD_Y * 2);
    }

    #[test]
    fn borderless_panel_never_panics_across_sizes() {
        for (w, h) in [(80, 20), (40, 8), (10, 3), (4, 2), (2, 1)] {
            let _ = render_to_string(w, h, Some("T"));
            let _ = render_to_string(w, h, None);
        }
    }

    #[test]
    fn borderless_panel_casts_a_flat_offset_shadow() {
        use ratatui::style::Color;
        let bright = Color::Rgb(200, 200, 200);
        let dim = (200.0 * pixtuoid_scene::display::cells::CARD_SHADOW) as u8;
        assert!(dim < 200, "CARD_SHADOW must actually darken");
        let area = Rect::new(5, 4, 8, 4); // small, well inside the 20x12 buffer
        let mut term = Terminal::new(TestBackend::new(20, 12)).unwrap();
        term.draw(|f| {
            let full = f.area();
            for y in 0..full.height {
                for x in 0..full.width {
                    let cell = &mut f.buffer_mut()[(x, y)];
                    cell.set_symbol("\u{2580}");
                    cell.fg = bright;
                    cell.bg = bright;
                }
            }
            borderless_panel(f, area, None, &pixtuoid_scene::theme::NORMAL);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let chan = |c: Color| match c {
            Color::Rgb(r, _, _) => r,
            other => panic!("expected Rgb, got {other:?}"),
        };
        let r = |x: u16, y: u16| chan(buf.cell((x, y)).unwrap().bg);
        let rf = |x: u16, y: u16| chan(buf.cell((x, y)).unwrap().fg);
        assert_eq!(rf(area.right(), area.y + 1), dim, "right band top px dim");
        assert_eq!(r(area.right(), area.y + 1), dim, "right band bottom px dim");
        assert_eq!(rf(area.x + 1, area.bottom()), dim, "bottom band top px dim");
        assert_eq!(
            r(area.x + 1, area.bottom()),
            200,
            "bottom band bottom px stays lit (a 1px line, not a full cell)"
        );
        assert_eq!(
            rf(area.right(), area.y + 1),
            rf(area.x + 1, area.bottom()),
            "right and bottom bands are ONE uniform color"
        );
        assert_eq!(rf(area.right(), area.bottom()), dim, "corner top px dim");
        assert_eq!(r(area.right(), area.bottom()), 200, "corner bottom px lit");
        assert_eq!(
            r(area.right() + 1, area.y + 1),
            200,
            "right band is 1 cell wide (the next column is lit)"
        );
        assert_eq!(
            r(area.right(), area.y),
            200,
            "top-right corner stays lit (offset down)"
        );
        assert_eq!(
            r(area.x, area.bottom()),
            200,
            "bottom-left corner stays lit (offset right)"
        );
        assert_eq!(
            r(area.x, area.y.saturating_sub(1)),
            200,
            "no shadow above the card"
        );
        assert_eq!(
            r(area.x.saturating_sub(1), area.y),
            200,
            "no shadow left of the card"
        );
        assert_eq!(r(0, 0), 200, "cells outside the band stay bright");
    }

    #[test]
    fn a_card_at_the_scene_floor_casts_no_shadow_onto_the_footer_row() {
        use ratatui::style::Color;
        let bright = Color::Rgb(200, 200, 200);
        let (w, h) = (20u16, 12u16);
        let footer_y = h - crate::panels::FOOTER_ROWS;
        let area = Rect::new(2, footer_y - 4, 8, 4);
        assert_eq!(
            area.bottom(),
            footer_y,
            "the card ends right above the footer"
        );
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            let full = f.area();
            for y in 0..full.height {
                for x in 0..full.width {
                    let cell = &mut f.buffer_mut()[(x, y)];
                    cell.set_symbol("\u{2580}");
                    cell.fg = bright;
                    cell.bg = bright;
                }
            }
            borderless_panel(f, area, None, &pixtuoid_scene::theme::NORMAL);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        for x in 0..w {
            let cell = buf.cell((x, footer_y)).expect("footer cell");
            assert_eq!(
                (cell.fg, cell.bg),
                (bright, bright),
                "the footer row must be untouched at x={x}, got {cell:?}"
            );
        }
        assert!(
            matches!(buf.cell((area.right(), area.y + 1)).expect("cell").fg, Color::Rgb(r, _, _) if r < 200),
            "the right band still casts above the footer"
        );
    }

    #[test]
    fn geometry_guard_trips_below_min_and_at_zero_scale() {
        let b = Rect::new(0, 0, 100, 50);
        let g = PanelGeometry::compute(b, 20, 5, Some("t"), 1.0).expect("renders");
        assert!(g.cell_rect(0, 0, 3).is_some());
        assert!(PanelGeometry::compute(b, 20, 5, Some("t"), 0.0).is_none());
        assert!(PanelGeometry::compute(Rect::new(0, 0, 3, 50), 20, 5, Some("t"), 1.0).is_none());
        assert!(PanelGeometry::compute(Rect::new(0, 0, 100, 2), 20, 5, Some("t"), 1.0).is_none());
    }

    #[test]
    fn a_too_tall_panel_stops_one_row_short_of_the_footer() {
        let b = Rect::new(0, 0, 40, 20);
        let tall = PanelGeometry::compute(b, 30, 100, Some("t"), 1.0)
            .expect("renders")
            .outer();
        // Asserted against the FOOTER's own authority: `== b.bottom() -
        // RESERVED_FOOTER_ROWS` restates the constant under test and can't fail.
        let footer_row = b.bottom() - crate::panels::FOOTER_ROWS;
        assert!(
            tall.bottom() <= footer_row,
            "a clamped panel must leave the footer row (y={footer_row}) free, got {tall:?}"
        );
        let fits = PanelGeometry::compute(b, 30, 5, Some("t"), 1.0)
            .expect("renders")
            .outer();
        assert_eq!(
            fits.y,
            (b.height - fits.height) / 2,
            "a fitting panel stays centered in the FULL height — centering it in \
             a footer-shortened box would move every committed modal still"
        );
    }

    #[test]
    fn geometry_inner_is_padded_and_drops_the_title_row() {
        let b = Rect::new(0, 0, 100, 50);
        let g = PanelGeometry::compute(b, 20, 5, Some("t"), 1.0).expect("renders");
        assert_eq!(g.outer(), Rect::new(38, 21, 24, 8));
        assert_eq!(g.inner(), Rect::new(40, 23, 20, 5));
        let u = PanelGeometry::compute(b, 20, 5, None, 1.0).expect("renders");
        assert_eq!(u.outer(), Rect::new(38, 21, 24, 7));
        assert_eq!(u.inner(), Rect::new(40, 22, 20, 5));
    }

    #[test]
    fn geometry_scale_centers_off_the_scaled_dims() {
        let b = Rect::new(0, 0, 100, 50);
        let g = PanelGeometry::compute(b, 40, 10, Some("t"), 0.5).expect("renders");
        assert_eq!(g.outer(), Rect::new(39, 21, 22, 7));
    }

    #[test]
    fn geometry_cell_rect_maps_and_clips() {
        let b = Rect::new(0, 0, 100, 50);
        let g = PanelGeometry::compute(b, 20, 5, Some("t"), 1.0).expect("renders");
        assert_eq!(g.cell_rect(0, 0, 5), Some(Rect::new(40, 23, 5, 1)));
        assert_eq!(g.cell_rect(2, 3, 4), Some(Rect::new(43, 25, 4, 1)));
        assert_eq!(g.cell_rect(0, 18, 10), Some(Rect::new(58, 23, 2, 1)));
        assert_eq!(g.cell_rect(0, 20, 5), None);
        assert_eq!(g.cell_rect(5, 0, 3), None);
    }

    #[test]
    fn panel_inner_width_agrees_with_compute_inner_width() {
        let b = Rect::new(0, 0, 100, 50);
        for &cw in &[8u16, 20, 60] {
            for &s in &[1.0f32, 0.9, 0.5] {
                // `content_rows` tall enough that the height guard passes at
                // every scale, so the only difference is height-independence.
                let inner_w =
                    PanelGeometry::compute(b, cw, 20, Some("t"), s).map(|g| g.inner().width);
                assert_eq!(panel_inner_width(b, cw, s), inner_w, "cw={cw} scale={s}");
            }
        }
    }

    #[test]
    fn inner_rect_raw_fallback_at_the_min_width() {
        let raw = inner_rect(Rect::new(0, 0, PANEL_PAD_X * 2, 10), true);
        assert_eq!(raw, Rect::new(0, 0, PANEL_PAD_X * 2, 10));
        assert_eq!(
            inner_rect(Rect::new(0, 0, PANEL_PAD_X * 2 + 1, 10), false).width,
            1
        );
    }

    #[test]
    fn window_range_fits_without_a_cue() {
        let w = window_range(4, Some(0), 0, 10);
        assert_eq!((w.start, w.count, w.cue), (0, 4, None));
    }

    #[test]
    fn window_range_reserves_a_line_for_the_cue_when_overflowing() {
        let w = window_range(20, Some(0), 0, 6);
        assert_eq!((w.start, w.count), (0, 5));
        assert_eq!(w.cue, Some(15));
    }

    #[test]
    fn window_range_drops_the_cue_when_selection_reaches_the_end() {
        let w = window_range(20, Some(19), 0, 6);
        assert_eq!(w.cue, None);
        assert_eq!((w.start, w.count), (14, 6));
    }

    #[test]
    fn window_range_follows_selection_below_the_window() {
        let w = window_range(30, Some(12), 0, 8);
        assert_eq!((w.start, w.count), (6, 7));
        assert_eq!(w.cue, Some(30 - (6 + 7)));
    }

    #[test]
    fn overflow_cue_text_is_the_shared_vocabulary() {
        assert_eq!(overflow_cue(3), "  \u{22ee} 3 more \u{25be}");
    }

    #[test]
    fn panel_windows_a_long_list_and_pins_the_chrome() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        term.draw(|f| {
            let above = vec![Line::from("HEADERLINE")];
            let below = vec![Line::from("FOOTERLINE")];
            let list: Vec<Line<'static>> =
                (0..20).map(|i| Line::from(format!("row{i:02}"))).collect();
            Panel {
                title: Some("T"),
                content_w: 30,
                above,
                list,
                below,
                overflow: Overflow::Follow {
                    selected: Some(0),
                    scroll: 0,
                    cap: None,
                },
            }
            .paint(f, Rect::new(0, 0, 40, 12), &pixtuoid_scene::theme::NORMAL);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut text = String::new();
        for y in 0..12 {
            for x in 0..40 {
                text.push_str(buf.cell((x, y)).unwrap().symbol());
            }
        }
        assert!(text.contains("HEADERLINE"), "above chrome must stay pinned");
        assert!(text.contains("FOOTERLINE"), "below chrome must stay pinned");
        assert!(text.contains("more"), "overflow cue must show on overflow");
        assert!(text.contains("row00"), "the windowed list top shows");
        assert!(!text.contains("row19"), "far list rows are windowed out");
    }
}
