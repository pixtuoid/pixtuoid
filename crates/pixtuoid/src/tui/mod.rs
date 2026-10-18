pub(crate) mod cutaway;
pub(crate) mod geometry;
pub(crate) mod hit_test;
pub(crate) mod renderer;
pub(crate) mod tui_renderer;

use std::io::stdout;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::time::MissedTickBehavior;

use tui_renderer::TuiRenderer;

use crate::panels::{
    FloorNav, KeyAction, KeyCtx, ModalState, apply_key_action, dispatch_key, ui_state,
};
use crate::runtime::{QuitArms, QuitSignal, SceneRx};
use pixtuoid_scene::hit::HitAction;
use pixtuoid_scene::{pet, theme};

/// Windows delivers Press AND Release per keystroke, so without this guard every key
/// double-fires there — `p` would pause then instantly unpause. Inert on Unix, which
/// is why a local green run is no evidence. Pinned by `only_press_events_dispatch`.
fn should_dispatch_key(kind: KeyEventKind) -> bool {
    kind == KeyEventKind::Press
}

/// What the terminal's help lists beside the shared shortcuts, when
/// [`TuiRenderer::offers_walkable_debug`] says it does.
pub(crate) const WALKABLE_SHORTCUTS: &[crate::panels::widgets::Shortcut] =
    &[("w", "walkable / approach / route debug")];

/// Flip the half-block painter's walkable debug layer on `w`; whether it did.
/// It does when no panel owns input and the last frame was the half-blocks
/// ([`TuiRenderer::offers_walkable_debug`]), which alone draw that layer.
pub(crate) fn toggle_walkable_debug_on_w<
    B: ratatui::backend::Backend<Error: Send + Sync + 'static>,
>(
    renderer: &mut TuiRenderer<B>,
    code: KeyCode,
    modal: &ModalState,
) -> bool {
    let toggles =
        renderer.offers_walkable_debug() && code == KeyCode::Char('w') && !modal.any_open();
    if toggles {
        let on = renderer.debug_walkable();
        renderer.set_debug_walkable(!on);
    }
    toggles
}

/// What a key does: the terminal's own `w` first, which neither the window nor
/// the cutaway share, then the shared dispatch.
pub(crate) fn route_key<B: ratatui::backend::Backend<Error: Send + Sync + 'static>>(
    renderer: &mut TuiRenderer<B>,
    (code, mods): (KeyCode, crossterm::event::KeyModifiers),
    modal: ModalState,
    floor: FloorNav,
) -> KeyAction {
    if toggle_walkable_debug_on_w(renderer, code, &modal) {
        KeyAction::None
    } else {
        dispatch_key(code, mods, modal, floor)
    }
}

/// The per-floor desk-capacity sweep, memoized on its own inputs.
///
/// `floor_capacity` runs a FULL `SceneLayout::compute_with_seed` — walkable-mask stamp plus
/// coarse BFS, quadratic in buffer area — once per floor, and keeps only
/// `home_desks.len()`. It is a pure function of `(buf_w, buf_h, desk_cap)` and the
/// publish is a monotone `fetch_max`, so a repeat with identical inputs could only
/// rewrite the same values.
struct FloorCapacitySweep {
    last: Option<(u16, u16, Option<usize>)>,
}

impl FloorCapacitySweep {
    fn new() -> Self {
        Self { last: None }
    }

    /// Returns whether it actually recomputed (`false` = served from the memo).
    fn publish(
        &mut self,
        buf_w: u16,
        buf_h: u16,
        desk_cap: Option<usize>,
        caps: &[std::sync::atomic::AtomicUsize; pixtuoid_core::state::MAX_FLOORS],
    ) -> bool {
        if self.last == Some((buf_w, buf_h, desk_cap)) {
            return false;
        }
        self.last = Some((buf_w, buf_h, desk_cap));
        for (floor_idx, cap_slot) in caps.iter().enumerate() {
            let seed = pixtuoid_scene::floor::floor_seed(floor_idx);
            let mut capacity = pixtuoid_scene::floor::floor_capacity(buf_w, buf_h, seed);
            if let Some(cap) = desk_cap {
                capacity = capacity.min(cap);
            }
            if capacity > 0 {
                // `fetch_max` keeps capacity monotone: a shrink would shift the
                // cumulative offsets and remap floor-1+ agents onto the wrong
                // desks (they go invisible).
                cap_slot.fetch_max(capacity, std::sync::atomic::Ordering::Relaxed);
            }
        }
        true
    }
}

impl<B: ratatui::backend::Backend<Error: Send + Sync + 'static>> crate::panels::Host
    for TuiRenderer<B>
{
    fn set_theme(&mut self, theme: &'static theme::Theme) {
        TuiRenderer::set_theme(self, theme);
    }

    fn navigate_floor(&mut self, floor: usize, now: SystemTime) {
        TuiRenderer::navigate_floor(self, floor, now);
    }

    fn redraw(&mut self) -> anyhow::Result<()> {
        TuiRenderer::redraw(self)
    }
}

pub(crate) type Term = Terminal<CrosstermBackend<FrameOut>>;

/// The terminal's output, held a frame at a time, from [`FrameOut::begin`] to
/// [`FrameOut::present`]: the frame's text and images reach the terminal in
/// one write, inside a synchronized update (mode 2026) where it has one, so
/// it never shows a frame half drawn. Outside a frame writes pass straight
/// through. Clones share one buffer, so the text and the images interleave
/// in the order they were written.
#[derive(Clone)]
pub(crate) struct FrameOut(Arc<std::sync::Mutex<Held>>);

impl std::fmt::Debug for FrameOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.held();
        f.debug_struct("FrameOut")
            .field("holding", &held.holding)
            .field("sync", &held.sync)
            .field("torn", &held.torn)
            .finish_non_exhaustive()
    }
}

struct Held {
    out: Box<dyn std::io::Write + Send>,
    /// This frame's bytes; its capacity kept across frames.
    frame: Vec<u8>,
    holding: bool,
    /// Wrap each frame in `CSI ? 2026 h` / `l`.
    sync: bool,
    /// The last present failed, perhaps mid-escape or inside its update: the
    /// next opens with ST, and ends that update.
    torn: bool,
}

