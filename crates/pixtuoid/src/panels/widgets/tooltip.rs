use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::to_color;
use crate::panels::clip_widget_rect;
use pixtuoid_core::AgentId;
use pixtuoid_scene::display::cells::{CellMap, CellRect};
use pixtuoid_scene::display::{Badge, HostText, TextRole, TextRun};
use pixtuoid_scene::layout::Bounds;
use pixtuoid_scene::look::Dim;
use pixtuoid_scene::tooltip::Tooltip;

/// Where a cursor tooltip anchors: the hovered cell, and the scene rect it must
/// stay inside.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TooltipAt {
    pub(crate) mx: u16,
    pub(crate) my: u16,
    pub(crate) scene_rect: Rect,
}

/// Paint `text` as terminal text over the office in `scene_rect`, whose
/// cells `map` lays over it, under its dim: the badges, each
/// bubble over them, then the signs, which a bubble must not cover.
pub(crate) fn paint_host_text(
    f: &mut ratatui::Frame<'_>,
    text: HostText<'_>,
    (scene_rect, map): (Rect, CellMap),
    hovered: Option<AgentId>,
) {
    let dim = text.dim;
    paint_badges(f, text.badges, (scene_rect, map), hovered, dim);
    paint_text_runs(f, text.bubbles, (scene_rect, map), dim);
    for (run, at) in set_signs(text.signs, map) {
        put_line(f, dim_line(run_line(run), dim), at, scene_rect);
    }
}

/// `line`'s colors under `dim`.
fn dim_line(mut line: Line<'_>, dim: Dim) -> Line<'_> {
    for span in &mut line.spans {
        span.style.fg = span.style.fg.map(|c| super::dim_color(c, dim));
        span.style.bg = span.style.bg.map(|c| super::dim_color(c, dim));
    }
    line
}

/// The logical units the cells of `map` show that the star among `signs`
/// is set in by [`paint_host_text`], where a pointer opens the repo; `None`
/// when none is set.
pub(crate) fn star_area(signs: &[TextRun], map: CellMap) -> Option<Bounds> {
    set_signs(signs, map)
        .into_iter()
        .find_map(|(run, at)| (run.role == TextRole::Star).then(|| map.area_of(at)))
}

/// Each of `signs` that is set ([`TextRun::set_first`]), with its cells on
/// `map`: a sign meets another in a cell of its row.
fn set_signs(signs: &[TextRun], map: CellMap) -> Vec<(&TextRun, CellRect)> {
    TextRun::set_first(
        signs,
        |run| run.line_at(line_width(&run_line(run)), map),
        |s, at| s.y == at.y && s.x < at.x + at.w && at.x < s.x + s.w,
    )
}

/// Paint `badges` as terminal text on their plates, each a ● in its marker's
/// ink then its name in the name's, in the cells [`Badge::line_at`] gives
/// the line on `map`; `hovered`'s reads `▸name` in bold white.
pub(crate) fn paint_badges(
    f: &mut ratatui::Frame<'_>,
    badges: &[Badge],
    (scene_rect, map): (Rect, CellMap),
    hovered: Option<AgentId>,
    dim: Dim,
) {
    use ratatui::style::Modifier;
    for badge in badges {
        let Badge {
            agent,
            marker,
            name,
            plate,
            ..
        } = badge;
        let on_plate = Style::default().bg(to_color(*plate));
        let line = if hovered == Some(*agent) {
            let style = on_plate.fg(Color::White).add_modifier(Modifier::BOLD);
            Line::from(Span::styled(format!("\u{25b8}{}", name.text()), style))
        } else {
            Line::from(vec![
                Span::styled(
                    pixtuoid_scene::badge::BADGE_MARKER.to_string(),
                    on_plate.fg(to_color(*marker)),
                ),
                Span::styled(name.text().to_owned(), on_plate.fg(to_color(name.ink))),
            ])
        };
        let at = badge.line_at(line_width(&line), map);
        put_line(f, dim_line(line, dim), at, scene_rect);
    }
}

/// Paint `runs` as terminal text, each in the cells [`TextRun::line_at`]
/// gives the line it writes on `map` (`world_text_paints_where_line_at_puts_it`).
pub(crate) fn paint_text_runs(
    f: &mut ratatui::Frame<'_>,
    runs: &[TextRun],
    (scene_rect, map): (Rect, CellMap),
    dim: Dim,
) {
    for run in runs {
        let line = run_line(run);
        let at = run.line_at(line_width(&line), map);
        put_line(f, dim_line(line, dim), at, scene_rect);
    }
}

/// The line a terminal writes `run` as. The board's brand and star and the
/// floor indicator are bold, the indicator and a chitchat bubble padded a
/// cell each side on their plates.
fn run_line(run: &TextRun) -> Line<'_> {
    use ratatui::style::Modifier;
    let bold = matches!(
        run.role,
        TextRole::Brand | TextRole::Star | TextRole::Indicator
    );
    let style = |ink| {
        let style = Style::default().fg(to_color(ink));
        if bold {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    };
    let spans: Vec<Span<'_>> = match (run.role, run.plate) {
        (TextRole::Indicator | TextRole::Bubble(_), Some(plate)) => {
            let on_plate = |s: Style| s.bg(to_color(plate));
            let pad = || Span::styled(" ", on_plate(Style::default()));
            std::iter::once(pad())
                .chain(
                    run.spans
                        .iter()
                        .map(|s| Span::styled(s.text(), on_plate(style(s.ink)))),
                )
                .chain(std::iter::once(pad()))
                .collect()
        }
        _ => run
            .spans
            .iter()
            .map(|s| Span::styled(s.text(), style(s.ink)))
            .collect(),
    };
    Line::from(spans)
}

