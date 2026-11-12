//! The modal panels both painters show: the dashboard, the Sources panel, the
//! help, the appearance picker, the version popup and the onboarding, with the one
//! key dispatch that drives them ([`dispatch_key`] decodes, [`apply_key_action`]
//! applies) and the one [`mouse_ladder`]. A painter draws them through
//! [`widgets`] into a ratatui frame, carries out what only it can through
//! [`Host`] and [`Office`], and names a click's desktop launchers in [`ClickCtx`].

pub(crate) mod connection;
pub(crate) mod dashboard;
pub(crate) mod ui_state;
pub(crate) mod welcome;
pub(crate) mod widgets;

use std::time::{Instant, SystemTime};

use crossterm::event::{KeyCode, KeyModifiers};
use pixtuoid_core::AgentSlot;
use pixtuoid_scene::hit::HitAction;
use pixtuoid_scene::theme;
use ratatui::layout::Rect;

/// How many rows at the bottom of a surface of cells the status footer owns.
pub(crate) const FOOTER_ROWS: u16 = 1;

/// `full` less its footer rows: where the office and the panels are.
pub(crate) fn scene_rect(full: Rect) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: full.width,
        height: full.height.saturating_sub(FOOTER_ROWS),
    }
}

/// Clip a widget rect to fit inside `bounds`; `None` when nothing survives.
/// Prevents ratatui's "index outside of buffer" panic when label/notice widgets
/// land near the right or bottom edge.
pub(crate) fn clip_widget_rect(rect: Rect, bounds: Rect) -> Option<Rect> {
    if rect.x >= bounds.x + bounds.width || rect.y >= bounds.y + bounds.height {
        return None;
    }
    if rect.x + rect.width <= bounds.x || rect.y + rect.height <= bounds.y {
        return None;
    }
    let x = rect.x.max(bounds.x);
    let y = rect.y.max(bounds.y);
    let right = (rect.x + rect.width).min(bounds.x + bounds.width);
    let bot = (rect.y + rect.height).min(bounds.y + bounds.height);
    if right <= x || bot <= y {
        return None;
    }
    Some(Rect {
        x,
        y,
        width: right - x,
        height: bot - y,
    })
}

/// One frame's open panels, as `paint_overlays` draws them.
#[derive(Debug, Clone, Copy)]
pub struct OverlayFrame<'a> {
    pub(crate) appearance: ui_state::AppearanceFrame,
    pub(crate) dashboard: &'a crate::panels::dashboard::DashboardFrame,
    pub(crate) connection: &'a crate::panels::connection::ConnectionFrame,
    /// The version popup's scale: 0.0 hidden, 1.0 whole.
    pub(crate) popup_scale: f32,
    /// What the version popup shows beside its release.
    pub(crate) version_popup: &'a ui_state::VersionPopupFrame,
    pub(crate) help_open: bool,
    /// Shortcuts the host adds to the help, after the shared ones.
    pub(crate) host_keys: &'a [widgets::Shortcut],
    pub(crate) onboarding: &'a crate::panels::welcome::OnboardingFrame,
}

impl OverlayFrame<'static> {
    /// Every panel closed: a still's.
    pub(crate) fn closed() -> Self {
        use std::sync::LazyLock;
        static CLOSED: LazyLock<crate::panels::ui_state::RenderFrames> =
            LazyLock::new(Default::default);
        CLOSED.overlays(0.0, &[])
    }
}

impl OverlayFrame<'_> {
    /// Whether any panel shows. Destructured whole, so a new panel can't
    /// paint without counting here.
    pub(crate) fn any_open(&self) -> bool {
        let &Self {
            appearance,
            dashboard,
            connection,
            popup_scale,
            version_popup: _,
            help_open,
            host_keys: _,
            onboarding,
        } = self;
        appearance.open
            || dashboard.open
            || connection.open
            || popup_scale > 0.0
            || help_open
            || onboarding.open
    }
}

/// The modal-overlay dispatch, centralized so the draw paths can't drift in
/// ordering or args. `bounds` is the FULL terminal area — a modal is centered over
/// the whole frame, and `PanelGeometry` keeps it off the footer row itself.
pub(crate) fn paint_overlays(
    f: &mut ratatui::Frame<'_>,
    ov: &OverlayFrame<'_>,
    now: SystemTime,
    bounds: Rect,
    theme: &pixtuoid_scene::theme::Theme,
) {
    let &OverlayFrame {
        appearance,
        dashboard,
        connection,
        popup_scale,
        version_popup,
        help_open,
        host_keys,
        onboarding,
    } = ov;
    if appearance.open {
        widgets::paint_appearance(f, appearance.shown, bounds, theme);
    }
    if dashboard.open {
        widgets::paint_dashboard(f, dashboard, now, bounds, theme);
    }
    if connection.open {
        widgets::paint_connection_panel(f, connection, now, bounds, theme);
    }
    if popup_scale > 0.0 {
        widgets::paint_version_popup(
            f,
            env!("CARGO_PKG_VERSION"),
            version_popup,
            bounds,
            theme,
            popup_scale,
        );
    }
    if help_open {
        widgets::paint_help_overlay(f, bounds, theme, host_keys, appearance.shown.look.is_some());
    }
    if onboarding.open {
        widgets::paint_welcome(f, onboarding, bounds, theme);
    }
}

/// What an applied key changes in the painter that shows the office.
pub(crate) trait Host {
    fn set_theme(&mut self, theme: &'static theme::Theme);
    /// Draw `look` from the next frame on, the screen cleared of the last.
    fn set_look(&mut self, look: pixtuoid_scene::look::LookKind) -> anyhow::Result<()>;
    fn navigate_floor(&mut self, floor: usize, now: SystemTime);
    /// Repaint the whole surface.
    fn redraw(&mut self) -> anyhow::Result<()>;
}

/// Which overlay (if any) currently owns input.
/// An open overlay swallows keys and the normal-scene bindings are suspended; the
/// precedence chain itself lives in [`dispatch_key`], and a host's own key reads
/// [`Self::any_open`] to defer the same way.
#[derive(Clone, Copy)]
pub(crate) struct ModalState {
    pub(crate) onboarding_open: bool,
    /// The onboarding's change runs: its card takes only a quit, and Esc,
    /// which hides it while the change goes on.
    pub(crate) onboarding_busy: bool,
    pub(crate) help_open: bool,
    pub(crate) version_popup_open: bool,
    pub(crate) appearance: Option<ui_state::AppearancePick>,
    pub(crate) dashboard_open: bool,
    pub(crate) connection_open: bool,
    /// A disconnect is armed on the Sources panel, awaiting [`keys::CONFIRM`]
    /// or [`keys::DECLINE`].
    pub(crate) connection_confirm: bool,
}

impl ModalState {
    /// No panel open.
    #[cfg(test)]
    pub(crate) fn closed() -> Self {
        Self {
            onboarding_open: false,
            onboarding_busy: false,
            help_open: false,
            version_popup_open: false,
            appearance: None,
            dashboard_open: false,
            connection_open: false,
            connection_confirm: false,
        }
    }

    /// Whether any panel owns input, so the office behind takes no click.
    /// Destructured whole, so a new panel can't take keys without counting
    /// here.
    pub(crate) fn any_open(&self) -> bool {
        let &Self {
            onboarding_open,
            onboarding_busy: _,
            help_open,
            version_popup_open,
            appearance,
            dashboard_open,
            connection_open,
            connection_confirm: _,
        } = self;
        onboarding_open
            || help_open
            || version_popup_open
            || appearance.is_some()
            || dashboard_open
            || connection_open
    }
}

/// What the open panels make of a pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModalMouse {
    /// No panel is open: the office takes it.
    Office,
    /// A panel acted on it: the help closed, or the popup's link opened.
    Took,
    /// A panel is open and did nothing with it.
    Inert,
}

/// A pointer event, as [`mouse_ladder`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PointerEvent {
    /// The left button pressed.
    Press,
    Move,
    /// The left button released.
    Release,
}

/// The office behind the panels, as a painter's pointer drives it.
pub(crate) trait Office {
    /// A left press the panels left it.
    fn press(&mut self);
    /// The pointer moved: a figure in hand follows.
    fn moved(&mut self);
    /// The left button released: the click's action; a figure in hand is set
    /// down.
    fn release(&mut self) -> Option<pixtuoid_scene::hit::HitAction>;
    /// A click on the floor's `kind` of pet.
    fn pet(&mut self, kind: pixtuoid_scene::pet::PetKind);
}

/// What a click reaches outside the office.
pub(crate) struct ClickCtx<'a> {
    /// The office a focused agent is looked up in, as the painter's keys read
    /// it; `None` before one runs.
    pub(crate) scene: Option<&'a pixtuoid_core::state::SceneState>,
    /// Injected because the real `crate::focus::focus_slot` raises a terminal,
    /// so a test's click would drive the developer's desktop.
    pub(crate) focus: fn(&AgentSlot),
    /// Injected because the real `crate::open_url` launches a browser.
    pub(crate) open: fn(&str),
}

/// The mouse ladder both painters run: [`modal_mouse`], then `office`. Only a
/// press waits on the panels; the move and release of a gesture the office
/// began reach it whatever opened since, so a figure lifted before a panel
/// opened still follows the pointer and is set down. A release's click is
/// carried out only with no panel open. `at` is the cell the pointer is on,
/// read for a press.
pub(crate) fn mouse_ladder(
    ui: &mut ui_state::UiState,
    popup_scale: f32,
    event: PointerEvent,
    at: Option<(u16, u16)>,
    screen: impl FnOnce() -> Option<(u16, u16)>,
    office: &mut impl Office,
    cx: ClickCtx<'_>,
) -> ModalMouse {
    let left_down = at.filter(|_| event == PointerEvent::Press);
    let modal = modal_mouse(ui, popup_scale, left_down, screen, cx.open);
    match event {
        PointerEvent::Press => {
            if modal == ModalMouse::Office {
                office.press();
            }
        }
        PointerEvent::Move => office.moved(),
        PointerEvent::Release => match office.release().filter(|_| modal == ModalMouse::Office) {
            Some(HitAction::Focus(id)) => {
                if let Some(slot) = cx.scene.and_then(|scene| scene.agents.get(&id)) {
                    (cx.focus)(slot);
                }
            }
            Some(HitAction::Pet(kind)) => office.pet(kind),
            Some(HitAction::Open(url)) => (cx.open)(url),
            None => {}
        },
    }
    modal
}

/// The modal half of the mouse ladder, in order: the onboarding swallows
/// everything, a left press closes the help (tested before the popup so it
/// wins mid popup-dismiss), the version popup takes only its URL, and the
/// picker, dashboard and Sources panel are inert BY DESIGN, closing only by
/// key ([`dispatch_key`]). A click never leaks to the office behind a panel,
/// where a coffee-machine or branding hit launches a browser. `left_down` is
/// the cell a left press landed on; `popup_scale` the popup's painted scale;
/// `screen` the surface's size in cells, read only for a press on the popup;
/// `open` what launches the popup's URL.
fn modal_mouse(
    ui: &mut ui_state::UiState,
    popup_scale: f32,
    left_down: Option<(u16, u16)>,
    screen: impl FnOnce() -> Option<(u16, u16)>,
    open: fn(&str),
) -> ModalMouse {
    if ui.onboarding_open() {
        return ModalMouse::Inert;
    }
    if ui.help_open() {
        if left_down.is_none() {
            return ModalMouse::Inert;
        }
        ui.close_help();
        return ModalMouse::Took;
    }
    if popup_scale > 0.0 {
        let notices = ui.version_notices();
        let on_url = left_down.is_some_and(|(col, row)| {
            screen()
                .is_some_and(|size| version_popup_url_clicked(col, row, popup_scale, size, notices))
        });
        if !on_url {
            return ModalMouse::Inert;
        }
        open(&widgets::release_url(env!("CARGO_PKG_VERSION")));
        return ModalMouse::Took;
    }
    if ui.appearance.is_some() || ui.dashboard.open || ui.connection.open {
        return ModalMouse::Inert;
    }
    ModalMouse::Office
}

