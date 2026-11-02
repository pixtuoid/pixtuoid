//! What the office calls each agent: its label, and its own suffix where
//! another agent wears the same label (`cc·api·3f2a`). A badge shows it cut to
//! its cells; a device listing agents beside the office, whole.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use pixtuoid_core::id::splitmix64;
use pixtuoid_core::{AgentId, AgentSlot};

/// The separator between a label's source prefix and its cwd/disambiguation
/// tail (`cc·repo`, `cc·repo·1a2b`). The label is WRITTEN core-side as a bare
/// `·` at THREE sites — `decoder::cwd_basename_label` (the chokepoint),
/// `claude_code::cc_derive_label`'s project-dir fallback, and the reducer's
/// SessionStart back-fill, both of which document their bypass — and a crate
/// boundary keeps this const out of reach of all three, so it must MATCH that
/// char.
pub(crate) const LABEL_SEP: char = '\u{b7}';

/// An agent's name in its two parts, so a cut can tell a suffix from the
/// label's own `·`. Displays as the whole name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nameplate {
    /// The agent's label.
    pub label: Arc<str>,
    /// Its [suffix](disambig_suffix), where a namesake shares the label.
    pub suffix: Option<String>,
}

impl fmt::Display for Nameplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)?;
        match &self.suffix {
            Some(suffix) => write!(f, "{LABEL_SEP}{suffix}"),
            None => Ok(()),
        }
    }
}

/// The labels a set of agents wear, counted, so each agent's
/// [`nameplate`](Self::nameplate) tells it from its namesakes.
#[derive(Debug)]
pub struct Namesakes<'a>(HashMap<&'a str, usize>);

impl<'a> Namesakes<'a> {
    /// `agents`' labels, counted. Count what a badge counts to name each agent
    /// as its badge does: the scene its office draws, one floor's
    /// [projection](crate::floor::project_floor_scene) in a multi-floor
    /// office, agents it does not draw included.
    pub fn of(agents: impl IntoIterator<Item = &'a AgentSlot>) -> Self {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for agent in agents {
            *counts.entry(&*agent.label).or_insert(0) += 1;
        }
        Self(counts)
    }

    /// `agent`'s name, whole: its label, with its suffix when another counted
    /// agent wears that label.
    pub fn nameplate(&self, agent: &AgentSlot) -> Nameplate {
        let shared = self.0.get(&*agent.label).copied().unwrap_or(0) > 1;
        Nameplate {
            label: agent.label.text(),
            suffix: shared.then(|| disambig_suffix(agent.agent_id)),
        }
    }
}

/// `id`'s 4-hex-digit suffix, from the [`AgentId`] that crosses the wire, so a
/// device and its host show one suffix: the low 16 bits of the
/// [`splitmix64`]-finalized id, the window `physics`'s `speed_mult` and
/// `pause_ms_for` leave free, so a suffix says nothing of a walk.
#[doc(hidden)]
pub fn disambig_suffix(id: AgentId) -> String {
    format!("{:04x}", splitmix64(id.raw()) & u64::from(u16::MAX))
}

#[cfg(test)]
mod tests {
    use super::{Namesakes, disambig_suffix};
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
        let want_a = format!("{label}\u{b7}{}", disambig_suffix(a.agent_id));
        let want_b = format!("{label}\u{b7}{}", disambig_suffix(b.agent_id));
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
        let bare = super::Nameplate {
            label: "cc\u{b7}web".into(),
            suffix: None,
        };
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
            assert_eq!(disambig_suffix(id), suffix, "{source} {opaque_id}");
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
            assert!(named.suffix.is_some(), "premise: {named:?}");
            assert_eq!(
                on_device.nameplate(&device.scene().agents[&agent.agent_id]),
                named
            );
        }
    }
}
