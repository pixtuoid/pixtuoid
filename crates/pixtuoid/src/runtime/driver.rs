//! The async runtime glue: builds the tokio runtime, spawns the reducer
//! task + sources, binds the hook socket, and drives either the TUI or the
//! headless summary loop until a quit signal ([`super::QuitArms`]) or, headless,
//! until the summary's reader leaves.
//!
//! `run` and `run_async` are structurally unreachable by a headless test (a
//! real tokio runtime + `block_on` + the quit arms + socket bind), so this file is
//! coverage-excluded and stays a shell: a DECISION lives in a covered sibling,
//! as the connection gate does in [`super::gate`], or behind an injected seam
//! tested here, as the headless loop's signal and stdout are.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use pixtuoid_core::source::DynSource;
use pixtuoid_core::source::antigravity::AntigravitySource;
use pixtuoid_core::source::claude_code::ClaudeCodeSource;
use pixtuoid_core::source::codex::CodexSource;
use pixtuoid_core::source::copilot::CopilotSource;
use pixtuoid_core::source::daemon::{self, PresenceMsg};
use pixtuoid_core::source::grok::GrokSource;
use pixtuoid_core::source::hook::HookRouter;
use pixtuoid_core::source::jsonl::ChildEndUnclaims;
use pixtuoid_core::source::omp::OmpSource;
use pixtuoid_core::state::MAX_FLOORS;
use pixtuoid_core::{Reducer, SceneState, TaggedReceiver};
use tokio::sync::mpsc;

use super::gate;
use super::pipeline::{HeadlessLog, SceneSink};
use super::{ConnectedSources, QuitArms, QuitSignal, RunConfig, resolve_boot_caps};

/// Boot the TUI (unless headless) and drive the source pipeline until it exits;
/// the quit signal that ended it, for the caller to re-raise.
///
/// # Errors
///
/// If the sprite pack fails to load, the tokio runtime cannot be built, or the headless loop or TUI session fails.
pub(crate) fn run(cfg: RunConfig) -> Result<Option<QuitSignal>> {
    // Before tokio and the boot caps: the query reads the terminal while no
    // other thread does, and once the cutaway paints, the boot seed is the
    // plan's geometry.
    let tui = if cfg.headless {
        None
    } else {
        Some(boot_tui(&cfg)?)
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move { run_async(cfg, tui).await })
}

/// The TUI's pack, the plan for painting it, and how its office moves.
type Boot = (
    Arc<pixtuoid_scene::pack::OfficeArt>,
    crate::graphics::Plan,
    pixtuoid_scene::anim::Motion,
);

/// Load the pack, whose densest art the plan fits, plan, and pick the motion
/// the plan affords unless the config names one.
fn boot_tui(cfg: &RunConfig) -> Result<Boot> {
    let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack()?);
    let plan = crate::graphics::plan_this_terminal(cfg.look, pack.max_density_variant(), true);
    tracing::info!(look = ?cfg.look, plan = ?plan, "graphics plan");
    let motion = cfg.motion.or(plan.motion(crate::graphics::Link::of_env()));
    tracing::info!(mode = ?cfg.motion, motion = ?motion, "motion");
    Ok((pack, plan, motion))
}

