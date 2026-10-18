//! The `native`-only runtime half of the grok source: the liveness probe over
//! grok's own crash-recovery registry (`active_sessions.json`) + `GrokSource`
//! and its `JsonlWatcher` wiring.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{SOURCE_NAME, decode_grok_line, grok_cwd_from_path, grok_home, grok_session_ended};
use crate::source::jsonl::{ChildEndUnclaims, JsonlWatcher, ProbeSnapshot};
use crate::source::{Source, TaggedSender};

/// Sized for the whole registry, not one entry: grok writes EVERY live TUI
/// session into this single file, where `cc_probe`'s 64 KiB bounds one
/// per-session file. Truncated bytes fail the JSON parse and take the
/// shape-drift path.
#[cfg(unix)]
const MAX_SESSION_REGISTRY_BYTES: u64 = 1024 * 1024;

/// grok's liveness probe: the session ids of every entry in
/// `{grok_home}/active_sessions.json` whose pid is alive, plus the owning pid
/// per id for the instant-exit watch.
///
/// The registry is grok's OWN crash-recovery design: registered per TUI session
/// with `std::process::id()`, removed on clean quit, left behind on crash. grok
/// keeps NO long-lived fd on its session files, so a Codex-style open-FD probe
/// is impossible. Headless (`-p`) sessions are NOT registered, so they ride the
/// mtime gate + short-idle reap instead.
///
/// Failure semantics: an ABSENT registry file is `Some(empty)` — a healthy
/// "nothing alive" observation. An unreadable or unparseable file is `None` —
/// the enumeration itself failed, so the watcher changes nothing.
#[cfg(unix)]
pub(crate) fn live_grok_session_ids(grok_root: &Path) -> Option<ProbeSnapshot> {
    let path = grok_root.join("active_sessions.json");
    let bytes = match crate::source::read_bounded_bytes(&path, MAX_SESSION_REGISTRY_BYTES) {
        Ok(b) => b,
        // Absent registry = healthy "no TUI clients".
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(ProbeSnapshot::default()),
        Err(_) => return None,
    };
    match grok_ids_from_registry(&bytes, crate::source::cc_probe::pid_alive, |pid| {
        crate::source::cc_probe::pid_start_time_secs(pid)
    }) {
        Some(snap) => Some(snap),
        None => {
            // A read that hit the cap parses as garbage too, so say which it
            // was — a truncation reported as upstream drift sends the reader
            // hunting a format change that never happened.
            let truncated = bytes.len() as u64 == MAX_SESSION_REGISTRY_BYTES;
            static SHAPE_DRIFT_WARNED: std::sync::Once = std::sync::Once::new();
            SHAPE_DRIFT_WARNED.call_once(|| {
                let cause = if truncated {
                    "was TRUNCATED at the read cap, so it cannot parse"
                } else {
                    "does not parse as the expected [{session_id,pid,cwd,opened_at}] \
                     array — the registry shape changed upstream"
                };
                crate::source::drift::shape_drift(
                    SOURCE_NAME,
                    &format!(
                        "active_sessions.json at {} {cause}; liveness degraded to \
                         mtime gating",
                        path.display()
                    ),
                );
            });
            None
        }
    }
}

/// Non-Unix stub — the probe needs pid-liveness plus a kernel-start recycle
/// check, so off Unix grok liveness degrades to pure mtime gating.
#[cfg(not(unix))]
pub(crate) fn live_grok_session_ids(_grok_root: &Path) -> Option<ProbeSnapshot> {
    None
}

