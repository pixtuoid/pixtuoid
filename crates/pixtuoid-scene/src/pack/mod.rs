//! The sprite pack: `sprites/default/`, compiled in (`include_str!`), so the
//! binary ships standalone, and parsed into [`OfficeArt`]: the pieces and what
//! the scene reads off them, each guaranteed by the parse.

mod density;
mod lookup;
#[cfg(test)]
pub(crate) mod validate;

use std::collections::BTreeMap;
use std::num::NonZeroU16;

use enum_map::EnumMap;
use pixtuoid_core::sprite::Sprite;
use pixtuoid_core::sprite::error::PackError;
use pixtuoid_core::sprite::format::{Density, IconArt, Pack, Piece, load_pack_from_strings};
use strum::VariantArray as _;

use crate::display::Icon;
use crate::render_scale::RenderScale;

pub(crate) use density::{DenseFrame, densest, densest_frame};
#[cfg(test)]
pub(crate) use lookup::MONITOR_KEYS;
pub(crate) use lookup::{
    CLOCK_FACE_KEY, COOLER_WATER, DESK_BEZEL_RAISE, DESK_BULB_KEY, ICON_INK_KEY,
    NORTH_SOFA_SEAT_ROWS, PropMark, SCREEN_GLASS_KEY, SCREEN_TEXT_KEY, appliance_frame_index,
    appliance_overrides, desk_art, desk_art_top, desk_mark, desk_prop_overrides, drawn_in,
    fixture_overrides, looping_frame_index, prop_left,
};

/// Which desk art a seat faces: only a back-turned seat needs its own, since
/// its occupant y-sorts in FRONT and covers the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, enum_map::Enum, strum::VariantArray)]
pub enum Desk {
    /// The viewer-facing desk, for a seat facing south, east or west.
    South,
    /// The back-turned desk, for a seat facing north.
    North,
}

impl Desk {
    /// The desk a seat facing `facing` sits at.
    pub(crate) fn facing(facing: crate::layout::Facing) -> Self {
        match facing {
            crate::layout::Facing::North => Desk::North,
            crate::layout::Facing::South
            | crate::layout::Facing::East
            | crate::layout::Facing::West => Desk::South,
        }
    }

    /// Its art.
    pub(crate) fn piece(self) -> Piece {
        match self {
            Desk::South => Piece::Desk,
            Desk::North => Piece::DeskNorth,
        }
    }

    /// Whether it stands its props mirrored, each on its mark's bottom-right
    /// cell: the back-turned desk is the viewer-facing one turned round, so
    /// its props turn with it.
    pub(crate) fn mirrors_props(self) -> bool {
        self == Desk::North
    }

    /// The overlay drawn over the props on its art, if it has one: what of
    /// the desk stands nearer the viewer than its sitter's props.
    pub(crate) fn front(self) -> Option<Piece> {
        Piece::VARIANTS
            .iter()
            .copied()
            .find(|p| p.overlay_of() == Some(self.piece()))
    }
}

/// A prop a desk stands at its mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, enum_map::Enum, strum::VariantArray)]
pub enum DeskProp {
    /// The cup, whose steam rises from it.
    Cup,
    /// The token tower.
    Tower,
}

impl DeskProp {
    /// The `@mark` a desk's first frame names it by.
    pub(crate) fn mark(self) -> &'static str {
        match self {
            DeskProp::Cup => "cup",
            DeskProp::Tower => "tower",
        }
    }
}

/// A desk's art at one density and what the scene reads off it.
#[derive(Debug, Clone)]
pub(crate) struct DeskArt {
    pub(crate) sprite: Sprite,
    /// Each prop's mark on the art's own grid.
    pub(crate) props: EnumMap<DeskProp, (u16, u16)>,
    /// The layout cell, from the art's top-left, that the middle of its
    /// [`DESK_BULB_KEY`] pixels lies in: its lamp's pool centres there.
    pub(crate) bulb: (u16, u16),
}

/// The wall clock's art at one density and how far its face reaches from its
/// centre along its middle row, in that art's pixels: the hands stay inside
/// the rim the art draws.
#[derive(Debug, Clone)]
pub(crate) struct ClockArt {
    pub(crate) sprite: Sprite,
    pub(crate) face_reach: f32,
}

/// A piece's art, or what the scene reads off it, at its base and at each
/// density the pack redraws it at.
#[derive(Debug, Clone)]
pub(crate) struct Densities<T> {
    base: T,
    variants: BTreeMap<Density, T>,
}

impl<T> Densities<T> {
    /// `piece`'s art in `pack`, each density read by `read`.
    fn of(
        pack: &Pack,
        piece: Piece,
        mut read: impl FnMut(&Sprite, Density) -> Result<T, ArtError>,
    ) -> Result<Self, ArtError> {
        Ok(Densities {
            base: read(pack.piece(piece), Density::ONE)?,
            variants: pack
                .variants_of(piece)
                .iter()
                .map(|(&d, sprite)| Ok((d, read(sprite, d)?)))
                .collect::<Result<_, ArtError>>()?,
        })
    }

