//! What bounds a child the hermetic tier waits on: past its budget it is
//! killed, and so is it when a signal ends the process waiting on it.
//! `capture_fixture` and `hermetic` both `#[path]`-include it: its deps are
//! dev-only, and in the lib they would join `deny-arch.toml`'s allowlist.

use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use pixtuoid_core::harness::wait_until;

/// The process group of the child running now with nobody attending, 0
/// between children.
#[cfg(unix)]
static GROUP: AtomicI32 = AtomicI32::new(0);

/// Who is at a child, as the stdin of the process waiting on it tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seat {
    /// An operator at the terminal, recording a TUI or watching a turn.
    Attended,
    /// Nobody: the hermetic tier, CI.
    Unattended,
}

/// How a bounded child ended, and what it wrote to a piped stdout and stderr.
#[derive(Debug)]
pub(crate) struct Ended {
    /// Its status, or `None` once its budget ran out and it was killed.
    pub(crate) status: Option<ExitStatus>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// `cmd` run within `budget`, past which it is killed. With nobody attending
/// it reads nothing and runs in its own process group (Unix), which the kill
/// takes whole; attended, it keeps the terminal and its foreground group, so a
/// TUI reads the keys typed into it and Ctrl-C reaches it, and the kill takes
/// it alone. Either way a Windows child runs in a Job Object of its own, which
/// the kill takes whole. A CLI that leads its own session under a driver's pty
/// is in neither: the pty's hangup when its driver dies is what ends it.
pub(crate) fn run(mut cmd: Command, seat: Seat, budget: Duration) -> std::io::Result<Ended> {
    if seat == Seat::Unattended {
        // A background process group that read the terminal would stop on SIGTTIN.
        cmd.stdin(Stdio::null());
    }
    let mut cmd = process_wrap::std::CommandWrap::from(cmd);
    #[cfg(unix)]
    if seat == Seat::Unattended {
        cmd.wrap(process_wrap::std::ProcessGroup::leader());
    }
    #[cfg(windows)]
    cmd.wrap(process_wrap::std::JobObject);
    let started = Instant::now();
    let mut child = cmd.spawn()?;
    #[cfg(unix)]
    if seat == Seat::Unattended {
        GROUP.store(i32::try_from(child.id()).unwrap_or(0), Ordering::SeqCst);
    }
    // Drained as it runs, as `wait_with_output` does: a child that filled a
    // pipe nobody read would block on it until its budget ran out.
    let (out, err) = (drain(child.stdout().take()), drain(child.stderr().take()));
    let status = waited(child.as_mut(), budget);
    if status.as_ref().is_ok_and(Option::is_some)
        && seat == Seat::Unattended
        && (out.is_some() || err.is_some())
    {
        // What it left holding its pipes would keep them open past its exit.
        let _ = child.start_kill();
    }
    #[cfg(unix)]
    GROUP.store(0, Ordering::SeqCst);
    let left = budget.saturating_sub(started.elapsed());
    let read = |pipe: Option<mpsc::Receiver<Vec<u8>>>| {
        pipe.and_then(|rx| rx.recv_timeout(left).ok())
            .unwrap_or_default()
    };
    Ok(Ended {
        status: status?,
        stdout: read(out),
        stderr: read(err),
    })
}

/// What `pipe` delivers until it closes, read on a thread of its own.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Option<mpsc::Receiver<Vec<u8>>> {
    let mut pipe = pipe?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut read = Vec::new();
        let _ = pipe.read_to_end(&mut read);
        let _ = tx.send(read);
    });
    Some(rx)
}

/// [`run`]'s wait: `child`'s status, or `None` once it was killed at `budget`.
fn waited(
    child: &mut dyn process_wrap::std::ChildWrapper,
    budget: Duration,
) -> std::io::Result<Option<ExitStatus>> {
    // An Err is the budget running out or a poll's error, which the poll
    // below meets again.
    let _ = wait_until("its exit", budget, || Ok(child.try_wait()?.is_some()));
    // A poll's status, never `wait`'s: a Job Object's waits on every process
    // the child left behind.
    if let Some(status) = child.try_wait()? {
        return Ok(Some(status));
    }
    child.kill()?;
    Ok(None)
}

/// Kill the running unattended child's process group when a signal ends this
/// process, then die by the signal's default: the terminal's Ctrl-C never
/// reaches that group, and once this process is gone nothing holds the child
/// to its budget.
#[cfg(unix)]
pub(crate) fn forward_endings() -> std::io::Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGHUP, SIGINT, SIGTERM])?;
    std::thread::spawn(move || {
        for signal in signals.forever() {
            if let Some(group) = rustix::process::Pid::from_raw(GROUP.load(Ordering::SeqCst)) {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            }
            let _ = signal_hook::low_level::emulate_default_handler(signal);
        }
    });
    Ok(())
}
