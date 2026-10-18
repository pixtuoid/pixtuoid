//! The `winit` window for `pixtuoid floating`.
//!
//! `FloatingApp` is the `ApplicationHandler`: on `Resumed` it creates ONE frameless,
//! always-on-top window + its GPU surface, renders the latest `watch`ed scene's
//! cutaway at the pack's densest art, composes it with the chrome, and presents it
//! upscaled a whole number of times ([`super::geometry::window_geometry`]).
//!
//! Platform glue — codecov-ignored; the testable seams are `floating::offscreen`
//! (render), `floating::overlays` and `floating::compose` (the chrome and the
//! frame), `floating::geometry` (the window/monitor rect math),
//! `floating::cadence` (the animation throttle), and `floating::lifecycle`
//! (the redraw's pacing and the exit order).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use pixtuoid_core::state::DaemonLiveness;
use pixtuoid_scene::pack::OfficeArt;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::{ResizeDirection, Window, WindowId, WindowLevel};

use super::offscreen::{OfficeRenderer, WindowFrame};
use crate::config::{self, FloatingConfig};
use pixtuoid_scene::cutaway::Face;
use pixtuoid_scene::floor::{FloorInputs, FloorMeta, PetInputs};
use pixtuoid_scene::look::Place;
use pixtuoid_scene::theme::Theme;

/// Wake reasons delivered to the winit loop from the background tokio pipeline.
#[derive(Debug, Clone)]
pub(crate) enum FloatingEvent {
    SceneChanged,
    /// wgpu reported a validation error or a lost device.
    GpuFailed(String),
    /// A quit signal ([`QuitArms`](crate::runtime::QuitArms)): quit as the close button does.
    Quit,
}

pub(crate) struct FloatingApp {
    cfg: FloatingConfig,
    theme: &'static Theme,
    pack: std::sync::Arc<OfficeArt>,
    config_path: PathBuf,
    /// The panels, the `[p]ause` and the theme picker, as the TUI holds them.
    ui: crate::panels::ui_state::UiState,
    /// The Sources panel's mutation seam, as the TUI's.
    connected: crate::runtime::ConnectedSources,
    /// The modifier keys held, which a key is read with.
    modifiers: winit::keyboard::ModifiersState,
    /// What the frame on screen shows beside the office.
    presented: super::compose::Presented,
    /// The frames' times and janks, reported as the TUI's are.
    jank: crate::jank::Jank,
    /// How the office moves.
    motion: pixtuoid_scene::anim::Motion,
    renderer: OfficeRenderer,
    audio_ctl: crate::audio::AudioController,
    /// The pipeline inputs, held until `resumed` can supply the REAL window size
    /// (the `[floating]` config size is LOGICAL and would mis-seed on HiDPI).
    /// `take`n exactly once; `None` afterwards.
    boot: Option<super::PipelineBoot>,
    /// The live pipeline — `None` until `resumed` boots it. `about_to_wait` DOES
    /// fire before then, so it reads `None` as an idle office. `redraw` cannot
    /// reach that state because `resumed` sets `live` BEFORE `window` and
    /// `redraw`'s window guard runs first. Don't reorder those assignments.
    live: Option<super::LivePipeline>,
    /// The buffer size the capacity atomics were last synced for — capacity only changes
    /// with the window size, so re-sync only on a size change (not every frame).
    last_caps_size: Option<(u16, u16)>,
    /// Latest cursor position (physical px) — for the corner resize hit-test on click.
    cursor: PhysicalPosition<f64>,
    /// Whether the pointer is over the window, where a hover shows its tooltip.
    cursor_in: bool,
    /// Whether the pointer last hovered something, so a move off it redraws
    /// once to drop its tooltip.
    hovered: bool,
    /// The cursor last set, so it is set only when it changes; `None` once
    /// the pointer leaves, whatever the OS showed meanwhile.
    cursor_icon: Option<winit::window::CursorIcon>,
    /// The last petting, played while it lasts.
    petting: Option<pixtuoid_scene::pet::PetState>,
    /// The animation-tick deadline — see [`super::cadence`] for why the redraw
    /// REQUEST (not just the wait) has to be gated on it.
    clock: super::cadence::FrameClock,
    /// How far the office is zoomed from its automatic scale.
    zoom: super::geometry::Zoom,
    /// The chrome over the office, each repainted only when it changed.
    overlays: super::overlays::OverlayLayers,
    window: Option<Arc<Window>>,
    gpu: Option<super::gpu::Gpu>,
    /// How a GPU failure reaches the loop: wgpu reports it from inside its
    /// own call, to a `Send + Sync + 'static` handler that cannot reach the
    /// app. `boot` is taken by `resumed`; the proxy outlives it.
    proxy: winit::event_loop::EventLoopProxy<FloatingEvent>,
    /// Why the window stopped, for [`super::run`] to return.
    failure: Stop,
}

