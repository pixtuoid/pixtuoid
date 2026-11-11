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

/// [`bucket_slot`] over `slots`.
fn counts<'a>(slots: impl Iterator<Item = &'a AgentSlot>) -> StateCounts {
    let mut c = StateCounts::default();
    for slot in slots {
        bucket_slot(&mut c, slot);
    }
    c
}

/// Bucket every agent in `scene` — the office-wide (or current-projected-floor)
/// tally.
pub fn scene_stats(scene: &SceneState) -> StateCounts {
    let c = counts(scene.agents.values());
    debug_assert_eq!(c.running + c.requires_action + c.idle + c.exiting, c.total);
    c
}

/// Per-floor [`StateCounts`]: a floor counts the agents its projection draws
/// ([`floor_agents`](crate::floor::floor_agents)), so an unseated agent is on
/// no floor's count.
/// Computed from the FULL scene, deliberately distinct from `scene_stats` on
/// the projected floor — don't derive one from the other.
#[doc(hidden)]
pub fn per_floor_counts(scene: &SceneState) -> [StateCounts; MAX_FLOORS] {
    std::array::from_fn(|floor| counts(crate::floor::floor_agents(scene, floor)))
}

/// The unseated agents' [`StateCounts`]: on no floor, so no
/// [`per_floor_counts`] bucket holds them.
pub(crate) fn unseated_counts(scene: &SceneState) -> StateCounts {
    counts(scene.agents.values().filter(|slot| slot.desk.is_none()))
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
    crate::localclock::local_time(now).date()
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

    /// A floor counts the agents its projection draws, so one on a floor past
    /// the building's counts on none.
    #[test]
    fn a_floor_counts_the_agents_its_projection_draws() {
        let mut scene = SceneState::uniform(1);
        for (key, floor) in [("ground", 0), ("top", MAX_FLOORS - 1), ("past", MAX_FLOORS)] {
            let id = pixtuoid_core::AgentId::from_transcript_path(&format!("/tally/{key}.jsonl"));
            let mut slot = crate::character::test_support::make_slot(id, ActivityState::Idle);
            slot.desk = Some(pixtuoid_core::FloorDesk::new(floor, 0));
            scene.agents.insert(id, slot);
        }
        let counts = per_floor_counts(&scene);
        for (floor, counted) in counts.iter().enumerate() {
            let drawn = crate::floor::floor_agents(&scene, floor).count();
            assert_eq!(counted.total, drawn, "floor {floor}");
        }
    }
}
