//! What a frame shows, layer by layer, and whether the screen already shows it.

use pixtuoid_core::sprite::{Rgb, RgbBuffer};

use pixtuoid_scene::cutaway::canvas::Dirty;
use pixtuoid_scene::layout::Bounds;
use pixtuoid_scene::render_scale::PixelFit;

use super::overlays::OverlayLayers;

/// Pack an `Rgb` into [`XrgbSurface`]'s word format, `0x00RRGGBB` (XRGB) — the
/// ONE definition of it; the compositor's office blit and its overlay blend
/// write into the SAME surface, so a lone edit to one would color-swap the
/// chrome with no compile error. The test oracle re-derives the
/// packing independently ON PURPOSE — don't route it through this.
pub(crate) fn pack_xrgb(c: Rgb) -> u32 {
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// The window's row-major `0x00RRGGBB` pixel surface, `w`×`h`, that the text
/// overlays composite into.
#[derive(Debug)]
pub struct XrgbSurface<'a> {
    px: &'a mut [u32],
    w: usize,
    h: usize,
}

impl<'a> XrgbSurface<'a> {
    /// Wrap `px` as a `w`×`h` surface; `None` when it holds fewer than `w * h`
    /// pixels (a transient resize race on the live window).
    pub fn new(px: &'a mut [u32], w: usize, h: usize) -> Option<Self> {
        (px.len() >= w * h).then_some(Self { px, w, h })
    }
}

impl pixtuoid_scene::cutaway::Canvas for XrgbSurface<'_> {
    fn pixel(&self, x: i32, y: i32) -> Option<Rgb> {
        let (x, y) = (usize::try_from(x).ok()?, usize::try_from(y).ok()?);
        (x < self.w && y < self.h).then(|| {
            let v = self.px[y * self.w + x];
            Rgb {
                r: (v >> 16) as u8,
                g: (v >> 8) as u8,
                b: v as u8,
            }
        })
    }

    fn set(&mut self, x: i32, y: i32, rgb: Rgb) {
        if let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y))
            && x < self.w
            && y < self.h
        {
            self.px[y * self.w + x] = pack_xrgb(rgb);
        }
    }
}

/// A rectangle of window pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PxRect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

impl PxRect {
    /// The part of it inside a `(w, h)`-px window, `None` when none is.
    pub(crate) fn within(self, (w, h): (u32, u32)) -> Option<Self> {
        let clip = |start: i32, len: u32, max: u32| {
            let lo = i64::from(start).clamp(0, i64::from(max));
            let hi = (i64::from(start) + i64::from(len)).clamp(0, i64::from(max));
            (hi > lo).then(|| (lo as i32, (hi - lo) as u32))
        };
        let (x, w) = clip(self.x, self.w, w)?;
        let (y, h) = clip(self.y, self.h, h)?;
        Some(Self { x, y, w, h })
    }
}

/// The alpha of the black a card's shadow lays over the office: the
/// office keeps `factor` of itself under it, as [`Canvas::shade`]'s
/// default darkening keeps.
///
/// [`Canvas::shade`]: pixtuoid_scene::cutaway::Canvas::shade
pub(crate) fn shadow_alpha(factor: f32) -> u8 {
    ((1.0 - factor.clamp(0.0, 1.0)) * f32::from(u8::MAX)).round() as u8
}

/// `s` over `d` at alpha `a`, rounded: the one blend the CPU compositor
/// and [`RgbaLayer`]'s shade share, the GPU's within one step.
fn over(s: u8, d: u8, a: u8) -> u8 {
    let max = u32::from(u8::MAX);
    ((u32::from(s) * u32::from(a) + u32::from(d) * (max - u32::from(a)) + max / 2) / max) as u8
}

/// A straight-alpha BGRA layer whose top-left sits at a window point. It
/// takes window coordinates, so a painter that paints the whole window
/// paints into a layer only as big as what it draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RgbaLayer {
    origin: (i32, i32),
    w: u32,
    h: u32,
    px: Vec<[u8; 4]>,
}

