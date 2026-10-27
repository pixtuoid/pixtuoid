//! Pure state → pose derivation: `derive(slot, now, layout)` is a function of
//! the snapshot inputs only — no routing, no per-frame history. The routed
//! variant (composing against a `Router` and a `PoseHistory` cache) is the
//! sibling `pose/mod.rs` (`derive_with_routing`).

use std::num::NonZeroU16;
use std::time::{Duration, SystemTime};

use crate::layout::{
    Bounds, DwellWindow, HomeDesk, Point, SceneLayout, WaypointKind, desk_furniture_def,
    desk_walk_anchor_facing, furniture_def,
};
use crate::physics::LegProgress;
use crate::walk::{WanderKind, WanderTarget};
use pixtuoid_core::AgentId;
use pixtuoid_core::sprite::Sprite;
use pixtuoid_core::state::{ActivityState, AgentSlot};

/// Base of the stale-resume / off-screen-gap sentinel in
/// `walk::advance_wander` — above on-screen frame cadence, below a
/// floor-switch-away gap. NOT the wander dwell; see `dwell_ms` for that.
pub(crate) const STALE_RESUME_GAP_BASE_MS: u64 = 7_000;
/// Maximum extra time added per agent — jitter range is `[0, RANGE)`.
pub(crate) const STALE_RESUME_GAP_RANGE_MS: u64 = 6_000;

/// Stateless-overlay estimate of one wander walk leg. Exact coherence with the
/// routed timeline is impossible (core has no router, and walk legs are
/// physics-timed only on the routed path), and `idle_pose` only needs an
/// approximate timeline to place the occupancy overlay.
pub(crate) const WANDER_WALK_EST_MS: u64 = 3_500;
/// Companion estimate: the at-waypoint dwell beat (paired with `WANDER_WALK_EST_MS`).
pub(crate) const WANDER_DWELL_EST_MS: u64 = 18_000;

/// The frame of typing loop `anim` for `slot` on `beat`: the art's own loop,
/// phased by when it began typing so neighbours key out of step; the first
/// frame at rest.
pub(crate) fn typing_frame(slot: &AgentSlot, beat: crate::anim::Beat, anim: &Sprite) -> usize {
    if beat.is_rest() {
        return 0;
    }
    let frames = anim.frames().len();
    let phase = crate::anim::epoch_ms(slot.state_started_at) / u64::from(anim.frame_ms().max(1));
    let phase = (phase % frames as u64) as usize;
    (crate::pack::looping_frame_index(anim, beat) + phase) % frames
}

/// The frame walk `anim` shows [`travelled`](crate::physics::Segment::travelled) along
/// its leg: a full cycle each `stride`.
pub(crate) fn walk_frame(travelled: u32, anim: &Sprite, stride: NonZeroU16) -> usize {
    let frames = anim.frames().len() as u64;
    let per_cycle = u64::from(stride.get()) * u64::from(crate::physics::OCTILE_STRAIGHT_COST);
    ((u64::from(travelled) * frames / per_cycle) % frames) as usize
}

/// Spawn-window guard for entry routing in `pose::derive_with_routing`: the
/// *upper bound* on the window during which the routed walk layer will
/// attempt an entry walk and (via `FloorCtx::door_anim_max_ms`) drive door-open
/// cosmetics. NOT the walk duration — the walk completes when
/// `physics::walk_arrived` returns true.
pub(crate) const ENTRY_ANIMATION_MS: u64 = 4000;

/// Per-agent stale-resume gap (ms): above this `now - last_advanced_at`,
/// `walk::advance_wander` treats the floor as off-screen/paused and
/// re-bootstraps analytically instead of replaying the backlog one transition
/// per frame. Jittered per agent so floors don't re-bootstrap in lockstep.
pub(crate) fn stale_resume_gap_ms(agent_id: AgentId) -> u64 {
    STALE_RESUME_GAP_BASE_MS + (agent_id.raw() >> 16) % STALE_RESUME_GAP_RANGE_MS
}

/// ⌊2⁶⁴/φ₃⌋, φ₃ the real root of x⁴ = x + 1.
const PHI3_GAMMA: u64 = 0xd1b5_4a32_d192_ed03;

/// Base dwell plus deterministic per-agent jitter within `window`. `tag` is NOT
/// a cryptographic salt — it decorrelates the callers' jitter from each other
/// and from the id-bit-slicing knobs (`speed_mult` / `pause_ms` / `cycle_ms`).
fn jittered_dwell(window: DwellWindow, agent_id: AgentId, tag: u64) -> u64 {
    let DwellWindow { base_ms, range_ms } = window;
    base_ms + pixtuoid_core::id::splitmix64(agent_id.raw() ^ tag) % range_ms.max(1)
}

