//! Per-agent walk state owned by each `FloorCtx` in this crate.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use crate::physics::{
    LegProgress, WalkIntent, WalkProfile, octile_distance, octile_path_len, walk_arrived,
    walk_profile,
};
use pixtuoid_core::AgentId;
use pixtuoid_core::state::AgentSlot;
use pixtuoid_core::walkable::{OccupancyOverlay, WalkableMask};

use crate::layout::{HomeDesk, Point, SceneLayout, WaypointKind};
use crate::pathfind::Router;
use crate::pose::{
    SpotClaims, WANDER_DWELL_EST_MS, dwell_ms, est_wander_cycle_ms, seated_dwell_ms,
    stale_resume_gap_ms, takes_trip,
};
use crate::pose::{desk_leg_endpoint, route_jittered};

/// Frozen A* polyline for one in-flight walk leg. Snapshotted when the leg's
/// `(from, to)` first appear and reused unchanged: per-frame occupancy churn
/// invalidates the A* cache, and mapping the frozen profile's progress `t` onto
/// a re-routed shape mid-stride makes the sprite visibly jump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WalkPathSnapshot {
    /// Leg start point.
    pub(crate) from: Point,
    /// Leg end point.
    pub(crate) to: Point,
    /// The frozen A* polyline from `from` to `to`.
    pub(crate) path: Vec<Point>,
}

/// Phase the wander cycle is currently in for a given agent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum WanderPhase {
    /// Sitting at the desk between trips.
    Seated,
    /// Walking from desk to the chosen waypoint, on this frozen out-leg profile.
    WalkingOut(WalkProfile),
    /// Standing/sitting at the waypoint during the dwell beat, holding the
    /// return-leg profile snapshotted at out-leg arrival.
    AtWaypoint(WalkProfile),
    /// Walking from the waypoint back to the desk, on the return-leg profile.
    WalkingBack(WalkProfile),
}

/// The wander frame `advance_wander` resolves, snapshotted off `WalkState` at
/// return time so the caller never re-reads (or re-borrows) `walks`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WanderFrame {
    /// The resolved phase this frame — selects the pose builder's arm.
    pub phase: WanderPhase,
    /// Physics walk progress, meaningful ONLY in `WalkingOut`/`WalkingBack`
    /// ([`LegProgress::ZERO`] otherwise).
    pub progress: LegProgress,
    /// The current trip's destination cell (the walk `to` / waypoint `dest`).
    pub dest: Point,
    /// The current trip's kind — the seat cell + waypoint identity the walk /
    /// at-waypoint arms need.
    pub kind: WanderKind,
}

/// A one-shot walk leg (entry / exit / snap-back / walk home).
#[derive(Debug, Clone)]
pub(crate) struct WalkLeg {
    /// Wall-clock instant the leg armed.
    pub(crate) started_at: SystemTime,
    /// Frozen physics profile for the leg.
    pub(crate) profile: WalkProfile,
    /// Frozen leg origin, recorded at arm-time so the leg doesn't drift.
    pub(crate) from: Point,
}

/// A resolved wander destination: the walkable target cell plus WHAT it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WanderTarget {
    /// Destination pixel of the current trip (the walkable approach/amble cell).
    pub dest: Point,
    /// Whether `dest` is a named waypoint (with optional seat) or an aimless amble.
    pub kind: WanderKind,
}

/// What KIND of wander destination [`WanderTarget::dest`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WanderKind {
    /// A named lounge waypoint. `seat = Some(S)` ⇒ the walk SETTLES from the
    /// approach point `dest` onto `S` (and rises from `S` on the way back) so
    /// arrival/departure don't pop; `seat = None` ⇒ the agent stands AT `dest`.
    Named {
        /// Index into `layout.waypoints`.
        wp_idx: usize,
        /// Kind of the target waypoint.
        kind: WaypointKind,
        /// Seat foot cell to settle onto; `None` for a stand-at obstacle.
        seat: Option<Point>,
    },
    /// An aimless amble to a random walkable point (no named waypoint, no seat).
    Aimless,
}

