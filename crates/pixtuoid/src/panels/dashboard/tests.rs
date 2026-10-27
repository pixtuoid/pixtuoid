use super::*;

use std::path::Path;
use std::time::SystemTime;

use pixtuoid_core::AgentId;
use pixtuoid_core::state::{ActivityState, AgentSlot, GlobalDeskIndex, ToolKind};

/// Build a slot with the fields the dashboard reads; the rest are inert.
fn mk_slot(
    id: AgentId,
    label: &str,
    desk_index: usize,
    floor_idx: usize,
    parent_id: Option<AgentId>,
    state: ActivityState,
) -> AgentSlot {
    let now = SystemTime::UNIX_EPOCH;
    AgentSlot {
        agent_id: id,
        source: Arc::from("claude-code"),
        session_id: Arc::from("s"),
        cwd: Arc::from(Path::new("/repo")),
        label: label.into(),
        state,
        state_started_at: now,
        created_at: now,
        last_event_at: now,
        exiting_at: None,
        desk_index: GlobalDeskIndex(desk_index),
        floor_idx,
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

fn id(p: &str) -> AgentId {
    AgentId::from_transcript_path(p)
}

#[test]
fn rows_are_tree_ordered_roots_by_desk_index_children_following_parent() {
    let a = id("/p/a.jsonl");
    let b = id("/p/b.jsonl");
    let a_sub = id("/p/a/subagents/agent-x.jsonl");

    let mut scene = SceneState::uniform(8);
    // Inserted out of desk order, and the child has a HIGHER desk_index than root `b`
    // — so BTreeMap/AgentId order would give a different answer.
    scene
        .agents
        .insert(b, mk_slot(b, "cc·b", 1, 0, None, ActivityState::Idle));
    scene
        .agents
        .insert(a, mk_slot(a, "cc·a", 0, 0, None, ActivityState::Idle));
    scene.agents.insert(
        a_sub,
        mk_slot(a_sub, "explorer", 5, 1, Some(a), ActivityState::Idle),
    );

    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());

    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_ref()).collect();
    assert_eq!(labels, ["cc·a", "explorer", "cc·b"]);

    let depths: Vec<u8> = rows.iter().map(|r| r.depth).collect();
    assert_eq!(depths, [0, 1, 0]);

    assert_eq!(rows[1].parent_id, Some(a));
    assert_eq!(rows[1].floor_idx, 1);
    assert_eq!(rows[0].child_count, 1);
    assert_eq!(rows[2].child_count, 0);
}

/// A root plus `n` direct subagents (desks 1..=n, floor 1).
fn root_with_children(n: usize) -> (SceneState, AgentId, usize) {
    let root = id("/p/root.jsonl");
    let mut scene = SceneState::uniform(16);
    scene.agents.insert(
        root,
        mk_slot(root, "cc·root", 0, 0, None, ActivityState::Idle),
    );
    for i in 0..n {
        let c = id(&format!("/p/root/subagents/agent-{i}.jsonl"));
        scene.agents.insert(
            c,
            mk_slot(
                c,
                &format!("sub{i}"),
                1 + i,
                1,
                Some(root),
                ActivityState::Idle,
            ),
        );
    }
    (scene, root, n)
}

#[test]
fn orphan_whose_parent_is_absent_is_treated_as_root() {
    let ghost = id("/p/ghost.jsonl"); // never inserted into the scene
    let orphan = id("/p/orphan.jsonl");
    let mut scene = SceneState::uniform(8);
    scene.agents.insert(
        orphan,
        mk_slot(orphan, "cc·orphan", 0, 2, Some(ghost), ActivityState::Idle),
    );

    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].depth, 0);
    assert_eq!(
        rows[0].parent_id, None,
        "an orphan shows as a root, no visible parent"
    );
    assert_eq!(rows[0].floor_idx, 2);
}

#[test]
fn root_over_threshold_auto_collapses_and_hides_its_subtree() {
    let (scene, _root, n) = root_with_children(AUTO_COLLAPSE_THRESHOLD + 1);
    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    assert_eq!(
        rows.len(),
        1,
        "a >threshold root auto-collapses; subtree hidden"
    );
    assert!(rows[0].collapsed);
    assert_eq!(rows[0].child_count, n);
}

#[test]
fn root_at_threshold_stays_expanded() {
    let (scene, _root, n) = root_with_children(AUTO_COLLAPSE_THRESHOLD);
    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    assert_eq!(rows.len(), 1 + n, "≤threshold root stays expanded");
    assert!(!rows[0].collapsed);
}

#[test]
fn unfold_all_overrides_auto_collapse_and_sticks() {
    let (scene, root, n) = root_with_children(AUTO_COLLAPSE_THRESHOLD + 1);
    let mut folds = DashboardFolds::default();
    assert!(
        build_dashboard_rows(&scene, &folds)[0].collapsed,
        "auto-collapsed before the override"
    );
    folds.unfold_all([root]);
    let rows = build_dashboard_rows(&scene, &folds);
    assert!(
        !rows[0].collapsed,
        "explicit unfold overrides auto-collapse"
    );
    assert_eq!(rows.len(), 1 + n);
}

