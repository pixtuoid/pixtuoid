//! Claude Code subagent lifecycle (parallels the sibling `codex` module), its
//! wire the hermetic tier's recording, `claude_code/fixtures/subagent-hermetic/`.

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

/// The recording's hook payloads, which every re-record re-keys, so each id
/// below is read off them.
const RECORDED_HOOKS: &str = "claude_code/fixtures/subagent-hermetic/hook-payloads.jsonl";

fn recorded_hooks() -> Vec<serde_json::Value> {
    super::captures::fixture_lines(&super::captures::sources_root().join(RECORDED_HOOKS))
        .iter()
        .map(|l| serde_json::from_str(l).expect("valid hook json"))
        .collect()
}

/// The recording's `events` payloads, decoded in file order.
fn recorded_events(events: &[&str]) -> Vec<AgentEvent> {
    recorded_hooks()
        .into_iter()
        .filter(|v| {
            v["hook_event_name"]
                .as_str()
                .is_some_and(|e| events.contains(&e))
        })
        .flat_map(|v| decode_hook_payload(v).expect("a recorded CC hook payload decodes"))
        .collect()
}

/// Its SubagentStart/Stop pair alone: hooks carry the child's whole lifecycle.
fn lifecycle_events() -> Vec<AgentEvent> {
    recorded_events(&["SubagentStart", "SubagentStop"])
}

/// The child's registration as its SubagentStart decodes it: its id, the
/// session it names, and the parent it links.
fn recorded_child() -> (AgentId, String, AgentId) {
    lifecycle_events()
        .into_iter()
        .find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                session_id,
                parent_id: Some(parent),
                ..
            } => Some((agent_id, session_id, parent)),
            _ => None,
        })
        .expect("SubagentStart registers a linked child")
}

/// Registers the recorded root through its own SessionStart, returning its id.
fn start_parent(r: &mut Reducer, scene: &mut SceneState, now: SystemTime) -> AgentId {
    let start = recorded_events(&["SessionStart"]);
    let root = start
        .first()
        .expect("the recording starts a session")
        .agent_id();
    for ev in start {
        r.apply(scene, ev, now, Transport::Hook);
    }
    root
}

#[test]
fn cc_subagent_hook_pair_registers_links_and_exits_its_child() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();
    let root = start_parent(&mut r, &mut scene, now);
    let (child, _, linked) = recorded_child();
    assert_eq!(linked, root, "SubagentStart links the recorded root");

    for ev in lifecycle_events() {
        r.apply(&mut scene, ev, now, Transport::Hook);
    }

    let slot = scene
        .agents
        .get(&child)
        .expect("SubagentStart must register the child");
    assert_eq!(slot.parent_id, Some(root), "the child links to its parent");
    assert!(
        slot.exiting_at.is_some(),
        "SubagentStop must mark the child exiting (the Workflow-fleet \
         desk-starvation fix — no b1, no stale-sweep wait)"
    );
    assert!(
        scene
            .agents
            .get(&root)
            .expect("parent still present")
            .exiting_at
            .is_none(),
        "parent must keep running after its subagent stops"
    );
}

#[test]
fn cc_jsonl_registered_subagent_exits_cleanly_on_hook_subagent_stop() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();
    let root = start_parent(&mut r, &mut scene, now);
    let (child, session_id, _) = recorded_child();
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id,
            cwd: PathBuf::from("/home/user/demo-project"),
            parent_id: Some(root),
        },
        now,
        Transport::Jsonl,
    );
    assert!(scene.agents.contains_key(&child), "JSONL registered it");

    for ev in lifecycle_events() {
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
    let bare = recorded_hooks()
        .into_iter()
        .find_map(|v| (v["hook_event_name"] == "SubagentStart").then(|| v["agent_id"].clone()))
        .and_then(|id| id.as_str().map(str::to_owned))
        .expect("SubagentStart names its agent");
    assert!(
        !scene
            .agents
            .contains_key(&AgentId::from_parts("claude-code", &bare)),
        "no bare-keyed phantom twin"
    );
}

#[test]
fn cc_reordered_subagent_stop_before_start_does_not_mint_a_phantom() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();
    let root = start_parent(&mut r, &mut scene, now);

    let (stops, starts): (Vec<_>, Vec<_>) = lifecycle_events()
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

    assert!(
        !scene.agents.contains_key(&recorded_child().0),
        "a SubagentStart reordered after its own Stop must not register"
    );
    let parent = scene.agents.get(&root).expect("parent untouched");
    assert!(
        parent.exiting_at.is_none(),
        "the child's tombstone must not affect the parent"
    );
}

#[test]
fn cc_hook_first_subagent_coalesces_with_later_jsonl_session_start() {
    let mut scene = SceneState::uniform(8);
    let mut r = Reducer::new();
    let now = SystemTime::now();
    let root = start_parent(&mut r, &mut scene, now);

    for ev in lifecycle_events() {
        if matches!(ev, AgentEvent::SessionStart { .. }) {
            r.apply(&mut scene, ev, now, Transport::Hook);
        }
    }
    let (child, session_id, _) = recorded_child();
    assert!(scene.agents.contains_key(&child), "hook registered it");
    let count_before = scene.agents.len();

    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: "claude-code".into(),
            session_id,
            cwd: PathBuf::from("/home/user/demo-project"),
            parent_id: Some(root),
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
        Some(root),
        "parent link survives the duplicate"
    );
}

/// The child's own transcript names its agent type, which relabels its sprite;
/// the root's never carries it.
#[test]
fn a_recorded_childs_transcript_names_its_agent() {
    let dir = super::captures::sources_root().join("claude_code/fixtures/subagent-hermetic");
    let transcripts = super::captures::transcripts_in(&dir);
    let stem = |t: &std::path::Path| {
        t.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let (child, root): (Vec<_>, Vec<_>) = transcripts
        .iter()
        .partition(|t| stem(t).starts_with("agent-"));
    let ([child], [root]) = (&child[..], &root[..]) else {
        panic!("a root's and a child's transcript: {transcripts:?}");
    };
    let lines = super::captures::fixture_lines(child);
    let named = lines
        .iter()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v["attributionAgent"].as_str().map(str::to_owned))
        .expect("the child names its agent");
    let d = pixtuoid_core::harness::Drive::transcript(
        "claude-code",
        &format!("/proj/{}/subagents/{}.jsonl", stem(root), stem(child)),
    )
    .expect("claude-code has a line decoder")
    .seeded()
    .lines(lines);
    d.assert_clean("claude-code child transcript");
    let slot = d
        .scene
        .agents
        .get(&AgentId::from_parts("claude-code", &stem(child)))
        .expect("the child registers");
    assert_eq!(&*slot.label, named);
}

/// The background child's own Read lands on the child, never the root, and the
/// child is the root's (#1701).
#[test]
fn a_recorded_background_childs_tools_land_on_the_child() {
    let lines =
        super::captures::fixture_lines(&super::captures::sources_root().join(RECORDED_HOOKS));
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
