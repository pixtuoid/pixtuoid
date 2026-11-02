use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{Overflow, Panel, to_color};
use pixtuoid_scene::theme::Theme;

/// A key and what it does, one help row.
pub(crate) type Shortcut = (&'static str, &'static str);

/// The shared rows over the appearance key's.
const OVER: &[Shortcut] = &[
    ("q", "quit"),
    ("Ctrl+C", "quit"),
    ("p", "pause / resume"),
    #[cfg(feature = "audio")]
    ("m", "sound on/off"),
    #[cfg(feature = "audio")]
    ("+/-", "volume; + unmutes"),
];

/// The appearance key's row: the look it sets too where the picker offers one.
fn appearance(look_offered: bool) -> Shortcut {
    (
        "t",
        if look_offered {
            "look & theme"
        } else {
            "themes"
        },
    )
}

/// The shared rows under the appearance key's.
const UNDER: &[Shortcut] = &[
    ("Tab", "agent dashboard"),
    ("s", "sources (connect / health)"),
    ("?", "toggle this overlay"),
    ("\u{2191} \u{2193} j k", "switch floor"),
    ("PgUp / PgDn", "switch floor"),
    ("click agent", "focus its terminal"),
    ("f (dashboard)", "focus selected agent's terminal"),
    ("v", "what's new (version popup)"),
    ("Enter / Esc", "dismiss popup"),
];

const ROW_INDENT: &str = "  ";

/// Every row the help lists: the shared ones, then `host`'s.
fn rows(host: &[Shortcut], look_offered: bool) -> impl Iterator<Item = Shortcut> + Clone + '_ {
    OVER.iter()
        .copied()
        .chain(std::iter::once(appearance(look_offered)))
        .chain(UNDER.iter().copied())
        .chain(host.iter().copied())
}

/// The widest key plus a one-space gutter, so even a full-width key keeps a gap
/// before its description instead of running into it.
fn key_col_width(rows: impl Iterator<Item = Shortcut>) -> usize {
    rows.map(|(k, _)| k.chars().count()).max().unwrap_or(0) + 1
}

/// DERIVED from the rows it lists, never a literal — a hardcoded width hard-clips
/// every long row mid-word at ANY terminal size (the panel is centered, not
/// edge-limited).
fn content_width(rows: impl Iterator<Item = Shortcut> + Clone) -> u16 {
    let widest_desc = rows
        .clone()
        .map(|(_, d)| d.chars().count())
        .max()
        .unwrap_or(0);
    (ROW_INDENT.chars().count() + key_col_width(rows) + widest_desc) as u16
}

/// The shared shortcuts, then `host`'s own; `look_offered` where the
/// appearance picker has a look row.
pub(crate) fn paint_help_overlay(
    f: &mut ratatui::Frame<'_>,
    bounds: Rect,
    theme: &Theme,
    host: &[Shortcut],
    look_offered: bool,
) {
    let rows = rows(host, look_offered);
    let key_col = key_col_width(rows.clone());
    let mut lines: Vec<Line<'static>> = vec![Line::from("")];
    for (key, desc) in rows.clone() {
        lines.push(Line::from(vec![
            Span::raw(ROW_INDENT),
            Span::styled(
                format!("{key:<key_col$}"),
                Style::default()
                    .fg(to_color(theme.ui.neon_brand))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                desc.to_string(),
                Style::default().fg(to_color(theme.ui.label_idle)),
            ),
        ]));
    }
    Panel {
        title: Some("? Keyboard"),
        content_w: content_width(rows),
        above: vec![],
        list: lines,
        below: vec![],
        overflow: Overflow::CueOnly,
    }
    .paint(f, bounds, theme);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_at(w: u16, h: u16) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            paint_help_overlay(
                f,
                Rect::new(0, 0, w, h),
                &pixtuoid_scene::theme::NORMAL,
                &[],
                true,
            );
        })
        .unwrap();
    }

    fn frame_text(w: u16, h: u16, host: &[Shortcut]) -> String {
        frame_offering(w, h, host, false)
    }

    fn frame_offering(w: u16, h: u16, host: &[Shortcut], look_offered: bool) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            paint_help_overlay(
                f,
                Rect::new(0, 0, w, h),
                &pixtuoid_scene::theme::NORMAL,
                host,
                look_offered,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let area = buf.area;
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(c) = buf.cell((x, y)) {
                    out.push_str(c.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    /// The walkable debug layer is the half-blocks': the shared rows, which
    /// the window's help draws, never list its key.
    #[test]
    fn the_shared_rows_do_not_list_the_walkable_key() {
        assert!(!frame_text(140, 40, &[]).contains("walkable"));
    }

    #[test]
    fn every_shortcut_row_renders_in_full() {
        for offered in [false, true] {
            let text = frame_offering(140, 40, &[], offered);
            let key_col = key_col_width(rows(&[], offered));
            for (key, desc) in rows(&[], offered) {
                let row = format!("{ROW_INDENT}{key:<key_col$}{desc}");
                assert!(
                    text.contains(&row),
                    "shortcut row clipped or gutter lost: {row:?}\nframe:\n{text}"
                );
            }
        }
    }

    /// The appearance key's row names the look only where the picker has a
    /// look row, and is the key the dispatch opens the picker on.
    #[test]
    fn the_appearance_row_names_the_look_only_where_offered() {
        assert_eq!(
            appearance(false).0,
            crate::panels::APPEARANCE_KEY.to_string()
        );
        let row = |offered| {
            frame_offering(140, 40, &[], offered)
                .lines()
                .find(|l| l.trim_start().starts_with("t "))
                .map(|l| l.split_whitespace().skip(1).collect::<Vec<_>>().join(" "))
        };
        assert_eq!(row(false).as_deref(), Some("themes"));
        assert_eq!(row(true).as_deref(), Some("look & theme"));
    }

    /// A host's own shortcuts list after the shared ones, in the same key
    /// column, which widens to the widest; a host with none lists none.
    #[test]
    fn a_hosts_shortcuts_list_after_the_shared_ones() {
        let host: &[Shortcut] = &[("a-much-wider-key-than-any", "host only")];
        let text = frame_text(160, 50, host);
        let key_col = key_col_width(rows(host, false));
        assert!(
            key_col > key_col_width(rows(&[], false)),
            "the column widens to the host's key"
        );
        for (key, desc) in rows(host, false) {
            let row = format!("{ROW_INDENT}{key:<key_col$}{desc}");
            assert!(text.contains(&row), "row {row:?} missing:\n{text}");
        }
        assert!(!frame_text(160, 50, &[]).contains("host only"));
    }

    #[test]
    fn help_overlay_renders_without_panic_across_sizes() {
        // Once the footer row is reserved, the last two sizes fall under
        // `PanelGeometry`'s minimum envelope and paint nothing.
        for (w, h) in [(200, 60), (40, 20), (24, 30), (10, 4), (4, 3), (2, 2)] {
            render_at(w, h);
        }
    }
}
