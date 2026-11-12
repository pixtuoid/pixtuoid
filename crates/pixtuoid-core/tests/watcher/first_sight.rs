use std::time::{Duration, SystemTime};

use filetime::{FileTime, set_file_mtime};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use pixtuoid_core::AgentId;
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::source::Source;
use pixtuoid_core::source::Transport;
use pixtuoid_core::source::antigravity::AntigravitySource;
use pixtuoid_core::source::jsonl::JsonlWatcher;

use crate::{
    TRAIL, Watched, a_first_sight_ends, ag_step_line, cc_watcher, codex_meta_line, codex_watcher,
    codex_work_line, collect_for, collect_until, copilot_tool_line, fast_watch, first_of, recorded,
    spawn_copilot, split_before_last, stage, start_id, starts_call, vouch_snapshot, write_lines,
};

#[tokio::test]
async fn codex_watcher_registers_stale_rollout_when_probe_says_live() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let day_dir = root.join("2026").join("06").join("10");
    tokio::fs::create_dir_all(&day_dir).await.unwrap();
    let uuid = "019e7762-9ded-7e33-be41-946ecf105bf4";
    let rollout = day_dir.join(format!("rollout-2026-06-10T08-00-00-{uuid}.jsonl"));
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": uuid, "cwd": "/Users/me/dotfiles" }
    });
    tokio::fs::write(&rollout, format!("{meta}\n"))
        .await
        .unwrap();
    let backdated = FileTime::from_system_time(SystemTime::now() - Duration::from_secs(3600));
    set_file_mtime(&rollout, backdated).unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = JsonlWatcher::new(root.clone(), "codex".to_string())
        .expect("a transcript source")
        .with_initial_window(Duration::from_secs(60))
        .with_liveness_probe(std::sync::Arc::new(move || vouch_snapshot(&[uuid])));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let start_id = first_of(&mut rx, "a probe-live stale rollout", start_id).await;
    assert_eq!(
        start_id,
        AgentId::from_parts("codex", uuid),
        "a probe-live stale rollout must register on startup, UUID-keyed"
    );
    handle.abort();
}

#[tokio::test]
async fn codex_first_sight_session_start_carries_bare_uuid_session_id() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let day_dir = root.join("2026").join("06").join("10");
    tokio::fs::create_dir_all(&day_dir).await.unwrap();
    let uuid = "019e7762-9ded-7e33-be41-946ecf105bf5";
    let rollout = day_dir.join(format!("rollout-2026-06-10T08-00-00-{uuid}.jsonl"));
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": uuid, "cwd": "/Users/me/dotfiles" }
    });
    tokio::fs::write(&rollout, format!("{meta}\n"))
        .await
        .unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher =
        JsonlWatcher::new(root.clone(), "codex".to_string()).expect("a transcript source");
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let (agent_id, session_id) =
        first_of(&mut rx, "the codex watcher's SessionStart", |e| match e {
            AgentEvent::SessionStart {
                agent_id,
                session_id,
                ..
            } => Some((*agent_id, session_id.clone())),
            _ => None,
        })
        .await;
    assert_eq!(agent_id, AgentId::from_parts("codex", uuid));
    assert_eq!(
        session_id, uuid,
        "first-sight session_id must be the IdDeriver's bare UUID, not the rollout file stem"
    );
    handle.abort();
}

#[tokio::test]
async fn watcher_emits_session_start_for_recent_files_on_startup() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    let fresh = root.join("fresh.jsonl");
    let content = format!(
        "{}\n{}\n",
        codex_meta_line("fresh", "/repo"),
        codex_work_line()
    );
    std::fs::write(&fresh, content).unwrap();
    // fsync the parent dir: APFS metadata propagation can otherwise race with
    // the watcher's initial seed walk. Unix-only — Windows can't open a
    // directory as a plain file.
    #[cfg(unix)]
    std::fs::File::open(&root).unwrap().sync_all().unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = codex_watcher(root.clone()).with_initial_window(Duration::from_secs(3600));
    let fresh_path = fresh.clone();
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    // A no-op append as a fallback notification, in case the initial seed
    // missed the file under I/O contention.
    tokio::time::sleep(Duration::from_millis(500)).await;
    tokio::fs::OpenOptions::new()
        .append(true)
        .open(&fresh_path)
        .await
        .unwrap()
        .sync_all()
        .await
        .unwrap();

    let (mut got_start, mut got_activity) = (false, false);
    collect_until(&mut rx, "a fresh file's SessionStart and replay", |e| {
        got_start |= matches!(e, AgentEvent::SessionStart { .. });
        got_activity |= matches!(e, AgentEvent::ToolUseStart { .. });
        got_start && got_activity
    })
    .await;
    handle.abort();
}

