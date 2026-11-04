//! What the cutaway lays under a display list's pieces, resolved from the
//! layout and the theme where the list is built, so the rasterizer reads
//! neither.

use std::ops::Range;

use crate::sprite::Rgb;

use crate::layout::{Bounds, FixtureKind, SceneLayout, Size};
use crate::theme::Theme;

/// The ground, the north wall band and the floor coverings: none of it moves
/// within a layout and theme.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Backdrop {
    /// The office's size, in layout cells.
    pub(crate) size: Size,
    /// The north wall band's height ([`SceneLayout::wall_band_h`]).
    pub(crate) wall_band_h: u16,
    /// The posts between the band's windows
    /// ([`window_posts`](crate::layout::window_posts)).
    pub(crate) posts: Vec<Range<u16>>,
    /// The band's window rows ([`window_rows`](crate::layout::window_rows)).
    pub(crate) window_rows: Range<u16>,
    /// The band's trim row ([`wall_trim_row`](crate::layout::wall_trim_row)).
    pub(crate) trim_row: u16,
    /// Each floor covering, in the layout's fixture order.
    pub(crate) coverings: Vec<(Bounds, Covering)>,
    pub(crate) tones: BackdropTones,
}

/// The theme's tones the backdrop is laid in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BackdropTones {
    /// What a buffer holds before its first frame.
    pub(crate) bg: Rgb,
    pub(crate) wall: Rgb,
    pub(crate) window_frame: Rgb,
    pub(crate) wall_trim: Rgb,
}

/// A fixture flat on the ground, laid with the backdrop under every shadow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Covering {
    Rug { trim: Rgb, accent: Rgb, field: Rgb },
    Runner { base: Rgb, stripe: Rgb, edge: Rgb },
}

impl Covering {
    /// `kind`'s covering in `theme`'s tones; `None` for a fixture that stands
    /// rather than lies.
    pub(crate) fn of(kind: FixtureKind, theme: &Theme) -> Option<Self> {
        use FixtureKind as K;
        let (f, o) = (&theme.furniture, &theme.office);
        match kind {
            K::MeetingRug { .. }
            | K::LoungeRug
            | K::Doormat { .. }
            | K::PantryMat
            | K::IslandMat => Some(Self::Rug {
                trim: f.rug_trim,
                accent: f.rug_accent,
                field: f.rug_field,
            }),
            K::Runner => Some(Self::Runner {
                base: o.runner_base,
                stripe: o.runner_stripe,
                edge: o.runner_edge,
            }),
            K::Desk(_)
            | K::FilingCabinet(_)
            | K::DeskChair(_)
            | K::Station { .. }
            | K::Plant { .. }
            | K::Pod { .. }
            | K::Wall { .. }
            | K::MeetingSofa { .. }
            | K::MeetingTable { .. }
            | K::MeetingChair { .. }
            | K::CoatRack { .. }
            | K::NoticeBoard { .. }
            | K::LoungeCouch
            | K::SideTable
            | K::FloorLamp
            | K::FishTank
            | K::KitchenIsland
            | K::WaterCooler
            | K::TrashBin
            | K::Door
            | K::NeonSign
            | K::Clock => None,
        }
    }
}

impl Backdrop {
    /// `layout`'s backdrop in `theme`'s tones.
    pub(crate) fn of(layout: &SceneLayout, theme: &Theme) -> Self {
        let band_h = layout.wall_band_h();
        let s = &theme.surface;
        Self {
            size: Size {
                w: layout.buf_w,
                h: layout.buf_h,
            },
            wall_band_h: band_h,
            posts: crate::layout::window_posts(layout.window_wall()).collect(),
            window_rows: crate::layout::window_rows(band_h),
            trim_row: crate::layout::wall_trim_row(band_h),
            coverings: layout
                .fixtures()
                .filter_map(|f| Covering::of(f.kind, theme).map(|c| (f.visual, c)))
                .collect(),
            tones: BackdropTones {
                bg: s.bg_fallback,
                wall: s.wall,
                window_frame: s.window_frame,
                wall_trim: s.wall_trim,
            },
        }
    }
}
