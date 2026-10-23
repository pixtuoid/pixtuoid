//! Which sprite a piece draws, which frame of it shows, and the palette keys a
//! theme recolours.

use crate::pack::OfficeArt;
use pixtuoid_core::sprite::format::{Density, Piece};
use pixtuoid_core::sprite::{Pixel, Rgb, Sprite};

/// The pack key of a monitor's glass.
pub(crate) const SCREEN_GLASS_KEY: char = 'j';

/// The pack key of the dim content an idle screen shows on its glass.
pub(crate) const SCREEN_TEXT_KEY: char = 'J';

/// The pack key an icon draws in its text's ink.
pub(crate) const ICON_INK_KEY: char = '\u{3b9}';

/// The pack key of a desk lamp's bulb, which glows of its own at any hour.
pub(crate) const DESK_BULB_KEY: char = '9';
/// The monitor's keys in the bundled desks: gen-art's casing, top and stand
/// (`BEZEL`, `SLATE`, `SHADOW`), its glass and its text.
#[cfg(test)]
pub(crate) const MONITOR_KEYS: [char; 5] = ['M', '3', '4', SCREEN_GLASS_KEY, SCREEN_TEXT_KEY];

/// The pack key of the wall clock's face, inside its rim.
pub(crate) const CLOCK_FACE_KEY: char = 'ц';

/// The cooler bottle's fill — theme-independent, so every theme's
/// `tank_water_line` glug bubble must stay distinguishable from it.
pub(crate) const COOLER_WATER: Rgb = Rgb {
    r: 100,
    g: 180,
    b: 230,
};

/// The pack keys the desk props draw their cup's body and shadow in.
pub(super) const CUP_KEY: char = 'V';
pub(super) const CUP_SHADE_KEY: char = '%';
/// The pack keys the token tower and its sheet draw their paper in.
pub(super) const PAPER_KEY: char = '¤';
pub(super) const PAPER_SHADE_KEY: char = '!';

/// The pack keys the desk props take from the theme.
pub(crate) fn desk_prop_overrides(theme: &crate::theme::Theme) -> [(char, Pixel); 4] {
    let f = &theme.furniture;
    [
        (CUP_KEY, Some(f.coffee_cup)),
        (CUP_SHADE_KEY, Some(f.coffee_cup_shadow)),
        (PAPER_KEY, Some(f.paper)),
        (PAPER_SHADE_KEY, Some(f.paper_shade)),
    ]
}

/// The fixtures' [`appliance_overrides`].
pub(crate) fn fixture_overrides(theme: &crate::theme::Theme) -> [(char, Pixel); 16] {
    let (f, o) = (&theme.furniture, &theme.office);
    let [c0, c1, c2] = theme.appliance.coats;
    [
        ('Д', Some(f.tank_water)),
        ('З', Some(f.tank_water_line)),
        ('И', Some(f.tank_fish)),
        ('Л', Some(f.tank_fish_alt)),
        ('Ь', Some(f.tank_plant)),
        ('ж', Some(o.room_wall_trim_dark)),
        ('б', Some(o.building_light)),
        ('ы', Some(f.magazine)),
        ('э', Some(f.magazine_trim)),
        ('ч', Some(c0)),
        ('ш', Some(c1)),
        ('щ', Some(c2)),
        ('ф', Some(o.clock_rim)),
        (CLOCK_FACE_KEY, Some(o.clock_face)),
        ('з', Some(o.clock_hand)),
        ('χ', Some(COOLER_WATER)),
    ]
}

/// The pack keys a corridor appliance's art is drawn in, each with the
/// [`ApplianceColors`](crate::theme::ApplianceColors) role it takes: the art
/// owns the form, the theme the palette. The pack's `[ramps]` of these keys are
/// the shading, re-derived by the recolour.
pub(crate) fn appliance_overrides(a: &crate::theme::ApplianceColors) -> [(char, Pixel); 13] {
    let [d0, d1, d2, d3] = a.vending_drinks;
    [
        ('Б', Some(a.vending_body)),
        ('П', Some(a.vending_panel)),
        ('Ч', Some(d0)),
        ('Ш', Some(d1)),
        ('Щ', Some(d2)),
        ('Э', Some(d3)),
        ('Ф', Some(a.vending_trim)),
        ('Ы', Some(a.vending_dark)),
        ('Ю', Some(a.printer_body)),
        ('Я', Some(a.printer_top)),
        ('Ё', Some(a.printer_glass)),
        ('Й', Some(a.printer_paper)),
        ('Ц', Some(a.printer_tray)),
    ]
}

/// The monitor bezel standing proud of the desk back, above `desk.y`.
pub(crate) const DESK_BEZEL_RAISE: u16 = 1;

/// The row a desk's art `art_h` tall blits from at `desk_y`: the bezel raise,
/// plus whatever a taller art adds ABOVE `desk.y`, so it keeps the base
/// [`Piece::Desk`]'s bottom row. Both profiles blit desks from this.
pub(crate) fn desk_art_top(pack: &OfficeArt, desk_y: u16, art_h: u16) -> u16 {
    desk_y.saturating_sub(DESK_BEZEL_RAISE + art_h.saturating_sub(base_desk_height(pack)))
}

