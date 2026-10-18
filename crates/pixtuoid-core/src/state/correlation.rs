//! The reducer's cross-slot correlation maps, extracted into one struct.
//! Don't move these maps onto `AgentSlot`: they span slots and are
//! deliberately not a semver surface.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::AgentId;
use crate::source::Transport;

/// Window in which a Hook event suppresses a later Jsonl event with the same
/// tool_use_id. The suppression is asymmetric by event kind — see
/// [`ToolEventKind`] (#150).
///
// `pub` + `#[doc(hidden)]`: these tuning knobs are visible only so the
// `tests/reducer/` binary (a separate crate) can derive its timing offsets
// from them, while staying off the rendered docs and cargo-semver-checks.
#[doc(hidden)]
pub const HOOK_WINS_WINDOW: Duration = Duration::from_millis(500);

/// How long the trailing events of a life a hook ended, or of a child's
/// [`ChildEnd::Reported`] end, are refused (#242, either transport), and how long a top-level life a hook
/// ended revives only on a hook start. Hook connections are per-connection
/// spawned tasks, so a session's SessionEnd and a trailing Stop/ActivityEnd can
/// be DELIVERED reordered, and the straggler would otherwise synthesize a blank
/// Idle ghost with NO SessionEnd left to remove it. 5s covers same-machine
/// scheduling jitter while never visibly delaying a genuine revival.
#[doc(hidden)]
pub const STRAGGLER_TTL: Duration = Duration::from_secs(5);

/// How long a child's end keeps refusing a transcript re-registration of that
/// child (#244). Sized past the watcher's poll backstop (`DEFAULT_POLL_INTERVAL`),
/// the worst case for a late transcript first-sight, the dead child's echo; a
/// hook past the straggler window is a new life, which
/// [`authorizes`](crate::state::reducer::Reducer::authorizes) readmits under a
/// live parent.
#[doc(hidden)]
pub const CHILD_END_GATE_TTL: Duration = Duration::from_secs(90);

/// How long a child's [`Lifecycle`] is RETAINED after it ended — the
/// `parent_id` memory the #246 parentless-revival re-link reads.
///
/// Deliberately LONGER than [`CHILD_END_GATE_TTL`]: the GATE is bounded by
/// the watcher's poll backstop (`DEFAULT_POLL_INTERVAL`), while the MEMORY must span a TURN gap,
/// which is unbounded. Sharing one clock meant a child idle past
/// [`CHILD_END_GATE_TTL`] came back an ORPHAN, the exact phantom #246 exists
/// to eliminate. The gate is
/// unaffected: [`Correlation::recent_child_end`] applies its OWN
/// freshness check, so a retained-but-stale record gates nothing.
#[doc(hidden)]
pub const CHILD_END_RELINK_TTL: Duration = Duration::from_secs(300);

/// How long a drained Task `tool_use_id` is remembered so a lagged JSONL
/// replay of its Start cannot re-fire `enter_delegating`. After the drain the
/// transcript's batched Start+End pair replays into an EMPTY set, so the
/// first-insert gate reads it as a fresh dispatch and would clobber a Waiting
/// the parent raised in the gap. Sized past the `scan_root` poll backstop
/// (`DEFAULT_POLL_INTERVAL`);
/// a `tool_use_id` is never legitimately re-dispatched, so generosity costs
/// only the tombstone's map entry.
pub(crate) const DRAINED_TASK_TOMBSTONE_TTL: Duration = Duration::from_secs(90);

/// How long an [`AgentEvent::ProofOfLife`](crate::AgentEvent::ProofOfLife) vouch exempts its slot from the
/// staleness sweeps (#220). The probe is ground truth that the OWNING PROCESS
/// is alive, while every `stale_threshold` window only models event silence — so a
/// vouched slot must not be swept on silence alone. Sized 2.5× the watcher's
/// poll cadence (`DEFAULT_POLL_INTERVAL`): two missed polls plus slack.
#[doc(hidden)]
pub const PROOF_OF_LIFE_TTL: Duration = Duration::from_secs(150);

