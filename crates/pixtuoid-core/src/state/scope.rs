//! The agent **scope** layer (Layer B) — the parent↔subagent tree over
//! `AgentSlot.parent_id`, and the lifecycle rules that propagate along it. It
//! encodes one invariant — *a subagent's lifetime is contained in its parent's*
//! — as directional operations the reducer delegates to:
//!
//! - **exit flows DOWN** — [`cascade_exit`]: a node leaving takes its whole
//!   subtree.
//! - **liveness flows UP** — [`refresh_lineage`]: a working descendant keeps its
//!   ancestors alive, so a blocked-but-delegating parent isn't stale-swept.
//! - **readiness, queried both ways** — [`ScopeTree::descendants_of`] and
//!   [`ScopeTree::ancestors_of`]: a node gated by a `RequiresAction` relative is "not ready", not dead.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::SystemTime;

use crate::AgentId;
use crate::state::{AgentSlot, SceneState, fsm};

#[cfg(test)]
thread_local! {
    /// The slots the scope walks have visited on this thread: a pass's work,
    /// which a test bounds by the slot count, never by wall clock.
    pub(crate) static VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn visit() {
    #[cfg(test)]
    VISITS.with(|v| v.set(v.get() + 1));
}

/// The scene's `parent_id` links, indexed once so a pass asks every slot's
/// scope question of it instead of rescanning the scene per slot.
#[derive(Debug)]
pub struct ScopeTree<'a> {
    agents: &'a BTreeMap<AgentId, AgentSlot>,
    /// Keyed by every `parent_id`, a dangling one too: a cascade from a removed
    /// parent still reaches the orphans it left.
    children: HashMap<AgentId, Vec<&'a AgentSlot>>,
}

impl<'a> ScopeTree<'a> {
    /// Indexes `agents`.
    pub fn of(agents: &'a BTreeMap<AgentId, AgentSlot>) -> Self {
        let mut children: HashMap<AgentId, Vec<&AgentSlot>> = HashMap::new();
        for slot in agents.values() {
            visit();
            if let Some(parent) = slot.parent_id {
                children.entry(parent).or_default().push(slot);
            }
        }
        Self { agents, children }
    }

    /// The slots with no parent slot, a dangling `parent_id`'s included.
    pub fn roots(&self) -> impl Iterator<Item = &'a AgentSlot> {
        self.agents
            .values()
            .filter(|s| s.parent_id.is_none_or(|p| !self.agents.contains_key(&p)))
    }

    /// The slots whose `parent_id` is `id`.
    pub fn children(&self, id: AgentId) -> &[&'a AgentSlot] {
        visit();
        self.children.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Every slot under a root, after the parent it hangs from (`None` for a
    /// root). A parent cycle has no root, so no walk enters one: a member can't
    /// count another, or itself, as its relative (#238).
    fn preorder(&self) -> Vec<(&'a AgentSlot, Option<&'a AgentSlot>)> {
        let mut frontier: Vec<_> = self.roots().map(|s| (s, None)).collect();
        let mut order = Vec::with_capacity(self.agents.len());
        while let Some((slot, parent)) = frontier.pop() {
            order.push((slot, parent));
            frontier.extend(
                self.children(slot.agent_id)
                    .iter()
                    .map(|&child| (child, Some(slot))),
            );
        }
        order
    }

    /// Every descendant of a slot satisfying `pred`.
    pub(crate) fn descendants_of(&self, pred: impl Fn(&AgentSlot) -> bool) -> HashSet<AgentId> {
        let mut out = HashSet::new();
        for (slot, parent) in self.preorder() {
            if parent.is_some_and(|p| out.contains(&p.agent_id) || pred(p)) {
                out.insert(slot.agent_id);
            }
        }
        out
    }

    /// Every ancestor of a slot satisfying `pred`.
    pub(crate) fn ancestors_of(&self, pred: impl Fn(&AgentSlot) -> bool) -> HashSet<AgentId> {
        let mut out = HashSet::new();
        // Children first, so a slot's verdict is whole before its parent reads it.
        for (slot, parent) in self.preorder().into_iter().rev() {
            if let Some(p) = parent
                && (out.contains(&slot.agent_id) || pred(slot))
            {
                out.insert(p.agent_id);
            }
        }
        out
    }
}

/// Whether [`cascade_exit`] also marks each root seed itself exiting — EXPLICIT
/// at each call site, replacing the old implicit "did the caller stamp
/// `root.exiting_at` before calling?" convention that had no assertion to catch
/// a caller that forgot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StampRoot {
    /// The whole tree leaves together — the root is marked exiting too.
    Yes,
    /// Only the subtree leaves; the root keeps running (the subagent-completion
    /// cascade: the parent stays alive as its finished children walk out).
    No,
}

