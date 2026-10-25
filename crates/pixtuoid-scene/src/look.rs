//! How a floor is drawn: a [`Look`], the per-floor [`Raster`] that keeps each
//! look's state across frames, and [`render`], the one frame entry every
//! painter calls.

use std::collections::HashSet;
use std::sync::Arc;

use crate::pack::OfficeArt;
use pixtuoid_core::sprite::{Rgb, RgbBuffer};
use pixtuoid_core::state::DaemonState;

/// Where a [`Rendered`] frame may differ from the last: the frame entry's
/// word for it, which the model reads without naming a rasterizer.
#[doc(hidden)]
pub use crate::cutaway::canvas::Dirty;
use crate::cutaway::canvas::{CanvasFrame, CutawayCanvas};
use crate::display::Hovers;
use crate::floor::{FloorInputs, OfficeStores, PerFloor, step_floor};
use crate::footer::FooterFloor;
use crate::layout::{SceneLayout, Size};
use crate::pixel_painter::{ClassicCaches, Drawn, PaintCtx, paint_frame};
use crate::render_scale::RenderScale;
use crate::theme::Theme;

/// How a floor is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// The half-block office: one buffer pixel per layout unit.
    Classic,
    /// The cutaway, `scale` buffer pixels per layout unit.
    Cutaway {
        /// Buffer pixels per layout unit.
        scale: RenderScale,
        /// Who sets its world text.
        text: WorldText,
    },
}

/// Who sets a cutaway's world text: the badges, bubbles, wall board and floor
/// indicator. The classic's is always the host's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldText {
    /// The image, in the scene's pixel font.
    Baked,
    /// The host, in its own font over the image ([`FloorPicture::text`]).
    Host,
}

/// What only the office knows about the floor a painter draws: its wall
/// board's gateway chip and breadcrumb.
#[derive(Debug, Clone, Copy, Default)]
pub struct Place {
    /// The office's [`office_gateway`](crate::tally::office_gateway).
    pub gateway: Option<DaemonState>,
    /// Where the floor sits, `None` in a one-floor office.
    pub floor: Option<FooterFloor>,
}

/// One floor's frame, beyond its stores.
#[derive(Debug, Clone, Copy)]
pub struct RenderInputs<'a> {
    /// The floor it shows.
    pub world: FloorInputs<'a>,
    /// The active color theme.
    pub theme: &'static Theme,
    /// The office's logical extent; a look's scale maps it to buffer pixels.
    pub size: Size,
    /// See [`Place`].
    pub place: Place,
    /// The classic's walkable/approach/route debug layer (the `w` toggle).
    pub debug_walkable: bool,
    /// The brightness the host's modal leaves the frame.
    pub dim: Dim,
}

/// A brightness multiplier, from black to unchanged, each pixel takes: a
/// host modal's backdrop over the office, a card's drop shadow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dim(f32);

impl Default for Dim {
    fn default() -> Self {
        Self::NONE
    }
}

impl Dim {
    /// Unchanged.
    pub const NONE: Self = Self(1.0);

    /// At `brightness` of its own, clamped to black..=unchanged; a NaN leaves
    /// it unchanged.
    pub fn from_brightness(brightness: f32) -> Self {
        if brightness.is_nan() {
            Self::NONE
        } else {
            Self(brightness.clamp(0.0, 1.0))
        }
    }

    /// The brightness it leaves.
    pub fn brightness(self) -> f32 {
        self.0
    }

    /// Darken every pixel of `buf` a frame may write: within its clip, where
    /// it has one.
    pub(crate) fn apply(self, buf: &mut RgbBuffer) {
        if self == Self::NONE {
            return;
        }
        for (_, _, row) in buf.writable_rows_mut() {
            for px in row {
                *px = self.of(*px);
            }
        }
    }

    /// `rgb` under this dim.
    pub fn of(self, rgb: Rgb) -> Rgb {
        let channel = |c: u8| (f32::from(c) * self.0) as u8;
        Rgb {
            r: channel(rgb.r),
            g: channel(rgb.g),
            b: channel(rgb.b),
        }
    }
}

