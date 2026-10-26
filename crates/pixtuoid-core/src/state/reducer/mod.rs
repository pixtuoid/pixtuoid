use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::AgentId;
use crate::source::decoder::label_prefix_for;
use crate::source::registry::{ChildEndInference, SourceCaps};
use crate::source::{AgentEvent, Transport};
use crate::state::correlation::{Correlation, ToolEventKind, elapsed_at_least, elapsed_past};
use crate::state::{ActivityState, AgentSlot, SceneState, ToolKind, fsm, scope};

#[doc(hidden)]
pub use crate::state::correlation::{
    CHILD_END_GATE_TTL, CHILD_END_RELINK_TTL, HOOK_WINS_WINDOW, PROOF_OF_LIFE_TTL, STRAGGLER_TTL,
};

/// How long to keep an exiting agent's slot alive after `SessionEnd` so the
/// walkout-to-door animation has time to play before the slot is removed.
pub const EXIT_GRACE_WINDOW: Duration = Duration::from_millis(4500);

// A hook end's clock runs from the end, so it must outlast the walkout it
// spans, or an exiting slot's end would expire before its slot does.
const _: () = assert!(EXIT_GRACE_WINDOW.as_nanos() < STRAGGLER_TTL.as_nanos());

/// Defers a drained parent's b1 cascade (#151): one FSEvents coalescing hop,
/// deliberately NOT the `DEFAULT_POLL_INTERVAL` backstop.
#[doc(hidden)]
pub const B1_CASCADE_GRACE: Duration = Duration::from_millis(2500);

/// Active→Idle debounce — hides the flicker of rapid PreToolUse chains.
#[doc(hidden)]
pub const ACTIVE_GRACE_WINDOW: Duration = Duration::from_millis(1500);

/// Stale spread by state: Active silence is death mid-tool; Idle/Waiting must
/// not reap a human on a break or a prompt; unknown-cwd is a seeding ghost.
#[doc(hidden)]
pub const STALE_ACTIVE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
#[doc(hidden)]
pub const STALE_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
#[doc(hidden)]
pub const STALE_WAITING_TIMEOUT: Duration = Duration::from_secs(60 * 60);
#[doc(hidden)]
pub const STALE_UNKNOWN_CWD_TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// For `SourceCaps::short_idle_reap()`. Codex motivates it: its `SessionEnd`
/// hook covers only graceful teardown, its payloads carry no PID, and
/// `ShutdownComplete` never reaches the rollout, and its open-fd probe (`source::fd_probe`) returns nothing
/// off macOS/Linux. CC has a clean-exit hook, so it keeps [`STALE_IDLE_TIMEOUT`].
#[doc(hidden)]
pub const STALE_SHORT_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// `slot`'s source's caps; `None` for an unregistered source.
fn caps_of(slot: &AgentSlot) -> Option<SourceCaps> {
    crate::source::registry::descriptor_for(&slot.source).map(|d| d.caps())
}

/// Which Task end arms b1 for `slot`; an unregistered source keeps the drain.
fn child_end_inference(slot: &AgentSlot) -> ChildEndInference {
    caps_of(slot).map_or(ChildEndInference::TaskDrain, |c| c.child_end_inference)
}

fn stale_threshold(slot: &AgentSlot) -> Duration {
    stale_threshold_with_caps(slot, caps_of(slot))
}

/// Policy half of [`stale_threshold`], split from the registry lookup so caps
/// combinations no registered source has YET are unit-testable with a synthetic
/// [`SourceCaps`].
fn stale_threshold_with_caps(
    slot: &AgentSlot,
    caps: Option<crate::source::registry::SourceCaps>,
) -> Duration {
    if slot.unknown_cwd {
        return STALE_UNKNOWN_CWD_TIMEOUT;
    }
    match &slot.state {
        // A Delegating slot on a source whose delegations are hook-silent
        // (in-process subagents that fire no hooks) emits NOTHING until the
        // dispatch tool's PostToolUse — `last_event_at` freezes for the whole
        // delegation, so a long run would be swept mid-turn on the Active
        // timer. Keyed on the typed `ToolKind::Task`, never the display string:
        // a Generic tool spelling "Delegating" is not a delegation.
        ActivityState::Active {
            kind: ToolKind::Task,
            ..
        } if caps.is_some_and(|c| c.delegations_are_hook_silent) => STALE_WAITING_TIMEOUT,
        ActivityState::Active { .. } => STALE_ACTIVE_TIMEOUT,
        ActivityState::Idle if caps.is_some_and(|c| c.short_idle_reap()) => {
            STALE_SHORT_IDLE_TIMEOUT
        }
        ActivityState::Idle => STALE_IDLE_TIMEOUT,
        ActivityState::Waiting { .. } => STALE_WAITING_TIMEOUT,
    }
}

/// Exactly the registry prefix (a `LabelDeriver`'s empty-cwd fallback) is a
/// [`LabelProvenance::PrefixFallback`](crate::state::LabelProvenance::PrefixFallback) the back-fill may still upgrade;
/// anything else is a real display name. Judged at the mint, not at back-fill
/// time — a bare-prefix Rename always lands on a slot whose source is already
/// set, so the slot's prefix is the right yardstick.
fn classify_rename(label: &str, source: &str) -> crate::state::SlotLabel {
    let prefix = label_prefix_for(source);
    if !prefix.is_empty() && label == prefix {
        crate::state::SlotLabel::prefix_fallback(label)
    } else {
        crate::state::SlotLabel::renamed(label)
    }
}

/// The two RAW wire strings a `ModelInfo` carries, bundled so the two
/// `Option<&str>`s (positionally interchangeable) can't be silently swapped at
/// the call site — the same hazard [`IdentityCtx`] exists to prevent.
#[derive(Clone, Copy)]
struct ModelObservation<'a> {
    model: Option<&'a str>,
    effort: Option<&'a str>,
}

/// Borrowed identity context threaded into slot registration/back-fill, bundled
/// so the two `&str`s (`source`/`session_id`, positionally interchangeable)
/// can't be silently swapped at a call site.
#[derive(Clone, Copy)]
struct IdentityCtx<'a> {
    source: &'a str,
    session_id: &'a str,
    cwd: &'a std::path::Path,
}

/// First-wins identity back-fill (#221): heal EMPTY source/session_id/cwd — an
/// established value is never overwritten. Returns the healed cwd's basename
/// when THIS call healed the cwd; only the SessionStart arm upgrades a fallback
/// label from it, since `Identity` carries no label authority.
fn backfill_identity<'a>(slot: &mut AgentSlot, ctx: IdentityCtx<'a>) -> Option<&'a str> {
    if slot.source.is_empty() && !ctx.source.is_empty() {
        slot.source = Arc::<str>::from(ctx.source);
    }
    if slot.session_id.is_empty() && !ctx.session_id.is_empty() {
        slot.session_id = Arc::<str>::from(ctx.session_id);
    }
    if (slot.unknown_cwd || slot.cwd.as_os_str().is_empty())
        && let Some(base) = ctx
            .cwd
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
    {
        slot.cwd = Arc::<std::path::Path>::from(ctx.cwd);
        slot.unknown_cwd = false;
        return Some(base);
    }
    None
}