/// Begin and end synchronized update
/// (<https://github.com/contour-terminal/vt-extensions/blob/master/synchronized-output.md>).
const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
const END_SYNC: &[u8] = b"\x1b[?2026l";

impl FrameOut {
    /// `out`, a frame at a time; inside a synchronized update when `sync`.
    pub(crate) fn new(out: impl std::io::Write + Send + 'static, sync: bool) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Held {
            out: Box::new(out),
            frame: Vec::new(),
            holding: false,
            sync,
            torn: false,
        })))
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Each frame goes out inside a synchronized update.
    pub(crate) fn synchronized(&self) -> bool {
        self.held().sync
    }

    /// Hold what is written until [`Self::present`].
    pub(crate) fn begin(&self) {
        let mut held = self.held();
        held.holding = true;
        held.frame.clear();
        if held.torn {
            held.frame.extend_from_slice(crate::graphics::ST);
            if held.sync {
                held.frame.extend_from_slice(END_SYNC);
            }
        }
        if held.sync {
            held.frame.extend_from_slice(BEGIN_SYNC);
        }
    }

    /// Drop every later write: the terminal is gone.
    fn abandon(&self) {
        self.held().out = Box::new(std::io::sink());
    }

    /// Write what was held since [`Self::begin`] in one write, and flush.
    ///
    /// # Errors
    ///
    /// If the write or the flush fails; the next frame then opens with ST.
    pub(crate) fn present(&self) -> std::io::Result<()> {
        let mut held = self.held();
        held.holding = false;
        if held.sync {
            held.frame.extend_from_slice(END_SYNC);
        }
        let Held { out, frame, .. } = &mut *held;
        let wrote = out.write_all(frame).and_then(|()| out.flush());
        held.torn = wrote.is_err();
        held.frame.clear();
        wrote
    }
}

impl std::io::Write for FrameOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut held = self.held();
        if held.holding {
            held.frame.extend_from_slice(buf);
            Ok(buf.len())
        } else {
            held.out.write(buf)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut held = self.held();
        if held.holding {
            Ok(())
        } else {
            held.out.flush()
        }
    }
}

/// Enters raw mode + the alternate screen ATOMICALLY: a failure after raw mode is on rolls
/// the terminal all the way back, or the error path strands the user's shell echo-less
/// and/or on the alt screen. `Terminal::new`'s `.size()` query can fail too.
/// `_armed` proves the quit arms came first, as a signal past this point killing the
/// process would strand the shell the same way.
///
/// # Errors
///
/// If the Windows console lacks VT support, or enabling raw mode, the alternate screen or mouse capture fails, or the terminal size query fails.
pub(crate) fn setup_terminal(out: FrameOut, _armed: &QuitArms) -> Result<Term> {
    // On the WinAPI fallback (no VT), crossterm maps Color::Rgb to console attribute 0
    // and the office renders black-on-black invisible. Gate, don't degrade.
    #[cfg(windows)]
    if !crossterm::ansi_support::supports_ansi() {
        anyhow::bail!(
            "pixtuoid needs a VT-capable terminal — use Windows Terminal \
             (or Windows 10 1703+ with VT processing enabled)"
        );
    }
    enable_raw_mode()?;
    let mut out = out;
    // Mouse capture drives the hover tooltip: terminals emit MouseEventKind::Moved on
    // cursor motion only while it is on.
    if let Err(e) = execute!(out, EnterAlternateScreen, EnableMouseCapture) {
        let _ = unwind_terminal_modes(&mut out, disable_raw_mode);
        return Err(e.into());
    }
    Terminal::new(CrosstermBackend::new(out)).map_err(|e| {
        let mut out = stdout();
        let _ = unwind_terminal_modes(&mut out, disable_raw_mode);
        e.into()
    })
}

/// THE terminal-mode unwind: the ONE definition of the order every exit path takes,
/// the panic hook in `app/crash.rs` included. It also unlinks every shared-memory
/// object this process published.
///
/// Every step runs even when an earlier one fails and the FIRST error is returned — a `?`
/// after the escape write would skip `disable_raw` exactly when it is needed most. And
/// DisableMouseCapture must run while raw mode is still ON: on Windows it restores the
/// input mode snapshotted at Enable time (raw-era), so after `disable_raw_mode` it re-raws
/// the console. Either slip strands the user's shell echo-less.
///
/// # Errors
///
/// If writing the graphics unwind, the mouse-capture and alternate-screen escapes, or `disable_raw` fails; every step still runs.
pub(crate) fn unwind_terminal_modes<W: std::io::Write>(
    out: &mut W,
    disable_raw: impl FnOnce() -> std::io::Result<()>,
) -> Result<()> {
    #[cfg(unix)]
    crate::graphics::shm::unlink_all();
    unwind_after(&crate::graphics::unwind_prelude(), out, disable_raw)
}

/// Writes `prelude`, then [`unwind_terminal_modes`]' sequence, so the images
/// go while the alt screen that holds them is still up.
fn unwind_after<W: std::io::Write>(
    prelude: &[u8],
    out: &mut W,
    disable_raw: impl FnOnce() -> std::io::Result<()>,
) -> Result<()> {
    // A frame cut short may have left a synchronized update open, which
    // would hold the screen after exit.
    let images = out
        .write_all(END_SYNC)
        .and_then(|()| out.write_all(prelude));
    let seq = execute!(out, DisableMouseCapture, LeaveAlternateScreen);
    let raw = disable_raw();
    images?;
    seq?;
    raw?;
    Ok(())
}

/// Restore the terminal modes and cursor that [`setup_terminal`] changed.
///
/// # Errors
///
/// If restoring the terminal modes or showing the cursor fails.
pub(crate) fn teardown_terminal(term: &mut Term, out: Option<&FrameOut>) -> Result<()> {
    let modes = unwind_terminal_modes(term.backend_mut(), disable_raw_mode);
    // Unconditional: a failed mode restore must not ALSO leave the cursor hidden.
    let cursor = term.show_cursor();
    if let (Err(_), Some(out)) = (&cursor, out) {
        // `Terminal`'s Drop shows the cursor again while its flag says hidden and
        // `eprintln!`s the failure, which panics on a stderr that left with the
        // terminal. The flag clears only on a successful show, so aim the writer
        // at a sink and show once more.
        out.abandon();
        let _ = term.show_cursor();
    }
    modes?;
    cursor?;
    Ok(())
}

