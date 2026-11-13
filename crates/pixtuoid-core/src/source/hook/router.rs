//! `HookRouter` — an instance's end of the ONE shared hook socket that EVERY
//! source's hooks ride (Codex, Reasonix, CodeWhale, opencode, Cursor, and the
//! OpenClaw daemon): it owns the socket, or subscribes to the instance that does
//! and takes the socket over when that instance exits.
//!
//! It implements [`Source`] DELIBERATELY, to inherit `spawn_with_health`'s
//! `SourceDeath` path for free instead of rebuilding it. It is still
//! INFRASTRUCTURE, not a CLI: it has NO registry/descriptor/badge row, and the
//! add-a-CLI checklist does not apply.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::source::{Source, TaggedSender};

use super::{
    ConnectedSources, HookPidWatch, HookSocketListener, PresenceSender, SocketBusy, handle_conn,
    relay,
};

/// Infrastructure name — NOT a registered source (no descriptor/badge). Names
/// the footer attribution `spawn_with_health` publishes when the plane dies.
pub(crate) const SOURCE_NAME: &str = "hook-router";

/// How many subscriptions in a row may fail before the plane is reported dead.
/// More than one: the loser of a takeover finds the winner not yet listening.
const SUBSCRIBE_ATTEMPTS: u32 = 3;
const SUBSCRIBE_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// An instance's end of the shared hook socket: binds the ONE socket or
/// subscribes to its owner, runs the decode loop, and feeds the daemon-presence
/// side channel.
#[derive(Debug)]
pub struct HookRouter {
    socket_path: PathBuf,
    /// The daemon-presence side channel (the gateway mascots): daemon payloads
    /// decode to presence deltas, not `AgentEvent`s. `None` disables it.
    presence_tx: Option<PresenceSender>,
    /// The gate a status reports while this instance owns the endpoint.
    connected: ConnectedSources,
}

impl HookRouter {
    /// Construct a router bound to `socket_path`, with the presence side
    /// channel disabled (attach it via the builder method).
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            presence_tx: None,
            connected: ConnectedSources::default(),
        }
    }

    /// Wire the daemon-presence side channel.
    #[must_use]
    pub fn with_presence_tx(mut self, presence_tx: Option<PresenceSender>) -> Self {
        self.presence_tx = presence_tx;
        self
    }

    /// Wire the connection gate a status reports.
    #[must_use]
    pub fn with_connected(mut self, connected: ConnectedSources) -> Self {
        self.connected = connected;
        self
    }
}

/// An instance's place on the hook endpoint.
enum Plane {
    Owner(HookSocketListener),
    Subscriber(relay::Payloads),
}

impl HookRouter {
    /// Take the endpoint, or subscribe to whoever holds it.
    async fn join(&self) -> Result<Plane> {
        let mut attempt = 0;
        loop {
            match HookSocketListener::bind(self.socket_path.clone()).await {
                Ok(socket) => return Ok(Plane::Owner(socket)),
                Err(e) if e.downcast_ref::<SocketBusy>().is_none() => return Err(e),
                Err(_) => {}
            }
            attempt += 1;
            match relay::subscribe(&self.socket_path).await {
                Ok(payloads) => return Ok(Plane::Subscriber(payloads)),
                Err(e) if attempt == SUBSCRIBE_ATTEMPTS => {
                    return Err(e).with_context(|| {
                        format!(
                            "subscribing to the hook endpoint at {}",
                            self.socket_path.display()
                        )
                    });
                }
                Err(_) => tokio::time::sleep(SUBSCRIBE_RETRY).await,
            }
        }
    }
}

impl Source for HookRouter {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        let pid_watch = HookPidWatch::spawn(tx.clone());
        loop {
            match self.join().await? {
                Plane::Owner(socket) => {
                    return socket
                        .with_pid_watch(pid_watch)
                        .with_presence(self.presence_tx.clone())
                        .with_connected(self.connected.clone())
                        .run(tx)
                        .await;
                }
                // No byte cap: the owner held each line to the listener's.
                Plane::Subscriber(payloads) => {
                    let presence_tx = self.presence_tx.clone();
                    handle_conn(payloads, tx.clone(), pid_watch.clone(), presence_tx, None).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "a non-`SocketBusy` bind failure must be Err (SourceDeath fuel), got {res:?}"
        );
    }
}