/// What a creating or reviving event asks of an id, for [`Reducer::authorizes`].
#[derive(Clone, Copy, PartialEq, Eq)]
struct Attempt {
    kind: AttemptKind,
    /// The one parent both the decision and the link read: the event's own,
    /// else the one the id was last linked under, a known child's next life
    /// (#246).
    parent: Option<AgentId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttemptKind {
    /// A `SessionStart`.
    Start,
    /// A hook `Identity` (#221), which names no parent.
    Identity,
    /// A hook tool or permission event that shows activity, synthesizing a slot.
    HookActivity,
}

/// What [`Reducer::authorizes`] lets an attempt do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Create under the attempt's parent.
    Create,
    /// Fold into the id's slot, and resurrect it in place when `revive`.
    Enrich { revive: bool },
    /// Neither: the id has no slot and may not get one.
    Refuse(Refusal),
}

/// Why [`Reducer::authorizes`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    /// An event of a life that ended within the straggler window (#242).
    Straggler,
    /// A re-registration of a child that already ended (#244).
    ChildEnded,
    /// A transcript sight of a top-level life a hook ended.
    HookOwnsRevival,
}

impl Refusal {
    fn log_start(self, agent_id: AgentId, ctx: IdentityCtx, proposed_parent: Option<AgentId>) {
        let session_id = ctx.session_id;
        match self {
            Self::Straggler => tracing::warn!(
                ?agent_id,
                ?session_id,
                ?proposed_parent,
                "skipped child SessionStart — its SessionEnd already passed \
                 (a late or reordered start, #242)"
            ),
            Self::ChildEnded => tracing::warn!(
                ?agent_id,
                ?session_id,
                ?proposed_parent,
                "skipped child SessionStart — the child already ended (#244)"
            ),
            Self::HookOwnsRevival => tracing::debug!(
                ?agent_id,
                ?session_id,
                "skipped a transcript's first sight — a hook ended this session"
            ),
        }
    }
}

/// Verdict of [`Reducer::preprocess`]. `#[must_use]` because every `Drop` arm
/// exists to STOP the event reaching the dispatch match — silently ignoring one
/// re-opens the leak or the double-apply the pre-pass was added to close.
#[must_use]
enum Preprocessed {
    /// An unnamed child's hook activity, a hook-wins duplicate, or a Start or
    /// Waiting whose call's End already landed (#242).
    Drop,
    /// Survived the pre-pass; carries what task tracking already applied, so
    /// the general arms can skip work the tracker has done.
    Dispatch(TaskTracking),
}

struct TaskTracking {
    /// An `ActivityEnd` drained a tracked Task, applying its transition.
    handled_by_task_tracking: bool,
    /// An `ActivityStart` was a Task dispatch, which the tracker owns — replays
    /// included.
    handled_by_task_start: bool,
}

/// What an end names, for [`Reducer::apply_end`].
#[derive(Clone, Copy)]
enum Ended<'a> {
    /// A call's own end.
    Call(&'a str),
    /// A tool end naming no call: its wire carries none (Reasonix's
    /// PostToolUse), or its decoder drops it (Cursor, deliberately).
    Unnamed,
    /// An [`AgentEvent::TurnEnd`].
    Turn,
}

/// The event coordinator: folds `AgentEvent`s into `SceneState`, owning the
/// cross-slot correlation maps, the Active→Idle debounce, and the stale/exit
/// sweeps.
#[derive(Debug, Default)]
pub struct Reducer {
    corr: Correlation,
    /// Parents whose last Task drained on a source that infers child ends from
    /// one, or whose id-less Task ended on an [`ChildEndInference::AnyTaskEnd`]
    /// one, awaiting the deferred b1 cascade
    /// ([`B1_CASCADE_GRACE`]) — fired only if `active_tasks` is STILL empty at
    /// fire time, which is how a Task insert inside the grace defuses it
    /// (#151).
    pending_b1_cascades: HashMap<AgentId, SystemTime>,

    next_label_n: u32,
}

impl Reducer {
    /// A reducer with empty correlation state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Run the GC + exit-sweep + Active→Idle debounce expiry without applying
    /// an event. Must be called periodically so exiting slots are reclaimed and
    /// pending-idle timers fire even when no event arrives to drive `apply`.
    pub fn tick(&mut self, scene: &mut SceneState, now: SystemTime) {
        self.corr.gc(now);
        self.corr.gc_slow(now);
        self.sweep_exited(scene, now);
        self.expire_pending_idles(scene, now);
        self.fire_pending_b1_cascades(scene, now);
        self.sweep_stale(scene, now);
        // Reap entries for agents that never got a SessionStart — a Task event
        // can arrive before JSONL creates the slot.
        self.corr
            .active_tasks
            .retain(|id, _| scene.agents.contains_key(id));
        self.corr
            .gated_before_waiting
            .retain(|id, _| scene.agents.contains_key(id));
        self.pending_b1_cascades
            .retain(|id, _| scene.agents.contains_key(id));
    }

    /// Mark exiting every non-exiting slot whose source is NOT connected, then
    /// cascade to its subtree. Driven by the Sources panel's DISCONNECT toggle —
    /// an explicit user action, not transcript content, so it does NOT violate
    /// the "content never drives lifecycle" invariant. Keying on the COMPLEMENT
    /// of the connected set also evicts a BLANK-source slot synthesized for an
    /// identity-less hook event that slipped the per-event gate; already-exiting
    /// slots stay untouched so a re-reconcile can't reset their walkout clock.
    pub fn reconcile_connected(
        &self,
        scene: &mut SceneState,
        connected: &HashSet<String>,
        now: SystemTime,
    ) {
        let ids: Vec<AgentId> = scene
            .agents
            .values()
            .filter(|s| s.exiting_at.is_none() && !connected.contains(s.source.as_ref()))
            .map(|s| s.agent_id)
            .collect();
        for id in ids {
            scope::cascade_exit(scene, id, scope::StampRoot::Yes, now);
        }
    }