/// Whether a left-click at `(col, row)` landed on the version popup's URL, hit-tested
/// against the full surface, not the scene rect. `scale` is the popup's last painted
/// scale, and `notices` the ones it lists.
fn version_popup_url_clicked(
    col: u16,
    row: u16,
    scale: f32,
    screen: (u16, u16),
    notices: &[String],
) -> bool {
    let bounds = Rect::new(0, 0, screen.0, screen.1);
    widgets::version_popup_url_rect(bounds, notices, scale)
        .is_some_and(|rect| rect.contains(ratatui::layout::Position { x: col, y: row }))
}

#[derive(Clone, Copy)]
pub(crate) struct FloorNav {
    pub(crate) n_floors: usize,
    pub(crate) current_floor: usize,
    pub(crate) in_transition: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyAction {
    None,
    Quit,
    TogglePause,
    ToggleHelp,
    CloseHelp,
    DismissVersionPopup,
    /// Reopen the version popup, its notices due again.
    OpenVersionPopup,
    OpenAppearance,
    /// The index is pre-clamped by the dispatch.
    ThemePreview(usize),
    /// Only where the picker has a look row.
    LookPreview(pixtuoid_scene::look::LookKind),
    AppearanceCommit,
    AppearanceCancel,
    /// Already validated: in range, and no transition in flight.
    NavigateFloor(usize),
    ToggleAudioMute,
    /// `true` = up.
    AdjustVolume(bool),
    ToggleDashboard,
    DashboardUp,
    DashboardDown,
    DashboardFoldLeft,
    DashboardFoldRight,
    DashboardFoldAll,
    DashboardJump,
    DashboardFocus,
    DashboardClose,
    /// Open/close the Sources panel.
    ToggleConnection,
    ConnectionUp,
    ConnectionDown,
    /// Connecting is immediate; disconnecting arms a confirm first, since it removes
    /// hooks and walks characters out.
    ConnectionToggle,
    ConnectionConfirm,
    ConnectionCancelConfirm,
    ConnectionClose,
    OnboardingUp,
    OnboardingDown,
    OnboardingToggle,
    OnboardingConfirm,
    OnboardingSkip,
    /// Ctrl-L: repaint the whole screen, images included.
    Redraw,
}

/// What the Sources panel or the onboarding runs off the loop: a change of
/// the sources, or a read of the panel's rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PanelJob {
    /// The panel's toggle: connect `source_id` when `on`, else disconnect it.
    Toggle {
        source_id: &'static str,
        name: &'static str,
        on: bool,
    },
    /// The onboarding's confirm: connect each, in order.
    Onboarding(Vec<&'static str>),
    /// The panel's open: its rows, read again.
    Refresh,
}

impl PanelJob {
    /// What it does to `source_id`, when it changes that source at all.
    fn op_for(&self, source_id: &str) -> Option<connection::SourceOp> {
        match self {
            Self::Toggle {
                source_id: id, on, ..
            } => (*id == source_id).then(|| connection::SourceOp::of(*on)),
            Self::Onboarding(ids) => ids
                .contains(&source_id)
                .then_some(connection::SourceOp::Connect),
            Self::Refresh => None,
        }
    }

    /// The line that refuses a change while this job runs.
    fn busy_line(&self) -> String {
        match self {
            Self::Toggle { name, on, .. } => format!(
                "{name}: still {} \u{2014} one change at a time",
                connection::SourceOp::of(*on).progress_word()
            ),
            Self::Onboarding(_) => format!(
                "still {} what you picked \u{2014} one change at a time",
                connection::SourceOp::Connect.progress_word()
            ),
            Self::Refresh => "still reading the sources \u{2014} try again in a moment".into(),
        }
    }
}

/// A [`PanelJob`] and what its thread runs it with.
pub(crate) struct JobInput {
    job: PanelJob,
    config_path: std::path::PathBuf,
    connected: crate::runtime::ConnectedSources,
    /// Where the rebuilt rows read their drift history.
    log: Option<crate::run_log::LogLocation>,
}

/// A finished [`PanelJob`]: what the user must hear of it, and the Sources
/// panel's rows rebuilt after it.
#[derive(Debug, Clone)]
pub(crate) struct JobDone {
    /// The onboarding's, whose card it closes.
    onboarding: bool,
    notices: Vec<ChangeNotice>,
    rows: Vec<connection::ConnectionRow>,
    /// The agent CLIs on this machine, which the version popup reads.
    installed: Vec<&'static str>,
}

/// What a painter's loop takes back from work the panels started off it.
#[derive(Debug, Clone)]
pub(crate) enum Landed {
    Job(JobDone),
    /// A [`Saves`] write of a setting the footer tracks.
    Saved(crate::config::SaveOutcome),
}

/// Where a painter's loop receives each [`Landed`].
pub(crate) type LandedTx = tokio::sync::mpsc::UnboundedSender<Landed>;

/// Run `input`'s job, with every step of it that can block: the config
/// writes, the host CLIs a connect runs, and the rows' rebuild, whose
/// `diagnose` can run a CLI too.
pub(crate) fn run_job(input: JobInput) -> JobDone {
    let JobInput {
        job,
        config_path,
        connected,
        log,
    } = input;
    let (onboarding, notices) = match job {
        PanelJob::Toggle {
            source_id,
            name,
            on,
        } => (
            false,
            vec![set_connected(&config_path, &connected, source_id, name, on)],
        ),
        PanelJob::Onboarding(ids) => {
            let outcomes = crate::sources::connect_each(&config_path, &ids);
            for (id, _) in &outcomes {
                crate::sources::refresh_gate(&config_path, &connected, id);
            }
            (true, onboarding_notices(&outcomes))
        }
        PanelJob::Refresh => (false, Vec::new()),
    };
    let rows = connection::build_rows(&connected.snapshot(), &ui_state::log_text(log.as_ref()));
    JobDone {
        onboarding,
        notices,
        rows,
        installed: crate::sources::detect(),
    }
}

/// Start `job` on a thread of its own, which sends its [`JobDone`] to
/// `cx.job_tx`: a host CLI's run can take [minutes](crate::install::io::HostCli::run),
/// which a painter's loop must not wait out. One runs at a time, as a paired
/// device's changes do (`serve::device`'s gate): a hung host CLI holds its job
/// until its limit, and each job's rows replace the panel's.
fn start_job<H: Host>(cx: &mut KeyCtx<'_, H>, job: PanelJob) {
    if let Some(running) = &cx.ui.running_job {
        // A refresh needs none of its own: the running job brings fresh rows.
        if job != PanelJob::Refresh {
            cx.ui.connection.last_result = Some(running.busy_line());
        }
        return;
    }
    let input = JobInput {
        job: job.clone(),
        config_path: cx.config_path.to_path_buf(),
        connected: cx.connected.clone(),
        log: cx.ui.log().cloned(),
    };
    let (run, job_tx) = (cx.run_job, cx.job_tx.clone());
    // Not `spawn_blocking`: the runtime ends before the quit kills the CLI
    // (`app::run`), which would leave the job unjoined and its rollback unwritten.
    let started = std::thread::Builder::new()
        .name("panel-job".into())
        .spawn(move || {
            let _ = job_tx.send(Landed::Job(run(input)));
        });
    match started {
        Ok(thread) => {
            panel_threads().push(thread);
            // The line a change before it left would stand for this one.
            if job != PanelJob::Refresh {
                cx.ui.connection.last_result = None;
            }
            cx.ui.running_job = Some(job);
        }
        Err(e) => cx.ui.connection.last_result = Some(format!("could not start: {e}")),
    }
}

/// The threads the panels started this run, for a quit to let them finish: a
/// killed CLI leaves its change a rollback to write, and closed [`Saves`] their
/// queued writes.
static PANEL_THREADS: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(Vec::new());

fn panel_threads() -> std::sync::MutexGuard<'static, Vec<std::thread::JoinHandle<()>>> {
    let mut threads = PANEL_THREADS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    threads.retain(|thread| !thread.is_finished());
    threads
}

/// Wait for every thread the panels started this run to end.
pub(crate) fn join_panel_threads() {
    // Taken out first: a `for` head's guard would hold the lock across the
    // joins, which a last `Saves` dropped meanwhile needs.
    let threads = std::mem::take(&mut *panel_threads());
    for thread in threads {
        let _ = thread.join();
    }
}

/// The painter's config writes, on a thread of their own in the order they
/// were made: a write can block on a stalled disk, which the painter's loop
/// must not wait out, and two saves of one setting must land in that order,
/// which the [config](crate::config)'s own turns do not keep. A setting's
/// outcome comes back as a [`Landed::Saved`].
#[derive(Clone)]
pub(crate) struct Saves(std::sync::Arc<SavesThread>);

/// One write of the config at the path handed to it, and what the footer
/// hears of it.
type Save = Box<dyn FnOnce(&std::path::Path) -> Option<crate::config::SaveOutcome> + Send>;

struct SavesThread {
    queue: std::sync::mpsc::Sender<Save>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Saves {
    /// The thread that writes the config at `config_path`, its outcomes sent
    /// to `landed`.
    ///
    /// # Errors
    ///
    /// If the thread cannot start.
    pub(crate) fn spawn(
        config_path: std::path::PathBuf,
        landed: LandedTx,
    ) -> std::io::Result<Self> {
        let (queue, saves) = std::sync::mpsc::channel::<Save>();
        // Not `spawn_blocking`, for `start_job`'s reason: the quit joins it
        // after the runtime ends.
        let thread = std::thread::Builder::new()
            .name("config-saves".into())
            .spawn(move || {
                for save in saves {
                    if let Some(outcome) = save(&config_path) {
                        let _ = landed.send(Landed::Saved(outcome));
                    }
                }
            })?;
        Ok(Self(std::sync::Arc::new(SavesThread {
            queue,
            thread: Some(thread),
        })))
    }

    pub(crate) fn save(
        &self,
        save: impl FnOnce(&std::path::Path) -> Option<crate::config::SaveOutcome> + Send + 'static,
    ) {
        let _ = self.0.queue.send(Box::new(save));
    }

    /// Wait at most `bound` for every save queued so far to land; whether
    /// they did. A save queued after them is the mark, since they run in order.
    pub(crate) fn flush(&self, bound: std::time::Duration) -> bool {
        let (mark, marked) = std::sync::mpsc::channel();
        self.save(move |_| {
            let _ = mark.send(());
            None
        });
        marked.recv_timeout(bound).is_ok()
    }
}

impl Drop for SavesThread {
    /// Hands the thread to the quit's [`join_panel_threads`] only once the queue closes,
    /// so that join waits for it to write what is queued rather than for good.
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            panel_threads().push(thread);
        }
    }
}

