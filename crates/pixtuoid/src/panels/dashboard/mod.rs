//! The agent dashboard model: a `SceneState` flattened into a navigable
//! parent→subagent row list, plus the fold + selection logic. PURE — no
//! ratatui (the painter lives in `panels::widgets::dashboard`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pixtuoid_core::AgentId;
use pixtuoid_core::state::{ActivityState, AgentSlot, SceneState};

/// Roots with more than this many direct subagents render collapsed by default,
/// so a large workflow doesn't flood the board.
pub(crate) const AUTO_COLLAPSE_THRESHOLD: usize = 5;

/// Inner visible-row count, shared by `clamp_scroll` and the painter so the
/// scroll math and the painted window can't disagree.
pub(crate) const DASHBOARD_VIEWPORT_ROWS: usize = 16;

#[derive(Debug, Clone, Default)]
pub struct DashboardFrame {
    pub open: bool,
    pub rows: Vec<DashboardRow>,
    pub selected: Option<AgentId>,
    pub scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowState {
    Running(Option<Arc<str>>),
    RequiresAction(Option<Arc<str>>),
    Idle,
}

#[derive(Debug, Clone)]
pub struct DashboardRow {
    pub agent_id: AgentId,
    /// Carried so the event loop can collapse a child's parent without re-querying.
    pub parent_id: Option<AgentId>,
    /// 0 = root, 1 = subagent.
    pub depth: u8,
    pub label: Arc<str>,
    pub source: Arc<str>,
    pub floor_idx: usize,
    pub state: RowState,
    pub child_count: usize,
    pub collapsed: bool,
}

/// Per-session fold state for root agents; persists across open/close.
#[derive(Debug, Default)]
pub struct DashboardFolds {
    collapsed: HashSet<AgentId>,
    user_toggled: HashSet<AgentId>,
}

impl DashboardFolds {
    fn is_collapsed(&self, root_id: AgentId, child_count: usize) -> bool {
        if self.user_toggled.contains(&root_id) {
            self.collapsed.contains(&root_id)
        } else {
            child_count > AUTO_COLLAPSE_THRESHOLD
        }
    }

    /// Collapse every given root and pin it; roots that appear later are not
    /// pinned, so they still auto-evaluate.
    pub(crate) fn fold_all(&mut self, roots: impl IntoIterator<Item = AgentId>) {
        for root in roots {
            self.user_toggled.insert(root);
            self.collapsed.insert(root);
        }
    }

