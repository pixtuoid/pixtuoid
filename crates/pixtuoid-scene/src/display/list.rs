//! The display list's types: what one frame shows, piece by piece.

use crate::pack::OfficeArt;

use super::Span;
use crate::atmosphere::Carpet;
use crate::dither::Dithered;
use crate::layout::Bounds;
use crate::outside::WindowView;
use crate::render_scale::RenderScale;
use crate::theme::Theme;

/// How many tones a standby screen's glass steps through as the room darkens:
/// a stepped glow, where a blend would put a colour of its own on every level.
const STANDBY_STOPS: u8 = 2;
/// Ramp levels a standby glass sits under the theme's idle tint for each stop it
/// is short of one past the last, so even the brightest standby stays a step
/// under the tint: the tint itself on the glass reads as a screen switched on.
const STANDBY_LEVEL_PER_STOP: i8 = -2;

/// What a desk's screen shows. A screen is its own light: whatever the room's
/// lights do, they leave its glass alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Screen {
    /// Dark glass.
    Off,
    /// The standby glow of an idle screen at night, its glass in this colour.
    Standby(crate::sprite::Rgb),
    /// Lit by its occupant's tool, in this glow, its scanline on glass column
    /// `scan` ([`crate::sim::scanline_col`]).
    Lit { glow: crate::sprite::Rgb, scan: u16 },
}

impl Screen {
    /// A desk's screen: lit by `glow` with its scanline on `scan`, else
    /// standing by at `idle` ([`crate::lighting::screen_idle`]) in `theme`'s
    /// idle tint, else dark.
    pub(super) fn of(
        glow: Option<crate::sprite::Rgb>,
        scan: u16,
        idle: f32,
        theme: &Theme,
    ) -> Self {
        if let Some(glow) = glow {
            return Self::Lit { glow, scan };
        }
        let share = idle / crate::lighting::SCREEN_IDLE_MAX;
        let stops = (share.clamp(0.0, 1.0) * f32::from(STANDBY_STOPS)).round() as u8;
        if stops == 0 {
            return Self::Off;
        }
        let lacking = (STANDBY_STOPS + 1 - stops) as i8;
        Self::Standby(
            theme
                .effects
                .monitor_idle
                .ramp(lacking * STANDBY_LEVEL_PER_STOP),
        )
    }
}

/// One frame's pieces — the windows, the decor hung on the wall and
/// everything standing on the ground — built and ordered but not painted.
///
/// ONE ordered list, so a character and the desk it sits at resolve against each
/// other by depth: that order IS the occlusion, and there is no second
/// occlusion pass.
pub(crate) struct DisplayList<'a> {
    pub(super) pieces: Vec<Piece>,
    /// The room's own lights, which light every piece at once.
    pub(super) lights: Vec<LightPiece>,
    /// How dark the room is: every non-emissive pixel is painted under it, so a
    /// change repaints the whole frame.
    pub(super) ambient: crate::display::light::Ambient,
    /// The carpet the backdrop lays: a change repaints the whole frame too.
    pub(super) carpet: Dithered<Carpet>,
    /// How far lightning lifts the room: a change repaints the whole frame.
    pub(super) flash: crate::display::light::Flash,
    /// The brightness the host's modal leaves the frame, painted last: a
    /// change repaints the whole frame.
    pub(super) dim: crate::look::Dim,
    /// What of the frame flashes, for a painter's hold.
    pub(super) flash_phase: crate::screen::FlashPhase,
    /// The figures' hovers, in `pieces`' order.
    pub(super) hovers: super::Hovers,
    /// What lies under every piece.
    pub(super) backdrop: super::Backdrop,
    pub(super) recolours: Recolours,
    /// Its world text, for a host that sets it over the frame itself.
    pub(super) host_text: super::text::HostRuns,
    // What it was built with, so painting it cannot use anything else: a
    // figure's key names its density, which only the build's scale picks.
    pub(super) pack: &'a OfficeArt,
    pub(super) scale: RenderScale,
}

/// The pack keys art takes from the theme, resolved once a frame, so painting
/// reads no theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recolours {
    /// The appliances' and fixtures' keys.
    pub(crate) art: Vec<(char, crate::sprite::Pixel)>,
    /// The desk props' cup and paper.
    pub(crate) desk_props: [(char, crate::sprite::Pixel); 4],
}

