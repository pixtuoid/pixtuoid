//! The per-scene tally of agent activity and gateway liveness, computed once a
//! frame and read by the footer, the neon sign and the audio, so no two
//! surfaces count apart, and the day's count as every surface words it.

use std::time::SystemTime;

use chrono::NaiveDate;
use pixtuoid_core::state::{ActivityState, DaemonPresence, DaemonState, MAX_FLOORS, Today};
use pixtuoid_core::{AgentSlot, SceneState};

/// Per-scene tally of agent activity states, computed once per frame and shared
/// by the footer and the board so the two surfaces can't disagree. `exiting` is a
/// first-class bucket, NOT folded into idle, so the footer's `n/total` counts
/// walkouts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StateCounts {
    pub running: usize,
    pub requires_action: usize,
    pub idle: usize,
    pub exiting: usize,
    pub total: usize,
}

/// Add one slot to `c` under the ONE exiting-first bucketing policy: an
/// **exiting** agent counts as `exiting` regardless of its last activity state.
fn bucket_slot(c: &mut StateCounts, slot: &AgentSlot) {
    c.total += 1;
    if slot.exiting_at.is_some() {
        c.exiting += 1;
        return;
    }
    match slot.state {
        ActivityState::Running { .. } => c.running += 1,
        ActivityState::RequiresAction { .. } => c.requires_action += 1,
        ActivityState::Idle => c.idle += 1,
    }
}

/// Bucket every agent in `scene` — the office-wide (or current-projected-floor)
/// tally.
pub fn scene_stats(scene: &SceneState) -> StateCounts {
    let mut c = StateCounts::default();
    for slot in scene.agents.values() {
        bucket_slot(&mut c, slot);
    }
    debug_assert_eq!(c.running + c.requires_action + c.idle + c.exiting, c.total);
    c
}

/// Per-floor [`StateCounts`], bucketed by `AgentSlot.floor_idx` (clamped to the
/// last floor). Computed from the FULL scene, deliberately distinct from
/// `scene_stats` on the projected floor — don't derive one from the other.
pub fn per_floor_counts(scene: &SceneState) -> [StateCounts; MAX_FLOORS] {
    let mut floors = [StateCounts::default(); MAX_FLOORS];
    for slot in scene.agents.values() {
        bucket_slot(&mut floors[slot.floor_idx.min(MAX_FLOORS - 1)], slot);
    }
    floors
}

/// The worst-of daemon-liveness rollup for the gateway chip. `None` = no daemon
/// configured (chip suppressed), distinct from `Some(DaemonState::Down)` (a
/// daemon was seen, then died). `DaemonState` has no `Ord`, hence the explicit
/// severity rank.
pub fn gateway_rollup<'a>(
    daemons: impl Iterator<Item = &'a DaemonPresence>,
) -> Option<DaemonState> {
    fn severity(s: DaemonState) -> u8 {
        match s {
            DaemonState::Idle => 0,
            DaemonState::Running => 1,
            DaemonState::Degraded => 2,
            DaemonState::Down => 3,
        }
    }
    daemons
        .map(|p| p.display_state())
        .max_by_key(|s| severity(*s))
}

/// The [`gateway_rollup`] over every daemon in `scene`.
pub fn office_gateway(scene: &SceneState) -> Option<DaemonState> {
    gateway_rollup(scene.daemons().map(|(_, _, p)| p))
}

/// `now`'s local date, what a host's reducer dates [`Today::day`] by.
pub fn local_day(now: SystemTime) -> NaiveDate {
    chrono::DateTime::<chrono::Local>::from(now).date_naive()
}

/// What every reader shows of `today`: its tokens, else its tool calls, else
/// nothing, so nobody sees `0`.
pub(crate) fn today_headline(today: Today) -> Option<String> {
    let (count, unit) = match (today.tokens, today.tool_calls) {
        (0, 0) => return None,
        (0, calls) => (calls, "tool call"),
        (tokens, _) => (tokens, "token"),
    };
    let plural = if count == 1 { "" } else { "s" };
    let count = crate::token_meter::compact_tokens(count);
    Some(format!("{count} {unit}{plural}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headline(tokens: u64, tool_calls: u64) -> Option<String> {
        let day = NaiveDate::from_ymd_opt(2027, 1, 15).expect("a date");
        today_headline(Today::new(day, tokens, tool_calls))
    }

    #[test]
    fn the_headline_is_the_tokens_else_the_calls_else_nothing() {
        assert_eq!(headline(1_200_000, 38).as_deref(), Some("1.2M tokens"));
        assert_eq!(headline(0, 38).as_deref(), Some("38 tool calls"));
        assert_eq!(headline(0, 0), None, "nobody sees 0");
    }

    #[test]
    fn one_of_anything_is_never_pluralised() {
        assert_eq!(headline(1, 38).as_deref(), Some("1 token"));
        assert_eq!(headline(0, 1).as_deref(), Some("1 tool call"));
    }

    #[test]
    fn a_big_count_of_calls_reads_compactly_too() {
        assert_eq!(headline(0, 1_500).as_deref(), Some("1.5K tool calls"));
    }
}
