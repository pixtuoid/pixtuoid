use pixtuoid_core::SceneState;
use pixtuoid_scene::footer::{FooterContext, FooterFloor, FooterInputs, build_footer};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::to_color;

const KEYS_STATS: &str = " [?]help [p]ause [t]heme [q]uit ";
const KEYS_ALERT: &str = " [q]uit ";

/// [`FooterContext::new`] with the TUI's keybind tails.
pub fn footer_context<'a>(
    office: &SceneState,
    floor: Option<FooterFloor>,
    audio_audible: bool,
    volume_flash: Option<u8>,
    source_warning: Option<&'a str>,
) -> FooterContext<'a> {
    FooterContext::new(
        office,
        floor,
        audio_audible,
        volume_flash,
        source_warning,
        KEYS_STATS,
        KEYS_ALERT,
    )
}

pub(crate) fn paint_footer(
    f: &mut ratatui::Frame<'_>,
    inputs: &FooterInputs<'_>,
    full_rect: Rect,
    theme: &pixtuoid_scene::theme::Theme,
) {
    use ratatui::text::Line;
    let spans = build_status_spans(inputs, full_rect.width, theme);
    // Base style on the whole row so cells past the rendered spans keep the muted
    // footer tone rather than the terminal default.
    let footer =
        Paragraph::new(Line::from(spans)).style(Style::default().fg(to_color(theme.ui.label_idle)));
    f.render_widget(
        footer,
        Rect {
            x: full_rect.x,
            y: full_rect.y + full_rect.height.saturating_sub(1),
            width: full_rect.width,
            height: 1,
        },
    );
}

pub(crate) fn build_status_spans<'a>(
    inputs: &FooterInputs<'_>,
    term_width: u16,
    theme: &pixtuoid_scene::theme::Theme,
) -> Vec<Span<'a>> {
    build_footer(inputs, term_width)
        .segments
        .into_iter()
        .map(|seg| Span::styled(seg.text, Style::default().fg(to_color(seg.tone.rgb(theme)))))
        .collect()
}

/// Byte-identical to `build_status_spans`'s content — the oracle that locks the exact
/// footer wording.
#[cfg(test)]
pub(crate) fn build_status_summary(inputs: &FooterInputs<'_>, term_width: u16) -> String {
    build_footer(inputs, term_width).text()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_core::state::{ActivityState, ToolUse};
    use pixtuoid_core::{AgentId, AgentSlot, GlobalDeskIndex};
    use pixtuoid_scene::footer::{FooterTone, RungKind};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::SystemTime;

    #[test]
    fn build_status_spans_tints_the_running_rung_via_the_shared_tone_authority() {
        let theme = &pixtuoid_scene::theme::NORMAL;
        let slot = AgentSlot {
            agent_id: AgentId::from_transcript_path("/p/a.jsonl"),
            source: Arc::from("cc"),
            session_id: Arc::from("s"),
            cwd: Arc::from(PathBuf::from("/p").as_path()),
            label: "a".into(),
            state: ActivityState::Running {
                item: Some(ToolUse {
                    tool_use_id: Some(Arc::from("t")),
                    detail: Some(Arc::from("Bash ls")),
                    kind: pixtuoid_core::state::ToolKind::Bash,
                }),
            },
            state_started_at: SystemTime::UNIX_EPOCH,
            created_at: SystemTime::UNIX_EPOCH,
            last_event_at: SystemTime::UNIX_EPOCH,
            exiting_at: None,
            desk_index: GlobalDeskIndex(0),
            floor_idx: 0,
            tool_call_count: 0,
            running_ms: 0,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        };
        let mut scene = SceneState::uniform(16);
        scene.agents.insert(slot.agent_id, slot);
        let inputs = FooterInputs::new(&scene, footer_context(&scene, None, false, None, None));
        let spans = build_status_spans(&inputs, 200, theme);
        let running_rgb = FooterTone::Rung(RungKind::Running).rgb(theme);
        let rung = spans
            .iter()
            .find(|s| s.content.contains("\u{25cf}1 R"))
            .expect("running rung span present");
        assert_eq!(rung.style.fg, Some(to_color(running_rgb)));
    }
}