impl Recolours {
    /// `theme`'s colours for the keys art takes from it.
    pub(crate) fn of(theme: &Theme) -> Self {
        Self {
            art: crate::pack::appliance_overrides(&theme.appliance)
                .into_iter()
                .chain(crate::pack::fixture_overrides(theme))
                .collect(),
            desk_props: crate::pack::desk_prop_overrides(theme),
        }
    }
}

/// One entry of a [`DisplayList`].
pub(crate) struct Piece {
    pub(crate) span: Span,
    pub(crate) kind: PieceKind,
    /// The shadow it casts on the ground
    /// ([`ground_shadow`](crate::display::compose::ground_shadow)). Every
    /// shadow is painted before any piece, so it lies under them all; it may
    /// reach past `span`, but never past [`reach`](Self::reach).
    pub(crate) shadow: Option<crate::ground::Contact>,
    /// Two pieces with one span and one fingerprint paint the same pixels over
    /// the same pixels under them, so a caller can keep a piece whose pair did
    /// not change without painting it. Compare it only within one layout,
    /// theme, pack, scale and ambient: it does not capture a change to those.
    pub(crate) fingerprint: u64,
}

impl Piece {
    /// Its hover: a character's body, a creature's span; no other piece is
    /// one.
    pub(crate) fn hover(&self) -> Option<super::Hover> {
        let (at, target) = match &self.kind {
            PieceKind::Character { figure, body, .. } => (
                *body,
                crate::hit::HoverTarget::Agent(figure.key.frame.agent_id),
            ),
            PieceKind::Creature { who, .. } => (self.span, who.clone()?),
            _ => return None,
        };
        Some(super::Hover {
            at: at.bounds(),
            target,
        })
    }
}

/// One of the room's lights: what it lifts, over which cells. A change repaints
/// its span from every light that meets it, which alone light a rect as the
/// whole frame does.
#[derive(Debug, Clone)]
pub(crate) struct LightPiece {
    pub(crate) span: Span,
    /// Shared with the [`LightCache`](crate::display::compose::LightCache)
    /// that keeps it for the next frame.
    pub(crate) view: std::sync::Arc<crate::display::light::LightView>,
    pub(crate) fingerprint: u64,
}

impl Piece {
    /// Every logical cell painting it touches: its span, and the ground its
    /// shadow falls on. A repaint of a damaged rect is complete only over the
    /// pieces whose reach meets it.
    pub(crate) fn reach(&self) -> Span {
        let Some(((x0, y0), (x1, y1))) = self.shadow.map(|c| c.bounds()) else {
            return self.span;
        };
        Span {
            x0: self.span.x0.min(x0),
            x1: self.span.x1.max(x1.saturating_sub(1)),
            y0: self.span.y0.min(y0),
            y1: self.span.y1.max(y1.saturating_sub(1)),
            ..self.span
        }
    }
}

impl<'a> DisplayList<'a> {
    /// The pieces, back to front.
    pub(crate) fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    /// The pieces, for a rasterizer test to add or drop one.
    #[cfg(test)]
    pub(crate) fn pieces_mut(&mut self) -> &mut Vec<Piece> {
        &mut self.pieces
    }

    /// The room's lights, in no order that matters: a pixel's light is the
    /// brightest's, ties to the lowest rank.
    pub(crate) fn lights(&self) -> &[LightPiece] {
        &self.lights
    }

    /// The room's lights, for a rasterizer test to switch one off.
    #[cfg(test)]
    pub(crate) fn lights_mut(&mut self) -> &mut Vec<LightPiece> {
        &mut self.lights
    }

    pub(crate) fn ambient(&self) -> crate::display::light::Ambient {
        self.ambient
    }

    /// How dark the room is, for a rasterizer test to change the hour.
    #[cfg(test)]
    pub(crate) fn ambient_mut(&mut self) -> &mut crate::display::light::Ambient {
        &mut self.ambient
    }

    pub(crate) fn carpet(&self) -> Dithered<Carpet> {
        self.carpet
    }

    pub(crate) fn flash(&self) -> crate::display::light::Flash {
        self.flash
    }

    pub(crate) fn dim(&self) -> crate::look::Dim {
        self.dim
    }

