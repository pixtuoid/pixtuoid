//! The per-agent **FSM** (Layer A) — the legal single-slot state transitions
//! (`Idle / Running / RequiresAction` plus the exit). The Layer-B scope tree over
//! `parent_id` lives in [`super::scope`].
//!
//! **Scope:** these fns mutate ONE `AgentSlot` only. The cross-slot correlation
//! state — `tasks_in_flight`, the action requests (permission), the hook-wins
//! dedup — stays in the reducer: it spans slots and is not the FSM. The reducer decides *whether* and *which* transition fires; these fns
//! decide *how* the slot changes.

use std::sync::Arc;
use std::time::SystemTime;

use crate::source::ToolDetail;
use crate::state::{ActivityState, AgentSlot, ToolKind, ToolUse};

/// Fold the time the slot has spent Running into `running_ms`, after which it is
/// safe to overwrite `state`/`state_started_at`.
fn accumulate_running_ms(slot: &mut AgentSlot, now: SystemTime) {
    if matches!(slot.state, ActivityState::Running { .. }) {
        let elapsed = now
            .duration_since(slot.state_started_at)
            .unwrap_or_default()
            .as_millis() as u64;
        slot.running_ms += elapsed;
    }
}

/// Enter `Running` with a concrete item — the general `ToolUseStart` path.
/// Stamps `last_event_at` (this is the actor's own event).
pub(crate) fn enter_running(slot: &mut AgentSlot, item: ToolUse, now: SystemTime) {
    accumulate_running_ms(slot, now);
    slot.state = ActivityState::Running { item: Some(item) };
    slot.state_started_at = now;
    slot.last_event_at = now;
}

/// The `Running("Delegating")` item of the Task `tool_use_id`.
pub(crate) fn delegating_item(tool_use_id: Option<Arc<str>>) -> ToolUse {
    ToolUse {
        tool_use_id,
        // HUD text only — semantics ride on `kind`, never on this string.
        detail: Some(Arc::<str>::from(ToolDetail::Task.display())),
        kind: ToolKind::Task,
    }
}

/// Enter the `Running("Delegating")` state — a parent dispatched a Task.
/// Deliberately does NOT stamp `last_event_at`: every caller already
/// refreshed lineage.
pub(crate) fn enter_delegating(
    slot: &mut AgentSlot,
    tool_use_id: Option<Arc<str>>,
    now: SystemTime,
) {
    accumulate_running_ms(slot, now);
    slot.state = ActivityState::Running {
        item: Some(delegating_item(tool_use_id)),
    };
    slot.state_started_at = now;
}

/// Enter `RequiresAction` (a permission gate). Stamps `last_event_at`.
pub(crate) fn enter_requires_action(
    slot: &mut AgentSlot,
    reason: Option<Arc<str>>,
    now: SystemTime,
) {
    accumulate_running_ms(slot, now);
    slot.state = ActivityState::RequiresAction { reason };
    slot.state_started_at = now;
    slot.last_event_at = now;
}

/// What a [`settle`] lands on besides the slot's own state.
pub(crate) struct SettleCtx {
    /// The item of the slot's newest call that started and has not ended.
    pub(crate) in_flight: Option<ToolUse>,
    /// A Task the slot dispatched that still runs.
    pub(crate) task_in_flight: Option<Arc<str>>,
    /// The slot's turn is still open.
    pub(crate) turn_open: bool,
}

/// A turn opened on an Idle slot: it works, showing no item yet. The caller's
/// lineage refresh stamps `last_event_at`.
pub(crate) fn enter_turn(slot: &mut AgentSlot, now: SystemTime) {
    if slot.state == ActivityState::Idle {
        slot.state = ActivityState::Running { item: None };
        slot.state_started_at = now;
    }
}

/// The slot's item or wait ended: land on its newest call still in flight,
/// else on Delegating while a Task it dispatched still runs, else on Running
/// with no item while its turn is open, else on Idle. Landing on the state the
/// slot is already in keeps its clock. The caller owns `last_event_at`.
pub(crate) fn settle(slot: &mut AgentSlot, ctx: SettleCtx, now: SystemTime) {
    let task = ctx.task_in_flight.map(|task| delegating_item(Some(task)));
    if let Some(item) = ctx.in_flight.or(task) {
        return show(slot, item, now);
    }
    match slot.state {
        ActivityState::Running { item: None } if ctx.turn_open => {}
        ActivityState::Idle if !ctx.turn_open => {}
        _ => {
            accumulate_running_ms(slot, now);
            slot.state = if ctx.turn_open {
                ActivityState::Running { item: None }
            } else {
                ActivityState::Idle
            };
            slot.state_started_at = now;
        }
    }
}

