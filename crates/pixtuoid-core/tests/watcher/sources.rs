use std::time::Duration;

use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::source::Source;
use pixtuoid_core::source::Transport;
use pixtuoid_core::source::antigravity::AntigravitySource;
use pixtuoid_core::source::claude_code::ClaudeCodeSource;
use pixtuoid_core::source::codex::CodexSource;
use pixtuoid_core::source::copilot::CopilotSource;
use pixtuoid_core::source::grok::GrokSource;

use crate::{TRAIL, collect_for, collect_until, fast_watch, first_of, start_id};

#[tokio::test]
async fn codex_source_run_emits_events_from_rollout() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let sessions_root = dir.path().to_path_buf();
    let uuid = "019e7762-9ded-7e33-be41-946ecf105bf4";
    let transcript = sessions_root.join(format!("rollout-2026-05-29T22-36-52-{uuid}.jsonl"));

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let src = CodexSource {
        sessions_root,
        child_end_unclaims: None,
    };
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": uuid, "cwd": "/repo" }
    });
    let work = crate::codex_work_line();
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("{meta}\n{work}\n").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    collect_until(
        &mut rx,
        "CodexSource::run should surface ToolUseStart",
        |e| matches!(e, AgentEvent::ToolUseStart { .. }),
    )
    .await;
    handle.abort();
}

#[tokio::test]
async fn antigravity_source_run_emits_events_from_transcript() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let brain_root = dir.path().to_path_buf();
    let project_dir = brain_root.join("sess");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let transcript = project_dir.join("transcript.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let src = AntigravitySource { brain_root };
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let planner = serde_json::json!({
        "step_index": 1,
        "cwd": "/repo",
        "type": "PLANNER_RESPONSE",
        "tool_calls": [ { "name": "list_dir", "args": { "DirectoryPath": "\"/repo/src\"" } } ]
    });
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("{planner}\n").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    collect_until(
        &mut rx,
        "AntigravitySource::run should surface ToolUseStart",
        |e| matches!(e, AgentEvent::ToolUseStart { .. }),
    )
    .await;
    handle.abort();
}

