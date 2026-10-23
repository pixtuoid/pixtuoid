//! Pure physics model for character walking.
//!
//! Imports only `pixtuoid_core::AgentId` and `layout::Point`. No router, no terminal deps.
//! All kinematics are f32; screen is ≤ ~4096 px → ≤ ~57k octile, well
//! within f32's 24-bit mantissa.

use pixtuoid_core::AgentId;

/// Why is this walk happening? Determines which cruise speed is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalkIntent {
    /// Agent spawned, walking door → desk. Brisk commute speed.
    Entry,
    /// Session ended, walking desk → door. Brisk commute speed.
    Exit,
    /// Idle wander: desk → waypoint leg. Ambling speed.
    WanderOut,
    /// Idle wander: waypoint → desk leg. Ambling speed.
    WanderBack,
    /// Routing correction snap-back. Brisk commute speed.
    SnapBack,
}

/// Cruise speed for Entry / Exit walks (octile/ms), tuned to keep the effective
/// average walk pace ≈ 4 s while making duration distance-proportional.
pub(crate) const V_CRUISE_COMMUTE: f32 = 0.36;
/// Cruise speed for WanderOut / WanderBack walks (octile/ms) — ambling, tuned in
/// proportion to [`V_CRUISE_COMMUTE`] to preserve the commute-vs-wander contrast.
pub(crate) const V_CRUISE_WANDER: f32 = 0.25;
/// Cruise speed for SnapBack walks (octile/ms) — faster than commute, since the
/// agent visibly *hurries* back. Paired with the higher [`WALK_ACCEL_SNAPBACK`]
/// so short (accel-limited) and far (cruise-limited) snap-backs both stay brisk.
pub(crate) const V_CRUISE_SNAPBACK: f32 = 0.65;
/// Shared acceleration/deceleration constant (octile/ms²), tuned for a ~0.55 s
/// accel ramp (`t_a = v/a`).
pub(crate) const WALK_ACCEL: f32 = 6.5e-4;
/// Acceleration for SnapBack walks (octile/ms²) — ~3× [`WALK_ACCEL`]. Short
/// snap-backs are acceleration-limited (`T = 2·√(L/a)`, cruise-independent), so
/// the urgent return has to *accelerate harder* to stay snappy.
pub(crate) const WALK_ACCEL_SNAPBACK: f32 = 2.0e-3;

/// Minimum per-agent speed multiplier.
pub(crate) const SPEED_MULT_MIN: f32 = 0.85;
/// Maximum per-agent speed multiplier.
pub(crate) const SPEED_MULT_MAX: f32 = 1.20;

/// Minimum arrival settle pause (ms).
pub(crate) const PAUSE_MS_MIN: u64 = 200;
/// Maximum arrival settle pause (ms).
pub(crate) const PAUSE_MS_MAX: u64 = 400;

/// Frozen kinematic profile for one walk leg, computed once at walk-start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WalkProfile {
    /// Accel → cruise → decel total time, **excluding** arrival pause.
    pub duration_ms: u64,
    /// Per-agent arrival settle before the pose flips to seated/at-waypoint.
    pub pause_ms: u64,
    /// Snapshotted A* path length (octile units).
    pub path_len_octile: u32,
    /// Effective cruise speed after `speed_mult` applied.
    pub v_cruise: f32,
    /// Acceleration constant for this leg's intent; stored so `walk_progress`
    /// replays the same kinematics.
    pub accel: f32,
}

/// Deterministic per-agent speed multiplier in [SPEED_MULT_MIN, SPEED_MULT_MAX].
///
/// Uses bits 24..34 of the agent's hash — disjoint from `personality_for`
/// (bits 0..14) and from the low-16 bits used by `stale_resume_gap_ms`.
pub(crate) fn speed_mult(agent_id: AgentId) -> f32 {
    // Finalize with splitmix64 before slicing so distinct agents get distinct
    // speeds (raw FNV-1a doesn't avalanche the high bits).
    let z = pixtuoid_core::id::splitmix64(agent_id.raw());
    let bits = (z >> 24) & 0x3FF;
    let t = bits as f32 / 1023.0;
    SPEED_MULT_MIN + t * (SPEED_MULT_MAX - SPEED_MULT_MIN)
}

/// Deterministic per-agent arrival pause in [PAUSE_MS_MIN, PAUSE_MS_MAX].
///
/// Uses bits 40..52 of the agent's hash — a disjoint window from `speed_mult`,
/// so a fast walker is not always a brief pauser.
pub(crate) fn pause_ms_for(agent_id: AgentId) -> u64 {
    let z = pixtuoid_core::id::splitmix64(agent_id.raw());
    let bits = (z >> 40) & 0xFFF;
    // f64 (not f32 like speed_mult): the output is a u64 ms count, so f64 keeps
    // the bits→[0,1]→ms integer round-trip exact across the full 200..=400 range.
    let t = bits as f64 / 4095.0;
    PAUSE_MS_MIN + (t * (PAUSE_MS_MAX - PAUSE_MS_MIN) as f64) as u64
}

