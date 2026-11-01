//! One hook endpoint, one office per instance: the instance holding the
//! endpoint fans each payload line out, and every other instance keeps a
//! subscription to it and decodes those lines through its own reducer.
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

use super::{CONN_TIMEOUT, Client, HookConn, connect};

/// A subscriber's first line. Not JSON: an owner older than the relay skips it
/// as malformed, where an object would raise that owner's decode-drift nudge.
const SUBSCRIBE_LINE: &str = "pixtuoid-subscribe\n";
const SUBSCRIBED_LINE: &str = "pixtuoid-subscribed\n";

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
    pub(super) async fn payload_or_subscribe(
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
    if !write_line(&mut conn, SUBSCRIBED_LINE).await {
        return;
    }
    loop {
        match lines.recv().await {
            Ok(line) => {
                if !write_line(&mut conn, &line).await {
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

/// Whether `line` went out in time. A subscriber that stopped reading (SIGSTOP,
/// a debugger) must lose its feed, or its receiver pins [`BACKLOG`] lines in the
/// owner; it subscribes again once it reads the hang-up.
async fn write_line(conn: &mut (impl AsyncWrite + Unpin), line: &str) -> bool {
    matches!(
        tokio::time::timeout(CONN_TIMEOUT, conn.write_all(line.as_bytes())).await,
        Ok(Ok(()))
    )
}

/// The payload lines of the instance holding an endpoint, until it exits.
pub(super) type Payloads = BufReader<Client>;

/// The subscriber's half.
pub(super) async fn subscribe(path: &Path) -> Result<Payloads> {
    let mut conn = connect(path).await.context("connecting")?;
    conn.write_all(SUBSCRIBE_LINE.as_bytes())
        .await
        .context("subscribing")?;
    let mut conn = BufReader::new(conn);
    let mut ack = String::new();
    let read = tokio::time::timeout(CONN_TIMEOUT, conn.read_line(&mut ack)).await;
    anyhow::ensure!(
        matches!(read, Ok(Ok(_))) && ack == SUBSCRIBED_LINE,
        "its owner took no subscription: quit it if it is an older pixtuoid, then restart this one"
    );
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::super::{HookPidWatch, HookRouter, HookSocketListener};
    use super::*;
    use crate::source::{AgentEvent, Source, TaggedReceiver};
    use std::path::PathBuf;
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

    #[tokio::test]
    async fn a_subscriber_that_stopped_reading_loses_its_feed() {
        const SMALLER_THAN_THE_ACK: usize = 4;
        let (conn, _stalled) = tokio::io::duplex(SMALLER_THAN_THE_ACK);
        let (tx, rx) = broadcast::channel::<Arc<str>>(1);
        tokio::time::timeout(PATIENCE, feed(conn, rx))
            .await
            .expect("a stalled write times out");
        assert_eq!(tx.receiver_count(), 0, "its receiver pins no line");
    }

    // An owner older than the relay is the one reader of this line that isn't
    // ours to change.
    #[tokio::test]
    async fn an_owner_without_a_relay_skips_a_subscribe_line() {
        let (logs, _guard) = crate::test_capture::capture_warns();
        let (mut client, server) = tokio::io::duplex(64);
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        client.write_all(SUBSCRIBE_LINE.as_bytes()).await.unwrap();
        drop(client);
        super::super::handle_conn(server, tx, None, None, None).await;
        assert!(rx.recv().await.is_none());
        let out = logs.contents();
        assert!(out.contains("malformed hook line skipped"), "got {out:?}");
        assert!(!out.contains("hook decode error"), "got {out:?}");
    }

    #[tokio::test]
    async fn the_feed_ends_when_its_subscriber_is_gone() {
        let (conn, mut peer) = tokio::io::duplex(64);
        let (tx, rx) = broadcast::channel(1);
        let feeding = tokio::spawn(feed(conn, rx));
        let mut ack = vec![0; SUBSCRIBED_LINE.len()];
        peer.read_exact(&mut ack).await.unwrap();
        drop(peer);
        tx.send(Arc::from("line\n")).unwrap();
        tokio::time::timeout(PATIENCE, feeding)
            .await
            .expect("a failed write ends the feed")
            .unwrap();
    }

    /// `tag`'s private endpoint and the directory keeping it so.
    fn endpoint(tag: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = if cfg!(windows) {
            let pid = std::process::id();
            PathBuf::from(format!(r"\\.\pipe\pixtuoid-relay-{pid}-{tag}"))
        } else {
            dir.path().join("hook.sock")
        };
        (dir, path)
    }

    fn listen(path: &Path) -> (tokio::task::JoinHandle<Result<()>>, TaggedReceiver) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let path = path.to_path_buf();
        let task = tokio::spawn(async move { HookSocketListener::bind(path).await?.run(tx).await });
        (task, rx)
    }

    /// A connection to `path` as soon as its listener takes one.
    async fn client(path: &Path) -> Client {
        tokio::time::timeout(PATIENCE, async {
            loop {
                match connect(path).await {
                    Ok(conn) => return conn,
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .expect("the listener comes up")
    }

    #[tokio::test]
    async fn a_subscribe_line_split_across_writes_still_subscribes() {
        let (_dir, path) = endpoint("split");
        let (task, _rx) = listen(&path);
        let mut conn = client(&path).await;
        let (head, tail) = SUBSCRIBE_LINE.split_at(SUBSCRIBE_LINE.len() / 2);
        conn.write_all(head.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        conn.write_all(tail.as_bytes()).await.unwrap();

        let mut ack = vec![0; SUBSCRIBED_LINE.len()];
        tokio::time::timeout(PATIENCE, conn.read_exact(&mut ack))
            .await
            .expect("a subscriber is answered")
            .unwrap();
        assert_eq!(ack, SUBSCRIBED_LINE.as_bytes());
        task.abort();
    }

    // Only a connection's first line subscribes, so the listener must not write
    // back to a shim. The read ends at the listener's budget.
    #[tokio::test]
    async fn a_subscribe_line_after_a_payload_gets_no_answer() {
        let (_dir, path) = endpoint("second-line");
        let (task, mut rx) = listen(&path);
        let mut conn = client(&path).await;
        let session = "{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s\",\
                       \"transcript_path\":\"/p/s.jsonl\",\"cwd\":\"/repo\"}\n";
        conn.write_all(format!("{session}{SUBSCRIBE_LINE}").as_bytes())
            .await
            .unwrap();
        let mut answer = Vec::new();
        let _ = tokio::time::timeout(PATIENCE, conn.read_to_end(&mut answer)).await;

        assert!(answer.is_empty(), "got {answer:?}");
        let (_, ev) = rx.recv().await.expect("the payload still decodes");
        assert!(matches!(ev, AgentEvent::SessionStart { .. }));
        task.abort();
    }

    // The exit watch runs on a subscriber too, or an abrupt exit leaves its
    // sprite to the stale sweep.
    #[tokio::test]
    async fn a_subscriber_ends_a_session_whose_pid_dies() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        if HookPidWatch::spawn(tx.clone()).is_none() {
            return; // no exit-watch backend on this platform
        }
        let (_dir, path) = endpoint("pid");
        let (owner, _owner_rx) = listen(&path);
        drop(client(&path).await);
        let subscriber = tokio::spawn(Box::new(HookRouter::new(path.clone())).run(tx));

        let mut cli = crate::test_child::sleeper();
        let pid = cli.id();
        let line = format!(
            "{{\"_pixtuoid_source\":\"kimi\",\"hook_event_name\":\"SessionStart\",\
             \"session_id\":\"sess-K\",\"cwd\":\"/repo\",\"_pid\":{pid}}}\n"
        );
        tokio::time::timeout(PATIENCE, async {
            loop {
                client(&path)
                    .await
                    .write_all(line.as_bytes())
                    .await
                    .unwrap();
                let got = tokio::time::timeout(Duration::from_millis(25), rx.recv()).await;
                if got.is_ok() {
                    return;
                }
            }
        })
        .await
        .expect("the line reaches the subscriber");

        cli.kill().expect("kill the cli");
        let _ = cli.wait();
        tokio::time::timeout(PATIENCE, async {
            while let Some((_, ev)) = rx.recv().await {
                if matches!(ev, AgentEvent::SessionEnd { .. }) {
                    return;
                }
            }
        })
        .await
        .expect("a SessionEnd once the watched pid dies");
        owner.abort();
        subscriber.abort();
    }
}
