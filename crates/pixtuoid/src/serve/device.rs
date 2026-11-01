//! One device's connection: the TLS handshake, its hello, then the office line
//! by line until it leaves.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use pixtuoid_core::remote::{self, HEARTBEAT, Hello, Reply};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::{Instant, interval_at, sleep, timeout, timeout_at};
use tokio_rustls::TlsAcceptor;

use super::store::{Hashed, Store};
use crate::runtime::SceneRx;

/// What a peer gets to finish the TLS handshake and say hello, before it has
/// proved anything and while it holds a [`super::MAX_CONNS`] slot.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// A hello is a token and a little JSON; a longer line fails to parse.
const MAX_HELLO_BYTES: u64 = 256;

/// A device whose socket takes longer than this to drain has stalled.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// A device renders a scene, not each event of a busy reducer.
const MIN_FRAME_GAP: Duration = Duration::from_millis(250);

/// What a device's connection reads from.
#[derive(Clone)]
pub(super) struct Devices {
    store: Store,
    scene: SceneRx,
    /// Each device's live stream, ended by dropping its sender. A phone that
    /// lost its Wi-Fi sends no FIN, so its old connection would hold a slot until
    /// TCP gave up on it.
    streams: Arc<Mutex<HashMap<Hashed, oneshot::Sender<()>>>>,
}

impl Devices {
    pub(super) fn new(store: Store, scene: SceneRx) -> Self {
        Self {
            store,
            scene,
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
        let mut line = serde_json::to_string(&reply)?;
        line.push('\n');
        write_line(stream, &line).await?;
        match device {
            Some(device) => self.stream(stream, device).await,
            None => Ok(()),
        }
    }

    /// Read the hello and answer it. Anything but an answer ends the connection
    /// with no reply: that's a retry, where a refusal is a re-pair.
    async fn greet<S>(&self, stream: &mut S) -> Result<(Reply, Option<Hashed>)>
    where
        S: AsyncBufRead + Unpin,
    {
        let mut line = String::new();
        (&mut *stream)
            .take(MAX_HELLO_BYTES)
            .read_line(&mut line)
            .await?;
        // No reply, as when the store can't answer: a newer device's tag is no
        // reason for it to re-pair.
        let hello: Hello = serde_json::from_str(&line).context("an unreadable hello")?;
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

    /// Write the office to `stream` on each change and each [`HEARTBEAT`].
    async fn stream<S>(&self, stream: &mut S, device: Hashed) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let mut replaced = self.take_over(device.clone());
        let mut scene = self.scene.clone();
        scene.mark_changed();
        let mut heartbeat = interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
        let mut unread = [0; 1];
        loop {
            tokio::select! {
                _ = &mut replaced => return Ok(()),
                // A hello is all a device has to say, so this is its end.
                _ = stream.read(&mut unread) => return Ok(()),
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
    use pixtuoid_core::remote::RemoteOffice;
    use tokio::io::{BufReader, DuplexStream, ReadHalf, WriteHalf, duplex, split};
    use tokio::sync::watch;

    use super::*;

    struct Fixture {
        store: Store,
        devices: Devices,
        scene: watch::Sender<Arc<SceneState>>,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("remote.json"));
        let (scene, scene_rx) = watch::channel(Arc::new(SceneState::uniform(4)));
        Fixture {
            devices: Devices::new(store.clone(), scene_rx),
            store,
            scene,
            _dir: dir,
        }
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

        async fn ended(self) -> Result<()> {
            self.served.await.expect("the task finishes")
        }
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
        let too_long = "x".repeat(usize::try_from(MAX_HELLO_BYTES).expect("small") * 2);
        for hello in [r#"{"hello":"from_the_future"}"#, "junk", too_long.as_str()] {
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
    async fn a_device_that_leaves_or_speaks_again_ends_its_stream() {
        let fixture = fixture();
        let token = paired(&fixture);
        let hello = Hello::Resume { token };

        let mut leaves = Peer::connect(&fixture.devices);
        leaves.hello(&hello).await;
        leaves.reply().await;
        leaves.line().await;
        leaves.writer.shutdown().await.expect("it closes");
        leaves.ended().await.expect("leaving is a clean end");

        let mut speaks = Peer::connect(&fixture.devices);
        speaks.hello(&hello).await;
        speaks.reply().await;
        speaks.line().await;
        speaks.say("anything").await;
        speaks.ended().await.expect("a read-only stream ends");
    }
}
