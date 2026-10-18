use std::io::Write;
use std::time::Instant;

use super::health::FailureLatch;
use super::liveness::{ProbeLadder, ProbeSnapshot, emit_session_exit, revouch_gated_files};
use super::unclaim::drain_child_end_unclaims;
use super::walk::{
    TASK_SCAN_BYTES, detect_parent_id, extract_cwd, park_if_truncated_below_cursor, scan_root,
    walk_jsonl,
};
use super::*;
use crate::AgentId;
use crate::source::claude_code::{cc_activity_recency, cc_session_ended, decode_cc_line};
use crate::source::decoder::{accept_all_paths, default_id_from_path};
use crate::source::registry::cwd_extractor_for;
use crate::source::{AgentEvent, Transport};

fn snap(pairs: &[(&str, i32)]) -> ProbeSnapshot {
    ProbeSnapshot {
        pid_of: pairs
            .iter()
            .map(|(id, pid)| (id.to_string(), *pid))
            .collect(),
    }
}

#[test]
fn bind_pid_keeps_the_larger_pid_in_both_orders() {
    for pids in [[100, 200], [200, 100]] {
        let mut s = ProbeSnapshot::default();
        for pid in pids {
            s.bind_pid("sess".to_string(), pid);
        }
        assert_eq!(
            s.pid_of.get("sess"),
            Some(&200),
            "larger pid wins ({pids:?})"
        );
    }
}

#[test]
fn from_open_fd_pairs_filters_under_root_then_recognizes_and_derives() {
    let root = std::path::Path::new("/root");
    let recognize = |p: &std::path::Path| p.extension().and_then(|e| e.to_str()) == Some("jsonl");
    let pairs = [
        (7, std::path::PathBuf::from("/root/a/keep.jsonl")),
        (9, std::path::PathBuf::from("/elsewhere/keep.jsonl")),
        (11, std::path::PathBuf::from("/root/a/skip.txt")),
    ];
    let got = ProbeSnapshot::from_open_fd_pairs(root, pairs.into_iter(), recognize, stem_id);
    assert_eq!(
        got.ids().cloned().collect::<Vec<_>>(),
        vec!["keep".to_string()]
    );
    assert_eq!(
        got.pid_of.get("keep"),
        Some(&7),
        "only the under-root .jsonl vouches, bound to its pid"
    );
}

fn stem_id(p: &std::path::Path) -> String {
    p.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn from_open_fds_with_reports_an_enumeration_failure_as_none() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let got = ProbeSnapshot::from_open_fds_with(
        tmp.path(),
        &["codex"],
        |_| true,
        default_id_from_path,
        |_| None,
        |_| Vec::new(),
    );
    assert!(
        got.is_none(),
        "an enumeration failure must be None, never a healthy empty"
    );
}

#[test]
fn from_open_fds_with_reports_an_absent_root_as_a_healthy_empty() {
    let got = ProbeSnapshot::from_open_fds_with(
        std::path::Path::new("/definitely/not/here"),
        &["codex"],
        |_| true,
        default_id_from_path,
        |_| panic!("an absent root must short-circuit before enumerating"),
        |_| Vec::new(),
    )
    .expect("an absent root is a healthy observation, not a probe failure");
    assert!(
        got.is_empty(),
        "nothing is alive under a root that does not exist"
    );
}

#[test]
fn from_open_fds_with_joins_each_pid_to_the_transcripts_it_holds_open() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    let held = [(7, root.join("a.jsonl")), (9, root.join("b.jsonl"))];
    let got = ProbeSnapshot::from_open_fds_with(
        tmp.path(),
        &["codex"],
        |p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"),
        stem_id,
        |_| Some(vec![7, 9]),
        move |pid| {
            held.iter()
                .filter(|(held_pid, _)| *held_pid == pid)
                .map(|(_, path)| path.clone())
                .collect()
        },
    )
    .expect("a healthy enumeration");
    assert!(
        !got.is_empty(),
        "a probe that found live sessions is not empty"
    );
    assert_eq!(got.pid_of.get("a"), Some(&7));
    assert_eq!(
        got.pid_of.get("b"),
        Some(&9),
        "each pid keeps its own rollout"
    );
}

/// Safe to call from a unit test: no lib unit test constructs a `JsonlWatcher`,
/// so this process-wide `OnceLock` changes nothing else in this binary.
#[test]
fn force_polling_backend_for_tests_actually_sets_the_override() {
    force_polling_backend_for_tests(Duration::from_millis(25));
    assert!(
        TEST_POLL_OVERRIDE.get().is_some(),
        "the seam must actually install the polling override"
    );
}

#[test]
fn fold_confirms_an_exit_only_after_a_sustained_miss() {
    let span = Duration::from_secs(60);
    let mut ladder = ProbeLadder::new(span);
    let t0 = Instant::now();
    assert!(ladder.fold(&snap(&[("sess", 1)]), t0).exits.is_empty());
    assert!(ladder.fold(&snap(&[]), t0).exits.is_empty());
    assert!(ladder.fold(&snap(&[]), t0 + span / 2).exits.is_empty());
    assert_eq!(
        ladder.fold(&snap(&[]), t0 + span).exits,
        vec!["sess".to_string()],
        "a miss sustained past min_span confirms the exit"
    );
    assert!(ladder.fold(&snap(&[]), t0 + span * 2).exits.is_empty());
}

#[test]
fn fold_reappearance_cancels_a_pending_miss_window() {
    let span = Duration::from_secs(60);
    let mut ladder = ProbeLadder::new(span);
    let t0 = Instant::now();
    ladder.fold(&snap(&[("sess", 1)]), t0);
    ladder.fold(&snap(&[]), t0);
    ladder.fold(&snap(&[("sess", 1)]), t0 + span / 2);
    assert!(
        ladder.fold(&snap(&[]), t0 + span).exits.is_empty(),
        "a re-appearing id resets its miss window"
    );
}

#[test]
fn fold_returns_each_new_pid_once_for_the_exit_watch() {
    let mut ladder = ProbeLadder::new(Duration::from_secs(60));
    let t = Instant::now();
    let mut watched = ladder
        .fold(&snap(&[("a", 1), ("b", 1), ("c", 2)]), t)
        .newly_watched;
    watched.sort_unstable();
    assert_eq!(watched, vec![1, 2]);
    assert!(
        ladder
            .fold(&snap(&[("a", 1), ("c", 2)]), t)
            .newly_watched
            .is_empty()
    );
}

#[test]
fn fold_migrates_a_rebound_id_and_keeps_the_old_pids_sibling() {
    let mut ladder = ProbeLadder::new(Duration::from_secs(60));
    let t = Instant::now();
    ladder.fold(&snap(&[("a", 1), ("b", 1)]), t);
    let out = ladder.fold(&snap(&[("a", 2), ("b", 1)]), t);
    assert_eq!(out.newly_watched, vec![2], "only the new pid 2 registers");
    assert_eq!(ladder.pid_died(1), vec!["b".to_string()]);
    assert_eq!(ladder.pid_died(2), vec!["a".to_string()]);
}

#[test]
fn pid_died_returns_its_ids_and_disarms_the_negative_vouch() {
    let span = Duration::from_secs(60);
    let mut ladder = ProbeLadder::new(span);
    let t0 = Instant::now();
    ladder.fold(&snap(&[("sess", 2)]), t0);
    assert_eq!(ladder.pid_died(2), vec!["sess".to_string()]);
    assert!(ladder.pid_died(2).is_empty());
    assert!(ladder.fold(&snap(&[]), t0).exits.is_empty());
    assert!(ladder.fold(&snap(&[]), t0 + span * 2).exits.is_empty());
}

#[test]
fn fold_drops_a_pid_emptied_by_a_confirmed_exit_so_a_reused_pid_re_registers() {
    let span = Duration::from_secs(60);
    let mut ladder = ProbeLadder::new(span);
    let t0 = Instant::now();
    assert_eq!(
        ladder.fold(&snap(&[("sess", 5)]), t0).newly_watched,
        vec![5]
    );
    ladder.fold(&snap(&[]), t0);
    assert_eq!(
        ladder.fold(&snap(&[]), t0 + span).exits,
        vec!["sess".to_string()]
    );
    assert_eq!(
        ladder.fold(&snap(&[("other", 5)]), t0 + span).newly_watched,
        vec![5]
    );
}

#[test]
fn default_id_from_path_returns_normalized_path_key() {
    let p = Path::new("/users/me/.claude/projects/x/abc.jsonl");
    assert_eq!(
        default_id_from_path(p),
        "/users/me/.claude/projects/x/abc.jsonl"
    );
}

#[test]
fn detect_parent_id_derives_grandparent_transcript_key() {
    let parent: PathBuf = ["projects", "x", "abc123"].iter().collect();
    let p = parent.join("subagents").join("agent-1.jsonl");
    let expected = AgentId::from_parts("claude-code", "abc123");
    assert_eq!(detect_parent_id(&p, "claude-code"), Some(expected));
    assert!(is_subagent_path(&p));
}

#[test]
fn detect_parent_id_none_for_regular_and_lookalike_paths() {
    assert_eq!(
        detect_parent_id(
            Path::new("/Users/me/.claude/projects/x/ses.jsonl"),
            "claude-code"
        ),
        None
    );
    let lookalike = Path::new("/Users/me/.claude/projects/subagents-paper/ses.jsonl");
    assert_eq!(detect_parent_id(lookalike, "claude-code"), None);
    assert!(!is_subagent_path(lookalike));
    assert_eq!(
        detect_parent_id(Path::new("subagents/agent-1.jsonl"), "claude-code"),
        None
    );
}

#[test]
fn detect_parent_id_keys_on_parent_uuid_component() {
    let sub = Path::new("/Users/me/.claude/projects/-Users-me-proj/abc123/subagents/agent-1.jsonl");
    let expected = AgentId::from_parts("claude-code", "abc123");
    assert_eq!(detect_parent_id(sub, "claude-code"), Some(expected));
}

#[test]
fn detect_parent_id_survives_cwd_split() {
    let under_a = Path::new("/Users/me/.claude/projects/-PROJECT-A/abc123/subagents/agent-1.jsonl");
    let under_b = Path::new("/Users/me/.claude/projects/-PROJECT-B/abc123/subagents/agent-1.jsonl");
    let expected = AgentId::from_parts("claude-code", "abc123");
    assert_eq!(detect_parent_id(under_a, "claude-code"), Some(expected));
    assert_eq!(detect_parent_id(under_b, "claude-code"), Some(expected));
    assert_eq!(
        detect_parent_id(under_a, "claude-code"),
        detect_parent_id(under_b, "claude-code"),
        "same <parent-uuid> under different project dirs resolves to the same parent"
    );
}

#[test]
fn detect_parent_id_handles_workflow_nesting() {
    let sub =
        Path::new("/Users/me/.claude/projects/p/abc123/subagents/workflows/wf_0d/agent-y.jsonl");
    let expected = AgentId::from_parts("claude-code", "abc123");
    assert_eq!(detect_parent_id(sub, "claude-code"), Some(expected));
}

#[cfg(windows)]
#[test]
fn detect_parent_id_handles_backslash_paths() {
    let p = Path::new(r"C:\Users\Me\.claude\projects\x\abc123\subagents\agent-1.jsonl");
    let expected = AgentId::from_parts("claude-code", "abc123");
    assert_eq!(detect_parent_id(p, "claude-code"), Some(expected));
    assert!(is_subagent_path(p));
}

#[test]
fn extract_cwd_dispatches_the_scanned_sources_shape() {
    let top = br#"{"cwd":"/repo/a"}"#;
    assert_eq!(
        extract_cwd(top, cwd_extractor_for("claude-code")),
        Some(PathBuf::from("/repo/a"))
    );
    assert_eq!(
        extract_cwd(top, cwd_extractor_for("test")),
        Some(PathBuf::from("/repo/a")),
        "an unregistered harness source keeps the shared top-level default"
    );
    let nested = br#"{"type":"session_meta","payload":{"cwd":"/repo/b","id":"u"}}"#;
    assert_eq!(
        extract_cwd(nested, cwd_extractor_for("codex")),
        Some(PathBuf::from("/repo/b"))
    );
    let mixed = b"not-json\n{\"type\":\"noise\"}\n{\"cwd\":\"/repo/c\"}\n";
    assert_eq!(
        extract_cwd(mixed, cwd_extractor_for("claude-code")),
        Some(PathBuf::from("/repo/c"))
    );
}