/// The cells `line` takes.
fn line_width(line: &Line<'_>) -> u16 {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    pixtuoid_scene::display::text::cells(&text)
}

/// Write `line` into scene cells `at`, clipped to `scene_rect`.
fn put_line(f: &mut ratatui::Frame<'_>, line: Line<'_>, at: CellRect, scene_rect: Rect) {
    if let Some(r) = clip_widget_rect(
        Rect {
            x: scene_rect.x.saturating_add(at.x),
            y: scene_rect.y.saturating_add(at.y),
            width: at.w,
            height: 1,
        },
        scene_rect,
    ) {
        f.render_widget(Paragraph::new(line), r);
    }
}

/// Paint `tip` by the pointer at `at`: its [`card`](Tooltip::card) where
/// [`place`](pixtuoid_scene::tooltip::place) opens it, on the cards' drop
/// shadow.
pub(crate) fn paint_tooltip(
    f: &mut ratatui::Frame<'_>,
    tip: &Tooltip,
    at: TooltipAt,
    theme: &pixtuoid_scene::theme::Theme,
) {
    let TooltipAt { mx, my, scene_rect } = at;
    let card = tip.card(theme);
    let area = CellRect {
        x: scene_rect.x,
        y: scene_rect.y,
        w: scene_rect.width,
        h: scene_rect.height,
    };
    let placed = pixtuoid_scene::tooltip::place(card.rect(), (mx, my), area, tip.anchor);
    let rect = Rect {
        x: placed.x,
        y: placed.y,
        width: placed.w,
        height: placed.h,
    };
    let Some(clipped) = clip_widget_rect(rect, scene_rect) else {
        return;
    };
    super::cast_drop_shadow(f, clipped);
    super::put_grid(f.buffer_mut(), &card, (placed.x, placed.y), clipped);
}

#[cfg(test)]
mod tests {
    use super::{Dim, TooltipAt, paint_tooltip};
    use pixtuoid_scene::display::GatewayCard;
    use pixtuoid_scene::tooltip::{DETAIL_CHARS, Tooltip, mascot_text as mascot_tooltip_text};

    /// Each tooltip the TUI shows, built by the shared model and painted by
    /// [`paint_tooltip`], as `renderer::paint_scene_tooltip` does.
    fn paint_hover_tooltip(
        f: &mut ratatui::Frame<'_>,
        scene: &pixtuoid_core::SceneState,
        id: pixtuoid_core::AgentId,
        at: TooltipAt,
        now: std::time::SystemTime,
        theme: &pixtuoid_scene::theme::Theme,
    ) {
        if let Some(tip) = pixtuoid_scene::tooltip::agent(scene, id, now) {
            paint_tooltip(f, &tip, at, theme);
        }
    }

    fn paint_simple_tooltip(
        f: &mut ratatui::Frame<'_>,
        text: &str,
        at: TooltipAt,
        theme: &pixtuoid_scene::theme::Theme,
    ) {
        paint_tooltip(f, &Tooltip::label(text), at, theme);
    }

    fn paint_coffee_tooltip(
        f: &mut ratatui::Frame<'_>,
        at: TooltipAt,
        theme: &pixtuoid_scene::theme::Theme,
    ) {
        paint_tooltip(f, &pixtuoid_scene::tooltip::coffee(), at, theme);
    }

    fn paint_pet_tooltip(
        f: &mut ratatui::Frame<'_>,
        kind: pixtuoid_scene::pet::PetKind,
        anim: pixtuoid_core::sprite::format::Piece,
        on_cooldown: bool,
        name: &str,
        at: TooltipAt,
        theme: &pixtuoid_scene::theme::Theme,
    ) {
        let tip = pixtuoid_scene::tooltip::pet(kind, anim, on_cooldown, name);
        paint_tooltip(f, &tip, at, theme);
    }