/// The ends and parent link that decide what may create or revive an id.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Lifecycle {
    /// The last APPLIED parent link.
    pub(super) parent_id: Option<AgentId>,
    /// The child's end; its slot's removal stamps one only when none came, and
    /// a reported end replaces an inferred one.
    pub(super) child_end: Option<ChildEnd>,
    /// When a hook ended this top-level life.
    pub(super) hook_ended_at: Option<SystemTime>,
}

impl Lifecycle {
    /// A CHILD's record: linked under a parent, or seen ending as one.
    fn is_child(&self) -> bool {
        self.parent_id.is_some() || self.child_end.is_some()
    }
}

/// How a child's end was learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildEnd {
    /// Its CLI's own SessionEnd for it.
    Reported(SystemTime),
    /// Its slot was removed with no end of its own, after a cascade or a stale
    /// sweep: pixtuoid's guess.
    Inferred(SystemTime),
}

impl ChildEnd {
    fn at(self) -> SystemTime {
        match self {
            Self::Reported(at) | Self::Inferred(at) => at,
        }
    }
}

/// Kind half of a hook-wins dedup record, driving the asymmetric drop matrix
/// (#150): an End record suppresses BOTH JSONL kinds — the tool is over, so a
/// lagged JSONL Start replay would falsely re-Activate and cancel the armed
/// idle debounce — while a Start record suppresses only Starts. A JSONL End
/// must never be eaten by its own tool's dispatch record: when the best-effort
/// PostToolUse hook drops, an eaten Task self-End leaks `active_tasks` for the
/// rest of the session. Don't "simplify" this to exact-kind matching — it
/// would orphan the lagged-pair case (pinned by
/// `late_batched_jsonl_pair_after_delivered_hook_end_is_fully_dropped`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ToolEventKind {
    Start,
    End,
}

/// The reducer-private correlation maps. In/out criterion for a future
/// map: PASSIVE cross-event memory (consulted to interpret a later event)
/// lives here; ARMED actions that mutate the scene on a schedule
/// (`pending_b1_cascades` and its fire pass) stay on `Reducer`.
#[derive(Debug, Default)]
pub(super) struct Correlation {
    /// Recent hook-derived events, so JSONL duplicates can be dropped. A hook
    /// End overwrites its tool's Start entry (kind-in-the-VALUE, not the key),
    /// which is what lets one End record cover the whole lagged JSONL pair.
    pub(super) recent_hook_tool_uses: HashMap<(AgentId, String), (SystemTime, ToolEventKind)>,
    /// One record per id, outliving its slot, read by
    /// [`Reducer::authorizes`](super::reducer::Reducer::authorizes).
    pub(super) lifecycles: HashMap<AgentId, Lifecycle>,
    /// Per-agent set of Task tool_use_ids currently in flight: the parent
    /// stays Delegating while one is, an unnamed child's hook activity is
    /// dropped, and the set's drain may arm b1.
    pub(super) active_tasks: HashMap<AgentId, HashSet<String>>,
    /// Tombstones for Task tool_use_ids whose drain already completed: a
    /// lagged JSONL pair-replay after the drain re-inserts the tuid as a
    /// FRESH first insert (set empty, dedup record GC'd), so the tracker's
    /// first-insert gate alone can't stop `enter_delegating` from clobbering
    /// a Waiting raised since.
    pub(super) recent_task_drains: HashMap<(AgentId, String), SystemTime>,
    /// Sweep-exemption timestamps from [`AgentEvent::ProofOfLife`](crate::AgentEvent::ProofOfLife) (#220):
    /// a slot vouched for within [`PROOF_OF_LIFE_TTL`] is skipped by
    /// `sweep_stale`'s candidate collection.
    pub(super) recent_proof_of_life: HashMap<AgentId, SystemTime>,
    /// The tool calls an agent's `Waiting` is gated on. Singly populated by
    /// inference — the `tool_use_id` that was Active immediately before the
    /// Waiting (a CC permission `Notification` fires mid-tool) — or pushed
    /// explicitly by a `Waiting` that NAMES its gated call (omp
    /// `tool_approval_requested`, which can queue several, #951). When a
    /// member's `ActivityEnd` arrives the permission has been resolved, so
    /// the Waiting resolves instead of lingering until the agent's next tool;
    /// a *parallel* tool ending carries a non-member id and cannot
    /// false-clear a still-pending permission. Codex never populates this —
    /// its tool events carry no `tool_use_id`.
    pub(super) gated_before_waiting: HashMap<AgentId, Vec<Arc<str>>>,
    /// Tool calls already counted into `tool_call_count`, so the SAME logical
    /// call re-observed on the other transport counts once (#951). Not
    /// [`HOOK_WINS_WINDOW`]'s job: omp's transcript writes the call BEFORE the
    /// approval request goes out, so the two Starts are separated by HUMAN
    /// approval latency and no dedup window can span them. The backstop TTL is
    /// [`STALE_WAITING_TIMEOUT`](crate::state::reducer::STALE_WAITING_TIMEOUT) because that bounds how long they can straddle; past
    /// it the cost is one HUD re-count. Nested (not `(AgentId, String)`-keyed)
    /// so the per-Start membership probe borrows `&str` without allocating and
    /// a life's eviction is one `remove`; swept in [`Self::gc_slow`], never the
    /// per-event [`Self::gc`] — with the longest TTL of the maps it is the one
    /// whose retain does real per-event work.
    pub(super) counted_calls: HashMap<AgentId, HashMap<String, SystemTime>>,
    /// When [`Self::gc_slow`] last swept, so its cost is once per
    /// [`COUNTED_SWEEP_INTERVAL`] no matter how often the caller runs — the
    /// bench harness ticks per EVENT, and production ticks per frame.
    counted_swept_at: Option<SystemTime>,
}

