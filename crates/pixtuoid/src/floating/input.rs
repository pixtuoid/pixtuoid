//! Floating-window keyboard input: a winit key read as the crossterm key the
//! TUI's [`dispatch_key`](crate::panels::dispatch_key) decodes, so both
//! painters take one vocabulary and one precedence of panels.

use crossterm::event::{KeyCode, KeyModifiers};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::geometry::{Zoom, ZoomKey};

use super::compose::Presented;
use super::offscreen::OfficeRenderer;
use pixtuoid_scene::theme::Theme;

/// `key` with `mods` held as the TUI reads it, `repeat` being winit's flag
/// for a held key; `None` for a key it binds nothing to, a held toggle, or a
/// Cmd chord, which a terminal keeps for itself and never hands the TUI.
pub(crate) fn key(
    key: &Key,
    mods: ModifiersState,
    repeat: bool,
) -> Option<(KeyCode, KeyModifiers)> {
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
    if repeat && !repeats(code) {
        return None;
    }
    let mut held = KeyModifiers::NONE;
    if mods.control_key() {
        held |= KeyModifiers::CONTROL;
    }
    if mods.alt_key() {
        held |= KeyModifiers::ALT;
    }
    Some((code, held))
}

/// The modifier a zoom chord is held with: Cmd on macOS, as a terminal
/// zooms, and Ctrl elsewhere. The window reads a zoom chord before [`key`],
/// so off macOS it takes Ctrl `+`/`-` from the volume, which
/// [`dispatch_key`](crate::panels::dispatch_key) reads whatever is held.
const ZOOM_HOLD: ModifiersState = if cfg!(target_os = "macos") {
    ModifiersState::SUPER
} else {
    ModifiersState::CONTROL
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
#[cfg(target_os = "macos")]
pub(crate) const ZOOM_SHORTCUTS: &[crate::panels::widgets::Shortcut] =
    &[("Cmd +/-", "zoom in / out"), ("Cmd 0", "reset zoom")];
#[cfg(not(target_os = "macos"))]
pub(crate) const ZOOM_SHORTCUTS: &[crate::panels::widgets::Shortcut] =
    &[("Ctrl +/-", "zoom in / out"), ("Ctrl 0", "reset zoom")];

/// The zoom a pressed `key` held with `mods` leaves a `size` window at,
/// from the `zoom` it shows; `None` for any other key, which goes on to
/// [`key`].
pub(crate) fn zoomed(
    key: &Key,
    mods: ModifiersState,
    zoom: Zoom,
    size: winit::dpi::PhysicalSize<u32>,
    density: pixtuoid_core::sprite::format::Density,
) -> Option<Zoom> {
    zoom_key(key, mods).map(|step| zoom.stepped(step, size, density))
}

/// The zoom `key` asks for, held with `mods`; `None` for any other key.
fn zoom_key(key: &Key, mods: ModifiersState) -> Option<ZoomKey> {
    zoom_key_with(key, mods, ZOOM_HOLD)
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

/// Whether a held `code` fires again: a step through a list, a floor or the
/// volume may, a toggle must not oscillate (winit flags repeats; a terminal
/// delivers each as a fresh press).
fn repeats(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'k' | 'h' | 'l' | '+' | '=' | '-' | '_')
    )
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

    fn navigate_floor(&mut self, floor: usize, now: std::time::SystemTime) {
        self.renderer.navigate(floor, now);
    }

    fn redraw(&mut self) -> anyhow::Result<()> {
        self.presented.stale();
        Ok(())
    }
}