pub(crate) struct TuiSession {
    pub scene_rx: SceneRx,
    pub pack: Arc<pixtuoid_scene::pack::OfficeArt>,
    /// What `boot_tui` planned to paint.
    pub plan: crate::graphics::Plan,
    /// How the office moves, which `boot_tui` resolved beside the plan.
    pub motion: pixtuoid_scene::anim::Motion,
    pub floor_caps: Arc<[std::sync::atomic::AtomicUsize; pixtuoid_core::state::MAX_FLOORS]>,
    pub theme: &'static theme::Theme,
    pub config_path: std::path::PathBuf,
    pub desk_cap: Option<usize>,
    pub pets: Vec<pet::Pet>,
    pub source_health:
        tokio::sync::watch::Receiver<Vec<pixtuoid_core::source::manager::SourceDeath>>,
    /// The hook socket (Unix) / named pipe (Windows) the daemon bound.
    pub socket_path: std::path::PathBuf,
    /// The Sources panel's mutation seam: a toggle calls `connected.set(src, on)`, which
    /// the reducer task's reconciler observes (gate + graceful evict).
    pub connected: crate::runtime::ConnectedSources,
    /// Where the warn-floor log lives, for the Sources panel's drift history.
    pub log: Option<crate::run_log::LogLocation>,
    /// The sources this run's decode drift has named, for the footer nudge.
    pub drift: crate::doctor::DriftSeen,
    /// The persisted mute/volume, handed whole to `AudioController::new`.
    pub audio_cfg: crate::config::AudioConfig,
    /// Focus-jump pid point-query roots: (CC projects root, Codex sessions root).
    pub focus_roots: (Option<std::path::PathBuf>, Option<std::path::PathBuf>),
    pub first_run: bool,
}

/// The mouse hit-test LADDER: the panels' half first ([`crate::panels::modal_mouse`]), then
/// the scene.
fn handle_mouse_event<B: ratatui::backend::Backend<Error: Send + Sync + 'static>>(
    m: crossterm::event::MouseEvent,
    ui: &mut ui_state::UiState,
    renderer: &mut TuiRenderer<B>,
    scene_rx: &SceneRx,
    focus: impl FnOnce(&pixtuoid_core::AgentSlot),
    now: SystemTime,
) {
    let left_down =
        matches!(m.kind, MouseEventKind::Down(MouseButton::Left)).then_some((m.column, m.row));
    if crate::panels::modal_mouse(ui, renderer.last_popup_scale(), left_down, || {
        crossterm::terminal::size().ok()
    }) != crate::panels::ModalMouse::Office
    {
        return;
    }
    match m.kind {
        MouseEventKind::Moved => {
            renderer.set_mouse_pos(Some((m.column, m.row)));
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            renderer.set_mouse_pos(Some((m.column, m.row)));
            renderer.drag(m.column, m.row);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            renderer.set_mouse_pos(Some((m.column, m.row)));
            renderer.press(m.column, m.row, now);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            renderer.set_mouse_pos(Some((m.column, m.row)));
            match renderer.release(m.column, m.row) {
                Some(HitAction::Focus(id)) => {
                    let slot = scene_rx.borrow().agents.get(&id).cloned();
                    if let Some(slot) = slot {
                        focus(&slot);
                    }
                }
                Some(HitAction::Pet(kind)) => {
                    renderer.set_active_pet(Some(renderer::PetState {
                        petted_at: now,
                        kind,
                        floor_idx: renderer.current_floor(),
                    }));
                }
                Some(HitAction::Open(url)) => {
                    crate::open_url(url);
                }
                None => {}
            }
        }
        _ => {}
    }
}

/// Hand `renderer` the painter `plan` names.
fn paint_plan<B: ratatui::backend::Backend<Error: Send + Sync + 'static>>(
    renderer: &mut TuiRenderer<B>,
    plan: crate::graphics::Plan,
    out: &FrameOut,
) {
    let (terminal, tmux_env) = crate::graphics::terminal_and_tmux();
    match plan {
        crate::graphics::Plan::Cutaway {
            fit, route, cell, ..
        } => {
            renderer.painted_by(crate::jank::Painter {
                look: route.protocol().name(),
                scale: fit.scale().get(),
                tmux: route.tmux(),
                terminal,
                sync: out.synchronized(),
            });
            renderer.set_cutaway(cutaway::TileCutaway::new(
                fit,
                cell,
                route,
                Box::new(out.clone()),
            ));
        }
        _ => {
            renderer.painted_by(crate::jank::Painter {
                look: "classic",
                scale: 1,
                tmux: tmux_env,
                terminal,
                sync: out.synchronized(),
            });
            tracing::info!(plan = ?plan, "painting classic");
        }
    }
}

/// The least time between two of the event loop's frames.
pub(crate) fn frame_tick() -> Duration {
    Duration::from_secs(1) / pixtuoid_scene::anim::PAINT_FPS
}

