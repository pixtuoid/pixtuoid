//! The cutaway's rasterizer: a frame's display list painted over its backdrop.

use crate::pack::Piece as PackPiece;
use crate::pack::{Desk, OfficeArt};
use crate::sprite::RgbBuffer;
use crate::sprite::blit::blit_frame_scaled;

use crate::atmosphere::Carpet;
use crate::cutaway::shade::{Ramp, fill, slab};
#[cfg(test)]
use crate::display::compose::{art_size, desk_front_h};
use crate::display::pen::{ArtPx, ArtRect, BufferPx, Pen};
use crate::display::{
    Art, Backdrop, Content, Covering, DisplayList, Emits, Figure, Flip, Office, PieceKind,
    Recolours, Screen, Showing, StoodProp, TextRun, compose, desk_span, face_rows, line_start,
    ring_insets, run_rect,
};
use crate::dither::Dithered;
use crate::layout::{Bounds, Point};
use crate::outside::WindowView;
use crate::pack::drawn_in;
use crate::render_scale::RenderScale;
use crate::sim::SimFrame;

/// How far the key light reaches down the room before the ground falls off.
///
/// The windows are the north wall, so the falloff runs north to south; these
/// bound the dithered transition between the lit and base ground tones.
const GROUND_LIT_NUMER: u16 = 1;
/// Denominator of [`GROUND_LIT_NUMER`].
const GROUND_LIT_DENOM: u16 = 3;

/// A carpet tile's side, in logical units: seams on this grid are what turn a
/// flat tone into ground you could walk on.
const GROUND_TILE: u16 = 4;
/// How many ramp stops a seam sits under the ground it crosses.
const SEAM_LEVEL: i8 = -1;
/// The smallest tile, in art pixels, that its seams leave reading as ground;
/// seams on a smaller one turn the ground to plaid.
const MIN_SEAMED_TILE: u16 = 8;

/// A rug's lattice pitch, in art pixels: diamonds this far apart.
const RUG_LATTICE: u16 = 6;
/// How many ramp stops the lattice sits under the rug's field: a pattern woven
/// in, not printed on.
const RUG_MOTIF_LEVEL: i8 = -1;
/// How many ramp stops a rug's fringe sits over its trim: loose threads catch
/// the light the bound edge doesn't.
const RUG_FRINGE_LEVEL: i8 = 2;

/// How many ramp levels the lit glass sits below its glow colour.
const SCREEN_GLASS_LEVEL: i8 = -3;

/// How many ramp levels a lit screen's text sits above its glow colour: bright
/// lines on the dark glass.
const SCREEN_TEXT_LEVEL: i8 = 9;

impl Screen {
    /// `art` with its screen keys showing this screen.
    fn on(self, art: crate::sprite::RecolorableFrame<'_>) -> Option<crate::sprite::Frame> {
        match self {
            Self::Off => None,
            Self::Standby(glass) => {
                Some(art.recolored(&[(crate::pack::SCREEN_GLASS_KEY, Some(glass))]))
            }
            Self::Lit { glow, scan } => Some(scanline(relight_screen(art, glow), art, glow, scan)),
        }
    }
}

/// Paint `run`: its plate, if it has one, and its ring, then its spans end to
/// end from its [line's start](line_start), each icon in `pack`'s art.
pub(crate) fn paint_run(run: &TextRun, (pack, pen): (&OfficeArt, Pen), buf: &mut RgbBuffer) {
    let rect = run_rect(run, pen);
    if let Some(ground) = run.plate {
        pen.fill(buf, rect, ground);
    }
    if let (Some(insets), Some(ink)) = (ring_insets(run), run.ring) {
        for edge in insets.ring(rect) {
            pen.fill(buf, edge, ink);
        }
    }
    let (ArtPx(mut x), y) = line_start(run, rect);
    for span in &run.spans {
        let at = (ArtPx(x), y);
        match &span.content {
            Content::Text(text) => crate::cutaway::text::paint(pen, buf, at, text, span.ink),
            Content::Icon(icon) => {
                crate::cutaway::text::paint_icon(pen, buf, at, (pack, *icon), span.ink)
            }
        }
        x += crate::display::text::advance(span.text()).0;
    }
}

/// Paint `frame`'s `office` into `buf` as an orthographic cutaway — the
/// classic painter's sibling, not its successor — as `showing` says: its
/// windows on the sky from its floor's altitude, its room lit for the hour.
pub fn render_cutaway(
    frame: &SimFrame,
    office: Office<'_>,
    showing: Showing<'_>,
    (cache, outside): (&mut CutawayCache, &mut crate::outside::OutsideCache),
    buf: &mut RgbBuffer,
) {
    let list = compose(
        frame,
        office,
        showing,
        (&mut crate::display::compose::LightCache::default(), outside),
    );
    paint(&list, cache, buf);
}

/// Paint `list` whole: its backdrop, then its pieces over it, then its dim
/// over all, which the cached backdrop never holds.
pub(crate) fn paint(list: &DisplayList<'_>, cache: &mut CutawayCache, buf: &mut RgbBuffer) {
    let key = BackdropKey {
        backdrop: list.backdrop(),
        carpet: list.carpet(),
        scale: list.scale(),
        pen: Pen::for_pack(list.scale(), list.pack()),
    };
    cache.backdrop.stamp(
        key,
        |k, layer| paint_backdrop(&k.backdrop, k.carpet, k.scale, k.pen, layer),
        buf,
    );
    paint_list(list, cache, buf);
    list.dim().apply(buf);
}

/// All [`paint_backdrop`] reads. Held owning its backdrop, probed borrowing
/// the list's.
#[derive(Debug, PartialEq)]
pub(crate) struct BackdropKey<B = Backdrop> {
    backdrop: B,
    carpet: Dithered<Carpet>,
    scale: RenderScale,
    /// The pack's art grid at `scale`.
    pen: Pen,
}

impl PartialEq<BackdropKey<&Backdrop>> for BackdropKey {
    fn eq(&self, probe: &BackdropKey<&Backdrop>) -> bool {
        // Destructured whole, so a field added to the key fails to compile
        // here until it is compared.
        let Self {
            backdrop,
            carpet,
            scale,
            pen,
        } = self;
        *backdrop == *probe.backdrop
            && *carpet == probe.carpet
            && *scale == probe.scale
            && *pen == probe.pen
    }
}

impl From<BackdropKey<&Backdrop>> for BackdropKey {
    fn from(
        BackdropKey {
            backdrop,
            carpet,
            scale,
            pen,
        }: BackdropKey<&Backdrop>,
    ) -> Self {
        Self {
            backdrop: backdrop.clone(),
            carpet,
            scale,
            pen,
        }
    }
}

/// Everything under the list's pieces, none of which moves within a
/// backdrop, carpet, pack and scale.
fn paint_backdrop(
    backdrop: &Backdrop,
    carpet: Dithered<Carpet>,
    scale: RenderScale,
    pen: Pen,
    buf: &mut RgbBuffer,
) {
    paint_carpet(backdrop, carpet, pen, buf);
    for &(at, covering) in &backdrop.coverings {
        match covering {
            Covering::Rug {
                trim,
                accent,
                field,
            } => paint_rug(at, [trim, accent, field], pen, buf),
            Covering::Runner { base, stripe, edge } => {
                paint_runner(at, [base, stripe, edge], pen, buf);
            }
        }
    }
    paint_wall(backdrop, scale, pen, buf);
}

