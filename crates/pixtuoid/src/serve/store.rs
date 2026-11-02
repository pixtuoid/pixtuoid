//! What a server keeps between runs, in one owner-only file: its TLS key, the
//! pairing code outstanding and the devices paired. Codes and tokens are held
//! hashed, so reading the file pairs nobody.
//!
//! State rather than config: a dotfiles repo must not collect a private key.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow};
use pixtuoid_core::remote::SECRET_BYTES;
use rcgen::{KeyPair, PublicKeyData};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::install::io::{lock_config, read_config};

/// How long `pixtuoid pair`'s code stays good.
pub(super) const CODE_LIFETIME: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key_pem: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<Pending>,
    #[serde(default)]
    devices: Vec<Hashed>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    hash: Hashed,
    expires: SystemTime,
}

impl Stored {
    fn outstanding(&self, code: &Hashed, now: SystemTime) -> bool {
        (self.code.as_ref()).is_some_and(|pending| pending.hash == *code && now < pending.expires)
    }
}

/// A code's or a token's hash, as a paired device is known. Compared in the open:
/// a timing leak gives away a digest's prefix, and no input can be chosen to
/// extend one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct Hashed(String);

impl Hashed {
    pub(super) fn of(token: &str) -> Self {
        Self(hex(digest(&SHA256, token.as_bytes()).as_ref()))
    }
}

/// The file behind [`Stored`]. A running server reads it per hello, so `pair`
/// works beside one, from another process.
#[derive(Debug, Clone)]
pub(super) struct Store(PathBuf);

impl Store {
    #[cfg(test)]
    pub(super) fn at(path: PathBuf) -> Self {
        Self(path)
    }

    pub(super) fn in_state_dir() -> Result<Self> {
        let dir =
            crate::config::state_dir().context("no home directory to keep the pairing state in")?;
        Ok(Self(dir.join("remote.json")))
    }

    /// An error rather than a fresh state, a missing file aside: a new key would
    /// break every device's pin with no word why.
    fn parse(&self, text: &str) -> Result<Stored> {
        if text.is_empty() && !self.0.exists() {
            return Ok(Stored::default());
        }
        serde_json::from_str(text).context("the pairing state is unreadable")
    }

    /// Unlocked: the write is an atomic rename, so a reader sees a whole file.
    fn read(&self) -> Result<Stored> {
        self.parse(&read_config(&self.0)?)
    }

    /// One locked read→change→write round.
    fn update<T>(&self, change: impl FnOnce(&mut Stored) -> Result<T>) -> Result<T> {
        let lock = lock_config(&self.0)?;
        let mut stored = self.parse(&lock.read()?)?;
        let out = change(&mut stored)?;
        lock.write_atomic(&serde_json::to_string_pretty(&stored)?)?;
        Ok(out)
    }

    /// The server's key, made on first use. It outlives `pair --reset`, since a
    /// running server holds it and a new one would break the pin a fresh link
    /// carries.
    pub(super) fn key(&self) -> Result<KeyPair> {
        self.update(|stored| {
            if let Some(pem) = &stored.key_pem {
                return KeyPair::from_pem(pem).context("the stored TLS key is unreadable");
            }
            let key = KeyPair::generate()?;
            stored.key_pem = Some(key.serialize_pem());
            Ok(key)
        })
    }

    /// A new code in place of any outstanding one.
    pub(super) fn issue_code(&self, now: SystemTime) -> Result<String> {
        let code = secret()?;
        self.update(|stored| {
            stored.code = Some(Pending {
                hash: Hashed::of(&code),
                expires: now + CODE_LIFETIME,
            });
            Ok(())
        })?;
        Ok(code)
    }

    /// Spend `code` on a new device, returning its token. A wrong code leaves the
    /// outstanding one alone, or a stranger could spend the owner's by guessing,
    /// and takes no lock, or a stranger's guesses could fail the owner's `pair`.
    pub(super) fn redeem(&self, code: &str, now: SystemTime) -> Result<Option<String>> {
        let presented = Hashed::of(code);
        if !self.read()?.outstanding(&presented, now) {
            return Ok(None);
        }
        let token = secret()?;
        self.update(|stored| {
            if !stored.outstanding(&presented, now) {
                return Ok(None);
            }
            stored.code = None;
            stored.devices.push(Hashed::of(&token));
            Ok(Some(token))
        })
    }

    pub(super) fn knows(&self, device: &Hashed) -> Result<bool> {
        Ok(self.read()?.devices.contains(device))
    }

    /// Forget every device and any outstanding code.
    pub(super) fn reset(&self) -> Result<()> {
        self.update(|stored| {
            stored.code = None;
            stored.devices.clear();
            Ok(())
        })
    }
}

