use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use filetime::{FileTime, set_file_mtime};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::source::Transport;
use pixtuoid_core::source::jsonl::{LivenessProbe, ProbeSnapshot};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::vouch_snapshot_with_pids;
use crate::{
    backdate, codex_meta_line, codex_watcher, codex_work_line, collect_for, collect_until,
    start_id, vouch_snapshot, write_lines,
};

/// The probe poll these tests run the watcher at.
const PROBE_POLL: Duration = Duration::from_millis(100);

/// A negative-vouch span no wait here reaches, so only the instant-exit rung
/// can end a session.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const UNREACHED_VOUCH_SPAN: Duration = Duration::from_secs(3600);

/// A probe a test answers for, flipping its answer mid-run, and each instant
/// the watcher asked it. A scan pass asks once and passes run one at a time,
/// so by an ask, the pass before it has sent all it caused: a wait counts
/// passes, never the clock.
#[derive(Clone)]
struct Probe(Arc<watch::Sender<Asked>>);

struct Asked {
    answer: Option<ProbeSnapshot>,
    at: Vec<Instant>,
}

impl Probe {
    fn new(answer: Option<ProbeSnapshot>) -> Self {
        Self(Arc::new(watch::Sender::new(Asked {
            answer,
            at: Vec::new(),
        })))
    }

    /// One ask, as the watcher makes it.
    fn ask(&self) -> Option<ProbeSnapshot> {
        let mut answer = None;
        self.0.send_modify(|asked| {
            asked.at.push(Instant::now());
            answer = asked.answer.clone();
        });
        answer
    }

    fn liveness(&self) -> LivenessProbe {
        let probe = self.clone();
        Arc::new(move || probe.ask())
    }

    /// Answer `answer` from now on; how many asks came before, so the first to
    /// take it is that index.
    fn answer(&self, answer: Option<ProbeSnapshot>) -> usize {
        let mut before = 0;
        self.0.send_modify(|asked| {
            asked.answer = answer;
            before = asked.at.len();
        });
        before
    }

    /// How many asks so far.
    fn asks(&self) -> usize {
        self.0.borrow().at.len()
    }

    /// Once the watcher has asked `n` times since the `from`th ask, leaving
    /// what it sent queued.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn asked(&self, from: usize, n: usize) {
        let mut asked = self.0.subscribe();
        tokio::time::timeout(crate::DEADLINE, asked.wait_for(|a| a.at.len() >= from + n))
            .await
            .expect("the watcher stopped asking")
            .expect("the probe outlives its watcher");
    }

    /// Every event `rx` gets until `done` holds of the asks' instants:
    /// nothing but listening proves an absence.
    async fn listen_until(
        &self,
        rx: &mut mpsc::Receiver<(Transport, AgentEvent)>,
        what: &str,
        done: impl Fn(&[Instant]) -> bool,
    ) -> Vec<(Transport, AgentEvent)> {
        let mut asked = self.0.subscribe();
        let deadline = tokio::time::Instant::now() + crate::DEADLINE;
        let mut events = Vec::new();
        while !done(&asked.borrow_and_update().at) {
            tokio::select! {
                tagged = rx.recv() => {
                    events.push(tagged.unwrap_or_else(|| panic!("{what}: the stream closed")));
                }
                changed = asked.changed() => changed.expect("the probe outlives its watcher"),
                () = tokio::time::sleep_until(deadline) => {
                    panic!("{what}: the watcher stopped asking");
                }
            }
        }
        events.extend(std::iter::from_fn(|| rx.try_recv().ok()));
        events
    }

    /// [`Self::listen_until`] three asks at or past `span` after the `from`th:
    /// the first may fold a hair inside the span, the second folds past it,
    /// and the third has the second's pass sent.
    async fn listen_past(
        &self,
        rx: &mut mpsc::Receiver<(Transport, AgentEvent)>,
        from: usize,
        span: Duration,
        what: &str,
    ) -> Vec<(Transport, AgentEvent)> {
        self.listen_until(rx, what, |at| {
            at.get(from)
                .is_some_and(|first| at.iter().filter(|a| **a >= *first + span).count() >= 3)
        })
        .await
    }
}