/// Take in what landed: a job, or a save's outcome for the footer.
pub(crate) fn land(ui: &mut ui_state::UiState, landed: Landed) {
    match landed {
        Landed::Job(done) => finish_job(ui, done),
        Landed::Saved(outcome) => ui.note_save(outcome),
    }
}

/// Take a finished job into the panels: its mark cleared, the onboarding's
/// card closed if it was its job and still shows, and its notices shown.
fn finish_job(ui: &mut ui_state::UiState, done: JobDone) {
    let JobDone {
        onboarding,
        notices,
        rows,
        installed,
    } = done;
    ui.note_installed(installed);
    // The refusal it caused ends with it; a line it did not cause stays.
    if let Some(job) = ui.running_job.take()
        && ui.connection.last_result.as_ref() == Some(&job.busy_line())
    {
        ui.connection.last_result = None;
    }
    if onboarding && ui.onboarding_open() {
        ui.close_onboarding();
    }
    surface_notices(ui, rows, notices);
}

/// Connect `source_id` when `on`, else disconnect it, and word what that did.
fn set_connected(
    config_path: &std::path::Path,
    connected: &crate::runtime::ConnectedSources,
    source_id: &'static str,
    name: &str,
    on: bool,
) -> ChangeNotice {
    let done = if on {
        crate::sources::connect_source(config_path, connected, source_id)
    } else {
        crate::sources::disconnect_source(config_path, connected, source_id)
    };
    ChangeNotice::of(source_id, &done, connection::SourceOp::of(on), name)
}

/// How the panel words a change of the source named `name`:
/// [`Changed::line`](pixtuoid_core::remote::Changed::line)'s, which a paired device
/// shows too, or on a failure of `op` its reason.
fn result_line(
    done: &anyhow::Result<pixtuoid_core::remote::Changed>,
    op: connection::SourceOp,
    name: &str,
) -> String {
    match done {
        Ok(changed) => changed.line(name),
        Err(e) => connection::format_failure(op, name, &crate::install::io::failure_reason(e)),
    }
}

/// A change's line about one source, and the row it is about, which the
/// panel selects: on a failed row [`keys::SOURCE_TOGGLE`] retries the change.
#[derive(Debug, Clone)]
struct ChangeNotice {
    source_id: &'static str,
    line: String,
    failed: bool,
    /// A success that asks no step and warns of nothing, so no closed panel
    /// opens for it.
    quiet: bool,
}

impl ChangeNotice {
    fn of(
        source_id: &'static str,
        done: &anyhow::Result<pixtuoid_core::remote::Changed>,
        op: connection::SourceOp,
        name: &str,
    ) -> Self {
        Self {
            source_id,
            line: result_line(done, op, name),
            failed: done.is_err(),
            quiet: done
                .as_ref()
                .is_ok_and(|changed| changed.notes().is_empty()),
        }
    }
}

/// One presentable line per onboarding connect the user must still hear about —
/// a failure, or a success whose change asks a step or warns: in TUI mode the
/// alternate screen owns the terminal, so the warn-floor log is not a user surface.
fn onboarding_notices(outcomes: &[crate::sources::SourceChange]) -> Vec<ChangeNotice> {
    let mut notices = Vec::new();
    for &(id, ref done) in outcomes {
        if let Err(e) = done {
            tracing::warn!(source = %id, error = ?e, "onboarding: connect failed");
        }
        let name = pixtuoid_core::source::display_name(id).unwrap_or(id);
        let notice = ChangeNotice::of(id, done, connection::SourceOp::Connect, name);
        if !notice.quiet {
            notices.push(notice);
        }
    }
    notices
}

/// Show a finished change's notices on the Sources panel, its rows now
/// `rows`: on the open panel as its result line, else, for a notice that is
/// not quiet, by opening it on the first failed row, where
/// [`keys::SOURCE_TOGGLE`] retries it, or on the first noted one.
fn surface_notices(
    ui: &mut ui_state::UiState,
    rows: Vec<connection::ConnectionRow>,
    notices: Vec<ChangeNotice>,
) {
    let heard = notices
        .iter()
        .find(|n| n.failed)
        .or_else(|| notices.iter().find(|n| !n.quiet))
        .map(|n| n.source_id);
    match heard {
        Some(source_id) if !ui.connection.open => {
            ui.open_connection(rows);
            ui.select_connection_source(source_id);
        }
        _ => ui.connection.rows = rows,
    }
    if ui.connection.open && !notices.is_empty() {
        ui.connection.last_result = Some(
            notices
                .into_iter()
                .map(|n| n.line)
                .collect::<Vec<_>>()
                .join("  \u{b7}  "),
        );
    }
}

pub(crate) fn is_quit_chord(code: KeyCode, mods: KeyModifiers) -> bool {
    matches!(
        (code, mods),
        (keys::QUIT, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL)
    )
}

/// The one key of each command a hint names: the dispatch matches it and the
/// hint writes it, so a rebind is one edit. Spelled out instead: the moves
/// (arrows, `j`/`k`, `h`/`l`, Page Up and Down), Enter and Esc, which no one
/// command owns; Ctrl+C and Ctrl+L, chords a `KeyCode` holds no modifier of;
/// and the files pixtuoid writes into another CLI's tree, which a test pins
/// to [`SOURCES`](keys::SOURCES)
/// (`every_file_naming_the_sources_panel_names_its_key`). The window's zoom
/// chords keep their own table, `floating::input::ZOOM_CHORDS`.
pub(crate) mod keys {
    use crossterm::event::KeyCode;

    pub(crate) const QUIT: KeyCode = KeyCode::Char('q');
    pub(crate) const PAUSE: KeyCode = KeyCode::Char('p');
    pub(crate) const MUTE: KeyCode = KeyCode::Char('m');
    pub(crate) const VOLUME_UP: KeyCode = KeyCode::Char('+');
    pub(crate) const VOLUME_DOWN: KeyCode = KeyCode::Char('-');
    /// Opens the appearance picker.
    pub(crate) const APPEARANCE: KeyCode = KeyCode::Char('t');
    pub(crate) const HELP: KeyCode = KeyCode::Char('?');
    /// Opens the version popup.
    pub(crate) const VERSION: KeyCode = KeyCode::Char('v');
    pub(crate) const DASHBOARD: KeyCode = KeyCode::Tab;
    /// The dashboard's: focuses the selected agent's terminal.
    pub(crate) const DASHBOARD_FOCUS: KeyCode = KeyCode::Char('f');
    /// The dashboard's: folds or unfolds every root.
    pub(crate) const DASHBOARD_FOLD_ALL: KeyCode = KeyCode::Char('z');
    pub(crate) const SOURCES: KeyCode = KeyCode::Char('s');
    /// The Sources panel's: connects or disconnects the selected source.
    pub(crate) const SOURCE_TOGGLE: KeyCode = KeyCode::Char('t');
    /// The Sources panel's answers to a disconnect's question.
    pub(crate) const CONFIRM: KeyCode = KeyCode::Char('y');
    pub(crate) const DECLINE: KeyCode = KeyCode::Char('n');
    /// The onboarding's: ticks or unticks the selected source.
    pub(crate) const ONBOARDING_TOGGLE: KeyCode = KeyCode::Char(' ');
    /// The terminal's own: flips the half-blocks' walkable debug layer.
    pub(crate) const WALKABLE: KeyCode = KeyCode::Char('w');
}

/// The look after `look` in [`LookKind::ALL`](pixtuoid_scene::look::LookKind::ALL),
/// or before it, round the ends: the picker's row cycles.
fn step_look(
    look: pixtuoid_scene::look::LookKind,
    forward: bool,
) -> pixtuoid_scene::look::LookKind {
    use enum_map::Enum;
    use pixtuoid_scene::look::LookKind;
    let n = LookKind::ALL.len();
    LookKind::from_usize((look.into_usize() + if forward { 1 } else { n - 1 }) % n)
}

/// [`Host::set_look`], a failure logged: the frame after repaints anyway.
fn set_look<H: Host>(host: &mut H, look: pixtuoid_scene::look::LookKind) {
    if let Err(e) = host.set_look(look) {
        tracing::warn!(error = ?e, "look switch failed");
    }
}

/// Modal precedence, highest first, is the body's early-return order.
pub(crate) fn dispatch_key(
    code: KeyCode,
    mods: KeyModifiers,
    modal: ModalState,
    floor: FloorNav,
) -> KeyAction {
    // Above every modal: a garbled screen is no less garbled under one.
    if (code, mods) == (KeyCode::Char('l'), KeyModifiers::CONTROL) {
        return KeyAction::Redraw;
    }
    if modal.onboarding_busy {
        return match (code, mods) {
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            (KeyCode::Esc, _) => KeyAction::OnboardingSkip,
            _ => KeyAction::None,
        };
    }
    if modal.onboarding_open {
        return match (code, mods) {
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => KeyAction::OnboardingUp,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => KeyAction::OnboardingDown,
            (keys::ONBOARDING_TOGGLE, _) => KeyAction::OnboardingToggle,
            (KeyCode::Enter, _) => KeyAction::OnboardingConfirm,
            (KeyCode::Esc, _) => KeyAction::OnboardingSkip,
            _ => KeyAction::None,
        };
    }
    if modal.help_open {
        return match (code, mods) {
            (KeyCode::Enter, _) | (KeyCode::Esc, _) | (keys::HELP, _) => KeyAction::CloseHelp,
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            _ => KeyAction::None,
        };
    }
    if modal.version_popup_open {
        return match (code, mods) {
            (KeyCode::Enter, _) | (KeyCode::Esc, _) => KeyAction::DismissVersionPopup,
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            _ => KeyAction::None,
        };
    }
    if modal.connection_open {
        if modal.connection_confirm {
            return match (code, mods) {
                _ if is_quit_chord(code, mods) => KeyAction::Quit,
                (keys::CONFIRM, _) => KeyAction::ConnectionConfirm,
                (keys::DECLINE, _) | (KeyCode::Esc, _) => KeyAction::ConnectionCancelConfirm,
                _ => KeyAction::None,
            };
        }
        return match (code, mods) {
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            (KeyCode::Esc, _) | (keys::SOURCES, _) => KeyAction::ConnectionClose,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => KeyAction::ConnectionUp,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => KeyAction::ConnectionDown,
            (keys::SOURCE_TOGGLE, _) => KeyAction::ConnectionToggle,
            _ => KeyAction::None,
        };
    }
    if modal.dashboard_open {
        return match (code, mods) {
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            (KeyCode::Esc, _) | (keys::DASHBOARD, _) => KeyAction::DashboardClose,
            (KeyCode::Enter, _) => KeyAction::DashboardJump,
            (keys::DASHBOARD_FOCUS, _) => KeyAction::DashboardFocus,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => KeyAction::DashboardUp,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => KeyAction::DashboardDown,
            (KeyCode::Left, _) | (KeyCode::Char('h'), _) => KeyAction::DashboardFoldLeft,
            (KeyCode::Right, _) | (KeyCode::Char('l'), _) => KeyAction::DashboardFoldRight,
            (keys::DASHBOARD_FOLD_ALL, _) => KeyAction::DashboardFoldAll,
            _ => KeyAction::None,
        };
    }
    if let Some(open) = modal.appearance {
        let idx = open.theme;
        let look = |forward| {
            open.look.map_or(KeyAction::None, |l| {
                KeyAction::LookPreview(step_look(l, forward))
            })
        };
        return match (code, mods) {
            // Safe to quit mid-preview: only a commit saves the appearance.
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            (KeyCode::Up | KeyCode::Char('k'), _) => KeyAction::ThemePreview(idx.saturating_sub(1)),
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                KeyAction::ThemePreview((idx + 1).min(theme::ALL_THEMES.len().saturating_sub(1)))
            }
            (KeyCode::Left | KeyCode::Char('h'), _) => look(false),
            (KeyCode::Right | KeyCode::Char('l'), _) => look(true),
            (KeyCode::Enter, _) => KeyAction::AppearanceCommit,
            (KeyCode::Esc, _) => KeyAction::AppearanceCancel,
            _ => KeyAction::None,
        };
    }
    if is_quit_chord(code, mods) || code == KeyCode::Esc {
        return KeyAction::Quit;
    }
    match code {
        keys::PAUSE => KeyAction::TogglePause,
        keys::MUTE => KeyAction::ToggleAudioMute,
        // `=` and `_` are the same keys without and with Shift.
        keys::VOLUME_UP | KeyCode::Char('=') => KeyAction::AdjustVolume(true),
        keys::VOLUME_DOWN | KeyCode::Char('_') => KeyAction::AdjustVolume(false),
        keys::APPEARANCE => KeyAction::OpenAppearance,
        keys::HELP => KeyAction::ToggleHelp,
        keys::VERSION => KeyAction::OpenVersionPopup,
        keys::DASHBOARD => KeyAction::ToggleDashboard,
        keys::SOURCES => KeyAction::ToggleConnection,
        KeyCode::PageUp | KeyCode::Up | KeyCode::Char('k') => {
            if floor.current_floor + 1 < floor.n_floors && !floor.in_transition {
                KeyAction::NavigateFloor(floor.current_floor + 1)
            } else {
                KeyAction::None
            }
        }
        KeyCode::PageDown | KeyCode::Down | KeyCode::Char('j') => {
            if floor.current_floor > 0 && !floor.in_transition {
                KeyAction::NavigateFloor(floor.current_floor - 1)
            } else {
                KeyAction::None
            }
        }
        _ => KeyAction::None,
    }
}

