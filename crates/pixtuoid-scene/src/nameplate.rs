//! What the office calls each agent: its label, and its session's suffix where
//! another agent wears the same label (`cc·api·3f2a`). A badge shows it cut to
//! its cells; a device listing agents beside the office, whole.

use std::borrow::Cow;
use std::collections::HashMap;

use pixtuoid_core::AgentSlot;

/// The separator between a label's source prefix and its cwd/disambiguation
/// tail (`cc·repo`, `cc·repo·1a2b`). The label is WRITTEN core-side as a bare
/// `·` at THREE sites — `decoder::cwd_basename_label` (the chokepoint),
/// `claude_code::cc_derive_label`'s project-dir fallback, and the reducer's
/// SessionStart back-fill, both of which document their bypass — and a crate
/// boundary keeps this const out of reach of all three, so it must MATCH that
/// char.
pub(crate) const LABEL_SEP: char = '\u{b7}';

/// The labels a set of agents wear, counted, so each agent's
/// [`label`](Self::label) tells it from its namesakes.
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

    /// `agent`'s name, whole: its label, with its session's suffix when
    /// another counted agent wears that label.
    pub fn label<'b>(&self, agent: &'b AgentSlot) -> Cow<'b, str> {
        if self.0.get(&*agent.label).copied().unwrap_or(0) > 1 {
            let id4 = disambig_suffix(&agent.session_id);
            Cow::Owned(format!("{}{LABEL_SEP}{id4}", agent.label))
        } else {
            Cow::Borrowed(&agent.label)
        }
    }
}

/// 4-hex-char disambiguation suffix, hashed from the WHOLE `session_id` —
/// shape-agnostic where any slice of the id is not (a raw-cwd id collides on
/// both head and tail: `/x/app` vs `/y/app`), and multi-byte-safe by
/// construction. `DefaultHasher` is per-process: a display aid, not an id.
#[doc(hidden)]
pub fn disambig_suffix(session_id: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    session_id.hash(&mut h);
    format!("{:04x}", h.finish() & 0xffff)
}

#[cfg(test)]
mod tests {
    use super::{Namesakes, disambig_suffix};
    use crate::character::test_support::make_slot;
    use pixtuoid_core::AgentId;
    use pixtuoid_core::state::{ActivityState, AgentSlot};

    fn agent(label: &str, session_id: &str) -> AgentSlot {
        let mut a = make_slot(
            AgentId::from_transcript_path(&format!("/p/{label}-{session_id}.jsonl")),
            ActivityState::Idle,
        );
        a.label = label.into();
        a.session_id = session_id.into();
        a
    }

    /// Each namesake carries its own session's suffix after its whole label,
    /// whatever floor it works on: a host can draw a whole office in one view.
    #[test]
    fn colliding_labels_get_disambig_suffixes() {
        let label = "cc\u{b7}a-very-long-project-name";
        let a = agent(label, "session-aaaa");
        let mut b = agent(label, "session-bbbb");
        b.floor_idx = a.floor_idx + 1;
        let all = [a.clone(), b.clone()];
        let namesakes = Namesakes::of(&all);
        let want_a = format!("{label}\u{b7}{}", disambig_suffix("session-aaaa"));
        let want_b = format!("{label}\u{b7}{}", disambig_suffix("session-bbbb"));
        assert_eq!(namesakes.label(&a), want_a);
        assert_eq!(namesakes.label(&b), want_b);
        assert_ne!(want_a, want_b);
    }

    #[test]
    fn a_label_no_other_agent_wears_reads_bare() {
        let lone = agent("cc\u{b7}web", "session-cccc");
        let all = [
            agent("cc\u{b7}api", "session-aaaa"),
            agent("cc\u{b7}api", "session-bbbb"),
            lone.clone(),
        ];
        assert_eq!(Namesakes::of(&all).label(&lone), "cc\u{b7}web");
        assert_eq!(Namesakes::of([&lone]).label(&lone), "cc\u{b7}web");
    }

    /// The suffix hashes the whole id, so a one-char id tells a namesake apart
    /// as well as a long one.
    #[test]
    fn namesakes_with_one_char_session_ids_get_distinct_suffixes() {
        let a = agent("cc\u{b7}api", "a");
        let b = agent("cc\u{b7}api", "b");
        let all = [a.clone(), b.clone()];
        let namesakes = Namesakes::of(&all);
        assert_eq!(
            namesakes.label(&a),
            format!("cc\u{b7}api\u{b7}{}", disambig_suffix("a"))
        );
        assert_ne!(namesakes.label(&a), namesakes.label(&b));
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

    #[test]
    fn multibyte_ids_are_safe_and_deterministic() {
        let a = disambig_suffix("/naïveté/app");
        assert_eq!(a, disambig_suffix("/naïveté/app"));
        assert_eq!(a.len(), 4);
    }
}