/// How often `counted_calls` is TTL-swept. Minutes of slack against an
/// hour-scale ceiling cost nothing; sweeping on every tick re-created the
/// per-event scan `gc_slow` exists to avoid.
const COUNTED_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Freshness under a TTL, clock-regression-safe: `duration_since` returns
/// `Err` when `ts` is in the future, which folds to NOT-fresh. The ONE
/// spelling of the `elapsed < ttl` policy every correlation map and predicate
/// routes through, so the strict-`<` boundary can't drift across the sites.
fn is_fresh(now: SystemTime, ts: SystemTime, ttl: Duration) -> bool {
    now.duration_since(ts).is_ok_and(|d| d < ttl)
}

/// Whether `ttl` has ELAPSED since `ts` — the `>=` (inclusive) complement of
/// [`is_fresh`], clock-regression-safe (a backward clock reads as NOT-yet-
/// elapsed rather than panicking like a hand-rolled `duration_since().unwrap()`
/// would). See [`elapsed_past`] for the strict `>` variant: the boundary case
/// differs and is separately test-pinned, so the two stay distinct.
pub(super) fn elapsed_at_least(now: SystemTime, ts: SystemTime, ttl: Duration) -> bool {
    now.duration_since(ts).is_ok_and(|d| d >= ttl)
}

/// [`elapsed_at_least`] with a STRICT `>`. The exit-grace GC's boundary rides
/// this.
pub(super) fn elapsed_past(now: SystemTime, ts: SystemTime, ttl: Duration) -> bool {
    now.duration_since(ts).is_ok_and(|d| d > ttl)
}

impl Correlation {
    /// An arbitrary in-flight Task tuid for `id`, as the `Arc<str>` the FSM
    /// takes. The CHOICE among several is deliberately unspecified —
    /// `fsm::enter_delegating` only needs proof that *some* delegation is live,
    /// and `active_tasks` is a `HashSet` with no ordering to promise.
    pub(super) fn any_active_task(&self, id: &AgentId) -> Option<Arc<str>> {
        self.active_tasks
            .get(id)
            .and_then(|s| s.iter().next())
            .map(|t| Arc::<str>::from(t.as_str()))
    }

