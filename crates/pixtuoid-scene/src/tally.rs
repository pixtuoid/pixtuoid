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
    /// Agents at work.
    pub running: usize,
    /// Agents waiting on their user.
    pub requires_action: usize,
    /// Agents between turns.
    pub idle: usize,
    /// Agents walking out, whatever they last did.
    pub exiting: usize,
    /// Every agent: the other four's sum.
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
#[doc(hidden)]
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
#[doc(hidden)]
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
#[doc(hidden)]
pub fn office_gateway(scene: &SceneState) -> Option<DaemonState> {
    gateway_rollup(scene.daemons().map(|(_, _, p)| p))
}

/// `now`'s local date, what a host's reducer dates [`Today::day`] by.
#[doc(hidden)]
pub fn local_day(now: SystemTime) -> NaiveDate {
    chrono::DateTime::<chrono::Local>::from(now).date_naive()
}

/// What every reader shows of `today`: its tokens, else its tool calls, else
/// nothing, so nobody sees `0`. Public so a device shows today's words as the
/// sign does, not a second wording.
///
/// ```
/// use pixtuoid_core::state::Today;
/// use pixtuoid_scene::tally::{local_day, today_headline};
///
/// let day = local_day(std::time::SystemTime::now());
/// assert_eq!(
///     today_headline(Today::new(day, 1_200_000, 38)).as_deref(),
///     Some("1.2M tokens"),
/// );
/// assert_eq!(today_headline(Today::new(day, 0, 0)), None);
/// ```
pub fn today_headline(today: Today) -> Option<String> {
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

    #[test]
    fn the_headline_is_the_tokens_else_the_calls_else_nothing() {
        let rows = [
            (1_200_000, 38, Some("1.2M tokens")),
            (0, 38, Some("38 tool calls")),
            (0, 0, None),
            (1, 38, Some("1 token")),
            (0, 1, Some("1 tool call")),
            (0, 1_500, Some("1.5K tool calls")),
        ];
        for (tokens, tool_calls, words) in rows {
            let today = Today::new(NaiveDate::MIN, tokens, tool_calls);
            assert_eq!(today_headline(today).as_deref(), words);
        }
    }
}
