//! Text as the display list lays it out: a grapheme cluster takes the cells
//! ratatui's buffer writes for it, each `ADVANCE` art pixels wide on a line
//! `LINE_H` tall. At the pack's 4x art a cell is one logical column, the
//! classic badge's terminal column. The rasterizer's font draws into these
//! cells.

use pixtuoid_core::sprite::Rgb;
use pixtuoid_core::{AgentId, AgentSlot};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::display::Icon;
use crate::display::cells::{CellMap, CellRect};
use crate::display::pen::ArtPx;
use crate::layout::Point;
use crate::theme::Theme;

/// A glyph's width in art pixels, its cell less the gap.
pub(crate) const GLYPH_W: u16 = 3;
/// A cell's width in art pixels: one glyph and its gap.
pub(crate) const ADVANCE: u16 = GLYPH_W + 1;
/// The row above the capitals, which only a lowercase letter's tall accent
/// (`ã`, `å`) inks: an accented capital keeps to the capitals' rows.
pub(crate) const ACCENT_ROWS: u16 = 1;
/// Fusion Pixel 8px's capital height in art pixels.
pub(crate) const CAP_H: u16 = 6;
/// A line's height in art pixels: the accent rows, the capitals, and a row for
/// descenders, Fusion Pixel 8px's own box. Pinned to `fusion-pixel-8px.bin`'s header.
pub(crate) const LINE_H: u16 = ACCENT_ROWS + CAP_H + 1;

/// `text`'s width in art pixels, from its first ink column to its last: its
/// last cell's gap column is ink only when [`fills_cell`].
pub(crate) fn width(text: &str) -> ArtPx {
    let joins = clusters(text)
        .last()
        .is_some_and(|(cluster, _)| cluster.chars().all(fills_cell));
    let advance = advance(text).0;
    ArtPx(if joins {
        advance
    } else {
        advance.saturating_sub(1)
    })
}

/// Whether `c` is one of the light box-drawing lines or block elements the
/// cutaway draws to its cell's edges so neighbours join; every other glyph
/// leaves its cell's last column as the gap.
pub(crate) fn fills_cell(c: char) -> bool {
    matches!(
        c,
        '\u{2500}'
            | '\u{2502}'
            | '\u{250c}'
            | '\u{2510}'
            | '\u{2514}'
            | '\u{2518}'
            | '\u{251c}'
            | '\u{2524}'
            | '\u{252c}'
            | '\u{2534}'
            | '\u{253c}'
            | '\u{256d}'..='\u{2570}'
            | '\u{2574}'..='\u{2577}'
            | '\u{2580}'..='\u{259f}'
    )
}

/// Art pixels from a run's left edge to where a run `n` cells on starts.
pub(crate) fn columns(n: u16) -> ArtPx {
    ArtPx(n.saturating_mul(ADVANCE))
}

/// The cells `cluster` takes on a terminal: ratatui-core 0.1.2 writes none
/// for one holding a control (`Buffer::set_stringn`) and else its
/// `unicode-width` plus one per halfwidth (semi-)voiced sound mark
/// (`CellWidth for str`, `buffer/cell_width.rs:42-43`).
fn cluster_cells(cluster: &str) -> u16 {
    if cluster.contains(char::is_control) {
        return 0;
    }
    let marks = cluster
        .chars()
        .filter(|c| HALFWIDTH_SOUND_MARKS.contains(c))
        .count();
    u16::try_from(cluster.width() + marks).unwrap_or(u16::MAX)
}

/// U+FF9E and U+FF9F, which `unicode-width` gives no width but a terminal
/// draws in a cell of their own (ratatui-core's `count_halfwidth_sound_marks`).
const HALFWIDTH_SOUND_MARKS: [char; 2] = ['\u{ff9e}', '\u{ff9f}'];

