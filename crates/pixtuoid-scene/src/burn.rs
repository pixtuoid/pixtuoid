//! Burn tier — how expensive an agent's LLM brain is, as a VISUAL fact.
//!
//! The model is the GATE, effort is the SPLIT (user-pinned): only the
//! `TOP_MODELS` ever color at all, and only a FRESH `MAX_EFFORTS` effort
//! promotes one to the flame crown. Everything else — including
//! opus/gpt-5.5-class flagships — stays [`BurnTier::Normal`].

use std::time::SystemTime;

use pixtuoid_core::AgentSlot;

/// Model prefixes that gate the color tiers — most-specific-first, first match
/// wins. PREFIX matching is deliberate (version-independent), so a future
/// CHEAPER variant sharing a prefix would wrongly burn until a Normal-override
/// row is added ABOVE its family line.
const TOP_MODELS: &[&str] = &["claude-fable", "claude-mythos", "gpt-5.6-sol"];

/// Effort values that set a top model on fire.
const MAX_EFFORTS: &[&str] = &["ultra", "ultrathink", "xhigh", "max"];

/// How long an effort observation stays "fresh". The per-turn carriers (Codex,
/// CC) re-stamp well inside this window; omp's fires only on CHANGE, so a
/// pinned max-effort session under-flames past it — cosmetic, accepted.
pub(crate) const EFFORT_TTL_SECS: u64 = 600;

/// The three visual tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BurnTier {
    /// Unchanged agent-seeded hair.
    Normal,
    /// Ember-red hair (a top model at ordinary effort).
    Premium,
    /// Flame crown (a top model at fresh max effort).
    Top,
}

/// The pure tier judgment. `effort_fresh` must already be freshness-filtered
/// by the caller (see [`slot_burn_tier`]).
pub(crate) fn burn_tier(model: Option<&str>, effort_fresh: Option<&str>) -> BurnTier {
    let top = model.is_some_and(|m| TOP_MODELS.iter().any(|p| m.starts_with(p)));
    if !top {
        return BurnTier::Normal;
    }
    if effort_fresh.is_some_and(|e| MAX_EFFORTS.contains(&e)) {
        BurnTier::Top
    } else {
        BurnTier::Premium
    }
}

/// The slot's effort observation, IF still fresh — the ONE freshness rule
/// shared by the tier judgment and the dossier's `· effort` suffix, so the
/// tooltip can't show an effort the flame already decayed past.
pub(crate) fn fresh_effort(slot: &AgentSlot, now: SystemTime) -> Option<&str> {
    slot.effort.as_ref().and_then(|obs| {
        // The CC decoder's exit sentinel exists ONLY to kill the flame via
        // last-seen-wins — it is not an effort; the dossier must never show it.
        if &*obs.value == pixtuoid_core::source::claude_code::ULTRA_EXIT_LABEL {
            return None;
        }
        let fresh = now
            .duration_since(obs.seen_at)
            .map(|d| d.as_secs() <= EFFORT_TTL_SECS)
            // A future-stamped observation (clock skew) counts as fresh.
            .unwrap_or(true);
        fresh.then_some(&*obs.value)
    })
}

/// The slot-level judgment the sim and the recolor call: freshness-filter,
/// then `burn_tier`.
pub(crate) fn slot_burn_tier(slot: &AgentSlot, now: SystemTime) -> BurnTier {
    burn_tier(slot.model.as_deref(), fresh_effort(slot, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn only_the_top_set_models_color_at_all() {
        for m in ["claude-fable-5", "claude-mythos-5", "gpt-5.6-sol"] {
            assert_eq!(burn_tier(Some(m), None), BurnTier::Premium, "{m}");
        }
        for m in [
            "claude-opus-4-8",
            "claude-sonnet-5",
            "gpt-5.5",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "deepseek-v4-flash-free",
            "unknown-model",
        ] {
            assert_eq!(burn_tier(Some(m), None), BurnTier::Normal, "{m}");
        }
        assert_eq!(burn_tier(None, Some("ultra")), BurnTier::Normal, "no model");
    }

    #[test]
    fn fresh_max_effort_sets_a_top_model_on_fire() {
        for e in ["ultra", "ultrathink", "xhigh", "max"] {
            assert_eq!(
                burn_tier(Some("claude-fable-5"), Some(e)),
                BurnTier::Top,
                "{e}"
            );
        }
        assert_eq!(
            burn_tier(Some("claude-fable-5"), Some("medium")),
            BurnTier::Premium
        );
        assert_eq!(
            burn_tier(Some("claude-fable-5"), Some("ultra_exit")),
            BurnTier::Premium
        );
        assert_eq!(burn_tier(Some("gpt-5.5"), Some("xhigh")), BurnTier::Normal);
    }

    fn slot() -> AgentSlot {
        use pixtuoid_core::state::ActivityState;
        use std::sync::Arc;
        let now = SystemTime::UNIX_EPOCH;
        AgentSlot {
            agent_id: pixtuoid_core::AgentId::from_parts("claude-code", "ses_b"),
            source: Arc::from("claude-code"),
            session_id: Arc::from("ses_b"),
            cwd: Arc::from(std::path::PathBuf::from("/w").as_path()),
            label: "x".into(),
            state: ActivityState::Idle,
            state_started_at: now,
            last_event_at: now,
            created_at: now,
            exiting_at: None,
            desk_index: pixtuoid_core::GlobalDeskIndex(0),
            floor_idx: 0,
            tool_call_count: 0,
            active_ms: 0,
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
    fn the_exit_sentinel_never_reaches_the_dossier() {
        use pixtuoid_core::source::claude_code::ULTRA_EXIT_LABEL;
        use pixtuoid_core::state::EffortObservation;
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut slot = slot();
        slot.model = Some("claude-fable-5".into());
        slot.effort = Some(EffortObservation::new(ULTRA_EXIT_LABEL.into(), t0));
        assert_eq!(fresh_effort(&slot, t0), None);
        assert_eq!(slot_burn_tier(&slot, t0), BurnTier::Premium);
        slot.effort = Some(EffortObservation::new("ultra".into(), t0));
        assert_eq!(fresh_effort(&slot, t0), Some("ultra"));
    }

    #[test]
    fn slot_tier_applies_the_effort_ttl() {
        use pixtuoid_core::state::EffortObservation;
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut slot = slot();
        slot.model = Some("claude-fable-5".into());
        slot.effort = Some(EffortObservation::new("ultra".into(), t0));
        assert_eq!(
            slot_burn_tier(&slot, t0 + Duration::from_secs(super::EFFORT_TTL_SECS)),
            BurnTier::Top
        );
        assert_eq!(
            slot_burn_tier(&slot, t0 + Duration::from_secs(super::EFFORT_TTL_SECS + 1)),
            BurnTier::Premium
        );
        slot.model = None;
        assert_eq!(slot_burn_tier(&slot, t0), BurnTier::Normal);
    }
}
