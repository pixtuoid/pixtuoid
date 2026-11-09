//! Claude Code subagent lifecycle (parallels the sibling `codex` module), its
//! wire the hermetic tier's recording, `claude_code/fixtures/subagent-hermetic/`.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::{Drive, Driven, Session};
use pixtuoid_core::source::AgentEvent;
use serde_json::json;

// The filename stem "parent" IS the session UUID the hook carries, so hook and
// JSONL coalesce on it.
const PARENT_PATH: &str = "/proj/parent.jsonl";
// `detect_parent_id` keys on the `<parent-uuid>` path component — i.e. exactly
// the parent's own AgentId — while the subagent's id is its own stem.
const SUB_PATH: &str = "/proj/parent/subagents/agent-1.jsonl";

/// The watcher's first sight of a transcript, before any line of it decodes.
const FIRST_SIGHT: [&str; 0] = [];

fn t0() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
}

fn parent_id() -> AgentId {
    AgentId::from_parts("claude-code", "parent")
}
fn sub_id() -> AgentId {
    AgentId::from_parts("claude-code", "agent-1")
}

#[test]
fn cc_subagent_links_renames_and_cascades_on_parent_exit() {
    let (hooks, mut session) = (Drive::hooks(), Session::default());
    session.feed(
        &hooks,
        [
            json!({
                "hook_event_name": "SessionStart",
                "session_id": "parent",
                "transcript_path": PARENT_PATH,
                "cwd": "/home/user/demo-project"
            }),
            // Real CC names the dispatch tool "Agent", not "Task" — Task-detection
            // must still fire so the reducer records a task in flight.
            json!({
                "hook_event_name": "PreToolUse",
                "session_id": "parent",
                "transcript_path": PARENT_PATH,
                "tool_name": "Agent",
                "tool_input": {"description": "explore", "subagent_type": "general-purpose"},
                "tool_use_id": "task-1"
            }),
        ]
        .map(|v| v.to_string()),
        t0(),
    );
    assert!(
        session.driven().scene.agents.contains_key(&parent_id()),
        "parent created"
    );

    // The subagent's own transcript appears: the watcher's first sight derives
    // its parent from the `/subagents/` path.
    let sub = Drive::transcript("claude-code", SUB_PATH)
        .expect("claude-code has a line decoder")
        .seeded();
    let read = json!({
        "type": "assistant",
        "attributionAgent": "general-purpose",
        "message": {"content": [
            {"type": "tool_use", "id": "s1", "name": "Read", "input": {"file_path": "/x"}}
        ]}
    });
    session.feed(&sub, [read.to_string()], t0());
    let linked = &session.driven().scene.agents[&sub_id()];
    assert_eq!(
        linked.parent_id,
        Some(parent_id()),
        "subagent linked to its parent via the /subagents/ path"
    );
    assert_eq!(
        &*linked.label, "general-purpose",
        "attributionAgent renames the subagent sprite"
    );

    session.feed(
        &hooks,
        [json!({
            "hook_event_name": "SessionEnd",
            "session_id": "parent",
            "transcript_path": PARENT_PATH
        })
        .to_string()],
        t0(),
    );
    let d = session.finish();
    d.assert_clean("claude-code subagent lifecycle");
    assert!(
        d.scene.agents[&parent_id()].exiting_at.is_some(),
        "parent exiting"
    );
    assert!(
        d.scene.agents[&sub_id()].exiting_at.is_some(),
        "CC subagent cascades out with its parent"
    );
}

/// The recording's hook payloads, which every re-record re-keys, so each id
/// below is read off them.
const RECORDED_HOOKS: &str = "claude_code/fixtures/subagent-hermetic/hook-payloads.jsonl";

/// The recording's `events` payloads, in file order.
fn recorded_lines(events: &[&str]) -> Vec<String> {
    super::captures::fixture_lines(&super::captures::sources_root().join(RECORDED_HOOKS))
        .into_iter()
        .filter(|l| {
            serde_json::from_str::<serde_json::Value>(l).expect("valid hook json")
                ["hook_event_name"]
                .as_str()
                .is_some_and(|e| events.contains(&e))
        })
        .collect()
}

/// The recorded root: its own SessionStart names no parent.
fn root_of(d: &Driven) -> AgentId {
    d.events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: None,
                ..
            } => Some(*agent_id),
            _ => None,
        })
        .expect("the recording starts a session")
}

/// The child a linked SessionStart registers, and the parent it links.
fn child_of(d: &Driven) -> (AgentId, AgentId) {
    d.events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: Some(parent),
                ..
            } => Some((*agent_id, *parent)),
            _ => None,
        })
        .expect("SubagentStart registers a linked child")
}

/// The child the recording's SubagentStart registers, as its hooks key it.
fn hook_child() -> AgentId {
    child_of(&Drive::hooks().lines(recorded_lines(&["SubagentStart"]))).0
}

/// The recording's child transcript, and the logical path the watcher finds
/// it at: nested under its root's session.
fn child_transcript() -> (PathBuf, String) {
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
    let logical = format!("/proj/{}/subagents/{}.jsonl", stem(root), stem(child));
    (child.to_path_buf(), logical)
}

fn child_drive() -> Drive {
    Drive::transcript("claude-code", &child_transcript().1)
        .expect("claude-code has a line decoder")
        .seeded()
}