#[test]
fn cc_head_scan_ignores_codex_shaped_payload_cwd() {
    let codex_shaped = br#"{"type":"session_meta","payload":{"cwd":"/foreign/repo","id":"u"}}"#;
    assert_eq!(
        extract_cwd(codex_shaped, cwd_extractor_for("claude-code")),
        None,
        "a foreign source's cwd shape must not extract for a CC transcript"
    );
}

fn t_decode(
    _t: &str,
    _s: &str,
    _v: serde_json::Value,
) -> crate::source::decoder::DecodeResult<Vec<AgentEvent>> {
    Ok(vec![])
}
fn t_decode_lifecycle(
    t: &str,
    s: &str,
    v: serde_json::Value,
) -> crate::source::decoder::DecodeResult<Vec<AgentEvent>> {
    if v.get("subtype").and_then(|x| x.as_str()) == Some("session_end") {
        return Ok(vec![AgentEvent::SessionEnd {
            agent_id: AgentId::from_parts(s, t),
            as_child: false,
        }]);
    }
    let agent_id = AgentId::from_parts(s, t);
    Ok(match v.get("type").and_then(|x| x.as_str()) {
        Some("tool_use") => vec![AgentEvent::ActivityStart {
            agent_id,
            tool_use_id: None,
            detail: None,
        }],
        Some("usage") => vec![AgentEvent::Usage {
            agent_id,
            fresh_tokens: 1,
        }],
        _ => vec![],
    })
}
fn t_label(_p: &Path, _s: &str, _c: &Path) -> String {
    "t".to_string()
}
fn t_ended(buf: &[u8]) -> bool {
    std::str::from_utf8(buf).is_ok_and(|s| s.contains("session_end"))
}

async fn walk_once_with(
    path: &Path,
    window: Duration,
    decode_line: LineDecoder,
    check_ended: SessionEndChecker,
    cursors: &Arc<Mutex<HashMap<PathBuf, u64>>>,
    seen: &Arc<Mutex<HashMap<PathBuf, Sight>>>,
) -> Vec<(Transport, AgentEvent)> {
    walk_once_with_recency(
        path,
        window,
        decode_line,
        check_ended,
        super::no_activity_recency,
        cursors,
        seen,
    )
    .await
}

async fn walk_once_with_recency(
    path: &Path,
    window: Duration,
    decode_line: LineDecoder,
    check_ended: SessionEndChecker,
    activity_recency: super::ActivityRecency,
    cursors: &Arc<Mutex<HashMap<PathBuf, u64>>>,
    seen: &Arc<Mutex<HashMap<PathBuf, Sight>>>,
) -> Vec<(Transport, AgentEvent)> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let decoders = SourceDecoders {
        decode_line,
        derive_label: t_label,
        check_ended,
        activity_recency,
        id_derive: super::folded::FoldedDeriver::new(default_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    };
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors,
        seen,
        tx: &tx,
        window,
        live: &live,
    };
    walk_jsonl(path, decoders, &ctx).await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    events
}

#[tokio::test]
async fn first_sight_cwd_falls_back_to_the_path_deriver_when_content_has_none() {
    fn derived_cwd(_p: &Path) -> Option<PathBuf> {
        Some(PathBuf::from("/derived/proj"))
    }
    async fn first_sight_cwd(line: &str) -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, format!("{line}\n")).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
        let source: Arc<str> = Arc::from("test");
        let decoders = SourceDecoders {
            decode_line: t_decode,
            derive_label: t_label,
            check_ended: t_ended,
            activity_recency: super::no_activity_recency,
            id_derive: super::folded::FoldedDeriver::new(default_id_from_path),
            path_filter: accept_all_paths,
            cwd_derive: derived_cwd,
            head_label: None,
        };
        let cursors = Arc::new(Mutex::new(HashMap::new()));
        let seen = Arc::new(Mutex::new(HashMap::new()));
        let live = Arc::new(Mutex::new(HashSet::new()));
        let ctx = WatchCtx {
            source: &source,
            cursors: &cursors,
            seen: &seen,
            tx: &tx,
            window: Duration::from_secs(3600),
            live: &live,
        };
        walk_jsonl(&path, decoders, &ctx).await;
        drop(tx);
        while let Ok((_, ev)) = rx.try_recv() {
            if let AgentEvent::SessionStart { cwd, .. } = ev {
                return cwd;
            }
        }
        panic!("no SessionStart emitted for {line:?}");
    }

    assert_eq!(
        first_sight_cwd(r#"{"x":1}"#).await,
        PathBuf::from("/derived/proj")
    );
    assert_eq!(
        first_sight_cwd(r#"{"cwd":"/content/proj"}"#).await,
        PathBuf::from("/content/proj")
    );
}

async fn walk_once_live(
    path: &Path,
    window: Duration,
    live_ids: &[&str],
    cursors: &Arc<Mutex<HashMap<PathBuf, u64>>>,
    seen: &Arc<Mutex<HashMap<PathBuf, Sight>>>,
) -> Vec<(Transport, AgentEvent)> {
    let live: Arc<Mutex<HashSet<String>>> =
        Arc::new(Mutex::new(live_ids.iter().map(|s| s.to_string()).collect()));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let decoders = SourceDecoders {
        decode_line: t_decode,
        derive_label: t_label,
        check_ended: t_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(crate::source::claude_code::cc_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    };
    let ctx = WatchCtx {
        source: &source,
        cursors,
        seen,
        tx: &tx,
        window,
        live: &live,
    };
    walk_jsonl(path, decoders, &ctx).await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    events
}

fn backdate_one_hour(path: &Path) {
    filetime::set_file_mtime(
        path,
        filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - Duration::from_secs(3600),
        ),
    )
    .unwrap();
}

async fn walk_once(
    path: &Path,
    window: Duration,
    check_ended: SessionEndChecker,
    cursors: &Arc<Mutex<HashMap<PathBuf, u64>>>,
    seen: &Arc<Mutex<HashMap<PathBuf, Sight>>>,
) -> Vec<(Transport, AgentEvent)> {
    walk_once_with(path, window, t_decode, check_ended, cursors, seen).await
}

/// A CC transcript whose only TURN line is old and whose tail is the metadata
/// run a DIFFERENT live session appends — so its mtime reads as live.
fn cc_metadata_touched_transcript() -> String {
    [
        r#"{"type":"assistant","cwd":"/repo/dead","timestamp":"2026-07-29T05:46:24.525Z"}"#,
        r#"{"type":"bridge-session","sessionId":"dead"}"#,
        r#"{"type":"last-prompt","sessionId":"dead"}"#,
        r#"{"type":"file-history-snapshot"}"#,
        r#"{"type":"pr-link","timestamp":"2026-08-02T05:56:43.894Z"}"#,
    ]
    .join("\n")
        + "\n"
}

#[tokio::test]
async fn a_metadata_touched_dead_cc_transcript_is_gated_though_its_mtime_is_fresh() {
    // Wide enough to hold the fixture's 2026 stamps for this suite's lifetime.
    const TEN_YEARS: Duration = Duration::from_secs(10 * 365 * 24 * 3600);
    const ONE_HOUR: Duration = Duration::from_secs(3600);

    async fn walk(window: Duration, recency: super::ActivityRecency) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dead-session.jsonl");
        tokio::fs::write(&path, cc_metadata_touched_transcript())
            .await
            .unwrap();
        let cursors = Arc::new(Mutex::new(HashMap::new()));
        let seen = Arc::new(Mutex::new(HashMap::new()));
        let events = walk_once_with_recency(
            &path,
            window,
            decode_cc_line,
            cc_session_ended,
            recency,
            &cursors,
            &seen,
        )
        .await;
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. }))
    }

    assert!(
        !walk(ONE_HOUR, cc_activity_recency).await,
        "a transcript whose newest TURN is outside the window must gate, \
         however recently something else wrote to it"
    );
    assert!(
        walk(TEN_YEARS, cc_activity_recency).await,
        "the same transcript registers once its newest turn is inside the \
         window — the gate reads the turn, not a blanket refusal"
    );
    assert!(
        walk(ONE_HOUR, super::no_activity_recency).await,
        "negative control: with no activity probe the mtime proxy admits it \
         (the pre-fix behaviour), so the first case is the new arm's doing"
    );
}

#[tokio::test]
async fn a_turnless_tail_gates_even_though_the_newest_turn_is_off_window() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("buried-turn.jsonl");
    let mut body = String::new();
    body.push_str(
        r#"{"type":"assistant","cwd":"/repo/dead","timestamp":"2026-07-29T05:46:24.525Z"}"#,
    );
    body.push('\n');
    // An oversized sidecar line pushes the turn above out of any tail window.
    body.push_str(&format!(
        r#"{{"type":"file-history-snapshot","blob":"{}"}}"#,
        "x".repeat(super::walk::TAIL_BYTES as usize)
    ));
    body.push('\n');
    for line in [
        r#"{"type":"custom-title","sessionId":"dead"}"#,
        r#"{"type":"mode","sessionId":"dead"}"#,
        r#"{"type":"pr-link","timestamp":"2026-08-02T05:56:43.894Z"}"#,
    ] {
        body.push_str(line);
        body.push('\n');
    }
    tokio::fs::write(&path, &body).await.unwrap();

    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let events = walk_once_with_recency(
        &path,
        Duration::from_secs(3600),
        decode_cc_line,
        cc_session_ended,
        cc_activity_recency,
        &cursors,
        &seen,
    )
    .await;
    assert!(
        !events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "a tail of nothing but sidecar lines is EVIDENCE the recent write was \
         metadata, not an absence of evidence"
    );
}

#[tokio::test]
async fn a_metadata_append_does_not_revive_a_gated_transcript_but_a_turn_does() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dead-then-touched.jsonl");
    let walk = revive_walker(path.clone());
    tokio::fs::write(&path, cc_metadata_touched_transcript())
        .await
        .unwrap();

    assert!(!walk(None).await, "first sight of the dead file must gate");
    assert!(
        !walk(Some(
            "{\"type\":\"pr-link\",\"timestamp\":\"2026-08-02T05:56:43.894Z\"}\n"
        ))
        .await,
        "a metadata-only append must not revive the corpse — this is the ordering \
         a running pixtuoid meets, and the first-sight gate never sees it"
    );
    assert!(
        !walk(Some(
            "{\"type\":\"system-v2\",\"timestamp\":\"2026-08-02T06:01:11.000Z\"}\n"
        ))
        .await,
        "an UNNAMED payload-less type reads as sidecar on the revive path too, so a \
         rename of `system`/`attachment`/`file-history-delta`/`queue-operation` \
         delays the return until the next payload-carrying line — the accepted \
         residual documented on `cc_activity_recency`"
    );
    assert!(
        walk(Some(
            "{\"type\":\"assistant\",\"timestamp\":\"2026-08-02T06:05:26.613Z\"}\n"
        ))
        .await,
        "a real turn still revives it — the documented revive-on-append must survive"
    );
}

/// The other half of the residual: an unnamed type that DOES carry the turn
/// payload revives immediately, which is what stops a renamed turn type gating
/// every live session at once. Its own file — a revival is once-per-transcript.
#[tokio::test]
async fn an_unnamed_type_carrying_the_turn_payload_revives_a_gated_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dead-then-renamed-turn.jsonl");
    let walk = revive_walker(path.clone());
    tokio::fs::write(&path, cc_metadata_touched_transcript())
        .await
        .unwrap();

    assert!(!walk(None).await, "first sight of the dead file must gate");
    assert!(
        walk(Some(
            "{\"type\":\"turn-v2\",\"timestamp\":\"2026-08-02T06:03:00.000Z\",\
             \"message\":{\"role\":\"assistant\",\"content\":[]}}\n"
        ))
        .await,
        "a renamed TURN type carries `message.role`+`content`, so it must revive"
    );
}

/// The gated-transcript revive harness both revive tests need: append one line,
/// walk, and report whether the walk re-emitted `SessionStart`. Separate files
/// because a revival is once-per-transcript; the closure has no such constraint.
fn revive_walker(
    path: std::path::PathBuf,
) -> impl Fn(Option<&'static str>) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool>>> {
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    move |body: Option<&'static str>| {
        let (path, cursors, seen) = (path.clone(), cursors.clone(), seen.clone());
        Box::pin(async move {
            if let Some(line) = body {
                // SYNC append: a tokio File buffers, so an un-flushed write
                // loses the race with the walk's own stat and tests nothing.
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap();
                f.write_all(line.as_bytes()).unwrap();
                f.sync_all().unwrap();
            }
            walk_once_with_recency(
                &path,
                Duration::from_secs(3600),
                decode_cc_line,
                cc_session_ended,
                cc_activity_recency,
                &cursors,
                &seen,
            )
            .await
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. }))
        })
    }
}

