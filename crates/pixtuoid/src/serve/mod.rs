//! `pixtuoid serve`, the one listener on a network, and `pixtuoid pair`, which
//! lets a device in. A server is the headless pipeline, hook owner or subscriber
//! like any instance, with its scene leaving as [`pixtuoid_core::remote`] lines
//! instead of being painted.

mod device;
mod store;

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use pixtuoid_core::remote::{Machine, Os, PairLink, is_host};
use pixtuoid_core::source::claude_code::ClaudeCodeSource;
use pixtuoid_core::state::MAX_FLOORS;
use qrcode::render::unicode::Dense1x2;
use qrcode::{EcLevel, QrCode};
use rcgen::{CertificateParams, KeyPair};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::crypto::ring::default_provider;
use tokio_rustls::rustls::pki_types::PrivatePkcs8KeyDer;
use tokio_rustls::rustls::{ServerConfig, version::TLS13};

use crate::runtime::pipeline::{Pipeline, SceneSink, spawn_pipeline};
use crate::runtime::{
    ConnectedSources, FALLBACK_DESKS, QuitArms, QuitSignal, RunConfig, format_source_death,
    unseen_deaths,
};
use device::Devices;
use store::{CODE_LIFETIME, Store, spki_pin};

/// In a range [IANA leaves unassigned](https://www.iana.org/assignments/service-names-port-numbers/service-names-port-numbers.csv),
/// and fixed so that a pairing outlives a restart.
pub(crate) const DEFAULT_PORT: u16 = 47_820;

/// Connections at once, a stranger's handshake included.
pub const MAX_CONNS: usize = 8;

/// The pause after a failed `accept`, whose usual cause (out of descriptors)
/// would otherwise spin.
const ACCEPT_BACKOFF: Duration = Duration::from_secs(1);

/// Serve until a quit signal, which the caller re-raises.
///
/// # Errors
///
/// If the port is taken or the pairing state is unreadable.
pub(crate) fn run(cfg: RunConfig, port: u16) -> Result<QuitSignal> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let RunConfig {
        socket,
        config_path,
        connected,
        drift,
        ..
    } = cfg;
    run_then_quit(runtime, serve(socket, config_path, connected, drift, port))
}

/// How long a quit waits on blocking work: past a store write, short of a CLI's
/// run. A device's write it abandons leaves whole files for the next connect to heal.
const QUIT_GRACE: Duration = Duration::from_secs(1);

/// `work` on `runtime`, then at most [`QUIT_GRACE`] for its blocking work,
/// which a dropped runtime would wait for without end
/// (<https://docs.rs/tokio/latest/tokio/runtime/struct.Runtime.html#shutdown>).
fn run_then_quit<F: Future>(runtime: tokio::runtime::Runtime, work: F) -> F::Output {
    let done = runtime.block_on(work);
    runtime.shutdown_timeout(QUIT_GRACE);
    done
}

async fn serve(
    socket: Option<PathBuf>,
    config_path: PathBuf,
    connected: std::collections::HashSet<String>,
    drift: crate::doctor::DriftSeen,
    port: u16,
) -> Result<QuitSignal> {
    let store = Store::in_state_dir()?;
    let acceptor = tls_acceptor(&store.key()?)?;
    // Before the pipeline: a second server fails here, with the hook plane untouched.
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
        .await
        .with_context(|| {
            format!("could not listen on port {port}; is a server already running?")
        })?;
    let mut quit = pin!(QuitArms::arm().signalled());

    let capacities = [FALLBACK_DESKS; MAX_FLOORS];
    let (sink, scene) = SceneSink::latest(capacities);
    let connected = ConnectedSources::new(connected);
    let Pipeline { mut health_rx, .. } = spawn_pipeline(
        socket.unwrap_or_else(ClaudeCodeSource::default_socket_path),
        connected.clone(),
        capacities,
        sink,
    );

    let mut out = crate::cli_stdout();
    writeln!(
        out,
        "listening on {}; `pixtuoid pair` lets a device in",
        listener.local_addr()?
    )?;

    let deaths = health_rx.clone();
    let live = connected.clone();
    let rows = Arc::new(move || {
        // Out of the watch first: reading files under its read lock holds the writer.
        let deaths = deaths.borrow().clone();
        crate::sources::device_rows(&connected.snapshot(), &deaths, &drift)
    });
    let change = Arc::new(move |who: &str, source: &str, on: bool| {
        crate::sources::device_change(&config_path, &live, who, source, on)
    });
    let machine = Machine::new(&machine_name(), Os::here());
    let devices = Devices::new(store, machine, scene, rows, change);
    let slots = Arc::new(Semaphore::new(MAX_CONNS));
    let mut deaths_seen = 0;
    let mut accept_failing = false;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((tcp, peer)) => {
                    accept_failing = false;
                    if !lan_peer(peer.ip()) {
                        tracing::debug!(peer = ?peer, "refused a peer outside the LAN");
                        continue;
                    }
                    let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
                        tracing::debug!(peer = ?peer, "refused a peer over the connection cap");
                        continue;
                    };
                    let devices = devices.clone();
                    let acceptor = acceptor.clone();
                    tokio::spawn(async move {
                        let _slot = slot;
                        if let Err(error) = devices.serve(tcp, acceptor).await {
                            tracing::debug!(peer = ?peer, error = ?error, "connection ended");
                        }
                    });
                }
                Err(error) => {
                    if !std::mem::replace(&mut accept_failing, true) {
                        tracing::warn!(error = %error, "accept failing (retrying)");
                    }
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            Ok(()) = health_rx.changed() => {
                let deaths = health_rx.borrow_and_update().clone();
                for death in unseen_deaths(&deaths, &mut deaths_seen) {
                    // A full log disk is no reason to end every device's stream.
                    let _ = writeln!(out, "{}", format_source_death(death));
                }
            }
            signal = &mut quit => return Ok(signal),
        }
    }
}