/// Parse the registry array and keep entries whose pid is alive AND — when BOTH
/// sides are available — whose `opened_at` matches the kernel-reported process
/// start within `cc_probe::PID_START_TOLERANCE_SECS` (the pid-recycle identity
/// check; either side missing → pid-alive-only). `None` ONLY when the DOCUMENT
/// doesn't parse as an array of entries (format drift); junk VALUES inside an
/// entry — a pid `decoder::checked_pid` rejects, an empty `session_id`, a
/// recycled pid — skip that entry silently.
#[cfg(unix)]
fn grok_ids_from_registry(
    bytes: &[u8],
    alive: fn(i32) -> bool,
    start_time: impl Fn(i32) -> Option<u64>,
) -> Option<ProbeSnapshot> {
    #[derive(serde::Deserialize)]
    struct Entry {
        session_id: String,
        // `i64`: an out-of-`i32` value must skip its entry, not fail the
        // document — see `decoder::checked_pid` (#831).
        pid: i64,
        #[serde(default)]
        opened_at: Option<String>,
    }
    let entries: Vec<Entry> = serde_json::from_slice(bytes).ok()?;
    let mut snap = ProbeSnapshot::default();
    for e in entries {
        // The i32-range + strictly-positive narrowing every JSON pid ingress
        // shares (the `kill(0)`/`kill(-n)`-targets-a-GROUP rationale lives on
        // `checked_pid`) — the same call `cc_probe::parse_registry_entry` makes.
        let Some(pid) = crate::source::decoder::checked_pid(e.pid) else {
            continue;
        };
        if e.session_id.is_empty() || !alive(pid) {
            continue;
        }
        if let (Some(claimed_secs), Some(actual_secs)) = (
            e.opened_at
                .as_deref()
                .and_then(crate::source::decoder::rfc3339_to_epoch_secs),
            start_time(pid),
        ) {
            // `opened_at` is stamped at session OPEN, which can lag process
            // start by however long the user sat on the welcome screen. Only a
            // claim EARLIER than the process start is evidence of recycling —
            // a session can't have opened before its process existed.
            if claimed_secs + crate::source::cc_probe::PID_START_TOLERANCE_SECS < actual_secs {
                tracing::debug!(
                    pid,
                    claimed_secs,
                    actual_secs,
                    "pid recycled — active_sessions opened_at predates process start; skipping"
                );
                continue;
            }
        }
        // Duplicate session_id across entries is upstream junk — keep the
        // deterministic tiebreak winner (larger pid).
        snap.bind_pid(e.session_id, pid);
    }
    Some(snap)
}

/// Attach the probe ONLY for grok's first-party layout, so a root pointed
/// elsewhere (a test's temp dir) keeps the pure-mtime first-sight gate. The registry file is a SIBLING of the sessions root, so the probe root
/// is the sessions root's PARENT.
fn grok_probe_root(sessions_root: &Path) -> Option<PathBuf> {
    grok_probe_root_resolved(sessions_root, &grok_home())
}

fn grok_probe_root_resolved(sessions_root: &Path, home: &Path) -> Option<PathBuf> {
    if sessions_root.file_name().and_then(|n| n.to_str()) != Some("sessions") {
        return None;
    }
    let parent = sessions_root.parent()?;
    let parent_is_grok = parent.file_name().and_then(|n| n.to_str()) == Some(".grok");
    let parent_is_resolved_home = parent == home;
    if !parent_is_grok && !parent_is_resolved_home {
        return None;
    }
    Some(parent.to_path_buf())
}

/// Source that watches the grok session transcript tree.
#[derive(Debug)]
pub struct GrokSource {
    /// The watched grok session-transcript root; per-session `updates.jsonl` lives under it.
    pub sessions_root: PathBuf,
    /// The child-end un-claim side-channel: releases an ended child's
    /// flat-sibling transcript claim so a `resume_from` / late-append revival
    /// re-registers cleanly. `None` disables it (bare test construction).
    pub child_end_unclaims: Option<ChildEndUnclaims>,
}

impl GrokSource {
    /// Construct pointed at the default grok `sessions` root.
    pub fn default_paths() -> Self {
        Self {
            sessions_root: grok_home().join("sessions"),
            child_end_unclaims: None,
        }
    }
}