/// The motion SHAPE of a walk leg: the regime the kinematics resolve to for
/// `(length, cruise, accel)`, plus its phase boundaries. `walk_profile` sums it
/// to a duration and `walk_progress` evaluates position against the SAME
/// boundaries, so the two can't disagree — a disagreement renders as a sprite
/// teleporting mid-leg.
enum WalkKinematics {
    /// `L < L_crit`: accel-limited, never reaches cruise. `t_half` = ms to the
    /// peak (mid-leg); total = `2·t_half`.
    Triangular { t_half: f32 },
    /// `L ≥ L_crit`: accel `t_a`, cruise `t_c`, decel `t_a`; total = `2·t_a + t_c`.
    Trapezoidal { t_a: f32, t_c: f32 },
}

impl WalkKinematics {
    /// Resolve the regime + boundaries for a leg of octile length `l` at cruise
    /// `v` and accel `a` (both > 0). `L_crit = v²/a` is the length below which the
    /// leg is accel-limited (triangular).
    fn resolve(l: f32, v: f32, a: f32) -> Self {
        let l_crit = v * v / a;
        if l < l_crit {
            WalkKinematics::Triangular {
                t_half: (l / a).sqrt(),
            }
        } else {
            WalkKinematics::Trapezoidal {
                t_a: v / a,
                t_c: (l - l_crit) / v,
            }
        }
    }

    fn total_ms(&self) -> f32 {
        match self {
            WalkKinematics::Triangular { t_half } => 2.0 * t_half,
            WalkKinematics::Trapezoidal { t_a, t_c } => 2.0 * t_a + t_c,
        }
    }
}

/// How a walker moves: its cruise speed (octile/ms), its acceleration
/// (octile/ms²) and its settle on arrival (ms).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Gait {
    pub(crate) cruise: f32,
    pub(crate) accel: f32,
    pub(crate) pause_ms: u64,
}

/// Freeze a [`WalkProfile`] for one walk leg over `path_len_octile`, with cruise
/// speed picked by `intent` and per-agent speed/pause personality seeded from
/// `agent_id`.
pub(crate) fn walk_profile(
    path_len_octile: u32,
    intent: WalkIntent,
    agent_id: AgentId,
) -> WalkProfile {
    let v_base = match intent {
        WalkIntent::SnapBack => V_CRUISE_SNAPBACK,
        WalkIntent::Entry | WalkIntent::Exit => V_CRUISE_COMMUTE,
        WalkIntent::WanderOut | WalkIntent::WanderBack => V_CRUISE_WANDER,
    };
    let accel = match intent {
        WalkIntent::SnapBack => WALK_ACCEL_SNAPBACK,
        _ => WALK_ACCEL,
    };
    walk_profile_for(
        path_len_octile,
        Gait {
            cruise: v_base * speed_mult(agent_id),
            accel,
            pause_ms: pause_ms_for(agent_id),
        },
    )
}

/// Freeze a [`WalkProfile`] for one walk leg over `path_len_octile` at `gait`.
pub(crate) fn walk_profile_for(path_len_octile: u32, gait: Gait) -> WalkProfile {
    let Gait {
        cruise: v,
        accel: a,
        pause_ms,
    } = gait;
    let duration_ms = if path_len_octile == 0 {
        0u64
    } else {
        // Already in ms: a is octile/ms², so T = sqrt(octile / (octile/ms²)) = ms.
        WalkKinematics::resolve(path_len_octile as f32, v, a)
            .total_ms()
            .round() as u64
    };
    WalkProfile {
        duration_ms,
        pause_ms,
        path_len_octile,
        v_cruise: v,
        accel: a,
    }
}

pub(crate) use progress::{
    LegProgress, OCTILE_DIAGONAL_COST, OCTILE_STRAIGHT_COST, Segment, octile_cost, octile_distance,
    octile_path_len, segment_pixels,
};

/// Walk progress, the segment it puts a walker on, and the distances and
/// pixels in between: the one place that knows its scale and builds either
/// kind of progress.
mod progress {
    use crate::layout::Point;

    /// Steps from one end of a leg, or of a segment, to the other.
    const SCALE: u16 = 1000;

    /// `done` of `total` in steps, rounded down; all of them from
    /// `done >= total` on, a zero `total` included. Every progress built from
    /// a ratio of counts rounds this way.
    fn steps(done: u64, total: u64) -> u16 {
        if done >= total {
            return SCALE;
        }
        // Below SCALE, as done < total.
        (u128::from(done) * u128::from(SCALE) / u128::from(total)) as u16
    }

    /// How far along a walk leg, from [`LegProgress::ZERO`] to
    /// [`LegProgress::DONE`]: the whole walk one profile times.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub(crate) struct LegProgress(u16);