#[tokio::test]
async fn first_sight_cwd_ignores_foreign_source_shapes() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("sess");
    tokio::fs::create_dir_all(&session_dir).await.unwrap();
    let transcript = session_dir.join("transcript.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let src = AntigravitySource {
        brain_root: dir.path().to_path_buf(),
    };
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Staged + renamed so first sight always reads the full content.
    let codex_shaped = codex_meta_line("ses-foreign", "/foreign/repo");
    let content = format!("{codex_shaped}\n{}\n", ag_step_line());
    let staging = session_dir.join("transcript.jsonl.partial");
    tokio::fs::write(&staging, content.as_bytes())
        .await
        .unwrap();
    tokio::fs::rename(&staging, &transcript).await.unwrap();

    let found_cwd = first_of(&mut rx, "the antigravity SessionStart", |e| match e {
        AgentEvent::SessionStart { cwd, .. } => Some(cwd.clone()),
        _ => None,
    })
    .await;
    assert_eq!(
        found_cwd,
        std::path::PathBuf::new(),
        "a foreign source's cwd shape must not label this source's session"
    );
    handle.abort();
}

#[tokio::test]
async fn codex_rollout_yields_uuid_keyed_session_start() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let uuid = "019e7762-9ded-7e33-be41-946ecf105bf4";
    let transcript = root.join(format!("rollout-2026-05-29T22-36-52-{uuid}.jsonl"));

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher =
        JsonlWatcher::new(root.clone(), "codex".to_string()).expect("a transcript source");
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": uuid, "cwd": "/Users/me/dotfiles" }
    });
    f.write_all(format!("{meta}\n").as_bytes()).await.unwrap();
    f.write_all(format!("{}\n", codex_work_line()).as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    let expected = AgentId::from_parts("codex", uuid);
    let mut saw_session_start = false;
    let mut default_label = None;
    collect_until(&mut rx, "the codex SessionStart and its label", |e| {
        match e {
            AgentEvent::SessionStart { agent_id, .. } => {
                assert_eq!(*agent_id, expected, "Codex SessionStart must be UUID-keyed");
                saw_session_start = true;
            }
            AgentEvent::Rename { label, .. } => default_label = Some(label.clone()),
            _ => {}
        }
        saw_session_start && default_label.is_some()
    })
    .await;
    assert_eq!(
        default_label.as_deref(),
        Some("cx·dotfiles"),
        "the default LabelDeriver must emit the source-prefixed cwd basename"
    );
    handle.abort();
}