/// Everything an applied [`KeyAction`] may touch — a parameter object, so the arm list
/// takes one argument.
pub(crate) struct KeyCtx<'a, H: Host> {
    pub(crate) ui: &'a mut ui_state::UiState,
    pub(crate) host: &'a mut H,
    pub(crate) audio_ctl: &'a mut crate::audio::AudioController,
    pub(crate) config_path: &'a std::path::Path,
    pub(crate) connected: &'a crate::runtime::ConnectedSources,
    pub(crate) snapshot: &'a pixtuoid_core::state::SceneState,
    pub(crate) now: SystemTime,
    /// Injected because the real `crate::audio::respawn` opens an output device, so a
    /// test firing an audio arm would grab the machine's sound hardware.
    pub(crate) respawn: fn(&crate::audio::AudioHandle, f32),
    pub(crate) saves: &'a Saves,
    /// Injected because the real `crate::focus::focus_slot` raises a terminal,
    /// so a test's focus key would drive the developer's desktop.
    pub(crate) focus: fn(&AgentSlot),
    /// Where a started job sends its [`JobDone`] back to the loop.
    pub(crate) job_tx: &'a LandedTx,
    /// Injected because the real [`run_job`] runs host CLIs and rewrites
    /// other CLIs' configs, so a test's toggle would reach the developer's own.
    pub(crate) run_job: fn(JobInput) -> JobDone,
}

/// Apply one decoded [`KeyAction`], returning whether it asked to QUIT — the single piece
/// of control flow the caller's event loop keeps. Paired with [`dispatch_key`], which
/// decodes; splitting them is what makes the arms reachable from a test at all, since
/// a painter's event loop needs a real terminal or window.
pub(crate) fn apply_key_action<H: Host>(action: KeyAction, cx: &mut KeyCtx<'_, H>) -> bool {
    match action {
        KeyAction::None => {}
        KeyAction::Quit => return true,
        KeyAction::TogglePause => {
            cx.ui.toggle_pause();
            // Unpause restores the user's own mute rather than clobbering it.
            cx.audio_ctl.set_paused(cx.ui.paused());
        }
        KeyAction::ToggleHelp => cx.ui.toggle_help(),
        KeyAction::CloseHelp => cx.ui.close_help(),
        KeyAction::DismissVersionPopup => cx.ui.dismiss_version_popup(),
        KeyAction::OpenVersionPopup => cx.ui.open_version_popup(&cx.connected.snapshot()),
        KeyAction::OpenAppearance => cx.ui.open_appearance(),
        KeyAction::ThemePreview(i) => {
            cx.ui.preview_theme(i);
            cx.host.set_theme(theme::ALL_THEMES[i]);
        }
        KeyAction::LookPreview(look) => {
            cx.ui.preview_look(look);
            set_look(cx.host, look);
        }
        KeyAction::AppearanceCommit => {
            if let Some(committed) = cx.ui.commit_appearance() {
                let name = theme::ALL_THEMES[committed.theme].name;
                cx.saves.save(move |path| {
                    let save = crate::config::save_appearance(path, name, committed.look);
                    Some(crate::config::SaveOutcome::of(
                        crate::config::Setting::Appearance,
                        save,
                    ))
                });
            }
        }
        KeyAction::AppearanceCancel => {
            let saved = cx.ui.cancel_appearance();
            cx.host.set_theme(theme::ALL_THEMES[saved.theme]);
            if let Some(look) = saved.look {
                set_look(cx.host, look);
            }
        }
        KeyAction::NavigateFloor(target) => {
            cx.host.navigate_floor(target, cx.now);
        }
        KeyAction::ToggleAudioMute => cx.audio_ctl.apply(
            crate::audio::AudioAction::ToggleMute,
            cx.ui.paused(),
            Instant::now(),
            cx.respawn,
        ),
        KeyAction::AdjustVolume(up) => cx.audio_ctl.apply(
            crate::audio::AudioAction::Volume(up),
            cx.ui.paused(),
            Instant::now(),
            cx.respawn,
        ),
        KeyAction::ToggleDashboard => cx.ui.toggle_dashboard(cx.snapshot),
        KeyAction::DashboardClose => cx.ui.close_dashboard(),
        KeyAction::DashboardUp => cx.ui.dashboard_move(cx.snapshot, -1),
        KeyAction::DashboardDown => cx.ui.dashboard_move(cx.snapshot, 1),
        KeyAction::DashboardFoldLeft => cx.ui.dashboard_fold_left(cx.snapshot),
        KeyAction::DashboardFoldRight => cx.ui.dashboard_fold_right(cx.snapshot),
        KeyAction::DashboardFoldAll => cx.ui.dashboard_fold_all(cx.snapshot),
        KeyAction::DashboardJump => {
            if let Some(floor) = cx.ui.dashboard_jump(cx.snapshot) {
                cx.host.navigate_floor(floor, cx.now);
            }
        }
        KeyAction::DashboardFocus => {
            if let Some(slot) = cx
                .ui
                .dashboard_focus()
                .and_then(|id| cx.snapshot.agents.get(&id))
            {
                (cx.focus)(slot);
            }
        }
        KeyAction::ToggleConnection => {
            if cx.ui.connection.open {
                cx.ui.close_connection();
            } else {
                // On the rows it last showed, else placeholders, until the
                // refresh brings what the files say: its reads can stall.
                let rows = match std::mem::take(&mut cx.ui.connection.rows) {
                    rows if rows.is_empty() => {
                        connection::placeholder_rows(&cx.connected.snapshot())
                    }
                    rows => rows,
                };
                cx.ui.open_connection(rows);
                start_job(cx, PanelJob::Refresh);
            }
        }
        KeyAction::ConnectionUp => cx.ui.connection_move(-1),
        KeyAction::ConnectionDown => cx.ui.connection_move(1),
        KeyAction::ConnectionToggle => {
            let action = cx
                .ui
                .connection
                .rows
                .get(cx.ui.connection.selected)
                .map(|r| {
                    (
                        r.state,
                        r.source_id,
                        r.display_name,
                        connection::no_action_hint(r),
                    )
                });
            if let Some((state, source_id, name, hint)) = action {
                use crate::sources::{ToggleIntent, toggle_intent};
                match toggle_intent(state) {
                    // Refused before its question, as `start_job` refuses a
                    // connect: an answer would only meet the same refusal.
                    ToggleIntent::Disconnect if let Some(running) = &cx.ui.running_job => {
                        cx.ui.connection.last_result = Some(running.busy_line());
                    }
                    ToggleIntent::Disconnect => {
                        cx.ui.connection.confirm = Some(cx.ui.connection.selected);
                    }
                    ToggleIntent::Connect => start_job(
                        cx,
                        PanelJob::Toggle {
                            source_id,
                            name,
                            on: true,
                        },
                    ),
                    // It starts nothing, so a job in flight refuses it nothing.
                    ToggleIntent::Hint => {
                        cx.ui.connection.last_result = Some(hint);
                    }
                }
            }
        }
        KeyAction::ConnectionConfirm => {
            let action = cx
                .ui
                .connection
                .confirm
                .take()
                .and_then(|idx| cx.ui.connection.rows.get(idx))
                .map(|r| (r.source_id, r.display_name));
            if let Some((source_id, name)) = action {
                start_job(
                    cx,
                    PanelJob::Toggle {
                        source_id,
                        name,
                        on: false,
                    },
                );
            }
        }
        KeyAction::ConnectionCancelConfirm => cx.ui.cancel_connection_confirm(),
        KeyAction::ConnectionClose => cx.ui.close_connection(),
        KeyAction::OnboardingUp => cx.ui.onboarding_ui.move_up(),
        KeyAction::OnboardingDown => cx.ui.onboarding_ui.move_down(),
        KeyAction::OnboardingToggle => cx.ui.onboarding_ui.toggle_selected(),
        KeyAction::OnboardingConfirm => {
            let checked = cx.ui.onboarding_ui.checked();
            if checked.is_empty() {
                cx.ui.close_onboarding();
            } else {
                start_job(cx, PanelJob::Onboarding(checked));
            }
        }
        KeyAction::OnboardingSkip => cx.ui.close_onboarding(),
        KeyAction::Redraw => {
            if let Err(e) = cx.host.redraw() {
                tracing::warn!(error = %e, "redraw failed");
            }
        }
    }
    false
}

#[cfg(test)]
mod dispatch_tests {
    use super::{
        FloorNav, KeyAction, ModalState, connection, dispatch_key, set_connected, ui_state,
    };
    use crossterm::event::{KeyCode, KeyModifiers};

    const NONE: KeyModifiers = KeyModifiers::NONE;
    const CTRL: KeyModifiers = KeyModifiers::CONTROL;