/// Whether `events` hold a root `SessionEnd` for `agent`.
fn ends(events: &[(Transport, AgentEvent)], agent: AgentId) -> bool {
    events
        .iter()
        .any(|(_, e)| matches!(e, AgentEvent::SessionEnd { agent_id, .. } if *agent_id == agent))
}

/// The agents a pid's death ended, through `witness`'s `SessionEnd`,
/// `witness` being bound to that pid alone: an instant exit ends every id the
/// dead pid held in one pass of the watch loop, so the ask after it has every
/// one sent.
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn ended_through(
    probe: &Probe,
    rx: &mut mpsc::Receiver<(Transport, AgentEvent)>,
    witness: AgentId,
) -> Vec<AgentId> {
    let mut events = collect_until(
        rx,
        "the witness's SessionEnd",
        |e| matches!(e, AgentEvent::SessionEnd { agent_id, .. } if *agent_id == witness),
    )
    .await;
    let from = probe.asks();
    events.extend(
        probe
            .listen_until(rx, "past the death", |at| at.len() > from)
            .await,
    );
    events
        .iter()
        .filter_map(|(_, e)| match e {
            AgentEvent::SessionEnd { agent_id, .. } => Some(*agent_id),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn watcher_emits_proof_of_life_for_probe_live_ids() {
    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-pol");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();

    let uuid = "01000000-0000-7000-8000-0000000000ab";
    let stale = project_dir.join(format!("{uuid}.jsonl"));
    let line = codex_meta_line(uuid, "/repo");
    tokio::fs::write(&stale, format!("{line}\n")).await.unwrap();
    let backdated = FileTime::from_system_time(SystemTime::now() - Duration::from_secs(3600));
    set_file_mtime(&stale, backdated).unwrap();

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = codex_watcher(projects_root.clone())
        .with_initial_window(Duration::from_secs(60))
        .with_liveness_probe(std::sync::Arc::new(move || vouch_snapshot(&[uuid])));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let expected = AgentId::from_parts("codex", uuid);
    let pol = collect_until(&mut rx, "a probe refresh's ProofOfLife", |e| {
        matches!(e, AgentEvent::ProofOfLife { .. })
    })
    .await
    .pop()
    .map(|(t, e)| (t, e.agent_id()));
    assert_eq!(
        pol,
        Some((Transport::Jsonl, expected)),
        "each probe refresh must emit a ProofOfLife per vouched id"
    );
    handle.abort();
}

/// The probe starts EMPTY so the initial seed gates the stale
/// file; the id becomes probe-live only afterwards, so the SessionStart can
/// ONLY come from a poll-arm snapshot refresh and the ProofOfLife only from
/// the poll-arm emission. `with_poll_interval` is the test seam — the
/// production 60s cadence makes the poll arm untestable.
#[tokio::test]
async fn poll_arm_refreshes_probe_snapshot_and_reemits_proof_of_life() {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    let dir = TempDir::new().unwrap();
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-poll-pol");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();

    let uuid = "01000000-0000-7000-8000-0000000000ac";
    let stale = project_dir.join(format!("{uuid}.jsonl"));
    let line = codex_meta_line(uuid, "/repo");
    tokio::fs::write(&stale, format!("{line}\n")).await.unwrap();
    let backdated = FileTime::from_system_time(SystemTime::now() - Duration::from_secs(3600));
    set_file_mtime(&stale, backdated).unwrap();

    let live: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let probe_view = live.clone();
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let watcher = codex_watcher(projects_root.clone())
        .with_initial_window(Duration::from_secs(60))
        .with_poll_interval(PROBE_POLL)
        .with_liveness_probe(Arc::new(move || {
            let ids = probe_view.lock().unwrap().clone();
            vouch_snapshot(&ids.iter().map(String::as_str).collect::<Vec<_>>())
        }));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let held = collect_for(&mut rx, 3 * PROBE_POLL, "the empty-probe gate").await;
    assert!(
        !held.iter().any(|(_, ev)| matches!(
            ev,
            AgentEvent::SessionStart { .. } | AgentEvent::ProofOfLife { .. }
        )),
        "the gate must hold while the probe snapshot is empty, got {held:?}"
    );

    live.lock().unwrap().insert(uuid.to_string());

    let expected = AgentId::from_parts("codex", uuid);
    let (mut got_start, mut got_pol) = (false, false);
    let events = collect_until(
        &mut rx,
        "the poll arm's re-vouched SessionStart and its ProofOfLife",
        |e| {
            got_start |= start_id(e) == Some(expected);
            got_pol |= matches!(e, AgentEvent::ProofOfLife { agent_id } if *agent_id == expected);
            got_start && got_pol
        },
    )
    .await;
    assert!(
        events.iter().all(|(t, _)| *t == Transport::Jsonl),
        "{events:?}"
    );
    handle.abort();
}

/// Shared fixture for the negative-vouch + instant-exit tests: a stale
/// transcript admitted via a [`Probe`] the test flips mid-run. Returns the
/// probe, the event receiver, the transcript path, and the watcher task
/// — after asserting the probe-vouched admission already happened.
async fn admitted_with_mutable_probe(
    projects_root: std::path::PathBuf,
    uuid: &'static str,
    min_span: Duration,
    initial: Option<ProbeSnapshot>,
) -> (
    Probe,
    mpsc::Receiver<(Transport, AgentEvent)>,
    std::path::PathBuf,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let project_dir = projects_root.join("proj-nvouch");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let stale = project_dir.join(format!("{uuid}.jsonl"));
    write_lines(&stale, &[codex_meta_line(uuid, "/repo")]).await;
    backdate(&stale, 7200);

    let probe = Probe::new(initial);
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = codex_watcher(projects_root)
        .with_initial_window(Duration::from_secs(60))
        .with_poll_interval(PROBE_POLL)
        .with_negative_vouch_min_span(min_span)
        .with_liveness_probe(probe.liveness());
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let expected = AgentId::from_parts("codex", uuid);
    collect_until(
        &mut rx,
        "the probe-vouched stale transcript must register",
        |e| start_id(e) == Some(expected),
    )
    .await;
    (probe, rx, stale, handle)
}

/// Drain `rx` until `expected`'s root `SessionEnd`, panicking with `why` if
/// none comes.
async fn expect_session_end(
    rx: &mut mpsc::Receiver<(Transport, AgentEvent)>,
    expected: AgentId,
    why: &str,
) {
    let end = collect_until(rx, why, |e| {
        matches!(e, AgentEvent::SessionEnd { agent_id, as_child: false } if *agent_id == expected)
    })
    .await;
    assert_eq!(end.last().map(|(t, _)| *t), Some(Transport::Jsonl), "{why}");
}

#[tokio::test]
async fn negative_vouch_emits_session_end_after_sustained_disappearance() {
    let dir = TempDir::new().unwrap();
    let uuid = "01000000-0000-7000-8000-0000000000ad";
    let (probe, mut rx, transcript, handle) = admitted_with_mutable_probe(
        dir.path().to_path_buf(),
        uuid,
        Duration::from_millis(300),
        vouch_snapshot(&[uuid]),
    )
    .await;
    let expected = AgentId::from_parts("codex", uuid);

    // The owning process exits: the probe stays HEALTHY but stops vouching.
    probe.answer(vouch_snapshot(&[]));

    expect_session_end(
        &mut rx,
        expected,
        "two healthy snapshots ≥ the span apart without the vouch must emit SessionEnd",
    )
    .await;

    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("{}\n", codex_work_line()).as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    collect_until(
        &mut rx,
        "an append after the negative-vouch exit must re-register the session (seen un-claim)",
        |e| start_id(e) == Some(expected),
    )
    .await;
    handle.abort();
}

/// One missed snapshot is NOT an exit: Codex briefly drops and reopens its
/// rollout fd on a write failure, so a vouch that re-appears within the
/// confirmation span must cancel the pending miss window.
#[tokio::test]
async fn one_missed_snapshot_does_not_end_the_session() {
    let dir = TempDir::new().unwrap();
    let uuid = "01000000-0000-7000-8000-0000000000ae";
    // The span must dwarf the drop below, two misses one poll apart, so the
    // re-vouch lands INSIDE it even when load stretches a pass; a span close to
    // the drop flakes into a confirmed exit on a busy machine.
    let span = PROBE_POLL * 10;
    let (probe, mut rx, _transcript, handle) = admitted_with_mutable_probe(
        dir.path().to_path_buf(),
        uuid,
        span,
        vouch_snapshot(&[uuid]),
    )
    .await;
    let expected = AgentId::from_parts("codex", uuid);

    // The first miss opens the window and the second lands inside it.
    let first_miss = probe.answer(vouch_snapshot(&[]));
    let mut events = probe
        .listen_until(&mut rx, "the drop", |at| at.len() >= first_miss + 2)
        .await;
    probe.answer(vouch_snapshot(&[uuid]));

    // The quiet window must reach PAST the span: an uncancelled miss window
    // would confirm inside it, which is what proves the cancellation.
    events.extend(
        probe
            .listen_past(&mut rx, first_miss, span, "past the span")
            .await,
    );
    assert!(
        !ends(&events, expected),
        "a vouch re-appearing within the span must cancel the miss window — no SessionEnd"
    );
    handle.abort();
}

/// A probe FAILURE (`None`) is not an observation. The span here is tiny on
/// purpose: treating `None` as an empty snapshot WOULD confirm an exit inside
/// the quiet window.
#[tokio::test]
async fn probe_failure_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let uuid = "01000000-0000-7000-8000-0000000000af";
    let span = Duration::from_millis(200);
    let (probe, mut rx, transcript, handle) = admitted_with_mutable_probe(
        dir.path().to_path_buf(),
        uuid,
        span,
        vouch_snapshot(&[uuid]),
    )
    .await;
    let expected = AgentId::from_parts("codex", uuid);

    let first_failure = probe.answer(None);

    let events = probe
        .listen_past(&mut rx, first_failure, span, "the failing probe")
        .await;
    assert!(
        !ends(&events, expected),
        "a probe failure must never confirm an exit — None is not an observation"
    );

    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("{}\n", codex_work_line()).as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    collect_until(
        &mut rx,
        "a fresh append must still walk normally while the probe is failing",
        |e| matches!(e, AgentEvent::ToolUseStart { agent_id, .. } if *agent_id == expected),
    )
    .await;
    handle.abort();
}

/// A probe that PANICS must fold into the same fail-safe as a `None`, via
/// `spawn_blocking`'s `JoinError` — the `Err(join_err) => warn!` arm the
/// `None` path never reaches.
#[tokio::test]
async fn probe_panic_changes_nothing() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = TempDir::new().unwrap();
    let uuid = "01000000-0000-7000-8000-0000000000b1";
    let projects_root = dir.path().to_path_buf();
    let project_dir = projects_root.join("proj-panic");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    let stale = project_dir.join(format!("{uuid}.jsonl"));
    write_lines(&stale, &[codex_meta_line(uuid, "/repo")]).await;
    backdate(&stale, 7200);

    let boom = Arc::new(AtomicBool::new(false));
    let probe_boom = boom.clone();
    let probe = Probe::new(vouch_snapshot(&[uuid]));
    let asked = probe.clone();
    // Short, so a panic taken for an empty snapshot confirms inside the wait.
    let span = Duration::from_millis(200);
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let watcher = codex_watcher(projects_root)
        .with_initial_window(Duration::from_secs(60))
        .with_poll_interval(PROBE_POLL)
        .with_negative_vouch_min_span(span)
        .with_liveness_probe(Arc::new(move || {
            let snap = asked.ask();
            if probe_boom.load(Ordering::SeqCst) {
                panic!("probe boom");
            }
            snap
        }));
    let handle = tokio::spawn(async move { watcher.run(tx).await });

    let expected = AgentId::from_parts("codex", uuid);
    collect_until(
        &mut rx,
        "the probe-vouched stale transcript must register while the probe is healthy",
        |e| start_id(e) == Some(expected),
    )
    .await;

    boom.store(true, Ordering::SeqCst);
    // Read after the store, so every ask from this index on panics.
    let first_panic = probe.asks();
    let events = probe
        .listen_past(&mut rx, first_panic, span, "the panicking probe")
        .await;
    assert!(
        !ends(&events, expected),
        "a probe panic must fold into the fail-safe, never confirm an exit"
    );
    assert!(
        !handle.is_finished(),
        "a probe panic must not crash the watcher task"
    );
    handle.abort();
}

/// A probe snapshot binds the vouched session id to the pid holding it; when
/// that sole holder dies, the kernel watch (kqueue NOTE_EXIT / pidfd+poll) emits
/// the SessionEnd within milliseconds. The [`UNREACHED_VOUCH_SPAN`] span this test passes is
/// past every wait, so a SessionEnd can ONLY have come from the instant-exit
/// path.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn instant_exit_emits_session_end_when_bound_pid_dies() {
    let dir = TempDir::new().unwrap();
    let uuid = "01000000-0000-7000-8000-0000000000b0";
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    let (probe, mut rx, transcript, handle) = admitted_with_mutable_probe(
        dir.path().to_path_buf(),
        uuid,
        UNREACHED_VOUCH_SPAN,
        vouch_snapshot_with_pids(&[uuid], &[pid]),
    )
    .await;
    let expected = AgentId::from_parts("codex", uuid);

    // Flip the probe FIRST — a real probe stops vouching a dead pid, and the
    // re-vouch sweep would otherwise re-admit the ended session. Under
    // `UNREACHED_VOUCH_SPAN` the flip alone cannot produce a SessionEnd.
    probe.answer(vouch_snapshot(&[]));
    let _ = child.kill();
    let _ = child.wait();

    expect_session_end(
        &mut rx,
        expected,
        "a bound pid dying must SessionEnd on its instant exit, not wait out the negative vouch",
    )
    .await;

    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .await
        .unwrap();
    f.write_all(format!("{}\n", codex_work_line()).as_bytes())
        .await
        .unwrap();
    f.flush().await.unwrap();
    drop(f);

    collect_until(
        &mut rx,
        "an append after the instant exit must re-register the session (seen un-claim)",
        |e| start_id(e) == Some(expected),
    )
    .await;
    handle.abort();
}

