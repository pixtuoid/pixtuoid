//! Ambient pass — painted between the background and the y-sorted
//! drawables: ceiling halos above running monitors.

use pixtuoid_core::sprite::RgbBuffer;

use crate::lighting::{Emitter, EmitterKind};
use crate::pixel_painter::background::paint_light;
use crate::theme::Theme;

/// Each halo over a lit monitor, tinted by its tool. Dark themes only — on a
/// light theme the warm tint reads as grime.
pub(super) fn paint_ceiling_halos(buf: &mut RgbBuffer, theme: &Theme, halos: &[Emitter]) {
    use crate::theme::ThemeKind;
    if theme.kind != ThemeKind::Dark {
        return;
    }
    for halo in halos {
        if let EmitterKind::MonitorHalo(work) = halo.kind {
            let color = theme.tool_glow.for_work(work);
            paint_light(buf, halo, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_core::sprite::Rgb;

    /// One monitor halo centred at `(x, y)`, over an edit.
    fn one_halo(x: u16, y: u16) -> Vec<Emitter> {
        vec![Emitter {
            kind: EmitterKind::MonitorHalo(crate::lighting::ScreenWork::ToolUse(
                pixtuoid_core::state::ToolKind::Edit,
            )),
            light: crate::lighting::Light::Patch {
                centre: crate::layout::Point { x, y },
            },
            strength: 0.8,
        }]
    }

    #[test]
    fn ceiling_halo_painted_on_dark_theme() {
        let mut buf = RgbBuffer::filled(160, 90, Rgb { r: 0, g: 0, b: 0 });
        let theme = &crate::theme::CYBERPUNK;
        let halos = one_halo(50, 10);
        let baseline = buf.get(50, 10);
        paint_ceiling_halos(&mut buf, theme, &halos);
        assert_ne!(baseline, buf.get(50, 10), "halo should brighten the pixel");
    }

    #[test]
    fn ceiling_halo_skipped_on_light_theme() {
        let mut buf = RgbBuffer::filled(160, 90, Rgb { r: 0, g: 0, b: 0 });
        let theme = &crate::theme::NORMAL;
        let halos = one_halo(50, 10);
        let baseline = buf.get(50, 10);
        paint_ceiling_halos(&mut buf, theme, &halos);
        assert_eq!(baseline, buf.get(50, 10), "no halo on light themes");
    }

    #[test]
    fn ceiling_halo_near_edge_does_not_panic() {
        let mut buf = RgbBuffer::filled(6, 4, Rgb { r: 0, g: 0, b: 0 });
        let theme = &crate::theme::CYBERPUNK; // Dark theme so halos paint.
        let halos = one_halo(5, 0);
        paint_ceiling_halos(&mut buf, theme, &halos);
    }
}
