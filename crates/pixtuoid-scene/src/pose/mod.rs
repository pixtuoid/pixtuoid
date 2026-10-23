//! Pose layer: re-exports the stateless state → pose derivation from the `pure`
//! sibling and adds the routed machinery — `PoseHistory` plus
//! `derive_with_routing`, which consults a `&mut dyn Router` so walking poses
//! follow A*-routed polylines and state transitions are smoothed with a
//! snap-back walk instead of teleporting back to the desk.

mod pure;

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::state::AgentSlot;

use crate::physics::{Segment, WalkIntent, WalkProfile, walk_arrived, walk_progress};
use crate::walk::{
    LegPlan, Lifted, Settle, WalkLeg, WalkPathSnapshot, WalkState, WanderKind, WanderPhase,
    advance_wander, snapshot_leg_profile,
};
use pixtuoid_core::walkable::{OccupancyOverlay, WalkableMask};

pub(crate) use pure::{
    ENTRY_ANIMATION_MS, STALE_RESUME_GAP_BASE_MS, WANDER_DWELL_EST_MS, derive, derive_state_only,
    dwell_ms, stale_resume_gap_ms,
};
pub(crate) use pure::{LegPose, Pose};
#[cfg(test)]
pub(crate) use pure::{aimless_wander_seed, pick_aimless_dest};
pub use pure::{
    est_wander_cycle_ms, is_aimless_cycle, seated_dwell_ms, takes_trip, waypoint_index_for_cycle,
};
// These stay crate-internal: a `pub use` would try to widen their `pub(crate)`
// visibility.
pub(crate) use pure::{SpotClaims, resolve_wander_target, typing_frame, walk_frame};

use crate::layout::{Point, SceneLayout, desk_walk_anchor_facing};
use crate::pathfind::Router;

/// The per-frame routing engine state threaded through pose derivation,
/// character anchoring, hit-testing and label placement. `now`/`layout` stay
/// separate args — frame inputs, not engine state.
#[derive(Debug)]
pub(crate) struct RouteCtx<'a> {
    /// The A* router for this frame.
    pub router: &'a mut dyn Router,
    /// Live occupancy overlay (shared, read-only here).
    pub overlay: &'a OccupancyOverlay,
    /// Per-agent rendered-position cache.
    pub history: &'a mut PoseHistory,
    /// Per-agent walk state, keyed by `AgentId`.
    pub walks: &'a mut HashMap<AgentId, WalkState>,
    /// Whether an idle agent wanders off its desk: an ambient loop, so not at
    /// [`Motion::Still`](crate::anim::Motion::Still).
    pub wanders: bool,
}

/// Owns the stores a [`RouteCtx`] borrows, so a test threads one value.
#[cfg(test)]
pub(crate) struct RouteRig<R> {
    pub(crate) router: R,
    pub(crate) overlay: OccupancyOverlay,
    pub(crate) history: PoseHistory,
    pub(crate) walks: HashMap<AgentId, WalkState>,
}

#[cfg(test)]
impl<R: Router> RouteRig<R> {
    pub(crate) fn new(router: R) -> Self {
        Self {
            router,
            overlay: OccupancyOverlay::new(),
            history: PoseHistory::new(),
            walks: HashMap::new(),
        }
    }

    pub(crate) fn rctx(&mut self) -> RouteCtx<'_> {
        RouteCtx {
            router: &mut self.router,
            overlay: &self.overlay,
            history: &mut self.history,
            walks: &mut self.walks,
            wanders: true,
        }
    }
}

/// Per-agent rendered position cache, consulted on state transitions so an agent
/// who was mid-walk when their state flipped can complete the walk visually
/// instead of teleporting back to their desk.
#[derive(Debug, Default, Clone)]
pub struct PoseHistory {
    last: std::collections::HashMap<AgentId, (Point, SystemTime)>,
}

