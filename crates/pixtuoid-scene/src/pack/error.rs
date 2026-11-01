//! Why a sprite pack failed to load, typed so a caller can match the failure.
//! A variant that wraps a cause keeps the cause out of its own message, so
//! `{:#}` prints each step of the chain once.

use super::{Material, Piece};
use crate::sprite::error::SpriteError;
use crate::sprite::format::{DENSITY_VARIANT_SEP, Density, MAX_DENSITY_VARIANT};
use crate::sprite::{HEAD_MARK, HeadView, MAX_RAMP_LEVEL, PALETTE_CAPACITY};

/// The manifest's name in errors.
pub(crate) const PACK_MANIFEST: &str = "pack.toml";

/// A sprite pack that could not be loaded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PackError {
    /// A manifest's JSON transcode is not valid JSON, or does not match the manifest schema.
    #[error("parsing the JSON transcode of {PACK_MANIFEST}: {message}")]
    #[non_exhaustive]
    JsonManifest {
        /// The parser's diagnostic, as text: `serde_json::Error` would cost
        /// the error `UnwindSafe`.
        message: String,
    },
    /// An in-memory pack names a frame it was not given.
    #[error("missing embedded frame {file}")]
    #[non_exhaustive]
    MissingEmbeddedFrame {
        /// The frame path as the manifest names it.
        file: String,
    },
    /// A `.sprite` file does not decode.
    #[error("decoding {file}")]
    #[non_exhaustive]
    Decode {
        /// The frame path as the manifest names it.
        file: String,
        /// What is wrong with it.
        #[source]
        source: SpriteError,
    },
    /// A key that must be one character is not.
    #[error("{what} {key:?} must be exactly one character")]
    #[non_exhaustive]
    NotOneChar {
        /// Which key.
        what: KeySite,
        /// The key as written.
        key: String,
    },
    /// The palette and ramps together declare more keys than a frame can index.
    #[error(
        "the palette declares {keys} keys; a frame can index at most {}",
        PALETTE_CAPACITY
    )]
    #[non_exhaustive]
    PaletteTooLarge {
        /// How many keys they declare.
        keys: usize,
    },
    /// A `[palette]` value is not a color.
    #[error("palette key {key:?}")]
    #[non_exhaustive]
    Color {
        /// The key.
        key: String,
        /// What is wrong with its value.
        #[source]
        source: ColorError,
    },
    /// A key is declared as a color and as a ramp.
    #[error("{key:?} is declared in both [palette] and [ramps]")]
    #[non_exhaustive]
    PaletteAndRamp {
        /// The key.
        key: char,
    },
    /// A ramp steps from a key with no opaque `[palette]` color.
    #[error("ramp {key:?} steps from {of:?}, which has no opaque color in [palette]")]
    #[non_exhaustive]
    RampBase {
        /// The ramp's key.
        key: char,
        /// The key it steps from.
        of: char,
    },
    /// A ramp's level is zero or past `MAX_RAMP_LEVEL`.
    #[error(
        "ramp {key:?} level {level} must be nonzero and within ±{}",
        MAX_RAMP_LEVEL
    )]
    #[non_exhaustive]
    RampLevel {
        /// The ramp's key.
        key: char,
        /// Its level.
        level: i8,
    },
    /// `[city]` names something that is no material.
    #[error("[city] names {name:?}, which is no material")]
    #[non_exhaustive]
    CityUnknownMaterial {
        /// The name.
        name: String,
    },
    /// `[city]` gives no key to a material.
    #[error("[city] names no key for {:?}", material.name())]
    #[non_exhaustive]
    CityMissingMaterial {
        /// The material.
        material: Material,
    },
    /// A key names no opaque palette color.
    #[error("{what} {key:?} is not an opaque key of the palette")]
    #[non_exhaustive]
    NotOpaque {
        /// Which key.
        what: KeySite,
        /// The key.
        key: char,
    },
    /// Two `[city]` materials share a key.
    #[error("[city] draws two materials in {key:?}")]
    #[non_exhaustive]
    CityKeyShared {
        /// The key.
        key: char,
    },
    /// The pack does not draw a piece.
    #[error("[animations] has no {}", piece.name())]
    #[non_exhaustive]
    MissingPiece {
        /// The piece.
        piece: Piece,
    },
    /// An `[animations]` key names no piece, nor a density variant of one.
    #[error("[animations] key {key:?} names no piece")]
    #[non_exhaustive]
    UnknownAnimation {
        /// The key.
        key: String,
    },
    /// An animation has fewer frames than its piece moves through.
    #[error("{key} has {have} frames; it needs at least {need}")]
    #[non_exhaustive]
    TooFewFrames {
        /// The `[animations]` key.
        key: String,
        /// The fewest it needs.
        need: usize,
        /// How many it has.
        have: usize,
    },
    /// A walk declares no `stride`.
    #[error("{} is a walk and needs a stride", walk.name())]
    #[non_exhaustive]
    WalkWithoutStride {
        /// The walk's piece.
        walk: Piece,
    },
    /// A density variant does not redraw its piece frame for frame at the
    /// size its density claims.
    #[error(
        "{key} does not redraw its piece: {variant_frames} frames against its {base_frames}, each its frame's size times the density"
    )]
    #[non_exhaustive]
    VariantDoesNotRedraw {
        /// The variant's key.
        key: String,
        /// The piece's frames.
        base_frames: usize,
        /// The variant's.
        variant_frames: usize,
    },
    /// A building key uses the density separator without a valid density.
    #[error(
        "building {key:?}: `{sep}` marks a density variant, `<name>{sep}<N>x` with N from 2 to {max}",
        sep = DENSITY_VARIANT_SEP,
        max = MAX_DENSITY_VARIANT
    )]
    #[non_exhaustive]
    BuildingDensity {
        /// The building's key.
        key: String,
    },
    /// A building names a plane that is not one.
    #[error("building {key:?} stands in {plane:?}: the planes are \"mid\" and \"near\"")]
    #[non_exhaustive]
    BuildingPlane {
        /// The building's key.
        key: String,
        /// The plane as named.
        plane: String,
    },
    /// A base building names no planes.
    #[error("building {key:?} names no planes to stand in")]
    #[non_exhaustive]
    BuildingWithoutPlanes {
        /// The building's key.
        key: String,
    },
    /// A density variant names planes of its own.
    #[error("building variant {key:?} names planes: its base's are its own")]
    #[non_exhaustive]
    VariantPlanes {
        /// The variant's key.
        key: String,
    },
    /// A density variant has no base building.
    #[error("building variant {key:?} has no base `[buildings.{base}]`")]
    #[non_exhaustive]
    VariantWithoutBase {
        /// The variant's key.
        key: String,
        /// The base it names.
        base: String,
    },
    /// A density variant is not its base redrawn at its density.
    #[error("building variant {key:?} is not {density} times its base's {base_w}x{base_h}")]
    #[non_exhaustive]
    VariantSize {
        /// The variant's key.
        key: String,
        /// Its density.
        density: Density,
        /// The base's width.
        base_w: u16,
        /// The base's height.
        base_h: u16,
    },
    /// A building's sprite is not one frame.
    #[error("building {file} must be one frame")]
    #[non_exhaustive]
    BuildingFrames {
        /// The sprite file.
        file: String,
    },
    /// A building draws a pixel in no `[city]` material.
    #[error("building {file} draws ({x}, {y}) outside its [city] materials")]
    #[non_exhaustive]
    BuildingOutsideMaterials {
        /// The sprite file.
        file: String,
        /// The pixel's column.
        x: usize,
        /// The pixel's row.
        y: usize,
    },
    /// A hairstyle's key names no density of 2x and up.
    #[error(
        "hairstyle {key:?} must be `<name>{DENSITY_VARIANT_SEP}<N>x`: only art of 2x and up is dressed"
    )]
    #[non_exhaustive]
    HairstyleDensity {
        /// The hairstyle's key.
        key: String,
    },
    /// An icon's art is not one frame.
    #[error("icon {file} must be one frame")]
    #[non_exhaustive]
    IconFrames {
        /// The art's sprite file.
        file: String,
    },
    /// A hair layer is not one frame.
    #[error("hair layer {file} must be one frame")]
    #[non_exhaustive]
    HairLayerFrames {
        /// The layer's sprite file.
        file: String,
    },
    /// A hair layer does not mark the head of its view.
    #[error("hair layer {file} must mark its head `{}{}`", HEAD_MARK, view.name())]
    #[non_exhaustive]
    HairLayerHead {
        /// The layer's sprite file.
        file: String,
        /// The view it is drawn for.
        view: HeadView,
    },
    /// The hairstyles differ between densities.
    #[error("hairstyles must be the same styles at every density: {first}x and {other}x differ")]
    #[non_exhaustive]
    HairstylesDiffer {
        /// The lowest density.
        first: Density,
        /// A density whose styles differ from it.
        other: Density,
    },
}

