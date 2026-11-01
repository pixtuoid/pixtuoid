//! The hook plane of every instance but the one holding the endpoint: a
//! subscriber keeps one connection to that owner and reads each payload line it
//! accepts, so every instance decodes them into its own office.
//!
//! Lines cross, not decoded events: a line is the shim's wire, which every
//! installed version already reads, and an `AgentEvent` is an internal type two
//! versions need not agree on.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::warn;

use super::{CONN_TIMEOUT, HookConn, imp};

/// A subscriber's first line. An owner older than the relay decodes it to
/// nothing and hangs up, which [`subscribe`] reads as a refusal.
pub(super) const SUBSCRIBE_LINE: &str = "{\"_pixtuoid_subscribe\":1}\n";
const SUBSCRIBED_LINE: &str = "{\"_pixtuoid_subscribed\":1}\n";

/// Payload lines a subscriber can fall behind before it loses the oldest.
const BACKLOG: usize = 1024;

/// The owner's half: fans each payload line out to its subscribers.
#[derive(Debug, Clone)]
pub(super) struct Relay(broadcast::Sender<Arc<str>>);

impl Relay {
    pub(super) fn new() -> Self {
        Self(broadcast::channel(BACKLOG).0)
    }

    /// Never blocks: the shim's connection is waiting on this reader.
    pub(super) fn forward(&self, line: &str) {
        if self.0.receiver_count() > 0 {
            // Fails only if the last subscriber left since the count.
            let _ = self.0.send(Arc::from(format!("{line}\n")));
        }
    }

    /// `conn`'s payload, or `None` once a subscribing `conn` is attached. The
    /// spawned feed is what leaves the listener's budget and permit behind.
    pub(super) async fn payload_of(
        &self,
        conn: HookConn,
    ) -> Option<impl AsyncRead + Unpin + Send + use<>> {
        let mut conn = BufReader::new(conn);
        let mut head = Vec::new();
        let read = (&mut conn)
            .take(SUBSCRIBE_LINE.len() as u64)
            .read_until(b'\n', &mut head)
            .await;
        if read.is_ok() && head == SUBSCRIBE_LINE.as_bytes() {
            tokio::spawn(feed(conn.into_inner().into_inner(), self.0.subscribe()));
            return None;
        }
        Some(std::io::Cursor::new(head).chain(conn))
    }
}

async fn feed(mut conn: impl AsyncWrite + Unpin, mut lines: broadcast::Receiver<Arc<str>>) {
    if conn.write_all(SUBSCRIBED_LINE.as_bytes()).await.is_err() {
        return;
    }
    loop {
        match lines.recv().await {
            Ok(line) => {
                if conn.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(
                    skipped,
                    "hook relay subscriber fell behind; payloads skipped"
                );
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// The subscriber's half: the payload lines of the instance holding `path`,
/// from here until it exits.
pub(super) async fn subscribe(path: &Path) -> Result<impl AsyncRead + Unpin + use<>> {
    let mut conn = imp::connect(path).await.context("connecting")?;
    conn.write_all(SUBSCRIBE_LINE.as_bytes())
        .await
        .context("subscribing")?;
    let mut conn = BufReader::new(conn);
    let mut ack = String::new();
    tokio::time::timeout(CONN_TIMEOUT, conn.read_line(&mut ack))
        .await
        .context("waiting for the subscription")?
        .context("reading the subscription")?;
    anyhow::ensure!(ack == SUBSCRIBED_LINE, "the owner refused the subscription");
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const PATIENCE: Duration = Duration::from_secs(5);

    fn lines_of(bytes: &[u8]) -> Vec<&str> {
        std::str::from_utf8(bytes).unwrap().lines().collect()
    }

    #[tokio::test]
    async fn a_subscriber_that_fell_behind_loses_the_oldest_and_keeps_reading() {
        const CAPACITY: usize = 2;
        let (tx, rx) = broadcast::channel(CAPACITY);
        for line in ["lost\n", "kept\n", "newest\n"] {
            tx.send(Arc::from(line)).unwrap();
        }
        drop(tx);
        let mut out = Vec::new();
        feed(&mut out, rx).await;
        assert_eq!(
            lines_of(&out),
            [SUBSCRIBED_LINE.trim_end(), "kept", "newest"]
        );
    }

    #[tokio::test]
    async fn a_forward_keeps_every_line_whole_and_in_order() {
        let relay = Relay::new();
        let rx = relay.0.subscribe();
        for line in ["one", "two"] {
            relay.forward(line);
        }
        let mut out = Vec::new();
        let feeding = feed(&mut out, rx);
        drop(relay);
        tokio::time::timeout(PATIENCE, feeding).await.unwrap();
        assert_eq!(lines_of(&out), [SUBSCRIBED_LINE.trim_end(), "one", "two"]);
    }

    // What an owner older than the relay does with a subscriber, which never
    // reaches its reducer as an agent.
    #[tokio::test]
    async fn an_owner_without_a_relay_decodes_a_subscribe_line_to_nothing() {
        let (mut client, server) = tokio::io::duplex(64);
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        client.write_all(SUBSCRIBE_LINE.as_bytes()).await.unwrap();
        drop(client);
        let conn = server.take(super::super::MAX_CONN_BYTES);
        super::super::handle_conn(conn, tx, None, None, None).await;
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn the_feed_ends_when_its_subscriber_is_gone() {
        let (conn, peer) = tokio::io::duplex(64);
        drop(peer);
        let (tx, rx) = broadcast::channel(1);
        tokio::time::timeout(PATIENCE, feed(conn, rx))
            .await
            .unwrap();
        drop(tx);
    }
}
