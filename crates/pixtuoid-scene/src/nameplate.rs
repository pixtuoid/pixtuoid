//! What the office calls each agent: its label, and its own [`Suffix`] where
//! another agent wears the same label (`cc·api·3f2a`). A badge shows it
//! [fit](Nameplate::fit) to its cells; a device listing agents beside the
//! office, whole.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use pixtuoid_core::id::splitmix64;
use pixtuoid_core::state::SlotLabel;
use pixtuoid_core::{AgentId, AgentSlot, SceneState};

/// What tells an agent from its namesakes: 16 bits of its [`AgentId`], which
/// crosses the wire, so a device and its host show one. Displays as its 4 hex
/// digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suffix(u16);

impl Suffix {
    /// `id`'s suffix: the low 16 bits of the [`splitmix64`]-finalized id, the
    /// window `physics`'s `speed_mult` and `pause_ms_for` leave free, so a
    /// suffix says nothing of a walk.
    pub fn of(id: AgentId) -> Self {
        let [low, high, ..] = splitmix64(id.raw()).to_le_bytes();
        Self(u16::from_le_bytes([low, high]))
    }

    /// The tail a name carries it as: [`SlotLabel::SEP`], then the digits.
    pub fn tail(self) -> String {
        format!("{}{self}", SlotLabel::SEP)
    }
}

impl fmt::Display for Suffix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}", self.0)
    }
}

/// An agent's name in its two parts, so a cut can tell a suffix from the
/// label's own `·`; only a count of its namesakes makes one. Displays as the
/// whole name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nameplate {
    label: Arc<str>,
    suffix: Option<Suffix>,
}

impl Nameplate {
    fn of(agent: &AgentSlot, shared: bool) -> Self {
        Self {
            label: agent.label.text(),
            suffix: shared.then(|| Suffix::of(agent.agent_id)),
        }
    }

    /// The agent's label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Its suffix, where a namesake shares the label.
    pub fn suffix(&self) -> Option<Suffix> {
        self.suffix
    }

    /// It in `cells` terminal cells, and as many chars. A suffix is kept by
    /// cutting the label, as a suffix cut short tells nothing
    /// (`TikTok-Android·a`); a name with none is cut from its end.
    pub fn fit(&self, cells: u16) -> Cow<'_, str> {
        let Some(suffix) = self.suffix else {
            return Cow::Borrowed(take_span(&self.label, cells));
        };
        let whole = self.to_string();
        if span(&whole) <= cells {
            return Cow::Owned(whole);
        }
        let tail = suffix.tail();
        let tail_span = span(&tail);
        if tail_span < cells {
            let base = take_span(&self.label, cells - tail_span);
            return Cow::Owned(format!("{base}{tail}"));
        }
        Cow::Owned(take_span(&whole, cells).to_owned())
    }
}

impl fmt::Display for Nameplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)?;
        match self.suffix {
            Some(suffix) => f.write_str(&suffix.tail()),
            None => Ok(()),
        }
    }
}

/// What a name spends of a cell budget: its cells, or its chars where a run of
/// zero-width marks has more.
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

/// The labels a set of agents wear, counted, so each agent's
/// [`nameplate`](Self::nameplate) tells it from its namesakes.
#[derive(Debug)]
pub struct Namesakes<'a>(HashMap<&'a str, usize>);

impl<'a> Namesakes<'a> {
    /// `agents`' labels, counted: the agents a frame draws together, as its
    /// badges count them, agents it does not draw included. A list of a whole
    /// multi-floor office counts each floor with [`FloorNamesakes`].
    pub fn of(agents: impl IntoIterator<Item = &'a AgentSlot>) -> Self {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for agent in agents {
            *counts.entry(&*agent.label).or_insert(0) += 1;
        }
        Self(counts)
    }

    fn shares(&self, agent: &AgentSlot) -> bool {
        self.0.get(&*agent.label).is_some_and(|&n| n > 1)
    }

    /// `agent`'s name, whole: its label, with its suffix when another counted
    /// agent wears that label.
    pub fn nameplate(&self, agent: &AgentSlot) -> Nameplate {
        Nameplate::of(agent, self.shares(agent))
    }
}

