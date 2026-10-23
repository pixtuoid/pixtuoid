//! `pixtuoid floating` — the frameless, always-on-top desktop window that renders the
//! live office (every agent across every connected CLI) without opening the TUI.
//!
//! A binary-only front-end on the shared engine: it boots the SAME
//! `runtime::pipeline::spawn_pipeline` spine the TUI uses — from
//! `window::resumed` rather than [`run`], because the desk-capacity seed needs
//! the REAL window size (see `PipelineBoot`) — but presents each frame as a
//! composition of layers ([`compose`]), the [`offscreen::OfficeRenderer`]
//! office and its chrome, drawn by the GPU into a `winit` window instead of
//! half-block terminal cells.
//! `pixtuoid-core` stays window-free (invariant #1) — all windowing lives here.

mod cadence;
pub(crate) mod compose;
#[cfg(test)]
mod fixtures;
pub(crate) mod geometry;
mod gpu;
mod input;
mod lifecycle;
pub(crate) mod offscreen;
pub(crate) mod overlays;
mod window;

use anyhow::{Context, Result};
use pixtuoid_core::source::claude_code::ClaudeCodeSource;
use winit::event_loop::EventLoop;

use crate::config;
use crate::runtime::{ConnectedSources, RunConfig};
use window::{FloatingApp, FloatingEvent};

/// Open the floating window and drive it until the user closes it.
///
/// `winit`'s event loop must own the main thread, so the source pipeline runs on a
/// background tokio runtime (spawned, NEVER `block_on` — that would stall the window),
/// and scene changes reach the loop via an `EventLoopProxy`. BLOCKS until the window
/// closes; `rt`, which the source tasks run on, lives across the call.
///
/// # Errors
///
/// If the sprite pack cannot be loaded, the tokio runtime or the winit event loop cannot be built, the event loop exits with an error, or the window or its GPU fails to start or is lost.
pub(crate) fn run(cfg: RunConfig) -> Result<Option<crate::runtime::QuitSignal>> {
    let RunConfig {
        socket,
        theme,
        pets,
        connected,
        config_path,
        audio,
        motion,
        drift,
        log,
        first_run,
        ..
    } = cfg;
    let app_config = config::load(&config_path, &mut Vec::new());
    let pack = pixtuoid_scene::pack::load_bundled_pack()
        .context("loading the bundled sprite pack for the floating window")?;
    let min = geometry::min_window(pack.max_density_variant());
    let floating_cfg = config::resolve_floating(&app_config).at_least(min.width, min.height);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the floating tokio runtime")?;
    let connected = ConnectedSources::new(connected);
    let socket_path = socket.unwrap_or_else(ClaudeCodeSource::default_socket_path);

    let mut builder = EventLoop::<FloatingEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        // Accessory: no Dock icon, doesn't steal focus — an ambient companion.
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let event_loop = builder
        .build()
        .context("building the floating event loop")?;
    let proxy = event_loop.create_proxy();
    let quit = {
        // The unix handlers register with the runtime's signal driver.
        let _in_rt = rt.enter();
        crate::runtime::QuitArms::arm().signalled()
    };
    let quit_proxy = proxy.clone();
    let fired = std::sync::Arc::new(std::sync::OnceLock::new());
    let fired_by = std::sync::Arc::clone(&fired);
    rt.spawn(async move {
        let _ = fired_by.set(quit.await);
        let _ = quit_proxy.send_event(FloatingEvent::Quit);
    });

    // After every fallible `?` boot step: `app` owns the audio device thread (see
    // `crate::audio::AudioController::new`).
    let mut app = FloatingApp::new(
        floating_cfg,
        window::Appearance {
            theme,
            motion: motion.or(pixtuoid_scene::anim::Motion::Full),
        },
        pack,
        pets,
        window::Settings {
            config_path,
            audio,
            log,
            first_run,
        },
        PipelineBoot {
            socket_path,
            connected,
            proxy,
            rt: rt.handle().clone(),
            drift,
        },
    );
    let ran = event_loop.run_app(&mut app);
    // The window's own failure is the cause; a loop error after it is not.
    if let Some(failure) = app.into_failure() {
        return Err(failure);
    }
    ran.context("running the floating window event loop")?;
    Ok(fired.get().copied())
}

