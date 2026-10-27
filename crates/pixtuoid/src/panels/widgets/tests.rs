use super::*;
use footer::{build_status_spans, build_status_summary, footer_context};
use pixtuoid_core::state::ActivityState;
use pixtuoid_core::{AgentId, AgentSlot, GlobalDeskIndex, SceneState};
use pixtuoid_scene::display::text::cells;
use pixtuoid_scene::footer::{FooterFloor, FooterInputs};
use pixtuoid_scene::tally::{StateCounts, gateway_rollup, per_floor_counts, scene_stats};
use std::path::PathBuf;
use std::sync::Arc;

fn stat_slot(path: &str, state: ActivityState, exiting: bool) -> AgentSlot {
    let now = SystemTime::UNIX_EPOCH;
    AgentSlot {
        agent_id: AgentId::from_transcript_path(path),
        source: Arc::from("claude-code"),
        session_id: Arc::from("s"),
        cwd: Arc::from(PathBuf::from("/p").as_path()),
        label: "x".into(),
        state,
        state_started_at: now,
        created_at: now,
        last_event_at: now,
        exiting_at: exiting.then_some(now),
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
    }
}

#[test]
fn scene_stats_buckets_exiting_first_and_totals() {
    use pixtuoid_core::state::ToolKind;
    let running = || ActivityState::Running {
        tool_use_id: None,
        detail: None,
        kind: ToolKind::Other,
    };
    let mut scene = SceneState::uniform(16);
    for s in [
        stat_slot("/exiting-active.jsonl", running(), true),
        stat_slot("/live-active.jsonl", running(), false),
        stat_slot(
            "/waiting.jsonl",
            ActivityState::RequiresAction {
                reason: Some(Arc::from("perm")),
            },
            false,
        ),
        stat_slot("/idle.jsonl", ActivityState::Idle, false),
    ] {
        scene.agents.insert(s.agent_id, s);
    }
    let c = scene_stats(&scene);
    assert_eq!(
        c.exiting, 1,
        "an exiting agent buckets as exiting even mid-Running"
    );
    assert_eq!(c.running, 1, "only the LIVE Running counts as running");
    assert_eq!(c.requires_action, 1);
    assert_eq!(c.idle, 1);
    assert_eq!(c.total, 4);
    assert_eq!(
        c.running + c.requires_action + c.idle + c.exiting,
        c.total,
        "the four buckets must partition the total"
    );
}

#[test]
fn scene_stats_empty_scene_is_all_zero() {
    let c = scene_stats(&SceneState::uniform(16));
    assert_eq!(c, StateCounts::default());
    assert_eq!(c.total, 0);
}

#[test]
fn state_vocab_is_total_and_distinct() {
    use std::collections::HashSet;
    let kinds = StateKind::ALL;
    assert_eq!(kinds.len(), 4, "the vocab covers exactly the four buckets");
    let glyphs: HashSet<char> = kinds.iter().map(|k| k.glyph()).collect();
    let letters: HashSet<char> = kinds.iter().map(|k| k.letter()).collect();
    let words: HashSet<&str> = kinds.iter().map(|k| k.word()).collect();
    assert_eq!(glyphs.len(), 4, "each state has a distinct glyph");
    assert_eq!(letters.len(), 4, "each state has a distinct letter");
    assert_eq!(words.len(), 4, "each state has a distinct word");
    let t = &pixtuoid_scene::theme::NORMAL;
    let tone = |k| pixtuoid_scene::tooltip::TipTone::Rung(k).rgb(t);
    assert_eq!(
        tone(StateKind::RequiresAction),
        Some(t.ui.label_requires_action)
    );
    assert_eq!(tone(StateKind::Exiting), Some(t.ui.label_exiting));
}

#[test]
fn cells_count_terminal_columns_not_chars() {
    // The state/HUD glyphs are all East-Asian *ambiguous* = 1 column under the
    // non-CJK `.width()`, so this measure == chars().count() for them.
    assert_eq!(cells("\u{b7}\u{d7}\u{2191}\u{2193}"), 4); // · × ↑ ↓
    assert_eq!(
        cells("\u{25cf}\u{25d0}\u{25cb}\u{25cc}"),
        4,
        "● ◐ ○ ◌ are one column each"
    );
    assert_eq!(cells("[q]uit"), 6);
    assert_eq!(cells("\u{1f99e}"), 2); // 🦞
    assert_eq!(cells("a\u{0301}"), 1);
}

