// Each test drives JsonlWatcher::run over a pre-populated projects-root — the
// attach moment — and asserts the emitted live set.

use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::mpsc;

use pixtuoid_core::AgentId;
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::source::Transport;

use crate::{
    TRAIL, backdate, cc_session_start_line, cc_subagent_line, cc_tool_use_line, cc_watcher,
    codex_meta_line, codex_watcher, codex_work_line, collect_for, collect_until,
    copilot_start_line, copilot_tool_line, spawn_copilot, start_id, vouch_snapshot, write_lines,
};

#[tokio::test]
async fn attach_matrix_registers_exactly_the_live_set() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let live_a = "aa000000-0000-7000-8000-00000000000a";
    let idle_b = "bb000000-0000-7000-8000-00000000000b";
    let dead_c = "cc000000-0000-7000-8000-00000000000c";

    write_lines(
        &root.join(format!("{live_a}.jsonl")),
        &[
            codex_meta_line(live_a, "/Users/me/proj-a"),
            codex_work_line(),
        ],
    )
    .await;
    for (uuid, cwd) in [(idle_b, "/Users/me/proj-b"), (dead_c, "/Users/me/proj-c")] {
        let path = root.join(format!("{uuid}.jsonl"));
        write_lines(&path, &[codex_meta_line(uuid, cwd)]).await;
        backdate(&path, 7200);
    }

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = codex_watcher(root.clone())
        .with_initial_window(Duration::from_secs(60))
        .with_liveness_probe(std::sync::Arc::new(move || vouch_snapshot(&[idle_b])));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let id_a = AgentId::from_parts("codex", live_a);
    let id_b = AgentId::from_parts("codex", idle_b);

    let (mut seen_a, mut seen_b) = (false, false);
    let mut events = collect_until(&mut rx, "the live set's starts", |e| {
        seen_a |= start_id(e) == Some(id_a);
        seen_b |= start_id(e) == Some(id_b);
        seen_a && seen_b
    })
    .await;
    // The seed pass may still queue c's (or a repeat) start behind them.
    events.extend(collect_for(&mut rx, TRAIL, "past the live set's starts").await);
    let mut starts: std::collections::HashMap<AgentId, usize> = Default::default();
    for id in events.iter().filter_map(|(_, e)| start_id(e)) {
        *starts.entry(id).or_default() += 1;
    }

    let expected: std::collections::HashMap<AgentId, usize> =
        [(id_a, 1), (id_b, 1)].into_iter().collect();
    assert_eq!(
        starts, expected,
        "attach must register EXACTLY the live set, once each — a (recent), b (probe-live); c (stale dead) stays hidden"
    );
    handle.abort();
}

/// The parent transcript is STALE with a pending Agent dispatch at its tail
/// (a tool_use with no tool_result) and its UUID in the probe's live set; the
/// subagent transcript is fresh.
#[tokio::test]
async fn delegating_parent_attach_registers_parent_and_links_subagent() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let proj = projects_root.join("-Users-me-deleg");
    let parent_uuid = "ee000000-0000-7000-8000-00000000000e";
    let sub_dir = proj.join(parent_uuid).join("subagents");
    tokio::fs::create_dir_all(&sub_dir).await.unwrap();

    let parent_path = proj.join(format!("{parent_uuid}.jsonl"));
    write_lines(
        &parent_path,
        &[
            cc_session_start_line(parent_uuid, "/Users/me/deleg"),
            cc_tool_use_line(
                parent_uuid,
                "/Users/me/deleg",
                "tu_task",
                "Agent",
                serde_json::json!({
                    "description": "explore", "subagent_type": "code-explorer", "prompt": "go"
                }),
            ),
        ],
    )
    .await;
    backdate(&parent_path, 7200);
    write_lines(
        &sub_dir.join("agent-x1.jsonl"),
        &[cc_subagent_line("agent-x1", "/Users/me/deleg", "tu_x")],
    )
    .await;

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = cc_watcher(projects_root.clone())
        .with_initial_window(Duration::from_secs(60))
        .with_liveness_probe(std::sync::Arc::new(move || vouch_snapshot(&[parent_uuid])));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let expected_parent = AgentId::from_parts("claude-code", parent_uuid);
    let mut parent_started = false;
    let mut sub_parent_id: Option<AgentId> = None;
    let mut dispatch_is_task: Option<bool> = None;
    collect_until(
        &mut rx,
        "the stale probe-live delegating parent, its subagent and its dispatch at attach",
        |e| {
            match e {
                AgentEvent::SessionStart {
                    agent_id,
                    parent_id,
                    ..
                } => match parent_id {
                    Some(pid) => sub_parent_id = Some(*pid),
                    None => parent_started |= *agent_id == expected_parent,
                },
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id,
                    detail,
                    ..
                } if *agent_id == expected_parent && tool_use_id.as_deref() == Some("tu_task") => {
                    dispatch_is_task = Some(detail.as_ref().is_some_and(|d| d.is_task()));
                }
                _ => {}
            }
            parent_started && sub_parent_id.is_some() && dispatch_is_task.is_some()
        },
    )
    .await;
    assert_eq!(
        sub_parent_id,
        Some(expected_parent),
        "the subagent must attach linked to the parent, not as an orphan"
    );
    assert_eq!(
        dispatch_is_task,
        Some(true),
        "the replayed pending Agent dispatch must decode as a Task ToolUseStart"
    );
    handle.abort();
}