async fn first_sight_walk(
    path: &Path,
    window: Duration,
    check_ended: SessionEndChecker,
) -> (Vec<(Transport, AgentEvent)>, Option<u64>) {
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let events = walk_once(path, window, check_ended, &cursors, &seen).await;
    let cursor = cursors.lock().await.get(path).copied();
    (events, cursor)
}

async fn gated_fixture(
    path: &Path,
    initial: &str,
) -> (
    Arc<Mutex<HashMap<PathBuf, u64>>>,
    Arc<Mutex<HashMap<PathBuf, Sight>>>,
) {
    tokio::fs::write(path, initial).await.unwrap();
    backdate_one_hour(path);
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let gated = walk_once(path, Duration::from_secs(60), t_ended, &cursors, &seen).await;
    assert!(
        gated.is_empty(),
        "stale first sight must gate silently, got {gated:?}"
    );
    assert!(
        !seen.lock().await.contains_key(path),
        "a gated file must not claim `seen`"
    );
    (cursors, seen)
}

#[tokio::test]
async fn walk_jsonl_honors_the_path_filter() {
    fn skip_full(p: &Path) -> bool {
        p.file_name().and_then(|s| s.to_str()) != Some("transcript_full.jsonl")
    }
    let dir = tempfile::tempdir().unwrap();
    let kept = dir.path().join("transcript.jsonl");
    let dropped = dir.path().join("transcript_full.jsonl");
    tokio::fs::write(&kept, "{\"type\":\"x\"}\n").await.unwrap();
    tokio::fs::write(&dropped, "{\"type\":\"x\"}\n")
        .await
        .unwrap();

    let (tx, _rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let live = Arc::new(Mutex::new(HashSet::new()));
    let decoders = SourceDecoders {
        decode_line: t_decode,
        derive_label: t_label,
        check_ended: t_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(default_id_from_path),
        path_filter: skip_full,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    };
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(60),
        live: &live,
    };
    walk_jsonl(dir.path(), decoders, &ctx).await;

    let cursors = cursors.lock().await;
    assert!(
        cursors.contains_key(&kept),
        "the admitted transcript.jsonl must be processed"
    );
    assert!(
        !cursors.contains_key(&dropped),
        "transcript_full.jsonl must be filtered before any cursor work"
    );
}

#[tokio::test]
async fn an_oversized_append_revives_an_ended_transcript_only_on_activity() {
    // The last row's final line is longer than the activity tail's window.
    let long_tool = format!(
        "{{\"type\":\"tool_use\",\"pad\":\"{}\"}}\n",
        "x".repeat(super::walk::TAIL_BYTES as usize * 2)
    );
    for (filler, last, revives) in [
        ("{\"type\":\"usage\"}\n", "", false),
        ("{\"type\":\"tool_use\"}\n", "", true),
        ("{\"type\":\"usage\"}\n", long_tool.as_str(), true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ended-big.jsonl");
        std::fs::write(&path, "{\"type\":\"tool_use\"}\n").unwrap();
        let cursors = Arc::new(Mutex::new(HashMap::new()));
        let seen = Arc::new(Mutex::new(HashMap::new()));
        let window = Duration::from_secs(3600);
        let append = |bytes: &[u8]| {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(bytes)
                .unwrap();
        };
        walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
        append(b"{\"type\":\"system\",\"subtype\":\"session_end\"}\n");
        walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
        assert_eq!(
            seen.lock().await.get(&path),
            Some(&Sight::Ended),
            "fixture: ended"
        );

        let span = filler.repeat(70_000) + last;
        assert!(
            span.len() as u64 > super::walk::MAX_PENDING_BYTES,
            "fixture: oversized"
        );
        append(span.as_bytes());
        let events =
            walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
        assert_eq!(
            events
                .iter()
                .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
            revives,
            "an oversized {filler:?} span ending {:?} revives only on activity: {events:?}",
            last.get(..24)
        );
    }
}

#[tokio::test]
async fn gated_file_registers_on_oversized_first_append() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gated-big.jsonl");
    let initial = "{\"type\":\"assistant\",\"cwd\":\"/repo/head\"}\n";
    let (cursors, seen) = gated_fixture(&path, initial).await;

    let mut full = String::from(initial);
    full.push_str(&"{\"type\":\"assistant\"}\n".repeat(60_000));
    tokio::fs::write(&path, &full).await.unwrap();
    assert!(
        (full.len() - initial.len()) as u64 > super::walk::MAX_PENDING_BYTES,
        "the appended span must exceed MAX_PENDING_BYTES"
    );

    let events = walk_once(&path, Duration::from_secs(60), t_ended, &cursors, &seen).await;
    let expected = AgentId::from_parts("test", &default_id_from_path(&path));
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == expected
        )),
        "a gated file's oversized first append must register the agent, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(full.len() as u64),
        "cursor must advance to EOF"
    );
}

/// A stand-in for omp's line-1 title slot: names the session from the head.
fn t_head_label(v: &serde_json::Value) -> Option<String> {
    let t = v.get("title")?.as_str()?;
    (!t.is_empty()).then(|| format!("t·{t}"))
}

/// Both head fields come back from ONE pass, and a source with no head label
/// still gets its cwd. (The scan's STOP point — the cost half of this seam — is
/// pinned by `walk::stop_tests`, where the predicate is directly observable.)
#[test]
fn a_head_read_yields_cwd_and_the_optional_title_in_one_pass() {
    let plain = "{\"type\":\"session\",\"cwd\":\"/repo/head\"}\n{\"type\":\"assistant\"}\n";
    let (cwd, label) =
        super::walk::extract_head_fields(plain.as_bytes(), cwd_extractor_for("test"), None);
    assert_eq!(cwd.as_deref(), Some(Path::new("/repo/head")));
    assert_eq!(label, None, "a source with no head label must name nothing");

    let titled = "{\"type\":\"title\",\"title\":\"Ship it\"}\n{\"type\":\"session\",\"cwd\":\"/repo/head\"}\n";
    let (cwd, label) = super::walk::extract_head_fields(
        titled.as_bytes(),
        cwd_extractor_for("test"),
        Some(t_head_label),
    );
    assert_eq!(cwd.as_deref(), Some(Path::new("/repo/head")));
    assert_eq!(label.as_deref(), Some("t·Ship it"));

    // The title slot exists but is EMPTY (omp's subagents, forever): the head
    // read must fall through to the cwd-basename deriver, not name it "".
    let empty =
        "{\"type\":\"title\",\"title\":\"\"}\n{\"type\":\"session\",\"cwd\":\"/repo/head\"}\n";
    let (_, label) = super::walk::extract_head_fields(
        empty.as_bytes(),
        cwd_extractor_for("test"),
        Some(t_head_label),
    );
    assert_eq!(label, None, "an empty slot must not become an empty name");
}

async fn walk_once_with_head_label(
    path: &Path,
    cursors: &Arc<Mutex<HashMap<PathBuf, u64>>>,
    seen: &Arc<Mutex<HashMap<PathBuf, Sight>>>,
) -> Vec<(Transport, AgentEvent)> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(64);
    let source: Arc<str> = Arc::from("test");
    let decoders = SourceDecoders {
        decode_line: t_decode,
        derive_label: t_label,
        check_ended: t_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(default_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: Some(t_head_label),
    };
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors,
        seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };
    walk_jsonl(path, decoders, &ctx).await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    events
}

fn first_rename(events: &[(Transport, AgentEvent)]) -> Option<&str> {
    events.iter().find_map(|(_, e)| match e {
        AgentEvent::Rename { label, .. } => Some(label.as_str()),
        _ => None,
    })
}

/// The oversized path skips the whole backlog, so the decoder never reaches a
/// head-borne title — the bounded head read is its ONLY carrier. Without it a
/// big transcript registers under the cwd basename every concurrent session in
/// one repo shares.
#[tokio::test]
async fn oversized_first_sight_names_the_slot_from_the_head_label() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.jsonl");
    let mut full = String::from("{\"type\":\"title\",\"title\":\"Ship the parser\"}\n");
    full.push_str("{\"type\":\"assistant\",\"cwd\":\"/repo/head\"}\n");
    full.push_str(&"{\"type\":\"assistant\"}\n".repeat(60_000));
    tokio::fs::write(&path, &full).await.unwrap();
    assert!(
        full.len() as u64 > super::walk::MAX_PENDING_BYTES,
        "the span must exceed MAX_PENDING_BYTES to take the oversized path"
    );

    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let events = walk_once_with_head_label(&path, &cursors, &seen).await;
    assert_eq!(
        first_rename(&events),
        Some("t·Ship the parser"),
        "the head label must win over the cwd deriver, got {events:?}"
    );
}

/// The head label is an OVERRIDE, not a requirement: a head with none leaves
/// the cwd-basename deriver in charge.
#[tokio::test]
async fn oversized_first_sight_falls_back_to_the_deriver_without_a_head_label() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big-untitled.jsonl");
    let mut full = String::from("{\"type\":\"title\",\"title\":\"\"}\n");
    full.push_str("{\"type\":\"assistant\",\"cwd\":\"/repo/head\"}\n");
    full.push_str(&"{\"type\":\"assistant\"}\n".repeat(60_000));
    tokio::fs::write(&path, &full).await.unwrap();

    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let events = walk_once_with_head_label(&path, &cursors, &seen).await;
    assert_eq!(
        first_rename(&events),
        Some(t_label(&path, "test", Path::new("/repo/head")).as_str()),
        "an empty title must leave the deriver's label, got {events:?}"
    );
}

/// A GATED file revived by an append reads only the tail, which is likewise
/// past a head-borne title — the same head read has to carry it.
#[tokio::test]
async fn revived_gated_file_still_names_the_slot_from_the_head_label() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("revived.jsonl");
    let initial = "{\"type\":\"title\",\"title\":\"Ship the parser\"}\n";
    let (cursors, seen) = gated_fixture(&path, initial).await;

    // The append carries NO cwd, so first-sight falls back to the head read.
    let mut full = String::from(initial);
    full.push_str("{\"type\":\"assistant\"}\n");
    tokio::fs::write(&path, &full).await.unwrap();

    let events = walk_once_with_head_label(&path, &cursors, &seen).await;
    assert_eq!(
        first_rename(&events),
        Some("t·Ship the parser"),
        "a revival must recover the head title, got {events:?}"
    );
}

#[tokio::test]
async fn gated_file_oversized_ended_append_stays_unregistered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gated-big-ended.jsonl");
    let initial = "{\"type\":\"assistant\",\"cwd\":\"/repo/head\"}\n";
    let (cursors, seen) = gated_fixture(&path, initial).await;

    let mut full = String::from(initial);
    full.push_str(&"{\"type\":\"assistant\"}\n".repeat(60_000));
    full.push_str("{\"type\":\"system\",\"subtype\":\"session_end\"}\n");
    tokio::fs::write(&path, &full).await.unwrap();

    let events = walk_once(&path, Duration::from_secs(60), t_ended, &cursors, &seen).await;
    let expected = AgentId::from_parts("test", &default_id_from_path(&path));
    assert!(
        events.iter().any(
            |(_, e)| matches!(e, AgentEvent::SessionEnd { agent_id, as_child: false } if *agent_id == expected)
        ),
        "the buried terminator must still emit SessionEnd, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "an ended oversized span must not register a ghost, got {events:?}"
    );
}

#[tokio::test]
async fn gated_file_oversized_metadata_only_append_stays_unregistered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gated-big-sidecar.jsonl");
    let initial = "{\"type\":\"assistant\",\"cwd\":\"/repo/head\"}\n";
    let (cursors, seen) = gated_fixture(&path, initial).await;

    let mut full = String::from(initial);
    full.push_str(&"{\"type\":\"assistant\"}\n".repeat(60_000));
    let sidecar = "{\"type\":\"custom-title\",\"sessionId\":\"s\"}\n";
    full.push_str(&sidecar.repeat((super::walk::TAIL_BYTES as usize / sidecar.len()) + 8));
    tokio::fs::write(&path, &full).await.unwrap();
    assert!(
        (full.len() - initial.len()) as u64 > super::walk::MAX_PENDING_BYTES,
        "the appended span must exceed MAX_PENDING_BYTES"
    );

    let events = walk_once_with_recency(
        &path,
        Duration::from_secs(60),
        decode_cc_line,
        cc_session_ended,
        cc_activity_recency,
        &cursors,
        &seen,
    )
    .await;
    assert!(
        !events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "an oversized span whose tail is pure metadata must not register, got {events:?}"
    );
    assert!(
        !seen.lock().await.contains_key(&path),
        "and it must not claim `seen` either"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(full.len() as u64),
        "the cursor must still advance to EOF"
    );
}