/// Each floor's [`Namesakes`], counted over the agents that floor draws, so a
/// list of a whole multi-floor office names each agent as its floor's badges
/// do.
#[derive(Debug)]
pub struct FloorNamesakes<'a>(Vec<Namesakes<'a>>);

impl<'a> FloorNamesakes<'a> {
    /// The floors of `scene`, the whole office: a floor's
    /// [projection](crate::floor::project_floor_scene) re-hosts its desks, so
    /// it reads as no floor's.
    pub fn of(scene: &'a SceneState) -> Self {
        let floors = 0..scene.floor_capacities.len();
        Self(
            floors
                .map(|floor| Namesakes::of(crate::floor::floor_agents(scene, floor)))
                .collect(),
        )
    }

    /// `agent`'s name as its floor's badges read it; an agent no floor draws
    /// is counted on none.
    pub fn nameplate(&self, agent: &AgentSlot) -> Nameplate {
        let shared = self
            .0
            .get(agent.floor_idx)
            .is_some_and(|floor| floor.shares(agent));
        Nameplate::of(agent, shared)
    }
}

#[cfg(test)]
mod tests {
    use super::{Nameplate, Namesakes, Suffix, take_span};
    use crate::character::test_support::make_slot;
    use pixtuoid_core::AgentId;
    use pixtuoid_core::state::{ActivityState, AgentSlot};

    fn agent(label: &str, key: &str) -> AgentSlot {
        let mut a = make_slot(
            AgentId::from_transcript_path(&format!("/p/{key}.jsonl")),
            ActivityState::Idle,
        );
        a.label = label.into();
        a
    }

    /// `label`'s name, with a real suffix when `suffixed`.
    fn plate(label: &str, suffixed: bool) -> Nameplate {
        let id = AgentId::from_transcript_path("/p/plate.jsonl");
        Nameplate {
            label: label.into(),
            suffix: suffixed.then(|| Suffix::of(id)),
        }
    }

    /// Each namesake carries its own suffix after its whole label, whatever
    /// floor it works on: a host can draw a whole office in one view.
    #[test]
    fn colliding_labels_get_disambig_suffixes() {
        let label = "cc\u{b7}a-very-long-project-name";
        let a = agent(label, "a");
        let mut b = agent(label, "b");
        b.floor_idx = a.floor_idx + 1;
        let all = [a.clone(), b.clone()];
        let namesakes = Namesakes::of(&all);
        let want_a = format!("{label}{}", Suffix::of(a.agent_id).tail());
        let want_b = format!("{label}{}", Suffix::of(b.agent_id).tail());
        assert_eq!(namesakes.nameplate(&a).to_string(), want_a);
        assert_eq!(namesakes.nameplate(&b).to_string(), want_b);
        assert_ne!(want_a, want_b);
    }

    #[test]
    fn a_label_no_other_agent_wears_reads_bare() {
        let lone = agent("cc\u{b7}web", "c");
        let all = [
            agent("cc\u{b7}api", "a"),
            agent("cc\u{b7}api", "b"),
            lone.clone(),
        ];
        let bare = plate("cc\u{b7}web", false);
        assert_eq!(Namesakes::of(&all).nameplate(&lone), bare);
        assert_eq!(Namesakes::of([&lone]).nameplate(&lone), bare);
    }

    /// A suffix reads the same on every build and platform that shows its
    /// agent, so a change to how it is derived has to edit these.
    #[test]
    fn a_suffix_is_pinned_per_agent_id() {
        for (source, opaque_id, suffix) in [
            (
                "claude-code",
                "c0f7fb3f-dc9c-47c3-840d-f775dd2855a3",
                "9d1e",
            ),
            ("codex", "019ea57d-7fa7-7812-b864-bdcb9b6c7e17", "d9d7"),
            ("reasonix", "/work/client-x/app", "d5db"),
        ] {
            let id = AgentId::from_parts(source, opaque_id);
            assert_eq!(Suffix::of(id).to_string(), suffix, "{source} {opaque_id}");
        }
    }

