//! A name badge's words and colours: its agent's disambiguated, truncated
//! name, its activity tone, its inks and its plate, which
//! [`Badge`](crate::display::Badge) puts together for every painter.

use std::collections::HashMap;

use pixtuoid_core::AgentSlot;
use pixtuoid_core::sprite::Rgb;
use pixtuoid_core::state::ActivityState;

use crate::layout::DESK_W;
use crate::theme::Theme;

/// The separator between a label's source prefix and its cwd/disambiguation
/// tail (`cc·repo`, `cc·repo·1a2b`). The label is WRITTEN core-side as a bare
/// `·` at THREE sites — `decoder::cwd_basename_label` (the chokepoint),
/// `claude_code::cc_derive_label`'s project-dir fallback, and the reducer's
/// SessionStart back-fill, both of which document their bypass — and a crate
/// boundary keeps this const out of reach of all three, so it must MATCH that
/// char.
const LABEL_SEP: char = '\u{b7}';

/// Activity-derived label tone — backend-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BadgeTone {
    Running,
    RequiresAction,
    Idle,
    Exiting,
}

impl BadgeTone {
    /// This tone's theme color role — the SINGLE authority every label painter
    /// shares. The `hovered` near-white highlight is deliberately NOT a
    /// `BadgeTone`; it stays a per-painter surface choice.
    pub(crate) fn rgb(self, theme: &Theme) -> Rgb {
        match self {
            BadgeTone::Exiting => theme.ui.label_exiting,
            BadgeTone::Running => theme.ui.label_running,
            BadgeTone::RequiresAction => theme.ui.label_requires_action,
            BadgeTone::Idle => theme.ui.label_idle,
        }
    }
}

/// The source's badge hue for a name-badge label, or `None` when the label has
/// no `LABEL_SEP` prefix (a bare-prefix / cwd-less label like `cx`) or the
/// prefix is unregistered.
pub(crate) fn badge_hue(text: &str, theme: &Theme) -> Option<Rgb> {
    text.split_once(LABEL_SEP)
        .and_then(|(prefix, _)| theme.source.by_prefix(prefix))
}

/// The colours of a badge's two parts, the one decision every painter reads:
/// the name in the activity tone, which every theme holds at text contrast,
/// and the source's identity on the marker, a graphic, since a brand hue as
/// text can fall below that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadgeInk {
    /// The source's badge hue, else the tone: the terminal's leading `●`,
    /// the pixel painters' strip.
    pub marker: Rgb,
    /// The label text: the tone.
    pub name: Rgb,
}

pub use crate::display::BADGE_MARKER;

// A decoded label ends in core's ELLIPSIS only past this many chars, and a
// badge keeps no more chars than cells (`truncate_label`), so it never shows
// the mark.
const _: () =
    assert!((BADGE_CELLS as usize) < pixtuoid_core::source::decoder::MAX_DECODED_FIELD_CHARS);

/// A badge's width in terminal cells, the terminal's marker included: the
/// classic painter clips its badge to it, and a label is truncated to what is
/// left.
pub const BADGE_CELLS: u16 = DESK_W + BADGE_OVERHANG;
/// The cells a badge may run past its desk's width.
const BADGE_OVERHANG: u16 = 4;

/// The [`BadgeInk`] of a badge reading `text` in `tone`.
pub fn badge_ink(text: &str, tone: BadgeTone, theme: &Theme) -> BadgeInk {
    let name = tone.rgb(theme);
    BadgeInk {
        marker: badge_hue(text, theme).unwrap_or(name),
        name,
    }
}

/// How many agents wear each label: an agent sharing its label carries its
/// session's suffix, even where the namesake is not drawn.
pub(crate) struct Namesakes<'a>(HashMap<&'a str, usize>);

impl<'a> Namesakes<'a> {
    pub(crate) fn of(agents: impl IntoIterator<Item = &'a AgentSlot>) -> Self {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for agent in agents {
            *counts.entry(&*agent.label).or_insert(0) += 1;
        }
        Self(counts)
    }

    /// `agent`'s badge text, disambiguated and truncated, with no marker.
    pub(crate) fn text(&self, agent: &AgentSlot) -> String {
        let needs_disambig = self.0.get(&*agent.label).copied().unwrap_or(0) > 1
            && agent.session_id.chars().count() >= 4;
        let raw: std::borrow::Cow<'_, str> = if needs_disambig {
            let id4 = disambig_suffix(&agent.session_id);
            std::borrow::Cow::Owned(format!("{}{LABEL_SEP}{id4}", agent.label))
        } else {
            std::borrow::Cow::Borrowed(&*agent.label)
        };
        let marker = crate::display::text::cells(BADGE_MARKER.encode_utf8(&mut [0; 4]));
        truncate_label(&raw, BADGE_CELLS.saturating_sub(marker)).into_owned()
    }
}

