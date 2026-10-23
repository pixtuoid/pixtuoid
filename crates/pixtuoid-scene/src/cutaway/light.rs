//! The cutaway's time of day, applied: the room darkens with the sky, and its
//! own lights lift what they fall on (the model's [`display::light`]), in
//! whole [`Rgb::ramp`] steps on the art grid ([`crate::dither::step`]), and a
//! light's colour is a tint at fixed stops, so the room stays a palette:
//! nothing blends continuously, the rule the rest of the cutaway is drawn by.
//!
//! The pieces are painted as by day; one pass ([`net_pass`]) then takes each
//! pixel `lift − ambient` steps along its ramp in a single step, so a pixel is
//! never darkened and relit (the ramp does not invert) nor darkened twice.
//!
//! [`display::light`]: crate::display::light

use pixtuoid_core::sprite::{Rgb, RgbBuffer};

use crate::display::light::{Ambient, Flash, LightView};
use crate::display::pen::{ArtRect, BufferPx, Pen};

/// The share of the way to its light's colour a pixel is tinted per step of
/// lift: the brightest cells take the most colour, as they would.
const TINT_PER_STEP: f32 = 0.08;
/// The deepest tint, in steps of [`TINT_PER_STEP`]: past it a lit pixel reads
/// as painted in the light's colour rather than lit by it.
const TINT_MAX_STEPS: u8 = 3;

/// How a painted pixel takes the room's light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Glow {
    /// Darkened with the room, lifted by its lights: most of it.
    Lit,
    /// Its own light, as painted: the sky in a window, a screen that glows, a
    /// bulb.
    Emissive,
    /// Darkened with the room but never lit: a dark screen's glass, which a
    /// lamp would show only as a reflection.
    Shaded,
    /// Its own light, which the room's lights still lift: a window's sky.
    Pane,
}

/// Each buffer pixel's [`Glow`], set by the last piece that painted it, so a
/// piece in front of a screen takes the room's light over it.
#[derive(Debug)]
pub(crate) struct Emission {
    w: u16,
    glow: Vec<Glow>,
}

impl Emission {
    /// A `w`×`h` buffer's, every pixel [`Glow::Lit`].
    pub(crate) fn new(w: u16, h: u16) -> Self {
        Self {
            w,
            glow: vec![Glow::Lit; usize::from(w) * usize::from(h)],
        }
    }

    /// Whether it is a `w`×`h` buffer's.
    pub(crate) fn fits(&self, w: u16, h: u16) -> bool {
        self.w == w && self.glow.len() == usize::from(w) * usize::from(h)
    }

    /// Every pixel of `xs × ys` back to [`Glow::Lit`]: a repaint resets only
    /// what it paints.
    pub(crate) fn reset(&mut self, xs: std::ops::Range<u16>, ys: std::ops::Range<u16>) {
        let w = usize::from(self.w);
        for y in ys {
            let row = usize::from(y) * w;
            self.glow[row + usize::from(xs.start)..row + usize::from(xs.end)].fill(Glow::Lit);
        }
    }

    pub(crate) fn set(&mut self, x: u16, y: u16, glow: Glow) {
        self.glow[usize::from(y) * usize::from(self.w) + usize::from(x)] = glow;
    }

    #[cfg(test)]
    pub(crate) fn get(&self, x: u16, y: u16) -> Glow {
        self.glow[usize::from(y) * usize::from(self.w) + usize::from(x)]
    }
}