    /// The [`densest`] at `scale`.
    pub(crate) fn at(&self, scale: RenderScale) -> (&T, Density, NonZeroU16) {
        densest(&self.base, &self.variants, scale)
    }

    /// The base, at 1x.
    pub(crate) fn base(&self) -> &T {
        &self.base
    }
}

/// Why the bundled pack can't draw the office.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ArtError {
    /// The pack itself does not load.
    #[error(transparent)]
    Pack(#[from] PackError),
    /// A desk leaves a prop's mark out.
    #[error("{} at {density}x marks no {}", desk.piece().name(), prop.mark())]
    #[non_exhaustive]
    DeskMark {
        /// The desk.
        desk: Desk,
        /// The density of the art that leaves it out.
        density: Density,
        /// The prop.
        prop: DeskProp,
    },
    /// A desk draws no bulb, or one too far above it.
    #[error("{} at {density}x draws no bulb in {DESK_BULB_KEY:?} the desk can hang", desk.piece().name())]
    #[non_exhaustive]
    DeskBulb {
        /// The desk.
        desk: Desk,
        /// The density of the art.
        density: Density,
    },
    /// The wall clock draws no face along its middle row.
    #[error("wall_clock at {density}x draws no face in {CLOCK_FACE_KEY:?} along its middle row")]
    #[non_exhaustive]
    ClockFace {
        /// The density of the art.
        density: Density,
    },
    /// An icon has no `[icons]` art.
    #[error("[icons] has no {}", icon.art())]
    #[non_exhaustive]
    IconArt {
        /// The icon.
        icon: Icon,
    },
}

/// The pack, parsed into what the office draws: [`Pack`]'s pieces, and each
/// desk's props and bulb, the clock's face and every icon's art, guaranteed by
/// the parse rather than checked where a painter reads them.
#[derive(Debug, Clone)]
pub struct OfficeArt {
    pack: Pack,
    desks: EnumMap<Desk, Densities<DeskArt>>,
    bulb_offsets: EnumMap<Desk, (u16, i16)>,
    clock: Densities<ClockArt>,
    icons: EnumMap<Icon, IconArt>,
}

impl OfficeArt {
    /// `pack` parsed.
    ///
    /// # Errors
    ///
    /// If a desk leaves a prop or its bulb out, the clock its face, or an
    /// icon its art.
    pub(crate) fn parse(pack: Pack) -> Result<Self, ArtError> {
        let desks = EnumMap::try_from_fn(|desk: Desk| {
            Densities::of(&pack, desk.piece(), |sprite, density| {
                read_desk(desk, sprite, density)
            })
        })?;
        let bulb_offsets = EnumMap::try_from_fn(|desk: Desk| {
            let (x, y) = desks[desk].base().bulb;
            // the art's rows from the desk's: its top is `desk_art_top` off the desk
            let art_h = desks[desk].base().sprite.first().height();
            let above =
                DESK_BEZEL_RAISE + art_h.saturating_sub(pack.piece(Piece::Desk).first().height());
            i16::try_from(i32::from(y) - i32::from(above))
                .map(|dy| (x, dy))
                .map_err(|_| ArtError::DeskBulb {
                    desk,
                    density: Density::ONE,
                })
        })?;
        let clock = Densities::of(&pack, Piece::WallClock, |sprite, density| {
            let face_reach = face_reach(sprite).ok_or(ArtError::ClockFace { density })?;
            Ok(ClockArt {
                sprite: sprite.clone(),
                face_reach,
            })
        })?;
        let icons = EnumMap::try_from_fn(|icon: Icon| {
            pack.icon(icon.art())
                .cloned()
                .ok_or(ArtError::IconArt { icon })
        })?;
        Ok(OfficeArt {
            pack,
            desks,
            bulb_offsets,
            clock,
            icons,
        })
    }

    /// The pieces.
    #[cfg(test)]
    pub(crate) fn pack(&self) -> &Pack {
        &self.pack
    }

    /// [`Pack::palette`].
    #[cfg(test)]
    pub(crate) fn palette(&self) -> &pixtuoid_core::sprite::Palette {
        self.pack.palette()
    }

    /// [`Pack::piece`].
    pub fn piece(&self, piece: Piece) -> &Sprite {
        self.pack.piece(piece)
    }

    /// [`Pack::variants_of`].
    pub(crate) fn variants_of(&self, piece: Piece) -> &BTreeMap<Density, Sprite> {
        self.pack.variants_of(piece)
    }

    /// [`Pack::stride`].
    pub(crate) fn stride(&self, walk: pixtuoid_core::sprite::format::Walk) -> NonZeroU16 {
        self.pack.stride(walk)
    }

