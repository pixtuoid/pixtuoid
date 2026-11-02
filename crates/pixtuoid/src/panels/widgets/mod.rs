//! Ratatui widget paint functions.

mod connection;
mod dashboard;
mod footer;
mod help;
mod panel;
mod theme_picker;
mod tooltip;
mod version_popup;
mod welcome;

pub(crate) use connection::{fmt_age, paint_connection_panel};
pub(crate) use dashboard::paint_dashboard;
pub use footer::footer_context;
pub(crate) use footer::paint_footer;
pub(crate) use help::{Shortcut, paint_help_overlay};
pub(crate) use panel::{Overflow, Panel, PanelGeometry, borderless_panel};
pub(crate) use theme_picker::paint_theme_picker;
pub(crate) use tooltip::{TooltipAt, paint_host_text, paint_tooltip, star_area};
pub(crate) use version_popup::{paint_version_popup, release_url, version_popup_url_rect};
pub(crate) use welcome::paint_welcome;

use std::time::SystemTime;

use pixtuoid_scene::sprite::Rgb;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Clear};

use pixtuoid_scene::display::cells::{CARD_SHADOW, CellGrid};
use pixtuoid_scene::theme::Theme;

fn to_color(c: Rgb) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

#[cfg(test)]
pub(crate) use pixtuoid_scene::footer::RungKind as StateKind;

/// Write `grid` into `buf` with its top-left at `at`, only where it falls in
/// `clip`: screen text as terminal cells. The cells a wide cluster covers are
/// reset, as ratatui's own `Buffer::set_stringn` leaves them.
pub(crate) fn put_grid(
    buf: &mut ratatui::buffer::Buffer,
    grid: &CellGrid,
    at: (u16, u16),
    clip: Rect,
) {
    let clip = clip.intersection(buf.area);
    for gy in 0..grid.height() {
        for gx in 0..grid.width() {
            let (x, y) = (at.0.saturating_add(gx), at.1.saturating_add(gy));
            let Some(c) = grid.get(gx, gy) else { continue };
            if x < clip.x || y < clip.y || x >= clip.right() || y >= clip.bottom() {
                continue;
            }
            let cell = &mut buf[(x, y)];
            cell.reset();
            if c.symbol.is_empty() {
                continue;
            }
            cell.set_symbol(&c.symbol);
            if let Some(fg) = c.fg {
                cell.fg = to_color(fg);
            }
            if let Some(bg) = c.bg {
                cell.bg = to_color(bg);
            }
            if c.bold {
                cell.modifier.insert(ratatui::style::Modifier::BOLD);
            }
        }
    }
}

/// `area`'s cells of `buf` as a [`CellGrid`], `put_grid`'s inverse: what a
/// painter with no terminal draws the same cells from. A colour the terminal
/// picks (`Reset`, `Indexed`) reads as the painter's own, and a cell a wide
/// cluster covers, which ratatui resets, as [`CellGrid::put`] writes it: empty,
/// in the cluster's colours.
pub fn grid_of(buf: &ratatui::buffer::Buffer, area: Rect) -> CellGrid {
    use pixtuoid_scene::display::cells::GridCell;
    let area = area.intersection(buf.area);
    let mut grid = CellGrid::new(area.width, area.height);
    for y in 0..area.height {
        let mut wide: Option<(GridCell, u16)> = None;
        for x in 0..area.width {
            let cell = match wide.take() {
                Some((covering, left)) => {
                    let covered = GridCell {
                        symbol: String::new(),
                        ..covering.clone()
                    };
                    if left > 1 {
                        wide = Some((covering, left - 1));
                    }
                    covered
                }
                None => {
                    let at = &buf[(area.x + x, area.y + y)];
                    let read = GridCell {
                        symbol: at.symbol().to_owned(),
                        fg: rgb_of(at.fg),
                        bg: rgb_of(at.bg),
                        bold: at.modifier.contains(ratatui::style::Modifier::BOLD),
                    };
                    let extra = pixtuoid_scene::display::text::cells(at.symbol()).saturating_sub(1);
                    if extra > 0 {
                        wide = Some((read.clone(), extra));
                    }
                    read
                }
            };
            grid.set((x, y), cell);
        }
    }
    grid
}