/// The event loop, running as the `block_on` ROOT future rather than on a tokio worker — so
/// `tokio::task::block_in_place` here is inert, not a yield point, and does not panic either
/// (that is `current_thread`-only). Pinned by `block_in_place_is_inert_on_the_block_on_thread`.
pub(crate) async fn run_tui(session: TuiSession) -> Result<Option<QuitSignal>> {
    let TuiSession {
        mut scene_rx,
        pack,
        plan,
        motion,
        floor_caps,
        theme,
        config_path,
        desk_cap,
        pets,
        mut source_health,
        socket_path,
        connected,
        log,
        drift,
        focus_roots,
        first_run,
        audio_cfg,
    } = session;
    let arms = QuitArms::arm();
    // Asked before raw mode is on: the query takes the terminal's own for
    // its reply.
    let out = FrameOut::new(
        stdout(),
        crate::term::query_sync_output(crate::term::SYNC_OUTPUT_PROBE_TIMEOUT),
    );
    let term = setup_terminal(out.clone(), &arms)?;
    let quit = arms.signalled();
    tokio::pin!(quit);
    let mut renderer = TuiRenderer::new(term, theme, pets, Arc::clone(&pack));
    renderer.set_motion(motion);
    paint_plan(&mut renderer, plan, &out);
    renderer.present_through(out);
    // A LOCAL so EVERY exit (q / Ctrl-C / terminate / error) drops it and joins
    // the device thread it owns.
    let mut audio_ctl = crate::audio::AudioController::new(audio_cfg, config_path.clone());
    renderer.set_audio(audio_ctl.handle().clone());
    let mut ui = ui_state::UiState::boot(ui_state::Boot {
        theme,
        first_run,
        config_path: &config_path,
        socket_path,
        log,
        drift,
        connected: &connected.snapshot(),
        cutaway: crate::version::CutawayTip {
            off: plan.graphics_off(),
            drawable: crate::graphics::names_a_cutaway_terminal(),
        },
    });
    renderer.warm(&scene_rx.borrow().clone(), &pack, ui.now());
    let mut cap_sweep = FloorCapacitySweep::new();

    let tick = frame_tick();
    renderer.scheduled_every(tick);
    let mut fired = None;
    let result: Result<()> = (async {
        let mut frames = frame_clock(tick);
        let mut events = EventStream::new();
        let mut snapshot = scene_rx.borrow().clone();
        let mut now = ui.now();
        loop {
            tokio::select! {
                due = frames.tick() => {
                    renderer.due_at(due.into_std());
                    now = ui.now();
                    snapshot = scene_rx.borrow_and_update().clone();
                    let health = source_health.borrow_and_update().clone();
                    renderer.set_frames(ui.build_frames(now, &snapshot, &health), now);
                    let audio_now = std::time::Instant::now();
                    audio_ctl.tick(audio_now);
                    renderer.set_volume_flash(audio_ctl.volume_flash(audio_now));
                    renderer.render(&snapshot, &pack, now)?;

                    if let Some(layout) = renderer.cached_layout() {
                        cap_sweep.publish(layout.buf_w, layout.buf_h, desk_cap, &floor_caps);
                    }
                }
                event = events.next() => match event.context("terminal input closed")?? {
                    Event::Key(k) if should_dispatch_key(k.kind) => {
                        let floor = FloorNav {
                            n_floors: pixtuoid_scene::floor::num_floors(&snapshot),
                            current_floor: renderer.current_floor(),
                            in_transition: renderer.transition().is_some(),
                        };
                        let action =
                            route_key(&mut renderer, (k.code, k.modifiers), ui.modal(), floor);
                        let quit = apply_key_action(
                            action,
                            &mut KeyCtx {
                                ui: &mut ui,
                                host: &mut renderer,
                                audio_ctl: &mut audio_ctl,
                                config_path: &config_path,
                                connected: &connected,
                                snapshot: &snapshot,
                                focus_roots: &focus_roots,
                                now,
                                respawn: crate::audio::respawn,
                                installed: crate::sources::detect,
                            },
                        );
                        if quit {
                            if ui.theme_picker.is_some() {
                                renderer.set_theme(theme::ALL_THEMES[ui.saved_theme_idx]);
                            }
                            break;
                        }
                    }
                    Event::Mouse(m) => handle_mouse_event(
                        m,
                        &mut ui,
                        &mut renderer,
                        &scene_rx,
                        |slot| crate::focus::focus_slot(slot, &focus_roots),
                        now,
                    ),
                    _ => {}
                },
                signal = &mut quit => {
                    fired = Some(signal);
                    break;
                }
            }
        }
        Ok(())
    })
    .await;

    renderer.finish_pacing();
    run_outcome(result, renderer.restore_terminal(), fired)
}

/// A signal that ended the loop outranks a failed teardown, which a closed
/// terminal causes: the process still dies by the signal (the exit status a
/// supervisor reads) rather than by an error to a dead stderr.
fn run_outcome(
    result: Result<()>,
    teardown: Result<()>,
    fired: Option<QuitSignal>,
) -> Result<Option<QuitSignal>> {
    match (fired, teardown) {
        (Some(signal), Err(e)) => {
            tracing::warn!(error = ?e, "terminal restore failed after a quit signal");
            result.map(|()| Some(signal))
        }
        (_, teardown) => {
            teardown?;
            result.map(|()| fired)
        }
    }
}

/// The TUI loop's frame clock, one tick per `period`. A frame that overran
/// paints at once and the next back on the grid, neither a burst to catch up
/// nor a re-anchor that lets every late wake stretch the rate
/// ([`MissedTickBehavior::Skip`]).
pub(crate) fn frame_clock(period: Duration) -> tokio::time::Interval {
    let mut frames = tokio::time::interval(period);
    frames.set_missed_tick_behavior(MissedTickBehavior::Skip);
    frames
}

#[cfg(test)]
mod frame_out_tests {
    use super::{BEGIN_SYNC, END_SYNC, FrameOut};
    use std::io::Write;

    /// A terminal that records each write it gets, and fails while `full`;
    /// clones share it.
    #[derive(Clone, Default)]
    struct Tty(std::sync::Arc<std::sync::Mutex<Seen>>);

    #[derive(Default)]
    struct Seen {
        writes: Vec<Vec<u8>>,
        /// Every write call, the failed ones too.
        attempts: usize,
        full: bool,
    }