/// A key read as the TUI's, through its dispatch and panels, the window
/// hosting; whether it asked to quit.
pub(crate) fn press_key(
    (code, mods): (crossterm::event::KeyCode, crossterm::event::KeyModifiers),
    cx: &mut crate::panels::KeyCtx<'_, WindowHost<'_>>,
) -> bool {
    let nav = cx.host.renderer.nav();
    let floor = crate::panels::FloorNav {
        n_floors: cx.host.renderer.n_floors(),
        current_floor: nav.current(),
        in_transition: nav.transition().is_some(),
    };
    let action = crate::panels::dispatch_key(code, mods, cx.ui.modal(), floor);
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
            key(&Key::Character("t".into()), none, false),
            Some((KeyCode::Char('t'), KeyModifiers::NONE))
        );
        assert_eq!(
            key(&Key::Character("c".into()), ModifiersState::CONTROL, false),
            Some((KeyCode::Char('c'), KeyModifiers::CONTROL))
        );
        assert_eq!(
            key(&Key::Character("q".into()), ModifiersState::ALT, false),
            Some((KeyCode::Char('q'), KeyModifiers::ALT))
        );
        assert_eq!(
            key(&Key::Named(NamedKey::Tab), none, false),
            Some((KeyCode::Tab, KeyModifiers::NONE))
        );
        assert_eq!(
            key(&Key::Named(NamedKey::Space), none, false),
            Some((KeyCode::Char(' '), KeyModifiers::NONE))
        );
        assert_eq!(key(&Key::Named(NamedKey::F1), none, false), None);
        assert_eq!(
            key(&Key::Character("q".into()), ModifiersState::SUPER, false),
            None,
            "Cmd+Q is the platform's, never the TUI's q"
        );
    }

    /// A held toggle fires once; a held step keeps stepping.
    #[test]
    fn only_steps_repeat() {
        let none = ModifiersState::empty();
        for k in [
            Key::Character("j".into()),
            Key::Named(NamedKey::ArrowDown),
            Key::Character("+".into()),
        ] {
            assert_eq!(key(&k, none, true), key(&k, none, false), "{k:?}");
            assert!(key(&k, none, true).is_some(), "{k:?}");
        }
        for k in [
            Key::Character("p".into()),
            Key::Character("m".into()),
            Key::Character("t".into()),
            Key::Named(NamedKey::Tab),
            Key::Named(NamedKey::Enter),
        ] {
            assert!(key(&k, none, false).is_some(), "{k:?}");
            assert_eq!(key(&k, none, true), None, "held {k:?}");
        }
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
        _tmp: tempfile::TempDir,
    }

    /// Stands in for `crate::audio::respawn`, which opens a real output device.
    fn no_respawn(_: &crate::audio::AudioHandle, _: f32) {}

    impl Keys {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let config_path = tmp.path().join("config.toml");
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
                    config_path.clone(),
                    no_respawn,
                ),
                connected: crate::runtime::ConnectedSources::default(),
                snapshot: SceneState::uniform(4),
                config_path,
                _tmp: tmp,
            }
        }

        /// Press `code`; whether it quit.
        fn press(&mut self, code: crossterm::event::KeyCode) -> bool {
            press_key(
                (code, crossterm::event::KeyModifiers::NONE),
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
                    installed: Vec::new,
                },
            )
        }

        fn saved_theme(&self) -> Option<String> {
            crate::config::load(&self.config_path, &mut Vec::new()).theme
        }
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
        k.ui.open_version_popup(&std::collections::HashSet::new(), &[]);
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
        assert!(k.ui.theme_picker.is_some(), "t opens the picker");
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
        k.config_path = blocker.join("config.toml");
        k.press(KeyCode::Char('t'));
        k.press(KeyCode::Char('j'));
        assert!(!k.press(KeyCode::Enter));
        assert!(!std::ptr::eq(k.theme, pixtuoid_scene::theme::ALL_THEMES[0]));
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
            zoomed(&char_key("="), ZOOM_HOLD, Zoom::default(), size, d),
            Some(Zoom::default().stepped(ZoomKey::In, size, d))
        );
        assert_ne!(
            zoomed(&char_key("="), ZOOM_HOLD, Zoom::default(), size, d),
            Some(Zoom::default()),
            "a step, not the zoom it started from"
        );
        assert_eq!(
            zoomed(&char_key("0"), ZOOM_HOLD, Zoom::new(3), size, d),
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

    /// The help names each zoom's first key and the modifier a chord is held
    /// with, so a change to one and not the other reds here.
    #[test]
    fn every_zoom_chord_is_in_the_help() {
        let keys: String = ZOOM_SHORTCUTS.iter().map(|(k, _)| *k).collect();
        for zoom in [ZoomKey::In, ZoomKey::Out, ZoomKey::Reset] {
            let (named, _) = ZOOM_CHORDS
                .iter()
                .find(|(_, z)| *z == zoom)
                .expect("every zoom has a chord");
            assert!(
                keys.contains(named),
                "{named} missing from the help's {keys:?}"
            );
        }
        let held = if ZOOM_HOLD == ModifiersState::SUPER {
            "Cmd "
        } else {
            "Ctrl "
        };
        for (k, _) in ZOOM_SHORTCUTS {
            assert!(k.starts_with(held), "{k:?} names another modifier");
        }
        for (c, zoom) in ZOOM_CHORDS {
            let hold = ZOOM_HOLD;
            assert_eq!(
                zoom_key_with(&Key::Character(c.into()), hold, hold),
                Some(zoom)
            );
        }
    }
}