/// One frame from [`render`].
#[derive(Debug)]
pub struct Rendered {
    /// Where it may differ from the last frame this raster showed.
    pub(crate) dirty: Dirty,
    /// What it answers a pointer with, the layout it was drawn on among it.
    pub(crate) hits: Hits,
    /// Waypoints with an occupant: the appliance audio cues' feed.
    pub(crate) occupied_waypoints: HashSet<usize>,
    /// What of the frame flashes.
    pub(crate) flash: crate::screen::FlashPhase,
}

impl Rendered {
    /// This frame's picture as `raster` drew it, a one-floor office's: for a
    /// painter driving [`render`] itself.
    pub fn picture<'a>(&'a self, raster: &'a Raster) -> Option<FloorPicture<'a>> {
        raster.floor_picture(0, &self.hits, self.flash, &self.dirty)
    }

    /// The layout the sim stepped on and the frame was drawn on.
    pub fn layout(&self) -> &Arc<SceneLayout> {
        self.hits.layout()
    }
}

/// The frame entry's `tracing` span names, for a profiler's subscriber.
#[doc(hidden)]
pub mod spans {
    /// Stepping the floor's model.
    pub const COMPOSE: &str = "frame.compose";
    /// Drawing the stepped floor in its look.
    pub const RASTERIZE: &str = "frame.rasterize";
}

/// What the last frame of a [`Raster`] was drawn under, for a painter's
/// jank report.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameNote {
    /// Why the cutaway painted it whole, when it did; `None` for the classic,
    /// which paints every frame whole.
    pub repaint: Option<crate::cutaway::canvas::Repaint>,
    /// The weather it shows: the slot's, the next slot's, and the next one's
    /// share of the clouds.
    pub weather: (crate::sky::Weather, crate::sky::Weather, f32),
    /// Whether a strike lit it.
    pub strike: bool,
}

/// The office's raster state, shared by every floor and both looks: the
/// cutaway's art, and the outside's clouds and last views.
#[derive(Debug, Default)]
pub struct OfficeRaster {
    pub(crate) cutaway: crate::cutaway::paint::CutawayCache,
    pub(crate) outside: crate::outside::OutsideCache,
    /// The pack and the theme every cache holds art of, by identity: the
    /// `Arc` keeps the pack's address its own while it is held.
    serving: Option<(Arc<OfficeArt>, &'static Theme)>,
}

impl OfficeRaster {
    /// Have the next frame draw every cloud mass the coming second needs,
    /// not a few a frame: the boot frame, which no frame before it drew
    /// ahead for.
    #[doc(hidden)]
    pub fn warm(&mut self) {
        self.outside.clouds.warm();
    }

    /// Drop the art of agents no longer in `scene`.
    pub(crate) fn evict_missing(&mut self, scene: &pixtuoid_core::SceneState) {
        self.cutaway.evict_missing(scene);
    }

    /// Draw `pack` in `theme` from now on: every cache holds one pack's art
    /// in one theme's colours, so a frame of another empties them first.
    fn serve(&mut self, pack: &Arc<OfficeArt>, theme: &'static Theme) {
        match &self.serving {
            Some((p, t)) if Arc::ptr_eq(p, pack) && std::ptr::eq(*t, theme) => {}
            Some(_) => {
                *self = Self {
                    serving: Some((Arc::clone(pack), theme)),
                    ..Self::default()
                }
            }
            // A warm before the first frame is for this one.
            None => self.serving = Some((Arc::clone(pack), theme)),
        }
    }
}

/// One floor's raster state, for every look it has been drawn in: each look's
/// is built the first time it draws and kept, so a look that switches back and
/// forth never rebuilds it.
#[derive(Debug)]
pub struct Raster {
    /// The pack this floor is stepped and drawn with.
    pack: Arc<OfficeArt>,
    classic: Option<Classic>,
    cutaway: Option<CutawayCanvas>,
    /// The look of the last frame drawn; the first frame in another repaints whole.
    shown: Option<Look>,
    note: Option<FrameNote>,
}

#[derive(Debug)]
struct Classic {
    buf: RgbBuffer,
    caches: ClassicCaches,
    /// The last frame's badges and bubbles.
    badges: Vec<crate::display::Badge>,
    bubbles: Vec<crate::display::TextRun>,
    /// What the last frame answers a pointer with.
    hovers: Arc<Hovers>,
    /// The last frame's wall board lines and floor indicator.
    signs: Vec<crate::display::TextRun>,
    /// The dim the last frame was drawn under.
    dim: Dim,
}

impl Classic {
    fn host_text(&self) -> crate::display::HostText<'_> {
        crate::display::HostText {
            badges: &self.badges,
            bubbles: &self.bubbles,
            signs: &self.signs,
            dim: self.dim,
        }
    }

