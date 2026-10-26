//! A kqueue whose blocked `kevent` wait another thread can end (the
//! `EVFILT_USER` slot the crate's kqueue threads are woken through), and the
//! `EVFILT_VNODE` watch set built on it.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rustix::event::kqueue::{
    Event, EventFilter, EventFlags, UserDefinedFlags, UserFlags, VnodeEvents, kevent, kqueue,
};
use rustix::io::Errno;

/// Per-wait event budget. Overflow just takes another loop turn — kqueue
/// events are queued state, never lost.
pub(crate) const EVENT_BUF: usize = 16;

/// The wake slot's ident — a pure key, carrying no fd/pid.
const WAKE_IDENT: isize = 0;

fn wake_filter(trigger: bool) -> EventFilter {
    EventFilter::User {
        ident: WAKE_IDENT,
        flags: if trigger {
            UserFlags::TRIGGER
        } else {
            UserFlags::empty()
        },
        user_flags: UserDefinedFlags::new(0),
    }
}

#[derive(Debug)]
pub(crate) struct WakeableKqueue {
    kq: OwnedFd,
}

impl WakeableKqueue {
    pub(crate) fn new() -> rustix::io::Result<Self> {
        let kq = kqueue()?;
        // EV_CLEAR so each delivered NOTE_TRIGGER auto-resets the event.
        let wake_slot = Event::new(
            wake_filter(false),
            EventFlags::ADD | EventFlags::CLEAR,
            ptr::null_mut(),
        );
        // An empty eventlist ⇒ nevents 0: the kernel processes the change
        // and writes nothing back.
        let eventlist: &mut [Event] = &mut [];
        // SAFETY: the EVFILT_USER change references no fd (`WAKE_IDENT` is a
        // pure user-event key), so the kqueue-fd-validity contract is
        // trivially met; the zero timeout means this can't block.
        unsafe { kevent(&kq, &[wake_slot], eventlist, Some(Duration::ZERO)) }?;
        Ok(Self { kq })
    }

    /// Trigger the wake slot, ending a blocked `kevent` wait. kevent(2) is
    /// thread-safe on one kq, so the waking side may call this while the
    /// waiting thread is blocked.
    pub(crate) fn wake(&self) -> rustix::io::Result<()> {
        let trigger = Event::new(wake_filter(true), EventFlags::empty(), ptr::null_mut());
        let eventlist: &mut [Event] = &mut [];
        // SAFETY: the trigger references no fd; the empty eventlist + zero
        // timeout write nothing and never block. Cannot ENOENT — the slot
        // is registered at construction and never deleted while the kq lives.
        unsafe { kevent(&self.kq, &[trigger], eventlist, Some(Duration::ZERO)) }.map(|_| ())
    }
}

impl AsFd for WakeableKqueue {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.kq.as_fd()
    }
}

/// Opened `O_EVTONLY`, so the watch never blocks an unmount of its volume
/// (`open(2)`: xnu `bsd/man/man2/open.2:237-240`, tag `xnu-12377.121.6`).
fn open_for_events(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_EVTONLY)
        .open(path)
}

/// What a [`VnodeWatch`]'s thread hands its sink.
#[derive(Debug)]
pub(crate) enum VnodeNote<'a> {
    Changed(&'a Path, VnodeEvents),
    /// The wait failed; the thread exits after this note.
    Failed(io::Error),
}

/// The watched paths, shared by a [`VnodeWatch`]'s owner and its sink, so a
/// sink can re-add a path whose vnode was replaced.
#[derive(Clone, Debug)]
pub(crate) struct Vnodes(Arc<Shared>);

#[derive(Debug)]
struct Shared {
    /// Declared before `files`, so it closes first: rustix's `kevent` wants
    /// every fd a kqueue refers to valid for the kqueue's lifetime.
    kq: WakeableKqueue,
    files: Mutex<Files>,
    closed: AtomicBool,
}

#[derive(Debug, Default)]
struct Files {
    /// By the knote's `udata`, not its fd: a released fd's number is reused
    /// process-wide, and a note still queued for it would name a stranger.
    by_token: HashMap<usize, (PathBuf, File)>,
    by_path: HashMap<PathBuf, usize>,
    next_token: usize,
}

