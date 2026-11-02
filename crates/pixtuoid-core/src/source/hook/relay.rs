//! One hook endpoint, one office per instance: the instance holding the
//! endpoint fans each payload line out, and every other instance keeps a
//! subscription to it and decodes those lines through its own reducer.
//!
//! Lines cross, not decoded events: a line is the shim's wire, which every
//! installed version already reads, and an `AgentEvent` is an internal type two
//! versions need not agree on.
//!
//! The same instance answers `pixtuoid doctor`, from memory, so what it reports
//! dies with it and there is no file to go stale.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::warn;

use super::{CONN_TIMEOUT, Client, ConnectedSources, HookSocketListener, SocketBusy, connect};

/// A subscriber's first line. Not JSON: an owner older than the relay skips it
/// as malformed, where an object would raise that owner's decode-drift nudge.
const SUBSCRIBE_LINE: &str = "pixtuoid-subscribe\n";
const SUBSCRIBED_LINE: &str = "pixtuoid-subscribed\n";
/// Not JSON, for [`SUBSCRIBE_LINE`]'s reason, and no longer than it, which bounds
/// the read of a connection's first line.
const STATUS_LINE: &str = "pixtuoid-status\n";

/// Payload lines a subscriber can fall behind before it loses the oldest.
const BACKLOG: usize = 1024;

/// A status reply's ceiling, far past what the registry's rows can fill.
const MAX_STATUS_BYTES: u64 = 64 * 1024;

/// What a connection's first line makes it. A control line matches whole, and
/// the shim serializes a JSON value, which starts no line like either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opening {
    Subscribe,
    Status,
    Payload,
}

impl Opening {
    fn of(head: &[u8]) -> Self {
        if head == SUBSCRIBE_LINE.as_bytes() {
            Self::Subscribe
        } else if head == STATUS_LINE.as_bytes() {
            Self::Status
        } else {
            Self::Payload
        }
    }
}

/// What the instance holding a hook endpoint reports. On the wire, between two
/// versions: a later field needs a `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerStatus {
    /// How long this instance has held the endpoint, and so how far back
    /// [`Self::last_hook_ago_ms`] reaches.
    pub held_for_ms: u64,
    /// Its connection gate as it stands, not as it booted.
    pub connected: BTreeSet<String>,
    /// Each source's latest payload that decoded to something.
    pub last_hook_ago_ms: BTreeMap<String, u64>,
}

/// What asking a hook endpoint found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointStatus {
    /// Nothing holds it, and an instance could.
    Absent,
    /// Nothing holds it, and a bind refused, for this reason.
    Unbindable(String),
    /// This user may not connect: another user's endpoint, or one whose owner
    /// could not be proved.
    Denied,
    /// No status came back: an older pixtuoid, an owner too busy to read, or one
    /// whose socket file is gone.
    Silent,
    /// The owner answered.
    Live(OwnerStatus),
}

#[derive(Debug)]
struct OwnerState {
    /// Wall clock, as the Sources panel's ages are: `Instant` may stop while a
    /// laptop sleeps, and "last hook 2m ago" after a night is the wrong answer.
    held_since: SystemTime,
    /// Keyed by the registry, never the wire: `_pixtuoid_source` arrives
    /// unchecked, so its names would grow this for the life of the instance.
    last_hook: Mutex<BTreeMap<&'static str, SystemTime>>,
    connected: ConnectedSources,
}

/// A poisoned table is still a table: an insert holds no invariant to break.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn ms_since(at: SystemTime) -> u64 {
    let age = at.elapsed().unwrap_or_default();
    u64::try_from(age.as_millis()).unwrap_or(u64::MAX)
}

/// The owner's half: fans each payload line out to its subscribers, and answers
/// a status line.
#[derive(Debug, Clone)]
pub(super) struct Relay {
    lines: broadcast::Sender<Arc<str>>,
    state: Arc<OwnerState>,
}

impl Relay {
    pub(super) fn new(connected: ConnectedSources) -> Self {
        Self {
            lines: broadcast::channel(BACKLOG).0,
            state: Arc::new(OwnerState {
                held_since: SystemTime::now(),
                last_hook: Mutex::default(),
                connected,
            }),
        }
    }

    /// Never blocks: the shim's connection is waiting on this reader.
    pub(super) fn forward(&self, line: &str) {
        if self.lines.receiver_count() > 0 {
            // Fails only if the last subscriber left since the count.
            let _ = self.lines.send(Arc::from(format!("{line}\n")));
        }
    }

    /// A payload of `source` did something.
    pub(super) fn sighted(&self, source: &str) {
        if let Some(row) = crate::source::registry::descriptor_for(source) {
            locked(&self.state.last_hook).insert(row.name, SystemTime::now());
        }
    }