/// The one width rule is the cells ratatui's buffer writes, one grapheme
/// cluster at a time, so a VS16 emoji or a ZWJ sequence takes the same cells
/// in the cutaway's image, in a truncated badge and on the terminal.
#[test]
fn every_width_is_the_cells_the_buffer_writes() {
    for text in [
        "[q]uit",
        "\u{6f22}\u{5b57}",
        "a\u{301}",
        "\u{2764}\u{fe0f}",
        "\u{1f469}\u{200d}\u{1f4bb}",
        "\u{644}\u{627}",
        "cc\u{b7}\u{1f99e}",
        "a\u{7}b",
        "\u{ff76}\u{ff9e}",
        "\u{ff8a}\u{ff9f}",
        "\u{ff9e}",
    ] {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 1));
        let (x, _) = buf.set_stringn(0, 0, text, usize::MAX, ratatui::style::Style::default());
        assert_eq!(cells(text), x, "{text:?}");
    }
}

#[test]
fn footer_vocabulary_is_single_column_so_scene_chars_count_matches_cells() {
    let vocab = "\u{b7}\u{d7}\u{2191}\u{2193}\u{25cf}\u{25d0}\u{25cb}\u{25cc}\u{2b22}\u{25b2}\u{2669}\u{26a0}\u{2026}";
    for c in vocab.chars() {
        let s = c.to_string();
        assert_eq!(
            usize::from(cells(&s)),
            s.chars().count(),
            "footer glyph U+{:04X} {c:?} must be single-column, else scene's chars().count() drifts from cells",
            c as u32,
        );
    }
}

#[test]
fn state_count_maps_each_kind() {
    let c = StateCounts {
        running: 3,
        requires_action: 2,
        idle: 7,
        exiting: 1,
        total: 13,
    };
    assert_eq!(StateKind::Running.count(c), 3);
    assert_eq!(StateKind::RequiresAction.count(c), 2);
    assert_eq!(StateKind::Idle.count(c), 7);
    assert_eq!(StateKind::Exiting.count(c), 1);
}

// Running needs a placeholder in-flight turn key because Running is DERIVED from the turn
// set, never stored — without one the fixture is silently Idle.
fn daemon(state: pixtuoid_core::state::DaemonState) -> pixtuoid_core::state::DaemonPresence {
    use pixtuoid_core::state::{DaemonLiveness, DaemonState};
    let (liveness, in_flight_turns) = match state {
        DaemonState::Idle => (DaemonLiveness::UP, Default::default()),
        DaemonState::Running => (
            DaemonLiveness::UP,
            [(
                pixtuoid_core::state::TurnKey::Id("fixture-run".into()),
                SystemTime::UNIX_EPOCH,
            )]
            .into_iter()
            .collect::<std::collections::HashMap<pixtuoid_core::state::TurnKey, SystemTime>>(),
        ),
        DaemonState::Degraded => (DaemonLiveness::Up { degraded: true }, Default::default()),
        DaemonState::Down => (DaemonLiveness::Down, Default::default()),
    };
    pixtuoid_core::state::DaemonPresence {
        liveness,
        active_sessions: 0,
        last_seen: SystemTime::UNIX_EPOCH,
        entered_at: SystemTime::UNIX_EPOCH,
        in_flight_turns,
        current_pid: None,
    }
}

#[test]
fn gateway_rollup_is_worst_of() {
    use pixtuoid_core::state::DaemonState;
    assert_eq!(gateway_rollup(std::iter::empty()), None);
    let running = daemon(DaemonState::Running);
    assert_eq!(
        gateway_rollup(std::iter::once(&running)),
        Some(DaemonState::Running)
    );
    let (idle, degraded, down) = (
        daemon(DaemonState::Idle),
        daemon(DaemonState::Degraded),
        daemon(DaemonState::Down),
    );
    assert_eq!(
        gateway_rollup([&running, &idle, &degraded, &down].into_iter()),
        Some(DaemonState::Down)
    );
    assert_eq!(
        gateway_rollup([&idle, &degraded].into_iter()),
        Some(DaemonState::Degraded)
    );
}

