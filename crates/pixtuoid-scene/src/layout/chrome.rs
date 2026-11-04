//! What a painter outside this workspace, the App, reads to lay its chrome
//! over an office: how its surface fits one, and the sign its chrome keeps
//! clear of.

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
    /// The office in layout units: the surface's, never narrower than the
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