    /// [`Pack::buildings`].
    pub(crate) fn buildings(
        &self,
    ) -> impl Iterator<Item = &pixtuoid_core::sprite::format::Building> {
        self.pack.buildings()
    }

    /// [`Pack::city_materials`].
    pub(crate) fn city_materials(&self) -> &pixtuoid_core::sprite::format::CityMaterials {
        self.pack.city_materials()
    }

    /// [`Pack::hairstyles`].
    pub(crate) fn hairstyles(
        &self,
    ) -> impl Iterator<Item = &pixtuoid_core::sprite::format::Hairstyle> {
        self.pack.hairstyles()
    }

    /// [`Pack::hairstyle`].
    pub(crate) fn hairstyle(
        &self,
        name: &str,
        density: Density,
    ) -> Option<&pixtuoid_core::sprite::format::Hairstyle> {
        self.pack.hairstyle(name, density)
    }

    /// [`Pack::character_outline`].
    pub(crate) fn character_outline(&self) -> pixtuoid_core::sprite::Rgb {
        self.pack.character_outline()
    }

    /// [`Pack::max_density_variant`].
    pub fn max_density_variant(&self) -> Density {
        self.pack.max_density_variant()
    }

    /// [`Pack::density_variants`].
    pub(crate) fn density_variants(&self) -> &[Density] {
        self.pack.density_variants()
    }

    /// `desk`'s art at each density.
    pub(crate) fn desk(&self, desk: Desk) -> &Densities<DeskArt> {
        &self.desks[desk]
    }

    /// Where `desk` hangs its lamp's bulb from the desk's point, at 1x, the
    /// rows signed (a bulb may stand above the desk's row).
    pub(crate) fn bulb_offset(&self, desk: Desk) -> (u16, i16) {
        self.bulb_offsets[desk]
    }

    /// The wall clock's art at each density.
    pub(crate) fn clock(&self) -> &Densities<ClockArt> {
        &self.clock
    }

    /// `icon`'s art.
    pub(crate) fn icon(&self, icon: Icon) -> &IconArt {
        &self.icons[icon]
    }
}

/// `desk`'s art `sprite` at `density`, its props' marks and bulb read.
fn read_desk(desk: Desk, sprite: &Sprite, density: Density) -> Result<DeskArt, ArtError> {
    let marks = sprite.marks(0);
    let props = EnumMap::try_from_fn(|prop: DeskProp| {
        marks
            .iter()
            .find(|m| m.name() == prop.mark())
            .map(|m| (m.x(), m.y()))
            .ok_or(ArtError::DeskMark {
                desk,
                density,
                prop,
            })
    })?;
    let bulb = lookup::bulb_cell(sprite, density).ok_or(ArtError::DeskBulb { desk, density })?;
    Ok(DeskArt {
        sprite: sprite.clone(),
        props,
        bulb,
    })
}

/// How far `dial`'s face ([`CLOCK_FACE_KEY`]) reaches from its centre along
/// its middle row, in its own pixels; `None` for art that draws no face there.
fn face_reach(dial: &Sprite) -> Option<f32> {
    let frame = dial.first();
    let face = dial.recolorable_at(0).drawn_in(&[CLOCK_FACE_KEY]);
    let (w, h) = (usize::from(frame.width()), usize::from(frame.height()));
    let row = &face[h / 2 * w..(h / 2 + 1) * w];
    let first = row.iter().position(|&f| f)?;
    Some(w as f32 / 2.0 - first as f32)
}

/// The bundled art, for unit tests: parsed once per process, since the parse
/// dominates a test that loads it per frame; each caller gets its own copy.
#[cfg(test)]
pub(crate) fn test_office() -> OfficeArt {
    static ART: std::sync::OnceLock<OfficeArt> = std::sync::OnceLock::new();
    ART.get_or_init(|| load_bundled_pack().expect("default pack loads"))
        .clone()
}

/// The bundled pack's pieces, for unit tests: [`test_office`]'s.
#[cfg(test)]
pub(crate) fn test_default_pack() -> Pack {
    test_office().pack().clone()
}

/// The default pack's manifest, as `build.rs` embeds it.
const BUNDLED_PACK_TOML: &str = include_str!(concat!(env!("OUT_DIR"), "/bundled_pack.toml"));

/// The compiled-in default pack, parsed: all a build without `native` can load.
///
/// # Errors
///
/// If the embedded manifest or a bundled sprite fails to load, or the pack
/// can't draw the office ([`ArtError`]).
pub fn load_bundled_pack() -> Result<OfficeArt, ArtError> {
    OfficeArt::parse(load_pack_from_strings(
        BUNDLED_PACK_TOML,
        &bundled_sprite_srcs(),
    )?)
}

/// Every default sprite as `(filename, source)`: every `.sprite` in
/// `sprites/default/`, listed by `build.rs` (less, without `cutaway-assets`, the
/// frames only a density variant draws), so a sprite committed there cannot be
/// left out by omission. `test_pack_with` swaps files within this EXACT set.
fn bundled_sprite_srcs() -> Vec<(&'static str, &'static str)> {
    const SPRITES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/bundled_sprites.rs"));
    SPRITES.to_vec()
}

