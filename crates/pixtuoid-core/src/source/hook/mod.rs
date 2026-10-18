use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tracing::{debug, warn};

use crate::AgentId;
use crate::source::decoder::{checked_pid, decode_hook_payload};
use crate::source::registry::SourceDescriptor;
use crate::source::{AgentEvent, TaggedSender, Transport};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(unix)]
pub(crate) use unix::{SOCKET_FILE_NAME, owned_socket_dir};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// One accepted hook connection, a Unix stream or a named-pipe instance, read
/// through [`MAX_CONN_BYTES`].
pub(crate) type HookConn = tokio::io::Take<imp::Conn>;

mod pid_watch;
pub(crate) use pid_watch::HookPidWatch;

mod router;
pub use router::HookRouter;

/// The variable naming the hook endpoint: [`super::claude_code::ClaudeCodeSource::default_socket_path`]
/// reads it, and so does the shim's parity-pinned twin (`tests/socket_path_parity.rs`).
pub(crate) const SOCKET_ENV: &str = "PIXTUOID_SOCKET";
pub(crate) const MAX_CONCURRENT_CONNS: usize = 128;
pub(crate) const CONN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Typed marker for "another live instance owns the hook endpoint" — bind's ONE
/// recoverable failure. `HookRouter::run` downcasts for it and degrades the HOOK
/// PLANE to a quiet `Ok(())` instead of dying, so a second instance takes only
/// that plane down while the transcript watchers keep running. Every other bind
/// error stays fatal → `SourceDeath`.
#[derive(Debug)]
pub struct SocketBusy {
    /// The socket/pipe path a live owner already holds.
    pub(crate) path: PathBuf,
}

impl std::fmt::Display for SocketBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "another pixtuoid instance is listening on {} — close it first, or run this one \
             with {SOCKET_ENV} pointing at a different path",
            self.path.display()
        )
    }
}

impl std::error::Error for SocketBusy {}

/// Listener for the shared hook socket (Unix domain socket / Windows named
/// pipe) every CLI's hook shim connects to. Frames + decodes each payload onto
/// the tagged `AgentEvent` channel; optionally attaches a pid-exit watch and the
/// daemon presence side-channel.
#[derive(Debug)]
pub struct HookSocketListener {
    inner: imp::Listener,
    path: PathBuf,
    pid_watch: Option<HookPidWatch>,
    presence_tx: Option<PresenceSender>,
}

pub(crate) type PresenceSender = crate::source::daemon::PresenceSender;

impl HookSocketListener {
    /// Bind the listener at `path`, returning [`SocketBusy`] if a live owner
    /// already holds it (the recoverable-bind case).
    ///
    /// # Errors
    ///
    /// If the socket directory is not an owner-only directory we own, or the lock or bind fails (a live owner yields [`SocketBusy`]).
    pub async fn bind(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let inner = imp::Listener::bind(&path).await?;
        Ok(Self {
            inner,
            path,
            pid_watch: None,
            presence_tx: None,
        })
    }

    /// The bound socket/pipe path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Attach a [`HookPidWatch`] so hook payloads carrying a `_pid` register a
    /// process-exit watch (instant `SessionEnd` on an abrupt CLI exit).
    pub(crate) fn with_pid_watch(mut self, pid_watch: Option<HookPidWatch>) -> Self {
        self.pid_watch = pid_watch;
        self
    }

    /// Attach the presence side-channel so daemon payloads decode to presence
    /// deltas (they produce no `AgentEvent`s).
    pub(crate) fn with_presence(mut self, presence_tx: Option<PresenceSender>) -> Self {
        self.presence_tx = presence_tx;
        self
    }

    /// Accept connections forever, decoding each hook payload onto `tx` (and, if
    /// wired, the pid-watch / presence side-channel).
    ///
    /// # Errors
    ///
    /// If the connection semaphore closes unexpectedly or, on Windows, the next pipe instance cannot be created.
    pub async fn run(self, tx: TaggedSender) -> Result<()> {
        let (pid_watch, presence_tx) = (self.pid_watch.clone(), self.presence_tx.clone());
        self.serve(move |conn| {
            handle_conn(conn, tx.clone(), pid_watch.clone(), presence_tx.clone())
        })
        .await
    }

    /// Accept connections forever, each handed to `handle` under the endpoint
    /// guards, [`CONN_TIMEOUT`] and [`MAX_CONN_BYTES`]: every handler's, so
    /// none can read past them.
    pub(crate) async fn serve<F, Fut>(self, handle: F) -> Result<()>
    where
        F: Fn(HookConn) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.inner
            .serve(move |conn| handle(conn.take(MAX_CONN_BYTES)))
            .await
    }
}