    /// Whether a child end of `id` landed after `since`, however old.
    pub(super) fn child_ended_since(&self, id: AgentId, since: SystemTime) -> bool {
        self.lifecycles
            .get(&id)
            .and_then(|l| l.child_end)
            .is_some_and(|end| end.at() > since)
    }

    /// Whether `id`'s child end is at least `age` old.
    pub(super) fn child_ended_before(&self, id: AgentId, now: SystemTime, age: Duration) -> bool {
        self.lifecycles
            .get(&id)
            .and_then(|l| l.child_end)
            .is_some_and(|end| elapsed_at_least(now, end.at(), age))
    }

    /// Whether `id` ended within [`STRAGGLER_TTL`] by a hook or by a reported
    /// child end: a reordered trailing event of that life is a straggler.
    pub(super) fn ended_within_straggler_window(&self, id: AgentId, now: SystemTime) -> bool {
        self.lifecycles.get(&id).is_some_and(|l| {
            let reported = match l.child_end {
                Some(ChildEnd::Reported(at)) => Some(at),
                _ => None,
            };
            [l.hook_ended_at, reported]
                .into_iter()
                .flatten()
                .any(|ts| is_fresh(now, ts, STRAGGLER_TTL))
        })
    }

    /// Whether only a hook start may revive `id`: a hook ended its top-level
    /// life within [`STRAGGLER_TTL`], and the CLI's hooks carry a start for any
    /// new one.
    pub(super) fn hook_owns_revival(&self, id: AgentId, now: SystemTime) -> bool {
        self.lifecycles.get(&id).is_some_and(|l| {
            l.hook_ended_at
                .is_some_and(|ts| is_fresh(now, ts, STRAGGLER_TTL))
        })
    }

    /// `id`'s end as a child within [`CHILD_END_GATE_TTL`] — the #244 gate's
    /// predicate.
    pub(super) fn recent_child_end(&self, id: AgentId, now: SystemTime) -> Option<ChildEnd> {
        self.lifecycles
            .get(&id)
            .and_then(|l| l.child_end)
            .filter(|end| is_fresh(now, end.at(), CHILD_END_GATE_TTL))
    }

    /// The parent `id` was last linked under, for a parentless new life to
    /// re-join (#246).
    pub(super) fn remembered_parent(&self, id: AgentId) -> Option<AgentId> {
        self.lifecycles.get(&id).and_then(|l| l.parent_id)
    }

    /// Record an APPLIED child→parent link, REVIVING the record: clearing
    /// `child_end` marks this life alive so [`Correlation::gc`] can't
    /// prune the memory while the child still lives. The ONE home of that
    /// coupled write — both `SessionStart` link arms call it, so a future third
    /// link site can't set `parent_id` and forget the clear.
    pub(super) fn link_applied_parent(&mut self, child: AgentId, parent: AgentId) {
        let l = self.lifecycles.entry(child).or_default();
        l.parent_id = Some(parent);
        l.child_end = None;
    }

    /// A session ended: a child's end from either transport, else a hook's end
    /// of a top-level life. Stamped whether or not its slot exists, which
    /// covers the Stop-before-Start reorder (#242/#244). A child's end never
    /// stamps the hook clock: its next turn may be announced by its transcript
    /// alone (Codex, #246), which that clock would refuse.
    /// A child the record already knows ends by its own word even when its
    /// decoder can't tell `as_child` (opencode's `session.deleted`).
    pub(super) fn note_end(&mut self, id: AgentId, by: Transport, as_child: bool, now: SystemTime) {
        let known_child = self.lifecycles.get(&id).is_some_and(Lifecycle::is_child);
        if as_child || known_child {
            self.lifecycles.entry(id).or_default().child_end = Some(ChildEnd::Reported(now));
        } else if by == Transport::Hook {
            self.lifecycles.entry(id).or_default().hook_ended_at = Some(now);
        }
    }

