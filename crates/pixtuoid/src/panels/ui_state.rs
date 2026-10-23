//! Each painter's loop's per-surface UI state, `run_tui`'s and the floating
//! window's: [`UiState`] owns each modal surface's open/close transitions,
//! projects the dispatch-facing [`ModalState`] (one source of truth instead of
//! an ad-hoc literal per key event), and computes the per-frame renderer
//! mirrors ([`RenderFrames`]). The loop keeps its painter's lifecycle and every
//! renderer/config/install side effect; the blocking-I/O sites (`build_rows`,
//! connect/disconnect, the onboarding apply) stay at the loop as brief inline
//! stalls.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use pixtuoid_core::source::manager::SourceDeath;
use pixtuoid_core::state::SceneState;
use pixtuoid_scene::theme;

use super::{ModalState, connection, dashboard, welcome};
use connection::{ConnectionFrame, ConnectionRow, ConnectionUi};
use dashboard::{DashboardFrame, DashboardUi};
use welcome::{OnboardingFrame, WelcomeUi};

/// One frame's renderer mirrors — bundling them keeps the compute (here) and
/// the push (one call in the loop) from drifting apart per surface.
#[derive(Debug, Default)]
pub struct RenderFrames {
    pub theme_picker: Option<usize>,
    pub version_popup: VersionPopupFrame,
    pub help_open: bool,
    pub source_warning: Option<String>,
    pub dashboard: DashboardFrame,
    pub connection: ConnectionFrame,
    pub onboarding: OnboardingFrame,
}

/// The version popup: whether it is open, and what it last opened with,
/// kept past its close so the shrinking popup still shows it.
#[derive(Debug, Default, Clone)]
pub struct VersionPopupFrame {
    pub open: bool,
    /// This run's boot crossed an upgrade, which the title says.
    pub upgraded: bool,
    pub notices: Arc<[String]>,
}

impl RenderFrames {
    /// Its panels as `paint_overlays` draws them,
    /// the version popup at `popup_scale` ([`popup_whole`] for a painter with
    /// no animation of its own) and `host_keys` after the help's shared rows.
    /// The one place a frame's panels become an
    /// [`OverlayFrame`](super::OverlayFrame).
    pub fn overlays(
        &self,
        popup_scale: f32,
        host_keys: &'static [super::widgets::Shortcut],
    ) -> super::OverlayFrame<'_> {
        super::OverlayFrame {
            theme_picker: self.theme_picker,
            dashboard: &self.dashboard,
            connection: &self.connection,
            popup_scale,
            version_popup: &self.version_popup,
            help_open: self.help_open,
            host_keys,
            onboarding: &self.onboarding,
        }
    }
}

/// The version popup's scale for a painter that draws it whole, with no
/// animation of its own.
pub fn popup_whole(open: bool) -> f32 {
    if open { 1.0 } else { 0.0 }
}

/// The per-surface UI state. Fields stay `pub(crate)` where the loop's I/O arms
/// read/write them directly: state lives here, side effects stay in the loop.
pub(crate) struct UiState {
    // First-run onboarding "move-in" overlay (TOP of the modal precedence
    // chain). It is "open" exactly while `onboarding_opened_at` is `Some`, which
    // is also the clock its painter's typewriter reads.
    pub(crate) onboarding_ui: WelcomeUi,
    onboarding_opened_at: Option<Instant>,
    /// `(fade start, the dim the open ramp was interrupted at)`.
    onboarding_closing_at: Option<(Instant, pixtuoid_scene::look::Dim)>,
    version_popup_open: bool,
    notices: Notices,
    help_open: bool,
    /// `[p]ause`: while paused, `now()` returns the frozen instant so every
    /// clock-driven animation (and the dashboard marquee) holds still.
    pause: pixtuoid_scene::anim::PauseClock,
    /// `$PIXTUOID_FAKE_NOW`'s instant, Unix seconds, and when it was read:
    /// the clock `just pace-check` starts at a transition or at dusk, read
    /// nowhere else.
    fake_now: Option<(Instant, SystemTime)>,
    /// Theme picker: `Some(preview index)` while open; `saved_theme_idx` is
    /// the committed selection the cancel path reverts to.
    pub(crate) theme_picker: Option<usize>,
    pub(crate) saved_theme_idx: usize,
    pub(crate) dashboard: DashboardUi,
    pub(crate) connection: ConnectionUi,
    drift: crate::doctor::DriftSeen,
    socket_path: std::path::PathBuf,
    /// Where the warn-floor log lives, for the Sources panel; `None` for a
    /// preview, which has no run.
    log: Option<crate::run_log::LogLocation>,
}