/// Everything the pipeline needs, held by [`FloatingApp`] until `resumed` can
/// supply the one input `run` cannot: the REAL window size.
///
/// `run` genuinely cannot do better: winit 0.30 exposes `primary_monitor` only
/// on `ActiveEventLoop`, which does not exist until `run_app` is already
/// driving, and `office_scale` ROUNDS, so no conservative logical-side seed is
/// sound either — the buffer for one logical size is NOT monotone in the scale
/// factor.
///
/// The accepted cost: the hook socket binds AFTER window + surface creation
/// rather than before the event loop. A hook landing inside that window fails to
/// connect and exits 0 silently — the shim's documented never-block contract. In
/// exchange, a window-creation failure no longer leaves a bound socket and a
/// live source set behind.
pub(crate) struct PipelineBoot {
    socket_path: std::path::PathBuf,
    connected: ConnectedSources,
    proxy: winit::event_loop::EventLoopProxy<FloatingEvent>,
    /// How `resumed` enters the runtime to boot the pipeline
    /// (`PipelineBoot::spawn`): `run` holds no guard, and a `tokio::spawn`
    /// outside a runtime panics.
    rt: tokio::runtime::Handle,
    drift: crate::doctor::DriftSeen,
}

/// The pipeline handles, available only once [`PipelineBoot::spawn`] has run.
pub(crate) struct LivePipeline {
    pub(crate) scene_rx: tokio::sync::watch::Receiver<std::sync::Arc<pixtuoid_core::SceneState>>,
    pub(crate) floor_caps:
        std::sync::Arc<[std::sync::atomic::AtomicUsize; pixtuoid_core::state::MAX_FLOORS]>,
    health_rx: tokio::sync::watch::Receiver<Vec<pixtuoid_core::source::manager::SourceDeath>>,
    drift: crate::doctor::DriftSeen,
}

impl LivePipeline {
    /// The sources that died, for the Sources panel and the footer.
    pub(crate) fn health(&self) -> Vec<pixtuoid_core::source::manager::SourceDeath> {
        self.health_rx.borrow().clone()
    }

    /// The footer's warning now: [`crate::doctor::footer_warning`], as the
    /// TUI's.
    pub(crate) fn footer_warning(&self) -> Option<String> {
        crate::doctor::footer_warning(&self.health_rx.borrow(), &self.drift.prefixes())
    }
}

impl PipelineBoot {
    /// Boot the source pipeline seeded from the REAL window size, and wire the
    /// two background consumers that need the event-loop proxy.
    pub(crate) fn spawn(
        self,
        window_size: winit::dpi::PhysicalSize<u32>,
        density: pixtuoid_core::sprite::format::Density,
        zoom: geometry::Zoom,
    ) -> LivePipeline {
        let _guard = self.rt.enter(); // spawn_pipeline's internal spawns need it
        let boot_caps = geometry::boot_capacities_for_window(window_size, density, zoom);
        tracing::debug!(
            ?window_size,
            floor0_desks = boot_caps[0],
            "pixtuoid floating: seeding desk capacity from the real window"
        );
        let crate::runtime::pipeline::Pipeline {
            scene_rx,
            health_rx,
            floor_caps,
        } = crate::runtime::pipeline::spawn_pipeline(self.socket_path, self.connected, boot_caps);

        {
            let mut scene_rx = scene_rx.clone();
            let proxy = self.proxy.clone();
            self.rt.spawn(async move {
                while scene_rx.changed().await.is_ok() {
                    if proxy.send_event(FloatingEvent::SceneChanged).is_err() {
                        break;
                    }
                }
            });
        }
        // Deduped by count: the watch value is a grow-only Vec, so logging the whole
        // borrow on every change would re-warn all prior deaths.
        {
            let mut health_rx = health_rx.clone();
            let proxy = self.proxy.clone();
            self.rt.spawn(async move {
                let mut deaths_seen = 0usize;
                while health_rx.changed().await.is_ok() {
                    let deaths = health_rx.borrow_and_update().clone();
                    for death in crate::runtime::unseen_deaths(&deaths, &mut deaths_seen) {
                        tracing::warn!(
                            source = %death.source,
                            error = ?death.error,
                            "pixtuoid floating: source exited"
                        );
                    }
                    // The footer names the death.
                    if proxy.send_event(FloatingEvent::SceneChanged).is_err() {
                        break;
                    }
                }
            });
        }

        LivePipeline {
            scene_rx,
            floor_caps,
            health_rx,
            drift: self.drift,
        }
    }
}