impl PoseHistory {
    /// A new, empty history.
    pub fn new() -> Self {
        Self::default()
    }
    /// Record where an agent was visually placed this frame.
    pub(crate) fn record(&mut self, agent_id: AgentId, anchor: Point, now: SystemTime) {
        self.last.insert(agent_id, (anchor, now));
    }
    /// Drop entries for agents no longer in `scene`. Without this, every AgentId
    /// ever rendered leaks an entry for the process lifetime, per floor.
    pub(crate) fn evict_missing(&mut self, scene: &pixtuoid_core::state::SceneState) {
        self.last.retain(|id, _| scene.agents.contains_key(id));
    }
    /// Whether an entry exists for `agent_id`.
    pub fn contains(&self, agent_id: AgentId) -> bool {
        self.last.contains_key(&agent_id)
    }
    /// Latest recorded position if it's at most `max_age_ms` old.
    pub fn recent(&self, agent_id: AgentId, max_age_ms: u64, now: SystemTime) -> Option<Point> {
        let (pt, when) = self.last.get(&agent_id).copied()?;
        let age = now.duration_since(when).ok()?.as_millis() as u64;
        if age <= max_age_ms { Some(pt) } else { None }
    }
}

/// Snap-back ARM window (ms): only trigger a snap-back walk if the desk-bound
/// state flip happened within this long. NOT a render cap — the armed walk runs
/// to completion by physics (`walk_arrived`).
const SNAP_BACK_MS: u64 = 900;
/// Minimum manhattan distance (px) from the current rendered position to the
/// desk before animating the snap-back; below this the teleport is invisible.
const SNAP_BACK_MIN_DIST: i32 = 8;
/// Max age (ms) for a recorded `PoseHistory` position to count as "where the
/// agent is now".
const HISTORY_RECENT_MS: u64 = 300;
/// Safety margin (ms) shaved off `EXIT_GRACE_WINDOW` so the time-compressed exit
/// walk reaches the door before the reducer GCs the slot. Equal to
/// `HISTORY_RECENT_MS` by coincidence, not by motivation — don't merge them.
const EXIT_BUDGET_MARGIN_MS: u64 = 300;

/// The home desk's ARRIVAL target: a reachable cell on an ALLOWED side
/// (`DESK_APPROACH` = N/E/W, excluding the south front), so an arriving agent
/// walks AROUND to sit behind the desk instead of straight through its front.
/// The chair itself is blocked, so targeting it directly made A\* fall back to a
/// straight `door→chair` line THROUGH the desk body.
///
/// Scans from the CHAIR, not the desk's top-left origin: the footprint is
/// anchored top-left, so a corner scan is lopsided and can't clear the body to
/// the EAST — the east side would read as walled-off. `None` only in a
/// degenerate layout where every allowed side is walled off.
pub(crate) fn desk_approach_cell(desk: Point, layout: &SceneLayout) -> Option<Point> {
    use crate::layout::{Furniture, desk_walk_anchor_facing};
    // The desk's OWN facing, not a constant: `ApproachSides` is canonical (facing-South) and
    // rotated by it, so a back-turned desk is approached from its south front, not walled off there.
    let facing = layout.desk_facing_at(desk);
    let chair = desk_walk_anchor_facing(desk, facing);
    let cell = layout.approach_point(Furniture::Desk, chair, chair, facing);
    // `approach_point` returns the scanned pos (== chair) as its "no approach"
    // sentinel when no allowed+reachable side exists.
    (cell != chair).then_some(cell)
}

/// The desk-side endpoint of a desk-bound walk leg, resolved the ONE unified way
/// so no leg can regress to aiming A\* at the blocked chair: aiming there makes
/// `find_path` snap to the NEAREST walkable cell — the south front for a
/// south-facing chair — so the agent arrives through the desk front.
///
/// Returns `(routing_endpoint, chair_settle)`: the reachable N/E/W cell to hand
/// A\*, plus `Some(chair)` to prepend/append via [`Settle`] (the short glide
/// on/off the seat the router never plans), or `None` in the degenerate boxed-in
/// layout where the leg reverts to the direct chair target.
pub(crate) fn desk_leg_endpoint(desk: Point, layout: &SceneLayout) -> (Point, Option<Point>) {
    let chair = crate::layout::desk_walk_anchor_facing(desk, layout.desk_facing_at(desk));
    match desk_approach_cell(desk, layout) {
        Some(approach) => (approach, Some(chair)),
        None => (chair, None),
    }
}
/// Where a cancelled walkout must re-enter FROM, or `None` when there was no
/// walkout to cancel.
enum ReEnter {
    /// The walkout ARRIVED — the sprite is off-floor, so the door is the only
    /// honest origin.
    Door,
    Live(Point),
}