    /// The site and the user docs write the app's keys as text no build
    /// reaches, so each copy is pinned to its [`keys`](super::keys) const here
    /// (the README's row is generated from `site/src/features.json`).
    #[test]
    fn the_site_and_the_docs_name_the_apps_keys() {
        use super::keys::{APPEARANCE, DASHBOARD, HELP, MUTE, SOURCES, VOLUME_DOWN, VOLUME_UP};
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        // crates.io-packaged test runs don't ship the repo's site/ or docs/.
        if !std::path::Path::new(&format!("{root}/site")).is_dir() {
            return;
        }
        let read = |file: &str| std::fs::read_to_string(format!("{root}/{file}")).expect(file);
        for (file, says) in [
            (
                "site/src/features.json",
                format!("`{MUTE}` mutes, `{VOLUME_UP}`/`{VOLUME_DOWN}` sets the volume"),
            ),
            (
                "site/src/features.json",
                format!("Press {DASHBOARD} to see every agent"),
            ),
            (
                "site/src/showcase.json",
                format!("{DASHBOARD} ⇥ agent tree"),
            ),
            ("docs/CONFIGURATION.md", format!("Press `{MUTE}`")),
            (
                "docs/CONFIGURATION.md",
                format!("`{VOLUME_UP}`/`{VOLUME_DOWN}` nudge it live"),
            ),
            (
                "docs/CONFIGURATION.md",
                format!("`{VOLUME_UP}` from muted also unmutes"),
            ),
            (
                "docs/CONFIGURATION.md",
                format!("Press `{APPEARANCE}` in the TUI"),
            ),
            (
                "docs/CONFIGURATION.md",
                format!("cutaway `{APPEARANCE}` can switch"),
            ),
            (
                "docs/CONFIGURATION.md",
                format!("**Sources panel** (`{SOURCES}`)"),
            ),
            (
                "README.md",
                format!("Press <kbd>{SOURCES}</kbd> and connect"),
            ),
            (
                "README.md",
                format!("<kbd>{DASHBOARD}</kbd> agent dashboard · <kbd>{APPEARANCE}</kbd> themes"),
            ),
            ("README.md", format!("<kbd>{MUTE}</kbd> sound")),
            ("README.md", format!("<kbd>{HELP}</kbd> every other key")),
            (
                "README.md",
                format!("Press <kbd>{APPEARANCE}</kbd> to preview every theme live"),
            ),
            (
                "site/src/components/HowItWorks.astro",
                format!("press {SOURCES} → connect"),
            ),
            (
                "site/src/layouts/Base.astro",
                format!("e.key !== '{APPEARANCE}'"),
            ),
        ] {
            assert!(read(file).contains(&says), "{file} no longer says {says:?}");
        }
    }

    fn modal() -> ModalState {
        ModalState::closed()
    }

    fn nav() -> FloorNav {
        FloorNav {
            n_floors: 3,
            current_floor: 1,
            in_transition: false,
        }
    }

    /// A busy onboarding takes only a quit, or Esc, which hides it while its
    /// connects run.
    #[test]
    fn a_busy_onboarding_takes_only_a_quit_or_esc() {
        let busy = ModalState {
            onboarding_open: true,
            onboarding_busy: true,
            ..modal()
        };
        for (code, mods, want) in [
            (KeyCode::Char('q'), NONE, KeyAction::Quit),
            (KeyCode::Char('c'), CTRL, KeyAction::Quit),
            (KeyCode::Esc, NONE, KeyAction::OnboardingSkip),
            (KeyCode::Enter, NONE, KeyAction::None),
            (KeyCode::Char(' '), NONE, KeyAction::None),
            (KeyCode::Down, NONE, KeyAction::None),
        ] {
            assert_eq!(dispatch_key(code, mods, busy, nav()), want, "{code:?}");
        }
    }

