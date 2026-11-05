//! What a painter outside this workspace, the App, reads to lay its chrome
//! over an office: how its surface fits one, the sign and badges its chrome
//! keeps clear of and how to tell, and the mark it draws.

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
pub(crate) const FITTED_MAX_H: u16 = 240;

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
    Fitted {
        viewport: Viewport {
            size: Size {
                w: w.max(FITTED_MIN_W),
                h,
            },
            insets: Insets {
                top: units(insets_px.top),
                right: units(insets_px.right),
                bottom: units(insets_px.bottom),
                left: units(insets_px.left),
            },
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
        // u32: a box's exclusive end can lie one past `u16::MAX`.
        let end = |at: u16, len: u16| u32::from(at) + u32::from(len);
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
    /// the area inside the insets or the canvas ends; where the frame's host
    /// sets its text, where the frame would have drawn it. `None` for an agent
    /// this floor doesn't draw.
    pub fn badge(&self, agent: &pixtuoid_core::AgentId) -> Option<Bounds> {
        let (badges, pen) = &self.badges;
        let canvas = Size {
            w: self.layout().buf_w,
            h: self.layout().buf_h,
        };
        badges
            .iter()
            .find(|b| b.agent == *agent)
            .map(|b| crate::display::compose::shown_box(&b.run(), *pen, canvas))
    }
}

/// A glyph on the art grid of the office's densest look: rows of `#` ink and
/// `.` gaps, each as wide as the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    rows: &'static [&'static str],
}

impl Mark {
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
        self.rows
            .get(usize::from(y))
            .and_then(|row| row.as_bytes().get(usize::from(x)))
            == Some(&b'#')
    }

    /// Its rows, which the painters stamp.
    pub(crate) const fn rows(self) -> &'static [&'static str] {
        self.rows
    }
}

/// The mark beside the head of an agent whose turn it is.
pub const REQUIRES_ACTION_MARK: Mark = Mark {
    rows: &[
        ".####.", //
        "##..##", //
        "##..##", //
        "...##.", //
        "..##..", //
        "..##..", //
        "......", //
        "..##..", //
        "..##..", //
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The requires-action mark is 6 art pixels wide and 9 tall, inked where
    /// its rows are and nowhere past them.
    #[test]
    fn the_requires_action_mark_inks_its_rows() {
        let mark = REQUIRES_ACTION_MARK;
        assert_eq!((mark.width(), mark.height()), (6, 9));
        let drawn: Vec<String> = (0..mark.height())
            .map(|y| {
                (0..mark.width())
                    .map(|x| if mark.ink(x, y) { '#' } else { '.' })
                    .collect()
            })
            .collect();
        assert_eq!(
            drawn,
            [
                ".####.", "##..##", "##..##", "...##.", "..##..", "..##..", "......", "..##..",
                "..##..",
            ]
        );
        for (x, y) in [(6, 0), (0, 9), (u16::MAX, u16::MAX)] {
            assert!(!mark.ink(x, y), "({x}, {y})");
        }
    }
}