    /// Fold one `AgentEvent` into the scene at `now`, tagged with the
    /// `Transport` it arrived on (load-bearing for hook-wins dedup).
    pub fn apply(
        &mut self,
        scene: &mut SceneState,
        event: AgentEvent,
        now: SystemTime,
        from: Transport,
    ) {
        let id = event.agent_id();
        let Preprocessed::Dispatch(tracking) = self.preprocess(scene, &event, id, now, from) else {
            return;
        };

        match event {
            AgentEvent::SessionStart {
                agent_id,
                source,
                session_id,
                cwd,
                parent_id,
            } => self.apply_session_start(
                scene,
                agent_id,
                IdentityCtx {
                    source: &source,
                    session_id: &session_id,
                    cwd: &cwd,
                },
                parent_id,
                from,
                now,
            ),
            // The tracker owns every Task-dispatch Start, replays included.
            AgentEvent::ActivityStart { .. } if tracking.handled_by_task_start => {}
            AgentEvent::ActivityStart {
                agent_id,
                tool_use_id,
                detail,
            } => self.apply_activity_start(scene, agent_id, tool_use_id, detail, from, now),
            // The pre-pass tracker already drained a tracked Task with this End:
            // the drain applied the transition itself, so re-running it would
            // restart Delegating — resetting `state_started_at` — whenever
            // parallel Tasks remain.
            AgentEvent::ActivityEnd { .. } if tracking.handled_by_task_tracking => {}
            AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id,
            } => {
                let ended = tool_use_id.as_deref().map_or(Ended::Unnamed, Ended::Call);
                self.apply_end(scene, agent_id, ended, from, now);
            }
            AgentEvent::TurnEnd { agent_id } => {
                self.apply_end(scene, agent_id, Ended::Turn, from, now);
            }
            AgentEvent::Waiting {
                agent_id,
                reason,
                tool_use_id,
            } => {
                self.apply_waiting(scene, agent_id, &reason, tool_use_id.as_deref(), now);
            }
            AgentEvent::Rename { agent_id, label } => {
                Self::apply_rename(scene, agent_id, &label, now);
            }
            AgentEvent::SessionEnd { agent_id, as_child } => {
                self.apply_session_end(scene, agent_id, as_child, from, now);
            }
            AgentEvent::ProofOfLife { agent_id } => {
                self.apply_proof_of_life(scene, agent_id, now);
            }
            AgentEvent::Identity {
                agent_id,
                source,
                session_id,
                cwd,
                pid,
            } => {
                // Defaulted HERE because `IdentityCtx` BORROWS the path,
                // mirroring the `SessionStart` arm above.
                let cwd = cwd.as_deref().unwrap_or_else(|| std::path::Path::new(""));
                self.apply_identity(
                    scene,
                    agent_id,
                    IdentityCtx {
                        source: &source,
                        session_id: &session_id,
                        cwd,
                    },
                    pid,
                    from,
                    now,
                );
            }
            AgentEvent::ModelInfo {
                agent_id,
                model,
                effort,
            } => Self::apply_model_info(
                scene,
                agent_id,
                ModelObservation {
                    model: model.as_deref(),
                    effort: effort.as_deref(),
                },
                now,
            ),
            AgentEvent::Usage {
                agent_id,
                fresh_tokens,
            } => Self::apply_usage(scene, agent_id, fresh_tokens, now),
        }
    }

    /// The pre-pass every event runs before dispatch. ORDER IS LOAD-BEARING:
    /// dedup before task tracking (#150), else a duplicate re-fires
    /// `enter_delegating` onto a Waiting parent; fire b1 cascades AFTER tracking,
    /// or a cancelling dispatch at the grace boundary evicts its own live subtree.
    fn preprocess(
        &mut self,
        scene: &mut SceneState,
        event: &AgentEvent,
        id: AgentId,
        now: SystemTime,
        from: Transport,
    ) -> Preprocessed {
        self.corr.gc(now);
        self.sweep_exited(scene, now);
        self.expire_pending_idles(scene, now);

        if from == Transport::Hook {
            self.synthesize_hook_registration(scene, event, id, now);
        }

        // `refresh_lineage` stamps the ACTOR too, yet the per-arm `last_event_at`
        // writes are NOT droppable: Rename and SessionStart never reach here.
        if event.refreshes_lineage() {
            scope::refresh_lineage(scene, id, now);
        }

        if from == Transport::Hook
            && scene
                .agents
                .get(&id)
                .and_then(caps_of)
                .is_some_and(|c| c.child_hooks_unnamed)
            && self.suppress_subagent_leak(scene, event, id, now)
        {
            return Preprocessed::Drop;
        }

        if from == Transport::Jsonl
            && let Some((kind, tuid)) = event_tool_use_id(event)
            && let Some((_, recorded)) =
                self.corr.recent_hook_tool_uses.get(&(id, tuid.to_string()))
        {
            // Kind-ASYMMETRIC (#150): a hook Start never eats a JSONL
            // End, the only completion signal left if PostToolUse drops.
            if !(*recorded == ToolEventKind::Start && kind == ToolEventKind::End) {
                return Preprocessed::Drop;
            }
        }

        // Per-connection hook delivery can decode a call's End before its Start
        // (#242); that Start would open a call that already returned.
        if from == Transport::Hook
            && let AgentEvent::ActivityStart {
                tool_use_id: Some(tuid),
                detail,
                ..
            } = event
            && self
                .corr
                .recent_hook_tool_uses
                .get(&(id, tuid.clone()))
                .is_some_and(|(_, kind)| *kind == ToolEventKind::End)
        {
            // The call did run.
            self.count_call(scene, id, Some(tuid), detail, now);
            return Preprocessed::Drop;
        }

        // Likewise a wait for a call whose hook End already landed, from a
        // reordered hook (#242) or a lagging watcher: its answer came first.
        if let AgentEvent::Waiting {
            tool_use_id: Some(tuid),
            ..
        } = event
            && self
                .corr
                .recent_hook_tool_uses
                .get(&(id, tuid.clone()))
                .is_some_and(|(_, kind)| *kind == ToolEventKind::End)
        {
            return Preprocessed::Drop;
        }

        // Gated on the slot EXISTING: a synthesis REFUSED for desk exhaustion
        // would leave a record that dedup-eats the JSONL registration's Start.
        if from == Transport::Hook
            && scene.agents.contains_key(&id)
            && let Some((kind, tuid)) = event_tool_use_id(event)
        {
            self.corr
                .recent_hook_tool_uses
                .insert((id, tuid.to_string()), (now, kind));
        }

        let tracking = self.track_active_tasks(scene, event, now);

        self.fire_pending_b1_cascades(scene, now);

        Preprocessed::Dispatch(tracking)
    }

    /// The `ActivityStart` arm, for a Start the pre-pass tracker left to it.
    ///
    /// A Start whose id is a GATED member is the same call re-observed across
    /// an approval round (#951): on Jsonl it is the transcript's Start landing
    /// while the human still decides — count it, keep Waiting, and drop its
    /// richer detail, since a Waiting slot has none to set and the hook resume
    /// re-labels from the wire's `toolName` (a label-only cost, pinned by
    /// `the_approval_resume_labels_from_the_wire_tool_name`). On Hook it is the
    /// approval resume — return to Active.
    ///
    /// An UNGATED Start moots the gate list, except while the slot still WAITS
    /// (a parallel auto-approved tool must not strip a pending approval) or for
    /// an already-counted id — a late transcript twin whose approval round may
    /// have queued siblings.
    fn apply_activity_start(
        &mut self,
        scene: &mut SceneState,
        agent_id: AgentId,
        tool_use_id: Option<String>,
        detail: Option<crate::source::ToolDetail>,
        from: Transport,
        now: SystemTime,
    ) {
        let gated = tool_use_id
            .as_deref()
            .is_some_and(|t| self.corr.gate_matches(&agent_id, t));
        if gated && from == Transport::Jsonl {
            self.count_call(scene, agent_id, tool_use_id.as_deref(), &detail, now);
            return;
        }
        if gated {
            if let Some(t) = tool_use_id.as_deref() {
                self.corr.remove_gate_member(&agent_id, t);
            }
        } else {
            let already_counted = tool_use_id.as_deref().is_some_and(|t| {
                self.corr
                    .counted_calls
                    .get(&agent_id)
                    .is_some_and(|calls| calls.contains_key(t))
            });
            let waiting = scene
                .agents
                .get(&agent_id)
                .is_some_and(|s| matches!(s.state, ActivityState::Waiting { .. }));
            if !waiting && !already_counted {
                self.corr.gated_before_waiting.remove(&agent_id);
            }
        }
        self.count_call(scene, agent_id, tool_use_id.as_deref(), &detail, now);
        if let Some(slot) = scene.agents.get_mut(&agent_id) {
            // Derive the category from the typed detail BEFORE it is erased to
            // the HUD display string.
            let kind = detail
                .as_ref()
                .map_or(ToolKind::Other, ToolKind::from_detail);
            fsm::enter_active(
                slot,
                tool_use_id.map(|s| Arc::<str>::from(s.as_str())),
                detail.map(|d| Arc::<str>::from(d.display())),
                kind,
                now,
            );
        }
    }

    /// Count a tool call ONCE per (agent, call id) life; the id-less path
    /// (Codex JSONL) counts every time.
    fn count_call(
        &mut self,
        scene: &mut SceneState,
        agent_id: AgentId,
        tool_use_id: Option<&str>,
        detail: &Option<crate::source::ToolDetail>,
        now: SystemTime,
    ) {
        if detail.as_ref().is_some_and(|d| d.is_task()) {
            return;
        }
        let Some(slot) = scene.agents.get_mut(&agent_id) else {
            return;
        };
        match tool_use_id {
            Some(t) => {
                let calls = self.corr.counted_calls.entry(agent_id).or_default();
                if !calls.contains_key(t) {
                    calls.insert(t.to_string(), now);
                    slot.tool_call_count += 1;
                }
            }
            None => slot.tool_call_count += 1,
        }
    }

    /// Whether `ended` answers the slot's wait: a call's end only when it is
    /// the gated call, on any transport; of ends naming no call, only a hook's,
    /// since a transcript line may race the hook's prompt.
    fn wait_resolved_by(
        &self,
        scene: &SceneState,
        agent_id: AgentId,
        ended: Ended<'_>,
        from: Transport,
    ) -> bool {
        let is_waiting = matches!(
            scene.agents.get(&agent_id).map(|s| &s.state),
            Some(ActivityState::Waiting { .. })
        );
        is_waiting
            && match ended {
                // Only the *gated* call's end, so a parallel tool's can't
                // false-clear a pending permission.
                Ended::Call(tuid) => self.corr.gate_matches(&agent_id, tuid),
                // A pending approval BLOCKS the turn, so its end means the
                // prompt was answered.
                Ended::Turn => from == Transport::Hook,
                // Reasonix's prompt names no call, nor does its PostToolUse.
                Ended::Unnamed => from == Transport::Hook,
            }
    }

    /// The `ActivityEnd` arm, for an End the pre-pass tracker left to it, and
    /// the `TurnEnd` arm.
    fn apply_end(
        &mut self,
        scene: &mut SceneState,
        agent_id: AgentId,
        ended: Ended<'_>,
        from: Transport,
        now: SystemTime,
    ) {
        let resolves_wait = self.wait_resolved_by(scene, agent_id, ended, from);
        match ended {
            // A call's End retires ITS gate whether or not it resolved a wait
            // (a denial, or a parallel tool finishing past its own gate).
            Ended::Call(t) => self.corr.remove_gate_member(&agent_id, t),
            // The turn that answered the wait is over: every gate is stale.
            Ended::Turn if resolves_wait => {
                self.corr.gated_before_waiting.remove(&agent_id);
            }
            // It answered the wait without naming which gate: every gate is
            // stale.
            Ended::Unnamed if resolves_wait => {
                self.corr.gated_before_waiting.remove(&agent_id);
            }
            Ended::Turn | Ended::Unnamed => {}
        }
        // A delegating parent's own parallel tool ending must not settle it to
        // Idle: nothing would restore Delegating for the rest of the run.
        let delegating_tuid = self.corr.any_active_task(&agent_id);
        if let Some(slot) = scene.agents.get_mut(&agent_id) {
            let untracked_task = matches!(
                slot.state,
                ActivityState::Active {
                    kind: ToolKind::Task,
                    tool_use_id: None,
                    ..
                }
            );
            if untracked_task && child_end_inference(slot).dispatch_end_ends_child(false) {
                self.pending_b1_cascades.insert(agent_id, now);
            }
            if matches!(slot.state, ActivityState::Active { .. }) || resolves_wait {
                match delegating_tuid {
                    Some(tuid) => fsm::enter_delegating(slot, Some(tuid), now),
                    None => fsm::arm_pending_idle(slot, now),
                }
            }
            slot.last_event_at = now;
        }
    }

    fn apply_waiting(
        &mut self,
        scene: &mut SceneState,
        agent_id: AgentId,
        reason: &str,
        tool_use_id: Option<&str>,
        now: SystemTime,
    ) {
        if let Some(slot) = scene.agents.get_mut(&agent_id) {
            if let Some(t) = tool_use_id {
                // The wire NAMED the gated call (#951): push it, keeping any
                // sibling gate a parallel approval already queued.
                let gates = self.corr.gated_before_waiting.entry(agent_id).or_default();
                if !gates.iter().any(|m| &**m == t) {
                    gates.push(Arc::from(t));
                }
            } else if let ActivityState::Active {
                tool_use_id: Some(tuid),
                ..
            } = &slot.state
            {
                // REPLACE, not push: an inferred member is a guess, and stale
                // guesses would accrete with nothing to clear them.
                self.corr
                    .gated_before_waiting
                    .insert(agent_id, vec![tuid.clone()]);
            } else if !matches!(slot.state, ActivityState::Waiting { .. }) {
                // A re-notified Waiting slot is the SAME gate: CC follows its
                // tool-less PermissionRequest with a Notification.
                self.corr.gated_before_waiting.remove(&agent_id);
            }
            fsm::enter_waiting(slot, Arc::<str>::from(reason), now);
        }
    }

    fn apply_rename(scene: &mut SceneState, agent_id: AgentId, label: &str, now: SystemTime) {
        if let Some(slot) = scene.agents.get_mut(&agent_id) {
            let label = classify_rename(label, &slot.source);
            fsm::rename(slot, label, now);
        }
    }

    fn apply_session_end(
        &mut self,
        scene: &mut SceneState,
        agent_id: AgentId,
        as_child: bool,
        from: Transport,
        now: SystemTime,
    ) {
        self.corr.note_end(agent_id, from, as_child, now);
        scope::cascade_exit(scene, agent_id, scope::StampRoot::Yes, now);
    }

    /// The `ProofOfLife` arm — #220: refresh the sweep exemption, and NOTHING
    /// else. No slot synthesis (this only vouches for already-visible slots),
    /// no state change, no `last_event_at` refresh. An exiting slot is left
    /// alone so the vouch can't tug against SessionEnd/cascade_exit.
    fn apply_proof_of_life(&mut self, scene: &SceneState, agent_id: AgentId, now: SystemTime) {
        if scene
            .agents
            .get(&agent_id)
            .is_some_and(|s| s.exiting_at.is_none())
        {
            self.corr.recent_proof_of_life.insert(agent_id, now);
        }
    }

    /// The `Identity` arm — #221: the identity context a hook decoder attaches
    /// ahead of a tool/permission event. Register-or-back-fill, NOTHING else:
    /// no label change, no activity-state change, no `last_event_at` refresh.
    /// The paired activity event right behind it carries those.
    fn apply_identity(
        &mut self,
        scene: &mut SceneState,
        agent_id: AgentId,
        ctx: IdentityCtx,
        pid: Option<crate::source::PidIdentity>,
        from: Transport,
        now: SystemTime,
    ) {
        // No in-tree JSONL path emits Identity today; the guard IS the
        // boundary, not dead code (replay hazard: see hook synthesis).
        if from != Transport::Hook {
            tracing::debug!(?agent_id, "ignoring Identity on a non-hook transport");
            return;
        }
        if let Some(slot) = scene.agents.get_mut(&agent_id) {
            backfill_identity(slot, ctx);
            // A pid-less Identity must never DOWNGRADE a cached Some (e.g. an
            // opencode plugin event following a shim-stamped one).
            if pid.is_some() {
                slot.pid = pid;
            }
            // Identity is hook-only, so the process is alive: a JSONL-seeded
            // unknown-cwd flag must not keep `STALE_UNKNOWN_CWD_TIMEOUT` armed.
            slot.unknown_cwd = false;
        } else if let Some(parent) =
            self.parent_to_create(scene, agent_id, ctx, AttemptKind::Identity, from, now)
            && self.register_and_link(scene, agent_id, ctx, parent, now)
            && let Some(slot) = scene.agents.get_mut(&agent_id)
        {
            slot.unknown_cwd = false;
            if pid.is_some() {
                slot.pid = pid;
            }
        }
    }

    /// Updates an EXISTING slot only; a model line must never register a
    /// session. Legitimate on BOTH transports: model/effort are wire data, not
    /// liveness. Known cosmetic residual: a first-sight replay stamps a
    /// HISTORICAL effort marker with apply-time `now`, so a session that used
    /// max effort earlier flames until the scene's effort TTL expires.
    fn apply_model_info(
        scene: &mut SceneState,
        agent_id: AgentId,
        obs: ModelObservation<'_>,
        now: SystemTime,
    ) {
        if let Some(slot) = scene.agents.get_mut(&agent_id) {
            if let Some(m) = obs.model
                && slot.model.as_deref() != Some(m)
            {
                slot.model = Some(Arc::from(m));
            }
            if let Some(e) = obs.effort {
                slot.effort = Some(crate::state::EffortObservation::new(Arc::from(e), now));
            }
        }
    }

    /// The `Usage` arm — updates an EXISTING slot only, the `ModelInfo`
    /// posture. Saturating so a hostile/corrupt transcript can't overflow it.
    fn apply_usage(scene: &mut SceneState, agent_id: AgentId, fresh_tokens: u64, now: SystemTime) {
        if let Some(slot) = scene.agents.get_mut(&agent_id) {
            slot.tokens_used = slot.tokens_used.saturating_add(fresh_tokens);
            slot.last_usage = Some(crate::state::UsageObservation::new(fresh_tokens, now));
        }
    }

    /// The `SessionStart` arm.
    fn apply_session_start(
        &mut self,
        scene: &mut SceneState,
        agent_id: AgentId,
        ctx: IdentityCtx,
        parent_id: Option<AgentId>,
        from: Transport,
        now: SystemTime,
    ) {
        let attempt = self.attempt(agent_id, AttemptKind::Start, parent_id);
        let verdict = self.authorizes(scene, agent_id, attempt, from, now);
        if let Verdict::Refuse(refusal) = verdict {
            refusal.log_start(agent_id, ctx, attempt.parent);
            return;
        }
        let parent_id = self.resolve_parent_link(scene, agent_id, ctx, attempt.parent);
        if let Verdict::Enrich { revive } = verdict {
            self.enrich_existing_start(scene, agent_id, ctx, parent_id, revive, now);
        } else {
            // A desk-exhaustion refusal records nothing — nothing registered.
            self.register_and_link(scene, agent_id, ctx, parent_id, now);
        }
    }

    fn attempt(&self, id: AgentId, kind: AttemptKind, named: Option<AgentId>) -> Attempt {
        Attempt {
            kind,
            parent: named.or_else(|| self.corr.remembered_parent(id)),
        }
    }

    /// What `attempt`, arriving on `from`, may do to `id`'s slot: the ONE
    /// creation-polarity decision every registering arm applies. Desk capacity
    /// and parent cycles are structure, not authority, and stay out.
    fn authorizes(
        &self,
        scene: &SceneState,
        id: AgentId,
        attempt: Attempt,
        from: Transport,
        now: SystemTime,
    ) -> Verdict {
        let corr = &self.corr;
        let starts_fresh = attempt.kind == AttemptKind::Start && attempt.parent.is_none();
        // A transcript's first sight proves only that a file exists: codex
        // writes its rollout at exit, after its hook end.
        let revival_owned =
            starts_fresh && from != Transport::Hook && corr.hook_owns_revival(id, now);
        let parent_lives =
            |p: &AgentId| scene.agents.get(p).is_some_and(|s| s.exiting_at.is_none());
        if let Some(slot) = scene.agents.get(&id) {
            // An exiting root revives on a fresh start, an exiting child only
            // on a hook start while its linked parent lives: a late duplicate
            // can't un-exit a cascaded subagent.
            let revive = slot.exiting_at.is_some()
                && match attempt.kind {
                    AttemptKind::Start if starts_fresh => {
                        slot.parent_id.is_none() && !revival_owned
                    }
                    // An end that landed on the walking-out slot is a turn whose
                    // own Start is still to come, reordered (#242).
                    AttemptKind::Start => {
                        from == Transport::Hook
                            && slot.parent_id.as_ref().is_some_and(parent_lives)
                            && slot
                                .exiting_at
                                .is_some_and(|exit| !corr.child_ended_since(id, exit))
                    }
                    AttemptKind::Identity | AttemptKind::HookActivity => false,
                };
            return Verdict::Enrich { revive };
        }
        let refusal = match attempt.kind {
            AttemptKind::Start if starts_fresh => revival_owned.then_some(Refusal::HookOwnsRevival),
            // #242: hook deliveries ride per-connection tasks, so an end can be
            // DECODED ahead of its life's trailing events. A hook start after
            // the walkout is the child's next turn, judged below: the slot's
            // own revive covered the window before it.
            _ if corr.ended_within_straggler_window(id, now)
                && !(from == Transport::Hook
                    && attempt.kind == AttemptKind::Start
                    && corr.child_ended_before(id, now, EXIT_GRACE_WINDOW)) =>
            {
                Some(Refusal::Straggler)
            }
            // #244: inside the child gate a transcript registration is the dead
            // child's late first sight.
            _ => match corr.recent_child_end(id, now) {
                None => None,
                // A hook is the CLI starting the child again (a CC resume),
                // under a parent that still lives.
                Some(_)
                    if from == Transport::Hook
                        && attempt.parent.as_ref().is_some_and(parent_lives) =>
                {
                    None
                }
                Some(_) => Some(Refusal::ChildEnded),
            },
        };
        refusal.map_or(Verdict::Create, Verdict::Refuse)
    }

    /// The #240 cycle refusal on an attempt's parent: a poisoned memory
    /// degrades to parentless. This is the ONE seam where `parent_id` is set or
    /// enriched, so a cycle can never EXIST.
    fn resolve_parent_link(
        &self,
        scene: &SceneState,
        agent_id: AgentId,
        ctx: IdentityCtx,
        parent_id: Option<AgentId>,
    ) -> Option<AgentId> {
        // Gated on the link APPLYING, so a duplicate's bad parent stays quiet.
        let link_would_apply = scene
            .agents
            .get(&agent_id)
            .is_none_or(|slot| slot.parent_id.is_none());
        parent_id.filter(|&p| {
            if !link_would_apply {
                return true;
            }
            // #238: a 2-cycle of BOTH-Waiting nodes would mutually satisfy
            // `has_waiting_ancestor` and escape `sweep_stale` forever.
            let cycle = scope::would_create_cycle(&scene.agents, agent_id, p);
            if cycle {
                tracing::warn!(
                    ?agent_id,
                    proposed_parent = ?p,
                    session_id = ?ctx.session_id,
                    cwd = ?ctx.cwd,
                    "refused parent_id link — it would close a parent cycle; degrading to parentless"
                );
            }
            !cycle
        })
    }

    /// Fold a duplicate or late `SessionStart` into an EXISTING slot. A
    /// pending parent link is applied ONCE — never re-parented — and revives its
    /// lifecycle (#244/#246). A duplicate is genuine liveness (Codex/Reasonix
    /// re-emit one per `UserPromptSubmit`), and an exiting slot resurrects when
    /// `revive`.
    fn enrich_existing_start(
        &mut self,
        scene: &mut SceneState,
        agent_id: AgentId,
        ctx: IdentityCtx,
        parent_id: Option<AgentId>,
        revive: bool,
        now: SystemTime,
    ) {
        let slot = scene
            .agents
            .get_mut(&agent_id)
            .expect("Verdict::Enrich is returned only for an existing slot");
        let was_linked = slot.parent_id.is_some();
        if !was_linked && let Some(p) = parent_id {
            slot.parent_id = Some(p);
            self.corr.link_applied_parent(agent_id, p);
        }
        // A slot can exist with MISSING identity: hook synthesis registers from
        // the AgentId alone, and a Codex revive ghost has an empty cwd.
        if let Some(base) = backfill_identity(slot, ctx) {
            // `base` BYPASSES the `cwd_basename_label` chokepoint, so the
            // same decode-boundary cap is applied here.
            let derived = crate::state::SlotLabel::cwd_derived(format!(
                "{}·{}",
                label_prefix_for(&slot.source),
                crate::source::decoder::ellipsize(
                    base,
                    crate::source::decoder::MAX_DECODED_FIELD_CHARS,
                )
            ));
            if slot.label.yields_to(&derived) {
                slot.label = derived;
            }
        }
        slot.last_event_at = now;
        if revive {
            // Route through fsm so an in-flight Active span folds into
            // `active_ms` — a direct `state = Idle` here silently dropped it.
            fsm::resurrect_in_place(slot, now);
            // Evict the dead life's correlation as `sweep_exited` would have;
            // `recent_proof_of_life` deliberately SURVIVES a resurrect.
            self.remove_agent_correlation(&agent_id);
            if let Some(p) = slot.parent_id {
                self.corr.link_applied_parent(agent_id, p);
            }
        }
    }

    /// A hook event is PROOF OF LIFE, so a tool/permission event whose id has no
    /// slot registers one — that session's transcript was gated at first sight.
    /// JSONL must NOT synthesize: a transcript line can be a historical replay.
    /// Only an event that [`AgentEvent::shows_activity`] qualifies. Since #221 decoders
    /// attach an `Identity` ahead, leaving this the identity-less fallback.
    fn synthesize_hook_registration(
        &mut self,
        scene: &mut SceneState,
        event: &AgentEvent,
        id: AgentId,
        now: SystemTime,
    ) {
        if !event.shows_activity() {
            return;
        }
        let ctx = IdentityCtx {
            source: "",
            session_id: "",
            cwd: std::path::Path::new(""),
        };
        // A blank Idle ghost of a DEAD session is unremovable.
        let Some(parent) = self.parent_to_create(
            scene,
            id,
            ctx,
            AttemptKind::HookActivity,
            Transport::Hook,
            now,
        ) else {
            return;
        };
        if self.register_and_link(scene, id, ctx, parent, now)
            && let Some(slot) = scene.agents.get_mut(&id)
        {
            // NOT an unknown-cwd ghost: that reap targets startup
            // JSONL-seeding artifacts, and a hook proves this one alive.
            slot.unknown_cwd = false;
        }
    }

    /// [`Reducer::authorizes`] for an arm whose event names no parent: `None`
    /// when it may not create, else the parent to create under, through
    /// [`Reducer::resolve_parent_link`].
    fn parent_to_create(
        &self,
        scene: &SceneState,
        id: AgentId,
        ctx: IdentityCtx,
        kind: AttemptKind,
        from: Transport,
        now: SystemTime,
    ) -> Option<Option<AgentId>> {
        let attempt = self.attempt(id, kind, None);
        match self.authorizes(scene, id, attempt, from, now) {
            Verdict::Create => Some(self.resolve_parent_link(scene, id, ctx, attempt.parent)),
            Verdict::Enrich { .. } | Verdict::Refuse(_) => None,
        }
    }

    /// [`Reducer::register_slot`], recording the link it applies.
    fn register_and_link(
        &mut self,
        scene: &mut SceneState,
        id: AgentId,
        ctx: IdentityCtx,
        parent: Option<AgentId>,
        now: SystemTime,
    ) -> bool {
        let registered = self.register_slot(scene, id, ctx, parent, now);
        if registered && let Some(p) = parent {
            self.corr.link_applied_parent(id, p);
        }
        registered
    }

    /// The slot-creation half of the `SessionStart` arm, shared with
    /// [`Reducer::synthesize_hook_registration`] so both run the same
    /// desk-capacity gate and label derivation. Returns `false` when all desks
    /// are occupied.
    fn register_slot(
        &mut self,
        scene: &mut SceneState,
        agent_id: AgentId,
        ctx: IdentityCtx,
        parent_id: Option<AgentId>,
        now: SystemTime,
    ) -> bool {
        let IdentityCtx {
            source,
            session_id,
            cwd,
        } = ctx;
        let Some(desk_index) = scene.next_free_desk() else {
            tracing::warn!(
                ?agent_id,
                cwd = ?cwd,
                session_id = ?session_id,
                total_capacity = scene.total_capacity(),
                "dropped SessionStart — all desks occupied; bump --max-desks"
            );
            return false;
        };
        let floor_idx = scene.floor_of(desk_index);
        // A hook-only source has no JSONL Rename — its prefix is minted here.
        let prefix = label_prefix_for(source);
        // The cwd is hook/transcript CONTENT — the `cwd_basename_label`
        // chokepoint caps the label at the decode boundary.
        let named = crate::source::decoder::cwd_basename_label(prefix, cwd);
        let has_cwd = named.is_some();
        let label = match named {
            Some(l) => crate::state::SlotLabel::cwd_derived(l),
            None => {
                // Only an unknown-cwd ghost consumes an ordinal, so labels stay
                // contiguous instead of skipping the preceding named sessions.
                self.next_label_n += 1;
                crate::state::SlotLabel::ordinal_ghost(format!("{prefix}#{}", self.next_label_n))
            }
        };
        // Same-cwd sessions are disambiguated at render time, not with a `·xxxx`.
        scene.agents.insert(
            agent_id,
            AgentSlot {
                agent_id,
                source: Arc::<str>::from(source),
                session_id: Arc::<str>::from(session_id),
                cwd: Arc::<std::path::Path>::from(cwd),
                label,
                state: ActivityState::Idle,
                state_started_at: now,
                last_event_at: now,
                created_at: now,
                exiting_at: None,
                pending_idle_at: None,
                desk_index,
                floor_idx,
                tool_call_count: 0,
                active_ms: 0,
                // A PARENTED slot came from an explicit subagent signal (e.g.
                // Copilot's `subagent.started`, whose payload carries no cwd).
                unknown_cwd: !has_cwd && parent_id.is_none(),
                parent_id,
                pid: None,
                model: None,
                effort: None,
                tokens_used: 0,
                last_usage: None,
            },
        );
        true
    }

    /// Hook transport only, on a source whose child hooks arrive unnamed under
    /// the parent's key: with any Task in flight, a hook ActivityStart/End or
    /// TurnEnd for this AgentId is the child's work, so it is dropped. Two
    /// exemptions: the Task's own PostToolUse (its tool_use_id matches), and a
    /// GATED member's Start/End — the parent's own approval round (#951), which
    /// eating would strand in Waiting until the parent's next own event or the
    /// stale sweep.
    fn suppress_subagent_leak(
        &mut self,
        scene: &mut SceneState,
        event: &AgentEvent,
        id: AgentId,
        now: SystemTime,
    ) -> bool {
        let tasks = self.corr.active_tasks.get(&id);
        let in_task = tasks.is_some_and(|s| !s.is_empty());
        let is_gated = |tool_use_id: &Option<String>| {
            tool_use_id
                .as_deref()
                .is_some_and(|t| self.corr.gate_matches(&id, t))
        };
        let suppress = match event {
            AgentEvent::ActivityStart { tool_use_id, .. } => in_task && !is_gated(tool_use_id),
            AgentEvent::ActivityEnd { tool_use_id, .. } => {
                let is_task_self_end = tool_use_id
                    .as_ref()
                    .is_some_and(|t| tasks.is_some_and(|s| s.contains(t)));
                in_task && !is_task_self_end && !is_gated(tool_use_id)
            }
            AgentEvent::TurnEnd { .. } => in_task,
            _ => false,
        };
        if suppress {
            // A suppressed child event means the subagent resumed, so its
            // misattributed gate resolved — unless the tuid ∉ `active_tasks`.
            if let Some(slot) = scene.agents.get_mut(&id)
                && matches!(slot.state, ActivityState::Waiting { .. })
            {
                let gate_is_own_tool =
                    self.corr.gated_before_waiting.get(&id).is_some_and(|g| {
                        g.iter().any(|m| !tasks.is_some_and(|s| s.contains(&**m)))
                    });
                if !gate_is_own_tool {
                    let task_tuid = self.corr.any_active_task(&id);
                    fsm::enter_delegating(slot, task_tuid, now);
                    self.corr.gated_before_waiting.remove(&id);
                }
            }
        }
        suppress
    }

    /// Tracks Task tool_use_ids from either transport, marking a parent that
    /// gains one Active("Delegating") so it doesn't look asleep while subagents
    /// work. b1 subagent-completion inference, where [`ChildEndInference`] says
    /// so: a drained parent Task means the subtree returned — cascade EXIT to the
    /// DESCENDANTS, not the parent, so they leave before [`STALE_IDLE_TIMEOUT`].
    fn track_active_tasks(
        &mut self,
        scene: &mut SceneState,
        event: &AgentEvent,
        now: SystemTime,
    ) -> TaskTracking {
        let mut handled_by_task_tracking = false;
        let mut handled_by_task_start = false;
        match event {
            AgentEvent::ActivityStart {
                agent_id,
                tool_use_id: Some(tuid),
                detail: Some(d),
                ..
            } if d.is_task() => {
                handled_by_task_start = true;
                // Delegating only on a FIRST insert of a never-drained tuid: an
                // out-of-window replay would clobber a since-Waiting parent.
                if self
                    .corr
                    .active_tasks
                    .entry(*agent_id)
                    .or_default()
                    .insert(tuid.clone())
                    && !self
                        .corr
                        .recent_task_drains
                        .contains_key(&(*agent_id, tuid.clone()))
                    && let Some(slot) = scene.agents.get_mut(agent_id)
                {
                    fsm::enter_delegating(slot, Some(Arc::<str>::from(tuid.as_str())), now);
                }
            }
            AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id: Some(tuid),
            } => {
                if let Some(set) = self.corr.active_tasks.get_mut(agent_id)
                    && set.remove(tuid)
                {
                    handled_by_task_tracking = true;
                    self.corr
                        .recent_task_drains
                        .insert((*agent_id, tuid.clone()), now);
                    // #152: the drain skips the main arm, so a gate on THIS
                    // tuid goes stale. Inline: `set` still borrows `corr`.
                    if let Some(g) = self.corr.gated_before_waiting.get_mut(agent_id) {
                        g.retain(|m| &**m != tuid.as_str());
                    }
                    if let Some(slot) = scene.agents.get_mut(agent_id) {
                        slot.last_event_at = now;
                        // Arm the debounce only when actually Active: a
                        // Waiting parent's expiry would false-clear it.
                        if set.is_empty() {
                            if child_end_inference(slot).dispatch_end_ends_child(true) {
                                self.pending_b1_cascades.insert(*agent_id, now);
                            }
                            if matches!(slot.state, ActivityState::Active { .. }) {
                                fsm::arm_pending_idle(slot, now);
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        TaskTracking {
            handled_by_task_tracking,
            handled_by_task_start,
        }
    }

    /// Fire deferred b1 cascades whose [`B1_CASCADE_GRACE`] elapsed (#151). The
    /// fire-time emptiness check IS the cancel mechanism: a Task insert inside
    /// the grace re-populates `active_tasks`, so the due entry is discarded
    /// instead of fired — no separate cancel-on-insert bookkeeping that could
    /// drift out of sync with the ledger.
    fn fire_pending_b1_cascades(&mut self, scene: &mut SceneState, now: SystemTime) {
        let due: Vec<AgentId> = self
            .pending_b1_cascades
            .iter()
            .filter(|(_, armed)| elapsed_at_least(now, **armed, B1_CASCADE_GRACE))
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            self.pending_b1_cascades.remove(&id);
            if self
                .corr
                .active_tasks
                .get(&id)
                .is_some_and(|s| !s.is_empty())
            {
                continue;
            }
            tracing::debug!(agent_id = ?id, "b1 grace elapsed — cascading completed subtree");
            // The delegating parent keeps running; only its subtree walks out.
            scope::cascade_exit(scene, id, scope::StampRoot::No, now);
        }
    }

    /// Flip agents with an elapsed `pending_idle_at` to Idle. Resets
    /// `state_started_at` to `now` so the Idle wander state machine starts from
    /// the visible transition, not the now-stale original end's time.
    fn expire_pending_idles(&mut self, scene: &mut SceneState, now: SystemTime) {
        for slot in scene.agents.values_mut() {
            let Some(pending) = slot.pending_idle_at else {
                continue;
            };
            if elapsed_at_least(now, pending, ACTIVE_GRACE_WINDOW) {
                // A Waiting slot carries `pending_idle_at` only once its gated
                // tool resolved; a parallel-prompt Waiting never arms it.
                fsm::settle_to_idle(slot, pending, now);
            }
        }
    }

    /// Mark agents as exiting when they haven't emitted any event for longer
    /// than their state-adaptive threshold. Uses `last_event_at` as the
    /// liveness signal, NOT `state_started_at`, which only tracks the current
    /// state's age.
    fn sweep_stale(&mut self, scene: &mut SceneState, now: SystemTime) {
        // A node gated by a `Waiting` relative is blocked with it, not dead: a
        // subagent under a waiting parent, or a parent over a waiting subagent.
        // A waiting descendant never exempts a waiting node, so a waiting pair
        // can't exempt each other (#238).
        let agents = &scene.agents;
        let stale: Vec<(AgentId, Duration, Duration)> = agents
            .values()
            .filter(|slot| slot.exiting_at.is_none())
            .filter_map(|slot| {
                let waiting = matches!(slot.state, ActivityState::Waiting { .. });
                if scope::has_waiting_ancestor(agents, slot.agent_id)
                    || (!waiting && scope::has_waiting_descendant(agents, slot.agent_id))
                {
                    return None;
                }
                // Probe-vouched (#220): a recent ProofOfLife means the process
                // is alive RIGHT NOW, so event silence is not death.
                if self.corr.vouch_fresh(&slot.agent_id, now) {
                    return None;
                }
                // A vouched ancestor's DELEGATED subtree inherits it: the probe
                // never vouches subagent ids, and a parked parent reads Active.
                if scope::has_ancestor_where(agents, slot.agent_id, |a| {
                    self.corr.vouch_fresh(&a.agent_id, now)
                        && self
                            .corr
                            .active_tasks
                            .get(&a.agent_id)
                            .is_some_and(|t| !t.is_empty())
                }) {
                    return None;
                }
                let age = now
                    .duration_since(slot.last_event_at)
                    .unwrap_or(Duration::ZERO);
                let threshold = stale_threshold(slot);
                (age > threshold).then_some((slot.agent_id, age, threshold))
            })
            .collect();

        // Cascading keeps a stale-swept (or SessionEnd-less) parent from leaving
        // orphans; the skip below keeps `exiting_at` and the log write-once.
        for (id, age, threshold) in stale {
            {
                let slot = scene
                    .agents
                    .get_mut(&id)
                    .expect("cascade_exit only marks exiting; no slot leaves between the passes");
                if slot.exiting_at.is_some() {
                    continue;
                }
                tracing::info!(
                    agent_id = ?id,
                    label = ?&*slot.label,
                    age_secs = age.as_secs(),
                    threshold_secs = threshold.as_secs(),
                    "stale agent — marking exiting"
                );
            }
            scope::cascade_exit(scene, id, scope::StampRoot::Yes, now);
        }
    }

    /// Drop the per-slot correlation a departing agent id owns, reclaimed on
    /// BOTH a resurrect-in-place and a slot removal. Death-only teardown
    /// (`recent_proof_of_life`, [`Correlation::note_removed`]) deliberately stays at
    /// the sweep site — it must NOT run on a resurrect.
    fn remove_agent_correlation(&mut self, id: &AgentId) {
        self.corr.active_tasks.remove(id);
        self.corr.gated_before_waiting.remove(id);
        self.corr.counted_calls.remove(id);
        self.pending_b1_cascades.remove(id);
    }

    /// Removing a parent does NOT null any surviving child's `parent_id` — that
    /// pointer is left dangling intentionally. The scope walks tolerate it via
    /// their `None => break` guards, and scanning every child on each parent
    /// removal would cost with no behavioral benefit.
    fn sweep_exited(&mut self, scene: &mut SceneState, now: SystemTime) {
        let expired: Vec<AgentId> = scene
            .agents
            .iter()
            .filter_map(|(id, slot)| {
                slot.exiting_at
                    .filter(|t| elapsed_past(now, *t, EXIT_GRACE_WINDOW))
                    .map(|_| *id)
            })
            .collect();
        for id in expired {
            scene.agents.remove(&id);
            // Runs on the apply path too, where `tick`'s `retain` doesn't.
            self.remove_agent_correlation(&id);
            // Evict with the slot, else a same-id ghost inherits the vouch.
            self.corr.recent_proof_of_life.remove(&id);
            self.corr.note_removed(id, now);
        }
    }
}

fn event_tool_use_id(ev: &AgentEvent) -> Option<(ToolEventKind, &str)> {
    match ev {
        AgentEvent::ActivityStart { tool_use_id, .. } => {
            tool_use_id.as_deref().map(|t| (ToolEventKind::Start, t))
        }
        AgentEvent::ActivityEnd { tool_use_id, .. } => {
            tool_use_id.as_deref().map(|t| (ToolEventKind::End, t))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