/// Each grapheme cluster of `text` that takes a cell, and the cells it takes.
pub(crate) fn clusters(text: &str) -> impl Iterator<Item = (&str, u16)> {
    text.graphemes(true)
        .map(|cluster| (cluster, cluster_cells(cluster)))
        .filter(|&(_, n)| n > 0)
}

/// The cells `text` takes, the one width every run of it is laid out by.
pub fn cells(text: &str) -> u16 {
    clusters(text).fold(0, |sum, (_, n)| sum.saturating_add(n))
}

/// [`columns`] past all of `text`: where the run after it starts.
pub(crate) fn advance(text: &str) -> ArtPx {
    columns(cells(text))
}

/// Logical rows between a badge's anchor and the line it sits on.
pub(crate) const LABEL_GAP: u16 = 2;

/// Logical rows a chitchat bubble's anchor rides over its speaker's badge's: a
/// cell row clear of the badge.
const BUBBLE_LIFT: u16 = 2 * crate::layout::CELL_ROWS;

/// One line of text a frame shows: its spans laid end to end, on `plate` if it
/// has one. Its inks are resolved, so a painter reads no theme for it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextRun {
    /// The logical point it is placed by, as `align` says.
    pub at: Point,
    /// Where `at` lies on its line.
    pub align: Align,
    /// Its spans, in reading order.
    pub spans: Vec<TextSpan>,
    /// The fill behind it, if any.
    pub plate: Option<Rgb>,
    /// A bar down its plate's left edge, in this ink.
    pub strip: Option<Rgb>,
    /// What it labels.
    pub role: TextRole,
}

/// A stretch of a [`TextRun`] in one ink.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextSpan {
    /// What it shows.
    pub content: Content,
    /// Its colour.
    pub ink: Rgb,
}

impl TextSpan {
    /// `text` in `ink`.
    pub fn new(text: impl Into<String>, ink: Rgb) -> Self {
        Self {
            content: Content::Text(text.into()),
            ink,
        }
    }

    /// It as a terminal writes it: [`Content::text`].
    pub fn text(&self) -> &str {
        self.content.text()
    }
}

/// What a stretch of text shows: characters, or an icon, which takes the
/// cells its terminal glyph does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Content {
    Text(String),
    Icon(super::Icon),
}

impl Content {
    /// It as a terminal writes it: an icon as its glyph.
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Icon(icon) => icon.terminal(),
        }
    }
}

/// An agent's name badge: its name in its tone on the badge plate, marked in
/// the source's hue, centred `LABEL_GAP` rows over `at`. A terminal marks it
/// with a [`BADGE_MARKER`](crate::badge::BADGE_MARKER) before the name, a
/// pixel painter with a bar down the plate's left edge. Its parts are fields,
/// so a painter reads them instead of a run's spans by position.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Badge {
    /// Whose badge it is.
    pub agent: AgentId,
    /// The point it hangs over, its sprite's top-centre.
    pub at: Point,
    /// The marker's ink.
    pub marker: Rgb,
    /// The name, in its ink.
    pub name: TextSpan,
    /// The fill behind it.
    pub plate: Rgb,
}

impl Badge {
    /// `agent`'s badge over `anchor`.
    pub(crate) fn new(
        anchor: Point,
        agent: &AgentSlot,
        namesakes: &crate::badge::Namesakes<'_>,
        theme: &Theme,
    ) -> Self {
        let text = namesakes.text(agent);
        let ink = crate::badge::badge_ink(&text, crate::badge::tone_of(agent), theme);
        Self {
            agent: agent.agent_id,
            at: anchor,
            marker: ink.marker,
            name: TextSpan::new(text, ink.name),
            plate: crate::badge::badge_plate_colour(theme),
        }
    }

    /// It as a pixel painter's run: the name on the plate, the marker its
    /// [`strip`](TextRun::strip).
    pub(crate) fn run(&self) -> TextRun {
        TextRun {
            at: self.at,
            align: Align::Over,
            spans: vec![self.name.clone()],
            plate: Some(self.plate),
            strip: Some(self.marker),
            role: TextRole::Badge(self.agent),
        }
    }