/// The pid binding must be UNBOUND when the negative vouch confirms an id: a
/// codex-style process owns many rollouts, so a session can end (rollout fd
/// closed → vouch gone) while its OS process lives on, and that process's
/// later death must emit NOTHING.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn negative_vouch_confirm_unbinds_pid_so_a_later_exit_is_quiet() {
    let dir = TempDir::new().unwrap();
    let uuid = "01000000-0000-7000-8000-0000000000b1";
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    let (probe, mut rx, _transcript, handle) = admitted_with_mutable_probe(
        dir.path().to_path_buf(),
        uuid,
        Duration::from_millis(300),
        vouch_snapshot_with_pids(&[uuid], &[pid]),
    )
    .await;
    let expected = AgentId::from_parts("codex", uuid);

    // The session ends while its process lives on: healthy probe, no vouch.
    probe.answer(vouch_snapshot(&[]));
    expect_session_end(
        &mut rx,
        expected,
        "the negative vouch must confirm the first exit",
    )
    .await;

    // Another of the process's rollouts, whose end is the death's receipt.
    let witness_sid = "01000000-0000-7000-8000-0000000000c1";
    let bound = probe.answer(vouch_snapshot_with_pids(&[witness_sid], &[pid]));
    probe.asked(bound, 2).await;
    let witness = AgentId::from_parts("codex", witness_sid);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        ended_through(&probe, &mut rx, witness).await,
        [witness],
        "a process exit after the id was negative-vouch-confirmed must not re-emit SessionEnd"
    );
    handle.abort();
}