/// Whether `ip` is in a local range: private, link-local or loopback. Only a
/// range can be judged before TLS, so this keeps out the internet and a tailnet's
/// [`100.64/10`](https://tailscale.com/kb/1015/100.x-addresses); within the
/// ranges, pairing is the gate. Bound to IPv4 only, so there is no v6 peer to
/// judge.
fn lan_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => on_a_local_network(v4) || v4.is_loopback(),
        IpAddr::V6(_) => false,
    }
}

/// Whether `ip` is in a private or link-local range.
fn on_a_local_network(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local()
}

/// What this machine's owner named it, the name a device shows: the OS's own
/// (macOS's ComputerName, Linux's pretty hostname, Windows' DNS host name), else
/// the hostname, as machine-info(5) has an application fall back.
fn machine_name() -> String {
    whoami::devicename()
        .or_else(|_| whoami::hostname())
        .unwrap_or_default()
}

/// A cert minted from `key` per run: its dates and name mean nothing to a device
/// that checks only the pin.
fn tls_acceptor(key: &KeyPair) -> Result<TlsAcceptor> {
    let cert = CertificateParams::new(["pixtuoid.local".to_owned()])?.self_signed(key)?;
    let config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_protocol_versions(&[&TLS13])?
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Print the QR that pairs a device, or with `reset` forget every device.
///
/// # Errors
///
/// If `host` can't go in a link, or the pairing state is unreadable.
pub(crate) fn pair(host: Option<String>, port: u16, reset: bool) -> Result<()> {
    let store = Store::in_state_dir()?;
    let mut out = crate::cli_stdout();
    if reset {
        store.reset()?;
        writeln!(out, "every device is forgotten")?;
        return Ok(());
    }
    let host = host.map_or_else(local_host, Ok)?;
    // Before the code: a typo mustn't cost the owner a code they already printed.
    anyhow::ensure!(is_host(&host), "--host takes a hostname or an IPv4 address");
    let by_address = host.parse::<Ipv4Addr>().is_ok();
    let issued = store.issue_code(SystemTime::now())?;
    let link = PairLink {
        host,
        port,
        spki: spki_pin(&store.key()?),
        code: issued.code,
        expires: issued.expires,
    }
    .to_string();
    let minutes = CODE_LIFETIME.as_secs() / 60;
    writeln!(out, "{}", qr(&link)?)?;
    writeln!(out, "Scan within {minutes} minutes, or enter:\n{link}")?;
    if by_address {
        writeln!(out, "{ADDRESS_CAN_MOVE}")?;
    }
    writeln!(out, "{CONSENT}")?;
    Ok(())
}

/// Said when a link pairs by IPv4: a DHCP server only "attempts" to hand a
/// client its address back (RFC 2131 §2.2), and a device can't follow it.
const ADDRESS_CAN_MOVE: &str = "This address can change with your network: pass --host \
                                with a name for one that doesn't.";

/// What pairing lets a device do, said where the owner pairs it.
const CONSENT: &str = "A paired device sees your agents and can connect or disconnect \
                       agent CLIs here; `pixtuoid pair --reset` forgets every device.";

/// `link` as a QR in dark-on-light whatever the terminal's theme, the polarity
/// scanners expect. Level L: it is read off a screen, with nothing to damage it,
/// and every level higher makes it taller.
fn qr(link: &str) -> Result<String> {
    let rows = QrCode::with_error_correction_level(link, EcLevel::L)?
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .build();
    Ok(rows
        .lines()
        .map(|row| format!("\u{1b}[97;40m{row}\u{1b}[0m"))
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(target_os = "macos")]
fn local_host() -> Result<String> {
    let out = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "LocalHostName"])
        .output()
        .context("could not ask scutil for this Mac's name")?;
    anyhow::ensure!(
        out.status.success(),
        "this Mac has no local name: pass --host"
    );
    Ok(format!(
        "{}.local",
        String::from_utf8_lossy(&out.stdout).trim()
    ))
}