/// The SHA-256 of `key`'s SPKI, which a device pins.
pub(super) fn spki_pin(key: &KeyPair) -> String {
    hex(digest(&SHA256, &key.subject_public_key_info()).as_ref())
}

fn secret() -> Result<String> {
    let mut bytes = [0; SECRET_BYTES];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow!("the system's random source failed"))?;
    Ok(hex(&bytes))
}

/// Uppercase, which a QR packs in its alphanumeric mode at 5.5 bits a character
/// rather than 8: a link's two digests decide how small its code can be.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    const NOW: Duration = Duration::from_secs(1_800_000_000);

    fn now() -> SystemTime {
        UNIX_EPOCH + NOW
    }

    fn store(dir: &tempfile::TempDir) -> Store {
        Store(dir.path().join("pixtuoid").join("remote.json"))
    }

    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(&dir);
        (dir, store)
    }

    fn disk(store: &Store) -> String {
        std::fs::read_to_string(&store.0).expect("the file exists")
    }

    #[test]
    fn the_key_a_device_pins_survives_a_restart_and_a_reset() {
        let (dir, first) = fixture();
        let pin = spki_pin(&first.key().expect("a new key"));
        first.reset().expect("it resets");
        let again = spki_pin(&store(&dir).key().expect("the same key"));
        assert_eq!(pin, again);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_holding_the_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, store) = fixture();
        store.key().expect("a new key");
        let mode = std::fs::metadata(&store.0)
            .expect("it exists")
            .permissions();
        assert_eq!(mode.mode() & 0o777, 0o600);
    }

    #[test]
    fn a_code_pairs_one_device_and_neither_secret_reaches_the_disk() {
        let (_dir, store) = fixture();
        let code = store.issue_code(now()).expect("a code");
        assert!(!disk(&store).contains(&code), "the code stays hashed");

        let token = store
            .redeem(&code, now())
            .expect("it redeems")
            .expect("a good code");
        assert!(!disk(&store).contains(&token), "the token stays hashed");
        assert!(store.knows(&Hashed::of(&token)).expect("it reads"));

        let again = store.redeem(&code, now()).expect("it redeems");
        assert_eq!(again, None, "a code is spent once");
    }

    #[test]
    fn a_code_is_good_until_its_lifetime_ends() {
        let (_dir, store) = fixture();
        let code = store.issue_code(now()).expect("a code");
        let end = now() + CODE_LIFETIME;
        assert_eq!(store.redeem(&code, end).expect("it redeems"), None);
        let in_time = end - Duration::from_millis(1);
        assert!(store.redeem(&code, in_time).expect("it redeems").is_some());
    }

    #[test]
    fn a_wrong_guess_leaves_the_owners_code_good() {
        let (_dir, store) = fixture();
        let code = store.issue_code(now()).expect("a code");
        assert_eq!(store.redeem("guess", now()).expect("it redeems"), None);
        assert!(store.redeem(&code, now()).expect("it redeems").is_some());
    }

    #[test]
    fn a_new_code_replaces_the_outstanding_one() {
        let (_dir, store) = fixture();
        let old = store.issue_code(now()).expect("a code");
        store.issue_code(now()).expect("a code");
        assert_eq!(store.redeem(&old, now()).expect("it redeems"), None);
    }

    #[test]
    fn a_reset_forgets_every_device_and_the_outstanding_code() {
        let (_dir, store) = fixture();
        let code = store.issue_code(now()).expect("a code");
        let token = store
            .redeem(&code, now())
            .expect("it redeems")
            .expect("a good code");
        let pending = store.issue_code(now()).expect("a code");

        store.reset().expect("it resets");

        assert!(!store.knows(&Hashed::of(&token)).expect("it reads"));
        assert_eq!(store.redeem(&pending, now()).expect("it redeems"), None);
    }

    /// A silent fresh start would mint a new key and break every device's pin
    /// with no word why.
    #[test]
    fn an_unreadable_file_is_an_error_not_a_new_identity() {
        let (_dir, store) = fixture();
        store.key().expect("a new key");
        for unreadable in ["{not json", ""] {
            std::fs::write(&store.0, unreadable).expect("it writes");
            assert!(store.key().is_err(), "{unreadable:?}");
            assert!(store.knows(&Hashed::of("t")).is_err(), "{unreadable:?}");
        }
    }

    #[test]
    fn nothing_paired_reads_as_nobody() {
        let (_dir, store) = fixture();
        assert!(
            !store
                .knows(&Hashed::of("t"))
                .expect("no file is no devices")
        );
    }

    #[test]
    fn a_strangers_guess_takes_no_lock() {
        let (_dir, store) = fixture();
        store.issue_code(now()).expect("a code");
        let _held = lock_config(&store.0).expect("the lock");
        assert_eq!(store.redeem("guess", now()).expect("it redeems"), None);
    }
}
