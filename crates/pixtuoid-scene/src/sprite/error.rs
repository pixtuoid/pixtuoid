//! Why a `.sprite` file does not decode, typed so a caller can match the
//! failure.

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