    pub(crate) fn flash_phase(&self) -> crate::screen::FlashPhase {
        self.flash_phase
    }

    pub(crate) fn pack(&self) -> &'a OfficeArt {
        self.pack
    }

    pub(crate) fn backdrop(&self) -> &super::Backdrop {
        &self.backdrop
    }

    /// What lies under every piece, for a rasterizer test to bare the floor.
    #[cfg(test)]
    pub(crate) fn backdrop_mut(&mut self) -> &mut super::Backdrop {
        &mut self.backdrop
    }

    pub(crate) fn recolours(&self) -> &Recolours {
        &self.recolours
    }

    pub(crate) fn scale(&self) -> RenderScale {
        self.scale
    }

    pub(crate) fn hovers(&self) -> &super::Hovers {
        &self.hovers
    }

    /// Leave the world text to the host: no text piece paints, and none
    /// counts toward what changed. Returns the text.
    pub(crate) fn host_text(&mut self) -> super::text::HostRuns {
        self.pieces
            .retain(|p| !matches!(p.kind, PieceKind::Text { .. }));
        std::mem::take(&mut self.host_text)
    }

    /// Each drawn agent's badge, held to outlive the frame.
    pub(crate) fn badge_list(&self) -> &std::sync::Arc<[super::Badge]> {
        &self.host_text.badges
    }

    /// Each text run, in draw order.
    pub(crate) fn texts(&self) -> impl Iterator<Item = &super::TextRun> + '_ {
        self.pieces.iter().filter_map(|p| match &p.kind {
            PieceKind::Text { run } => Some(run),
            _ => None,
        })
    }

    /// Each agent's badge, in draw order.
    #[cfg(test)]
    pub(crate) fn badges(&self) -> impl Iterator<Item = &super::TextRun> + '_ {
        self.pieces.iter().filter_map(|p| match &p.kind {
            PieceKind::Text { run } if matches!(run.role, super::TextRole::Badge(_)) => Some(run),
            _ => None,
        })
    }
}

/// A hash of every input `paint_piece` and
/// [`ground_shadow`](crate::display::compose::ground_shadow) draw `kind`
/// from beyond the layout, theme, pack and scale. Each arm destructures every field,
/// so a field added to a kind fails to compile here until it is hashed or named
/// `_`.
pub(crate) fn fingerprint(kind: &PieceKind) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = fingerprint_hasher();
    std::mem::discriminant(kind).hash(&mut h);
    match *kind {
        PieceKind::WallSeg { piece, rows, trim } => (piece, rows, trim).hash(&mut h),
        PieceKind::Desk { at, desk, screen } | PieceKind::DeskFront { at, desk, screen } => {
            (at, desk, screen).hash(&mut h);
        }
        PieceKind::Chair { at } => at.hash(&mut h),
        PieceKind::DeskProp(prop) => prop.hash(&mut h),
        PieceKind::Creature {
            at,
            art,
            degraded,
            who: _,
        } => (at, art, degraded).hash(&mut h),
        PieceKind::Prop { at, art } | PieceKind::Animated { at, art } => (at, art).hash(&mut h),
        PieceKind::PropBand { at, sprite, rows } => (at, sprite, rows).hash(&mut h),
        PieceKind::Table { at } => at.hash(&mut h),
        PieceKind::Door { at, frame } => (at, frame).hash(&mut h),
        PieceKind::Neon {
            at,
            tube,
            hue,
            interior,
        } => (at, tube, hue, interior).hash(&mut h),
        PieceKind::Clock { at, reading, hand } => (at, reading, hand).hash(&mut h),
        // The body follows from `at` and `key`.
        PieceKind::Character {
            figure:
                Figure {
                    at,
                    shadow,
                    ref key,
                },
            chair,
            body: _,
        } => (at, shadow, key, chair).hash(&mut h),
        PieceKind::Text { ref run } => run.hash(&mut h),
        PieceKind::Window { ref view, frame } => (view, frame).hash(&mut h),
        PieceKind::Hung { at, sprite } => (at, sprite).hash(&mut h),
        PieceKind::Effect(riding) => riding.hash(&mut h),
    }
    h.finish()
}

