//! The TUI's startup on a real pty, with this test playing the terminal: what
//! only a terminal answering the startup queries shows.
#![cfg(unix)]

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, ExitStatus, NativePtySystem, PtySize, PtySystem};

mod common;

/// Each startup query the terminal answers, with the reply of a truecolor
/// terminal that keeps synchronized output (xterm's ctlseqs: DECRQSS, DECRQM,
/// DA1). The SGR reply's text ends in `m`, the office's mute key.
const ANSWERS: [(&[u8], &[u8]); 3] = [
    (b"\x1bP$qm\x1b\\", b"\x1bP1$r0;48;2;1;2;3m\x1b\\"),
    (b"\x1b[?2026$p", b"\x1b[?2026;2$y"),
    (b"\x1b[c", b"\x1b[?62;22c"),
];
/// [`ANSWERS`]' rows.
const DECRQSS: usize = 0;
const DECRQM: usize = 1;
const DA1: usize = 2;
/// How far the terminal is: each reply lands this long after its query. Slower
/// than a terrestrial round trip, the slow link the query budget is sized for.
const LINK_RTT: Duration = Duration::from_millis(500);
/// crossterm's `EnterAlternateScreen`: the startup is past its queries.
const ALT_SCREEN: &[u8] = b"\x1b[?1049h";
/// A size the office lays out in.
const SIZE: PtySize = PtySize {
    rows: 40,
    cols: 120,
    pixel_width: 0,
    pixel_height: 0,
};
/// How long the office may take to start, and to exit once told to.
const BUDGET: Duration = Duration::from_secs(20);
/// How long a typed `q` gets to quit the office before it is typed again: one
/// typed before the office reads input is discarded with what the startup left.
const RETYPE: Duration = Duration::from_millis(500);
const POLL: Duration = Duration::from_millis(20);

/// The reply to each [`ANSWERS`] row asked.
type Answer = fn(usize) -> Vec<u8>;

/// What the terminal has seen of the office's output.
#[derive(Default)]
struct Seen {
    /// The [`ANSWERS`] rows asked, in order.
    asked: Vec<usize>,
    /// Replies not yet sent.
    pending: usize,
    started: bool,
}

/// What a run of the office showed.
struct Run {
    asked: Vec<usize>,
    status: ExitStatus,
    seeded: String,
    config: String,
}

/// The office's process, killed if the test ends before it exits.
struct Reaped(Box<dyn portable_pty::Child + Send + Sync>);

impl Drop for Reaped {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// The earliest [`ANSWERS`] query in `out`: its row and where it ends.
fn next_query(out: &[u8]) -> Option<(usize, usize)> {
    ANSWERS
        .iter()
        .enumerate()
        .filter_map(|(row, (query, _))| {
            let at = out.windows(query.len()).position(|w| w == *query)?;
            Some((at, row, at + query.len()))
        })
        .min()
        .map(|(_, row, end)| (row, end))
}

/// Wait until `done` holds, for at most [`BUDGET`].
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + BUDGET;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(POLL);
    }
}

