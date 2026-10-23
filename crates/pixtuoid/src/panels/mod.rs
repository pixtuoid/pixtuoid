//! The modal panels both painters show: the dashboard, the Sources panel, the
//! help, the theme picker, the version popup and the onboarding, with the one
//! key dispatch that drives them ([`dispatch_key`] decodes, [`apply_key_action`]
//! applies). A painter draws them through [`widgets`] into a ratatui frame and
//! carries out what only it can through [`Host`].

pub(crate) mod connection;
pub(crate) mod dashboard;
pub(crate) mod ui_state;
pub(crate) mod welcome;
pub(crate) mod widgets;

use std::time::{Instant, SystemTime};

use crossterm::event::{KeyCode, KeyModifiers};
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
    pub(crate) theme_picker: Option<usize>,
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
            theme_picker,
            dashboard,
            connection,
            popup_scale,
            version_popup: _,
            help_open,
            host_keys: _,
            onboarding,
        } = self;
        theme_picker.is_some()
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
        theme_picker,
        dashboard,
        connection,
        popup_scale,
        version_popup,
        help_open,
        host_keys,
        onboarding,
    } = ov;
    if let Some(idx) = theme_picker {
        widgets::paint_theme_picker(f, idx, bounds, theme);
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
        widgets::paint_help_overlay(f, bounds, theme, host_keys);
    }
    if onboarding.open {
        widgets::paint_welcome(f, onboarding, bounds, theme);
    }
}