#[tokio::test]
async fn session_end_ends_its_claim_so_later_activity_re_registers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resumed.jsonl");
    tokio::fs::write(&path, "{\"type\":\"assistant\",\"cwd\":\"/repo\"}\n")
        .await
        .unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let window = Duration::from_secs(3600);
    let events = walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "live first sight must register, got {events:?}"
    );

    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::write_all(
        &mut f,
        b"{\"type\":\"system\",\"subtype\":\"session_end\"}\n",
    )
    .await
    .unwrap();
    tokio::io::AsyncWriteExt::flush(&mut f).await.unwrap();
    drop(f);
    let events = walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionEnd { .. })),
        "the structural end must decode to SessionEnd, got {events:?}"
    );
    assert_eq!(
        seen.lock().await.get(&path),
        Some(&Sight::Ended),
        "SessionEnd must END `seen` (not remove it) so a revival can \
         re-register while the re-vouch sweep still skips the path"
    );

    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut f, b"{\"type\":\"tool_use\"}\n")
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::flush(&mut f).await.unwrap();
    drop(f);
    let events = walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
    assert!(
        matches!(
            events.as_slice(),
            [
                (_, AgentEvent::SessionStart { .. }),
                ..,
                (_, AgentEvent::ActivityStart { .. })
            ]
        ),
        "a post-end append that decodes to activity must re-register the agent \
         ahead of that activity, got {events:?}"
    );
}

#[tokio::test]
async fn a_post_end_trailer_that_decodes_to_nothing_never_re_registers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ended.jsonl");
    tokio::fs::write(&path, "{\"type\":\"tool_use\"}\n")
        .await
        .unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let window = Duration::from_secs(3600);
    let append = |line: &[u8]| {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(line)
            .unwrap();
    };
    walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
    append(b"{\"type\":\"system\",\"subtype\":\"session_end\"}\n");
    walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
    assert_eq!(
        seen.lock().await.get(&path),
        Some(&Sight::Ended),
        "fixture: ended"
    );

    // grok flushes `hook_execution{stop}` after its end line; it decodes to nothing.
    // Nor does one that decodes to usage, which proves no life either.
    append(b"{\"type\":\"system\",\"subtype\":\"hook_trailer\"}\n{\"type\":\"usage\"}\n");
    let events = walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
    assert!(
        !events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "a trailer that proves no life must not resurrect the ended session, got {events:?}"
    );
    assert_eq!(
        seen.lock().await.get(&path),
        Some(&Sight::Ended),
        "the claim stays ended, so a real revival still re-registers"
    );

    append(b"{\"type\":\"tool_use\"}\n");
    let events = walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "activity after the trailer is the revival, got {events:?}"
    );
}

#[tokio::test]
async fn session_exit_drains_pending_bytes_so_a_straggler_walk_cannot_resurrect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    std::fs::write(&path, "{\"type\":\"assistant\"}\n").unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let window = Duration::from_secs(3600);

    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. }))
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"assistant\"}\n")
        .unwrap();
    let pre_exit_cursor = *cursors.lock().await.get(&path).unwrap();
    let file_len = std::fs::metadata(&path).unwrap().len();
    assert!(pre_exit_cursor < file_len, "fixture: bytes must be pending");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let decoders = SourceDecoders {
        decode_line: t_decode,
        derive_label: t_label,
        check_ended: t_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(default_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    };
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window,
        live: &live,
    };
    let id = default_id_from_path(&path);
    emit_session_exit(&id, decoders, &ctx).await;
    drop(tx);
    let mut exit_events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        exit_events.push(ev);
    }
    assert!(
        matches!(
            exit_events.last(),
            Some((Transport::Jsonl, AgentEvent::SessionEnd { .. }))
        ),
        "the terminator must be emitted (last), got {exit_events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(file_len),
        "the exit must drain pending bytes to EOF before un-claiming"
    );
    assert!(
        !seen.lock().await.contains_key(&path),
        "seen must be un-claimed so a genuine post-death append revives"
    );

    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events.is_empty(),
        "a straggler walk after the exit must not resurrect, got {events:?}"
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"assistant\"}\n")
        .unwrap();
    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "a post-exit append must re-register, got {events:?}"
    );
}

#[tokio::test]
async fn session_exit_purges_live_so_a_probe_failure_pass_cannot_revouch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    std::fs::write(&path, "{\"type\":\"assistant\"}\n").unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let window = Duration::from_secs(3600);

    let id = default_id_from_path(&path);
    let live: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::from([id.clone()])));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(64);
    let source: Arc<str> = Arc::from("test");
    let decoders = SourceDecoders {
        decode_line: t_decode,
        derive_label: t_label,
        check_ended: t_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(default_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    };
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window,
        live: &live,
    };

    walk_jsonl(&path, decoders, &ctx).await;
    emit_session_exit(&id, decoders, &ctx).await;
    assert!(
        !live.lock().await.contains(&id),
        "the exit must purge the dead id from the admission set"
    );
    while rx.try_recv().is_ok() {}

    let mut health = FailureLatch::default();
    scan_root(dir.path(), decoders, &ctx, &mut health).await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    assert!(
        !events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "a probe-failure pass after the instant exit must not mint a phantom SessionStart, got {events:?}"
    );
}

#[test]
fn failure_latch_fires_once_per_state_change() {
    let mut latch = FailureLatch::default();
    assert!(latch.on_failure(), "first failure after a success reports");
    assert!(!latch.on_failure(), "a persistent failure stays quiet");
    assert!(latch.on_success(), "recovery reports once");
    assert!(!latch.on_success(), "steady success stays quiet");
    assert!(
        latch.on_failure(),
        "a NEW failure after recovery reports again"
    );
}

fn t_decoders() -> SourceDecoders {
    SourceDecoders {
        decode_line: t_decode,
        derive_label: t_label,
        check_ended: t_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(default_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    }
}

fn drain_events(
    rx: &mut tokio::sync::mpsc::Receiver<(Transport, AgentEvent)>,
) -> Vec<(Transport, AgentEvent)> {
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    events
}

#[tokio::test]
async fn child_end_unclaim_drains_stragglers_then_ends_the_claim_without_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("child.jsonl");
    std::fs::write(&path, "{\"type\":\"assistant\"}\n").unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let window = Duration::from_secs(3600);

    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. }))
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"assistant\"}\n")
        .unwrap();
    let file_len = std::fs::metadata(&path).unwrap().len();
    assert!(
        *cursors.lock().await.get(&path).unwrap() < file_len,
        "fixture: bytes must be pending"
    );

    let unclaims = ChildEndUnclaims::new();
    let id = AgentId::from_parts("test", &default_id_from_path(&path));
    unclaims.push(id);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(64);
    let source: Arc<str> = Arc::from("test");
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window,
        live: &live,
    };
    drain_child_end_unclaims(Some(&unclaims), t_decoders(), &ctx).await;
    let events = drain_events(&mut rx);
    assert!(
        events.is_empty(),
        "the un-claim emits NOTHING — no SessionEnd (the hook already \
         ended the slot), no straggler registration — got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(file_len),
        "stragglers must be drained to EOF BEFORE the end (#228)"
    );
    assert_eq!(
        seen.lock().await.get(&path),
        Some(&Sight::Ended),
        "the claim must be ENDED (kept known, so the re-vouch sweep \
         cannot replay it)"
    );

    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events.is_empty(),
        "a straggler walk after the end must not resurrect, got {events:?}"
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"tool_use\"}\n")
        .unwrap();
    let events = walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == id
        )),
        "the turn-N+1 append must re-register the child, got {events:?}"
    );
}

#[tokio::test]
async fn an_ended_claim_is_not_revouched_into_a_full_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("child.jsonl");
    std::fs::write(&path, "{\"type\":\"assistant\"}\n").unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let id = default_id_from_path(&path);
    let agent_id = AgentId::from_parts("test", &id);
    let file_len = std::fs::metadata(&path).unwrap().len();

    let events = walk_once(&path, Duration::from_secs(3600), t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. }))
    );
    let unclaims = ChildEndUnclaims::new();
    unclaims.push(agent_id);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(64);
    let source: Arc<str> = Arc::from("test");
    let live: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::from([id.clone()])));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };
    drain_child_end_unclaims(Some(&unclaims), t_decoders(), &ctx).await;
    let events = drain_events(&mut rx);
    assert!(events.is_empty(), "the end emits nothing, got {events:?}");
    assert_eq!(
        seen.lock().await.get(&path),
        Some(&Sight::Ended),
        "the claim must be ENDED (`Sight::Ended`), not removed — removal is \
         exactly what would expose the path to the re-vouch replay below"
    );

    let mut health = FailureLatch::default();
    scan_root(dir.path(), t_decoders(), &ctx, &mut health).await;
    let events = drain_events(&mut rx);
    assert!(
        events.is_empty(),
        "a re-vouch sweep over an ENDED claim must not replay/re-register, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(file_len),
        "the ended path's cursor must stay parked at EOF (no reset-to-0 replay)"
    );
}

#[tokio::test]
async fn a_decoded_terminators_ended_claim_is_not_revouched_into_a_full_replay() {
    fn never_ended(_tail: &[u8]) -> bool {
        false
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"assistant\"}\n{\"type\":\"system\",\"subtype\":\"session_end\"}\n",
    )
    .unwrap();
    let file_len = std::fs::metadata(&path).unwrap().len();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let id = default_id_from_path(&path);

    let events = walk_once_with(
        &path,
        Duration::from_secs(3600),
        t_decode_lifecycle,
        never_ended,
        &cursors,
        &seen,
    )
    .await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionEnd { .. })),
        "the decoded terminator must be forwarded, got {events:?}"
    );
    assert_eq!(
        seen.lock().await.get(&path),
        Some(&Sight::Ended),
        "the claim must be ENDED (`Sight::Ended`), not removed — removal is exactly \
         what exposes the path to the re-vouch replay below"
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(64);
    let source: Arc<str> = Arc::from("test");
    let live: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::from([id])));
    let decoders = SourceDecoders {
        decode_line: t_decode_lifecycle,
        derive_label: t_label,
        check_ended: never_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(default_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    };
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };
    let mut health = FailureLatch::default();
    scan_root(dir.path(), decoders, &ctx, &mut health).await;
    let events = drain_events(&mut rx);
    assert!(
        events.is_empty(),
        "a re-vouch sweep over an ENDED path must not replay it, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(file_len),
        "the ended path's cursor must stay parked at EOF (no reset-to-0 replay)"
    );
}

#[tokio::test]
async fn unclaim_for_foreign_id_stays_pending_and_leaves_local_claims_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.jsonl");
    std::fs::write(&path, "{\"type\":\"assistant\"}\n").unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    walk_once(&path, Duration::from_secs(3600), t_ended, &cursors, &seen).await;
    assert_eq!(seen.lock().await.get(&path), Some(&Sight::Claimed));

    let unclaims = ChildEndUnclaims::new();
    let foreign = AgentId::from_parts("codex", "not-claimed-here");
    unclaims.push(foreign);
    let (tx, _rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(64);
    let source: Arc<str> = Arc::from("test");
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };
    drain_child_end_unclaims(Some(&unclaims), t_decoders(), &ctx).await;
    assert_eq!(
        seen.lock().await.get(&path),
        Some(&Sight::Claimed),
        "a foreign id must not end this watcher's claims"
    );
    assert_eq!(
        unclaims.take_matching(|x| *x == foreign),
        vec![foreign],
        "the foreign id must survive the non-matching drain for its owning watcher"
    );
}