/// Absolute dwell (ms) an agent lingers at a waypoint, per spot kind, with
/// per-agent jitter. A sofa / meeting seat is a long lounge; a vending grab
/// is quick.
pub(crate) fn dwell_ms(kind: WaypointKind, agent_id: AgentId) -> u64 {
    jittered_dwell(furniture_def(kind.furniture()).dwell, agent_id, PHI3_GAMMA)
}

/// Absolute dwell (ms) an agent sits at its desk between wander trips.
pub fn seated_dwell_ms(agent_id: AgentId) -> u64 {
    jittered_dwell(desk_furniture_def().dwell, agent_id, crate::GOLDEN_GAMMA)
}

/// Estimated full wander-cycle wall-time for an agent (desk dwell + two walk
/// legs + one waypoint dwell). Shared by `idle_pose` and `advance_wander`'s
/// bootstrap so both place a long-idle agent on the same approximate cycle.
pub fn est_wander_cycle_ms(agent_id: AgentId) -> u64 {
    seated_dwell_ms(agent_id) + 2 * WANDER_WALK_EST_MS + WANDER_DWELL_EST_MS
}

/// Per-agent wander personality derived from the agent's id hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Personality {
    /// Probability (in percent) that this agent takes a trip on a given cycle.
    pub trip_chance_pct: u8,
    /// Probability (in percent) that a trip is aimless wander rather than a
    /// lounge waypoint.
    pub aimless_pref_pct: u8,
}

const TRIP_CHANCE_SPAN_PCT: u64 = 36;
const AIMLESS_PREF_SPAN_PCT: u64 = 51;

/// Derives an agent's wander `Personality` from its id hash.
pub(crate) fn personality_for(agent_id: AgentId) -> Personality {
    let h = agent_id.raw();
    Personality {
        trip_chance_pct: (25 + (h % TRIP_CHANCE_SPAN_PCT)) as u8,
        aimless_pref_pct: ((h >> 8) % AIMLESS_PREF_SPAN_PCT) as u8,
    }
}

/// Deterministic per-(agent, cycle) decision: does this agent take a wander
/// trip on this cycle, or stay seated?
pub fn takes_trip(agent_id: AgentId, cycle_n: u64) -> bool {
    let p = personality_for(agent_id);
    let mix = agent_id.raw() ^ cycle_n.wrapping_mul(crate::GOLDEN_GAMMA);
    (mix % 100) < u64::from(p.trip_chance_pct)
}

/// Per-(agent, cycle) decision: when the agent takes a trip, is it an aimless
/// wander (random cubicle_aisle point) or a directed visit to a named waypoint?
pub fn is_aimless_cycle(agent_id: AgentId, cycle_n: u64) -> bool {
    let p = personality_for(agent_id);
    let type_mix = agent_id.raw() ^ cycle_n.wrapping_mul(pixtuoid_core::id::SPLITMIX64_M1);
    (type_mix % 100) < u64::from(p.aimless_pref_pct)
}

/// Per-(agent, cycle) waypoint index. Only meaningful when `takes_trip` is
/// true AND `is_aimless_cycle` is false.
#[deny(
    clippy::cast_possible_truncation,
    reason = "a hash picks through `crate::spread`, alike on every target"
)]
pub fn waypoint_index_for_cycle(agent_id: AgentId, cycle_n: u64, num_waypoints: usize) -> usize {
    crate::spread(agent_id.raw() ^ cycle_n, num_waypoints)
}

/// The pose an agent renders this frame — the output of pose derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pose {
    /// Seated at the desk, idle.
    SeatedIdle,
    /// Seated at the desk, typing; the sim steps its frame on the floor's
    /// beat.
    SeatedTyping,
    /// At a lounge waypoint; the concrete sprite depends on the kind.
    AtWaypoint {
        /// Index of the target waypoint.
        wp: usize,
        /// Kind of the target waypoint (selects the concrete sprite).
        kind: WaypointKind,
    },
    /// Walking along the current leg, drawn on the segment it is on.
    Walking {
        segment: crate::physics::Segment,
        /// Whether the agent renders holding a coffee on this leg.
        carrying_coffee: bool,
    },
    /// Standing at a random cubicle_aisle point (not at any waypoint).
    AimlessAt {
        /// Layout-pixel point the agent ambled to.
        dest: Point,
    },
    /// Lifted by a pointer, off the floor.
    Held {
        /// Layout-pixel point its feet hang at.
        at: Point,
    },
}