#[test]
fn per_floor_buckets_by_floor_idx() {
    use pixtuoid_core::state::ToolKind;
    let running = || ActivityState::Running {
        tool_use_id: None,
        detail: None,
        kind: ToolKind::Other,
    };
    let mut scene = SceneState::uniform(16);
    let add = |scene: &mut SceneState, path, state, exiting, floor: usize| {
        let mut s = stat_slot(path, state, exiting);
        s.floor_idx = floor;
        scene.agents.insert(s.agent_id, s);
    };
    add(&mut scene, "/f0a.jsonl", running(), false, 0);
    add(&mut scene, "/f0b.jsonl", running(), false, 0);
    add(
        &mut scene,
        "/f1w.jsonl",
        ActivityState::RequiresAction {
            reason: Some(Arc::from("p")),
        },
        false,
        1,
    );
    add(&mut scene, "/f1x.jsonl", running(), true, 1); // exiting on floor 1
    add(&mut scene, "/f2i.jsonl", ActivityState::Idle, false, 2);

    let pf = per_floor_counts(&scene);
    assert_eq!((pf[0].running, pf[0].total), (2, 2));
    assert_eq!(
        (pf[1].requires_action, pf[1].exiting, pf[1].total),
        (1, 1, 2)
    );
    assert_eq!((pf[2].idle, pf[2].total), (1, 1));
    assert_eq!(pf[3], StateCounts::default(), "an untouched floor is zero");
}

// A 10-char string scrolled in a 5-col window: max_off=5, scroll_ms=750,
// pause=1200, cycle = 2*1200 + 2*750 = 3900. Phases (ms):
//   [0,1200)        hold head  -> "ABCDE"
//   [1200,1950)     scroll out -> off=(p-1200)/150
//   [1950,3150)     hold tail  -> "FGHIJ"
//   [3150,3900)     scroll back-> off=5-((p-3150)/150)
const M: &str = "ABCDEFGHIJ";
fn at(ms: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(ms)
}

#[test]
fn marquee_fits_returns_unchanged_no_ellipsis() {
    assert_eq!(marquee_window("short", 10, at(99_999)), "short");
    assert_eq!(marquee_window("EXACTLYTEN", 10, at(99_999)), "EXACTLYTEN");
}

#[test]
fn marquee_zero_width_is_empty() {
    assert_eq!(marquee_window(M, 0, at(0)), "");
    assert_eq!(marquee_window("", 0, at(0)), "");
}

#[test]
fn marquee_holds_head_then_tail() {
    assert_eq!(marquee_window(M, 5, at(0)), "ABCDE");
    assert_eq!(marquee_window(M, 5, at(2000)), "FGHIJ");
}

#[test]
fn marquee_scrolls_out_and_back() {
    assert_eq!(marquee_window(M, 5, at(1500)), "CDEFG");
    assert_eq!(marquee_window(M, 5, at(3450)), "DEFGH");
}

#[test]
fn marquee_is_deterministic_and_cycles() {
    assert_eq!(
        marquee_window(M, 5, at(1500)),
        marquee_window(M, 5, at(1500))
    );
    assert_eq!(
        marquee_window(M, 5, at(1500)),
        marquee_window(M, 5, at(1500 + 3900))
    );
}

#[test]
fn marquee_min_overflow_reaches_both_ends() {
    // len == width + 1 (max_off=1), so scroll_ms=150 and the cycle is 2700.
    let s = "ABCDEF"; // len 6, width 5
    assert_eq!(marquee_window(s, 5, at(0)), "ABCDE"); // head
    assert_eq!(marquee_window(s, 5, at(1500)), "BCDEF"); // tail-hold [1350,2550)
}

#[test]
fn marquee_never_panics_on_multibyte() {
    let s = "café·ünïcödé·scroll·test";
    for ms in [0u64, 500, 1500, 2500, 5000, 9999] {
        let out = marquee_window(s, 8, at(ms));
        assert_eq!(out.chars().count(), 8, "ms={ms}: {out:?}");
    }
}