/// Take a spent EXIT leg off the non-exiting path and say where entry re-arms.
///
/// An exit leg surviving onto this path means `exiting_at` was cleared under us
/// without re-stamping `created_at`, so the entry branch's spawn-window gate can
/// never re-arm on its own and the agent would pop onto its chair. The take is
/// load-bearing on its own: a retained arrived leg is replayed by the NEXT exit,
/// vanishing the sprite on its first frame instead of walking out.
fn take_cancelled_walkout(
    walk: Option<&mut WalkState>,
    now: SystemTime,
    live: Option<Point>,
) -> Option<ReEnter> {
    let walk = walk?;
    let leg = walk.exit.take()?;
    walk.entry = None;
    let elapsed = crate::anim::elapsed_ms(now, leg.started_at);
    Some(
        if walk_arrived(&leg.profile, exit_elapsed_ms(&leg.profile, elapsed)) {
            ReEnter::Door
        } else {
            live.map_or(ReEnter::Door, ReEnter::Live)
        },
    )
}

/// Time-compressed elapsed for an EXIT leg, so the walk REACHES the door before
/// the reducer's `EXIT_GRACE_WINDOW` reaps the slot; without it the slot is GC'd
/// mid-walk and the sprite vanishes in the corridor. (Entry has no such cap —
/// nothing GCs an entering agent.)
///
/// `floor::recompute_door_anim_max_ms` asks "has the walkout finished?" too and
/// deliberately stays UNCOMPRESSED: it wants the physics window for a door
/// cosmetic, not the render's deadline.
fn exit_elapsed_ms(profile: &WalkProfile, elapsed_ms: u64) -> u64 {
    let budget = (pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW.as_millis() as u64)
        .saturating_sub(EXIT_BUDGET_MARGIN_MS);
    if profile.duration_ms.saturating_add(profile.pause_ms) > budget {
        (elapsed_ms.saturating_mul(profile.duration_ms) / budget.max(1)).max(elapsed_ms)
    } else {
        elapsed_ms
    }
}

