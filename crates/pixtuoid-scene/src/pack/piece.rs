//! The pieces a pack draws, and the manifest keys that name them.

#[cfg(test)]
use strum::VariantArray;

use crate::sprite::format::{Density, split_density_variant};

/// Every piece a pack draws: the character poses, the furniture and the
/// room's fixtures, and the creatures. A pack ships each, so a loaded pack
/// hands out any of them ([`OfficeArt::piece`](super::OfficeArt::piece)); the
/// manifest keys it by its `name`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    enum_map::Enum,
    strum::VariantArray,
    strum::IntoStaticStr,
    strum::EnumString,
)]
#[strum(serialize_all = "snake_case")]
#[expect(missing_docs, reason = "a variant is its manifest key, `Piece::name`")]
pub enum Piece {
    Seated,
    Typing,
    Standing,
    Walking,
    WalkingBack,
    SeatedSleeping,
    SeatedSleepingAlt,
    HoldingCoffee,
    BackCouch,
    WalkingCoffee,
    SideSeated,
    SeatedBack,
    TypingBack,
    Desk,
    DeskNorth,
    DeskFront,
    FilingCabinet,
    Plant,
    PlantTall,
    PlantFlower,
    PlantSucculent,
    ChristmasTree,
    ChristmasTreeShort,
    Poinsettia,
    Presents,
    FloorLamp,
    Door,
    MeetingSofa,
    MeetingSofaNorth,
    MeetingScreen,
    Pantry,
    PantrySmall,
    Whiteboard,
    Bookshelf,
    SnackShelf,
    TvStand,
    PhoneBooth,
    StandingDesk,
    BulletinBoard,
    ExitSign,
    DeskChair,
    DeskCup,
    TokenTower,
    TokenSheet,
    VendingMachine,
    Printer,
    MeetingTable,
    KitchenIsland,
    SideTable,
    WaterCooler,
    PantryBin,
    FishTank,
    CoatRack,
    NoticeBoard,
    WallClock,
    MeetingChair,
    CatWalk,
    CatSit,
    CatSleep,
    DogWalk,
    DogSit,
    DogSleep,
    LobsterWalk,
    LobsterRest,
}

impl Piece {
    /// Its key in the manifest's `[animations]`.
    pub(crate) fn name(self) -> &'static str {
        self.into()
    }

    /// The piece `name` keys, if it keys one.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        name.parse().ok()
    }

    /// The walk it is, if a walker steps it by the ground it covers.
    #[cfg(test)]
    pub(crate) fn walk(self) -> Option<Walk> {
        Walk::VARIANTS.iter().copied().find(|w| w.piece() == self)
    }

    /// The fewest frames it is drawn in: an animation that never moves is
    /// not one.
    pub(crate) fn min_frames(self) -> usize {
        match self {
            Piece::Door => 3,
            Piece::Typing
            | Piece::Walking
            | Piece::WalkingBack
            | Piece::CatWalk
            | Piece::DogWalk
            | Piece::LobsterWalk
            | Piece::ChristmasTree
            | Piece::ChristmasTreeShort => 2,
            _ => 1,
        }
    }

    /// The plant it stands in for on a holiday, drawn on that plant's canvas
    /// at every density, so the plant's footprint and box hold for it.
    pub(crate) fn stands_in_for(self) -> Option<Piece> {
        match self {
            Piece::ChristmasTree => Some(Piece::PlantTall),
            Piece::ChristmasTreeShort => Some(Piece::Plant),
            Piece::Poinsettia => Some(Piece::PlantFlower),
            Piece::Presents => Some(Piece::PlantSucculent),
            _ => None,
        }
    }

    /// The piece it is drawn over, on that piece's canvas: `desk_front` is
    /// what of `desk` stands nearer the viewer than its sitter's props. An
    /// overlay keeps that piece's canvas rather than grounding on its own
    /// bottom row.
    pub(crate) fn overlay_of(self) -> Option<Piece> {
        match self {
            Piece::DeskFront => Some(Piece::Desk),
            _ => None,
        }
    }
}

/// A piece a walker steps by the ground it covers: its frames advance by
/// distance, so a planted foot stays planted at any speed (`Pack::stride`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, enum_map::Enum, strum::VariantArray)]
#[expect(missing_docs, reason = "a variant is its piece, `Walk::piece`")]
pub enum Walk {
    Walking,
    WalkingBack,
    WalkingCoffee,
    CatWalk,
    DogWalk,
    LobsterWalk,
}

impl Walk {
    /// Its piece.
    pub fn piece(self) -> Piece {
        match self {
            Walk::Walking => Piece::Walking,
            Walk::WalkingBack => Piece::WalkingBack,
            Walk::WalkingCoffee => Piece::WalkingCoffee,
            Walk::CatWalk => Piece::CatWalk,
            Walk::DogWalk => Piece::DogWalk,
            Walk::LobsterWalk => Piece::LobsterWalk,
        }
    }
}