/// The office's config under `home`, seeded with this version already seen,
/// so the run has nothing of its own to save; and what it was seeded with.
fn seed_config(home: &std::path::Path) -> (std::path::PathBuf, String) {
    let config = home.join(".config/pixtuoid/config.toml");
    std::fs::create_dir_all(config.parent().expect("config dir")).expect("config dir");
    let seeded = format!("last-seen-version = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(&config, &seeded).expect("seed config");
    (config, seeded)
}

/// The office running on a pty, this test its terminal.
struct Office {
    child: Reaped,
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// What the terminal types: when, the bytes, and whether they answer a query.
    send: mpsc::Sender<(Instant, Vec<u8>, bool)>,
    seen: Arc<Mutex<Seen>>,
    closed: Arc<AtomicBool>,
    /// Every byte the office has written.
    wrote: Arc<AtomicUsize>,
    scanner: std::thread::JoinHandle<()>,
    replier: std::thread::JoinHandle<()>,
}

impl Office {
    /// Start `run` (an [`common::isolated`] `run`) on a pty whose terminal
    /// replies with `answer`, each reply `link_rtt` after its query, in order,
    /// and wait until the office is past its queries and every reply is in.
    /// Over ssh, where the graphics probe isn't asked: the truecolor and sync
    /// queries are the whole startup.
    fn start(mut run: std::process::Command, link_rtt: Duration, answer: Answer) -> Self {
        run.env("TERM", "xterm-256color")
            .env("SSH_CONNECTION", "10.0.0.2 51234 10.0.0.1 22");
        let mut on_pty = CommandBuilder::new(run.get_program());
        on_pty.args(run.get_args());
        on_pty.env_clear();
        for (key, value) in run.get_envs() {
            if let Some(value) = value {
                on_pty.env(key, value);
            }
        }
        let pty = NativePtySystem::default().openpty(SIZE).expect("openpty");
        let child = Reaped(pty.slave.spawn_command(on_pty).expect("spawn pixtuoid run"));
        drop(pty.slave);

        let seen = Arc::new(Mutex::new(Seen::default()));
        let (send, sends) = mpsc::channel::<(Instant, Vec<u8>, bool)>();
        let mut writer = pty.master.take_writer().expect("pty writer");
        let replier = {
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                for (due, bytes, reply) in sends {
                    std::thread::sleep(due.saturating_duration_since(Instant::now()));
                    if writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        return;
                    }
                    if reply {
                        seen.lock().expect("seen").pending -= 1;
                    }
                }
            })
        };
        let mut reader = pty.master.try_clone_reader().expect("pty reader");
        let closed = Arc::new(AtomicBool::new(false));
        let wrote = Arc::new(AtomicUsize::new(0));
        let scanner = {
            let (seen, send, closed, wrote) = (
                Arc::clone(&seen),
                send.clone(),
                Arc::clone(&closed),
                Arc::clone(&wrote),
            );
            std::thread::spawn(move || {
                let (mut out, mut chunk, mut scanned) = (Vec::new(), [0u8; 4096], 0);
                while let Ok(n @ 1..) = reader.read(&mut chunk) {
                    wrote.fetch_add(n, Ordering::Relaxed);
                    if seen.lock().expect("seen").started {
                        continue;
                    }
                    out.extend_from_slice(&chunk[..n]);
                    while let Some((row, end)) = next_query(&out[scanned..]) {
                        let mut s = seen.lock().expect("seen");
                        s.asked.push(row);
                        s.pending += 1;
                        let _ = send.send((Instant::now() + link_rtt, answer(row), true));
                        scanned += end;
                    }
                    if out.windows(ALT_SCREEN.len()).any(|w| w == ALT_SCREEN) {
                        seen.lock().expect("seen").started = true;
                    }
                }
                closed.store(true, Ordering::Relaxed);
            })
        };

        wait_for("the office past its queries, every reply sent", || {
            let s = seen.lock().expect("seen");
            s.started && s.pending == 0
        });
        Self {
            child,
            master: pty.master,
            send,
            seen,
            closed,
            wrote,
            scanner,
            replier,
        }
    }

    /// Type `key` until `done` holds: one typed before the office reads input
    /// is discarded with what the startup left.
    fn type_until(&mut self, key: &[u8], what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + BUDGET;
        loop {
            self.send
                .send((Instant::now(), key.to_vec(), false))
                .expect("type");
            let retype = Instant::now() + RETYPE;
            while Instant::now() < retype {
                if done(self) {
                    return;
                }
                std::thread::sleep(POLL);
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
        }
    }

    /// How the office ended, once it has.
    fn exited(&mut self) -> Option<ExitStatus> {
        self.child.0.try_wait().expect("try_wait")
    }

    /// Wait for the pty to close, and the startup queries the office asked.
    fn finish(self) -> Vec<usize> {
        let Self {
            send,
            seen,
            closed,
            scanner,
            replier,
            ..
        } = self;
        drop(send);
        wait_for("the pty to close", || closed.load(Ordering::Relaxed));
        scanner.join().expect("scanner");
        replier.join().expect("replier");
        std::mem::take(&mut seen.lock().expect("seen").asked)
    }
}

/// Start the office on a pty whose terminal replies with `answer` (see
/// [`Office::start`]); once it is past its queries and every reply is in, type
/// `q` until it quits.
fn run_office(link_rtt: Duration, answer: Answer) -> Run {
    let home = tempfile::tempdir().expect("home");
    let (config, seeded) = seed_config(home.path());
    let mut office = Office::start(common::isolated(&["run"], home.path()), link_rtt, answer);
    // Each `q` lands behind every reply, so the office dispatches any reply
    // that reached it as keys before it quits.
    let mut status = None;
    office.type_until(b"q", "the office to quit on q", |office| {
        status = office.exited();
        status.is_some()
    });
    Run {
        asked: office.finish(),
        status: status.expect("the office quit"),
        seeded,
        config: std::fs::read_to_string(&config).expect("config"),
    }
}

/// Every terminal's answer, on time.
fn prompt(row: usize) -> Vec<u8> {
    ANSWERS[row].1.to_vec()
}

