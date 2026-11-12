//! Floating-window keyboard input: a winit key read as the crossterm key the
//! TUI's [`dispatch_key`](crate::panels::dispatch_key) decodes, so both
//! painters take one vocabulary and one precedence of panels.

use crossterm::event::{KeyCode, KeyModifiers};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::geometry::{Zoom, ZoomKey};

use super::compose::Presented;
use super::offscreen::OfficeRenderer;
use crate::panels::KeyAction;
use pixtuoid_scene::theme::Theme;

/// `key` with `mods` held as the TUI reads it; `None` for a key it binds
/// nothing to, or a Cmd chord, which a terminal keeps for itself and never
/// hands the TUI.
pub(crate) fn key(key: &Key, mods: ModifiersState) -> Option<(KeyCode, KeyModifiers)> {
    if mods.super_key() {
        return None;
    }
    let code = match key {
        Key::Named(named) => match named {
            NamedKey::Enter => KeyCode::Enter,
            NamedKey::Escape => KeyCode::Esc,
            NamedKey::Tab => KeyCode::Tab,
            NamedKey::Space => KeyCode::Char(' '),
            NamedKey::ArrowUp => KeyCode::Up,
            NamedKey::ArrowDown => KeyCode::Down,
            NamedKey::ArrowLeft => KeyCode::Left,
            NamedKey::ArrowRight => KeyCode::Right,
            NamedKey::PageUp => KeyCode::PageUp,
            NamedKey::PageDown => KeyCode::PageDown,
            _ => return None,
        },
        Key::Character(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return None,
            }
        }
        _ => return None,
    };
    let mut held = KeyModifiers::NONE;
    if mods.control_key() {
        held |= KeyModifiers::CONTROL;
    }
    if mods.alt_key() {
        held |= KeyModifiers::ALT;
    }
    Some((code, held))
}

/// The modifier a zoom chord is held with, and the help's name for it: Cmd on
/// macOS, as a terminal zooms, and Ctrl elsewhere. The window reads a zoom
/// chord before [`key`], so off macOS it takes Ctrl `+`/`-` from the volume,
/// which [`dispatch_key`](crate::panels::dispatch_key) reads whatever is held.
const ZOOM_HOLD: (ModifiersState, &str) = if cfg!(target_os = "macos") {
    (ModifiersState::SUPER, "Cmd")
} else {
    (ModifiersState::CONTROL, "Ctrl")
};

/// The key each zoom chord takes. A zoom's first key is the one the help
/// names; `=` and `_` are `+` and `-` typed without and with Shift.
const ZOOM_CHORDS: [(&str, ZoomKey); 5] = [
    ("+", ZoomKey::In),
    ("=", ZoomKey::In),
    ("-", ZoomKey::Out),
    ("_", ZoomKey::Out),
    ("0", ZoomKey::Reset),
];

/// The zoom chords, as the help lists them.
pub(crate) static ZOOM_SHORTCUTS: std::sync::LazyLock<[crate::panels::widgets::Shortcut; 2]> =
    std::sync::LazyLock::new(|| {
        use crate::panels::widgets::shortcut;
        let first = |zoom| {
            ZOOM_CHORDS
                .iter()
                .find(|&&(_, z)| z == zoom)
                .map_or("", |&(key, _)| key)
        };
        let hold = ZOOM_HOLD.1;
        [
            shortcut(
                format!("{hold} {}/{}", first(ZoomKey::In), first(ZoomKey::Out)),
                "zoom in / out",
            ),
            shortcut(format!("{hold} {}", first(ZoomKey::Reset)), "reset zoom"),
        ]
    });

/// The zoom a pressed `key` held with `mods` leaves a `size` window at,
/// from the `zoom` it shows; `None` for any other key, which goes on to
/// [`key`].
pub(crate) fn zoomed(
    key: &Key,
    mods: ModifiersState,
    zoom: Zoom,
    size: winit::dpi::PhysicalSize<u32>,
    density: pixtuoid_scene::sprite::format::Density,
) -> Option<Zoom> {
    zoom_key(key, mods).map(|step| zoom.stepped(step, size, density))
}

