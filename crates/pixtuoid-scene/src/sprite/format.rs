use std::num::NonZeroU16;

use crate::grid::Grid;
use crate::sprite::error::{LineError, SpriteError};
use crate::sprite::{
    Frame, HEAD_MARK, HeadMark, IndexedFrame, Mark, Palette, PaletteIndex, Sprite,
};

struct Block {
    header: usize,
    rows: Vec<Vec<PaletteIndex>>,
    marks: Vec<(Mark, usize)>,
}

/// Parse a `.sprite` text file: one indexed frame per `@frame N` block, each
/// with the `@mark <name> <x> <y>` lines its block carries.
pub(crate) fn parse_indexed(
    src: &str,
    palette: &Palette,
) -> Result<Vec<(IndexedFrame, Vec<Mark>)>, SpriteError> {
    let mut frames = Vec::new();
    let mut current: Option<Block> = None;
    let at = |lineno: usize| {
        move |kind| SpriteError::Line {
            line: lineno + 1,
            kind,
        }
    };
    let finish = |block: Block| -> Result<(IndexedFrame, Vec<Mark>), SpriteError> {
        let frame = rows_to_frame(block.rows).map_err(at(block.header))?;
        let grid = &frame.0;
        if let Some((m, lineno)) = block
            .marks
            .iter()
            .find(|(m, _)| m.x() >= grid.width() || m.y() >= grid.height())
        {
            return Err(at(*lineno)(LineError::MarkOutside {
                name: m.name().to_owned(),
                x: m.x(),
                y: m.y(),
                width: grid.width(),
                height: grid.height(),
            }));
        }
        Ok((frame, block.marks.into_iter().map(|(m, _)| m).collect()))
    };

    for (lineno, raw) in src.lines().enumerate() {
        let line = strip_comment_and_trim(raw);
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("@frame") {
            if let Some(block) = current.take() {
                frames.push(finish(block)?);
            }
            let _ = rest
                .trim()
                .parse::<u32>()
                .map_err(|_| at(lineno)(LineError::FrameNumber))?;
            current = Some(Block {
                header: lineno,
                rows: Vec::new(),
                marks: Vec::new(),
            });
            continue;
        }

        let Block { rows, marks, .. } = current
            .as_mut()
            .ok_or_else(|| at(lineno)(LineError::DataBeforeFrame))?;

        if let Some(rest) = line.strip_prefix("@mark") {
            let mark = parse_mark(rest).map_err(at(lineno))?;
            let head = |m: &Mark| m.name().starts_with(HEAD_MARK);
            if marks
                .iter()
                .any(|(m, _)| m.name() == mark.name() || (head(m) && head(&mark)))
            {
                return Err(at(lineno)(LineError::DuplicateMark));
            }
            marks.push((mark, lineno));
            continue;
        }

        let row = parse_row(line, palette).map_err(at(lineno))?;
        push_row(rows, row).map_err(at(lineno))?;
    }

    if let Some(block) = current.take() {
        frames.push(finish(block)?);
    }

    if frames.is_empty() {
        return Err(SpriteError::NoFrames);
    }
    Ok(frames)
}

/// The fields of an `@mark <name> <x> <y>` line after its keyword. A head mark
/// must name a view.
fn parse_mark(fields: &str) -> Result<Mark, LineError> {
    let mut it = fields.split_whitespace();
    let (Some(name), Some(x), Some(y), None) = (it.next(), it.next(), it.next(), it.next()) else {
        return Err(LineError::MarkFields);
    };
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
    {
        return Err(LineError::MarkName {
            name: name.to_owned(),
        });
    }
    let coord = |v: &str| {
        v.parse::<u16>().map_err(|_| LineError::MarkCoordinate {
            value: v.to_owned(),
        })
    };
    let mark = Mark::new(name.to_owned(), coord(x)?, coord(y)?);
    if name.starts_with(HEAD_MARK) && HeadMark::of(&mark).is_none() {
        return Err(LineError::HeadWithoutView {
            name: name.to_owned(),
        });
    }
    Ok(mark)
}

