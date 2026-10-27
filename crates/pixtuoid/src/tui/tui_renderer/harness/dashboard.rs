use super::*;

use crate::panels::connection::ConnectionFrame;
use crate::panels::dashboard::{DashboardFolds, DashboardFrame, build_dashboard_rows};

#[test]
fn dashboard_popup_renders_labels_states_and_live_tool() {
    let mut r = build(120, 44, vec![]);
    let mut a = running("/h/alpha.jsonl", 0, "Edit reducer.rs", t0());
    a.label = "cc\u{b7}alpha".into();
    let mut b = idle("/h/beta.jsonl", 1, t0());
    b.label = "cc\u{b7}beta".into();
    let scene = scene_with(vec![a, b], 16);

    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    let first = rows[0].agent_id;
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                selected: Some(first),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    let text = frame_text(r.frame_buffer());
    assert!(text.contains("Agents ("), "header missing:\n{text}");
    assert!(text.contains("cc\u{b7}alpha"), "alpha row missing:\n{text}");
    assert!(text.contains("cc\u{b7}beta"), "beta row missing:\n{text}");
    assert!(
        text.contains("Edit reducer.rs"),
        "live tool detail missing:\n{text}"
    );
    assert!(text.contains("idle"), "idle state missing:\n{text}");
}