/// The zoom `key` asks for, held with `mods`; `None` for any other key.
fn zoom_key(key: &Key, mods: ModifiersState) -> Option<ZoomKey> {
    zoom_key_with(key, mods, ZOOM_HOLD.0)
}

/// [`zoom_key`] with `hold` as the zoom modifier.
fn zoom_key_with(key: &Key, mods: ModifiersState, hold: ModifiersState) -> Option<ZoomKey> {
    // Shift is how `+` and `_` are typed, never a different chord.
    if mods.difference(ModifiersState::SHIFT) != hold {
        return None;
    }
    let Key::Character(c) = key else {
        return None;
    };
    ZOOM_CHORDS
        .iter()
        .find(|(chord, _)| *chord == c.as_str())
        .map(|&(_, zoom)| zoom)
}

/// Whether a held key's `action` fires again: a step through a list, a floor
/// or the volume may, a toggle must not oscillate (winit flags repeats; a
/// terminal delivers each as a fresh press). No `_` arm, so a new action
/// declares itself.
fn repeats(action: KeyAction) -> bool {
    match action {
        KeyAction::ThemePreview(_)
        | KeyAction::LookPreview(_)
        | KeyAction::NavigateFloor(_)
        | KeyAction::AdjustVolume(_)
        | KeyAction::DashboardUp
        | KeyAction::DashboardDown
        | KeyAction::DashboardFoldLeft
        | KeyAction::DashboardFoldRight
        | KeyAction::ConnectionUp
        | KeyAction::ConnectionDown
        | KeyAction::OnboardingUp
        | KeyAction::OnboardingDown
        // Idempotent: a held Ctrl-L only repaints again.
        | KeyAction::Redraw => true,
        KeyAction::None
        | KeyAction::Quit
        | KeyAction::TogglePause
        | KeyAction::ToggleHelp
        | KeyAction::CloseHelp
        | KeyAction::DismissVersionPopup
        | KeyAction::OpenVersionPopup
        | KeyAction::OpenAppearance
        | KeyAction::AppearanceCommit
        | KeyAction::AppearanceCancel
        | KeyAction::ToggleAudioMute
        | KeyAction::ToggleDashboard
        | KeyAction::DashboardFoldAll
        | KeyAction::DashboardJump
        | KeyAction::DashboardFocus
        | KeyAction::DashboardClose
        | KeyAction::ToggleConnection
        | KeyAction::ConnectionToggle
        | KeyAction::ConnectionConfirm
        | KeyAction::ConnectionCancelConfirm
        | KeyAction::ConnectionClose
        | KeyAction::OnboardingToggle
        | KeyAction::OnboardingConfirm
        | KeyAction::OnboardingSkip => false,
    }
}

/// What an applied key changes in the window: its theme, its floor and its
/// next frame.
pub(crate) struct WindowHost<'a> {
    pub(crate) renderer: &'a mut OfficeRenderer,
    pub(crate) theme: &'a mut &'static Theme,
    pub(crate) presented: &'a mut Presented,
}

impl crate::panels::Host for WindowHost<'_> {
    fn set_theme(&mut self, theme: &'static Theme) {
        *self.theme = theme;
    }

    /// Never asked: the window draws the cutaway alone, so its picker has
    /// no look row.
    fn set_look(&mut self, _: pixtuoid_scene::look::LookKind) -> anyhow::Result<()> {
        Ok(())
    }

    fn navigate_floor(&mut self, floor: usize, now: std::time::SystemTime) {
        self.renderer.navigate(floor, now);
    }

    fn redraw(&mut self) -> anyhow::Result<()> {
        self.presented.stale();
        Ok(())
    }
}