async fn run_async(cfg: RunConfig, tui: Option<Boot>) -> Result<Option<QuitSignal>> {
    let RunConfig {
        socket,
        desk_cap,
        headless: _,
        config_path,
        theme,
        pets,
        connected,
        log,
        drift,
        first_run,
        audio,
        look: _,
        motion: _,
    } = cfg;
    // Audio owns no state here: `run_tui` builds the AudioController, which owns
    // the device thread and tears it down on Drop at any exit.
    let connected = ConnectedSources::new(connected);
    let socket_path = socket.unwrap_or_else(ClaudeCodeSource::default_socket_path);
    // The terminal-size query stays here in the shell (the injected `measure`);
    // the policy is the covered + mutation-tested `resolve_boot_caps`.
    let boot_caps = resolve_boot_caps(
        desk_cap,
        tui.as_ref().map(|(_, plan, _)| *plan),
        crate::graphics::terminal_cells,
    );
    // The shared spine — ONE authority with `floating::run`. The tasks live on
    // this fn's runtime.
    match tui {
        None => {
            let (sink, log) = SceneSink::transitions(super::TRANSITION_LOG_CAPACITY);
            let pipeline = super::pipeline::spawn_pipeline(socket_path, connected, boot_caps, sink);
            headless_loop(log, pipeline.health_rx).await
        }
        Some((pack, plan, motion)) => {
            let (sink, scene_rx) = SceneSink::latest(boot_caps);
            let super::pipeline::Pipeline {
                health_rx,
                floor_caps,
            } = super::pipeline::spawn_pipeline(
                socket_path.clone(),
                connected.clone(),
                boot_caps,
                sink,
            );
            crate::tui::run_tui(crate::tui::TuiSession {
                scene_rx,
                pack,
                plan,
                motion,
                floor_caps,
                theme,
                config_path,
                desk_cap,
                pets,
                source_health: health_rx,
                socket_path,
                connected,
                log,
                drift,
                first_run,
                audio_cfg: audio,
            })
            .await
        }
    }
}

/// Build the runtime source set `run_async` spawns — the ONE place that set is
/// constructed. Imperative rather than a registry-driven loop: cc, codex and
/// grok share one `ChildEndUnclaims` the others don't take. Hook-only sources + the daemon (OpenClaw) are absent by
/// design — they ride the router's shared socket.
pub(crate) fn build_source_set(
    socket_path: PathBuf,
    presence_tx: Option<daemon::PresenceSender>,
) -> Vec<Box<dyn DynSource>> {
    let mut cc_src = ClaudeCodeSource::default_paths();
    let ag_src = AntigravitySource::default_paths();
    let copilot_src = CopilotSource::default_paths();
    let omp_src = OmpSource::default_paths();

    let mut codex_src = CodexSource::default_paths();

    // The hook tee is the sole PRODUCER, and each watcher drains only ids whose
    // transcripts it claims (AgentId is source-namespaced), so a Codex child waits
    // for the Codex watcher even though the router decoded its hook.
    let child_end_unclaims = ChildEndUnclaims::new();
    cc_src.child_end_unclaims = Some(child_end_unclaims.clone());
    codex_src.child_end_unclaims = Some(child_end_unclaims.clone());

    // grok consumes too: its subagent_stop/end hooks decode to Hook-transport
    // `SessionEnd{as_child:true}`, the tee's trigger.
    let mut grok_src = GrokSource::default_paths();
    grok_src.child_end_unclaims = Some(child_end_unclaims.clone());

    let hook_router = HookRouter::new(socket_path)
        .with_child_end_unclaims(Some(child_end_unclaims))
        .with_presence_tx(presence_tx);

    vec![
        Box::new(hook_router) as Box<dyn DynSource>,
        Box::new(cc_src),
        Box::new(ag_src),
        Box::new(codex_src),
        Box::new(copilot_src),
        Box::new(omp_src),
        Box::new(grok_src),
    ]
}

