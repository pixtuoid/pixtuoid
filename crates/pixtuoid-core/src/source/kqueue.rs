//! A kqueue whose blocked `kevent` wait another thread can end: the
//! `EVFILT_USER` slot the crate's kqueue threads are woken through.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr;
use std::time::Duration;

use rustix::event::kqueue::{
    Event, EventFilter, EventFlags, UserDefinedFlags, UserFlags, kevent, kqueue,
};

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
