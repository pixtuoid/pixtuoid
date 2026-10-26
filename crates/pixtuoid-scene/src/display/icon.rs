//! The symbols text shows as art rather than as a font's glyph. A pixel
//! painter draws an icon's art from the pack's `[icons]`; a terminal writes
//! its [`Icon::terminal`] glyph, in the cells that glyph takes.

/// What a terminal leads a badge with, in
/// [`BadgeInk::marker`](crate::badge::BadgeInk::marker); a pixel painter draws
/// the plate's [`strip`](super::TextRun::strip) in its place.
pub const BADGE_MARKER: char = '\u{25cf}';

/// A symbol in a line of text, named by what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, enum_map::Enum, strum::VariantArray)]
pub enum Icon {
    /// Agents waiting on you: the board's amber lamp, the footer's alarm.
    Alert,
    /// Agents at work.
    Active,
    /// Agents idle.
    Idle,
    /// An agent waiting: the footer's and the dashboard's rung.
    Waiting,
    /// Agents leaving.
    Exiting,
    /// The board's star, a link to the repo; a model's name.
    Star,
    /// A gateway daemon.
    Gateway,
    /// The floor indicator's arrow to a floor above.
    Up,
    /// Its arrow where no floor is above.
    NoUp,
    /// Its arrow to a floor below.
    Down,
    /// Its arrow where no floor is below.
    NoDown,
    /// A key hint's arrow keys.
    ArrowLeft,
    ArrowUp,
    ArrowRight,
    ArrowDown,
    /// The enter key.
    Enter,
    /// A link out.
    Link,
    /// A subagent, under its parent.
    Child,
    /// More rows than a panel shows.
    More,
    /// A working directory.
    Folder,
    /// A meter's filled step.
    MeterOn,
    /// A meter's empty step.
    MeterOff,
    /// The selected row, or a group to open.
    Pointer,
    /// An open group.
    Fold,
    /// Time spent.
    Clock,
    /// Buying the author a coffee.
    Coffee,
    /// The office's sound.
    Sound,
    /// A warning.
    Warning,
    /// Done, or connected.
    Check,
}

impl Icon {
    /// Its `[icons]` name in the pack: an arrow with no floor its way is the
    /// lit arrow in a dim ink.
    pub(crate) fn art(self) -> &'static str {
        match self {
            Self::Alert => "alert",
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Waiting => "waiting",
            Self::Exiting => "exiting",
            Self::Star => "star",
            Self::Gateway => "gateway",
            Self::Up | Self::NoUp => "floor_up",
            Self::Down | Self::NoDown => "floor_down",
            Self::ArrowLeft => "arrow_left",
            Self::ArrowUp => "arrow_up",
            Self::ArrowRight => "arrow_right",
            Self::ArrowDown => "arrow_down",
            Self::Enter => "enter",
            Self::Link => "link",
            Self::Child => "child",
            Self::More => "more",
            Self::Folder => "folder",
            Self::MeterOn => "meter_on",
            Self::MeterOff => "meter_off",
            Self::Pointer => "pointer",
            Self::Fold => "fold",
            Self::Clock => "clock",
            Self::Coffee => "coffee",
            Self::Sound => "sound",
            Self::Warning => "warning",
            Self::Check => "check",
        }
    }

    /// Whether it is one of the board's status lamps, which a page draws as a
    /// lamp rather than its glyph.
    pub fn is_lamp(self) -> bool {
        matches!(self, Self::Alert | Self::Active | Self::Idle)
    }

    /// What a terminal writes for it: an arrow with no floor its way is
    /// hollow.
    pub(crate) fn terminal(self) -> &'static str {
        match self {
            Self::Alert | Self::Up => "\u{25b2}",
            Self::Active => "\u{25cf}",
            Self::Idle => "\u{25cb}",
            Self::Waiting => "\u{25d0}",
            Self::Exiting => "\u{25cc}",
            Self::Star => "\u{2605}",
            Self::Gateway => "\u{2b22}",
            Self::NoUp => "\u{25b3}",
            Self::Down => "\u{25bc}",
            Self::NoDown => "\u{25bd}",
            Self::ArrowLeft => "\u{2190}",
            Self::ArrowUp => "\u{2191}",
            Self::ArrowRight => "\u{2192}",
            Self::ArrowDown => "\u{2193}",
            Self::Enter => "\u{23ce}",
            Self::Link => "\u{2197}",
            Self::Child => "\u{21b3}",
            Self::More => "\u{22ee}",
            Self::Folder => "\u{25a4}",
            Self::MeterOn => "\u{25ae}",
            Self::MeterOff => "\u{25af}",
            Self::Pointer => "\u{25b8}",
            Self::Fold => "\u{25be}",
            Self::Clock => "\u{25f7}",
            Self::Coffee => "\u{2615}",
            Self::Sound => "\u{2669}",
            Self::Warning => "\u{26a0}",
            Self::Check => "\u{2713}",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::text::{LINE_H, cells, columns};
    use strum::VariantArray;

    /// Each icon's art is a line tall and as wide as its glyph's cells less
    /// the last gap, and the pack draws no icon nothing names.
    #[test]
    fn every_icon_has_its_art_and_every_art_an_icon() {
        let pack = crate::pack::test_office();
        for icon in Icon::VARIANTS {
            let art = pack.icon(*icon).first();
            let w = columns(cells(icon.terminal())).0 - 1;
            assert_eq!((art.width(), art.height()), (w, LINE_H), "{icon:?}");
        }
        let named: std::collections::BTreeSet<&str> =
            Icon::VARIANTS.iter().map(|i| i.art()).collect();
        let drawn: std::collections::BTreeSet<&str> = pack.pack().icon_names().collect();
        assert_eq!(drawn, named);
    }

    /// Each icon's art draws in its text's ink: it would keep the pack's own
    /// colour whatever its text's tone.
    #[test]
    fn every_icon_draws_in_its_texts_ink() {
        let pack = crate::pack::test_office();
        for icon in Icon::VARIANTS {
            let inked = pack
                .icon(*icon)
                .recolorable_at(0)
                .drawn_in(&[crate::pack::ICON_INK_KEY])
                .contains(&true);
            assert!(inked, "{icon:?}");
        }
    }
}