/// The colour a cell's `c` names, `None` where the terminal picks it.
fn rgb_of(c: Color) -> Option<Rgb> {
    let rgb = |r, g, b| Some(Rgb { r, g, b });
    match c {
        Color::Rgb(r, g, b) => rgb(r, g, b),
        // The named colours at xterm's defaults (`XTerm-col.ad`, color0-15).
        Color::Black => rgb(0, 0, 0),
        Color::Red => rgb(205, 0, 0),
        Color::Green => rgb(0, 205, 0),
        Color::Yellow => rgb(205, 205, 0),
        Color::Blue => rgb(0, 0, 238),
        Color::Magenta => rgb(205, 0, 205),
        Color::Cyan => rgb(0, 205, 205),
        Color::Gray => rgb(229, 229, 229),
        Color::DarkGray => rgb(127, 127, 127),
        Color::LightRed => rgb(255, 0, 0),
        Color::LightGreen => rgb(0, 255, 0),
        Color::LightYellow => rgb(255, 255, 0),
        Color::LightBlue => rgb(92, 92, 255),
        Color::LightMagenta => rgb(255, 0, 255),
        Color::LightCyan => rgb(0, 255, 255),
        Color::White => rgb(255, 255, 255),
        Color::Reset | Color::Indexed(_) => None,
    }
}

/// How far the shadow silhouette is offset down-and-right of the card, in cells — what
/// makes it read as a cast box-shadow (the card floats above it) rather than an outline.
const SHADOW_OFFSET: u16 = 1;

/// An `Rgb` color under `dim`; a named one is the terminal's, so it stays.
fn dim_color(c: Color, dim: pixtuoid_scene::look::Dim) -> Color {
    match c {
        Color::Rgb(r, g, b) => {
            let pixtuoid_scene::sprite::Rgb { r, g, b } =
                dim.of(pixtuoid_scene::sprite::Rgb { r, g, b });
            Color::Rgb(r, g, b)
        }
        other => other,
    }
}

/// Darken the cell at `(x, y)` by the uniform [`CARD_SHADOW`], if it is a real `Rgb` and
/// inside `bounds`. Half-block office cells carry a real RGB on BOTH `fg` (top sub-pixel)
/// and `bg` (bottom sub-pixel), so a clean shadow darkens both — ratatui's own
/// `Block::shadow` tints bg-only and smears over the pixel art. With `top_half_only`,
/// darkens only the upper half-block sub-pixel (`fg`) and leaves the lower one lit — a
/// 1px-tall line.
fn dim_cell(f: &mut ratatui::Frame<'_>, x: u16, y: u16, bounds: Rect, top_half_only: bool) {
    if x < bounds.x || y < bounds.y || x >= bounds.right() || y >= bounds.bottom() {
        return;
    }
    let cell = &mut f.buffer_mut()[(x, y)];
    let shadow = pixtuoid_scene::look::Dim::from_brightness(CARD_SHADOW);
    cell.fg = dim_color(cell.fg, shadow);
    if !top_half_only {
        cell.bg = dim_color(cell.bg, shadow);
    }
}

/// Cast a flat, single-color drop shadow: the card's own silhouette darkened by one
/// uniform [`CARD_SHADOW`] and offset `SHADOW_OFFSET` cells down-and-right. The
/// bottom-most row of the silhouette is rendered TOP-HALF only, so the bottom shadow
/// reads as a 1px contact line instead of a full 2px cell.
///
/// Clipped to `scene_rect`, NOT the frame: the footer is painted before every card and
/// the bottom band dims `fg` only, so a band reaching that row repaints the live `[q]uit`
/// over its still-lit bg. Keeping the card BODY off the footer
/// (`panel::RESERVED_FOOTER_ROWS`) is only half the rule — the silhouette is offset a row
/// further DOWN.
fn cast_drop_shadow(f: &mut ratatui::Frame<'_>, area: Rect) {
    let bounds = crate::panels::scene_rect(f.area());
    let sx = area.x.saturating_add(SHADOW_OFFSET);
    let sy = area.y.saturating_add(SHADOW_OFFSET);
    let last_row = sy.saturating_add(area.height.saturating_sub(1));
    for y in sy..sy.saturating_add(area.height) {
        let top_half_only = y == last_row;
        for x in sx..sx.saturating_add(area.width) {
            dim_cell(f, x, y, bounds, top_half_only);
        }
    }
}