/// Per-connection byte ceiling: twice the shim's stamped-line quota (`STDIN_CAP` +
/// `STAMP_HEADROOM`). `lines()` buffers until a newline, so without this
/// an adversarial client could grow the buffer unboundedly for the whole
/// `CONN_TIMEOUT` window × [`MAX_CONCURRENT_CONNS`] slots.
pub(crate) const MAX_CONN_BYTES: u64 = 2 * 1024 * 1024;

/// Breadcrumb for the silent-loss path: the per-connection budget bounds
/// `handle_conn` by DROPPING its future, so no code after the cancelled await
/// ever runs and only a `Drop` impl can record that a payload's already-decoded
/// remainder never reached the reducer. Runtime shutdown reaches this same
/// `Drop`, so the message must not assert WHICH cancellation fired. Disarmed on
/// channel-closed (receiver gone = daemon shutdown): those events are dropped on
/// purpose, not lost.
struct UndeliveredEvents {
    ids: Vec<crate::AgentId>,
    delivered: usize,
}

impl UndeliveredEvents {
    fn new(evs: &[crate::source::AgentEvent]) -> Self {
        Self {
            ids: evs.iter().map(|ev| ev.agent_id()).collect(),
            delivered: 0,
        }
    }

    fn delivered_one(&mut self) {
        self.delivered += 1;
    }

    fn disarm(&mut self) {
        self.delivered = self.ids.len();
    }
}

impl Drop for UndeliveredEvents {
    fn drop(&mut self) {
        let undelivered = &self.ids[self.delivered..];
        if undelivered.is_empty() {
            return;
        }
        let mut agents: Vec<String> = undelivered.iter().map(ToString::to_string).collect();
        // One payload's ids are adjacent by construction, so no sort is needed.
        agents.dedup();
        warn!(
            events = undelivered.len(),
            agents = %agents.join(", "),
            budget = ?CONN_TIMEOUT,
            "hook connection cancelled mid-payload; decoded events never reached the reducer"
        );
    }
}

/// The agent a decoded event should bind to the connection's `_pid` (for the
/// abrupt-exit watch). BOTH `SessionStart` and `Identity` register-or-back-fill
/// a slot, and `Identity` is the only carrier for a session attached mid-turn
/// until its next `SessionStart`. Activity/Waiting/End never register a new
/// slot.
///
/// Dispatches on the same `FocusChannel` as [`patch_identity_pids`], which
/// reads a narrower fact off it: a `ServerStamp` pid ends sessions but is no
/// focus target. A `TranscriptProbe` source is excluded on its own terms: its
/// pid comes from the CLI's own registry via `jsonl::liveness`, which drives
/// its own exit watch, so binding here would only offer a second, weaker
/// answer. omp carries BOTH reapers and no precedence is enforced: on an abrupt
/// kill this watch usually ends the slot first, and the jsonl exit path's
/// trailing rows then land on an exiting slot as no-ops — accepted, a duplicate
/// `SessionEnd` is a documented reducer no-op.
fn pid_bind_target(ev: &AgentEvent) -> Option<(AgentId, bool)> {
    use crate::source::registry::FocusChannel;
    let (agent_id, source) = match ev {
        AgentEvent::SessionStart {
            agent_id, source, ..
        }
        | AgentEvent::Identity {
            agent_id, source, ..
        } => (agent_id, source),
        _ => return None,
    };
    match crate::source::registry::descriptor_for(source).map(SourceDescriptor::focus_channel) {
        // The shim GUESSED which ancestor is the CLI — corroborate it.
        Some(FocusChannel::ShimStamp) => Some((*agent_id, true)),
        // The source stamped its own `process.pid`; a session that never calls a
        // tool reports it once.
        Some(FocusChannel::PluginStamp | FocusChannel::ServerStamp) => Some((*agent_id, false)),
        _ => None,
    }
}

/// The batch's bind targets, at most one per agent: a payload must not
/// corroborate its own pid. omp pairs `SessionStart` + `Identity` in its
/// session batches (activity events are not bind targets), so the collapse is
/// load-bearing — pinned by `a_batch_yields_one_bind_target_per_agent` and
/// `an_omp_session_batch_pairs_start_and_identity_and_binds_once_trusted`.
fn bind_targets(evs: &[AgentEvent]) -> std::collections::BTreeMap<AgentId, bool> {
    evs.iter().filter_map(pid_bind_target).collect()
}

