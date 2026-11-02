use super::{Overflow, Panel, to_color};
use crate::panels::ui_state::Appearance;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};

fn theme_swatch(t: &pixtuoid_scene::theme::Theme) -> (Color, Color) {
    (to_color(t.ui.neon_brand), to_color(t.surface.carpet_base))
}

/// The theme picker, and over its list the look row where `shown` has a
/// look: the look on its own axis, ←→/hl, as the themes are on ↑↓/jk.
pub(crate) fn paint_appearance(
    f: &mut ratatui::Frame<'_>,
    shown: Appearance,
    bounds: Rect,
    theme: &pixtuoid_scene::theme::Theme,
) {
    use pixtuoid_scene::theme;

    /// The theme picker's width, wider than its title and its rows.
    const THEME_W: u16 = 28;
    let selected = shown.theme;
    let accent = Style::default()
        .fg(to_color(theme.ui.neon_brand))
        .add_modifier(Modifier::BOLD);
    let idle = Style::default().fg(to_color(theme.ui.label_idle));
    let name_w = theme::ALL_THEMES
        .iter()
        .map(|t| t.display_name.chars().count())
        .max()
        .unwrap_or_default();
    let items: Vec<Line<'static>> = theme::ALL_THEMES
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let prefix = if i == selected { "\u{25b8} " } else { "  " };
            let name_style = if i == selected { accent } else { idle };
            let (brand, surface) = theme_swatch(t);
            Line::from(vec![
                TSpan::styled(format!("{prefix}{:<name_w$}", t.display_name), name_style),
                TSpan::raw(" "),
                TSpan::styled("\u{2588}", Style::default().fg(brand)),
                TSpan::styled("\u{2588}", Style::default().fg(surface)),
            ])
        })
        .collect();
    let (title, above) = match shown.look {
        None => ("Theme [\u{2191}\u{2193}/jk] Enter/Esc", vec![]),
        Some(look) => (
            "Appearance [\u{2191}\u{2193}/jk \u{2190}\u{2192}/hl] Enter/Esc",
            vec![
                Line::from(vec![
                    TSpan::styled("  Look  \u{2039} ", idle),
                    TSpan::styled(look.display_name(), accent),
                    TSpan::styled(" \u{203a}", idle),
                ]),
                Line::from(""),
            ],
        ),
    };
    let title_w = u16::try_from(title.chars().count()).unwrap_or(u16::MAX);
    Panel {
        title: Some(title),
        content_w: THEME_W.max(title_w),
        above,
        list: items,
        below: vec![],
        overflow: Overflow::Follow {
            selected: Some(selected),
            scroll: 0,
            cap: None,
        },
    }
    .paint(f, bounds, theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    // 24 is narrower than THEME_W: an unclamped Clear panics indexing past the buffer.
    #[test]
    fn theme_picker_narrow_terminal_does_not_panic() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;
        let mut term = Terminal::new(TestBackend::new(24, 30)).unwrap();
        term.draw(|f| {
            paint_appearance(
                f,
                Appearance::default(),
                Rect::new(0, 0, 24, 30),
                &pixtuoid_scene::theme::NORMAL,
            );
        })
        .unwrap();
    }

    #[test]
    fn every_swatch_sits_one_cell_past_the_longest_display_name() {
        use pixtuoid_scene::theme::ALL_THEMES;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut term = Terminal::new(TestBackend::new(60, 30)).unwrap();
        term.draw(|f| {
            paint_appearance(
                f,
                Appearance::default(),
                Rect::new(0, 0, 60, 30),
                &pixtuoid_scene::theme::NORMAL,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let column = |row: &str, text: &str| row[..row.find(text).expect("drawn")].chars().count();
        let longest = ALL_THEMES
            .iter()
            .map(|t| t.display_name.chars().count())
            .max()
            .expect("a theme");
        for t in ALL_THEMES {
            let row = rows
                .iter()
                .find(|r| r.contains(t.display_name))
                .expect("listed");
            assert_eq!(
                column(row, "\u{2588}"),
                column(row, t.display_name) + longest + 1,
                "{}",
                t.display_name
            );
        }
    }

    /// The picker's rows as text, drawn over a `w`×`h` screen.
    fn picker_text(shown: Appearance, w: u16, h: u16) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            paint_appearance(
                f,
                shown,
                Rect::new(0, 0, w, h),
                &pixtuoid_scene::theme::NORMAL,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    /// With no look, the picker is the theme picker, every row of it; with
    /// one, it says Appearance and shows the look on its row over the
    /// themes, which keep their rows below it.
    #[test]
    fn a_look_row_sits_over_the_themes_only_where_there_is_a_look() {
        use pixtuoid_scene::look::LookKind;
        use pixtuoid_scene::theme::ALL_THEMES;
        let theme_only = picker_text(Appearance::default(), 80, 40);
        assert!(theme_only.iter().any(|r| r.contains("Theme [")));
        assert!(!theme_only.iter().any(|r| r.contains("Look")));
        for look in LookKind::ALL.iter().copied() {
            let rows = picker_text(
                Appearance {
                    theme: 0,
                    look: Some(look),
                },
                80,
                40,
            );
            let at = |text: &str| rows.iter().position(|r| r.contains(text));
            let title = at("Appearance [").expect("titled");
            let row = at(&format!("Look  \u{2039} {} \u{203a}", look.display_name()))
                .expect("the look row");
            let first = at(ALL_THEMES[0].display_name).expect("the themes");
            assert!(title < row && row < first, "{look:?}: {rows:?}");
            let column = |r: usize, text: &str| {
                rows[r][..rows[r].find(text).expect("drawn")]
                    .chars()
                    .count()
            };
            assert_eq!(
                column(row, "Look"),
                column(first, ALL_THEMES[0].display_name),
                "the look's name lines up with the themes'"
            );
        }
    }

    #[test]
    fn theme_swatch_distinguishes_themes() {
        use pixtuoid_scene::theme;
        let cyber = theme_swatch(&theme::CYBERPUNK);
        let normal = theme_swatch(&theme::NORMAL);
        assert_ne!(
            cyber, normal,
            "distinct themes must yield distinct swatches"
        );
        assert_eq!(cyber.0, to_color(theme::CYBERPUNK.ui.neon_brand));
        assert_eq!(cyber.1, to_color(theme::CYBERPUNK.surface.carpet_base));
    }
}
