//! Ambient creatures — the office pet and the gateway mascots — walking the
//! people's walker: a frozen A* leg on the walkable mask, timed by the shared
//! physics at the creature's own gait, its frames stepped by the ground it
//! covers. This is sim/behaviour: the painters draw what [`CreatureWalk::step`]
//! answers. The WALK runs on the wall clock; the decision to roam is ambient
//! and runs on the beat, so a calmer motion tier starts fewer walks and a still
//! one starts none, while a walk under way always finishes.

use std::num::NonZeroU16;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentSlot;
use pixtuoid_core::SceneState;
use pixtuoid_core::id::splitmix64;
use pixtuoid_core::source::daemon::DaemonInstanceKey;
use pixtuoid_core::state::{ActivityState, DaemonState};
use pixtuoid_core::walkable::OccupancyOverlay;

use crate::anim::{FULL_TICK_MS, Timing, elapsed_ms};
use crate::layout::{Point, SceneLayout};
use crate::pathfind::{Router, snap_point_to_walkable, snap_point_where};
use crate::pet::PetKind;
use crate::physics::{
    Gait, OCTILE_STRAIGHT_COST, Segment, WALK_ACCEL, WalkProfile, octile_path_len, walk_arrived,
    walk_profile_for, walk_progress,
};
use crate::pose::STALE_RESUME_GAP_BASE_MS;

/// How close a resting spot must be to an idle agent's desk to count as "napping
/// beside them" — sized to the desk's footprint plus a creature's width, so it
/// reads as sharing that workstation rather than merely being on the same floor.
const NAP_NEAR_DESK_PX: i32 = 16;

/// Whether `at` is beside an idle agent's desk, where a pet that naps near
/// idlers sleeps.
pub(crate) fn naps_here(layout: &SceneLayout, agents: &[AgentSlot], at: Point) -> bool {
    agents
        .iter()
        .filter(|a| matches!(a.state, ActivityState::Idle) && a.exiting_at.is_none())
        .filter_map(|a| layout.home_desk(a.desk_index.single_floor_local()))
        .any(|d| {
            (i32::from(at.x) - i32::from(d.at.x)).abs() <= NAP_NEAR_DESK_PX
                && (i32::from(at.y) - i32::from(d.at.y)).abs() <= NAP_NEAR_DESK_PX
        })
}

/// How many draws `walkable_target` tries before falling back to a snap. Walkable
/// floor is roughly half the buffer, so P(all miss) is ~2^-8 per call.
const TARGET_TRIES: u32 = 8;

/// A destination drawn from the WHOLE walkable floor, deterministic per
/// `(seed, n)` — the ONE destination rule both roamers and every daemon state use.
/// A curated spot list is not an option: it is small enough that N creatures share
/// destinations by pigeonhole, and no per-creature offset scheme recovers from that.
///
/// REJECTION sampling, not enumeration: collecting every walkable cell would cost
/// O(w*h) per creature per frame in the render loop. `snap_point_to_walkable` is
/// the exact backstop if every draw lands on furniture.
fn walkable_target(layout: &SceneLayout, seed: u64, n: u64) -> Point {
    let (w, h) = (layout.walkable.width(), layout.walkable.height());
    if w == 0 || h == 0 {
        return Point { x: 0, y: 0 };
    }
    let mut z = seed ^ n.wrapping_mul(crate::GOLDEN_GAMMA);
    let mut last = Point { x: 0, y: 0 };
    for _ in 0..TARGET_TRIES {
        z = pixtuoid_core::id::splitmix64(z);
        // Independent halves of one hash: the high word picks x, the low word y, so
        // a draw is not diagonal-biased.
        last = Point {
            x: ((z >> 32) % u64::from(w)) as u16,
            y: (z % u64::from(h)) as u16,
        };
        // Judge the SNAPPED point, not the draw: `snap_point_to_walkable`
        // answers with the coarse cell's CENTRE, so a clear draw can still rest
        // on decor. Snapping is idempotent, which makes every downstream
        // `snap(dest)` an identity — the walk lands exactly where the rest is.
        if let Some(cand) = snap_point_to_walkable(&layout.walkable, last)
            && rests_at(layout, cand)
        {
            return cand;
        }
    }
    // Snapped like the loop's answers, so the idempotence above holds for EVERY
    // return and not just the ones a draw found.
    snap_point_to_walkable(&layout.walkable, layout.door_threshold).unwrap_or(last)
}

/// Whether a creature may rest at walkable `p`.
fn rests_at(layout: &SceneLayout, p: Point) -> bool {
    // Walkable is not enough: a cell under a desk's overhang is walkable by
    // invariant #6 and painted over anyway, and a creature RESTS here between
    // walks. Walking through one stays fine.
    // And reachable: a creature walks there, and A* from the door's ground
    // cannot reach a pocket the walls close off.
    layout.is_visually_clear(p) && layout.reachable.reaches(p)
}

/// Full ticks per walk cycle at a pet's cruise.
const PET_TICKS_PER_STRIDE: u64 = 2;
/// Full ticks per walk cycle at a running gateway mascot's cruise; an idle one
/// takes twice as many, a degraded one three times.
const MASCOT_TICKS_PER_STRIDE: u64 = 4;

/// About how long a pet rests between walks, in beat ms.
const PET_REST_MS: u64 = 25_000;
/// The longest a pet rests between walks at Full, in ms: a test watching it
/// roam waits this long.
pub const PET_LONGEST_REST_MS: u64 = longest_rest(PET_REST_MS);
/// About how long a gateway mascot rests between walks, in beat ms.
const MASCOT_RUNNING_REST_MS: u64 = 2_500;
const MASCOT_IDLE_REST_MS: u64 = 5_000;
const MASCOT_DEGRADED_REST_MS: u64 = 7_700;

/// Which creature a walk belongs to: a floor's pet, or a gateway's mascot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum CreatureKey {
    Pet(PetKind),
    Mascot(DaemonInstanceKey),
}

/// How a creature roams: its gait, and about how long it rests between walks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Roam {
    pub(crate) gait: Gait,
    pub(crate) rest_ms: u64,
}

impl Roam {
    /// Cruising one `stride` of its walk per `ticks` Full ticks, resting
    /// about `rest_ms`.
    fn of(stride: NonZeroU16, ticks: u64, rest_ms: u64) -> Self {
        let cruise = (u64::from(stride.get()) * u64::from(OCTILE_STRAIGHT_COST)) as f32
            / (ticks * FULL_TICK_MS) as f32;
        Roam {
            gait: Gait {
                cruise,
                accel: WALK_ACCEL,
                pause_ms: 0,
            },
            rest_ms,
        }
    }

    /// The pet's roam on its walk's `stride`.
    pub(crate) fn pet(stride: NonZeroU16) -> Self {
        Self::of(stride, PET_TICKS_PER_STRIDE, PET_REST_MS)
    }

    /// A gateway mascot's roam in `state` on its walk's `stride`: the busier,
    /// the brisker its step and the shorter its rests.
    pub(crate) fn mascot(stride: NonZeroU16, state: DaemonState) -> Self {
        match state {
            // a downed gateway's mascot hurries out
            DaemonState::Running | DaemonState::Down => {
                Self::of(stride, MASCOT_TICKS_PER_STRIDE, MASCOT_RUNNING_REST_MS)
            }
            DaemonState::Degraded => {
                Self::of(stride, 3 * MASCOT_TICKS_PER_STRIDE, MASCOT_DEGRADED_REST_MS)
            }
            _ => Self::of(stride, 2 * MASCOT_TICKS_PER_STRIDE, MASCOT_IDLE_REST_MS),
        }
    }
}

/// What a creature walks on this frame: the floor, its router, and the
/// stationary people its legs route around.
pub(crate) struct Ground<'a> {
    pub(crate) layout: &'a SceneLayout,
    pub(crate) router: &'a mut dyn Router,
    pub(crate) overlay: &'a OccupancyOverlay,
}

/// One frozen leg: when it began, its timing, and the polyline it follows.
#[derive(Debug, Clone)]
struct Walk {
    started_at: SystemTime,
    profile: WalkProfile,
    path: Vec<Point>,
    to: Point,
}

