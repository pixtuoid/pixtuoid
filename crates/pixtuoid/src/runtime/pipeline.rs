//! The ONE source→reducer pipeline spine both painters and `serve` boot through.
//!
//! What deliberately STAYS caller-side, because the two painters genuinely
//! diverge there:
//! - **`boot_caps`**: the TUI measures the terminal (footer-subtracting,
//!   cap-clamped, headless arms); floating measures its window pixels — a
//!   deliberate divergence (reusing the TUI math over-seeds and strands agents on
//!   non-existent desks), so the pipeline takes the seed as a PARAMETER and must
//!   never compute it. It is also WHEN they diverge: floating cannot seed until a
//!   real window exists, so it calls this from `window::resumed`, not at boot.
//! - **socket resolution + `ConnectedSources`**: both values outlive the boot (the
//!   Sources panel displays the path and mutates the live set), so the callers own
//!   them and hand in a clone.
//!
//! Requires an ambient tokio runtime context — `run_async` executes inside
//! `block_on`, floating holds an `rt.enter()` guard — so the `tokio::spawn`s here
//! land on the caller's runtime either way.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use pixtuoid_core::source::daemon;
use pixtuoid_core::source::manager::{SourceDeath, SourceManager};
use pixtuoid_core::state::MAX_FLOORS;
use pixtuoid_core::{AgentEvent, SceneState, Transport};
use tokio::sync::{mpsc, watch};

use super::driver::{build_source_set, reducer_task};
use super::today::DayCount;
use super::{ConnectedSources, LogQueue, SceneRx, TransitionLog};

/// Where [`reducer_task`] publishes the scene after each change it applies.
pub(crate) enum SceneSink {
    /// A painter's or `serve`'s: it reads the newest scene, so one it never read
    /// is gone.
    Latest(watch::Sender<Arc<SceneState>>),
    /// Headless's: each change, as its [`TransitionLog`] line.
    Transitions(LogQueue, TransitionLog),
}

/// Headless's end of the transition log: the lines to write, and the queue
/// its own health lines join.
pub(crate) struct HeadlessLog {
    pub(crate) lines: mpsc::Receiver<String>,
    pub(crate) queue: LogQueue,
}

impl SceneSink {
    /// A sink for the newest scene, and the watch it's read from, seeded with
    /// the empty boot scene.
    pub(crate) fn latest(boot_caps: [usize; MAX_FLOORS]) -> (Self, SceneRx) {
        let (tx, rx) = watch::channel(Arc::new(SceneState::new(boot_caps)));
        (Self::Latest(tx), rx)
    }

    /// Headless's sink, and the log it fills, `capacity` lines deep.
    pub(crate) fn transitions(capacity: usize) -> (Self, HeadlessLog) {
        let (queue, lines) = LogQueue::bounded(capacity);
        (
            Self::Transitions(queue.clone(), TransitionLog::default()),
            HeadlessLog { lines, queue },
        )
    }

    /// `false` once the reader is gone or stalled.
    pub(super) fn publish(&mut self, scene: &SceneState) -> bool {
        match self {
            // A scene equal to the one its readers hold notifies nobody: each
            // notification is a floating repaint and a frame per `serve` device,
            // and a quiet sweep tick or a read's `Backlog` changes nothing they show.
            Self::Latest(tx) => {
                tx.send_if_modified(|held| {
                    let changed = **held != *scene;
                    if changed {
                        *held = Arc::new(scene.clone());
                    }
                    changed
                });
                !tx.is_closed()
            }
            Self::Transitions(queue, log) => match log.line(scene) {
                Some(line) => queue.push(line),
                None => !queue.is_closed(),
            },
        }
    }
}

/// The live pipeline's caller-facing handles; the scene reaches the caller
/// through the [`SceneSink`] it handed in.
pub(crate) struct Pipeline {
    pub(crate) health_rx: watch::Receiver<Vec<SourceDeath>>,
    pub(crate) floor_caps: Arc<[AtomicUsize; MAX_FLOORS]>,
}

