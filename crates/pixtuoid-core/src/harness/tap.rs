//! The recorder's listener: the shim's endpoint under the daemon's own guards
//! ([`crate::source::hook`]), each payload kept as received and teed to a headless
//! pixtuoid, on Unix and Windows alike.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::source::hook::{HookConn, HookSocketListener, SocketBusy};

/// A hook can still be in flight when the CLI's own process exits, so the wait
/// is measured from the LAST PAYLOAD rather than from that exit.
const SETTLE: Duration = Duration::from_millis(500);
const QUIET_ROUNDS: u32 = 4;
const MAX_ROUNDS: u32 = 40;

/// How long a send waits out a pipe whose every instance is mid-handshake.
#[cfg(windows)]
const PIPE_BUSY_BOUND: Duration = Duration::from_secs(2);
#[cfg(windows)]
const PIPE_BUSY_BACKOFF: Duration = Duration::from_millis(10);

/// Where a shim connects: a Unix socket, or a Windows named pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint(PathBuf);

impl Endpoint {
    /// The variable a shim reads its endpoint from.
    pub const ENV: &str = crate::source::hook::SOCKET_ENV;

    /// The endpoint `name` private to `dir`: a socket inside it, or on Windows a
    /// pipe named after it, since the pipe namespace is machine-wide.
    pub fn in_dir(dir: &Path, name: &str) -> Self {
        if cfg!(windows) {
            let tag = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            Self(PathBuf::from(format!(r"\\.\pipe\pixtuoid-{tag}-{name}")))
        } else {
            Self(dir.join(format!("{name}.sock")))
        }
    }

    /// The endpoint a caller already named.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    /// What [`Self::ENV`] and `pixtuoid run --socket` take.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Whether a server listens here, asked without connecting: a probe that
    /// connected would take a pipe's instance from the next client.
    pub fn accepting(&self) -> bool {
        #[cfg(windows)]
        {
            pipe_accepting(&self.0)
        }
        #[cfg(not(windows))]
        {
            self.0.exists()
        }
    }

