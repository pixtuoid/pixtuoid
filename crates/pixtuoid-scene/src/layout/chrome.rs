//! What a painter outside this workspace, the App, reads to lay its chrome
//! over an office: how its surface fits one, the sign and badges its chrome
//! keeps clear of, whether two boxes overlap, and the mark it draws.

use super::{Bounds, Insets, SceneLayout, Size, Viewport};

/// The narrowest office [`Viewport::fitted`] lays out, so a narrower surface
/// scrolls across it. Every meeting room keeps its furniture this wide
/// (`a_fitted_office_keeps_every_meeting_rooms_furniture`), and the value is
/// the App's design margin over the narrowest that does (pixtuoid-apple
/// `docs/design.md`).
pub(crate) const FITTED_MIN_W: u16 = 150;

/// The tallest office [`Viewport::fitted`] lays out, so a taller surface takes
/// bigger pixels instead: the App's (pixtuoid-apple `docs/design.md`), past
/// which a layout stretches its rooms and shrinks its people.
pub(crate) const FITTED_MAX_H: u16 = 218;

/// [`Viewport::fitted`] before its layout's size gate.
pub(super) fn fit(
    surface_px: Size,
    insets_px: Insets,
    density: crate::sprite::format::Density,
    per_point: std::num::NonZeroU16,
) -> Fitted {
    let fit =
        crate::render_scale::PixelFit::at_most_high(FITTED_MAX_H, (density, per_point), surface_px);
    let units = |px: u16| px.div_ceil(fit.scale().get());
    let Size { w, h } = fit.logical();
    let insets = Insets {
        top: units(insets_px.top),
        right: units(insets_px.right),
        bottom: units(insets_px.bottom),
        left: units(insets_px.left),
    };
    Fitted {
        viewport: Viewport {
            size: Size {
                // The layout runs inside the insets, so FITTED_MIN_W is counted
                // inside them.
                w: w.max(
                    FITTED_MIN_W
                        .saturating_add(insets.left)
                        .saturating_add(insets.right),
                ),
                h,
            },
            insets,
        },
        upscale: fit.upscale(),
    }
}

/// What a surface of device pixels fits an office as ([`Viewport::fitted`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fitted {
    /// The viewport in layout units: the surface's, never narrower than the
    /// fitted width that keeps its meeting rooms furnished, its insets rounded
    /// up to whole units.
    pub viewport: Viewport,
    /// Whole device pixels a frame pixel takes.
    pub upscale: u16,
}

impl Viewport {
    /// The office a surface `surface_px` device pixels big lays out, under
    /// `insets_px` of them, drawn at `density` frame pixels a unit and
    /// `per_point` device pixels at the least a frame pixel, so no frame pixel
    /// is smaller than a point; `None` where the insets leave it too small to
    /// lay out.
    pub fn fitted(
        surface_px: Size,
        insets_px: Insets,
        density: crate::sprite::format::Density,
        per_point: std::num::NonZeroU16,
    ) -> Option<Fitted> {
        let fitted = fit(surface_px, insets_px, density, per_point);
        super::compute::office_area(fitted.viewport).map(|_| fitted)
    }
}

impl SceneLayout {
    /// The wall sign's outer box, its glowing frame included, which a host's
    /// chrome keeps clear of.
    pub fn board_panel(&self) -> Bounds {
        super::roster::neon_panel(self.inner)
    }
}

impl Bounds {
    /// Whether the two half-open boxes share a pixel. A zero-sized box shares
    /// none.
    pub fn overlaps(self, other: Bounds) -> bool {
        let end = super::exclusive_end;
        self.width > 0
            && self.height > 0
            && other.width > 0
            && other.height > 0
            && u32::from(self.x) < end(other.x, other.width)
            && u32::from(other.x) < end(self.x, self.width)
            && u32::from(self.y) < end(other.y, other.height)
            && u32::from(other.y) < end(self.y, self.height)
    }
}

impl crate::look::Hits {
    /// `agent`'s badge plate as this frame drew it, in layout units, cut where
    /// the area inside the insets or the canvas ends; where it baked no text,
    /// the plate it would have baked. `None` for an agent this floor doesn't
    /// draw.
    pub fn badge(&self, agent: &pixtuoid_core::AgentId) -> Option<Bounds> {
        let canvas = Size {
            w: self.layout().buf_w,
            h: self.layout().buf_h,
        };
        self.badges
            .iter()
            .find(|b| b.agent == *agent)
            .map(|b| crate::display::compose::shown_box(&b.run(), self.pen, canvas))
    }
}

/// Rows of art pixels, every row as wide as the first: `.` a gap, and any
/// other character ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    rows: &'static [&'static str],
}

impl Glyph {
    /// `rows` as a glyph: a row not ASCII or not as wide as the first fails
    /// the build of the const it is made in.
    pub(crate) const fn new(rows: &'static [&'static str]) -> Self {
        let mut i = 0;
        while i < rows.len() {
            assert!(
                rows[i].is_ascii() && rows[i].len() == rows[0].len(),
                "a glyph's rows are ASCII and as wide as its first"
            );
            i += 1;
        }
        Self { rows }
    }

    /// Its width, in art pixels.
    pub const fn width(self) -> u16 {
        match self.rows.first() {
            Some(row) => row.len() as u16,
            None => 0,
        }
    }

    /// Its height, in art pixels.
    pub const fn height(self) -> u16 {
        self.rows.len() as u16
    }

    /// Whether art pixel `(x, y)` of it is inked; `false` past its edges.
    pub fn ink(self, x: u16, y: u16) -> bool {
        self.inked().any(|at| at == (x, y))
    }

    /// Its inked pixels, as offsets from its top-left: the one rule every
    /// painter draws a glyph by.
    pub(crate) fn inked(self) -> impl Iterator<Item = (u16, u16)> {
        (0u16..).zip(self.rows).flat_map(|(dy, row)| {
            (0u16..)
                .zip(row.chars())
                .filter(|&(_, c)| c != '.')
                .map(move |(dx, _)| (dx, dy))
        })
    }
}

/// The mark beside the head of an agent whose turn it is.
pub const REQUIRES_ACTION_MARK: Glyph = Glyph::new(&[
    ".####.", //
    "##..##", //
    "##..##", //
    "...##.", //
    "..##..", //
    "..##..", //
    "......", //
    "..##..", //
    "..##..", //
]);

#[cfg(test)]
mod tests {
    use super::*;

    /// A glyph inks a pixel exactly where its rows put any character but a
    /// `.`, and none past its edges.
    #[test]
    fn a_glyph_inks_where_its_rows_do() {
        let glyph = REQUIRES_ACTION_MARK;
        for (y, row) in (0u16..).zip(glyph.rows) {
            for (x, c) in (0u16..).zip(row.chars()) {
                assert_eq!(glyph.ink(x, y), c != '.', "({x}, {y})");
            }
        }
        for (x, y) in [
            (glyph.width(), 0),
            (0, glyph.height()),
            (u16::MAX, u16::MAX),
        ] {
            assert!(!glyph.ink(x, y), "({x}, {y})");
        }
    }
}