impl WanderKind {
    pub(crate) fn seat(self) -> Option<Point> {
        match self {
            WanderKind::Named { seat, .. } => seat,
            WanderKind::Aimless => None,
        }
    }

    /// Shared by the pure `idle_pose` overlay and the routed
    /// `derive_with_routing` so the two can't drift.
    pub(crate) fn at_pose(self, dest: Point) -> crate::pose::Pose {
        match self {
            WanderKind::Named { wp_idx, kind, .. } => {
                crate::pose::Pose::AtWaypoint { wp: wp_idx, kind }
            }
            WanderKind::Aimless => crate::pose::Pose::AimlessAt { dest },
        }
    }

    pub(crate) fn carries_coffee(self) -> bool {
        matches!(
            self,
            WanderKind::Named {
                kind: WaypointKind::Pantry,
                ..
            }
        )
    }
}

/// The elastic cyclic-wander timeline state machine for one agent (desk →
/// waypoint → desk, repeating). `advance_wander` transitions the fields as a
/// unit.
#[derive(Debug, Clone)]
pub struct WanderState {
    /// Wander cycle counter, incremented each time `WalkingBack` completes —
    /// selects the waypoint destination (mirrors `pose::pure`'s derivation).
    pub(crate) cycle_n: u64,
    /// Current phase of the wander cycle.
    pub(crate) phase: WanderPhase,
    /// Wall-clock instant the current phase began, reset every transition.
    /// Sentinel `UNIX_EPOCH` ⇒ a fresh agent `advance_wander` bootstraps.
    pub(crate) phase_started_at: SystemTime,
    /// The current trip's resolved destination. Set on each new `WalkingOut`;
    /// its `kind` resets to `Aimless` when a cycle completes.
    pub(crate) target: WanderTarget,
    /// Last `now` at which `advance_wander` performed a transition — idempotency:
    /// `now <= last_advanced_at` ⇒ a no-op on mutable state. Sentinel
    /// `UNIX_EPOCH` ⇒ never advanced.
    pub(crate) last_advanced_at: SystemTime,
}

/// Walk state for one live agent on one floor.
#[derive(Debug, Clone)]
pub struct WalkState {
    /// The arrival walk, snapshotted once at door-crossing, or a pointer's
    /// walk home ([`Lifted::Home`]) once that arrives. Carries its own `from`
    /// because a resurrect that cancels an IN-FLIGHT walkout re-enters from
    /// wherever the sprite is; a hardcoded door origin teleports it.
    pub(crate) entry: Option<WalkLeg>,
    /// The walkout, snapshotted once when `exiting_at` fires. `from` is where
    /// the sprite actually is (where a pointer's hold lands it, its wander
    /// position if it was out, else the desk anchor), so the exit doesn't
    /// teleport to the desk.
    pub(crate) exit: Option<WalkLeg>,
    /// The state-transition snap-back walk. `from` is the FROZEN origin recorded
    /// when the leg armed, reused every frame so the walk doesn't drift toward
    /// the desk (mirrors `exit`).
    pub(crate) snap_back: Option<WalkLeg>,

    /// The elastic cyclic-wander timeline state machine. See [`WanderState`].
    pub(crate) wander: WanderState,

    /// Frozen A* polyline for the current walk leg, `None` while not walking.
    /// Re-snapshotted when the leg's `(from, to)` change. See
    /// [`WalkPathSnapshot`].
    pub(crate) walk_path: Option<WalkPathSnapshot>,

    /// Lifted by a pointer, and walking home after: overrides the wander and
    /// the state's pose until it arrives.
    pub(crate) lifted: Option<Lifted>,
}

/// An agent a pointer lifted.
#[derive(Debug, Clone)]
pub(crate) enum Lifted {
    /// In hand, its feet at this point.
    Held(Point),
    /// Set down here, until the next derivation arms the walk home from it.
    Dropped(Point),
    /// The walk home.
    Home(WalkLeg),
}

