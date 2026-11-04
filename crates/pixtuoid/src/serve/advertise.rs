//! The office announced on the local network as a DNS-SD service (RFC 6763),
//! through the OS's own mDNS responder: one per host, as RFC 6762 §15.4 has it,
//! so `serve` binds no multicast socket. A device finds the server with no host
//! in hand and pins what it finds by the TXT record's `spki`, as it pins a QR.

#[cfg(target_os = "macos")]
#[path = "advertise/macos.rs"]
mod imp;

#[cfg(windows)]
#[path = "advertise/windows.rs"]
mod imp;

#[cfg(all(unix, not(target_os = "macos")))]
#[path = "advertise/avahi.rs"]
mod imp;

use anyhow::Result;

/// The service type a device browses for: RFC 6335 §5.1 caps the name at 15
/// letters, digits and hyphens.
const SERVICE: &str = "_pixtuoid._tcp";

/// RFC 6763 §4.1.1: "DNS labels are currently limited to 63 octets".
const MAX_INSTANCE_BYTES: usize = 63;

/// The TXT record's one key, the SHA-256 of the server's SPKI a device checks
/// before it trusts a server it found (RFC 6763 §6.4: SHOULD be at most nine
/// characters).
const SPKI_KEY: &str = "spki";

/// The service, announced until dropped.
#[derive(Debug)]
pub(super) struct Advertised {
    _registration: imp::Registration,
}

/// Announce the office on `port` as `name`, pinned by `spki`.
///
/// # Errors
///
/// If the OS has no responder to announce through, or it refuses the service.
pub(super) async fn advertise(name: &str, port: u16, spki: &str) -> Result<Advertised> {
    let registration = imp::register(&instance(name), port, &txt(spki)).await?;
    Ok(Advertised {
        _registration: registration,
    })
}

/// `name` cut to an instance label on a character boundary, or the crate's name
/// when nothing is left of it.
fn instance(name: &str) -> String {
    let mut end = name.len().min(MAX_INSTANCE_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    match name[..end].trim() {
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
    use super::*;

    #[test]
    fn an_instance_is_one_label_cut_on_a_character() {
        assert_eq!(instance("studio"), "studio");
        let long = "é".repeat(40);
        let cut = instance(&long);
        assert!(cut.len() <= MAX_INSTANCE_BYTES, "{}", cut.len());
        assert_eq!(cut, "é".repeat(MAX_INSTANCE_BYTES / 2));
        assert_eq!(instance("  "), env!("CARGO_PKG_NAME"));
        assert_eq!(instance(""), env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn the_service_and_its_key_are_names_dns_sd_takes() {
        let name = SERVICE
            .strip_prefix('_')
            .and_then(|rest| rest.strip_suffix("._tcp"))
            .expect("a TCP service");
        assert!((1..=15).contains(&name.len()), "{name}");
        assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
        assert!(SPKI_KEY.len() <= 9 && SPKI_KEY.is_ascii());
        assert!(
            u8::try_from(txt(&"A".repeat(64)).len()).is_ok(),
            "one TXT string"
        );
    }
}