    #[test]
    fn normal_quit_pause_picker_help() {
        assert_eq!(
            dispatch_key(KeyCode::Char('q'), NONE, modal(), nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), CTRL, modal(), nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Esc, NONE, modal(), nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('p'), NONE, modal(), nav()),
            KeyAction::TogglePause
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('m'), NONE, modal(), nav()),
            KeyAction::ToggleAudioMute
        );
        for up in ['+', '='] {
            assert_eq!(
                dispatch_key(KeyCode::Char(up), NONE, modal(), nav()),
                KeyAction::AdjustVolume(true)
            );
        }
        for down in ['-', '_'] {
            assert_eq!(
                dispatch_key(KeyCode::Char(down), NONE, modal(), nav()),
                KeyAction::AdjustVolume(false)
            );
        }
        assert_eq!(
            dispatch_key(KeyCode::Char('t'), NONE, modal(), nav()),
            KeyAction::OpenAppearance
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('?'), NONE, modal(), nav()),
            KeyAction::ToggleHelp
        );
        // The classic's walkable debug layer is the terminal's alone: the
        // shared dispatch, the window's too, never reads its key.
        assert_eq!(
            dispatch_key(KeyCode::Char('w'), NONE, modal(), nav()),
            KeyAction::None
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('x'), NONE, modal(), nav()),
            KeyAction::None
        );
    }

    #[test]
    fn floor_nav_guards() {
        for code in [KeyCode::PageUp, KeyCode::Up, KeyCode::Char('k')] {
            assert_eq!(
                dispatch_key(code, NONE, modal(), nav()),
                KeyAction::NavigateFloor(2)
            );
        }
        for code in [KeyCode::PageDown, KeyCode::Down, KeyCode::Char('j')] {
            assert_eq!(
                dispatch_key(code, NONE, modal(), nav()),
                KeyAction::NavigateFloor(0)
            );
        }
        let top = FloorNav {
            current_floor: 2,
            ..nav()
        };
        assert_eq!(
            dispatch_key(KeyCode::Up, NONE, modal(), top),
            KeyAction::None
        );
        let bottom = FloorNav {
            current_floor: 0,
            ..nav()
        };
        assert_eq!(
            dispatch_key(KeyCode::Down, NONE, modal(), bottom),
            KeyAction::None
        );
        let mid_trans = FloorNav {
            in_transition: true,
            ..nav()
        };
        assert_eq!(
            dispatch_key(KeyCode::Up, NONE, modal(), mid_trans),
            KeyAction::None
        );
        assert_eq!(
            dispatch_key(KeyCode::Down, NONE, modal(), mid_trans),
            KeyAction::None
        );
    }

    #[test]
    fn help_overlay_has_priority_and_dismisses() {
        let c = ModalState {
            help_open: true,
            version_popup_open: true,
            appearance: Some(theme_at(2)),
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Enter, NONE, c, nav()),
            KeyAction::CloseHelp
        );
        assert_eq!(
            dispatch_key(KeyCode::Esc, NONE, c, nav()),
            KeyAction::CloseHelp
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('?'), NONE, c, nav()),
            KeyAction::CloseHelp
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('q'), NONE, c, nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), CTRL, c, nav()),
            KeyAction::Quit
        );
        assert_eq!(dispatch_key(KeyCode::Up, NONE, c, nav()), KeyAction::None);
    }

    #[test]
    fn onboarding_is_top_precedence_and_maps_its_keys() {
        let on = ModalState {
            onboarding_open: true,
            help_open: true,
            version_popup_open: true,
            connection_open: true,
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Up, NONE, on, nav()),
            KeyAction::OnboardingUp
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('k'), NONE, on, nav()),
            KeyAction::OnboardingUp
        );
        assert_eq!(
            dispatch_key(KeyCode::Down, NONE, on, nav()),
            KeyAction::OnboardingDown
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('j'), NONE, on, nav()),
            KeyAction::OnboardingDown
        );
        assert_eq!(
            dispatch_key(KeyCode::Char(' '), NONE, on, nav()),
            KeyAction::OnboardingToggle
        );
        assert_eq!(
            dispatch_key(KeyCode::Enter, NONE, on, nav()),
            KeyAction::OnboardingConfirm
        );
        assert_eq!(
            dispatch_key(KeyCode::Esc, NONE, on, nav()),
            KeyAction::OnboardingSkip
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), CTRL, on, nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('s'), NONE, on, nav()),
            KeyAction::None
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('?'), NONE, on, nav()),
            KeyAction::None
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('t'), NONE, on, nav()),
            KeyAction::None
        );
    }

    #[test]
    fn version_popup_enter_and_esc_dismiss() {
        let c = ModalState {
            version_popup_open: true,
            ..modal()
        };
        for dismiss in [KeyCode::Enter, KeyCode::Esc] {
            assert_eq!(
                dispatch_key(dismiss, NONE, c, nav()),
                KeyAction::DismissVersionPopup
            );
        }
        assert_eq!(
            dispatch_key(KeyCode::Char('q'), NONE, c, nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), CTRL, c, nav()),
            KeyAction::Quit
        );
        assert_eq!(dispatch_key(KeyCode::Up, NONE, c, nav()), KeyAction::None);
    }

    /// [`keys::VERSION`](crate::panels::keys::VERSION) reopens the popup from the
    /// office only: every panel's own key map leaves it unbound, so it never
    /// acts behind an open panel.
    #[test]
    fn v_reopens_the_version_popup_from_the_office_only() {
        let v = KeyCode::Char('v');
        assert_eq!(
            dispatch_key(v, NONE, modal(), nav()),
            KeyAction::OpenVersionPopup
        );
        for open in [
            ModalState {
                onboarding_open: true,
                ..modal()
            },
            ModalState {
                help_open: true,
                ..modal()
            },
            ModalState {
                version_popup_open: true,
                ..modal()
            },
            ModalState {
                connection_open: true,
                ..modal()
            },
            ModalState {
                connection_open: true,
                connection_confirm: true,
                ..modal()
            },
            ModalState {
                dashboard_open: true,
                ..modal()
            },
            ModalState {
                appearance: Some(theme_at(0)),
                ..modal()
            },
        ] {
            assert_eq!(dispatch_key(v, NONE, open, nav()), KeyAction::None);
        }
    }

    /// The popup is DISMISS-ONLY: its notices are cut to fit, never scrolled.
    /// A scroll key bound here would promise rows that do not exist.
    #[test]
    fn the_version_popup_binds_no_scroll_key() {
        let c = ModalState {
            version_popup_open: true,
            ..modal()
        };
        for code in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::PageDown,
            KeyCode::PageUp,
        ] {
            assert_eq!(
                dispatch_key(code, NONE, c, nav()),
                KeyAction::None,
                "{code:?} must stay unbound while the version popup is up"
            );
        }
    }

    #[test]
    fn theme_picker_preview_commit_cancel_and_clamps() {
        let c = ModalState {
            appearance: Some(theme_at(2)),
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Up, NONE, c, nav()),
            KeyAction::ThemePreview(1)
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('k'), NONE, c, nav()),
            KeyAction::ThemePreview(1)
        );
        assert_eq!(
            dispatch_key(KeyCode::Down, NONE, c, nav()),
            KeyAction::ThemePreview(3)
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('j'), NONE, c, nav()),
            KeyAction::ThemePreview(3)
        );
        assert_eq!(
            dispatch_key(KeyCode::Enter, NONE, c, nav()),
            KeyAction::AppearanceCommit
        );
        assert_eq!(
            dispatch_key(KeyCode::Esc, NONE, c, nav()),
            KeyAction::AppearanceCancel
        );
        for side in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('h'),
            KeyCode::Char('l'),
        ] {
            assert_eq!(
                dispatch_key(side, NONE, c, nav()),
                KeyAction::None,
                "no look row: {side:?}"
            );
        }
        assert_eq!(
            dispatch_key(KeyCode::Char('q'), NONE, c, nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), CTRL, c, nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('p'), NONE, c, nav()),
            KeyAction::None
        );

        let lo = ModalState {
            appearance: Some(theme_at(0)),
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Up, NONE, lo, nav()),
            KeyAction::ThemePreview(0)
        );
        let hi = ModalState {
            appearance: Some(theme_at(pixtuoid_scene::theme::ALL_THEMES.len() - 1)),
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Down, NONE, hi, nav()),
            KeyAction::ThemePreview(pixtuoid_scene::theme::ALL_THEMES.len() - 1)
        );
    }

    /// The theme picker at row `theme`, with no look row.
    fn theme_at(theme: usize) -> ui_state::AppearancePick {
        ui_state::AppearancePick { theme, look: None }
    }

    /// With a look row, ←→/hl cycle the look round its ends, and ↑↓/jk stay
    /// the themes'.
    #[test]
    fn the_look_row_cycles_on_its_own_axis() {
        use pixtuoid_scene::look::LookKind;
        for look in LookKind::ALL.iter().copied() {
            let c = ModalState {
                appearance: Some(ui_state::AppearancePick {
                    theme: 2,
                    look: Some(look),
                }),
                ..modal()
            };
            let other = LookKind::ALL
                .iter()
                .copied()
                .find(|&l| l != look)
                .expect("two looks");
            for side in [
                KeyCode::Left,
                KeyCode::Right,
                KeyCode::Char('h'),
                KeyCode::Char('l'),
            ] {
                assert_eq!(
                    dispatch_key(side, NONE, c, nav()),
                    KeyAction::LookPreview(other),
                    "{look:?} {side:?}"
                );
            }
            assert_eq!(
                dispatch_key(KeyCode::Down, NONE, c, nav()),
                KeyAction::ThemePreview(3)
            );
        }
    }

    #[test]
    fn ctrl_l_redraws_under_any_modal() {
        let dashboard = ModalState {
            dashboard_open: true,
            ..modal()
        };
        let onboarding = ModalState {
            onboarding_open: true,
            ..modal()
        };
        for m in [modal(), dashboard, onboarding] {
            assert_eq!(
                dispatch_key(KeyCode::Char('l'), CTRL, m, nav()),
                KeyAction::Redraw
            );
        }
        assert_eq!(
            dispatch_key(KeyCode::Char('l'), NONE, dashboard, nav()),
            KeyAction::DashboardFoldRight
        );
    }

    #[test]
    fn tab_toggles_dashboard_from_normal_scene() {
        assert_eq!(
            dispatch_key(KeyCode::Tab, NONE, modal(), nav()),
            KeyAction::ToggleDashboard
        );
    }

    #[test]
    fn dashboard_tier_maps_nav_fold_jump_close() {
        let d = ModalState {
            dashboard_open: true,
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Up, NONE, d, nav()),
            KeyAction::DashboardUp
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('k'), NONE, d, nav()),
            KeyAction::DashboardUp
        );
        assert_eq!(
            dispatch_key(KeyCode::Down, NONE, d, nav()),
            KeyAction::DashboardDown
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('j'), NONE, d, nav()),
            KeyAction::DashboardDown
        );
        assert_eq!(
            dispatch_key(KeyCode::Left, NONE, d, nav()),
            KeyAction::DashboardFoldLeft
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('h'), NONE, d, nav()),
            KeyAction::DashboardFoldLeft
        );
        assert_eq!(
            dispatch_key(KeyCode::Right, NONE, d, nav()),
            KeyAction::DashboardFoldRight
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('l'), NONE, d, nav()),
            KeyAction::DashboardFoldRight
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('z'), NONE, d, nav()),
            KeyAction::DashboardFoldAll
        );
        assert_eq!(
            dispatch_key(KeyCode::Enter, NONE, d, nav()),
            KeyAction::DashboardJump
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('f'), NONE, d, nav()),
            KeyAction::DashboardFocus,
            "f focuses the selected agent's terminal"
        );
        assert_eq!(
            dispatch_key(KeyCode::Esc, NONE, d, nav()),
            KeyAction::DashboardClose
        );
        assert_eq!(
            dispatch_key(KeyCode::Tab, NONE, d, nav()),
            KeyAction::DashboardClose
        );
    }

    #[test]
    fn dashboard_modal_passes_quit_chord_but_swallows_other_keys() {
        let d = ModalState {
            dashboard_open: true,
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Char('q'), NONE, d, nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), CTRL, d, nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('p'), NONE, d, nav()),
            KeyAction::None,
            "modal swallows pause"
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('t'), NONE, d, nav()),
            KeyAction::None,
            "modal swallows theme picker"
        );
    }

    #[test]
    fn tab_swallowed_while_other_overlays_open() {
        let h = ModalState {
            help_open: true,
            ..modal()
        };
        assert_eq!(dispatch_key(KeyCode::Tab, NONE, h, nav()), KeyAction::None);
        let v = ModalState {
            version_popup_open: true,
            ..modal()
        };
        assert_eq!(dispatch_key(KeyCode::Tab, NONE, v, nav()), KeyAction::None);
        let p = ModalState {
            appearance: Some(theme_at(0)),
            ..modal()
        };
        assert_eq!(dispatch_key(KeyCode::Tab, NONE, p, nav()), KeyAction::None);
    }

    #[test]
    fn s_opens_sources_panel_from_normal_scene() {
        assert_eq!(
            dispatch_key(KeyCode::Char('s'), NONE, modal(), nav()),
            KeyAction::ToggleConnection
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), NONE, modal(), nav()),
            KeyAction::None
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), CTRL, modal(), nav()),
            KeyAction::Quit
        );
    }

    #[test]
    fn connection_tier_maps_nav_toggle_close() {
        let s = ModalState {
            connection_open: true,
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Up, NONE, s, nav()),
            KeyAction::ConnectionUp
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('k'), NONE, s, nav()),
            KeyAction::ConnectionUp
        );
        assert_eq!(
            dispatch_key(KeyCode::Down, NONE, s, nav()),
            KeyAction::ConnectionDown
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('j'), NONE, s, nav()),
            KeyAction::ConnectionDown
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('t'), NONE, s, nav()),
            KeyAction::ConnectionToggle
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('i'), NONE, s, nav()),
            KeyAction::None
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('u'), NONE, s, nav()),
            KeyAction::None
        );
        assert_eq!(
            dispatch_key(KeyCode::Enter, NONE, s, nav()),
            KeyAction::None
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('s'), NONE, s, nav()),
            KeyAction::ConnectionClose
        );
        assert_eq!(
            dispatch_key(KeyCode::Esc, NONE, s, nav()),
            KeyAction::ConnectionClose
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('q'), NONE, s, nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), CTRL, s, nav()),
            KeyAction::Quit
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('y'), NONE, s, nav()),
            KeyAction::None
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('n'), NONE, s, nav()),
            KeyAction::None
        );
    }

    #[test]
    fn connection_armed_tier_maps_yn_and_swallows_nav() {
        let s = ModalState {
            connection_open: true,
            connection_confirm: true,
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Char('y'), NONE, s, nav()),
            KeyAction::ConnectionConfirm
        );
        assert_eq!(
            dispatch_key(KeyCode::Char('n'), NONE, s, nav()),
            KeyAction::ConnectionCancelConfirm
        );
        assert_eq!(
            dispatch_key(KeyCode::Esc, NONE, s, nav()),
            KeyAction::ConnectionCancelConfirm
        );
        for k in [
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Char('i'),
            KeyCode::Char('u'),
        ] {
            assert_eq!(dispatch_key(k, NONE, s, nav()), KeyAction::None);
        }
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), CTRL, s, nav()),
            KeyAction::Quit
        );
    }

    #[test]
    fn connection_precedence_help_version_win_and_connection_swallows_tab() {
        let h = ModalState {
            help_open: true,
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), NONE, h, nav()),
            KeyAction::None
        );
        let v = ModalState {
            version_popup_open: true,
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Char('c'), NONE, v, nav()),
            KeyAction::None
        );
        let s = ModalState {
            connection_open: true,
            ..modal()
        };
        assert_eq!(dispatch_key(KeyCode::Tab, NONE, s, nav()), KeyAction::None);
    }

    /// The panel and a paired device show one sentence for a change.
    #[test]
    fn the_panels_line_for_a_change_is_a_devices() {
        use pixtuoid_core::remote::{Changed, Outcome};
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.toml");
        let connected = crate::runtime::ConnectedSources::default();
        for (on, outcome) in [(true, Outcome::Connected), (false, Outcome::Disconnected)] {
            let device = Changed::new("copilot", on, outcome, false, None, None);
            let panel = set_connected(&cfg, &connected, "copilot", "Copilot CLI", on).line;
            assert_eq!(panel, device.line("Copilot CLI"));
        }
    }

    #[test]
    fn connect_source_persists_then_flips_the_gate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.toml");
        let connected = crate::runtime::ConnectedSources::default();

        let res = set_connected(&cfg, &connected, "copilot", "Copilot CLI", true).line;
        assert!(res.contains("connected"), "result: {res}");
        assert!(connected.is_connected("copilot"), "gate opened");
        let written = std::fs::read_to_string(&cfg).unwrap();
        assert!(
            written.contains("copilot") && written.contains("true"),
            "the flag was persisted: {written}"
        );
    }

    #[test]
    fn disconnect_source_persists_then_closes_the_gate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.toml");
        let connected =
            crate::runtime::ConnectedSources::new(std::iter::once("copilot".to_string()).collect());

        let res = set_connected(&cfg, &connected, "copilot", "Copilot CLI", false).line;
        assert!(res.contains("disconnected"), "result: {res}");
        assert!(!connected.is_connected("copilot"), "gate closed");
        let written = std::fs::read_to_string(&cfg).unwrap();
        assert!(
            !written.contains("copilot"),
            "the flag was dropped: {written}"
        );
    }

    #[test]
    fn connect_source_aborts_without_flipping_the_gate_when_persist_fails() {
        let tmp = tempfile::TempDir::new().unwrap();
        // A regular file used as a directory component makes the config write's
        // create-parent-dir fail.
        let blocker = tmp.path().join("not-a-dir");
        std::fs::write(&blocker, "x").unwrap();
        let cfg = blocker.join("config.toml");
        let connected = crate::runtime::ConnectedSources::default();

        let res = set_connected(&cfg, &connected, "copilot", "Copilot CLI", true).line;
        assert!(res.contains("failed"), "must report the failure: {res}");
        assert!(
            !connected.is_connected("copilot"),
            "a failed persist must NOT open the gate (else restart re-evicts)"
        );
    }

    /// The gate follows the fact after a FAILED change, not the change's outcome: a
    /// disconnect that could not write leaves the source connected, and so the gate.
    #[test]
    fn a_failed_disconnect_leaves_the_gate_on_the_fact() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.toml");
        std::fs::write(&cfg, "[sources]\ncopilot = true\n").unwrap();
        let connected =
            crate::runtime::ConnectedSources::new(std::iter::once("copilot".to_string()).collect());
        // A held lock refuses the rewrite (`lock_config` fails on contention)
        // while the flag still reads back.
        let held = crate::install::io::lock_config(&cfg).unwrap();
        let res = set_connected(&cfg, &connected, "copilot", "Copilot CLI", false).line;
        drop(held);
        assert!(res.contains("disconnect failed"), "result: {res}");
        assert!(
            connected.is_connected("copilot"),
            "still flagged on disk, so still connected"
        );
    }

    /// The user can toggle the row again, so a busy config's line says it works.
    #[test]
    fn a_busy_configs_failure_line_tells_the_user_to_retry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.toml");
        let connected = crate::runtime::ConnectedSources::default();
        let _held = crate::install::io::lock_config(&cfg).unwrap();
        let line = set_connected(&cfg, &connected, "copilot", "Copilot CLI", true).line;
        assert!(line.ends_with("; try again once it finishes"), "{line}");
    }

    /// The worker runs the job it is handed, then reads the rows from the
    /// files: a confirmed disconnect disconnects, and a refresh changes nothing.
    #[test]
    fn a_job_runs_as_handed_then_reads_the_rows() {
        use super::{JobDone, JobInput, PanelJob, run_job};
        use crate::sources::ConnState;
        use pixtuoid_core::remote::{Changed, Outcome};
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.toml");
        std::fs::write(&cfg, "[sources]\ncopilot = true\n").unwrap();
        let connected =
            crate::runtime::ConnectedSources::new(std::iter::once("copilot".to_string()).collect());
        let run = |job| {
            run_job(JobInput {
                job,
                config_path: cfg.clone(),
                connected: connected.clone(),
                log: None,
            })
        };
        // Its connection, whichever CLIs this machine has.
        let copilot_on = |done: &JobDone| {
            let row = done.rows.iter().find(|r| r.source_id == "copilot");
            row.map(|r| {
                matches!(
                    r.state,
                    ConnState::Connected | ConnState::NoCli { connected: true }
                )
            })
        };

        let refreshed = run(PanelJob::Refresh);
        assert!(refreshed.notices.is_empty() && !refreshed.onboarding);
        assert_eq!(
            refreshed.installed,
            crate::sources::detect(),
            "the CLIs as the boot's probe finds them"
        );
        assert_eq!(copilot_on(&refreshed), Some(true));
        assert!(
            refreshed.rows.iter().any(|r| r.config_path.is_some()),
            "the rows are read from the files"
        );

        let off = run(PanelJob::Toggle {
            source_id: "copilot",
            name: "Copilot CLI",
            on: false,
        });
        let gone = Changed::new("copilot", false, Outcome::Disconnected, false, None, None);
        let lines: Vec<_> = off.notices.iter().map(|n| n.line.clone()).collect();
        assert_eq!(lines, [gone.line("Copilot CLI")]);
        assert!(
            !std::fs::read_to_string(&cfg).unwrap().contains("copilot"),
            "the flag was dropped"
        );
        assert_eq!(copilot_on(&off), Some(false), "the rows are read after it");
    }

    /// A failure's reason and a success's step both reach the user, each in the
    /// panel's own words; a change with nothing to say stays quiet.
    #[test]
    fn onboarding_reports_each_failure_and_each_step() {
        use pixtuoid_core::remote::{Changed, Outcome};
        let step = crate::sources::post_install_hint("openclaw").expect("openclaw declares one");
        let reason = "settings is valid JSON but not an object";
        let outcomes = vec![
            (
                "openclaw",
                Ok(Changed::new(
                    "openclaw",
                    true,
                    Outcome::Connected,
                    false,
                    Some(step),
                    None,
                )),
            ),
            ("cursor", Err(anyhow::anyhow!(reason))),
            (
                "antigravity",
                Ok(Changed::new(
                    "antigravity",
                    true,
                    Outcome::Connected,
                    false,
                    None,
                    None,
                )),
            ),
        ];
        let name = |id| pixtuoid_core::source::display_name(id).expect("a registered source");
        let notices: Vec<_> = super::onboarding_notices(&outcomes)
            .into_iter()
            .map(|n| (n.source_id, n.line, n.failed))
            .collect();
        assert_eq!(
            notices,
            [
                (
                    "openclaw",
                    format!("\u{2713} {} connected \u{b7} {step}", name("openclaw")),
                    false
                ),
                (
                    "cursor",
                    connection::format_failure(
                        connection::SourceOp::Connect,
                        name("cursor"),
                        reason
                    ),
                    true
                ),
            ]
        );
    }

    #[test]
    fn onboarding_notices_open_the_sources_panel_on_the_failed_row() {
        let connected = crate::runtime::ConnectedSources::default();
        let mut ui = crate::panels::ui_state::UiState::new(
            pixtuoid_scene::theme::ALL_THEMES[0],
            crate::panels::welcome::WelcomeUi::from_detected(&[]),
            std::path::PathBuf::from("/tmp/sock"),
            None,
            crate::doctor::DriftSeen::default(),
        );
        assert!(!ui.modal().connection_open, "panel starts closed");

        let rows = || connection::build_rows(&connected.snapshot(), "");
        super::surface_notices(&mut ui, rows(), Vec::new());
        assert!(
            !ui.modal().connection_open,
            "a clean apply must not pop the panel"
        );
        super::surface_notices(
            &mut ui,
            rows(),
            vec![super::ChangeNotice {
                source_id: "codex",
                line: "\u{2713} Codex connected".into(),
                failed: false,
                quiet: true,
            }],
        );
        assert!(
            !ui.modal().connection_open,
            "nor a quiet success that lands on a closed panel"
        );

        super::surface_notices(
            &mut ui,
            rows(),
            vec![
                super::ChangeNotice {
                    source_id: "openclaw",
                    line: "\u{2713} OpenClaw connected \u{b7} restart it".into(),
                    failed: false,
                    quiet: false,
                },
                super::ChangeNotice {
                    source_id: "cursor",
                    line: "Cursor: connect failed \u{2014} boom".into(),
                    failed: true,
                    quiet: false,
                },
            ],
        );
        assert!(ui.modal().connection_open, "a notice opens the panel");
        assert_eq!(
            ui.connection.last_result.as_deref(),
            Some(
                "\u{2713} OpenClaw connected \u{b7} restart it  \u{b7}  Cursor: connect failed \u{2014} boom"
            ),
            "every notice rides the panel's own result line"
        );
        let selected = ui.connection.rows[ui.connection.selected].source_id;
        assert_eq!(
            selected, "cursor",
            "the panel must open ON the failed row, not the first noted one — \
             the source toggle acts on the SELECTED one"
        );
        assert_ne!(
            ui.connection.selected, 0,
            "cursor is not the first registry row, so this could not pass by default"
        );
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct NotedOffice {
    pub(crate) got: Vec<PointerEvent>,
    pub(crate) click: Option<pixtuoid_scene::hit::HitAction>,
    pub(crate) petted: Vec<pixtuoid_scene::pet::PetKind>,
}

#[cfg(test)]
impl Office for NotedOffice {
    fn press(&mut self) {
        self.got.push(PointerEvent::Press);
    }

    fn moved(&mut self) {
        self.got.push(PointerEvent::Move);
    }

    fn release(&mut self) -> Option<pixtuoid_scene::hit::HitAction> {
        self.got.push(PointerEvent::Release);
        self.click
    }

    fn pet(&mut self, kind: pixtuoid_scene::pet::PetKind) {
        self.petted.push(kind);
    }
}

/// The desktop calls a test's click or key makes, noted in place of
/// `crate::focus::focus_slot` and `crate::open_url`, and the panel job it
/// starts, held in place of [`run_job`](super::run_job). The record is
/// per thread because an injected `fn` captures nothing to note them in.
#[cfg(test)]
pub(crate) mod noted {
    use std::cell::RefCell;

    use pixtuoid_core::{AgentId, AgentSlot};

    thread_local! {
        static FOCUSED: RefCell<Vec<AgentId>> = const { RefCell::new(Vec::new()) };
        static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn focus(slot: &AgentSlot) {
        FOCUSED.with_borrow_mut(|focused| focused.push(slot.agent_id));
    }

    pub(crate) fn open(url: &str) {
        OPENED.with_borrow_mut(|opened| opened.push(url.to_owned()));
    }

    /// The agents focused since the last take.
    pub(crate) fn take_focused() -> Vec<AgentId> {
        FOCUSED.take()
    }

    /// The pages opened since the last take.
    pub(crate) fn take_opened() -> Vec<String> {
        OPENED.take()
    }

    /// The gate beside a test's config, which [`gated_job`] waits on: a
    /// file, since the job runs on a thread of its own.
    fn gate(config_path: &std::path::Path) -> std::path::PathBuf {
        config_path.with_file_name("gate")
    }

    /// What the gate holds for a change that fails.
    const FAILS: &str = "fails";

    /// Let the job [`gated_job`] holds end, a failure when `fails`.
    pub(crate) fn open_gate(config_path: &std::path::Path, fails: bool) {
        // Renamed into place, so the job never reads it half written.
        let ajar = config_path.with_file_name("gate.tmp");
        std::fs::write(&ajar, if fails { FAILS } else { "" }).expect("the gate's file");
        std::fs::rename(ajar, gate(config_path)).expect("the gate opens");
    }

    /// Stands in for [`run_job`](super::run_job): holds its job
    /// until [`open_gate`], then words each source as a real change would.
    /// Its rows are the registry's, each source it changed connected unless
    /// it failed.
    pub(crate) fn gated_job(input: super::JobInput) -> super::JobDone {
        use pixtuoid_core::remote::{Changed, Outcome};
        let gate = gate(&input.config_path);
        let fails = loop {
            match std::fs::read_to_string(&gate) {
                Ok(held) => break held == FAILS,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        };
        let done = |id, on| {
            let outcome = if on {
                Outcome::Connected
            } else {
                Outcome::Disconnected
            };
            if fails {
                Err(anyhow::anyhow!("the gate said so"))
            } else {
                Ok(Changed::new(id, on, outcome, false, None, None))
            }
        };
        let (onboarding, notices) = match &input.job {
            &super::PanelJob::Toggle {
                source_id,
                name,
                on,
            } => (
                false,
                vec![super::ChangeNotice::of(
                    source_id,
                    &done(source_id, on),
                    super::connection::SourceOp::of(on),
                    name,
                )],
            ),
            super::PanelJob::Onboarding(ids) => {
                let outcomes: Vec<_> = ids.iter().map(|&id| (id, done(id, true))).collect();
                (true, super::onboarding_notices(&outcomes))
            }
            super::PanelJob::Refresh => (false, Vec::new()),
        };
        super::JobDone {
            onboarding,
            notices,
            rows: rows(|id| {
                !fails && input.job.op_for(id) == Some(super::connection::SourceOp::Connect)
            }),
            installed: INSTALLED.to_vec(),
        }
    }

    /// The agent CLIs [`gated_job`] finds here.
    pub(crate) const INSTALLED: [&str; 2] = ["codex", "reasonix"];

    /// The [placeholder rows](super::connection::placeholder_rows) with each
    /// source connected where `connected` says.
    pub(crate) fn rows(connected: impl Fn(&str) -> bool) -> Vec<super::connection::ConnectionRow> {
        let set = pixtuoid_core::source::registry::REGISTRY
            .iter()
            .filter(|source| connected(source.name))
            .map(|source| source.name.to_owned())
            .collect();
        super::connection::placeholder_rows(&set)
    }

    /// Far past a job's or a save's own time on a loaded machine.
    pub(crate) const LANDS_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

    /// The outcomes of every save `saves` had queued, landed.
    pub(crate) fn saved(
        saves: &super::Saves,
        landed: &mut tokio::sync::mpsc::UnboundedReceiver<super::Landed>,
    ) -> Vec<crate::config::SaveOutcome> {
        assert!(saves.flush(LANDS_WITHIN), "the saves ran");
        std::iter::from_fn(|| landed.try_recv().ok())
            .map(|landed| match landed {
                super::Landed::Saved(outcome) => outcome,
                super::Landed::Job(done) => panic!("a job landed: {done:?}"),
            })
            .collect()
    }

    /// How long [`stall`] holds a read: far past a call that returns at once.
    #[cfg(unix)]
    pub(crate) const STALL: std::time::Duration = std::time::Duration::from_secs(2);

    /// Make `path` a FIFO whose opens for reading wait, as a stalled disk's
    /// reads do, until the returned thread opens it for writing [`STALL`] from
    /// now (<https://pubs.opengroup.org/onlinepubs/9799919799/functions/open.html>,
    /// "When opening a FIFO"); its close then ends their reads empty.
    #[cfg(unix)]
    pub(crate) fn stall(path: &std::path::Path) -> std::thread::JoinHandle<()> {
        use std::os::unix::ffi::OsStrExt;
        let _ = std::fs::remove_file(path);
        let at = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("no NUL");
        // SAFETY: `at` is a NUL-terminated path the test owns.
        assert_eq!(unsafe { libc::mkfifo(at.as_ptr(), 0o600) }, 0, "mkfifo");
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            std::thread::sleep(STALL);
            drop(std::fs::OpenOptions::new().write(true).open(&path));
        })
    }
}

#[cfg(all(test, unix))]
mod saves_tests {
    use super::{Saves, noted};

    /// A flush waits for what is queued while the disk answers within its
    /// bound, and goes on at the bound when it does not: the window's
    /// teardown neither loses its last saves nor hangs on them.
    #[test]
    fn a_flush_waits_for_the_queue_up_to_its_bound() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let landing = tokio::sync::mpsc::unbounded_channel().0;
        let saves = Saves::spawn(path.clone(), landing).expect("a save thread");
        let release = noted::stall(&path);
        saves.save(|path| {
            let _ = crate::config::save_audio_muted(path, true);
            None
        });
        let started = std::time::Instant::now();
        assert!(!saves.flush(noted::STALL / 4), "a stalled save outlasts it");
        assert!(
            started.elapsed() < noted::STALL,
            "the flush went on at its bound"
        );
        assert!(
            saves.flush(noted::LANDS_WITHIN),
            "the save lands once the disk answers"
        );
        let saved = std::fs::read_to_string(&path).expect("the config");
        assert!(saved.contains("muted = true"), "{saved:?}");
        release.join().expect("the stall ends");
    }
}

