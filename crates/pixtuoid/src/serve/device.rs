//! One device's connection: the TLS handshake, its hello, then the office line
//! by line, with an answer to whatever it asks, until it leaves.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, ensure};
use pixtuoid_core::remote::{self, Answer, Ask, Changed, HEARTBEAT, Hello, Reply, SourceRow};
use serde::Serialize;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};
use tokio::net::TcpStream;
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinSet;
use tokio::time::{Instant, interval_at, sleep, sleep_until, timeout, timeout_at};
use tokio_rustls::TlsAcceptor;

use super::store::{Hashed, Store};
use crate::runtime::SceneRx;

/// What a peer gets to finish the TLS handshake and say hello, before it has
/// proved anything and while it holds a [`super::MAX_CONNS`] slot.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest line a device may write, its newline included: a hello is a token
/// and a little JSON, an ask less.
const MAX_LINE_BYTES: usize = 256;

/// A device whose socket takes longer than this to drain has stalled.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// A device renders a scene, not each event of a busy reducer.
const MIN_FRAME_GAP: Duration = Duration::from_millis(250);

/// The least time between two writes' starts, across every device; a chosen
/// value. A write can rewrite another CLI's config, racing that CLI's own unlocked
/// write, and can run the CLI: the gap bounds how often devices, together, start
/// one. One sooner waits it out, holding [`Devices::gate`].
const MIN_WRITE_GAP: Duration = Duration::from_secs(1);

/// What an [`Ask::Sources`] reads: blocking, as it reads files and may run a CLI.
pub(super) type ReadRows = Arc<dyn Fn() -> Vec<SourceRow> + Send + Sync>;

/// What an [`Ask::Change`] does, given the device's [`Hashed::short`], the source
/// and `on`: blocking, as it writes another CLI's config and may run it. `None`
/// for a source this build doesn't know.
pub(super) type MakeChange = Arc<dyn Fn(&str, &str, bool) -> Option<Changed> + Send + Sync>;

/// What a device's connection reads from.
#[derive(Clone)]
pub(super) struct Devices {
    store: Store,
    scene: SceneRx,
    rows: ReadRows,
    change: MakeChange,
    /// One permit, held through an ask's blocking work across every device, a
    /// read's or a write's: that work can run a host CLI, which a hang holds
    /// until its limit, and a device that reconnects and asks again must not
    /// start a second.
    gate: Arc<Semaphore>,
    /// When the last write began, for [`MIN_WRITE_GAP`].
    last_write: Arc<Mutex<Option<Instant>>>,
    /// Each device's live stream, ended by dropping its sender. A phone that
    /// lost its Wi-Fi sends no FIN, so its old connection would hold a slot until
    /// TCP gave up on it.
    streams: Arc<Mutex<HashMap<Hashed, oneshot::Sender<()>>>>,
}

impl Devices {
    pub(super) fn new(store: Store, scene: SceneRx, rows: ReadRows, change: MakeChange) -> Self {
        Self {
            store,
            scene,
            rows,
            change,
            gate: Arc::new(Semaphore::new(1)),
            last_write: Arc::default(),
            streams: Arc::default(),
        }
    }

    /// Serve the peer on `tcp` until either side ends it.
    pub(super) async fn serve(self, tcp: TcpStream, acceptor: TlsAcceptor) -> Result<()> {
        let deadline = Instant::now() + HELLO_TIMEOUT;
        let tls = timeout_at(deadline, acceptor.accept(tcp))
            .await
            .context("the TLS handshake timed out")??;
        let mut stream = tokio::io::BufReader::new(tls);
        let served = self.converse(&mut stream, deadline).await;
        // A close_notify, so a refusal's reader sees an end and not a cut.
        let _ = timeout(WRITE_TIMEOUT, stream.shutdown()).await;
        served
    }

    async fn converse<S>(&self, stream: &mut S, deadline: Instant) -> Result<()>
    where
        S: AsyncBufRead + AsyncRead + AsyncWrite + Unpin,
    {
        let (reply, device) = timeout_at(deadline, self.greet(stream))
            .await
            .context("no hello in time")??;
        write_line(stream, &json_line(&reply)?).await?;
        match device {
            Some(device) => self.stream(stream, device).await,
            None => Ok(()),
        }
    }