/// The instant-exit arm must purge the dead id from the shared admission set:
/// `live` is only rewritten by a HEALTHY probe refresh, so a probe FAILURE
/// right after the exit would keep the stale snapshot vouching the dead id,
/// and the re-vouch sweep would replay the parked transcript into a phantom
/// SessionStart unreachable by every fast rung.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn instant_exit_under_probe_failure_does_not_resurrect_the_session() {
    let dir = TempDir::new().unwrap();
    let uuid = "01000000-0000-7000-8000-0000000000b2";
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    let (probe, mut rx, _transcript, handle) = admitted_with_mutable_probe(
        dir.path().to_path_buf(),
        uuid,
        UNREACHED_VOUCH_SPAN,
        vouch_snapshot_with_pids(&[uuid], &[pid]),
    )
    .await;
    let expected = AgentId::from_parts("codex", uuid);

    // The probe breaks FIRST: `None` leaves `live` holding the last healthy
    // snapshot, which still vouches the id.
    probe.answer(None);
    let _ = child.kill();
    let _ = child.wait();

    expect_session_end(
        &mut rx,
        expected,
        "the instant exit must emit the SessionEnd",
    )
    .await;

    // A whole failing pass: its re-vouch sweep would reset the parked
    // transcript and its scan replay it.
    let from = probe.asks();
    let after = probe
        .listen_until(&mut rx, "past the instant exit", |at| at.len() >= from + 2)
        .await;
    assert!(
        !after.iter().any(|(_, e)| start_id(e) == Some(expected)),
        "a probe-failure pass after the instant exit must not mint a phantom SessionStart"
    );
    handle.abort();
}

