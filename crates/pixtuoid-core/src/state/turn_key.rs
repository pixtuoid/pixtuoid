/// A turn's correlation key: codex's `turn_id` on its rollout and hooks, and a
/// daemon's `runId`, else its coarse fallback ([`crate::source::openclaw`]'s
/// `turn_key`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TurnKey {
    /// A position on the agent's ordered bracket stream: opencode's durable
    /// `seq`.
    Seq(u64),
    /// An opaque wire id.
    Id(#[serde(with = "super::arc_str_serde")] std::sync::Arc<str>),
}

impl TurnKey {
    /// Whether this turn comes after `other` on its stream; `None` where no
    /// order is known: an opaque id carries none, and two kinds share no
    /// stream.
    pub(crate) fn newer_than(&self, other: &Self) -> Option<bool> {
        match (self, other) {
            (Self::Seq(this), Self::Seq(other)) => Some(this > other),
            (Self::Seq(_) | Self::Id(_), _) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_two_stream_positions_are_ordered() {
        let id = |k: &str| TurnKey::Id(k.into());
        assert_eq!(TurnKey::Seq(2).newer_than(&TurnKey::Seq(1)), Some(true));
        assert_eq!(TurnKey::Seq(1).newer_than(&TurnKey::Seq(2)), Some(false));
        assert_eq!(TurnKey::Seq(1).newer_than(&TurnKey::Seq(1)), Some(false));
        assert_eq!(TurnKey::Seq(1).newer_than(&id("a")), None);
        assert_eq!(id("b").newer_than(&id("a")), None);
        assert_eq!(id("a").newer_than(&TurnKey::Seq(1)), None);
    }
}