/// Paint the shared backing for a borderless modal card over `area`: drop shadow, `Clear`,
/// then a solid `tooltip_bg` fill. Tooltips fill themselves in `Tooltip::card` and cast the
/// same [`cast_drop_shadow`].
fn paint_card_backing(f: &mut ratatui::Frame<'_>, area: Rect, theme: &Theme) {
    cast_drop_shadow(f, area);
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default().style(Style::default().bg(to_color(theme.ui.tooltip_bg))),
        area,
    );
}

/// [`Theme::source_hue`] as a cell colour.
fn badge_color_for(tag: &str, theme: &Theme) -> Color {
    to_color(theme.source_hue(tag))
}

/// The `[xx]` two-letter source badge span, coloured by the source's theme hue. Never
/// REVERSED — a low-luminance hue inverted vanishes against a highlight bg, so callers
/// reverse the OTHER spans (name/state) on selection, never this one.
pub(crate) fn source_badge_span(tag: &str, theme: &Theme) -> ratatui::text::Span<'static> {
    ratatui::text::Span::styled(
        format!("[{tag:<2}]"),
        Style::default().fg(badge_color_for(tag, theme)),
    )
}

/// Truncate to `max` characters (char-safe), appending `…` when clipped. The `…` is
/// INCLUDED in the budget, so the clipped output is EXACTLY `max` chars — unlike
/// `decoder::ellipsize`, which excludes it (N+1).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('\u{2026}');
    out
}

/// Time (ms) the marquee dwells on each character while scrolling.
const MARQUEE_MS_PER_CHAR: u64 = 150;
/// Time (ms) the marquee holds at each end (head / tail) before reversing.
const MARQUEE_END_PAUSE_MS: u64 = 1200;

/// Visible char-window of `s` for a ping-pong auto-scrolling field `width` columns wide,
/// at time `now`. If `s` fits, it is returned unchanged. Otherwise it bounces — hold head
/// → scroll to tail → hold tail → scroll back — purely as a function of `now`, with NO
/// per-frame state, so two painters can call it freely. Char-windowed like `truncate` (a
/// wide CJK glyph would misalign by a column mid-scroll); unlike `truncate` it emits NO
/// `…` — the motion signals "more".
fn marquee_window(s: &str, width: usize, now: SystemTime) -> String {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    if len <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let max_off = len - width; // >= 1
    let scroll_ms = max_off as u64 * MARQUEE_MS_PER_CHAR; // >= MARQUEE_MS_PER_CHAR
    let pause = MARQUEE_END_PAUSE_MS;
    let cycle = 2 * pause + 2 * scroll_ms; // > 0
    let elapsed = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let phase = elapsed % cycle;
    let off = if phase < pause {
        0 // hold head
    } else if phase < pause + scroll_ms {
        (((phase - pause) / MARQUEE_MS_PER_CHAR) as usize).min(max_off) // scroll out
    } else if phase < 2 * pause + scroll_ms {
        max_off // hold tail
    } else {
        let back = (phase - (2 * pause + scroll_ms)) / MARQUEE_MS_PER_CHAR;
        max_off.saturating_sub(back as usize) // scroll back
    };
    chars[off..off + width].iter().collect()
}

/// The focused (selected) row auto-scrolls overflowing text via ping-pong; every other
/// row stays statically `…`-truncated. Both honor the same `width` contract, so the
/// caller's fixed-width padding is unchanged.
fn marquee_or_truncate(s: &str, width: usize, selected: bool, now: SystemTime) -> String {
    if selected {
        marquee_window(s, width, now)
    } else {
        truncate(s, width)
    }
}

#[cfg(test)]
mod tests;