/// An id a later snapshot names under a new pid alone MIGRATES: the old pid,
/// still alive, no longer holds it up, so the new pid's death is its instant exit.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn a_rebound_session_ends_with_its_new_pid_while_the_old_one_lives() {
    let dir = TempDir::new().unwrap();
    let uuid = "01000000-0000-7000-8000-0000000000b3";
    let mut old = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut new = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let old_pid = old.id() as i32;
    let new_pid = new.id() as i32;
    let (probe, mut rx, _transcript, handle) = admitted_with_mutable_probe(
        dir.path().to_path_buf(),
        uuid,
        UNREACHED_VOUCH_SPAN,
        vouch_snapshot_with_pids(&[uuid], &[old_pid]),
    )
    .await;
    let expected = AgentId::from_parts("codex", uuid);

    // The pass that takes the migration folds it before the next one asks.
    let migrated = probe.answer(vouch_snapshot_with_pids(&[uuid], &[new_pid]));
    probe.asked(migrated, 2).await;

    let _ = new.kill();
    let _ = new.wait();
    expect_session_end(
        &mut rx,
        expected,
        "the migrated binding must follow the new pid — its death is the session's instant exit",
    )
    .await;
    let _ = old.kill();
    let _ = old.wait();
    handle.abort();
}

/// One session two live processes hold (a CC session resumed in a second
/// terminal): the first one's death leaves it running, the last one's ends it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn a_session_two_live_pids_hold_ends_only_when_both_die() {
    let dir = TempDir::new().unwrap();
    let uuid = "01000000-0000-7000-8000-0000000000b4";
    let sleeper = || {
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap()
    };
    let (mut first, mut last) = (sleeper(), sleeper());
    // The larger pid dies first, so binding only the largest holder ends it.
    if first.id() < last.id() {
        std::mem::swap(&mut first, &mut last);
    }
    let (first_pid, last_pid) = (first.id() as i32, last.id() as i32);
    // Held by the first pid alone, so its end is that death's receipt.
    let witness_sid = "01000000-0000-7000-8000-0000000000c4";
    let mut held = vouch_snapshot_with_pids(&[uuid], &[first_pid, last_pid]).expect("healthy");
    held.bind_pid(witness_sid.to_owned(), first_pid);
    let witness = AgentId::from_parts("codex", witness_sid);
    let (probe, mut rx, _transcript, handle) = admitted_with_mutable_probe(
        dir.path().to_path_buf(),
        uuid,
        UNREACHED_VOUCH_SPAN,
        Some(held),
    )
    .await;
    let expected = AgentId::from_parts("codex", uuid);

    // The pass that admitted it folded both bindings first; the probe still
    // lists the first pid when its exit lands, as the kernel watch outruns a refresh.
    let _ = first.kill();
    let _ = first.wait();
    assert_eq!(
        ended_through(&probe, &mut rx, witness).await,
        [witness],
        "a holder's death must not end a session another live pid holds"
    );

    probe.answer(vouch_snapshot_with_pids(&[uuid], &[last_pid]));
    let _ = last.kill();
    let _ = last.wait();
    expect_session_end(
        &mut rx,
        expected,
        "the last holder's death is the session's instant exit",
    )
    .await;
    handle.abort();
}