/// The version popup's notices: what they are measured against, and what it
/// last opened with.
#[derive(Default)]
struct Notices {
    versions: Option<crate::version::Versions>,
    /// [`crate::version::BootDecision::should_show_popup`], before the
    /// onboarding could hold the popup back.
    upgraded: bool,
    terminal_cutaway: bool,
    shown: Arc<[String]>,
}

/// What a painter's loop hands its panels at boot.
pub(crate) struct Boot<'a> {
    pub(crate) theme: &'static theme::Theme,
    /// Whether the onboarding may open ([`crate::sources::is_first_run`]).
    pub(crate) first_run: bool,
    pub(crate) config_path: &'a std::path::Path,
    pub(crate) socket_path: std::path::PathBuf,
    pub(crate) log: crate::run_log::LogLocation,
    pub(crate) drift: crate::doctor::DriftSeen,
    /// The sources connected at boot, for the notices a version popup opens
    /// with.
    pub(crate) connected: &'a HashSet<String>,
    /// [`crate::version::Facts::terminal_cutaway`].
    pub(crate) terminal_cutaway: bool,
}

/// `$PIXTUOID_FAKE_NOW`'s clock, said once at warn: a stray export would
/// otherwise skew the sky and the weather unseen.
fn fake_now() -> Option<(Instant, SystemTime)> {
    let set = pixtuoid_core::platform::text_env("PIXTUOID_FAKE_NOW")?;
    let Some(at) =
        set.trim().parse::<u64>().ok().and_then(|secs| {
            SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(secs))
        })
    else {
        tracing::warn!(value = ?set, "PIXTUOID_FAKE_NOW is not a Unix second this clock holds: the clock is now");
        return None;
    };
    tracing::warn!(at = ?at, "the clock starts at PIXTUOID_FAKE_NOW, not now");
    Some((Instant::now(), at))
}

impl UiState {
    /// The panels a run opens with: the onboarding on a first run that finds
    /// an agent CLI to connect, else the version popup when this version is
    /// new to the config at `boot.config_path`.
    pub(crate) fn boot(boot: Boot<'_>) -> Self {
        let detected = if boot.first_run {
            crate::sources::detect_targets()
        } else {
            Vec::new()
        };
        Self::boot_with(
            boot,
            WelcomeUi::from_detected(&detected),
            crate::sources::detect,
            env!("CARGO_PKG_VERSION"),
        )
    }

    /// [`Self::boot`] with the onboarding, the installed-CLI probe and this
    /// build's version injected.
    fn boot_with(
        boot: Boot<'_>,
        onboarding_ui: WelcomeUi,
        installed: impl FnOnce() -> Vec<&'static str>,
        current: &str,
    ) -> Self {
        let decision = resolve_version_popup(boot.config_path, current);
        // Yields to the onboarding but still STAMPS `last_seen_version`: gated on
        // the overlay SHOWING, not on `first_run`, which a no-CLI user carries
        // forever.
        let pops_up = decision.should_show_popup && onboarding_ui.is_empty();
        let mut ui = Self::new(
            boot.theme,
            onboarding_ui,
            boot.socket_path,
            Some(boot.log),
            boot.drift,
        );
        ui.notices.versions = decision.versions;
        ui.notices.upgraded = decision.should_show_popup;
        ui.notices.terminal_cutaway = boot.terminal_cutaway;
        if pops_up {
            ui.open_version_popup(boot.connected, &installed());
        }
        ui
    }

    pub(crate) fn new(
        boot_theme: &'static theme::Theme,
        onboarding_ui: WelcomeUi,
        socket_path: std::path::PathBuf,
        log: Option<crate::run_log::LogLocation>,
        drift: crate::doctor::DriftSeen,
    ) -> Self {
        let onboarding_opened_at = (!onboarding_ui.is_empty()).then(Instant::now);
        let saved_theme_idx = theme::ALL_THEMES
            .iter()
            .position(|t| std::ptr::eq(*t, boot_theme))
            .expect("the boot theme is one of ALL_THEMES");
        Self {
            onboarding_ui,
            onboarding_opened_at,
            onboarding_closing_at: None,
            version_popup_open: false,
            notices: Notices::default(),
            help_open: false,
            pause: pixtuoid_scene::anim::PauseClock::default(),
            fake_now: fake_now(),
            theme_picker: None,
            saved_theme_idx,
            dashboard: DashboardUi::default(),
            connection: ConnectionUi::default(),
            drift,
            socket_path,
            log,
        }
    }