/// Paint `list` over the backdrop `buf` holds: every piece's shadow
/// ([`ground_shadow`](crate::display::compose::ground_shadow))
/// first, then the pieces back to front as by day, noting which pixels glow of
/// their own ([`Glow`](crate::cutaway::light::Glow)), and last one pass
/// ([`net_pass`](crate::cutaway::light::net_pass)) takes every other pixel to
/// the hour: darkened with the room and lifted by its lights at once, so no
/// pixel is darkened twice or darkened and relit.
pub(crate) fn paint_list(list: &DisplayList<'_>, cache: &mut CutawayCache, buf: &mut RgbBuffer) {
    let pen = Pen::for_pack(list.scale(), list.pack());
    paint_ground_shadows(
        list.pieces().iter().filter_map(|p| p.shadow),
        crate::ground::shadow_strength(list.ambient().darkness()),
        pen,
        buf,
    );
    let emission = paint_pieces(list, cache, buf);
    let lights: Vec<&crate::display::light::LightView> =
        list.lights().iter().map(|l| &*l.view).collect();
    let (art_xs, art_ys) = pen.writable_art(buf);
    let lit = ArtRect {
        x: ArtPx(art_xs.start),
        y: ArtPx(art_ys.start),
        w: ArtPx(art_xs.end - art_xs.start),
        h: ArtPx(art_ys.end - art_ys.start),
    };
    crate::cutaway::light::net_pass(
        lit,
        &lights,
        (list.ambient(), list.flash()),
        &emission,
        pen,
        &mut cache.net_colours,
        buf,
    );
    cache.emission = Some(emission);
}

/// Paint every piece but the lights, back to front as by day, and return the
/// [`Emission`](crate::cutaway::light::Emission) they leave.
fn paint_pieces(
    list: &DisplayList<'_>,
    cache: &mut CutawayCache,
    buf: &mut RgbBuffer,
) -> crate::cutaway::light::Emission {
    use crate::cutaway::light::{Emission, Glow};
    let (w, h) = (buf.width(), buf.height());
    let (xs, ys) = buf.writable();
    let mut emission = cache
        .emission
        .take()
        .filter(|e| e.fits(w, h))
        .unwrap_or_else(|| Emission::new(w, h));
    emission.reset(xs.clone(), ys.clone());
    let mut marks = cache
        .marks
        .take()
        .filter(|m| (m.width(), m.height()) == (w, h))
        .unwrap_or_else(|| RgbBuffer::filled(w, h, NO_MARK));
    let pen = Pen::for_pack(list.scale(), list.pack());
    for piece in list.pieces() {
        let (x0, y0) = (
            list.scale().to_buffer(piece.span.x0),
            list.scale().to_buffer(piece.span.y0),
        );
        let x1 = list.scale().to_buffer(piece.span.x1 + 1).min(w);
        let y1 = list.scale().to_buffer(piece.span.y1 + 1).min(h);
        // what it can write here: a piece off a repaint's clip paints nothing
        let (cx0, cx1) = (x0.max(xs.start), x1.min(xs.end));
        let (cy0, cy1) = (y0.max(ys.start), y1.min(ys.end));
        if cx0 >= cx1 || cy0 >= cy1 {
            continue;
        }
        let epoch = buf.begin_writes();
        paint_piece(
            &piece.kind,
            list.pack(),
            list.recolours(),
            list.scale(),
            cache,
            buf,
        );
        let glowing = mark(
            &piece.kind,
            (list.pack(), list.scale()),
            &mut cache.art,
            &mut marks,
        );
        for y in cy0..cy1 {
            for x in cx0..cx1 {
                if !buf.written_in(x, y, epoch) {
                    continue;
                }
                let marked = || match marks.get(x, y) {
                    EMISSIVE_MARK => Glow::Emissive,
                    SHADED_MARK => Glow::Shaded,
                    _ => Glow::Lit,
                };
                let glow = match piece.kind.emits() {
                    Emits::Pane(view)
                        if view.shows((
                            pen.art_of_buffer(BufferPx(x)).0,
                            pen.art_of_buffer(BufferPx(y)).0,
                        )) =>
                    {
                        Glow::Pane
                    }
                    Emits::Emissive => Glow::Emissive,
                    Emits::ByArt if glowing => marked(),
                    Emits::Pane(_) | Emits::ByArt | Emits::Lit => Glow::Lit,
                };
                emission.set(x, y, glow);
            }
        }
        if glowing {
            for y in y0..y1 {
                for x in x0..x1 {
                    marks.put(x, y, NO_MARK);
                }
            }
        }
    }
    buf.end_writes();
    cache.marks = Some(marks);
    emission
}

/// [`mark_glow`]'s marks: colours no art is painted in, only a scratch buffer.
const NO_MARK: crate::sprite::Rgb = crate::sprite::Rgb { r: 0, g: 0, b: 0 };
const EMISSIVE_MARK: crate::sprite::Rgb = crate::sprite::Rgb { r: 1, g: 0, b: 0 };
const SHADED_MARK: crate::sprite::Rgb = crate::sprite::Rgb { r: 2, g: 0, b: 0 };

/// Mark into `marks` where `kind` glows of its own, never outside its span,
/// the most [`paint_pieces`] clears; whether it marked any.
fn mark(
    kind: &PieceKind,
    drawn: (&OfficeArt, RenderScale),
    art_cache: &mut ArtCache,
    marks: &mut RgbBuffer,
) -> bool {
    match *kind {
        PieceKind::Desk { at, desk, screen } => {
            mark_glow(at, desk, screen, drawn, art_cache, marks)
        }
        PieceKind::Prop { at, art } | PieceKind::Animated { at, art } => {
            mark_bulbs(Placed::Centred(at), art, drawn, art_cache, marks)
        }
        PieceKind::Hung { at, sprite } => mark_bulbs(
            Placed::TopLeft(at),
            Art::still(sprite),
            drawn,
            art_cache,
            marks,
        ),
        PieceKind::Door { at, frame } => mark_bulbs(
            Placed::TopLeft(at),
            Art {
                sprite: PackPiece::Door,
                frame,
                flip: Flip::None,
            },
            drawn,
            art_cache,
            marks,
        ),
        PieceKind::Window { .. }
        | PieceKind::WallSeg { .. }
        | PieceKind::Chair { .. }
        | PieceKind::DeskProp(_)
        | PieceKind::DeskFront { .. }
        | PieceKind::Creature { .. }
        | PieceKind::PropBand { .. }
        | PieceKind::Table { .. }
        | PieceKind::Character { .. }
        | PieceKind::Effect(_)
        | PieceKind::Neon { .. }
        | PieceKind::Clock { .. }
        | PieceKind::Text { .. } => false,
    }
}

