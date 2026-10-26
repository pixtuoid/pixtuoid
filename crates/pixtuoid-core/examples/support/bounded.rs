//! What bounds a child the recorder waits on: past its budget it is killed,
//! and so is it when a signal ends the recorder. `capture_fixture`
//! `#[path]`-includes it: its deps are dev-only, and in the lib they would
//! join `deny-arch.toml`'s allowlist.

use std::process::{Command, ExitStatus, Stdio};
#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use pixtuoid_core::harness::wait_until;

/// The running child's process group, 0 between children.
#[cfg(unix)]
static GROUP: AtomicI32 = AtomicI32::new(0);

/// How long a killed child may take to be reaped: a Job Object's exit messages
/// are "not guaranteed" (learn.microsoft.com, `JOBOBJECT_ASSOCIATE_COMPLETION_PORT`),
/// so the kill's own wait for one could block forever.
const REAP: Duration = Duration::from_secs(10);

/// How `cmd` ended within `budget`, or `None` once the budget ran out and it
/// was killed. It reads nothing and runs in a process group (Unix) or a Job
/// Object (Windows) of its own, which the kill takes whole.
pub(crate) fn run(mut cmd: Command, budget: Duration) -> std::io::Result<Option<ExitStatus>> {
    // A background process group that read the terminal would stop on SIGTTIN.
    cmd.stdin(Stdio::null());
    let mut cmd = process_wrap::std::CommandWrap::from(cmd);
    #[cfg(unix)]
    cmd.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    cmd.wrap(process_wrap::std::JobObject);
    let mut child = cmd.spawn()?;
    #[cfg(unix)]
    GROUP.store(i32::try_from(child.id()).unwrap_or(0), Ordering::SeqCst);
    let status = wait_or_kill(child.as_mut(), budget);
    #[cfg(unix)]
    GROUP.store(0, Ordering::SeqCst);
    status
}

/// [`run`]'s wait: `child`'s status, or `None` once it was killed at `budget`.
fn wait_or_kill(
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
    child.start_kill()?;
    let _ = wait_until("its reaping", REAP, || Ok(child.try_wait()?.is_some()));
    Ok(None)
}

/// Kill the running child's process group when a signal ends this process,
/// then die by the signal's default: the terminal's Ctrl-C never reaches that
/// group, and once this process is gone nothing holds the child to its budget.
#[cfg(unix)]
pub(crate) fn kill_child_on_signal() -> std::io::Result<()> {
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