impl Walk {
    /// `from` to `to` on `ground` at `gait`, from `now`; `None` with no way
    /// there.
    fn plan(
        from: Point,
        to: Point,
        gait: Gait,
        ground: &mut Ground<'_>,
        now: SystemTime,
    ) -> Option<Self> {
        // The router answers an unroutable pair with the straight line through
        // whatever stands between: only a reachable pair walks.
        let reach = &ground.layout.reachable;
        if !reach.reaches(from) || !reach.reaches(to) {
            return None;
        }
        let path = ground
            .router
            .route(&ground.layout.walkable, ground.overlay, from, to);
        let len = octile_path_len(&path);
        (len > 0).then(|| Walk {
            started_at: now,
            profile: walk_profile_for(len, gait),
            path,
            to,
        })
    }

    /// Where on its path it is at `now`; `None` once it has arrived.
    fn segment(&self, now: SystemTime) -> Option<Segment> {
        let elapsed = elapsed_ms(now, self.started_at);
        (!walk_arrived(&self.profile, elapsed))
            .then(|| Segment::on_route(&self.path, walk_progress(&self.profile, elapsed)))
    }
}

#[derive(Debug, Clone)]
enum Phase {
    /// Resting at `at`, its last roam set off at the wall instant `since` and
    /// walked `walked_ms`: the next sets off once that walk and a rest have
    /// passed ON THE BEAT, so a tier ¼ as fast roams ¼ as often.
    Resting {
        at: Point,
        since: SystemTime,
        walked_ms: u64,
    },
    Walking(Walk),
    /// Walking out, gone on arrival.
    Leaving(Walk),
    /// Lifted by a pointer, at `at`: off the floor, so it neither rests nor
    /// roams until set down.
    Carried {
        at: Point,
    },
    Gone,
}

/// Where a creature is this frame, and the segment it is on while it walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stance {
    pub(crate) at: Point,
    pub(crate) walking: Option<Segment>,
}

/// One creature on the people's walker: resting somewhere, or on a leg.
#[derive(Debug, Clone)]
pub(crate) struct CreatureWalk {
    phase: Phase,
    seed: u64,
    /// The roams begun: the next destination is draw `roams + 1`.
    roams: u64,
    advanced_at: SystemTime,
    /// The walkable mask's size its points are on.
    ground_size: (u16, u16),
}

fn ground_size(layout: &SceneLayout) -> (u16, u16) {
    (layout.walkable.width(), layout.walkable.height())
}

impl CreatureWalk {
    /// Resting at its home draw, where a creature first seen settled is.
    pub(crate) fn at_home(layout: &SceneLayout, seed: u64, now: SystemTime) -> Self {
        CreatureWalk {
            phase: Phase::Resting {
                at: walkable_target(layout, seed, 0),
                since: now,
                walked_ms: 0,
            },
            seed,
            roams: 0,
            advanced_at: now,
            ground_size: ground_size(layout),
        }
    }

    /// Walking in from `from` to its home draw, set off at `started_at`; one
    /// first seen after that walk would have ended rests at home from `now`.
    pub(crate) fn arriving(
        from: Point,
        seed: u64,
        roam: Roam,
        ground: &mut Ground<'_>,
        started_at: SystemTime,
        now: SystemTime,
    ) -> Self {
        let mut walk = Self::at_home(ground.layout, seed, now);
        if let Phase::Resting { at: home, .. } = walk.phase
            && let Some(w) = Walk::plan(from, home, roam.gait, ground, started_at)
            && w.segment(now).is_some()
        {
            walk.phase = Phase::Walking(w);
        }
        walk
    }

    /// Whether it is on its way out, or gone.
    pub(crate) fn leaving(&self) -> bool {
        matches!(self.phase, Phase::Leaving(_) | Phase::Gone)
    }

    /// Whether it is on a leg at `now`.
    pub(crate) fn walks_at(&self, now: SystemTime) -> bool {
        self.stance(now).is_some_and(|s| s.walking.is_some())
    }

    /// Whether a pointer holds it.
    pub(crate) fn carried(&self) -> bool {
        matches!(self.phase, Phase::Carried { .. })
    }

    /// Whether it has walked out.
    pub(crate) fn gone(&self) -> bool {
        matches!(self.phase, Phase::Gone)
    }

    /// Turn and walk out to `exit` from wherever it is at `now`, set down
    /// first if in hand; with no way there it is simply gone.
    pub(crate) fn leave(
        &mut self,
        exit: Point,
        roam: Roam,
        ground: &mut Ground<'_>,
        now: SystemTime,
    ) {
        if let Phase::Carried { at } = self.phase {
            self.set_down(at, ground.layout, now);
        }
        self.phase = match self.stance(now) {
            Some(Stance { at, .. }) => {
                Walk::plan(at, exit, roam.gait, ground, now).map_or(Phase::Gone, Phase::Leaving)
            }
            None => Phase::Gone,
        };
    }

    /// Held still where it stands (a pet being petted) since it last
    /// stepped: a rest restarts from `now`; a leg pauses, its clock pushed on
    /// by the hold, and walks on from the same point after, so it never rests
    /// mid-leg off the clear floor a rest needs.
    pub(crate) fn hold(&mut self, now: SystemTime) {
        let held = Duration::from_millis(elapsed_ms(now, self.advanced_at));
        match &mut self.phase {
            Phase::Resting {
                since, walked_ms, ..
            } => {
                *since = now;
                *walked_ms = 0;
            }
            Phase::Walking(w) | Phase::Leaving(w) => w.started_at += held,
            Phase::Carried { .. } | Phase::Gone => {}
        }
        self.advanced_at = now;
    }

    /// Lifted, at `at`.
    pub(crate) fn carry(&mut self, at: Point) {
        self.phase = Phase::Carried { at };
    }

    /// Set down near `at`, from `now`, unless it is on its way out: on the
    /// nearest floor it [rests on](rests_at), else at its latest draw, where a
    /// roam would have taken it; it rests there, and roams on from it.
    pub(crate) fn set_down(&mut self, at: Point, layout: &SceneLayout, now: SystemTime) {
        if self.leaving() {
            return;
        }
        let at = snap_point_where(&layout.walkable, at, |p| rests_at(layout, p))
            .unwrap_or_else(|| walkable_target(layout, self.seed, self.roams));
        self.phase = Phase::Resting {
            at,
            since: now,
            walked_ms: 0,
        };
        self.advanced_at = now;
    }

    /// Where it is at `now`, without advancing it; `None` once gone.
    fn stance(&self, now: SystemTime) -> Option<Stance> {
        let walk = match &self.phase {
            Phase::Resting { at, .. } | Phase::Carried { at } => {
                return Some(Stance {
                    at: *at,
                    walking: None,
                });
            }
            Phase::Walking(w) | Phase::Leaving(w) => w,
            Phase::Gone => return None,
        };
        Some(match walk.segment(now) {
            Some(segment) => Stance {
                at: segment.at(),
                walking: Some(segment),
            },
            None => Stance {
                at: walk.to,
                walking: None,
            },
        })
    }

    /// Advanced to `timing`; `None` once a leaving creature is out.
    ///
    /// A leg that arrived rests where it ended, and the next roam sets off on
    /// the beat ([`Phase::Resting`]): a still tier starts none, while a walk
    /// under way finishes on the wall clock. Unadvanced
    /// past [`STALE_RESUME_GAP_BASE_MS`] its floor was off screen, and on a
    /// resized floor its points are stale: either way it re-seats at its
    /// latest draw rather than replaying the walks it missed.
    pub(crate) fn step(
        &mut self,
        roam: Roam,
        ground: &mut Ground<'_>,
        timing: Timing,
    ) -> Option<Stance> {
        let Timing { now, beat } = timing;
        let size = ground_size(ground.layout);
        if elapsed_ms(now, self.advanced_at) > STALE_RESUME_GAP_BASE_MS || size != self.ground_size
        {
            self.ground_size = size;
            self.phase = match self.phase {
                Phase::Leaving(_) | Phase::Gone => Phase::Gone,
                // the pointer still holds it
                Phase::Carried { at } => Phase::Carried { at },
                _ => Phase::Resting {
                    at: walkable_target(ground.layout, self.seed, self.roams),
                    since: now,
                    walked_ms: 0,
                },
            };
        }
        self.advanced_at = now;
        match &self.phase {
            Phase::Walking(w) if w.segment(now).is_none() => {
                self.phase = Phase::Resting {
                    at: w.to,
                    since: w.started_at,
                    walked_ms: w.profile.duration_ms,
                };
            }
            Phase::Leaving(w) if w.segment(now).is_none() => self.phase = Phase::Gone,
            _ => {}
        }
        if let Phase::Resting {
            at,
            since,
            walked_ms,
        } = self.phase
            && !beat.is_rest()
            && beat.ms().saturating_sub(beat.loop_at(since)) >= walked_ms + self.rest_ms(roam)
        {
            self.roams += 1;
            let dest = walkable_target(ground.layout, self.seed, self.roams);
            self.phase = (dest != at && ground.layout.reachable.reaches(dest))
                .then(|| Walk::plan(at, dest, roam.gait, ground, now))
                .flatten()
                // nowhere to go this time: rest out another spell
                .map_or(
                    Phase::Resting {
                        at,
                        since: now,
                        walked_ms: 0,
                    },
                    Phase::Walking,
                );
        }
        self.stance(now)
    }