    /// Keep what `drawn` drew besides its pixels.
    fn drew(&mut self, drawn: Drawn) {
        let Drawn {
            badges,
            bubbles,
            hovers,
        } = drawn;
        self.badges = badges;
        self.bubbles = bubbles;
        self.hovers = Arc::new(hovers);
    }
}

/// What a frame shows: one floor, or the two a floor slide composes.
#[derive(Debug, Clone)]
pub enum Picture<'a> {
    /// One floor, laid out.
    Floor(FloorPicture<'a>),
    /// A slide: no one layout, so nothing a pointer finds and no host text.
    Slide(SlidePicture<'a>),
}

/// One floor's frame.
#[derive(Debug, Clone)]
pub struct FloorPicture<'a> {
    /// Which floor of the office it shows.
    pub floor: usize,
    /// The frame.
    pub pixels: &'a RgbBuffer,
    /// Where it may differ from the last frame its session RENDERED; what a
    /// screen holds is its painter's to track.
    pub(crate) dirty: &'a Dirty,
    /// What of it flashes.
    pub flashes: crate::screen::Flashes,
    /// The world text its host sets over it; `None` where the look baked it.
    pub text: Option<crate::display::HostText<'a>>,
    /// What it answers a pointer with.
    pub hits: &'a Hits,
}

/// A floor slide's frame: its two floors composed.
#[derive(Debug, Clone, Copy)]
pub struct SlidePicture<'a> {
    /// The two floors composed.
    pub(crate) pixels: &'a RgbBuffer,
    /// What of both flashes.
    pub(crate) flashes: crate::screen::Flashes,
}

impl<'a> Picture<'a> {
    /// Its pixels.
    pub fn pixels(&self) -> &'a RgbBuffer {
        match self {
            Self::Floor(floor) => floor.pixels,
            Self::Slide(slide) => slide.pixels,
        }
    }

    /// What of it flashes.
    pub fn flashes(&self) -> crate::screen::Flashes {
        match self {
            Self::Floor(floor) => floor.flashes,
            Self::Slide(slide) => slide.flashes,
        }
    }

    /// Where it may differ from the last frame its session rendered: a
    /// slide's anywhere.
    pub fn dirty(&self) -> &'a Dirty {
        match self {
            Self::Floor(floor) => floor.dirty,
            Self::Slide(_) => &Dirty::All,
        }
    }

    /// What it answers a pointer with; `None` for a slide.
    pub fn hits(&self) -> Option<&'a Hits> {
        match self {
            Self::Floor(floor) => Some(floor.hits),
            Self::Slide(_) => None,
        }
    }
}

/// What a floor's frame answers a pointer with, kept for as long as a screen
/// shows it.
#[derive(Debug, Clone)]
pub struct Hits {
    hovers: Arc<Hovers>,
    layout: Arc<SceneLayout>,
    /// The star the look drew; a host's text places its own.
    star: Option<crate::layout::Bounds>,
}

impl Hits {
    pub(crate) fn new(
        hovers: Arc<Hovers>,
        layout: Arc<SceneLayout>,
        star: Option<crate::layout::Bounds>,
    ) -> Self {
        Self {
            hovers,
            layout,
            star,
        }
    }