    impl Tty {
        fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
            self.0.lock().expect("unpoisoned")
        }
    }

    impl Write for Tty {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut seen = self.seen();
            seen.attempts += 1;
            if seen.full {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            seen.writes.push(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A terminal that left: the restore fails, and nothing may write to it
    /// again, or ratatui's Drop `eprintln!`s on the dead stderr and panics.
    /// Unix-only: under `windows-test` the console API takes these commands and
    /// no writer sees a byte (see `teardown_tests`' note).
    #[cfg(unix)]
    #[test]
    fn a_failed_restore_leaves_ratatui_nothing_to_retry_on_drop() {
        use ratatui::{
            Terminal, TerminalOptions, Viewport, backend::CrosstermBackend, layout::Rect,
        };
        let tty = Tty::default();
        let out = FrameOut::new(tty.clone(), false);
        let mut term = Terminal::with_options(
            CrosstermBackend::new(out.clone()),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 4, 4)),
            },
        )
        .expect("a fixed viewport needs no size query");
        term.hide_cursor().expect("hidden");
        tty.seen().full = true;

        assert!(super::teardown_terminal(&mut term, Some(&out)).is_err());
        let attempts = tty.seen().attempts;
        drop(term);

        assert_eq!(
            tty.seen().attempts,
            attempts,
            "the Drop wrote to the dead terminal"
        );
    }

    #[test]
    fn a_quit_signal_outranks_a_failed_teardown() {
        let signal = crate::runtime::QuitSignal::test();
        let failed = || Err(anyhow::anyhow!("EIO"));
        assert_eq!(
            super::run_outcome(Ok(()), failed(), Some(signal)).expect("the signal wins"),
            Some(signal)
        );
        assert!(super::run_outcome(Ok(()), failed(), None).is_err());
        assert!(super::run_outcome(failed(), Ok(()), None).is_err());
    }

    fn frame(out: &mut FrameOut, parts: &[&[u8]]) -> std::io::Result<()> {
        out.begin();
        for part in parts {
            out.write_all(part).expect("held");
            out.flush().expect("held");
        }
        out.present()
    }

    /// A frame's writes reach the terminal as one, inside a synchronized
    /// update when it has one and bare when not; outside a frame they pass.
    #[test]
    fn a_frame_reaches_the_terminal_in_one_write() {
        for sync in [true, false] {
            let tty = Tty::default();
            let mut out = FrameOut::new(tty.clone(), sync);
            frame(&mut out, &[b"tiles", b"text"]).expect("presented");
            let wrapped = [BEGIN_SYNC, b"tiles", b"text", END_SYNC].concat();
            let bare = b"tilestext".to_vec();
            assert_eq!(tty.seen().writes, vec![if sync { wrapped } else { bare }]);
            out.write_all(b"teardown").expect("passes");
            assert_eq!(
                tty.seen().writes.len(),
                2,
                "outside a frame: straight through"
            );
        }
    }

    /// A frame the terminal refused leaves the next one opening with ST, so
    /// an escape it cut short ends before the next frame's bytes, and with
    /// the end of the update it may have left open.
    #[test]
    fn a_refused_frame_opens_the_next_with_st() {
        for sync in [false, true] {
            let tty = Tty::default();
            let mut out = FrameOut::new(tty.clone(), sync);
            tty.seen().full = true;
            assert!(frame(&mut out, &[b"\x1b_Ga=T"]).is_err());
            tty.seen().full = false;
            frame(&mut out, &[b"next"]).expect("presented");
            let opening: &[&[u8]] = if sync {
                &[crate::graphics::ST, END_SYNC, BEGIN_SYNC, b"next", END_SYNC]
            } else {
                &[crate::graphics::ST, b"next"]
            };
            assert_eq!(tty.seen().writes, vec![opening.concat()], "sync {sync}");
        }
    }
}

#[cfg(test)]
mod frame_clock_tests {
    use super::frame_clock;
    use std::time::Duration;
    use tokio::time::{Instant, advance};

    // Whole milliseconds: tokio's timer rounds a deadline up to one.
    const PERIOD: Duration = Duration::from_millis(30);

    /// A render that overran by a non-whole number of periods: the next frame
    /// at once, then back on the start's grid — neither a burst nor a whole
    /// period from the late frame.
    #[tokio::test(start_paused = true)]
    async fn an_overrun_paints_at_once_then_falls_back_on_the_grid() {
        let mut frames = frame_clock(PERIOD);
        let start = Instant::now();
        frames.tick().await;
        advance(PERIOD * 5 / 2).await;
        let late = Instant::now();
        frames.tick().await;
        assert_eq!(Instant::now(), late, "the late frame paints at once");
        frames.tick().await;
        assert_eq!(Instant::now(), start + 3 * PERIOD);
    }
}

#[cfg(test)]
mod capacity_sweep_tests {
    use super::FloorCapacitySweep;
    use pixtuoid_core::state::MAX_FLOORS;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn caps() -> [AtomicUsize; MAX_FLOORS] {
        std::array::from_fn(|_| AtomicUsize::new(0))
    }

    // A 192x80 terminal, i.e. a 192x158 buffer.
    const W: u16 = 192;
    const H: u16 = 158;

    #[test]
    fn a_repeat_frame_serves_the_memo_instead_of_recomputing() {
        let caps = caps();
        let mut sweep = FloorCapacitySweep::new();
        assert!(sweep.publish(W, H, None, &caps), "first frame computes");
        let published: Vec<usize> = caps.iter().map(|c| c.load(Ordering::Relaxed)).collect();
        assert!(
            !sweep.publish(W, H, None, &caps),
            "an unchanged frame must skip the whole MAX_FLOORS layout sweep"
        );
        let after: Vec<usize> = caps.iter().map(|c| c.load(Ordering::Relaxed)).collect();
        assert_eq!(
            published, after,
            "the memo hit must publish the same values"
        );
    }

    #[test]
    fn a_resize_or_a_new_cap_recomputes() {
        let caps = caps();
        let mut sweep = FloorCapacitySweep::new();
        sweep.publish(W, H, None, &caps);
        assert!(sweep.publish(W, H - 2, None, &caps), "a resize recomputes");
        assert!(
            sweep.publish(W, H - 2, Some(4), &caps),
            "a different desk cap recomputes"
        );
    }

    #[test]
    fn published_capacities_are_the_per_floor_auto_capacity_clamped_by_the_cap() {
        let caps = caps();
        let mut sweep = FloorCapacitySweep::new();
        sweep.publish(W, H, None, &caps);
        for (i, slot) in caps.iter().enumerate() {
            let want =
                pixtuoid_scene::floor::floor_capacity(W, H, pixtuoid_scene::floor::floor_seed(i));
            assert_eq!(slot.load(Ordering::Relaxed), want, "floor {i}");
            assert!(want > 0, "floor {i} must seat someone at 192x80");
        }
        let capped: [AtomicUsize; MAX_FLOORS] = std::array::from_fn(|_| AtomicUsize::new(0));
        let mut sweep = FloorCapacitySweep::new();
        sweep.publish(W, H, Some(3), &capped);
        for (i, slot) in capped.iter().enumerate() {
            assert_eq!(
                slot.load(Ordering::Relaxed),
                3,
                "floor {i} clamped to the cap"
            );
        }
    }
}

#[cfg(test)]
mod teardown_tests {
    use super::unwind_after;
    use std::cell::Cell;