#[tokio::test]
async fn child_end_unclaims_ttl_prunes_unmatched_entries() {
    // Generous vs. the between-assert wall time: the "inside the TTL" drains
    // must land before it elapses even on a loaded machine.
    let ttl = Duration::from_millis(250);
    let unclaims = ChildEndUnclaims::with_ttl(ttl);
    let id = AgentId::from_parts("codex", "orphaned-entry");
    unclaims.push(id);
    assert!(
        unclaims.take_matching(|_| false).is_empty(),
        "a non-matching drain must not consume the entry"
    );
    assert_eq!(
        unclaims.take_matching(|x| *x == id),
        vec![id],
        "inside the TTL a later drain still finds it"
    );
    unclaims.push(id);
    tokio::time::sleep(ttl * 2).await;
    assert!(
        unclaims.take_matching(|_| true).is_empty(),
        "past the TTL the unmatched entry is pruned"
    );
}

#[tokio::test]
async fn child_end_unclaim_ends_only_the_matched_ids_own_path() {
    let dir = tempfile::tempdir().unwrap();
    let ended_child = dir.path().join("ended-child.jsonl");
    let sibling = dir.path().join("live-sibling.jsonl");
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    for p in [&ended_child, &sibling] {
        std::fs::write(p, "{\"type\":\"assistant\"}\n").unwrap();
        walk_once(p, Duration::from_secs(3600), t_ended, &cursors, &seen).await;
        assert_eq!(
            seen.lock().await.get(p),
            Some(&Sight::Claimed),
            "fixture: registered"
        );
    }

    let unclaims = ChildEndUnclaims::new();
    unclaims.push(AgentId::from_parts(
        "test",
        &default_id_from_path(&ended_child),
    ));
    let (tx, _rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(64);
    let source: Arc<str> = Arc::from("test");
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };
    drain_child_end_unclaims(Some(&unclaims), t_decoders(), &ctx).await;
    assert_eq!(
        seen.lock().await.get(&ended_child),
        Some(&Sight::Ended),
        "the ended child's claim is ended"
    );
    assert_eq!(
        seen.lock().await.get(&sibling),
        Some(&Sight::Claimed),
        "the sibling session's claim must survive another child's un-claim"
    );
}

#[tokio::test]
async fn oversized_ended_skip_ends_its_claim_so_later_activity_re_registers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big-resumed.jsonl");
    let initial = "{\"type\":\"assistant\",\"cwd\":\"/repo\"}\n";
    tokio::fs::write(&path, initial).await.unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let window = Duration::from_secs(3600);
    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "live first sight must register, got {events:?}"
    );

    let mut full = String::from(initial);
    full.push_str(&"{\"type\":\"assistant\"}\n".repeat(60_000));
    full.push_str("{\"type\":\"system\",\"subtype\":\"session_end\"}\n");
    tokio::fs::write(&path, &full).await.unwrap();
    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionEnd { .. })),
        "the buried terminator must emit SessionEnd, got {events:?}"
    );
    assert!(
        seen.lock().await.get(&path) == Some(&Sight::Ended),
        "the oversized-ended skip must end `seen`"
    );

    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut f, b"{\"type\":\"tool_use\"}\n")
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::flush(&mut f).await.unwrap();
    drop(f);
    let events = walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "post-end activity must re-register the agent, got {events:?}"
    );
}

#[tokio::test]
async fn walk_jsonl_gates_a_first_sight_ended_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ended.jsonl");
    let content = "{\"type\":\"system\",\"subtype\":\"session_start\"}\n\
                   {\"type\":\"system\",\"subtype\":\"session_end\"}\n";
    tokio::fs::write(&path, content).await.unwrap();
    let len = tokio::fs::metadata(&path).await.unwrap().len();

    let (events, cursor) = first_sight_walk(&path, Duration::from_secs(3600), t_ended).await;
    assert!(
        events.is_empty(),
        "a never-seeded ENDED file must not emit SessionStart, got {events:?}"
    );
    assert_eq!(cursor, Some(len), "ended file must be seeded at EOF");
}

#[tokio::test]
async fn walk_jsonl_gates_a_first_sight_stale_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.jsonl");
    tokio::fs::write(&path, "{\"type\":\"assistant\",\"cwd\":\"/r\"}\n")
        .await
        .unwrap();
    backdate_one_hour(&path);
    let len = tokio::fs::metadata(&path).await.unwrap().len();

    let (events, cursor) = first_sight_walk(&path, Duration::from_secs(60), t_ended).await;
    assert!(
        events.is_empty(),
        "a never-seeded STALE file must not emit SessionStart, got {events:?}"
    );
    assert_eq!(cursor, Some(len), "stale file must be seeded at EOF");
}

#[tokio::test]
async fn known_oversized_tail_emits_session_end_if_the_skipped_span_ended() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.jsonl");
    let initial = "{\"type\":\"assistant\",\"cwd\":\"/r\"}\n";
    tokio::fs::write(&path, initial).await.unwrap();
    let seeded = initial.len() as u64;

    let mut full = String::from(initial);
    full.push_str(&"{\"type\":\"assistant\"}\n".repeat(60_000));
    full.push_str("{\"type\":\"system\",\"subtype\":\"session_end\"}\n");
    tokio::fs::write(&path, &full).await.unwrap();
    let len = full.len() as u64;
    assert!(
        len - seeded > super::walk::MAX_PENDING_BYTES,
        "the appended span must exceed MAX_PENDING_BYTES"
    );

    let cursors = Arc::new(Mutex::new(HashMap::from([(path.clone(), seeded)])));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let decoders = SourceDecoders {
        decode_line: t_decode,
        derive_label: t_label,
        check_ended: t_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(default_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    };
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };
    walk_jsonl(&path, decoders, &ctx).await;
    drop(tx);

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    let expected = AgentId::from_parts("test", &default_id_from_path(&path));
    assert!(
        events.iter().any(
            |(_, e)| matches!(e, AgentEvent::SessionEnd { agent_id, as_child: false } if *agent_id == expected)
        ),
        "a buried session_end in the skipped span must still emit SessionEnd, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(len),
        "cursor must advance to EOF"
    );
}

#[tokio::test]
async fn gated_revive_falls_back_to_head_cwd_when_tail_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-gated.jsonl");
    let head = "{\"type\":\"meta\",\"cwd\":\"/repo/head\",\"id\":\"u\"}\n";
    let (cursors, seen) = gated_fixture(&path, head).await;

    let mut full = String::from(head);
    full.push_str("{\"type\":\"assistant\"}\n");
    tokio::fs::write(&path, &full).await.unwrap();

    let events = walk_once(&path, Duration::from_secs(60), t_ended, &cursors, &seen).await;
    let cwds: Vec<PathBuf> = events
        .iter()
        .filter_map(|(_, e)| match e {
            AgentEvent::SessionStart { cwd, .. } => Some(cwd.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        cwds,
        vec![PathBuf::from("/repo/head")],
        "the revive SessionStart must carry the head cwd, got {events:?}"
    );
}

#[tokio::test]
async fn gated_file_revives_on_small_append_with_tail_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gated-small.jsonl");
    let head = "{\"type\":\"assistant\",\"cwd\":\"/repo/head\"}\n";
    let (cursors, seen) = gated_fixture(&path, head).await;

    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::write_all(
        &mut f,
        b"{\"type\":\"assistant\",\"cwd\":\"/repo/tail\"}\n",
    )
    .await
    .unwrap();
    tokio::io::AsyncWriteExt::flush(&mut f).await.unwrap();
    drop(f);

    let events = walk_once(&path, Duration::from_secs(60), t_ended, &cursors, &seen).await;
    let expected = AgentId::from_parts("test", &default_id_from_path(&path));
    let starts: Vec<(AgentId, PathBuf)> = events
        .iter()
        .filter_map(|(_, e)| match e {
            AgentEvent::SessionStart { agent_id, cwd, .. } => Some((*agent_id, cwd.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        vec![(expected, PathBuf::from("/repo/tail"))],
        "the small-append revive must register exactly once, carrying the APPEND's cwd, got {events:?}"
    );
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            AgentEvent::Rename { agent_id, .. } if *agent_id == expected
        )),
        "the revive must emit the Rename half of the registration pair, got {events:?}"
    );
}

#[tokio::test]
async fn walk_jsonl_emits_for_a_first_sight_recent_live_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.jsonl");
    tokio::fs::write(&path, "{\"type\":\"assistant\",\"cwd\":\"/r\"}\n")
        .await
        .unwrap();

    let (events, _cursor) = first_sight_walk(&path, Duration::from_secs(3600), t_ended).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "a recent, not-ended file seen first must still emit SessionStart, got {events:?}"
    );
}

const LIVE_UUID: &str = "01000000-0000-7000-8000-0000000000aa";

#[tokio::test]
async fn probe_live_stale_file_registers_at_first_sight() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{LIVE_UUID}.jsonl"));
    tokio::fs::write(&path, "{\"type\":\"assistant\",\"cwd\":\"/repo\"}\n")
        .await
        .unwrap();
    backdate_one_hour(&path);
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once_live(
        &path,
        Duration::from_secs(60),
        &[LIVE_UUID],
        &cursors,
        &seen,
    )
    .await;
    let expected = AgentId::from_parts("test", LIVE_UUID);
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == expected
        )),
        "a probe-live stale transcript must register at first sight, got {events:?}"
    );
}

#[tokio::test]
async fn probe_miss_keeps_the_stale_gate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{LIVE_UUID}.jsonl"));
    tokio::fs::write(&path, "{\"type\":\"assistant\",\"cwd\":\"/repo\"}\n")
        .await
        .unwrap();
    backdate_one_hour(&path);
    let len = tokio::fs::metadata(&path).await.unwrap().len();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once_live(
        &path,
        Duration::from_secs(60),
        &["99999999-9999-7999-8999-999999999999"],
        &cursors,
        &seen,
    )
    .await;
    assert!(
        events.is_empty(),
        "a stale transcript the probe does not vouch for must stay gated, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(len),
        "gated file must be seeded at EOF"
    );
}

#[tokio::test]
async fn probe_never_gates_a_recent_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{LIVE_UUID}.jsonl"));
    tokio::fs::write(&path, "{\"type\":\"assistant\",\"cwd\":\"/repo\"}\n")
        .await
        .unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once_live(
        &path,
        Duration::from_secs(3600),
        &["99999999-9999-7999-8999-999999999999"],
        &cursors,
        &seen,
    )
    .await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "a recent file must register regardless of the probe, got {events:?}"
    );
}

#[tokio::test]
async fn probe_live_oversized_stale_file_registers_via_head_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{LIVE_UUID}.jsonl"));
    let mut full = String::from("{\"type\":\"assistant\",\"cwd\":\"/repo/head\"}\n");
    full.push_str(&"{\"type\":\"assistant\"}\n".repeat(60_000));
    assert!(full.len() as u64 > (1 << 20), "body must exceed 1 MiB");
    tokio::fs::write(&path, &full).await.unwrap();
    backdate_one_hour(&path);
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once_live(
        &path,
        Duration::from_secs(60),
        &[LIVE_UUID],
        &cursors,
        &seen,
    )
    .await;
    let cwds: Vec<PathBuf> = events
        .iter()
        .filter_map(|(_, e)| match e {
            AgentEvent::SessionStart { cwd, .. } => Some(cwd.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        cwds,
        vec![PathBuf::from("/repo/head")],
        "the oversized probe-live first sight must register with the head cwd, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(full.len() as u64),
        "backlog must be skipped to EOF, not replayed"
    );
}

#[tokio::test]
async fn scan_pass_re_vouches_a_transiently_gated_live_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{LIVE_UUID}.jsonl"));
    tokio::fs::write(&path, "{\"type\":\"assistant\",\"cwd\":\"/repo\"}\n")
        .await
        .unwrap();
    backdate_one_hour(&path);
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let live: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let decoders = SourceDecoders {
        decode_line: t_decode,
        derive_label: t_label,
        check_ended: t_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(crate::source::claude_code::cc_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    };
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(60),
        live: &live,
    };

    let mut health = FailureLatch::default();
    scan_root(dir.path(), decoders, &ctx, &mut health).await;
    assert!(rx.try_recv().is_err(), "pass 1 must gate silently");
    assert!(
        !seen.lock().await.contains_key(&path),
        "gated, not registered"
    );

    live.lock().await.insert(LIVE_UUID.to_string());

    scan_root(dir.path(), decoders, &ctx, &mut health).await;
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    let expected = AgentId::from_parts("test", LIVE_UUID);
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == expected
        )),
        "a re-vouched scan pass must register the gated live session, got {events:?}"
    );

    scan_root(dir.path(), decoders, &ctx, &mut health).await;
    assert!(
        rx.try_recv().is_err(),
        "a registered file must not be re-vouched/replayed again"
    );
}