#[tokio::test]
async fn claude_code_source_run_emits_session_start_from_jsonl() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().join("projects");
    let project_dir = projects_root.join("proj-cc");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let transcript = project_dir.join("ses-cc.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let src = ClaudeCodeSource {
        projects_root,
        child_end_unclaims: None,
    };
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let line = serde_json::json!({
        "type": "assistant",
        "sessionId": "ses-cc",
        "cwd": "/repo",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_1", "name": "Bash", "input": { "command": "ls" } }
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

    collect_until(
        &mut rx,
        "ClaudeCodeSource::run should surface SessionStart from the JSONL leg",
        |e| matches!(e, AgentEvent::SessionStart { .. }),
    )
    .await;
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_code_source_run_emits_usage_from_jsonl() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().join("projects");
    let project_dir = projects_root.join("proj-cu");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let transcript = project_dir.join("ses-cu.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let src = ClaudeCodeSource {
        projects_root,
        child_end_unclaims: None,
    };
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let line = serde_json::json!({
        "type": "assistant",
        "sessionId": "ses-cu",
        "cwd": "/repo",
        "message": {
            "role": "assistant",
            "content": [],
            "usage": {
                "input_tokens": 1200,
                "cache_creation_input_tokens": 50000,
                "cache_read_input_tokens": 940000,
                "output_tokens": 5300
            }
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

    let fresh_tokens = first_of(
        &mut rx,
        "ClaudeCodeSource::run should surface Usage from a usage-bearing line",
        |e| match e {
            AgentEvent::Usage { fresh_tokens, .. } => Some(*fresh_tokens),
            _ => None,
        },
    )
    .await;
    assert_eq!(fresh_tokens, 56_500, "fresh = in + cache_create + out");
    handle.abort();
}

// Layout: <sessions_root>/<sessionId>/events.jsonl — the id derives from the
// PARENT DIR, not the "events" stem.
#[tokio::test]
async fn copilot_source_run_emits_session_start_from_events_jsonl() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let sessions_root = dir.path().to_path_buf();
    let session_dir = sessions_root.join("65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3");
    tokio::fs::create_dir_all(&session_dir).await.unwrap();
    let transcript = session_dir.join("events.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let src = CopilotSource { sessions_root };
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let start = serde_json::json!({
        "type": "session.start",
        "data": {"sessionId": "65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3", "version": 1,
                 "producer": "copilot-agent", "context": {"cwd": "/repo"}},
        "id": "a", "timestamp": "2026-05-22T05:59:45.488Z", "parentId": serde_json::Value::Null
    });
    let tool = serde_json::json!({
        "type": "tool.execution_start",
        "data": {"toolCallId": "tooluse_1", "toolName": "view", "arguments": {"path": "/repo"}},
        "id": "b", "timestamp": "2026-05-22T06:00:14.298Z", "parentId": "a"
    });
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("{start}\n{tool}\n").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    let (source, session_id) = first_of(&mut rx, "a copilot SessionStart", |e| match e {
        AgentEvent::SessionStart {
            source, session_id, ..
        } => Some((source.clone(), session_id.clone())),
        _ => None,
    })
    .await;
    assert_eq!(source, "copilot");
    assert_eq!(
        session_id, "65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3",
        "CopilotSource::run should surface a copilot SessionStart from events.jsonl"
    );
    handle.abort();
}

// grok was the ONE transcript source with no `run` coverage here, which is why
// mutating its body to `Ok(())` survived the whole suite while the other five
// sources' identical mutants were caught (#828).
// Layout: <sessions_root>/<url-encoded-cwd>/<session-id>/updates.jsonl.
#[tokio::test]
async fn grok_source_run_emits_events_from_updates_jsonl() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    let sessions_root = dir.path().to_path_buf();
    let session_id = "0197fa30-1111-7000-8000-000000000001";
    let session_dir = sessions_root.join("%2Frepo").join(session_id);
    tokio::fs::create_dir_all(&session_dir).await.unwrap();
    let transcript = session_dir.join("updates.jsonl");

    use pixtuoid_core::AgentId;

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let src = GrokSource {
        sessions_root,
        child_end_unclaims: None,
    };
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let tool_call = serde_json::json!({
        "timestamp": 1_784_203_205u64,
        "method": "session/update",
        "params": {"sessionId": session_id,
                   "update": {"sessionUpdate": "tool_call",
                              "toolCallId": "c1", "title": "grep"}}
    });
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("{tool_call}\n").as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    let agent_id = first_of(
        &mut rx,
        "GrokSource::run should surface ToolUseStart from updates.jsonl",
        |e| match e {
            AgentEvent::ToolUseStart { agent_id, .. } => Some(*agent_id),
            _ => None,
        },
    )
    .await;
    // grok keys on the transcript's PARENT-DIR name, so the id the watcher
    // mints must equal the hook leg's for the same session.
    assert_eq!(agent_id, AgentId::from_parts("grok", session_id));
    handle.abort();
}

#[tokio::test]
async fn omp_source_run_watches_profile_roots_beside_the_primary() {
    use pixtuoid_core::AgentId;
    use pixtuoid_core::source::omp::OmpSource;

    fast_watch();
    let dir = TempDir::new().unwrap();
    let primary = dir.path().join("agent").join("sessions");
    let profile = dir
        .path()
        .join("profiles")
        .join("work")
        .join("agent")
        .join("sessions");
    const STEM: &str = "2026-07-09T08-00-00-000Z_0197f0aa-0000-7000-8000-000000000021";
    let cwd_dir = profile.join("-dev-proj");
    tokio::fs::create_dir_all(&primary).await.unwrap();
    tokio::fs::create_dir_all(&cwd_dir).await.unwrap();
    let transcript = cwd_dir.join(format!("{STEM}.jsonl"));

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let mut src = OmpSource::single_root(primary);
    src.profile_sessions_roots = vec![profile];
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    write_omp_header(&transcript, "0197f0aa-0000-7000-8000-000000000021").await;

    let want = AgentId::from_parts("omp", &omp_watcher_key(&transcript));
    collect_until(
        &mut rx,
        "a transcript under a PROFILE root must register like a primary one",
        |e| start_id(e) == Some(want),
    )
    .await;
    handle.abort();
}

/// An `OmpSource` over `dir`'s primary root, its profile roots every
/// `dir/profiles/<entry>/agent/sessions`, sorted; a backstop past every test
/// deadline leaves binding a new profile to the profiles-dir watch.
fn omp_profiles_source(dir: &std::path::Path) -> pixtuoid_core::source::omp::OmpSource {
    let profiles = dir.join("profiles");
    let mut src =
        pixtuoid_core::source::omp::OmpSource::single_root(dir.join("agent").join("sessions"));
    src.profiles_dir = Some(profiles.clone());
    src.rescan = std::sync::Arc::new(move || {
        let Ok(entries) = std::fs::read_dir(&profiles) else {
            return Vec::new();
        };
        let mut roots: Vec<_> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path().join("agent").join("sessions"))
            .collect();
        roots.sort();
        roots
    });
    src.rescan_interval = Duration::from_secs(3600);
    src
}

/// Writes a session under `profile`'s sessions root, creating the profile, and
/// returns the id the watcher must register it under.
async fn write_profile_session(
    dir: &std::path::Path,
    profile: &str,
    session_id: &str,
) -> pixtuoid_core::AgentId {
    let cwd_dir = dir
        .join("profiles")
        .join(profile)
        .join("agent")
        .join("sessions")
        .join("-dev-proj");
    tokio::fs::create_dir_all(&cwd_dir).await.unwrap();
    let transcript = cwd_dir.join(format!("2026-07-09T08-00-00-000Z_{session_id}.jsonl"));
    write_omp_header(&transcript, session_id).await;
    pixtuoid_core::AgentId::from_parts("omp", &omp_watcher_key(&transcript))
}

/// Once `profiles` exists, its own watch reports each new profile; its anchor
/// sees only `profiles` itself.
#[tokio::test]
async fn omp_source_rescan_hot_plugs_a_profile_created_mid_run() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    tokio::fs::create_dir_all(dir.path().join("agent").join("sessions"))
        .await
        .unwrap();
    tokio::fs::create_dir(dir.path().join("profiles"))
        .await
        .unwrap();
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let src = omp_profiles_source(dir.path());
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(300)).await;
    let late =
        write_profile_session(dir.path(), "late", "0197f0aa-0000-7000-8000-000000000022").await;
    collect_until(
        &mut rx,
        "a profile created after startup must gain a watcher without a restart",
        |e| start_id(e) == Some(late),
    )
    .await;

    // A second profile re-lists the first: the already-bound root must not be
    // disturbed. (Watcher-count dedup itself is not observable here — one
    // watcher already emits SessionStart twice, seed + decoded header.)
    let later =
        write_profile_session(dir.path(), "later", "0197f0aa-0000-7000-8000-000000000023").await;
    collect_until(&mut rx, "a second profile must gain a watcher too", |e| {
        start_id(e) == Some(later)
    })
    .await;
    let again =
        write_profile_session(dir.path(), "late", "0197f0aa-0000-7000-8000-000000000024").await;
    collect_until(
        &mut rx,
        "a re-listed root must keep registering new transcripts",
        |e| start_id(e) == Some(again),
    )
    .await;
    handle.abort();
}

