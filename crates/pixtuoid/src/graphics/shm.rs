//! Pixels handed to a terminal on this host through POSIX shared memory, the
//! kitty graphics protocol's `t=s` medium
//! (<https://sw.kovidgoyal.net/kitty/graphics-protocol/>, "The transmission
//! medium"): the terminal "must read the data from the memory object and then
//! unlink and close it on POSIX". An object the terminal never reads — its
//! escape dropped, or this process gone first — outlives the process, so the
//! [`Ledger`] holds every name until it is unlinked, by the terminal or by us.

use std::collections::VecDeque;
use std::io;
use std::os::fd::OwnedFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long an object waits for the terminal before we unlink it: a terminal
/// reads a frame's escapes long before the next frames are due, so one still
/// unread this late was dropped, and unlinking it costs at most a stale tile.
const READ_WITHIN: Duration = Duration::from_secs(2);

/// The most bytes the ledger holds at once, oldest unlinked first: a terminal
/// that never reads (a dropped passthrough) must not pile up [`READ_WITHIN`]
/// of whole frames. The terminal's own unlink goes unseen, so the ledger
/// counts read objects too: publishing more than this within [`READ_WITHIN`]
/// unlinks objects before the terminal has had that long to read them.
const MAX_LEDGER_BYTES: usize = 256 << 20;

/// The names this process has published and not yet unlinked itself, oldest
/// first.
#[derive(Debug, Default)]
struct Ledger {
    held: VecDeque<Held>,
    bytes: usize,
}

#[derive(Debug)]
struct Held {
    name: String,
    at: Instant,
    len: usize,
}

impl Ledger {
    /// Record `name`, then unlink what is past [`READ_WITHIN`] at `now` or
    /// over [`MAX_LEDGER_BYTES`].
    fn hold(&mut self, held: Held, now: Instant) {
        self.bytes += held.len;
        self.held.push_back(held);
        while let Some(oldest) = self.held.front() {
            let late = now.saturating_duration_since(oldest.at) > READ_WITHIN;
            if !late && self.bytes <= MAX_LEDGER_BYTES {
                break;
            }
            self.drop_oldest();
        }
    }

    fn drop_oldest(&mut self) {
        if let Some(held) = self.held.pop_front() {
            self.bytes -= held.len;
            unlink(&held.name);
        }
    }

    fn drain(&mut self) {
        while !self.held.is_empty() {
            self.drop_oldest();
        }
    }
}

static LEDGER: Mutex<Ledger> = Mutex::new(Ledger {
    held: VecDeque::new(),
    bytes: 0,
});

fn ledger() -> std::sync::MutexGuard<'static, Ledger> {
    LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// This process's names: `/pxt`, its pid (no live process shares it) and a
/// sequence number, within macOS's 31-byte limit on a name (`PSHMNAMLEN`).
/// A dead process's unread leftover under a reused pid fails the `O_EXCL`
/// open, and that tile takes the escapes.
fn next_name() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "/pxt{:x}-{:x}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// A new object holding `bytes`, and its name; held in the ledger until the
/// terminal or [`READ_WITHIN`] unlinks it.
pub(crate) fn publish(bytes: &[u8], now: Instant) -> io::Result<String> {
    let name = create(bytes)?;
    ledger().hold(
        Held {
            name: name.clone(),
            at: now,
            len: bytes.len(),
        },
        now,
    );
    Ok(name)
}

/// A new object holding `bytes`, held by no ledger.
fn create(bytes: &[u8]) -> io::Result<String> {
    use rustix::shm::{Mode, OFlags};
    let name = next_name();
    let fd = rustix::shm::open(
        name.as_str(),
        OFlags::CREATE | OFlags::EXCL | OFlags::RDWR,
        Mode::RUSR | Mode::WUSR,
    )?;
    if let Err(e) = fill(&fd, bytes) {
        unlink(&name);
        return Err(e);
    }
    Ok(name)
}

/// Size the object to `bytes` and copy them in. An object takes no
/// `write(2)` on macOS, so through a mapping.
fn fill(fd: &OwnedFd, bytes: &[u8]) -> io::Result<()> {
    use rustix::mm::{MapFlags, ProtFlags};
    rustix::fs::ftruncate(fd, u64::try_from(bytes.len()).map_err(io::Error::other)?)?;
    // SAFETY: maps `bytes.len()` bytes of the object just sized to that length.
    let map = unsafe {
        rustix::mm::mmap(
            std::ptr::null_mut(),
            bytes.len(),
            ProtFlags::WRITE,
            MapFlags::SHARED,
            fd,
            0,
        )
    }?;
    // SAFETY: `map` is a fresh writable mapping of `bytes.len()` bytes that
    // cannot overlap `bytes`; it is unmapped once, here.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), map.cast::<u8>(), bytes.len());
        let _ = rustix::mm::munmap(map, bytes.len());
    }
    Ok(())
}