#[tokio::test]
async fn watcher_skips_oversized_pending_tail() {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("big");
    tokio::fs::create_dir_all(&session_dir).await.unwrap();
    let transcript = session_dir.join("events.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let handle = spawn_copilot(dir.path().to_path_buf(), tx);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let junk = vec![b'x'; (1 << 20) + 1024];
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(&junk).await.unwrap();
    f.flush().await.unwrap();
    drop(f);

    let mut got_start = false;
    let mut got_rename = None;
    let mut first_sight = collect_until(
        &mut rx,
        "a first-sight oversized transcript must register an agent (#204)",
        |e| {
            match e {
                AgentEvent::SessionStart { cwd, .. } => {
                    got_start = true;
                    assert_eq!(
                        *cwd,
                        std::path::PathBuf::from(""),
                        "a no-newline head yields an empty-cwd SessionStart"
                    );
                }
                AgentEvent::Rename { label, .. } => got_rename = Some(label.clone()),
                _ => {}
            }
            got_start && got_rename.is_some()
        },
    )
    .await;
    first_sight.extend(collect_for(&mut rx, TRAIL, "past the oversized registration").await);
    assert_eq!(
        got_rename.as_deref(),
        Some("cp"),
        "empty-cwd Rename falls back to the bare source prefix"
    );
    let activity_before = first_sight
        .iter()
        .filter(|(_, e)| matches!(e, AgentEvent::ToolUseStart { .. }))
        .count();
    assert_eq!(
        activity_before, 0,
        "the oversized backlog must not be replayed (got {activity_before} ToolUseStart)"
    );

    let valid = copilot_tool_line("tu_after_junk", "bash");
    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("\n{valid}\n").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    collect_until(
        &mut rx,
        "the post-skip valid line must decode after the oversized tail is skipped",
        starts_call("tu_after_junk"),
    )
    .await;
    handle.abort();
}

#[tokio::test]
async fn watcher_registers_large_transcript_on_first_sight() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let transcript = root.join("big-session.jsonl");

    let mut contents = format!(
        "{}\n",
        codex_meta_line("big-session", "/Users/me/work/bigrepo")
    );
    let backlog_line = format!("{}\n", codex_work_line());
    while contents.len() <= (1usize << 20) + 4096 {
        contents.push_str(&backlog_line);
    }
    assert!(
        contents.len() > (1usize << 20),
        "test transcript must exceed MAX_PENDING_BYTES"
    );

    // Write the whole file BEFORE the watcher first sees it, so the body is one
    // oversized first-sight pending tail.
    tokio::fs::write(&transcript, contents.as_bytes())
        .await
        .unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(256);
    let watcher = codex_watcher(root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let mut start_cwd = None;
    let mut label = None;
    let mut first_sight = collect_until(
        &mut rx,
        "the large first-sight transcript's SessionStart and label",
        |e| {
            match e {
                AgentEvent::SessionStart { cwd, .. } => start_cwd = Some(cwd.clone()),
                AgentEvent::Rename { label: l, .. } => label = Some(l.clone()),
                _ => {}
            }
            start_cwd.is_some() && label.is_some()
        },
    )
    .await;
    first_sight.extend(collect_for(&mut rx, TRAIL, "past the large first sight").await);
    let activity_count = first_sight
        .iter()
        .filter(|(_, e)| matches!(e, AgentEvent::ToolUseStart { .. }))
        .count();
    let start_cwd = start_cwd.expect("collect_until saw it");
    let label = label.expect("collect_until saw it");
    assert_eq!(
        start_cwd,
        std::path::PathBuf::from("/Users/me/work/bigrepo"),
        "cwd must come from the bounded head read of the first line"
    );
    assert_eq!(
        label, "cx·bigrepo",
        "label must derive from the head-read cwd basename"
    );
    assert!(
        activity_count < 5,
        "the giant backlog must not be replayed wholesale (got {activity_count} ToolUseStart)"
    );
    handle.abort();
}

#[tokio::test]
async fn watcher_derives_parent_id_for_subagent_path() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let subagent_dir = projects_root.join("proj").join("parent").join("subagents");
    tokio::fs::create_dir_all(&subagent_dir).await.unwrap();
    let transcript = subagent_dir.join("agent-1.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let line = serde_json::json!({
        "type": "assistant",
        "sessionId": "agent-1",
        "cwd": "/repo",
        "attributionAgent": "feature-dev:code-explorer",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_1", "name": "Read", "input": { "file_path": "/x" } }
            ]
        }
    });
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    f.flush().await.unwrap();
    drop(f);

    let expected = AgentId::from_parts("claude-code", "parent");

    let found = first_of(&mut rx, "a SessionStart carrying parent_id", |e| match e {
        AgentEvent::SessionStart {
            parent_id: Some(pid),
            ..
        } => Some(*pid),
        _ => None,
    })
    .await;
    assert_eq!(
        found, expected,
        "parent_id must key on the <parent-uuid> dir component; got {found:?}"
    );
    handle.abort();
}

