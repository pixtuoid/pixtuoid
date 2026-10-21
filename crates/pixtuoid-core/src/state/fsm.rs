//! The per-agent **FSM** (Layer A) — the legal single-slot state transitions
//! (`Idle / Active / Waiting` plus the exit + Active→Idle debounce). The Layer-B
//! scope tree over `parent_id` lives in [`super::scope`].
//!
//! **Scope:** these fns mutate ONE `AgentSlot` only. The cross-slot correlation
//! state — `active_tasks` (Tasks in flight), `gated_before_waiting`
//! (permission), the hook-wins dedup — stays in the reducer: it spans slots and
//! is not the FSM. The reducer decides *whether* and *which* transition fires;
//! these fns decide *how* the slot changes.

use std::sync::Arc;
use std::time::SystemTime;

use crate::source::ToolDetail;
use crate::state::{ActivityState, AgentSlot, ToolKind};

/// Fold the time the slot has spent Active into `active_ms`, after which it is
/// safe to overwrite `state`/`state_started_at`. `until` is the instant the
/// Active span ends — `now` for a live transition, the debounce `pending` mark
/// for an idle-expiry, so the grace window isn't counted as work.
fn accumulate_active_ms(slot: &mut AgentSlot, until: SystemTime) {
    if matches!(slot.state, ActivityState::Active { .. }) {
        let elapsed = until
            .duration_since(slot.state_started_at)
            .unwrap_or_default()
            .as_millis() as u64;
        slot.active_ms += elapsed;
    }
}

/// Enter `Active` with a concrete tool — the general `ActivityStart` path.
/// Stamps `last_event_at` (this is the actor's own event).
pub(crate) fn enter_active(
    slot: &mut AgentSlot,
    tool_use_id: Option<Arc<str>>,
    detail: Option<Arc<str>>,
    kind: ToolKind,
    now: SystemTime,
) {
    accumulate_active_ms(slot, now);
    slot.state = ActivityState::Active {
        tool_use_id,
        detail,
        kind,
    };
    slot.state_started_at = now;
    slot.last_event_at = now;
    slot.pending_idle_at = None;
}

/// Enter the `Active("Delegating")` state — a parent dispatched a Task, its
/// own tool ended while one is in flight, or an unnamed child's dropped event
/// resumed it from the child's gate. Deliberately does NOT stamp
/// `last_event_at`: every caller already refreshed lineage.
pub(crate) fn enter_delegating(
    slot: &mut AgentSlot,
    tool_use_id: Option<Arc<str>>,
    now: SystemTime,
) {
    accumulate_active_ms(slot, now);
    slot.state = ActivityState::Active {
        tool_use_id,
        // HUD text only — semantics ride on `kind`, never on this string.
        detail: Some(Arc::<str>::from(ToolDetail::Task.display())),
        kind: ToolKind::Task,
    };
    slot.state_started_at = now;
    slot.pending_idle_at = None;
}

/// Enter `Waiting` (a permission gate). Stamps `last_event_at`.
pub(crate) fn enter_waiting(slot: &mut AgentSlot, reason: Arc<str>, now: SystemTime) {
    accumulate_active_ms(slot, now);
    slot.state = ActivityState::Waiting { reason };
    slot.state_started_at = now;
    slot.last_event_at = now;
    slot.pending_idle_at = None;
}

/// Arm the Active→Idle debounce: the slot stays visually Active until
/// `settle_to_idle` fires `ACTIVE_GRACE_WINDOW` later (or a new `ActivityStart`
/// cancels it). The caller owns `last_event_at` — it differs per call site.
pub(crate) fn arm_pending_idle(slot: &mut AgentSlot, now: SystemTime) {
    slot.pending_idle_at = Some(now);
}