    /// How far along one straight segment of a drawn walk: what
    /// [`walking_position`] lerps by.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub(crate) struct SegmentProgress(u16);

    impl LegProgress {
        pub(crate) const ZERO: Self = Self(0);
        pub(crate) const DONE: Self = Self(SCALE);

        /// `done` of `total`, in [`steps`].
        pub(crate) fn of(done: u64, total: u64) -> Self {
            Self(steps(done, total))
        }

        /// `done` of `total` to the nearest step; a ratio below zero or NaN
        /// (0/0 too) is [`LegProgress::ZERO`], one past one or infinite is
        /// [`LegProgress::DONE`].
        pub(crate) fn nearest(done: f32, total: f32) -> Self {
            Self(((f32::from(SCALE) * done / total).round() as u16).min(SCALE))
        }

        /// The same progress on a leg drawn as the one segment between its own
        /// ends.
        fn straight(self) -> SegmentProgress {
            SegmentProgress(self.0)
        }
    }

    impl SegmentProgress {
        /// `done` of `total`, in [`steps`].
        fn of(done: u64, total: u64) -> Self {
            Self(steps(done, total))
        }
    }

    #[cfg(test)]
    impl LegProgress {
        pub(crate) const SCALE: u16 = SCALE;

        pub(crate) fn get(self) -> u16 {
            self.0
        }
    }

    /// Pure linear interpolation along the walk segment `from → to` at `t`.
    fn walking_position(from: Point, to: Point, t: SegmentProgress) -> Point {
        let (t, scale) = (i32::from(t.0), i32::from(SCALE));
        let dx = i32::from(to.x) - i32::from(from.x);
        let dy = i32::from(to.y) - i32::from(from.y);
        Point {
            x: (i32::from(from.x) + dx * t / scale) as u16,
            y: (i32::from(from.y) + dy * t / scale) as u16,
        }
    }

    /// Every pixel [`walking_position`] lands on along `from → to`, in walk order,
    /// each once: the progress values where either axis's truncated offset steps.
    pub(crate) fn segment_pixels(from: Point, to: Point) -> impl Iterator<Item = Point> {
        let scale = u32::from(SCALE);
        let axis_steps = |a: u16, b: u16| {
            let d = u32::from(a.abs_diff(b));
            (1..=d)
                .map(move |k| SegmentProgress((k * scale).div_ceil(d) as u16))
                .peekable()
        };
        let (mut xs, mut ys) = (axis_steps(from.x, to.x), axis_steps(from.y, to.y));
        let mut next = Some(SegmentProgress(0));
        std::iter::from_fn(move || {
            let t = next?;
            while xs.next_if(|&s| s <= t).is_some() {}
            while ys.next_if(|&s| s <= t).is_some() {}
            next = xs
                .peek()
                .copied()
                .into_iter()
                .chain(ys.peek().copied())
                .min();
            Some(walking_position(from, to, t))
        })
    }

    /// The distance `t` along a leg `length` long is, both in
    /// [`Segment`]'s `travelled` unit.
    fn distance_at(t: LegProgress, length: u32) -> u32 {
        // At most `length`, as t <= DONE.
        (u64::from(t.0) * u64::from(length) / u64::from(SCALE)) as u32
    }

    /// Octile-distance step costs, integer so A* needs no floats — a diagonal
    /// about √2 straights. Shared by [`octile_distance`] and
    /// [A*](crate::pathfind::find_path), so they can't drift.
    pub(crate) const OCTILE_STRAIGHT_COST: u32 = 10;
    pub(crate) const OCTILE_DIAGONAL_COST: u32 = 14;

    /// The octile distance for deltas `(dx, dy)` — THE formula shared by the heuristic of
    /// [A*](crate::pathfind::find_path) (coarse cells) and [`octile_distance`] (pixel Points).
    pub(crate) fn octile_cost(dx: u32, dy: u32) -> u32 {
        OCTILE_DIAGONAL_COST * dx.min(dy) + OCTILE_STRAIGHT_COST * (dx.max(dy) - dx.min(dy))
    }

    pub(crate) fn octile_distance(a: Point, b: Point) -> u32 {
        let dx = (i32::from(a.x) - i32::from(b.x)).unsigned_abs();
        let dy = (i32::from(a.y) - i32::from(b.y)).unsigned_abs();
        octile_cost(dx, dy)
    }

    /// Octile length of a routed polyline — the same metric A* uses, so the
    /// snapshotted length is consistent with per-segment timing. 0 below 2 points.
    pub(crate) fn octile_path_len(path: &[Point]) -> u32 {
        if path.len() < 2 {
            return 0;
        }
        path.windows(2).map(|w| octile_distance(w[0], w[1])).sum()
    }