/// Unlink `name`; one the terminal already unlinked is no error.
fn unlink(name: &str) {
    let _ = rustix::shm::unlink(name);
}

/// Unlink every object this process still holds: at teardown, and in the
/// unwind, so none outlives a quit [`QuitArms`](crate::runtime::QuitArms) catches.
pub(crate) fn unlink_all() {
    ledger().drain();
}

/// What a terminal does with `name`: its first `len` bytes, then the object
/// unlinked; `None` when there is no such object.
#[cfg(test)]
pub(crate) fn read_and_unlink(name: &str, len: usize) -> Option<Vec<u8>> {
    let mut bytes = tests::read(name)?;
    unlink(name);
    bytes.truncate(len);
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The object's bytes as a reader on this host sees them, `None` once
    /// it is unlinked.
    pub(super) fn read(name: &str) -> Option<Vec<u8>> {
        let fd = rustix::shm::open(
            name,
            rustix::shm::OFlags::RDONLY,
            rustix::shm::Mode::empty(),
        )
        .ok()?;
        let file = std::fs::File::from(fd);
        let len = usize::try_from(file.metadata().ok()?.len()).ok()?;
        if len == 0 {
            return Some(Vec::new());
        }
        // SAFETY: maps the object's whole length read-only.
        let map = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                rustix::mm::ProtFlags::READ,
                rustix::mm::MapFlags::SHARED,
                &file,
                0,
            )
        }
        .expect("mapped");
        // SAFETY: `map` holds `len` readable bytes until the unmap below.
        let out = unsafe { std::slice::from_raw_parts(map.cast::<u8>(), len) }.to_vec();
        // SAFETY: unmaps the mapping made above, once.
        unsafe { rustix::mm::munmap(map, len) }.expect("unmapped");
        Some(out)
    }

    #[test]
    fn a_published_object_holds_its_bytes_under_a_fresh_name() {
        let now = Instant::now();
        let a = publish(b"rgbrgb", now).expect("published");
        let b = publish(b"x", now).expect("published");
        assert_ne!(a, b);
        assert!(
            a.starts_with('/') && !a[1..].contains('/') && a.len() <= 31,
            "{a}"
        );
        // The stat size is a page multiple on some systems; the bytes lead it.
        assert!(read(&a).expect("readable").starts_with(b"rgbrgb"));
        unlink(&a);
        unlink(&b);
        assert_eq!(read(&a), None);
    }

    /// The ledger unlinks what the terminal leaves past its deadline, or past
    /// the byte cap, oldest first, and an unlink of an object the terminal
    /// already unlinked is no error.
    #[test]
    fn the_ledger_unlinks_what_is_left_unread() {
        let mut ledger = Ledger::default();
        let t0 = Instant::now();
        let first = create(b"a").expect("created");
        let second = create(b"b").expect("created");
        unlink(&second);
        for name in [&first, &second] {
            ledger.hold(
                Held {
                    name: name.clone(),
                    at: t0,
                    len: 1,
                },
                t0,
            );
        }
        assert!(read(&first).is_some(), "unlinked before its deadline");
        let third = next_name();
        ledger.hold(
            Held {
                name: third,
                at: t0 + READ_WITHIN * 2,
                len: 1,
            },
            t0 + READ_WITHIN * 2,
        );
        assert_eq!(read(&first), None, "left past its deadline");
        assert_eq!(ledger.held.len(), 1);
        let huge = Held {
            name: next_name(),
            at: t0 + READ_WITHIN * 2,
            len: MAX_LEDGER_BYTES + 1,
        };
        ledger.hold(huge, t0 + READ_WITHIN * 2);
        assert!(ledger.held.is_empty() && ledger.bytes == 0, "over the cap");
    }

    /// An object is late only PAST [`READ_WITHIN`]: at the deadline the
    /// terminal has had exactly its window, and still keeps it.
    #[test]
    fn an_object_is_late_only_past_its_deadline() {
        let mut ledger = Ledger::default();
        let t0 = Instant::now();
        let mut hold_at = |at| {
            let name = next_name();
            ledger.hold(Held { name, at, len: 1 }, at);
            ledger.held.len()
        };
        hold_at(t0);
        assert_eq!(hold_at(t0 + READ_WITHIN), 2, "at the deadline");
        let past = t0 + READ_WITHIN + Duration::from_nanos(1);
        assert_eq!(hold_at(past), 2, "past it, the first goes");
    }

    #[test]
    fn a_drained_ledger_leaves_nothing_behind() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let name = create(b"left").expect("created");
        ledger.hold(
            Held {
                name: name.clone(),
                at: now,
                len: 4,
            },
            now,
        );
        ledger.drain();
        assert_eq!(read(&name), None);
    }
}