/// Realize an armed debounce: settle an `Active` (normal tool end) or a
/// `Waiting` slot whose gated permission resolved down to `Idle`. The Active
/// span that ended at `pending` is folded into `active_ms`. The caller checks
/// the grace window first.
pub(crate) fn settle_to_idle(slot: &mut AgentSlot, pending: SystemTime, now: SystemTime) {
    match &slot.state {
        ActivityState::Active { .. } => {
            accumulate_active_ms(slot, pending);
            slot.state = ActivityState::Idle;
            slot.state_started_at = now;
        }
        ActivityState::Waiting { .. } => {
            slot.state = ActivityState::Idle;
            slot.state_started_at = now;
        }
        ActivityState::Idle => {}
    }
    slot.pending_idle_at = None;
}

/// Begin the exit animation (write-once `exiting_at`). The actual slot removal
/// happens `EXIT_GRACE_WINDOW` later in the reducer's `sweep_exited`.
pub(crate) fn mark_exiting(slot: &mut AgentSlot, now: SystemTime) {
    if slot.exiting_at.is_none() {
        slot.exiting_at = Some(now);
    }
}

/// Resurrect an EXITING slot back to a live `Idle` state — a SessionStart
/// landed on a slot mid-walkout (Reasonix `/new` fires SessionEnd+SessionStart
/// on the same cwd-keyed id). The reducer gates this to a root, or a child
/// whose linked parent lives, so a late duplicate can't un-exit a cascaded one.
pub(crate) fn resurrect_in_place(slot: &mut AgentSlot, now: SystemTime) {
    accumulate_active_ms(slot, now);
    slot.exiting_at = None;
    slot.pending_idle_at = None;
    slot.state = ActivityState::Idle;
    slot.state_started_at = now;
}