/// Mark into `marks` where the desk [`paint_desk`] drew at `at` glows of its own:
/// its screen, emissive when on and shaded when off, and its lamp's bulb. Each
/// is where the art draws its key, found by painting that key transparent, so
/// it is the art's own at any density. Returns whether it marked anything.
fn mark_glow(
    at: crate::layout::Point,
    which: Desk,
    screen: Screen,
    (pack, scale): (&OfficeArt, RenderScale),
    art: &mut ArtCache,
    marks: &mut RgbBuffer,
) -> bool {
    use crate::pack::{DESK_BULB_KEY, SCREEN_GLASS_KEY, SCREEN_TEXT_KEY};
    let art_name = which.piece();
    let span = desk_span(pack, which, at, scale);
    let desk = crate::pack::densest_frame(pack, art_name, 0, scale);
    let screen_cells = art
        .cells(art_name, 0, &desk, &[SCREEN_GLASS_KEY, SCREEN_TEXT_KEY])
        .to_vec();
    let bulb = art.cells(art_name, 0, &desk, &[DESK_BULB_KEY]);
    let screen_mark = match screen {
        Screen::Off => SHADED_MARK,
        Screen::Standby(_) | Screen::Lit { .. } => EMISSIVE_MARK,
    };
    let (w, h) = (desk.frame.width(), desk.frame.height());
    let pixels: Vec<crate::sprite::Pixel> = bulb
        .iter()
        .zip(&screen_cells)
        .map(|(&bulb, &screen)| {
            if bulb {
                Some(EMISSIVE_MARK)
            } else if screen {
                Some(screen_mark)
            } else {
                None
            }
        })
        .collect();
    let any = pixels.iter().any(Option::is_some);
    let only = crate::sprite::Frame::from_pixels(w, h, pixels);
    let (x, top_y) = (scale.to_buffer(span.x0), scale.to_buffer(span.y0));
    blit_frame_scaled(&only, x, top_y, desk.blit_at, marks);
    any
}

/// Where a piece's art lands: centred on a layout point, or from its top-left.
#[derive(Clone, Copy)]
enum Placed {
    Centred(Point),
    TopLeft(Point),
}

/// Mark emissive where `art` draws a bulb
/// ([`DESK_BULB_KEY`](crate::pack::DESK_BULB_KEY)); whether it marked
/// any.
fn mark_bulbs(
    placed: Placed,
    art: Art,
    (pack, scale): (&OfficeArt, RenderScale),
    cache: &mut ArtCache,
    marks: &mut RgbBuffer,
) -> bool {
    let dense = crate::pack::densest_frame(pack, art.sprite, art.frame, scale);
    let bulbs = cache.cells(art.sprite, art.frame, &dense, &[crate::pack::DESK_BULB_KEY]);
    if !bulbs.iter().any(|&b| b) {
        return false;
    }
    let only = crate::sprite::Frame::from_pixels(
        dense.frame.width(),
        dense.frame.height(),
        bulbs.iter().map(|&b| b.then_some(EMISSIVE_MARK)).collect(),
    );
    let (x, y) = placed.top_left(dense.logical, scale);
    blit_frame_scaled(&art.flip.turn(only), x, y, dense.blit_at, marks);
    true
}

impl Placed {
    /// The buffer top-left of `logical`-sized art placed so.
    fn top_left(self, logical: (u16, u16), scale: RenderScale) -> (u16, u16) {
        match self {
            Self::Centred(at) => centred_top_left(at, logical, scale),
            Self::TopLeft(at) => (scale.to_buffer(at.x), scale.to_buffer(at.y)),
        }
    }
}

/// What the cutaway keeps across frames, for one pack.
#[derive(Debug, Default)]
pub struct CutawayCache {
    /// Under every piece: drawn once a backdrop, carpet and scale.
    backdrop: crate::cached_layer::CachedLayer<BackdropKey>,
    figures: crate::frame_cache::FrameCache,
    art: ArtCache,
    net_colours: crate::cutaway::light::NetMemo,
    /// [`paint_pieces`]'s frame-sized scratch, kept so a repaint of a few
    /// rects allocates and clears none of it whole.
    emission: Option<crate::cutaway::light::Emission>,
    /// Every pixel [`NO_MARK`] between pieces: each glowing piece clears the
    /// span it marked.
    marks: Option<RgbBuffer>,
}

impl CutawayCache {
    /// Drop the figures of agents no longer in `scene`.
    pub(crate) fn evict_missing(&mut self, scene: &pixtuoid_core::SceneState) {
        self.figures.evict_missing(scene);
    }

    /// How many recoloured figure frames it holds.
    #[cfg(test)]
    pub(crate) fn figures_len(&self) -> usize {
        self.figures.len()
    }
}

/// Art found by recolouring, kept across frames; keyed by sprite name, so one
/// cache serves one pack.
#[derive(Debug, Default)]
pub(crate) struct ArtCache {
    cells: std::collections::HashMap<(PackPiece, usize, u16, &'static [char]), Vec<bool>>,
    screens: std::collections::HashMap<(PackPiece, u16, Screen), crate::sprite::Frame>,
}

impl ArtCache {
    fn cells(
        &mut self,
        sprite: PackPiece,
        frame: usize,
        dense: &crate::pack::DenseFrame<'_>,
        keys: &'static [char],
    ) -> &[bool] {
        self.cells
            .entry((sprite, frame, dense.density.get(), keys))
            .or_insert_with(|| drawn_in(dense, keys))
    }