/// Wire and spawn the whole live pipeline: presence channel + exit watch →
/// [`build_source_set`] → event/health channels → boot-seeded floor-caps
/// atomics → [`reducer_task`] publishing to `scene` →
/// `SourceManager::spawn_with_health`.
pub(crate) fn spawn_pipeline(
    socket_path: PathBuf,
    connected: ConnectedSources,
    boot_caps: [usize; MAX_FLOORS],
    scene: SceneSink,
) -> Pipeline {
    // Daemon-presence SIDE channel (invariant #2: NOT the AgentEvent channel).
    // The demux sends source-tagged presence deltas here and the exit watch drains
    // gateway-pid deaths into the SAME channel; the reducer task merges both.
    let (presence_tx, presence_rx) = mpsc::unbounded_channel();
    let presence_exit_watch = daemon::spawn_presence_exit_watch(presence_tx.clone());
    let sources = build_source_set(socket_path, Some(presence_tx), connected.clone());

    let (tx, rx) =
        mpsc::channel::<(Transport, AgentEvent)>(pixtuoid_core::source::EVENT_CHANNEL_CAPACITY);
    let floor_caps: Arc<[AtomicUsize; MAX_FLOORS]> =
        Arc::new(std::array::from_fn(|i| AtomicUsize::new(boot_caps[i])));
    tokio::spawn(reducer_task(
        rx,
        scene,
        Arc::clone(&floor_caps),
        connected,
        presence_rx,
        presence_exit_watch,
        DayCount::local(),
    ));

    // Source-health side channel: a fatal source exit must reach the painter's
    // footer. Deliberately NOT an AgentEvent — the one event channel carries agent
    // activity (its Transport tag drives hook-wins dedup), not source lifecycle.
    let (health_tx, health_rx) = watch::channel(Vec::new());
    let mut manager = SourceManager::new();
    for src in sources {
        manager = manager.with_source(src);
    }
    manager.spawn_with_health(tx, health_tx);

    Pipeline {
        health_rx,
        floor_caps,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::time::SystemTime;

    use pixtuoid_core::{AgentId, Reducer};

    use super::*;
    use crate::runtime::gate;

    fn connected() -> ConnectedSources {
        ConnectedSources::new(["claude-code".to_string()].into())
    }

    fn apply(reducer: &mut Reducer, scene: &mut SceneState, ev: AgentEvent, at: SystemTime) {
        gate::apply_gated_event(
            reducer,
            scene,
            ev,
            Transport::Jsonl,
            &connected(),
            at,
            &mut HashSet::new(),
        );
    }

    /// The 1-Hz sweep of a quiet office, a read's re-registered start and its
    /// backlog each publish a scene equal to the one the reader holds.
    #[test]
    fn a_publish_reaches_the_reader_only_when_the_scene_changed() {
        let caps = [8; MAX_FLOORS];
        let (mut sink, mut rx) = SceneSink::latest(caps);
        let mut notified = |scene: &SceneState| {
            assert!(sink.publish(scene), "the reader lives");
            let changed = rx.has_changed().expect("the sink lives");
            rx.borrow_and_update();
            changed
        };
        let id = AgentId::from_parts("claude-code", "s");
        let start = || AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        };
        let at = SystemTime::now();
        let mut reducer = Reducer::new();
        let mut scene = SceneState::new(caps);

        apply(&mut reducer, &mut scene, start(), at);
        assert!(notified(&scene), "a registration is a change");
        gate::reconcile_sweep_tick(&mut reducer, &mut scene, &connected(), at);
        assert!(!notified(&scene), "the sweep tick of a quiet office");
        apply(&mut reducer, &mut scene, start(), at);
        assert!(
            !notified(&scene),
            "a re-registered start at its slot's own clock"
        );
        let backlog = AgentEvent::Backlog {
            agent_id: id,
            replaying: false,
        };
        apply(&mut reducer, &mut scene, backlog, at);
        assert!(!notified(&scene), "a read's backlog");
    }
}
