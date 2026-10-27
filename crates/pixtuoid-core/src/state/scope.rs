//! The agent **scope** layer (Layer B) — the parent↔subagent tree over
//! `AgentSlot.parent_id`, and the lifecycle rules that propagate along it. It
//! encodes one invariant — *a subagent's lifetime is contained in its parent's*
//! — as directional operations the reducer delegates to:
//!
//! - **exit flows DOWN** — [`cascade_exit`]: a node leaving takes its whole
//!   subtree.
//! - **liveness flows UP** — [`refresh_lineage`]: a working descendant keeps its
//!   ancestors alive, so a blocked-but-delegating parent isn't stale-swept.
//! - **readiness, queried both ways** — [`has_requires_action_ancestor`] and
//!   [`has_requires_action_descendant`]: a node gated by a `RequiresAction` relative is "not ready", not dead.

use std::collections::{BTreeMap, HashSet};
use std::time::SystemTime;

use crate::AgentId;
use crate::state::{ActivityState, AgentSlot, SceneState, fsm};

/// Whether [`cascade_exit`] also marks the `root` seed itself exiting — EXPLICIT
/// at each call site, replacing the old implicit "did the caller stamp
/// `root.exiting_at` before calling?" convention that had no assertion to catch
/// a caller that forgot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StampRoot {
    /// The whole tree leaves together — `root` is marked exiting too.
    Yes,
    /// Only the subtree leaves; `root` keeps running (the subagent-completion
    /// cascade: the parent stays alive as its finished children walk out).
    No,
}

/// Mark every not-yet-exiting descendant of `root` exiting, over `parent_id`
/// links — and, when `stamp_root` is [`StampRoot::Yes`], mark `root` itself
/// exiting first (via [`fsm::mark_exiting`], so the earliest `exiting_at` wins).
/// Idempotent through that write-once stamp, NOT by pruning the walk: an
/// already-exiting node keeps its original `exiting_at` but is still traversed,
/// so a descendant registered under it after it started exiting is reached.
pub(crate) fn cascade_exit(
    scene: &mut SceneState,
    root: AgentId,
    stamp_root: StampRoot,
    now: SystemTime,
) {
    if stamp_root == StampRoot::Yes
        && let Some(slot) = scene.agents.get_mut(&root)
    {
        fsm::mark_exiting(slot, now);
    }
    for cid in descendants(&scene.agents, root) {
        if let Some(slot) = scene.agents.get_mut(&cid) {
            fsm::mark_exiting(slot, now);
        }
    }
}

/// Every descendant of `root` over `parent_id` links; a cycle is walked once.
fn descendants(agents: &BTreeMap<AgentId, AgentSlot>, root: AgentId) -> Vec<AgentId> {
    let mut visited: HashSet<AgentId> = HashSet::new();
    visited.insert(root);
    let mut frontier = vec![root];
    let mut out = Vec::new();
    while let Some(parent) = frontier.pop() {
        for child in agents.values().filter(|s| s.parent_id == Some(parent)) {
            if visited.insert(child.agent_id) {
                out.push(child.agent_id);
                frontier.push(child.agent_id);
            }
        }
    }
    out
}

/// True if any descendant of `id` is in `RequiresAction` state: a parent whose
/// subagent is blocked on a human gate waits with it, not dead.
pub(crate) fn has_requires_action_descendant(
    agents: &BTreeMap<AgentId, AgentSlot>,
    id: AgentId,
) -> bool {
    descendants(agents, id).into_iter().any(|d| {
        agents
            .get(&d)
            .is_some_and(|s| matches!(s.state, ActivityState::RequiresAction { .. }))
    })
}