    #[cfg(unix)]
    #[test]
    fn the_unwind_unlinks_every_shared_memory_object() {
        use crate::graphics::shm;
        let name = shm::publish(b"tile", std::time::Instant::now()).expect("published");
        let _ = super::unwind_terminal_modes(&mut Vec::new(), || Ok(()));
        assert_eq!(shm::read_and_unlink(&name, 4), None);
    }

    struct FailingWriter;
    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("terminal gone"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("terminal gone"))
        }
    }

    #[test]
    fn raw_mode_is_disabled_even_when_the_escape_write_fails() {
        let disabled = Cell::new(false);
        let err = unwind_after(&[], &mut FailingWriter, || {
            disabled.set(true);
            Ok(())
        })
        .expect_err("the write failure still propagates");
        assert!(
            disabled.get(),
            "raw mode must be disabled even when the escape-sequence write failed \
             — a `?` there strands the user's shell echo-less: {err:#}"
        );
    }

    /// Unix-only: with crossterm's ANSI flag false — as under `windows-test` (piped stdout,
    /// no console, no `TERM`) — these go to the console API and no writer sees a byte.
    #[cfg(unix)]
    const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l";

    #[cfg(unix)]
    #[test]
    fn the_unwind_writes_the_leave_sequence_into_the_writer_it_is_given() {
        let mut buf: Vec<u8> = Vec::new();
        unwind_after(&[], &mut buf, || Ok(())).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.contains(LEAVE_ALT_SCREEN),
            "the unwind must reach the writer handed to it, not a fixed stream: {s:?}"
        );
    }

    /// Unix-only for the same console-API reason as `LEAVE_ALT_SCREEN` above.
    #[cfg(unix)]
    #[test]
    fn raw_mode_is_disabled_only_after_the_escape_bytes_are_written() {
        struct Recorder<'a>(&'a Cell<bool>);
        impl std::io::Write for Recorder<'_> {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.set(true);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let wrote = Cell::new(false);
        let raw_saw_write = Cell::new(false);
        unwind_after(&[], &mut Recorder(&wrote), || {
            raw_saw_write.set(wrote.get());
            Ok(())
        })
        .unwrap();
        assert!(
            raw_saw_write.get(),
            "DisableMouseCapture must reach the terminal while raw mode is still ON"
        );
    }

    #[cfg(unix)]
    #[test]
    fn our_images_go_before_the_alt_screen_that_holds_them() {
        let prelude = crate::graphics::kitty::unwind_for(65536..=65541, false);
        let mut buf: Vec<u8> = Vec::new();
        unwind_after(&prelude, &mut buf, || Ok(())).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let leave = s.find(LEAVE_ALT_SCREEN).expect("leaves");
        let ahead = [super::END_SYNC, &prelude].concat();
        assert!(
            s.as_bytes().starts_with(&ahead) && ahead.len() <= leave,
            "{s:?}"
        );
    }

    /// Every exit, the panic hook's included, first ends a synchronized
    /// update a cut-short frame may have left open. Unix-only for the same
    /// console-API reason as `LEAVE_ALT_SCREEN` above.
    #[cfg(unix)]
    #[test]
    fn the_unwind_first_ends_a_synchronized_update() {
        let mut buf: Vec<u8> = Vec::new();
        unwind_after(&[], &mut buf, || Ok(())).unwrap();
        assert!(buf.starts_with(super::END_SYNC), "{buf:?}");
    }

    #[test]
    fn the_escape_write_error_outranks_a_later_raw_mode_error() {
        let err = unwind_after(&[], &mut FailingWriter, || {
            Err(std::io::Error::other("raw mode gone"))
        })
        .expect_err("both steps failed");
        assert!(
            err.to_string().contains("terminal gone"),
            "the first failure is reported, got: {err:#}"
        );
    }
}

#[cfg(test)]
mod runtime_model {
    #[test]
    fn block_in_place_is_inert_on_the_block_on_thread() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("multi-thread runtime");
        rt.block_on(async {
            let (tx, rx) = std::sync::mpsc::channel::<u8>();
            tokio::spawn(async move {
                tx.send(1).expect("send");
            });
            let got = tokio::task::block_in_place(|| rx.recv().expect("recv"));
            assert_eq!(
                got, 1,
                "the spawned worker progressed while the loop blocked"
            );
            // Without it: observably identical.
            let (tx2, rx2) = std::sync::mpsc::channel::<u8>();
            tokio::spawn(async move {
                tx2.send(2).expect("send");
            });
            assert_eq!(rx2.recv().expect("recv"), 2);
        });
    }
}

/// Tests for the terminal's own key step, ahead of the shared dispatch.
#[cfg(all(test, debug_assertions))]
mod route_tests {
    use super::KeyAction;
    use crate::tui::tui_renderer::TuiRenderer;
    use pixtuoid_scene::theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::SystemTime;

    /// Pins the `!` the mutation run flagged: dropping it makes the toggle a
    /// no-op that always writes the value already there. A panel that owns
    /// input, and any other key, leave the layer alone.
    #[test]
    fn the_w_key_flips_the_walkable_layer_only_with_no_panel_open() {
        use crate::panels::ModalState;
        use crossterm::event::KeyCode;
        let modal = |help_open| ModalState {
            help_open,
            ..ModalState::closed()
        };
        let pack =
            std::sync::Arc::new(pixtuoid_scene::pack::load_bundled_pack().expect("bundled pack"));
        let mut r = TuiRenderer::new(
            Terminal::new(TestBackend::new(120, 60)).expect("test backend"),
            &theme::NORMAL,
            Vec::new(),
            std::sync::Arc::clone(&pack),
        );
        r.render(
            &pixtuoid_core::state::SceneState::uniform(16),
            &pack,
            SystemTime::UNIX_EPOCH,
        )
        .expect("a half-block frame");
        let before = r.debug_walkable();
        assert!(super::toggle_walkable_debug_on_w(
            &mut r,
            KeyCode::Char('w'),
            &modal(false)
        ));
        assert_eq!(r.debug_walkable(), !before, "w must FLIP");
        assert!(super::toggle_walkable_debug_on_w(
            &mut r,
            KeyCode::Char('w'),
            &modal(false)
        ));
        assert_eq!(r.debug_walkable(), before, "w must flip back");
        for (code, modal) in [
            (KeyCode::Char('w'), modal(true)),
            (KeyCode::Char('x'), modal(false)),
        ] {
            assert!(!super::toggle_walkable_debug_on_w(&mut r, code, &modal));
            assert_eq!(r.debug_walkable(), before);
        }
    }