#[test]
fn marquee_or_truncate_selected_scrolls_unselected_ellipsizes() {
    assert_eq!(marquee_or_truncate(M, 5, true, at(0)), "ABCDE");
    assert_eq!(marquee_or_truncate(M, 5, false, at(0)), "ABCD\u{2026}");
}

fn slot_with(state: ActivityState, label: &str) -> AgentSlot {
    AgentSlot {
        agent_id: AgentId::from_transcript_path(&format!("/p/{label}.jsonl")),
        source: Arc::from("claude-code"),
        session_id: Arc::from("s"),
        cwd: Arc::from(PathBuf::from("/p").as_path()),
        label: label.into(),
        state,
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
    }
}
fn running_with(detail: &str, label: &str) -> AgentSlot {
    slot_with(
        ActivityState::Running {
            tool_use_id: Some(Arc::from("t")),
            detail: Some(Arc::from(detail)),
            kind: pixtuoid_core::state::ToolKind::from_display(detail),
        },
        label,
    )
}
fn requires_action(label: &str) -> AgentSlot {
    slot_with(
        ActivityState::RequiresAction {
            reason: Some(Arc::from("perm")),
        },
        label,
    )
}
fn idle(label: &str) -> AgentSlot {
    slot_with(ActivityState::Idle, label)
}
fn running_kind(detail: &str, kind: pixtuoid_core::state::ToolKind, label: &str) -> AgentSlot {
    slot_with(
        ActivityState::Running {
            tool_use_id: Some(Arc::from("t")),
            detail: Some(Arc::from(detail)),
            kind,
        },
        label,
    )
}
fn scene_of(slots: Vec<AgentSlot>) -> SceneState {
    let mut s = SceneState::uniform(16);
    for slot in slots {
        s.agents.insert(slot.agent_id, slot);
    }
    s
}

fn inputs<'a>(
    scene: &SceneState,
    floor: Option<FooterFloor>,
    warning: Option<&'a str>,
) -> FooterInputs<'a> {
    FooterInputs::new(scene, footer_context(scene, floor, false, None, warning))
}

fn footer_line(
    scene: &SceneState,
    width: u16,
    floor: Option<FooterFloor>,
    warning: Option<&str>,
) -> String {
    build_status_summary(&inputs(scene, floor, warning), width)
}

const QUIT_SUFFIX: &str = " [?]help [p]ause [t]heme [q]uit ";

#[test]
fn footer_source_warning_replaces_stats_and_keeps_quit() {
    let s = scene_of(vec![idle("myproject")]);
    let line = footer_line(
        &s,
        100,
        None,
        Some("claude-code source died — its agents are frozen; restart pixtuoid (see log)"),
    );
    assert!(line.contains('⚠'), "warning marker present: {line}");
    assert!(line.contains("claude-code source died"), "got: {line}");
    assert!(line.ends_with(" [q]uit "), "quit hint survives: {line}");
    assert!(
        !line.contains('\u{25cb}') && !line.contains('\u{25cf}'),
        "stale state rungs are replaced by the warning: {line}"
    );
    insta::assert_snapshot!(line);
}

#[test]
fn footer_source_warning_survives_every_width() {
    let s = scene_of(vec![idle("myproject")]);
    for w in [20u16, 30, 40, 60, 80] {
        let line = footer_line(
            &s,
            w,
            None,
            Some("claude-code source died — its agents are frozen; restart pixtuoid (see log)"),
        );
        assert!(
            line.contains('⚠') || line.contains('…'),
            "warning must never be tiered away (w={w}): {line}"
        );
        assert!(
            line.chars().count() <= w as usize,
            "must fit the row (w={w}): {line:?}"
        );
    }
}

#[test]
fn footer_zero_agents() {
    let s = scene_of(vec![]);
    let line = footer_line(&s, 80, None, None);
    assert_eq!(line.len(), 80, "should pad to full width");
    insta::assert_snapshot!(line);
}