    /// Read the hello and reply to it. Anything but a verdict ends the connection
    /// with no reply: that's a retry, where a refusal is a re-pair.
    async fn greet<S>(&self, stream: &mut S) -> Result<(Reply, Option<Hashed>)>
    where
        S: AsyncBufRead + Unpin,
    {
        let mut line = Vec::new();
        ensure!(next_line(stream, &mut line).await?, "no hello");
        // No reply, as when the store can't answer: a newer device's tag is no
        // reason for it to re-pair.
        let hello: Hello = serde_json::from_slice(&line).context("an unreadable hello")?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || match hello {
            Hello::Pair { code } => Ok(store.redeem(&code, SystemTime::now())?.map_or(
                (Reply::Refused, None),
                |token| {
                    let device = Hashed::of(&token);
                    (Reply::Paired { token }, Some(device))
                },
            )),
            Hello::Resume { token } => {
                let device = Hashed::of(&token);
                Ok(if store.knows(&device)? {
                    (Reply::Resumed, Some(device))
                } else {
                    (Reply::Refused, None)
                })
            }
        })
        .await?
    }

    /// Write the office to `stream` on each change and each [`HEARTBEAT`], and an
    /// answer to each ask between those lines, all from this one loop.
    async fn stream<S>(&self, stream: &mut S, device: Hashed) -> Result<()>
    where
        S: AsyncBufRead + AsyncWrite + Unpin,
    {
        let mut replaced = self.take_over(device.clone());
        let mut scene = self.scene.clone();
        scene.mark_changed();
        let mut heartbeat = interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
        let mut line = Vec::new();
        let mut asked = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut replaced => return Ok(()),
                // One ask at a time, so answers, which name no ask, come back in
                // the order asked; what follows waits in the socket.
                read = next_line(stream, &mut line), if asked.is_empty() => {
                    if !read? {
                        return Ok(());
                    }
                    let ask = serde_json::from_slice(&line).context("an unreadable ask")?;
                    line.clear();
                    match ask {
                        Ask::Sources => {
                            asked.spawn(self.sources());
                        }
                        Ask::Change { source, on } => {
                            asked.spawn(self.change(device.clone(), source, on));
                        }
                        Ask::Unknown => write_line(stream, &json_line(&Answer::Unsupported)?).await?,
                    }
                    continue;
                }
                Some(answer) = asked.join_next() => {
                    write_line(stream, &json_line(&answer??)?).await?;
                    continue;
                }
                changed = scene.changed() => changed?,
                // Per beat, so `pair --reset` reaches a live stream.
                _ = heartbeat.tick() => {
                    let store = self.store.clone();
                    let device = device.clone();
                    let paired = tokio::task::spawn_blocking(move || store.knows(&device));
                    if !paired.await?? {
                        return Ok(());
                    }
                }
            }
            // Out of the watch first: encoding under its read lock holds the writer.
            let latest = Arc::clone(&scene.borrow_and_update());
            let line = remote::encode(&latest, SystemTime::now())?;
            write_line(stream, &line).await?;
            sleep(MIN_FRAME_GAP).await;
        }
    }

    /// Become `device`'s live stream, ending any older one.
    fn take_over(&self, device: Hashed) -> oneshot::Receiver<()> {
        let (end, ended) = oneshot::channel();
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(device, end);
        ended
    }

    /// [`Ask::Sources`]'s answer, its blocking read run under [`Self::gate`].
    fn sources(&self) -> impl Future<Output = Result<Answer>> + Send + 'static {
        let (gate, rows) = (Arc::clone(&self.gate), Arc::clone(&self.rows));
        async move {
            let held = gate.acquire_owned().await?;
            // Moved in, so it is held until the read returns, though the
            // connection that asked has ended.
            let rows = tokio::task::spawn_blocking(move || {
                let _held = held;
                rows()
            })
            .await?;
            Ok(Answer::Sources { rows })
        }
    }

    /// [`Ask::Change`]'s answer, its write run under [`Self::gate`] once
    /// [`MIN_WRITE_GAP`] has passed since the last began, for a device the store
    /// still knows. A forgotten device's write ends the connection and spends no gap.
    fn change(
        &self,
        device: Hashed,
        source: String,
        on: bool,
    ) -> impl Future<Output = Result<Answer>> + Send + 'static {
        let (gate, last_write) = (Arc::clone(&self.gate), Arc::clone(&self.last_write));
        let (store, change) = (self.store.clone(), Arc::clone(&self.change));
        async move {
            let held = gate.acquire_owned().await?;
            let last = *last_write.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(at) = last {
                sleep_until(at + MIN_WRITE_GAP).await;
            }
            let changed = tokio::task::spawn_blocking(move || {
                let _held = held;
                // Here, not per beat, so a `pair --reset` stops the next write.
                ensure!(store.knows(&device)?, "a write by a forgotten device");
                *last_write.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                Ok(change(device.short(), &source, on))
            })
            .await??;
            Ok(changed.map_or(Answer::Unsupported, Answer::Changed))
        }
    }
}