#[test]
fn fold_all_then_unfold_all_flip_every_root() {
    let (scene, root, _n) = root_with_children(2);
    let mut folds = DashboardFolds::default();
    folds.fold_all([root]);
    assert!(build_dashboard_rows(&scene, &folds)[0].collapsed);
    folds.unfold_all([root]);
    assert!(!build_dashboard_rows(&scene, &folds)[0].collapsed);
}

/// `n` flat root agents at desks/floors `0..n` — `floor_idx == desk_index` here so
/// `resolve_floor` is checkable, and rows come back in desk order (`rows[i] == i`).
fn flat_rows(n: usize) -> Vec<DashboardRow> {
    let mut scene = SceneState::uniform(n.max(1));
    for i in 0..n {
        let a = id(&format!("/p/{i}.jsonl"));
        scene.agents.insert(
            a,
            mk_slot(a, &format!("cc·{i}"), i, i, None, ActivityState::Idle),
        );
    }
    build_dashboard_rows(&scene, &DashboardFolds::default())
}

#[test]
fn move_selection_steps_and_clamps_and_reanchors() {
    let rows = flat_rows(3);
    let r = |i: usize| rows[i].agent_id;
    assert_eq!(move_selection(&rows, Some(r(0)), 1), Some(r(1)));
    assert_eq!(
        move_selection(&rows, Some(r(2)), 1),
        Some(r(2)),
        "clamp at bottom"
    );
    assert_eq!(
        move_selection(&rows, Some(r(0)), -1),
        Some(r(0)),
        "clamp at top"
    );
    assert_eq!(
        move_selection(&rows, None, 1),
        Some(r(0)),
        "no selection -> first"
    );
    let gone = id("/p/gone.jsonl");
    assert_eq!(
        move_selection(&rows, Some(gone), 1),
        Some(r(0)),
        "vanished -> first"
    );
    assert_eq!(move_selection(&[], None, 1), None, "empty -> none");
}

#[test]
fn reanchor_keeps_visible_else_first() {
    let rows = flat_rows(3);
    let r = |i: usize| rows[i].agent_id;
    assert_eq!(
        reanchor_selection(&rows, Some(r(1))),
        Some(r(1)),
        "visible kept"
    );
    let gone = id("/p/gone.jsonl");
    assert_eq!(
        reanchor_selection(&rows, Some(gone)),
        Some(r(0)),
        "vanished -> first"
    );
    assert_eq!(reanchor_selection(&rows, None), Some(r(0)));
    assert_eq!(reanchor_selection(&[], Some(r(0))), None);
}

#[test]
fn resolve_floor_finds_selected_agents_floor() {
    let rows = flat_rows(4);
    assert_eq!(resolve_floor(&rows, rows[2].agent_id), Some(2));
    assert_eq!(resolve_floor(&rows, id("/p/absent.jsonl")), None);
}

#[test]
fn clamp_scroll_keeps_selection_in_view() {
    let rows = flat_rows(10);
    let r = |i: usize| rows[i].agent_id;
    assert_eq!(
        clamp_scroll(&rows, Some(r(0)), 0, 4),
        0,
        "top already in view"
    );
    assert_eq!(
        clamp_scroll(&rows, Some(r(5)), 0, 4),
        2,
        "scroll down: 5+1-4"
    );
    assert_eq!(
        clamp_scroll(&rows, Some(r(1)), 5, 4),
        1,
        "scroll up above window"
    );
    assert_eq!(
        clamp_scroll(&rows, Some(r(3)), 2, 4),
        2,
        "already visible -> unchanged"
    );
}

#[test]
fn clamp_scroll_edge_cases() {
    let rows = flat_rows(6);
    let r = |i: usize| rows[i].agent_id;
    assert_eq!(clamp_scroll(&rows, None, 3, 4), 0, "no selection -> 0");
    assert_eq!(
        clamp_scroll(&rows, Some(id("/p/gone.jsonl")), 3, 4),
        3,
        "selection not in rows -> scroll unchanged"
    );
    // A zero-height viewport must not panic (saturating arithmetic).
    let _ = clamp_scroll(&rows, Some(r(5)), 0, 0);
}

#[test]
fn build_rows_carries_requires_action_and_running_state() {
    let w = id("/p/w.jsonl");
    let a = id("/p/a.jsonl");
    let mut scene = SceneState::uniform(8);
    scene.agents.insert(
        w,
        mk_slot(
            w,
            "cc·w",
            0,
            0,
            None,
            ActivityState::RequiresAction {
                reason: Some(Arc::from("permission")),
            },
        ),
    );
    scene.agents.insert(
        a,
        mk_slot(
            a,
            "cc·a",
            1,
            0,
            None,
            ActivityState::Running {
                tool_use_id: None,
                detail: Some(Arc::from("Edit x")),
                kind: ToolKind::Edit,
            },
        ),
    );
    let rows = build_dashboard_rows(&scene, &DashboardFolds::default());
    assert_eq!(
        rows[0].state,
        RowState::RequiresAction(Some(Arc::from("permission")))
    );
    assert_eq!(rows[1].state, RowState::Running(Some(Arc::from("Edit x"))));
}

#[test]
fn build_dashboard_rows_empty_scene_is_empty() {
    let scene = SceneState::uniform(8);
    assert!(build_dashboard_rows(&scene, &DashboardFolds::default()).is_empty());
}