/// `agent`'s badge tone: exiting over whatever it last did.
pub(crate) fn tone_of(agent: &AgentSlot) -> BadgeTone {
    if agent.exiting_at.is_some() {
        return BadgeTone::Exiting;
    }
    match &agent.state {
        ActivityState::Running { .. } => BadgeTone::Running,
        ActivityState::RequiresAction { .. } => BadgeTone::RequiresAction,
        ActivityState::Idle => BadgeTone::Idle,
    }
}

/// The plate a badge drawn in pixels sits on, so its text keeps one contrast
/// whatever the room behind it
/// (`every_badge_ink_reads_on_its_plate_in_every_theme`).
pub(crate) fn badge_plate_colour(theme: &Theme) -> Rgb {
    theme.ui.tooltip_bg
}

/// Fit a label into `budget` terminal cells, and as many chars, without
/// losing the `·xxxx` session-id disambiguation suffix. Truncates from the
/// base (left of the `·`), not the suffix — otherwise the disambig becomes
/// useless ("TikTok-Android·a" tells us nothing the base alone wouldn't).
pub(crate) fn truncate_label(label: &str, budget: u16) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if span(label) <= budget {
        return Cow::Borrowed(label);
    }
    if let Some(sep_byte) = label.rfind(LABEL_SEP) {
        let suffix = &label[sep_byte..];
        let suffix_span = span(suffix);
        if suffix_span < budget {
            let base = take_span(&label[..sep_byte], budget - suffix_span);
            return Cow::Owned(format!("{base}{suffix}"));
        }
    }
    Cow::Borrowed(take_span(label, budget))
}

/// What a label spends of a badge's budget: its cells, or its chars where a
/// run of zero-width marks has more.
fn span(text: &str) -> u16 {
    let chars = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
    crate::display::text::cells(text).max(chars)
}

/// The longest start of `text` whose [`span`] fits `budget`, its clusters
/// whole.
fn take_span(text: &str, budget: u16) -> &str {
    use unicode_segmentation::UnicodeSegmentation;
    let end = text
        .grapheme_indices(true)
        .map(|(i, cluster)| i + cluster.len())
        .take_while(|&end| span(&text[..end]) <= budget)
        .last()
        .unwrap_or(0);
    &text[..end]
}

/// 4-hex-char disambiguation suffix, hashed from the WHOLE `session_id` —
/// shape-agnostic where any slice of the id is not (a raw-cwd id collides on
/// both head and tail: `/x/app` vs `/y/app`), and multi-byte-safe by
/// construction. `DefaultHasher` is per-process: a display aid, not an id.
pub fn disambig_suffix(session_id: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    session_id.hash(&mut h);
    format!("{:04x}", h.finish() & 0xffff)
}