    fn paint_mascot_tooltip(
        f: &mut ratatui::Frame<'_>,
        card: &GatewayCard,
        at: TooltipAt,
        theme: &pixtuoid_scene::theme::Theme,
    ) {
        paint_tooltip(f, &Tooltip::label(&mascot_tooltip_text(card)), at, theme);
    }
    use pixtuoid_scene::display::cells::CellMap;
    use pixtuoid_scene::theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    /// Join the whole buffer into one newline-free string, so a `.contains` probe
    /// finds text regardless of which cell the box landed in.
    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf[(x, y)].symbol());
            }
        }
        out
    }

    fn row_of(term: &Terminal<TestBackend>, needle: &str) -> Option<u16> {
        let buf = term.backend().buffer();
        let area = buf.area;
        for y in 0..area.height {
            let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
            if row.contains(needle) {
                return Some(y);
            }
        }
        None
    }

    /// A lone `OpenClaw` mascot; only the tooltip-bearing fields vary.
    fn mascot(
        instance: Option<&str>,
        busy: bool,
        degraded: bool,
        active_sessions: u32,
    ) -> GatewayCard {
        GatewayCard {
            name: "OpenClaw",
            instance: instance.map(str::to_string),
            busy,
            degraded,
            active_sessions,
        }
    }

    /// A badge reading `name` in `tone` under `theme`, hung from `at`.
    fn badge(
        at: pixtuoid_scene::layout::Point,
        name: &str,
        tone: pixtuoid_scene::badge::BadgeTone,
        theme: &pixtuoid_scene::theme::Theme,
    ) -> super::Badge {
        let ink = pixtuoid_scene::badge::badge_ink(name, tone, theme);
        super::Badge {
            agent: pixtuoid_core::AgentId::from_transcript_path("/badge/0.jsonl"),
            at,
            marker: ink.marker,
            name: pixtuoid_scene::display::TextSpan::new(name, ink.name),
            plate: theme.ui.tooltip_bg,
        }
    }

    /// The board's lines land on the neon sign's interior: the brand leading
    /// L1, the star flush to its right, the mood on L2 and the context on L3.
    #[test]
    fn the_board_runs_land_on_the_signs_interior() {
        use pixtuoid_core::state::DaemonState;
        use pixtuoid_scene::layout::{NEON_PANEL_INNER_W, NEON_PANEL_INNER_X, NEON_PANEL_INNER_Y};
        let counts = pixtuoid_scene::tally::StateCounts {
            active: 2,
            waiting: 1,
            idle: 1,
            exiting: 0,
            total: 4,
        };
        let model = pixtuoid_scene::neon_sign::build_board(
            counts,
            0,
            None,
            Some(DaemonState::Idle),
            pixtuoid_scene::anim::Motion::Full,
            std::time::SystemTime::UNIX_EPOCH,
        );
        let mut term = Terminal::new(TestBackend::new(120, 44)).unwrap();
        let scene_rect = Rect::new(0, 0, 120, 44);
        term.draw(|f| {
            super::paint_text_runs(
                f,
                &model.runs(&theme::NORMAL),
                (scene_rect, CellMap::HALF_BLOCK),
                Dim::NONE,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let row = |line: u16| -> String {
            (0..NEON_PANEL_INNER_W)
                .map(|dx| buf[(NEON_PANEL_INNER_X + dx, NEON_PANEL_INNER_Y / 2 + line)].symbol())
                .collect()
        };
        let (l1, l2, l3) = (row(0), row(1), row(2));
        assert!(
            l1.starts_with(pixtuoid_scene::neon_sign::BOARD_BRAND),
            "brand leads L1: {l1:?}"
        );
        assert!(l1.ends_with("\u{2605} Star"), "star flush right: {l1:?}");
        assert!(
            l2.contains("\u{25b2}1 wait")
                && l2.contains("\u{25cf}2 work")
                && l2.contains("\u{25cb}1 idle"),
            "mood pulse (UNIX_EPOCH opens on the tally): {l2:?}"
        );
        assert!(l3.contains("\u{2b22}gw ok"), "gateway chip: {l3:?}");
    }

    /// World text paints in exactly the cells `line_at` gives its line on
    /// any terminal's grid. On the classic's, a badge centres on its anchor
    /// in the cell row `LABEL_GAP` rows over it, whichever row its anchor
    /// falls on, and every sign is set; on a cutaway's, no two signs meet.
    #[test]
    fn world_text_paints_where_line_at_puts_it() {
        use pixtuoid_core::state::DaemonState;
        use pixtuoid_scene::badge::BadgeTone;
        use pixtuoid_scene::layout::Point;
        let model = pixtuoid_scene::neon_sign::build_board(
            pixtuoid_scene::tally::StateCounts {
                active: 2,
                waiting: 1,
                idle: 1,
                exiting: 0,
                total: 4,
            },
            0,
            None,
            Some(DaemonState::Idle),
            pixtuoid_scene::anim::Motion::Full,
            std::time::SystemTime::UNIX_EPOCH,
        );
        let area = Rect::new(0, 0, 120, 44);
        let painted = |paint: &dyn Fn(&mut ratatui::Frame<'_>)| -> Vec<(u16, u16)> {
            let mut term = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            term.draw(|f| paint(f)).unwrap();
            let buf = term.backend().buffer();
            (0..area.height)
                .flat_map(|y| (0..area.width).map(move |x| (x, y)))
                .filter(|&(x, y)| buf[(x, y)] != ratatui::buffer::Cell::default())
                .collect()
        };
        for (y, row) in [(9, 3), (10, 4)] {
            let badge = badge(
                Point { x: 40, y },
                "cc\u{b7}repo",
                BadgeTone::Idle,
                &theme::NORMAL,
            );
            // The marker and the name.
            let w = 1 + 7;
            assert_eq!(
                painted(&|f| super::paint_badges(
                    f,
                    std::slice::from_ref(&badge),
                    (area, CellMap::HALF_BLOCK),
                    None,
                    Dim::NONE
                )),
                (40 - w / 2..40 - w / 2 + w)
                    .map(|x| (x, row))
                    .collect::<Vec<_>>(),
                "a badge at {:?}",
                badge.at
            );
        }
        let runs = model.runs(&theme::NORMAL);
        let stars = runs.iter().filter(|r| r.role == super::TextRole::Star);
        assert_eq!(stars.count(), 1, "the board has its star");
        let grid = |w, h, scale| CellMap {
            scale,
            cell: pixtuoid_scene::display::cells::CellPx { w, h },
        };
        for map in [
            CellMap::HALF_BLOCK,
            grid(14, 34, 16),
            grid(8, 16, 4),
            grid(10, 20, 4),
        ] {
            for run in &runs {
                let at = run.line_at(super::line_width(&super::run_line(run)), map);
                let cells: Vec<(u16, u16)> = (at.x..at.x + at.w).map(|x| (x, at.y)).collect();
                assert_eq!(
                    painted(&|f| super::paint_text_runs(
                        f,
                        std::slice::from_ref(run),
                        (area, map),
                        Dim::NONE
                    )),
                    cells,
                    "{map:?}: {:?}",
                    run.role
                );
            }
            let set = super::set_signs(&runs, map);
            for (i, (_, a)) in set.iter().enumerate() {
                for (_, b) in &set[i + 1..] {
                    let apart = a.y != b.y || a.x + a.w <= b.x || b.x + b.w <= a.x;
                    assert!(apart, "{map:?}: {a:?} meets {b:?}");
                }
            }
            if map == CellMap::HALF_BLOCK {
                assert_eq!(set.len(), runs.len(), "the classic sets every sign");
            }
        }
    }

    /// A hovered badge reads `▸name` in place of its marker.
    #[test]
    fn a_hovered_badge_reads_its_name_alone() {
        use pixtuoid_scene::badge::BadgeTone;
        use pixtuoid_scene::layout::Point;
        let badge = badge(
            Point { x: 20, y: 9 },
            "repo",
            BadgeTone::Idle,
            &theme::NORMAL,
        );
        let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
        term.draw(|f| {
            super::paint_badges(
                f,
                std::slice::from_ref(&badge),
                (Rect::new(0, 0, 40, 8), CellMap::HALF_BLOCK),
                Some(badge.agent),
                Dim::NONE,
            )
        })
        .unwrap();
        let buf = term.backend().buffer();
        let row = (0..8u16)
            .map(|y| (0..40u16).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .find(|row| !row.trim().is_empty())
            .unwrap_or_default();
        assert_eq!(row.trim(), "\u{25b8}repo");
    }

    /// The floor indicator centres on its anchor by display columns, not
    /// bytes, padded a cell each side on its plate.
    #[test]
    fn the_indicator_centres_by_display_columns_on_its_plate() {
        use pixtuoid_scene::layout::Point;
        let theme = &theme::NORMAL;
        let at = Point { x: 28, y: 8 };
        let run = super::TextRun {
            at,
            align: pixtuoid_scene::display::Align::Centre,
            spans: pixtuoid_scene::layout::floor_indicator(
                pixtuoid_scene::floor::FloorMeta::ground(),
            )
            .into_iter()
            .map(|content| pixtuoid_scene::display::TextSpan {
                content,
                ink: theme.ui.neon_brand,
            })
            .collect(),
            plate: Some(theme.ui.tooltip_bg),
            strip: None,
            role: super::TextRole::Indicator,
        };
        let text: String = run.spans.iter().map(|s| s.text()).collect();
        let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
        term.draw(|f| {
            super::paint_text_runs(
                f,
                &[run],
                (Rect::new(0, 0, 80, 30), CellMap::HALF_BLOCK),
                Dim::NONE,
            )
        })
        .unwrap();
        let buf = term.backend().buffer();
        let bg = super::to_color(theme.ui.tooltip_bg);
        let cols: Vec<u16> = (0..80u16)
            .filter(|&x| buf[(x, at.y / 2)].style().bg == Some(bg))
            .collect();
        let padded = format!(" {text} ");
        assert_eq!(
            cols.len(),
            padded.chars().count(),
            "its display-column width"
        );
        assert_eq!(
            cols.first(),
            Some(&(at.x - cols.len() as u16 / 2)),
            "centred on its anchor"
        );
    }

    /// A badge sits on its plate: every cell of its text takes the plate's
    /// colour as its background, hovered or not.
    #[test]
    fn a_badge_sits_on_its_plate() {
        use pixtuoid_scene::badge::BadgeTone;
        use pixtuoid_scene::layout::Point;
        let plate = pixtuoid_core::sprite::Rgb { r: 1, g: 2, b: 3 };
        let badge = super::Badge {
            plate,
            ..badge(
                Point { x: 20, y: 8 },
                "cc\u{b7}repo",
                BadgeTone::Idle,
                &theme::NORMAL,
            )
        };
        for hovered in [None, Some(badge.agent)] {
            let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
            term.draw(|f| {
                super::paint_badges(
                    f,
                    std::slice::from_ref(&badge),
                    (f.area(), CellMap::HALF_BLOCK),
                    hovered,
                    Dim::NONE,
                )
            })
            .unwrap();
            let row = row_of(&term, "repo").expect("the badge painted");
            let buf = term.backend().buffer();
            let inked: Vec<_> = (0..buf.area.width)
                .filter(|&x| buf[(x, row)].symbol() != " ")
                .map(|x| buf[(x, row)].bg)
                .collect();
            assert!(!inked.is_empty(), "premise: the badge painted");
            assert!(
                inked.iter().all(|&bg| bg == super::to_color(plate)),
                "{hovered:?}: {inked:?}"
            );
        }
    }

    /// A badge's text centres on its anchor, the sprite's top-centre.
    #[test]
    fn a_badge_centres_its_text_on_the_anchor() {
        use pixtuoid_scene::badge::BadgeTone;
        use pixtuoid_scene::layout::Point;
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let scene_rect = Rect {
            x: 3,
            y: 1,
            width: 36,
            height: 8,
        };
        let anchor = Point { x: 20, y: 8 };
        let text = "abcdefgh";
        term.draw(|f| {
            super::paint_badges(
                f,
                &[badge(anchor, text, BadgeTone::Idle, &theme::NORMAL)],
                (scene_rect, CellMap::HALF_BLOCK),
                None,
                Dim::NONE,
            )
        })
        .unwrap();
        let row = row_of(&term, text).expect("the badge painted");
        let buf = term.backend().buffer();
        let left = (0..buf.area.width)
            .find(|&x| buf[(x, row)].symbol() != " ")
            .expect("a painted cell");
        // The ● marker plus the name.
        let width = 1 + text.chars().count() as u16;
        assert_eq!(left, scene_rect.x + anchor.x - width / 2);
    }

    /// The classic badge paints the shared model's ink, which
    /// `every_badge_ink_reads_on_its_plate_in_every_theme` holds at WCAG AA:
    /// the ● in the source's hue, the name in the tone, in every theme.
    #[test]
    fn a_badge_paints_the_models_ink() {
        use pixtuoid_scene::badge::{BadgeTone, badge_ink};
        use pixtuoid_scene::layout::Point;
        let text = "cc\u{b7}repo";
        for theme in pixtuoid_scene::theme::ALL_THEMES {
            for tone in [
                BadgeTone::Active,
                BadgeTone::Waiting,
                BadgeTone::Idle,
                BadgeTone::Exiting,
            ] {
                let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
                term.draw(|f| {
                    super::paint_badges(
                        f,
                        &[badge(Point { x: 20, y: 8 }, text, tone, theme)],
                        (f.area(), CellMap::HALF_BLOCK),
                        None,
                        Dim::NONE,
                    )
                })
                .unwrap();
                let row = row_of(&term, "repo").expect("the badge painted");
                let buf = term.backend().buffer();
                let fg = |s: &str| {
                    (0..buf.area.width)
                        .find(|&x| buf[(x, row)].symbol() == s)
                        .and_then(|x| buf[(x, row)].style().fg)
                };
                let ink = badge_ink(text, tone, theme);
                let at = format!("{} {tone:?}", theme.name);
                let marker = pixtuoid_scene::badge::BADGE_MARKER.to_string();
                assert_eq!(fg(&marker), Some(super::to_color(ink.marker)), "{at}");
                assert_eq!(fg("r"), Some(super::to_color(ink.name)), "{at}");
            }
        }
    }

    #[test]
    fn a_chitchat_bubble_centres_over_its_speakers_badge() {
        use pixtuoid_scene::badge::BadgeTone;
        use pixtuoid_scene::layout::Point;
        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let scene_rect = Rect {
            x: 3,
            y: 1,
            width: 36,
            height: 10,
        };
        let (name, quip) = ("abcdefgh", "LGTM!");
        let speaker = badge(
            Point { x: 20, y: 14 },
            name,
            BadgeTone::Idle,
            &theme::NORMAL,
        );
        let id = speaker.agent;
        let bubble = super::TextRun {
            at: Point {
                y: speaker.at.y - 2 * pixtuoid_scene::layout::CELL_ROWS,
                ..speaker.at
            },
            align: pixtuoid_scene::display::Align::Over,
            spans: vec![pixtuoid_scene::display::TextSpan::new(
                quip,
                theme::NORMAL.ui.tooltip_text,
            )],
            plate: Some(theme::NORMAL.ui.tooltip_bg),
            strip: None,
            role: super::TextRole::Bubble(id),
        };
        term.draw(|f| {
            super::paint_badges(
                f,
                std::slice::from_ref(&speaker),
                (scene_rect, CellMap::HALF_BLOCK),
                None,
                Dim::NONE,
            );
            super::paint_text_runs(f, &[bubble], (scene_rect, CellMap::HALF_BLOCK), Dim::NONE);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let centre = |needle: &str| {
            let row = row_of(&term, needle).expect("painted");
            let cells: Vec<u16> = (0..buf.area.width)
                .filter(|&x| {
                    buf[(x, row)].symbol() != " "
                        || buf[(x, row)].bg != ratatui::style::Color::Reset
                })
                .collect();
            let (l, r) = (cells[0], cells[cells.len() - 1]);
            u16::midpoint(l, r)
        };
        assert_eq!(centre(quip), centre(name));
    }

    #[test]
    fn mascot_tooltip_paints_gateway_verb_into_buffer() {
        let mut term = Terminal::new(TestBackend::new(60, 8)).unwrap();
        term.draw(|f| {
            paint_mascot_tooltip(
                f,
                &mascot(None, true, false, 1),
                TooltipAt {
                    mx: 10,
                    my: 3,
                    scene_rect: f.area(),
                },
                &theme::NORMAL,
            )
        })
        .unwrap();
        let busy = buffer_text(&term);
        assert!(
            busy.contains("OpenClaw gateway"),
            "busy paint should render the gateway name+verb, got: {busy:?}"
        );
        assert!(
            busy.contains("working"),
            "busy=true should render the 'working' verb, got: {busy:?}"
        );

        let mut term2 = Terminal::new(TestBackend::new(60, 8)).unwrap();
        term2
            .draw(|f| {
                paint_mascot_tooltip(
                    f,
                    &mascot(None, true, true, 1),
                    TooltipAt {
                        mx: 10,
                        my: 3,
                        scene_rect: f.area(),
                    },
                    &theme::NORMAL,
                )
            })
            .unwrap();
        let degraded = buffer_text(&term2);
        assert!(
            degraded.contains("model error"),
            "degraded should render 'model error', got: {degraded:?}"
        );
        assert!(
            !degraded.contains("working"),
            "degraded must override busy → no 'working' verb, got: {degraded:?}"
        );
    }

    #[test]
    fn pet_tooltip_shows_pet_me_on_sit_anim() {
        use pixtuoid_scene::pet::PetKind;
        let kind = PetKind::Dog;
        let sit = kind.sit_anim();
        let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
        term.draw(|f| {
            paint_pet_tooltip(
                f,
                kind,
                sit,
                false,
                "Rex",
                TooltipAt {
                    mx: 10,
                    my: 3,
                    scene_rect: f.area(),
                },
                &theme::NORMAL,
            )
        })
        .unwrap();
        let text = buffer_text(&term);
        assert!(
            text.contains("Pet me!"),
            "sit anim + not-on-cooldown should render 'Pet me!', got: {text:?}"
        );
        assert!(
            !text.contains("woof"),
            "sit arm must not fall through to the cooldown woof, got: {text:?}"
        );
        assert!(
            !text.contains("sleeping"),
            "sit arm must not be the sleep arm, got: {text:?}"
        );
    }

    #[test]
    fn hover_tooltip_idle_shows_no_meter_and_casts_a_drop_shadow() {
        use std::path::Path;
        use std::sync::Arc;
        use std::time::{Duration, SystemTime};

        use pixtuoid_core::state::{ActivityState, AgentSlot, GlobalDeskIndex};
        use pixtuoid_core::{AgentId, SceneState};

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
        let id = AgentId::from_transcript_path("/fresh/0.jsonl");
        let slot = AgentSlot {
            agent_id: id,
            source: Arc::from("claude-code"),
            session_id: Arc::from("s"),
            cwd: Arc::from(Path::new("/repo")),
            label: "fresh".into(),
            state: ActivityState::Idle,
            state_started_at: now,
            // 2s < the 5s freshness floor → `--%`.
            created_at: now - Duration::from_secs(2),
            last_event_at: now,
            exiting_at: None,
            desk_index: GlobalDeskIndex(0),
            floor_idx: 0,
            tool_call_count: 0,
            active_ms: 0,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        };
        let mut scene = SceneState::uniform(12);
        scene.agents.insert(id, slot);

        let mut term = Terminal::new(TestBackend::new(60, 24)).unwrap();
        let bright = ratatui::style::Color::Rgb(200, 200, 200);
        term.draw(|f| {
            // Stand in for the already-flushed office so the drop shadow has real
            // cells to dim.
            let full = f.area();
            for y in 0..full.height {
                for x in 0..full.width {
                    let cell = &mut f.buffer_mut()[(x, y)];
                    cell.set_symbol("\u{2580}");
                    cell.fg = bright;
                    cell.bg = bright;
                }
            }
            paint_hover_tooltip(
                f,
                &scene,
                id,
                TooltipAt {
                    mx: 20,
                    my: 10,
                    scene_rect: f.area(),
                },
                now,
                &theme::NORMAL,
            );
        })
        .unwrap();
        let text = buffer_text(&term);
        assert!(
            !text.contains('%'),
            "an idle agent's dossier carries no active-% meter, got: {text:?}"
        );
        assert!(text.contains("Idle"), "idle state word, got: {text:?}");
        // A bright equal-channel office cell can only turn into a DIMMER
        // equal-channel gray via the drop-shadow dim — the card's `tooltip_bg` is a
        // distinct hue — so its presence proves the shadow ran.
        let buf = term.backend().buffer();
        let shadowed = (0..buf.area.height).any(|y| {
            (0..buf.area.width).any(|x| {
                matches!(buf[(x, y)].bg, ratatui::style::Color::Rgb(r, g, b) if r == g && g == b && r == (200.0 * pixtuoid_scene::display::cells::CARD_SHADOW) as u8)
            })
        });
        assert!(
            shadowed,
            "the agent hover tooltip must cast a drop shadow via the shared backing"
        );
    }

    #[test]
    fn both_detail_sources_clip_at_the_one_card_budget() {
        use std::path::Path;
        use std::sync::Arc;
        use std::time::{Duration, SystemTime};

        use pixtuoid_core::state::{ActivityState, AgentSlot, GlobalDeskIndex, ToolKind};
        use pixtuoid_core::{AgentId, SceneState};

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
        let id = AgentId::from_transcript_path("/budget/0.jsonl");
        // Long enough that DETAIL_CHARS + 1 chars still exist to over-render.
        let payload: String = ('a'..='z').chain('A'..='Z').collect();
        let render = |state: ActivityState| {
            let slot = AgentSlot {
                agent_id: id,
                source: Arc::from("claude-code"),
                session_id: Arc::from("s"),
                cwd: Arc::from(Path::new("/repo")),
                label: "budget".into(),
                state,
                state_started_at: now,
                created_at: now - Duration::from_secs(2),
                last_event_at: now,
                exiting_at: None,
                desk_index: GlobalDeskIndex(0),
                floor_idx: 0,
                tool_call_count: 0,
                active_ms: 0,
                unknown_cwd: false,
                parent_id: None,
                pid: None,
                model: None,
                effort: None,
                tokens_used: 0,
                last_usage: None,
            };
            let mut scene = SceneState::uniform(12);
            scene.agents.insert(id, slot);
            let mut term = Terminal::new(TestBackend::new(90, 30)).unwrap();
            term.draw(|f| {
                paint_hover_tooltip(
                    f,
                    &scene,
                    id,
                    TooltipAt {
                        mx: 20,
                        my: 12,
                        scene_rect: f.area(),
                    },
                    now,
                    &theme::NORMAL,
                );
            })
            .unwrap();
            buffer_text(&term)
        };

        let budget = DETAIL_CHARS;
        let fits: String = payload.chars().take(budget).collect();
        let overruns: String = payload.chars().take(budget + 1).collect();

        let active = render(ActivityState::Active {
            detail: Some(Arc::from(format!("Read {payload}").as_str())),
            kind: ToolKind::Read,
            tool_use_id: None,
        });
        assert!(
            active.contains(&fits) && !active.contains(&overruns),
            "the Active tool-detail row must clip at exactly {budget} chars, got: {active:?}"
        );

        let waiting = render(ActivityState::Waiting {
            reason: Arc::from(payload.as_str()),
        });
        assert!(
            waiting.contains(&fits) && !waiting.contains(&overruns),
            "the Waiting reason row must clip at the SAME {budget} chars, got: {waiting:?}"
        );
    }

    #[test]
    fn simple_tooltip_flips_below_when_cursor_near_top() {
        let scene = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 24,
        };

        // Box height is 3 (1 content line wrapped in `Padding::uniform(1)`), so the
        // content row = box-top + 1.
        let mut top = Terminal::new(TestBackend::new(40, 24)).unwrap();
        top.draw(|f| {
            paint_simple_tooltip(
                f,
                " PROBE ",
                TooltipAt {
                    mx: 5,
                    my: 0,
                    scene_rect: scene,
                },
                &theme::NORMAL,
            )
        })
        .unwrap();
        let top_y = row_of(&top, "PROBE").expect("PROBE rendered when cursor at top");
        assert_eq!(
            top_y, 2,
            "cursor at the top edge → box flips below (top=my+1=1, content row 2)"
        );

        let mut low = Terminal::new(TestBackend::new(40, 24)).unwrap();
        low.draw(|f| {
            paint_simple_tooltip(
                f,
                " PROBE ",
                TooltipAt {
                    mx: 5,
                    my: 20,
                    scene_rect: scene,
                },
                &theme::NORMAL,
            )
        })
        .unwrap();
        let low_y = row_of(&low, "PROBE").expect("PROBE rendered when cursor low");
        assert_eq!(
            low_y, 18,
            "cursor well below the top → box floats above (top=my-3=17, content row 18)"
        );
    }

    #[test]
    fn mascot_tooltip_verb_keys_on_turn_state_not_session_count() {
        assert_eq!(
            mascot_tooltip_text(&mascot(None, false, false, 0)),
            " OpenClaw gateway · idle "
        );
        assert_eq!(
            mascot_tooltip_text(&mascot(None, false, false, 1)),
            " OpenClaw gateway · idle "
        );
        assert_eq!(
            mascot_tooltip_text(&mascot(None, true, false, 1)),
            " OpenClaw gateway · working "
        );
        assert_eq!(
            mascot_tooltip_text(&mascot(None, true, false, 3)),
            " OpenClaw gateway · working · 3 sessions "
        );
    }

    #[test]
    fn mascot_tooltip_names_the_instance_only_when_there_is_a_sibling() {
        assert_eq!(
            mascot_tooltip_text(&mascot(Some("19789"), true, false, 0)),
            " OpenClaw:19789 gateway · working "
        );
        assert_eq!(
            mascot_tooltip_text(&mascot(Some("18789"), false, true, 2)),
            " OpenClaw:18789 gateway · model error · 2 sessions "
        );
        assert_eq!(
            mascot_tooltip_text(&mascot(None, false, false, 0)),
            " OpenClaw gateway · idle ",
            "a single gateway's tooltip stays byte-identical"
        );
    }

    #[test]
    fn mascot_tooltip_degraded_overrides_busy_and_idle() {
        assert_eq!(
            mascot_tooltip_text(&mascot(None, false, true, 0)),
            " OpenClaw gateway · model error "
        );
        assert_eq!(
            mascot_tooltip_text(&mascot(None, true, true, 1)),
            " OpenClaw gateway · model error "
        );
        assert_eq!(
            mascot_tooltip_text(&mascot(None, true, true, 3)),
            " OpenClaw gateway · model error · 3 sessions "
        );
    }

    /// Pre-fills the buffer with a bright equal-channel gray; only a SHADOWED office
    /// cell ends up an equal-channel gray darker than that (the card's `tooltip_bg`
    /// is a distinct hue), so its presence proves the shadow ran.
    #[test]
    fn coffee_tooltip_casts_a_drop_shadow_via_the_shared_backing() {
        use ratatui::style::Color;
        let scene = Rect::new(0, 0, 48, 16);
        let bright = Color::Rgb(200, 200, 200);
        let mut term = Terminal::new(TestBackend::new(48, 16)).unwrap();
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
            paint_coffee_tooltip(
                f,
                TooltipAt {
                    mx: 20,
                    my: 8,
                    scene_rect: scene,
                },
                &theme::NORMAL,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let shadowed = (0..buf.area.height).any(|y| {
            (0..buf.area.width).any(|x| {
                matches!(buf[(x, y)].bg, Color::Rgb(r, g, b) if r == g && g == b && r == (200.0 * pixtuoid_scene::display::cells::CARD_SHADOW) as u8)
            })
        });
        assert!(
            shadowed,
            "the tooltip path must dim office cells into a drop shadow"
        );
    }
}