/// A walk stated on its whole leg, before it is drawn.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LegPose {
    pub(crate) from: Point,
    pub(crate) to: Point,
    pub(crate) progress: LegProgress,
    /// Whether the agent renders holding a coffee on this leg.
    pub(crate) carrying_coffee: bool,
}

impl LegPose {
    /// Drawn as the one straight segment between its own ends: no route bends it.
    pub(crate) fn straight(self) -> Pose {
        Pose::Walking {
            segment: crate::physics::Segment::straight(self.from, self.to, self.progress),
            carrying_coffee: self.carrying_coffee,
        }
    }
}

/// Returns `None` if the slot's desk_index is out of range for `layout`.
///
/// Priority, first match wins: exit override (walk to the door, then `None`
/// once the window passes) → entry override (door → desk, regardless of state
/// changes) → the state-driven pose.
pub(crate) fn derive(slot: &AgentSlot, now: SystemTime, layout: &SceneLayout) -> Option<Pose> {
    let desk = layout.home_desk(slot.desk_index.single_floor_local())?;

    // The target is `door_threshold` (the on-floor point below the door), not
    // the door itself, so the character doesn't paint through the wall trim.
    if let Some(exit_time) = slot.exiting_at {
        let target = layout.door_threshold;
        let since_exit = now
            .duration_since(exit_time)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64;
        if since_exit < ENTRY_ANIMATION_MS {
            return Some(linear_walk_pose(
                since_exit,
                desk_walk_anchor_facing(desk.at, desk.facing),
                target,
            ));
        }
        return None;
    }

    // The entry walk ends at the seated anchor, not inside the desk obstacle:
    // otherwise the A* router detours around the desk and always approaches
    // from one side.
    let from = layout.door_threshold;
    let since_spawn = now
        .duration_since(slot.created_at)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64;
    if since_spawn < ENTRY_ANIMATION_MS {
        return Some(linear_walk_pose(
            since_spawn,
            from,
            desk_walk_anchor_facing(desk.at, desk.facing),
        ));
    }

    state_driven_pose(slot, desk, layout, now)
}

/// The shared `Walking` pose for the stateless entry/exit overrides: a LINEAR
/// (not physics-timed) interpolation over `ENTRY_ANIMATION_MS`. Deliberately
/// distinct from the routed path's kinematic profiles — the overlay/snapshot
/// path stays linear so it has no per-frame history.
fn linear_walk_pose(since_ms: u64, from: Point, to: Point) -> Pose {
    LegPose {
        from,
        to,
        progress: LegProgress::of(since_ms, ENTRY_ANIMATION_MS),
        carrying_coffee: false,
    }
    .straight()
}

/// The state→pose tail shared by `derive` and `derive_state_only`, applied
/// AFTER each caller's own override guards — one place, so the two entry points
/// can't drift.
fn state_driven_pose(
    slot: &AgentSlot,
    desk: HomeDesk,
    layout: &SceneLayout,
    now: SystemTime,
) -> Option<Pose> {
    let elapsed = now
        .duration_since(slot.state_started_at)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64;

    match &slot.state {
        ActivityState::Running { .. } => Some(Pose::SeatedTyping),
        ActivityState::RequiresAction { .. } => Some(Pose::SeatedIdle),
        ActivityState::Idle => Some(idle_pose(slot, desk, layout, elapsed)),
    }
}

/// Pure state → pose derivation, **excluding** the exit and entry override
/// blocks at the top of `derive` — the seam `pose::derive_with_routing` uses so
/// a physics-driven entry already in flight doesn't restart a redundant linear
/// entry walk.
///
/// Returns `None` when `slot.desk_index` is out of range for `layout`.
pub(crate) fn derive_state_only(
    slot: &AgentSlot,
    now: SystemTime,
    layout: &SceneLayout,
) -> Option<Pose> {
    let desk = layout.home_desk(slot.desk_index.single_floor_local())?;
    state_driven_pose(slot, desk, layout, now)
}

/// Per-(agent, cycle) seed for `pick_aimless_dest`, shared by the stateless
/// `idle_pose` and the routed `pick_wander_dest` so the two can't drift to
/// different aimless destinations.
pub(crate) fn aimless_wander_seed(agent_id: AgentId, cycle_n: u64) -> u64 {
    agent_id.raw() ^ cycle_n.wrapping_mul(PHI3_GAMMA)
}

