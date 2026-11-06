//! The inks of an agent's name badge, [`badge_ink`]'s [`BadgeInk`]: every
//! painter draws a badge in them, and a device beside the office marks the
//! agent as its badge does.

use crate::sprite::Rgb;
use pixtuoid_core::AgentSlot;
use pixtuoid_core::source::registry::descriptor_for;
use pixtuoid_core::state::ActivityState;

use crate::layout::DESK_W;
use crate::nameplate::Nameplate;
use crate::theme::Theme;

/// Activity-derived label tone — backend-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BadgeTone {
    Running,
    RequiresAction,
    Idle,
    Exiting,
}

impl BadgeTone {
    /// This tone's theme color role — the SINGLE authority every label painter
    /// shares. The `hovered` near-white highlight is deliberately NOT a
    /// `BadgeTone`; it stays a per-painter surface choice.
    fn rgb(self, theme: &Theme) -> Rgb {
        match self {
            BadgeTone::Exiting => theme.ui.label_exiting,
            BadgeTone::Running => theme.ui.label_running,
            BadgeTone::RequiresAction => theme.ui.label_requires_action,
            BadgeTone::Idle => theme.ui.label_idle,
        }
    }
}

/// The colours of a badge's two parts, the one decision every painter reads:
/// the name in the activity tone, which every theme holds at text contrast,
/// and the source's identity on the marker, a graphic, since a brand hue as
/// text can fall below that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BadgeInk {
    /// The source's badge hue, else the tone: the terminal's leading `●`,
    /// the pixel painters' ring.
    pub marker: Rgb,
    /// The label text: the tone.
    pub name: Rgb,
}

#[doc(hidden)]
pub use crate::display::BADGE_MARKER;

// A decoded label ends in core's ELLIPSIS only past this many chars, and a
// badge keeps no more chars than cells (`Nameplate::fit`), so it never shows
// the mark.
const _: () =
    assert!((BADGE_CELLS as usize) < pixtuoid_core::source::decoder::MAX_DECODED_FIELD_CHARS);

/// A badge's width in terminal cells, the terminal's marker included: the
/// classic painter clips its badge to it, and a label is truncated to what is
/// left.
#[doc(hidden)]
pub const BADGE_CELLS: u16 = DESK_W + BADGE_OVERHANG;
/// The cells a badge may run past its desk's width.
const BADGE_OVERHANG: u16 = 4;

/// `agent`'s [`BadgeInk`]. Its marker reads the agent's source, never its
/// label, which a cwd-less agent wears as a bare prefix and a rename as its
/// own name.
pub fn badge_ink(agent: &AgentSlot, theme: &Theme) -> BadgeInk {
    let name = tone_of(agent).rgb(theme);
    let hue = descriptor_for(&agent.source).and_then(|d| theme.source.by_prefix(d.label_prefix));
    BadgeInk {
        marker: hue.unwrap_or(name),
        name,
    }
}

/// `name` as a badge shows it, with no marker: [fit](Nameplate::fit) to the
/// cells the marker leaves.
pub(crate) fn badge_text(name: &Nameplate) -> String {
    let marker = crate::display::text::cells(BADGE_MARKER.encode_utf8(&mut [0; 4]));
    name.fit(BADGE_CELLS.saturating_sub(marker)).into_owned()
}