/// What an applied key changes in the painter that shows the office.
pub(crate) trait Host {
    fn set_theme(&mut self, theme: &'static theme::Theme);
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
    pub(crate) help_open: bool,
    pub(crate) version_popup_open: bool,
    pub(crate) theme_picker: Option<usize>,
    pub(crate) dashboard_open: bool,
    pub(crate) connection_open: bool,
    /// A disconnect is armed on the Sources panel, awaiting y/n.
    pub(crate) connection_confirm: bool,
}

impl ModalState {
    /// No panel open.
    #[cfg(test)]
    pub(crate) fn closed() -> Self {
        Self {
            onboarding_open: false,
            help_open: false,
            version_popup_open: false,
            theme_picker: None,
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
            help_open,
            version_popup_open,
            theme_picker,
            dashboard_open,
            connection_open,
            connection_confirm: _,
        } = self;
        onboarding_open
            || help_open
            || version_popup_open
            || theme_picker.is_some()
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
    /// A panel swallowed it and did nothing.
    Inert,
}

/// The modal half of the mouse ladder, in order: the onboarding swallows
/// everything, a left press closes the help (tested before the popup so it
/// wins mid popup-dismiss), the version popup takes only its URL, and the
/// picker, dashboard and Sources panel are inert BY DESIGN, closing only by
/// key ([`dispatch_key`]). A click never leaks to the office behind a panel,
/// where a coffee-machine or branding hit launches a browser. `left_down` is
/// the cell a left press landed on; `popup_scale` the popup's painted scale;
/// `screen` the surface's size in cells, read only for a press on the popup.
pub(crate) fn modal_mouse(
    ui: &mut ui_state::UiState,
    popup_scale: f32,
    left_down: Option<(u16, u16)>,
    screen: impl FnOnce() -> Option<(u16, u16)>,
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
        crate::open_url(&widgets::release_url(env!("CARGO_PKG_VERSION")));
        return ModalMouse::Took;
    }
    if ui.theme_picker.is_some() || ui.dashboard.open || ui.connection.open {
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
    OpenThemePicker,
    /// The index is pre-clamped by the dispatch.
    ThemePreview(usize),
    ThemeCommit(usize),
    ThemeCancel,
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

/// Re-read `source_id`'s connection into the live gate after a change, whatever its
/// outcome: a failed install or uninstall can still have moved the hooks.
fn refresh_gate(
    config_path: &std::path::Path,
    connected: &crate::runtime::ConnectedSources,
    source_id: &str,
) {
    connected.set(
        source_id,
        crate::sources::is_connected(config_path, source_id),
    );
}

fn connect_source(
    config_path: &std::path::Path,
    connected: &crate::runtime::ConnectedSources,
    source_id: &str,
    display_name: &str,
) -> String {
    let done = crate::sources::connect(config_path, source_id);
    refresh_gate(config_path, connected, source_id);
    match done {
        Ok(crate::sources::ConnectOutcome::Installed(r)) => {
            connection::format_connect_result(&r, display_name)
        }
        Ok(crate::sources::ConnectOutcome::FlagOnly) => {
            format!("\u{2713} {display_name} connected")
        }
        Err(e) => connection::format_failure(
            connection::FailedOp::Connect,
            display_name,
            &format!("{e:#}"),
        ),
    }
}

fn disconnect_source(
    config_path: &std::path::Path,
    connected: &crate::runtime::ConnectedSources,
    source_id: &str,
    display_name: &str,
) -> String {
    let done = crate::sources::disconnect(config_path, source_id);
    refresh_gate(config_path, connected, source_id);
    match done {
        Ok(crate::sources::DisconnectOutcome::Uninstalled(r)) => {
            connection::format_disconnect_result(&r, display_name)
        }
        Ok(crate::sources::DisconnectOutcome::FlagOnly) => {
            format!("\u{2713} {display_name} disconnected")
        }
        Err(e) => connection::format_failure(
            connection::FailedOp::Disconnect,
            display_name,
            &format!("{e:#}"),
        ),
    }
}

/// The source id rides along so the panel can put its selection — and the offered `t`
/// retry — on the row that actually failed.
#[derive(Debug)]
struct OnboardingFailure {
    source_id: String,
    line: String,
}

/// One presentable failure per failed onboarding connect: in TUI mode the alternate
/// screen owns the terminal, so the warn-floor log is not a user surface.
fn onboarding_failures(
    outcomes: &[(String, crate::sources::AppliedChange)],
) -> Vec<OnboardingFailure> {
    let mut failures = Vec::new();
    for (id, oc) in outcomes {
        if let crate::sources::AppliedChange::Failed(e) = oc {
            tracing::warn!(source = %id, error = ?e, "onboarding: connect failed");
            let name =
                crate::install::target::by_source(id).map_or(id.as_str(), |t| t.display_name);
            failures.push(OnboardingFailure {
                source_id: id.clone(),
                line: connection::format_failure(connection::FailedOp::Connect, name, e),
            });
        }
    }
    failures
}

/// Open the Sources panel ON the first failed row and seed its result line, so the `t`
/// retry is one keystroke away on the right source.
fn surface_onboarding_failures(
    ui: &mut ui_state::UiState,
    connected: &crate::runtime::ConnectedSources,
    failures: Vec<OnboardingFailure>,
) {
    let Some(first) = failures.first() else {
        return;
    };
    let first_id = first.source_id.clone();
    let rows = connection::build_rows(&connected.snapshot(), &ui.read_conn_log());
    ui.open_connection(rows);
    ui.select_connection_source(&first_id);
    ui.connection.last_result = Some(
        failures
            .into_iter()
            .map(|f| f.line)
            .collect::<Vec<_>>()
            .join("  \u{b7}  "),
    );
}

pub(crate) fn is_quit_chord(code: KeyCode, mods: KeyModifiers) -> bool {
    matches!(
        (code, mods),
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL)
    )
}

/// What pressing `t` on a Sources-panel row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToggleIntent {
    ArmConfirm,
    Connect,
    /// Absent CLI that was never connected — an inert "not detected" hint.
    Hint,
}

/// The load-bearing arm is `NoCli { connected: true }` → `ArmConfirm`: a source whose
/// CLI vanished is still disconnectable, since its hooks live in the config, not in the
/// missing binary.
fn toggle_intent(state: connection::ConnState) -> ToggleIntent {
    match state {
        connection::ConnState::Connected | connection::ConnState::NoCli { connected: true } => {
            ToggleIntent::ArmConfirm
        }
        connection::ConnState::Disconnected => ToggleIntent::Connect,
        connection::ConnState::NoCli { connected: false } => ToggleIntent::Hint,
    }
}

/// The key that opens and closes the Sources panel.
pub(crate) const SOURCES_KEY: char = 's';

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
    if modal.onboarding_open {
        return match (code, mods) {
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => KeyAction::OnboardingUp,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => KeyAction::OnboardingDown,
            (KeyCode::Char(' '), _) => KeyAction::OnboardingToggle,
            (KeyCode::Enter, _) => KeyAction::OnboardingConfirm,
            (KeyCode::Esc, _) => KeyAction::OnboardingSkip,
            _ => KeyAction::None,
        };
    }
    if modal.help_open {
        return match (code, mods) {
            (KeyCode::Enter, _) | (KeyCode::Esc, _) | (KeyCode::Char('?'), _) => {
                KeyAction::CloseHelp
            }
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
                (KeyCode::Char('y'), _) => KeyAction::ConnectionConfirm,
                (KeyCode::Char('n'), _) | (KeyCode::Esc, _) => KeyAction::ConnectionCancelConfirm,
                _ => KeyAction::None,
            };
        }
        return match (code, mods) {
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            (KeyCode::Esc, _) | (KeyCode::Char(SOURCES_KEY), _) => KeyAction::ConnectionClose,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => KeyAction::ConnectionUp,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => KeyAction::ConnectionDown,
            (KeyCode::Char('t'), _) => KeyAction::ConnectionToggle,
            _ => KeyAction::None,
        };
    }
    if modal.dashboard_open {
        return match (code, mods) {
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            (KeyCode::Esc, _) | (KeyCode::Tab, _) => KeyAction::DashboardClose,
            (KeyCode::Enter, _) => KeyAction::DashboardJump,
            (KeyCode::Char('f'), _) => KeyAction::DashboardFocus,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => KeyAction::DashboardUp,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => KeyAction::DashboardDown,
            (KeyCode::Left, _) | (KeyCode::Char('h'), _) => KeyAction::DashboardFoldLeft,
            (KeyCode::Right, _) | (KeyCode::Char('l'), _) => KeyAction::DashboardFoldRight,
            (KeyCode::Char('z'), _) => KeyAction::DashboardFoldAll,
            _ => KeyAction::None,
        };
    }
    if let Some(idx) = modal.theme_picker {
        return match (code, mods) {
            // Safe to quit mid-preview: only a commit saves the theme.
            _ if is_quit_chord(code, mods) => KeyAction::Quit,
            (KeyCode::Up | KeyCode::Char('k'), _) => KeyAction::ThemePreview(idx.saturating_sub(1)),
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                KeyAction::ThemePreview((idx + 1).min(theme::ALL_THEMES.len().saturating_sub(1)))
            }
            (KeyCode::Enter, _) => KeyAction::ThemeCommit(idx),
            (KeyCode::Esc, _) => KeyAction::ThemeCancel,
            _ => KeyAction::None,
        };
    }
    if is_quit_chord(code, mods) || code == KeyCode::Esc {
        return KeyAction::Quit;
    }
    match code {
        KeyCode::Char('p') => KeyAction::TogglePause,
        KeyCode::Char('m') => KeyAction::ToggleAudioMute,
        KeyCode::Char('+') | KeyCode::Char('=') => KeyAction::AdjustVolume(true),
        KeyCode::Char('-') | KeyCode::Char('_') => KeyAction::AdjustVolume(false),
        KeyCode::Char('t') => KeyAction::OpenThemePicker,
        KeyCode::Char('?') => KeyAction::ToggleHelp,
        KeyCode::Char('v') => KeyAction::OpenVersionPopup,
        KeyCode::Tab => KeyAction::ToggleDashboard,
        KeyCode::Char(SOURCES_KEY) => KeyAction::ToggleConnection,
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
    /// The agent CLIs installed here, injected because the real
    /// `crate::sources::detect` reads the developer's own machine.
    pub(crate) installed: fn() -> Vec<&'static str>,
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
            // Unpause restores the user's own m-key state rather than clobbering it.
            cx.audio_ctl.set_paused(cx.ui.paused());
        }
        KeyAction::ToggleHelp => cx.ui.toggle_help(),
        KeyAction::CloseHelp => cx.ui.close_help(),
        KeyAction::DismissVersionPopup => cx.ui.dismiss_version_popup(),
        KeyAction::OpenVersionPopup => cx
            .ui
            .open_version_popup(&cx.connected.snapshot(), &(cx.installed)()),
        KeyAction::OpenThemePicker => cx.ui.open_theme_picker(),
        KeyAction::ThemePreview(i) => {
            cx.ui.preview_theme(i);
            cx.host.set_theme(theme::ALL_THEMES[i]);
        }
        KeyAction::ThemeCommit(i) => {
            cx.ui.commit_theme(i);
            let name = theme::ALL_THEMES[i].name;
            if let Err(e) = crate::config::save(cx.config_path, name) {
                tracing::warn!(error = ?e, "failed to persist theme");
            }
        }
        KeyAction::ThemeCancel => {
            let saved = cx.ui.cancel_theme();
            cx.host.set_theme(theme::ALL_THEMES[saved]);
        }
        KeyAction::NavigateFloor(target) => {
            cx.host.navigate_floor(target, cx.now);
        }
        KeyAction::ToggleAudioMute => {
            cx.audio_ctl.apply(
                crate::audio::AudioAction::ToggleMute,
                cx.ui.paused(),
                Instant::now(),
                cx.respawn,
            );
        }
        KeyAction::AdjustVolume(up) => {
            cx.audio_ctl.apply(
                crate::audio::AudioAction::Volume(up),
                cx.ui.paused(),
                Instant::now(),
                cx.respawn,
            );
        }
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
                crate::focus::focus_slot(slot);
            }
        }
        KeyAction::ToggleConnection => {
            if cx.ui.connection.open {
                cx.ui.close_connection();
            } else {
                // FS reads happen on open and after each toggle, never per frame.
                let rows = connection::build_rows(&cx.connected.snapshot(), &cx.ui.read_conn_log());
                cx.ui.open_connection(rows);
            }
        }
        KeyAction::ConnectionUp => cx.ui.connection_move(-1),
        KeyAction::ConnectionDown => cx.ui.connection_move(1),
        KeyAction::ConnectionToggle => {
            // Copy the fields out before any rebuild of `rows` (which would
            // invalidate a `&ConnectionRow` borrow).
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
                match toggle_intent(state) {
                    ToggleIntent::ArmConfirm => {
                        cx.ui.connection.confirm = Some(cx.ui.connection.selected);
                    }
                    ToggleIntent::Connect => {
                        cx.ui.connection.last_result = Some(connect_source(
                            cx.config_path,
                            cx.connected,
                            source_id,
                            name,
                        ));
                        cx.ui.connection.rows = connection::build_rows(
                            &cx.connected.snapshot(),
                            &cx.ui.read_conn_log(),
                        );
                    }
                    ToggleIntent::Hint => {
                        cx.ui.connection.last_result = Some(hint);
                    }
                }
            }
        }
        KeyAction::ConnectionConfirm => {
            if let Some(idx) = cx.ui.connection.confirm {
                let action = cx
                    .ui
                    .connection
                    .rows
                    .get(idx)
                    .map(|r| (r.source_id, r.display_name));
                if let Some((source_id, name)) = action {
                    cx.ui.connection.last_result = Some(disconnect_source(
                        cx.config_path,
                        cx.connected,
                        source_id,
                        name,
                    ));
                    cx.ui.connection.rows =
                        connection::build_rows(&cx.connected.snapshot(), &cx.ui.read_conn_log());
                }
            }
            cx.ui.connection.confirm = None;
        }
        KeyAction::ConnectionCancelConfirm => cx.ui.cancel_connection_confirm(),
        KeyAction::ConnectionClose => cx.ui.close_connection(),
        KeyAction::OnboardingUp => cx.ui.onboarding_ui.move_up(),
        KeyAction::OnboardingDown => cx.ui.onboarding_ui.move_down(),
        KeyAction::OnboardingToggle => cx.ui.onboarding_ui.toggle_selected(),
        KeyAction::OnboardingConfirm => {
            let checked = cx.ui.onboarding_ui.checked();
            let outcomes = crate::sources::connect_each(cx.config_path, &checked);
            for (id, _) in &outcomes {
                refresh_gate(cx.config_path, cx.connected, id);
            }
            let failed = onboarding_failures(&outcomes);
            cx.ui.close_onboarding();
            surface_onboarding_failures(cx.ui, cx.connected, failed);
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
        FloorNav, KeyAction, ModalState, connect_source, connection, disconnect_source,
        dispatch_key,
    };
    use crossterm::event::{KeyCode, KeyModifiers};

    const NONE: KeyModifiers = KeyModifiers::NONE;
    const CTRL: KeyModifiers = KeyModifiers::CONTROL;

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

    #[test]
    fn toggle_intent_covers_the_four_arms() {
        use super::connection::ConnState;
        use super::{ToggleIntent, toggle_intent};
        assert_eq!(
            toggle_intent(ConnState::Connected),
            ToggleIntent::ArmConfirm
        );
        assert_eq!(
            toggle_intent(ConnState::Disconnected),
            ToggleIntent::Connect
        );
        assert_eq!(
            toggle_intent(ConnState::NoCli { connected: true }),
            ToggleIntent::ArmConfirm
        );
        assert_eq!(
            toggle_intent(ConnState::NoCli { connected: false }),
            ToggleIntent::Hint
        );
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
            KeyAction::OpenThemePicker
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
            theme_picker: Some(2),
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

    /// `v` reopens the popup from the office only: every panel's own key map
    /// leaves it unbound, so it never acts behind an open panel.
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
                theme_picker: Some(0),
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
            theme_picker: Some(2),
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
            KeyAction::ThemeCommit(2)
        );
        assert_eq!(
            dispatch_key(KeyCode::Esc, NONE, c, nav()),
            KeyAction::ThemeCancel
        );
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
            theme_picker: Some(0),
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Up, NONE, lo, nav()),
            KeyAction::ThemePreview(0)
        );
        let hi = ModalState {
            theme_picker: Some(pixtuoid_scene::theme::ALL_THEMES.len() - 1),
            ..modal()
        };
        assert_eq!(
            dispatch_key(KeyCode::Down, NONE, hi, nav()),
            KeyAction::ThemePreview(pixtuoid_scene::theme::ALL_THEMES.len() - 1)
        );
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
            theme_picker: Some(0),
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

    #[test]
    fn connect_source_persists_then_flips_the_gate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.toml");
        let connected = crate::runtime::ConnectedSources::default();

        let res = connect_source(&cfg, &connected, "copilot", "Copilot CLI");
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

        let res = disconnect_source(&cfg, &connected, "copilot", "Copilot CLI");
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

        let res = connect_source(&cfg, &connected, "copilot", "Copilot CLI");
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
        let res = disconnect_source(&cfg, &connected, "copilot", "Copilot CLI");
        drop(held);
        assert!(res.contains("disconnect failed"), "result: {res}");
        assert!(
            connected.is_connected("copilot"),
            "still flagged on disk, so still connected"
        );
    }

    #[test]
    fn a_failed_onboarding_connect_reports_the_reason_to_the_caller() {
        use crate::sources::AppliedChange;
        let outcomes = vec![
            (
                "cursor".to_string(),
                AppliedChange::Failed("settings is valid JSON but not an object".into()),
            ),
            ("antigravity".to_string(), AppliedChange::Connected),
        ];
        let failures = super::onboarding_failures(&outcomes);
        assert_eq!(
            failures.len(),
            1,
            "only the failed row reports: {failures:?}"
        );
        let line = &failures[0].line;
        assert!(
            line.contains("settings is valid JSON but not an object"),
            "the REASON must survive — it is the whole point: {line}"
        );
        let display_name = crate::install::target::by_source("cursor")
            .expect("cursor is a target-bearing source")
            .display_name;
        assert!(
            line.contains(display_name),
            "the row must be named the way every other surface names it: {line}"
        );
        assert_eq!(
            line,
            &connection::format_failure(
                connection::FailedOp::Connect,
                display_name,
                "settings is valid JSON but not an object",
            ),
            "the panel's own wording, from the panel's own formatter: {line}"
        );
        assert_eq!(
            failures[0].source_id, "cursor",
            "the failure carries the row it belongs to, so the panel can select it"
        );
    }

    #[test]
    fn onboarding_failures_open_the_sources_panel_on_the_failed_row() {
        let connected = crate::runtime::ConnectedSources::default();
        let mut ui = crate::panels::ui_state::UiState::new(
            pixtuoid_scene::theme::ALL_THEMES[0],
            crate::panels::welcome::WelcomeUi::from_detected(&[]),
            std::path::PathBuf::from("/tmp/sock"),
            None,
            crate::doctor::DriftSeen::default(),
        );
        assert!(!ui.modal().connection_open, "panel starts closed");

        super::surface_onboarding_failures(&mut ui, &connected, Vec::new());
        assert!(
            !ui.modal().connection_open,
            "a clean apply must not pop the panel"
        );

        super::surface_onboarding_failures(
            &mut ui,
            &connected,
            vec![super::OnboardingFailure {
                source_id: "cursor".into(),
                line: "Cursor: connect failed \u{2014} boom".into(),
            }],
        );
        assert!(ui.modal().connection_open, "a failure opens the panel");
        assert_eq!(
            ui.connection.last_result.as_deref(),
            Some("Cursor: connect failed \u{2014} boom"),
            "the reason rides the panel's own result line"
        );
        let selected = ui.connection.rows[ui.connection.selected].source_id;
        assert_eq!(
            selected, "cursor",
            "the panel must open ON the failed row — `t` acts on the SELECTED one"
        );
        assert_ne!(
            ui.connection.selected, 0,
            "cursor is not the first registry row, so this could not pass by default"
        );
    }
}