/// With no `profiles` dir at boot, its anchor reports the dir appearing with its
/// first profile.
#[tokio::test]
async fn omp_source_hot_plugs_a_profiles_dir_created_mid_run() {
    fast_watch();
    let dir = TempDir::new().unwrap();
    tokio::fs::create_dir_all(dir.path().join("agent").join("sessions"))
        .await
        .unwrap();
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let src = omp_profiles_source(dir.path());
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(300)).await;
    let first =
        write_profile_session(dir.path(), "first", "0197f0aa-0000-7000-8000-000000000025").await;
    collect_until(
        &mut rx,
        "the first profile must gain a watcher without a restart",
        |e| start_id(e) == Some(first),
    )
    .await;
    handle.abort();
}

/// The expected-id seam shared by the omp cases: the SAME fold + deriver the
/// watcher uses, so a raw-case literal cannot pass on Unix and red only on
/// windows-test.
fn omp_watcher_key(p: &std::path::Path) -> String {
    use pixtuoid_core::source::omp::omp_id_from_path;
    omp_id_from_path(std::path::Path::new(
        &pixtuoid_core::id::normalize_path_key(&p.to_string_lossy()),
    ))
}

async fn write_omp_header(path: &std::path::Path, id: &str) {
    let header = serde_json::json!({
        "type": "session", "version": 3, "id": id,
        "timestamp": "2026-07-09T08:00:00.000Z", "cwd": "/dev/proj"
    });
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .unwrap();
    f.write_all(format!("{header}\n").as_bytes()).await.unwrap();
    f.flush().await.unwrap();
}