/// The probe bypass exempts only the RECENCY half of the first-sight gate: a
/// liveness vouch is a PROXY for "the owning process is alive", never for "this
/// session is over" — omp's fd vouch fires for any bun tool merely READING an
/// old transcript.
#[tokio::test]
async fn probe_live_ended_first_sight_stays_unregistered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{LIVE_UUID}.jsonl"));
    tokio::fs::write(
        &path,
        "{\"type\":\"assistant\",\"cwd\":\"/repo\"}\n\
         {\"type\":\"system\",\"subtype\":\"session_end\"}\n",
    )
    .await
    .unwrap();
    let file_len = tokio::fs::metadata(&path).await.unwrap().len();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    // Recent mtime, so ONLY the ended half of the gate can fire.
    let events = walk_once_live(
        &path,
        Duration::from_secs(3600),
        &[LIVE_UUID],
        &cursors,
        &seen,
    )
    .await;
    assert!(
        events.is_empty(),
        "an ENDED probe-admitted first sight must emit nothing, got {events:?}"
    );
    assert!(
        !seen.lock().await.contains_key(&path),
        "an ENDED probe-admitted first sight must not claim/register"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(file_len),
        "the ended transcript must be parked at EOF"
    );
}

#[tokio::test]
async fn revouch_does_not_replay_a_probe_vouched_ended_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{LIVE_UUID}.jsonl"));
    tokio::fs::write(
        &path,
        "{\"type\":\"assistant\",\"cwd\":\"/repo\"}\n\
         {\"type\":\"system\",\"subtype\":\"session_end\"}\n",
    )
    .await
    .unwrap();
    let file_len = tokio::fs::metadata(&path).await.unwrap().len();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let live: Arc<Mutex<HashSet<String>>> =
        Arc::new(Mutex::new(HashSet::from([LIVE_UUID.to_string()])));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let decoders = SourceDecoders {
        decode_line: t_decode,
        derive_label: t_label,
        check_ended: t_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(crate::source::claude_code::cc_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    };
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };

    let mut health = FailureLatch::default();
    scan_root(dir.path(), decoders, &ctx, &mut health).await;
    assert!(
        drain_events(&mut rx).is_empty(),
        "the first pass must park the ended transcript silently"
    );
    scan_root(dir.path(), decoders, &ctx, &mut health).await;
    let events = drain_events(&mut rx);
    assert!(
        events.is_empty(),
        "a re-vouched ENDED transcript must not be replayed, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(file_len),
        "the ended transcript's cursor must stay parked at EOF (no reset-to-0 replay)"
    );
}

#[tokio::test]
async fn probe_live_oversized_ended_first_sight_stays_unregistered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{LIVE_UUID}.jsonl"));
    let mut full = String::from("{\"type\":\"assistant\",\"cwd\":\"/repo/head\"}\n");
    full.push_str(&"{\"type\":\"assistant\"}\n".repeat(60_000));
    full.push_str("{\"type\":\"system\",\"subtype\":\"session_end\"}\n");
    assert!(full.len() as u64 > (1 << 20), "body must exceed 1 MiB");
    tokio::fs::write(&path, &full).await.unwrap();
    backdate_one_hour(&path);
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once_live(
        &path,
        Duration::from_secs(60),
        &[LIVE_UUID],
        &cursors,
        &seen,
    )
    .await;
    assert!(
        events.is_empty(),
        "an ended oversized probe-admitted first sight must not register a ghost \
         (and its terminator is a reducer no-op for the unknown id it parks), got {events:?}"
    );
    assert!(
        !seen.lock().await.contains_key(&path),
        "an ended oversized probe-admitted first sight must not claim/register"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(full.len() as u64),
        "backlog must be skipped to EOF, not replayed"
    );
}

#[tokio::test]
async fn probe_parent_uuid_does_not_admit_subagent_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let sub_dir = dir.path().join(LIVE_UUID).join("subagents");
    tokio::fs::create_dir_all(&sub_dir).await.unwrap();
    let path = sub_dir.join("agent-deadbeef.jsonl");
    tokio::fs::write(&path, "{\"type\":\"assistant\",\"cwd\":\"/repo\"}\n")
        .await
        .unwrap();
    backdate_one_hour(&path);
    let len = tokio::fs::metadata(&path).await.unwrap().len();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once_live(
        &path,
        Duration::from_secs(60),
        &[LIVE_UUID],
        &cursors,
        &seen,
    )
    .await;
    assert!(
        events.is_empty(),
        "a stale subagent transcript must stay gated even when its parent is probe-live, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(len),
        "gated subagent transcript must be seeded at EOF"
    );
}

const FILLER_LINE: &str = "{\"type\":\"assistant\"}\n";
const CC_HEAD_LINE: &str = "{\"type\":\"assistant\",\"cwd\":\"/repo/head\"}\n";

fn cc_task_dispatch_line(tuid: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "cwd": "/repo/head",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": tuid, "name": "Agent",
                  "input": { "description": "explore",
                             "subagent_type": "code-explorer",
                             "prompt": "go" } }
            ]
        }
    })
    .to_string()
        + "\n"
}

fn cc_task_result_line(tuid: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [
                { "type": "tool_result", "tool_use_id": tuid, "content": "done" }
            ]
        }
    })
    .to_string()
        + "\n"
}

fn oversized_body(tail_lines: &[String]) -> String {
    let mut full = String::from(CC_HEAD_LINE);
    while full.len() <= (1usize << 20) + 4096 {
        full.push_str(FILLER_LINE);
    }
    for l in tail_lines {
        full.push_str(l);
    }
    full
}

fn task_start_tuids(events: &[(Transport, AgentEvent)]) -> Vec<String> {
    events
        .iter()
        .filter_map(|(t, e)| match e {
            AgentEvent::ActivityStart {
                tool_use_id: Some(tuid),
                detail: Some(d),
                ..
            } if d.is_task() => {
                assert_eq!(*t, Transport::Jsonl, "synthesized starts are Jsonl-tagged");
                Some(tuid.clone())
            }
            _ => None,
        })
        .collect()
}

async fn walk_oversized_cc(
    path: &Path,
    window: Duration,
    cursors: &Arc<Mutex<HashMap<PathBuf, u64>>>,
    seen: &Arc<Mutex<HashMap<PathBuf, Sight>>>,
) -> Vec<(Transport, AgentEvent)> {
    walk_once_with(
        path,
        window,
        crate::source::claude_code::decode_cc_line,
        t_ended,
        cursors,
        seen,
    )
    .await
}

#[tokio::test]
async fn oversized_attach_seeds_unmatched_task_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deleg-big.jsonl");
    let full = oversized_body(&[cc_task_dispatch_line("tu_task")]);
    tokio::fs::write(&path, &full).await.unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_oversized_cc(&path, Duration::from_secs(3600), &cursors, &seen).await;
    let start_pos = events
        .iter()
        .position(|(_, e)| matches!(e, AgentEvent::SessionStart { .. }))
        .expect("the oversized first sight must register the agent (#204)");
    assert_eq!(
        task_start_tuids(&events),
        vec!["tu_task".to_string()],
        "the unmatched in-flight dispatch must be re-emitted, got {events:?}"
    );
    let task_pos = events
        .iter()
        .position(|(_, e)| matches!(e, AgentEvent::ActivityStart { .. }))
        .expect("checked above");
    assert!(
        start_pos < task_pos,
        "registration must precede the synthesized Task start (a JSONL event for an unknown id is a reducer no-op)"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(full.len() as u64),
        "cursor must land at EOF — the scan seeds tasks, it must not replay the backlog"
    );
}

#[tokio::test]
async fn oversized_attach_matched_task_is_not_seeded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deleg-done.jsonl");
    let full = oversized_body(&[
        cc_task_dispatch_line("tu_task"),
        cc_task_result_line("tu_task"),
    ]);
    tokio::fs::write(&path, &full).await.unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_oversized_cc(&path, Duration::from_secs(3600), &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "registration still fires, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::ActivityStart { .. })),
        "a matched (returned) dispatch must not be seeded — and no other backlog event may leak from the scan, got {events:?}"
    );
}

#[tokio::test]
async fn pending_span_of_exactly_max_bytes_replays_instead_of_skipping() {
    let bash_line = serde_json::json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_bash", "name": "Bash",
                  "input": { "command": "ls" } }
            ]
        }
    })
    .to_string()
        + "\n";
    // walk_jsonl's MAX_PENDING_BYTES (a fn-local const).
    let target = 1usize << 20;
    let pad_open = "{\"type\":\"assistant\",\"pad\":\"";
    let pad_close = "\"}\n";
    let pad_len = target - CC_HEAD_LINE.len() - bash_line.len() - pad_open.len() - pad_close.len();
    let full = format!(
        "{CC_HEAD_LINE}{pad_open}{}{pad_close}{bash_line}",
        "x".repeat(pad_len)
    );
    assert_eq!(full.len(), target, "fixture: exactly 1 MiB pending");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boundary.jsonl");
    tokio::fs::write(&path, &full).await.unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let events = walk_oversized_cc(&path, Duration::from_secs(3600), &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::ActivityStart { .. })),
        "an exactly-at-the-cap span must REPLAY (strict >) — the ordinary \
         tool activity must come through, got {events:?}"
    );
}

#[tokio::test]
async fn oversized_attach_seeds_dispatches_across_the_full_scan_window() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deleg-deep.jsonl");
    let mut tail = vec![cc_task_dispatch_line("tu_deep")];
    tail.extend(std::iter::repeat_n(FILLER_LINE.to_string(), 200));
    tail.push(cc_task_dispatch_line("tu_near"));
    let full = oversized_body(&tail);
    tokio::fs::write(&path, &full).await.unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_oversized_cc(&path, Duration::from_secs(3600), &cursors, &seen).await;
    assert_eq!(
        task_start_tuids(&events),
        vec!["tu_deep".to_string(), "tu_near".to_string()],
        "both in-flight dispatches across the window must be seeded once, in file order"
    );
}

#[tokio::test]
async fn oversized_attach_ended_session_skips_task_scan() {
    // Pre-seeded KNOWN at the head: a recent ENDED file at FIRST sight is gated
    // by should_seed_at_eof and never reaches the oversized branch at all.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deleg-ended.jsonl");
    let full = oversized_body(&[
        cc_task_dispatch_line("tu_task"),
        "{\"type\":\"system\",\"subtype\":\"session_end\"}\n".to_string(),
    ]);
    tokio::fs::write(&path, &full).await.unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::from([(
        path.clone(),
        CC_HEAD_LINE.len() as u64,
    )])));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_oversized_cc(&path, Duration::from_secs(3600), &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionEnd { .. })),
        "the buried terminator must still emit SessionEnd, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::ActivityStart { .. })),
        "an ended span must not seed Task starts, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "an ended span must not register either (ghost), got {events:?}"
    );
}

#[tokio::test]
async fn oversized_attach_unregistered_skips_task_scan() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deleg-stale.jsonl");
    let full = oversized_body(&[cc_task_dispatch_line("tu_task")]);
    tokio::fs::write(&path, &full).await.unwrap();
    backdate_one_hour(&path);
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_oversized_cc(&path, Duration::from_secs(60), &cursors, &seen).await;
    assert!(
        events.is_empty(),
        "a gated unregistered oversized file must emit NOTHING — no Task seeding without a slot, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(full.len() as u64),
        "gated file must still be seeded at EOF"
    );
}

#[tokio::test]
async fn oversized_attach_dispatch_outside_window_is_missed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deleg-buried.jsonl");
    let mut full = String::from(CC_HEAD_LINE);
    let dispatch = cc_task_dispatch_line("tu_buried");
    full.push_str(&dispatch);
    let dispatch_end = full.len();
    while full.len() <= (1usize << 20) + 4096 {
        full.push_str(FILLER_LINE);
    }
    assert!(
        (full.len() - dispatch_end) as u64 > TASK_SCAN_BYTES,
        "the dispatch must sit deeper than the scan window"
    );
    tokio::fs::write(&path, &full).await.unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_oversized_cc(&path, Duration::from_secs(3600), &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "registration still fires, got {events:?}"
    );
    assert!(
        task_start_tuids(&events).is_empty(),
        "a dispatch outside the tail window is consciously missed (bounded residual), got {events:?}"
    );
}

