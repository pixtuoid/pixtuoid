//! The [effects](crate::effects) riding on the cutaway's figures, painted:
//! each [`Riding`]'s look, a square of art pixels at a time, a fade dithered on
//! the look's own grid.

use crate::sprite::RgbBuffer;

use crate::display::effects::Riding;
use crate::display::pen::{ArtPx, ArtRect};

impl Riding {
    /// Paint it into `buf`.
    pub(crate) fn paint(&self, buf: &mut RgbBuffer) {
        let pen = self.pen;
        self.look(&mut |at, size, c, coverage| {
            let (Ok(x), Ok(y), Ok(side)) = (
                u16::try_from(at.x),
                u16::try_from(at.y),
                u16::try_from(size),
            ) else {
                return;
            };
            // Dithered on the look's own grid, so a look drawn in blocks
            // matches itself at every density.
            if coverage >= 1.0 || crate::dither::takes_next(x / side, y / side, coverage) {
                let size = ArtPx(side);
                let r = ArtRect {
                    x: ArtPx(x),
                    y: ArtPx(y),
                    w: size,
                    h: size,
                };
                pen.fill(buf, r, c);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::sprite::Rgb;

    use super::*;
    use crate::display::pen::Pen;
    use crate::display::pen::test_density;
    use crate::effects::{Effect, EffectKind};

    /// A look stays whole while it shows at least its solid share and only then
    /// dithers out: a fresh heart paints all its cells, a fading one fewer, a
    /// spent one none.
    #[test]
    fn a_fading_look_stays_whole_then_dissolves() {
        let theme = crate::theme::theme_by_name("normal").expect("theme");
        let pen =
            Pen::new(crate::render_scale::RenderScale::ONE, test_density(1)).expect("1 divides 1");
        let bg = Rgb { r: 0, g: 0, b: 0 };
        let drawn = |phase| {
            let heart = Riding {
                effect: Effect {
                    kind: EffectKind::PetHeart,
                    at: crate::layout::Point { x: 10, y: 20 },
                    phase,
                },
                head: None,
                pen,
                inks: crate::effects::look::Inks::of(theme),
            };
            let mut buf = RgbBuffer::filled(32, 32, bg);
            heart.paint(&mut buf);
            buf.as_slice().iter().filter(|&&c| c != bg).count()
        };
        let life = crate::effects::HEART_LIFE_MS;
        let fresh = drawn(0);
        assert_eq!(fresh, 4, "a fresh heart is whole");
        let fading = drawn(life * 4 / 5);
        assert!(
            0 < fading && fading < fresh,
            "a fading heart dissolves: {fading}"
        );
        assert_eq!(drawn(life - 1), 0, "a spent heart is gone");
    }
}