impl RgbaLayer {
    /// A transparent layer over `rect`.
    pub(crate) fn new(rect: PxRect) -> Self {
        Self {
            origin: (rect.x, rect.y),
            w: rect.w,
            h: rect.h,
            px: vec![[0; 4]; rect.w as usize * rect.h as usize],
        }
    }

    pub(crate) fn origin(&self) -> (i32, i32) {
        self.origin
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    /// The texel at layer point `(x, y)`, BGRA.
    pub(crate) fn texel(&self, x: u32, y: u32) -> [u8; 4] {
        self.px[y as usize * self.w as usize + x as usize]
    }

    /// Its rows, top first, tightly packed: a GPU texture's upload.
    pub(crate) fn bytes(&self) -> &[u8] {
        self.px.as_flattened()
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let x = u32::try_from(i64::from(x) - i64::from(self.origin.0)).ok()?;
        let y = u32::try_from(i64::from(y) - i64::from(self.origin.1)).ok()?;
        (x < self.w && y < self.h).then(|| y as usize * self.w as usize + x as usize)
    }
}

impl pixtuoid_scene::cutaway::Canvas for RgbaLayer {
    fn pixel(&self, x: i32, y: i32) -> Option<Rgb> {
        let [b, g, r, a] = self.px[self.index(x, y)?];
        (a == u8::MAX).then_some(Rgb { r, g, b })
    }

    fn set(&mut self, x: i32, y: i32, rgb: Rgb) {
        if let Some(i) = self.index(x, y) {
            self.px[i] = [rgb.b, rgb.g, rgb.r, u8::MAX];
        }
    }

    fn shade(&mut self, x: i32, y: i32, factor: f32) {
        let Some(i) = self.index(x, y) else {
            return;
        };
        let a = shadow_alpha(factor);
        let [b, g, r, alpha] = self.px[i];
        self.px[i] = if alpha == u8::MAX {
            [over(0, b, a), over(0, g, a), over(0, r, a), u8::MAX]
        } else {
            [0, 0, 0, a]
        };
    }
}

/// A frame's layers, in paint order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayerId {
    Office,
    Footer,
    Tooltip,
    Panels,
}

impl LayerId {
    pub(crate) const ALL: [Self; 4] = [Self::Office, Self::Footer, Self::Tooltip, Self::Panels];

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// A layer's pixels: the office's opaque RGB at its density, or an
/// overlay's straight-alpha BGRA.
#[derive(Debug, Clone, Copy)]
pub(crate) enum LayerPixels<'a> {
    Opaque(&'a RgbBuffer),
    Rgba(&'a RgbaLayer),
}

impl LayerPixels<'_> {
    pub(crate) fn size(&self) -> (u32, u32) {
        match self {
            Self::Opaque(buf) => (u32::from(buf.width()), u32::from(buf.height())),
            Self::Rgba(layer) => layer.size(),
        }
    }
}

/// Where a layer's pixels may differ from the frame before it, in its own
/// pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change<'a> {
    Unchanged,
    Rects(&'a [Bounds]),
    All,
}

impl<'a> From<&'a Dirty> for Change<'a> {
    fn from(dirty: &'a Dirty) -> Self {
        match dirty {
            Dirty::All => Self::All,
            Dirty::Rects(rects) => Self::Rects(rects.as_slice()),
            Dirty::Unchanged => Self::Unchanged,
        }
    }
}

/// One layer of a frame: its pixels drawn `scale` times over at window
/// point `origin`, `extent` px of them, past which its edge texel repeats.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Layer<'a> {
    pub(crate) id: LayerId,
    pub(crate) pixels: LayerPixels<'a>,
    pub(crate) origin: (i32, i32),
    pub(crate) scale: u16,
    pub(crate) extent: (u32, u32),
    pub(crate) change: Change<'a>,
}

impl Layer<'_> {
    /// The window pixels it draws over.
    fn rect(&self) -> PxRect {
        PxRect {
            x: self.origin.0,
            y: self.origin.1,
            w: self.extent.0,
            h: self.extent.1,
        }
    }
}