#[tokio::test]
async fn task_scan_handles_partial_first_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deleg-straddle.jsonl");
    let task_a = cc_task_dispatch_line("tu_straddle");
    let task_b = cc_task_dispatch_line("tu_inside");

    let mut full = String::from(CC_HEAD_LINE);
    // Deep enough that suffix + window keeps the total > MAX_PENDING_BYTES.
    while full.len() < (1usize << 20) {
        full.push_str(FILLER_LINE);
    }
    let offset_a = full.len();
    full.push_str(&task_a);
    full.push_str(&task_b);
    let delta = task_a.len() / 2;
    let target_len = offset_a + delta + TASK_SCAN_BYTES as usize;
    let pad = target_len - full.len();
    assert!(pad > FILLER_LINE.len(), "padding must fit one JSON line");
    full.push_str("{\"type\":\"assistant\"}");
    full.push_str(&" ".repeat(pad - FILLER_LINE.len()));
    full.push('\n');
    assert_eq!(full.len(), target_len);
    let boundary = full.len() - TASK_SCAN_BYTES as usize;
    assert!(
        boundary > offset_a && boundary < offset_a + task_a.len(),
        "the window boundary must split the straddled dispatch mid-line"
    );
    tokio::fs::write(&path, &full).await.unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_oversized_cc(&path, Duration::from_secs(3600), &cursors, &seen).await;
    assert_eq!(
        task_start_tuids(&events),
        vec!["tu_inside".to_string()],
        "the complete in-window dispatch seeds; the straddled fragment is skipped, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(full.len() as u64),
        "cursor must land at EOF"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn walk_refuses_symlinked_entries() {
    let outside = tempfile::tempdir().unwrap();
    let foreign = outside.path().join("foreign.jsonl");
    std::fs::write(&foreign, "{\"type\":\"assistant\"}\n").unwrap();

    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real.jsonl");
    std::fs::write(&real, "{\"type\":\"assistant\"}\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    std::os::unix::fs::symlink(&foreign, root.path().join("link.jsonl")).unwrap();
    std::os::unix::fs::symlink(root.path(), root.path().join("loop")).unwrap();

    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(64);
    let source: Arc<str> = Arc::from("test");
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };
    let mut health = FailureLatch::default();
    scan_root(root.path(), t_decoders(), &ctx, &mut health).await;
    drop(tx);
    let events = drain_events(&mut rx);

    let real_id = AgentId::from_parts("test", &default_id_from_path(&real));
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == real_id
        )),
        "the real transcript must still register, got {events:?}"
    );
    let foreign_id = AgentId::from_parts("test", &default_id_from_path(&foreign));
    assert!(
        !events.iter().any(|(_, e)| e.agent_id() == foreign_id),
        "a symlinked foreign transcript must emit nothing, got {events:?}"
    );
    assert!(
        cursors
            .lock()
            .await
            .keys()
            .all(|p| p.parent() == Some(root.path()) && !p.is_symlink()),
        "no symlinked or out-of-root path may be tracked: {:?}",
        cursors.lock().await.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn session_exit_parks_truncated_transcript_so_a_straggler_walk_cannot_resurrect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"assistant\"}\n{\"type\":\"assistant\"}\n{\"type\":\"assistant\"}\n",
    )
    .unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let window = Duration::from_secs(3600);

    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. }))
    );

    std::fs::write(&path, "{\"type\":\"assistant\"}\n").unwrap();
    let new_len = std::fs::metadata(&path).unwrap().len();
    assert!(
        *cursors.lock().await.get(&path).unwrap() > new_len,
        "fixture: the file must be truncated below the cursor"
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window,
        live: &live,
    };
    let id = default_id_from_path(&path);
    emit_session_exit(&id, t_decoders(), &ctx).await;
    drop(tx);
    let exit_events = drain_events(&mut rx);
    assert!(
        matches!(
            exit_events.last(),
            Some((Transport::Jsonl, AgentEvent::SessionEnd { .. }))
        ),
        "the terminator must be emitted (last), got {exit_events:?}"
    );
    assert!(
        !exit_events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "the exit drain must not register anything, got {exit_events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(new_len),
        "a truncated transcript must be PARKED at the new EOF, not reset to 0"
    );
    assert!(
        !seen.lock().await.contains_key(&path),
        "seen must be un-claimed so a genuine post-death append revives"
    );

    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events.is_empty(),
        "a straggler walk after the exit must not resurrect, got {events:?}"
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"assistant\"}\n")
        .unwrap();
    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "a post-exit append must re-register, got {events:?}"
    );
}

#[tokio::test]
async fn child_end_unclaim_parks_truncated_transcript_before_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("child.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"assistant\"}\n{\"type\":\"assistant\"}\n{\"type\":\"assistant\"}\n",
    )
    .unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let window = Duration::from_secs(3600);

    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. }))
    );

    std::fs::write(&path, "{\"type\":\"assistant\"}\n").unwrap();
    let new_len = std::fs::metadata(&path).unwrap().len();
    assert!(
        *cursors.lock().await.get(&path).unwrap() > new_len,
        "fixture: the file must be truncated below the cursor"
    );

    let unclaims = ChildEndUnclaims::new();
    let id = AgentId::from_parts("test", &default_id_from_path(&path));
    unclaims.push(id);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(64);
    let source: Arc<str> = Arc::from("test");
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window,
        live: &live,
    };
    drain_child_end_unclaims(Some(&unclaims), t_decoders(), &ctx).await;
    let events = drain_events(&mut rx);
    assert!(
        events.is_empty(),
        "the un-claim emits NOTHING, truncated or not — got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(new_len),
        "a truncated rollout must be PARKED at the new EOF before the end"
    );
    assert_eq!(
        seen.lock().await.get(&path),
        Some(&Sight::Ended),
        "the claim must still be ENDED (kept known)"
    );

    let events = walk_once(&path, window, t_ended, &cursors, &seen).await;
    assert!(
        events.is_empty(),
        "a straggler walk after the end must not resurrect, got {events:?}"
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"tool_use\"}\n")
        .unwrap();
    let events = walk_once_with(&path, window, t_decode_lifecycle, t_ended, &cursors, &seen).await;
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == id
        )),
        "the turn-N+1 append must re-register the child, got {events:?}"
    );
}

#[tokio::test]
async fn park_if_truncated_below_cursor_lands_exactly_at_new_eof() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    std::fs::write(&path, vec![b'x'; 40]).unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::from([(path.clone(), 100u64)])));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let (tx, _rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
    let source: Arc<str> = Arc::from("test");
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };

    park_if_truncated_below_cursor(&path, &ctx).await;
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(40),
        "the park must land at the NEW EOF, not 0"
    );

    cursors.lock().await.insert(path.clone(), 10);
    park_if_truncated_below_cursor(&path, &ctx).await;
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(10),
        "a cursor at/below the file length must be untouched"
    );
}

#[tokio::test]
async fn scan_root_on_unreadable_root_latches_failure_and_emits_nothing() {
    // A file where the root should be: present, but not listable.
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("projects");
    std::fs::write(&bad, b"").unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };

    let mut health = FailureLatch::default();
    scan_root(&bad, t_decoders(), &ctx, &mut health).await;
    drop(tx);
    let events = drain_events(&mut rx);
    assert!(
        events.is_empty(),
        "an unreadable root discovers no sessions, got {events:?}"
    );
    assert!(
        !health.on_failure(),
        "scan_root's Err arm must have already latched the failure"
    );
}

#[tokio::test]
async fn scan_root_on_absent_root_is_healthy_and_emits_nothing() {
    let bad: PathBuf = std::env::temp_dir().join(format!("pixtuoid-no-such-{}", uuid_like()));
    assert!(!bad.exists(), "fixture: the bad root must not exist");
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let live = Arc::new(Mutex::new(HashSet::new()));
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };

    let mut health = FailureLatch::default();
    scan_root(&bad, t_decoders(), &ctx, &mut health).await;
    drop(tx);
    let events = drain_events(&mut rx);
    assert!(
        events.is_empty(),
        "an absent root discovers no sessions, got {events:?}"
    );
    assert!(
        health.on_failure(),
        "an absent root (an uninstalled CLI's) must not latch a failure"
    );
}

#[tokio::test]
async fn scan_root_recovers_after_a_failed_root_reports_success_once() {
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(32);
    let source: Arc<str> = Arc::from("test");
    let live = Arc::new(Mutex::new(HashSet::new()));

    let good = tempfile::tempdir().unwrap();
    let real = good.path().join("real.jsonl");
    tokio::fs::write(&real, "{\"type\":\"assistant\",\"cwd\":\"/r\"}\n")
        .await
        .unwrap();

    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(3600),
        live: &live,
    };

    let mut health = FailureLatch::default();
    let bad: PathBuf = std::env::temp_dir().join(format!("pixtuoid-no-such-{}", uuid_like()));
    scan_root(&bad, t_decoders(), &ctx, &mut health).await;
    assert!(rx.try_recv().is_err(), "the bad root emits nothing");

    scan_root(good.path(), t_decoders(), &ctx, &mut health).await;
    drop(tx);
    let events = drain_events(&mut rx);
    let expected = AgentId::from_parts("test", &default_id_from_path(&real));
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == expected
        )),
        "the recovered root must register the real transcript, got {events:?}"
    );
    assert!(
        !health.on_success(),
        "scan_root's Ok arm must have already reported the recovery"
    );
    assert!(
        health.on_failure(),
        "a failure after the consumed recovery must report again"
    );
}

#[tokio::test]
async fn walk_jsonl_recurses_into_a_subdirectory_and_registers_nested_transcripts() {
    let dir = tempfile::tempdir().unwrap();
    let day = dir.path().join("day");
    tokio::fs::create_dir_all(&day).await.unwrap();
    let nested = day.join("ses.jsonl");
    tokio::fs::write(&nested, "{\"type\":\"assistant\",\"cwd\":\"/r\"}\n")
        .await
        .unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once(
        dir.path(),
        Duration::from_secs(3600),
        t_ended,
        &cursors,
        &seen,
    )
    .await;
    let expected = AgentId::from_parts("test", &default_id_from_path(&nested));
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == expected
        )),
        "the nested transcript must register through the directory recursion, got {events:?}"
    );
    assert!(
        cursors.lock().await.contains_key(&nested),
        "the nested transcript's cursor must be tracked"
    );
}

#[tokio::test]
async fn walk_jsonl_skips_a_non_jsonl_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("foo.txt");
    tokio::fs::write(&path, "{\"type\":\"assistant\",\"cwd\":\"/r\"}\n")
        .await
        .unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once(&path, Duration::from_secs(3600), t_ended, &cursors, &seen).await;
    assert!(
        events.is_empty(),
        "a non-.jsonl file must emit nothing, got {events:?}"
    );
    assert!(
        cursors.lock().await.is_empty(),
        "a non-.jsonl file must never be tracked"
    );
}

#[tokio::test]
async fn walk_jsonl_on_a_missing_path_is_a_silent_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ghost.jsonl");
    assert!(!path.exists(), "fixture: the path must not exist");
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once(&path, Duration::from_secs(3600), t_ended, &cursors, &seen).await;
    assert!(
        events.is_empty(),
        "a missing path must emit nothing, got {events:?}"
    );
    assert!(
        !cursors.lock().await.contains_key(&path),
        "a missing path must never be tracked"
    );
}

#[tokio::test]
async fn walk_jsonl_resets_cursor_to_zero_when_known_file_truncated_below_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resync.jsonl");
    tokio::fs::write(&path, "{\"type\":\"assistant\"}\n")
        .await
        .unwrap();
    let file_len = tokio::fs::metadata(&path).await.unwrap().len();
    assert!(
        file_len < 999,
        "fixture: the file must be shorter than the seeded cursor"
    );

    // `seen` is pre-claimed so this takes the normal-walk truncation arm (the
    // first-sight gate is skipped for a known file).
    let cursors = Arc::new(Mutex::new(HashMap::from([(path.clone(), 999u64)])));
    let seen = Arc::new(Mutex::new(HashMap::from([(path.clone(), Sight::Claimed)])));

    let events = walk_once(&path, Duration::from_secs(3600), t_ended, &cursors, &seen).await;
    assert!(
        events.is_empty(),
        "the truncation resync emits nothing this pass, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(0),
        "the normal-walk truncation arm must RESET the cursor to 0 (not park at EOF)"
    );
}