/// Show `item` on the slot. One Task over another, or the item it shows, is
/// the same state, so it keeps its clock.
fn show(slot: &mut AgentSlot, item: ToolUse, now: SystemTime) {
    match (&mut slot.state, item) {
        (
            ActivityState::Running {
                item:
                    Some(ToolUse {
                        kind: ToolKind::Task,
                        tool_use_id: shown,
                        ..
                    }),
            },
            ToolUse {
                kind: ToolKind::Task,
                tool_use_id,
                ..
            },
        ) => *shown = tool_use_id,
        (ActivityState::Running { item: Some(shown) }, item) if *shown == item => {}
        (_, item) => {
            accumulate_running_ms(slot, now);
            slot.state = ActivityState::Running { item: Some(item) };
            slot.state_started_at = now;
        }
    }
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
    accumulate_running_ms(slot, now);
    slot.exiting_at = None;
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
            desk_index: crate::state::GlobalDeskIndex(0),
            floor_idx: 0,
            tool_call_count: 0,
            running_ms: 0,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        }
    }

    fn ctx(task_in_flight: Option<&str>, turn_open: bool) -> SettleCtx {
        SettleCtx {
            in_flight: None,
            task_in_flight: task_in_flight.map(Arc::from),
            turn_open,
        }
    }

    fn call(id: &str) -> ToolUse {
        ToolUse {
            tool_use_id: Some(Arc::from(id)),
            detail: Some(Arc::from("Read: a.rs")),
            kind: ToolKind::Read,
        }
    }

    #[test]
    fn a_settle_lands_on_the_newest_call_in_flight_before_any_other_state() {
        let t0 = SystemTime::now();
        let now = t0 + Duration::from_secs(1);
        let asking = ActivityState::RequiresAction { reason: None };
        let mut s = slot_at(asking, t0);
        let first = SettleCtx {
            in_flight: Some(call("c2")),
            ..ctx(Some("task-1"), true)
        };
        settle(&mut s, first, now);
        assert_eq!(
            s.state,
            ActivityState::Running {
                item: Some(call("c2"))
            }
        );
        assert_eq!(s.state_started_at, now);
        let again = SettleCtx {
            in_flight: Some(call("c2")),
            ..ctx(None, false)
        };
        settle(&mut s, again, now + Duration::from_secs(1));
        assert_eq!(s.state_started_at, now, "the item it shows keeps its clock");
    }

    #[test]
    fn a_settle_inside_an_open_turn_lands_running_with_no_item_and_then_keeps_its_clock() {
        let t0 = SystemTime::now();
        let now = t0 + Duration::from_secs(1);
        let mut s = running(t0);
        settle(&mut s, ctx(None, true), now);
        assert_eq!(s.state, ActivityState::Running { item: None });
        assert_eq!(s.running_ms, 1000, "the item's span folds in");
        assert_eq!(s.state_started_at, now);
        settle(&mut s, ctx(None, true), now + Duration::from_secs(1));
        assert_eq!(
            s.state_started_at, now,
            "Running with no item isn't re-stamped"
        );
        settle(&mut s, ctx(Some("task-1"), true), now);
        assert!(
            matches!(
                s.state,
                ActivityState::Running {
                    item: Some(ToolUse {
                        kind: ToolKind::Task,
                        ..
                    })
                }
            ),
            "a Task in flight still shows Delegating: {:?}",
            s.state
        );
    }

    #[test]
    fn a_turn_shows_only_an_idle_slot_running() {
        let t0 = SystemTime::now();
        let now = t0 + Duration::from_secs(1);
        let mut s = slot_at(ActivityState::Idle, t0);
        enter_turn(&mut s, now);
        assert_eq!(s.state, ActivityState::Running { item: None });
        assert_eq!(s.state_started_at, now);
        let mut s = running(t0);
        enter_turn(&mut s, now);
        assert_eq!(s.state, running(t0).state, "the item it shows stays");
        let asking = ActivityState::RequiresAction { reason: None };
        let mut s = slot_at(asking.clone(), t0);
        enter_turn(&mut s, now);
        assert_eq!(s.state, asking);
    }

    fn running(started: SystemTime) -> AgentSlot {
        slot_at(
            ActivityState::Running {
                item: Some(ToolUse {
                    tool_use_id: None,
                    detail: None,
                    kind: ToolKind::Other,
                }),
            },
            started,
        )
    }

    #[test]
    fn enter_running_accumulates_prior_running_span_and_resets_timers() {
        let t0 = SystemTime::now();
        let mut s = running(t0);
        enter_running(&mut s, call("c1"), t0 + Duration::from_secs(1));
        assert_eq!(s.running_ms, 1000, "prior Running span folded in");
        assert_eq!(s.state_started_at, t0 + Duration::from_secs(1));
        assert_eq!(s.last_event_at, t0 + Duration::from_secs(1));
    }

    #[test]
    fn enter_delegating_from_requires_action_does_not_accumulate_or_stamp_last_event() {
        let t0 = SystemTime::now();
        let mut s = slot_at(
            ActivityState::RequiresAction {
                reason: Some(Arc::from("perm")),
            },
            t0,
        );
        enter_delegating(&mut s, None, t0 + Duration::from_secs(5));
        assert_eq!(s.running_ms, 0, "RequiresAction time is not Running time");
        assert_eq!(
            s.last_event_at, t0,
            "enter_delegating leaves last_event_at to refresh_lineage"
        );
        match &s.state {
            ActivityState::Running {
                item: Some(ToolUse { detail, kind, .. }),
            } => {
                assert_eq!(detail.as_deref(), Some("Delegating"));
                assert_eq!(*kind, ToolKind::Task, "delegation is typed, not spelled");
            }
            other => panic!("expected Running(Delegating), got {other:?}"),
        }
    }

    #[test]
    fn enter_requires_action_accumulates_from_running() {
        let t0 = SystemTime::now();
        let mut s = running(t0);
        enter_requires_action(&mut s, Some(Arc::from("perm")), t0 + Duration::from_secs(2));
        assert_eq!(s.running_ms, 2000);
        assert!(matches!(s.state, ActivityState::RequiresAction { .. }));
    }

    #[test]
    fn settle_folds_the_running_span_up_to_now_and_lands_idle() {
        let t0 = SystemTime::now();
        let now = t0 + Duration::from_secs(1);
        let mut s = running(t0);
        settle(&mut s, ctx(None, false), now);
        assert!(matches!(s.state, ActivityState::Idle));
        assert_eq!(s.running_ms, 1000);
        assert_eq!(s.state_started_at, now);
    }

    #[test]
    fn settle_resolves_a_requires_action_slot_without_accumulating() {
        let t0 = SystemTime::now();
        let now = t0 + Duration::from_secs(3);
        let mut s = slot_at(
            ActivityState::RequiresAction {
                reason: Some(Arc::from("perm")),
            },
            t0,
        );
        settle(&mut s, ctx(None, false), now);
        assert!(matches!(s.state, ActivityState::Idle));
        assert_eq!(
            s.running_ms, 0,
            "RequiresAction time is not folded into running_ms"
        );
        assert_eq!(s.state_started_at, now);
    }

    #[test]
    fn settle_lands_on_delegating_while_a_task_is_in_flight() {
        let t0 = SystemTime::now();
        let now = t0 + Duration::from_secs(2);
        let mut s = running(t0);
        settle(&mut s, ctx(Some("task-1"), false), now);
        match &s.state {
            ActivityState::Running {
                item: Some(ToolUse {
                    tool_use_id, kind, ..
                }),
            } => {
                assert_eq!(tool_use_id.as_deref(), Some("task-1"));
                assert_eq!(*kind, ToolKind::Task);
            }
            other => panic!("expected Delegating, got {other:?}"),
        }
        assert_eq!(s.running_ms, 2000, "the tool's span ended");
        assert_eq!(s.state_started_at, now);
    }

    #[test]
    fn settle_onto_the_current_state_keeps_its_clock() {
        let t0 = SystemTime::now();
        let now = t0 + Duration::from_secs(5);
        let mut s = slot_at(ActivityState::Idle, t0);
        settle(&mut s, ctx(None, false), now);
        assert!(matches!(s.state, ActivityState::Idle));
        assert_eq!(s.state_started_at, t0, "Idle isn't re-stamped");

        let mut s = running(t0);
        enter_delegating(&mut s, Some(Arc::from("task-1")), t0);
        let folded = s.running_ms;
        settle(&mut s, ctx(Some("task-2"), false), now);
        match &s.state {
            ActivityState::Running {
                item: Some(ToolUse { tool_use_id, .. }),
            } => {
                assert_eq!(
                    tool_use_id.as_deref(),
                    Some("task-2"),
                    "it shows a Task still in flight"
                );
            }
            other => panic!("expected Delegating, got {other:?}"),
        }
        assert_eq!(s.state_started_at, t0, "Delegating isn't re-stamped");
        assert_eq!(s.running_ms, folded, "nor its running span folded early");
    }

    #[test]
    fn mark_exiting_is_write_once() {
        let t0 = SystemTime::now();
        let mut s = running(t0);
        mark_exiting(&mut s, t0);
        mark_exiting(&mut s, t0 + Duration::from_secs(10));
        assert_eq!(s.exiting_at, Some(t0), "first exit time wins");
    }

    #[test]
    fn rename_is_idempotent_but_always_refreshes_liveness() {
        let t0 = SystemTime::now();
        let mut s = running(t0);
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
        let mut s = running(t0);
        s.label = crate::state::SlotLabel::ordinal_ghost("cc#1");
        rename(&mut s, "cc#1".into(), t0);
        assert!(!s.label.is_upgradable());
    }
}