    /// Where a walker `travelled` along a polyline is: the segment it is on and
    /// how far through it, by cumulative OCTILE distance — the metric A* planned
    /// with, so timing stays uniform along diagonals. Only [`Segment::straight`]
    /// and [`Segment::on_route`] build one, from a [`LegProgress`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Segment {
        /// Segment start point (layout pixels).
        from: Point,
        /// Segment end point (layout pixels).
        to: Point,
        progress: SegmentProgress,
        /// How far into the whole leg the walker is, in octile units: the
        /// tenths of a layout pixel A* measures by.
        travelled: u32,
    }

    impl Segment {
        /// The segment a leg is drawn on when no route bends it: the one between its own ends.
        pub(crate) fn straight(from: Point, to: Point, progress: LegProgress) -> Self {
            Segment {
                from,
                to,
                progress: progress.straight(),
                travelled: distance_at(progress, octile_distance(from, to)),
            }
        }

        /// `progress` of the whole leg along `path` (two or more points), its
        /// last segment taking the end.
        pub(crate) fn on_route(path: &[Point], progress: LegProgress) -> Self {
            let travelled = distance_at(progress, octile_path_len(path));
            let (mut acc, mut i) = (0, 0);
            while i + 2 < path.len() && acc + octile_distance(path[i], path[i + 1]) < travelled {
                acc += octile_distance(path[i], path[i + 1]);
                i += 1;
            }
            let (from, to) = (path[i], path[i + 1]);
            Segment {
                from,
                to,
                progress: SegmentProgress::of(
                    u64::from(travelled - acc),
                    u64::from(octile_distance(from, to)),
                ),
                travelled,
            }
        }

        /// The point it stands on.
        pub(crate) fn at(self) -> Point {
            walking_position(self.from, self.to, self.progress)
        }

        pub(crate) fn from(self) -> Point {
            self.from
        }

        pub(crate) fn to(self) -> Point {
            self.to
        }

        pub(crate) fn travelled(self) -> u32 {
            self.travelled
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use proptest::prelude::*;

        #[test]
        fn of_rounds_down() {
            assert_eq!(LegProgress::of(1, 3), LegProgress(333));
            assert_eq!(LegProgress::of(2, 3), LegProgress(666));
        }

        #[test]
        fn of_is_done_from_the_total_on() {
            for (done, total) in [(3, 3), (5, 3), (0, 0)] {
                assert_eq!(
                    LegProgress::of(done, total),
                    LegProgress::DONE,
                    "{done}/{total}"
                );
            }
        }

        #[test]
        fn nearest_saturates_what_is_not_a_ratio() {
            let nearest = LegProgress::nearest;
            assert_eq!(nearest(f32::NAN, 1.0), LegProgress::ZERO, "NaN");
            assert_eq!(nearest(0.0, 0.0), LegProgress::ZERO, "0/0");
            assert_eq!(nearest(-1.0, 4.0), LegProgress::ZERO, "negative");
            assert_eq!(nearest(f32::INFINITY, 1.0), LegProgress::DONE, "infinite");
            assert_eq!(nearest(1.0, 0.0), LegProgress::DONE, "x/0");
        }

        #[test]
        fn of_survives_a_total_the_scale_would_overflow() {
            assert!(LegProgress::of(u64::MAX - 1, u64::MAX) < LegProgress::DONE);
        }

        #[test]
        fn segment_pixels_are_every_position_a_walk_lands_on() {
            let legs = [
                (Point { x: 3, y: 5 }, Point { x: 6, y: 7 }),
                (Point { x: 20, y: 4 }, Point { x: 13, y: 15 }),
                (Point { x: 8, y: 8 }, Point { x: 8, y: 1 }),
                (Point { x: 2, y: 2 }, Point { x: 2, y: 2 }),
                (Point { x: 0, y: 3 }, Point { x: 2500, y: 4 }),
            ];
            for (a, b) in legs {
                let mut walked: Vec<Point> = (0..=SCALE)
                    .map(|t| walking_position(a, b, SegmentProgress(t)))
                    .collect();
                walked.dedup();
                assert_eq!(
                    segment_pixels(a, b).collect::<Vec<_>>(),
                    walked,
                    "{a:?}->{b:?}"
                );
            }
        }

        proptest! {
            #[test]
            fn of_floors_below_the_total(done in 0u64..u64::MAX, total in 1u64..u64::MAX) {
                prop_assume!(done < total);
                let want = u128::from(done) * u128::from(SCALE) / u128::from(total);
                prop_assert_eq!(u128::from(LegProgress::of(done, total).get()), want);
            }

            #[test]
            fn straight_keeps_the_fraction(done in 0u64..10_000, total in 0u64..10_000) {
                prop_assert_eq!(
                    LegProgress::of(done, total).straight(),
                    SegmentProgress::of(done, total)
                );
            }

            #[test]
            fn nearest_never_passes_done(done in prop::num::f32::ANY, total in prop::num::f32::ANY) {
                prop_assert!(LegProgress::nearest(done, total) <= LegProgress::DONE);
            }

            #[test]
            fn nearest_is_the_rounded_ratio_in_range(
                total in 1.0f32..1.0e6,
                frac in 0.0f32..=1.0,
            ) {
                let done = total * frac;
                let want = (f32::from(SCALE) * done / total).round() as u16;
                prop_assert_eq!(LegProgress::nearest(done, total).get(), want);
            }

            #[test]
            fn the_walk_stays_inside_its_endpoints(
                fx in any::<u16>(), fy in any::<u16>(),
                tx in any::<u16>(), ty in any::<u16>(),
                t in 0..=SCALE,
            ) {
                let p = walking_position(
                    Point { x: fx, y: fy },
                    Point { x: tx, y: ty },
                    SegmentProgress(t),
                );
                prop_assert!(p.x >= fx.min(tx) && p.x <= fx.max(tx));
                prop_assert!(p.y >= fy.min(ty) && p.y <= fy.max(ty));
            }
        }
    }
}