/// Light the art pixels of `rect`, clipped to the buffer: each pixel takes
/// `lift − ambient` steps along its ramp at once, where its lift is the most
/// any of `lights` gives it, as its [`Glow`] in `emission` allows. A rect
/// repainted alone comes out as the whole frame does there, so long as
/// `lights` holds every light that meets it.
pub(crate) fn net_pass(
    rect: ArtRect,
    lights: &[&LightView],
    (ambient, flash): (Ambient, Flash),
    emission: &Emission,
    pen: Pen,
    memo: &mut NetMemo,
    buf: &mut RgbBuffer,
) {
    let mut lights: Vec<&LightView> = lights
        .iter()
        .copied()
        .filter(|l| overlaps(l.rect(), rect))
        .collect();
    lights.sort_by_key(|l| l.rank());
    let (w, h) = (usize::from(rect.w.0), usize::from(rect.h.0));
    // The first of the brightest, in rank order.
    let mut lift = vec![0u8; w * h];
    let mut tint: Vec<Option<Rgb>> = vec![None; w * h];
    for l in &lights {
        let r = l.rect();
        let (x0, x1) = (
            r.x.0.max(rect.x.0),
            r.x.0
                .saturating_add(r.w.0)
                .min(rect.x.0.saturating_add(rect.w.0)),
        );
        let (y0, y1) = (
            r.y.0.max(rect.y.0),
            r.y.0
                .saturating_add(r.h.0)
                .min(rect.y.0.saturating_add(rect.h.0)),
        );
        for ay in y0..y1 {
            for ax in x0..x1 {
                let i = usize::from(ay - rect.y.0) * w + usize::from(ax - rect.x.0);
                let here = l.lift_at(ax, ay);
                if here > lift[i] {
                    lift[i] = here;
                    tint[i] = l.tint();
                }
            }
        }
    }
    let (bx0, by0) = (pen.buffer(rect.x).0, pen.buffer(rect.y).0);
    let bx1 = bx0.saturating_add(pen.buffer(rect.w).0);
    let by1 = by0.saturating_add(pen.buffer(rect.h).0);
    let bw = usize::from(buf.width());
    for (by, first, row) in buf.writable_rows_mut() {
        if !(by0..by1).contains(&by) {
            continue;
        }
        let art_row = usize::from(pen.art_of_buffer(BufferPx(by)).0 - rect.y.0) * w;
        for (bx, px) in (first..).zip(row.iter_mut()) {
            if !(bx0..bx1).contains(&bx) {
                continue;
            }
            let a = art_row + usize::from(pen.art_of_buffer(BufferPx(bx)).0 - rect.x.0);
            let (lift, tint) = (lift[a], tint[a]);
            if lift == 0 && ambient.steps() == 0 && flash.steps() == 0 {
                continue;
            }
            let i = usize::from(by) * bw + usize::from(bx);
            *px = memo.of(*px, emission.glow[i], lift, tint, (ambient, flash));
        }
    }
}

/// [`net_pass`]'s colours, kept across frames: OKLab maths a room asks again
/// every frame.
#[derive(Debug, Default)]
pub(crate) struct NetMemo {
    colours: std::collections::HashMap<NetKey, Rgb, foldhash::fast::FixedState>,
    /// Neighbouring pixels mostly ask the last question again.
    last: Option<(NetKey, Rgb)>,
}

/// Everything [`net_colour`] reads for one pixel.
type NetKey = (Rgb, Glow, u8, Option<Rgb>, Ambient, Flash);

/// Bounds the memo in a room whose colours never settle.
const NET_MEMO_CAP: usize = 1 << 16;

impl NetMemo {
    fn of(
        &mut self,
        under: Rgb,
        glow: Glow,
        lift: u8,
        tint: Option<Rgb>,
        (ambient, flash): (Ambient, Flash),
    ) -> Rgb {
        let key = (under, glow, lift, tint, ambient, flash);
        if let Some((k, c)) = self.last
            && k == key
        {
            return c;
        }
        if self.colours.len() >= NET_MEMO_CAP {
            self.colours.clear();
        }
        // A strike lights everything, what glows of its own too.
        let c = *self.colours.entry(key).or_insert_with(|| {
            flash.on(match glow {
                Glow::Lit => net_colour(under, lift, ambient, tint),
                Glow::Emissive => under,
                Glow::Shaded => ambient.on(under),
                Glow::Pane => net_colour(under, lift, Ambient::default(), tint),
            })
        });
        self.last = Some((key, c));
        c
    }
}