/// The bundled pack without its density art, with each `(file, source)` in
/// `overrides` swapped in: a swapped base would orphan the variants drawn over
/// it.
#[cfg(test)]
pub(crate) fn test_pack_with(overrides: &[(&str, &'static str)]) -> Pack {
    let (toml, mut srcs) = base_pack_srcs();
    for &(file, source) in overrides {
        let entry = srcs
            .iter_mut()
            .find(|(name, _)| *name == file)
            .expect("an override names a base sprite");
        entry.1 = source;
    }
    load_pack_from_strings(&toml, &srcs).expect("the test pack loads")
}

/// The bundled manifest without its density art, and the sprites it still
/// draws: the web hero's pack.
#[cfg(test)]
fn base_pack_srcs() -> (String, Vec<(&'static str, &'static str)>) {
    let (toml, dropped) =
        density_art::bundled_without_density_art(include_str!("../../sprites/default/pack.toml"));
    let srcs = bundled_sprite_srcs()
        .into_iter()
        .filter(|(name, _)| !dropped.contains(*name))
        .collect();
    (toml, srcs)
}

/// The bundled pack without its density art, parsed, with each table `extra`
/// names laid over the bundled manifest's (a key it repeats replaces the
/// bundled one) and `frames` added to its sprites: a test names only the pieces
/// it changes and keeps the desks, clock and icons the parse requires.
#[cfg(test)]
pub(crate) fn test_office_with(extra: &str, frames: &[(&str, &str)]) -> OfficeArt {
    let (toml, srcs) = base_pack_srcs();
    let mut manifest: toml_edit::DocumentMut = toml.parse().expect("the bundled manifest parses");
    let extra: toml_edit::DocumentMut = extra.parse().expect("the test tables parse");
    for (name, tables) in extra.iter() {
        let tables = tables.as_table_like().expect("a table of tables");
        let into = manifest
            .entry(name)
            .or_insert_with(toml_edit::table)
            .as_table_like_mut()
            .expect("the bundled manifest has this table");
        for (key, value) in tables.iter() {
            into.insert(key, value.clone());
        }
    }
    let srcs: Vec<(&str, &str)> = srcs.into_iter().chain(frames.iter().copied()).collect();
    let pack = load_pack_from_strings(&manifest.to_string(), &srcs).expect("the test pack loads");
    OfficeArt::parse(pack).expect("the test pack parses")
}

/// The bundled pack with its manifest's `old` text read as `new`, for a test
/// of what a pack declares.
#[cfg(test)]
pub(crate) fn test_pack_declaring(old: &str, new: &str) -> Pack {
    assert!(
        BUNDLED_PACK_TOML.contains(old),
        "the bundled manifest says {old:?}"
    );
    load_pack_from_strings(
        &BUNDLED_PACK_TOML.replacen(old, new, 1),
        &bundled_sprite_srcs(),
    )
    .expect("the test pack loads")
}

#[cfg(test)]
#[path = "../../build_support/density_art.rs"]
mod density_art;

#[cfg(test)]
#[path = "../../build_support/comments.rs"]
mod comments;

#[cfg(test)]
mod tests {
    use super::validate::validate_pack;
    use super::*;
    use crate::display::pen::test_density;
    use crate::render_scale::RenderScale;
    use pixtuoid_core::sprite::format::Density;

    /// The beat check reaches every loop a painter plays: a typist off the
    /// beat is reported.
    #[test]
    fn a_typist_off_the_beat_is_reported() {
        let pack = test_pack_declaring(
            "[animations.typing]\nframes   = [\"typing_0.sprite\", \"typing_1.sprite\"]\nframe_ms = 125\n",
            "[animations.typing]\nframes   = [\"typing_0.sprite\", \"typing_1.sprite\"]\nframe_ms = 400\n",
        );
        let off: Vec<_> = validate_pack(&pack)
            .off_beat_loops
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(off, ["typing"]);
    }

    /// Every key the desk props take a theme colour in is one their art draws,
    /// at every density: a key renamed in the pack would stop the theme
    /// reaching the prop.
    #[test]
    fn the_desk_props_draw_the_keys_the_theme_recolours() {
        let pack = test_office();
        for s in [1, pack.max_density_variant().get()] {
            let scale = crate::render_scale::RenderScale::new(s).expect("nonzero");
            for (sprite, frame, keys) in [
                (
                    Piece::DeskCup,
                    0,
                    &[lookup::CUP_KEY, lookup::CUP_SHADE_KEY][..],
                ),
                (
                    Piece::TokenTower,
                    0,
                    &[lookup::PAPER_KEY, lookup::PAPER_SHADE_KEY],
                ),
                (Piece::TokenSheet, 0, &[lookup::PAPER_KEY]),
            ] {
                let art = densest_frame(&pack, sprite, frame, scale);
                for &key in keys {
                    assert!(
                        drawn_in(&art, &[key]).contains(&true),
                        "{} at scale {s} draws no {key:?}",
                        sprite.name()
                    );
                }
            }
        }
    }

    /// Art that loads but draws other than its author meant is a finding, so
    /// the bundled pack has none.
    #[test]
    fn the_bundled_pack_passes_its_own_validation() {
        let pack = test_default_pack();
        let report = validate_pack(&pack);
        assert_eq!(report.finding_count(), 0, "{report:?}");
    }

    /// A desk's front is its own art, cut down: on the desk's canvas, every
    /// pixel it draws the desk's, so a desk with no props in front of its
    /// front paints as it always did.
    #[test]
    fn a_desk_front_is_its_desk_cut_down() {
        let pack = test_office();
        let front = Desk::South.front().expect("the bundled desk has a front");
        for scale in [
            RenderScale::ONE,
            RenderScale::from(pack.max_density_variant()),
        ] {
            let desk = densest_frame(&pack, Piece::Desk, 0, scale);
            let over = densest_frame(&pack, front, 0, scale);
            assert_eq!(
                (over.frame.width(), over.frame.height(), over.blit_at),
                (desk.frame.width(), desk.frame.height(), desk.blit_at),
                "at {scale:?}, the front keeps its desk's canvas"
            );
            let mut drawn = 0;
            for y in 0..desk.frame.height() {
                for x in 0..desk.frame.width() {
                    if let Some(px) = over.frame.get(x, y).copied().flatten() {
                        drawn += 1;
                        assert_eq!(
                            desk.frame.get(x, y).copied().flatten(),
                            Some(px),
                            "at {scale:?}, the front differs from its desk at ({x}, {y})"
                        );
                    }
                }
            }
            assert!(drawn > 0, "at {scale:?}, the front draws something");
        }
        assert_eq!(
            Desk::North.front(),
            None,
            "nothing stands before a back-turned sitter's props"
        );
    }

    /// A back-turned desk's mirrored tower teeters off its west wing: every
    /// such desk the layout places leaves it the room, so none loses its tower.
    #[test]
    fn every_back_turned_desk_has_room_for_its_teeter() {
        use crate::layout::{Facing, SceneLayout};
        let pack = test_office();
        let tower = densest_frame(&pack, Piece::TokenTower, 0, RenderScale::ONE)
            .frame
            .width();
        let mut desks = 0;
        for (w, h) in [(60, 40), (100, 60), (160, 96), (240, 144), (400, 240)] {
            for seed in 0..4 {
                let Some(layout) = SceneLayout::compute_with_seed(w, h, None, seed) else {
                    continue;
                };
                for &desk in &layout.home_desks {
                    if layout.desk_facing_at(desk) != Facing::North {
                        continue;
                    }
                    desks += 1;
                    let mark = desk_mark(&pack, desk, Facing::North, DeskProp::Tower);
                    assert!(mark.mirrored, "the back-turned desk mirrors its props");
                    assert!(
                        mark.left(tower).is_some(),
                        "{w}x{h} seed {seed}: the desk at {desk:?} has no room for its tower"
                    );
                }
            }
        }
        assert!(desks > 0, "the sample must place back-turned desks");
    }

    /// The pack authors' guide names the desk's contract by the keys and marks
    /// the painters read: a rename here fails until the guide follows.
    #[test]
    fn the_guide_names_the_desk_contract() {
        let guide = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/AGENTS.md"));
        for needle in [
            format!("@mark {} <x> <y>", DeskProp::Cup.mark()),
            format!("@mark {} <x> <y>", DeskProp::Tower.mark()),
            format!("palette key `{DESK_BULB_KEY}`"),
        ] {
            assert!(guide.contains(&needle), "the sprite format lost {needle:?}");
        }
    }

    /// `build.rs` embeds every sprite in `sprites/default/`, so one no animation,
    /// hairstyle or building registers — a generated file whose `pack.toml`
    /// entry was never written — ships as dead bytes and draws nothing. A sprite
    /// the pack still loads without is exactly that.
    #[test]
    fn every_bundled_sprite_is_a_frame_the_pack_loads() {
        let unregistered = undrawn(BUNDLED_PACK_TOML, &bundled_sprite_srcs());
        assert!(
            unregistered.is_empty(),
            "sprites pack.toml never registers: {unregistered:?}"
        );
    }

    /// The sprites `toml` loads without: the ones nothing it registers draws.
    fn undrawn<'a>(toml: &str, srcs: &[(&'a str, &'a str)]) -> Vec<&'a str> {
        let read: std::collections::HashSet<String> =
            pixtuoid_core::sprite::format::frames_read_by(toml, srcs)
                .expect("the whole set loads")
                .into_iter()
                .collect();
        srcs.iter()
            .map(|&(name, _)| name)
            .filter(|&name| !read.contains(name))
            .collect()
    }

    /// The web hero's pack. Nothing runs this suite without `cutaway-assets`
    /// (`just hack` builds no tests), so this is the one place it is loaded.
    #[test]
    fn the_pack_without_density_art_loads_whole() {
        let (_, dropped) = density_art::bundled_without_density_art(include_str!(
            "../../sprites/default/pack.toml"
        ));
        assert!(!dropped.is_empty(), "the bundled pack ships density art");
        let (toml, srcs) = base_pack_srcs();
        let pack = load_pack_from_strings(&toml, &srcs).expect("loads without density art");
        assert_eq!(pack.max_density_variant(), Density::ONE);
        assert!(
            pack.buildings().next().is_some(),
            "the city keeps its buildings"
        );
        let d = |n| test_density(n);
        assert!(
            pack.buildings().all(|b| b.variant(d(4)).is_none()),
            "at their base alone"
        );
        let report = validate_pack(&pack);
        assert_eq!(report.finding_count(), 0, "{report:?}");
        let undrawn = undrawn(&toml, &srcs);
        assert!(undrawn.is_empty(), "undrawn sprites: {undrawn:?}");
    }

    /// What a default run's render scale rounds to (`RenderScale::fit`): a
    /// change to the bundled art's densest variant should be a decision.
    #[test]
    #[cfg(feature = "cutaway-assets")]
    fn the_bundled_pack_is_drawn_at_most_at_4x() {
        assert_eq!(test_default_pack().max_density_variant().get(), 4);
    }

    /// Each bundled building ships a variant at the bundled art's density,
    /// beside the base the classic painter draws, for a city drawn on that
    /// art's grid.
    #[test]
    #[cfg(feature = "cutaway-assets")]
    fn every_bundled_building_is_drawn_at_1x_and_4x() {
        let pack = test_default_pack();
        assert!(
            pack.buildings().next().is_some(),
            "the bundled pack draws a city"
        );
        let d = |n| test_density(n);
        for b in pack.buildings() {
            assert!(b.variant(d(4)).is_some(), "{}", b.name());
        }
    }

    /// Core's creature list is exactly the animations the pets and the gateway
    /// mascots draw, so neither can gain a creature the other misses.
    #[test]
    fn the_creature_animations_are_the_pets_and_mascots() {
        let drawn: std::collections::BTreeSet<Piece> = crate::pet::PetKind::ALL
            .iter()
            .flat_map(|k| [k.walk_anim().piece(), k.sit_anim(), k.sleep_anim()])
            .chain(
                pixtuoid_core::source::registry::registered_source_names()
                    .filter_map(crate::creatures::gateway_mascot_def)
                    .flat_map(|m| [m.walk.piece(), m.rest]),
            )
            .collect();
        let listed: std::collections::BTreeSet<Piece> = Piece::VARIANTS
            .iter()
            .copied()
            .filter(|p| p.kind() == pixtuoid_core::sprite::format::PieceKind::Creature)
            .collect();
        assert_eq!(drawn, listed);
    }

    /// A transparent last row lifts a piece off the floor both painters ground
    /// it on.
    #[test]
    fn every_bundled_furniture_frame_draws_its_bottom_row() {
        let pack = test_default_pack();
        let floating: Vec<String> = Piece::VARIANTS
            .iter()
            .copied()
            .filter(|p| {
                p.kind() == pixtuoid_core::sprite::format::PieceKind::Furniture
                    && p.overlay_of().is_none()
            })
            .flat_map(|p| {
                let name = p.name();
                std::iter::once((name.to_owned(), pack.piece(p))).chain(
                    pack.variants_of(p)
                        .iter()
                        .map(move |(d, s)| (format!("{name}@{d}x"), s)),
                )
            })
            .filter(|(_, s)| {
                s.frames().iter().any(|f| {
                    let w = usize::from(f.width());
                    f.as_slice()[f.as_slice().len() - w..]
                        .iter()
                        .all(Option::is_none)
                })
            })
            .map(|(name, _)| name)
            .collect();
        assert!(floating.is_empty(), "{floating:?}");
    }

    #[test]
    fn character_sprite_size_matches_the_bundled_pack() {
        let pack = test_default_pack();
        let frame = pack.piece(Piece::Standing).first();
        let (w, h) = (frame.width(), frame.height());
        assert_eq!(
            w,
            crate::layout::CHARACTER_SPRITE_W,
            "bundled 'standing' sprite is {w}px wide but CHARACTER_SPRITE_W is {} — \
             update the const so hit-test/decor/label geometry tracks the pack",
            crate::layout::CHARACTER_SPRITE_W
        );
        assert_eq!(
            h,
            crate::layout::CHARACTER_SPRITE_H,
            "bundled 'standing' sprite is {h}px tall but CHARACTER_SPRITE_H is {} — \
             update the const so the hit-test box and the painter's fallback track the pack",
            crate::layout::CHARACTER_SPRITE_H
        );
    }

    /// Every character pose fits the half-block's 8x12 sprite
    /// ([`CHARACTER_SPRITE_W`](crate::layout::CHARACTER_SPRITE_W) by
    /// [`CHARACTER_SPRITE_H`](crate::layout::CHARACTER_SPRITE_H)): a taller one
    /// breaks on terminals whose cells are taller than 1:2.
    #[test]
    fn every_character_pose_fits_the_half_block_sprite() {
        let pack = test_default_pack();
        for &piece in Piece::VARIANTS
            .iter()
            .filter(|p| p.kind() == pixtuoid_core::sprite::format::PieceKind::Character)
        {
            let name = piece.name();
            for frame in pack.piece(piece).frames() {
                assert!(
                    frame.width() <= crate::layout::CHARACTER_SPRITE_W
                        && frame.height() <= crate::layout::CHARACTER_SPRITE_H,
                    "{name} is {}x{}",
                    frame.width(),
                    frame.height()
                );
            }
        }
    }

    /// A facing does not change how big a desk IS: `desk_north` is taller only above `desk.y`,
    /// and below it the same desk mirrored, as its arrangement mirrors the lamp. Checked on the
    /// edge COLUMNS the monitor never covers — the middle legitimately differs.
    #[test]
    fn both_desk_variants_are_the_same_desk_below_the_monitor() {
        let pack = test_default_pack();
        let (base, north) = (
            pack.piece(Piece::Desk).first(),
            pack.piece(Piece::DeskNorth).first(),
        );
        assert_eq!(base.width(), north.width(), "a facing never changes width");
        // Both blit so their BOTTOM rows coincide, so the taller one's extra rows are all above.
        let lift = north
            .height()
            .checked_sub(base.height())
            .expect("the raised variant is the taller one");
        let raise = crate::pack::DESK_BEZEL_RAISE;
        let edges = [0, 1, base.width() - 2, base.width() - 1];
        for x in edges {
            for dy in 0..(base.height() - raise) {
                let b = base.get(x, raise + dy);
                let n = north.get(base.width() - 1 - x, raise + lift + dy);
                assert_eq!(
                    b, n,
                    "column {x} differs at desk.y+{dy}: the two variants must be \
                     the same desk, mirrored, below the monitor"
                );
            }
        }

        // The column loop above starts at `desk.y`, so wood a variant grows ABOVE that row is
        // invisible to it: count the rows holding surface — the wood or its lit back edge — in
        // any column.
        let surface = ['D', 'O'].map(|key| {
            pack.palette()
                .get(key)
                .flatten()
                .unwrap_or_else(|| panic!("{key:?} is an opaque surface key"))
        });
        let surface_rows = |f: &pixtuoid_core::sprite::Frame| {
            (0..f.height())
                .filter(|&y| {
                    (0..f.width()).any(|x| {
                        f.get(x, y)
                            .copied()
                            .flatten()
                            .is_some_and(|c| surface.contains(&c))
                    })
                })
                .count() as u16
        };
        for (name, art) in [("desk", base), ("desk_north", north)] {
            assert_eq!(
                surface_rows(art),
                crate::layout::DESK_SURFACE_ROWS,
                "{name} draws {} rows of surface; DESK_SURFACE_ROWS declares {}",
                surface_rows(art),
                crate::layout::DESK_SURFACE_ROWS
            );
        }
    }

    /// The generated desk art follows the layout's row split — the bezel raise,
    /// then surface, front lip and legs — which scripts/gen-art.py copies; a
    /// drift there moves the wood the depth sort and the glow assume. Read by
    /// STRUCTURE, so an art restyle cannot fail it: legs show open floor
    /// between them, the lip spans the width, the surface is opaque at the west
    /// edge, and nothing but the monitor rises above it.
    #[test]
    fn a_desks_rows_follow_the_layout() {
        use crate::layout::{DESK_FRONT_ROWS, DESK_LEG_ROWS, DESK_SURFACE_ROWS};
        let pack = test_default_pack();
        let raise = crate::pack::DESK_BEZEL_RAISE;
        let base_h = raise + DESK_SURFACE_ROWS + DESK_FRONT_ROWS + DESK_LEG_ROWS;
        for piece in [Piece::Desk, Piece::DeskNorth] {
            let name = piece.name();
            let f = pack.piece(piece).first();
            let (w, h) = (f.width(), f.height());
            let opaque = |x: u16, y: u16| f.get(x, y).copied().flatten().is_some();
            let lift = h
                .checked_sub(base_h)
                .expect("no desk is shorter than the layout's split");
            let legs0 = h - DESK_LEG_ROWS;
            let lip0 = legs0 - DESK_FRONT_ROWS;
            let top = lip0 - DESK_SURFACE_ROWS;
            assert_eq!(
                top,
                raise + lift,
                "{name}: the surface starts below the monitor's raise"
            );
            for y in legs0..h {
                assert!(
                    opaque(0, y) && !opaque(w / 2, y),
                    "{name} row {y}: legs, open between"
                );
            }
            for y in lip0..legs0 {
                assert!(
                    (0..w).all(|x| opaque(x, y)),
                    "{name} row {y}: the lip spans the desk"
                );
            }
            for y in top..lip0 {
                assert!(opaque(0, y), "{name} row {y}: surface at the west edge");
            }
            for y in 0..top {
                assert!(
                    !opaque(0, y),
                    "{name} row {y}: only the monitor rises above the wood"
                );
            }
        }
    }

    // The desk art's width is scripts/gen-art.py's `DESK_ART_W`, a copy of
    // `desk_furniture_def().visual.w`: a `DESK_W` edit moves `visual.w` but not
    // the generated art, silently desyncing render vs mask/occlusion/collision.
    #[test]
    fn desk_sprite_width_tracks_the_footprint_overhang() {
        let pack = test_default_pack();
        let w = pack.piece(Piece::Desk).first().width();
        assert_eq!(
            w,
            crate::layout::desk_furniture_def().visual.w,
            "bundled 'desk' sprite is {w}px wide but visual.w is {} — \
             a DESK_W edit moved visual.w but not scripts/gen-art.py's DESK_ART_W; render/mask/z-sort will drift",
            crate::layout::desk_furniture_def().visual.w
        );
    }

    /// `src` with `key` drawn as the desk's body instead.
    fn without_key(src: &str, key: char) -> &'static str {
        src.lines()
            .map(|line| {
                if line.starts_with(['@', '#']) {
                    line.to_owned()
                } else {
                    line.replace(key, "D")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            .leak()
    }

    /// A desk that marks no `cup` leaves its cup nowhere to stand.
    #[test]
    fn a_desk_missing_a_prop_mark_does_not_parse() {
        let desk: &'static str = include_str!("../../sprites/default/desk.sprite")
            .lines()
            .filter(|l| !l.starts_with("@mark cup"))
            .collect::<Vec<_>>()
            .join("\n")
            .leak();
        let err = OfficeArt::parse(test_pack_with(&[("desk.sprite", desk)])).expect_err("no cup");
        assert!(
            matches!(
                err,
                ArtError::DeskMark {
                    desk: Desk::South,
                    prop: DeskProp::Cup,
                    ..
                }
            ),
            "{err}"
        );
    }

    /// A variant desk is parsed as its base is: at 4x, too, a desk that marks
    /// no `cup` does not parse.
    #[cfg(feature = "cutaway-assets")]
    #[test]
    fn a_variant_desk_missing_a_prop_mark_does_not_parse() {
        let srcs: Vec<(&str, &str)> = bundled_sprite_srcs()
            .into_iter()
            .map(|(name, src)| {
                if name != "desk@4x.sprite" {
                    return (name, src);
                }
                let src: &'static str = src
                    .lines()
                    .filter(|l| !l.starts_with("@mark cup"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .leak();
                (name, src)
            })
            .collect();
        let pack = load_pack_from_strings(BUNDLED_PACK_TOML, &srcs).expect("the pack loads");
        let err = OfficeArt::parse(pack).expect_err("no cup at 4x");
        assert!(
            matches!(
                err,
                ArtError::DeskMark {
                    desk: Desk::South,
                    prop: DeskProp::Cup,
                    density,
                } if density.get() == 4
            ),
            "{err}"
        );
    }

    /// A desk that draws no bulb hangs no lamp.
    #[test]
    fn a_desk_without_a_bulb_does_not_parse() {
        let desk = without_key(
            include_str!("../../sprites/default/desk_north.sprite"),
            DESK_BULB_KEY,
        );
        let err =
            OfficeArt::parse(test_pack_with(&[("desk_north.sprite", desk)])).expect_err("no bulb");
        assert!(
            matches!(
                err,
                ArtError::DeskBulb {
                    desk: Desk::North,
                    ..
                }
            ),
            "{err}"
        );
    }

    /// A clock that draws no face has no rim for the hands to stay inside.
    #[test]
    fn a_clock_without_a_face_does_not_parse() {
        let clock = without_key(
            include_str!("../../sprites/default/wall_clock.sprite"),
            CLOCK_FACE_KEY,
        );
        let err =
            OfficeArt::parse(test_pack_with(&[("wall_clock.sprite", clock)])).expect_err("no face");
        assert!(matches!(err, ArtError::ClockFace { .. }), "{err}");
    }

    /// An icon the pack leaves out of `[icons]` has no art to draw.
    #[test]
    fn an_icon_missing_from_the_pack_does_not_parse() {
        let err = OfficeArt::parse(test_pack_declaring("[icons.alert]", "[icons.alarm]"))
            .expect_err("no alert icon");
        assert!(
            matches!(err, ArtError::IconArt { icon: Icon::Alert }),
            "{err}"
        );
    }
}