/// The session has > MAX_PENDING_BYTES pending at attach, so the backlog is
/// skipped to EOF instead of replayed and the in-flight Task dispatch must be
/// recovered by the TASK_SCAN_BYTES tail scan (#222), while the session still
/// registers via the bounded head read (#204).
#[tokio::test]
async fn oversized_delegating_parent_attach_replays_pending_dispatch() {
    let dir = TempDir::new().unwrap();
    let sid = "ab000000-0000-7000-8000-0000000000ab";
    let session_dir = dir.path().join(sid);
    tokio::fs::create_dir_all(&session_dir).await.unwrap();

    let mut contents = format!("{}\n", copilot_start_line(sid, "/Users/me/bigdeleg"));
    let backlog_line = format!("{}\n", copilot_tool_line("tu_backlog", "bash"));
    while contents.len() <= (1usize << 20) + 4096 {
        contents.push_str(&backlog_line);
    }
    contents.push_str(&format!("{}\n", copilot_tool_line("tu_task", "task")));
    tokio::fs::write(session_dir.join("events.jsonl"), contents.as_bytes())
        .await
        .unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(256);
    let handle = spawn_copilot(dir.path().to_path_buf(), tx);

    let expected_parent = AgentId::from_parts("copilot", sid);
    let is_parent_start = |e: &AgentEvent| {
        matches!(e, AgentEvent::SessionStart { agent_id, parent_id: None, .. }
            if *agent_id == expected_parent)
    };
    let is_task_start = |e: &AgentEvent| {
        matches!(e, AgentEvent::ToolUseStart { agent_id, tool_use_id, detail,
 .. }
            if *agent_id == expected_parent
                && tool_use_id.as_deref() == Some("tu_task")
                && detail.as_ref().is_some_and(|d| d.is_task()))
    };

    let (mut started, mut dispatched) = (false, false);
    let mut events = collect_until(
        &mut rx,
        "the oversized delegating session's start at attach (#204 head read) and dispatch",
        |e| {
            started |= is_parent_start(e);
            dispatched |= is_task_start(e);
            started && dispatched
        },
    )
    .await;
    // The same pass may still queue a repeat dispatch behind them.
    events.extend(collect_for(&mut rx, TRAIL, "past the dispatch").await);

    let task_starts: Vec<Transport> = events
        .iter()
        .filter(|(_, e)| is_task_start(e))
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(
        task_starts,
        vec![Transport::Jsonl],
        "the in-flight dispatch must be re-emitted exactly once, Jsonl-tagged (it passes the hook-wins dedup at mid-attach)"
    );
    let backlog_replays = events
        .iter()
        .filter(|(_, e)| {
            matches!(e, AgentEvent::ToolUseStart { tool_use_id, .. }
                if tool_use_id.as_deref() == Some("tu_backlog"))
        })
        .count();
    assert_eq!(
        backlog_replays, 0,
        "the completed backlog must not be replayed — this is a Task-seeding scan, not a replay"
    );
    handle.abort();
}

