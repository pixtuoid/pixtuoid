//! A real `pixtuoid serve` from the outside: a device pairing through `pair`'s
//! link over TLS pinned to it, the office rebuilt from its lines, what leaves the
//! machine, and the server beside other instances on one hook plane.

#![cfg(feature = "serve")]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use pixtuoid_core::harness::host::SandboxEnv;
use pixtuoid_core::harness::tap::Endpoint;
use pixtuoid_core::remote::{Answer, Ask, Hello, PairLink, RemoteOffice, Reply, SourceState};
use pixtuoid_core::state::MAX_FLOORS;
use ring::digest::{SHA256, digest};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::crypto::ring::default_provider;
use tokio_rustls::rustls::crypto::{CryptoProvider, verify_tls13_signature};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::server::ParsedCertificate;
use tokio_rustls::rustls::{
    ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme, version::TLS13,
};

const PATIENCE: Duration = Duration::from_secs(30);

/// Well inside the hello deadline, which ends a connection that got a slot.
const AT_ONCE: Duration = Duration::from_secs(2);

/// What a device checks in place of a CA.
#[derive(Debug)]
struct PinnedTo {
    spki: String,
    provider: CryptoProvider,
}

impl ServerCertVerifier for PinnedTo {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let cert = ParsedCertificate::try_from(end_entity)?;
        let spki = cert.subject_public_key_info();
        let pin: String = digest(&SHA256, spki.as_ref())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        if pin == self.spki {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(format!(
                "pinned {}, got {pin}",
                self.spki
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Err(TlsError::General("TLS 1.3 only".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        let algorithms = &self.provider.signature_verification_algorithms;
        verify_tls13_signature(message, cert, signature, algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        let algorithms = &self.provider.signature_verification_algorithms;
        algorithms.supported_schemes()
    }
}

/// A device's end of one connection, keeping every byte it received.
struct Device {
    stream: AsyncBufReader<TlsStream<TcpStream>>,
    office: RemoteOffice,
    received: String,
}

impl Device {
    async fn connect(link: &PairLink) -> Self {
        let provider = Arc::new(default_provider());
        let verifier = PinnedTo {
            spki: link.spki.clone(),
            provider: default_provider(),
        };
        let config = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&TLS13])
            .expect("TLS 1.3 is on offer")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        let tcp = TcpStream::connect(("127.0.0.1", link.port))
            .await
            .expect("the server listens");
        let name = ServerName::try_from("pixtuoid.local").expect("a name");
        let tls = TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await
            .expect("the handshake passes the link's pin");
        Self {
            stream: AsyncBufReader::new(tls),
            office: RemoteOffice::new([4; MAX_FLOORS]),
            received: String::new(),
        }
    }

    async fn say(&mut self, value: &impl Serialize) {
        let line = serde_json::to_string(value).expect("it encodes") + "\n";
        let stream = self.stream.get_mut();
        stream.write_all(line.as_bytes()).await.expect("sent");
        stream.flush().await.expect("sent");
    }

    async fn hello(&mut self, hello: &Hello) -> Reply {
        self.say(hello).await;
        serde_json::from_str(&self.line().await).expect("a reply")
    }

    /// Ask, and read past the office lines to the answer.
    async fn ask(&mut self, ask: &Ask) -> Answer {
        self.say(ask).await;
        let wait = async {
            loop {
                let line = self.line().await;
                assert!(!line.is_empty(), "the stream ended, awaiting an answer");
                if let Some(answer) = Answer::from_line(line.as_bytes()) {
                    return answer;
                }
            }
        };
        tokio::time::timeout(PATIENCE, wait)
            .await
            .expect("an answer in time")
    }

    async fn line(&mut self) -> String {
        let mut line = String::new();
        self.stream.read_line(&mut line).await.expect("it reads");
        self.received.push_str(&line);
        line
    }

    /// Read lines until `repo` is at a desk.
    async fn sees(&mut self, repo: &str) {
        let wait = async {
            loop {
                let line = self.line().await;
                assert!(!line.is_empty(), "the stream ended, awaiting {repo}");
                self.office
                    .apply(line.as_bytes(), SystemTime::now())
                    .expect("a line");
                let cwds = self.office.scene().agents.values();
                if cwds.map(|a| &a.cwd).any(|cwd| cwd.ends_with(repo)) {
                    return;
                }
            }
        };
        tokio::time::timeout(PATIENCE, wait)
            .await
            .unwrap_or_else(|_| panic!("{repo} never reached the device"));
    }
}

/// A HOME of its own, where cursor is connected, and one hook endpoint.
struct World {
    home: tempfile::TempDir,
    env: SandboxEnv,
    endpoint: Endpoint,
}

impl World {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        let endpoint = Endpoint::in_dir(home.path(), "hook");
        let mut env = SandboxEnv::new(home.path(), [], &[]).expect("a sandbox");
        // Where `just coverage` collects a child's profile.
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            env.set("LLVM_PROFILE_FILE", profile);
        }
        let world = Self {
            home,
            env,
            endpoint,
        };
        // The hook path it installs is one nothing here runs.
        let connect = world
            .pixtuoid()
            .args(["connect", "cursor"])
            .env("PIXTUOID_HOOK", env!("CARGO_BIN_EXE_pixtuoid"))
            .output()
            .expect("it runs");
        assert!(connect.status.success(), "{connect:?}");
        world
    }

    fn pixtuoid(&self) -> Command {
        self.env.command(env!("CARGO_BIN_EXE_pixtuoid"))
    }

    fn run(&self, args: &[&str]) -> Output {
        self.pixtuoid().args(args).output().expect("it runs")
    }

    /// Start a long-lived instance, capturing its stdout line by line.
    fn spawn(&self, args: &[&str]) -> Instance {
        let socket = self.endpoint.path().to_str().expect("a UTF-8 path");
        let mut child = self
            .pixtuoid()
            .args(args)
            .args(["--socket", socket])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("it starts");
        let stdout = child.stdout.take().expect("piped");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                sink.lock().expect("not poisoned").push(line);
            }
        });
        Instance { child, lines }
    }

    fn serve(&self) -> (Instance, u16) {
        let server = self.spawn(&["serve", "--port", "0"]);
        let line = server.line_with("listening on");
        let port = line
            .split(';')
            .next()
            .and_then(|addr| addr.rsplit(':').next())
            .and_then(|port| port.parse().ok())
            .unwrap_or_else(|| panic!("no port in {line:?}"));
        (server, port)
    }

    fn headless(&self) -> Instance {
        self.spawn(&["run", "--headless"])
    }

    fn pair(&self, port: u16) -> PairLink {
        let args = ["pair", "--host", "127.0.0.1", "--port", &port.to_string()];
        let out = self.run(&args);
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout)
            .expect("UTF-8")
            .lines()
            .find(|line| line.starts_with("pixtuoid://"))
            .expect("pair prints the link")
            .parse()
            .expect("a link")
    }

    /// Keep starting sessions until `sees` says one landed: an instance taking
    /// the plane over is out of reach for a moment, which a shim shrugs off.
    async fn start_session<F: Future<Output = ()>>(&self, repo: &str, sees: F) {
        let line = format!(
            concat!(
                r#"{{"session_id":"s-{}","hook_event_name":"sessionStart","#,
                r#""workspace_roots":["/canary-parent/{}"],"transcript_path":null,"#,
                r#""_pixtuoid_source":"cursor"}}"#,
                "\n"
            ),
            repo, repo
        );
        let mut sees = std::pin::pin!(sees);
        let deadline = Instant::now() + PATIENCE;
        loop {
            let _ = self.endpoint.send(line.as_bytes()).await;
            let tick = tokio::time::timeout(Duration::from_millis(500), sees.as_mut());
            if tick.await.is_ok() {
                return;
            }
            assert!(Instant::now() < deadline, "{repo} never landed");
        }
    }
}