/// The hasher every display fingerprint is taken with, seeded alike on every
/// call so one frame built twice hashes alike (`one_frame_builds_one_list`).
/// foldhash's quality hasher, not std's SipHash: a fingerprint only tells this
/// frame's piece from the last frame's in one process, so HashDoS resistance
/// buys it nothing (perf-book "Hashing"); and not foldhash's `fast`, since a
/// collision is a missed repaint.
pub(crate) fn fingerprint_hasher() -> impl std::hash::Hasher {
    use std::hash::BuildHasher;
    foldhash::quality::FixedState::with_seed(0).build_hasher()
}

/// How a piece takes the room's light, which the cutaway's emission pass
/// reads. The classic paints its lights over everything they fall on, the
/// glass included, so the two looks light a window alike
/// (`the_neon_halo_lifts_the_window_glass_it_falls_on`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Emits<'a> {
    /// A window. What its glass shows ([`WindowView::shows`]) is its own light,
    /// which the room's lights still lift, so a light that falls on the glass
    /// glows on it. Its joinery is lit.
    Pane(&'a WindowView),
    /// Its own light, as painted: a sign, or text that keeps the contrast its
    /// theme pins at every hour.
    Emissive,
    /// Each pixel as its art marks it: a screen that glows, a bulb.
    ByArt,
    /// Darkened with the room, lifted by its lights: most of it.
    Lit,
}

impl PieceKind {
    /// How it takes the room's light.
    pub(crate) fn emits(&self) -> Emits<'_> {
        match self {
            PieceKind::Window { view, .. } => Emits::Pane(view),
            PieceKind::Neon { .. } | PieceKind::Text { .. } => Emits::Emissive,
            PieceKind::Effect(r) if r.effect.kind == crate::effects::EffectKind::FlameCrown => {
                Emits::Emissive
            }
            PieceKind::Desk { .. }
            | PieceKind::Prop { .. }
            | PieceKind::Animated { .. }
            | PieceKind::Hung { .. }
            | PieceKind::Door { .. } => Emits::ByArt,
            PieceKind::WallSeg { .. }
            | PieceKind::Chair { .. }
            | PieceKind::DeskProp(_)
            | PieceKind::DeskFront { .. }
            | PieceKind::Creature { .. }
            | PieceKind::PropBand { .. }
            | PieceKind::Table { .. }
            | PieceKind::Character { .. }
            | PieceKind::Effect(_)
            | PieceKind::Clock { .. } => Emits::Lit,
        }
    }

    /// Whether it recolours what lies under it rather than painting colours of
    /// its own: a room wall's glass ([`PieceKind::WallSeg`]), not a window
    /// ([`PieceKind::Window`]). The oracle the span-bounds tests tell the two
    /// apart by.
    #[cfg(test)]
    pub(crate) fn reads_under(&self) -> bool {
        matches!(self, PieceKind::WallSeg { .. })
    }
}