    /// The loop's routing: `w` flips the layer and is swallowed with no panel
    /// open; with one open, and for every other key, the shared dispatch decides.
    #[test]
    fn route_key_reads_w_before_the_shared_dispatch() {
        use crate::panels::{FloorNav, ModalState};
        use crossterm::event::{KeyCode, KeyModifiers};
        let pack =
            std::sync::Arc::new(pixtuoid_scene::pack::load_bundled_pack().expect("bundled pack"));
        let mut r = TuiRenderer::new(
            Terminal::new(TestBackend::new(120, 60)).expect("test backend"),
            &theme::NORMAL,
            Vec::new(),
            std::sync::Arc::clone(&pack),
        );
        r.render(
            &pixtuoid_core::state::SceneState::uniform(16),
            &pack,
            SystemTime::UNIX_EPOCH,
        )
        .expect("a half-block frame");
        let floor = FloorNav {
            n_floors: 1,
            current_floor: 0,
            in_transition: false,
        };
        let key = |code| (code, KeyModifiers::NONE);
        let help = ModalState {
            help_open: true,
            ..ModalState::closed()
        };
        assert_eq!(
            super::route_key(&mut r, key(KeyCode::Char('w')), ModalState::closed(), floor),
            KeyAction::None
        );
        assert!(r.debug_walkable(), "w flipped the layer");
        assert_eq!(
            super::route_key(&mut r, key(KeyCode::Char('?')), ModalState::closed(), floor),
            KeyAction::ToggleHelp,
            "other keys reach the shared dispatch"
        );
        assert_eq!(
            super::route_key(&mut r, key(KeyCode::Char('w')), help, floor),
            crate::panels::dispatch_key(KeyCode::Char('w'), KeyModifiers::NONE, help, floor),
            "a panel open sends w on"
        );
        assert!(r.debug_walkable(), "and leaves the layer alone");
    }
}

/// Tests for the APPLIER half of the key path; `dispatch_tests` covers the decoder.
#[cfg(test)]
mod apply_key_action_tests {
    use crate::panels::{KeyAction, KeyCtx, apply_key_action};
    use crate::tui::tui_renderer::TuiRenderer;
    use pixtuoid_scene::theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::collections::HashSet;
    use std::time::SystemTime;

    /// Stands in for `crate::audio::respawn`, which opens a real output device.
    fn no_respawn(_: &crate::audio::AudioHandle, _: f32) {}

    struct Harness {
        ui: super::ui_state::UiState,
        renderer: TuiRenderer<TestBackend>,
        audio_ctl: crate::audio::AudioController,
        connected: crate::runtime::ConnectedSources,
        snapshot: pixtuoid_core::state::SceneState,
        focus_roots: (Option<std::path::PathBuf>, Option<std::path::PathBuf>),
        installed: fn() -> Vec<&'static str>,
        _tmp: tempfile::TempDir,
        config_path: std::path::PathBuf,
    }

    impl Harness {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let config_path = tmp.path().join("config.toml");
            Self {
                ui: super::ui_state::UiState::new(
                    &theme::NORMAL,
                    crate::panels::welcome::WelcomeUi::from_detected(&[]),
                    tmp.path().join("sock"),
                    None,
                    crate::doctor::DriftSeen::default(),
                ),
                renderer: TuiRenderer::new(
                    Terminal::new(TestBackend::new(80, 24)).expect("test backend"),
                    &theme::NORMAL,
                    Vec::new(),
                    std::sync::Arc::new(
                        pixtuoid_scene::pack::load_bundled_pack().expect("bundled pack"),
                    ),
                ),
                // UNMUTED, because a MUTED controller hides pause (`set_paused` ORs the
                // mute flag in).
                audio_ctl: crate::audio::AudioController::new_with(
                    crate::config::AudioConfig {
                        muted: false,
                        volume: 1.0,
                    },
                    config_path.clone(),
                    no_respawn,
                ),
                connected: crate::runtime::ConnectedSources::new(HashSet::new()),
                snapshot: pixtuoid_core::state::SceneState::uniform(4),
                focus_roots: (None, None),
                installed: Vec::new,
                _tmp: tmp,
                config_path,
            }
        }

        /// Register `n` agents so the dashboard has rows to move through.
        fn seed_agents(&mut self, n: usize) {
            use pixtuoid_core::source::{AgentEvent, Transport};
            let mut r = pixtuoid_core::Reducer::new();
            for i in 0..n {
                let path = format!("/p/agent-{i}.jsonl");
                r.apply(
                    &mut self.snapshot,
                    AgentEvent::SessionStart {
                        agent_id: pixtuoid_core::AgentId::from_transcript_path(&path),
                        source: "claude-code".into(),
                        session_id: path.clone(),
                        cwd: std::path::PathBuf::from("/repo"),
                        parent_id: None,
                    },
                    SystemTime::UNIX_EPOCH,
                    Transport::Jsonl,
                );
            }
        }

