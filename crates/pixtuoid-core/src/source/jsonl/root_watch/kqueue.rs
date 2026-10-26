//! macOS's watcher of one directory's entries — every anchor, and a
//! non-recursive root — over kqueue `EVFILT_VNODE`, opening nothing but that
//! directory.

use std::fs::File;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use notify::event::{EventKind, ModifyKind, RemoveKind, RenameMode};
use notify::{Config, EventHandler, RecursiveMode, Watcher, WatcherKind};
use rustix::event::kqueue::{Event, EventFilter, EventFlags, VnodeEvents, kevent};
use rustix::io::Errno;

use crate::source::kqueue::{EVENT_BUF, WakeableKqueue};

/// The directory's entry list changing, or the directory itself going away.
const WATCHED: VnodeEvents = VnodeEvents::WRITE
    .union(VnodeEvents::LINK)
    .union(VnodeEvents::DELETE)
    .union(VnodeEvents::RENAME)
    .union(VnodeEvents::REVOKE);

/// Watches the one directory its single [`Watcher::watch`] names: a
/// `RootWatch` builds a watcher per watch.
pub(super) struct KqueueDirWatcher {
    shared: Arc<Shared>,
}

struct Shared {
    /// Declared before `dir`, so it closes first: rustix's `kevent` wants
    /// every fd a kqueue refers to valid for the kqueue's lifetime.
    kq: WakeableKqueue,
    dir: OnceLock<(PathBuf, File)>,
    closed: AtomicBool,
}

/// Opened `O_EVTONLY`, so the watch never blocks an unmount of its volume
/// (`open(2)`: xnu `bsd/man/man2/open.2:237-240`, tag `xnu-12377.121.6`).
fn open_for_events(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_EVTONLY)
        .open(path)
}

/// Adds or deletes the knote of `dir`, the fd `Shared` owns.
fn change_knote(kq: &WakeableKqueue, dir: RawFd, flags: EventFlags) -> notify::Result<()> {
    let knote = Event::new(
        EventFilter::Vnode {
            vnode: dir,
            flags: WATCHED,
        },
        flags,
        ptr::null_mut(),
    );
    let eventlist: &mut [Event] = &mut [];
    // SAFETY: `Shared` closes `dir` only after the kqueue.
    unsafe { kevent(kq, &[knote], eventlist, Some(Duration::ZERO)) }
        .map(|_| ())
        .map_err(|e| notify::Error::io(e.into()))
}

fn kind_of(flags: VnodeEvents) -> EventKind {
    if flags.intersects(VnodeEvents::DELETE | VnodeEvents::REVOKE) {
        EventKind::Remove(RemoveKind::Folder)
    } else if flags.contains(VnodeEvents::RENAME) {
        EventKind::Modify(ModifyKind::Name(RenameMode::Any))
    } else {
        // kqueue says only that an entry changed, not which.
        EventKind::Any
    }
}

fn run(shared: &Shared, handler: &mut impl EventHandler) {
    loop {
        let mut buf = [const { MaybeUninit::<Event>::uninit() }; EVENT_BUF];
        // SAFETY: the empty changelist references no fd; the `None` timeout
        // blocks until a vnode event or a wake, both queued kqueue state, so a
        // wake sent before this call is still seen.
        let events = match unsafe { kevent(&shared.kq, &[], &mut buf, None) } {
            Ok((events, _rest)) => events,
            Err(e) if e == Errno::INTR => continue,
            Err(e) => {
                handler.handle_event(Err(notify::Error::io(e.into())));
                return;
            }
        };
        if shared.closed.load(Ordering::SeqCst) {
            return;
        }
        let Some((path, _)) = shared.dir.get() else {
            continue;
        };
        for event in events.iter() {
            // The wake slot is the only other filter.
            if let EventFilter::Vnode { flags, .. } = event.filter() {
                handler.handle_event(Ok(notify::Event::new(kind_of(flags)).add_path(path.clone())));
            }
        }
    }
}

