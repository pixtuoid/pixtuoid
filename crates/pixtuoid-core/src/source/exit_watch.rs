//! Instant process-exit detection: the kernel reports the moment a probed
//! agent process dies, instead of the negative vouch's ~60–120s or the
//! reducer's TTL/stale sweeps.
//!
//! One `ExitWatch` = one kernel wait primitive + ONE detached `std::thread`
//! blocked on it (macOS: a `kqueue` with `EVFILT_PROC NOTE_EXIT` knotes;
//! Linux: `poll` over `pidfd_open` descriptors plus a self-pipe; Windows:
//! `WaitForMultipleObjects` over process handles plus a wake event). NOT
//! `tokio::spawn_blocking` — a forever-loop would pin a blocking-pool slot for
//! the process lifetime. The primitive's fds live in the shared `Arc` state, so
//! the waker side can never touch a recycled fd after the thread closes up, and
//! every thread-exit path marks `closed` so `watch()` rejects instead of
//! queueing onto a queue nothing will ever drain.
//!
//! Contract (workspace invariant: log + continue, never panic): every failure
//! path — backend init, registration, thread death — degrades to the slower
//! exit rungs. A missing instant exit costs latency, never correctness. A pid
//! that died BEFORE registration is detected at registration (ESRCH; on Windows
//! ERROR_INVALID_PARAMETER) and its exit synthesized immediately.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::mpsc::UnboundedSender;

/// Whether this platform compiled a backend, so `ExitWatch::spawn` returns
/// `Some` barring an init failure. Tests assert against it rather than
/// restating the platform list the `imp` arms own.
#[cfg(test)]
pub(crate) const HAS_BACKEND: bool = imp::AVAILABLE;

#[derive(Debug)]
pub(crate) struct ExitWatch {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    pending: Mutex<Vec<i32>>,
    closed: AtomicBool,
    backend: imp::Backend,
}

/// A poisoned lock only means another thread panicked mid-push of an `i32` —
/// the Vec is still structurally sound, so take the guard anyway.
fn lock_pending(shared: &Shared) -> std::sync::MutexGuard<'_, Vec<i32>> {
    shared
        .pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Take everything `watch()` queued since the last wake, DEDUPED. The dedup is
/// load-bearing for the AlreadyDead path: a duplicate `watch()` of a pid that
/// died before the drain would otherwise register → ESRCH → send, then
/// re-register the batch's second copy and synthesize a SECOND exit.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn drain_pending(shared: &Shared) -> Vec<i32> {
    let mut batch = std::mem::take(&mut *lock_pending(shared));
    batch.sort_unstable();
    batch.dedup();
    batch
}

impl ExitWatch {
    /// Spawns the watcher thread. `exit_tx` receives the pid of every watched
    /// process that exits (or was already dead at registration). `None` when
    /// the platform backend failed to init or has no validated primitive —
    /// callers treat that as "instant exit off".
    pub(crate) fn spawn(exit_tx: UnboundedSender<i32>) -> Option<Self> {
        let backend = imp::Backend::init()?;
        let shared = Arc::new(Shared {
            pending: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            backend,
        });
        let thread_shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("pixtuoid-exit-watch".into())
            .spawn(move || {
                imp::run(&thread_shared, &exit_tx);
                // EVERY run() exit lands here: mark the handle dead so `watch()`
                // rejects rather than queueing — producers push per hook event, and
                // with no drainer `pending` grows unboundedly.
                thread_shared.closed.store(true, Ordering::SeqCst);
                lock_pending(&thread_shared).clear();
            });
        if let Err(e) = spawned {
            tracing::debug!(
                error = %e,
                "exit-watch thread spawn failed; instant exit off (backstops cover)"
            );
            return None;
        }
        Some(Self { shared })
    }

    /// Ask the thread to watch `pid`. Idempotent while `pid` is watched; a watch
    /// registered after its death is answered with an exit of its own, which a
    /// binding re-armed on a dead pid waits for (`pid_watch`'s `note`). Once the
    /// thread has died the pid is rejected outright — nothing would ever drain
    /// it, so pushing would only grow `pending`.
    pub(crate) fn watch(&self, pid: i32) {
        if self.shared.closed.load(Ordering::SeqCst) {
            return;
        }
        lock_pending(&self.shared).push(pid);
        self.shared.backend.wake();
    }
}

