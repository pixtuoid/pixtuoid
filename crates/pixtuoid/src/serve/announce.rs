//! The office announced on the local network as a DNS-SD service (RFC 6763),
//! through the OS's own mDNS responder: one per host, as RFC 6762 §15.4 has it,
//! so `serve` binds no multicast socket. What it announces is
//! [`SERVICE_TYPE`](pixtuoid_core::remote::SERVICE_TYPE)'s to describe.

#[cfg(target_os = "macos")]
#[path = "announce/macos.rs"]
mod imp;

#[cfg(windows)]
#[path = "announce/windows.rs"]
mod imp;

#[cfg(all(unix, not(target_os = "macos")))]
#[path = "announce/avahi.rs"]
mod imp;

use std::time::Duration;

use pixtuoid_core::remote::SPKI_KEY;
use tokio::task::JoinHandle;

/// RFC 6763 §4.1.1: "DNS labels are currently limited to 63 octets".
const MAX_INSTANCE_BYTES: usize = 63;

/// How long a responder that refused or dropped the service waits to be asked
/// again, a chosen value: long enough not to spin on a host with none, short
/// enough that a restarted daemon has the office back soon.
const RETRY: Duration = Duration::from_secs(30);

/// The office's announcement, kept up until dropped.
#[derive(Debug)]
pub(super) struct Announcement(JoinHandle<()>);

impl Drop for Announcement {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Announce the office on `port` as `name`, pinned by `spki`, and again each time
/// the responder drops it: a daemon's restart takes its services with it. The
/// QR pairs whether or not this ever succeeds.
pub(super) fn announce(name: &str, port: u16, spki: &str) -> Announcement {
    let instance = instance(name);
    let spki = spki.to_owned();
    Announcement(tokio::spawn(keep_up(async move || {
        imp::announce(&instance, port, &spki).await
    })))
}

/// Run `attempt` again a [`RETRY`] after each end, warning once per run of
/// failures.
async fn keep_up(mut attempt: impl AsyncFnMut() -> anyhow::Result<()>) {
    let mut warned = false;
    loop {
        match attempt().await {
            Ok(()) => {
                tracing::warn!("the mDNS responder dropped the office; announcing it again");
                warned = false;
            }
            Err(error) if !warned => {
                tracing::warn!(error = ?error, "could not announce the office on the local network");
                warned = true;
            }
            Err(_) => {}
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// `name` cut to an instance label on a character boundary, or the crate's name
/// when nothing is left of it.
fn instance(name: &str) -> String {
    match name[..name.floor_char_boundary(MAX_INSTANCE_BYTES)].trim() {
        "" => env!("CARGO_PKG_NAME").to_owned(),
        cut => cut.to_owned(),
    }
}

/// The TXT record's one string.
fn txt(spki: &str) -> String {
    format!("{SPKI_KEY}={spki}")
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use tokio::time::Instant;

    use super::*;

    /// A lost announcement and a refused one are each asked for again a
    /// [`RETRY`] later.
    #[tokio::test(start_paused = true)]
    async fn an_ended_attempt_is_tried_again_a_retry_later() {
        let start = Instant::now();
        let (tried, mut tries) = mpsc::unbounded_channel();
        let mut ends = vec![Ok(()), Err(anyhow::anyhow!("refused"))].into_iter();
        let task = tokio::spawn(keep_up(async move || {
            tried.send(Instant::now()).expect("the test listens");
            match ends.next() {
                Some(end) => end,
                None => std::future::pending().await,
            }
        }));
        for n in 0..3 {
            let at = tries.recv().await.expect("an attempt");
            assert_eq!(at - start, RETRY * n);
        }
        task.abort();
    }

    #[test]
    fn an_instance_is_one_label_cut_on_a_character() {
        assert_eq!(instance("studio"), "studio");
        let cut = instance(&"é".repeat(40));
        assert_eq!(cut, "é".repeat(MAX_INSTANCE_BYTES / 2));
        assert_eq!(instance("  "), env!("CARGO_PKG_NAME"));
        assert_eq!(instance(""), env!("CARGO_PKG_NAME"));
    }
}