    /// `id`'s slot was removed: a child with no end of its own ends here,
    /// inferred, arming #244-w2 (`get_or_insert` keeps an earlier stamp).
    pub(super) fn note_removed(&mut self, id: AgentId, now: SystemTime) {
        if let Some(l) = self.lifecycles.get_mut(&id)
            && l.is_child()
        {
            l.child_end.get_or_insert(ChildEnd::Inferred(now));
        }
    }

    /// TTL-prune every correlation map.
    ///
    /// A [`Lifecycle`] lives while any clock it carries does: a linked live
    /// child's memory, a child end under [`CHILD_END_RELINK_TTL`] (not the
    /// gate's TTL), or a hook end under [`STRAGGLER_TTL`].
    pub(super) fn gc(&mut self, now: SystemTime) {
        self.recent_hook_tool_uses
            .retain(|_, (ts, _)| is_fresh(now, *ts, HOOK_WINS_WINDOW));
        self.recent_proof_of_life
            .retain(|_, ts| is_fresh(now, *ts, PROOF_OF_LIFE_TTL));
        self.recent_task_drains
            .retain(|_, ts| is_fresh(now, *ts, DRAINED_TASK_TOMBSTONE_TTL));
        self.lifecycles.retain(|_, l| {
            let child = match l.child_end {
                None => l.parent_id.is_some(),
                Some(end) => is_fresh(now, end.at(), CHILD_END_RELINK_TTL),
            };
            child
                || l.hook_ended_at
                    .is_some_and(|ts| is_fresh(now, ts, STRAGGLER_TTL))
        });
    }

    /// The sweeps too costly for the per-event [`Self::gc`]: `counted_calls`
    /// keeps entries for up to [`STALE_WAITING_TIMEOUT`](crate::state::reducer::STALE_WAITING_TIMEOUT), so its retain scans real
    /// state. Self-amortized to [`COUNTED_SWEEP_INTERVAL`] — callers may run
    /// this as often as they like.
    pub(super) fn gc_slow(&mut self, now: SystemTime) {
        if self
            .counted_swept_at
            .is_some_and(|at| is_fresh(now, at, COUNTED_SWEEP_INTERVAL))
        {
            return;
        }
        self.counted_swept_at = Some(now);
        self.counted_calls.retain(|_, calls| {
            calls.retain(|_, ts| is_fresh(now, *ts, super::reducer::STALE_WAITING_TIMEOUT));
            !calls.is_empty()
        });
    }

    /// Whether `tuid` is a live gate member for `id` — THE membership
    /// predicate, so the member-vs-whole-set decisions can't drift per site.
    pub(super) fn gate_matches(&self, id: &AgentId, tuid: &str) -> bool {
        self.gated_before_waiting
            .get(id)
            .is_some_and(|g| g.iter().any(|m| &**m == tuid))
    }

    /// Remove ONE gate member; a parallel approval keeps its own.
    pub(super) fn remove_gate_member(&mut self, id: &AgentId, tuid: &str) {
        if let Some(g) = self.gated_before_waiting.get_mut(id) {
            g.retain(|m| &**m != tuid);
        }
    }

    /// Whether `id` holds a FRESH probe vouch. The single freshness predicate
    /// shared by `sweep_stale`'s own-id exemption and its delegating-ancestor
    /// walk, so the TTL logic can't fork.
    pub(super) fn vouch_fresh(&self, id: &AgentId, now: SystemTime) -> bool {
        self.recent_proof_of_life
            .get(id)
            .is_some_and(|t| is_fresh(now, *t, PROOF_OF_LIFE_TTL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_helpers_pin_inclusive_vs_strict_and_survive_a_backward_clock() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let ttl = Duration::from_millis(1500);
        // Exactly AT the boundary: `>=` fires, strict `>` does NOT — the
        // load-bearing distinction (exit-grace GC uses `>`, grace timers `>=`).
        let at = t0 + ttl;
        assert!(elapsed_at_least(at, t0, ttl), ">= fires at the boundary");
        assert!(
            !elapsed_past(at, t0, ttl),
            "> does NOT fire at the boundary"
        );
        let past = t0 + ttl + Duration::from_millis(1);
        assert!(elapsed_at_least(past, t0, ttl) && elapsed_past(past, t0, ttl));
        let before = t0 + Duration::from_millis(1499);
        assert!(!elapsed_at_least(before, t0, ttl) && !elapsed_past(before, t0, ttl));
        let backward = t0 - Duration::from_secs(10);
        assert!(!elapsed_at_least(backward, t0, ttl) && !elapsed_past(backward, t0, ttl));
    }

    /// A fixed anchor well past the epoch so `t0 + TTL` never under/overflows.
    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000)
    }