#[cfg(test)]
mod mouse_tests {
    use std::sync::Arc;

    use super::{
        ClickCtx, ModalMouse, NotedOffice, PointerEvent, modal_mouse, mouse_ladder, noted,
        ui_state::UiState, welcome::WelcomeUi,
    };
    use pixtuoid_core::state::SceneState;
    use pixtuoid_core::{AgentId, AgentSlot};
    use pixtuoid_scene::hit::HitAction;
    use pixtuoid_scene::pet::PetKind;

    fn ui() -> UiState {
        UiState::new(
            &pixtuoid_scene::theme::NORMAL,
            WelcomeUi::from_detected(&[]),
            std::path::PathBuf::new(),
            None,
            crate::doctor::DriftSeen::default(),
        )
    }

    fn no_screen() -> Option<(u16, u16)> {
        None
    }

    fn no_open(url: &str) {
        panic!("opened {url}");
    }

    /// What one pointer event carried out: the agents focused, the pages opened and the pets petted.
    #[derive(Debug, Default, PartialEq)]
    struct Carried {
        focused: Vec<AgentId>,
        opened: Vec<String>,
        petted: Vec<PetKind>,
    }

    /// `event` at cell (0, 0) through the ladder, with no popup, recording
    /// what it carried out.
    fn send(
        ui: &mut UiState,
        office: &mut NotedOffice,
        scene: Option<&SceneState>,
        event: PointerEvent,
    ) -> (ModalMouse, Carried) {
        let petted_before = office.petted.len();
        let modal = mouse_ladder(
            ui,
            0.0,
            event,
            Some((0, 0)),
            no_screen,
            office,
            ClickCtx {
                scene,
                focus: noted::focus,
                open: noted::open,
            },
        );
        let carried = Carried {
            focused: noted::take_focused(),
            opened: noted::take_opened(),
            petted: office.petted[petted_before..].to_vec(),
        };
        (modal, carried)
    }