    fn desk(
        &mut self,
        art: PackPiece,
        desk: &crate::pack::DenseFrame<'_>,
        screen: Screen,
    ) -> Option<&crate::sprite::Frame> {
        match screen {
            Screen::Off => None,
            Screen::Standby(_) | Screen::Lit { .. } => Some(
                self.screens
                    .entry((art, desk.density.get(), screen))
                    .or_insert_with(|| {
                        screen
                            .on(desk.recolorable)
                            .unwrap_or_else(|| desk.frame.clone())
                    }),
            ),
        }
    }
}

/// Ramp stops a shadow steps the ground at its centre, per unit of
/// [`shadow_strength`].
///
/// [`shadow_strength`]: crate::ground::shadow_strength
const SHADOW_STOPS_PER_STRENGTH: f32 = 6.0;

/// Step the ground darker under `shadows`, toward each one's centre: its falloff
/// at `strength`, rounded to whole ramp stops by
/// [`nearest`](crate::dither::nearest) on the art grid, over their
/// [`Depths`](crate::ground::Depths). A shadow is the ground it falls on,
/// darker, never a colour of its own.
fn paint_ground_shadows(
    shadows: impl Iterator<Item = crate::ground::Contact> + Clone,
    strength: f32,
    pen: Pen,
    buf: &mut RgbBuffer,
) {
    // a shadow off the writable art darkens none of it, so a repaint of a few
    // rects builds only theirs
    let (art_xs, art_ys) = pen.writable_art(buf);
    let per = pen.density();
    let near = shadows.filter(|c| {
        let ((x0, y0), (x1, y1)) = c.bounds();
        let cells = |a: u16, b: u16| a.saturating_mul(per.get())..b.saturating_mul(per.get());
        let (cx, cy) = (cells(x0, x1), cells(y0, y1));
        cx.start < art_xs.end
            && art_xs.start < cx.end
            && cy.start < art_ys.end
            && art_ys.start < cy.end
    });
    let Some(depths) = crate::ground::Depths::of(near, per) else {
        return;
    };
    let mut stepped: Vec<crate::dither::Stepped> = Vec::new();
    for (ax, ay, depth) in depths.cells() {
        if !art_xs.contains(&ax) || !art_ys.contains(&ay) {
            continue;
        }
        let level = crate::dither::nearest(depth * strength * SHADOW_STOPS_PER_STRENGTH, ax, ay);
        if level == 0 {
            continue;
        }
        while stepped.len() < usize::from(level) {
            stepped.push(crate::dither::Stepped::new(-(stepped.len() as i8 + 1)));
        }
        let r = ArtRect {
            x: ArtPx(ax),
            y: ArtPx(ay),
            w: ArtPx(1),
            h: ArtPx(1),
        };
        let memo = &mut stepped[usize::from(level) - 1];
        pen.recolour(buf, r, |_, _, under| memo.of(under));
    }
}

/// Paint one piece of the display list.
fn paint_piece(
    kind: &PieceKind,
    pack: &OfficeArt,
    recolours: &Recolours,
    scale: RenderScale,
    cache: &mut CutawayCache,
    buf: &mut RgbBuffer,
) {
    match *kind {
        PieceKind::Desk { at, desk, screen } => {
            paint_desk(at, desk, screen, (pack, scale), &mut cache.art, buf);
        }
        PieceKind::DeskFront { at, desk, screen } => {
            paint_desk_front(at, desk, screen, (pack, scale), &mut cache.art, buf);
        }
        PieceKind::Chair { at } => paint_chair(at, pack, scale, buf),
        PieceKind::DeskProp(prop) => {
            paint_desk_prop(prop, pack, &recolours.desk_props, scale, buf);
        }
        PieceKind::Creature {
            at, art, degraded, ..
        } => {
            paint_creature(at, art, degraded, pack, scale, buf);
        }
        PieceKind::Effect(ref riding) => riding.paint(buf),
        PieceKind::Character {
            ref figure, chair, ..
        } => {
            paint_figure(figure, pack, scale, &mut cache.figures, buf);
            // The sitter's own chair, straight after them: one piece, so
            // nothing can sort between a person and the chair they sit in.
            if let Some(at) = chair {
                paint_chair(at, pack, scale, buf);
            }
        }
        PieceKind::Prop { at, art } | PieceKind::Animated { at, art } => {
            paint_art(at, art, pack, &recolours.art, scale, buf)
        }
        PieceKind::PropBand { at, sprite, rows } => {
            paint_prop_band(at, sprite, rows, pack, scale, buf);
        }
        PieceKind::Table { at } => paint_table(at, pack, scale, buf),
        PieceKind::Door { at, frame } => paint_door(at, frame, pack, scale, buf),
        PieceKind::Neon {
            at,
            tube,
            hue,
            interior,
        } => paint_neon(at, [tube, hue, interior], Pen::for_pack(scale, pack), buf),
        PieceKind::Clock { at, reading, hand } => {
            paint_clock(at, reading, hand, pack, &recolours.art, scale, buf);
        }
        PieceKind::WallSeg {
            piece,
            rows: (y0, y1),
            trim,
        } => crate::cutaway::wall::paint_wall(buf, trim, piece, y0..y1, Pen::for_pack(scale, pack)),
        PieceKind::Window { ref view, frame } => {
            paint_window(view, frame, Pen::for_pack(scale, pack), buf);
        }
        PieceKind::Hung { at, sprite } => paint_wall_decor(at, sprite, pack, scale, buf),
        PieceKind::Text { ref run } => paint_run(run, (pack, Pen::for_pack(scale, pack)), buf),
    }
}

/// The deepest a noon shadow steps the ground.
#[cfg(test)]
fn deepest_shadow_stop() -> i8 {
    let stops = crate::ground::shadow_strength(NOON_DARKNESS) * SHADOW_STOPS_PER_STRENGTH;
    let phases = 0..crate::dither::PERIOD;
    phases
        .clone()
        .flat_map(|y| {
            phases
                .clone()
                .map(move |x| crate::dither::nearest(stops, x, y))
        })
        .max()
        .map_or(0, |deepest| deepest as i8)
}

/// A clear noon's darkness, the hour the shadow tests pin.
#[cfg(test)]
const NOON_DARKNESS: f32 = 0.0;

/// Compares `variant`, a cutaway render of a pack whose desks ship density
/// variants, against `base`, one of `base_pack`, on the two things the cutaway
/// promises: the art
/// lands exactly where the base's does (every pixel outside each desk's foot — its
/// face band and the shadows under it — is identical), and a variant, which draws
/// its own front, has floor under its art where the base gets a derived face.
#[cfg(test)]
pub(crate) fn assert_variant_desk_foot(
    variant: &[crate::sprite::Rgb],
    base: &[crate::sprite::Rgb],
    layout: &crate::layout::SceneLayout,
    base_pack: &OfficeArt,
    theme: &crate::theme::Theme,
    scale: RenderScale,
    now: std::time::SystemTime,
) {
    // The room darkens every pixel by the hour's steps; its lights must be off.
    let tones = crate::atmosphere::SkyTones::resolve(&crate::sky::Sky::clock(now), theme);
    let ambient = crate::display::light::Ambient::of(&tones);
    let mut ground = RgbBuffer::filled(
        scale.to_buffer(layout.buf_w),
        scale.to_buffer(layout.buf_h),
        theme.surface.bg_fallback,
    );
    paint_carpet(
        &Backdrop::of(layout, theme),
        tones.carpet(theme),
        Pen::for_pack(scale, base_pack),
        &mut ground,
    );
    let buf_w = usize::from(scale.to_buffer(layout.buf_w));
    let face = desk_front_h();
    // Each desk's columns and the first row below its art, in logical units.
    let feet: Vec<(u16, u16, u16)> = layout
        .home_desks
        .iter()
        .map(|&crate::layout::HomeDesk { at: d, facing }| {
            let art = Desk::facing(facing).piece();
            let (w, h) = art_size(base_pack, art);
            (
                d.x,
                d.x + w - 1,
                crate::pack::desk_art_top(base_pack, d.y, h) + h,
            )
        })
        .collect();
    assert!(!feet.is_empty(), "the office must have a desk");
    // A desk's foot: its face band, and where a variant's shadow and the base's
    // fall, one face band apart.
    let in_foot = |x: u16, y: u16| {
        feet.iter().any(|&(x0, x1, below)| {
            let shadow = |base: u16| {
                let ((sx0, sy0), (sx1, sy1)) =
                    crate::ground::Contact::under(x0, x1 - x0 + 1, base).bounds();
                (sx0..sx1).contains(&x) && (sy0..sy1).contains(&y)
            };
            ((x0..=x1).contains(&x) && (below..=below + face).contains(&y))
                || shadow(below)
                || shadow(below + face)
        })
    };
    let differs_outside = (0..variant.len()).find(|&i| {
        let (x, y) = ((i % buf_w) as u16, (i / buf_w) as u16);
        variant[i] != base[i] && !in_foot(scale.logical(x), scale.logical(y))
    });
    assert_eq!(
        differs_outside, None,
        "the desk art moved with its density (first differing buffer pixel)"
    );
    let at = |px: &[crate::sprite::Rgb], x: u16, y: u16| {
        px[usize::from(scale.to_buffer(y)) * buf_w + usize::from(scale.to_buffer(x))]
    };
    let deepest = deepest_shadow_stop();
    let ground_or_its_shadow = |c: crate::sprite::Rgb, floor: crate::sprite::Rgb| {
        (0..=deepest).any(|k| c == ambient.on(floor.ramp(-k)))
    };
    // The desk's own edge column: a sitter and their chair stand centred on it.
    for &(x0, _, below) in &feet {
        let x = x0 + 1;
        for y in below..=below + face {
            assert!(
                ground_or_its_shadow(at(variant, x, y), at(ground.as_slice(), x, y)),
                "a variant draws its own front: under its art lies floor or its shadow"
            );
        }
        assert!(
            !ground_or_its_shadow(at(base, x, below), at(ground.as_slice(), x, below)),
            "the base gets a derived face under its art"
        );
    }
}

impl Flip {
    fn turn(self, frame: crate::sprite::Frame) -> crate::sprite::Frame {
        match self {
            Self::None => frame,
            Self::Horizontal => frame.mirror_horizontal(),
        }
    }
}

/// Paint the north wall band: the wall, the posts between its windows and its
/// trim. The windows are list pieces
/// ([`push_windows`](crate::display::compose::push_windows)).
///
/// Its height is the backdrop's `wall_band_h`, not `top_margin`: the rows
/// between are floor the agents walk on, so a band drawn to `top_margin` would
/// paint over them.
fn paint_wall(backdrop: &Backdrop, scale: RenderScale, pen: Pen, buf: &mut RgbBuffer) {
    let band_h = backdrop.wall_band_h;
    if band_h == 0 {
        return;
    }
    let tones = backdrop.tones;
    let s = scale.get();
    let w = scale.to_buffer(backdrop.size.w);
    let wall = Ramp::from_base(tones.wall);
    slab(buf, 0, 0, w, scale.to_buffer(band_h), &wall, scale);
    let rows = backdrop.window_rows.clone();
    let window_h = rows.end - rows.start;
    for post in &backdrop.posts {
        let cell = ArtRect {
            x: pen.art(post.start),
            y: pen.art(rows.start),
            w: pen.art(post.end - post.start),
            h: pen.art(window_h),
        };
        pen.fill(buf, cell, tones.window_frame);
    }
    fill(
        buf,
        0,
        scale.to_buffer(backdrop.trim_row),
        w,
        s,
        tones.wall_trim,
    );
    // The wall's contact line: the ground under it, a shade step down.
    let mut contact = crate::dither::Stepped::new(crate::cutaway::shade::RAMP_SHADE_LEVEL);
    pen.recolour(
        buf,
        ArtRect {
            x: ArtPx(0),
            y: pen.art(band_h),
            w: pen.art(backdrop.size.w),
            h: pen.art(1),
        },
        |_, _, under| contact.of(under),
    );
}

/// Paint a [`PieceKind::Window`], each cell an art pixel.
fn paint_window(view: &WindowView, frame: crate::sprite::Rgb, pen: Pen, buf: &mut RgbBuffer) {
    let cell = |(x, y)| ArtRect {
        x: ArtPx(x),
        y: ArtPx(y),
        w: ArtPx(1),
        h: ArtPx(1),
    };
    for (at, c) in view.every() {
        pen.fill(buf, cell(at), c.unwrap_or(frame));
    }
}

/// The carpet [`paint_carpet_tones`] lays, each art pixel in the tones
/// `carpet` dithers it to.
fn paint_carpet(backdrop: &Backdrop, carpet: Dithered<Carpet>, pen: Pen, buf: &mut RgbBuffer) {
    if let Some(tones) = carpet.as_solid() {
        return paint_carpet_tones(backdrop, tones, pen, buf);
    }
    let [from, to] = carpet.ends();
    paint_carpet_tones(backdrop, from, pen, buf);
    let mut incoming = buf.clone();
    paint_carpet_tones(backdrop, to, pen, &mut incoming);
    pen.take_where(buf, &incoming, |x, y| carpet.takes_to(x.0, y.0));
}

/// The carpet, lit near the windows, falling off south and laid in tiles, on
/// the art grid: every edge, dither step and seam lands on an art pixel,
/// whatever the scale.
fn paint_carpet_tones(backdrop: &Backdrop, carpet: Carpet, pen: Pen, buf: &mut RgbBuffer) {
    let Carpet { lit, base, dark } = carpet;

    let h = pen.art(backdrop.size.h);
    let w = pen.art(backdrop.size.w);
    let band = |y0: u16, rows: u16| ArtRect {
        x: ArtPx(0),
        y: ArtPx(y0),
        w,
        h: ArtPx(rows),
    };
    pen.fill(buf, band(0, h.0), base);

    // Anchored at the wall's foot, where the ground begins, not buffer row 0: the
    // wall band paints over the top of the buffer, so a lit zone anchored there
    // would start behind it.
    let ground_top = pen.art(backdrop.wall_band_h).0;
    let ground_h = h.0.saturating_sub(ground_top);

    // The lit share of the ground: its first half solid, dithering to base by its
    // end, then a final fall to dark at the south edge.
    let lit_h = ground_h * GROUND_LIT_NUMER / GROUND_LIT_DENOM;
    pen.fill(buf, band(ground_top, lit_h / 2), lit);
    pen.dither_band(
        buf,
        ArtPx(ground_top + lit_h / 2),
        ArtPx(ground_top + lit_h),
        base,
        lit,
    );
    pen.dither_band(buf, ArtPx(h.0.saturating_sub(lit_h / 2)), h, dark, base);

    let tile = pen.art(GROUND_TILE);
    if tile.0 >= MIN_SEAMED_TILE {
        pen.shade_grid(buf, band(ground_top, ground_h), tile, SEAM_LEVEL);
    }
}

/// A rug on the art grid: the classic's trim, accent line and field, then a
/// woven lattice and fringe at its short ends wherever the density has room.
fn paint_rug(
    rug: crate::layout::Bounds,
    [rug_trim, rug_accent, rug_field]: [crate::sprite::Rgb; 3],
    pen: Pen,
    buf: &mut RgbBuffer,
) {
    let d = pen.density().get();
    let (x0, y0, w, h) = (
        pen.art(rug.x).0,
        pen.art(rug.y).0,
        pen.art(rug.width).0,
        pen.art(rug.height).0,
    );
    let rect = |x: u16, y: u16, w: u16, h: u16| ArtRect {
        x: ArtPx(x),
        y: ArtPx(y),
        w: ArtPx(w),
        h: ArtPx(h),
    };
    let inset = |i: u16| {
        rect(
            x0 + i,
            y0 + i,
            w.saturating_sub(2 * i),
            h.saturating_sub(2 * i),
        )
    };
    // Half a logical unit wide, so at 1x this is the classic's rug, cell for cell.
    let trim = (d / 2).max(1);
    pen.fill(buf, inset(0), rug_trim);
    pen.fill(buf, inset(trim), rug_accent);
    pen.fill(buf, inset(trim + 1), rug_field);
    if d == 1 {
        return;
    }

    // Mirrored about the rug's centre column, so it sits square in the field.
    let motif = rug_field.ramp(RUG_MOTIF_LEVEL);
    let lattice = inset(trim + 3);
    let (art_xs, art_ys) = pen.writable_art(buf);
    for y in lattice.y.0.max(art_ys.start)..(lattice.y.0 + lattice.h.0).min(art_ys.end) {
        for x in lattice.x.0.max(art_xs.start)..(lattice.x.0 + lattice.w.0).min(art_xs.end) {
            let (dx, dy) = (x - x0, y - y0);
            if (dx + dy).is_multiple_of(RUG_LATTICE)
                || (w - 1 - dx + dy).is_multiple_of(RUG_LATTICE)
            {
                pen.fill(buf, rect(x, y, 1, 1), motif);
            }
        }
    }
    // A tassel every other pixel along each short end, as long as the trim.
    let tassel = rug_trim.ramp(RUG_FRINGE_LEVEL);
    if w >= h {
        for y in (y0 + 1..y0 + h - 1).step_by(2) {
            pen.fill(buf, rect(x0.saturating_sub(trim), y, trim, 1), tassel);
            pen.fill(buf, rect(x0 + w, y, trim, 1), tassel);
        }
    } else {
        for x in (x0 + 1..x0 + w - 1).step_by(2) {
            pen.fill(buf, rect(x, y0.saturating_sub(trim), 1, trim), tassel);
            pen.fill(buf, rect(x, y0 + h, 1, trim), tassel);
        }
    }
}

fn paint_desk(
    at: crate::layout::Point,
    which: Desk,
    screen: Screen,
    (pack, scale): (&OfficeArt, RenderScale),
    art: &mut ArtCache,
    buf: &mut RgbBuffer,
) {
    let art_name = which.piece();
    let span = desk_span(pack, which, at, scale);
    let desk = crate::pack::densest_frame(pack, art_name, 0, scale);
    let (x, top_y) = (scale.to_buffer(span.x0), scale.to_buffer(span.y0));
    blit_frame_scaled(
        art.desk(art_name, &desk, screen).unwrap_or(desk.frame),
        x,
        top_y,
        desk.blit_at,
        buf,
    );
    paint_derived_face(
        &desk,
        (x, top_y),
        face_rows(pack, art_name, scale),
        scale,
        buf,
    );
}

/// The front of the `which` desk drawn at `at`, over its props: drawn on the
/// desk's canvas as [`paint_desk`] draws the desk.
fn paint_desk_front(
    at: crate::layout::Point,
    which: Desk,
    screen: Screen,
    (pack, scale): (&OfficeArt, RenderScale),
    art: &mut ArtCache,
    buf: &mut RgbBuffer,
) {
    let Some(front) = which.front() else {
        return;
    };
    let span = desk_span(pack, which, at, scale);
    let f = crate::pack::densest_frame(pack, front, 0, scale);
    let (x, top_y) = (scale.to_buffer(span.x0), scale.to_buffer(span.y0));
    blit_frame_scaled(
        art.desk(front, &f, screen).unwrap_or(f.frame),
        x,
        top_y,
        f.blit_at,
        buf,
    );
}

/// `rows` of front face under `art` drawn at `top_left`, in the material of its
/// bottom row ([`dominant_opaque_row`]).
fn paint_derived_face(
    art: &crate::pack::DenseFrame<'_>,
    top_left: (u16, u16),
    rows: u16,
    scale: RenderScale,
    buf: &mut RgbBuffer,
) {
    if rows == 0 {
        return;
    }
    let Some(material) = dominant_opaque_row(art.frame, art.frame.height().saturating_sub(1))
    else {
        return;
    };
    // The drawn size, from the logical size: variant art blits at `blit_at`, so
    // its own pixel size times the scale would drop a base's face a whole piece
    // low.
    let (drawn_w, drawn_h) = (
        scale.to_buffer(art.logical.0),
        scale.to_buffer(art.logical.1),
    );
    slab(
        buf,
        top_left.0,
        top_left.1 + drawn_h,
        drawn_w,
        scale.to_buffer(rows),
        &Ramp::from_base(material),
        scale,
    );
}

/// The desk art with its screen lit in `glow`: the glass takes a dark step of
/// the glow and its dim content turns to bright text. Recoloring the pack's own
/// screen KEYS ([`SCREEN_GLASS_KEY`](crate::pack::SCREEN_GLASS_KEY),
/// [`SCREEN_TEXT_KEY`](crate::pack::SCREEN_TEXT_KEY)), rather than
/// painting a band over the desk or matching a colour, lights exactly the screen
/// the art drew — at whatever density it was drawn — and no other pixel, even
/// one the same colour as the glass.
fn relight_screen(
    art: crate::sprite::RecolorableFrame<'_>,
    glow: crate::sprite::Rgb,
) -> crate::sprite::Frame {
    art.recolored(&[
        (
            crate::pack::SCREEN_GLASS_KEY,
            Some(glow.ramp(SCREEN_GLASS_LEVEL)),
        ),
        (
            crate::pack::SCREEN_TEXT_KEY,
            Some(glow.ramp(SCREEN_TEXT_LEVEL)),
        ),
    ])
}

/// `lit`, the screen of `art` relit in `glow`, with its scanline on glass
/// column `scan`: the glass's columns split evenly among the classic's
/// [`SCREEN_GLASS_COLS`](crate::layout::SCREEN_GLASS_COLS).
fn scanline(
    lit: crate::sprite::Frame,
    art: crate::sprite::RecolorableFrame<'_>,
    glow: crate::sprite::Rgb,
    scan: u16,
) -> crate::sprite::Frame {
    use crate::pack::{SCREEN_GLASS_KEY, SCREEN_TEXT_KEY};
    // The glass is wherever the screen keys draw: what clearing them uncovers.
    let bare = art.recolored(&[(SCREEN_GLASS_KEY, None), (SCREEN_TEXT_KEY, None)]);
    let (w, h) = (lit.width(), lit.height());
    let px = |f: &crate::sprite::Frame, x, y| f.get(x, y).copied().flatten();
    let glass = |x, y| px(&lit, x, y).is_some() && px(&bare, x, y).is_none();
    let Some((x0, x1)) = (0..w)
        .filter(|&x| (0..h).any(|y| glass(x, y)))
        .fold(None, |r: Option<(u16, u16)>, x| {
            Some(r.map_or((x, x), |(a, _)| (a, x)))
        })
    else {
        return lit;
    };
    let cols = crate::layout::SCREEN_GLASS_COLS;
    let band = ((x1 - x0 + 1) / (cols.end() - cols.start() + 1)).max(1);
    let line = (x0 + scan * band)..(x0 + (scan + 1) * band);
    let color = crate::effects::look::scanline_color(glow);
    let pixels = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            if line.contains(&x) && glass(x, y) {
                Some(color)
            } else {
                px(&lit, x, y)
            }
        })
        .collect();
    crate::sprite::Frame::from_pixels(w, h, pixels)
}