    #[test]
    fn straggler_window_expires_at_exactly_its_ttl() {
        let id = AgentId::from_parts("claude-code", "s1");
        let mut corr = Correlation::default();
        corr.note_end(id, Transport::Hook, false, t0());
        let just_inside = t0() + STRAGGLER_TTL - Duration::from_millis(1);
        assert!(corr.hook_owns_revival(id, just_inside));
        assert!(corr.ended_within_straggler_window(id, just_inside));
        assert!(
            !corr.hook_owns_revival(id, t0() + STRAGGLER_TTL),
            "freshness is strict: elapsed == TTL is expired"
        );
        assert!(
            corr.recent_child_end(id, just_inside).is_none(),
            "a hook end of a never-child is not a child end"
        );
    }

    #[test]
    fn child_end_gate_expires_at_exactly_its_ttl() {
        let id = AgentId::from_parts("claude-code", "child");
        let mut corr = Correlation::default();
        corr.note_end(id, Transport::Hook, true, t0());
        let just_inside = t0() + CHILD_END_GATE_TTL - Duration::from_millis(1);
        assert!(corr.recent_child_end(id, just_inside).is_some());
        assert!(
            corr.recent_child_end(id, t0() + CHILD_END_GATE_TTL)
                .is_none(),
            "freshness is strict: elapsed == TTL is expired"
        );
    }

    #[test]
    fn link_applied_parent_sets_the_link_and_revives_an_ended_entry() {
        let child = AgentId::from_parts("claude-code", "child");
        let parent = AgentId::from_parts("claude-code", "parent");
        let mut corr = Correlation::default();
        corr.note_end(child, Transport::Hook, true, t0());
        corr.link_applied_parent(child, parent);
        let entry = &corr.lifecycles[&child];
        assert_eq!(entry.parent_id, Some(parent));
        assert_eq!(
            entry.child_end, None,
            "a re-link must clear the end (revive)"
        );
        assert!(
            corr.recent_child_end(child, t0() + Duration::from_secs(1))
                .is_none(),
            "a revived entry is no longer 'recently ended'"
        );
    }

    #[test]
    fn vouch_freshness_expires_at_exactly_its_ttl() {
        let id = AgentId::from_parts("claude-code", "s1");
        let mut corr = Correlation::default();
        corr.recent_proof_of_life.insert(id, t0());
        let just_inside = t0() + PROOF_OF_LIFE_TTL - Duration::from_millis(1);
        assert!(corr.vouch_fresh(&id, just_inside));
        assert!(
            !corr.vouch_fresh(&id, t0() + PROOF_OF_LIFE_TTL),
            "freshness is strict: elapsed == TTL is expired"
        );
    }