/// Read on toward the end of a device's line, appending to `line`: `true` once
/// it holds a whole one, `false` at the stream's end. Cancel safe, as
/// [`read_until`](https://docs.rs/tokio/latest/tokio/io/trait.AsyncBufReadExt.html#method.read_until)
/// is and `read_line` is not, so a hello and an ask are framed alike.
///
/// # Errors
///
/// If the stream fails, or no newline comes within [`MAX_LINE_BYTES`].
async fn next_line<S>(stream: &mut S, line: &mut Vec<u8>) -> Result<bool>
where
    S: AsyncBufRead + Unpin,
{
    let room = MAX_LINE_BYTES - line.len();
    (&mut *stream)
        .take(room as u64)
        .read_until(b'\n', line)
        .await?;
    if line.ends_with(b"\n") {
        return Ok(true);
    }
    // Short of the cap, `read_until` stops only at the end.
    ensure!(line.len() < MAX_LINE_BYTES, "a line past the cap");
    Ok(false)
}

fn json_line(value: &impl Serialize) -> serde_json::Result<String> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

async fn write_line(stream: &mut (impl AsyncWrite + Unpin), line: &str) -> Result<()> {
    timeout(WRITE_TIMEOUT, async {
        stream.write_all(line.as_bytes()).await?;
        stream.flush().await
    })
    .await
    .context("the device stopped reading")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pixtuoid_core::SceneState;
    use pixtuoid_core::remote::{Outcome, RemoteOffice, SourceState};
    use tokio::io::{BufReader, DuplexStream, ReadHalf, WriteHalf, duplex, split};
    use tokio::sync::watch;

    use super::*;

    /// Each change the fixture's devices made: the device, the source and `on`.
    type Made = Arc<Mutex<Vec<(String, String, bool)>>>;

    struct Fixture {
        store: Store,
        devices: Devices,
        scene: watch::Sender<Arc<SceneState>>,
        made: Made,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        fixture_reading(Arc::new(Vec::new))
    }

    /// A source the fixture knows that is already as asked.
    const ALREADY_SO: &str = "codex";

    fn fixture_reading(rows: ReadRows) -> Fixture {
        fixture_with(rows, Arc::new(|| {}))
    }

    /// What runs as a change begins, past its pairing check.
    type Pause = Arc<dyn Fn() + Send + Sync>;

    /// Its changes each run `before`, then are recorded: `copilot` changes,
    /// [`ALREADY_SO`] doesn't, and it knows no other source.
    fn fixture_with(rows: ReadRows, before: Pause) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("remote.json"));
        let (scene, scene_rx) = watch::channel(Arc::new(SceneState::uniform(4)));
        let made = Made::default();
        let making = Arc::clone(&made);
        let change: MakeChange = Arc::new(move |who, source, on| {
            before();
            let entry = (who.to_owned(), source.to_owned(), on);
            making.lock().expect("not poisoned").push(entry);
            let (id, outcome) = match source {
                "copilot" if on => ("copilot", Outcome::Connected),
                "copilot" => ("copilot", Outcome::Disconnected),
                ALREADY_SO => (ALREADY_SO, Outcome::NoOp),
                _ => return None,
            };
            Some(Changed::new(id, on, outcome, false, None, None))
        });
        Fixture {
            devices: Devices::new(store.clone(), scene_rx, rows, change),
            store,
            scene,
            made,
            _dir: dir,
        }
    }

    fn change(source: &str, on: bool) -> Ask {
        Ask::Change {
            source: source.to_owned(),
            on,
        }
    }

    fn made(fixture: &Fixture) -> Vec<(String, String, bool)> {
        fixture.made.lock().expect("not poisoned").clone()
    }

    /// Past any test's wait: a read on the runtime's own thread then fails its
    /// test instead of hanging it.
    const HOLD: Duration = Duration::from_secs(5);

    /// A pause that waits on `release`, and rows behind it, to show what goes on
    /// during an ask.
    struct Held {
        pause: Pause,
        rows: ReadRows,
        started: tokio::sync::mpsc::UnboundedReceiver<()>,
        release: std::sync::mpsc::Sender<()>,
    }

    fn held() -> Held {
        let (start, started) = tokio::sync::mpsc::unbounded_channel();
        let (release, released) = std::sync::mpsc::channel();
        let released = Mutex::new(released);
        let pause: Pause = Arc::new(move || {
            start.send(()).expect("the test waits on it");
            let _ = released.lock().expect("not poisoned").recv_timeout(HOLD);
        });
        let before_rows = Arc::clone(&pause);
        Held {
            pause,
            rows: Arc::new(move || {
                before_rows();
                vec![one_row()]
            }),
            started,
            release,
        }
    }

    fn one_row() -> SourceRow {
        SourceRow::new("codex", "Codex", SourceState::Connected, false)
    }

    /// The device's end of a connection `fixture` is serving.
    struct Peer {
        lines: BufReader<ReadHalf<DuplexStream>>,
        writer: WriteHalf<DuplexStream>,
        served: tokio::task::JoinHandle<Result<()>>,
    }

    impl Peer {
        fn connect(devices: &Devices) -> Self {
            let (ours, theirs) = duplex(64 * 1024);
            let devices = devices.clone();
            let served = tokio::spawn(async move {
                let mut stream = BufReader::new(theirs);
                let deadline = Instant::now() + HELLO_TIMEOUT;
                devices.converse(&mut stream, deadline).await
            });
            let (reader, writer) = split(ours);
            Self {
                lines: BufReader::new(reader),
                writer,
                served,
            }
        }

        async fn say(&mut self, line: &str) {
            let line = format!("{line}\n");
            self.writer.write_all(line.as_bytes()).await.expect("sent");
        }

        async fn hello(&mut self, hello: &Hello) {
            self.say(&serde_json::to_string(hello).expect("it encodes"))
                .await;
        }

        async fn line(&mut self) -> String {
            let mut line = String::new();
            self.lines.read_line(&mut line).await.expect("it reads");
            line
        }

        async fn reply(&mut self) -> Reply {
            serde_json::from_str(&self.line().await).expect("a reply")
        }

        async fn ask(&mut self, ask: &Ask) {
            self.say(&serde_json::to_string(ask).expect("it encodes"))
                .await;
        }

        /// The next answer, past any office lines.
        async fn answer(&mut self) -> Answer {
            loop {
                let line = self.line().await;
                assert!(!line.is_empty(), "the stream ended");
                if let Some(answer) = Answer::from_line(line.as_bytes()) {
                    return answer;
                }
            }
        }

        /// Prove the stream's loop live: a change comes back as an office line.
        async fn sees_a_change(&mut self, fixture: &Fixture) {
            fixture
                .scene
                .send(Arc::new(SceneState::uniform(4)))
                .expect("the office is live");
            let line = self.line().await;
            assert!(!line.is_empty(), "the stream ended");
            assert_eq!(Answer::from_line(line.as_bytes()), None, "{line}");
        }

        async fn ended(self) -> Result<()> {
            self.served.await.expect("the task finishes")
        }
    }

    /// A device past its hello and its first office line.
    async fn streaming(fixture: &Fixture) -> Peer {
        let token = paired(fixture);
        let mut peer = Peer::connect(&fixture.devices);
        peer.hello(&Hello::Resume { token }).await;
        assert_eq!(peer.reply().await, Reply::Resumed);
        peer.line().await;
        peer
    }

    fn paired(fixture: &Fixture) -> String {
        let code = fixture.store.issue_code(SystemTime::now()).expect("a code");
        fixture
            .store
            .redeem(&code, SystemTime::now())
            .expect("it redeems")
            .expect("a good code")
    }

    #[tokio::test]
    async fn a_code_pairs_and_the_first_line_after_the_reply_is_the_office() {
        let fixture = fixture();
        let code = fixture.store.issue_code(SystemTime::now()).expect("a code");
        let mut peer = Peer::connect(&fixture.devices);
        peer.hello(&Hello::Pair { code }).await;

        let Reply::Paired { token } = peer.reply().await else {
            panic!("a good code pairs");
        };
        let mut office = RemoteOffice::new([4; pixtuoid_core::state::MAX_FLOORS]);
        office
            .apply(peer.line().await.as_bytes(), SystemTime::now())
            .expect("a line");
        assert!(fixture.store.knows(&Hashed::of(&token)).expect("it reads"));
    }

    #[tokio::test]
    async fn a_spent_code_and_a_stranger_are_refused_and_closed() {
        let fixture = fixture();
        let token = "not-a-token".to_owned();
        for hello in [
            Hello::Pair {
                code: "spent".into(),
            },
            Hello::Resume { token },
        ] {
            let mut peer = Peer::connect(&fixture.devices);
            peer.hello(&hello).await;
            assert_eq!(peer.reply().await, Reply::Refused, "{hello:?}");
            assert_eq!(peer.line().await, "", "nothing follows a refusal");
            peer.ended().await.expect("a refusal is a clean end");
        }
    }

    /// A refusal would send a newer device back to a QR over a tag this build
    /// doesn't know.
    #[tokio::test]
    async fn a_hello_this_build_cant_read_gets_no_reply() {
        let fixture = fixture();
        let past_the_cap = Hello::Resume {
            token: "t".repeat(MAX_LINE_BYTES),
        };
        let past_the_cap = serde_json::to_string(&past_the_cap).expect("it encodes");
        for hello in [
            r#"{"hello":"from_the_future"}"#,
            "junk",
            past_the_cap.as_str(),
        ] {
            let mut peer = Peer::connect(&fixture.devices);
            peer.say(hello).await;
            assert_eq!(peer.line().await, "", "{hello}");
            assert!(peer.ended().await.is_err(), "{hello}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_peer_that_says_nothing_loses_its_slot_at_the_deadline() {
        let fixture = fixture();
        let peer = Peer::connect(&fixture.devices);
        let started = Instant::now();
        assert!(peer.ended().await.is_err());
        assert_eq!(started.elapsed(), HELLO_TIMEOUT);
    }

    /// The clock pauses so that only a change can deliver within half a beat.
    #[tokio::test(start_paused = true)]
    async fn a_change_reaches_a_device_ahead_of_the_heartbeat() {
        let fixture = fixture();
        let token = paired(&fixture);
        let mut peer = Peer::connect(&fixture.devices);
        peer.hello(&Hello::Resume { token }).await;
        assert_eq!(peer.reply().await, Reply::Resumed);
        peer.line().await;

        fixture
            .scene
            .send(Arc::new(SceneState::uniform(4)))
            .expect("the office is live");

        let line = timeout(HEARTBEAT / 2, peer.line()).await;
        assert!(line.is_ok_and(|line| !line.is_empty()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_stops_draining_is_dropped_at_the_timeout() {
        let (mut ours, _theirs) = duplex(1);
        let started = Instant::now();
        assert!(write_line(&mut ours, "more than one byte").await.is_err());
        assert_eq!(started.elapsed(), WRITE_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_office_still_sends_its_line() {
        let fixture = fixture();
        let token = paired(&fixture);
        let mut peer = Peer::connect(&fixture.devices);
        peer.hello(&Hello::Resume { token }).await;
        peer.reply().await;
        peer.line().await;

        let beat = timeout(HEARTBEAT * 2, peer.line()).await;

        assert!(beat.is_ok_and(|line| !line.is_empty()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_reset_reaches_a_live_stream_within_a_heartbeat() {
        let fixture = fixture();
        let token = paired(&fixture);
        let mut peer = Peer::connect(&fixture.devices);
        peer.hello(&Hello::Resume { token }).await;
        peer.reply().await;
        peer.line().await;

        fixture.store.reset().expect("it resets");

        timeout(HEARTBEAT * 2, peer.ended())
            .await
            .expect("the stream ends")
            .expect("revoked is a clean end");
    }

    #[tokio::test]
    async fn a_devices_new_connection_ends_its_old_one() {
        let fixture = fixture();
        let token = paired(&fixture);
        let hello = Hello::Resume { token };
        let mut old = Peer::connect(&fixture.devices);
        old.hello(&hello).await;
        assert_eq!(old.reply().await, Reply::Resumed);
        old.line().await;

        let mut new = Peer::connect(&fixture.devices);
        new.hello(&hello).await;
        assert_eq!(new.reply().await, Reply::Resumed);

        old.ended().await.expect("replaced is a clean end");
    }

    /// One device's reconnect must not cost another its stream.
    #[tokio::test(start_paused = true)]
    async fn two_devices_stream_side_by_side() {
        let fixture = fixture();
        let (one, two) = (paired(&fixture), paired(&fixture));
        let mut first = Peer::connect(&fixture.devices);
        first.hello(&Hello::Resume { token: one }).await;
        first.reply().await;
        first.line().await;
        let mut second = Peer::connect(&fixture.devices);
        second.hello(&Hello::Resume { token: two }).await;
        second.reply().await;
        second.line().await;

        fixture
            .scene
            .send(Arc::new(SceneState::uniform(4)))
            .expect("the office is live");

        let lines = timeout(HEARTBEAT / 2, async {
            (first.line().await, second.line().await)
        });
        let (one, two) = lines.await.expect("both streams are live");
        assert!(!one.is_empty() && !two.is_empty());
    }

    #[tokio::test]
    async fn a_device_that_leaves_ends_its_stream() {
        let fixture = fixture();
        let mut leaves = streaming(&fixture).await;
        leaves.writer.shutdown().await.expect("it closes");
        leaves.ended().await.expect("leaving is a clean end");

        let mut mid_line = streaming(&fixture).await;
        let half_an_ask = br#"{"ask":"#;
        mid_line.writer.write_all(half_an_ask).await.expect("sent");
        mid_line.writer.shutdown().await.expect("it closes");
        mid_line.ended().await.expect("a cut line is a clean end");
    }

    #[tokio::test]
    async fn an_ask_is_answered_and_the_stream_goes_on() {
        let fixture = fixture_reading(Arc::new(|| vec![one_row()]));
        let mut peer = streaming(&fixture).await;

        peer.ask(&Ask::Sources).await;
        assert_eq!(
            peer.answer().await,
            Answer::Sources {
                rows: vec![one_row()]
            }
        );

        peer.sees_a_change(&fixture).await;
        peer.ask(&Ask::Sources).await;
        assert!(matches!(peer.answer().await, Answer::Sources { .. }));
    }

    /// A newer device's ask costs it nothing here.
    #[tokio::test]
    async fn an_ask_this_build_doesnt_know_is_unsupported_and_keeps_the_stream() {
        let fixture = fixture();
        let mut peer = streaming(&fixture).await;

        peer.say(r#"{"ask":"from_the_future","x":1}"#).await;
        assert_eq!(peer.answer().await, Answer::Unsupported);

        peer.sees_a_change(&fixture).await;
    }

    #[tokio::test]
    async fn an_unreadable_line_ends_the_stream() {
        let fixture = fixture();
        for line in ["junk", "", "{}"] {
            let mut peer = streaming(&fixture).await;
            peer.say(line).await;
            assert!(peer.ended().await.is_err(), "{line}");
        }
    }

    #[tokio::test]
    async fn a_line_may_fill_the_cap_and_not_pass_it() {
        let padded = |bytes: usize| format!("{:<bytes$}", r#"{"ask":"sources"}"#);
        let newline = 1;
        let fixture = fixture();

        let mut peer = streaming(&fixture).await;
        peer.say(&padded(MAX_LINE_BYTES - newline)).await;
        assert!(matches!(peer.answer().await, Answer::Sources { .. }));

        let mut peer = streaming(&fixture).await;
        peer.say(&padded(MAX_LINE_BYTES)).await;
        assert!(peer.ended().await.is_err());
    }

    /// Reads end anywhere: two lines in one, or one across two.
    #[tokio::test]
    async fn an_ask_may_share_a_write_or_span_two() {
        let fixture = fixture();
        let token = paired(&fixture);
        let hello = serde_json::to_string(&Hello::Resume { token }).expect("it encodes");
        let ask = serde_json::to_string(&Ask::Sources).expect("it encodes");

        let mut peer = Peer::connect(&fixture.devices);
        peer.say(&format!("{hello}\n{ask}\n{ask}")).await;
        assert_eq!(peer.reply().await, Reply::Resumed);
        assert!(matches!(peer.answer().await, Answer::Sources { .. }));
        assert!(matches!(peer.answer().await, Answer::Sources { .. }));

        let (head, tail) = ask.split_at(ask.len() / 2);
        let head = head.as_bytes();
        peer.writer.write_all(head).await.expect("sent");
        peer.sees_a_change(&fixture).await;
        peer.say(tail).await;
        assert!(matches!(peer.answer().await, Answer::Sources { .. }));
    }

    #[tokio::test]
    async fn the_office_keeps_streaming_while_an_ask_runs() {
        let mut held = held();
        let fixture = fixture_reading(Arc::clone(&held.rows));
        let mut peer = streaming(&fixture).await;

        peer.ask(&Ask::Sources).await;
        held.started.recv().await.expect("it starts");
        peer.sees_a_change(&fixture).await;

        held.release.send(()).expect("it waits");
        assert!(matches!(peer.answer().await, Answer::Sources { .. }));
    }

    #[tokio::test]
    async fn a_second_ask_waits_for_the_first() {
        let mut held = held();
        let fixture = fixture_reading(Arc::clone(&held.rows));
        let mut peer = streaming(&fixture).await;
        let ask = serde_json::to_string(&Ask::Sources).expect("it encodes");

        peer.say(&format!("{ask}\n{ask}")).await;
        held.started.recv().await.expect("the first starts");
        peer.sees_a_change(&fixture).await;
        assert!(held.started.try_recv().is_err(), "the second started");

        held.release.send(()).expect("it waits");
        assert!(matches!(peer.answer().await, Answer::Sources { .. }));
        held.started.recv().await.expect("the second starts");
        held.release.send(()).expect("it waits");
        assert!(matches!(peer.answer().await, Answer::Sources { .. }));
    }

    /// An answer names no ask, so a device matches it by order: an ask the
    /// server could answer at once still waits its turn, and asks past a line's
    /// cap wait in the socket.
    #[tokio::test]
    async fn answers_come_back_in_the_order_asked() {
        let mut held = held();
        let fixture = fixture_reading(Arc::clone(&held.rows));
        let mut peer = streaming(&fixture).await;
        let ask = serde_json::to_string(&Ask::Sources).expect("it encodes");
        let behind = MAX_LINE_BYTES / ask.len() + 1;

        peer.ask(&Ask::Sources).await;
        held.started.recv().await.expect("it starts");
        let unknown = r#"{"ask":"from_the_future"}"#;
        peer.say(&(format!("{ask}\n").repeat(behind) + unknown))
            .await;
        peer.sees_a_change(&fixture).await;

        for _ in 0..=behind {
            held.release.send(()).expect("it waits");
            assert!(matches!(peer.answer().await, Answer::Sources { .. }));
        }
        assert_eq!(peer.answer().await, Answer::Unsupported);
    }

    /// A device that reconnects while its read runs can't start a second, which
    /// gets this long to show it would.
    #[tokio::test]
    async fn one_read_runs_at_a_time_across_connections() {
        const A_SECOND_WOULD_START: Duration = Duration::from_millis(500);
        let mut held = held();
        let fixture = fixture_reading(Arc::clone(&held.rows));
        let hello = Hello::Resume {
            token: paired(&fixture),
        };
        let mut old = Peer::connect(&fixture.devices);
        old.hello(&hello).await;
        assert_eq!(old.reply().await, Reply::Resumed);
        old.line().await;
        old.ask(&Ask::Sources).await;
        held.started.recv().await.expect("it starts");

        let mut new = Peer::connect(&fixture.devices);
        new.hello(&hello).await;
        assert_eq!(new.reply().await, Reply::Resumed);
        new.line().await;
        old.ended().await.expect("replaced is a clean end");
        new.ask(&Ask::Sources).await;
        new.sees_a_change(&fixture).await;
        let second = timeout(A_SECOND_WOULD_START, held.started.recv()).await;
        assert!(second.is_err(), "a second read started");

        held.release.send(()).expect("it waits");
        held.started.recv().await.expect("the new read starts");
        held.release.send(()).expect("it waits");
        assert!(matches!(new.answer().await, Answer::Sources { .. }));
    }

    /// The device reaches the change, and so its audit line, by a short hash.
    #[tokio::test(start_paused = true)]
    async fn a_change_is_answered_and_made_for_a_short_hash_never_the_token() {
        let fixture = fixture();
        let token = paired(&fixture);
        let mut peer = Peer::connect(&fixture.devices);
        peer.hello(&Hello::Resume {
            token: token.clone(),
        })
        .await;
        assert_eq!(peer.reply().await, Reply::Resumed);

        peer.ask(&change("copilot", true)).await;
        let Answer::Changed(changed) = peer.answer().await else {
            panic!("a change's answer");
        };
        assert_eq!(changed.outcome(), Outcome::Connected);
        let [(who, source, on)] = made(&fixture).try_into().expect("one change");
        assert_eq!((source.as_str(), on), ("copilot", true));
        assert_eq!(who, Hashed::of(&token).short());
        assert!(who.len() < token.len() && !token.contains(&who), "{who}");

        peer.ask(&change("no-such-cli", false)).await;
        assert_eq!(peer.answer().await, Answer::Unsupported);
    }

    /// It spends none of the gap, which a device still paired would wait out.
    #[tokio::test(start_paused = true)]
    async fn a_forgotten_devices_write_is_refused() {
        let fixture = fixture();
        let mut forgotten = streaming(&fixture).await;
        fixture.store.reset().expect("it resets");

        forgotten.ask(&change("copilot", true)).await;
        assert!(forgotten.ended().await.is_err());
        assert!(made(&fixture).is_empty(), "{:?}", made(&fixture));

        let mut paired = streaming(&fixture).await;
        let asked = Instant::now();
        paired.ask(&change("copilot", true)).await;
        assert!(matches!(paired.answer().await, Answer::Changed(_)));
        assert!(asked.elapsed() < MIN_WRITE_GAP, "{:?}", asked.elapsed());
    }

    /// A reset stops the next write, not one past its check, which is made and
    /// answered for the device that asked.
    #[tokio::test]
    async fn a_write_past_its_check_finishes_through_a_reset() {
        let mut held = held();
        let fixture = fixture_with(Arc::new(Vec::new), Arc::clone(&held.pause));
        let token = paired(&fixture);
        let mut peer = Peer::connect(&fixture.devices);
        peer.hello(&Hello::Resume {
            token: token.clone(),
        })
        .await;
        assert_eq!(peer.reply().await, Reply::Resumed);
        peer.ask(&change("copilot", true)).await;
        held.started
            .recv()
            .await
            .expect("the write is past its check");

        fixture.store.reset().expect("it resets");
        held.release.send(()).expect("it waits");
        assert!(matches!(peer.answer().await, Answer::Changed(_)));
        let device = Hashed::of(&token).short().to_owned();
        assert_eq!(made(&fixture), [(device, "copilot".to_owned(), true)]);
    }

    /// `first`'s answer, once a write asked as soon as it came has run within
    /// `EDGE` of the gap's end, on either side. The clock pauses, and a
    /// `spawn_blocking` holds its auto-advance until it ends (tokio's
    /// `runtime/blocking/schedule.rs`), so the first write began as it answered.
    async fn a_second_write_waits_after(first: &Ask) -> Answer {
        const EDGE: Duration = Duration::from_millis(1);
        let fixture = fixture();
        let mut peer = streaming(&fixture).await;
        peer.ask(first).await;
        let answer = peer.answer().await;
        let began = Instant::now();

        peer.ask(&change("copilot", true)).await;
        sleep_until(began + MIN_WRITE_GAP - EDGE).await;
        assert_eq!(
            made(&fixture).len(),
            1,
            "the second write ran inside the gap"
        );
        let second = timeout_at(began + MIN_WRITE_GAP + EDGE, peer.answer()).await;
        assert!(matches!(second, Ok(Answer::Changed(_))), "{second:?}");
        assert_eq!(made(&fixture).len(), 2);
        answer
    }

    #[tokio::test(start_paused = true)]
    async fn a_write_inside_the_gap_waits_it_out() {
        let first = a_second_write_waits_after(&change("copilot", true)).await;
        assert!(matches!(first, Answer::Changed(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_write_to_a_source_this_build_doesnt_know_spends_the_gap() {
        let first = a_second_write_waits_after(&change("no-such-cli", true)).await;
        assert_eq!(first, Answer::Unsupported);
    }

    #[tokio::test(start_paused = true)]
    async fn a_write_that_changes_nothing_spends_the_gap() {
        let first = a_second_write_waits_after(&change(ALREADY_SO, true)).await;
        let Answer::Changed(first) = first else {
            panic!("a change's answer");
        };
        assert_eq!(first.outcome(), Outcome::NoOp);
    }

    /// A write waits for a read on another connection, which can hold a CLI.
    #[tokio::test]
    async fn a_write_waits_for_a_read_on_the_gate() {
        const A_WRITE_WOULD_START: Duration = Duration::from_millis(500);
        let mut held = held();
        let fixture = fixture_reading(Arc::clone(&held.rows));
        let mut reader = streaming(&fixture).await;
        reader.ask(&Ask::Sources).await;
        held.started.recv().await.expect("the read starts");

        let mut writer = streaming(&fixture).await;
        writer.ask(&change("copilot", true)).await;
        writer.sees_a_change(&fixture).await;
        sleep(A_WRITE_WOULD_START).await;
        assert!(made(&fixture).is_empty(), "the write ran beside the read");

        held.release.send(()).expect("it waits");
        assert!(matches!(reader.answer().await, Answer::Sources { .. }));
        assert!(matches!(writer.answer().await, Answer::Changed(_)));
        assert_eq!(made(&fixture).len(), 1);
    }
}