/// Routed variant of `derive`: Walking poses trace an A*-routed polyline
/// (layout mask + per-frame [`RouteCtx::overlay`]) corner-by-corner instead of
/// cutting through obstacles or other agents.
///
/// [`RouteCtx::walks`] drives entry/exit physics — the A* path length is
/// snapshotted into a `WalkProfile` on first sighting (commit-to-route), and
/// later frames compute progress against that frozen profile.
/// [`RouteCtx::history`] is consulted on state transitions so an agent whose
/// pose flipped mid-wander walks back to the desk instead of teleporting.
pub(crate) fn derive_with_routing(
    slot: &AgentSlot,
    now: SystemTime,
    layout: &SceneLayout,
    rctx: &mut RouteCtx<'_>,
) -> Option<Pose> {
    let desk = layout.home_desk(slot.desk_index.single_floor_local())?;

    if let Some(exit_time) = slot.exiting_at {
        let door_target = layout.door_threshold;

        let walk = rctx.walks.entry(slot.agent_id).or_default();

        if walk.exit.is_none() {
            // From wherever the agent actually is — otherwise one mid-coffee-run at
            // session end teleports to the desk before walking to the door.
            let desk_anchor = desk_walk_anchor_facing(desk, layout.desk_facing_at(desk));
            let from = match walk.lifted.take() {
                Some(Lifted::Held(at) | Lifted::Dropped(at)) => {
                    landing(at, desk_leg_endpoint(desk, layout).0, layout)
                }
                Some(Lifted::Home(_)) | None => rctx
                    .history
                    .recent(slot.agent_id, HISTORY_RECENT_MS, now)
                    .unwrap_or(desk_anchor),
            };
            let (route_from, chair_rise) = if from == desk_anchor {
                desk_leg_endpoint(desk, layout)
            } else {
                (from, None)
            };
            let profile = snapshot_leg_profile(
                rctx.router,
                &layout.walkable,
                rctx.overlay,
                slot.agent_id,
                LegPlan {
                    from: route_from,
                    to: door_target,
                    settle: chair_rise.map_or(Settle::None, Settle::Start),
                    intent: WalkIntent::Exit,
                },
            );
            // Store the ORIGIN so the render can detect a desk departure and
            // re-derive the same approach + settle.
            walk.exit = Some(WalkLeg {
                started_at: exit_time,
                profile,
                from,
            });
        }

        let e = walk.exit.as_ref()?;
        let started_at = e.started_at;
        let profile = &e.profile;
        let stored_from = e.from;

        let elapsed_ms = crate::anim::elapsed_ms(now, started_at);

        let eff_elapsed = exit_elapsed_ms(profile, elapsed_ms);

        if walk_arrived(profile, eff_elapsed) {
            return None;
        }

        let progress = walk_progress(profile, eff_elapsed);

        // Must reproduce the snapshotted profile's endpoints, or the leg's
        // duration no longer matches the distance it renders.
        let (from, exit_settle) =
            if stored_from == desk_walk_anchor_facing(desk, layout.desk_facing_at(desk)) {
                let (approach, chair) = desk_leg_endpoint(desk, layout);
                (approach, chair.map_or(Settle::None, Settle::Start))
            } else {
                (stored_from, Settle::None)
            };

        return Some(route_walking_pose(
            slot,
            now,
            layout,
            rctx,
            LegPose {
                from,
                to: door_target,
                progress,
                carrying_coffee: false,
            },
            exit_settle,
        ));
    }

    let live_now = rctx.history.recent(slot.agent_id, HISTORY_RECENT_MS, now);
    let re_enter = take_cancelled_walkout(rctx.walks.get_mut(&slot.agent_id), now, live_now);

    // A pointer's hold, and the walk home after it, outrank all but the exit,
    // and stand in for the re-entry of a walkout they cancel.
    if let Some(pose) = lifted_pose(slot, now, layout, desk, rctx) {
        return Some(pose);
    }

    // ENTRY_ANIMATION_MS bounds only how long we try to ROUTE; the physics
    // duration is the real walk time.
    let since_spawn = now
        .duration_since(slot.created_at)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64;

    let door = layout.door_threshold;
    let (approach, chair_settle) = desk_leg_endpoint(desk, layout);
    let settle = chair_settle.map_or(Settle::None, Settle::End);

    let walk = rctx.walks.entry(slot.agent_id).or_default();

    let entry_from = match re_enter {
        Some(ReEnter::Live(p)) => p,
        _ => door,
    };
    if walk.entry.is_none() && (since_spawn < ENTRY_ANIMATION_MS || re_enter.is_some()) {
        let profile = snapshot_leg_profile(
            rctx.router,
            &layout.walkable,
            rctx.overlay,
            slot.agent_id,
            LegPlan {
                from: entry_from,
                to: approach,
                settle: chair_settle.map_or(Settle::None, Settle::End),
                intent: WalkIntent::Entry,
            },
        );
        walk.entry = Some(WalkLeg {
            started_at: if re_enter.is_some() {
                now
            } else {
                slot.created_at
            },
            profile,
            from: entry_from,
        });
    }

    if let Some(WalkLeg {
        started_at,
        profile,
        from,
    }) = walk.entry
    {
        let elapsed_ms = crate::anim::elapsed_ms(now, started_at);

        if !walk_arrived(&profile, elapsed_ms) {
            let progress = walk_progress(&profile, elapsed_ms);
            return Some(route_walking_pose(
                slot,
                now,
                layout,
                rctx,
                LegPose {
                    from,
                    to: approach,
                    progress,
                    carrying_coffee: false,
                },
                settle,
            ));
        }
        // DO NOT call `derive()` here — it re-fires the linear entry override
        // and causes a double-walk. Fall through to the state-driven pose.
    }

    // Gates on Idle, NOT `since_spawn >= ENTRY_ANIMATION_MS`: that fixed gate sat a
    // near-desk agent in `idle_pose` for seconds, then snapped it to physics wander.
    let is_idle = matches!(slot.state, pixtuoid_core::state::ActivityState::Idle);
    if is_idle && slot.exiting_at.is_none() {
        // Before `advance_wander`, so the routed render and the pure overlay share
        // the ONE `pure::in_thinking_window` gate and can't drift.
        if pure::in_thinking_window(slot, now) {
            return Some(Pose::SeatedThinking);
        }
        if !rctx.wanders {
            return Some(Pose::SeatedIdle);
        }

        // A per-frame snapshot, so the arms below never re-borrow `rctx.walks`.
        let wf = advance_wander(
            slot,
            now,
            layout,
            desk,
            rctx.router,
            rctx.overlay,
            rctx.walks,
        );

        match wf.phase {
            WanderPhase::WalkingOut(_) => {
                let dest = wf.dest;
                let seat = wf.kind.seat();
                let (from, chair_settle) = desk_leg_endpoint(desk, layout);
                let settle = Settle::from_pair(chair_settle, seat);
                return Some(route_walking_pose(
                    slot,
                    now,
                    layout,
                    rctx,
                    LegPose {
                        from,
                        to: dest,
                        progress: wf.progress,
                        carrying_coffee: false,
                    },
                    settle,
                ));
            }
            WanderPhase::AtWaypoint(_) => {
                let pose = wf.kind.at_pose(wf.dest);
                // The RENDERED position, which snap-back/exit read as their origin:
                // the seat cell for a seat waypoint, else `dest`. Recording `dest`
                // popped the sprite to the off-side approach cell on frame one.
                let pt = wf.kind.seat().unwrap_or(wf.dest);
                rctx.history.record(slot.agent_id, pt, now);
                return Some(pose);
            }
            WanderPhase::WalkingBack(_) => {
                let carrying_coffee = wf.kind.carries_coffee();
                let seat = wf.kind.seat();
                let (snap_target, chair_settle) = desk_leg_endpoint(desk, layout);
                let settle = Settle::from_pair(seat, chair_settle);
                return Some(route_walking_pose(
                    slot,
                    now,
                    layout,
                    rctx,
                    LegPose {
                        from: wf.dest,
                        to: snap_target,
                        progress: wf.progress,
                        carrying_coffee,
                    },
                    settle,
                ));
            }
            WanderPhase::Seated => {
                // DIRECTLY, not via `derive_state_only`: that re-runs the STATELESS
                // `idle_pose`, whose independent timeline returns AtWaypoint at an
                // unrelated location and teleports the sprite as the clocks drift.
                return Some(Pose::SeatedIdle);
            }
        }
    }

    // Went Active/Waiting: drop any exclusive-spot claim, else the frozen
    // `wander.target` blocks that spot for the whole burst.
    if let Some(walk) = rctx.walks.get_mut(&slot.agent_id) {
        walk.wander.target.kind = WanderKind::Aimless;
    }

    // derive_state_only, NOT derive: derive() re-triggers the linear entry/exit
    // overrides and produces a double-walk.
    let raw = derive_state_only(slot, now, layout)?;

    let since_state = crate::anim::elapsed_ms(now, slot.state_started_at);
    // A desk-bound state pose would teleport an agent who was mid-wander when the
    // state changed, so walk it from the previous rendered position instead.
    let snap = {
        let walk = rctx.walks.entry(slot.agent_id).or_default();
        // ARM ONCE per transition: `route_walking_pose` records the advancing walker
        // into history every call, so re-checking the gate on a second `derive` this
        // frame sees a CLOSER `prev` and drops the agent to Seated mid-walk. Keyed on
        // `state_started_at`, not `now`, so a new transition re-arms with a fresh clock.
        let already_armed =
            matches!(&walk.snap_back, Some(leg) if leg.started_at == slot.state_started_at);
        if !already_armed {
            walk.snap_back = None;
            if since_state < SNAP_BACK_MS
                && let Some(prev) = rctx.history.recent(slot.agent_id, HISTORY_RECENT_MS, now)
            {
                // To the CHAIR, not the desk origin: the chair is offset, so a
                // desk-origin gate re-fires forever once the agent settles on it.
                let chair = desk_walk_anchor_facing(desk, layout.desk_facing_at(desk));
                let dist = (i32::from(prev.x) - i32::from(chair.x)).abs()
                    + (i32::from(prev.y) - i32::from(chair.y)).abs();
                if dist >= SNAP_BACK_MIN_DIST {
                    let (snap_target, chair_settle) = desk_leg_endpoint(desk, layout);
                    let p = snapshot_leg_profile(
                        rctx.router,
                        &layout.walkable,
                        rctx.overlay,
                        slot.agent_id,
                        LegPlan {
                            from: prev,
                            to: snap_target,
                            settle: chair_settle.map_or(Settle::None, Settle::End),
                            intent: WalkIntent::SnapBack,
                        },
                    );
                    walk.snap_back = Some(WalkLeg {
                        started_at: slot.state_started_at,
                        profile: p,
                        from: prev,
                    });
                }
            }
        }
        match walk.snap_back.clone() {
            Some(WalkLeg {
                started_at,
                profile,
                from: snap_prev,
            }) if started_at == slot.state_started_at => {
                let elapsed_ms = crate::anim::elapsed_ms(now, started_at);
                // PURE physics, no time-compression: `WalkIntent::SnapBack`'s higher
                // accel keeps the urgent return brisk on its own.
                if walk_arrived(&profile, elapsed_ms) {
                    walk.snap_back = None;
                    None
                } else {
                    let progress = walk_progress(&profile, elapsed_ms);
                    // Deterministic, so the rendered leg reproduces the armed
                    // profile's endpoint.
                    let (snap_target, chair_settle) = desk_leg_endpoint(desk, layout);
                    // The FROZEN origin from arm time, not the per-frame `prev`:
                    // history holds the advancing walker, so reading it back creeps
                    // `from` deskward and breaks the freeze's `wp.from == from` guard.
                    Some((
                        LegPose {
                            from: snap_prev,
                            to: snap_target,
                            progress,
                            carrying_coffee: false,
                        },
                        chair_settle.map_or(Settle::None, Settle::End),
                    ))
                }
            }
            _ => None,
        }
    };

    match snap {
        Some((leg, settle)) => Some(route_walking_pose(slot, now, layout, rctx, leg, settle)),
        None => {
            if let Some(walk) = rctx.walks.get_mut(&slot.agent_id) {
                walk.walk_path = None;
            }
            Some(raw)
        }
    }
}