    /// A device names each namesake as its host does.
    #[test]
    fn a_device_reads_its_hosts_nameplates() {
        use pixtuoid_core::remote::{RemoteOffice, encode};
        use pixtuoid_core::state::MAX_FLOORS;
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let mut host = pixtuoid_core::SceneState::uniform(4);
        for key in ["a", "b"] {
            let mut a = agent("cc\u{b7}api", key);
            a.cwd = std::sync::Arc::from(std::path::Path::new("/work/api"));
            a.session_id = format!("host-session-{key}").into();
            host.agents.insert(a.agent_id, a);
        }
        let line = encode(&host, now).expect("a scene encodes");
        let mut device = RemoteOffice::new([4; MAX_FLOORS]);
        device.apply(line.as_bytes(), now).expect("it applies");
        let (on_host, on_device) = (
            Namesakes::of(host.agents.values()),
            Namesakes::of(device.scene().agents.values()),
        );
        for agent in host.agents.values() {
            let named = on_host.nameplate(agent);
            assert!(named.suffix().is_some(), "premise: {named:?}");
            assert_eq!(
                on_device.nameplate(&device.scene().agents[&agent.agent_id]),
                named
            );
        }
    }

    #[test]
    fn a_fit_passes_a_short_name_through() {
        assert_eq!(plate("hello", false).fit(16), "hello");
        let short = plate("hi", true);
        assert_eq!(short.fit(16), short.to_string());
    }

    #[test]
    fn a_fit_keeps_the_suffix() {
        let name = plate("TikTok-Android", true);
        let tail = name.suffix().expect("suffixed").tail();
        let out = name.fit(16);
        assert_eq!(out.chars().count(), 16);
        assert!(out.ends_with(&tail), "suffix lost: {out}");
        assert!(out.starts_with("TikTok"), "label over-cut: {out}");
    }

    #[test]
    fn a_fit_cuts_a_bare_name_from_its_end() {
        assert_eq!(plate("a-very-long-project-name", false).fit(8), "a-very-l");
    }

    /// Cells too few for the tail take the whole name's start.
    #[test]
    fn a_fit_too_narrow_for_the_tail_cuts_the_whole_name() {
        let name = plate("x", true);
        let cells = 4;
        let start: String = name.to_string().chars().take(usize::from(cells)).collect();
        assert_eq!(name.fit(cells), start);
    }

    /// The budget is in cells, which a CJK character takes two of: a wide name
    /// gets half the characters, the suffix kept as for any other.
    #[test]
    fn a_fit_budgets_cells_not_chars() {
        assert_eq!(plate("日本語プロジェクト", false).fit(8), "日本語プ");
        let suffixed = plate("日本語プロジェクト", true);
        let tail = suffixed.suffix().expect("suffixed").tail();
        assert_eq!(suffixed.fit(12), format!("日本語{tail}"));
    }

    /// A budget never splits a grapheme cluster: a ZWJ sequence is kept whole
    /// or dropped whole.
    #[test]
    fn a_take_keeps_clusters_whole() {
        let coder = "\u{1f469}\u{200d}\u{1f4bb}";
        let text = format!("a{coder}b");
        // The sequence is three chars in two cells, so it takes three.
        assert_eq!(take_span(&text, 4), format!("a{coder}"));
        assert_eq!(take_span(&text, 3), "a");
        assert_eq!(take_span(&text, u16::MAX), text);
    }

    /// A first cluster wider than the budget takes nothing, as a terminal
    /// can't draw half of it.
    #[test]
    fn a_cluster_wider_than_the_budget_is_dropped() {
        assert_eq!(take_span("\u{65e5}x", 1), "");
    }
}