impl Watcher for KqueueDirWatcher {
    fn new<F: EventHandler>(mut handler: F, _config: Config) -> notify::Result<Self> {
        let kq = WakeableKqueue::new().map_err(|e| notify::Error::io(e.into()))?;
        let shared = Arc::new(Shared {
            kq,
            dir: OnceLock::new(),
            closed: AtomicBool::new(false),
        });
        let thread_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("pixtuoid-dir-watch".into())
            .spawn(move || {
                run(&thread_shared, &mut handler);
                // Every exit lands here, so `watch` refuses instead of
                // registering with a kqueue nothing reads.
                thread_shared.closed.store(true, Ordering::SeqCst);
            })
            .map_err(notify::Error::io)?;
        Ok(Self { shared })
    }

    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
        let refused = if recursive_mode == RecursiveMode::Recursive {
            Some("a kqueue directory watch is non-recursive")
        } else if self.shared.dir.get().is_some() {
            Some("a kqueue directory watcher watches one directory")
        } else if self.shared.closed.load(Ordering::SeqCst) {
            Some("the kqueue watch thread has exited")
        } else {
            None
        };
        if let Some(why) = refused {
            return Err(notify::Error::generic(why).add_path(path.to_path_buf()));
        }
        let dir = open_for_events(path)
            .map_err(|e| notify::Error::io_watch(e).add_path(path.to_path_buf()))?;
        let fd = dir.as_raw_fd();
        // Set before the knote is added, so its first event finds the path.
        // `&mut self` and the `get` above leave the cell empty.
        let _ = self.shared.dir.set((path.to_path_buf(), dir));
        change_knote(&self.shared.kq, fd, EventFlags::ADD | EventFlags::CLEAR)
            .map_err(|e| e.add_path(path.to_path_buf()))
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        match self.shared.dir.get() {
            Some((watched, dir)) if watched == path => {
                change_knote(&self.shared.kq, dir.as_raw_fd(), EventFlags::DELETE)
            }
            _ => Err(notify::Error::watch_not_found().add_path(path.to_path_buf())),
        }
    }

    fn kind() -> WatcherKind {
        WatcherKind::Kqueue
    }
}

impl Drop for KqueueDirWatcher {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        let _ = self.shared.kq.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEADLINE: Duration = Duration::from_secs(5);

    fn watcher() -> (KqueueDirWatcher, std::sync::mpsc::Receiver<notify::Event>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = KqueueDirWatcher::new(
            move |res: notify::Result<notify::Event>| {
                let _ = tx.send(res.expect("a healthy kqueue reports no error"));
            },
            Config::default(),
        )
        .unwrap();
        (watcher, rx)
    }

    #[test]
    fn reports_an_entry_change_then_its_own_removal() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path().join("anchor");
        std::fs::create_dir(&anchor).unwrap();
        let (mut watcher, rx) = watcher();
        watcher.watch(&anchor, RecursiveMode::NonRecursive).unwrap();

        std::fs::create_dir(anchor.join("child")).unwrap();
        let event = rx.recv_timeout(DEADLINE).expect("a new child is reported");
        assert_eq!(event.paths, vec![anchor.clone()]);
        assert_eq!(event.kind, EventKind::Any);

        std::fs::remove_dir(anchor.join("child")).unwrap();
        std::fs::remove_dir(&anchor).unwrap();
        let removed = std::iter::from_fn(|| rx.recv_timeout(DEADLINE).ok())
            .find(|e| matches!(e.kind, EventKind::Remove(_)));
        assert!(removed.is_some(), "the anchor's own removal is reported");
    }

    #[test]
    fn watches_one_directory_non_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let (mut watcher, _rx) = watcher();
        assert!(watcher.watch(dir.path(), RecursiveMode::Recursive).is_err());
        watcher
            .watch(dir.path(), RecursiveMode::NonRecursive)
            .unwrap();
        assert!(
            watcher
                .watch(dir.path(), RecursiveMode::NonRecursive)
                .is_err(),
            "a second watch is refused"
        );
    }
}