impl Default for WalkState {
    /// A fresh `WalkState`. Both wander instants are `UNIX_EPOCH` so
    /// `advance_wander` detects a bootstrap agent via the sentinel.
    fn default() -> Self {
        Self {
            entry: None,
            exit: None,
            snap_back: None,
            wander: WanderState {
                cycle_n: 0,
                phase: WanderPhase::Seated,
                phase_started_at: SystemTime::UNIX_EPOCH,
                // Placeholder — replaced on first WalkingOut transition.
                target: WanderTarget {
                    dest: Point { x: 0, y: 0 },
                    kind: WanderKind::Aimless,
                },
                last_advanced_at: SystemTime::UNIX_EPOCH,
            },
            walk_path: None,
            lifted: None,
        }
    }
}

impl WalkState {
    /// The agent's wander timeline.
    #[doc(hidden)]
    pub fn wander(&self) -> &WanderState {
        &self.wander
    }

    /// Whether a pointer holds it.
    pub(crate) fn carried(&self) -> bool {
        matches!(self.lifted, Some(Lifted::Held(_)))
    }

    /// In a pointer's hand, its feet at `at`.
    pub(crate) fn carry(&mut self, at: Point) {
        self.lifted = Some(Lifted::Held(at));
    }

    /// Set down at `at`, to walk home from there.
    pub(crate) fn set_down(&mut self, at: Point) {
        self.lifted = Some(Lifted::Dropped(at));
    }
}

impl WanderState {
    /// When the current phase began.
    #[doc(hidden)]
    pub fn phase_started_at(&self) -> SystemTime {
        self.phase_started_at
    }

    /// The last `now` at which the timeline transitioned.
    #[doc(hidden)]
    pub fn last_advanced_at(&self) -> SystemTime {
        self.last_advanced_at
    }
}