// Layout: <sessions_root>/<encoded-cwd>/<ts>_<uuid>.jsonl with a subagent child
// at <sessions_root>/<encoded-cwd>/<ts>_<uuid>/<taskId>.jsonl — the root keys on
// its stem, the child on the stem CHAIN.
#[tokio::test]
async fn omp_source_run_links_a_nested_subagent_to_its_root() {
    use pixtuoid_core::AgentId;
    use pixtuoid_core::source::omp::OmpSource;

    fast_watch();
    let dir = TempDir::new().unwrap();
    let sessions_root = dir.path().to_path_buf();
    const ROOT_STEM: &str = "2026-07-09T08-00-00-000Z_0197f0aa-0000-7000-8000-000000000001";
    let cwd_dir = sessions_root.join("-dev-proj");
    let child_dir = cwd_dir.join(ROOT_STEM);
    tokio::fs::create_dir_all(&child_dir).await.unwrap();
    let root_transcript = cwd_dir.join(format!("{ROOT_STEM}.jsonl"));
    let child_transcript = child_dir.join("Alpha.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let src = OmpSource::single_root(sessions_root);
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    for (path, id) in [
        (&root_transcript, "0197f0aa-0000-7000-8000-000000000001"),
        (&child_transcript, "0197f0cc-0000-7000-8000-000000000003"),
    ] {
        write_omp_header(path, id).await;
    }

    let root_id = AgentId::from_parts("omp", &omp_watcher_key(&root_transcript));
    let child_id = AgentId::from_parts("omp", &omp_watcher_key(&child_transcript));
    let (agent_id, source, parent) = first_of(
        &mut rx,
        "OmpSource::run should surface the nested child's parent-linked SessionStart",
        |e| match e {
            AgentEvent::SessionStart {
                agent_id,
                source,
                parent_id: Some(parent),
                ..
            } => Some((*agent_id, source.clone(), *parent)),
            _ => None,
        },
    )
    .await;
    assert_eq!(source, "omp");
    assert_eq!(agent_id, child_id, "the parented start must be the child");
    assert_eq!(parent, root_id, "child must link to the root stem's id");
    handle.abort();
}

#[tokio::test]
async fn omp_ended_transcript_is_gated_at_first_sight_and_a_live_one_seeds() {
    use pixtuoid_core::source::omp::OmpSource;

    fast_watch();
    let header = r#"{"type":"session","version":3,"id":"0197","timestamp":"2026-07-09T08:00:00.000Z","cwd":"/dev/proj"}"#;
    let exit = r#"{"type":"custom","id":"e1","parentId":null,"timestamp":"2026-07-09T08:10:00.000Z","customType":"session_exit","data":{"reason":"exit command","kind":"normal","recordedAt":"2026-07-09T08:10:00.000Z"}}"#;

    for (name, content, expect_start) in [
        ("live", format!("{header}\n"), true),
        ("ended", format!("{header}\n{exit}\n"), false),
    ] {
        let dir = TempDir::new().unwrap();
        let sessions_root = dir.path().to_path_buf();
        let cwd_dir = sessions_root.join("-dev-proj");
        tokio::fs::create_dir_all(&cwd_dir).await.unwrap();
        // Pre-existing, mtime "now" — inside the default initial window, so
        // ONLY the ended-marker gate can suppress the seed.
        std::fs::write(
            cwd_dir.join("2026-07-09T08-00-00-000Z_0197f0aa-0000-7000-8000-000000000001.jsonl"),
            &content,
        )
        .unwrap();

        let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
        let src = OmpSource::single_root(sessions_root);
        let handle = tokio::spawn(async move { Box::new(src).run(tx).await });

        let is_start = |e: &AgentEvent| matches!(e, AgentEvent::SessionStart { .. });
        let what = format!("{name}: a recent pre-existing transcript's SessionStart");
        if expect_start {
            collect_until(&mut rx, &what, is_start).await;
        } else {
            // Nothing to wait on: a first sight lands on the boot scan.
            let boot = collect_for(&mut rx, TRAIL, &what).await;
            assert!(!boot.iter().any(|(_, e)| is_start(e)), "{what}: {boot:?}");
        }
        handle.abort();
    }
}

#[tokio::test]
async fn omp_oversized_first_sight_uses_the_head_title_through_source_wiring() {
    use pixtuoid_core::source::omp::OmpSource;

    // Keeps the tool start outside the bounded pending-task tail while forcing
    // the production oversized-skip branch; the no-Activity assertion below
    // fails if this ever stops being oversized.
    const BACKLOG_PADDING_BYTES: usize = 2 * 1024 * 1024;
    const ROOT_STEM: &str = "2026-07-09T08-00-00-000Z_0197f0aa-0000-7000-8000-000000000001";

    fast_watch();
    let dir = TempDir::new().unwrap();
    let sessions_root = dir.path().to_path_buf();
    let cwd_dir = sessions_root.join("-dev-proj");
    tokio::fs::create_dir_all(&cwd_dir).await.unwrap();
    let transcript = cwd_dir.join(format!("{ROOT_STEM}.jsonl"));

    let title = serde_json::json!({
        "type": "title", "v": 1, "title": "Oversized root title",
        "source": "auto", "updatedAt": "t", "pad": ""
    });
    let header = serde_json::json!({
        "type": "session", "version": 3, "id": "0197f0aa-0000-7000-8000-000000000001",
        "timestamp": "2026-07-09T08:00:00.000Z", "cwd": "/dev/proj"
    });
    let tool = serde_json::json!({
        "type": "message", "id": "m", "parentId": null, "timestamp": "t",
        "message": {
            "role": "assistant", "timestamp": 1,
            "content": [{
                "type": "toolCall", "id": "tool-before-padding",
                "name": "bash", "arguments": {"command": "true"}
            }]
        }
    });
    let padding = serde_json::json!({
        "type": "ignored", "padding": "x".repeat(BACKLOG_PADDING_BYTES)
    });
    tokio::fs::write(
        &transcript,
        format!("{title}\n{header}\n{tool}\n{padding}\n"),
    )
    .await
    .unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let handle = tokio::spawn(async move {
        Box::new(OmpSource::single_root(sessions_root))
            .run(tx)
            .await
    });

    let mut events = collect_until(&mut rx, "the oversized root's Rename", |e| {
        matches!(e, AgentEvent::Rename { .. })
    })
    .await;
    events.extend(collect_for(&mut rx, TRAIL, "past its Rename").await);
    handle.abort();

    let rename_labels: Vec<&str> = events
        .iter()
        .filter_map(|(_, e)| match e {
            AgentEvent::Rename { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(rename_labels, vec!["om·Oversized root title"]);
    let saw_activity = events
        .iter()
        .any(|(_, e)| matches!(e, AgentEvent::ToolUseStart { .. }));
    assert!(
        !saw_activity,
        "the buried tool must not replay; otherwise this never tested the oversized path"
    );
}

#[tokio::test]
async fn codex_permission_hermetic_fixture_drives_the_reducer_through_its_turn() {
    use pixtuoid_core::state::ActivityState;
    use pixtuoid_core::{Reducer, SceneState};

    fast_watch();
    let dir = TempDir::new().unwrap();
    let sessions_root = dir.path().to_path_buf();

    // `codex_id_from_path` keys on the filename's trailing UUID, so the replay
    // name (not the fixture's own) is what identifies the session.
    let recorded =
        pixtuoid_core::harness::sources_root().join("codex/fixtures/permission-hermetic");
    let fixture = pixtuoid_core::harness::transcript_in("codex", &recorded)
        .unwrap_or_else(|found| panic!("one recorded rollout, found {found:?}"));
    let body = std::fs::read_to_string(&fixture).expect("committed permission-hermetic fixture");
    let transcript = sessions_root
        .join("rollout-2026-01-01T00-00-00-0a0a0a0a-0b0b-0c0c-0d0d-0e0e0e0e0e0e.jsonl");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let src = CodexSource {
        sessions_root,
        child_end_unclaims: None,
    };
    let handle = tokio::spawn(async move { Box::new(src).run(tx).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Written in one shot: first sight reads the whole file from the top, so the
    // events arrive in file order without a flaky per-line sleep.
    tokio::fs::write(&transcript, &body).await.unwrap();

    // The read of the finite fixture ends on the event its last line decodes to.
    let last = pixtuoid_core::harness::Drive::transcript_at("codex", &transcript)
        .expect("a transcript source")
        .lines(body.lines())
        .events
        .pop()
        .expect("the fixture decodes");
    let events = collect_until(&mut rx, "the fixture's turn", |e| *e == last).await;
    handle.abort();

    let mut scene = SceneState::uniform(8);
    let mut reducer = Reducer::new();
    let mut states: Vec<ActivityState> = Vec::new();
    for (transport, ev) in events {
        reducer.apply(&mut scene, ev, std::time::SystemTime::now(), transport);
        if let Some(slot) = scene.agents.values().next()
            && states.last() != Some(&slot.state)
        {
            states.push(slot.state.clone());
        }
    }

    let slot = scene
        .agents
        .values()
        .next()
        .expect("the rollout must register exactly one cx· agent");
    assert_eq!(scene.agents.len(), 1, "one rollout is one agent");
    assert_eq!(
        &*slot.label.text(),
        "cx·proj",
        "label derives from the session_meta cwd basename"
    );

    // The approval rides the hooks alone, so the rollout never parks the turn.
    assert!(
        matches!(states.first(), Some(ActivityState::Idle)),
        "registration lands Idle, got {states:?}"
    );
    assert!(
        states
            .iter()
            .any(|s| matches!(s, ActivityState::Running { .. })),
        "task_started must drive Running, got {states:?}"
    );
    assert!(
        !states
            .iter()
            .any(|s| matches!(s, ActivityState::RequiresAction { .. })),
        "the rollout alone must never require action, got {states:?}"
    );
    assert_eq!(
        states.last(),
        Some(&ActivityState::Idle),
        "the turn's end settles the agent"
    );
}