#[test]
fn footer_single_idle_agent() {
    let s = scene_of(vec![idle("myproject")]);
    let line = footer_line(&s, 80, None, None);
    assert!(line.contains(" 1 \u{b7} \u{25cb}1 I"), "got: {line}");
    insta::assert_snapshot!(line);
}

#[test]
fn footer_full_width_mixed_states() {
    let s = scene_of(vec![
        running_with("Edit src/a.rs", "a"),
        running_with("Edit src/b.rs", "b"),
        running_with("Bash: ls", "c"),
        requires_action("d"),
        requires_action("e"),
        idle("f"),
        idle("g"),
        idle("h"),
    ]);
    let line = footer_line(&s, 120, None, None);
    for frag in [
        "\u{25cf}3 R",
        "\u{25d0}2 A",
        "\u{25cb}3 I",
        "Edit\u{d7}2",
        "Bash\u{d7}1",
    ] {
        assert!(line.contains(frag), "missing {frag:?} in: {line}");
    }
    insta::assert_snapshot!(line);
}

#[test]
fn footer_medium_width_compact() {
    let s = scene_of(vec![
        running_with("Edit src/a.rs", "a"),
        requires_action("b"),
        idle("c"),
    ]);
    let line = footer_line(&s, 60, None, None);
    assert!(!line.contains("Edit"), "medium drops tools: {line}");
    assert!(line.contains("\u{25cf}1R"), "compact running rung: {line}");
    insta::assert_snapshot!(line);
}

#[test]
fn footer_minimal_width() {
    let s = scene_of(vec![idle("a"), idle("b")]);
    let w = QUIT_SUFFIX.len() + 6;
    let line = footer_line(&s, w as u16, None, None);
    assert_eq!(line.len(), w);
    insta::assert_snapshot!(line);
}

#[test]
fn footer_quit_only_below_threshold() {
    let s = scene_of(vec![idle("a")]);
    let w = QUIT_SUFFIX.len();
    let line = footer_line(&s, w as u16, None, None);
    insta::assert_snapshot!(line);
}

#[test]
fn footer_caps_tools_at_four() {
    let s = scene_of(vec![
        running_with("Edit x", "a"),
        running_with("Bash x", "b"),
        running_with("Read x", "c"),
        running_with("Write x", "d"),
        running_with("Grep x", "e"),
        running_with("Glob x", "f"),
    ]);
    let line = footer_line(&s, 200, None, None);
    let crosses = line.matches('\u{00d7}').count();
    assert_eq!(crosses, 4, "expected <=4 tools in breakdown");
    insta::assert_snapshot!(line);
}

#[test]
fn footer_minimal_leads_with_requires_action_alarm() {
    let s = scene_of(vec![
        requires_action("a"),
        requires_action("b"),
        idle("c"),
        idle("d"),
    ]);
    let w = QUIT_SUFFIX.len() + 10;
    let line = footer_line(&s, w as u16, None, None);
    assert!(
        line.contains("\u{25b2}2 \u{b7} 4"),
        "▲2 · 4 (alarm leads): {line}"
    );
}

#[test]
fn footer_death_keeps_the_requires_action_alarm() {
    let s = scene_of(vec![requires_action("a"), requires_action("b"), idle("c")]);
    let line = footer_line(&s, 120, None, Some("codex disconnected"));
    assert!(line.contains('\u{26a0}'), "warning present: {line}");
    assert!(
        line.contains("\u{25b2}2 need you"),
        "alarm survives death: {line}"
    );
}

fn fi(current: usize, total_floors: usize, total_agents: usize) -> FooterFloor {
    FooterFloor {
        current,
        total_floors,
        total_agents,
    }
}

#[test]
fn footer_with_floor_info() {
    let s = scene_of(vec![idle("a"), idle("b")]);
    let line = footer_line(&s, 120, Some(fi(2, 3, 5)), None);
    assert!(line.contains(" F2/3 "), "floor breadcrumb: {line}");
    insta::assert_snapshot!(line);
}

// Direct assertions for count_str: snapshots alone mask regressions, being easy to
// ratify away in `cargo insta review`.

