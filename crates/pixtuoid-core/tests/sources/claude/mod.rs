//! Claude Code subagent lifecycle (parallels the sibling `codex` module).
//!
//! `fixtures/lifecycle-recorded/` holds REAL SubagentStart/Stop wire payloads,
//! sanitized to synthetic ids and a generic cwd;
//! `last_assistant_message`/`background_tasks` are truncated but KEPT so the
//! decoder's tolerance of fields we don't consume stays pinned.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::claude_code::decode_cc_line;
use pixtuoid_core::source::decoder::decode_hook_payload;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::Reducer;
use serde_json::json;

// The filename stem "parent" IS the session UUID the hook carries, so hook and
// JSONL coalesce on it.
const PARENT_PATH: &str = "/proj/parent.jsonl";
// `detect_parent_id` keys on the `<parent-uuid>` path component — i.e. exactly
// the parent's own AgentId — while the subagent's id is its own stem.
const SUB_PATH: &str = "/proj/parent/subagents/agent-1.jsonl";

fn parent_id() -> AgentId {
    AgentId::from_parts("claude-code", "parent")
}
fn sub_id() -> AgentId {
    AgentId::from_parts("claude-code", "agent-1")
}

#[test]
fn cc_subagent_links_renames_and_cascades_on_parent_exit() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();

    for ev in decode_hook_payload(json!({
        "hook_event_name": "SessionStart",
        "session_id": "parent",
        "transcript_path": PARENT_PATH,
        "cwd": "/home/user/demo-project"
    }))
    .unwrap()
    {
        r.apply(&mut scene, ev, now, Transport::Hook);
    }
    assert!(scene.agents.contains_key(&parent_id()), "parent created");

    // Real CC names the dispatch tool "Agent", not "Task" — Task-detection
    // must still fire so the reducer records a task in flight.
    for ev in decode_hook_payload(json!({
        "hook_event_name": "PreToolUse",
        "session_id": "parent",
        "transcript_path": PARENT_PATH,
        "tool_name": "Agent",
        "tool_input": {"description": "explore", "subagent_type": "general-purpose"},
        "tool_use_id": "task-1"
    }))
    .unwrap()
    {
        r.apply(&mut scene, ev, now, Transport::Hook);
    }

    // Mirrors the watcher's emission when the subagent's own transcript
    // appears, with parent_id derived from the `/subagents/` path.
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: sub_id(),
            source: "claude-code".into(),
            session_id: "parent".into(),
            cwd: PathBuf::from("/home/user/demo-project"),
            parent_id: Some(parent_id()),
        },
        now,
        Transport::Jsonl,
    );

    for ev in decode_cc_line(
        SUB_PATH,
        "claude-code",
        json!({
            "type": "assistant",
            "attributionAgent": "general-purpose",
            "message": {"content": [
                {"type": "tool_use", "id": "s1", "name": "Read", "input": {"file_path": "/x"}}
            ]}
        }),
    ) {
        r.apply(&mut scene, ev, now, Transport::Jsonl);
    }

    let sub = scene.agents.get(&sub_id()).expect("subagent present");
    assert_eq!(
        sub.parent_id,
        Some(parent_id()),
        "subagent linked to its parent via the /subagents/ path"
    );
    assert_eq!(
        &*sub.label, "general-purpose",
        "attributionAgent renames the subagent sprite"
    );

    r.apply(
        &mut scene,
        AgentEvent::SessionEnd {
            agent_id: parent_id(),
            as_child: false,
        },
        now,
        Transport::Hook,
    );
    assert!(
        scene.agents.get(&parent_id()).unwrap().exiting_at.is_some(),
        "parent exiting"
    );
    assert!(
        scene.agents.get(&sub_id()).unwrap().exiting_at.is_some(),
        "CC subagent cascades out with its parent"
    );
}

// The fixture's `agent_id` is BARE hex; these keys carry the `agent-` prefix
// because the transcript filename stem — the watcher's id space — does.
const HOOK_PARENT: &str = "01000000-0000-7000-8000-0000000000cc";
const HOOK_CHILD_GP: &str = "agent-a0000000000000001";
const HOOK_CHILD_WF: &str = "agent-a0000000000000002";

fn hook_parent_id() -> AgentId {
    AgentId::from_parts("claude-code", HOOK_PARENT)
}

/// Decode the captured hook payloads in file order. The records carry NO
/// `_pixtuoid_source` — exactly like production, where CC's hook entry is the
/// bare shim with no env, so the decoder's claude-code default applies.
fn captured_hook_events() -> Vec<AgentEvent> {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("claude/fixtures/lifecycle-recorded/hook-payloads.jsonl"),
    )
    .iter()
    .flat_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).expect("valid hook json");
        decode_hook_payload(v).expect("captured CC subagent hook payload must decode")
    })
    .collect()
}

fn start_parent(r: &mut Reducer, scene: &mut SceneState, now: SystemTime) {
    for ev in decode_hook_payload(json!({
        "hook_event_name": "SessionStart",
        "session_id": HOOK_PARENT,
        "transcript_path": format!(
            "/home/user/.claude/projects/-home-user-demo-project/{HOOK_PARENT}.jsonl"
        ),
        "cwd": "/home/user/demo-project"
    }))
    .unwrap()
    {
        r.apply(scene, ev, now, Transport::Hook);
    }
}