    /// The dispatch-facing modal snapshot — THE projection: no surface's open
    /// flag has a second home.
    pub(crate) fn modal(&self) -> ModalState {
        ModalState {
            onboarding_open: self.onboarding_open(),
            help_open: self.help_open,
            version_popup_open: self.version_popup_open,
            theme_picker: self.theme_picker,
            dashboard_open: self.dashboard.open,
            connection_open: self.connection.open,
            connection_confirm: self.connection.confirm.is_some(),
        }
    }

    /// This frame's wall clock: real time, or the frozen instant while paused.
    pub(crate) fn now(&mut self) -> SystemTime {
        let wall = self
            .fake_now
            .and_then(|(read, at)| at.checked_add(read.elapsed()))
            .unwrap_or_else(SystemTime::now);
        self.pause.now(wall)
    }

    pub(crate) fn toggle_pause(&mut self) {
        self.pause.toggle();
    }

    pub(crate) fn paused(&self) -> bool {
        self.pause.paused()
    }

    pub(crate) fn toggle_help(&mut self) {
        self.help_open = !self.help_open;
    }

    pub(crate) fn close_help(&mut self) {
        self.help_open = false;
    }

    pub(crate) fn help_open(&self) -> bool {
        self.help_open
    }

    pub(crate) fn dismiss_version_popup(&mut self) {
        self.version_popup_open = false;
    }

