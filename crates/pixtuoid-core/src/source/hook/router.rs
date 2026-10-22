//! `HookRouter` — the single owner of the ONE shared hook socket that EVERY
//! source's hooks ride (Codex, Reasonix, CodeWhale, opencode, Cursor, and the
//! OpenClaw daemon).
//!
//! It implements [`Source`] DELIBERATELY, to inherit `spawn_with_health`'s
//! `SourceDeath` path for free instead of rebuilding it. It is still
//! INFRASTRUCTURE, not a CLI: it has NO registry/descriptor/badge row, and the
//! add-a-CLI checklist does not apply.

use std::path::PathBuf;

use anyhow::Result;

use crate::source::jsonl::ChildEndUnclaims;
use crate::source::{AgentEvent, Source, TaggedReceiver, TaggedSender, Transport};

use super::{HookPidWatch, HookSocketListener, PresenceSender, SocketBusy};

/// Infrastructure name — NOT a registered source (no descriptor/badge). Names
/// the footer attribution `spawn_with_health` publishes for a fatal bind error.
pub(crate) const SOURCE_NAME: &str = "hook-router";

/// Producer half of the #246 child-end un-claim side-channel (see
/// `ChildEndUnclaims` for the WHY): the ONE seam every decoded `SubagentStop`
/// passes through, since all sources' hooks ride the one shared socket. Every
/// event is forwarded UNCHANGED, transport tag included (invariant #2). Pushing
/// BEFORE the forward is not required for correctness, but it keeps the
/// un-claim pending by the time the reducer applies the end, so tests stay
/// deterministic.
pub(crate) async fn tee_child_end_unclaims(
    mut rx: TaggedReceiver,
    tx: TaggedSender,
    unclaims: ChildEndUnclaims,
) {
    while let Some((transport, ev)) = rx.recv().await {
        if transport == Transport::Hook
            && let AgentEvent::SessionEnd {
                agent_id,
                as_child: true,
            } = &ev
        {
            unclaims.push(*agent_id);
        }
        if tx.send((transport, ev)).await.is_err() {
            return;
        }
    }
}

/// The shared hook-socket owner: binds the ONE socket, runs the per-connection
/// decode loop, interposes the #246 tee, and feeds the daemon-presence side
/// channel.
#[derive(Debug)]
pub struct HookRouter {
    socket_path: PathBuf,
    /// The #246 child-end un-claim PRODUCER handle; the runtime shares ONE with
    /// the CC + Codex watchers (the CONSUMERS). `None` disables the tee.
    child_end_unclaims: Option<ChildEndUnclaims>,
    /// The daemon-presence side channel (the gateway mascots): daemon payloads
    /// decode to presence deltas, not `AgentEvent`s. `None` disables it.
    presence_tx: Option<PresenceSender>,
}

impl HookRouter {
    /// Construct a router bound to `socket_path`, with both side channels
    /// disabled (attach them via the builder methods).
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            child_end_unclaims: None,
            presence_tx: None,
        }
    }

    /// Wire the #246 child-end un-claim producer.
    #[must_use]
    pub fn with_child_end_unclaims(mut self, unclaims: Option<ChildEndUnclaims>) -> Self {
        self.child_end_unclaims = unclaims;
        self
    }

    /// Wire the daemon-presence side channel.
    #[must_use]
    pub fn with_presence_tx(mut self, presence_tx: Option<PresenceSender>) -> Self {
        self.presence_tx = presence_tx;
        self
    }
}

impl Source for HookRouter {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        // SocketBusy (another live instance owns the endpoint) takes ONLY the
        // hook plane down: the hooks belong to the owning instance, so return
        // Ok(()) with no `SourceDeath`. Every other bind error is fatal.
        let socket = match HookSocketListener::bind(self.socket_path.clone()).await {
            Ok(s) => s,
            Err(e) if e.downcast_ref::<SocketBusy>().is_some() => {
                let chain = format!("{e:#}");
                tracing::warn!(
                    error = ?chain,
                    "hook plane disabled — hook-borne signals (permission Waiting, instant \
                     lifecycle, daemon presence) belong to the owning instance; transcript \
                     sources keep running"
                );
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let tx_hook = match &self.child_end_unclaims {
            Some(unclaims) => {
                let (tee_tx, tee_rx) =
                    tokio::sync::mpsc::channel(crate::source::EVENT_CHANNEL_CAPACITY);
                tokio::spawn(tee_child_end_unclaims(tee_rx, tx.clone(), unclaims.clone()));
                tee_tx
            }
            None => tx.clone(),
        };
        // The pid-watch's synthesized SessionEnd goes on the main `tx`, not
        // `tx_hook`: it is `as_child: false`, which the #246 tee ignores anyway.
        let socket = socket
            .with_pid_watch(HookPidWatch::spawn(tx.clone()))
            .with_presence(self.presence_tx.clone());
        socket.run(tx_hook).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentId;
    use std::time::Duration;

    #[tokio::test]
    async fn tee_pushes_hook_child_ends_and_forwards_every_event_unchanged() {
        let unclaims = ChildEndUnclaims::new();
        let (in_tx, in_rx) = tokio::sync::mpsc::channel(16);
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(16);
        let tee = tokio::spawn(tee_child_end_unclaims(in_rx, out_tx, unclaims.clone()));

        let child = AgentId::from_parts("codex", "child-uuid");
        let root = AgentId::from_parts(crate::source::claude_code::SOURCE_NAME, "root-uuid");
        let events: Vec<(Transport, AgentEvent)> = vec![
            (
                Transport::Hook,
                AgentEvent::ActivityStart {
                    agent_id: root,
                    tool_use_id: Some("tu_1".into()),
                    detail: None,
                },
            ),
            // Copilot's transcript decode really does emit Jsonl-tagged child
            // ends (`subagent.completed`/`failed`), so the transport guard is
            // the boundary, not mere defensiveness.
            (
                Transport::Jsonl,
                AgentEvent::SessionEnd {
                    agent_id: root,
                    as_child: true,
                },
            ),
            (
                Transport::Hook,
                AgentEvent::SessionEnd {
                    agent_id: root,
                    as_child: false,
                },
            ),
            (
                Transport::Hook,
                AgentEvent::SessionEnd {
                    agent_id: child,
                    as_child: true,
                },
            ),
        ];
        for ev in &events {
            in_tx.send(ev.clone()).await.unwrap();
        }
        for expected in &events {
            let got = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
                .await
                .expect("tee must forward promptly")
                .expect("tee must not drop the channel");
            assert_eq!(
                &got, expected,
                "event parity: forwarded unchanged, in order"
            );
        }
        assert_eq!(
            unclaims.take_matching(|_| true),
            vec![child],
            "exactly the Hook-transport as_child end lands in the handle"
        );
        drop(in_tx);
        tokio::time::timeout(Duration::from_secs(5), tee)
            .await
            .expect("tee exits when the listener side closes")
            .unwrap();
    }

    #[test]
    fn router_name_is_the_infrastructure_source_name() {
        assert_eq!(
            HookRouter::new(PathBuf::from("/tmp/x.sock")).name(),
            SOURCE_NAME
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fatal_bind_error_is_err_not_quiet_degradation() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("no-such-dir").join("hook.sock");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let res = Box::new(HookRouter::new(bad)).run(tx).await;
        assert!(
            res.is_err(),
            "a non-Busy bind failure must be Err (SourceDeath fuel), got {res:?}"
        );
    }
}