/// Pick an aimless wander destination: a weighted zone choice, then
/// rejection-sampling within it for a walkable pixel. The weights are taste —
/// the window strip and pantry are where people drift on a break, the meeting
/// room is a rare drift-in. Falls back to the nearest walkable corridor-midline
/// cell, then to `home_desk`'s walk anchor — never a cell inside furniture.
///
/// `home_desk` is the same desk on both callers, so the result stays
/// deterministic in `(layout, seed)` across them.
pub(crate) fn pick_aimless_dest(layout: &SceneLayout, seed: u64, home_desk: HomeDesk) -> Point {
    let window_strip = Bounds {
        x: layout.cubicle_band.x,
        y: layout.top_margin + 1,
        width: layout.cubicle_band.width,
        height: 10,
    };
    let zones: [(Bounds, u16); 5] = [
        (window_strip, 30),
        (layout.pantry.map_or(window_strip, |p| p.bounds), 25),
        (layout.corridor, 20),
        (layout.cubicle_band, 15),
        (layout.meeting_room_bounds(0).unwrap_or(window_strip), 10),
    ];
    let total: u16 = zones.iter().map(|(_, w)| *w).sum();
    let mut roll = ((seed >> 32) as u16) % total;
    // What `roll` has left after every zone but the last lands in the last.
    let [rest @ .., last] = &zones;
    let zone = rest
        .iter()
        .find_map(|(b, w)| {
            if roll < *w {
                Some(b)
            } else {
                roll -= w;
                None
            }
        })
        .unwrap_or(&last.0);
    // ROUTABLE, not merely walkable — the same `reaches` filter every NAMED
    // destination gets via `approach_point`.
    let routable =
        |x: u16, y: u16| layout.is_walkable(x, y) && layout.reachable.reaches(Point { x, y });
    const AIMLESS_SAMPLE_ATTEMPTS: u64 = 32;
    for i in 0..AIMLESS_SAMPLE_ATTEMPTS {
        let h = seed
            .wrapping_add(i.wrapping_mul(crate::GOLDEN_GAMMA))
            .wrapping_mul(crate::MURMUR64A_M);
        let x = zone.x + (h as u16) % zone.width.max(1);
        let y = zone.y + ((h >> 16) as u16) % zone.height.max(1);
        if routable(x, y) {
            return Point { x, y };
        }
    }
    // Randomised x so fallback agents spread out instead of clustering. The
    // midline is NOT guaranteed open (the vending / printer footprints sit in
    // the cubicle_aisle band), so scan outward from it for the nearest walkable
    // cell — the contract is "returns a walkable pixel".
    let c = layout.corridor;
    let x_jitter = (seed as u16) % c.width.max(1);
    let base_x = c.x + x_jitter;
    let mid_y = c.y + c.height / 2;
    let in_band = |x: u16| x >= c.x && x < c.x.saturating_add(c.width);
    for d in 0..c.width {
        let east = base_x.saturating_add(d);
        if in_band(east) && routable(east, mid_y) {
            return Point { x: east, y: mid_y };
        }
        let west = base_x.saturating_sub(d);
        if in_band(west) && routable(west, mid_y) {
            return Point { x: west, y: mid_y };
        }
    }
    // Whole midline blocked (degenerate layout): the agent's own desk anchor is
    // a destination every consumer already handles — return it rather than hand
    // out a cell inside furniture.
    desk_walk_anchor_facing(home_desk.at, home_desk.facing)
}

/// The EXCLUSIVE-spot waypoints already spoken for. An exclusive spot holds
/// exactly one occupant and is its OWN waypoint, so two agents at the same one
/// has no valid rendering — the painter's de-collision can only slide the second
/// sideways onto thin air. Claims are therefore resolved where the destination
/// is CHOSEN, not where it's drawn. Small by construction (at most one entry per
/// agent out on an exclusive-spot trip), so linear `contains` beats hashing.
#[derive(Debug, Default, Clone)]
pub(crate) struct SpotClaims(Vec<usize>);

impl SpotClaims {
    /// Claim `wp_idx` for an exclusive-spot trip; idempotent, so an agent
    /// re-picking its own held spot can't grow the set.
    pub(crate) fn claim(&mut self, wp_idx: usize) {
        if !self.holds(wp_idx) {
            self.0.push(wp_idx);
        }
    }

    /// Whether some agent is already headed for (or occupying) this spot —
    /// always false for a shareable queue spot, which is never claimed.
    pub(crate) fn holds(&self, wp_idx: usize) -> bool {
        self.0.contains(&wp_idx)
    }
}