/// `slot`'s pose while a pointer holds it, or on its walk home from where it
/// was set down, `desk` its desk; `None` once home, or never lifted. Home, its
/// wander restarts seated from `now`, so the timeline it missed cannot teleport it.
fn lifted_pose(
    slot: &AgentSlot,
    now: SystemTime,
    layout: &SceneLayout,
    desk: Point,
    rctx: &mut RouteCtx<'_>,
) -> Option<Pose> {
    let (home, chair_settle) = desk_leg_endpoint(desk, layout);
    let settle = chair_settle.map_or(Settle::None, Settle::End);
    let walk = rctx.walks.get_mut(&slot.agent_id)?;
    match walk.lifted {
        Some(Lifted::Held(at)) => return Some(Pose::Held { at }),
        Some(Lifted::Dropped(at)) => {
            let from = landing(at, home, layout);
            let profile = snapshot_leg_profile(
                rctx.router,
                &layout.walkable,
                rctx.overlay,
                slot.agent_id,
                LegPlan {
                    from,
                    to: home,
                    settle,
                    intent: WalkIntent::SnapBack,
                },
            );
            walk.lifted = Some(Lifted::Home(WalkLeg {
                started_at: now,
                profile,
                from,
            }));
        }
        Some(Lifted::Home(_)) | None => {}
    }
    let Some(Lifted::Home(leg)) = walk.lifted.clone() else {
        return None;
    };
    let elapsed_ms = crate::anim::elapsed_ms(now, leg.started_at);
    if walk_arrived(&leg.profile, elapsed_ms) {
        walk.lifted = None;
        // Its way in now, so the one it was lifted off never resumes.
        walk.entry = Some(leg);
        walk.snap_back = None;
        walk.wander.phase = WanderPhase::Seated;
        walk.wander.phase_started_at = now;
        walk.wander.last_advanced_at = now;
        walk.wander.target.kind = WanderKind::Aimless;
        return None;
    }
    let leg_pose = LegPose {
        from: leg.from,
        to: home,
        progress: walk_progress(&leg.profile, elapsed_ms),
        carrying_coffee: false,
    };
    Some(route_walking_pose(
        slot, now, layout, rctx, leg_pose, settle,
    ))
}