/// A worktree cwd-split puts the parent transcript and the subagent
/// transcript under DIFFERENT project dirs (#203).
#[tokio::test]
async fn cwd_split_attach_links_subagent_to_probe_live_stale_parent() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let parent_uuid = "ff000000-0000-7000-8000-00000000000f";

    let proj_a = projects_root.join("-Users-me-wt-main");
    tokio::fs::create_dir_all(&proj_a).await.unwrap();
    let parent_path = proj_a.join(format!("{parent_uuid}.jsonl"));
    write_lines(
        &parent_path,
        &[cc_session_start_line(parent_uuid, "/Users/me/wt-main")],
    )
    .await;
    backdate(&parent_path, 7200);

    let sub_dir = projects_root
        .join("-Users-me-wt-feature")
        .join(parent_uuid)
        .join("subagents");
    tokio::fs::create_dir_all(&sub_dir).await.unwrap();
    write_lines(
        &sub_dir.join("agent-w1.jsonl"),
        &[cc_subagent_line("agent-w1", "/Users/me/wt-feature", "tu_w")],
    )
    .await;

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = cc_watcher(projects_root.clone())
        .with_initial_window(Duration::from_secs(60))
        .with_liveness_probe(std::sync::Arc::new(move || vouch_snapshot(&[parent_uuid])));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let mut parent_agent_id: Option<AgentId> = None;
    let mut sub_parent_id: Option<AgentId> = None;
    collect_until(
        &mut rx,
        "the stale probe-live parent and the fresh subagent at attach",
        |e| {
            if let AgentEvent::SessionStart {
                agent_id,
                parent_id,
                ..
            } = e
            {
                match parent_id {
                    Some(pid) => sub_parent_id = Some(*pid),
                    None => parent_agent_id = Some(*agent_id),
                }
            }
            parent_agent_id.is_some() && sub_parent_id.is_some()
        },
    )
    .await;
    let parent_agent_id = parent_agent_id.expect("collect_until saw it");
    let sub_parent_id = sub_parent_id.expect("collect_until saw it");
    assert_eq!(
        parent_agent_id,
        AgentId::from_parts("claude-code", parent_uuid),
        "the parent registers on its session UUID"
    );
    assert_eq!(
        sub_parent_id, parent_agent_id,
        "the UUID join must link the subagent to the parent across project dirs (a worktree cwd-split) on the attach path"
    );
    handle.abort();
}

/// A watcher creates no directory: its root is another CLI's, whose presence
/// auto-detect keys on (`install::grok::detect_installed`). It attaches when the
/// root appears, not at the next poll, however deep the absent chain.
#[tokio::test]
async fn an_absent_root_is_never_created_and_is_watched_once_it_appears() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join(".codex").join("sessions");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = codex_watcher(root.clone())
        .with_initial_window(Duration::from_secs(60))
        .with_poll_interval(Duration::from_secs(3600));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(300)).await;
    let parent = root.parent().unwrap();
    assert!(!parent.exists(), "the watcher created {parent:?}");

    let uuid = "ee000000-0000-7000-8000-00000000000e";
    let proj = root.join("-Users-me-late");
    tokio::fs::create_dir_all(&proj).await.unwrap();
    let session = proj.join(format!("{uuid}.jsonl"));
    write_lines(&session, &[codex_meta_line(uuid, "/Users/me/late")]).await;
    // The poll is set far past `collect_until`'s bound, so only the watch can
    // attach the root in time.
    let id = AgentId::from_parts("codex", uuid);
    collect_until(
        &mut rx,
        "a session under a root that appeared mid-run",
        |e| start_id(e) == Some(id),
    )
    .await;

    // The anchor now watches the root's parent, whose own files (CC's
    // `history.jsonl` beside its projects root) are no transcripts.
    let beside = "ff000000-0000-7000-8000-00000000000f";
    write_lines(
        &parent.join("history.jsonl"),
        &[codex_meta_line(beside, "/Users/me/beside")],
    )
    .await;
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&session)
        .await
        .unwrap();
    let line = codex_work_line();
    tokio::io::AsyncWriteExt::write_all(&mut file, format!("{line}\n").as_bytes())
        .await
        .unwrap();
    let is_active =
        |e: &AgentEvent| matches!(e, AgentEvent::ToolUseStart { agent_id, .. } if *agent_id == id);
    let mut events = collect_until(&mut rx, "a write under the appeared root", is_active).await;
    events.extend(collect_for(&mut rx, TRAIL, "past the watched write").await);
    let events: Vec<AgentEvent> = events.into_iter().map(|(_, e)| e).collect();
    let beside_id = AgentId::from_parts("codex", beside);
    assert!(
        !events.iter().any(
            |e| matches!(e, AgentEvent::SessionStart { agent_id, .. } if *agent_id == beside_id)
        ),
        "a file beside the root was decoded as a session"
    );
    assert!(!handle.is_finished(), "the watcher stopped: {handle:?}");
    handle.abort();
}