/// Resolve the wander destination for `(agent_id, cycle_n)` on `layout`, with
/// `origin` (the home desk) as the approach-side tiebreaker. The ONE stateless
/// wander-destination resolver: the stateful walk authority
/// (`walk::advance_wander` via `pick_wander_dest`) delegates to it, and
/// `idle_pose` calls it then maps the [`WanderTarget`] to a [`Pose`].
///
/// `idle_pose` passes an EMPTY `claimed` set because the pure half has no
/// occupancy knowledge, so the two DO diverge on a contested seat: the A\*
/// occupancy prepass then reserves the loser's cell and a walker may clip a real
/// sitter. Accepted — rare and cosmetic, and it keeps the pure half a function
/// of its snapshot inputs, so the overlay's cache signature stays frame-stable.
///
/// A named waypoint whose only reachable side is its excluded backrest (or a
/// fully-blocked obstacle) yields `approach_point == wp.pos`, the "no valid
/// approach" sentinel — this ambles aimlessly that cycle instead of routing into
/// the furniture.
pub(crate) fn resolve_wander_target(
    id: AgentId,
    cycle_n: u64,
    layout: &SceneLayout,
    origin: HomeDesk,
    claimed: &SpotClaims,
) -> WanderTarget {
    let amble = || WanderTarget {
        dest: pick_aimless_dest(layout, aimless_wander_seed(id, cycle_n), origin),
        kind: WanderKind::Aimless,
    };
    if is_aimless_cycle(id, cycle_n) {
        return amble();
    }
    let n = layout.waypoints.len();
    let first = waypoint_index_for_cycle(id, cycle_n, n);
    // A claimed seat isn't a destination: probe forward to the next unclaimed
    // waypoint. A venue's seats are CONTIGUOUS in `waypoints`, so the probe
    // lands on a neighbouring seat of the SAME venue first — "that chair's
    // taken, I'll take the next one" — and only walks off once the venue is
    // full.
    let Some(wp_idx) = (0..n)
        .map(|step| (first + step) % n)
        .find(|i| !claimed.holds(*i))
    else {
        return amble();
    };
    let wp = layout.waypoints[wp_idx];
    // The A*-reachable approach point on an allowed side, NOT the raw blocked
    // `wp.pos` (which made A* detour and the sprite pop).
    let dest = layout.approach_point(wp.kind.furniture(), wp.pos, origin.at, wp.facing);
    if dest == wp.pos {
        return amble();
    }
    // Seat foot cell: the walk SETTLES from `dest` onto it and the sprite renders
    // there. `None` for obstacles — the agent stands AT `dest`.
    let seat = crate::layout::seated_foot_cell(wp.kind.furniture(), wp.pos);
    WanderTarget {
        dest,
        kind: WanderKind::Named {
            wp_idx,
            kind: wp.kind,
            seat,
        },
    }
}

fn idle_pose(slot: &AgentSlot, desk: HomeDesk, layout: &SceneLayout, elapsed_ms: u64) -> Pose {
    let cycle_ms = est_wander_cycle_ms(slot.agent_id);
    let cycle_n = elapsed_ms / cycle_ms;
    let phase_t = elapsed_ms % cycle_ms;

    if !takes_trip(slot.agent_id, cycle_n) || layout.waypoints.is_empty() {
        return Pose::SeatedIdle;
    }

    let seated_end = seated_dwell_ms(slot.agent_id);
    let walk_out_end = seated_end + WANDER_WALK_EST_MS;
    let at_wp_end = walk_out_end + WANDER_DWELL_EST_MS;

    // Seat claims are EMPTY here: this half has no occupancy knowledge (see
    // `resolve_wander_target`), and its job is a cache-stable overlay estimate,
    // not the authoritative placement.
    let target =
        resolve_wander_target(slot.agent_id, cycle_n, layout, desk, &SpotClaims::default());
    let dest = target.dest;
    let at_dest_pose: Pose = target.kind.at_pose(target.dest);

    if phase_t < seated_end {
        Pose::SeatedIdle
    } else if phase_t < walk_out_end {
        let span = walk_out_end - seated_end;
        let t = LegProgress::of(phase_t - seated_end, span);
        LegPose {
            from: desk.at,
            to: dest,
            progress: t,
            carrying_coffee: false,
        }
        .straight()
    } else if phase_t < at_wp_end {
        at_dest_pose
    } else {
        let span = cycle_ms - at_wp_end;
        let t = LegProgress::of(phase_t - at_wp_end, span);
        LegPose {
            from: dest,
            to: desk.at,
            progress: t,
            carrying_coffee: target.kind.carries_coffee(),
        }
        .straight()
    }
}

#[cfg(test)]
mod tests;