/// Why the window stopped: its first failure. wgpu reports each error as it
/// happens and `exit` stops the loop only after the event, so a follow-on
/// error (an invalid command buffer after the encoder error that caused it)
/// arrives too.
#[derive(Debug, Default)]
struct Stop(Option<anyhow::Error>);

impl Stop {
    /// Hold `e` unless a failure is held already; whether it is the first.
    fn fail(&mut self, e: anyhow::Error) -> bool {
        if self.0.is_some() {
            return false;
        }
        self.0 = Some(e);
        true
    }

    fn into_inner(self) -> Option<anyhow::Error> {
        self.0
    }
}

/// What the run keeps beside the office: the config it persists to, its
/// audio, the log the Sources panel's drift history comes from, and whether
/// this is the first run, which opens the onboarding.
pub(crate) struct Settings {
    pub(crate) config_path: PathBuf,
    pub(crate) audio: config::AudioConfig,
    pub(crate) log: Option<crate::run_log::LogLocation>,
    pub(crate) first_run: bool,
}

/// How the office looks and moves.
pub(crate) struct Appearance {
    pub(crate) theme: &'static Theme,
    pub(crate) motion: pixtuoid_scene::anim::Motion,
}

impl FloatingApp {
    pub(crate) fn new(
        cfg: FloatingConfig,
        Appearance { theme, motion }: Appearance,
        pack: OfficeArt,
        pets: Vec<pixtuoid_scene::pet::Pet>,
        Settings {
            config_path,
            audio,
            log,
            first_run,
        }: Settings,
        boot: super::PipelineBoot,
    ) -> Self {
        let audio_ctl = crate::audio::AudioController::new(audio, config_path.clone());
        let pack = std::sync::Arc::new(pack);
        let mut renderer = OfficeRenderer::new(std::sync::Arc::clone(&pack));
        renderer.set_audio(audio_ctl.handle().clone());
        renderer.set_pets(pets);
        let proxy = boot.proxy.clone();
        let ui = crate::panels::ui_state::UiState::boot(crate::panels::ui_state::Boot {
            theme,
            first_run,
            config_path: &config_path,
            socket_path: boot.socket_path.clone(),
            log,
            drift: boot.drift.clone(),
            connected: &boot.connected.snapshot(),
            cutaway: crate::version::CutawayTip::default(),
        });
        let connected = boot.connected.clone();
        Self {
            cfg,
            theme,
            pack,
            config_path,
            ui,
            connected,
            modifiers: winit::keyboard::ModifiersState::empty(),
            // Until `resumed` names the platform: presenting every frame is
            // safe on any.
            presented: super::compose::Presented::new(false),
            jank: crate::jank::Jank::new(),
            motion,
            renderer,
            audio_ctl,
            boot: Some(boot),
            live: None,
            last_caps_size: None,
            cursor: PhysicalPosition::new(0.0, 0.0),
            cursor_in: false,
            hovered: false,
            petting: None,
            clock: super::cadence::FrameClock::new(Instant::now(), motion),
            cursor_icon: None,
            zoom: cfg.zoom,
            overlays: super::overlays::OverlayLayers::default(),
            window: None,
            gpu: None,
            proxy,
            failure: Stop::default(),
        }
    }

    /// Why the window stopped, when it failed: its creation, its GPU's, or a
    /// GPU failure reported while it ran.
    pub(crate) fn into_failure(self) -> Option<anyhow::Error> {
        self.failure.into_inner()
    }