    /// This rest's length, about [`Roam::rest_ms`] and seeded per roam so two
    /// creatures never set off in step.
    fn rest_ms(&self, roam: Roam) -> u64 {
        let draw = splitmix64(self.seed ^ self.roams.wrapping_mul(crate::GOLDEN_GAMMA));
        roam.rest_ms / 2 + draw % roam.rest_ms.max(1)
    }
}

/// The longest [`CreatureWalk::rest_ms`] draws about `rest_ms`.
const fn longest_rest(rest_ms: u64) -> u64 {
    rest_ms / 2 + rest_ms
}

/// Per-source gateway mascot facts: its sprite (walk, rest) + the hover-tooltip
/// display name. The ONE place a new gateway registers its creature — `None` for
/// non-gateway sources gates the whole mascot in the sim's `mascot_placements`.
pub(crate) struct GatewayMascotDef {
    pub walk: pixtuoid_core::sprite::format::Walk,
    pub rest: pixtuoid_core::sprite::format::Piece,
    pub display_name: &'static str,
}

pub(crate) fn gateway_mascot_def(source: &str) -> Option<GatewayMascotDef> {
    match source {
        s if s == pixtuoid_core::source::openclaw::SOURCE_NAME => Some(GatewayMascotDef {
            walk: pixtuoid_core::sprite::format::Walk::LobsterWalk,
            rest: pixtuoid_core::sprite::format::Piece::LobsterRest,
            display_name: "OpenClaw",
        }),
        _ => None,
    }
}

/// What a gateway's mascot says about it when hovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayCard {
    /// Human-readable gateway name (e.g. "OpenClaw").
    pub name: &'static str,
    /// WHICH instance of that gateway this is, so a hover over one of two
    /// concurrent lobsters names the one under the cursor. `None` when its
    /// source runs a single instance, whose id means nothing to the user.
    pub instance: Option<String>,
    /// An agent turn is in flight. Keyed on the turn state, NOT the session count
    /// — a single-user gateway holds one persistent session even at rest.
    pub running: bool,
    /// Gateway up but its model backend is failing every turn.
    pub degraded: bool,
    /// Number of sessions the gateway currently holds.
    pub active_sessions: u32,
}

impl GatewayCard {
    /// `key`'s card in `scene`; `None` where it is absent or its source has no
    /// mascot.
    pub(crate) fn of(scene: &SceneState, key: &DaemonInstanceKey) -> Option<Self> {
        let def = gateway_mascot_def(key.source())?;
        let presence = scene.daemon(key.source(), key.instance())?;
        let state = presence.display_state();
        // Per SOURCE: two gateways of ONE daemon need their ports, while two
        // daemon sources already read apart by name and sprite.
        let siblings = scene
            .daemons()
            .filter(|&(s, _, _)| s == key.source())
            .count();
        Some(Self {
            name: def.display_name,
            instance: (siblings > 1).then(|| key.instance().as_str().to_string()),
            running: state == DaemonState::Running,
            degraded: state == DaemonState::Degraded,
            active_sessions: presence.active_sessions,
        })
    }
}

/// The OpenClaw gateway listening on `port`.
#[cfg(test)]
pub(crate) fn openclaw_key(port: &str) -> DaemonInstanceKey {
    DaemonInstanceKey::new(
        pixtuoid_core::source::openclaw::SOURCE_NAME,
        pixtuoid_core::state::DaemonInstanceId::new(port).expect("a port is an id"),
    )
}

/// The walkable cell the mascot enters from / leaves to: the elevator
/// threshold, snapped to floor.
pub(crate) fn mascot_elevator(layout: &SceneLayout) -> Option<Point> {
    snap_point_to_walkable(&layout.walkable, layout.door_threshold)
}

/// The wander seed for ONE daemon instance — folds the source AND the instance id
/// (OpenClaw's resolved gateway port), so N gateways of one source take N different
/// paths and a gateway restarting on its own port keeps its path.
pub(crate) fn mascot_seed(source: &str, instance: &pixtuoid_core::state::DaemonInstanceId) -> u64 {
    source
        .bytes()
        .chain(std::iter::once(b'@'))
        .chain(instance.as_str().bytes())
        .fold(0u64, |h, b| h.wrapping_mul(131).wrapping_add(u64::from(b)))
}

/// How long one mascot may be held at the elevator before its walk-in starts.
/// Gateways that first-sight in the SAME beat leave the door at the same instant
/// from the same cell, so without this they are superimposed at the door. Their
/// seeded walk-in lines usually diverge from there, but not always — a seed pair
/// can draw the same terminus, and then only this separates them.
const MASCOT_ENTER_STAGGER_MS: u64 = 900;

