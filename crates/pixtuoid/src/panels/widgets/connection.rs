//! The Sources panel painter (ratatui). Pure presentation over the pre-built row
//! list + per-frame live facet from `panels::connection`; all model logic lives
//! there.

use std::time::{Duration, SystemTime};

use super::{Overflow, Panel, marquee_or_truncate, marquee_window, source_badge_span, to_color};

use crate::panels::connection::{
    ConnState, ConnectionFrame, ConnectionRow, LiveFacet, LiveInfo, no_action_hint,
};
use pixtuoid_scene::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

const CONNECTION_POPUP_W: u16 = 66;
const NAME_W: usize = 13;
const CONN_W: usize = 15;

/// The column header, kept as one fn so the "Live" position can't drift from the
/// data row. The two trailing spaces before "Live" mirror the fixed 2-col
/// `health_flag` slot each data row carries — without them "Live" sits 2 cols
/// left of its data.
fn column_header() -> String {
    format!(
        "  {:<18}{:<width$}  Live",
        "CLI",
        "Connection",
        width = CONN_W
    )
}

pub(crate) fn paint_connection_panel(
    f: &mut ratatui::Frame<'_>,
    frame: &ConnectionFrame,
    now: SystemTime,
    bounds: Rect,
    theme: &Theme,
) {
    let &ConnectionFrame {
        open: _,
        ref rows,
        selected,
        confirm,
        ref result,
        ref socket_line,
    } = frame;
    let dim = Style::default().fg(to_color(theme.ui.label_idle));

    // The detail marquee's width budget needs the inner WIDTH before the row count
    // (hence the height) is known — the height-independent two-phase seam. `None`
    // ⇒ the terminal is too narrow to render at all.
    let Some(inner_w) = Panel::inner_width(bounds, CONNECTION_POPUP_W) else {
        return;
    };

    let above = vec![
        Line::from(Span::styled(format!("  {socket_line}"), dim)),
        Line::from(""),
        Line::from(Span::styled(column_header(), dim)),
    ];

    let list: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .map(|(i, (row, live))| connection_line(row, live, selected == i, now, theme))
        .collect();

    let detail = if let Some(ci) = confirm {
        let name = rows.get(ci).map_or("", |(r, _)| r.display_name);
        format!("\u{26a0} disconnect {name}? (y/n)")
    } else if let Some(res) = result {
        res.clone()
    } else if let Some((row, _)) = rows.get(selected) {
        if let Some(h) = &row.health {
            h.clone()
        } else {
            // The install path is surfaced ONLY when Connected — otherwise it is a
            // meaningless future destination.
            match row.state {
                ConnState::Connected => match &row.config_path {
                    Some(p) => format!("installed at: {}", p.display()),
                    None => "connected".to_string(),
                },
                ConnState::Disconnected => "disconnected \u{2014} press t to connect".to_string(),
                ConnState::NoCli { .. } => no_action_hint(row),
            }
        }
    } else {
        String::new()
    };
    // Reserves the 2-space left indent + a symmetric 2-col right margin, so a
    // full-width scroll never runs flush to the panel edge.
    let detail_w = (inner_w as usize).saturating_sub(4);
    let below = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", marquee_window(&detail, detail_w, now)),
            dim,
        )),
        Line::from(Span::styled(
            "  j/k move \u{00b7} t toggle \u{00b7} s/esc close",
            dim,
        )),
    ];

    Panel {
        title: Some("Sources \u{2014} s/esc close"),
        content_w: CONNECTION_POPUP_W,
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

/// One CLI row: a colored badge, the name (tinted/reversed by selection), the
/// connection-state column, and the live-activity column.
fn connection_line(
    row: &ConnectionRow,
    live: &LiveInfo,
    is_selected: bool,
    now: SystemTime,
    theme: &Theme,
) -> Line<'static> {
    let prefix = if is_selected { "\u{25b8} " } else { "  " };

    let badge_tag = row.label_prefix;

    let base = if is_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };

    let (c_glyph, c_text, c_color) = match row.state {
        ConnState::Connected => ('\u{25cf}', "connected", theme.ui.label_ok),
        ConnState::Disconnected => ('\u{25cb}', "disconnected", theme.ui.label_idle),
        ConnState::NoCli { .. } => ('\u{2014}', "no CLI", theme.ui.label_idle),
    };
    // glyph + space (2) + text padded to CONN_W - 2.
    let conn_cell = format!("{c_glyph} {:<width$}", c_text, width = CONN_W - 2);

    let (l_glyph, l_text, l_color) = if live.dead {
        (
            '\u{26a0}',
            "transport died".to_string(),
            theme.ui.label_warn,
        )
    } else {
        match &live.facet {
            LiveFacet::Agents {
                agents: 0,
                last_event_age: _,
            } => ('\u{25cc}', "idle".to_string(), theme.ui.label_idle),
            LiveFacet::Agents {
                agents,
                last_event_age,
            } => {
                let age = last_event_age.map(fmt_age).unwrap_or_default();
                let plural = if *agents == 1 { "" } else { "s" };
                (
                    '\u{25cf}',
                    format!("{agents} agent{plural} \u{00b7} {age} ago"),
                    theme.ui.label_ok,
                )
            }
            // Zero instances reports the OBSERVATION, not a verdict: presence is
            // announce-driven, so a gateway that announced before this pixtuoid
            // started is alive and merely unheard. A diagnosis surface must not
            // assert a fact it cannot observe.
            LiveFacet::Daemon(None) => (
                '\u{25cc}',
                "no gateway seen".to_string(),
                theme.ui.label_idle,
            ),
            LiveFacet::Daemon(Some(rollup)) => {
                let instances = rollup.instances.get();
                let plural = if instances == 1 { "" } else { "s" };
                // The WORD and the hue both come from the shared board model, the
                // exact pair the footer's `⬢gw` chip renders, so the panel can't
                // describe the same gateway differently from the chip below it.
                let rolled = rollup.state;
                (
                    '\u{25cf}',
                    format!(
                        "{instances} gateway{plural} \u{00b7} {}",
                        pixtuoid_scene::neon_sign::gateway_label(rolled)
                    ),
                    pixtuoid_scene::neon_sign::gateway_tone(rolled).rgb(theme),
                )
            }
        }
    };

    let name_cell = format!(
        "{:<NAME_W$}",
        marquee_or_truncate(row.display_name, NAME_W, is_selected, now)
    );

    // A fixed 2-col slot — blank rather than absent, to keep the Live column
    // aligned. SEPARATE from the Connection column on purpose: ConnState is the
    // lifecycle, health is the sub-state it annotates.
    let health_flag = if row.health.is_some() {
        "\u{26a0} "
    } else {
        "  "
    };

    Line::from(vec![
        Span::raw(prefix),
        source_badge_span(badge_tag, theme),
        Span::raw(" "),
        Span::styled(name_cell, base.fg(to_color(theme.ui.tooltip_text))),
        Span::styled(conn_cell, base.fg(to_color(c_color))),
        Span::styled(
            health_flag.to_string(),
            base.fg(to_color(theme.ui.label_warn)),
        ),
        Span::styled(format!("{l_glyph} {l_text}"), base.fg(to_color(l_color))),
    ])
}