/// What a [`Piece`] IS; its [`Span`] rides beside it.
///
/// The span is computed WHERE THE PIECE IS BUILT, from the same anchor and box
/// the piece's paint fn draws, plus the rows it stamps under that box — the box
/// depends on the PACK, so a sprite's height is not knowable from its layout
/// point, and geometry derived from anywhere but where the sprite lands can drift
/// from it. `every_piece_paints_only_inside_its_span` pins that every pixel a
/// paint fn writes lies inside its span.
#[derive(Debug)]
pub(crate) enum PieceKind {
    /// One window: its glass, then its joinery in `frame`.
    Window {
        /// Shared as [`LightPiece::view`] is.
        view: std::sync::Arc<WindowView>,
        frame: crate::sprite::Rgb,
    },
    /// Decor hung on the north band, blitted at its top-left `at`.
    Hung {
        at: crate::layout::Point,
        sprite: crate::pack::Piece,
    },
    /// One segment of a room's wall run.
    WallSeg {
        piece: crate::layout::WallPiece,
        /// The logical rows of `piece` this segment paints, top inclusive and
        /// bottom exclusive.
        rows: (u16, u16),
        trim: crate::glass::WallTrim,
    },
    Desk {
        at: crate::layout::Point,
        /// The facing's desk ([`Desk::facing`](crate::pack::Desk::facing)).
        desk: crate::pack::Desk,
        screen: Screen,
    },
    /// What of the desk at `at` stands nearer the viewer than its props
    /// ([`Desk::front`](crate::pack::Desk::front)), drawn over them as the
    /// desk is drawn.
    DeskFront {
        at: crate::layout::Point,
        /// The desk, which the front covers.
        desk: crate::pack::Desk,
        screen: Screen,
    },
    Chair {
        at: crate::layout::Point,
    },
    /// Art centred on `at` that holds still.
    Prop {
        at: crate::layout::Point,
        art: Art,
    },
    /// Art centred on `at`, on the frame it shows now.
    Animated {
        at: crate::layout::Point,
        art: Art,
    },
    /// Rows `rows` of a centred prop, top inclusive, bottom exclusive, in
    /// logical rows from the art's top: one band of a piece its sitter sits
    /// between (`push_sofa`).
    PropBand {
        at: crate::layout::Point,
        sprite: crate::pack::Piece,
        rows: (u16, u16),
    },
    Table {
        at: crate::layout::Point,
    },
    /// The elevator from its top-left `at`, as open as `frame` shows.
    Door {
        at: crate::layout::Point,
        frame: usize,
    },
    /// The neon sign over `at`, in this frame's colours.
    Neon {
        at: Bounds,
        tube: crate::sprite::Rgb,
        hue: crate::sprite::Rgb,
        interior: crate::sprite::Rgb,
    },
    /// The wall clock's dial from its top-left `at`, its hands at `reading`.
    Clock {
        at: crate::layout::Point,
        reading: crate::sky::ClockReading,
        /// The theme's hand colour.
        hand: crate::sprite::Rgb,
    },
    /// A prop the model stands on a desk (`push_desk_props`).
    DeskProp(StoodProp),
    /// The pet or a gateway mascot, its art centred on `at`; a degraded
    /// gateway's greyed (`push_creatures`).
    Creature {
        at: crate::layout::Point,
        art: Art,
        degraded: bool,
        /// `None`: it hovers as nothing ([`MascotPlacement::on_roster`](crate::sim::MascotPlacement::on_roster)).
        who: Option<crate::hit::HoverTarget>,
    },
    Character {
        figure: Figure,
        /// A back-turned sitter's chair, painted straight after them.
        chair: Option<crate::layout::Point>,
        /// The box their art lands in: the span without their chair. A standing
        /// figure's shadow falls under it.
        body: Span,
    },
    /// An effect riding on the figure pushed beside it.
    Effect(crate::display::effects::Riding),
    /// A line of text over everything it meets.
    Text {
        run: super::TextRun,
    },
}

/// One desk prop: frame `frame` of `sprite`, its art's top-left at buffer pixel
/// `at`, where the desk art's mark for it stands it, turned as its desk turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct StoodProp {
    pub(crate) sprite: crate::pack::Piece,
    pub(crate) frame: usize,
    pub(crate) at: (u16, u16),
    pub(crate) flip: Flip,
}

/// Which art a prop draws: a sprite's frame, turned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Art {
    pub(crate) sprite: crate::pack::Piece,
    pub(crate) frame: usize,
    pub(crate) flip: Flip,
}

impl Art {
    /// `sprite`'s first frame, as drawn.
    pub(crate) fn still(sprite: crate::pack::Piece) -> Self {
        Self {
            sprite,
            frame: 0,
            flip: Flip::None,
        }
    }
}

/// How a prop's art is turned: the far meeting chair side to side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Flip {
    None,
    Horizontal,
}

/// Everything `paint_figure` draws one figure from, resolved when the list is
/// built.
pub(crate) struct Figure {
    /// The art's top-left, in logical units.
    pub(crate) at: crate::layout::Point,
    /// Grounded by a contact shadow: not sitting on furniture, which grounds a
    /// sitter instead.
    pub(super) shadow: bool,
    pub(crate) key: crate::character::CharacterKey,
}

impl std::fmt::Debug for Figure {
    // `CharacterKey` is not `Debug`; its frame's name and index say which art.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Figure")
            .field("at", &self.at)
            .field("shadow", &self.shadow)
            .field("agent", &self.key.frame.agent_id)
            .field("anim_name", &self.key.frame.anim_name)
            .field("frame_idx", &self.key.frame.frame_idx)
            .finish()
    }
}