    /// An office holding one idle agent, `id`.
    fn scene_of(id: AgentId) -> SceneState {
        let slot = AgentSlot {
            agent_id: id,
            source: Arc::from("claude-code"),
            session_id: Arc::from("s"),
            cwd: Arc::from(std::path::Path::new("/repo")),
            label: "x".into(),
            state: pixtuoid_core::state::ActivityState::Idle,
            state_started_at: std::time::SystemTime::UNIX_EPOCH,
            last_event_at: std::time::SystemTime::UNIX_EPOCH,
            created_at: std::time::SystemTime::UNIX_EPOCH,
            arrived_at: None,
            exiting_at: None,
            desk: None,
            tool_call_count: 0,
            running_ms: 0,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        };
        let mut scene = SceneState::uniform(1);
        scene.agents.insert(id, slot);
        scene
    }

    /// With nothing open the office takes the pointer; an inert panel
    /// swallows it whole.
    #[test]
    fn only_a_closed_ladder_reaches_the_office() {
        let mut ui = ui();
        assert_eq!(
            modal_mouse(&mut ui, 0.0, Some((0, 0)), no_screen, no_open),
            ModalMouse::Office
        );
        ui.open_appearance();
        assert_eq!(
            modal_mouse(&mut ui, 0.0, Some((0, 0)), no_screen, no_open),
            ModalMouse::Inert
        );
        assert!(ui.appearance.is_some(), "a click never closes the picker");
    }

    /// A panel opened mid-gesture withholds a press and a click from the
    /// office, never the gesture's move or release, so a figure in hand still
    /// follows and is set down.
    #[test]
    fn a_panel_withholds_the_press_and_the_click_but_not_the_gesture() {
        use PointerEvent::{Move, Press, Release};
        let id = AgentId::from_parts("claude-code", "s");
        let scene = scene_of(id);
        let mut ui = ui();
        let mut office = NotedOffice {
            click: Some(HitAction::Focus(id)),
            ..NotedOffice::default()
        };
        let mut pointer = |ui: &mut UiState, event| send(ui, &mut office, Some(&scene), event);
        assert_eq!(
            pointer(&mut ui, Press),
            (ModalMouse::Office, Carried::default())
        );
        assert_eq!(
            pointer(&mut ui, Move),
            (ModalMouse::Office, Carried::default())
        );
        ui.open_appearance();
        assert_eq!(
            pointer(&mut ui, Move),
            (ModalMouse::Inert, Carried::default())
        );
        assert_eq!(
            pointer(&mut ui, Release),
            (ModalMouse::Inert, Carried::default()),
            "under a panel the click is carried out nowhere"
        );
        assert_eq!(
            pointer(&mut ui, Press),
            (ModalMouse::Inert, Carried::default())
        );
        ui.cancel_appearance();
        assert_eq!(
            pointer(&mut ui, Release),
            (
                ModalMouse::Office,
                Carried {
                    focused: vec![id],
                    ..Carried::default()
                }
            ),
            "with no panel the click is carried out"
        );
        assert_eq!(
            office.got,
            [Press, Move, Move, Release, Release],
            "the gesture's move and release reach the office, the press under the panel does not"
        );
    }

    /// A released click is carried out by what it names: a focus on the
    /// office's live slot, and only there; a page through the launcher; a
    /// petting through the office.
    #[test]
    fn a_click_is_carried_out_by_what_it_names() {
        let id = AgentId::from_parts("claude-code", "s");
        let scene = scene_of(id);
        let mut ui = ui();
        let mut release = |click, scene| {
            let mut office = NotedOffice {
                click: Some(click),
                ..NotedOffice::default()
            };
            let (modal, carried) = send(&mut ui, &mut office, scene, PointerEvent::Release);
            assert_eq!(modal, ModalMouse::Office, "premise: no panel is open");
            carried
        };
        let url = pixtuoid_scene::hit::REPO_URL;
        assert_eq!(
            release(HitAction::Focus(id), Some(&scene)),
            Carried {
                focused: vec![id],
                ..Carried::default()
            }
        );
        assert_eq!(
            release(
                HitAction::Focus(AgentId::from_parts("claude-code", "gone")),
                Some(&scene)
            ),
            Carried::default(),
            "an agent gone from the office is focused nowhere"
        );
        assert_eq!(
            release(HitAction::Focus(id), None),
            Carried::default(),
            "with no live office there is no agent to focus"
        );
        assert_eq!(
            release(HitAction::Open(url), Some(&scene)),
            Carried {
                opened: vec![url.to_owned()],
                ..Carried::default()
            }
        );
        assert_eq!(
            release(HitAction::Pet(PetKind::Cat), Some(&scene)),
            Carried {
                petted: vec![PetKind::Cat],
                ..Carried::default()
            }
        );
    }

    /// A left press closes the help; a move over it does not.
    #[test]
    fn a_press_closes_the_help_and_a_move_does_not() {
        let mut ui = ui();
        ui.toggle_help();
        assert_eq!(
            modal_mouse(&mut ui, 0.0, None, no_screen, no_open),
            ModalMouse::Inert
        );
        assert!(ui.help_open());
        assert_eq!(
            modal_mouse(&mut ui, 1.0, Some((0, 0)), no_screen, no_open),
            ModalMouse::Took
        );
        assert!(!ui.help_open(), "the help wins over the popup");
    }

    /// The popup swallows a press off its URL.
    #[test]
    fn the_popup_swallows_a_press_off_its_url() {
        let mut ui = ui();
        assert_eq!(
            modal_mouse(&mut ui, 1.0, Some((0, 0)), || Some((120, 44)), no_open),
            ModalMouse::Inert
        );
    }

    /// Ignoring `scale` would launch a browser where the popup is still animating.
    #[test]
    fn version_popup_url_clicked_respects_the_rect_and_the_scale() {
        use crate::panels::widgets::version_popup_url_rect;
        let term = (120u16, 44u16);
        let bounds = ratatui::layout::Rect::new(0, 0, term.0, term.1);
        let notices = ["a notice above the link".to_string()];
        let rect = version_popup_url_rect(bounds, &notices, 1.0)
            .expect("a URL rect at scale 1.0, or the misses below pass vacuously");

        assert!(
            super::version_popup_url_clicked(rect.x, rect.y, 1.0, term, &notices),
            "a click inside the URL rect at full scale must hit"
        );
        assert!(
            !super::version_popup_url_clicked(rect.x, rect.y, 1.0, term, &[]),
            "the link sits lower with a notice above it"
        );
        assert!(
            !super::version_popup_url_clicked(
                rect.x,
                rect.y.saturating_sub(1),
                1.0,
                term,
                &notices
            ),
            "a click one row above the URL must miss"
        );
        assert!(
            !super::version_popup_url_clicked(rect.x, rect.y, 0.5, term, &notices),
            "mid-animation (below the clickable scale) there is no rect, so no hit"
        );
    }
}