    /// Open the version popup with the notices due while `connected` holds
    /// and the agent CLIs `installed` here are.
    pub(crate) fn open_version_popup(
        &mut self,
        connected: &HashSet<String>,
        installed: &[&'static str],
    ) {
        let facts = crate::version::Facts {
            installed,
            connected,
            terminal_cutaway: self.notices.terminal_cutaway,
        };
        self.notices.shown = crate::version::due(self.notices.versions.as_ref(), &facts).into();
        self.version_popup_open = true;
    }

    /// The notices the version popup last opened with.
    pub(crate) fn version_notices(&self) -> &[String] {
        &self.notices.shown
    }

    pub(crate) fn open_theme_picker(&mut self) {
        self.theme_picker = Some(self.saved_theme_idx);
    }

    pub(crate) fn preview_theme(&mut self, idx: usize) {
        self.theme_picker = Some(idx);
    }

    pub(crate) fn commit_theme(&mut self, idx: usize) {
        self.saved_theme_idx = idx;
        self.theme_picker = None;
    }

    /// Esc in the picker: close and return the saved index to revert to.
    pub(crate) fn cancel_theme(&mut self) -> usize {
        self.theme_picker = None;
        self.saved_theme_idx
    }

    pub(crate) fn toggle_dashboard(&mut self, scene: &SceneState) {
        self.dashboard.open = !self.dashboard.open;
        if self.dashboard.open {
            let rows = dashboard::build_dashboard_rows(scene, &self.dashboard.folds);
            self.dashboard.selected = dashboard::reanchor_selection(&rows, self.dashboard.selected);
        }
    }

    pub(crate) fn close_dashboard(&mut self) {
        self.dashboard.open = false;
    }

    pub(crate) fn dashboard_move(&mut self, scene: &SceneState, delta: i32) {
        let rows = dashboard::build_dashboard_rows(scene, &self.dashboard.folds);
        self.dashboard.selected = dashboard::move_selection(&rows, self.dashboard.selected, delta);
    }

    /// `←/h`: on a child, collapse its parent and move the cursor up to the
    /// (now collapsed) root so it stays visible; on a root, collapse it.
    pub(crate) fn dashboard_fold_left(&mut self, scene: &SceneState) {
        let rows = dashboard::build_dashboard_rows(scene, &self.dashboard.folds);
        if let Some(sel) = self.dashboard.selected
            && let Some(row) = rows.iter().find(|r| r.agent_id == sel)
        {
            let root = row.parent_id.unwrap_or(sel);
            self.dashboard.folds.fold_all([root]);
            self.dashboard.selected = Some(root);
        }
    }

    /// `→/l`: only roots are collapsible; expand the selected one.
    pub(crate) fn dashboard_fold_right(&mut self, scene: &SceneState) {
        let rows = dashboard::build_dashboard_rows(scene, &self.dashboard.folds);
        if let Some(sel) = self.dashboard.selected
            && rows
                .iter()
                .any(|r| r.agent_id == sel && r.parent_id.is_none())
        {
            self.dashboard.folds.unfold_all([sel]);
        }
    }

    /// `z`: fold-all / unfold-all toggle across every root.
    pub(crate) fn dashboard_fold_all(&mut self, scene: &SceneState) {
        let rows = dashboard::build_dashboard_rows(scene, &self.dashboard.folds);
        let roots: Vec<_> = rows
            .iter()
            .filter(|r| r.parent_id.is_none())
            .map(|r| r.agent_id)
            .collect();
        let any_expanded = rows.iter().any(|r| r.parent_id.is_none() && !r.collapsed);
        if any_expanded {
            self.dashboard.folds.fold_all(roots);
        } else {
            self.dashboard.folds.unfold_all(roots);
        }
    }

    /// `Enter`: close and return the selected agent's floor (if resolvable)
    /// for the loop to navigate to.
    pub(crate) fn dashboard_jump(&mut self, scene: &SceneState) -> Option<usize> {
        let floor = self.dashboard.selected.and_then(|sel| {
            let rows = dashboard::build_dashboard_rows(scene, &self.dashboard.folds);
            dashboard::resolve_floor(&rows, sel)
        });
        self.dashboard.open = false;
        floor
    }

    /// `f` in the dashboard: the selected row's agent, for the focus-jump. The
    /// panel STAYS open — focusing a terminal is a glance-and-return action,
    /// unlike Enter's floor navigation which closes to show it.
    pub(crate) fn dashboard_focus(&self) -> Option<pixtuoid_core::AgentId> {
        self.dashboard.selected
    }

    /// `s` on a closed panel: open with the freshly rebuilt connection facet.
    /// The rows' blocking I/O (`build_rows` — FS probes + per-source `diagnose`)
    /// runs at the loop and is handed in; it happens ON OPEN only, never per
    /// frame.
    pub(crate) fn open_connection(&mut self, rows: Vec<ConnectionRow>) {
        self.connection.open = true;
        self.connection.confirm = None;
        self.connection.rows = rows;
        self.connection.selected =
            connection::move_selection(&self.connection.rows, self.connection.selected, 0);
        self.connection.last_result = None;
    }

    /// Park the selection on `source_id`'s row, leaving it where it is when the
    /// panel has no such row. `open_connection` deliberately CARRIES the previous
    /// index, so a caller that opens the panel about ONE source has to say which;
    /// otherwise the `t` it offers acts on an unrelated row. Unlike
    /// `connection_move` this keeps `last_result` — the reason the panel opened.
    pub(crate) fn select_connection_source(&mut self, source_id: &str) {
        if let Some(idx) = self
            .connection
            .rows
            .iter()
            .position(|r| r.source_id == source_id)
        {
            self.connection.selected = idx;
        }
    }

    pub(crate) fn connection_move(&mut self, delta: i32) {
        self.connection.selected =
            connection::move_selection(&self.connection.rows, self.connection.selected, delta);
        self.connection.last_result = None;
    }

    pub(crate) fn cancel_connection_confirm(&mut self) {
        self.connection.confirm = None;
    }

    pub(crate) fn close_connection(&mut self) {
        self.connection.open = false;
        self.connection.confirm = None;
    }

    pub(crate) fn onboarding_open(&self) -> bool {
        self.onboarding_opened_at.is_some()
    }

    /// Whether the backdrop's dim is moving: through the open ramp, or the
    /// close fade, which every frame shows a step of.
    pub(crate) fn dim_moving(&self) -> bool {
        self.onboarding_opened_at.is_some_and(|opened| {
            opened.elapsed() < std::time::Duration::from_millis(welcome::DIM_RAMP_MS)
        }) || self.onboarding_closing_at.is_some()
    }

    /// Confirm/skip both end the overlay the same way: card gone, close fade
    /// armed from the open ramp's current dim.
    pub(crate) fn close_onboarding(&mut self) {
        self.onboarding_closing_at = self.onboarding_opened_at.take().map(|o| {
            (
                Instant::now(),
                welcome::dim_opening(o.elapsed().as_millis() as u64),
            )
        });
    }

    /// Compute this frame's renderer mirrors from the live scene + health
    /// snapshot. Mutates the pieces that are themselves per-frame state: the
    /// dashboard reanchor/scroll clamp and the onboarding close-fade expiry.
    pub(crate) fn build_frames(
        &mut self,
        now: SystemTime,
        scene: &SceneState,
        health: &[SourceDeath],
    ) -> RenderFrames {
        let source_warning = crate::doctor::footer_warning(health, &self.drift.prefixes());

        // Re-anchor the selection by AgentId — an agent may have exited.
        let dashboard_frame = if self.dashboard.open {
            let rows = dashboard::build_dashboard_rows(scene, &self.dashboard.folds);
            self.dashboard.selected = dashboard::reanchor_selection(&rows, self.dashboard.selected);
            self.dashboard.scroll = dashboard::clamp_scroll(
                &rows,
                self.dashboard.selected,
                self.dashboard.scroll,
                dashboard::DASHBOARD_VIEWPORT_ROWS,
            );
            DashboardFrame {
                open: true,
                rows,
                selected: self.dashboard.selected,
                scroll: self.dashboard.scroll,
            }
        } else {
            DashboardFrame::default()
        };

        // The HOOK facet (`connection.rows`) is cached — rebuilt on open and
        // after actions, NOT per frame, because it does FS reads. Only the LIVE
        // facet + socket line recompute here.
        let connection_frame = if self.connection.open {
            let live = connection::live_view(now, &self.connection.rows, scene, health);
            // NOT "(listening)": a second pixtuoid instance loses the hook plane
            // to the lock-holding owner, so THIS process may own no socket at
            // all. Don't assert what we haven't checked.
            let socket_line = format!("socket  {}", self.socket_path.display());
            ConnectionFrame {
                open: true,
                rows: self.connection.rows.clone(),
                live,
                selected: self.connection.selected,
                confirm: self.connection.confirm,
                result: self.connection.last_result.clone(),
                socket_line,
            }
        } else {
            ConnectionFrame::default()
        };

        let onboarding_frame = if let Some(opened) = self.onboarding_opened_at {
            let e = opened.elapsed().as_millis() as u64;
            OnboardingFrame {
                open: true,
                rows: self.onboarding_ui.rows.clone(),
                selected: self.onboarding_ui.selected,
                elapsed_ms: e,
                dim: welcome::dim_opening(e),
            }
        } else if let Some((closing, from)) = self.onboarding_closing_at {
            match welcome::dim_closing(from, closing.elapsed().as_millis() as u64) {
                Some(dim) => OnboardingFrame {
                    dim,
                    ..Default::default()
                },
                None => {
                    self.onboarding_closing_at = None;
                    OnboardingFrame::default()
                }
            }
        } else {
            OnboardingFrame::default()
        };

        RenderFrames {
            theme_picker: self.theme_picker,
            version_popup: VersionPopupFrame {
                open: self.version_popup_open,
                upgraded: self.notices.upgraded,
                notices: Arc::clone(&self.notices.shown),
            },
            help_open: self.help_open,
            source_warning,
            dashboard: dashboard_frame,
            connection: connection_frame,
            onboarding: onboarding_frame,
        }
    }

    /// The Sources panel's cached rows carry a per-source HEALTH summary that
    /// scans the retained logs, so read them fresh at each (infrequent) rebuild.
    pub(crate) fn read_conn_log(&self) -> String {
        self.log.as_ref().map(|at| at.read().0).unwrap_or_default()
    }
}

#[cfg(test)]
impl UiState {
    /// A run an upgrade from `previous` to `current` booted, with its popup
    /// closed.
    pub(crate) fn upgraded_from(previous: &str, current: &str) -> Self {
        let mut ui = Self::new(
            &theme::NORMAL,
            WelcomeUi::from_detected(&[]),
            std::path::PathBuf::new(),
            None,
            crate::doctor::DriftSeen::default(),
        );
        let decision = crate::version::boot_decision(current, Some(previous));
        ui.notices.versions = decision.versions;
        ui.notices.upgraded = decision.should_show_popup;
        ui
    }
}

/// Whether this version's popup shows, and the version before it, stamping
/// `current_ver` as `last_seen_version` in the config at `config_path` at boot, so the popup
/// shows at most once per upgrade however the run exits. The config's warnings
/// are dropped: `main`'s pre-altscreen pass already surfaced them.
fn resolve_version_popup(
    config_path: &std::path::Path,
    current_ver: &str,
) -> crate::version::BootDecision {
    let cfg = crate::config::load(config_path, &mut Vec::new());
    let decision = crate::version::boot_decision(current_ver, cfg.last_seen_version.as_deref());
    if decision.should_persist
        && let Err(e) = crate::config::save_version(config_path, current_ver)
    {
        tracing::warn!(error = ?e, "failed to persist version");
    }
    decision
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_scene::theme::ALL_THEMES;

    fn ui() -> UiState {
        UiState::new(
            ALL_THEMES[0],
            WelcomeUi::from_detected(&[]),
            std::path::PathBuf::from("/tmp/sock"),
            None,
            crate::doctor::DriftSeen::default(),
        )
    }

    /// A fake instant past what `SystemTime` holds takes the real clock, as an
    /// unparseable one does, not a panic.
    #[test]
    fn a_fake_clock_past_the_end_of_time_is_the_real_clock() {
        temp_env::with_var("PIXTUOID_FAKE_NOW", Some(u64::MAX.to_string()), || {
            assert_eq!(fake_now(), None);
        });
        temp_env::with_var("PIXTUOID_FAKE_NOW", Some("tomorrow"), || {
            assert_eq!(fake_now(), None);
        });
    }

    /// `$PIXTUOID_FAKE_NOW`'s clock runs on from its instant, and holds still
    /// while paused, as the wall clock does: pace-check's sky rests on both.
    #[test]
    fn a_fake_clock_runs_from_its_instant_and_holds_when_paused() {
        let mut ui = ui();
        let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        ui.fake_now = Some((Instant::now(), at));
        let first = ui.now();
        assert!(first >= at && first < at + std::time::Duration::from_secs(60));
        std::thread::sleep(std::time::Duration::from_millis(5));
        let later = ui.now();
        assert!(later > first, "it advances");
        ui.toggle_pause();
        let held = ui.now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert_eq!(ui.now(), held, "it holds while paused");
        assert!(held >= later);
    }

    #[test]
    fn modal_projection_mirrors_each_surface_open_flag() {
        let mut ui = ui();
        let m = ui.modal();
        assert!(
            !m.onboarding_open
                && !m.help_open
                && !m.version_popup_open
                && m.theme_picker.is_none()
                && !m.dashboard_open
                && !m.connection_open
                && !m.connection_confirm,
            "everything closed at boot (no detected CLIs, no version popup)"
        );

        ui.toggle_help();
        assert!(ui.modal().help_open);
        ui.close_help();
        assert!(!ui.modal().help_open);

        ui.open_theme_picker();
        assert_eq!(ui.modal().theme_picker, Some(ui.saved_theme_idx));
        ui.preview_theme(2);
        assert_eq!(ui.modal().theme_picker, Some(2));
        ui.commit_theme(2);
        assert_eq!(ui.modal().theme_picker, None);
        assert_eq!(ui.saved_theme_idx, 2);

        let scene = SceneState::new([4, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        ui.toggle_dashboard(&scene);
        assert!(ui.modal().dashboard_open);
        ui.close_dashboard();
        assert!(!ui.modal().dashboard_open);

        ui.open_connection(Vec::new());
        assert!(ui.modal().connection_open);
        assert!(!ui.modal().connection_confirm);
        ui.connection.confirm = Some(0);
        assert!(ui.modal().connection_confirm);
        ui.close_connection();
        let m = ui.modal();
        assert!(!m.connection_open && !m.connection_confirm);
    }

    /// The backdrop's dim moves through the open ramp and the close fade, and
    /// only then.
    #[test]
    fn the_dim_moves_through_its_ramps_only() {
        use std::time::Duration;
        let mut ui = UiState::new(
            ALL_THEMES[0],
            WelcomeUi::from_ids(&["codex"]),
            std::path::PathBuf::from("sock"),
            None,
            crate::doctor::DriftSeen::default(),
        );
        assert!(ui.dim_moving(), "the open ramp");
        let ramped = Duration::from_millis(welcome::DIM_RAMP_MS);
        ui.onboarding_opened_at = Instant::now().checked_sub(ramped);
        assert!(!ui.dim_moving(), "dimmed, the card waits");
        ui.close_onboarding();
        assert!(ui.dim_moving(), "the close fade");
        let (_, from) = ui.onboarding_closing_at.expect("the fade is armed");
        let faded = Duration::from_millis(welcome::DIM_FADE_OUT_MS);
        ui.onboarding_closing_at = Instant::now().checked_sub(faded).map(|at| (at, from));
        ui.build_frames(SystemTime::now(), &SceneState::new([0; 10]), &[]);
        assert!(!ui.dim_moving(), "faded back");
        assert!(!self::ui().dim_moving(), "no onboarding");
    }

    #[test]
    fn onboarding_opens_only_with_a_roster_and_closes_for_good() {
        assert!(!ui().modal().onboarding_open);

        let mut ui = UiState::new(
            ALL_THEMES[0],
            WelcomeUi::from_ids(&["codex", "claude-code"]),
            std::path::PathBuf::from("/tmp/sock"),
            None,
            crate::doctor::DriftSeen::default(),
        );
        assert!(ui.modal().onboarding_open, "a roster opens the overlay");
        ui.close_onboarding();
        assert!(!ui.modal().onboarding_open);
        let scene = SceneState::new([4, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let frames = ui.build_frames(SystemTime::now(), &scene, &[]);
        assert!(!frames.onboarding.open, "the card itself is gone");
        // Closed immediately, so the open ramp had barely dimmed.
        assert!(
            frames.onboarding.dim.brightness() > 0.9,
            "an instantly-skipped overlay must not snap the office to the dim \
             floor before fading back, got {:?}",
            frames.onboarding.dim
        );
    }

    #[test]
    fn pause_freezes_the_clock() {
        let mut ui = ui();
        ui.toggle_pause();
        let a = ui.now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert_eq!(a, ui.now(), "paused: the same frozen instant");
        ui.toggle_pause();
        assert_ne!(a, ui.now(), "unpaused: live time again");
    }

    /// The replugged hosts, installed.
    const INSTALLED: [&str; 3] = ["claude-code", "codex", "reasonix"];

    /// A run of `current` booted over the config at `path`, with `welcome`'s
    /// onboarding and nothing connected.
    fn boot_at(path: &std::path::Path, welcome: WelcomeUi, current: &str) -> UiState {
        boot_into(path, welcome, current, &HashSet::new(), false)
    }

    /// [`boot_at`] with `connected` at boot and whether the TUI draws the
    /// cutaway.
    fn boot_into(
        path: &std::path::Path,
        welcome: WelcomeUi,
        current: &str,
        connected: &HashSet<String>,
        terminal_cutaway: bool,
    ) -> UiState {
        UiState::boot_with(
            Boot {
                theme: &theme::NORMAL,
                first_run: false,
                config_path: path,
                socket_path: path.with_extension("sock"),
                log: crate::run_log::LogLocation::File(path.with_extension("log")),
                drift: crate::doctor::DriftSeen::default(),
                connected,
                terminal_cutaway,
            },
            welcome,
            || INSTALLED.to_vec(),
            current,
        )
    }

    /// A version new to the config shows its popup once, and stamps it; an
    /// open onboarding holds the popup back but still stamps.
    #[test]
    fn a_new_version_pops_up_once_and_yields_to_the_onboarding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let boot = |path: &std::path::Path, welcome: WelcomeUi| {
            boot_at(path, welcome, env!("CARGO_PKG_VERSION"))
                .modal()
                .version_popup_open
        };
        let stamped =
            |path: &std::path::Path| crate::config::load(path, &mut Vec::new()).last_seen_version;
        let path = tmp.path().join("config.toml");
        crate::config::save_version(&path, "0.0.1").expect("seed an old version");
        assert!(
            boot(&path, WelcomeUi::from_detected(&[])),
            "a new version pops up"
        );
        assert_eq!(stamped(&path).as_deref(), Some(env!("CARGO_PKG_VERSION")));
        assert!(!boot(&path, WelcomeUi::from_detected(&[])), "only once");

        let path = tmp.path().join("onboarding.toml");
        crate::config::save_version(&path, "0.0.1").expect("seed an old version");
        assert!(!boot(&path, WelcomeUi::from_ids(&["codex"])));
        assert_eq!(stamped(&path).as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }

    /// The popup an upgrade boots with lists the notices due from the version
    /// the config recorded to the one running, against the boot's connections,
    /// the CLIs installed and the TUI's cutaway.
    #[test]
    fn an_upgrade_boots_the_popup_with_its_notices() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let across = crate::version::across_every_notice();
        crate::config::save_version(&path, &across.previous.to_string())
            .expect("seed the release before");
        let ui = boot_into(
            &path,
            WelcomeUi::from_detected(&[]),
            &across.current.to_string(),
            &HashSet::from(["codex".to_string()]),
            true,
        );
        assert!(ui.modal().version_popup_open);
        let shown = ui.version_notices();
        assert_eq!(shown.len(), 2, "{shown:?}");
        assert!(
            shown[0].contains("Claude Code and Reasonix as"),
            "the connected Codex is not named: {shown:?}"
        );
        assert!(shown[1].contains("cutaway"), "{shown:?}");
    }

    /// Reopened, the popup lists only what is still due: a host reconnected
    /// since has resolved its notice. Closed, it keeps the notices it showed,
    /// for the frames of its shrink.
    #[test]
    fn the_popup_reopens_with_only_the_notices_still_due() {
        let mut ui = ui();
        ui.notices.versions = Some(crate::version::Versions {
            previous: semver::Version::new(0, 19, 0),
            current: semver::Version::new(0, 20, 0),
        });
        ui.open_version_popup(&HashSet::new(), &["codex"]);
        assert!(ui.modal().version_popup_open);
        let shown = ui.version_notices().to_vec();
        assert!(shown.iter().any(|n| n.contains("Codex")), "{shown:?}");

        ui.dismiss_version_popup();
        let frames = ui.build_frames(SystemTime::now(), &SceneState::new([0; 10]), &[]);
        assert!(!frames.version_popup.open);
        assert_eq!(
            &*frames.version_popup.notices,
            shown.as_slice(),
            "the shrink still shows them"
        );

        let connected = HashSet::from(["codex".to_string()]);
        ui.open_version_popup(&connected, &["codex"]);
        assert!(ui.modal().version_popup_open);
        assert!(
            ui.version_notices().is_empty(),
            "{:?}",
            ui.version_notices()
        );
    }

    /// A press meets the link where the popup paints it below the notices
    /// it holds: one where the link would sit without them is swallowed.
    #[test]
    fn a_press_meets_the_link_below_the_notices() {
        use crate::panels::widgets::version_popup_url_rect;
        let mut ui = ui();
        ui.notices.versions = Some(crate::version::Versions {
            previous: semver::Version::new(0, 19, 0),
            current: semver::Version::new(0, 20, 0),
        });
        ui.open_version_popup(&HashSet::new(), &["codex"]);
        let screen = (120, 44);
        let bounds = ratatui::layout::Rect::new(0, 0, screen.0, screen.1);
        let bare = version_popup_url_rect(bounds, &[], 1.0).expect("a link");
        let held = version_popup_url_rect(bounds, ui.version_notices(), 1.0).expect("a link");
        assert_ne!(bare, held, "the notices move the link");
        assert_eq!(
            crate::panels::modal_mouse(&mut ui, 1.0, Some((bare.x, bare.y)), || Some(screen)),
            crate::panels::ModalMouse::Inert
        );
    }

    /// Only the run an upgrade booted says it updated, even on a `v` after
    /// the onboarding held its popup back; a later run, or a first install,
    /// does not.
    #[test]
    fn only_the_run_an_upgrade_booted_says_it_updated() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let upgraded = |name: &str, seed: Option<&str>, welcome: WelcomeUi| {
            let path = tmp.path().join(name);
            if let Some(seed) = seed {
                crate::config::save_version(&path, seed).expect("seed a version");
            }
            let mut ui = boot_at(&path, welcome, "0.20.0");
            ui.open_version_popup(&HashSet::new(), &[]);
            let frames = ui.build_frames(SystemTime::now(), &SceneState::new([0; 10]), &[]);
            frames.version_popup.upgraded
        };
        assert!(upgraded(
            "up.toml",
            Some("0.19.0"),
            WelcomeUi::from_detected(&[])
        ));
        assert!(
            upgraded("held.toml", Some("0.19.0"), WelcomeUi::from_ids(&["codex"])),
            "the onboarding held the popup back"
        );
        assert!(
            !upgraded("later.toml", Some("0.20.0"), WelcomeUi::from_detected(&[])),
            "a later run"
        );
        assert!(
            !upgraded("fresh.toml", None, WelcomeUi::from_detected(&[])),
            "a first install"
        );
        assert!(
            !upgraded("down.toml", Some("0.21.0"), WelcomeUi::from_detected(&[])),
            "a downgrade"
        );
    }
}