impl Source for GrokSource {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        let mut watcher = JsonlWatcher::new(
            self.sessions_root.clone(),
            SOURCE_NAME.to_string(),
            decode_grok_line,
            grok_session_ended,
        )
        .with_cwd_deriver(grok_cwd_from_path);
        if let Some(root) = grok_probe_root(&self.sessions_root) {
            watcher = watcher
                .with_liveness_probe(std::sync::Arc::new(move || live_grok_session_ids(&root)));
        }
        if let Some(unclaims) = &self.child_end_unclaims {
            watcher = watcher.with_child_end_unclaims(unclaims.clone());
        }
        watcher.run(tx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_root_accepts_first_party_layouts_only() {
        let home = Path::new("/custom/grok-home");
        assert_eq!(
            grok_probe_root_resolved(Path::new("/Users/u/.grok/sessions"), home),
            Some(PathBuf::from("/Users/u/.grok"))
        );
        // Resolved GROK_HOME layout: parent == home even though not `.grok`.
        assert_eq!(
            grok_probe_root_resolved(&home.join("sessions"), home),
            Some(home.to_path_buf())
        );
        assert_eq!(
            grok_probe_root_resolved(Path::new("/tmp/fixture"), home),
            None
        );
        assert_eq!(
            grok_probe_root_resolved(Path::new("/tmp/other/sessions"), home),
            None
        );
    }

    #[cfg(unix)]
    mod registry_join {
        use super::*;

        fn alive_all(_pid: i32) -> bool {
            true
        }
        fn alive_none(_pid: i32) -> bool {
            false
        }

        const REG: &str = r#"[
            {"session_id":"0197-a","pid":100,"cwd":"/r/a","opened_at":"2026-07-16T12:00:05Z"},
            {"session_id":"0197-b","pid":200,"cwd":"/r/b","opened_at":"2026-07-16T12:01:00+00:00"}
        ]"#;

        /// `None` here would freeze the negative-vouch ledger on every machine
        /// that never ran grok.
        #[test]
        fn binder_absent_registry_is_a_healthy_empty_not_a_failure() {
            let dir = tempfile::tempdir().unwrap();
            let snap =
                live_grok_session_ids(dir.path()).expect("an absent registry is not a failure");
            assert!(snap.pid_of.is_empty());
        }

        #[test]
        fn binder_unreadable_registry_is_a_failure_not_an_empty() {
            let dir = tempfile::tempdir().unwrap();
            // A directory in its place makes `std::fs::read` fail with EISDIR on
            // both platforms; chmod 000 would not fail for root, which CI
            // sometimes is.
            std::fs::create_dir(dir.path().join("active_sessions.json")).unwrap();
            assert!(
                live_grok_session_ids(dir.path()).is_none(),
                "an unreadable registry must be None, never a healthy empty"
            );
        }

        /// The cap is a MEGAbyte, not a kilobyte: a registry far past any
        /// plausible kilobyte bound must still be read WHOLE, or a busy host's
        /// sessions silently stop vouching. Padding rides an ignored field so
        /// the document stays valid at every size.
        #[test]
        fn a_registry_well_past_a_kilobyte_is_read_whole() {
            let dir = tempfile::tempdir().unwrap();
            let me = std::process::id();
            let pad = "x".repeat(8 * 1024);
            std::fs::write(
                dir.path().join("active_sessions.json"),
                format!(r#"[{{"session_id":"big","pid":{me},"cwd":"/r","note":"{pad}"}}]"#),
            )
            .unwrap();
            let snap = live_grok_session_ids(dir.path()).expect("8 KiB is far under the 1 MiB cap");
            assert_eq!(snap.pid_of.get("big"), Some(&(me as i32)));
        }

        /// The drift breadcrumb must name the RIGHT cause: a truncation
        /// reported as upstream drift sends the reader hunting a format change
        /// that never happened.
        ///
        /// Rooted in `/tmp` deliberately. `shape_drift` caps its detail at
        /// `MAX_DECODED_FIELD_CHARS` (80) and the message spends 24 on its
        /// prefix, so the cause survives only while the PATH stays short — the
        /// default `$TMPDIR` on macOS is ~55 chars on its own and truncates the
        /// cause away entirely.
        #[test]
        fn an_unparseable_registry_reports_drift_not_truncation() {
            let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
            std::fs::write(dir.path().join("active_sessions.json"), "not json {{{").unwrap();
            let logs = crate::test_capture::capture_logs(|| {
                assert!(live_grok_session_ids(dir.path()).is_none());
            });
            assert!(logs.contains("does not parse"), "got:\n{logs}");
            assert!(
                !logs.contains("TRUNCATED"),
                "a 12-byte file cannot have hit the cap, got:\n{logs}"
            );
        }

        #[test]
        fn binder_reads_a_real_registry_and_binds_our_own_live_pid() {
            let dir = tempfile::tempdir().unwrap();
            let me = std::process::id();
            std::fs::write(
                dir.path().join("active_sessions.json"),
                format!(r#"[{{"session_id":"live-1","pid":{me},"cwd":"/r"}}]"#),
            )
            .unwrap();
            let snap = live_grok_session_ids(dir.path()).expect("a readable registry is healthy");
            assert_eq!(
                snap.pid_of.get("live-1"),
                Some(&(me as i32)),
                "a live registry entry must bind its id to its pid"
            );
        }

        #[test]
        fn live_entries_bind_ids_to_pids() {
            let snap = grok_ids_from_registry(REG.as_bytes(), alive_all, |_| None).unwrap();
            assert_eq!(snap.pid_of.get("0197-a"), Some(&100));
            assert_eq!(snap.pid_of.get("0197-b"), Some(&200));
        }

        #[test]
        fn dead_pids_and_junk_values_are_skipped_not_failures() {
            let snap = grok_ids_from_registry(REG.as_bytes(), alive_none, |_| None).unwrap();
            assert!(snap.pid_of.is_empty(), "dead pids yield a healthy empty");
            // Both halves of `checked_pid`'s narrowing — zero AND negative (a
            // negative pid would make `kill(-n, 0)` probe a process GROUP).
            let junk = r#"[{"session_id":"","pid":100,"cwd":"/r","opened_at":"x"},
                           {"session_id":"s","pid":0,"cwd":"/r","opened_at":"x"},
                           {"session_id":"neg","pid":-42,"cwd":"/r","opened_at":"x"},
                           {"session_id":"live","pid":77,"cwd":"/r","opened_at":"x"}]"#;
            let snap = grok_ids_from_registry(junk.as_bytes(), alive_all, |_| None).unwrap();
            assert_eq!(
                snap.pid_of.get("live"),
                Some(&77),
                "a junk entry must not cost its healthy siblings"
            );
            assert_eq!(snap.pid_of.len(), 1, "junk VALUES skip entries silently");
        }

        /// A pid outside `i32` range skips ITS OWN entry, exactly like every
        /// other junk value — it must not fail the whole document, because that
        /// path emits a `shape_drift` breadcrumb claiming the registry's SHAPE
        /// changed upstream and degrades all grok liveness to mtime (#831).
        ///
        /// The no-truncation assertion is the teeth: `4294967297 as i32` is
        /// `1`, so an implementation that narrows by cast instead of
        /// `checked_pid` would bind init's pid and vouch a session forever.
        #[test]
        fn out_of_i32_range_pid_skips_only_its_own_entry_and_never_truncates() {
            let over = r#"{"session_id":"over","pid":4294967297,"cwd":"/r"}"#;
            let ok = r#"{"session_id":"ok","pid":200,"cwd":"/r"}"#;
            // BOTH orders: today the document fails whichever side the bad
            // entry sits on, so ordering must not be what makes this pass.
            for reg in [format!("[{over},{ok}]"), format!("[{ok},{over}]")] {
                let snap = grok_ids_from_registry(reg.as_bytes(), alive_all, |_| None)
                    .expect("one out-of-range VALUE is not document-level shape drift");
                assert_eq!(snap.pid_of.get("ok"), Some(&200), "siblings still bind");
                assert_eq!(snap.pid_of.get("over"), None, "the bad entry is skipped");
                assert!(
                    !snap.pid_of.values().any(|&p| p == 1),
                    "narrowing must reject, never truncate ({over} casts to pid 1)"
                );
            }
        }

        #[test]
        fn unparseable_document_is_format_drift_none() {
            assert!(grok_ids_from_registry(b"not json", alive_all, |_| None).is_none());
            assert!(grok_ids_from_registry(br#"{"an":"object"}"#, alive_all, |_| None).is_none());
        }

        /// The negative control for the test above: widening `pid` to `i64` must
        /// not widen the entry's TYPE discipline. A string-typed or renamed pid
        /// is still document-level drift, because the derive's strictness IS the
        /// #247 upstream-rename detector — a later "simplify to `Vec<Value>`"
        /// refactor would pass every other test in this module while silently
        /// deleting it.
        #[test]
        fn a_mistyped_or_renamed_pid_is_still_document_level_drift() {
            for reg in [
                r#"[{"session_id":"s","pid":"100","cwd":"/r"}]"#,
                r#"[{"session_id":"s","pid":100.5,"cwd":"/r"}]"#,
                r#"[{"session_id":"s","processId":100,"cwd":"/r"}]"#,
            ] {
                assert!(
                    grok_ids_from_registry(reg.as_bytes(), alive_all, |_| None).is_none(),
                    "a mistyped/renamed pid key is SHAPE drift, not a junk value: {reg}"
                );
            }
        }

        #[test]
        fn recycled_pid_is_rejected_when_both_sides_agree_it_is() {
            let opened =
                crate::source::decoder::rfc3339_to_epoch_secs("2026-07-16T12:00:05Z").unwrap();
            let tolerance = crate::source::cc_probe::PID_START_TOLERANCE_SECS;
            let recycled_start = opened + tolerance + 1;
            let snap = grok_ids_from_registry(REG.as_bytes(), alive_all, |_| Some(recycled_start));
            assert_eq!(snap.unwrap().pid_of.get("0197-a"), None);
            // At exactly claim + tolerance the entry SURVIVES.
            let boundary_start = opened + tolerance;
            let snap = grok_ids_from_registry(REG.as_bytes(), alive_all, |_| Some(boundary_start));
            assert_eq!(snap.unwrap().pid_of.get("0197-a"), Some(&100));
            // A process started BEFORE the claim is the NORMAL welcome-screen
            // lag — never rejected.
            let snap = grok_ids_from_registry(REG.as_bytes(), alive_all, |_| Some(opened - 3600));
            assert_eq!(snap.unwrap().pid_of.get("0197-a"), Some(&100));
        }

        #[test]
        fn duplicate_session_id_keeps_the_larger_pid_in_both_orders() {
            for reg in [
                r#"[{"session_id":"s","pid":100,"cwd":"/r","opened_at":"2026-01-01T00:00:00Z"},
                    {"session_id":"s","pid":200,"cwd":"/r","opened_at":"2026-01-01T00:00:00Z"}]"#,
                r#"[{"session_id":"s","pid":200,"cwd":"/r","opened_at":"2026-01-01T00:00:00Z"},
                    {"session_id":"s","pid":100,"cwd":"/r","opened_at":"2026-01-01T00:00:00Z"}]"#,
            ] {
                let snap = grok_ids_from_registry(reg.as_bytes(), alive_all, |_| None).unwrap();
                assert_eq!(snap.pid_of.get("s"), Some(&200));
            }
        }

        /// grok's probe vouches ONLY what `active_sessions.json` binds: a
        /// present, LIVE `leader.sock` must not add a session the registry did
        /// not, however fresh that session's transcript is (#826).
        ///
        /// Differential — the same tree read twice, once with a real bound and
        /// listening `UnixListener` at the leader path — so it pins the
        /// OBSERVABLE decision rather than any mechanism, and still fails if a
        /// leader vouch is reintroduced by a different route. Proven red before
        /// the deletion by stubbing the socket owner to this process's pid;
        /// without that step it is a tautology over absent code.
        #[test]
        fn a_present_leader_socket_binds_nothing_the_registry_did_not() {
            let build = |with_socket: bool| {
                let tmp = tempfile::tempdir().unwrap();
                let me = std::process::id();
                std::fs::write(
                    tmp.path().join("active_sessions.json"),
                    format!(r#"[{{"session_id":"client-1","pid":{me},"cwd":"/r"}}]"#),
                )
                .unwrap();
                // A leader-kept session: no registry entry, transcript appended
                // just now — the population the deleted vouch targeted.
                let dir = tmp.path().join("sessions").join("%2Frepo").join("leader-1");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("updates.jsonl"), "{}\n").unwrap();
                let listener = with_socket.then(|| {
                    std::os::unix::net::UnixListener::bind(tmp.path().join("leader.sock"))
                        .expect("bind leader.sock")
                });
                let snap =
                    live_grok_session_ids(tmp.path()).expect("a readable registry is healthy");
                drop(listener);
                snap
            };
            let without = build(false);
            let with = build(true);
            assert_eq!(
                with.pid_of, without.pid_of,
                "a live leader.sock must not change what the probe vouches"
            );
            assert_eq!(
                with.pid_of.get("leader-1"),
                None,
                "a leader-kept session is deliberately unvouched (#826)"
            );
            assert!(
                with.pid_of.contains_key("client-1"),
                "control: the registry's own live entry still binds"
            );
        }
    }
}