    /// The cells of `map` a line `w` cells wide takes over its anchor.
    pub fn line_at(&self, w: u16, map: CellMap) -> CellRect {
        line_at(self.at, Align::Over, w, map)
    }
}

/// The world text a host sets over a frame in its own font, in paint order:
/// the badges, each bubble over them, then the signs over every badge and
/// bubble.
#[derive(Debug, Clone, Copy)]
pub struct HostText<'a> {
    /// Each drawn agent's badge.
    pub badges: &'a [Badge],
    /// Each chitchat bubble, hung over its speaker's badge.
    pub bubbles: &'a [TextRun],
    /// The wall board's lines and the floor indicator.
    pub signs: &'a [TextRun],
    /// The dim of the frame it is set over, which it takes too.
    pub dim: crate::look::Dim,
}

/// A frame's [`HostText`], held.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HostRuns {
    pub(crate) badges: Vec<Badge>,
    pub(crate) bubbles: Vec<TextRun>,
    pub(crate) signs: Vec<TextRun>,
}

impl HostRuns {
    /// Its text, set over a frame under `dim`.
    pub(crate) fn view(&self, dim: crate::look::Dim) -> HostText<'_> {
        HostText {
            badges: &self.badges,
            bubbles: &self.bubbles,
            signs: &self.signs,
            dim,
        }
    }
}

/// Where a [`TextRun`]'s `at` lies on its line, a cell row tall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    /// Centred over it, `LABEL_GAP` rows above: a badge over a head.
    Over,
    /// At its top-left.
    Left,
    /// At its top-right.
    Right,
    /// At its top-centre.
    Centre,
}

/// What a [`TextRun`] labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextRole {
    /// An agent's name badge.
    Badge(AgentId),
    /// The wall board's brand.
    Brand,
    /// The wall board's star: a link to the repo.
    Star,
    /// A line of the wall board's mood or context.
    Board,
    /// The floor indicator over the elevator.
    Indicator,
    /// An agent's chitchat bubble.
    Bubble(AgentId),
}

impl TextRun {
    /// Each of `runs` that is set, with where `place` sets it: a run yields
    /// to one before it that it `meets` there, since on a grid wider than the
    /// one the runs were laid out on the star would write over the brand
    /// (`no_run_overprints_another_on_its_line`). Each painter's grid brings
    /// its own `place` and `meets`.
    pub fn set_first<P>(
        runs: &[TextRun],
        place: impl Fn(&TextRun) -> P,
        meets: impl Fn(&P, &P) -> bool,
    ) -> Vec<(&TextRun, P)> {
        let mut set: Vec<(&TextRun, P)> = Vec::new();
        for run in runs {
            let at = place(run);
            if !set.iter().any(|(_, s)| meets(s, &at)) {
                set.push((run, at));
            }
        }
        set
    }

    /// The floor indicator naming `floor` over the elevator at `door`:
    /// centred on the door, in the cell over it, on the badge plate, an arrow
    /// with no floor its way dim.
    pub(crate) fn indicator(door: Point, floor: crate::floor::FloorMeta, theme: &Theme) -> Self {
        Self {
            at: Point {
                x: door.x + crate::layout::ELEVATOR_W / 2,
                y: crate::layout::floor_indicator_rows(door.y).start,
            },
            align: Align::Centre,
            spans: crate::layout::floor_indicator(floor)
                .into_iter()
                .map(|content| TextSpan {
                    ink: match content {
                        Content::Icon(Icon::NoUp | Icon::NoDown) => theme.ui.tooltip_dim,
                        _ => theme.ui.neon_brand,
                    },
                    content,
                })
                .collect(),
            plate: Some(crate::badge::badge_plate_colour(theme)),
            strip: None,
            role: TextRole::Indicator,
        }
    }