    pub(crate) fn unfold_all(&mut self, roots: impl IntoIterator<Item = AgentId>) {
        for root in roots {
            self.user_toggled.insert(root);
            self.collapsed.remove(&root);
        }
    }
}

/// Flatten the scene into a tree-ordered row list: roots sorted by
/// `desk_index`, each immediately followed by its visible subagents. An agent
/// whose `parent_id` is absent from the scene (an orphan) anchors its own
/// subtree, and nesting deeper than two levels is walked too, so nothing is
/// ever silently dropped.
pub fn build_dashboard_rows(scene: &SceneState, folds: &DashboardFolds) -> Vec<DashboardRow> {
    let mut children: HashMap<AgentId, Vec<&AgentSlot>> = HashMap::new();
    for slot in scene.agents.values() {
        if let Some(parent) = slot.parent_id
            && scene.agents.contains_key(&parent)
        {
            children.entry(parent).or_default().push(slot);
        }
    }

    let mut roots: Vec<&AgentSlot> = scene
        .agents
        .values()
        .filter(|s| s.parent_id.is_none_or(|p| !scene.agents.contains_key(&p)))
        .collect();
    roots.sort_by_key(|s| s.desk_index);

    let mut rows = Vec::new();
    for root in roots {
        push_subtree(&children, folds, root, 0, None, &mut rows);
    }
    rows
}

fn push_subtree(
    children: &HashMap<AgentId, Vec<&AgentSlot>>,
    folds: &DashboardFolds,
    node: &AgentSlot,
    depth: u8,
    parent_id: Option<AgentId>,
    rows: &mut Vec<DashboardRow>,
) {
    let mut kids = children.get(&node.agent_id).cloned().unwrap_or_default();
    let child_count = kids.len();
    let collapsed = depth == 0 && folds.is_collapsed(node.agent_id, child_count);
    rows.push(row_for(node, parent_id, depth, child_count, collapsed));
    if collapsed {
        return;
    }

    kids.sort_by_key(|s| s.desk_index);
    for kid in kids {
        push_subtree(children, folds, kid, depth + 1, Some(node.agent_id), rows);
    }
}

fn row_for(
    slot: &AgentSlot,
    parent_id: Option<AgentId>,
    depth: u8,
    child_count: usize,
    collapsed: bool,
) -> DashboardRow {
    DashboardRow {
        agent_id: slot.agent_id,
        parent_id,
        depth,
        label: slot.label.text(),
        source: slot.source.clone(),
        floor_idx: slot.floor_idx,
        state: row_state(&slot.state),
        child_count,
        collapsed,
    }
}

fn row_state(state: &ActivityState) -> RowState {
    match state {
        ActivityState::Running { detail, .. } => RowState::Running(detail.clone()),
        ActivityState::RequiresAction { reason } => RowState::RequiresAction(reason.clone()),
        ActivityState::Idle => RowState::Idle,
    }
}

/// Move the selection one visible row up (`dir = -1`) or down (`dir = +1`),
/// clamped at the ends.
pub(crate) fn move_selection(
    rows: &[DashboardRow],
    current: Option<AgentId>,
    dir: i32,
) -> Option<AgentId> {
    if rows.is_empty() {
        return None;
    }
    let new_idx = match current.and_then(|c| rows.iter().position(|r| r.agent_id == c)) {
        Some(i) => (i as i32 + dir).clamp(0, rows.len() as i32 - 1) as usize,
        None => 0,
    };
    Some(rows[new_idx].agent_id)
}

pub(crate) fn reanchor_selection(
    rows: &[DashboardRow],
    current: Option<AgentId>,
) -> Option<AgentId> {
    match current {
        Some(c) if rows.iter().any(|r| r.agent_id == c) => Some(c),
        _ => rows.first().map(|r| r.agent_id),
    }
}

pub(crate) fn resolve_floor(rows: &[DashboardRow], selected: AgentId) -> Option<usize> {
    rows.iter()
        .find(|r| r.agent_id == selected)
        .map(|r| r.floor_idx)
}

pub(crate) fn clamp_scroll(
    rows: &[DashboardRow],
    selected: Option<AgentId>,
    scroll: usize,
    visible_height: usize,
) -> usize {
    // A `None` selection resets to the top HERE, unlike `clamp_scroll_idx`,
    // which keeps `scroll`.
    let Some(sel) = selected else {
        return 0;
    };
    match rows.iter().position(|r| r.agent_id == sel) {
        Some(idx) => clamp_scroll_idx(Some(idx), scroll, visible_height),
        None => scroll,
    }
}

/// The pure index core [`clamp_scroll`] and the panel's `window_range` both
/// use, so every list panel slides its viewport identically.
pub(crate) fn clamp_scroll_idx(
    selected: Option<usize>,
    scroll: usize,
    visible_height: usize,
) -> usize {
    let Some(idx) = selected else {
        return scroll;
    };
    if idx < scroll {
        idx
    } else if visible_height > 0 && idx >= scroll + visible_height {
        idx + 1 - visible_height
    } else {
        scroll
    }
}

/// Only `open` flips on close, so folds + selection survive close/reopen for
/// the session.
#[derive(Debug, Default)]
pub(crate) struct DashboardUi {
    pub open: bool,
    pub selected: Option<AgentId>,
    pub scroll: usize,
    pub folds: DashboardFolds,
}

#[cfg(test)]
mod tests;