    /// Persist the current window geometry and zoom into `[floating]` (best-effort — a save error
    /// must not block quitting). Size is stored LOGICAL (HiDPI-stable); position PHYSICAL.
    fn persist_geometry(&self) {
        let Some(window) = &self.window else {
            return;
        };
        let logical = window.inner_size().to_logical::<f64>(window.scale_factor());
        let pos = window.outer_position().ok();
        let save = config::FloatingSave {
            width: logical.width.round() as u32,
            height: logical.height.round() as u32,
            position: pos.map(|p| (p.x, p.y)),
            zoom: self.zoom,
        };
        if let Err(e) = config::save_floating(&self.config_path, &save) {
            tracing::warn!(error = ?e, "pixtuoid floating: could not persist window geometry");
        }
    }

    /// End the run. Geometry MUST persist HERE — the window is gone once
    /// `run_app` returns — and a run shorter than a summary's window still
    /// reports its spread.
    fn quit(&self, event_loop: &ActiveEventLoop) {
        self.persist_geometry();
        self.jank.finish(Instant::now());
        event_loop.exit();
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Show the cursor for what is under the pointer. A frame calls it too: a
    /// figure walks under a pointer at rest, and a carry ends on release.
    fn recursor(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        if !self.cursor_in || self.renderer.shown_fit().is_none() {
            return;
        }
        let size = window.inner_size();
        let icon = self.renderer.cursor_icon(
            (self.cursor.x, self.cursor.y),
            (size.width, size.height),
            &self.ui.modal(),
            self.petting.as_ref(),
            self.ui.now(),
        );
        if self.cursor_icon != Some(icon) {
            self.cursor_icon = Some(icon);
            window.set_cursor(icon);
        }
    }

    /// Redraw when the pointer hovers something, or stops, so its tooltip
    /// follows within a move rather than at the next paint tick, which an
    /// idle office spaces a second apart.
    fn rehover(&mut self) {
        self.recursor();
        let hovered = self.cursor_in
            && self
                .renderer
                .hit_at((self.cursor.x, self.cursor.y))
                .is_some();
        // A shown tooltip follows the pointer, so it redraws on every move.
        if hovered || self.hovered {
            self.hovered = hovered;
            self.request_redraw();
        }
    }

    /// A left press: the open panels' first, as the TUI's ladder
    /// ([`crate::panels::modal_mouse`]); then resize from the corner, hand the
    /// pointer what the frame on screen shows under it (a click or a drag
    /// follows), or drag the frameless window. Errors are non-fatal — some
    /// platforms refuse a drag outside a real press.
    fn press(&mut self) {
        use super::offscreen::Press;
        use crate::panels::ModalMouse;
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        let cursor = (self.cursor.x, self.cursor.y);
        match super::overlays::modal_press(
            &mut self.ui,
            cursor,
            (size.width, size.height),
            self.renderer.shown_fit(),
        ) {
            ModalMouse::Office => {}
            // A panel the press did nothing in leaves the window to drag.
            ModalMouse::Inert => {
                let _ = window.drag_window();
                return;
            }
            ModalMouse::Took => {
                window.request_redraw();
                return;
            }
        }
        let now = self.ui.now();
        let press = self.renderer.press_at(
            cursor,
            (size.width, size.height),
            super::offscreen::Pressing {
                scale_factor: window.scale_factor(),
                petting: self.petting.as_ref(),
                now,
            },
        );
        match press {
            Press::Resize => {
                let _ = window.drag_resize_window(ResizeDirection::SouthEast);
            }
            Press::Drag => {
                let _ = window.drag_window();
            }
            Press::Pointer => {}
        }
    }

    /// The left button released: carry out a click as the TUI's does, or set
    /// a carried figure down, even one a panel opened over mid-carry.
    fn release(&mut self) {
        use pixtuoid_scene::hit::HitAction;
        let action = self
            .renderer
            .release_under((self.cursor.x, self.cursor.y), &self.ui.modal());
        let now = self.ui.now();
        match action {
            Some(HitAction::Focus(id)) => {
                let slot = self
                    .live
                    .as_ref()
                    .and_then(|l| l.scene_rx.borrow().agents.get(&id).cloned());
                if let Some(slot) = slot {
                    crate::focus::focus_slot(&slot);
                }
            }
            Some(HitAction::Pet(kind)) => {
                self.petting = Some(pixtuoid_scene::pet::PetState {
                    petted_at: now,
                    kind,
                    floor_idx: self.renderer.nav().current(),
                });
            }
            Some(HitAction::Open(url)) => {
                crate::open_url(url);
            }
            None => {}
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// A pressed key, through the TUI's dispatch and its panels; whether it
    /// asked to quit.
    fn key(&mut self, event: &winit::event::KeyEvent) -> bool {
        let Some(key) = super::input::key(&event.logical_key, self.modifiers, event.repeat) else {
            return false;
        };
        let Some(snapshot) = self.live.as_ref().map(|l| Arc::clone(&l.scene_rx.borrow())) else {
            return false;
        };
        let now = self.ui.now();
        super::input::press_key(
            key,
            &mut crate::panels::KeyCtx {
                ui: &mut self.ui,
                host: &mut super::input::WindowHost {
                    renderer: &mut self.renderer,
                    theme: &mut self.theme,
                    presented: &mut self.presented,
                },
                audio_ctl: &mut self.audio_ctl,
                config_path: &self.config_path,
                connected: &self.connected,
                snapshot: &snapshot,
                now,
                respawn: crate::audio::respawn,
                installed: crate::sources::detect,
            },
        )
    }

    fn paint(&mut self) {
        let started = Instant::now();
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let size = window.inner_size();
        let (win_w, win_h) = (size.width, size.height);
        if win_w == 0 || win_h == 0 {
            return; // a 0-area window: nothing to draw
        }
        // Cloned out: a held `watch::Ref` read-locks the channel, stalling the sender.
        let Some((scene, floor_caps)) = self
            .live
            .as_ref()
            .map(|l| (l.scene_rx.borrow().clone(), Arc::clone(&l.floor_caps)))
        else {
            return;
        };
        let audio_now = Instant::now();
        self.audio_ctl.tick(audio_now);
        let audio_audible = self.audio_ctl.handle().is_audible();
        let volume_flash = self.audio_ctl.volume_flash(audio_now);
        let at = super::geometry::window_geometry(size, self.pack.max_density_variant(), self.zoom);
        super::geometry::sync_floor_caps(
            &mut self.last_caps_size,
            &floor_caps,
            at.logical().w,
            at.logical().h,
        );
        // The office's weather and motion; the office picks each floor's own.
        let floor_meta = FloorMeta::ground().with_motion(self.motion);
        // ONE clock read, so the overlays below annotate the frame actually rendered.
        let now = self.ui.now();
        let world = FloorInputs {
            scene: &scene,
            pack: &self.pack,
            now,
            floor: floor_meta,
            pets: PetInputs {
                pet: None,
                petting: self.petting.as_ref(),
            },
        };
        let health = self
            .live
            .as_ref()
            .map(super::LivePipeline::health)
            .unwrap_or_default();
        let frames = self.ui.build_frames(now, &scene, &health);
        let live = self.renderer.render_live(
            at,
            WindowFrame {
                world,
                theme: self.theme,
                place: Place {
                    gateway: pixtuoid_scene::tally::office_gateway(&scene),
                    floor: None,
                },
                dim: frames.onboarding.dim,
            },
            (win_w, win_h),
        );
        let Some(office) = self
            .renderer
            .buf()
            .filter(|o| live && o.width() > 0 && o.height() > 0)
        else {
            // Nothing rendered, or held: the window keeps the last frame, which
            // the next one's dirt is no longer measured against.
            self.presented.stale();
            return;
        };
        let cursor = (self.cursor.x, self.cursor.y);
        let next = super::overlays::Overlays {
            window: (win_w, win_h),
            footer: self.renderer.footer(
                &scene,
                super::overlays::footer_budget(win_w as usize, Face::chrome(at)),
                audio_audible,
                volume_flash,
                self.live
                    .as_ref()
                    .and_then(super::LivePipeline::footer_warning)
                    .as_deref(),
            ),
            tooltip: self
                .renderer
                .tooltip_shows(self.cursor_in, &self.ui.modal())
                .then(|| self.renderer.drawn_hit_at(cursor, at))
                .flatten()
                .and_then(|hit| {
                    // The office picks each floor's pet as it renders; the
                    // tooltip names the one the floor showing drew.
                    let shown = FloorInputs {
                        pets: PetInputs {
                            pet: self.renderer.showing_pet(),
                            ..world.pets
                        },
                        ..world
                    };
                    pixtuoid_scene::tooltip::for_hit(hit, &shown)
                })
                .map(|tip| (tip, (cursor.0 as i32, cursor.1 as i32))),
            panels: super::overlays::panels_grid(
                &frames,
                super::overlays::panel_cells((win_w, win_h), Face::chrome(at)),
                now,
                self.theme,
            ),
        };
        self.overlays.update(next, at, (self.theme, &*self.pack));
        let comp = super::compose::composition(
            office,
            super::compose::Change::from(self.renderer.dirty()),
            at,
            (win_w, win_h),
            &self.overlays,
        );
        if !self.presented.needs(&comp) {
            drop(comp);
            // The frame on screen is this one: its flash phase shows.
            self.renderer.presented();
            return;
        }
        let in_sync = self.presented.in_sync();
        // Until this one presents, the screen matches no frame rendered.
        self.presented.stale();
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let presenting = Instant::now();
        let shown = gpu.present(&comp, in_sync);
        if shown {
            self.presented.shown(&comp);
        }
        drop(comp);
        if shown {
            self.renderer.presented();
        }
        let now = Instant::now();
        self.jank.painted_by(crate::jank::Painter {
            look: "floating",
            scale: at.scale().get(),
            ..crate::jank::Painter::default()
        });
        self.renderer
            .record(&mut self.jank, now - started, now - presenting, now);
    }
}

fn position_on_a_monitor(event_loop: &ActiveEventLoop, x: i32, y: i32, w: u32, h: u32) -> bool {
    super::geometry::window_visible_on_monitors(
        (x, y, w, h),
        event_loop.available_monitors().map(|m| {
            let (pos, size) = (m.position(), m.size());
            (pos.x, pos.y, size.width, size.height)
        }),
    )
}

impl super::lifecycle::Redraw for FloatingApp {
    fn jank(&mut self) -> &mut crate::jank::Jank {
        &mut self.jank
    }

    fn paint(&mut self) {
        FloatingApp::paint(self);
    }

    /// The cursor reads the frame on screen, as a press does; the tooltip
    /// reads the frame just rendered.
    fn recursor(&mut self) {
        FloatingApp::recursor(self);
    }
}

impl super::lifecycle::Teardown for FloatingApp {
    fn hide_window(&mut self) {
        if let Some(window) = &self.window {
            window.set_visible(false);
        }
    }

    // The surface holds its own clone of the window's `Arc`, so the window is
    // destroyed once both are dropped.
    fn release_surface(&mut self) {
        self.gpu = None;
        self.window = None;
    }

    fn stop_audio(&mut self) {
        self.audio_ctl.stop();
    }
}

impl ApplicationHandler<FloatingEvent> for FloatingApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // already created — a re-resume must not spawn a second window
        }
        let min = super::geometry::min_window(self.pack.max_density_variant());
        let mut attrs = Window::default_attributes()
            .with_title("pixtuoid")
            .with_decorations(false)
            .with_resizable(true)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_inner_size(LogicalSize::new(self.cfg.width, self.cfg.height))
            .with_min_inner_size(min);
        // A spot on a since-disconnected monitor would open the frameless window unreachably.
        if let Some((x, y)) = self.cfg.position
            && position_on_a_monitor(event_loop, x, y, self.cfg.width, self.cfg.height)
        {
            attrs = attrs.with_position(PhysicalPosition::new(x, y));
        }
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs = attrs.with_has_shadow(true).with_titlebar_hidden(true);
        }
        #[cfg(target_os = "windows")]
        {
            // No taskbar button — it's an ambient overlay, not a primary window.
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true);
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.failure
                    .fail(anyhow::Error::new(e).context("creating the floating window"));
                event_loop.exit();
                return;
            }
        };
        let proxy = self.proxy.clone();
        let gpu = match super::gpu::Gpu::for_window(
            Arc::clone(&window),
            event_loop.owned_display_handle(),
            move |message| {
                let _ = proxy.send_event(FloatingEvent::GpuFailed(message));
            },
        ) {
            Ok(gpu) => gpu,
            Err(e) => {
                self.failure.fail(e.context(
                    "starting the floating window's GPU (`pixtuoid run` shows the office in the terminal)",
                ));
                event_loop.exit();
                return;
            }
        };
        // Seeded from the REAL window — the first PHYSICAL size there is, never the
        // logical config size (#803: buffer size is not monotone in scale factor).
        // Past the window/surface failure arms, so a failed boot binds no socket.
        // Pinned by `the_boot_seed_tracks_the_physical_window_not_the_logical_config`.
        if let Some(boot) = self.boot.take() {
            self.live = Some(boot.spawn(
                window.inner_size(),
                self.pack.max_density_variant(),
                self.zoom,
            ));
        }
        self.presented = super::compose::Presented::of_window(
            winit::raw_window_handle::HasWindowHandle::window_handle(&*window)
                .ok()
                .map(|h| h.as_raw()),
        );
        window.request_redraw();
        self.window = Some(window);
        self.gpu = Some(gpu);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: FloatingEvent) {
        match event {
            FloatingEvent::SceneChanged => self.request_redraw(),
            FloatingEvent::GpuFailed(message) => {
                tracing::error!(error = ?message, "pixtuoid floating: the GPU failed");
                let failure = anyhow::anyhow!(message).context("the floating window's GPU failed");
                if self.failure.fail(failure) {
                    self.quit(event_loop);
                }
            }
            FloatingEvent::Quit => self.quit(event_loop),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.quit(event_loop),
            // `is_synthetic: false`: winit fabricates a Pressed for every key
            // physically held when the window GAINS FOCUS (X11 + Windows). A
            // muted user holding `+`/`m` who clicks in would otherwise be
            // spuriously unmuted AND have it persisted.
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed => {
                if let Some(window) = &self.window
                    && let Some(zoom) = super::input::zoomed(
                        &event.logical_key,
                        self.modifiers,
                        self.zoom,
                        window.inner_size(),
                        self.pack.max_density_variant(),
                    )
                {
                    self.zoom = zoom;
                    window.request_redraw();
                    return;
                }
                if self.key(&event) {
                    self.quit(event_loop);
                    return;
                }
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::RedrawRequested => super::lifecycle::redraw(self),
            WindowEvent::Resized(_) => self.request_redraw(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                self.cursor_in = true;
                let carried = self.renderer.pointer_moved((position.x, position.y));
                if carried && let Some(window) = &self.window {
                    window.request_redraw();
                }
                self.rehover();
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor_in = false;
                self.cursor_icon = None;
                self.rehover();
            }
            // Its release goes elsewhere now: a figure in hand is set down.
            WindowEvent::Focused(false) => {
                if self.renderer.cancel_pointer()
                    && let Some(window) = &self.window
                {
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.press(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.release(),
            _ => {}
        }
    }

    /// The loop is ending, by [`FloatingApp::quit`] or by the OS: the window and
    /// the audio go.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        super::lifecycle::tear_down(self);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // An EMPTY office still steps on its beat (clock hands, weather,
        // lightning, day/night), so it paints each beat, never 0fps. A LIVE
        // gateway daemon lives in `daemons`, not `agents`, and is a WANDERING
        // mascot, so it holds the fast cadence, as does whatever the floor
        // reports moving between beats (a walk, or a light mid-fade) and the
        // onboarding's backdrop dim mid-ramp.
        let office_idle = self.live.as_ref().is_none_or(|live| {
            let scene = live.scene_rx.borrow();
            scene.agents.is_empty()
                && scene
                    .daemons()
                    .all(|(_, _, d)| d.liveness == DaemonLiveness::Down)
        }) && !self.renderer.moves_off_beat()
            && self.renderer.nav().transition().is_none()
            && !self.ui.dim_moving();
        // The redraw REQUEST rides the same deadline as the wait: requesting one
        // unconditionally here leaves winit a pending redraw, so `WaitUntil` never
        // sleeps and both cadences collapse to max-rate (see `super::cadence`).
        let (paint, deadline) = self
            .clock
            .poll(Instant::now(), SystemTime::now(), office_idle);
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        if let Some(due) = paint {
            self.jank.due_at(due);
            self.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A follow-on failure, as an invalid command buffer after the encoder
    /// error that caused it, never replaces the first, and only the first
    /// stops the window.
    #[test]
    fn a_follow_on_failure_keeps_the_first() {
        let mut stop = Stop::default();
        assert!(stop.fail(anyhow::anyhow!("root cause")), "the first stops");
        assert!(
            !stop.fail(anyhow::anyhow!("follow-on")),
            "a second does not"
        );
        let kept = stop.into_inner().expect("a failure");
        assert_eq!(kept.to_string(), "root cause");
    }
}