/// What a `window`-px frame shows: its layers in paint order, over black.
#[derive(Debug)]
pub struct Composition<'a> {
    pub(crate) window: (u32, u32),
    pub(crate) layers: Vec<Layer<'a>>,
}

/// The window's frame: the office at `at`'s upscale above the footer band,
/// then the overlays.
pub fn composition<'a>(
    office: &'a RgbBuffer,
    change: Change<'a>,
    at: PixelFit,
    window: (u32, u32),
    overlays: &'a OverlayLayers,
) -> Composition<'a> {
    let band = u32::from(super::geometry::footer_band(at));
    let office = Layer {
        id: LayerId::Office,
        pixels: LayerPixels::Opaque(office),
        origin: (0, 0),
        scale: at.upscale(),
        extent: (window.0, window.1.saturating_sub(band)),
        change,
    };
    Composition {
        window,
        layers: std::iter::once(office).chain(overlays.layers()).collect(),
    }
}

/// `c` painted on the CPU: the reference the GPU draws to within a shadow's
/// rounding, and what a snapshot shows.
pub fn composite(c: &Composition<'_>, surf: &mut XrgbSurface<'_>) {
    surf.px.fill(0);
    for layer in &c.layers {
        match layer.pixels {
            LayerPixels::Opaque(buf) => surf.blit(buf, layer),
            LayerPixels::Rgba(rgba) => surf.blend(rgba),
        }
    }
}

impl XrgbSurface<'_> {
    /// The part of `rect` on the surface, as row and column ranges.
    fn clip(&self, rect: PxRect) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
        let size = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let r = rect.within((size(self.w), size(self.h)))?;
        let (x, y) = (r.x as usize, r.y as usize);
        Some((y..y + r.h as usize, x..x + r.w as usize))
    }

    /// `buf` scaled whole-number times over `layer`'s extent, its edge texel
    /// repeating into the remainder.
    fn blit(&mut self, buf: &RgbBuffer, layer: &Layer<'_>) {
        let (bw, bh) = (usize::from(buf.width()), usize::from(buf.height()));
        let Some((rows, cols)) = self.clip(layer.rect()).filter(|_| bw > 0 && bh > 0) else {
            return;
        };
        let s = usize::from(layer.scale);
        let (ox, oy) = (layer.origin.0 as isize, layer.origin.1 as isize);
        let src = buf.as_slice();
        for wy in rows {
            let row = (((wy as isize - oy) as usize) / s).min(bh - 1) * bw;
            let dst = wy * self.w;
            for wx in cols.clone() {
                let col = (((wx as isize - ox) as usize) / s).min(bw - 1);
                self.px[dst + wx] = pack_xrgb(src[row + col]);
            }
        }
    }

    /// `layer` blended over what is there.
    fn blend(&mut self, layer: &RgbaLayer) {
        let ((ox, oy), (w, h)) = (layer.origin(), layer.size());
        let Some((rows, cols)) = self.clip(PxRect { x: ox, y: oy, w, h }) else {
            return;
        };
        for wy in rows {
            for wx in cols.clone() {
                let lx = (wx as i64 - i64::from(ox)) as u32;
                let ly = (wy as i64 - i64::from(oy)) as u32;
                let [b, g, r, a] = layer.texel(lx, ly);
                if a == 0 {
                    continue;
                }
                let i = wy * self.w + wx;
                let d = self.px[i];
                self.px[i] = pack_xrgb(Rgb {
                    r: over(r, (d >> 16) as u8, a),
                    g: over(g, (d >> 8) as u8, a),
                    b: over(b, d as u8, a),
                });
            }
        }
    }
}

/// What the screen shows of one layer, as far as a present may skip.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShownLayer {
    id: LayerId,
    origin: (i32, i32),
    extent: (u32, u32),
    scale: u16,
    size: (u32, u32),
}