impl Drop for ExitWatch {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.backend.wake();
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::HashSet;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
    use std::ptr;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use rustix::event::kqueue::{
        Event, EventFilter, EventFlags, ProcessEvents, UserDefinedFlags, UserFlags, kevent, kqueue,
    };
    use rustix::io::Errno;
    use rustix::process::Pid;
    use tokio::sync::mpsc::UnboundedSender;

    use super::{Shared, drain_pending};

    /// Per-wait event budget. Overflow just takes another loop turn — kqueue
    /// events are queued state, never lost.
    const EVENT_BUF: usize = 16;

    /// The `EVFILT_USER` wake slot ident — a pure key, carrying no fd/pid.
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

    #[cfg(test)]
    pub(super) const AVAILABLE: bool = true;

    #[derive(Debug)]
    pub(super) struct Backend {
        /// One kqueue for the whole watch. Owned HERE (reached by both sides
        /// through `Arc<Shared>`) so the fd closes only when both the waker and
        /// the thread are gone — `wake()` can never hit a recycled fd (tokio's
        /// own selector is a kqueue; triggering a user event on a recycled fd
        /// would corrupt a foreign event loop).
        kq: OwnedFd,
    }

    impl Backend {
        pub(super) fn init() -> Option<Self> {
            let kq = match kqueue() {
                Ok(kq) => kq,
                Err(e) => {
                    tracing::debug!(error = %e, "kqueue() failed; instant exit off (backstops cover)");
                    return None;
                }
            };
            // EV_CLEAR so each delivered NOTE_TRIGGER auto-resets the event.
            let wake_slot = Event::new(
                wake_filter(false),
                EventFlags::ADD | EventFlags::CLEAR,
                ptr::null_mut(),
            );
            // An empty eventlist ⇒ nevents 0: the kernel processes the change
            // and writes nothing back.
            let eventlist: &mut [Event] = &mut [];
            // SAFETY: the EVFILT_USER change references no fd (ident 0 is a pure
            // user-event key), so the kqueue-fd-validity contract is trivially
            // met; the zero timeout means this can't block.
            let registered = unsafe { kevent(&kq, &[wake_slot], eventlist, Some(Duration::ZERO)) };
            if let Err(e) = registered {
                tracing::debug!(error = %e, "EVFILT_USER wake-slot registration failed; instant exit off");
                return None;
            }
            Some(Self { kq })
        }

        /// Trigger the EVFILT_USER ident-0 slot, waking a blocked `kevent`.
        /// kevent(2) is thread-safe on one kq, so the registering side may call
        /// this while the thread is blocked.
        pub(super) fn wake(&self) {
            let trigger = Event::new(wake_filter(true), EventFlags::empty(), ptr::null_mut());
            let eventlist: &mut [Event] = &mut [];
            // SAFETY: the trigger references no fd; the empty eventlist + zero
            // timeout write nothing and never block. Cannot ENOENT — the slot
            // is registered at init and never deleted while the kq lives.
            if let Err(e) = unsafe { kevent(&self.kq, &[trigger], eventlist, Some(Duration::ZERO)) }
            {
                tracing::debug!(error = %e, "exit-watch wake trigger failed");
            }
        }
    }

    enum Registered {
        Ok,
        /// The pid died before registration (ESRCH receipt) — the caller
        /// synthesizes the exit immediately.
        AlreadyDead,
        /// EPERM / anything else: dropped (logged); backstops cover.
        Failed,
    }