#[cfg(test)]
mod mouse_tests {
    use super::{ModalMouse, modal_mouse, ui_state::UiState, welcome::WelcomeUi};

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

    /// With nothing open the office takes the pointer; an inert panel
    /// swallows it whole.
    #[test]
    fn only_a_closed_ladder_reaches_the_office() {
        let mut ui = ui();
        assert_eq!(
            modal_mouse(&mut ui, 0.0, Some((0, 0)), no_screen),
            ModalMouse::Office
        );
        ui.open_theme_picker();
        assert_eq!(
            modal_mouse(&mut ui, 0.0, Some((0, 0)), no_screen),
            ModalMouse::Inert
        );
        assert!(ui.theme_picker.is_some(), "a click never closes the picker");
    }

    /// A left press closes the help; a move over it does not.
    #[test]
    fn a_press_closes_the_help_and_a_move_does_not() {
        let mut ui = ui();
        ui.toggle_help();
        assert_eq!(
            modal_mouse(&mut ui, 0.0, None, no_screen),
            ModalMouse::Inert
        );
        assert!(ui.help_open());
        assert_eq!(
            modal_mouse(&mut ui, 1.0, Some((0, 0)), no_screen),
            ModalMouse::Took
        );
        assert!(!ui.help_open(), "the help wins over the popup");
    }

    /// The popup swallows a press off its URL.
    #[test]
    fn the_popup_swallows_a_press_off_its_url() {
        let mut ui = ui();
        assert_eq!(
            modal_mouse(&mut ui, 1.0, Some((0, 0)), || Some((120, 44))),
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