fn shown_of(c: &Composition<'_>) -> ((u32, u32), Vec<ShownLayer>) {
    let layers = c
        .layers
        .iter()
        .map(|l| ShownLayer {
            id: l.id,
            origin: l.origin,
            extent: l.extent,
            scale: l.scale,
            size: l.pixels.size(),
        })
        .collect();
    (c.window, layers)
}

/// What the window shows, as far as a frame may skip presenting: the layers
/// of the frame on screen, known only while the screen holds the last frame
/// rendered, which an office's dirt is measured against.
#[derive(Debug)]
pub(crate) struct Presented {
    shown: Option<((u32, u32), Vec<ShownLayer>)>,
    /// Whether the platform keeps the window's pixels between presents. X11
    /// does not ("X does not guarantee to preserve the contents of windows",
    /// Xlib's overview), and winit hands its `Expose` over as the same
    /// `RedrawRequested` a paint tick asks for, so no frame may skip there.
    retains: bool,
}

impl Presented {
    /// Whether `c` must be presented: unchanged layers, the ones on screen,
    /// are the frame on screen, and presenting it again only spends the
    /// copy and the present.
    pub(crate) fn needs(&self, c: &Composition<'_>) -> bool {
        !self.retains
            || c.layers.iter().any(|l| l.change != Change::Unchanged)
            || self.shown.as_ref() != Some(&shown_of(c))
    }

    /// Whether the screen shows the last frame rendered, which each layer's
    /// change is measured against: an upload out of sync must be whole.
    pub(crate) fn in_sync(&self) -> bool {
        self.shown.is_some()
    }

    /// A window on a platform that keeps its pixels (`retains`) or not.
    pub(crate) fn new(retains: bool) -> Self {
        Self {
            shown: None,
            retains,
        }
    }