#[tokio::test]
async fn watcher_links_subagent_across_project_dirs() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();

    let parent_uuid = "abc123def456";
    let project_a = projects_root.join("-Users-me-PROJECT-A");
    tokio::fs::create_dir_all(&project_a).await.unwrap();
    let parent_transcript = project_a.join(format!("{parent_uuid}.jsonl"));
    let subagent_dir = projects_root
        .join("-Users-me-PROJECT-B")
        .join(parent_uuid)
        .join("subagents");
    tokio::fs::create_dir_all(&subagent_dir).await.unwrap();
    let sub_transcript = subagent_dir.join("agent-1.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = cc_watcher(projects_root.clone());
    let handle = tokio::spawn(async move { watcher.run(tx).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let parent_line = serde_json::json!({
        "type": "system",
        "subtype": "session_start",
        "sessionId": parent_uuid,
        "cwd": "/Users/me/PROJECT-A"
    });
    let mut pf = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&parent_transcript)
        .await
        .unwrap();
    pf.write_all(format!("{parent_line}\n").as_bytes())
        .await
        .unwrap();
    pf.flush().await.unwrap();
    drop(pf);

    let sub_line = serde_json::json!({
        "type": "assistant",
        "sessionId": "agent-1",
        "cwd": "/Users/me/PROJECT-B",
        "attributionAgent": "feature-dev:code-explorer",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_1", "name": "Read", "input": { "file_path": "/x" } }
            ]
        }
    });
    let mut sf = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&sub_transcript)
        .await
        .unwrap();
    sf.write_all(format!("{sub_line}\n").as_bytes())
        .await
        .unwrap();
    sf.flush().await.unwrap();
    drop(sf);

    let mut parent_agent_id: Option<AgentId> = None;
    let mut sub_parent_id: Option<AgentId> = None;
    collect_until(
        &mut rx,
        "the parent's and the subagent's SessionStarts",
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
        sub_parent_id, parent_agent_id,
        "subagent parent_id must equal the parent's agent_id across a cwd-split (different project dirs)"
    );
    handle.abort();
}

#[tokio::test]
async fn the_registry_row_filters_the_untruncated_copy_and_the_builder_overrides_it() {
    fn admit_every_jsonl(_p: &std::path::Path) -> bool {
        true
    }

    async fn first_sight_starts(root: std::path::PathBuf, admit_all: bool, want: usize) -> usize {
        let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
        let mut watcher =
            JsonlWatcher::new(root, "antigravity".to_string()).expect("a transcript source");
        if admit_all {
            watcher = watcher.with_path_filter(admit_every_jsonl);
        }
        let handle = tokio::spawn(async move { watcher.run(tx).await });

        let is_start = |e: &AgentEvent| matches!(e, AgentEvent::SessionStart { .. });
        let mut starts = 0;
        collect_until(&mut rx, "the first sights", |e| {
            starts += usize::from(is_start(e));
            starts == want
        })
        .await;
        // Both files exist before the first scan, so a start past `want` trails it.
        let trail = collect_for(&mut rx, TRAIL, "past the first sights").await;
        handle.abort();
        starts + trail.iter().filter(|(_, e)| is_start(e)).count()
    }

    fast_watch();
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("sess");
    tokio::fs::create_dir_all(&session_dir).await.unwrap();
    for name in ["transcript.jsonl", "transcript_full.jsonl"] {
        write_lines(&session_dir.join(name), &[ag_step_line()]).await;
    }

    assert_eq!(
        first_sight_starts(dir.path().to_path_buf(), false, 1).await,
        1,
        "the antigravity row's path_filter must exclude transcript_full.jsonl — only the \
         canonical transcript first-sights"
    );
    assert_eq!(
        first_sight_starts(dir.path().to_path_buf(), true, 2).await,
        2,
        "an explicit with_path_filter must OVERRIDE the row (both files first-sight)"
    );
}

/// A resumed copilot session sits idle with its pre-resume shutdown still
/// in its tail; its last word is the resume, so first sight registers it live.
#[tokio::test]
async fn a_resumed_session_is_live_at_first_sight() {
    use pixtuoid_core::Reducer;

    let (transcript, dir) = recorded("copilot", "resume-hermetic");
    let (head, _) = split_before_last("copilot", &transcript, |e| {
        matches!(
            e,
            AgentEvent::SessionEnd {
                as_child: false,
                ..
            }
        )
    });
    let (root, _) = stage(&transcript, &dir, &head);
    let (tx, rx) = mpsc::channel(64);
    let handle = spawn_copilot(root.path().to_path_buf(), tx);
    let mut fold = Watched::new(rx, Reducer::new(), SystemTime::now());
    let first_sight = a_first_sight_ends("copilot", &transcript, &head);
    let events = fold.until("copilot", first_sight).await;
    handle.abort();
    let id = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SessionStart { agent_id, .. } => Some(*agent_id),
            _ => None,
        })
        .expect("first sight declares it");
    let exiting = fold.scene.agents.get(&id).map(|s| s.exiting_at);
    assert_eq!(exiting, Some(None), "{events:?}");
}