struct Instance {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Instance {
    fn line_with(&self, needle: &str) -> String {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let lines = self.lines.lock().expect("not poisoned");
            if let Some(line) = lines.iter().find(|l| l.contains(needle)) {
                return line.clone();
            }
            drop(lines);
            assert!(Instant::now() < deadline, "no line with {needle:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn said(&self, needle: &str) -> bool {
        let lines = self.lines.lock().expect("not poisoned");
        lines.iter().any(|l| l.contains(needle))
    }

    async fn says(&self, needle: &str) {
        while !self.said(needle) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_pairs_and_reads_an_office_that_names_only_the_repo() {
    let world = World::new();
    let (_server, port) = world.serve();
    let link = world.pair(port);

    let mut device = Device::connect(&link).await;
    let code = link.code.clone();
    let Reply::Paired { token } = device.hello(&Hello::Pair { code }).await else {
        panic!("a fresh code pairs");
    };
    world
        .start_session("canary-repo", device.sees("canary-repo"))
        .await;
    assert!(device.received.contains("canary-repo"));
    for private in ["canary-parent", "s-canary-repo", "workspace_roots"] {
        assert!(
            !device.received.contains(private),
            "{private} left the machine: {}",
            device.received
        );
    }

    let spent = Hello::Pair {
        code: link.code.clone(),
    };
    assert_eq!(
        Device::connect(&link).await.hello(&spent).await,
        Reply::Refused
    );

    let resume = Hello::Resume { token };
    let mut again = Device::connect(&link).await;
    assert_eq!(again.hello(&resume).await, Reply::Resumed);
    again.sees("canary-repo").await;

    let mut ended = String::new();
    let stale = device.stream.read_to_string(&mut ended);
    tokio::time::timeout(PATIENCE, stale)
        .await
        .expect("a device's new connection ends its old one")
        .expect("with a clean close");

    let reset = world.run(&["pair", "--reset"]);
    assert!(reset.status.success(), "{reset:?}");
    assert_eq!(
        Device::connect(&link).await.hello(&resume).await,
        Reply::Refused,
        "a running server reads a reset made beside it"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_lists_the_agent_clis_with_no_path_and_keeps_its_stream() {
    let world = World::new();
    let (_server, port) = world.serve();
    let link = world.pair(port);
    let mut device = Device::connect(&link).await;
    let code = link.code.clone();
    let reply = device.hello(&Hello::Pair { code }).await;
    assert!(matches!(reply, Reply::Paired { .. }), "{reply:?}");

    let Answer::Sources { rows } = device.ask(&Ask::Sources).await else {
        panic!("a server knows this ask");
    };
    let cursor = rows.iter().find(|row| row.id() == "cursor");
    let cursor = cursor.expect("every registered source has a row");
    assert_eq!(cursor.display_name(), "Cursor CLI");
    assert_eq!(cursor.state(), SourceState::Connected);

    world.start_session("canary", device.sees("canary")).await;
    let home = world.home.path().to_str().expect("a UTF-8 path");
    assert!(!device.received.contains(home), "{}", device.received);
}

/// No TUI is needed: the server owns the plane alone, and either instance can
/// leave without the other losing it.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_shares_the_hook_plane_in_either_order_and_survives_either_exit() {
    let world = World::new();
    let (server, port) = world.serve();
    let link = world.pair(port);
    let mut device = Device::connect(&link).await;
    let code = link.code.clone();
    let Reply::Paired { token } = device.hello(&Hello::Pair { code }).await else {
        panic!("a fresh code pairs");
    };
    world.start_session("alone", device.sees("alone")).await;

    let late_tui = world.headless();
    world.start_session("beside", late_tui.says("beside")).await;
    device.sees("beside").await;

    drop(server);
    world
        .start_session("took-over", late_tui.says("took-over"))
        .await;

    let (_server, port) = world.serve();
    let link = PairLink { port, ..link };
    let mut device = Device::connect(&link).await;
    let resume = Hello::Resume { token };
    assert_eq!(device.hello(&resume).await, Reply::Resumed);
    world
        .start_session("subscribed", device.sees("subscribed"))
        .await;

    drop(late_tui);
    world
        .start_session("owner-again", device.sees("owner-again"))
        .await;
}

/// A connection the cap turned away ends at once, and a stranger's slot comes
/// back when it leaves.
#[tokio::test(flavor = "multi_thread")]
async fn connections_are_capped_and_a_slot_comes_back() {
    let world = World::new();
    let (_server, port) = world.serve();
    let link = world.pair(port);
    let connect = || TcpStream::connect(("127.0.0.1", port));

    let mut silent = Vec::new();
    for _ in 0..pixtuoid::dev::MAX_CONNS {
        silent.push(connect().await.expect("the server listens"));
    }
    // Past every accept above, or the next could overtake one of them.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut refused = connect().await.expect("the backlog takes it");
    let mut unread = [0; 1];
    let closed = tokio::time::timeout(AT_ONCE, refused.read(&mut unread)).await;
    assert!(
        matches!(closed, Ok(Ok(0) | Err(_))),
        "over the cap is closed unanswered: {closed:?}"
    );

    drop(silent);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let hello = Hello::Pair {
        code: link.code.clone(),
    };
    let reply = Device::connect(&link).await.hello(&hello).await;
    assert!(matches!(reply, Reply::Paired { .. }), "{reply:?}");
}

#[test]
fn a_second_server_on_a_port_says_so_and_leaves() {
    let world = World::new();
    let (_first, port) = world.serve();
    let socket = world.endpoint.path().to_str().expect("a UTF-8 path");
    let second = world.run(&["serve", "--port", &port.to_string(), "--socket", socket]);
    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("already running"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_cant_go_in_a_link_leaves_the_printed_code_good() {
    let world = World::new();
    let (_server, port) = world.serve();
    let link = world.pair(port);

    let refused = world.run(&["pair", "--host", "evil.example/path"]);
    assert!(!refused.status.success());

    let code = link.code.clone();
    let reply = Device::connect(&link)
        .await
        .hello(&Hello::Pair { code })
        .await;
    assert!(matches!(reply, Reply::Paired { .. }), "{reply:?}");
}
