//! The per-scene tally of agent activity and gateway liveness, computed once a
//! frame and read by the footer, the neon sign and the audio, so no two
//! surfaces count apart.

use pixtuoid_core::state::{ActivityState, DaemonPresence, DaemonState, MAX_FLOORS};
use pixtuoid_core::{AgentSlot, SceneState};

/// Per-scene tally of agent activity states, computed once per frame and shared
/// by the footer and the board so the two surfaces can't disagree. `exiting` is a
/// first-class bucket, NOT folded into idle, so the footer's `n/total` counts
/// walkouts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StateCounts {
    pub active: usize,
    pub waiting: usize,
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
        ActivityState::Active { .. } => c.active += 1,
        ActivityState::Waiting { .. } => c.waiting += 1,
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
    debug_assert_eq!(c.active + c.waiting + c.idle + c.exiting, c.total);
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
            DaemonState::Busy => 1,
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