/// The most common opaque colour in `row` of `frame` — how the cutaway learns a
/// sprite's material without hardcoding it. The front face a top-down sprite
/// never had has to be SOME colour, and the desk's lives in the PACK (the
/// shaded wood, palette key `d`), not the theme, where `furniture.wood_top`
/// reads nearly like the carpet.
fn dominant_opaque_row(frame: &crate::sprite::Frame, row: u16) -> Option<crate::sprite::Rgb> {
    let w = frame.width();
    let mut best: Option<(crate::sprite::Rgb, usize)> = None;
    for x in 0..w {
        let Some(c) = frame.get(x, row).and_then(|p| *p) else {
            continue;
        };
        let n = (0..w)
            .filter(|&i| frame.get(i, row).and_then(|p| *p) == Some(c))
            .count();
        if best.is_none_or(|(_, bn)| n > bn) {
            best = Some((c, n));
        }
    }
    best.map(|(c, _)| c)
}

/// Wall-hung decor, blitted at its own `pos`: [`paint_art`] would centre it, but
/// `WallDecorItem.pos` is TOP-LEFT, like the classic painter's `Pivot::TopLeft`
/// z-sort rather than the centre-pinned furniture, so centring would hang every
/// board up and west of where it belongs.
fn paint_wall_decor(
    pos: crate::layout::Point,
    sprite: PackPiece,
    pack: &OfficeArt,
    scale: RenderScale,
    buf: &mut RgbBuffer,
) {
    let art = crate::pack::densest_frame(pack, sprite, 0, scale);
    blit_frame_scaled(
        art.frame,
        scale.to_buffer(pos.x),
        scale.to_buffer(pos.y),
        art.blit_at,
        buf,
    );
}