#[tokio::test]
async fn walk_jsonl_skips_a_line_whose_decoder_errors_and_advances_cursor() {
    fn err_decode(
        _t: &str,
        _s: &str,
        v: serde_json::Value,
    ) -> crate::source::decoder::DecodeResult<Vec<AgentEvent>> {
        if v.get("boom").is_some() {
            return Err(crate::source::decoder::DecodeError::unsupported(
                "fixture", "boom",
            ));
        }
        Ok(vec![])
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("badline.jsonl");
    let body = "{\"boom\":1}\n{\"type\":\"assistant\",\"cwd\":\"/r\"}\n";
    tokio::fs::write(&path, body).await.unwrap();
    let file_len = body.len() as u64;
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once_with(
        &path,
        Duration::from_secs(3600),
        err_decode,
        t_ended,
        &cursors,
        &seen,
    )
    .await;
    let expected = AgentId::from_parts("test", &default_id_from_path(&path));
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            AgentEvent::SessionStart { agent_id, .. } if *agent_id == expected
        )),
        "the erroring line is non-fatal; first-sight registration must still run, got {events:?}"
    );
    assert_eq!(
        cursors.lock().await.get(&path).copied(),
        Some(file_len),
        "the cursor must advance to EOF despite the decode error"
    );
}

fn oversized_body_bytes(tail_chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut full = Vec::from(CC_HEAD_LINE_BYTES);
    while full.len() <= (1usize << 20) + 4096 {
        full.extend_from_slice(FILLER_LINE_BYTES);
    }
    for c in tail_chunks {
        full.extend_from_slice(c);
    }
    full
}

const CC_HEAD_LINE_BYTES: &[u8] = b"{\"type\":\"assistant\",\"cwd\":\"/repo/head\"}\n";
const FILLER_LINE_BYTES: &[u8] = b"{\"type\":\"assistant\"}\n";

#[tokio::test]
async fn task_scan_skips_empty_and_non_utf8_lines_and_still_seeds_a_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deleg-garbage.jsonl");
    let full = oversized_body_bytes(&[
        b"\n".to_vec(),
        b"\xff\xfe garbage \xff\n".to_vec(),
        cc_task_dispatch_line("tu_x").into_bytes(),
    ]);
    tokio::fs::write(&path, &full).await.unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_oversized_cc(&path, Duration::from_secs(3600), &cursors, &seen).await;
    assert_eq!(
        task_start_tuids(&events),
        vec!["tu_x".to_string()],
        "the garbage lines must be skipped and the valid dispatch still seed, got {events:?}"
    );
}

#[tokio::test]
async fn task_scan_skips_a_decoder_error_line_and_still_seeds_a_later_dispatch() {
    fn deco(
        t: &str,
        s: &str,
        v: serde_json::Value,
    ) -> crate::source::decoder::DecodeResult<Vec<AgentEvent>> {
        if v.get("boom").is_some() {
            return Err(crate::source::decoder::DecodeError::unsupported(
                "fixture", "x",
            ));
        }
        crate::source::claude_code::decode_cc_line(t, s, v)
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deleg-decode-err.jsonl");
    let full = oversized_body(&["{\"boom\":1}\n".to_string(), cc_task_dispatch_line("tu_y")]);
    tokio::fs::write(&path, &full).await.unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));

    let events = walk_once_with(
        &path,
        Duration::from_secs(3600),
        deco,
        t_ended,
        &cursors,
        &seen,
    )
    .await;
    assert_eq!(
        task_start_tuids(&events),
        vec!["tu_y".to_string()],
        "the decoder-error line must be skipped and the valid dispatch still seed, got {events:?}"
    );
}

#[tokio::test]
async fn deleted_gated_file_walk_evicts_its_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gone.jsonl");
    let (cursors, seen) = gated_fixture(&path, "{\"type\":\"assistant\"}\n").await;
    assert!(cursors.lock().await.contains_key(&path));

    tokio::fs::remove_file(&path).await.unwrap();
    let events = walk_once(&path, Duration::from_secs(60), t_ended, &cursors, &seen).await;
    assert!(
        events.is_empty(),
        "a deleted path emits nothing: {events:?}"
    );
    assert!(
        !cursors.lock().await.contains_key(&path),
        "the cursor entry of a deleted file must be evicted"
    );
}

#[tokio::test]
async fn deleted_registered_file_walk_evicts_cursor_and_claim() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gone-live.jsonl");
    tokio::fs::write(&path, "{\"type\":\"assistant\",\"cwd\":\"/r\"}\n")
        .await
        .unwrap();
    let cursors = Arc::new(Mutex::new(HashMap::new()));
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let events = walk_once(&path, Duration::from_secs(60), t_ended, &cursors, &seen).await;
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::SessionStart { .. })),
        "fixture must register first, got {events:?}"
    );

    tokio::fs::remove_file(&path).await.unwrap();
    walk_once(&path, Duration::from_secs(60), t_ended, &cursors, &seen).await;
    assert!(!cursors.lock().await.contains_key(&path));
    assert!(
        !seen.lock().await.contains_key(&path),
        "the first-sight claim of a deleted file must be evicted"
    );
}

#[tokio::test]
async fn revouch_pass_prunes_deleted_files_from_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("deleted.jsonl");
    let cursors: Arc<Mutex<HashMap<PathBuf, u64>>> = Arc::new(Mutex::new(HashMap::new()));
    cursors.lock().await.insert(gone.clone(), 42);
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let live: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(
        std::iter::once("someone-alive".to_string()).collect(),
    ));
    let (tx, _rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
    let source: Arc<str> = Arc::from("test");
    let decoders = SourceDecoders {
        decode_line: t_decode,
        derive_label: t_label,
        check_ended: t_ended,
        activity_recency: super::no_activity_recency,
        id_derive: super::folded::FoldedDeriver::new(default_id_from_path),
        path_filter: accept_all_paths,
        cwd_derive: no_cwd_from_path,
        head_label: None,
    };
    let ctx = WatchCtx {
        source: &source,
        cursors: &cursors,
        seen: &seen,
        tx: &tx,
        window: Duration::from_secs(60),
        live: &live,
    };
    revouch_gated_files(decoders, &ctx).await;
    assert!(
        !cursors.lock().await.contains_key(&gone),
        "a NotFound re-vouch candidate must be pruned from cursors"
    );
}

/// Property tests for the #223 probe ladder under INTERLEAVED rungs. The
/// properties are deliberately WEAKER than the implementation: a model that
/// mirrors the algorithm reproduces its bugs and passes with them.
mod ladder_props {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet;

    const IDS: [&str; 3] = ["s0", "s1", "s2"];
    const PIDS: [i32; 3] = [11, 22, 33];
    const SPAN: Duration = Duration::from_secs(60);

    fn snap_of(live: &[(usize, usize)]) -> ProbeSnapshot {
        let mut s = ProbeSnapshot::default();
        for (id, pid) in live {
            s.bind_pid(IDS[*id].to_string(), PIDS[*pid]);
        }
        s
    }

    #[derive(Debug, Clone)]
    enum Op {
        Fold {
            live: Vec<(usize, usize)>,
            advance_ms: u64,
        },
        PidDied(usize),
    }

    fn arb_live() -> impl Strategy<Value = Vec<(usize, usize)>> {
        proptest::collection::vec((0usize..3, 0usize..3), 0..4)
    }

    fn arb_ops() -> impl Strategy<Value = Vec<Op>> {
        // Deltas straddle the confirm boundary on BOTH sides, so a window can
        // open, age partway, and cross `min_span` mid-sequence.
        let advance = prop_oneof![Just(0u64), Just(30_000), Just(60_000), Just(120_000)];
        let fold =
            (arb_live(), advance).prop_map(|(live, advance_ms)| Op::Fold { live, advance_ms });
        let died = (0usize..3).prop_map(Op::PidDied);
        proptest::collection::vec(prop_oneof![3 => fold, 1 => died], 1..14)
    }

    proptest! {
        #[test]
        fn no_exit_is_confirmed_while_the_clock_stands_still(
            snaps in proptest::collection::vec(arb_live(), 1..14)
        ) {
            let mut ladder = ProbeLadder::new(SPAN);
            let t = Instant::now();
            for live in &snaps {
                let out = ladder.fold(&snap_of(live), t);
                prop_assert!(
                    out.exits.is_empty(),
                    "confirmed {:?} with zero elapsed time", out.exits
                );
            }
        }

        #[test]
        fn a_continuously_vouched_id_never_exits(
            others in proptest::collection::vec(arb_live(), 1..14)
        ) {
            let mut ladder = ProbeLadder::new(SPAN);
            let mut now = Instant::now();
            for (i, extra) in others.iter().enumerate() {
                now += SPAN * (i as u32 % 3 + 1);
                let mut live = vec![(0usize, 0usize)];
                live.extend(extra.iter().copied());
                let out = ladder.fold(&snap_of(&live), now);
                prop_assert!(
                    !out.exits.iter().any(|id| id == IDS[0]),
                    "a continuously vouched id was confirmed dead: {:?}", out.exits
                );
            }
        }

        #[test]
        fn no_id_exits_twice_without_a_re_vouch(ops in arb_ops()) {
            let mut ladder = ProbeLadder::new(SPAN);
            let mut now = Instant::now();
            let mut dead: HashSet<String> = HashSet::new();
            for op in &ops {
                match op {
                    Op::Fold { live, advance_ms } => {
                        now += Duration::from_millis(*advance_ms);
                        let snap = snap_of(live);
                        for id in snap.pid_of.keys() {
                            dead.remove(id);
                        }
                        for id in ladder.fold(&snap, now).exits {
                            prop_assert!(
                                dead.insert(id.clone()),
                                "{id} exited twice without a re-vouch"
                            );
                        }
                    }
                    Op::PidDied(p) => {
                        for id in ladder.pid_died(PIDS[*p]) {
                            prop_assert!(
                                dead.insert(id.clone()),
                                "{id} exited twice without a re-vouch"
                            );
                        }
                    }
                }
            }
        }

        #[test]
        fn an_id_is_bound_under_at_most_one_pid(ops in arb_ops()) {
            let mut ladder = ProbeLadder::new(SPAN);
            let mut now = Instant::now();
            for op in &ops {
                match op {
                    Op::Fold { live, advance_ms } => {
                        now += Duration::from_millis(*advance_ms);
                        ladder.fold(&snap_of(live), now);
                    }
                    Op::PidDied(p) => {
                        ladder.pid_died(PIDS[*p]);
                    }
                }
            }
            let mut seen: HashSet<String> = HashSet::new();
            for pid in PIDS {
                for id in ladder.pid_died(pid) {
                    prop_assert!(
                        seen.insert(id.clone()),
                        "{id} was bound under more than one pid"
                    );
                }
            }
        }
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{nanos}-{:p}", &nanos)
}

/// Pins that `id_for` derives from `id_path(path)` rather than the raw path.
///
/// This arm CANNOT fail on Unix — the fold is identity there, so a
/// pass-through `id_for` returns the same string. It pins the shape; the
/// discriminating assertion is the `cfg(windows)` twin below, which runs in the
/// `windows-test` job (the same split as
/// `claude_code::detect_parent_id_handles_backslash_paths`).
#[test]
fn folded_deriver_applies_the_id_path_fold() {
    fn echo(p: &std::path::Path) -> String {
        p.to_string_lossy().into_owned()
    }
    let d = super::folded::FoldedDeriver::new(echo);
    for raw in [
        "/Users/me/.claude/projects/repo/abc.jsonl",
        r"C:\Users\Me\.claude\Projects\Repo\ABC.jsonl",
    ] {
        assert_eq!(
            d.id_for(std::path::Path::new(raw)),
            crate::id::normalize_path_key(raw),
            "id_for must derive from the FOLDED path, not the raw one"
        );
    }
}

/// The discriminating half: on Windows the fold is not identity, so a
/// pass-through `id_for` would return the raw backslash/mixed-case form. Mirrors
/// `claude_code::detect_parent_id_handles_backslash_paths`.
#[cfg(windows)]
#[test]
fn folded_deriver_folds_separators_and_case_on_windows() {
    fn echo(p: &std::path::Path) -> String {
        p.to_string_lossy().into_owned()
    }
    let d = super::folded::FoldedDeriver::new(echo);
    let id = d.id_for(std::path::Path::new(r"C:\Users\Me\Repo\ABC.jsonl"));
    assert!(
        !id.contains('\\') && id == id.to_lowercase(),
        "the Windows fold must normalize separators AND case, got {id:?}"
    );
}