/// Mark every not-yet-exiting descendant of each of `roots` exiting, over
/// `parent_id` links — and, when `stamp_root` is [`StampRoot::Yes`], each root
/// itself (via [`fsm::mark_exiting`], so the earliest `exiting_at` wins).
/// Idempotent through that write-once stamp, NOT by pruning the walk at an
/// exiting node: one keeps its original `exiting_at` but is still traversed, so
/// a descendant registered under it after it started exiting is reached.
pub(crate) fn cascade_exit(
    scene: &mut SceneState,
    roots: &[AgentId],
    stamp_root: StampRoot,
    now: SystemTime,
) {
    if roots.is_empty() {
        return;
    }
    let tree = ScopeTree::of(&scene.agents);
    let mut doomed: HashSet<AgentId> = HashSet::new();
    for &root in roots {
        // A doomed root's subtree is doomed already, so the roots share one walk.
        if stamp_root == StampRoot::Yes && !doomed.insert(root) {
            continue;
        }
        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            for child in tree.children(parent) {
                if doomed.insert(child.agent_id) {
                    frontier.push(child.agent_id);
                }
            }
        }
    }
    for id in doomed {
        if let Some(slot) = scene.agents.get_mut(&id) {
            fsm::mark_exiting(slot, now);
        }
    }
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

/// True if linking `child.parent_id = proposed_parent` would close a
/// `parent_id` cycle. The reducer calls this at every seam that sets or enriches
/// `parent_id` and REFUSES the link (degrading to parentless), so a cycle can
/// never EXIST: it has no root, so the dashboard's walk down from the roots
/// would drop its members.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ActivityState;
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
            arrived_at: Some(now),
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

    fn is_requires_action(slot: &AgentSlot) -> bool {
        matches!(slot.state, ActivityState::RequiresAction { .. })
    }

    #[test]
    fn readiness_reaches_every_relative_but_never_the_slot_itself() {
        let id = |name: &str| AgentId::from_transcript_path(&format!("/p/{name}.jsonl"));
        let [top, parent, gate, child, grandchild, uncle, orphan, missing] = [
            "top",
            "parent",
            "gate",
            "child",
            "grandchild",
            "uncle",
            "orphan",
            "missing",
        ]
        .map(id);
        let mut scene = SceneState::uniform(8);
        for (agent, parent_id, state) in [
            (top, None, ActivityState::Idle),
            (parent, Some(top), ActivityState::Idle),
            (gate, Some(parent), requires_action()),
            (child, Some(gate), ActivityState::Idle),
            (grandchild, Some(child), ActivityState::Idle),
            (uncle, Some(top), ActivityState::Idle),
            (orphan, Some(missing), ActivityState::Idle),
        ] {
            scene.agents.insert(agent, slot(agent, parent_id, state));
        }

        let tree = ScopeTree::of(&scene.agents);
        assert_eq!(
            tree.descendants_of(is_requires_action),
            HashSet::from([child, grandchild])
        );
        assert_eq!(
            tree.ancestors_of(is_requires_action),
            HashSet::from([parent, top])
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
    fn readiness_never_walks_into_a_parent_cycle() {
        let (mut scene, _a, _b) = cycle_scene(ActivityState::Idle, requires_action());
        let x = AgentId::from_transcript_path("/p/self.jsonl");
        scene.agents.insert(x, slot(x, Some(x), requires_action()));

        let tree = ScopeTree::of(&scene.agents);
        for (query, gated) in [
            ("descendants_of", tree.descendants_of(is_requires_action)),
            ("ancestors_of", tree.ancestors_of(is_requires_action)),
        ] {
            assert!(
                gated.is_empty(),
                "{query}: no cycle member may count another, or itself, as its relative: {gated:?}"
            );
        }
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
        cascade_exit(&mut scene, &[root], StampRoot::Yes, t_root);

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
    fn a_subtree_only_cascade_exits_a_root_nested_under_another_in_either_order() {
        let root = AgentId::from_transcript_path("/p/root.jsonl");
        let mid = AgentId::from_transcript_path("/p/mid.jsonl");
        let leaf = AgentId::from_transcript_path("/p/leaf.jsonl");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);
        for roots in [[root, mid], [mid, root]] {
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

            cascade_exit(&mut scene, &roots, StampRoot::No, now);

            assert_eq!(scene.agents[&root].exiting_at, None, "{roots:?}");
            assert_eq!(
                scene.agents[&mid].exiting_at,
                Some(now),
                "{roots:?}: a root under another root's subtree leaves with it"
            );
            assert_eq!(scene.agents[&leaf].exiting_at, Some(now), "{roots:?}");
        }
    }

    #[test]
    fn a_cascade_from_a_parent_with_no_slot_reaches_the_orphans_naming_it() {
        let orphan = AgentId::from_transcript_path("/p/orphan.jsonl");
        let missing = AgentId::from_transcript_path("/p/never-created.jsonl");
        let mut scene = SceneState::uniform(4);
        scene
            .agents
            .insert(orphan, slot(orphan, Some(missing), ActivityState::Idle));
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);

        cascade_exit(&mut scene, &[missing], StampRoot::Yes, now);

        assert_eq!(scene.agents[&orphan].exiting_at, Some(now));
        assert!(!scene.agents.contains_key(&missing));
    }
}