/// Advance the wander state machine by one frame for the given idle agent.
///
/// Transitions run ONLY when `now > wander.last_advanced_at`; otherwise the pose
/// is computed from the existing phase WITHOUT mutating any wander field, so a
/// repeat call at the same `now` changes nothing.
///
/// On the first call for a fresh Idle slot, `cycle_n` is fast-forwarded so
/// destination selection agrees with what the stateless `pose::pure::idle_pose` would
/// have derived for an agent that was Idle before the first render.
pub(crate) fn advance_wander(
    slot: &AgentSlot,
    now: SystemTime,
    layout: &SceneLayout,
    desk: HomeDesk,
    router: &mut dyn Router,
    overlay: &OccupancyOverlay,
    walks: &mut HashMap<AgentId, WalkState>,
) -> WanderFrame {
    let id = slot.agent_id;
    // Claims must be snapshotted BEFORE this agent's `&mut` — the two borrows of
    // `walks` can't overlap.
    let claimed = spot_claims(walks, id);
    let walk = walks.entry(id).or_default();

    // A fresh WalkState's epoch `phase_started_at` is below any real
    // `state_started_at`; we also re-seed when the slot (re-)entered Idle after a
    // different state.
    let is_fresh = walk.wander.phase_started_at + Duration::from_millis(1) <= slot.state_started_at;

    // Stale resume: advanced before, but more than a full wander cycle ago — its
    // floor was off-screen (only the current floor renders) or `now` was frozen
    // (pause). Treat it as fresh so the bootstrap below snaps it to the right
    // cycle analytically instead of the phase machine replaying the backlog one
    // transition per frame (the visible "fast-forward all the movement" bug).
    // Don't raise the trigger to "max dwell": on-screen, `advance_wander` runs
    // every frame even DURING a 40 s lounge dwell, so only an off-screen floor or
    // a pause can exceed it. `unwrap_or(false)` treats a backward clock step as
    // "not stale" rather than snapping every agent to Seated.
    let is_stale_resume = walk.wander.last_advanced_at != SystemTime::UNIX_EPOCH
        && now
            .duration_since(walk.wander.last_advanced_at)
            .map(|d| d.as_millis() as u64 > stale_resume_gap_ms(id))
            .unwrap_or(false);

    if is_fresh || is_stale_resume {
        let elapsed_idle = crate::anim::elapsed_ms(now, slot.state_started_at);
        // The estimated full cycle (matches idle_pose), NOT stale_resume_gap_ms.
        let cycle = est_wander_cycle_ms(id);

        // Fast-forward `cycle_n`, but ALWAYS (re)start the phase clock cleanly in
        // Seated at `now`. Anchoring mid-cycle (`now - partial_ms`) made the phase
        // machine rush through the partial cycle's already-expired legs one
        // transition per frame — a desk↔waypoint teleport.
        walk.wander.phase = WanderPhase::Seated;
        walk.wander.cycle_n = elapsed_idle / cycle;
        walk.wander.phase_started_at = now;
    }

    let may_transition = now > walk.wander.last_advanced_at;

    let elapsed_phase = crate::anim::elapsed_ms(now, walk.wander.phase_started_at);

    let seated_dur = seated_dwell_ms(id);
    let dwell_dur = match walk.wander.target.kind {
        WanderKind::Named { kind, .. } => dwell_ms(kind, id),
        WanderKind::Aimless => WANDER_DWELL_EST_MS,
    };

    let result = match walk.wander.phase {
        WanderPhase::Seated => {
            if may_transition && elapsed_phase >= seated_dur {
                if !takes_trip(id, walk.wander.cycle_n) || layout.waypoints.is_empty() {
                    walk.wander.cycle_n += 1;
                    walk.wander.phase_started_at += Duration::from_millis(seated_dur);
                } else {
                    // The origin must match pose::pure::idle_pose's `desk` so the
                    // stateless/stateful destinations stay in lockstep.
                    let target = pick_wander_dest(id, walk.wander.cycle_n, layout, desk, &claimed);
                    walk.wander.target = target;
                    let dest = target.dest;
                    let seat = target.kind.seat();

                    let (from, chair_settle) = desk_leg_endpoint(desk, layout);
                    walk.wander.phase = WanderPhase::WalkingOut(snapshot_leg_profile(
                        router,
                        &layout.walkable,
                        overlay,
                        id,
                        LegPlan {
                            from,
                            to: dest,
                            settle: Settle::from_pair(chair_settle, seat),
                            intent: WalkIntent::WanderOut,
                        },
                    ));
                    walk.wander.phase_started_at += Duration::from_millis(seated_dur);
                }
            }
            (walk.wander.phase, LegProgress::ZERO)
        }

        WanderPhase::WalkingOut(profile) => {
            match poll_walk_leg(&profile, elapsed_phase, may_transition) {
                WalkLegStatus::InFlight(t) => (WanderPhase::WalkingOut(profile), t),
                WalkLegStatus::Arrived { walk_total } => {
                    // Snapshot the walk-back profile now — the overlay may differ
                    // by the time the dwell ends.
                    let back = snapshot_back_profile(slot, walk, layout, desk, router, overlay);
                    walk.wander.phase = WanderPhase::AtWaypoint(back);
                    advance_phase_clock(walk, walk_total);
                    (WanderPhase::AtWaypoint(back), LegProgress::ZERO)
                }
            }
        }

        WanderPhase::AtWaypoint(back) => {
            if may_transition && elapsed_phase >= dwell_dur {
                walk.wander.phase = WanderPhase::WalkingBack(back);
                walk.wander.phase_started_at += Duration::from_millis(dwell_dur);
            }
            (walk.wander.phase, LegProgress::ZERO)
        }

        WanderPhase::WalkingBack(profile) => {
            match poll_walk_leg(&profile, elapsed_phase, may_transition) {
                WalkLegStatus::InFlight(t) => (WanderPhase::WalkingBack(profile), t),
                WalkLegStatus::Arrived { walk_total } => {
                    // `target.dest` is deliberately left as-is: the Seated arm
                    // never reads it and the next WalkingOut overwrites it.
                    walk.wander.cycle_n += 1;
                    walk.wander.target.kind = WanderKind::Aimless;
                    walk.wander.phase = WanderPhase::Seated;
                    advance_phase_clock(walk, walk_total);
                    (WanderPhase::Seated, LegProgress::ZERO)
                }
            }
        }
    };

    if may_transition {
        walk.wander.last_advanced_at = now;
    }

    // `result.0` == `walk.wander.phase` in every arm, so the frame's
    // `dest`/`kind` always describe the returned phase.
    let (phase, progress) = result;
    WanderFrame {
        phase,
        progress,
        dest: walk.wander.target.dest,
        kind: walk.wander.target.kind,
    }
}