/// Apply a display rename the label yields to, and refresh liveness.
pub(crate) fn rename(slot: &mut AgentSlot, label: crate::state::SlotLabel, now: SystemTime) {
    if slot.label.yields_to(&label) {
        slot.label = label;
    }
    slot.last_event_at = now;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentId;
    use std::path::Path;
    use std::time::Duration;

    fn slot_at(state: ActivityState, started: SystemTime) -> AgentSlot {
        AgentSlot {
            agent_id: AgentId::from_parts("cc", "t"),
            source: Arc::from("cc"),
            session_id: Arc::from("s"),
            cwd: Arc::from(Path::new("/repo")),
            label: "cc·repo".into(),
            state,
            state_started_at: started,
            last_event_at: started,
            created_at: started,
            exiting_at: None,
            pending_idle_at: None,
            desk_index: crate::state::GlobalDeskIndex(0),
            floor_idx: 0,
            tool_call_count: 0,
            active_ms: 0,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        }
    }

    fn active(started: SystemTime) -> AgentSlot {
        slot_at(
            ActivityState::Active {
                tool_use_id: None,
                detail: None,
                kind: ToolKind::Other,
            },
            started,
        )
    }

    #[test]
    fn enter_active_accumulates_prior_active_span_and_resets_timers() {
        let t0 = SystemTime::now();
        let mut s = active(t0);
        s.pending_idle_at = Some(t0);
        enter_active(
            &mut s,
            None,
            None,
            ToolKind::Other,
            t0 + Duration::from_secs(1),
        );
        assert_eq!(s.active_ms, 1000, "prior Active span folded in");
        assert_eq!(s.state_started_at, t0 + Duration::from_secs(1));
        assert_eq!(s.last_event_at, t0 + Duration::from_secs(1));
        assert_eq!(
            s.pending_idle_at, None,
            "an ActivityStart cancels pending idle"
        );
    }

    #[test]
    fn enter_delegating_from_waiting_does_not_accumulate_or_stamp_last_event() {
        let t0 = SystemTime::now();
        let mut s = slot_at(
            ActivityState::Waiting {
                reason: Arc::from("perm"),
            },
            t0,
        );
        enter_delegating(&mut s, None, t0 + Duration::from_secs(5));
        assert_eq!(s.active_ms, 0, "Waiting time is not Active time");
        assert_eq!(
            s.last_event_at, t0,
            "enter_delegating leaves last_event_at to refresh_lineage"
        );
        match &s.state {
            ActivityState::Active { detail, kind, .. } => {
                assert_eq!(detail.as_deref(), Some("Delegating"));
                assert_eq!(*kind, ToolKind::Task, "delegation is typed, not spelled");
            }
            other => panic!("expected Active(Delegating), got {other:?}"),
        }
    }

    #[test]
    fn enter_waiting_accumulates_from_active() {
        let t0 = SystemTime::now();
        let mut s = active(t0);
        enter_waiting(&mut s, Arc::from("perm"), t0 + Duration::from_secs(2));
        assert_eq!(s.active_ms, 2000);
        assert!(matches!(s.state, ActivityState::Waiting { .. }));
        assert_eq!(s.pending_idle_at, None);
    }

    #[test]
    fn settle_to_idle_folds_active_span_up_to_pending_not_now() {
        let t0 = SystemTime::now();
        let pending = t0 + Duration::from_secs(1);
        let now = t0 + Duration::from_secs(3);
        let mut s = active(t0);
        s.pending_idle_at = Some(pending);
        settle_to_idle(&mut s, pending, now);
        assert!(matches!(s.state, ActivityState::Idle));
        assert_eq!(s.active_ms, 1000, "span ends at pending, not now");
        assert_eq!(s.state_started_at, now);
        assert_eq!(s.pending_idle_at, None);
    }

    #[test]
    fn settle_to_idle_resolves_a_waiting_slot_without_accumulating() {
        let t0 = SystemTime::now();
        let pending = t0 + Duration::from_secs(1);
        let now = t0 + Duration::from_secs(3);
        let mut s = slot_at(
            ActivityState::Waiting {
                reason: Arc::from("perm"),
            },
            t0,
        );
        s.pending_idle_at = Some(pending);
        settle_to_idle(&mut s, pending, now);
        assert!(matches!(s.state, ActivityState::Idle));
        assert_eq!(s.active_ms, 0, "Waiting time is not folded into active_ms");
        assert_eq!(s.state_started_at, now);
        assert_eq!(s.pending_idle_at, None);
    }

    #[test]
    fn settle_to_idle_on_idle_is_a_noop_but_clears_mark() {
        let t0 = SystemTime::now();
        let mut s = slot_at(ActivityState::Idle, t0);
        s.pending_idle_at = Some(t0);
        let started = s.state_started_at;
        settle_to_idle(&mut s, t0, t0 + Duration::from_secs(1));
        assert!(matches!(s.state, ActivityState::Idle));
        assert_eq!(s.state_started_at, started, "Idle isn't re-stamped");
        assert_eq!(s.pending_idle_at, None);
    }

    #[test]
    fn mark_exiting_is_write_once() {
        let t0 = SystemTime::now();
        let mut s = active(t0);
        mark_exiting(&mut s, t0);
        mark_exiting(&mut s, t0 + Duration::from_secs(10));
        assert_eq!(s.exiting_at, Some(t0), "first exit time wins");
    }

    #[test]
    fn rename_is_idempotent_but_always_refreshes_liveness() {
        let t0 = SystemTime::now();
        let mut s = active(t0);
        rename(&mut s, "cc·repo".into(), t0 + Duration::from_secs(1));
        assert_eq!(&*s.label, "cc·repo");
        assert_eq!(s.last_event_at, t0 + Duration::from_secs(1));
        rename(&mut s, "code-explorer".into(), t0 + Duration::from_secs(2));
        assert_eq!(&*s.label, "code-explorer");
        assert_eq!(s.last_event_at, t0 + Duration::from_secs(2));
    }

    #[test]
    fn a_rename_to_a_placeholders_own_text_makes_it_a_real_name() {
        let t0 = SystemTime::now();
        let mut s = active(t0);
        s.label = crate::state::SlotLabel::ordinal_ghost("cc#1");
        rename(&mut s, "cc#1".into(), t0);
        assert!(!s.label.is_upgradable());
    }
}
