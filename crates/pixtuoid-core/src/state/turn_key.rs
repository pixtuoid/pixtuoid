/// An agent turn's identity, as a source's wire names it.
///
/// `Seq` is a stream position (ordered); `Id` is opaque and carries no order,
/// so staleness is the named [`TurnKey::newer_than`], never `PartialOrd`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TurnKey {
    /// A position in a source's ordered stream.
    Seq(u64),
    /// An opaque wire id (a codex `turn_id`, a daemon's run key).
    Id(std::sync::Arc<str>),
}

impl TurnKey {
    /// Whether `self` is newer than `other`: `None` when the keys carry no
    /// order (an `Id`, or two different variants).
    pub fn newer_than(&self, other: &Self) -> Option<bool> {
        match (self, other) {
            (Self::Seq(a), Self::Seq(b)) => Some(a > b),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TurnKey;

    #[test]
    fn only_two_seq_keys_are_ordered() {
        let id = |s: &str| TurnKey::Id(s.into());
        assert_eq!(TurnKey::Seq(2).newer_than(&TurnKey::Seq(1)), Some(true));
        assert_eq!(TurnKey::Seq(1).newer_than(&TurnKey::Seq(2)), Some(false));
        assert_eq!(TurnKey::Seq(1).newer_than(&TurnKey::Seq(1)), Some(false));
        assert_eq!(id("b").newer_than(&id("a")), None);
        assert_eq!(TurnKey::Seq(1).newer_than(&id("a")), None);
        assert_eq!(id("a").newer_than(&TurnKey::Seq(1)), None);
    }
}