/// Adds or deletes the knote of `fd`, tagged with `token`.
fn change_knote(
    kq: &WakeableKqueue,
    fd: RawFd,
    flags: VnodeEvents,
    action: EventFlags,
    token: usize,
) -> io::Result<()> {
    let knote = Event::new(
        EventFilter::Vnode { vnode: fd, flags },
        action,
        ptr::without_provenance_mut(token),
    );
    let eventlist: &mut [Event] = &mut [];
    // SAFETY: `fd` is open across this call, and no knote outlives its fd:
    // `Vnodes::remove` deletes it before the fd closes.
    unsafe { kevent(kq, &[knote], eventlist, Some(Duration::ZERO)) }
        .map(|_| ())
        .map_err(io::Error::from)
}

impl Vnodes {
    fn files(&self) -> MutexGuard<'_, Files> {
        self.0.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Watches `path` for `flags`; `Ok(false)` when it already is.
    pub(crate) fn add(&self, path: &Path, flags: VnodeEvents) -> io::Result<bool> {
        let mut files = self.files();
        if files.by_path.contains_key(path) {
            return Ok(false);
        }
        let file = open_for_events(path)?;
        let token = files.next_token;
        change_knote(
            &self.0.kq,
            file.as_raw_fd(),
            flags,
            EventFlags::ADD | EventFlags::CLEAR,
            token,
        )?;
        files.next_token += 1;
        files.by_path.insert(path.to_path_buf(), token);
        files.by_token.insert(token, (path.to_path_buf(), file));
        Ok(true)
    }

    /// Stops watching `path`; `false` when it was not watched.
    pub(crate) fn remove(&self, path: &Path) -> bool {
        let mut files = self.files();
        let Some(token) = files.by_path.remove(path) else {
            return false;
        };
        if let Some((_, file)) = files.by_token.remove(&token) {
            // A revoked vnode's one-shot knote is already gone (xnu
            // `bsd/vfs/vfs_vnops.c:2171-2172` at `f6217f89`), so this may
            // fail harmlessly.
            let _ = change_knote(
                &self.0.kq,
                file.as_raw_fd(),
                VnodeEvents::empty(),
                EventFlags::DELETE,
                token,
            );
        }
        true
    }

    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        self.files().by_path.keys().cloned().collect()
    }

    fn path_of(&self, token: usize) -> Option<PathBuf> {
        self.files()
            .by_token
            .get(&token)
            .map(|(path, _)| path.clone())
    }
}

/// A kqueue `EVFILT_VNODE` watch over a set of paths: one detached thread
/// waits on it and hands each note to a sink. Dropping it ends the thread.
#[derive(Debug)]
pub(crate) struct VnodeWatch {
    vnodes: Vnodes,
}

impl VnodeWatch {
    pub(crate) fn spawn<S>(name: &str, mut sink: S) -> io::Result<Self>
    where
        S: FnMut(&Vnodes, VnodeNote<'_>) + Send + 'static,
    {
        let vnodes = Vnodes(Arc::new(Shared {
            kq: WakeableKqueue::new()?,
            files: Mutex::default(),
            closed: AtomicBool::new(false),
        }));
        let thread_vnodes = vnodes.clone();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || run(&thread_vnodes, &mut sink))?;
        Ok(Self { vnodes })
    }

    pub(crate) fn vnodes(&self) -> &Vnodes {
        &self.vnodes
    }
}

impl Drop for VnodeWatch {
    fn drop(&mut self) {
        self.vnodes.0.closed.store(true, Ordering::SeqCst);
        let _ = self.vnodes.0.kq.wake();
    }
}