/// Where an agent set down at `at` lands: on the nearest floor its legs
/// reach, else `home`.
fn landing(at: Point, home: Point, layout: &SceneLayout) -> Point {
    crate::pathfind::snap_point_where(&layout.walkable, at, |p| layout.reachable.reaches(p))
        .unwrap_or(home)
}

fn route_walking_pose(
    slot: &AgentSlot,
    now: SystemTime,
    layout: &SceneLayout,
    rctx: &mut RouteCtx<'_>,
    leg: LegPose,
    settle: Settle,
) -> Pose {
    let router = &mut *rctx.router;
    let overlay = rctx.overlay;
    let history = &mut *rctx.history;
    let walks = &mut *rctx.walks;
    let LegPose {
        from,
        to,
        progress,
        carrying_coffee,
    } = leg;

    // Frozen per leg (`WalkPathSnapshot`), which also spares every frame a re-route's
    // A* cost.
    let path = {
        let walk = walks.entry(slot.agent_id).or_default();
        match &walk.walk_path {
            Some(wp) if wp.from == from && wp.to == to => wp.path.clone(),
            _ => {
                let mut p =
                    route_jittered(router, &layout.walkable, overlay, slot.agent_id, from, to);
                match settle {
                    Settle::End(s) if p.last() != Some(&s) => p.push(s),
                    Settle::Start(s) if p.first() != Some(&s) => p.insert(0, s),
                    Settle::Both { start, end } => {
                        // Append the end first so the prepend can't shift it.
                        if p.last() != Some(&end) {
                            p.push(end);
                        }
                        if p.first() != Some(&start) {
                            p.insert(0, start);
                        }
                    }
                    _ => {}
                }
                // Only CORNERED routes (>2 points): freezing a straight 2-point walk
                // sticks a transient `find_path` miss — a walk through walls — for the
                // whole leg, where unfrozen the next frame recovers.
                if p.len() > 2 {
                    walk.walk_path = Some(WalkPathSnapshot {
                        from,
                        to,
                        path: p.clone(),
                    });
                } else {
                    walk.walk_path = None;
                }
                p
            }
        }
    };
    let segment = if path.len() <= 2 {
        Segment::straight(from, to, progress)
    } else {
        Segment::on_route(&path, progress)
    };
    // Record the interpolated position for the next frame's snap-back lookup.
    history.record(slot.agent_id, segment.at(), now);
    Pose::Walking {
        segment,
        carrying_coffee,
    }
}