    /// One connection carrying `bytes`, as the shim sends a payload.
    ///
    /// # Errors
    ///
    /// If nothing listens, or the write fails.
    pub(crate) async fn send(&self, bytes: &[u8]) -> std::io::Result<()> {
        #[cfg(windows)]
        let mut conn = {
            use tokio::net::windows::named_pipe::ClientOptions;
            let started = std::time::Instant::now();
            loop {
                match ClientOptions::new().open(&self.0) {
                    Ok(c) => break c,
                    Err(e)
                        if e.raw_os_error()
                            == Some(windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32)
                            && started.elapsed() < PIPE_BUSY_BOUND =>
                    {
                        tokio::time::sleep(PIPE_BUSY_BACKOFF).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        #[cfg(not(windows))]
        let mut conn = tokio::net::UnixStream::connect(&self.0).await?;
        conn.write_all(bytes).await?;
        conn.shutdown().await
    }
}

/// `WaitNamedPipeW` "returns immediately" when no instance exists, and is
/// nonzero when "at least one instance of the pipe [is] available", connecting
/// none (learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-waitnamedpipew).
#[cfg(windows)]
fn pipe_accepting(name: &Path) -> bool {
    /// Long enough to find a free instance, short enough to poll with.
    const WAIT_MS: u32 = 1;
    let wide = crate::win32::nul_terminated(name.as_os_str());
    // SAFETY: `wide` is NUL-terminated and outlives the call, which only reads it.
    unsafe { windows_sys::Win32::System::Pipes::WaitNamedPipeW(wide.as_ptr(), WAIT_MS) != 0 }
}

/// Every payload a shim delivered to one endpoint, in arrival order.
#[derive(Debug)]
pub struct Tap {
    lines: Arc<Mutex<Vec<String>>>,
    serving: Mutex<Serving>,
}

/// The listener's thread, which returns only once the listener stops, with
/// why: every later payload is lost.
#[derive(Debug)]
struct Serving {
    thread: Option<std::thread::JoinHandle<String>>,
    stopped: Option<String>,
}

impl Tap {
    /// Listen at `at` on a runtime of its own, keeping each payload and teeing
    /// it to `tee`; returns once the endpoint takes connections.
    ///
    /// # Errors
    ///
    /// `AddrInUse` if a live listener holds `at` (a squatted pipe name is one);
    /// any other bind failure as itself.
    pub fn bind(at: &Endpoint, tee: Option<Endpoint>) -> std::io::Result<Self> {
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let at = at.path().to_path_buf();
        let (bound_tx, bound_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let why = e.to_string();
                    let _ = bound_tx.send(Err(e));
                    return why;
                }
            };
            rt.block_on(async move {
                let listener = match HookSocketListener::bind(&at).await {
                    Ok(l) => l,
                    Err(e) => {
                        let kind = if e.is::<SocketBusy>() {
                            std::io::ErrorKind::AddrInUse
                        } else {
                            std::io::ErrorKind::Other
                        };
                        let why = format!("{e:#}");
                        let _ = bound_tx.send(Err(std::io::Error::new(kind, why.clone())));
                        return why;
                    }
                };
                let _ = bound_tx.send(Ok(()));
                let tee = Arc::new(tee);
                match listener
                    .serve(move |conn| keep(conn, Arc::clone(&sink), Arc::clone(&tee)))
                    .await
                {
                    Ok(()) => "the listener returned".to_owned(),
                    Err(e) => format!("{e:#}"),
                }
            })
        });
        bound_rx.recv().map_err(std::io::Error::other)??;
        Ok(Self::watching(lines, thread))
    }

    /// A tap over `lines` whose listener runs on `thread`.
    fn watching(lines: Arc<Mutex<Vec<String>>>, thread: std::thread::JoinHandle<String>) -> Self {
        Self {
            lines,
            serving: Mutex::new(Serving {
                thread: Some(thread),
                stopped: None,
            }),
        }
    }

    fn len(&self) -> usize {
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Wait until no payload has arrived for `QUIET_ROUNDS` × `SETTLE`, at most
    /// `MAX_ROUNDS` × `SETTLE` in all.
    pub fn settle(&self) {
        let (mut prev, mut quiet, mut round) = (usize::MAX, 0, 0);
        while quiet < QUIET_ROUNDS && round < MAX_ROUNDS {
            let n = self.len();
            quiet = if n == prev { quiet + 1 } else { 0 };
            prev = n;
            round += 1;
            std::thread::sleep(SETTLE);
        }
    }

    /// Every payload so far.
    pub fn payloads(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Why the listener stopped accepting, if it has.
    pub fn stopped(&self) -> Option<String> {
        let mut serving = self
            .serving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(thread) = serving.thread.take_if(|t| t.is_finished()) {
            serving.stopped = Some(
                thread
                    .join()
                    .unwrap_or_else(|_| "the listener panicked".to_owned()),
            );
        }
        serving.stopped.clone()
    }
}

/// Keep one connection's payload. The tee runs detached, outside the
/// listener's per-connection budget, so a slow headless pixtuoid never costs
/// the recording a payload; a tee that cannot connect is its own failure, left
/// in its scene log.
async fn keep(mut conn: HookConn, sink: Arc<Mutex<Vec<String>>>, tee: Arc<Option<Endpoint>>) {
    let mut bytes = Vec::new();
    if conn.read_to_end(&mut bytes).await.is_err() {
        return;
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.trim().is_empty() {
        return;
    }
    sink.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(text);
    if let Some(to) = tee.as_ref().clone() {
        tokio::spawn(async move {
            let _ = to.send(&bytes).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = "{\"hook_event_name\":\"SessionStart\"}\n";
    /// How long a test waits on a thread it started, and how often it looks.
    const PATIENCE: Duration = Duration::from_secs(5);
    const POLL: Duration = Duration::from_millis(10);

    /// `bytes` to `at` on a runtime that ends with the call: a Windows pipe
    /// client's handle closes only while its runtime runs, and a test that then
    /// blocks in `settle` would hold it open past the tap's budget.
    fn deliver(at: &Endpoint, bytes: &str) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(at.send(bytes.as_bytes()))
            .expect("send");
    }

    #[test]
    fn a_payload_is_kept_and_teed() {
        let d = tempfile::tempdir().expect("tempdir");
        let tee_at = Endpoint::in_dir(d.path(), "tee");
        let tee = Tap::bind(&tee_at, None).expect("tee");
        let at = Endpoint::in_dir(d.path(), "tap");
        let tap = Tap::bind(&at, Some(tee_at)).expect("tap");
        deliver(&at, LINE);
        tap.settle();
        tee.settle();
        assert_eq!(tap.payloads(), [LINE]);
        assert_eq!(tee.payloads(), [LINE]);
    }

    #[test]
    fn accepting_turns_true_on_bind_and_takes_no_instance() {
        let d = tempfile::tempdir().expect("tempdir");
        let at = Endpoint::in_dir(d.path(), "tap");
        assert!(!at.accepting(), "nothing listens yet");
        let tap = Tap::bind(&at, None).expect("tap");
        assert!(at.accepting());
        assert!(
            at.accepting(),
            "a probe leaves the instance for the next one"
        );
        deliver(&at, LINE);
        tap.settle();
        assert_eq!(tap.payloads(), [LINE]);
    }

    #[test]
    fn a_second_tap_on_one_endpoint_is_refused_as_in_use() {
        let d = tempfile::tempdir().expect("tempdir");
        let at = Endpoint::in_dir(d.path(), "tap");
        let _first = Tap::bind(&at, None).expect("first");
        let second = Tap::bind(&at, None).expect_err("a live listener holds it");
        assert_eq!(second.kind(), std::io::ErrorKind::AddrInUse, "{second}");
    }

    #[test]
    fn a_payload_is_cut_at_the_listeners_byte_cap() {
        let cap = usize::try_from(crate::source::hook::MAX_CONN_BYTES).expect("fits");
        let d = tempfile::tempdir().expect("tempdir");
        let at = Endpoint::in_dir(d.path(), "tap");
        let tap = Tap::bind(&at, None).expect("tap");
        // The tap closes once it holds the cap, so the write past it may break.
        let _ = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(at.send("x".repeat(cap + 1).as_bytes()));
        tap.settle();
        assert_eq!(
            tap.payloads().iter().map(String::len).collect::<Vec<_>>(),
            [cap]
        );
    }

    #[test]
    fn a_listener_that_stops_is_reported_with_its_reason() {
        let d = tempfile::tempdir().expect("tempdir");
        let live = Tap::bind(&Endpoint::in_dir(d.path(), "tap"), None).expect("tap");
        assert_eq!(live.stopped(), None, "a live listener");
        let gone = Tap::watching(
            Arc::default(),
            std::thread::spawn(|| "re-creating the pipe failed".to_owned()),
        );
        let started = std::time::Instant::now();
        while gone.stopped().is_none() && started.elapsed() < PATIENCE {
            std::thread::sleep(POLL);
        }
        assert_eq!(
            gone.stopped().as_deref(),
            Some("re-creating the pipe failed")
        );
    }

    #[test]
    fn a_tee_with_no_listener_costs_the_recording_nothing() {
        let d = tempfile::tempdir().expect("tempdir");
        let at = Endpoint::in_dir(d.path(), "tap");
        let tap = Tap::bind(&at, Some(Endpoint::in_dir(d.path(), "absent"))).expect("tap");
        deliver(&at, LINE);
        tap.settle();
        assert_eq!(tap.payloads(), [LINE]);
    }

    /// The hook server always pre-creates its next instance, so only a server
    /// of exactly one shows the probe reading "every instance is taken".
    #[cfg(windows)]
    #[tokio::test]
    async fn accepting_reads_a_single_instance_server() {
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
        let d = tempfile::tempdir().expect("tempdir");
        let at = Endpoint::in_dir(d.path(), "single");
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .create(at.path())
            .expect("server");
        assert!(at.accepting(), "its one instance is free");
        let _client = ClientOptions::new().open(at.path()).expect("client");
        server.connect().await.expect("connected");
        assert!(!at.accepting(), "its one instance is taken");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_send_waits_out_a_pipe_whose_one_instance_is_taken() {
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
        const INSTANCES: usize = 2;
        let d = tempfile::tempdir().expect("tempdir");
        let at = Endpoint::in_dir(d.path(), "busy");
        let first = ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(INSTANCES)
            .create(at.path())
            .expect("server");
        let _holder = ClientOptions::new().open(at.path()).expect("holder");
        first.connect().await.expect("held");
        let send = tokio::spawn({
            let at = at.clone();
            async move { at.send(LINE.as_bytes()).await }
        });
        tokio::time::sleep(PIPE_BUSY_BACKOFF * 5).await;
        let mut second = ServerOptions::new()
            .max_instances(INSTANCES)
            .create(at.path())
            .expect("a free instance");
        let (connected, sent) = tokio::join!(
            tokio::time::timeout(PIPE_BUSY_BOUND, second.connect()),
            send
        );
        connected
            .expect("the send connects once an instance frees")
            .expect("connected");
        sent.expect("joined").expect("sent");
        let mut got = String::new();
        second.read_to_string(&mut got).await.expect("read");
        assert_eq!(got, LINE);
    }
}