/// The reducer event loop: gate + apply incoming `AgentEvent`s, merge daemon
/// presence, and run the 1-Hz reconcile sweep — an async shell over
/// [`super::gate`], which owns every decision.
pub(crate) async fn reducer_task(
    mut rx: TaggedReceiver,
    mut sink: SceneSink,
    floor_caps: Arc<[AtomicUsize; MAX_FLOORS]>,
    connected: ConnectedSources,
    mut presence_rx: tokio::sync::mpsc::UnboundedReceiver<PresenceMsg>,
    presence_exit_watch: Option<daemon::PresenceExitWatch>,
) {
    let mut reducer = Reducer::new();
    // Disabled once the presence channel closes (all senders dropped) so its
    // `recv() -> None` branch can't busy-loop the select.
    let mut presence_open = true;
    // `&'static str`, NOT the raw wire name: `_pixtuoid_source` arrives verbatim
    // from socket JSON with no registry check and no length cap, so keying on it
    // lets a long-lived `run` accumulate an entry per distinct name seen.
    let mut gate_logged: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    let initial_caps: [usize; MAX_FLOORS] =
        std::array::from_fn(|i| floor_caps[i].load(Ordering::Relaxed));
    let mut scene = SceneState::new(initial_caps);
    if !sink.publish(&scene) {
        return;
    }
    // Ticks so exit-grace sweeps run even when no new events arrive.
    const SWEEP_TICK_INTERVAL_SECS: u64 = 1;
    let mut sweep_interval = tokio::time::interval(Duration::from_secs(SWEEP_TICK_INTERVAL_SECS));
    sweep_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        // Sync per-floor capacities from the shared atomics so the
        // auto-computed layout capacity propagates to next_free_desk().
        for (i, a) in floor_caps.iter().enumerate() {
            scene.floor_capacities[i] = a.load(Ordering::Relaxed);
        }
        tokio::select! {
            event = rx.recv() => {
                let Some((transport, ev)) = event else { break };
                // A gated event mutates nothing, so publish only when it applied.
                if gate::apply_gated_event(
                    &mut reducer,
                    &mut scene,
                    ev,
                    transport,
                    &connected,
                    SystemTime::now(),
                    &mut gate_logged,
                ) && !sink.publish(&scene)
                {
                    tracing::warn!("scene reader gone or stalled — reducer stopping");
                    break;
                }
            }
            // Merged into SceneState::daemons, NEVER through the AgentId-pure
            // `Reducer::apply`. Routed by `DaemonInstanceKey`, so N instances of one
            // daemon need no per-source special-casing.
            update = presence_rx.recv(), if presence_open => {
                match update {
                    Some(PresenceMsg { key, delta }) => {
                        if let gate::PresenceGate::Applied { arm_pid } = gate::apply_gated_presence(
                            &mut scene,
                            &key,
                            delta,
                            &connected,
                            SystemTime::now(),
                        ) {
                            // Arming AFTER apply_presence is safe: the two touch
                            // DISJOINT state (the exit-watch pid map vs
                            // scene.daemons), and a dead pid's synthesized PidExited
                            // re-enters only on a LATER select iteration.
                            if let (Some(ew), Some(pid)) = (presence_exit_watch.as_ref(), arm_pid) {
                                ew.watch(&key, pid);
                            }
                            if !sink.publish(&scene) {
                                tracing::warn!("scene reader gone or stalled — reducer stopping");
                                break;
                            }
                        }
                    }
                    None => presence_open = false,
                }
            }
            _ = sweep_interval.tick() => {
                gate::reconcile_sweep_tick(&mut reducer, &mut scene, &connected, SystemTime::now());
                if !sink.publish(&scene) {
                    tracing::warn!("scene reader gone or stalled — reducer stopping");
                    break;
                }
            }
        }
    }
}

async fn headless_loop(
    log: HeadlessLog,
    health_rx: tokio::sync::watch::Receiver<Vec<pixtuoid_core::source::manager::SourceDeath>>,
) -> Result<Option<QuitSignal>> {
    headless_loop_with_signal(
        log,
        health_rx,
        Box::pin(QuitArms::arm().signalled()),
        std::io::stdout(),
    )
    .await
}

/// `Ok(None)` when the reader left (`| head`) rather than a quit signal; an
/// error when it stopped reading.
async fn headless_loop_with_signal(
    log: HeadlessLog,
    mut health_rx: tokio::sync::watch::Receiver<Vec<pixtuoid_core::source::manager::SourceDeath>>,
    mut quit: std::pin::Pin<Box<dyn std::future::Future<Output = QuitSignal> + Send>>,
    out: impl std::io::Write + Send + 'static,
) -> Result<Option<QuitSignal>> {
    tracing::info!("pixtuoid headless mode — Ctrl-C to quit");
    let HeadlessLog { mut lines, queue } = log;
    // Stdout gets its own thread: a write into a pipe nobody drains parks that
    // thread, never this loop, which must still end the run.
    let (written_tx, mut written) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("headless-stdout".into())
        .spawn(move || {
            let _ = written_tx.send(write_log(&mut lines, out));
        })?;
    // Headless has no TUI footer and no stderr subscriber guarantee, so source
    // deaths must surface in the summary stream or a dead transport reads as a
    // silently empty office. Tracked by count: the watch Vec only grows.
    let mut deaths_seen = 0usize;
    loop {
        tokio::select! {
            () = queue.stalled() => anyhow::bail!(
                "the headless reader stopped reading; {} transitions unwritten",
                queue.unwritten()
            ),
            written = &mut written => {
                written.map_err(|_| anyhow::anyhow!("the headless stdout writer panicked"))??;
                tracing::info!("stdout closed — shutting down");
                return Ok(None);
            }
            Ok(()) = health_rx.changed() => {
                let deaths = health_rx.borrow_and_update().clone();
                for d in super::unseen_deaths(&deaths, &mut deaths_seen) {
                    queue.push(super::format_source_death(d));
                }
            }
            signal = &mut quit => {
                tracing::info!("shutting down");
                return Ok(Some(signal));
            }
        }
    }
}

