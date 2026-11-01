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
use pixtuoid_core::remote::{PairLink, is_host};
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

/// In a range IANA leaves unassigned, and fixed so that a pairing outlives a
/// restart.
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
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(cfg.socket, cfg.connected, port))
}

async fn serve(
    socket: Option<PathBuf>,
    connected: std::collections::HashSet<String>,
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
    let Pipeline { mut health_rx, .. } = spawn_pipeline(
        socket.unwrap_or_else(ClaudeCodeSource::default_socket_path),
        ConnectedSources::new(connected),
        capacities,
        sink,
    );

    let mut out = crate::cli_stdout();
    writeln!(
        out,
        "listening on {}; `pixtuoid pair` lets a device in",
        listener.local_addr()?
    )?;

    let devices = Devices::new(store, scene);
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
/// range can be judged before TLS, so this keeps out the internet and a Tailscale
/// tailnet's `100.64/10`; within the ranges, pairing is the gate. Bound to IPv4
/// only, so there is no v6 peer to judge.
fn lan_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(_) => false,
    }
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
    let link = PairLink {
        host,
        port,
        spki: spki_pin(&store.key()?),
        code: store.issue_code(SystemTime::now())?,
    }
    .to_string();
    let minutes = CODE_LIFETIME.as_secs() / 60;
    writeln!(out, "{}", qr(&link)?)?;
    writeln!(out, "Scan within {minutes} minutes, or enter:\n{link}")?;
    Ok(())
}

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

#[cfg(not(target_os = "macos"))]
fn local_host() -> Result<String> {
    anyhow::bail!("pass --host: the name or IPv4 address a device reaches this machine at")
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
        };
        let qr = qr(&link.to_string()).expect("it encodes");
        let rows = qr.lines().count();
        assert!(rows <= LAPTOP_TERMINAL_ROWS, "{rows}");
    }
}