/// The seeded walk-in delay for one mascot — its slice of
/// [`MASCOT_ENTER_STAGGER_MS`]: a mascot's motion never depends on which
/// SIBLINGS exist, its walk-in just starts later.
///
/// The delay comes off an AVALANCHED hash, not `seed % STAGGER` directly: the
/// realistic multi-gateway deployment is CONSECUTIVE ports, whose folded seeds
/// differ by 1, so a raw modulo reads only the low bits and hands adjacent
/// gateways delays a millisecond apart — no stagger at all.
pub(crate) fn mascot_enter_delay(seed: u64) -> u64 {
    pixtuoid_core::id::splitmix64(seed) % MASCOT_ENTER_STAGGER_MS
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use crate::pack::OfficeArt;
    use pixtuoid_core::SceneState;
    use pixtuoid_core::sprite::format::{Piece, Walk};
    use pixtuoid_core::state::{DaemonInstanceId, DaemonLiveness, DaemonPresence};

    use super::*;

    /// One set down where no leg reaches rests at its latest draw, on the
    /// floor it roams, not off it.
    #[test]
    fn a_creature_set_down_out_of_reach_rests_where_it_can_roam() {
        let layout = SceneLayout::compute(160, 96, None).expect("layout fits");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut walk = CreatureWalk::at_home(&layout, 7, now);
        let off = Point {
            x: u16::MAX,
            y: u16::MAX,
        };
        walk.carry(off);
        assert_eq!(walk.stance(now).map(|s| s.at), Some(off), "carried");
        walk.set_down(off, &layout, now);
        let at = walk.stance(now).expect("rests").at;
        assert!(layout.reachable.reaches(at), "{at:?}");
        assert_eq!(at, walkable_target(&layout, 7, 0));
    }

    /// One set down on a desk, a plant, a sofa or a table rests beside it, on
    /// floor no art paints over, as a roam's rest does.
    #[test]
    fn a_creature_set_down_on_furniture_rests_beside_it() {
        use crate::layout::FixtureKind;
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut checked = 0u32;
        for (w, h) in [(160, 96), (192, 80), (240, 180)] {
            let layout = SceneLayout::compute(w, h, None).expect("layout fits");
            for f in layout.fixtures() {
                if !matches!(
                    f.kind,
                    FixtureKind::Desk(_)
                        | FixtureKind::Plant { .. }
                        | FixtureKind::MeetingSofa { .. }
                        | FixtureKind::MeetingTable { .. }
                        | FixtureKind::LoungeCouch
                ) {
                    continue;
                }
                let on = Point {
                    x: f.visual.x + f.visual.width / 2,
                    y: f.visual.y + f.visual.height / 2,
                };
                let mut walk = CreatureWalk::at_home(&layout, 7, now);
                walk.carry(on);
                walk.set_down(on, &layout, now);
                let at = walk.stance(now).expect("rests").at;
                let what = format!("{w}x{h} {:?} at {on:?}: rests at {at:?}", f.kind);
                assert!(
                    layout.is_visually_clear(at) && layout.reachable.reaches(at),
                    "{what}, under art or out of reach"
                );
                let reach = f.visual.width.max(f.visual.height);
                assert!(
                    at.x.abs_diff(on.x) <= reach && at.y.abs_diff(on.y) <= reach,
                    "{what}, not beside it"
                );
                checked += 1;
            }
        }
        assert!(
            checked > 20,
            "the sweep must reach furniture, saw {checked}"
        );
    }
    use crate::anim::{Motion, PAINT_FPS};
    use crate::floor::{FloorInputs, FloorMeta, FloorSession, PetInputs};
    use crate::layout::Size;
    use crate::pathfind::{AStarRouter, point_in_walkable_cell};
    use crate::pet::{Pet, PetState};
    use crate::sim::SimFrame;

    /// One paint, as a painter repaints.
    const PAINT_MS: u64 = 1_000 / PAINT_FPS as u64;

    fn test_pack() -> OfficeArt {
        crate::pack::test_office()
    }

    fn at(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000) + Duration::from_millis(ms)
    }

    /// Every creature's roam with the Full ticks it takes per cycle of its
    /// walk: each pet's, and the mascot's in every state.
    fn roams(pack: &OfficeArt) -> Vec<(Walk, Roam, u64)> {
        let lobster = pack.stride(Walk::LobsterWalk);
        PetKind::ALL
            .iter()
            .map(|k| {
                (
                    k.walk_anim(),
                    Roam::pet(pack.stride(k.walk_anim())),
                    PET_TICKS_PER_STRIDE,
                )
            })
            .chain(
                [
                    (DaemonState::Idle, 2 * MASCOT_TICKS_PER_STRIDE),
                    (DaemonState::Running, MASCOT_TICKS_PER_STRIDE),
                    (DaemonState::Degraded, 3 * MASCOT_TICKS_PER_STRIDE),
                    (DaemonState::Down, MASCOT_TICKS_PER_STRIDE),
                ]
                .map(|(s, ticks)| (Walk::LobsterWalk, Roam::mascot(lobster, s), ticks)),
            )
            .collect()
    }

    /// `walk` stepped once a paint over `[from_ms, to_ms)` at `motion`.
    fn drive(
        walk: &mut CreatureWalk,
        roam: Roam,
        layout: &SceneLayout,
        motion: Motion,
        (from_ms, to_ms): (u64, u64),
    ) -> Vec<Option<Stance>> {
        let mut router = AStarRouter::new();
        let overlay = OccupancyOverlay::new();
        let mut ground = Ground {
            layout,
            router: &mut router,
            overlay: &overlay,
        };
        (from_ms..to_ms)
            .step_by(PAINT_MS as usize)
            .map(|ms| walk.step(roam, &mut ground, motion.timing(at(ms))))
            .collect()
    }

    /// How many walks `stances` set off on.
    fn starts(stances: &[Option<Stance>]) -> usize {
        stances
            .windows(2)
            .filter(|w| {
                matches!(
                    w,
                    [
                        Some(Stance { walking: None, .. }),
                        Some(Stance {
                            walking: Some(_),
                            ..
                        })
                    ]
                )
            })
            .count()
    }

    fn layout(w: u16, h: u16) -> SceneLayout {
        SceneLayout::compute(w, h, None).expect("layout fits")
    }

    /// A planted foot stays planted: each creature cruises exactly one stride
    /// per its own Full ticks, never covers more, nor half a stride
    /// between two paints, where its distance-stepped frames would alias.
    #[test]
    fn no_creature_outpaces_its_stride() {
        // a second of paints
        const PAINTS: usize = (1_000 / PAINT_MS) as usize;
        let pack = test_pack();
        let l = layout(192, 80);
        // the travel progress rounds at each end of a span, on a path about
        // the floor's width and height long
        let rounding = 2
            * u64::from(l.walkable.width() + l.walkable.height())
            * u64::from(OCTILE_STRAIGHT_COST)
            / u64::from(crate::physics::LegProgress::SCALE)
            + 1;
        for (walk, roam, ticks) in roams(&pack) {
            let anim = walk.piece().name();
            let stride = u32::from(pack.stride(walk).get()) * OCTILE_STRAIGHT_COST;
            assert_eq!(
                roam.gait.cruise,
                stride as f32 / (ticks * FULL_TICK_MS) as f32,
                "{anim} cruises off its {ticks}-tick cadence"
            );
            let cadence = u64::from(stride) * PAINTS as u64 * PAINT_MS / (ticks * FULL_TICK_MS);
            let mut walked = 0;
            for seed in 0..4 {
                let mut walk = CreatureWalk::at_home(&l, seed, at(0));
                let stances = drive(&mut walk, roam, &l, Motion::Full, (0, 90_000));
                for run in stances.split(|s| s.is_none_or(|s| s.walking.is_none())) {
                    let segments: Vec<Segment> = run
                        .iter()
                        .filter_map(|s| s.and_then(|s| s.walking))
                        .collect();
                    for pair in segments.windows(2) {
                        let step = pair[1].travelled().saturating_sub(pair[0].travelled());
                        assert!(
                            step <= stride / 2,
                            "{anim} seed {seed}: {step} in one paint of a {stride} stride"
                        );
                    }
                    for span in segments.windows(PAINTS + 1) {
                        let (first, last) = (span[0], span[PAINTS]);
                        let covered = u64::from(last.travelled().saturating_sub(first.travelled()));
                        assert!(
                            covered <= cadence + rounding,
                            "{anim} seed {seed}: {covered} in a second, its cadence allows {cadence}"
                        );
                    }
                    walked += segments.len();
                }
            }
            assert!(walked > 0, "{anim} must walk in the sample");
        }
    }

    /// The walker's own ground: a walking creature is always on a cell its
    /// router routes, and a resting one on clear floor no sprite paints over,
    /// from its home, through its roams, and re-seated after its floor was
    /// off screen; the narrow offices whose walls close off pockets too.
    #[test]
    fn a_creature_walks_routed_ground_and_rests_on_clear_floor() {
        let pack = test_pack();
        let roams = [
            Roam::mascot(pack.stride(Walk::LobsterWalk), DaemonState::Running),
            Roam::pet(pack.stride(PetKind::Cat.walk_anim())),
        ];
        let min = crate::layout::min_layout_size();
        let (mut walking, mut resting) = (0u32, 0u32);
        let back = 30_000 + 2 * STALE_RESUME_GAP_BASE_MS;
        for (w, h) in [
            (min.w, min.h),
            (55, 45),
            (80, 46),
            (120, 46),
            (160, 120),
            (192, 80),
            (240, 180),
        ] {
            let l = layout(w, h);
            for (seed, roam) in (0..8).flat_map(|seed| roams.map(|roam| (seed, roam))) {
                let mut walk = CreatureWalk::at_home(&l, seed, at(0));
                let mut stances = drive(&mut walk, roam, &l, Motion::Full, (0, 30_000));
                stances.extend(drive(
                    &mut walk,
                    roam,
                    &l,
                    Motion::Full,
                    (back, back + 30_000),
                ));
                for Stance {
                    at: p,
                    walking: leg,
                } in stances.into_iter().flatten()
                {
                    if leg.is_some() {
                        assert!(
                            point_in_walkable_cell(&l.walkable, p),
                            "{w}x{h} seed {seed}: walking at {p:?} off the routed ground"
                        );
                        walking += 1;
                    } else {
                        assert!(
                            l.walkable.is_walkable(p.x, p.y) && l.is_visually_clear(p),
                            "{w}x{h} seed {seed}: resting at {p:?}, not clear floor"
                        );
                        resting += 1;
                    }
                }
            }
        }
        assert!(
            walking > 1_000 && resting > 1_000,
            "the sweep must walk and rest: {walking}/{resting}"
        );
    }

    /// Roaming is ambient life: Calm sets off ¼ as often as Full, and Still
    /// never.
    #[test]
    fn a_calmer_tier_roams_a_quarter_as_often_and_a_still_one_never() {
        let pack = test_pack();
        let roam = Roam::mascot(pack.stride(Walk::LobsterWalk), DaemonState::Running);
        let l = layout(192, 80);
        let starts_at = |motion| {
            (0..4)
                .map(|seed| {
                    let mut walk = CreatureWalk::at_home(&l, seed, at(0));
                    starts(&drive(&mut walk, roam, &l, motion, (0, 240_000)))
                })
                .sum::<usize>()
        };
        let (full, calm) = (starts_at(Motion::Full), starts_at(Motion::Calm));
        assert!(full >= 40, "Full must roam in the sample, set off {full}");
        let ratio = full as f32 / calm.max(1) as f32;
        assert!(
            (3.5..=4.5).contains(&ratio),
            "Full set off {full}, Calm {calm}: not ¼ as often"
        );
        assert_eq!(starts_at(Motion::Still), 0, "a still office never sets off");
    }

    /// Stilling the office ends no walk midway: one under way arrives.
    #[test]
    fn a_walk_under_way_finishes_when_the_office_stills() {
        let pack = test_pack();
        let roam = Roam::pet(pack.stride(PetKind::Cat.walk_anim()));
        let l = layout(192, 80);
        let mut walk = CreatureWalk::at_home(&l, 3, at(0));
        let full = drive(&mut walk, roam, &l, Motion::Full, (0, 120_000));
        let off = full
            .iter()
            .position(|s| s.is_some_and(|s| s.walking.is_some()))
            .expect("the cat sets off");
        let mut walk = CreatureWalk::at_home(&l, 3, at(0));
        let start = off as u64 * PAINT_MS;
        drive(&mut walk, roam, &l, Motion::Full, (0, start + PAINT_MS));
        let still = drive(
            &mut walk,
            roam,
            &l,
            Motion::Still,
            (start + PAINT_MS, start + 120_000),
        );
        let last = still.last().copied().flatten().expect("still on the floor");
        assert_eq!(last.walking, None, "the walk arrived");
        assert_eq!(starts(&still), 0, "and no other set off");
        assert!(
            still.iter().flatten().any(|s| s.walking.is_some()),
            "it walked on through the still"
        );
    }

    /// A floor back on screen after a while re-seats its creatures where they
    /// would rest, rather than replaying the walks it missed.
    #[test]
    fn an_off_screen_floor_resumes_without_replaying_its_walks() {
        let pack = test_pack();
        let roam = Roam::pet(pack.stride(PetKind::Dog.walk_anim()));
        let l = layout(192, 80);
        let mut walk = CreatureWalk::at_home(&l, 5, at(0));
        drive(&mut walk, roam, &l, Motion::Full, (0, 60_000));
        let roams = walk.roams;
        let back = 60_000 + 10 * STALE_RESUME_GAP_BASE_MS;
        let resumed = drive(
            &mut walk,
            roam,
            &l,
            Motion::Full,
            (back, back + PET_REST_MS / 2),
        );
        assert_eq!(walk.roams, roams, "no missed roam replayed");
        let first = resumed[0].expect("drawn");
        assert_eq!(
            first.at,
            walkable_target(&l, 5, roams),
            "re-seated at its latest draw"
        );
        assert!(
            resumed
                .iter()
                .all(|s| s.is_some_and(|s| s.walking.is_none())),
            "and rests from there"
        );
    }

    #[test]
    fn longest_rest_bounds_every_rest() {
        let walk = CreatureWalk::at_home(&layout(192, 80), 0, at(0));
        let roam = Roam::pet(test_pack().stride(PetKind::Cat.walk_anim()));
        for roams in 0..2_000 {
            let w = CreatureWalk {
                roams,
                ..walk.clone()
            };
            assert!(w.rest_ms(roam) < PET_LONGEST_REST_MS);
        }
    }

    /// An office stepped as a painter steps it: one floor session, a paint at
    /// a time.
    struct Office {
        session: FloorSession,
        size: Size,
        floor: FloorMeta,
    }

    impl Office {
        fn new(w: u16, h: u16) -> Self {
            Self {
                session: FloorSession::new(std::sync::Arc::new(test_pack())),
                size: Size { w, h },
                floor: FloorMeta::ground(),
            }
        }

        fn frame(
            &mut self,
            scene: &SceneState,
            pet: Option<&Pet>,
            petting: Option<&PetState>,
            ms: u64,
        ) -> SimFrame {
            self.session
                .step(
                    FloorInputs {
                        scene,
                        now: at(ms),
                        floor: self.floor,
                        pets: PetInputs { pet, petting },
                    },
                    self.size,
                )
                .expect("the office lays out")
                .frame
        }
    }

    fn gateway(
        scene: &mut SceneState,
        port: &str,
        liveness: DaemonLiveness,
        entered_ms: u64,
        seen_ms: u64,
    ) {
        scene.insert_daemon(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            DaemonInstanceId::new(port).expect("non-empty"),
            DaemonPresence {
                liveness,
                active_sessions: 0,
                last_seen: at(seen_ms),
                entered_at: at(entered_ms),
                in_flight_turns: Default::default(),
                current_pid: Some(1),
            },
        );
    }

    /// A petted pet holds still where it stands, and walks on from there.
    #[test]
    fn petting_holds_a_pet_where_it_stands() {
        let mut office = Office::new(192, 80);
        let scene = SceneState::default();
        let cat = Pet::defaulted(PetKind::Cat);
        let walking_at = (0..120_000)
            .step_by(PAINT_MS as usize)
            .find(|&ms| {
                office
                    .frame(&scene, Some(&cat), None, ms)
                    .pet
                    .is_some_and(|p| p.anim_name == PetKind::Cat.walk_anim().piece())
            })
            .expect("the cat sets off");
        let held = office
            .frame(&scene, Some(&cat), None, walking_at + PAINT_MS)
            .pet
            .expect("drawn");
        let petting = PetState {
            petted_at: at(walking_at + PAINT_MS),
            kind: PetKind::Cat,
            floor_idx: 0,
        };
        let pet_ms = walking_at + PAINT_MS;
        for ms in (pet_ms..pet_ms + crate::pet::PET_DURATION_MS).step_by(PAINT_MS as usize) {
            let p = office
                .frame(&scene, Some(&cat), Some(&petting), ms)
                .pet
                .expect("drawn");
            assert_eq!(
                (
                    p.anim_name,
                    p.pos.x.abs_diff(held.pos.x) <= 1,
                    p.pos.y.abs_diff(held.pos.y) <= 1
                ),
                (PetKind::Cat.sit_anim(), true, true),
                "petted at +{}ms",
                ms - pet_ms
            );
            assert!(!p.effects.is_empty(), "hearts ride a petted pet");
        }
        let after = office
            .frame(
                &scene,
                Some(&cat),
                None,
                pet_ms + crate::pet::PET_DURATION_MS,
            )
            .pet
            .expect("drawn");
        assert!(
            after.pos.x.abs_diff(held.pos.x) <= 1 && after.pos.y.abs_diff(held.pos.y) <= 1,
            "it resumes where it was held"
        );
    }

    /// Four gateways on one floor rarely crowd: the whole floor is their
    /// destination rule, so they spread. Crowding is box overlap, the
    /// pessimistic metric; the bound sits well above what the whole-floor rule
    /// gives and well below what a small curated spot list gave.
    #[test]
    fn four_gateways_rarely_crowd_now_that_the_whole_floor_is_in_play() {
        const SPRITE_W: u16 = 14;
        const SPRITE_H: u16 = 12;
        const CROWDED_MAX_PCT: usize = 60;
        let pack = test_pack();
        let roam = Roam::mascot(pack.stride(Walk::LobsterWalk), DaemonState::Idle);
        let l = layout(140, 120);
        let tracks: Vec<Vec<Point>> = (0..4u32)
            .map(|i| {
                let id = DaemonInstanceId::new((18901 + i).to_string()).expect("non-empty");
                let mut walk = CreatureWalk::at_home(&l, mascot_seed("openclaw", &id), at(0));
                drive(&mut walk, roam, &l, Motion::Full, (0, 90_000))
                    .into_iter()
                    .map(|s| s.expect("in the room").at)
                    .collect()
            })
            .collect();
        let frames = tracks[0].len();
        let crowded = (0..frames)
            .filter(|&f| {
                (0..4).any(|i| {
                    ((i + 1)..4).any(|j| {
                        let (a, b) = (tracks[i][f], tracks[j][f]);
                        a.x.abs_diff(b.x) < SPRITE_W && a.y.abs_diff(b.y) < SPRITE_H
                    })
                })
            })
            .count();
        let pct = 100 * crowded / frames;
        assert!(
            pct <= CROWDED_MAX_PCT,
            "four gateways crowded in {pct}% of {frames} frames — a small destination set is back"
        );
    }

    /// A pet petted mid-walk holds where it stands, then walks on from there
    /// and rests where its leg was headed: never on the leg's way, off the
    /// clear floor a rest needs.
    #[test]
    fn a_pet_petted_mid_walk_walks_on_from_where_it_was_held() {
        let pack = test_pack();
        let roam = Roam::pet(pack.stride(PetKind::Cat.walk_anim()));
        let l = layout(192, 80);
        let mut router = AStarRouter::new();
        let overlay = OccupancyOverlay::new();
        let mut ground = Ground {
            layout: &l,
            router: &mut router,
            overlay: &overlay,
        };
        let mut walk = CreatureWalk::at_home(&l, 3, at(0));
        let mut ms = 0;
        let held = loop {
            ms += PAINT_MS;
            let s = walk
                .step(roam, &mut ground, Motion::Full.timing(at(ms)))
                .expect("drawn");
            if s.walking.is_some() {
                break s;
            }
            assert!(ms < 120_000, "the cat sets off");
        };
        let until = ms + crate::pet::PET_DURATION_MS;
        while ms < until {
            ms += PAINT_MS;
            walk.hold(at(ms));
            let s = walk
                .step(roam, &mut ground, Motion::Full.timing(at(ms)))
                .expect("drawn");
            assert_eq!(s.at, held.at, "it moved while petted");
        }
        let rest = loop {
            ms += PAINT_MS;
            let s = walk
                .step(roam, &mut ground, Motion::Full.timing(at(ms)))
                .expect("drawn");
            if s.walking.is_none() {
                break s.at;
            }
        };
        assert_ne!(rest, held.at, "it rested where it was held, mid-leg");
        assert!(
            l.walkable.is_walkable(rest.x, rest.y) && l.is_visually_clear(rest),
            "it rested at {rest:?}, off clear floor"
        );
    }

    /// A painter that slows an idle office hears while a creature walks, and
    /// not while it rests.
    #[test]
    fn a_walking_creature_is_told_to_the_painter() {
        let mut office = Office::new(192, 80);
        let scene = SceneState::default();
        let cat = Pet::defaulted(PetKind::Cat);
        let (mut rested, mut walked) = (false, false);
        for ms in (0..120_000).step_by(PAINT_MS as usize) {
            let walking = office
                .frame(&scene, Some(&cat), None, ms)
                .pet
                .is_some_and(|p| p.anim_name == PetKind::Cat.walk_anim().piece());
            let told = office.session.moves_off_beat();
            assert!(!walking || told, "the cat walks unseen at {ms} ms");
            rested |= !walking && !told;
            walked |= walking;
        }
        assert!(
            rested && walked,
            "the cat rests ({rested}) and sets off ({walked})"
        );
    }

    /// Stepped only as each beat turns, as a painter of an idle office steps
    /// it, a creature that rests at a step walks at no instant before the
    /// next: a roam sets off in a step, never between two, so a painter that
    /// sleeps to the beat while nothing moves off it shows every leg from its
    /// first step.
    #[test]
    fn a_resting_creature_sets_off_only_in_a_step() {
        let mut office = Office::new(192, 80);
        let scene = SceneState::default();
        let cat = Pet::defaulted(PetKind::Cat);
        let beat = crate::anim::FULL_TICK_MS;
        let mut set_off = 0;
        for turn in 0..120_000 / beat {
            let ms = turn * beat + 1;
            office.frame(&scene, Some(&cat), None, ms);
            if office.session.moves_off_beat() {
                set_off += 1;
                continue;
            }
            for into in (10..beat).step_by(10) {
                let walks = office
                    .session
                    .floor()
                    .ctx
                    .creatures
                    .values()
                    .any(|w| w.walks_at(at(ms + into)));
                assert!(
                    !walks,
                    "the cat set off {into} ms after the step at {ms} ms"
                );
            }
        }
        assert!(set_off > 0, "the cat never set off");
    }

    /// A walking pet turns to where its leg heads, both ways; a resting one
    /// in an office of idlers sleeps.
    #[test]
    fn a_pet_faces_where_it_walks_and_sleeps_among_idlers() {
        let mut office = Office::new(192, 80);
        let scene = SceneState::default();
        let dog = Pet::defaulted(PetKind::Dog);
        let (mut west, mut east, mut slept) = (0, 0, 0);
        let mut last: Option<(Point, bool)> = None;
        for ms in (0..300_000).step_by(PAINT_MS as usize) {
            let p = office
                .frame(&scene, Some(&dog), None, ms)
                .pet
                .expect("drawn");
            if p.anim_name != PetKind::Dog.walk_anim().piece() {
                slept += usize::from(p.anim_name == PetKind::Dog.sleep_anim());
                last = None;
                continue;
            }
            if let Some((prev, flip)) = last
                && prev.x != p.pos.x
                && flip == p.flip
            {
                assert_eq!(
                    p.flip,
                    p.pos.x < prev.x,
                    "facing away from its walk at +{ms}ms"
                );
                if p.flip { west += 1 } else { east += 1 }
            }
            last = Some((p.pos, p.flip));
        }
        assert!(
            west > 0 && east > 0,
            "the walks sampled head both ways: {west}/{east}"
        );
        assert!(slept > 0, "with every agent idle a resting pet sleeps");
    }

    fn lobsters(f: &SimFrame) -> Vec<(Point, Piece)> {
        f.mascots.iter().map(|m| (m.pos, m.anim_name)).collect()
    }

    /// A gateway's mascot walks in from the elevator once its stagger is up,
    /// and walks out from where it stands when the gateway dies — on past the
    /// roster dropping it, until it is through the door.
    #[test]
    fn a_mascot_walks_in_and_out_through_the_elevator() {
        let mut office = Office::new(192, 80);
        let mut up = SceneState::default();
        gateway(&mut up, "18789", DaemonLiveness::UP, 0, 0);
        let delay = mascot_enter_delay(mascot_seed(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            &DaemonInstanceId::new("18789").expect("non-empty"),
        ));
        let mut long_up = SceneState::default();
        gateway(&mut long_up, "18789", DaemonLiveness::UP, 0, 0);
        assert_eq!(
            lobsters(&Office::new(192, 80).frame(&long_up, None, None, 600_000))[0].1,
            Piece::LobsterRest,
            "first seen long after its walk-in, it rests at home"
        );
        assert!(
            lobsters(&office.frame(&up, None, None, delay.saturating_sub(1))).is_empty(),
            "unseen in its stagger"
        );
        let first = lobsters(&office.frame(&up, None, None, delay));
        let (pos, anim) = first[0];
        let elevator = mascot_elevator(
            &office
                .session
                .floor_mut()
                .ctx
                .frame_layout(192, 80, office.floor.floor_seed)
                .expect("lays out"),
        )
        .expect("an elevator");
        assert_eq!(anim, Piece::LobsterWalk, "it walks in");
        assert!(
            pos.x.abs_diff(elevator.x) <= 8 && pos.y.abs_diff(elevator.y) <= 8,
            "from the elevator: {pos:?} vs {elevator:?}"
        );

        let mut ms = delay;
        let mut stood = pos;
        while ms < 60_000 {
            ms += PAINT_MS;
            stood = lobsters(&office.frame(&up, None, None, ms))[0].0;
        }
        let mut down = SceneState::default();
        gateway(&mut down, "18789", DaemonLiveness::Down, 0, ms);
        ms += PAINT_MS;
        let (out, anim) = lobsters(&office.frame(&down, None, None, ms))[0];
        assert_eq!(anim, Piece::LobsterWalk, "it walks out");
        assert!(
            out.x.abs_diff(stood.x) <= 1 && out.y.abs_diff(stood.y) <= 1,
            "from where it stood: {out:?} vs {stood:?}"
        );

        let gone = SceneState::default();
        let mut walking_out = 0;
        loop {
            ms += PAINT_MS;
            let f = office.frame(&gone, None, None, ms);
            match lobsters(&f)[..] {
                [] => break,
                [(_, anim)] => assert_eq!(anim, Piece::LobsterWalk, "still walking out"),
                _ => panic!("one gateway, one lobster"),
            }
            assert_eq!(
                f.mascots[0].target(),
                None,
                "off the roster it has no card, so it hovers as nothing"
            );
            walking_out += 1;
            assert!(
                walking_out < 120_000 / PAINT_MS,
                "it never reached the elevator"
            );
        }
        assert!(walking_out > 0, "it walks on after the roster drops it");
        assert!(
            office.session.floor().ctx.creatures.is_empty(),
            "and its walk with it"
        );
        assert!(
            lobsters(&office.frame(&down, None, None, ms + PAINT_MS)).is_empty(),
            "a gone gateway does not walk back in"
        );
    }

    /// A gateway back up mid-walk-out drops its walk out and walks in again from
    /// the elevator, once its stagger from the re-anchored `entered_at` is up.
    #[test]
    fn a_gateway_back_up_walks_in_again() {
        let mut office = Office::new(192, 80);
        let delay = mascot_enter_delay(mascot_seed(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            &DaemonInstanceId::new("18789").expect("non-empty"),
        ));
        let mut up = SceneState::default();
        gateway(&mut up, "18789", DaemonLiveness::UP, 0, 0);
        let mut ms = delay;
        while ms < 60_000 {
            ms += PAINT_MS;
            office.frame(&up, None, None, ms);
        }
        let mut down = SceneState::default();
        gateway(&mut down, "18789", DaemonLiveness::Down, 0, ms);
        for _ in 0..3 {
            ms += PAINT_MS;
            assert_eq!(
                lobsters(&office.frame(&down, None, None, ms))[0].1,
                Piece::LobsterWalk
            );
        }

        let back = ms + PAINT_MS;
        let mut again = SceneState::default();
        gateway(&mut again, "18789", DaemonLiveness::UP, back, back);
        assert!(
            lobsters(&office.frame(&again, None, None, back)).is_empty(),
            "its walk out is dropped, and it waits out its stagger"
        );
        let first = lobsters(&office.frame(&again, None, None, back + delay));
        let elevator = mascot_elevator(
            &office
                .session
                .floor_mut()
                .ctx
                .frame_layout(192, 80, office.floor.floor_seed)
                .expect("lays out"),
        )
        .expect("an elevator");
        let (pos, anim) = first[0];
        assert_eq!(anim, Piece::LobsterWalk, "it walks in again");
        assert!(
            pos.x.abs_diff(elevator.x) <= 8 && pos.y.abs_diff(elevator.y) <= 8,
            "from the elevator: {pos:?} vs {elevator:?}"
        );
        assert!(
            office
                .session
                .floor()
                .ctx
                .creatures
                .values()
                .all(|w| !w.leaving()),
            "no stale walk out is left"
        );
    }

    /// A mascot in hand when its gateway dies walks out from where it would
    /// have been set down, and out of the hand: back up, it walks in again
    /// rather than into the hand.
    #[test]
    fn a_carried_mascot_walks_out_of_the_hand_when_its_gateway_dies() {
        use crate::interact::{Figure, Gesture};
        let mut office = Office::new(192, 80);
        let mut up = SceneState::default();
        gateway(&mut up, "18789", DaemonLiveness::UP, 0, 0);
        let mut ms = 60_000;
        office.frame(&up, None, None, ms);
        let layout = office
            .session
            .floor_mut()
            .ctx
            .frame_layout(192, 80, office.floor.floor_seed)
            .expect("lays out");
        // Over a desk, on ground no leg stands on.
        let (over, reach) = layout
            .fixtures()
            .filter(|f| matches!(f.kind, crate::layout::FixtureKind::Desk(_)))
            .find_map(|f| {
                let b = f.visual;
                (b.x..b.x + b.width)
                    .flat_map(|x| (b.y..b.y + b.height).map(move |y| Point { x, y }))
                    .find(|&p| !layout.reachable.reaches(p))
                    .map(|p| (p, b.width.max(b.height)))
            })
            .expect("a desk stands on ground no leg reaches");
        let figure = Figure::Mascot(openclaw_key("18789"));
        office
            .session
            .floor_mut()
            .grip(&Gesture::Lift { figure, at: over });
        ms += PAINT_MS;
        assert_eq!(
            lobsters(&office.frame(&up, None, None, ms))[0].1,
            Piece::LobsterRest,
            "in hand"
        );

        let mut down = SceneState::default();
        gateway(&mut down, "18789", DaemonLiveness::Down, 0, ms);
        ms += PAINT_MS;
        let out = lobsters(&office.frame(&down, None, None, ms));
        let [(pos, anim)] = out[..] else {
            panic!("it walks out, not vanishes: {out:?}");
        };
        assert_eq!(anim, Piece::LobsterWalk, "it walks out");
        assert!(
            pos.x.abs_diff(over.x) <= reach && pos.y.abs_diff(over.y) <= reach,
            "from the floor beside the desk: {pos:?} vs {over:?}"
        );

        let delay = mascot_enter_delay(mascot_seed(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            &DaemonInstanceId::new("18789").expect("non-empty"),
        ));
        let back = ms + PAINT_MS;
        let mut again = SceneState::default();
        gateway(&mut again, "18789", DaemonLiveness::UP, back, back);
        for ms in (back..=back + delay).step_by(PAINT_MS as usize) {
            office.session.floor_mut().grip(&Gesture::Carry(over));
            office.frame(&again, None, None, ms);
        }
        office.session.floor_mut().grip(&Gesture::Carry(over));
        let first = lobsters(&office.frame(&again, None, None, back + delay + PAINT_MS));
        assert_eq!(
            first.first().map(|&(_, anim)| anim),
            Some(Piece::LobsterWalk),
            "back up, it walks in again, not into the hand: {first:?}"
        );
    }

    /// Losing the elevator would lose the mascot: every narrow office keeps it.
    #[test]
    fn every_narrow_layout_still_draws_its_mascot() {
        let min = crate::layout::min_layout_size();
        let mut scene = SceneState::default();
        gateway(&mut scene, "18789", DaemonLiveness::UP, 0, 0);
        let mut checked = 0u32;
        for w in (min.w..min.w + 24).step_by(3) {
            for h in (min.h..min.h + 24).step_by(3) {
                let mut office = Office::new(w, h);
                assert!(
                    !office.frame(&scene, None, None, 30_000).mascots.is_empty(),
                    "{w}x{h} draws no mascot"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 64, "the derived window must visit 8x8 sizes");
    }

    /// A gateway's one `DaemonState` lights exactly its own flag on its card.
    #[test]
    fn a_gateways_state_reaches_its_card() {
        let key = openclaw_key("18789");
        let in_flight = || {
            std::collections::HashMap::from([(
                pixtuoid_core::state::TurnKey::Id("run".into()),
                at(0),
            )])
        };
        for (liveness, in_flight_turns, running, degraded) in [
            (DaemonLiveness::UP, Default::default(), false, false),
            (DaemonLiveness::UP, in_flight(), true, false),
            (
                DaemonLiveness::Up { degraded: true },
                in_flight(),
                false,
                true,
            ),
            (DaemonLiveness::Down, in_flight(), false, false),
        ] {
            let mut scene = SceneState::uniform(16);
            scene.insert_daemon(
                key.source(),
                key.instance().clone(),
                DaemonPresence {
                    liveness,
                    active_sessions: 0,
                    last_seen: at(0),
                    entered_at: at(0),
                    in_flight_turns,
                    current_pid: Some(1),
                },
            );
            let card = GatewayCard::of(&scene, &key).expect("a gateway with a mascot");
            assert_eq!(
                (card.running, card.degraded),
                (running, degraded),
                "{liveness:?}"
            );
        }
    }

    /// Two gateways first seen together walk in apart: their staggers part
    /// them at the door.
    #[test]
    fn two_gateways_seen_together_never_share_a_spot_walking_in() {
        let mut office = Office::new(160, 120);
        let mut scene = SceneState::default();
        for port in ["18901", "18902"] {
            gateway(&mut scene, port, DaemonLiveness::UP, 0, 0);
        }
        let mut both = 0;
        for ms in (0..6_000).step_by(PAINT_MS as usize) {
            let f = office.frame(&scene, None, None, ms);
            // a sibling still in its stagger is a sibling: the port shows
            assert!(
                f.mascots
                    .iter()
                    .all(|m| GatewayCard::of(&scene, &m.key)
                        .is_some_and(|card| card.instance.is_some())),
                "a gateway's port hidden at +{ms}ms"
            );
            if let [(a, _), (b, _)] = lobsters(&f)[..] {
                assert_ne!(a, b, "superimposed at +{ms}ms");
                both += 1;
            }
        }
        assert!(both > 0, "the sample must draw both");
    }

    #[test]
    fn every_registered_daemon_source_has_a_mascot_def() {
        // `gateway_mascot_def` is the ONE per-source daemon table with neither a
        // compile error nor a lockstep test behind it: a new daemon row would decode,
        // key, sweep and roll up correctly and render NO mascot at all.
        use pixtuoid_core::source::registry::REGISTRY;
        for d in REGISTRY.iter().filter(|d| d.is_daemon()) {
            assert!(
                super::gateway_mascot_def(d.name).is_some(),
                "daemon source {:?} has no GatewayMascotDef — it would render no mascot",
                d.name
            );
        }
    }

    /// Whether `f`'s eye (the bundled pack's `e`) sits east of its middle.
    fn faces_east(pack: &OfficeArt, f: &pixtuoid_core::sprite::Frame) -> bool {
        let eye = pack.palette().get('e').flatten();
        let xs: Vec<u32> = (0..f.height())
            .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| f.get(x, y).copied().flatten() == eye)
            .map(|(x, _)| u32::from(x))
            .collect();
        !xs.is_empty() && xs.iter().sum::<u32>() * 2 > xs.len() as u32 * u32::from(f.width())
    }

    /// Every pet's walk faces east, the way `sim::pet_placement` flips it west. (Its
    /// 1x is its master read at 1x: `gen-art --check` holds that.)
    #[test]
    fn every_pet_walk_faces_east() {
        let pack = test_pack();
        for kind in PetKind::ALL {
            let walk = pack.piece(kind.walk_anim().piece());
            for (i, f) in walk.frames().iter().enumerate() {
                assert!(
                    faces_east(&pack, f),
                    "{} {i} faces west",
                    kind.walk_anim().piece().name()
                );
            }
        }
    }

    /// A pet's 1x walk moves its legs: each contact's ground row differs from
    /// the pass a quarter cycle on, or the classic's pet glides on frozen feet.
    #[test]
    fn every_pet_walk_steps_its_legs_at_1x() {
        let pack = test_pack();
        for kind in PetKind::ALL {
            let frames = pack.piece(kind.walk_anim().piece()).frames();
            let ground = |i: usize| {
                let f = &frames[i];
                let y = f.height() - 1;
                (0..f.width())
                    .map(|x| f.get(x, y).copied().flatten().is_some())
                    .collect::<Vec<_>>()
            };
            let quarter = (frames.len() / 4).max(1);
            for contact in [0, frames.len() / 2] {
                assert_ne!(
                    ground(contact),
                    ground((contact + quarter) % frames.len()),
                    "{}'s legs hold still from frame {contact} at 1x",
                    kind.walk_anim().piece().name()
                );
            }
        }
    }

    /// Every pet's walk master faces east too, so both densities face one way.
    #[cfg(feature = "cutaway-assets")]
    #[test]
    fn every_pet_walk_master_faces_east() {
        let pack = test_pack();
        let n = pack.max_density_variant();
        for kind in PetKind::ALL {
            let name = format!("{}@{n}x", kind.walk_anim().piece().name());
            let master = &pack.variants_of(kind.walk_anim().piece())[&n];
            for (i, f) in master.frames().iter().enumerate() {
                assert!(faces_east(&pack, f), "{name} {i} faces west");
            }
        }
    }

    #[test]
    fn gateway_mascot_def_maps_openclaw_and_rejects_others() {
        let def = gateway_mascot_def(pixtuoid_core::source::openclaw::SOURCE_NAME)
            .expect("openclaw must have a mascot def");
        assert_eq!(def.walk, Walk::LobsterWalk);
        assert_eq!(def.rest, Piece::LobsterRest);
        assert_eq!(def.display_name, "OpenClaw");
        assert!(
            gateway_mascot_def("codex").is_none(),
            "codex is not a gateway → no mascot"
        );
        assert!(
            gateway_mascot_def("some-other").is_none(),
            "unknown source → no mascot"
        );
    }

    #[test]
    fn a_wander_target_is_never_parked_under_a_sprite_that_paints_over_it() {
        use crate::layout::{Furniture, Pivot};
        let mut checked = 0u32;
        let min = crate::layout::min_layout_size();
        for &(w, h) in &[
            (min.w, min.h),
            (min.w + 24, min.h + 6),
            (160, 120),
            (192, 160),
            (240, 180),
            (320, 200),
        ] {
            for seed in 0..24u64 {
                let Some(l) = crate::layout::SceneLayout::compute(w, h, None) else {
                    panic!("{w}x{h}: refused at or above the derived floor");
                };
                // Independent rects: the pantry counter from its RUNTIME size, the
                // aquarium from the lounge, both of which the const table cannot give.
                let mut boxes: Vec<(crate::layout::Point, crate::layout::Size)> = Vec::new();
                for wp in &l.waypoints {
                    if wp.kind == crate::layout::WaypointKind::Pantry {
                        let sz = l.pantry_counter_size();
                        boxes.push((
                            crate::layout::anchored_top_left(Pivot::Center, wp.pos, sz.w, sz.h),
                            sz,
                        ));
                    }
                }
                if let Some(t) = l.lounge.and_then(|lo| lo.fish_tank) {
                    let sz = crate::layout::furniture_def(Furniture::FishTank).visual;
                    boxes.push((
                        crate::layout::anchored_top_left(Pivot::Center, t, sz.w, sz.h),
                        sz,
                    ));
                }
                for n in 0..24u64 {
                    let p = walkable_target(&l, seed, n);
                    if !l.walkable.is_walkable(p.x, p.y) {
                        continue;
                    }
                    for (tl, sz) in &boxes {
                        assert!(
                            !(p.x >= tl.x && p.x < tl.x + sz.w && p.y >= tl.y && p.y < tl.y + sz.h),
                            "{w}x{h} seed {seed} cycle {n}: target {p:?} rests inside {tl:?}+{sz:?}"
                        );
                    }
                    checked += 1;
                }
            }
        }
        assert!(
            checked > 500,
            "the sweep must actually reach targets, saw {checked}"
        );
    }

    #[test]
    fn consecutive_gateway_ports_get_spread_walk_in_delays() {
        // CONSECUTIVE ports are the realistic deployment and their folded seeds
        // differ by 1 — the case a raw `seed % STAGGER` gets wrong. Pinned on the
        // REAL seeds, so the fixture can't drift off the production key.
        let src = pixtuoid_core::source::openclaw::SOURCE_NAME;
        let delays: Vec<u64> = ["18901", "18902", "18903", "18904", "18905", "18906"]
            .iter()
            .map(|p| {
                let inst = pixtuoid_core::state::DaemonInstanceId::new(*p).expect("non-empty");
                mascot_enter_delay(mascot_seed(src, &inst))
            })
            .collect();
        let spread = delays.iter().max().unwrap() - delays.iter().min().unwrap();
        assert!(
            spread > MASCOT_ENTER_STAGGER_MS / 3,
            "adjacent ports must spread across the stagger window, got {delays:?}"
        );
        let distinct: std::collections::BTreeSet<_> = delays.iter().collect();
        assert_eq!(
            distinct.len(),
            delays.len(),
            "no two adjacent ports may share a walk-in slice: {delays:?}"
        );
        let seeds: std::collections::BTreeSet<u64> = ["18901", "18902", "18903", "18904"]
            .iter()
            .map(|p| {
                mascot_seed(
                    src,
                    &pixtuoid_core::state::DaemonInstanceId::new(*p).expect("non-empty"),
                )
            })
            .collect();
        assert_eq!(seeds.len(), 4, "each instance must seed differently");
    }
}