    /// Register a NOTE_EXIT knote for `pid`, resolving the registration race
    /// via EV_RECEIPT: the receipt entry comes back in the SAME call (flags
    /// carry EV_ERROR; data is 0 on success, the errno otherwise), so a pid
    /// that died before this call is detected HERE rather than silently never
    /// firing. NOTE_EXITSTATUS is deliberately NOT requested — xnu's
    /// filt_procattach has no credential check for plain NOTE_EXIT, so
    /// same-user non-children (every CC/codex session) are watchable
    /// unprivileged.
    fn register(kq: BorrowedFd, pid: i32) -> Registered {
        let Some(rpid) = Pid::from_raw(pid) else {
            return Registered::Failed;
        };
        let change = Event::new(
            EventFilter::Proc {
                pid: rpid,
                flags: ProcessEvents::EXIT,
            },
            // EV_ONESHOT: NOTE_EXIT can only fire once per process; the knote
            // self-removes on delivery, so no post-fire cleanup exists.
            EventFlags::ADD | EventFlags::ONESHOT | EventFlags::RECEIPT,
            ptr::null_mut(),
        );
        // EV_RECEIPT guarantees the receipt fills this slot ahead of any
        // pending event.
        let mut slot = [const { MaybeUninit::<Event>::uninit() }; 1];
        // SAFETY: the EVFILT_PROC change references a pid, not an fd, so the
        // kqueue-fd-validity contract is trivially met; the zero timeout can
        // never block the loop.
        let receipt = match unsafe { kevent(kq, &[change], &mut slot, Some(Duration::ZERO)) } {
            Ok((got, _rest)) => got,
            Err(e) => {
                tracing::debug!(
                    pid,
                    error = %e,
                    "EVFILT_PROC registration failed; dropped (backstops cover)"
                );
                return Registered::Failed;
            }
        };
        let Some(ev) = receipt.first() else {
            return Registered::Ok;
        };
        let data = ev.data();
        if !ev.flags().contains(EventFlags::ERROR) || data == 0 {
            return Registered::Ok;
        }
        let errno = Errno::from_raw_os_error(data as i32);
        if errno == Errno::SRCH {
            Registered::AlreadyDead
        } else if errno == Errno::PERM {
            tracing::debug!(pid, "EVFILT_PROC EPERM; dropped (backstops cover)");
            Registered::Failed
        } else {
            tracing::debug!(pid, error = %errno, "EVFILT_PROC registration failed; dropped");
            Registered::Failed
        }
    }

    pub(super) fn run(shared: &Shared, exit_tx: &UnboundedSender<i32>) {
        let kq = shared.backend.kq.as_fd();
        let mut watched: HashSet<i32> = HashSet::new();
        loop {
            let mut buf = [const { MaybeUninit::<Event>::uninit() }; EVENT_BUF];
            // SAFETY: the empty changelist references no fds; the None timeout
            // blocks until a NOTE_EXIT delivery or a NOTE_TRIGGER wake (both are
            // queued kqueue state, so a wake sent before this call is still
            // picked up — no lost wakeups).
            let events = match unsafe { kevent(kq, &[], &mut buf, None) } {
                Ok((events, _rest)) => events,
                Err(e) if e == Errno::INTR => continue,
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "exit-watch kevent wait failed; thread exiting (backstops cover)"
                    );
                    return;
                }
            };
            if shared.closed.load(Ordering::SeqCst) {
                return;
            }
            // Drain pending BEFORE processing exit deliveries: a duplicate
            // watch() batched alongside the pid's NOTE_EXIT must dedup against
            // the still-watched entry — events-first would remove the entry,
            // then re-register the duplicate against a dead pid and synthesize
            // a SECOND exit.
            for pid in drain_pending(shared) {
                if !watched.insert(pid) {
                    continue; // idempotent: already watched
                }
                match register(kq, pid) {
                    Registered::Ok => {}
                    Registered::AlreadyDead => {
                        watched.remove(&pid);
                        if exit_tx.send(pid).is_err() {
                            return; // receiver gone — the watcher task ended
                        }
                    }
                    Registered::Failed => {
                        watched.remove(&pid);
                    }
                }
            }
            for ev in events.iter() {
                if let EventFilter::Proc { pid, flags } = ev.filter()
                    && flags.contains(ProcessEvents::EXIT)
                {
                    let pid = pid.as_raw_pid();
                    // The knote already self-removed (EV_ONESHOT); drop our
                    // bookkeeping so a recycled pid can be re-watched.
                    watched.remove(&pid);
                    if exit_tx.send(pid).is_err() {
                        return;
                    }
                }
                // EVFILT_USER ident 0 is the wake — the drain above handles it.
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
    use std::sync::atomic::Ordering;

    use rustix::event::{PollFd, PollFlags, poll};
    use rustix::io::Errno;
    use rustix::pipe::{PipeFlags, pipe_with};
    use rustix::process::{Pid, PidfdFlags, pidfd_open};
    use tokio::sync::mpsc::UnboundedSender;

    use super::{Shared, drain_pending};

    #[cfg(test)]
    pub(super) const AVAILABLE: bool = true;

    #[derive(Debug)]
    pub(super) struct Backend {
        /// Self-pipe: `wake()` writes a byte; the thread keeps the read end in
        /// its poll set and drains on wake. Both ends are O_NONBLOCK — a full
        /// pipe means a wake is already pending (EAGAIN is success). Owned HERE
        /// (reached through `Arc<Shared>`) so neither side can ever touch a
        /// recycled fd.
        pipe_rd: OwnedFd,
        pipe_wr: OwnedFd,
    }

    impl Backend {
        pub(super) fn init() -> Option<Self> {
            let (pipe_rd, pipe_wr) = match pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK) {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::debug!(error = %e, "pipe2 failed; instant exit off (backstops cover)");
                    return None;
                }
            };
            Some(Self { pipe_rd, pipe_wr })
        }