// The Workflow-fleet scenario: no `Agent` tool_use and no JSONL, so hooks
// alone carry the whole lifecycle.
#[test]
fn cc_subagent_hook_pairs_register_link_and_exit_both_agent_types() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();
    start_parent(&mut r, &mut scene, now);

    for ev in captured_hook_events() {
        r.apply(&mut scene, ev, now, Transport::Hook);
    }

    for child_key in [HOOK_CHILD_GP, HOOK_CHILD_WF] {
        let child = AgentId::from_parts("claude-code", child_key);
        let slot = scene
            .agents
            .get(&child)
            .unwrap_or_else(|| panic!("SubagentStart must register {child_key}"));
        assert_eq!(
            slot.parent_id,
            Some(hook_parent_id()),
            "{child_key} must link to the parent session"
        );
        assert!(
            slot.exiting_at.is_some(),
            "SubagentStop must mark {child_key} exiting (the Workflow-fleet \
             desk-starvation fix — no b1, no stale-sweep wait)"
        );
    }
    assert!(
        scene
            .agents
            .get(&hook_parent_id())
            .expect("parent still present")
            .exiting_at
            .is_none(),
        "parent must keep running after its subagents stop"
    );
}

#[test]
fn cc_jsonl_registered_subagent_exits_cleanly_on_hook_subagent_stop() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();
    start_parent(&mut r, &mut scene, now);

    let child = AgentId::from_parts("claude-code", HOOK_CHILD_GP);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: HOOK_CHILD_GP.into(),
            cwd: PathBuf::from("/home/user/demo-project"),
            parent_id: Some(hook_parent_id()),
        },
        now,
        Transport::Jsonl,
    );
    assert!(scene.agents.contains_key(&child), "JSONL registered it");

    for ev in captured_hook_events() {
        r.apply(&mut scene, ev, now, Transport::Hook);
    }
    assert!(
        scene
            .agents
            .get(&child)
            .expect("the hook events must coalesce onto the JSONL slot, not mint a twin")
            .exiting_at
            .is_some(),
        "hook SubagentStop must exit the JSONL-registered subagent slot"
    );
    assert!(
        !scene
            .agents
            .contains_key(&AgentId::from_parts("claude-code", "a0000000000000001")),
        "no bare-keyed phantom twin"
    );
}

#[test]
fn cc_reordered_subagent_stop_before_start_does_not_mint_a_phantom() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();
    start_parent(&mut r, &mut scene, now);

    let (stops, starts): (Vec<_>, Vec<_>) = captured_hook_events()
        .into_iter()
        .partition(|ev| matches!(ev, AgentEvent::SessionEnd { .. }));
    for ev in stops {
        r.apply(&mut scene, ev, now, Transport::Hook);
    }
    // Applied TWICE: the gate must not consume the tombstone.
    for ev in starts.iter().chain(starts.iter()).cloned() {
        r.apply(
            &mut scene,
            ev,
            now + Duration::from_millis(50),
            Transport::Hook,
        );
    }

    for child_key in [HOOK_CHILD_GP, HOOK_CHILD_WF] {
        assert!(
            !scene
                .agents
                .contains_key(&AgentId::from_parts("claude-code", child_key)),
            "{child_key}: a SubagentStart reordered after its own Stop must not register"
        );
    }
    let parent = scene
        .agents
        .get(&hook_parent_id())
        .expect("parent untouched");
    assert!(
        parent.exiting_at.is_none(),
        "the children's tombstones must not affect the parent"
    );
}

#[test]
fn cc_hook_first_subagent_coalesces_with_later_jsonl_session_start() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();
    start_parent(&mut r, &mut scene, now);

    for ev in captured_hook_events() {
        if matches!(ev, AgentEvent::SessionStart { .. }) {
            r.apply(&mut scene, ev, now, Transport::Hook);
        }
    }
    let child = AgentId::from_parts("claude-code", HOOK_CHILD_GP);
    assert!(scene.agents.contains_key(&child), "hook registered it");
    let count_before = scene.agents.len();

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id: HOOK_CHILD_GP.into(),
            cwd: PathBuf::from("/home/user/demo-project"),
            parent_id: Some(hook_parent_id()),
        },
        now,
        Transport::Jsonl,
    );
    assert_eq!(
        scene.agents.len(),
        count_before,
        "the JSONL SessionStart must coalesce (no twin sprite)"
    );
    assert_eq!(
        scene.agents.get(&child).unwrap().parent_id,
        Some(hook_parent_id()),
        "parent link survives the duplicate"
    );
}

/// The hermetic tier's recorded background delegation (`fixtures/subagent-hermetic`):
/// the child's own Read lands on the child, never the root, and the child is
/// the root's (#1701).
#[test]
fn a_recorded_background_childs_tools_land_on_the_child() {
    let lines = super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("claude/fixtures/subagent-hermetic/hook-payloads.jsonl"),
    );
    let d = pixtuoid_core::harness::Drive::hooks().lines(lines);
    d.assert_clean("claude-code delegation hooks");
    let (child, root) = d
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: Some(p),
                ..
            } => Some((*agent_id, *p)),
            _ => None,
        })
        .expect("the run spawns a child");
    let tools_on = |id: AgentId| {
        d.events
            .iter()
            .filter(|e| {
                matches!(e, AgentEvent::ToolUseStart { agent_id, detail, .. }
                if *agent_id == id && !detail.as_ref().is_some_and(|t| t.is_task()))
            })
            .count()
    };
    assert_eq!(
        (tools_on(root), tools_on(child)),
        (0, 1),
        "root's tools, then the child's"
    );
    assert!(
        d.events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionEnd { agent_id, .. } if *agent_id == root)),
        "the root's SessionStart and SessionEnd keep the session's own id"
    );
}
