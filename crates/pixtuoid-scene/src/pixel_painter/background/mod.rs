//! Background pass — depth-independent floor, walls, windows, skyline,
//! clock, corridor runner, entry mat, time-of-day overlays, lamp halo,
//! floor shadows, and weather effects.
//!
//! Everything here paints BEFORE the y-sorted entity pass, in the order the
//! orchestrator (`pixel_painter/mod.rs`) calls it; the backdrop fixtures
//! among it (clock, runner, mats) in roster order.

mod ground_wash;
mod lighting;

pub(super) use ground_wash::paint_ground_wash;
pub(super) use lighting::{
    paint_clock, paint_corridor_runner, paint_light, paint_neon_panel, paint_shadows,
};

use crate::pack::OfficeArt;
use crate::sprite::{Rgb, RgbBuffer};

use super::palette::{RgbLut, WHITE, blend_rgb};

use crate::atmosphere::Moment;
use crate::dither::Dithered;
use crate::glass_weather::GlassWeather;
use crate::layout::{wall_trim_row, window_posts, window_rows};
use crate::sky::Sky;
use crate::theme::Theme;

/// How far a strike's peak washes the window glass white.
const BOLT_ALPHA: f32 = 0.6;

/// Room-wide ambient bounce from a lightning strike, at [`Sky::flash`].
pub(super) fn paint_lightning_flash(buf: &mut RgbBuffer, sky: &Sky) {
    let level = sky.flash();
    if level <= 0.0 {
        return;
    }
    let alpha = 0.20 * level;
    let lut = RgbLut::tabulate(|c| blend_rgb(c, WHITE, alpha));
    for px in buf.as_mut_slice() {
        *px = lut.apply(*px);
    }
}

/// The base fill's complete input set: the [`CachedLayer`](crate::cached_layer::CachedLayer)
/// key, and so its correctness boundary.
#[derive(Debug, PartialEq)]
struct BaseFillKey {
    band_h: u16,
    carpet: Dithered<[Rgb; 3]>,
    wall: Rgb,
}

/// Memoized carpet-noise + wall-band base fill — the full-buffer fills were
/// the largest single cost of a frame (#900's profile) yet their inputs
/// ([`BaseFillKey`]) change only on a resize, theme swap, or weather-tint
/// change.
#[derive(Debug, Default)]
pub(crate) struct BaseFillCache(crate::cached_layer::CachedLayer<BaseFillKey>);

/// The carpet's speckle hash: two linear mixes of a pixel's x and y, xored.
const SPECKLE_MIX: [(u32, u32); 2] = [(73, 151), (11, 37)];
/// The speckle's cycle: the hash modulo it picks the tone, two of its
/// values each speckle tone and the rest the carpet's base.
const SPECKLE_PERIOD: u32 = 17;

impl BaseFillCache {
    /// Empty cache — no fill retained yet.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Stamp the memoized fill over `buf`, refilling on any key change.
    fn blit_into(&mut self, buf: &mut RgbBuffer, key: BaseFillKey) {
        let fill = |key: &BaseFillKey, filled: &mut RgbBuffer| {
            let BaseFillKey {
                band_h,
                carpet,
                wall,
            } = *key;
            let (buf_w, buf_h) = (filled.width(), filled.height());
            filled.resize_fill(buf_w, buf_h, wall);
            for y in band_h..buf_h {
                for x in 0..buf_w {
                    let [(ax, ay), (bx, by)] = SPECKLE_MIX;
                    let hash = u32::from(x)
                        .wrapping_mul(ax)
                        .wrapping_add(u32::from(y).wrapping_mul(ay))
                        ^ (u32::from(x).wrapping_mul(bx) ^ u32::from(y).wrapping_mul(by));
                    let carpet = carpet.at(x, y);
                    let color = match hash % SPECKLE_PERIOD {
                        0 | 1 => carpet[0],
                        2 | 3 => carpet[1],
                        _ => carpet[2],
                    };
                    filled.put(x, y, color);
                }
            }
        };
        self.0.stamp(key, fill, buf);
    }
}

/// The ground and the north `wall`'s band, the posts between its windows
/// included, over the whole of `buf`, at `moment`.
pub(super) fn paint_ground_and_walls(
    base_fill: &mut BaseFillCache,
    buf: &mut RgbBuffer,
    wall: &crate::outside::Wall,
    moment: &Moment,
    theme: &Theme,
) {
    let top_wall_h = wall.band_h;
    let (buf_w, buf_h) = (buf.width(), buf.height());
    let look = &moment.look;
    let window_frame = theme.surface.window_frame;
    let wall_color = theme.surface.wall;
    let wall_trim_color = theme.surface.wall_trim;

    let carpet = look.carpet(theme).map(|c| [c.lit, c.dark, c.base]);
    base_fill.blit_into(
        buf,
        BaseFillKey {
            band_h: top_wall_h.min(buf_h),
            carpet,
            wall: wall_color,
        },
    );

    let rows = window_rows(top_wall_h);
    for post in window_posts(wall.span.clone()) {
        for x in post {
            for y in rows.clone() {
                buf.put_checked(x, y, window_frame);
            }
        }
    }

    let trim_y = wall_trim_row(top_wall_h);
    if trim_y < buf_h {
        for x in 0..buf_w {
            buf.put(x, trim_y, wall_trim_color);
        }
    }
}

/// `wall`'s windows, at `moment`.
pub(super) fn paint_windows(
    buf: &mut RgbBuffer,
    wall: crate::outside::Wall,
    moment: &Moment,
    pack: &OfficeArt,
    theme: &Theme,
    outside: &mut crate::outside::OutsideCache,
) {
    let views = outside.views(
        moment,
        pack,
        theme,
        wall,
        crate::sprite::format::Density::ONE,
        GlassWeather::of(moment),
    );
    // The bolt, the strike's source, lights the glass in lockstep with
    // `paint_lightning_flash`, over all it shows.
    let bolt = BOLT_ALPHA * moment.sky.flash();
    for (_, mut view) in views {
        if bolt > 0.0 {
            std::sync::Arc::make_mut(&mut view).paint(|_, c| blend_rgb(c, WHITE, bolt));
        }
        for ((x, y), c) in view.every() {
            buf.put_checked(x, y, c.unwrap_or(theme.surface.window_frame));
        }
    }
}

#[cfg(test)]
mod tests;