enum WalkLegStatus {
    /// Still walking: the physics progress.
    InFlight(LegProgress),
    /// The walk and its pause have completed; `walk_total` = `duration_ms +
    /// pause_ms`.
    Arrived { walk_total: u64 },
}

/// The progress/arrival check the `WalkingOut` and `WalkingBack` arms share;
/// each arm runs its OWN divergent on-arrival cleanup.
fn poll_walk_leg(profile: &WalkProfile, elapsed_phase: u64, may_transition: bool) -> WalkLegStatus {
    let progress = crate::physics::walk_progress(profile, elapsed_phase);
    if may_transition && walk_arrived(profile, elapsed_phase) {
        WalkLegStatus::Arrived {
            walk_total: profile.duration_ms + profile.pause_ms,
        }
    } else {
        WalkLegStatus::InFlight(progress)
    }
}

/// Advance the phase clock from its CURRENT anchor — not from `now` — so the
/// next phase starts exactly when this one's wall-time budget elapsed.
fn advance_phase_clock(walk: &mut WalkState, walk_total: u64) {
    walk.wander.phase_started_at += Duration::from_millis(walk_total);
}

/// Delegates to the ONE stateless resolver `crate::pose::resolve_wander_target`,
/// which `idle_pose` also calls, so the routed walk path and the stateless
/// overlay can never drift to different destinations. `origin` is the agent's
/// home desk, kept identical to `idle_pose`'s `desk`.
fn pick_wander_dest(
    id: AgentId,
    cycle_n: u64,
    layout: &SceneLayout,
    origin: HomeDesk,
    claimed: &SpotClaims,
) -> WanderTarget {
    crate::pose::resolve_wander_target(id, cycle_n, layout, origin, claimed)
}

/// The exclusive-spot waypoints every OTHER agent on this floor is out on a trip
/// to — the exclusion set that keeps a single-occupancy spot to one occupant.
/// Read from the live wander targets, so it must be built BEFORE the caller
/// takes its own `&mut WalkState`.
///
/// Gating on **phase ≠ Seated** (not on the kind) is the honest "is this agent
/// actually out" signal: the bootstrap / stale-resume path re-seats an agent
/// WITHOUT touching its target. Gating on **`exclusive`** reuses the one
/// authority for "single-occupancy destination", so a future exclusive kind
/// inherits it; shareable waypoints are NOT claimed, since the painter's rank
/// offset is a genuine step-aside queue there.
fn spot_claims(walks: &HashMap<AgentId, WalkState>, exclude: AgentId) -> SpotClaims {
    let mut claims = SpotClaims::default();
    for (id, walk) in walks {
        if *id == exclude || matches!(walk.wander.phase, WanderPhase::Seated) {
            continue;
        }
        if let WanderKind::Named { wp_idx, kind, .. } = walk.wander.target.kind
            && crate::layout::furniture_def(kind.furniture()).exclusive
        {
            claims.claim(wp_idx);
        }
    }
    claims
}