#[test]
fn connection_panel_renders_both_facets_borderless() {
    use crate::panels::connection::{ConnState, ConnectionRow, LiveFacet, LiveInfo};
    let mut r = build(120, 44, vec![]);
    let scene = scene_with(vec![], 16);
    let rows = vec![
        (
            ConnectionRow {
                source_id: "claude",
                label_prefix: "cc",
                display_name: "Claude Code",
                state: ConnState::Connected,
                config_path: Some(std::path::PathBuf::from("~/.claude/settings.json")),
                target: None,
                health: None,
            },
            LiveInfo {
                facet: LiveFacet::Agents {
                    agents: 2,
                    last_event_age: Some(std::time::Duration::from_secs(3)),
                },
                dead: false,
            },
        ),
        (
            ConnectionRow {
                source_id: "antigravity",
                label_prefix: "ag",
                display_name: "Antigravity",
                state: ConnState::Disconnected,
                config_path: None,
                target: None,
                health: None,
            },
            LiveInfo {
                facet: LiveFacet::Agents {
                    agents: 1,
                    last_event_age: Some(std::time::Duration::from_secs(12)),
                },
                dead: false,
            },
        ),
    ];
    r.set_frames(
        RenderFrames {
            connection: ConnectionFrame {
                open: true,
                rows,
                socket_line: "socket  /tmp/p.sock".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    let text = frame_text(r.frame_buffer());
    assert!(text.contains("Sources"), "title missing:\n{text}");
    assert!(text.contains("[cc]"), "cc badge missing:\n{text}");
    assert!(text.contains("[ag]"), "ag badge missing:\n{text}");
    assert!(text.contains("2 agents"), "live count missing:\n{text}");
    assert!(text.contains("socket"), "socket line missing:\n{text}");
    assert!(
        text.contains("installed at"),
        "connected detail (install path) missing:\n{text}"
    );
    let popup = dash_popup(r.frame_buffer());
    for g in [
        '\u{256d}', '\u{256e}', '\u{2570}', '\u{256f}', '\u{2502}', '\u{2500}',
    ] {
        assert!(
            !popup.contains(g),
            "Sources panel must be borderless, found {g}:\n{popup}"
        );
    }
}

// The health string here is glyph-FREE, so the only ⚠ in the buffer is the
// per-row flag the painter adds — proving the flag specifically.
#[test]
fn connection_panel_health_flag_and_detail_preempt_the_install_path() {
    use crate::panels::connection::{ConnState, ConnectionRow, LiveInfo};
    let mut r = build(120, 44, vec![]);
    let scene = scene_with(vec![], 16);
    let rows = vec![(
        ConnectionRow {
            source_id: "reasonix",
            label_prefix: "rx",
            display_name: "Reasonix",
            state: ConnState::Connected,
            config_path: Some(std::path::PathBuf::from("~/.reasonix/settings.json")),
            target: None,
            health: Some("install broken: shim binary missing".into()), // NO ⚠ prefix
        },
        LiveInfo::default(),
    )];
    r.set_frames(
        RenderFrames {
            connection: ConnectionFrame {
                open: true,
                rows,
                socket_line: "socket  /tmp/p.sock".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains('\u{26a0}'),
        "the per-row health flag (⚠) must render (health string carries none):\n{text}"
    );
    assert!(
        text.contains("install broken"),
        "the full reason must show in the detail line:\n{text}"
    );
    assert!(
        !text.contains("installed at"),
        "the health verdict must PREEMPT the install-path hint:\n{text}"
    );
}

#[test]
fn connection_panel_armed_shows_confirm_prompt() {
    use crate::panels::connection::{ConnState, ConnectionRow, LiveInfo};
    let mut r = build(120, 44, vec![]);
    let scene = scene_with(vec![], 16);
    let rows = vec![(
        ConnectionRow {
            source_id: "codex",
            label_prefix: "cx",
            display_name: "Codex",
            state: ConnState::Connected,
            config_path: Some(std::path::PathBuf::from("~/.codex/config.toml")),
            target: None,
            health: None,
        },
        LiveInfo::default(),
    )];
    r.set_frames(
        RenderFrames {
            connection: ConnectionFrame {
                open: true,
                rows,
                confirm: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("(y/n)"),
        "armed confirm prompt missing:\n{text}"
    );
    assert!(text.contains("Codex"), "armed target name missing:\n{text}");
}

#[test]
fn connection_panel_disconnected_selected_shows_connect_hint() {
    use crate::panels::connection::{ConnState, ConnectionRow, LiveInfo};
    let mut r = build(120, 44, vec![]);
    let scene = scene_with(vec![], 16);
    let rows = vec![(
        ConnectionRow {
            source_id: "antigravity",
            label_prefix: "ag",
            display_name: "Antigravity",
            state: ConnState::Disconnected,
            config_path: None,
            target: None,
            health: None,
        },
        LiveInfo::default(),
    )];
    r.set_frames(
        RenderFrames {
            connection: ConnectionFrame {
                open: true,
                rows,
                socket_line: "socket  /tmp/p.sock".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("press t to connect"),
        "disconnected detail hint missing:\n{text}"
    );
    assert!(
        !text.contains("installed at"),
        "a disconnected row must NOT show an install path:\n{text}"
    );
}

#[test]
fn connection_panel_no_cli_selected_shows_not_detected_hint() {
    use crate::panels::connection::{ConnState, ConnectionRow, LiveInfo};
    let mut r = build(120, 44, vec![]);
    let scene = scene_with(vec![], 16);
    let rows = vec![(
        ConnectionRow {
            source_id: "codex",
            label_prefix: "cx",
            display_name: "Codex",
            state: ConnState::NoCli { connected: false },
            config_path: None,
            target: None,
            health: None,
        },
        LiveInfo::default(),
    )];
    r.set_frames(
        RenderFrames {
            connection: ConnectionFrame {
                open: true,
                rows,
                socket_line: "socket  /tmp/p.sock".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("not detected on this machine"),
        "no-CLI detail hint missing:\n{text}"
    );
    assert!(
        text.contains("Codex"),
        "no-CLI hint must name the CLI:\n{text}"
    );
}

#[test]
fn connection_panel_connected_without_config_path_shows_connected() {
    use crate::panels::connection::{ConnState, ConnectionRow, LiveInfo};
    let mut r = build(120, 44, vec![]);
    let scene = scene_with(vec![], 16);
    let rows = vec![(
        ConnectionRow {
            source_id: "claude",
            label_prefix: "cc",
            display_name: "Claude Code",
            state: ConnState::Connected,
            config_path: None,
            target: None,
            health: None,
        },
        LiveInfo::default(),
    )];
    r.set_frames(
        RenderFrames {
            connection: ConnectionFrame {
                open: true,
                rows,
                socket_line: "socket  /tmp/p.sock".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("connected"),
        "connected fallback detail missing:\n{text}"
    );
    assert!(
        !text.contains("installed at"),
        "no config_path → must NOT show an install path:\n{text}"
    );
}

#[test]
fn connection_panel_last_result_overrides_per_state_detail() {
    use crate::panels::connection::{ConnState, ConnectionRow, LiveInfo};
    let mut r = build(120, 44, vec![]);
    let scene = scene_with(vec![], 16);
    let rows = vec![(
        ConnectionRow {
            source_id: "claude",
            label_prefix: "cc",
            display_name: "Claude Code",
            state: ConnState::Connected,
            config_path: Some(std::path::PathBuf::from("~/.claude/settings.json")),
            target: None,
            health: None,
        },
        LiveInfo::default(),
    )];
    r.set_frames(
        RenderFrames {
            connection: ConnectionFrame {
                open: true,
                rows,
                result: Some("X-RESULT-SENTINEL".to_string()),
                socket_line: "socket  /tmp/p.sock".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("X-RESULT-SENTINEL"),
        "last_result string must render in the detail line:\n{text}"
    );
    assert!(
        !text.contains("installed at"),
        "last_result must PREEMPT the per-state install hint:\n{text}"
    );
}

#[test]
fn connection_panel_empty_rows_renders_panel_with_blank_detail() {
    let mut r = build(120, 44, vec![]);
    let scene = scene_with(vec![], 16);
    r.set_frames(
        RenderFrames {
            connection: ConnectionFrame {
                open: true,
                socket_line: "socket  /tmp/p.sock".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(text.contains("Sources"), "title missing:\n{text}");
    assert!(text.contains("j/k move"), "footer missing:\n{text}");
    assert!(
        !text.contains("installed at"),
        "empty rows → blank detail, no install hint:\n{text}"
    );
    assert!(
        !text.contains("press t to connect"),
        "empty rows → blank detail, no connect hint:\n{text}"
    );
    assert!(
        !text.contains("not detected"),
        "empty rows → blank detail, no no-CLI hint:\n{text}"
    );
}

#[test]
fn dashboard_collapsed_big_tree_shows_badge_and_hides_children() {
    let mut r = build(120, 44, vec![]);
    let root_id = AgentId::from_transcript_path("/h/root.jsonl");
    let mut root = slot(root_id, 0, 0, t0());
    root.label = "cc\u{b7}root".into();
    let mut agents = vec![root];
    // Past the threshold → the root auto-collapses on open.
    let children = crate::panels::dashboard::AUTO_COLLAPSE_THRESHOLD + 1;
    for i in 0..children {
        let cid = AgentId::from_transcript_path(&format!("/h/root/subagents/agent-{i}.jsonl"));
        let mut c = slot(cid, 0, 1 + i, t0());
        c.label = format!("explorer{i}").into();
        c.parent_id = Some(root_id);
        agents.push(c);
    }
    let scene = scene_with(agents, 16);

    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                selected: Some(root_id),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    let text = frame_text(r.frame_buffer());
    assert!(text.contains("cc\u{b7}root"), "root row missing:\n{text}");
    assert!(
        text.contains(&format!("({children})")),
        "collapsed hidden-count badge missing:\n{text}"
    );
    // The popup's own count proves only the root is listed: a global
    // `!contains("explorer0")` would false-fail on the office sprite label
    // painted behind the popup.
    assert!(
        text.contains("Agents (1)"),
        "collapsed tree must list exactly one row:\n{text}"
    );
}

#[test]
fn dashboard_closed_paints_no_popup() {
    let mut r = build(120, 44, vec![]);
    let scene = scene_with(vec![idle("/h/a.jsonl", 0, t0())], 16);
    // Real rows, so only the `open` gate stands between them and an `Agents (1)` title.
    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: false,
                rows,
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    let text = frame_text(r.frame_buffer());
    assert!(
        !text.contains("Agents ("),
        "no dashboard popup when closed:\n{text}"
    );
}

/// The popup's content lines, so substring assertions don't false-match the
/// office sprite labels behind it. The popup is borderless (no `│` to key on),
/// so find its rect by its `tooltip_bg` fill: the span most rows' runs wider
/// than a badge's plate, [`BADGE_CELLS`](pixtuoid_scene::badge::BADGE_CELLS),
/// share, which no bubble on the same fill can. Every cell inside the rect is
/// read, whatever its background, so a popup row a cell of another colour
/// splits is read whole.
fn dash_popup(buf: &ratatui::buffer::Buffer) -> String {
    let tb = pixtuoid_scene::theme::NORMAL.ui.tooltip_bg;
    let bg = ratatui::style::Color::Rgb(tb.r, tb.g, tb.b);
    let area = buf.area;
    // Each run of the fill wider than a badge's plate, as (row, first, last).
    let mut wide = Vec::new();
    for y in area.y..area.y + area.height {
        let mut start = None;
        for x in area.x..=area.x + area.width {
            let on = x < area.x + area.width && buf[(x, y)].bg == bg;
            match (on, start) {
                (true, None) => start = Some(x),
                (false, Some(x0)) => {
                    if x - x0 > pixtuoid_scene::badge::BADGE_CELLS {
                        wide.push((y, x0, x - 1));
                    }
                    start = None;
                }
                _ => {}
            }
        }
    }
    // The popup is a rectangle: its rows share one span, which a bubble or a
    // badge plate on the same fill does not.
    let mut spans: std::collections::HashMap<(u16, u16), usize> = std::collections::HashMap::new();
    for &(_, x0, x1) in &wide {
        *spans.entry((x0, x1)).or_default() += 1;
    }
    let Some((&(left, right), _)) = spans.iter().max_by_key(|&(span, n)| (*n, *span)) else {
        return String::new();
    };
    let rows = wide
        .iter()
        .filter(|w| (w.1, w.2) == (left, right))
        .map(|w| w.0);
    let (Some(top), Some(bottom)) = (rows.clone().min(), rows.max()) else {
        return String::new();
    };
    (top..=bottom)
        .map(|y| {
            (left..=right)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .filter(|row| !row.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// [`dash_popup`] reads the popup's rect and nothing beside it: not a bubble
/// on the same fill wider than a badge's plate, nor office text, while a popup
/// row a cell of another colour splits is read whole.
#[test]
fn dash_popup_reads_the_popup_rect_alone() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let tb = pixtuoid_scene::theme::NORMAL.ui.tooltip_bg;
    let bg = ratatui::style::Color::Rgb(tb.r, tb.g, tb.b);
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 12));
    let fill = |buf: &mut Buffer, y: u16, x: std::ops::Range<u16>, text: &str| {
        for (x, c) in x.zip(text.chars().chain(std::iter::repeat(' '))) {
            buf[(x, y)].set_char(c).set_bg(bg);
        }
    };
    for y in 4..8 {
        fill(&mut buf, y, 10..40, &format!("popup row {y}"));
    }
    buf[(25, 6)].set_bg(ratatui::style::Color::Reset);
    fill(&mut buf, 1, 30..50, " a wide bubble quip ");
    buf.set_string(0, 10, "office label", ratatui::style::Style::default());
    let popup = dash_popup(&buf);
    assert!(popup.contains("popup row 6"), "{popup}");
    assert!(!popup.contains("bubble"), "{popup}");
    assert!(!popup.contains("office"), "{popup}");
}

#[test]
fn dashboard_renders_a_reasonless_requires_action_as_the_bare_state_words() {
    let mut r = build(120, 44, vec![]);
    let mut w = idle("/h/w.jsonl", 0, t0());
    w.label = "cc\u{b7}wait".into();
    w.state = ActivityState::RequiresAction { reason: None };
    let scene = scene_with(vec![w], 16);

    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    let popup = dash_popup(r.frame_buffer());
    assert!(popup.contains("requires action"), "{popup}");
    assert!(!popup.contains("requires action:"), "{popup}");
}

#[test]
fn dashboard_shows_a_long_requires_action_reason_whole_on_an_unselected_row() {
    let mut r = build(120, 44, vec![]);
    let mut a = running("/h/a.jsonl", 0, "Edit x", t0());
    a.label = "cc\u{b7}act".into();
    let mut w = idle("/h/w.jsonl", 1, t0());
    w.label = "cc\u{b7}wait".into();
    w.state = ActivityState::RequiresAction {
        reason: Some(Arc::from("permission: shell")),
    };
    let scene = scene_with(vec![a, w], 16);

    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    // The glyph and the tint name the state, so the reason takes the whole cell.
    let popup = dash_popup(r.frame_buffer());
    assert!(popup.contains("\u{25d0} permission: shell"), "{popup}");
}

#[test]
fn dashboard_renders_requires_action_reason_and_running_without_detail() {
    let mut r = build(120, 44, vec![]);
    let mut w = idle("/h/w.jsonl", 0, t0());
    w.label = "cc\u{b7}wait".into();
    w.state = ActivityState::RequiresAction {
        reason: Some(Arc::from("permission")),
    };
    let mut a = idle("/h/a.jsonl", 1, t0());
    a.label = "cc\u{b7}act".into();
    a.state = ActivityState::Running {
        item: Some(ToolUse {
            tool_use_id: Some(Arc::from("t")),
            detail: None,
            kind: ToolKind::Other,
        }),
    };
    let scene = scene_with(vec![w, a], 16);

    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    let popup = dash_popup(r.frame_buffer());
    assert!(
        popup.contains("\u{25d0} permission"),
        "requires-action reason missing:\n{popup}"
    );
    assert!(
        popup.contains("\u{25cf} running"),
        "running-without-detail must render the bare state word:\n{popup}"
    );
}

#[test]
fn dashboard_scrolls_to_keep_a_deep_selection_visible() {
    // More roots than the viewport holds, spread across floors so the office
    // only labels a couple while the popup lists all 20.
    let mut agents = Vec::new();
    for i in 0..20 {
        let mut s = idle(&format!("/h/r{i}.jsonl"), i, t0());
        s.label = format!("row{i:02}").into();
        s.floor_idx = i % 10;
        agents.push(s);
    }
    let scene = scene_with(agents, 16);
    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    let row18 = rows[18].agent_id;
    let mut r = build(120, 44, vec![]);
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                selected: Some(row18),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    let buf = r.frame_buffer();
    let text = frame_text(buf);
    let popup = dash_popup(buf);
    assert!(text.contains("Agents (20)"), "all 20 rows counted:\n{text}");
    assert!(
        popup.contains("row18"),
        "deep selection scrolled into view (painter re-clamps scroll to the real window):\n{popup}"
    );
    assert!(
        !popup.contains("row00"),
        "top row must scroll off when a deep row is selected:\n{popup}"
    );
}

#[test]
fn dashboard_empty_scene_shows_placeholder() {
    let mut r = build(120, 44, vec![]);
    let scene = scene_with(vec![], 16);
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    assert!(
        frame_text(r.frame_buffer()).contains("No agents"),
        "empty dashboard must show the placeholder"
    );
}

/// A terminal too short for the popup's own `DASHBOARD_VIEWPORT_ROWS` cap gets
/// clamped by `PanelGeometry::compute` to `bounds.height - FOOTER_ROWS`, so
/// `Panel::paint` windows the list into a viewport SMALLER than that cap — and the
/// selection has to stay inside it. Reachable ONLY on the footer-only draw path: a
/// terminal tall enough to lay out the office is also tall enough for `full_h` to
/// saturate at the content height, so the viewport stays at the cap there.
#[test]
fn a_short_terminal_windows_the_dashboard_below_its_row_cap() {
    let mut agents = Vec::new();
    for i in 0..20 {
        let mut s = idle(&format!("/h/r{i}.jsonl"), i, t0());
        s.label = format!("row{i:02}").into();
        agents.push(s);
    }
    let scene = scene_with(agents, 32);
    let model = build_dashboard_rows(&scene, &DashboardFolds::default());
    let deep = model[18].agent_id;

    let painted = |rows: u16| -> (usize, String) {
        let mut r = build(80, rows, vec![]);
        r.set_frames(
            RenderFrames {
                dashboard: DashboardFrame {
                    open: true,
                    rows: build_dashboard_rows(&scene, &DashboardFolds::default()),
                    selected: Some(deep),
                    ..Default::default()
                },
                ..Default::default()
            },
            t0(),
        );
        r.render(&scene, t0()).unwrap();
        // `dash_popup`, NOT `frame_text`: at a roomy height the OFFICE also renders
        // and paints agent name badges, which a whole-frame scan counts as rows.
        let text = dash_popup(r.frame_buffer());
        let n = (0..20)
            .filter(|i| text.contains(&format!("row{i:02}")))
            .count();
        (n, text)
    };

    let (roomy_n, roomy) = painted(44);
    let (tight_n, tight) = painted(18);
    assert!(
        tight_n < roomy_n,
        "the band must track the CLAMPED inner height, not the {}-row cap: \
         {tight_n} rows at 80×18 vs {roomy_n} at 80×44",
        crate::panels::dashboard::DASHBOARD_VIEWPORT_ROWS,
    );
    for (label, text) in [("80×44", &roomy), ("80×18", &tight)] {
        assert!(
            text.contains("row18"),
            "the selected row must stay in the window at {label}:\n{text}"
        );
        assert!(
            text.contains('\u{22ee}'),
            "20 rows overflow both viewports, so the cue must show at {label}:\n{text}"
        );
    }
}

#[test]
fn dashboard_badge_text_present_for_cc_and_cx() {
    let mut r = build(120, 44, vec![]);
    let mut cc_slot = idle("/h/cc.jsonl", 0, t0());
    cc_slot.source = Arc::from("claude-code");
    cc_slot.label = "cc\u{b7}alpha".into();
    let mut cx_slot = idle("/h/cx.jsonl", 1, t0());
    cx_slot.source = Arc::from("codex");
    cx_slot.label = "cx\u{b7}beta".into();
    let scene = scene_with(vec![cc_slot, cx_slot], 16);

    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    let popup = dash_popup(r.frame_buffer());
    assert!(popup.contains("[cc]"), "cc badge missing:\n{popup}");
    assert!(popup.contains("[cx]"), "cx badge missing:\n{popup}");
}

#[test]
fn dashboard_overflow_cue_appears_below_when_more_than_viewport() {
    // More roots than DASHBOARD_VIEWPORT_ROWS, at scroll=0 → rows hidden below.
    let mut agents = Vec::new();
    for i in 0..20 {
        let mut s = idle(&format!("/h/r{i}.jsonl"), i % 16, t0());
        s.label = format!("overflow{i:02}").into();
        s.floor_idx = i % 10;
        agents.push(s);
    }
    let scene = scene_with(agents, 32);
    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    let mut r = build(120, 44, vec![]);
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    let popup = dash_popup(r.frame_buffer());
    assert!(
        popup.contains('\u{22ee}'),
        "overflow cue ⋮ must appear:\n{popup}"
    );
}

#[test]
fn dashboard_overflow_cue_absent_when_all_visible() {
    // Fewer roots than DASHBOARD_VIEWPORT_ROWS → nothing hidden → no cue.
    let mut agents = Vec::new();
    for i in 0..8 {
        let mut s = idle(&format!("/h/r{i}.jsonl"), i, t0());
        s.label = format!("fit{i:02}").into();
        agents.push(s);
    }
    let scene = scene_with(agents, 16);
    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    let mut r = build(120, 44, vec![]);
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();

    let popup = dash_popup(r.frame_buffer());
    assert!(
        !popup.contains('\u{22ee}'),
        "no overflow cue for 8 rows:\n{popup}"
    );
}

#[test]
fn dashboard_overflow_cue_keeps_a_bottom_navigated_selection_visible() {
    // row20 of 25 parks at the window's bottom with several rows still below.
    let mut agents = Vec::new();
    for i in 0..25 {
        let mut s = idle(&format!("/h/r{i}.jsonl"), i, t0());
        s.label = format!("row{i:02}").into();
        s.floor_idx = i % 10;
        agents.push(s);
    }
    let scene = scene_with(agents, 16);
    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    let row20 = rows[20].agent_id;
    let mut r = build(120, 44, vec![]);
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                selected: Some(row20),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let popup = dash_popup(r.frame_buffer());
    assert!(
        popup.contains("row20"),
        "selected bottom row must stay visible when a cue shows:\n{popup}"
    );
    assert!(
        popup.contains('\u{22ee}'),
        "overflow cue present (rows below):\n{popup}"
    );
}

#[test]
fn dashboard_overflow_no_blank_line_when_selection_is_last_row() {
    // One row past the viewport, with the LAST row selected: scrolled to the very
    // end, nothing is below, so no cue line may be reserved.
    let mut agents = Vec::new();
    for i in 0..17 {
        let mut s = idle(&format!("/h/r{i}.jsonl"), i, t0());
        s.label = format!("row{i:02}").into();
        s.floor_idx = i % 10;
        agents.push(s);
    }
    let scene = scene_with(agents, 16);
    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    let last = rows[16].agent_id;
    let mut r = build(120, 44, vec![]);
    r.set_frames(
        RenderFrames {
            dashboard: DashboardFrame {
                open: true,
                rows,
                selected: Some(last),
                ..Default::default()
            },
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).unwrap();
    let popup = dash_popup(r.frame_buffer());
    assert!(
        popup.contains("row16"),
        "selected last row visible:\n{popup}"
    );
    // The blank-line bug drops the topmost visible row for a reserved cue line,
    // so `row01` present is what distinguishes the two.
    assert!(
        popup.contains("row01"),
        "all 16 visible lines must be filled — no blank reserved cue line:\n{popup}"
    );
    assert!(
        !popup.contains('\u{22ee}'),
        "no cue when scrolled to the end (nothing below):\n{popup}"
    );
}