/// The default route's private or link-local IPv4, which a device reaches with
/// no name service to answer for this machine: the source address of a
/// datagram to TEST-NET-1, which no route but the default covers (RFC 5737 §4:
/// "These blocks are not for local use").
#[cfg(not(target_os = "macos"))]
fn local_host() -> Result<String> {
    source_address_toward(TEST_NET_1)
}

#[cfg(any(test, not(target_os = "macos")))]
const TEST_NET_1: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);

/// The private or link-local IPv4 this machine would send to `target` from. A
/// UDP `connect` sends nothing; it sets "the address to which datagrams are
/// sent by default" (connect(2)).
#[cfg(any(test, not(target_os = "macos")))]
fn source_address_toward(target: Ipv4Addr) -> Result<String> {
    const PASS_HOST: &str =
        "pass --host: the name or IPv4 address a device reaches this machine at";
    let probe = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).context(PASS_HOST)?;
    probe
        .connect((target, 1))
        .with_context(|| format!("{PASS_HOST} (no route)"))?;
    // Winsock may leave the address unset until I/O, which reads as 0.0.0.0
    // (`getsockname`) and fails the range check like any other non-LAN address.
    match probe.local_addr()?.ip() {
        IpAddr::V4(ip) if on_a_local_network(ip) => Ok(ip.to_string()),
        _ => anyhow::bail!("{PASS_HOST} (the default route has no local-network address)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_local_network_is_served() {
        for ip in [
            "10.0.0.2",
            "172.16.0.2",
            "192.168.1.2",
            "169.254.0.2",
            "127.0.0.1",
        ] {
            assert!(lan_peer(ip.parse().expect("an IP")), "{ip}");
        }
        for ip in [
            "100.64.0.1",
            "100.127.255.254",
            "172.32.0.2",
            "8.8.8.8",
            "::1",
            "fe80::1",
            "fd00::1",
        ] {
            assert!(!lan_peer(ip.parse().expect("an IP")), "{ip}");
        }
    }

    /// A default host is one a device on the same network can be sent to: no
    /// loopback, which `lan_peer` still serves.
    #[test]
    fn a_default_host_is_private_or_link_local() {
        for ip in ["10.0.0.2", "172.16.0.2", "192.168.1.2", "169.254.0.2"] {
            assert!(on_a_local_network(ip.parse().expect("an IP")), "{ip}");
        }
        for ip in [
            "127.0.0.1",
            "0.0.0.0",
            "100.64.0.1",
            "172.32.0.2",
            "8.8.8.8",
        ] {
            assert!(!on_a_local_network(ip.parse().expect("an IP")), "{ip}");
        }
    }

    /// A probe answers only with a local-network address: toward loopback with
    /// none, toward TEST-NET-1 with one or none, whatever the runner's network.
    #[test]
    fn a_probed_host_is_only_ever_on_a_local_network() {
        assert!(source_address_toward(Ipv4Addr::LOCALHOST).is_err());
        if let Ok(host) = source_address_toward(TEST_NET_1) {
            assert!(on_a_local_network(host.parse().expect("an IPv4")), "{host}");
        }
    }

    /// A read still running when a quit comes is left behind, not waited for.
    #[test]
    fn a_quit_leaves_blocking_work_behind() {
        const HELD: Duration = Duration::from_secs(10);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .build()
            .expect("a runtime");
        let (_release, held) = std::sync::mpsc::channel::<()>();
        let started = std::time::Instant::now();
        run_then_quit(runtime, async {
            let (up, is_up) = tokio::sync::oneshot::channel();
            tokio::task::spawn_blocking(move || {
                let _ = up.send(());
                held.recv_timeout(HELD)
            });
            is_up.await.expect("it runs");
        });
        assert!(started.elapsed() < HELD / 2, "{:?}", started.elapsed());
    }

    #[test]
    fn the_acceptor_takes_the_stored_key() {
        tls_acceptor(&KeyPair::generate().expect("a key")).expect("it builds");
    }

    /// A QR has to be on screen whole to scan.
    #[test]
    fn a_long_hosts_qr_fits_a_laptops_terminal() {
        const LAPTOP_TERMINAL_ROWS: usize = 40;
        let link = PairLink {
            host: "a-rather-long-macbook-name.local".into(),
            port: DEFAULT_PORT,
            spki: "AB".repeat(32),
            code: "CD".repeat(32),
            expires: SystemTime::now() + CODE_LIFETIME,
        };
        let qr = qr(&link.to_string()).expect("it encodes");
        let rows = qr.lines().count();
        assert!(rows <= LAPTOP_TERMINAL_ROWS, "{rows}");
    }
}