    fn status(&self) -> OwnerStatus {
        let OwnerState {
            held_since,
            last_hook,
            connected,
        } = &*self.state;
        OwnerStatus {
            held_for_ms: ms_since(*held_since),
            connected: connected.snapshot().into_iter().collect(),
            last_hook_ago_ms: locked(last_hook)
                .iter()
                .map(|(source, at)| ((*source).to_string(), ms_since(*at)))
                .collect(),
        }
    }

    /// Answered here, inside the listener's budget and permit, unlike a feed.
    async fn answer(&self, mut conn: impl AsyncWrite + Unpin) {
        if let Ok(mut reply) = serde_json::to_string(&self.status()) {
            reply.push('\n');
            let _ = write_line(&mut conn, &reply).await;
        }
    }

    /// `conn`'s payload, or `None` once a control line has claimed `conn`. The
    /// spawned feed is what leaves the listener's budget and permit behind.
    pub(super) async fn payload_of<C>(
        &self,
        conn: tokio::io::Take<C>,
    ) -> Option<impl AsyncRead + Unpin + Send + use<C>>
    where
        C: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut conn = BufReader::new(conn);
        let mut head = Vec::new();
        let read = (&mut conn)
            .take(SUBSCRIBE_LINE.len() as u64)
            .read_until(b'\n', &mut head)
            .await;
        match read.map(|_| Opening::of(&head)) {
            Ok(Opening::Subscribe) => {
                let lines = self.lines.subscribe();
                tokio::spawn(feed(conn.into_inner().into_inner(), lines));
                None
            }
            Ok(Opening::Status) => {
                self.answer(conn.into_inner().into_inner()).await;
                None
            }
            Ok(Opening::Payload) | Err(_) => Some(std::io::Cursor::new(head).chain(conn)),
        }
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

/// Ask `path`'s owner what it knows, which changes nothing there. With no owner
/// to ask, bind the endpoint for a moment, as the next instance would: nothing
/// short of a bind can say whether one succeeds, and a connect's errno can't tell
/// a dead endpoint from a saturated live one (`acquire_bind_lock`'s rationale).
pub async fn endpoint_status(path: &Path) -> EndpointStatus {
    let query = async {
        match connect(path).await {
            Ok(conn) => status_over(conn).await,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => EndpointStatus::Denied,
            Err(_) => match HookSocketListener::bind(path).await {
                Ok(_) => EndpointStatus::Absent,
                Err(e) if e.is::<SocketBusy>() => EndpointStatus::Silent,
                Err(e) => EndpointStatus::Unbindable(format!("{e:#}")),
            },
        }
    };
    // Past the budget that ends a served connection: a saturated owner lets the
    // connect land in its backlog and never reads it.
    tokio::time::timeout(2 * CONN_TIMEOUT, query)
        .await
        .unwrap_or(EndpointStatus::Silent)
}

/// No half-close after the line: `NamedPipeClient::poll_shutdown` is a no-op, so
/// an older owner, which skips the line and waits for more, holds a pipe to its
/// budget, and one path costs both OSes that second.
async fn status_over(mut conn: impl AsyncRead + AsyncWrite + Unpin) -> EndpointStatus {
    if conn.write_all(STATUS_LINE.as_bytes()).await.is_err() {
        return EndpointStatus::Silent;
    }
    let mut reply = String::new();
    let read = BufReader::new(conn.take(MAX_STATUS_BYTES))
        .read_line(&mut reply)
        .await;
    match read.map(|_| serde_json::from_str(&reply)) {
        Ok(Ok(status)) => EndpointStatus::Live(status),
        _ => EndpointStatus::Silent,
    }
}

/// What the hook fuzz target drives past the decoder: `bytes` as a connection,
/// which leaves a payload exactly as it came, and as an owner's reply.
#[cfg(feature = "harness")]
pub(crate) async fn drive_wire(bytes: &[u8]) {
    let capacity = bytes.len() + SUBSCRIBED_LINE.len() + 1;
    let (mut shim, server) = tokio::io::duplex(capacity);
    shim.write_all(bytes).await.expect("it fits the duplex");
    drop(shim);
    let relay = Relay::new(ConnectedSources::default());
    let payload = relay.payload_of(server.take(super::MAX_CONN_BYTES)).await;
    let head = bytes.split_inclusive(|b| *b == b'\n').next();
    if let Some(mut payload) = payload {
        let mut read = Vec::new();
        payload.read_to_end(&mut read).await.expect("a duplex read");
        assert_eq!(read, bytes, "the demux changed a payload");
    } else {
        assert_ne!(head.map(Opening::of), Some(Opening::Payload));
    }
    let (client, mut owner) = tokio::io::duplex(capacity);
    let reply = async move {
        owner.write_all(bytes).await.expect("it fits the duplex");
    };
    let _ = tokio::join!(status_over(client), reply);
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
        let relay = Relay::new(ConnectedSources::default());
        let rx = relay.lines.subscribe();
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

    // An owner older than this line skips it the same way, and that reads as
    // Silent, never as a drift nudge on its office.
    #[tokio::test]
    async fn an_owner_without_a_status_answer_reads_as_silent() {
        let (logs, _guard) = crate::test_capture::capture_warns();
        let (client, server) = tokio::io::duplex(64);
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        // It waits for more lines, so only the listener's budget hangs up.
        let older = tokio::spawn(tokio::time::timeout(
            CONN_TIMEOUT,
            super::super::handle_conn(server, tx, None, None, None),
        ));
        let status = tokio::spawn(status_over(client));
        let _ = older.await.unwrap();
        assert_eq!(status.await.unwrap(), EndpointStatus::Silent);
        assert!(rx.recv().await.is_none());
        let out = logs.contents();
        assert!(out.contains("malformed hook line skipped"), "got {out:?}");
        assert!(!out.contains("hook decode error"), "got {out:?}");
    }

    #[tokio::test]
    async fn a_reply_that_is_no_status_reads_as_silent() {
        for reply in [&b"not json\n"[..], b"{\"held_for_ms\":1}\n", b"\n", b"{"] {
            let (client, mut owner) = tokio::io::duplex(1024);
            let status = tokio::spawn(status_over(client));
            let mut line = vec![0; STATUS_LINE.len()];
            owner.read_exact(&mut line).await.unwrap();
            assert_eq!(line, STATUS_LINE.as_bytes());
            owner.write_all(reply).await.unwrap();
            drop(owner);
            assert_eq!(status.await.unwrap(), EndpointStatus::Silent, "{reply:?}");
        }
    }

    // The ONE demux of a connection: a hook line must never reach the reply path.
    #[test]
    fn only_a_whole_control_line_opens_anything_but_a_payload() {
        assert_eq!(Opening::of(SUBSCRIBE_LINE.as_bytes()), Opening::Subscribe);
        assert_eq!(Opening::of(STATUS_LINE.as_bytes()), Opening::Status);
        for head in [
            &b""[..],
            b"\n",
            b"pixtuoid-status",
            b"pixtuoid-status \n",
            b"PIXTUOID-STATUS\n",
            b"\"pixtuoid-status\"\n",
            b"{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s\"}\n",
            b"{\"_pixtuoid_source\":\"pixtuoid-status\"}\n",
        ] {
            assert_eq!(Opening::of(head), Opening::Payload, "{head:?}");
        }
    }

    #[test]
    fn a_status_line_fits_the_read_of_a_first_line() {
        assert!(STATUS_LINE.len() <= SUBSCRIBE_LINE.len());
    }

    #[tokio::test]
    async fn a_daemon_envelope_with_no_delta_is_no_sighting() {
        let relay = Relay::new(ConnectedSources::default());
        let (presence_tx, _presence_rx) = tokio::sync::mpsc::unbounded_channel();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(
                b"{\"_pixtuoid_source\":\"openclaw\",\"type\":\"no_such_type\",\
                         \"gatewayPort\":18789,\"_pid\":4242}\n",
            )
            .await
            .unwrap();
        drop(client);
        super::super::handle_conn(server, tx, None, Some(presence_tx), Some(relay.clone())).await;
        assert!(relay.status().last_hook_ago_ms.is_empty());
    }

    #[test]
    fn only_a_registered_source_is_a_sighting() {
        let relay = Relay::new(ConnectedSources::default());
        relay.sighted("codex");
        relay.sighted("not-a-source");
        let seen = relay.status().last_hook_ago_ms;
        assert_eq!(seen.keys().collect::<Vec<_>>(), ["codex"]);
    }

    #[test]
    fn a_later_field_does_not_break_a_status() {
        let parsed: Result<OwnerStatus, _> = serde_json::from_str(
            r#"{"held_for_ms":1,"connected":[],"last_hook_ago_ms":{},"later":true}"#,
        );
        assert!(parsed.is_ok(), "{parsed:?}");
    }

    #[tokio::test]
    async fn the_demux_hands_a_payload_on_byte_for_byte() {
        let past_the_head_read = vec![b'x'; SUBSCRIBE_LINE.len() * 2];
        for bytes in [
            &b""[..],
            b"\n",
            b"{\"session_id\":\"s\"}\n",
            b"{\"session_id\":\"s\"}\n{\"session_id\":\"t\"}",
            b"\xff\n\xfe",
            b"pixtuoid-status",
            b"pixtuoid-status \n",
            b"pixtuoid-subscribed\n",
            &past_the_head_read,
        ] {
            drive_wire(bytes).await;
        }
    }

    #[tokio::test]
    async fn a_control_line_never_reads_as_a_payload() {
        for line in [SUBSCRIBE_LINE, STATUS_LINE] {
            let (mut peer, server) = tokio::io::duplex(1024);
            peer.write_all(line.as_bytes()).await.unwrap();
            let relay = Relay::new(ConnectedSources::default());
            assert!(
                relay.payload_of(server.take(1024)).await.is_none(),
                "{line:?}"
            );
        }
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