/// A first run's connect runs off the office's loop: while its host CLI hangs,
/// a resize still repaints, and a quit, by signal or by key, ends the office at
/// once, the CLI's job with it.
#[test]
fn a_hung_connect_leaves_the_office_live_and_a_quit_takes_its_cli() {
    for by_signal in [true, false] {
        let home = tempfile::tempdir().expect("home");
        seed_config(home.path());
        // Claude Code's home, so the onboarding offers it, checked.
        std::fs::create_dir_all(home.path().join(".claude")).expect("claude home");
        let run = common::isolated(&["run"], home.path());
        let job = home.path().join("job.pid");
        let claude = home.path().join(".fake-bin/claude");
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\nsleep 600 &\necho $! > '{}'\nwait\n",
                job.display()
            ),
        )
        .expect("a hung claude");
        let mut office = Office::start(run, Duration::ZERO, prompt);
        office.type_until(b"\r", "the connect's CLI", |_| {
            std::fs::read_to_string(&job).is_ok_and(|pid| !pid.trim().is_empty())
        });

        let painted = office.wrote.load(Ordering::Relaxed);
        office
            .master
            .resize(PtySize { cols: 100, ..SIZE })
            .expect("resize");
        wait_for("a repaint while the CLI hangs", || {
            office.wrote.load(Ordering::Relaxed) > painted
        });

        let quit_at = Instant::now();
        let status = if by_signal {
            let pid = office.child.0.process_id().expect("its pid");
            // SAFETY: signalling the office this test spawned and still owns.
            unsafe {
                libc::kill(i32::try_from(pid).expect("a pid"), libc::SIGTERM);
            }
            let mut status = None;
            wait_for("the office to end on SIGTERM", || {
                status = office.exited();
                status.is_some()
            });
            status.expect("ended")
        } else {
            let mut status = None;
            office.type_until(b"q", "the office to quit on q", |office| {
                status = office.exited();
                status.is_some()
            });
            status.expect("ended")
        };
        assert!(quit_at.elapsed() < BUDGET);
        // SAFETY: `strsignal` returns a static string for a valid signal.
        let sigterm = unsafe { std::ffi::CStr::from_ptr(libc::strsignal(libc::SIGTERM)) };
        let want = by_signal.then(|| sigterm.to_string_lossy().into_owned());
        assert_eq!(status.signal().map(str::to_owned), want, "{status}");
        let pid = std::fs::read_to_string(&job).expect("its pid");
        wait_for("the CLI's job to die with the office", || {
            !std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        });
        office.finish();
    }
}

/// A terminal across a slow link answers the startup queries late, yet within
/// the budget each query waits: none of their bytes reaches the office as a
/// key, so the office stays muted and its config untouched.
#[test]
fn a_slow_terminals_startup_replies_never_reach_the_office_as_keys() {
    assert!(
        LINK_RTT < pixtuoid::dev::QUERY_TIMEOUT,
        "the query budget no longer covers the link this test models"
    );
    let run = run_office(LINK_RTT, prompt);
    assert_eq!(
        run.config, run.seeded,
        "a late reply reached the office as keys"
    );
    assert!(run.status.success(), "{:?}", run.status);
    assert_eq!(
        run.asked,
        [DECRQSS, DA1, DECRQM, DA1],
        "the truecolor then the sync query, each ending on DA1"
    );
}

/// Bytes queued behind the last probe's DA1, which it never reads, are
/// discarded before the office reads its first key.
#[test]
fn bytes_queued_behind_the_last_reply_never_reach_the_office_as_keys() {
    let run = run_office(LINK_RTT, |row| {
        let mut reply = ANSWERS[row].1.to_vec();
        if row == DECRQM {
            reply.extend_from_slice(ANSWERS[DA1].1);
            // The probe stops at the read that brings DA1 in, a chunk at most:
            // past it, wherever the chunk cuts them, a whole stale reply stays.
            let stale = ANSWERS[DECRQSS].1;
            let mut tail = Vec::new();
            while tail.len() < pixtuoid::dev::TTY_READ_CHUNK + 2 * stale.len() {
                tail.extend_from_slice(stale);
            }
            reply.extend(tail);
        }
        reply
    });
    assert_eq!(
        run.config, run.seeded,
        "a stale reply reached the office as keys"
    );
    assert!(run.status.success(), "{:?}", run.status);
}

/// A link slower than one budget but within two: the truecolor probe gives up,
/// and its late reply, DA1 included, ends the sync probe's read rather than
/// reaching the office as keys; the sync probe's own late reply (CSI only)
/// lands after the flush, where crossterm keeps it as an internal event.
#[test]
fn a_reply_that_outlives_its_probe_is_read_by_the_next_one() {
    let run = run_office(pixtuoid::dev::QUERY_TIMEOUT * 3 / 2, prompt);
    assert_eq!(
        run.config, run.seeded,
        "a reply that outlived its probe reached the office as keys"
    );
    assert!(run.status.success(), "{:?}", run.status);
    assert_eq!(
        run.asked,
        [DECRQSS, DA1, DECRQM, DA1],
        "the sync query, asked after the truecolor one went unanswered"
    );
}