/// Refresh `last_event_at` for `id` and every ancestor, so a parent isn't
/// stale-swept while a descendant is still emitting events. `last_event_at` only
/// gates the stale-sweep, so this never alters an ancestor's visible state/pose.
/// The `None => break` arm deliberately tolerates a DANGLING `parent_id` — a
/// JSONL-first orphan, or a parent `sweep_exited` removed without nulling its
/// children's `parent_id`.
pub(crate) fn refresh_lineage(scene: &mut SceneState, id: AgentId, now: SystemTime) {
    let mut visited: HashSet<AgentId> = HashSet::new();
    let mut cur = Some(id);
    while let Some(aid) = cur {
        if !visited.insert(aid) {
            break;
        }
        match scene.agents.get_mut(&aid) {
            Some(slot) => {
                slot.last_event_at = now;
                cur = slot.parent_id;
            }
            None => break,
        }
    }
}

/// True if any ancestor of `id` (walking `parent_id`, the node itself excluded)
/// satisfies `pred` — the ONE ancestor walk behind the readiness queries, so the
/// cycle guard and dangling-parent tolerance can't fork. Takes `&BTreeMap`
/// rather than `&SceneState` so it can be called inside `sweep_stale`'s pass-1
/// closure while `&scene.agents` is already borrowed immutably.
pub(crate) fn has_ancestor_where(
    agents: &BTreeMap<AgentId, AgentSlot>,
    id: AgentId,
    pred: impl Fn(&AgentSlot) -> bool,
) -> bool {
    // Seeded with the start node: in a parent cycle the walk returns to `id` and
    // would otherwise run `pred` on it before the guard breaks — a RequiresAction cycle
    // member counting as its own RequiresAction ancestor self-exempts from sweep_stale
    // forever.
    let mut visited: HashSet<AgentId> = HashSet::from([id]);
    let mut cur = agents.get(&id).and_then(|s| s.parent_id);
    while let Some(pid) = cur {
        if !visited.insert(pid) {
            break;
        }
        match agents.get(&pid) {
            Some(p) if pred(p) => return true,
            Some(p) => cur = p.parent_id,
            None => break,
        }
    }
    false
}

/// True if linking `child.parent_id = proposed_parent` would close a
/// `parent_id` cycle. The reducer calls this at every seam that sets or enriches
/// `parent_id` and REFUSES the link (degrading to parentless), so a cycle can
/// never EXIST: a 2-cycle whose members are BOTH `RequiresAction` would mutually
/// satisfy [`has_requires_action_ancestor`] and exempt each other from the stale sweep
/// forever — an immortal pair (#238).
pub(crate) fn would_create_cycle(
    agents: &BTreeMap<AgentId, AgentSlot>,
    child: AgentId,
    proposed_parent: AgentId,
) -> bool {
    if proposed_parent == child {
        return true;
    }
    let mut visited: HashSet<AgentId> = HashSet::from([proposed_parent]);
    let mut cur = agents.get(&proposed_parent).and_then(|s| s.parent_id);
    while let Some(pid) = cur {
        if pid == child {
            return true;
        }
        if !visited.insert(pid) {
            break;
        }
        cur = agents.get(&pid).and_then(|s| s.parent_id);
    }
    false
}

