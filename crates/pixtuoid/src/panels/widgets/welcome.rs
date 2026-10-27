//! The first-run onboarding overlay painter (ratatui). Pure presentation over a
//! `panels::welcome::WelcomeRow` snapshot + an `elapsed_ms` clock that drives the
//! typewriter reveal and the staged "move-in" of roster rows.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{Overflow, Panel, badge_color_for, to_color};
use crate::panels::welcome::OnboardingFrame;
use pixtuoid_scene::theme::Theme;

const WELCOME_W: u16 = 54;
const TYPE_MS_PER_CHAR: u64 = 38;
const ROW_LEAD_MS: u64 = 140;
const ROW_STAGGER_MS: u64 = 110;
const SUBTITLE: &str = "Let's move your agents in. Pick who walks in:";

fn subtitle_done_ms() -> u64 {
    SUBTITLE.chars().count() as u64 * TYPE_MS_PER_CHAR
}

pub(crate) fn paint_welcome(
    f: &mut ratatui::Frame<'_>,
    frame: &OnboardingFrame,
    bounds: Rect,
    theme: &Theme,
) {
    let &OnboardingFrame {
        open: _,
        ref rows,
        selected,
        elapsed_ms,
        dim: _,
    } = frame;
    let dim = Style::default().fg(to_color(theme.ui.label_idle));
    let bright = Style::default().fg(to_color(theme.ui.neon_brand));

    let total = SUBTITLE.chars().count();
    let typed = ((elapsed_ms / TYPE_MS_PER_CHAR) as usize).min(total);
    let shown: String = SUBTITLE.chars().take(typed).collect();
    let caret = if typed < total && (elapsed_ms / 450).is_multiple_of(2) {
        "\u{2588}"
    } else {
        ""
    };
    let above = vec![
        Line::from(Span::styled(format!("  {shown}{caret}"), dim)),
        Line::from(""),
    ];

    // A not-yet-due row reserves a BLANK line, so the staged reveal never resizes
    // the panel.
    let base = subtitle_done_ms() + ROW_LEAD_MS;
    let list: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            if elapsed_ms < base + i as u64 * ROW_STAGGER_MS {
                return Line::from("");
            }
            let is_sel = i == selected;
            let badge = format!("[{:<2}]", row.label_prefix);
            // `[x]`/`[ ]` (not a check glyph) so the checkbox reads in any font.
            let check = if row.checked { "[x]" } else { "[ ]" };
            let mut name_style = if row.checked {
                Style::default().fg(to_color(theme.ui.label_ok))
            } else {
                dim
            };
            if is_sel {
                name_style = name_style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
            }
            Line::from(vec![
                Span::raw(if is_sel { "  \u{25b8} " } else { "    " }),
                Span::styled(
                    badge,
                    Style::default().fg(badge_color_for(row.label_prefix, theme)),
                ),
                Span::raw(" "),
                Span::styled(check.to_string(), if row.checked { bright } else { dim }),
                Span::raw(" "),
                Span::styled(row.display_name.clone(), name_style),
            ])
        })
        .collect();

    // The hint rows stay RESERVED (blank) before `all_in`, so the panel height is
    // constant across the whole reveal.
    let all_in = base + rows.len().saturating_sub(1) as u64 * ROW_STAGGER_MS;
    let shown_hints = elapsed_ms >= all_in;
    let mut below = vec![Line::from("")];
    below.push(if shown_hints {
        Line::from(Span::styled(
            "  \u{2191}\u{2193} move \u{00b7} space toggle \u{00b7} enter connect \u{00b7} esc skip",
            dim,
        ))
    } else {
        Line::from("")
    });
    if cfg!(feature = "audio") {
        below.push(if shown_hints {
            Line::from(Span::styled(
                // Must fit WELCOME_W incl. the 2-col indent, or it clips mid-word.
                // Absent on no-audio builds: a dead m key reads as broken.
                "  \u{2669} office sound \u{2014} press m anytime",
                dim,
            ))
        } else {
            Line::from("")
        });
    }

    Panel {
        title: Some("Welcome to pixtuoid"),
        content_w: WELCOME_W,
        above,
        list,
        below,
        overflow: Overflow::Follow {
            selected: Some(selected),
            scroll: 0,
            cap: None,
        },
    }
    .paint(f, bounds, theme);
}