#[cfg(test)]
mod tests {
    use super::{BadgeTone, Namesakes, badge_hue, disambig_suffix, tone_of, truncate_label};
    use pixtuoid_core::AgentId;
    use pixtuoid_core::state::{ActivityState, AgentSlot, GlobalDeskIndex, SceneState, ToolKind};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn slot(label: &str, session_id: &str, desk: usize, state: ActivityState) -> AgentSlot {
        AgentSlot {
            agent_id: AgentId::from_transcript_path(&format!("/p/{label}-{session_id}.jsonl")),
            source: Arc::from("claude-code"),
            session_id: Arc::from(session_id),
            cwd: Arc::from(PathBuf::from("/p").as_path()),
            label: label.into(),
            state,
            state_started_at: SystemTime::UNIX_EPOCH,
            last_event_at: SystemTime::UNIX_EPOCH,
            created_at: SystemTime::UNIX_EPOCH,
            exiting_at: None,
            desk_index: GlobalDeskIndex(desk),
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

    fn running() -> ActivityState {
        ActivityState::Running {
            tool_use_id: Some(Arc::from("t")),
            detail: Some(Arc::from("Edit")),
            kind: ToolKind::Edit,
        }
    }

    fn scene_of(slots: Vec<AgentSlot>) -> SceneState {
        let mut s = SceneState::uniform(16);
        for slot in slots {
            s.agents.insert(slot.agent_id, slot);
        }
        s
    }

    #[test]
    fn a_lone_running_agent_reads_its_bare_label_in_the_running_tone() {
        let a = slot("cc", "sess-abcd", 0, running());
        let s = scene_of(vec![a.clone()]);
        assert_eq!(Namesakes::of(s.agents.values()).text(&a), "cc");
        assert_eq!(tone_of(&a), BadgeTone::Running);
    }

    #[test]
    fn badge_hue_resolves_a_registered_prefix() {
        let theme = &crate::theme::NORMAL;
        assert!(badge_hue("cc·repo", theme).is_some());
        assert_eq!(badge_hue("cc·repo", theme), theme.source.by_prefix("cc"));
    }

    #[test]
    fn badge_hue_is_none_for_a_bare_prefix_without_separator() {
        assert_eq!(badge_hue("cx", &crate::theme::NORMAL), None);
    }

    #[test]
    fn badge_hue_is_none_for_an_unregistered_prefix() {
        assert_eq!(badge_hue("zz·repo", &crate::theme::NORMAL), None);
    }

    #[test]
    fn colliding_labels_get_disambig_suffixes() {
        let a = slot("cc", "session-aaaa", 0, running());
        let b = slot("cc", "session-bbbb", 1, running());
        let s = scene_of(vec![a.clone(), b.clone()]);
        let namesakes = Namesakes::of(s.agents.values());
        let want_a = format!("cc\u{00b7}{}", disambig_suffix(&a.session_id));
        let want_b = format!("cc\u{00b7}{}", disambig_suffix(&b.session_id));
        assert_eq!(namesakes.text(&a), want_a);
        assert_eq!(namesakes.text(&b), want_b);
        assert_ne!(want_a, want_b);
    }

    #[test]
    fn tone_maps_state_and_exiting_overrides_running() {
        let requires_action = slot(
            "wa",
            "sess-w",
            0,
            ActivityState::RequiresAction {
                reason: Some(Arc::from("perm")),
            },
        );
        let idle = slot("id", "sess-i", 1, ActivityState::Idle);
        let mut exiting = slot("ex", "sess-e", 2, running());
        exiting.exiting_at = Some(now());

        assert_eq!(tone_of(&requires_action), BadgeTone::RequiresAction);
        assert_eq!(tone_of(&idle), BadgeTone::Idle);
        assert_eq!(tone_of(&exiting), BadgeTone::Exiting);
    }

    /// A budget never splits a grapheme cluster: a ZWJ sequence is kept whole
    /// or dropped whole.
    #[test]
    fn a_take_keeps_clusters_whole() {
        let coder = "\u{1f469}\u{200d}\u{1f4bb}";
        let text = format!("a{coder}b");
        // The sequence is three chars in two cells, so it takes three.
        assert_eq!(super::take_span(&text, 4), format!("a{coder}"));
        assert_eq!(super::take_span(&text, 3), "a");
        assert_eq!(super::take_span(&text, u16::MAX), text);
    }

    /// A first cluster wider than the budget takes nothing, as a terminal
    /// can't draw half of it.
    #[test]
    fn a_cluster_wider_than_the_budget_is_dropped() {
        assert_eq!(super::take_span("\u{65e5}x", 1), "");
    }

    #[test]
    fn a_label_of_zero_width_marks_never_shows_the_cap_mark() {
        use pixtuoid_core::source::decoder::{ELLIPSIS, MAX_DECODED_FIELD_CHARS};
        let capped = format!(
            "a{}{ELLIPSIS}",
            "\u{301}".repeat(MAX_DECODED_FIELD_CHARS - 1)
        );
        let shown = truncate_label(&capped, super::BADGE_CELLS);
        assert!(!shown.contains(ELLIPSIS), "{shown:?}");
    }

    #[test]
    fn truncate_label_passes_short_labels_through() {
        assert_eq!(truncate_label("hello", 16), "hello");
    }

    #[test]
    fn truncate_label_preserves_disambig_suffix() {
        let out = truncate_label("TikTok-Android\u{00b7}a09a", 16);
        assert_eq!(out.chars().count(), 16);
        assert!(out.ends_with("\u{00b7}a09a"), "suffix lost: {out}");
        assert!(out.starts_with("TikTok"), "base over-truncated: {out}");
    }

    #[test]
    fn truncate_label_falls_back_to_plain_truncate_when_no_separator() {
        let out = truncate_label("a-very-long-project-name", 8);
        assert_eq!(out, "a-very-l");
    }

    #[test]
    fn truncate_label_plain_take_when_suffix_exceeds_budget() {
        let out = truncate_label("x\u{00b7}abcdefgh", 4);
        assert_eq!(out.chars().count(), 4);
        assert_eq!(out, "x\u{00b7}ab");
    }

    /// A long name's badge, marker and all, fills [`BADGE_CELLS`] and no
    /// more, short only by the half a wide character would split.
    ///
    /// [`BADGE_CELLS`]: super::BADGE_CELLS
    #[test]
    fn a_long_names_badge_fills_its_cells_marker_included() {
        use crate::display::text::cells;
        for label in [
            "cc\u{b7}a-very-long-project-name",
            "cc\u{b7}日本語プロジェクト管理ツール",
            "cc\u{b7}a日本語プロジェクト管理",
        ] {
            let a = slot(label, "sess-abcd", 0, running());
            let s = scene_of(vec![a.clone()]);
            let name = Namesakes::of(s.agents.values()).text(&a);
            let badge = format!("{}{name}", super::BADGE_MARKER);
            let n = cells(&badge);
            assert!(
                (super::BADGE_CELLS - 1..=super::BADGE_CELLS).contains(&n),
                "{badge:?} takes {n} cells"
            );
        }
    }

    /// The budget is in cells, which a CJK character takes two of: a wide name
    /// gets half the characters, the suffix kept as for any other.
    #[test]
    fn truncate_label_budgets_cells_not_chars() {
        assert_eq!(truncate_label("日本語プロジェクト", 8), "日本語プ");
        assert_eq!(
            truncate_label("日本語プロジェクト\u{00b7}a09a", 12),
            "日本語\u{00b7}a09a"
        );
    }

    #[test]
    fn uuid_ids_get_distinct_suffixes() {
        let a = disambig_suffix("c0f7fb3f-dc9c-47c3-840d-f775dd2855a3");
        let b = disambig_suffix("019ea57d-7fa7-7812-b864-bdcb9b6c7e17");
        assert_ne!(a, b);
        assert_eq!(a.len(), 4);
    }

    #[test]
    fn ag_full_path_ids_get_distinct_suffixes() {
        let a = disambig_suffix("/users/me/.gravity/sessions/proj/alpha-01.jsonl");
        let b = disambig_suffix("/users/me/.gravity/sessions/proj/beta-02.jsonl");
        assert_ne!(a, b);
    }

    #[test]
    fn rx_cwd_ids_with_colliding_basenames_get_distinct_suffixes() {
        let a = disambig_suffix("/work/client-x/app");
        let b = disambig_suffix("/work/client-y/app");
        assert_ne!(a, b);
    }

    /// WCAG 2.2's relative luminance
    /// (<https://www.w3.org/TR/WCAG22/#dfn-relative-luminance>).
    fn luminance(c: pixtuoid_core::sprite::Rgb) -> f64 {
        let linear = |v: u8| {
            let s = f64::from(v) / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
    }

    /// WCAG 2.2's contrast ratio (<https://www.w3.org/TR/WCAG22/#dfn-contrast-ratio>).
    fn contrast(a: pixtuoid_core::sprite::Rgb, b: pixtuoid_core::sprite::Rgb) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// A badge's name is small text, never large-scale, so it holds WCAG 2.2
    /// AA's 4.5:1 (<https://www.w3.org/TR/WCAG22/#contrast-minimum>) on its
    /// plate; its marker is a graphic, held to 3:1
    /// (<https://www.w3.org/TR/WCAG22/#non-text-contrast>). Over every source's
    /// label and a bare one, in every tone. The guarantee holds on the plate,
    /// which the cutaway paints: the painter #873 measured an idle badge in at
    /// 2.05:1, straight on the floor.
    #[test]
    fn every_badge_ink_reads_on_its_plate_in_every_theme() {
        use super::{badge_ink, badge_plate_colour};
        let labels: Vec<String> = pixtuoid_core::source::registry::REGISTRY
            .iter()
            .map(|s| format!("{}\u{b7}repo", s.label_prefix))
            .chain(["bare".to_owned()])
            .collect();
        for theme in crate::theme::ALL_THEMES {
            let plate = badge_plate_colour(theme);
            for tone in [
                BadgeTone::Running,
                BadgeTone::RequiresAction,
                BadgeTone::Idle,
                BadgeTone::Exiting,
            ] {
                for label in &labels {
                    let ink = badge_ink(label, tone, theme);
                    let name = contrast(ink.name, plate);
                    let marker = contrast(ink.marker, plate);
                    let at = format!("{} {tone:?} {label}", theme.name);
                    assert!(name >= 4.5, "{at}: name {name:.2}:1");
                    assert!(marker >= 3.0, "{at}: marker {marker:.2}:1");
                }
            }
        }
    }

    #[test]
    fn multibyte_ids_are_safe_and_deterministic() {
        let a = disambig_suffix("/naïveté/app");
        assert_eq!(a, disambig_suffix("/naïveté/app"));
        assert_eq!(a.len(), 4);
    }
}