/// `agent`'s badge tone: exiting over whatever it last did.
fn tone_of(agent: &AgentSlot) -> BadgeTone {
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

#[cfg(test)]
mod tests {
    use super::{BadgeTone, badge_ink, badge_text, tone_of};
    use crate::nameplate::{Namesakes, Suffix};
    use pixtuoid_core::AgentId;
    use pixtuoid_core::state::{
        ActivityState, AgentSlot, GlobalDeskIndex, SceneState, ToolKind, ToolUse,
    };
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
            arrived_at: Some(SystemTime::UNIX_EPOCH),
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

    /// `slot` working for `source`.
    fn of(source: &str, label: &str, state: ActivityState) -> AgentSlot {
        AgentSlot {
            source: Arc::from(source),
            ..slot(label, "sess-abcd", 0, state)
        }
    }

    fn running() -> ActivityState {
        ActivityState::Running {
            item: Some(ToolUse {
                tool_use_id: Some(Arc::from("t")),
                detail: Some(Arc::from("Edit")),
                kind: ToolKind::Edit,
            }),
        }
    }

    /// `agent` in each [`BadgeTone`]: running, requiring action, idle, then
    /// exiting.
    fn in_every_tone(agent: &AgentSlot) -> [AgentSlot; 4] {
        let with = |state| AgentSlot {
            state,
            ..agent.clone()
        };
        [
            with(running()),
            with(ActivityState::RequiresAction { reason: None }),
            with(ActivityState::Idle),
            AgentSlot {
                exiting_at: Some(now()),
                ..with(running())
            },
        ]
    }

    fn scene_of(slots: Vec<AgentSlot>) -> SceneState {
        let mut s = SceneState::uniform(16);
        for slot in slots {
            s.agents.insert(slot.agent_id, slot);
        }
        s
    }

    /// `agent`'s badge text among `slots`.
    fn shown(slots: &[AgentSlot], agent: &AgentSlot) -> String {
        badge_text(&Namesakes::of(slots).nameplate(agent))
    }

    #[test]
    fn a_lone_running_agent_reads_its_bare_label_in_the_running_tone() {
        let a = slot("cc", "sess-abcd", 0, running());
        assert_eq!(shown(std::slice::from_ref(&a), &a), "cc");
        assert_eq!(tone_of(&a), BadgeTone::Running);
    }

    /// Core labels a codex agent with no cwd by its bare prefix.
    #[test]
    fn a_cwd_less_agents_marker_takes_its_sources_hue() {
        let theme = &crate::theme::NORMAL;
        let cx = of(pixtuoid_core::source::codex::SOURCE_NAME, "cx", running());
        assert_eq!(badge_ink(&cx, theme).marker, theme.source.codex);
    }

    /// A rename brings its own name, as a CC subagent's `subagent` does.
    #[test]
    fn a_renamed_agents_marker_takes_its_sources_hue() {
        use pixtuoid_core::source::claude_code::SOURCE_NAME;
        let theme = &crate::theme::NORMAL;
        let sub = of(SOURCE_NAME, "subagent", running());
        assert_eq!(badge_ink(&sub, theme).marker, theme.source.claude_code);
    }

    /// Whatever its label reads.
    #[test]
    fn an_unregistered_sources_marker_takes_its_tone() {
        let theme = &crate::theme::NORMAL;
        let ink = badge_ink(&of("unregistered", "cc\u{b7}repo", running()), theme);
        assert_eq!(ink.marker, BadgeTone::Running.rgb(theme));
    }

    /// A namesake's badge, as every painter builds it, is its whole nameplate
    /// truncated, the suffix kept.
    #[test]
    fn a_long_namesakes_badge_is_its_nameplate_truncated() {
        let label = "cc\u{b7}a-very-long-project-name";
        let a = slot(label, "session-aaaa", 0, running());
        let b = slot(label, "session-bbbb", 1, running());
        let s = scene_of(vec![a.clone(), b]);
        let namesakes = Namesakes::of(s.agents.values());
        let anchor = crate::layout::Point { x: 0, y: 0 };
        let badge = crate::display::Badge::new(
            (anchor, crate::layout::BARE_OFFICE),
            &a,
            &namesakes,
            &crate::theme::NORMAL,
        );
        let (shown, whole) = (badge.name.text(), namesakes.nameplate(&a));
        let marker = crate::display::text::cells(super::BADGE_MARKER.encode_utf8(&mut [0; 4]));
        assert!(
            shown.len() < whole.to_string().len(),
            "premise: {whole} is cut"
        );
        assert_eq!(shown, whole.fit(super::BADGE_CELLS - marker));
        assert!(shown.ends_with(&Suffix::of(a.agent_id).tail()), "{shown:?}");
    }

    /// The snapshot casts share each team's label, so every team badge is suffixed.
    #[test]
    fn every_demo_teams_suffixed_badge_fits() {
        use pixtuoid_core::source::claude_code::{SOURCE_NAME, cc_derive_label};
        for repo in &crate::DEMO_REPOS[..crate::DEMO_TEAMS] {
            let cwd = PathBuf::from("/demo").join(repo);
            let label = cc_derive_label(std::path::Path::new(""), SOURCE_NAME, &cwd);
            let a = slot(&label, "session-aaaa", 0, running());
            let b = slot(&label, "session-bbbb", 1, running());
            let want = format!("{label}{}", Suffix::of(a.agent_id).tail());
            assert_eq!(shown(&[a.clone(), b], &a), want, "{repo}");
        }
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

    #[test]
    fn a_label_of_zero_width_marks_never_shows_the_cap_mark() {
        use pixtuoid_core::source::decoder::{ELLIPSIS, MAX_DECODED_FIELD_CHARS};
        let capped = format!(
            "a{}{ELLIPSIS}",
            "\u{301}".repeat(MAX_DECODED_FIELD_CHARS - 1)
        );
        let a = slot(&capped, "sess-abcd", 0, running());
        let text = shown(std::slice::from_ref(&a), &a);
        assert!(!text.contains(ELLIPSIS), "{text:?}");
    }

    /// A lone agent's label one char past its badge keeps its source prefix:
    /// only a namesake's suffix is kept by cutting the label.
    #[test]
    fn a_lone_label_just_past_its_badge_keeps_its_source_prefix() {
        let marker = crate::display::text::cells(super::BADGE_MARKER.encode_utf8(&mut [0; 4]));
        let budget = usize::from(super::BADGE_CELLS - marker);
        let label = format!("cc\u{b7}{}", "r".repeat(budget - 2));
        let a = slot(&label, "sess-abcd", 0, running());
        let text = shown(std::slice::from_ref(&a), &a);
        assert_eq!(text, label.chars().take(budget).collect::<String>());
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
            let name = shown(std::slice::from_ref(&a), &a);
            let badge = format!("{}{name}", super::BADGE_MARKER);
            let n = cells(&badge);
            assert!(
                (super::BADGE_CELLS - 1..=super::BADGE_CELLS).contains(&n),
                "{badge:?} takes {n} cells"
            );
        }
    }

    /// A badge's name is small text, never large-scale, so it holds WCAG 2.2
    /// AA's 4.5:1 (<https://www.w3.org/TR/WCAG22/#contrast-minimum>) on its
    /// plate; its marker is a graphic, held to
    /// [`NON_TEXT_CONTRAST`](crate::sprite::NON_TEXT_CONTRAST). Over every
    /// registered source and an unregistered one, in every tone. The guarantee holds on the plate,
    /// which the cutaway paints: the painter #873 measured an idle badge in at
    /// 2.05:1, straight on the floor.
    #[test]
    fn every_badge_ink_reads_on_its_plate_in_every_theme() {
        use super::badge_plate_colour;
        use crate::sprite::NON_TEXT_CONTRAST;
        let agents: Vec<AgentSlot> = pixtuoid_core::source::registry::REGISTRY
            .iter()
            .map(|s| s.name)
            .chain(["unregistered"])
            .flat_map(|source| in_every_tone(&of(source, "repo", running())))
            .collect();
        for theme in crate::theme::ALL_THEMES {
            let plate = badge_plate_colour(theme);
            for agent in &agents {
                let ink = badge_ink(agent, theme);
                let name = ink.name.contrast(plate);
                let marker = ink.marker.contrast(plate);
                let at = format!("{} {:?} {}", theme.name, tone_of(agent), agent.source);
                assert!(name >= 4.5, "{at}: name {name:.2}:1");
                assert!(marker >= NON_TEXT_CONTRAST, "{at}: marker {marker:.2}:1");
            }
        }
    }
}