/// Render progress as the nearest [`LegProgress`] to `s(elapsed_ms) / L`.
///
/// - `elapsed_ms < duration_ms`: physics kinematics (accel/cruise/decel).
/// - `elapsed_ms >= duration_ms`: [`LegProgress::DONE`] (also covers pause window).
/// - Zero-length profile: always [`LegProgress::DONE`].
pub(crate) fn walk_progress(p: &WalkProfile, elapsed_ms: u64) -> LegProgress {
    if p.path_len_octile == 0 || elapsed_ms >= p.duration_ms {
        return LegProgress::DONE;
    }

    let l = p.path_len_octile as f32;
    let v = p.v_cruise;
    let a = p.accel;
    let t = elapsed_ms as f32;
    let s = match WalkKinematics::resolve(l, v, a) {
        WalkKinematics::Triangular { t_half } => {
            if t <= t_half {
                0.5 * a * t * t
            } else {
                let dt = 2.0 * t_half - t;
                l - 0.5 * a * dt * dt
            }
        }
        WalkKinematics::Trapezoidal { t_a, t_c } => {
            let t_cruise_end = t_a + t_c;
            let t_total = 2.0 * t_a + t_c;
            if t <= t_a {
                0.5 * a * t * t
            } else if t <= t_cruise_end {
                let d_a = 0.5 * a * t_a * t_a;
                d_a + v * (t - t_a)
            } else {
                let dt = t_total - t;
                l - 0.5 * a * dt * dt
            }
        }
    };

    // The early return above prevents reaching here at t ≥ t_total, but f32
    // rounding can still nudge s slightly outside [0, L] at phase boundaries.
    LegProgress::nearest(s, l)
}