#[test]
fn count_str_single_floor_shows_bare_n() {
    let s = scene_of(vec![idle("a"), idle("b")]);
    let line = footer_line(&s, 120, None, None);
    assert!(line.contains(" 2 \u{b7} \u{25cb}2 I"), "got: {line}");
    assert!(!line.contains("2/"), "no slash on a single floor: {line}");
}

#[test]
fn count_str_multi_floor_shows_n_slash_total() {
    let s = scene_of(vec![idle("a"), idle("b")]);
    let line = footer_line(&s, 120, Some(fi(2, 3, 5)), None);
    assert!(line.contains(" 2/5 \u{b7}"), "got: {line}");
}

#[test]
fn count_str_multi_floor_shows_slash_even_when_total_equals_n() {
    let s = scene_of(vec![idle("a"), idle("b")]);
    let line = footer_line(&s, 120, Some(fi(1, 3, 2)), None);
    assert!(line.contains(" 2/2 \u{b7}"), "got: {line}");
}

#[test]
fn count_str_empty_floor_still_shows_total() {
    let s = scene_of(vec![]);
    let line = footer_line(&s, 120, Some(fi(2, 3, 5)), None);
    assert!(line.contains(" 0/5 "), "got: {line}");
}

#[test]
fn count_str_multi_floor_keeps_slash_at_narrow_tier() {
    let s = scene_of(vec![idle("a"), idle("b"), idle("c")]);
    let line = footer_line(&s, 50, Some(fi(1, 3, 10)), None);
    assert!(
        line.contains("3/10"),
        "slash kept at the narrow tier: {line}"
    );
}

fn footer_spans_text(
    scene: &SceneState,
    width: u16,
    floor: Option<FooterFloor>,
    theme: &pixtuoid_scene::theme::Theme,
) -> String {
    build_status_spans(&inputs(scene, floor, None), width, theme)
        .iter()
        .map(|sp| sp.content.as_ref().to_string())
        .collect()
}

#[test]
fn status_spans_text_matches_summary_across_tiers() {
    let theme = &pixtuoid_scene::theme::NORMAL;
    let s = scene_of(vec![
        running_with("Edit src/a.rs", "a"),
        requires_action("b"),
        idle("c"),
        idle("d"),
    ]);
    for (w, fl) in [
        (120u16, None),
        (60, None),
        (28, None),
        (10, None),
        (120, Some(fi(2, 3, 9))),
    ] {
        let summary = footer_line(&s, w, fl, None);
        let spans_text = footer_spans_text(&s, w, fl, theme);
        assert_eq!(spans_text, summary, "tier width {w} drifted");
    }
}

#[test]
fn status_spans_color_code_state_segments() {
    let theme = &pixtuoid_scene::theme::NORMAL;
    let s = scene_of(vec![
        running_with("Edit src/a.rs", "a"),
        requires_action("b"),
        idle("c"),
    ]);
    let spans = build_status_spans(&inputs(&s, None, None), 120, theme);
    let running = spans
        .iter()
        .find(|sp| sp.content.contains('\u{25cf}'))
        .unwrap();
    let requires_action = spans
        .iter()
        .find(|sp| sp.content.contains('\u{25d0}'))
        .unwrap();
    assert_eq!(running.style.fg, Some(to_color(theme.ui.label_running)));
    assert_eq!(
        requires_action.style.fg,
        Some(to_color(theme.ui.label_requires_action))
    );
}

#[test]
fn footer_counts_agree_with_board_on_walkout() {
    let mut gone = running_with("Edit x", "gone");
    gone.exiting_at = Some(SystemTime::UNIX_EPOCH);
    let s = scene_of(vec![
        running_with("Edit a", "a"),
        running_with("Bash b", "b"),
        gone,
    ]);
    let c = scene_stats(&s);
    assert_eq!((c.running, c.exiting, c.total), (2, 1, 3), "shared spine");
    let line = footer_line(&s, 160, None, None);
    assert!(
        line.contains(" 3 \u{b7} \u{25cf}2 R"),
        "total incl. exiting: {line}"
    );
    assert!(
        line.contains("\u{25cc}1 x"),
        "first-class exiting rung: {line}"
    );
}

