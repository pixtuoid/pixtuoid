//! Why a sprite pack failed to load, typed so a caller can match the failure.
//! A variant that wraps a cause keeps the cause out of its own message, so
//! `{:#}` prints each step of the chain once.

use super::HeadView;
use super::format::{DENSITY_VARIANT_SEP, Material, PACK_MANIFEST};

/// A sprite pack that could not be loaded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PackError {
    /// `pack.toml` is not valid TOML, or does not match the manifest schema.
    #[cfg(feature = "pack-toml")]
    #[error("parsing {PACK_MANIFEST}")]
    #[non_exhaustive]
    Manifest {
        /// The parser's diagnostic.
        #[source]
        source: toml::de::Error,
    },
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
        super::PALETTE_CAPACITY
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
    /// A ramp's level is zero or past [`MAX_RAMP_LEVEL`](super::format::MAX_RAMP_LEVEL).
    #[error(
        "ramp {key:?} level {level} must be nonzero and within ±{}",
        super::format::MAX_RAMP_LEVEL
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
        piece: super::format::Piece,
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
        walk: super::format::Piece,
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
        sep = super::format::DENSITY_VARIANT_SEP,
        max = super::format::MAX_DENSITY_VARIANT
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
        density: super::format::Density,
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
    #[error("hair layer {file} must mark its head `{}{}`", super::HEAD_MARK, view.name())]
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
        first: super::format::Density,
        /// A density whose styles differ from it.
        other: super::format::Density,
    },
}

/// A `.sprite` file that does not decode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SpriteError {
    /// A line is wrong.
    #[error("{kind} (line {line})")]
    #[non_exhaustive]
    Line {
        /// The 1-based line: for a frame's shape, the row or mark at fault, or
        /// the `@frame` of a frame with no rows.
        line: usize,
        /// What is wrong on it.
        kind: LineError,
    },
    /// The file holds no `@frame` block.
    #[error("sprite file contains no frames")]
    NoFrames,
}

/// What is wrong on one line of a `.sprite` file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LineError {
    /// An `@frame` header has no number.
    #[error("@frame requires a number")]
    FrameNumber,
    /// Pixel rows or marks come before any `@frame`.
    #[error("pixel data before any @frame")]
    DataBeforeFrame,
    /// A frame has no rows.
    #[error("frame has no rows")]
    NoRows,
    /// A frame has more rows than a grid holds.
    #[error("frame has {rows} rows (maximum {})", u16::MAX)]
    #[non_exhaustive]
    TooManyRows {
        /// How many.
        rows: usize,
    },
    /// A frame's rows are wider than a grid holds.
    #[error("frame row width {width} exceeds the maximum {}", u16::MAX)]
    #[non_exhaustive]
    TooWide {
        /// The width.
        width: usize,
    },
    /// A frame's rows differ in width.
    #[error("inconsistent row width at row {row} (expected {expected}, got {got})")]
    #[non_exhaustive]
    Ragged {
        /// The 0-based row within the frame.
        row: usize,
        /// The first row's width.
        expected: usize,
        /// This row's.
        got: usize,
    },
    /// A pixel token is more than one character.
    #[error("each pixel must be a single character (got {token:?})")]
    #[non_exhaustive]
    PixelToken {
        /// The token.
        token: String,
    },
    /// A pixel names no key of the palette.
    #[error("unknown palette key {key:?}")]
    #[non_exhaustive]
    UnknownKey {
        /// The key.
        key: char,
    },
    /// A pixel's key sits past the indices a frame can hold.
    #[error(
        "palette key {key:?} is past the {} a frame can index",
        super::PALETTE_CAPACITY
    )]
    #[non_exhaustive]
    KeyPastCapacity {
        /// The key.
        key: char,
    },
    /// An `@mark` line does not have a name, a column and a row.
    #[error("@mark takes a name and a column and a row")]
    MarkFields,
    /// A mark's name uses characters a name may not.
    #[error("@mark name {name:?} is not lowercase letters, digits, '.' and '_'")]
    #[non_exhaustive]
    MarkName {
        /// The name.
        name: String,
    },
    /// A mark's coordinate is not a pixel coordinate.
    #[error("@mark {value:?} is not a pixel coordinate")]
    #[non_exhaustive]
    MarkCoordinate {
        /// The coordinate as written.
        value: String,
    },
    /// A head mark names no view.
    #[error("@mark {name} names no view: a head is {}<one of {:?}>", super::HEAD_MARK, super::HeadView::ALL.map(super::HeadView::name))]
    #[non_exhaustive]
    HeadWithoutView {
        /// The mark's name.
        name: String,
    },
    /// A frame names a mark twice, or two heads.
    #[error("a frame names each mark once, and one head")]
    DuplicateMark,
    /// A mark lies outside its frame.
    #[error("@mark {name} {x} {y} lies outside its {width}x{height} frame")]
    #[non_exhaustive]
    MarkOutside {
        /// The mark's name.
        name: String,
        /// Its column.
        x: u16,
        /// Its row.
        y: u16,
        /// The frame's width.
        width: u16,
        /// The frame's height.
        height: u16,
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
    use std::error::Error as _;

    /// Every error with a cause, built once — a new wrapping variant joins here.
    fn wrapping() -> Vec<PackError> {
        let toml = toml::from_str::<toml::Table>("= broken").expect_err("not TOML");
        vec![
            PackError::Manifest { source: toml },
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