/// A key read as the TUI's, through its dispatch and panels, the window
/// hosting; whether it asked to quit. A `repeat`, winit's flag for a held
/// key, applies only an action that [`repeats`]: the dispatch is pure, so
/// one decoded and dropped changes nothing.
pub(crate) fn press_key(
    (code, mods): (KeyCode, KeyModifiers),
    repeat: bool,
    cx: &mut crate::panels::KeyCtx<'_, WindowHost<'_>>,
) -> bool {
    let nav = cx.host.renderer.nav();
    let floor = crate::panels::FloorNav {
        n_floors: cx.host.renderer.n_floors(),
        current_floor: nav.current(),
        in_transition: nav.transition().is_some(),
    };
    let action = crate::panels::dispatch_key(code, mods, cx.ui.modal(), floor);
    if repeat && !repeats(action) {
        return false;
    }
    crate::panels::apply_key_action(action, cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_core::state::SceneState;
    use std::time::SystemTime;

    /// The window's keys reach the TUI's dispatch as its own: characters as
    /// typed, the named keys it binds, and Ctrl held through.
    #[test]
    fn a_window_key_reads_as_the_tuis() {
        let none = ModifiersState::empty();
        assert_eq!(
            key(&Key::Character("t".into()), none),
            Some((KeyCode::Char('t'), KeyModifiers::NONE))
        );
        assert_eq!(
            key(&Key::Character("c".into()), ModifiersState::CONTROL),
            Some((KeyCode::Char('c'), KeyModifiers::CONTROL))
        );
        assert_eq!(
            key(&Key::Character("q".into()), ModifiersState::ALT),
            Some((KeyCode::Char('q'), KeyModifiers::ALT))
        );
        assert_eq!(
            key(&Key::Named(NamedKey::Tab), none),
            Some((KeyCode::Tab, KeyModifiers::NONE))
        );
        assert_eq!(
            key(&Key::Named(NamedKey::Space), none),
            Some((KeyCode::Char(' '), KeyModifiers::NONE))
        );
        assert_eq!(key(&Key::Named(NamedKey::F1), none), None);
        assert_eq!(
            key(&Key::Character("q".into()), ModifiersState::SUPER),
            None,
            "Cmd+Q is the platform's, never the TUI's q"
        );
    }

    /// The window's keys through the TUI's dispatch, the window hosting.
    struct Keys {
        ui: crate::panels::ui_state::UiState,
        renderer: OfficeRenderer,
        theme: &'static Theme,
        presented: Presented,
        audio_ctl: crate::audio::AudioController,
        connected: crate::runtime::ConnectedSources,
        snapshot: SceneState,
        config_path: std::path::PathBuf,
        saves: crate::panels::Saves,
        landed: tokio::sync::mpsc::UnboundedReceiver<crate::panels::Landed>,
        _tmp: tempfile::TempDir,
    }

    /// Stands in for `crate::audio::respawn`, which opens a real output device.
    fn no_respawn(_: &crate::audio::AudioHandle, _: f32) {}

    impl Keys {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let config_path = tmp.path().join("config.toml");
            let (landing, landed) = tokio::sync::mpsc::unbounded_channel();
            let saves =
                crate::panels::Saves::spawn(config_path.clone(), landing).expect("a save thread");
            let theme = pixtuoid_scene::theme::ALL_THEMES[0];
            Self {
                ui: crate::panels::ui_state::UiState::new(
                    theme,
                    crate::panels::welcome::WelcomeUi::from_detected(&[]),
                    tmp.path().join("sock"),
                    None,
                    crate::doctor::DriftSeen::default(),
                ),
                renderer: OfficeRenderer::new(std::sync::Arc::new(
                    pixtuoid_scene::pack::load_bundled_pack().expect("bundled pack"),
                )),
                theme,
                presented: Presented::new(true),
                audio_ctl: crate::audio::AudioController::new_with(
                    crate::config::AudioConfig {
                        muted: true,
                        volume: 1.0,
                    },
                    saves.clone(),
                    no_respawn,
                ),
                connected: crate::runtime::ConnectedSources::default(),
                snapshot: {
                    use super::super::fixtures::{running_on, scene_with};
                    // Two floors, and a root with a subagent: what a floor
                    // step, a dashboard step and a fold each move.
                    let root = running_on("/p/root.jsonl", 0, 0);
                    let child = pixtuoid_core::state::AgentSlot {
                        parent_id: Some(root.agent_id),
                        ..running_on("/p/child.jsonl", 0, 1)
                    };
                    scene_with(vec![root, child, running_on("/p/up.jsonl", 1, 0)], 4)
                },
                config_path,
                saves,
                landed,
                _tmp: tmp,
            }
        }

        /// The office as the window's next frame leaves it: the scene's floors
        /// counted, and a slide the keys started (at the epoch) landed.
        fn settle(&mut self) {
            const PAST_ANY_SLIDE: std::time::Duration = std::time::Duration::from_secs(60);
            let conditions = pixtuoid_scene::floor::Conditions::default();
            (self.renderer).hear(
                &self.snapshot,
                conditions,
                SystemTime::UNIX_EPOCH + PAST_ANY_SLIDE,
            );
        }

        /// Press `code`; whether it quit.
        fn press(&mut self, code: crossterm::event::KeyCode) -> bool {
            self.send(code, false)
        }

        /// A repeat of `code` held down; whether it quit.
        fn hold(&mut self, code: crossterm::event::KeyCode) -> bool {
            self.send(code, true)
        }

        fn send(&mut self, code: crossterm::event::KeyCode, repeat: bool) -> bool {
            let quit = press_key(
                (code, crossterm::event::KeyModifiers::NONE),
                repeat,
                &mut crate::panels::KeyCtx {
                    ui: &mut self.ui,
                    host: &mut WindowHost {
                        renderer: &mut self.renderer,
                        theme: &mut self.theme,
                        presented: &mut self.presented,
                    },
                    audio_ctl: &mut self.audio_ctl,
                    config_path: &self.config_path,
                    connected: &self.connected,
                    snapshot: &self.snapshot,
                    now: SystemTime::UNIX_EPOCH,
                    respawn: no_respawn,
                    saves: &self.saves,
                    focus: |_| panic!("a window key focuses nothing"),
                    job_tx: &tokio::sync::mpsc::unbounded_channel().0,
                    run_job: |_| panic!("a window key here changes no source"),
                },
            );
            // The stand-in panics on its own thread, which fails no test.
            assert!(
                self.ui.running_job.is_none(),
                "a window key here starts no job"
            );
            quit
        }

        /// Take in what the saves made so far came to, as the loop does.
        fn land_saves(&mut self) {
            for outcome in crate::panels::noted::saved(&self.saves, &mut self.landed) {
                self.ui.note_save(outcome);
            }
        }

        /// The keys' saves go to the config at `path` from now on.
        fn config_at(&mut self, path: std::path::PathBuf) {
            let (landing, landed) = tokio::sync::mpsc::unbounded_channel();
            self.saves = crate::panels::Saves::spawn(path.clone(), landing).expect("a save thread");
            (self.config_path, self.landed) = (path, landed);
        }

        fn saved_theme(&mut self) -> Option<String> {
            self.land_saves();
            crate::config::load(&self.config_path, &mut Vec::new()).theme
        }
    }

    /// A held step keeps stepping; a held toggle fires once.
    #[test]
    fn only_a_held_step_repeats() {
        use crate::panels::keys::{APPEARANCE, DASHBOARD, MUTE, PAUSE, VOLUME_DOWN, VOLUME_UP};
        use crossterm::event::KeyCode;
        let mut k = Keys::new();
        for toggle in [PAUSE, APPEARANCE, DASHBOARD] {
            assert!(!k.hold(toggle), "{toggle:?}");
        }
        assert!(!k.ui.paused(), "a held pause toggled");
        assert!(k.ui.appearance.is_none(), "a held t opened the picker");
        assert!(
            !k.ui.modal().dashboard_open,
            "a held Tab opened the dashboard"
        );

        // The harness starts muted at full volume, so down comes first: up
        // then both raises the volume and unmutes.
        k.hold(VOLUME_DOWN);
        let lowered = k.audio_ctl.handle().volume();
        assert!(lowered < 1.0, "a held volume-down stopped");
        k.hold(VOLUME_UP);
        assert!(
            k.audio_ctl.handle().volume() > lowered,
            "a held volume-up stopped"
        );
        assert!(!k.audio_ctl.handle().is_muted());
        k.hold(MUTE);
        assert!(!k.audio_ctl.handle().is_muted(), "a held mute toggled");

        k.press(APPEARANCE);
        k.hold(KeyCode::Down);
        assert!(
            !std::ptr::eq(k.theme, pixtuoid_scene::theme::ALL_THEMES[0]),
            "a held step stopped"
        );
        k.hold(KeyCode::Enter);
        assert!(k.ui.appearance.is_some(), "a held Enter committed the pick");
        assert_eq!(k.saved_theme(), None, "a held Enter saved the theme");
    }

    #[test]
    fn a_held_floor_step_repeats_both_ways() {
        use crossterm::event::KeyCode::Char;
        let mut k = Keys::new();
        k.settle();
        assert_eq!(k.renderer.n_floors(), 2);
        k.hold(Char('k'));
        k.settle();
        assert_eq!(k.renderer.nav().current(), 1, "a held k stopped");
        k.hold(Char('j'));
        k.settle();
        assert_eq!(k.renderer.nav().current(), 0, "a held j stopped");
    }

    #[test]
    fn a_held_list_step_repeats_both_ways_in_each_panel() {
        use crossterm::event::KeyCode::{Down, Up};
        let mut k = Keys::new();
        k.ui.open_connection(crate::panels::noted::rows(|_| false));
        k.hold(Down);
        assert_eq!(
            k.ui.connection.selected, 1,
            "a held Down stopped in Sources"
        );
        k.hold(Up);
        assert_eq!(k.ui.connection.selected, 0, "a held Up stopped in Sources");
        k.ui.close_connection();

        k.ui.toggle_dashboard(&k.snapshot);
        let top = k.ui.dashboard.selected;
        k.hold(Down);
        assert_ne!(
            k.ui.dashboard.selected, top,
            "a held Down stopped in the dashboard"
        );
        k.hold(Up);
        assert_eq!(
            k.ui.dashboard.selected, top,
            "a held Up stopped in the dashboard"
        );
        k.ui.close_dashboard();

        k.ui = crate::panels::ui_state::UiState::new(
            k.theme,
            crate::panels::welcome::WelcomeUi::from_ids(&["codex", "cursor"]),
            k._tmp.path().join("sock"),
            None,
            crate::doctor::DriftSeen::default(),
        );
        k.hold(Down);
        assert_eq!(
            k.ui.onboarding_ui.selected, 1,
            "a held Down stopped in the onboarding"
        );
        k.hold(Up);
        assert_eq!(
            k.ui.onboarding_ui.selected, 0,
            "a held Up stopped in the onboarding"
        );
    }

    #[test]
    fn a_held_dashboard_fold_repeats_both_ways() {
        use crossterm::event::KeyCode::{Left, Right};
        let mut k = Keys::new();
        k.ui.toggle_dashboard(&k.snapshot);
        let rows = |k: &Keys| {
            crate::panels::dashboard::build_dashboard_rows(&k.snapshot, &k.ui.dashboard.folds).len()
        };
        let unfolded = rows(&k);
        k.hold(Left);
        assert!(rows(&k) < unfolded, "a held Left left the root unfolded");
        k.hold(Right);
        assert_eq!(rows(&k), unfolded, "a held Right left the root folded");
    }

    /// Esc closes the version popup, as the help's "Enter / Esc" row says; it
    /// never quits.
    #[test]
    fn esc_dismisses_the_version_popup() {
        use crossterm::event::KeyCode;
        let mut k = Keys::new();
        k.ui = crate::panels::ui_state::UiState::new(
            pixtuoid_scene::theme::ALL_THEMES[0],
            crate::panels::welcome::WelcomeUi::from_detected(&[]),
            k._tmp.path().join("sock"),
            None,
            crate::doctor::DriftSeen::default(),
        );
        k.ui.open_version_popup(&std::collections::HashSet::new());
        assert!(k.ui.modal().version_popup_open);
        assert!(!k.press(KeyCode::Esc), "Esc quit");
        assert!(!k.ui.modal().version_popup_open, "the popup is still up");
    }

    /// A previewed theme shows in the window at once, and only a commit
    /// saves it: quitting or cancelling mid-preview leaves the config as it
    /// was.
    #[test]
    fn only_a_committed_theme_is_saved() {
        use crossterm::event::KeyCode;
        let first = pixtuoid_scene::theme::ALL_THEMES[0];
        let mut k = Keys::new();
        assert!(!k.press(KeyCode::Char('t')));
        assert!(k.ui.appearance.is_some(), "t opens the picker");
        assert!(!k.press(KeyCode::Char('j')));
        assert!(!std::ptr::eq(k.theme, first), "j previews in the window");
        assert!(k.press(KeyCode::Char('q')), "q quits mid-preview");
        assert_eq!(k.saved_theme(), None);

        let mut k = Keys::new();
        k.press(KeyCode::Char('t'));
        k.press(KeyCode::Char('j'));
        assert!(
            !k.press(KeyCode::Esc),
            "Esc closes the picker, not the window"
        );
        assert!(std::ptr::eq(k.theme, first), "and restores the theme");
        assert_eq!(k.saved_theme(), None);

        k.press(KeyCode::Char('t'));
        k.press(KeyCode::Char('j'));
        assert!(!k.press(KeyCode::Enter));
        assert_eq!(k.saved_theme().as_deref(), Some(k.theme.name));
    }

    /// An unwritable config costs only the save: the committed theme still
    /// shows.
    #[test]
    fn an_unwritable_config_keeps_the_committed_theme() {
        use crossterm::event::KeyCode;
        let mut k = Keys::new();
        let blocker = k._tmp.path().join("missing");
        std::fs::write(&blocker, "a file, not a dir").expect("write");
        k.config_at(blocker.join("config.toml"));
        k.press(KeyCode::Char('t'));
        k.press(KeyCode::Char('j'));
        assert!(!k.press(KeyCode::Enter));
        assert!(!std::ptr::eq(k.theme, pixtuoid_scene::theme::ALL_THEMES[0]));
    }

    /// A refused save reaches `RenderFrames.warning` and the footer tier, within
    /// the window's budget and never at the cost of `[q]`.
    #[test]
    fn a_refused_save_shows_in_the_windows_footer() {
        use crossterm::event::KeyCode;
        let mut k = Keys::new();
        std::fs::write(&k.config_path, "not = [valid").expect("write");
        for code in [
            KeyCode::Char('t'),
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Char('m'),
        ] {
            assert!(!k.press(code));
        }
        k.land_saves();
        let frames = k.ui.build_frames(SystemTime::UNIX_EPOCH, &k.snapshot, &[]);
        let at = super::super::geometry::window_geometry(
            winit::dpi::PhysicalSize::new(960, 640),
            crate::test_flash::pack_arc().max_density_variant(),
            Zoom::default(),
        );
        let budget =
            super::super::overlays::footer_budget(960, pixtuoid_scene::cutaway::chrome_cell(at));
        let footer = k
            .renderer
            .footer(&k.snapshot, budget, true, None, frames.warning.as_deref())
            .text();
        assert!(footer.contains("appearance, mute not saved"), "{footer}");
        assert!(footer.contains("[q]"), "{footer}");
    }

    /// Esc closes an open panel before it quits the window.
    #[test]
    fn esc_closes_a_panel_before_it_quits() {
        use crossterm::event::KeyCode;
        let mut k = Keys::new();
        assert!(!k.press(KeyCode::Char('?')));
        assert!(k.ui.help_open());
        assert!(!k.press(KeyCode::Esc));
        assert!(!k.ui.help_open());
        assert!(k.press(KeyCode::Esc), "with nothing open Esc quits");
    }

    /// A zoom chord is `=`/`+` in, `-`/`_` out and `0` back to none, held
    /// with the platform's zoom modifier and nothing else but Shift, which
    /// `+` and `_` need.
    #[test]
    fn a_zoom_chord_is_its_modifier_and_a_zoom_key() {
        use super::super::geometry::ZoomKey;
        let char_key = |c: &str| Key::Character(c.into());
        for hold in [ModifiersState::SUPER, ModifiersState::CONTROL] {
            let shifted = hold | ModifiersState::SHIFT;
            assert_eq!(zoom_key_with(&char_key("="), hold, hold), Some(ZoomKey::In));
            assert_eq!(
                zoom_key_with(&char_key("+"), shifted, hold),
                Some(ZoomKey::In)
            );
            assert_eq!(
                zoom_key_with(&char_key("-"), hold, hold),
                Some(ZoomKey::Out)
            );
            assert_eq!(
                zoom_key_with(&char_key("_"), shifted, hold),
                Some(ZoomKey::Out)
            );
            assert_eq!(
                zoom_key_with(&char_key("0"), hold, hold),
                Some(ZoomKey::Reset)
            );
            assert_eq!(
                zoom_key_with(&char_key("="), ModifiersState::empty(), hold),
                None,
                "plain = is volume"
            );
            assert_eq!(
                zoom_key_with(&char_key("="), hold | ModifiersState::ALT, hold),
                None
            );
            assert_eq!(zoom_key_with(&char_key("a"), hold, hold), None);
            assert_eq!(
                zoom_key_with(&Key::Named(NamedKey::Enter), hold, hold),
                None
            );
        }
        assert_eq!(
            zoom_key_with(
                &char_key("="),
                ModifiersState::CONTROL,
                ModifiersState::SUPER
            ),
            None,
            "the other platform's modifier"
        );
    }

    /// A zoom chord leaves the window at the zoom one step from the one it
    /// shows, and anything else is no zoom and goes on to [`key`].
    #[test]
    fn a_zoom_chord_steps_the_zoom_and_nothing_else_does() {
        use super::super::geometry::Zoom;
        let d = super::super::fixtures::density();
        let size = winit::dpi::PhysicalSize::new(3840, 2160);
        let char_key = |c: &str| Key::Character(c.into());
        assert_eq!(
            zoomed(&char_key("="), ZOOM_HOLD.0, Zoom::default(), size, d),
            Some(Zoom::default().stepped(ZoomKey::In, size, d))
        );
        assert_ne!(
            zoomed(&char_key("="), ZOOM_HOLD.0, Zoom::default(), size, d),
            Some(Zoom::default()),
            "a step, not the zoom it started from"
        );
        assert_eq!(
            zoomed(&char_key("0"), ZOOM_HOLD.0, Zoom::new(3), size, d),
            Some(Zoom::default())
        );
        assert_eq!(
            zoomed(
                &char_key("="),
                ModifiersState::empty(),
                Zoom::default(),
                size,
                d
            ),
            None,
            "plain = goes on to the volume"
        );
    }

    /// Every zoom has a chord for the help to name, and each chord zooms.
    #[test]
    fn every_zoom_has_a_chord_and_each_chord_zooms() {
        for zoom in [ZoomKey::In, ZoomKey::Out, ZoomKey::Reset] {
            assert!(ZOOM_CHORDS.iter().any(|&(_, z)| z == zoom), "{zoom:?}");
        }
        for (c, zoom) in ZOOM_CHORDS {
            let hold = ZOOM_HOLD.0;
            assert_eq!(
                zoom_key_with(&Key::Character(c.into()), hold, hold),
                Some(zoom)
            );
        }
    }
}