fn paint_figure(
    figure: &Figure,
    pack: &OfficeArt,
    scale: RenderScale,
    cache: &mut crate::frame_cache::FrameCache,
    buf: &mut RgbBuffer,
) {
    let Figure { at, ref key, .. } = *figure;
    // The classic painter's own recolor + facing-flip path, through the same
    // cache: a raw pack blit clones one placeholder-palette person per agent.
    let art = crate::character::keyed_character_frame(key, pack, scale, cache);
    // A dressed frame reaches up over its hair, its art's top staying at `at`:
    // whatever rows would start above the buffer are cut, not the whole figure
    // pushed down.
    let blit = i32::from(art.blit_at.get());
    let top = i32::from(scale.to_buffer(at.y)) - i32::from(art.rise) * blit;
    let cut = u16::try_from((-top).max(0).unsigned_abs().div_ceil(blit.unsigned_abs()))
        .unwrap_or(u16::MAX);
    let clipped;
    let frame = if cut == 0 {
        art.frame
    } else {
        let f = art.frame;
        let rows = f.height().saturating_sub(cut);
        let px = (cut..f.height())
            .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
            .map(|(x, y)| f.get(x, y).copied().flatten())
            .collect();
        clipped = crate::sprite::Frame::from_pixels(f.width(), rows, px);
        &clipped
    };
    blit_frame_scaled(
        frame,
        scale.to_buffer(at.x),
        u16::try_from(top + i32::from(cut) * blit).unwrap_or(0),
        art.blit_at,
        buf,
    );
}