/// Half-range (px) of the per-agent destination jitter — the offset spans
/// `-JITTER_MAX_PX..=JITTER_MAX_PX`, so the modulus span is `2*MAX+1`.
const JITTER_MAX_PX: i32 = 4;
const JITTER_SPAN: u64 = (2 * JITTER_MAX_PX + 1) as u64;

/// Per-agent routing-destination jitter, hashed from the agent_id, so converging
/// agents take visibly different polylines (breaks the "ant trail"). Output must
/// stay bit-identical across every call site — a site routing the RAW goal
/// measures a differently-shaped polyline than the one rendered AND mints a
/// second router-cache key per leg.
pub(crate) fn jitter_dest(id: AgentId, p: Point) -> Point {
    let h = id.raw();
    let jx = ((h % JITTER_SPAN) as i32 - JITTER_MAX_PX) as i16;
    let jy = (((h >> 16) % JITTER_SPAN) as i32 - JITTER_MAX_PX) as i16;
    Point {
        x: p.x.saturating_add_signed(jx),
        y: p.y.saturating_add_signed(jy),
    }
}

/// Route `from → to` against the per-agent JITTERED goal, then restore the
/// polyline's endpoint to the true `to`. EVERY walk leg routes through here, so
/// the rendered shape, the measured profile length and the router-cache key
/// can't diverge (the [`jitter_dest`] lockstep contract).
pub(crate) fn route_jittered(
    router: &mut dyn Router,
    mask: &WalkableMask,
    overlay: &OccupancyOverlay,
    id: AgentId,
    from: Point,
    to: Point,
) -> Vec<Point> {
    let mut p = router.route(mask, overlay, from, jitter_dest(id, to));
    if let Some(last) = p.last_mut() {
        *last = to;
    }
    p
}

#[cfg(test)]
mod tests;