    /// What the frame shows a pointer over `area`, in layout units, its
    /// board's star at `host_star` where its host set the text.
    pub fn at(
        &self,
        area: crate::layout::Bounds,
        host_star: Option<crate::layout::Bounds>,
    ) -> Option<crate::hit::SceneHit<'_>> {
        crate::hit::scene_hit(&self.hovers, self.star.or(host_star), &self.layout, area)
    }

    /// The layout the frame was drawn on.
    pub fn layout(&self) -> &Arc<SceneLayout> {
        &self.layout
    }

    /// Its hovers, back to front.
    pub fn hovers(&self) -> &Hovers {
        &self.hovers
    }
}

impl Raster {
    /// A floor drawn with `pack`, in no look yet.
    pub(crate) fn new(pack: Arc<OfficeArt>) -> Self {
        Self {
            pack,
            classic: None,
            cutaway: None,
            shown: None,
            note: None,
        }
    }

    fn classic(&mut self) -> &mut Classic {
        self.classic.get_or_insert_with(|| Classic {
            buf: RgbBuffer::filled(0, 0, pixtuoid_core::sprite::Rgb { r: 0, g: 0, b: 0 }),
            caches: ClassicCaches::new(),
            badges: Vec::new(),
            bubbles: Vec::new(),
            hovers: Arc::default(),
            signs: Vec::new(),
            dim: Dim::NONE,
        })
    }

    /// The last frame, as floor `floor` of its office, answering a pointer
    /// with `hits`, flashing `flash`, `dirty` where it may differ from the
    /// one before it; `None` before the first.
    pub(crate) fn floor_picture<'a>(
        &'a self,
        floor: usize,
        hits: &'a Hits,
        flash: crate::screen::FlashPhase,
        dirty: &'a Dirty,
    ) -> Option<FloorPicture<'a>> {
        Some(FloorPicture {
            floor,
            pixels: self.pixels()?,
            dirty,
            flashes: crate::screen::Flashes::of_floor(flash),
            text: self.host_text(),
            hits,
        })
    }

    /// The world text the last frame left to its host, in either look;
    /// `None` before the first and for a cutaway that baked its own.
    pub(crate) fn host_text(&self) -> Option<crate::display::HostText<'_>> {
        match self.shown? {
            Look::Classic => self.classic.as_ref().map(Classic::host_text),
            Look::Cutaway { .. } => self.cutaway.as_ref()?.host_text(),
        }
    }

    /// What the last frame drawn answers a pointer with, in either look;
    /// `None` before the first.
    pub(crate) fn hovers(&self) -> Option<&Arc<Hovers>> {
        match self.shown? {
            Look::Classic => self.classic.as_ref().map(|c| &c.hovers),
            Look::Cutaway { .. } => self.cutaway.as_ref()?.hovers(),
        }
    }

    /// The cells of the star the last frame baked, where a pointer opens the
    /// repo; `None` before the first, when it drew none, or where its host
    /// sets it ([`Self::host_text`]), which then places it.
    pub(crate) fn star(&self) -> Option<crate::layout::Bounds> {
        match self.shown? {
            Look::Classic => None,
            Look::Cutaway { .. } => self.cutaway.as_ref()?.star(),
        }
    }

    /// Drop the classic's recolored sprites of agents no longer in `scene`.
    pub(crate) fn evict_missing(&mut self, scene: &pixtuoid_core::SceneState) {
        if let Some(classic) = &mut self.classic {
            classic.caches.sprites.evict_missing(scene);
        }
    }

    /// Paint the next cutaway frame whole: the frame-pacing bench's worst case.
    #[doc(hidden)]
    pub fn forget_shown(&mut self) {
        if let Some(cutaway) = &mut self.cutaway {
            cutaway.forget();
        }
    }

    /// The look of the last frame drawn, `None` before the first.
    pub fn shown(&self) -> Option<Look> {
        self.shown
    }

    /// What the last frame drawn was drawn under, `None` before the first.
    pub(crate) fn note(&self) -> Option<FrameNote> {
        self.note
    }

    pub(crate) fn pack(&self) -> &OfficeArt {
        &self.pack
    }

    /// The pixels of the last frame drawn, `None` before the first.
    pub fn pixels(&self) -> Option<&RgbBuffer> {
        match self.shown? {
            Look::Classic => self.classic.as_ref().map(|c| &c.buf),
            Look::Cutaway { .. } => self.cutaway.as_ref().map(CutawayCanvas::buf),
        }
    }
}