    /// The room's signs: `board`'s lines, then the floor indicator naming
    /// `floor` over the elevator at `door`.
    pub(crate) fn signs(
        board: &crate::neon_sign::BoardModel,
        door: Point,
        floor: crate::floor::FloorMeta,
        theme: &Theme,
    ) -> Vec<Self> {
        let mut runs = board.runs(theme);
        runs.push(Self::indicator(door, floor, theme));
        runs
    }

    /// `bubble` over its speaker's badge, which hangs over `badge_at`, in
    /// the tooltip's ink on its plate.
    pub(crate) fn bubble(
        bubble: &crate::chitchat::ChitchatBubble,
        badge_at: Point,
        theme: &Theme,
    ) -> Self {
        Self {
            at: Point {
                x: badge_at.x,
                y: badge_at.y.saturating_sub(BUBBLE_LIFT),
            },
            align: Align::Over,
            spans: vec![TextSpan::new(bubble.text, theme.ui.tooltip_text)],
            plate: Some(theme.ui.tooltip_bg),
            strip: None,
            role: TextRole::Bubble(bubble.speaker),
        }
    }

    /// Its spans' text, end to end.
    pub(crate) fn text(&self) -> String {
        self.spans.iter().map(TextSpan::text).collect()
    }

    /// The cells of `map` a line `w` cells wide takes where its align puts
    /// it: the one placement every terminal painter and the star's hit test
    /// read.
    pub fn line_at(&self, w: u16, map: CellMap) -> CellRect {
        line_at(self.at, self.align, w, map)
    }
}

/// The cells of `map` a line `w` cells wide takes, placed by `at` as `align`
/// says: the row `at`'s cell lies in, `LABEL_GAP` rows up for
/// [`Align::Over`].
fn line_at(at: Point, align: Align, w: u16, map: CellMap) -> CellRect {
    let anchor = match align {
        Align::Over => Point {
            y: at.y.saturating_sub(LABEL_GAP),
            ..at
        },
        Align::Left | Align::Right | Align::Centre => at,
    };
    let (col, row) = map.cell_of(anchor);
    let x = match align {
        Align::Left => col,
        Align::Right => col.saturating_sub(w),
        Align::Over | Align::Centre => col.saturating_sub(w / 2),
    };
    CellRect { x, y: row, w, h: 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The indicator's arrow is hollow and dim where no floor is its way:
    /// both lit between floors, the down arrow out on the ground floor, the up
    /// one on the top.
    #[test]
    fn an_arrow_with_no_floor_its_way_is_dim() {
        let theme = &crate::theme::NORMAL;
        let arrows = |floor, floors| -> Vec<(Icon, Rgb)> {
            let meta = crate::floor::FloorMeta::for_floor(floor, floors);
            TextRun::indicator(Point { x: 0, y: 0 }, meta, theme)
                .spans
                .into_iter()
                .filter_map(|s| match s.content {
                    Content::Icon(icon) => Some((icon, s.ink)),
                    Content::Text(_) => None,
                })
                .collect()
        };
        let (lit, dim) = (theme.ui.neon_brand, theme.ui.tooltip_dim);
        assert_eq!(arrows(1, 3), [(Icon::Up, lit), (Icon::Down, lit)]);
        assert_eq!(arrows(0, 3), [(Icon::Up, lit), (Icon::NoDown, dim)]);
        assert_eq!(arrows(2, 3), [(Icon::NoUp, dim), (Icon::Down, lit)]);
        assert_eq!(arrows(0, 1), [(Icon::NoUp, dim), (Icon::NoDown, dim)]);
    }

    #[test]
    fn a_run_is_its_advances_less_the_trailing_gap() {
        assert_eq!(width(""), ArtPx(0));
        assert_eq!(width("cc\u{b7}a"), ArtPx(4 * ADVANCE - 1));
    }
}
