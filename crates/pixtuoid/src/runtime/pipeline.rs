//! The ONE source→reducer pipeline spine both painters boot through.
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
use super::{ConnectedSources, TransitionLog};

/// Where [`reducer_task`] publishes the scene after each change it applies.
pub(crate) enum SceneSink {
    /// A painter's: it draws the newest scene, so one it never read is gone.
    Latest(watch::Sender<Arc<SceneState>>),
    /// Headless's: each change, as its [`TransitionLog`] line.
    Transitions(mpsc::UnboundedSender<String>, TransitionLog),
}

impl SceneSink {
    pub(crate) fn transitions(tx: mpsc::UnboundedSender<String>) -> Self {
        Self::Transitions(tx, TransitionLog::default())
    }

    /// `false` once the reader is gone.
    pub(super) fn publish(&mut self, scene: &SceneState) -> bool {
        match self {
            Self::Latest(tx) => tx.send(Arc::new(scene.clone())).is_ok(),
            Self::Transitions(tx, log) => match log.line(scene) {
                Some(line) => tx.send(line).is_ok(),
                None => !tx.is_closed(),
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
    let sources = build_source_set(socket_path, Some(presence_tx));

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