        fn apply(&mut self, action: KeyAction) -> bool {
            apply_key_action(
                action,
                &mut KeyCtx {
                    ui: &mut self.ui,
                    host: &mut self.renderer,
                    audio_ctl: &mut self.audio_ctl,
                    config_path: &self.config_path,
                    connected: &self.connected,
                    snapshot: &self.snapshot,
                    focus_roots: &self.focus_roots,
                    now: SystemTime::UNIX_EPOCH,
                    respawn: no_respawn,
                    installed: self.installed,
                },
            )
        }
    }

    impl Harness {
        /// Onboarding open on `detected`, as a first run shows it.
        fn onboarding(detected: &[&'static str]) -> Self {
            let mut h = Self::new();
            h.ui = super::ui_state::UiState::new(
                &theme::NORMAL,
                crate::panels::welcome::WelcomeUi::from_detected(detected),
                h._tmp.path().join("sock"),
                None,
                crate::doctor::DriftSeen::default(),
            );
            h
        }
    }

    #[test]
    fn skipping_onboarding_writes_nothing() {
        let mut h = Harness::onboarding(&["copilot"]);
        assert!(h.ui.onboarding_open());
        h.apply(KeyAction::OnboardingSkip);
        assert!(!h.ui.onboarding_open());
        assert!(!h.config_path.exists(), "skip must not touch the config");
        assert!(!h.connected.is_connected("copilot"));
    }

    #[test]
    fn confirming_onboarding_connects_the_checked_rows_and_opens_their_gate() {
        let mut h = Harness::onboarding(&["copilot"]);
        h.apply(KeyAction::OnboardingConfirm);
        assert!(!h.ui.onboarding_open());
        assert!(h.connected.is_connected("copilot"));
        let cfg = crate::config::load(&h.config_path, &mut Vec::new());
        assert_eq!(cfg.sources, [("copilot".to_string(), true)].into());
    }

    /// `v` reopens the popup with the notices due against the LIVE connections:
    /// a host connected since boot is not named.
    #[test]
    fn v_reopens_the_popup_with_the_notices_still_due() {
        let mut h = Harness::new();
        h.ui = super::ui_state::UiState::upgraded_from("0.19.0", "0.20.0");
        h.connected = crate::runtime::ConnectedSources::new(HashSet::from(["codex".to_string()]));
        h.installed = || vec!["codex", "reasonix"];
        assert!(!h.apply(KeyAction::OpenVersionPopup));
        assert!(h.ui.modal().version_popup_open);
        let shown = h.ui.version_notices();
        assert!(
            shown
                .iter()
                .any(|n| n.contains("Reasonix") && !n.contains("Codex")),
            "{shown:?}"
        );
    }

    /// The highest-blast-radius pair: `Quit` must be the ONLY action that ends
    /// the loop. A mutant returning a constant makes every keypress quit (or
    /// makes `q` inert), and nothing else in the suite would notice.
    #[test]
    fn only_quit_returns_true() {
        let mut h = Harness::new();
        assert!(h.apply(KeyAction::Quit), "Quit must end the loop");
        for action in [
            KeyAction::None,
            KeyAction::TogglePause,
            KeyAction::ToggleHelp,
            KeyAction::CloseHelp,
            KeyAction::DismissVersionPopup,
            KeyAction::OpenVersionPopup,
            KeyAction::OpenThemePicker,
            KeyAction::ToggleDashboard,
            KeyAction::DashboardClose,
            KeyAction::ConnectionClose,
        ] {
            assert!(
                !h.apply(action),
                "only Quit may end the loop, but {action:?} did"
            );
        }
    }

    /// `set_paused` must read `paused()` AFTER the toggle. Asserting only
    /// `ui.paused()` is NOT enough — that survives swapping the two statements,
    /// because the UI flag flips either way. The audio handle is what goes out
    /// of sync, so assert THAT: pausing must mute, unpausing must unmute.
    #[test]
    fn toggle_pause_drives_the_audio_handle_in_step_with_the_ui() {
        let mut h = Harness::new();
        assert!(!h.ui.paused());
        assert!(
            !h.audio_ctl.handle().is_muted(),
            "harness precondition: the controller starts unmuted"
        );

        h.apply(KeyAction::TogglePause);
        assert!(h.ui.paused(), "p must pause the UI");
        assert!(
            h.audio_ctl.handle().is_muted(),
            "p must mute audio in the SAME apply — a set_paused read before the \
             toggle leaves the handle a step behind"
        );

        h.apply(KeyAction::TogglePause);
        assert!(!h.ui.paused(), "p again must unpause the UI");
        assert!(
            !h.audio_ctl.handle().is_muted(),
            "unpause must unmute in the same apply"
        );
    }

    /// `delete -` on either `-1` makes Up behave as Down. The panels are
    /// independent, so both pairs need pinning — and each needs at least two
    /// rows, or the move is a no-op in both directions and the assertion is
    /// vacuous.
    #[test]
    fn dashboard_and_connection_up_move_opposite_to_down() {
        let mut h = Harness::new();
        h.seed_agents(3);

        h.apply(KeyAction::ToggleDashboard);
        let start = h.ui.dashboard.selected;
        h.apply(KeyAction::DashboardDown);
        let after_down = h.ui.dashboard.selected;
        assert_ne!(after_down, start, "precondition: Down actually moves");
        h.apply(KeyAction::DashboardUp);
        // Must return to `start`, not merely DIFFER from `after_down` — with the
        // `-1` deleted, Up moves further DOWN, which also differs.
        assert_eq!(
            h.ui.dashboard.selected, start,
            "DashboardUp must undo DashboardDown, not advance further"
        );

        h.apply(KeyAction::ToggleConnection);
        assert!(
            h.ui.connection.rows.len() > 1,
            "precondition: the Sources panel lists the registry, so it has rows"
        );
        let start = h.ui.connection.selected;
        h.apply(KeyAction::ConnectionDown);
        let after_down = h.ui.connection.selected;
        assert_ne!(after_down, start, "precondition: Down actually moves");
        h.apply(KeyAction::ConnectionUp);
        assert_eq!(
            h.ui.connection.selected, start,
            "ConnectionUp must undo ConnectionDown, not advance further"
        );
    }

    /// `ThemeCommit` persists; `ThemeCancel` restores the last COMMITTED theme,
    /// not merely the previewed one.
    #[test]
    fn theme_commit_persists_and_cancel_restores_the_saved_theme() {
        let mut h = Harness::new();
        h.apply(KeyAction::OpenThemePicker);
        h.apply(KeyAction::ThemeCommit(1));
        let saved = std::fs::read_to_string(&h.config_path).expect("config written");
        assert!(
            saved.contains(theme::ALL_THEMES[1].name),
            "commit must persist the theme: {saved:?}"
        );

        h.apply(KeyAction::OpenThemePicker);
        h.apply(KeyAction::ThemePreview(3));
        h.apply(KeyAction::ThemeCancel);
        assert_eq!(
            h.ui.saved_theme_idx, 1,
            "cancel must restore the COMMITTED theme, not the preview"
        );
    }
}

#[cfg(test)]
mod key_event_tests {
    #[test]
    fn only_press_events_dispatch() {
        use crossterm::event::KeyEventKind;
        assert!(super::should_dispatch_key(KeyEventKind::Press));
        assert!(!super::should_dispatch_key(KeyEventKind::Release));
        assert!(!super::should_dispatch_key(KeyEventKind::Repeat));
    }
}