/// `c` in a room `ambient` dark, lifted `lift` steps: `lift − ambient` steps
/// along its ramp in one, then tinted toward `tint` a fixed stop per step of
/// lift.
fn net_colour(c: Rgb, lift: u8, ambient: Ambient, tint: Option<Rgb>) -> Rgb {
    let net = lift.min(ambient.ceiling()) as i8 - ambient.steps() as i8;
    let stepped = c.ramp(net);
    match tint {
        Some(t) if lift > 0 => stepped.mix(t, f32::from(lift.min(TINT_MAX_STEPS)) * TINT_PER_STEP),
        _ => stepped,
    }
}

fn overlaps(a: ArtRect, b: ArtRect) -> bool {
    a.x.0 < b.x.0.saturating_add(b.w.0)
        && b.x.0 < a.x.0.saturating_add(a.w.0)
        && a.y.0 < b.y.0.saturating_add(b.h.0)
        && b.y.0 < a.y.0.saturating_add(a.h.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::light::AMBIENT_MAX_STEPS;
    use crate::display::light::tests::{lamp, pen, view};
    use crate::display::pen::ArtPx;
    use crate::layout::Point;

    /// A repaint's reset takes its rect back to lit and leaves the rest as
    /// the last frame marked it.
    #[test]
    fn a_reset_lights_only_its_rect() {
        let mut emission = Emission::new(4, 3);
        for y in 0..3 {
            for x in 0..4 {
                emission.set(x, y, Glow::Emissive);
            }
        }
        emission.reset(1..3, 1..2);
        let lit: Vec<(u16, u16)> = (0..3)
            .flat_map(|y| (0..4).map(move |x| (x, y)))
            .filter(|&(x, y)| emission.get(x, y) == Glow::Lit)
            .collect();
        assert_eq!(lit, [(1, 1), (2, 1)]);
    }

    const FLOOR: Rgb = Rgb {
        r: 110,
        g: 96,
        b: 84,
    };
    const WARM: Rgb = Rgb {
        r: 255,
        g: 214,
        b: 150,
    };

    fn whole(w: u16, h: u16) -> ArtRect {
        ArtRect {
            x: ArtPx(0),
            y: ArtPx(0),
            w: pen().art(w),
            h: pen().art(h),
        }
    }

    fn lit_ground(views: &[&LightView], ambient: Ambient) -> RgbBuffer {
        let mut buf = RgbBuffer::filled(160, 64, FLOOR);
        net_pass(
            whole(40, 16),
            views,
            (ambient, Flash::default()),
            &Emission::new(160, 64),
            pen(),
            &mut NetMemo::default(),
            &mut buf,
        );
        buf
    }

    #[test]
    fn a_lit_lamp_lifts_its_pool_at_night_and_nothing_past_it() {
        let night = Ambient::NIGHT;
        let lamp = view(&lamp(0.6, Point { x: 10, y: 8 }), Some(WARM), night);
        let buf = lit_ground(&[&lamp], night);
        let luma = Rgb::lightness;
        assert!(luma(buf.get(10 * 4 + 1, 8 * 4 + 1)) > luma(night.on(FLOOR)));
        assert_eq!(buf.get(0, 0), night.on(FLOOR));
    }

    /// A lit pixel is taken `lift − ambient` steps in one: never darkened, then
    /// relit, which the ramp would not undo.
    #[test]
    fn a_lit_pixel_steps_once_by_its_net() {
        let night = Ambient::NIGHT;
        let lamp = view(&lamp(0.6, Point { x: 10, y: 8 }), None, night);
        let buf = lit_ground(&[&lamp], night);
        let mut checked = 0;
        for ay in 0..64u16 {
            for ax in 0..160u16 {
                let lift = lamp.lift_at(ax, ay);
                let want = FLOOR.ramp(lift as i8 - night.steps() as i8);
                assert_eq!(buf.get(ax, ay), want, "({ax}, {ay}) lifted {lift}");
                checked += usize::from(lift > 0);
            }
        }
        assert!(checked > 0, "the lamp lit nothing");
    }

    /// Nothing blends: a lit pixel is its colour some whole steps along the ramp,
    /// tinted at that many fixed stops, and nothing between.
    #[test]
    fn a_light_paints_only_whole_steps_and_tint_stops() {
        let night = Ambient::NIGHT;
        let lamp = view(&lamp(0.6, Point { x: 10, y: 8 }), Some(WARM), night);
        let buf = lit_ground(&[&lamp], night);
        let allowed: std::collections::HashSet<Rgb> = (0..=AMBIENT_MAX_STEPS)
            .map(|l| net_colour(FLOOR, l, night, Some(WARM)))
            .collect();
        let seen: std::collections::HashSet<Rgb> = buf.as_slice().iter().copied().collect();
        assert!(seen.len() > 2, "a pool steps through tones: {seen:?}");
        assert!(
            seen.is_subset(&allowed),
            "off-stop colours: {:?}",
            seen.difference(&allowed)
        );
    }

    /// Where two lights lift a pixel alike, the same one lights it whatever order
    /// they come in.
    #[test]
    fn overlapping_lights_resolve_alike_in_any_order() {
        let night = Ambient::NIGHT;
        let (a, b) = (
            view(&lamp(0.8, Point { x: 10, y: 8 }), Some(WARM), night),
            view(
                &lamp(0.8, Point { x: 14, y: 8 }),
                Some(Rgb {
                    r: 120,
                    g: 160,
                    b: 255,
                }),
                night,
            ),
        );
        assert_eq!(
            lit_ground(&[&a, &b], night).as_slice(),
            lit_ground(&[&b, &a], night).as_slice()
        );
    }

    /// A rect repainted alone comes out as the whole frame does there, so the
    /// canvas may repaint only what changed.
    #[test]
    fn a_rect_relit_alone_matches_the_whole_frame() {
        let night = Ambient::NIGHT;
        let (a, b) = (
            view(&lamp(0.8, Point { x: 10, y: 8 }), Some(WARM), night),
            view(&lamp(0.7, Point { x: 16, y: 9 }), None, night),
        );
        let full = lit_ground(&[&a, &b], night);
        let rect = ArtRect {
            x: ArtPx(40),
            y: ArtPx(20),
            w: ArtPx(30),
            h: ArtPx(24),
        };
        let mut part = RgbBuffer::filled(160, 64, FLOOR);
        net_pass(
            rect,
            &[&a, &b],
            (night, Flash::default()),
            &Emission::new(160, 64),
            pen(),
            &mut NetMemo::default(),
            &mut part,
        );
        for y in rect.y.0..rect.y.0 + rect.h.0 {
            for x in rect.x.0..rect.x.0 + rect.w.0 {
                assert_eq!(part.get(x, y), full.get(x, y), "({x}, {y})");
            }
        }
    }

    /// An emissive pixel keeps its colour and a shaded one only darkens, under
    /// the brightest light.
    #[test]
    fn a_glowing_pixel_is_its_own_light() {
        let night = Ambient::NIGHT;
        let lamp = view(&lamp(1.0, Point { x: 10, y: 8 }), Some(WARM), night);
        let mut emission = Emission::new(160, 64);
        emission.set(41, 33, Glow::Emissive);
        emission.set(42, 33, Glow::Shaded);
        let mut buf = RgbBuffer::filled(160, 64, FLOOR);
        net_pass(
            whole(40, 16),
            &[&lamp],
            (night, Flash::default()),
            &emission,
            pen(),
            &mut NetMemo::default(),
            &mut buf,
        );
        assert_eq!(buf.get(41, 33), FLOOR);
        assert_eq!(buf.get(42, 33), night.on(FLOOR));
        assert_ne!(
            buf.get(43, 33),
            night.on(FLOOR),
            "the lamp lights its bulb's cell"
        );
    }
}