/// Stamp the connection's peeked `_pid` onto every `Identity` event of a decoded
/// batch — the per-source decoders never see the envelope key, so this single
/// patch point IS the whole focus-jump wiring.
///
/// The gate is the registry's [`FocusChannel`](crate::source::registry::FocusChannel) capability. `TranscriptProbe`
/// sources are skipped: the
/// shim's resolved pid is the nearest non-shell ancestor, not necessarily the CLI,
/// and a stamped stale pid would shadow the probe's recycle guard in
/// `resolve_pid`. The kernel start marker (#527, so the
/// click-time guard can refuse a recycled pid) is read LAZILY on the first
/// accepting Identity, sparing the high-volume non-accepting sources a syscall.
fn patch_identity_pids(evs: &mut [AgentEvent], pid: Option<i32>) {
    use crate::source::registry::FocusChannel;
    let Some(pid) = pid else { return };
    let mut stamp: Option<crate::source::PidIdentity> = None;
    for ev in evs {
        if let AgentEvent::Identity { source, pid: p, .. } = ev {
            let channel = crate::source::registry::descriptor_for(source)
                .map_or(FocusChannel::Unsupported, |d| d.focus_channel());
            if channel.stamps_focus_pid() {
                *p = Some(*stamp.get_or_insert_with(|| crate::source::PidIdentity {
                    pid,
                    started: crate::source::pid_start_marker(pid),
                }));
            }
        }
    }
}