/// Returns `true` when the full walk **and** its arrival pause have elapsed —
/// the pose flips to seated/at-waypoint only after the settle beat completes.
pub(crate) fn walk_arrived(p: &WalkProfile, elapsed_ms: u64) -> bool {
    elapsed_ms >= p.duration_ms + p.pause_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every profile `walk_profile` freezes over a sweep of lengths, intents
    /// and agents.
    fn pinned_profiles() -> impl Iterator<Item = WalkProfile> {
        let intents = [
            WalkIntent::Entry,
            WalkIntent::Exit,
            WalkIntent::WanderOut,
            WalkIntent::WanderBack,
            WalkIntent::SnapBack,
        ];
        [0u32, 1, 7, 10, 99, 100, 1_000, 4_321, 20_000]
            .into_iter()
            .flat_map(move |len| {
                intents.into_iter().flat_map(move |intent| {
                    (0..32).map(move |i| {
                        let agent = AgentId::from_transcript_path(&format!("/p/pin{i}.jsonl"));
                        walk_profile(len, intent, agent)
                    })
                })
            })
    }

    /// The pinned profiles, folded to one digest: a refactor of the kinematics
    /// leaves the people's walks as they were.
    #[test]
    fn the_people_s_walk_profiles_are_pinned() {
        let digest = pixtuoid_core::id::fnv1a(pinned_profiles().flat_map(|p| {
            [
                p.duration_ms,
                p.pause_ms,
                u64::from(p.path_len_octile),
                u64::from(p.v_cruise.to_bits()),
                u64::from(p.accel.to_bits()),
            ]
        }));
        assert_eq!(digest, WALK_PROFILE_DIGEST, "{digest:#018x}");
    }

    const WALK_PROFILE_DIGEST: u64 = 0xd201_4d4b_4be1_5652;

    /// The progress each pinned profile reaches across its walk, one sample past
    /// the end included, folded to one digest: a one-step rounding shift in a
    /// walk position is invisible to the bounds and monotonicity proptests.
    #[test]
    fn the_people_s_walk_progress_is_pinned() {
        const SAMPLES: u64 = 8;
        let digest = pixtuoid_core::id::fnv1a(pinned_profiles().flat_map(|p| {
            (0..=SAMPLES + 1)
                .map(move |k| u64::from(walk_progress(&p, p.duration_ms * k / SAMPLES).get()))
        }));
        assert_eq!(digest, WALK_PROGRESS_DIGEST, "{digest:#018x}");
    }

    const WALK_PROGRESS_DIGEST: u64 = 0x3492_d826_2471_18b0;

    fn id(n: u8) -> AgentId {
        AgentId::from_parts("test", &format!("agent-{n}"))
    }

    #[test]
    fn speed_mult_in_range() {
        for n in 0..=50u8 {
            let m = speed_mult(id(n));
            assert!(
                (SPEED_MULT_MIN..=SPEED_MULT_MAX).contains(&m),
                "agent {n}: speed_mult {m} out of [{SPEED_MULT_MIN}, {SPEED_MULT_MAX}]"
            );
        }
    }

    #[test]
    fn speed_mult_is_deterministic() {
        let a = id(7);
        assert_eq!(
            speed_mult(a),
            speed_mult(a),
            "speed_mult must be deterministic for the same AgentId"
        );
    }

    #[test]
    fn speed_mult_varies_across_agents() {
        let values: Vec<f32> = (0..20u8).map(|n| speed_mult(id(n))).collect();
        let distinct: std::collections::HashSet<u32> = values.iter().map(|v| v.to_bits()).collect();
        assert!(
            distinct.len() >= 5,
            "expected variance in speed_mult across agents, got {distinct:?}"
        );
    }

    #[test]
    fn pause_ms_in_range() {
        for n in 0..=50u8 {
            let p = pause_ms_for(id(n));
            assert!(
                (PAUSE_MS_MIN..=PAUSE_MS_MAX).contains(&p),
                "agent {n}: pause_ms {p} out of [{PAUSE_MS_MIN}, {PAUSE_MS_MAX}]"
            );
        }
    }

    #[test]
    fn pause_ms_independent_of_speed_mult() {
        let pairs: Vec<(f32, u64)> = (0..50u8)
            .map(|n| (speed_mult(id(n)), pause_ms_for(id(n))))
            .collect();
        let speed_mid = f32::midpoint(SPEED_MULT_MIN, SPEED_MULT_MAX);
        let pause_mid = u64::midpoint(PAUSE_MS_MIN, PAUSE_MS_MAX);
        let cross_a = pairs
            .iter()
            .filter(|(s, p)| *s < speed_mid && *p > pause_mid)
            .count();
        let cross_b = pairs
            .iter()
            .filter(|(s, p)| *s >= speed_mid && *p <= pause_mid)
            .count();
        assert!(
            cross_a + cross_b >= 4,
            "pause_ms should be independent of speed_mult; cross-quadrant count too low: {cross_a}+{cross_b}"
        );
    }

    /// L_crit = v²/a. A path shorter than L_crit never reaches cruise.
    fn l_crit(v: f32) -> f32 {
        v * v / WALK_ACCEL
    }

    #[test]
    fn triangular_duration_formula() {
        let v_min = V_CRUISE_COMMUTE * SPEED_MULT_MIN;
        let l_crit_min = l_crit(v_min);
        let l = (l_crit_min / 4.0) as u32; // guaranteed triangular for all agents

        for n in 0..10u8 {
            let profile = walk_profile(l, WalkIntent::Entry, id(n));
            let v = profile.v_cruise;
            let l_crit_v = l_crit(v);
            assert!(
                (l as f32) < l_crit_v,
                "agent {n}: L={l} should be < L_crit={l_crit_v}"
            );
            let t_expected_ms = (2.0 * ((l as f32) / WALK_ACCEL).sqrt()).round() as u64;
            let diff = profile.duration_ms.abs_diff(t_expected_ms);
            assert!(
                diff <= 5,
                "agent {n}: triangular T={} expected≈{t_expected_ms} (diff={diff}ms)",
                profile.duration_ms
            );
        }
    }

    #[test]
    fn trapezoidal_duration_formula() {
        let l: u32 = 1200; // ≫ L_crit for every agent at commute speed

        for n in 0..10u8 {
            let profile = walk_profile(l, WalkIntent::Entry, id(n));
            let v = profile.v_cruise;
            let l_f = l as f32;
            let lc = l_crit(v);
            assert!(
                l_f >= lc,
                "agent {n}: L={l_f} should be >= L_crit={lc} for trapezoidal"
            );
            let t_a = v / WALK_ACCEL;
            let t_c = (l_f - lc) / v;
            let t_expected_ms = (2.0 * t_a + t_c) as u64;
            let diff = profile.duration_ms.abs_diff(t_expected_ms);
            assert!(
                diff <= 5,
                "agent {n}: trapezoidal T={} expected≈{t_expected_ms} (diff={diff}ms)",
                profile.duration_ms
            );
        }
    }

    const EPS: u16 = 2;

    #[test]
    fn progress_at_zero_is_zero() {
        let profile = walk_profile(1000, WalkIntent::Entry, id(0));
        let p = walk_progress(&profile, 0).get();
        assert!(p <= EPS, "p(0) should be ≈0, got {p}");
    }

    #[test]
    fn progress_at_half_duration_triangular_is_near_half() {
        // In the triangular regime, s(T/2) = L/2 exactly (symmetry).
        let v_min = V_CRUISE_COMMUTE * SPEED_MULT_MIN;
        let l_crit_min = l_crit(v_min);
        let l = (l_crit_min / 4.0) as u32;
        let profile = walk_profile(l, WalkIntent::Entry, id(0));
        let half = profile.duration_ms / 2;
        let p = walk_progress(&profile, half);
        assert!(
            LegProgress::of(1, 2).get().abs_diff(p.get()) <= EPS + 10,
            "triangular p(T/2) should be ≈half, got {p:?}"
        );
    }

    #[test]
    fn progress_in_triangular_decel_arm() {
        let v_min = V_CRUISE_COMMUTE * SPEED_MULT_MIN;
        let l_crit_min = l_crit(v_min);
        let l = (l_crit_min / 4.0) as u32; // guaranteed triangular for all agents
        let profile = walk_profile(l, WalkIntent::Entry, id(0));

        // 0.75·T is well past t_half (= T/2), so it lands in the decel arm.
        let t75 = profile.duration_ms * 3 / 4;
        let p75 = walk_progress(&profile, t75);
        assert!(
            (LegProgress::of(1, 2)..LegProgress::DONE).contains(&p75),
            "triangular decel p(0.75T) should be in (half,DONE), got {p75:?}"
        );

        let t60 = profile.duration_ms * 3 / 5;
        let p60 = walk_progress(&profile, t60);
        assert!(
            p75 > p60,
            "decel must keep advancing: p(0.75T)={p75:?} should exceed p(0.6T)={p60:?}"
        );
    }

    #[test]
    fn progress_at_half_duration_trapezoidal() {
        let l: u32 = 1200;
        let profile = walk_profile(l, WalkIntent::Entry, id(0));
        let half = profile.duration_ms / 2;
        let p = walk_progress(&profile, half);
        assert!(
            (LegProgress::of(2, 5)..=LegProgress::of(3, 5)).contains(&p),
            "trapezoidal p(T/2) should be in 2/5..=3/5, got {p:?}"
        );
    }

    #[test]
    fn cruise_plateau_has_constant_delta() {
        let l: u32 = 1200; // trapezoidal, with a clear cruise band
        let profile = walk_profile(l, WalkIntent::Entry, id(3));
        let v = profile.v_cruise;
        let lc = l_crit(v);
        let t_a_ms = (v / WALK_ACCEL) as u64;
        let cruise_start = t_a_ms + 50;
        let cruise_end = profile.duration_ms - t_a_ms - 50;
        assert!(
            cruise_start < cruise_end,
            "need a cruise band: t_a={t_a_ms}ms, T={}ms, L={l}, Lc={lc}",
            profile.duration_ms
        );
        let step = (cruise_end - cruise_start) / 5;
        assert!(step > 0, "cruise band too narrow to sample");
        let samples: Vec<u16> = (0..=5)
            .map(|i| walk_progress(&profile, cruise_start + i * step).get())
            .collect();
        let deltas: Vec<i32> = samples
            .windows(2)
            .map(|w| i32::from(w[1]) - i32::from(w[0]))
            .collect();
        let first = deltas[0];
        for (i, d) in deltas.iter().enumerate() {
            assert!(
                (d - first).abs() <= i32::from(EPS),
                "cruise Δ[{i}]={d} differs from Δ[0]={first} by more than {EPS} — not constant velocity"
            );
        }
    }

    #[test]
    fn arrived_false_before_duration() {
        let profile = walk_profile(600, WalkIntent::Exit, id(4));
        assert!(
            !walk_arrived(&profile, profile.duration_ms - 1),
            "must not arrive before duration_ms elapses"
        );
    }

    #[test]
    fn arrived_false_during_pause() {
        let profile = walk_profile(600, WalkIntent::Exit, id(4));
        assert!(
            !walk_arrived(&profile, profile.duration_ms),
            "must not arrive at duration_ms (still in pause)"
        );
        let mid_pause = profile.duration_ms + profile.pause_ms / 2;
        assert!(
            !walk_arrived(&profile, mid_pause),
            "must not arrive mid-pause"
        );
    }

    #[test]
    fn arrived_true_after_pause() {
        let profile = walk_profile(600, WalkIntent::Exit, id(4));
        let after = profile.duration_ms + profile.pause_ms;
        assert!(
            walk_arrived(&profile, after),
            "must arrive once duration + pause elapsed"
        );
    }

    #[test]
    fn zero_length_no_panic() {
        let profile = walk_profile(0, WalkIntent::SnapBack, id(6));
        let p = walk_progress(&profile, 0);
        assert_eq!(
            p,
            LegProgress::DONE,
            "zero-length walk should report full progress at t=0"
        );
        assert!(
            walk_arrived(&profile, profile.pause_ms),
            "zero-length walk should arrive after its pause"
        );
    }

    #[test]
    fn commute_intents_faster_than_wander_intents() {
        let l: u32 = 800;
        let agent = id(9);
        let commute_dur = walk_profile(l, WalkIntent::Entry, agent).duration_ms;
        let wander_dur = walk_profile(l, WalkIntent::WanderOut, agent).duration_ms;
        assert!(
            commute_dur < wander_dur,
            "Entry ({commute_dur}ms) must be faster than WanderOut ({wander_dur}ms) for same path length"
        );
        let exit_dur = walk_profile(l, WalkIntent::Exit, agent).duration_ms;
        let back_dur = walk_profile(l, WalkIntent::WanderBack, agent).duration_ms;
        assert!(exit_dur < back_dur);
        let snap_dur = walk_profile(l, WalkIntent::SnapBack, agent).duration_ms;
        assert!(snap_dur < wander_dur);
    }

    #[test]
    fn exit_uses_commute_speed() {
        let l: u32 = 800;
        let a = id(0);
        let entry = walk_profile(l, WalkIntent::Entry, a);
        let exit = walk_profile(l, WalkIntent::Exit, a);
        assert_eq!(
            entry.v_cruise.to_bits(),
            exit.v_cruise.to_bits(),
            "Exit and Entry must use the same cruise speed (commute)"
        );
    }

    #[test]
    fn wander_out_and_back_use_same_speed() {
        let l: u32 = 600;
        let a = id(1);
        let out = walk_profile(l, WalkIntent::WanderOut, a);
        let back = walk_profile(l, WalkIntent::WanderBack, a);
        assert_eq!(
            out.v_cruise.to_bits(),
            back.v_cruise.to_bits(),
            "WanderOut and WanderBack must use the same cruise speed"
        );
    }
}