fn strip_comment_and_trim(line: &str) -> &str {
    let line = match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    };
    line.trim()
}

fn parse_row(line: &str, palette: &Palette) -> Result<Vec<PaletteIndex>, LineError> {
    let mut out = Vec::new();
    for tok in line.split_whitespace() {
        let mut chars = tok.chars();
        let (Some(key), None) = (chars.next(), chars.next()) else {
            return Err(LineError::PixelToken {
                token: tok.to_owned(),
            });
        };
        let index = palette
            .drawable_index(key)
            .ok_or(LineError::UnknownKey { key })?;
        let index =
            PaletteIndex::try_from(index).map_err(|_| LineError::KeyPastCapacity { key })?;
        out.push(index);
    }
    Ok(out)
}

/// Append `row` to a frame's `rows`, rejected where it breaks the frame's
/// shape, so the error names the row's own line.
fn push_row(rows: &mut Vec<Vec<PaletteIndex>>, row: Vec<PaletteIndex>) -> Result<(), LineError> {
    // Grid dims are u16: past them a frame's `data` would outgrow its grid.
    if u16::try_from(rows.len() + 1).is_err() {
        return Err(LineError::TooManyRows {
            rows: rows.len() + 1,
        });
    }
    match rows.first() {
        None if u16::try_from(row.len()).is_err() => {
            return Err(LineError::TooWide { width: row.len() });
        }
        Some(first) if row.len() != first.len() => {
            return Err(LineError::Ragged {
                row: rows.len(),
                expected: first.len(),
                got: row.len(),
            });
        }
        _ => {}
    }
    rows.push(row);
    Ok(())
}

fn rows_to_frame(rows: Vec<Vec<PaletteIndex>>) -> Result<IndexedFrame, LineError> {
    let height =
        u16::try_from(rows.len()).map_err(|_| LineError::TooManyRows { rows: rows.len() })?;
    let first = rows.first().ok_or(LineError::NoRows)?;
    let width =
        u16::try_from(first.len()).map_err(|_| LineError::TooWide { width: first.len() })?;
    let data = rows.into_iter().flatten().collect();
    Ok(IndexedFrame(Grid::from_vec(width, height, data)))
}

/// Separator joining an animation to the density it is drawn at: `desk@4x` is
/// the `desk` piece drawn on a 4x grid, for a painter rendering at a scale
/// where the base art would otherwise be block-upscaled.
///
/// The SCALE is in the name: a name that says only "denser" cannot express a
/// pack shipping BOTH a 2x and a 4x variant of one piece, and leaves the file's
/// meaning dependent on whichever render scale happens to measure it.
pub(crate) const DENSITY_VARIANT_SEP: char = '@';

/// The grid art is authored on, in art pixels per logical unit: 1 for base
/// art, `N` for a `<name>@<N>x` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Density(NonZeroU16);

impl Density {
    /// The base art's density.
    pub const ONE: Self = Self(NonZeroU16::MIN);

    /// `n` as a density, or `None` for 0.
    pub const fn new(n: u16) -> Option<Self> {
        match NonZeroU16::new(n) {
            Some(n) => Some(Self(n)),
            None => None,
        }
    }

    /// The density as a number.
    pub const fn get(self) -> u16 {
        self.0.get()
    }

    /// The density as the non-zero factor it is.
    pub(crate) const fn as_nonzero(self) -> NonZeroU16 {
        self.0
    }
}

