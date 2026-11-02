use super::{Overflow, Panel, to_color};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

fn theme_swatch(t: &pixtuoid_scene::theme::Theme) -> (Color, Color) {
    (to_color(t.ui.neon_brand), to_color(t.surface.carpet_base))
}

pub(crate) fn paint_theme_picker(
    f: &mut ratatui::Frame<'_>,
    selected: usize,
    bounds: Rect,
    theme: &pixtuoid_scene::theme::Theme,
) {
    use pixtuoid_scene::theme;
    use ratatui::style::Modifier;
    use ratatui::text::{Line, Span as TSpan};

    /// Fits the title, which outruns a row.
    const THEME_W: u16 = 28;
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
            let name_style = if i == selected {
                Style::default()
                    .fg(to_color(theme.ui.neon_brand))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(to_color(theme.ui.label_idle))
            };
            let (brand, surface) = theme_swatch(t);
            Line::from(vec![
                TSpan::styled(format!("{prefix}{:<name_w$}", t.display_name), name_style),
                TSpan::raw(" "),
                TSpan::styled("\u{2588}", Style::default().fg(brand)),
                TSpan::styled("\u{2588}", Style::default().fg(surface)),
            ])
        })
        .collect();
    Panel {
        title: Some("Theme [\u{2191}\u{2193}/jk] Enter/Esc"),
        content_w: THEME_W,
        above: vec![],
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
            paint_theme_picker(
                f,
                0,
                Rect::new(0, 0, 24, 30),
                &pixtuoid_scene::theme::NORMAL,
            );
        })
        .unwrap();
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