/// Step `floor` one frame and draw it in `look` on its raster: the one frame
/// entry every painter calls. `None` when `inputs.size` can't lay out.
pub fn render(
    floor: &mut PerFloor,
    office: OfficeStores<'_>,
    look: Look,
    inputs: RenderInputs<'_>,
) -> Option<Rendered> {
    let RenderInputs {
        world,
        theme,
        size,
        place,
        debug_walkable,
        dim,
    } = inputs;
    office.raster.serve(&floor.raster.pack, theme);
    let stepped = tracing::trace_span!(spans::COMPOSE)
        .in_scope(|| step_floor(floor, office.coffee, office.chitchat, world, size));
    let PerFloor { ctx, raster } = floor;
    let pack = Arc::clone(&raster.pack);
    let Some(stepped) = stepped else {
        if look == Look::Classic {
            let classic = raster.classic();
            classic
                .buf
                .resize_fill(size.w, size.h, theme.surface.bg_fallback);
            classic.drew(Drawn::default());
            classic.signs.clear();
            raster.shown = Some(look);
        }
        return None;
    };
    let switched = raster
        .shown
        .replace(look)
        .is_none_or(|was| std::mem::discriminant(&was) != std::mem::discriminant(&look));
    let _rasterize = tracing::trace_span!(spans::RASTERIZE).entered();
    let board = crate::neon_sign::wall_board(
        world.scene,
        place.gateway,
        place.floor,
        world.floor.motion,
        world.now,
    );
    let sky = crate::sky::Sky::at(world.floor.motion.timing(world.now), world.floor.weather);
    raster.note = Some(FrameNote {
        repaint: None,
        weather: {
            let mix = sky.weather();
            let [from, to] = mix.ends();
            (from, to, mix.incoming(crate::sky::Element::Cloud))
        },
        strike: sky.strike().is_some(),
    });
    let (dirty, flash) = match look {
        Look::Classic => {
            let classic = raster.classic();
            classic
                .buf
                .resize_fill(size.w, size.h, theme.surface.bg_fallback);
            let mut paint = PaintCtx::classic(
                (world, &pack),
                &stepped.layout,
                theme,
                (&mut classic.caches, &mut office.raster.outside),
                &mut classic.buf,
                &ctx.walks,
                debug_walkable,
            );
            let flash = paint.flash(&stepped.frame);
            let drawn = paint_frame(&mut paint, &stepped.frame);
            classic.drew(drawn);
            dim.apply(&mut classic.buf);
            classic.dim = dim;
            classic.signs =
                crate::display::TextRun::signs(&board, stepped.layout.door, world.floor, theme);
            (Dirty::All, flash)
        }
        Look::Cutaway { scale, text } => {
            let canvas = raster
                .cutaway
                .get_or_insert_with(|| CutawayCanvas::new(Arc::clone(&pack)));
            let CanvasFrame {
                dirty,
                flash,
                repaint,
            } = canvas.frame(
                &stepped,
                theme,
                scale,
                crate::display::Showing {
                    floor: world.floor,
                    now: world.now,
                    board: &board,
                    dim,
                },
                text,
                (&mut office.raster.cutaway, &mut office.raster.outside),
            );
            if let Some(note) = &mut raster.note {
                note.repaint = repaint.or(switched.then(|| crate::cutaway::canvas::Repaint {
                    first: true,
                    ..Default::default()
                }));
            }
            (if switched { Dirty::All } else { dirty }, flash)
        }
    };
    Some(Rendered {
        dirty,
        hits: Hits::new(Arc::clone(raster.hovers()?), stepped.layout, raster.star()),
        occupied_waypoints: stepped.frame.occupied_waypoints,
        flash,
    })
}

#[cfg(test)]
mod tests;