/// A `[palette]` value that is not a color.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ColorError {
    /// It starts with neither `#` nor `transparent`.
    #[error("color must start with '#' or be 'transparent', got {value:?}")]
    #[non_exhaustive]
    Prefix {
        /// The value.
        value: String,
    },
    /// Its hex part is not six hex digits.
    #[error("color {value:?} must be 6 hex digits")]
    #[non_exhaustive]
    Hex {
        /// The value.
        value: String,
    },
}

/// A manifest key that must be one character, or name an opaque color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum KeySite {
    /// A `[palette]` key.
    #[error("palette key")]
    Palette,
    /// A `[ramps]` key.
    #[error("ramp key")]
    Ramp,
    /// The key a ramp steps from.
    #[error("ramp `of`")]
    RampBase,
    /// A `[city]` material's key.
    #[error("[city] material")]
    CityMaterial,
    /// `[characters] outline`.
    #[error("[characters] outline")]
    CharacterOutline,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sprite::error::SpriteError;
    use std::error::Error as _;

    /// Every error with a cause, built once — a new wrapping variant joins here.
    fn wrapping() -> Vec<PackError> {
        vec![
            PackError::Color {
                key: "A".into(),
                source: ColorError::Hex {
                    value: "#12".into(),
                },
            },
            PackError::Decode {
                file: "f.sprite".into(),
                source: SpriteError::NoFrames,
            },
        ]
    }

    /// `{:#}` prints a step and then its cause; a step that also embedded the
    /// cause would print it twice.
    #[test]
    fn no_error_repeats_its_cause_in_its_own_message() {
        for e in wrapping() {
            let cause = e
                .source()
                .expect("a wrapping variant has a cause")
                .to_string();
            assert!(
                !e.to_string().contains(&cause),
                "{e:?} repeats its cause {cause:?}"
            );
        }
    }
}