/// True if any ancestor of `id` is in `RequiresAction` state. A subagent's permission
/// event that lands on an ancestor leaves the blocked subagent `Running` — paused
/// on a human gate, not dead, so `sweep_stale` exempts it from the aggressive
/// `STALE_TOOL_TIMEOUT`.
pub(crate) fn has_requires_action_ancestor(
    agents: &BTreeMap<AgentId, AgentSlot>,
    id: AgentId,
) -> bool {
    has_ancestor_where(agents, id, |p| {
        matches!(p.state, ActivityState::RequiresAction { .. })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn slot(id: AgentId, parent_id: Option<AgentId>, state: ActivityState) -> AgentSlot {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        AgentSlot {
            agent_id: id,
            source: Arc::from("cc"),
            session_id: Arc::from("s"),
            cwd: Arc::from(std::path::Path::new("/repo")),
            label: "cc·repo".into(),
            state,
            state_started_at: now,
            last_event_at: now,
            created_at: now,
            exiting_at: None,
            desk_index: crate::state::GlobalDeskIndex(0),
            floor_idx: 0,
            tool_call_count: 0,
            running_ms: 0,
            unknown_cwd: false,
            parent_id,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        }
    }

    fn requires_action() -> ActivityState {
        ActivityState::RequiresAction {
            reason: Some(Arc::from("perm")),
        }
    }

    #[test]
    fn refresh_lineage_tolerates_dangling_parent_id() {
        let child = AgentId::from_transcript_path("/p/child.jsonl");
        let missing = AgentId::from_transcript_path("/p/never-created.jsonl");
        let mut scene = SceneState::uniform(4);
        scene
            .agents
            .insert(child, slot(child, Some(missing), ActivityState::Idle));

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);
        refresh_lineage(&mut scene, child, now);

        assert_eq!(scene.agents.get(&child).unwrap().last_event_at, now);
        assert!(
            !scene.agents.contains_key(&missing),
            "the dangling parent is never materialized by the walk"
        );
    }

    #[test]
    fn has_requires_action_ancestor_false_when_parent_id_dangling() {
        let child = AgentId::from_transcript_path("/p/child.jsonl");
        let missing = AgentId::from_transcript_path("/p/never-created.jsonl");
        let mut scene = SceneState::uniform(4);
        scene
            .agents
            .insert(child, slot(child, Some(missing), ActivityState::Idle));

        assert!(
            !has_requires_action_ancestor(&scene.agents, child),
            "a dangling parent_id is not a RequiresAction ancestor — the walk breaks safely"
        );
    }

    fn cycle_scene(
        a_state: ActivityState,
        b_state: ActivityState,
    ) -> (SceneState, AgentId, AgentId) {
        let a = AgentId::from_transcript_path("/p/a.jsonl");
        let b = AgentId::from_transcript_path("/p/b.jsonl");
        let mut scene = SceneState::uniform(4);
        scene.agents.insert(a, slot(a, Some(b), a_state));
        scene.agents.insert(b, slot(b, Some(a), b_state));
        (scene, a, b)
    }

    #[test]
    fn refresh_lineage_terminates_on_parent_id_cycle() {
        let (mut scene, a, b) = cycle_scene(ActivityState::Idle, ActivityState::Idle);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);

        refresh_lineage(&mut scene, a, now);

        assert_eq!(scene.agents.get(&a).unwrap().last_event_at, now);
        assert_eq!(
            scene.agents.get(&b).unwrap().last_event_at,
            now,
            "the cycle's other node is stamped exactly once before the break"
        );
    }

    #[test]
    fn has_requires_action_ancestor_breaks_on_cycle_with_no_requires_action_node() {
        let (scene, a, _b) = cycle_scene(ActivityState::Idle, ActivityState::Idle);
        assert!(
            !has_requires_action_ancestor(&scene.agents, a),
            "a cycle with no RequiresAction node must terminate and return false"
        );
    }

    #[test]
    fn has_requires_action_ancestor_true_via_cyclic_ancestor() {
        let (scene, a, _b) = cycle_scene(ActivityState::Idle, requires_action());
        assert!(has_requires_action_ancestor(&scene.agents, a));
    }

    #[test]
    fn has_requires_action_ancestor_excludes_self_reached_via_cycle() {
        let (scene, _a, b) = cycle_scene(ActivityState::Idle, requires_action());
        assert!(
            !has_requires_action_ancestor(&scene.agents, b),
            "a node must never count as its own RequiresAction ancestor through a parent cycle"
        );
    }

    #[test]
    fn would_create_cycle_rejects_self_parent() {
        let x = AgentId::from_transcript_path("/p/self.jsonl");
        let scene = SceneState::uniform(4);
        assert!(would_create_cycle(&scene.agents, x, x));
    }

    #[test]
    fn would_create_cycle_rejects_two_node_closure() {
        let a = AgentId::from_transcript_path("/p/a.jsonl");
        let b = AgentId::from_transcript_path("/p/b.jsonl");
        let mut scene = SceneState::uniform(4);
        scene
            .agents
            .insert(a, slot(a, Some(b), ActivityState::Idle));
        scene.agents.insert(b, slot(b, None, ActivityState::Idle));
        assert!(would_create_cycle(&scene.agents, b, a));
    }

    #[test]
    fn would_create_cycle_rejects_deep_chain_closure() {
        let a = AgentId::from_transcript_path("/p/a.jsonl");
        let b = AgentId::from_transcript_path("/p/b.jsonl");
        let c = AgentId::from_transcript_path("/p/c.jsonl");
        let mut scene = SceneState::uniform(4);
        scene.agents.insert(a, slot(a, None, ActivityState::Idle));
        scene
            .agents
            .insert(b, slot(b, Some(a), ActivityState::Idle));
        scene
            .agents
            .insert(c, slot(c, Some(b), ActivityState::Idle));
        assert!(would_create_cycle(&scene.agents, a, c));
    }

    #[test]
    fn would_create_cycle_allows_legitimate_and_dangling_links() {
        let a = AgentId::from_transcript_path("/p/a.jsonl");
        let b = AgentId::from_transcript_path("/p/b.jsonl");
        let ghost = AgentId::from_transcript_path("/p/never-created.jsonl");
        let mut scene = SceneState::uniform(4);
        scene.agents.insert(b, slot(b, None, ActivityState::Idle));
        assert!(!would_create_cycle(&scene.agents, a, b));
        assert!(!would_create_cycle(&scene.agents, a, ghost));
    }

    #[test]
    fn would_create_cycle_terminates_on_preexisting_cycle_elsewhere() {
        let (mut scene, a, _b) = cycle_scene(ActivityState::Idle, ActivityState::Idle);
        let child = AgentId::from_transcript_path("/p/child.jsonl");
        scene
            .agents
            .insert(child, slot(child, None, ActivityState::Idle));
        assert!(!would_create_cycle(&scene.agents, child, a));
    }

    #[test]
    fn cascade_exit_reaches_a_descendant_added_under_an_already_exiting_node() {
        let root = AgentId::from_transcript_path("/p/root.jsonl");
        let mid = AgentId::from_transcript_path("/p/mid.jsonl");
        let leaf = AgentId::from_transcript_path("/p/leaf.jsonl");
        let mut scene = SceneState::uniform(8);
        scene
            .agents
            .insert(root, slot(root, None, ActivityState::Idle));
        scene
            .agents
            .insert(mid, slot(mid, Some(root), ActivityState::Idle));
        scene
            .agents
            .insert(leaf, slot(leaf, Some(mid), ActivityState::Idle));

        let t_mid = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);
        fsm::mark_exiting(scene.agents.get_mut(&mid).unwrap(), t_mid);

        let t_root = t_mid + Duration::from_secs(2);
        cascade_exit(&mut scene, root, StampRoot::Yes, t_root);

        assert_eq!(scene.agents[&root].exiting_at, Some(t_root));
        assert_eq!(
            scene.agents[&mid].exiting_at,
            Some(t_mid),
            "the walk must not RE-stamp an exiting node — that would reset its \
             walkout clock and push its GC out by another grace window"
        );
        assert_eq!(
            scene.agents[&leaf].exiting_at,
            Some(t_root),
            "the grandchild added under the exiting node must still be reached"
        );
    }

    #[test]
    fn has_requires_action_ancestor_excludes_self_parented_node() {
        let x = AgentId::from_transcript_path("/p/self.jsonl");
        let mut scene = SceneState::uniform(4);
        scene.agents.insert(x, slot(x, Some(x), requires_action()));
        assert!(
            !has_requires_action_ancestor(&scene.agents, x),
            "a self-parented RequiresAction node is not its own ancestor"
        );
    }
}
