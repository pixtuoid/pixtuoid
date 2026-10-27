//! Token meter — how much an agent's session has BURNED, as a desk visual: a
//! paper tower on the desk's right wing grows one 2px ream per tier, and a big
//! spend reading drops a visible sheet onto the pile.
//!
//! The RAW counters live on the slot (`AgentSlot::{tokens_used, last_usage}`,
//! core); ALL interpretation happens here. The meter reads FRESH tokens only
//! (new input + cache writes + output; cache READS are excluded at decode):
//! cumulative per session, monotone — no ceilings, no model→window tables.

use std::time::SystemTime;

use pixtuoid_core::AgentSlot;

/// T1's threshold; T2/T3 derive by `TIER_FACTOR`. The site copy
/// (`site/src/features.json` "Token meter" desc → the README table) repeats the
/// derived thresholds — update it on recalibration.
pub(crate) const TIER_BASE_TOKENS: u64 = 250_000;
/// The geometric step between tiers — wide enough that crossing a tier stays
/// an event (a linear ladder saturates by mid-session).
pub(crate) const TIER_FACTOR: u64 = 8;
/// The ladder caps here — another ream would climb over the monitor.
pub(crate) const MAX_TIER: u8 = 3;

/// A single usage reading must clear this to drop a visible sheet, so routine
/// per-turn readings stay silent and sheet frequency itself reads as burn rate.
pub(crate) const SHEET_MIN_DELTA_TOKENS: u64 = 25_000;
/// How long the dropped sheet is in the air (ease-in fall).
pub(crate) const SHEET_FALL_MS: u64 = 550;
/// Fall start height above the stack top, in buffer px.
pub(crate) const SHEET_FALL_PX: u16 = 6;

/// Cumulative fresh tokens → tier 0..=`MAX_TIER`. Pure and monotone.
pub fn token_tier(tokens_used: u64) -> u8 {
    let mut tier = 0u8;
    let mut threshold = TIER_BASE_TOKENS;
    while tier < MAX_TIER && tokens_used >= threshold {
        tier += 1;
        threshold = threshold.saturating_mul(TIER_FACTOR);
    }
    tier
}

/// The falling sheet's distance FALLEN (0..=[`SHEET_FALL_PX`]) at `now`, or
/// `None` when no sheet is in the air (reading too small, too old, or the clock
/// went backwards). The ease-in is hand-rolled in integer math because
/// `anim::eased_progress`'s clamped f32 curve cannot express the backward-clock
/// `None`, and integers keep the goldens deterministic.
pub(crate) fn sheet_fall_dist(slot: &AgentSlot, now: SystemTime) -> Option<u16> {
    let reading = slot.last_usage.as_ref()?;
    if reading.delta < SHEET_MIN_DELTA_TOKENS {
        return None;
    }
    let elapsed = now.duration_since(reading.seen_at).ok()?.as_millis() as u64;
    if elapsed >= SHEET_FALL_MS {
        return None;
    }
    let px = u64::from(SHEET_FALL_PX);
    Some(((px * elapsed * elapsed) / (SHEET_FALL_MS * SHEET_FALL_MS)) as u16)
}

/// Compact human form for the dossier row: `9.5K` / `2.4M` / `816` —
/// one decimal under 10 of the unit, none above.
pub(crate) fn compact_tokens(tokens: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000_000;
    match tokens {
        t if t >= M => format_scaled(t, M, 'M'),
        t if t >= K => format_scaled(t, K, 'K'),
        t => t.to_string(),
    }
}

fn format_scaled(t: u64, unit: u64, suffix: char) -> String {
    // u128 intermediate: the reducer saturates tokens_used to u64::MAX on a
    // hostile transcript, so `t * 10` would overflow in the tooltip's paint path.
    let tenths = (u128::from(t) * 10 / u128::from(unit)) as u64;
    if tenths >= 100 {
        format!("{}{suffix}", tenths / 10)
    } else {
        format!("{}.{}{suffix}", tenths / 10, tenths % 10)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn tier_ladder_boundaries_derive_from_the_consts() {
        let t1 = TIER_BASE_TOKENS;
        let t2 = t1 * TIER_FACTOR;
        let t3 = t2 * TIER_FACTOR;
        assert_eq!(token_tier(0), 0);
        assert_eq!(token_tier(t1 - 1), 0);
        assert_eq!(token_tier(t1), 1);
        assert_eq!(token_tier(t2 - 1), 1);
        assert_eq!(token_tier(t2), 2);
        assert_eq!(token_tier(t3 - 1), 2);
        assert_eq!(token_tier(t3), 3);
        assert_eq!(token_tier(t3 * TIER_FACTOR), MAX_TIER);
        assert_eq!(token_tier(u64::MAX), MAX_TIER);
    }

    fn slot_with_usage(delta: u64, at: SystemTime) -> AgentSlot {
        use pixtuoid_core::state::ActivityState;
        use std::sync::Arc;
        let now = SystemTime::UNIX_EPOCH;
        AgentSlot {
            agent_id: pixtuoid_core::AgentId::from_parts("claude-code", "ses_t"),
            source: Arc::from("claude-code"),
            session_id: Arc::from("ses_t"),
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
            running_ms: 0,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: Some(pixtuoid_core::state::UsageObservation::new(delta, at)),
        }
    }

    #[test]
    fn sheet_falls_only_for_a_big_fresh_reading_within_the_window() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let slot = slot_with_usage(SHEET_MIN_DELTA_TOKENS, t0);
        let early = sheet_fall_dist(&slot, t0).expect("just dropped");
        let late = sheet_fall_dist(&slot, t0 + Duration::from_millis(SHEET_FALL_MS - 1))
            .expect("still falling");
        assert!(early < late, "ease-in must descend: {early} !< {late}");
        assert!(late <= SHEET_FALL_PX);
        assert_eq!(
            sheet_fall_dist(&slot, t0 + Duration::from_millis(SHEET_FALL_MS)),
            None
        );
        let small = slot_with_usage(SHEET_MIN_DELTA_TOKENS - 1, t0);
        assert_eq!(sheet_fall_dist(&small, t0), None);
        let mut none = slot_with_usage(SHEET_MIN_DELTA_TOKENS, t0);
        none.last_usage = None;
        assert_eq!(sheet_fall_dist(&none, t0), None);
        // Clock skew: a reading in the future stays silent instead of panicking.
        assert_eq!(sheet_fall_dist(&slot, t0 - Duration::from_secs(1)), None);
    }

    #[test]
    fn compact_tokens_reads_like_the_dossier() {
        assert_eq!(compact_tokens(0), "0");
        assert_eq!(compact_tokens(816), "816");
        assert_eq!(compact_tokens(9_500), "9.5K");
        assert_eq!(compact_tokens(56_500), "56K");
        assert_eq!(compact_tokens(999_999), "999K");
        assert_eq!(compact_tokens(2_400_000), "2.4M");
        assert_eq!(compact_tokens(80_130_000), "80M");
        // The reducer clamps a hostile accumulation to u64::MAX — it must
        // render, not overflow.
        assert_eq!(compact_tokens(u64::MAX), "18446744073709M");
    }
}