        pub(super) fn wake(&self) {
            let _ = rustix::io::write(&self.pipe_wr, &[1]);
        }
    }

    enum PidfdOpen {
        Opened(OwnedFd),
        /// ESRCH — the pid died (and was reaped) before registration; the
        /// caller synthesizes the exit immediately.
        AlreadyDead,
        /// ENOSYS — pre-5.3 kernel; the whole backend is unavailable.
        Unsupported,
        /// Anything else: dropped (logged); backstops cover.
        Failed,
    }

    fn pidfd_open_for(pid: i32) -> PidfdOpen {
        let Some(rpid) = Pid::from_raw(pid) else {
            return PidfdOpen::Failed;
        };
        match pidfd_open(rpid, PidfdFlags::empty()) {
            Ok(fd) => PidfdOpen::Opened(fd),
            Err(e) if e == Errno::SRCH => PidfdOpen::AlreadyDead,
            Err(e) if e == Errno::NOSYS => PidfdOpen::Unsupported,
            Err(e) => {
                tracing::debug!(pid, error = %e, "pidfd_open failed; dropped (backstops cover)");
                PidfdOpen::Failed
            }
        }
    }

    /// Empty the self-pipe so the next wake byte makes it readable again.
    fn drain_pipe(pipe_rd: BorrowedFd) {
        let mut buf = [0u8; 64];
        loop {
            // O_NONBLOCK so this never blocks; a short read means the pipe is
            // now empty.
            match rustix::io::read(pipe_rd, &mut buf) {
                Ok(n) if n == buf.len() => continue,
                _ => return,
            }
        }
    }

    pub(super) fn run(shared: &Shared, exit_tx: &UnboundedSender<i32>) {
        let pipe_rd = shared.backend.pipe_rd.as_fd();
        // pid → its pidfd, in poll-set order: `fds[0]` is always the pipe,
        // `fds[i+1]` is `watched[i]` — pushes only append, so the zip below
        // stays aligned with the fds snapshot taken before any mutation.
        let mut watched: Vec<(i32, OwnedFd)> = Vec::new();
        loop {
            // Re-check `closed` BEFORE blocking in poll(). `Drop` sets `closed`
            // then writes a wake byte, but `drain_pipe` below drains the pipe
            // unconditionally — a Drop landing after the post-poll check but
            // before that drain has its wake byte EATEN, and the thread would
            // re-enter poll() and block on a still-live watched pid forever
            // (the self-pipe lost-wakeup that flaked `drop_stops_cleanly` on
            // Linux CI). SeqCst pairs with Drop's store-before-wake.
            if shared.closed.load(Ordering::SeqCst) {
                return;
            }
            let mut fds: Vec<PollFd> = Vec::with_capacity(watched.len() + 1);
            fds.push(PollFd::from_borrowed_fd(pipe_rd, PollFlags::IN));
            for (_, pidfd) in &watched {
                fds.push(PollFd::from_borrowed_fd(pidfd.as_fd(), PollFlags::IN));
            }
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(e) if e == Errno::INTR => continue,
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "exit-watch poll failed; thread exiting (backstops cover)"
                    );
                    return;
                }
            }
            if shared.closed.load(Ordering::SeqCst) {
                return;
            }
            // A pidfd polls POLLIN when the process exits (zombie) and POLLHUP
            // once reaped; ERR/NVAL are defensive — all four mean "this watch
            // is over".
            let pipe_woke = !fds[0].revents().is_empty();
            let exited: Vec<i32> = fds[1..]
                .iter()
                .zip(watched.iter())
                .filter(|(slot, _)| {
                    slot.revents().intersects(
                        PollFlags::IN | PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL,
                    )
                })
                .map(|(_, (pid, _))| *pid)
                .collect();
            // `fds` borrows `watched`; drop it before mutating the set below.
            drop(fds);
            // Drain pending BEFORE removing/sending the exits: a duplicate
            // watch() arriving in the same wake as its pid's exit must dedup
            // against the still-watched entry — exits-first would remove it,
            // then pidfd_open the duplicate on a dead pid and synthesize a
            // SECOND exit.
            if pipe_woke {
                drain_pipe(pipe_rd);
                for pid in drain_pending(shared) {
                    if watched.iter().any(|(p, _)| *p == pid) {
                        continue; // idempotent: already watched
                    }
                    match pidfd_open_for(pid) {
                        PidfdOpen::Opened(pidfd) => watched.push((pid, pidfd)),
                        PidfdOpen::AlreadyDead => {
                            if exit_tx.send(pid).is_err() {
                                return; // receiver gone — the watcher task ended
                            }
                        }
                        PidfdOpen::Unsupported => {
                            tracing::debug!(
                                "pidfd_open ENOSYS (pre-5.3 kernel); instant exit off (backstops cover)"
                            );
                            return;
                        }
                        PidfdOpen::Failed => {}
                    }
                }
            }
            for pid in &exited {
                // The pid leaves the set so it can be re-watched if recycled.
                watched.retain(|(p, _)| p != pid);
                if exit_tx.send(*pid).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::atomic::Ordering;

    use tokio::sync::mpsc::UnboundedSender;
    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_PARAMETER, FALSE, WAIT_FAILED, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::SystemServices::MAXIMUM_WAIT_OBJECTS;
    use windows_sys::Win32::System::Threading::{
        CreateEventW, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, SetEvent, WaitForMultipleObjects,
    };

    use super::{Shared, drain_pending};

    #[cfg(test)]
    pub(super) const AVAILABLE: bool = true;

    #[derive(Debug)]
    pub(super) struct Backend {
        /// Auto-reset: a `wake()` that lands while the thread isn't waiting
        /// stays set until its next wait consumes it, so unlike a drained
        /// self-pipe no wake is ever lost.
        wake: OwnedHandle,
    }

    impl Backend {
        pub(super) fn init() -> Option<Self> {
            // SAFETY: null attributes and name, by-value flags; the null check
            // screens the handle.
            let event = unsafe { CreateEventW(std::ptr::null(), FALSE, FALSE, std::ptr::null()) };
            if event.is_null() {
                tracing::debug!(
                    error = %std::io::Error::last_os_error(),
                    "CreateEventW failed; instant exit off (backstops cover)"
                );
                return None;
            }
            // SAFETY: a fresh handle nothing else owns.
            let wake = unsafe { OwnedHandle::from_raw_handle(event) };
            Some(Self { wake })
        }

        pub(super) fn wake(&self) {
            // SAFETY: the event lives as long as `self`.
            unsafe { SetEvent(self.wake.as_raw_handle()) };
        }
    }

    enum Opened {
        Handle(OwnedHandle),
        /// No process holds the pid any more; the caller synthesizes the exit.
        /// A pid that exited while something still holds its handle opens fine
        /// and is waited on as already signaled.
        AlreadyDead,
        /// Anything else, such as access denied: dropped (logged); backstops cover.
        Failed,
    }

    fn open(pid: i32) -> Opened {
        let Ok(id) = u32::try_from(pid) else {
            return Opened::Failed;
        };
        // SAFETY: by-value arguments; the null check screens the handle.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, FALSE, id) };
        if !handle.is_null() {
            // SAFETY: a fresh handle nothing else owns.
            return Opened::Handle(unsafe { OwnedHandle::from_raw_handle(handle) });
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == i32::try_from(ERROR_INVALID_PARAMETER).ok() {
            return Opened::AlreadyDead;
        }
        tracing::debug!(pid, error = %error, "OpenProcess failed; dropped (backstops cover)");
        Opened::Failed
    }

    /// Processes one wait holds beside the wake event; a pid past it is dropped
    /// (logged) to the backstops, since one handle more fails the whole wait.
    pub(super) const CAP: usize = MAXIMUM_WAIT_OBJECTS as usize - 1;

    pub(super) fn run(shared: &Shared, exit_tx: &UnboundedSender<i32>) {
        let wake = shared.backend.wake.as_raw_handle();
        // pid → its process handle; the wait's handle `i + 1` is `watched[i]`.
        let mut watched: Vec<(i32, OwnedHandle)> = Vec::new();
        loop {
            if shared.closed.load(Ordering::SeqCst) {
                return;
            }
            let handles: Vec<_> = std::iter::once(wake)
                .chain(watched.iter().map(|(_, h)| h.as_raw_handle()))
                .collect();
            // `watched` is capped at `CAP`, so this is at most `MAXIMUM_WAIT_OBJECTS`.
            let count = handles.len() as u32;
            // SAFETY: every handle is live through the wait: the event for
            // `shared`'s life, each process handle while `watched` owns it.
            let signaled =
                unsafe { WaitForMultipleObjects(count, handles.as_ptr(), FALSE, INFINITE) };
            if signaled == WAIT_FAILED {
                tracing::debug!(
                    error = %std::io::Error::last_os_error(),
                    "exit-watch wait failed; thread exiting (backstops cover)"
                );
                return;
            }
            if shared.closed.load(Ordering::SeqCst) {
                return;
            }
            // The wait reports the LOWEST signaled index, so the wake (index 0)
            // drains pending before any exit is sent: a duplicate watch() of a
            // pid whose exit is also signaled dedups against the still-watched
            // entry, and that handle stays signaled for the next turn.
            match usize::try_from(signaled.wrapping_sub(WAIT_OBJECT_0)) {
                Ok(0) => {
                    for pid in drain_pending(shared) {
                        if watched.iter().any(|(p, _)| *p == pid) {
                            continue; // idempotent: already watched
                        }
                        if watched.len() == CAP {
                            tracing::debug!(
                                pid,
                                "exit-watch wait is full; dropped (backstops cover)"
                            );
                            continue;
                        }
                        match open(pid) {
                            Opened::Handle(handle) => watched.push((pid, handle)),
                            Opened::AlreadyDead => {
                                if exit_tx.send(pid).is_err() {
                                    return; // receiver gone — the watcher task ended
                                }
                            }
                            Opened::Failed => {}
                        }
                    }
                }
                Ok(i) if i <= watched.len() => {
                    // The pid leaves the set so it can be re-watched if recycled.
                    let (pid, _) = watched.remove(i - 1);
                    if exit_tx.send(pid).is_err() {
                        return;
                    }
                }
                _ => {
                    tracing::debug!(
                        signaled,
                        "exit-watch wait returned no handle; thread exiting (backstops cover)"
                    );
                    return;
                }
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    use tokio::sync::mpsc::UnboundedSender;

    use super::Shared;

    #[cfg(test)]
    pub(super) const AVAILABLE: bool = false;

    /// No validated exit-watch primitive on this platform. `init` returning
    /// `None` makes `ExitWatch::spawn` return `None`, so the watcher keeps the
    /// negative-vouch + TTL backstops only — instant exit is an additive fast
    /// path, never a dependency.
    #[derive(Debug)]
    pub(super) struct Backend;

    impl Backend {
        pub(super) fn init() -> Option<Self> {
            None
        }

        pub(super) fn wake(&self) {}
    }

    pub(super) fn run(_shared: &Shared, _exit_tx: &UnboundedSender<i32>) {}
}

#[cfg(test)]
mod tests {
    use super::{ExitWatch, HAS_BACKEND};

    #[tokio::test]
    async fn spawn_returns_a_watch_exactly_where_a_backend_exists() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert_eq!(ExitWatch::spawn(tx).is_some(), HAS_BACKEND);
    }

    /// Each test returns early where no backend exists;
    /// `spawn_returns_a_watch_exactly_where_a_backend_exists` holds where one must.
    mod live {
        use std::process::Child;
        use std::time::Duration;

        use super::ExitWatch;
        use crate::test_child::sleeper;

        /// Liveness ceiling for the real-process `live::` tests — wide enough
        /// to absorb `cargo llvm-cov` instrumentation and CI scheduling jitter
        /// (3s flaked `drop_stops_cleanly`). A liveness bound, NOT a latency
        /// assertion: a true hang is still failed by nextest's slow-timeout.
        const LIVE_PID_DEADLINE: Duration = Duration::from_secs(10);

        fn kill_and_reap(child: &mut Child) {
            let _ = child.kill();
            let _ = child.wait();
        }

        async fn wait_for_pid(
            rx: &mut tokio::sync::mpsc::UnboundedReceiver<i32>,
            pid: i32,
            within: Duration,
        ) -> bool {
            let deadline = tokio::time::Instant::now() + within;
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                    Ok(Some(p)) if p == pid => return true,
                    Ok(Some(_)) => {}
                    Ok(None) => return false,
                    Err(_) => {}
                }
            }
            false
        }

        /// A pid no process holds: on Windows dropping `child` closes its handle,
        /// so OpenProcess fails with ERROR_INVALID_PARAMETER, a code its docs name
        /// only for pid 0 (learn.microsoft.com, `OpenProcess`). A pid recycled
        /// before the caller's `watch` would flake that caller.
        fn dead_pid() -> i32 {
            let mut child = sleeper();
            let pid = child.id() as i32;
            kill_and_reap(&mut child);
            drop(child);
            pid
        }

        /// Returns once every drain the watcher began before the call has
        /// finished registering. A dead pid's exit is sent from the drain that
        /// took it, so a second one, queued only after the first's exit
        /// arrived, is taken by a later drain than anything queued before.
        async fn settle(watch: &ExitWatch, rx: &mut tokio::sync::mpsc::UnboundedReceiver<i32>) {
            for _ in 0..2 {
                let dead = dead_pid();
                watch.watch(dead);
                assert!(
                    wait_for_pid(rx, dead, LIVE_PID_DEADLINE).await,
                    "a dead pid's watch is answered"
                );
            }
        }

        async fn assert_no_pid_within(
            rx: &mut tokio::sync::mpsc::UnboundedReceiver<i32>,
            pid: i32,
            window: Duration,
            why: &str,
        ) {
            let deadline = tokio::time::Instant::now() + window;
            while tokio::time::Instant::now() < deadline {
                if let Ok(Some(p)) =
                    tokio::time::timeout(Duration::from_millis(50), rx.recv()).await
                {
                    assert_ne!(p, pid, "{why}");
                }
            }
        }

        #[tokio::test]
        async fn exit_watch_reports_a_killed_child() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let Some(watch) = ExitWatch::spawn(tx) else {
                return;
            };
            let mut child = sleeper();
            let pid = child.id() as i32;
            watch.watch(pid);
            kill_and_reap(&mut child);
            assert!(
                wait_for_pid(&mut rx, pid, LIVE_PID_DEADLINE).await,
                "a watched process dying must surface its pid on the exit channel"
            );
        }

        #[tokio::test]
        async fn exit_watch_already_dead_pid_fires_immediately() {
            let pid = dead_pid();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let Some(watch) = ExitWatch::spawn(tx) else {
                return;
            };
            watch.watch(pid);
            assert!(
                wait_for_pid(&mut rx, pid, LIVE_PID_DEADLINE).await,
                "watching an already-dead pid must synthesize its exit (already-dead path)"
            );
        }

        #[tokio::test]
        async fn watch_is_idempotent() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let Some(watch) = ExitWatch::spawn(tx) else {
                return;
            };
            let mut child = sleeper();
            let pid = child.id() as i32;
            watch.watch(pid);
            watch.watch(pid);
            // Both registered while it lives: a watch the drain takes only
            // after the death is answered, by `watch`'s contract.
            settle(&watch, &mut rx).await;
            kill_and_reap(&mut child);
            assert!(
                wait_for_pid(&mut rx, pid, LIVE_PID_DEADLINE).await,
                "the first exit must arrive"
            );
            assert_no_pid_within(
                &mut rx,
                pid,
                Duration::from_millis(500),
                "double-watching a live pid must yield exactly ONE exit",
            )
            .await;
        }

        #[tokio::test]
        async fn a_watch_registered_after_the_death_is_answered_again() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let Some(watch) = ExitWatch::spawn(tx) else {
                return;
            };
            let mut child = sleeper();
            let pid = child.id() as i32;
            watch.watch(pid);
            settle(&watch, &mut rx).await;
            kill_and_reap(&mut child);
            assert!(
                wait_for_pid(&mut rx, pid, LIVE_PID_DEADLINE).await,
                "the death of a watched pid"
            );
            drop(child);
            watch.watch(pid);
            assert!(
                wait_for_pid(&mut rx, pid, LIVE_PID_DEADLINE).await,
                "a watch after the death is answered"
            );
        }

        /// A garbage pid may either synthesize an exit (ESRCH) or stay silent —
        /// the load-bearing assertion is ISOLATION: a real watch registered
        /// after it still fires.
        #[tokio::test]
        async fn watch_bogus_pid_does_not_panic() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let Some(watch) = ExitWatch::spawn(tx) else {
                return;
            };
            watch.watch(999_999_999);
            let mut child = sleeper();
            let pid = child.id() as i32;
            watch.watch(pid);
            kill_and_reap(&mut child);
            assert!(
                wait_for_pid(&mut rx, pid, LIVE_PID_DEADLINE).await,
                "a bogus registration must not break later real watches"
            );
        }

        /// Thread death is forced via the receiver-gone exit path — the only
        /// one triggerable portably.
        #[tokio::test]
        async fn watch_after_thread_death_stops_queueing() {
            use std::sync::atomic::Ordering;
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let Some(watch) = ExitWatch::spawn(tx) else {
                return;
            };
            drop(rx);
            // An already-dead pid synthesizes an exit whose send fails
            // (receiver dropped) → run() returns → the death hook must fire.
            let mut child = sleeper();
            let pid = child.id() as i32;
            kill_and_reap(&mut child);
            watch.watch(pid);
            let deadline = tokio::time::Instant::now() + LIVE_PID_DEADLINE;
            while !watch.shared.closed.load(Ordering::SeqCst)
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(
                watch.shared.closed.load(Ordering::SeqCst),
                "a dead watcher thread must mark the handle closed"
            );
            watch.watch(4242);
            watch.watch(4243);
            assert!(
                super::super::lock_pending(&watch.shared).is_empty(),
                "watch() after thread death must not grow `pending`"
            );
        }

        /// Looped to STRESS the `watch()`-then-`drop()` wake race: on Linux a
        /// Drop landing between the watcher thread's post-poll `closed` check
        /// and its unconditional `drain_pipe` could have its wake byte EATEN by
        /// that drain, stranding the thread in `poll()` on the still-live
        /// child. One unfixed iteration hangs the whole test.
        #[tokio::test]
        async fn drop_stops_cleanly() {
            for _ in 0..50 {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let Some(watch) = ExitWatch::spawn(tx) else {
                    return;
                };
                let mut child = sleeper();
                watch.watch(child.id() as i32);
                drop(watch);
                let closed = tokio::time::timeout(LIVE_PID_DEADLINE, async {
                    while rx.recv().await.is_some() {}
                })
                .await;
                // Reap BEFORE asserting: a trailing reap would be skipped if the
                // assert fired, leaking the sleeper.
                kill_and_reap(&mut child);
                assert!(
                    closed.is_ok(),
                    "dropping the ExitWatch must stop the thread (its sender must drop)"
                );
            }
        }

        /// One handle past the cap would fail the whole wait and end the thread,
        /// losing instant exit for every later pid.
        #[cfg(windows)]
        #[tokio::test]
        async fn a_pid_past_the_wait_cap_is_dropped_and_the_watch_lives_on() {
            use std::sync::atomic::Ordering;

            use super::super::imp::CAP;

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let Some(watch) = ExitWatch::spawn(tx) else {
                return;
            };
            let mut children: Vec<Child> = (0..=CAP).map(|_| sleeper()).collect();
            // Registered in the order watched, so the highest pid is the one past the cap.
            children.sort_by_key(Child::id);
            for child in &children {
                watch.watch(child.id() as i32);
            }
            let (inside, past) = (children[CAP - 1].id() as i32, children[CAP].id() as i32);
            kill_and_reap(&mut children[CAP - 1]);
            kill_and_reap(&mut children[CAP]);
            let fired = wait_for_pid(&mut rx, inside, LIVE_PID_DEADLINE).await;
            assert_no_pid_within(&mut rx, past, Duration::from_millis(500), "never watched").await;
            let closed = watch.shared.closed.load(Ordering::SeqCst);
            for child in &mut children {
                kill_and_reap(child);
            }
            assert!(fired, "the last pid inside the cap must still fire");
            assert!(
                !closed,
                "the wait must never be handed more handles than it takes"
            );
        }
    }
}
