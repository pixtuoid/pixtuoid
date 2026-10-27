/// A turn's correlation key: codex's `turn_id` on its rollout and hooks, and a
/// daemon's `runId`, else its coarse fallback ([`crate::source::openclaw`]'s
/// `turn_key`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TurnKey {
    /// An opaque wire id.
    Id(#[serde(with = "super::arc_str_serde")] std::sync::Arc<str>),
}
