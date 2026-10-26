//! The macOS watch anchor: a [`notify::Watcher`] of single directories over
//! kqueue `EVFILT_VNODE`, opening nothing but each watched directory itself.

use std::collections::HashMap;
use std::fs::File;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
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

pub(super) struct KqueueDirWatcher {
    shared: Arc<Shared>,
}

struct Shared {
    /// Declared before `dirs`, so it closes first: rustix's `kevent` wants
    /// every fd a kqueue refers to valid for the kqueue's lifetime.
    kq: WakeableKqueue,
    dirs: Mutex<HashMap<PathBuf, File>>,
    closed: AtomicBool,
}

/// A poisoned lock means another thread panicked mid-insert of a map entry,
/// which leaves the map sound, so take the guard anyway.
fn dirs(shared: &Shared) -> MutexGuard<'_, HashMap<PathBuf, File>> {
    shared.dirs.lock().unwrap_or_else(PoisonError::into_inner)
}

fn knote(dir: &File, flags: EventFlags) -> Event {
    Event::new(
        EventFilter::Vnode {
            vnode: dir.as_raw_fd(),
            flags: WATCHED,
        },
        flags,
        ptr::null_mut(),
    )
}

/// Deletes `dir`'s knote before closing it, so the kqueue never refers to a
/// closed fd.
fn forget(kq: &WakeableKqueue, dir: File) {
    let eventlist: &mut [Event] = &mut [];
    // SAFETY: `dir` is open until this returns; afterwards the kqueue no
    // longer refers to it.
    let _ = unsafe {
        kevent(
            kq,
            &[knote(&dir, EventFlags::DELETE)],
            eventlist,
            Some(Duration::ZERO),
        )
    };
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
        for event in events.iter() {
            // The wake slot is the only other filter.
            let EventFilter::Vnode { vnode, flags } = event.filter() else {
                continue;
            };
            let path = dirs(shared)
                .iter()
                .find(|(_, dir)| dir.as_raw_fd() == vnode)
                .map(|(path, _)| path.clone());
            if let Some(path) = path {
                handler.handle_event(Ok(notify::Event::new(kind_of(flags)).add_path(path)));
            }
        }
    }
}

impl Watcher for KqueueDirWatcher {
    fn new<F: EventHandler>(mut handler: F, _config: Config) -> notify::Result<Self> {
        let kq = WakeableKqueue::new().map_err(|e| notify::Error::io(e.into()))?;
        let shared = Arc::new(Shared {
            kq,
            dirs: Mutex::default(),
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
        if recursive_mode == RecursiveMode::Recursive {
            return Err(
                notify::Error::generic("a kqueue directory watch is non-recursive")
                    .add_path(path.to_path_buf()),
            );
        }
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(notify::Error::generic("the kqueue watch thread has exited"));
        }
        let dir = super::open_for_events(path)
            .map_err(|e| notify::Error::io_watch(e).add_path(path.to_path_buf()))?;
        let eventlist: &mut [Event] = &mut [];
        // SAFETY: `dir` stays open in `dirs` until `forget` deletes this knote.
        unsafe {
            kevent(
                &self.shared.kq,
                &[knote(&dir, EventFlags::ADD | EventFlags::CLEAR)],
                eventlist,
                Some(Duration::ZERO),
            )
        }
        .map_err(|e| notify::Error::io(e.into()).add_path(path.to_path_buf()))?;
        let replaced = dirs(&self.shared).insert(path.to_path_buf(), dir);
        if let Some(old) = replaced {
            forget(&self.shared.kq, old);
        }
        Ok(())
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        let dir = dirs(&self.shared)
            .remove(path)
            .ok_or_else(|| notify::Error::watch_not_found().add_path(path.to_path_buf()))?;
        forget(&self.shared.kq, dir);
        Ok(())
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
    fn an_unwatched_dir_goes_quiet_and_recursion_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut watcher, rx) = watcher();
        assert!(watcher.watch(dir.path(), RecursiveMode::Recursive).is_err());
        watcher
            .watch(dir.path(), RecursiveMode::NonRecursive)
            .unwrap();
        watcher.unwatch(dir.path()).unwrap();
        assert!(
            watcher.unwatch(dir.path()).is_err(),
            "nothing left to unwatch"
        );
        std::fs::create_dir(dir.path().join("child")).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