// These sweep the per-frame f32 kinematics across generated (length, intent,
// agent, elapsed) inputs, INCLUDING lengths within rounding of `L_crit = v²/a`,
// where walk_profile's and walk_progress's branch selection could disagree — a
// violation renders as a live sprite teleporting mid-leg, so a failure here is a
// real regression, not flake.
#[cfg(test)]
mod prop {
    use super::*;
    use proptest::prelude::*;

    const ALL_INTENTS: [WalkIntent; 5] = [
        WalkIntent::Entry,
        WalkIntent::Exit,
        WalkIntent::WanderOut,
        WalkIntent::WanderBack,
        WalkIntent::SnapBack,
    ];

    fn prop_id(seed: u64) -> pixtuoid_core::AgentId {
        pixtuoid_core::AgentId::from_parts("prop", &seed.to_string())
    }

    /// Sweep one profile's whole timeline: progress is non-decreasing, exactly
    /// [`LegProgress::DONE`] at/after duration_ms, and never panics.
    fn assert_progress_invariants(p: &WalkProfile) -> Result<(), TestCaseError> {
        let mut prev = walk_progress(p, 0);
        // 32 in-flight samples + 4 past-duration ones (saturation).
        const STEPS: u64 = 32;
        for i in 1..=STEPS + 4 {
            // Saturating: duration_ms can be 0 (zero-length legs).
            let t = p.duration_ms.saturating_mul(i) / STEPS;
            let cur = walk_progress(p, t);
            prop_assert!(
                cur >= prev,
                "progress regressed: p({})={:?} < previous {:?}",
                t,
                cur,
                prev
            );
            prev = cur;
        }
        prop_assert_eq!(
            walk_progress(p, p.duration_ms),
            LegProgress::DONE,
            "p(duration) must saturate"
        );
        Ok(())
    }

    proptest! {
        #[test]
        fn walk_progress_is_monotone_and_saturating(
            l in 0u32..=60_000, intent_idx in 0usize..ALL_INTENTS.len(), seed in any::<u64>(),
        ) {
            let p = walk_profile(l, ALL_INTENTS[intent_idx], prop_id(seed));
            assert_progress_invariants(&p)?;
        }

        // L_crit comes off the profile's own frozen v_cruise/accel — the SAME
        // formula the branch uses — so ±3 octile really straddles the crossover.
        #[test]
        fn regime_boundary_lengths_keep_the_invariants(
            intent_idx in 0usize..ALL_INTENTS.len(), seed in any::<u64>(), delta in -3i64..=3,
        ) {
            let intent = ALL_INTENTS[intent_idx];
            let agent = prop_id(seed);
            // Any length yields the same frozen per-agent v/a pair.
            let probe = walk_profile(1, intent, agent);
            let l_crit = probe.v_cruise * probe.v_cruise / probe.accel;
            let l = (l_crit.round() as i64 + delta).max(0) as u32;
            let p = walk_profile(l, intent, agent);
            assert_progress_invariants(&p)?;
        }
    }
}