pub(crate) async fn handle_conn(
    stream: tokio::io::Take<impl AsyncRead + Unpin>,
    tx: TaggedSender,
    pid_watch: Option<HookPidWatch>,
    presence_tx: Option<PresenceSender>,
) {
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(error = %e, "malformed hook line skipped");
                        continue;
                    }
                };
                // DAEMON demux, registry-DRIVEN so a 2nd daemon needs NO edit
                // here: `presence_decoder_for` returns `None` for every agent
                // source, and a daemon's payloads decode to presence deltas on
                // the sibling channel instead of `AgentEvent`s.
                if let (Some(ptx), Some(src)) = (
                    presence_tx.as_ref(),
                    v.get("_pixtuoid_source")
                        .and_then(serde_json::Value::as_str),
                ) && let Some(decode) = crate::source::registry::presence_decoder_for(src)
                {
                    match decode(&v) {
                        Ok(decoded) => {
                            let key = crate::source::daemon::DaemonInstanceKey::new(
                                src,
                                decoded.instance,
                            );
                            for u in decoded.updates {
                                let _ = ptx.send(crate::source::daemon::PresenceMsg {
                                    key: key.clone(),
                                    delta: u,
                                });
                            }
                        }
                        Err(e) => warn!(error = %e, "daemon presence decode error"),
                    }
                    // A daemon produces no AgentEvents — never the agent arms.
                    continue;
                }
                // Peek the shim-supplied CLI pid BEFORE `v` is consumed by
                // decode. Deliberately NOT gated on `pid_watch`: a platform with
                // no exit-watch backend, or one that failed to init, must not
                // take the focus-jump pid cache down with it — only the BIND
                // below needs the watch.
                let pid = v
                    .get("_pid")
                    .and_then(serde_json::Value::as_i64)
                    .and_then(checked_pid);
                match decode_hook_payload(v) {
                    // One payload can decode to multiple events (#221) — sent in
                    // order on the same channel, so the reducer registers with
                    // real identity before the activity event applies.
                    Ok(evs) => {
                        if let (Some(pid), Some(watch)) = (pid, pid_watch.as_ref()) {
                            for (agent_id, corroborate) in bind_targets(&evs) {
                                watch.note(pid, agent_id, corroborate);
                            }
                        }
                        let mut evs = evs;
                        patch_identity_pids(&mut evs, pid);
                        let mut undelivered = UndeliveredEvents::new(&evs);
                        for ev in evs {
                            debug!(?ev, "hook event");
                            if tx.send((Transport::Hook, ev)).await.is_err() {
                                undelivered.disarm();
                                return;
                            }
                            undelivered.delivered_one();
                        }
                    }
                    Err(e) => warn!(error = %e, "hook decode error"),
                }
            }
            Ok(None) => return,
            Err(e) => {
                warn!(error = %e, "hook conn read error");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentId;
    use crate::source::AgentEvent;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn patch_identity_pids_stamps_only_identity_events() {
        let id = AgentId::from_parts("omp", "ses_x");
        let mut evs = vec![
            AgentEvent::Identity {
                agent_id: id,
                source: "omp".into(),
                session_id: "ses_x".into(),
                cwd: None,
                pid: None,
            },
            AgentEvent::ActivityStart {
                agent_id: id,
                tool_use_id: None,
                detail: None,
            },
        ];
        patch_identity_pids(&mut evs, Some(i32::MAX));
        assert!(
            matches!(evs[0], AgentEvent::Identity { pid: Some(p), .. } if p.pid == i32::MAX && p.started.is_none()),
            "Identity stamped"
        );
        patch_identity_pids(&mut evs, None);
        assert!(
            matches!(evs[0], AgentEvent::Identity { pid: Some(p), .. } if p.pid == i32::MAX && p.started.is_none())
        );
    }

    #[test]
    fn patch_identity_pids_stamps_only_a_focus_target() {
        for source in ["claude-code", "codex", "antigravity", "copilot", "opencode"] {
            let mut evs = vec![AgentEvent::Identity {
                agent_id: AgentId::from_parts(source, "ses_t"),
                source: source.into(),
                session_id: "ses_t".into(),
                cwd: None,
                pid: None,
            }];
            patch_identity_pids(&mut evs, Some(i32::MAX));
            assert!(
                matches!(evs[0], AgentEvent::Identity { pid: None, .. }),
                "{source} Identity must stay pid: None"
            );
        }
        for source in ["cursor", "codewhale", "hermes", "reasonix", "omp"] {
            let mut evs = vec![AgentEvent::Identity {
                agent_id: AgentId::from_parts(source, "ses_t"),
                source: source.into(),
                session_id: "ses_t".into(),
                cwd: None,
                pid: None,
            }];
            patch_identity_pids(&mut evs, Some(i32::MAX));
            assert!(
                matches!(evs[0], AgentEvent::Identity { pid: Some(p), .. } if p.pid == i32::MAX && p.started.is_none()),
                "{source} Identity must be stamped"
            );
        }
    }

    /// The guard `handle_conn` relies on: a batch yields at most ONE sighting per
    /// agent, so a payload can never corroborate its own pid. Pinned directly
    /// (the handle_conn-level test would pass with the dedupe deleted); the
    /// live pin is `an_omp_session_batch_pairs_start_and_identity_and_binds_once_trusted`.
    #[test]
    fn a_batch_yields_one_bind_target_per_agent() {
        let id = AgentId::from_parts("cursor", "sess-A");
        let batch = [
            AgentEvent::SessionStart {
                agent_id: id,
                source: "cursor".into(),
                session_id: "sess-A".into(),
                cwd: "/r".into(),
                parent_id: None,
            },
            AgentEvent::Identity {
                agent_id: id,
                source: "cursor".into(),
                session_id: "sess-A".into(),
                cwd: None,
                pid: None,
            },
        ];
        assert_eq!(
            bind_targets(&batch).len(),
            1,
            "two bind-target events for one agent must collapse to one sighting"
        );
        assert_eq!(
            bind_targets(&batch).get(&id),
            Some(&true),
            "cursor is ShimStamp — its pid is a guess and needs corroborating"
        );
    }

    /// omp is the first decoder that really pairs `SessionStart` + `Identity`
    /// in one batch, so the collapse the guard above pinned synthetically is
    /// now load-bearing — and a PluginStamp pid needs no corroboration.
    #[test]
    fn an_omp_session_batch_pairs_start_and_identity_and_binds_once_trusted() {
        let evs = crate::source::omp::decode_omp_hook_payload(&serde_json::json!({
            "type": "session_start",
            "sessionFile": "/h/.omp/agent/sessions/-repo/2026-08-30T01-00-00-000Z_01a00000-0000-7000-8000-000000000001.jsonl",
            "sessionId": "01a00000-0000-7000-8000-000000000001",
            "cwd": "/repo",
        }))
        .unwrap();
        assert!(
            matches!(
                &evs[..],
                [AgentEvent::SessionStart { .. }, AgentEvent::Identity { .. }]
            ),
            "the pair this test exists for: {evs:?}"
        );
        let targets = bind_targets(&evs);
        assert_eq!(targets.len(), 1, "the pair must collapse to one sighting");
        assert_eq!(
            targets.values().next(),
            Some(&false),
            "a PluginStamp pid is the CLI's own — no corroboration"
        );

        // The switch batch: End(prev) is no bind target, and the current id's
        // Start+Identity pair still collapses to one trusted sighting.
        let evs = crate::source::omp::decode_omp_hook_payload(&serde_json::json!({
            "type": "session_switch",
            "sessionFile": "/h/.omp/agent/sessions/-repo/2026-08-30T01-00-00-000Z_01a00000-0000-7000-8000-000000000002.jsonl",
            "previousSessionFile": "/h/.omp/agent/sessions/-repo/2026-08-30T01-00-00-000Z_01a00000-0000-7000-8000-000000000001.jsonl",
            "sessionId": "01a00000-0000-7000-8000-000000000002",
            "cwd": "/repo",
        }))
        .unwrap();
        assert_eq!(
            evs.len(),
            3,
            "End(prev) + Start(cur) + Identity(cur): {evs:?}"
        );
        let targets = bind_targets(&evs);
        assert_eq!(
            (targets.len(), targets.values().next()),
            (1, Some(&false)),
            "one trusted sighting for the current id, none for the ended one"
        );
    }

    #[test]
    fn pid_bind_target_skips_a_source_whose_stamp_is_untrusted() {
        for source in ["claude-code", "codex"] {
            let id = AgentId::from_parts(source, "s");
            let ev = AgentEvent::SessionStart {
                agent_id: id,
                source: source.into(),
                session_id: "s".into(),
                cwd: "/r".into(),
                parent_id: None,
            };
            assert_eq!(
                pid_bind_target(&ev),
                None,
                "{source} declares its shim stamp untrusted; it must not bind"
            );
        }
    }

    #[test]
    fn pid_bind_target_covers_both_registration_carriers() {
        let id = AgentId::from_parts("opencode", "ses_x");
        for ev in [
            AgentEvent::SessionStart {
                agent_id: id,
                source: "opencode".into(),
                session_id: "ses_x".into(),
                cwd: "/r".into(),
                parent_id: None,
            },
            AgentEvent::Identity {
                agent_id: id,
                source: "opencode".into(),
                session_id: "ses_x".into(),
                cwd: None,
                pid: None,
            },
        ] {
            assert_eq!(
                pid_bind_target(&ev),
                Some((id, false)),
                "{ev:?} must bind the pid; opencode's server stamps its own, so no corroboration"
            );
        }
        for ev in [
            AgentEvent::ActivityStart {
                agent_id: id,
                tool_use_id: None,
                detail: None,
            },
            AgentEvent::ActivityEnd {
                agent_id: id,
                tool_use_id: None,
            },
            AgentEvent::Waiting {
                agent_id: id,
                reason: "x".into(),
                tool_use_id: None,
            },
            AgentEvent::SessionEnd {
                agent_id: id,
                as_child: false,
            },
        ] {
            assert_eq!(pid_bind_target(&ev), None, "{ev:?} must not bind the pid");
        }
    }

    use crate::test_capture::capture_warns;

    // Decodes to TWO events (Identity + ActivityStart) — the shape the
    // mid-payload loss path needs: more than one event per payload.
    const PRE_TOOL_USE_LINE: &[u8] = b"{\"hook_event_name\":\"PreToolUse\",\
        \"session_id\":\"ses-abc\",\
        \"transcript_path\":\"/p/ses-abc.jsonl\",\
        \"cwd\":\"/repo\",\
        \"tool_name\":\"Bash\",\
        \"tool_input\":{\"command\":\"ls\"},\
        \"tool_use_id\":\"t1\"}\n";

    #[tokio::test]
    async fn handle_conn_decodes_one_line_via_duplex() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);

        let task = tokio::spawn(handle_conn(server.take(MAX_CONN_BYTES), tx, None, None));
        client
            .write_all(
                b"{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s1\",\
                  \"transcript_path\":\"/Users/me/.claude/projects/x/s1.jsonl\"}\n",
            )
            .await
            .unwrap();
        drop(client);

        let (transport, ev) = rx.recv().await.expect("one decoded event");
        assert_eq!(transport, Transport::Hook);
        assert!(matches!(ev, AgentEvent::SessionStart { .. }));
        task.await.unwrap();
    }

    /// The writer is SPAWNED because the payload is ~20× the duplex buffer, so
    /// `write_all` only completes as `handle_conn` drains it: written inline,
    /// anything that stops the drain parks the await forever and the test fails
    /// by HANGING instead of asserting.
    #[tokio::test]
    async fn max_conn_bytes_leaves_headroom_over_the_shim_line_cap() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let task = tokio::spawn(handle_conn(server.take(MAX_CONN_BYTES), tx, None, None));
        let pad = "x".repeat((1 << 20) + (1 << 18));
        let line = format!(
            "{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s1\",\
             \"transcript_path\":\"/p/s1.jsonl\",\"pad\":\"{pad}\"}}\n"
        );
        let writer = tokio::spawn(async move {
            let _ = client.write_all(line.as_bytes()).await;
            drop(client);
        });
        let (transport, ev) = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("a 1.25 MiB line must decode well inside the 2 MiB conn ceiling")
            .expect("one decoded event");
        assert_eq!(transport, Transport::Hook);
        assert!(matches!(ev, AgentEvent::SessionStart { .. }));
        writer.abort();
        task.abort();
    }

    #[tokio::test]
    async fn handle_conn_routes_openclaw_presence_to_the_side_channel_only() {
        use crate::source::daemon::DaemonPresenceUpdate;
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let (ptx, mut prx) =
            tokio::sync::mpsc::unbounded_channel::<crate::source::daemon::PresenceMsg>();

        let task = tokio::spawn(handle_conn(
            server.take(MAX_CONN_BYTES),
            tx,
            None,
            Some(ptx),
        ));
        client
            .write_all(
                b"{\"_pixtuoid_source\":\"openclaw\",\"type\":\"gateway_start\",\
                  \"gatewayPort\":18789,\"_pid\":4242}\n",
            )
            .await
            .unwrap();
        client
            .write_all(
                b"{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s1\",\
                  \"transcript_path\":\"/Users/me/.claude/projects/x/s1.jsonl\"}\n",
            )
            .await
            .unwrap();
        drop(client);

        let msg = prx.recv().await.expect("one presence update");
        assert_eq!(
            msg.key.source(),
            "openclaw",
            "presence is source-tagged for N-daemon routing"
        );
        assert_eq!(
            msg.key.instance().as_str(),
            "18789",
            "the demux carries the decoder's gateway identity, so N GATEWAYS route too"
        );
        assert!(matches!(
            msg.delta,
            DaemonPresenceUpdate::GatewayUp { pid: Some(4242) }
        ));
        assert!(
            prx.try_recv().is_err(),
            "the CC line must not reach presence"
        );

        let (transport, ev) = rx.recv().await.expect("one agent event");
        assert_eq!(transport, Transport::Hook);
        assert!(matches!(ev, AgentEvent::SessionStart { .. }));
        assert!(
            rx.try_recv().is_err(),
            "the openclaw line emits no AgentEvent"
        );
        task.await.unwrap();
    }

    // The INVERSE of the routing test above: it FAILS the moment an agent source
    // gains a presence decoder (a dual-natured source), the silent cross-route
    // no other test catches.
    #[tokio::test]
    async fn handle_conn_agent_source_with_pid_never_routes_to_presence() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let (ptx, mut prx) =
            tokio::sync::mpsc::unbounded_channel::<crate::source::daemon::PresenceMsg>();

        // Spawn with the presence channel ACTIVE so the demux block is entered.
        let task = tokio::spawn(handle_conn(
            server.take(MAX_CONN_BYTES),
            tx,
            None,
            Some(ptx),
        ));
        client
            .write_all(
                b"{\"_pixtuoid_source\":\"opencode\",\"type\":\"session.created\",\
                  \"data\":{\"sessionID\":\"ses_neg\",\"location\":{\"directory\":\"/repo\"}},\
                  \"_pid\":5555}\n",
            )
            .await
            .unwrap();
        drop(client);

        let (transport, ev) = rx.recv().await.expect("agent event arrived");
        assert_eq!(transport, Transport::Hook);
        assert!(
            matches!(ev, AgentEvent::SessionStart { .. }),
            "opencode session.created decodes to SessionStart, got {ev:?}"
        );
        assert!(
            prx.try_recv().is_err(),
            "an agent-source payload with _pid must NOT route to presence"
        );
        task.await.unwrap();
    }

    // Deterministic by construction: nothing drains the capacity-1 channel, so
    // the second send can NEVER complete and the timeout always cancels
    // mid-payload regardless of scheduling.
    #[tokio::test]
    async fn cancelled_conn_leaves_breadcrumb_for_undelivered_events() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(1);
        let (logs, _guard) = capture_warns();

        client.write_all(PRE_TOOL_USE_LINE).await.unwrap();

        let timed_out = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            handle_conn(server.take(MAX_CONN_BYTES), tx, None, None),
        )
        .await
        .is_err();
        assert!(timed_out, "second send parks on the full channel forever");

        let (_, ev) = rx.try_recv().expect("first decoded event delivered");
        assert!(matches!(ev, AgentEvent::Identity { .. }));
        assert!(
            rx.try_recv().is_err(),
            "second event must have been dropped"
        );

        let out = logs.contents();
        assert!(
            out.contains("events=1"),
            "loss breadcrumb missing from logs: {out:?}"
        );
        assert!(
            out.contains("cancelled mid-payload"),
            "breadcrumb must report a cancellation, not assert a cause: {out:?}"
        );
        let expected = AgentId::from_parts("claude-code", "ses-abc");
        assert!(
            out.contains(&expected.to_string()),
            "breadcrumb must attribute the loss to its agent id: {out:?}"
        );
    }

    #[tokio::test]
    async fn clean_delivery_leaves_no_breadcrumb() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let (logs, _guard) = capture_warns();

        client.write_all(PRE_TOOL_USE_LINE).await.unwrap();
        drop(client);
        handle_conn(server.take(MAX_CONN_BYTES), tx, None, None).await;

        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_ok());
        let out = logs.contents();
        assert!(
            !out.contains("cancelled mid-payload"),
            "fully delivered payload must not warn: {out:?}"
        );
    }

    /// Both `rx.recv()`s are BOUNDED because `HookPidWatch::spawn` holds a `tx`
    /// clone, so this channel never closes on its own: an unbounded recv parks
    /// forever whenever the payload decodes to nothing, and nextest cannot
    /// finish cancelling while one test hangs.
    #[tokio::test]
    async fn handle_conn_binds_pid_from_payload_so_killing_it_ends_the_agent() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let Some(watch) = HookPidWatch::spawn(tx.clone()) else {
            return; // no exit-watch backend on this platform — nothing to assert
        };
        let mut child = crate::test_child::sleeper();
        let pid = child.id();

        let (mut client, server) = tokio::io::duplex(4096);
        let task = tokio::spawn(handle_conn(
            server.take(MAX_CONN_BYTES),
            tx,
            Some(watch),
            None,
        ));
        // TWO payloads: the CLI's own pid rides every hook event, and that
        // repeat is what arms the watch (#896).
        let line = format!(
            "{{\"_pixtuoid_source\":\"codewhale\",\"event\":\"session_start\",\
             \"cwd\":\"/repo\",\"_pid\":{pid}}}\n\
             {{\"_pixtuoid_source\":\"codewhale\",\"event\":\"tool_call_before\",\
             \"cwd\":\"/repo\",\"tool\":\"read_file\",\"_pid\":{pid}}}\n"
        );
        client.write_all(line.as_bytes()).await.unwrap();

        drop(client);
        task.await.unwrap();

        child.kill().expect("kill the watched child");
        let _ = child.wait();
        let expected = AgentId::from_parts("codewhale", "/repo");
        let end = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            // The decoded payloads land first; the pid's death arrives behind them.
            loop {
                match rx.recv().await {
                    Some((transport, AgentEvent::SessionEnd { agent_id, as_child })) => {
                        return Some((transport, agent_id, as_child));
                    }
                    Some(_) => continue,
                    None => return None,
                }
            }
        })
        .await
        .expect("a SessionEnd within 5s of the watched pid dying")
        .expect("channel still open");
        assert_eq!(
            end,
            (Transport::Hook, expected, false),
            "the payload-bound agent must end when its pid dies"
        );
    }

    /// The #896 shape at this seam: ONE payload carrying a `_pid`, then that pid
    /// dies. A single sighting of a shim-guessed pid must end nothing. (The
    /// batch-dedupe guard itself is pinned by `a_batch_yields_one_bind_target_per_agent`
    /// and exercised for real by omp's paired session batches —
    /// `an_omp_session_batch_pairs_start_and_identity_and_binds_once_trusted`.)
    #[tokio::test]
    async fn handle_conn_a_single_sighting_dying_ends_nothing() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let Some(watch) = HookPidWatch::spawn(tx.clone()) else {
            return; // no exit-watch backend on this platform — nothing to assert
        };
        let mut wrapper = crate::test_child::sleeper();
        let pid = wrapper.id();

        let (mut client, server) = tokio::io::duplex(4096);
        let task = tokio::spawn(handle_conn(
            server.take(MAX_CONN_BYTES),
            tx,
            Some(watch),
            None,
        ));
        let line = format!(
            "{{\"_pixtuoid_source\":\"cursor\",\"hook_event_name\":\"preToolUse\",\
             \"session_id\":\"sess-A\",\"workspace_roots\":[\"/repo\"],\
             \"tool_name\":\"Read\",\"_pid\":{pid}}}\n"
        );
        client.write_all(line.as_bytes()).await.unwrap();
        drop(client);
        task.await.unwrap();

        wrapper.kill().expect("kill the wrapper");
        let _ = wrapper.wait();
        let ends = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some((_, ev)) = rx.recv().await {
                if matches!(ev, AgentEvent::SessionEnd { .. }) {
                    return true;
                }
            }
            false
        })
        .await;
        assert!(
            matches!(ends, Err(_) | Ok(false)),
            "a single payload's pid must not end the session when it dies"
        );
    }

    #[tokio::test]
    async fn handle_conn_never_stamps_a_transcript_family_identity_pid() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let (mut client, server) = tokio::io::duplex(4096);
        let task = tokio::spawn(handle_conn(server.take(MAX_CONN_BYTES), tx, None, None));
        let me = std::process::id();
        let line = format!(
            "{{\"hook_event_name\":\"PreToolUse\",\"session_id\":\"ses-cc\",\
             \"transcript_path\":\"/p/a.jsonl\",\"cwd\":\"/repo\",\
             \"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"_pid\":{me}}}\n"
        );
        client.write_all(line.as_bytes()).await.unwrap();
        drop(client);
        task.await.unwrap();

        let (_, ev) = rx
            .recv()
            .await
            .expect("the Identity ahead of the tool event");
        assert!(
            matches!(&ev, AgentEvent::Identity { source, pid: None, .. } if source == "claude-code"),
            "CC Identity must stay pid: None through the socket loop, got {ev:?}"
        );
        let (_, ev) = rx.recv().await.expect("the paired ActivityStart");
        assert!(matches!(ev, AgentEvent::ActivityStart { .. }), "got {ev:?}");
    }

    #[tokio::test]
    async fn handle_conn_stamps_identity_pid_even_without_a_pid_watch() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let (mut client, server) = tokio::io::duplex(4096);
        let task = tokio::spawn(handle_conn(server.take(MAX_CONN_BYTES), tx, None, None));
        // i32::MAX as the pid: the stamp path does a REAL kernel marker read,
        // and an allocatable pid can be a live process on a busy CI host — an
        // unallocatable one guarantees `started: None`.
        let line = format!(
            "{{\"_pixtuoid_source\":\"codewhale\",\"event\":\"tool_call_before\",\
             \"cwd\":\"/repo\",\"tool\":\"exec_shell\",\"_pid\":{}}}\n",
            i32::MAX
        );
        client.write_all(line.as_bytes()).await.unwrap();
        drop(client);
        task.await.unwrap();

        let (_, ev) = rx
            .recv()
            .await
            .expect("the Identity ahead of the tool event");
        assert!(
            matches!(&ev, AgentEvent::Identity { source, pid: Some(p), .. }
                if source == "codewhale" && p.pid == i32::MAX && p.started.is_none()),
            "hook-family Identity must carry the peeked pid without a watch \
             (and no start marker for a pid that does not exist), got {ev:?}"
        );
    }

    #[tokio::test]
    async fn handle_conn_skips_valid_json_with_unsupported_event_and_emits_nothing() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let (logs, _guard) = capture_warns();

        let task = tokio::spawn(handle_conn(server.take(MAX_CONN_BYTES), tx, None, None));
        client
            .write_all(b"{\"hook_event_name\":\"BogusEvent\",\"session_id\":\"s1\"}\n")
            .await
            .unwrap();
        client
            .write_all(
                b"{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s2\",\
                  \"transcript_path\":\"/Users/me/.claude/projects/x/s2.jsonl\"}\n",
            )
            .await
            .unwrap();
        drop(client);
        task.await.unwrap();

        let (_, ev) = rx.try_recv().expect("the valid SessionStart");
        assert!(matches!(ev, AgentEvent::SessionStart { .. }), "got {ev:?}");
        assert!(
            rx.try_recv().is_err(),
            "the unsupported-event line must emit no AgentEvent"
        );
        assert!(
            logs.contents().contains("hook decode error"),
            "the bail must log a decode error: {:?}",
            logs.contents()
        );
    }

    #[tokio::test]
    async fn handle_conn_silently_skips_blank_lines_without_a_malformed_warning() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let (logs, _guard) = capture_warns();

        let task = tokio::spawn(handle_conn(server.take(MAX_CONN_BYTES), tx, None, None));
        client.write_all(b"   \n").await.unwrap();
        client
            .write_all(
                b"{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s1\",\
                  \"transcript_path\":\"/Users/me/.claude/projects/x/s1.jsonl\"}\n",
            )
            .await
            .unwrap();
        drop(client);
        task.await.unwrap();

        let (_, ev) = rx.try_recv().expect("the valid SessionStart");
        assert!(matches!(ev, AgentEvent::SessionStart { .. }), "got {ev:?}");
        assert!(
            rx.try_recv().is_err(),
            "only the SessionStart, the blank line produces nothing"
        );
        assert!(
            !logs.contents().contains("malformed hook line skipped"),
            "a blank line must skip before from_str — no malformed warning: {:?}",
            logs.contents()
        );
    }

    #[tokio::test]
    async fn handle_conn_malformed_openclaw_presence_continues_without_falling_through() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let (ptx, mut prx) =
            tokio::sync::mpsc::unbounded_channel::<crate::source::daemon::PresenceMsg>();

        let task = tokio::spawn(handle_conn(
            server.take(MAX_CONN_BYTES),
            tx,
            None,
            Some(ptx),
        ));
        // An object without `type` — the openclaw decoder errs on it.
        client
            .write_all(b"{\"_pixtuoid_source\":\"openclaw\"}\n")
            .await
            .unwrap();
        client
            .write_all(
                b"{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s1\",\
                  \"transcript_path\":\"/Users/me/.claude/projects/x/s1.jsonl\"}\n",
            )
            .await
            .unwrap();
        drop(client);
        task.await.unwrap();

        assert!(
            prx.try_recv().is_err(),
            "a daemon decode failure must emit no presence message"
        );
        let (transport, ev) = rx.try_recv().expect("the valid CC SessionStart");
        assert_eq!(transport, Transport::Hook);
        assert!(matches!(ev, AgentEvent::SessionStart { .. }), "got {ev:?}");
        assert!(
            rx.try_recv().is_err(),
            "the malformed openclaw line emits no AgentEvent"
        );
    }

    #[tokio::test]
    async fn channel_closed_shutdown_leaves_no_breadcrumb() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        drop(rx); // receiver gone = daemon shutdown
        let (logs, _guard) = capture_warns();

        client.write_all(PRE_TOOL_USE_LINE).await.unwrap();
        handle_conn(server.take(MAX_CONN_BYTES), tx, None, None).await;

        let out = logs.contents();
        assert!(
            !out.contains("cancelled mid-payload"),
            "shutdown drop is deliberate, not a loss: {out:?}"
        );
    }
}