#[test]
fn footer_tool_hue_reads_kind_field() {
    // A Task dispatch DISPLAYS "Delegating" but its typed kind is Task, so the hue
    // must never come from a re-parse of the displayed string.
    let theme = &pixtuoid_scene::theme::NORMAL;
    let s = scene_of(vec![running_kind(
        "Delegating",
        pixtuoid_core::state::ToolKind::Task,
        "lead",
    )]);
    let spans = build_status_spans(&inputs(&s, None, None), 160, theme);
    let tool = spans
        .iter()
        .find(|sp| sp.content.contains("Delegating"))
        .expect("tool segment present");
    let expected = to_color(
        theme
            .tool_glow
            .for_kind(pixtuoid_core::state::ToolKind::Task),
    );
    assert_eq!(tool.style.fg, Some(expected), "hue from the typed kind");
    assert_eq!(
        expected,
        to_color(theme.tool_glow.agent),
        "== the agent glow"
    );
}

#[test]
fn footer_gateway_chip_reflects_rollup_and_suppresses_when_absent() {
    use pixtuoid_core::state::DaemonState;
    let s = scene_of(vec![idle("a")]);
    let mut with_gw = inputs(&s, None, None);
    with_gw.context.gateway = Some(DaemonState::Degraded);
    let line = build_status_summary(&with_gw, 160);
    assert!(line.contains("\u{2b22}gw err"), "degraded chip: {line}");
    let no_gw = inputs(&s, None, None);
    let line2 = build_status_summary(&no_gw, 160);
    assert!(
        !line2.contains("gw"),
        "chip suppressed when no daemon: {line2}"
    );
}

#[test]
fn footer_cross_floor_alarm_points_at_requires_action_floor() {
    // The requires-action agent sits on floor 2 (index 1) while floor 1 is the one shown —
    // `per_floor` is office-wide, not the projected floor.
    let s = scene_of(vec![idle("a")]);
    let mut footer = inputs(&s, Some(fi(1, 3, 2)), None);
    footer.context.per_floor[1].requires_action = 1;
    footer.context.per_floor[1].total = 1;
    let line = build_status_summary(&footer, 160);
    assert!(
        line.contains("\u{25b2}F2"),
        "cross-floor requires-action cue: {line}"
    );
}

/// `grid_of` reads back what `put_grid` wrote: symbols (a wide one's covered
/// cell empty), colours, an unset fill as the terminal's, and bold.
#[test]
fn grid_of_reads_back_what_put_grid_wrote() {
    use pixtuoid_scene::display::cells::CellGrid;
    let ink = Rgb { r: 1, g: 2, b: 3 };
    let mut grid = CellGrid::new(5, 2);
    grid.put((0, 0), "a\u{65e5}b", Some(ink), true);
    grid.put((1, 1), "x", None, false);
    let area = ratatui::layout::Rect::new(0, 0, 5, 2);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    put_grid(&mut buf, &grid, (0, 0), area);
    assert_eq!(grid_of(&buf, area), grid);
}

/// A colour the cell names reads back as a colour, a named one at xterm's
/// default, and one the terminal picks (`Reset`, `Indexed`) as the painter's.
#[test]
fn grid_of_reads_a_named_colour_and_leaves_the_terminals_to_the_painter() {
    use ratatui::style::Color;
    let area = ratatui::layout::Rect::new(0, 0, 3, 1);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    for (x, fg, bg) in [
        (0, Color::White, Color::Rgb(1, 2, 3)),
        (1, Color::Red, Color::Indexed(4)),
        (2, Color::Reset, Color::DarkGray),
    ] {
        let cell = &mut buf[(x, 0)];
        cell.set_symbol("x");
        cell.fg = fg;
        cell.bg = bg;
    }
    let grid = grid_of(&buf, area);
    let rgb = |r, g, b| Some(Rgb { r, g, b });
    let colours = |x| {
        let c = grid.get(x, 0).expect("a cell");
        (c.fg, c.bg)
    };
    assert_eq!(colours(0), (rgb(255, 255, 255), rgb(1, 2, 3)));
    assert_eq!(colours(1), (rgb(205, 0, 0), None));
    assert_eq!(colours(2), (None, rgb(127, 127, 127)));
}