    /// The screen of the window whose handle is `raw`: X11's (Xlib or XCB)
    /// keeps no pixels, and a window that names no handle is taken as one
    /// that might not.
    pub(crate) fn of_window(raw: Option<winit::raw_window_handle::RawWindowHandle>) -> Self {
        use winit::raw_window_handle::RawWindowHandle;
        Self::new(!matches!(
            raw,
            None | Some(RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_))
        ))
    }

    /// A frame was rendered that may not reach the screen — held back, or
    /// about to present: until one shows, the screen matches nothing rendered.
    pub(crate) fn stale(&mut self) {
        self.shown = None;
    }

    /// `c` reached the screen.
    pub(crate) fn shown(&mut self, c: &Composition<'_>) {
        self.shown = Some(shown_of(c));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_surface_shorter_than_its_extent_is_refused() {
        let mut sb = vec![0u32; 3];
        assert!(XrgbSurface::new(&mut sb, 2, 2).is_none());
    }

    #[test]
    fn pack_xrgb_is_0x00rrggbb() {
        assert_eq!(
            pack_xrgb(Rgb {
                r: 255,
                g: 128,
                b: 0
            }),
            0x00FF_8000
        );
        assert_eq!(pack_xrgb(Rgb { r: 0, g: 0, b: 0 }), 0x0000_0000);
        assert_eq!(pack_xrgb(Rgb { r: 1, g: 2, b: 3 }), 0x0001_0203);
    }

    /// A layer takes window coordinates: what a painter sets at a window
    /// point lands at that point less the layer's origin, and nothing lands
    /// outside it.
    #[test]
    fn a_layer_paints_in_window_coordinates() {
        use pixtuoid_scene::cutaway::Canvas;
        let mut layer = RgbaLayer::new(PxRect {
            x: 10,
            y: 20,
            w: 2,
            h: 2,
        });
        let red = Rgb { r: 255, g: 0, b: 0 };
        layer.set(11, 20, red);
        layer.set(9, 20, red);
        layer.set(12, 21, red);
        assert_eq!(layer.texel(1, 0), [0, 0, 255, u8::MAX], "BGRA, opaque");
        assert_eq!(layer.texel(0, 0), [0; 4], "untouched stays transparent");
        assert_eq!(layer.bytes().iter().filter(|&&b| b != 0).count(), 2);
        assert_eq!(layer.pixel(11, 20), Some(red));
        assert_eq!(
            layer.pixel(10, 20),
            None,
            "nothing under a transparent texel"
        );
    }

    /// A shadow over nothing is black at the shadow's alpha, which the
    /// compositor and the GPU blend over the office; over an opaque texel it
    /// blends there and stays opaque.
    #[test]
    fn a_shadow_over_nothing_is_translucent_black() {
        use pixtuoid_scene::cutaway::Canvas;
        let factor = pixtuoid_scene::display::cells::CARD_SHADOW;
        let a = shadow_alpha(factor);
        assert_eq!(a, ((1.0 - factor) * 255.0).round() as u8);
        let mut layer = RgbaLayer::new(PxRect {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
        });
        layer.shade(0, 0, factor);
        assert_eq!(layer.texel(0, 0), [0, 0, 0, a]);
        layer.set(
            1,
            0,
            Rgb {
                r: 200,
                g: 100,
                b: 0,
            },
        );
        layer.shade(1, 0, factor);
        let keep = |v: u32| ((v * u32::from(u8::MAX - a) + 127) / 255) as u8;
        assert_eq!(layer.texel(1, 0), [0, keep(100), keep(200), u8::MAX]);
    }

    #[test]
    fn a_rect_clips_to_the_window() {
        let r = PxRect {
            x: -2,
            y: 5,
            w: 10,
            h: 10,
        };
        assert_eq!(
            r.within((6, 8)),
            Some(PxRect {
                x: 0,
                y: 5,
                w: 6,
                h: 3
            })
        );
        assert_eq!(
            PxRect {
                x: 7,
                y: 0,
                w: 2,
                h: 2
            }
            .within((6, 8)),
            None
        );
    }

    /// An office over a footer, and maybe a tooltip, `(4, 4)` px big, each
    /// layer changed as `changes` says.
    fn frame<'a>(
        office: &'a RgbBuffer,
        footer: &'a RgbaLayer,
        tip: Option<&'a RgbaLayer>,
        (o, f): (Change<'a>, Change<'a>),
    ) -> Composition<'a> {
        let rgba = |id, l: &'a RgbaLayer, change| Layer {
            id,
            pixels: LayerPixels::Rgba(l),
            origin: l.origin(),
            scale: 1,
            extent: l.size(),
            change,
        };
        let mut layers = vec![
            Layer {
                id: LayerId::Office,
                pixels: LayerPixels::Opaque(office),
                origin: (0, 0),
                scale: 2,
                extent: (4, 4),
                change: o,
            },
            rgba(LayerId::Footer, footer, f),
        ];
        layers.extend(tip.map(|t| rgba(LayerId::Tooltip, t, Change::Unchanged)));
        Composition {
            window: (4, 4),
            layers,
        }
    }

    /// A frame presents when its office changed, a layer over it did, or the
    /// set of layers did; an unchanged composition is the frame on screen,
    /// and a frame rendered but not shown leaves nothing to skip against.
    #[test]
    fn only_a_changed_frame_presents() {
        let office = RgbBuffer::filled(2, 2, Rgb { r: 9, g: 9, b: 9 });
        let footer = RgbaLayer::new(PxRect {
            x: 0,
            y: 3,
            w: 4,
            h: 1,
        });
        let tip = RgbaLayer::new(PxRect {
            x: 1,
            y: 1,
            w: 2,
            h: 1,
        });
        let still = (Change::Unchanged, Change::Unchanged);
        let mut screen = Presented::new(true);
        assert!(
            screen.needs(&frame(&office, &footer, None, still)),
            "the first frame"
        );
        screen.shown(&frame(&office, &footer, None, still));
        assert!(!screen.needs(&frame(&office, &footer, None, still)));
        assert!(screen.needs(&frame(
            &office,
            &footer,
            None,
            (Change::All, Change::Unchanged)
        )));
        assert!(screen.needs(&frame(
            &office,
            &footer,
            None,
            (Change::Unchanged, Change::All)
        )));
        assert!(
            screen.needs(&frame(&office, &footer, Some(&tip), still)),
            "a tooltip appeared"
        );
        // A held frame changed the office off screen: the next, unchanged
        // against it, still presents.
        screen.stale();
        assert!(
            screen.needs(&frame(&office, &footer, None, still)),
            "after a held frame"
        );
        // Where the platform keeps no pixels, every frame presents: X11's
        // windows, and one that names no handle.
        use winit::raw_window_handle::{
            RawWindowHandle, Win32WindowHandle, XcbWindowHandle, XlibWindowHandle,
        };
        let x11 = [
            RawWindowHandle::Xlib(XlibWindowHandle::new(1)),
            RawWindowHandle::Xcb(XcbWindowHandle::new(std::num::NonZeroU32::MIN)),
        ];
        assert!(
            x11.into_iter()
                .all(|raw| !Presented::of_window(Some(raw)).retains)
        );
        assert!(!Presented::of_window(None).retains);
        let win32 = Win32WindowHandle::new(std::num::NonZeroIsize::MIN);
        assert!(Presented::of_window(Some(RawWindowHandle::Win32(win32))).retains);
        let mut forgetful = Presented::new(false);
        forgetful.shown(&frame(&office, &footer, None, still));
        assert!(
            forgetful.needs(&frame(&office, &footer, None, still)),
            "X11 retains nothing"
        );
    }

    /// A frame not shown is what forces the next upload whole: until one
    /// shows, the screen is out of sync with every layer.
    #[test]
    fn an_unshown_frame_forces_a_full_upload_next() {
        let office = RgbBuffer::filled(2, 2, Rgb { r: 9, g: 9, b: 9 });
        let footer = RgbaLayer::new(PxRect {
            x: 0,
            y: 3,
            w: 4,
            h: 1,
        });
        let still = (Change::Unchanged, Change::Unchanged);
        let mut screen = Presented::new(true);
        assert!(!screen.in_sync(), "nothing shown yet");
        screen.shown(&frame(&office, &footer, None, still));
        assert!(screen.in_sync());
        screen.stale();
        assert!(!screen.in_sync(), "the next upload is whole");
    }

    /// The office's whole-number upscale repeats its last texel into the
    /// remainder edge, as the window always drew it.
    #[test]
    fn the_office_layer_repeats_its_last_pixel_into_the_remainder_edge() {
        let px = |v: u8| Rgb { r: v, g: v, b: v };
        let mut office = RgbBuffer::filled(2, 2, px(0));
        for (x, y, v) in [(0, 0, 10), (1, 0, 20), (0, 1, 30), (1, 1, 40)] {
            office.put(x, y, px(v));
        }
        let c = Composition {
            window: (5, 5),
            layers: vec![Layer {
                id: LayerId::Office,
                pixels: LayerPixels::Opaque(&office),
                origin: (0, 0),
                scale: 2,
                extent: (5, 5),
                change: Change::All,
            }],
        };
        let mut sb = vec![0u32; 5 * 5];
        composite(&c, &mut XrgbSurface::new(&mut sb, 5, 5).expect("sized"));
        let at = |x: usize, y: usize| sb[y * 5 + x];
        assert_eq!(at(0, 0), pack_xrgb(px(10)));
        assert_eq!(at(3, 0), pack_xrgb(px(20)));
        assert_eq!(
            at(4, 0),
            pack_xrgb(px(20)),
            "right remainder repeats the last column"
        );
        assert_eq!(
            at(0, 4),
            pack_xrgb(px(30)),
            "bottom remainder repeats the last row"
        );
        assert_eq!(at(4, 4), pack_xrgb(px(40)));
    }

    /// The layered frame is the frame the window painted flat: office,
    /// footer, tooltip and panels over one window-sized surface, exactly,
    /// but for a shadow pixel's rounding, within one step.
    #[test]
    fn layers_composite_to_the_flat_paint() {
        use super::super::overlays::{OverlayLayers, Overlays, footer_budget, panel_preview};
        use super::super::overlays::{PlacedTip, paint_footer, paint_panels, paint_tooltip};
        let theme = pixtuoid_scene::theme::theme_by_name("normal").expect("normal theme exists");
        let pack = crate::test_flash::pack_arc();
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let scene = super::super::fixtures::scene_with(
            vec![super::super::fixtures::running_on("/p/a.jsonl", 0, 0)],
            16,
        );
        for window in [(640u32, 400u32), (1001, 333)] {
            let at = super::super::geometry::window_geometry(
                winit::dpi::PhysicalSize::new(window.0, window.1),
                pack.max_density_variant(),
                super::super::geometry::Zoom::default(),
            );
            let mut renderer =
                super::super::offscreen::OfficeRenderer::new(std::sync::Arc::clone(&pack));
            let office = renderer
                .render(at, super::super::fixtures::frame(&scene, theme, now))
                .expect("a frame")
                .clone();
            let cell = pixtuoid_scene::cutaway::chrome_cell(at);
            let footer = renderer.footer(
                &scene,
                footer_budget(window.0 as usize, cell),
                true,
                None,
                None,
            );
            let panels = panel_preview("help", &scene, theme, (window, cell), now);
            assert!(panels.is_some(), "the help panel opens");
            for cursor in [(30, 30), (window.0 as i32 - 2, window.1 as i32 - 2)] {
                let tip = pixtuoid_scene::tooltip::coffee();
                let next = Overlays {
                    window,
                    footer: footer.clone(),
                    tooltip: Some((tip.clone(), cursor)),
                    panels: panels.clone(),
                };
                let (w, h) = (window.0 as usize, window.1 as usize);
                // Flat: the window as it painted, every painter on one surface.
                let mut flat = vec![0u32; w * h];
                {
                    let mut s = XrgbSurface::new(&mut flat, w, h).expect("sized");
                    // The office under the whole window, the band included.
                    let full = Layer {
                        id: LayerId::Office,
                        pixels: LayerPixels::Opaque(&office),
                        origin: (0, 0),
                        scale: at.upscale(),
                        extent: window,
                        change: Change::All,
                    };
                    composite(
                        &Composition {
                            window,
                            layers: vec![full],
                        },
                        &mut s,
                    );
                    paint_footer(&mut s, &next.footer, (theme, &pack), at, window);
                    let c = (f64::from(cursor.0), f64::from(cursor.1));
                    let placed = PlacedTip::new(&tip, c, theme, cell, window);
                    paint_tooltip(&mut s, &placed, (theme, &pack), cell);
                    if let Some(p) = &next.panels {
                        paint_panels(&mut s, p, (theme, &pack), cell);
                    }
                }
                let mut overlays = OverlayLayers::default();
                overlays.update(next, at, (theme, &pack));
                let mut layered = vec![0u32; w * h];
                composite(
                    &composition(&office, Change::All, at, window, &overlays),
                    &mut XrgbSurface::new(&mut layered, w, h).expect("sized"),
                );
                for (i, (&f, &l)) in flat.iter().zip(&layered).enumerate() {
                    let close = (0..3)
                        .all(|k| ((f >> (8 * k)) & 0xFF).abs_diff((l >> (8 * k)) & 0xFF) <= 1);
                    assert!(
                        close,
                        "{window:?} {cursor:?} px {i}: flat {f:06x} layered {l:06x}"
                    );
                }
            }
        }
    }

    /// A window too short for the footer band composes without panicking.
    #[test]
    fn a_window_shorter_than_the_band_composes_without_panicking() {
        let office = RgbBuffer::filled(4, 4, Rgb { r: 1, g: 2, b: 3 });
        let at = super::super::geometry::window_geometry(
            winit::dpi::PhysicalSize::new(400, 3),
            crate::test_flash::pack().max_density_variant(),
            super::super::geometry::Zoom::default(),
        );
        let overlays = super::super::overlays::OverlayLayers::default();
        let mut sb = vec![0u32; 400 * 3];
        composite(
            &composition(&office, Change::All, at, (400, 3), &overlays),
            &mut XrgbSurface::new(&mut sb, 400, 3).expect("sized"),
        );
    }
}