/// The meeting table's art, centred on its layout point, over the front face
/// [`face_rows`] derives under a base-density drawing.
fn paint_table(
    at: crate::layout::Point,
    pack: &OfficeArt,
    scale: RenderScale,
    buf: &mut RgbBuffer,
) {
    let table = crate::pack::densest_frame(pack, PackPiece::MeetingTable, 0, scale);
    let (x, y) = centred_top_left(at, table.logical, scale);
    blit_frame_scaled(table.frame, x, y, table.blit_at, buf);
    paint_derived_face(
        &table,
        (x, y),
        face_rows(pack, PackPiece::MeetingTable, scale),
        scale,
        buf,
    );
}

/// Blit `art` centred on `at`, in the theme's colours
/// ([`Recolours::art`](crate::display::Recolours)).
fn paint_art(
    at: Point,
    art: Art,
    pack: &OfficeArt,
    recolours: &[(char, crate::sprite::Pixel)],
    scale: RenderScale,
    buf: &mut RgbBuffer,
) {
    let dense = crate::pack::densest_frame(pack, art.sprite, art.frame, scale);
    let themed = dense.recolorable.recolored(recolours);
    let (x, y) = centred_top_left(at, dense.logical, scale);
    blit_frame_scaled(&art.flip.turn(themed), x, y, dense.blit_at, buf);
}

/// A creature's art centred on `at`, in the pack's own colours as the classic
/// draws it, greyed when `degraded`.
fn paint_creature(
    at: Point,
    art: Art,
    degraded: bool,
    pack: &OfficeArt,
    scale: RenderScale,
    buf: &mut RgbBuffer,
) {
    let dense = crate::pack::densest_frame(pack, art.sprite, art.frame, scale);
    let turned = art.flip.turn(dense.frame.clone());
    let shown = if degraded {
        crate::pixel_painter::palette::degraded_frame(&turned)
    } else {
        turned
    };
    let (x, y) = centred_top_left(at, dense.logical, scale);
    blit_frame_scaled(&shown, x, y, dense.blit_at, buf);
}

/// A desk prop in the theme's cup and paper, turned as its desk turns it.
fn paint_desk_prop(
    prop: StoodProp,
    pack: &OfficeArt,
    recolours: &[(char, crate::sprite::Pixel)],
    scale: RenderScale,
    buf: &mut RgbBuffer,
) {
    let f = crate::pack::densest_frame(pack, prop.sprite, prop.frame, scale);
    let f_themed = f.recolorable.recolored(recolours);
    blit_frame_scaled(
        &prop.flip.turn(f_themed),
        prop.at.0,
        prop.at.1,
        f.blit_at,
        buf,
    );
}

fn paint_door(at: Point, frame: usize, pack: &OfficeArt, scale: RenderScale, buf: &mut RgbBuffer) {
    let art = crate::pack::densest_frame(pack, PackPiece::Door, frame, scale);
    blit_frame_scaled(
        art.frame,
        scale.to_buffer(at.x),
        scale.to_buffer(at.y),
        art.blit_at,
        buf,
    );
}

/// The neon sign over `at`. At base density its border is the tube, as the
/// classic draws it; denser, the tube is a one-pixel lit core between two
/// pixels of its hue.
fn paint_neon(
    at: Bounds,
    [tube, hue, interior]: [crate::sprite::Rgb; 3],
    pen: Pen,
    buf: &mut RgbBuffer,
) {
    let (x, y, w, h) = (
        pen.art(at.x).0,
        pen.art(at.y).0,
        pen.art(at.width).0,
        pen.art(at.height).0,
    );
    let rect = |x: u16, y: u16, w: u16, h: u16| ArtRect {
        x: ArtPx(x),
        y: ArtPx(y),
        w: ArtPx(w),
        h: ArtPx(h),
    };
    pen.fill(buf, rect(x, y, w, h), interior);
    let border = pen.art(crate::layout::NEON_PANEL_BORDER).0;
    let ring = |buf: &mut RgbBuffer, i: u16, c| {
        let (x, y, w, h) = (x + i, y + i, w - 2 * i, h - 2 * i);
        for edge in [
            rect(x, y, w, 1),
            rect(x, y + h - 1, w, 1),
            rect(x, y, 1, h),
            rect(x + w - 1, y, 1, h),
        ] {
            pen.fill(buf, edge, c);
        }
    };
    if border == 1 {
        ring(buf, 0, tube);
        return;
    }
    let core = border / 2;
    ring(buf, core - 1, hue);
    ring(buf, core, tube);
    if core + 1 < border {
        ring(buf, core + 1, hue);
    }
}