impl std::fmt::Display for Density {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The animation name for `base` drawn at `density`x.
pub(crate) fn density_variant_name(base: &str, density: Density) -> String {
    format!("{base}{DENSITY_VARIANT_SEP}{density}x")
}

/// The largest density a variant name may claim.
///
/// A pack author types this number, so a claim past any real authoring grid is
/// a typo, and a bound there costs no real pack anything. It keeps
/// `desk@60000x` an unknown name rather than a variant, which would become one
/// of [`Pack::density_variants`](crate::pack::Pack::density_variants) that no
/// real render scale lands on.
pub(crate) const MAX_DENSITY_VARIANT: u16 = 64;

/// The base animation and density a variant name denotes, if it is one.
///
/// `1x` is deliberately NOT a variant: it would be a second name for the base
/// animation, and one thing with two names is how a pack ends up shipping both.
pub(crate) fn split_density_variant(name: &str) -> Option<(&str, Density)> {
    let (base, density) = name.rsplit_once(DENSITY_VARIANT_SEP)?;
    let digits = density.strip_suffix('x')?;
    // Digits only, with no leading zero. `u16::from_str` accepts a leading `+`
    // or `0`, which would give one density two spellings — and this name is a
    // lookup KEY, so two spellings is two files a renderer picks between
    // arbitrarily, or one it never looks up.
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let density = Density::new(digits.parse().ok()?)
        .filter(|d| (2..=MAX_DENSITY_VARIANT).contains(&d.get()))?;
    Some((base, density))
}

/// The size a `<base>@<N>x` variant must be: `base`'s times `density`, exactly.
///
/// Wider than a frame dimension: a claim past `u16::MAX` stays a size no frame
/// can meet, where a saturated one would equal a `u16::MAX`-wide frame.
pub(crate) fn claimed_variant_size(base: &Frame, density: Density) -> (u32, u32) {
    (
        u32::from(base.width()) * u32::from(density.get()),
        u32::from(base.height()) * u32::from(density.get()),
    )
}

/// Whether `variant` is exactly the size its density claims over `base`
/// ([`claimed_variant_size`]).
pub(crate) fn variant_fits(base: &Frame, density: Density, variant: &Frame) -> bool {
    claimed_variant_size(base, density) == (u32::from(variant.width()), u32::from(variant.height()))
}

/// Whether `variant` redraws `base` at `density`: frame for frame, each frame
/// exactly the size its density claims over the matching base frame
/// ([`variant_fits`]). The one rule a renderer takes a variant by.
pub(crate) fn variant_redraws(base: &Sprite, density: Density, variant: &Sprite) -> bool {
    let (base, variant) = (base.frames(), variant.frames());
    !variant.is_empty()
        && variant.len() == base.len()
        && base
            .iter()
            .zip(variant)
            .all(|(base, variant)| variant_fits(base, density, variant))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sprite::Rgb;

    fn d(n: u16) -> Density {
        Density::new(n).expect("nonzero")
    }

    #[test]
    fn final_frame_width_error_carries_line_context() {
        let mut pal = Palette::new();
        pal.insert('X', Some(Rgb { r: 1, g: 1, b: 1 }));
        let src = "@frame 0\nX X\nX\n";
        let err = parse_indexed(src, &pal).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("line"),
            "final-frame parse error needs line context: {msg}"
        );
    }

    #[test]
    fn a_variant_name_carries_the_density_it_is_drawn_at() {
        // The name is the CLAIM a renderer looks up by, so it round-trips.
        assert_eq!(density_variant_name("desk", d(4)), "desk@4x");
        assert_eq!(split_density_variant("desk@4x"), Some(("desk", d(4))));
        assert_eq!(split_density_variant("desk@12x"), Some(("desk", d(12))));
        assert_eq!(split_density_variant("desk@1x"), None);
        assert_eq!(split_density_variant("desk@0x"), None);
        // Malformed claims are not variants; they fall through to the plain
        // name, where the registry rejects them as unknown.
        assert_eq!(split_density_variant("desk"), None);
        assert_eq!(split_density_variant("desk@x"), None);
        assert_eq!(split_density_variant("desk@4"), None);
        assert_eq!(split_density_variant("desk@-2x"), None);
    }
}