#[test]
fn cc_subagent_hook_pair_registers_links_and_exits_its_child() {
    let d = Drive::hooks().lines(recorded_lines(&[
        "SessionStart",
        "SubagentStart",
        "SubagentStop",
    ]));
    d.assert_clean("claude-code subagent hook pair");
    let root = root_of(&d);
    let (child, linked) = child_of(&d);
    assert_eq!(linked, root, "SubagentStart links the recorded root");

    let slot = d
        .scene
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
        d.scene
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
    let child = hook_child();
    let (hooks, mut session) = (Drive::hooks(), Session::default());
    session.feed(&hooks, recorded_lines(&["SessionStart"]), t0());
    session.feed(&child_drive(), FIRST_SIGHT, t0());
    assert!(
        session.driven().scene.agents.contains_key(&child),
        "JSONL registered it"
    );

    session.feed(
        &hooks,
        recorded_lines(&["SubagentStart", "SubagentStop"]),
        t0(),
    );
    let d = session.finish();
    d.assert_clean("claude-code JSONL-registered subagent");
    assert!(
        d.scene
            .agents
            .get(&child)
            .expect("the hook events must coalesce onto the JSONL slot, not mint a twin")
            .exiting_at
            .is_some(),
        "hook SubagentStop must exit the JSONL-registered subagent slot"
    );
    let bare = recorded_lines(&["SubagentStart"])
        .iter()
        .find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("valid hook json");
            v["agent_id"].as_str().map(str::to_owned)
        })
        .expect("SubagentStart names its agent");
    assert!(
        !d.scene
            .agents
            .contains_key(&AgentId::from_parts("claude-code", &bare)),
        "no bare-keyed phantom twin"
    );
}

#[test]
fn cc_reordered_subagent_stop_before_start_does_not_mint_a_phantom() {
    let start = recorded_lines(&["SubagentStart"]);
    let mut lines = recorded_lines(&["SessionStart"]);
    lines.extend(recorded_lines(&["SubagentStop"]));
    // Applied TWICE: the gate must not consume the tombstone.
    lines.extend(start.iter().chain(&start).cloned());
    let d = Drive::hooks().lines(&lines);
    d.assert_clean("claude-code reordered subagent hooks");

    assert!(
        !d.scene.agents.contains_key(&child_of(&d).0),
        "a SubagentStart reordered after its own Stop must not register"
    );
    let parent = d.scene.agents.get(&root_of(&d)).expect("parent untouched");
    assert!(
        parent.exiting_at.is_none(),
        "the child's tombstone must not affect the parent"
    );
}

#[test]
fn cc_hook_first_subagent_coalesces_with_later_jsonl_session_start() {
    let (hooks, mut session) = (Drive::hooks(), Session::default());
    session.feed(
        &hooks,
        recorded_lines(&["SessionStart", "SubagentStart"]),
        t0(),
    );
    let (child, root) = child_of(session.driven());
    assert!(
        session.driven().scene.agents.contains_key(&child),
        "hook registered it"
    );
    let count_before = session.driven().scene.agents.len();

    session.feed(&child_drive(), FIRST_SIGHT, t0());
    let d = session.finish();
    d.assert_clean("claude-code hook-first subagent");
    assert_eq!(
        d.scene.agents.len(),
        count_before,
        "the JSONL SessionStart must coalesce (no twin sprite)"
    );
    assert_eq!(
        d.scene.agents[&child].parent_id,
        Some(root),
        "parent link survives the duplicate"
    );
}

/// The child's own transcript names its agent type, which relabels its sprite;
/// the root's never carries it.
#[test]
fn a_recorded_childs_transcript_names_its_agent() {
    let (child, _) = child_transcript();
    let lines = super::captures::fixture_lines(&child);
    let named = lines
        .iter()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v["attributionAgent"].as_str().map(str::to_owned))
        .expect("the child names its agent");
    let d = child_drive().lines(lines);
    d.assert_clean("claude-code child transcript");
    let stem = child
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let slot = d
        .scene
        .agents
        .get(&AgentId::from_parts("claude-code", &stem))
        .expect("the child registers");
    assert_eq!(&*slot.label, named);
}

/// The background child's own Read lands on the child, never the root, and the
/// child is the root's (#1701).
#[test]
fn a_recorded_background_childs_tools_land_on_the_child() {
    let lines =
        super::captures::fixture_lines(&super::captures::sources_root().join(RECORDED_HOOKS));
    let d = Drive::hooks().lines(lines);
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

/// One message writes a line per content block, each with the whole message's
/// usage: in this recording, the thinking and the tool call of one reply.
#[test]
fn the_meter_is_the_sum_per_message_of_a_recorded_run() {
    let dir = super::captures::sources_root().join("claude_code/fixtures/tool-run-hermetic");
    let transcript = pixtuoid_core::harness::transcript_in("claude-code", &dir)
        .unwrap_or_else(|found| panic!("one recorded transcript, found {found:?}"));
    let body = std::fs::read_to_string(&transcript).expect("a committed recording");

    let mut per_line = 0;
    let mut per_message = std::collections::BTreeMap::new();
    for line in body.lines() {
        let v: serde_json::Value = serde_json::from_str(line).expect("recorded JSON");
        if v["type"] != "assistant" {
            continue;
        }
        let fresh: u64 = [
            "input_tokens",
            "cache_creation_input_tokens",
            "output_tokens",
        ]
        .iter()
        .filter_map(|field| v["message"]["usage"][field].as_u64())
        .sum();
        let id = v["message"]["id"].as_str().expect("a recorded message id");
        assert!(!id.is_empty(), "the strip keeps what a decoder reads");
        per_line += fresh;
        per_message.insert(id.to_owned(), fresh);
    }
    let per_message: u64 = per_message.values().sum();
    assert!(
        per_message < per_line,
        "no message spans two lines here, so this recording proves nothing"
    );

    let driven = Drive::transcript_at("claude-code", &transcript)
        .expect("a transcript source")
        .seeded()
        .body(body.as_bytes());
    driven.assert_clean("claude-code tool run");
    let meter: u64 = driven.scene.agents.values().map(|a| a.tokens_used).sum();
    assert_eq!(meter, per_message);
}