/// A manifest key: a piece, or a density variant of one (`desk@4x`).
/// Variants are legal BY DERIVATION rather than by their own registry rows: a
/// second list would have to be kept in step with the first.
pub(super) fn parse_key(name: &str) -> Option<(Piece, Option<Density>)> {
    match split_density_variant(name) {
        Some((base, density)) => Some((Piece::from_name(base)?, Some(density))),
        None => Some((Piece::from_name(name)?, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sprite::format::MAX_DENSITY_VARIANT;

    fn d(n: u16) -> Density {
        Density::new(n).expect("nonzero")
    }

    #[test]
    fn every_piece_round_trips_its_name() {
        for &piece in Piece::VARIANTS {
            assert_eq!(Piece::from_name(piece.name()), Some(piece), "{piece:?}");
        }
        assert_eq!(Piece::from_name("idle"), None);
        assert_eq!(Piece::from_name("desk@4x"), None);
    }

    /// Pins [`Piece::min_frames`] and [`Piece::walk`].
    #[test]
    fn a_piece_moves_through_the_frames_its_animation_needs() {
        let moving: Vec<_> = Piece::VARIANTS
            .iter()
            .filter(|p| p.min_frames() > 1)
            .map(|p| (p.name(), p.min_frames()))
            .collect();
        assert_eq!(
            moving,
            [
                ("typing", 2),
                ("walking", 2),
                ("walking_back", 2),
                ("christmas_tree", 2),
                ("christmas_tree_short", 2),
                ("door", 3),
                ("cat_walk", 2),
                ("dog_walk", 2),
                ("lobster_walk", 2),
            ]
        );
        for &walk in Walk::VARIANTS {
            assert_eq!(walk.piece().walk(), Some(walk));
        }
        assert_eq!(
            Piece::VARIANTS
                .iter()
                .filter(|p| p.walk().is_some())
                .count(),
            Walk::VARIANTS.len()
        );
    }

    /// Pins [`Piece::stands_in_for`]: each stand-in replaces a plant, the
    /// only pieces both painters draw in a holiday's art
    /// (`FixtureKind::replaceable_piece`).
    #[test]
    fn each_stand_in_names_the_plant_it_replaces() {
        use crate::layout::PlantKind;
        let stood_in: Vec<_> = Piece::VARIANTS
            .iter()
            .filter_map(|&p| Some((p.name(), p.stands_in_for()?.name())))
            .collect();
        assert_eq!(
            stood_in,
            [
                ("christmas_tree", "plant_tall"),
                ("christmas_tree_short", "plant"),
                ("poinsettia", "plant_flower"),
                ("presents", "plant_succulent"),
            ]
        );
        for &stand_in in Piece::VARIANTS {
            if let Some(plant) = stand_in.stands_in_for() {
                assert!(
                    PlantKind::ALL.iter().any(|k| k.piece() == plant),
                    "{}",
                    stand_in.name()
                );
            }
        }
    }

    /// Pins [`parse_key`].
    #[test]
    fn a_key_names_a_registered_animation_or_a_density_variant_of_one() {
        assert_eq!(parse_key("desk"), Some((Piece::Desk, None)));
        assert_eq!(parse_key("desk@4x"), Some((Piece::Desk, Some(d(4)))));
        assert_eq!(
            parse_key("standing@2x"),
            Some((Piece::Standing, Some(d(2))))
        );
        assert_eq!(
            parse_key("walking_coffee@8x"),
            Some((Piece::WalkingCoffee, Some(d(8))))
        );
        // The BASE must be registered, or a typo'd `dsek@4x` would validate.
        assert_eq!(parse_key("dsek@4x"), None);
        assert_eq!(parse_key("typo"), None);
        assert_eq!(parse_key("desk@1x"), None);
    }

    /// Pins [`MAX_DENSITY_VARIANT`].
    #[test]
    fn a_density_past_the_ceiling_is_not_a_variant_at_all() {
        assert_eq!(split_density_variant("desk@60000x"), None);
        assert_eq!(split_density_variant("desk@65535x"), None);
        // The boundary from both sides, so a future `<` typo cannot slip through.
        assert_eq!(
            split_density_variant("desk@64x"),
            Some(("desk", d(MAX_DENSITY_VARIANT)))
        );
        assert_eq!(split_density_variant("desk@65x"), None);
        // An out-of-range density is not a variant, so the name is simply
        // unknown — never a piece whose base the pack must supply.
        assert_eq!(parse_key("desk@60000x"), None);
    }
}