fn fmt_age(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

#[cfg(test)]
mod tests {

    /// `n == 0` is `Daemon(None)` by construction — the point of the atomic pair.
    fn daemon_facet(n: usize, state: pixtuoid_core::state::DaemonState) -> LiveFacet {
        LiveFacet::Daemon(
            std::num::NonZeroUsize::new(n).map(|instances| DaemonRollup { instances, state }),
        )
    }
    use super::*;
    use crate::panels::connection::DaemonRollup;
    use crate::sources::{RowFacts, RowInput};
    use pixtuoid_core::state::DaemonState;
    use pixtuoid_scene::theme::NORMAL;

    fn row(source_id: &'static str, label_prefix: &'static str, state: ConnState) -> ConnectionRow {
        ConnectionRow {
            source_id,
            label_prefix,
            display_name: "Name",
            state,
            config_path: None,
            target: None,
            health: None,
        }
    }

    #[test]
    fn connection_line_badge_uses_source_color_and_is_never_reversed() {
        let r = row("codex", "cx", ConnState::Disconnected);
        let line = connection_line(
            &r,
            &LiveInfo::default(),
            true,
            SystemTime::UNIX_EPOCH,
            &NORMAL,
        );
        let badge = &line.spans[1];
        assert_eq!(badge.style.fg, Some(to_color(NORMAL.source.codex)));
        assert!(!badge.style.add_modifier.contains(Modifier::REVERSED));
        assert!(
            line.spans[3]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    // Char-count == column here only because every glyph is single-width BMP.
    #[test]
    fn live_header_aligns_with_the_live_data_column() {
        let header = column_header();
        let header_live_col = header.find("Live").expect("header has a Live column");

        for health in [None, Some("install broken".to_string())] {
            let r = ConnectionRow {
                source_id: "claude",
                label_prefix: "cc",
                display_name: "Name",
                state: ConnState::Connected,
                config_path: None,
                target: None,
                health,
            };
            let line = connection_line(
                &r,
                &LiveInfo::default(),
                false,
                SystemTime::UNIX_EPOCH,
                &NORMAL,
            );
            let n = line.spans.len();
            let live_col: usize = line.spans[..n - 1]
                .iter()
                .map(|s| s.content.chars().count())
                .sum();
            assert_eq!(
                header_live_col, live_col,
                "header Live col must match data live col; header={header:?}"
            );
        }
    }

    #[test]
    fn connection_line_no_cli_state_renders_no_cli_cell() {
        let r = row("some-cli", "xx", ConnState::NoCli { connected: false });
        let line = connection_line(
            &r,
            &LiveInfo::default(),
            false,
            SystemTime::UNIX_EPOCH,
            &NORMAL,
        );
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("no CLI"), "NoCli cell text missing: {text:?}");
        assert!(
            text.contains('\u{2014}'),
            "NoCli em-dash glyph missing: {text:?}"
        );
        assert!(
            !text.contains("connected"),
            "NoCli must not say connected: {text:?}"
        );
        assert!(
            !text.contains("disconnected"),
            "NoCli must not say disconnected: {text:?}"
        );
    }

    #[test]
    fn connection_line_renders_state_and_live_text() {
        let r = row("claude", "cc", ConnState::Connected);
        let live = LiveInfo {
            facet: LiveFacet::Agents {
                agents: 2,
                last_event_age: Some(Duration::from_secs(3)),
            },
            dead: false,
        };
        let line = connection_line(&r, &live, false, SystemTime::UNIX_EPOCH, &NORMAL);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("[cc]"));
        assert!(text.contains("connected"));
        assert!(text.contains("2 agents"));
        assert!(text.contains("3s ago"));
    }

    #[test]
    fn connection_line_daemon_names_its_gateways_and_their_state() {
        let r = row("openclaw", "ok", ConnState::Connected);
        let cell = |facet: LiveFacet| -> String {
            let live = LiveInfo { facet, dead: false };
            connection_line(&r, &live, false, SystemTime::UNIX_EPOCH, &NORMAL)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };

        let stopped = cell(LiveFacet::Daemon(None));
        assert!(stopped.contains("no gateway seen"), "{stopped}");
        assert!(
            !stopped.chars().any(|c| c.is_ascii_digit()),
            "a zero-instance daemon must report no count: {stopped}"
        );
        for live_word in [
            pixtuoid_scene::neon_sign::gateway_label(DaemonState::Running),
            pixtuoid_scene::neon_sign::gateway_label(DaemonState::Degraded),
            pixtuoid_scene::neon_sign::gateway_label(DaemonState::Down),
        ] {
            assert!(
                !stopped.contains(live_word),
                "a zero-instance daemon must not borrow a live state word ({live_word}): {stopped}"
            );
        }

        let running = cell(daemon_facet(2, DaemonState::Running));
        assert!(running.contains("2 gateways"), "{running}");
        assert!(
            running.contains(pixtuoid_scene::neon_sign::gateway_label(
                DaemonState::Running
            )),
            "the state word must be the shared board vocabulary: {running}"
        );

        let one = cell(daemon_facet(1, DaemonState::Degraded));
        assert!(
            one.contains("1 gateway \u{00b7}"),
            "singular, no 's': {one}"
        );
        assert!(
            one.contains(pixtuoid_scene::neon_sign::gateway_label(
                DaemonState::Degraded
            )),
            "{one}"
        );
    }

    #[test]
    fn connection_line_dead_transport_overrides_live_column() {
        let r = row("codex", "cx", ConnState::Connected);
        let live = LiveInfo {
            facet: LiveFacet::Agents {
                agents: 1,
                last_event_age: Some(Duration::from_secs(1)),
            },
            dead: true,
        };
        let line = connection_line(&r, &live, false, SystemTime::UNIX_EPOCH, &NORMAL);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("transport died"));
    }

    #[test]
    fn connection_line_singular_vs_plural_agents() {
        let r = row("claude", "cc", ConnState::Connected);
        let one = connection_line(
            &r,
            &LiveInfo {
                facet: LiveFacet::Agents {
                    agents: 1,
                    last_event_age: Some(Duration::from_secs(0)),
                },
                dead: false,
            },
            false,
            SystemTime::UNIX_EPOCH,
            &NORMAL,
        );
        let t1: String = one.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(t1.contains("1 agent "), "singular: {t1}");
        assert!(!t1.contains("1 agents"));
    }

    #[test]
    fn connection_line_selected_long_name_scrolls_unselected_truncates() {
        let r = ConnectionRow {
            source_id: "x",
            label_prefix: "cc",
            display_name: "A-Very-Long-CLI-Display-Name-That-Overflows",
            state: ConnState::Connected,
            config_path: None,
            target: None,
            health: None,
        };
        let unsel = connection_line(
            &r,
            &LiveInfo::default(),
            false,
            SystemTime::UNIX_EPOCH,
            &NORMAL,
        );
        let name_unsel = unsel.spans[3].content.to_string();
        assert!(
            name_unsel.contains('\u{2026}'),
            "unselected long name must ellipsize: {name_unsel:?}"
        );
        let t1 = SystemTime::UNIX_EPOCH + Duration::from_millis(3000);
        let n0 = connection_line(
            &r,
            &LiveInfo::default(),
            true,
            SystemTime::UNIX_EPOCH,
            &NORMAL,
        )
        .spans[3]
            .content
            .to_string();
        let n1 = connection_line(&r, &LiveInfo::default(), true, t1, &NORMAL).spans[3]
            .content
            .to_string();
        assert!(
            !n0.contains('\u{2026}'),
            "selected scrolling name must not ellipsize: {n0:?}"
        );
        assert_ne!(n0, n1, "selected name must animate across time");
    }

    #[test]
    fn every_registry_source_has_a_non_fallback_badge_color() {
        use crate::sources::build_rows_from;
        use pixtuoid_core::source::registry::REGISTRY;
        let fallback = to_color(NORMAL.ui.label_idle);
        // Build through the real builder so the prefixes come from the registry.
        let inputs: Vec<RowInput> = REGISTRY
            .iter()
            .map(|d| RowInput {
                source_id: d.name,
                label_prefix: d.label_prefix,
                target: None,
                health: None,
                facts: Some(RowFacts {
                    present: true,
                    config_path: None,
                }),
                connected: true,
            })
            .collect();
        for sr in build_rows_from(inputs) {
            let line = connection_line(
                &sr,
                &LiveInfo::default(),
                false,
                SystemTime::UNIX_EPOCH,
                &NORMAL,
            );
            assert_ne!(
                line.spans[1].style.fg,
                Some(fallback),
                "source {:?} (prefix {:?}) renders the idle FALLBACK badge — add its arm to connection_line",
                sr.source_id,
                sr.label_prefix,
            );
        }
    }
}