/// Where a desk stands a prop: its mark's cell, as the cell's column and the
/// row past the prop's foot, and whether the art stands its props mirrored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PropMark {
    pub(crate) at: crate::layout::Point,
    pub(crate) mirrored: bool,
}

impl PropMark {
    /// The column a 1x prop frame `w` wide stood here starts at.
    pub(crate) fn left(self, w: u16) -> Option<u16> {
        prop_left(self.at.x, 1, w, self.mirrored)
    }
}

/// The column a prop frame `w` wide starts at on a mark whose cell starts at
/// `x` and is `cell` wide: there, or on a desk that mirrors its props, so its
/// east edge meets the cell's; `None` past the buffer's west edge.
pub(crate) fn prop_left(x: u16, cell: u16, w: u16, mirrored: bool) -> Option<u16> {
    if mirrored {
        x.checked_add(cell)?.checked_sub(w)
    } else {
        Some(x)
    }
}

/// Where the 1x desk at `desk` facing `facing` stands the prop `prop`.
pub(crate) fn desk_mark(
    pack: &OfficeArt,
    desk: crate::layout::Point,
    facing: crate::layout::Facing,
    prop: super::DeskProp,
) -> PropMark {
    let which = super::Desk::facing(facing);
    let art = pack.desk(which).base();
    let top = desk_art_top(pack, desk.y, art.sprite.first().height());
    let (mx, my) = art.props[prop];
    PropMark {
        at: crate::layout::Point {
            x: desk.x + mx,
            y: top + my + 1,
        },
        mirrored: which.mirrors_props(),
    }
}

/// The 1x art a desk seating its occupant toward `facing` draws.
pub(crate) fn desk_art(
    pack: &OfficeArt,
    facing: crate::layout::Facing,
) -> &pixtuoid_core::sprite::Frame {
    pack.desk(super::Desk::facing(facing)).base().sprite.first()
}

/// The layout cell, from `art`'s top-left, that the middle of its first
/// frame's [`DESK_BULB_KEY`] pixels lies in, the art drawn at `density`;
/// `None` for art that draws no bulb.
pub(crate) fn bulb_cell(art: &Sprite, density: Density) -> Option<(u16, u16)> {
    let w = usize::from(art.first().width());
    let (mut n, mut sx, mut sy) = (0u32, 0u32, 0u32);
    for (i, _) in art
        .recolorable_at(0)
        .drawn_in(&[DESK_BULB_KEY])
        .iter()
        .enumerate()
        .filter(|&(_, &bulb)| bulb)
    {
        n += 1;
        sx += (i % w) as u32;
        sy += (i / w) as u32;
    }
    // a pixel's middle, `(sum / n + ½) / d`, floored to its cell
    let cell = |sum: u32| {
        let d = u32::from(density.get());
        u16::try_from((2 * sum + n) / (2 * n * d)).ok()
    };
    (n > 0).then(|| Some((cell(sx)?, cell(sy)?))).flatten()
}

/// The base [`Piece::Desk`]'s height, which every desk's art keeps at its
/// bottom.
fn base_desk_height(pack: &OfficeArt) -> u16 {
    pack.piece(Piece::Desk).first().height()
}

/// The rows of [`Piece::MeetingSofaNorth`]'s art that lie UNDER its sitter, the
/// seat; the backrest below them draws OVER the sitter's lap.
/// `scripts/gen-art.py`'s `SOFA_SEAT_ROWS` draws to it
/// (`the_north_sofas_backrest_starts_on_its_lit_ridge`).
pub(crate) const NORTH_SOFA_SEAT_ROWS: u16 = 3;

/// Which of `art`'s pixels, row by row, it draws in one of `keys`: those that
/// go transparent when the keys are painted so.
pub(crate) fn drawn_in(art: &super::DenseFrame<'_>, keys: &[char]) -> Vec<bool> {
    art.recolorable.drawn_in(keys)
}

/// The frames an appliance's art opens with that stand still: it shows them
/// idle, and loops the rest while busy.
pub(crate) const APPLIANCE_IDLE_FRAMES: usize = 1;

/// The frame of an appliance's `anim` showing on `beat`: frame 0 when idle,
/// else its busy loop — the frames after the [`APPLIANCE_IDLE_FRAMES`], one
/// each of the art's own `frame_ms`.
pub(crate) fn appliance_frame_index(anim: &Sprite, busy: bool, beat: crate::anim::Beat) -> usize {
    let loop_len = anim.frames().len().saturating_sub(APPLIANCE_IDLE_FRAMES);
    if !busy || loop_len == 0 {
        return 0;
    }
    let step = beat.ms() / u64::from(anim.frame_ms().max(1));
    APPLIANCE_IDLE_FRAMES + usize::try_from(step % loop_len as u64).unwrap_or(0)
}

/// The frame of a looping `anim` showing on `beat`: one each of the art's own
/// `frame_ms`, round and round.
pub(crate) fn looping_frame_index(anim: &Sprite, beat: crate::anim::Beat) -> usize {
    let frames = anim.frames().len() as u64;
    let step = beat.ms() / u64::from(anim.frame_ms().max(1));
    usize::try_from(step % frames).unwrap_or(0)
}