fn run(vnodes: &Vnodes, sink: &mut impl FnMut(&Vnodes, VnodeNote<'_>)) {
    loop {
        let mut buf = [const { MaybeUninit::<Event>::uninit() }; EVENT_BUF];
        // SAFETY: the empty changelist references no fd; the `None` timeout
        // blocks until a vnode note or a wake, both queued kqueue state, so a
        // wake sent before this call is still seen.
        let events = match unsafe { kevent(&vnodes.0.kq, &[], &mut buf, None) } {
            Ok((events, _rest)) => events,
            Err(e) if e == Errno::INTR => continue,
            Err(e) => {
                sink(vnodes, VnodeNote::Failed(e.into()));
                return;
            }
        };
        if vnodes.0.closed.load(Ordering::SeqCst) {
            return;
        }
        dispatch(vnodes, events, sink);
    }
}

fn dispatch(vnodes: &Vnodes, events: &[Event], sink: &mut impl FnMut(&Vnodes, VnodeNote<'_>)) {
    for event in events {
        // The wake slot is the only other filter.
        let EventFilter::Vnode { flags, .. } = event.filter() else {
            continue;
        };
        // Queued before its path was removed: nothing watches it now.
        let Some(path) = vnodes.path_of(event.udata().addr()) else {
            continue;
        };
        // Unlocked, so the sink may add or remove.
        sink(vnodes, VnodeNote::Changed(&path, flags));
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};

    use super::*;

    const DEADLINE: Duration = Duration::from_secs(5);
    /// Past any note the kernel would have sent.
    const QUIET: Duration = Duration::from_millis(300);

    type Notes = Receiver<(PathBuf, VnodeEvents)>;

    fn watch() -> (VnodeWatch, Notes) {
        let (tx, rx) = channel();
        let watch = VnodeWatch::spawn("pixtuoid-vnode-test", move |_, note| {
            if let VnodeNote::Changed(path, flags) = note {
                let _ = tx.send((path.to_path_buf(), flags));
            }
        })
        .unwrap();
        (watch, rx)
    }

    fn held(path: &Path) -> File {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
    }

    #[test]
    fn reports_each_write_to_a_held_open_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut file = held(&path);
        let (watch, notes) = watch();
        assert!(watch.vnodes().add(&path, VnodeEvents::WRITE).unwrap());
        for _ in 0..2 {
            file.write_all(b"{}\n").unwrap();
            let (got, flags) = notes
                .recv_timeout(DEADLINE)
                .expect("a held-open write is reported");
            assert_eq!((got.as_path(), flags), (path.as_path(), VnodeEvents::WRITE));
        }
    }

    #[test]
    fn a_removed_path_reports_nothing_and_can_be_added_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut file = held(&path);
        let (watch, notes) = watch();
        let vnodes = watch.vnodes();
        vnodes.add(&path, VnodeEvents::WRITE).unwrap();
        assert!(vnodes.remove(&path));
        file.write_all(b"{}\n").unwrap();
        assert_eq!(notes.recv_timeout(QUIET), Err(RecvTimeoutError::Timeout));
        assert!(vnodes.add(&path, VnodeEvents::WRITE).unwrap(), "re-added");
        file.write_all(b"{}\n").unwrap();
        assert!(
            notes.recv_timeout(DEADLINE).is_ok(),
            "the re-added path is watched"
        );
    }

    /// The fd a removed path held can be the next path's, so a note still
    /// queued for the old knote must not name the new path.
    #[test]
    fn a_note_queued_for_a_removed_path_names_no_other() {
        let dir = tempfile::tempdir().unwrap();
        let (watch, _notes) = watch();
        let vnodes = watch.vnodes();
        let old = dir.path().join("old.jsonl");
        let new = dir.path().join("new.jsonl");
        drop(held(&old));
        drop(held(&new));
        vnodes.add(&old, VnodeEvents::WRITE).unwrap();
        let old_token = vnodes.files().by_path[&old];
        vnodes.remove(&old);
        vnodes.add(&new, VnodeEvents::WRITE).unwrap();
        let new_fd = {
            let files = vnodes.files();
            files.by_token[&files.by_path[&new]].1.as_raw_fd()
        };
        let queued = Event::new(
            EventFilter::Vnode {
                vnode: new_fd,
                flags: VnodeEvents::WRITE,
            },
            EventFlags::empty(),
            ptr::without_provenance_mut(old_token),
        );
        let mut named = Vec::new();
        dispatch(vnodes, &[queued], &mut |_, note| {
            if let VnodeNote::Changed(path, _) = note {
                named.push(path.to_path_buf());
            }
        });
        assert_eq!(named, Vec::<PathBuf>::new());
    }

    #[test]
    fn dropping_the_watch_ends_its_thread() {
        let (watch, notes) = watch();
        drop(watch);
        // The sink, owned by the thread, drops its sender as the thread ends.
        assert_eq!(
            notes.recv_timeout(DEADLINE),
            Err(RecvTimeoutError::Disconnected)
        );
    }
}