/// How a walk leg extends its polyline onto a seat — a short terminal motion the
/// A* router never plans (the seat cell may be blocked). `End` = sit down on
/// arrival (append the seat); `Start` = stand up on departure (prepend it).
/// The profile snapshot and the render route share it, so walk-end ≡
/// render-feet and seat arrival/departure don't pop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settle {
    None,
    End(Point),
    Start(Point),
    Both { start: Point, end: Point },
}

impl Settle {
    /// Argument order encodes direction: `start` = the seat to rise OFF
    /// (prepended), `end` = the seat to glide ONTO (appended).
    pub(crate) fn from_pair(start: Option<Point>, end: Option<Point>) -> Self {
        match (start, end) {
            (Some(start), Some(end)) => Self::Both { start, end },
            (Some(start), None) => Self::Start(start),
            (None, Some(end)) => Self::End(end),
            (None, None) => Self::None,
        }
    }

    fn start(self) -> Option<Point> {
        match self {
            Self::Start(start) | Self::Both { start, .. } => Some(start),
            Self::None | Self::End(_) => None,
        }
    }

    fn end(self) -> Option<Point> {
        match self {
            Self::End(end) | Self::Both { end, .. } => Some(end),
            Self::None | Self::Start(_) => None,
        }
    }
}

/// One walk leg to freeze a profile for.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LegPlan {
    pub(crate) from: Point,
    pub(crate) to: Point,
    pub(crate) settle: Settle,
    /// Why the agent walks this leg — picks the gait.
    pub(crate) intent: WalkIntent,
}

/// Freeze one walk leg's timing profile. Measuring the ROUTED (not raw)
/// polyline is load-bearing: the duration must cover the whole path or `t`
/// reaches [`LegProgress::DONE`] before the sprite
/// arrives and it pops.
pub(crate) fn snapshot_leg_profile(
    router: &mut dyn Router,
    mask: &WalkableMask,
    overlay: &OccupancyOverlay,
    id: AgentId,
    leg: LegPlan,
) -> WalkProfile {
    let path = route_jittered(router, mask, overlay, id, leg.from, leg.to);
    walk_profile(measured_leg_len(&path, leg.settle), leg.intent, id)
}

/// Freeze the WanderBack profile. The endpoint is the desk APPROACH cell
/// (matching `seated_top_left` via the chair-glide) so there's no jump on arrival;
/// this intentionally differs from `pose::pure::idle_pose`'s raw `to: desk`, since
/// only the routed path is user-visible.
fn snapshot_back_profile(
    slot: &AgentSlot,
    walk: &WalkState,
    layout: &SceneLayout,
    desk: HomeDesk,
    router: &mut dyn Router,
    overlay: &OccupancyOverlay,
) -> WalkProfile {
    let (snap_to, chair_settle) = desk_leg_endpoint(desk, layout);
    snapshot_leg_profile(
        router,
        &layout.walkable,
        overlay,
        slot.agent_id,
        LegPlan {
            from: walk.wander.target.dest,
            to: snap_to,
            settle: Settle::from_pair(walk.wander.target.kind.seat(), chair_settle),
            intent: WalkIntent::WanderBack,
        },
    )
}

/// Octile length of the settle segment `approach → seat`, or 0 when there is no
/// seat, so a leg's DURATION covers the sit-down/stand-up settle too.
pub(crate) fn settle_len(approach: Point, seat: Option<Point>) -> u32 {
    seat.map_or(0, |s| octile_distance(approach, s))
}

/// Rendered-polyline length of a walk leg: the routed polyline plus its
/// [`Settle`] segments, floored at 1. The profile's DURATION is derived from
/// this so it covers the FULL rendered leg.
pub(crate) fn measured_leg_len(route: &[Point], settle: Settle) -> u32 {
    let start = route.first().map_or(0, |&p| settle_len(p, settle.start()));
    let end = route.last().map_or(0, |&p| settle_len(p, settle.end()));
    (octile_path_len(route) + start + end).max(1)
}

#[cfg(test)]
mod tests;