    #[test]
    fn gc_prunes_each_map_at_exactly_its_ttl() {
        let old = AgentId::from_parts("claude-code", "old");
        let young = AgentId::from_parts("claude-code", "young");
        let step = Duration::from_millis(1);
        let mut corr = Correlation::default();
        corr.recent_hook_tool_uses
            .insert((old, "t1".into()), (t0(), ToolEventKind::Start));
        corr.recent_hook_tool_uses
            .insert((young, "t2".into()), (t0() + step, ToolEventKind::End));
        corr.gc(t0() + HOOK_WINS_WINDOW);
        assert!(!corr.recent_hook_tool_uses.contains_key(&(old, "t1".into())));
        assert!(
            corr.recent_hook_tool_uses
                .contains_key(&(young, "t2".into()))
        );

        let mut corr = Correlation::default();
        corr.note_end(old, Transport::Hook, false, t0());
        corr.note_end(young, Transport::Hook, false, t0() + step);
        corr.gc(t0() + STRAGGLER_TTL);
        assert!(!corr.lifecycles.contains_key(&old));
        assert!(corr.lifecycles.contains_key(&young));

        let mut corr = Correlation::default();
        corr.recent_proof_of_life.insert(old, t0());
        corr.recent_proof_of_life.insert(young, t0() + step);
        corr.gc(t0() + PROOF_OF_LIFE_TTL);
        assert!(!corr.recent_proof_of_life.contains_key(&old));
        assert!(corr.recent_proof_of_life.contains_key(&young));

        let mut corr = Correlation::default();
        corr.recent_task_drains.insert((old, "t1".into()), t0());
        corr.recent_task_drains
            .insert((young, "t2".into()), t0() + step);
        corr.gc(t0() + DRAINED_TASK_TOMBSTONE_TTL);
        assert!(!corr.recent_task_drains.contains_key(&(old, "t1".into())));
        assert!(corr.recent_task_drains.contains_key(&(young, "t2".into())));

        let mut corr = Correlation::default();
        corr.note_end(old, Transport::Jsonl, true, t0());
        corr.note_end(young, Transport::Jsonl, true, t0() + step);
        let alive = AgentId::from_parts("claude-code", "alive");
        corr.link_applied_parent(alive, AgentId::from_parts("claude-code", "parent"));
        corr.gc(t0() + CHILD_END_RELINK_TTL);
        assert!(!corr.lifecycles.contains_key(&old));
        assert!(corr.lifecycles.contains_key(&young));
        assert!(
            corr.lifecycles.contains_key(&alive),
            "a linked live child keeps its memory"
        );

        let mut corr = Correlation::default();
        corr.counted_calls
            .entry(old)
            .or_default()
            .insert("t1".into(), t0());
        corr.counted_calls
            .entry(young)
            .or_default()
            .insert("t2".into(), t0() + step);
        corr.gc_slow(t0() + crate::state::reducer::STALE_WAITING_TIMEOUT);
        assert!(
            !corr.counted_calls.contains_key(&old),
            "the swept life's emptied submap is dropped with it"
        );
        assert!(corr.counted_calls[&young].contains_key("t2"));
    }

    #[test]
    fn the_parent_link_outlives_the_end_gate_that_stops_gating() {
        let child = AgentId::from_parts("claude-code", "child");
        let parent = AgentId::from_parts("claude-code", "parent");
        let mut corr = Correlation::default();
        corr.link_applied_parent(child, parent);
        corr.note_end(child, Transport::Hook, true, t0());

        let after_gate = t0() + CHILD_END_GATE_TTL + Duration::from_secs(1);
        corr.gc(after_gate);
        assert!(
            corr.recent_child_end(child, after_gate).is_none(),
            "the end gate must have lapsed"
        );
        assert_eq!(
            corr.remembered_parent(child),
            Some(parent),
            "the re-link memory must survive the gate it does not share a purpose with"
        );

        corr.gc(t0() + CHILD_END_RELINK_TTL);
        assert!(
            !corr.lifecycles.contains_key(&child),
            "the relink memory must still expire at its own budget"
        );
    }

    #[test]
    fn a_removal_stamps_only_a_child() {
        let root = AgentId::from_parts("claude-code", "root");
        let child = AgentId::from_parts("claude-code", "child");
        let mut corr = Correlation::default();
        corr.note_end(root, Transport::Hook, false, t0());
        corr.link_applied_parent(child, root);
        let removed = t0() + Duration::from_secs(4);
        corr.note_removed(root, removed);
        corr.note_removed(child, removed);
        assert!(
            corr.recent_child_end(root, removed).is_none(),
            "a root's removal is no child end"
        );
        assert!(
            corr.recent_child_end(child, removed).is_some(),
            "a child's removal is its end"
        );
    }
}