/// Write `lines` to `out` until its reader leaves, since headless mode exists
/// to feed this stream (`| head` ends the run).
fn write_log(lines: &mut mpsc::Receiver<String>, out: impl std::io::Write) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = crate::CliOut::new(out);
    while let Some(line) = lines.blocking_recv() {
        writeln!(out, "{line}")?;
        if out.closed() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{FALLBACK_DESKS, TRANSITION_LOG_CAPACITY};
    use pixtuoid_core::source::manager::SourceDeath;
    use tokio::sync::watch;

    type HealthPair = (
        watch::Sender<Vec<SourceDeath>>,
        watch::Receiver<Vec<SourceDeath>>,
    );

    fn health() -> HealthPair {
        watch::channel(Vec::new())
    }

    /// A reader that never drains: every write parks forever.
    struct Stalled;

    impl std::io::Write for Stalled {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            loop {
                std::thread::park();
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A session start, then `calls` calls that each start and end: a scene
    /// change per event, queued as one batch.
    async fn batch(calls: usize) -> pixtuoid_core::TaggedReceiver {
        use pixtuoid_core::AgentId;
        use pixtuoid_core::source::{AgentEvent, Transport};
        let id = AgentId::from_parts("claude-code", "s");
        let mut events = vec![AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/proj"),
            parent_id: None,
        }];
        for n in 0..calls {
            events.push(AgentEvent::ActivityStart {
                agent_id: id,
                tool_use_id: Some(format!("t{n}")),
                detail: Some("Bash".into()),
            });
            events.push(AgentEvent::ActivityEnd {
                agent_id: id,
                tool_use_id: Some(format!("t{n}")),
            });
        }
        let (tx, rx) = tokio::sync::mpsc::channel(events.len());
        for ev in events {
            tx.send((Transport::Jsonl, ev))
                .await
                .expect("the batch queues");
        }
        rx
    }

    /// Folds `events` to its end, publishing to `sink`.
    async fn reduce(events: pixtuoid_core::TaggedReceiver, sink: SceneSink) {
        let (_presence_tx, presence_rx) = tokio::sync::mpsc::unbounded_channel();
        reducer_task(
            events,
            sink,
            Arc::new(std::array::from_fn(|_| AtomicUsize::new(FALLBACK_DESKS))),
            ConnectedSources::new(["claude-code".to_string()].into()),
            presence_rx,
            None,
        )
        .await;
    }

    #[test]
    fn build_source_set_wires_every_transcript_bearing_source_plus_the_hook_router() {
        use pixtuoid_core::source::registry::{self, descriptor_for};
        use std::collections::BTreeSet;

        let sources = build_source_set(PathBuf::from("/tmp/pixtuoid-test.sock"), None);
        let built: BTreeSet<&str> = sources.iter().map(|s| s.name()).collect();

        // The HookRouter is infrastructure, not a registered CLI: it has no
        // descriptor, so it is excluded from the transcript check below.
        assert!(
            built.contains("hook-router"),
            "the shared-socket HookRouter must be spawned (else hook signals never decode)"
        );

        let transcript_built: BTreeSet<&str> = built
            .iter()
            .copied()
            .filter(|&n| n != "hook-router")
            .collect();
        let expected: BTreeSet<&str> = registry::registered_source_names()
            .filter(|&name| descriptor_for(name).is_some_and(|d| d.line_decoder().is_some()))
            .collect();
        assert_eq!(
            transcript_built, expected,
            "run_async's transcript-source wiring diverged from the registry: a \
             transcript-bearing source is registered but not built (it would never \
             spawn), or a built source isn't registered"
        );
    }

    /// A transcript read hands over a call's start and end together: both
    /// states reach the log, though no scene shows the first.
    #[tokio::test]
    async fn a_call_that_starts_and_ends_in_one_batch_logs_both_states() {
        let (sink, mut log) = SceneSink::transitions(TRANSITION_LOG_CAPACITY);
        reduce(batch(1).await, sink).await;
        let mut lines = Vec::new();
        while let Ok(line) = log.lines.try_recv() {
            lines.push(line);
        }
        assert_eq!(
            lines,
            [
                "agents=[] daemons=[]",
                "agents=[cc\u{b7}proj@0:idle] daemons=[]",
                "agents=[cc\u{b7}proj@0:active(Bash)] daemons=[]",
                "agents=[cc\u{b7}proj@0:idle] daemons=[]",
            ]
        );
    }

    /// The reducer outruns a reader that stopped reading: it never waits on
    /// the reader, and the run ends with an error, the log still complete up to
    /// where it stopped.
    #[tokio::test]
    async fn a_stalled_reader_ends_the_run_and_never_blocks_the_reducer() {
        const CAPACITY: usize = 2;
        let (sink, log) = SceneSink::transitions(CAPACITY);
        tokio::time::timeout(Duration::from_secs(5), reduce(batch(CAPACITY).await, sink))
            .await
            .expect("the reducer never blocks on its reader");
        let (_health_tx, health_rx) = health();
        let err =
            headless_loop_with_signal(log, health_rx, Box::pin(std::future::pending()), Stalled)
                .await
                .expect_err("a stalled reader ends the run");
        let msg = err.to_string();
        assert!(
            msg.starts_with("the headless reader stopped reading; ")
                && msg.ends_with(" transitions unwritten"),
            "{msg}"
        );
    }

    /// No line is merged or dropped for a reader that drains as it goes.
    #[tokio::test]
    async fn a_reader_that_keeps_up_gets_every_line() {
        const LINES: usize = 10_000;
        let (_sink, log) = SceneSink::transitions(TRANSITION_LOG_CAPACITY);
        let sent: Vec<String> = (0..LINES).map(|n| format!("line {n}")).collect();
        for line in &sent {
            assert!(log.queue.push(line.clone()));
        }
        let out = crate::test_capture::Buf::default();
        let (quit_tx, quit_rx) = tokio::sync::oneshot::channel::<()>();
        let (_health_tx, health_rx) = health();
        let run = tokio::spawn(headless_loop_with_signal(
            log,
            health_rx,
            Box::pin(async move {
                let _ = quit_rx.await;
                QuitSignal::test()
            }),
            out.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            while out.text().lines().count() < LINES {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the writer drains the queue");
        let _ = quit_tx.send(());
        assert!(matches!(run.await, Ok(Ok(Some(_)))));
        assert_eq!(out.text().lines().collect::<Vec<_>>(), sent);
    }

    #[tokio::test]
    async fn headless_loop_shuts_down_on_a_delivered_signal() {
        let (_sink, log) = SceneSink::transitions(TRANSITION_LOG_CAPACITY);
        let (_health_tx, health_rx) = health();
        let fired = headless_loop_with_signal(
            log,
            health_rx,
            Box::pin(async { QuitSignal::test() }),
            std::io::sink(),
        )
        .await
        .expect("a delivered quit signal is a clean shutdown");
        assert_eq!(fired, Some(QuitSignal::test()));
    }

    #[tokio::test]
    async fn headless_loop_ends_when_the_reader_leaves() {
        let (_sink, log) = SceneSink::transitions(TRANSITION_LOG_CAPACITY);
        assert!(log.queue.push("agents=[] daemons=[]".into()));
        let (_health_tx, health_rx) = health();
        let res = tokio::time::timeout(
            Duration::from_secs(5),
            headless_loop_with_signal(
                log,
                health_rx,
                Box::pin(std::future::pending()),
                crate::test_io::FailOnce::new(0, std::io::ErrorKind::BrokenPipe),
            ),
        )
        .await;
        assert!(
            matches!(res, Ok(Ok(None))),
            "a closed stdout is a clean end of the run, got {res:?}"
        );
    }
}