/// The wall clock's dial from its top-left `at`, its hands at `reading` on the
/// art grid: the classic's octant hands at base density, a line from the
/// centre pin denser.
fn paint_clock(
    at: Point,
    reading: crate::sky::ClockReading,
    hand: crate::sprite::Rgb,
    pack: &OfficeArt,
    recolours: &[(char, crate::sprite::Pixel)],
    scale: RenderScale,
    buf: &mut RgbBuffer,
) {
    let (clock, density, blit_at) = pack.clock().at(scale);
    let dial = crate::pack::DenseFrame::of(
        pack.piece(PackPiece::WallClock),
        &clock.sprite,
        0,
        (density, blit_at),
    );
    let themed = dial.recolorable.recolored(recolours);
    let (bx, by) = (scale.to_buffer(at.x), scale.to_buffer(at.y));
    blit_frame_scaled(&themed, bx, by, dial.blit_at, buf);
    let pen = Pen::for_pack(scale, pack);
    let (ax, ay) = (pen.art(at.x).0, pen.art(at.y).0);
    let dot = |buf: &mut RgbBuffer, x: i32, y: i32| {
        if let (Ok(x), Ok(y)) = (u16::try_from(x), u16::try_from(y)) {
            pen.fill(
                buf,
                ArtRect {
                    x: ArtPx(x),
                    y: ArtPx(y),
                    w: ArtPx(1),
                    h: ArtPx(1),
                },
                hand,
            );
        }
    };
    let (hour, minute) = reading.turns();
    let d = pen.density().get();
    let side = pen.art(crate::layout::CLOCK.w).0;
    if d == 1 {
        // The classic's [`CLOCK`](crate::layout::CLOCK) face: the centre pin, the
        // hour a step out, the minute two, but one on a diagonal, where two
        // would cut the rim.
        let (cx, cy) = (i32::from(ax + side / 2), i32::from(ay + side / 2));
        dot(buf, cx, cy);
        let (hx, hy) = crate::sky::octant_offset(hour);
        dot(buf, cx + hx, cy + hy);
        let (mx, my) = crate::sky::octant_offset(minute);
        let reach = if mx != 0 && my != 0 { 1 } else { 2 };
        for step in 1..=reach {
            dot(buf, cx + mx * step, cy + my * step);
        }
        return;
    }
    let centre = (
        f32::from(ax) + f32::from(side) / 2.0,
        f32::from(ay) + f32::from(side) / 2.0,
    );
    let face = clock.face_reach * (f32::from(d) / f32::from(density.get()));
    for (turn, share) in [
        (hour, CLOCK_HOUR_HAND_SHARE),
        (minute, CLOCK_MINUTE_HAND_SHARE),
    ] {
        let (sin, cos) = (turn * std::f32::consts::TAU).sin_cos();
        for step in 1..=(face * share) as i32 {
            let t = step as f32;
            dot(
                buf,
                (centre.0 + t * sin).floor() as i32,
                (centre.1 - t * cos).floor() as i32,
            );
        }
    }
}

/// How far across the face each hand reaches.
const CLOCK_HOUR_HAND_SHARE: f32 = 0.5;
const CLOCK_MINUTE_HAND_SHARE: f32 = 0.85;

/// The classic's corridor runner on the art grid, its lines one art pixel wide.
fn paint_runner(
    b: Bounds,
    [runner_base, runner_stripe, runner_edge]: [crate::sprite::Rgb; 3],
    pen: Pen,
    buf: &mut RgbBuffer,
) {
    let (x0, y0, w, h) = (
        pen.art(b.x).0,
        pen.art(b.y).0,
        pen.art(b.width).0,
        pen.art(b.height).0,
    );
    let pitch = i32::from(pen.density().get()) * crate::layout::roster::RUNNER_LATTICE_STRIDE;
    let px = |buf: &mut RgbBuffer, x: u16, y: u16, c| {
        pen.fill(
            buf,
            ArtRect {
                x: ArtPx(x),
                y: ArtPx(y),
                w: ArtPx(1),
                h: ArtPx(1),
            },
            c,
        );
    };
    pen.fill(
        buf,
        ArtRect {
            x: ArtPx(x0),
            y: ArtPx(y0),
            w: ArtPx(w),
            h: ArtPx(h),
        },
        runner_base,
    );
    let (art_xs, art_ys) = pen.writable_art(buf);
    let dxs = art_xs.start.saturating_sub(x0).min(w)..art_xs.end.saturating_sub(x0).min(w);
    let dys = art_ys.start.saturating_sub(y0).max(1)
        ..art_ys.end.saturating_sub(y0).min(h.saturating_sub(1));
    for dy in dys {
        for dx in dxs.clone() {
            let (i, j) = (i32::from(dx), i32::from(dy));
            if (i + j) % pitch == 0 || (i - j).rem_euclid(pitch) == 0 {
                px(buf, x0 + dx, y0 + dy, runner_stripe);
            }
        }
    }
    for dx in 0..w {
        px(buf, x0 + dx, y0, runner_edge);
        px(buf, x0 + dx, y0 + h - 1, runner_edge);
    }
}

/// The buffer top-left of a `logical`-sized piece centred on `at`, for
/// `blit_frame_scaled`, which takes a top-left: the centring is undone in
/// logical space before converting, so the piece lands on the layout's grid.
fn centred_top_left(
    at: crate::layout::Point,
    logical: (u16, u16),
    scale: RenderScale,
) -> (u16, u16) {
    let crate::layout::Point { x, y } =
        crate::layout::anchored_top_left(crate::layout::Pivot::Center, at, logical.0, logical.1);
    (scale.to_buffer(x), scale.to_buffer(y))
}

/// Blit rows `rows` of a centred prop ([`PieceKind::PropBand`]).
fn paint_prop_band(
    at: crate::layout::Point,
    sprite: PackPiece,
    rows: (u16, u16),
    pack: &OfficeArt,
    scale: RenderScale,
    buf: &mut RgbBuffer,
) {
    let dense = crate::pack::densest_frame(pack, sprite, 0, scale);
    let (w, h) = dense.logical;
    let tl = crate::layout::anchored_top_left(crate::layout::Pivot::Center, at, w, h);
    let (r0, r1) = (rows.0.min(h), rows.1.min(h));
    let d = dense.density.get();
    let fw = usize::from(dense.frame.width());
    let px = dense.frame.as_slice();
    let band = crate::sprite::Frame::from_pixels(
        dense.frame.width(),
        (r1 - r0).saturating_mul(d),
        px[usize::from(r0) * usize::from(d) * fw..usize::from(r1) * usize::from(d) * fw].to_vec(),
    );
    blit_frame_scaled(
        &band,
        scale.to_buffer(tl.x),
        scale.to_buffer(tl.y + r0),
        dense.blit_at,
        buf,
    );
}

/// A task chair from the pack's art.
fn paint_chair(
    at: crate::layout::Point,
    pack: &OfficeArt,
    scale: RenderScale,
    buf: &mut RgbBuffer,
) {
    let art = crate::pack::densest_frame(pack, PackPiece::DeskChair, 0, scale);
    blit_frame_scaled(
        art.frame,
        scale.to_buffer(at.x),
        scale.to_buffer(at.y),
        art.blit_at,
        buf,
    );
}

#[cfg(test)]
mod tests;
