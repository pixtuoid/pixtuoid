/// A daemon turn's correlation key. Today's one producer is openclaw's
/// `runId`, else its coarse fallback ([`crate::source::openclaw`]'s `turn_key`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TurnKey {
    /// An opaque wire id.
    Id(std::sync::Arc<str>),
}
